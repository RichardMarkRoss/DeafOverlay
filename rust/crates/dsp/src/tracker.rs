//! Turns per-hop histogram peaks into stable sources: nearest-neighbour association, an
//! exponential angle filter, and a confidence that rises on hits and decays on misses.
//! ponytail: alpha filter, no velocity term; add alpha-beta if fast movers lag visibly.
//!
//! Stereo tracks also accumulate per-band L/R power for the front/back guess ([`crate::frontback`]).

use crate::frontback::{self, FB_BANDS, NF};
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
const FB_DECAY: f32 = 0.97; // per-hop memory of the band powers (~175 ms)
const FB_ALPHA: f32 = 0.2; // per-hop smoothing of P(behind)
const FB_MIN_EVIDENCE: u32 = 8; // hops with usable features before a track may be shown behind
/// Shown behind only above this. High on purpose: a wrong "behind" is worse than a missed one.
const REAR_SHOW: f32 = 0.8;

/// One histogram peak this hop, with the band powers of the bins that voted for it.
pub(crate) struct Obs {
    pub az: f32,
    pub strength: f32,
    pub pl: [f32; FB_BANDS],
    pub pr: [f32; FB_BANDS],
}

struct Track {
    az: f32,
    strength: f32,
    conf: f32,
    hits: u32,
    matched: bool,
    pl: [f32; FB_BANDS],
    pr: [f32; FB_BANDS],
    rear: f32, // smoothed P(behind); starts at 0, so "front" until proven otherwise
    fb_evidence: u32,
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

    pub fn strongest_features(&self) -> Option<[f32; NF]> {
        let t = self.tracks.iter().filter(|t| t.hits >= MIN_HITS).max_by(|a, b| {
            (a.strength * a.conf).total_cmp(&(b.strength * b.conf))
        })?;
        frontback::features(&t.pl, &t.pr)
    }

    /// `peaks`: strongest first. `front_back`: when set, mirror confidently-behind stereo tracks
    /// to the rear using this model.
    pub fn update(&mut self, peaks: &[Obs], front_back: Option<&frontback::Model>) {
        for t in &mut self.tracks {
            t.matched = false;
        }
        for o in peaks {
            let (az, s) = (o.az, o.strength);
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
                for b in 0..FB_BANDS {
                    t.pl[b] = t.pl[b] * FB_DECAY + o.pl[b];
                    t.pr[b] = t.pr[b] * FB_DECAY + o.pr[b];
                }
                if let (Some(m), Some(f)) = (front_back, frontback::features(&t.pl, &t.pr)) {
                    t.rear += FB_ALPHA * (m.p_back(&f) - t.rear);
                    t.fb_evidence += 1;
                }
                continue;
            }
            let fresh = Track {
                az,
                strength: s,
                conf: CONF_UP,
                hits: 1,
                matched: true,
                pl: o.pl,
                pr: o.pr,
                rear: 0.0,
                fb_evidence: 0,
            };
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

        let mirror = front_back.is_some() && !self.circ;
        self.out.clear();
        self.out.extend(self.tracks.iter().filter(|t| t.hits >= MIN_HITS).map(|t| {
            let behind = mirror && t.fb_evidence >= FB_MIN_EVIDENCE && t.rear >= REAR_SHOW;
            Source {
                // Same lateral angle, other side of the head: 30° -> 150°, -30° -> -150°.
                azimuth: if behind { (180.0 - t.az.abs()).copysign(t.az) } else { t.az },
                strength: t.strength,
                confidence: t.conf,
            }
        }));
        self.out
            .sort_by(|a, b| (b.strength * b.confidence).total_cmp(&(a.strength * a.confidence)));
    }
}
