mod camera;

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

const VERSION: &str = "0.4.0";
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

    /// (right, bottom)
    fn placement(self) -> (bool, bool) {
        match self {
            Self::TopLeft => (false, false),
            Self::TopRight => (true, false),
            Self::BottomLeft => (false, true),
            Self::BottomRight => (true, true),
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

    /// Camera overlay height as a share of the output height.
    fn height_share(self) -> f64 {
        match self {
            Self::Small => 0.20,
            Self::Medium => 0.26,
            Self::Large => 0.34,
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
    /// Camera device; None picks the first regular capture camera.
    camera: Option<String>,
    camera_shape: camera::Shape,
    camera_mirror: bool,
    /// `theme` (NIRUX accent), `none` or a hex colour.
    camera_border: String,
    check_camera: bool,
    preview: bool,
    /// `mp4` or `gif`.
    export: Option<String>,
    share: bool,
    /// A recording for --export/--share; default is the last saved one.
    input: Option<PathBuf>,
    trim_from: Option<f64>,
    trim_to: Option<f64>,
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
            camera: None,
            camera_shape: camera::Shape::Rounded,
            camera_mirror: false,
            camera_border: "theme".into(),
            check_camera: false,
            preview: false,
            export: None,
            share: false,
            input: None,
            trim_from: None,
            trim_to: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_mirror: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_border: Option<String>,
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
    /// Camera defaults for every recording; a profile can override them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_mirror: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera_border: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

impl Config {
    fn load() -> Result<Self, String> {
        let mut config = Self {
            schema: 1,
            default_profile: None,
            encoder: None,
            camera: None,
            camera_shape: None,
            camera_mirror: None,
            camera_border: None,
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
            config.camera = loaded.camera;
            config.camera_shape = loaded.camera_shape;
            config.camera_mirror = loaded.camera_mirror;
            config.camera_border = loaded.camera_border;
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
            camera: options.camera.clone(),
            camera_shape: Some(options.camera_shape.as_str().into()),
            camera_mirror: Some(options.camera_mirror),
            camera_border: Some(options.camera_border.clone()),
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
                    ..Profile::default()
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

fn valid_border(value: &str) -> Result<String, String> {
    if value == "theme" || value == "none" || camera::parse_hex(value).is_some() {
        Ok(value.to_string())
    } else {
        Err(format!(
            "camera border must be theme, none or a hex colour: {value}"
        ))
    }
}

fn apply_camera_settings(
    options: &mut Options,
    device: Option<&str>,
    shape: Option<&str>,
    mirror: Option<bool>,
    border: Option<&str>,
) -> Result<(), String> {
    if let Some(value) = device.filter(|value| !value.is_empty() && *value != "auto") {
        options.camera = Some(value.to_string());
    }
    if let Some(value) = shape {
        options.camera_shape = camera::Shape::parse(value)?;
    }
    if let Some(value) = mirror {
        options.camera_mirror = value;
    }
    if let Some(value) = border {
        options.camera_border = valid_border(value)?;
    }
    Ok(())
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
    apply_camera_settings(
        options,
        profile.camera.as_deref(),
        profile.camera_shape.as_deref(),
        profile.camera_mirror,
        profile.camera_border.as_deref(),
    )?;
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
    /// The camera in use and whether its picture is usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    camera: Option<CameraStatus>,
}

/// Camera health as shown by --status, NIRUBAR and NIRU Launcher.
/// `status`: ok, checking, privacy, frozen, no-frames, busy or missing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CameraStatus {
    device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl CameraStatus {
    fn problem(&self) -> bool {
        !matches!(self.status.as_str(), "ok" | "checking")
    }
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
            camera: None,
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
    /// The camera and the mode requested from it (None: ffmpeg's default).
    camera: Option<camera::Camera>,
    camera_mode: Option<camera::Mode>,
    camera_status: Option<CameraStatus>,
}

/// Packets each live input may queue while the filter graph is busy. 8 (screen)
/// and 32 (camera) made ffmpeg block inputs under load with a camera overlay,
/// which shows up as dropped or duplicated frames.
const INPUT_QUEUE: &str = "512";

/// The v4l2 input. An explicit pixel format and size matter: without them
/// ffmpeg takes whatever the driver last used, which on most webcams is
/// YUYV 640x480. `-ts abs` turns the kernel's monotonic frame timestamps into
/// wall-clock time like x11grab and pulse use, so with `-copyts` every input
/// shares one clock (see build_plan).
fn camera_input(device: &str, mode: Option<&camera::Mode>) -> Vec<String> {
    let mut argv: Vec<String> = ["-thread_queue_size", INPUT_QUEUE, "-f", "v4l2"]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let mut framerate = "30".to_string();
    if let Some(mode) = mode {
        if let Some(format) = mode.ffmpeg_format() {
            argv.extend(["-input_format".into(), format.into()]);
        }
        argv.extend([
            "-video_size".into(),
            format!("{}x{}", mode.width, mode.height),
        ]);
        if mode.fps > 0.0 && mode.fps < 29.5 {
            framerate = format!("{}", mode.fps.round() as u32);
        }
    }
    argv.extend([
        "-framerate".into(),
        framerate,
        "-ts".into(),
        "abs".into(),
        "-i".into(),
        device.into(),
    ]);
    argv
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

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

/// The explicit camera: option/profile/config, then NIRUCLIP_CAMERA.
fn explicit_camera(options: &Options) -> Option<String> {
    options
        .camera
        .clone()
        .or_else(|| std::env::var("NIRUCLIP_CAMERA").ok())
        .filter(|value| !value.is_empty())
}

fn busy_message(device: &str) -> String {
    let holders = camera::holders(device);
    if holders.is_empty() {
        format!("camera {device} is in use by another program")
    } else {
        format!("camera {device} is in use by {}", holders.join(", "))
    }
}

/// Find the camera, check that nobody else is streaming from it and pick
/// its capture mode. A camera-only recording fails clearly; a screen
/// recording with a camera overlay continues without the camera and says so.
fn prepare_camera(
    resolved: &Options,
) -> Result<(Option<camera::Camera>, Option<camera::Mode>, CameraStatus), String> {
    let camera_only = resolved.mode == Mode::WebcamOnly;
    let explicit = explicit_camera(resolved);
    let missing = |message: String, status: &str| -> Result<_, String> {
        if camera_only {
            return Err(message);
        }
        Ok((
            None,
            None,
            CameraStatus {
                device: explicit.clone().unwrap_or_default(),
                status: status.into(),
                message: Some(format!("{message}; recording without the camera")),
                ..CameraStatus::default()
            },
        ))
    };
    let found = match camera::resolve(explicit.as_deref()) {
        Ok(found) => found,
        Err(message) => return missing(message, "missing"),
    };
    if camera::busy(&found.device) {
        return missing(busy_message(&found.device), "busy");
    }
    let want = camera::wanted_size(camera_only, resolved.preset.camera_max_width());
    let mode = camera::choose_mode(&found.modes, want);
    let status = CameraStatus {
        device: found.device.clone(),
        name: (!found.name.is_empty()).then(|| found.name.clone()),
        mode: mode.as_ref().map(camera::Mode::label),
        status: "checking".into(),
        message: None,
    };
    Ok((Some(found), mode, status))
}

fn capture_input(resolved: &Options, display: &str) -> Result<CaptureInputs, String> {
    let (found, camera_mode, camera_status) =
        if resolved.mode == Mode::WebcamOnly || resolved.webcam != Webcam::Off {
            let (found, mode, status) = prepare_camera(resolved)?;
            (found, mode, Some(status))
        } else {
            (None, None, None)
        };
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
            if let (Webcam::Corner, Some(found)) = (resolved.webcam, &found) {
                // The camera is opened first on purpose. With x11grab first,
                // FFmpeg's two live inputs drift apart and the overlay path
                // duplicates or drops most of the frame stream even though
                // either source is fine on its own.
                camera_index = Some(video_inputs);
                video_inputs += 1;
                argv.extend(camera_input(&found.device, camera_mode.as_ref()));
            }
            screen_index = Some(video_inputs);
            video_inputs += 1;
            argv.extend(screen_input(display, geometry));
        }
        Mode::WebcamOnly => {
            let found = found
                .as_ref()
                .ok_or_else(|| "camera input is missing".to_string())?;
            camera_index = Some(video_inputs);
            video_inputs += 1;
            argv.extend(camera_input(&found.device, camera_mode.as_ref()));
        }
    }
    Ok(CaptureInputs {
        argv,
        geometry,
        screen_index,
        camera_index,
        audio_index: video_inputs,
        camera: found,
        camera_mode,
        camera_status,
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

fn border_rgb(value: &str) -> Option<(u8, u8, u8)> {
    match value {
        "none" => None,
        "theme" => Some(camera::theme_accent(&config_home()).unwrap_or((232, 232, 232))),
        other => camera::parse_hex(other),
    }
}

fn build_plan(
    resolved: &Options,
    capture: &CaptureInputs,
    partial: &Path,
    progress: &Path,
    encoder: &Encoder,
    probe: Option<&Path>,
) -> Result<Plan, String> {
    // -copyts keeps every input on its own capture clock (x11grab and pulse
    // use wall-clock time, the camera does with -ts abs), so the camera, the
    // screen and the microphone line up by when they were captured instead
    // of each input being shifted to start at zero on its own; the latter put
    // the camera overlay about 0.3 s out of sync with the voice. The output
    // is shifted to start at zero as a whole (-avoid_negative_ts make_zero).
    let mut argv: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-n",
        "-copyts",
    ]
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
    let mirror = if resolved.camera_mirror { "hflip," } else { "" };
    // The camera stream, split once when the live picture check is on.
    let camera_source = |filters: &mut Vec<String>, camera_index: usize| -> String {
        if probe.is_some() {
            filters.push(format!(
                "[{camera_index}:v]split=2[camsrc][probesrc];[probesrc]{}[probe]",
                camera::probe_chain(None)
            ));
            "camsrc".to_string()
        } else {
            format!("{camera_index}:v")
        }
    };
    if resolved.mode == Mode::WebcamOnly {
        let camera_index = capture
            .camera_index
            .ok_or_else(|| "camera input is missing".to_string())?;
        let max_width = resolved.preset.camera_max_width();
        let source = camera_source(&mut filters, camera_index);
        filters.push(format!(
            "[{source}]{mirror}scale=w='trunc(min(iw,{max_width})/2)*2':h=-2:flags=bicubic,setsar=1[{vout}]"
        ));
        match &capture.camera_mode {
            Some(mode) => {
                width = even(mode.width.min(max_width));
                height = even(
                    ((mode.height as f64) * (width as f64) / (mode.width as f64)).round() as u32,
                );
            }
            None => {
                width = max_width;
                height = 0;
            }
        }
    } else {
        let screen_index = capture
            .screen_index
            .ok_or_else(|| "screen input is missing".to_string())?;
        let (_, out_width, out_height) =
            screen.ok_or_else(|| "screen geometry is missing".to_string())?;
        width = out_width;
        height = out_height;
        if let (Webcam::Corner, Some(camera_index)) = (resolved.webcam, capture.camera_index) {
            let camera_size = capture
                .camera_mode
                .as_ref()
                .map(|mode| (mode.width, mode.height))
                .unwrap_or((0, 0));
            let border = border_rgb(&resolved.camera_border);
            let overlay = camera::overlay_box(
                (out_width, out_height),
                camera_size,
                resolved.webcam_size.height_share(),
                resolved.camera_shape,
                resolved.corner.placement(),
                border.is_some(),
            );
            let (ow, oh) = (overlay.width, overlay.height);
            filters.push(format!(
                "[{screen_index}:v]scale={out_width}:{out_height}:flags=bicubic,setsar=1[base]"
            ));
            let source = camera_source(&mut filters, camera_index);
            // Fill the box and crop the rest: correct for any camera aspect,
            // and a centre crop for the circle.
            filters.push(format!(
                "[{source}]{mirror}scale={ow}:{oh}:force_original_aspect_ratio=increase:flags=bicubic,crop={ow}:{oh},setsar=1[wc]"
            ));
            filters.extend(camera::shape_filters(
                "wc",
                "wco",
                &overlay,
                border.unwrap_or((0, 0, 0)),
            ));
            filters.push(format!(
                "[base][wco]overlay={}:{}:format=auto[{vout}]",
                overlay.x, overlay.y
            ));
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
    argv.extend(["-avoid_negative_ts".into(), "make_zero".into()]);
    argv.push(partial.display().to_string());
    if let (Some(path), Some(_)) = (probe, capture.camera_index) {
        // A tiny greyscale copy of every camera frame for the live picture
        // check, replaced atomically in one tmpfs file (about 12 KB).
        argv.extend([
            "-map".into(),
            "[probe]".into(),
            "-f".into(),
            "image2".into(),
            "-update".into(),
            "1".into(),
            "-atomic_writing".into(),
            "1".into(),
            "-c:v".into(),
            "pgm".into(),
            path.display().to_string(),
        ]);
    }
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
    apply_camera_settings(
        &mut options,
        config.camera.as_deref(),
        config.camera_shape.as_deref(),
        config.camera_mirror,
        config.camera_border.as_deref(),
    )?;
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
            "--camera" => {
                let value = next()?;
                options.camera = (value != "auto").then_some(value);
                index += 1;
            }
            "--camera-shape" => {
                options.camera_shape = camera::Shape::parse(&next()?)?;
                index += 1;
            }
            "--mirror" => options.camera_mirror = true,
            "--no-mirror" => options.camera_mirror = false,
            "--camera-border" => {
                options.camera_border = valid_border(&next()?)?;
                index += 1;
            }
            "--check-camera" => options.check_camera = true,
            "--preview" => options.preview = true,
            "--export" => {
                let value = next()?;
                if value != "mp4" && value != "gif" {
                    return Err("--export needs mp4 or gif".into());
                }
                options.export = Some(value);
                index += 1;
            }
            "--share" => options.share = true,
            "--input" => {
                options.input = Some(PathBuf::from(next()?));
                index += 1;
            }
            "--from" | "--to" => {
                let value: f64 = next()?
                    .parse()
                    .ok()
                    .filter(|value: &f64| value.is_finite() && *value >= 0.0)
                    .ok_or_else(|| format!("{arg} needs seconds"))?;
                if arg == "--from" {
                    options.trim_from = Some(value);
                } else {
                    options.trim_to = Some(value);
                }
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
    if let (Some(from), Some(to)) = (options.trim_from, options.trim_to) {
        if to <= from {
            return Err("--to must be after --from".into());
        }
    }
    Ok(options)
}

fn help() {
    println!(
        "NIRUCLIP {VERSION}\n\
Usage: niruclip --full|--region|--window|--webcam-only [options]\n\
       niruclip --stop | --pause | --resume | --status [--json] | --recover\n\
       niruclip --list-profiles | --save-profile NAME\n\
       niruclip --check-camera | --preview\n\
       niruclip --export mp4|gif [--input FILE] [--from S] [--to S] | --share [--input FILE]\n\n\
Options:\n\
  --audio none|mic|desktop|both\n\
  --webcam off|corner|only          camera placement\n\
  --corner top-left|top-right|bottom-left|bottom-right\n\
  --webcam-size small|medium|large  camera height: 20, 26 or 34 % of the video\n\
  --camera DEVICE|auto              camera device (auto skips IR sensors)\n\
  --camera-shape rounded|circle|rect\n\
  --camera-border theme|none|RRGGBB thin border around the camera\n\
  --mirror | --no-mirror            mirror the camera picture\n\
  --preset share|small|quality\n\
  --encoder auto|software|vaapi     auto uses VAAPI when a probe succeeds\n\
  --countdown 0..10\n\
  --output PATH                     existing file or directory\n\
  --screen OUTPUT                   connected RandR output for full/overlay\n\
  --profile NAME                    apply a saved or built-in profile\n\
  --no-notify                       do not use desktop notifications"
    );
}

/// `--status --json`: the session as stored plus fields derived when asked.
#[derive(Serialize)]
struct StatusView<'a> {
    #[serde(flatten)]
    session: &'a Session,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    drop_warning: Option<bool>,
}

/// Recorded time: finished segments plus the current segment's progress.
fn session_elapsed(session: &Session) -> Option<f64> {
    if !session.active() {
        return None;
    }
    let current = match session.state.as_str() {
        "recording" | "pausing" | "finalizing" => session
            .performance
            .as_ref()
            .and_then(|performance| performance.out_time_us)
            .map(|us| us as f64 / 1e6)
            .unwrap_or(0.0),
        _ => 0.0,
    };
    Some(session.elapsed_before.unwrap_or(0.0) + current)
}

fn session_size(session: &Session) -> Option<u64> {
    let files = if session.state == "finished" {
        vec![PathBuf::from(&session.output)]
    } else {
        session_media(session)
    };
    let sizes: Vec<u64> = files
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|meta| meta.len()))
        .collect();
    (!sizes.is_empty()).then(|| sizes.iter().sum())
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

fn status(options: &Options) {
    let session = read_state().unwrap_or_else(Session::empty);
    let elapsed = session_elapsed(&session);
    let size = session_size(&session);
    let drop_warning = session
        .performance
        .as_ref()
        .map(Performance::needs_attention);
    if options.json {
        let view = StatusView {
            session: &session,
            elapsed_seconds: elapsed.map(|value| (value * 10.0).round() / 10.0),
            size_bytes: size,
            drop_warning,
        };
        match serde_json::to_string_pretty(&view) {
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
    if session.state != "idle" {
        let mut mode = session.mode.clone();
        if session.webcam == "corner" {
            mode.push_str(&format!(", camera {}", session.corner));
        }
        mode.push_str(&format!(
            ", audio {}, preset {}",
            session.audio, session.preset
        ));
        println!("mode: {mode}");
    }
    if let Some(elapsed) = elapsed {
        println!("elapsed: {}", clock(elapsed));
    }
    if let Some(size) = size {
        println!("size: {}", human_bytes(size));
    }
    if let Some(encoder) = &session.encoder {
        println!("encoder: {encoder}");
    }
    if let Some(fallback) = &session.fallback {
        println!("fallback: {fallback}");
    }
    if let Some(fps) = session.performance.as_ref().and_then(|p| p.fps) {
        println!("fps: {fps:.1}");
    }
    if let Some(performance) = &session.performance {
        let summary = performance.summary();
        if !summary.is_empty() {
            println!("performance: {summary}");
        }
    }
    if drop_warning == Some(true) {
        println!("warning: capture cannot keep up; try preset small or disable the camera overlay");
    }
    if let Some(camera) = &session.camera {
        let mut line = camera.device.clone();
        if let Some(name) = &camera.name {
            line.push_str(&format!(" ({name})"));
        }
        if let Some(mode) = &camera.mode {
            line.push_str(&format!(", {mode}"));
        }
        println!("camera: {line}: {}", camera.status);
        if let Some(message) = camera.message.as_ref().filter(|_| camera.problem()) {
            println!("warning: {message}");
        }
    }
    if !session.output.is_empty() {
        println!("output: {}", session.output);
    }
    if !session.partial.is_empty() && session.state != "finished" {
        println!("partial: {}", session.partial);
    }
    if let Some(segments) = &session.segments {
        println!("segments: {}", segments.len());
    }
    if let Some(reason) = &session.reason {
        println!("reason: {reason}");
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
    let probe = capture.camera_index.and_then(|_| probe_path());
    let plan = build_plan(
        &options,
        &capture,
        &partial,
        &progress_path,
        &selection.encoder,
        probe.as_deref(),
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
        camera: capture.camera_status.clone(),
    };
    write_state(&session)?;
    if let Some(status) = session.camera.as_ref().filter(|status| status.problem()) {
        let message = status.message.clone().unwrap_or_default();
        eprintln!("niruclip: {message}");
        notify(&options, "NIRUCLIP camera", &capitalize(&message));
    }
    if capture.camera.is_some() {
        // Ask for a steady 30 fps: dynamic exposure framerate halves the
        // camera rate in low light. The previous value is persisted first.
        let device = capture
            .camera
            .as_ref()
            .map(|found| found.device.clone())
            .unwrap_or_default();
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
    // A countdown of 2 s or more doubles as a camera check: a short probe
    // reads the camera while the user waits, so a privacy image, a frozen or
    // a silent camera is reported before anything is recorded.
    let ffmpeg_for_preroll = command_path("ffmpeg", "NIRUCLIP_FFMPEG");
    let mut preroll = match (&capture.camera, options.countdown >= 2) {
        (Some(found), true) => PreRoll::start(
            &ffmpeg_for_preroll,
            &found.device,
            capture.camera_mode.as_ref(),
        ),
        _ => None,
    };
    if options.countdown > 0 {
        let mut remaining = options.countdown;
        while remaining > 0 {
            if SIGNAL.load(Ordering::SeqCst) != 0 {
                if let Some(preroll) = preroll.take() {
                    preroll.finish();
                }
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
    if let Some(preroll) = preroll.take() {
        let verdict = preroll.finish();
        let device = session
            .camera
            .as_ref()
            .map(|status| status.device.clone())
            .unwrap_or_default();
        if let Some((state, message)) = verdict.status(&device) {
            update_camera_status(&mut session, &options, state, message);
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
        probe: probe.clone(),
    };
    let mut monitor = match (&capture_device(&recorder), &recorder.probe) {
        (Some(device), Some(path)) => Some(CameraMonitor::new(device, path)),
        _ => None,
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
            if let Some(monitor) = monitor.as_mut() {
                if recorder.probe.is_none() {
                    monitor.probe = None;
                }
                if let Some((state, message)) = monitor.poll() {
                    update_camera_status(&mut session, &options, state, message);
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
                if let Some(monitor) = monitor.as_mut() {
                    monitor.reset();
                }
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
    /// Live picture-check output; dropped if ffmpeg cannot write it.
    probe: Option<PathBuf>,
}

fn capture_device(recorder: &Recorder) -> Option<String> {
    recorder
        .capture
        .camera
        .as_ref()
        .filter(|_| recorder.capture.camera_index.is_some())
        .map(|found| found.device.clone())
}

/// Store a camera status change and tell the user about problems (and about
/// the picture coming back after one).
fn update_camera_status(
    session: &mut Session,
    options: &Options,
    state: &str,
    message: Option<String>,
) {
    let Some(status) = session.camera.as_mut() else {
        return;
    };
    if status.status == state && status.message == message {
        return;
    }
    let was_problem = status.problem();
    status.status = state.into();
    status.message = message.clone();
    let problem = status.problem();
    let _ = write_state(session);
    if let (true, Some(message)) = (problem, message) {
        eprintln!("niruclip: {message}");
        notify(options, "NIRUCLIP camera", &capitalize(&message));
    } else if was_problem && !problem {
        notify(options, "NIRUCLIP camera", "Camera picture is back");
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Where ffmpeg writes the live picture-check frame. It is rewritten for
/// every camera frame, so only a tmpfs is used; without one the live check
/// is off (the countdown check and the privacy control still work).
fn probe_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("NIRUCLIP_PROBE_DIR") {
        return Some(PathBuf::from(dir).join(format!("niruclip-{}.pgm", std::process::id())));
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/dev/shm")))
        .find(|dir| dir.is_dir() && on_tmpfs(dir))
        .map(|dir| dir.join(format!("niruclip-{}.pgm", std::process::id())))
}

fn on_tmpfs(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and a zeroed statfs buffer.
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return false;
    }
    // TMPFS_MAGIC / RAMFS_MAGIC
    matches!(stat.f_type as i64, 0x0102_1994 | 0x8584_58f6)
}

/// The countdown camera check: a separate short ffmpeg that reads tiny
/// greyscale frames from the camera into memory. Nothing is stored.
struct PreRoll {
    child: std::process::Child,
    reader: std::thread::JoinHandle<Vec<(Instant, Vec<u8>)>>,
    errors: std::thread::JoinHandle<String>,
}

struct PreRollVerdict {
    picture: camera::Picture,
    frames: usize,
    errors: String,
}

impl PreRollVerdict {
    /// (status, message) for the session, None when nothing can be said.
    fn status(&self, device: &str) -> Option<(&'static str, Option<String>)> {
        if self.frames == 0 {
            if self.errors.contains("busy") {
                return Some(("busy", Some(busy_message(device))));
            }
            return Some((
                "no-frames",
                Some(format!(
                    "camera {device} delivered no frames during the countdown"
                )),
            ));
        }
        picture_status(self.picture)
    }
}

fn picture_status(picture: camera::Picture) -> Option<(&'static str, Option<String>)> {
    match picture {
        camera::Picture::Unknown => None,
        camera::Picture::Live => Some(("ok", None)),
        camera::Picture::Placeholder => Some((
            "privacy",
            Some(
                "the camera shows its privacy image (crossed-out camera): check the camera key, \
                 the shutter or the BIOS camera setting"
                    .into(),
            ),
        )),
        camera::Picture::Frozen => Some((
            "frozen",
            Some(
                "the camera picture is frozen (every frame is identical): check the camera key \
                 or shutter, a covered lens, or unplug and replug the camera"
                    .into(),
            ),
        )),
    }
}

impl PreRoll {
    fn start(ffmpeg: &str, device: &str, mode: Option<&camera::Mode>) -> Option<Self> {
        use std::io::Read;
        let mut argv: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error"]
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        argv.extend(camera_input(device, mode));
        argv.extend([
            "-vf".into(),
            camera::probe_chain(Some(5)),
            "-f".into(),
            "rawvideo".into(),
            "-".into(),
        ]);
        let mut child = Command::new(ffmpeg)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let mut stderr = child.stderr.take()?;
        let reader = std::thread::spawn(move || {
            let mut frames = Vec::new();
            let mut frame = vec![0u8; camera::PROBE_BYTES];
            while stdout.read_exact(&mut frame).is_ok() {
                frames.push((Instant::now(), frame.clone()));
                if frames.len() > 64 {
                    frames.remove(0);
                }
            }
            frames
        });
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text.chars().take(2000).collect()
        });
        Some(Self {
            child,
            reader,
            errors,
        })
    }

    /// Stop the probe (the camera is released before the recording opens
    /// it) and judge the frames that came after the first second, when
    /// exposure has settled and a firmware privacy image has replaced the
    /// first real frames.
    fn finish(mut self) -> PreRollVerdict {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let frames = self.reader.join().unwrap_or_default();
        let errors = self.errors.join().unwrap_or_default();
        let count = frames.len();
        let first = frames.first().map(|(at, _)| *at);
        let settled: Vec<&[u8]> = frames
            .iter()
            .filter(|(at, _)| {
                first
                    .map(|first| at.duration_since(first) >= Duration::from_millis(900))
                    .unwrap_or(false)
            })
            .map(|(_, frame)| frame.as_slice())
            .collect();
        PreRollVerdict {
            picture: camera::assess(&settled),
            frames: count,
            errors: errors.to_ascii_lowercase(),
        }
    }
}

/// Watches the camera during a recording: the probe frame ffmpeg rewrites
/// twice a second, and the UVC privacy control when the camera has one.
struct CameraMonitor {
    device: String,
    probe: Option<PathBuf>,
    frames: std::collections::VecDeque<Vec<u8>>,
    last_mtime: Option<SystemTime>,
    last_frame: Instant,
    next_check: Instant,
    reported: Option<&'static str>,
}

impl CameraMonitor {
    fn new(device: &str, probe: &Path) -> Self {
        Self {
            device: device.to_string(),
            probe: Some(probe.to_path_buf()),
            frames: std::collections::VecDeque::new(),
            last_mtime: None,
            last_frame: Instant::now(),
            next_check: Instant::now(),
            reported: None,
        }
    }

    /// A new segment (resume) opens the camera again.
    fn reset(&mut self) {
        self.frames.clear();
        self.last_mtime = None;
        self.last_frame = Instant::now();
    }

    /// The new (status, message) when it changed.
    fn poll(&mut self) -> Option<(&'static str, Option<String>)> {
        let now = Instant::now();
        if now < self.next_check {
            return None;
        }
        self.next_check = now + Duration::from_millis(500);
        let mut verdict: Option<(&'static str, Option<String>)> = None;
        if let Some(path) = &self.probe {
            let mtime = fs::metadata(path).and_then(|meta| meta.modified()).ok();
            if mtime.is_some() && mtime != self.last_mtime {
                if let Some(frame) = camera::read_pgm(path) {
                    self.last_mtime = mtime;
                    self.last_frame = now;
                    self.frames.push_back(frame);
                    while self.frames.len() > 6 {
                        self.frames.pop_front();
                    }
                }
            }
            if now.duration_since(self.last_frame) > Duration::from_secs(6) {
                verdict = Some((
                    "no-frames",
                    Some(format!(
                        "camera {} stopped delivering frames (unplugged or taken by another program?)",
                        self.device
                    )),
                ));
            } else {
                let frames: Vec<&[u8]> = self.frames.iter().map(Vec::as_slice).collect();
                verdict = picture_status(camera::assess(&frames));
            }
        }
        if camera::privacy(&self.device) == Some(true) {
            verdict = Some((
                "privacy",
                Some("the camera privacy switch is on (camera key or shutter)".into()),
            ));
        }
        let (state, message) = verdict?;
        if self.reported == Some(state) {
            return None;
        }
        self.reported = Some(state);
        Some((state, message))
    }
}

impl Drop for CameraMonitor {
    fn drop(&mut self) {
        if let Some(path) = &self.probe {
            let _ = fs::remove_file(path);
        }
    }
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
            if let Some(probe) = &self.probe {
                let _ = fs::remove_file(probe);
            }
            let plan = build_plan(
                self.options,
                &self.capture,
                path,
                &self.progress,
                &self.encoder,
                self.probe.as_deref(),
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
                    // The live picture check must never cost a recording:
                    // if ffmpeg could not write it, record without it.
                    if let (Some(probe), false) = (&self.probe, valid) {
                        let name = probe
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if log_tail().map(|log| log.contains(&name)).unwrap_or(false) {
                            eprintln!(
                                "niruclip: camera picture check unavailable; recording without it"
                            );
                            let _ = fs::remove_file(path);
                            self.probe = None;
                            continue;
                        }
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
        .any(|path| validate_media(path) || has_video_packets(path));
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

/// True when ffmpeg wrote at least one video packet, even if the file was
/// never finalized (no duration or cues after a SIGKILL or power loss).
fn has_video_packets(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    Command::new(command_path("ffprobe", "NIRUCLIP_FFPROBE"))
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-read_intervals",
            "%+#1",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| {
            output.status.success() && output.stdout.iter().any(|byte| !byte.is_ascii_whitespace())
        })
        .unwrap_or(false)
}

/// A partial that validates is used as is. One that ffmpeg never finalized
/// is remuxed losslessly (stream copy) into a sibling file; only when that
/// copy validates does it replace the damaged original, under the same name.
fn usable_or_repaired(path: &Path) -> bool {
    if validate_media(path) {
        return true;
    }
    if !has_video_packets(path) {
        return false;
    }
    let repaired = path.with_file_name(format!(
        "{}.repair.mkv",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("NIRUCLIP")
    ));
    if repaired.exists() {
        return false;
    }
    let status = Command::new(command_path("ffmpeg", "NIRUCLIP_FFMPEG"))
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(path)
        .args(["-map", "0", "-c", "copy"])
        .arg(&repaired)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if status.map(|status| status.success()).unwrap_or(false)
        && validate_media(&repaired)
        && fs::rename(&repaired, path).is_ok()
    {
        eprintln!("niruclip: repaired unfinished file {}", path.display());
        return true;
    }
    let _ = fs::remove_file(&repaired);
    false
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
            .filter(|path| usable_or_repaired(path))
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
    if !usable_or_repaired(&candidate) {
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

/// A recording that uses the camera right now, if any.
fn camera_in_recording() -> Option<CameraStatus> {
    read_state()
        .filter(|session| session.active() && pid_alive(session.pid, session.pid_start))
        .and_then(|session| session.camera)
        .filter(|camera| camera.status != "busy" && camera.status != "missing")
}

fn mode_summary(found: &camera::Camera) -> String {
    let mut best: Vec<(String, u32, u32, f64)> = Vec::new();
    for mode in &found.modes {
        match best.iter_mut().find(|entry| entry.0 == mode.format) {
            Some(entry) => {
                if (mode.width as u64 * mode.height as u64) > (entry.1 as u64 * entry.2 as u64) {
                    entry.1 = mode.width;
                    entry.2 = mode.height;
                    entry.3 = mode.fps;
                }
            }
            None => best.push((mode.format.clone(), mode.width, mode.height, mode.fps)),
        }
    }
    best.iter()
        .map(|(format, w, h, fps)| format!("{format} up to {w}x{h}@{}", fps.round()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `--check-camera`: which camera, which mode, privacy control, busy, and a
/// 2.5 s look at the picture. Prints a report, notifies a one-line summary
/// and exits 1 when the camera is not usable.
fn check_camera(options: &Options) -> Result<(), String> {
    if let Some(camera) = camera_in_recording() {
        println!(
            "camera {} is being recorded: {}",
            camera.device, camera.status
        );
        if let Some(message) = camera.message.filter(|_| camera.status != "ok") {
            println!("warning: {message}");
        }
        return Ok(());
    }
    let explicit = explicit_camera(options);
    if explicit.is_none() {
        for other in camera::discover() {
            let kind = if other.infrared() {
                ", infrared: skipped"
            } else {
                ""
            };
            println!("found: {} ({}{kind})", other.device, other.name);
        }
    }
    let result = (|| -> Result<String, String> {
        let found = camera::resolve(explicit.as_deref())?;
        println!("camera: {} ({})", found.device, found.name);
        if !found.bus.is_empty() {
            println!("bus: {}", found.bus);
        }
        if !found.modes.is_empty() {
            println!("modes: {}", mode_summary(&found));
        }
        let overlay = camera::choose_mode(&found.modes, camera::wanted_size(false, 1920));
        let alone = camera::choose_mode(
            &found.modes,
            camera::wanted_size(true, options.preset.camera_max_width()),
        );
        println!(
            "recording uses: overlay {}, camera-only {}",
            overlay
                .as_ref()
                .map(camera::Mode::label)
                .unwrap_or_else(|| "ffmpeg default".into()),
            alone
                .as_ref()
                .map(camera::Mode::label)
                .unwrap_or_else(|| "ffmpeg default".into())
        );
        match camera::privacy(&found.device) {
            Some(true) => {
                return Err("the camera privacy switch is on (camera key or shutter)".into())
            }
            Some(false) => println!("privacy control: off"),
            None => {}
        }
        if camera::busy(&found.device) {
            return Err(busy_message(&found.device));
        }
        let ffmpeg = command_path("ffmpeg", "NIRUCLIP_FFMPEG");
        let probe = PreRoll::start(&ffmpeg, &found.device, overlay.as_ref())
            .ok_or_else(|| "could not start ffmpeg".to_string())?;
        std::thread::sleep(Duration::from_millis(2500));
        let verdict = probe.finish();
        println!("frames: {} in 2.5 s", verdict.frames);
        match verdict.status(&found.device) {
            Some(("ok", _)) => Ok(format!(
                "{} works: {}",
                if found.name.is_empty() {
                    &found.device
                } else {
                    &found.name
                },
                overlay
                    .as_ref()
                    .map(camera::Mode::label)
                    .unwrap_or_default()
            )),
            Some((_, Some(message))) => Err(message),
            _ => Err("could not judge the camera picture (too few frames)".into()),
        }
    })();
    match result {
        Ok(summary) => {
            println!("picture: ok");
            notify(options, "NIRUCLIP camera", &summary);
            Ok(())
        }
        Err(message) => {
            println!("problem: {message}");
            notify(options, "NIRUCLIP camera", &capitalize(&message));
            Err("camera check failed".into())
        }
    }
}

/// `--preview`: a live window with the camera exactly as the overlay will
/// look (shape, border, mirroring). Closes with q/Esc or after two minutes.
fn preview(options: &Options) -> Result<(), String> {
    if camera_in_recording().is_some() {
        return Err("the camera is being recorded; no preview while recording".into());
    }
    let found = camera::resolve(explicit_camera(options).as_deref())?;
    if camera::busy(&found.device) {
        let message = busy_message(&found.device);
        notify(options, "NIRUCLIP camera", &capitalize(&message));
        return Err(message);
    }
    let mode = camera::choose_mode(&found.modes, (1280, 720));
    let size = mode.as_ref().map(|m| (m.width, m.height)).unwrap_or((0, 0));
    let border = border_rgb(&options.camera_border);
    let shape = camera::overlay_box(
        (1600, 720),
        size,
        0.5,
        options.camera_shape,
        (false, false),
        border.is_some(),
    );
    let (w, h) = (shape.width, shape.height);
    let (canvas_w, canvas_h) = (w + 2 * shape.x, h + 2 * shape.y);
    let mirror = if options.camera_mirror { "hflip," } else { "" };
    let mut graph = vec![format!(
        "[in]setpts=PTS-STARTPTS,{mirror}scale={w}:{h}:force_original_aspect_ratio=increase:flags=bicubic,crop={w}:{h},setsar=1[wc]"
    )];
    graph.extend(camera::shape_filters(
        "wc",
        "wco",
        &shape,
        border.unwrap_or((0, 0, 0)),
    ));
    graph.push(format!("color=c=0x101010:s={canvas_w}x{canvas_h}:r=30[bg]"));
    graph.push(format!(
        "[bg][wco]overlay={}:{}:shortest=1:format=auto[out]",
        shape.x, shape.y
    ));
    let mut argv: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "v4l2"]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    if let Some(mode) = &mode {
        if let Some(format) = mode.ffmpeg_format() {
            argv.extend(["-input_format".into(), format.into()]);
        }
        argv.extend([
            "-video_size".into(),
            format!("{}x{}", mode.width, mode.height),
        ]);
    }
    argv.extend([
        "-framerate".into(),
        "30".into(),
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-framedrop".into(),
        "-an".into(),
        "-t".into(),
        "120".into(),
        "-autoexit".into(),
        "-window_title".into(),
        "NIRUCLIP camera preview (q to close)".into(),
        "-vf".into(),
        graph.join(";"),
        found.device.clone(),
    ]);
    // exec: the preview window *is* this process, so closing or killing
    // it never leaves an ffplay holding the camera.
    let error = Command::new(command_path("ffplay", "NIRUCLIP_FFPLAY"))
        .args(&argv)
        .stdin(Stdio::null())
        .exec();
    Err(format!("could not start ffplay: {error}"))
}

/// The recording --export and --share work on: --input, else the last saved
/// recording, else the newest NIRUCLIP recording in the output directory.
fn last_recording(options: &Options) -> Result<PathBuf, String> {
    if let Some(input) = &options.input {
        return input
            .is_file()
            .then(|| input.clone())
            .ok_or_else(|| format!("{} does not exist", input.display()));
    }
    if let Some(session) = read_state().filter(|session| session.state == "finished") {
        let path = PathBuf::from(&session.output);
        if path.is_file() {
            return Ok(path);
        }
    }
    let dir = output_path(None)?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::read_dir(&dir)
        .map_err(|error| format!("could not read {}: {error}", dir.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            name.starts_with("NIRUCLIP-") && name.ends_with(".mkv") && !name.ends_with(".part.mkv")
        })
        .max_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
        .ok_or_else(|| format!("no recording found in {}", dir.display()))
}

/// A path next to `input` with `extension` that does not exist yet.
fn export_target(input: &Path, extension: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "NIRUCLIP".into());
    let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut candidate = dir.join(format!("{stem}.{extension}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{n}.{extension}"));
        n += 1;
    }
    candidate
}

/// The longest GIF made without an explicit --to.
const GIF_MAX_SECONDS: f64 = 30.0;

/// `--export mp4|gif`: a share-friendly copy of a recording next to it. MP4
/// without trimming is a lossless remux (instant); trimming re-encodes so
/// the cut is frame-accurate. The MKV is never changed.
fn export(options: &Options, format: &str) -> Result<PathBuf, String> {
    let input = last_recording(options)?;
    let target = export_target(&input, format);
    let ffmpeg = command_path("ffmpeg", "NIRUCLIP_FFMPEG");
    let mut trim: Vec<String> = Vec::new();
    if let Some(from) = options.trim_from {
        trim.extend(["-ss".into(), format!("{from:.3}")]);
    }
    let mut to = options.trim_to;
    let mut capped = false;
    if format == "gif" && to.is_none() {
        let duration = media_duration(&input).unwrap_or(0.0);
        if duration - options.trim_from.unwrap_or(0.0) > GIF_MAX_SECONDS {
            to = Some(options.trim_from.unwrap_or(0.0) + GIF_MAX_SECONDS);
            capped = true;
        }
    }
    if let Some(to) = to {
        trim.extend(["-to".into(), format!("{to:.3}")]);
    }
    let base = |args: &[String]| -> Vec<String> {
        let mut argv: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-n"]
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        argv.extend(trim.iter().cloned());
        argv.extend(["-i".into(), input.display().to_string()]);
        argv.extend(args.iter().cloned());
        argv.push(target.display().to_string());
        argv
    };
    let strings =
        |items: &[&str]| -> Vec<String> { items.iter().map(|v| (*v).to_string()).collect() };
    let attempts: Vec<Vec<String>> = if format == "gif" {
        vec![base(&strings(&[
            "-an",
            "-vf",
            "fps=12,scale='min(720,iw)':-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4",
            "-loop",
            "0",
        ]))]
    } else {
        let encode = base(&strings(&[
            "-map",
            "0:v:0",
            "-map",
            "0:a?",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ]));
        if trim.is_empty() {
            vec![
                base(&strings(&[
                    "-map",
                    "0",
                    "-c",
                    "copy",
                    "-movflags",
                    "+faststart",
                ])),
                encode,
            ]
        } else {
            vec![encode]
        }
    };
    let mut last_error = String::new();
    for argv in attempts {
        let _ = fs::remove_file(&target);
        let output = Command::new(&ffmpeg)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("could not start ffmpeg: {error}"))?;
        if output.status.success() && fs::metadata(&target).map(|m| m.len() > 0).unwrap_or(false) {
            let note = if capped {
                " (first 30 s; use --to for more)"
            } else {
                ""
            };
            println!("NIRUCLIP exported: {}{note}", target.display());
            notify(
                options,
                "NIRUCLIP",
                &format!("Exported {}{note}", target.display()),
            );
            return Ok(target);
        }
        last_error = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(400)
            .collect();
    }
    let _ = fs::remove_file(&target);
    notify(
        options,
        "NIRUCLIP",
        "Export failed; see the terminal output",
    );
    Err(format!("export failed: {last_error}"))
}

/// `--share`: export the recording as MP4 and open NIRU Share with it.
fn share(options: &Options) -> Result<(), String> {
    let mp4 = export(options, "mp4")?;
    Command::new(command_path("nirulauncher", "NIRUCLIP_LAUNCHER"))
        .arg("--share")
        .env("NIRUSHARE_SEND_FILE", &mp4)
        .stdin(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open NIRU Share: {error}"))
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
        if options.check_camera {
            return check_camera(&options);
        }
        if options.preview {
            return preview(&options);
        }
        if options.share {
            return share(&options);
        }
        if let Some(format) = &options.export {
            return export(&options, format).map(|_| ());
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
