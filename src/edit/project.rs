//! The edit decision list: what the user has done, never the pictures.
//!
//! A project is a list of media files and a sequence of clips cut from them,
//! plus an optional music lane. Source files are never written. Every edit
//! is a cheap change to this structure; the undo history keeps whole copies
//! (a project is a few kilobytes).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 2: nine title positions (was top, middle, bottom), styled titles, zoom,
/// hidden areas, voice sound, captions, logo and loudness.
/// 3: picture-in-picture (the recording's camera track or another file),
/// text effects, more looks and a free text position.
pub const SCHEMA: u32 = 3;

/// The path of the built-in title card "media": a plain colour.
pub const CARD: &str = "niruclip:card";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub path: PathBuf,
    /// Seconds. A still picture gets the length it is used for.
    pub duration: f64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub has_video: bool,
    #[serde(default)]
    pub has_audio: bool,
    /// A still picture (png, jpg, webp): shown for the clip's length.
    #[serde(default)]
    pub still: bool,
    /// Titles of the sound tracks, in order ("Mix", "Mic", "Desktop" in a
    /// NIRUCLIP recording with both sources).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<String>,
    /// Captions said in this file, in its own seconds: they follow every
    /// cut, move and speed change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captions: Vec<Caption>,
    /// A NIRUCLIP recording with the camera on a video track of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraTrack>,
}

/// The camera of a recording, on its own video track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraTrack {
    /// Which video stream of the file (0 is the screen).
    pub stream: usize,
    /// How it was placed when recorded (the recorder's NIRUCLIP_CAMERA tag).
    #[serde(default)]
    pub layout: String,
}

