//! Settings file, same place and format as v1 (`save_settings_file` / `load_settings_file`):
//! `%APPDATA%\DeafOverlay\settings.json`, flat keys, so v1 and this build share one file.
//! Loaded values are clamped like v1; missing or malformed keys keep their defaults.

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

/// Same keys and layout v1 writes.
pub fn to_json(s: &Settings, win: (i32, i32)) -> String {
    let l = &s.layout;
    let preset = match s.preset {
        Preset::None => 0,
        Preset::Cs2 => 1,
        Preset::R6s => 2,
        Preset::Cod => 3,
    };
    let bands: Vec<String> = s.band_gain.iter().map(|g| g.to_string()).collect();
    format!(
        "{{\n  \"sphereScale\": {},\n  \"ringOffsetY\": {},\n  \"sensitivity\": {},\n  \"swapLR\": {},\n  \
         \"spherePosX\": {},\n  \"spherePosY\": {},\n  \"winX\": {},\n  \"winY\": {},\n  \"preset\": {},\n  \
         \"bandGain\": [{}]\n}}\n",
        l.sphere_scale,
        l.ring_offset_y,
        s.sensitivity,
        s.swap_lr as i32,
        l.sphere_x,
        l.sphere_y,
        win.0,
        win.1,
        preset,
        bands.join(", ")
    )
}

pub fn from_json(text: &str) -> (Settings, Option<(i32, i32)>) {
    let mut s = Settings::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (s, None);
    };
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).map(|x| x as f32);
    let l = &mut s.layout;
    if let Some(x) = num("sphereScale") {
        l.sphere_scale = x.clamp(0.6, 1.6);
    }
    if let Some(x) = num("ringOffsetY") {
        l.ring_offset_y = x.clamp(0.40, 0.90);
    }
    if let Some(x) = num("spherePosX") {
        l.sphere_x = x.clamp(0.05, 0.95);
    }
    if let Some(x) = num("spherePosY") {
        l.sphere_y = x.clamp(0.05, 0.95);
    }
    if let Some(x) = num("sensitivity") {
        s.sensitivity = x.clamp(0.5, 2.0);
    }
    if let Some(x) = num("swapLR") {
        s.swap_lr = x >= 0.5;
    }
    s.preset = match num("preset").map(|x| x as i32) {
        Some(1) => Preset::Cs2,
        Some(2) => Preset::R6s,
        Some(3) => Preset::Cod,
        _ => Preset::None,
    };
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
        s.layout.sphere_x = 0.3;
        s.layout.sphere_scale = 1.25;
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
        assert_eq!((s.layout.sphere_scale, s.sensitivity, s.swap_lr, s.preset), (1.6, 1.2, true, Preset::Cs2));
        assert_eq!(s.band_gain, [0.75, 1.05, 1.35, 1.45, 1.2, 0.0]);
    }

    #[test]
    fn junk_or_partial_files_fall_back_to_defaults() {
        assert_eq!(from_json("not json {"), (Settings::default(), None));
        let (s, win) = from_json("{\"sensitivity\": 0.8}");
        assert_eq!((s.sensitivity, win), (0.8, None));
        assert_eq!(s.band_gain, [1.0; 6]);
    }
}
