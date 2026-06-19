// DeafOverlay.cpp — Fullscreen spatial overlay with settings, per-band controls, info tooltips, and accuracy-first defaults
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
//
// ACCURACY MODEL:
// 1) One-time first-run guidance (stereo or 7.1 output, spatializers OFF) stored in HKCU registry.
// 2) Auto noise-floor tracking per channel/band + auto gain normalization ("install and done").
// 3) Transient emphasis: direction authority favors transients (shots/steps) over ambience.
// 4) DIRECTION is adaptive to the captured channel layout (capture.channels = 0 => native):
//      - Multichannel (>=6ch, e.g. 5.1/7.1): true 360 deg from discrete channel energies mapped to
//        their physical speaker azimuths (energy-weighted direction vector). Front/back is real.
//      - Stereo (2ch): honest left/right front-arc only, from inter-channel level difference (ILD).
//        Front/back & elevation are NOT recoverable from a stereo downmix, so we do not fake them.
//
// NOTE: A stereo downmix physically cannot encode front/back/height. For full 360 the user must run a
// real 5.1/7.1 output with spatial sound (Windows Sonic/Atmos/DTS) OFF so loopback sees discrete channels.

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
#include <shlobj.h>      // SHGetKnownFolderPath
#include <shellapi.h>    // ShellExecuteW (deep-link to Windows settings)
#include <string>
#include <fstream>
#include <iterator>
#include <filesystem>

#pragma comment(lib, "Dwmapi.lib")
#pragma comment(lib, "Shell32.lib")

/* ===== Base tunables ===== */
static constexpr float BASE_SENS_GAIN = 0.95f;
static constexpr float MAX_AZIMUTH_DEG = 90.0f;   // per-side span on ring (full quarter turn)
static constexpr float PEAK_DECAY_HZ = 1.0f;      // slower fade = more visible
static constexpr float REAR_SCALE = 1.00f;
static constexpr float ILD_K = 2.5f;              // ILD→angle curve steepness

/* ===== Accuracy-first tunables (new) ===== */
static constexpr float FLOOR_ATTACK = 0.0035f;  // how fast noise floor rises
static constexpr float FLOOR_RELEASE = 0.0008f;  // how fast noise floor falls
static constexpr float FLOOR_MULT = 1.05f;    // subtract floor*mult from energy (was 1.25, less aggressive)
static constexpr float AUTO_GAIN_RATE = 0.015f;   // how fast auto gain adapts
static constexpr float TARGET_LEVEL = 0.24f;    // target normalized “activity” for easy visibility

static constexpr float TRANSIENT_ALPHA = 0.20f;   // transient detector smoothing
static constexpr float TRANSIENT_HI = 0.18f;   // transient threshold (normalized) for “direction authority”

/* ===== Runtime user settings ===== */
static float gSphereScale = 1.0f;   // slider: 0.6 .. 1.6
static float gRingOffsetY = 0.66f;  // slider: 0.40 .. 0.90 (screen fraction)
static float gSensitivity = 1.0f;   // slider: 0.5 .. 2.0
static bool  gSwapLR = false;       // swap channels
static bool  gSettingsOpen = false; // Alt+I
static bool  gShowSetup = false;    // within the settings window: Setup view vs Controls view
static bool  gFirstRun = false;     // true on first launch -> auto-open the Setup view

// Sphere position (normalized 0..1)
static float gSpherePosX = 0.5f;
static float gSpherePosY = 0.47f;

/* ===== Frequency bands ===== */
static constexpr int NB = 6;
// Per-band user gains (set via sliders)
static float gBandGain[NB] = { 1.f,1.f,1.f,1.f,1.f,1.f };

/* ===== Game presets ===== */
enum class GamePreset : int { None = 0, CS2, R6S, COD };
static GamePreset gPreset = GamePreset::None;

static void force_alpha_rect(void* bits, int w, int h, int x0, int y0, int x1, int y1) {
    x0 = std::max(0, x0); y0 = std::max(0, y0);
    x1 = std::min(w, x1); y1 = std::min(h, y1);

    for (int y = y0; y < y1; ++y) {
        BYTE* row = (BYTE*)bits + ((size_t)y * w + x0) * 4;
        for (int x = x0; x < x1; ++x) {
            row[3] = 255; // alpha channel
            row += 4;
        }
    }
}

static void text_out_opaque(HDC dc, void* bits, int w, int h, int x, int y, const wchar_t* s) {
    int len = (int)wcslen(s);

    // Draw text (RGB)
    TextOutW(dc, x, y, s, len);

    // Measure the exact drawn area
    SIZE sz{};
    GetTextExtentPoint32W(dc, s, len, &sz);

    // Force alpha to opaque where the text lives (+2px padding for glyph edges)
    force_alpha_rect(bits, w, h, x - 2, y - 2, x + sz.cx + 2, y + sz.cy + 2);
}


static void apply_preset(GamePreset p) {
    gPreset = p;

    // Bands: 0 Bass (0-250), 1 LowMid (250-2k), 2 Mid (2-5k), 3 HighMid (5-8k), 4 Air (8-12k), 5 Ultra/Total (12k+ / residual)
    if (p == GamePreset::None) return;

    if (p == GamePreset::CS2) {
        gSensitivity = 1.20f;
        gBandGain[0] = 0.75f;
        gBandGain[1] = 1.05f;
        gBandGain[2] = 1.35f;
        gBandGain[3] = 1.45f;
        gBandGain[4] = 1.20f;
        gBandGain[5] = 1.00f;
    }
    else if (p == GamePreset::R6S) {
        gSensitivity = 1.25f;
        gBandGain[0] = 0.85f;
        gBandGain[1] = 1.20f;
        gBandGain[2] = 1.30f;
        gBandGain[3] = 1.35f;
        gBandGain[4] = 1.10f;
        gBandGain[5] = 1.00f;
    }
    else if (p == GamePreset::COD) {
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
    if (gPreset == p) { gPreset = GamePreset::None; return; }
    apply_preset(p);
}

/* ===== Settings panel state ===== */
static const int PANEL_W = 360;
static const int PANEL_H = 420;

/* ===== Compact HUD window geometry =====
   The window is small and movable so it never matches the game's client rect (anti-cheat).
   Normal mode: HUD_W x HUD_AREA_H, shows only the sphere+ring (click-through).
   Settings mode: grows downward to WIN_H_SETTINGS to reveal the panel below the HUD.
   The HUD always lives in the top HUD_AREA_H region, so it looks identical in both modes. */
static const int HUD_W = 380;
static const int HUD_AREA_H = 380;
static const int PANEL_TOP = HUD_AREA_H + 8;                 // settings panel sits below the HUD area
static const int WIN_H_NORMAL = HUD_AREA_H;
static const int WIN_H_SETTINGS = PANEL_TOP + PANEL_H + 14;

static int  gWinX = 0, gWinY = 0;        // HUD window screen position (persisted as winX/winY)
static int  gPanelX = 10;                // panel is fixed within the compact window
static int  gPanelY = PANEL_TOP;

// Active slider index during drag
// 0 sphere, 1 ringY, 2 sens, 3..8 bands 0..5
static int gActiveSlider = -1;

// Sphere drag in settings mode
static bool gDraggingSphere = false;

/* ===== Info button support ===== */
struct InfoRegion { int x0, y0, x1, y1; const wchar_t* text; };
static InfoRegion gInfoRegions[48];
static int gInfoCount = 0;

/* ===== Preset checkbox hit regions ===== */
struct HitRegion { int x0, y0, x1, y1; int id; };
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

/* ===== Capture target (per-process loopback: capture only the game, not Discord/music/system) ===== */
static ma_uint32  gCapturePID = 0;             // 0 = entire system; else a target process tree
static wchar_t    gCaptureName[64] = { 0 };    // exe name of the captured process (for display)
static bool       gProcLoopbackUnsupported = false; // set if a per-process request fell back to system-wide

/* ===== Left/Right self-test (plays panned tones so a deaf user can confirm direction visually) ===== */
static ma_device         g_tone{};
static bool              g_toneOK = false;
static std::atomic<int>  gToneCh{ -1 };          // -1 silent, 0 left, 1 right
static bool              gSelfTestActive = false;
static bool              gSelfTestRequested = false;
static float             gSelfTestT0 = 0.f;

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
    SIZE  sz{ w,h };
    POINT srcpt{ 0,0 };
    BLENDFUNCTION bf{};
    bf.BlendOp = AC_SRC_OVER;
    bf.SourceConstantAlpha = 255;
    bf.AlphaFormat = AC_SRC_ALPHA;
    // pptDst = nullptr keeps the window's current screen position, so the window can grow/shrink
    // (HUD-only vs HUD+settings) from a fixed top-left as the DIB size changes.
    UpdateLayeredWindow(hWnd, nullptr, nullptr, &sz, src, &srcpt, 0, &bf, ULW_ALPHA);
}

