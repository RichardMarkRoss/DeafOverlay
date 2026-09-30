//! Settings panel below the HUD: a port of v1 `draw_settings_panel` / `draw_setup_body` and the
//! click handling in `WndProc`.
//!
//! One layout function ([`panel`]) builds everything. With a canvas it draws; either way it returns
//! the clickable widgets and the text to draw. v1 computed the geometry twice, in the draw code
//! and in the mouse handler, and the two had to be kept in step by hand.

use crate::hud::{Canvas, Layout, HUD_AREA_H};

pub const PANEL_W: i32 = 360;
pub const PANEL_H: i32 = 420;
pub const PANEL_X: i32 = 10;
pub const PANEL_TOP: i32 = HUD_AREA_H as i32 + 8;
pub const WIN_H_SETTINGS: usize = (PANEL_TOP + PANEL_H + 14) as usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    None,
    Cs2,
    R6s,
    Cod,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub layout: Layout,
    pub sensitivity: f32, // 0.5 .. 2.0
    /// 0-250 Hz, 250-2k, 2-5k, 5-8k, 8-12k, 12k+; each 0 .. 3, 1 = neutral.
    pub band_gain: [f32; 6],
    pub swap_lr: bool,
    pub preset: Preset,
}

impl Default for Settings {
    fn default() -> Self {
        Self { layout: Layout::default(), sensitivity: 1.0, band_gain: [1.0; 6], swap_lr: false, preset: Preset::None }
    }
}

impl Settings {
    /// v1 `apply_preset`: sensitivity + band gains tuned per game.
    pub fn apply_preset(&mut self, p: Preset) {
        self.preset = p;
        let (sens, gains) = match p {
            Preset::None => return,
            Preset::Cs2 => (1.20, [0.75, 1.05, 1.35, 1.45, 1.20, 1.00]),
            Preset::R6s => (1.25, [0.85, 1.20, 1.30, 1.35, 1.10, 1.00]),
            Preset::Cod => (1.10, [1.10, 1.25, 1.25, 1.15, 0.95, 1.00]),
        };
        self.sensitivity = sens;
        self.band_gain = gains;
    }

    /// v1 `set_preset_exclusive`: clicking the active preset unticks it (values stay).
    pub fn toggle_preset(&mut self, p: Preset) {
        if self.preset == p {
            self.preset = Preset::None;
        } else {
            self.apply_preset(p);
        }
    }

    /// Slider position 0..1 (v1 `slider_t_from_value`). 0 sphere, 1 ring, 2 sensitivity, 3.. bands.
    pub fn slider(&self, i: usize) -> f32 {
        let t = match i {
            0 => (self.layout.sphere_scale - 0.6) / 1.0,
            1 => (self.layout.ring_offset_y - 0.40) / 0.50,
            2 => (self.sensitivity - 0.5) / 1.5,
            _ => {
                // Bands: left half 0..1, right half 1..3, so neutral sits in the middle.
                let g = self.band_gain[i - 3].clamp(0.0, 3.0);
                if g <= 1.0 { 0.5 * g } else { 0.5 + 0.25 * (g - 1.0) }
            }
        };
        t.clamp(0.0, 1.0)
    }

    /// v1 `slider_apply`.
    pub fn set_slider(&mut self, i: usize, t: f32) {
        let t = t.clamp(0.0, 1.0);
        match i {
            0 => self.layout.sphere_scale = 0.6 + t,
            1 => self.layout.ring_offset_y = 0.40 + 0.50 * t,
            2 => self.sensitivity = 0.5 + 1.5 * t,
            _ => self.band_gain[i - 3] = if t <= 0.5 { 2.0 * t } else { 1.0 + 4.0 * (t - 0.5) },
        }
    }
}

