// DeafOverlay.cpp — Fullscreen spatial overlay with settings, per-band controls, info tooltips, and game presets
// - Alt+H: toggle overlay
// - Alt+I: settings mode
//   * Fullscreen dark background (captures clicks)
//   * Movable settings panel
//   * Sphere: size slider + drag to position
//   * Ring: vertical offset slider
//   * Global sensitivity slider
//   * 6-band EQ-style sliders (Bass..Air)
//   * Swap L/R
//   * Game preset checkboxes (CS2 / R6 Siege / Call of Duty)
//   * Info ("i") buttons explaining each control for deaf / hard of hearing users
// - Static hint box at top-left (always visible): hotkeys reminder
// - Normal mode: click-through, transparent, spatial visual only
// - Uses WASAPI loopback via miniaudio

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
static float gRingOffsetY = 0.66f;  // slider: 0.40 .. 0.90 (screen fraction)
static float gSensitivity = 1.0f;   // slider: 0.5 .. 2.0
static bool  gSwapLR = false;       // swap channels
static bool  gSettingsOpen = false; // Alt+I

// Sphere position (normalized 0..1)
static float gSpherePosX = 0.5f;
static float gSpherePosY = 0.47f;

/* ===== Frequency bands ===== */
static constexpr int NB = 6;
// Per-band user gains (set via sliders)
static float gBandGain[NB] = { 1.f,1.f,1.f,1.f,1.f,1.f };

/* ===== Game presets ===== */
enum class GamePreset : int {
    None = 0,
    CS2,
    R6S,
    COD
};
static GamePreset gPreset = GamePreset::None;

static void apply_preset(GamePreset p) {
    gPreset = p;

    // Reasonable “starter” tuning (you can refine with real gameplay tests):
    // Bands: 0 Bass (0-250), 1 LowMid (250-2k), 2 Mid (2-5k), 3 HighMid (5-8k), 4 Air (8-12k), 5 Ultra/Total (12k+ / residual)
    if (p == GamePreset::None) {
        // Do nothing (keep current)
        return;
    }

    if (p == GamePreset::CS2) {
        // CS2: footsteps/utility detail often sits in mids/high-mids; tame bass rumble.
        gSensitivity = 1.20f;
        gBandGain[0] = 0.75f; // bass down
        gBandGain[1] = 1.05f;
        gBandGain[2] = 1.35f; // mid up
        gBandGain[3] = 1.45f; // high-mid up
        gBandGain[4] = 1.20f; // air slightly up
        gBandGain[5] = 1.00f;
    }
    else if (p == GamePreset::R6S) {
        // R6 Siege: lots of subtle movement + gadget cues; favor low-mid through high-mid, keep bass controlled.
        gSensitivity = 1.25f;
        gBandGain[0] = 0.85f;
        gBandGain[1] = 1.20f;
        gBandGain[2] = 1.30f;
        gBandGain[3] = 1.35f;
        gBandGain[4] = 1.10f;
        gBandGain[5] = 1.00f;
    }
    else if (p == GamePreset::COD) {
        // Call of Duty: explosions can drown detail; keep bass but boost low-mid/mid to retain footsteps/reload presence.
        gSensitivity = 1.10f;
        gBandGain[0] = 1.10f;
        gBandGain[1] = 1.25f;
        gBandGain[2] = 1.25f;
        gBandGain[3] = 1.15f;
        gBandGain[4] = 0.95f;
        gBandGain[5] = 1.00f;
    }
}

static void set_preset_exclusive(GamePreset p) {
    if (gPreset == p) {
        // uncheck -> go None (leave current values as-is)
        gPreset = GamePreset::None;
        return;
    }
    apply_preset(p);
}

/* ===== Settings panel state ===== */
static const int PANEL_W = 360;
static const int PANEL_H = 420;

static int  gPanelX = 40;
static int  gPanelY = 40;
static bool gDraggingPanel = false;
static int  gPanelDragOffX = 0;
static int  gPanelDragOffY = 0;

// Active slider index during drag
// 0 sphere, 1 ringY, 2 sens, 3..8 bands 0..5
static int gActiveSlider = -1;

// Sphere drag in settings mode
static bool gDraggingSphere = false;