impl CameraTrack {
    pub fn layout(&self) -> crate::camera::TrackLayout {
        crate::camera::TrackLayout::parse(&self.layout)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl Media {
    pub fn card() -> Self {
        Media {
            path: PathBuf::from(CARD),
            duration: 4.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_video: true,
            has_audio: false,
            still: true,
            tracks: Vec::new(),
            captions: Vec::new(),
            camera: None,
        }
    }

    /// The camera track, when the screen picture does not have the camera
    /// drawn in already.
    pub fn own_camera(&self) -> Option<&CameraTrack> {
        self.camera.as_ref().filter(|c| !c.layout().burned)
    }

    pub fn is_card(&self) -> bool {
        self.path.as_os_str() == CARD
    }

    /// The microphone and desktop tracks of a recording with both, as
    /// audio stream indexes.
    pub fn split_tracks(&self) -> Option<(usize, usize)> {
        let find = |name: &str| {
            self.tracks
                .iter()
                .position(|t| t.eq_ignore_ascii_case(name))
        };
        Some((find("mic")?, find("desktop")?))
    }

    pub fn name(&self) -> String {
        if self.is_card() {
            return "Title card".into();
        }
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    Fade,
    Dissolve,
    FadeBlack,
    FadeWhite,
    WipeLeft,
    WipeRight,
    SlideLeft,
    SlideRight,
    CircleOpen,
    Pixelize,
}

impl Transition {
    pub const ALL: [Transition; 10] = [
        Transition::Fade,
        Transition::Dissolve,
        Transition::FadeBlack,
        Transition::FadeWhite,
        Transition::WipeLeft,
        Transition::WipeRight,
        Transition::SlideLeft,
        Transition::SlideRight,
        Transition::CircleOpen,
        Transition::Pixelize,
    ];

    /// The name of ffmpeg's xfade transition.
    pub fn xfade(self) -> &'static str {
        match self {
            Transition::Fade => "fade",
            Transition::Dissolve => "dissolve",
            Transition::FadeBlack => "fadeblack",
            Transition::FadeWhite => "fadewhite",
            Transition::WipeLeft => "wipeleft",
            Transition::WipeRight => "wiperight",
            Transition::SlideLeft => "slideleft",
            Transition::SlideRight => "slideright",
            Transition::CircleOpen => "circleopen",
            Transition::Pixelize => "pixelize",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Transition::Fade => "Cross fade",
            Transition::Dissolve => "Dissolve",
            Transition::FadeBlack => "Through black",
            Transition::FadeWhite => "Through white",
            Transition::WipeLeft => "Wipe left",
            Transition::WipeRight => "Wipe right",
            Transition::SlideLeft => "Slide left",
            Transition::SlideRight => "Slide right",
            Transition::CircleOpen => "Circle",
            Transition::Pixelize => "Pixelize",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    /// -1..1, 0 is unchanged.
    #[serde(default)]
    pub brightness: f64,
    /// 0..3, 1 is unchanged.
    #[serde(default = "one")]
    pub contrast: f64,
    /// 0..3, 1 is unchanged; 0 is black and white.
    #[serde(default = "one")]
    pub saturation: f64,
    /// Box blur radius in pixels, 0 is off.
    #[serde(default)]
    pub blur: u32,
    /// Sharpen 0..2, 0 is off.
    #[serde(default)]
    pub sharpen: f64,
    /// Quarter turns clockwise, 0..3.
    #[serde(default)]
    pub rotate: u8,
    #[serde(default)]
    pub flip: bool,
    /// Crop in percent of each edge (left, top, right, bottom).
    #[serde(default)]
    pub crop: [f64; 4],
    /// A text over the clip, empty is none.
    #[serde(default)]
    pub text: String,
    /// Nine places, row by row: 0 top left, 1 top, 2 top right, 3 left,
    /// 4 middle, 5 right, 6 bottom left, 7 bottom, 8 bottom right.
    #[serde(default = "seven")]
    pub text_position: u8,
    /// Height of the text as a share of the picture height, in percent.
    #[serde(default = "text_size")]
    pub text_size: f64,
    /// Font family (fontconfig name); empty is the desktop font.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub font: String,
    /// 0 regular, 1 bold, 2 italic, 3 bold italic.
    #[serde(default)]
    pub font_style: u8,
    /// 0xRRGGBB.
    #[serde(default = "white")]
    pub text_colour: u32,
    /// 0 box behind, 1 outline, 2 shadow, 3 plain.
    #[serde(default)]
    pub text_look: u8,
    /// Seconds into the clip the text shows from, and for how long (0: to
    /// the end of the clip).
    #[serde(default)]
    pub text_start: f64,
    #[serde(default)]
    pub text_length: f64,
    /// Fade the text in and out, seconds.
    #[serde(default)]
    pub text_fade: f64,
    /// Background colour of a title card, 0xRRGGBB.
    #[serde(default = "card_colour")]
    pub card_colour: u32,
    /// Zoom into this part of the picture, percent x, y, w, h of the frame;
    /// a width of 0 is no zoom.
    #[serde(default)]
    pub zoom_area: [f64; 4],
    #[serde(default)]
    pub zoom_start: f64,
    /// 0: to the end of the clip.
    #[serde(default)]
    pub zoom_length: f64,
    /// Seconds to glide in and out.
    #[serde(default = "zoom_ramp")]
    pub zoom_ramp: f64,
    /// Hide part of the picture: 0 off, 1 blur, 2 pixelate, 3 black box.
    #[serde(default)]
    pub hide: u8,
    /// Percent x, y, w, h of the frame.
    #[serde(default)]
    pub hide_area: [f64; 4],
    #[serde(default)]
    pub hide_start: f64,
    #[serde(default)]
    pub hide_length: f64,
    /// How the text comes and goes: 0 appears (or fades, with a fade time),
    /// 1 slides up, 2 slides in from the left, 3 pops, 4 types itself.
    #[serde(default)]
    pub text_anim: u8,
    /// The second colour of the lower third, neon and marker looks.
    #[serde(default = "accent")]
    pub text_accent: u32,
    /// With position 9 (free): the text's centre x and top y, percent.
    #[serde(default = "free_place")]
    pub text_xy: [f64; 2],
}

fn accent() -> u32 {
    0xffd84d
}
fn free_place() -> [f64; 2] {
    [50.0, 70.0]
}

fn white() -> u32 {
    0xffffff
}
fn card_colour() -> u32 {
    0x101014
}
fn zoom_ramp() -> f64 {
    0.6
}
fn seven() -> u8 {
    7
}

fn one() -> f64 {
    1.0
}
fn text_size() -> f64 {
    6.0
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            blur: 0,
            sharpen: 0.0,
            rotate: 0,
            flip: false,
            crop: [0.0; 4],
            text: String::new(),
            text_position: 7,
            text_size: 6.0,
            font: String::new(),
            font_style: 0,
            text_colour: 0xffffff,
            text_look: 0,
            text_start: 0.0,
            text_length: 0.0,
            text_fade: 0.0,
            card_colour: 0x101014,
            zoom_area: [0.0; 4],
            zoom_start: 0.0,
            zoom_length: 0.0,
            zoom_ramp: 0.6,
            hide: 0,
            hide_area: [0.0; 4],
            hide_start: 0.0,
            hide_length: 0.0,
            text_anim: 0,
            text_accent: 0xffd84d,
            text_xy: [50.0, 70.0],
        }
    }
}

impl Effects {
    pub fn is_default(&self) -> bool {
        *self == Effects::default()
    }

    pub fn has_colour(&self) -> bool {
        self.brightness.abs() > 1e-6
            || (self.contrast - 1.0).abs() > 1e-6
            || (self.saturation - 1.0).abs() > 1e-6
    }

    pub fn has_crop(&self) -> bool {
        self.crop.iter().any(|c| *c > 1e-6)
    }

    pub fn has_zoom(&self) -> bool {
        self.zoom_area[2] > 0.5 && self.zoom_area[3] > 0.5
    }

    pub fn has_hide(&self) -> bool {
        self.hide > 0 && self.hide_area[2] > 0.5 && self.hide_area[3] > 0.5
    }
}

/// When something in a clip shows: from `start` for `length` seconds (0 is
/// to the end), within a clip `clip` seconds long.
pub fn window(start: f64, length: f64, clip: f64) -> (f64, f64) {
    let a = start.clamp(0.0, clip);
    let b = if length > 0.0 {
        (a + length).min(clip)
    } else {
        clip
    };
    (a, b.max(a))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Index into `Project::media`.
    pub media: usize,
    /// Source seconds.
    pub start: f64,
    pub end: f64,
    /// Playback speed, 0.25..4.
    #[serde(default = "one")]
    pub speed: f64,
    /// Linear gain, 1 is unchanged.
    #[serde(default = "one")]
    pub volume: f64,
    #[serde(default)]
    pub mute: bool,
    /// Fade from/to black (and silence), seconds of the clip's timeline.
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    /// How this clip starts after the previous one; none is a hard cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    #[serde(default = "transition_length")]
    pub transition_length: f64,
    #[serde(default)]
    pub effects: Effects,
    /// Sound treatment: 0 off, 1 voice, 2 voice in a noisy room, 3 even out.
    #[serde(default)]
    pub voice: u8,
    /// Level of the desktop sound against the microphone, in a recording
    /// with both on their own tracks.
    #[serde(default = "one")]
    pub desktop_volume: f64,
    #[serde(default, skip_serializing_if = "is_default_pip")]
    pub pip: Pip,
}

fn is_default_pip(p: &Pip) -> bool {
    *p == Pip::default()
}

fn transition_length() -> f64 {
    0.8
}

/// Picture-in-picture over a clip: the recording's own camera track, or a
/// picture or film of the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pip {
    /// 0 the recording's camera when it has a track of its own, 1 off,
    /// 2 the picture or film `media`.
    #[serde(default)]
    pub mode: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<usize>,
    /// Centre x, centre y and width in percent of the frame; a width of 0
    /// is where and how large it was recorded.
    #[serde(default)]
    pub area: [f64; 3],
    /// 0 as recorded, 1 rounded, 2 circle, 3 rectangle.
    #[serde(default)]
    pub shape: u8,
    /// 0 as recorded, then the colours of PIP_BORDERS.
    #[serde(default)]
    pub border: u8,
    #[serde(default)]
    pub shadow: bool,
    /// 0 none, 1 fade, 2 slide in from its side.
    #[serde(default)]
    pub effect: u8,
    #[serde(default)]
    pub start: f64,
    /// 0: to the end of the clip.
    #[serde(default)]
    pub length: f64,
}

impl Default for Pip {
    fn default() -> Self {
        Self {
            mode: 0,
            media: None,
            area: [0.0; 3],
            shape: 0,
            border: 0,
            shadow: false,
            effect: 0,
            start: 0.0,
            length: 0.0,
        }
    }
}

/// Border choices: (colour, name); index 0 is "as recorded".
pub const PIP_BORDERS: [(Option<u32>, &str); 6] = [
    (None, "As recorded"),
    (None, "None"),
    (Some(0xffffff), "White"),
    (Some(0x111111), "Black"),
    (Some(0xffd84d), "Yellow"),
    (Some(0x7aa2f7), "Blue"),
];

impl Clip {
    pub fn new(media: usize, start: f64, end: f64) -> Self {
        Self {
            media,
            start,
            end,
            speed: 1.0,
            volume: 1.0,
            mute: false,
            fade_in: 0.0,
            fade_out: 0.0,
            transition: None,
            transition_length: 0.8,
            effects: Effects::default(),
            voice: 0,
            desktop_volume: 1.0,
            pip: Pip::default(),
        }
    }

