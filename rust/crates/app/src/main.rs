//! DeafOverlay windows: a compact, click-through, always-on-top layered popup that draws the HUD,
//! and a second one for the settings panel, so the two can be placed independently. A port of v1
//! `wWinMain` / `WndProc` / `set_settings_mode` / `blit_layered`.
//!
//! Not a fullscreen transparent layer: a fullscreen overlay matching the game's client rect is the
//! textbook ESP-cheat heuristic that anti-cheat looks for.
//!
//! Hotkeys: Alt+H show/hide, Alt+I settings, Alt+G capture only the focused game (again to go back
//! to the whole mix), Alt+Q quit.
//!
//! Settings persist to `%APPDATA%\DeafOverlay\settings.json` (shared with v1, see `persist`); the
//! first launch opens the Setup view, flagged in `HKCU\Software\DeafOverlay\OnboardingDone` like v1.

#![windows_subsystem = "windows"]

mod store;

use capture::{Capture, Tone};
use deafoverlay::gfx::{Canvas, Paint, BLACK, WHITE};
use deafoverlay::hud::{self, HudStyle, Normalizer, Scene};
use deafoverlay::persist::{self, Positions};
use deafoverlay::text::{Anchor, Font, Text};
use deafoverlay::ui::{self, Action, PanelState, Settings, SetupInfo, View, PANEL_H, PANEL_W};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

const HK_TOGGLE_VIS: i32 = 1;
const HK_TOGGLE_CFG: i32 = 2;
const HK_QUIT: i32 = 3;
const HK_GAME: i32 = 4;

/// Read by `wnd_proc` for hit-testing: whether the HUD window takes clicks (settings open, and
/// not locked to the centre by crosshair mode). The panel window always does.
static HUD_INTERACTIVE: AtomicBool = AtomicBool::new(false);
/// Tells `wnd_proc` which of the two windows a message is for.
static PANEL_HWND: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Hud,
    Panel,
}

/// Mouse input queued by `wnd_proc` and handled in the main loop, which owns the app state.
/// Window moves run a modal loop that re-enters `wnd_proc`, so handling input in there would
/// mean re-entrant access to that state.
#[derive(Clone, Copy)]
enum Ev {
    Down(Which, i32, i32),
    Move(i32), // x only: sliders are horizontal
    Up,
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

/// A 32-bit top-down DIB section with its own memory DC. The window's size is this DIB's size
/// (see [`blit`]), exactly like v1.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut u32,
    w: usize,
    h: usize,
}

impl Dib {
    fn new(w: usize, h: usize) -> windows::core::Result<Self> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            SelectObject(dc, bmp.into());
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
            Ok(Self { dc, bmp, bits: bits.cast(), w, h })
        }
    }

    fn canvas(&mut self) -> Canvas<'_> {
        // SAFETY: CreateDIBSection allocated w*h 32-bit pixels; `&mut self` keeps this the only view.
        let px = unsafe { std::slice::from_raw_parts_mut(self.bits, self.w * self.h) };
        Canvas::new(self.w, self.h, px)
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

/// Geist, embedded (SIL Open Font License 1.1, see `fonts/OFL.txt`). Registered privately for
/// this process at startup: nothing gets installed on the user's machine.
const FONT_FILES: [&[u8]; 5] = [
    include_bytes!("../fonts/Geist-Regular.ttf"),
    include_bytes!("../fonts/Geist-Medium.ttf"),
    include_bytes!("../fonts/Geist-SemiBold.ttf"),
    include_bytes!("../fonts/GeistMono-Regular.ttf"),
    include_bytes!("../fonts/GeistMono-Medium.ttf"),
];

fn register_fonts() {
    for data in FONT_FILES {
        let mut count = 0u32;
        // Windows writes the font count through this pointer, so it must stay `&mut`.
        #[allow(clippy::unnecessary_mut_passed)]
        unsafe {
            AddFontMemResourceEx(data.as_ptr().cast(), data.len() as u32, None, &mut count);
        }
    }
}

