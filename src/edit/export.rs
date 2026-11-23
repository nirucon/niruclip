//! Turn a project into ffmpeg runs.
//!
//! Every clip becomes its own input (seeked on the input side, so a long
//! recording is not decoded from the start), gets its own filter chain, and
//! the chains are joined with `concat` or `xfade`/`acrossfade`. Music is
//! delayed to its place, lowered under speech and mixed under the film.
//!
//! An export is one to three steps: the loudness is measured first when
//! the film is levelled (two-pass `loudnorm` is exact, one pass guesses),
//! and a size limit encodes twice so the file lands just under it.
use super::project::{window, Clip, Effects, Project};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mp4,
    Mp4Small,
    WebM,
    Mkv,
    Gif,
    Mp3,
    Wav,
    Flac,
    Opus,
}

impl Format {
    pub const ALL: [Format; 9] = [
        Format::Mp4,
        Format::Mp4Small,
        Format::WebM,
        Format::Mkv,
        Format::Gif,
        Format::Mp3,
        Format::Wav,
        Format::Flac,
        Format::Opus,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Mp4 => "MP4 (H.264, best for most uses)",
            Format::Mp4Small => "MP4 small (to share or mail)",
            Format::WebM => "WebM (VP9 and Opus)",
            Format::Mkv => "MKV (high quality)",
            Format::Gif => "GIF (animation, no sound)",
            Format::Mp3 => "MP3 (sound only)",
            Format::Wav => "WAV (sound only, uncompressed)",
            Format::Flac => "FLAC (sound only, lossless)",
            Format::Opus => "Opus (sound only, small)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp4 | Format::Mp4Small => "mp4",
            Format::WebM => "webm",
            Format::Mkv => "mkv",
            Format::Gif => "gif",
            Format::Mp3 => "mp3",
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Opus => "opus",
        }
    }

    pub fn has_video(self) -> bool {
        matches!(
            self,
            Format::Mp4 | Format::Mp4Small | Format::WebM | Format::Mkv | Format::Gif
        )
    }

    pub fn has_audio(self) -> bool {
        self != Format::Gif
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "mp4" => Format::Mp4,
            "mp4-small" | "share" => Format::Mp4Small,
            "webm" => Format::WebM,
            "mkv" => Format::Mkv,
            "gif" => Format::Gif,
            "mp3" => Format::Mp3,
            "wav" => Format::Wav,
            "flac" => Format::Flac,
            "opus" => Format::Opus,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Mp4Small => "mp4-small",
            Format::WebM => "webm",
            Format::Mkv => "mkv",
            Format::Gif => "gif",
            Format::Mp3 => "mp3",
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Opus => "opus",
        }
    }
}

/// Output picture height; 0 keeps the project size.
pub const HEIGHTS: [u32; 6] = [0, 2160, 1440, 1080, 720, 480];

/// Size limits in MB, 0 is none.
pub const LIMITS: [u32; 6] = [0, 8, 10, 25, 50, 100];

pub fn limit_label(mb: u32) -> String {
    match mb {
        0 => "No limit".into(),
        10 => "10 MB (Discord)".into(),
        25 => "25 MB (mail)".into(),
        n => format!("{n} MB"),
    }
}

/// Loudness targets (LUFS) by `Project::loudness`.
pub const LOUDNESS: [(&str, f64); 4] = [
    ("Off", 0.0),
    ("Speech and web, −16 LUFS", -16.0),
    ("Streaming, −14 LUFS", -14.0),
    ("Broadcast, −23 LUFS", -23.0),
];

/// Sound treatments by `Clip::voice`.
pub const VOICE: [&str; 4] = ["Off", "Voice", "Voice, noisy room", "Even out"];

/// The filters of a sound treatment (also used live in the preview).
pub fn voice_chain(voice: u8) -> &'static str {
    match voice {
        // Rumble out, light noise reduction, softer s sounds, an even level.
        1 => "highpass=f=80,afftdn=nr=12:nf=-50:tn=1,deesser=i=0.4,acompressor=threshold=0.1:ratio=3:attack=10:release=180:makeup=2",
        2 => "highpass=f=100,lowpass=f=12000,afftdn=nr=24:nf=-38:tn=1,deesser=i=0.5,acompressor=threshold=0.1:ratio=4:attack=8:release=200:makeup=2.5",
        3 => "dynaudnorm=f=250:g=15:p=0.9:m=8",
        _ => "",
    }
}

/// What to do with captions: 0 leave out, 1 burn into the picture, 2 a
/// subtitle track (and an .srt file beside the film).
pub const CAPTIONS: [&str; 3] = [
    "Leave out",
    "Burn into the picture",
    "Subtitle track and .srt",
];

#[derive(Clone, Debug)]
pub struct Settings {
    pub format: Format,
    pub height: u32,
    pub output: PathBuf,
    /// The font file for titles without a font of their own.
    pub font: Option<PathBuf>,
    /// Folder for the title text files.
    pub work: PathBuf,
    /// Render only this film range (preview of a part); None is all.
    pub range: Option<(f64, f64)>,
    /// Fast, lower quality encoding (previews).
    pub draft: bool,
    /// The file must fit in this many MB, 0 is no limit.
    pub limit_mb: u32,
    /// See `CAPTIONS`.
    pub captions: u8,
}

impl Settings {
    pub fn new(format: Format, output: PathBuf, work: PathBuf) -> Self {
        Self {
            format,
            height: 0,
            output,
            font: None,
            work,
            range: None,
            draft: false,
            limit_mb: 0,
            captions: 0,
        }
    }
}

/// What a plan is for.
#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    /// The film (or a preview).
    Film,
    /// Measure the loudness: sound only, nothing written.
    Measure,
    /// The film's own sound (no music, no levelling) through `tail` into
    /// `out` (output arguments), for pause finding and captions.
    Sound { tail: String, out: Vec<String> },
}

fn even(n: f64) -> u32 {
    ((n / 2.0).round() as u32 * 2).max(2)
}

/// Output size and rate for a project and a chosen height.
pub fn frame(project: &Project, height: u32, format: Format) -> (u32, u32, f64) {
    let (mut w, mut h) = if project.width > 0 && project.height > 0 {
        (project.width, project.height)
    } else {
        (1920, 1080)
    };
    let mut target = height;
    if format == Format::Mp4Small && (target == 0 || target > 1080) {
        target = h.min(1080);
    }
    if format == Format::Gif && (target == 0 || target > 480) {
        target = h.min(480);
    }
    if target > 0 && target != h {
        w = even(w as f64 * target as f64 / h as f64);
        h = target;
    }
    let fps = if format == Format::Gif {
        12.0
    } else if project.fps > 0.0 {
        project.fps.min(60.0)
    } else {
        30.0
    };
    (even(w as f64), even(h as f64), fps)
}

