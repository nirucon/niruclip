//! Microphone selection: which PipeWire/Pulse source the recording reads.
//!
//! The sources are listed with `ffmpeg -sources pulse`, which talks to the
//! same Pulse server (PipeWire's pipewire-pulse) the recording does and marks
//! the system default with `*`. Nothing here changes the system default: a
//! chosen microphone is passed to ffmpeg's pulse input by name for this
//! recording only.

use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The Pulse name ffmpeg resolves to the system default source at open.
pub const DEFAULT_SOURCE: &str = "@DEFAULT_SOURCE@";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub description: String,
    pub default: bool,
}

impl Source {
    /// A sink monitor (desktop audio), not a microphone.
    pub fn monitor(&self) -> bool {
        self.name.ends_with(".monitor") || self.description.starts_with("Monitor of ")
    }
}

/// Parse `ffmpeg -sources pulse`:
/// `* name [description] (none)` for the default, `  name [...]` otherwise.
pub fn parse_sources(text: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    for line in text.lines() {
        let (default, rest) = if let Some(rest) = line.strip_prefix("* ") {
            (true, rest)
        } else if let Some(rest) = line.strip_prefix("  ") {
            (false, rest)
        } else {
            continue;
        };
        let rest = rest.trim();
        let Some((name, tail)) = rest.split_once(" [") else {
            continue;
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        // The description ends at the last "]" before the media types.
        let description = match tail.rfind(']') {
            Some(end) => &tail[..end],
            None => tail,
        };
        sources.push(Source {
            name: name.to_string(),
            description: description.to_string(),
            default,
        });
    }
    sources
}

/// List the sources, or None when ffmpeg cannot (no Pulse server, no pulse
/// support, or no answer within two seconds).
pub fn list_sources(ffmpeg: &str) -> Option<Vec<Source>> {
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-sources", "pulse"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let sources = parse_sources(&String::from_utf8_lossy(&output.stdout));
    (!sources.is_empty()).then_some(sources)
}

/// True for an absent, empty or `default` microphone setting.
pub fn is_default(requested: Option<&str>) -> bool {
    match requested.map(str::trim) {
        None | Some("") => true,
        Some(value) => value.eq_ignore_ascii_case("default"),
    }
}

/// Find the source a `--mic` / `mic =` value means: an exact source name
/// (monitors included), else a case-insensitive name or description, else a
/// case-insensitive substring of a microphone's description or name. Monitors
/// only match by their exact name. The first match in Pulse order wins.
pub fn find<'a>(sources: &'a [Source], wanted: &str) -> Option<&'a Source> {
    let wanted = wanted.trim();
    if let Some(found) = sources.iter().find(|source| source.name == wanted) {
        return Some(found);
    }
    let lower = wanted.to_lowercase();
    let mics = || sources.iter().filter(|source| !source.monitor());
    mics()
        .find(|source| {
            source.name.to_lowercase() == lower || source.description.to_lowercase() == lower
        })
        .or_else(|| mics().find(|source| source.description.to_lowercase().contains(&lower)))
        .or_else(|| mics().find(|source| source.name.to_lowercase().contains(&lower)))
}

/// What the recording reads, stored in session.json as `mic`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MicStatus {
    /// The setting: `default` or the --mic / config value.
    pub requested: String,
    /// What ffmpeg opens: a source name or `@DEFAULT_SOURCE@`.
    pub source: String,
    /// The source's Pulse name, when known (also for the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Why the requested microphone was not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

impl MicStatus {
    /// One line for --status and notifications.
    pub fn label(&self) -> String {
        let mut line = match (&self.description, &self.name) {
            (Some(description), Some(name)) => format!("{description} ({name})"),
            (None, Some(name)) => name.clone(),
            (Some(description), None) => description.clone(),
            (None, None) => self.source.clone(),
        };
        if self.source == DEFAULT_SOURCE {
            line.push_str(", system default");
        }
        line
    }
}

/// Resolve the microphone at start. `sources` is None when they could not be
/// listed. A requested microphone that is missing falls back to the system
/// default with `fallback` set; the recording is never refused for it.
pub fn resolve(requested: Option<&str>, sources: Option<&[Source]>) -> MicStatus {
    let default = sources.and_then(|list| list.iter().find(|source| source.default));
    let system_default = |requested: String, fallback: Option<String>| MicStatus {
        requested,
        source: DEFAULT_SOURCE.into(),
        name: default.map(|source| source.name.clone()),
        description: default.map(|source| source.description.clone()),
        fallback,
    };
    if is_default(requested) {
        return system_default("default".into(), None);
    }
    let wanted = requested.unwrap_or_default().trim().to_string();
    let Some(list) = sources else {
        return system_default(
            wanted.clone(),
            Some(format!(
                "could not list audio sources; microphone \"{wanted}\" was not checked"
            )),
        );
    };
    match find(list, &wanted) {
        Some(found) => MicStatus {
            requested: wanted,
            source: found.name.clone(),
            name: Some(found.name.clone()),
            description: Some(found.description.clone()),
            fallback: None,
        },
        None => system_default(
            wanted.clone(),
            Some(format!("microphone \"{wanted}\" not found")),
        ),
    }
}

