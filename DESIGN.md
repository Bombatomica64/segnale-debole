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

Each run descends through increasingly hostile Mars Bunker Sectors (**Floor 1 → Floor 5**, ending with THE CARRIER DAEMON boss), then optionally continues in **Endless Overdrive** (`E` on the victory screen; Carrier Daemon rematches on every 5th floor):

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
3. **Free-Roam Proximity Uplink (Milestone 32):** you still *initiate* the SOS at the tower (`E` inside the 9 m Signal Ring, or stand by the mast for `1.5 s`), but once it runs you can **bhop anywhere**: upload speed scales smoothly with flat distance to the tower (`proximity_upload_rate` in `src/extraction.rs`). Closer = faster, and the game never stops you for leaving.
   - **SOS OVERDRIVE `1.25x`** anywhere inside the Signal Ring (`<= 9 m`), which also fuels your **5G Apps** at 4–5 bars.
   - Smooth ease-out falloff `1.25 - 0.8 * t * (2 - t)`, `t = (d - 9) / 33`: about `1.03x` at 14 m (outer atrium), `0.81x` at 20 m (adjacent halls), `0.60x` at 28 m (mid-bunker), down to a **`0.45x` free-roam floor** at 42 m or more (far side of the bunker). Kiting a heavy swarm through the halls still uploads.
   - If a **Jammer Fiend** enters the Tower zone it jams the uplink (`0.0x`, wherever you are) until you blast it; on boss floors the Carrier Daemon still holds the upload at 99% until it dies.
   - **Frameless HUD readout:** `SOS OVERDRIVE  1.25x  42%` (acid green) in the ring, `UPLINKING  0.93x  42%` (cyan) at 0.80x or faster, `UPLINKING  0.61x  (CLOSER = FASTER)  42%` (warm amber) further out. It motivates and never scolds. The jam and boss-hold lines are unchanged.

