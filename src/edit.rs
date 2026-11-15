//! Non-destructive edits: quarter turns, free rotation, crop, exposure and gamma.
//!
//! Geometry: the "canvas" is the image after quarter turns (same size, or width
//! and height swapped). Free rotation turns the image inside that canvas about its
//! centre; the crop is a rectangle in canvas coordinates and must stay inside the
//! rotated image so no empty corners are ever printed.
use image::RgbaImage;
use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EditParams {
    /// Clockwise quarter turns, 0..=3.
    pub quarter: u8,
    /// Free rotation in degrees, clockwise positive.
    pub angle: f32,
    /// Crop as [x, y, w, h], normalised 0..1 in canvas coordinates. None = whole canvas.
    pub crop: Option<[f32; 4]>,
    /// Exposure in EV stops.
    pub exposure: f32,
    /// Gamma; >1 brightens midtones, <1 darkens them.
    pub gamma: f32,
}

impl Default for EditParams {
    fn default() -> Self {
        Self {
            quarter: 0,
            angle: 0.0,
            crop: None,
            exposure: 0.0,
            gamma: 1.0,
        }
    }
}

impl EditParams {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
    pub fn tone_is_identity(&self) -> bool {
        self.exposure == 0.0 && self.gamma == 1.0
    }
    /// Total clockwise rotation in radians (quarter turns + free angle).
    pub fn total_rotation(&self) -> f32 {
        (self.quarter as f32 * 90.0 + self.angle).to_radians()
    }
    /// Size of the edited result for a source of `w`×`h` pixels.
    pub fn output_size(&self, w: f32, h: f32) -> (f32, f32) {
        let (cw, ch) = canvas_size(w, h, self.quarter);
        let c = self.crop.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        (c[2] * cw, c[3] * ch)
    }
}

pub fn canvas_size(w: f32, h: f32, quarter: u8) -> (f32, f32) {
    if quarter % 2 == 1 { (h, w) } else { (w, h) }
}

/// Canvas geometry for crop constraints, in canvas pixels.
#[derive(Clone, Copy)]
pub struct Geo {
    pub cw: f32,
    pub ch: f32,
    pub angle: f32, // radians
}

impl Geo {
    /// Is canvas point (x, y) on the rotated image?
    pub fn inside(&self, x: f32, y: f32) -> bool {
        let (cx, cy) = (self.cw / 2.0, self.ch / 2.0);
        let (dx, dy) = (x - cx, y - cy);
        let (s, c) = (-self.angle).sin_cos();
        let ux = c * dx - s * dy + cx;
        let uy = s * dx + c * dy + cy;
        let eps = 1e-3 * self.cw.max(self.ch);
        ux >= -eps && uy >= -eps && ux <= self.cw + eps && uy <= self.ch + eps
    }

    pub fn rect_inside(&self, r: [f32; 4]) -> bool {
        let [x, y, w, h] = r;
        w > 0.0
            && h > 0.0
            && self.inside(x, y)
            && self.inside(x + w, y)
            && self.inside(x, y + h)
            && self.inside(x + w, y + h)
    }

    /// The largest rectangle of the given aspect (w/h) centred on `center` that fits.
    pub fn max_rect_at(&self, aspect: f32, center: (f32, f32)) -> [f32; 4] {
        let (bw, bh) = if self.cw / self.ch > aspect {
            (self.ch * aspect, self.ch)
        } else {
            (self.cw, self.cw / aspect)
        };
        let make = |s: f32| {
            [
                center.0 - bw * s / 2.0,
                center.1 - bh * s / 2.0,
                bw * s,
                bh * s,
            ]
        };
        if self.rect_inside(make(1.0)) {
            return make(1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            let mid = (lo + hi) / 2.0;
            if self.rect_inside(make(mid)) {
                lo = mid
            } else {
                hi = mid
            }
        }
        make(lo)
    }

    pub fn max_rect(&self, aspect: f32) -> [f32; 4] {
        self.max_rect_at(aspect, (self.cw / 2.0, self.ch / 2.0))
    }

    /// Shrink `r` about its centre until it fits; recentre if that isn't enough.
    pub fn fit_rect(&self, r: [f32; 4]) -> [f32; 4] {
        if self.rect_inside(r) {
            return r;
        }
        let aspect = r[2] / r[3];
        let c = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let at_center = self.max_rect_at(aspect, c);
        let full = self.max_rect(aspect);
        // Keep the user's framing if it still leaves a reasonably sized crop.
        if at_center[2] >= 0.6 * r[2].min(full[2]) {
            at_center
        } else {
            full
        }
    }
}

/// 256-entry tone curve: exposure applied in linear light, then gamma.
pub fn lut(exposure: f32, gamma: f32) -> [u8; 256] {
    let mul = 2f32.powf(exposure);
    let inv_g = 1.0 / gamma.max(0.05);
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        let s = i as f32 / 255.0;
        let lin = if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        };
        let lin = (lin * mul).clamp(0.0, 1.0);
        let s = if lin <= 0.0031308 {
            lin * 12.92
        } else {
            1.055 * lin.powf(1.0 / 2.4) - 0.055
        };
        *v = (s.powf(inv_g) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    t
}

pub fn apply_lut(img: &mut RgbaImage, t: &[u8; 256]) {
    img.par_chunks_mut(4 * 4096).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<4>().0 {
            px[0] = t[px[0] as usize];
            px[1] = t[px[1] as usize];
            px[2] = t[px[2] as usize];
        }
    });
}

