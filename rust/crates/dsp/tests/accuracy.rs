use dsp::eval::{self, make_new};
use dsp::{synth, Analyzer, Estimator};

#[test]
fn stereo_pan_under_10_deg() {
    let e = eval::stereo_pan(make_new);
    assert!(e < 10.0, "{e}");
}

#[test]
fn surround_under_15_deg() {
    let e = eval::surround(make_new);
    assert!(e < 15.0, "{e}");
}

#[test]
fn two_simultaneous_sources_resolved() {
    let s = eval::two_sources_stereo(make_new);
    let m = eval::two_sources_surround(make_new);
    assert!(s >= 0.8 && m >= 0.8, "stereo {s}, 7.1 {m}");
}

#[test]
fn beats_v1_everywhere() {
    for s in eval::report() {
        let better = if s.error { s.new < s.baseline } else { s.new > s.baseline };
        assert!(better, "{}: new {} vs v1 {}", s.name, s.new, s.baseline);
    }
}

#[test]
fn silence_has_no_sources() {
    let mut a = Analyzer::new(&synth::STEREO, eval::SR);
    a.process(&vec![0.0; 48_000 * 2]);
    assert!(a.sources().is_empty());
}

#[test]
fn band_gains_weight_their_band() {
    // A footstep-band source loses most of its strength when the bands it lives in are muted.
    let sig = synth::pan_stereo(&synth::bursts(48_000, eval::SR, 3, synth::Band::Low), -40.0);
    let strength = |gains| {
        let mut a = Analyzer::new(&synth::STEREO, eval::SR);
        a.set_band_gains(gains);
        a.process(&sig[..sig.len() - 2 * 9_000]); // stop inside a burst, before it fades
        a.sources().first().map_or(0.0, |s| s.strength)
    };
    let full = strength([1.0; 6]);
    let muted = strength([0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    assert!(full > 0.0 && muted < 0.2 * full, "full {full}, muted {muted}");
}
