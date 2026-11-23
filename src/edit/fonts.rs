//! The fonts installed on this computer, through fontconfig.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

pub const STYLES: [&str; 4] = ["Regular", "Bold", "Italic", "Bold Italic"];

/// One family and the file of its regular face (to show its name in it).
#[derive(Clone, Debug)]
pub struct Family {
    pub name: String,
    pub file: PathBuf,
}

/// Every font family, sorted by name, without symbol and emoji fonts.
pub fn families() -> Vec<Family> {
    let Ok(out) = Command::new("fc-list")
        .args([":", "family", "style", "file"])
        .output()
    else {
        return Vec::new();
    };
    parse_list(&String::from_utf8_lossy(&out.stdout))
}

fn parse_list(text: &str) -> Vec<Family> {
    let mut map: BTreeMap<String, (String, PathBuf, bool)> = BTreeMap::new();
    for line in text.lines() {
        // /path/file.ttf: Family,Other name:style=Regular,Normal
        let Some((file, rest)) = line.split_once(": ") else {
            continue;
        };
        let (family, style) = match rest.split_once(":style=") {
            Some((f, s)) => (f, s),
            None => (rest.trim_end_matches(':'), ""),
        };
        let name = family.split(',').next().unwrap_or("").trim().to_string();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        let lower = name.to_lowercase();
        if ["emoji", "symbol", "awesome", "icons", "dingbat", "cursor"]
            .iter()
            .any(|w| lower.contains(w))
        {
            continue;
        }
        let regular = style
            .split(',')
            .any(|s| matches!(s.trim(), "Regular" | "Book" | "Normal" | "Roman"));
        let entry = map
            .entry(lower)
            .or_insert_with(|| (name.clone(), PathBuf::from(file), regular));
        if regular && !entry.2 {
            *entry = (name, PathBuf::from(file), true);
        }
    }
    map.into_values()
        .map(|(name, file, _)| Family { name, file })
        .collect()
}

/// The font file for a family and style (0..3), cached. An empty family is
/// the desktop's usual sans font.
pub fn file(family: &str, style: u8) -> Option<PathBuf> {
    type Files = HashMap<(String, u8), Option<PathBuf>>;
    static CACHE: OnceLock<Mutex<Files>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (family.to_string(), style.min(3));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let base = if family.is_empty() {
        "Noto Sans,DejaVu Sans,Liberation Sans"
    } else {
        family
    };
    let pattern = format!("{base}:style={}", STYLES[style.min(3) as usize]);
    let found = Command::new("fc-match")
        .args(["-f", "%{file}", &pattern])
        .output()
        .ok()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_file());
    if let Ok(mut c) = cache.lock() {
        c.insert(key, found.clone());
    }
    found
}

/// The font file has a letter for every character of `text` (a family
/// for another script draws empty boxes).
pub fn covers(file: &std::path::Path, text: &str) -> bool {
    use ab_glyph::Font;
    type Fonts = HashMap<PathBuf, Option<ab_glyph::FontArc>>;
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut fonts) = fonts.lock() else {
        return true;
    };
    let font = fonts
        .entry(file.to_path_buf())
        .or_insert_with(|| {
            std::fs::read(file)
                .ok()
                .and_then(|b| ab_glyph::FontArc::try_from_vec(b).ok())
        })
        .clone();
    match font {
        Some(f) => text
            .chars()
            .all(|c| c.is_whitespace() || f.glyph_id(c).0 != 0),
        // A font ab_glyph cannot read (a collection): trust fontconfig.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_has_one_row_a_family_and_prefers_the_regular_face() {
        let text = "\
/f/DejaVuSans-Bold.ttf: DejaVu Sans:style=Bold
/f/DejaVuSans.ttf: DejaVu Sans:style=Book
/f/Noto.ttf: Noto Sans,Noto Sans Display:style=Regular,Normal
/f/emoji.ttf: Noto Color Emoji:style=Regular
/f/x.pcf: Fixed:
";
        let list = parse_list(text);
        let names: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["DejaVu Sans", "Fixed", "Noto Sans"]);
        assert_eq!(list[0].file, PathBuf::from("/f/DejaVuSans.ttf"));
    }
}
