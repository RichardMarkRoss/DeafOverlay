// DeafOverlay.cpp — layered window (edit mode + dotted 3D sphere + tilted ring glow)
// Drop-in single file.

#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <windowsx.h> 
#include <dwmapi.h>
#include <chrono>
#include <cmath>
#include <algorithm>
#include <cstring>

#pragma comment(lib, "Dwmapi.lib")

#ifdef min
#undef min
#endif
#ifdef max
#undef max
#endif

static const wchar_t* kClassName = L"DeafOverlayWnd";

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
static void free_dib(DIB& d) { if (d.dc) DeleteDC(d.dc); if (d.bmp) DeleteObject(d.bmp); d = {}; }

// Premultiplied-alpha put pixel (AC_SRC_ALPHA expects premultiplied RGB)
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

/* ---------- 3D dotted sphere ---------- */
static void draw_sphere_dots(
    void* bits, int w, int h,
    float cx, float cy, float radius,
    float tilt_deg, float yaw_deg,   // rotate for a bit of parallax
    BYTE r, BYTE g, BYTE b)
{
    const float tilt = tilt_deg * 3.14159265f / 180.0f; // X axis
    const float yaw = yaw_deg * 3.14159265f / 180.0f; // Y axis
    const float ctx = std::cos(tilt), stx = std::sin(tilt);
    const float cty = std::cos(yaw), sty = std::sin(yaw);

    const int latSteps = 16;     // parallels
    const int lonSteps = 36;     // meridians
    const int dotR = std::max(1, int(radius * 0.010f));

    auto drawDot = [&](float xf, float yf, float zf) {
        float depth = (zf * 0.5f + 0.5f);               // -1..+1 -> 0..1
        BYTE a = (BYTE)std::clamp(int(36 + depth * 160), 0, 200);
        int xi = (int)std::round(xf), yi = (int)std::round(yf);
        for (int dy = -dotR; dy <= dotR; ++dy)
            for (int dx = -dotR; dx <= dotR; ++dx)
                if (dx * dx + dy * dy <= dotR * dotR)
                    putpx_PM(bits, w, h, xi + dx, yi + dy, a, r, g, b);
        };

    // latitude rings
    for (int i = 1; i < latSteps; i++) {
        float v = float(i) / latSteps;                 // 0..1
        float phi = v * 3.14159265f - 3.14159265f / 2.f;  // -pi/2..+pi/2
        float sy = std::sin(phi);
        float cr = std::cos(phi);
        for (int j = 0; j < lonSteps; ++j) {
            float u = float(j) / lonSteps;
            float theta = u * 2.f * 3.14159265f;

            // sphere xyz
            float x = cr * std::cos(theta);
            float y = sy;
            float z = cr * std::sin(theta);

            // rotate yaw (Y), then tilt (X)
            float x1 = cty * x + sty * z;
            float z1 = -sty * x + cty * z;
            float y2 = ctx * y - stx * z1;
            float z2 = stx * y + ctx * z1;

            // mild perspective (closer looks slightly larger)
            float denom = 1.0f + 0.55f * z2;
            float xp = (x1 / denom) * radius + cx;
            float yp = (y2 / denom) * radius + cy;

            drawDot(xp, yp, z2);
        }
    }
}

