//! Settings panel (its own window, detached from the HUD), styled after the Heroes Without Sound website: a rounded dark
//! card, the HWS mark, uppercase letter-spaced nav and buttons, orange as the one accent, Geist.
//! Three views (Display, Sound, Setup) keep it short enough to sit under a HUD.
//!
//! One layout function ([`panel`]) builds everything. With a canvas it draws; either way it returns
//! the clickable widgets and the text to draw. v1 computed the geometry twice, in the draw code
//! and in the mouse handler, and the two had to be kept in step by hand.

use crate::gfx::{Canvas, Paint, Rgb, BLACK, WHITE};
use crate::hud::{HudParams, HudStyle, PaletteKind};
use crate::text::{wrap, Anchor, Font, Text};

pub const PANEL_W: i32 = 360;
pub const PANEL_H: i32 = 480;
/// Content stays above the hotkey footer.
const FOOTER_H: i32 = 36;

// The website's Tailwind colours.
const ORANGE_400: Rgb = Rgb(251, 146, 60); // hover
const ORANGE_500: Rgb = Rgb(249, 115, 22);
const ORANGE_600: Rgb = Rgb(234, 88, 12); // pressed
const ORANGE_300: Rgb = Rgb(253, 186, 116);
const GRAY_200: Rgb = Rgb(229, 231, 235);
const GRAY_300: Rgb = Rgb(209, 213, 219);
const GRAY_400: Rgb = Rgb(156, 163, 175);
const GREEN_300: Rgb = Rgb(134, 239, 172);
const RED_300: Rgb = Rgb(252, 165, 165);

// Vertical rhythm, in px. Every view steps by these, so the four tabs share one spacing.
/// Section caption to the control under it.
const CAPTION: i32 = 20;
/// Between sibling controls.
const GAP: i32 = 8;
/// Between groups.
const GROUP: i32 = 12;
/// Band sliders sit tighter than other siblings, so the Sound view fits.
const DENSE: i32 = 2;
/// Status rows in Setup.
const ROW: i32 = 22;
const PILL_H: i32 = 28;
const SLIDER_H: i32 = 30;
const TOGGLE_H: i32 = 20;
const CARD_R: f32 = 22.0;
/// Width of the content inside the panel's 18 px side padding.
const CONTENT_W: f32 = (PANEL_W - 36) as f32;

/// Line pitch for wrapped text of size `px`.
const fn line(px: i32) -> i32 {
    px + 5
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    None,
    Cs2,
    R6s,
    Cod,
}

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::None => "None",
            Preset::Cs2 => "CS2",
            Preset::R6s => "R6",
            Preset::Cod => "CoD",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub hud: HudParams,
    pub sensitivity: f32, // 0.5 .. 2.0
    /// 0-250 Hz, 250-2k, 2-5k, 5-8k, 8-12k, 12k+; each 0 .. 3, 1 = neutral.
    pub band_gain: [f32; 6],
    pub swap_lr: bool,
    /// Stereo HRTF front/back guess (experimental, off by default).
    pub front_back: bool,
    pub preset: Preset,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hud: HudParams::default(), sensitivity: 1.0, band_gain: [1.0; 6], swap_lr: false, front_back: false, preset: Preset::None }
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
        if self.preset == p || p == Preset::None {
            self.preset = Preset::None;
        } else {
            self.apply_preset(p);
        }
    }

    /// Put back the tuning a preset overwrites (the preset's undo). Everything else stays.
    pub fn restore_tuning(&mut self, from: &Settings) {
        self.preset = from.preset;
        self.sensitivity = from.sensitivity;
        self.band_gain = from.band_gain;
    }

    /// Slider position 0..1. 0 size, 1 opacity, 2 sensitivity, 3.. bands.
    pub fn slider(&self, i: usize) -> f32 {
        let t = match i {
            0 => (self.hud.scale - 0.6) / 1.0,
            1 => (self.hud.opacity - 0.3) / 0.7,
            2 => (self.sensitivity - 0.5) / 1.5,
            _ => {
                // Bands: left half 0..1, right half 1..3, so neutral sits in the middle.
                let g = self.band_gain[i - 3].clamp(0.0, 3.0);
                if g <= 1.0 { 0.5 * g } else { 0.5 + 0.25 * (g - 1.0) }
            }
        };
        t.clamp(0.0, 1.0)
    }

    pub fn set_slider(&mut self, i: usize, t: f32) {
        let t = t.clamp(0.0, 1.0);
        match i {
            0 => self.hud.scale = 0.6 + t,
            1 => self.hud.opacity = 0.3 + 0.7 * t,
            2 => self.sensitivity = 0.5 + 1.5 * t,
            _ => self.band_gain[i - 3] = if t <= 0.5 { 2.0 * t } else { 1.0 + 4.0 * (t - 0.5) },
        }
    }

    /// The readout next to a slider.
    fn slider_value(&self, i: usize) -> String {
        match i {
            0 => format!("{:.0}%", self.hud.scale * 100.0),
            1 => format!("{:.0}%", self.hud.opacity * 100.0),
            2 => format!("{:.2}×", self.sensitivity),
            _ => format!("{:.2}×", self.band_gain[i - 3]),
        }
    }
}

