// DeafOverlay.cpp — “Collect everything” spatial logic.
// - Every band emits up to 3 peaks (Left, Front, Right).
// - No mono suppression (center shows at 12 o’clock).
// - Continuous azimuth for L/R from ILD per band.
// - Smooth visuals kept (same renderer), rear/top hints kept.

#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <windowsx.h>
#include <dwmapi.h>
#include <atomic>
#include <chrono>
#include <cmath>
#include <algorithm>
#include <cstring>

#pragma comment(lib, "Dwmapi.lib")

/* ===== Tunables ===== */
static constexpr float SENSITY_GAIN = 0.95f;   // overall visual loudness
static constexpr float MAX_AZIMUTH_DEG = 78.0f;   // per-side span on ring
static constexpr float PEAK_DECAY_HZ = 1.7f;    // slower decay so peaks persist
static constexpr float ELEV_SCALE = 0.30f;   // sphere top/bottom bias
static constexpr float REAR_SCALE = 1.00f;   // rear trail strength
static constexpr bool  SWAP_LR = false;   // flip if headset reversed

/* ===== Audio (miniaudio) ===== */
#define MINIAUDIO_IMPLEMENTATION
#define MA_NO_DECODING
#define MA_NO_ENCODING
#define MA_NO_RESOURCE_MANAGER
#include "third_party/miniaudio.h"

static ma_context g_ctx{};
static ma_device  g_dev{};
static bool g_audioOK = false;

/* ===== DIB / drawing ===== */
struct DIB { HBITMAP bmp{}; HDC dc{}; void* bits{}; int w{}, h{}; };
static DIB make_dib(int w, int h) {
    DIB d{}; d.w = w; d.h = h;
    BITMAPINFO bmi{}; bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmi.bmiHeader.biWidth = w; bmi.bmiHeader.biHeight = -h;
    bmi.bmiHeader.biPlanes = 1; bmi.bmiHeader.biBitCount = 32; bmi.bmiHeader.biCompression = BI_RGB;
    HDC s = GetDC(nullptr); d.dc = CreateCompatibleDC(s);
    d.bmp = CreateDIBSection(d.dc, &bmi, DIB_RGB_COLORS, &d.bits, nullptr, 0);
    SelectObject(d.dc, d.bmp); ReleaseDC(nullptr, s); return d;
}
static void free_dib(DIB& d) { if (d.dc) DeleteDC(d.dc); if (d.bmp) DeleteObject(d.bmp); d = {}; }
static inline void putpx_PM(void* bits, int w, int h, int x, int y, BYTE a, BYTE r, BYTE g, BYTE b) {
    if ((unsigned)x >= (unsigned)w || (unsigned)y >= (unsigned)h) return;
    BYTE* p = ((BYTE*)bits) + ((size_t)y * w + x) * 4;
    p[0] = (BYTE)((b * a + 127) / 255); p[1] = (BYTE)((g * a + 127) / 255); p[2] = (BYTE)((r * a + 127) / 255); p[3] = a;
}
static void blit_layered(HWND hWnd, HDC src, int w, int h) {
    POINT pos{ 0,0 }; SIZE sz{ w,h }; POINT srcpt{ 0,0 };
    BLENDFUNCTION bf{}; bf.BlendOp = AC_SRC_OVER; bf.SourceConstantAlpha = 255; bf.AlphaFormat = AC_SRC_ALPHA;
    UpdateLayeredWindow(hWnd, nullptr, &pos, &sz, src, &srcpt, 0, &bf, ULW_ALPHA);
}

/* ===== Band energies (per ear) =====
 * B0:[0–250], B1:(250–2k], B2:(2–5k], B3:(5–8k], B4:(8–12k], B5:(>12k]
 */
static constexpr int NB = 6;
static std::atomic<float> gEL[NB], gER[NB];

static inline float lp_step(float y, float x, float a) { return y + a * (x - y); }

