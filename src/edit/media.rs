//! Thumbnails and sound waves for the timeline, made once and cached.
//!
//! Each is one ffmpeg run in a background thread. The result is kept in
//! `~/.cache/niruclip/` under a key of path, size and modification time, so
//! opening a project again is instant.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Thumbnails per media: one every `THUMB_STEP` seconds, `THUMB_H` high.
pub const THUMB_H: u32 = 54;
/// Peaks per second of sound.
pub const PEAKS_PER_SECOND: usize = 50;

pub struct Thumbs {
    pub width: u32,
    pub step: f64,
    /// 0x00RRGGBB, `width * THUMB_H` per picture.
    pub frames: Vec<Vec<u32>>,
}

pub enum Ready {
    Thumbs(PathBuf, Thumbs),
    Peaks(PathBuf, Vec<u8>),
}

pub struct Cache {
    dir: PathBuf,
    pub thumbs: HashMap<PathBuf, Thumbs>,
    pub peaks: HashMap<PathBuf, Vec<u8>>,
    asked: HashMap<PathBuf, ()>,
    tx: Sender<Ready>,
    pub rx: Receiver<Ready>,
}

fn key(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // FNV-1a over the path: short, stable, no extra crate.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}-{size}-{mtime}")
}

impl Cache {
    pub fn new() -> Self {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| super::home().join(".cache"))
            .join("niruclip");
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel();
        Self {
            dir,
            thumbs: HashMap::new(),
            peaks: HashMap::new(),
            asked: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Ask for a media file's thumbnails and peaks; they arrive on `rx`.
    pub fn want(&mut self, media: &super::project::Media) {
        if media.is_card() || self.asked.contains_key(&media.path) {
            return;
        }
        self.asked.insert(media.path.clone(), ());
        let base = self.dir.join(key(&media.path));
        let (path, duration, w, h) = (
            media.path.clone(),
            media.duration,
            media.width.max(1),
            media.height.max(1),
        );
        if media.has_video {
            let tx = self.tx.clone();
            let file = base.with_extension("thumbs");
            let path = path.clone();
            let still = media.still;
            std::thread::spawn(move || {
                if let Some(t) = load_thumbs(&file).or_else(|| {
                    let t = make_thumbs(&path, duration, w, h, still)?;
                    save_thumbs(&file, &t);
                    Some(t)
                }) {
                    let _ = tx.send(Ready::Thumbs(path, t));
                }
            });
        }
        if media.has_audio {
            let tx = self.tx.clone();
            let file = base.with_extension("peaks");
            std::thread::spawn(move || {
                let peaks = std::fs::read(&file).ok().or_else(|| {
                    let p = make_peaks(&path)?;
                    let _ = std::fs::write(&file, &p);
                    Some(p)
                });
                if let Some(p) = peaks {
                    let _ = tx.send(Ready::Peaks(path, p));
                }
            });
        }
    }

    /// Take what the workers finished. True when something arrived.
    pub fn collect(&mut self) -> bool {
        let mut any = false;
        while let Ok(ready) = self.rx.try_recv() {
            any = true;
            match ready {
                Ready::Thumbs(p, t) => {
                    self.thumbs.insert(p, t);
                }
                Ready::Peaks(p, v) => {
                    self.peaks.insert(p, v);
                }
            }
        }
        any
    }
}

fn thumb_step(duration: f64) -> f64 {
    // About 120 pictures for any length, never closer than a second.
    (duration / 120.0).max(1.0)
}

fn make_thumbs(path: &Path, duration: f64, w: u32, h: u32, still: bool) -> Option<Thumbs> {
    let expected = if still {
        1
    } else {
        (duration / thumb_step(duration)) as usize
    };
    if duration > 60.0 && !still {
        if let Some(t) = scan_thumbs(path, duration, w, h, still, true) {
            if t.frames.len() * 2 >= expected {
                return Some(t);
            }
        }
    }
    scan_thumbs(path, duration, w, h, still, false)
}

fn scan_thumbs(
    path: &Path,
    duration: f64,
    w: u32,
    h: u32,
    still: bool,
    keyframes_only: bool,
) -> Option<Thumbs> {
    let tw = ((w as f64 * THUMB_H as f64 / h as f64).round() as u32).clamp(16, 200) & !1;
    let step = if still {
        duration
    } else {
        thumb_step(duration)
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    // Skip non-key frames on long files: thumbnails need no exact frame and
    // this makes the scan many times faster. A file with few key frames
    // (a short screen recording) is scanned whole.
    if !still && keyframes_only {
        cmd.args(["-skip_frame", "nokey"]);
    }
    cmd.arg("-i").arg(path).args([
        "-an",
        "-vf",
        &format!("fps=1/{step:.3},scale={tw}:{}:flags=fast_bilinear", THUMB_H),
        "-frames:v",
        "400",
        "-pix_fmt",
        "bgra",
        "-f",
        "rawvideo",
        "-",
    ]);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut raw = Vec::new();
    child.stdout.take()?.read_to_end(&mut raw).ok()?;
    let _ = child.wait();
    let size = (tw * THUMB_H * 4) as usize;
    let frames: Vec<Vec<u32>> = raw
        .chunks_exact(size)
        .map(|f| {
            f.chunks_exact(4)
                .map(|p| u32::from(p[2]) << 16 | u32::from(p[1]) << 8 | u32::from(p[0]))
                .collect()
        })
        .collect();
    if frames.is_empty() {
        return None;
    }
    Some(Thumbs {
        width: tw,
        step,
        frames,
    })
}

fn save_thumbs(file: &Path, t: &Thumbs) {
    let mut out = Vec::with_capacity(16 + t.frames.len() * t.frames[0].len() * 4);
    out.extend_from_slice(&t.width.to_le_bytes());
    out.extend_from_slice(&t.step.to_le_bytes());
    out.extend_from_slice(&(t.frames.len() as u32).to_le_bytes());
    for f in &t.frames {
        for p in f {
            out.extend_from_slice(&p.to_le_bytes());
        }
    }
    let _ = std::fs::write(file, out);
}

fn load_thumbs(file: &Path) -> Option<Thumbs> {
    let b = std::fs::read(file).ok()?;
    if b.len() < 16 {
        return None;
    }
    let width = u32::from_le_bytes(b[0..4].try_into().ok()?);
    let step = f64::from_le_bytes(b[4..12].try_into().ok()?);
    let n = u32::from_le_bytes(b[12..16].try_into().ok()?) as usize;
    let per = (width * THUMB_H) as usize;
    if b.len() != 16 + n * per * 4 {
        return None;
    }
    let frames = b[16..]
        .chunks_exact(per * 4)
        .map(|f| {
            f.chunks_exact(4)
                .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
                .collect()
        })
        .collect();
    Some(Thumbs {
        width,
        step,
        frames,
    })
}

fn make_peaks(path: &Path) -> Option<Vec<u8>> {
    let rate = 8000;
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let per = rate / PEAKS_PER_SECOND;
    let mut peaks = Vec::new();
    let mut buf = vec![0u8; per * 2];
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            match out.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => break,
            }
        }
        if filled < 2 {
            break;
        }
        let peak = buf[..filled]
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        // A square root scale shows speech clearly next to loud parts.
        let level = ((f64::from(peak) / 32768.0).sqrt() * 255.0).round() as u8;
        peaks.push(level);
        if filled < buf.len() {
            break;
        }
    }
    let _ = child.wait();
    Some(peaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_survive_the_cache_file() {
        let t = Thumbs {
            width: 4,
            step: 2.5,
            frames: vec![vec![0x112233; (4 * THUMB_H) as usize]; 3],
        };
        let file = std::env::temp_dir().join(format!("niruclip-thumbs-{}", std::process::id()));
        save_thumbs(&file, &t);
        let back = load_thumbs(&file).unwrap();
        let _ = std::fs::remove_file(&file);
        assert_eq!(back.width, 4);
        assert_eq!(back.frames.len(), 3);
        assert_eq!(back.frames[2][7], 0x112233);
        assert!((back.step - 2.5).abs() < 1e-12);
    }

    #[test]
    fn the_thumbnail_step_keeps_the_count_small() {
        assert!((thumb_step(30.0) - 1.0).abs() < 1e-9);
        assert!((thumb_step(3600.0) - 30.0).abs() < 1e-9);
    }
}
