//! Microphone selection: which PipeWire/Pulse source the recording reads.
//!
//! The sources are listed with `ffmpeg -sources pulse`, which talks to the
//! same Pulse server (PipeWire's pipewire-pulse) the recording does and marks
//! the system default with `*`. Nothing here changes the system default: a
//! chosen microphone is passed to ffmpeg's pulse input by name for this
//! recording only.
//!
//! Multichannel interfaces (a UMC1820 shows 18 inputs as AUX0..AUX17): the
//! channel count and names come from `pw-dump`, and `--mic-channels` picks one
//! input as mono or a pair as stereo. The mapping happens in the capture
//! pipeline only: ffmpeg opens the source with all its channels and the
//! stream property `stream.dont-remix=true` (so PipeWire hands the inputs
//! over one to one instead of mapping AUX positions onto FL/FR), and a `pan`
//! filter keeps the chosen inputs. No PipeWire node is created and nothing
//! outlives the recording.

use serde::{Deserialize, Serialize};
use std::io::Read;
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

/// The most inputs a choice may name (Pulse streams carry at most 32).
pub const MAX_CHANNELS: u32 = 32;

/// The PipeWire stream property that hands a multichannel source's inputs
/// over one to one (AUX0 is channel 1, ...) instead of remixing them.
pub const DONT_REMIX: &str = "stream.dont-remix=true";

/// A choice of inputs on a multichannel source, 1-based like the labels on
/// the interface: one input recorded as mono, or a pair as stereo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
    Mono(u32),
    Stereo(u32, u32),
}

impl Channels {
    /// `3` (mono), `1,2` or `1+2` (stereo, left then right); `all`,
    /// `default` or empty mean no choice (None).
    pub fn parse(value: &str) -> Result<Option<Self>, String> {
        let value = value.trim();
        if value.is_empty()
            || value.eq_ignore_ascii_case("all")
            || value.eq_ignore_ascii_case("default")
        {
            return Ok(None);
        }
        let error = || {
            format!(
                "mic channels must be one input (3), a pair (1,2) or all, with inputs 1..{MAX_CHANNELS}: {value}"
            )
        };
        let input = |text: &str| -> Result<u32, String> {
            let number: u32 = text.trim().parse().map_err(|_| error())?;
            if (1..=MAX_CHANNELS).contains(&number) {
                Ok(number)
            } else {
                Err(error())
            }
        };
        let parts: Vec<&str> = value.split([',', '+']).collect();
        match parts.as_slice() {
            [one] => Ok(Some(Self::Mono(input(one)?))),
            [left, right] => {
                let (left, right) = (input(left)?, input(right)?);
                if left == right {
                    return Err(error());
                }
                Ok(Some(Self::Stereo(left, right)))
            }
            _ => Err(error()),
        }
    }

    /// The setting as written in the config: `3` or `1,2`.
    pub fn as_setting(&self) -> String {
        match self {
            Self::Mono(input) => input.to_string(),
            Self::Stereo(left, right) => format!("{left},{right}"),
        }
    }

    /// The 1-based inputs, in output order.
    pub fn inputs(&self) -> Vec<u32> {
        match *self {
            Self::Mono(input) => vec![input],
            Self::Stereo(left, right) => vec![left, right],
        }
    }

    pub fn highest(&self) -> u32 {
        self.inputs().into_iter().max().unwrap_or(1)
    }

    /// The ffmpeg `pan` filter that keeps the chosen inputs. `stereo_out`
    /// puts a mono input on both sides (for mixing with desktop audio).
    pub fn pan(&self, stereo_out: bool) -> String {
        match *self {
            Self::Mono(input) if stereo_out => {
                format!("pan=stereo|c0=c{0}|c1=c{0}", input - 1)
            }
            Self::Mono(input) => format!("pan=mono|c0=c{}", input - 1),
            Self::Stereo(left, right) => {
                format!("pan=stereo|c0=c{}|c1=c{}", left - 1, right - 1)
            }
        }
    }
}

/// A source's channel layout as PipeWire reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub count: u32,
    /// Position names (AUX0.., FL, FR); may be empty when unknown.
    pub positions: Vec<String>,
}

impl Layout {
    /// The name of a 1-based input, or `inN` when PipeWire gave none.
    pub fn position(&self, input: u32) -> String {
        self.positions
            .get(input as usize - 1)
            .cloned()
            .unwrap_or_else(|| format!("in{input}"))
    }
}