/// Apply all edits to `src` in one resampling pass, at the source's resolution.
pub fn render(src: &RgbaImage, p: &EditParams) -> RgbaImage {
    let (w, h) = (src.width() as f32, src.height() as f32);
    if p.quarter == 0 && p.angle == 0.0 && p.crop.is_none() {
        let mut out = src.clone();
        if !p.tone_is_identity() {
            apply_lut(&mut out, &lut(p.exposure, p.gamma));
        }
        return out;
    }
    let (cw, ch) = canvas_size(w, h, p.quarter);
    let [nx, ny, nw, nh] = p.crop.unwrap_or([0.0, 0.0, 1.0, 1.0]);
    let (x0, y0, rw, rh) = (nx * cw, ny * ch, nw * cw, nh * ch);
    let ow = rw.round().max(1.0) as u32;
    let oh = rh.round().max(1.0) as u32;
    let (sx_scale, sy_scale) = (rw / ow as f32, rh / oh as f32);
    let (s, c) = (-p.angle.to_radians()).sin_cos();
    let t = lut(p.exposure, p.gamma);
    let tone = !p.tone_is_identity();
    let q = p.quarter % 4;
    let mut out = RgbaImage::new(ow, oh);
    out.par_chunks_mut(ow as usize * 4)
        .enumerate()
        .for_each(|(oy, row)| {
            let cy = y0 + (oy as f32 + 0.5) * sy_scale;
            for (ox, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let cx = x0 + (ox as f32 + 0.5) * sx_scale;
                let (dx, dy) = (cx - cw / 2.0, cy - ch / 2.0);
                let ux = c * dx - s * dy + cw / 2.0;
                let uy = s * dx + c * dy + ch / 2.0;
                let (sx, sy) = match q {
                    0 => (ux, uy),
                    1 => (uy, h - ux),
                    2 => (w - ux, h - uy),
                    _ => (w - uy, ux),
                };
                let mut v = bilinear(src, sx - 0.5, sy - 0.5);
                if tone {
                    v[0] = t[v[0] as usize];
                    v[1] = t[v[1] as usize];
                    v[2] = t[v[2] as usize];
                }
                px.copy_from_slice(&v);
            }
        });
    out
}

fn bilinear(img: &RgbaImage, x: f32, y: f32) -> [u8; 4] {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if x < -1.0 || y < -1.0 || x > w as f32 || y > h as f32 {
        return [255, 255, 255, 255];
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let (fx, fy) = (x - x0, y - y0);
    let get = |xi: i64, yi: i64| {
        let p = img.get_pixel(xi.clamp(0, w - 1) as u32, yi.clamp(0, h - 1) as u32);
        p.0
    };
    let (xi, yi) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (
        get(xi, yi),
        get(xi + 1, yi),
        get(xi, yi + 1),
        get(xi + 1, yi + 1),
    );
    let mut o = [0u8; 4];
    for k in 0..4 {
        let top = a[k] as f32 * (1.0 - fx) + b[k] as f32 * fx;
        let bot = c[k] as f32 * (1.0 - fx) + d[k] as f32 * fx;
        o[k] = (top * (1.0 - fy) + bot * fy).round() as u8;
    }
    o
}

/// Undo/redo history of snapshots. `record` is called with the current state
/// whenever nothing is being dragged; a change since the last recorded state
/// becomes one undo step, so a whole slider or crop drag undoes in one go.
pub struct History<T: Copy + PartialEq> {
    past: Vec<T>,
    future: Vec<T>,
    last: T,
}

impl<T: Copy + PartialEq> History<T> {
    const LIMIT: usize = 200;

    pub fn new(initial: T) -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            last: initial,
        }
    }

    /// Note the settled state; returns true if it became a new undo step.
    pub fn record(&mut self, now: T) -> bool {
        if now == self.last {
            return false;
        }
        self.past.push(self.last);
        if self.past.len() > Self::LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
        self.last = now;
        true
    }

    /// The state to go back to, if any. `now` is the current state.
    pub fn undo(&mut self, now: T) -> Option<T> {
        self.record(now);
        let prev = self.past.pop()?;
        self.future.push(self.last);
        self.last = prev;
        Some(prev)
    }

    pub fn redo(&mut self, now: T) -> Option<T> {
        self.record(now);
        let next = self.future.pop()?;
        self.past.push(self.last);
        self.last = next;
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
}

#[cfg(test)]
mod history_tests {
    use super::History;

    #[test]
    fn undo_redo_steps() {
        let mut h = History::new(0);
        assert!(!h.can_undo());
        h.record(0); // unchanged: no step
        assert!(!h.can_undo());
        h.record(5);
        h.record(7);
        assert_eq!(h.undo(7), Some(5));
        assert_eq!(h.undo(5), Some(0));
        assert_eq!(h.undo(0), None);
        assert_eq!(h.redo(0), Some(5));
        assert_eq!(h.redo(5), Some(7));
        assert_eq!(h.redo(7), None);
    }

    #[test]
    fn unrecorded_change_is_undoable_and_new_change_clears_redo() {
        let mut h = History::new(1);
        // A change made since the last record (e.g. a button click) is undone too.
        assert_eq!(h.undo(2), Some(1));
        assert!(h.can_redo());
        h.record(3); // a new edit after undo drops the redo branch
        assert!(!h.can_redo());
        assert_eq!(h.undo(3), Some(1));
    }
}
