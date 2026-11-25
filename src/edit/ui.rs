//! The editor window: layout, drawing, mouse and keys.
use super::browser::{human_size, Browser, Purpose};
use super::export::{self, Format, HEIGHTS};
use super::gfx::{Canvas, Text};
use super::job::Job;
use super::media::{Cache, PEAKS_PER_SECOND, THUMB_H};
use super::mpv::{self, Mpv, Timeline};
use super::project::{AudioClip, Clip, History, Media, Project, Transition, PIP_BORDERS};
use super::record::{self, Recording};
use super::{analyze, probe, Settings, Theme};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event as XEvent;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

// ── Keys ────────────────────────────────────────────────────────────────────
const K_RETURN: u32 = 0xff0d;
const K_KP_ENTER: u32 = 0xff8d;
const K_ESCAPE: u32 = 0xff1b;
const K_BACKSPACE: u32 = 0xff08;
const K_TAB: u32 = 0xff09;
const K_ISO_LEFT_TAB: u32 = 0xfe20;
const K_DELETE: u32 = 0xffff;
const K_HOME: u32 = 0xff50;
const K_LEFT: u32 = 0xff51;
const K_UP: u32 = 0xff52;
const K_RIGHT: u32 = 0xff53;
const K_DOWN: u32 = 0xff54;
const K_PAGE_UP: u32 = 0xff55;
const K_PAGE_DOWN: u32 = 0xff56;
const K_END: u32 = 0xff57;
const K_INSERT: u32 = 0xff63;
const K_F1: u32 = 0xffbe;
const K_F2: u32 = 0xffbf;
const K_F11: u32 = 0xffc8;
const K_KP_ADD: u32 = 0xffab;
const K_KP_SUB: u32 = 0xffad;

const SHIFT: u16 = 1;
const CTRL: u16 = 4;
const ALT: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    fn has(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Open,
    Import,
    Music,
    Save,
    Export,
    Settings,
    Help,
    Play,
    Split,
    Undo,
    Redo,
    MarkIn,
    MarkOut,
    RemoveRange,
    KeepRange,
    Preview,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ExportStart,
    ExportCancel,
    ExportFolder,
    ExportName,
    BrowserUp,
    BrowserAccept,
    BrowserCancel,
    BrowserHidden,
    MediaFolder,
    ExportFolderSetting,
    ResetFolders,
    CloseOverlay,
    Recent(usize),
    Recover,
    Record,
    RecordStart,
    Film,
    Pauses,
    PausesApply,
    Captions,
    CaptionsMake,
    CaptionEdit,
    CaptionDelete,
    Card,
    AreaAccept,
    AreaClear,
    AreaWhole,
    FontAccept,
    PlaceReset,
    GoStart,
    GoEnd,
    PrevCut,
    NextCut,
    Loop,
    Snap,
    FullScreen,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Minus,
    Plus,
    Bar(f64),
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Button(Action),
    Bin(usize),
    Param(usize, Part),
    Ruler,
    Clip(usize, Edge),
    Music(usize, Edge),
    Lane,
    Entry(usize),
    Format(usize),
    Height(usize),
    /// Inside a dialog: nothing, but the click does not fall through.
    Block,
    /// A choice in a dialog of choices (record, film, pauses): row and
    /// option.
    Choice(usize, usize),
    Limit(usize),
    CaptionMode(usize),
    /// A row in the caption or font list.
    ListRow(usize),
    /// The still in the area picker.
    AreaImage,
    /// The rendered picture in the placing view.
    PlaceImage,
    /// A clip's title (0) or picture-in-picture (1) on its lane.
    Window(usize, u8, Edge),
    /// A clip with a camera track that is switched off: its ghost.
    CameraOff(usize),
    /// A lane's switch: 0 titles, 1 camera, 2 sound, 3 music.
    LaneOff(u8),
    /// The place where the sound changes over into clip `i` (J and L cuts).
    SoundCut(usize),
    /// A clip's volume line.
    Volume(usize),
    /// A marker: clip and its index there.
    Marker(usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edge {
    Left,
    Body,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Timeline,
    Bin,
    Inspector,
}

#[derive(Clone, Debug)]
enum Drag {
    /// Moving the title (0) or the picture-in-picture (1) in the placing
    /// view: the picture's rect and where it was taken hold of, in frame
    /// pixels from the thing's centre.
    Place {
        image: Rect,
        what: u8,
        grab: (f64, f64),
    },
    Scrub,
    Range(f64),
    /// A title (0) or picture-in-picture (1) window of clip `i`, from
    /// (a0, z0) seconds into the clip.
    Window {
        i: usize,
        what: u8,
        edge: Edge,
        a0: f64,
        z0: f64,
        x0: i32,
    },
    SoundCut {
        i: usize,
        o0: f64,
        x0: i32,
    },
    Volume {
        i: usize,
        v0: f64,
        y0: i32,
        h: i32,
    },
    Clip {
        i: usize,
        x0: i32,
        moved: bool,
    },
    TrimLeft {
        i: usize,
        start0: f64,
        x0: i32,
    },
    TrimRight {
        i: usize,
        end0: f64,
        x0: i32,
    },
    Music {
        j: usize,
        at0: f64,
        x0: i32,
    },
    MusicLeft {
        j: usize,
        start0: f64,
        at0: f64,
        x0: i32,
    },
    MusicRight {
        j: usize,
        end0: f64,
        x0: i32,
    },
    Slider {
        row: usize,
        rect: Rect,
    },
    Media {
        i: usize,
        x0: i32,
        y0: i32,
        active: bool,
    },
    /// Drawing the rectangle of the area picker, from (fx, fy).
    Area {
        fx: f64,
        fy: f64,
        image: Rect,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Param {
    Speed,
    Volume,
    Mute,
    FadeIn,
    FadeOut,
    Voice,
    DesktopVolume,
    SoundCut,
    Transition,
    TransitionLength,
    CardColour,
    Brightness,
    Contrast,
    Saturation,
    Blur,
    Sharpen,
    Rotate,
    Mirror,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
    ZoomArea,
    ZoomStart,
    ZoomLength,
    ZoomRamp,
    HideKind,
    HideArea,
    HideStart,
    HideLength,
    Title,
    Template,
    Font,
    FontStyle,
    TextColour,
    TextLook,
    TitlePosition,
    TitleSize,
    TextStart,
    TextLength,
    TextFade,
    TextEffect,
    TextAccent,
    PlaceText,
    Pip,
    PipShape,
    PipBorder,
    PipShadow,
    PipEffect,
    PipStart,
    PipLength,
    PlacePip,
    MusicVolume,
    MusicFadeIn,
    MusicFadeOut,
}

/// Every clip setting, in the order the panel shows them.
const ALL_CLIP_PARAMS: [Param; 52] = [
    Param::Speed,
    Param::Volume,
    Param::Mute,
    Param::FadeIn,
    Param::FadeOut,
    Param::Voice,
    Param::DesktopVolume,
    Param::SoundCut,
    Param::Transition,
    Param::TransitionLength,
    Param::CardColour,
    Param::Brightness,
    Param::Contrast,
    Param::Saturation,
    Param::Blur,
    Param::Sharpen,
    Param::Rotate,
    Param::Mirror,
    Param::CropLeft,
    Param::CropTop,
    Param::CropRight,
    Param::CropBottom,
    Param::ZoomArea,
    Param::ZoomStart,
    Param::ZoomLength,
    Param::ZoomRamp,
    Param::HideKind,
    Param::HideArea,
    Param::HideStart,
    Param::HideLength,
    Param::Pip,
    Param::PlacePip,
    Param::PipShape,
    Param::PipBorder,
    Param::PipShadow,
    Param::PipEffect,
    Param::PipStart,
    Param::PipLength,
    Param::Title,
    Param::Template,
    Param::PlaceText,
    Param::Font,
    Param::FontStyle,
    Param::TextColour,
    Param::TextLook,
    Param::TextAccent,
    Param::TitlePosition,
    Param::TitleSize,
    Param::TextEffect,
    Param::TextStart,
    Param::TextLength,
    Param::TextFade,
];
const MUSIC_PARAMS: [Param; 3] = [Param::MusicVolume, Param::MusicFadeIn, Param::MusicFadeOut];

/// The settings that apply to this clip: the rest of a group shows once
/// its first setting is in use.
fn clip_params(c: &Clip, m: &Media) -> Vec<Param> {
    let e = &c.effects;
    let card = m.is_card();
    ALL_CLIP_PARAMS
        .iter()
        .copied()
        .filter(|p| match p {
            Param::Speed => !m.still,
            Param::Volume | Param::Mute => m.has_audio && !m.still,
            Param::Voice => m.has_audio && !m.still,
            Param::DesktopVolume => m.split_tracks().is_some(),
            Param::SoundCut => m.has_audio && !m.still && c.transition.is_none(),
            Param::TransitionLength => c.transition.is_some(),
            Param::CardColour => card,
            Param::Brightness
            | Param::Contrast
            | Param::Saturation
            | Param::Blur
            | Param::Sharpen
            | Param::Rotate
            | Param::Mirror
            | Param::CropLeft
            | Param::CropTop
            | Param::CropRight
            | Param::CropBottom
            | Param::ZoomArea
            | Param::HideKind => !card && m.has_video,
            Param::ZoomStart | Param::ZoomLength | Param::ZoomRamp => !card && e.has_zoom(),
            Param::HideArea | Param::HideStart | Param::HideLength => !card && e.hide > 0,
            Param::Pip => m.has_video,
            Param::PlacePip
            | Param::PipShape
            | Param::PipBorder
            | Param::PipShadow
            | Param::PipEffect
            | Param::PipStart
            | Param::PipLength => pip_on(c, m),
            Param::TextAccent => !e.text.trim().is_empty() && e.text_look >= 4,
            Param::Template
            | Param::Font
            | Param::FontStyle
            | Param::TextColour
            | Param::TextLook
            | Param::TitlePosition
            | Param::TitleSize
            | Param::TextStart
            | Param::TextLength
            | Param::TextFade
            | Param::TextEffect
            | Param::PlaceText => !e.text.trim().is_empty(),
            _ => true,
        })
        .collect()
}

/// Whether a clip shows a picture-in-picture: its own camera track or a
/// chosen picture or film.
fn pip_on(c: &Clip, m: &Media) -> bool {
    match c.pip.mode {
        1 => false,
        2 => c.pip.media.is_some(),
        _ => !m.still && !m.is_card() && m.own_camera().is_some(),
    }
}

const PIP_SHAPES: [&str; 4] = ["As recorded", "Rounded", "Circle", "Rectangle"];
const PIP_EFFECTS: [&str; 3] = ["None", "Fade", "Slide in"];

/// What a clip's picture-in-picture shows, for the panel.
fn pip_label(project: &Project, c: &Clip) -> String {
    let m = &project.media[c.media];
    match c.pip.mode {
        1 => "Off".into(),
        2 => c
            .pip
            .media
            .and_then(|k| project.media.get(k))
            .map(|x| x.name())
            .unwrap_or_else(|| "Off".into()),
        _ if m.own_camera().is_some() && !m.still => "Camera (its own track)".into(),
        _ => "Off (←→ picks a picture or film)".into(),
    }
}

/// Text colours to choose from.
const TEXT_COLOURS: [(u32, &str); 8] = [
    (0xffffff, "White"),
    (0xffd84d, "Yellow"),
    (0x000000, "Black"),
    (0x7aa2f7, "Blue"),
    (0xff6b6b, "Red"),
    (0x6bd68a, "Green"),
    (0xff9f43, "Orange"),
    (0xb8b8c0, "Grey"),
];

/// Title card backgrounds.
const CARD_COLOURS: [(u32, &str); 8] = [
    (0x101014, "Near black"),
    (0x000000, "Black"),
    (0xffffff, "White"),
    (0x1e2a4a, "Navy"),
    (0x2f4a3a, "Forest"),
    (0x5a1f2a, "Wine"),
    (0x3a3a42, "Slate"),
    (0xf4ecd8, "Cream"),
];

const PLACES: [&str; 10] = [
    "Top left",
    "Top",
    "Top right",
    "Left",
    "Middle",
    "Right",
    "Bottom left",
    "Bottom",
    "Bottom right",
    "Free (placed with E)",
];
const HIDES: [&str; 4] = ["Off", "Blur", "Pixelate", "Black box"];
const TEMPLATES: [&str; 9] = [
    "Custom",
    "Heading",
    "Name tag",
    "Subtitle bar",
    "Lower third",
    "Neon heading",
    "Marker",
    "Pop heading",
    "Typewriter",
];

/// (position, size, look, style, start, length, fade, effect) of each
/// template.
type Template = (u8, f64, u8, u8, f64, f64, f64, u8);
fn template(i: usize) -> Option<Template> {
    match i {
        1 => Some((4, 10.0, 2, 1, 0.0, 0.0, 0.5, 0)),
        2 => Some((6, 5.0, 0, 1, 0.5, 5.0, 0.4, 0)),
        3 => Some((7, 5.0, 0, 0, 0.0, 0.0, 0.0, 0)),
        4 => Some((6, 5.5, 4, 1, 0.5, 6.0, 0.5, 2)),
        5 => Some((4, 11.0, 5, 1, 0.0, 0.0, 0.6, 3)),
        6 => Some((7, 6.0, 6, 1, 0.0, 0.0, 0.4, 1)),
        7 => Some((4, 12.0, 2, 1, 0.0, 0.0, 0.5, 3)),
        8 => Some((7, 6.0, 0, 0, 0.0, 0.0, 0.0, 4)),
        _ => None,
    }
}

fn template_of(e: &super::project::Effects) -> usize {
    (1..TEMPLATES.len())
        .find(|i| {
            template(*i).is_some_and(|(pos, size, look, style, start, length, fade, anim)| {
                e.text_position == pos
                    && (e.text_size - size).abs() < 1e-6
                    && e.text_look == look
                    && e.font_style == style
                    && (e.text_start - start).abs() < 1e-6
                    && (e.text_length - length).abs() < 1e-6
                    && (e.text_fade - fade).abs() < 1e-6
                    && e.text_anim == anim
            })
        })
        .unwrap_or(0)
}

fn palette_index(list: &[(u32, &str)], colour: u32) -> usize {
    list.iter().position(|(c, _)| *c == colour).unwrap_or(0)
}

impl Param {
    fn group(self) -> &'static str {
        match self {
            Param::Speed | Param::Volume | Param::Mute | Param::FadeIn | Param::FadeOut => "Clip",
            Param::Voice | Param::DesktopVolume | Param::SoundCut => "Sound",
            Param::Transition | Param::TransitionLength => "Transition into this clip",
            Param::CardColour => "Title card",
            Param::Brightness
            | Param::Contrast
            | Param::Saturation
            | Param::Blur
            | Param::Sharpen
            | Param::Rotate
            | Param::Mirror => "Picture",
            Param::CropLeft | Param::CropTop | Param::CropRight | Param::CropBottom => "Crop",
            Param::ZoomArea | Param::ZoomStart | Param::ZoomLength | Param::ZoomRamp => "Zoom",
            Param::HideKind | Param::HideArea | Param::HideStart | Param::HideLength => {
                "Hide part of the picture"
            }
            Param::Title
            | Param::Template
            | Param::Font
            | Param::FontStyle
            | Param::TextColour
            | Param::TextLook
            | Param::TitlePosition
            | Param::TitleSize
            | Param::TextStart
            | Param::TextLength
            | Param::TextFade
            | Param::TextEffect
            | Param::TextAccent
            | Param::PlaceText => "Title",
            Param::Pip
            | Param::PipShape
            | Param::PipBorder
            | Param::PipShadow
            | Param::PipEffect
            | Param::PipStart
            | Param::PipLength
            | Param::PlacePip => "Picture in picture",
            Param::MusicVolume | Param::MusicFadeIn | Param::MusicFadeOut => "Music",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Param::Speed => "Speed",
            Param::Volume | Param::MusicVolume => "Volume",
            Param::Mute => "Sound",
            Param::FadeIn | Param::MusicFadeIn => "Fade in",
            Param::FadeOut | Param::MusicFadeOut => "Fade out",
            Param::Voice => "Treatment",
            Param::DesktopVolume => "Desktop",
            Param::SoundCut => "Sound cut",
            Param::Transition => "Kind",
            Param::TransitionLength => "Length",
            Param::CardColour => "Background",
            Param::Brightness => "Brightness",
            Param::Contrast => "Contrast",
            Param::Saturation => "Colour",
            Param::Blur => "Blur",
            Param::Sharpen => "Sharpen",
            Param::Rotate => "Rotate",
            Param::Mirror => "Mirror",
            Param::CropLeft => "Left",
            Param::CropTop => "Top",
            Param::CropRight => "Right",
            Param::CropBottom => "Bottom",
            Param::ZoomArea | Param::HideArea => "Area",
            Param::ZoomStart | Param::HideStart | Param::TextStart | Param::PipStart => "Starts at",
            Param::ZoomLength | Param::HideLength | Param::TextLength | Param::PipLength => "Lasts",
            Param::TextEffect | Param::PipEffect => "Effect",
            Param::TextAccent => "Accent",
            Param::PlaceText | Param::PlacePip => "On the picture",
            Param::Pip => "Shows",
            Param::PipShape => "Shape",
            Param::PipBorder => "Border",
            Param::PipShadow => "Shadow",
            Param::ZoomRamp => "Glide",
            Param::HideKind => "How",
            Param::Title => "Text",
            Param::Template => "Template",
            Param::Font => "Font",
            Param::FontStyle => "Style",
            Param::TextColour => "Colour",
            Param::TextLook => "Look",
            Param::TitlePosition => "Position",
            Param::TitleSize => "Size",
            Param::TextFade => "Fade",
        }
    }

    /// (min, max, step); times inside the clip go to its length.
    fn limits(self, c: &Clip) -> (f64, f64, f64) {
        let len = c.length().max(0.1);
        match self {
            Param::ZoomStart
            | Param::ZoomLength
            | Param::HideStart
            | Param::HideLength
            | Param::TextStart
            | Param::TextLength
            | Param::PipStart
            | Param::PipLength => (0.0, (len * 10.0).floor() / 10.0, 0.1),
            _ => self.range(),
        }
    }

    /// (min, max, step)
    fn range(self) -> (f64, f64, f64) {
        match self {
            Param::Speed => (0.25, 4.0, 0.25),
            Param::Volume => (0.0, 4.0, 0.1),
            Param::DesktopVolume => (0.0, 2.0, 0.05),
            Param::SoundCut => (-5.0, 5.0, 0.1),
            Param::MusicVolume => (0.0, 2.0, 0.05),
            Param::Mute | Param::Mirror => (0.0, 1.0, 1.0),
            Param::FadeIn | Param::FadeOut | Param::MusicFadeIn | Param::MusicFadeOut => {
                (0.0, 5.0, 0.25)
            }
            Param::Voice => (0.0, 3.0, 1.0),
            Param::Transition => (0.0, Transition::ALL.len() as f64, 1.0),
            Param::TransitionLength => (0.2, 3.0, 0.1),
            Param::CardColour => (0.0, (CARD_COLOURS.len() - 1) as f64, 1.0),
            Param::Brightness => (-0.5, 0.5, 0.05),
            Param::Contrast => (0.5, 2.0, 0.05),
            Param::Saturation => (0.0, 2.0, 0.1),
            Param::Blur => (0.0, 20.0, 1.0),
            Param::Sharpen => (0.0, 1.5, 0.1),
            Param::Rotate => (0.0, 3.0, 1.0),
            Param::CropLeft | Param::CropTop | Param::CropRight | Param::CropBottom => {
                (0.0, 40.0, 1.0)
            }
            Param::ZoomRamp => (0.1, 3.0, 0.1),
            Param::HideKind => (0.0, 3.0, 1.0),
            Param::Template => (0.0, (TEMPLATES.len() - 1) as f64, 1.0),
            Param::FontStyle => (0.0, 3.0, 1.0),
            Param::TextColour => (0.0, (TEXT_COLOURS.len() - 1) as f64, 1.0),
            Param::TextLook => (0.0, 6.0, 1.0),
            Param::TitlePosition => (0.0, 9.0, 1.0),
            Param::TextEffect => (0.0, 4.0, 1.0),
            Param::TextAccent => (0.0, (TEXT_COLOURS.len() - 1) as f64, 1.0),
            Param::PipShape => (0.0, 3.0, 1.0),
            Param::PipBorder => (0.0, (PIP_BORDERS.len() - 1) as f64, 1.0),
            Param::PipShadow => (0.0, 1.0, 1.0),
            Param::PipEffect => (0.0, 2.0, 1.0),
            Param::TitleSize => (2.0, 20.0, 1.0),
            Param::TextFade => (0.0, 2.0, 0.1),
            Param::ZoomStart
            | Param::ZoomLength
            | Param::HideStart
            | Param::HideLength
            | Param::TextStart
            | Param::TextLength
            | Param::PipStart
            | Param::PipLength => (0.0, 60.0, 0.1),
            Param::Title
            | Param::Font
            | Param::ZoomArea
            | Param::HideArea
            | Param::Pip
            | Param::PlaceText
            | Param::PlacePip => (0.0, 0.0, 0.0),
        }
    }

    fn slider(self) -> bool {
        !matches!(
            self,
            Param::Mute
                | Param::Mirror
                | Param::Voice
                | Param::Transition
                | Param::CardColour
                | Param::Rotate
                | Param::HideKind
                | Param::Template
                | Param::FontStyle
                | Param::TextColour
                | Param::TextLook
                | Param::Title
                | Param::Font
                | Param::ZoomArea
                | Param::HideArea
                | Param::TitlePosition
                | Param::TextEffect
                | Param::TextAccent
                | Param::PlaceText
                | Param::Pip
                | Param::PipShape
                | Param::PipBorder
                | Param::PipShadow
                | Param::PipEffect
                | Param::PlacePip
        )
    }

    /// A row that opens a dialog instead of stepping a value.
    fn opens(self) -> bool {
        matches!(
            self,
            Param::Title
                | Param::Font
                | Param::ZoomArea
                | Param::HideArea
                | Param::PlaceText
                | Param::PlacePip
        )
    }

    /// A start or a length: Enter takes it from the playhead.
    fn is_time(self) -> bool {
        matches!(
            self,
            Param::ZoomStart
                | Param::ZoomLength
                | Param::HideStart
                | Param::HideLength
                | Param::TextStart
                | Param::TextLength
                | Param::PipStart
                | Param::PipLength
        )
    }

    fn get(self, c: &Clip) -> f64 {
        let e = &c.effects;
        match self {
            Param::Speed => c.speed,
            Param::Volume => c.volume,
            Param::Mute => f64::from(u8::from(c.mute)),
            Param::FadeIn => c.fade_in,
            Param::FadeOut => c.fade_out,
            Param::Voice => f64::from(c.voice),
            Param::DesktopVolume => c.desktop_volume,
            Param::SoundCut => c.audio_offset,
            Param::Transition => c
                .transition
                .and_then(|t| Transition::ALL.iter().position(|x| *x == t))
                .map(|i| i as f64 + 1.0)
                .unwrap_or(0.0),
            Param::TransitionLength => c.transition_length,
            Param::CardColour => palette_index(&CARD_COLOURS, e.card_colour) as f64,
            Param::Brightness => e.brightness,
            Param::Contrast => e.contrast,
            Param::Saturation => e.saturation,
            Param::Blur => f64::from(e.blur),
            Param::Sharpen => e.sharpen,
            Param::Rotate => f64::from(e.rotate),
            Param::Mirror => f64::from(u8::from(e.flip)),
            Param::CropLeft => e.crop[0],
            Param::CropTop => e.crop[1],
            Param::CropRight => e.crop[2],
            Param::CropBottom => e.crop[3],
            Param::ZoomStart => e.zoom_start,
            Param::ZoomLength => e.zoom_length,
            Param::ZoomRamp => e.zoom_ramp,
            Param::HideKind => f64::from(e.hide),
            Param::HideStart => e.hide_start,
            Param::HideLength => e.hide_length,
            Param::Template => template_of(e) as f64,
            Param::FontStyle => f64::from(e.font_style),
            Param::TextColour => palette_index(&TEXT_COLOURS, e.text_colour) as f64,
            Param::TextLook => f64::from(e.text_look),
            Param::TitlePosition => f64::from(e.text_position),
            Param::TitleSize => e.text_size,
            Param::TextStart => e.text_start,
            Param::TextLength => e.text_length,
            Param::TextFade => e.text_fade,
            Param::TextEffect => f64::from(e.text_anim),
            Param::TextAccent => palette_index(&TEXT_COLOURS, e.text_accent) as f64,
            Param::PipShape => f64::from(c.pip.shape),
            Param::PipBorder => f64::from(c.pip.border),
            Param::PipShadow => f64::from(u8::from(c.pip.shadow)),
            Param::PipEffect => f64::from(c.pip.effect),
            Param::PipStart => c.pip.start,
            Param::PipLength => c.pip.length,
            _ => 0.0,
        }
    }

    fn set(self, c: &mut Clip, v: f64) {
        let (lo, hi, _) = self.limits(c);
        let v = v.clamp(lo, hi);
        let e = &mut c.effects;
        match self {
            Param::Speed => c.speed = v,
            Param::Volume => c.volume = v,
            Param::Mute => c.mute = v >= 0.5,
            Param::FadeIn => c.fade_in = v,
            Param::FadeOut => c.fade_out = v,
            Param::Voice => c.voice = v.round() as u8,
            Param::DesktopVolume => c.desktop_volume = v,
            Param::SoundCut => c.audio_offset = if v.abs() < 0.05 { 0.0 } else { v },
            Param::Transition => {
                let i = v.round() as usize;
                c.transition = if i == 0 {
                    None
                } else {
                    Transition::ALL.get(i - 1).copied()
                };
            }
            Param::TransitionLength => c.transition_length = v,
            Param::CardColour => e.card_colour = CARD_COLOURS[v.round() as usize].0,
            Param::Brightness => e.brightness = v,
            Param::Contrast => e.contrast = v,
            Param::Saturation => e.saturation = v,
            Param::Blur => e.blur = v.round() as u32,
            Param::Sharpen => e.sharpen = v,
            Param::Rotate => e.rotate = (v.round() as u8) % 4,
            Param::Mirror => e.flip = v >= 0.5,
            Param::CropLeft => e.crop[0] = v,
            Param::CropTop => e.crop[1] = v,
            Param::CropRight => e.crop[2] = v,
            Param::CropBottom => e.crop[3] = v,
            Param::ZoomStart => e.zoom_start = v,
            Param::ZoomLength => e.zoom_length = v,
            Param::ZoomRamp => e.zoom_ramp = v,
            Param::HideKind => {
                e.hide = v.round() as u8;
                if e.hide > 0 && e.hide_area[2] < 0.5 {
                    // A sensible first area: the middle of the picture.
                    e.hide_area = [35.0, 35.0, 30.0, 30.0];
                }
            }
            Param::HideStart => e.hide_start = v,
            Param::HideLength => e.hide_length = v,
            Param::Template => {
                if let Some((pos, size, look, style, start, length, fade, anim)) =
                    template(v.round() as usize)
                {
                    e.text_position = pos;
                    e.text_size = size;
                    e.text_look = look;
                    e.font_style = style;
                    e.text_start = start;
                    e.text_length = length;
                    e.text_fade = fade;
                    e.text_anim = anim;
                }
            }
            Param::FontStyle => e.font_style = v.round() as u8,
            Param::TextColour => e.text_colour = TEXT_COLOURS[v.round() as usize].0,
            Param::TextLook => e.text_look = v.round() as u8,
            Param::TitlePosition => e.text_position = v.round() as u8,
            Param::TitleSize => e.text_size = v,
            Param::TextStart => e.text_start = v,
            Param::TextLength => e.text_length = v,
            Param::TextFade => e.text_fade = v,
            Param::TextEffect => e.text_anim = v.round() as u8,
            Param::TextAccent => e.text_accent = TEXT_COLOURS[v.round() as usize].0,
            Param::PipShape => c.pip.shape = v.round() as u8,
            Param::PipBorder => c.pip.border = v.round() as u8,
            Param::PipShadow => c.pip.shadow = v >= 0.5,
            Param::PipEffect => c.pip.effect = v.round() as u8,
            Param::PipStart => c.pip.start = v,
            Param::PipLength => c.pip.length = v,
            _ => {}
        }
    }

    fn show(self, c: &Clip) -> String {
        let v = self.get(c);
        let e = &c.effects;
        let seconds = |v: f64, zero: &str| {
            if v <= 0.0 {
                zero.to_string()
            } else {
                format!("{v:.1} s")
            }
        };
        match self {
            Param::Speed => format!("{v:.2}×"),
            Param::Volume | Param::DesktopVolume => format!("{:.0} %", v * 100.0),
            Param::SoundCut if v > 0.0 => format!("J: {v:.1} s early"),
            Param::SoundCut if v < 0.0 => format!("L: previous runs {:.1} s on", -v),
            Param::SoundCut => "With the picture".into(),
            Param::Mute => if c.mute { "Muted" } else { "On" }.into(),
            Param::FadeIn | Param::FadeOut | Param::TransitionLength => format!("{v:.2} s"),
            Param::Voice => export::VOICE[c.voice.min(3) as usize].into(),
            Param::Transition => match c.transition {
                Some(t) => t.label().into(),
                None => "Cut (none)".into(),
            },
            Param::CardColour => CARD_COLOURS[v as usize].1.into(),
            Param::Brightness => format!("{:+.0}", v * 100.0),
            Param::Contrast | Param::Saturation => format!("{:.0} %", v * 100.0),
            Param::Blur => {
                if v == 0.0 {
                    "Off".into()
                } else {
                    format!("{v:.0}")
                }
            }
            Param::Sharpen => {
                if v == 0.0 {
                    "Off".into()
                } else {
                    format!("{v:.1}")
                }
            }
            Param::Rotate => format!("{}°", (v as u32) * 90),
            Param::Mirror => if e.flip { "On" } else { "Off" }.into(),
            Param::CropLeft | Param::CropTop | Param::CropRight | Param::CropBottom => {
                format!("{v:.0} %")
            }
            Param::ZoomArea => {
                if e.has_zoom() {
                    format!("{:.0} % of the width (Enter)", e.zoom_area[2])
                } else {
                    "Off (Enter to choose)".into()
                }
            }
            Param::HideArea => format!(
                "{:.0}×{:.0} % at {:.0},{:.0} (Enter)",
                e.hide_area[2], e.hide_area[3], e.hide_area[0], e.hide_area[1]
            ),
            Param::ZoomStart | Param::HideStart | Param::TextStart | Param::PipStart => {
                seconds(v, "The start")
            }
            Param::ZoomLength | Param::HideLength | Param::TextLength | Param::PipLength => {
                seconds(v, "To the end")
            }
            Param::ZoomRamp => format!("{v:.1} s"),
            Param::HideKind => HIDES[e.hide.min(3) as usize].into(),
            Param::Title => {
                if e.text.is_empty() {
                    "None (Enter to write)".into()
                } else {
                    e.text.replace('\n', " ⏎ ")
                }
            }
            Param::Template => TEMPLATES[v as usize].into(),
            Param::Font => {
                if e.font.is_empty() {
                    "Desktop font (Enter)".into()
                } else {
                    e.font.clone()
                }
            }
            Param::FontStyle => super::fonts::STYLES[e.font_style.min(3) as usize].into(),
            Param::TextColour => {
                let i = palette_index(&TEXT_COLOURS, e.text_colour);
                if TEXT_COLOURS[i].0 == e.text_colour {
                    TEXT_COLOURS[i].1.into()
                } else {
                    format!("#{:06x}", e.text_colour)
                }
            }
            Param::TextLook => export::LOOK_NAMES[e.text_look.min(6) as usize].into(),
            Param::TitlePosition => PLACES[(v as usize).min(9)].into(),
            Param::TextEffect => export::TEXT_EFFECTS[e.text_anim.min(4) as usize].into(),
            Param::TextAccent => {
                let i = palette_index(&TEXT_COLOURS, e.text_accent);
                if TEXT_COLOURS[i].0 == e.text_accent {
                    TEXT_COLOURS[i].1.into()
                } else {
                    format!("#{:06x}", e.text_accent)
                }
            }
            Param::PlaceText | Param::PlacePip => "Drag it there (Enter or E)".into(),
            Param::PipShape => PIP_SHAPES[c.pip.shape.min(3) as usize].into(),
            Param::PipBorder => PIP_BORDERS[(c.pip.border as usize).min(PIP_BORDERS.len() - 1)]
                .1
                .into(),
            Param::PipShadow => if c.pip.shadow { "On" } else { "Off" }.into(),
            Param::PipEffect => PIP_EFFECTS[c.pip.effect.min(2) as usize].into(),
            Param::TitleSize => format!("{v:.0} %"),
            Param::TextFade => seconds(v, "None"),
            _ => String::new(),
        }
    }

    fn get_music(self, m: &AudioClip) -> f64 {
        match self {
            Param::MusicVolume => m.volume,
            Param::MusicFadeIn => m.fade_in,
            Param::MusicFadeOut => m.fade_out,
            _ => 0.0,
        }
    }

    fn set_music(self, m: &mut AudioClip, v: f64) {
        let (lo, hi, _) = self.range();
        let v = v.clamp(lo, hi);
        match self {
            Param::MusicVolume => m.volume = v,
            Param::MusicFadeIn => m.fade_in = v,
            Param::MusicFadeOut => m.fade_out = v,
            _ => {}
        }
    }

    fn show_music(self, m: &AudioClip) -> String {
        let v = self.get_music(m);
        match self {
            Param::MusicVolume => format!("{:.0} %", v * 100.0),
            _ => format!("{v:.2} s"),
        }
    }
}

enum Overlay {
    None,
    Browser(Browser),
    Export(ExportForm),
    Settings,
    Help,
    Record(record::Form),
    TextInput {
        title: String,
        text: String,
        target: TextTarget,
    },
    /// Settings of the whole film: loudness, music, logo, captions.
    Film {
        row: usize,
    },
    Pauses(PauseForm),
    Captions(CaptionForm),
    Fonts(FontPicker),
    Area(AreaPicker),
    Place(Placer),
}

/// The placing view: one rendered picture of the clip (every effect, the
/// picture-in-picture and the title as exported) to drag the title and the
/// picture-in-picture into place and give them a look.
struct Placer {
    clip: usize,
    image: Vec<u32>,
    iw: i32,
    ih: i32,
    /// The frame the boxes are measured in.
    fw: u32,
    fh: u32,
    /// 0 the title, 1 the picture-in-picture.
    selected: u8,
}

#[derive(Clone, Copy)]
enum TextTarget {
    Title(usize),
    ExportName,
    /// A caption, by its place in the film's list.
    Caption(usize),
    /// A marker: clip and its index there.
    Marker(usize, usize),
}

struct ExportForm {
    format: usize,
    height: usize,
    name: String,
    /// Index into export::LIMITS.
    limit: usize,
    /// See export::CAPTIONS.
    captions: usize,
}

/// Finding pauses: shortest pause, level, air kept around the speech.
struct PauseForm {
    row: usize,
    choice: [usize; 3],
}

const PAUSE_MIN: [f64; 4] = [0.5, 0.8, 1.2, 2.0];
const PAUSE_LEVEL: [(&str, f64); 5] = [
    ("Automatic", 0.0),
    ("−50 dB", -50.0),
    ("−45 dB", -45.0),
    ("−40 dB", -40.0),
    ("−35 dB", -35.0),
];
const PAUSE_KEEP: [f64; 3] = [0.1, 0.2, 0.35];

struct CaptionForm {
    selected: usize,
    language: usize,
}

struct FontPicker {
    clip: usize,
    list: Vec<super::fonts::Family>,
    filter: String,
    selected: usize,
}

impl FontPicker {
    /// Row 0 is the desktop font, then the families under the filter.
    fn visible(&self) -> Vec<Option<&super::fonts::Family>> {
        let f = self.filter.to_lowercase();
        let mut out = vec![None];
        out.extend(
            self.list
                .iter()
                .filter(|x| f.is_empty() || x.name.to_lowercase().contains(&f))
                .map(Some),
        );
        out
    }
}

/// Drawing a rectangle on a still of the clip: the zoom or the hidden part.
struct AreaPicker {
    clip: usize,
    zoom: bool,
    image: Vec<u32>,
    iw: i32,
    ih: i32,
    /// Percent x, y, w, h.
    rect: [f64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FilmRow {
    Loudness,
    Duck,
    Logo,
    LogoCorner,
    LogoSize,
    LogoOpacity,
    CaptionSize,
}

const LOGO_CORNERS: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
const LOGO_SIZES: [f64; 5] = [6.0, 9.0, 12.0, 16.0, 20.0];
const LOGO_OPACITY: [f64; 4] = [0.4, 0.6, 0.8, 1.0];

enum Preview {
    /// The live EDL of the timeline.
    Timeline,
    /// A rendered stretch of film from `offset`.
    Rendered { offset: f64 },
}

struct State {
    theme: Theme,
    settings: Settings,
    project: Project,
    history: History,
    path: Option<PathBuf>,
    saved: Project,
    playhead: f64,
    playing: bool,
    rate: f64,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    selected: Option<usize>,
    /// More clips chosen with Ctrl or Shift: edits apply to all of them.
    picked: std::collections::BTreeSet<usize>,
    /// Clips copied or cut, with their media (and a picture-in-picture's).
    clipboard: Vec<(Clip, Media, Option<Media>)>,
    /// On the selected clip: its title (0) or picture-in-picture (1) lane.
    lane_sel: Option<u8>,
    looping: bool,
    snapping: bool,
    fullscreen: bool,
    music_selected: Option<usize>,
    bin_selected: usize,
    param_row: usize,
    inspector_scroll: i32,
    focus: Focus,
    zoom: f64,
    scroll: f64,
    fitted: bool,
    overlay: Overlay,
    drag: Option<Drag>,
    mouse: (i32, i32),
    status: Option<(String, Instant, bool)>,
    quit_armed: bool,
    job: Option<Job>,
    preview_job: Option<(Job, f64)>,
    preview: Preview,
    timeline: Option<Timeline>,
    loaded_url: String,
    loaded_graph: String,
    cache: Cache,
    mpv: Option<Mpv>,
    work: PathBuf,
    last_autosave: Instant,
    /// mpv loaded a file: wiggle its window once so the picture shows (some
    /// GL drivers draw nothing to a surface until its first resize).
    nudge: bool,
    /// The last left click, for double clicks.
    last_click: Option<(Hit, Instant)>,
    /// A recording started from the editor; the window hides meanwhile.
    recording: Option<Recording>,
    /// The film's sound levels, for the project they were measured on.
    levels: Option<(Project, analyze::Levels)>,
    levels_task: Option<analyze::Task>,
    /// Captions being made, from this snapshot of the project.
    caption_task: Option<(analyze::Task, Project)>,
    caption_progress: Option<(f64, String)>,
    /// The voice treatment mpv plays with now (255: not set yet).
    live_voice: u8,
}

struct Painter {
    cv: Canvas,
    tx: Text,
    hits: Vec<(Rect, Hit)>,
    /// Fonts loaded to show their own names in the font list.
    fonts: std::collections::HashMap<PathBuf, Option<Text>>,
}

struct Layout {
    top: Rect,
    bin: Rect,
    inspector: Rect,
    video: Rect,
    transport: Rect,
    timeline: Rect,
    ruler: Rect,
    lane_title: Rect,
    lane_camera: Rect,
    lane_video: Rect,
    lane_audio: Rect,
    lane_music: Rect,
    track_x: i32,
}

fn layout(w: i32, h: i32, s: i32) -> Layout {
    let top_h = s * 5 / 2;
    let side_l = (s * 17).min(w / 5).max(s * 10);
    let side_r = (s * 21).min(w / 4).max(s * 13);
    let tl_h = (s * 17).min(h * 9 / 20);
    let tr_h = s * 5 / 2;
    let mid_h = (h - top_h - tl_h - tr_h).max(s * 4);
    let top = Rect::new(0, 0, w, top_h);
    let bin = Rect::new(0, top_h, side_l, mid_h + tr_h);
    let inspector = Rect::new(w - side_r, top_h, side_r, mid_h + tr_h);
    let video = Rect::new(side_l, top_h, w - side_l - side_r, mid_h);
    let transport = Rect::new(side_l, top_h + mid_h, w - side_l - side_r, tr_h);
    let timeline = Rect::new(0, h - tl_h, w, tl_h);
    let ruler_h = s * 3 / 2;
    let lanes = tl_h - ruler_h - s / 2;
    let lt = lanes * 12 / 100;
    let lk = lanes * 13 / 100;
    let lv = lanes * 33 / 100;
    let la = lanes * 23 / 100;
    let lm = lanes - lt - lk - lv - la;
    let track_x = s * 6;
    let ruler = Rect::new(track_x, timeline.y, w - track_x, ruler_h);
    let lane_title = Rect::new(track_x, ruler.y + ruler_h + s / 4, w - track_x, lt);
    let lane_camera = Rect::new(track_x, lane_title.y + lt, w - track_x, lk);
    let lane_video = Rect::new(track_x, lane_camera.y + lk, w - track_x, lv);
    let lane_audio = Rect::new(track_x, lane_video.y + lv, w - track_x, la);
    let lane_music = Rect::new(track_x, lane_audio.y + la, w - track_x, lm);
    Layout {
        top,
        bin,
        inspector,
        video,
        transport,
        timeline,
        ruler,
        lane_title,
        lane_camera,
        lane_video,
        lane_audio,
        lane_music,
        track_x,
    }
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    let cs = (t * 100.0).round() as u64;
    let (h, m, s, c) = (cs / 360000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{c:02}")
    } else {
        format!("{m:02}:{s:02}.{c:02}")
    }
}

/// Hours, minutes, seconds and frames: "00:01:23:12".
fn timecode(t: f64, fps: f64) -> String {
    let fps = if fps > 0.0 { fps } else { 30.0 };
    let frames = (t.max(0.0) * fps + 1e-6).floor() as u64;
    let per = fps.round().max(1.0) as u64;
    let secs = (t.max(0.0) + 1e-6).floor() as u64;
    let f = frames
        .saturating_sub((secs as f64 * fps).round() as u64)
        .min(per - 1);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        f
    )
}

pub fn run(files: Vec<PathBuf>) -> Result<(), String> {
    if !Mpv::available() {
        return Err("the editor needs mpv for its preview: install mpv".into());
    }
    let (c, screen_num) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
    let screen = c.setup().roots[screen_num].clone();
    let theme = Theme::load();
    let s = theme.size as i32;
    let (mut w, mut h) = (
        i32::from(screen.width_in_pixels) * 4 / 5,
        i32::from(screen.height_in_pixels) * 4 / 5,
    );
    w = w.max(s * 50);
    h = h.max(s * 36);
    let win = c.generate_id().map_err(|e| e.to_string())?;
    c.create_window(
        screen.root_depth,
        win,
        screen.root,
        0,
        0,
        w as u16,
        h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(theme.background)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::FOCUS_CHANGE,
            ),
    )
    .map_err(|e| e.to_string())?;
    let atom = |name: &str| -> Result<u32, String> {
        Ok(c.intern_atom(false, name.as_bytes())
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom)
    };
    let wm_protocols = atom("WM_PROTOCOLS")?;
    let wm_delete = atom("WM_DELETE_WINDOW")?;
    let net_wm_name = atom("_NET_WM_NAME")?;
    let utf8 = atom("UTF8_STRING")?;
    let net_wm_pid = atom("_NET_WM_PID")?;
    let net_wm_state = atom("_NET_WM_STATE")?;
    let net_wm_state_fullscreen = atom("_NET_WM_STATE_FULLSCREEN")?;
    let _ = c.change_property32(
        PropMode::REPLACE,
        win,
        wm_protocols,
        AtomEnum::ATOM,
        &[wm_delete],
    );
    let _ = c.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"niruclip-edit\0NIRUCLIP\0",
    );
    let _ = c.change_property32(
        PropMode::REPLACE,
        win,
        net_wm_pid,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    );
    // The video area: a child window mpv draws into.
    let video = c.generate_id().map_err(|e| e.to_string())?;
    let l = layout(w, h, s);
    c.create_window(
        screen.root_depth,
        video,
        win,
        l.video.x as i16,
        l.video.y as i16,
        l.video.w.max(1) as u16,
        l.video.h.max(1) as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().background_pixel(0),
    )
    .map_err(|e| e.to_string())?;
    let gc = c.generate_id().map_err(|e| e.to_string())?;
    c.create_gc(gc, win, &CreateGCAux::new())
        .map_err(|e| e.to_string())?;
    c.map_window(win).map_err(|e| e.to_string())?;
    c.flush().map_err(|e| e.to_string())?;

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let work = runtime.join(format!("niruclip-edit-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);

    let mut p = Painter {
        cv: Canvas::new(&c, w, h),
        tx: Text::new(theme.font.as_deref())?,
        hits: Vec::new(),
        fonts: std::collections::HashMap::new(),
    };
    let mut st = State {
        theme,
        settings: Settings::load(),
        project: Project::default(),
        history: History::default(),
        path: None,
        saved: Project::default(),
        playhead: 0.0,
        playing: false,
        rate: 1.0,
        mark_in: None,
        mark_out: None,
        selected: None,
        picked: std::collections::BTreeSet::new(),
        clipboard: Vec::new(),
        lane_sel: None,
        looping: false,
        snapping: true,
        fullscreen: false,
        music_selected: None,
        bin_selected: 0,
        param_row: 0,
        inspector_scroll: 0,
        focus: Focus::Timeline,
        zoom: 40.0,
        scroll: 0.0,
        fitted: false,
        overlay: Overlay::None,
        drag: None,
        mouse: (0, 0),
        status: None,
        quit_armed: false,
        job: None,
        preview_job: None,
        preview: Preview::Timeline,
        timeline: None,
        loaded_url: String::new(),
        loaded_graph: String::new(),
        cache: Cache::new(),
        mpv: None,
        work: work.clone(),
        last_autosave: Instant::now(),
        nudge: false,
        last_click: None,
        recording: None,
        levels: None,
        levels_task: None,
        caption_task: None,
        caption_progress: None,
        live_voice: 255,
    };
    for f in files {
        if probe::is_project(&f) {
            st.open_project(&f);
        } else {
            st.import(&[f], true);
        }
    }
    st.saved = st.project.clone();

    let keymap = Keymap::load(&c)?;
    let mut video_mapped = false;
    let mut main_hidden = false;
    let mut video_geometry = Rect::default();
    let mut full = false;
    let mut title = String::new();
    let fd = {
        use std::os::fd::AsRawFd;
        c.stream().as_raw_fd()
    };
    let mut dirty = true;
    let mut last_draw = Instant::now();
    let result = 'main: loop {
        // Window title.
        let want = format!(
            "{}{} · NIRUCLIP",
            if st.project != st.saved { "● " } else { "" },
            st.path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into())
        );
        if want != title {
            title = want;
            let _ = c.change_property8(PropMode::REPLACE, win, net_wm_name, utf8, title.as_bytes());
            let _ = c.change_property8(
                PropMode::REPLACE,
                win,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            );
        }
        // X events.
        while let Some(ev) = c.poll_for_event().map_err(|e| e.to_string())? {
            dirty = true;
            match ev {
                XEvent::ConfigureNotify(e) if e.window == win => {
                    let (nw, nh) = (i32::from(e.width), i32::from(e.height));
                    if nw != w || nh != h {
                        w = nw;
                        h = nh;
                        p.cv.resize(&c, w, h);
                    }
                }
                XEvent::ClientMessage(e)
                    if e.format == 32
                        && e.data.as_data32()[0] == wm_delete
                        && st.request_quit() =>
                {
                    break 'main Ok(());
                }
                XEvent::KeyPress(e) => {
                    let state = u16::from(e.state);
                    let sym = keymap.sym(e.detail, state & SHIFT != 0);
                    if st.key(sym, state) {
                        break 'main Ok(());
                    }
                }
                XEvent::ButtonPress(e) => {
                    st.press(
                        &p.hits,
                        &layout(w, h, s),
                        e.detail,
                        u16::from(e.state),
                        i32::from(e.event_x),
                        i32::from(e.event_y),
                    );
                }
                XEvent::ButtonRelease(e) => {
                    st.release(
                        &layout(w, h, s),
                        e.detail,
                        i32::from(e.event_x),
                        i32::from(e.event_y),
                    );
                }
                XEvent::MotionNotify(e) => {
                    st.motion(&layout(w, h, s), i32::from(e.event_x), i32::from(e.event_y));
                }
                _ => {}
            }
        }
        // mpv, caches and jobs.
        if st.pump() {
            dirty = true;
        }
        let l = layout(w, h, s);
        if !st.fitted && st.project.duration() > 0.0 {
            st.fit(&l);
            st.fitted = true;
        }
        // The whole window hides while a recording from here runs.
        let want_hidden = st.recording.is_some();
        if want_hidden != main_hidden {
            if want_hidden {
                let _ = c.unmap_window(win);
            } else {
                let _ = c.map_window(win);
                st.nudge = true;
            }
            main_hidden = want_hidden;
            let _ = c.flush();
        }
        // Keep the video window where the layout says, hidden when empty or
        // behind an overlay that covers it.
        let show_video = !st.project.clips.is_empty() && matches!(st.overlay, Overlay::None);
        let mut refresh = false;
        if show_video != video_mapped {
            if show_video {
                let _ = c.map_window(video);
                refresh = true;
                if st.mpv.is_none() {
                    // mpv starts once its window is on screen: a surface made
                    // on a hidden window may never show a picture.
                    let _ = c.sync();
                    match Mpv::start(video, &runtime) {
                        Ok(m) => {
                            st.mpv = Some(m);
                            st.live_voice = 255;
                            st.reload(true);
                        }
                        Err(e) => st.warn(e),
                    }
                }
            } else {
                let _ = c.unmap_window(video);
            }
            video_mapped = show_video;
            dirty = true;
        }
        // Full screen: the window asks for the whole screen and the picture
        // takes all of it.
        if st.fullscreen != full {
            full = st.fullscreen;
            let ev = ClientMessageEvent::new(
                32,
                win,
                net_wm_state,
                [u32::from(full), net_wm_state_fullscreen, 0, 1, 0],
            );
            let _ = c.send_event(
                false,
                screen.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            );
            let _ = c.flush();
            if full {
                st.say("Full screen: click plays or pauses, Esc or a double click comes back");
            }
        }
        let video_rect = if full { Rect::new(0, 0, w, h) } else { l.video };
        if video_rect != video_geometry {
            video_geometry = video_rect;
            refresh = true;
            let _ = c.configure_window(
                video,
                &ConfigureWindowAux::new()
                    .x(video_rect.x)
                    .y(video_rect.y)
                    .width(video_rect.w.max(1) as u32)
                    .height(video_rect.h.max(1) as u32),
            );
        }
        if st.nudge && video_mapped {
            st.nudge = false;
            let _ = c.configure_window(
                video,
                &ConfigureWindowAux::new().width((video_rect.w - 1).max(1) as u32),
            );
            let _ = c.sync();
            std::thread::sleep(Duration::from_millis(30));
            let _ = c.configure_window(
                video,
                &ConfigureWindowAux::new().width(video_rect.w.max(1) as u32),
            );
            refresh = true;
        }
        if refresh {
            // A paused mpv does not draw again by itself when its window
            // is shown or resized: show the frame once more.
            let _ = c.flush();
            if !st.playing {
                let at = st.playhead;
                st.seek(at);
            }
        }
        st.autosave();
        // Draw at most ~60 times a second.
        let gap = if st.playing && st.drag.is_none() {
            32
        } else {
            15
        };
        if dirty && last_draw.elapsed() >= Duration::from_millis(gap) {
            p.hits.clear();
            draw(&mut p, &mut st, &l, w, h);
            p.cv.present(&c, win, gc, screen.root_depth, 0, 0, w, h);
            let _ = c.flush();
            dirty = false;
            last_draw = Instant::now();
        }
        let timeout = if dirty {
            16
        } else if st.recording.is_some() {
            100
        } else if st.playing || st.job.is_some() || st.preview_job.is_some() {
            // The playhead moves: 30 redraws a second are plenty.
            33
        } else if st.status.is_some() {
            100
        } else {
            500
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut pfd, 1, timeout);
        }
        if st
            .status
            .as_ref()
            .is_some_and(|(_, t, _)| t.elapsed() > Duration::from_secs(6))
        {
            st.status = None;
            dirty = true;
        }
        if st.playing || st.job.is_some() || st.preview_job.is_some() {
            dirty = true;
        }
    };
    st.mpv = None;
    let _ = std::fs::remove_dir_all(&work);
    result
}

struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

impl Keymap {
    fn load(c: &RustConnection) -> Result<Self, String> {
        let setup = c.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let r = c
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            min,
            per: usize::from(r.keysyms_per_keycode),
            syms: r.keysyms,
        })
    }

    fn sym(&self, code: u8, shift: bool) -> u32 {
        let i = usize::from(code.saturating_sub(self.min)) * self.per;
        let base = self.syms.get(i).copied().unwrap_or(0);
        let shifted = self.syms.get(i + 1).copied().unwrap_or(0);
        if shift && shifted != 0 {
            shifted
        } else {
            base
        }
    }
}

