// DeafOverlay.cpp — Fullscreen spatial overlay with settings & per-band controls
// - Fullscreen layered window
// - Normal mode: click-through, sphere+ring only
// - Alt+H: toggle overlay visibility
// - Alt+I: settings mode
//     * Dark transparent fullscreen background (blocks clicks to game)
//     * Movable settings panel
//     * Sphere size slider
//     * Ring vertical position slider
//     * Global sensitivity slider
//     * 6-band frequency sliders: Bass, Low-Mid, Mid, High-Mid, Treble, Air
//     * Swap L/R checkbox
//     * Sphere draggable to preferred position (saved normalized)
// - Uses WASAPI loopback via miniaudio
// - Spatial logic from previous checkpoint: multi-peaks, rear hints, elevation hints

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

/* ===== Base tunables ===== */
static constexpr float BASE_SENS_GAIN = 0.95f;
static constexpr float MAX_AZIMUTH_DEG = 78.0f;   // per-side span on ring
static constexpr float PEAK_DECAY_HZ = 1.7f;
static constexpr float ELEV_SCALE = 0.30f;
static constexpr float REAR_SCALE = 1.00f;

/* ===== Runtime user settings ===== */
static float gSphereScale = 1.0f;   // slider: 0.6 .. 1.6
static float gRingOffsetY = 0.66f;  // slider: 0.40 .. 0.90 (fraction of screen height)
static float gSensitivity = 1.0f;   // slider: 0.5 .. 2.0
static bool  gSwapLR = false;  // Swap L/R channels for direction
static bool  gSettingsOpen = false; // Alt+I

// Sphere position in normalized screen coords (0..1)
static float gSpherePosX = 0.5f;
static float gSpherePosY = 0.47f;

// Per-band gain (user adjustable via sliders), NB=6
// Bands (rough):
// 0: Bass (0–250Hz)
// 1: Low-Mid (250–2k)
// 2: Mid (2k–5k)
// 3: High-Mid (5k–8k)
// 4: Treble (8k–12k)
// 5: Air (>12k)
static constexpr int NB = 6;
static float gBandGain[NB] = { 1.f,1.f,1.f,1.f,1.f,1.f };

/* ===== Settings panel state ===== */
static const int PANEL_W = 320;
static const int PANEL_H = 310;

static int  gPanelX = 40;
static int  gPanelY = 40;
static bool gDraggingPanel = false;
static int  gPanelDragOffX = 0;
static int  gPanelDragOffY = 0;

// Active slider index:
// 0 = sphere size, 1 = ring Y, 2 = sensitivity,
// 3..8 = band 0..5 (Bass..Air)
static int gActiveSlider = -1;

// Sphere drag (in settings mode)
static bool gDraggingSphere = false;

/* ===== Audio (miniaudio) ===== */
#define MINIAUDIO_IMPLEMENTATION
#define MA_NO_DECODING
#define MA_NO_ENCODING
#define MA_NO_RESOURCE_MANAGER
#include "third_party/miniaudio.h"

static ma_context g_ctx{};
static ma_device  g_dev{};
static bool       g_audioOK = false;

/* ===== DIB / drawing ===== */
struct DIB { HBITMAP bmp{}; HDC dc{}; void* bits{}; int w{}, h{}; };
static DIB g_dib{};

static DIB make_dib(int w, int h) {
    DIB d{}; d.w = w; d.h = h;
    BITMAPINFO bmi{};
    bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    HDC s = GetDC(nullptr);
    d.dc = CreateCompatibleDC(s);
    d.bmp = CreateDIBSection(d.dc, &bmi, DIB_RGB_COLORS, &d.bits, nullptr, 0);
    SelectObject(d.dc, d.bmp);
    ReleaseDC(nullptr, s);
    return d;
}
static void free_dib(DIB& d) {
    if (d.dc)  DeleteDC(d.dc);
    if (d.bmp) DeleteObject(d.bmp);
    d = {};
}

// Premultiplied alpha pixel
static inline void putpx_PM(void* bits, int w, int h, int x, int y,
    BYTE a, BYTE r, BYTE g, BYTE b)
{
    if ((unsigned)x >= (unsigned)w || (unsigned)y >= (unsigned)h) return;
    BYTE* p = (BYTE*)bits + ((size_t)y * w + x) * 4;
    p[0] = (BYTE)((b * a + 127) / 255);
    p[1] = (BYTE)((g * a + 127) / 255);
    p[2] = (BYTE)((r * a + 127) / 255);
    p[3] = a;
}

static void blit_layered(HWND hWnd, HDC src, int w, int h) {
    POINT pos{ 0,0 };
    SIZE  sz{ w,h };
    POINT srcpt{ 0,0 };
    BLENDFUNCTION bf{};
    bf.BlendOp = AC_SRC_OVER;
    bf.SourceConstantAlpha = 255;
    bf.AlphaFormat = AC_SRC_ALPHA;
    UpdateLayeredWindow(hWnd, nullptr, &pos, &sz, src, &srcpt, 0, &bf, ULW_ALPHA);
}

