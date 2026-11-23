# SEGNALE DEBOLE

A Doom-style 3D retro boomer shooter roguelike built in **Rust + Bevy 0.20** for a work game jam on the prompt **"Segnale debole"** (*Weak Signal*).

Mars Research Bunker 7 has gone dark. Your **left hand grips a cracked tactical smartphone** hunting for a signal bar; your **right hand grips a pump shotgun**. Navigate procedural multi-elevation Mars corridors, smash Demonic Jammers projecting Dead Zones, reach the **5-bar Extraction Signal Tower**, survive the **SOS Countdown Demon Rush**, and download firmware upgrades from the **App Store** as you push through Floors 1–5.

## Features
- **Live Diegetic 3D Phone OLED Screen:** Signal bars, dBm readout, oscilloscope waveform, directional tower radar, `[RMB] PING` & `[Q] 5G EMP` app status tiles, SOS progress, and Dead-Zone static snow rendered directly on the left-hand smartphone.
- **Dual-Wield Boomer Shooter Combat:** Mars Pump Shotgun (hitscan pellets + shell ejection) & UAC Plasma Repeater (3D energy bolts), plus signal-powered Phone Apps (**Ping Scan**, **5G Overload EMP**, **Airplane Mode Dash**).
- **4 Classic Doom-Inspired Demons:** Static Imps (electric-violet Dead-Zone enrage), floating Dead-Zone Cacodemons, mobile **Jammer Fiends** (project a live Dead Zone and stall SOS uplinks), and the **Hell Baron**.
- **Roguelike Extraction Loop & Meta-Progression:** Procedural 3D bunkers with ramps, catwalks, lava trenches, open-roof crimson atrium, SOS countdown rush, orbital extraction beam, **15 App Store upgrades** (10 core + 5 feat-licensed signature apps curated into a **10-card active catalog**), **4 Starter Phone OS Chips**, **4 stackable Carrier Contracts**, **4 3D Handset Skins**, and **10 Feats** persisted to JSON (plus a non-destructive **Jam Demo: Unlock All** toggle).
- **100% Procedural Retro Assets & Audio:** Nearest-neighbor 64×64 textures, sculpted 3D meshes, and pure-Rust procedural PCM WAV synth SFX (`--features audio`).

## Controls
- `WASD` — Move | `Mouse` — Look (`Click` capture, `Esc` / `P` pause menu)
- `Space` — Jump (hold to bunny hop; `A`/`D` + mouse turn to air-strafe) | `Shift` — Airplane Mode Dash (`Shift`+`Space` = dash-hop)
- `LMB` — Fire | `1` / `2` / `Tab` / `Wheel` — Switch Shotgun / Plasma Repeater
- `RMB` — **Ping Scan** (1+ bar) | `Q` — **5G Overload EMP** (3+ bars)
- `E` — Trigger SOS Uplink at the 5-bar Signal Tower
- `1` / `2` / `3` or Click — Draft firmware upgrade in the App Store
- Menus: `Enter` — Play / Deploy / Resume | `L` — Loadout | `C` — Contracts & Feats | `Esc` — Back / Pause (`R` restart, `M` main menu while paused)

## Build & Run
```bash
# Run (Linux/Windows/macOS, no ALSA required)
cargo run

# Run with procedural retro WAV audio (requires ALSA dev libs on Linux; works out of the box on Windows/macOS)
cargo run --features audio
```

## Automated Testing & Playtest Bot
```bash
# Unit + headless ECS integration tests
cargo test

# Full test suite + live rendered autonomous playtest bot (under xvfb-run)
./scripts/playtest.sh
```

Meta-progression screenshot tour (in-memory showcase profile, never touches your saved profile):
```bash
xvfb-run -a -s "-screen 0 1280x720x24" env SEGNALE_META_SHOTS=$HOME/scratch/meta-shots cargo run
```

See [DESIGN.md](DESIGN.md) for the full game design and architecture notes.
