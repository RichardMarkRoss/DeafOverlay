//! Port of the v1 C++ direction path (`DeafOverlay.cpp`: `audio_cb`, the DSP block in the
//! `wWinMain` loop, `add_peak_merge`) so the eval can score the new pipeline against it.
//! Auto gain starts at its 4.5 ceiling: that's where v1 sits after any quiet stretch, which is
//! the state it's in when a sound arrives.

#![allow(clippy::needless_range_loop)] // index loops mirror the C++ line for line

use crate::{Estimator, Source};
use std::f32::consts::PI;

const NB: usize = 6;
const MAX_CH: usize = 8;
const CB_FRAMES: usize = 480; // ~10 ms miniaudio callback
const LOOP_FRAMES: usize = 800; // ~16.7 ms main loop (Sleep(16))
const MAX_AZ: f32 = PI / 2.0;
const ILD_K: f32 = 2.5;
const PEAK_DECAY_HZ: f32 = 1.0;
const TRANSIENT_ALPHA: f32 = 0.20;
const TRANSIENT_HI: f32 = 0.18;
const GAIN: f32 = 1.8 * 0.95; // 1.8 * BASE_SENS_GAIN * gSensitivity(1)
const FLOOR_ATTACK: f32 = 0.0035;
const FLOOR_RELEASE: f32 = 0.0008;
const FLOOR_MULT: f32 = 1.05;
const AUTO_GAIN_RATE: f32 = 0.015;
const TARGET_LEVEL: f32 = 0.24;

#[derive(Clone, Copy, Default)]
struct Peak {
    ang: f32,
    amp: f32,
    life: f32,
}

pub struct Baseline {
    sr: f32,
    nch: usize,
    az: Vec<Option<f32>>, // radians
    lp: Vec<[f32; 6]>,    // lp250, lp2k, lp5k, lp8k, lp12k, tot
    c_lr: f32,
    c_ll: f32,
    c_rr: f32,
    coh: f32,
    chan: Vec<[f32; NB]>,
    floor: Vec<[f32; NB]>,
    auto_gain: f32,
    band_prev: [f32; NB],
    transient: f32,
    peaks: [Peak; 18],
    cb_n: usize,
    loop_n: usize,
    out: Vec<Source>,
}

impl Baseline {
    pub fn new(chan_az: &[Option<f32>], sample_rate: f32) -> Self {
        let nch = chan_az.len().clamp(1, MAX_CH);
        Self {
            sr: sample_rate,
            nch,
            az: chan_az.iter().map(|a| a.map(f32::to_radians)).collect(),
            lp: vec![[0.0; 6]; nch],
            c_lr: 0.0,
            c_ll: 0.0,
            c_rr: 0.0,
            coh: 1.0,
            chan: vec![[0.0; NB]; nch],
            floor: vec![[0.0; NB]; nch],
            auto_gain: 4.5,
            band_prev: [0.0; NB],
            transient: 0.0,
            peaks: [Peak::default(); 18],
            cb_n: 0,
            loop_n: 0,
            out: Vec::new(),
        }
    }

    fn a_fc(&self, fc: f32) -> f32 {
        1.0 - (-2.0 * PI * fc / self.sr).exp()
    }

    fn end_callback(&mut self) {
        self.coh = if self.nch >= 2 {
            (self.c_lr.abs() / (self.c_ll * self.c_rr + 1e-12).sqrt()).clamp(0.0, 1.0)
        } else {
            1.0
        };
        for c in 0..self.nch {
            let l = self.lp[c];
            let mut b = [
                l[0],
                (l[1] - l[0]).max(0.0),
                (l[2] - l[1]).max(0.0),
                (l[3] - l[2]).max(0.0),
                (l[4] - l[3]).max(0.0),
                (l[5] - l[4]).max(0.0),
            ];
            b[0] *= 1.25;
            b[1] *= 1.10;
            for k in 0..NB {
                let v = GAIN * b[k];
                self.chan[c][k] += 0.22 * (v - self.chan[c][k]);
            }
        }
    }

    fn add_peak_merge(&mut self, q: Peak) {
        let mut best = None;
        let mut best_d = f32::MAX;
        for (i, p) in self.peaks.iter().enumerate() {
            if p.life > 0.0 {
                let x = q.ang - p.ang;
                let d = (x - (x / (2.0 * PI)).round() * 2.0 * PI).abs();
                if d < best_d {
                    best_d = d;
                    best = Some(i);
                }
            }
        }
        if let Some(i) = best.filter(|_| best_d < 14f32.to_radians()) {
            let p = &mut self.peaks[i];
            let (sa, sb) = (p.amp, q.amp);
            let sx = p.ang.sin() * sa + q.ang.sin() * sb;
            let sy = -p.ang.cos() * sa - q.ang.cos() * sb;
            p.ang = sx.atan2(-sy);
            p.amp = sa.max(0.70 * sa + 0.55 * sb).clamp(0.0, 1.0);
            p.life = p.life.max(q.life);
            return;
        }
        let mut slot = 0;
        for i in 1..self.peaks.len() {
            if self.peaks[i].amp < self.peaks[slot].amp {
                slot = i;
            }
        }
        self.peaks[slot] = q;
    }