/// GDI face name and weight per cut. Static weights register under their own legacy family
/// (name ID 1 in each file: "Geist SemiBold", not "Geist" + weight 600), so ask for those names.
fn face(f: Font) -> (PCWSTR, FONT_WEIGHT) {
    match f {
        Font::Regular => (w!("Geist"), FW_NORMAL),
        Font::Medium => (w!("Geist Medium"), FW_MEDIUM),
        Font::SemiBold => (w!("Geist SemiBold"), FW_SEMIBOLD),
        Font::Mono => (w!("Geist Mono"), FW_NORMAL),
        Font::MonoMedium => (w!("Geist Mono Medium"), FW_MEDIUM),
    }
}

/// All text, HUD and panel alike, drawn properly over a translucent window: GDI renders white-on-
/// black text into a scratch DIB (grayscale antialiasing), and that brightness becomes the coverage
/// for an alpha-blended glyph in any colour, optionally outlined. (Plain GDI text can't be
/// alpha-blended; v1 forced an opaque box behind every string instead.)
struct TextRenderer {
    scratch: Dib,
    fonts: HashMap<(Font, i32), HFONT>,
}

impl TextRenderer {
    fn new() -> windows::core::Result<Self> {
        Ok(Self { scratch: Dib::new(512, 48)?, fonts: HashMap::new() })
    }

    fn font(&mut self, f: Font, px: i32) -> HFONT {
        *self.fonts.entry((f, px)).or_insert_with(|| unsafe {
            let (name, weight) = face(f);
            CreateFontW(-px, 0, 0, 0, weight.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, name)
        })
    }

    fn draw(&mut self, c: &mut Canvas, texts: &[Text]) {
        for t in texts {
            let s = if t.upper { t.s.to_uppercase() } else { t.s.clone() };
            let wide: Vec<u16> = s.encode_utf16().collect();
            let font = self.font(t.font, t.px);
            let d = &mut self.scratch;
            let mut sz = SIZE::default();
            unsafe {
                SelectObject(d.dc, font.into());
                SetTextCharacterExtra(d.dc, t.tracking);
                let _ = GetTextExtentPoint32W(d.dc, &wide, &mut sz);
            }
            // Tracking adds space after the last letter too; don't let it shift centring.
            let ink_w = (sz.cx - t.tracking).max(0);
            let (w, h) = ((sz.cx as usize + 2).min(d.w), (sz.cy as usize + 2).min(d.h));
            {
                let mask = d.canvas();
                for y in 0..h {
                    mask.px[y * mask.w..y * mask.w + w].fill(0);
                }
            }
            unsafe {
                SetTextColor(d.dc, COLORREF(0x00FF_FFFF));
                let _ = TextOutW(d.dc, 1, 1, &wide);
                let _ = GdiFlush();
            }
            let mask = d.canvas();
            let cov = |x: i32, y: i32| -> f32 {
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    0.0
                } else {
                    ((mask.px[y as usize * mask.w + x as usize] >> 16) & 0xFF) as f32 / 255.0
                }
            };
            let (iw, ih) = (ink_w as f32 + 2.0, h as f32);
            let (fx, fy) = match t.anchor {
                Anchor::TopLeft => (t.x - 1.0, t.y - 1.0),
                Anchor::Center => (t.x - iw / 2.0, t.y - ih / 2.0),
                Anchor::MidLeft => (t.x - 1.0, t.y - ih / 2.0),
                Anchor::MidRight => (t.x - iw + 1.0, t.y - ih / 2.0),
            };
            let (x0, y0) = (fx.round() as i32, fy.round() as i32);
            if t.outline {
                // dark outline first (coverage grown by a pixel, plus-shaped so small text stays crisp)
                for y in -1..=h as i32 {
                    for x in -1..=w as i32 {
                        let mut o = 0.0f32;
                        for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                            o = o.max(cov(x + dx, y + dy));
                        }
                        c.blend(x0 + x, y0 + y, BLACK, 0.8 * o);
                    }
                }
            }
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    c.blend(x0 + x, y0 + y, t.rgb, cov(x, y));
                }
            }
        }
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        for f in self.fonts.values() {
            unsafe {
                let _ = DeleteObject((*f).into());
            }
        }
    }
}

