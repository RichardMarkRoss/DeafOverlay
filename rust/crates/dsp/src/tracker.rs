//! Turns per-hop histogram peaks into stable sources: nearest-neighbour association, an
//! exponential angle filter, and a confidence that rises on hits and decays on misses.
//! ponytail: alpha filter, no velocity term; add alpha-beta if fast movers lag visibly.

use crate::{ang_dist, wrap180, Source};

const GATE_DEG: f32 = 20.0;
const ALPHA: f32 = 0.35; // angle smoothing per hop (~15 ms to settle)
const STRENGTH_ALPHA: f32 = 0.3;
const CONF_UP: f32 = 0.25;
const CONF_DOWN: f32 = 0.012; // ~0.4 s from full confidence to gone
const MISS_FADE: f32 = 0.97;
const MAX_TRACKS: usize = 6;
const MIN_HITS: u32 = 2; // debounce one-hop blips
const MERGE_DEG: f32 = 8.0;

struct Track {
    az: f32,
    strength: f32,
    conf: f32,
    hits: u32,
    matched: bool,
}

pub(crate) struct Tracker {
    circ: bool,
    tracks: Vec<Track>,
    out: Vec<Source>,
}

impl Tracker {
    pub fn new(circ: bool) -> Self {
        Self { circ, tracks: Vec::new(), out: Vec::new() }
    }

    pub fn sources(&self) -> &[Source] {
        &self.out
    }

    /// `peaks`: (azimuth deg, strength), strongest first.
    pub fn update(&mut self, peaks: &[(f32, f32)]) {
        for t in &mut self.tracks {
            t.matched = false;
        }
        for &(az, s) in peaks {
            let best = self
                .tracks
                .iter_mut()
                .filter(|t| !t.matched)
                .map(|t| (ang_dist(t.az, az, self.circ), t))
                .filter(|(d, _)| *d < GATE_DEG)
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, t)) = best {
                let d = if self.circ { wrap180(az - t.az) } else { az - t.az };
                t.az += ALPHA * d;
                if self.circ {
                    t.az = wrap180(t.az);
                }
                t.strength += STRENGTH_ALPHA * (s - t.strength);
                t.conf = (t.conf + CONF_UP).min(1.0);
                t.hits += 1;
                t.matched = true;
                continue;
            }
            let fresh = Track { az, strength: s, conf: CONF_UP, hits: 1, matched: true };
            if self.tracks.len() < MAX_TRACKS {
                self.tracks.push(fresh);
            } else if let Some(weakest) = self
                .tracks
                .iter_mut()
                .filter(|t| t.conf < CONF_UP)
                .min_by(|a, b| a.conf.total_cmp(&b.conf))
            {
                *weakest = fresh;
            }
        }
        for t in &mut self.tracks {
            if !t.matched {
                t.conf -= CONF_DOWN;
                t.strength *= MISS_FADE;
            }
        }
        self.tracks.retain(|t| t.conf > 0.0);

        // Two tracks that drifted onto the same spot are one source; keep the stronger.
        let score = |t: &Track| t.conf * t.strength;
        let mut i = 0;
        while i < self.tracks.len() {
            let dup = (0..self.tracks.len()).find(|&j| {
                j != i
                    && ang_dist(self.tracks[i].az, self.tracks[j].az, self.circ) < MERGE_DEG
                    && score(&self.tracks[j]) >= score(&self.tracks[i])
            });
            if dup.is_some() {
                self.tracks.swap_remove(i);
            } else {
                i += 1;
            }
        }

        self.out.clear();
        self.out.extend(self.tracks.iter().filter(|t| t.hits >= MIN_HITS).map(|t| Source {
            azimuth: t.az,
            strength: t.strength,
            confidence: t.conf,
        }));
        self.out
            .sort_by(|a, b| (b.strength * b.confidence).total_cmp(&(a.strength * a.confidence)));
    }
}
