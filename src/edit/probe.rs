//! What a file holds, through ffprobe.
use super::project::Media;
use std::path::Path;
use std::process::Command;

pub const VIDEO_EXT: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "mts",
    "3gp", "ogv", "gif",
];
pub const AUDIO_EXT: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "oga", "opus", "m4a", "aac", "wma", "aiff", "aif",
];
pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"];

pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_media(path: &Path) -> bool {
    let ext = extension(path);
    VIDEO_EXT.contains(&ext.as_str())
        || AUDIO_EXT.contains(&ext.as_str())
        || IMAGE_EXT.contains(&ext.as_str())
}

pub fn is_project(path: &Path) -> bool {
    extension(path) == "niruclip"
}

pub fn probe(path: &Path) -> Result<Media, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe could not start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{} cannot be read: {}",
            path.display(),
            err.lines().last().unwrap_or("unknown format")
        ));
    }
    parse(path, &String::from_utf8_lossy(&out.stdout))
}

fn rate(text: &str) -> f64 {
    match text.split_once('/') {
        Some((a, b)) => {
            let a: f64 = a.parse().unwrap_or(0.0);
            let b: f64 = b.parse().unwrap_or(0.0);
            if b > 0.0 {
                a / b
            } else {
                0.0
            }
        }
        None => text.parse().unwrap_or(0.0),
    }
}

pub fn parse(path: &Path, json: &str) -> Result<Media, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("ffprobe output: {e}"))?;
    let mut media = Media {
        path: path.to_path_buf(),
        ..Media::default()
    };
    let image = IMAGE_EXT.contains(&extension(path).as_str());
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    for s in &streams {
        match s["codec_type"].as_str() {
            Some("video") if !media.has_video => {
                // Cover art in a music file is a picture, not a film.
                let attached = s["disposition"]["attached_pic"].as_i64() == Some(1);
                if attached {
                    continue;
                }
                media.has_video = true;
                media.width = s["width"].as_u64().unwrap_or(0) as u32;
                media.height = s["height"].as_u64().unwrap_or(0) as u32;
                let avg = rate(s["avg_frame_rate"].as_str().unwrap_or("0"));
                let real = rate(s["r_frame_rate"].as_str().unwrap_or("0"));
                media.fps = if avg > 0.0 && avg < 241.0 { avg } else { real };
                if let Some(rotation) = side_rotation(s) {
                    if rotation % 180 != 0 {
                        std::mem::swap(&mut media.width, &mut media.height);
                    }
                }
            }
            Some("audio") => media.has_audio = true,
            _ => {}
        }
    }
    media.duration = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    if image || (media.has_video && media.duration <= 0.0 && !media.has_audio) {
        media.still = true;
        media.duration = 5.0;
    }
    if !media.has_video && !media.has_audio {
        return Err(format!("{} has no picture or sound", path.display()));
    }
    if media.duration <= 0.0 {
        return Err(format!("{} has no length", path.display()));
    }
    Ok(media)
}

fn side_rotation(stream: &serde_json::Value) -> Option<i64> {
    stream["side_data_list"]
        .as_array()?
        .iter()
        .find_map(|d| d["rotation"].as_i64())
        .map(|r| r.rem_euclid(360))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_recording() {
        let json = r#"{"streams":[
            {"codec_type":"video","width":2560,"height":1080,"avg_frame_rate":"30/1","r_frame_rate":"30/1"},
            {"codec_type":"audio"}],
            "format":{"duration":"12.5"}}"#;
        let m = parse(Path::new("/v/a.mkv"), json).unwrap();
        assert!(m.has_video && m.has_audio && !m.still);
        assert_eq!((m.width, m.height), (2560, 1080));
        assert!((m.fps - 30.0).abs() < 1e-9);
        assert!((m.duration - 12.5).abs() < 1e-9);
    }

    #[test]
    fn music_with_cover_art_is_audio_and_a_png_is_a_still() {
        let json = r#"{"streams":[
            {"codec_type":"audio"},
            {"codec_type":"video","width":500,"height":500,"disposition":{"attached_pic":1}}],
            "format":{"duration":"200"}}"#;
        let m = parse(Path::new("/m/song.mp3"), json).unwrap();
        assert!(!m.has_video && m.has_audio);
        let png = r#"{"streams":[{"codec_type":"video","width":800,"height":600}],"format":{}}"#;
        let m = parse(Path::new("/p/a.png"), png).unwrap();
        assert!(m.still && m.has_video);
        assert!((m.duration - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_phone_video_turned_on_its_side_swaps_its_size() {
        let json = r#"{"streams":[{"codec_type":"video","width":1920,"height":1080,
            "avg_frame_rate":"30/1","side_data_list":[{"rotation":-90}]}],
            "format":{"duration":"3"}}"#;
        let m = parse(Path::new("/v/p.mp4"), json).unwrap();
        assert_eq!((m.width, m.height), (1080, 1920));
    }
}
