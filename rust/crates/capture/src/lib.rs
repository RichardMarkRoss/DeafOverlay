//! WASAPI loopback capture feeding [`dsp::Analyzer`]. Windows only.
//!
//! One thread captures and analyzes; the UI polls [`Capture::sources`] / [`Capture::status`].
//! ponytail: capture and DSP share a thread because a hop costs microseconds against a 100 ms
//! WASAPI buffer. Split them with an SPSC ring if the DSP ever grows (e.g. an HRTF classifier).
//!
//! Mirrors v1 (`audio_open_loopback` / `audio_set_process`): system mix by default, optionally
//! one process tree (Alt+G), falling back to the system mix if per-process loopback fails.

mod layout;
mod tone;
pub use layout::channel_azimuths;
pub use tone::Tone;

use dsp::{Analyzer, Estimator, Source};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wasapi::{AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const BUFFER_HNS: i64 = 1_000_000; // 100 ms WASAPI buffer
const WAIT_MS: u32 = 100;
const DEVICE_CHECK: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// True once a stream is open; false while (re)opening or after an error.
    pub running: bool,
    pub channels: usize,
    pub sample_rate: u32,
    /// 5.1/7.1: real 360°. Stereo: front arc only.
    pub full_360: bool,
    pub device: String,
    /// 0 = whole system mix.
    pub pid: u32,
    /// Per-process loopback failed (needs Windows 10 build 20348+); fell back to the system mix.
    pub proc_unsupported: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct Shared {
    sources: Vec<Source>,
    status: Status,
}

enum Cmd {
    SetProcess(u32),
    SwapLr(bool),
    BandGains([f32; 6]),
    FrontBack(bool),
    Stop,
}

/// User DSP settings, kept across stream reopens (device change, Alt+G).
#[derive(Clone, Copy)]
struct DspSettings {
    swap_lr: bool,
    gains: [f32; 6],
    front_back: bool,
}

impl DspSettings {
    /// Take a settings command; false if `cmd` is something else.
    fn update(&mut self, cmd: &Cmd) -> bool {
        match *cmd {
            Cmd::SwapLr(on) => self.swap_lr = on,
            Cmd::BandGains(g) => self.gains = g,
            Cmd::FrontBack(on) => self.front_back = on,
            _ => return false,
        }
        true
    }

    fn apply(&self, a: &mut Analyzer) {
        a.swap_lr = self.swap_lr;
        a.set_band_gains(self.gains);
        a.front_back = self.front_back;
    }
}

enum Exit {
    Stop,
    Reopen,
}

pub struct Capture {
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Starts capturing the system mix on a background thread.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn(move || worker(rx, s))
            .expect("spawn capture thread");
        Self { tx, shared, thread: Some(thread) }
    }

    /// Latest tracked sources, strongest first.
    pub fn sources(&self) -> Vec<Source> {
        lock(&self.shared).sources.clone()
    }

    pub fn status(&self) -> Status {
        lock(&self.shared).status.clone()
    }

    /// Capture only this process tree (a game), or 0 for the whole system mix.
    pub fn set_process(&self, pid: u32) {
        let _ = self.tx.send(Cmd::SetProcess(pid));
    }

    pub fn set_swap_lr(&self, on: bool) {
        let _ = self.tx.send(Cmd::SwapLr(on));
    }

    /// Stereo only: guess front/back for HRTF ("headphone 3D") games. See [`dsp::frontback`].
    pub fn set_front_back(&self, on: bool) {
        let _ = self.tx.send(Cmd::FrontBack(on));
    }

    /// See [`dsp::Analyzer::set_band_gains`].
    pub fn set_band_gains(&self, gains: [f32; 6]) {
        let _ = self.tx.send(Cmd::BandGains(gains));
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // A panic elsewhere can't leave Shared half-written in a way that matters; keep going.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Stream {
    client: AudioClient,
    capture: wasapi::AudioCaptureClient,
    event: wasapi::Handle,
    block_align: usize,
    channels: usize,
    sample_rate: u32,
    azimuths: Vec<Option<f32>>,
    device: String,
    device_id: Option<String>, // system-wide only: reopen when the default device changes
}

fn worker(rx: Receiver<Cmd>, shared: Arc<Mutex<Shared>>) {
    let _ = wasapi::initialize_mta().ok();
    let mut pid = 0u32;
    let mut dsp = DspSettings { swap_lr: false, gains: [1.0; 6], front_back: false };
    loop {
        let opened = open(pid);
        let stream = match opened {
            Ok(s) => s,
            Err(e) if pid != 0 => {
                // Same fallback as v1: per-process failed, go back to the whole mix.
                lock(&shared).status.proc_unsupported = true;
                log_err("per-process loopback", &e);
                pid = 0;
                continue;
            }
            Err(e) => {
                {
                    let mut sh = lock(&shared);
                    sh.status.running = false;
                    sh.status.error = Some(e.to_string());
                    sh.sources.clear();
                }
                match rx.recv_timeout(RETRY) {
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(Cmd::SetProcess(p)) => pid = p,
                    Ok(cmd) => {
                        dsp.update(&cmd);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
                continue;
            }
        };
        let mut analyzer = Analyzer::new(&stream.azimuths, stream.sample_rate as f32);
        dsp.apply(&mut analyzer);
        {
            let mut sh = lock(&shared);
            sh.status = Status {
                running: true,
                channels: stream.channels,
                sample_rate: stream.sample_rate,
                full_360: analyzer.full_360(),
                device: stream.device.clone(),
                pid,
                proc_unsupported: sh.status.proc_unsupported,
                error: None,
            };
        }
        match run(&stream, &mut analyzer, &rx, &shared, &mut pid, &mut dsp) {
            Ok(Exit::Stop) => return,
            Ok(Exit::Reopen) => {}
            Err(e) => {
                // Device invalidated (unplugged, format/speaker-config change): reopen shortly.
                log_err("capture", &e);
                lock(&shared).status.running = false;
                thread::sleep(Duration::from_millis(200));
            }
        }
        let _ = stream.client.stop_stream();
    }
}

fn run(
    st: &Stream,
    analyzer: &mut Analyzer,
    rx: &Receiver<Cmd>,
    shared: &Mutex<Shared>,
    pid: &mut u32,
    dsp: &mut DspSettings,
) -> Res<Exit> {
    let mut bytes = Vec::new();
    let mut samples = Vec::new();
    let mut last_check = Instant::now();
    st.client.start_stream()?;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::SetProcess(p)) if p != *pid => {
                    *pid = p;
                    return Ok(Exit::Reopen);
                }
                Ok(Cmd::SetProcess(_)) => {}
                Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => return Ok(Exit::Stop),
                Ok(cmd) => {
                    if dsp.update(&cmd) {
                        dsp.apply(analyzer);
                    }
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        match st.event.wait_for_event(WAIT_MS) {
            Ok(()) => {}
            // Loopback delivers no packets while nothing plays. Feed the gap as silence so tracked
            // sources fade instead of freezing on screen.
            Err(wasapi::WasapiError::EventTimeout) => {
                let frames = st.sample_rate as usize * WAIT_MS as usize / 1000;
                samples.clear();
                samples.resize(frames * st.channels, 0.0);
                analyzer.process(&samples);
            }
            Err(e) => return Err(e.into()),
        }

        while let Some(n) = st.capture.get_next_packet_size()?.filter(|n| *n > 0) {
            bytes.resize(n as usize * st.block_align, 0);
            let (frames, info) = st.capture.read_from_device(&mut bytes)?;
            let len = frames as usize * st.block_align;
            samples.clear();
            if info.flags.silent {
                samples.resize(len / 4, 0.0);
            } else {
                samples.extend(
                    bytes[..len].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
            }
            analyzer.process(&samples);
        }

        {
            let mut sh = lock(shared);
            sh.sources.clear();
            sh.sources.extend_from_slice(analyzer.sources());
        }

        // Loopback on the old device keeps "working" silently after the user switches output, so
        // poll the default device instead of waiting for an error that never comes.
        if st.device_id.is_some() && last_check.elapsed() >= DEVICE_CHECK {
            last_check = Instant::now();
            let now = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?.get_id()?;
            if st.device_id.as_deref() != Some(now.as_str()) {
                return Ok(Exit::Reopen);
            }
        }
    }
}

/// Open a float loopback stream in the default render device's native layout
/// (like v1's `capture.channels = 0`), scoped to `pid`'s process tree when `pid != 0`.
fn open(pid: u32) -> Res<Stream> {
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    let client = device.get_iaudioclient()?;
    let mix = client.get_mixformat()?;
    let (rate, ch, mask) = (mix.get_samplespersec(), mix.get_nchannels() as usize, mix.get_dwchannelmask());
    let native = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, ch, Some(mask));
    let (mut client, fmt, buffer, device_id) = if pid == 0 {
        (client, native, BUFFER_HNS, Some(device.get_id()?))
    } else {
        // The process-loopback client has no mix format of its own; ask for the endpoint's layout
        // and let WASAPI convert. ponytail: stereo fallback only if that layout is refused.
        let mut proc_client = AudioClient::new_application_loopback_client(pid, true)?;
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
        if proc_client.initialize_client(&native, &Direction::Capture, &mode).is_ok() {
            return finish(proc_client, native, &device, None);
        }
        let stereo = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, 2, None);
        proc_client = AudioClient::new_application_loopback_client(pid, true)?;
        (proc_client, stereo, 0, None)
    };
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: buffer };
    client.initialize_client(&fmt, &Direction::Capture, &mode)?;
    finish(client, fmt, &device, device_id)
}

fn finish(client: AudioClient, fmt: WaveFormat, device: &wasapi::Device, device_id: Option<String>) -> Res<Stream> {
    let channels = fmt.get_nchannels() as usize;
    Ok(Stream {
        event: client.set_get_eventhandle()?,
        capture: client.get_audiocaptureclient()?,
        client,
        block_align: fmt.get_blockalign() as usize,
        channels,
        sample_rate: fmt.get_samplespersec(),
        azimuths: channel_azimuths(fmt.get_dwchannelmask(), channels),
        device: device.get_friendlyname().unwrap_or_default(),
        device_id,
    })
}

fn log_err(what: &str, e: &dyn std::fmt::Display) {
    eprintln!("deafoverlay {what}: {e}");
}
