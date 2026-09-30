# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

DeafOverlay is a Windows-only accessibility tool: an always-on-top, click-through overlay that turns the PC's final system audio into a directional visual HUD (a dotted 3D sphere + a "Saturn" compass ring) so deaf and hard-of-hearing players can *see* where in-game sounds come from. See `README.md` for the full product vision.

It reads audio via **WASAPI loopback** (the mixed system output), so there is no DLL injection or game memory access — anti-cheat safe, but it means direction is inferred only from the final stereo mix. For best accuracy the user must set Windows output to stereo and disable spatializers (Windows Sonic / Dolby Atmos / DTS); the app shows this guidance once on first run.

## Build

Windows + MSVC (`cl.exe`) + Ninja, driven by CMake presets. C++20. Builds a GUI exe (no console window).

```sh
cmake --preset x64-debug          # configure (presets: x64-debug, x64-release, x86-*)
cmake --build out/build/x64-debug # build
# -> out/build/x64-debug/DeafOverlay/DeafOverlay.exe
```

**The build must run from a Visual Studio Developer environment.** A plain shell has neither `cl.exe` nor `ninja` on PATH, so `cmake --preset` fails with `unable to find a build program corresponding to "Ninja"`. Either build from a Developer PowerShell / VS itself, or import the toolchain first by sourcing `vcvars64.bat`, e.g.:

```powershell
$vcvars = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] }
}
# now: cmake --preset x64-debug ; cmake --build out/build/x64-debug
```

