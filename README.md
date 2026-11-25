# SEGNALE DEBOLE

**Play live in your browser:** **https://signal.bombatomica64.dev** *(share custom bunker seeds with `#seed=MARS7`)*

A Doom-style 3D retro boomer-shooter roguelike built in **Rust + Bevy 0.20** for a work game jam on the prompt **"Segnale debole"** (*Weak Signal*).

Mars Research Bunker 7 has gone dark. Your **left hand grips a cracked tactical smartphone** hunting for a signal bar; your **right hand grips a pump shotgun**. Navigate procedural multi-elevation Mars bunkers, smash Demonic Jammers projecting Dead Zones, reach the **5-bar Extraction Signal Tower**, survive the **SOS Countdown Demon Rush**, take down **THE CARRIER DAEMON** on Floor 5, and download firmware upgrades from the **App Store** between floors. Then keep going in **Endless Overdrive**.

## Features
- **Live diegetic 3D phone OLED:** signal bars, dBm readout, oscilloscope waveform, tower radar, `[RMB] PING` and `[Q] 5G EMP` app tiles, SOS progress and Dead-Zone static, all rendered on the left-hand smartphone.
- **3-weapon arsenal:** Mars Pump Shotgun (hitscan pellets), UAC Plasma Repeater (3D energy bolts) and the **Signal-Mortar Rocket Launcher** (splash damage plus **Quake-style rocket jumping**). Phone Apps add **Ping Scan**, **5G Overload EMP** and **Airplane Mode Dash**.
- **Quake / CPMA movement:** bunny hopping, air-strafing, CPMA air control, dash-to-bhop momentum carry, a `VEL` tachometer HUD, and persistent **Peak Speed** and **Max Bhop Chain** records.
- **7 demon archetypes:** Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell Baron, **Phase Stalker** (blinks through Dead Zones), **Catwalk Relay Sniper** (telegraphed rail lock-on), and **THE CARRIER DAEMON** boss, a 5-phase Signal-Shield fight with destructible pylons on Floor 5 and on every 5th Endless floor.
- **High-speed bunker arenas:** procedural labyrinths with ramps, catwalks, lava trenches, an open-roof atrium with **twin atrium ramps**, **Signal-Gated Jump Pads** (powered only at 2+ bars), **Explosive Server-Rack Barrels** and **5 sector visual themes** (Rust Bunker, Toxic Coolant, Cryo Relay, Magma Foundry, Obsidian Core).
- **Roguelike extraction loop and meta-progression:** SOS countdown rush, orbital extraction beam, **17 App Store upgrades** (12 core, including Doppler Overdrive and Blast Thrusters, plus 5 feat-licensed signature apps) curated into a **12-card active catalog**, **4 Starter Phone OS Chips**, **4 stackable Carrier Contracts**, **4 3D Handset Skins** and **10 Feats**, persisted to JSON. A non-destructive **Jam Demo: Unlock All** toggle is included.
- **Endless Overdrive mode:** press `E` on the Floor 5 victory screen to keep descending, with themes cycling and Carrier Daemon rematches on floors 10, 15, 20 and so on.
- **Persistent Settings menu (`O`):** mouse sensitivity, base FOV, speed-FOV and strafe-roll toggles and scales, master volume and mute, crosshair style and color.
- **100% procedural retro assets and spatial 3D audio:** nearest-neighbor 64×64 textures, sculpted 3D meshes, 31 procedural PCM synth SFX with **3D positional audio** (hear Phase Stalkers blink, Snipers lock on, Jammers hum through walls, surface footsteps on metal catwalks vs Mars regolith, landing thuds, shell-bounce clinks), and a **4-layer adaptive industrial-synth soundtrack** (`--features audio`):
  - **Dead-Zone Drone:** E1 sub-bass, FM shimmer and RF static.
  - **Signal Pulse:** 16th-note E-Phrygian bass, clock-tick hats and radar pings.
  - **Rush Metal:** distorted power-bass riff, kick, snare, 16th hats and lead stabs.
  - **Boss Overdrive (Milestone 20):** relentless 16th-note bass gallop, distorted E-Phrygian Dominant alarm lead and double-kick fills for Carrier Daemon fights.

  All four stems are equal-length 132 BPM looping tracks spawned in parallel. They crossfade live with signal strength, the SOS upload, Time Trial, the boss fight (and Overdrive), and menu or pause state.
