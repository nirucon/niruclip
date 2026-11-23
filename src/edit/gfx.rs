//! A small software canvas for the editor window.
//!
//! Everything but the video is drawn into one client-side picture and only
//! the changed rectangles are sent to the X server, through MIT-SHM when the
//! server is local (no copy through the socket) and `PutImage` otherwise.
use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use std::collections::HashMap;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Drawable, Gcontext, ImageFormat};
use x11rb::rust_connection::RustConnection;

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    shm: Option<Shm>,
    heap: Vec<u32>,
    /// Clip rectangle for drawing (x0, y0, x1, y1).
    clip: (i32, i32, i32, i32),
}

struct Shm {
    seg: u32,
    id: i32,
    addr: *mut u32,
    len: usize,
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe {
            libc::shmdt(self.addr as *const libc::c_void);
        }
    }
}

impl Canvas {
    pub fn new(c: &RustConnection, w: i32, h: i32) -> Self {
        let mut canvas = Self {
            w,
            h,
            shm: None,
            heap: Vec::new(),
            clip: (0, 0, w, h),
        };
        canvas.alloc(c);
        canvas
    }

    fn alloc(&mut self, c: &RustConnection) {
        if let Some(old) = self.shm.take() {
            let _ = c.shm_detach(old.seg);
        }
        let len = (self.w.max(1) * self.h.max(1)) as usize;
        self.shm = Self::try_shm(c, len);
        self.heap = if self.shm.is_some() {
            Vec::new()
        } else {
            vec![0; len]
        };
        self.clip = (0, 0, self.w, self.h);
    }

    fn try_shm(c: &RustConnection, len: usize) -> Option<Shm> {
        if std::env::var_os("NIRUCLIP_NO_SHM").is_some() {
            return None;
        }
        c.shm_query_version().ok()?.reply().ok()?;
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, len * 4, libc::IPC_CREAT | 0o600);
            if id < 0 {
                return None;
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                return None;
            }
            let seg = c.generate_id().ok()?;
            let ok = c
                .shm_attach(seg, id as u32, false)
                .ok()
                .and_then(|cookie| cookie.check().ok())
                .is_some();
            // Marked for removal now: it goes away with the last detach,
            // also when the editor is killed.
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            if !ok {
                libc::shmdt(addr);
                return None;
            }
            Some(Shm {
                seg,
                id,
                addr: addr as *mut u32,
                len,
            })
        }
    }

    pub fn resize(&mut self, c: &RustConnection, w: i32, h: i32) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        self.alloc(c);
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        match &self.shm {
            Some(s) => unsafe { std::slice::from_raw_parts_mut(s.addr, s.len) },
            None => &mut self.heap,
        }
    }

    pub fn set_clip(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.clip = (x.max(0), y.max(0), (x + w).min(self.w), (y + h).min(self.h));
    }

    pub fn no_clip(&mut self) {
        self.clip = (0, 0, self.w, self.h);
    }

    /// Send a rectangle of the canvas to the window.
    pub fn present(
        &mut self,
        c: &RustConnection,
        win: Drawable,
        gc: Gcontext,
        depth: u8,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) {
        let x0 = x.clamp(0, self.w);
        let y0 = y.clamp(0, self.h);
        let x1 = (x + w).clamp(0, self.w);
        let y1 = (y + h).clamp(0, self.h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if let Some(s) = &self.shm {
            let _ = c.shm_put_image(
                win,
                gc,
                self.w as u16,
                self.h as u16,
                x0 as u16,
                y0 as u16,
                (x1 - x0) as u16,
                (y1 - y0) as u16,
                x0 as i16,
                y0 as i16,
                depth,
                ImageFormat::Z_PIXMAP.into(),
                false,
                s.seg,
                0,
            );
            let _ = s.id;
            return;
        }
        // PutImage in bands that fit in one request.
        let max = (c.maximum_request_bytes().saturating_sub(64) / 4).max(1);
        let rw = (x1 - x0) as usize;
        let rows = (max / rw).max(1);
        let mut row = y0;
        while row < y1 {
            let n = ((y1 - row) as usize).min(rows) as i32;
            let mut data = Vec::with_capacity(rw * n as usize * 4);
            for yy in row..row + n {
                let start = (yy * self.w + x0) as usize;
                for p in &self.heap[start..start + rw] {
                    data.extend_from_slice(&p.to_le_bytes());
                }
            }
            let _ = c.put_image(
                ImageFormat::Z_PIXMAP,
                win,
                gc,
                rw as u16,
                n as u16,
                x0 as i16,
                row as i16,
                0,
                depth,
                &data,
            );
            row += n;
        }
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, colour: u32) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let x0 = x.max(cx0);
        let y0 = y.max(cy0);
        let x1 = (x + w).min(cx1);
        let y1 = (y + h).min(cy1);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let stride = self.w as usize;
        let px = self.pixels();
        for yy in y0..y1 {
            let row = yy as usize * stride;
            px[row + x0 as usize..row + x1 as usize].fill(colour);
        }
    }

    /// Fill with `alpha` (0..255) over what is there.
    pub fn blend(&mut self, x: i32, y: i32, w: i32, h: i32, colour: u32, alpha: u8) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let x0 = x.max(cx0);
        let y0 = y.max(cy0);
        let x1 = (x + w).min(cx1);
        let y1 = (y + h).min(cy1);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let stride = self.w as usize;
        let px = self.pixels();
        for yy in y0..y1 {
            let row = yy as usize * stride;
            for p in &mut px[row + x0 as usize..row + x1 as usize] {
                *p = mix(*p, colour, alpha);
            }
        }
    }

    /// A rectangle with rounded corners, antialiased at the corners.
    pub fn round(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, colour: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        if r == 0 {
            self.fill(x, y, w, h, colour);
            return;
        }
        self.fill(x, y + r, w, h - 2 * r, colour);
        for dy in 0..r {
            // Row distance from the corner circle's centre.
            let cy = r as f32 - dy as f32 - 0.5;
            let span = (r as f32 * r as f32 - cy * cy).max(0.0).sqrt();
            let inset = (r as f32 - span).floor() as i32;
            let frac = 1.0 - ((r as f32 - span) - inset as f32);
            for (yy, _) in [(y + dy, 0), (y + h - 1 - dy, 1)] {
                self.fill(x + inset + 1, yy, w - 2 * inset - 2, 1, colour);
                let a = (frac * 255.0) as u8;
                self.blend(x + inset, yy, 1, 1, colour, a);
                self.blend(x + w - 1 - inset, yy, 1, 1, colour, a);
            }
        }
    }

    /// Copy 0x00RRGGBB pixels, scaled to fit `dw x dh` (nearest).
    pub fn image(&mut self, x: i32, y: i32, dw: i32, dh: i32, src: &[u32], sw: i32, sh: i32) {
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.clip;
        let stride = self.w as usize;
        let px = self.pixels();
        for yy in y.max(cy0)..(y + dh).min(cy1) {
            let sy = ((yy - y) * sh / dh) as usize;
            for xx in x.max(cx0)..(x + dw).min(cx1) {
                let sx = ((xx - x) * sw / dw) as usize;
                px[yy as usize * stride + xx as usize] = src[sy * sw as usize + sx];
            }
        }
    }
}

