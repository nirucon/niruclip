//! An export in the background: one to three ffmpeg runs, with progress.
use super::export::{measured_options, Plan, Step, MEASURED};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

pub enum Progress {
    /// 0..1 of the current run.
    At(f64),
    /// The run's error output.
    Log(String),
}

struct Run {
    child: Child,
    rx: Receiver<Progress>,
}

pub struct Job {
    steps: Vec<Step>,
    step: usize,
    run: Option<Run>,
    duration: f64,
    pub output: PathBuf,
    /// 0..1 of the whole job.
    pub fraction: f64,
    temp: Vec<PathBuf>,
    pub extra: Vec<PathBuf>,
    pub started: std::time::Instant,
    /// The loudness filled into later runs.
    measured: Option<String>,
}

fn spawn(args: &[String], duration: f64) -> Result<Run, String> {
    if std::env::var_os("NIRUCLIP_EDIT_DEBUG").is_some() {
        eprintln!("{}", serde_json::to_string(args).unwrap_or_default());
    }
    let mut child = super::die_with_parent(&mut Command::new("ffmpeg"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("no ffmpeg output")?;
    let mut stderr = child.stderr.take().ok_or("no ffmpeg errors")?;
    let (tx, rx) = channel();
    let total = duration.max(0.001);
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = us.trim().parse::<f64>() {
                    let _ = tx.send(Progress::At((us / 1e6 / total).clamp(0.0, 1.0)));
                }
            }
        }
        let _ = tx.send(Progress::Log(errors.join().unwrap_or_default()));
    });
    Ok(Run { child, rx })
}

impl Job {
    pub fn start(plan: Plan, output: PathBuf) -> Result<Self, String> {
        let mut job = Self {
            steps: plan.steps,
            step: 0,
            run: None,
            duration: plan.duration,
            output,
            fraction: 0.0,
            temp: plan.temp,
            extra: plan.extra,
            started: std::time::Instant::now(),
            measured: None,
        };
        job.begin()?;
        Ok(job)
    }

    fn begin(&mut self) -> Result<(), String> {
        let mut args = self.steps[self.step].args.clone();
        let fill = self.measured.clone().unwrap_or_default();
        for a in &mut args {
            if a.contains(MEASURED) {
                *a = a.replace(MEASURED, &fill);
            }
        }
        self.run = Some(spawn(&args, self.duration)?);
        Ok(())
    }

    fn done_before(&self) -> f64 {
        self.steps[..self.step].iter().map(|s| s.weight).sum()
    }

    /// Poll: None while running, Some(result) once finished.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let before = self.done_before();
        let weight = self.steps.get(self.step).map(|s| s.weight).unwrap_or(0.0);
        let run = self.run.as_mut()?;
        let mut log = None;
        let mut at = None;
        while let Ok(p) = run.rx.try_recv() {
            match p {
                Progress::At(f) => at = Some(f),
                Progress::Log(text) => log = Some(text),
            }
        }
        if let Some(f) = at {
            self.fraction = before + f * weight;
        }
        let status = match run.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            Err(e) => return Some(Err(e.to_string())),
        };
        if log.is_none() {
            // Give the reader a moment to deliver the error text.
            if let Ok(Progress::Log(text)) =
                run.rx.recv_timeout(std::time::Duration::from_millis(500))
            {
                log = Some(text);
            }
        }
        let log = log.unwrap_or_default();
        self.run = None;
        if !status.success() {
            self.cleanup();
            let _ = std::fs::remove_file(&self.output);
            let last = log
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("ffmpeg ended with {status}"));
            return Some(Err(last));
        }
        if self.steps[self.step].measure {
            // A failed measurement still levels, in one pass.
            self.measured = measured_options(&log);
        }
        self.step += 1;
        if self.step >= self.steps.len() {
            self.cleanup();
            self.fraction = 1.0;
            return Some(Ok(()));
        }
        self.fraction = self.done_before();
        if let Err(e) = self.begin() {
            self.cleanup();
            return Some(Err(e));
        }
        None
    }

    pub fn cancel(&mut self) {
        if let Some(mut run) = self.run.take() {
            let _ = run.child.kill();
            let _ = run.child.wait();
        }
        let _ = std::fs::remove_file(&self.output);
        for f in self.extra.drain(..) {
            let _ = std::fs::remove_file(f);
        }
        self.cleanup();
    }

    fn cleanup(&mut self) {
        for f in self.temp.drain(..) {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.run.is_some() {
            self.cancel();
        }
    }
}
