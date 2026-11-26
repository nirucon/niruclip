//! Window-content capture (`--window-id`): the picture of one X window,
//! also while it is covered by other windows or placed outside every screen.
//!
//! The window is redirected with the Composite extension (`Automatic`, so it
//! keeps showing on screen as before and no compositor is needed) and its
//! off-screen pixmap is read with MIT-SHM. A capture thread writes the
//! frames as raw `bgr0` video into a FIFO that ffmpeg reads with wall-clock
//! timestamps, so the window shares one clock with the camera (`-ts abs`)
//! and the microphone (pulse). Every ffmpeg segment (pause and resume, a
//! camera reconnect) opens the FIFO again and the thread follows it.
//!
//! - The stream keeps the size the window had at the start. A window that
//!   is resized later is fitted into that size with black bars, so the
//!   recording never changes size halfway.
//! - The X Damage extension tells the thread when the window changed: a
//!   still slide is read once and the same frame is written again, which
//!   keeps the X server idle.
//! - A window that is closed stops the capture; NIRUCLIP then saves what was
//!   recorded. A window that is unmapped keeps its last picture.
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::composite::{self, ConnectionExt as _};
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConnectionExt as _, EventMask, ExposeEvent, ImageFormat, MapState,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

/// Frames per second written to ffmpeg (the recording rate).
pub const FPS: u32 = 30;
/// Packets ffmpeg may queue from the FIFO: a raw 1080p frame is 8 MB, so the
/// queue is kept short (the screen inputs use 512 small packets).
pub const QUEUE: &str = "32";

/// Parse a window id: decimal or `0x` hexadecimal.
pub fn parse_id(value: &str) -> Option<u32> {
    let value = value.trim();
    let id = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        value.parse().ok()?
    };
    (id != 0).then_some(id)
}

/// Round down to an even number of at least 2 (yuv420p needs even sizes).
pub fn even(value: u32) -> u32 {
    value.max(2) & !1
}

/// Where a `src` sized picture goes inside a `dst` sized frame: scaled to
/// fit (keeping its aspect) and centred. Returns (x, y, width, height).
pub fn fit(src: (u32, u32), dst: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (src.0.max(1) as u64, src.1.max(1) as u64);
    let (dw, dh) = (dst.0.max(1) as u64, dst.1.max(1) as u64);
    // Compare sw/sh with dw/dh without floating point.
    let (w, h) = if sw * dh >= sh * dw {
        (dw, (sh * dw + sw / 2) / sw)
    } else {
        ((sw * dh + sh / 2) / sh, dh)
    };
    let (w, h) = (w.clamp(1, dw) as u32, h.clamp(1, dh) as u32);
    ((dst.0 - w) / 2, (dst.1 - h) / 2, w, h)
}

/// Copy a `src` picture (4 bytes per pixel, rows of `src_stride` bytes)
/// into the `dst` frame: cropped when it is the frame size or up to one
/// pixel larger (odd window sizes), otherwise scaled to fit with black bars.
pub fn compose(
    src: &[u8],
    src_size: (u32, u32),
    src_stride: usize,
    dst: &mut [u8],
    size: (u32, u32),
) {
    let (sw, sh) = src_size;
    let (dw, dh) = size;
    let dst_stride = dw as usize * 4;
    if sw >= dw && sh >= dh && sw - dw <= 1 && sh - dh <= 1 {
        for y in 0..dh as usize {
            let from = y * src_stride;
            dst[y * dst_stride..(y + 1) * dst_stride]
                .copy_from_slice(&src[from..from + dst_stride]);
        }
        return;
    }
    dst.fill(0);
    let (ox, oy, w, h) = fit(src_size, size);
    // Nearest neighbour: resizes are rare, and this keeps the per-frame cost
    // to one table lookup per pixel.
    let columns: Vec<usize> = (0..w as u64)
        .map(|x| ((x * sw as u64 + sw as u64 / 2) / w as u64).min(sw as u64 - 1) as usize * 4)
        .collect();
    for y in 0..h as usize {
        let sy = ((y as u64 * sh as u64 + sh as u64 / 2) / h as u64).min(sh as u64 - 1) as usize;
        let row = &src[sy * src_stride..];
        let out = &mut dst[(oy as usize + y) * dst_stride + ox as usize * 4..];
        for (x, from) in columns.iter().enumerate() {
            out[x * 4..x * 4 + 4].copy_from_slice(&row[*from..*from + 4]);
        }
    }
}