void MA_API audio_cb(ma_device* dev, void*, const void* pIn, ma_uint32 frames) {
    const float* s = (const float*)pIn; if (!s) return;
    const ma_uint32 ch = dev->capture.channels ? dev->capture.channels : 1;

    auto a_fc = [&](float fc) { return 1.0f - std::exp(-6.2831853f * fc / (float)dev->sampleRate); };
    const float a250 = a_fc(250.f), a2k = a_fc(2000.f), a5k = a_fc(5000.f), a8k = a_fc(8000.f), a12k = a_fc(12000.f);
    const float aTot = 0.07f;

    float lp250L = 0, lp2kL = 0, lp5kL = 0, lp8kL = 0, lp12kL = 0, totL = 0;
    float lp250R = 0, lp2kR = 0, lp5kR = 0, lp8kR = 0, lp12kR = 0, totR = 0;

    for (ma_uint32 i = 0; i < frames; ++i) {
        float L = s[i * ch + 0], R = (ch > 1) ? s[i * ch + 1] : L;
        float eL = L * L, eR = R * R;

        lp250L = lp_step(lp250L, eL, a250); lp2kL = lp_step(lp2kL, eL, a2k);
        lp5kL = lp_step(lp5kL, eL, a5k);   lp8kL = lp_step(lp8kL, eL, a8k);
        lp12kL = lp_step(lp12kL, eL, a12k); totL = lp_step(totL, eL, aTot);

        lp250R = lp_step(lp250R, eR, a250); lp2kR = lp_step(lp2kR, eR, a2k);
        lp5kR = lp_step(lp5kR, eR, a5k);   lp8kR = lp_step(lp8kR, eR, a8k);
        lp12kR = lp_step(lp12kR, eR, a12k); totR = lp_step(totR, eR, aTot);
    }

    float bL[NB], bR[NB];
    bL[0] = lp250L;
    bL[1] = std::max(0.f, lp2kL - lp250L);
    bL[2] = std::max(0.f, lp5kL - lp2kL);
    bL[3] = std::max(0.f, lp8kL - lp5kL);
    bL[4] = std::max(0.f, lp12kL - lp8kL);
    bL[5] = std::max(0.f, totL - lp12kL);

    bR[0] = lp250R;
    bR[1] = std::max(0.f, lp2kR - lp250R);
    bR[2] = std::max(0.f, lp5kR - lp2kR);
    bR[3] = std::max(0.f, lp8kR - lp5kR);
    bR[4] = std::max(0.f, lp12kR - lp8kR);
    bR[5] = std::max(0.f, totR - lp12kR);

    // bring into nice range; slight low lift for quiet footsteps
    const float g = 1.8f * SENSITY_GAIN;
    bL[0] *= 1.25f; bR[0] *= 1.25f;
    bL[1] *= 1.10f; bR[1] *= 1.10f;

    for (int k = 0; k < NB; ++k) {
        float eL = g * bL[k], eR = g * bR[k];
        auto upd = [&](std::atomic<float>& a, float v) { float p = a.load(std::memory_order_relaxed); a.store(p + 0.22f * (v - p), std::memory_order_relaxed); };
        upd(gEL[k], eL); upd(gER[k], eR);
    }
}

/* ===== Device ===== */
static bool audio_init() {
    if (ma_context_init(nullptr, 0, nullptr, &g_ctx) != MA_SUCCESS) return false;
    ma_device_config cfg = ma_device_config_init(ma_device_type_loopback);
    cfg.capture.format = ma_format_f32; cfg.capture.channels = 2; cfg.sampleRate = 48000;
    cfg.performanceProfile = ma_performance_profile_low_latency;
    cfg.dataCallback = audio_cb;
    if (ma_device_init(&g_ctx, &cfg, &g_dev) != MA_SUCCESS) { ma_context_uninit(&g_ctx); return false; }
    if (ma_device_start(&g_dev) != MA_SUCCESS) { ma_device_uninit(&g_dev); ma_context_uninit(&g_ctx); return false; }
    return g_audioOK = true;
}
static void audio_shutdown() { if (!g_audioOK) return; ma_device_uninit(&g_dev); ma_context_uninit(&g_ctx); g_audioOK = false; }

/* ===== Peaks & rendering ===== */
constexpr float PI_F = 3.14159265358979323846f;

struct Peak {
    float ang = 0.f;   // 0 front (12 o’clock), +right, -left
    float amp = 0.f;   // 0..1
    float life = 0.f;  // seconds
    int   band = 0;
    float rearGain = 0.f;
    float elevFactor = 1.f;
};
static Peak g_peaks[18]; // up to 3 per band * 6 bands