/// `atempo` takes 0.5..2; chain it for 0.25..4.
fn atempo(speed: f64) -> String {
    let mut parts = Vec::new();
    let mut s = speed;
    while s > 2.0 {
        parts.push("atempo=2".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s /= 0.5;
    }
    if (s - 1.0).abs() > 1e-6 {
        let text = format!("{s:.6}");
        parts.push(format!(
            "atempo={}",
            text.trim_end_matches('0').trim_end_matches('.')
        ));
    }
    parts.join(",")
}

pub fn quote(path: &Path) -> String {
    // Filter option values are single-quoted; a quote inside cannot occur in
    // the paths we write ourselves, and is escaped for system font paths.
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// One ffmpeg run of an export.
#[derive(Clone, Debug)]
pub struct Step {
    pub args: Vec<String>,
    /// Share of the whole job, for the progress bar.
    pub weight: f64,
    /// Read the loudness measurement from this run's log.
    pub measure: bool,
}

pub struct Plan {
    pub steps: Vec<Step>,
    pub duration: f64,
    /// Title text files and pass logs to remove afterwards.
    pub temp: Vec<PathBuf>,
    /// Files written beside the film (an .srt).
    pub extra: Vec<PathBuf>,
}

impl Plan {
    /// The run that writes the film.
    pub fn main(&self) -> &[String] {
        &self.steps.last().expect("a plan has a step").args
    }
}

/// Where the measured loudness goes in a later step's filters.
pub const MEASURED: &str = "@MEASURED@";

/// The `loudnorm` options for a measurement (ffmpeg's JSON print).
pub fn measured_options(log: &str) -> Option<String> {
    let start = log.rfind("\"input_i\"")?;
    let open = log[..start].rfind('{')?;
    let close = open + log[open..].find('}')?;
    let v: serde_json::Value = serde_json::from_str(&log[open..=close]).ok()?;
    let get = |k: &str| -> Option<f64> {
        let x = v[k].as_str()?.trim().parse::<f64>().ok()?;
        x.is_finite().then_some(x)
    };
    let (i, tp, lra, thresh, offset) = (
        get("input_i")?,
        get("input_tp")?,
        get("input_lra")?,
        get("input_thresh")?,
        get("target_offset")?,
    );
    // Silence measures as -inf / -70: nothing to level.
    if i < -69.0 {
        return None;
    }
    Some(format!(
        ":measured_I={i:.2}:measured_TP={tp:.2}:measured_LRA={lra:.2}:measured_thresh={thresh:.2}:offset={offset:.2}:linear=true"
    ))
}

/// The clips (and how much of each) a range of the film needs.
fn ranged(project: &Project, range: Option<(f64, f64)>) -> Project {
    let mut p = project.clone();
    if let Some((a, b)) = range {
        p.keep_range(a, b);
        // Music moves with the cut.
        p.music = project
            .music
            .iter()
            .filter_map(|m| {
                let end = m.at + m.length();
                if end <= a || m.at >= b {
                    return None;
                }
                let mut m = m.clone();
                if m.at < a {
                    m.start += a - m.at;
                    m.at = a;
                }
                m.end = m.end.min(m.start + (b - m.at));
                m.at -= a;
                Some(m)
            })
            .collect();
    }
    p
}

/// The x264/VP9 bitrate (bits/s) and height that make `seconds` of film
/// fit in `mb` megabytes, with `audio` bits/s of sound.
pub fn fit_size(mb: u32, seconds: f64, audio: f64, w: u32, h: u32, fps: f64) -> (f64, u32) {
    let bits = f64::from(mb) * 8.0e6 * 0.94;
    let video = (bits / seconds.max(0.1) - audio).max(60_000.0);
    // Enough bits a pixel for screen text to stay readable; else smaller.
    let mut height = h;
    for candidate in [h, 1080, 720, 540, 480, 360] {
        if candidate > h {
            continue;
        }
        height = candidate;
        let cw = f64::from(w) * f64::from(candidate) / f64::from(h);
        if video / (cw * f64::from(candidate) * fps.max(1.0)) >= 0.035 {
            break;
        }
    }
    (video, height)
}

pub fn plan(project: &Project, settings: &Settings) -> Result<Plan, String> {
    let film = ranged(project, settings.range);
    if film.clips.is_empty() {
        return Err("the timeline is empty".into());
    }
    let format = settings.format;
    let mut temp = Vec::new();
    let mut extra = Vec::new();
    let level = film.loudness > 0 && format.has_audio();
    let mut settings = settings.clone();
    let mut bitrate = None;
    if settings.limit_mb > 0 && format.has_video() && format != Format::Gif && !settings.draft {
        let (w, h, fps) = frame(&film, settings.height, format);
        let audio = if settings.limit_mb <= 10 {
            96_000.0
        } else {
            128_000.0
        };
        let (video, height) = fit_size(settings.limit_mb, film.duration(), audio, w, h, fps);
        if height < h {
            settings.height = height;
        }
        bitrate = Some((video, audio));
    }
    let main = build(&film, &settings, &Purpose::Film, &mut temp, &mut extra)?;
    let duration = main.1;
    let mut steps = Vec::new();
    if level && !settings.draft {
        let (args, _) = build(&film, &settings, &Purpose::Measure, &mut temp, &mut extra)?;
        steps.push(Step {
            args,
            weight: 0.15,
            measure: true,
        });
    }
    let mut args = main.0;
    if settings.draft {
        // A preview levels in one pass.
        for a in &mut args {
            if a.contains(MEASURED) {
                *a = a.replace(MEASURED, "");
            }
        }
    }
    match bitrate {
        Some((video, audio)) => {
            let log = settings
                .work
                .join(format!("pass-{}-{}", std::process::id(), temp.len()));
            for suffix in [
                "-0.log",
                "-0.log.mbtree",
                "-0.log.temp",
                "-0.log.mbtree.temp",
            ] {
                let mut name = log.as_os_str().to_os_string();
                name.push(suffix);
                temp.push(PathBuf::from(name));
            }
            let rate = |args: &mut Vec<String>, pass: u32| {
                let out = args.pop().unwrap_or_default();
                // The quality setting gives way to the bitrate.
                if let Some(i) = args.iter().position(|a| a == "-crf") {
                    args.drain(i..i + 2);
                }
                if let Some(i) = args.iter().position(|a| a == "-b:v") {
                    args.drain(i..i + 2);
                }
                if let Some(i) = args.iter().position(|a| a == "-b:a") {
                    args[i + 1] = format!("{}k", (audio / 1000.0) as u32);
                }
                args.extend([
                    "-b:v".into(),
                    format!("{}", video as u64),
                    "-pass".into(),
                    pass.to_string(),
                    "-passlogfile".into(),
                    log.display().to_string(),
                ]);
                if pass == 1 {
                    args.extend(["-f".into(), "null".into(), "-".into()]);
                } else {
                    args.push(out);
                }
            };
            let mut first = args.clone();
            // Pass one needs no subtitle track.
            strip_subtitles(&mut first);
            rate(&mut first, 1);
            rate(&mut args, 2);
            steps.push(Step {
                args: first,
                weight: 0.4,
                measure: false,
            });
            steps.push(Step {
                args,
                weight: 0.45,
                measure: false,
            });
        }
        None => steps.push(Step {
            args,
            weight: 0.85,
            measure: false,
        }),
    }
    let total: f64 = steps.iter().map(|s| s.weight).sum();
    for s in &mut steps {
        s.weight /= total;
    }
    Ok(Plan {
        steps,
        duration,
        temp,
        extra,
    })
}

fn strip_subtitles(args: &mut Vec<String>) {
    let mut i = 0;
    while i + 1 < args.len() {
        if (args[i] == "-map" && args[i + 1].ends_with(":s")) || args[i] == "-c:s" {
            args.drain(i..i + 2);
        } else {
            i += 1;
        }
    }
}

/// A plan for the film's own sound (see `Purpose::Sound`).
pub fn sound_plan(
    project: &Project,
    work: &Path,
    tail: &str,
    out: Vec<String>,
) -> Result<Plan, String> {
    let mut temp = Vec::new();
    let mut extra = Vec::new();
    let settings = Settings::new(Format::Wav, PathBuf::from("-"), work.to_path_buf());
    let purpose = Purpose::Sound {
        tail: tail.to_string(),
        out,
    };
    let (args, duration) = build(project, &settings, &purpose, &mut temp, &mut extra)?;
    Ok(Plan {
        steps: vec![Step {
            args,
            weight: 1.0,
            measure: false,
        }],
        duration,
        temp,
        extra,
    })
}

/// One ffmpeg command for a purpose, and the film length.
#[allow(clippy::needless_range_loop)]
fn build(
    project: &Project,
    settings: &Settings,
    purpose: &Purpose,
    temp: &mut Vec<PathBuf>,
    extra: &mut Vec<PathBuf>,
) -> Result<(Vec<String>, f64), String> {
    let film_only = *purpose == Purpose::Film;
    let format = settings.format;
    let want_video = film_only && format.has_video();
    let want_audio = !film_only || format.has_audio();
    let music = film_only || *purpose == Purpose::Measure;
    let (w, h, fps) = frame(project, settings.height, format);
    let loglevel = if *purpose == Purpose::Measure {
        "info"
    } else {
        "error"
    };
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-loglevel".into(),
        loglevel.into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ];
    let mut graph = String::new();
    let mut input = 0usize;
    let mut lengths = Vec::new();
    // Burnt-in captions take the bottom: titles there sit above them.
    let lift = if film_only && settings.captions == 1 && project.caption_count() > 0 {
        let share = [4.2, 5.2, 6.5][project.caption_size.min(2) as usize];
        (f64::from(h) * share / 100.0 * 1.3 * 2.0 + f64::from(h) * 0.02).round()
    } else {
        0.0
    };
    for (i, clip) in project.clips.iter().enumerate() {
        let media = &project.media[clip.media];
        let length = clip.length();
        lengths.push(length);
        let source = clip.end - clip.start;
        let sound = media.has_audio && !clip.mute && !media.still;
        let needed = want_video || (want_audio && sound);
        let index = input;
        if needed {
            if media.is_card() {
                args.extend([
                    "-f".into(),
                    "lavfi".into(),
                    "-t".into(),
                    format!("{source:.6}"),
                    "-i".into(),
                    format!(
                        "color=c=0x{:06x}:s={w}x{h}:r={fps}",
                        clip.effects.card_colour & 0xffffff
                    ),
                ]);
            } else if media.still {
                args.extend([
                    "-loop".into(),
                    "1".into(),
                    "-framerate".into(),
                    format!("{fps}"),
                    "-t".into(),
                    format!("{source:.6}"),
                    "-i".into(),
                    media.path.display().to_string(),
                ]);
            } else {
                args.extend([
                    "-ss".into(),
                    format!("{:.6}", clip.start),
                    "-t".into(),
                    format!("{source:.6}"),
                    "-i".into(),
                    media.path.display().to_string(),
                ]);
            }
            input += 1;
        }
        // Picture.
        if want_video {
            if media.has_video {
                let mut chain = format!("[{index}:v]setpts=PTS-STARTPTS");
                if (clip.speed - 1.0).abs() > 1e-6 {
                    let _ = write!(chain, ",setpts=PTS/{:.6}", clip.speed);
                }
                if !media.is_card() {
                    video_effects(&mut chain, clip, w, h, fps);
                } else {
                    chain.push_str(",format=yuv420p,settb=AVTB");
                }
                hide(&mut chain, &clip.effects, i, w, h, length);
                zoom(&mut chain, &clip.effects, w, h, fps, length);
                // Exactly the clip's length, so joins and fades land where the
                // timeline shows them (a file can end a frame early).
                let _ = write!(
                    chain,
                    ",tpad=stop_mode=clone:stop_duration=1,trim=duration={length:.6},setpts=PTS-STARTPTS"
                );
                chain.push_str(&title(&clip.effects, settings, lift, temp, w, h, length)?);
                fades(&mut chain, clip, length, "fade");
                // setpts leaves the rate unknown; xfade needs it constant.
                let _ = write!(chain, ",fps={fps},settb=AVTB[v{i}];");
                graph.push_str(&chain);
            } else {
                let _ = write!(
                    graph,
                    "color=c=black:s={w}x{h}:r={fps}:d={length:.6},format=yuv420p,fps={fps},settb=AVTB[v{i}];"
                );
            }
        }
        // Sound.
        if want_audio {
            if sound {
                let tempo = atempo(clip.speed);
                let prep = |track: usize| {
                    let mut c = format!("[{index}:a:{track}]asetpts=PTS-STARTPTS");
                    if !tempo.is_empty() {
                        let _ = write!(c, ",{tempo}");
                    }
                    c.push_str(",aformat=sample_rates=48000:channel_layouts=stereo");
                    c
                };
                let voice = voice_chain(clip.voice);
                let split = media
                    .split_tracks()
                    .filter(|_| clip.voice > 0 || (clip.desktop_volume - 1.0).abs() > 1e-6);
                let mut chain = match split {
                    Some((mic, desk)) => {
                        let mut m = prep(mic);
                        if !voice.is_empty() {
                            let _ = write!(m, ",{voice}");
                        }
                        let _ = write!(
                            graph,
                            "{m}[am{i}];{},volume={:.4}[ad{i}];",
                            prep(desk),
                            clip.desktop_volume
                        );
                        format!("[am{i}][ad{i}]amix=inputs=2:normalize=0:duration=longest")
                    }
                    None => {
                        let mut c = prep(0);
                        if !voice.is_empty() {
                            let _ = write!(c, ",{voice}");
                        }
                        c
                    }
                };
                if (clip.volume - 1.0).abs() > 1e-6 {
                    let _ = write!(chain, ",volume={:.4}", clip.volume);
                }
                let _ = write!(chain, ",apad,atrim=duration={length:.6}");
                fades(&mut chain, clip, length, "afade");
                let _ = write!(chain, "[a{i}];");
                graph.push_str(&chain);
            } else {
                let _ = write!(
                    graph,
                    "anullsrc=r=48000:cl=stereo,atrim=duration={length:.6}[a{i}];"
                );
            }
        }
    }
    // Join.
    let mut v = "v0".to_string();
    let mut a = "a0".to_string();
    let mut total = lengths[0];
    for i in 1..project.clips.len() {
        let overlap = project.overlap(i);
        let (nv, na) = (format!("j{i}v"), format!("j{i}a"));
        match project.clips[i].transition {
            Some(t) if overlap > 0.0 => {
                let offset = (total - overlap).max(0.0);
                if want_video {
                    let _ = write!(
                        graph,
                        "[{v}][v{i}]xfade=transition={}:duration={overlap:.6}:offset={offset:.6}[{nv}];",
                        t.xfade()
                    );
                }
                if want_audio {
                    let _ = write!(graph, "[{a}][a{i}]acrossfade=d={overlap:.6}[{na}];");
                }
                total += lengths[i] - overlap;
            }
            _ => {
                match (want_video, want_audio) {
                    (true, true) => {
                        let _ = write!(
                            graph,
                            "[{v}][{a}][v{i}][a{i}]concat=n=2:v=1:a=1[{nv}][{na}];"
                        );
                    }
                    (true, false) => {
                        let _ = write!(graph, "[{v}][v{i}]concat=n=2:v=1:a=0[{nv}];");
                    }
                    (false, true) => {
                        let _ = write!(graph, "[{a}][a{i}]concat=n=2:v=0:a=1[{na}];");
                    }
                    (false, false) => {}
                }
                total += lengths[i];
            }
        }
        v = nv;
        a = na;
    }
    // Music, lowered while the film speaks.
    if music && want_audio && !project.music.is_empty() {
        let mut lanes = String::new();
        for (j, m) in project.music.iter().enumerate() {
            let media = &project.media[m.media];
            args.extend([
                "-ss".into(),
                format!("{:.6}", m.start),
                "-t".into(),
                format!("{:.6}", m.length()),
                "-i".into(),
                media.path.display().to_string(),
            ]);
            let delay = (m.at * 1000.0).round() as u64;
            let mut chain = format!(
                "[{input}:a:0]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume={:.4}",
                m.volume
            );
            if m.fade_in > 0.0 {
                let _ = write!(chain, ",afade=t=in:st=0:d={:.3}", m.fade_in);
            }
            if m.fade_out > 0.0 {
                let _ = write!(
                    chain,
                    ",afade=t=out:st={:.3}:d={:.3}",
                    (m.length() - m.fade_out).max(0.0),
                    m.fade_out
                );
            }
            let _ = write!(chain, ",adelay={delay}|{delay}[m{j}];");
            graph.push_str(&chain);
            let _ = write!(lanes, "[m{j}]");
            input += 1;
        }
        let n = project.music.len();
        let mus = if n > 1 {
            let _ = write!(
                graph,
                "{lanes}amix=inputs={n}:duration=longest:normalize=0:dropout_transition=0[mus];"
            );
            "mus".to_string()
        } else {
            "m0".to_string()
        };
        let mus = if project.duck > 0 {
            let (threshold, ratio) = if project.duck == 1 {
                (0.03, 4)
            } else {
                (0.015, 10)
            };
            let _ = write!(
                graph,
                "[{a}]asplit=2[film][key];[{mus}][key]sidechaincompress=threshold={threshold}:ratio={ratio}:attack=30:release=600:makeup=1[ducked];"
            );
            a = "film".into();
            "ducked".to_string()
        } else {
            mus
        };
        let _ = write!(
            graph,
            "[{a}][{mus}]amix=inputs=2:duration=first:normalize=0:dropout_transition=0[mixed];"
        );
        a = "mixed".into();
    }
    // The level of the finished film.
    let target = LOUDNESS[project.loudness.min(3) as usize].1;
    if want_audio && film_only && project.loudness > 0 {
        let _ = write!(
            graph,
            "[{a}]loudnorm=I={target}:TP=-1.5:LRA=11{MEASURED},aresample=48000,alimiter=limit=0.89:level=0[lvl];"
        );
        a = "lvl".into();
    }
    if *purpose == Purpose::Measure {
        let target = if project.loudness > 0 { target } else { -16.0 };
        let _ = write!(
            graph,
            "[{a}]loudnorm=I={target}:TP=-1.5:LRA=11:print_format=json[lvl];"
        );
        a = "lvl".into();
    }
    if let Purpose::Sound { tail, .. } = purpose {
        if !tail.is_empty() {
            let _ = write!(graph, "[{a}]{tail}[snd];");
            a = "snd".into();
        }
    }
    // Over the whole picture: a logo and burnt-in captions.
    if want_video {
        if let Some(logo) = project.logo.as_ref().filter(|l| l.path.is_file()) {
            args.extend(["-i".into(), logo.path.display().to_string()]);
            let lw = even(f64::from(w) * logo.size.clamp(2.0, 50.0) / 100.0);
            let margin = (f64::from(h) * 0.035).round() as u32;
            let (x, y) = match logo.corner {
                0 => (format!("{margin}"), format!("{margin}")),
                1 => (format!("W-w-{margin}"), format!("{margin}")),
                2 => (format!("{margin}"), format!("H-h-{margin}")),
                _ => (format!("W-w-{margin}"), format!("H-h-{margin}")),
            };
            let _ = write!(
                graph,
                "[{input}:v]scale=w={lw}:h=-2,format=rgba,colorchannelmixer=aa={:.3}[logo];\
                 [{v}][logo]overlay=x={x}:y={y}:format=auto,format=yuv420p[lv];",
                logo.opacity.clamp(0.05, 1.0)
            );
            input += 1;
            v = "lv".into();
        }
        if settings.captions == 1 {
            let lines = caption_filters(project, settings, temp, h)?;
            if !lines.is_empty() {
                let _ = write!(graph, "[{v}]null{lines}[cv];");
                v = "cv".into();
            }
        }
    }
    // Subtitles as a track and a file.
    let mut subtitle_input = None;
    if film_only && settings.captions == 2 && project.caption_count() > 0 {
        let srt = settings.output.with_extension("srt");
        std::fs::write(&srt, srt_text(&project.film_captions()))
            .map_err(|e| format!("could not write {}: {e}", srt.display()))?;
        extra.push(srt.clone());
        if matches!(
            format,
            Format::Mp4 | Format::Mp4Small | Format::Mkv | Format::WebM
        ) {
            args.extend(["-i".into(), srt.display().to_string()]);
            subtitle_input = Some(input);
        }
    }
    // Output stage.
    let vout = if format == Format::Gif && want_video {
        let _ = write!(
            graph,
            "[{v}]split[g1][g2];[g1]palettegen=stats_mode=diff[pal];[g2][pal]paletteuse=dither=bayer:bayer_scale=4[gif];"
        );
        "gif".to_string()
    } else {
        v
    };
    let graph = graph.trim_end_matches(';').to_string();
    args.extend(["-filter_complex".into(), graph]);
    if want_video {
        args.extend(["-map".into(), format!("[{vout}]")]);
    }
    if want_audio {
        args.extend(["-map".into(), format!("[{a}]")]);
    }
    match purpose {
        Purpose::Film => {
            if let Some(s) = subtitle_input {
                let codec = match format {
                    Format::Mkv => "srt",
                    Format::WebM => "webvtt",
                    _ => "mov_text",
                };
                args.extend(["-map".into(), format!("{s}:s"), "-c:s".into(), codec.into()]);
            }
            codec(&mut args, format, settings.draft);
            args.extend(["-t".into(), format!("{total:.6}")]);
            args.push(settings.output.display().to_string());
        }
        Purpose::Measure => {
            args.extend([
                "-t".into(),
                format!("{total:.6}"),
                "-f".into(),
                "null".into(),
                "-".into(),
            ]);
        }
        Purpose::Sound { out, .. } => {
            args.extend(["-t".into(), format!("{total:.6}")]);
            args.extend(out.iter().cloned());
        }
    }
    Ok((args, total))
}

fn video_effects(chain: &mut String, clip: &Clip, w: u32, h: u32, fps: f64) {
    let e = &clip.effects;
    match e.rotate % 4 {
        1 => chain.push_str(",transpose=1"),
        2 => chain.push_str(",hflip,vflip"),
        3 => chain.push_str(",transpose=2"),
        _ => {}
    }
    if e.flip {
        chain.push_str(",hflip");
    }
    if e.has_crop() {
        let [l, t, r, b] = e.crop.map(|c| c.clamp(0.0, 45.0) / 100.0);
        let _ = write!(
            chain,
            ",crop=w=iw*{:.4}:h=ih*{:.4}:x=iw*{l:.4}:y=ih*{t:.4}",
            (1.0 - l - r).max(0.1),
            (1.0 - t - b).max(0.1)
        );
    }
    if e.has_colour() {
        let _ = write!(
            chain,
            ",eq=brightness={:.3}:contrast={:.3}:saturation={:.3}",
            e.brightness.clamp(-1.0, 1.0),
            e.contrast.clamp(0.0, 3.0),
            e.saturation.clamp(0.0, 3.0)
        );
    }
    if e.blur > 0 {
        let _ = write!(chain, ",boxblur={}:1", e.blur.min(40));
    }
    if e.sharpen > 0.0 {
        let _ = write!(chain, ",unsharp=5:5:{:.2}", e.sharpen.min(2.0));
    }
    // Colour and blur touch the picture only, not the bars around it.
    let _ = write!(
        chain,
        ",scale=w={w}:h={h}:force_original_aspect_ratio=decrease:flags=bicubic,\
         pad=w={w}:h={h}:x=(ow-iw)/2:y=(oh-ih)/2:color=black,setsar=1,fps={fps}"
    );
    chain.push_str(",format=yuv420p,settb=AVTB");
}

/// The picture chain of a clip up to its frame (rotate, crop, colour,
/// scale), for a still of what the clip looks like.
pub fn frame_chain(clip: &Clip, w: u32, h: u32) -> String {
    let mut c = String::from("null");
    video_effects(&mut c, clip, w, h, 30.0);
    c
}

/// A share of the frame in even pixels: (x, y, w, h).
fn area_px(area: [f64; 4], w: u32, h: u32) -> (u32, u32, u32, u32) {
    let px = |v: f64, of: u32| ((v / 100.0 * f64::from(of)) / 2.0).round() as u32 * 2;
    let x = px(area[0], w).min(w.saturating_sub(4));
    let y = px(area[1], h).min(h.saturating_sub(4));
    let aw = px(area[2], w).clamp(4, w - x);
    let ah = px(area[3], h).clamp(4, h - y);
    (x, y, aw, ah)
}

/// Blur, pixelate or black out part of the picture for a while.
fn hide(chain: &mut String, e: &Effects, i: usize, w: u32, h: u32, length: f64) {
    if !e.has_hide() {
        return;
    }
    let (x, y, aw, ah) = area_px(e.hide_area, w, h);
    let (a, b) = window(e.hide_start, e.hide_length, length);
    if b - a < 0.01 {
        return;
    }
    let enable = format!("enable='between(t,{a:.3},{b:.3})'");
    match e.hide {
        3 => {
            let _ = write!(
                chain,
                ",drawbox=x={x}:y={y}:w={aw}:h={ah}:color=black:t=fill:{enable}"
            );
        }
        kind => {
            let treat = if kind == 2 {
                let bw = (aw / 14).max(1);
                let bh = (ah / 14).max(1);
                format!("scale={bw}:{bh}:flags=area,scale={aw}:{ah}:flags=neighbor")
            } else {
                let sigma = (f64::from(aw.min(ah)) / 8.0).clamp(4.0, 60.0);
                format!("gblur=sigma={sigma:.1}:steps=3")
            };
            let _ = write!(
                chain,
                ",split[h{i}a][h{i}b];[h{i}b]crop={aw}:{ah}:{x}:{y},{treat}[h{i}c];\
                 [h{i}a][h{i}c]overlay=x={x}:y={y}:{enable}"
            );
        }
    }
}

/// Glide into part of the picture and back out.
fn zoom(chain: &mut String, e: &Effects, w: u32, h: u32, fps: f64, length: f64) {
    if !e.has_zoom() {
        return;
    }
    let (ts, te) = window(e.zoom_start, e.zoom_length, length);
    if te - ts < 0.05 {
        return;
    }
    // The area grown to the frame's shape, kept inside the frame.
    let (fw, fh) = (f64::from(w), f64::from(h));
    let mut aw = e.zoom_area[2] / 100.0 * fw;
    let mut ah = e.zoom_area[3] / 100.0 * fh;
    let cx = (e.zoom_area[0] / 100.0 * fw) + aw / 2.0;
    let cy = (e.zoom_area[1] / 100.0 * fh) + ah / 2.0;
    if aw / ah > fw / fh {
        ah = aw * fh / fw;
    } else {
        aw = ah * fw / fh;
    }
    aw = aw.clamp(fw / 8.0, fw);
    ah = ah.clamp(fh / 8.0, fh);
    let left = (cx - aw / 2.0).clamp(0.0, fw - aw) / fw;
    let top = (cy - ah / 2.0).clamp(0.0, fh - ah) / fh;
    let share = aw / fw;
    let ramp = e.zoom_ramp.clamp(0.05, 5.0).min((te - ts) / 2.0).max(0.01);
    // p: 0 outside, 1 zoomed; smoothstep for a soft start and stop.
    let p = format!("(clip((it-{ts:.3})/{ramp:.3},0,1)*clip(({te:.3}-it)/{ramp:.3},0,1))");
    let s = format!("({p}*{p}*(3-2*{p}))");
    let _ = write!(
        chain,
        // zoompan's own timestamps never end (trim cannot stop it, and a
        // GIF palette then waits for ever): number the frames again.
        ",zoompan=z='1/(1-{:.5}*{s})':x='iw*{left:.5}*{s}':y='ih*{top:.5}*{s}':d=1:s={w}x{h}:fps={fps},setpts=N/({fps}*TB),setsar=1,format=yuv420p,settb=AVTB",
        1.0 - share
    );
}

/// Positions of a block of lines in one of nine places.
fn place(position: u8, w: u32, h: u32, lines: usize, line_h: f64) -> (String, f64) {
    let (col, row) = (position % 3, position / 3);
    let mx = (f64::from(w) * 0.05).round();
    let my = f64::from(h) * 0.07;
    let block = lines as f64 * line_h;
    let y0 = match row {
        0 => my,
        1 => (f64::from(h) - block) / 2.0,
        _ => f64::from(h) - my - block,
    };
    let x = match col {
        0 => format!("{mx}"),
        1 => "(w-text_w)/2".to_string(),
        _ => format!("w-text_w-{mx}"),
    };
    (x, y0)
}

fn look(look: u8, size: f64) -> String {
    match look {
        0 => format!(
            ":box=1:boxcolor=black@0.5:boxborderw={}",
            (size * 0.28).round().max(2.0)
        ),
        1 => format!(
            ":borderw={}:bordercolor=black@0.9",
            (size / 14.0).round().max(1.0)
        ),
        2 => {
            let d = (size / 16.0).round().max(1.0);
            format!(":shadowx={d}:shadowy={d}:shadowcolor=black@0.7")
        }
        _ => String::new(),
    }
}

fn text_file(work: &Path, temp: &mut Vec<PathBuf>, text: &str) -> Result<PathBuf, String> {
    let file = work.join(format!("text-{}-{}.txt", std::process::id(), temp.len()));
    std::fs::write(&file, text).map_err(|e| format!("could not write a title: {e}"))?;
    temp.push(file.clone());
    Ok(file)
}

fn font_for(e: &Effects, settings: &Settings) -> String {
    let file = if e.font.is_empty() && e.font_style == 0 {
        settings.font.clone()
    } else {
        super::fonts::file(&e.font, e.font_style)
            // A font for another script: the usual one in that style.
            .filter(|f| super::fonts::covers(f, &e.text))
            .or_else(|| super::fonts::file("", e.font_style))
            .or_else(|| settings.font.clone())
    };
    file.map(|f| format!(":fontfile={}", quote(&f)))
        .unwrap_or_default()
}

/// The title over a clip: one drawtext a line, in its place, font, colour
/// and look, shown for its window and faded.
fn title(
    e: &Effects,
    settings: &Settings,
    lift: f64,
    temp: &mut Vec<PathBuf>,
    w: u32,
    h: u32,
    length: f64,
) -> Result<String, String> {
    let text = e.text.trim_end();
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    let lines: Vec<&str> = text.lines().collect();
    let size = (f64::from(h) * e.text_size.clamp(2.0, 30.0) / 100.0).round();
    let line_h = (size * 1.3).round();
    let (x, mut y0) = place(e.text_position.min(8), w, h, lines.len(), line_h);
    if e.text_position >= 6 {
        y0 -= lift;
    }
    let (a, b) = window(e.text_start, e.text_length, length);
    let font = font_for(e, settings);
    let style = look(e.text_look, size);
    let mut timing = String::new();
    if a > 0.001 || b < length - 0.001 {
        let _ = write!(timing, ":enable='between(t,{a:.3},{b:.3})'");
    }
    let fade = e.text_fade.min((b - a) / 2.0);
    if fade > 0.01 {
        let _ = write!(
            timing,
            ":alpha='clip(min((t-{a:.3})/{fade:.3},({b:.3}-t)/{fade:.3}),0,1)'"
        );
    }
    let mut out = String::new();
    for (k, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let file = text_file(&settings.work, temp, line)?;
        let _ = write!(
            out,
            ",drawtext=textfile={}{font}:expansion=none:fontsize={size}:fontcolor=0x{:06x}{style}:x={x}:y={:.0}{timing}",
            quote(&file),
            e.text_colour & 0xffffff,
            y0 + k as f64 * line_h
        );
    }
    Ok(out)
}

/// Lines of at most `width` characters, broken at spaces.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn caption_filters(
    project: &Project,
    settings: &Settings,
    temp: &mut Vec<PathBuf>,
    h: u32,
) -> Result<String, String> {
    let share = [4.2, 5.2, 6.5][project.caption_size.min(2) as usize];
    let size = (f64::from(h) * share / 100.0).round();
    let line_h = (size * 1.3).round();
    let font = settings
        .font
        .as_ref()
        .map(|f| format!(":fontfile={}", quote(f)))
        .unwrap_or_default();
    let style = look(0, size);
    let mut out = String::new();
    for (a, b, text) in project.film_captions() {
        let lines = wrap(&text, 42);
        let n = lines.len();
        for (k, line) in lines.iter().enumerate() {
            let file = text_file(&settings.work, temp, line)?;
            let y = f64::from(h) * 0.93 - (n - k) as f64 * line_h;
            let _ = write!(
                out,
                ",drawtext=textfile={}{font}:expansion=none:fontsize={size}:fontcolor=white{style}:x=(w-text_w)/2:y={y:.0}:enable='between(t,{a:.3},{b:.3})'",
                quote(&file)
            );
        }
    }
    Ok(out)
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

pub fn srt_text(captions: &[(f64, f64, String)]) -> String {
    let mut out = String::new();
    for (k, (a, b, text)) in captions.iter().enumerate() {
        let _ = write!(
            out,
            "{}\n{} --> {}\n{}\n\n",
            k + 1,
            srt_time(*a),
            srt_time(*b),
            wrap(text, 42).join("\n")
        );
    }
    out
}

fn fades(chain: &mut String, clip: &Clip, length: f64, filter: &str) {
    let fin = clip.fade_in.min(length * 0.5);
    let fout = clip.fade_out.min(length * 0.5);
    if fin > 0.0 {
        let _ = write!(chain, ",{filter}=t=in:st=0:d={fin:.3}");
    }
    if fout > 0.0 {
        let _ = write!(
            chain,
            ",{filter}=t=out:st={:.3}:d={fout:.3}",
            (length - fout).max(0.0)
        );
    }
}

fn codec(args: &mut Vec<String>, format: Format, draft: bool) {
    let push = |args: &mut Vec<String>, list: &[&str]| {
        args.extend(list.iter().map(|s| s.to_string()));
    };
    if draft {
        push(
            args,
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "26",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
            ],
        );
        return;
    }
    match format {
        Format::Mp4 => push(
            args,
            &[
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-crf",
                "20",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-movflags",
                "+faststart",
            ],
        ),
        Format::Mp4Small => push(
            args,
            &[
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "27",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "112k",
                "-movflags",
                "+faststart",
            ],
        ),
        Format::WebM => push(
            args,
            &[
                "-c:v",
                "libvpx-vp9",
                "-crf",
                "32",
                "-b:v",
                "0",
                "-row-mt",
                "1",
                "-deadline",
                "good",
                "-cpu-used",
                "4",
                "-c:a",
                "libopus",
                "-b:a",
                "128k",
            ],
        ),
        Format::Mkv => push(
            args,
            &[
                "-c:v", "libx264", "-preset", "slow", "-crf", "17", "-pix_fmt", "yuv420p", "-c:a",
                "flac",
            ],
        ),
        Format::Gif => push(args, &["-loop", "0"]),
        Format::Mp3 => push(args, &["-c:a", "libmp3lame", "-q:a", "2"]),
        Format::Wav => push(args, &["-c:a", "pcm_s16le"]),
        Format::Flac => push(args, &["-c:a", "flac"]),
        Format::Opus => push(args, &["-c:a", "libopus", "-b:a", "96k"]),
    }
}

