//! Test signals with a known true azimuth: stereo pan laws, 5.1/7.1 VBAP, and a spherical-head
//! binaural render (Woodworth ITD + broadband head shadow).
//! ponytail: the head model stands in for measured HRTFs; convolve SOFA sets (CIPIC/SADIE) once
//! front/back classification is being worked on.

use std::f32::consts::{FRAC_PI_4, PI};

pub const STEREO: [Option<f32>; 2] = [Some(-30.0), Some(30.0)];
/// Default 5.1 WASAPI order, same as v1 `default_azimuth`: FL FR FC LFE BL BR.
pub const SURROUND_51: [Option<f32>; 6] =
    [Some(-30.0), Some(30.0), Some(0.0), None, Some(-110.0), Some(110.0)];
/// Default 7.1 WASAPI order, same as v1 `default_azimuth`: FL FR FC LFE BL BR SL SR.
pub const SURROUND_71: [Option<f32>; 8] =
    [Some(-30.0), Some(30.0), Some(0.0), None, Some(-135.0), Some(135.0), Some(-90.0), Some(90.0)];

#[derive(Clone, Copy)]
pub enum Band {
    Full,
    Low,  // < ~1.2 kHz, footstep-ish
    High, // > ~3 kHz, gunshot/shell-ish
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

fn one_pole(fc: f32, sr: f32) -> f32 {
    1.0 - (-2.0 * PI * fc / sr).exp()
}

/// Noise bursts every 250 ms, sharp attack, 30 ms decay.
pub fn bursts(len: usize, sr: f32, seed: u64, band: Band) -> Vec<f32> {
    let mut rng = Rng(seed | 1);
    let period = (0.25 * sr) as usize;
    let tau = 0.03 * sr;
    let (a_lo, a_hi) = (one_pole(1200.0, sr), one_pole(3000.0, sr));
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    (0..len)
        .map(|i| {
            let n = rng.next();
            let x = match band {
                Band::Full => n,
                Band::Low => {
                    s1 += a_lo * (n - s1);
                    s2 += a_lo * (s1 - s2);
                    2.0 * s2
                }
                Band::High => {
                    s1 += a_hi * (n - s1);
                    n - s1
                }
            };
            0.3 * x * (-((i % period) as f32) / tau).exp()
        })
        .collect()
}

/// Independent L/R noise: ambience/reverb with no direction.
pub fn diffuse_stereo(len: usize, seed: u64) -> Vec<f32> {
    let mut rng = Rng(seed | 1);
    (0..len * 2).map(|_| 0.09 * rng.next()).collect()
}

/// Constant-power pan, `az` in [-90, 90].
pub fn pan_stereo(mono: &[f32], az: f32) -> Vec<f32> {
    let phi = (az / 90.0 + 1.0) * FRAC_PI_4;
    let (gl, gr) = (phi.cos(), phi.sin());
    mono.iter().flat_map(|&x| [gl * x, gr * x]).collect()
}

/// Linear (-6 dB centre) pan: a different law from the one the analyzer inverts.
pub fn pan_stereo_linear(mono: &[f32], az: f32) -> Vec<f32> {
    let (gl, gr) = ((90.0 - az) / 180.0, (90.0 + az) / 180.0);
    mono.iter().flat_map(|&x| [gl * x, gr * x]).collect()
}

/// Pairwise 2-D VBAP onto `layout` (degrees, `None` = LFE), power-normalized.
pub fn vbap(mono: &[f32], az: f32, layout: &[Option<f32>]) -> Vec<f32> {
    let mut spk: Vec<(usize, f32)> =
        layout.iter().enumerate().filter_map(|(i, a)| a.map(|a| (i, a))).collect();
    spk.sort_by(|a, b| a.1.total_cmp(&b.1));
    let u = |d: f32| (d.to_radians().sin(), d.to_radians().cos());
    let p = u(az);
    let mut gains = vec![0.0f32; layout.len()];
    for w in 0..spk.len() {
        let (i1, a1) = spk[w];
        let (i2, a2) = spk[(w + 1) % spk.len()];
        let (m1, m2) = (u(a1), u(a2));
        let det = m1.0 * m2.1 - m2.0 * m1.1;
        let g1 = (p.0 * m2.1 - m2.0 * p.1) / det;
        let g2 = (m1.0 * p.1 - p.0 * m1.1) / det;
        if g1 >= -1e-6 && g2 >= -1e-6 {
            let n = g1.hypot(g2);
            gains[i1] = g1.max(0.0) / n;
            gains[i2] = g2.max(0.0) / n;
            break;
        }
    }
    mono.iter().flat_map(|&x| gains.iter().map(move |g| g * x)).collect()
}

/// Spherical head: Woodworth ITD (integer samples) + up to 10 dB far-ear shadow. `az` in [-90, 90].
pub fn binaural(mono: &[f32], az: f32, sr: f32) -> Vec<f32> {
    let th = az.to_radians().abs();
    let tau = 0.0875 / 343.0 * (th + th.sin());
    let d = (tau * sr).round() as usize;
    let far = 10f32.powf(-10.0 * th.sin() / 20.0);
    (0..mono.len())
        .flat_map(|i| {
            let near = mono[i];
            let late = if i >= d { far * mono[i - d] } else { 0.0 };
            if az >= 0.0 { [late, near] } else { [near, late] }
        })
        .collect()
}

pub fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}
