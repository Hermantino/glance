//! The glance logo — a lens with a glint on a dark rounded tile — rasterised in code
//! (same design as assets/glance.svg), used for the toolbar and the window icon.

const BG: [u8; 3] = [0x23, 0x2c, 0x38];
const AMBER: [u8; 3] = [0xe6, 0xb4, 0x50];
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// Colour of the logo at point (x, y) in its 64×64 design space, if any.
fn sample(x: f32, y: f32) -> Option<[u8; 3]> {
    // Rounded square: 2..62, corner radius 14.
    let (lo, hi, r) = (2.0, 62.0, 14.0);
    if x < lo || y < lo || x > hi || y > hi {
        return None;
    }
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    if (x - cx).hypot(y - cy) > r {
        return None;
    }
    if (x - 40.5).hypot(y - 23.5) <= 3.2 {
        return Some(WHITE); // glint
    }
    let d = (x - 32.0).hypot(y - 32.0);
    if d <= 7.0 || (d - 17.0).abs() <= 3.5 {
        return Some(AMBER); // pupil and lens ring
    }
    Some(BG)
}

/// RGBA pixels of the logo at `size`×`size`, 4×4 supersampled.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let k = 64.0 / size as f32;
    for py in 0..size {
        for px in 0..size {
            let (mut acc, mut n) = ([0f32; 3], 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) * k;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * k;
                    if let Some(c) = sample(x, y) {
                        for i in 0..3 {
                            acc[i] += c[i] as f32;
                        }
                        n += 1;
                    }
                }
            }
            let o = ((py * size + px) * 4) as usize;
            if n > 0 {
                for i in 0..3 {
                    out[o + i] = (acc[i] / n as f32).round() as u8;
                }
                out[o + 3] = (n as f32 / 16.0 * 255.0).round() as u8;
            }
        }
    }
    out
}