fn sym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        0x0100_0000..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        _ => None,
    }
}

impl State {
    fn say(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now(), false));
    }

    fn warn(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now(), true));
    }

    fn change(&mut self, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        if self.project != before {
            self.history.record(&before);
            self.after_change();
        }
    }

    fn after_change(&mut self) {
        self.project.repair();
        if let Some(i) = self.selected {
            if i >= self.project.clips.len() {
                self.selected = self.project.clips.len().checked_sub(1);
            }
        }
        if let Some(j) = self.music_selected {
            if j >= self.project.music.len() {
                self.music_selected = None;
            }
        }
        let n = self.project.clips.len();
        self.picked.retain(|i| *i < n);
        let d = self.project.duration();
        self.playhead = self.playhead.min(d);
        self.preview = Preview::Timeline;
        self.reload(false);
    }

    /// Load the timeline in mpv when it changed.
    fn reload(&mut self, force: bool) {
        self.timeline = mpv::timeline(&self.project, &mut self.cache);
        let (url, graph) = self
            .timeline
            .as_ref()
            .map(|t| (t.url.clone(), t.graph.clone()))
            .unwrap_or_default();
        let mute = self.project.mute_sound;
        if let Some(m) = &mut self.mpv {
            m.mute(mute);
        }
        if !force && url == self.loaded_url {
            if graph != self.loaded_graph {
                // Only the camera's place changed: new filters, same film.
                self.loaded_graph = graph.clone();
                if let Some(m) = &mut self.mpv {
                    m.set_graph(&graph);
                }
                if !self.playing {
                    let at = self.playhead;
                    self.seek(at);
                }
            }
            return;
        }
        self.loaded_url = url.clone();
        self.loaded_graph = graph.clone();
        let at = self
            .timeline
            .as_ref()
            .map(|t| t.to_edl(self.playhead))
            .unwrap_or(0.0);
        let playing = self.playing;
        if let Some(m) = &mut self.mpv {
            if url.is_empty() {
                m.stop();
            } else {
                m.load(&url, at, !playing, &graph);
            }
        }
    }

    fn seek(&mut self, film: f64) {
        let d = self.project.duration();
        self.playhead = film.clamp(0.0, d);
        match &self.preview {
            Preview::Rendered { offset, .. } => {
                let local = self.playhead - offset;
                if local < 0.0 {
                    self.preview = Preview::Timeline;
                    self.reload(true);
                } else if let Some(m) = &mut self.mpv {
                    m.seek(local);
                }
            }
            Preview::Timeline => {
                let at = self
                    .timeline
                    .as_ref()
                    .map(|t| t.to_edl(self.playhead))
                    .unwrap_or(0.0);
                if let Some(m) = &mut self.mpv {
                    m.seek(at);
                }
            }
        }
    }

    fn play(&mut self, on: bool) {
        if self.project.clips.is_empty() {
            return;
        }
        if on && self.playhead >= self.project.duration() - 0.05 {
            self.seek(0.0);
        }
        self.playing = on;
        if let Some(m) = &mut self.mpv {
            m.pause(!on);
            if !on {
                self.rate = 1.0;
                m.speed(1.0);
            }
        }
    }

    fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        if let Some(m) = &mut self.mpv {
            m.speed(rate);
        }
        self.play(true);
    }

    /// Handle what arrived from mpv, the caches and the jobs.
    fn pump(&mut self) -> bool {
        let mut any = self.cache.collect();
        if self.cache.take_proxy_ready()
            && self.drag.is_none()
            && matches!(self.preview, Preview::Timeline)
        {
            // A side file for the live camera is ready: play with it.
            self.reload(false);
        }
        let mut events = Vec::new();
        if let Some(m) = &self.mpv {
            while let Ok(e) = m.events.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            any = true;
            match e {
                mpv::Event::Time(t) => {
                    if matches!(self.drag, Some(Drag::Scrub)) {
                        continue;
                    }
                    self.playhead = match &self.preview {
                        Preview::Rendered { offset, .. } => offset + t,
                        Preview::Timeline => self
                            .timeline
                            .as_ref()
                            .map(|tl| tl.to_film(t))
                            .unwrap_or(0.0),
                    };
                    if self.looping && self.playing && matches!(self.preview, Preview::Timeline) {
                        let (a, b) = self.loop_range();
                        if self.playhead >= b - 0.03 || self.playhead < a - 0.5 {
                            self.seek(a);
                        }
                    }
                }
                mpv::Event::Pause(p) => {
                    self.playing = !p;
                    if p {
                        self.nudge = true;
                    }
                }
                mpv::Event::Ended if self.looping && matches!(self.preview, Preview::Timeline) => {
                    let (a, _) = self.loop_range();
                    self.seek(a);
                    if let Some(m) = &mut self.mpv {
                        m.pause(false);
                    }
                    self.playing = true;
                }
                mpv::Event::Ended => {
                    if self.playing {
                        self.playing = false;
                        if let Some(m) = &mut self.mpv {
                            m.pause(true);
                        }
                    }
                }
                mpv::Event::Loaded => self.nudge = true,
            }
        }
        if any {
            self.sync_voice();
        }
        // Listening for pauses.
        if let Some(task) = &self.levels_task {
            let mut done = None;
            while let Ok(msg) = task.rx.try_recv() {
                match msg {
                    analyze::Msg::Levels(l) => done = Some(Ok(l)),
                    analyze::Msg::Failed(e) => done = Some(Err(e)),
                    _ => {}
                }
            }
            if let Some(result) = done {
                any = true;
                self.levels_task = None;
                match result {
                    Ok(l) => self.levels = Some((self.project.clone(), l)),
                    Err(e) => self.warn(format!("Could not listen to the film: {e}")),
                }
            }
        }
        // Captions.
        if let Some((task, _)) = &self.caption_task {
            let mut done = None;
            while let Ok(msg) = task.rx.try_recv() {
                any = true;
                match msg {
                    analyze::Msg::Progress(f, text) => self.caption_progress = Some((f, text)),
                    analyze::Msg::Captions(list) => done = Some(Ok(list)),
                    analyze::Msg::Failed(e) => done = Some(Err(e)),
                    analyze::Msg::Levels(_) => {}
                }
            }
            if let Some(result) = done {
                let (_, mut snapshot) = self.caption_task.take().expect("a caption task");
                self.caption_progress = None;
                match result {
                    Ok(list) if list.is_empty() => self.warn("No words were recognised."),
                    Ok(list) => self.take_captions(&mut snapshot, &list),
                    Err(e) => {
                        self.warn(format!("Captions: {e}"));
                        notify("Captions failed", &e);
                    }
                }
            }
        }
        // A recording started from here.
        if let Some(r) = &mut self.recording {
            if let Some(done) = r.poll() {
                any = true;
                let place = r.place_at;
                self.recording = None;
                match done {
                    record::Done::File(p) => self.add_recording(&p, place),
                    record::Done::Cancelled => self.say("Recording cancelled"),
                    record::Done::Failed(e) => self.warn(format!("Recording failed: {e}")),
                }
            }
        }
        // Export.
        if let Some(job) = &mut self.job {
            any = true;
            if let Some(result) = job.poll() {
                let out = job.output.clone();
                let took = job.started.elapsed().as_secs();
                self.job = None;
                match result {
                    Ok(()) => {
                        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                        self.say(format!(
                            "Exported {} ({}) in {} s",
                            out.display(),
                            human_size(size),
                            took
                        ));
                        notify("Film exported", &out.display().to_string());
                    }
                    Err(e) => {
                        self.warn(format!("Export failed: {e}"));
                        notify("Export failed", &e);
                    }
                }
            }
        }
        if let Some((job, offset)) = &mut self.preview_job {
            any = true;
            if let Some(result) = job.poll() {
                let file = job.output.clone();
                let offset = *offset;
                self.preview_job = None;
                match result {
                    Ok(()) => {
                        self.preview = Preview::Rendered { offset };
                        self.loaded_url.clear();
                        self.loaded_graph.clear();
                        let at = (self.playhead - offset).max(0.0);
                        if let Some(m) = &mut self.mpv {
                            m.load(&file.display().to_string(), at, false, "");
                        }
                        self.playing = true;
                        self.say("Rendered preview with effects and transitions. Any edit returns to the live timeline.");
                    }
                    Err(e) => self.warn(format!("Preview failed: {e}")),
                }
            }
        }
        any
    }

    fn autosave(&mut self) {
        if self.last_autosave.elapsed() < Duration::from_secs(20) {
            return;
        }
        self.last_autosave = Instant::now();
        if self.project.clips.is_empty() || self.project == self.saved {
            return;
        }
        let _ = self.project.save(&autosave_path());
    }

    fn request_quit(&mut self) -> bool {
        if self.project == self.saved || self.project.clips.is_empty() || self.quit_armed {
            if self.project.clips.is_empty() || self.project == self.saved {
                let _ = std::fs::remove_file(autosave_path());
            }
            return true;
        }
        self.quit_armed = true;
        self.warn("Unsaved changes: Ctrl+S saves, close again to quit without saving.");
        false
    }

    // ── Files ───────────────────────────────────────────────────────────

    fn import(&mut self, files: &[PathBuf], to_timeline: bool) {
        let mut added = 0;
        let mut errors = Vec::new();
        let before = self.project.clone();
        for f in files {
            match probe::probe(f) {
                Ok(media) => {
                    self.cache.want(&media);
                    let i = self.project.add_media(media);
                    if to_timeline {
                        let m = &self.project.media[i];
                        if m.has_video || m.has_audio {
                            self.project.append(i, None);
                        }
                    }
                    self.bin_selected = i;
                    added += 1;
                }
                Err(e) => errors.push(e),
            }
        }
        if self.project != before {
            self.history.record(&before);
            self.after_change();
            self.fitted = false;
        }
        if let Some(e) = errors.first() {
            self.warn(e.clone());
        } else if added > 0 {
            self.say(format!(
                "Imported {added} file{}",
                if added == 1 { "" } else { "s" }
            ));
        }
    }

    fn add_music(&mut self, file: &Path) {
        match probe::probe(file) {
            Ok(media) if media.has_audio => {
                self.cache.want(&media);
                let at = self.playhead;
                let length = media.duration;
                self.change(|p| {
                    let i = p.add_media(media);
                    p.music.push(AudioClip {
                        media: i,
                        at,
                        start: 0.0,
                        end: length,
                        volume: 0.5,
                        fade_in: 0.0,
                        fade_out: 2.0,
                    });
                });
                self.music_selected = Some(self.project.music.len() - 1);
                self.say("Music added at the playhead. Drag it in the Music lane.");
            }
            Ok(_) => self.warn("That file has no sound."),
            Err(e) => self.warn(e),
        }
    }

    fn open_project(&mut self, f: &Path) {
        match Project::load(f) {
            Ok(project) => {
                for m in &project.media {
                    self.cache.want(m);
                }
                let missing: Vec<String> = project
                    .media
                    .iter()
                    .filter(|m| !m.path.exists())
                    .map(|m| m.name())
                    .collect();
                self.project = project;
                self.saved = self.project.clone();
                self.history = History::default();
                self.path = Some(f.to_path_buf());
                self.selected = None;
                self.music_selected = None;
                self.playhead = 0.0;
                self.mark_in = None;
                self.mark_out = None;
                self.fitted = false;
                self.settings.remember(f);
                self.settings.save();
                self.after_change();
                if missing.is_empty() {
                    self.say(format!("Opened {}", f.display()));
                } else {
                    self.warn(format!("Missing media: {}", missing.join(", ")));
                }
            }
            Err(e) => self.warn(e),
        }
    }

    fn save(&mut self, as_new: bool) {
        match (&self.path, as_new) {
            (Some(path), false) => {
                let path = path.clone();
                self.save_to(&path);
            }
            _ => {
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().into_owned())
                    .map(|n| n.trim_end_matches(".niruclip").to_string())
                    .unwrap_or_else(|| self.suggested_name());
                let dir = self
                    .path
                    .as_ref()
                    .and_then(|p| p.parent())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.settings.media_folder());
                self.overlay = Overlay::Browser(Browser::new(Purpose::SaveAs, &dir, &name));
            }
        }
    }

    /// A name from the first clip ("recording-edit"), else "film".
    fn suggested_name(&self) -> String {
        self.project
            .clips
            .first()
            .and_then(|c| self.project.media[c.media].path.file_stem())
            .map(|s| format!("{}-edit", s.to_string_lossy()))
            .unwrap_or_else(|| "film".into())
    }

    fn save_to(&mut self, path: &Path) {
        let mut path = path.to_path_buf();
        if !probe::is_project(&path) {
            let mut name = path.as_os_str().to_os_string();
            name.push(".niruclip");
            path = PathBuf::from(name);
        }
        match self.project.save(&path) {
            Ok(()) => {
                self.saved = self.project.clone();
                self.path = Some(path.clone());
                self.quit_armed = false;
                self.settings.remember(&path);
                self.settings.save();
                let _ = std::fs::remove_file(autosave_path());
                self.say(format!("Saved {}", path.display()));
            }
            Err(e) => self.warn(e),
        }
    }

    fn browse(&mut self, purpose: Purpose) {
        let dir = match purpose {
            Purpose::ExportFolder => self.settings.export_folder(),
            _ => self.settings.media_folder(),
        };
        self.overlay = Overlay::Browser(Browser::new(purpose, &dir, ""));
    }

    fn browser_accept(&mut self, pick_dir: bool) {
        let Overlay::Browser(b) = &mut self.overlay else {
            return;
        };
        let purpose = b.purpose;
        let current = b.current();
        if purpose == Purpose::SaveAs {
            if let Some(e) = &current {
                if e.dir && b.filter.is_empty() {
                    b.enter(e.path.clone());
                    return;
                }
            }
            let name = b.filter.trim().to_string();
            if name.is_empty() {
                return;
            }
            let path = b.dir.join(name);
            self.overlay = Overlay::None;
            self.save_to(&path);
            return;
        }
        if purpose.folders_only() {
            let dir = match (&current, pick_dir) {
                (Some(e), false) if e.dir => {
                    b.enter(e.path.clone());
                    return;
                }
                _ => b.dir.clone(),
            };
            match purpose {
                Purpose::MediaFolder => self.settings.media_folder = Some(dir.clone()),
                _ => self.settings.export_folder = Some(dir.clone()),
            }
            self.settings.save();
            self.overlay = Overlay::Settings;
            self.say(format!("Folder set to {}", dir.display()));
            return;
        }
        let Some(entry) = current else { return };
        if entry.dir {
            b.enter(entry.path);
            return;
        }
        let mut files = std::mem::take(&mut b.marked);
        if files.is_empty() {
            files.push(entry.path.clone());
        }
        let dir = b.dir.clone();
        self.overlay = Overlay::None;
        if purpose == Purpose::Logo {
            let path = entry.path.clone();
            self.change(|p| {
                let mut logo = super::project::Logo::new(path);
                if let Some(old) = &p.logo {
                    logo.corner = old.corner;
                    logo.size = old.size;
                    logo.opacity = old.opacity;
                }
                p.logo = Some(logo);
            });
            self.overlay = Overlay::Film { row: 2 };
            self.say("Logo added over the whole film. P shows it rendered.");
            return;
        }
        if purpose != Purpose::Music {
            self.settings.media_folder = Some(dir);
            self.settings.save();
        }
        match purpose {
            Purpose::Open => {
                if probe::is_project(&entry.path) {
                    self.open_project(&entry.path);
                } else {
                    let to_timeline = self.project.clips.is_empty();
                    self.import(&files, to_timeline);
                }
            }
            Purpose::Import => self.import(&files, self.project.clips.is_empty()),
            Purpose::Music => self.add_music(&entry.path),
            _ => {}
        }
    }

    // ── Record ──────────────────────────────────────────────────────────

    fn open_record(&mut self) {
        if self.recording.is_some() {
            self.warn("A recording is already running.");
            return;
        }
        self.play(false);
        let choices = self.settings.record.clone().unwrap_or_default();
        self.overlay = Overlay::Record(record::Form::new(choices));
    }

    fn start_record(&mut self) {
        let Overlay::Record(f) = &self.overlay else {
            return;
        };
        let c = f.choices.clone();
        self.settings.record = Some(c.clone());
        self.settings.save();
        let place = (c.place == 1 && !self.project.clips.is_empty()).then_some(self.playhead);
        let wait = record::pre_delay(&c);
        if wait > 0 {
            let how = if c.what == 1 {
                "then click the window to record"
            } else {
                "then drag the area to record"
            };
            notify(
                &format!("Recording in {wait} s"),
                &format!("Switch to the right workspace, {how}."),
            );
        }
        self.play(false);
        self.recording = Some(Recording::new(&c, place, &self.work));
        self.overlay = Overlay::None;
    }

    /// Put a finished recording on the timeline.
    fn add_recording(&mut self, path: &Path, place: Option<f64>) {
        let mic = self
            .settings
            .record
            .as_ref()
            .is_some_and(|c| c.sound == 1 || c.sound == 3);
        let media = match probe::probe(path) {
            Ok(m) => m,
            Err(e) => {
                self.warn(e);
                return;
            }
        };
        self.cache.want(&media);
        let mut index = 0;
        self.change(|p| {
            let i = p.add_media(media);
            let at = match place {
                Some(t) => {
                    p.split(t);
                    let starts = p.starts();
                    starts
                        .iter()
                        .position(|s| *s >= t - 1e-6)
                        .unwrap_or(p.clips.len())
                }
                None => p.clips.len(),
            };
            index = p.append(i, Some(at));
            // Speech recorded here gets the voice treatment from the start.
            let m = &p.media[i];
            if m.has_audio && (m.split_tracks().is_some() || mic) {
                p.clips[index].voice = 1;
            }
        });
        self.selected = Some(index);
        self.bin_selected = self.project.media.len().saturating_sub(1);
        self.fitted = false;
        let start = self.project.starts().get(index).copied().unwrap_or(0.0);
        self.seek(start);
        self.say(format!("Recording added: {}", path.display()));
    }

    // ── Export ──────────────────────────────────────────────────────────

    fn open_export(&mut self) {
        if self.project.clips.is_empty() {
            self.warn("Nothing to export: add a video first.");
            return;
        }
        let format = self
            .settings
            .format
            .as_deref()
            .and_then(Format::from_name)
            .and_then(|f| Format::ALL.iter().position(|x| *x == f))
            .unwrap_or(0);
        let height = HEIGHTS
            .iter()
            .position(|h| Some(*h) == self.settings.height)
            .unwrap_or(0);
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.suggested_name());
        let limit = export::LIMITS
            .iter()
            .position(|l| Some(*l) == self.settings.limit_mb)
            .unwrap_or(0);
        let captions = self
            .settings
            .captions
            .map(|c| c as usize)
            .unwrap_or(if self.project.caption_count() > 0 {
                2
            } else {
                0
            })
            .min(export::CAPTIONS.len() - 1);
        self.overlay = Overlay::Export(ExportForm {
            format,
            height,
            name,
            limit,
            captions,
        });
    }

    fn start_export(&mut self) {
        let Overlay::Export(form) = &self.overlay else {
            return;
        };
        if self.job.is_some() {
            self.warn("An export is already running.");
            return;
        }
        let format = Format::ALL[form.format];
        let height = HEIGHTS[form.height];
        let (form_limit, form_captions) = (form.limit, form.captions);
        let form = ExportForm {
            format: form.format,
            height: form.height,
            name: form.name.clone(),
            limit: form_limit,
            captions: form_captions,
        };
        let folder = self.settings.export_folder();
        let _ = std::fs::create_dir_all(&folder);
        let name = form.name.trim();
        let name = if name.is_empty() { "film" } else { name };
        let output = export::free_name(&folder, name, format.extension());
        self.settings.format = Some(format.name().into());
        self.settings.height = Some(height);
        self.settings.save();
        let range = match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) if (b - a).abs() > 0.1 => Some((a.min(b), a.max(b))),
            _ => None,
        };
        let limit_mb = export::LIMITS[form.limit];
        let captions = form.captions as u8;
        self.settings.limit_mb = Some(limit_mb);
        if self.project.caption_count() > 0 {
            self.settings.captions = Some(captions);
        }
        self.settings.save();
        let mut settings = export::Settings::new(format, output.clone(), self.work.clone());
        settings.height = height;
        settings.font = super::gfx::font_path(self.theme.font.as_deref()).map(PathBuf::from);
        settings.range = range;
        settings.limit_mb = limit_mb;
        settings.captions = captions;
        match export::plan(&self.project, &settings)
            .and_then(|plan| Job::start(plan, output.clone()))
        {
            Ok(job) => {
                self.job = Some(job);
                self.overlay = Overlay::None;
                self.say(format!(
                    "Exporting {}{}",
                    output.display(),
                    if range.is_some() {
                        " (the marked part)"
                    } else {
                        ""
                    }
                ));
            }
            Err(e) => self.warn(e),
        }
    }

    /// Render a short stretch around the playhead (or the marked part) with
    /// every effect and transition, and play it.
    fn render_preview(&mut self) {
        if self.project.clips.is_empty() {
            return;
        }
        if let Some((mut job, _)) = self.preview_job.take() {
            job.cancel();
        }
        let d = self.project.duration();
        let (a, b) = match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) if (b - a).abs() > 0.1 => (a.min(b), a.max(b)),
            _ => {
                let a = (self.playhead - 1.0).max(0.0);
                (a, (a + 8.0).min(d))
            }
        };
        let output = self.work.join(format!("preview-{}.mp4", now_ms()));
        let mut settings = export::Settings::new(Format::Mp4, output.clone(), self.work.clone());
        settings.height = 720.min(self.project.height.max(2));
        settings.font = super::gfx::font_path(self.theme.font.as_deref()).map(PathBuf::from);
        settings.range = Some((a, b));
        settings.draft = true;
        // The preview shows captions as they would be burnt in.
        settings.captions = u8::from(self.project.caption_count() > 0);
        match export::plan(&self.project, &settings).and_then(|plan| Job::start(plan, output)) {
            Ok(job) => {
                self.preview_job = Some((job, a));
                self.play(false);
                self.say("Rendering a preview with effects…");
            }
            Err(e) => self.warn(e),
        }
    }

    // ── Editing helpers ─────────────────────────────────────────────────

    fn target(&self) -> Option<usize> {
        self.selected
            .filter(|i| *i < self.project.clips.len())
            .or_else(|| self.project.locate(self.playhead).map(|(i, _)| i))
    }

    /// The settings rows of the clip the panel shows.
    fn params(&self) -> Vec<Param> {
        match self.target() {
            Some(i) => {
                let c = &self.project.clips[i];
                let mut v = clip_params(c, &self.project.media[c.media]);
                if i == 0 {
                    v.retain(|p| *p != Param::SoundCut);
                }
                v
            }
            None => Vec::new(),
        }
    }

    fn adjust(&mut self, row: usize, delta: f64) {
        if let Some(j) = self.music_selected {
            let Some(param) = MUSIC_PARAMS.get(row).copied() else {
                return;
            };
            let (_, _, step) = param.range();
            self.change(|p| {
                let m = &mut p.music[j];
                param.set_music(m, param.get_music(m) + delta * step);
            });
            return;
        }
        let Some(i) = self.target() else { return };
        let Some(param) = self.params().get(row).copied() else {
            return;
        };
        if param.opens() {
            self.open_param(i, param);
            return;
        }
        if param == Param::Pip {
            self.cycle_pip(i, delta);
            return;
        }
        let (lo, hi, step) = param.limits(&self.project.clips[i]);
        let all = self.also(i, param);
        self.change(|p| {
            let c = &mut p.clips[i];
            let mut v = param.get(c) + delta * step;
            // Choices wrap around.
            if !param.slider() && step >= 1.0 {
                let n = hi - lo + 1.0;
                v = lo + (v - lo).rem_euclid(n);
            }
            param.set(c, v);
            // The other chosen clips take the same value.
            for j in all {
                param.set(&mut p.clips[j], v);
            }
        });
        if param == Param::Transition && i == 0 && self.project.clips[0].transition.is_some() {
            self.say("The first clip has nothing to blend from: use Fade in instead.");
        }
        if param == Param::Voice {
            self.live_voice = 255;
            self.sync_voice();
        }
    }

    /// Enter on a row: open its dialog, take a time from the playhead, or
    /// step a choice.
    fn enter_param(&mut self, row: usize) {
        let Some(i) = self.target() else { return };
        let Some(param) = self.params().get(row).copied() else {
            return;
        };
        if param.opens() {
            self.open_param(i, param);
        } else if param.is_time() {
            let start = self.project.starts()[i];
            let local = (self.playhead - start).clamp(0.0, self.project.clips[i].length());
            let e = &self.project.clips[i].effects;
            let value = match param {
                Param::ZoomLength => local - e.zoom_start,
                Param::HideLength => local - e.hide_start,
                Param::TextLength => local - e.text_start,
                _ => local,
            };
            if value <= 0.0 {
                self.warn("Move the playhead past the start first.");
                return;
            }
            self.change(|p| param.set(&mut p.clips[i], value));
            self.say(format!("{} set from the playhead", param.label()));
        } else {
            self.adjust(row, 1.0);
        }
    }

    /// The picture-in-picture shows the clip's camera, nothing, or one of
    /// the project's pictures and films, in turn.
    fn cycle_pip(&mut self, i: usize, delta: f64) {
        let c = &self.project.clips[i];
        let m = &self.project.media[c.media];
        let mut choices: Vec<(u8, Option<usize>)> = Vec::new();
        if m.own_camera().is_some() && !m.still {
            choices.push((0, None));
        }
        choices.push((1, None));
        for (k, x) in self.project.media.iter().enumerate() {
            if k != c.media && x.has_video && !x.is_card() {
                choices.push((2, Some(k)));
            }
        }
        if choices.len() == 1 {
            self.warn("Import a picture or a film first (Ctrl+I): it can then show in the corner.");
            return;
        }
        let now = if pip_on(c, m) {
            choices
                .iter()
                .position(|(mode, media)| {
                    *mode == c.pip.mode && (*mode != 2 || *media == c.pip.media)
                })
                .unwrap_or(0)
        } else {
            choices.iter().position(|(mode, _)| *mode == 1).unwrap_or(0)
        };
        let n = choices.len() as i64;
        let next = (now as i64 + if delta < 0.0 { -1 } else { 1 }).rem_euclid(n) as usize;
        let (mode, media) = choices[next];
        self.change(|p| {
            let pip = &mut p.clips[i].pip;
            pip.mode = mode;
            pip.media = media;
        });
    }

    fn open_param(&mut self, i: usize, param: Param) {
        match param {
            Param::PlaceText => self.open_place(i, 0),
            Param::PlacePip => self.open_place(i, 1),
            Param::Title => self.edit_title(i),
            Param::Font => self.open_fonts(i),
            Param::ZoomArea => self.open_area(i, true),
            Param::HideArea => self.open_area(i, false),
            _ => {}
        }
    }

    fn set_param(&mut self, row: usize, value: f64) {
        if let Some(j) = self.music_selected {
            if let Some(param) = MUSIC_PARAMS.get(row).copied() {
                self.change(|p| param.set_music(&mut p.music[j], value));
            }
            return;
        }
        let Some(i) = self.target() else { return };
        if let Some(param) = self.params().get(row).copied() {
            let all = self.also(i, param);
            self.change(|p| {
                param.set(&mut p.clips[i], value);
                for j in all {
                    param.set(&mut p.clips[j], value);
                }
            });
        }
    }

    /// The other chosen clips a setting applies to as well.
    fn also(&self, i: usize, param: Param) -> Vec<usize> {
        if param.opens() || matches!(param, Param::Pip | Param::Title) {
            return Vec::new();
        }
        self.selection()
            .into_iter()
            .filter(|&j| j != i)
            .filter(|&j| {
                let c = &self.project.clips[j];
                clip_params(c, &self.project.media[c.media]).contains(&param)
            })
            .collect()
    }

    fn edit_title(&mut self, i: usize) {
        let text = self.project.clips[i].effects.text.replace('\n', " | ");
        self.overlay = Overlay::TextInput {
            title: "Title text (| starts a new line, empty removes it)".into(),
            text,
            target: TextTarget::Title(i),
        };
    }

    /// A title card after the clip under the playhead.
    fn add_card(&mut self) {
        let at = match self.project.locate(self.playhead) {
            Some((i, _)) => i + 1,
            None => 0,
        };
        let mut index = 0;
        self.change(|p| {
            let m = p.card();
            let mut c = Clip::new(m, 0.0, 4.0);
            c.effects.text = "Title".into();
            c.effects.text_position = 4;
            c.effects.text_size = 10.0;
            c.effects.text_look = 3;
            c.effects.font_style = 1;
            c.fade_in = 0.5;
            c.fade_out = 0.5;
            let at = at.min(p.clips.len());
            p.clips.insert(at, c);
            index = at;
        });
        self.selected = Some(index);
        self.music_selected = None;
        self.fitted = false;
        let start = self.project.starts()[index];
        self.seek(start);
        // Typing replaces the placeholder; Esc keeps it.
        self.overlay = Overlay::TextInput {
            title: "Text on the card (| starts a new line)".into(),
            text: String::new(),
            target: TextTarget::Title(index),
        };
    }

    fn open_fonts(&mut self, i: usize) {
        let list = super::fonts::families();
        if list.is_empty() {
            self.warn("No fonts found: fontconfig (fc-list) is missing.");
            return;
        }
        let current = self.project.clips[i].effects.font.clone();
        let mut f = FontPicker {
            clip: i,
            list,
            filter: String::new(),
            selected: 0,
        };
        if let Some(k) = f
            .visible()
            .iter()
            .position(|x| x.is_some_and(|x| x.name == current))
        {
            f.selected = k;
        }
        self.overlay = Overlay::Fonts(f);
    }

    fn accept_font(&mut self) {
        let Overlay::Fonts(f) = &self.overlay else {
            return;
        };
        let i = f.clip;
        let name = f
            .visible()
            .get(f.selected)
            .copied()
            .flatten()
            .map(|x| x.name.clone())
            .unwrap_or_default();
        self.overlay = Overlay::None;
        if i < self.project.clips.len() {
            let shown = if name.is_empty() {
                "the desktop font".to_string()
            } else {
                name.clone()
            };
            self.change(|p| p.clips[i].effects.font = name);
            self.say(format!("Font: {shown}. P shows it rendered."));
        }
    }

    /// A still of the clip at the playhead (or its start), as the film shows it.
    fn grab_frame(&self, i: usize) -> Result<(Vec<u32>, i32, i32), String> {
        let clip = &self.project.clips[i];
        let media = &self.project.media[clip.media];
        let (w, h, _) = export::frame(&self.project, 0, Format::Mp4);
        let dw = w.min(1280);
        let dh = ((f64::from(h) * f64::from(dw) / f64::from(w)) / 2.0).round() as u32 * 2;
        if media.is_card() || !media.has_video {
            let colour = if media.is_card() {
                clip.effects.card_colour
            } else {
                0
            };
            return Ok((vec![colour; (dw * dh) as usize], dw as i32, dh as i32));
        }
        let start = self.project.starts()[i];
        let local = (self.playhead - start).clamp(0.0, clip.length());
        let source = if media.still {
            0.0
        } else {
            clip.source_at(local)
        };
        let chain = format!("{},scale={dw}:{dh}", export::frame_chain(clip, w, h));
        let out = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-ss"])
            .arg(format!("{source:.3}"))
            .arg("-i")
            .arg(&media.path)
            .args(["-frames:v", "1", "-vf", &chain])
            .args(["-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("ffmpeg could not start: {e}"))?;
        let want = (dw * dh * 4) as usize;
        if out.stdout.len() < want {
            return Err("could not read a picture from the clip".into());
        }
        let image = out.stdout[..want]
            .chunks_exact(4)
            .map(|b| (u32::from(b[2]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[0]))
            .collect();
        Ok((image, dw as i32, dh as i32))
    }

    /// One picture of clip `i` at the playhead, rendered as the export
    /// draws it.
    fn render_still(&self, i: usize) -> Result<(Vec<u32>, i32, i32, u32, u32), String> {
        let (fw, fh, _) = export::frame(&self.project, 0, Format::Mp4);
        let height = fh.min(540);
        let start = self.project.starts()[i];
        let local = (self.playhead - start).clamp(0.0, self.project.clips[i].length());
        // Show the title and the picture fully in, even when the playhead
        // is where they are still coming in or already gone.
        let (from, to) = export::settled_window(&self.project.clips[i]);
        let local = if local + 0.02 < from || local > to {
            from + 0.05
        } else {
            local
        };
        let mut settings =
            export::Settings::new(Format::Mp4, PathBuf::from("/dev/null"), self.work.clone());
        settings.font = super::gfx::font_path(self.theme.font.as_deref()).map(PathBuf::from);
        let mut temp = Vec::new();
        let (args, w, h) = export::still(&self.project, i, local, height, &settings, &mut temp)?;
        let out = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("ffmpeg could not start: {e}"));
        for f in temp {
            let _ = std::fs::remove_file(f);
        }
        let out = out?;
        let want = (w * h * 4) as usize;
        if out.stdout.len() < want {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "could not draw the picture: {}",
                err.lines().last().unwrap_or("ffmpeg gave no picture")
            ));
        }
        let image = out.stdout[..want]
            .chunks_exact(4)
            .map(|b| (u32::from(b[2]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[0]))
            .collect();
        Ok((image, w as i32, h as i32, fw, fh))
    }

    /// The placing view for clip `i`, the title (0) or the
    /// picture-in-picture (1) taken first.
    fn open_place(&mut self, i: usize, what: u8) {
        self.play(false);
        let c = &self.project.clips[i];
        let has_text = !c.effects.text.trim().is_empty();
        let has_pip = export::pip_source(&self.project, c).is_some();
        if !has_text && !has_pip {
            self.warn("Nothing to place: write a title (T) or show a camera or picture (Picture in picture).");
            return;
        }
        let selected = match what {
            1 if has_pip => 1,
            0 if has_text => 0,
            _ => u8::from(!has_text),
        };
        match self.render_still(i) {
            Ok((image, iw, ih, fw, fh)) => {
                self.overlay = Overlay::Place(Placer {
                    clip: i,
                    image,
                    iw,
                    ih,
                    fw,
                    fh,
                    selected,
                });
            }
            Err(e) => self.warn(e),
        }
    }

    /// Draw the placing view's picture again after a change.
    fn refresh_place(&mut self) {
        let Overlay::Place(pl) = &self.overlay else {
            return;
        };
        let i = pl.clip;
        if i >= self.project.clips.len() {
            self.overlay = Overlay::None;
            return;
        }
        match self.render_still(i) {
            Ok((image, iw, ih, fw, fh)) => {
                if let Overlay::Place(pl) = &mut self.overlay {
                    pl.image = image;
                    pl.iw = iw;
                    pl.ih = ih;
                    pl.fw = fw;
                    pl.fh = fh;
                }
            }
            Err(e) => self.warn(e),
        }
    }

    /// The title's and the picture-in-picture's boxes in frame pixels.
    fn place_boxes(&self, pl: &Placer) -> [Option<(f64, f64, f64, f64)>; 2] {
        let Some(c) = self.project.clips.get(pl.clip) else {
            return [None, None];
        };
        let text = export::text_rect(&c.effects, pl.fw, pl.fh);
        let pip = export::pip_source(&self.project, c).map(|src| {
            let (b, _) = export::pip_box(&self.project, c, &src, pl.fw, pl.fh);
            (
                f64::from(b.x),
                f64::from(b.y),
                f64::from(b.width),
                f64::from(b.height),
            )
        });
        [text, pip]
    }

    /// Move the selected thing so its centre is at (cx, cy) frame pixels.
    fn place_move(&mut self, what: u8, cx: f64, cy: f64) {
        let Overlay::Place(pl) = &self.overlay else {
            return;
        };
        let (i, fw, fh) = (pl.clip, f64::from(pl.fw), f64::from(pl.fh));
        let boxes = self.place_boxes(pl);
        let Some((_, _, bw, bh)) = boxes[what as usize] else {
            return;
        };
        let c = &mut self.project.clips[i];
        if what == 0 {
            let e = &mut c.effects;
            e.text_position = 9;
            e.text_xy = [
                (cx / fw * 100.0).clamp(0.0, 100.0),
                ((cy - bh / 2.0) / fh * 100.0).clamp(0.0, 100.0),
            ];
        } else {
            let pip = &mut c.pip;
            if pip.area[2] < 0.5 {
                pip.area[2] = bw / fw * 100.0;
            }
            pip.area[0] = (cx / fw * 100.0).clamp(0.0, 100.0);
            pip.area[1] = (cy / fh * 100.0).clamp(0.0, 100.0);
        }
    }

    /// Larger or smaller: the title's size or the picture's width.
    fn place_resize(&mut self, up: bool) {
        let Overlay::Place(pl) = &self.overlay else {
            return;
        };
        let (i, what, fw, fh) = (pl.clip, pl.selected, f64::from(pl.fw), f64::from(pl.fh));
        let pip_box = self.place_boxes(pl)[1];
        self.change(|p| {
            let c = &mut p.clips[i];
            if what == 0 {
                let e = &mut c.effects;
                e.text_size = (e.text_size + if up { 0.5 } else { -0.5 }).clamp(2.0, 20.0);
            } else {
                let pip = &mut c.pip;
                if pip.area[2] < 0.5 {
                    // From "as recorded": keep where it is, then change it.
                    if let Some((x, y, bw, bh)) = pip_box {
                        pip.area = [
                            (x + bw / 2.0) / fw * 100.0,
                            (y + bh / 2.0) / fh * 100.0,
                            bw / fw * 100.0,
                        ];
                    }
                }
                pip.area[2] = (pip.area[2] + if up { 2.0 } else { -2.0 }).clamp(5.0, 90.0);
            }
        });
        self.refresh_place();
    }

    /// A key in the placing view; true when it was taken.
    fn place_key(&mut self, sym: u32, shift: bool) -> bool {
        let Overlay::Place(pl) = &self.overlay else {
            return false;
        };
        let (i, what) = (pl.clip, pl.selected);
        let boxes = self.place_boxes(pl);
        let (fw, fh) = (f64::from(pl.fw), f64::from(pl.fh));
        let ch = sym_char(sym).map(|c| c.to_ascii_lowercase());
        let step = if shift { 5.0 } else { 1.0 };
        let nudge = |dx: f64, dy: f64| {
            boxes[what as usize]
                .map(|(x, y, w, h)| (x + w / 2.0 + dx * fw / 100.0, y + h / 2.0 + dy * fh / 100.0))
        };
        let moved = match sym {
            K_LEFT => nudge(-step, 0.0),
            K_RIGHT => nudge(step, 0.0),
            K_UP => nudge(0.0, -step),
            K_DOWN => nudge(0.0, step),
            _ => None,
        };
        if let Some((cx, cy)) = moved {
            let before = self.project.clone();
            self.place_move(what, cx, cy);
            if self.project != before {
                self.history.record(&before);
                self.after_change();
            }
            self.refresh_place();
            return true;
        }
        match (sym, ch) {
            (K_TAB, _) => {
                let other = 1 - what;
                if boxes[other as usize].is_some() {
                    if let Overlay::Place(pl) = &mut self.overlay {
                        pl.selected = other;
                    }
                }
                return true;
            }
            (K_DELETE, _) => {
                self.change(|p| {
                    let c = &mut p.clips[i];
                    if what == 0 {
                        c.effects.text.clear();
                    } else {
                        c.pip.mode = 1;
                    }
                });
                self.say(if what == 0 {
                    "Title removed"
                } else {
                    "Picture in picture off for this clip"
                });
                if self.place_boxes_now().iter().all(Option::is_none) {
                    self.overlay = Overlay::None;
                    return true;
                }
                if let Overlay::Place(pl) = &mut self.overlay {
                    pl.selected = 1 - what;
                }
            }
            (_, Some('+')) | (_, Some('=')) => {
                self.place_resize(true);
                return true;
            }
            (_, Some('-')) => {
                self.place_resize(false);
                return true;
            }
            (_, Some(k)) if "sbdelcr".contains(k) => {
                self.change(|p| {
                    let c = &mut p.clips[i];
                    let e = &mut c.effects;
                    match (what, k) {
                        (1, 's') => c.pip.shape = (c.pip.shape + 1) % 4,
                        (1, 'b') => c.pip.border = (c.pip.border + 1) % PIP_BORDERS.len() as u8,
                        (1, 'd') => c.pip.shadow = !c.pip.shadow,
                        (1, 'e') => c.pip.effect = (c.pip.effect + 1) % 3,
                        (1, 'r') => c.pip.area = [0.0; 3],
                        (0, 'e') => e.text_anim = (e.text_anim + 1) % 5,
                        (0, 'l') => e.text_look = (e.text_look + 1) % 7,
                        (0, 'c') => {
                            let k = palette_index(&TEXT_COLOURS, e.text_colour);
                            e.text_colour = TEXT_COLOURS[(k + 1) % TEXT_COLOURS.len()].0;
                        }
                        (0, 'b') => {
                            let k = palette_index(&TEXT_COLOURS, e.text_accent);
                            e.text_accent = TEXT_COLOURS[(k + 1) % TEXT_COLOURS.len()].0;
                        }
                        (0, 'r') => e.text_position = 7,
                        _ => {}
                    }
                });
            }
            _ => return false,
        }
        self.refresh_place();
        true
    }

    fn place_boxes_now(&self) -> [Option<(f64, f64, f64, f64)>; 2] {
        match &self.overlay {
            Overlay::Place(pl) => self.place_boxes(pl),
            _ => [None, None],
        }
    }

    fn open_area(&mut self, i: usize, zoom: bool) {
        self.play(false);
        match self.grab_frame(i) {
            Ok((image, iw, ih)) => {
                let e = &self.project.clips[i].effects;
                let rect = if zoom {
                    if e.has_zoom() {
                        e.zoom_area
                    } else {
                        [25.0, 25.0, 50.0, 50.0]
                    }
                } else {
                    e.hide_area
                };
                self.overlay = Overlay::Area(AreaPicker {
                    clip: i,
                    zoom,
                    image,
                    iw,
                    ih,
                    rect,
                });
            }
            Err(e) => self.warn(e),
        }
    }

    fn accept_area(&mut self, clear: bool) {
        let Overlay::Area(a) = &self.overlay else {
            return;
        };
        let (i, zoom, rect) = (a.clip, a.zoom, a.rect);
        self.overlay = Overlay::None;
        if i >= self.project.clips.len() {
            return;
        }
        self.change(|p| {
            let e = &mut p.clips[i].effects;
            match (zoom, clear) {
                (true, true) => e.zoom_area = [0.0; 4],
                (true, false) => e.zoom_area = rect,
                (false, true) => e.hide = 0,
                (false, false) => {
                    e.hide_area = rect;
                    if e.hide == 0 {
                        e.hide = 1;
                    }
                }
            }
        });
        self.say(match (zoom, clear) {
            (true, true) => "Zoom removed",
            (true, false) => "Zoom set: P shows it rendered",
            (false, true) => "The picture shows whole again",
            (false, false) => "Area hidden: P shows it rendered",
        });
    }

    // ── Film settings, pauses and captions ──────────────────────────────

    fn film_rows(&self) -> Vec<FilmRow> {
        let mut rows = vec![FilmRow::Loudness, FilmRow::Duck, FilmRow::Logo];
        if self.project.logo.is_some() {
            rows.extend([FilmRow::LogoCorner, FilmRow::LogoSize, FilmRow::LogoOpacity]);
        }
        rows.push(FilmRow::CaptionSize);
        rows
    }

    fn film_options(&self, row: FilmRow) -> (&'static str, Vec<String>, usize) {
        let p = &self.project;
        let list = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let nearest = |list: &[f64], v: f64| {
            list.iter()
                .enumerate()
                .min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs()))
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        match row {
            FilmRow::Loudness => (
                "Loudness",
                export::LOUDNESS
                    .iter()
                    .map(|(l, _)| l.to_string())
                    .collect(),
                p.loudness as usize,
            ),
            FilmRow::Duck => (
                "Music under speech",
                list(&["As it is", "A little lower", "Much lower"]),
                p.duck as usize,
            ),
            FilmRow::Logo => match &p.logo {
                Some(l) => {
                    let name = l
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (
                        "Logo",
                        vec!["None".into(), name, "Choose another…".into()],
                        1,
                    )
                }
                None => ("Logo", list(&["None", "Choose a picture…"]), 0),
            },
            FilmRow::LogoCorner => (
                "Logo corner",
                list(&LOGO_CORNERS),
                p.logo.as_ref().map(|l| l.corner as usize).unwrap_or(1),
            ),
            FilmRow::LogoSize => (
                "Logo size",
                LOGO_SIZES.iter().map(|v| format!("{v:.0} %")).collect(),
                nearest(&LOGO_SIZES, p.logo.as_ref().map(|l| l.size).unwrap_or(12.0)),
            ),
            FilmRow::LogoOpacity => (
                "Logo strength",
                LOGO_OPACITY
                    .iter()
                    .map(|v| format!("{:.0} %", v * 100.0))
                    .collect(),
                nearest(
                    &LOGO_OPACITY,
                    p.logo.as_ref().map(|l| l.opacity).unwrap_or(0.8),
                ),
            ),
            FilmRow::CaptionSize => (
                "Caption size",
                list(&["Small", "Medium", "Large"]),
                p.caption_size as usize,
            ),
        }
    }

    fn film_set(&mut self, row: FilmRow, i: usize) {
        match row {
            FilmRow::Loudness => self.change(|p| p.loudness = i as u8),
            FilmRow::Duck => self.change(|p| p.duck = i as u8),
            FilmRow::Logo => {
                let has = self.project.logo.is_some();
                if i == 0 {
                    self.change(|p| p.logo = None);
                } else if !has || i == 2 {
                    self.browse(Purpose::Logo);
                }
            }
            FilmRow::LogoCorner => self.change(|p| {
                if let Some(l) = &mut p.logo {
                    l.corner = i as u8;
                }
            }),
            FilmRow::LogoSize => self.change(|p| {
                if let Some(l) = &mut p.logo {
                    l.size = LOGO_SIZES[i];
                }
            }),
            FilmRow::LogoOpacity => self.change(|p| {
                if let Some(l) = &mut p.logo {
                    l.opacity = LOGO_OPACITY[i];
                }
            }),
            FilmRow::CaptionSize => self.change(|p| p.caption_size = i as u8),
        }
    }

    fn film_step(&mut self, row: FilmRow, delta: isize) {
        let (_, opts, chosen) = self.film_options(row);
        if row == FilmRow::Logo {
            return;
        }
        let n = opts.len() as isize;
        let i = (chosen as isize + delta).rem_euclid(n) as usize;
        self.film_set(row, i);
    }

    fn open_pauses(&mut self) {
        if self.project.clips.is_empty() {
            return;
        }
        self.play(false);
        let choice = self.settings.pauses.unwrap_or([1, 0, 1]);
        let choice = [
            choice[0].min(PAUSE_MIN.len() - 1),
            choice[1].min(PAUSE_LEVEL.len() - 1),
            choice[2].min(PAUSE_KEEP.len() - 1),
        ];
        self.overlay = Overlay::Pauses(PauseForm { row: 0, choice });
        let fresh = self
            .levels
            .as_ref()
            .is_some_and(|(p, _)| *p == self.project);
        if !fresh && self.levels_task.is_none() {
            self.levels = None;
            self.levels_task = Some(analyze::levels(self.project.clone(), self.work.clone()));
        }
    }

    /// The pauses the dialog's choices find, in film seconds.
    fn found_pauses(&self) -> Option<Vec<(f64, f64)>> {
        let Overlay::Pauses(f) = &self.overlay else {
            return None;
        };
        let (_, levels) = self.levels.as_ref()?;
        let level = PAUSE_LEVEL[f.choice[1]].1;
        let threshold = if level == 0.0 {
            levels.auto_threshold()
        } else {
            level
        };
        Some(analyze::pauses(
            levels,
            threshold,
            PAUSE_MIN[f.choice[0]],
            PAUSE_KEEP[f.choice[2]],
        ))
    }

    fn apply_pauses(&mut self) {
        let Some(found) = self.found_pauses() else {
            self.warn("Still listening to the film…");
            return;
        };
        if let Overlay::Pauses(f) = &self.overlay {
            self.settings.pauses = Some(f.choice);
            self.settings.save();
        }
        self.overlay = Overlay::None;
        if found.is_empty() {
            self.say("No pauses to cut");
            return;
        }
        let before = self.project.duration();
        self.change(|p| {
            for (a, b) in found.iter().rev() {
                p.remove_range(*a, *b);
            }
        });
        let saved = before - self.project.duration();
        self.seek(0.0);
        self.fitted = false;
        self.say(format!(
            "Cut {} pauses, {} shorter. Ctrl+Z brings them back.",
            found.len(),
            clock(saved)
        ));
    }

    fn open_captions(&mut self) {
        let language = analyze::LANGUAGES
            .iter()
            .position(|(_, code)| Some(*code) == self.settings.caption_language.as_deref())
            .unwrap_or(0);
        self.play(false);
        self.overlay = Overlay::Captions(CaptionForm {
            selected: 0,
            language,
        });
    }

    fn make_captions(&mut self) {
        if self.project.clips.is_empty() {
            return;
        }
        if self.caption_task.is_some() {
            self.warn("Captions are already being made.");
            return;
        }
        if !analyze::have_nirudictate() {
            self.warn("Captions need NIRUDICTATE: nirux extras install nirudictate");
            return;
        }
        let language = match &self.overlay {
            Overlay::Captions(f) => f.language,
            _ => 0,
        };
        let (_, code) = analyze::LANGUAGES[language];
        self.settings.caption_language = Some(code.to_string());
        self.settings.save();
        let snapshot = self.project.clone();
        let task = analyze::captions(snapshot.clone(), self.work.clone(), code);
        self.caption_task = Some((task, snapshot));
        self.caption_progress = Some((0.0, "Starting…".into()));
        self.say("Making captions in the background: keep working meanwhile.");
    }

    /// Put made captions into the project, by media file: they were made
    /// on a snapshot, and the film may have changed since.
    fn take_captions(&mut self, snapshot: &mut Project, list: &[(f64, f64, String)]) {
        snapshot.set_film_captions(list);
        let n = list.len();
        self.change(|p| {
            for m in &mut p.media {
                if let Some(src) = snapshot.media.iter().find(|s| s.path == m.path) {
                    m.captions = src.captions.clone();
                }
            }
        });
        self.say(format!(
            "{n} captions made. Edit them in Captions (Ctrl+L)."
        ));
        notify("Captions ready", &format!("{n} captions"));
    }

    fn edit_caption(&mut self, k: usize) {
        let list = self.project.film_captions();
        if let Some((a, _, text)) = list.get(k) {
            let at = *a;
            self.seek(at);
            self.overlay = Overlay::TextInput {
                title: format!("Caption at {}", clock(at)),
                text: text.clone(),
                target: TextTarget::Caption(k),
            };
        }
    }

    fn delete_caption(&mut self, k: usize) {
        let mut list = self.project.film_captions();
        if k < list.len() {
            list.remove(k);
            self.change(|p| p.set_film_captions(&list));
        }
    }

    /// Play the voice treatment of the clip under the playhead live.
    fn sync_voice(&mut self) {
        let voice = match self.preview {
            Preview::Timeline => self
                .project
                .locate(self.playhead)
                .map(|(i, _)| self.project.clips[i].voice)
                .unwrap_or(0),
            Preview::Rendered { .. } => 0,
        };
        if voice != self.live_voice {
            self.live_voice = voice;
            if let Some(m) = &mut self.mpv {
                m.audio_filter(export::voice_chain(voice));
            }
        }
    }

    fn split(&mut self) {
        let at = self.playhead;
        let mut new = None;
        self.change(|p| new = p.split(at));
        match new {
            Some(i) => {
                self.selected = Some(i);
                self.say(format!("Split at {}", clock(at)));
            }
            None => self.warn("Move the playhead inside a clip to split it."),
        }
    }

    fn delete(&mut self) {
        if let (Some(a), Some(b)) = (self.mark_in, self.mark_out) {
            self.change(|p| p.remove_range(a, b));
            self.mark_in = None;
            self.mark_out = None;
            self.playhead = a.min(b);
            self.reload(true);
            self.say("Removed the marked part");
            return;
        }
        if let Some(j) = self.music_selected {
            self.change(|p| {
                p.music.remove(j);
            });
            self.music_selected = None;
            return;
        }
        if let (Some(i), Some(what)) = (self.selected, self.lane_sel) {
            // On a lane: the title goes, or the camera is switched off.
            if what == 0 {
                self.change(|p| p.clips[i].effects.text.clear());
                self.say("Title removed (Ctrl+Z brings it back)");
            } else {
                self.change(|p| p.clips[i].pip.mode = 1);
                self.say("Camera off for this clip (double-click its ghost to turn it on)");
            }
            self.lane_sel = None;
            return;
        }
        let n = self.selection().len();
        if n > 1 {
            self.remove_selection();
            self.say(format!("{n} clips removed (Ctrl+Z brings them back)"));
            return;
        }
        if let Some(i) = self.target() {
            self.change(|p| {
                p.clips.remove(i);
            });
            self.selected = None;
            self.say("Clip removed (Ctrl+Z brings it back)");
        }
    }

    fn duplicate(&mut self) {
        let sel = self.selection();
        if sel.len() > 1 {
            let last = *sel.last().expect("a clip");
            let n = sel.len();
            self.change(|p| {
                for (k, &i) in sel.iter().enumerate() {
                    let mut c = p.clips[i].clone();
                    if k == 0 {
                        c.audio_offset = 0.0;
                    }
                    p.clips.insert(last + 1 + k, c);
                }
            });
            self.picked = (last + 1..last + n).collect();
            self.selected = Some(last + n);
            return;
        }
        if let Some(i) = self.target() {
            self.change(|p| {
                let mut c = p.clips[i].clone();
                c.audio_offset = 0.0;
                p.clips.insert(i + 1, c);
            });
            self.selected = Some(i + 1);
        }
    }

    fn move_clip(&mut self, delta: isize) {
        let Some(i) = self.target() else { return };
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.project.clips.len() {
            return;
        }
        self.change(|p| p.clips.swap(i, j as usize));
        self.selected = Some(j as usize);
    }

    /// Every chosen clip, in order: the selected one and those picked
    /// with Ctrl or Shift.
    fn selection(&self) -> Vec<usize> {
        let n = self.project.clips.len();
        let mut v: Vec<usize> = self.picked.iter().copied().filter(|i| *i < n).collect();
        if let Some(i) = self.selected.filter(|i| *i < n) {
            if !v.contains(&i) {
                v.push(i);
            }
        }
        v.sort_unstable();
        v
    }

    /// Show a clip's title (0) or picture-in-picture (1) settings.
    fn show_group(&mut self, what: u8) {
        let want = if what == 0 { Param::Title } else { Param::Pip };
        if let Some(row) = self.params().iter().position(|p| *p == want) {
            self.param_row = row;
            self.focus = Focus::Inspector;
        }
    }

    /// To the next (or previous) cut or marker.
    fn jump(&mut self, forward: bool) {
        let mut points = self.cut_points();
        points.extend(self.project.film_markers().into_iter().map(|m| m.0));
        points.sort_by(f64::total_cmp);
        let t = if forward {
            points
                .iter()
                .find(|p| **p > self.playhead + 1e-3)
                .copied()
                .unwrap_or(self.project.duration())
        } else {
            points
                .iter()
                .rev()
                .find(|p| **p < self.playhead - 1e-3)
                .copied()
                .unwrap_or(0.0)
        };
        self.seek(t);
    }

    /// The part that loops: the marked part, else the whole film.
    fn loop_range(&self) -> (f64, f64) {
        match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) if (b - a).abs() > 0.1 => (a.min(b), a.max(b)),
            _ => (0.0, self.project.duration()),
        }
    }

    fn toggle_loop(&mut self) {
        self.looping = !self.looping;
        self.say(if !self.looping {
            "Loop off"
        } else if self.mark_in.is_some() && self.mark_out.is_some() {
            "Loop on: the marked part plays over and over"
        } else {
            "Loop on: the film plays over and over (I and O loop a part)"
        });
    }

    fn toggle_snap(&mut self) {
        self.snapping = !self.snapping;
        self.say(if self.snapping {
            "Snapping on: edges catch cuts, markers and the playhead"
        } else {
            "Snapping off"
        });
    }

    /// Set a marker at the playhead, or remove the one there.
    fn toggle_marker(&mut self) {
        let near = self
            .project
            .film_markers()
            .into_iter()
            .find(|m| (m.0 - self.playhead).abs() < self.frame() * 2.0);
        if let Some((_, _, i, k)) = near {
            self.change(|p| {
                p.clips[i].markers.remove(k);
            });
            self.say("Marker removed");
            return;
        }
        let at = self.playhead;
        let mut done = None;
        self.change(|p| done = p.add_marker(at, String::new()));
        if done.is_some() {
            self.say("Marker set: double-click it to name it, it becomes a chapter");
        } else {
            self.warn("Put the playhead inside a clip to set a marker.");
        }
    }

    /// Copy the chosen clips (and what they show).
    fn copy(&mut self) -> usize {
        let sel = self.selection();
        self.clipboard = sel
            .iter()
            .map(|&i| {
                let c = self.project.clips[i].clone();
                let m = self.project.media[c.media].clone();
                let pip = c.pip.media.and_then(|k| self.project.media.get(k).cloned());
                (c, m, pip)
            })
            .collect();
        sel.len()
    }

    /// Remove the chosen clips.
    fn remove_selection(&mut self) {
        let sel = self.selection();
        if sel.is_empty() {
            return;
        }
        self.change(|p| {
            for &i in sel.iter().rev() {
                p.clips.remove(i);
            }
        });
        self.picked.clear();
        self.selected = None;
        self.lane_sel = None;
    }

    /// Put the copied clips in at the playhead (a clip there is split).
    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.warn("Nothing copied: choose clips and press Ctrl+C.");
            return;
        }
        let at = self.playhead;
        let items = self.clipboard.clone();
        let mut first = 0;
        let n = items.len();
        self.change(|p| {
            let starts = p.starts();
            let index = match p.locate(at) {
                None => 0,
                Some((i, local)) if local < 0.05 => i,
                Some((i, local)) if p.clips[i].length() - local < 0.05 => i + 1,
                Some((i, _)) if at >= starts[i] + p.clips[i].length() - 1e-6 => i + 1,
                Some(_) => p.split(at).unwrap_or(p.clips.len()),
            };
            first = index;
            for (k, (clip, media, pip)) in items.into_iter().enumerate() {
                let mut clip = clip;
                clip.media = p.add_media(media);
                clip.pip.media = pip.map(|m| p.add_media(m));
                if k == 0 {
                    clip.audio_offset = 0.0;
                }
                p.clips.insert(index + k, clip);
            }
        });
        self.picked = (first..first + n).collect();
        self.selected = Some(first + n - 1);
        self.picked.remove(&(first + n - 1));
        self.say(format!("Pasted {n} clip{}", if n == 1 { "" } else { "s" }));
    }

    fn cut_points(&self) -> Vec<f64> {
        let mut v = self.project.starts();
        v.push(self.project.duration());
        v
    }

    fn project_fps(&self) -> f64 {
        if self.project.fps > 0.0 {
            self.project.fps
        } else {
            30.0
        }
    }

    fn frame(&self) -> f64 {
        1.0 / if self.project.fps > 0.0 {
            self.project.fps
        } else {
            30.0
        }
    }

    fn fit(&mut self, l: &Layout) {
        let d = self.project.duration().max(1.0);
        self.zoom = (f64::from(l.lane_video.w - 20) / d).clamp(0.5, 2000.0);
        self.scroll = 0.0;
    }

    fn zoom_by(&mut self, factor: f64, l: &Layout, at_x: Option<i32>) {
        let x = at_x.unwrap_or_else(|| self.time_x(l, self.playhead));
        let t = self.x_time(l, x);
        self.zoom = (self.zoom * factor).clamp(0.5, 2000.0);
        self.scroll = (t - f64::from(x - l.lane_video.x) / self.zoom).max(0.0);
    }

    fn time_x(&self, l: &Layout, t: f64) -> i32 {
        l.lane_video.x + ((t - self.scroll) * self.zoom).round() as i32
    }

    fn x_time(&self, l: &Layout, x: i32) -> f64 {
        (self.scroll + f64::from(x - l.lane_video.x) / self.zoom).max(0.0)
    }

    /// Keep the playhead in view while playing.
    fn follow(&mut self, l: &Layout) {
        let x = self.time_x(l, self.playhead);
        if x > l.lane_video.x + l.lane_video.w - 40 || x < l.lane_video.x {
            self.scroll = (self.playhead - 2.0).max(0.0);
        }
    }

    fn snap(&self, l: &Layout, t: f64) -> f64 {
        if !self.snapping {
            return t;
        }
        let tol = 8.0 / self.zoom;
        let mut best = t;
        let mut dist = tol;
        let mut points = self.cut_points();
        points.extend(self.project.film_markers().into_iter().map(|m| m.0));
        points.push(self.playhead);
        points.extend(self.mark_in);
        points.extend(self.mark_out);
        for p in points {
            if (p - t).abs() < dist {
                dist = (p - t).abs();
                best = p;
            }
        }
        let _ = l;
        best
    }

    // ── Keys ────────────────────────────────────────────────────────────

    /// Returns true to quit.
    fn key(&mut self, sym: u32, state: u16) -> bool {
        let ctrl = state & CTRL != 0;
        let shift = state & SHIFT != 0;
        let alt = state & ALT != 0;
        if !matches!(sym, 0xffe1..=0xffee) && !(ctrl && (sym == u32::from(b'q'))) {
            self.quit_armed = false;
        }
        // Overlays take the keys first.
        match &mut self.overlay {
            Overlay::TextInput { text, target, .. } => {
                match sym {
                    K_ESCAPE => {
                        let back = matches!(target, TextTarget::Caption(_));
                        self.overlay = Overlay::None;
                        if back {
                            self.open_captions();
                        }
                    }
                    K_RETURN | K_KP_ENTER => {
                        let value = text.trim().to_string();
                        let target = *target;
                        self.overlay = Overlay::None;
                        match target {
                            TextTarget::Title(i) => {
                                if i < self.project.clips.len() {
                                    let value = value
                                        .split('|')
                                        .map(str::trim)
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    self.change(|p| p.clips[i].effects.text = value);
                                    self.focus = Focus::Inspector;
                                    self.say(
                                        "Title set: style it in the panel, P shows it rendered.",
                                    );
                                }
                            }
                            TextTarget::Caption(k) => {
                                let mut list = self.project.film_captions();
                                if k < list.len() {
                                    if value.is_empty() {
                                        list.remove(k);
                                    } else {
                                        list[k].2 = value;
                                    }
                                    self.change(|p| p.set_film_captions(&list));
                                }
                                self.open_captions();
                                if let Overlay::Captions(f) = &mut self.overlay {
                                    f.selected = k;
                                }
                            }
                            TextTarget::Marker(i, k) => {
                                if self
                                    .project
                                    .clips
                                    .get(i)
                                    .is_some_and(|c| k < c.markers.len())
                                {
                                    self.change(|p| p.clips[i].markers[k].name = value);
                                }
                            }
                            TextTarget::ExportName => {
                                self.open_export();
                                if let Overlay::Export(f) = &mut self.overlay {
                                    f.name = value;
                                }
                            }
                        }
                    }
                    K_BACKSPACE => {
                        if ctrl {
                            text.clear();
                        } else {
                            text.pop();
                        }
                    }
                    _ => {
                        if let Some(ch) = sym_char(sym).filter(|_| !ctrl) {
                            text.push(ch);
                        }
                    }
                }
                return false;
            }
            Overlay::Browser(b) => {
                let n = b.visible().len();
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.browser_accept(ctrl),
                    K_UP => b.selected = b.selected.saturating_sub(1),
                    K_DOWN => b.selected = (b.selected + 1).min(n.saturating_sub(1)),
                    K_PAGE_UP => b.selected = b.selected.saturating_sub(15),
                    K_PAGE_DOWN => b.selected = (b.selected + 15).min(n.saturating_sub(1)),
                    K_HOME => b.selected = 0,
                    K_END => b.selected = n.saturating_sub(1),
                    K_LEFT if b.filter.is_empty() || b.purpose != Purpose::SaveAs => b.up(),
                    K_BACKSPACE => {
                        if ctrl {
                            b.filter.clear();
                        } else if b.filter.is_empty() {
                            b.up();
                        } else {
                            b.filter.pop();
                        }
                        b.selected = 0;
                    }
                    K_RIGHT => {
                        if let Some(e) = b.current() {
                            if e.dir {
                                b.enter(e.path);
                            }
                        }
                    }
                    K_INSERT => {
                        b.toggle_mark();
                        b.selected = (b.selected + 1).min(n.saturating_sub(1));
                    }
                    _ if ctrl && sym == u32::from(b'h') => {
                        b.hidden = !b.hidden;
                        b.read();
                    }
                    _ if ctrl && sym == u32::from(b'a') => {
                        b.marked = b
                            .visible()
                            .iter()
                            .filter(|e| !e.dir)
                            .map(|e| e.path.clone())
                            .collect();
                    }
                    0x20 if b.purpose != Purpose::SaveAs && b.filter.is_empty() => {
                        b.toggle_mark();
                        b.selected = (b.selected + 1).min(n.saturating_sub(1));
                    }
                    _ => {
                        if let Some(ch) = sym_char(sym).filter(|_| !ctrl) {
                            b.filter.push(ch);
                            b.selected = 0;
                        }
                    }
                }
                return false;
            }
            Overlay::Export(f) => {
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.start_export(),
                    K_UP => f.format = f.format.saturating_sub(1),
                    K_DOWN => f.format = (f.format + 1).min(Format::ALL.len() - 1),
                    K_LEFT => f.height = f.height.saturating_sub(1),
                    K_RIGHT => f.height = (f.height + 1).min(HEIGHTS.len() - 1),
                    _ if sym_char(sym).map(|c| c.to_ascii_lowercase()) == Some('l') => {
                        f.limit = (f.limit + 1) % export::LIMITS.len();
                    }
                    _ if sym_char(sym).map(|c| c.to_ascii_lowercase()) == Some('u') => {
                        f.captions = (f.captions + 1) % export::CAPTIONS.len();
                    }
                    K_F2 => {
                        let name = f.name.clone();
                        self.overlay = Overlay::TextInput {
                            title: "File name (without extension)".into(),
                            text: name,
                            target: TextTarget::ExportName,
                        };
                    }
                    _ => {}
                }
                return false;
            }
            Overlay::Record(f) => {
                let rows = f.rows();
                f.row = f.row.min(rows.len() - 1);
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.start_record(),
                    K_UP => f.row = f.row.saturating_sub(1),
                    K_DOWN | K_TAB => f.row = (f.row + 1).min(rows.len() - 1),
                    K_LEFT => f.step(rows[f.row], -1),
                    K_RIGHT | 0x20 => f.step(rows[f.row], 1),
                    _ => {}
                }
                return false;
            }
            Overlay::Film { row } => {
                let cur = *row;
                let rows = self.film_rows();
                let r = cur.min(rows.len() - 1);
                let mut to = r;
                match sym {
                    K_RETURN | K_KP_ENTER if rows[r] == FilmRow::Logo => {
                        let n = self.film_options(FilmRow::Logo).1.len();
                        self.film_set(FilmRow::Logo, n - 1);
                    }
                    K_ESCAPE | K_RETURN | K_KP_ENTER => self.overlay = Overlay::None,
                    K_UP => to = r.saturating_sub(1),
                    K_DOWN | K_TAB => to = (r + 1).min(rows.len() - 1),
                    K_LEFT => self.film_step(rows[r], -1),
                    K_RIGHT | 0x20 => self.film_step(rows[r], 1),
                    _ if ctrl && sym_char(sym) == Some('l') => self.open_captions(),
                    _ if ctrl && sym_char(sym) == Some('k') => self.open_pauses(),
                    _ => {}
                }
                if let Overlay::Film { row } = &mut self.overlay {
                    *row = to;
                }
                return false;
            }
            Overlay::Pauses(f) => {
                let counts = [PAUSE_MIN.len(), PAUSE_LEVEL.len(), PAUSE_KEEP.len()];
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.apply_pauses(),
                    K_UP => f.row = f.row.saturating_sub(1),
                    K_DOWN | K_TAB => f.row = (f.row + 1).min(2),
                    K_LEFT => {
                        let n = counts[f.row];
                        f.choice[f.row] = (f.choice[f.row] + n - 1) % n;
                    }
                    K_RIGHT | 0x20 => {
                        f.choice[f.row] = (f.choice[f.row] + 1) % counts[f.row];
                    }
                    _ => {}
                }
                return false;
            }
            Overlay::Captions(f) => {
                let n = self.project.film_captions().len();
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_UP => f.selected = f.selected.saturating_sub(1),
                    K_DOWN => f.selected = (f.selected + 1).min(n.saturating_sub(1)),
                    K_PAGE_UP => f.selected = f.selected.saturating_sub(10),
                    K_PAGE_DOWN => f.selected = (f.selected + 10).min(n.saturating_sub(1)),
                    K_TAB | K_LEFT | K_RIGHT => {
                        f.language = (f.language + 1) % analyze::LANGUAGES.len()
                    }
                    K_RETURN | K_KP_ENTER if n > 0 => {
                        let k = f.selected;
                        self.edit_caption(k);
                    }
                    K_DELETE if n > 0 => {
                        let k = f.selected;
                        self.delete_caption(k);
                        if let Overlay::Captions(f) = &mut self.overlay {
                            f.selected = k.min(n.saturating_sub(2));
                        }
                    }
                    0x20 if n > 0 => {
                        let k = f.selected;
                        if let Some((a, _, _)) = self.project.film_captions().get(k) {
                            let at = *a;
                            self.seek(at);
                            self.say(format!("Playhead at caption {}", k + 1));
                        }
                    }
                    _ if sym_char(sym).map(|c| c.to_ascii_lowercase()) == Some('g') => {
                        self.make_captions()
                    }
                    _ => {}
                }
                return false;
            }
            Overlay::Fonts(f) => {
                let n = f.visible().len();
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.accept_font(),
                    K_UP => f.selected = f.selected.saturating_sub(1),
                    K_DOWN => f.selected = (f.selected + 1).min(n.saturating_sub(1)),
                    K_PAGE_UP => f.selected = f.selected.saturating_sub(12),
                    K_PAGE_DOWN => f.selected = (f.selected + 12).min(n.saturating_sub(1)),
                    K_HOME => f.selected = 0,
                    K_END => f.selected = n.saturating_sub(1),
                    K_BACKSPACE => {
                        if ctrl {
                            f.filter.clear();
                        } else {
                            f.filter.pop();
                        }
                        f.selected = 0;
                    }
                    _ => {
                        if let Some(ch) = sym_char(sym).filter(|_| !ctrl) {
                            f.filter.push(ch);
                            f.selected = usize::from(f.visible().len() > 1);
                        }
                    }
                }
                return false;
            }
            Overlay::Area(_) => {
                match sym {
                    K_ESCAPE => self.overlay = Overlay::None,
                    K_RETURN | K_KP_ENTER => self.accept_area(false),
                    K_DELETE => self.accept_area(true),
                    _ => {}
                }
                return false;
            }
            Overlay::Place(_) => {
                if matches!(sym, K_ESCAPE | K_RETURN | K_KP_ENTER) {
                    self.overlay = Overlay::None;
                    self.say("Placed: P shows the film rendered.");
                } else if ctrl && sym_char(sym).map(|c| c.to_ascii_lowercase()) == Some('z') {
                    if self.history.undo(&mut self.project) {
                        self.after_change();
                    }
                    self.refresh_place();
                } else {
                    self.place_key(sym, shift);
                }
                return false;
            }
            Overlay::Settings | Overlay::Help => {
                if matches!(sym, K_ESCAPE | K_RETURN | K_KP_ENTER | K_F1) || sym == u32::from(b'?')
                {
                    self.overlay = Overlay::None;
                }
                return false;
            }
            Overlay::None => {}
        }
        if ctrl {
            match sym_char(sym).map(|c| c.to_ascii_lowercase()) {
                Some('q') => return self.request_quit(),
                Some('o') => self.browse(Purpose::Open),
                Some('r') => self.open_record(),
                Some('i') => self.browse(Purpose::Import),
                Some('m') => self.browse(Purpose::Music),
                Some('s') => self.save(shift),
                Some('e') => self.open_export(),
                Some('z') if shift => {
                    if self.history.redo(&mut self.project) {
                        self.after_change();
                    }
                }
                Some('z') => {
                    if self.history.undo(&mut self.project) {
                        self.after_change();
                        self.say("Undone");
                    }
                }
                Some('y') => {
                    if self.history.redo(&mut self.project) {
                        self.after_change();
                    }
                }
                Some('d') => self.duplicate(),
                Some('c') => {
                    let n = self.copy();
                    if n > 0 {
                        self.say(format!(
                            "Copied {n} clip{}: Ctrl+V puts them in at the playhead",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                }
                Some('x') => {
                    let n = self.copy();
                    if n > 0 {
                        self.remove_selection();
                        self.say(format!(
                            "Cut {n} clip{}: Ctrl+V puts them back in at the playhead",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                }
                Some('v') => self.paste(),
                Some('a') => {
                    let n = self.project.clips.len();
                    if n > 0 {
                        self.picked = (0..n - 1).collect();
                        self.selected = Some(n - 1);
                        self.music_selected = None;
                        self.say(format!("All {n} clips chosen"));
                    }
                }
                Some(',') => self.overlay = Overlay::Settings,
                Some('t') => self.add_card(),
                Some('f') => self.overlay = Overlay::Film { row: 0 },
                Some('k') => self.open_pauses(),
                Some('l') => self.open_captions(),
                _ => {}
            }
            return false;
        }
        match sym {
            K_F1 => self.overlay = Overlay::Help,
            K_F11 => self.fullscreen = !self.fullscreen,
            K_ESCAPE if self.fullscreen => self.fullscreen = false,
            K_TAB => {
                self.focus = match self.focus {
                    Focus::Timeline => Focus::Inspector,
                    Focus::Inspector => Focus::Bin,
                    Focus::Bin => Focus::Timeline,
                }
            }
            K_ISO_LEFT_TAB => {
                self.focus = match self.focus {
                    Focus::Timeline => Focus::Bin,
                    Focus::Bin => Focus::Inspector,
                    Focus::Inspector => Focus::Timeline,
                }
            }
            K_ESCAPE => {
                if matches!(self.preview, Preview::Rendered { .. }) {
                    self.preview = Preview::Timeline;
                    self.reload(true);
                } else if self.mark_in.is_some() || self.mark_out.is_some() {
                    self.mark_in = None;
                    self.mark_out = None;
                } else {
                    self.selected = None;
                    self.picked.clear();
                    self.lane_sel = None;
                    self.music_selected = None;
                }
            }
            0x20 => {
                let on = !self.playing;
                self.play(on);
            }
            K_DELETE | K_BACKSPACE => self.delete(),
            K_HOME => self.seek(0.0),
            K_END => {
                let d = self.project.duration();
                self.seek(d);
            }
            K_UP | K_DOWN if self.focus == Focus::Inspector => {
                let n = if self.music_selected.is_some() {
                    MUSIC_PARAMS.len()
                } else {
                    self.params().len().max(1)
                };
                self.param_row = if sym == K_UP {
                    self.param_row.saturating_sub(1)
                } else {
                    (self.param_row + 1).min(n - 1)
                };
            }
            K_LEFT | K_RIGHT if self.focus == Focus::Inspector => {
                let d = if sym == K_LEFT { -1.0 } else { 1.0 };
                self.adjust(self.param_row, if shift { d * 5.0 } else { d });
            }
            K_RETURN | K_KP_ENTER if self.focus == Focus::Inspector => {
                if self.music_selected.is_none() {
                    self.enter_param(self.param_row);
                }
            }
            K_UP | K_DOWN if self.focus == Focus::Bin => {
                let n = self.project.media.len();
                if n > 0 {
                    self.bin_selected = if sym == K_UP {
                        self.bin_selected.saturating_sub(1)
                    } else {
                        (self.bin_selected + 1).min(n - 1)
                    };
                }
            }
            K_RETURN | K_KP_ENTER if self.focus == Focus::Bin => {
                let i = self.bin_selected;
                if i < self.project.media.len() {
                    self.change(|p| {
                        p.append(i, None);
                    });
                    self.selected = Some(self.project.clips.len() - 1);
                    self.fitted = false;
                }
            }
            K_LEFT | K_RIGHT => {
                let d = if sym == K_LEFT { -1.0 } else { 1.0 };
                if alt {
                    self.move_clip(d as isize);
                } else {
                    let step = if shift { 1.0 } else { self.frame() };
                    self.play(false);
                    self.seek(self.playhead + d * step);
                }
            }
            K_UP | K_DOWN => self.jump(sym == K_DOWN),
            K_KP_ADD => self.zoom *= 1.5,
            K_KP_SUB => self.zoom = (self.zoom / 1.5).max(0.5),
            _ => match sym_char(sym) {
                Some('j') | Some('J') => {
                    self.play(false);
                    self.seek(self.playhead - 2.0);
                }
                Some('k') | Some('K') => self.play(false),
                Some('l') | Some('L') => {
                    let rate = if self.playing && self.rate < 8.0 {
                        self.rate * 2.0
                    } else if self.playing {
                        self.rate
                    } else {
                        1.0
                    };
                    self.set_rate(rate);
                }
                Some('s') | Some('S') => self.split(),
                Some('i') | Some('I') => {
                    self.mark_in = Some(self.playhead);
                    if self.mark_out.is_some_and(|o| o <= self.playhead) {
                        self.mark_out = None;
                    }
                }
                Some('o') | Some('O') => {
                    self.mark_out = Some(self.playhead);
                    if self.mark_in.is_some_and(|i| i >= self.playhead) {
                        self.mark_in = None;
                    }
                }
                Some('x') | Some('X') => self.delete(),
                Some('c') | Some('C') => {
                    if let (Some(a), Some(b)) = (self.mark_in, self.mark_out) {
                        self.change(|p| p.keep_range(a, b));
                        self.mark_in = None;
                        self.mark_out = None;
                        self.seek(0.0);
                        self.say("Kept only the marked part");
                    } else {
                        self.warn("Mark a part with I and O first.");
                    }
                }
                Some('p') | Some('P') => self.render_preview(),
                Some('m') | Some('M') => {
                    if let Some(i) = self.target() {
                        let muted = !self.project.clips[i].mute;
                        let sel = self.selection();
                        self.change(|p| {
                            for j in sel {
                                p.clips[j].mute = muted;
                            }
                        });
                    }
                }
                Some('b') | Some('B') => self.toggle_marker(),
                Some('n') | Some('N') => self.toggle_snap(),
                Some('r') | Some('R') => self.toggle_loop(),
                Some('f') | Some('F') => self.fullscreen = !self.fullscreen,
                Some('t') | Some('T') => {
                    if let Some(i) = self.target() {
                        self.edit_title(i);
                    }
                }
                Some('e') | Some('E') => {
                    if let Some(i) = self.target() {
                        self.open_place(i, 0);
                    }
                }
                Some('+') | Some('=') => self.zoom *= 1.5,
                Some('-') => self.zoom = (self.zoom / 1.5).max(0.5),
                Some('z') | Some('Z') => self.fitted = false,
                Some('?') => self.overlay = Overlay::Help,
                _ => {}
            },
        }
        false
    }

    // ── Mouse ───────────────────────────────────────────────────────────

    fn hit(hits: &[(Rect, Hit)], x: i32, y: i32) -> Option<Hit> {
        hits.iter()
            .rev()
            .find(|(r, _)| r.has(x, y))
            .map(|(_, h)| *h)
    }

    fn action(&mut self, a: Action, l: &Layout) {
        match a {
            Action::Open => self.browse(Purpose::Open),
            Action::Import => self.browse(Purpose::Import),
            Action::Music => self.browse(Purpose::Music),
            Action::Save => self.save(false),
            Action::Export => self.open_export(),
            Action::Settings => self.overlay = Overlay::Settings,
            Action::Help => self.overlay = Overlay::Help,
            Action::Play => {
                let on = !self.playing;
                self.play(on);
            }
            Action::Split => self.split(),
            Action::Undo => {
                if self.history.undo(&mut self.project) {
                    self.after_change();
                }
            }
            Action::Redo => {
                if self.history.redo(&mut self.project) {
                    self.after_change();
                }
            }
            Action::MarkIn => self.mark_in = Some(self.playhead),
            Action::MarkOut => self.mark_out = Some(self.playhead),
            Action::RemoveRange => self.delete(),
            Action::KeepRange => {
                let _ = self.key(u32::from(b'c'), 0);
            }
            Action::Preview => self.render_preview(),
            Action::ZoomIn => self.zoom_by(1.5, l, None),
            Action::ZoomOut => self.zoom_by(1.0 / 1.5, l, None),
            Action::ZoomFit => self.fit(l),
            Action::ExportStart => self.start_export(),
            Action::ExportCancel => {
                if let Some(mut j) = self.job.take() {
                    j.cancel();
                    self.say("Export cancelled");
                }
            }
            Action::ExportFolder | Action::ExportFolderSetting => {
                self.browse(Purpose::ExportFolder)
            }
            Action::ExportName => {
                let _ = self.key(K_F2, 0);
            }
            Action::BrowserUp => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.up();
                }
            }
            Action::BrowserAccept => self.browser_accept(true),
            Action::BrowserCancel | Action::CloseOverlay => self.overlay = Overlay::None,
            Action::BrowserHidden => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.hidden = !b.hidden;
                    b.read();
                }
            }
            Action::MediaFolder => self.browse(Purpose::MediaFolder),
            Action::ResetFolders => {
                self.settings.media_folder = None;
                self.settings.export_folder = None;
                self.settings.save();
                self.say("Folders reset to Videos");
            }
            Action::Record => self.open_record(),
            Action::RecordStart => self.start_record(),
            Action::Film => self.overlay = Overlay::Film { row: 0 },
            Action::Pauses => self.open_pauses(),
            Action::PausesApply => self.apply_pauses(),
            Action::Captions => self.open_captions(),
            Action::CaptionsMake => self.make_captions(),
            Action::CaptionEdit => {
                if let Overlay::Captions(f) = &self.overlay {
                    let k = f.selected;
                    self.edit_caption(k);
                }
            }
            Action::CaptionDelete => {
                if let Overlay::Captions(f) = &self.overlay {
                    let k = f.selected;
                    self.delete_caption(k);
                }
            }
            Action::Card => self.add_card(),
            Action::AreaAccept => self.accept_area(false),
            Action::AreaClear => self.accept_area(true),
            Action::PlaceReset => {
                if let Overlay::Place(pl) = &self.overlay {
                    let (i, what) = (pl.clip, pl.selected);
                    self.change(|p| {
                        let c = &mut p.clips[i];
                        if what == 0 {
                            c.effects.text_position = 7;
                        } else {
                            c.pip.area = [0.0; 3];
                        }
                    });
                    self.refresh_place();
                }
            }
            Action::AreaWhole => {
                if let Overlay::Area(a) = &mut self.overlay {
                    a.rect = [0.0, 0.0, 100.0, 100.0];
                }
            }
            Action::FontAccept => self.accept_font(),
            Action::GoStart => self.seek(0.0),
            Action::GoEnd => {
                let d = self.project.duration();
                self.seek(d);
            }
            Action::PrevCut => self.jump(false),
            Action::NextCut => self.jump(true),
            Action::Loop => self.toggle_loop(),
            Action::Snap => self.toggle_snap(),
            Action::FullScreen => self.fullscreen = !self.fullscreen,
            Action::Recover => {
                let path = autosave_path();
                self.open_project(&path);
                self.path = None;
                self.saved = Project::default();
            }
            Action::Recent(i) => {
                if let Some(p) = self.settings.recent.get(i).cloned() {
                    self.open_project(&p);
                }
            }
        }
    }

    fn press(&mut self, hits: &[(Rect, Hit)], l: &Layout, button: u8, state: u16, x: i32, y: i32) {
        self.mouse = (x, y);
        let ctrl = state & CTRL != 0;
        let shift = state & SHIFT != 0;
        let hit = Self::hit(hits, x, y);
        let double = button == 1
            && hit.is_some()
            && self
                .last_click
                .is_some_and(|(h, t)| Some(h) == hit && t.elapsed() < Duration::from_millis(400));
        if button == 1 {
            self.last_click = if double {
                None
            } else {
                hit.map(|h| (h, Instant::now()))
            };
        }
        // Full screen: the picture covers everything. A click plays or
        // pauses, a double click (or Esc) comes back.
        if self.fullscreen {
            match button {
                1 if double => self.fullscreen = false,
                1 => {
                    let on = !self.playing;
                    self.play(on);
                }
                4..=7 => {
                    self.play(false);
                    let d = if button == 4 || button == 6 {
                        -1.0
                    } else {
                        1.0
                    };
                    self.seek(self.playhead + d * if shift { 1.0 } else { self.frame() });
                }
                _ => {}
            }
            return;
        }
        // Wheel.
        if matches!(button, 4..=7) {
            let up = button == 4 || button == 6;
            if matches!(self.overlay, Overlay::Place(_)) {
                self.place_resize(up);
                return;
            }
            if l.timeline.has(x, y) {
                if ctrl {
                    self.zoom_by(if up { 1.25 } else { 0.8 }, l, Some(x));
                } else {
                    let step = 80.0 / self.zoom;
                    self.scroll = (self.scroll + if up { -step } else { step }).max(0.0);
                }
            } else if let Overlay::Browser(b) = &mut self.overlay {
                let n = b.visible().len();
                b.selected = if up {
                    b.selected.saturating_sub(3)
                } else {
                    (b.selected + 3).min(n.saturating_sub(1))
                };
            } else if l.inspector.has(x, y) {
                self.inspector_scroll = (self.inspector_scroll + if up { -40 } else { 40 }).max(0);
            } else if l.video.has(x, y) || l.transport.has(x, y) {
                self.play(false);
                let d = if up { -1.0 } else { 1.0 };
                self.seek(self.playhead + d * if shift { 1.0 } else { self.frame() });
            }
            return;
        }
        if button == 3 {
            // Right click on a marker removes it.
            if let Some(Hit::Marker(i, k)) = hit {
                self.change(|p| {
                    if k < p.clips[i].markers.len() {
                        p.clips[i].markers.remove(k);
                    }
                });
                self.say("Marker removed");
                return;
            }
            // Right click on a clip: select and split there.
            if let Some(Hit::Clip(i, _)) = hit {
                self.selected = Some(i);
                self.music_selected = None;
                let t = self.x_time(l, x);
                self.seek(t);
            }
            return;
        }
        if button != 1 {
            return;
        }
        // Clicks outside an overlay close it, except the browser and export.
        match hit {
            Some(Hit::Button(a)) => self.action(a, l),
            Some(Hit::Entry(i)) => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.selected = i;
                    if ctrl {
                        b.toggle_mark();
                    } else if double {
                        self.browser_accept(false);
                    }
                }
            }
            Some(Hit::Choice(r, i)) => match &mut self.overlay {
                Overlay::Record(f) => {
                    let rows = f.rows();
                    if let Some(row) = rows.get(r).copied() {
                        f.row = r;
                        f.set(row, i);
                    }
                }
                Overlay::Film { row } => {
                    *row = r;
                    if let Some(fr) = self.film_rows().get(r).copied() {
                        self.film_set(fr, i);
                    }
                }
                Overlay::Pauses(f) => {
                    f.row = r.min(2);
                    f.choice[f.row] = i;
                }
                Overlay::Captions(f) => f.language = i.min(analyze::LANGUAGES.len() - 1),
                _ => {}
            },
            Some(Hit::Limit(i)) => {
                if let Overlay::Export(f) = &mut self.overlay {
                    f.limit = i;
                }
            }
            Some(Hit::CaptionMode(i)) => {
                if let Overlay::Export(f) = &mut self.overlay {
                    f.captions = i;
                }
            }
            Some(Hit::ListRow(k)) => match &mut self.overlay {
                Overlay::Captions(f) => {
                    f.selected = k;
                    if let Some((a, _, _)) = self.project.film_captions().get(k) {
                        let at = *a;
                        self.seek(at);
                    }
                    if double {
                        self.edit_caption(k);
                    }
                }
                Overlay::Fonts(f) => {
                    f.selected = k;
                    if double {
                        self.accept_font();
                    }
                }
                _ => {}
            },
            Some(Hit::PlaceImage) => {
                let image = hits
                    .iter()
                    .find(|(_, h)| *h == Hit::PlaceImage)
                    .map(|(r, _)| *r)
                    .unwrap_or_default();
                if let Overlay::Place(pl) = &self.overlay {
                    if image.w > 0 && image.h > 0 {
                        let fx = f64::from(x - image.x) / f64::from(image.w) * f64::from(pl.fw);
                        let fy = f64::from(y - image.y) / f64::from(image.h) * f64::from(pl.fh);
                        let boxes = self.place_boxes(pl);
                        // What was clicked; else the nearer one.
                        let inside = |b: &Option<(f64, f64, f64, f64)>| {
                            b.is_some_and(|(bx, by, bw, bh)| {
                                fx >= bx && fx <= bx + bw && fy >= by && fy <= by + bh
                            })
                        };
                        let dist = |b: &Option<(f64, f64, f64, f64)>| {
                            b.map(|(bx, by, bw, bh)| (bx + bw / 2.0 - fx).hypot(by + bh / 2.0 - fy))
                                .unwrap_or(f64::MAX)
                        };
                        let what = if inside(&boxes[pl.selected as usize]) {
                            pl.selected
                        } else if inside(&boxes[0]) {
                            0
                        } else if inside(&boxes[1]) {
                            1
                        } else if dist(&boxes[0]) <= dist(&boxes[1]) {
                            0
                        } else {
                            1
                        };
                        if let Some((bx, by, bw, bh)) = boxes[what as usize] {
                            let grab = (fx - (bx + bw / 2.0), fy - (by + bh / 2.0));
                            let before = self.project.clone();
                            self.history.record(&before);
                            if let Overlay::Place(pl) = &mut self.overlay {
                                pl.selected = what;
                            }
                            self.drag = Some(Drag::Place { image, what, grab });
                        }
                    }
                }
            }
            Some(Hit::AreaImage) => {
                let image = hits
                    .iter()
                    .find(|(_, h)| *h == Hit::AreaImage)
                    .map(|(r, _)| *r)
                    .unwrap_or_default();
                if image.w > 0 && image.h > 0 {
                    let fx = (f64::from(x - image.x) / f64::from(image.w)).clamp(0.0, 1.0);
                    let fy = (f64::from(y - image.y) / f64::from(image.h)).clamp(0.0, 1.0);
                    self.drag = Some(Drag::Area { fx, fy, image });
                }
            }
            Some(Hit::Format(i)) => {
                if let Overlay::Export(f) = &mut self.overlay {
                    f.format = i;
                }
            }
            Some(Hit::Height(i)) => {
                if let Overlay::Export(f) = &mut self.overlay {
                    f.height = i;
                }
            }
            Some(Hit::Bin(i)) => {
                self.focus = Focus::Bin;
                self.bin_selected = i;
                if double {
                    // A second click adds it to the end.
                    self.change(|p| {
                        p.append(i, None);
                    });
                    self.selected = Some(self.project.clips.len() - 1);
                    self.fitted = false;
                } else {
                    self.drag = Some(Drag::Media {
                        i,
                        x0: x,
                        y0: y,
                        active: false,
                    });
                }
            }
            Some(Hit::Param(row, part)) => {
                self.focus = Focus::Inspector;
                self.param_row = row;
                match part {
                    Part::Minus => self.adjust(row, -1.0),
                    Part::Plus => self.adjust(row, 1.0),
                    Part::Value => self.adjust(row, 1.0),
                    Part::Bar(_) => {
                        let rect = hits
                            .iter()
                            .find(|(_, h)| matches!(h, Hit::Param(r, Part::Bar(_)) if *r == row))
                            .map(|(r, _)| *r)
                            .unwrap_or_default();
                        if rect.w > 0 {
                            self.slider_to(row, f64::from(x - rect.x) / f64::from(rect.w));
                        }
                        self.drag = Some(Drag::Slider { row, rect });
                    }
                }
            }
            Some(Hit::Ruler) => {
                self.focus = Focus::Timeline;
                let t = self.x_time(l, x);
                if shift {
                    self.drag = Some(Drag::Range(t));
                    self.mark_in = Some(t);
                    self.mark_out = None;
                } else {
                    self.play(false);
                    self.drag = Some(Drag::Scrub);
                    self.seek(t);
                }
            }
            Some(Hit::Clip(i, edge)) if ctrl || shift => {
                // Ctrl adds or takes away one clip, Shift a run of them.
                self.focus = Focus::Timeline;
                self.music_selected = None;
                self.lane_sel = None;
                let _ = edge;
                if shift {
                    let from = self.selected.unwrap_or(i);
                    let (a, b) = (from.min(i), from.max(i));
                    self.picked.extend(a..=b);
                    self.selected = Some(i);
                } else if self.picked.remove(&i) || self.selected == Some(i) {
                    if self.selected == Some(i) {
                        self.selected = self.picked.iter().next_back().copied();
                        if let Some(n) = self.selected {
                            self.picked.remove(&n);
                        }
                    }
                } else {
                    if let Some(old) = self.selected {
                        self.picked.insert(old);
                    }
                    self.selected = Some(i);
                }
                let n = self.selection().len();
                if n > 1 {
                    self.say(format!(
                        "{n} clips chosen: panel changes apply to all of them"
                    ));
                }
            }
            Some(Hit::Clip(i, edge)) => {
                self.focus = Focus::Timeline;
                if !self.picked.contains(&i) {
                    self.picked.clear();
                }
                self.selected = Some(i);
                self.lane_sel = None;
                self.music_selected = None;
                let before = self.project.clone();
                self.history.record(&before);
                match edge {
                    Edge::Left => {
                        self.drag = Some(Drag::TrimLeft {
                            i,
                            start0: self.project.clips[i].start,
                            x0: x,
                        })
                    }
                    Edge::Right => {
                        self.drag = Some(Drag::TrimRight {
                            i,
                            end0: self.project.clips[i].end,
                            x0: x,
                        })
                    }
                    Edge::Body => {
                        let t = self.x_time(l, x);
                        self.play(false);
                        self.seek(t);
                        self.drag = Some(Drag::Clip {
                            i,
                            x0: x,
                            moved: false,
                        });
                    }
                }
            }
            Some(Hit::Music(j, edge)) => {
                self.focus = Focus::Inspector;
                self.music_selected = Some(j);
                self.selected = None;
                self.param_row = 0;
                let before = self.project.clone();
                self.history.record(&before);
                let m = &self.project.music[j];
                self.drag = Some(match edge {
                    Edge::Left => Drag::MusicLeft {
                        j,
                        start0: m.start,
                        at0: m.at,
                        x0: x,
                    },
                    Edge::Right => Drag::MusicRight {
                        j,
                        end0: m.end,
                        x0: x,
                    },
                    Edge::Body => Drag::Music {
                        j,
                        at0: m.at,
                        x0: x,
                    },
                });
            }
            Some(Hit::Window(i, what, edge)) => {
                self.focus = Focus::Timeline;
                self.selected = Some(i);
                self.picked.clear();
                self.music_selected = None;
                self.lane_sel = Some(what);
                self.show_group(what);
                if double {
                    self.open_place(i, what);
                    return;
                }
                let c = &self.project.clips[i];
                let (a0, z0) = if what == 0 {
                    super::project::window(c.effects.text_start, c.effects.text_length, c.length())
                } else {
                    super::project::window(c.pip.start, c.pip.length, c.length())
                };
                let before = self.project.clone();
                self.history.record(&before);
                self.drag = Some(Drag::Window {
                    i,
                    what,
                    edge,
                    a0,
                    z0,
                    x0: x,
                });
            }
            Some(Hit::CameraOff(i)) => {
                self.selected = Some(i);
                self.picked.clear();
                self.lane_sel = Some(1);
                self.show_group(1);
                if double {
                    self.change(|p| p.clips[i].pip.mode = 0);
                    self.say("Camera on for this clip");
                }
            }
            Some(Hit::LaneOff(k)) => {
                let label = ["Titles", "Camera", "Sound", "Music"][k as usize];
                let mut now = false;
                self.change(|p| {
                    let flag = match k {
                        0 => &mut p.hide_titles,
                        1 => &mut p.hide_camera,
                        2 => &mut p.mute_sound,
                        _ => &mut p.mute_music,
                    };
                    *flag = !*flag;
                    now = !*flag;
                });
                self.say(format!(
                    "{label} {} in the preview and the export",
                    if now { "on" } else { "off" }
                ));
            }
            Some(Hit::SoundCut(i)) => {
                self.focus = Focus::Timeline;
                self.selected = Some(i);
                self.picked.clear();
                self.lane_sel = None;
                let before = self.project.clone();
                self.history.record(&before);
                if double {
                    self.change(|p| p.clips[i].audio_offset = 0.0);
                    self.say("The sound cuts with the picture again");
                    return;
                }
                self.drag = Some(Drag::SoundCut {
                    i,
                    o0: self.project.clips[i].audio_offset,
                    x0: x,
                });
            }
            Some(Hit::Volume(i)) => {
                self.selected = Some(i);
                self.picked.clear();
                let before = self.project.clone();
                self.history.record(&before);
                if double {
                    self.change(|p| p.clips[i].volume = 1.0);
                    return;
                }
                self.drag = Some(Drag::Volume {
                    i,
                    v0: self.project.clips[i].volume,
                    y0: y,
                    h: (l.lane_audio.h - 10).max(8),
                });
            }
            Some(Hit::Marker(i, k)) => {
                let t = self
                    .project
                    .film_markers()
                    .into_iter()
                    .find(|m| m.2 == i && m.3 == k)
                    .map(|m| m.0);
                if let Some(t) = t {
                    self.play(false);
                    self.seek(t);
                }
                if double {
                    let name = self.project.clips[i].markers[k].name.clone();
                    self.overlay = Overlay::TextInput {
                        title: "Marker name (a chapter in the export)".into(),
                        text: name,
                        target: TextTarget::Marker(i, k),
                    };
                }
            }
            Some(Hit::Block) => {
                if matches!(self.overlay, Overlay::Settings | Overlay::Help) {
                    // The dimmed area beside a small dialog closes it.
                }
            }
            Some(Hit::Lane) => {
                self.focus = Focus::Timeline;
                // A double click on the title lane over a clip writes a title.
                if double && l.lane_title.has(x, y) {
                    let t = self.x_time(l, x);
                    if let Some((i, _)) = self.project.locate(t) {
                        self.selected = Some(i);
                        self.edit_title(i);
                        return;
                    }
                }
                self.selected = None;
                self.picked.clear();
                self.lane_sel = None;
                self.music_selected = None;
                self.play(false);
                let t = self.x_time(l, x);
                self.drag = Some(Drag::Scrub);
                self.seek(t);
            }
            None => {
                if !matches!(self.overlay, Overlay::None) {
                    // A click beside a dialog closes it.
                    let keep = matches!(self.overlay, Overlay::TextInput { .. });
                    if !keep {
                        self.overlay = Overlay::None;
                    }
                } else if l.video.has(x, y) {
                    let on = !self.playing;
                    self.play(on);
                }
            }
        }
    }

    fn slider_to(&mut self, row: usize, f: f64) {
        let param = if self.music_selected.is_some() {
            MUSIC_PARAMS.get(row).copied()
        } else {
            self.params().get(row).copied()
        };
        if let Some(param) = param {
            let (lo, hi, step) = match (self.music_selected, self.target()) {
                (None, Some(i)) => param.limits(&self.project.clips[i]),
                _ => param.range(),
            };
            let v = lo + (hi - lo) * f.clamp(0.0, 1.0);
            let v = (v / step).round() * step;
            self.set_param(row, v);
        }
    }

    fn motion(&mut self, l: &Layout, x: i32, y: i32) {
        self.mouse = (x, y);
        let Some(drag) = self.drag.clone() else {
            return;
        };
        let t = self.x_time(l, x);
        match drag {
            Drag::Place { image, what, grab } => {
                if let Overlay::Place(pl) = &self.overlay {
                    let fx = f64::from(x - image.x) / f64::from(image.w.max(1)) * f64::from(pl.fw);
                    let fy = f64::from(y - image.y) / f64::from(image.h.max(1)) * f64::from(pl.fh);
                    self.place_move(what, fx - grab.0, fy - grab.1);
                }
            }
            Drag::Scrub => {
                self.seek(t);
            }
            Drag::Window {
                i,
                what,
                edge,
                a0,
                z0,
                x0,
            } => {
                let Some(c) = self.project.clips.get(i) else {
                    return;
                };
                let len = c.length();
                let start = self.project.starts()[i];
                let dt = f64::from(x - x0) / self.zoom;
                let snap = |st: &Self, v: f64| st.snap(l, start + v) - start;
                let min = 0.2f64.min(len);
                let (a, z) = match edge {
                    Edge::Left => (snap(self, a0 + dt).clamp(0.0, z0 - min), z0),
                    Edge::Right => (a0, snap(self, z0 + dt).clamp(a0 + min, len)),
                    Edge::Body => {
                        let d = z0 - a0;
                        let a = snap(self, a0 + dt).clamp(0.0, (len - d).max(0.0));
                        (a, a + d)
                    }
                };
                // Running to the clip's end keeps following it (length 0).
                let length = if z >= len - 0.02 { 0.0 } else { z - a };
                let c = &mut self.project.clips[i];
                if what == 0 {
                    c.effects.text_start = a;
                    c.effects.text_length = length;
                } else {
                    c.pip.start = a;
                    c.pip.length = length;
                }
            }
            Drag::SoundCut { i, o0, x0 } => {
                if i >= self.project.clips.len() {
                    return;
                }
                let start = self.project.starts()[i];
                let dt = f64::from(x - x0) / self.zoom;
                // Dragging left makes the sound come early (J), right late (L).
                let at = self.snap(l, start - o0 + dt);
                let (lo, hi) = self.project.audio_offset_limits(i);
                let o = (start - at).clamp(lo, hi);
                self.project.clips[i].audio_offset = if o.abs() < 0.02 { 0.0 } else { o };
                let shown = self.project.clips[i].audio_offset;
                self.say(match shown {
                    o if o > 0.0 => format!("J cut: the sound comes {o:.2} s before the picture"),
                    o if o < 0.0 => format!("L cut: the sound runs {:.2} s past the picture", -o),
                    _ => "The sound cuts with the picture".into(),
                });
            }
            Drag::Volume { i, v0, y0, h } => {
                let dv = f64::from(y0 - y) / f64::from(h.max(1)) * 2.0;
                let v = ((v0 + dv) * 100.0).round() / 100.0;
                if let Some(c) = self.project.clips.get_mut(i) {
                    c.volume = v.clamp(0.0, 4.0);
                }
            }
            Drag::Range(a) => {
                let t = self.snap(l, t);
                self.mark_in = Some(a.min(t));
                self.mark_out = Some(a.max(t));
            }
            Drag::Slider { row, rect } => {
                if rect.w > 0 {
                    self.slider_to(row, f64::from(x - rect.x) / f64::from(rect.w));
                }
            }
            Drag::TrimLeft { i, start0, x0 } => {
                if i >= self.project.clips.len() {
                    return;
                }
                let c = &self.project.clips[i];
                let dt = f64::from(x - x0) / self.zoom * c.speed;
                let start = (start0 + dt).clamp(0.0, c.end - 0.1);
                self.project.clips[i].start = start;
                self.preview_source(i, start);
            }
            Drag::TrimRight { i, end0, x0 } => {
                if i >= self.project.clips.len() {
                    return;
                }
                let c = &self.project.clips[i];
                let media = &self.project.media[c.media];
                let limit = if media.still { 3600.0 } else { media.duration };
                let dt = f64::from(x - x0) / self.zoom * c.speed;
                let end = (end0 + dt).clamp(c.start + 0.1, limit);
                self.project.clips[i].end = end;
                self.preview_source(i, end - self.frame());
            }
            Drag::Clip { i, x0, .. } => {
                if (x - x0).abs() > 6 {
                    self.drag = Some(Drag::Clip { i, x0, moved: true });
                }
            }
            Drag::Music { j, at0, x0 } => {
                let at = (at0 + f64::from(x - x0) / self.zoom).max(0.0);
                let at = self.snap(l, at);
                if let Some(m) = self.project.music.get_mut(j) {
                    m.at = at;
                }
            }
            Drag::MusicLeft { j, start0, at0, x0 } => {
                let dt = f64::from(x - x0) / self.zoom;
                if let Some(m) = self.project.music.get_mut(j) {
                    let dt = dt.max(-start0).max(-at0).min(m.end - start0 - 0.1);
                    m.start = start0 + dt;
                    m.at = at0 + dt;
                }
            }
            Drag::MusicRight { j, end0, x0 } => {
                let dt = f64::from(x - x0) / self.zoom;
                let limit = self
                    .project
                    .music
                    .get(j)
                    .map(|m| self.project.media[m.media].duration)
                    .unwrap_or(0.0);
                if let Some(m) = self.project.music.get_mut(j) {
                    m.end = (end0 + dt).clamp(m.start + 0.1, limit);
                }
            }
            Drag::Area { fx, fy, image } => {
                let gx = (f64::from(x - image.x) / f64::from(image.w.max(1))).clamp(0.0, 1.0);
                let gy = (f64::from(y - image.y) / f64::from(image.h.max(1))).clamp(0.0, 1.0);
                if let Overlay::Area(a) = &mut self.overlay {
                    let w = (gx - fx).abs() * 100.0;
                    let h = (gy - fy).abs() * 100.0;
                    if w > 1.0 && h > 1.0 {
                        a.rect = [fx.min(gx) * 100.0, fy.min(gy) * 100.0, w, h];
                    }
                }
            }
            Drag::Media { i, x0, y0, active } => {
                if !active && ((x - x0).abs() > 6 || (y - y0).abs() > 6) {
                    self.drag = Some(Drag::Media {
                        i,
                        x0,
                        y0,
                        active: true,
                    });
                }
            }
        }
    }

    /// While trimming, show the frame at the edge straight from the source.
    fn preview_source(&mut self, i: usize, source: f64) {
        let media = &self.project.media[self.project.clips[i].media];
        if media.still || !media.has_video {
            return;
        }
        let path = media.path.display().to_string();
        let url = format!("trim:{path}");
        if let Some(m) = &mut self.mpv {
            if self.loaded_url != url {
                self.loaded_url = url;
                self.loaded_graph.clear();
                m.load(&path, source, true, "");
            } else {
                m.seek(source);
            }
        }
        self.preview = Preview::Timeline;
    }

    /// Where a dragged clip or media file would go.
    fn drop_index(&self, l: &Layout, x: i32) -> usize {
        let t = self.x_time(l, x);
        let starts = self.project.starts();
        for (i, c) in self.project.clips.iter().enumerate() {
            if t < starts[i] + c.length() / 2.0 {
                return i;
            }
        }
        self.project.clips.len()
    }

    fn release(&mut self, l: &Layout, button: u8, x: i32, y: i32) {
        if button != 1 {
            return;
        }
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Place { .. } => {
                self.after_change();
                self.refresh_place();
            }
            Drag::Window { .. } | Drag::SoundCut { .. } | Drag::Volume { .. } => {
                self.after_change();
            }
            Drag::TrimLeft { .. }
            | Drag::TrimRight { .. }
            | Drag::Music { .. }
            | Drag::MusicLeft { .. }
            | Drag::MusicRight { .. } => {
                self.after_change();
                self.reload(true);
            }
            Drag::Clip { i, moved, .. } => {
                if !moved {
                    // A plain click on one of several chosen clips picks it alone.
                    self.picked.clear();
                }
                if moved && l.timeline.has(x, y) {
                    let to = self.drop_index(l, x);
                    let to = if to > i { to - 1 } else { to };
                    if to != i {
                        self.change(|p| {
                            let c = p.clips.remove(i);
                            p.clips.insert(to, c);
                        });
                        self.selected = Some(to);
                        self.say("Clip moved");
                    }
                }
            }
            Drag::Media { i, active, .. } if active && l.timeline.has(x, y) => {
                if l.lane_music.has(x, y) {
                    let path = self.project.media[i].path.clone();
                    let t = self.x_time(l, x);
                    self.playhead = t;
                    self.add_music(&path);
                } else {
                    let at = self.drop_index(l, x);
                    self.change(|p| {
                        p.append(i, Some(at));
                    });
                    self.selected = Some(at);
                }
                self.fitted = false;
            }
            _ => {}
        }
    }
}