/// `--list-mics`: microphones only, `*` marks the system default.
pub fn format_list(sources: &[Source]) -> String {
    let mics: Vec<&Source> = sources.iter().filter(|source| !source.monitor()).collect();
    let width = mics
        .iter()
        .map(|source| source.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for source in &mics {
        let marker = if source.default { '*' } else { ' ' };
        text.push_str(&format!(
            "{marker} {:<width$}  {}{}\n",
            source.name,
            source.description,
            if source.default { "  (default)" } else { "" }
        ));
    }
    if mics.is_empty() {
        text.push_str("no microphones found\n");
    }
    if let Some(monitor) = sources
        .iter()
        .find(|source| source.default && source.monitor())
    {
        text.push_str(&format!(
            "note: the system default source is {} (desktop audio, not a microphone)\n",
            monitor.name
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines are joined with concat! so their leading spaces are kept.
    const LISTING: &str = concat!(
        "Auto-detected sources for pulse:\n",
        "* alsa_output.pci-0000_00_1f.3.analog-stereo.monitor ",
        "[Monitor of Built-in Audio Analog Stereo] (none)\n",
        "  alsa_input.pci-0000_00_1f.3.analog-stereo [Built-in Audio Analog Stereo] (none)\n",
        "  alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo ",
        "[HD Pro Webcam C920 Analog Stereo] (none)\n",
        "  alsa_input.usb-BEHRINGER_UMC1820_D132E6B3-00.capture.0.0 ",
        "[UMC1820 [Pro] Capture] (none)\n",
    );

    #[test]
    fn parses_ffmpeg_source_listing() {
        let sources = parse_sources(LISTING);
        assert_eq!(sources.len(), 4);
        assert!(sources[0].default && sources[0].monitor());
        assert_eq!(sources[1].name, "alsa_input.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(sources[1].description, "Built-in Audio Analog Stereo");
        assert!(!sources[1].default && !sources[1].monitor());
        // Brackets inside a description are kept.
        assert_eq!(sources[3].description, "UMC1820 [Pro] Capture");
        assert!(parse_sources("Cannot list sources: Not implemented\n").is_empty());
    }

    #[test]
    fn finds_by_name_description_and_substring() {
        let sources = parse_sources(LISTING);
        let name = |wanted: &str| find(&sources, wanted).map(|found| found.name.as_str());
        let webcam = "alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo";
        assert_eq!(name(webcam), Some(webcam));
        assert_eq!(name("webcam"), Some(webcam));
        assert_eq!(name("WEBCAM"), Some(webcam));
        assert_eq!(name("hd pro webcam c920 analog stereo"), Some(webcam));
        assert_eq!(
            name("umc1820"),
            Some("alsa_input.usb-BEHRINGER_UMC1820_D132E6B3-00.capture.0.0")
        );
        // A name substring matches when no description does.
        assert_eq!(name("behringer"), name("umc1820"));
        // "built-in" is in the monitor's description too, but monitors only
        // match by exact name.
        assert_eq!(
            name("built-in"),
            Some("alsa_input.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(
            name("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor")
        );
        assert_eq!(name("monitor"), None);
        assert_eq!(name("scarlett"), None);
    }

    #[test]
    fn resolve_default_found_missing_and_unlisted() {
        let sources = parse_sources(LISTING);
        for requested in [None, Some(""), Some("default"), Some("Default")] {
            let mic = resolve(requested, Some(&sources));
            assert_eq!(mic.requested, "default");
            assert_eq!(mic.source, DEFAULT_SOURCE);
            assert_eq!(
                mic.name.as_deref(),
                Some("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor")
            );
            assert!(mic.fallback.is_none());
        }
        let mic = resolve(Some("webcam"), Some(&sources));
        assert_eq!(
            mic.source,
            "alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo"
        );
        assert_eq!(mic.requested, "webcam");
        assert!(mic.fallback.is_none());
        assert_eq!(
            mic.label(),
            "HD Pro Webcam C920 Analog Stereo (alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo)"
        );
        // Unplugged: fall back to the system default, with the reason.
        let mic = resolve(Some("scarlett"), Some(&sources));
        assert_eq!(mic.source, DEFAULT_SOURCE);
        assert_eq!(mic.requested, "scarlett");
        assert_eq!(
            mic.fallback.as_deref(),
            Some("microphone \"scarlett\" not found")
        );
        assert!(mic.label().ends_with(", system default"));
        // No listing: the default, and the setting is reported unchecked.
        let mic = resolve(Some("webcam"), None);
        assert_eq!(mic.source, DEFAULT_SOURCE);
        assert!(mic.name.is_none());
        assert!(mic.fallback.unwrap().contains("could not list"));
        assert_eq!(
            resolve(None, None).label(),
            "@DEFAULT_SOURCE@, system default"
        );
    }

    #[test]
    fn list_excludes_monitors_and_marks_default() {
        let mut sources = parse_sources(LISTING);
        let text = format_list(&sources);
        assert!(!text
            .lines()
            .any(|line| !line.starts_with("note:") && line.contains(".monitor")));
        assert!(text.contains("HD Pro Webcam C920"));
        assert!(text.contains("UMC1820 [Pro] Capture"));
        assert!(text.contains("note: the system default source is"));
        assert!(!text.contains("(default)"));
        sources[0].default = false;
        sources[2].default = true;
        let text = format_list(&sources);
        let line = text.lines().find(|line| line.starts_with('*')).unwrap();
        assert!(line.contains("Webcam") && line.ends_with("(default)"));
        assert!(!text.contains("note:"));
        assert_eq!(format_list(&[]), "no microphones found\n");
    }
}