### Phase 3: EXTRACT & DOWNLOAD ("App Store" Roguelike Upgrades)
1. At **100% SOS Uplink**, an orbital beam locks onto the tower, vaporizing remaining demons and extracting you to the next sector.
2. With full 5-bar reception during extraction, your phone opens the **FIRMWARE / APP DOWNLOAD** screen — choose **1 of 3 randomized Roguelike Upgrades** drawn from your active **12-card App Store Catalog**:
   Each card shows a live **NEXT:** line previewing exactly what the next rank changes (e.g. `8 -> 11 pellets`, `Min 0 -> 1 bar`, `4.0s -> 2.0s ping, +35% dmg`). 17 total upgrades (12 core + 5 feat-licensed signature apps); stackable ones (S) can be re-bought, toggles (T) are drafted again only when the pool runs dry.
   - **Dual-SIM Barrels (S):** Shotgun fires +3 pellets with a 25% tighter choke.
   - **5G Overclock (S):** At 4–5 bars, fire rate +35% and move speed +25%.
   - **External Copper Antenna (S):** +1.25 minimum signal bars everywhere (capped at 4.0); resists Dead Zones.
   - **Signal Siphon App (S):** While you have 3+ bars, demon kills restore +8 HP.
   - **Firewall Daemon (S):** +40 armor & max armor; regenerates 6/s while the SOS advances (**not jammed**, so a spared Jammer Fiend is not an armor farm). Since Milestone 32 the regen also works while kiting away from the tower, gently scaled by `min(rate, 1.0)`.
   - **Packet Sniffer (S):** Ping cooldown halved; ping-marked demons take +35% bonus damage.
   - **EMP Overcharge (T):** 5G Overload radius +40%, damage +35%, chains to 3 nearby demons.
   - **Airplane Mode Phase (T):** Dash grants invulnerability, a static shockwave, and -15% dash cooldown.
   - **High-Capacity Cell (S):** Ammo caps +35%; all pickups +30% fuller. (No fire-rate bonus: that is 5G Overclock's niche.)
   - **Sub-GHz Penetration (S):** Walls attenuate 55% less (multiplicative per rank); every Ping Scan also projects a pulsing cyan **floor route** — the next 14 cells of the shortest walkable BFS path to the Tower (5 s) — so you can follow it around bunker walls.
   - **Doppler Overdrive (S):** At 20+ m/s: +30% weapon damage and a 20% tighter shotgun choke (rewards bhop chains).
   - **Blast Thrusters (S):** Rocket jumps +25% boost, +20% blast radius, -50% rocket self-damage.
   - **Store & Forward (T, Signature):** Kills during a *natural* 0-bar blackout (measured before the Copper Antenna floor, so antenna owners still qualify) queue up to 3 packets, shown as glowing pips on the phone's status bar. Reaching 3+ bars discharges every packet as a 20-damage lightning arc at the nearest demons within 22 m (cycling over targets; ping marks apply).
   - **Emergency Callback (T, Signature):** Killing a ping-marked Jammer Fiend unlocks one any-signal 5G Overload for 6 s (cooldown still applies). The phone's `Q EMP` tile flashes gold **CALL!**.
   - **Missed Call (T, Signature):** Pressing Ping at 0 bars drops a ringing, vibrating decoy handset (shares the Ping cooldown, one active at a time) that pulls every Imp within 18 m toward it for 2.5 s. The `RMB PNG` tile reads amber **DECOY** when ready.
   - **Signal Spoofer (T, Signature):** Dashing inside a 0–1 bar Dead Zone spoofs 4 bars of signal for 2.5 s (`6 s` rearm cooldown; preserves natural blackout detection for Store & Forward; OLED status bar reads cyan **SPOOF**).
   - **Jammer Hijack (T, Signature):** Destroying a Demon Jammer or Jammer Fiend turns its position into a temporary 5-bar Signal Hotspot for 8 s (`12 m` radius with glowing emerald floor ring & beacon; OLED status bar reads emerald **HIJK**).

### Phase 4: Outside-the-Round Meta-Progression (`src/meta.rs`, `src/meta_ui.rs`)
Core principle: **persist player choices, unlocks, and opt-in challenges — reset raw combat power at run start** (no permanent `+1 bar` or raw damage boosts that erase Floor 1's 0-bar tension).
- **Persistent JSON Profile (`serde` + `serde_json`):** Saved atomically to `$XDG_DATA_HOME/segnale-debole/profile.json` (`SEGNALE_PROFILE` override; in-memory for headless tests/bots/screenshots). Incompatible or corrupt files are backed up to `.bak` with a recovery notice; failed writes stay marked dirty and retry automatically.
- **4 Starter Phone OS Chips (Balanced Sidegrades):** *Stock UAC OS* (neutral), *Survey OS* (`-30%` Ping CD & `+25%` wall penetration, `-15` max HP), *Overclock Radio* (`+20%` fire & `+15%` move at 4–5 bars, `-25%` pickup yield), *Faraday Case* (`+25` starting armor & `-25%` damage taken in Dead Zones including hazards, `+25%` Ping/EMP cooldowns).
- **12-Card Active App Store Catalog:** Curate 12 unlocked apps before each run (missing slots display `AUTO-FILL` and pad deterministically on deploy) so unlocking all 5 signature apps keeps 4-draft run odds sharp.
- **4 Stackable Carrier Contracts:** *Roaming Charges* (`+30%` Jammer radius & `+20%` harsher wall loss), *Priority Jamming* (extra Jammer Fiend crashes every SOS + higher Fiend weight), *Overtime Uplink* (`+35%` SOS duration & `20%` faster rush waves), *Low-Battery Mode* (no between-floor HP heal).
- **4 3D In-Hand Handset Skins:** *UAC Standard Orange*, *Mars Hazard Yellow*, *Stealth Carbon*, and *5G Chrome Gold* — styling the live left-hand 3D smartphone viewmodel.
- **10 Feats & Jam Demo Mode:** All unlockable chips, signature apps, and skins tie transparently to 10 Feats; a one-click **JAM DEMO UNLOCK ALL** toggle lets judges/testers try everything immediately (exhibition runs — like `F9` debug skips — never pollute permanent feats or lifetime stats).
- **Pause Menu (`Esc` / `P`):** Freezes gameplay over the live 3D viewport with Resume, Restart Run, Loadout, Settings, and Main Menu.
- **Persistent Settings Menu (`O`, from Main Menu or Pause):** Mouse sensitivity, base FOV, speed-FOV and strafe-roll toggles/scales, master volume + mute, crosshair style & color — saved in the profile; opening it mid-run keeps the run alive.
- **Endless Overdrive Mode:** After the Floor 5 victory, `E` keeps the run going on Floor 6+ (sector themes cycle; THE CARRIER DAEMON returns on floors 10, 15, 20…).
- **Movement Records:** Per-run **Peak Speed** and **Max Bhop Chain** feed lifetime records shown in the run summary and Contracts & Feats screen.

---

## 3. Dual-Wield Mechanics: Right Hand Gun + Left Hand Phone

### Right Hand — Classic Boomer Shooter Arsenal
- **1 — Mars Pump Shotgun (Default):** Punchy close-to-mid range 8-pellet spread, heavy recoil kick, muzzle light flash, chunky knockback.
- **2 — UAC Plasma Repeater:** Fast-firing glowing cyan energy bolts (true 3D projectiles) ideal for Cacodemons on upper catwalks and shredding Jammers at range.
- **3 — Signal-Mortar Rocket Launcher:** Slow (0.85 s) 34 m/s rockets — 115 direct + 95 splash damage in a 5.5 m radius. Fire at your feet mid-jump for a **Quake rocket jump** (strong vertical + horizontal kick, only 15% self-damage), chaining into bhops and jump pads.
- *(Switch with `1`/`2`/`3`, `Tab` or Mouse Wheel; unlimited backup recharge on low-ammo or generous demon drops so combat never stalls.)*

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
- **Frameless HUD:** no plates or backing slabs anywhere in the in-game HUD (`src/hud.rs` + the `src/hud_fx.rs` kit). Text floats on hard ink shadows, each gauge keeps a dark well under its fill, and every cluster hangs off one razor-thin glowing accent: a mode / state colored rail on the top-left floor strip, an underline under the status line (steady while searching, a smooth swell on a signal lock, a sharp flicker in a dead zone, a beat during the rush), a rule under the split-timer numerals (top-right corner, callouts flowing under it) and a right-edge pip on each killfeed callout. Short decaying envelopes pop whatever just changed (a counter, the status line, a split, the ammo count on fire / swap); critical HP and a dry magazine throb red.
- **HUD `VEL` row** (the first line of the frameless vitals cluster, above HP and the ARMOR | STAMINA pair): `VEL` label, a framed 16-segment tachometer (2 m/s per segment, 0–32 m/s) with fixed zones — ash 0–12, arc-cyan 12–20, acid-green 20–28, hazard-yellow 28–32 — split by 4 px zone breaks, lit segments at full zone color and unlit ones at a faint 16% tint, and a constant-width integer readout (`18 m/s`). The track brackets and readout take the current tier color with 0.5 m/s hysteresis (no flicker at 12 / 20 / 28). At 32+ m/s the readout snaps to a solid hazard-yellow badge with black text (steady, no strobing). A solid stamped `BHOP xN` tag floats above the row from a 2-hop chain.
- **Frame-rate independence:** every frame is split into uniform movement substeps of at most 1/120 s (timers, dash, jump, air-strafe and collision all run per substep, with the frame's mouse turn spread across them), so 30 / 60 / 120 / 144 Hz air-strafe chains end within 1 m/s of each other. Landing and re-hopping can happen inside one frame.
- **Physics safety:** collision substeps bound horizontal travel to half the player radius and vertical travel to half a step per substep, so 48 m/s hops never tunnel walls, pillars or ledges, and fast falls at low frame rates can't skip a catwalk top. A 9-point footprint (center + 8 points at 0.85 × radius) makes diagonal catwalk grazes support you like face-on contact; a touchdown anywhere inside a step (even crossing a catwalk edge at 30 m/s) keeps the coyote window; only a grounded player is glued down small drops (an airborne one is never snapped onto a catwalk early); and inside corners clip velocity per wall (Quake crease rule) instead of bouncing you backwards.
- **Aim:** shots use this frame's eye pose (body + camera transforms after movement and mouse look), so moving, turning and firing on the same frame hits where you see.

---

## 4. Enemies (7 Distinct Silhouettes & Roles)

All enemies use bold, readable procedural 3D retro-stylized meshes / billboarded pixel-art sprites with emissive eyes, distinct colors, hit-flash feedback, and death bursts:
1. **Static Imp (Rust-Red):** Fast corridor skirmisher; hurls glowing orange fireballs; moves 30% faster and glows static-red when in 0–1 bar Dead Zones.
2. **Dead-Zone Cacodemon (Crimson/Violet Floating Orb):** Ignores floor geometry, hovers over ramps/catwalks, fires twin plasma orbs, forces vertical aim.
3. **Jammer Fiend (Sickly Green/Black Armored Cyber-Demon):** Carries a crackling demonic router/antenna on its back — projects a **mobile Dead Zone** (`-2.5 bars` radius) around itself! Priority target during both exploration and SOS defense.
4. **Hell Baron (Towering Molten-Gold/Red Heavy):** High-HP boss-tier bruiser that spawns during the SOS Countdown, throwing triple-spread hellfire volleys and charging the Tower pad.
5. **Phase Stalker (Floor 2+ flanker):** Blinks sideways through 0–1 bar Dead Zones with a glitchy shimmer to flank you.
6. **Catwalk Relay Sniper (Floor 2+ sentinel):** Perches on catwalks and paints a telegraphed laser lock-on before a rail shot — break line of sight or kill it first.
7. **THE CARRIER DAEMON (Floor 5 / every 5th Endless floor boss):** Lands in the Signal Atrium when the SOS starts and parks the uplink at 99% until it dies. Phase machine `Phase1 → Shielded1 → Phase2 → Shielded2 → Overdrive`: each Signal Shield is held up by two tethered, destructible pylons; shattering it stuns the boss and drops supplies.

---

## 5. Procedural 3D Mars Bunker Level Design (With True Verticality)

Grid-based room-and-corridor labyrinth (`21x21` to `29x29` cells, cell size `4.0m`) compiled into rich 3D Bevy geometry:
- **Multi-Elevation Zones:**
  - Ground corridors (`y = 0.0`)
  - Raised catwalks, observation decks, and Tower pedestals (`y = 2.4`) connected by walkable **3D sloped ramps** (`step_height` + smooth slope climbing in player physics!)
  - Hazardous lava/sludge pits or recessed server trenches (`y = -1.2`) with glowing red underlighting
  - **Open-roof Signal Atrium:** Tall walls opening to a dark crimson Martian sky with an iconic **3D Radio/Relay Signal Tower** and pulsing beacon light, reached by **twin atrium ramps** for high-speed loops.
- **High-Speed Arena Props:**
  - **Signal-Gated Jump Pads:** Launch you up to catwalks only while powered (2+ bars at the pad); a Jammer nearby drains them dormant (red light).
  - **Explosive Server-Rack Barrels:** Shootable racks that detonate with splash damage — chain them into demon packs.
- **Lived-in Mars Bunker Furniture (Milestone 32, `src/architecture.rs`):** 8 wall-flush archetypes (PC station + swivel chair, 19-inch wall rack, NO SIGNAL box, dual-monitor Ops Console, UAC vending machine, 4-tier supply shelf, triple locker bank + bench rail, signal-research lab bench). Each one stays `<= 0.55 m` off its wall on a straight wall with `>= 1` cell clear of doorways and corners, `>= 2` cells apart, up to `22 + 4 * floor` pieces per floor (limited by available slots). Corner equipment nooks (crates, cable reel, diagnostic beacon) sit `<= 0.72 m` deep in dead 90° corners. Overhead HVAC ducts and cable trays hang at `5.6–6.8 m` in every closed-roof room and corridor (never the open-roof Atrium). Everything is baked into the static per-material batches: zero new materials, no collision, and the 8 m lanes stay fully open. Interactive CRTs and UPS racks are planned first, so their budgets and the chain-reaction pair are always met.
- **5 Sector Visual Themes:** Floors 1–5 map to *Rust Bunker*, *Toxic Coolant*, *Cryo Relay*, *Magma Foundry* and *Obsidian Core* (sky, fog, lamps, wall/floor tints, hazard-pit glow); Endless floors cycle through them.
- **100% Procedural Pixel-Art Textures (`64x64`, `ImageSampler::nearest()`):**
  - Riveted steel bunker walls with warning stripes & grime
  - Grated metal catwalk floors & Mars regolith rock
  - Glowing crimson/amber tech panels & lava pools
  - Demonic flesh-wire textures for Jammers

---

## 6. Technical Architecture (Rust + Bevy `0.20.1`)

### Platform & Build Compatibility Notes
- **Bevy version:** `0.20.1`
- **Platform-minimal Bevy features:** `Cargo.toml` disables `default-features` and enables only the Bevy 0.20 sub-features actually used (`default_app`, `std`, `bevy_winit`, `default_font`, `bevy_camera`, `bevy_color`, `bevy_image`, `bevy_mesh`, `bevy_shader`, `bevy_material`, `bevy_text`, `bevy_window`, `bevy_light`, `bevy_render`, `bevy_core_pipeline`, `bevy_pbr`, `bevy_ui`, `bevy_ui_render`, `tonemapping_luts`, `zstd_rust`), omitting unused crates (`bevy_gltf`, `bevy_animation`, `morph`, `bevy_gizmos`, `bevy_anti_alias`/`smaa_luts`, `hdr`, `bevy_post_process`, `bevy_mikktspace`, `bevy_curve`, `bevy_gilrs`). Platform-specific features stay strictly gated: `multi_threaded` + `custom_cursor` + `png` on `cfg(not(target_arch = "wasm32"))`, `x11` + `wayland` on `cfg(target_os = "linux")`, `webgl2` + `web` on `cfg(target_arch = "wasm32")`, and `bevy_audio` behind the optional `audio` Cargo feature (`audio = ["bevy/bevy_audio", "bevy/wav"]`).
- **Optional ray-traced lighting (`raytracing` feature → `bevy/bevy_solari`):** `src/raytracing.rs` adds Bevy Solari when a renderer exists and `SEGNALE_RT` isn't `off`. It activates only if the GPU exposes ray-query + binding-array features. Under RT, the player camera gets `SolariLighting { restir: true }`, HDR, `Msaa::Off`, a storage main texture, unit exposure and a solid-color sky environment map. The sun and camera ambient are scaled by the raster exposure (≈1/1000) so lit surfaces keep their brightness. World point/spot lights move to an undrawn render layer, since Solari traces only the sun and emissive meshes. Opaque world `StandardMaterial` meshes get `RaytracingMesh3d` while visible. Their meshes are converted in place to Solari's layout (u32 indices, zero UV/tangent fill), and their materials switch to the deferred path. The default renderer method stays forward, so the viewmodel overlay (no deferred prepass) and translucent FX render as before.
- **Custom Lightweight 3D FPS Physics:** Fast, deterministic swept AABB / cylinder-vs-grid+ramp collision (`src/physics.rs`) — zero external physics crate version conflicts, silky-smooth Doom wall-sliding, ramp climbing, jumping, and instant raycasts for hitscan weapons & signal wall-attenuation!

- **Performance (Milestone 9):** Static level geometry and demon parts are merged into batched meshes (`src/mesh_batch.rs`), MSAA picks 2× only when the adapter supports it (else off), and `F3` (frame-time overlay) / `F4` (VSync) are live perf controls (`src/perf.rs`).
- **CI / Releases:** `.github/workflows/ci.yml` (test + clippy + `--features audio` check on Ubuntu, plus a `wasm32-unknown-unknown` clippy job) and `.github/workflows/release.yml` (Linux x86_64 / Windows x86_64 / macOS aarch64 `--release --features audio` archives and the `segnale-debole-web-itch.zip` WebGL2 build, published to a GitHub Release on `v*` tags).

### Module Layout (`src/`)
```
src/
├── main.rs            # Window config (native window / web `#bevy` canvas) + GamePlugin
├── lib.rs             # CoreGameplayPlugin, GamePlugin, headless test app builder
├── state.rs           # AppState (MainMenu, Playing, Paused, UpgradeSelect, GameOver, Victory, Loadout, ContractsFeats, Settings), RunStats, UpgradeId
├── assets_gen.rs      # Procedural 64x64 nearest-neighbor retro textures & shared Materials/Meshes
├── level.rs           # Procedural 3D Mars Bunker labyrinth generator (rooms, corridors, ramps, catwalks, atrium, tower, jammers), light culling + WebGL2 light cap
├── architecture.rs    # Sculpted trim, overhead trusses/conduits, catwalk girders and sector props baked into the level batches
├── sculpt.rs          # Procedural sculpted meshes for demons, boss, pylons and guns
├── decals.rs          # Pooled blood / ichor / scorch / plasma-burn floor & wall decals
├── trials.rs          # Daily Seeded Run (UTC date -> seed + loadout), split timer, movement Time Trial course
├── physics.rs         # Fast 3D FPS collision (walls, multi-elevation floors, walkable ramps, line-of-sight raycast)
├── player.rs          # Quake/CPMA FPS controller (friction, air-strafe, air control, bhop, coyote/buffer), mouse look, Airplane Mode Dash, health/armor/ammo
├── signal.rs          # Continuous 0..5 bar signal solver (distance, wall occlusion, Jammers, Dead Zones), Ping Scan pulse, 5G Overload
├── weapons.rs         # Pump Shotgun (hitscan pellets) + Plasma Repeater (3D projectiles), viewmodel recoil/bob, pickups
├── enemies.rs         # Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell Baron AI, projectiles, hit-flash, death bursts
├── extraction.rs      # SOS Countdown state machine, alarm lighting, demon wave spawner, extraction beam
├── upgrades.rs        # Roguelike upgrade pool (17 upgrades), next-rank previews, stat modifiers, App Store 1-of-3 selection
├── apps.rs            # Phone Apps + signature app effects (decoys, spoofing, hotspots, routes)
├── boss.rs            # THE CARRIER DAEMON phase machine, Signal Shield pylons, boss HUD snapshot
├── meta.rs / meta_ui.rs  # Persistent JSON profile (disk on native, localStorage on web), loadout, contracts, feats, settings + their menus
├── hud.rs / ui_kit.rs / phone_screen.rs  # Doom HUD, menus, procedural UI kit, live phone OLED texture
├── fx.rs              # Bursts, flashes, HUD feed toasts
├── mesh_batch.rs / perf.rs  # Static/demon mesh batching, MSAA selection, F3/F4 perf controls
├── playtest_bot.rs / bot_input.rs / dev.rs  # Autonomous playtest bot, virtual input, screenshot tours
└── audio.rs           # Pure-Rust synth SFX + 3-layer adaptive soundtrack (always compiled & tested); playback behind `audio`
```

---

## 7. Adaptive 3-Layer Industrial-Synth Soundtrack (`src/audio.rs`)

All music is synthesized at startup in pure Rust (no audio files), deterministic, and compiled + unit-tested even without `--features audio`; only playback is feature-gated.
- **Three equal-length looping stems** (`MusicLayer`), each exactly 4 bars at **132 BPM** (64 sixteenth-steps × 2,505 samples = 160,320 samples ≈ 7.27 s at 22,050 Hz), spawned together at startup and crossfaded in parallel:
  - **Dead-Zone Drone:** E1 (41.2 Hz) sub-bass + octave, a detuned-fifth carrier FM-modulated by the tritone, slow loop-synced swells, and intermittent low-passed RF static crackles.
  - **Signal Pulse:** Driving 16th-note E-Phrygian saw/square bass (per-bar roots E–E–F–D), metallic clock-tick hats, and E6 radar pings with 2×/3× overtones.
  - **Rush Metal:** `tanh`-distorted saw/square power-chord riff in E Phrygian, pitch-dropping kick, noise snare on 2 & 4, driving 16th hats and octave/fifth lead stabs.
- **Seamless loops:** each stem renders 20 ms past the loop end and crossfades that tail over the start, so the sample after the last one is the true continuation (no click); peaks are normalized to `0.85` (no clipping).
- **Adaptive mix (`MusicMixState`):** `target_music_weights(state, bars, uploading, boss_active)` picks `[drone, pulse, rush]` targets — menus `[0.45, 0.15, 0]`, pause `[0.25, 0, 0]`, App Store / victory `[0.15, 0.55, 0]`, game over `[0.5, 0, 0]`; while playing: 0–1 bars `[0.95, 0.05, 0]`, 2–3 bars `[0.45, 0.75, 0]`, 4–5 bars `[0.2, 0.95, 0.25]`, SOS upload or boss `[0.15, 0.35, 1.0]`. `update_music_mix` eases toward the target every frame with a frame-rate-independent exponential crossfade (rate 3/s).
- **Playback (`--features audio`):** three looping `AudioPlayer` stems start together at volume 0; every frame each `AudioSink` gets `weight × 0.32 × master volume` (0 when muted) from the Settings menu.

---

## 8. Web / itch.io Build (WebGL2 WASM)

Shipped in Milestone 19. The browser build is the same game compiled to `wasm32-unknown-unknown` and rendered with WebGL2; `trunk` handles `wasm-bindgen`, `wasm-opt` and the `dist/` bundle.

**Build:** `trunk serve --features audio` (dev) / `trunk build --release --features audio` (release into the gitignored `dist/`). CI and releases add `--cargo-profile wasm-release` (fat LTO, `opt-level = "s"`) to shrink the download. `index.html` passes rustc's default wasm32 features (bulk memory, sign-ext, ...) to `wasm-opt -Oz`. `Trunk.toml` sets `public_url = "./"` so asset URLs stay relative inside itch.io's iframe.

**Why it ports cleanly:**
- **All content is procedural, in-memory Rust:** textures, meshes, UI kit, phone OLED texture and every WAV are generated at startup, so there are no asset files to fetch.
- **Physics, signal solver, AI and boss logic** are plain Rust with no threads or OS APIs; `multi_threaded`, `custom_cursor` and `png` stay gated to native targets (`cfg(not(target_arch = "wasm32"))`).
- **Audio:** `bevy_audio` pulls `rodio` with `wasm-bindgen` (WebAudio). `index.html` resumes every `AudioContext` on the first click or key press, because browsers start audio suspended.

**Platform layer (all `cfg(target_arch = "wasm32")`; native code paths unchanged):**
- **Cargo:** `bevy` adds `webgl2` + `web`; `getrandom 0.3` (via `rand 0.9`) uses `wasm_js`, with `--cfg getrandom_backend="wasm_js"` in `.cargo/config.toml`; `js-sys` + `web-sys` (`Window`, `Storage`, `Document`, `Element`).
- **Window (`main.rs`):** `canvas: "#bevy"`, `fit_canvas_to_parent`, `prevent_default_event_handling` (the right-click Ping Scan and the F-keys reach the game), `PresentMode::AutoVsync`.
- **Daily date (`trials.rs`):** `DailyDate::today_utc` reads `js_sys::Date` (`getUTCFullYear/Month/Date`), so web and native players share the same Daily Seeded Run.
- **Persistence (`meta.rs`):** `ProfileStore::open_web` loads and saves through `ProfileKv`, a small key/value trait. On wasm32 it is backed by `localStorage` under `segnale_debole.profile.v1`. Corrupt or incompatible JSON is copied to `segnale_debole.profile.v1.bak` (then `.bak.1`, ...), mirroring the native `.bak` policy. Blocked storage falls back to an in-memory profile with a notice. The same JSON format is used as the native `profile.json`, and `load_profile_kv` / `save_profile_kv` are unit-tested natively against a map.
- **Rendering (`perf.rs`, `level.rs`):** `platform_msaa` forces `Msaa::Off`, avoiding multi-camera MSAA resolve hazards and fill-rate cost on integrated GPUs. WebGL2 has no storage buffers, so clustered forward packs lights into small uniform buffers, and the world point-light budget is `WEB_POINT_LIGHT_CAP = 12`: `cull_world_lights` keeps only the nearest 12 lit `CullableLight`s (`light_cap_cutoff`). Native is uncapped (`point_light_cap(false) == None`). `F4` shows `VSYNC MANAGED BY BROWSER` instead of toggling `AutoNoVsync`.
- **Pointer Lock (`player.rs`):** browsers handle `Esc` themselves (exit Pointer Lock) and usually never deliver the key, so `sync_browser_pointer_lock` polls `document.pointerLockElement` every frame. It pauses when a lock we actually held disappears while `Playing` (`browser_unlock_should_pause`). A lock that was never granted (pending request, or the browser's re-lock cooldown after `Esc`) never pauses; a click re-requests it, and mouse look waits for the real lock (`mouse_look_active`). Our own `OnExit(Playing)` release is never seen from `Playing`, and a state transition already queued that frame wins over the pause.
- **Dev tooling:** the playtest bot uses `bevy::platform::time::Instant` (web-safe). `SEGNALE_*` lookups via `std::env::var` return `Err` on wasm32, so the bot, screenshot tours and `std::fs` paths are never reached in the browser.

**Browser caveats:**
- Expect lower frame rates than native on integrated GPUs, and a multi-second first-load shader compile.
- Chrome (and possibly other browsers) briefly refuses to re-lock the pointer right after `Esc`, so resuming may need one extra click to re-capture.
- itch.io needs the "played in the browser" option, and fullscreen is via itch's button.

---

## 9. Implementation Plan (Orchestrated via Claude Work Opus 5.5)

- **Milestone 1 (Foundation & World):** Cargo project (`bevy 0.20.1`), procedural retro textures, 3D multi-level Mars bunker labyrinth with ramps/catwalks/atrium, Doom FPS controller & collision, Signal Field solver (Tower + Jammers + wall attenuation), basic HUD readout + unit tests.
- **Milestone 2 (Combat, Phone & Roguelike Extraction):** Dual-wield Phone & Gun viewmodels, Shotgun + Plasma Repeater, Ping Scan & 5G Overload apps, 4 Demon archetypes with AI & projectiles, SOS Countdown rush + alarm lighting, 10 Roguelike App Store upgrades, Dead-Zone glitch FX, menus/game-over/victory flow, procedural SFX, and comprehensive tests.
- **Milestone 3 (Polish & Verification):** Playability audit, game-feel tuning (hitstop, recoil, FOV kick on dash, screen flash, signal readability), full `cargo test` + `cargo build --release` check.
- **Milestone 6 (Upgrade Reworks & Signature Upgrades):** High-Capacity Cell narrowed (no fire rate), Sub-GHz floor route on Ping, Firewall regen only while un-jammed, clearer card text + live next-rank previews, and three blackout-themed signature upgrades (Store & Forward, Emergency Callback, Missed Call) with phone OLED indicators and headless ECS tests.
- **Milestone 7 (Outside-the-Round Meta-Progression & Pause Menu):** Persistent JSON profile (`src/meta.rs`), 4 Starter OS Chips, 15 upgrades (adding Signal Spoofer & Jammer Hijack) with 10-card active catalog curation, 4 stackable Carrier Contracts, 4 3D Handset Skins, 10 Feats, Jam Demo toggle, Loadout / Contracts & Feats / Pause UI (`src/meta_ui.rs`, `src/hud.rs`), and 155 passing tests.
- **Milestone 8 (Quake / CPMA Movement):** Bunny hopping and air-strafing in `src/player.rs`: Quake friction and accel, the CPMA strafe curve plus VQ3 strafe-jumping, CPMA air control, hold-Space zero-friction landing hops, jump buffer, coyote time, dash-to-bhop momentum carry and soft high-speed air drag. Also speed FOV, strafe roll, a bhop SFX, the HUD `VEL` gauge, a vertical-substep physics safety fix, and unit plus headless ECS tests.
- **Milestone 9 (Performance):** 90–120 FPS target via static-level and demon mesh batching, adaptive MSAA, `F3` perf overlay and `F4` VSync toggle.
- **Milestone 10 (Rocket Launcher & Movement Synergy):** Signal-Mortar Rocket Launcher (`3`) with Quake rocket jumping, plus Doppler Overdrive and Blast Thrusters (17 upgrades, 12-card catalog).
- **Milestone 11 (High-Speed Bunker Arenas):** Signal-Gated Jump Pads, Explosive Server-Rack Barrels, twin atrium ramps, and 5 sector visual themes.
- **Milestone 12 (Settings, Endless & Records):** Persistent Settings menu (`O`), Endless Overdrive mode (`E` after Floor 5), Peak Speed / Max Bhop Chain records, playtest-bot pit recovery.
- **Milestone 13 (New Demons & Boss):** Phase Stalker, Catwalk Relay Sniper, and THE CARRIER DAEMON shield-pylon boss on Floor 5 and every 5th Endless floor.
- **Milestone 14 (Soundtrack, CI/Release & Web Feasibility):** 3-layer adaptive industrial-synth soundtrack (Dead-Zone Drone / Signal Pulse / Rush Metal) with live ECS mixing, GitHub Actions CI and cross-platform release packaging, and the WASM / itch.io feasibility study (310 passing tests).
- **Milestone 15 (3D Models Overhaul):** procedurally sculpted meshes (`src/sculpt.rs`) for all 7 enemies, the Carrier Daemon and its pylons, and all 3 guns.
- **Milestone 16 (World Architecture & Sector Props):** sculpted trim, overhead trusses and conduits, catwalk girders, flush wall-mounted sector props and animated hazards (`src/architecture.rs`), baked into the static level batches.
- **Milestone 17 (Enemy Animation, Decals & Gibs):** procedural enemy attack animations, directional hit-stagger, pooled combat decals (`src/decals.rs`) and gibs.
- **Milestone 18 (Daily Runs, Splits & Time Trials):** Daily Seeded Runs (UTC date -> seed + loadout, per-date records), the speedrun split timer with PB deltas, and the movement Time Trial course with checkpoints and instant `R` reset (`src/trials.rs`; 374 passing tests).
- **Milestone 19 (Web / itch.io):** the WebGL2 WASM build described in section 8: `trunk` + `index.html` + `Trunk.toml`, browser UTC clock, `localStorage` profile, a WebGL2 light cap and MSAA off, pointer-lock pause, a wasm32 clippy CI job and the `segnale-debole-web-itch.zip` release asset (379 passing tests).
- **Milestone 32 (Free-Roam SOS Uplink & Richer Bunker Furniture):** proximity-scaled SOS uplink (`1.25x` OVERDRIVE in the ring, easing out to a `0.45x` free-roam floor at 42 m) with live-speed HUD copy, Firewall regen while kiting, 8 wall-flush furniture archetypes at roughly double the density, corner equipment nooks, and overhead HVAC ducts and cable trays. Interactive props are planned before static furniture, so their budgets always hold.
