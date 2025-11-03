// DeafOverlay.cpp — dotted sphere + straight ring, sound-driven only (no auto animation)

#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <windowsx.h>   // GET_X/Y_LPARAM
#include <dwmapi.h>
#include <chrono>
#include <cmath>
#include <algorithm>
#include <cstring>

#pragma comment(lib, "Dwmapi.lib")

static const wchar_t* kClassName = L"DeafOverlayWnd";

/* ===================== DIB ===================== */

struct DIB { HBITMAP bmp{}; HDC dc{}; void* bits{}; int w{}, h{}; };

static DIB make_dib(int w, int h) {
    DIB d{}; d.w = w; d.h = h;
    BITMAPINFO bmi{};
    bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    HDC screen = GetDC(nullptr);
    d.dc = CreateCompatibleDC(screen);
    d.bmp = CreateDIBSection(d.dc, &bmi, DIB_RGB_COLORS, &d.bits, nullptr, 0);
    SelectObject(d.dc, d.bmp);
    ReleaseDC(nullptr, screen);
    return d;
}

static void free_dib(DIB& d) {
    if (d.dc)  DeleteDC(d.dc);
    if (d.bmp) DeleteObject(d.bmp);
    d = {};
}

// Premultiplied-alpha pixel writer (AC_SRC_ALPHA expects premultiplied RGB)
static inline void putpx_PM(void* bits, int w, int h, int x, int y, BYTE a, BYTE r, BYTE g, BYTE b) {
    if ((unsigned)x >= (unsigned)w || (unsigned)y >= (unsigned)h) return;
    UINT pr = (UINT(r) * a + 127) / 255;
    UINT pg = (UINT(g) * a + 127) / 255;
    UINT pb = (UINT(b) * a + 127) / 255;
    BYTE* p = ((BYTE*)bits) + ((size_t)y * w + x) * 4; // BGRA
    p[0] = (BYTE)pb; p[1] = (BYTE)pg; p[2] = (BYTE)pr; p[3] = a;
}

static void blit_layered(HWND hWnd, HDC src, int w, int h) {
    POINT ptPos{ 0,0 }; SIZE sizeWnd{ w,h }; POINT ptSrc{ 0,0 };
    BLENDFUNCTION bf{}; bf.BlendOp = AC_SRC_OVER; bf.SourceConstantAlpha = 255; bf.AlphaFormat = AC_SRC_ALPHA;
    UpdateLayeredWindow(hWnd, nullptr, &ptPos, &sizeWnd, src, &ptSrc, 0, &bf, ULW_ALPHA);
}

/* ===================== Sound control (set these from your audio code) ===================== */
static float g_soundLevel = 0.0f;  // 0..1 envelope (0 = still)
static float g_soundDirRad = 0.0f;  // sound direction on screen (radians, 0 at +X, CCW)
static float g_glowPhase = 0.0f;  // ring glow phase; advanced only when g_soundLevel > 0

/* ===================== Sphere (lat/long dotted) ===================== */

