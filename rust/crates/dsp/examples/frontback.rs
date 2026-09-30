//! Train and evaluate the stereo front/back model on measured HRTFs, through the real Analyzer.
//!
//! `cargo run --release -p dsp --example frontback -- <dir of .hrir files>`
//!
//! Files named `subject_*` (CIPIC) are the training heads, scored leave-one-head-out; any other
//! file (e.g. MIT KEMAR) is a fully held-out head from another lab, the stand-in for "a game's HRTF
//! we never saw". Prints the model trained on every head, to paste into `frontback::SHIPPED`.

use dsp::frontback::{Model, NF};
use dsp::synth::{self, Hrir, STEREO};
use dsp::{Analyzer, Estimator};
use std::path::PathBuf;

const SR: f32 = 48_000.0;
const LEN: usize = 19_200; // 0.4 s: one burst at 0 and one at 0.25 s
const SOURCES_PER_DIR: u64 = 3;
const PANNED_PER_HEAD: u64 = 80;
const SHOW: f32 = 0.8; // must match tracker::REAR_SHOW

type Sample = ([f32; NF], f32);

fn features(sig: &[f32]) -> Option<[f32; NF]> {
    let mut a = Analyzer::new(&STEREO, SR);
    a.process(sig);
    a.front_back_features()
}

/// HRTF renders labelled front (0) / behind (1), plus amplitude-panned stereo labelled front:
/// with no HRTF cues at all, the answer must be "not behind".
fn samples(dirs: &[Hrir], seed: u64) -> (Vec<Sample>, Vec<Sample>) {
    let mut hrtf = Vec::new();
    for (i, h) in dirs.iter().enumerate() {
        let d = h.azimuth.abs();
        // Median plane has no interaural cue to read; ±90° is neither front nor back.
        if !(10.0..=170.0).contains(&d) || (d - 90.0).abs() < 12.0 {
            continue;
        }
        for s in 0..SOURCES_PER_DIR {
            let mono = synth::colored_bursts(LEN, SR, seed * 100_000 + i as u64 * 10 + s);
            if let Some(f) = features(&synth::render_hrir(&mono, h)) {
                hrtf.push((f, (d > 90.0) as u8 as f32));
            }
        }
    }
    let panned = (0..PANNED_PER_HEAD)
        .filter_map(|k| {
            let az = -80.0 + 160.0 * ((k * 37 + seed) % PANNED_PER_HEAD) as f32 / PANNED_PER_HEAD as f32;
            let mono = synth::colored_bursts(LEN, SR, seed * 7_919 + k);
            features(&synth::pan_stereo(&mono, az)).map(|f| (f, 0.0))
        })
        .collect();
    (hrtf, panned)
}

fn train(sets: &[&(String, Vec<Sample>, Vec<Sample>)]) -> Model {
    let all: Vec<&Sample> = sets.iter().flat_map(|s| s.1.iter().chain(&s.2)).collect();
    let x: Vec<[f32; NF]> = all.iter().map(|s| s.0).collect();
    let y: Vec<f32> = all.iter().map(|s| s.1).collect();
    Model::train(&x, &y, 1e-3, 1500)
}

/// Accuracy, plus what the HUD would actually show at the display threshold.
fn score(name: &str, m: &Model, hrtf: &[Sample], panned: &[Sample]) -> (f32, f32, f32) {
    let p: Vec<(f32, f32)> = hrtf.iter().map(|(f, y)| (m.p_back(f), *y)).collect();
    let acc = p.iter().filter(|(p, y)| (*p > 0.5) == (*y > 0.5)).count() as f32 / p.len() as f32;
    let shown: Vec<&(f32, f32)> = p.iter().filter(|(p, _)| *p >= SHOW).collect();
    let precision = shown.iter().filter(|(_, y)| *y > 0.5).count() as f32 / shown.len().max(1) as f32;
    let behind = p.iter().filter(|(_, y)| *y > 0.5).count().max(1) as f32;
    let recall = shown.iter().filter(|(_, y)| *y > 0.5).count() as f32 / behind;
    let front = p.iter().filter(|(_, y)| *y < 0.5).count().max(1) as f32;
    let false_front = shown.iter().filter(|(_, y)| *y < 0.5).count() as f32 / front;
    let false_panned = panned.iter().filter(|(f, _)| m.p_back(f) >= SHOW).count() as f32 / panned.len().max(1) as f32;
    println!(
        "{name:<24} acc {:>5.1}%   shown-behind: right {:>5.1}%, catches {:>5.1}% of rear sounds, \
         flags {:>4.1}% of front sounds, {:>4.1}% of panned stereo",
        acc * 100.0,
        precision * 100.0,
        recall * 100.0,
        false_front * 100.0,
        false_panned * 100.0
    );
    (acc, precision, false_panned)
}

fn analyzer(m: &Model) -> Analyzer {
    let mut a = Analyzer::new(&STEREO, SR);
    a.front_back = true;
    a.set_front_back_model(m.clone());
    a
}

