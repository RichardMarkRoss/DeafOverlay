//! The in-game HUD in three user-selectable styles (from `design/HUD Concepts.html`):
//!
//! - **Orbit**: a faint wireframe orb over an F/R/B/L ring; arcs and chevrons on the ring.
//! - **Radar**: a top-down disc of 16 sectors (or thin arcs around the crosshair).
//! - **Compass tape**: a 360° bearing strip, front in the middle, behind at both ends.
//!
//! Shared encoding (accessibility guidance: never colour alone): loudness = size, thickness and
//! opacity. Front = solid shape. Behind (5.1/7.1) = hollow or hatched + "BEHIND". Behind as guessed
//! by the stereo front/back model = dashed + "BEHIND?". Every style draws a dark scrim so it
//! survives bright scenes. Platform-free: draws into a [`Canvas`] and returns the text it wants
//! (the window renders text with GDI).

use crate::gfx::{Canvas, Paint, Rgb, BLACK, WHITE};
use dsp::Source;
use std::f32::consts::PI;

/// Loudness → 0..1 auto-gain (see [`Normalizer`]).
const REF_MIN: f32 = 1.0; // ponytail: calibration knob. Tracker strengths below ~REF_MIN read as faint.
const REF_HALF_LIFE_S: f32 = 3.0;
const MIN_AMP: f32 = 0.02;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HudStyle {
    #[default]
    Orbit,
    Radar,
    Tape,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaletteKind {
    #[default]
    Brand,
    ColorBlind,
    HighContrast,
}

/// HUD colours. `front`/`rear` only reinforce the shape encoding.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub front: Rgb,
    pub rear: Rgb,
    pub line: Rgb,
    pub muted: Rgb,
    pub scrim: f32,
}

impl PaletteKind {
    pub fn colors(self) -> Palette {
        match self {
            // Brand amber / cool blue (the demo's oklch values, in sRGB).
            PaletteKind::Brand => Palette { front: Rgb(246, 178, 60), rear: Rgb(98, 190, 236), line: Rgb(246, 246, 246), muted: Rgb(170, 170, 170), scrim: 0.55 },
            // Okabe-Ito orange / sky blue: distinct under the common colour-vision deficiencies.
            PaletteKind::ColorBlind => Palette { front: Rgb(230, 159, 0), rear: Rgb(86, 180, 233), line: Rgb(246, 246, 246), muted: Rgb(170, 170, 170), scrim: 0.55 },
            PaletteKind::HighContrast => Palette { front: Rgb(255, 230, 0), rear: WHITE, line: WHITE, muted: Rgb(215, 215, 215), scrim: 0.78 },
        }
    }
}

/// Everything about how the HUD looks (persisted with the settings).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudParams {
    pub style: HudStyle,
    /// Radar only: thin arcs around the aim point instead of the disc.
    pub crosshair: bool,
    pub palette: PaletteKind,
    pub scale: f32,   // 0.6 .. 1.6
    pub opacity: f32, // 0.3 .. 1.0
}

impl Default for HudParams {
    fn default() -> Self {
        Self { style: HudStyle::Orbit, crosshair: false, palette: PaletteKind::Brand, scale: 1.0, opacity: 1.0 }
    }
}

/// Window size the style needs at 100% scale (the demo's dashed bounds).
fn base_size(p: &HudParams) -> (f32, f32) {
    match p.style {
        HudStyle::Orbit => (240.0, 240.0),
        HudStyle::Radar if p.crosshair => (180.0, 180.0),
        HudStyle::Radar => (200.0, 200.0),
        HudStyle::Tape => (520.0, 72.0),
    }
}