const INFO_SPHERE: &str = "Sphere size:\nAdjust how large the 3D cue appears.\nSmaller = subtle, larger = easier to see peripheral threats.";
const INFO_RING: &str = "Ring vertical position:\nMove the horizontal ring up/down.\nPlace it near your crosshair/focus area for faster reading.";
const INFO_SENS: &str = "Global sensitivity:\nControls how strongly all sounds light up.\nLower = only louder/closer sounds.\nHigher = includes quiet cues like footsteps and reloads.";
const INFO_BASS: &str = "Bass (0–250 Hz):\nFootsteps thuds, rumbles, explosions, distant booms.\nIncrease to see low movement.\nReduce if constant rumble is too noisy.";
const INFO_LOWMID: &str = "Low-Mid (250–2 kHz):\nBody of gunshots, doors, movement, voices.\nGood for tracking nearby players.";
const INFO_MID: &str = "Mid (2–5 kHz):\nClarity of gunshots, glass, impacts, voice detail.\nImportant for precise combat cues.";
const INFO_HIGHMID: &str = "High-Mid (5–8 kHz):\nSharp details: metallic pings, shell drops, some footsteps.\nHelps separate close threats from background.";
const INFO_AIR: &str = "Treble / Air (8 kHz+):\nVery high details: hiss, sparks, UI beeps.\nTurn up to catch subtle directional cues, down if too busy.";
const INFO_SWAP: &str = "Swap L/R:\nFlip left and right.\nUse if your headset or game output feels reversed.";
const INFO_PRESET: &str = "Game presets:\nOne-click tuning for common games.\nThese adjust sensitivity + band gains to make key cues easier to see.\nYou can still fine-tune sliders after selecting.";

/// Label and info text per slider, in slider-index order. (v1 draws 8; band 5, 12k+, is
/// preset-only.)
const SLIDERS: [(&str, &str); 8] = [
    ("Sphere size", INFO_SPHERE),
    ("Ring vertical position", INFO_RING),
    ("Global sensitivity", INFO_SENS),
    ("Bass (0-250 Hz)", INFO_BASS),
    ("Low-Mid (250-2k)", INFO_LOWMID),
    ("Mid (2-5k)", INFO_MID),
    ("High-Mid (5-8k)", INFO_HIGHMID),
    ("Treble / Air (8k+)", INFO_AIR),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Controls,
    Setup,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Info(&'static str),
    Slider(usize),
    SwapLr,
    Preset(Preset),
    OpenSoundSettings,
    OpenSpatialSettings,
    SelfTest,
    Show(View),
}

#[derive(Clone, Copy, Debug)]
pub struct Widget {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub action: Action,
}

impl Widget {
    fn contains(&self, x: i32, y: i32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }
}

/// GDI draws these (it can measure text; this module can't).
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub x: i32,
    pub y: i32,
    pub s: String,
    pub rgb: (u8, u8, u8),
    /// Buttons: centre the label in this (x0, y0, x1, y1) box instead of drawing at (x, y).
    pub center_in: Option<(i32, i32, i32, i32)>,
}

/// What the Setup view reports.
#[derive(Clone, Debug, Default)]
pub struct SetupInfo {
    pub capture_ok: bool,
    pub channels: usize,
    /// `None` = all system audio; otherwise the captured process's exe name.
    pub capture_name: Option<String>,
    pub proc_unsupported: bool,
    /// Self-test running: `Some(-1)` silent gap, `Some(0)` left, `Some(1)` right.
    pub self_test: Option<i32>,
}

/// The settings view the user asked for, plus the panel's current layout.
pub struct Panel {
    pub widgets: Vec<Widget>,
    pub texts: Vec<Text>,
}

impl Panel {
    /// Info icons sit on top of their rows, so they win (v1 checked them first).
    pub fn hit(&self, x: i32, y: i32) -> Option<Action> {
        let hits = || self.widgets.iter().filter(move |w| w.contains(x, y));
        hits().find(|w| matches!(w.action, Action::Info(_))).or_else(|| hits().next()).map(|w| w.action)
    }