/* ===== Info button support ===== */
struct InfoRegion {
    int x0, y0, x1, y1;
    const wchar_t* text;
};
static InfoRegion gInfoRegions[48];
static int gInfoCount = 0;

/* ===== Preset checkbox hit regions ===== */
struct HitRegion {
    int x0, y0, x1, y1;
    int id; // arbitrary
};
static HitRegion gHitRegions[32];
static int gHitCount = 0;

static void add_hit_region(int x0, int y0, int x1, int y1, int id) {
    if (gHitCount >= (int)(sizeof(gHitRegions) / sizeof(gHitRegions[0]))) return;
    gHitRegions[gHitCount++] = { x0,y0,x1,y1,id };
}

/* ===== Explanations (short, game-focused) ===== */
static const wchar_t* kInfoSphereSize =
L"Sphere size:\nAdjust how large the 3D cue appears.\nSmaller = subtle, larger = easier to see peripheral threats.";

static const wchar_t* kInfoRingY =
L"Ring vertical position:\nMove the horizontal ring up/down.\nPlace it near your crosshair/focus area for faster reading.";

static const wchar_t* kInfoSensitivity =
L"Global sensitivity:\nControls how strongly all sounds light up.\nLower = only louder/closer sounds.\nHigher = includes quiet cues like footsteps and reloads.";

static const wchar_t* kInfoBass =
L"Bass (0–250 Hz):\nFootsteps thuds, rumbles, explosions, distant booms.\nIncrease to see low movement.\nReduce if constant rumble is too noisy.";

static const wchar_t* kInfoLowMid =
L"Low-Mid (250–2 kHz):\nBody of gunshots, doors, movement, voices.\nGood for tracking nearby players.";

static const wchar_t* kInfoMid =
L"Mid (2–5 kHz):\nClarity of gunshots, glass, impacts, voice detail.\nImportant for precise combat cues.";

static const wchar_t* kInfoHighMid =
L"High-Mid (5–8 kHz):\nSharp details: metallic pings, shell drops, some footsteps.\nHelps separate close threats from background.";

static const wchar_t* kInfoAir =
L"Treble / Air (8 kHz+):\nVery high details: hiss, sparks, UI beeps.\nTurn up to catch subtle directional cues, down if too busy.";

static const wchar_t* kInfoSwapLR =
L"Swap L/R:\nFlip left and right.\nUse if your headset or game output feels reversed.";

static const wchar_t* kInfoPreset =
L"Game presets:\nOne-click tuning for common games.\nThese adjust sensitivity + band gains to make key cues easier to see.\nYou can still fine-tune sliders after selecting.";

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
static inline void putpx_PM(void* bits, int w, int h,
    int x, int y, BYTE a, BYTE r, BYTE g, BYTE b)
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

/* ===== Info icon helpers ===== */
static void add_info_region(int cx, int cy, const wchar_t* text) {
    if (gInfoCount >= (int)(sizeof(gInfoRegions) / sizeof(gInfoRegions[0]))) return;
    int r = 7;
    gInfoRegions[gInfoCount++] = { cx - r, cy - r, cx + r, cy + r, text };
}

static void draw_info_icon(void* bits, int w, int h, int cx, int cy) {
    int r = 6;
    // Circle
    for (int dy = -r; dy <= r; ++dy) {
        for (int dx = -r; dx <= r; ++dx) {
            if (dx * dx + dy * dy <= r * r) {
                BYTE a = 220;
                putpx_PM(bits, w, h, cx + dx, cy + dy, a, 40, 40, 60);
            }
        }
    }
    // "i"
    putpx_PM(bits, w, h, cx, cy - 2, 255, 255, 255, 255);
    for (int yy = -1; yy <= 2; ++yy)
        putpx_PM(bits, w, h, cx, cy + yy, 255, 255, 255, 255);
}

/* ===== Band energies (atomic from callback) ===== */
static std::atomic<float> gEL[NB], gER[NB];

static inline float lp_step(float y, float x, float a) { return y + a * (x - y); }

/* ===== Peaks / elevation ===== */
constexpr float PI_F = 3.14159265358979323846f;
static float gBackB[NB] = { 0 };
static float gFrontB[NB] = { 0 };
static float gElevS = 0.f;
static inline float ema(float prev, float x, float a) { return prev + a * (x - prev); }