/* ===== Band energies (atomic from callback) ===== */
static std::atomic<float> gEL[NB], gER[NB];

static inline float lp_step(float y, float x, float a) { return y + a * (x - y); }

/* ===== Peaks / pinna helpers ===== */
constexpr float PI_F = 3.14159265358979323846f;
static float gBackB[NB] = { 0 };
static float gFrontB[NB] = { 0 };
static float gElevS = 0.f;
static inline float ema(float prev, float x, float a) { return prev + a * (x - prev); }

struct Peak {
    float ang = 0.f;    // 0=front (12 o'clock), +right, -left
    float amp = 0.f;    // 0..1
    float life = 0.f;   // seconds
    int   band = 0;
    float rearGain = 0.f;
    float elevFactor = 1.f;
};
static Peak g_peaks[18];  // up to 3 per band

static inline void level_to_color(float s, BYTE& r, BYTE& g, BYTE& b) {
    r = 255;
    g = (BYTE)std::clamp(255.0f - s * 255.0f * 0.6f, 0.0f, 255.0f);
    b = (BYTE)std::clamp(255.0f - s * 255.0f, 0.0f, 255.0f);
}

static void add_peak_merge(const Peak& q) {
    int best = -1;
    float bestD = 1e9f;
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

    // Insert / replace lowest amp
    int slot = 0;
    for (int i = 1; i < (int)std::size(g_peaks); ++i)
        if (g_peaks[i].amp < g_peaks[slot].amp) slot = i;
    g_peaks[slot] = q;
}

/* ===== Audio callback ===== */
void MA_API audio_cb(ma_device* dev, void*, const void* pIn, ma_uint32 frames) {
    const float* s = (const float*)pIn;
    if (!s) return;

    const ma_uint32 ch = dev->capture.channels ? dev->capture.channels : 1;

    auto a_fc = [&](float fc) { return 1.0f - std::exp(-6.2831853f * fc / (float)dev->sampleRate); };
    const float a250 = a_fc(250.f), a2k = a_fc(2000.f), a5k = a_fc(5000.f),
        a8k = a_fc(8000.f), a12k = a_fc(12000.f);
    const float aTot = 0.07f;

    float lp250L = 0, lp2kL = 0, lp5kL = 0, lp8kL = 0, lp12kL = 0, totL = 0;
    float lp250R = 0, lp2kR = 0, lp5kR = 0, lp8kR = 0, lp12kR = 0, totR = 0;

    for (ma_uint32 i = 0; i < frames; ++i) {
        float L = s[i * ch + 0];
        float R = (ch > 1) ? s[i * ch + 1] : L;

        float eL = L * L;
        float eR = R * R;

        lp250L = lp_step(lp250L, eL, a250);
        lp2kL = lp_step(lp2kL, eL, a2k);
        lp5kL = lp_step(lp5kL, eL, a5k);
        lp8kL = lp_step(lp8kL, eL, a8k);
        lp12kL = lp_step(lp12kL, eL, a12k);
        totL = lp_step(totL, eL, aTot);

        lp250R = lp_step(lp250R, eR, a250);
        lp2kR = lp_step(lp2kR, eR, a2k);
        lp5kR = lp_step(lp5kR, eR, a5k);
        lp8kR = lp_step(lp8kR, eR, a8k);
        lp12kR = lp_step(lp12kR, eR, a12k);
        totR = lp_step(totR, eR, aTot);
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

    // Global and per-band gains
    float g = 1.8f * BASE_SENS_GAIN * gSensitivity;

    // Slight built-in lift for quiet lows/mids (before user EQ)
    bL[0] *= 1.25f; bR[0] *= 1.25f;
    bL[1] *= 1.10f; bR[1] *= 1.10f;

    for (int k = 0; k < NB; ++k) {
        float bandGain = std::clamp(gBandGain[k], 0.0f, 3.0f); // user EQ
        float eL = g * bandGain * bL[k];
        float eR = g * bandGain * bR[k];

        auto upd = [&](std::atomic<float>& a, float v) {
            float p = a.load(std::memory_order_relaxed);
            a.store(p + 0.22f * (v - p), std::memory_order_relaxed);
            };
        upd(gEL[k], eL);
        upd(gER[k], eR);
    }
}

/* ===== Device ===== */
static bool audio_init() {
    if (ma_context_init(nullptr, 0, nullptr, &g_ctx) != MA_SUCCESS) return false;

    ma_device_config cfg = ma_device_config_init(ma_device_type_loopback);
    cfg.capture.format = ma_format_f32;
    cfg.capture.channels = 2;
    cfg.sampleRate = 48000;
    cfg.performanceProfile = ma_performance_profile_low_latency;
    cfg.dataCallback = audio_cb;

    if (ma_device_init(&g_ctx, &cfg, &g_dev) != MA_SUCCESS) {
        ma_context_uninit(&g_ctx);
        return false;
    }
    if (ma_device_start(&g_dev) != MA_SUCCESS) {
        ma_device_uninit(&g_dev);
        ma_context_uninit(&g_ctx);
        return false;
    }
    g_audioOK = true;
    return true;
}
static void audio_shutdown() {
    if (!g_audioOK) return;
    ma_device_uninit(&g_dev);
    ma_context_uninit(&g_ctx);
    g_audioOK = false;
}

/* ===== Sphere & Ring drawing ===== */

static inline void base_ring_pixel(void* bits, int w, int h, int x, int y, float t) {
    putpx_PM(bits, w, h, x, y, (BYTE)(t * 150), 255, 255, 255);
}

static void draw_ring(void* bits, int w, int h, float cx, float cy, float a, float b) {
    const float th_px = std::max(1.f, a * 0.06f);

    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            float dx = x - cx;
            float dy = y - cy;
            float r = std::sqrt((dx * dx) / (a * a) + (dy * dy) / (b * b));
            float d = std::fabs(r - 1.f);
            float t = 1.f - std::min(1.f, d / (th_px / a));
            if (t <= 0.f) continue;

            base_ring_pixel(bits, w, h, x, y, t);

            float ang = std::atan2((dx / a), (-dy / b)); // 0 at top
            float bestGlow = 0.f, bestAmp = 0.f;

            for (const auto& p : g_peaks) if (p.life > 0.f) {
                float dist = std::fabs(std::remainder(ang - p.ang, 2 * PI_F));
                float hot = std::max(0.f, 1.f - dist / 0.9f);
                float win = (1.f - std::cosf(hot * PI_F)) * 0.5f;
                float g = win * p.amp;
                if (g > bestGlow) { bestGlow = g; bestAmp = p.amp; }
            }

            if (bestGlow > 0.f) {
                BYTE rr, gg, bb; level_to_color(bestAmp, rr, gg, bb);
                BYTE aa = (BYTE)std::clamp(int(bestGlow * 230), 0, 230);
                putpx_PM(bits, w, h, x, y, aa, rr, gg, bb);
            }
        }
    }

    // Arrowheads for each peak
    for (const auto& p : g_peaks) if (p.life > 0.f) {
        BYTE rr, gg, bb; level_to_color(p.amp, rr, gg, bb);
        float th = p.ang;
        float ex = cx + a * std::sinf(th);
        float ey = cy - b * std::cosf(th);
        float nx = -b * std::cosf(th);
        float ny = -a * std::sinf(th);
        float nlen = std::max(1.f, std::sqrt(nx * nx + ny * ny));
        nx /= nlen; ny /= nlen;
        float tx = std::cosf(th), ty = std::sinf(th);

        for (int i = -5; i <= 5; ++i) {
            for (int j = 0; j < 7; ++j) {
                int xi = int(ex + nx * i + (a * 0.035f) * tx * j);
                int yi = int(ey + ny * i + (b * 0.035f) * ty * j);
                putpx_PM(bits, w, h, xi, yi,
                    (BYTE)(120 + 100 * p.amp), rr, gg, bb);
            }
        }
    }
}