    pub fn slider_span(&self, i: usize) -> Option<(i32, i32)> {
        self.widgets.iter().find(|w| w.action == Action::Slider(i)).map(|w| (w.x0, w.x1))
    }
}

struct Builder<'a, 'b> {
    c: Option<&'a mut Canvas<'b>>,
    widgets: Vec<Widget>,
    texts: Vec<Text>,
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const CREAM: (u8, u8, u8) = (255, 255, 225);
const GREEN: (u8, u8, u8) = (120, 230, 120);
const RED: (u8, u8, u8) = (240, 120, 120);
const YELLOW: (u8, u8, u8) = (240, 220, 90);
const GREY: (u8, u8, u8) = (225, 225, 225);

impl Builder<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: u8, r: u8, g: u8, b: u8) {
        if let Some(c) = self.c.as_deref_mut() {
            c.fill_rect(x0, y0, x1, y1, a, r, g, b);
        }
    }

    fn put(&mut self, x: i32, y: i32, a: u8, rgb: (u8, u8, u8)) {
        if let Some(c) = self.c.as_deref_mut() {
            c.put(x, y, a, rgb.0, rgb.1, rgb.2);
        }
    }

    fn text(&mut self, x: i32, y: i32, s: impl Into<String>, rgb: (u8, u8, u8)) {
        self.texts.push(Text { x, y, s: s.into(), rgb, center_in: None });
    }

    fn widget(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, action: Action) {
        self.widgets.push(Widget { x0, y0, x1, y1, action });
    }

    fn info_icon(&mut self, cx: i32, cy: i32, info: &'static str) {
        for dy in -6..=6 {
            for dx in -6..=6 {
                if dx * dx + dy * dy <= 36 {
                    self.put(cx + dx, cy + dy, 220, (40, 40, 60));
                }
            }
        }
        self.put(cx, cy - 2, 255, WHITE); // the "i": dot...
        for yy in -1..=2 {
            self.put(cx, cy + yy, 255, WHITE); // ...and stem
        }
        self.widget(cx - 7, cy - 7, cx + 7, cy + 7, Action::Info(info));
    }

    fn slider(&mut self, i: usize, x0: i32, y: i32, x1: i32, t: f32) {
        for x in x0..=x1 {
            self.put(x, y, 90, (220, 220, 220));
        }
        let knob = x0 + (t * (x1 - x0) as f32) as i32;
        for dy in -4..=4 {
            for dx in -3..=3 {
                self.put(knob + dx, y + dy, 210, (255, 180, 80));
            }
        }
        self.widget(x0, y - 10, x1, y + 10, Action::Slider(i));
    }

    fn checkbox(&mut self, x: i32, y: i32, checked: bool) {
        const S: i32 = 12;
        for yy in 0..S {
            for xx in 0..S {
                let border = yy == 0 || yy == S - 1 || xx == 0 || xx == S - 1;
                self.put(x + xx, y + yy, if border { 220 } else { 40 }, WHITE);
            }
        }
        if checked {
            for i in 2..S - 2 {
                self.put(x + i, y + i, 230, (255, 220, 80));
                self.put(x + S - 1 - i, y + i, 230, (255, 220, 80));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, x: i32, y: i32, w: i32, h: i32, label: &str, action: Action, highlight: bool) {
        let (r, g, b) = if highlight { (70, 110, 170) } else { (38, 62, 120) };
        self.rect(x, y, x + w, y + h, 210, r, g, b);
        for xx in x..x + w {
            self.put(xx, y, 240, (r, g, b));
            self.put(xx, y + h - 1, 240, (r, g, b));
        }
        for yy in y..y + h {
            self.put(x, yy, 240, (r, g, b));
            self.put(x + w - 1, yy, 240, (r, g, b));
        }
        self.texts.push(Text { x, y, s: label.into(), rgb: WHITE, center_in: Some((x, y, x + w, y + h)) });
        self.widget(x, y, x + w, y + h, action);
    }
}

/// Lay out (and, given a canvas, draw) the settings panel.
pub fn panel(c: Option<&mut Canvas>, view: View, s: &Settings, info: &SetupInfo) -> Panel {
    let mut b = Builder { c, widgets: Vec::new(), texts: Vec::new() };
    let (x0, y0) = (PANEL_X, PANEL_TOP);
    let (x1, y1) = (x0 + PANEL_W, y0 + PANEL_H);
    b.rect(x0, y0, x1, y1, 205, 16, 16, 20);
    b.rect(x0, y0, x1, y0 + 24, 235, 32, 32, 40);
    let (ix0, ix1) = (x0 + 12, x1 - 32);
    let info_x = x1 - 14;

    match view {
        View::Setup => {
            b.text(x0 + 10, y0 + 5, "DeafOverlay - Setup", CREAM);
            setup_body(&mut b, y0, ix0, ix1, info);
        }
        View::Controls => {
            b.text(x0 + 10, y0 + 5, "DeafOverlay Settings", CREAM);
            let mut y = y0 + 40;
            for (i, (label, help)) in SLIDERS.iter().enumerate() {
                b.slider(i, ix0, y, ix1, s.slider(i));
                b.text(ix0, y - 14, *label, CREAM);
                b.info_icon(info_x, y - 10, help);
                y += 26;
            }
            let y_air = y - 26;

            let cb_y = y_air + 12;
            b.checkbox(ix0, cb_y, s.swap_lr);
            b.text(ix0 + 18, cb_y - 1, "Swap L/R", CREAM);
            b.info_icon(info_x, cb_y + 2, INFO_SWAP);
            b.widget(ix0, cb_y - 4, ix0 + 132, cb_y + 16, Action::SwapLr);

            let pres_y = cb_y + 28;
            b.text(ix0, pres_y - 2, "Game presets", CREAM);
            b.info_icon(info_x, pres_y + 2, INFO_PRESET);
            let row_y = pres_y + 18;
            for (k, (p, name)) in
                [(Preset::Cs2, "CS2"), (Preset::R6s, "Rainbow Six Siege"), (Preset::Cod, "Call of Duty")].into_iter().enumerate()
            {
                let by = row_y + 18 * k as i32;
                b.checkbox(ix0, by, s.preset == p);
                b.text(ix0 + 18, by - 1, name, CREAM);
                b.widget(ix0, by - 4, ix0 + 240, by + 16, Action::Preset(p));
            }
            b.text(ix0, row_y + 58, "(Select one, then fine-tune sliders)", (220, 220, 220));
            b.button(ix0, row_y + 76, ix1 - ix0, 22, "Setup / Help", Action::Show(View::Setup), false);
        }
    }
    Panel { widgets: b.widgets, texts: b.texts }
}

/// v1 `draw_setup_body`: detected layout, deep links to Windows settings, and the L/R self-test.
fn setup_body(b: &mut Builder, y0: i32, ix0: i32, ix1: i32, info: &SetupInfo) {
    let mut y = y0 + 30;
    let line = |b: &mut Builder, y: &mut i32, s: &str, rgb| {
        b.text(ix0, *y, s, rgb);
        *y += 18;
    };
    b.text(ix0, y, "Audio setup status", CREAM);
    y += 22;
    let n = info.channels;
    let full360 = n >= 6;
    if info.capture_ok {
        line(b, &mut y, "Capture: OK (system audio)", GREEN);
    } else {
        line(b, &mut y, "Capture: FAILED", RED);
    }
    let layout = match n {
        8.. => "7.1 surround",
        6..=7 => "5.1 surround",
        2 => "Stereo",
        _ => "Mono",
    };
    line(b, &mut y, &format!("Output: {layout} ({n} ch)"), GREY);
    if full360 {
        line(b, &mut y, "Direction: Full 360 (front + back)", GREEN);
    } else {
        line(b, &mut y, "Direction: Left / Right only", YELLOW);
    }
    y += 6;
    if full360 {
        line(b, &mut y, "Keep Spatial sound OFF for the", GREEN);
        line(b, &mut y, "most accurate direction.", GREY);
        y += 18;
    } else {
        line(b, &mut y, "Stereo can't show front/back.", YELLOW);
        line(b, &mut y, "For 360: set Windows output to 7.1,", GREY);
        line(b, &mut y, "then turn Spatial sound OFF.", GREY);
    }
    y += 4;
    match &info.capture_name {
        None => line(b, &mut y, "Capturing: all system audio", (220, 220, 220)),
        Some(name) => line(b, &mut y, &format!("Capturing: {name}"), GREEN),
    }
    if info.proc_unsupported {
        line(b, &mut y, "Game-only needs Win10 build 20348+", (240, 180, 90));
    }
    line(b, &mut y, "Alt+G: capture only the focused game", (180, 180, 200));

    y += 8;
    let (bw, bh) = (ix1 - ix0, 22);
    b.button(ix0, y, bw, bh, "Open Windows Sound settings", Action::OpenSoundSettings, false);
    y += bh + 6;
    b.button(ix0, y, bw, bh, "Open Spatial sound settings", Action::OpenSpatialSettings, false);
    y += bh + 6;
    let label = match info.self_test {
        None => "Test Left / Right",
        Some(0) => "Testing... LEFT  <",
        Some(1) => "Testing... RIGHT  >",
        Some(_) => "Testing...",
    };
    b.button(ix0, y, bw, bh, label, Action::SelfTest, info.self_test.is_some());
    y += bh + 4;
    b.text(ix0, y, "If sides are reversed, use Swap L/R.", (200, 200, 200));
    y += 22;
    b.button(ix0, y, bw, bh, "Back to Controls", Action::Show(View::Controls), false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn centre(p: &Panel, a: Action) -> (i32, i32) {
        let w = p.widgets.iter().find(|w| w.action == a).unwrap_or_else(|| panic!("no widget {a:?}"));
        ((w.x0 + w.x1) / 2, (w.y0 + w.y1) / 2)
    }

    #[test]
    fn every_control_is_clickable_where_it_is_drawn() {
        let s = Settings::default();
        let info = SetupInfo { capture_ok: true, channels: 2, ..Default::default() };
        for view in [View::Controls, View::Setup] {
            let p = panel(None, view, &s, &info);
            for w in &p.widgets {
                let (x, y) = centre(&p, w.action);
                assert_eq!(p.hit(x, y), Some(w.action), "{view:?}");
                assert!(w.y1 <= PANEL_TOP + PANEL_H && w.x1 <= PANEL_X + PANEL_W, "{:?} outside the panel", w.action);
            }
        }
        // Sliders only exist (and can only be hit) in the Controls view, like v1's !gShowSetup guard.
        assert!(panel(None, View::Setup, &s, &info).slider_span(0).is_none());
    }

    #[test]
    fn slider_mapping_round_trips_and_bands_are_neutral_in_the_middle() {
        let mut s = Settings::default();
        for i in 0..8 {
            for t in [0.0, 0.25, 0.5, 0.8, 1.0] {
                s.set_slider(i, t);
                assert!((s.slider(i) - t).abs() < 1e-5, "slider {i} at {t}");
            }
        }
        s.set_slider(3, 0.5);
        assert_eq!(s.band_gain[0], 1.0);
    }

    #[test]
    fn presets_are_exclusive_and_untick_on_second_click() {
        let mut s = Settings::default();
        s.toggle_preset(Preset::Cs2);
        assert_eq!((s.preset, s.sensitivity), (Preset::Cs2, 1.20));
        s.toggle_preset(Preset::Cod);
        assert_eq!((s.preset, s.band_gain[0]), (Preset::Cod, 1.10));
        s.toggle_preset(Preset::Cod);
        assert_eq!((s.preset, s.band_gain[0]), (Preset::None, 1.10));
    }
}
