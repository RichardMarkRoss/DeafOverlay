//! DeafOverlay window: a compact, click-through, always-on-top layered popup that draws the HUD,
//! growing downward to show the settings panel. A port of v1 `wWinMain` / `WndProc` /
//! `set_settings_mode` / `blit_layered`.
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

use capture::{Capture, Tone};
use deafoverlay::hud::{self, Canvas, Normalizer, HUD_AREA_H, HUD_W};
use deafoverlay::persist;
use deafoverlay::ui::{self, Action, Settings, SetupInfo, View, WIN_H_SETTINGS};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
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

const WIN_H_NORMAL: usize = HUD_AREA_H;
const HK_TOGGLE_VIS: i32 = 1;
const HK_TOGGLE_CFG: i32 = 2;
const HK_QUIT: i32 = 3;
const HK_GAME: i32 = 4;

/// Read by `wnd_proc` for hit-testing (v1 `gSettingsOpen`).
static SETTINGS_OPEN: AtomicBool = AtomicBool::new(false);

/// Mouse input queued by `wnd_proc` and handled in the main loop, which owns the app state.
/// Info popups and window moves run modal loops that re-enter `wnd_proc`, so handling input
/// in there would mean re-entrant access to that state.
#[derive(Clone, Copy)]
enum Ev {
    Down(i32, i32),
    Move(i32, i32),
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
        Canvas { w: self.w, h: self.h, px }
    }

    /// v1 `text_out_opaque`: GDI text, then force the glyph box opaque (GDI leaves alpha at 0).
    fn text(&mut self, x: i32, y: i32, s: &str, rgb: (u8, u8, u8)) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let size = self.measure(s);
        unsafe {
            SetTextColor(self.dc, COLORREF(rgb.0 as u32 | (rgb.1 as u32) << 8 | (rgb.2 as u32) << 16));
            let _ = TextOutW(self.dc, x, y, &wide);
            let _ = GdiFlush(); // GDI batches; flush before touching the bits directly
        }
        self.canvas().force_alpha(x - 2, y - 2, x + size.cx + 2, y + size.cy + 2);
    }

    fn measure(&self, s: &str) -> SIZE {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE::default();
        unsafe {
            let _ = GetTextExtentPoint32W(self.dc, &wide, &mut size);
        }
        size
    }

    fn draw_texts(&mut self, texts: &[ui::Text]) {
        for t in texts {
            let (x, y) = match t.center_in {
                // v1 `draw_button` label placement
                Some((x0, y0, x1, y1)) => {
                    let sz = self.measure(&t.s);
                    (x0 + ((x1 - x0 - sz.cx) / 2).max(6), y0 + ((y1 - y0 - sz.cy) / 2).max(2))
                }
                None => (t.x, t.y),
            };
            self.text(x, y, &t.s, t.rgb);
        }
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
    Sphere,
}

struct App {
    hwnd: HWND,
    dib: Dib,
    capture: Capture,
    tone: Option<Tone>,
    norm: Normalizer,
    settings: Settings,
    view: View,
    open: bool,
    visible: bool,
    drag: Drag,
    /// HUD top-left on screen; the settings window may be nudged up to fit, then restored.
    win: (i32, i32),
    /// (pid, exe name) for the Setup view's "Capturing:" line.
    proc_name: (u32, String),
}