pub fn mix(under: u32, over: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha);
    let inv = 255 - a;
    let c = |s: u32| {
        let u = (under >> s) & 255;
        let o = (over >> s) & 255;
        ((o * a + u * inv) / 255) << s
    };
    c(16) | c(8) | c(0)
}

/// Text in one proportional font, with a cache of rasterised glyphs.
pub struct Text {
    font: FontArc,
    /// For symbols the main font lacks (▶ ❚ − ✓ …).
    fallback: Option<FontArc>,
    glyphs: HashMap<(char, u32), Glyph>,
}

struct Glyph {
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    advance: f32,
    coverage: Vec<u8>,
}

impl Text {
    pub fn new(path: Option<&str>) -> Result<Self, String> {
        let fallback = ["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols"]
            .iter()
            .find_map(|name| {
                let out = std::process::Command::new("fc-match")
                    .args(["-f", "%{file}", name])
                    .output()
                    .ok()?;
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                FontArc::try_from_vec(std::fs::read(p).ok()?).ok()
            })
            .or_else(|| {
                [
                    "/usr/share/fonts/TTF/DejaVuSans.ttf",
                    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
                ]
                .iter()
                .find_map(|p| FontArc::try_from_vec(std::fs::read(p).ok()?).ok())
            });
        Ok(Self {
            font: load_font(path)?,
            fallback,
            glyphs: HashMap::new(),
        })
    }

    /// One font file alone (to show a family in its own letters).
    pub fn from_file(path: &std::path::Path) -> Option<Self> {
        let font = FontArc::try_from_vec(std::fs::read(path).ok()?).ok()?;
        Some(Self {
            font,
            fallback: None,
            glyphs: HashMap::new(),
        })
    }

    /// The font has a letter for `ch`.
    pub fn has(&self, ch: char) -> bool {
        ch.is_whitespace() || self.font.glyph_id(ch).0 != 0
    }