const INFO_STYLE: &str = "HUD style:\nOrbit: a 3D orb over a compass ring.\nRadar: a top-down disc, or thin arcs around your crosshair.\nTape: a thin 360° compass strip, front in the middle, behind at both ends.\nAll three show the same sounds. Pick what reads fastest for you.";
const INFO_COLOURS: &str = "Colours:\nBrand: orange for front, blue for behind.\nColour-blind: orange and sky blue, distinct for common colour-vision deficiencies.\nContrast: yellow and white on a darker backing.\nFront and behind are also shown by shape, never by colour alone.";
const INFO_SIZE: &str = "Size:\nHow large the HUD is. It grows or shrinks around its centre.";
const INFO_OPACITY: &str = "Opacity:\nHow see-through the whole HUD is.";
const INFO_SENS: &str = "Sensitivity:\nHow strongly all sounds light up.\nLower: only louder, closer sounds.\nHigher: quiet cues like footsteps and reloads too.";
const INFO_BASS: &str = "Bass (0–250 Hz):\nFootstep thuds, rumbles, explosions, distant booms.\nTurn up to see low movement; down if constant rumble is too noisy.";
const INFO_LOWMID: &str = "Low-mid (250 Hz – 2 kHz):\nThe body of gunshots, doors, movement and voices. Good for tracking nearby players.";
const INFO_MID: &str = "Mid (2–5 kHz):\nThe clarity of gunshots, glass, impacts and voices. Important for precise combat cues.";
const INFO_HIGHMID: &str = "High-mid (5–8 kHz):\nSharp details: metallic pings, shell drops, some footsteps.";
const INFO_AIR: &str = "Air (8 kHz+):\nVery high details: hiss, sparks, UI beeps. Turn up for subtle cues, down if too busy.";
const INFO_SWAP: &str = "Swap left / right:\nFlip left and right if your headset or game output feels reversed.";
const INFO_FB: &str = "Guess front / back (experimental):\nWith stereo output, some games render 3D audio for headphones (HRTF). This tries to tell sounds behind you from sounds in front by how the ear shapes them.\nIt only shows BEHIND? when confident, so many rear sounds still appear in front. No effect on plain stereo, 5.1 or 7.1.";
const INFO_PRESET: &str = "Game presets:\nOne-click tuning of sensitivity and bands for common games. You can still fine-tune afterwards.";

/// Bands: (label, info), slider index 3.. (band 5, 12k+, is preset-only).
const BANDS: [(&str, &str); 5] = [
    ("Bass", INFO_BASS),
    ("Low-mid", INFO_LOWMID),
    ("Mid", INFO_MID),
    ("High-mid", INFO_HIGHMID),
    ("Air", INFO_AIR),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Display,
    Sound,
    Setup,
    Support,
}

/// Optional support, as the panel shows it. The window layer fills it from the Microsoft Store
/// (localised prices, licence); `available == None` while that first check is running.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Support {
    pub available: Option<bool>,
    /// Store-formatted price per option, in `SUPPORT_FALLBACK` order.
    pub prices: [Option<String>; 3],
    pub supporter: bool,
    pub busy: bool,
    pub message: Option<&'static str>,
}

/// In-app support through Microsoft Store add-ons. Off for now: the Support tab offers only
/// BackaBuddy, and the app never contacts the Store. Flip to `true` to bring the add-ons back
/// (also fill in `store::STORE_IDS`).
pub const STORE_SUPPORT: bool = false;

/// Shown until the Store reports local prices: $3 one-off, $10 one-off, $2 a month.
pub const SUPPORT_FALLBACK: [&str; 3] = ["$3", "$10", "$2"];

/// Which view is showing, what's open over it, and where the pointer is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelState {
    pub view: View,
    /// Info card open over the panel.
    pub help: Option<&'static str>,
    /// A preset just overwrote the sliders: the undo toast is showing.
    pub undo: bool,
    /// The control under the pointer, and whether the button is held on it.
    pub hot: Option<Action>,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Help(&'static str),
    CloseHelp,
    Slider(usize),
    Style(HudStyle),
    Crosshair,
    Palette(PaletteKind),
    SwapLr,
    FrontBack,
    Preset(Preset),
    UndoPreset,
    OpenSoundSettings,
    OpenSpatialSettings,
    SelfTest,
    Show(View),
    /// Support option 0..3 (see `SUPPORT_FALLBACK`).
    Buy(usize),
    OpenBackaBuddy,
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
    pub support: Support,
}

pub struct Panel {
    pub widgets: Vec<Widget>,
    pub texts: Vec<Text>,
}

impl Panel {
    /// Info icons sit on top of their rows, so they win (they never overlap a slider track).
    pub fn hit(&self, x: i32, y: i32) -> Option<Action> {
        let hits = || self.widgets.iter().filter(move |w| w.contains(x, y));
        hits().find(|w| matches!(w.action, Action::Help(_))).or_else(|| hits().next()).map(|w| w.action)
    }

    pub fn slider_span(&self, i: usize) -> Option<(i32, i32)> {
        self.widgets.iter().find(|w| w.action == Action::Slider(i)).map(|w| (w.x0, w.x1))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pill {
    Primary,
    Secondary,
    Nav { active: bool },
}

struct Builder<'a, 'b> {
    c: Option<&'a mut Canvas<'b>>,
    widgets: Vec<Widget>,
    texts: Vec<Text>,
    hot: Option<Action>,
    pressed: bool,
}

impl<'b> Builder<'_, 'b> {
    fn canvas(&mut self) -> Option<&mut Canvas<'b>> {
        self.c.as_deref_mut()
    }

