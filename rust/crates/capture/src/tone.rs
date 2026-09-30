//! L/R self-test (v1 `tone_cb` + the self-test state machine in `wWinMain`): a 600 Hz tone hard-left,
//! then hard-right, through the default output. Loopback captures it, so the HUD lights the
//! matching side: a deaf user can *see* that capture works and that left/right isn't reversed.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

const RATE: usize = 48_000;
const LEVEL: f32 = 0.22;
const FREQ: f32 = 600.0;
const BUFFER_HNS: i64 = 500_000; // 50 ms
/// Beeps rather than one steady tone: the DSP votes on onsets and tunes out steady sounds within
/// about half a second, so a 1.5 s held tone would fade off the HUD.
const BEEP_PERIOD_S: f32 = 0.25;
const BEEP_ON_S: f32 = 0.15;

/// (end time in seconds, side) with side -1 = silent, 0 = left, 1 = right.
const TIMELINE: [(f32, i32); 4] = [(0.3, -1), (1.8, 0), (2.1, -1), (3.6, 1)];

pub struct Tone {
    side: Arc<AtomicI32>,
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tone {
    /// Starts the ~3.6 s left-then-right sequence on its own thread.
    pub fn start() -> Self {
        let side = Arc::new(AtomicI32::new(-1));
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, d, st) = (side.clone(), done.clone(), stop.clone());
        let thread = thread::Builder::new()
            .name("self-test tone".into())
            .spawn(move || {
                if let Err(e) = play(&s, &st) {
                    eprintln!("deafoverlay self-test: {e}");
                }
                s.store(-1, Ordering::Relaxed);
                d.store(true, Ordering::Relaxed);
            })
            .expect("spawn tone thread");
        Self { side, done, stop, thread: Some(thread) }
    }

    /// -1 silent, 0 left, 1 right.
    pub fn side(&self) -> i32 {
        self.side.load(Ordering::Relaxed)
    }

    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn play(side: &AtomicI32, stop: &AtomicBool) -> Result<(), Box<dyn std::error::Error>> {
    let _ = wasapi::initialize_mta().ok();
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    let mut client = device.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: BUFFER_HNS };
    client.initialize_client(&fmt, &Direction::Render, &mode)?;
    let event = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;

    let inc = 2.0 * std::f32::consts::PI * FREQ / RATE as f32;
    let mut phase = 0.0f32;
    let mut bytes = Vec::new();
    let mut written = 0usize; // audio time, so beeps stay regular however writes are scheduled
    let side_at = |t: f32| TIMELINE.iter().find(|(end, _)| t < *end).map(|&(_, s)| s);
    let mut started = false;
    while !stop.load(Ordering::Relaxed) {
        let Some(s) = side_at(written as f32 / RATE as f32) else { break };
        side.store(s, Ordering::Relaxed);

        let frames = client.get_available_space_in_frames()? as usize;
        bytes.clear();
        for _ in 0..frames {
            let t = written as f32 / RATE as f32;
            written += 1;
            let s = side_at(t).unwrap_or(-1);
            let on = s >= 0 && t % BEEP_PERIOD_S < BEEP_ON_S;
            let x = if on { LEVEL * phase.sin() } else { 0.0 };
            phase = (phase + inc) % (2.0 * std::f32::consts::PI);
            let (l, r) = match s {
                0 => (x, 0.0),
                1 => (0.0, x),
                _ => (0.0, 0.0),
            };
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        render.write_to_device(frames, &bytes, None)?;
        if !started {
            client.start_stream()?; // the buffer is pre-filled before starting, as WASAPI wants
            started = true;
        }
        event.wait_for_event(1000)?;
    }
    client.stop_stream()?;
    Ok(())
}