static void draw_sphere(void* bits, int w, int h,
    float cx, float cy, float radius, float tilt_deg)
{
    const float tilt = tilt_deg * PI_F / 180.f;
    const float ct = std::cos(tilt), st = std::sin(tilt);
    const int latSteps = 16, lonSteps = 36;

    for (int i = 1; i < latSteps; ++i) {
        float v = float(i) / latSteps;
        float phi = v * PI_F - PI_F * 0.5f;
        float sy = std::sin(phi), cr = std::cos(phi);

        for (int j = 0; j < lonSteps; ++j) {
            float u = float(j) / lonSteps;
            float th = u * 2.f * PI_F;
            float x = cr * std::cos(th);
            float y = sy;
            float z = cr * std::sin(th);

            float y2 = ct * y - st * z;
            float z2 = st * y + ct * z;

            float denom = 1.f + 0.55f * z2;
            float xp = (x / denom) * radius + cx;
            float yp = (y2 / denom) * radius + cy;

            float vx = xp - cx;
            float vy = yp - cy;
            float vlen = std::max(1.0f, std::sqrt(vx * vx + vy * vy));

            float alignBest = 0.f, ampBest = 0.f;
            for (const auto& p : g_peaks) if (p.life > 0.f) {
                float dirx = std::sinf(p.ang);
                float diry = -std::cosf(p.ang);
                float align = (vx * dirx + vy * diry) / vlen;
                float contrib = align * p.amp;
                if (contrib > alignBest) { alignBest = contrib; ampBest = p.amp; }
                else ampBest = std::max(ampBest, p.amp);
            }

            float grow = 1.0f + 0.22f * std::max(0.0f, alignBest);
            int   dotR = std::max(2, int(radius * 0.012f * grow));
            float depth = (z2 * 0.5f + 0.5f);
            float bright = (0.55f + 0.55f * depth +
                0.95f * std::max(0.0f, alignBest));

            BYTE rr, gg, bb; level_to_color(ampBest, rr, gg, bb);
            BYTE a = (BYTE)std::clamp(int(24 + 200 * bright), 0, 238);

            int xi0 = (int)std::round(xp);
            int yi0 = (int)std::round(yp);
            for (int dy = -dotR; dy <= dotR; ++dy)
                for (int dx = -dotR; dx <= dotR; ++dx)
                    if (dx * dx + dy * dy <= dotR * dotR)
                        putpx_PM(bits, w, h, xi0 + dx, yi0 + dy, a, rr, gg, bb);
        }
    }

    // Rays & rear accent
    for (const auto& p : g_peaks) if (p.life > 0.f) {
        BYTE rr, gg, bb; level_to_color(p.amp, rr, gg, bb);
        float fx = std::sinf(p.ang), fy = -std::cosf(p.ang);
        int len = int(radius * 0.9f);
        for (int i = 0; i < len; ++i)
            putpx_PM(bits, w, h, int(cx + fx * i), int(cy + fy * i),
                (BYTE)(40 + 150 * p.amp), rr, gg, bb);

        if (p.rearGain > 0.f) {
            float rx = std::sinf(p.ang + PI_F), ry = -std::cosf(p.ang + PI_F);
            int rlen = int(radius * 0.55f);
            BYTE a = (BYTE)std::clamp(int((20 + 160 * p.rearGain) * REAR_SCALE), 0, 255);
            for (int i = 0; i < rlen; ++i)
                putpx_PM(bits, w, h, int(cx + rx * i), int(cy + ry * i),
                    a, 200, 220, 255);
        }
    }
}