impl App {
    fn set_settings_mode(&mut self, on: bool) {
        self.open = on;
        SETTINGS_OPEN.store(on, Ordering::Relaxed);
        if !on {
            self.save();
            self.view = View::Controls;
            self.drag = Drag::None;
            self.tone = None; // stops the self-test
        }
        let h = if on { WIN_H_SETTINGS } else { WIN_H_NORMAL };
        if let Ok(d) = Dib::new(HUD_W, h) {
            self.dib = d;
        }
        unsafe {
            let mut ex = GetWindowLongW(self.hwnd, GWL_EXSTYLE);
            if on {
                ex &= !(WS_EX_TRANSPARENT.0 as i32);
            } else {
                ex |= WS_EX_TRANSPARENT.0 as i32;
                let _ = ReleaseCapture();
            }
            SetWindowLongW(self.hwnd, GWL_EXSTYLE, ex);
            // Keep the taller settings window on-screen; the HUD position itself is preserved.
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let mut top = self.win.1;
            if on && top + WIN_H_SETTINGS as i32 > screen_h {
                top = (screen_h - WIN_H_SETTINGS as i32).max(0);
            }
            let flags = SWP_NOSIZE | SWP_FRAMECHANGED | SWP_NOACTIVATE;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), self.win.0, top, 0, 0, flags);
        }
    }

    fn push_dsp_settings(&self) {
        self.capture.set_swap_lr(self.settings.swap_lr);
        self.capture.set_band_gains(self.settings.band_gain);
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
        }
    }

    fn handle(&mut self, ev: Ev) {
        if !self.open {
            return;
        }
        match ev {
            Ev::Down(x, y) => self.mouse_down(x, y),
            Ev::Move(x, y) => match self.drag {
                Drag::Slider { i, x0, x1 } => {
                    self.settings.set_slider(i, (x - x0) as f32 / (x1 - x0) as f32);
                    self.settings_changed();
                }
                Drag::Sphere => {
                    self.settings.layout.sphere_x = (x as f32 / HUD_W as f32).clamp(0.05, 0.95);
                    self.settings.layout.sphere_y = (y as f32 / HUD_AREA_H as f32).clamp(0.05, 0.95);
                }
                Drag::None => {}
            },
            Ev::Up => {
                if !matches!(self.drag, Drag::None) {
                    self.save(); // v1 saved after every slider/sphere drag
                }
                self.drag = Drag::None;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
        }
    }

    fn settings_changed(&mut self) {
        self.push_dsp_settings();
    }

    /// Best effort, like v1: a failed save must never take the overlay down.
    fn save(&self) {
        let _ = persist::save(&self.settings, self.win);
    }

    fn mouse_down(&mut self, x: i32, y: i32) {
        let info = self.setup_info();
        let panel = ui::panel(None, self.view, &self.settings, &info);
        if let Some(action) = panel.hit(x, y) {
            match action {
                Action::Info(text) => unsafe {
                    let _ = MessageBoxW(Some(self.hwnd), &HSTRING::from(text), w!("Overlay Setting Info"), MB_OK | MB_ICONINFORMATION);
                },
                Action::Slider(i) => {
                    let (x0, x1) = panel.slider_span(i).unwrap_or((0, 1));
                    self.drag = Drag::Slider { i, x0, x1 };
                    unsafe { SetCapture(self.hwnd) };
                    self.handle(Ev::Move(x, y));
                }
                Action::SwapLr => {
                    self.settings.swap_lr = !self.settings.swap_lr;
                    self.settings_changed();
                    self.save();
                }
                Action::Preset(p) => {
                    self.settings.toggle_preset(p);
                    self.settings_changed();
                    self.save();
                }
                Action::OpenSoundSettings => open_uri(w!("ms-settings:sound")),
                Action::OpenSpatialSettings => open_uri(w!("ms-settings:sound-defaultoutputproperties")),
                Action::SelfTest => {
                    if self.tone.is_none() {
                        self.tone = Some(Tone::start());
                    }
                }
                Action::Show(v) => self.view = v,
            }
            return;
        }

        // The sphere: drag to fine-position it within the HUD area.
        let (cx, cy, r) = self.settings.layout.sphere();
        let (dx, dy) = (x as f32 - cx, y as f32 - cy);
        if y < HUD_AREA_H as i32 && dx * dx + dy * dy <= r * r * 1.3 {
            self.drag = Drag::Sphere;
            unsafe { SetCapture(self.hwnd) };
            return;
        }

        // Anywhere else: move the whole window through the system move loop, so the layered
        // window follows the cursor cleanly. It returns when the button is released.
        unsafe {
            let _ = ReleaseCapture();
            SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
            let mut rc = RECT::default();
            if GetWindowRect(self.hwnd, &mut rc).is_ok() {
                self.win = (rc.left, rc.top);
            }
            self.save();
        }
    }

    fn frame(&mut self, dt: f32) {
        if self.tone.as_ref().is_some_and(Tone::finished) {
            self.tone = None;
        }
        let st = self.capture.status();
        let peaks = self.norm.peaks(&self.capture.sources(), st.full_360, self.settings.sensitivity, dt);
        let lay = self.settings.layout;
        hud::draw(&mut self.dib.canvas(), &lay, &peaks, self.open);

        // "FRONT" above the ring's top so forward is always clear (top = front, bottom = behind).
        let (rcx, rcy, _, rb) = lay.ring();
        let sz = self.dib.measure("FRONT");
        self.dib.text(rcx.round() as i32 - sz.cx / 2, (rcy - rb).round() as i32 - sz.cy - 3, "FRONT", (150, 235, 170));

        if self.open {
            let (tx, ty) = hud::draw_hint_box(&mut self.dib.canvas());
            self.dib.text(tx, ty, "DeafOverlay Hotkeys", (255, 255, 255));
            self.dib.text(tx, ty + 20, "Alt+H: Show/Hide   Alt+I: Settings   Alt+Q: Quit", (255, 255, 255));
            self.dib.text(tx, ty + 38, "Best accuracy: 7.1 or Stereo, Spatial OFF", (255, 255, 255));

            let info = self.setup_info();
            let panel = ui::panel(Some(&mut self.dib.canvas()), self.view, &self.settings, &info);
            self.dib.draw_texts(&panel.texts);
        }
        blit(self.hwnd, &self.dib);
    }

    fn hotkey(&mut self, id: i32) {
        match id {
            HK_TOGGLE_VIS => {
                self.visible = !self.visible;
                unsafe {
                    let _ = ShowWindow(self.hwnd, if self.visible { SW_SHOWNA } else { SW_HIDE });
                }
            }
            HK_TOGGLE_CFG => {
                if !self.visible {
                    self.visible = true;
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_SHOWNA);
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
    let settings = SETTINGS_OPEN.load(Ordering::Relaxed);
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    let push = |ev| EVENTS.with(|q| q.borrow_mut().push(ev));
    unsafe {
        match msg {
            WM_NCHITTEST => LRESULT(if settings { HTCLIENT as isize } else { HTTRANSPARENT as isize }),
            WM_LBUTTONDOWN if settings => {
                push(Ev::Down(x, y));
                LRESULT(0)
            }
            WM_MOUSEMOVE if settings && (wparam.0 & 0x0001) != 0 => { // MK_LBUTTON
                push(Ev::Move(x, y));
                LRESULT(0)
            }
            WM_LBUTTONUP if settings => {
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

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("DeafOverlayWnd");
        RegisterClassW(&WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });

        // Saved position, else upper-right like v1. Clamp so a stale position (resolution change,
        // unplugged monitor) can't strand the window off-screen.
        let (settings, saved_win) = persist::load();
        let (screen_w, screen_h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let (wx, wy) = saved_win.unwrap_or((screen_w - HUD_W as i32 - 40, 80));
        let win = (wx.clamp(0, (screen_w - HUD_W as i32).max(0)), wy.clamp(0, (screen_h - WIN_H_NORMAL as i32).max(0)));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            class,
            w!("DeafOverlay"),
            WS_POPUP,
            win.0,
            win.1,
            HUD_W as i32,
            WIN_H_NORMAL as i32,
            None,
            None,
            Some(instance),
            None,
        )?;
        let no_shadow = 0i32; // DWMWA_NCRENDERING_POLICY = DWMNCRP_USEWINDOWSTYLE
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(2), (&no_shadow as *const i32).cast(), 4);
        let _ = ShowWindow(hwnd, SW_SHOWNA);

        for (id, key) in [(HK_TOGGLE_VIS, 'H'), (HK_TOGGLE_CFG, 'I'), (HK_QUIT, 'Q'), (HK_GAME, 'G')] {
            let _ = RegisterHotKey(Some(hwnd), id, MOD_ALT | MOD_NOREPEAT, key as u32);
        }

        let mut app = App {
            hwnd,
            dib: Dib::new(HUD_W, WIN_H_NORMAL)?,
            capture: Capture::start(),
            tone: None,
            norm: Normalizer::default(),
            settings,
            view: View::Controls,
            open: false,
            visible: true,
            drag: Drag::None,
            win,
            proc_name: (0, String::new()),
        };
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
            let _ = UnregisterHotKey(Some(hwnd), id);
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

fn open_uri(uri: PCWSTR) {
    unsafe {
        ShellExecuteW(None, w!("open"), uri, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
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
