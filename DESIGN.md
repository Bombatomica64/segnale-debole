# SEGNALE DEBOLE — Game Design & Technical Architecture

> **Game Jam Prompt:** *Segnale debole* (Weak Signal)
> **Genre:** Classic Doom-Style 3D Retro Boomer Shooter × Roguelike Extraction
> **Engine:** Rust + Bevy `0.20.1` (full 3D, verticality, ECS, 100% procedural retro assets)
> **Setting:** Classic Doom Sci-Fi Hell — Mars Research Bunker overrun by demons under a blood-red sky

---

## 1. Core Fantasy & Design Pillar

You are a stranded Marine/Tech trapped deep inside a hell-corrupted Mars UAC-style bunker. Your **left hand grips a cracked tactical smartphone** hunting for a cell/uplink bar; your **right hand grips a pump shotgun**.

The jam prompt **"Segnale debole"** is not a passive gimmick — it drives **navigation, combat power, enemy threat, and extraction tension** simultaneously:
- **Weak Signal (0–1 bars — Dead Zones):** Your phone screen glitches with static, radar goes blind, your HUD flickers, and demons inside Dead Zones become **Static-Enraged** (faster, glowing red-static aura).
- **Moderate Signal (2–3 bars):** Clean directional signal bars & pulse tones guide you toward the Extraction Tower; basic Phone Apps come online.
- **Strong Signal (4–5 bars — Signal Hotspots & Extraction Tower):** Full **5G Overcharge** — faster weapon reload/fire-rate bonus, high-power Phone Apps unlocked, and the ability to trigger & accelerate the **SOS Extraction Uplink**.

---

## 2. The Roguelike Gameplay Loop (Labyrinth Extraction)

Each run descends/ascends through increasingly hostile Mars Bunker Sectors (**Floor 1 → Floor 5** for victory, plus endless scaling option):

### Phase 1: HUNT (Navigate the Dark Mars Labyrinth)
1. You spawn at the airlock of a procedurally generated **3D multi-level bunker maze** (corridors, industrial rooms, raised catwalks, ramps/stairs, sunken plasma trenches, and an open-roof central atrium).
2. Signal starts critically weak (**0–1 bars**). The **5-Bar Extraction Tower** is hidden somewhere in the labyrinth, while **Demonic Jammers** (pulsing fleshy-tech antennas) project **Dead Zones** that suppress signal in surrounding rooms.
3. Follow the phone's **signal bars**, **signal strength meter (dBm)**, and **Ping Scan** (Right-Click) to:
   - Locate and destroy **Demon Jammers** (clearing interference and dropping ammo/health).
   - Fight roaming **Static Imps**, **Dead-Zone Cacodemons**, and **Jammer Fiends**.
   - Find the **5-Bar Signal Tower** (typically positioned on an elevated platform or multi-entry atrium).

### Phase 2: SOS COUNTDOWN (Hold the Signal / Boomer Shooter Arena)
1. Reach the **5-Bar Signal Tower** and press `E` (or step onto the uplink pad) to trigger the **SOS Uplink** (`30s` on Floor 1, scaling up to `45s` on deeper floors).
2. **Alarm lights pulse crimson** across the atrium and a relentless **Demon Rush** converges from all corridors and upper catwalks, capped by an **Extraction Heavy / Hell Baron**!
3. **Risk-Reward Positioning:**
   - Standing inside the Tower's **Signal Ring (4–5 bars)** advances the SOS upload at **100% speed** and fuels your **5G Apps**.
   - Kiting outside the ring keeps you alive during heavy swarms, slowing/pausing the uplink until you fight your way back in.
   - If a **Jammer Fiend** enters the Tower zone, it jams the uplink until you blast it!

