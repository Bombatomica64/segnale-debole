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
- **Controller** (no mouse click needed): Left Stick move | Right Stick look | `RT` fire | `A` jump (hold to bunny hop) | `LB` dash | `LT` Ping Scan | `RB` 5G Overload | `X` SOS uplink | `Y` / D-Pad Right next weapon, D-Pad Left previous | D-Pad Down inspect | `Start` pause | `Select` / `R3` Time Trial reset
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
```

## CI & Releases
- **`.github/workflows/ci.yml`:** runs on pushes to `main` and on pull requests. It runs `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo check --all-targets --features audio` (with warnings denied) on Ubuntu. A parallel job runs `cargo clippy --target wasm32-unknown-unknown --features audio -- -D warnings`.
- **`.github/workflows/release.yml`:** runs on `v*` tags and manual dispatch. It builds `--release --features audio` binaries and packages each binary with this README:
  - `segnale-debole-linux-x86_64.tar.gz`
  - `segnale-debole-windows-x86_64.zip`
  - `segnale-debole-macos-aarch64.tar.gz`

  - `segnale-debole-web-itch.zip`: the WebGL2 build (`trunk build --release --features audio --cargo-profile wasm-release`), with `index.html` at the zip root so it can be uploaded to itch.io as-is.

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

See [DESIGN.md](DESIGN.md) for the full game design and architecture notes.