struct Peak {
    float ang = 0.f;
    float amp = 0.f;
    float life = 0.f;
    int   band = 0;
    float rearGain = 0.f;
    float elevFactor = 1.f;
};
static Peak g_peaks[18];

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
    const float a250 = a_fc(250.f),
        a2k = a_fc(2000.f),
        a5k = a_fc(5000.f),
        a8k = a_fc(8000.f),
        a12k = a_fc(12000.f);
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

    // Global & per-band gain
    float g = 1.8f * BASE_SENS_GAIN * gSensitivity;

    // Subtle built-in emphasis for lows/mids
    bL[0] *= 1.25f; bR[0] *= 1.25f;
    bL[1] *= 1.10f; bR[1] *= 1.10f;

    for (int k = 0; k < NB; ++k) {
        float bandGain = std::clamp(gBandGain[k], 0.0f, 3.0f);
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
                float gg = win * p.amp;
                if (gg > bestGlow) { bestGlow = gg; bestAmp = p.amp; }
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
        if (t <= 0.5f)
            gBandGain[b] = (t / 0.5f) * 1.0f;                 // 0..1
        else
            gBandGain[b] = 1.0f + ((t - 0.5f) / 0.5f) * 2.0f; // 1..3
    }
}

static float slider_t_from_value(int idx) {
    if (idx == 0)
        return std::clamp((gSphereScale - 0.6f) / (1.6f - 0.6f), 0.0f, 1.0f);
    if (idx == 1)
        return std::clamp((gRingOffsetY - 0.40f) / (0.90f - 0.40f), 0.0f, 1.0f);
    if (idx == 2)
        return std::clamp((gSensitivity - 0.5f) / (2.0f - 0.5f), 0.0f, 1.0f);
    if (idx >= 3 && idx < 3 + NB) {
        int b = idx - 3;
        float gg = std::clamp(gBandGain[b], 0.0f, 3.0f);
        if (gg <= 1.0f) return 0.5f * (gg / 1.0f);
        return 0.5f + 0.5f * ((gg - 1.0f) / 2.0f);
    }
    return 0.0f;
}

static void draw_slider_bar(void* bits, int w, int h,
    int x0, int y, int x1, float t)
{
    for (int x = x0; x <= x1; ++x)
        putpx_PM(bits, w, h, x, y, 90, 220, 220, 220);

    int knobX = x0 + int(t * float(x1 - x0));
    for (int dy = -4; dy <= 4; ++dy)
        for (int dx = -3; dx <= 3; ++dx)
            putpx_PM(bits, w, h, knobX + dx, y + dy, 210, 255, 180, 80);
}

/* ===== Small UI primitives ===== */
static void draw_box_outline(void* bits, int w, int h, int x, int y, int s, BYTE a = 220) {
    for (int yy = 0; yy < s; ++yy)
        for (int xx = 0; xx < s; ++xx) {
            bool border = (yy == 0 || yy == s - 1 || xx == 0 || xx == s - 1);
            putpx_PM(bits, w, h, x + xx, y + yy, border ? a : 40, 255, 255, 255);
        }
}
static void draw_check_mark(void* bits, int w, int h, int x, int y, int s) {
    // simple X mark
    for (int i = 2; i < s - 2; ++i) {
        putpx_PM(bits, w, h, x + i, y + i, 230, 255, 220, 80);
        putpx_PM(bits, w, h, x + (s - 1 - i), y + i, 230, 255, 220, 80);
    }
}

/* ===== Hint box (top-left) ===== */
static void draw_hint_box() {
    if (!g_dib.bits) return;
    int w = g_dib.w, h = g_dib.h;
    void* bits = g_dib.bits;

    const int x0 = 14;
    const int y0 = 14;
    const int bw = 290;
    const int bh = 54;
    const int x1 = std::min(w - 14, x0 + bw);
    const int y1 = std::min(h - 14, y0 + bh);

    // background
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 170, 12, 12, 14);

    // top strip
    for (int y = y0; y < std::min(y1, y0 + 18); ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 200, 28, 28, 34);

    HDC dc = g_dib.dc;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    HFONT hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    HFONT oldF = (HFONT)SelectObject(dc, hFont);

    TextOutW(dc, x0 + 10, y0 + 2, L"DeafOverlay Hotkeys", 18);
    TextOutW(dc, x0 + 10, y0 + 22, L"Alt+H: Show/Hide   Alt+I: Settings", 36);

    SelectObject(dc, oldF);
}

