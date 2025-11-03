🎯 Master Prompt: "Deaf Assisted Directional Audio Overlay Project"
Project Title: Deaf Assisted Directional Audio Overlay for PC Games Core
Vision: Create a real-time PC gaming overlay that helps deaf and
hard-of-hearing players visualize in-game sounds through a directional,
color-coded visual HUD. The overlay interprets audio spatially
(left/right/front/back) and displays it as an elegant, minimal 3D sphere
with a glowing "Saturn ring" that indicates sound direction and
loudness.

🧠 Concept Overview The system captures the game's audio output,
analyzes it in real time, determines the direction and intensity of each
sound, and visually represents it on screen. Visual Metaphor: A floating
sphere made of dots (representing 3D space). A ring (like Saturn) around
the bottom of the sphere shows a clear, 2D compass view of sound
direction. Color intensity encodes loudness: White: faint or distant
sound Orange: moderately audible Red: loud or nearby sound The overlay
is draggable, semi-transparent, and click-through when locked. End Goal:
A fully packaged Windows app (and later, console companion) that enables
deaf players to "see sound."

🧩 Technical Breakdown 1. Audio Input Capture system/game audio via
WASAPI loopback. Support for stereo and 5.1/7.1 surround when available.
Buffer size target: 10--20ms per frame for low latency. Sample rate:
48kHz, 16-bit or 24-bit. 2. DSP (Digital Signal Processing) Detect sound
onsets (short bursts of energy). Compute direction using: GCC-PHAT for
stereo (time difference between L/R). Channel energy mapping for 5.1/7.1
(assign azimuths). Optionally classify sounds (gunshot, step, explosion)
using spectrogram + CNN later. Smooth direction + loudness using
exponential moving averages or Kalman filters. 3. Visual Overlay (HUD)
Always-on-top transparent window using: Direct2D (C++) for native or
Canvas/WebView (Overwolf/Game Bar widget). Draw components: 3D sphere
made of glowing dots. Saturn-like ring around base. Highlight pulse at
detected azimuth (and elevation, if possible). Fade and size scale with
loudness and time. Must support drag, reposition, lock/unlock, and
opacity control. Performance: \<2% CPU and \<1% GPU usage ideally. 4.
Color Mapping Color blend formula (smoothstep curve) for seamless
transition. 5. User Settings Size, position, opacity, FPS limit Color
theme (contrast modes) Sound sensitivity calibration Ring thickness,
fade time, and animation style 6. Anti-Cheat & Safety No DLL injection
or memory reading. Use only system audio. Deploy via Overwolf SDK or
Xbox Game Bar Widget for safety and platform compliance. 7. Testing &
Metrics Test with FPS and racing games for real-time responsiveness.
Validate latency (\<30ms end-to-end). Verify click-through and no frame
drop in fullscreen games. User feedback from deaf gamers (accessibility
testing). 8. Deployment Build with C++17/20, using CMake + vcpkg.
Integrate miniaudio (for WASAPI) + KissFFT (for GCC-PHAT). Package into:
Overwolf app for distribution (PC) Later: Xbox/PlayStation Accessibility
companion app

🚀 Roadmap

🧰 Tech Stack Summary

🎨 UX Summary Minimal, futuristic, and non-distracting design. Sphere =
3D soundspace Ring = directional compass Color + motion = intensity
feedback Works in peripheral vision --- lets the player focus on
gameplay.

💡 Purpose & Impact This project addresses a major accessibility gap in
PC gaming: While mobile games often visualize sound (e.g., PUBG Mobile
for deaf players), PC and console titles rarely offer it natively. This
overlay empowers deaf players with spatial awareness, enabling
competitive and immersive play by transforming audio cues into visual
ones in real time.

🧭 Tagline Ideas "See the Sound." "Turning Hearing into Vision."
"Accessibility meets immersion." "Every sound, every direction ---
visible."
