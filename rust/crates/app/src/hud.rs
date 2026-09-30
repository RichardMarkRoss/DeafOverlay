//! HUD drawing: dotted sphere, "Saturn" compass ring and arrowheads. A port of v1 `draw_sphere` /
//! `draw_ring` / `draw_hint_box`. Platform-free: it writes premultiplied BGRA (`0xAARRGGBB`, the
//! layout of a 32-bit top-down DIB) so it can be rendered to an image and checked offline.
//! Text is drawn by the window layer (GDI); this module only reports where it goes.

use dsp::Source;
use std::f32::consts::{FRAC_PI_2, PI};

pub const HUD_W: usize = 380;
pub const HUD_AREA_H: usize = 380;

/// Loudness → 0..1 auto-gain (see [`Normalizer`]).
const REF_MIN: f32 = 1.0; // ponytail: calibration knob. Tracker strengths below ~REF_MIN read as faint.
const REF_HALF_LIFE_S: f32 = 3.0;
const MIN_AMP: f32 = 0.02; // v1 dropped peaks below this

pub struct Canvas<'a> {
    pub w: usize,
    pub h: usize,
    pub px: &'a mut [u32],
}

impl Canvas<'_> {
    /// v1 `putpx_PM`: overwrite (no blend) with a premultiplied colour.
    #[inline]
    pub fn put(&mut self, x: i32, y: i32, a: u8, r: u8, g: u8, b: u8) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let pm = |c: u8| (c as u32 * a as u32 + 127) / 255;
        self.px[y as usize * self.w + x as usize] = (a as u32) << 24 | pm(r) << 16 | pm(g) << 8 | pm(b);
    }

    pub fn fill(&mut self, a: u8, r: u8, g: u8, b: u8) {
        for y in 0..self.h as i32 {
            for x in 0..self.w as i32 {
                self.put(x, y, a, r, g, b);
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // mirrors put(): position + ARGB
    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: u8, r: u8, g: u8, b: u8) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, a, r, g, b);
            }
        }
    }

    /// GDI text leaves alpha at 0; v1 `force_alpha_rect` makes the glyph box opaque.
    pub fn force_alpha(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
        let (x1, y1) = ((x1.max(0) as usize).min(self.w), (y1.max(0) as usize).min(self.h));
        for y in y0..y1 {
            for p in &mut self.px[y * self.w + x0..y * self.w + x1.max(x0)] {
                *p |= 0xFF00_0000;
            }
        }
    }
}

/// One direction to show. `ang`: radians, 0 = front, + = right. `amp`: 0..1 loudness.
/// `rear_gain`: accent strength when the sound is behind (360° layouts only).
#[derive(Clone, Copy, Debug)]
pub struct Peak {
    pub ang: f32,
    pub amp: f32,
    pub rear_gain: f32,
}

/// User-adjustable placement, same defaults and meaning as v1's sliders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub sphere_scale: f32,  // 0.6 .. 1.6
    pub ring_offset_y: f32, // 0.40 .. 0.90 of the HUD height
    pub sphere_x: f32,      // 0..1 of the HUD width
    pub sphere_y: f32,      // 0..1 of the HUD height
}

impl Default for Layout {
    fn default() -> Self {
        Self { sphere_scale: 1.0, ring_offset_y: 0.66, sphere_x: 0.5, sphere_y: 0.47 }
    }
}

const BASE_SCALE: f32 = 0.13;

impl Layout {
    /// (cx, cy, radius)
    pub fn sphere(&self) -> (f32, f32, f32) {
        let r = HUD_W.min(HUD_AREA_H) as f32 * BASE_SCALE * self.sphere_scale;
        (self.sphere_x * HUD_W as f32, self.sphere_y * HUD_AREA_H as f32, r)
    }