/* ---------- Ring (tilted ellipse) with moving glow sector ---------- */
static void draw_ring(void* bits, int w, int h, float phase,
    float cx, float cy, float a, float b,
    float rotationXY_deg)
{
    const float rot = rotationXY_deg * 3.14159265f / 180.0f;
    const float c = std::cos(rot), s = std::sin(rot);
    const float th_px = std::max(1.f, a * 0.06f);

    // Glow sector angle uses phase (advanced only when sound present)
    const float theta0 = phase;          // radians
    const float sector_width = 0.55f;

    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            float dx = x - cx, dy = y - cy;

            // rotate into ring frame
            float xr = c * dx + s * dy;
            float yr = -s * dx + c * dy;

            // ellipse radius
            float r = std::sqrt((xr * xr) / (a * a) + (yr * yr) / (b * b));
            float d = std::fabs(r - 1.0f);
            float t = 1.0f - std::min(1.0f, d / (th_px / a));
            if (t <= 0.f) continue;

            // angle around ellipse (approx via atan2 in unscaled space)
            float ang = std::atan2(yr / b, xr / a);

            // color ramp: white base + warm glow near theta0 (wrap properly)
            auto wrap = [](float a) { while (a < -3.14159f) a += 6.28318f; while (a > 3.14159f) a -= 6.28318f; return a; };
            float dist = std::fabs(wrap(ang - theta0));
            float hot = std::max(0.f, 1.f - dist / sector_width);   // 0..1
            // base ring
            float base = t;
            BYTE a1 = (BYTE)(base * 170);
            putpx_PM(bits, w, h, x, y, a1, 255, 255, 255);

            if (hot > 0.f) {
                // additive warm glow (draw thicker / brighter)
                float glow = std::pow(hot, 1.7f);
                BYTE ag = (BYTE)std::clamp(int(glow * 220), 0, 220);
                putpx_PM(bits, w, h, x, y, ag, 255, (BYTE)(120 + 80 * glow), (BYTE)(40 * glow));
            }
        }
    }
}

static void draw_sphere_dots_DEBUG(
    void* bits, int w, int h,
    float cx, float cy, float radius,
    float tilt_deg, float yaw_deg)
{
    const float tilt = tilt_deg * 3.14159265f / 180.0f; // X axis
    const float yaw = yaw_deg * 3.14159265f / 180.0f; // Y axis
    const float ctx = std::cos(tilt), stx = std::sin(tilt);
    const float cty = std::cos(yaw), sty = std::sin(yaw);

    const int latSteps = 12;               // fewer = faster
    const int lonSteps = 24;
    const int dotR = std::max(2, int(radius * 0.02f)); // BIG dots to prove it works

    auto dot = [&](int x, int y, BYTE a, BYTE r, BYTE g, BYTE b) {
        for (int dy = -dotR; dy <= dotR; ++dy)
            for (int dx = -dotR; dx <= dotR; ++dx)
                if (dx * dx + dy * dy <= dotR * dotR)
                    putpx_PM(bits, w, h, x + dx, y + dy, a, r, g, b);
        };

    for (int i = 1; i < latSteps; i++) {
        float v = float(i) / latSteps;                      // 0..1
        float phi = v * 3.14159265f - 3.14159265f * 0.5f;    // -pi/2..+pi/2
        float sy = std::sin(phi), cr = std::cos(phi);

        for (int j = 0; j < lonSteps; j++) {
            float u = float(j) / lonSteps;
            float th = u * 2.0f * 3.14159265f;

            // unit sphere
            float x = cr * std::cos(th);
            float y = sy;
            float z = cr * std::sin(th);

            // yaw(Y) then tilt(X)
            float x1 = cty * x + sty * z;
            float z1 = -sty * x + cty * z;
            float y2 = ctx * y - stx * z1;
            float z2 = stx * y + ctx * z1;

            // simple projection
            float denom = 1.0f + 0.5f * z2;
            int px = int(std::round((x1 / denom) * radius + cx));
            int py = int(std::round((y2 / denom) * radius + cy));

            // BRIGHT so you can’t miss it
            dot(px, py, 255, 255, 255, 255);
        }
    }

    // center crosshair (debug – remove later)
    for (int d = -5; d <= 5; ++d) {
        putpx_PM(bits, w, h, int(cx) + d, int(cy), 255, 0, 255, 0);
        putpx_PM(bits, w, h, int(cx), int(cy) + d, 255, 0, 255, 0);
    }
}

