//! The NIRUX `record-start` and `record-stop` sounds, through the running
//! NIRUNOTIFY.
//!
//! NIRUNOTIFY owns the system sounds and their settings
//! (`~/.config/nirux/sounds.toml`, category `feedback`). Asking it is one line
//! on its control socket: no process is started. Without NIRUNOTIFY, with
//! notifications off (`--no-notify`, `NIRUCLIP_NOTIFY=0`) or with
//! `NIRUCLIP_SOUND=0`, the recording is silent.

use std::{
    io::{Read, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::PathBuf,
    time::Duration,
};

fn socket_path() -> Option<PathBuf> {
    // The owner of /proc/self is this process's user.
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Some(base.join(format!("nirunotify-{uid}.sock")))
}

fn wanted(options: &crate::Options) -> bool {
    crate::notifications_on(options)
        && std::env::var_os("NIRUCLIP_SOUND").as_deref() != Some(std::ffi::OsStr::new("0"))
}

/// Ask NIRUNOTIFY to play EVENT. Bounded to a fraction of a second and
/// silent on any error.
pub fn play(options: &crate::Options, event: &str) {
    if !wanted(options) {
        return;
    }
    let Some(path) = socket_path() else { return };
    let Ok(mut stream) = UnixStream::connect(path) else {
        return;
    };
    let limit = Some(Duration::from_millis(300));
    let _ = stream.set_write_timeout(limit);
    let _ = stream.set_read_timeout(limit);
    if stream
        .write_all(format!("sound {event}\n").as_bytes())
        .is_ok()
    {
        let mut reply = [0u8; 64];
        let _ = stream.read(&mut reply);
    }
}
