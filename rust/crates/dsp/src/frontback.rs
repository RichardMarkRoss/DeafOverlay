//! Front/back guess for stereo *HRTF* mixes ("headphone 3D" games). Experimental, off by default.
//!
//! A stereo mix of a source at 30° and one at 150° has the same interaural time and broadband level
//! difference (the "cone of confusion"). What differs is how the ear shapes each frequency band:
//! the per-band level difference between the ears, and the spectral shape. A logistic regression
//! over those features, trained on measured HRTFs, gives P(behind) per tracked source.
//!
//! Plain amplitude-panned stereo carries none of these cues, and it's in the training set as
//! "front", so non-HRTF games should stay on the front arc.
//!
//! Retrain with `cargo run --release -p dsp --example frontback -- <dir of .hrir>` (see
//! `rust/tools/sofa_to_hrir.py`) and paste the printed `SHIPPED` model below.

pub const FB_BANDS: usize = 13;
pub const NF: usize = 2 * FB_BANDS + 1;
const F_LO: f32 = 700.0;
const F_HI: f32 = 16_000.0;
/// A band with less than this share of the source's power counts as missing: no guess.
const MIN_BAND_SHARE: f32 = 1e-4;

/// Band index for a frequency, `None` outside 700 Hz – 16 kHz. Log-spaced (~1/3 octave).
pub(crate) fn band_of(hz: f32) -> Option<usize> {
    if !(F_LO..F_HI).contains(&hz) {
        return None;
    }
    Some(((hz / F_LO).ln() / (F_HI / F_LO).ln() * FB_BANDS as f32) as usize).map(|b| b.min(FB_BANDS - 1))
}

/// Features from per-band left/right power of one source. Folded so the near ear is always
/// "right" (front/back cues are mirror-symmetric). `None` if the source doesn't cover every band.
pub fn features(pl: &[f32; FB_BANDS], pr: &[f32; FB_BANDS]) -> Option<[f32; NF]> {
    let (sl, sr): (f32, f32) = (pl.iter().sum(), pr.iter().sum());
    let total = sl + sr;
    if total <= 0.0 {
        return None;
    }
    let (near, far) = if sl > sr { (pl, pr) } else { (pr, pl) };
    let mut f = [0.0; NF];
    let mut shape_mean = 0.0;
    for b in 0..FB_BANDS {
        let (n, fr) = (near[b], far[b]);
        if n + fr < MIN_BAND_SHARE * total || fr <= 0.0 || n <= 0.0 {
            return None;
        }
        f[b] = 10.0 * (n / fr).log10(); // ILD, dB
        f[FB_BANDS + b] = 10.0 * (n + fr).log10(); // spectral shape, dB
        shape_mean += f[FB_BANDS + b] / FB_BANDS as f32;
    }
    for v in &mut f[FB_BANDS..2 * FB_BANDS] {
        *v -= shape_mean; // shape relative to its own mean: level-independent
    }
    f[NF - 1] = 10.0 * (sl.max(sr) / sl.min(sr).max(1e-30)).log10(); // broadband lateral ILD
    Some(f)
}

/// Standardized logistic regression.
#[derive(Clone, Debug)]
pub struct Model {
    pub mu: [f32; NF],
    pub sd: [f32; NF],
    pub w: [f32; NF],
    pub b: f32,
}

impl Model {
    /// P(behind).
    pub fn p_back(&self, f: &[f32; NF]) -> f32 {
        let z: f32 = (0..NF).map(|i| (f[i] - self.mu[i]) / self.sd[i] * self.w[i]).sum::<f32>() + self.b;
        1.0 / (1.0 + (-z).exp())
    }

    /// Plain gradient-descent logistic regression with L2; `y` is 1.0 for behind.
    pub fn train(x: &[[f32; NF]], y: &[f32], l2: f32, iters: usize) -> Self {
        let n = x.len() as f32;
        let mut mu = [0.0; NF];
        let mut sd = [0.0; NF];
        for i in 0..NF {
            mu[i] = x.iter().map(|r| r[i]).sum::<f32>() / n;
            sd[i] = (x.iter().map(|r| (r[i] - mu[i]).powi(2)).sum::<f32>() / n).sqrt().max(1e-6);
        }
        let z: Vec<[f32; NF]> = x.iter().map(|r| std::array::from_fn(|i| (r[i] - mu[i]) / sd[i])).collect();
        let (mut w, mut b) = ([0.0f32; NF], 0.0f32);
        let lr = 0.5;
        for _ in 0..iters {
            let mut gw = [0.0f32; NF];
            let mut gb = 0.0f32;
            for (r, &t) in z.iter().zip(y) {
                let p = 1.0 / (1.0 + (-(r.iter().zip(&w).map(|(a, c)| a * c).sum::<f32>() + b)).exp());
                for i in 0..NF {
                    gw[i] += (p - t) * r[i];
                }
                gb += p - t;
            }
            for i in 0..NF {
                w[i] -= lr * (gw[i] / n + l2 * w[i]);
            }
            b -= lr * gb / n;
        }
        Self { mu, sd, w, b }
    }

    /// Rust source for pasting into [`SHIPPED`].
    pub fn to_rust(&self) -> String {
        let arr = |a: &[f32; NF]| a.iter().map(|v| format!("{v:.5}")).collect::<Vec<_>>().join(", ");
        format!(
            "pub const SHIPPED: Model = Model {{\n    mu: [{}],\n    sd: [{}],\n    w: [{}],\n    b: {:.5},\n}};",
            arr(&self.mu),
            arr(&self.sd),
            arr(&self.w),
            self.b
        )
    }
}

/// Trained on MIT KEMAR + 10 CIPIC heads (horizontal plane, random-spectrum bursts) plus
/// amplitude-panned stereo as "front". Measured on a head from another lab (KEMAR) with a model
/// that never saw it, through the real Analyzer + tracker: every "behind" shown was right, and front
/// sounds and panned stereo were never shown behind. But only ~17% of the time a rear sound
/// was playing did it show as behind (~7% with ambience): the thresholds favour silence
/// over a wrong answer. Regenerate with the `frontback` example.
#[rustfmt::skip]
#[allow(clippy::excessive_precision)] // generated by the `frontback` example
pub const SHIPPED: Model = Model {
    mu: [5.55609, 6.63794, 7.33564, 7.86647, 7.95763, 7.73889, 7.32961, 8.13158, 10.31920, 13.48047, 13.27152, 12.20842, 14.18981, 2.67430, -0.33201, -1.58700, 0.31751, 0.61508, 2.58239, 3.40374, 4.26826, 2.61505, -0.72824, -5.33428, -3.79821, -4.69657, 8.88646],
    sd: [3.26558, 3.54625, 4.38459, 5.40991, 5.34959, 4.92179, 4.67291, 5.67802, 7.78768, 9.18330, 8.76387, 8.24270, 8.96841, 9.16673, 8.45610, 7.52890, 6.44124, 7.26053, 6.60605, 7.13071, 8.35580, 7.56002, 8.80774, 8.35222, 8.69623, 8.81219, 5.60982],
    w: [1.50632, 0.26217, -0.57187, -0.84945, 0.59227, -1.36298, -1.68046, -1.99236, -1.06622, 0.75863, 0.53548, 0.36291, 2.72984, -0.34082, -0.12697, 0.31832, 0.10754, -0.34152, 0.32260, 0.39959, -0.20704, -0.76413, 0.93275, 0.63093, 0.77313, -1.59935, 0.82498],
    b: -0.52833,
};