static inline void level_to_color(float s, BYTE& r, BYTE& g, BYTE& b) {
    r = 255;
    g = (BYTE)std::clamp(255.0f - s * 255.0f * 0.6f, 0.0f, 255.0f);
    b = (BYTE)std::clamp(255.0f - s * 255.0f, 0.0f, 255.0f);
}
static void add_peak_merge(const Peak& q) {
    int best = -1; float bestD = 1e9f;
    for (int i = 0; i < (int)std::size(g_peaks); ++i) if (g_peaks[i].life > 0.f) {
        float d = std::fabs(std::remainder(q.ang - g_peaks[i].ang, 2 * PI_F));
        if (d < bestD) { bestD = d; best = i; }
    }
    if (best != -1 && bestD < (14.f * PI_F / 180.f)) {
        auto& p = g_peaks[best];
        float sA = p.amp, sB = q.amp;
        float sx = std::sinf(p.ang) * sA + std::sinf(q.ang) * sB;
        float sy = -std::cosf(p.ang) * sA + -std::cosf(q.ang) * sB;
        p.ang = std::atan2f(sx, -sy);
        p.amp = std::clamp(sA + 0.65f * sB, 0.f, 1.f);
        p.rearGain = std::max(p.rearGain, q.rearGain);
        p.elevFactor = 0.5f * (p.elevFactor + q.elevFactor);
        if (sB > sA) p.band = q.band;
        p.life = std::max(p.life, q.life);
        return;
    }
    // insert/replace lowest
    int slot = 0; for (int i = 1; i < (int)std::size(g_peaks); ++i) if (g_peaks[i].amp < g_peaks[slot].amp) slot = i;
    g_peaks[slot] = q;
}

/* ==== Sphere & Ring (same visuals as before) ==== */
static void draw_sphere(void* bits, int w, int h, float cx, float cy, float radius, float tilt_deg);
static void draw_ring(void* bits, int w, int h, float cx, float cy, float a, float b);

static inline void base_ring_pixel(void* bits, int w, int h, int x, int y, float t) {
    putpx_PM(bits, w, h, x, y, (BYTE)(t * 150), 255, 255, 255);
}

static void draw_ring(void* bits, int w, int h, float cx, float cy, float a, float b) {
    const float th_px = std::max(1.f, a * 0.06f);
    for (int y = 0; y < h; ++y) for (int x = 0; x < w; ++x) {
        float dx = x - cx, dy = y - cy;
        float r = std::sqrt((dx * dx) / (a * a) + (dy * dy) / (b * b));
        float d = std::fabs(r - 1.f);
        float t = 1.f - std::min(1.f, d / (th_px / a));
        if (t <= 0.f) continue;
        base_ring_pixel(bits, w, h, x, y, t);

        // composite peaks
        float ang = std::atan2((dx / a), (-dy / b)); // 0 up
        float bestGlow = 0.f, bestAmp = 0.f;
        for (const auto& p : g_peaks) if (p.life > 0.f) {
            float dist = std::fabs(std::remainder(ang - p.ang, 2 * PI_F));
            float hot = std::max(0.f, 1.f - dist / 0.9f);
            float win = (1.f - std::cosf(hot * PI_F)) * 0.5f;
            bestGlow = std::max(bestGlow, win * p.amp);
            bestAmp = std::max(bestAmp, p.amp);
        }
        if (bestGlow > 0.f) {
            BYTE rr, gg, bb; level_to_color(bestAmp, rr, gg, bb);
            putpx_PM(bits, w, h, x, y, (BYTE)std::clamp(int(bestGlow * 230), 0, 230), rr, gg, bb);
        }
    }
    // little arrowheads at each peak
    for (const auto& p : g_peaks) if (p.life > 0.f) {
        float th = p.ang; BYTE rr, gg, bb; level_to_color(p.amp, rr, gg, bb);
        float ex = cx + a * std::sinf(th), ey = cy - b * std::cosf(th);
        float nx = -b * std::cosf(th), ny = -a * std::sinf(th);
        float nlen = std::max(1.f, std::sqrt(nx * nx + ny * ny)); nx /= nlen; ny /= nlen;
        float tx = std::cosf(th), ty = std::sinf(th);
        for (int i = -6; i <= 6; ++i) for (int j = 0; j < 8; ++j) {
            int xi = int(ex + nx * i + (a * 0.04f) * tx * j);
            int yi = int(ey + ny * i + (b * 0.04f) * ty * j);
            putpx_PM(bits, w, h, xi, yi, (BYTE)(120 + 120 * p.amp), rr, gg, bb);
        }
    }
}