/* ===== Settings panel drawing ===== */
static void draw_settings_panel() {
    if (!gSettingsOpen || !g_dib.bits) return;

    int w = g_dib.w, h = g_dib.h;
    void* bits = g_dib.bits;

    gInfoCount = 0; // rebuild info hit regions
    gHitCount = 0;  // rebuild UI hit regions

    // Clamp panel
    if (gPanelX < 10) gPanelX = 10;
    if (gPanelY < 10) gPanelY = 10;
    if (gPanelX + PANEL_W > w - 10) gPanelX = w - 10 - PANEL_W;
    if (gPanelY + PANEL_H > h - 10) gPanelY = h - 10 - PANEL_H;

    int x0 = gPanelX;
    int y0 = gPanelY;
    int x1 = x0 + PANEL_W;
    int y1 = y0 + PANEL_H;

    // Background
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 205, 16, 16, 20);

    // Title bar
    for (int y = y0; y < y0 + 24; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 235, 32, 32, 40);

    int innerX0 = x0 + 12;
    int innerX1 = x1 - 32; // leave room for info icons on the right
    int row = y0 + 40;
    int dy = 26;

    int ySphere = row;   row += dy;
    int yRing = row;   row += dy;
    int ySens = row;   row += dy;
    int yBass = row;   row += dy;
    int yLowMid = row;   row += dy;
    int yMid = row;   row += dy;
    int yHigh = row;   row += dy;
    int yAir = row;   row += dy;

    // Sliders
    draw_slider_bar(bits, w, h, innerX0, ySphere, innerX1, slider_t_from_value(0));
    draw_slider_bar(bits, w, h, innerX0, yRing, innerX1, slider_t_from_value(1));
    draw_slider_bar(bits, w, h, innerX0, ySens, innerX1, slider_t_from_value(2));
    draw_slider_bar(bits, w, h, innerX0, yBass, innerX1, slider_t_from_value(3));
    draw_slider_bar(bits, w, h, innerX0, yLowMid, innerX1, slider_t_from_value(4));
    draw_slider_bar(bits, w, h, innerX0, yMid, innerX1, slider_t_from_value(5));
    draw_slider_bar(bits, w, h, innerX0, yHigh, innerX1, slider_t_from_value(6));
    draw_slider_bar(bits, w, h, innerX0, yAir, innerX1, slider_t_from_value(7));

    // Text labels with info icons
    HDC dc = g_dib.dc;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    HFONT hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    HFONT oldF = (HFONT)SelectObject(dc, hFont);

    TextOutW(dc, x0 + 10, y0 + 5, L"DeafOverlay Settings", 21);

    auto label_with_info = [&](int y, const wchar_t* txt, const wchar_t* info) {
        TextOutW(dc, innerX0, y - 14, txt, (int)wcslen(txt));
        int ix = x1 - 14;
        int iy = y - 10;
        draw_info_icon(bits, w, h, ix, iy);
        add_info_region(ix, iy, info);
        };

    label_with_info(ySphere, L"Sphere size", kInfoSphereSize);
    label_with_info(yRing, L"Ring vertical position", kInfoRingY);
    label_with_info(ySens, L"Global sensitivity", kInfoSensitivity);
    label_with_info(yBass, L"Bass (0-250 Hz)", kInfoBass);
    label_with_info(yLowMid, L"Low-Mid (250-2k)", kInfoLowMid);
    label_with_info(yMid, L"Mid (2-5k)", kInfoMid);
    label_with_info(yHigh, L"High-Mid (5-8k)", kInfoHighMid);
    label_with_info(yAir, L"Treble / Air (8k+)", kInfoAir);

    // Swap L/R checkbox
    int cbX = innerX0;
    int cbY = yAir + 12;
    int cbS = 12;

    draw_box_outline(bits, w, h, cbX, cbY, cbS);
    if (gSwapLR) draw_check_mark(bits, w, h, cbX, cbY, cbS);

    TextOutW(dc, cbX + cbS + 6, cbY - 1, L"Swap L/R", 8);

    // Info icon for Swap L/R
    int swapInfoX = x1 - 14;
    int swapInfoY = cbY + 2;
    draw_info_icon(bits, w, h, swapInfoX, swapInfoY);
    add_info_region(swapInfoX, swapInfoY, kInfoSwapLR);

    // Add hit region for swap toggle (id 100)
    add_hit_region(cbX, cbY - 4, cbX + cbS + 120, cbY + cbS + 4, 100);

    // --- Game presets section ---
    int presY = cbY + 28;
    TextOutW(dc, innerX0, presY - 2, L"Game presets", 12);

    // Info icon for presets
    int presInfoX = x1 - 14;
    int presInfoY = presY + 2;
    draw_info_icon(bits, w, h, presInfoX, presInfoY);
    add_info_region(presInfoX, presInfoY, kInfoPreset);

    int pBoxS = 12;
    int pRowY = presY + 18;

    auto draw_preset_row = [&](int rowY, GamePreset preset, const wchar_t* name, int hitId) {
        int bx = innerX0;
        int by = rowY;
        draw_box_outline(bits, w, h, bx, by, pBoxS);
        if (gPreset == preset) draw_check_mark(bits, w, h, bx, by, pBoxS);
        TextOutW(dc, bx + pBoxS + 6, by - 1, name, (int)wcslen(name));
        add_hit_region(bx, by - 4, bx + 240, by + pBoxS + 4, hitId);
        };

    draw_preset_row(pRowY + 0, GamePreset::CS2, L"CS2", 200);
    draw_preset_row(pRowY + 18, GamePreset::R6S, L"Rainbow Six Siege", 201);
    draw_preset_row(pRowY + 36, GamePreset::COD, L"Call of Duty", 202);

    // Small note line
    SetTextColor(dc, RGB(220, 220, 220));
    TextOutW(dc, innerX0, pRowY + 58, L"(Select one, then fine-tune sliders)", 36);
    SetTextColor(dc, RGB(255, 255, 255));

    SelectObject(dc, oldF);
}

