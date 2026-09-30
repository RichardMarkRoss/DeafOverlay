//! Direction-of-arrival DSP for DeafOverlay. Platform-free so accuracy can be tested offline.
//!
//! Per hop: STFT per channel -> per-bin direction vote (stereo: ILD + low-frequency ITD;
//! surround: amplitude vector over speaker azimuths), weighted by onset flux and by
//! direct-power share (stereo) or focus (surround) -> azimuth histogram -> peak picking -> tracker.
//!
//! Azimuth convention everywhere: degrees, 0 = front, + = right, +-180 = behind.

pub mod baseline;
pub mod eval;
pub mod synth;
mod tracker;

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::sync::Arc;

pub const FFT_N: usize = 1024; // 21 ms @ 48 kHz
pub const HOP: usize = 256; // 5.3 ms @ 48 kHz
const F_MIN: f32 = 100.0;
const F_MAX: f32 = 16_000.0;
const ITD_F_MAX: f32 = 750.0; // below this a head-sized ITD (<0.66 ms) can't wrap the phase
const SPEC_ALPHA: f32 = 0.25; // per-hop smoothing of auto/cross spectra (stereo)
const FLOOR_UP: f32 = 0.03; // per-bin noise floor: creeps up (~180 ms)...
const FLOOR_DOWN: f32 = 0.2; // ...drops fast (~25 ms)
const ONSET_K: f32 = 2.0; // a bin is an onset when it rises this far above its floor
const STEADY_W: f32 = 0.1; // vote weight of non-onset energy; onsets get full weight
const FREQ_AVG_DIV: usize = 24; // half-width k/24 bins each side = ~1/6 octave total
const DTR_LO: f32 = 0.3; // stereo bins whose direct share of power is below this don't vote
const DTR_HI: f32 = 0.8; // ... and above this vote fully
const HIST_DECAY: f32 = 0.6; // per-hop histogram memory
const BIN_DEG: f32 = 2.0;
const KERNEL: [f32; 5] = [1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
const PEAK_REL: f32 = 0.15; // peaks must reach this fraction of the tallest
const PEAK_ABS: f32 = 1e-3; // below this the histogram is silence
const PEAK_SNR: f32 = 3.0; // peaks must also clear the histogram's own noise level by this much
const NOISE_UP: f32 = 0.01; // histogram noise level creeps up (~0.5 s)...
const NOISE_DOWN: f32 = 0.2; // ...and drops fast
const PEAK_MIN_SEP_DEG: f32 = 16.0;
const MAX_PEAKS: usize = 4;
const BAND_EDGES_HZ: [f32; 5] = [250.0, 2000.0, 5000.0, 8000.0, 12000.0];
const HEAD_RADIUS_M: f32 = 0.0875;
const SPEED_OF_SOUND: f32 = 343.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    pub azimuth: f32,
    pub strength: f32,
    pub confidence: f32,
}

/// Anything the eval can score: the new [`Analyzer`] and the v1 [`baseline::Baseline`].
pub trait Estimator {
    fn process(&mut self, interleaved: &[f32]);
    /// Strongest first.
    fn sources(&self) -> &[Source];
    fn full_360(&self) -> bool;
}

#[derive(Clone, Copy)]
enum Mode {
    Stereo { l: usize, r: usize },
    Surround,
}

pub struct Analyzer {
    pub swap_lr: bool,
    band_w: Vec<f32>, // per-bin user weight from set_band_gains
    sr: f32,
    nch: usize,
    mode: Mode,
    dirs: Vec<Option<(f32, f32)>>, // per channel (sin, cos) of speaker azimuth; None = LFE
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    ring: Vec<Vec<f32>>, // per channel, last FFT_N samples; oldest at `pos`
    pos: usize,
    pending: usize,
    fft_in: Vec<f32>,
    fft_scratch: Vec<Complex32>,
    spectra: Vec<Vec<Complex32>>,
    k_min: usize,
    k_max: usize,
    k_itd: usize,
    mag_floor: Vec<f32>,
    sxx: Vec<f32>,
    syy: Vec<f32>,
    sxy: Vec<Complex32>,
    cxx: Vec<f32>, // prefix sums of the above, len nb + 1
    cyy: Vec<f32>,
    cxy: Vec<Complex32>,
    votes: Vec<f32>,
    hist: Vec<f32>,
    smooth: Vec<f32>,
    peaks: Vec<(f32, f32)>,
    hist_noise: f32,
    tracker: tracker::Tracker,
}