/* ===== Settings File (Option A): %APPDATA%\DeafOverlay\settings.json ===== */
static std::filesystem::path get_settings_path() {
    PWSTR roaming = nullptr;
    std::filesystem::path p = L"settings.json";

    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_RoamingAppData, 0, nullptr, &roaming)) && roaming) {
        std::filesystem::path dir = std::filesystem::path(roaming) / L"DeafOverlay";
        CoTaskMemFree(roaming);

        std::error_code ec;
        std::filesystem::create_directories(dir, ec); // ok if already exists

        p = dir / L"settings.json";
    }
    return p;
}

static bool json_find_number(const std::string& s, const char* key, float& out) {
    size_t p = s.find(key);
    if (p == std::string::npos) return false;
    p = s.find(':', p);
    if (p == std::string::npos) return false;
    ++p;
    while (p < s.size() && (s[p] == ' ' || s[p] == '\t')) ++p;

    out = (float)std::atof(s.c_str() + p);
    return true;
}

static bool json_find_int(const std::string& s, const char* key, int& out) {
    float v = 0.f;
    if (!json_find_number(s, key, v)) return false;
    out = (int)v;
    return true;
}

static void save_settings_file() {
    std::filesystem::path path = get_settings_path();

    std::ofstream f(path, std::ios::binary);
    if (!f) return;

    f << "{\n";
    f << "  \"sphereScale\": " << gSphereScale << ",\n";
    f << "  \"ringOffsetY\": " << gRingOffsetY << ",\n";
    f << "  \"sensitivity\": " << gSensitivity << ",\n";
    f << "  \"swapLR\": " << (gSwapLR ? 1 : 0) << ",\n";
    f << "  \"spherePosX\": " << gSpherePosX << ",\n";
    f << "  \"spherePosY\": " << gSpherePosY << ",\n";
    f << "  \"winX\": " << gWinX << ",\n";
    f << "  \"winY\": " << gWinY << ",\n";
    f << "  \"preset\": " << (int)gPreset << ",\n";
    f << "  \"bandGain\": [";
    for (int i = 0; i < NB; ++i) {
        f << gBandGain[i];
        if (i + 1 < NB) f << ", ";
    }
    f << "]\n";
    f << "}\n";
}

static void load_settings_file() {
    std::filesystem::path path = get_settings_path();

    std::ifstream f(path, std::ios::binary);
    if (!f) return;

    std::string s((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());

    json_find_number(s, "\"sphereScale\"", gSphereScale);
    json_find_number(s, "\"ringOffsetY\"", gRingOffsetY);
    json_find_number(s, "\"sensitivity\"", gSensitivity);

    float swap = 0.f;
    if (json_find_number(s, "\"swapLR\"", swap)) gSwapLR = (swap >= 0.5f);

    json_find_number(s, "\"spherePosX\"", gSpherePosX);
    json_find_number(s, "\"spherePosY\"", gSpherePosY);

    int wx = 0, wy = 0;
    if (json_find_int(s, "\"winX\"", wx)) gWinX = wx;
    if (json_find_int(s, "\"winY\"", wy)) gWinY = wy;

    int preset = 0;
    if (json_find_int(s, "\"preset\"", preset)) gPreset = (GamePreset)preset;

    // bandGain array
    size_t p = s.find("\"bandGain\"");
    if (p != std::string::npos) {
        p = s.find('[', p);
        size_t q = s.find(']', p);
        if (p != std::string::npos && q != std::string::npos && q > p) {
            std::string arr = s.substr(p + 1, q - (p + 1));

            size_t start = 0;
            for (int i = 0; i < NB; ++i) {
                // Skip commas for i-th value
                if (i > 0) {
                    start = arr.find(',', start);
                    if (start == std::string::npos) break;
                    start++;
                }
                while (start < arr.size() && (arr[start] == ' ' || arr[start] == '\t')) start++;
                if (start >= arr.size()) break;

                gBandGain[i] = (float)std::atof(arr.c_str() + start);
            }
        }
    }

    // Clamp for safety
    gSphereScale = std::clamp(gSphereScale, 0.6f, 1.6f);
    gRingOffsetY = std::clamp(gRingOffsetY, 0.40f, 0.90f);
    gSensitivity = std::clamp(gSensitivity, 0.5f, 2.0f);

    for (int i = 0; i < NB; ++i) gBandGain[i] = std::clamp(gBandGain[i], 0.0f, 3.0f);

    gSpherePosX = std::clamp(gSpherePosX, 0.05f, 0.95f);
    gSpherePosY = std::clamp(gSpherePosY, 0.05f, 0.95f);
}


/* ===== First-run “accuracy setup” guidance (new) ===== */
static bool reg_read_dword(HKEY root, const wchar_t* path, const wchar_t* name, DWORD& outVal) {
    HKEY h{};
    if (RegOpenKeyExW(root, path, 0, KEY_READ, &h) != ERROR_SUCCESS) return false;
    DWORD type = 0, sz = sizeof(DWORD);
    DWORD v = 0;
    LONG rc = RegQueryValueExW(h, name, nullptr, &type, (LPBYTE)&v, &sz);
    RegCloseKey(h);
    if (rc == ERROR_SUCCESS && type == REG_DWORD) { outVal = v; return true; }
    return false;
}
static void reg_write_dword(HKEY root, const wchar_t* path, const wchar_t* name, DWORD val) {
    HKEY h{};
    DWORD disp = 0;
    if (RegCreateKeyExW(root, path, 0, nullptr, 0, KEY_WRITE, nullptr, &h, &disp) != ERROR_SUCCESS) return;
    RegSetValueExW(h, name, 0, REG_DWORD, (const BYTE*)&val, sizeof(val));
    RegCloseKey(h);
}

static void show_first_run_guidance(HWND) {
    // First run opens the in-app Setup view (not a wall-of-text MessageBox). We only set a flag
    // here; wWinMain opens the panel. Mark onboarding done so it doesn't auto-open every launch.
    const wchar_t* kKeyPath = L"Software\\DeafOverlay";
    DWORD done = 0;
    if (reg_read_dword(HKEY_CURRENT_USER, kKeyPath, L"OnboardingDone", done) && done == 1) return;
    gFirstRun = true;
    reg_write_dword(HKEY_CURRENT_USER, kKeyPath, L"OnboardingDone", 1);
}

/* ===== Info icon helpers ===== */
static void add_info_region(int cx, int cy, const wchar_t* text) {
    if (gInfoCount >= (int)(sizeof(gInfoRegions) / sizeof(gInfoRegions[0]))) return;
    int r = 7;
    gInfoRegions[gInfoCount++] = { cx - r, cy - r, cx + r, cy + r, text };
}
static void draw_info_icon(void* bits, int w, int h, int cx, int cy) {
    int r = 6;
    for (int dy = -r; dy <= r; ++dy)
        for (int dx = -r; dx <= r; ++dx)
            if (dx * dx + dy * dy <= r * r)
                putpx_PM(bits, w, h, cx + dx, cy + dy, 220, 40, 40, 60);
    putpx_PM(bits, w, h, cx, cy - 2, 255, 255, 255, 255);
    for (int yy = -1; yy <= 2; ++yy) putpx_PM(bits, w, h, cx, cy + yy, 255, 255, 255, 255);
}

/* ===== Channel-aware band energies (atomic from callback) ===== */
static constexpr int MAX_CH = 8;                       // up to 7.1 discrete channels
static std::atomic<float> gChan[MAX_CH][NB];           // per-channel, per-band smoothed energy
static std::atomic<int>   gNumCh{ 2 };                 // actual capture channel count (set at init)
static float gChanAzi[MAX_CH] = { 0 };                 // azimuth (radians, 0=front, +=right) per channel
static bool  gChanHasDir[MAX_CH] = { false };          // false for non-directional channels (LFE)
static std::atomic<float> gCoh{ 1.0f };                // L/R inter-channel coherence (0=diffuse, 1=coherent)
static inline float lp_step(float y, float x, float a) { return y + a * (x - y); }

/* ===== Peaks ===== */
constexpr float PI_F = 3.14159265358979323846f;
static inline float ema(float prev, float x, float a) { return prev + a * (x - prev); }

/* ===== Accuracy-first runtime state ===== */
static float gFloor[MAX_CH][NB] = { { 0 } };           // per-channel, per-band noise floor
static float gAutoGain = 1.0f;
static float gTransient = 0.0f;      // smoothed transient measure
static float gBandPrev[NB] = { 0 };  // previous per-band level (for spectral-flux onset detection)

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

// Colour that encodes front/back via hue: the front hemisphere is warm (white -> amber -> red) and
// behind is cool (pale -> strong blue), so direction reads at a glance. Loudness (s, 0..1) deepens
// the colour; callers also scale size/brightness with loudness so faint and loud look clearly different.
static inline void dir_color(float s, float ang, BYTE& r, BYTE& g, BYTE& b) {
    s = std::clamp(s, 0.0f, 1.0f);
    if (std::fabs(ang) <= PI_F * 0.5f) {
        // Front: warm (white -> amber -> red).
        r = 255;
        g = (BYTE)std::clamp(255.0f - s * 215.0f, 0.0f, 255.0f);
        b = (BYTE)std::clamp(245.0f - s * 245.0f, 0.0f, 255.0f);
    }
    else {
        // Behind: cool blue, tinted even when faint so 'behind' is distinguishable.
        r = (BYTE)std::clamp(150.0f - s * 130.0f, 0.0f, 255.0f);
        g = (BYTE)std::clamp(205.0f - s * 95.0f, 0.0f, 255.0f);
        b = 255;
    }
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

        // conservative amplitude merge (avoid sudden “lies”)
        p.amp = std::clamp(std::max(sA, 0.70f * sA + 0.55f * sB), 0.f, 1.f);

        p.rearGain = std::max(p.rearGain, q.rearGain);
        p.elevFactor = 0.5f * (p.elevFactor + q.elevFactor);
        if (sB > sA) p.band = q.band;
        p.life = std::max(p.life, q.life);
        return;
    }

    int slot = 0;
    for (int i = 1; i < (int)std::size(g_peaks); ++i)
        if (g_peaks[i].amp < g_peaks[slot].amp) slot = i;
    g_peaks[slot] = q;
}

