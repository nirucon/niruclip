//! Turn a project into one ffmpeg run.
//!
//! Every clip becomes its own input (seeked on the input side, so a long
//! recording is not decoded from the start), gets its own filter chain, and
//! the chains are joined with `concat` or `xfade`/`acrossfade`. Music is
//! delayed to its place and mixed under the film.
use super::project::{Clip, Project};
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

#[derive(Clone, Debug)]
pub struct Settings {
    pub format: Format,
    pub height: u32,
    pub output: PathBuf,
    /// A font file for titles.
    pub font: Option<PathBuf>,
    /// Folder for the title text files.
    pub work: PathBuf,
    /// Render only this film range (preview of a part); None is all.
    pub range: Option<(f64, f64)>,
    /// Fast, lower quality encoding (previews).
    pub draft: bool,
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

fn quote(path: &Path) -> String {
    // Filter option values are single-quoted; a quote inside cannot occur in
    // the paths we write ourselves, and is escaped for system font paths.
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

pub struct Plan {
    pub args: Vec<String>,
    pub duration: f64,
    /// Title text files to remove afterwards.
    pub temp: Vec<PathBuf>,
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

pub fn plan(project: &Project, settings: &Settings) -> Result<Plan, String> {
    let project = ranged(project, settings.range);
    if project.clips.is_empty() {
        return Err("the timeline is empty".into());
    }
    let format = settings.format;
    let (w, h, fps) = frame(&project, settings.height, format);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ];
    let mut graph = String::new();
    let mut temp = Vec::new();
    let mut input = 0usize;
    let mut lengths = Vec::new();
    for (i, clip) in project.clips.iter().enumerate() {
        let media = &project.media[clip.media];
        let length = clip.length();
        lengths.push(length);
        let source = clip.end - clip.start;
        let index = input;
        if media.still {
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
        // Picture.
        if media.has_video {
            let mut chain = format!("[{index}:v]setpts=PTS-STARTPTS");
            if (clip.speed - 1.0).abs() > 1e-6 {
                let _ = write!(chain, ",setpts=PTS/{:.6}", clip.speed);
            }
            video_effects(&mut chain, clip, w, h, fps);
            // Exactly the clip's length, so joins and fades land where the
            // timeline shows them (a file can end a frame early).
            let _ = write!(
                chain,
                ",tpad=stop_mode=clone:stop_duration=1,trim=duration={length:.6},setpts=PTS-STARTPTS"
            );
            if let Some(file) = title(clip, settings, i, &mut temp)? {
                chain.push_str(&file);
            }
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
        // Sound.
        if media.has_audio && !clip.mute && !media.still {
            let mut chain = format!("[{index}:a]asetpts=PTS-STARTPTS");
            let tempo = atempo(clip.speed);
            if !tempo.is_empty() {
                let _ = write!(chain, ",{tempo}");
            }
            if (clip.volume - 1.0).abs() > 1e-6 {
                let _ = write!(chain, ",volume={:.4}", clip.volume);
            }
            chain.push_str(",aformat=sample_rates=48000:channel_layouts=stereo");
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
                let _ = write!(
                    graph,
                    "[{v}][v{i}]xfade=transition={}:duration={overlap:.6}:offset={offset:.6}[{nv}];\
                     [{a}][a{i}]acrossfade=d={overlap:.6}[{na}];",
                    t.xfade()
                );
                total += lengths[i] - overlap;
            }
            _ => {
                let _ = write!(
                    graph,
                    "[{v}][{a}][v{i}][a{i}]concat=n=2:v=1:a=1[{nv}][{na}];"
                );
                total += lengths[i];
            }
        }
        v = nv;
        a = na;
    }
    // Music.
    if !project.music.is_empty() && format.has_audio() {
        let mut mix = format!("[{a}]");
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
                "[{input}:a]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume={:.4}",
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
            let _ = write!(mix, "[m{j}]");
            input += 1;
        }
        let _ = write!(
            graph,
            "{mix}amix=inputs={}:duration=first:normalize=0:dropout_transition=0[mixed];",
            project.music.len() + 1
        );
        a = "mixed".into();
    }
    // Output stage.
    let vout = if format == Format::Gif {
        let _ = write!(
            graph,
            "[{v}]split[g1][g2];[g1]palettegen=stats_mode=diff[pal];[g2][pal]paletteuse=dither=bayer:bayer_scale=4[gif];"
        );
        "gif".to_string()
    } else {
        v
    };
    // An output nobody maps must still end somewhere, or ffmpeg refuses
    // the graph.
    if !format.has_video() {
        let _ = write!(graph, "[{vout}]nullsink;");
    }
    if !format.has_audio() {
        let _ = write!(graph, "[{a}]anullsink;");
    }
    let graph = graph.trim_end_matches(';').to_string();
    args.extend(["-filter_complex".into(), graph]);
    if format.has_video() {
        args.extend(["-map".into(), format!("[{vout}]")]);
    }
    if format.has_audio() {
        args.extend(["-map".into(), format!("[{a}]")]);
    }
    codec(&mut args, format, settings.draft);
    args.extend(["-t".into(), format!("{total:.6}")]);
    args.push(settings.output.display().to_string());
    Ok(Plan {
        args,
        duration: total,
        temp,
    })
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

fn title(
    clip: &Clip,
    settings: &Settings,
    i: usize,
    temp: &mut Vec<PathBuf>,
) -> Result<Option<String>, String> {
    let text = clip.effects.text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let file = settings
        .work
        .join(format!("title-{}-{i}.txt", std::process::id()));
    std::fs::write(&file, text).map_err(|e| format!("could not write a title: {e}"))?;
    temp.push(file.clone());
    let size = clip.effects.text_size.clamp(2.0, 30.0) / 100.0;
    let y = match clip.effects.text_position {
        0 => "h*0.08",
        1 => "(h-text_h)/2",
        _ => "h-text_h-h*0.08",
    };
    let font = settings
        .font
        .as_ref()
        .map(|f| format!(":fontfile={}", quote(f)))
        .unwrap_or_default();
    Ok(Some(format!(
        ",drawtext=textfile={}{font}:fontsize=h*{size:.4}:fontcolor=white:\
         box=1:boxcolor=black@0.45:boxborderw=h*{:.4}:x=(w-text_w)/2:y={y}",
        quote(&file),
        size * 0.35
    )))
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
    use super::super::project::{Media, Transition};
    use super::*;

    fn project() -> Project {
        let mut p = Project::default();
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
            });
            p.append(m, None);
        }
        p
    }