    fn text(&mut self, t: Text) {
        self.texts.push(t);
    }

    fn widget(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, action: Action) {
        self.widgets.push(Widget { x0, y0, x1, y1, action });
    }

    /// (under the pointer, held down) for the control that does `action`.
    fn state(&self, action: Action) -> (bool, bool) {
        let hot = self.hot == Some(action);
        (hot, hot && self.pressed)
    }

    /// The website's section card: `rounded-3xl`, dark, hairline white border.
    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, radius: f32, fill: f32) {
        if let Some(c) = self.canvas() {
            let (x0, y0, x1, y1) = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
            c.rect(x0, y0, x1, y1, radius, BLACK, fill, Paint::Fill);
            c.rect(x0 + 0.5, y0 + 0.5, x1 - 0.5, y1 - 0.5, radius, WHITE, 0.10, Paint::Stroke { width: 1.0, dash: None });
        }
    }

    /// Wrapped text with its top at `y`; returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn para(&mut self, s: &str, x: i32, mut y: i32, width: f32, px: i32, font: Font, rgb: Rgb) -> i32 {
        for l in wrap(s, width, px) {
            self.text(Text::new(l, x as f32, (y + line(px) / 2) as f32, Anchor::MidLeft, font, px, rgb));
            y += line(px);
        }
        y
    }

    /// The site's "HWS" logo: initials in a thin white circle.
    fn brand_mark(&mut self, cx: i32, cy: i32) {
        if let Some(c) = self.canvas() {
            c.circle(cx as f32, cy as f32, 14.0, WHITE, 0.40, Paint::Stroke { width: 1.0, dash: None });
        }
        self.text(Text::new("HWS", cx as f32 + 0.5, cy as f32, Anchor::Center, Font::SemiBold, 9, WHITE).caps(1));
    }

    /// The site's pills, with its hover (`hover:bg-white/10`, orange-400) and a darker pressed shade.
    #[allow(clippy::too_many_arguments)]
    fn pill(&mut self, x0: i32, y0: i32, x1: i32, h: i32, label: &str, action: Action, kind: Pill) {
        let (hot, down) = self.state(action);
        let (fx0, fy0, fx1, fy1, r) = (x0 as f32, y0 as f32, x1 as f32, (y0 + h) as f32, h as f32 / 2.0);
        if let Some(c) = self.canvas() {
            match kind {
                Pill::Primary => {
                    let fill = if down { ORANGE_600 } else if hot { ORANGE_400 } else { ORANGE_500 };
                    c.rect(fx0, fy0, fx1, fy1, r, fill, 1.0, Paint::Fill);
                }
                Pill::Secondary => {
                    c.rect(fx0, fy0, fx1, fy1, r, BLACK, 0.40, Paint::Fill);
                    if hot {
                        c.rect(fx0, fy0, fx1, fy1, r, WHITE, if down { 0.16 } else { 0.10 }, Paint::Fill);
                    }
                    let edge = if hot { 0.55 } else { 0.35 };
                    c.rect(fx0 + 0.5, fy0 + 0.5, fx1 - 0.5, fy1 - 0.5, r, WHITE, edge, Paint::Stroke { width: 1.0, dash: None });
                }
                Pill::Nav { active } => {
                    let fill = if active { 0.10 } else { 0.0 } + if down { 0.12 } else if hot { 0.06 } else { 0.0 };
                    if fill > 0.0 {
                        c.rect(fx0, fy0, fx1, fy1, r, WHITE, fill, Paint::Fill);
                    }
                }
            }
        }
        let (font, rgb, track) = match kind {
            Pill::Primary => (Font::SemiBold, WHITE, 1),
            Pill::Secondary => (Font::Medium, if hot { WHITE } else { GRAY_200 }, 1),
            Pill::Nav { active } => (if active { Font::SemiBold } else { Font::Medium }, if active || hot { WHITE } else { GRAY_300 }, 2),
        };
        let (cx, cy) = ((x0 + x1) as f32 / 2.0, y0 as f32 + h as f32 / 2.0);
        self.text(Text::new(label, cx, cy, Anchor::Center, font, 10, rgb).caps(track));
        self.widget(x0, y0, x1, y0 + h, action);
    }

    /// A row of equal pills; the selected one is the primary (orange) pill.
    fn choices(&mut self, x0: i32, y: i32, x1: i32, items: &[(&str, Action, bool)]) {
        let n = items.len() as i32;
        let w = (x1 - x0 - 6 * (n - 1)) / n;
        for (i, (label, action, on)) in items.iter().enumerate() {
            let px = x0 + i as i32 * (w + 6);
            self.pill(px, y, px + w, PILL_H, label, *action, if *on { Pill::Primary } else { Pill::Secondary });
        }
    }

    /// The "i" icon: drawn 14 px, clickable over 24×24 (WCAG 2.2 target size).
    fn info(&mut self, cx: i32, cy: i32, text: &'static str) {
        let (hot, _) = self.state(Action::Help(text));
        if let Some(c) = self.canvas() {
            c.circle(cx as f32, cy as f32, 7.0, WHITE, if hot { 0.60 } else { 0.30 }, Paint::Stroke { width: 1.0, dash: None });
        }
        self.text(Text::new("i", cx as f32 + 0.5, cy as f32, Anchor::Center, Font::SemiBold, 10, if hot { WHITE } else { GRAY_300 }));
        self.widget(cx - 12, cy - 12, cx + 12, cy + 12, Action::Help(text));
    }

    /// Section caption: the site's small uppercase letter-spaced grey label.
    fn section(&mut self, x0: i32, x1: i32, y: i32, title: &str, info: Option<&'static str>) {
        self.text(Text::new(title, x0 as f32, y as f32, Anchor::TopLeft, Font::Medium, 10, GRAY_400).caps(2));
        if let Some(t) = info {
            self.info(x1 - 7, y + 6, t);
        }
    }

    /// Label and value on top, the track below. The info icon sits beside the track's end rather
    /// than above it, so its target and the track's never overlap.
    #[allow(clippy::too_many_arguments)]
    fn slider(&mut self, i: usize, x0: i32, y: i32, x1: i32, label: &str, value: String, t: f32, info: &'static str) {
        let (hot, down) = self.state(Action::Slider(i));
        let tx1 = x1 - 26;
        self.text(Text::new(label, x0 as f32, y as f32 + 7.0, Anchor::MidLeft, Font::Regular, 12, if hot { WHITE } else { GRAY_200 }));
        self.text(Text::new(value, tx1 as f32, y as f32 + 7.0, Anchor::MidRight, Font::Mono, 11, GRAY_300));
        self.info(x1 - 7, y + 21, info);
        let (fx0, fx1, ty) = (x0 as f32, tx1 as f32, y as f32 + 21.0);
        let knob = fx0 + t * (fx1 - fx0);
        if let Some(c) = self.canvas() {
            c.rect(fx0, ty - 1.5, fx1, ty + 1.5, 1.5, WHITE, 0.15, Paint::Fill);
            c.rect(fx0, ty - 1.5, knob, ty + 1.5, 1.5, ORANGE_500, 1.0, Paint::Fill);
            if hot {
                c.circle(knob, ty, if down { 12.0 } else { 10.0 }, ORANGE_500, 0.25, Paint::Fill);
            }
            c.circle(knob, ty, 7.0, BLACK, 0.35, Paint::Fill);
            c.circle(knob, ty, 6.0, WHITE, 1.0, Paint::Fill);
        }
        self.widget(x0, y + 10, tx1, y + SLIDER_H + 1, Action::Slider(i));
    }

    /// A switch; the whole row (label included) toggles it.
    #[allow(clippy::too_many_arguments)]
    fn toggle(&mut self, x0: i32, y: i32, x1: i32, label: &str, on: bool, action: Action, info: Option<&'static str>) {
        let (hot, down) = self.state(action);
        self.text(Text::new(label, x0 as f32, y as f32 + 10.0, Anchor::MidLeft, Font::Regular, 12, if hot { WHITE } else { GRAY_200 }));
        let (sx0, sx1, sy) = ((x1 - 58) as f32, (x1 - 28) as f32, y as f32 + 10.0);
        if let Some(c) = self.canvas() {
            if on {
                let fill = if down { ORANGE_600 } else if hot { ORANGE_400 } else { ORANGE_500 };
                c.rect(sx0, sy - 8.0, sx1, sy + 8.0, 8.0, fill, 1.0, Paint::Fill);
            } else {
                c.rect(sx0, sy - 8.0, sx1, sy + 8.0, 8.0, WHITE, if hot { 0.30 } else { 0.20 }, Paint::Fill);
            }
            c.circle(if on { sx1 - 8.0 } else { sx0 + 8.0 }, sy, 6.0, WHITE, 1.0, Paint::Fill);
        }
        if let Some(t) = info {
            self.info(x1 - 7, y + 10, t);
        }
        self.widget(x0, y, x1 - 20, y + TOGGLE_H, action);
    }

    /// Info text in a card over the bottom of the panel; any click (or Escape) closes it.
    fn help_card(&mut self, (x0, y0, x1, y1): (i32, i32, i32, i32), text: &str) {
        const INSET: i32 = 10;
        const PAD: i32 = 18;
        let lines = wrap(text, (x1 - x0 - 2 * (INSET + PAD)) as f32, 12);
        let h = PAD + line(12) * lines.len() as i32 + GROUP + PILL_H + PAD;
        let (cx0, cy0, cx1, cy1) = (x0 + INSET, y1 - INSET - h, x1 - INSET, y1 - INSET);
        // Nested in the panel's corner: inner radius = outer radius - inset.
        self.card(cx0, cy0, cx1, cy1, CARD_R - INSET as f32, 0.96);
        // Text is drawn after every shape, so the card can't hide what's under it by painting
        // over; drop that text instead.
        self.texts.retain(|t| t.y < cy0 as f32 || t.y > cy1 as f32);
        let mut y = cy0 + PAD;
        for (i, l) in lines.into_iter().enumerate() {
            let (font, rgb) = if i == 0 { (Font::SemiBold, ORANGE_300) } else { (Font::Regular, GRAY_200) };
            self.text(Text::new(l, (cx0 + PAD) as f32, (y + line(12) / 2) as f32, Anchor::MidLeft, font, 12, rgb));
            y += line(12);
        }
        let mid = (cx0 + cx1) / 2;
        self.pill(mid - 50, cy1 - PAD - PILL_H, mid + 50, PILL_H, "Got it", Action::CloseHelp, Pill::Secondary);
        // While it's open, a click anywhere just closes it.
        self.widgets = vec![Widget { x0, y0, x1, y1, action: Action::CloseHelp }];
    }

    /// The undo toast, in place of the hotkey footer for a few seconds after a preset.
    fn toast(&mut self, x0: i32, x1: i32, y1: i32, msg: &str) {
        let (tx0, ty0, tx1, ty1) = (x0 + 10, y1 - 34, x1 - 10, y1 - 6);
        if let Some(c) = self.canvas() {
            let r = (ty1 - ty0) as f32 / 2.0;
            c.rect(tx0 as f32, ty0 as f32, tx1 as f32, ty1 as f32, r, WHITE, 0.12, Paint::Fill);
        }
        let cy = (ty0 + ty1) / 2;
        self.text(Text::new(msg, (tx0 + 16) as f32, cy as f32, Anchor::MidLeft, Font::Regular, 12, GRAY_200));
        self.pill(tx1 - 4 - 64, cy - 10, tx1 - 4, 20, "Undo", Action::UndoPreset, Pill::Primary);
    }
}