fn autosave_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".local/state"))
        .join("niruclip/autosave.niruclip")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn notify(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "NIRUCLIP", title, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn button(
    p: &mut Painter,
    t: &Theme,
    x: i32,
    y: i32,
    label: &str,
    action: Action,
    s: i32,
    primary: bool,
) -> i32 {
    let size = (s as u32 * 9 / 10).max(10);
    let w = p.tx.width(label, size) + s;
    let h = s * 17 / 10;
    let bg = if primary { t.accent } else { t.raised };
    let fg = if primary { t.background } else { t.foreground };
    p.cv.round(x, y, w, h, s / 3, bg);
    let ty = y + (h - (size as i32 * 5 / 4)) / 2;
    p.tx.draw(&mut p.cv, x + s / 2, ty, label, size, fg);
    p.hits.push((Rect::new(x, y, w, h), Hit::Button(action)));
    w
}

fn draw(p: &mut Painter, st: &mut State, l: &Layout, w: i32, h: i32) {
    let t = st.theme.clone();
    let s = t.size as i32;
    p.cv.no_clip();
    p.cv.fill(0, 0, w, h, t.background);
    if st.playing {
        st.follow(l);
    }
    // Keep the chosen settings row in view.
    if st.focus == Focus::Inspector && st.music_selected.is_none() {
        let mut y = 0;
        let mut group = "";
        let mut at = 0;
        for (row, param) in st.params().iter().enumerate() {
            if param.group() != group {
                group = param.group();
                y += s / 3 + s * 3 / 2;
            }
            if row == st.param_row {
                at = y;
            }
            y += s * 2;
        }
        let view = l.inspector.h - s * 5 - s * 2;
        if at - st.inspector_scroll > view {
            st.inspector_scroll = at - view;
        } else if at - s * 2 < st.inspector_scroll {
            st.inspector_scroll = (at - s * 2).max(0);
        }
    }
    draw_top(p, st, l, &t, s);
    draw_bin(p, st, l, &t, s);
    draw_inspector(p, st, l, &t, s);
    draw_center(p, st, l, &t, s);
    draw_transport(p, st, l, &t, s);
    draw_timeline(p, st, l, &t, s);
    p.cv.no_clip();
    match &st.overlay {
        Overlay::None => {}
        Overlay::Browser(_) => draw_browser(p, st, l, &t, s),
        Overlay::Export(_) => draw_export(p, st, l, &t, s),
        Overlay::Settings => draw_settings(p, st, l, &t, s),
        Overlay::Record(_) => draw_record(p, st, l, &t, s),
        Overlay::Help => draw_help(p, l, &t, s),
        Overlay::TextInput { .. } => draw_text_input(p, st, l, &t, s),
        Overlay::Film { .. } => draw_film(p, st, l, &t, s),
        Overlay::Pauses(_) => draw_pauses(p, st, l, &t, s),
        Overlay::Captions(_) => draw_captions(p, st, l, &t, s),
        Overlay::Fonts(_) => draw_fonts(p, st, l, &t, s),
        Overlay::Area(_) => draw_area(p, st, l, &t, s),
        Overlay::Place(_) => draw_place(p, st, l, &t, s),
    }
    // Dragging media from the bin.
    if let Some(Drag::Media {
        i, active: true, ..
    }) = &st.drag
    {
        if let Some(m) = st.project.media.get(*i) {
            let (mx, my) = st.mouse;
            let name = m.name();
            let tw = p.tx.width(&name, t.size) + s;
            p.cv.round(mx + 12, my + 12, tw, s * 2, s / 3, t.accent);
            p.tx.draw(
                &mut p.cv,
                mx + 12 + s / 2,
                my + 12 + s / 3,
                &name,
                t.size,
                t.background,
            );
        }
    }
    // Status line.
    if let Some((text, _, warn)) = &st.status {
        let size = t.size;
        let tw = p.tx.width(text, size).min(w - 4 * s);
        let bx = (w - tw) / 2 - s;
        // In the top bar: the video area is mpv's window and would hide it.
        let by = l.top.y + (l.top.h - s * 2) / 2;
        p.cv.round(
            bx,
            by,
            tw + 2 * s,
            s * 2,
            s / 2,
            if *warn { t.error } else { t.raised },
        );
        let fg = if *warn { t.background } else { t.foreground };
        p.tx.fit(&mut p.cv, bx + s, by + s / 3, text, size, fg, tw);
    }
}

fn draw_top(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.top;
    p.cv.fill(r.x, r.y, r.w, r.h, t.surface);
    p.cv.fill(r.x, r.y + r.h - 1, r.w, 1, t.border);
    let y = r.y + (r.h - s * 17 / 10) / 2;
    let mut x = s;
    p.tx.draw(&mut p.cv, x, y + s / 5, "NIRUCLIP", t.size, t.accent);
    x += p.tx.width("NIRUCLIP", t.size) + s * 3 / 2;
    for (label, a) in [
        ("Open", Action::Open),
        ("Import", Action::Import),
        ("Music", Action::Music),
        ("Save", Action::Save),
        ("● Record", Action::Record),
        ("Film…", Action::Film),
    ] {
        x += button(p, t, x, y, label, a, s, false) + s / 2;
    }
    x += s / 2;
    x += button(p, t, x, y, "Undo", Action::Undo, s, false) + s / 2;
    x += button(p, t, x, y, "Redo", Action::Redo, s, false) + s / 2;
    let _ = x;
    // Right side: export, settings, help.
    let mut rx = r.w - s;
    for (label, a, primary) in [
        ("?", Action::Help, false),
        ("Settings", Action::Settings, false),
        ("Export", Action::Export, true),
    ] {
        let size = (s as u32 * 9 / 10).max(10);
        let bw = p.tx.width(label, size) + s;
        rx -= bw;
        button(p, t, rx, y, label, a, s, primary);
        rx -= s / 2;
    }
    // Captions being made.
    if let Some((f, text)) = &st.caption_progress {
        let label = format!("Captions {:.0} %", f * 100.0);
        let lw = p.tx.width(&label, t.size * 8 / 10);
        let bx = if st.job.is_some() {
            rx - s * 30
        } else {
            rx - s * 12
        };
        p.tx.draw(
            &mut p.cv,
            bx - lw,
            y + s / 5,
            &label,
            t.size * 8 / 10,
            t.warning,
        );
        let _ = text;
        p.hits.push((
            Rect::new(bx - lw, y, lw, s * 2),
            Hit::Button(Action::Captions),
        ));
    }
    // Export progress.
    if let Some(job) = &st.job {
        let bw = s * 12;
        let bx = rx - bw - s;
        let by = y + s / 2;
        p.cv.round(bx, by, bw, s * 7 / 10, s / 3, t.raised);
        p.cv.round(
            bx,
            by,
            (bw as f64 * job.fraction) as i32,
            s * 7 / 10,
            s / 3,
            t.success,
        );
        let label = format!("Export {:.0} %", job.fraction * 100.0);
        let lw = p.tx.width(&label, t.size * 8 / 10);
        p.tx.draw(
            &mut p.cv,
            bx - lw - s / 2,
            by - s / 5,
            &label,
            t.size * 8 / 10,
            t.muted,
        );
        p.hits.push((
            Rect::new(bx, by - s / 2, bw, s * 2),
            Hit::Button(Action::ExportCancel),
        ));
    }
}

fn panel_title(p: &mut Painter, t: &Theme, r: Rect, s: i32, title: &str, focused: bool) {
    p.tx.draw(
        &mut p.cv,
        r.x + s,
        r.y + s / 2,
        title,
        t.size * 85 / 100,
        if focused { t.accent } else { t.muted },
    );
}

fn draw_bin(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.bin;
    p.cv.fill(r.x, r.y, r.w, r.h, t.surface);
    p.cv.fill(r.x + r.w - 1, r.y, 1, r.h, t.border);
    panel_title(p, t, r, s, "MEDIA", st.focus == Focus::Bin);
    let row_h = s * 3;
    let mut y = r.y + s * 2;
    p.cv.set_clip(r.x, y, r.w - 1, r.h - s * 2);
    if st.project.media.is_empty() {
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            y + s / 2,
            "No media yet.",
            t.size * 9 / 10,
            t.muted,
            r.w - 2 * s,
        );
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            y + s * 2,
            "Import with Ctrl+I",
            t.size * 9 / 10,
            t.muted,
            r.w - 2 * s,
        );
    }
    for (i, m) in st.project.media.iter().enumerate() {
        let sel = i == st.bin_selected;
        if sel {
            p.cv.round(
                r.x + s / 3,
                y,
                r.w - s * 2 / 3,
                row_h - s / 4,
                s / 3,
                if st.focus == Focus::Bin {
                    t.selected
                } else {
                    t.raised
                },
            );
        }
        // Thumbnail.
        let th = row_h - s / 2 - s / 4;
        let tw = th * 16 / 9;
        let tx = r.x + s / 2 + s / 4;
        let ty = y + s / 4;
        if m.is_card() {
            p.cv.fill(tx, ty, tw, th, 0x101014);
            p.tx.draw(
                &mut p.cv,
                tx + tw / 2 - s / 2,
                ty + th / 2 - s / 2,
                "Aa",
                t.size,
                0xffffff,
            );
        } else if let Some(th_set) = st.cache.thumbs.get(&m.path) {
            let f = &th_set.frames[th_set.frames.len() / 3];
            p.cv.image(tx, ty, tw, th, f, th_set.width as i32, THUMB_H as i32);
        } else {
            p.cv.fill(tx, ty, tw, th, t.raised);
            let icon = if m.has_video { "▶" } else { "♪" };
            p.tx.draw(
                &mut p.cv,
                tx + tw / 2 - s / 3,
                ty + th / 2 - s / 2,
                icon,
                t.size,
                t.muted,
            );
        }
        let nx = tx + tw + s / 2;
        let avail = r.x + r.w - nx - s / 2;
        p.tx.fit(
            &mut p.cv,
            nx,
            y + s / 4,
            &m.name(),
            t.size * 9 / 10,
            t.foreground,
            avail,
        );
        let kind = if m.is_card() {
            "Text on a colour: Ctrl+T".to_string()
        } else if m.still {
            "Picture".to_string()
        } else if m.has_video {
            format!("{}×{} · {}", m.width, m.height, clock(m.duration))
        } else {
            format!("Sound · {}", clock(m.duration))
        };
        p.tx.fit(
            &mut p.cv,
            nx,
            y + s * 3 / 2,
            &kind,
            t.size * 75 / 100,
            t.muted,
            avail,
        );
        p.hits.push((Rect::new(r.x, y, r.w, row_h), Hit::Bin(i)));
        y += row_h;
    }
    p.cv.no_clip();
    // Bottom buttons.
    let by = r.y + r.h - s * 5 / 2;
    let bx = r.x + s / 2;
    let w1 = button(p, t, bx, by, "+ Import", Action::Import, s, false);
    let w2 = button(
        p,
        t,
        bx + w1 + s / 2,
        by,
        "+ Music",
        Action::Music,
        s,
        false,
    );
    if bx + w1 + w2 + s * 6 < r.x + r.w {
        button(p, t, bx + w1 + w2 + s, by, "+ Card", Action::Card, s, false);
    }
}