    fn settings(format: Format) -> Settings {
        Settings {
            format,
            height: 0,
            output: PathBuf::from("/out/film.mp4"),
            font: None,
            work: std::env::temp_dir(),
            range: None,
            draft: false,
        }
    }

    fn graph(plan: &Plan) -> &str {
        let i = plan
            .args
            .iter()
            .position(|a| a == "-filter_complex")
            .unwrap();
        &plan.args[i + 1]
    }

    #[test]
    fn two_clips_are_concatenated_and_seeked_on_the_input() {
        let mut p = project();
        p.clips[0].start = 2.0;
        let plan = plan(&p, &settings(Format::Mp4)).unwrap();
        assert!((plan.duration - 14.0).abs() < 1e-6);
        let joined = plan.args.join(" ");
        assert!(joined.contains("-ss 2.000000 -t 8.000000 -i /v/0.mkv"));
        assert!(graph(&plan).contains("concat=n=2:v=1:a=1[j1v][j1a]"));
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
        let g = graph(&plan);
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
        let g = graph(&plan);
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
        assert!(graph(&plan).contains("palettegen"));
        assert!(graph(&plan).ends_with("[j1a]anullsink"));
        assert!(!plan.args.join(" ").contains("-map [j1a]"));
    }

    #[test]
    fn a_sound_only_export_maps_no_picture() {
        let p = project();
        let plan = plan(&p, &settings(Format::Mp3)).unwrap();
        let joined = plan.args.join(" ");
        assert!(!joined.contains("-map [j1v]"));
        assert!(joined.contains("-map [j1a]"));
        assert!(joined.contains("libmp3lame"));
        assert!(graph(&plan).ends_with("[j1v]nullsink"));
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
}
