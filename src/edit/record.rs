//! Record from the editor: the choices, and the recording it starts.
//!
//! The editor runs NIRUCLIP's own recorder as a child process with the
//! chosen options, so the bar, recovery and camera checks work as always.
//! It hides while recording and puts the new file on the timeline when the
//! recording stops.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const WHAT: [&str; 4] = ["Full screen", "Window", "Area", "Camera only"];
pub const SOUND: [&str; 4] = ["None", "Microphone", "Desktop", "Both"];
pub const CAMERA: [&str; 2] = ["Off", "In a corner"];
pub const CORNER: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
pub const SIZE: [&str; 3] = ["Small", "Medium", "Large"];
pub const COUNTDOWN: [u64; 5] = [0, 3, 5, 10, 15];
pub const PLACE: [&str; 2] = ["At the end", "At the playhead"];

/// The choices, kept in edit.toml between sessions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choices {
    #[serde(default)]
    pub what: usize,
    /// 0 is the primary screen, then the outputs in `screens`.
    #[serde(default)]
    pub screen: String,
    #[serde(default = "one")]
    pub sound: usize,
    /// Empty: the default microphone.
    #[serde(default)]
    pub mic: String,
    #[serde(default)]
    pub camera: usize,
    #[serde(default = "three")]
    pub corner: usize,
    #[serde(default = "one")]
    pub size: usize,
    #[serde(default = "two")]
    pub countdown: usize,
    #[serde(default)]
    pub place: usize,
}

fn one() -> usize {
    1
}
fn two() -> usize {
    2
}
fn three() -> usize {
    3
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            what: 0,
            screen: String::new(),
            sound: 1,
            mic: String::new(),
            camera: 0,
            corner: 3,
            size: 1,
            countdown: 2,
            place: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    What,
    Screen,
    Sound,
    Mic,
    Camera,
    Corner,
    Size,
    Countdown,
    Place,
}

pub struct Form {
    pub choices: Choices,
    pub row: usize,
    /// Connected outputs (name, size).
    pub screens: Vec<(String, String)>,
    /// Microphones (name, description), the default first.
    pub mics: Vec<(String, String)>,
}

impl Form {
    pub fn new(choices: Choices) -> Self {
        let mut form = Self {
            choices,
            row: 0,
            screens: screens(),
            mics: mics(),
        };
        let c = &mut form.choices;
        c.what = c.what.min(WHAT.len() - 1);
        c.sound = c.sound.min(SOUND.len() - 1);
        c.camera = c.camera.min(CAMERA.len() - 1);
        c.corner = c.corner.min(CORNER.len() - 1);
        c.size = c.size.min(SIZE.len() - 1);
        c.countdown = c.countdown.min(COUNTDOWN.len() - 1);
        c.place = c.place.min(PLACE.len() - 1);
        if !c.screen.is_empty() && !form.screens.iter().any(|(n, _)| *n == c.screen) {
            c.screen.clear();
        }
        if !c.mic.is_empty() && !form.mics.iter().any(|(n, _)| *n == c.mic) {
            c.mic.clear();
        }
        form
    }

    /// The rows that apply to the current choices.
    pub fn rows(&self) -> Vec<Row> {
        let c = &self.choices;
        let mut rows = vec![Row::What];
        if c.what == 0 && self.screens.len() > 1 {
            rows.push(Row::Screen);
        }
        rows.push(Row::Sound);
        if (c.sound == 1 || c.sound == 3) && self.mics.len() > 1 {
            rows.push(Row::Mic);
        }
        if c.what != 3 {
            rows.push(Row::Camera);
            if c.camera == 1 {
                rows.push(Row::Corner);
                rows.push(Row::Size);
            }
        }
        rows.push(Row::Countdown);
        rows.push(Row::Place);
        rows
    }

