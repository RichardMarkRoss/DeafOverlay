//! `cargo run --release -p dsp --example eval`: accuracy table, v1 baseline vs new pipeline.

fn main() {
    println!("{:<22}{:>12}{:>12}", "scenario", "v1 (C++)", "new");
    for s in dsp::eval::report() {
        let fmt = |v: f32| if s.error { format!("{v:.1}°") } else { format!("{:.0}%", v * 100.0) };
        println!("{:<22}{:>12}{:>12}", s.name, fmt(s.baseline), fmt(s.new));
    }
    println!("\nangles: mean of per-angle median error of the strongest source (lower is better)");
    println!("2 sources: share of time both are shown within 10° (higher is better)");
}