/* ---------- Render frame ---------- */
static void render_frame(void* bits, int w, int h, float t_seconds, bool editMode) {
    std::memset(bits, 0, size_t(w) * size_t(h) * 4);

    // Sphere params
    float cx = w * 0.5f, cy = h * 0.47f;
    float R = std::min(w, h) * 0.33f;
    //draw_sphere_dots_DEBUG(bits, w, h, cx, cy, R, 22.0f, std::sin(t_seconds * 0.7f) * 12.0f);
    float dirx = std::cos(g_soundDirRad), diry = std::sin(g_soundDirRad);
    // Animate a slight yaw so dots “twinkle”
    float yaw = std::sin(t_seconds * 0.7f) * 12.0f; // ±12°
    draw_sphere_dots(bits, w, h, cx, cy, R, /*tilt*/22.0f, /*yaw*/0.0f, 235, 235, 235);

    // Ring params (sits lower)
    float rx = std::min(w, h) * 0.34f;  // major axis
    float ry = rx * 0.45f;              // minor axis (depth foreshortening)
    float rcy = h * 0.66f;
    draw_ring(bits, w, h, /*phase*/ g_glowPhase, cx, rcy, rx, ry, /*rotationXY*/ 0.0f);

    // Edit-mode dashed frame
    if (editMode) {
        const int pad = 2;
        for (int x = pad; x < w - pad; ++x) { if ((x / 4) % 2 == 0) { putpx_PM(bits, w, h, x, pad, 180, 255, 255, 255); putpx_PM(bits, w, h, x, h - 1 - pad, 180, 255, 255, 255); } }
        for (int y = pad; y < h - pad; ++y) { if ((y / 4) % 2 == 0) { putpx_PM(bits, w, h, pad, y, 180, 255, 255, 255); putpx_PM(bits, w, h, w - 1 - pad, y, 180, 255, 255, 255); } }
    }
}

/* ---------- Window plumbing (same edit mode as before; right-click menu) ---------- */
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
    LONG s = GetWindowLongW(hWnd, GWL_STYLE);   s &= ~WS_THICKFRAME;     SetWindowLongW(hWnd, GWL_STYLE, s);
    SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}
static void toggle_edit(HWND h) { if (g_editMode) exit_edit(h); else enter_edit(h); }

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
        if (GetKeyState(VK_MENU) & 0x8000) return HTCAPTION; // Alt-drag fallback
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
        else if (id == kMID_Quit) DestroyWindow(hWnd);
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

int APIENTRY wWinMain(HINSTANCE hInst, HINSTANCE, LPWSTR, int) {
    // DPI aware
    using SetProcessDpiAwarenessContext_t = BOOL(WINAPI*)(HANDLE);
    if (auto setCtx = (SetProcessDpiAwarenessContext_t)
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetProcessDpiAwarenessContext")) {
        setCtx((HANDLE)-4); // PER_MONITOR_AWARE_V2
    }

    WNDCLASSW wc{}; wc.hInstance = hInst; wc.lpfnWndProc = WndProc; wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW); wc.hbrBackground = nullptr; RegisterClassW(&wc);

    const int W = 420, H = 420;
    DWORD ex = WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT;
    HWND hWnd = CreateWindowExW(ex, kClassName, L"DeafOverlay", WS_POPUP,
        CW_USEDEFAULT, CW_USEDEFAULT, W, H, nullptr, nullptr, hInst, nullptr);

    BOOL noShadow = FALSE; DwmSetWindowAttribute(hWnd, 2 /*NCRenderingPolicy*/, &noShadow, sizeof(noShadow));
    ShowWindow(hWnd, SW_SHOWNA); UpdateWindow(hWnd);

    g_dib = make_dib(W, H);

    bool dragging = false; POINT last{};
    auto t0 = std::chrono::steady_clock::now();
    MSG msg{};
    while (true) {
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) { free_dib(g_dib); return 0; }

            // Legacy ALT-drag when not in edit mode
            if (!g_editMode) {
                if (msg.message == WM_LBUTTONDOWN && (GetKeyState(VK_MENU) & 0x8000)) { dragging = true; GetCursorPos(&last); }
                else if (msg.message == WM_LBUTTONUP) { dragging = false; }
                else if (msg.message == WM_MOUSEMOVE && dragging) {
                    POINT p; GetCursorPos(&p); int dx = p.x - last.x, dy = p.y - last.y; last = p;
                    RECT r; GetWindowRect(hWnd, &r);
                    SetWindowPos(hWnd, HWND_TOPMOST, r.left + dx, r.top + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        float t = std::chrono::duration<float>(std::chrono::steady_clock::now() - t0).count();
        render_frame(g_dib.bits, g_dib.w, g_dib.h, t, g_editMode);
        blit_layered(hWnd, g_dib.dc, g_dib.w, g_dib.h);
        Sleep(16);
    }
}
