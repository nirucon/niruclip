//! `niruclip edit`: a small, fast video editor.
//!
//! Cut, join and trim recordings and other videos, add music, effects,
//! styled titles, zooms, hidden areas, captions and transitions, level the
//! sound for speech and export to the usual formats or a size limit. The
//! sources are never changed: a project is a list of decisions that one
//! ffmpeg run turns into a film.

// Drawing calls take a position, a size and colours.
#![allow(clippy::too_many_arguments)]
mod analyze;
mod browser;
mod export;
mod fonts;
mod gfx;
mod job;
mod media;
mod mpv;
mod probe;
mod project;
mod record;
mod ui;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A helper process (ffmpeg, nirudictate) ends with the editor, even when
/// the editor is killed: no orphan keeps rendering in the background.
pub fn die_with_parent(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl is async-signal-safe and touches no memory of ours.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        })
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// The user's Videos folder (`XDG_VIDEOS_DIR`), else ~/Videos.
pub fn videos_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_VIDEOS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(text) = std::fs::read_to_string(config_dir().join("user-dirs.dirs")) {
        for line in text.lines() {
            if let Some(value) = line.trim().strip_prefix("XDG_VIDEOS_DIR=") {
                let value = value.trim().trim_matches('"');
                let value = value.replace("$HOME", &home().display().to_string());
                if !value.is_empty() {
                    return PathBuf::from(value);
                }
            }
        }
    }
    home().join("Videos")
}

/// Editor settings in `~/.config/niruclip/edit.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Where Open and Import start. Empty: the Videos folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_folder: Option<PathBuf>,
    /// Where exports go. Empty: the Videos folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_folder: Option<PathBuf>,
    /// Last chosen export format (mp4, mp4-small, webm, mkv, gif, mp3, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Last chosen export height, 0 keeps the size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Last chosen size limit in MB, 0 is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_mb: Option<u32>,
    /// Last chosen captions handling (see export::CAPTIONS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captions: Option<u8>,
    /// Language of generated captions ("sv", "en").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_language: Option<String>,
    /// Pause finding: shortest pause, silence level (0 auto), air kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pauses: Option<[usize; 3]>,
    /// The last choices for recording from the editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<record::Choices>,
    /// Recent projects, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<PathBuf>,
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("niruclip/edit.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let tmp = path.with_extension("toml.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn media_folder(&self) -> PathBuf {
        self.media_folder
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(videos_dir)
    }

    pub fn export_folder(&self) -> PathBuf {
        self.export_folder
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(videos_dir)
    }

    pub fn remember(&mut self, project: &Path) {
        self.recent.retain(|p| p != project);
        self.recent.insert(0, project.to_path_buf());
        self.recent.truncate(10);
    }
}

/// Colours and font from the NIRUX appearance file.
#[derive(Clone, Debug)]
pub struct Theme {
    pub background: u32,
    pub surface: u32,
    pub raised: u32,
    pub foreground: u32,
    pub muted: u32,
    pub border: u32,
    pub accent: u32,
    pub selected: u32,
    pub warning: u32,
    pub error: u32,
    pub success: u32,
    pub font: Option<String>,
    pub size: u32,
}

fn hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok())?
}

