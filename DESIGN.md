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
2. With full 5-bar reception during extraction, your phone opens the **FIRMWARE / APP DOWNLOAD** screen — choose **1 of 3 randomized Roguelike Upgrades** drawn from your active **10-card App Store Catalog**:
   Each card shows a live **NEXT:** line previewing exactly what the next rank changes (e.g. `8 -> 11 pellets`, `Min 0 -> 1 bar`, `4.0s -> 2.0s ping, +35% dmg`). 15 total upgrades (10 core + 5 feat-licensed signature apps); stackable ones (S) can be re-bought, toggles (T) are drafted again only when the pool runs dry.
   - **Dual-SIM Barrels (S):** Shotgun fires +3 pellets with a 25% tighter choke.
   - **5G Overclock (S):** At 4–5 bars, fire rate +35% and move speed +25%.
   - **External Copper Antenna (S):** +1.25 minimum signal bars everywhere (capped at 4.0); resists Dead Zones.
   - **Signal Siphon App (S):** While you have 3+ bars, demon kills restore +8 HP.
   - **Firewall Daemon (S):** +40 armor & max armor; regenerates 6/s while the SOS advances in the Signal Ring (in ring **and not jammed** — a spared Jammer Fiend is not an armor farm).
   - **Packet Sniffer (S):** Ping cooldown halved; ping-marked demons take +35% bonus damage.
   - **EMP Overcharge (T):** 5G Overload radius +40%, damage +35%, chains to 3 nearby demons.
   - **Airplane Mode Phase (T):** Dash grants invulnerability, a static shockwave, and -15% dash cooldown.
   - **High-Capacity Cell (S):** Ammo caps +35%; all pickups +30% fuller. (No fire-rate bonus: that is 5G Overclock's niche.)
   - **Sub-GHz Penetration (S):** Walls attenuate 55% less (multiplicative per rank); every Ping Scan also projects a pulsing cyan **floor route** — the next 14 cells of the shortest walkable BFS path to the Tower (5 s) — so you can follow it around bunker walls.
   - **Store & Forward (T, Signature):** Kills during a *natural* 0-bar blackout (measured before the Copper Antenna floor, so antenna owners still qualify) queue up to 3 packets, shown as glowing pips on the phone's status bar. Reaching 3+ bars discharges every packet as a 20-damage lightning arc at the nearest demons within 22 m (cycling over targets; ping marks apply).
   - **Emergency Callback (T, Signature):** Killing a ping-marked Jammer Fiend unlocks one any-signal 5G Overload for 6 s (cooldown still applies). The phone's `Q EMP` tile flashes gold **CALL!**.
   - **Missed Call (T, Signature):** Pressing Ping at 0 bars drops a ringing, vibrating decoy handset (shares the Ping cooldown, one active at a time) that pulls every Imp within 18 m toward it for 2.5 s. The `RMB PNG` tile reads amber **DECOY** when ready.
   - **Signal Spoofer (T, Signature):** Dashing inside a 0–1 bar Dead Zone spoofs 4 bars of signal for 2.5 s (`6 s` rearm cooldown; preserves natural blackout detection for Store & Forward; OLED status bar reads cyan **SPOOF**).
   - **Jammer Hijack (T, Signature):** Destroying a Demon Jammer or Jammer Fiend turns its position into a temporary 5-bar Signal Hotspot for 8 s (`12 m` radius with glowing emerald floor ring & beacon; OLED status bar reads emerald **HIJK**).

### Phase 4: Outside-the-Round Meta-Progression (`src/meta.rs`, `src/meta_ui.rs`)
Core principle: **persist player choices, unlocks, and opt-in challenges — reset raw combat power at run start** (no permanent `+1 bar` or raw damage boosts that erase Floor 1's 0-bar tension).
- **Persistent JSON Profile (`serde` + `serde_json`):** Saved atomically to `$XDG_DATA_HOME/segnale-debole/profile.json` (`SEGNALE_PROFILE` override; in-memory for headless tests/bots/screenshots). Incompatible or corrupt files are backed up to `.bak` with a recovery notice; failed writes stay marked dirty and retry automatically.
- **4 Starter Phone OS Chips (Balanced Sidegrades):** *Stock UAC OS* (neutral), *Survey OS* (`-30%` Ping CD & `+25%` wall penetration, `-15` max HP), *Overclock Radio* (`+20%` fire & `+15%` move at 4–5 bars, `-25%` pickup yield), *Faraday Case* (`+25` starting armor & `-25%` damage taken in Dead Zones including hazards, `+25%` Ping/EMP cooldowns).
- **10-Card Active App Store Catalog:** Curate 10 unlocked apps before each run (missing slots display `AUTO-FILL` and pad deterministically on deploy) so unlocking all 5 signature apps keeps 4-draft run odds sharp.
- **4 Stackable Carrier Contracts:** *Roaming Charges* (`+30%` Jammer radius & `+20%` harsher wall loss), *Priority Jamming* (extra Jammer Fiend crashes every SOS + higher Fiend weight), *Overtime Uplink* (`+35%` SOS duration & `20%` faster rush waves), *Low-Battery Mode* (no between-floor HP heal).
- **4 3D In-Hand Handset Skins:** *UAC Standard Orange*, *Mars Hazard Yellow*, *Stealth Carbon*, and *5G Chrome Gold* — styling the live left-hand 3D smartphone viewmodel.
- **10 Feats & Jam Demo Mode:** All unlockable chips, signature apps, and skins tie transparently to 10 Feats; a one-click **JAM DEMO UNLOCK ALL** toggle lets judges/testers try everything immediately (exhibition runs — like `F9` debug skips — never pollute permanent feats or lifetime stats).
- **Pause Menu (`Esc` / `P`):** Freezes gameplay over the live 3D viewport with Resume, Restart Run, Loadout, and Main Menu.

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
  - **`Shift` — AIRPLANE MODE DASH (2s cooldown):** High-speed Doom horizontal burst dash (scales with movement direction) to dodge fireballs and vault gaps. Never slows you down: the dash keeps the faster of your current horizontal speed (in any direction, up to 48 m/s) and 30 m/s, so a 90° Airplane Mode dash at 30+ m/s redirects your full bhop speed around tight 4 m bunker corners. **Dash + `Space`** cancels the burst into a hop that keeps the full 30 m/s, the fastest way to start a bhop chain.

### Movement — Quake / CPMA Boomer-Shooter Hybrid (`src/player.rs`)
Tuned so plain corridor play stays snappy while skilled players can bunny hop and air-strafe well past run speed:
- **Ground:** Quake friction (`GROUND_FRICTION 8`, `STOP_SPEED 3.5`) + Quake accel (`GROUND_ACCEL 12`). Crisp: ~0.25 s from 0 to 11 m/s, ~0.3 s to a dead stop, no ice-skating.
- **`SPACE` — jump / hold to bhop:** Holding `Space` re-jumps on the landing tick *before* friction applies, so every hop keeps 100% of its horizontal speed. A tap up to **0.12 s** before touchdown is buffered, and **0.10 s** of coyote time still lets you jump after running off a ledge (never a double jump).
- **Air-strafing (`A`/`D` only + mouse turn):** CPMA strafe curve (`AIR_STRAFE_ACCEL 70`, wish speed 2.8 m/s). Turning smoothly while strafing curves your path and gains speed. **Strafe-jumping (`W+A`/`W+D`):** VQ3 air accel (`AIR_ACCEL 2.4`).
- **Air control (`W` only):** CPMA-style steering turns your airborne heading toward the view while keeping your speed, for tight 4 m bunker corners and 2.4 m catwalk landings.
- **High ceiling:** no air drag below 22 m/s, light quadratic drag above it (good chains settle around 30–34 m/s) and a 36 m/s soft ceiling: air-strafing never adds speed past it, and speed above it bleeds off smoothly (2.5× drag, at least 6 m/s²) instead of snapping, so losing a 5G Overclock bar mid-hop eases 45 → 36 m/s over ~0.4 s. All thresholds scale with Overclock / move-speed upgrades; 48 m/s is the only hard clamp.
- **Feel:** FOV widens with speed (up to +14° at 32 m/s, blended with the +12° dash kick, +18° max), a subtle ~1.4° strafe camera roll (all eased with frame-rate-independent exponential smoothing), a damped viewmodel bob that doesn't spike on one-frame bhop touchdowns, and a brighter kinetic **bhop chirp** from the second linked hop on (the same `BHOP x2+` milestone the HUD stamps).
- **HUD `VEL` strip** (a 31 px telemetry plate docked 4 px above the vitals plate): `VEL` label, a framed 16-segment tachometer (2 m/s per segment, 0–32 m/s) with fixed zones — ash 0–12, arc-cyan 12–20, acid-green 20–28, hazard-yellow 28–32 — split by 4 px zone breaks, lit segments at full zone color and unlit ones at a faint 16% tint, and a constant-width integer readout (`18 m/s`). The plate rail, track brackets and readout take the current tier color with 0.5 m/s hysteresis (no flicker at 12 / 20 / 28). At 32+ m/s the readout snaps to a solid hazard-yellow badge with black text (steady, no strobing). A solid stamped `BHOP xN` tag sits on top of the strip from a 2-hop chain.
- **Frame-rate independence:** every frame is split into uniform movement substeps of at most 1/120 s (timers, dash, jump, air-strafe and collision all run per substep, with the frame's mouse turn spread across them), so 30 / 60 / 120 / 144 Hz air-strafe chains end within 1 m/s of each other. Landing and re-hopping can happen inside one frame.
- **Physics safety:** collision substeps bound horizontal travel to half the player radius and vertical travel to half a step per substep, so 48 m/s hops never tunnel walls, pillars or ledges, and fast falls at low frame rates can't skip a catwalk top. A 9-point footprint (center + 8 points at 0.85 × radius) makes diagonal catwalk grazes support you like face-on contact; a touchdown anywhere inside a step (even crossing a catwalk edge at 30 m/s) keeps the coyote window; only a grounded player is glued down small drops (an airborne one is never snapped onto a catwalk early); and inside corners clip velocity per wall (Quake crease rule) instead of bouncing you backwards.
- **Aim:** shots use this frame's eye pose (body + camera transforms after movement and mouse look), so moving, turning and firing on the same frame hits where you see.

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
├── player.rs          # Quake/CPMA FPS controller (friction, air-strafe, air control, bhop, coyote/buffer), mouse look, Airplane Mode Dash, health/armor/ammo
├── signal.rs          # Continuous 0..5 bar signal solver (distance, wall occlusion, Jammers, Dead Zones), Ping Scan pulse, 5G Overload
├── weapons.rs         # Pump Shotgun (hitscan pellets) + Plasma Repeater (3D projectiles), viewmodel recoil/bob, pickups
├── enemies.rs         # Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell Baron AI, projectiles, hit-flash, death bursts
├── extraction.rs      # SOS Countdown state machine, alarm lighting, demon wave spawner, extraction beam
├── upgrades.rs        # Roguelike upgrade pool (13 upgrades), next-rank previews, stat modifiers, App Store 1-of-3 selection
├── hud.rs             # Dual-wield left-hand Phone diegetic UI + right-hand weapon overlay + Doom HUD + Dead-Zone glitch FX + menus
└── audio.rs           # Procedural retro synth SFX generator (gated under `audio` feature, no-op stub otherwise)
```

---

## 7. Implementation Plan (Orchestrated via Claude Work Opus 5.5)

- **Milestone 1 (Foundation & World):** Cargo project (`bevy 0.20.1`), procedural retro textures, 3D multi-level Mars bunker labyrinth with ramps/catwalks/atrium, Doom FPS controller & collision, Signal Field solver (Tower + Jammers + wall attenuation), basic HUD readout + unit tests.
- **Milestone 2 (Combat, Phone & Roguelike Extraction):** Dual-wield Phone & Gun viewmodels, Shotgun + Plasma Repeater, Ping Scan & 5G Overload apps, 4 Demon archetypes with AI & projectiles, SOS Countdown rush + alarm lighting, 10 Roguelike App Store upgrades, Dead-Zone glitch FX, menus/game-over/victory flow, procedural SFX, and comprehensive tests.
- **Milestone 3 (Polish & Verification):** Playability audit, game-feel tuning (hitstop, recoil, FOV kick on dash, screen flash, signal readability), full `cargo test` + `cargo build --release` check.
- **Milestone 6 (Upgrade Reworks & Signature Upgrades):** High-Capacity Cell narrowed (no fire rate), Sub-GHz floor route on Ping, Firewall regen only while un-jammed, clearer card text + live next-rank previews, and three blackout-themed signature upgrades (Store & Forward, Emergency Callback, Missed Call) with phone OLED indicators and headless ECS tests.
- **Milestone 7 (Outside-the-Round Meta-Progression & Pause Menu):** Persistent JSON profile (`src/meta.rs`), 4 Starter OS Chips, 15 upgrades (adding Signal Spoofer & Jammer Hijack) with 10-card active catalog curation, 4 stackable Carrier Contracts, 4 3D Handset Skins, 10 Feats, Jam Demo toggle, Loadout / Contracts & Feats / Pause UI (`src/meta_ui.rs`, `src/hud.rs`), and 155 passing tests.
- **Milestone 8 (Quake / CPMA Movement):** Bunny hopping and air-strafing in `src/player.rs`: Quake friction and accel, the CPMA strafe curve plus VQ3 strafe-jumping, CPMA air control, hold-Space zero-friction landing hops, jump buffer, coyote time, dash-to-bhop momentum carry and soft high-speed air drag. Also speed FOV, strafe roll, a bhop SFX, the HUD `VEL` gauge, a vertical-substep physics safety fix, and unit plus headless ECS tests.