- **Sculpted 3D models (Milestone 15):** procedurally sculpted meshes for all 7 demons, the Carrier Daemon and its pylons, and all 3 guns, merged into per-material batches at startup.
- **3D world architecture (Milestone 16):** baseboards, crown trim, pilasters, overhead trusses and conduits, catwalk girders, flush wall-mounted sector props and animated hazards, baked into the static level batches.
- **Enemy animation and combat feedback (Milestone 17):** procedural attack animations, directional hit-stagger, pooled floor and wall decals (blood, ichor, scorch marks, plasma burns) and gibs.
- **Daily Seeded Run, split timer and Time Trial (Milestone 18):**
  - **`DAILY RUN [D]`:** the UTC date picks the seed and the loadout, so every player (native or browser) gets the same bunker that day.
  - **Speedrun split timer:** per-floor splits with PB deltas.
  - **`TIME TRIAL [T]`:** a handcrafted movement course with 3 checkpoint rings and the tower finish, plus instant `R` reset.
- **Web / itch.io build (Milestone 19):** WebGL2 in the browser, with saves in `localStorage` (see [Web / itch.io](#web--itchio)) and a self-hosted k3s + Cloudflare deployment at **https://signal.bombatomica64.dev** (`deploy/`).
- **Shareable Custom Seeds & 3D Bestiary / Armory (Milestone 21):**
  - **`SEED [S]` / `#seed=CODE`:** enter or roll deterministic bunker seeds (`MARS7`, `791E-0A5A`) and share clickable `#seed=` links directly from the menu, pause, or end screens.
  - **`BESTIARY & ARMORY [B]`:** live interactive 3D turntable inspecting all 7 sculpted demons (with lifetime kill counts and tactical tips) and all 3 weapons.
- **Wide high-speed corridors (Milestone 25):** 2-cell-wide boomer-shooter corridors and open turn hubs keep bhop chains and rocket jumps flowing through every floor.
- **Authentik login, cloud profile sync & global leaderboards (Milestone 26):**
  - **`LEADERBOARD [K]`:** top 25 of today's **Daily** (victory, floor, then time) and of the **Time Trial** (with CP splits). `[ID]` marks Authentik-verified entries, and your own callsign is highlighted. Switch boards with `Tab`/`Q`/`E` or `LB`/`RB`, refresh with `R`/`Y`, and go back with `Esc`/`B`.
  - **Callsign:** press `C` (or confirm on the plate) to type `[A-Z0-9_-]`, 2 to 12 characters. `Enter` saves it locally and syncs it to the cloud when you're signed in. On a controller, the D-Pad cycles and adds or removes characters.
  - **Account:** on the web, `SIGN IN` goes through Authentik (`auth.bombatomica64.dev`). Once signed in, the profile is merged into Postgres **monotonically**: unlocks are unioned, records and lifetime stats take the max, the fastest PBs win, and settings stay per device. Play on several devices and your progress never goes backwards. Desktop and Android can pass a session token in `SEGNALE_AUTH_TOKEN`.
  - **End-card auto-submit:** Daily Victory and Game Over (exhibition runs are never sent) and Time Trial finishes are posted in the background. The end card shows `SUBMITTING...`, then `GLOBAL RANK #N (NEW PB!)` or `OFFLINE - SAVED LOCALLY`.
  - **Offline-first:** with no server the screen reads `OFFLINE - LOCAL RECORDS ONLY` and shows your local Daily and Time Trial records. Tests, bots, screenshot tours and `SEGNALE_LEADERBOARD_OFFLINE=1` never touch the network. Desktop overrides the server with `SEGNALE_LEADERBOARD_URL`.
- **v1.0 jam polish (Milestone 27):**
  - **Live input prompts:** every keycap, footer and HUD hint follows the device you touched last (mouse & keyboard or controller), switching instantly mid-screen: the main menu field manual, `STAMINA [SHIFT]` / `[LB]`, the tower `[E]` / `[X]` prompt, the phone's `RMB PNG` / `LT PNG` tiles, the weapon deck and every menu footer.
  - **`L3` jump:** hold the left stick to bunny hop while the right thumb stays on the Right Stick for air-strafe aiming (`A` still works).
  - **Leaderboard pages:** the Top 25 shows 10 rows per page (`[` / `]`, arrows or `LT` / `RT`, plus PREV / NEXT). A failed sign-out keeps you signed in (`SIGN OUT FAILED - RETRY`), an expired session says `SESSION EXPIRED - SIGN IN AGAIN`, and refused scores show a short reason (`RATE LIMITED - TRY AGAIN SOON`, `REJECTED: ...`). Desktop / Android guests see why their scores post as GUEST.
  - **Embed-safe layout:** below 1280x720 the whole UI scales down uniformly (960x540 -> 0.75), so every screen fits itch.io embeds; upgrade cards are fluid.
  - **Audio:** a rising chime on Time Trial pit rescues and quiet, debounced menu focus / confirm blips. On the web a small banner asks for a click or key press while the browser still holds audio suspended (controller-only players).
  - `F9` (skip floor) is a dev tool: debug builds or `SEGNALE_DEV=1` only.
- **90–120 FPS target:** static and demon mesh batching, a `F3` perf overlay and a `F4` VSync toggle.

## Controls
- `WASD`: move | `Mouse`: look (`Click` to capture; `Esc` / `P` opens the pause menu)
- `Space`: jump (hold to bunny hop; `A`/`D` plus mouse turn to air-strafe) | `Shift`: Airplane Mode Dash (`Shift`+`Space` = dash-hop)
- `LMB`: fire | `1` / `2` / `3` / `Tab` / `Wheel`: Shotgun / Plasma Repeater / Signal-Mortar (fire a rocket at your feet while jumping to rocket jump)
- `RMB`: **Ping Scan** (1+ bar) | `Q`: **5G Overload EMP** (3+ bars)
- `E`: trigger the SOS Uplink at the 5-bar Signal Tower
- `1` / `2` / `3` or click: draft a firmware upgrade in the App Store
- Menus: `Enter` play / deploy / resume | `L` Loadout | `C` Contracts & Feats | `O` Settings (main menu and pause) | `E` continue into **Endless Overdrive** (Floor 5 victory screen) | `Esc` back or pause (`R` restart, `M` main menu while paused)
- Performance: `F3` toggles the FPS / frame-time overlay | `F4` toggles VSync
- **Controller** (no mouse click needed): Left Stick move | Right Stick look | `RT` fire | `A` or `L3` jump (hold to bunny hop) | `LB` dash | `LT` Ping Scan | `RB` 5G Overload | `X` SOS uplink | `Y` / D-Pad Right next weapon, D-Pad Left previous | D-Pad Down inspect | `Start` pause | `Select` / `R3` Time Trial reset
- Controller menus: D-Pad / Left Stick move the focus, `A` select, `B` back, `Start` play / deploy; Settings: Up/Down pick a row, Left/Right adjust; Bestiary: Left/Right or `LB`/`RB` cycle, `Y` switch archive, Right Stick spins. Gamepad sensitivity and Invert Y live in Settings.

## Build & Run
```bash
# Run (Linux/Windows/macOS, no ALSA required, silent build)
cargo run

# Run with procedural SFX + the adaptive 3-layer soundtrack
# (Linux needs ALSA dev libs: libasound2-dev pkg-config; works out of the box on Windows/macOS)
cargo run --features audio

# Optimized build
cargo build --release --features audio

# Native controllers (gilrs; Linux needs libudev-dev pkg-config). The web
# build always has controller support through the browser Gamepad API.
cargo run --features audio,gamepad

# GPU ray-traced lighting (Bevy Solari): hardware ray queries, ReSTIR GI,
# emissive lamps / lava / plasma as real area lights with ray-traced shadows
cargo run --release --features raytracing
```

Each GitHub release ships two archives per OS: `segnale-debole-*` (standard) and `segnale-debole-raytracing-*` (this feature on).

Ray tracing needs a GPU with ray-query support on Vulkan or DX12 (NVIDIA RTX, AMD RDNA2+, Intel Arc). On other GPUs the game logs a warning and falls back to raster lighting. Runtime switches:
- `SEGNALE_RT=off` disables it in a `raytracing` build.
- `SEGNALE_RT_LAMP=<gain>` (default `30`) sets ceiling-lamp emission.
- `SEGNALE_RT_SKY=<nits>` (default `4`) sets sky brightness through the atrium roof.

With ray tracing, world point lights and distance fog are off: Solari lights only from the sun and emissive meshes. MSAA is forced off. Expect some grain, because ReSTIR is the only denoising.

Linux build dependencies: `libasound2-dev pkg-config libx11-dev libwayland-dev libxkbcommon-dev`.

## Automated Testing & Playtest Bot
```bash
# Unit + headless ECS integration tests (incl. 5 autonomous playtest-bot seeds)
cargo test
cargo clippy --all-targets -- -D warnings

# Full test suite + live rendered autonomous playtest bot (under xvfb-run)
./scripts/playtest.sh
```

Meta-progression screenshot tour (uses an in-memory showcase profile and never touches your saved profile):
```bash
xvfb-run -a -s "-screen 0 1280x720x24" env SEGNALE_META_SHOTS=$HOME/scratch/meta-shots cargo run
# Milestone 26: main menu account pill + staged Daily / Time Trial boards, callsign editor, offline fallback
xvfb-run -a -s "-screen 0 1280x720x24" env SEGNALE_LEADERBOARD_SHOTS=$HOME/scratch/lb-shots cargo run
# Milestone 27: embed layouts (main menu, paginated Top 25, upgrades, HUD; M&K and controller prompts)
xvfb-run -a -s "-screen 0 1280x720x24" env SEGNALE_WINDOW_SIZE=960x540 SEGNALE_LAYOUT_SHOTS=$HOME/scratch/layout-540 cargo run
```

Backend (`server/`, a standalone crate with its own `Cargo.lock`): an in-memory store runs the HTTP tests, so no Postgres is needed. Set `TEST_DATABASE_URL` to also run the Postgres test.
```bash
cargo test --manifest-path server/Cargo.toml
cargo clippy --manifest-path server/Cargo.toml --all-targets -- -D warnings
```

## CI & Releases
- **`.github/workflows/ci.yml`:** runs on pushes to `main` and on pull requests. It runs `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy --all-targets --features audio,gamepad -- -D warnings` and `cargo clippy --all-targets --features raytracing -- -D warnings` on Ubuntu. Parallel jobs run `cargo clippy --target wasm32-unknown-unknown --features audio -- -D warnings` and `cargo ndk -t arm64-v8a -P 26 clippy --lib -- -D warnings` (Android).
- **`server` job (`ci.yml`):** runs `cargo test` and `cargo clippy -D warnings` on the `server/` backend.
- **`.github/workflows/deploy-web.yml`:** on `main` pushes (path-filtered) and `v*` tags, builds and pushes `ghcr.io/<owner>/segnale-debole-web` (nginx + WASM) and `ghcr.io/<owner>/segnale-debole-api`.
- **`.github/workflows/release.yml`:** runs on `v*` tags and manual dispatch. It builds Linux `--release --features audio,gamepad` binaries (plain and `raytracing`) and packages each with this README:
  - `segnale-debole-linux-x86_64.tar.gz`
  - `segnale-debole-raytracing-linux-x86_64.tar.gz`
  - `segnale-debole-web-itch.zip`: the WebGL2 build (`trunk build --release --features audio --cargo-profile wasm-release`), with `index.html` at the zip root so it can be uploaded to itch.io as-is.
  - `segnale-debole-android-arm64.apk`: the Android build (`android/build-apk.sh --release`), signed with the Gradle debug key for sideloading.

  Every run uploads the archives as workflow artifacts. A `v*` tag also publishes them to a GitHub Release.

To cut a release:
```bash
git tag v1.0.0 && git push origin v1.0.0
```

## Web / itch.io
The game also runs in the browser on WebGL2. One-time setup:
```bash
rustup target add wasm32-unknown-unknown
cargo install --locked trunk      # trunk fetches the matching wasm-bindgen CLI
# plus binaryen's wasm-opt on PATH (apt/brew `binaryen`, or a GitHub release)
```

Build and run:
```bash
# Dev server with live reload on http://127.0.0.1:8080
trunk serve --features audio

# Release build into dist/ (gitignored)
trunk build --release --features audio

# What CI ships: same, with the size-tuned profile (fat LTO, opt-level "s")
trunk build --release --features audio --cargo-profile wasm-release
```

To publish on itch.io, zip the **contents** of `dist/` (`cd dist && zip -r ../segnale-debole-web-itch.zip .`), upload it as an HTML5 project and tick **"This file will be played in the browser"**. The `segnale-debole-web-itch.zip` release asset is already in that shape. `Trunk.toml` sets `public_url = "./"` so asset URLs stay relative inside itch.io's iframe.

Differences from the native build:
- **Saves:** the profile lives in browser `localStorage` under `segnale_debole.profile.v1`. A corrupt entry is copied to `segnale_debole.profile.v1.bak` before starting fresh. If the browser blocks storage (some private windows), progress is not saved and the Loadout and Contracts screens say so.
- **Mouse:** click the game to capture the mouse. Pressing `Esc` (or switching tabs) releases the browser's pointer lock and pauses the run. Some browsers briefly refuse to re-lock right after `Esc`, so click again if mouse look doesn't resume right away.
- **Rendering:** MSAA is off, and only the nearest 12 world point lights are lit at once (WebGL2 light budget). `F4` shows `VSYNC MANAGED BY BROWSER`, because the browser paces frames. `F3` works as usual.
- **Audio** starts on your first click or key press (browser autoplay policy).
- The `raytracing` feature and the `SEGNALE_*` environment switches are native-only.

## Deployment (k3s + Postgres + Authentik)
https://signal.bombatomica64.dev runs as one pod with two containers. The `nginx` container serves `dist/` and proxies `/api/` to the `api` sidecar (`segnale_server`: axum + sqlx on `127.0.0.1:8081`).
- **API (`/api/v1`):** `healthz`, `auth/{login,callback,logout,me}` (OIDC code flow with PKCE; the session is an HttpOnly `segnale_session` cookie, valid for 30 days), `profile` (GET, plus PUT with a monotonic merge), and `leaderboard/{daily,trial}` (GET top 25, plus a strictly validated, per-IP rate-limited POST that keeps the best run only). Tables are created idempotently at startup: `players`, `sessions`, `daily_scores`, `trial_scores`.
- **Authentik:** add `deploy/authentik-blueprint-snippet.yaml` (provider `provider-segnale-debole` and application `segnale-debole`) to the GitOps blueprints. The redirect URI is `https://signal.bombatomica64.dev/api/v1/auth/callback`.
- **Secrets** are never in git. Create them once:
  ```bash
  kubectl -n segnale-debole create secret generic segnale-debole-secrets \
    --from-literal=DATABASE_URL='postgres://USER:PASS@HOST:5432/segnale' \
    --from-literal=OIDC_CLIENT_SECRET='...'
  ```
- **Deploy:** `deploy/deploy-k3s.sh` does everything in one step: it builds the WASM, builds the `web` and `api` images, imports them into k3s, runs `kubectl apply -f deploy/k8s.yaml` and waits for the rollout.

See [DESIGN.md](DESIGN.md) for the full game design and architecture notes.

## Android

Native arm64 Android build (GameActivity, Vulkan / GLES 3.2, landscape,
immersive fullscreen). **A controller (Bluetooth / USB) or a mouse & keyboard
is required: there are no touch controls.** Controllers and the mouse reach
Bevy through a small JNI bridge (`src/android_input.rs` +
`android/app/src/main/java/.../MainActivity.kt`), because Bevy 0.20.1 has no
Android gamepad backend and winit reports mice as touches. In a run the mouse
uses Android pointer capture. Backgrounding the app pauses the run and
suspends audio. The profile is stored in the app's internal storage.

```bash
# Needs JDK 17, the Android SDK (platform 35) + NDK, and:
rustup target add aarch64-linux-android
cargo install cargo-ndk

# libsegnale_debole.so (cargo ndk) + debug APK (Gradle wrapper)
ANDROID_HOME=~/Android/Sdk android/build-apk.sh          # release Rust lib, assembleDebug
android/build-apk.sh --debug                             # debug Rust lib
android/build-apk.sh --release                           # assembleRelease (debug-key signed)
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```
