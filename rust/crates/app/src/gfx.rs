//! Software drawing into premultiplied `0xAARRGGBB` pixels (the layout of a 32-bit top-down DIB).
//!
//! Shapes are signed-distance functions: each one scans only its bounding box, and coverage comes
//! from the distance to the edge, so every edge is antialiased and shapes alpha-blend over what's
//! below. A small overlay window (a few hundred pixels square) keeps this cheap on the CPU.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const BLACK: Rgb = Rgb(0, 0, 0);
pub const WHITE: Rgb = Rgb(255, 255, 255);

pub struct Canvas<'a> {
    pub w: usize,
    pub h: usize,
    pub px: &'a mut [u32],
    /// Multiplies every blended shape (the HUD opacity setting).
    pub alpha: f32,
}

/// How a distance field is painted.
#[derive(Clone, Copy, Debug)]
pub enum Paint {
    Fill,
    /// Fill with diagonal stripes: a pattern, so "behind" never relies on colour alone.
    Hatch,
    /// Outline of this width; `dash` = on+off period along the outline, in the shape's own units.
    Stroke { width: f32, dash: Option<f32> },
}

impl Canvas<'_> {
    pub fn new(w: usize, h: usize, px: &mut [u32]) -> Canvas<'_> {
        Canvas { w, h, px, alpha: 1.0 }
    }

    /// Overwrite (no blend) with a premultiplied colour, like v1 `putpx_PM`. Panel code uses this.
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

    /// Source-over blend of `c` at alpha `a` (0..1) onto one pixel.
    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        let a = (a * self.alpha).clamp(0.0, 1.0);
        if a <= 0.002 || x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        let d = self.px[i];
        let k = 1.0 - a;
        let ch = |src: u8, shift: u32| (src as f32 * a + ((d >> shift) & 0xFF) as f32 * k + 0.5) as u32;
        self.px[i] = ch(255, 24) << 24 | ch(c.0, 16) << 16 | ch(c.1, 8) << 8 | ch(c.2, 0);
    }

    /// Paint a signed-distance shape over `bbox` (x0, y0, x1, y1). `sd` returns (distance, where
    /// along the outline) for a pixel centre; negative distance is inside.
    fn shape(&mut self, bbox: (f32, f32, f32, f32), c: Rgb, a: f32, paint: Paint, sd: impl Fn(f32, f32) -> (f32, f32)) {
        let x0 = (bbox.0.floor() as i32 - 2).max(0);
        let y0 = (bbox.1.floor() as i32 - 2).max(0);
        let x1 = (bbox.2.ceil() as i32 + 2).min(self.w as i32);
        let y1 = (bbox.3.ceil() as i32 + 2).min(self.h as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let (d, along) = sd(x as f32 + 0.5, y as f32 + 0.5);
                let cov = match paint {
                    Paint::Fill => (0.5 - d).clamp(0.0, 1.0),
                    Paint::Hatch => {
                        if (x + y).rem_euclid(6) >= 2 {
                            continue;
                        }
                        (0.5 - d).clamp(0.0, 1.0)
                    }
                    Paint::Stroke { width, dash } => {
                        if let Some(p) = dash {
                            if along.rem_euclid(p) > p * 0.55 {
                                continue;
                            }
                        }
                        (width * 0.5 - d.abs() + 0.5).clamp(0.0, 1.0)
                    }
                };
                if cov > 0.0 {
                    self.blend(x, y, c, a * cov);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgb, a: f32, paint: Paint) {
        let pad = stroke_pad(paint);
        self.shape((cx - r - pad, cy - r - pad, cx + r + pad, cy + r + pad), c, a, paint, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            ((dx * dx + dy * dy).sqrt() - r, dx.atan2(-dy).to_degrees() * r.to_radians())
        });
    }

    /// Ellipse outline, optionally only the arc `(centre_deg, span_deg)` where 0° is the top and
    /// angles grow clockwise (the HUD's azimuth convention). Dash period is in degrees.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse_stroke(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, width: f32, c: Rgb, a: f32, arc: Option<(f32, f32)>, dash_deg: Option<f32>) {
        let pad = width + 1.0;
        self.shape(
            (cx - rx - pad, cy - ry - pad, cx + rx + pad, cy + ry + pad),
            c,
            a,
            Paint::Stroke { width, dash: dash_deg },
            |x, y| {
                let (dx, dy) = (x - cx, y - cy);
                let deg = (dx / rx).atan2(-dy / ry).to_degrees();
                if let Some((centre, span)) = arc {
                    if angle_diff(deg, centre).abs() > span * 0.5 {
                        return (f32::MAX, 0.0);
                    }
                }
                // Distance to an ellipse, first order: implicit value over gradient length.
                let g = (dx / rx).powi(2) + (dy / ry).powi(2) - 1.0;
                let grad = ((2.0 * dx / (rx * rx)).powi(2) + (2.0 * dy / (ry * ry)).powi(2)).sqrt().max(1e-6);
                (g / grad, deg.rem_euclid(360.0))
            },
        );
    }

    /// Soft-edged filled ellipse (a scrim under HUD marks): opaque to `1 - soft`, fading to the rim.
    #[allow(clippy::too_many_arguments)]
    pub fn soft_ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, soft: f32, c: Rgb, a: f32) {
        let (x0, y0) = (((cx - rx).floor() as i32).max(0), ((cy - ry).floor() as i32).max(0));
        let (x1, y1) = (((cx + rx).ceil() as i32).min(self.w as i32), ((cy + ry).ceil() as i32).min(self.h as i32));
        for y in y0..y1 {
            for x in x0..x1 {
                let r = (((x as f32 + 0.5 - cx) / rx).powi(2) + ((y as f32 + 0.5 - cy) / ry).powi(2)).sqrt();
                let cov = ((1.0 - r) / soft).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, a * cov * cov * (3.0 - 2.0 * cov)); // smoothstep
                }
            }
        }
    }

    /// Any polygon (concave is fine), filled, hatched or outlined.
    pub fn polygon(&mut self, pts: &[(f32, f32)], c: Rgb, a: f32, paint: Paint) {
        let pad = stroke_pad(paint);
        let bx0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min) - pad;
        let by0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min) - pad;
        let bx1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) + pad;
        let by1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max) + pad;
        self.shape((bx0, by0, bx1, by1), c, a, paint, |x, y| polygon_sd(pts, x, y));
    }

    /// Straight line with round caps.
    #[allow(clippy::too_many_arguments)]
    pub fn line(&mut self, ax: f32, ay: f32, bx: f32, by: f32, width: f32, c: Rgb, a: f32) {
        let pad = width;
        let bbox = (ax.min(bx) - pad, ay.min(by) - pad, ax.max(bx) + pad, ay.max(by) + pad);
        self.shape(bbox, c, a, Paint::Fill, |x, y| {
            let (px, py, ex, ey) = (x - ax, y - ay, bx - ax, by - ay);
            let t = ((px * ex + py * ey) / (ex * ex + ey * ey).max(1e-6)).clamp(0.0, 1.0);
            ((px - ex * t).hypot(py - ey * t) - width * 0.5, 0.0)
        });
    }

    /// Annular sector: radii `r0..r1`, centred on `centre_deg` (0° up, clockwise), `half_deg` each way.
    #[allow(clippy::too_many_arguments)]
    pub fn wedge(&mut self, cx: f32, cy: f32, r0: f32, r1: f32, centre_deg: f32, half_deg: f32, c: Rgb, a: f32, paint: Paint) {
        let pad = r1 + stroke_pad(paint);
        self.shape((cx - pad, cy - pad, cx + pad, cy + pad), c, a, paint, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            let r = dx.hypot(dy);
            let deg = dx.atan2(-dy).to_degrees();
            let ang = (angle_diff(deg, centre_deg).abs() - half_deg).to_radians() * r;
            let d = (r0 - r).max(r - r1).max(ang);
            (d, deg * r.to_radians() + r)
        });
    }

    /// Axis-aligned rectangle with corner radius. Corners smaller than a full pill end are
    /// continuous (squircle-like, an L4 superellipse), so the curve eases into the straight edge
    /// instead of meeting it at a visible joint; the radius is stretched 1.25× so the corner reads
    /// as round as a circular one of `radius`.
    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, c: Rgb, a: f32, paint: Paint) {
        let pad = stroke_pad(paint);
        let (cx, cy, hx, hy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5, (x1 - x0) * 0.5, (y1 - y0) * 0.5);
        let squircle = radius > 0.0 && radius < hx.min(hy) - 0.5;
        let radius = if squircle { (radius * 1.25).min(hx.min(hy)) } else { radius };
        self.shape((x0 - pad, y0 - pad, x1 + pad, y1 + pad), c, a, paint, |x, y| {
            let (qx, qy) = ((x - cx).abs() - hx + radius, (y - cy).abs() - hy + radius);
            let (ox, oy) = (qx.max(0.0), qy.max(0.0));
            let outside = if squircle { (ox.powi(4) + oy.powi(4)).sqrt().sqrt() } else { ox.hypot(oy) };
            (outside + qx.max(qy).min(0.0) - radius, x + y)
        });
    }
}