fn draw_inspector(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.inspector;
    p.cv.fill(r.x, r.y, r.w, r.h, t.surface);
    p.cv.fill(r.x, r.y, 1, r.h, t.border);
    let focused = st.focus == Focus::Inspector;
    let small = t.size * 85 / 100;
    if let Some(j) = st.music_selected.filter(|j| *j < st.project.music.len()) {
        let m = &st.project.music[j];
        panel_title(p, t, r, s, "MUSIC", focused);
        let name = st.project.media[m.media].name();
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            r.y + s * 2,
            &name,
            t.size,
            t.foreground,
            r.w - 2 * s,
        );
        let info = format!("at {} · {}", clock(m.at), clock(m.length()));
        p.tx.draw(&mut p.cv, r.x + s, r.y + s * 7 / 2, &info, small, t.muted);
        let mut y = r.y + s * 5;
        for (row, param) in MUSIC_PARAMS.iter().enumerate() {
            let v = param.get_music(m);
            param_row(
                p,
                t,
                r,
                s,
                y,
                row,
                param.label(),
                &param.show_music(m),
                Some(v),
                *param,
                focused && row == st.param_row,
            );
            y += s * 5 / 2;
        }
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            y + s,
            "Drag it in the Music lane; Delete removes it.",
            small,
            t.muted,
            r.w - 2 * s,
        );
        return;
    }
    let Some(i) = st.target() else {
        panel_title(p, t, r, s, "CLIP", focused);
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            r.y + s * 2,
            "Select a clip on the timeline.",
            small,
            t.muted,
            r.w - 2 * s,
        );
        return;
    };
    let c = &st.project.clips[i];
    let m = &st.project.media[c.media];
    panel_title(
        p,
        t,
        r,
        s,
        &match st.selection().len() {
            n if n > 1 => format!("CLIP {} OF {} · {n} CHOSEN", i + 1, st.project.clips.len()),
            _ => format!("CLIP {} OF {}", i + 1, st.project.clips.len()),
        },
        focused,
    );
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        r.y + s * 2,
        &m.name(),
        t.size,
        t.foreground,
        r.w - 2 * s,
    );
    let info = format!(
        "{} – {} · {}",
        clock(c.start),
        clock(c.end),
        clock(c.length())
    );
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        r.y + s * 7 / 2,
        &info,
        small,
        t.muted,
        r.w - 2 * s,
    );
    let top = r.y + s * 5;
    let row_h = s * 2;
    p.cv.set_clip(r.x + 1, top, r.w - 1, r.h - (top - r.y));
    // Keep the selected row in view.
    let mut y = top - st.inspector_scroll;
    let mut group = "";
    let params = st.params();
    for (row, param) in params.iter().enumerate() {
        if param.group() != group {
            group = param.group();
            y += s / 3;
            p.tx.draw(
                &mut p.cv,
                r.x + s,
                y,
                &group.to_uppercase(),
                small * 9 / 10,
                t.muted,
            );
            y += s * 3 / 2;
        }
        let v = if param.slider() {
            Some(param.get(c))
        } else {
            None
        };
        param_row(
            p,
            t,
            r,
            s,
            y,
            row,
            param.label(),
            &if *param == Param::Pip {
                pip_label(&st.project, c)
            } else {
                param.show(c)
            },
            v,
            *param,
            focused && row == st.param_row,
        );
        y += row_h;
    }
    // What the panel can do beyond its rows.
    y += s / 2;
    let hint = if params.get(st.param_row).is_some_and(|p| p.is_time()) && focused {
        "Enter takes the time from the playhead."
    } else if c.effects.text.trim().is_empty() {
        "T writes a title; Ctrl+T adds a title card."
    } else {
        "P renders a preview with titles and effects."
    };
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y,
        hint,
        small * 9 / 10,
        t.muted,
        r.w - 2 * s,
    );
    p.cv.no_clip();
}

