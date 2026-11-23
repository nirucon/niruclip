//! Listening to the film: where the pauses are, and what is said.
//!
//! Both start from the film's own sound (no music, no levelling) as 16 kHz
//! mono, the format speech recognition wants. The level of every 20 ms is
//! enough to find pauses; captions then send each spoken stretch to
//! nirudictate, which transcribes on this computer.
use super::export;
use super::project::Project;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub const RATE: u32 = 16_000;
/// Seconds per level.
pub const FRAME: f64 = 0.02;

/// The sound level of the film, in dBFS for every `FRAME`.
#[derive(Clone, Debug, Default)]
pub struct Levels {
    pub db: Vec<f32>,
}

impl Levels {
    pub fn from_samples(samples: &[i16]) -> Self {
        let n = (f64::from(RATE) * FRAME) as usize;
        let db = samples
            .chunks(n)
            .map(|c| {
                let sum: f64 = c.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
                let rms = (sum / c.len().max(1) as f64).sqrt() / 32768.0;
                (20.0 * rms.max(1e-6).log10()) as f32
            })
            .collect();
        Self { db }
    }

    /// Between the room's noise and the voice: a level that works for
    /// quiet and loud recordings alike.
    pub fn auto_threshold(&self) -> f64 {
        let mut v: Vec<f32> = self.db.iter().copied().filter(|d| *d > -100.0).collect();
        if v.is_empty() {
            return -40.0;
        }
        v.sort_by(f32::total_cmp);
        let at = |q: f64| f64::from(v[((v.len() - 1) as f64 * q) as usize]);
        let (floor, voice) = (at(0.10), at(0.90));
        if voice - floor < 6.0 {
            return floor + 3.0;
        }
        floor + (voice - floor) * 0.35
    }

    /// Stretches quieter than `threshold` dB for at least `min` seconds.
    pub fn quiet(&self, threshold: f64, min: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut start = None;
        for (i, d) in self.db.iter().enumerate() {
            let quiet = f64::from(*d) < threshold;
            match (quiet, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    let (a, b) = (s as f64 * FRAME, i as f64 * FRAME);
                    if b - a >= min {
                        out.push((a, b));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            let (a, b) = (s as f64 * FRAME, self.db.len() as f64 * FRAME);
            if b - a >= min {
                out.push((a, b));
            }
        }
        out
    }

    pub fn seconds(&self) -> f64 {
        self.db.len() as f64 * FRAME
    }
}

/// Pauses to cut: quiet stretches less what to keep around the speech. A
/// pause at the very start or end goes completely.
pub fn pauses(levels: &Levels, threshold: f64, min: f64, keep: f64) -> Vec<(f64, f64)> {
    let end = levels.seconds();
    levels
        .quiet(threshold, min)
        .into_iter()
        .filter_map(|(a, b)| {
            let a = if a <= FRAME { 0.0 } else { a + keep };
            let b = if b >= end - FRAME {
                end + 1.0
            } else {
                b - keep
            };
            (b - a > 0.1).then_some((a, b))
        })
        .collect()
}

/// Spoken stretches grouped into captions of at most `max` seconds.
pub fn chunks(levels: &Levels, threshold: f64, max: f64) -> Vec<(f64, f64)> {
    let end = levels.seconds();
    let quiet = levels.quiet(threshold, 0.25);
    // Speech is what lies between the pauses.
    let mut speech = Vec::new();
    let mut at = 0.0;
    for (a, b) in &quiet {
        if *a - at > 0.15 {
            speech.push((at, *a));
        }
        at = *b;
    }
    if end - at > 0.15 {
        speech.push((at, end));
    }
    // A long stretch is cut at its quietest moment near the middle.
    let mut pieces = Vec::new();
    for (a, b) in speech {
        let mut stack = vec![(a, b)];
        while let Some((a, b)) = stack.pop() {
            if b - a <= max * 1.3 {
                pieces.push((a, b));
                continue;
            }
            let lo = ((a + (b - a) * 0.3) / FRAME) as usize;
            let hi = (((a + (b - a) * 0.7) / FRAME) as usize).min(levels.db.len());
            let cut = (lo..hi)
                .min_by(|x, y| levels.db[*x].total_cmp(&levels.db[*y]))
                .map(|i| i as f64 * FRAME)
                .unwrap_or((a + b) / 2.0);
            stack.push((cut, b));
            stack.push((a, cut));
        }
    }
    pieces.sort_by(|x, y| x.0.total_cmp(&y.0));
    // Short neighbours with a short gap read better as one caption.
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, b) in pieces {
        if let Some(last) = out.last_mut() {
            if a - last.1 < 0.6 && b - last.0 <= max {
                last.1 = b;
                continue;
            }
        }
        out.push((a, b));
    }
    out
}

/// A caption text spread over its time: at most two lines a caption.
pub fn split_text(a: f64, b: f64, text: &str) -> Vec<(f64, f64, String)> {
    let lines = export::wrap(text, 42);
    if lines.len() <= 2 {
        return vec![(a, b, lines.join(" "))];
    }
    let words: usize = text.split_whitespace().count().max(1);
    let mut out = Vec::new();
    let mut at = a;
    for pair in lines.chunks(2) {
        let part = pair.join(" ");
        let share = part.split_whitespace().count() as f64 / words as f64;
        let end = (at + (b - a) * share).min(b);
        out.push((at, end, part));
        at = end;
    }
    out
}

pub enum Msg {
    Progress(f64, String),
    Levels(Levels),
    Captions(Vec<(f64, f64, String)>),
    Failed(String),
}

pub struct Task {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Render the film's sound to a 16 kHz mono WAV.
fn render_sound(project: &Project, work: &Path, file: &Path) -> Result<(), String> {
    let plan = export::sound_plan(
        project,
        work,
        "aresample=16000,aformat=sample_fmts=s16:channel_layouts=mono",
        vec![
            "-c:a".into(),
            "pcm_s16le".into(),
            "-f".into(),
            "wav".into(),
            file.display().to_string(),
        ],
    )?;
    let out = super::die_with_parent(&mut Command::new("ffmpeg"))
        .args(plan.main())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("ffmpeg could not start: {e}"))?;
    for f in plan.temp {
        let _ = std::fs::remove_file(f);
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .lines()
            .last()
            .unwrap_or("ffmpeg could not read the sound")
            .to_string());
    }
    Ok(())
}