static void draw_sphere_dots(
    void* bits, int w, int h,
    float cx, float cy, float radius,
    float tilt_deg,               // “camera pitch” (around X) for the look
    BYTE baseR, BYTE baseG, BYTE baseB)
{
    const float tilt = tilt_deg * 3.14159265f / 180.0f; // X rotation
    const float ct = std::cos(tilt), st = std::sin(tilt);

    // density & dot size
    const int latSteps = 16;
    const int lonSteps = 36;
    const int dotR = std::max(2, int(radius * 0.014f));

    // projected sound direction in screen space
    const float dirx = std::cos(g_soundDirRad), diry = std::sin(g_soundDirRad);

    auto dot = [&](int xi, int yi, BYTE a, BYTE r, BYTE g, BYTE b) {
        for (int dy = -dotR; dy <= dotR; ++dy)
            for (int dx = -dotR; dx <= dotR; ++dx)
                if (dx * dx + dy * dy <= dotR * dotR)
                    putpx_PM(bits, w, h, xi + dx, yi + dy, a, r, g, b);
        };

    for (int i = 1; i < latSteps; ++i) {
        float v = float(i) / latSteps;                      // 0..1
        float phi = v * 3.14159265f - 3.14159265f * 0.5f;   // -pi/2..+pi/2
        float sy = std::sin(phi), cr = std::cos(phi);

        for (int j = 0; j < lonSteps; ++j) {
            float u = float(j) / lonSteps;
            float th = u * 2.0f * 3.14159265f;

            // unit sphere
            float x = cr * std::cos(th);
            float y = sy;
            float z = cr * std::sin(th);

            // tilt around X (pitch)
            float y2 = ct * y - st * z;
            float z2 = st * y + ct * z;

            // simple perspective
            float denom = 1.0f + 0.55f * z2;
            float xp = (x / denom) * radius + cx;
            float yp = (y2 / denom) * radius + cy;

            // sound-driven directional “bounce”/brighten (no time animation)
            float vx = xp - cx, vy = yp - cy;
            float vlen = std::max(1.0f, std::sqrt(vx * vx + vy * vy));
            float align = (vx * dirx + vy * diry) / vlen;        // -1..+1; toward sound => >0

            float push = 1.0f + 0.06f * g_soundLevel * std::max(0.0f, align);
            xp = cx + vx * push;
            yp = cy + vy * push;

            float depth = (z2 * 0.5f + 0.5f);                    // 0..1
            float bright = 1.0f + 0.8f * g_soundLevel * std::max(0.0f, align);
            BYTE a = (BYTE)std::clamp(int(36 + depth * 160 * bright), 0, 220);
            BYTE rr = (BYTE)std::min(255.0f, baseR * bright);
            BYTE gg = (BYTE)std::min(255.0f, baseG * bright);
            BYTE bb = (BYTE)std::min(255.0f, baseB * bright);

            dot((int)std::round(xp), (int)std::round(yp), a, rr, gg, bb);
        }
    }
}

/* ===================== Ring (straight in XY; glow uses phase) ===================== */

static void draw_ring(
    void* bits, int w, int h,
    float phase,                   // glow phase, we advance this only if sound is present
    float cx, float cy,            // center
    float a, float b)              // ellipse axes (a: x-major, b: y-minor -> “Z tilt” look)
{
    // no rotation in XY plane (straight ring), just an ellipse
    const float th_px = std::max(1.f, a * 0.06f);

    const float theta0 = phase;            // glow center angle (radians)
    const float sector_width = 0.55f;      // radians half-width

    auto wrap = [](float ang) {
        while (ang < -3.14159f) ang += 6.28318f;
        while (ang > 3.14159f) ang -= 6.28318f;
        return ang;
        };

    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            float dx = x - cx, dy = y - cy;

            // ellipse radius
            float r = std::sqrt((dx * dx) / (a * a) + (dy * dy) / (b * b));
            float d = std::fabs(r - 1.0f);
            float t = 1.0f - std::min(1.0f, d / (th_px / a));
            if (t <= 0.f) continue;

            // angle around ellipse (approx)
            float ang = std::atan2(dy / b, dx / a);

            // base ring (white)
            BYTE a1 = (BYTE)(t * 170);
            putpx_PM(bits, w, h, x, y, a1, 255, 255, 255);

            // glow only when sound present (phase only moves then)
            if (g_soundLevel > 0.02f) {
                float dist = std::fabs(wrap(ang - theta0));
                float hot = std::max(0.f, 1.f - dist / sector_width);
                float glow = std::pow(hot, 1.7f) * std::clamp(g_soundLevel, 0.0f, 1.0f);
                BYTE ag = (BYTE)std::clamp(int(glow * 220), 0, 220);
                putpx_PM(bits, w, h, x, y, ag, 255, (BYTE)(120 + 80 * glow), (BYTE)(40 * glow));
            }
        }
    }
}

/* ===================== Render frame ===================== */