/* ===== Slider helpers ===== */

static void slider_apply(int idx, float t) {
    t = std::clamp(t, 0.0f, 1.0f);
    if (idx == 0) {
        gSphereScale = 0.6f + t * (1.6f - 0.6f);
    }
    else if (idx == 1) {
        gRingOffsetY = 0.40f + t * (0.90f - 0.40f);
    }
    else if (idx == 2) {
        gSensitivity = 0.5f + t * (2.0f - 0.5f);
    }
    else if (idx >= 3 && idx < 3 + NB) {
        int b = idx - 3;
        // Map:
        // t=0   -> gain=0.0 (mute that band)
        // t=0.5 -> gain=1.0 (neutral)
        // t=1.0 -> gain=3.0 (boost)
        if (t <= 0.5f) {
            gBandGain[b] = (t / 0.5f) * 1.0f;                // 0..1
        }
        else {
            gBandGain[b] = 1.0f + ((t - 0.5f) / 0.5f) * 2.0f; // 1..3
        }
    }
}

static float slider_t_from_value(int idx) {
    if (idx == 0) {
        return std::clamp((gSphereScale - 0.6f) / (1.6f - 0.6f), 0.0f, 1.0f);
    }
    else if (idx == 1) {
        return std::clamp((gRingOffsetY - 0.40f) / (0.90f - 0.40f), 0.0f, 1.0f);
    }
    else if (idx == 2) {
        return std::clamp((gSensitivity - 0.5f) / (2.0f - 0.5f), 0.0f, 1.0f);
    }
    else if (idx >= 3 && idx < 3 + NB) {
        int b = idx - 3;
        float g = std::clamp(gBandGain[b], 0.0f, 3.0f);
        if (g <= 1.0f) {
            return 0.5f * (g / 1.0f);                          // 0..0.5
        }
        else {
            return 0.5f + 0.5f * ((g - 1.0f) / 2.0f);          // 0.5..1.0
        }
    }
    return 0.0f;
}

static void draw_slider_bar(void* bits, int w, int h,
    int x0, int y, int x1, float t)
{
    // Track
    for (int x = x0; x <= x1; ++x)
        putpx_PM(bits, w, h, x, y, 90, 220, 220, 220);

    // Knob
    int knobX = x0 + int(t * float(x1 - x0));
    for (int dy = -4; dy <= 4; ++dy)
        for (int dx = -3; dx <= 3; ++dx)
            putpx_PM(bits, w, h, knobX + dx, y + dy, 210, 255, 180, 80);
}

/* ===== Settings panel drawing ===== */