static void draw_sphere(void* bits, int w, int h, float cx, float cy, float radius, float tilt_deg) {
    const float tilt = tilt_deg * PI_F / 180.f; const float ct = std::cos(tilt), st = std::sin(tilt);
    const int latSteps = 16, lonSteps = 36;

    for (int i = 1; i < latSteps; ++i) {
        float v = float(i) / latSteps, phi = v * PI_F - PI_F * 0.5f;
        float sy = std::sin(phi), cr = std::cos(phi);

        for (int j = 0; j < lonSteps; ++j) {
            float u = float(j) / lonSteps, th = u * 2.f * PI_F;
            float x = cr * std::cos(th), y = sy, z = cr * std::sin(th);

            float y2 = ct * y - st * z;
            float z2 = st * y + ct * z;

            float denom = 1.f + 0.55f * z2;
            float xp = (x / denom) * radius + cx;
            float yp = (y2 / denom) * radius + cy;

            float vx = xp - cx, vy = yp - cy; float vlen = std::max(1.0f, std::sqrt(vx * vx + vy * vy));

            float alignBest = 0.f, ampBest = 0.f;
            for (const auto& p : g_peaks) if (p.life > 0.f) {
                float dirx = std::sinf(p.ang), diry = -std::cosf(p.ang);
                float align = (vx * dirx + vy * diry) / vlen;
                float contrib = align * p.amp;
                if (contrib > alignBest) { alignBest = contrib; ampBest = p.amp; }
                else ampBest = std::max(ampBest, p.amp);
            }

            float grow = 1.0f + 0.22f * std::max(0.0f, alignBest);
            int   dotR = std::max(2, int(radius * 0.012f * grow));
            float depth = (z2 * 0.5f + 0.5f);
            float bright = (0.55f + 0.55f * depth + 0.95f * std::max(0.0f, alignBest));

            BYTE rr, gg, bb; level_to_color(ampBest, rr, gg, bb);
            BYTE a = (BYTE)std::clamp(int(24 + 200 * bright), 0, 238);
            for (int dy = -dotR; dy <= dotR; ++dy)
                for (int dx = -dotR; dx <= dotR; ++dx)
                    if (dx * dx + dy * dy <= dotR * dotR)
                        putpx_PM(bits, w, h, int(std::round(xp)) + dx, int(std::round(yp)) + dy, a, rr, gg, bb);
        }
    }

    // forward rays + rear accent
    for (const auto& p : g_peaks) if (p.life > 0.f) {
        BYTE rr, gg, bb; level_to_color(p.amp, rr, gg, bb);
        float fx = std::sinf(p.ang), fy = -std::cosf(p.ang);
        int len = int(radius * 0.9f);
        for (int i = 0; i < len; ++i) putpx_PM(bits, w, h, int(cx + fx * i), int(cy + fy * i), (BYTE)(40 + 150 * p.amp), rr, gg, bb);

        if (p.rearGain > 0.f) {
            float rx = std::sinf(p.ang + PI_F), ry = -std::cosf(p.ang + PI_F);
            int rlen = int(radius * 0.55f);
            BYTE a = (BYTE)std::clamp(int((20 + 160 * p.rearGain) * REAR_SCALE), 0, 255);
            for (int i = 0; i < rlen; ++i) putpx_PM(bits, w, h, int(cx + rx * i), int(cy + ry * i), a, 200, 220, 255);
        }
    }
}

/* ===== Window stuff (unchanged essentials) ===== */
static bool g_editMode = false; static DIB g_dib{};
static const wchar_t* kClassName = L"DeafOverlayWnd";
enum : int { HK_TOGGLE_VIS = 1, HK_OPEN_SETTINGS = 2 };