/* ===== Settings mode toggle ===== */
static const wchar_t* kClassName = L"DeafOverlayWnd";
enum : int { HK_TOGGLE_VIS = 1, HK_TOGGLE_CFG = 2 };

static void set_settings_mode(HWND h, bool on) {
    gSettingsOpen = on;
    LONG ex = GetWindowLongW(h, GWL_EXSTYLE);
    if (on) {
        ex &= ~WS_EX_TRANSPARENT;  // capture clicks
    }
    else {
        ex |= WS_EX_TRANSPARENT;   // click-through
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
        // Dark fullscreen mask
        for (int y = 0; y < h; ++y)
            for (int x = 0; x < w; ++x)
                putpx_PM(bits, w, h, x, y, 160, 0, 0, 0);
    }
    else {
        std::memset(bits, 0, (size_t)w * (size_t)h * 4);
    }

    // Always draw hint box (visual only)
    draw_hint_box();

    float cx = gSpherePosX * w;
    float cy = gSpherePosY * h;

    float baseScale = 0.13f; // matches our 420px baseline feel
    float sphereR = std::min(w, h) * baseScale * gSphereScale;

    draw_sphere(bits, w, h, cx, cy, sphereR, 22.f);

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
        if (!gSettingsOpen)
            return HTTRANSPARENT; // full click-through in normal mode
        return HTCLIENT;          // settings mode: handle manually

    case WM_LBUTTONDOWN:
        if (gSettingsOpen) {
            SetCapture(h);
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);
            int ww = g_dib.w;
            int hh = g_dib.h;

            // 0) Info icons: if clicked, show description
            for (int i = 0; i < gInfoCount; ++i) {
                const auto& inf = gInfoRegions[i];
                if (mx >= inf.x0 && mx <= inf.x1 &&
                    my >= inf.y0 && my <= inf.y1) {
                    MessageBoxW(h, inf.text, L"Overlay Setting Info",
                        MB_OK | MB_ICONINFORMATION);
                    ReleaseCapture();
                    return 0;
                }
            }

            // 0.5) General hit regions (checkboxes etc.)
            for (int i = 0; i < gHitCount; ++i) {
                const auto& hr = gHitRegions[i];
                if (mx >= hr.x0 && mx <= hr.x1 && my >= hr.y0 && my <= hr.y1) {
                    if (hr.id == 100) {
                        gSwapLR = !gSwapLR;
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 200) { set_preset_exclusive(GamePreset::CS2); ReleaseCapture(); return 0; }
                    if (hr.id == 201) { set_preset_exclusive(GamePreset::R6S); ReleaseCapture(); return 0; }
                    if (hr.id == 202) { set_preset_exclusive(GamePreset::COD); ReleaseCapture(); return 0; }
                }
            }

            // Reconstruct panel geometry (must match draw_settings_panel)
            int innerX0 = gPanelX + 12;
            int innerX1 = gPanelX + PANEL_W - 32;
            int row = gPanelY + 40;
            int dy = 26;

            int ySphere = row;   row += dy;
            int yRing = row;   row += dy;
            int ySens = row;   row += dy;
            int yBass = row;   row += dy;
            int yLowMid = row;   row += dy;
            int yMid = row;   row += dy;
            int yHigh = row;   row += dy;
            int yAir = row;   row += dy;

            auto inSlider = [&](int sy) {
                const int halfH = 10;
                return (mx >= innerX0 && mx <= innerX1 &&
                    my >= sy - halfH && my <= sy + halfH);
                };

            // 1) Panel drag via title bar
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
            if (inSlider(ySphere)) { gActiveSlider = 0; slider_apply(0, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yRing)) { gActiveSlider = 1; slider_apply(1, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(ySens)) { gActiveSlider = 2; slider_apply(2, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yBass)) { gActiveSlider = 3; slider_apply(3, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yLowMid)) { gActiveSlider = 4; slider_apply(4, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yMid)) { gActiveSlider = 5; slider_apply(5, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yHigh)) { gActiveSlider = 6; slider_apply(6, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            if (inSlider(yAir)) { gActiveSlider = 7; slider_apply(7, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }

            // 3) Sphere drag (inside sphere radius)
            {
                float cx = gSpherePosX * ww;
                float cy = gSpherePosY * hh;
                float baseScale = 0.13f;
                float sphereR = std::min(ww, hh) * baseScale * gSphereScale;
                float dx = (float)mx - cx;
                float dy2 = (float)my - cy;
                if (dx * dx + dy2 * dy2 <= sphereR * sphereR * 1.3f) {
                    gDraggingSphere = true;
                    gActiveSlider = -1;
                    gDraggingPanel = false;
                    return 0;
                }
            }

            // Click on dim background: no-op
            return 0;
        }
        break;

    case WM_MOUSEMOVE:
        if (gSettingsOpen && (wParam & MK_LBUTTON)) {
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);
            int ww = g_dib.w;
            int hh = g_dib.h;
            int innerX0 = gPanelX + 12;
            int innerX1 = gPanelX + PANEL_W - 32;

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
                gSpherePosX = std::clamp(mx / (float)ww, 0.05f, 0.95f);
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

    // Class
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

        float t = std::chrono::duration<float>(
            std::chrono::steady_clock::now() - t0).count();
        float dt = t - tPrev;
        tPrev = t;

        // Read energies
        float EL[NB], ER[NB];
        for (int b = 0; b < NB; ++b) {
            EL[b] = gEL[b].load(std::memory_order_relaxed);
            ER[b] = gER[b].load(std::memory_order_relaxed);
        }

        // Pinna / elevation / rear estimate (same idea as before)
        auto L1p = [](float v) { return std::log1p(std::max(0.f, v)); };
        const float Ww[NB] = { 0.0f, 0.2f, 0.6f, 1.0f, 0.9f, 0.2f };

        float elevInst = 0.f, backInst = 0.f, frontInst = 0.f;
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

        // Build peaks from band energies
        for (int b = 0; b < NB; ++b) {
            float L = EL[b];
            float R = ER[b];
            float S = L + R;
            if (S <= 1e-6f) continue;

            float ild = (gSwapLR ? (L - R) : (R - L)) / S; // + = right
            ild = std::clamp(ild, -1.f, 1.f);
            (void)ild;

            float center = std::min(L, R);
            float left = std::max(0.f, L - R);
            float right = std::max(0.f, R - L);

            auto norm = [](float v) { return std::tanh(1.2f * v); };

            // Front: shared energy, but damped if strong rear cue
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
            // Left continuous
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
            // Right continuous
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

            // Rear mirrors when pinna says "back"
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