#[allow(clippy::too_many_arguments)]
fn param_row(
    p: &mut Painter,
    t: &Theme,
    r: Rect,
    s: i32,
    y: i32,
    row: usize,
    label: &str,
    value: &str,
    slider: Option<f64>,
    param: Param,
    selected: bool,
) {
    let size = t.size * 9 / 10;
    let h = s * 2 - s / 4;
    if selected {
        p.cv.round(
            r.x + s / 3,
            y - s / 8,
            r.w - s * 2 / 3,
            h,
            s / 3,
            t.selected,
        );
    }
    let lw = s * 6;
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y + s / 6,
        label,
        size,
        t.foreground,
        lw - s / 2,
    );
    let bx = r.x + s + lw;
    let bw = r.w - lw - s * 2 - s * 3;
    let minus = Rect::new(bx, y, s * 3 / 2, h - s / 4);
    let plus = Rect::new(r.x + r.w - s * 5 / 2, y, s * 3 / 2, h - s / 4);
    if let Some(v) = slider {
        let (lo, hi, _) = param.range();
        let f = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        let track = Rect::new(bx + s * 2, y + h / 2 - s / 8, bw - s * 2, s / 4);
        p.cv.round(track.x, track.y, track.w, track.h, s / 8, t.raised);
        p.cv.round(
            track.x,
            track.y,
            (f64::from(track.w) * f) as i32,
            track.h,
            s / 8,
            t.accent,
        );
        let kx = track.x + (f64::from(track.w) * f) as i32;
        p.cv.round(
            kx - s / 3,
            track.y - s / 4,
            s * 2 / 3,
            s * 3 / 4,
            s / 3,
            t.foreground,
        );
        let vw = p.tx.width(value, size * 9 / 10);
        p.tx.draw(
            &mut p.cv,
            track.x + track.w - vw,
            y - s / 3,
            value,
            size * 8 / 10,
            t.muted,
        );
        let bar = Rect::new(track.x, y, track.w, h);
        p.hits.push((bar, Hit::Param(row, Part::Bar(0.0))));
        // The fraction is computed from the mouse on press.
        let _ = bar;
    } else {
        let vr = Rect::new(bx + s * 2, y, bw - s * 2, h - s / 4);
        p.cv.round(vr.x, vr.y, vr.w, vr.h, s / 3, t.raised);
        p.tx.fit(
            &mut p.cv,
            vr.x + s / 2,
            vr.y + s / 8,
            value,
            size,
            t.foreground,
            vr.w - s,
        );
        p.hits.push((vr, Hit::Param(row, Part::Value)));
    }
    if !param.opens() {
        p.cv.round(minus.x, minus.y, minus.w, minus.h, s / 3, t.raised);
        p.tx.draw(
            &mut p.cv,
            minus.x + minus.w / 2 - s / 4,
            minus.y,
            "−",
            size,
            t.foreground,
        );
        p.hits.push((minus, Hit::Param(row, Part::Minus)));
        p.cv.round(plus.x, plus.y, plus.w, plus.h, s / 3, t.raised);
        p.tx.draw(
            &mut p.cv,
            plus.x + plus.w / 2 - s / 4,
            plus.y,
            "+",
            size,
            t.foreground,
        );
        p.hits.push((plus, Hit::Param(row, Part::Plus)));
    }
}