static void draw_settings_panel() {
    if (!gSettingsOpen || !g_dib.bits) return;

    int w = g_dib.w, h = g_dib.h;
    void* bits = g_dib.bits;

    // Clamp panel on-screen
    if (gPanelX < 10) gPanelX = 10;
    if (gPanelY < 10) gPanelY = 10;
    if (gPanelX + PANEL_W > w - 10) gPanelX = w - 10 - PANEL_W;
    if (gPanelY + PANEL_H > h - 10) gPanelY = h - 10 - PANEL_H;

    int x0 = gPanelX;
    int y0 = gPanelY;
    int x1 = x0 + PANEL_W;
    int y1 = y0 + PANEL_H;

    // Panel background
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 200, 16, 16, 20);

    // Title bar
    for (int y = y0; y < y0 + 24; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 230, 32, 32, 40);

    // Layout
    int innerX0 = x0 + 12;
    int innerX1 = x1 - 12;
    int row = y0 + 40;
    int dy = 26;

    int ySphere = row;          row += dy;
    int yRing = row;          row += dy;
    int ySens = row;          row += dy;
    int yBass = row;          row += dy;
    int yLowMid = row;          row += dy;
    int yMid = row;          row += dy;
    int yHigh = row;          row += dy;
    int yAir = row;          row += dy;

    // Sliders
    draw_slider_bar(bits, w, h, innerX0, ySphere, innerX1, slider_t_from_value(0));
    draw_slider_bar(bits, w, h, innerX0, yRing, innerX1, slider_t_from_value(1));
    draw_slider_bar(bits, w, h, innerX0, ySens, innerX1, slider_t_from_value(2));
    draw_slider_bar(bits, w, h, innerX0, yBass, innerX1, slider_t_from_value(3));
    draw_slider_bar(bits, w, h, innerX0, yLowMid, innerX1, slider_t_from_value(4));
    draw_slider_bar(bits, w, h, innerX0, yMid, innerX1, slider_t_from_value(5));
    draw_slider_bar(bits, w, h, innerX0, yHigh, innerX1, slider_t_from_value(6));
    draw_slider_bar(bits, w, h, innerX0, yAir, innerX1, slider_t_from_value(7));

    // Text labels via GDI
    HDC dc = g_dib.dc;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    HFONT hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    HFONT oldF = (HFONT)SelectObject(dc, hFont);

    TextOutW(dc, x0 + 10, y0 + 5, L"DeafOverlay Settings", 21);

    TextOutW(dc, innerX0, ySphere - 14, L"Sphere size", 11);
    TextOutW(dc, innerX0, yRing - 14, L"Ring vertical position", 23);
    TextOutW(dc, innerX0, ySens - 14, L"Global sensitivity", 18);

    TextOutW(dc, innerX0, yBass - 14, L"Bass (0-250 Hz)", 15);
    TextOutW(dc, innerX0, yLowMid - 14, L"Low-Mid (250-2k)", 17);
    TextOutW(dc, innerX0, yMid - 14, L"Mid (2-5k)", 11);
    TextOutW(dc, innerX0, yHigh - 14, L"High-Mid (5-8k)", 17);
    TextOutW(dc, innerX0, yAir - 14, L"Treble / Air (8k+)", 19);

    // Swap L/R checkbox
    int cbX = innerX0;
    int cbY = yAir + 10;
    int cbS = 12;

    for (int yy = 0; yy < cbS; ++yy)
        for (int xx = 0; xx < cbS; ++xx) {
            BYTE a = (yy == 0 || yy == cbS - 1 || xx == 0 || xx == cbS - 1) ? 220 : 40;
            putpx_PM(bits, w, h, cbX + xx, cbY + yy, a, 255, 255, 255);
        }

    if (gSwapLR) {
        for (int i = 2; i < cbS - 2; ++i)
            putpx_PM(bits, w, h, cbX + i, cbY + i, 230, 255, 220, 80);
        for (int i = 2; i < cbS - 2; ++i)
            putpx_PM(bits, w, h, cbX + (cbS - 1 - i), cbY + i, 230, 255, 220, 80);
    }
    TextOutW(dc, cbX + cbS + 6, cbY - 1, L"Swap L/R", 8);

    SelectObject(dc, oldF);
}

/* ===== Settings mode toggle ===== */

static const wchar_t* kClassName = L"DeafOverlayWnd";
enum : int { HK_TOGGLE_VIS = 1, HK_TOGGLE_CFG = 2 };

static void set_settings_mode(HWND h, bool on) {
    gSettingsOpen = on;
    LONG ex = GetWindowLongW(h, GWL_EXSTYLE);
    if (on) {
        ex &= ~WS_EX_TRANSPARENT; // capture clicks
    }
    else {
        ex |= WS_EX_TRANSPARENT; // click-through
        gActiveSlider = -1;
        gDraggingPanel = false;
        gDraggingSphere = false;
        ReleaseCapture();
    }
    SetWindowLongW(h, GWL_EXSTYLE, ex);
    SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER |
        SWP_FRAMECHANGED | SWP_NOACTIVATE);
}

/* ===== Render frame ===== */

static void render_frame() {
    if (!g_dib.bits) return;

    int w = g_dib.w;
    int h = g_dib.h;
    void* bits = g_dib.bits;

    if (gSettingsOpen) {
        // Dark translucent fullscreen background
        for (int y = 0; y < h; ++y)
            for (int x = 0; x < w; ++x)
                putpx_PM(bits, w, h, x, y, 160, 0, 0, 0);
    }
    else {
        std::memset(bits, 0, (size_t)w * (size_t)h * 4);
    }

    // Sphere position and size (small base to match original look)
    float cx = gSpherePosX * w;
    float cy = gSpherePosY * h;
    float baseScale = 0.13f; // tuned to match old ~420px overlay
    float sphereR = std::min(w, h) * baseScale * gSphereScale;

    draw_sphere(bits, w, h, cx, cy, sphereR, 22.f);

    // Ring aligned horizontally with sphere; vertical via slider
    float rx = std::min(w, h) * (baseScale * 1.1f) * gSphereScale;
    float ry = rx * 0.45f;
    float ringY = gRingOffsetY * h;

    draw_ring(bits, w, h, cx, ringY, rx, ry);

    if (gSettingsOpen) {
        draw_settings_panel();
    }
}

