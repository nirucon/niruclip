//! A file browser inside the editor: open, import, save as, pick a folder.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Open a project or a media file (a media file starts a new project).
    Open,
    /// Add media files to the project.
    Import,
    /// Add sound under the film.
    Music,
    /// Name a project file.
    SaveAs,
    /// Choose the folder media is opened from.
    MediaFolder,
    /// Choose the folder exports go to.
    ExportFolder,
    /// A picture over the whole film.
    Logo,
}

impl Purpose {
    pub fn title(self) -> &'static str {
        match self {
            Purpose::Open => "Open a project or a video",
            Purpose::Import => "Import media",
            Purpose::Music => "Add music or sound",
            Purpose::SaveAs => "Save the project as",
            Purpose::MediaFolder => "Folder to open media from",
            Purpose::ExportFolder => "Folder to export to",
            Purpose::Logo => "A logo or picture over the film",
        }
    }

    pub fn folders_only(self) -> bool {
        matches!(self, Purpose::MediaFolder | Purpose::ExportFolder)
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

pub struct Browser {
    pub purpose: Purpose,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub scroll: usize,
    /// Typed text: a filter, or the file name when saving.
    pub filter: String,
    pub hidden: bool,
    /// Files marked for a multiple import.
    pub marked: Vec<PathBuf>,
}

impl Browser {
    pub fn new(purpose: Purpose, dir: &Path, name: &str) -> Self {
        let dir = if dir.is_dir() {
            dir.to_path_buf()
        } else {
            super::home()
        };
        let mut b = Self {
            purpose,
            dir,
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            filter: name.to_string(),
            hidden: false,
            marked: Vec::new(),
        };
        b.read();
        b
    }

    fn wanted(&self, path: &Path) -> bool {
        match self.purpose {
            Purpose::Open => super::probe::is_media(path) || super::probe::is_project(path),
            Purpose::Import => super::probe::is_media(path),
            Purpose::Music => {
                let ext = super::probe::extension(path);
                super::probe::AUDIO_EXT.contains(&ext.as_str())
                    || super::probe::VIDEO_EXT.contains(&ext.as_str())
            }
            Purpose::SaveAs => super::probe::is_project(path),
            Purpose::Logo => {
                super::probe::IMAGE_EXT.contains(&super::probe::extension(path).as_str())
            }
            Purpose::MediaFolder | Purpose::ExportFolder => false,
        }
    }

    pub fn read(&mut self) {
        let mut entries = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !self.hidden {
                    continue;
                }
                let path = e.path();
                let dir = path.is_dir();
                if !dir && !self.wanted(&path) {
                    continue;
                }
                let size = if dir {
                    0
                } else {
                    e.metadata().map(|m| m.len()).unwrap_or(0)
                };
                entries.push(Entry {
                    path,
                    name,
                    dir,
                    size,
                });
            }
        }
        entries.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.entries = entries;
        self.selected = 0;
        self.scroll = 0;
    }

    /// The entries shown under the typed filter (not when saving).
    pub fn visible(&self) -> Vec<&Entry> {
        if self.purpose == Purpose::SaveAs || self.filter.is_empty() {
            return self.entries.iter().collect();
        }
        let f = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&f))
            .collect()
    }

    pub fn current(&self) -> Option<Entry> {
        self.visible().get(self.selected).map(|e| (*e).clone())
    }

    pub fn enter(&mut self, dir: PathBuf) {
        self.dir = dir;
        if self.purpose != Purpose::SaveAs {
            self.filter.clear();
        }
        self.read();
    }

    pub fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.clone();
            self.enter(parent);
            if let Some(i) = self.visible().iter().position(|e| e.path == from) {
                self.selected = i;
            }
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(e) = self.current() {
            if e.dir {
                return;
            }
            if let Some(i) = self.marked.iter().position(|p| *p == e.path) {
                self.marked.remove(i);
            } else {
                self.marked.push(e.path);
            }
        }
    }
}

pub fn human_size(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1} GB", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.0} MB", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0} kB", n / 1e3)
    } else {
        format!("{n} B")
    }
}