/* ===== Audio callback ===== */
void MA_API audio_cb(ma_device* dev, void*, const void* pIn, ma_uint32 frames) {
    const float* s = (const float*)pIn;
    if (!s) return;

    const ma_uint32 chTotal = dev->capture.channels ? dev->capture.channels : 1; // device stride
    const ma_uint32 ch = (chTotal < (ma_uint32)MAX_CH) ? chTotal : (ma_uint32)MAX_CH;

    auto a_fc = [&](float fc) { return 1.0f - std::exp(-6.2831853f * fc / (float)dev->sampleRate); };
    const float a250 = a_fc(250.f),
        a2k = a_fc(2000.f),
        a5k = a_fc(5000.f),
        a8k = a_fc(8000.f),
        a12k = a_fc(12000.f);
    const float aTot = 0.07f;

    // Per-channel cascaded low-pass state; persists across calls for continuous IIR filtering.
    static float lp250[MAX_CH] = { 0 }, lp2k[MAX_CH] = { 0 }, lp5k[MAX_CH] = { 0 };
    static float lp8k[MAX_CH] = { 0 }, lp12k[MAX_CH] = { 0 }, tot[MAX_CH] = { 0 };

    // Running L/R correlation for inter-channel coherence (a confidence gate in the stereo path).
    static float cLR = 0, cLL = 0, cRR = 0;
    const float aCoh = a_fc(8.f);   // ~20 ms averaging window

    for (ma_uint32 i = 0; i < frames; ++i) {
        const float* frame = s + (size_t)i * chTotal;
        for (ma_uint32 c = 0; c < ch; ++c) {
            float e = frame[c] * frame[c];
            lp250[c] = lp_step(lp250[c], e, a250);
            lp2k[c]  = lp_step(lp2k[c],  e, a2k);
            lp5k[c]  = lp_step(lp5k[c],  e, a5k);
            lp8k[c]  = lp_step(lp8k[c],  e, a8k);
            lp12k[c] = lp_step(lp12k[c], e, a12k);
            tot[c]   = lp_step(tot[c],   e, aTot);
        }
        if (ch >= 2) {
            float L = frame[0], R = frame[1];
            cLR = lp_step(cLR, L * R, aCoh);
            cLL = lp_step(cLL, L * L, aCoh);
            cRR = lp_step(cRR, R * R, aCoh);
        }
    }

    // Coherence = |corr(L,R)|: ~1 for a single panned source (trust direction), ~0 for diffuse stereo.
    if (ch >= 2) {
        float coh = std::fabs(cLR) / std::sqrt(cLL * cRR + 1e-12f);
        gCoh.store(std::clamp(coh, 0.0f, 1.0f), std::memory_order_relaxed);
    }
    else {
        gCoh.store(1.0f, std::memory_order_relaxed);
    }

    // Global & per-band gain (user sensitivity only; auto gain is applied in main loop).
    const float g = 1.8f * BASE_SENS_GAIN * gSensitivity;

    for (ma_uint32 c = 0; c < ch; ++c) {
        float b[NB];
        b[0] = lp250[c];
        b[1] = std::max(0.f, lp2k[c]  - lp250[c]);
        b[2] = std::max(0.f, lp5k[c]  - lp2k[c]);
        b[3] = std::max(0.f, lp8k[c]  - lp5k[c]);
        b[4] = std::max(0.f, lp12k[c] - lp8k[c]);
        b[5] = std::max(0.f, tot[c]   - lp12k[c]);

        // Subtle built-in emphasis for lows/mids (footsteps/movement).
        b[0] *= 1.25f;
        b[1] *= 1.10f;

        for (int k = 0; k < NB; ++k) {
            float v = g * std::clamp(gBandGain[k], 0.0f, 3.0f) * b[k];
            float p = gChan[c][k].load(std::memory_order_relaxed);
            gChan[c][k].store(p + 0.22f * (v - p), std::memory_order_relaxed);
        }
    }
}

/* ===== Device ===== */
// Map a miniaudio channel to its physical speaker azimuth (radians, 0=front, +=right).
// hasDir=false for channels with no meaningful direction (LFE / unknown).
static float ma_channel_to_azimuth(ma_channel c, bool& hasDir) {
    hasDir = true;
    switch (c) {
    case MA_CHANNEL_FRONT_CENTER:       return    0.0f;
    case MA_CHANNEL_FRONT_LEFT:         return  -30.0f * (PI_F / 180.f);
    case MA_CHANNEL_FRONT_RIGHT:        return   30.0f * (PI_F / 180.f);
    case MA_CHANNEL_FRONT_LEFT_CENTER:  return  -15.0f * (PI_F / 180.f);
    case MA_CHANNEL_FRONT_RIGHT_CENTER: return   15.0f * (PI_F / 180.f);
    case MA_CHANNEL_SIDE_LEFT:          return  -90.0f * (PI_F / 180.f);
    case MA_CHANNEL_SIDE_RIGHT:         return   90.0f * (PI_F / 180.f);
    case MA_CHANNEL_BACK_LEFT:          return -135.0f * (PI_F / 180.f);
    case MA_CHANNEL_BACK_RIGHT:         return  135.0f * (PI_F / 180.f);
    case MA_CHANNEL_BACK_CENTER:        return  180.0f * (PI_F / 180.f);
    default:                            hasDir = false; return 0.0f;
    }
}
// Fallback azimuths by channel count when the device reports no usable channel map.
static float default_azimuth(int nch, int idx, bool& hasDir) {
    static const float a2[2] = { -30, 30 };
    static const float a6[6] = { -30, 30, 0, 0, -110, 110 };
    static const bool  d6[6] = { true, true, true, false, true, true };
    static const float a8[8] = { -30, 30, 0, 0, -135, 135, -90, 90 };
    static const bool  d8[8] = { true, true, true, false, true, true, true, true };
    hasDir = true;
    if (nch == 2 && idx < 2) return a2[idx] * (PI_F / 180.f);
    if (nch == 6 && idx < 6) { hasDir = d6[idx]; return a6[idx] * (PI_F / 180.f); }
    if (nch == 8 && idx < 8) { hasDir = d8[idx]; return a8[idx] * (PI_F / 180.f); }
    return 0.0f; // mono / unknown layouts collapse to front-center
}