impl Analyzer {
    /// `chan_az`: speaker azimuth per captured channel in degrees, `None` for LFE.
    /// 6+ channels get full 360°; fewer get the stereo front arc (like v1).
    pub fn new(chan_az: &[Option<f32>], sample_rate: f32) -> Self {
        let nch = chan_az.len().max(1);
        let mode = if nch >= 6 {
            Mode::Surround
        } else {
            let mut l = 0;
            let mut r = if nch > 1 { 1 } else { 0 };
            for (c, az) in chan_az.iter().enumerate() {
                match az {
                    Some(a) if *a < 0.0 => l = c,
                    Some(a) if *a > 0.0 => r = c,
                    _ => {}
                }
            }
            Mode::Stereo { l, r }
        };
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_N);
        let nb = FFT_N / 2 + 1;
        let bin_hz = sample_rate / FFT_N as f32;
        let hist_len = match mode {
            Mode::Stereo { .. } => (180.0 / BIN_DEG) as usize + 1,
            Mode::Surround => (360.0 / BIN_DEG) as usize,
        };
        Self {
            swap_lr: false,
            band_w: vec![1.0; nb],
            sr: sample_rate,
            nch,
            mode,
            dirs: chan_az
                .iter()
                .map(|a| a.map(|d| (d.to_radians().sin(), d.to_radians().cos())))
                .collect(),
            window: (0..FFT_N)
                .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_N as f32).cos())
                .collect(),
            ring: vec![vec![0.0; FFT_N]; nch],
            pos: 0,
            pending: 0,
            fft_in: fft.make_input_vec(),
            fft_scratch: fft.make_scratch_vec(),
            spectra: vec![fft.make_output_vec(); nch],
            fft,
            k_min: (F_MIN / bin_hz).ceil() as usize,
            k_max: ((F_MAX / bin_hz) as usize).min(nb - 1),
            k_itd: (ITD_F_MAX / bin_hz) as usize,
            mag_floor: vec![0.0; nb],
            sxx: vec![0.0; nb],
            syy: vec![0.0; nb],
            sxy: vec![Complex32::new(0.0, 0.0); nb],
            cxx: vec![0.0; nb + 1],
            cyy: vec![0.0; nb + 1],
            cxy: vec![Complex32::new(0.0, 0.0); nb + 1],
            votes: vec![0.0; hist_len],
            hist: vec![0.0; hist_len],
            smooth: vec![0.0; hist_len],
            peaks: Vec::with_capacity(MAX_PEAKS),
            hist_noise: 0.0,
            tracker: tracker::Tracker::new(matches!(mode, Mode::Surround)),
        }
    }

    fn analyze(&mut self) {
        for c in 0..self.nch {
            for i in 0..FFT_N {
                self.fft_in[i] = self.ring[c][(self.pos + i) % FFT_N] * self.window[i];
            }
            // Can't fail: every buffer came from this plan.
            let _ = self
                .fft
                .process_with_scratch(&mut self.fft_in, &mut self.spectra[c], &mut self.fft_scratch);
        }

        if let Mode::Stereo { l, r } = self.mode {
            let (l, r) = if self.swap_lr { (r, l) } else { (l, r) };
            for k in 0..self.sxx.len() {
                let (xl, xr) = (self.spectra[l][k], self.spectra[r][k]);
                self.sxx[k] += SPEC_ALPHA * (xl.norm_sqr() - self.sxx[k]);
                self.syy[k] += SPEC_ALPHA * (xr.norm_sqr() - self.syy[k]);
                let sxy = self.sxy[k];
                self.sxy[k] = sxy + (xr * xl.conj() - sxy) * SPEC_ALPHA;
                self.cxx[k + 1] = self.cxx[k] + self.sxx[k];
                self.cyy[k + 1] = self.cyy[k] + self.syy[k];
                self.cxy[k + 1] = self.cxy[k] + self.sxy[k];
            }
        }

        self.votes.fill(0.0);
        let bin_hz = self.sr / FFT_N as f32;
        for k in self.k_min..=self.k_max {
            let (az, mag, total, gate) = match self.mode {
                Mode::Stereo { l, r } => {
                    // Spectra averaged over ~1/6 octave around k (prefix sums): a single bin's
                    // L/R statistics are far too noisy for the direct/diffuse split below.
                    let h = (k / FREQ_AVG_DIV).max(1);
                    let (lo, hi) = (k - h, (k + h + 1).min(self.sxx.len()));
                    let sxx = self.cxx[hi] - self.cxx[lo];
                    let syy = self.cyy[hi] - self.cyy[lo];
                    let sxy = self.cxy[hi] - self.cxy[lo];
                    let cross = sxy.norm_sqr();
                    // Direct/diffuse split (upmixer model: one panned source + equal-power diffuse
                    // in L and R). The diffuse power is the smaller root of
                    // (Sxx - d)(Syy - d) = |Sxy|^2. Removing it stops ambience dragging ILD to centre.
                    let diffuse =
                        0.5 * ((sxx + syy) - ((sxx - syy).powi(2) + 4.0 * cross).sqrt()).max(0.0);
                    let (pl, pr) = ((sxx - diffuse).max(0.0), (syy - diffuse).max(0.0));
                    // Gate on the direct share of power, not on coherence: coherence also drops for a
                    // hard-panned source whose weak side sits near the noise, which would bias every
                    // estimate toward centre.
                    let dtr = (pl + pr) / (sxx + syy + 1e-20);
                    let gate = ((dtr - DTR_LO) / (DTR_HI - DTR_LO)).clamp(0.0, 1.0);
                    // Constant-power pan law, inverted: phi 0 = hard left, pi/2 = hard right.
                    let phi = pr.sqrt().atan2(pl.sqrt());
                    let mut az = (phi / FRAC_PI_4 - 1.0) * FRAC_PI_2;
                    if k <= self.k_itd {
                        let tau = sxy.arg() / (2.0 * PI * k as f32 * bin_hz);
                        let az_itd = itd_to_azimuth(tau);
                        // ponytail: keep the more lateral cue. Amplitude-panned mixes carry no ITD and
                        // HRTF mixes carry little low-frequency ILD, so each cue only ever under-reads.
                        if az_itd.abs() > az.abs() {
                            az = az_itd;
                        }
                    }
                    let n = (hi - lo) as f32;
                    let total = self.spectra[l][k].norm() + self.spectra[r][k].norm();
                    (az, (pl / n).sqrt() + (pr / n).sqrt(), total, gate)
                }
                Mode::Surround => {
                    let (mut vx, mut vy, mut sum) = (0.0f32, 0.0f32, 0.0f32);
                    for (c, dir) in self.dirs.iter().enumerate() {
                        if let Some((s, co)) = dir {
                            let m = self.spectra[c][k].norm();
                            vx += m * if self.swap_lr { -s } else { *s };
                            vy += m * co;
                            sum += m;
                        }
                    }
                    if sum <= 0.0 {
                        continue;
                    }
                    // Amplitude (velocity) vector: exactly the source direction for pairwise
                    // amplitude panning (VBAP). Its length/sum is the focus (1 = point source).
                    let focus = (vx.hypot(vy) / sum).min(1.0);
                    (vx.atan2(vy), sum, sum, focus)
                }
            };
            // Onset = rise well above the bin's noise floor. Stationary noise almost never does
            // (a Rayleigh magnitude tops 2x its mean ~4% of the time), and a sound's own decaying
            // tail sinks under the floor it raised: a cheap precedence effect against reverb.
            let floor = &mut self.mag_floor[k];
            let onset = (total - ONSET_K * *floor).max(0.0).min(mag);
            *floor += if total > *floor { FLOOR_UP } else { FLOOR_DOWN } * (total - *floor);
            // 1/f: every octave votes equally, so a narrow-band footstep isn't outvoted by the
            // hundreds of high bins a broadband shot covers.
            let w = (onset + STEADY_W * mag) * gate * (self.k_min as f32 / k as f32) * self.band_w[k];
            if w > 0.0 {
                self.splat(az.to_degrees(), w);
            }
        }

        for (h, v) in self.hist.iter_mut().zip(&self.votes) {
            *h = *h * HIST_DECAY + v;
        }
        let circ = matches!(self.mode, Mode::Surround);
        let nb = self.hist.len() as isize;
        let at = |v: &[f32], i: isize| -> f32 {
            if circ {
                v[i.rem_euclid(nb) as usize]
            } else if (0..nb).contains(&i) {
                v[i as usize]
            } else {
                0.0
            }
        };
        for i in 0..nb {
            self.smooth[i as usize] = KERNEL
                .iter()
                .enumerate()
                .map(|(j, kw)| kw * at(&self.hist, i + j as isize - 2))
                .sum();
        }

        self.peaks.clear();
        let s = &self.smooth;
        let max = s.iter().copied().fold(0.0, f32::max);
        // Stationary ambience settles into a steady histogram level; real events tower over it.
        let noise = self.hist_noise;
        self.hist_noise += if max > noise { NOISE_UP } else { NOISE_DOWN } * (max - noise);
        if max > PEAK_ABS.max(PEAK_SNR * noise) {
            let mut cand: Vec<isize> = (0..nb)
                .filter(|&i| {
                    let v = at(s, i);
                    v >= (PEAK_REL * max).max(PEAK_SNR * noise) && v >= at(s, i - 1) && v > at(s, i + 1)
                })
                .collect();
            cand.sort_by(|a, b| at(s, *b).total_cmp(&at(s, *a)));
            for i in cand {
                let (a, b, c) = (at(s, i - 1), at(s, i), at(s, i + 1));
                let den = a - 2.0 * b + c;
                let edge = !circ && (i == 0 || i == nb - 1);
                let d = if den.abs() > 1e-12 && !edge { 0.5 * (a - c) / den } else { 0.0 };
                let pos = (i as f32 + d) * BIN_DEG;
                let az = if circ { wrap180(pos) } else { pos - 90.0 };
                if self
                    .peaks
                    .iter()
                    .all(|p| ang_dist(p.0, az, circ) >= PEAK_MIN_SEP_DEG)
                {
                    self.peaks.push((az, b));
                    if self.peaks.len() == MAX_PEAKS {
                        break;
                    }
                }
            }
        }
        self.tracker.update(&self.peaks);
    }

    /// v1's per-band sliders and game presets, as vote weights over the same six bands:
    /// 0-250 Hz, 250-2k, 2-5k, 5-8k, 8-12k, 12k+. 1.0 everywhere = neutral.
    pub fn set_band_gains(&mut self, gains: [f32; 6]) {
        let bin_hz = self.sr / FFT_N as f32;
        for (k, w) in self.band_w.iter_mut().enumerate() {
            let band = BAND_EDGES_HZ.iter().take_while(|&&e| k as f32 * bin_hz >= e).count();
            *w = gains[band].clamp(0.0, 3.0);
        }
    }

    /// Linear split of one vote between the two nearest histogram bins.
    fn splat(&mut self, az_deg: f32, w: f32) {
        let nb = self.votes.len();
        let (p, circ) = match self.mode {
            Mode::Stereo { .. } => (((az_deg + 90.0) / BIN_DEG).clamp(0.0, (nb - 1) as f32), false),
            Mode::Surround => (az_deg.rem_euclid(360.0) / BIN_DEG, true),
        };
        let i = p.floor() as usize % nb;
        let f = p - p.floor();
        self.votes[i] += w * (1.0 - f);
        let j = if circ { (i + 1) % nb } else { (i + 1).min(nb - 1) };
        self.votes[j] += w * f;
    }
}