enum MenuIds { kMID_ToggleEdit = 1, kMID_ResetSize, kMID_Quit };
static LRESULT edit_hit_test(HWND h, LPARAM lp) {
    const int grip = 8; POINT pt{ GET_X_LPARAM(lp),GET_Y_LPARAM(lp) }; RECT r; GetWindowRect(h, &r);
    int x = pt.x - r.left, y = pt.y - r.top, w = r.right - r.left, ht = r.bottom - r.top;
    bool L = x < grip, R = x >= w - grip, T = y < grip, B = y >= ht - grip;
    if (T && L) return HTTOPLEFT; if (T && R) return HTTOPRIGHT; if (B && L) return HTBOTTOMLEFT; if (B && R) return HTBOTTOMRIGHT;
    if (L) return HTLEFT; if (R) return HTRIGHT; if (T) return HTTOP; if (B) return HTBOTTOM; return HTCAPTION;
}
static void enter_edit(HWND h) {
    if (g_editMode) return; g_editMode = true;
    LONG ex = GetWindowLongW(h, GWL_EXSTYLE); ex &= ~WS_EX_TRANSPARENT; SetWindowLongW(h, GWL_EXSTYLE, ex);
    LONG st = GetWindowLongW(h, GWL_STYLE); st |= WS_THICKFRAME; SetWindowLongW(h, GWL_STYLE, st);
    SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}
static void exit_edit(HWND h) {
    if (!g_editMode) return; g_editMode = false;
    LONG ex = GetWindowLongW(h, GWL_EXSTYLE); ex |= WS_EX_TRANSPARENT; SetWindowLongW(h, GWL_EXSTYLE, ex);
    LONG st = GetWindowLongW(h, GWL_STYLE); st &= ~WS_THICKFRAME; SetWindowLongW(h, GWL_STYLE, st);
    SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}
static void toggle_edit(HWND h) { if (g_editMode) exit_edit(h); else enter_edit(h); }

LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM wParam, LPARAM lParam) {
    switch (m) {
    case WM_NCHITTEST:
        if (g_editMode) return edit_hit_test(h, lParam);
        if (GetKeyState(VK_MENU) & 0x8000) return HTCAPTION;
        return HTTRANSPARENT;
    case WM_RBUTTONUP: {
        HMENU menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, kMID_ToggleEdit, g_editMode ? L"Exit Edit Mode" : L"Enter Edit Mode");
        AppendMenuW(menu, MF_STRING, kMID_ResetSize, L"Reset Size (420×420)");
        AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
        AppendMenuW(menu, MF_STRING, kMID_Quit, L"Quit");
        POINT p; GetCursorPos(&p);
        UINT id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, h, nullptr);
        DestroyMenu(menu);
        if (id == kMID_ToggleEdit) toggle_edit(h);
        else if (id == kMID_ResetSize) { SetWindowPos(h, HWND_TOPMOST, 0, 0, 420, 420, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE); PostMessageW(h, WM_SIZE, 0, MAKELPARAM(420, 420)); }
        else if (id == kMID_Quit) DestroyWindow(h);
        return 0;
    }
    case WM_SIZE: { static DIB& d = g_dib; int ww = LOWORD(lParam), hh = HIWORD(lParam); if (ww > 0 && hh > 0) { free_dib(d); d = make_dib(ww, hh); } return 0; }
    case WM_DESTROY: PostQuitMessage(0); return 0;
    default: return DefWindowProcW(h, m, wParam, lParam);
    }
}

