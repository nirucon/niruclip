use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = "0.2.0";
const SCHEMA: u32 = 1;
const ACTIVE_STATES: &[&str] = &["countdown", "starting", "recording", "finalizing"];
const TERMINAL_STATES: &[&str] = &["finished", "failed", "interrupted", "cancelled", "idle"];

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
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

#[derive(Clone, Debug)]
struct Options {
    mode: Mode,
    audio: Audio,
    webcam: Webcam,
    corner: Corner,
    webcam_size: Size,
    preset: Preset,
    countdown: u64,
    output: Option<PathBuf>,
    screen: Option<String>,
    profile: Option<String>,
    save_profile: Option<String>,
    json: bool,
    no_notify: bool,
    list_profiles: bool,
    stop: bool,
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
            countdown: 3,
            output: None,
            screen: None,
            profile: None,
            save_profile: None,
            json: false,
            no_notify: false,
            list_profiles: false,
            stop: false,
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
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

impl Config {
    fn load() -> Result<Self, String> {
        let mut config = Self {
            schema: 1,
            default_profile: None,
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
    reason: Option<String>,
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
            reason: None,
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
    }
}

fn notify(options: &Options, summary: &str, body: &str) {
    if options.no_notify
        || std::env::var_os("NIRUCLIP_NOTIFY").as_deref() == Some(std::ffi::OsStr::new("0"))
    {
        return;
    }
    let _ = Command::new("notify-send")
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

fn capture_input(
    resolved: &Options,
    display: &str,
) -> Result<(Vec<String>, Option<Geometry>), String> {
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
    let mut inputs = Vec::new();
    match resolved.mode {
        Mode::Full | Mode::Region | Mode::Window => {
            let geometry = geometry.ok_or_else(|| "screen geometry is missing".to_string())?;
            inputs.extend([
                "-f".into(),
                "x11grab".into(),
                "-framerate".into(),
                "30".into(),
                "-video_size".into(),
                format!("{}x{}", geometry.width, geometry.height),
                "-i".into(),
                format!("{display}+{},{}", geometry.x, geometry.y),
            ]);
            if resolved.webcam == Webcam::Corner {
                inputs.extend(["-f".into(), "v4l2".into(), "-i".into(), camera]);
            }
        }
        Mode::WebcamOnly => {
            inputs.extend(["-f".into(), "v4l2".into(), "-i".into(), camera]);
        }
    }
    Ok((inputs, geometry))
}

fn build_plan(resolved: &Options, display: &str, partial: &Path) -> Result<Plan, String> {
    let (mut inputs, geometry) = capture_input(resolved, display)?;
    let mut argv: Vec<String> = ["-hide_banner", "-loglevel", "warning", "-nostdin", "-n"]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    argv.append(&mut inputs);
    let screen = geometry.map(|geometry| {
        let (width, height) = scaled_screen(geometry.width, geometry.height, resolved.preset);
        (geometry, width, height)
    });
    let video_index = 0;
    let mut audio_index = if resolved.webcam == Webcam::Corner && resolved.mode != Mode::WebcamOnly
    {
        2
    } else {
        1
    };
    if resolved.audio == Audio::Mic
        || resolved.audio == Audio::Desktop
        || resolved.audio == Audio::Both
    {
        if resolved.audio == Audio::Mic || resolved.audio == Audio::Both {
            argv.extend([
                "-f".into(),
                "pulse".into(),
                "-i".into(),
                "@DEFAULT_SOURCE@".into(),
            ]);
        }
        if resolved.audio == Audio::Desktop || resolved.audio == Audio::Both {
            argv.extend([
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
        let max_width = resolved.preset.camera_max_width();
        filters.push(format!(
            "[0:v]scale=w='trunc(min(iw,{max_width})/2)*2':h=-2:flags=bicubic,setsar=1[v]"
        ));
        width = max_width;
        height = 0;
    } else {
        let (_, out_width, out_height) =
            screen.ok_or_else(|| "screen geometry is missing".to_string())?;
        width = out_width;
        height = out_height;
        if resolved.webcam == Webcam::Corner {
            let target = even(((out_width as f64) * resolved.webcam_size.factor()).round() as u32);
            let (x, y) = resolved.corner.overlay();
            filters.push(format!(
                "[0:v]scale={out_width}:{out_height}:flags=bicubic,setsar=1[base]"
            ));
            filters.push(format!(
                "[1:v]scale=w='trunc(min(iw,{target})/2)*2':h=-2:flags=bicubic,setsar=1[wc]"
            ));
            filters.push(format!("[base][wc]overlay={x}:{y}:format=auto[v]"));
        } else {
            filters.push(format!(
                "[{video_index}:v]scale={out_width}:{out_height}:flags=bicubic,setsar=1[v]"
            ));
        }
    }
    if resolved.webcam == Webcam::Corner && resolved.mode != Mode::WebcamOnly {
        audio_index = 2;
    }
    let has_audio = resolved.audio != Audio::None;
    if resolved.audio == Audio::Both {
        let second = if audio_index == 1 { 2 } else { audio_index + 1 };
        filters.push(format!(
            "[{audio_index}:a][{second}:a]amix=inputs=2:normalize=0[a]"
        ));
    }
    if resolved.mode == Mode::WebcamOnly && resolved.audio == Audio::None {
        filters.retain(|filter| !filter.starts_with("[0:a]"));
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
       niruclip --stop | --status [--json] | --recover\n\
       niruclip --list-profiles | --save-profile NAME\n\n\
Options:\n\
  --audio none|mic|desktop|both\n\
  --webcam off|corner|only          camera placement\n\
  --corner top-left|top-right|bottom-left|bottom-right\n\
  --webcam-size small|medium|large  target share of final video width\n\
  --preset share|small|quality\n\
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
    if session.recoverable {
        println!("recovery: available (niruclip --recover)");
    }
}

fn validate_media(path: &Path) -> bool {
    if !path.is_file() {
        return false;
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
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
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
    duration > 0.05 && has_video
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
    if let Some(previous) = read_state() {
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
    let plan = build_plan(&options, display.as_deref().unwrap_or_default(), &partial)?;
    let width = plan.width;
    let height = plan.height;
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
        width,
        height,
        recoverable: false,
        reason: None,
    };
    write_state(&session)?;
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
    let log_path = state_dir().join("ffmpeg.log");
    let log = File::create(&log_path)
        .map_err(|error| format!("could not create {}: {error}", log_path.display()))?;
    let mut command = Command::new(command_path("ffmpeg", "NIRUCLIP_FFMPEG"));
    command
        .args(&plan.argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            finish_session(
                &mut session,
                "failed",
                Some(format!("could not start ffmpeg: {error}")),
            );
            return Err(format!("could not start ffmpeg: {error}"));
        }
    };
    session.state = "starting".into();
    session.child_pid = child.id();
    session.child_start = proc_start(child.id()).unwrap_or(0);
    write_state(&session)?;
    let startup_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < startup_deadline {
        if SIGNAL.load(Ordering::SeqCst) != 0 {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            if validate_media(&partial) {
                session.recoverable = true;
                finish_session(
                    &mut session,
                    "interrupted",
                    Some(format!("ffmpeg exited during startup: {status}")),
                );
                return Err("ffmpeg stopped during startup; a partial file is recoverable".into());
            }
            finish_session(
                &mut session,
                "failed",
                Some(format!("ffmpeg exited during startup: {status}")),
            );
            return Err("ffmpeg exited during startup".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if SIGNAL.load(Ordering::SeqCst) == 0 {
        session.state = "recording".into();
        write_state(&session)?;
        notify(&options, "NIRUCLIP", "Recording started");
        eprintln!("NIRUCLIP recording: {}", output.display());
    }
    let mut requested_stop = false;
    let mut status: Option<ExitStatus> = None;
    loop {
        if SIGNAL.load(Ordering::SeqCst) != 0 && !requested_stop {
            requested_stop = true;
            session.state = "finalizing".into();
            let _ = write_state(&session);
            signal_group(child.id(), libc::SIGINT);
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {
                if requested_stop {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                finish_session(
                    &mut session,
                    "failed",
                    Some(format!("could not wait for ffmpeg: {error}")),
                );
                return Err(format!("could not wait for ffmpeg: {error}"));
            }
        }
    }
    if let Some(exit) = status {
        if requested_stop {
            signal_group(child.id(), libc::SIGINT);
        }
        let _ = exit;
    }
    let wait_deadline = Instant::now() + Duration::from_secs(8);
    let mut exit_status = status;
    while exit_status.is_none() && Instant::now() < wait_deadline {
        match child.try_wait() {
            Ok(Some(status)) => exit_status = Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    if exit_status.is_none() {
        signal_group(child.id(), libc::SIGTERM);
        std::thread::sleep(Duration::from_secs(1));
        if child.try_wait().ok().flatten().is_none() {
            signal_group(child.id(), libc::SIGKILL);
        }
        exit_status = child.wait().ok();
    }
    session.state = "finalizing".into();
    let _ = write_state(&session);
    let valid = validate_media(&partial);
    let reason = if valid {
        None
    } else {
        log_tail().or_else(|| Some("no usable video was produced".into()))
    };
    if valid {
        if exit_status
            .as_ref()
            .map(|status| status.success())
            .unwrap_or(false)
            || requested_stop
        {
            match fs::rename(&partial, &output) {
                Ok(()) => {
                    session.output = output.display().to_string();
                    session.recoverable = false;
                    finish_session(&mut session, "finished", None);
                    let _ = fs::remove_file(state_dir().join("ffmpeg.log"));
                    notify(&options, "NIRUCLIP", &format!("Saved {}", output.display()));
                    println!("NIRUCLIP saved: {}", output.display());
                    return Ok(());
                }
                Err(error) => {
                    session.recoverable = validate_media(&partial);
                    finish_session(
                        &mut session,
                        "failed",
                        Some(format!("could not finalize {}: {error}", partial.display())),
                    );
                    return Err(format!("could not finalize recording: {error}"));
                }
            }
        } else {
            session.recoverable = true;
            finish_session(&mut session, "interrupted", reason);
            notify(
                &options,
                "NIRUCLIP",
                "Recording interrupted; a partial file is available",
            );
            return Err("ffmpeg stopped before finalizing; partial retained".into());
        }
    }
    session.recoverable = false;
    finish_session(&mut session, "failed", reason);
    notify(
        &options,
        "NIRUCLIP",
        "Recording failed; see niruclip --status",
    );
    Err("recording failed; no usable video was produced".into())
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
    session.recoverable = validate_media(Path::new(&session.partial));
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

fn recover() -> Result<(), String> {
    let Some(mut session) = read_state() else {
        return Err("no recording state to recover".into());
    };
    if session.state == "finished" {
        println!("NIRUCLIP recording already saved: {}", session.output);
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