/// Lay out (and, given a canvas, draw) the settings panel with its top-left at `(x0, y0)`.
pub fn panel(c: Option<&mut Canvas>, (x0, y0): (i32, i32), st: PanelState, s: &Settings, info: &SetupInfo) -> Panel {
    let mut b = Builder { c, widgets: Vec::new(), texts: Vec::new(), hot: st.hot, pressed: st.pressed };
    let (x1, y1) = (x0 + PANEL_W, y0 + PANEL_H);
    b.card(x0, y0, x1, y1, CARD_R, 0.92); // the site uses 55% + blur; with no blur, over a game, it needs more
    let (ix0, ix1) = (x0 + 18, x1 - 18);

    // header, like the website's floating nav bar
    b.brand_mark(ix0 + 14, y0 + 30);
    b.text(Text::new("Heroes Without Sound", (ix0 + 36) as f32, (y0 + 30) as f32, Anchor::MidLeft, Font::SemiBold, 10, GRAY_300).caps(2));
    if info.support.supporter {
        // The supporter's thank-you: an orange badge in the header (nothing else changes).
        if let Some(c) = b.canvas() {
            c.rect((ix1 - 84) as f32, (y0 + 20) as f32, ix1 as f32, (y0 + 40) as f32, 10.0, ORANGE_500, 1.0, Paint::Fill);
        }
        b.text(Text::new("Supporter", (ix1 - 42) as f32, (y0 + 30) as f32, Anchor::Center, Font::SemiBold, 9, WHITE).caps(1));
    } else {
        b.text(Text::new("ALT+I", ix1 as f32, (y0 + 30) as f32, Anchor::MidRight, Font::Mono, 10, GRAY_400));
    }
    let mut nx = ix0;
    for (label, v) in [("Display", View::Display), ("Sound", View::Sound), ("Setup", View::Setup), ("Support", View::Support)] {
        let w = 8 + 9 * label.len() as i32 + 10; // ~uppercase width at 10 px + tracking, plus padding
        b.pill(nx, y0 + 56, nx + w, 26, label, Action::Show(v), Pill::Nav { active: st.view == v });
        nx += w + 4;
    }
    if let Some(c) = b.canvas() {
        c.line(ix0 as f32, (y0 + 93) as f32, ix1 as f32, (y0 + 93) as f32, 1.0, WHITE, 0.10);
    }

    let top = y0 + 106;
    match st.view {
        View::Display => display(&mut b, top, ix0, ix1, s),
        View::Sound => sound(&mut b, top, ix0, ix1, s),
        View::Setup => setup(&mut b, top, ix0, ix1, info),
        View::Support => support(&mut b, top, ix0, ix1, &info.support, STORE_SUPPORT),
    }
    if st.undo {
        b.toast(x0, x1, y1, &format!("{} preset applied", s.preset.label()));
    } else {
        b.text(Text::new("ALT+H hide · ALT+G game only · ALT+Q quit", ((x0 + x1) / 2) as f32, (y1 - FOOTER_H / 2 - 2) as f32, Anchor::Center, Font::Mono, 10, GRAY_400));
    }
    if let Some(h) = st.help {
        b.help_card((x0, y0, x1, y1), h);
    }
    Panel { widgets: b.widgets, texts: b.texts }
}