    pub fn label(row: Row) -> &'static str {
        match row {
            Row::What => "Record",
            Row::Screen => "Screen",
            Row::Sound => "Sound",
            Row::Mic => "Microphone",
            Row::Camera => "Camera",
            Row::Corner => "Corner",
            Row::Size => "Camera size",
            Row::Countdown => "Countdown",
            Row::Place => "Put it",
        }
    }

    /// The options of a row and which one is chosen.
    pub fn options(&self, row: Row) -> (Vec<String>, usize) {
        let c = &self.choices;
        let list = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match row {
            Row::What => (list(&WHAT), c.what),
            Row::Screen => {
                let mut v = vec!["Main screen".to_string()];
                v.extend(self.screens.iter().map(|(n, size)| format!("{n} {size}")));
                let i = self
                    .screens
                    .iter()
                    .position(|(n, _)| *n == c.screen)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                (v, i)
            }
            Row::Sound => (list(&SOUND), c.sound),
            Row::Mic => {
                let mut v = vec!["Default".to_string()];
                v.extend(self.mics.iter().map(|(_, d)| d.clone()));
                let i = self
                    .mics
                    .iter()
                    .position(|(n, _)| *n == c.mic)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                (v, i)
            }
            Row::Camera => (list(&CAMERA), c.camera),
            Row::Corner => (list(&CORNER), c.corner),
            Row::Size => (list(&SIZE), c.size),
            Row::Countdown => (
                COUNTDOWN
                    .iter()
                    .map(|n| {
                        if *n == 0 {
                            "None".to_string()
                        } else {
                            format!("{n} s")
                        }
                    })
                    .collect(),
                c.countdown,
            ),
            Row::Place => (list(&PLACE), c.place),
        }
    }

    pub fn set(&mut self, row: Row, i: usize) {
        let (opts, _) = self.options(row);
        if opts.is_empty() {
            return;
        }
        let i = i.min(opts.len() - 1);
        let c = &mut self.choices;
        match row {
            Row::What => c.what = i,
            Row::Screen => {
                c.screen = if i == 0 {
                    String::new()
                } else {
                    self.screens[i - 1].0.clone()
                }
            }
            Row::Sound => c.sound = i,
            Row::Mic => {
                c.mic = if i == 0 {
                    String::new()
                } else {
                    self.mics[i - 1].0.clone()
                }
            }
            Row::Camera => c.camera = i,
            Row::Corner => c.corner = i,
            Row::Size => c.size = i,
            Row::Countdown => c.countdown = i,
            Row::Place => c.place = i,
        }
    }

    pub fn step(&mut self, row: Row, delta: isize) {
        let (opts, i) = self.options(row);
        let n = opts.len() as isize;
        if n == 0 {
            return;
        }
        self.set(row, (i as isize + delta).rem_euclid(n) as usize);
    }
}

/// Seconds the editor waits itself before starting the recorder: a window
/// or an area is picked with a click as the recorder starts, so the wait
/// comes first, to switch to the right workspace.
pub fn pre_delay(c: &Choices) -> u64 {
    if c.what == 1 || c.what == 2 {
        COUNTDOWN[c.countdown]
    } else {
        0
    }
}

/// The recorder's command line for the choices.
pub fn args(c: &Choices) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    a.push(
        match c.what {
            1 => "--window",
            2 => "--region",
            3 => "--webcam-only",
            _ => "--full",
        }
        .into(),
    );
    a.push("--audio".into());
    a.push(["none", "mic", "desktop", "both"][c.sound.min(3)].into());
    if (c.sound == 1 || c.sound == 3) && !c.mic.is_empty() {
        a.push("--mic".into());
        a.push(c.mic.clone());
    }
    if c.what == 0 && !c.screen.is_empty() {
        a.push("--screen".into());
        a.push(c.screen.clone());
    }
    if c.what != 3 {
        a.push("--webcam".into());
        if c.camera == 1 {
            a.push("corner".into());
            a.push("--corner".into());
            a.push(
                ["top-left", "top-right", "bottom-left", "bottom-right"][c.corner.min(3)].into(),
            );
            a.push("--webcam-size".into());
            a.push(["small", "medium", "large"][c.size.min(2)].into());
        } else {
            a.push("off".into());
        }
    }
    a.push("--countdown".into());
    // The wait for a window or area came before the click.
    let n = if pre_delay(c) > 0 {
        0
    } else {
        COUNTDOWN[c.countdown]
    };
    a.push(n.to_string());
    a
}

fn screens() -> Vec<(String, String)> {
    let Ok(out) = Command::new("xrandr").arg("--current").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(" connected"))
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?.to_string();
            // Only outputs that show a picture: "3440x1440+0+0".
            let geo = words.find(|w| w.contains('x') && w.contains('+'))?;
            let size = geo.split('+').next()?.to_string();
            Some((name, size))
        })
        .collect()
}

