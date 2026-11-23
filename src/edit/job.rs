//! One ffmpeg render in the background, with progress.
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

pub enum Progress {
    /// 0..1
    At(f64),
    Done(Result<(), String>),
}

pub struct Job {
    child: Child,
    pub rx: Receiver<Progress>,
    pub output: PathBuf,
    pub fraction: f64,
    temp: Vec<PathBuf>,
    pub started: std::time::Instant,
}

impl Job {
    pub fn start(plan: super::export::Plan, output: PathBuf) -> Result<Self, String> {
        if std::env::var_os("NIRUCLIP_EDIT_DEBUG").is_some() {
            eprintln!("{}", serde_json::to_string(&plan.args).unwrap_or_default());
        }
        let mut child = Command::new("ffmpeg")
            .args(&plan.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffmpeg could not start: {e}"))?;
        let stdout = child.stdout.take().ok_or("no ffmpeg output")?;
        let mut stderr = child.stderr.take().ok_or("no ffmpeg errors")?;
        let (tx, rx) = channel();
        let total = plan.duration.max(0.001);
        let errors = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let txp = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(us) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = us.trim().parse::<f64>() {
                        let _ = txp.send(Progress::At((us / 1e6 / total).clamp(0.0, 1.0)));
                    }
                }
            }
            let text = errors.join().unwrap_or_default();
            let _ = txp.send(Progress::Done(Ok(())));
            // The real result comes from wait(); keep the error text.
            let _ = tx.send(Progress::Done(if text.trim().is_empty() {
                Ok(())
            } else {
                Err(text)
            }));
        });
        Ok(Self {
            child,
            rx,
            output,
            fraction: 0.0,
            temp: plan.temp,
            started: std::time::Instant::now(),
        })
    }

    /// Poll: None while running, Some(result) once finished.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let mut errors = None;
        while let Ok(p) = self.rx.try_recv() {
            match p {
                Progress::At(f) => self.fraction = f,
                Progress::Done(Err(e)) => errors = Some(e),
                Progress::Done(Ok(())) => {}
            }
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                // Give the reader a moment to deliver the error text.
                if !status.success() && errors.is_none() {
                    if let Ok(Progress::Done(Err(e))) =
                        self.rx.recv_timeout(std::time::Duration::from_millis(300))
                    {
                        errors = Some(e);
                    }
                }
                self.cleanup();
                if status.success() {
                    self.fraction = 1.0;
                    Some(Ok(()))
                } else {
                    let _ = std::fs::remove_file(&self.output);
                    let text = errors.unwrap_or_else(|| format!("ffmpeg ended with {status}"));
                    let last = text
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("ffmpeg failed")
                        .to_string();
                    Some(Err(last))
                }
            }
            Ok(None) => None,
            Err(e) => Some(Err(e.to_string())),
        }
    }

    pub fn cancel(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.output);
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
        if let Ok(None) = self.child.try_wait() {
            self.cancel();
        }
    }
}