    /// (cx, cy, semi-axis x, semi-axis y)
    pub fn ring(&self) -> (f32, f32, f32, f32) {
        let a = HUD_W.min(HUD_AREA_H) as f32 * BASE_SCALE * 1.1 * self.sphere_scale;
        (self.sphere_x * HUD_W as f32, self.ring_offset_y * HUD_AREA_H as f32, a, a * 0.45)
    }
}

/// Tracker strengths are level-dependent and unbounded; v1's HUD wants 0..1. A slow auto-gain
/// (peak reference that halves every few seconds, floored so silence stays dim) plays the role of
/// v1's auto gain. sqrt compresses the range so quieter sounds stay visible next to loud ones.
pub struct Normalizer {
    reference: f32,
}

impl Default for Normalizer {
    fn default() -> Self {
        Self { reference: REF_MIN }
    }
}

impl Normalizer {
    pub fn peaks(&mut self, sources: &[Source], full_360: bool, sensitivity: f32, dt: f32) -> Vec<Peak> {
        let loudest = sources.iter().map(|s| s.strength).fold(0.0, f32::max);
        self.reference = (self.reference * 0.5f32.powf(dt / REF_HALF_LIFE_S)).max(loudest).max(REF_MIN);
        sources
            .iter()
            .map(|s| {
                let amp = ((s.strength / self.reference).sqrt() * s.confidence * sensitivity).clamp(0.0, 1.0);
                let ang = s.azimuth.to_radians();
                // Stereo can't place anything behind, so no rear accent (v1 did the same).
                let rear = if full_360 { ((ang.abs() - FRAC_PI_2) / FRAC_PI_2).clamp(0.0, 1.0) * amp } else { 0.0 };
                Peak { ang, amp, rear_gain: rear }
            })
            .filter(|p| p.amp >= MIN_AMP)
            .collect()
    }
}

/// v1 `dir_color`: front is warm (white → amber → red), behind is cool blue; `s` deepens it.
fn dir_color(s: f32, ang: f32) -> (u8, u8, u8) {
    let s = s.clamp(0.0, 1.0);
    let c = |v: f32| v.clamp(0.0, 255.0) as u8;
    if ang.abs() <= FRAC_PI_2 {
        (255, c(255.0 - s * 215.0), c(245.0 - s * 245.0))
    } else {
        (c(150.0 - s * 130.0), c(205.0 - s * 95.0), 255)
    }
}

fn wrap_pi(x: f32) -> f32 {
    x - (x / (2.0 * PI)).round() * 2.0 * PI
}

/// Clear, then sphere and ring, like v1 `render_frame`. `dim` = settings/move mode: a light
/// full-window dim so every pixel is opaque enough to take clicks.
pub fn draw(c: &mut Canvas, lay: &Layout, peaks: &[Peak], dim: bool) {
    if dim {
        c.fill(150, 0, 0, 0);
    } else {
        c.px.fill(0);
    }
    let (cx, cy, r) = lay.sphere();
    draw_sphere(c, cx, cy, r, 22.0, peaks);
    let (rx, ry, a, b) = lay.ring();
    draw_ring(c, rx, ry, a, b, peaks);
}