// (Re)open the loopback capture device, optionally scoped to one process tree (pid != 0 needs
// Windows 10 build 20348+). Resolves the captured channel layout. Assumes g_ctx is initialized.
static bool audio_open_loopback(ma_uint32 pid) {
    ma_device_config cfg = ma_device_config_init(ma_device_type_loopback);
    cfg.capture.format = ma_format_f32;
    cfg.capture.channels = 0;       // 0 = inherit the device's native layout (stereo / 5.1 / 7.1)
    cfg.sampleRate = 48000;
    cfg.performanceProfile = ma_performance_profile_low_latency;
    cfg.dataCallback = audio_cb;
    cfg.wasapi.loopbackProcessID = pid;   // 0 = capture all processes (the entire system mix)

    if (ma_device_init(&g_ctx, &cfg, &g_dev) != MA_SUCCESS) return false;

    // Resolve the captured channel layout into per-channel azimuths used for direction.
    int nch = std::clamp((int)g_dev.capture.channels, 1, MAX_CH);
    gNumCh.store(nch, std::memory_order_relaxed);
    for (int c = 0; c < MAX_CH; ++c) {
        if (c < (int)g_dev.capture.channels) {
            ma_channel mc = g_dev.capture.channelMap[c];
            bool dir = false;
            float az = ma_channel_to_azimuth(mc, dir);
            // If the map is missing/unknown (not a real LFE), fall back to a standard layout.
            if (!dir && mc != MA_CHANNEL_LFE) az = default_azimuth(nch, c, dir);
            gChanAzi[c] = az;
            gChanHasDir[c] = dir;
        }
        else {
            gChanAzi[c] = 0.f;
            gChanHasDir[c] = false;
        }
    }

    if (ma_device_start(&g_dev) != MA_SUCCESS) { ma_device_uninit(&g_dev); return false; }
    gCapturePID = pid;
    return true;
}

static bool audio_init() {
    if (ma_context_init(nullptr, 0, nullptr, &g_ctx) != MA_SUCCESS) return false;
    if (!audio_open_loopback(0)) { ma_context_uninit(&g_ctx); return false; }
    g_audioOK = true;
    return true;
}

// Resolve gCapturePID -> gCaptureName (exe file name) for display.
static void update_capture_name() {
    gCaptureName[0] = 0;
    if (gCapturePID == 0) return;
    HANDLE hp = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, gCapturePID);
    if (!hp) { lstrcpynW(gCaptureName, L"(game)", 64); return; }
    wchar_t path[MAX_PATH]; DWORD sz = MAX_PATH;
    if (QueryFullProcessImageNameW(hp, 0, path, &sz)) {
        const wchar_t* base = path;
        for (const wchar_t* s = path; *s; ++s) if (*s == L'\\') base = s + 1;
        lstrcpynW(gCaptureName, base, 64);
    }
    else {
        lstrcpynW(gCaptureName, L"(game)", 64);
    }
    CloseHandle(hp);
}

// Switch the capture target at runtime. pid 0 = whole system. Falls back to system-wide if a
// per-process request fails (e.g. Windows older than 10 build 20348).
static void audio_set_process(ma_uint32 pid) {
    if (!g_audioOK) return;
    ma_device_uninit(&g_dev);
    g_audioOK = false;
    gProcLoopbackUnsupported = false;
    if (audio_open_loopback(pid)) {
        g_audioOK = true;
    }
    else if (audio_open_loopback(0)) {           // fall back to the whole-system mix
        g_audioOK = true;
        if (pid != 0) gProcLoopbackUnsupported = true;
    }
    update_capture_name();
}
static void audio_shutdown() {
    if (!g_audioOK) return;
    ma_device_uninit(&g_dev);
    ma_context_uninit(&g_ctx);
    g_audioOK = false;
}

/* ===== Self-test tone playback =====
   Plays a 600 Hz tone hard-left then hard-right through the default output. Because we capture
   system audio via loopback, the tone lights up the HUD on the matching side — letting a deaf
   user visually confirm capture works and that left/right isn't reversed. */
void MA_API tone_cb(ma_device* dev, void* pOut, const void*, ma_uint32 frames) {
    float* out = (float*)pOut;
    if (!out) return;
    const ma_uint32 ch = dev->playback.channels ? dev->playback.channels : 2;
    const float inc = 2.0f * PI_F * 600.0f / (float)dev->sampleRate;
    static float phase = 0.0f;
    const int side = gToneCh.load(std::memory_order_relaxed);
    for (ma_uint32 i = 0; i < frames; ++i) {
        float s = (side >= 0) ? 0.22f * std::sinf(phase) : 0.0f;
        phase += inc;
        if (phase > 2.0f * PI_F) phase -= 2.0f * PI_F;
        for (ma_uint32 c = 0; c < ch; ++c) out[i * ch + c] = 0.0f;
        if (side == 0)                  out[i * ch + 0] = s;     // hard left
        else if (side == 1 && ch > 1)   out[i * ch + 1] = s;     // hard right
    }
}
static bool tone_start() {
    if (g_toneOK) return true;
    if (!g_audioOK) return false;                 // reuse the context created in audio_init()
    ma_device_config cfg = ma_device_config_init(ma_device_type_playback);
    cfg.playback.format = ma_format_f32;
    cfg.playback.channels = 2;
    cfg.sampleRate = 48000;
    cfg.dataCallback = tone_cb;
    if (ma_device_init(&g_ctx, &cfg, &g_tone) != MA_SUCCESS) return false;
    if (ma_device_start(&g_tone) != MA_SUCCESS) { ma_device_uninit(&g_tone); return false; }
    g_toneOK = true;
    return true;
}
static void tone_stop() {
    gToneCh.store(-1, std::memory_order_relaxed);
    if (!g_toneOK) return;
    ma_device_uninit(&g_tone);
    g_toneOK = false;
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
            float bestGlow = 0.f, bestAmp = 0.f, bestAng = 0.f;

            for (const auto& p : g_peaks) if (p.life > 0.f) {
                float dist = std::fabs(std::remainder(ang - p.ang, 2 * PI_F));
                float spread = 0.30f + 0.55f * p.amp;            // louder = wider arc of glow
                float hot = std::max(0.f, 1.f - dist / spread);
                float win = (1.f - std::cosf(hot * PI_F)) * 0.5f;
                float gg = win * (0.30f + 0.70f * p.amp);        // louder = brighter
                if (gg > bestGlow) { bestGlow = gg; bestAmp = p.amp; bestAng = p.ang; }
            }

            if (bestGlow > 0.f) {
                BYTE rr, gg, bb; dir_color(bestAmp, bestAng, rr, gg, bb);
                BYTE aa = (BYTE)std::clamp(int(bestGlow * 235), 0, 235);
                putpx_PM(bits, w, h, x, y, aa, rr, gg, bb);
            }
        }
    }

    // Static "FRONT" label above the ring top so 'forward' is always clear (top = front, bottom = behind).
    {
        HDC dc = g_dib.dc;
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(150, 235, 170));
        HFONT oldF = (HFONT)SelectObject(dc, (HFONT)GetStockObject(DEFAULT_GUI_FONT));
        const wchar_t* lbl = L"FRONT";
        SIZE sz{}; GetTextExtentPoint32W(dc, lbl, 5, &sz);
        int tx = (int)std::round(cx) - (int)sz.cx / 2;
        int ty = (int)std::round(cy - b) - (int)sz.cy - 3;
        text_out_opaque(dc, bits, w, h, tx, ty, lbl);
        SelectObject(dc, oldF);
    }

    // Arrowheads for each peak (size + brightness scale with loudness; hue with front/back).
    for (const auto& p : g_peaks) if (p.life > 0.f) {
        BYTE rr, gg, bb; dir_color(p.amp, p.ang, rr, gg, bb);
        float th = p.ang;
        float ex = cx + a * std::sinf(th);
        float ey = cy - b * std::cosf(th);
        float nx = -b * std::cosf(th);
        float ny = -a * std::sinf(th);
        float nlen = std::max(1.f, std::sqrt(nx * nx + ny * ny));
        nx /= nlen; ny /= nlen;
        float tx = std::cosf(th), ty = std::sinf(th);

        int   half = (int)(3 + 5 * p.amp);                       // wider when louder
        int   reach = (int)(4 + 7 * p.amp);                      // longer when louder
        BYTE  aa = (BYTE)std::clamp(int(110 + 130 * p.amp), 0, 240);
        for (int i = -half; i <= half; ++i) {
            for (int j = 0; j < reach; ++j) {
                int xi = int(ex + nx * i + (a * 0.035f) * tx * j);
                int yi = int(ey + ny * i + (b * 0.035f) * ty * j);
                putpx_PM(bits, w, h, xi, yi, aa, rr, gg, bb);
            }
        }
    }
}