fn draw_center(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.video;
    p.cv.fill(r.x, r.y, r.w, r.h, 0x000000);
    if !st.project.clips.is_empty() {
        return;
    }
    // Welcome.
    p.cv.fill(r.x, r.y, r.w, r.h, t.background);
    let cx = r.x + r.w / 2;
    let mut y = r.y + r.h / 4;
    let big = t.size * 3 / 2;
    let title = "Start a film";
    let tw = p.tx.width(title, big);
    p.tx.draw(&mut p.cv, cx - tw / 2, y, title, big, t.foreground);
    y += s * 3;
    let bw = s * 9;
    let x0 = cx - (bw * 3 + s * 2) / 2;
    button(p, t, x0, y, "Open…  Ctrl+O", Action::Open, s, true);
    button(
        p,
        t,
        x0 + 2 * (bw + s),
        y,
        "● Record…  Ctrl+R",
        Action::Record,
        s,
        false,
    );
    button(
        p,
        t,
        x0 + bw + s,
        y,
        "Import…  Ctrl+I",
        Action::Import,
        s,
        false,
    );
    y += s * 4;
    let hint = "Or start it with files: niruclip edit FILE…";
    let hw = p.tx.width(hint, t.size * 9 / 10);
    p.tx.draw(&mut p.cv, cx - hw / 2, y, hint, t.size * 9 / 10, t.muted);
    y += s * 3;
    let recent: Vec<PathBuf> = st
        .settings
        .recent
        .iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
    let autosave = autosave_path();
    if autosave.exists() {
        let label = "Recover the last unsaved session";
        let lw = p.tx.width(label, t.size) + s;
        let rx = cx - lw / 2;
        p.cv.round(rx, y, lw, s * 2, s / 3, t.warning);
        p.tx.draw(
            &mut p.cv,
            rx + s / 2,
            y + s / 4,
            label,
            t.size,
            t.background,
        );
        p.hits
            .push((Rect::new(rx, y, lw, s * 2), Hit::Button(Action::Recover)));
        y += s * 3;
    }
    if !recent.is_empty() {
        let head = "Recent projects";
        let hw = p.tx.width(head, t.size * 85 / 100);
        p.tx.draw(&mut p.cv, cx - hw / 2, y, head, t.size * 85 / 100, t.muted);
        y += s * 3 / 2;
        for (i, path) in recent.iter().take(6).enumerate() {
            let name = path.display().to_string();
            let nw = p.tx.width(&name, t.size).min(r.w - 2 * s);
            let row = Rect::new(cx - nw / 2 - s / 2, y, nw + s, s * 9 / 5);
            if row.has(st.mouse.0, st.mouse.1) {
                p.cv.round(row.x, row.y, row.w, row.h, s / 3, t.raised);
            }
            p.tx.fit(
                &mut p.cv,
                cx - nw / 2,
                y + s / 5,
                &name,
                t.size,
                t.accent,
                r.w - 2 * s,
            );
            let idx = st
                .settings
                .recent
                .iter()
                .position(|p| p == path)
                .unwrap_or(i);
            p.hits.push((row, Hit::Button(Action::Recent(idx))));
            y += s * 2;
        }
    }
}

fn draw_transport(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.transport;
    p.cv.fill(r.x, r.y, r.w, r.h, t.surface);
    p.cv.fill(r.x, r.y, r.w, 1, t.border);
    let y = r.y + (r.h - s * 17 / 10) / 2;
    let mut x = r.x + s / 2;
    let play = if st.playing { "❚❚" } else { "▶" };
    for (label, a, primary) in [
        ("|◀", Action::GoStart, false),
        ("◀◀", Action::PrevCut, false),
        (play, Action::Play, true),
        ("▶▶", Action::NextCut, false),
        ("▶|", Action::GoEnd, false),
    ] {
        x += button(p, t, x, y, label, a, s, primary) + s / 4;
    }
    x += s / 2;
    let d = st.project.duration();
    let fps = st.project_fps();
    let time = format!("{} / {}", timecode(st.playhead, fps), timecode(d, fps));
    let tw = p.tx.width(&time, t.size);
    p.tx.draw(&mut p.cv, x, y + s / 5, &time, t.size, t.foreground);
    x += tw + s;
    for (label, a, on) in [
        ("↻ Loop", Action::Loop, st.looping),
        ("Snap", Action::Snap, st.snapping),
        ("Full", Action::FullScreen, false),
    ] {
        x += button(p, t, x, y, label, a, s, on) + s / 4;
    }
    x += s / 2;
    if st.rate > 1.0 && st.playing {
        p.tx.draw(
            &mut p.cv,
            x,
            y + s / 5,
            &format!("{}×", st.rate),
            t.size,
            t.warning,
        );
        x += s * 2;
    }
    if let Preview::Rendered { .. } = st.preview {
        p.tx.draw(
            &mut p.cv,
            x,
            y + s / 5,
            "RENDERED PREVIEW (Esc)",
            t.size * 85 / 100,
            t.success,
        );
        x += s * 12;
    } else if st.preview_job.is_some() {
        let f = st
            .preview_job
            .as_ref()
            .map(|(j, _)| j.fraction)
            .unwrap_or(0.0);
        p.tx.draw(
            &mut p.cv,
            x,
            y + s / 5,
            &format!("Rendering preview {:.0} %", f * 100.0),
            t.size * 85 / 100,
            t.warning,
        );
        x += s * 12;
    }
    let _ = x;
    // Right: split, marks, preview.
    let mut rx = r.x + r.w - s;
    let mut items: Vec<(&str, Action, bool)> = vec![
        ("Preview effects  P", Action::Preview, false),
        ("Split  S", Action::Split, false),
    ];
    if st.mark_in.is_some() && st.mark_out.is_some() {
        items.insert(0, ("Keep part  C", Action::KeepRange, false));
        items.insert(0, ("Cut part  X", Action::RemoveRange, true));
    }
    items.push(("Out  O", Action::MarkOut, false));
    items.push(("In  I", Action::MarkIn, false));
    for (label, a, primary) in items {
        let size = (s as u32 * 9 / 10).max(10);
        let bw = p.tx.width(label, size) + s;
        rx -= bw;
        if rx < x {
            break;
        }
        button(p, t, rx, y, label, a, s, primary);
        rx -= s / 2;
    }
}