static void render_frame(void* bits, int w, int h, bool editMode) {
    std::memset(bits, 0, size_t(w) * size_t(h) * 4);

    // Sphere (static unless sound nudges it)
    float cx = w * 0.5f, cy = h * 0.47f;
    float R = std::min(w, h) * 0.33f;
    draw_sphere_dots(bits, w, h, cx, cy, R, /*tilt*/22.0f, 235, 235, 235);

    // Ring (straight in XY; lower than sphere). Glow uses g_glowPhase.
    float rx = std::min(w, h) * 0.36f;   // major axis
    float ry = rx * 0.45f;               // minor axis (depth foreshortening)
    float rcy = h * 0.66f;
    draw_ring(bits, w, h, /*phase*/ g_glowPhase, cx, rcy, rx, ry);

    // Edit-mode dashed border
    if (editMode) {
        const int pad = 2;
        for (int x = pad; x < w - pad; ++x) {
            if ((x / 4) % 2 == 0) { putpx_PM(bits, w, h, x, pad, 180, 255, 255, 255); putpx_PM(bits, w, h, x, h - 1 - pad, 180, 255, 255, 255); }
        }
        for (int y = pad; y < h - pad; ++y) {
            if ((y / 4) % 2 == 0) { putpx_PM(bits, w, h, pad, y, 180, 255, 255, 255); putpx_PM(bits, w, h, w - 1 - pad, y, 180, 255, 255, 255); }
        }
    }
}

/* ===================== Window plumbing (edit mode + menu) ===================== */

static bool g_editMode = false;
static DIB  g_dib{};

enum MenuIds { kMID_ToggleEdit = 1, kMID_ResetSize, kMID_Quit };

static void enter_edit(HWND hWnd) {
    if (g_editMode) return; g_editMode = true;
    LONG ex = GetWindowLongW(hWnd, GWL_EXSTYLE); ex &= ~WS_EX_TRANSPARENT; SetWindowLongW(hWnd, GWL_EXSTYLE, ex);
    LONG s = GetWindowLongW(hWnd, GWL_STYLE);   s |= WS_THICKFRAME;      SetWindowLongW(hWnd, GWL_STYLE, s);
    SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}
static void exit_edit(HWND hWnd) {
    if (!g_editMode) return; g_editMode = false;
    LONG ex = GetWindowLongW(hWnd, GWL_EXSTYLE); ex |= WS_EX_TRANSPARENT; SetWindowLongW(hWnd, GWL_EXSTYLE, ex);
    LONG s = GetWindowLongW(hWnd, GWL_STYLE);   s &= ~WS_THICKFRAME;    SetWindowLongW(hWnd, GWL_STYLE, s);
    SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}
static void toggle_edit(HWND hWnd) { if (g_editMode) exit_edit(hWnd); else enter_edit(hWnd); }

static LRESULT edit_hit_test(HWND hWnd, LPARAM lParam) {
    const int grip = 8;
    POINT pt{ GET_X_LPARAM(lParam), GET_Y_LPARAM(lParam) };
    RECT r; GetWindowRect(hWnd, &r);
    int x = pt.x - r.left, y = pt.y - r.top, w = r.right - r.left, h = r.bottom - r.top;
    bool L = x < grip, R = x >= w - grip, T = y < grip, B = y >= h - grip;
    if (T && L) return HTTOPLEFT; if (T && R) return HTTOPRIGHT; if (B && L) return HTBOTTOMLEFT; if (B && R) return HTBOTTOMRIGHT;
    if (L) return HTLEFT; if (R) return HTRIGHT; if (T) return HTTOP; if (B) return HTBOTTOM; return HTCAPTION;
}