/// Window size in pixels for these settings.
pub fn size(p: &HudParams) -> (usize, usize) {
    let (w, h) = base_size(p);
    ((w * p.scale).round() as usize, (h * p.scale).round() as usize)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rear {
    None,
    /// Measured: 5.1/7.1 channels.
    Real,
    /// Stereo front/back model's guess.
    Guess,
}

/// One direction to show. `ang`: radians, 0 = front, + = right. `amp`: 0..1 loudness.
#[derive(Clone, Copy, Debug)]
pub struct Peak {
    pub ang: f32,
    pub amp: f32,
    pub rear: Rear,
}

/// What the audio can and can't tell, for the "rear needs 7.1" notes.
#[derive(Clone, Copy, Debug)]
pub struct Scene {
    pub full_360: bool,
    pub front_back: bool,
}

/// Text for the window layer to draw, centred on (x, y) in canvas pixels, with a dark outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub x: f32,
    pub y: f32,
    pub text: &'static str,
    pub rgb: Rgb,
    pub px: i32,
    pub mono: bool,
}

/// Tracker strengths are level-dependent and unbounded; the HUD wants 0..1. A slow auto-gain
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
    /// Quietest first, so the loudest draws on top. Anything past ±90° is behind: measured on
    /// 5.1/7.1, otherwise it can only have come from the stereo front/back guess.
    pub fn peaks(&mut self, sources: &[Source], full_360: bool, sensitivity: f32, dt: f32) -> Vec<Peak> {
        let loudest = sources.iter().map(|s| s.strength).fold(0.0, f32::max);
        self.reference = (self.reference * 0.5f32.powf(dt / REF_HALF_LIFE_S)).max(loudest).max(REF_MIN);
        let mut out: Vec<Peak> = sources
            .iter()
            .map(|s| {
                let amp = ((s.strength / self.reference).sqrt() * s.confidence * sensitivity).clamp(0.0, 1.0);
                let rear = match (s.azimuth.abs() > 90.0, full_360) {
                    (false, _) => Rear::None,
                    (true, true) => Rear::Real,
                    (true, false) => Rear::Guess,
                };
                Peak { ang: s.azimuth.to_radians(), amp, rear }
            })
            .filter(|p| p.amp >= MIN_AMP)
            .collect();
        out.sort_by(|a, b| a.amp.total_cmp(&b.amp));
        out
    }
}

/// Draw the HUD with its top-left at `origin` in the canvas. Returns the labels to draw on top.
pub fn draw(c: &mut Canvas, origin: (f32, f32), p: &HudParams, peaks: &[Peak], scene: Scene) -> Vec<Label> {
    let mut out = Hud { labels: Vec::new(), pal: p.palette.colors(), k: p.scale, scene };
    let before = c.alpha;
    c.alpha = p.opacity;
    match p.style {
        HudStyle::Orbit => out.orbit(c, origin, peaks),
        HudStyle::Radar if p.crosshair => out.crosshair(c, origin, peaks),
        HudStyle::Radar => out.radar(c, origin, peaks),
        HudStyle::Tape => out.tape(c, origin, peaks),
    }
    c.alpha = before;
    out.labels
}

struct Hud {
    labels: Vec<Label>,
    pal: Palette,
    k: f32,
    scene: Scene,
}

impl Hud {
    fn label(&mut self, x: f32, y: f32, text: &'static str, rgb: Rgb, px: f32, mono: bool) {
        let px = (px * self.k.max(1.0)).round().max(10.0) as i32; // below 10 px, outlined text smears
        self.labels.push(Label { x, y, text, rgb, px, mono });
    }

    fn color(&self, p: &Peak) -> Rgb {
        if p.rear == Rear::None { self.pal.front } else { self.pal.rear }
    }

    fn rear_tag(&mut self, p: &Peak, x: f32, y: f32) {
        match p.rear {
            Rear::Real => self.label(x, y, "BEHIND", self.pal.rear, 10.0, true),
            Rear::Guess => self.label(x, y, "BEHIND?", self.pal.rear, 10.0, true),
            Rear::None => {}
        }
    }

    /// Stereo can't see behind (only the front/back guess can): say so instead of hiding it.
    fn rear_unknown(&self) -> bool {
        !self.scene.full_360
    }