### Phase 3: EXTRACT & DOWNLOAD ("App Store" Roguelike Upgrades)
1. At **100% SOS Uplink**, an orbital beam locks onto the tower, vaporizing remaining demons and extracting you to the next sector.
2. With full 5-bar reception during extraction, your phone opens the **FIRMWARE / APP DOWNLOAD** screen — choose **1 of 3 randomized Roguelike Upgrades**:
   - **Dual-SIM Barrels:** Shotgun fires +3 extra pellets with tighter choke.
   - **5G Overclock:** At 4–5 bars, fire rate +35% and move speed +25%.
   - **External Copper Antenna:** +1.25 minimum baseline signal bars everywhere (resists Dead Zones).
   - **Signal Siphon App:** Killing demons inside ≥3-bar zones restores +8 HP.
   - **Firewall Daemon:** +40 Max Armor and regenerates Armor while uploading SOS.
   - **Packet Sniffer:** Ping Scan cooldown halved and marks enemies for +35% bonus crit damage for 5s.
   - **EMP Overcharge:** 5G Overload blast radius +40%, higher damage, chains to nearby demons.
   - **Airplane Mode Phase:** Dash (`Shift`) grants brief invulnerability + leaves a static shockwave.
   - **High-Capacity Cell:** +35% max ammo, +15% faster weapon fire rate, +30% pickups.
   - **Sub-GHz Penetration:** Wall signal attenuation reduced by 45% (better signal through bunker walls).

---

## 3. Dual-Wield Mechanics: Right Hand Gun + Left Hand Phone

### Right Hand — Classic Boomer Shooter Arsenal
- **1 — Mars Pump Shotgun (Default):** Punchy close-to-mid range 8-pellet spread, heavy recoil kick, muzzle light flash, chunky knockback.
- **2 — UAC Plasma Repeater:** Fast-firing glowing cyan energy bolts (true 3D projectiles) ideal for Cacodemons on upper catwalks and shredding Jammers at range.
- *(Switch with `1`/`2` or Mouse Wheel; unlimited backup recharge on low-ammo or generous demon drops so combat never stalls.)*

### Left Hand — Tactical Smartphone (Always Visible Viewmodel + Live UI)
Displayed in the bottom-left / left-hand FPS viewmodel with a live diegetic screen:
- **5 Signal Bars** (color-coded: Red 0–1, Amber 2–3, Neon Green/Cyan 4–5) + numeric **dBm** readout (`-118 dBm` to `-55 dBm`).
- **Signal Waveform / Proximity Compass** that pulses faster as you face and approach the Signal Tower (and jitters wildly near Jammers).
- **Active Phone Apps:**
  - **Right-Click — PING SCAN (Needs ≥1 bar, 4s cooldown):** Sends out a 3D expanding cyan wireframe/ring sonar pulse. Briefly reveals the Extraction Tower beam, Jammers, and nearby Demons with glowing beacons/markers even through walls! Stronger signal = longer scan radius & duration.
  - **`Q` — 5G OVERLOAD (Needs ≥3 bars, 10s cooldown):** Unleashes an intense 3D electromagnetic shockwave + chain lightning from your phone, dealing heavy damage, stunning demons, and shattering Jammer shields!
  - **`Shift` — AIRPLANE MODE DASH (2s cooldown):** High-speed Doom horizontal burst dash (scales with movement direction) to dodge fireballs and vault gaps.

---

## 4. Enemies (4 Distinct Silhouettes & Roles)

All enemies use bold, readable procedural 3D retro-stylized meshes / billboarded pixel-art sprites with emissive eyes, distinct colors, hit-flash feedback, and death bursts:
1. **Static Imp (Rust-Red):** Fast corridor skirmisher; hurls glowing orange fireballs; moves 30% faster and glows static-red when in 0–1 bar Dead Zones.
2. **Dead-Zone Cacodemon (Crimson/Violet Floating Orb):** Ignores floor geometry, hovers over ramps/catwalks, fires twin plasma orbs, forces vertical aim.
3. **Jammer Fiend (Sickly Green/Black Armored Cyber-Demon):** Carries a crackling demonic router/antenna on its back — projects a **mobile Dead Zone** (`-2.5 bars` radius) around itself! Priority target during both exploration and SOS defense.
4. **Hell Baron (Towering Molten-Gold/Red Heavy):** High-HP boss-tier bruiser that spawns during the SOS Countdown, throwing triple-spread hellfire volleys and charging the Tower pad.

---

## 5. Procedural 3D Mars Bunker Level Design (With True Verticality)

Grid-based room-and-corridor labyrinth (`21x21` to `29x29` cells, cell size `4.0m`) compiled into rich 3D Bevy geometry:
- **Multi-Elevation Zones:**
  - Ground corridors (`y = 0.0`)
  - Raised catwalks, observation decks, and Tower pedestals (`y = 2.4`) connected by walkable **3D sloped ramps** (`step_height` + smooth slope climbing in player physics!)
  - Hazardous lava/sludge pits or recessed server trenches (`y = -1.2`) with glowing red underlighting
  - **Open-roof Signal Atrium:** Tall walls opening to a dark crimson Martian sky with an iconic **3D Radio/Relay Signal Tower** and pulsing beacon light.
