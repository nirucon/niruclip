//! The preview: mpv drawing into a child window, steered over its JSON IPC.
//!
//! mpv decodes on the graphics card and seeks frame-exactly; NIRUCLIP only
//! sends it commands. It runs as its own process, so the static recorder
//! binary needs no video libraries, and a crash in a decoder cannot take
//! the editor with it.
use super::export::{pip_box, pip_source, PipSource};
use super::media::{ProxyKind, FILLER_SECONDS};
use super::project::{window, Media, Project};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
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

    /// Load `url` at `at` seconds, with the filters `graph` laying a
    /// picture-in-picture over it ("" for none).
    pub fn load(&mut self, url: &str, at: f64, pause: bool, graph: &str) {
        self.set_graph(graph);
        self.set("pause", serde_json::Value::from(pause));
        self.set("start", serde_json::Value::from(format!("{at:.3}")));
        self.send(serde_json::json!({ "command": ["loadfile", url, "replace"] }));
    }

    pub fn set_graph(&mut self, graph: &str) {
        self.set("lavfi-complex", serde_json::Value::from(graph));
    }

    pub fn mute(&mut self, mute: bool) {
        self.set("mute", serde_json::Value::from(mute));
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
///
/// With a picture-in-picture somewhere, the EDL has two streams: first the
/// camera (or chosen film) of every clip, cut to match, then the film.
/// `graph` puts the first over the second where and when each clip shows
/// it; it is empty for a plain timeline.
pub struct Timeline {
    pub url: String,
    pub map: Vec<(f64, f64, f64, f64)>,
    pub graph: String,
}

/// Where the live preview finds its side files (the media cache).
pub trait Live {
    fn proxy(&mut self, path: &Path, kind: &ProxyKind) -> Option<PathBuf>;
    fn filler(&mut self) -> Option<PathBuf>;
}

impl Live for super::media::Cache {
    fn proxy(&mut self, path: &Path, kind: &ProxyKind) -> Option<PathBuf> {
        super::media::Cache::proxy(self, path, kind)
    }
    fn filler(&mut self) -> Option<PathBuf> {
        super::media::Cache::filler(self)
    }
}

fn entry(url: &mut String, path: &str, start: Option<f64>, length: f64) {
    if !url.ends_with("edl://") {
        url.push(';');
    }
    match start {
        Some(s) => url.push_str(&format!(
            "%{}%{path},start={s:.6},length={length:.6}",
            path.len()
        )),
        None => url.push_str(&format!("%{}%{path},length={length:.6}", path.len())),
    }
}

/// The size of a title card picture in the preview.
fn card_size(project: &Project) -> (u32, u32) {
    let (w, h) = if project.width > 0 {
        (project.width, project.height)
    } else {
        (1920, 1080)
    };
    let ph = ((f64::from(h) * 320.0 / f64::from(w.max(1))).round() as u32 / 2 * 2).max(2);
    (320, ph)
}

pub fn timeline(project: &Project, live: &mut dyn Live) -> Option<Timeline> {
    if project.clips.is_empty() {
        return None;
    }
    let starts = project.starts();
    // The first piece decides which tracks the whole EDL has: when it has
    // no sound but a later one does, it plays from a copy with silence.
    let first = &project.media[project.clips[0].media];
    let silent_first = !first.has_audio
        && project
            .clips
            .iter()
            .any(|c| project.media[c.media].has_audio && !project.media[c.media].still);
    // Whether the film stream has a sound track (from its first piece).
    let mut sound = first.has_audio && !first.still;
    let mut film = String::from("edl://");
    let mut map = Vec::new();
    let mut edl = 0.0;
    for (i, clip) in project.clips.iter().enumerate() {
        let media = &project.media[clip.media];
        let source = clip.end - clip.start;
        let silent = i == 0 && silent_first;
        if media.is_card() {
            let (w, h) = card_size(project);
            let colour = clip.effects.card_colour;
            let file = if silent {
                live.proxy(
                    Path::new("card"),
                    &ProxyKind::Card {
                        colour,
                        w,
                        h,
                        length: source,
                    },
                )
            } else {
                None
            };
            match file {
                Some(f) => {
                    sound = true;
                    entry(&mut film, &f.display().to_string(), Some(0.0), source)
                }
                None => {
                    // mpv's EDL takes no lavfi source: a small picture in
                    // the colour.
                    let path = card_picture(colour, w, h)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    entry(&mut film, &path, None, source);
                }
            }
        } else {
            let proxy = if silent {
                live.proxy(
                    &media.path,
                    &ProxyKind::Silent {
                        still: media.still,
                        length: source,
                    },
                )
            } else {
                None
            };
            if proxy.is_some() {
                sound = true;
            }
            match (proxy, media.still) {
                (Some(f), true) => entry(&mut film, &f.display().to_string(), Some(0.0), source),
                (Some(f), false) => entry(
                    &mut film,
                    &f.display().to_string(),
                    Some(clip.start),
                    source,
                ),
                (None, true) => entry(&mut film, &media.path.display().to_string(), None, source),
                (None, false) => entry(
                    &mut film,
                    &media.path.display().to_string(),
                    Some(clip.start),
                    source,
                ),
            }
        }
        map.push((starts[i], edl, clip.length(), clip.speed));
        edl += source;
    }
    let (camera, graph) = if project.hide_camera {
        (String::new(), String::new())
    } else {
        pip_stream(project, live, &map, sound)
    };
    if graph.is_empty() {
        return Some(Timeline {
            url: film,
            map,
            graph: String::new(),
        });
    }
    let url = format!(
        "edl://!new_stream;{};!new_stream;{}",
        camera.trim_start_matches("edl://"),
        film.trim_start_matches("edl://")
    );
    Some(Timeline { url, map, graph })
}

/// The picture size of a clip's piece in the preview.
fn piece_size(project: &Project, media: &Media) -> (u32, u32) {
    if media.is_card() {
        card_size(project)
    } else if media.width > 0 && media.height > 0 {
        (media.width, media.height)
    } else {
        (1920, 1080)
    }
}

/// Where a picture-in-picture shows: (from, to) in EDL seconds and its
/// x, y, width and height on the piece.
type Show = (f64, f64, u32, u32, u32, u32);

/// The picture-in-picture stream (as EDL entries) and the filters that
/// lay it over the film. Empty when no clip shows one, or its side files
/// are still being made.
fn pip_stream(
    project: &Project,
    live: &mut dyn Live,
    map: &[(f64, f64, f64, f64)],
    sound: bool,
) -> (String, String) {
    let sources: Vec<Option<PipSource>> = project
        .clips
        .iter()
        .map(|c| pip_source(project, c))
        .collect();
    if sources.iter().all(Option::is_none) {
        return (String::new(), String::new());
    }
    let Some(filler) = live.filler() else {
        return (String::new(), String::new());
    };
    let filler = filler.display().to_string();
    let mut url = String::from("edl://");
    let pad = |url: &mut String, mut length: f64| {
        while length > 1e-6 {
            let l = length.min(FILLER_SECONDS);
            entry(url, &filler, Some(0.0), l);
            length -= l;
        }
    };
    // (from, to, x, y, w, h) in EDL seconds and the piece's pixels.
    let mut shows: Vec<Show> = Vec::new();
    for (i, clip) in project.clips.iter().enumerate() {
        let source = clip.end - clip.start;
        let edl0 = map[i].1;
        let media = &project.media[clip.media];
        let piece = match &sources[i] {
            Some(PipSource::Camera { stream, layout }) => live
                .proxy(
                    &media.path,
                    &ProxyKind::Video {
                        stream: *stream,
                        content: layout.content,
                    },
                )
                .map(|f| (f.display().to_string(), Some(clip.start), source)),
            Some(PipSource::File { media: k }) => {
                let m = &project.media[*k];
                if m.still {
                    Some((m.path.display().to_string(), None, source))
                } else {
                    live.proxy(
                        &m.path,
                        &ProxyKind::Video {
                            stream: 0,
                            content: (0, 0),
                        },
                    )
                    .map(|f| (f.display().to_string(), Some(0.0), source.min(m.duration)))
                }
            }
            None => None,
        };
        match piece {
            Some((path, start, length)) if length > 0.01 => {
                entry(&mut url, &path, start, length);
                pad(&mut url, source - length);
                let src = sources[i].as_ref().expect("a source");
                let (fw, fh) = piece_size(project, media);
                let (b, _) = pip_box(project, clip, src, fw, fh);
                // The preview shows the whole picture, never squeezed: a
                // circle is drawn as its square's width.
                let content = match src {
                    PipSource::Camera { layout, .. } => layout.content,
                    PipSource::File { media: k } => {
                        let m = &project.media[*k];
                        (m.width.max(2), m.height.max(2))
                    }
                };
                let bw = b.width.max(2);
                let bh = ((f64::from(bw) * f64::from(content.1) / f64::from(content.0.max(1)))
                    .round() as u32
                    / 2
                    * 2)
                .clamp(2, fh);
                let cy = b.y + b.height / 2;
                let y = cy.saturating_sub(bh / 2).min(fh.saturating_sub(bh));
                let (a, z) = window(clip.pip.start, clip.pip.length, clip.length());
                shows.push((edl0 + a * clip.speed, edl0 + z * clip.speed, b.x, y, bw, bh));
            }
            _ => pad(&mut url, source),
        }
    }
    if shows.is_empty() {
        return (String::new(), String::new());
    }
    // One sum per value: each clip's term counts only inside its window.
    let sum = |base: u32, pick: &dyn Fn(&Show) -> u32| {
        let mut e = format!("{base}");
        for s in &shows {
            let v = i64::from(pick(s)) - i64::from(base);
            if v != 0 {
                e.push_str(&format!("+gte(t,{:.3})*lt(t,{:.3})*{v}", s.0, s.1));
            }
        }
        e
    };
    let on: Vec<String> = shows
        .iter()
        .map(|s| format!("gte(t,{:.3})*lt(t,{:.3})", s.0, s.1))
        .collect();
    let graph = format!(
        "[vid1]scale=w='{}':h='{}':eval=frame,setsar=1[pip];\
         [vid2][pip]overlay=x='{}':y='{}':eval=frame:enable='{}'[vo]{}",
        sum(2, &|s| s.4),
        sum(2, &|s| s.5),
        sum(0, &|s| s.2),
        sum(0, &|s| s.3),
        on.join("+"),
        if sound { ";[aid1]anull[ao]" } else { "" },
    );
    (url, graph)
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
    use super::super::project::{CameraTrack, Media, Project};
    use super::*;

    /// Side files that are all made already, at made-up paths.
    struct Ready;
    impl Live for Ready {
        fn proxy(&mut self, path: &Path, kind: &ProxyKind) -> Option<PathBuf> {
            let tag = match kind {
                ProxyKind::Video { stream, .. } => format!("v{stream}"),
                ProxyKind::Silent { .. } => "s".into(),
                ProxyKind::Card { .. } => "card".into(),
            };
            Some(PathBuf::from(format!(
                "/c/{}-{tag}.mkv",
                path.file_stem()?.to_string_lossy()
            )))
        }
        fn filler(&mut self) -> Option<PathBuf> {
            Some(PathBuf::from("/c/filler.mkv"))
        }
    }

    /// Nothing is made yet.
    struct Waiting;
    impl Live for Waiting {
        fn proxy(&mut self, _: &Path, _: &ProxyKind) -> Option<PathBuf> {
            None
        }
        fn filler(&mut self) -> Option<PathBuf> {
            None
        }
    }

    fn recording(p: &mut Project, name: &str, camera: bool) -> usize {
        p.add_media(Media {
            path: PathBuf::from(format!("/v/{name}.mkv")),
            duration: 20.0,
            has_video: true,
            has_audio: true,
            width: 1920,
            height: 1080,
            fps: 30.0,
            camera: camera.then(|| CameraTrack {
                stream: 1,
                layout: "corner=br;size=0.26;shape=rounded;border=none;burned=0;content=1280x720"
                    .into(),
            }),
            ..Media::default()
        })
    }

    #[test]
    fn the_camera_plays_live_over_the_film() {
        let mut p = Project::default();
        let rec = recording(&mut p, "rec", true);
        let plain = recording(&mut p, "plain", false);
        p.append(rec, None);
        p.append(plain, None);
        p.clips[0].end = 6.0;
        p.clips[1].end = 4.0;
        p.clips[0].pip.start = 1.0;
        p.clips[0].pip.length = 3.0;
        let t = timeline(&p, &mut Ready).unwrap();
        // The camera stream first (vid1), the film second (vid2).
        assert_eq!(
            t.url,
            "edl://!new_stream;%13%/c/rec-v1.mkv,start=0.000000,length=6.000000;\
             %13%/c/filler.mkv,start=0.000000,length=4.000000;!new_stream;\
             %10%/v/rec.mkv,start=0.000000,length=6.000000;\
             %12%/v/plain.mkv,start=0.000000,length=4.000000"
        );
        assert!(t.graph.starts_with("[vid1]scale="), "{}", t.graph);
        assert!(t.graph.contains("[vid2][pip]overlay="), "{}", t.graph);
        assert!(t.graph.contains("gte(t,1.000)*lt(t,4.000)"), "{}", t.graph);
        assert!(t.graph.ends_with("[aid1]anull[ao]"), "{}", t.graph);
        // Hidden camera, or side files still being made: the plain film.
        p.hide_camera = true;
        assert!(timeline(&p, &mut Ready).unwrap().graph.is_empty());
        p.hide_camera = false;
        let t = timeline(&p, &mut Waiting).unwrap();
        assert!(t.graph.is_empty() && !t.url.contains("new_stream"));
        // Switched off on the clip: no camera.
        p.clips[0].pip.mode = 1;
        assert!(timeline(&p, &mut Ready).unwrap().graph.is_empty());
    }

    #[test]
    fn a_silent_first_piece_plays_from_a_copy_with_sound() {
        let mut p = Project::default();
        let card = p.card();
        let rec = recording(&mut p, "rec", false);
        p.append(card, None);
        p.append(rec, None);
        let t = timeline(&p, &mut Ready).unwrap();
        assert!(
            t.url
                .starts_with("edl://%16%/c/card-card.mkv,start=0.000000"),
            "{}",
            t.url
        );
        // Waiting for the copy: the plain picture meanwhile (made by
        // ffmpeg when there is one), never the copy.
        let t = timeline(&p, &mut Waiting).unwrap();
        assert!(!t.url.contains("card-card.mkv"), "{}", t.url);
        assert!(t.url.contains(",length=5.000000;"), "{}", t.url);
    }

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
        let t = timeline(&p, &mut Ready).unwrap();
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