    fn main_step(&mut self, dt: f32) {
        let nch = self.nch;
        let mut e = self.chan.clone();
        let mut tot = 0.0;
        for c in 0..nch {
            for b in 0..NB {
                let v = e[c][b];
                let d = v - self.floor[c][b];
                self.floor[c][b] += if d > 0.0 { FLOOR_ATTACK } else { FLOOR_RELEASE } * d;
                e[c][b] = (v - FLOOR_MULT * self.floor[c][b]).max(0.0);
                tot += e[c][b];
            }
        }
        let err = TARGET_LEVEL - tot.max(0.0).ln_1p();
        self.auto_gain = (self.auto_gain + AUTO_GAIN_RATE * err).clamp(0.40, 4.50);
        for row in &mut e {
            for v in row.iter_mut() {
                *v *= self.auto_gain;
            }
        }

        let mut flux = 0.0;
        for b in 0..NB {
            let bt: f32 = (0..nch).map(|c| e[c][b]).sum();
            let btn = bt.max(0.0).ln_1p();
            flux += (btn - self.band_prev[b]).max(0.0);
            self.band_prev[b] = btn;
        }
        self.transient += TRANSIENT_ALPHA * (flux - self.transient);
        let boost = (self.transient / TRANSIENT_HI).clamp(0.0, 1.0);

        for p in &mut self.peaks {
            if p.life > 0.0 {
                p.life -= dt;
                p.amp *= (-dt * PEAK_DECAY_HZ).exp();
                if p.life <= 0.0 || p.amp < 0.02 {
                    *p = Peak::default();
                }
            }
        }

        let norm = |v: f32| (1.2 * v).tanh();
        if nch >= 6 {
            for b in 0..NB {
                let (mut vx, mut vy, mut sum) = (0.0f32, 0.0f32, 0.0f32);
                for c in 0..nch {
                    let Some(az) = self.az[c] else { continue };
                    let en = e[c][b];
                    if en <= 0.0 {
                        continue;
                    }
                    vx += en * az.sin();
                    vy += en * az.cos();
                    sum += en;
                }
                if sum <= 1e-6 {
                    continue;
                }
                let a = norm(sum);
                if a <= 0.01 {
                    continue;
                }
                let focus = (vx.hypot(vy) / sum).clamp(0.0, 1.0);
                let q = Peak { ang: vx.atan2(vy), amp: a * (0.55 + 0.45 * focus), life: 0.55 + 0.22 * boost };
                self.add_peak_merge(q);
            }
        } else {
            let mut li = 0;
            let mut ri = if nch > 1 { 1 } else { 0 };
            for (c, az) in self.az.iter().enumerate() {
                match az {
                    Some(a) if *a < 0.0 => li = c,
                    Some(a) if *a > 0.0 => ri = c,
                    _ => {}
                }
            }
            let coh_gate = ((self.coh - 0.30) / 0.50).clamp(0.0, 1.0);
            for b in 0..NB {
                let (l, r) = (e[li][b], e[ri][b]);
                let s = l + r;
                if s <= 1e-6 {
                    continue;
                }
                let ild = ((r - l) / (s + 1e-6)).abs();
                let c_dir = ((ild - 0.02) / 0.28).clamp(0.0, 1.0);
                let c_event = (0.25 + 0.75 * boost).clamp(0.25, 1.0);
                let conf = ((0.55 * c_dir + 0.45 * c_event) * (0.45 + 0.55 * coh_gate)).clamp(0.0, 1.0);
                let ang = MAX_AZ * (ILD_K * (r - l) / (s + 1e-6)).tanh();
                let a = norm(l.max(r));
                if a <= 0.005 {
                    continue;
                }
                self.add_peak_merge(Peak { ang, amp: a * (0.62 + 0.38 * conf), life: 0.55 + 0.22 * boost });
            }
        }

        self.out.clear();
        self.out.extend(self.peaks.iter().filter(|p| p.life > 0.0).map(|p| Source {
            azimuth: p.ang.to_degrees(),
            strength: p.amp,
            confidence: 1.0,
        }));
        self.out.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    }
}

impl Estimator for Baseline {
    fn process(&mut self, interleaved: &[f32]) {
        let a: [f32; 5] = [250.0, 2000.0, 5000.0, 8000.0, 12000.0].map(|f| self.a_fc(f));
        let a_coh = self.a_fc(8.0);
        let stride = self.az.len().max(1);
        for frame in interleaved.chunks_exact(stride) {
            for (c, x) in frame.iter().take(self.nch).enumerate() {
                let e = x * x;
                let l = &mut self.lp[c];
                for i in 0..5 {
                    l[i] += a[i] * (e - l[i]);
                }
                l[5] += 0.07 * (e - l[5]);
            }
            if self.nch >= 2 {
                let (l, r) = (frame[0], frame[1]);
                self.c_lr += a_coh * (l * r - self.c_lr);
                self.c_ll += a_coh * (l * l - self.c_ll);
                self.c_rr += a_coh * (r * r - self.c_rr);
            }
            self.cb_n += 1;
            if self.cb_n == CB_FRAMES {
                self.cb_n = 0;
                self.end_callback();
            }
            self.loop_n += 1;
            if self.loop_n == LOOP_FRAMES {
                self.loop_n = 0;
                self.main_step(LOOP_FRAMES as f32 / self.sr);
            }
        }
    }

    fn sources(&self) -> &[Source] {
        &self.out
    }

    fn full_360(&self) -> bool {
        self.nch >= 6
    }
}
