//! Settings file, same place and format as v1 (`save_settings_file` / `load_settings_file`):
//! `%APPDATA%\DeafOverlay\settings.json`, flat keys, so v1 and this build share one file.
//! v1's keys keep their meaning (`sphereScale` is the HUD size). This build adds `hudStyle`,
//! `palette`, `opacity`, `radarCrosshair` and `frontBack`, which v1 ignores. It no longer writes
//! v1's `ringOffsetY` / `spherePosX` / `spherePosY` (no style uses them), so v1 falls back to its
//! defaults for those. Loaded values are clamped; missing or malformed keys keep their defaults.

use crate::hud::{HudStyle, PaletteKind};
use crate::ui::{Preset, Settings};
use std::path::PathBuf;

pub fn path() -> Option<PathBuf> {
    // ponytail: %APPDATA% is FOLDERID_RoamingAppData for every normal profile; no shell API needed.
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("DeafOverlay").join("settings.json"))
}

/// Settings plus the saved window position (absent on first run).
pub fn load() -> (Settings, Option<(i32, i32)>) {
    match path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => from_json(&s),
        None => (Settings::default(), None),
    }
}

/// Write via a temp file + rename, so a crash mid-save can't leave a truncated settings file.
pub fn save(s: &Settings, win: (i32, i32)) -> std::io::Result<()> {
    let p = path().ok_or(std::io::ErrorKind::NotFound)?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, to_json(s, win))?;
    std::fs::rename(tmp, p)
}

const PRESETS: [Preset; 4] = [Preset::None, Preset::Cs2, Preset::R6s, Preset::Cod];
const STYLES: [HudStyle; 3] = [HudStyle::Orbit, HudStyle::Radar, HudStyle::Tape];
const PALETTES: [PaletteKind; 3] = [PaletteKind::Brand, PaletteKind::ColorBlind, PaletteKind::HighContrast];

fn index<T: PartialEq>(all: &[T], v: T) -> usize {
    all.iter().position(|x| *x == v).unwrap_or(0)
}

pub fn to_json(s: &Settings, win: (i32, i32)) -> String {
    let h = &s.hud;
    let bands: Vec<String> = s.band_gain.iter().map(|g| g.to_string()).collect();
    format!(
        "{{\n  \"sphereScale\": {},\n  \"opacity\": {},\n  \"hudStyle\": {},\n  \"radarCrosshair\": {},\n  \
         \"palette\": {},\n  \"sensitivity\": {},\n  \"swapLR\": {},\n  \"frontBack\": {},\n  \"winX\": {},\n  \
         \"winY\": {},\n  \"preset\": {},\n  \"bandGain\": [{}]\n}}\n",
        h.scale,
        h.opacity,
        index(&STYLES, h.style),
        h.crosshair as i32,
        index(&PALETTES, h.palette),
        s.sensitivity,
        s.swap_lr as i32,
        s.front_back as i32,
        win.0,
        win.1,
        index(&PRESETS, s.preset),
        bands.join(", ")
    )
}

pub fn from_json(text: &str) -> (Settings, Option<(i32, i32)>) {
    let mut s = Settings::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (s, None);
    };
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).map(|x| x as f32);
    let pick = |k: &str, n: usize| num(k).map(|x| x as usize).filter(|i| *i < n);
    if let Some(x) = num("sphereScale") {
        s.hud.scale = x.clamp(0.6, 1.6);
    }
    if let Some(x) = num("opacity") {
        s.hud.opacity = x.clamp(0.3, 1.0);
    }
    if let Some(i) = pick("hudStyle", STYLES.len()) {
        s.hud.style = STYLES[i];
    }
    if let Some(x) = num("radarCrosshair") {
        s.hud.crosshair = x >= 0.5;
    }
    if let Some(i) = pick("palette", PALETTES.len()) {
        s.hud.palette = PALETTES[i];
    }
    if let Some(x) = num("sensitivity") {
        s.sensitivity = x.clamp(0.5, 2.0);
    }
    if let Some(x) = num("swapLR") {
        s.swap_lr = x >= 0.5;
    }
    if let Some(x) = num("frontBack") {
        s.front_back = x >= 0.5;
    }
    s.preset = PRESETS[pick("preset", PRESETS.len()).unwrap_or(0)];
    if let Some(arr) = v.get("bandGain").and_then(serde_json::Value::as_array) {
        for (g, x) in s.band_gain.iter_mut().zip(arr) {
            if let Some(x) = x.as_f64() {
                *g = (x as f32).clamp(0.0, 3.0);
            }
        }
    }
    let win = num("winX").zip(num("winY")).map(|(x, y)| (x as i32, y as i32));
    (s, win)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut s = Settings::default();
        s.apply_preset(Preset::R6s);
        s.swap_lr = true;
        s.front_back = true;
        s.hud.style = HudStyle::Tape;
        s.hud.palette = PaletteKind::ColorBlind;
        s.hud.crosshair = true;
        s.hud.scale = 1.25;
        s.hud.opacity = 0.7;
        assert_eq!(from_json(&to_json(&s, (1325, -12))), (s, Some((1325, -12))));
    }

    #[test]
    fn reads_a_v1_file_and_clamps_it() {
        // What v1 writes (C++ ostream float formatting), with a few values out of range.
        let v1 = "{\n  \"sphereScale\": 9,\n  \"ringOffsetY\": 0.66,\n  \"sensitivity\": 1.2,\n  \
                  \"swapLR\": 1,\n  \"spherePosX\": 0.5,\n  \"spherePosY\": 0.47,\n  \"winX\": 1500,\n  \
                  \"winY\": 80,\n  \"preset\": 1,\n  \"bandGain\": [0.75, 1.05, 1.35, 1.45, 1.2, -4]\n}\n";
        let (s, win) = from_json(v1);
        assert_eq!(win, Some((1500, 80)));
        assert_eq!((s.hud.scale, s.sensitivity, s.swap_lr, s.preset), (1.6, 1.2, true, Preset::Cs2));
        assert_eq!(s.band_gain, [0.75, 1.05, 1.35, 1.45, 1.2, 0.0]);
        assert_eq!(s.hud.style, HudStyle::Orbit, "v1 users land on the closest style");
    }

    #[test]
    fn junk_or_partial_files_fall_back_to_defaults() {
        assert_eq!(from_json("not json {"), (Settings::default(), None));
        let (s, win) = from_json("{\"sensitivity\": 0.8, \"hudStyle\": 7}");
        assert_eq!((s.sensitivity, s.hud.style, win), (0.8, HudStyle::Orbit, None));
        assert_eq!(s.band_gain, [1.0; 6]);
    }
}