fn display(b: &mut Builder, mut y: i32, ix0: i32, ix1: i32, s: &Settings) {
    let hud = s.hud;
    b.section(ix0, ix1, y, "HUD style", Some(INFO_STYLE));
    y += CAPTION;
    b.choices(ix0, y, ix1, &[
        ("Orbit", Action::Style(HudStyle::Orbit), hud.style == HudStyle::Orbit),
        ("Radar", Action::Style(HudStyle::Radar), hud.style == HudStyle::Radar),
        ("Tape", Action::Style(HudStyle::Tape), hud.style == HudStyle::Tape),
    ]);
    y += PILL_H + GROUP;
    if hud.style == HudStyle::Radar {
        b.toggle(ix0, y, ix1, "Crosshair mode", hud.crosshair, Action::Crosshair, None);
        y += TOGGLE_H + GROUP;
    }
    b.section(ix0, ix1, y, "Colours", Some(INFO_COLOURS));
    y += CAPTION;
    b.choices(ix0, y, ix1, &[
        ("Brand", Action::Palette(PaletteKind::Brand), hud.palette == PaletteKind::Brand),
        ("Colour-blind", Action::Palette(PaletteKind::ColorBlind), hud.palette == PaletteKind::ColorBlind),
        ("Contrast", Action::Palette(PaletteKind::HighContrast), hud.palette == PaletteKind::HighContrast),
    ]);
    y += PILL_H + GROUP;
    b.section(ix0, ix1, y, "Display", None);
    y += CAPTION;
    b.slider(0, ix0, y, ix1, "Size", s.slider_value(0), s.slider(0), INFO_SIZE);
    y += SLIDER_H + GAP;
    b.slider(1, ix0, y, ix1, "Opacity", s.slider_value(1), s.slider(1), INFO_OPACITY);
    y += SLIDER_H + GROUP;
    // The HUD and this panel are separate windows; say how to move them (the HUD itself is too
    // tight to carry a hint without covering its own labels).
    let hint = if hud.style == HudStyle::Radar && hud.crosshair {
        "Crosshair mode is locked to the centre of the screen. Drag an empty part of this panel to move it."
    } else {
        "Drag the HUD, or an empty part of this panel, to move it."
    };
    b.para(hint, ix0, y, CONTENT_W, 11, Font::Regular, GRAY_400);
}