- **100% Procedural Pixel-Art Textures (`64x64`, `ImageSampler::nearest()`):**
  - Riveted steel bunker walls with warning stripes & grime
  - Grated metal catwalk floors & Mars regolith rock
  - Glowing crimson/amber tech panels & lava pools
  - Demonic flesh-wire textures for Jammers

---

## 6. Technical Architecture (Rust + Bevy `0.20.1`)

### Platform & Build Compatibility Notes
- **Bevy version:** `0.20.1`
- **Linux dependency safety:** On environments without `libudev-dev` or `libasound2-dev`, `Cargo.toml` disables `default-features` and explicitly selects the Bevy 0.20 feature collections (`default_app`, `3d_bevy_render`, `ui_bevy_render`, `std`, `bevy_winit`, `default_font`, `multi_threaded`, `x11`, `wayland`, `custom_cursor`, `png`), omitting `bevy_gilrs` and keeping `bevy_audio` behind an optional Cargo feature `audio` (`audio = ["bevy/bevy_audio", "bevy/wav"]`, which can be enabled on systems with ALSA or non-Linux targets).
- **Custom Lightweight 3D FPS Physics:** Fast, deterministic swept AABB / cylinder-vs-grid+ramp collision (`src/physics.rs`) — zero external physics crate version conflicts, silky-smooth Doom wall-sliding, ramp climbing, jumping, and instant raycasts for hitscan weapons & signal wall-attenuation!

### Module Layout (`src/`)
```
src/
├── main.rs            # App setup, states, plugin registration, window config
├── state.rs           # AppState (MainMenu, Playing, UpgradeSelect, GameOver, Victory), RunStats, FloorConfig
├── assets_gen.rs      # Procedural 64x64 nearest-neighbor retro textures & shared Materials/Meshes
├── level.rs           # Procedural 3D Mars Bunker labyrinth generator (rooms, corridors, ramps, catwalks, atrium, tower, jammers)
├── physics.rs         # Fast 3D FPS collision (walls, multi-elevation floors, walkable ramps, line-of-sight raycast)
├── player.rs          # Doom-speed FPS controller, mouse look, jump, Airplane Mode Dash, health/armor/ammo
├── signal.rs          # Continuous 0..5 bar signal solver (distance, wall occlusion, Jammers, Dead Zones), Ping Scan pulse, 5G Overload
├── weapons.rs         # Pump Shotgun (hitscan pellets) + Plasma Repeater (3D projectiles), viewmodel recoil/bob, pickups
├── enemies.rs         # Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell Baron AI, projectiles, hit-flash, death bursts
├── extraction.rs      # SOS Countdown state machine, alarm lighting, demon wave spawner, extraction beam
├── upgrades.rs        # Roguelike upgrade pool (10 upgrades), stat modifiers, App Store 1-of-3 selection
├── hud.rs             # Dual-wield left-hand Phone diegetic UI + right-hand weapon overlay + Doom HUD + Dead-Zone glitch FX + menus
└── audio.rs           # Procedural retro synth SFX generator (gated under `audio` feature, no-op stub otherwise)
```

---

## 7. Implementation Plan (Orchestrated via Claude Work Opus 5.5)

- **Milestone 1 (Foundation & World):** Cargo project (`bevy 0.20.1`), procedural retro textures, 3D multi-level Mars bunker labyrinth with ramps/catwalks/atrium, Doom FPS controller & collision, Signal Field solver (Tower + Jammers + wall attenuation), basic HUD readout + unit tests.
- **Milestone 2 (Combat, Phone & Roguelike Extraction):** Dual-wield Phone & Gun viewmodels, Shotgun + Plasma Repeater, Ping Scan & 5G Overload apps, 4 Demon archetypes with AI & projectiles, SOS Countdown rush + alarm lighting, 10 Roguelike App Store upgrades, Dead-Zone glitch FX, menus/game-over/victory flow, procedural SFX, and comprehensive tests.
- **Milestone 3 (Polish & Verification):** Playability audit, game-feel tuning (hitstop, recoil, FOV kick on dash, screen flash, signal readability), full `cargo test` + `cargo build --release` check.