    /// Seconds this clip lasts in the finished film.
    pub fn length(&self) -> f64 {
        ((self.end - self.start) / self.speed.max(0.01)).max(0.0)
    }

    /// Source seconds for a time within the clip's own timeline.
    pub fn source_at(&self, local: f64) -> f64 {
        (self.start + local * self.speed).clamp(self.start, self.end)
    }
}

/// A piece of music or narration under the film.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    pub media: usize,
    /// Where it starts in the finished film, seconds.
    pub at: f64,
    pub start: f64,
    pub end: f64,
    #[serde(default = "half")]
    pub volume: f64,
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
}

fn half() -> f64 {
    0.5
}

impl AudioClip {
    pub fn length(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub schema: u32,
    #[serde(default)]
    pub media: Vec<Media>,
    #[serde(default)]
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub music: Vec<AudioClip>,
    /// Output picture size; 0 follows the first clip.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub fps: f64,
    /// Loudness of the finished film: 0 off, 1 speech and web (-16 LUFS),
    /// 2 streaming (-14 LUFS), 3 broadcast (-23 LUFS).
    #[serde(default)]
    pub loudness: u8,
    /// Lower the music while someone speaks: 0 off, 1 a little, 2 a lot.
    #[serde(default)]
    pub duck: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<Logo>,
    /// Caption text size: 0 small, 1 medium, 2 large.
    #[serde(default = "one_u8")]
    pub caption_size: u8,
}

fn one_u8() -> u8 {
    1
}

/// A picture over the whole film, such as a logo or a watermark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Logo {
    pub path: PathBuf,
    /// 0 top left, 1 top right, 2 bottom left, 3 bottom right.
    #[serde(default = "logo_corner")]
    pub corner: u8,
    /// Width as a share of the picture width, percent.
    #[serde(default = "logo_size")]
    pub size: f64,
    /// 0..1.
    #[serde(default = "logo_opacity")]
    pub opacity: f64,
}