    fn glyph(&mut self, ch: char, size: u32) -> &Glyph {
        let mut font = self.font.clone();
        if font.glyph_id(ch).0 == 0 && !ch.is_whitespace() {
            if let Some(f) = self.fallback.as_ref().filter(|f| f.glyph_id(ch).0 != 0) {
                font = f.clone();
            }
        }
        self.glyphs.entry((ch, size)).or_insert_with(|| {
            let scale = PxScale::from(size as f32);
            let scaled = font.as_scaled(scale);
            let id = font.glyph_id(ch);
            let advance = scaled.h_advance(id);
            let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
            match font.outline_glyph(glyph) {
                Some(outline) => {
                    let b = outline.px_bounds();
                    let w = b.width().ceil() as i32;
                    let h = b.height().ceil() as i32;
                    let mut coverage = vec![0u8; (w.max(0) * h.max(0)) as usize];
                    outline.draw(|gx, gy, a| {
                        let i = gy as i32 * w + gx as i32;
                        if gx < w as u32 && gy < h as u32 {
                            coverage[i as usize] = (a.clamp(0.0, 1.0) * 255.0) as u8;
                        }
                    });
                    Glyph {
                        w,
                        h,
                        x: b.min.x.floor() as i32,
                        y: b.min.y.floor() as i32,
                        advance,
                        coverage,
                    }
                }
                None => Glyph {
                    w: 0,
                    h: 0,
                    x: 0,
                    y: 0,
                    advance,
                    coverage: Vec::new(),
                },
            }
        })
    }

    pub fn ascent(&self, size: u32) -> i32 {
        self.font
            .as_scaled(PxScale::from(size as f32))
            .ascent()
            .round() as i32
    }

    pub fn width(&mut self, s: &str, size: u32) -> i32 {
        let mut w = 0.0;
        for ch in s.chars() {
            w += self.glyph(ch, size).advance;
        }
        w.round() as i32
    }

    /// Draw `s` with its top at `y`. Returns the width.
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        s: &str,
        size: u32,
        colour: u32,
    ) -> i32 {
        let base = y + self.ascent(size);
        let mut pen = x as f32;
        let (cx0, cy0, cx1, cy1) = cv.clip;
        let stride = cv.w as usize;
        for ch in s.chars() {
            self.glyph(ch, size);
            let g = &self.glyphs[&(ch, size)];
            let gx = pen.round() as i32 + g.x;
            let gy = base + g.y;
            let advance = g.advance;
            let (w, h) = (g.w, g.h);
            let coverage = &g.coverage;
            let px = cv.pixels();
            for yy in 0..h {
                let py = gy + yy;
                if py < cy0 || py >= cy1 {
                    continue;
                }
                for xx in 0..w {
                    let pxx = gx + xx;
                    if pxx < cx0 || pxx >= cx1 {
                        continue;
                    }
                    let a = coverage[(yy * w + xx) as usize];
                    if a > 0 {
                        let i = py as usize * stride + pxx as usize;
                        px[i] = mix(px[i], colour, a);
                    }
                }
            }
            pen += advance;
        }
        (pen - x as f32).round() as i32
    }

    /// Draw `s` cut with an ellipsis to fit `max` pixels.
    pub fn fit(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        s: &str,
        size: u32,
        colour: u32,
        max: i32,
    ) -> i32 {
        if self.width(s, size) <= max {
            return self.draw(cv, x, y, s, size, colour);
        }
        let mut t: String = s.to_string();
        while !t.is_empty() && self.width(&format!("{t}…"), size) > max {
            t.pop();
        }
        self.draw(cv, x, y, &format!("{t}…"), size, colour)
    }
}

pub fn font_path(preferred: Option<&str>) -> Option<String> {
    if let Some(p) = preferred.filter(|p| std::path::Path::new(p).exists()) {
        return Some(p.to_string());
    }
    if let Ok(out) = std::process::Command::new("fc-match")
        .args([
            "-f",
            "%{file}",
            "Noto Sans,DejaVu Sans,Liberation Sans:style=Regular",
        ])
        .output()
    {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    [
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .map(|p| p.to_string())
}

fn load_font(path: Option<&str>) -> Result<FontArc, String> {
    let p = font_path(path).ok_or("no font found: install fonts-dejavu or noto-fonts")?;
    let bytes = std::fs::read(&p).map_err(|e| format!("{p}: {e}"))?;
    FontArc::try_from_vec(bytes).map_err(|_| format!("{p} is not a TrueType font"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_is_exact_at_the_ends() {
        assert_eq!(mix(0x000000, 0xffffff, 255), 0xffffff);
        assert_eq!(mix(0x123456, 0xffffff, 0), 0x123456);
        assert_eq!(mix(0x000000, 0xff0000, 128) >> 16, 128);
    }
}