fn sound(b: &mut Builder, mut y: i32, ix0: i32, ix1: i32, s: &Settings) {
    b.section(ix0, ix1, y, "Game preset", Some(INFO_PRESET));
    y += CAPTION;
    let presets = [Preset::None, Preset::Cs2, Preset::R6s, Preset::Cod].map(|p| (p.label(), Action::Preset(p), s.preset == p));
    b.choices(ix0, y, ix1, &presets);
    y += PILL_H + GROUP;
    b.slider(2, ix0, y, ix1, "Sensitivity", s.slider_value(2), s.slider(2), INFO_SENS);
    y += SLIDER_H + GROUP;
    b.section(ix0, ix1, y, "Bands", None);
    y += CAPTION;
    for (k, (label, help)) in BANDS.iter().enumerate() {
        b.slider(3 + k, ix0, y, ix1, label, s.slider_value(3 + k), s.slider(3 + k), help);
        y += SLIDER_H + DENSE;
    }
    y += GAP - DENSE;
    b.toggle(ix0, y, ix1, "Swap left / right", s.swap_lr, Action::SwapLr, Some(INFO_SWAP));
    y += TOGGLE_H + GAP;
    b.toggle(ix0, y, ix1, "Guess front / back (beta)", s.front_back, Action::FrontBack, Some(INFO_FB));
}

fn setup(b: &mut Builder, mut y: i32, ix0: i32, ix1: i32, info: &SetupInfo) {
    let n = info.channels;
    let full360 = n >= 6;
    let layout = match n {
        8.. => "7.1 surround",
        6..=7 => "5.1 surround",
        2 => "Stereo",
        _ => "Mono",
    };
    let mut rows: Vec<(&str, String, Rgb)> = vec![
        ("Capture", if info.capture_ok { "OK".into() } else { "Failed".into() }, if info.capture_ok { GREEN_300 } else { RED_300 }),
        ("Output", format!("{layout} · {n} ch"), GRAY_200),
        ("Direction", if full360 { "Full 360°".into() } else { "Front arc only".into() }, if full360 { GREEN_300 } else { ORANGE_300 }),
        ("Capturing", info.capture_name.clone().unwrap_or_else(|| "All system audio".into()), GRAY_200),
    ];
    if info.proc_unsupported {
        rows.push(("Game only", "Needs Win10 build 20348+".into(), ORANGE_300));
    }
    b.section(ix0, ix1, y, "Audio setup", None);
    y += CAPTION;
    for (k, v, rgb) in rows {
        let cy = (y + line(12) / 2) as f32;
        b.text(Text::new(k, ix0 as f32, cy, Anchor::MidLeft, Font::Regular, 12, GRAY_400));
        b.text(Text::new(v, (ix0 + 96) as f32, cy, Anchor::MidLeft, Font::Medium, 12, rgb));
        y += ROW;
    }
    let hint = if full360 {
        "Keep Spatial sound off in Windows for the most accurate direction."
    } else {
        "Stereo can't tell front from back. For 360°, set Windows output to 7.1 and turn Spatial sound off."
    };
    y = b.para(hint, ix0, y, CONTENT_W, 12, Font::Regular, GRAY_300) + GROUP;
    b.pill(ix0, y, ix1, PILL_H, "Windows sound settings", Action::OpenSoundSettings, Pill::Secondary);
    y += PILL_H + GAP;
    b.pill(ix0, y, ix1, PILL_H, "Spatial sound settings", Action::OpenSpatialSettings, Pill::Secondary);
    y += PILL_H + GAP;
    // Clicking again while it runs stops it.
    let (label, kind) = match info.self_test {
        None => ("Test left / right", Pill::Secondary),
        Some(0) => ("Testing left ‹ · stop", Pill::Primary),
        Some(1) => ("Testing right › · stop", Pill::Primary),
        Some(_) => ("Testing… · stop", Pill::Primary),
    };
    b.pill(ix0, y, ix1, PILL_H, label, Action::SelfTest, kind);
    y += PILL_H + GROUP;
    b.para("If sides look reversed, turn on Swap left / right in Sound.", ix0, y, CONTENT_W, 11, Font::Regular, GRAY_400);
}