/* ===== Window proc ===== */

LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM wParam, LPARAM lParam) {
    switch (m) {
    case WM_NCHITTEST:
        if (!gSettingsOpen) {
            // Normal mode: full click-through
            return HTTRANSPARENT;
        }
        // In settings: everything is client; we'll handle hit logic manually
        return HTCLIENT;

    case WM_LBUTTONDOWN:
        if (gSettingsOpen) {
            SetCapture(h);
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);
            int w = g_dib.w;
            int hh = g_dib.h;

            // Recompute layout to match draw_settings_panel
            int innerX0 = gPanelX + 12;
            int innerX1 = gPanelX + PANEL_W - 12;
            int row = gPanelY + 40;
            int dy = 26;
            int ySphere = row;          row += dy;
            int yRing = row;          row += dy;
            int ySens = row;          row += dy;
            int yBass = row;          row += dy;
            int yLowMid = row;          row += dy;
            int yMid = row;          row += dy;
            int yHigh = row;          row += dy;
            int yAir = row;          row += dy;

            auto inSlider = [&](int sy) {
                const int halfH = 10;
                return (mx >= innerX0 && mx <= innerX1 &&
                    my >= sy - halfH && my <= sy + halfH);
                };

            // 1) Panel drag (title bar)
            int px0 = gPanelX;
            int py0 = gPanelY;
            int px1 = px0 + PANEL_W;
            int py1 = py0 + 24;
            if (mx >= px0 && mx <= px1 && my >= py0 && my <= py1) {
                gDraggingPanel = true;
                gPanelDragOffX = mx - gPanelX;
                gPanelDragOffY = my - gPanelY;
                gActiveSlider = -1;
                gDraggingSphere = false;
                return 0;
            }

            // 2) Sliders
            if (inSlider(ySphere)) {
                gActiveSlider = 0;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(0, t);
                return 0;
            }
            if (inSlider(yRing)) {
                gActiveSlider = 1;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(1, t);
                return 0;
            }
            if (inSlider(ySens)) {
                gActiveSlider = 2;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(2, t);
                return 0;
            }
            if (inSlider(yBass)) {
                gActiveSlider = 3;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(3, t);
                return 0;
            }
            if (inSlider(yLowMid)) {
                gActiveSlider = 4;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(4, t);
                return 0;
            }
            if (inSlider(yMid)) {
                gActiveSlider = 5;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(5, t);
                return 0;
            }
            if (inSlider(yHigh)) {
                gActiveSlider = 6;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(6, t);
                return 0;
            }
            if (inSlider(yAir)) {
                gActiveSlider = 7;
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(7, t);
                return 0;
            }

            // 3) Swap L/R checkbox region
            int cbX = innerX0;
            int cbY = yAir + 10;
            int cbS = 12;
            if (mx >= cbX && mx <= cbX + cbS + 70 &&
                my >= cbY - 4 && my <= cbY + cbS + 4)
            {
                gSwapLR = !gSwapLR;
                gActiveSlider = -1;
                gDraggingPanel = false;
                gDraggingSphere = false;
                ReleaseCapture();
                return 0;
            }

            // 4) Sphere drag (click near current sphere)
            {
                float cx = gSpherePosX * w;
                float cy = gSpherePosY * hh;
                float baseScale = 0.13f;
                float sphereR = std::min(w, hh) * baseScale * gSphereScale;
                float dx = (float)mx - cx;
                float dy2 = (float)my - cy;
                if (dx * dx + dy2 * dy2 <= sphereR * sphereR * 1.3f) {
                    gDraggingSphere = true;
                    gActiveSlider = -1;
                    gDraggingPanel = false;
                    return 0;
                }
            }

            // Otherwise: click on dark background; no-op
            return 0;
        }
        break;

    case WM_MOUSEMOVE:
        if (gSettingsOpen && (wParam & MK_LBUTTON)) {
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);
            int w = g_dib.w;
            int hh = g_dib.h;

            int innerX0 = gPanelX + 12;
            int innerX1 = gPanelX + PANEL_W - 12;

            if (gDraggingPanel) {
                gPanelX = mx - gPanelDragOffX;
                gPanelY = my - gPanelDragOffY;
                return 0;
            }
            if (gActiveSlider != -1) {
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(gActiveSlider, t);
                return 0;
            }
            if (gDraggingSphere) {
                gSpherePosX = std::clamp(mx / (float)w, 0.05f, 0.95f);
                gSpherePosY = std::clamp(my / (float)hh, 0.05f, 0.95f);
                return 0;
            }
        }
        break;

    case WM_LBUTTONUP:
        if (gSettingsOpen) {
            gActiveSlider = -1;
            gDraggingPanel = false;
            gDraggingSphere = false;
            ReleaseCapture();
            return 0;
        }
        break;

    case WM_SIZE: {
        int ww = LOWORD(lParam);
        int hh = HIWORD(lParam);
        if (ww > 0 && hh > 0) {
            free_dib(g_dib);
            g_dib = make_dib(ww, hh);
        }
        return 0;
    }

    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;

    default:
        return DefWindowProcW(h, m, wParam, lParam);
    }
}