static void draw_sphere(void* bits, int w, int h, float cx, float cy, float radius, float tilt_deg) {
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

            // Directional intensity for THIS dot, plus the dominant peak's angle (for front/back hue).
            float dirInt = 0.f, domAng = 0.f;
            for (const auto& p : g_peaks) if (p.life > 0.f) {
                float dirx = std::sinf(p.ang);
                float diry = -std::cosf(p.ang);
                float align = (vx * dirx + vy * diry) / vlen;             // -1..1 (1 = dot faces this peak)
                float contrib = std::max(0.0f, align) * p.amp;            // only the facing side lights up
                if (contrib > dirInt) { dirInt = contrib; domAng = p.ang; }
            }

            float grow = 1.0f + 0.7f * dirInt;                            // louder/closer = bigger dots
            int   dotR = std::max(2, int(radius * 0.012f * grow));
            float depth = (z2 * 0.5f + 0.5f);

            // Dim base sphere (depth-shaded); the side facing the sound brightens and takes colour.
            float baseBright = 0.26f + 0.28f * depth;
            float bright = std::min(1.0f, baseBright + 1.2f * dirInt);

            BYTE rr, gg, bb; dir_color(dirInt, domAng, rr, gg, bb);       // hue = front/back, intensity = loudness
            BYTE a = (BYTE)std::clamp(int(20 + 215 * bright), 0, 238);

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
        BYTE rr, gg, bb; dir_color(p.amp, p.ang, rr, gg, bb);
        float fx = std::sinf(p.ang), fy = -std::cosf(p.ang);
        int len = int(radius * 0.9f);
        for (int i = 0; i < len; ++i)
            putpx_PM(bits, w, h, int(cx + fx * i), int(cy + fy * i), (BYTE)(40 + 150 * p.amp), rr, gg, bb);

        if (p.rearGain > 0.f) {
            float rx = std::sinf(p.ang + PI_F), ry = -std::cosf(p.ang + PI_F);
            int rlen = int(radius * 0.55f);
            BYTE a = (BYTE)std::clamp(int((20 + 160 * p.rearGain) * REAR_SCALE), 0, 255);
            for (int i = 0; i < rlen; ++i)
                putpx_PM(bits, w, h, int(cx + rx * i), int(cy + ry * i), a, 200, 220, 255);
        }
    }
}

/* ===== Slider helpers ===== */
static void slider_apply(int idx, float t) {
    t = std::clamp(t, 0.0f, 1.0f);
    if (idx == 0) gSphereScale = 0.6f + t * (1.6f - 0.6f);
    else if (idx == 1) gRingOffsetY = 0.40f + t * (0.90f - 0.40f);
    else if (idx == 2) gSensitivity = 0.5f + t * (2.0f - 0.5f);
    else if (idx >= 3 && idx < 3 + NB) {
        int b = idx - 3;
        if (t <= 0.5f) gBandGain[b] = (t / 0.5f) * 1.0f;
        else gBandGain[b] = 1.0f + ((t - 0.5f) / 0.5f) * 2.0f;
    }
}
static float slider_t_from_value(int idx) {
    if (idx == 0) return std::clamp((gSphereScale - 0.6f) / (1.6f - 0.6f), 0.0f, 1.0f);
    if (idx == 1) return std::clamp((gRingOffsetY - 0.40f) / (0.90f - 0.40f), 0.0f, 1.0f);
    if (idx == 2) return std::clamp((gSensitivity - 0.5f) / (2.0f - 0.5f), 0.0f, 1.0f);
    if (idx >= 3 && idx < 3 + NB) {
        int b = idx - 3;
        float gg = std::clamp(gBandGain[b], 0.0f, 3.0f);
        if (gg <= 1.0f) return 0.5f * (gg / 1.0f);
        return 0.5f + 0.5f * ((gg - 1.0f) / 2.0f);
    }
    return 0.0f;
}
static void draw_slider_bar(void* bits, int w, int h, int x0, int y, int x1, float t) {
    for (int x = x0; x <= x1; ++x) putpx_PM(bits, w, h, x, y, 90, 220, 220, 220);
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
    const int bw = 310;
    const int bh = 62;
    const int x1 = std::min(w - 14, x0 + bw);
    const int y1 = std::min(h - 14, y0 + bh);

    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 170, 12, 12, 14);

    for (int y = y0; y < std::min(y1, y0 + 18); ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 200, 28, 28, 34);

    HDC dc = g_dib.dc;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    HFONT hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    HFONT oldF = (HFONT)SelectObject(dc, hFont);

    text_out_opaque(dc, bits, w, h, x0 + 10, y0 + 2, L"DeafOverlay Hotkeys");
    text_out_opaque(dc, bits, w, h, x0 + 10, y0 + 22, L"Alt+H: Show/Hide   Alt+I: Settings   Alt+Q: Quit");
    text_out_opaque(dc, bits, w, h, x0 + 10, y0 + 40, L"Best accuracy: 7.1 or Stereo, Spatial OFF");

    SelectObject(dc, oldF);
}

/* ===== Button helper (filled rect + centered label + click region) ===== */
static void draw_button(void* bits, int w, int h, HDC dc, int x, int y, int bw, int bh,
    const wchar_t* label, int hitId, bool highlight = false) {
    BYTE r = highlight ? 70 : 38, g = highlight ? 110 : 62, b = highlight ? 170 : 120;
    for (int yy = 0; yy < bh; ++yy)
        for (int xx = 0; xx < bw; ++xx) {
            bool border = (yy == 0 || yy == bh - 1 || xx == 0 || xx == bw - 1);
            putpx_PM(bits, w, h, x + xx, y + yy, border ? 240 : 210, r, g, b);
        }
    SIZE sz{}; GetTextExtentPoint32W(dc, label, (int)wcslen(label), &sz);
    int tx = x + std::max(6, (bw - (int)sz.cx) / 2);
    int ty = y + std::max(2, (bh - (int)sz.cy) / 2);
    SetTextColor(dc, RGB(255, 255, 255));
    text_out_opaque(dc, bits, w, h, tx, ty, label);
    add_hit_region(x, y, x + bw, y + bh, hitId);
}

/* ===== Setup / Help view: auto-detected status, deep links, and the L/R self-test ===== */
static void draw_setup_body(void* bits, int w, int h, HDC dc, int y0, int innerX0, int innerX1) {
    int nCh = std::clamp(gNumCh.load(std::memory_order_relaxed), 1, MAX_CH);
    const wchar_t* layout =
        (nCh >= 8) ? L"7.1 surround" :
        (nCh >= 6) ? L"5.1 surround" :
        (nCh == 2) ? L"Stereo" : L"Mono";
    bool full360 = (nCh >= 6);

    int y = y0 + 30;
    auto line = [&](const wchar_t* s, COLORREF col) {
        SetTextColor(dc, col);
        text_out_opaque(dc, bits, w, h, innerX0, y, s);
        y += 18;
        };

    SetTextColor(dc, RGB(255, 255, 225));
    text_out_opaque(dc, bits, w, h, innerX0, y, L"Audio setup status"); y += 22;

    line(g_audioOK ? L"Capture: OK (system audio)" : L"Capture: FAILED",
        g_audioOK ? RGB(120, 230, 120) : RGB(240, 120, 120));

    {
        wchar_t buf[64];
        wsprintfW(buf, L"Output: %s (%d ch)", layout, nCh);
        line(buf, RGB(225, 225, 225));
    }

    line(full360 ? L"Direction: Full 360 (front + back)" : L"Direction: Left / Right only",
        full360 ? RGB(120, 230, 120) : RGB(240, 220, 90));

    y += 6;
    if (!full360) {
        line(L"Stereo can't show front/back.", RGB(240, 220, 90));
        line(L"For 360: set Windows output to 7.1,", RGB(225, 225, 225));
        line(L"then turn Spatial sound OFF.", RGB(225, 225, 225));
    }
    else {
        line(L"Keep Spatial sound OFF for the", RGB(120, 230, 120));
        line(L"most accurate direction.", RGB(225, 225, 225));
        y += 18;
    }

    y += 4;
    if (gCapturePID == 0) {
        line(L"Capturing: all system audio", RGB(220, 220, 220));
    }
    else {
        wchar_t cbuf[96];
        wsprintfW(cbuf, L"Capturing: %s", gCaptureName[0] ? gCaptureName : L"(game)");
        line(cbuf, RGB(120, 230, 120));
    }
    if (gProcLoopbackUnsupported)
        line(L"Game-only needs Win10 build 20348+", RGB(240, 180, 90));
    line(L"Alt+G: capture only the focused game", RGB(180, 180, 200));

    y += 8;
    int bw = innerX1 - innerX0;
    int bh = 22;
    draw_button(bits, w, h, dc, innerX0, y, bw, bh, L"Open Windows Sound settings", 300); y += bh + 6;
    draw_button(bits, w, h, dc, innerX0, y, bw, bh, L"Open Spatial sound settings", 301); y += bh + 6;

    int tch = gToneCh.load(std::memory_order_relaxed);
    const wchar_t* testLabel =
        gSelfTestActive ? (tch == 0 ? L"Testing... LEFT  <" : tch == 1 ? L"Testing... RIGHT  >" : L"Testing...")
        : L"Test Left / Right";
    draw_button(bits, w, h, dc, innerX0, y, bw, bh, testLabel, 302, gSelfTestActive); y += bh + 4;

    SetTextColor(dc, RGB(200, 200, 200));
    text_out_opaque(dc, bits, w, h, innerX0, y, L"If sides are reversed, use Swap L/R."); y += 22;

    draw_button(bits, w, h, dc, innerX0, y, bw, bh, L"Back to Controls", 306);
}