/// A channel list from pw-dump: a JSON array or a string such as
/// `FL,FR` or `[ AUX0 AUX1 ]`.
fn positions_of(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(text) => text
            .split(|c: char| c == ',' || c == '[' || c == ']' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn count_of(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(number) => number.as_u64().map(|n| n as u32),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        // A range in EnumFormat: {"default": 2, "min": 1, "max": 8}.
        serde_json::Value::Object(map) => map.get("default").and_then(count_of),
        _ => None,
    }
}

/// Parse `pw-dump` into (node name, layout) for every audio source. The
/// negotiated Format wins, then the first EnumFormat, the DSP port config
/// and the node's `audio.channels` / `audio.position` properties.
pub fn parse_pw_dump(text: &str) -> Vec<(String, Layout)> {
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return Vec::new();
    };
    let mut layouts = Vec::new();
    for object in &objects {
        let info = &object["info"];
        let props = &info["props"];
        let class = props["media.class"].as_str().unwrap_or_default();
        if !(class.starts_with("Audio/Source") || class.starts_with("Audio/Duplex")) {
            continue;
        }
        let Some(name) = props["node.name"].as_str() else {
            continue;
        };
        let params = &info["params"];
        let formats = [
            &params["Format"][0],
            &params["EnumFormat"][0],
            &params["PortConfig"][0]["format"],
        ];
        let mut layout = None;
        for format in formats {
            if let Some(count) = count_of(&format["channels"]).filter(|count| *count > 0) {
                layout = Some(Layout {
                    count,
                    positions: positions_of(&format["position"]),
                });
                break;
            }
        }
        if layout.is_none() {
            if let Some(count) = count_of(&props["audio.channels"]).filter(|count| *count > 0) {
                layout = Some(Layout {
                    count,
                    positions: positions_of(&props["audio.position"]),
                });
            }
        }
        if let Some(mut layout) = layout {
            if layout.positions.len() != layout.count as usize {
                layout.positions.clear();
            }
            layouts.push((name.to_string(), layout));
        }
    }
    layouts
}

/// Run `pw-dump` (read in a thread: the dump is larger than a pipe buffer),
/// or None when it is missing, fails or takes longer than three seconds.
pub fn list_layouts(pw_dump: &str) -> Option<Vec<(String, Layout)>> {
    let mut child = Command::new(pw_dump)
        .arg("--no-colors")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = reader.join().ok()?;
    Some(parse_pw_dump(&text))
}

pub fn layout_of<'a>(layouts: &'a [(String, Layout)], name: &str) -> Option<&'a Layout> {
    layouts
        .iter()
        .find(|(node, _)| node == name)
        .map(|(_, layout)| layout)
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
    /// The source's channel count, when inputs were chosen (0.7.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// The inputs recorded, `3` or `1,2`, when a choice is in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<String>,
    /// Their PipeWire names (AUX2, or AUX0 and AUX1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_names: Option<Vec<String>>,
    /// Why the requested inputs were not used (the source is recorded as
    /// before, in stereo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_fallback: Option<String>,
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
        if let (Some(inputs), Some(names)) = (&self.inputs, &self.input_names) {
            let kind = if names.len() == 1 { "mono" } else { "stereo" };
            let count = self
                .channels
                .map(|count| format!(" of {count}"))
                .unwrap_or_default();
            line.push_str(&format!(
                ", input {}{count} ({}) as {kind}",
                inputs.replace(',', "+"),
                names.join(", ")
            ));
        }
        line
    }
}

/// Apply a channel choice to the resolved microphone. Returns the source's
/// channel count and the choice when the recording should use it; otherwise
/// records why in `inputs_fallback` and the microphone is recorded as
/// before. The recording is never refused for it.
pub fn resolve_channels(
    status: &mut MicStatus,
    wanted: Option<Channels>,
    layouts: Option<&[(String, Layout)]>,
) -> Option<(u32, Channels)> {
    let wanted = wanted?;
    let setting = wanted.as_setting();
    let reason = if status.fallback.is_some() {
        format!("the chosen microphone is missing, so inputs {setting} were not used")
    } else if let Some(name) = status.name.clone() {
        match layouts {
            None => format!(
                "could not read the channel layout (pw-dump), so inputs {setting} were not used"
            ),
            Some(list) => match layout_of(list, &name) {
                None => {
                    format!("the channel layout of {name} is unknown, so inputs {setting} were not used")
                }
                Some(layout) if wanted.highest() > layout.count => format!(
                    "{} has {} input{}, so inputs {setting} were not used",
                    status.description.as_deref().unwrap_or(&name),
                    layout.count,
                    if layout.count == 1 { "" } else { "s" }
                ),
                Some(layout) => {
                    status.channels = Some(layout.count);
                    status.inputs = Some(setting);
                    status.input_names = Some(
                        wanted
                            .inputs()
                            .into_iter()
                            .map(|input| layout.position(input))
                            .collect(),
                    );
                    return Some((layout.count, wanted));
                }
            },
        }
    } else {
        format!(
            "could not tell which source is the system default, so inputs {setting} were not used"
        )
    };
    status.inputs_fallback = Some(reason);
    None
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
        ..MicStatus::default()
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
            ..MicStatus::default()
        },
        None => system_default(
            wanted.clone(),
            Some(format!("microphone \"{wanted}\" not found")),
        ),
    }
}