/* ===== Entry / main loop ===== */

int APIENTRY wWinMain(HINSTANCE inst, HINSTANCE, LPWSTR, int) {
    // DPI awareness
    using SetProcessDpiAwarenessContext_t = BOOL(WINAPI*)(HANDLE);
    if (auto setCtx = (SetProcessDpiAwarenessContext_t)
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetProcessDpiAwarenessContext"))
    {
        setCtx((HANDLE)-4); // PER_MONITOR_AWARE_V2
    }

    // Window class
    WNDCLASSW wc{};
    wc.hInstance = inst;
    wc.lpfnWndProc = WndProc;
    wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW);
    wc.hbrBackground = nullptr;
    RegisterClassW(&wc);

    // Fullscreen overlay
    int screenW = GetSystemMetrics(SM_CXSCREEN);
    int screenH = GetSystemMetrics(SM_CYSCREEN);

    HWND h = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
        kClassName, L"DeafOverlay", WS_POPUP,
        0, 0, screenW, screenH,
        nullptr, nullptr, inst, nullptr);

    BOOL noShadow = FALSE;
    DwmSetWindowAttribute(h, 2, &noShadow, sizeof(noShadow));

    ShowWindow(h, SW_SHOWNA);
    UpdateWindow(h);

    // Hotkeys
    RegisterHotKey(h, HK_TOGGLE_VIS, MOD_ALT | MOD_NOREPEAT, 'H'); // Alt+H
    RegisterHotKey(h, HK_TOGGLE_CFG, MOD_ALT | MOD_NOREPEAT, 'I'); // Alt+I

    g_dib = make_dib(screenW, screenH);
    gPanelX = 40;
    gPanelY = 40;

    audio_init();

    bool overlayVisible = true;
    auto t0 = std::chrono::steady_clock::now();
    float tPrev = 0.f;

    MSG msg{};
    while (true) {
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) {
                free_dib(g_dib);
                UnregisterHotKey(h, HK_TOGGLE_VIS);
                UnregisterHotKey(h, HK_TOGGLE_CFG);
                audio_shutdown();
                return 0;
            }

            if (msg.message == WM_HOTKEY) {
                if (msg.wParam == HK_TOGGLE_VIS) {
                    overlayVisible = !overlayVisible;
                    ShowWindow(h, overlayVisible ? SW_SHOWNA : SW_HIDE);
                    if (overlayVisible) {
                        SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                    }
                }
                else if (msg.wParam == HK_TOGGLE_CFG) {
                    if (!overlayVisible) {
                        overlayVisible = true;
                        ShowWindow(h, SW_SHOWNA);
                        SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                    }
                    set_settings_mode(h, !gSettingsOpen);
                }
                continue;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Time step
        float t = std::chrono::duration<float>(
            std::chrono::steady_clock::now() - t0).count();
        float dt = t - tPrev;
        tPrev = t;

        // Pull band energies
        float EL[NB], ER[NB];
        for (int b = 0; b < NB; ++b) {
            EL[b] = gEL[b].load(std::memory_order_relaxed);
            ER[b] = gER[b].load(std::memory_order_relaxed);
        }

        // Pinna / elevation / front/back scoring
        auto L1p = [](float v) { return std::log1p(std::max(0.f, v)); };
        const float Ww[NB] = { 0.0f, 0.2f, 0.6f, 1.0f, 0.9f, 0.2f };

        float elevInst = 0.f;
        float backInst = 0.f;
        float frontInst = 0.f;

        for (int b = 0; b < NB; ++b) {
            float e25L = EL[2], e25R = ER[2];
            float e58L = EL[3], e58R = ER[3];
            float e812L = EL[4], e812R = ER[4];

            float L25 = L1p(e25L), R25 = L1p(e25R);
            float L58 = L1p(e58L), R58 = L1p(e58R);
            float L812 = L1p(e812L), R812 = L1p(e812R);

            float elevB = std::clamp(0.5f * ((R812 - R58) + (L812 - L58)), -1.f, 1.f);
            float notchL = L58 - 0.5f * (L25 + L812);
            float notchR = R58 - 0.5f * (R25 + R812);
            float rearB = std::clamp(0.5f * (notchL + notchR), 0.f, 1.f);
            float frontB = std::clamp(1.f - rearB, 0.f, 1.f);

            gBackB[b] = ema(gBackB[b], rearB, 0.18f);
            gFrontB[b] = ema(gFrontB[b], frontB, 0.18f);

            elevInst += Ww[b] * elevB;
            backInst += Ww[b] * gBackB[b];
            frontInst += Ww[b] * gFrontB[b];
        }

        float denom = std::max(1e-6f, (Ww[2] + Ww[3] + Ww[4]));
        float elevScore = std::clamp(ema(gElevS, elevInst / denom, 0.12f), -1.f, 1.f);
        gElevS = elevScore;
        float backScore = std::clamp(backInst / denom, 0.f, 1.f);
        float frontScore = std::clamp(frontInst / denom, 0.f, 1.f);

        // Decay peaks
        for (auto& p : g_peaks) if (p.life > 0.f) {
            p.life -= dt;
            p.amp *= std::exp(-dt * PEAK_DECAY_HZ);
            if (p.life <= 0.f || p.amp < 0.02f) p = Peak{};
        }

        const float maxAng = MAX_AZIMUTH_DEG * PI_F / 180.f;

        // Build peaks: for each band, Left / Center / Right + rear mirrors
        for (int b = 0; b < NB; ++b) {
            float L = EL[b];
            float R = ER[b];
            float S = L + R;
            if (S <= 1e-6f) continue;

            float ild = (gSwapLR ? (L - R) : (R - L)) / S; // right-positive
            ild = std::clamp(ild, -1.f, 1.f);

            float center = std::min(L, R);
            float left = std::max(0.f, L - R);
            float right = std::max(0.f, R - L);

            auto norm = [](float v) { return std::tanh(1.2f * v); };

            // Front/mono
            {
                float a = norm(center);
                if (a > 0.01f) {
                    float rearDom = std::clamp(backScore - frontScore, 0.f, 1.f);
                    a *= (1.0f - 0.25f * rearDom);
                    Peak q;
                    q.ang = 0.f;
                    q.amp = a;
                    q.life = 0.50f;
                    q.band = b;
                    q.rearGain = gBackB[b];
                    q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }
            // Left
            {
                float a = norm(left);
                if (a > 0.01f) {
                    float frac = (L > 0.f ? left / std::max(L, 1e-6f) : 0.f);
                    float ang = -maxAng * std::clamp(0.35f + 0.65f * frac, 0.f, 1.f);
                    Peak q;
                    q.ang = ang;
                    q.amp = a;
                    q.life = 0.55f;
                    q.band = b;
                    q.rearGain = gBackB[b];
                    q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }
            // Right
            {
                float a = norm(right);
                if (a > 0.01f) {
                    float frac = (R > 0.f ? right / std::max(R, 1e-6f) : 0.f);
                    float ang = +maxAng * std::clamp(0.35f + 0.65f * frac, 0.f, 1.f);
                    Peak q;
                    q.ang = ang;
                    q.amp = a;
                    q.life = 0.55f;
                    q.band = b;
                    q.rearGain = gBackB[b];
                    q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                    add_peak_merge(q);
                }
            }

            // Rear mirrors
            float backB = gBackB[b];
            if (backB > 0.10f) {
                float rearScale = std::clamp(backB, 0.10f, 1.0f);

                // Rear center
                {
                    float a = norm(center) * rearScale * 0.85f;
                    if (a > 0.01f) {
                        Peak q;
                        q.ang = PI_F;
                        q.amp = a;
                        q.life = 0.45f;
                        q.band = b;
                        q.rearGain = backB;
                        q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                        add_peak_merge(q);
                    }
                }
                // Rear left
                if (left > 0.f) {
                    float frac = (L > 0.f ? left / std::max(L, 1e-6f) : 0.f);
                    float angL = -maxAng * std::clamp(0.35f + 0.65f * frac, 0.f, 1.f);
                    float a = norm(left) * rearScale * 0.75f;
                    if (a > 0.01f) {
                        Peak q;
                        q.ang = std::remainder(angL + PI_F, 2 * PI_F);
                        q.amp = a;
                        q.life = 0.45f;
                        q.band = b;
                        q.rearGain = backB;
                        q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                        add_peak_merge(q);
                    }
                }
                // Rear right
                if (right > 0.f) {
                    float frac = (R > 0.f ? right / std::max(R, 1e-6f) : 0.f);
                    float angR = +maxAng * std::clamp(0.35f + 0.65f * frac, 0.f, 1.f);
                    float a = norm(right) * rearScale * 0.75f;
                    if (a > 0.01f) {
                        Peak q;
                        q.ang = std::remainder(angR + PI_F, 2 * PI_F);
                        q.amp = a;
                        q.life = 0.45f;
                        q.band = b;
                        q.rearGain = backB;
                        q.elevFactor = 1.0f + elevScore * ELEV_SCALE;
                        add_peak_merge(q);
                    }
                }
            }
        }

        render_frame();
        blit_layered(h, g_dib.dc, g_dib.w, g_dib.h);

        Sleep(16);
    }
}