fn mics() -> Vec<(String, String)> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Ok(out) = Command::new(exe)
        .args(["--list-mics", "--json"])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some((
                        m["name"].as_str()?.to_string(),
                        m["description"].as_str().unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A recording in progress, from its wait to the finished file.
pub struct Recording {
    pub child: Option<Child>,
    pub start_at: Instant,
    pub args: Vec<String>,
    pub place_at: Option<f64>,
    err: PathBuf,
}

pub enum Done {
    File(PathBuf),
    Cancelled,
    Failed(String),
}

fn session_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".local/state"))
        .join("niruclip/session.json")
}

impl Recording {
    pub fn new(c: &Choices, place_at: Option<f64>, work: &std::path::Path) -> Self {
        Self {
            child: None,
            start_at: Instant::now() + Duration::from_secs(pre_delay(c)),
            args: args(c),
            place_at,
            err: work.join("record.err"),
        }
    }

    /// Start the recorder when its time has come; poll it after that.
    pub fn poll(&mut self) -> Option<Done> {
        if self.child.is_none() {
            if Instant::now() < self.start_at {
                return None;
            }
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => return Some(Done::Failed(e.to_string())),
            };
            if std::env::var_os("NIRUCLIP_EDIT_DEBUG").is_some() {
                eprintln!("record: {}", self.args.join(" "));
            }
            let err = std::fs::File::create(&self.err).ok();
            let spawned = Command::new(exe)
                .args(&self.args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(err.map(Stdio::from).unwrap_or_else(Stdio::null))
                .spawn();
            match spawned {
                Ok(c) => self.child = Some(c),
                Err(e) => return Some(Done::Failed(format!("could not start the recorder: {e}"))),
            }
            return None;
        }
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => {
                let text = std::fs::read_to_string(&self.err).unwrap_or_default();
                let _ = std::fs::remove_file(&self.err);
                let session: Option<serde_json::Value> = std::fs::read_to_string(session_path())
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok());
                let output = session
                    .as_ref()
                    .filter(|s| s["state"].as_str() == Some("finished"))
                    .and_then(|s| s["output"].as_str())
                    .map(PathBuf::from)
                    .filter(|p| p.exists());
                if let Some(p) = output.filter(|_| status.success()) {
                    return Some(Done::File(p));
                }
                let reason = session
                    .as_ref()
                    .and_then(|s| s["reason"].as_str().map(str::to_string))
                    .or_else(|| {
                        text.lines()
                            .rev()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| l.trim_start_matches("niruclip: ").to_string())
                    })
                    .unwrap_or_else(|| format!("the recorder ended with {status}"));
                if reason.contains("cancelled") {
                    Some(Done::Cancelled)
                } else {
                    Some(Done::Failed(reason))
                }
            }
            Err(e) => Some(Done::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_screen_with_mic_and_camera() {
        let c = Choices {
            what: 0,
            screen: "DP-2-1".into(),
            sound: 3,
            mic: "alsa_input.usb-RODE".into(),
            camera: 1,
            corner: 1,
            size: 2,
            countdown: 2,
            place: 0,
        };
        assert_eq!(
            args(&c).join(" "),
            "--full --audio both --mic alsa_input.usb-RODE --screen DP-2-1 --webcam corner \
             --corner top-right --webcam-size large --countdown 5"
        );
        assert_eq!(pre_delay(&c), 0);
    }

    #[test]
    fn a_window_waits_in_the_editor_before_the_click() {
        let c = Choices {
            what: 1,
            sound: 2,
            countdown: 3,
            ..Choices::default()
        };
        assert_eq!(
            args(&c).join(" "),
            "--window --audio desktop --webcam off --countdown 0"
        );
        assert_eq!(pre_delay(&c), 10);
    }

    #[test]
    fn camera_only_has_no_camera_or_screen_rows() {
        let mut f = Form {
            choices: Choices {
                what: 3,
                ..Choices::default()
            },
            row: 0,
            screens: vec![("A".into(), "1x1".into()), ("B".into(), "1x1".into())],
            mics: vec![],
        };
        assert!(!f.rows().contains(&Row::Camera));
        assert!(!f.rows().contains(&Row::Screen));
        assert_eq!(args(&f.choices)[0], "--webcam-only");
        f.set(Row::What, 0);
        assert!(f.rows().contains(&Row::Screen));
        f.set(Row::Screen, 2);
        assert_eq!(f.choices.screen, "B");
        f.step(Row::Screen, 1);
        assert_eq!(f.choices.screen, "");
    }
}