fn ruler_step(zoom: f64) -> f64 {
    for step in [
        0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
    ] {
        if step * zoom >= 90.0 {
            return step;
        }
    }
    3600.0
}

fn draw_timeline(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = l.timeline;
    p.cv.fill(r.x, r.y, r.w, r.h, t.surface);
    p.cv.fill(r.x, r.y, r.w, 1, t.border);
    let small = t.size * 8 / 10;
    // Lane labels, with a switch on the lanes that can be turned off.
    let pr = &st.project;
    for (lane, label, switch) in [
        (l.lane_title, "Titles", Some((0u8, !pr.hide_titles))),
        (l.lane_camera, "Camera", Some((1, !pr.hide_camera))),
        (l.lane_video, "Video", None),
        (l.lane_audio, "Sound", Some((2, !pr.mute_sound))),
        (l.lane_music, "Music", Some((3, !pr.mute_music))),
    ] {
        p.cv.fill(0, lane.y, l.track_x - 2, lane.h - 2, t.raised);
        let ty = lane.y + (lane.h - 2) / 2 - s / 2;
        p.tx.fit(
            &mut p.cv,
            s / 3,
            ty,
            label,
            small,
            t.muted,
            l.track_x - s * 2,
        );
        if let Some((k, on)) = switch {
            let bw = s * 3 / 2;
            let bh = (lane.h - 6).clamp(4, s * 5 / 4);
            let bx = l.track_x - bw - s / 4;
            let by = lane.y + (lane.h - 2 - bh) / 2;
            p.cv.round(bx, by, bw, bh, s / 4, if on { t.success } else { t.border });
            let mark = match (k, on) {
                (0 | 1, true) => "●",
                (0 | 1, false) => "○",
                (_, true) => "♪",
                (_, false) => "×",
            };
            let mw = p.tx.width(mark, small);
            p.tx.draw(
                &mut p.cv,
                bx + (bw - mw) / 2,
                by + (bh - small as i32 * 5 / 4) / 2,
                mark,
                small,
                t.background,
            );
            p.hits.push((Rect::new(bx, by, bw, bh), Hit::LaneOff(k)));
        }
        p.cv.fill(lane.x, lane.y, lane.w, lane.h - 2, t.background);
        p.hits.push((lane, Hit::Lane));
    }
    // Ruler.
    let ru = l.ruler;
    p.hits.push((ru, Hit::Ruler));
    p.cv.set_clip(ru.x, ru.y, ru.w, r.h);
    let step = ruler_step(st.zoom);
    let first = (st.scroll / step).floor() * step;
    let mut tt = first;
    let end = st.scroll + f64::from(ru.w) / st.zoom;
    while tt <= end {
        let x = st.time_x(l, tt);
        p.cv.fill(x, ru.y + ru.h / 2, 1, ru.h / 2, t.muted);
        p.tx.draw(
            &mut p.cv,
            x + 3,
            ru.y + s / 6,
            clock(tt).trim_end_matches(".00"),
            small,
            t.muted,
        );
        for k in 1..5 {
            let mx = st.time_x(l, tt + step * f64::from(k) / 5.0);
            p.cv.fill(mx, ru.y + ru.h * 3 / 4, 1, ru.h / 4, t.border);
        }
        tt += step;
    }
    // Clips.
    let starts = st.project.starts();
    let spans = st.project.sound_spans();
    let lv = l.lane_video;
    let la = l.lane_audio;
    let target = st.target();
    for (i, c) in st.project.clips.iter().enumerate() {
        let m = &st.project.media[c.media];
        let x0 = st.time_x(l, starts[i]);
        let x1 = st.time_x(l, starts[i] + c.length());
        if x1 < lv.x || x0 > lv.x + lv.w {
            continue;
        }
        let w = (x1 - x0).max(2);
        let sel = st.selected == Some(i) || st.picked.contains(&i);
        // Video strip.
        let vy = lv.y + 2;
        let vh = lv.h - 6;
        p.cv.round(x0, vy, w, vh, s / 4, if sel { t.accent } else { t.border });
        let inner = Rect::new(x0 + 2, vy + 2, w - 4, vh - 4);
        p.cv.set_clip(inner.x.max(lv.x), inner.y, inner.w, inner.h);
        if let Some(th) = st.cache.thumbs.get(&m.path) {
            let tw = (th.width as i32 * inner.h / THUMB_H as i32).max(8);
            let mut x = inner.x;
            while x < inner.x + inner.w {
                let local = f64::from(x - inner.x) / st.zoom;
                let src = c.source_at(local);
                let idx = if m.still {
                    0
                } else {
                    ((src / th.step) as usize).min(th.frames.len() - 1)
                };
                p.cv.image(
                    x,
                    inner.y,
                    tw,
                    inner.h,
                    &th.frames[idx],
                    th.width as i32,
                    THUMB_H as i32,
                );
                x += tw;
            }
            p.cv.blend(inner.x, inner.y, inner.w, s * 5 / 4, 0x000000, 140);
        } else if m.is_card() {
            p.cv.fill(inner.x, inner.y, inner.w, inner.h, c.effects.card_colour);
        } else {
            p.cv.fill(inner.x, inner.y, inner.w, inner.h, t.raised);
        }
        let mut label = m.name();
        if m.is_card() && !c.effects.text.is_empty() {
            label = format!("“{}”", c.effects.text.replace('\n', " "));
        }
        if (c.speed - 1.0).abs() > 1e-6 {
            label = format!("{:.2}× {label}", c.speed);
        }
        if !c.effects.is_default() {
            label = format!("✦ {label}");
        }
        p.tx.fit(
            &mut p.cv,
            inner.x + s / 3,
            inner.y + s / 8,
            &label,
            small,
            0xffffff,
            inner.w - s / 2,
        );
        p.cv.set_clip(lv.x, r.y, lv.w, r.h);
        // Transition marker.
        if c.transition.is_some() && i > 0 {
            let ov = st.project.overlap(i);
            let tw = ((ov * st.zoom) as i32).max(6);
            p.cv.blend(x0, vy, tw, vh, t.warning, 110);
            p.tx.draw(
                &mut p.cv,
                x0 + 2,
                vy + vh - s * 5 / 4,
                "⇄",
                small,
                t.background,
            );
        }
        // Fades.
        if c.fade_in > 0.0 {
            let fw = (c.fade_in * st.zoom) as i32;
            p.cv.blend(x0, vy, fw, vh, 0x000000, 90);
        }
        if c.fade_out > 0.0 {
            let fw = (c.fade_out * st.zoom) as i32;
            p.cv.blend(x1 - fw, vy, fw, vh, 0x000000, 90);
        }
        p.hits
            .push((Rect::new(x0, lv.y, w, lv.h), Hit::Clip(i, Edge::Body)));
        let edge = (s / 2).min(w / 4).max(3);
        p.hits
            .push((Rect::new(x0, lv.y, edge, lv.h), Hit::Clip(i, Edge::Left)));
        p.hits.push((
            Rect::new(x1 - edge, lv.y, edge, lv.h),
            Hit::Clip(i, Edge::Right),
        ));
        // Sound wave, over the clip's own stretch of sound (a J or L cut
        // moves it off the picture).
        let (sa, sb) = spans[i];
        let (ax0, ax1) = (st.time_x(l, sa), st.time_x(l, sb));
        let aw = (ax1 - ax0).max(2);
        let ay = la.y + 2;
        let ah = la.h - 6;
        p.cv.round(
            ax0,
            ay,
            aw,
            ah,
            s / 4,
            if sel { t.selected } else { t.raised },
        );
        let mut volume_hit = None;
        if m.has_audio && !m.still {
            let colour = if c.mute || st.project.mute_sound {
                t.muted
            } else {
                t.success
            };
            if let Some(peaks) = st.cache.peaks.get(&m.path) {
                let (from, speed, start, dur) = (sa - starts[i], c.speed, c.start, m.duration);
                draw_wave(
                    p,
                    st,
                    peaks,
                    ax0,
                    ax1,
                    ay,
                    ah,
                    colour,
                    |local| (start + (local + from) * speed).clamp(0.0, dur),
                    c.volume,
                    l,
                );
            }
            if c.mute {
                p.tx.draw(&mut p.cv, ax0 + s / 3, ay + 2, "muted", small, t.foreground);
            } else {
                // The volume line: 100 % at the middle, 200 % at the top.
                let vy = ay + ah - ((c.volume.min(2.0) / 2.0) * f64::from(ah - 4)) as i32 - 2;
                let hot = matches!(st.drag, Some(Drag::Volume { i: d, .. }) if d == i);
                p.cv.fill(ax0 + 2, vy - 1, aw - 4, 3, t.background);
                p.cv.fill(ax0 + 2, vy, aw - 4, if hot { 2 } else { 1 }, t.warning);
                if hot || (sel && aw > s * 6) {
                    let label = volume_label(c.volume);
                    let lw = p.tx.width(&label, small);
                    p.tx.draw(
                        &mut p.cv,
                        ax1 - lw - s / 3,
                        (vy - s * 5 / 4).max(ay),
                        &label,
                        small,
                        t.warning,
                    );
                }
                volume_hit = Some(Rect::new(ax0, vy - 3, aw, 7));
            }
        }
        p.hits.push((
            Rect::new(ax0.max(x0), la.y, (ax1.min(x1) - ax0.max(x0)).max(2), la.h),
            Hit::Clip(i, Edge::Body),
        ));
        if let Some(r) = volume_hit {
            p.hits.push((r, Hit::Volume(i)));
        }
        // The sound's cut into this clip can move: a J or L cut.
        if i > 0 && c.transition.is_none() {
            let o = c.audio_offset;
            if o.abs() > 1e-6 {
                let tag = if o > 0.0 { "J" } else { "L" };
                p.cv.fill(ax0 - 1, la.y, 2, la.h - 2, t.warning);
                p.tx.draw(
                    &mut p.cv,
                    ax0 + 3,
                    ay + ah - s * 5 / 4,
                    tag,
                    small,
                    t.warning,
                );
            }
            let hw = (s / 2).max(6);
            p.hits
                .push((Rect::new(ax0 - hw / 2, la.y, hw, la.h), Hit::SoundCut(i)));
        }
        // Titles and the picture-in-picture, on their lanes.
        let len = c.length();
        let card = m.is_card();
        if !card && !c.effects.text.trim().is_empty() {
            let e = &c.effects;
            let (a, z) = super::project::window(e.text_start, e.text_length, len);
            let label = e.text.replace('\n', " ");
            let on = st.selected == Some(i) && st.lane_sel == Some(0);
            lane_item(
                p,
                st,
                l,
                t,
                s,
                l.lane_title,
                starts[i] + a,
                starts[i] + z,
                &label,
                i,
                0,
                on,
                st.project.hide_titles,
            );
        }
        if export::pip_source(&st.project, c).is_some() {
            let (a, z) = super::project::window(c.pip.start, c.pip.length, len);
            let label = pip_label(&st.project, c);
            let on = st.selected == Some(i) && st.lane_sel == Some(1);
            lane_item(
                p,
                st,
                l,
                t,
                s,
                l.lane_camera,
                starts[i] + a,
                starts[i] + z,
                &label,
                i,
                1,
                on,
                st.project.hide_camera,
            );
        } else if c.pip.mode == 1 && !card && !m.still && m.own_camera().is_some() {
            // The camera is there but off: a ghost to turn it back on.
            let lk = l.lane_camera;
            let gy = lk.y + 2;
            let gh = lk.h - 6;
            p.cv.blend(x0, gy, w, gh, t.border, 90);
            p.tx.fit(
                &mut p.cv,
                x0 + s / 3,
                gy + (gh - small as i32 * 5 / 4) / 2,
                "camera off (double-click)",
                small,
                t.muted,
                w - s / 2,
            );
            p.hits
                .push((Rect::new(x0, lk.y, w, lk.h), Hit::CameraOff(i)));
        }
        let _ = target;
    }
    // Captions, as thin bars along the bottom of the sound lane.
    let cy = la.y + la.h - 6;
    for (a, b, _) in st.project.film_captions() {
        let xa = st.time_x(l, a);
        let xb = st.time_x(l, b);
        if xb < la.x || xa > la.x + la.w {
            continue;
        }
        p.cv.fill(xa, cy, (xb - xa - 1).max(1), 3, t.accent);
    }
    // Drop marker while dragging.
    let drop_x = match &st.drag {
        Some(Drag::Clip { moved: true, .. }) | Some(Drag::Media { active: true, .. })
            if r.has(st.mouse.0, st.mouse.1) && !l.lane_music.has(st.mouse.0, st.mouse.1) =>
        {
            let i = st.drop_index(l, st.mouse.0);
            Some(if i < starts.len() {
                st.time_x(l, starts[i])
            } else {
                st.time_x(l, st.project.duration())
            })
        }
        _ => None,
    };
    if let Some(x) = drop_x {
        p.cv.fill(x - 2, lv.y, 4, la.y + la.h - lv.y, t.warning);
    }
    // Music.
    let lm = l.lane_music;
    for (j, mu) in st.project.music.iter().enumerate() {
        let x0 = st.time_x(l, mu.at);
        let x1 = st.time_x(l, mu.at + mu.length());
        let w = (x1 - x0).max(2);
        let sel = st.music_selected == Some(j);
        let my = lm.y + 2;
        let mh = lm.h - 6;
        p.cv.round(x0, my, w, mh, s / 4, if sel { t.accent } else { t.raised });
        let media = &st.project.media[mu.media];
        if let Some(peaks) = st.cache.peaks.get(&media.path) {
            let start = mu.start;
            draw_wave(
                p,
                st,
                peaks,
                x0,
                x1,
                my,
                mh,
                if sel { t.background } else { t.warning },
                |local| start + local,
                mu.volume,
                l,
            );
        }
        p.tx.fit(
            &mut p.cv,
            x0 + s / 3,
            my + 2,
            &media.name(),
            small,
            t.foreground,
            w - s / 2,
        );
        p.hits
            .push((Rect::new(x0, lm.y, w, lm.h), Hit::Music(j, Edge::Body)));
        let edge = (s / 2).min(w / 4).max(3);
        p.hits
            .push((Rect::new(x0, lm.y, edge, lm.h), Hit::Music(j, Edge::Left)));
        p.hits.push((
            Rect::new(x1 - edge, lm.y, edge, lm.h),
            Hit::Music(j, Edge::Right),
        ));
    }
    // Markers: flags on the ruler and a line down the lanes.
    for (tm, name, ci, k) in st.project.film_markers() {
        let x = st.time_x(l, tm);
        if x < ru.x - s * 4 || x > ru.x + ru.w {
            continue;
        }
        let colour = 0x00ff9f43;
        p.cv.blend(
            x,
            l.lane_title.y,
            1,
            lm.y + lm.h - l.lane_title.y,
            colour,
            120,
        );
        let label = if name.trim().is_empty() {
            "◆".to_string()
        } else {
            format!("◆ {name}")
        };
        let fw = (p.tx.width(&label, small) + s / 2).min(s * 10);
        p.cv.round(x, ru.y + 1, fw, ru.h - 2, s / 4, colour);
        p.tx.fit(
            &mut p.cv,
            x + s / 4,
            ru.y + (ru.h - small as i32 * 5 / 4) / 2,
            &label,
            small,
            t.background,
            fw - s / 3,
        );
        p.hits
            .push((Rect::new(x - 2, ru.y, fw + 2, ru.h), Hit::Marker(ci, k)));
    }
    // Marked part.
    if let Some(a) = st.mark_in {
        let b = st.mark_out.unwrap_or(st.playhead);
        let (a, b) = (a.min(b), a.max(b));
        let xa = st.time_x(l, a);
        let xb = st.time_x(l, b);
        p.cv.blend(xa, ru.y, (xb - xa).max(1), lm.y + lm.h - ru.y, t.accent, 50);
        p.cv.fill(xa, ru.y, 2, lm.y + lm.h - ru.y, t.accent);
        if st.mark_out.is_some() {
            p.cv.fill(xb - 1, ru.y, 2, lm.y + lm.h - ru.y, t.accent);
        }
    }
    // Playhead.
    let px = st.time_x(l, st.playhead);
    p.cv.fill(px - 1, ru.y, 2, lm.y + lm.h - ru.y, t.error);
    p.cv.round(px - s / 2, ru.y, s, s * 2 / 3, s / 4, t.error);
    p.cv.no_clip();
    // Zoom buttons.
    let by = r.y + s / 8;
    let mut bx = s / 4;
    let _ = by;
    for (label, a) in [
        ("−", Action::ZoomOut),
        ("+", Action::ZoomIn),
        ("Fit", Action::ZoomFit),
    ] {
        let bw = button(p, t, bx, r.y + 2, label, a, s * 3 / 4, false);
        bx += bw + 2;
    }
}

/// "100 %" and its decibels: "70 % (−3 dB)".
fn volume_label(v: f64) -> String {
    if v <= 0.001 {
        return "0 % (silent)".into();
    }
    let db = 20.0 * v.log10();
    if db.abs() < 0.05 {
        "100 %".into()
    } else {
        let sign = if db > 0.0 { "+" } else { "\u{2212}" };
        format!("{:.0} % ({sign}{:.1} dB)", v * 100.0, db.abs())
    }
}

/// A clip's title or picture-in-picture on its lane: from `a` to `z` in
/// the film, with handles at both ends.
#[allow(clippy::too_many_arguments)]
fn lane_item(
    p: &mut Painter,
    st: &State,
    l: &Layout,
    t: &Theme,
    s: i32,
    lane: Rect,
    a: f64,
    z: f64,
    label: &str,
    i: usize,
    what: u8,
    on: bool,
    off: bool,
) {
    let x0 = st.time_x(l, a);
    let x1 = st.time_x(l, z);
    if x1 < lane.x || x0 > lane.x + lane.w {
        return;
    }
    let w = (x1 - x0).max(4);
    let y = lane.y + 2;
    let h = lane.h - 6;
    let base = if what == 0 { 0x00b48cf2 } else { 0x005fb3d9 };
    let colour = if off {
        t.border
    } else if on {
        t.accent
    } else {
        base
    };
    p.cv.round(x0, y, w, h, s / 4, colour);
    let small = t.size * 8 / 10;
    let fg = if off { t.muted } else { t.background };
    p.tx.fit(
        &mut p.cv,
        x0 + s / 3,
        y + (h - small as i32 * 5 / 4) / 2,
        label,
        small,
        fg,
        w - s / 2,
    );
    p.hits.push((
        Rect::new(x0, lane.y, w, lane.h),
        Hit::Window(i, what, Edge::Body),
    ));
    let edge = (s / 2).min(w / 4).max(3);
    p.hits.push((
        Rect::new(x0, lane.y, edge, lane.h),
        Hit::Window(i, what, Edge::Left),
    ));
    p.hits.push((
        Rect::new(x1 - edge, lane.y, edge, lane.h),
        Hit::Window(i, what, Edge::Right),
    ));
}

#[allow(clippy::too_many_arguments)]
fn draw_wave(
    p: &mut Painter,
    st: &State,
    peaks: &[u8],
    x0: i32,
    x1: i32,
    y: i32,
    h: i32,
    colour: u32,
    source: impl Fn(f64) -> f64,
    gain: f64,
    l: &Layout,
) {
    let mid = y + h / 2;
    let from = x0.max(l.lane_video.x);
    let to = x1.min(l.lane_video.x + l.lane_video.w);
    let per = PEAKS_PER_SECOND as f64;
    for x in from..to {
        let a = source(f64::from(x - x0) / st.zoom);
        let b = source(f64::from(x + 1 - x0) / st.zoom);
        let ia = (a * per) as usize;
        let ib = ((b * per) as usize).max(ia + 1);
        let peak = peaks
            .get(ia.min(peaks.len())..ib.min(peaks.len()))
            .and_then(|s| s.iter().max().copied())
            .unwrap_or(0);
        let amp = ((f64::from(peak) / 255.0) * gain.min(2.0) * f64::from(h / 2 - 1)) as i32;
        if amp > 0 {
            p.cv.fill(x, mid - amp, 1, amp * 2, colour);
        }
    }
}

fn dialog(p: &mut Painter, l: &Layout, t: &Theme, s: i32, w: i32, h: i32, title: &str) -> Rect {
    let full = Rect::new(0, l.top.h, l.top.w, l.timeline.y + l.timeline.h - l.top.h);
    p.cv.blend(full.x, full.y, full.w, full.h, 0x000000, 120);
    let w = w.min(full.w - 2 * s);
    let h = h.min(full.h - 2 * s);
    let r = Rect::new((l.top.w - w) / 2, full.y + (full.h - h) / 2, w, h);
    p.cv.round(r.x - 1, r.y - 1, r.w + 2, r.h + 2, s / 2, t.border);
    p.cv.round(r.x, r.y, r.w, r.h, s / 2, t.surface);
    p.tx.draw(
        &mut p.cv,
        r.x + s,
        r.y + s * 2 / 3,
        title,
        t.size * 11 / 10,
        t.foreground,
    );
    // Clicks inside the dialog must not reach what is under it.
    p.hits.push((full, Hit::Block));
    r
}

fn draw_browser(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Browser(b) = &st.overlay else {
        return;
    };
    let r = dialog(p, l, t, s, s * 50, s * 36, b.purpose.title());
    let small = t.size * 85 / 100;
    let mut y = r.y + s * 5 / 2;
    let dir = b.dir.display().to_string();
    p.tx.fit(&mut p.cv, r.x + s, y, &dir, small, t.muted, r.w - 2 * s);
    y += s * 3 / 2;
    // Filter or name field.
    let field = Rect::new(r.x + s, y, r.w - 2 * s, s * 2);
    p.cv.round(field.x, field.y, field.w, field.h, s / 3, t.background);
    let hint = match b.purpose {
        Purpose::SaveAs => "Name",
        p if p.folders_only() => "Type to filter folders",
        _ => "Type to filter",
    };
    let shown = if b.filter.is_empty() {
        hint.to_string()
    } else {
        format!("{}▏", b.filter)
    };
    p.tx.fit(
        &mut p.cv,
        field.x + s / 2,
        field.y + s / 4,
        &shown,
        t.size,
        if b.filter.is_empty() {
            t.muted
        } else {
            t.foreground
        },
        field.w - s,
    );
    y += s * 5 / 2;
    let list_top = y;
    let row_h = s * 9 / 5;
    let bottom = r.y + r.h - s * 4;
    let rows = ((bottom - list_top) / row_h).max(1) as usize;
    let visible = b.visible();
    let first = if b.selected >= rows {
        b.selected + 1 - rows
    } else {
        0
    };
    p.cv.set_clip(r.x, list_top, r.w, bottom - list_top);
    for (k, e) in visible.iter().enumerate().skip(first).take(rows) {
        let ry = list_top + (k - first) as i32 * row_h;
        let row = Rect::new(r.x + s / 2, ry, r.w - s, row_h - 2);
        if k == b.selected {
            p.cv.round(row.x, row.y, row.w, row.h, s / 3, t.selected);
        }
        let marked = b.marked.contains(&e.path);
        let icon = if e.dir {
            "▸"
        } else if marked {
            "✓"
        } else {
            " "
        };
        p.tx.draw(
            &mut p.cv,
            row.x + s / 2,
            ry + s / 5,
            icon,
            t.size,
            if marked { t.success } else { t.accent },
        );
        let name = if e.dir {
            format!("{}/", e.name)
        } else {
            e.name.clone()
        };
        p.tx.fit(
            &mut p.cv,
            row.x + s * 2,
            ry + s / 5,
            &name,
            t.size,
            t.foreground,
            row.w - s * 8,
        );
        if !e.dir {
            let size = human_size(e.size);
            let sw = p.tx.width(&size, small);
            p.tx.draw(
                &mut p.cv,
                row.x + row.w - sw - s / 2,
                ry + s / 4,
                &size,
                small,
                t.muted,
            );
        }
        p.hits.push((row, Hit::Entry(k)));
    }
    if visible.is_empty() {
        p.tx.draw(
            &mut p.cv,
            r.x + s,
            list_top + s / 2,
            "Nothing here.",
            t.size,
            t.muted,
        );
    }
    p.cv.no_clip();
    // Buttons.
    let by = r.y + r.h - s * 5 / 2;
    let mut bx = r.x + s;
    bx += button(p, t, bx, by, "↑ Up", Action::BrowserUp, s, false) + s / 2;
    bx += button(
        p,
        t,
        bx,
        by,
        if b.hidden {
            "Hide hidden"
        } else {
            "Show hidden"
        },
        Action::BrowserHidden,
        s,
        false,
    ) + s / 2;
    let help = match b.purpose {
        Purpose::Import | Purpose::Open => "Space marks several · Enter opens",
        Purpose::SaveAs => "Enter saves",
        _ => "Enter goes in · Ctrl+Enter picks",
    };
    p.tx.draw(&mut p.cv, bx + s / 2, by + s / 4, help, small, t.muted);
    let accept = match b.purpose {
        Purpose::SaveAs => "Save",
        Purpose::Import | Purpose::Music => "Add",
        Purpose::Open => "Open",
        _ => "Use this folder",
    };
    let size = (s as u32 * 9 / 10).max(10);
    let aw = p.tx.width(accept, size) + s;
    let cw = p.tx.width("Cancel", size) + s;
    let ax = r.x + r.w - s - aw;
    button(p, t, ax, by, accept, Action::BrowserAccept, s, true);
    button(
        p,
        t,
        ax - s / 2 - cw,
        by,
        "Cancel",
        Action::BrowserCancel,
        s,
        false,
    );
}

fn draw_export(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Export(f) = &st.overlay else {
        return;
    };
    let r = dialog(p, l, t, s, s * 48, s * 42, "Export");
    let small = t.size * 85 / 100;
    let mut y = r.y + s * 3;
    p.tx.draw(&mut p.cv, r.x + s, y, "FORMAT", small, t.muted);
    y += s * 3 / 2;
    for (i, fmt) in Format::ALL.iter().enumerate() {
        let row = Rect::new(r.x + s / 2, y, r.w - s, s * 9 / 5);
        if i == f.format {
            p.cv.round(row.x, row.y, row.w, row.h - 2, s / 3, t.selected);
        }
        p.tx.draw(
            &mut p.cv,
            row.x + s / 2,
            y + s / 5,
            if i == f.format { "●" } else { "○" },
            t.size,
            t.accent,
        );
        p.tx.draw(
            &mut p.cv,
            row.x + s * 2,
            y + s / 5,
            fmt.label(),
            t.size,
            t.foreground,
        );
        p.hits.push((row, Hit::Format(i)));
        y += s * 9 / 5;
    }
    y += s / 2;
    let format = Format::ALL[f.format];
    if format.has_video() {
        p.tx.draw(&mut p.cv, r.x + s, y, "SIZE", small, t.muted);
        y += s * 3 / 2;
        let mut x = r.x + s;
        for (i, h) in HEIGHTS.iter().enumerate() {
            let (w, hh, _) = export::frame(&st.project, *h, format);
            let label = if *h == 0 {
                format!("Original {w}×{hh}")
            } else {
                format!("{h}p")
            };
            let size = t.size * 9 / 10;
            let bw = p.tx.width(&label, size) + s;
            let rect = Rect::new(x, y, bw, s * 17 / 10);
            p.cv.round(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                s / 3,
                if i == f.height { t.accent } else { t.raised },
            );
            p.tx.draw(
                &mut p.cv,
                x + s / 2,
                y + s / 5,
                &label,
                size,
                if i == f.height {
                    t.background
                } else {
                    t.foreground
                },
            );
            p.hits.push((rect, Hit::Height(i)));
            x += bw + s / 3;
        }
        y += s * 5 / 2;
    }
    let chips =
        |p: &mut Painter, y: i32, labels: &[String], chosen: usize, hit: fn(usize) -> Hit| {
            let mut x = r.x + s;
            let size = t.size * 9 / 10;
            for (i, label) in labels.iter().enumerate() {
                let bw = p.tx.width(label, size) + s;
                let rect = Rect::new(x, y, bw, s * 17 / 10);
                let on = i == chosen;
                p.cv.round(
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    s / 3,
                    if on { t.accent } else { t.raised },
                );
                p.tx.draw(
                    &mut p.cv,
                    x + s / 2,
                    y + s / 5,
                    label,
                    size,
                    if on { t.background } else { t.foreground },
                );
                p.hits.push((rect, hit(i)));
                x += bw + s / 3;
            }
        };
    if format.has_video() && format != Format::Gif {
        p.tx.draw(&mut p.cv, r.x + s, y, "SIZE LIMIT  (L)", small, t.muted);
        y += s * 3 / 2;
        let labels: Vec<String> = export::LIMITS
            .iter()
            .map(|m| export::limit_label(*m))
            .collect();
        chips(p, y, &labels, f.limit, Hit::Limit);
        y += s * 5 / 2;
    }
    if st.project.caption_count() > 0 {
        p.tx.draw(&mut p.cv, r.x + s, y, "CAPTIONS  (U)", small, t.muted);
        y += s * 3 / 2;
        let labels: Vec<String> = export::CAPTIONS.iter().map(|c| c.to_string()).collect();
        chips(p, y, &labels, f.captions, Hit::CaptionMode);
        y += s * 5 / 2;
    }
    // Name and folder.
    let folder = st.settings.export_folder();
    let name = format!("{}.{}", f.name, format.extension());
    p.tx.draw(&mut p.cv, r.x + s, y, "FILE", small, t.muted);
    y += s * 3 / 2;
    let nr = Rect::new(r.x + s, y, r.w - 2 * s - s * 8, s * 17 / 10);
    p.cv.round(nr.x, nr.y, nr.w, nr.h, s / 3, t.background);
    p.tx.fit(
        &mut p.cv,
        nr.x + s / 2,
        nr.y + s / 5,
        &name,
        t.size,
        t.foreground,
        nr.w - s,
    );
    p.hits.push((nr, Hit::Button(Action::ExportName)));
    button(
        p,
        t,
        nr.x + nr.w + s / 2,
        y,
        "Rename F2",
        Action::ExportName,
        s,
        false,
    );
    y += s * 2;
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y + s / 4,
        &format!("in {}", folder.display()),
        small,
        t.muted,
        r.w - s * 10,
    );
    button(
        p,
        t,
        r.x + r.w - s * 7,
        y,
        "Folder…",
        Action::ExportFolder,
        s,
        false,
    );
    y += s * 5 / 2;
    let what = match (st.mark_in, st.mark_out) {
        (Some(a), Some(b)) if (b - a).abs() > 0.1 => {
            format!("Only the marked part: {} long", clock((b - a).abs()))
        }
        _ => format!("The whole film: {}", clock(st.project.duration())),
    };
    let mut notes = Vec::new();
    if st.project.loudness > 0 && format.has_audio() {
        notes.push(format!(
            "sound levelled to {}",
            export::LOUDNESS[st.project.loudness as usize].0
        ));
    }
    if export::LIMITS[f.limit] > 0 && format.has_video() && format != Format::Gif {
        notes.push("two passes to fit the limit".to_string());
    }
    let what = if notes.is_empty() {
        what
    } else {
        format!("{what} · {}", notes.join(", "))
    };
    p.tx.fit(&mut p.cv, r.x + s, y, &what, small, t.muted, r.w - 2 * s);
    let by = r.y + r.h - s * 5 / 2;
    let size = (s as u32 * 9 / 10).max(10);
    let aw = p.tx.width("Export  Enter", size) + s;
    button(
        p,
        t,
        r.x + r.w - s - aw,
        by,
        "Export  Enter",
        Action::ExportStart,
        s,
        true,
    );
    let cw = p.tx.width("Cancel", size) + s;
    button(
        p,
        t,
        r.x + r.w - s - aw - s / 2 - cw,
        by,
        "Cancel",
        Action::CloseOverlay,
        s,
        false,
    );
}

