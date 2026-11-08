//! Camera discovery, capture-mode selection, picture health and the overlay
//! shape. Everything here is either a direct V4L2 ioctl (no helper process)
//! or a pure function, so the selection and detection rules are unit-tested
//! with synthetic data.

use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// V4L2 ioctls (only the read-only queries NIRUCLIP needs).
// ---------------------------------------------------------------------------

const fn iowr(nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr
}
const fn ior(nr: u32, size: usize) -> u32 {
    (2 << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr
}

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct FmtDesc {
    index: u32,
    kind: u32,
    flags: u32,
    description: [u8; 32],
    pixelformat: u32,
    mbus_code: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct FrmSizeEnum {
    index: u32,
    pixel_format: u32,
    kind: u32,
    data: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
struct FrmIvalEnum {
    index: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    kind: u32,
    data: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
struct Control {
    id: u32,
    value: i32,
}

/// `struct v4l2_format`: a type plus a 200-byte union that is 8-byte aligned
/// on 64-bit targets (it contains pointers), 208 bytes in total there.
#[repr(C)]
struct Format {
    kind: u32,
    data: [u64; 25],
}

const VIDIOC_QUERYCAP: u32 = ior(0, std::mem::size_of::<Capability>());
const VIDIOC_ENUM_FMT: u32 = iowr(2, std::mem::size_of::<FmtDesc>());
const VIDIOC_G_FMT: u32 = iowr(4, std::mem::size_of::<Format>());
const VIDIOC_S_FMT: u32 = iowr(5, std::mem::size_of::<Format>());
const VIDIOC_G_CTRL: u32 = iowr(27, std::mem::size_of::<Control>());
const VIDIOC_ENUM_FRAMESIZES: u32 = iowr(74, std::mem::size_of::<FrmSizeEnum>());
const VIDIOC_ENUM_FRAMEINTERVALS: u32 = iowr(75, std::mem::size_of::<FrmIvalEnum>());

const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const FRMSIZE_TYPE_DISCRETE: u32 = 1;
const FRMIVAL_TYPE_DISCRETE: u32 = 1;
/// `V4L2_CID_PRIVACY`: a privacy shutter or camera key reported by UVC.
const CID_PRIVACY: u32 = 0x009a_0910;

fn xioctl<T>(fd: i32, request: u32, arg: &mut T) -> Result<(), i32> {
    loop {
        // SAFETY: `arg` is a properly sized #[repr(C)] struct for `request`.
        let rc = unsafe { libc::ioctl(fd, request as _, arg as *mut T) };
        if rc == 0 {
            return Ok(());
        }
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
}

fn open_device(device: &str) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(device)
        .ok()
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn fourcc(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|b| *b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Discovery and modes.
// ---------------------------------------------------------------------------

/// One capture mode a camera offers: pixel format, size and best frame rate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

impl Mode {
    /// The `-input_format` name ffmpeg's v4l2 demuxer uses.
    pub fn ffmpeg_format(&self) -> Option<&'static str> {
        match self.format.as_str() {
            "MJPG" | "JPEG" => Some("mjpeg"),
            "YUYV" => Some("yuyv422"),
            "NV12" => Some("nv12"),
            "UYVY" => Some("uyvy422"),
            "GREY" => Some("gray"),
            "H264" => Some("h264"),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{} {}x{}@{}",
            self.format,
            self.width,
            self.height,
            self.fps.round()
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Camera {
    pub device: String,
    pub name: String,
    pub bus: String,
    pub modes: Vec<Mode>,
}

impl Camera {
    /// Infrared (face login) sensors are video-capture nodes too, but they
    /// only deliver greyscale and are never the camera a user means.
    pub fn infrared(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        name.contains(" ir ")
            || name.ends_with(" ir")
            || name.contains("infrared")
            || name.contains("ir camera")
            || (!self.modes.is_empty() && self.modes.iter().all(|mode| mode.format == "GREY"))
    }
}

fn frame_rates(fd: i32, format: u32, width: u32, height: u32) -> f64 {
    let mut best = 0.0f64;
    for index in 0..32 {
        let mut ival = FrmIvalEnum {
            index,
            pixel_format: format,
            width,
            height,
            kind: 0,
            data: [0; 6],
            reserved: [0; 2],
        };
        if xioctl(fd, VIDIOC_ENUM_FRAMEINTERVALS, &mut ival).is_err() {
            break;
        }
        // discrete: numerator/denominator; stepwise: min interval first.
        let (num, den) = (ival.data[0], ival.data[1]);
        if num > 0 {
            best = best.max(den as f64 / num as f64);
        }
        if ival.kind != FRMIVAL_TYPE_DISCRETE {
            break;
        }
    }
    best
}

/// Query one device node. None when it is not a video-capture node (UVC
/// metadata nodes, loopback outputs) or cannot be opened.
pub fn query(device: &str) -> Option<Camera> {
    let file = open_device(device)?;
    let fd = file.as_raw_fd();
    let mut cap = Capability {
        driver: [0; 16],
        card: [0; 32],
        bus_info: [0; 32],
        version: 0,
        capabilities: 0,
        device_caps: 0,
        reserved: [0; 3],
    };
    xioctl(fd, VIDIOC_QUERYCAP, &mut cap).ok()?;
    let caps = if cap.capabilities & CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    };
    if caps & CAP_VIDEO_CAPTURE == 0 {
        return None;
    }
    let mut modes = Vec::new();
    for index in 0..64 {
        let mut desc = FmtDesc {
            index,
            kind: BUF_TYPE_VIDEO_CAPTURE,
            flags: 0,
            description: [0; 32],
            pixelformat: 0,
            mbus_code: 0,
            reserved: [0; 3],
        };
        if xioctl(fd, VIDIOC_ENUM_FMT, &mut desc).is_err() {
            break;
        }
        let name = fourcc(desc.pixelformat);
        for size_index in 0..128 {
            let mut size = FrmSizeEnum {
                index: size_index,
                pixel_format: desc.pixelformat,
                kind: 0,
                data: [0; 6],
                reserved: [0; 2],
            };
            if xioctl(fd, VIDIOC_ENUM_FRAMESIZES, &mut size).is_err() {
                break;
            }
            let sizes: Vec<(u32, u32)> = if size.kind == FRMSIZE_TYPE_DISCRETE {
                vec![(size.data[0], size.data[1])]
            } else {
                // Stepwise/continuous (v4l2loopback, some USB bridges): offer
                // the common sizes that fit.
                let (min_w, max_w, min_h, max_h) =
                    (size.data[0], size.data[1], size.data[3], size.data[4]);
                [(640, 480), (1280, 720), (1920, 1080)]
                    .into_iter()
                    .filter(|(w, h)| *w >= min_w && *w <= max_w && *h >= min_h && *h <= max_h)
                    .collect()
            };
            for (width, height) in sizes {
                let fps = frame_rates(fd, desc.pixelformat, width, height);
                modes.push(Mode {
                    format: name.clone(),
                    width,
                    height,
                    fps,
                });
            }
            if size.kind != FRMSIZE_TYPE_DISCRETE {
                break;
            }
        }
    }
    Some(Camera {
        device: device.to_string(),
        name: c_string(&cap.card),
        bus: c_string(&cap.bus_info),
        modes,
    })
}

fn video_index(path: &Path) -> u32 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("video"))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(u32::MAX)
}

/// Every video-capture node, regular cameras before infrared sensors.
pub fn discover() -> Vec<Camera> {
    let dev = std::env::var("NIRUCLIP_DEV_DIR").unwrap_or_else(|_| "/dev".into());
    let mut nodes: Vec<PathBuf> = fs::read_dir(&dev)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .map(|name| name.as_bytes().starts_with(b"video"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort_by_key(|path| video_index(path));
    let mut cameras: Vec<Camera> = nodes
        .iter()
        .filter_map(|path| query(&path.display().to_string()))
        .collect();
    cameras.sort_by_key(|camera| camera.infrared());
    cameras
}

/// Test hook: `NIRUCLIP_CAMERA_MODES="MJPG 1280x720@30,YUYV 640x480@30"`
/// stands in for the ioctl enumeration of the configured device.
pub fn parse_modes(text: &str) -> Vec<Mode> {
    text.split(',')
        .filter_map(|item| {
            let (format, rest) = item.trim().split_once(' ')?;
            let (size, fps) = rest.split_once('@').unwrap_or((rest, "30"));
            let (width, height) = size.split_once('x')?;
            Some(Mode {
                format: format.to_string(),
                width: width.parse().ok()?,
                height: height.parse().ok()?,
                fps: fps.parse().ok()?,
            })
        })
        .collect()
}

/// The camera to use: an explicit device (option, profile, NIRUCLIP_CAMERA)
/// or the first regular capture camera. Modes are queried for the explicit
/// device too; an empty list means "let ffmpeg decide", the old behaviour.
pub fn resolve(explicit: Option<&str>) -> Result<Camera, String> {
    let test_modes = std::env::var("NIRUCLIP_CAMERA_MODES")
        .ok()
        .map(|text| parse_modes(&text));
    if let Some(device) = explicit {
        use std::os::unix::fs::FileTypeExt;
        let metadata =
            fs::metadata(device).map_err(|_| format!("camera {device} is unavailable"))?;
        if !metadata.file_type().is_char_device() {
            return Err(format!("camera {device} is not a character device"));
        }
        let mut camera = query(device).unwrap_or_else(|| Camera {
            device: device.to_string(),
            ..Camera::default()
        });
        if let Some(modes) = test_modes {
            camera.modes = modes;
        }
        if camera.infrared() && !camera.modes.is_empty() {
            eprintln!("niruclip: {device} looks like an infrared sensor");
        }
        return Ok(camera);
    }
    let mut cameras = discover();
    if let Some(modes) = test_modes {
        if let Some(first) = cameras.first_mut() {
            first.modes = modes;
        }
    }
    cameras
        .into_iter()
        .find(|camera| !camera.infrared())
        .ok_or_else(|| "no camera found (no V4L2 video-capture device)".to_string())
}

/// The capture size NIRUCLIP asks for: about 720p for an overlay (sharp after
/// downscaling, cheap to decode) and up to 1080p for camera-only.
pub fn wanted_size(camera_only: bool, max_width: u32) -> (u32, u32) {
    if camera_only && max_width >= 1920 {
        (1920, 1080)
    } else {
        (1280, 720)
    }
}

fn format_rank(format: &str) -> u32 {
    // Uncompressed first when it can deliver the size at full rate (no
    // MJPEG artifacts or decode cost); MJPEG is what most webcams need for
    // anything above 640x480. GREY is a last resort.
    match format {
        "YUYV" => 0,
        "MJPG" | "JPEG" => 1,
        "NV12" | "UYVY" => 2,
        "H264" => 4,
        "GREY" => 5,
        _ => 9,
    }
}

/// Choose the capture mode: full frame rate first, then the smallest size
/// that covers the wanted size (preferring the wanted aspect), else the
/// largest available, then the cheapest pixel format.
pub fn choose_mode(modes: &[Mode], want: (u32, u32)) -> Option<Mode> {
    let usable: Vec<&Mode> = modes
        .iter()
        .filter(|mode| mode.ffmpeg_format().is_some() && mode.width > 0 && mode.height > 0)
        .collect();
    let best_fps = usable.iter().map(|mode| mode.fps).fold(0.0f64, f64::max);
    // 25 fps is "full rate" for this purpose; a camera whose best is 15 keeps
    // its best rate.
    let floor = best_fps.min(25.0) - 0.5;
    let want_aspect = want.0 as f64 / want.1 as f64;
    usable
        .into_iter()
        .filter(|mode| mode.fps >= floor)
        .min_by_key(|mode| {
            let covers = mode.width >= want.0 && mode.height >= want.1;
            let aspect = mode.width as f64 / mode.height as f64;
            let aspect_off = ((aspect - want_aspect).abs() > 0.02) as u32;
            let area = mode.width as u64 * mode.height as u64;
            (
                !covers,
                aspect_off,
                if covers { area } else { u64::MAX - area },
                format_rank(&mode.format),
            )
        })
        .cloned()
}

// ---------------------------------------------------------------------------
// Privacy control and "camera busy".
// ---------------------------------------------------------------------------

/// The UVC privacy control (a shutter or camera key), when the camera has one.
pub fn privacy(device: &str) -> Option<bool> {
    if let Ok(value) = std::env::var("NIRUCLIP_TEST_PRIVACY") {
        return Some(value == "1");
    }
    let file = open_device(device)?;
    let mut control = Control {
        id: CID_PRIVACY,
        value: 0,
    };
    xioctl(file.as_raw_fd(), VIDIOC_G_CTRL, &mut control).ok()?;
    Some(control.value != 0)
}

/// True when another program is streaming from the camera: setting the
/// current format again is refused with EBUSY while someone else owns the
/// stream. It changes nothing on the device.
pub fn busy(device: &str) -> bool {
    if std::env::var("NIRUCLIP_TEST_CAMERA_BUSY").as_deref() == Ok("1") {
        return true;
    }
    let Some(file) = open_device(device) else {
        return false;
    };
    let fd = file.as_raw_fd();
    let mut format = Format {
        kind: BUF_TYPE_VIDEO_CAPTURE,
        data: [0; 25],
    };
    if xioctl(fd, VIDIOC_G_FMT, &mut format).is_err() {
        return false;
    }
    matches!(xioctl(fd, VIDIOC_S_FMT, &mut format), Err(libc::EBUSY))
}

/// Programs holding the device open, by name (best effort, own processes).
pub fn holders(device: &str) -> Vec<String> {
    let target = fs::canonicalize(device).unwrap_or_else(|_| PathBuf::from(device));
    let own = std::process::id();
    let mut names = Vec::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return names;
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        if fds.flatten().any(|fd| {
            fs::read_link(fd.path())
                .map(|link| link == target)
                .unwrap_or(false)
        }) {
            if let Ok(comm) = fs::read_to_string(entry.path().join("comm")) {
                let comm = comm.trim().to_string();
                if !names.contains(&comm) {
                    names.push(comm);
                }
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Picture health.
// ---------------------------------------------------------------------------

/// Size of the tiny greyscale frames used for health checks: 128x96, sampled
/// with nearest-neighbour scaling so real sensor noise is kept, not averaged.
pub const PROBE_W: usize = 128;
pub const PROBE_H: usize = 96;
pub const PROBE_BYTES: usize = PROBE_W * PROBE_H;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picture {
    /// Not enough frames to judge yet.
    Unknown,
    /// A live picture.
    Live,
    /// A still, near-black frame with a small bright symbol: the firmware
    /// "camera disabled" image some laptops show for a privacy key or shutter.
    Placeholder,
    /// Every frame is bit-identical: a frozen camera, a covered lens in total
    /// darkness, or a privacy image without a symbol.
    Frozen,
}

/// Mean absolute difference between two frames, in grey levels.
fn mad(a: &[u8], b: &[u8]) -> f64 {
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64)
        .sum();
    total as f64 / a.len().max(1) as f64
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStats {
    pub mean: f64,
    pub p90: u8,
    pub max: u8,
    pub bright_share: f64,
}

pub fn frame_stats(frame: &[u8]) -> FrameStats {
    let mut histogram = [0u32; 256];
    for value in frame {
        histogram[*value as usize] += 1;
    }
    let count = frame.len().max(1) as f64;
    let mean = frame.iter().map(|v| *v as f64).sum::<f64>() / count;
    let mut seen = 0u32;
    let mut p90 = 255u8;
    for (value, n) in histogram.iter().enumerate() {
        seen += n;
        if seen as f64 >= count * 0.9 {
            p90 = value as u8;
            break;
        }
    }
    let max = frame.iter().copied().max().unwrap_or(0);
    let bright = histogram[100..].iter().sum::<u32>() as f64 / count;
    FrameStats {
        mean,
        p90,
        max,
        bright_share: bright,
    }
}

/// Judge a run of probe frames (oldest first, about 2-6 per second).
///
/// The rules are deliberately narrow so that a dark room is never reported:
/// a real sensor, however dark, produces noise, so consecutive frames are not
/// pixel-identical and their mean difference stays well above the 0.2 grey
/// levels a synthetic still image gives. A frame must be still *and* look
/// like a symbol on black to be called a privacy image; any content that is
/// bit-identical across every frame is "frozen".
pub fn assess(frames: &[&[u8]]) -> Picture {
    let frames: Vec<&[u8]> = frames
        .iter()
        .copied()
        .filter(|frame| frame.len() == PROBE_BYTES)
        .collect();
    if frames.len() < 4 {
        return Picture::Unknown;
    }
    let diffs: Vec<f64> = frames
        .windows(2)
        .map(|pair| mad(pair[0], pair[1]))
        .collect();
    let still = diffs.iter().all(|d| *d < 0.2);
    let identical = diffs.iter().all(|d| *d == 0.0);
    let last = frame_stats(frames[frames.len() - 1]);
    let symbol_on_black =
        last.p90 <= 24 && last.max >= 140 && last.bright_share > 0.001 && last.bright_share < 0.2;
    if still && symbol_on_black {
        Picture::Placeholder
    } else if identical {
        Picture::Frozen
    } else {
        Picture::Live
    }
}

/// Read a binary PGM (`P5 W H 255`) as written by ffmpeg's pgm encoder.
pub fn read_pgm(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let mut fields = Vec::new();
    let mut pos = 0;
    while fields.len() < 4 && pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..pos]).to_string());
    }
    if fields.len() < 4 || fields[0] != "P5" || fields[3] != "255" {
        return None;
    }
    let width: usize = fields[1].parse().ok()?;
    let height: usize = fields[2].parse().ok()?;
    let data = bytes.get(pos + 1..)?;
    (width == PROBE_W && height == PROBE_H && data.len() == PROBE_BYTES).then(|| data.to_vec())
}

/// The filter chain that turns a camera stream into probe frames. `rate`
/// thins the frames for the separate countdown probe; inside the recording
/// every frame is kept (decimating in a side branch of the recording graph
/// makes ffmpeg report bogus dropped/duplicated frames for the recording).
pub fn probe_chain(rate: Option<u32>) -> String {
    let thin = rate.map(|rate| format!("fps={rate},")).unwrap_or_default();
    format!("{thin}scale={PROBE_W}:{PROBE_H}:flags=neighbor,format=gray")
}

// ---------------------------------------------------------------------------
// Overlay geometry and shape.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,
    Rounded,
    Circle,
}

impl Shape {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "rect" | "square" | "box" => Ok(Self::Rect),
            "rounded" => Ok(Self::Rounded),
            "circle" | "round" => Ok(Self::Circle),
            _ => Err("camera shape must be rect, rounded or circle".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rect => "rect",
            Self::Rounded => "rounded",
            Self::Circle => "circle",
        }
    }
}

fn even(value: f64) -> u32 {
    ((value.max(2.0) / 2.0).round() as u32) * 2
}

/// Where and how large the camera overlay is, in output pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayBox {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    /// Corner radius (circle: half the side).
    pub radius: u32,
    pub border: u32,
}

/// Size the overlay from the output *height* (a share of the picture that
/// looks the same on 16:10, 16:9 and ultrawide screens), keep the camera's
/// aspect (a circle is a centre crop), and keep a margin from the edges.
pub fn overlay_box(
    out: (u32, u32),
    camera: (u32, u32),
    height_share: f64,
    shape: Shape,
    corner: (bool, bool),
    border: bool,
) -> OverlayBox {
    let (out_w, out_h) = out;
    let aspect = if camera.0 > 0 && camera.1 > 0 {
        camera.0 as f64 / camera.1 as f64
    } else {
        16.0 / 9.0
    };
    let mut height = even(out_h as f64 * height_share);
    let mut width = if shape == Shape::Circle {
        height
    } else {
        even(height as f64 * aspect)
    };
    // Never cover more than 45 % of the width.
    let max_width = even(out_w as f64 * 0.45);
    if width > max_width {
        let scale = max_width as f64 / width as f64;
        width = max_width;
        height = even(height as f64 * scale);
        if shape == Shape::Circle {
            height = width;
        }
    }
    let margin = even((out_h as f64 * 0.025).max(8.0));
    let (right, bottom) = corner;
    let x = if right {
        out_w.saturating_sub(width + margin)
    } else {
        margin
    };
    let y = if bottom {
        out_h.saturating_sub(height + margin)
    } else {
        margin
    };
    let radius = match shape {
        Shape::Rect => 0,
        Shape::Rounded => ((height as f64 * 0.12).round() as u32).max(4),
        Shape::Circle => width / 2,
    };
    let border = if border {
        ((height as f64 / 110.0).round() as u32).max(2)
    } else {
        0
    };
    OverlayBox {
        width,
        height,
        x,
        y,
        radius,
        border,
    }
}

/// geq expression for the signed distance to a rounded box (radius `r`):
/// stores it in register 2. Exact for the inset shape too, so the same
/// distance draws the border ring.
fn distance_expr(r: u32) -> String {
    format!(
        "st(0,max(abs(X+0.5-W/2)-(W/2-{r}),0));st(1,max(abs(Y+0.5-H/2)-(H/2-{r}),0));st(2,hypot(ld(0),ld(1)))"
    )
}

/// Filter-graph fragments for the camera overlay. `input` is the scaled,
/// cropped camera ([w]x[h], setsar=1); the result is labelled `output` and
/// carries alpha when a shape or border is used. The mask and ring are
/// generated once (one frame each, repeated by the framesync of alphamerge
/// and overlay), so the per-frame cost is two small alpha blends.
pub fn shape_filters(
    input: &str,
    output: &str,
    overlay: &OverlayBox,
    rgb: (u8, u8, u8),
) -> Vec<String> {
    let (w, h, r, b) = (
        overlay.width,
        overlay.height,
        overlay.radius,
        overlay.border,
    );
    let mut filters = Vec::new();
    let mut current = input.to_string();
    if r > 0 {
        filters.push(format!(
            "color=c=black:s={w}x{h}:r=1:d=1,format=gray,geq=lum='{};255*clip({r}-ld(2)+0.5,0,1)'[nc_mask]",
            distance_expr(r)
        ));
        filters.push(format!(
            "[{current}]format=yuva420p[nc_cam];[nc_cam][nc_mask]alphamerge[nc_shaped]"
        ));
        current = "nc_shaped".into();
    }
    if b > 0 {
        let inner = r.saturating_sub(b);
        // Ring = inside the outer shape minus inside the inset shape. For a
        // rectangle (r = 0) the distance is measured with radius 0, which is
        // the plain inset box.
        let ring = if r > 0 {
            format!(
                "{};255*(clip({r}-ld(2)+0.5,0,1)-clip({inner}-ld(2)+0.5,0,1))",
                distance_expr(r)
            )
        } else {
            format!("255*(1-(gte(X,{b})*lt(X,W-{b})*gte(Y,{b})*lt(Y,H-{b})))")
        };
        let (red, green, blue) = rgb;
        filters.push(format!(
            "color=c=black:s={w}x{h}:r=1:d=1,format=rgba,geq=r={red}:g={green}:b={blue}:a='{ring}'[nc_ring]"
        ));
        filters.push(format!(
            "[{current}][nc_ring]overlay=0:0:format=auto[{output}]"
        ));
    } else if current != input {
        filters.push(format!("[{current}]null[{output}]"));
    } else {
        filters.push(format!("[{input}]null[{output}]"));
    }
    filters
}

// ---------------------------------------------------------------------------
// Theme accent for the border.
// ---------------------------------------------------------------------------

pub fn parse_hex(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x");
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn builtin_accent(theme: &str) -> Option<&'static str> {
    // Same accents as the built-in NIRUX themes (niruwm/nirulauncher).
    Some(match theme {
        "niru-noir" => "5a5a60",
        "niru-nord" => "88c0d0",
        "niru-satie" => "b08d7d",
        "niru-carl-larsson" => "a63d2f",
        "niru-forest-vigil" => "8aa87e",
        "niru-blood-of-urdr" => "c96050",
        "niru-root-and-stone" => "c19768",
        "niru-blue-horizon" => "86aec8",
        "niru-the-norns" => "c9c2b9",
        "niru-winter-nights" => "d8c4aa",
        _ => return None,
    })
}

/// The NIRUX theme accent: a user theme file, the appearance override, then
/// the built-in theme table; None outside NIRUX.
pub fn theme_accent(config_home: &Path) -> Option<(u8, u8, u8)> {
    let config: toml::Value = fs::read_to_string(config_home.join("niruwm/config.toml"))
        .ok()?
        .parse()
        .ok()?;
    let theme = config
        .get("theme")
        .and_then(|value| value.as_str())
        .unwrap_or("niru-noir")
        .to_string();
    let theme_file = config_home.join(format!("niruwm/themes/{theme}.toml"));
    if let Some(value) = fs::read_to_string(theme_file)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
    {
        if let Some(rgb) = ["accent", "focus"]
            .iter()
            .find_map(|key| value.get(*key).and_then(|v| v.as_str()).and_then(parse_hex))
        {
            return Some(rgb);
        }
    }
    if let Some(rgb) = fs::read_to_string(config_home.join("niruwm/appearance.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| {
            value
                .get("colors")?
                .get("focus")?
                .as_str()
                .and_then(parse_hex)
        })
    {
        return Some(rgb);
    }
    builtin_accent(&theme).and_then(parse_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(text: &str) -> Vec<Mode> {
        parse_modes(text)
    }

    #[test]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xc040_5602);
        assert_eq!(VIDIOC_G_CTRL, 0xc008_561b);
        assert_eq!(VIDIOC_ENUM_FRAMESIZES, 0xc02c_564a);
        assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS, 0xc034_564b);
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(VIDIOC_G_FMT, 0xc0d0_5604);
            assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        }
    }

    #[test]
    fn hp_5mp_camera_gets_mjpeg_720p_for_overlay_and_1080p_alone() {
        // The niruxwork camera: YUYV only at 640x480/640x360, MJPEG up to 2880x1800.
        let hp = modes(
            "MJPG 640x480@30,MJPG 640x360@30,MJPG 1280x720@30,MJPG 1920x1080@30,\
             MJPG 2560x1440@30,MJPG 2880x1620@30,MJPG 2880x1800@30,YUYV 640x480@30,YUYV 640x360@30",
        );
        assert_eq!(
            choose_mode(&hp, wanted_size(false, 1920)).unwrap().label(),
            "MJPG 1280x720@30"
        );
        assert_eq!(
            choose_mode(&hp, wanted_size(true, 1920)).unwrap().label(),
            "MJPG 1920x1080@30"
        );
        assert_eq!(
            choose_mode(&hp, wanted_size(true, 1280)).unwrap().label(),
            "MJPG 1280x720@30"
        );
    }

    #[test]
    fn slow_uncompressed_modes_lose_to_full_rate_mjpeg() {
        // Logitech BRIO style: YUYV 720p only at 10 fps.
        let brio = modes("YUYV 640x480@30,YUYV 1280x720@10,MJPG 1280x720@60,MJPG 1920x1080@30");
        assert_eq!(
            choose_mode(&brio, (1280, 720)).unwrap().label(),
            "MJPG 1280x720@60"
        );
        // YUYV wins a tie at the same size and full rate.
        let both = modes("MJPG 1280x720@30,YUYV 1280x720@30");
        assert_eq!(choose_mode(&both, (1280, 720)).unwrap().format, "YUYV");
    }

    #[test]
    fn small_cameras_use_their_largest_full_rate_mode() {
        let vga = modes("YUYV 320x240@30,YUYV 640x480@30,YUYV 1280x960@7");
        assert_eq!(
            choose_mode(&vga, (1280, 720)).unwrap().label(),
            "YUYV 640x480@30"
        );
        // A 15 fps-only camera keeps 15 fps instead of finding nothing.
        let slow = modes("MJPG 1280x720@15");
        assert_eq!(choose_mode(&slow, (1280, 720)).unwrap().fps, 15.0);
        assert!(choose_mode(&[], (1280, 720)).is_none());
        assert!(choose_mode(&modes("ABCD 1280x720@30"), (1280, 720)).is_none());
    }

    #[test]
    fn wanted_aspect_is_preferred_among_covering_modes() {
        let cam = modes("MJPG 1600x1200@30,MJPG 1920x1080@30");
        assert_eq!(
            choose_mode(&cam, (1280, 720)).unwrap().label(),
            "MJPG 1920x1080@30"
        );
    }

    #[test]
    fn infrared_nodes_are_recognised() {
        let ir = Camera {
            name: "HP 5MP Camera: HP IR Camera".into(),
            modes: modes("GREY 400x400@15"),
            ..Camera::default()
        };
        assert!(ir.infrared());
        let rgb = Camera {
            name: "HP 5MP Camera: HP 5MP Camera".into(),
            modes: modes("MJPG 1280x720@30"),
            ..Camera::default()
        };
        assert!(!rgb.infrared());
        let brio_ir = Camera {
            name: "BRIO 4K Stream Edition".into(),
            modes: modes("GREY 640x360@30"),
            ..Camera::default()
        };
        assert!(brio_ir.infrared());
    }

    /// A tiny deterministic noise source for synthetic sensor frames.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f64) / (1u64 << 31) as f64
        }
        /// Roughly normal noise with the given standard deviation.
        fn noise(&mut self, sigma: f64) -> f64 {
            (0..4).map(|_| self.next() - 0.5).sum::<f64>() * sigma * 1.73
        }
    }

    fn placeholder() -> Vec<u8> {
        // Black frame with a white crossed-camera symbol in the middle.
        let mut frame = vec![2u8; PROBE_BYTES];
        for y in 18..30 {
            for x in 24..40 {
                if y == 18 || y == 29 || x == 24 || x == 39 || x - 24 == (y - 18) * 4 / 3 {
                    frame[y * PROBE_W + x] = 250;
                }
            }
        }
        frame
    }

    fn scene(rng: &mut Lcg, base: f64, sigma: f64, lamp: bool) -> Vec<u8> {
        (0..PROBE_BYTES)
            .map(|i| {
                let (x, y) = (i % PROBE_W, i / PROBE_W);
                let mut value = base + (x as f64 * 0.12) + (y as f64 * 0.05);
                if lamp && (28..34).contains(&x) && (6..10).contains(&y) {
                    value = 240.0;
                }
                (value + rng.noise(sigma)).clamp(0.0, 255.0) as u8
            })
            .collect()
    }

    fn judge(frames: &[Vec<u8>]) -> Picture {
        let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        assess(&refs)
    }

    #[test]
    fn the_hp_privacy_image_is_detected() {
        let frames = vec![placeholder(); 8];
        assert_eq!(judge(&frames), Picture::Placeholder);
        // A little encoder flicker on the still image is still a placeholder.
        let mut flicker = frames.clone();
        flicker[3][5] = 3;
        flicker[5][100] = 1;
        assert_eq!(judge(&flicker), Picture::Placeholder);
    }

    #[test]
    fn a_dark_room_is_never_reported() {
        let mut rng = Lcg(7);
        // Very dark (mean ~6) with modest sensor noise.
        let dark: Vec<Vec<u8>> = (0..8).map(|_| scene(&mut rng, 4.0, 2.0, false)).collect();
        assert_eq!(judge(&dark), Picture::Live);
        // Dark room with a bright lamp in view: symbol-on-black statistics,
        // but sensor noise makes it live.
        let lamp: Vec<Vec<u8>> = (0..8).map(|_| scene(&mut rng, 5.0, 1.5, true)).collect();
        assert!(frame_stats(&lamp[0]).p90 <= 24);
        assert_eq!(judge(&lamp), Picture::Live);
        // Even barely-there noise (MJPEG-smoothed) is enough.
        let smooth: Vec<Vec<u8>> = (0..8).map(|_| scene(&mut rng, 6.0, 0.7, true)).collect();
        assert_eq!(judge(&smooth), Picture::Live);
    }

    #[test]
    fn a_lit_static_scene_is_live() {
        let mut rng = Lcg(42);
        let lit: Vec<Vec<u8>> = (0..8).map(|_| scene(&mut rng, 90.0, 2.5, false)).collect();
        assert_eq!(judge(&lit), Picture::Live);
    }

    #[test]
    fn identical_frames_are_frozen() {
        let mut rng = Lcg(3);
        let still = scene(&mut rng, 90.0, 2.5, false);
        assert_eq!(judge(&vec![still; 6]), Picture::Frozen);
        assert_eq!(judge(&vec![vec![0u8; PROBE_BYTES]; 6]), Picture::Frozen);
    }

    #[test]
    fn too_few_or_malformed_frames_are_unknown() {
        assert_eq!(judge(&vec![placeholder(); 3]), Picture::Unknown);
        assert_eq!(judge(&vec![vec![0u8; 10]; 8]), Picture::Unknown);
    }

    #[test]
    fn pgm_frames_are_read_only_when_complete() {
        let dir = std::env::temp_dir().join(format!("niruclip-pgm-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.pgm");
        let mut bytes = format!("P5\n{PROBE_W} {PROBE_H}\n255\n").into_bytes();
        bytes.extend(placeholder());
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read_pgm(&path).unwrap(), placeholder());
        fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
        assert!(read_pgm(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_is_sized_from_the_output_height_and_keeps_the_camera_aspect() {
        let medium = overlay_box(
            (1728, 1080),
            (1280, 720),
            0.26,
            Shape::Rounded,
            (true, true),
            true,
        );
        assert_eq!((medium.width, medium.height), (498, 280));
        assert_eq!(medium.x, 1728 - 498 - 28);
        assert_eq!(medium.y, 1080 - 280 - 28);
        assert!(medium.radius > 0 && medium.border >= 2);
        let circle = overlay_box(
            (1920, 1080),
            (1280, 720),
            0.26,
            Shape::Circle,
            (false, false),
            false,
        );
        assert_eq!(
            (circle.width, circle.height, circle.radius),
            (280, 280, 140)
        );
        assert_eq!((circle.x, circle.y, circle.border), (28, 28, 0));
        // A portrait/narrow output never gets a camera wider than 45 %.
        let narrow = overlay_box(
            (800, 1200),
            (1280, 720),
            0.34,
            Shape::Rect,
            (true, false),
            false,
        );
        assert!(narrow.width <= 360);
        assert_eq!(narrow.width % 2, 0);
        assert_eq!(narrow.height % 2, 0);
    }

    #[test]
    fn shape_filters_generate_the_mask_once() {
        let overlay = overlay_box(
            (1920, 1080),
            (1280, 720),
            0.26,
            Shape::Circle,
            (true, true),
            true,
        );
        let graph = shape_filters("wc", "wco", &overlay, (138, 168, 126)).join(";");
        assert!(graph.contains("r=1:d=1"));
        assert!(graph.contains("alphamerge"));
        assert!(graph.contains("geq=r=138:g=168:b=126"));
        assert!(graph.ends_with("[wco]"));
        let plain = overlay_box(
            (1920, 1080),
            (1280, 720),
            0.26,
            Shape::Rect,
            (true, true),
            false,
        );
        assert_eq!(
            shape_filters("wc", "wco", &plain, (0, 0, 0)),
            vec!["[wc]null[wco]"]
        );
    }

    #[test]
    fn theme_accent_follows_the_nirux_theme() {
        let dir = std::env::temp_dir().join(format!("niruclip-theme-{}", std::process::id()));
        fs::create_dir_all(dir.join("niruwm")).unwrap();
        fs::write(
            dir.join("niruwm/config.toml"),
            "theme = \"niru-forest-vigil\"\n",
        )
        .unwrap();
        assert_eq!(theme_accent(&dir), Some((0x8a, 0xa8, 0x7e)));
        fs::write(
            dir.join("niruwm/appearance.toml"),
            "[colors]\nfocus = \"#112233\"\n",
        )
        .unwrap();
        assert_eq!(theme_accent(&dir), Some((0x11, 0x22, 0x33)));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(parse_hex("#ff8000"), Some((255, 128, 0)));
        assert_eq!(parse_hex("nope"), None);
    }
}
