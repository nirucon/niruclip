//! The edit decision list: what the user has done, never the pictures.
//!
//! A project is a list of media files and a sequence of clips cut from them,
//! plus an optional music lane. Source files are never written. Every edit
//! is a cheap change to this structure; the undo history keeps whole copies
//! (a project is a few kilobytes).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

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
}

impl Media {
    pub fn name(&self) -> String {
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
    /// 0 top, 1 middle, 2 bottom.
    #[serde(default = "two")]
    pub text_position: u8,
    /// Height of the text as a share of the picture height, in percent.
    #[serde(default = "text_size")]
    pub text_size: f64,
}

fn one() -> f64 {
    1.0
}
fn two() -> u8 {
    2
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
            text_position: 2,
            text_size: 6.0,
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
}

fn transition_length() -> f64 {
    0.8
}

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
        project.schema = SCHEMA;
        // Relative media paths are relative to the project file.
        let base = path.parent().unwrap_or(Path::new("."));
        for media in &mut project.media {
            if media.path.is_relative() {
                media.path = base.join(&media.path);
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
        }
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
}
