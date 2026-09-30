//! Accuracy scenarios, shared by `cargo test` and `cargo run --example eval`.

use crate::synth::{self, Band, STEREO, SURROUND_51, SURROUND_71};
use crate::{ang_dist, baseline::Baseline, Analyzer, Estimator};

pub const SR: f32 = 48_000.0;
const LEN: usize = 48_000; // 1 s per trial
const STEP: usize = 256; // frames between scoring samples
const HIT_DEG: f32 = 10.0;

pub type Make = fn(&[Option<f32>]) -> Box<dyn Estimator>;
type Scenario = (&'static str, fn(Make) -> f32);

pub fn make_new(layout: &[Option<f32>]) -> Box<dyn Estimator> {
    Box::new(Analyzer::new(layout, SR))
}

pub fn make_baseline(layout: &[Option<f32>]) -> Box<dyn Estimator> {
    Box::new(Baseline::new(layout, SR))
}

/// Feed `sig` in STEP chunks; call `score` after each chunk in the second half (settled).
fn run(est: &mut dyn Estimator, sig: &[f32], nch: usize, mut score: impl FnMut(&dyn Estimator)) {
    let frames = sig.len() / nch;
    let mut f = 0;
    while f + STEP <= frames {
        est.process(&sig[f * nch..(f + STEP) * nch]);
        f += STEP;
        if f > frames / 2 {
            score(est);
        }
    }
}

/// Median absolute error of the strongest source. No source at all counts as the worst miss.
pub fn median_error(est: &mut dyn Estimator, sig: &[f32], nch: usize, truth: f32) -> f32 {
    let circ = est.full_360();
    let miss = if circ { 180.0 } else { 90.0 };
    let mut errs = Vec::new();
    run(est, sig, nch, |e| {
        errs.push(e.sources().first().map_or(miss, |s| ang_dist(s.azimuth, truth, circ)))
    });
    errs.sort_by(f32::total_cmp);
    errs[errs.len() / 2]
}

/// Fraction of samples where every truth has one of the top-4 sources within HIT_DEG.
pub fn resolved_fraction(est: &mut dyn Estimator, sig: &[f32], nch: usize, truths: &[f32]) -> f32 {
    let circ = est.full_360();
    let (mut hit, mut n) = (0, 0);
    run(est, sig, nch, |e| {
        n += 1;
        let top = &e.sources()[..e.sources().len().min(4)];
        if truths.iter().all(|t| top.iter().any(|s| ang_dist(s.azimuth, *t, circ) < HIT_DEG)) {
            hit += 1;
        }
    });
    hit as f32 / n as f32
}

fn mean_error(make: Make, layout: &[Option<f32>], angles: impl Iterator<Item = f32>, render: impl Fn(f32) -> Vec<f32>) -> f32 {
    let errs: Vec<f32> = angles
        .map(|az| median_error(make(layout).as_mut(), &render(az), layout.len(), az))
        .collect();
    errs.iter().sum::<f32>() / errs.len() as f32
}

fn front_arc() -> impl Iterator<Item = f32> {
    (-4..=4).map(|i| i as f32 * 20.0)
}

/// Amplitude-panned stereo bursts across the front arc. Mean of per-angle median error, degrees.
pub fn stereo_pan(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    mean_error(make, &STEREO, front_arc(), |az| synth::pan_stereo(&mono, az))
}

/// Panned with a linear law the analyzer doesn't assume: how much the pan-law guess costs.
pub fn stereo_linear_law(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    mean_error(make, &STEREO, front_arc(), |az| synth::pan_stereo_linear(&mono, az))
}

/// Same, buried in equal-level diffuse L/R noise.
pub fn stereo_diffuse(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    let amb = synth::diffuse_stereo(LEN, 99);
    mean_error(make, &STEREO, front_arc(), |az| synth::mix(&synth::pan_stereo(&mono, az), &amb))
}

/// HRTF-style stereo (ITD + head shadow), as rendered by games with headphone spatializers.
pub fn binaural(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    mean_error(make, &STEREO, front_arc(), |az| synth::binaural(&mono, az, SR))
}

/// 7.1 VBAP all the way round.
pub fn surround(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    mean_error(make, &SURROUND_71, (-9..9).map(|i| i as f32 * 20.0), |az| {
        synth::vbap(&mono, az, &SURROUND_71)
    })
}

/// 5.1 VBAP all the way round (no side speakers: 110° to 110° across the back).
pub fn surround_51(make: Make) -> f32 {
    let mono = synth::bursts(LEN, SR, 1, Band::Full);
    mean_error(make, &SURROUND_51, (-9..9).map(|i| i as f32 * 20.0), |az| {
        synth::vbap(&mono, az, &SURROUND_51)
    })
}

/// Footstep-band source + gunshot-band source at once, stereo. Fraction resolved (1 = always).
pub fn two_sources_stereo(make: Make) -> f32 {
    let lo = synth::pan_stereo(&synth::bursts(LEN, SR, 3, Band::Low), -45.0);
    let hi = synth::pan_stereo(&synth::bursts(LEN, SR, 7, Band::High), 40.0);
    resolved_fraction(make(&STEREO).as_mut(), &synth::mix(&lo, &hi), 2, &[-45.0, 40.0])
}

/// Same idea on 7.1, one source behind-left and one front-right.
pub fn two_sources_surround(make: Make) -> f32 {
    let lo = synth::vbap(&synth::bursts(LEN, SR, 3, Band::Low), -120.0, &SURROUND_71);
    let hi = synth::vbap(&synth::bursts(LEN, SR, 7, Band::High), 60.0, &SURROUND_71);
    resolved_fraction(make(&SURROUND_71).as_mut(), &synth::mix(&lo, &hi), 8, &[-120.0, 60.0])
}

pub struct Score {
    pub name: &'static str,
    pub new: f32,
    pub baseline: f32,
    /// true: lower is better (degrees). false: higher is better (fraction resolved).
    pub error: bool,
}

pub fn report() -> Vec<Score> {
    let err: [Scenario; 6] = [
        ("stereo pan", stereo_pan),
        ("stereo linear law", stereo_linear_law),
        ("stereo + diffuse", stereo_diffuse),
        ("binaural (HRTF-ish)", binaural),
        ("5.1 surround 360", surround_51),
        ("7.1 surround 360", surround),
    ];
    let res: [Scenario; 2] =
        [("2 sources stereo", two_sources_stereo), ("2 sources 7.1", two_sources_surround)];
    err.iter()
        .map(|(n, f)| Score { name: n, new: f(make_new), baseline: f(make_baseline), error: true })
        .chain(res.iter().map(|(n, f)| Score { name: n, new: f(make_new), baseline: f(make_baseline), error: false }))
        .collect()
}