impl Theme {
    pub fn load() -> Self {
        let mut t = Theme {
            background: 0x0f0f10,
            surface: 0x18181b,
            raised: 0x232328,
            foreground: 0xe6e6e6,
            muted: 0x8a8a93,
            border: 0x2e2e35,
            accent: 0x7aa2f7,
            selected: 0x2a3550,
            warning: 0xe0af68,
            error: 0xf7768e,
            success: 0x9ece6a,
            font: None,
            size: 15,
        };
        let file = config_dir().join("niruwm/appearance.toml");
        if let Some(v) = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| s.parse::<toml::Value>().ok())
        {
            let colour = |k: &str| v.get("colors")?.get(k)?.as_str().and_then(hex);
            for (field, key) in [
                (&mut t.background, "background"),
                (&mut t.foreground, "foreground"),
                (&mut t.muted, "muted"),
                (&mut t.border, "border"),
                (&mut t.accent, "focus"),
                (&mut t.warning, "warning"),
                (&mut t.error, "error"),
                (&mut t.success, "success"),
            ] {
                if let Some(c) = colour(key) {
                    *field = c;
                }
            }
            t.surface = colour("surface").unwrap_or(gfx::mix(t.background, t.foreground, 14));
            t.raised = gfx::mix(t.background, t.foreground, 26);
            t.selected = colour("selected").unwrap_or(gfx::mix(t.background, t.accent, 60));
            if let Some(size) = v
                .get("font")
                .and_then(|f| f.get("size"))
                .and_then(|s| s.as_float().or_else(|| s.as_integer().map(|i| i as f64)))
            {
                t.size = (size.round() as u32 + 3).clamp(11, 28);
            }
        }
        let scale = ui_scale();
        t.size = ((t.size as f32) * scale).round() as u32;
        t
    }
}

fn ui_scale() -> f32 {
    let explicit = std::env::var("NIRU_UI_SCALE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok());
    let dpi = || {
        std::process::Command::new("xrdb")
            .arg("-query")
            .output()
            .ok()
            .and_then(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        (k.trim() == "Xft.dpi").then(|| v.trim().parse::<f32>().ok())?
                    })
            })
            .map(|d| d / 96.0)
    };
    explicit
        .or_else(dpi)
        .filter(|s| s.is_finite())
        .map(|s| s.clamp(0.75, 2.5))
        .unwrap_or(1.0)
}

const HELP: &str = "\
niruclip edit [FILE...]      open the editor, with videos or a project
niruclip edit --export PROJECT [--format F] [--height H] [--output FILE]
                  [--limit MB] [--captions burn|track]
                             render a saved project without a window

Formats: mp4 (default), mp4-small, webm, mkv, gif, mp3, wav, flac, opus.
--limit fits the film in a size (two passes, smaller if it must be).
The editor needs mpv for the preview and ffmpeg for everything else;
captions need nirudictate.";

pub fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "--export") {
        return export_cli(args, i);
    }
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .collect();
    ui::run(files)
}

fn export_cli(args: &[String], i: usize) -> Result<(), String> {
    let project_path = args
        .get(i + 1)
        .map(PathBuf::from)
        .ok_or("--export needs a project file")?;
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let project = project::Project::load(&project_path)?;
    let format = match value("--format") {
        Some(f) => export::Format::from_name(&f).ok_or(format!("unknown format {f}"))?,
        None => export::Format::Mp4,
    };
    let height = value("--height")
        .map(|h| h.parse::<u32>().map_err(|_| format!("bad height {h}")))
        .transpose()?
        .unwrap_or(0);
    let output = match value("--output") {
        Some(o) => PathBuf::from(o),
        None => {
            let base = project_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "film".into());
            export::free_name(&Settings::load().export_folder(), &base, format.extension())
        }
    };
    let limit_mb = value("--limit")
        .map(|l| l.parse::<u32>().map_err(|_| format!("bad size limit {l}")))
        .transpose()?
        .unwrap_or(0);
    let captions = match value("--captions").as_deref() {
        None | Some("off") => 0,
        Some("burn") => 1,
        Some("track") | Some("srt") => 2,
        Some(other) => return Err(format!("--captions takes burn or track, not {other}")),
    };
    let mut settings = export::Settings::new(format, output.clone(), std::env::temp_dir());
    settings.height = height;
    settings.font = gfx::font_path(None).map(PathBuf::from);
    settings.limit_mb = limit_mb;
    settings.captions = captions;
    let plan = export::plan(&project, &settings)?;
    let mut job = job::Job::start(plan, output.clone())?;
    let mut last = -1i32;
    loop {
        if let Some(result) = job.poll() {
            result?;
            println!("{}", output.display());
            return Ok(());
        }
        let pct = (job.fraction * 100.0) as i32;
        if pct / 10 != last / 10 {
            eprintln!("niruclip edit: {pct} %");
            last = pct;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