/// A free name in `folder`: `base.ext`, then `base-2.ext` and so on.
pub fn free_name(folder: &Path, base: &str, ext: &str) -> PathBuf {
    let first = folder.join(format!("{base}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| folder.join(format!("{base}-{n}.{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::super::project::{Logo, Media, Transition};
    use super::*;

    fn project() -> Project {
        let mut p = Project {
            loudness: 0,
            duck: 0,
            ..Project::default()
        };
        for (i, l) in [10.0, 6.0].iter().enumerate() {
            let m = p.add_media(Media {
                path: PathBuf::from(format!("/v/{i}.mkv")),
                duration: *l,
                width: 2560,
                height: 1080,
                fps: 30.0,
                has_video: true,
                has_audio: true,
                still: false,
                ..Media::default()
            });
            p.append(m, None);
        }
        p
    }

    fn settings(format: Format) -> Settings {
        Settings::new(format, PathBuf::from("/out/film.mp4"), std::env::temp_dir())
    }

    fn graph(args: &[String]) -> &str {
        let i = args.iter().position(|a| a == "-filter_complex").unwrap();
        &args[i + 1]
    }

    #[test]
    fn two_clips_are_concatenated_and_seeked_on_the_input() {
        let mut p = project();
        p.clips[0].start = 2.0;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        assert!((plan.duration - 14.0).abs() < 1e-6);
        assert_eq!(plan.steps.len(), 1);
        let joined = plan.main().join(" ");
        assert!(joined.contains("-ss 2.000000 -t 8.000000 -i /v/0.mkv"));
        assert!(graph(plan.main()).contains("concat=n=2:v=1:a=1[j1v][j1a]"));
        assert!(joined.contains("-map [j1v] -map [j1a]"));
        assert!(joined.ends_with("/out/film.mp4"));
    }

    #[test]
    fn a_transition_overlaps_the_clips() {
        let mut p = project();
        p.clips[1].transition = Some(Transition::WipeLeft);
        p.clips[1].transition_length = 1.0;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        assert!((plan.duration - 15.0).abs() < 1e-6);
        let g = graph(plan.main());
        assert!(g.contains("xfade=transition=wipeleft:duration=1.000000:offset=9.000000"));
        assert!(g.contains("acrossfade=d=1.000000"));
    }

    #[test]
    fn speed_mute_and_effects_reach_the_filters() {
        let mut p = project();
        p.clips[0].speed = 3.0;
        p.clips[0].effects.saturation = 0.0;
        p.clips[0].effects.blur = 4;
        p.clips[1].mute = true;
        p.clips[1].fade_out = 1.0;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        let g = graph(plan.main());
        assert!(g.contains("setpts=PTS/3.000000"));
        assert!(g.contains("atempo=2,atempo=1.5"));
        assert!(g.contains("eq=brightness=0.000:contrast=1.000:saturation=0.000"));
        assert!(g.contains("boxblur=4:1"));
        assert!(g.contains("anullsrc=r=48000:cl=stereo,atrim=duration=6.000000[a1]"));
        assert!(g.contains("fade=t=out:st=5.000:d=1.000"));
    }

    #[test]
    fn small_and_gif_exports_shrink_the_picture() {
        let p = project();
        assert_eq!(frame(&p, 0, Format::Mp4), (2560, 1080, 30.0));
        assert_eq!(frame(&p, 0, Format::Mp4Small).1, 1080);
        assert_eq!(frame(&p, 720, Format::Mp4), (1706, 720, 30.0));
        let (_, h, fps) = frame(&p, 0, Format::Gif);
        assert_eq!((h, fps), (480, 12.0));
        let plan = plan(&p, &settings(Format::Gif)).unwrap();
        let g = graph(plan.main());
        assert!(g.contains("palettegen"));
        assert!(!g.contains("[a0]"), "a GIF has no sound chains: {g}");
        assert!(!plan.main().join(" ").contains("-map [j1a]"));
    }

    #[test]
    fn a_sound_only_export_decodes_no_picture() {
        let p = project();
        let plan = plan(&p, &settings(Format::Mp3)).unwrap();
        let joined = plan.main().join(" ");
        assert!(!joined.contains("-map [j1v]"));
        assert!(joined.contains("-map [j1a]"));
        assert!(joined.contains("libmp3lame"));
        assert!(!graph(plan.main()).contains(":v]"));
        assert!(graph(plan.main()).contains("concat=n=2:v=0:a=1[j1a]"));
    }

    #[test]
    fn a_range_renders_only_that_part() {
        let p = project();
        let mut s = settings(Format::Mp4);
        s.range = Some((8.0, 12.0));
        let plan = plan(&p, &s).unwrap();
        assert!((plan.duration - 4.0).abs() < 1e-6);
    }

    #[test]
    fn atempo_chains_stay_in_range() {
        assert_eq!(atempo(1.0), "");
        assert_eq!(atempo(0.25), "atempo=0.5,atempo=0.5");
        assert_eq!(atempo(4.0), "atempo=2,atempo=2");
    }

    #[test]
    fn levelling_measures_first_and_a_preview_does_not() {
        let mut p = project();
        p.loudness = 1;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps[0].measure);
        let measure = plan.steps[0].args.join(" ");
        assert!(measure.contains("print_format=json"));
        assert!(measure.ends_with("-f null -"));
        assert!(!graph(&plan.steps[0].args).contains(":v]"));
        assert!(graph(plan.main()).contains("loudnorm=I=-16:TP=-1.5:LRA=11@MEASURED@"));
        let mut s = settings(Format::Mp4);
        s.draft = true;
        let preview = super::plan(&p, &s).unwrap();
        assert_eq!(preview.steps.len(), 1);
        assert!(!preview.main().join(" ").contains(MEASURED));
    }

    #[test]
    fn the_measurement_becomes_loudnorm_options() {
        let log = r#"[Parsed_loudnorm_0 @ 0x1]
{
	"input_i" : "-27.61",
	"input_tp" : "-4.47",
	"input_lra" : "18.06",
	"input_thresh" : "-39.20",
	"output_i" : "-16.58",
	"target_offset" : "0.58"
}"#;
        let o = measured_options(log).unwrap();
        assert_eq!(o, ":measured_I=-27.61:measured_TP=-4.47:measured_LRA=18.06:measured_thresh=-39.20:offset=0.58:linear=true");
        assert!(measured_options("no json").is_none());
        let silent = log.replace("-27.61", "-inf");
        assert!(measured_options(&silent).is_none());
    }

    #[test]
    fn voice_and_separate_tracks_treat_the_microphone_only() {
        let mut p = project();
        p.media[0].tracks = vec!["Mix".into(), "Mic".into(), "Desktop".into()];
        p.clips[0].voice = 1;
        p.clips[0].desktop_volume = 0.5;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        let g = graph(plan.main());
        assert!(g.contains("[0:a:1]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,highpass=f=80"));
        assert!(g.contains("[0:a:2]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume=0.5000[ad0]"));
        assert!(g.contains("[am0][ad0]amix=inputs=2"));
        // Without a treatment the mix track plays as recorded.
        assert!(g.contains("[1:a:0]asetpts"));
    }

    #[test]
    fn music_ducks_under_the_film() {
        let mut p = project();
        let m = p.add_media(Media {
            path: PathBuf::from("/m/song.mp3"),
            duration: 100.0,
            has_audio: true,
            ..Media::default()
        });
        p.music.push(super::super::project::AudioClip {
            media: m,
            at: 1.0,
            start: 0.0,
            end: 30.0,
            volume: 0.5,
            fade_in: 0.0,
            fade_out: 0.0,
        });
        p.duck = 2;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        let g = graph(plan.main());
        assert!(g.contains(
            "[j1a]asplit=2[film][key];[m0][key]sidechaincompress=threshold=0.015:ratio=10"
        ));
        assert!(g.contains("[film][ducked]amix=inputs=2:duration=first"));
    }

    #[test]
    fn titles_hide_zoom_logo_and_cards_reach_the_picture() {
        let mut p = project();
        let e = &mut p.clips[0].effects;
        e.text = "Line one\nLine two".into();
        e.text_position = 6;
        e.text_colour = 0xffd84d;
        e.text_look = 1;
        e.text_start = 1.0;
        e.text_length = 3.0;
        e.text_fade = 0.5;
        e.hide = 1;
        e.hide_area = [10.0, 10.0, 20.0, 20.0];
        e.zoom_area = [50.0, 50.0, 25.0, 25.0];
        e.zoom_start = 2.0;
        e.zoom_length = 4.0;
        let card = p.card();
        p.clips.push(Clip::new(card, 0.0, 3.0));
        p.clips[2].effects.card_colour = 0x223344;
        let logo = std::env::temp_dir().join(format!("niruclip-logo-{}.png", std::process::id()));
        std::fs::write(&logo, b"png").unwrap();
        p.logo = Some(Logo::new(logo.clone()));
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        let g = graph(plan.main()).to_string();
        let joined = plan.main().join(" ");
        assert_eq!(g.matches("drawtext=").count(), 2);
        assert!(g.contains("fontcolor=0xffd84d:borderw="));
        assert!(g.contains(":x=128:y=834"), "{g}");
        assert!(g.contains("enable='between(t,1.000,4.000)'"));
        assert!(g.contains("alpha='clip(min((t-1.000)/0.500,(4.000-t)/0.500),0,1)'"));
        assert!(g.contains("[h0b]crop=512:216:256:108,gblur"));
        assert!(g.contains("zoompan=z='1/(1-0.75000*"));
        assert!(joined.contains("-f lavfi -t 3.000000 -i color=c=0x223344:s=2560x1080:r=30"));
        assert!(g.contains("colorchannelmixer=aa=0.800[logo]"));
        assert!(g.contains("overlay=x=W-w-38:y=38"));
        let _ = std::fs::remove_file(&logo);
        for f in &plan.temp {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn a_size_limit_encodes_twice_and_shrinks_a_long_film() {
        let p = project();
        let mut s = settings(Format::Mp4);
        s.limit_mb = 10;
        let plan = plan(&p, &s).unwrap();
        assert_eq!(plan.steps.len(), 2);
        let first = plan.steps[0].args.join(" ");
        let second = plan.main().join(" ");
        assert!(first.contains("-pass 1") && first.ends_with("-f null -"));
        assert!(second.contains("-pass 2") && second.ends_with("/out/film.mp4"));
        assert!(!second.contains("-crf"));
        let (video, _) = fit_size(10, 16.0, 96_000.0, 2560, 1080, 30.0);
        assert!((video - (10.0 * 8.0e6 * 0.94 / 16.0 - 96_000.0)).abs() < 1.0);
        // Ten minutes in 10 MB is far too few bits for 1080 lines.
        let (_, h) = fit_size(10, 600.0, 96_000.0, 1920, 1080, 30.0);
        assert!(h <= 480, "{h}");
    }

    #[test]
    fn captions_burn_in_or_ride_along() {
        let mut p = project();
        p.set_film_captions(&[(
            1.0,
            3.0,
            "Hej och välkommen till filmen om redigering i NIRUCLIP".into(),
        )]);
        let mut s = settings(Format::Mp4);
        s.captions = 1;
        let plan = super::plan(&p, &s).unwrap();
        let g = graph(plan.main()).to_string();
        assert_eq!(g.matches("drawtext=").count(), 2, "wrapped onto two lines");
        assert!(g.contains("enable='between(t,1.000,3.000)'"));
        for f in &plan.temp {
            let _ = std::fs::remove_file(f);
        }
        let dir = std::env::temp_dir().join(format!("niruclip-srt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        s.captions = 2;
        s.output = dir.join("film.mp4");
        let plan = super::plan(&p, &s).unwrap();
        let joined = plan.main().join(" ");
        assert!(joined.contains("-c:s mov_text"));
        let srt = std::fs::read_to_string(dir.join("film.srt")).unwrap();
        assert!(srt.starts_with("1\n00:00:01,000 --> 00:00:03,000\nHej och"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sound_plan_skips_music_and_levelling() {
        let mut p = project();
        p.loudness = 1;
        let plan = sound_plan(
            &p,
            &std::env::temp_dir(),
            "silencedetect=n=-35dB:d=0.5",
            vec!["-f".into(), "null".into(), "-".into()],
        )
        .unwrap();
        let g = graph(plan.main());
        assert!(g.ends_with("[j1a]silencedetect=n=-35dB:d=0.5[snd]"));
        assert!(!g.contains("loudnorm"));
    }
}