    fn rear_note(&self) -> &'static str {
        if self.scene.front_back { "rear: guess only" } else { "rear needs 7.1" }
    }

    /// Chevron pointing along `dir` (canvas radians). Shape carries front / behind / guessed.
    #[allow(clippy::too_many_arguments)]
    fn chevron(&self, c: &mut Canvas, x: f32, y: f32, dir: f32, size: f32, p: &Peak, alpha: f32) {
        let (s, co) = dir.sin_cos();
        let pts: Vec<(f32, f32)> = [(1.0f32, 0.0f32), (-0.7, 0.8), (-0.25, 0.0), (-0.7, -0.8)]
            .iter()
            .map(|(u, v)| (x + (u * co - v * s) * size, y + (u * s + v * co) * size))
            .collect();
        c.polygon(&pts, BLACK, 0.75 * alpha, Paint::Stroke { width: 3.5, dash: None });
        match p.rear {
            Rear::None => c.polygon(&pts, self.pal.front, alpha, Paint::Fill),
            Rear::Real => c.polygon(&pts, self.pal.rear, alpha, Paint::Stroke { width: 2.0, dash: None }),
            Rear::Guess => c.polygon(&pts, self.pal.rear, alpha, Paint::Stroke { width: 2.0, dash: Some(5.0) }),
        }
    }

    fn orbit(&mut self, c: &mut Canvas, (ox, oy): (f32, f32), peaks: &[Peak]) {
        let k = self.k;
        let (cx, cy) = (ox + 120.0 * k, oy + 120.0 * k);
        let (orb_y, r) = (cy - 42.0 * k, 44.0 * k);
        let (ring_y, rx, ry) = (cy + 44.0 * k, 100.0 * k, 36.0 * k);
        c.soft_ellipse(cx, cy + 8.0 * k, 118.0 * k, 114.0 * k, 0.3, BLACK, self.pal.scrim);

        // wireframe orb: faint, so it doesn't compete with the game's own white HUD
        let line = self.pal.line;
        c.ellipse_stroke(cx, orb_y, r, r, 1.0, line, 0.3, None, None);
        for f in [-0.55f32, 0.0, 0.55] {
            let w = (1.0 - f * f).sqrt();
            c.ellipse_stroke(cx, orb_y + f * r, r * w, r * 0.28 * w, 1.0, line, 0.25, None, None);
        }
        for f in [0.35, 0.75] {
            c.ellipse_stroke(cx, orb_y, r * f, r, 1.0, line, 0.25, None, None);
        }
        // the orb glows toward the loudest sound (bright in front, dim behind)
        if let Some(top) = peaks.last() {
            let (gx, gy) = (cx + top.ang.sin() * r * 0.62, orb_y - top.ang.cos() * r * 0.28);
            let a = if top.rear == Rear::None { 0.55 } else { 0.3 } * top.amp;
            c.soft_ellipse(gx, gy, r * 0.62, r * 0.62, 1.0, self.color(top), a);
        }

        c.ellipse_stroke(cx, ring_y, rx, ry, 1.5, line, 0.7, None, None);
        let pt = |deg: f32, grow: f32| {
            let a = deg.to_radians();
            (cx + (rx + grow) * a.sin(), ring_y - (ry + grow * 0.36) * a.cos())
        };
        for (deg, t) in [(0.0, "F"), (90.0, "R"), (180.0, "B"), (-90.0, "L")] {
            // A chevron sitting on this letter already says the direction; don't stack them.
            if peaks.iter().any(|p| crate::gfx::angle_diff(p.ang.to_degrees(), deg).abs() < 12.0) {
                continue;
            }
            let (x, y) = pt(deg, 14.0 * k);
            let dim = deg == 180.0 && self.rear_unknown() && !self.scene.front_back;
            self.label(x, y, t, if dim { self.pal.muted } else { line }, 11.0, false);
        }
        for p in peaks {
            let deg = p.ang.to_degrees();
            let dash = if p.rear == Rear::Guess { Some(8.0) } else { None };
            let (w, a) = ((2.0 + 6.0 * p.amp) * k, 0.35 + 0.65 * p.amp);
            c.ellipse_stroke(cx, ring_y, rx, ry, w, self.color(p), a, Some((deg, 14.0 + 36.0 * p.amp)), dash);
            let (x, y) = pt(deg, (10.0 + 6.0 * p.amp) * k);
            let dir = (-p.ang.cos() / ry).atan2(p.ang.sin() / rx); // ellipse outward normal
            self.chevron(c, x, y, dir, (6.0 + 7.0 * p.amp) * k, p, 0.5 + 0.5 * p.amp);
            self.rear_tag(p, x, y + 16.0 * k);
        }
    }

    fn radar(&mut self, c: &mut Canvas, (ox, oy): (f32, f32), peaks: &[Peak]) {
        let k = self.k;
        let (cx, cy) = (ox + 100.0 * k, oy + 100.0 * k);
        let (r0, r) = (11.0 * k, 76.0 * k);
        c.circle(cx, cy, r + 8.0 * k, BLACK, self.pal.scrim, Paint::Fill);
        if self.rear_unknown() {
            c.wedge(cx, cy, 0.0, r, 180.0, 90.0, WHITE, 0.07, Paint::Hatch);
            self.label(cx, cy + r + 2.0 * k, self.rear_note(), self.pal.muted, 9.0, true);
        }
        let line = self.pal.line;
        for (f, a) in [(0.33, 0.18), (0.66, 0.18), (1.0, 0.55)] {
            c.circle(cx, cy, r0 + (r - r0) * f, line, a, Paint::Stroke { width: 1.0, dash: None });
        }
        for i in 0..16 {
            let (s, co) = (i as f32 * 22.5).to_radians().sin_cos();
            c.line(cx + s * r0, cy - co * r0, cx + s * r, cy - co * r, 1.0, line, 0.12);
        }
        for (deg, t) in [(0.0f32, "000"), (90.0, "090"), (180.0, "180"), (270.0, "270")] {
            let (s, co) = deg.to_radians().sin_cos();
            self.label(cx + s * (r + 14.0 * k), cy - co * (r + 14.0 * k), t, self.pal.muted, 9.0, true);
        }
        // loudest sound per sector
        let mut sectors: [Option<Peak>; 16] = [None; 16];
        for p in peaks {
            let i = ((p.ang.to_degrees() / 22.5).round() as i32).rem_euclid(16) as usize;
            if sectors[i].is_none_or(|q| q.amp < p.amp) {
                sectors[i] = Some(*p);
            }
        }
        for (i, p) in sectors.iter().enumerate() {
            let Some(p) = p else { continue };
            let centre = i as f32 * 22.5;
            let rr = r0 + (r - r0) * (0.25 + 0.75 * p.amp);
            let a = 0.45 + 0.55 * p.amp;
            match p.rear {
                Rear::None => c.wedge(cx, cy, r0, rr, centre, 10.0, self.pal.front, a, Paint::Fill),
                Rear::Real => {
                    c.wedge(cx, cy, r0, rr, centre, 10.0, self.pal.rear, a, Paint::Hatch);
                    c.wedge(cx, cy, r0, rr, centre, 10.0, self.pal.rear, a, Paint::Stroke { width: 1.5, dash: None });
                }
                Rear::Guess => c.wedge(cx, cy, r0, rr, centre, 10.0, self.pal.rear, a, Paint::Stroke { width: 1.5, dash: Some(6.0) }),
            }
            let (s, co) = centre.to_radians().sin_cos();
            self.rear_tag(p, cx + s * (rr + 12.0 * k), cy - co * (rr + 12.0 * k));
        }
        let you = [(cx, cy - 7.0 * k), (cx + 5.0 * k, cy + 5.0 * k), (cx - 5.0 * k, cy + 5.0 * k)];
        c.polygon(&you, line, 1.0, Paint::Fill);
    }

    /// Radar's crosshair mode: thin arcs around the aim point, nothing else.
    fn crosshair(&mut self, c: &mut Canvas, (ox, oy): (f32, f32), peaks: &[Peak]) {
        let k = self.k;
        let (cx, cy, r) = (ox + 90.0 * k, oy + 90.0 * k, 62.0 * k);
        for p in peaks {
            let deg = p.ang.to_degrees();
            let span = 2.0 * (10.0 + 22.0 * p.amp);
            let w = (3.0 + 5.0 * p.amp) * k;
            c.ellipse_stroke(cx, cy, r, r, w + 3.0, BLACK, 0.7, Some((deg, span)), None);
            let dash = match p.rear {
                Rear::None => None,
                Rear::Real => Some(12.0),
                Rear::Guess => Some(6.0),
            };
            c.ellipse_stroke(cx, cy, r, r, w, self.color(p), 0.5 + 0.5 * p.amp, Some((deg, span)), dash);
            self.rear_tag(p, cx + p.ang.sin() * (r + 18.0 * k), cy - p.ang.cos() * (r + 18.0 * k));
        }
    }

    fn tape(&mut self, c: &mut Canvas, (ox, oy): (f32, f32), peaks: &[Peak]) {
        let k = self.k;
        let (cx, cy) = (ox + 260.0 * k, oy + 36.0 * k);
        let (w, h) = (480.0 * k, 44.0 * k);
        let (x0, top) = (cx - w / 2.0, cy - h / 2.0);
        let xat = |deg: f32| cx + deg / 180.0 * (w / 2.0);
        c.rect(x0 - 16.0 * k, top - 8.0 * k, x0 + w + 16.0 * k, top + h + 14.0 * k, 10.0 * k, BLACK, self.pal.scrim, Paint::Fill);
        if self.rear_unknown() {
            c.rect(x0, top, xat(-90.0), top + h, 0.0, WHITE, 0.08, Paint::Hatch);
            c.rect(xat(90.0), top, x0 + w, top + h, 0.0, WHITE, 0.08, Paint::Hatch);
            let note = self.rear_note();
            self.label((x0 + xat(-90.0)) / 2.0, top + h * 0.72, note, self.pal.muted, 9.0, true);
            self.label((xat(90.0) + x0 + w) / 2.0, top + h * 0.72, note, self.pal.muted, 9.0, true);
        }
        let line = self.pal.line;
        for i in 0..=24 {
            let deg = -180.0 + 15.0 * i as f32;
            let major = i % 3 == 0;
            let len = if major { 8.0 } else { 4.0 } * k;
            c.line(xat(deg), top, xat(deg), top + len, 1.0, line, if major { 0.6 } else { 0.25 });
        }
        for (d, t) in [(-180.0, "B"), (-90.0, "L"), (0.0, "F"), (90.0, "R"), (180.0, "B")] {
            self.label(xat(d), top + 16.0 * k, t, if d == 0.0 { line } else { self.pal.muted }, 11.0, false);
        }
        let notch = [(cx, top + h + 2.0 * k), (cx - 5.0 * k, top + h + 9.0 * k), (cx + 5.0 * k, top + h + 9.0 * k)];
        c.polygon(&notch, line, 1.0, Paint::Fill);
        for p in peaks {
            let x = xat(p.ang.to_degrees());
            let (bh, bw, yb) = ((8.0 + 24.0 * p.amp) * k, (3.0 + 4.0 * p.amp) * k, top + h);
            let a = 0.45 + 0.55 * p.amp;
            c.rect(x - bw / 2.0 - 1.5, yb - bh - 1.5, x + bw / 2.0 + 1.5, yb + 1.5, 0.0, BLACK, 0.7 * a, Paint::Fill);
            let paint = match p.rear {
                Rear::None => Paint::Fill,
                Rear::Real => Paint::Stroke { width: 1.5, dash: None },
                Rear::Guess => Paint::Stroke { width: 1.5, dash: Some(5.0) },
            };
            c.rect(x - bw / 2.0, yb - bh, x + bw / 2.0, yb, 0.0, self.color(p), a, paint);
            self.chevron(c, x, yb - bh - 7.0 * k, PI / 2.0, (4.0 + 3.0 * p.amp) * k, p, a);
            if p.rear == Rear::Guess {
                self.label(x + 9.0 * k, yb - bh - 7.0 * k, "?", self.pal.rear, 10.0, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(style: HudStyle, crosshair: bool, peaks: &[Peak]) -> (Vec<u32>, usize, usize) {
        let p = HudParams { style, crosshair, ..Default::default() };
        let (w, h) = size(&p);
        let mut px = vec![0u32; w * h];
        draw(&mut Canvas::new(w, h, &mut px), (0.0, 0.0), &p, peaks, Scene { full_360: true, front_back: false });
        (px, w, h)
    }

    /// Summed alpha in the left and right halves.
    fn halves(px: &[u32], w: usize, h: usize) -> (i64, i64) {
        let side = |r: std::ops::Range<usize>| (0..h).flat_map(|y| r.clone().map(move |x| (px[y * w + x] >> 24) as i64)).sum();
        (side(0..w / 2), side(w / 2..w))
    }

    #[test]
    fn every_style_lights_the_side_the_sound_is_on() {
        let right = [Peak { ang: PI / 2.0, amp: 1.0, rear: Rear::None }];
        for (style, crosshair) in [(HudStyle::Orbit, false), (HudStyle::Radar, false), (HudStyle::Radar, true), (HudStyle::Tape, false)] {
            let (idle, w, h) = render(style, crosshair, &[]);
            let (lit, ..) = render(style, crosshair, &right);
            let ((il, ir), (l, r)) = (halves(&idle, w, h), halves(&lit, w, h));
            let (gain_l, gain_r) = (l - il, r - ir);
            assert!(gain_r > 2000 && gain_r > 3 * gain_l.max(0), "{style:?}/crosshair {crosshair}: left +{gain_l}, right +{gain_r}");
        }
    }

    #[test]
    fn behind_is_labelled_and_a_guess_says_so() {
        let behind = |rear| [Peak { ang: 150f32.to_radians(), amp: 0.8, rear }];
        let p = HudParams::default();
        let scene = Scene { full_360: false, front_back: true };
        let mut px = vec![0u32; 240 * 240];
        let labels = draw(&mut Canvas::new(240, 240, &mut px), (0.0, 0.0), &p, &behind(Rear::Real), scene);
        assert!(labels.iter().any(|l| l.text == "BEHIND"));
        let labels = draw(&mut Canvas::new(240, 240, &mut px), (0.0, 0.0), &p, &behind(Rear::Guess), scene);
        assert!(labels.iter().any(|l| l.text == "BEHIND?"));
    }

    #[test]
    fn normalizer_scales_to_the_loudest_and_marks_behind_by_layout() {
        let mut n = Normalizer::default();
        let s = |az, strength| Source { azimuth: az, strength, confidence: 1.0 };
        let p = n.peaks(&[s(30.0, 8.0), s(-60.0, 2.0)], false, 1.0, 0.016);
        assert!((p[1].amp - 1.0).abs() < 1e-6 && p[0].amp > 0.4 && p[0].amp < 0.6, "quietest first");
        assert_eq!(n.peaks(&[s(150.0, 8.0)], true, 1.0, 0.016)[0].rear, Rear::Real);
        assert_eq!(n.peaks(&[s(150.0, 8.0)], false, 1.0, 0.016)[0].rear, Rear::Guess);
        assert!(n.peaks(&[s(0.0, 1e-4)], false, 1.0, 0.016).is_empty());
    }
}