/// Push the DIB to the layered window. No destination point: keep the window where it is and take
/// the size from the DIB, so resizing the window means resizing the DIB (v1 `blit_layered`).
fn blit(hwnd: HWND, dib: &Dib) {
    let size = SIZE { cx: dib.w as i32, cy: dib.h as i32 };
    let src = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        let _ = UpdateLayeredWindow(hwnd, None, None, Some(&size), Some(dib.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

enum Drag {
    None,
    Slider { i: usize, x0: i32, x1: i32 },
}

fn screen() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// Keep a `w`×`h` window's top-left on the primary screen.
fn on_screen((x, y): (i32, i32), w: i32, h: i32) -> (i32, i32) {
    let (sw, sh) = screen();
    (x.clamp(0, (sw - w).max(0)), y.clamp(0, (sh - h).max(0)))
}

fn place(hwnd: HWND, (x, y): (i32, i32)) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn window_pos(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rc).is_ok().then_some((rc.left, rc.top)) }
}

/// Move a window by following the cursor (the system move loop, as if dragging a title bar).
/// Returns when the button is released.
fn drag_window(hwnd: HWND) -> Option<(i32, i32)> {
    unsafe {
        let _ = ReleaseCapture();
        SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
    }
    window_pos(hwnd)
}

struct App {
    /// The HUD and the settings panel are separate windows, each dragged and saved on its own.
    hud: HWND,
    panel: HWND,
    hud_dib: Dib,
    panel_dib: Dib,
    text: TextRenderer,
    capture: Capture,
    tone: Option<Tone>,
    norm: Normalizer,
    settings: Settings,
    view: View,
    /// Info card open over the panel.
    help: Option<&'static str>,
    open: bool,
    visible: bool,
    drag: Drag,
    /// HUD top-left on screen.
    win: (i32, i32),
    /// Panel top-left on screen; `None` until first placed (then it starts beside the HUD).
    panel_pos: Option<(i32, i32)>,
    /// (pid, exe name) for the Setup view's "Capturing:" line.
    proc_name: (u32, String),
    /// Optional support (Store add-ons); see `store`.
    store: store::Store,
}

impl App {
    fn hud_size(&self) -> (i32, i32) {
        let (w, h) = hud::size(&self.settings.hud);
        (w as i32, h as i32)
    }

    fn crosshair(&self) -> bool {
        self.settings.hud.style == HudStyle::Radar && self.settings.hud.crosshair
    }

    /// Size and position the HUD window. Crosshair mode is pinned to the screen centre, always.
    fn place_hud(&mut self) {
        let (w, h) = self.hud_size();
        if self.crosshair() {
            let (sw, sh) = screen();
            self.win = ((sw - w) / 2, (sh - h) / 2);
        }
        self.win = on_screen(self.win, w, h);
        if (self.hud_dib.w, self.hud_dib.h) != (w as usize, h as usize) {
            if let Ok(d) = Dib::new(w as usize, h as usize) {
                self.hud_dib = d;
            }
        }
        place(self.hud, self.win);
        self.update_hud_input();
    }

    /// While settings are open the HUD takes clicks so it can be dragged, except in crosshair
    /// mode, which stays locked to the centre and click-through.
    fn update_hud_input(&self) {
        let interactive = self.open && !self.crosshair();
        HUD_INTERACTIVE.store(interactive, Ordering::Relaxed);
        unsafe {
            let mut ex = GetWindowLongW(self.hud, GWL_EXSTYLE);
            if interactive {
                ex &= !(WS_EX_TRANSPARENT.0 as i32);
            } else {
                ex |= WS_EX_TRANSPARENT.0 as i32;
            }
            SetWindowLongW(self.hud, GWL_EXSTYLE, ex);
        }
    }

    /// Change what the HUD looks like, keeping its centre where it is on screen.
    fn change_hud(&mut self, f: impl FnOnce(&mut Settings)) {
        let (w0, h0) = self.hud_size();
        f(&mut self.settings);
        let (w1, h1) = self.hud_size();
        self.win = (self.win.0 + (w0 - w1) / 2, self.win.1 + (h0 - h1) / 2);
        self.place_hud();
    }

    /// First time: beside the HUD (right if it fits, else left, else centred), vertically aligned.
    fn default_panel_pos(&self) -> (i32, i32) {
        let (sw, _) = screen();
        let (hw, hh) = self.hud_size();
        let x = if self.win.0 + hw + 16 + PANEL_W <= sw {
            self.win.0 + hw + 16
        } else if self.win.0 - 16 - PANEL_W >= 0 {
            self.win.0 - 16 - PANEL_W
        } else {
            (sw - PANEL_W) / 2
        };
        (x, self.win.1 + hh / 2 - PANEL_H / 2)
    }

    fn set_settings_mode(&mut self, on: bool) {
        self.open = on;
        if on {
            let pos = on_screen(self.panel_pos.unwrap_or_else(|| self.default_panel_pos()), PANEL_W, PANEL_H);
            self.panel_pos = Some(pos);
            place(self.panel, pos);
        } else {
            self.save();
            self.view = View::Display;
            self.help = None;
            self.drag = Drag::None;
            self.tone = None; // stops the self-test
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        unsafe {
            let _ = ShowWindow(self.panel, if on && self.visible { SW_SHOWNA } else { SW_HIDE });
        }
        self.update_hud_input();
    }

    fn panel_state(&self) -> PanelState {
        PanelState { view: self.view, help: self.help }
    }

    fn push_dsp_settings(&self) {
        self.capture.set_swap_lr(self.settings.swap_lr);
        self.capture.set_band_gains(self.settings.band_gain);
        self.capture.set_front_back(self.settings.front_back);
    }

    fn setup_info(&mut self) -> SetupInfo {
        let st = self.capture.status();
        if st.pid != self.proc_name.0 {
            self.proc_name = (st.pid, process_name(st.pid));
        }
        SetupInfo {
            capture_ok: st.running,
            channels: st.channels,
            capture_name: (st.pid != 0).then(|| self.proc_name.1.clone()),
            proc_unsupported: st.proc_unsupported,
            self_test: self.tone.as_ref().map(Tone::side),
            support: self.store.state.clone(),
        }
    }

    fn handle(&mut self, ev: Ev) {
        if !self.open {
            return;
        }
        match ev {
            Ev::Down(Which::Panel, x, y) => self.panel_down(x, y),
            Ev::Down(Which::Hud, _, _) => {
                if !self.crosshair() {
                    if let Some(p) = drag_window(self.hud) {
                        self.win = p;
                    }
                    self.save();
                }
            }
            Ev::Move(x) => match self.drag {
                Drag::Slider { i, x0, x1 } => {
                    let t = (x - x0) as f32 / (x1 - x0) as f32;
                    // Size (slider 0) resizes the HUD, so go through change_hud to stay centred.
                    self.change_hud(|s| s.set_slider(i, t));
                    self.push_dsp_settings();
                }
                Drag::None => {}
            },
            Ev::Up => {
                if !matches!(self.drag, Drag::None) {
                    self.save(); // v1 saved after every slider drag
                }
                self.drag = Drag::None;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
        }
    }

    /// Best effort, like v1: a failed save must never take the overlay down.
    fn save(&self) {
        let _ = persist::save(&self.settings, Positions { hud: Some(self.win), panel: self.panel_pos });
    }

    fn panel_down(&mut self, x: i32, y: i32) {
        let info = self.setup_info();
        let panel = ui::panel(None, (0, 0), self.panel_state(), &self.settings, &info);
        let Some(action) = panel.hit(x, y) else {
            // Empty card: drag the panel on its own.
            self.panel_pos = drag_window(self.panel).or(self.panel_pos);
            self.save();
            return;
        };
        match action {
            Action::Help(text) => self.help = Some(text),
            Action::CloseHelp => self.help = None,
            Action::Slider(i) => {
                let (x0, x1) = panel.slider_span(i).unwrap_or((0, 1));
                self.drag = Drag::Slider { i, x0, x1 };
                unsafe { SetCapture(self.panel) };
                self.handle(Ev::Move(x));
            }
            Action::Style(style) => {
                self.change_hud(|s| s.hud.style = style);
                self.update_hud_input();
                self.save();
            }
            Action::Crosshair => {
                self.change_hud(|s| s.hud.crosshair = !s.hud.crosshair);
                self.save();
            }
            Action::Palette(p) => {
                self.settings.hud.palette = p;
                self.save();
            }
            Action::SwapLr => {
                self.settings.swap_lr = !self.settings.swap_lr;
                self.push_dsp_settings();
                self.save();
            }
            Action::FrontBack => {
                self.settings.front_back = !self.settings.front_back;
                self.push_dsp_settings();
                self.save();
            }
            Action::Preset(p) => {
                self.settings.toggle_preset(p);
                self.push_dsp_settings();
                self.save();
            }
            Action::OpenSoundSettings => open_url("ms-settings:sound"),
            Action::OpenSpatialSettings => open_url("ms-settings:sound-defaultoutputproperties"),
            Action::Buy(i) => self.store.buy(i),
            Action::OpenBackaBuddy => open_url(store::BACKABUDDY),
            Action::SelfTest => {
                if self.tone.is_none() {
                    self.tone = Some(Tone::start());
                }
            }
            Action::Show(v) => self.view = v,
        }
    }

    fn frame(&mut self, dt: f32) {
        if self.tone.as_ref().is_some_and(Tone::finished) {
            self.tone = None;
        }
        self.store.poll();
        let st = self.capture.status();
        let peaks = self.norm.peaks(&self.capture.sources(), st.full_360, self.settings.sensitivity, dt);
        let scene = Scene { full_360: st.full_360, front_back: self.settings.front_back };
        let hud = self.settings.hud;
        let (crosshair, open) = (self.crosshair(), self.open);
        let labels = {
            let mut c = self.hud_dib.canvas();
            c.px.fill(0); // transparent, and click-through
            if open {
                // Settings open: show the HUD's bounds. A light fill makes it draggable (layered
                // windows only take clicks on visible pixels); crosshair mode stays locked.
                let (w, h) = (c.w as f32, c.h as f32);
                if !crosshair {
                    c.rect(0.0, 0.0, w, h, 12.0, BLACK, 0.30, Paint::Fill);
                }
                c.rect(1.0, 1.0, w - 1.0, h - 1.0, 12.0, WHITE, 0.45, Paint::Stroke { width: 1.0, dash: Some(8.0) });
            }
            hud::draw(&mut c, (0.0, 0.0), &hud, &peaks, scene)
        };
        let mut c = self.hud_dib.canvas();
        c.alpha = hud.opacity;
        self.text.draw(&mut c, &labels);
        blit(self.hud, &self.hud_dib);

        if open {
            let info = self.setup_info();
            let st = self.panel_state();
            self.panel_dib.canvas().px.fill(0);
            let panel = ui::panel(Some(&mut self.panel_dib.canvas()), (0, 0), st, &self.settings, &info);
            self.text.draw(&mut self.panel_dib.canvas(), &panel.texts);
            blit(self.panel, &self.panel_dib);
        }
    }

    fn hotkey(&mut self, id: i32) {
        match id {
            HK_TOGGLE_VIS => {
                self.visible = !self.visible;
                let show = |hwnd, on| unsafe {
                    let _ = ShowWindow(hwnd, if on { SW_SHOWNA } else { SW_HIDE });
                };
                show(self.hud, self.visible);
                show(self.panel, self.visible && self.open);
            }
            HK_TOGGLE_CFG => {
                if !self.visible {
                    self.visible = true;
                    unsafe {
                        let _ = ShowWindow(self.hud, SW_SHOWNA);
                    }
                }
                self.set_settings_mode(!self.open);
            }
            HK_GAME => {
                let pid = if self.capture.status().pid != 0 { 0 } else { foreground_pid() };
                self.capture.set_process(pid);
            }
            HK_QUIT => {
                self.save();
                unsafe { PostQuitMessage(0) }
            }
            _ => {}
        }
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let which = if hwnd.0 as isize == PANEL_HWND.load(Ordering::Relaxed) { Which::Panel } else { Which::Hud };
    // The panel always takes clicks; the HUD only while it's draggable.
    let interactive = which == Which::Panel || HUD_INTERACTIVE.load(Ordering::Relaxed);
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    let push = |ev| EVENTS.with(|q| q.borrow_mut().push(ev));
    unsafe {
        match msg {
            WM_NCHITTEST => LRESULT(if interactive { HTCLIENT as isize } else { HTTRANSPARENT as isize }),
            WM_LBUTTONDOWN if interactive => {
                push(Ev::Down(which, x, y));
                LRESULT(0)
            }
            WM_MOUSEMOVE if interactive && (wparam.0 & 0x0001) != 0 => { // MK_LBUTTON
                push(Ev::Move(x));
                LRESULT(0)
            }
            WM_LBUTTONUP if interactive => {
                push(Ev::Up);
                LRESULT(0)
            }
            WM_SIZE => LRESULT(0), // size comes from the DIB (see `blit`)
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// A compact layered popup: topmost, no taskbar button, no shadow.
unsafe fn create_overlay(class: PCWSTR, instance: HINSTANCE, (x, y): (i32, i32), (w, h): (i32, i32), click_through: bool) -> windows::core::Result<HWND> {
    let mut ex = WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW;
    if click_through {
        ex |= WS_EX_TRANSPARENT;
    }
    let hwnd = CreateWindowExW(ex, class, w!("DeafOverlay"), WS_POPUP, x, y, w, h, None, None, Some(instance), None)?;
    let no_shadow = 0i32; // DWMWA_NCRENDERING_POLICY = DWMNCRP_USEWINDOWSTYLE
    let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(2), (&no_shadow as *const i32).cast(), 4);
    Ok(hwnd)
}

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        register_fonts();
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("DeafOverlayWnd");
        RegisterClassW(&WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });

        // Saved position, else just below the crosshair (research: keep cues near the centre).
        let (settings, saved) = persist::load();
        let (screen_w, screen_h) = screen();
        let (hw, hh) = hud::size(&settings.hud);
        let (hw, hh) = (hw as i32, hh as i32);
        let win = saved.hud.unwrap_or(((screen_w - hw) / 2, screen_h * 62 / 100 - hh / 2));
        let hud_hwnd = create_overlay(class, instance, win, (hw, hh), true)?;
        let panel_hwnd = create_overlay(class, instance, (0, 0), (PANEL_W, PANEL_H), false)?;
        PANEL_HWND.store(panel_hwnd.0 as isize, Ordering::Relaxed);
        let _ = ShowWindow(hud_hwnd, SW_SHOWNA);

        for (id, key) in [(HK_TOGGLE_VIS, 'H'), (HK_TOGGLE_CFG, 'I'), (HK_QUIT, 'Q'), (HK_GAME, 'G')] {
            let _ = RegisterHotKey(Some(hud_hwnd), id, MOD_ALT | MOD_NOREPEAT, key as u32);
        }

        let mut app = App {
            hud: hud_hwnd,
            panel: panel_hwnd,
            hud_dib: Dib::new(hw as usize, hh as usize)?,
            panel_dib: Dib::new(PANEL_W as usize, PANEL_H as usize)?,
            text: TextRenderer::new()?,
            capture: Capture::start(),
            tone: None,
            norm: Normalizer::default(),
            settings,
            view: View::Display,
            help: None,
            open: false,
            visible: true,
            drag: Drag::None,
            win,
            panel_pos: saved.panel,
            proc_name: (0, String::new()),
            store: store::Store::start(panel_hwnd),
        };
        app.place_hud(); // clamps a stale position on-screen, and centres crosshair mode
        app.push_dsp_settings();
        if first_run() {
            app.view = View::Setup;
            app.set_settings_mode(true);
        }
        let mut last = Instant::now();

        'run: loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                if msg.message == WM_HOTKEY {
                    app.hotkey(msg.wParam.0 as i32);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                for ev in EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut())) {
                    app.handle(ev);
                }
            }

            let now = Instant::now();
            let dt = (now - last).as_secs_f32().clamp(0.001, 0.05);
            last = now;
            app.frame(dt);
            std::thread::sleep(Duration::from_millis(16));
        }

        for id in [HK_TOGGLE_VIS, HK_TOGGLE_CFG, HK_QUIT, HK_GAME] {
            let _ = UnregisterHotKey(Some(hud_hwnd), id);
        }
    }
    Ok(())
}