fn logo_corner() -> u8 {
    1
}
fn logo_size() -> f64 {
    12.0
}
fn logo_opacity() -> f64 {
    0.8
}

impl Logo {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            corner: 1,
            size: 12.0,
            opacity: 0.8,
        }
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            media: Vec::new(),
            clips: Vec::new(),
            music: Vec::new(),
            width: 0,
            height: 0,
            fps: 0.0,
            // New films are levelled for speech; older projects keep what
            // they had (off).
            loudness: 1,
            duck: 1,
            logo: None,
            caption_size: 1,
        }
    }
}

impl Project {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let mut project: Project = serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a NIRUCLIP project: {e}", path.display()))?;
        if project.schema > SCHEMA {
            return Err(format!(
                "{} was saved by a newer NIRUCLIP (schema {})",
                path.display(),
                project.schema
            ));
        }
        if project.schema < 2 {
            // Top, middle, bottom became three of nine places.
            for clip in &mut project.clips {
                let e = &mut clip.effects;
                e.text_position = match e.text_position {
                    0 => 1,
                    1 => 4,
                    _ => 7,
                };
            }
        }
        project.schema = SCHEMA;
        // Relative media paths are relative to the project file.
        let base = path.parent().unwrap_or(Path::new("."));
        for media in &mut project.media {
            if media.path.is_relative() && !media.is_card() {
                media.path = base.join(&media.path);
            }
        }
        if let Some(logo) = &mut project.logo {
            if logo.path.is_relative() {
                logo.path = base.join(&logo.path);
            }
        }
        project.repair();
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("niruclip.tmp");
        std::fs::write(&tmp, text + "\n")
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("could not save {}: {e}", path.display()))
    }

    /// Drop clips that point at missing media and keep every range sane.
    pub fn repair(&mut self) {
        let n = self.media.len();
        self.clips.retain(|c| c.media < n);
        self.music.retain(|c| c.media < n);
        for clip in &mut self.clips {
            let media = &self.media[clip.media];
            let limit = if media.still {
                f64::MAX
            } else {
                media.duration
            };
            clip.start = clip.start.clamp(0.0, limit);
            clip.end = clip.end.clamp(clip.start, limit);
            clip.speed = clip.speed.clamp(0.25, 4.0);
            clip.volume = clip.volume.clamp(0.0, 4.0);
            clip.transition_length = clip.transition_length.clamp(0.1, 5.0);
            clip.desktop_volume = clip.desktop_volume.clamp(0.0, 4.0);
            clip.voice = clip.voice.min(3);
            let e = &mut clip.effects;
            e.text_position = e.text_position.min(9);
            e.font_style = e.font_style.min(3);
            e.text_look = e.text_look.min(6);
            e.text_anim = e.text_anim.min(4);
            e.text_xy = [
                e.text_xy[0].clamp(0.0, 100.0),
                e.text_xy[1].clamp(0.0, 100.0),
            ];
            let pip = &mut clip.pip;
            if pip.media.is_some_and(|m| m >= n) {
                pip.media = None;
            }
            if pip.mode == 2 && pip.media.is_none() {
                pip.mode = 0;
            }
            pip.mode = pip.mode.min(2);
            pip.shape = pip.shape.min(3);
            pip.border = pip.border.min((PIP_BORDERS.len() - 1) as u8);
            pip.effect = pip.effect.min(2);
            pip.area = [
                pip.area[0].clamp(0.0, 100.0),
                pip.area[1].clamp(0.0, 100.0),
                pip.area[2].clamp(0.0, 90.0),
            ];
            e.hide = e.hide.min(3);
            for area in [&mut e.zoom_area, &mut e.hide_area] {
                area[0] = area[0].clamp(0.0, 100.0);
                area[1] = area[1].clamp(0.0, 100.0);
                area[2] = area[2].clamp(0.0, 100.0 - area[0]);
                area[3] = area[3].clamp(0.0, 100.0 - area[1]);
            }
        }
        self.loudness = self.loudness.min(3);
        self.duck = self.duck.min(2);
        self.caption_size = self.caption_size.min(2);
    }

    /// Every caption as it lands in the finished film: (start, end, text),
    /// in film order. A caption cut by an edit shows in the part that stays.
    pub fn film_captions(&self) -> Vec<(f64, f64, String)> {
        let starts = self.starts();
        let mut out = Vec::new();
        for (i, clip) in self.clips.iter().enumerate() {
            let media = &self.media[clip.media];
            for c in &media.captions {
                let a = c.start.max(clip.start);
                let b = c.end.min(clip.end);
                if b - a < 0.05 {
                    continue;
                }
                let speed = clip.speed.max(0.01);
                let fa = starts[i] + (a - clip.start) / speed;
                let fb = starts[i] + (b - clip.start) / speed;
                out.push((fa, fb, c.text.clone()));
            }
        }
        out.sort_by(|x, y| x.0.total_cmp(&y.0));
        // A caption cut in two by a join reads as one.
        let mut merged: Vec<(f64, f64, String)> = Vec::with_capacity(out.len());
        for c in out {
            if let Some(last) = merged.last_mut() {
                if last.2 == c.2 && (c.0 - last.1).abs() < 0.05 {
                    last.1 = last.1.max(c.1);
                    continue;
                }
            }
            merged.push(c);
        }
        merged
    }

    /// The source pieces behind a stretch of the film: (media, from, to).
    pub fn sources(&self, a: f64, b: f64) -> Vec<(usize, f64, f64)> {
        let starts = self.starts();
        let mut out = Vec::new();
        for (i, clip) in self.clips.iter().enumerate() {
            let s = starts[i];
            let e = s + clip.length();
            let lo = a.max(s);
            let hi = b.min(e);
            if hi - lo < 0.02 {
                continue;
            }
            out.push((clip.media, clip.source_at(lo - s), clip.source_at(hi - s)));
        }
        out
    }

    /// Replace the captions with ones made for the film's times.
    pub fn set_film_captions(&mut self, captions: &[(f64, f64, String)]) {
        for m in &mut self.media {
            m.captions.clear();
        }
        for (a, b, text) in captions {
            for (m, from, to) in self.sources(*a, *b) {
                self.media[m].captions.push(Caption {
                    start: from,
                    end: to,
                    text: text.clone(),
                });
            }
        }
        for m in &mut self.media {
            m.captions.sort_by(|x, y| x.start.total_cmp(&y.start));
            m.captions.dedup();
        }
    }

    pub fn caption_count(&self) -> usize {
        self.media.iter().map(|m| m.captions.len()).sum()
    }

    pub fn duration(&self) -> f64 {
        let mut total = 0.0;
        for (i, clip) in self.clips.iter().enumerate() {
            total += clip.length();
            if i > 0 {
                total -= self.overlap(i);
            }
        }
        total.max(0.0)
    }

    /// Seconds clip `i` overlaps the previous one (its transition).
    pub fn overlap(&self, i: usize) -> f64 {
        if i == 0 || i >= self.clips.len() {
            return 0.0;
        }
        let clip = &self.clips[i];
        match clip.transition {
            Some(_) => clip
                .transition_length
                .min(clip.length() * 0.5)
                .min(self.clips[i - 1].length() * 0.5)
                .max(0.0),
            None => 0.0,
        }
    }

    /// Where each clip starts in the finished film.
    pub fn starts(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.clips.len());
        let mut at = 0.0;
        for (i, clip) in self.clips.iter().enumerate() {
            at -= self.overlap(i);
            out.push(at.max(0.0));
            at += clip.length();
        }
        out
    }

    /// The clip under a film time and the time inside it.
    pub fn locate(&self, time: f64) -> Option<(usize, f64)> {
        let starts = self.starts();
        for (i, clip) in self.clips.iter().enumerate().rev() {
            if time >= starts[i] - 1e-9 {
                let local = (time - starts[i]).min(clip.length());
                return Some((i, local));
            }
        }
        if self.clips.is_empty() {
            None
        } else {
            Some((0, 0.0))
        }
    }

    /// The one title card media of the project.
    pub fn card(&mut self) -> usize {
        if let Some(i) = self.media.iter().position(Media::is_card) {
            return i;
        }
        self.media.push(Media::card());
        self.media.len() - 1
    }

    pub fn add_media(&mut self, media: Media) -> usize {
        if let Some(i) = self.media.iter().position(|m| m.path == media.path) {
            return i;
        }
        self.media.push(media);
        self.media.len() - 1
    }

    /// Put a whole media file at the end (or at `index`).
    pub fn append(&mut self, media: usize, index: Option<usize>) -> usize {
        let m = &self.media[media];
        let length = if m.still { 5.0 } else { m.duration };
        let clip = Clip::new(media, 0.0, length);
        let at = index.unwrap_or(self.clips.len()).min(self.clips.len());
        self.clips.insert(at, clip);
        if self.width == 0 && m.has_video && m.width > 0 {
            self.width = m.width;
            self.height = m.height;
            self.fps = if m.fps > 0.0 { m.fps } else { 30.0 };
        }
        at
    }

    /// Split the clip under `time` into two. Returns the new clip's index.
    pub fn split(&mut self, time: f64) -> Option<usize> {
        let (i, local) = self.locate(time)?;
        let clip = self.clips[i].clone();
        if local < 0.05 || clip.length() - local < 0.05 {
            return None;
        }
        let cut = clip.source_at(local);
        let mut right = clip.clone();
        right.start = cut;
        right.transition = None;
        right.fade_in = 0.0;
        self.clips[i].end = cut;
        self.clips[i].fade_out = 0.0;
        self.clips.insert(i + 1, right);
        Some(i + 1)
    }

    /// Cut the film between two times: everything in [a, b) goes.
    pub fn remove_range(&mut self, a: f64, b: f64) {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if b - a < 0.01 {
            return;
        }
        self.split(b);
        self.split(a);
        let starts = self.starts();
        let mut keep = Vec::new();
        for (i, clip) in self.clips.iter().enumerate() {
            let mid = starts[i] + clip.length() * 0.5;
            if !(mid > a && mid < b) {
                keep.push(clip.clone());
            }
        }
        self.clips = keep;
    }

    /// Keep only [a, b) of the film.
    pub fn keep_range(&mut self, a: f64, b: f64) {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let end = self.duration();
        self.remove_range(b, end + 1.0);
        self.remove_range(0.0, a);
    }
}

