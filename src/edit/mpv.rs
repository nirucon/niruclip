//! The preview: mpv drawing into a child window, steered over its JSON IPC.
//!
//! mpv decodes on the graphics card and seeks frame-exactly; NIRUCLIP only
//! sends it commands. It runs as its own process, so the static recorder
//! binary needs no video libraries, and a crash in a decoder cannot take
//! the editor with it.
use super::project::Project;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub enum Event {
    Time(f64),
    Pause(bool),
    Loaded,
    Ended,
}

pub struct Mpv {
    child: Child,
    socket: UnixStream,
    path: PathBuf,
    pub events: Receiver<Event>,
}

impl Mpv {
    pub fn available() -> bool {
        Command::new("mpv")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn start(window: u32, runtime: &std::path::Path) -> Result<Self, String> {
        let path = runtime.join(format!("niruclip-mpv-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let child = Command::new("mpv")
            .args([
                "--no-config",
                "--load-scripts=no",
                "--ytdl=no",
                "--idle=yes",
                "--keep-open=always",
                "--force-window=yes",
                "--osc=no",
                "--osd-level=0",
                "--no-input-default-bindings",
                "--input-vo-keyboard=no",
                "--input-cursor=no",
                "--cursor-autohide=no",
                "--hwdec=auto-safe",
                // Only outputs that draw into our window: never a console
                // (DRM) or a window of its own (SDL). A software renderer is
                // allowed rather than no picture.
                "--vo=gpu,x11",
                "--gpu-context=x11egl,x11vk",
                "--gpu-sw=yes",
                "--hr-seek=yes",
                "--audio-display=no",
                "--really-quiet",
                "--pause=yes",
                "--image-display-duration=inf",
                "--background=color",
                "--background-color=#000000",
            ])
            .args(
                std::env::var("NIRUCLIP_MPV_LOG")
                    .ok()
                    .map(|f| format!("--log-file={f}")),
            )
            .arg(format!("--wid={window}"))
            .arg(format!("--input-ipc-server={}", path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mpv could not start: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let socket = loop {
            match UnixStream::connect(&path) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(format!("mpv did not answer: {e}")),
            }
        };
        let reader = socket.try_clone().map_err(|e| e.to_string())?;
        let (tx, events) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                let event = match (v["event"].as_str(), v["name"].as_str()) {
                    (Some("property-change"), Some("time-pos")) => {
                        v["data"].as_f64().map(Event::Time)
                    }
                    (Some("property-change"), Some("pause")) => {
                        v["data"].as_bool().map(Event::Pause)
                    }
                    (Some("property-change"), Some("eof-reached")) => {
                        (v["data"].as_bool() == Some(true)).then_some(Event::Ended)
                    }
                    (Some("file-loaded"), _) => Some(Event::Loaded),
                    _ => None,
                };
                if let Some(event) = event {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
        });
        let mut mpv = Self {
            child,
            socket,
            path,
            events,
        };
        mpv.command(&["observe_property", "1", "time-pos"]);
        mpv.command(&["observe_property", "2", "pause"]);
        mpv.command(&["observe_property", "3", "eof-reached"]);
        Ok(mpv)
    }

    fn send(&mut self, value: serde_json::Value) {
        let mut line = value.to_string();
        line.push('\n');
        let _ = self.socket.write_all(line.as_bytes());
    }

    pub fn command(&mut self, args: &[&str]) {
        let args: Vec<serde_json::Value> = args
            .iter()
            .map(|a| match a.parse::<i64>() {
                Ok(n) if !a.starts_with('+') => serde_json::Value::from(n),
                _ => serde_json::Value::from(*a),
            })
            .collect();
        self.send(serde_json::json!({ "command": args }));
    }

    pub fn set(&mut self, name: &str, value: serde_json::Value) {
        self.send(serde_json::json!({ "command": ["set_property", name, value] }));
    }

    pub fn load(&mut self, url: &str, at: f64, pause: bool) {
        self.set("pause", serde_json::Value::from(pause));
        self.set("start", serde_json::Value::from(format!("{at:.3}")));
        self.send(serde_json::json!({ "command": ["loadfile", url, "replace"] }));
    }

    pub fn seek(&mut self, at: f64) {
        self.send(serde_json::json!({ "command": ["seek", at, "absolute+exact"] }));
    }

    pub fn pause(&mut self, pause: bool) {
        self.set("pause", serde_json::Value::from(pause));
    }

    pub fn speed(&mut self, speed: f64) {
        self.set("speed", serde_json::Value::from(speed));
    }

    /// Sound filters for what plays now (a voice treatment), "" for none.
    pub fn audio_filter(&mut self, chain: &str) {
        let value = if chain.is_empty() {
            String::new()
        } else {
            format!("lavfi=[{chain}]")
        };
        self.set("af", serde_json::Value::from(value));
    }

    pub fn stop(&mut self) {
        self.command(&["stop"]);
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.command(&["quit"]);
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The live preview of a project as an mpv EDL, and how to map times.
///
/// EDL plays the cuts back to back at normal speed: speed changes and
/// transitions are shown as plain cuts until a rendered preview is asked
/// for. `map` holds (film start, EDL start, length in film seconds, speed).
pub struct Timeline {
    pub url: String,
    pub map: Vec<(f64, f64, f64, f64)>,
}

pub fn timeline(project: &Project) -> Option<Timeline> {
    if project.clips.is_empty() {
        return None;
    }
    let starts = project.starts();
    let mut url = String::from("edl://");
    let mut map = Vec::new();
    let mut edl = 0.0;
    for (i, clip) in project.clips.iter().enumerate() {
        let media = &project.media[clip.media];
        let path = if media.is_card() {
            // mpv's EDL takes no lavfi source: a small picture in the colour.
            let (w, h) = if project.width > 0 {
                (project.width, project.height)
            } else {
                (1920, 1080)
            };
            card_picture(clip.effects.card_colour, w, h)
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        } else {
            media.path.display().to_string()
        };
        let source = clip.end - clip.start;
        if i > 0 {
            url.push(';');
        }
        if media.still {
            url.push_str(&format!("%{}%{},length={source:.6}", path.len(), path));
        } else {
            url.push_str(&format!(
                "%{}%{},start={:.6},length={source:.6}",
                path.len(),
                path,
                clip.start
            ));
        }
        map.push((starts[i], edl, clip.length(), clip.speed));
        edl += source;
    }
    Some(Timeline { url, map })
}

/// A picture of a title card's colour in the film's shape, made once.
fn card_picture(colour: u32, w: u32, h: u32) -> Option<std::path::PathBuf> {
    let pw = 320u32;
    let ph = ((f64::from(h) * f64::from(pw) / f64::from(w.max(1))).round() as u32).max(2);
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".cache"))
        .join("niruclip");
    let file = dir.join(format!("card-{:06x}-{pw}x{ph}.png", colour & 0xffffff));
    if file.exists() {
        return Some(file);
    }
    let _ = std::fs::create_dir_all(&dir);
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c=0x{:06x}:s={pw}x{ph}", colour & 0xffffff))
        .args(["-frames:v", "1"])
        .arg(&file)
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(file)
}

impl Timeline {
    pub fn to_edl(&self, film: f64) -> f64 {
        for &(start, edl, length, speed) in self.map.iter().rev() {
            if film >= start - 1e-9 {
                return edl + (film - start).min(length) * speed;
            }
        }
        0.0
    }

    pub fn to_film(&self, edl_time: f64) -> f64 {
        for &(start, edl, length, speed) in self.map.iter().rev() {
            if edl_time >= edl - 1e-9 {
                return start + ((edl_time - edl) / speed).min(length);
            }
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::super::project::{Media, Project};
    use super::*;

    #[test]
    fn the_edl_holds_every_cut_and_maps_speed() {
        let mut p = Project::default();
        let m = p.add_media(Media {
            path: PathBuf::from("/v/a b.mkv"),
            duration: 20.0,
            has_video: true,
            has_audio: true,
            width: 1920,
            height: 1080,
            fps: 30.0,
            still: false,
            ..Media::default()
        });
        p.append(m, None);
        p.split(10.0);
        p.clips[0].start = 2.0;
        p.clips[1].speed = 2.0;
        let t = timeline(&p).unwrap();
        assert_eq!(
            t.url,
            "edl://%10%/v/a b.mkv,start=2.000000,length=8.000000;%10%/v/a b.mkv,start=10.000000,length=10.000000"
        );
        // Film 8 s is the start of the fast clip, 5 film seconds into it is
        // 10 seconds of source.
        assert!((t.to_edl(8.0) - 8.0).abs() < 1e-9);
        assert!((t.to_edl(13.0) - 18.0).abs() < 1e-9);
        assert!((t.to_film(18.0) - 13.0).abs() < 1e-9);
    }
}