fn draw_sphere(c: &mut Canvas, cx: f32, cy: f32, radius: f32, tilt_deg: f32, peaks: &[Peak]) {
    let (st, ct) = tilt_deg.to_radians().sin_cos();
    const LAT: usize = 16;
    const LON: usize = 36;
    for i in 1..LAT {
        let phi = i as f32 / LAT as f32 * PI - FRAC_PI_2;
        let (sy, cr) = phi.sin_cos();
        for j in 0..LON {
            let th = j as f32 / LON as f32 * 2.0 * PI;
            let (x, y, z) = (cr * th.cos(), sy, cr * th.sin());
            let y2 = ct * y - st * z;
            let z2 = st * y + ct * z;
            let denom = 1.0 + 0.55 * z2;
            let xp = x / denom * radius + cx;
            let yp = y2 / denom * radius + cy;
            let (vx, vy) = (xp - cx, yp - cy);
            let vlen = vx.hypot(vy).max(1.0);

            // Light the side of the sphere that faces each sound; the loudest facing one wins.
            let (mut dir_int, mut dom_ang) = (0.0f32, 0.0f32);
            for p in peaks {
                let align = (vx * p.ang.sin() - vy * p.ang.cos()) / vlen;
                let contrib = align.max(0.0) * p.amp;
                if contrib > dir_int {
                    dir_int = contrib;
                    dom_ang = p.ang;
                }
            }

            let grow = 1.0 + 0.7 * dir_int;
            let dot_r = ((radius * 0.012 * grow) as i32).max(2);
            let depth = z2 * 0.5 + 0.5;
            let bright = (0.26 + 0.28 * depth + 1.2 * dir_int).min(1.0);
            let (rr, gg, bb) = dir_color(dir_int, dom_ang);
            let a = (20.0 + 215.0 * bright).clamp(0.0, 238.0) as u8;
            let (xi, yi) = (xp.round() as i32, yp.round() as i32);
            for dy in -dot_r..=dot_r {
                for dx in -dot_r..=dot_r {
                    if dx * dx + dy * dy <= dot_r * dot_r {
                        c.put(xi + dx, yi + dy, a, rr, gg, bb);
                    }
                }
            }
        }
    }

    // Rays toward each sound, plus a pale rear accent when it's behind.
    for p in peaks {
        let (rr, gg, bb) = dir_color(p.amp, p.ang);
        let (fx, fy) = (p.ang.sin(), -p.ang.cos());
        let a = (40.0 + 150.0 * p.amp) as u8;
        for i in 0..(radius * 0.9) as i32 {
            c.put((cx + fx * i as f32) as i32, (cy + fy * i as f32) as i32, a, rr, gg, bb);
        }
        if p.rear_gain > 0.0 {
            let (rx, ry) = ((p.ang + PI).sin(), -(p.ang + PI).cos());
            let a = (20.0 + 160.0 * p.rear_gain).clamp(0.0, 255.0) as u8;
            for i in 0..(radius * 0.55) as i32 {
                c.put((cx + rx * i as f32) as i32, (cy + ry * i as f32) as i32, a, 200, 220, 255);
            }
        }
    }
}

fn draw_ring(c: &mut Canvas, cx: f32, cy: f32, a: f32, b: f32, peaks: &[Peak]) {
    let th_px = (a * 0.06).max(1.0);
    // v1 scanned the whole window; only the ellipse's bounding box can light up.
    let pad = th_px + 2.0;
    let (x0, x1) = ((cx - a - pad).floor() as i32, (cx + a + pad).ceil() as i32);
    let (y0, y1) = ((cy - b - pad).floor() as i32, (cy + b + pad).ceil() as i32);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let r = ((dx * dx) / (a * a) + (dy * dy) / (b * b)).sqrt();
            let t = 1.0 - ((r - 1.0).abs() / (th_px / a)).min(1.0);
            if t <= 0.0 {
                continue;
            }
            c.put(x, y, (t * 150.0) as u8, 255, 255, 255);

            let ang = (dx / a).atan2(-dy / b); // 0 at top = front
            let (mut best, mut best_amp, mut best_ang) = (0.0f32, 0.0f32, 0.0f32);
            for p in peaks {
                let dist = wrap_pi(ang - p.ang).abs();
                let spread = 0.30 + 0.55 * p.amp; // louder = wider arc
                let hot = (1.0 - dist / spread).max(0.0);
                let win = (1.0 - (hot * PI).cos()) * 0.5;
                let g = win * (0.30 + 0.70 * p.amp); // louder = brighter
                if g > best {
                    best = g;
                    best_amp = p.amp;
                    best_ang = p.ang;
                }
            }
            if best > 0.0 {
                let (rr, gg, bb) = dir_color(best_amp, best_ang);
                c.put(x, y, (best * 235.0).clamp(0.0, 235.0) as u8, rr, gg, bb);
            }
        }
    }

    // Arrowheads on the ring: size and brightness scale with loudness, hue with front/back.
    for p in peaks {
        let (rr, gg, bb) = dir_color(p.amp, p.ang);
        let th = p.ang;
        let (ex, ey) = (cx + a * th.sin(), cy - b * th.cos());
        let (mut nx, mut ny) = (-b * th.cos(), -a * th.sin());
        let nlen = nx.hypot(ny).max(1.0);
        nx /= nlen;
        ny /= nlen;
        let (tx, ty) = (th.cos(), th.sin());
        let half = (3.0 + 5.0 * p.amp) as i32;
        let reach = (4.0 + 7.0 * p.amp) as i32;
        let al = (110.0 + 130.0 * p.amp).clamp(0.0, 240.0) as u8;
        for i in -half..=half {
            for j in 0..reach {
                let xi = ex + nx * i as f32 + a * 0.035 * tx * j as f32;
                let yi = ey + ny * i as f32 + b * 0.035 * ty * j as f32;
                c.put(xi as i32, yi as i32, al, rr, gg, bb);
            }
        }
    }
}