impl Estimator for Analyzer {
    /// Interleaved frames, `nch` samples each. Runs one analysis every `HOP` frames.
    fn process(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.nch) {
            for (c, &x) in frame.iter().enumerate() {
                self.ring[c][self.pos] = x;
            }
            self.pos = (self.pos + 1) % FFT_N;
            self.pending += 1;
            if self.pending == HOP {
                self.pending = 0;
                self.analyze();
            }
        }
    }

    fn sources(&self) -> &[Source] {
        self.tracker.sources()
    }

    fn full_360(&self) -> bool {
        matches!(self.mode, Mode::Surround)
    }
}

/// Invert Woodworth's spherical-head ITD, `tau = r/c * (theta + sin theta)`. Radians, signed like `tau`.
fn itd_to_azimuth(tau: f32) -> f32 {
    let x = (tau.abs() * SPEED_OF_SOUND / HEAD_RADIUS_M).min(FRAC_PI_2 + 1.0);
    let mut th = 0.5 * x;
    for _ in 0..4 {
        th -= (th + th.sin() - x) / (1.0 + th.cos());
    }
    th.clamp(0.0, FRAC_PI_2).copysign(tau)
}

pub(crate) fn wrap180(d: f32) -> f32 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

pub(crate) fn ang_dist(a: f32, b: f32, circ: bool) -> f32 {
    if circ {
        wrap180(a - b).abs()
    } else {
        (a - b).abs()
    }
}