/* ===== Settings panel drawing ===== */
static void draw_settings_panel() {
    if (!gSettingsOpen || !g_dib.bits) return;

    int w = g_dib.w, h = g_dib.h;
    void* bits = g_dib.bits;

    gInfoCount = 0;
    gHitCount = 0;

    if (gPanelX < 10) gPanelX = 10;
    if (gPanelY < 10) gPanelY = 10;
    if (gPanelX + PANEL_W > w - 10) gPanelX = w - 10 - PANEL_W;
    if (gPanelY + PANEL_H > h - 10) gPanelY = h - 10 - PANEL_H;

    int x0 = gPanelX;
    int y0 = gPanelY;
    int x1 = x0 + PANEL_W;
    int y1 = y0 + PANEL_H;

    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 205, 16, 16, 20);

    for (int y = y0; y < y0 + 24; ++y)
        for (int x = x0; x < x1; ++x)
            putpx_PM(bits, w, h, x, y, 235, 32, 32, 40);

    int innerX0 = x0 + 12;
    int innerX1 = x1 - 32;

    HDC dc = g_dib.dc;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 225));
    HFONT hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    HFONT oldF = (HFONT)SelectObject(dc, hFont);

    text_out_opaque(dc, bits, w, h, x0 + 10, y0 + 5,
        gShowSetup ? L"DeafOverlay - Setup" : L"DeafOverlay Settings");

    if (gShowSetup) {
        draw_setup_body(bits, w, h, dc, y0, innerX0, innerX1);
        SelectObject(dc, oldF);
        return;
    }

    int row = y0 + 40;
    int dy = 26;

    int ySphere = row; row += dy;
    int yRing = row; row += dy;
    int ySens = row; row += dy;
    int yBass = row; row += dy;
    int yLowMid = row; row += dy;
    int yMid = row; row += dy;
    int yHigh = row; row += dy;
    int yAir = row; row += dy;

    draw_slider_bar(bits, w, h, innerX0, ySphere, innerX1, slider_t_from_value(0));
    draw_slider_bar(bits, w, h, innerX0, yRing, innerX1, slider_t_from_value(1));
    draw_slider_bar(bits, w, h, innerX0, ySens, innerX1, slider_t_from_value(2));
    draw_slider_bar(bits, w, h, innerX0, yBass, innerX1, slider_t_from_value(3));
    draw_slider_bar(bits, w, h, innerX0, yLowMid, innerX1, slider_t_from_value(4));
    draw_slider_bar(bits, w, h, innerX0, yMid, innerX1, slider_t_from_value(5));
    draw_slider_bar(bits, w, h, innerX0, yHigh, innerX1, slider_t_from_value(6));
    draw_slider_bar(bits, w, h, innerX0, yAir, innerX1, slider_t_from_value(7));

    auto label_with_info = [&](int y, const wchar_t* txt, const wchar_t* info) {
        text_out_opaque(dc, bits, w, h, innerX0, y - 14, txt);
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

    text_out_opaque(dc, bits, w, h, cbX + cbS + 6, cbY - 1, L"Swap L/R");

    int swapInfoX = x1 - 14;
    int swapInfoY = cbY + 2;
    draw_info_icon(bits, w, h, swapInfoX, swapInfoY);
    add_info_region(swapInfoX, swapInfoY, kInfoSwapLR);

    add_hit_region(cbX, cbY - 4, cbX + cbS + 120, cbY + cbS + 4, 100);

    // Game presets
    int presY = cbY + 28;
    text_out_opaque(dc, bits, w, h, innerX0, presY - 2, L"Game presets");

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
        text_out_opaque(dc, bits, w, h, bx + pBoxS + 6, by - 1, name);
        add_hit_region(bx, by - 4, bx + 240, by + pBoxS + 4, hitId);
        };

    draw_preset_row(pRowY + 0, GamePreset::CS2, L"CS2", 200);
    draw_preset_row(pRowY + 18, GamePreset::R6S, L"Rainbow Six Siege", 201);
    draw_preset_row(pRowY + 36, GamePreset::COD, L"Call of Duty", 202);

    SetTextColor(dc, RGB(220, 220, 220));
    text_out_opaque(dc, bits, w, h, innerX0, pRowY + 58, L"(Select one, then fine-tune sliders)");
    SetTextColor(dc, RGB(255, 255, 255));

    draw_button(bits, w, h, dc, innerX0, pRowY + 76, innerX1 - innerX0, 22, L"Setup / Help", 305);

    SelectObject(dc, oldF);
}

/* ===== Settings mode toggle ===== */
static const wchar_t* kClassName = L"DeafOverlayWnd";
enum : int { HK_TOGGLE_VIS = 1, HK_TOGGLE_CFG = 2, HK_QUIT = 3, HK_GAME = 4 };