/* ===== Entry / main loop ===== */
int APIENTRY wWinMain(HINSTANCE inst, HINSTANCE, LPWSTR, int) {
    using SetProcessDpiAwarenessContext_t = BOOL(WINAPI*)(HANDLE);
    if (auto setCtx = (SetProcessDpiAwarenessContext_t)GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetProcessDpiAwarenessContext"))
        setCtx((HANDLE)-4);

    WNDCLASSW wc{}; wc.hInstance = inst; wc.lpfnWndProc = WndProc; wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW); wc.hbrBackground = nullptr; RegisterClassW(&wc);

    const int W = 420, H = 420;
    HWND h = CreateWindowExW(WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
        kClassName, L"DeafOverlay", WS_POPUP, CW_USEDEFAULT, CW_USEDEFAULT, W, H, nullptr, nullptr, inst, nullptr);
    BOOL noShadow = FALSE; DwmSetWindowAttribute(h, 2, &noShadow, sizeof(noShadow));
    ShowWindow(h, SW_SHOWNA); UpdateWindow(h);

    RegisterHotKey(h, HK_TOGGLE_VIS, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'H');
    RegisterHotKey(h, HK_OPEN_SETTINGS, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'O');

    g_dib = make_dib(W, H);
    audio_init();

    bool overlayVisible = true, dragging = false; POINT last{};
    auto t0 = std::chrono::steady_clock::now(); float tPrev = 0.f;

    MSG msg{};
    while (true) {
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) { free_dib(g_dib); UnregisterHotKey(h, HK_TOGGLE_VIS); UnregisterHotKey(h, HK_OPEN_SETTINGS); audio_shutdown(); return 0; }
            if (msg.message == WM_HOTKEY) {
                if (msg.wParam == HK_TOGGLE_VIS) {
                    overlayVisible = !overlayVisible; ShowWindow(h, overlayVisible ? SW_SHOWNA : SW_HIDE);
                    if (overlayVisible) SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
                else if (msg.wParam == HK_OPEN_SETTINGS) {
                    MessageBoxW(h, L"Settings soon: sensitivity, L/R swap.", L"Overlay Settings", MB_OK | MB_ICONINFORMATION);
                } continue;
            }
            if (overlayVisible && !g_editMode) {
                if (msg.message == WM_LBUTTONDOWN && (GetKeyState(VK_MENU) & 0x8000)) { dragging = true; GetCursorPos(&last); }
                else if (msg.message == WM_LBUTTONUP) { dragging = false; }
                else if (msg.message == WM_MOUSEMOVE && dragging) {
                    POINT p; GetCursorPos(&p); RECT r; GetWindowRect(h, &r);
                    SetWindowPos(h, HWND_TOPMOST, r.left + (p.x - last.x), r.top + (p.y - last.y), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE); last = p;
                }
            }
            TranslateMessage(&msg); DispatchMessageW(&msg);
        }

        float t = std::chrono::duration<float>(std::chrono::steady_clock::now() - t0).count();
        float dt = t - tPrev; tPrev = t;

        // Read energies
        float EL[NB], ER[NB]; for (int b = 0; b < NB; ++b) { EL[b] = gEL[b].load(std::memory_order_relaxed); ER[b] = gER[b].load(std::memory_order_relaxed); }

        // Pinna (rear/top hints)
        auto L1p = [](float v) { return std::log1p(std::max(0.f, v)); };
        float E58L = EL[3], E58R = ER[3], E812L = EL[4], E812R = ER[4], E25L = EL[2], E25R = ER[2];
        float elevScore = std::clamp(0.5f * ((L1p(E812L) - L1p(E58L)) + (L1p(E812R) - L1p(E58R))), -1.f, 1.f);
        float notch = 0.5f * ((L1p(E58L) - 0.5f * (L1p(E25L) + L1p(E812L))) + (L1p(E58R) - 0.5f * (L1p(E25R) + L1p(E812R))));
        float backScore = std::clamp(notch, 0.f, 1.f);

        // Clear old peaks a bit (will merge below)
        for (auto& p : g_peaks) if (p.life > 0.f) {
            p.life -= dt;
            p.amp *= std::exp(-dt * PEAK_DECAY_HZ);
            if (p.life <= 0.f || p.amp < 0.02f) p = Peak{};
        }

        const float maxAng = MAX_AZIMUTH_DEG * PI_F / 180.f;

        // Build peaks: for each band, emit Left, Center, Right
        for (int b = 0; b < NB; ++b) {
            float L = EL[b], R = ER[b]; float S = L + R;
            if (S <= 1e-6f) continue;

            float ild = (SWAP_LR ? (L - R) : (R - L)) / S;  // -1..+1 (right positive)
            ild = std::clamp(ild, -1.f, 1.f);

            // Components
            float center = std::min(L, R);          // shared energy = mono/front
            float left = std::max(0.f, L - R);   // unique left
            float right = std::max(0.f, R - L);   // unique right

            // Normalize amplitudes to 0..1 using soft knee
            auto norm = [](float v) { return std::tanh(1.2f * v); };

            // TOP
            {
                float a = norm(0.7f * (L + R) * elevScore); // top based on elevScore
                if (a > 0.01f) {
                    Peak q; q.ang = PI_F * 0.5f; q.amp = a; q.life = 0.35f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
			}
            //BOTTOM
            {
                float a = norm(0.4f * (L + R) * -elevScore); // bottom based on elevScore
                if (a > 0.01f) {
                    Peak q; q.ang = std::remainder(PI_F * 1.5f, 2 * PI_F); q.amp = a; q.life = 0.30f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
				}
	        }
			// CENTER
            { 
                float a = norm(0.6f * (L + R) * (1.f - std::fabs(elevScore))); // center based on lack of elevScore
                if (a > 0.01f) {
                    Peak q; q.ang = 0.f; q.amp = a; q.life = 0.40f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
				}
			}
            // FRONT (12 o’clock)
            {
                float a = norm(center);
                if (a > 0.01f) {
                    Peak q; q.ang = 0.f; q.amp = a; q.life = 0.45f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }
            // REAR (6 o’clock)
            {
                float a = norm(0.5f * ild * S); // rear based on ILD
                if (a > 0.01f) {
                    float ang = (ild >= 0.f) ? +maxAng : -maxAng;
                    Peak q; q.ang = std::remainder(ang + PI_F, 2 * PI_F); q.amp = a; q.life = 0.40f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
			}
			// FRONT LEFT
            {
                float a = norm(0.5f * (1.f - ild) * S); // front-left based on ILD
                if (a > 0.01f) {
                    float ang = -maxAng * 0.5f;
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.50f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
				}
            }
            // LEFT
            {
                float a = norm(left);
                if (a > 0.01f) {
                    // place within left span proportional to bias
                    float frac = (L > 0.f ? (left / std::max(L, 1e-6f)) : 0.f); // 0..1
                    float ang = -maxAng * std::clamp(0.35f + 0.65f * frac, 0.0f, 1.0f);
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.55f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }
            // REAR LEFT
            {
                float a = norm(-0.5f * ild * S); // rear-left based on ILD
                if (a > 0.01f) {
                    float ang = std::remainder(-maxAng + PI_F, 2 * PI_F);
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.40f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
				}
            //FRONT RIGHT
            {
                float a = norm(0.5f * (1.f + ild) * S); // front-right based on ILD
                if (a > 0.01f) {
                    float ang = +maxAng * 0.5f;
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.50f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
			}
            // RIGHT
            {
                float a = norm(right);
                if (a > 0.01f) {
                    float frac = (R > 0.f ? (right / std::max(R, 1e-6f)) : 0.f);
                    float ang = +maxAng * std::clamp(0.35f + 0.65f * frac, 0.0f, 1.0f);
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.55f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }
            // REAR RIGHT
            {
                float a = norm(0.5f * ild * S); // rear-right based on ILD
                if (a > 0.01f) {
                    float ang = std::remainder(+maxAng + PI_F, 2 * PI_F);
                    Peak q; q.ang = ang; q.amp = a; q.life = 0.40f; q.band = b;
                    q.rearGain = backScore; q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
			}
        }

        // Render
        std::memset(g_dib.bits, 0, size_t(g_dib.w) * size_t(g_dib.h) * 4);
        float cx = g_dib.w * 0.5f, cy = g_dib.h * 0.47f;
        float R = std::min(g_dib.w, g_dib.h) * 0.33f;
        draw_sphere(g_dib.bits, g_dib.w, g_dib.h, cx, cy, R, 22.f);
        float rx = std::min(g_dib.w, g_dib.h) * 0.36f, ry = rx * 0.45f, rcy = g_dib.h * 0.66f;
        draw_ring(g_dib.bits, g_dib.w, g_dib.h, cx, rcy, rx, ry);
        blit_layered(h, g_dib.dc, g_dib.w, g_dib.h);

        Sleep(16);
    }
}