fn stroke_pad(p: Paint) -> f32 {
    match p {
        Paint::Stroke { width, .. } => width + 1.0,
        _ => 1.0,
    }
}

/// Signed angular difference a - b in degrees, in (-180, 180].
pub fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// Signed distance to a polygon (Inigo Quilez's sdPolygon), plus how far along the outline the
/// nearest edge point is, for dashes.
fn polygon_sd(v: &[(f32, f32)], x: f32, y: f32) -> (f32, f32) {
    let mut best = f32::MAX;
    let mut along = 0.0;
    let mut walked = 0.0;
    let mut inside = false;
    let n = v.len();
    for i in 0..n {
        let (a, b) = (v[i], v[(i + 1) % n]);
        let (ex, ey, wx, wy) = (b.0 - a.0, b.1 - a.1, x - a.0, y - a.1);
        let len2 = (ex * ex + ey * ey).max(1e-9);
        let t = ((wx * ex + wy * ey) / len2).clamp(0.0, 1.0);
        let d2 = (wx - ex * t).powi(2) + (wy - ey * t).powi(2);
        if d2 < best {
            best = d2;
            along = walked + t * len2.sqrt();
        }
        walked += len2.sqrt();
        // crossing test for inside/outside
        if (a.1 > y) != (b.1 > y) && x < a.0 + (y - a.1) * ex / ey {
            inside = !inside;
        }
    }
    let d = best.sqrt();
    (if inside { -d } else { d }, along)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_source_over_on_premultiplied_pixels() {
        let mut px = vec![0u32; 1];
        let mut c = Canvas::new(1, 1, &mut px);
        c.blend(0, 0, WHITE, 0.5);
        assert_eq!(c.px[0] >> 24, 128); // half-transparent white over nothing
        c.blend(0, 0, BLACK, 1.0);
        assert_eq!(c.px[0], 0xFF00_0000); // opaque black covers it
    }

    #[test]
    fn filled_circle_covers_its_centre_not_its_corner() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas::new(20, 20, &mut px);
        c.circle(10.0, 10.0, 6.0, WHITE, 1.0, Paint::Fill);
        assert_eq!(c.px[10 * 20 + 10] >> 24, 255);
        assert_eq!(c.px[0], 0);
    }

    #[test]
    fn concave_polygon_inside_test() {
        // A chevron: the notch point must be outside.
        let chevron = [(10.0, 0.0), (-7.0, 8.0), (-2.5, 0.0), (-7.0, -8.0)];
        assert!(polygon_sd(&chevron, 5.0, 0.0).0 < 0.0);
        assert!(polygon_sd(&chevron, -5.0, 0.0).0 > 0.0);
    }
}