fn draw_record(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Record(f) = &st.overlay else {
        return;
    };
    let rows = f.rows();
    let r = dialog(
        p,
        l,
        t,
        s,
        s * 52,
        s * 6 + rows.len() as i32 * s * 3 + s * 6,
        "Record",
    );
    let list: Vec<(String, Vec<String>, usize)> = rows
        .iter()
        .map(|row| {
            let (opts, chosen) = f.options(*row);
            (record::Form::label(*row).to_string(), opts, chosen)
        })
        .collect();
    let y = choice_rows(p, t, s, r, r.y + s * 3, &list, f.row.min(rows.len() - 1));
    let small = t.size * 85 / 100;
    let wait = record::pre_delay(&f.choices);
    let info = match f.choices.what {
        1 | 2 if wait > 0 => format!(
            "The editor hides and waits {wait} s so you can switch workspace, then you {}.",
            if f.choices.what == 1 {
                "click the window"
            } else {
                "drag the area"
            }
        ),
        1 => "The editor hides, then you click the window.".to_string(),
        2 => "The editor hides, then you drag the area.".to_string(),
        _ => "The editor hides during the countdown and the recording.".to_string(),
    };
    p.tx.fit(&mut p.cv, r.x + s, y, &info, small, t.muted, r.w - 2 * s);
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y + s * 3 / 2,
        "Stop it from the bar or with niruclip --stop: the film lands on the timeline.",
        small,
        t.muted,
        r.w - 2 * s,
    );
    let by = r.y + r.h - s * 5 / 2;
    let label = "● Start recording  Enter";
    let size9 = (s as u32 * 9 / 10).max(10);
    let aw = p.tx.width(label, size9) + s;
    button(
        p,
        t,
        r.x + r.w - s - aw,
        by,
        label,
        Action::RecordStart,
        s,
        true,
    );
    let cw = p.tx.width("Cancel", size9) + s;
    button(
        p,
        t,
        r.x + r.w - s - aw - s / 2 - cw,
        by,
        "Cancel",
        Action::CloseOverlay,
        s,
        false,
    );
}

fn draw_settings(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let r = dialog(p, l, t, s, s * 44, s * 20, "Settings");
    let small = t.size * 85 / 100;
    let mut y = r.y + s * 3;
    for (label, path, action) in [
        (
            "Open and import from",
            st.settings.media_folder(),
            Action::MediaFolder,
        ),
        (
            "Export to",
            st.settings.export_folder(),
            Action::ExportFolderSetting,
        ),
    ] {
        p.tx.draw(&mut p.cv, r.x + s, y, label, small, t.muted);
        y += s * 3 / 2;
        p.tx.fit(
            &mut p.cv,
            r.x + s,
            y + s / 5,
            &path.display().to_string(),
            t.size,
            t.foreground,
            r.w - s * 11,
        );
        button(p, t, r.x + r.w - s * 8, y, "Change…", action, s, false);
        y += s * 3;
    }
    button(
        p,
        t,
        r.x + s,
        y,
        "Reset both to Videos",
        Action::ResetFolders,
        s,
        false,
    );
    y += s * 3;
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y,
        "Stored in ~/.config/niruclip/edit.toml",
        small,
        t.muted,
        r.w - 2 * s,
    );
    let by = r.y + r.h - s * 5 / 2;
    button(
        p,
        t,
        r.x + r.w - s * 6,
        by,
        "Close",
        Action::CloseOverlay,
        s,
        true,
    );
}

/// Rows of choices, as chips: label, options, the chosen one. Returns
/// the y below them.
fn choice_rows(
    p: &mut Painter,
    t: &Theme,
    s: i32,
    r: Rect,
    mut y: i32,
    rows: &[(String, Vec<String>, usize)],
    focus: usize,
) -> i32 {
    let size = t.size * 9 / 10;
    let lx = r.x + s;
    let cx0 = r.x + s * 11;
    let max_x = r.x + r.w - s;
    for (k, (label, opts, chosen)) in rows.iter().enumerate() {
        let focused = k == focus;
        if focused {
            p.cv.round(
                r.x + s / 3,
                y - s / 4,
                r.w - s * 2 / 3,
                s * 2 + s / 4,
                s / 3,
                t.selected,
            );
        }
        p.tx.fit(
            &mut p.cv,
            lx,
            y + s / 5,
            label,
            size,
            if focused { t.accent } else { t.muted },
            cx0 - lx - s / 2,
        );
        let mut x = cx0;
        let mut cut = false;
        for (i, label) in opts.iter().enumerate() {
            let w = p.tx.width(label, size) + s;
            if x + w > max_x && x > cx0 {
                cut = i <= *chosen;
                break;
            }
            let rect = Rect::new(x, y, w, s * 17 / 10);
            let on = i == *chosen;
            p.cv.round(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                s / 3,
                if on { t.accent } else { t.raised },
            );
            p.tx.draw(
                &mut p.cv,
                x + s / 2,
                y + s / 5,
                label,
                size,
                if on { t.background } else { t.foreground },
            );
            p.hits.push((rect, Hit::Choice(k, i)));
            x += w + s / 3;
        }
        if cut {
            // A long list: the chosen one did not fit.
            let label = format!("▸ {}   (← → to choose)", opts[*chosen]);
            p.tx.fit(
                &mut p.cv,
                cx0,
                y + s * 2,
                &label,
                size,
                t.accent,
                max_x - cx0,
            );
            y += s;
        }
        y += s * 3;
    }
    y
}

/// Buttons along the bottom right of a dialog, the first rightmost.
fn dialog_buttons(p: &mut Painter, t: &Theme, s: i32, r: Rect, items: &[(&str, Action, bool)]) {
    let by = r.y + r.h - s * 5 / 2;
    let size = (s as u32 * 9 / 10).max(10);
    let mut x = r.x + r.w - s;
    for (label, action, primary) in items {
        let w = p.tx.width(label, size) + s;
        x -= w;
        button(p, t, x, by, label, *action, s, *primary);
        x -= s / 2;
    }
}

fn draw_film(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Film { row } = &st.overlay else {
        return;
    };
    let rows = st.film_rows();
    let list: Vec<(String, Vec<String>, usize)> = rows
        .iter()
        .map(|r| {
            let (label, opts, chosen) = st.film_options(*r);
            (label.to_string(), opts, chosen)
        })
        .collect();
    let r = dialog(
        p,
        l,
        t,
        s,
        s * 56,
        s * 9 + rows.len() as i32 * s * 3 + s * 5,
        "The whole film",
    );
    let mut y = choice_rows(p, t, s, r, r.y + s * 3, &list, (*row).min(rows.len() - 1));
    let small = t.size * 85 / 100;
    let n = st.project.caption_count();
    let captions = if n == 0 {
        "No captions yet: Captions… makes them from the speech.".to_string()
    } else {
        format!("{n} captions in the film.")
    };
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y,
        &captions,
        small,
        t.muted,
        r.w - 2 * s,
    );
    y += s * 3 / 2;
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y,
        "Loudness and music are heard in the rendered preview (P) and the export.",
        small,
        t.muted,
        r.w - 2 * s,
    );
    dialog_buttons(
        p,
        t,
        s,
        r,
        &[
            ("Close", Action::CloseOverlay, true),
            ("Cut pauses…  Ctrl+K", Action::Pauses, false),
            ("Captions…  Ctrl+L", Action::Captions, false),
        ],
    );
}

fn draw_pauses(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Pauses(f) = &st.overlay else {
        return;
    };
    // The timeline shows through so the pauses can be seen.
    let found = st.found_pauses();
    let r = Rect::new(
        (l.top.w - (s * 54).min(l.top.w - 2 * s)) / 2,
        l.top.h + s,
        (s * 54).min(l.top.w - 2 * s),
        (s * 20).min(l.timeline.y - l.top.h - 2 * s),
    );
    p.cv.blend(0, l.top.h, l.top.w, l.timeline.y - l.top.h, 0x000000, 120);
    p.hits.push((
        Rect::new(0, l.top.h, l.top.w, l.timeline.y + l.timeline.h - l.top.h),
        Hit::Block,
    ));
    p.cv.round(r.x - 1, r.y - 1, r.w + 2, r.h + 2, s / 2, t.border);
    p.cv.round(r.x, r.y, r.w, r.h, s / 2, t.surface);
    p.tx.draw(
        &mut p.cv,
        r.x + s,
        r.y + s * 2 / 3,
        "Cut pauses",
        t.size * 11 / 10,
        t.foreground,
    );
    let list = vec![
        (
            "Shortest pause".to_string(),
            PAUSE_MIN.iter().map(|v| format!("{v} s")).collect(),
            f.choice[0],
        ),
        (
            "Silence below".to_string(),
            PAUSE_LEVEL.iter().map(|(l, _)| l.to_string()).collect(),
            f.choice[1],
        ),
        (
            "Keep around speech".to_string(),
            PAUSE_KEEP.iter().map(|v| format!("{v} s")).collect(),
            f.choice[2],
        ),
    ];
    let y = choice_rows(p, t, s, r, r.y + s * 3, &list, f.row);
    let summary = match &found {
        None => "Listening to the film…".to_string(),
        Some(v) if v.is_empty() => {
            "No pauses that long: try a shorter pause or another level.".into()
        }
        Some(v) => {
            let d = st.project.duration();
            let total: f64 = v.iter().map(|(a, b)| b.min(d) - a).sum();
            format!(
                "{} pauses, {} in all, marked red on the timeline. Enter cuts them.",
                v.len(),
                clock(total)
            )
        }
    };
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        y,
        &summary,
        t.size * 9 / 10,
        t.foreground,
        r.w - 2 * s,
    );
    dialog_buttons(
        p,
        t,
        s,
        r,
        &[
            ("Cut them  Enter", Action::PausesApply, true),
            ("Cancel", Action::CloseOverlay, false),
        ],
    );
    // The pauses on the timeline.
    if let Some(v) = found {
        let lv = l.lane_video;
        let la = l.lane_audio;
        p.cv.set_clip(lv.x, lv.y, lv.w, la.y + la.h - lv.y);
        for (a, b) in v {
            let xa = st.time_x(l, a);
            let xb = st.time_x(l, b.min(st.project.duration()));
            p.cv.blend(xa, lv.y, (xb - xa).max(1), la.y + la.h - lv.y, t.error, 150);
        }
        p.cv.no_clip();
    }
}

fn draw_captions(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Captions(f) = &st.overlay else {
        return;
    };
    let r = dialog(p, l, t, s, s * 60, s * 40, "Captions");
    let small = t.size * 85 / 100;
    let langs = vec![(
        "Language".to_string(),
        analyze::LANGUAGES
            .iter()
            .map(|(n, _)| n.to_string())
            .collect(),
        f.language,
    )];
    let mut y = choice_rows(p, t, s, r, r.y + s * 3, &langs, usize::MAX);
    let list = st.project.film_captions();
    let status = match &st.caption_progress {
        Some((frac, text)) => format!("{text} {:.0} %", frac * 100.0),
        None if list.is_empty() => {
            "Make captions from the speech (G). NIRUDICTATE transcribes on this computer.".into()
        }
        None => format!(
            "{} captions. Enter edits, Delete removes, Space moves the playhead there.",
            list.len()
        ),
    };
    p.tx.fit(&mut p.cv, r.x + s, y, &status, small, t.muted, r.w - 2 * s);
    y += s * 2;
    let row_h = s * 2;
    let bottom = r.y + r.h - s * 3;
    let rows = ((bottom - y) / row_h).max(1) as usize;
    let first = f
        .selected
        .saturating_sub(rows / 2)
        .min(list.len().saturating_sub(rows));
    p.cv.set_clip(r.x, y, r.w, bottom - y);
    for (k, (a, b, text)) in list.iter().enumerate().skip(first).take(rows) {
        let rect = Rect::new(r.x + s / 2, y, r.w - s, row_h - 2);
        if k == f.selected {
            p.cv.round(rect.x, rect.y, rect.w, rect.h, s / 3, t.selected);
        }
        let time = format!("{} – {}", clock(*a), clock(*b));
        p.tx.draw(&mut p.cv, rect.x + s / 2, y + s / 4, &time, small, t.muted);
        p.tx.fit(
            &mut p.cv,
            rect.x + s * 12,
            y + s / 5,
            text,
            t.size * 9 / 10,
            t.foreground,
            rect.w - s * 13,
        );
        p.hits.push((rect, Hit::ListRow(k)));
        y += row_h;
    }
    p.cv.no_clip();
    let make = if list.is_empty() {
        "Make captions  G"
    } else {
        "Make again  G"
    };
    let mut items = vec![
        ("Close", Action::CloseOverlay, false),
        (make, Action::CaptionsMake, list.is_empty()),
    ];
    if !list.is_empty() {
        items.push(("Remove", Action::CaptionDelete, false));
        items.push(("Edit  Enter", Action::CaptionEdit, true));
    }
    dialog_buttons(p, t, s, r, &items);
}

fn draw_fonts(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Fonts(f) = &st.overlay else {
        return;
    };
    let r = dialog(p, l, t, s, s * 46, s * 40, "Font for the title");
    let field = Rect::new(r.x + s, r.y + s * 3, r.w - 2 * s, s * 2);
    p.cv.round(field.x, field.y, field.w, field.h, s / 3, t.background);
    let shown = if f.filter.is_empty() {
        "Type to search…".to_string()
    } else {
        format!("{}▏", f.filter)
    };
    p.tx.fit(
        &mut p.cv,
        field.x + s / 2,
        field.y + s / 4,
        &shown,
        t.size,
        if f.filter.is_empty() {
            t.muted
        } else {
            t.foreground
        },
        field.w - s,
    );
    let visible = f.visible();
    let row_h = s * 5 / 2;
    let mut y = field.y + field.h + s / 2;
    let bottom = r.y + r.h - s * 3;
    let rows = ((bottom - y) / row_h).max(1) as usize;
    let first = f
        .selected
        .saturating_sub(rows / 2)
        .min(visible.len().saturating_sub(rows));
    p.cv.set_clip(r.x, y, r.w, bottom - y);
    for (k, fam) in visible.iter().enumerate().skip(first).take(rows) {
        let rect = Rect::new(r.x + s / 2, y, r.w - s, row_h - 2);
        if k == f.selected {
            p.cv.round(rect.x, rect.y, rect.w, rect.h, s / 3, t.selected);
        }
        match fam {
            None => {
                p.tx.draw(
                    &mut p.cv,
                    rect.x + s / 2,
                    y + s / 3,
                    "Desktop font",
                    t.size,
                    t.foreground,
                );
            }
            Some(fam) => {
                let size = t.size * 11 / 10;
                let own = p
                    .fonts
                    .entry(fam.file.clone())
                    .or_insert_with(|| Text::from_file(&fam.file));
                let drawn = match own {
                    Some(tx) if fam.name.chars().all(|c| tx.has(c)) => {
                        tx.draw(
                            &mut p.cv,
                            rect.x + s / 2,
                            y + s / 4,
                            &fam.name,
                            size,
                            t.foreground,
                        );
                        true
                    }
                    _ => false,
                };
                if !drawn {
                    p.tx.draw(
                        &mut p.cv,
                        rect.x + s / 2,
                        y + s / 3,
                        &fam.name,
                        t.size,
                        t.foreground,
                    );
                }
            }
        }
        p.hits.push((rect, Hit::ListRow(k)));
        y += row_h;
    }
    p.cv.no_clip();
    let _ = st;
    dialog_buttons(
        p,
        t,
        s,
        r,
        &[
            ("Use it  Enter", Action::FontAccept, true),
            ("Cancel", Action::CloseOverlay, false),
        ],
    );
}

fn draw_place(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Place(pl) = &st.overlay else {
        return;
    };
    let r = dialog(
        p,
        l,
        t,
        s,
        s * 74,
        s * 50,
        "Place the title and the picture-in-picture",
    );
    let avail_w = r.w - 2 * s;
    let avail_h = r.h - s * 12;
    let scale = (f64::from(avail_w) / f64::from(pl.iw)).min(f64::from(avail_h) / f64::from(pl.ih));
    let dw = (f64::from(pl.iw) * scale) as i32;
    let dh = (f64::from(pl.ih) * scale) as i32;
    let img = Rect::new(r.x + (r.w - dw) / 2, r.y + s * 3, dw, dh);
    p.cv.image(img.x, img.y, img.w, img.h, &pl.image, pl.iw, pl.ih);
    let boxes = st.place_boxes(pl);
    let to_screen = |(bx, by, bw, bh): (f64, f64, f64, f64)| {
        let kx = f64::from(img.w) / f64::from(pl.fw.max(1));
        let ky = f64::from(img.h) / f64::from(pl.fh.max(1));
        Rect::new(
            img.x + (bx * kx) as i32,
            img.y + (by * ky) as i32,
            (bw * kx).max(4.0) as i32,
            (bh * ky).max(4.0) as i32,
        )
    };
    for (k, b) in boxes.iter().enumerate() {
        let Some(b) = b else { continue };
        let q = to_screen(*b);
        let selected = k as u8 == pl.selected;
        let (c, w) = if selected {
            (t.accent, 2)
        } else {
            (t.muted, 1)
        };
        p.cv.fill(q.x, q.y, q.w, w, c);
        p.cv.fill(q.x, q.y + q.h - w, q.w, w, c);
        p.cv.fill(q.x, q.y, w, q.h, c);
        p.cv.fill(q.x + q.w - w, q.y, w, q.h, c);
        let name = if k == 0 { "Title" } else { "Picture" };
        let tw = p.tx.width(name, t.size * 8 / 10) + s / 2;
        p.cv.fill(q.x, q.y - s, tw, s, c);
        p.tx.draw(
            &mut p.cv,
            q.x + s / 4,
            q.y - s,
            name,
            t.size * 8 / 10,
            t.background,
        );
    }
    p.hits.push((img, Hit::PlaceImage));
    let help = if pl.selected == 0 {
        "Title: drag it, wheel or + − size, arrows nudge (Shift: more). L look, E effect, C colour, B accent, R back to the bottom, Delete removes it. Tab: the picture."
    } else {
        "Picture: drag it, wheel or + − size, arrows nudge. S shape, B border, D shadow, E effect, R as recorded, Delete hides it in this clip. Tab: the title."
    };
    let c = &st.project.clips[pl.clip.min(st.project.clips.len().saturating_sub(1))];
    let state = if pl.selected == 0 {
        format!(
            "{} · {} · {:.1} %",
            export::LOOK_NAMES[c.effects.text_look.min(6) as usize],
            export::TEXT_EFFECTS[c.effects.text_anim.min(4) as usize],
            c.effects.text_size
        )
    } else {
        format!(
            "{} · {} border · shadow {} · {}",
            PIP_SHAPES[c.pip.shape.min(3) as usize],
            PIP_BORDERS[(c.pip.border as usize).min(PIP_BORDERS.len() - 1)].1,
            if c.pip.shadow { "on" } else { "off" },
            PIP_EFFECTS[c.pip.effect.min(2) as usize]
        )
    };
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        img.y + img.h + s / 3,
        &state,
        t.size,
        t.foreground,
        r.w - 2 * s,
    );
    let mut y = img.y + img.h + s * 3 / 2;
    for line in export::wrap(
        help,
        ((r.w - 2 * s) / (t.size as i32 * 42 / 100).max(1)) as usize,
    ) {
        p.tx.draw(&mut p.cv, r.x + s, y, &line, t.size * 85 / 100, t.muted);
        y += t.size as i32;
    }
    dialog_buttons(
        p,
        t,
        s,
        r,
        &[
            ("Done  Enter", Action::CloseOverlay, true),
            ("Reset  R", Action::PlaceReset, false),
        ],
    );
}

fn draw_area(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::Area(a) = &st.overlay else {
        return;
    };
    let title = if a.zoom {
        "Drag the part to zoom into"
    } else {
        "Drag the part to hide"
    };
    let r = dialog(p, l, t, s, s * 70, s * 46, title);
    let avail_w = r.w - 2 * s;
    let avail_h = r.h - s * 9;
    let scale = (f64::from(avail_w) / f64::from(a.iw)).min(f64::from(avail_h) / f64::from(a.ih));
    let dw = (f64::from(a.iw) * scale) as i32;
    let dh = (f64::from(a.ih) * scale) as i32;
    let img = Rect::new(r.x + (r.w - dw) / 2, r.y + s * 3, dw, dh);
    p.cv.image(img.x, img.y, img.w, img.h, &a.image, a.iw, a.ih);
    let [x, y, w, h] = a.rect;
    let rx = img.x + (f64::from(img.w) * x / 100.0) as i32;
    let ry = img.y + (f64::from(img.h) * y / 100.0) as i32;
    let rw = (f64::from(img.w) * w / 100.0) as i32;
    let rh = (f64::from(img.h) * h / 100.0) as i32;
    if rw > 0 && rh > 0 {
        // Dim what is outside the area, frame what is in it.
        p.cv.blend(img.x, img.y, img.w, ry - img.y, 0x000000, 110);
        p.cv.blend(
            img.x,
            ry + rh,
            img.w,
            img.y + img.h - ry - rh,
            0x000000,
            110,
        );
        p.cv.blend(img.x, ry, rx - img.x, rh, 0x000000, 110);
        p.cv.blend(rx + rw, ry, img.x + img.w - rx - rw, rh, 0x000000, 110);
        let c = t.accent;
        p.cv.fill(rx, ry, rw, 2, c);
        p.cv.fill(rx, ry + rh - 2, rw, 2, c);
        p.cv.fill(rx, ry, 2, rh, c);
        p.cv.fill(rx + rw - 2, ry, 2, rh, c);
    }
    p.hits.push((img, Hit::AreaImage));
    let note = if a.zoom {
        "The zoom keeps the film's shape: the area grows to fit. Set when and how long in the panel."
    } else {
        "Choose blur, pixelate or a black box, and when, in the panel."
    };
    p.tx.fit(
        &mut p.cv,
        r.x + s,
        img.y + img.h + s / 2,
        note,
        t.size * 85 / 100,
        t.muted,
        r.w - 2 * s,
    );
    let _ = st;
    dialog_buttons(
        p,
        t,
        s,
        r,
        &[
            ("Use this area  Enter", Action::AreaAccept, true),
            ("Cancel", Action::CloseOverlay, false),
            (
                if a.zoom {
                    "No zoom  Delete"
                } else {
                    "Show all  Delete"
                },
                Action::AreaClear,
                false,
            ),
            ("Whole picture", Action::AreaWhole, false),
        ],
    );
}

const KEYS: &[(&str, &str)] = &[
    ("Space  K", "Play or pause (K stops)"),
    ("J  L", "Back 2 s; play, L again: faster"),
    ("← →", "One frame (Shift: one second)"),
    ("↑ ↓", "Previous or next cut or marker"),
    ("Home  End", "Start or end of the film"),
    ("S", "Split the clip at the playhead"),
    ("I  O", "Mark the start and end of a part"),
    (
        "X  Delete",
        "Cut the marked part, else remove the chosen clips",
    ),
    ("C", "Keep only the marked part"),
    (
        "Ctrl/Shift+click",
        "Choose more clips: panel changes apply to all",
    ),
    (
        "Ctrl+C  X  V  A",
        "Copy, cut, paste at the playhead, choose all",
    ),
    ("Alt+← →", "Move the clip earlier or later"),
    ("M", "Mute or unmute the chosen clips"),
    ("B", "Set or remove a marker (a chapter)"),
    ("T", "Title text over the clip (| for a new line)"),
    ("Ctrl+T", "Title card: text on a colour"),
    ("E", "Place the title and the camera on the picture"),
    ("P", "Preview with titles, effects and transitions"),
    ("R  N", "Loop the film or the marked part; snapping"),
    ("F  F11", "Full screen picture (Esc comes back)"),
    ("Tab", "Timeline, settings panel, media list"),
    ("+  −  Z", "Zoom in, out, whole film"),
    ("Ctrl+Z  Ctrl+Y", "Undo, redo"),
    ("Ctrl+D", "Duplicate the chosen clips"),
    ("Ctrl+O  Ctrl+I  Ctrl+M", "Open, import, add music"),
    ("Ctrl+S  Ctrl+Shift+S", "Save, save as"),
    ("Ctrl+E", "Export (L size limit, U captions)"),
    ("Ctrl+F", "The whole film: loudness, music, logo"),
    ("Ctrl+K  Ctrl+L", "Cut pauses; captions from the speech"),
    (
        "Ctrl+R",
        "Record the screen, a window, an area or the camera",
    ),
    ("Ctrl+Q", "Quit"),
    (
        "Mouse",
        "Drag clips, edges, title and camera bars, the sound cut (J/L)",
    ),
    ("Volume line", "Drag up or down; double-click: 100 %"),
    (
        "Wheel",
        "Scroll the timeline (Ctrl: zoom); over the video: step",
    ),
    ("Shift+drag", "On the ruler: mark a part"),
];

fn draw_help(p: &mut Painter, l: &Layout, t: &Theme, s: i32) {
    // One column when it fits the window, else two side by side.
    let (ww, wh) = (l.timeline.w, l.timeline.y + l.timeline.h);
    let row = s * 13 / 10;
    let n = KEYS.len() as i32;
    let cols = if s * 6 + n * row <= wh - s * 2 { 1 } else { 2 };
    let per = (n + cols - 1) / cols;
    let col_w = (s * 46).min((ww - s * 2) / cols);
    let r = dialog(p, l, t, s, col_w * cols, s * 6 + per * row, "Keys");
    let key_w = (col_w * 3 / 10).min(s * 13);
    for (k, (key, what)) in KEYS.iter().enumerate() {
        let (c, j) = (k as i32 / per, k as i32 % per);
        let x = r.x + c * col_w;
        let y = r.y + s * 3 + j * row;
        p.tx.fit(
            &mut p.cv,
            x + s,
            y,
            key,
            t.size * 9 / 10,
            t.accent,
            key_w - s / 2,
        );
        p.tx.fit(
            &mut p.cv,
            x + s + key_w,
            y,
            what,
            t.size * 9 / 10,
            t.foreground,
            col_w - key_w - s * 2,
        );
    }
}

fn draw_text_input(p: &mut Painter, st: &State, l: &Layout, t: &Theme, s: i32) {
    let Overlay::TextInput { title, text, .. } = &st.overlay else {
        return;
    };
    let r = dialog(p, l, t, s, s * 40, s * 9, title);
    let field = Rect::new(r.x + s, r.y + s * 3, r.w - 2 * s, s * 2);
    p.cv.round(field.x, field.y, field.w, field.h, s / 3, t.background);
    p.tx.fit(
        &mut p.cv,
        field.x + s / 2,
        field.y + s / 4,
        &format!("{text}▏"),
        t.size,
        t.foreground,
        field.w - s,
    );
    p.tx.draw(
        &mut p.cv,
        r.x + s,
        r.y + s * 6,
        "Enter accepts · Esc cancels",
        t.size * 85 / 100,
        t.muted,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_reads_well() {
        assert_eq!(clock(0.0), "00:00.00");
        assert_eq!(clock(83.456), "01:23.46");
        assert_eq!(clock(3725.0), "1:02:05.00");
    }

    #[test]
    fn every_parameter_round_trips_its_range() {
        let mut c = Clip::new(0, 0.0, 10.0);
        for p in ALL_CLIP_PARAMS {
            let (lo, hi, _) = p.limits(&c);
            if p.opens() || p == Param::Template {
                continue;
            }
            p.set(&mut c, hi);
            assert!((p.get(&c) - hi).abs() < 1e-6 || !p.slider(), "{p:?}");
            p.set(&mut c, lo);
            assert!((p.get(&c) - lo).abs() < 1e-6, "{p:?}");
        }
        Param::Transition.set(&mut c, 3.0);
        assert_eq!(c.transition, Some(Transition::FadeBlack));
        Param::Transition.set(&mut c, 0.0);
        assert_eq!(c.transition, None);
    }

    #[test]
    fn the_panel_shows_only_what_applies() {
        let media = Media {
            has_video: true,
            has_audio: true,
            duration: 10.0,
            ..Media::default()
        };
        let mut c = Clip::new(0, 0.0, 10.0);
        let rows = clip_params(&c, &media);
        assert!(rows.contains(&Param::Voice) && rows.contains(&Param::Title));
        assert!(!rows.contains(&Param::Font) && !rows.contains(&Param::ZoomStart));
        assert!(!rows.contains(&Param::DesktopVolume) && !rows.contains(&Param::CardColour));
        c.effects.text = "Hi".into();
        c.effects.zoom_area = [10.0, 10.0, 40.0, 40.0];
        let rows = clip_params(&c, &media);
        assert!(rows.contains(&Param::Font) && rows.contains(&Param::ZoomStart));
        let card = Media::card();
        let rows = clip_params(&c, &card);
        assert!(rows.contains(&Param::CardColour));
        assert!(!rows.contains(&Param::Brightness) && !rows.contains(&Param::Voice));
        // A template sets the look in one go and is recognised again.
        Param::Template.set(&mut c, 2.0);
        assert_eq!(template_of(&c.effects), 2);
        assert_eq!(c.effects.text_position, 6);
    }

    #[test]
    fn the_layout_fits_a_small_and_a_wide_screen() {
        for (w, h) in [(1280, 720), (3440, 1440), (1920, 1080)] {
            let l = layout(w, h, 15);
            assert!(l.video.w > 300, "{w}x{h}");
            assert!(l.timeline.y + l.timeline.h == h);
            assert!(l.lane_music.y + l.lane_music.h <= h);
        }
    }
}
