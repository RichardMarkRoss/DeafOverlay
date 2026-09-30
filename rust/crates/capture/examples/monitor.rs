//! Live console view of what the capture + DSP sees (stand-in until the overlay is ported).
//!
//! `cargo run --release -p capture --example monitor [-- --secs N] [--pid PID]`

use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| -> Option<u64> { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)?.parse().ok()) };
    let secs: u64 = arg("--secs").unwrap_or(u64::MAX);
    let pid = arg("--pid").unwrap_or(0) as u32;

    let cap = capture::Capture::start();
    if pid != 0 {
        cap.set_process(pid);
    }
    let t0 = Instant::now();
    let mut last_status = String::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(100));
        let st = cap.status();
        let line = format!(
            "{} | {} ch @ {} Hz | {} | pid {}{}{}",
            st.device,
            st.channels,
            st.sample_rate,
            if st.full_360 { "360°" } else { "front arc" },
            st.pid,
            if st.proc_unsupported { " (per-process unsupported)" } else { "" },
            st.error.as_deref().map(|e| format!(" | error: {e}")).unwrap_or_default(),
        );
        if line != last_status {
            println!("{line}");
            last_status = line;
        }
        let top: Vec<String> = cap
            .sources()
            .iter()
            .take(3)
            .map(|s| format!("{:>5.0}° str {:.2} conf {:.2}", s.azimuth, s.strength, s.confidence))
            .collect();
        if !top.is_empty() {
            println!("{:>6.1}s  {}", t0.elapsed().as_secs_f32(), top.join("  |  "));
        }
    }
}