/// The ffmpeg input that reads the frames.
pub fn ffmpeg_input(fifo: &Path, size: (u32, u32)) -> Vec<String> {
    vec![
        "-thread_queue_size".into(),
        QUEUE.into(),
        "-f".into(),
        "rawvideo".into(),
        "-pixel_format".into(),
        "bgr0".into(),
        "-video_size".into(),
        format!("{}x{}", size.0, size.1),
        "-framerate".into(),
        FPS.to_string(),
        "-use_wallclock_as_timestamps".into(),
        "1".into(),
        "-i".into(),
        fifo.display().to_string(),
    ]
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Default)]
struct Shared {
    /// The window was destroyed (or the X connection broke).
    gone: AtomicBool,
    stop: AtomicBool,
    /// Unix time of the first frame ffmpeg read in its current connection.
    first_frame: Mutex<Option<f64>>,
    /// Why the capture ended, for the session.
    reason: Mutex<Option<String>>,
}

/// A running window capture. Dropping it stops the thread, gives the window
/// back to normal drawing and removes the FIFO.
pub struct Capture {
    /// The stream size (the window size at the start, even).
    pub size: (u32, u32),
    pub fifo: PathBuf,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ShmSeg {
    seg: u32,
    addr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is owned by one capture thread at a time; it is
// created before the thread starts or inside it and never shared.
unsafe impl Send for ShmSeg {}

impl ShmSeg {
    fn new(conn: &RustConnection, len: usize) -> Option<Self> {
        // SAFETY: plain System V shared memory calls; the segment is marked
        // for removal right after both sides attached it.
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600);
            if id < 0 {
                return None;
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                return None;
            }
            let seg = conn.generate_id().ok()?;
            let ok = conn
                .shm_attach(seg, id as u32, false)
                .ok()
                .and_then(|cookie| cookie.check().ok())
                .is_some();
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            if !ok {
                libc::shmdt(addr);
                return None;
            }
            Some(Self {
                seg,
                addr: addr as *mut u8,
                len,
            })
        }
    }

    fn free(self, conn: &RustConnection) {
        let _ = conn.shm_detach(self.seg);
        let _ = conn.flush();
        // SAFETY: the address came from shmat and is detached once.
        unsafe { libc::shmdt(self.addr as *const libc::c_void) };
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes long and lives until free().
        unsafe { std::slice::from_raw_parts(self.addr, self.len) }
    }
}

struct Source {
    conn: RustConnection,
    window: u32,
    damage: Option<u32>,
    pixmap: Option<u32>,
    /// The current window size.
    size: (u32, u32),
    shm: Option<ShmSeg>,
    shm_ok: bool,
    mapped: bool,
    /// The window changed since the last read.
    dirty: bool,
}

impl Source {
    fn release_pixmap(&mut self) {
        if let Some(pixmap) = self.pixmap.take() {
            let _ = self.conn.free_pixmap(pixmap);
        }
    }

    /// Name the window's current pixmap (again after a map or a resize).
    fn name_pixmap(&mut self) {
        self.release_pixmap();
        if !self.mapped {
            return;
        }
        let Ok(pixmap) = self.conn.generate_id() else {
            return;
        };
        let named = self
            .conn
            .composite_name_window_pixmap(self.window, pixmap)
            .ok()
            .and_then(|cookie| cookie.check().ok())
            .is_some();
        if named {
            self.pixmap = Some(pixmap);
            self.dirty = true;
        }
    }