/// The samples of a 16-bit mono WAV written by `render_sound`.
fn read_wav(file: &Path) -> Result<Vec<i16>, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        if id == b"data" {
            let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
            return Ok(body
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        pos += 8 + size + (size & 1);
    }
    Err("the sound file has no data".into())
}

fn write_wav(file: &Path, samples: &[i16]) -> Result<(), String> {
    let data = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(file, b).map_err(|e| e.to_string())
}

/// The sound levels of the film, in the background.
pub fn levels(project: Project, work: PathBuf) -> Task {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        let file = work.join(format!("sound-{}.wav", std::process::id()));
        let _ = tx.send(Msg::Progress(0.0, "Listening to the film…".into()));
        let result = render_sound(&project, &work, &file).and_then(|_| read_wav(&file));
        let _ = std::fs::remove_file(&file);
        let _ = tx.send(match result {
            Ok(samples) => Msg::Levels(Levels::from_samples(&samples)),
            Err(e) => Msg::Failed(e),
        });
    });
    Task { rx, cancel }
}

pub const LANGUAGES: [(&str, &str); 2] = [("Swedish", "sv"), ("English", "en")];

pub fn have_nirudictate() -> bool {
    Command::new("nirudictate")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// The installed model to use for a language: small is the best trade of
/// speed and accuracy for a film, then bigger, then smaller.
pub fn model_for(language: &str) -> Result<String, String> {
    let out = Command::new("nirudictate")
        .args(["models", "list", "--json"])
        .output()
        .map_err(|_| "captions need NIRUDICTATE: nirux extras install nirudictate".to_string())?;
    let list: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "nirudictate did not list its models".to_string())?;
    let installed: Vec<String> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|m| m["installed"].as_bool() == Some(true))
                .filter(|m| m["language"].as_str() == Some(language))
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for size in ["small", "medium", "turbo", "base", "large", "tiny"] {
        let id = format!("{language}-{size}");
        if installed.contains(&id) {
            return Ok(id);
        }
    }
    installed.into_iter().next().ok_or_else(|| {
        format!("no {language} speech model: nirudictate models install {language}-small")
    })
}

/// Captions for the whole film, in the background.
pub fn captions(project: Project, work: PathBuf, language: &'static str) -> Task {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::spawn(move || {
        if let Err(e) = transcribe(&project, &work, language, &tx, &stop) {
            let _ = tx.send(Msg::Failed(e));
        }
    });
    Task { rx, cancel }
}