/// v1 `draw_hint_box` background. Returns the text origin for the three lines.
pub fn draw_hint_box(c: &mut Canvas) -> (i32, i32) {
    let (x0, y0) = (14, 14);
    let x1 = (c.w as i32 - 14).min(x0 + 310);
    let y1 = (c.h as i32 - 14).min(y0 + 62);
    c.fill_rect(x0, y0, x1, y1, 170, 12, 12, 14);
    c.fill_rect(x0, y0, x1, y1.min(y0 + 18), 200, 28, 28, 34);
    (x0 + 10, y0 + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(px: &[u32], x0: usize, x1: usize) -> u32 {
        // Summed red-channel energy of warm (front) ring pixels in a column band.
        (0..HUD_AREA_H)
            .flat_map(|y| (x0..x1).map(move |x| px[y * HUD_W + x]))
            .map(|p| (p >> 16) & 0xFF)
            .sum()
    }

    #[test]
    fn a_sound_on_the_right_lights_the_right() {
        let mut px = vec![0u32; HUD_W * HUD_AREA_H];
        let quiet = {
            let mut c = Canvas { w: HUD_W, h: HUD_AREA_H, px: &mut px };
            draw(&mut c, &Layout::default(), &[], false);
            (lit(&px, 0, HUD_W / 2), lit(&px, HUD_W / 2, HUD_W))
        };
        let mut c = Canvas { w: HUD_W, h: HUD_AREA_H, px: &mut px };
        draw(&mut c, &Layout::default(), &[Peak { ang: FRAC_PI_2, amp: 1.0, rear_gain: 0.0 }], false);
        let (l, r) = (lit(&px, 0, HUD_W / 2), lit(&px, HUD_W / 2, HUD_W));
        // The idle sphere is already white (red channel full), so look for a clear rise on the
        // right and no change at all on the left.
        assert!(r > quiet.1 * 3 / 2 && l == quiet.0, "left {l} right {r} (idle {quiet:?})");
    }

    #[test]
    fn normalizer_maps_loudest_to_one_and_drops_silence() {
        let mut n = Normalizer::default();
        let s = |az, strength| Source { azimuth: az, strength, confidence: 1.0 };
        let p = n.peaks(&[s(30.0, 8.0), s(-60.0, 2.0)], false, 1.0, 0.016);
        assert!((p[0].amp - 1.0).abs() < 1e-6 && p[1].amp > 0.4 && p[1].amp < 0.6);
        assert!(p.iter().all(|p| p.rear_gain == 0.0));
        let p = n.peaks(&[s(150.0, 8.0)], true, 1.0, 0.016);
        assert!(p[0].rear_gain > 0.5, "behind in 360 mode gets the rear accent");
        assert!(n.peaks(&[s(0.0, 1e-4)], false, 1.0, 0.016).is_empty());
    }
}