    /// Read the pixmap and compose it into `frame` (the stream size).
    fn read_into(&mut self, frame: &mut [u8], size: (u32, u32)) -> bool {
        let Some(pixmap) = self.pixmap else {
            return false;
        };
        let (w, h) = self.size;
        let len = w as usize * h as usize * 4;
        let stride = w as usize * 4;
        if self.shm_ok && self.shm.as_ref().map(|s| s.len) != Some(len) {
            if let Some(old) = self.shm.take() {
                old.free(&self.conn);
            }
            self.shm = ShmSeg::new(&self.conn, len);
            if self.shm.is_none() {
                self.shm_ok = false;
            }
        }
        if let Some(seg) = &self.shm {
            let ok = self
                .conn
                .shm_get_image(
                    pixmap,
                    0,
                    0,
                    w as u16,
                    h as u16,
                    !0,
                    ImageFormat::Z_PIXMAP.into(),
                    seg.seg,
                    0,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.size as usize >= len);
            if ok {
                compose(&seg.bytes()[..len], self.size, stride, frame, size);
            }
            return ok;
        }
        let Some(reply) = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, pixmap, 0, 0, w as u16, h as u16, !0)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return false;
        };
        if reply.data.len() < len {
            return false;
        }
        compose(&reply.data, self.size, stride, frame, size);
        true
    }
}