fn transcribe(
    project: &Project,
    work: &Path,
    language: &str,
    tx: &Sender<Msg>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let model = model_for(language)?;
    let _ = tx.send(Msg::Progress(0.0, "Listening to the film…".into()));
    let file = work.join(format!("speech-{}.wav", std::process::id()));
    let samples = render_sound(project, work, &file).and_then(|_| read_wav(&file));
    let _ = std::fs::remove_file(&file);
    let samples = samples?;
    let levels = Levels::from_samples(&samples);
    let parts = chunks(&levels, levels.auto_threshold(), 6.0);
    if parts.is_empty() {
        return Err("no speech found in the film".into());
    }
    let slice = work.join(format!("caption-{}.wav", std::process::id()));
    let mut out = Vec::new();
    for (k, (a, b)) in parts.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&slice);
            return Ok(());
        }
        let _ = tx.send(Msg::Progress(
            k as f64 / parts.len() as f64,
            format!("Transcribing {} of {}…", k + 1, parts.len()),
        ));
        // A little air around the words helps the recogniser.
        let from = ((a - 0.15).max(0.0) * f64::from(RATE)) as usize;
        let to = (((b + 0.15) * f64::from(RATE)) as usize).min(samples.len());
        if to <= from {
            continue;
        }
        write_wav(&slice, &samples[from..to])?;
        let result = super::die_with_parent(&mut Command::new("nirudictate"))
            .args(["transcribe"])
            .arg(&slice)
            .args(["--model", &model])
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("nirudictate could not start: {e}"))?;
        if !result.status.success() {
            let _ = std::fs::remove_file(&slice);
            let err = String::from_utf8_lossy(&result.stderr);
            return Err(format!(
                "nirudictate: {}",
                err.lines().last().unwrap_or("transcription failed")
            ));
        }
        let text = String::from_utf8_lossy(&result.stdout).trim().to_string();
        if !text.is_empty() && !is_noise(&text) {
            out.extend(split_text(*a, *b, &text));
        }
    }
    let _ = std::fs::remove_file(&slice);
    let _ = tx.send(Msg::Captions(out));
    Ok(())
}

/// What a recogniser says to music or silence.
fn is_noise(text: &str) -> bool {
    let t = text
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    t.is_empty()
        || t.starts_with("musik")
        || t.starts_with("music")
        || t.starts_with("textning")
        || t.contains("svensktextning")
        || t == "tack"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of tone, half a second of near silence, and so on.
    fn levels(pattern: &[(f64, bool)]) -> Levels {
        let mut db = Vec::new();
        for (secs, loud) in pattern {
            for _ in 0..(secs / FRAME).round() as usize {
                db.push(if *loud { -18.0 } else { -60.0 });
            }
        }
        Levels { db }
    }

    #[test]
    fn levels_come_from_samples() {
        let loud: Vec<i16> = (0..3200)
            .map(|i| if i % 2 == 0 { 16000 } else { -16000 })
            .collect();
        let l = Levels::from_samples(&loud);
        assert_eq!(l.db.len(), 10);
        assert!((l.db[0] + 6.2).abs() < 0.5, "{}", l.db[0]);
    }

    #[test]
    fn pauses_keep_some_air_and_the_ends_go() {
        let l = levels(&[
            (1.0, false),
            (2.0, true),
            (1.5, false),
            (2.0, true),
            (1.0, false),
        ]);
        let t = l.auto_threshold();
        assert!(t > -60.0 && t < -18.0);
        let p = pauses(&l, t, 0.8, 0.2);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(p[0].0, 0.0);
        assert!((p[0].1 - 0.8).abs() < 1e-6);
        assert!((p[1].0 - 3.2).abs() < 1e-6 && (p[1].1 - 4.3).abs() < 1e-6);
        assert!(p[2].1 > l.seconds());
        // A short breath is not a pause.
        assert!(pauses(&l, t, 2.0, 0.2).is_empty());
    }

    #[test]
    fn long_speech_is_cut_into_captions() {
        let l = levels(&[(0.5, false), (20.0, true), (1.0, false), (2.0, true)]);
        let c = chunks(&l, -40.0, 6.0);
        assert!(c.len() >= 4, "{c:?}");
        assert!(c.iter().all(|(a, b)| b - a <= 6.0 * 1.3 + 1e-6));
        assert!((c[0].0 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_long_text_is_spread_over_its_time() {
        let text = "ett två tre fyra fem sex sju åtta nio tio elva tolv tretton fjorton femton sexton sjutton arton nitton tjugo";
        let parts = split_text(0.0, 10.0, text);
        assert!(parts.len() >= 2);
        assert_eq!(parts[0].0, 0.0);
        assert!((parts.last().unwrap().1 - 10.0).abs() < 1e-6);
        assert_eq!(split_text(1.0, 2.0, "kort").len(), 1);
        assert!(is_noise("Musik"));
        assert!(!is_noise("Hej allihop"));
    }

    #[test]
    fn a_wav_round_trips() {
        let f = std::env::temp_dir().join(format!("niruclip-wav-{}.wav", std::process::id()));
        write_wav(&f, &[1, -2, 300]).unwrap();
        assert_eq!(read_wav(&f).unwrap(), vec![1, -2, 300]);
        let _ = std::fs::remove_file(&f);
    }
}