Both `cl.exe` and `ninja.exe` ship with Visual Studio (ninja under `Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\`); `vcvars64.bat` puts them on PATH. Adjust the VS edition/path (`Community` / `18`) to match the local install. PowerShell env state does not persist between separate shell invocations, so run the import and the `cmake` commands in the same call.

The repo is also a Visual Studio "Open Folder" CMake project — VS picks up `CMakePresets.json` directly (and provides the developer environment automatically).

The C++ app has **no tests and no linter**. The only quality gate is the compiler: MSVC is configured with `/W4 /permissive- /Zc:preprocessor`, so keep the build warning-clean.

## Rust rewrite (in progress, `rust/`)

A Cargo workspace that replaces the C++ app, with feature parity plus the new direction DSP.
- `crates/dsp`: the platform-free direction pipeline (STFT → per-bin votes → azimuth histogram → tracker). It also holds `baseline.rs`, a port of the v1 C++ direction maths that the new pipeline is scored against.
- `crates/capture`: WASAPI loopback (the `wasapi` crate) feeding `dsp::Analyzer`.
  - One background thread does both capture and analysis. The UI polls `Capture::sources()` / `status()` through a mutex; commands arrive over a channel.
  - The stream opens in the default device's native layout. `layout.rs` maps the channel mask to azimuths, and 5.1 surround pairs map to ±110°.
  - The stream reopens itself when the default device changes or is invalidated.
  - `set_process(pid)` is the Alt+G path. It falls back to the system mix and sets `proc_unsupported`, like v1.
  - While nothing plays, loopback delivers no packets, so the thread feeds silence on each 100 ms wait timeout to keep sources fading.
  - `tone.rs` is the L/R self-test: a 600 Hz tone on a second (render) WASAPI stream. It beeps rather than holding a steady tone, because the DSP votes on onsets and tunes out steady sounds within about 0.5 s.
  - `set_band_gains` carries v1's six band sliders and presets into the DSP as per-bin vote weights, using the same band edges.
- `crates/app` (binary `deafoverlay`) is the overlay:
  - `hud.rs` is platform-free. It ports `draw_sphere` / `draw_ring` / `dir_color` into premultiplied `0xAARRGGBB` pixels, so it's unit-tested without a window.
  - `Normalizer` maps the tracker's unbounded `strength` to v1's 0–1 `amp` with a slow auto-gain. `REF_MIN` is its calibration knob.
  - `ui.rs` is the settings panel (Controls and Setup views). A single `panel()` builder does both the drawing (given a canvas) and the list of clickable widgets, so drawing and hit-testing can't drift apart like v1's did. Add controls there only.
  - `main.rs` is the Win32 shell. It follows the same rules as v1: the window size comes from the DIB (settings mode grows it to `WIN_H_SETTINGS`), GDI text is forced opaque, `HTTRANSPARENT` makes it click-through, and Alt+H/I/G/Q are the hotkeys.
  - `persist.rs` handles settings persistence. It uses v1's file, keys and layout (`%APPDATA%\DeafOverlay\settings.json`), so both builds share one settings file. It reads with `serde_json` (clamped, and missing keys keep their defaults) and writes via a temp file plus rename. Saves happen after each change, window move, settings close and quit. The first-run flag is v1's `HKCU\Software\DeafOverlay\OnboardingDone`, and on first run the app opens the Setup view.
  - `rust/.cargo/config.toml` links the C runtime statically, so `target/release/deafoverlay.exe` is one standalone file with no VC++ Redistributable needed.
  - `wnd_proc` only queues mouse events; the main loop handles them. Info popups and the window-move loop are modal and re-enter `wnd_proc`, so handling input there would mean re-entrant access to the app state.

```sh
cd rust
cargo test                                           # accuracy thresholds (incl. beating v1 everywhere) + layout
cargo run --release -p dsp --example eval            # accuracy table: v1 vs new
cargo run --release -p capture --example monitor     # live console view of detected sources [-- --pid N --secs N]
cargo run --release -p deafoverlay                   # the overlay itself (Alt+Q quits)
```

Change DSP tunables (the consts at the top of `crates/dsp/src/lib.rs`) only against the eval table. Add a scenario to `eval.rs` for any new failure case you find, rather than tuning by ear.

## Architecture

The entire application is **one translation unit**: `DeafOverlay/DeafOverlay.cpp` (~1800 lines). There is no class hierarchy — it's a flat Win32 program built around global state and a single message/render loop in `wWinMain`. When adding behavior, follow the existing pattern of file-scope `static` globals (the `g*`/`k*` naming) rather than introducing new abstractions.

**Vendored dependency:** miniaudio is the only third-party code, included as `DeafOverlay/third_party/miniaudio.h` with `#define MINIAUDIO_IMPLEMENTATION`. Never edit it. Note that `DeafOverlay/DeafOverlay.h` is an **unused duplicate copy** of miniaudio — the `.cpp` includes `third_party/miniaudio.h`, not the sibling header. Don't add code to `DeafOverlay.h` expecting it to compile in.

### Two threads, shared via atomics

1. **Audio callback thread** (`audio_cb`, driven by miniaudio): runs a 6-band cascaded one-pole IIR filterbank (band = difference of adjacent low-passes) **per captured channel** and writes smoothed per-channel/per-band energy into `std::atomic<float> gChan[MAX_CH][NB]`. The capture is opened with `cfg.capture.channels = 0` so it inherits the device's **native layout** (stereo / 5.1 / 7.1); `audio_init` resolves each channel's physical azimuth into `gChanAzi[]`/`gChanHasDir[]`, and the actual count goes in `gNumCh`. This is the only cross-thread channel — keep it lock-free; do not call Win32/GDI from here.
2. **Main thread** (`wWinMain` loop, ~60 fps via `Sleep(16)`): reads the atomics, runs the DSP-to-visual pipeline, then renders. Per frame it does: per-channel noise-floor tracking + auto-gain normalization → transient detection → **adaptive direction** → peak generation → render → blit.

### DSP → direction (adaptive to channel count)

Direction estimation **branches on `gNumCh`** because a stereo downmix physically cannot encode front/back:
- **Multichannel (`nCh >= 6`, i.e. 5.1/7.1):** for each band, an **energy-weighted direction vector** over the channels' physical azimuths (`atan2(Σ E·sinθ, Σ E·cosθ)`) gives true, unambiguous **360°** including front/back; `focus` = vector length / total energy (point-like vs diffuse).
- **Stereo (`nCh == 2`):** **ILD only**, `(R−L)/(R+L)` through a `tanh` curve → a **front-arc-only** azimuth. Rear/elevation are deliberately **not** produced (`rearGain = 0`) — they can't be recovered from two channels, so the code no longer fabricates them. (The older spectral-notch rear/elevation heuristics were removed for this reason.) Stereo confidence is **coherence-gated**: the callback maintains a running normalized L/R correlation (`gCoh`, ~1 for a single amplitude-panned source, ~0 for diffuse ambience) which multiplies the per-peak confidence so reverb/music don't jitter the HUD.

Transient detection is **spectral (band) flux** — the sum of positive per-band level increases (`gBandPrev`) — which is more onset-selective than the previous total-level delta. (A full FFT band-split was scoped but not done: it needs a vendored FFT lib or a from-scratch FFT plus a buffering/windowing rewrite of the lock-free callback, for low marginal gain over the IIR bands.)

Channel→azimuth mapping lives in `ma_channel_to_azimuth` (with `default_azimuth` as a fallback when the device reports no channel map). Base tuning constants are in the `Base tunables` / `Accuracy-first tunables` blocks at the top of the file — adjust behavior there rather than scattering magic numbers in the loop. Detected sounds become entries in the `g_peaks[]` array, merged by angle (`add_peak_merge`), decayed each frame, and consumed by the drawing code.

### Rendering: software-drawn layered window

There is no GPU/Direct2D path — everything is **CPU per-pixel writes** into a 32-bit top-down `CreateDIBSection` (`DIB g_dib`), using **premultiplied alpha** (`putpx_PM`). The frame is pushed to screen with `UpdateLayeredWindow` (`blit_layered`). Per-pixel passes (e.g. `draw_ring`, the settings dim, the clear in `render_frame`) are O(width×height) every frame, but the window is now small (see below) so this is cheap. GDI `TextOutW` doesn't set alpha, so text uses the `text_out_opaque` / `force_alpha_rect` helper to force opacity where glyphs land.

### Window behavior & input

A **compact, movable** `WS_POPUP` window (not fullscreen — a fullscreen transparent layer matching the game client rect is the textbook ESP-overlay heuristic), `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT`. Geometry constants are near the top of the file: `HUD_W`, `HUD_AREA_H`, `WIN_H_NORMAL`, `WIN_H_SETTINGS`. The HUD (sphere+ring) always occupies the top `HUD_AREA_H` region, computed against `HUD_W`/`HUD_AREA_H` (**not** the DIB size, which changes between modes).

**The window size is driven by the DIB size**, not by `MoveWindow`: `blit_layered` passes `pptDst = nullptr` to `UpdateLayeredWindow` (keeps the screen position) and sets the size from the DIB. So to resize the window you resize `g_dib`. `set_settings_mode` does exactly this — it recreates `g_dib` at `WIN_H_SETTINGS` (grows downward to reveal the panel below the HUD) or `WIN_H_NORMAL`, and `WM_SIZE` is intentionally a no-op. Click-through in normal mode comes from `WS_EX_TRANSPARENT` + `HTTRANSPARENT`; settings mode clears `WS_EX_TRANSPARENT` and draws a light full-window dim so every pixel is opaque enough to capture clicks. The window's screen position lives in `gWinX`/`gWinY` (persisted as `winX`/`winY`); the user repositions the HUD by dragging in settings mode, which triggers a system move via `SendMessage(WM_NCLBUTTONDOWN, HTCAPTION)`.

The settings panel is a hand-rolled **immediate-mode UI**: sliders, checkboxes, buttons (`draw_button`), and info "i" icons are drawn pixel-by-pixel each frame, and clicks are matched against manually-registered rectangles (`gInfoRegions`, `gHitRegions`, with integer hit-IDs handled in `WM_LBUTTONDOWN`) and recomputed slider Y positions. The draw code and the hit-test code compute the same layout independently — **if you move/add a control, update both the drawing in `draw_settings_panel` and the geometry in the mouse handlers**, or clicks will land on the wrong control.

The settings window has **two views**, switched by `gShowSetup` (both share the same window geometry): the **Controls** view (sliders/swap/presets, `draw_settings_panel`) and the **Setup** view (`draw_setup_body`). Setup auto-detects the captured layout from `gNumCh` and shows status + deep-link buttons (`ShellExecuteW` to `ms-settings:sound` and `ms-settings:sound-defaultoutputproperties`) + the **L/R self-test**. The slider hit-tests in `WM_LBUTTONDOWN` are guarded by `!gShowSetup` since they aren't drawn in the Setup view.

The **L/R self-test** opens a *second* miniaudio device — a **playback** device (`g_tone`, `tone_cb`) that plays a 600 Hz tone panned hard-left then hard-right, driven by a small time-based state machine in the `wWinMain` loop (`gSelfTestActive`/`gSelfTestT0`/`gToneCh`). Because the loopback captures system audio, the tone lights the HUD on the matching side — a visual confirmation of the whole pipeline for a deaf user. It reuses the `g_ctx` created by `audio_init`, so it only runs when `g_audioOK`. (This is the only place a second audio device exists; remember `tone_stop()` on teardown.)

Global hotkeys (registered in `wWinMain`): **Alt+H** show/hide, **Alt+I** settings, **Alt+Q** quit, **Alt+G** capture only the focused game.

**Capture target (per-process loopback).** By default the loopback captures the whole system mix. **Alt+G** reads the foreground window's PID (`GetForegroundWindow`/`GetWindowThreadProcessId`) and re-opens the loopback scoped to that process tree via `cfg.wasapi.loopbackProcessID` (already in the vendored miniaudio) — so only the game's audio drives the HUD (no Discord/music/system sounds). `audio_open_loopback(pid)` is the single device-open path (pid 0 = system-wide); `audio_set_process(pid)` switches at runtime and **falls back to system-wide** if the per-process open fails (needs Windows 10 build 20348+, flagged via `gProcLoopbackUnsupported`). The target PID is **not persisted** (PIDs change between sessions); each launch starts system-wide. The Setup view shows the current `gCapturePID`/`gCaptureName`.

### Persistence

- User settings → `%APPDATA%\DeafOverlay\settings.json`, written and parsed by **hand-rolled** mini-JSON helpers (`save_settings_file` / `load_settings_file`, `json_find_number`). It is not a real JSON parser — it does substring/key lookups, so keep the format flat and simple. Loaded values are clamped to valid ranges on read.
- First-run onboarding flag → registry `HKCU\Software\DeafOverlay\OnboardingDone` (DWORD), via `reg_read_dword` / `reg_write_dword`. On first launch (`show_first_run_guidance` sets `gFirstRun`) the app auto-opens the in-app **Setup** view instead of the old wall-of-text `MessageBox`. To re-trigger the first-run flow during testing, delete that registry value.

Game presets (`apply_preset`: CS2 / R6 Siege / Call of Duty) are just bundles of sensitivity + 6 band-gain values applied to the same globals; they're mutually exclusive (`set_preset_exclusive`) and saved like any other setting.