/// Optional support: Store add-ons ($3 / $10 one-off, $2 a month) and BackaBuddy. Everything in
/// the app stays free; this tab only ever asks.
fn support(b: &mut Builder, mut y: i32, ix0: i32, ix1: i32, sup: &Support, store: bool) {
    b.section(ix0, ix1, y, "Support the project", None);
    y += CAPTION;
    let intro = "Heroes Without Sound is free, and every feature stays free. If it helps you play, you can chip in to keep it going.";
    y = b.para(intro, ix0, y, CONTENT_W, 12, Font::Regular, GRAY_300) + GROUP;
    if sup.supporter {
        y = b.para("You're a supporter. Thank you!", ix0, y, CONTENT_W, 12, Font::Medium, ORANGE_300) + GAP;
    }
    if store {
        let price = |i: usize| sup.prices[i].clone().unwrap_or_else(|| SUPPORT_FALLBACK[i].to_string());
        let half = (ix1 - ix0 - 6) / 2;
        b.pill(ix0, y, ix0 + half, PILL_H, &format!("One-off · {}", price(0)), Action::Buy(0), Pill::Secondary);
        b.pill(ix1 - half, y, ix1, PILL_H, &format!("One-off · {}", price(1)), Action::Buy(1), Pill::Secondary);
        y += PILL_H + GAP;
        let monthly = if sup.busy { "Opening the Store…".to_string() } else { format!("Monthly · {}/mo", price(2)) };
        b.pill(ix0, y, ix1, PILL_H, &monthly, Action::Buy(2), Pill::Primary);
        y += PILL_H + GROUP;
        let status = match (sup.message, sup.available) {
            (Some(m), _) => Some(m),
            (None, None) => Some("Checking the Microsoft Store…"),
            _ => None,
        };
        if let Some(m) = status {
            y = b.para(m, ix0, y, CONTENT_W, 11, Font::Regular, GRAY_400) + GAP;
        }
        b.section(ix0, ix1, y, "Prefer not to use the Store?", None);
        y += CAPTION;
    }
    // Without the Store options, BackaBuddy is the one way to support, so it's the primary pill.
    let kind = if store { Pill::Secondary } else { Pill::Primary };
    b.pill(ix0, y, ix1, PILL_H, "Support on BackaBuddy", Action::OpenBackaBuddy, kind);
    y += PILL_H + GROUP;
    b.para("Support is a thank-you, not a purchase: nothing in the app is locked.", ix0, y, CONTENT_W, 11, Font::Regular, GRAY_400);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn centre(p: &Panel, a: Action) -> (i32, i32) {
        let w = p.widgets.iter().find(|w| w.action == a).unwrap_or_else(|| panic!("no widget {a:?}"));
        ((w.x0 + w.x1) / 2, (w.y0 + w.y1) / 2)
    }

    const ORIGIN: (i32, i32) = (10, 250);

    fn st(view: View) -> PanelState {
        PanelState { view, ..Default::default() }
    }

    #[test]
    fn every_control_is_clickable_where_it_is_drawn_and_clear_of_the_footer() {
        let info = SetupInfo { capture_ok: true, channels: 2, proc_unsupported: true, ..Default::default() };
        for style in [HudStyle::Orbit, HudStyle::Radar, HudStyle::Tape] {
            let mut s = Settings::default();
            s.hud.style = style;
            for view in [View::Display, View::Sound, View::Setup, View::Support] {
                let p = panel(None, ORIGIN, st(view), &s, &info);
                for w in &p.widgets {
                    let (x, y) = centre(&p, w.action);
                    assert_eq!(p.hit(x, y), Some(w.action), "{style:?} {view:?}");
                    assert!(w.x0 >= ORIGIN.0 && w.x1 <= ORIGIN.0 + PANEL_W, "{:?} outside the panel", w.action);
                    assert!(w.y0 >= ORIGIN.1 && w.y1 <= ORIGIN.1 + PANEL_H - FOOTER_H, "{:?} over the footer ({view:?})", w.action);
                }
                for v in [View::Display, View::Sound, View::Setup, View::Support] {
                    assert!(p.widgets.iter().any(|w| w.action == Action::Show(v)), "nav to {v:?} missing");
                }
            }
            let p = panel(None, ORIGIN, st(View::Display), &s, &info);
            assert_eq!(p.widgets.iter().any(|w| w.action == Action::Crosshair), style == HudStyle::Radar);
        }
        let s = Settings::default();
        assert!(panel(None, ORIGIN, st(View::Setup), &s, &info).slider_span(0).is_none());
    }

    #[test]
    fn an_open_info_card_takes_every_click_and_fits_the_panel() {
        let s = Settings::default();
        for help in [INFO_STYLE, INFO_FB, INFO_SIZE] {
            let p = panel(None, ORIGIN, PanelState { view: View::Sound, help: Some(help), ..Default::default() }, &s, &SetupInfo::default());
            assert_eq!(p.hit(ORIGIN.0 + 20, ORIGIN.1 + 70), Some(Action::CloseHelp));
            assert_eq!(p.widgets.len(), 1);
            let underneath = p.texts.iter().filter(|t| t.s == "ALT+H hide · ALT+G game only · ALT+Q quit").count();
            assert_eq!(underneath, 0, "text under the card must not draw over it");
            let title = p.texts.iter().find(|t| t.rgb == ORANGE_300).expect("help title");
            assert!(title.y > (ORIGIN.1 + 10 + 18) as f32, "help card too tall for {help:.20}");
        }
    }

    /// The Support tab built with the Store options on or off, regardless of `STORE_SUPPORT`.
    fn support_panel(sup: &Support, store: bool) -> Panel {
        let mut b = Builder { c: None, widgets: Vec::new(), texts: Vec::new(), hot: None, pressed: false };
        support(&mut b, ORIGIN.1 + 106, ORIGIN.0 + 18, ORIGIN.0 + PANEL_W - 18, sup, store);
        Panel { widgets: b.widgets, texts: b.texts }
    }

    #[test]
    fn with_store_support_off_only_backabuddy_is_offered() {
        let p = support_panel(&Support::default(), false);
        assert!(!p.widgets.iter().any(|w| matches!(w.action, Action::Buy(_))), "Store options must be hidden");
        assert!(p.widgets.iter().any(|w| w.action == Action::OpenBackaBuddy));
        assert!(!p.texts.iter().any(|t| t.s.contains("Store")), "no mention of the Store when it's off");
        // and the real panel follows the switch
        let real = panel(None, ORIGIN, st(View::Support), &Settings::default(), &SetupInfo::default());
        assert_eq!(real.widgets.iter().any(|w| matches!(w.action, Action::Buy(_))), STORE_SUPPORT);
    }

    #[test]
    fn support_tab_offers_all_three_options_and_backabuddy_and_fits() {
        let s = Settings::default();
        // the tallest case: supporter line + a two-line status message
        let support = Support { supporter: true, message: Some("The Store couldn't complete that. Please try again."), ..Default::default() };
        let p = support_panel(&support, true);
        for a in [Action::Buy(0), Action::Buy(1), Action::Buy(2), Action::OpenBackaBuddy] {
            let w = p.widgets.iter().find(|w| w.action == a).unwrap_or_else(|| panic!("{a:?} missing"));
            assert!(w.y1 <= ORIGIN.1 + PANEL_H - FOOTER_H, "{a:?} over the footer");
        }
        let lowest = p.texts.iter().filter(|t| !t.s.starts_with("ALT+H")).map(|t| t.y).fold(0.0, f32::max);
        assert!(lowest <= (ORIGIN.1 + PANEL_H - FOOTER_H) as f32, "support text runs into the footer");
        // fallback prices until the Store answers, the Store's own once it has
        assert!(p.texts.iter().any(|t| t.s == "One-off · $3") && p.texts.iter().any(|t| t.s == "Monthly · $2/mo"));
        let local = Support { prices: [Some("R55.00".into()), None, None], ..Default::default() };
        let p = support_panel(&local, true);
        assert!(p.texts.iter().any(|t| t.s == "One-off · R55.00"));
        // the badge replaces the hotkey hint for supporters only
        let badge = |sup: bool| {
            let info = SetupInfo { support: Support { supporter: sup, ..Default::default() }, ..Default::default() };
            panel(None, ORIGIN, st(View::Display), &s, &info).texts.iter().any(|t| t.s == "Supporter")
        };
        assert!(badge(true) && !badge(false));
    }

    #[test]
    fn undo_toast_replaces_the_footer_and_restores_the_tuning() {
        let mut s = Settings::default();
        s.band_gain[0] = 2.5;
        let before = s;
        s.toggle_preset(Preset::Cs2);
        s.swap_lr = true; // changed after the preset: undo must keep it
        let p = panel(None, ORIGIN, PanelState { view: View::Sound, undo: true, ..Default::default() }, &s, &SetupInfo::default());
        assert!(p.texts.iter().any(|t| t.s == "CS2 preset applied"));
        assert!(!p.texts.iter().any(|t| t.s.starts_with("ALT+H")), "the toast takes the footer's place");
        let (x, y) = centre(&p, Action::UndoPreset);
        assert_eq!(p.hit(x, y), Some(Action::UndoPreset));
        assert!(y < ORIGIN.1 + PANEL_H);
        s.restore_tuning(&before);
        assert_eq!((s.preset, s.band_gain, s.sensitivity, s.swap_lr), (Preset::None, before.band_gain, 1.0, true));
    }

    #[test]
    fn info_targets_are_24px_and_never_cover_a_slider_track() {
        for view in [View::Display, View::Sound] {
            let p = panel(None, ORIGIN, st(view), &Settings::default(), &SetupInfo::default());
            for i in p.widgets.iter().filter(|w| matches!(w.action, Action::Help(_))) {
                assert!(i.x1 - i.x0 >= 24 && i.y1 - i.y0 >= 24);
                for t in p.widgets.iter().filter(|w| matches!(w.action, Action::Slider(_))) {
                    let overlap = i.x0 <= t.x1 && t.x0 <= i.x1 && i.y0 <= t.y1 && t.y0 <= i.y1;
                    assert!(!overlap, "{:?} covers {:?}", i.action, t.action);
                }
            }
        }
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
        assert_eq!(s.slider_value(3), "1.00×");
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
        s.toggle_preset(Preset::R6s);
        s.toggle_preset(Preset::None);
        assert_eq!(s.preset, Preset::None);
    }
}