static void set_settings_mode(HWND h, bool on) {
    gSettingsOpen = on;
    LONG ex = GetWindowLongW(h, GWL_EXSTYLE);
    if (on) {
        ex &= ~WS_EX_TRANSPARENT;
    }
    else {
        ex |= WS_EX_TRANSPARENT;
        gActiveSlider = -1;
        gDraggingSphere = false;
        gShowSetup = false;
        gSelfTestActive = false;
        gSelfTestRequested = false;
        tone_stop();
        ReleaseCapture();
    }
    SetWindowLongW(h, GWL_EXSTYLE, ex);

    // Grow the window downward to reveal the panel (or shrink back to HUD-only). The actual
    // window resize happens on the next blit via UpdateLayeredWindow, sized from this DIB.
    int newH = on ? WIN_H_SETTINGS : WIN_H_NORMAL;
    if (g_dib.w != HUD_W || g_dib.h != newH) {
        free_dib(g_dib);
        g_dib = make_dib(HUD_W, newH);
    }

    // Keep the (taller) settings window on-screen; the saved HUD position (gWinX/gWinY) is preserved.
    int screenH = GetSystemMetrics(SM_CYSCREEN);
    int top = gWinY;
    if (on && top + WIN_H_SETTINGS > screenH) top = std::max(0, screenH - WIN_H_SETTINGS);
    SetWindowPos(h, HWND_TOPMOST, gWinX, top, 0, 0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
}

/* ===== Render frame ===== */
static void render_frame() {
    if (!g_dib.bits) return;

    int w = g_dib.w;
    int h = g_dib.h;
    void* bits = g_dib.bits;

    if (gSettingsOpen) {
        // Light dim over the (small) window so the whole settings surface captures clicks.
        for (int y = 0; y < h; ++y)
            for (int x = 0; x < w; ++x)
                putpx_PM(bits, w, h, x, y, 150, 0, 0, 0);
    }
    else {
        std::memset(bits, 0, (size_t)w * (size_t)h * 4);   // transparent + click-through in normal mode
    }

    // HUD (sphere + ring) drawn in the fixed top region, so it looks identical in both modes.
    float cx = gSpherePosX * HUD_W;
    float cy = gSpherePosY * HUD_AREA_H;

    float baseScale = 0.13f;
    float sphereR = std::min(HUD_W, HUD_AREA_H) * baseScale * gSphereScale;

    draw_sphere(bits, w, h, cx, cy, sphereR, 22.f);

    float rx = std::min(HUD_W, HUD_AREA_H) * (baseScale * 1.1f) * gSphereScale;
    float ry = rx * 0.45f;
    float ringY = gRingOffsetY * HUD_AREA_H;

    draw_ring(bits, w, h, cx, ringY, rx, ry);

    if (gSettingsOpen) {
        draw_hint_box();
        draw_settings_panel();
    }
}

/* ===== Window proc ===== */
LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM wParam, LPARAM lParam) {
    switch (m) {
    case WM_NCHITTEST:
        if (!gSettingsOpen) return HTTRANSPARENT;
        return HTCLIENT;

    case WM_LBUTTONDOWN:
        if (gSettingsOpen) {
            SetCapture(h);
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);

            for (int i = 0; i < gInfoCount; ++i) {
                const auto& inf = gInfoRegions[i];
                if (mx >= inf.x0 && mx <= inf.x1 && my >= inf.y0 && my <= inf.y1) {
                    MessageBoxW(h, inf.text, L"Overlay Setting Info", MB_OK | MB_ICONINFORMATION);
                    ReleaseCapture();
                    return 0;
                }
            }

            for (int i = 0; i < gHitCount; ++i) {
                const auto& hr = gHitRegions[i];
                if (mx >= hr.x0 && mx <= hr.x1 && my >= hr.y0 && my <= hr.y1) {
                    if (hr.id == 100) {
                        gSwapLR = !gSwapLR;
                        save_settings_file();
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 200) {
                        set_preset_exclusive(GamePreset::CS2);
                        save_settings_file();
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 201) {
                        set_preset_exclusive(GamePreset::R6S);
                        save_settings_file();
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 202) {
                        set_preset_exclusive(GamePreset::COD);
                        save_settings_file();
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 300) {   // Open Windows Sound settings
                        ShellExecuteW(nullptr, L"open", L"ms-settings:sound", nullptr, nullptr, SW_SHOWNORMAL);
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 301) {   // Open the page with the Spatial sound dropdown
                        ShellExecuteW(nullptr, L"open", L"ms-settings:sound-defaultoutputproperties", nullptr, nullptr, SW_SHOWNORMAL);
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 302) {   // Start the L/R self-test
                        gSelfTestRequested = true;
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 305) {   // Controls -> Setup view
                        gShowSetup = true;
                        ReleaseCapture();
                        return 0;
                    }
                    if (hr.id == 306) {   // Setup -> Controls view
                        gShowSetup = false;
                        ReleaseCapture();
                        return 0;
                    }
                }
            }

            // Controls view: sliders (skipped in the Setup view, where these aren't drawn).
            if (!gShowSetup) {
                int innerX0 = gPanelX + 12;
                int innerX1 = gPanelX + PANEL_W - 32;
                int row = gPanelY + 40;
                int dy = 26;

                int ySphere = row; row += dy;
                int yRing = row; row += dy;
                int ySens = row; row += dy;
                int yBass = row; row += dy;
                int yLowMid = row; row += dy;
                int yMid = row; row += dy;
                int yHigh = row; row += dy;
                int yAir = row; row += dy;

                auto inSlider = [&](int sy) {
                    const int halfH = 10;
                    return (mx >= innerX0 && mx <= innerX1 && my >= sy - halfH && my <= sy + halfH);
                    };

                if (inSlider(ySphere)) { gActiveSlider = 0; slider_apply(0, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yRing)) { gActiveSlider = 1; slider_apply(1, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(ySens)) { gActiveSlider = 2; slider_apply(2, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yBass)) { gActiveSlider = 3; slider_apply(3, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yLowMid)) { gActiveSlider = 4; slider_apply(4, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yMid)) { gActiveSlider = 5; slider_apply(5, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yHigh)) { gActiveSlider = 6; slider_apply(6, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
                if (inSlider(yAir)) { gActiveSlider = 7; slider_apply(7, (float)(mx - innerX0) / (float)(innerX1 - innerX0)); return 0; }
            }

            // Sphere: drag to fine-position it within the HUD area.
            {
                float cx = gSpherePosX * HUD_W;
                float cy = gSpherePosY * HUD_AREA_H;
                float baseScale = 0.13f;
                float sphereR = std::min(HUD_W, HUD_AREA_H) * baseScale * gSphereScale;
                float dx = (float)mx - cx;
                float dy2 = (float)my - cy;
                if (my < HUD_AREA_H && dx * dx + dy2 * dy2 <= sphereR * sphereR * 1.3f) {
                    gDraggingSphere = true;
                    gActiveSlider = -1;
                    return 0;
                }
            }

            // Anywhere else (panel body, dimmed HUD background): drag the whole HUD to move it.
            // Use the system move loop so the layered window follows the cursor cleanly.
            gActiveSlider = -1;
            gDraggingSphere = false;
            ReleaseCapture();
            SendMessageW(h, WM_NCLBUTTONDOWN, HTCAPTION, 0);
            { RECT rc{}; GetWindowRect(h, &rc); gWinX = rc.left; gWinY = rc.top; }
            save_settings_file();
            return 0;
        }
        break;

    case WM_MOUSEMOVE:
        if (gSettingsOpen && (wParam & MK_LBUTTON)) {
            int mx = GET_X_LPARAM(lParam);
            int my = GET_Y_LPARAM(lParam);
            int innerX0 = gPanelX + 12;
            int innerX1 = gPanelX + PANEL_W - 32;

            if (gActiveSlider != -1) {
                float t = (float)(mx - innerX0) / (float)(innerX1 - innerX0);
                slider_apply(gActiveSlider, t);
                return 0;
            }
            if (gDraggingSphere) {
                gSpherePosX = std::clamp(mx / (float)HUD_W, 0.05f, 0.95f);
                gSpherePosY = std::clamp(my / (float)HUD_AREA_H, 0.05f, 0.95f);
                return 0;
            }
        }
        break;

    case WM_LBUTTONUP:
        if (gSettingsOpen) {
            gActiveSlider = -1;
            gDraggingSphere = false;
            ReleaseCapture();

            // Persist changes after any drag
            save_settings_file();

            return 0;
        }
        break;


    case WM_SIZE:
        // Window size is driven explicitly by the DIB (see set_settings_mode / blit_layered).
        return 0;

    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;

    default:
        return DefWindowProcW(h, m, wParam, lParam);
    }
    return DefWindowProcW(h, m, wParam, lParam);
}

/* ===== Entry / main loop ===== */
int APIENTRY wWinMain(HINSTANCE inst, HINSTANCE, LPWSTR, int) {
    using SetProcessDpiAwarenessContext_t = BOOL(WINAPI*)(HANDLE);
    if (auto setCtx = (SetProcessDpiAwarenessContext_t)
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetProcessDpiAwarenessContext"))
    {
        setCtx((HANDLE)-4); // PER_MONITOR_AWARE_V2
    }

    WNDCLASSW wc{};
    wc.hInstance = inst;
    wc.lpfnWndProc = WndProc;
    wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW);
    wc.hbrBackground = nullptr;
    RegisterClassW(&wc);

    int screenW = GetSystemMetrics(SM_CXSCREEN);
    int screenH = GetSystemMetrics(SM_CYSCREEN);

    // Default the compact HUD to the upper-right area; a saved position (below) may override it.
    gWinX = screenW - HUD_W - 40;
    gWinY = 80;

    // Load previous user settings (including window position) before creating the window.
    load_settings_file();

    // Keep the window on-screen even if the saved position is stale (resolution change, etc.).
    gWinX = std::clamp(gWinX, 0, std::max(0, screenW - HUD_W));
    gWinY = std::clamp(gWinY, 0, std::max(0, screenH - WIN_H_NORMAL));

    HWND h = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
        kClassName, L"DeafOverlay", WS_POPUP,
        gWinX, gWinY, HUD_W, WIN_H_NORMAL,
        nullptr, nullptr, inst, nullptr);

    BOOL noShadow = FALSE;
    DwmSetWindowAttribute(h, 2, &noShadow, sizeof(noShadow));

    ShowWindow(h, SW_SHOWNA);
    UpdateWindow(h);

    RegisterHotKey(h, HK_TOGGLE_VIS, MOD_ALT | MOD_NOREPEAT, 'H');
    RegisterHotKey(h, HK_TOGGLE_CFG, MOD_ALT | MOD_NOREPEAT, 'I');
    RegisterHotKey(h, HK_QUIT, MOD_ALT | MOD_NOREPEAT, 'Q'); // Alt+Q = Quit
    RegisterHotKey(h, HK_GAME, MOD_ALT | MOD_NOREPEAT, 'G');  // Alt+G = capture only the focused game

    g_dib = make_dib(HUD_W, WIN_H_NORMAL);

    // First launch: flag onboarding so we can auto-open the Setup view (after audio_init, so the
    // status panel reports the real captured channel layout).
    show_first_run_guidance(h);

    audio_init();

    if (gFirstRun) {
        gShowSetup = true;
        set_settings_mode(h, true);
    }

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
                UnregisterHotKey(h, HK_QUIT);
                UnregisterHotKey(h, HK_GAME);
                tone_stop();
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
                else if (msg.wParam == HK_GAME) {
                    if (gCapturePID != 0) {
                        audio_set_process(0);                  // back to all system audio
                    }
                    else {
                        HWND fg = GetForegroundWindow();
                        DWORD pid = 0;
                        if (fg) GetWindowThreadProcessId(fg, &pid);
                        if (pid && pid != GetCurrentProcessId()) audio_set_process(pid);
                    }
                }
                else if (msg.wParam == HK_QUIT) {
                    save_settings_file();
                    PostQuitMessage(0);
                }
                continue;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        float t = std::chrono::duration<float>(std::chrono::steady_clock::now() - t0).count();
        float dt = t - tPrev;
        tPrev = t;
        dt = std::clamp(dt, 0.001f, 0.050f);

        // Self-test: play a tone hard-left (~1.5s) then hard-right (~1.5s). Loopback captures it, so
        // the HUD lights the matching side and the user can confirm left/right visually.
        if (gSelfTestRequested && !gSelfTestActive) {
            gSelfTestRequested = false;
            if (tone_start()) { gSelfTestActive = true; gSelfTestT0 = t; }
        }
        if (gSelfTestActive) {
            float e = t - gSelfTestT0;
            int side = -1;
            if (e < 0.3f)      side = -1;   // brief lead-in
            else if (e < 1.8f) side = 0;    // left
            else if (e < 2.1f) side = -1;   // gap
            else if (e < 3.6f) side = 1;    // right
            else { gSelfTestActive = false; tone_stop(); }
            gToneCh.store(side, std::memory_order_relaxed);
        }

        // Read per-channel band energies; subtract a per-channel noise floor (asymmetric).
        int nCh = std::clamp(gNumCh.load(std::memory_order_relaxed), 1, MAX_CH);
        float E[MAX_CH][NB];
        float tot = 0.0f;
        for (int c = 0; c < nCh; ++c) {
            for (int b = 0; b < NB; ++b) {
                float v = gChan[c][b].load(std::memory_order_relaxed);
                float d = v - gFloor[c][b];
                gFloor[c][b] += (d > 0 ? FLOOR_ATTACK : FLOOR_RELEASE) * d;
                float v2 = std::max(0.0f, v - FLOOR_MULT * gFloor[c][b]);
                E[c][b] = v2;
                tot += v2;
            }
        }

        // Auto gain toward a comfortable visibility target (log keeps it stable across devices).
        float totN = std::log1p(std::max(0.0f, tot));
        float err = (TARGET_LEVEL - totN);
        gAutoGain = std::clamp(gAutoGain + AUTO_GAIN_RATE * err, 0.40f, 4.50f);
        for (int c = 0; c < nCh; ++c)
            for (int b = 0; b < NB; ++b) E[c][b] *= gAutoGain;

        // Transient detector via spectral (band) flux: sum of positive per-band level increases.
        // Onsets (footsteps/shots) redistribute energy across bands, so this is more selective than
        // tracking total level alone.
        float flux = 0.0f;
        for (int b = 0; b < NB; ++b) {
            float bt = 0.0f;
            for (int c = 0; c < nCh; ++c) bt += E[c][b];
            float btN = std::log1p(std::max(0.0f, bt));
            flux += std::max(0.0f, btN - gBandPrev[b]);
            gBandPrev[b] = btN;
        }
        gTransient = ema(gTransient, flux, TRANSIENT_ALPHA);
        float transientBoost = std::clamp(gTransient / TRANSIENT_HI, 0.0f, 1.0f);

        // Decay existing peaks.
        for (auto& p : g_peaks) if (p.life > 0.f) {
            p.life -= dt;
            p.amp *= std::exp(-dt * PEAK_DECAY_HZ);
            if (p.life <= 0.f || p.amp < 0.02f) p = Peak{};
        }

        auto norm = [](float v) { return std::tanh(1.2f * v); };
        const float maxAng = MAX_AZIMUTH_DEG * PI_F / 180.f;

        if (nCh >= 6) {
            // ===== Multichannel (5.1/7.1): true 360 deg from discrete channel energies =====
            // Each band's direction = energy-weighted vector over the physical channel azimuths.
            for (int b = 0; b < NB; ++b) {
                float vx = 0.f, vy = 0.f, sumE = 0.f;
                for (int c = 0; c < nCh; ++c) {
                    if (!gChanHasDir[c]) continue;          // skip LFE / non-directional
                    float e = E[c][b];
                    if (e <= 0.f) continue;
                    float az = gSwapLR ? -gChanAzi[c] : gChanAzi[c];
                    vx += e * std::sinf(az);
                    vy += e * std::cosf(az);
                    sumE += e;
                }
                if (sumE <= 1e-6f) continue;
                float a = norm(sumE);
                if (a <= 0.01f) continue;

                float ang = std::atan2f(vx, vy);            // 0=front, +=right, +/-pi=rear
                float focus = std::clamp(std::sqrt(vx * vx + vy * vy) / sumE, 0.f, 1.f); // 1=point,0=diffuse
                float rearness = std::clamp((std::fabs(ang) - 0.5f * PI_F) / (0.5f * PI_F), 0.f, 1.f);

                Peak q;
                q.ang = ang;
                q.amp = a * (0.55f + 0.45f * focus);
                q.life = 0.55f + 0.22f * transientBoost;
                q.band = b;
                q.rearGain = rearness * a;                  // visual rear accent when behind
                q.elevFactor = 1.0f;
                add_peak_merge(q);
            }
        }
        else {
            // ===== Stereo: honest left/right front-arc only (no fabricated rear/elevation) =====
            int li = 0, ri = (nCh > 1 ? 1 : 0);
            for (int c = 0; c < nCh; ++c) {
                if (gChanAzi[c] < 0.f) li = c;
                else if (gChanAzi[c] > 0.f) ri = c;
            }
            // Coherence gate: a single panned source is L/R-correlated (trust its direction); diffuse
            // ambience/reverb is uncorrelated (suppress it so the HUD doesn't jitter).
            float coh = gCoh.load(std::memory_order_relaxed);
            float cohGate = std::clamp((coh - 0.30f) / 0.50f, 0.0f, 1.0f);
            for (int b = 0; b < NB; ++b) {
                float L = E[li][b];
                float R = E[ri][b];
                if (gSwapLR) { float tmp = L; L = R; R = tmp; }
                float S = L + R;
                if (S <= 1e-6f) continue;

                // Confidence from level imbalance + transient authority, gated by L/R coherence.
                float ild = std::fabs((R - L) / (S + 1e-6f));
                float cDir = std::clamp((ild - 0.02f) / 0.28f, 0.0f, 1.0f);
                float cEvent = std::clamp(0.25f + 0.75f * transientBoost, 0.25f, 1.0f);
                float conf = std::clamp((0.55f * cDir + 0.45f * cEvent) * (0.45f + 0.55f * cohGate), 0.0f, 1.0f);

                float ild_signed = (R - L) / (S + 1e-6f);
                float ang = maxAng * std::tanh(ILD_K * ild_signed); // front arc only

                float a = norm(std::max(L, R));
                if (a <= 0.005f) continue;

                Peak q;
                q.ang = ang;
                q.amp = a * (0.62f + 0.38f * conf);
                q.life = 0.55f + 0.22f * transientBoost;
                q.band = b;
                q.rearGain = 0.f;       // stereo cannot resolve rear — do not fake it
                q.elevFactor = 1.0f;
                add_peak_merge(q);
            }
        }

        render_frame();
        blit_layered(h, g_dib.dc, g_dib.w, g_dib.h);

        Sleep(16);
    }
}