LRESULT CALLBACK WndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam) {
    switch (msg) {
    case WM_NCHITTEST:
        if (g_editMode) return edit_hit_test(hWnd, lParam);
        if (GetKeyState(VK_MENU) & 0x8000) return HTCAPTION; // Alt-drag when not editing
        return HTTRANSPARENT;

    case WM_RBUTTONUP: {
        HMENU m = CreatePopupMenu();
        AppendMenuW(m, MF_STRING, kMID_ToggleEdit, g_editMode ? L"Exit Edit Mode" : L"Enter Edit Mode");
        AppendMenuW(m, MF_STRING, kMID_ResetSize, L"Reset Size (420×420)");
        AppendMenuW(m, MF_SEPARATOR, 0, nullptr);
        AppendMenuW(m, MF_STRING, kMID_Quit, L"Quit");
        POINT p; GetCursorPos(&p);
        UINT id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hWnd, nullptr);
        DestroyMenu(m);
        if (id == kMID_ToggleEdit) toggle_edit(hWnd);
        else if (id == kMID_ResetSize) {
            SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 420, 420, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            PostMessageW(hWnd, WM_SIZE, 0, MAKELPARAM(420, 420));
        }
        else if (id == kMID_Quit) {
            DestroyWindow(hWnd);
        }
        return 0;
    }

    case WM_SIZE: {
        int w = LOWORD(lParam), h = HIWORD(lParam);
        if (w > 0 && h > 0) { free_dib(g_dib); g_dib = make_dib(w, h); }
        return 0;
    }

    case WM_DESTROY: PostQuitMessage(0); return 0;
    default: return DefWindowProcW(hWnd, msg, wParam, lParam);
    }
}

/* ===================== Entry ===================== */

int APIENTRY wWinMain(HINSTANCE hInst, HINSTANCE, LPWSTR, int) {
    // DPI awareness
    using SetProcessDpiAwarenessContext_t = BOOL(WINAPI*)(HANDLE);
    if (auto setCtx = (SetProcessDpiAwarenessContext_t)
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetProcessDpiAwarenessContext")) {
        setCtx((HANDLE)-4); // PER_MONITOR_AWARE_V2
    }

    // class
    WNDCLASSW wc{}; wc.hInstance = hInst; wc.lpfnWndProc = WndProc; wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW); wc.hbrBackground = nullptr;
    RegisterClassW(&wc);

    // window (layered; click-through; topmost; not in alt-tab)
    const int W = 420, H = 420;
    DWORD ex = WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT;
    HWND hWnd = CreateWindowExW(ex, kClassName, L"DeafOverlay", WS_POPUP,
        CW_USEDEFAULT, CW_USEDEFAULT, W, H, nullptr, nullptr, hInst, nullptr);

    BOOL noShadow = FALSE;
    DwmSetWindowAttribute(hWnd, 2 /*NCRenderingPolicy*/, &noShadow, sizeof(noShadow));

    ShowWindow(hWnd, SW_SHOWNA);
    UpdateWindow(hWnd);

    g_dib = make_dib(W, H);

    bool dragging = false; POINT last{};
    auto t0 = std::chrono::steady_clock::now();
    float tPrev = 0.0f;

    MSG msg{};
    while (true) {
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) { free_dib(g_dib); return 0; }

            // Legacy ALT-drag (only when not in edit mode)
            if (!g_editMode) {
                if (msg.message == WM_LBUTTONDOWN && (GetKeyState(VK_MENU) & 0x8000)) {
                    dragging = true; GetCursorPos(&last);
                }
                else if (msg.message == WM_LBUTTONUP) {
                    dragging = false;
                }
                else if (msg.message == WM_MOUSEMOVE && dragging) {
                    POINT p; GetCursorPos(&p);
                    int dx = p.x - last.x, dy = p.y - last.y; last = p;
                    RECT r; GetWindowRect(hWnd, &r);
                    SetWindowPos(hWnd, HWND_TOPMOST, r.left + dx, r.top + dy, 0, 0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // “Time” only used to compute dt for sound-phase advance
        float t = std::chrono::duration<float>(std::chrono::steady_clock::now() - t0).count();
        float dt = t - tPrev; tPrev = t;

        // Advance ring glow only with sound
        if (g_soundLevel > 0.02f) {
            g_glowPhase += dt * (1.0f + 2.0f * g_soundLevel);
        }

        render_frame(g_dib.bits, g_dib.w, g_dib.h, g_editMode);
        blit_layered(hWnd, g_dib.dc, g_dib.w, g_dib.h);

        Sleep(16);
    }
}