/// The real thing over time: Analyzer + tracker with the feature on, scored every 512 frames
/// (~10 ms) over the settled second half. Returns (shown-behind correct %, rear time shown
/// behind %, front time shown behind %).
fn live(dirs: &[Hrir], m: &Model, ambience: bool) -> (f32, f32, f32) {
    let (mut shown, mut right, mut rear_t, mut rear_hit, mut front_t, mut front_hit) = (0, 0, 0, 0, 0, 0);
    let amb = synth::diffuse_stereo(48_000, 99);
    for (i, h) in dirs.iter().enumerate().step_by(2) {
        let d = h.azimuth.abs();
        if !(10.0..=170.0).contains(&d) || (d - 90.0).abs() < 12.0 {
            continue;
        }
        let mono = synth::colored_bursts(48_000, SR, 5_000 + i as u64);
        let mut sig = synth::render_hrir(&mono, h);
        if ambience {
            sig = synth::mix(&sig, &amb);
        }
        let mut a = analyzer(m);
        for (j, chunk) in sig.chunks(1024).enumerate() {
            a.process(chunk);
            let Some(s) = a.sources().first().filter(|_| j >= 47) else { continue };
            let (truth, behind) = (d > 90.0, s.azimuth.abs() > 90.0);
            shown += behind as u32;
            right += (behind && truth) as u32;
            if truth {
                rear_t += 1;
                rear_hit += behind as u32;
            } else {
                front_t += 1;
                front_hit += behind as u32;
            }
        }
    }
    let pct = |a: u32, b: u32| a as f32 / b.max(1) as f32 * 100.0;
    (pct(right, shown), pct(rear_hit, rear_t), pct(front_hit, front_t))
}

/// % of the time plain amplitude-panned stereo gets shown behind (should be ~0).
fn panned_live(m: &Model) -> f32 {
    let (mut n, mut behind) = (0, 0);
    for k in 0..17u64 {
        let sig = synth::pan_stereo(&synth::colored_bursts(48_000, SR, 900 + k), -80.0 + 10.0 * k as f32);
        let mut a = analyzer(m);
        for (j, chunk) in sig.chunks(1024).enumerate() {
            a.process(chunk);
            if let Some(s) = a.sources().first().filter(|_| j >= 47) {
                n += 1;
                behind += (s.azimuth.abs() > 90.0) as u32;
            }
        }
    }
    behind as f32 / n.max(1) as f32 * 100.0
}

fn main() {
    let dir =PathBuf::from(std::env::args().nth(1).expect("usage: frontback <dir of .hrir files>"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "hrir"))
        .collect();
    files.sort();

    // One thread per head: rendering + analysis dominates the run time.
    let sets: Vec<(String, Vec<Sample>, Vec<Sample>)> = std::thread::scope(|s| {
        let jobs: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                s.spawn(move || {
                    let name = f.file_stem().unwrap().to_string_lossy().into_owned();
                    let dirs = synth::load_hrir(f).expect("load .hrir");
                    let (hrtf, panned) = samples(&dirs, i as u64 + 1);
                    (name, hrtf, panned)
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    });
    for (n, h, p) in &sets {
        println!("{n:<24} {} HRTF samples, {} panned", h.len(), p.len());
    }

    let (train_heads, test_heads): (Vec<_>, Vec<_>) = sets.iter().partition(|s| s.0.starts_with("subject"));
    println!("\nleave-one-head-out over {} CIPIC heads:", train_heads.len());
    let mut accs = Vec::new();
    for held in &train_heads {
        let others: Vec<_> = train_heads.iter().copied().filter(|s| s.0 != held.0).collect();
        accs.push(score(&held.0, &train(&others), &held.1, &held.2).0);
    }
    println!("mean accuracy {:.1}%", accs.iter().sum::<f32>() / accs.len() as f32 * 100.0);

    println!("\ntrained on every CIPIC head, tested on heads from other labs:");
    let m = train(&train_heads);
    for t in &test_heads {
        score(&t.0, &m, &t.1, &t.2);
    }

    println!("\nend to end (Analyzer + tracker, feature on, model never saw these heads), over time:");
    for t in &test_heads {
        let path = files.iter().find(|f| f.file_stem().is_some_and(|s| s.to_string_lossy() == t.0)).unwrap();
        let dirs = synth::load_hrir(path).expect("load .hrir");
        for (label, amb) in [("clean", false), ("+ ambience", true)] {
            let (right, rear, front) = live(&dirs, &m, amb);
            println!(
                "{:<24} {label:<11} shown-behind right {right:>5.1}%, rear sounds shown behind {rear:>5.1}% \
                 of the time, front sounds {front:>4.1}%",
                t.0
            );
        }
    }
    println!("panned stereo (no HRTF)   shown behind {:.1}% of the time", panned_live(&m));

    println!("\nmodel trained on all {} heads (paste into frontback::SHIPPED):", sets.len());
    println!("{}", train(&sets.iter().collect::<Vec<_>>()).to_rust());
}