/// v1 `show_first_run_guidance`: true once, then marks onboarding done.
fn first_run() -> bool {
    const KEY: PCWSTR = w!(r"Software\DeafOverlay");
    const VALUE: PCWSTR = w!("OnboardingDone");
    let mut done = 0u32;
    let mut size = 4u32;
    unsafe {
        let read = RegGetValueW(HKEY_CURRENT_USER, KEY, VALUE, RRF_RT_REG_DWORD, None, Some((&mut done as *mut u32).cast()), Some(&mut size));
        if read.is_ok() && done == 1 {
            return false;
        }
        let one = 1u32;
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, KEY, VALUE, REG_DWORD.0, Some((&one as *const u32).cast()), 4);
    }
    true
}

/// Open a web page or an `ms-settings:` page in its default handler.
fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Exe file name of `pid` for display (v1 `update_capture_name`).
fn process_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "(game)".into();
        };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return "(game)".into();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }
}

/// PID of the focused window, skipping ourselves (v1 Alt+G).
fn foreground_pid() -> u32 {
    let mut pid = 0u32;
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.is_invalid() {
            GetWindowThreadProcessId(fg, Some(&mut pid));
        }
        if pid == GetCurrentProcessId() {
            return 0;
        }
    }
    pid
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role must resolve to the embedded Geist, not silently fall back to a system font.
    #[test]
    fn embedded_geist_faces_resolve() {
        register_fonts();
        let dib = Dib::new(8, 8).unwrap();
        for f in [Font::Regular, Font::Medium, Font::SemiBold, Font::Mono, Font::MonoMedium] {
            let (name, weight) = face(f);
            let mut buf = [0u16; 64];
            let got = unsafe {
                let font = CreateFontW(-12, 0, 0, 0, weight.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, name);
                let old = SelectObject(dib.dc, font.into());
                let n = GetTextFaceW(dib.dc, Some(&mut buf));
                SelectObject(dib.dc, old);
                let _ = DeleteObject(font.into());
                String::from_utf16_lossy(&buf[..n.max(1) as usize - 1])
            };
            assert_eq!(got, unsafe { name.to_string().unwrap() }, "{f:?}");
        }
    }
}