/// `--list-mics`: microphones only, `*` marks the system default. With the
/// PipeWire layouts each microphone gets a second line with its channels;
/// sources with more than two say how to pick inputs.
pub fn format_list(sources: &[Source], layouts: Option<&[(String, Layout)]>) -> String {
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
        if let Some(layout) = layouts.and_then(|list| layout_of(list, &source.name)) {
            let names = if layout.positions.is_empty() {
                String::new()
            } else {
                format!(": {}", layout.positions.join(" "))
            };
            text.push_str(&format!(
                "    {} channel{}{names}\n",
                layout.count,
                if layout.count == 1 { "" } else { "s" }
            ));
            if layout.count > 2 {
                text.push_str(&format!(
                    "    pick inputs with --mic-channels N (mono) or N,M (stereo), 1..{}\n",
                    layout.count
                ));
            }
        }
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
        let text = format_list(&sources, None);
        assert!(!text
            .lines()
            .any(|line| !line.starts_with("note:") && line.contains(".monitor")));
        assert!(text.contains("HD Pro Webcam C920"));
        assert!(text.contains("UMC1820 [Pro] Capture"));
        assert!(text.contains("note: the system default source is"));
        assert!(!text.contains("(default)"));
        sources[0].default = false;
        sources[2].default = true;
        let text = format_list(&sources, None);
        let line = text.lines().find(|line| line.starts_with('*')).unwrap();
        assert!(line.contains("Webcam") && line.ends_with("(default)"));
        assert!(!text.contains("note:"));
        assert_eq!(format_list(&[], None), "no microphones found\n");
    }

    // A trimmed pw-dump: a UMC1820-like 18-input source with only an
    // EnumFormat (suspended), a stereo webcam with a negotiated Format, a
    // source known only from its properties, a sink and a client.
    const PW_DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Client", "info": {"props": {"application.name": "x"}}},
      {"id": 41, "type": "PipeWire:Interface:Node", "info": {"props": {
        "media.class": "Audio/Source",
        "node.name": "alsa_input.usb-BEHRINGER_UMC1820_D132E6B3-00.capture.0.0",
        "audio.channels": 18},
        "params": {"EnumFormat": [{"mediaType": "audio", "channels": 18, "position":
          ["AUX0","AUX1","AUX2","AUX3","AUX4","AUX5","AUX6","AUX7","AUX8","AUX9","AUX10",
           "AUX11","AUX12","AUX13","AUX14","AUX15","AUX16","AUX17"]}]}}},
      {"id": 42, "type": "PipeWire:Interface:Node", "info": {"props": {
        "media.class": "Audio/Source",
        "node.name": "alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo"},
        "params": {"Format": [{"channels": 2, "position": ["FL","FR"]}],
                   "EnumFormat": [{"channels": {"default": 2, "min": 1, "max": 2}}]}}},
      {"id": 43, "type": "PipeWire:Interface:Node", "info": {"props": {
        "media.class": "Audio/Source/Virtual", "node.name": "virtual_4ch",
        "audio.channels": "4", "audio.position": "[ AUX0 AUX1 AUX2 AUX3 ]"}, "params": {}}},
      {"id": 44, "type": "PipeWire:Interface:Node", "info": {"props": {
        "media.class": "Audio/Sink", "node.name": "alsa_output.speakers",
        "audio.channels": 2}, "params": {}}}
    ]"#;

    #[test]
    fn parses_channel_choices() {
        assert_eq!(Channels::parse("3"), Ok(Some(Channels::Mono(3))));
        assert_eq!(Channels::parse(" 1,2 "), Ok(Some(Channels::Stereo(1, 2))));
        assert_eq!(Channels::parse("4+3"), Ok(Some(Channels::Stereo(4, 3))));
        for none in ["", "all", "ALL", "default"] {
            assert_eq!(Channels::parse(none), Ok(None));
        }
        for bad in ["0", "33", "1,1", "1,2,3", "x", "-1", "1,"] {
            assert!(Channels::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Channels::Stereo(4, 3).as_setting(), "4,3");
        assert_eq!(Channels::Mono(3).pan(false), "pan=mono|c0=c2");
        assert_eq!(Channels::Mono(3).pan(true), "pan=stereo|c0=c2|c1=c2");
        assert_eq!(Channels::Stereo(1, 2).pan(false), "pan=stereo|c0=c0|c1=c1");
        assert_eq!(Channels::Stereo(1, 2).pan(true), "pan=stereo|c0=c0|c1=c1");
    }

    #[test]
    fn parses_pw_dump_layouts() {
        let layouts = parse_pw_dump(PW_DUMP);
        assert_eq!(layouts.len(), 3);
        let umc = layout_of(
            &layouts,
            "alsa_input.usb-BEHRINGER_UMC1820_D132E6B3-00.capture.0.0",
        )
        .unwrap();
        assert_eq!(umc.count, 18);
        assert_eq!(umc.position(3), "AUX2");
        let webcam = layout_of(
            &layouts,
            "alsa_input.usb-046d_HD_Pro_Webcam_C920-02.analog-stereo",
        )
        .unwrap();
        assert_eq!(
            (webcam.count, webcam.positions.join(" ").as_str()),
            (2, "FL FR")
        );
        let virtual4 = layout_of(&layouts, "virtual_4ch").unwrap();
        assert_eq!(virtual4.count, 4);
        assert_eq!(virtual4.positions, ["AUX0", "AUX1", "AUX2", "AUX3"]);
        assert!(layout_of(&layouts, "alsa_output.speakers").is_none());
        assert!(parse_pw_dump("not json").is_empty());
        assert!(list_layouts("/nonexistent/pw-dump").is_none());
    }

    #[test]
    fn resolves_channels_or_falls_back() {
        let sources = parse_sources(LISTING);
        let layouts = parse_pw_dump(PW_DUMP);
        let mut mic = resolve(Some("umc1820"), Some(&sources));
        assert_eq!(
            resolve_channels(&mut mic, Channels::parse("3").unwrap(), Some(&layouts)),
            Some((18, Channels::Mono(3)))
        );
        assert_eq!(mic.channels, Some(18));
        assert_eq!(mic.input_names.as_deref(), Some(&["AUX2".to_string()][..]));
        assert!(mic.inputs_fallback.is_none());
        assert!(mic.label().ends_with(", input 3 of 18 (AUX2) as mono"));
        let mut mic = resolve(Some("umc1820"), Some(&sources));
        assert!(
            resolve_channels(&mut mic, Channels::parse("1,2").unwrap(), Some(&layouts)).is_some()
        );
        assert!(mic
            .label()
            .ends_with(", input 1+2 of 18 (AUX0, AUX1) as stereo"));
        // No choice: nothing changes.
        let mut mic = resolve(Some("umc1820"), Some(&sources));
        assert!(resolve_channels(&mut mic, None, Some(&layouts)).is_none());
        assert_eq!(mic, resolve(Some("umc1820"), Some(&sources)));
        // An input the source does not have.
        let mut mic = resolve(Some("webcam"), Some(&sources));
        assert!(
            resolve_channels(&mut mic, Channels::parse("3").unwrap(), Some(&layouts)).is_none()
        );
        assert_eq!(
            mic.inputs_fallback.as_deref(),
            Some("HD Pro Webcam C920 Analog Stereo has 2 inputs, so inputs 3 were not used")
        );
        assert!(mic.inputs.is_none());
        // A missing microphone, an unknown layout, no pw-dump.
        let mut mic = resolve(Some("scarlett"), Some(&sources));
        assert!(
            resolve_channels(&mut mic, Channels::parse("1").unwrap(), Some(&layouts)).is_none()
        );
        assert!(mic.inputs_fallback.unwrap().contains("missing"));
        let mut mic = resolve(Some("built-in"), Some(&sources));
        assert!(
            resolve_channels(&mut mic, Channels::parse("1").unwrap(), Some(&layouts)).is_none()
        );
        assert!(mic.inputs_fallback.unwrap().contains("unknown"));
        let mut mic = resolve(Some("umc1820"), Some(&sources));
        assert!(resolve_channels(&mut mic, Channels::parse("1").unwrap(), None).is_none());
        assert!(mic.inputs_fallback.unwrap().contains("pw-dump"));
        // Older session files without the new fields still load.
        let old: MicStatus =
            serde_json::from_str(r#"{"requested":"default","source":"@DEFAULT_SOURCE@"}"#).unwrap();
        assert!(old.channels.is_none() && old.inputs.is_none());
    }

    #[test]
    fn list_shows_channels() {
        let sources = parse_sources(LISTING);
        let layouts = parse_pw_dump(PW_DUMP);
        let text = format_list(&sources, Some(&layouts));
        assert!(text.contains("    18 channels: AUX0 AUX1 AUX2"));
        assert!(text.contains("    2 channels: FL FR\n"));
        assert_eq!(text.matches("--mic-channels").count(), 1);
        assert!(text.contains("1..18"));
    }
}