/// Undo and redo by snapshots.
#[derive(Default)]
pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
}

impl History {
    /// Call before a change, with the project as it is.
    pub fn record(&mut self, before: &Project) {
        if self.undo.last() == Some(before) {
            return;
        }
        self.undo.push(before.clone());
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, current: &mut Project) -> bool {
        match self.undo.pop() {
            Some(previous) => {
                self.redo.push(std::mem::replace(current, previous));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, current: &mut Project) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(current, next));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(lengths: &[f64]) -> Project {
        let mut p = Project::default();
        for (i, l) in lengths.iter().enumerate() {
            let m = p.add_media(Media {
                path: PathBuf::from(format!("/v/{i}.mkv")),
                duration: *l,
                width: 1920,
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

    #[test]
    fn split_and_remove_keep_the_rest_of_the_film() {
        let mut p = project(&[10.0, 5.0]);
        assert!((p.duration() - 15.0).abs() < 1e-9);
        assert_eq!(p.split(4.0), Some(1));
        assert_eq!(p.clips.len(), 3);
        assert!((p.clips[0].end - 4.0).abs() < 1e-9);
        assert!((p.clips[1].start - 4.0).abs() < 1e-9);
        p.remove_range(2.0, 6.0);
        assert!((p.duration() - 11.0).abs() < 1e-9);
        assert!((p.clips[0].end - 2.0).abs() < 1e-9);
        assert!((p.clips[1].start - 6.0).abs() < 1e-9);
    }

    #[test]
    fn speed_and_transitions_change_the_length() {
        let mut p = project(&[10.0, 10.0]);
        p.clips[0].speed = 2.0;
        assert!((p.duration() - 15.0).abs() < 1e-9);
        p.clips[1].transition = Some(Transition::Fade);
        p.clips[1].transition_length = 1.0;
        assert!((p.duration() - 14.0).abs() < 1e-9);
        assert_eq!(p.starts(), vec![0.0, 4.0]);
        assert_eq!(p.locate(4.5).map(|(i, _)| i), Some(1));
    }

    #[test]
    fn keep_range_trims_both_ends() {
        let mut p = project(&[20.0]);
        p.keep_range(3.0, 8.0);
        assert!((p.duration() - 5.0).abs() < 1e-9);
        assert!((p.clips[0].start - 3.0).abs() < 1e-9);
    }

    #[test]
    fn undo_and_redo_restore_snapshots() {
        let mut p = project(&[10.0]);
        let mut h = History::default();
        h.record(&p);
        p.split(5.0);
        assert_eq!(p.clips.len(), 2);
        assert!(h.undo(&mut p));
        assert_eq!(p.clips.len(), 1);
        assert!(h.redo(&mut p));
        assert_eq!(p.clips.len(), 2);
    }

    #[test]
    fn a_project_round_trips_through_json() {
        let mut p = project(&[3.0]);
        p.clips[0].effects.text = "Hello".into();
        p.clips[0].transition = Some(Transition::WipeLeft);
        let text = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn captions_follow_the_cuts() {
        let mut p = project(&[10.0]);
        p.set_film_captions(&[(1.0, 2.0, "one".into()), (6.0, 7.0, "two".into())]);
        assert_eq!(p.caption_count(), 2);
        p.remove_range(3.0, 5.0);
        let c = p.film_captions();
        assert_eq!(c.len(), 2);
        assert!((c[1].0 - 4.0).abs() < 1e-6, "{c:?}");
        p.clips[0].speed = 2.0;
        let c = p.film_captions();
        assert!((c[0].0 - 0.5).abs() < 1e-6);
        // One caption over a cut stays one caption.
        let mut p = project(&[10.0]);
        p.set_film_captions(&[(1.0, 6.0, "across".into())]);
        p.split(3.0);
        assert_eq!(p.caption_count(), 1);
        p.set_film_captions(&p.film_captions());
        assert_eq!(p.caption_count(), 2, "stored per piece");
        assert_eq!(p.film_captions().len(), 1, "shown as one");
    }

    #[test]
    fn an_old_project_keeps_its_title_places() {
        let dir = std::env::temp_dir().join(format!("niruclip-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("old.niruclip");
        std::fs::write(
            &file,
            r#"{"schema":1,"media":[{"path":"/v/a.mkv","duration":5,"has_video":true}],
            "clips":[{"media":0,"start":0,"end":5,"effects":{"text":"Hi","text_position":0}}]}"#,
        )
        .unwrap();
        let p = Project::load(&file).unwrap();
        assert_eq!(p.clips[0].effects.text_position, 1);
        assert_eq!(p.loudness, 0, "an old film keeps its sound");
        assert_eq!(Project::default().loudness, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_window_stays_inside_the_clip() {
        assert_eq!(window(1.0, 0.0, 5.0), (1.0, 5.0));
        assert_eq!(window(1.0, 2.0, 5.0), (1.0, 3.0));
        assert_eq!(window(7.0, 2.0, 5.0), (5.0, 5.0));
    }
}
