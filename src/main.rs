use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = "0.2.3";
const SCHEMA: u32 = 1;
const ACTIVE_STATES: &[&str] = &[
    "countdown",
    "starting",
    "recording",
    "pausing",
    "paused",
    "finalizing",
];
const TERMINAL_STATES: &[&str] = &["finished", "failed", "interrupted", "cancelled", "idle"];

static SIGNAL: AtomicI32 = AtomicI32::new(0);
static PAUSE: AtomicBool = AtomicBool::new(false);
static RESUME: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
}

extern "C" fn on_control(signal: libc::c_int) {
    if signal == libc::SIGUSR1 {
        PAUSE.store(true, Ordering::SeqCst);
    } else {
        RESUME.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Full,
    Region,
    Window,
    WebcamOnly,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "full" => Ok(Self::Full),
            "region" => Ok(Self::Region),
            "window" => Ok(Self::Window),
            "webcam-only" => Ok(Self::WebcamOnly),
            _ => Err(format!("invalid mode: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Region => "region",
            Self::Window => "window",
            Self::WebcamOnly => "webcam-only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Audio {
    None,
    Mic,
    Desktop,
    Both,
}

impl Audio {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "mic" => Ok(Self::Mic),
            "desktop" => Ok(Self::Desktop),
            "both" => Ok(Self::Both),
            _ => Err(format!("audio must be none, mic, desktop or both: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Mic => "mic",
            Self::Desktop => "desktop",
            Self::Both => "both",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Webcam {
    Off,
    Corner,
    Only,
}

impl Webcam {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "corner" => Ok(Self::Corner),
            "only" => Ok(Self::Only),
            _ => Err(format!("webcam must be off, corner or only: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Corner => "corner",
            Self::Only => "only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "top-left" => Ok(Self::TopLeft),
            "top-right" => Ok(Self::TopRight),
            "bottom-left" => Ok(Self::BottomLeft),
            "bottom-right" => Ok(Self::BottomRight),
            _ => Err(format!("invalid corner: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    fn overlay(self) -> (&'static str, &'static str) {
        match self {
            Self::TopLeft => ("16", "16"),
            Self::TopRight => ("main_w-overlay_w-16", "16"),
            Self::BottomLeft => ("16", "main_h-overlay_h-16"),
            Self::BottomRight => ("main_w-overlay_w-16", "main_h-overlay_h-16"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "small" => Ok(Self::Small),
            "medium" => Ok(Self::Medium),
            "large" => Ok(Self::Large),
            _ => Err(format!(
                "webcam-size must be small, medium or large: {value}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn factor(self) -> f64 {
        match self {
            Self::Small => 0.20,
            Self::Medium => 0.30,
            Self::Large => 0.40,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preset {
    Share,
    Small,
    Quality,
}

impl Preset {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "share" => Ok(Self::Share),
            "small" => Ok(Self::Small),
            "quality" => Ok(Self::Quality),
            _ => Err(format!("preset must be share, small or quality: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::Small => "small",
            Self::Quality => "quality",
        }
    }

    fn max_height(self) -> Option<u32> {
        match self {
            Self::Share => Some(1080),
            Self::Small => Some(720),
            Self::Quality => None,
        }
    }

    fn crf(self) -> &'static str {
        match self {
            Self::Share => "23",
            Self::Small => "28",
            Self::Quality => "20",
        }
    }

    fn camera_max_width(self) -> u32 {
        match self {
            Self::Small => 1280,
            Self::Share | Self::Quality => 1920,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncoderChoice {
    Auto,
    Software,
    Vaapi,
}

impl EncoderChoice {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "software" => Ok(Self::Software),
            "vaapi" => Ok(Self::Vaapi),
            _ => Err(format!("encoder must be auto, software or vaapi: {value}")),
        }
    }
}

/// The encoder a capture actually uses. Software (libx264) is always
/// available and is the guaranteed fallback for every hardware failure.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Encoder {
    Software,
    Vaapi(String),
}

impl Encoder {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::Vaapi(_) => "vaapi",
        }
    }
}

#[derive(Clone, Debug)]
struct Options {
    mode: Mode,
    audio: Audio,
    webcam: Webcam,
    corner: Corner,
    webcam_size: Size,
    preset: Preset,
    encoder: EncoderChoice,
    countdown: u64,
    output: Option<PathBuf>,
    screen: Option<String>,
    profile: Option<String>,
    save_profile: Option<String>,
    json: bool,
    no_notify: bool,
    list_profiles: bool,
    stop: bool,
    pause: bool,
    resume: bool,
    recover: bool,
    status: bool,
    version: bool,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Full,
            audio: Audio::Mic,
            webcam: Webcam::Off,
            corner: Corner::BottomRight,
            webcam_size: Size::Medium,
            preset: Preset::Share,
            encoder: EncoderChoice::Auto,
            countdown: 3,
            output: None,
            screen: None,
            profile: None,
            save_profile: None,
            json: false,
            no_notify: false,
            list_profiles: false,
            stop: false,
            pause: false,
            resume: false,
            recover: false,
            status: false,
            version: false,
            help: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    webcam: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    webcam_size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    countdown: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_profile: Option<String>,
    /// `auto` (default), `software` or `vaapi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoder: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

impl Config {
    fn load() -> Result<Self, String> {
        let mut config = Self {
            schema: 1,
            default_profile: None,
            encoder: None,
            profiles: BTreeMap::new(),
        };
        for (name, profile) in builtin_profiles() {
            config.profiles.insert(name, profile);
        }
        let path = config_path();
        if path.exists() {
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            let loaded: Config = toml::from_str(&text)
                .map_err(|error| format!("invalid {}: {error}", path.display()))?;
            if loaded.schema > 1 {
                return Err(format!(
                    "{} uses config schema {}, newer than supported schema 1",
                    path.display(),
                    loaded.schema
                ));
            }
            config.default_profile = loaded.default_profile;
            config.encoder = loaded.encoder;
            config.profiles.extend(loaded.profiles);
        }
        Ok(config)
    }

    fn save_profile(&mut self, name: &str, options: &Options) -> Result<(), String> {
        if !valid_profile_name(name) {
            return Err("profile names may use letters, digits, dot, dash and underscore".into());
        }
        let profile = Profile {
            description: Some("Custom capture profile".into()),
            mode: Some(options.mode.as_str().into()),
            audio: Some(options.audio.as_str().into()),
            webcam: Some(options.webcam.as_str().into()),
            corner: Some(options.corner.as_str().into()),
            webcam_size: Some(options.webcam_size.as_str().into()),
            preset: Some(options.preset.as_str().into()),
            countdown: Some(options.countdown),
            screen: options.screen.clone(),
        };
        self.profiles.insert(name.to_string(), profile);
        self.schema = 1;
        write_atomic(
            &config_path(),
            toml::to_string_pretty(self)
                .map_err(|error| format!("could not serialize profiles: {error}"))?
                .as_bytes(),
        )?;
        Ok(())
    }
}

fn builtin_profiles() -> Vec<(String, Profile)> {
    [
        (
            "screen-mic",
            "Screen and microphone",
            Mode::Full,
            Audio::Mic,
            Webcam::Off,
            Corner::BottomRight,
            Size::Medium,
            Preset::Share,
        ),
        (
            "screen-camera",
            "Screen with camera overlay and microphone",
            Mode::Full,
            Audio::Mic,
            Webcam::Corner,
            Corner::BottomRight,
            Size::Medium,
            Preset::Share,
        ),
        (
            "camera-mic",
            "Camera only with microphone",
            Mode::WebcamOnly,
            Audio::Mic,
            Webcam::Only,
            Corner::BottomRight,
            Size::Medium,
            Preset::Share,
        ),
        (
            "screen-desktop",
            "Screen with desktop audio",
            Mode::Full,
            Audio::Desktop,
            Webcam::Off,
            Corner::BottomRight,
            Size::Medium,
            Preset::Share,
        ),
    ]
    .into_iter()
    .map(
        |(name, description, mode, audio, webcam, corner, size, preset)| {
            (
                name.to_string(),
                Profile {
                    description: Some(description.to_string()),
                    mode: Some(mode.as_str().into()),
                    audio: Some(audio.as_str().into()),
                    webcam: Some(webcam.as_str().into()),
                    corner: Some(corner.as_str().into()),
                    webcam_size: Some(size.as_str().into()),
                    preset: Some(preset.as_str().into()),
                    countdown: Some(3),
                    screen: None,
                },
            )
        },
    )
    .collect()
}

fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn apply_profile(options: &mut Options, profile: &Profile) -> Result<(), String> {
    if let Some(value) = &profile.mode {
        options.mode = Mode::parse(value)?;
    }
    if let Some(value) = &profile.audio {
        options.audio = Audio::parse(value)?;
    }
    if let Some(value) = &profile.webcam {
        options.webcam = Webcam::parse(value)?;
    }
    if let Some(value) = &profile.corner {
        options.corner = Corner::parse(value)?;
    }
    if let Some(value) = &profile.webcam_size {
        options.webcam_size = Size::parse(value)?;
    }
    if let Some(value) = &profile.preset {
        options.preset = Preset::parse(value)?;
    }
    if let Some(value) = profile.countdown {
        options.countdown = value;
    }
    if let Some(value) = &profile.screen {
        options.screen = Some(value.clone());
    }
    if options.mode == Mode::WebcamOnly {
        options.webcam = Webcam::Only;
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Performance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fps: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drop_frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dup_frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out_time_us: Option<u64>,
}

impl Performance {
    fn has_frames(&self) -> bool {
        self.frame.unwrap_or(0) > 0 || self.out_time_us.unwrap_or(0) > 0
    }

    fn needs_attention(&self) -> bool {
        let frames = self.frame.unwrap_or(0);
        let dropped = self.drop_frames.unwrap_or(0);
        let duplicated = self.dup_frames.unwrap_or(0);
        if frames == 0 {
            return dropped > 0 || duplicated >= 10;
        }
        let frames = frames as f64;
        dropped as f64 / frames > 0.02 || duplicated as f64 / frames > 0.05
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(fps) = self.fps {
            parts.push(format!("{fps:.1} fps"));
        }
        if let Some(dropped) = self.drop_frames {
            parts.push(format!("{dropped} dropped"));
        }
        if let Some(duplicated) = self.dup_frames {
            parts.push(format!("{duplicated} duplicated"));
        }
        if let Some(speed) = self.speed {
            parts.push(format!("{speed:.2}x"));
        }
        parts.join(", ")
    }
}

fn parse_progress(text: &str) -> Option<Performance> {
    let mut performance = Performance::default();
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        match key {
            "frame" => {
                if let Ok(value) = value.parse() {
                    performance.frame = Some(value);
                }
            }
            "fps" => {
                if let Ok(value) = value.parse() {
                    performance.fps = Some(value);
                }
            }
            "drop_frames" => {
                if let Ok(value) = value.parse() {
                    performance.drop_frames = Some(value);
                }
            }
            "dup_frames" => {
                if let Ok(value) = value.parse() {
                    performance.dup_frames = Some(value);
                }
            }
            "speed" => {
                if let Ok(value) = value.trim_end_matches('x').parse() {
                    performance.speed = Some(value);
                }
            }
            "out_time_us" => {
                if let Ok(value) = value.parse() {
                    performance.out_time_us = Some(value);
                }
            }
            _ => {}
        }
    }
    performance.has_frames().then_some(performance)
}

fn read_progress(path: &Path) -> Option<Performance> {
    parse_progress(&fs::read_to_string(path).ok()?)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
    schema: u32,
    state: String,
    pid: u32,
    pid_start: u64,
    child_pid: u32,
    child_start: u64,
    started: f64,
    updated: f64,
    output: String,
    partial: String,
    mode: String,
    preset: String,
    audio: String,
    webcam: String,
    corner: String,
    webcam_size: String,
    width: u32,
    height: u32,
    recoverable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    performance: Option<Performance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// `software` or `vaapi`: the encoder that is producing the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoder: Option<String>,
    /// Why a requested or detected hardware encoder was not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_restore: Option<CameraRestore>,
    /// Version of the recorder that owns the session. --pause and --resume
    /// only signal a recorder that understands them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recorder: Option<String>,
    /// Present once a recording was paused: every segment in order. The
    /// first one is `partial`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    segments: Option<Vec<Segment>>,
    /// Seconds recorded in the segments before the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elapsed_before: Option<f64>,
}

impl Session {
    fn empty() -> Self {
        Self {
            schema: SCHEMA,
            state: "idle".into(),
            pid: 0,
            pid_start: 0,
            child_pid: 0,
            child_start: 0,
            started: 0.0,
            updated: now(),
            output: String::new(),
            partial: String::new(),
            mode: Mode::Full.as_str().into(),
            preset: Preset::Share.as_str().into(),
            audio: Audio::Mic.as_str().into(),
            webcam: Webcam::Off.as_str().into(),
            corner: Corner::BottomRight.as_str().into(),
            webcam_size: Size::Medium.as_str().into(),
            width: 0,
            height: 0,
            recoverable: false,
            performance: None,
            reason: None,
            encoder: None,
            fallback: None,
            camera_restore: None,
            recorder: None,
            segments: None,
            elapsed_before: None,
        }
    }

    fn active(&self) -> bool {
        ACTIVE_STATES.contains(&self.state.as_str())
    }

    fn terminal(&self) -> bool {
        TERMINAL_STATES.contains(&self.state.as_str())
    }
}

struct Lock {
    path: PathBuf,
    pid: u32,
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(owner) = read_lock(&self.path) {
            if owner.0 == self.pid {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct LockOwner {
    schema: u32,
    pid: u32,
    start: u64,
}

fn acquire_lock() -> Result<Lock, String> {
    let dir = state_dir();
    fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let path = dir.join("session.lock");
    let pid = std::process::id();
    let start = proc_start(pid).unwrap_or(0);
    for attempt in 0..6 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                let owner = LockOwner {
                    schema: SCHEMA,
                    pid,
                    start,
                };
                let text = serde_json::to_vec(&owner).map_err(|error| error.to_string())?;
                file.write_all(&text)
                    .map_err(|error| format!("could not write session lock: {error}"))?;
                return Ok(Lock { path, pid });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some((owner_pid, owner_start)) = read_lock(&path) {
                    if pid_alive(owner_pid, owner_start) {
                        return Err("a recording is already active".into());
                    }
                    let _ = fs::remove_file(&path);
                } else if attempt < 5 {
                    std::thread::sleep(Duration::from_millis(40));
                } else {
                    let _ = fs::remove_file(&path);
                }
            }
            Err(error) => return Err(format!("could not create session lock: {error}")),
        }
    }
    Err("could not acquire the recording lock".into())
}

fn read_lock(path: &Path) -> Option<(u32, u64)> {
    let text = fs::read_to_string(path).ok()?;
    let owner: LockOwner = serde_json::from_str(&text).ok()?;
    Some((owner.pid, owner.start))
}

fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("NIRUCLIP_STATE_DIR") {
        return PathBuf::from(path);
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("niruclip")
}

fn state_path() -> PathBuf {
    state_dir().join("session.json")
}

fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NIRUCLIP_CONFIG") {
        return PathBuf::from(path);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("niruclip/config.toml")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn read_state() -> Option<Session> {
    let text = fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_state(session: &Session) -> Result<(), String> {
    let path = state_path();
    let mut session = session.clone();
    session.schema = SCHEMA;
    session.updated = now();
    let text = serde_json::to_vec(&session).map_err(|error| error.to_string())?;
    write_atomic(&path, &text)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("niruclip"),
        std::process::id()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|error| format!("could not stage {}: {error}", path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("could not publish {}: {error}", path.display())
    })
}

fn proc_start(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = text.rfind(')')?;
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    fields.get(19)?.parse().ok()
}

fn pid_alive(pid: u32, start: u64) -> bool {
    if pid == 0 {
        return false;
    }
    let Some(actual) = proc_start(pid) else {
        return false;
    };
    start == 0 || actual == start
}

fn signal_pid(pid: u32, signal: libc::c_int) {
    if pid > 0 {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

fn signal_group(pid: u32, signal: libc::c_int) {
    if pid > 0 {
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR1, on_control as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, on_control as *const () as libc::sighandler_t);
    }
}

fn notify(options: &Options, summary: &str, body: &str) {
    if options.no_notify
        || std::env::var_os("NIRUCLIP_NOTIFY").as_deref() == Some(std::ffi::OsStr::new("0"))
    {
        return;
    }
    let _ = Command::new(command_path("notify-send", "NIRUCLIP_NOTIFY_SEND"))
        .args(["-a", "NIRUCLIP", "-t", "3000", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn command_path(name: &str, env_name: &str) -> String {
    std::env::var(env_name).unwrap_or_else(|_| name.to_string())
}

fn current_display() -> Result<String, String> {
    std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "DISPLAY is not set; X11 capture needs a graphical session".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn parse_geometry(value: &str) -> Option<Geometry> {
    let (width, rest) = value.split_once('x')?;
    let width: u32 = width.parse().ok()?;
    let end = rest.find(['+', '-']).unwrap_or(rest.len());
    let height: u32 = rest[..end].parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let tail = &rest[end..];
    let bytes = tail.as_bytes();
    let mut values = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let sign = bytes[index];
        if sign != b'+' && sign != b'-' {
            return None;
        }
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if start == index {
            return None;
        }
        let value: i32 = tail[start..index].parse().ok()?;
        values.push(if sign == b'-' { -value } else { value });
    }
    if values.len() != 2 {
        return None;
    }
    Some(Geometry {
        x: values[0],
        y: values[1],
        width,
        height,
    })
}

fn xrandr_geometry(screen: Option<&str>) -> Result<Geometry, String> {
    let command = command_path("xrandr", "NIRUCLIP_XRANDR");
    let output = Command::new(&command)
        .args(["--current"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run {command}: {error}"))?;
    if !output.status.success() {
        return Err("xrandr could not read the display layout".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut primary = None;
    let mut connected = None;
    let mut current = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 2 && fields[1] == "connected" {
            let geometry = fields
                .iter()
                .skip(2)
                .find_map(|field| parse_geometry(field));
            if let Some(geometry) = geometry {
                if screen == fields.first().copied() {
                    return Ok(geometry);
                }
                if fields.contains(&"primary") && primary.is_none() {
                    primary = Some(geometry);
                }
                if connected.is_none() {
                    connected = Some(geometry);
                }
            }
        }
        if current.is_none() && line.trim_start().starts_with("Screen ") {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if let Some(index) = fields.iter().position(|field| *field == "current") {
                if let (Some(width), Some(height)) = (
                    fields
                        .get(index + 1)
                        .and_then(|value| value.parse::<u32>().ok()),
                    fields
                        .get(index + 3)
                        .and_then(|value| value.parse::<u32>().ok()),
                ) {
                    current = Some(Geometry {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    });
                }
            }
        }
    }
    if let Some(name) = screen {
        return Err(format!("display output {name} is not connected"));
    }
    primary
        .or(connected)
        .or(current)
        .ok_or_else(|| "could not determine the connected display geometry".into())
}

fn slop_geometry() -> Result<Geometry, String> {
    let command = command_path("slop", "NIRUCLIP_SLOP");
    let output = Command::new(&command)
        .args(["-f", "%x %y %w %h"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("could not run {command}: {error}"))?;
    if !output.status.success() {
        return Err("selection cancelled".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let values: Vec<i32> = text
        .split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() != 4 || values[2] <= 0 || values[3] <= 0 {
        return Err("selection did not return a valid rectangle".into());
    }
    Ok(Geometry {
        x: values[0],
        y: values[1],
        width: values[2] as u32,
        height: values[3] as u32,
    })
}

fn even(value: u32) -> u32 {
    value.max(2) & !1
}

fn scaled_screen(width: u32, height: u32, preset: Preset) -> (u32, u32) {
    if let Some(max_height) = preset.max_height() {
        if height > max_height {
            let scaled_width =
                ((width as f64 * max_height as f64 / height as f64).round() as u32).max(2);
            (even(scaled_width), even(max_height))
        } else {
            (even(width), even(height))
        }
    } else {
        (even(width), even(height))
    }
}

#[derive(Debug)]
struct Plan {
    argv: Vec<String>,
    width: u32,
    height: u32,
}

const CAMERA_CONTROL: &str = "exposure_dynamic_framerate";

/// A camera control NIRUCLIP changed and must put back. It is stored in
/// session.json *before* the change, so --stop, --recover and the next start
/// can restore it after a SIGKILL, a crash or a power loss.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CameraRestore {
    device: String,
    control: String,
    previous: String,
}

fn v4l2_ctl() -> String {
    command_path("v4l2-ctl", "NIRUCLIP_V4L2CTL")
}

fn read_camera_control(device: &str) -> Option<String> {
    Command::new(v4l2_ctl())
        .args(["--device", device, "--get-ctrl", CAMERA_CONTROL])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_once(':')
                .map(|(_, value)| value.trim().to_string())
        })
        .filter(|value| value == "0" || value == "1")
}

fn set_camera_control(device: &str, control: &str, value: &str) -> bool {
    Command::new(v4l2_ctl())
        .args([
            "--device",
            device,
            "--set-ctrl",
            &format!("{control}={value}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn restore_camera(restore: &CameraRestore) {
    if !set_camera_control(&restore.device, &restore.control, &restore.previous) {
        eprintln!(
            "niruclip: could not restore {}={} on {}",
            restore.control, restore.previous, restore.device
        );
    }
}

/// Restore a camera control left behind by a recorder that is gone, and
/// clear it from the session. Returns true when the session changed.
fn restore_stale_camera(session: &mut Session) -> bool {
    if session.camera_restore.is_none() || pid_alive(session.pid, session.pid_start) {
        return false;
    }
    if let Some(restore) = session.camera_restore.take() {
        restore_camera(&restore);
    }
    true
}

struct CaptureInputs {
    argv: Vec<String>,
    geometry: Option<Geometry>,
    screen_index: Option<usize>,
    camera_index: Option<usize>,
    audio_index: usize,
}

/// Packets each live input may queue while the filter graph is busy. 8 (screen)
/// and 32 (camera) made ffmpeg block inputs under load with a camera overlay,
/// which shows up as dropped or duplicated frames.
const INPUT_QUEUE: &str = "512";

fn camera_input(camera: &str) -> Vec<String> {
    [
        "-thread_queue_size",
        INPUT_QUEUE,
        "-f",
        "v4l2",
        "-framerate",
        "30",
        "-i",
        camera,
    ]
    .iter()
    .map(|value| (*value).to_string())
    .collect()
}

fn screen_input(display: &str, geometry: &Geometry) -> Vec<String> {
    [
        "-thread_queue_size".to_string(),
        INPUT_QUEUE.to_string(),
        "-f".to_string(),
        "x11grab".to_string(),
        "-framerate".to_string(),
        "30".to_string(),
        "-video_size".to_string(),
        format!("{}x{}", geometry.width, geometry.height),
        "-i".to_string(),
        format!("{display}+{},{}", geometry.x, geometry.y),
    ]
    .to_vec()
}

fn capture_input(resolved: &Options, display: &str) -> Result<CaptureInputs, String> {
    let camera = std::env::var("NIRUCLIP_CAMERA").unwrap_or_else(|_| "/dev/video0".into());
    if resolved.mode == Mode::WebcamOnly || resolved.webcam != Webcam::Off {
        let metadata =
            fs::metadata(&camera).map_err(|_| format!("camera {camera} is unavailable"))?;
        if !metadata.file_type().is_char_device() {
            return Err(format!("camera {camera} is not a character device"));
        }
    }
    let geometry = if resolved.mode == Mode::WebcamOnly {
        None
    } else if resolved.mode == Mode::Region || resolved.mode == Mode::Window {
        Some(slop_geometry()?)
    } else {
        Some(xrandr_geometry(resolved.screen.as_deref())?)
    };
    let mut argv = Vec::new();
    let mut screen_index = None;
    let mut camera_index = None;
    let mut video_inputs = 0usize;
    match resolved.mode {
        Mode::Full | Mode::Region | Mode::Window => {
            let geometry = geometry
                .as_ref()
                .ok_or_else(|| "screen geometry is missing".to_string())?;
            if resolved.webcam == Webcam::Corner {
                // The camera is opened first on purpose. With x11grab first,
                // FFmpeg's two live inputs drift apart and the overlay path
                // duplicates or drops most of the frame stream even though
                // either source is fine on its own.
                camera_index = Some(video_inputs);
                video_inputs += 1;
                argv.extend(camera_input(&camera));
            }
            screen_index = Some(video_inputs);
            video_inputs += 1;
            argv.extend(screen_input(display, geometry));
        }
        Mode::WebcamOnly => {
            camera_index = Some(video_inputs);
            video_inputs += 1;
            argv.extend(camera_input(&camera));
        }
    }
    Ok(CaptureInputs {
        argv,
        geometry,
        screen_index,
        camera_index,
        audio_index: video_inputs,
    })
}

/// Render nodes that may carry a VAAPI encoder, in a stable order.
/// `NIRUCLIP_VAAPI_DEVICE` selects one explicitly (and lets tests point at a
/// fixture); a path that does not exist means "no render node".
fn render_nodes() -> Vec<String> {
    if let Some(device) = std::env::var_os("NIRUCLIP_VAAPI_DEVICE") {
        let device = PathBuf::from(device);
        return if device.exists() {
            vec![device.display().to_string()]
        } else {
            Vec::new()
        };
    }
    let mut nodes: Vec<String> = fs::read_dir("/dev/dri")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
                .filter(|entry| {
                    entry
                        .file_type()
                        .map(|kind| kind.is_char_device())
                        .unwrap_or(false)
                })
                .map(|entry| entry.path().display().to_string())
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes
}

fn boot_id() -> String {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// One VAAPI probe result per boot, ffmpeg binary and render node. A driver
/// or ffmpeg change normally comes with a reboot or a different binary path.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProbeCache {
    schema: u32,
    boot_id: String,
    ffmpeg: String,
    #[serde(default)]
    devices: BTreeMap<String, bool>,
}

fn probe_cache_path() -> PathBuf {
    state_dir().join("vaapi-probe.json")
}

fn read_probe_cache(ffmpeg: &str) -> ProbeCache {
    let boot = boot_id();
    fs::read_to_string(probe_cache_path())
        .ok()
        .and_then(|text| serde_json::from_str::<ProbeCache>(&text).ok())
        .filter(|cache| cache.schema == 1 && cache.boot_id == boot && cache.ffmpeg == ffmpeg)
        .unwrap_or(ProbeCache {
            schema: 1,
            boot_id: boot,
            ffmpeg: ffmpeg.to_string(),
            devices: BTreeMap::new(),
        })
}

fn store_probe_result(ffmpeg: &str, device: &str, ok: bool) {
    let mut cache = read_probe_cache(ffmpeg);
    cache.devices.insert(device.to_string(), ok);
    if let Ok(text) = serde_json::to_vec(&cache) {
        let _ = write_atomic(&probe_cache_path(), &text);
    }
}

/// A one-frame synthetic encode with a 3 s deadline. It touches no capture
/// device and writes nothing to disk.
fn probe_vaapi(ffmpeg: &str, device: &str) -> bool {
    let child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-vaapi_device",
            device,
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=256x144:rate=30",
            "-frames:v",
            "1",
            "-vf",
            "format=nv12,hwupload",
            "-c:v",
            "h264_vaapi",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                signal_group(child.id(), libc::SIGKILL);
                let _ = child.wait();
                return false;
            }
        }
    }
}

struct EncoderSelection {
    encoder: Encoder,
    fallback: Option<String>,
}

fn select_encoder(choice: EncoderChoice, ffmpeg: &str) -> EncoderSelection {
    let software = |fallback: Option<&str>| EncoderSelection {
        encoder: Encoder::Software,
        fallback: fallback.map(str::to_string),
    };
    match choice {
        EncoderChoice::Software => software(None),
        EncoderChoice::Vaapi => match render_nodes().into_iter().next() {
            // Forced VAAPI skips the probe; the first-frame retry still
            // guarantees a software recording if the encoder fails.
            Some(device) => EncoderSelection {
                encoder: Encoder::Vaapi(device),
                fallback: None,
            },
            None => software(Some("no VAAPI render node")),
        },
        EncoderChoice::Auto => {
            let nodes = render_nodes();
            if nodes.is_empty() {
                // Expected on hosts without a GPU: not a fallback worth reporting.
                return software(None);
            }
            let cache = read_probe_cache(ffmpeg);
            for device in &nodes {
                let ok = match cache.devices.get(device) {
                    Some(ok) => *ok,
                    None => {
                        let ok = probe_vaapi(ffmpeg, device);
                        store_probe_result(ffmpeg, device, ok);
                        ok
                    }
                };
                if ok {
                    return EncoderSelection {
                        encoder: Encoder::Vaapi(device.clone()),
                        fallback: None,
                    };
                }
            }
            software(Some("VAAPI probe failed"))
        }
    }
}

impl Preset {
    /// Constant QP for h264_vaapi, chosen a little above the libx264 CRF
    /// because CQP has no psycho-visual tuning; sizes stay comparable.
    fn vaapi_qp(self) -> &'static str {
        match self {
            Self::Share => "25",
            Self::Small => "30",
            Self::Quality => "22",
        }
    }
}

fn build_plan(
    resolved: &Options,
    capture: &CaptureInputs,
    partial: &Path,
    progress: &Path,
    encoder: &Encoder,
) -> Result<Plan, String> {
    let mut argv: Vec<String> = ["-hide_banner", "-loglevel", "warning", "-nostdin", "-n"]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    if let Encoder::Vaapi(device) = encoder {
        argv.extend(["-vaapi_device".into(), device.clone()]);
    }
    // With VAAPI the filters end in [vsrc] and an upload tail produces [v];
    // the software graph is unchanged.
    let vaapi = matches!(encoder, Encoder::Vaapi(_));
    let vout = if vaapi { "vsrc" } else { "v" };
    // A plain "process is still alive" check cannot tell a working capture
    // from a camera that has not delivered a single frame. FFmpeg writes
    // machine-readable progress here; run_recording uses it for both startup
    // confirmation and the live performance summary.
    argv.extend([
        "-progress".into(),
        progress.display().to_string(),
        "-stats_period".into(),
        "0.5".into(),
    ]);
    argv.extend(capture.argv.iter().cloned());
    let screen = capture.geometry.map(|geometry| {
        let (width, height) = scaled_screen(geometry.width, geometry.height, resolved.preset);
        (geometry, width, height)
    });
    let audio_index = capture.audio_index;
    if resolved.audio == Audio::Mic
        || resolved.audio == Audio::Desktop
        || resolved.audio == Audio::Both
    {
        if resolved.audio == Audio::Mic || resolved.audio == Audio::Both {
            argv.extend([
                "-thread_queue_size".into(),
                INPUT_QUEUE.into(),
                "-f".into(),
                "pulse".into(),
                "-i".into(),
                "@DEFAULT_SOURCE@".into(),
            ]);
        }
        if resolved.audio == Audio::Desktop || resolved.audio == Audio::Both {
            argv.extend([
                "-thread_queue_size".into(),
                INPUT_QUEUE.into(),
                "-f".into(),
                "pulse".into(),
                "-i".into(),
                "@DEFAULT_MONITOR@".into(),
            ]);
        }
    }
    let mut filters = Vec::new();
    let width;
    let height;
    if resolved.mode == Mode::WebcamOnly {
        let camera_index = capture
            .camera_index
            .ok_or_else(|| "camera input is missing".to_string())?;
        let max_width = resolved.preset.camera_max_width();
        filters.push(format!(
            "[{camera_index}:v]scale=w='trunc(min(iw,{max_width})/2)*2':h=-2:flags=bicubic,setsar=1[{vout}]"
        ));
        width = max_width;
        height = 0;
    } else {
        let screen_index = capture
            .screen_index
            .ok_or_else(|| "screen input is missing".to_string())?;
        let (_, out_width, out_height) =
            screen.ok_or_else(|| "screen geometry is missing".to_string())?;
        width = out_width;
        height = out_height;
        if resolved.webcam == Webcam::Corner {
            let camera_index = capture
                .camera_index
                .ok_or_else(|| "camera input is missing".to_string())?;
            let target = even(((out_width as f64) * resolved.webcam_size.factor()).round() as u32);
            let (x, y) = resolved.corner.overlay();
            filters.push(format!(
                "[{screen_index}:v]scale={out_width}:{out_height}:flags=bicubic,setsar=1[base]"
            ));
            filters.push(format!(
                "[{camera_index}:v]scale=w='trunc(min(iw,{target})/2)*2':h=-2:flags=bicubic,setsar=1[wc]"
            ));
            filters.push(format!("[base][wc]overlay={x}:{y}:format=auto[{vout}]"));
        } else {
            filters.push(format!(
                "[{screen_index}:v]scale={out_width}:{out_height}:flags=bicubic,setsar=1[{vout}]"
            ));
        }
    }
    if vaapi {
        filters.push("[vsrc]format=nv12,hwupload[v]".to_string());
    }
    let has_audio = resolved.audio != Audio::None;
    if resolved.audio == Audio::Both {
        // The mix keeps both sources at unity gain (normalize=0). A peak
        // limiter at 0.95 (level=0: no automatic make-up gain) only acts when
        // the sum would clip, so levels are otherwise unchanged.
        filters.push(format!(
            "[{audio_index}:a][{}:a]amix=inputs=2:normalize=0,alimiter=limit=0.95:level=0[a]",
            audio_index + 1
        ));
    }
    argv.extend([
        "-filter_complex".into(),
        filters.join(";"),
        "-map".into(),
        "[v]".into(),
    ]);
    if resolved.audio == Audio::Both {
        argv.extend(["-map".into(), "[a]".into()]);
    } else if has_audio {
        argv.extend(["-map".into(), format!("{audio_index}:a:0")]);
    }
    if vaapi {
        argv.extend([
            "-c:v".into(),
            "h264_vaapi".into(),
            "-rc_mode".into(),
            "CQP".into(),
            "-qp".into(),
            resolved.preset.vaapi_qp().into(),
            "-r".into(),
            "30".into(),
        ]);
    } else {
        argv.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            resolved.preset.crf().into(),
            "-r".into(),
            "30".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ]);
    }
    if has_audio {
        argv.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "160k".into()]);
    }
    argv.push(partial.display().to_string());
    Ok(Plan {
        argv,
        width,
        height,
    })
}

fn output_path(requested: Option<&Path>) -> Result<PathBuf, String> {
    let base = if let Some(path) = requested {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| format!("could not resolve output path: {error}"))?
                .join(path)
        };
        if path.is_dir() || path.to_string_lossy().ends_with('/') {
            fs::create_dir_all(&path)
                .map_err(|error| format!("could not create {}: {error}", path.display()))?;
            path.join(format!("NIRUCLIP-{}.mkv", chrono_stamp()))
        } else {
            path
        }
    } else {
        let dir = std::env::var_os("NIRUCLIP_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("Videos"));
        fs::create_dir_all(&dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
        dir.join(format!("NIRUCLIP-{}.mkv", chrono_stamp()))
    };
    let base = if base.extension().and_then(|value| value.to_str()) == Some("mkv") {
        base
    } else {
        PathBuf::from(format!("{}.mkv", base.display()))
    };
    let parent = base
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", base.display()))?;
    if !parent.is_dir() {
        return Err(format!(
            "output directory does not exist: {}",
            parent.display()
        ));
    }
    Ok(base)
}

fn unique_pair(base: &Path) -> (PathBuf, PathBuf) {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("NIRUCLIP");
    for index in 0..10000 {
        let name = if index == 0 {
            format!("{stem}.mkv")
        } else {
            format!("{stem}-{}.mkv", index + 1)
        };
        let final_path = parent.join(name);
        let partial = final_path.with_file_name(format!(
            "{}.part.mkv",
            final_path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(stem)
        ));
        if !final_path.exists() && !partial.exists() {
            return (final_path, partial);
        }
    }
    (base.to_path_buf(), base.with_extension("part.mkv"))
}

fn chrono_stamp() -> String {
    let output = Command::new("date")
        .args(["+%F_%H-%M-%S"])
        .stdin(Stdio::null())
        .output();
    output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}", now() as u64))
}

fn parse_args(argv: &[String], config: &Config) -> Result<Options, String> {
    let mut options = Options::default();
    if let Some(value) = &config.encoder {
        options.encoder = EncoderChoice::parse(value)?;
    }
    let profile_name = argv
        .windows(2)
        .find(|pair| pair[0] == "--profile")
        .map(|pair| pair[1].clone())
        .or_else(|| config.default_profile.clone());
    if let Some(name) = &profile_name {
        let profile = config
            .profiles
            .get(name)
            .ok_or_else(|| format!("unknown profile: {name}"))?;
        apply_profile(&mut options, profile)?;
        options.profile = Some(name.clone());
    }
    let mut index = 0;
    while index < argv.len() {
        let arg = argv[index].as_str();
        let next = || {
            argv.get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--full" => options.mode = Mode::Full,
            "--region" => options.mode = Mode::Region,
            "--window" => options.mode = Mode::Window,
            "--webcam-only" => {
                options.mode = Mode::WebcamOnly;
                options.webcam = Webcam::Only;
            }
            "--audio" => {
                options.audio = Audio::parse(&next()?)?;
                index += 1;
            }
            "--webcam" => {
                let value = next()?;
                options.webcam = Webcam::parse(&value)?;
                if options.webcam == Webcam::Only {
                    options.mode = Mode::WebcamOnly;
                }
                index += 1;
            }
            "--corner" => {
                options.corner = Corner::parse(&next()?)?;
                index += 1;
            }
            "--webcam-size" => {
                options.webcam_size = Size::parse(&next()?)?;
                index += 1;
            }
            "--preset" => {
                options.preset = Preset::parse(&next()?)?;
                index += 1;
            }
            "--encoder" => {
                options.encoder = EncoderChoice::parse(&next()?)?;
                index += 1;
            }
            "--countdown" => {
                options.countdown = next()?
                    .parse()
                    .map_err(|_| "countdown must be 0..10".to_string())?;
                if options.countdown > 10 {
                    return Err("countdown must be 0..10".into());
                }
                index += 1;
            }
            "--output" => {
                options.output = Some(PathBuf::from(next()?));
                index += 1;
            }
            "--screen" => {
                options.screen = Some(next()?);
                index += 1;
            }
            "--profile" => {
                index += 1;
            }
            "--save-profile" => {
                options.save_profile = Some(next()?);
                index += 1;
            }
            "--list-profiles" => options.list_profiles = true,
            "--status" => options.status = true,
            "--json" => options.json = true,
            "--stop" => options.stop = true,
            "--pause" => options.pause = true,
            "--resume" => options.resume = true,
            "--recover" => options.recover = true,
            "--no-notify" => options.no_notify = true,
            "--version" => options.version = true,
            "-h" | "--help" => options.help = true,
            _ => return Err(format!("unknown option: {arg}")),
        }
        index += 1;
    }
    if options.mode == Mode::WebcamOnly {
        options.webcam = Webcam::Only;
    }
    if options.webcam == Webcam::Only {
        options.mode = Mode::WebcamOnly;
    }
    if options.webcam == Webcam::Corner && options.mode == Mode::WebcamOnly {
        options.webcam = Webcam::Only;
    }
    if options.countdown > 10 {
        return Err("countdown must be 0..10".into());
    }
    Ok(options)
}

fn help() {
    println!(
        "NIRUCLIP {VERSION}\n\
Usage: niruclip --full|--region|--window|--webcam-only [options]\n\
       niruclip --stop | --pause | --resume | --status [--json] | --recover\n\
       niruclip --list-profiles | --save-profile NAME\n\n\
Options:\n\
  --audio none|mic|desktop|both\n\
  --webcam off|corner|only          camera placement\n\
  --corner top-left|top-right|bottom-left|bottom-right\n\
  --webcam-size small|medium|large  target share of final video width\n\
  --preset share|small|quality\n\
  --encoder auto|software|vaapi     auto uses VAAPI when a probe succeeds\n\
  --countdown 0..10\n\
  --output PATH                     existing file or directory\n\
  --screen OUTPUT                   connected RandR output for full/overlay\n\
  --profile NAME                    apply a saved or built-in profile\n\
  --no-notify                       do not use desktop notifications"
    );
}

fn status(options: &Options) {
    let session = read_state().unwrap_or_else(Session::empty);
    if options.json {
        match serde_json::to_string_pretty(&session) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("niruclip: could not serialize state: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    println!("NIRUCLIP {VERSION}");
    println!("state: {}", session.state);
    if !session.output.is_empty() {
        println!("output: {}", session.output);
    }
    if !session.partial.is_empty() {
        println!("partial: {}", session.partial);
    }
    if let Some(reason) = &session.reason {
        println!("reason: {reason}");
    }
    if let Some(encoder) = &session.encoder {
        println!("encoder: {encoder}");
    }
    if let Some(fallback) = &session.fallback {
        println!("fallback: {fallback}");
    }
    if let Some(performance) = &session.performance {
        let summary = performance.summary();
        if !summary.is_empty() {
            println!("performance: {summary}");
        }
        if performance.needs_attention() {
            println!(
                "warning: capture cannot keep up; try preset small or disable the camera overlay"
            );
        }
    }
    if session.recoverable {
        println!("recovery: available (niruclip --recover)");
    }
}

fn validate_media(path: &Path) -> bool {
    media_duration(path).is_some()
}

/// The duration of a file that contains a usable video stream, else None.
fn media_duration(path: &Path) -> Option<f64> {
    if !path.is_file() {
        return None;
    }
    let command = command_path("ffprobe", "NIRUCLIP_FFPROBE");
    let output = Command::new(&command)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
            path.to_str().unwrap_or_default(),
        ])
        .stdin(Stdio::null())
        .output();
    let output = output.ok().filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let duration = value
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let has_video = value
        .get("streams")
        .and_then(|streams| streams.as_array())
        .map(|streams| !streams.is_empty())
        .unwrap_or(false);
    (duration > 0.05 && has_video).then_some(duration)
}

fn log_tail() -> Option<String> {
    let path = state_dir().join("ffmpeg.log");
    let text = fs::read_to_string(path).ok()?;
    let tail: Vec<&str> = text.lines().rev().take(8).collect();
    let joined = tail.into_iter().rev().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(joined.chars().take(1200).collect())
    }
}

fn finish_session(session: &mut Session, state: &str, reason: Option<String>) {
    session.state = state.into();
    session.pid = 0;
    session.pid_start = 0;
    session.child_pid = 0;
    session.child_start = 0;
    session.recoverable = state == "interrupted" || state == "failed" && session.recoverable;
    session.reason = reason;
    let _ = write_state(session);
}

fn run_recording(options: Options) -> Result<(), String> {
    install_signal_handlers();
    SIGNAL.store(0, Ordering::SeqCst);
    let _lock = acquire_lock()?;
    let mut camera = None;
    let result = record(options, &mut camera);
    if let Some(restore) = camera {
        restore_camera(&restore);
        if let Some(mut session) = read_state() {
            if session.camera_restore.as_ref() == Some(&restore) {
                session.camera_restore = None;
                let _ = write_state(&session);
            }
        }
    }
    result
}

fn record(options: Options, camera: &mut Option<CameraRestore>) -> Result<(), String> {
    if let Some(mut previous) = read_state() {
        if restore_stale_camera(&mut previous) {
            let _ = write_state(&previous);
        }
        if previous.state == "interrupted" || previous.state == "failed" {
            eprintln!(
                "niruclip: previous session is {}; run niruclip --recover if a partial exists",
                previous.state
            );
        }
        if previous.active() && pid_alive(previous.pid, previous.pid_start) {
            return Err("a recording is already active".into());
        }
    }
    let display = if options.mode == Mode::WebcamOnly {
        None
    } else {
        Some(current_display()?)
    };
    let requested = output_path(options.output.as_deref())?;
    let (output, partial) = unique_pair(&requested);
    if output.exists() || partial.exists() {
        return Err(format!("output already exists: {}", output.display()));
    }
    let output_dir = output
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut disk = DiskGuard::new(&output_dir);
    match disk.check() {
        DiskLevel::Critical(free) => {
            let message = format!(
                "only {} free in {}; at least 500 MB is needed to record",
                human_bytes(free),
                output_dir.display()
            );
            notify(&options, "NIRUCLIP", &format!("Not recording: {message}"));
            return Err(message);
        }
        DiskLevel::Low(free) => {
            let message = format!(
                "low disk space: {} free in {}",
                human_bytes(free),
                output_dir.display()
            );
            eprintln!("niruclip: {message}");
            notify(&options, "NIRUCLIP", &format!("Warning: {message}"));
        }
        DiskLevel::Ok => {}
    }
    let progress_path = state_dir().join("ffmpeg.progress");
    let _ = fs::remove_file(&progress_path);
    let capture = capture_input(&options, display.as_deref().unwrap_or_default())?;
    let ffmpeg = command_path("ffmpeg", "NIRUCLIP_FFMPEG");
    let selection = select_encoder(options.encoder, &ffmpeg);
    let plan = build_plan(
        &options,
        &capture,
        &partial,
        &progress_path,
        &selection.encoder,
    )?;
    let pid = std::process::id();
    let mut session = Session {
        schema: SCHEMA,
        state: "countdown".into(),
        pid,
        pid_start: proc_start(pid).unwrap_or(0),
        child_pid: 0,
        child_start: 0,
        started: now(),
        updated: now(),
        output: output.display().to_string(),
        partial: partial.display().to_string(),
        mode: options.mode.as_str().into(),
        preset: options.preset.as_str().into(),
        audio: options.audio.as_str().into(),
        webcam: options.webcam.as_str().into(),
        corner: options.corner.as_str().into(),
        webcam_size: options.webcam_size.as_str().into(),
        width: plan.width,
        height: plan.height,
        recoverable: false,
        performance: None,
        reason: None,
        encoder: Some(selection.encoder.as_str().into()),
        fallback: selection.fallback.clone(),
        camera_restore: None,
        recorder: Some(VERSION.into()),
        segments: None,
        elapsed_before: None,
    };
    write_state(&session)?;
    if options.mode == Mode::WebcamOnly || options.webcam != Webcam::Off {
        // Ask for a steady 30 fps: dynamic exposure framerate halves the
        // camera rate in low light. The previous value is persisted first.
        let device = std::env::var("NIRUCLIP_CAMERA").unwrap_or_else(|_| "/dev/video0".into());
        if let Some(previous) = read_camera_control(&device).filter(|value| value != "0") {
            let restore = CameraRestore {
                device: device.clone(),
                control: CAMERA_CONTROL.into(),
                previous,
            };
            session.camera_restore = Some(restore.clone());
            write_state(&session)?;
            *camera = Some(restore);
            set_camera_control(&device, CAMERA_CONTROL, "0");
        }
    }
    if options.countdown > 0 {
        let mut remaining = options.countdown;
        while remaining > 0 {
            if SIGNAL.load(Ordering::SeqCst) != 0 {
                finish_session(
                    &mut session,
                    "cancelled",
                    Some("countdown cancelled".into()),
                );
                println!("NIRUCLIP countdown cancelled");
                return Ok(());
            }
            notify(
                &options,
                "NIRUCLIP",
                &format!("Recording in {remaining}..."),
            );
            eprintln!("NIRUCLIP recording in {remaining}...");
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if SIGNAL.load(Ordering::SeqCst) != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            remaining -= 1;
        }
    }
    if SIGNAL.load(Ordering::SeqCst) != 0 {
        finish_session(
            &mut session,
            "cancelled",
            Some("cancelled before capture".into()),
        );
        println!("NIRUCLIP countdown cancelled");
        return Ok(());
    }
    // A pause or resume request that arrived during the countdown is stale.
    PAUSE.store(false, Ordering::SeqCst);
    RESUME.store(false, Ordering::SeqCst);
    let mut recorder = Recorder {
        options: &options,
        capture,
        ffmpeg,
        encoder: selection.encoder,
        progress: progress_path.clone(),
    };
    let mut segments = vec![Segment {
        path: partial.display().to_string(),
        encoder: recorder.encoder.as_str().into(),
    }];
    let mut child = match recorder.start_segment(&mut session, &partial)? {
        SegmentStart::Running(child) => child,
        SegmentStart::Failed {
            reason,
            valid,
            no_frames,
        } => {
            session.recoverable = valid;
            finish_session(
                &mut session,
                if valid { "interrupted" } else { "failed" },
                Some(reason.clone()),
            );
            if no_frames {
                notify(&options, "NIRUCLIP", "Recording failed: no frames arrived");
            }
            return Err(if valid {
                "ffmpeg stopped during startup; a partial file is recoverable".into()
            } else {
                reason
            });
        }
    };
    if SIGNAL.load(Ordering::SeqCst) == 0 {
        session.state = "recording".into();
        write_state(&session)?;
        notify(&options, "NIRUCLIP", &started_message(&session));
        eprintln!("NIRUCLIP recording: {}", output.display());
    }
    let mut stop_reason: Option<String> = None;
    let mut low_warned = false;
    // How the last segment ended: Some(status) when ffmpeg exited by
    // itself, None when NIRUCLIP stopped it (stop, pause, low disk space).
    let mut unrequested_exit: Option<ExitStatus> = None;
    'capture: loop {
        let mut pause_requested = false;
        loop {
            if SIGNAL.load(Ordering::SeqCst) != 0 {
                notify(&options, "NIRUCLIP", "Saving…");
                break;
            }
            if PAUSE.swap(false, Ordering::SeqCst) {
                pause_requested = true;
                break;
            }
            RESUME.store(false, Ordering::SeqCst);
            if let Some(performance) = read_progress(&recorder.progress) {
                if session.performance.as_ref() != Some(&performance) {
                    session.performance = Some(performance);
                    let _ = write_state(&session);
                }
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    unrequested_exit = Some(exit);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    finish_session(
                        &mut session,
                        "failed",
                        Some(format!("could not wait for ffmpeg: {error}")),
                    );
                    return Err(format!("could not wait for ffmpeg: {error}"));
                }
            }
            if disk.due() {
                match disk.check() {
                    DiskLevel::Critical(free) => {
                        let message = format!(
                            "stopped: only {} free in {}",
                            human_bytes(free),
                            output_dir.display()
                        );
                        eprintln!("niruclip: {message}");
                        notify(
                            &options,
                            "NIRUCLIP",
                            &format!(
                                "Recording stopped: disk almost full ({} free). Saving…",
                                human_bytes(free)
                            ),
                        );
                        stop_reason = Some(message);
                        break;
                    }
                    DiskLevel::Low(free) if !low_warned => {
                        low_warned = true;
                        notify(
                            &options,
                            "NIRUCLIP",
                            &format!("Low disk space: {} free", human_bytes(free)),
                        );
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if unrequested_exit.is_none() {
            session.state = if pause_requested {
                "pausing".into()
            } else {
                "finalizing".into()
            };
            let _ = write_state(&session);
            stop_child(&mut child, Duration::from_secs(8));
        }
        if let Some(performance) = read_progress(&recorder.progress) {
            session.performance = Some(performance);
        }
        if !pause_requested || unrequested_exit.is_some() {
            break 'capture;
        }
        // Paused: the segment is closed and on disk; no ffmpeg is running.
        let current = segments.last().map(|segment| PathBuf::from(&segment.path));
        if let Some(duration) = current.as_deref().and_then(media_duration) {
            session.elapsed_before = Some(session.elapsed_before.unwrap_or(0.0) + duration);
        }
        session.state = "paused".into();
        session.child_pid = 0;
        session.child_start = 0;
        session.performance = None;
        write_state(&session)?;
        notify(&options, "NIRUCLIP", "Recording paused");
        eprintln!("NIRUCLIP paused");
        if !wait_for_resume() {
            notify(&options, "NIRUCLIP", "Saving…");
            break 'capture;
        }
        let next = segment_path(&partial, segments.len() + 1);
        segments.push(Segment {
            path: next.display().to_string(),
            encoder: recorder.encoder.as_str().into(),
        });
        session.segments = Some(segments.clone());
        write_state(&session)?;
        match recorder.start_segment(&mut session, &next)? {
            SegmentStart::Running(next_child) => {
                child = next_child;
                if let Some(segment) = segments.last_mut() {
                    // A VAAPI failure on resume falls back to software.
                    segment.encoder = recorder.encoder.as_str().into();
                }
                session.segments = Some(segments.clone());
                session.state = "recording".into();
                write_state(&session)?;
                notify(&options, "NIRUCLIP", "Recording resumed");
                eprintln!("NIRUCLIP resumed");
            }
            SegmentStart::Failed { reason, valid, .. } => {
                // Never lose what is already recorded: save the earlier
                // segments and report why recording could not resume.
                if !valid {
                    let _ = fs::remove_file(&next);
                    segments.pop();
                    session.segments = (segments.len() > 1).then(|| segments.clone());
                }
                stop_reason = Some(format!("could not resume: {reason}"));
                notify(&options, "NIRUCLIP", "Could not resume; saving…");
                break 'capture;
            }
        }
    }
    session.child_pid = 0;
    session.child_start = 0;
    session.state = "finalizing".into();
    let _ = write_state(&session);
    let paths: Vec<PathBuf> = segments.iter().map(|s| PathBuf::from(&s.path)).collect();
    let valid: Vec<PathBuf> = paths
        .iter()
        .filter(|path| validate_media(path))
        .cloned()
        .collect();
    let completed = match unrequested_exit {
        None => true,
        Some(status) => status.success(),
    };
    if valid.is_empty() {
        let reason = log_tail().or_else(|| Some("no usable video was produced".into()));
        session.recoverable = false;
        finish_session(&mut session, "failed", reason);
        notify(
            &options,
            "NIRUCLIP",
            "Recording failed; see niruclip --status",
        );
        return Err("recording failed; no usable video was produced".into());
    }
    if !completed {
        session.recoverable = true;
        finish_session(
            &mut session,
            "interrupted",
            log_tail().or_else(|| Some("ffmpeg stopped unexpectedly".into())),
        );
        notify(
            &options,
            "NIRUCLIP",
            "Recording interrupted; a partial file is available",
        );
        return Err("ffmpeg stopped before finalizing; partial retained".into());
    }
    let same_encoder = segments
        .windows(2)
        .all(|pair| pair[0].encoder == pair[1].encoder);
    match finalize_segments(&valid, same_encoder, &output) {
        Ok(saved) => {
            session.output = saved.output.display().to_string();
            session.recoverable = false;
            session.segments = None;
            let reason = match (stop_reason, saved.note) {
                (Some(a), Some(b)) => Some(format!("{a}; {b}")),
                (a, b) => a.or(b),
            };
            finish_session(&mut session, "finished", reason);
            let _ = fs::remove_file(state_dir().join("ffmpeg.log"));
            let _ = fs::remove_file(&progress_path);
            notify(
                &options,
                "NIRUCLIP",
                &format!("Saved {}", saved.output.display()),
            );
            println!("NIRUCLIP saved: {}", saved.output.display());
            Ok(())
        }
        Err(error) => {
            session.recoverable = true;
            finish_session(&mut session, "failed", Some(error.clone()));
            notify(
                &options,
                "NIRUCLIP",
                "Recording could not be finalized; run niruclip --recover",
            );
            Err(format!("could not finalize recording: {error}"))
        }
    }
}

/// Free-space thresholds for the output directory.
const DISK_WARN_BYTES: u64 = 2_000_000_000;
const DISK_STOP_BYTES: u64 = 500_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiskLevel {
    Ok,
    Low(u64),
    Critical(u64),
}

impl DiskLevel {
    fn classify(free: u64) -> Self {
        if free < DISK_STOP_BYTES {
            Self::Critical(free)
        } else if free < DISK_WARN_BYTES {
            Self::Low(free)
        } else {
            Self::Ok
        }
    }
}

/// Checks the free space of the output directory before a recording and
/// every 30 s while recording, inside the existing recorder loop (one
/// statvfs call, no process spawned).
struct DiskGuard {
    dir: PathBuf,
    interval: Duration,
    last: Instant,
}

impl DiskGuard {
    fn new(dir: &Path) -> Self {
        // Test hook only: shortens the 30 s interval for tests/lifecycle.sh.
        let interval = std::env::var("NIRUCLIP_TEST_DISK_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(30));
        Self {
            dir: dir.to_path_buf(),
            interval,
            last: Instant::now(),
        }
    }

    fn due(&self) -> bool {
        self.last.elapsed() >= self.interval
    }

    fn check(&mut self) -> DiskLevel {
        self.last = Instant::now();
        free_bytes(&self.dir).map_or(DiskLevel::Ok, DiskLevel::classify)
    }
}

fn free_bytes(dir: &Path) -> Option<u64> {
    // Test hook only: a file holding the free byte count replaces statvfs,
    // so the thresholds can be tested without filling a disk.
    if let Some(path) = std::env::var_os("NIRUCLIP_TEST_FREE_BYTES_FILE") {
        return fs::read_to_string(path).ok()?.trim().parse().ok();
    }
    let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

/// One ffmpeg run of a recording. Pause closes a segment; resume starts the
/// next one; the stop concatenates them losslessly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Segment {
    path: String,
    encoder: String,
}

fn segment_path(partial: &Path, index: usize) -> PathBuf {
    let name = partial
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("NIRUCLIP.part.mkv");
    let base = name.strip_suffix(".part.mkv").unwrap_or(name);
    let mut attempt = 0;
    loop {
        let candidate = if attempt == 0 {
            partial.with_file_name(format!("{base}.seg{index}.part.mkv"))
        } else {
            partial.with_file_name(format!("{base}.seg{index}-{attempt}.part.mkv"))
        };
        if !candidate.exists() || attempt > 1000 {
            return candidate;
        }
        attempt += 1;
    }
}

struct Saved {
    output: PathBuf,
    note: Option<String>,
}

/// Turn the valid segments of a recording into saved files without ever
/// losing one: a single segment is renamed; several are joined with the
/// concat demuxer (stream copy) and the join is validated before the
/// segments are removed. If they cannot be joined, each is saved on its own.
fn finalize_segments(
    valid: &[PathBuf],
    same_encoder: bool,
    output: &Path,
) -> Result<Saved, String> {
    let target = if output.exists() {
        unique_pair(output).0
    } else {
        output.to_path_buf()
    };
    if valid.len() == 1 {
        fs::rename(&valid[0], &target)
            .map_err(|error| format!("could not finalize {}: {error}", valid[0].display()))?;
        return Ok(Saved {
            output: target,
            note: None,
        });
    }
    let mut join_error = if same_encoder {
        match join_segments(valid, &target) {
            Ok(()) => {
                for segment in valid {
                    let _ = fs::remove_file(segment);
                }
                return Ok(Saved {
                    output: target,
                    note: None,
                });
            }
            Err(error) => error,
        }
    } else {
        "segments use different encoders".to_string()
    };
    let mut saved = Vec::new();
    for segment in valid {
        let destination = if saved.is_empty() && !target.exists() {
            target.clone()
        } else {
            unique_pair(&target).0
        };
        match fs::rename(segment, &destination) {
            Ok(()) => saved.push(destination),
            Err(error) => {
                join_error = format!(
                    "{join_error}; could not save {}: {error}",
                    segment.display()
                )
            }
        }
    }
    let first = saved
        .first()
        .cloned()
        .ok_or_else(|| format!("could not save any segment: {join_error}"))?;
    Ok(Saved {
        output: first,
        note: Some(format!(
            "saved as {} separate parts ({join_error}): {}",
            saved.len(),
            saved
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    })
}

fn join_segments(valid: &[PathBuf], target: &Path) -> Result<(), String> {
    let list = state_dir().join("concat.txt");
    let mut text = String::from("ffconcat version 1.0\n");
    for segment in valid {
        let path = segment.display().to_string().replace('\'', "'\\''");
        text.push_str(&format!("file '{path}'\n"));
    }
    write_atomic(&list, text.as_bytes())?;
    let joined = target.with_file_name(format!(
        "{}.joined.part.mkv",
        target
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("NIRUCLIP")
    ));
    if joined.exists() {
        return Err(format!("{} already exists", joined.display()));
    }
    let status = Command::new(command_path("ffmpeg", "NIRUCLIP_FFMPEG"))
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-map", "0", "-c", "copy"])
        .arg(&joined)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = fs::remove_file(&list);
    let ok = status.map(|status| status.success()).unwrap_or(false);
    let expected: f64 = valid.iter().filter_map(|path| media_duration(path)).sum();
    let joined_duration = if ok { media_duration(&joined) } else { None };
    match joined_duration {
        // A join that lost part of the material is not accepted.
        Some(duration) if duration + 0.5 >= expected * 0.9 => fs::rename(&joined, target)
            .map_err(|error| format!("could not finalize {}: {error}", joined.display())),
        _ => {
            let _ = fs::remove_file(&joined);
            Err("the segments could not be joined".into())
        }
    }
}

struct Recorder<'a> {
    options: &'a Options,
    capture: CaptureInputs,
    ffmpeg: String,
    encoder: Encoder,
    progress: PathBuf,
}

enum SegmentStart {
    Running(std::process::Child),
    Failed {
        reason: String,
        valid: bool,
        no_frames: bool,
    },
}

impl Recorder<'_> {
    /// Start ffmpeg into `path` and wait for its first frame. A VAAPI
    /// encoder that dies before the first frame is retried exactly once in
    /// software, so hardware encoding never turns a working recording into a
    /// failed one; the reason is recorded in the session.
    fn start_segment(
        &mut self,
        session: &mut Session,
        path: &Path,
    ) -> Result<SegmentStart, String> {
        let mut retried = false;
        loop {
            let _ = fs::remove_file(&self.progress);
            let plan = build_plan(
                self.options,
                &self.capture,
                path,
                &self.progress,
                &self.encoder,
            )?;
            let mut child = match spawn_ffmpeg(&self.ffmpeg, &plan.argv) {
                Ok(child) => child,
                Err(reason) => {
                    return Ok(SegmentStart::Failed {
                        reason,
                        valid: false,
                        no_frames: false,
                    })
                }
            };
            session.state = "starting".into();
            session.child_pid = child.id();
            session.child_start = proc_start(child.id()).unwrap_or(0);
            session.encoder = Some(self.encoder.as_str().into());
            write_state(session)?;
            match wait_first_frame(&mut child, &self.progress) {
                Startup::Frames(performance) => {
                    session.performance = Some(performance);
                    return Ok(SegmentStart::Running(child));
                }
                Startup::Signalled => return Ok(SegmentStart::Running(child)),
                Startup::Exited(status) => {
                    let valid = validate_media(path);
                    if let (Encoder::Vaapi(device), false, false) = (&self.encoder, valid, retried)
                    {
                        let detail = log_tail().unwrap_or_else(|| "no ffmpeg output".into());
                        let reason: String = format!(
                            "h264_vaapi on {device} failed before the first frame ({status}): {detail}"
                        )
                        .chars()
                        .take(600)
                        .collect();
                        eprintln!("niruclip: {reason}; retrying with software encoding");
                        if self.options.encoder == EncoderChoice::Auto {
                            store_probe_result(&self.ffmpeg, device, false);
                        }
                        let _ = fs::remove_file(path);
                        self.encoder = Encoder::Software;
                        session.encoder = Some(self.encoder.as_str().into());
                        session.fallback = Some(reason);
                        retried = true;
                        continue;
                    }
                    return Ok(SegmentStart::Failed {
                        reason: format!("ffmpeg exited during startup: {status}"),
                        valid,
                        no_frames: false,
                    });
                }
                Startup::NoFrames => {
                    session.state = "finalizing".into();
                    let _ = write_state(session);
                    stop_child(&mut child, Duration::from_secs(3));
                    session.performance = read_progress(&self.progress);
                    return Ok(SegmentStart::Failed {
                        reason: "capture produced no frames; check the camera and screen source"
                            .into(),
                        valid: validate_media(path),
                        no_frames: true,
                    });
                }
            }
        }
    }
}

/// Ask ffmpeg to finish its file (SIGINT), then escalate if it hangs.
fn stop_child(child: &mut std::process::Child, grace: Duration) -> Option<ExitStatus> {
    if let Ok(Some(status)) = child.try_wait() {
        return Some(status);
    }
    signal_group(child.id(), libc::SIGINT);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    signal_group(child.id(), libc::SIGTERM);
    std::thread::sleep(Duration::from_secs(1));
    if child.try_wait().ok().flatten().is_none() {
        signal_group(child.id(), libc::SIGKILL);
    }
    child.wait().ok()
}

/// Sleep until --resume (true) or a stop signal (false). Blocks in
/// sigsuspend, so a paused recording costs no CPU and no polling.
fn wait_for_resume() -> bool {
    unsafe {
        let mut block: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        for signal in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGUSR1,
            libc::SIGUSR2,
        ] {
            libc::sigaddset(&mut block, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut previous);
        PAUSE.store(false, Ordering::SeqCst);
        while SIGNAL.load(Ordering::SeqCst) == 0 && !RESUME.load(Ordering::SeqCst) {
            libc::sigsuspend(&previous);
            PAUSE.store(false, Ordering::SeqCst);
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
    }
    RESUME.store(false, Ordering::SeqCst);
    SIGNAL.load(Ordering::SeqCst) == 0
}

fn started_message(session: &Session) -> String {
    match (session.encoder.as_deref(), session.fallback.as_deref()) {
        (Some("vaapi"), _) => "Recording started (VAAPI hardware encoding)".into(),
        (_, Some(_)) => "Recording started (software encoding; VAAPI unavailable)".into(),
        _ => "Recording started".into(),
    }
}

fn spawn_ffmpeg(ffmpeg: &str, argv: &[String]) -> Result<std::process::Child, String> {
    let log_path = state_dir().join("ffmpeg.log");
    let log = File::create(&log_path)
        .map_err(|error| format!("could not create {}: {error}", log_path.display()))?;
    let mut command = Command::new(ffmpeg);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    command.process_group(0);
    command
        .spawn()
        .map_err(|error| format!("could not start ffmpeg: {error}"))
}

enum Startup {
    Frames(Performance),
    Signalled,
    Exited(ExitStatus),
    NoFrames,
}

/// "ffmpeg is alive" is not the same as "frames are arriving". Wait for
/// encoded progress before announcing a recording, and fail clearly when a
/// camera or screen source never delivers its first frame.
fn wait_first_frame(child: &mut std::process::Child, progress: &Path) -> Startup {
    let startup_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < startup_deadline {
        if SIGNAL.load(Ordering::SeqCst) != 0 {
            return Startup::Signalled;
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Startup::Exited(status);
        }
        if let Some(performance) = read_progress(progress) {
            if performance.has_frames() {
                return Startup::Frames(performance);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Startup::NoFrames
}

fn wait_for_terminal(timeout: Duration) -> Option<Session> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(session) = read_state() {
            if session.terminal() {
                return Some(session);
            }
        }
        if Instant::now() >= deadline {
            return read_state();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop_recording() -> Result<(), String> {
    let Some(mut session) = read_state() else {
        return Err("no active recording".into());
    };
    if !session.active() {
        return Err(format!("no active recording (state: {})", session.state));
    }
    if pid_alive(session.pid, session.pid_start) {
        signal_pid(session.pid, libc::SIGINT);
        if let Some(done) = wait_for_terminal(Duration::from_secs(20)) {
            match done.state.as_str() {
                "finished" => {
                    println!("NIRUCLIP stopped: {}", done.output);
                    return Ok(());
                }
                "cancelled" => {
                    println!("NIRUCLIP cancelled");
                    return Ok(());
                }
                "interrupted" => {
                    println!("NIRUCLIP stopped; partial retained: {}", done.partial);
                    return Err("recording interrupted; use niruclip --recover".into());
                }
                "failed" => return Err(done.reason.unwrap_or_else(|| "recording failed".into())),
                _ => return Err("recording did not stop in time".into()),
            }
        }
        signal_pid(session.pid, libc::SIGTERM);
        if wait_for_terminal(Duration::from_secs(3)).is_some() {
            return Err("recording was terminated; check niruclip --status".into());
        }
    }
    if pid_alive(session.child_pid, session.child_start) {
        signal_group(session.child_pid, libc::SIGINT);
        std::thread::sleep(Duration::from_millis(500));
        signal_group(session.child_pid, libc::SIGTERM);
    }
    restore_stale_camera(&mut session);
    session.recoverable = session_media(&session)
        .iter()
        .any(|path| validate_media(path));
    session.state = if session.recoverable {
        "interrupted".into()
    } else {
        "failed".into()
    };
    session.pid = 0;
    session.pid_start = 0;
    session.child_pid = 0;
    session.child_start = 0;
    session.reason = Some("recording process is gone".into());
    write_state(&session)?;
    Err(if session.recoverable {
        "recording process is gone; use niruclip --recover".into()
    } else {
        "recording process is gone and no usable partial was found".into()
    })
}

/// Every file a session may have recorded into, in order.
fn session_media(session: &Session) -> Vec<PathBuf> {
    match &session.segments {
        Some(segments) if !segments.is_empty() => segments
            .iter()
            .map(|segment| PathBuf::from(&segment.path))
            .collect(),
        _ => vec![PathBuf::from(&session.partial)],
    }
}

/// --pause (SIGUSR1) and --resume (SIGUSR2): ask the recorder to close the
/// current segment or start the next one, and wait for it to confirm.
fn control_recording(pause: bool) -> Result<(), String> {
    let session = read_state().ok_or_else(|| "no active recording".to_string())?;
    if !session.active() || !pid_alive(session.pid, session.pid_start) {
        return Err(format!("no active recording (state: {})", session.state));
    }
    if session.recorder.is_none() {
        return Err(
            "the running recorder predates pause support; stop it with niruclip --stop".into(),
        );
    }
    let (from, target, signal, verb) = if pause {
        ("recording", "paused", libc::SIGUSR1, "pause")
    } else {
        ("paused", "recording", libc::SIGUSR2, "resume")
    };
    if session.state == target {
        println!("NIRUCLIP already {target}");
        return Ok(());
    }
    if session.state != from {
        return Err(format!("cannot {verb} while {}", session.state));
    }
    signal_pid(session.pid, signal);
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        let Some(current) = read_state() else {
            continue;
        };
        if current.state == target {
            println!("NIRUCLIP {}", if pause { "paused" } else { "resumed" });
            return Ok(());
        }
        if current.terminal() {
            return Err(current
                .reason
                .unwrap_or_else(|| format!("recording ended ({})", current.state)));
        }
    }
    Err(format!("the recorder did not {verb} in time"))
}

fn recover() -> Result<(), String> {
    let Some(mut session) = read_state() else {
        return Err("no recording state to recover".into());
    };
    if session.active() && pid_alive(session.pid, session.pid_start) {
        return Err("the recording is still running; use niruclip --stop".into());
    }
    if restore_stale_camera(&mut session) {
        write_state(&session)?;
    }
    if session.state == "finished" {
        println!("NIRUCLIP recording already saved: {}", session.output);
        return Ok(());
    }
    if let Some(segments) = session.segments.clone().filter(|list| list.len() > 1) {
        let valid: Vec<PathBuf> = segments
            .iter()
            .map(|segment| PathBuf::from(&segment.path))
            .filter(|path| validate_media(path))
            .collect();
        if valid.is_empty() {
            session.recoverable = false;
            session.reason = Some("no segment is a usable video".into());
            let _ = write_state(&session);
            return Err("no segment of the previous session is a usable video".into());
        }
        let same_encoder = segments
            .windows(2)
            .all(|pair| pair[0].encoder == pair[1].encoder);
        let saved = finalize_segments(&valid, same_encoder, Path::new(&session.output))?;
        session.state = "finished".into();
        session.output = saved.output.display().to_string();
        session.recoverable = false;
        session.segments = None;
        session.pid = 0;
        session.pid_start = 0;
        session.child_pid = 0;
        session.child_start = 0;
        session.reason = Some(match saved.note {
            Some(note) => format!("recovered from {} segments; {note}", valid.len()),
            None => format!("recovered from {} segments", valid.len()),
        });
        write_state(&session)?;
        println!("NIRUCLIP recovered: {}", saved.output.display());
        return Ok(());
    }
    let partial = PathBuf::from(&session.partial);
    let output = PathBuf::from(&session.output);
    let candidate = if partial.is_file() {
        partial
    } else if output.is_file() {
        output.clone()
    } else {
        return Err("no partial or saved file exists for the previous session".into());
    };
    if !validate_media(&candidate) {
        session.recoverable = false;
        session.reason = Some("partial file is not a usable video".into());
        let _ = write_state(&session);
        return Err("the partial file is not a usable video".into());
    }
    let final_path = if session.output.is_empty() || candidate == output {
        candidate.clone()
    } else if output.exists() {
        let (unique, _) = unique_pair(&output);
        unique
    } else {
        output.clone()
    };
    if candidate != final_path {
        fs::rename(&candidate, &final_path)
            .map_err(|error| format!("could not recover {}: {error}", candidate.display()))?;
    }
    session.state = "finished".into();
    session.output = final_path.display().to_string();
    session.recoverable = false;
    session.pid = 0;
    session.pid_start = 0;
    session.child_pid = 0;
    session.child_start = 0;
    session.reason = Some("recovered from partial file".into());
    write_state(&session)?;
    println!("NIRUCLIP recovered: {}", final_path.display());
    Ok(())
}

fn list_profiles(config: &Config) {
    for (name, profile) in &config.profiles {
        println!(
            "{name}\t{}\t{}\t{}\t{}",
            profile.description.as_deref().unwrap_or("Capture profile"),
            profile.mode.as_deref().unwrap_or("full"),
            profile.audio.as_deref().unwrap_or("mic"),
            profile.webcam.as_deref().unwrap_or("off")
        );
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = (|| -> Result<(), String> {
        if argv.iter().any(|arg| arg == "--version") {
            println!("NIRUCLIP {VERSION}");
            return Ok(());
        }
        if argv.is_empty() || argv.iter().any(|arg| arg == "-h" || arg == "--help") {
            help();
            return Ok(());
        }
        let config = Config::load()?;
        let options = parse_args(&argv, &config)?;
        if options.version {
            println!("NIRUCLIP {VERSION}");
            return Ok(());
        }
        if options.help {
            help();
            return Ok(());
        }
        if options.list_profiles {
            list_profiles(&config);
            return Ok(());
        }
        if let Some(name) = &options.save_profile {
            let mut config = config;
            config.save_profile(name, &options)?;
            println!("NIRUCLIP profile saved: {name}");
            return Ok(());
        }
        if options.stop {
            return stop_recording();
        }
        if options.pause {
            return control_recording(true);
        }
        if options.resume {
            return control_recording(false);
        }
        if options.recover {
            return recover();
        }
        if options.status {
            status(&options);
            return Ok(());
        }
        run_recording(options)
    })();
    if let Err(error) = result {
        eprintln!("niruclip: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_with_offsets_and_negative_origin() {
        assert_eq!(
            parse_geometry("1920x1200+0+0"),
            Some(Geometry {
                x: 0,
                y: 0,
                width: 1920,
                height: 1200
            })
        );
        assert_eq!(
            parse_geometry("2560x1440-1920+120"),
            Some(Geometry {
                x: -1920,
                y: 120,
                width: 2560,
                height: 1440
            })
        );
        assert_eq!(
            parse_geometry("1200x1920+0-200"),
            Some(Geometry {
                x: 0,
                y: -200,
                width: 1200,
                height: 1920
            })
        );
        assert!(parse_geometry("1200+0+0").is_none());
    }

    #[test]
    fn presets_never_upscale_and_stay_even() {
        assert_eq!(scaled_screen(1920, 1200, Preset::Share), (1728, 1080));
        assert_eq!(scaled_screen(1280, 720, Preset::Share), (1280, 720));
        assert_eq!(scaled_screen(1919, 1199, Preset::Quality), (1918, 1198));
    }

    #[test]
    fn config_profile_validates_names() {
        assert!(valid_profile_name("screen-mic"));
        assert!(valid_profile_name("min_profil.1"));
        assert!(!valid_profile_name("../escape"));
        assert!(!valid_profile_name("bad name"));
    }

    #[test]
    fn progress_reports_frames_and_capture_health() {
        let text = "frame=120\nfps=29.8\ndrop_frames=0\ndup_frames=0\nspeed=0.99x\nout_time_us=4000000\nprogress=continue\n";
        let progress = parse_progress(text).unwrap();
        assert_eq!(progress.frame, Some(120));
        assert_eq!(progress.drop_frames, Some(0));
        assert_eq!(progress.dup_frames, Some(0));
        assert!(!progress.needs_attention());
        assert!(progress.has_frames());
    }

    #[test]
    fn progress_flags_a_stream_that_cannot_keep_up() {
        let text = "frame=90\nfps=20.0\ndrop_frames=4\ndup_frames=55\nspeed=0.68x\nout_time_us=3000000\nprogress=continue\n";
        let progress = parse_progress(text).unwrap();
        assert!(progress.needs_attention());
        assert!(progress.summary().contains("4 dropped"));
        assert!(progress.summary().contains("55 duplicated"));
        assert!(parse_progress("frame=0\nfps=0\nprogress=continue\n").is_none());
    }

    #[test]
    fn segments_are_named_after_the_partial() {
        let partial = Path::new("/nonexistent/NIRUCLIP-x.part.mkv");
        assert_eq!(
            segment_path(partial, 2),
            PathBuf::from("/nonexistent/NIRUCLIP-x.seg2.part.mkv")
        );
    }

    #[test]
    fn paused_sessions_are_active() {
        let mut session = Session::empty();
        session.state = "paused".into();
        assert!(session.active());
        assert!(!session.terminal());
    }

    #[test]
    fn builtins_include_camera_only() {
        let profiles = builtin_profiles();
        let camera = profiles
            .iter()
            .find(|(name, _)| name == "camera-mic")
            .map(|(_, profile)| profile)
            .unwrap();
        assert_eq!(camera.mode.as_deref(), Some("webcam-only"));
        assert_eq!(camera.webcam.as_deref(), Some("only"));
    }
}