impl Capture {
    /// Check the window and the X extensions, redirect the window and start
    /// the capture thread. The FIFO is created at `fifo`.
    pub fn start(id: u32, fifo: &Path) -> Result<Self, String> {
        let (conn, _) = x11rb::connect(None)
            .map_err(|error| format!("could not open the X display: {error}"))?;
        if conn
            .extension_information(composite::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_none()
        {
            return Err("the X server has no Composite extension; window capture needs it".into());
        }
        conn.composite_query_version(0, 4)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .ok_or("the Composite extension does not answer")?;
        let geometry = conn
            .get_geometry(id)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .ok_or_else(|| format!("window 0x{id:x} does not exist"))?;
        if geometry.root == id {
            return Err("the root window cannot be captured as a window; use --full".into());
        }
        let bpp = conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|format| format.depth == geometry.depth)
            .map(|format| format.bits_per_pixel)
            .unwrap_or(0);
        if bpp != 32 {
            return Err(format!(
                "window 0x{id:x} has depth {} ({bpp} bits per pixel); only 24 and 32 bit windows can be captured",
                geometry.depth
            ));
        }
        let attributes = conn
            .get_window_attributes(id)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .ok_or_else(|| format!("window 0x{id:x} does not exist"))?;
        conn.change_window_attributes(
            id,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(|error| error.to_string())?;
        conn.composite_redirect_window(id, composite::Redirect::AUTOMATIC)
            .ok()
            .and_then(|cookie| cookie.check().ok())
            .ok_or_else(|| format!("window 0x{id:x} could not be redirected"))?;
        let damage = (conn
            .extension_information(damage::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_some()
            && conn
                .damage_query_version(1, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some())
        .then(|| conn.generate_id().ok())
        .flatten()
        .filter(|damage_id| {
            conn.damage_create(*damage_id, id, damage::ReportLevel::NON_EMPTY)
                .ok()
                .and_then(|cookie| cookie.check().ok())
                .is_some()
        });
        let shm_ok = conn
            .extension_information(shm::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_some()
            && conn
                .shm_query_version()
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some();
        let window_size = (geometry.width as u32, geometry.height as u32);
        let size = (even(window_size.0), even(window_size.1));
        // A covered window's pixmap holds whatever was drawn before it was
        // redirected; a synthetic expose asks its owner to paint it again
        // without clearing anything on screen.
        let _ = conn.send_event(
            false,
            id,
            EventMask::EXPOSURE,
            ExposeEvent {
                response_type: x11rb::protocol::xproto::EXPOSE_EVENT,
                sequence: 0,
                window: id,
                x: 0,
                y: 0,
                width: geometry.width,
                height: geometry.height,
                count: 0,
            },
        );
        let _ = conn.flush();
        make_fifo(fifo)?;
        let mut source = Source {
            conn,
            window: id,
            damage,
            pixmap: None,
            size: window_size,
            shm: None,
            shm_ok,
            mapped: attributes.map_state != MapState::UNMAPPED,
            dirty: true,
        };
        source.name_pixmap();
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let thread_fifo = fifo.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("window-capture".into())
            .spawn(move || run(source, size, &thread_fifo, &thread_shared))
            .map_err(|error| format!("could not start the window capture: {error}"))?;
        Ok(Self {
            size,
            fifo: fifo.to_path_buf(),
            shared,
            thread: Some(thread),
        })
    }

    /// The window was closed: the recording should stop and be saved.
    pub fn gone(&self) -> bool {
        self.shared.gone.load(Ordering::SeqCst)
    }

    pub fn reason(&self) -> Option<String> {
        self.shared.reason.lock().ok().and_then(|r| r.clone())
    }

    /// Unix time of the first frame the current ffmpeg read.
    pub fn first_frame(&self) -> Option<f64> {
        self.shared.first_frame.lock().ok().and_then(|t| *t)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.fifo);
    }
}

fn make_fifo(path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: valid C string.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(format!(
            "could not create {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Open the FIFO for writing once ffmpeg has opened it for reading.
fn open_writer(path: &Path) -> Option<i32> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: valid C string; O_NONBLOCK makes open fail (ENXIO) instead of
    // waiting while no reader is there.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        // A larger pipe buffer smooths the reads (best effort).
        libc::fcntl(fd, libc::F_SETPIPE_SZ, 1 << 20);
    }
    Some(fd)
}

/// Write one whole frame; false when the reader went away.
fn write_frame(fd: i32, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        // SAFETY: fd is open and the buffer is valid for its length.
        let n = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        bytes = &bytes[n as usize..];
    }
    true
}

/// The writer: one frame every 1/30 s, the newest picture the grabber has
/// published, whatever the X server is doing. A slow or busy X server (a
/// heavy redraw, CPU contention) only repeats the last picture; it never
/// starves ffmpeg, so the timeline stays steady.
fn write_loop(fifo: &Path, shared: &Shared, latest: &Mutex<Arc<Vec<u8>>>) {
    let mut writer: Option<i32> = None;
    let period = Duration::from_nanos(1_000_000_000 / FPS as u64);
    let mut next = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        if writer.is_none() {
            writer = open_writer(fifo);
            if writer.is_some() {
                if let Ok(mut t) = shared.first_frame.lock() {
                    *t = None;
                }
            }
        }
        if let Some(fd) = writer {
            let frame = latest.lock().map(|f| f.clone()).ok();
            if let Some(frame) = frame {
                if write_frame(fd, &frame) {
                    if let Ok(mut t) = shared.first_frame.lock() {
                        if t.is_none() {
                            *t = Some(now());
                        }
                    }
                } else {
                    // ffmpeg ended this segment; the next one opens the FIFO
                    // again. SAFETY: fd came from open_writer, closed once.
                    unsafe { libc::close(fd) };
                    writer = None;
                }
            }
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > period * 3 {
            // Far behind (a blocked reader): start the cadence again.
            next = now;
        }
    }
    if let Some(fd) = writer {
        // SAFETY: closed once.
        unsafe { libc::close(fd) };
    }
}

/// The grabber: follows the window (events, damage) and publishes a new
/// picture whenever the window changed, at most 30 times a second.
fn run(mut source: Source, size: (u32, u32), fifo: &Path, shared: &Arc<Shared>) {
    let frame_len = size.0 as usize * size.1 as usize * 4;
    // Black until the first picture arrives.
    let latest = Arc::new(Mutex::new(Arc::new(vec![0u8; frame_len])));
    let mut mine = vec![0u8; frame_len];
    let writer = {
        let latest = latest.clone();
        let shared = shared.clone();
        let fifo = fifo.to_path_buf();
        std::thread::Builder::new()
            .name("window-writer".into())
            .spawn(move || write_loop(&fifo, &shared, &latest))
            .ok()
    };
    let period = Duration::from_nanos(1_000_000_000 / FPS as u64);
    let mut next = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) && writer.is_some() {
        // Window events: resizes, maps, the window closing, damage.
        loop {
            match source.conn.poll_for_event() {
                Ok(Some(Event::ConfigureNotify(e))) if e.window == source.window => {
                    let new = (e.width as u32, e.height as u32);
                    if new != source.size {
                        source.size = new;
                        source.name_pixmap();
                    }
                }
                Ok(Some(Event::MapNotify(e))) if e.window == source.window => {
                    source.mapped = true;
                    source.name_pixmap();
                }
                Ok(Some(Event::UnmapNotify(e))) if e.window == source.window => {
                    // The named pixmap keeps the last picture.
                    source.mapped = false;
                }
                Ok(Some(Event::DestroyNotify(e))) if e.window == source.window => {
                    if let Ok(mut r) = shared.reason.lock() {
                        *r = Some("the recorded window was closed".into());
                    }
                    shared.gone.store(true, Ordering::SeqCst);
                    source.damage = None;
                    source.pixmap = None;
                }
                Ok(Some(Event::DamageNotify(_))) => source.dirty = true,
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    if let Ok(mut r) = shared.reason.lock() {
                        *r = Some("the X connection was lost".into());
                    }
                    shared.gone.store(true, Ordering::SeqCst);
                    source.pixmap = None;
                    break;
                }
            }
        }
        let gone = shared.gone.load(Ordering::SeqCst);
        if !gone && (source.dirty || source.damage.is_none()) && source.pixmap.is_some() {
            if let Some(damage) = source.damage {
                let _ = source.conn.damage_subtract(damage, 0u32, 0u32);
            }
            source.dirty = false;
            if source.read_into(&mut mine, size) {
                // Publish without copying; take the old buffer back unless
                // the writer is still sending it.
                let published = Arc::new(std::mem::take(&mut mine));
                let old = latest
                    .lock()
                    .map(|mut slot| std::mem::replace(&mut *slot, published))
                    .ok();
                mine = old
                    .map(|old| Arc::try_unwrap(old).unwrap_or_else(|old| (*old).clone()))
                    .unwrap_or_else(|| vec![0u8; frame_len]);
            }
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > period * 3 {
            next = now;
        }
    }
    shared.stop.store(true, Ordering::SeqCst);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    source.release_pixmap();
    if let Some(damage) = source.damage.take() {
        let _ = source.conn.damage_destroy(damage);
    }
    if !shared.gone.load(Ordering::SeqCst) {
        let _ = source
            .conn
            .composite_unredirect_window(source.window, composite::Redirect::AUTOMATIC);
    }
    if let Some(seg) = source.shm.take() {
        seg.free(&source.conn);
    }
    let _ = source.conn.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_in_hex_and_decimal() {
        assert_eq!(parse_id("0x1a00003"), Some(0x1a00003));
        assert_eq!(parse_id("27262979"), Some(27262979));
        assert_eq!(parse_id("0"), None);
        assert_eq!(parse_id("window"), None);
    }

    #[test]
    fn fit_keeps_the_aspect_and_centres() {
        assert_eq!(fit((1920, 1080), (1920, 1080)), (0, 0, 1920, 1080));
        // A 16:10 window in a 16:9 stream: bars left and right.
        assert_eq!(fit((1920, 1200), (1920, 1080)), (96, 0, 1728, 1080));
        // A smaller window is scaled up to fill the height or width.
        assert_eq!(fit((960, 540), (1920, 1080)), (0, 0, 1920, 1080));
        assert_eq!(fit((1000, 1000), (1920, 1080)), (420, 0, 1080, 1080));
    }

    #[test]
    fn compose_crops_odd_sizes_and_scales_others() {
        // 3x3 source into a 2x2 frame: a crop of the top-left 2x2.
        let src: Vec<u8> = (0..9u8).flat_map(|v| [v, v, v, 0]).collect();
        let mut dst = vec![9u8; 2 * 2 * 4];
        compose(&src, (3, 3), 12, &mut dst, (2, 2));
        assert_eq!(dst[0], 0);
        assert_eq!(dst[4], 1);
        assert_eq!(dst[8], 3);
        assert_eq!(dst[12], 4);
        // 2x1 source into a 4x4 frame: scaled to 4x2, centred, bars black.
        let src = vec![10, 10, 10, 0, 20, 20, 20, 0];
        let mut dst = vec![9u8; 4 * 4 * 4];
        compose(&src, (2, 1), 8, &mut dst, (4, 4));
        assert_eq!(&dst[0..4], &[0, 0, 0, 0]);
        let row1 = 4 * 4;
        assert_eq!(dst[row1], 10);
        assert_eq!(dst[row1 + 3 * 4], 20);
        assert_eq!(&dst[3 * 16..3 * 16 + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn input_reads_raw_frames_with_wall_clock_time() {
        let argv = ffmpeg_input(Path::new("/run/x.fifo"), (1920, 1080));
        let joined = argv.join(" ");
        assert!(joined.contains("-f rawvideo -pixel_format bgr0 -video_size 1920x1080"));
        assert!(joined.contains("-use_wallclock_as_timestamps 1 -i /run/x.fifo"));
    }
}
