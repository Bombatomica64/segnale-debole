//! Layer 1: headless multi-frame ECS integration tests.
//!
//! Every test builds the real gameplay plugins in a window-less Bevy `App`
//! ([`segnale_debole::build_headless_test_app`]) stepping a fixed 1/60 s per
//! `update()`, and drives the game only through [`BotControl`] virtual input
//! (move / look / fire / switch / ping / overload / interact / pick upgrade),
//! so player physics, weapons, phone apps, demon AI, signal solving, SOS
//! extraction and the App Store all run through their production systems.
//!
//! Test-side setup is limited to staging (teleporting the player, spawning or
//! clearing demons, granting god mode where survival is not under test).

use bevy::ecs::system::RunSystemOnce;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::mouse::MouseMotion;
use bevy::input::ButtonState;
use bevy::prelude::*;

use segnale_debole::apps::{
    tower_route, DecoyHandset, HijackedHotspot, PhoneApps, RouteBreadcrumb, CALLBACK_SECS, HIJACK_SECS, PING_COOLDOWN, ROUTE_MAX_STEPS,
    SIGNAL_SPOOF_SECS, STORE_FORWARD_DAMAGE,
};
use segnale_debole::bot_input::BotControl;
use segnale_debole::playtest_bot::{PlaytestBot, PlaytestBotPlugin, BOT_TIMEOUT_SECS};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin};
use segnale_debole::enemies::{
    garrison_size, spawn_enemy, spawn_enemy_projectile, spawn_pos_for, Enemy, EnemyAssets, EnemyKind, EnemyProjectile, PingMarked,
    ENRAGE_SPEED_MULT,
};
use segnale_debole::boss::{BossHud, BossPhase, BossPylon, BossState, BOSS_ARRIVAL_TEXT, BOSS_DEATH_TEXT, BOSS_SHIELD_STUN};
use segnale_debole::enemies::{damage_enemy, pick_phase_blink_target, SniperLaser, SNIPER_LOCK_SECS};
use segnale_debole::extraction::{rush_cap, upload_duration, ExtractionState, BOSS_HOLD_PROGRESS};
use segnale_debole::fx::{BurstColor, FxAssets, HudFeed};
use segnale_debole::hud::{HudCrosshair, HudPlugin, ENDLESS_CTA};
use segnale_debole::meta_ui::{
    commit_meta_edit, commit_settings_edit, EditOutcome, MetaCard, MetaTarget, ReticlePreview, SettingKnob, SettingToggle, SettingsControl,
    SettingsTarget, NO_UNLOCKS_LINE,
};
use segnale_debole::meta::{
    load_profile, start_run_with_loadout, CarrierContract, CrosshairColor, CrosshairStyle, FeatId, HandsetSkin, PlayerProfile, ProfileStore,
    RunLoadout, SkinSlot, StarterChip, UserSettings,
};
use segnale_debole::phone_screen::{PhoneScreenTexture, SCREEN_W, SPOOF_CYAN};
use segnale_debole::extraction::SignalRingFx;
use segnale_debole::level::{jammer_count_for_floor, Cell, DemonJammer, LevelBatch, LevelGrid, SignalTower, TowerBeacon};
use segnale_debole::perf::{PerfOverlay, PerfOverlayText, PerfSettings};
use segnale_debole::physics::has_line_of_sight;
use segnale_debole::player::{hazard_damage, CursorCaptured, Player, PlayerCamera, BASE_FOV_DEG, DASH_SPEED, MOUSE_SENSITIVITY, MOVE_SPEED, PLAYER_EYE_HEIGHT};
use segnale_debole::signal::{bars_from_strength, SignalState, TOWER_RING_RADIUS};
use segnale_debole::state::{complete_floor, is_boss_floor, start_run, AppState, PlayerModifiers, RunStats, SettingsReturn, UpgradeId, FINAL_FLOOR};
use segnale_debole::upgrades::{apply_upgrade, UpgradeChoices};
use segnale_debole::weapons::{Pickup, PhoneSkin, PhoneSkinPart, PlayerProjectile, Weapon, WeaponState, PLASMA_DAMAGE};

const FPS: usize = 60;

// ---------------------------------------------------------------------------
// Harness helpers
// ---------------------------------------------------------------------------

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn app_state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn bot(app: &mut App) -> Mut<'_, BotControl> {
    app.world_mut().resource_mut::<BotControl>()
}

fn grid(app: &App) -> LevelGrid {
    app.world().resource::<LevelGrid>().clone()
}

fn extraction(app: &App) -> ExtractionState {
    app.world().resource::<ExtractionState>().clone()
}

fn signal(app: &App) -> SignalState {
    app.world().resource::<SignalState>().clone()
}

fn run_stats(app: &App) -> RunStats {
    app.world().resource::<RunStats>().clone()
}

fn player(app: &mut App) -> Player {
    let mut q = app.world_mut().query::<&Player>();
    q.single(app.world()).expect("exactly one player").clone()
}

fn player_pos(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).expect("exactly one player").translation
}

fn with_player(app: &mut App, f: impl FnOnce(&mut Player)) {
    let mut q = app.world_mut().query::<&mut Player>();
    let mut p = q.single_mut(app.world_mut()).expect("exactly one player");
    f(&mut p);
}

fn player_feet(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

fn count<C: Component>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<(), With<C>>();
    q.iter(app.world()).count()
}

/// Same path as the main menu "START": new seed, neutral mods, `Playing`.
fn begin_run(app: &mut App, seed: u64) {
    app.world_mut()
        .run_system_once(move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, seed);
        })
        .unwrap();
    step(app, 3);
    assert_eq!(app_state(app), AppState::Playing);
}

/// God mode: for tests where player survival is not what is being verified.
fn godmode(app: &mut App) {
    with_player(app, |p| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
}

/// Place the player (feet) and point the view at `look_at`.
fn teleport(app: &mut App, feet: Vec3, look_at: Vec3) {
    {
        let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
        let (mut p, mut tf) = q.single_mut(app.world_mut()).unwrap();
        tf.translation = feet;
        p.velocity = Vec3::ZERO;
    }
    aim_at(app, look_at);
}

/// Aim the camera (eye) at a world point through the bot's look input.
fn aim_at(app: &mut App, target: Vec3) {
    let eye = player_feet(app) + Vec3::Y * PLAYER_EYE_HEIGHT;
    let d = target - eye;
    let yaw = segnale_debole::level::yaw_facing(Vec2::new(d.x, d.z));
    let pitch = d.y.atan2(Vec2::new(d.x, d.z).length());
    let mut b = bot(app);
    b.look_yaw = Some(yaw);
    b.look_pitch = Some(pitch);
}

/// A walkable cell on the tower pedestal next to the mast (inside the ring).
fn ring_cell(g: &LevelGrid) -> IVec2 {
    [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .map(|o| g.tower + o)
        .find(|c| matches!(g.cell(*c), Cell::Floor { .. }) && g.is_reachable(g.spawn, *c))
        .expect("pedestal cell next to the tower")
}

fn despawn_all_enemies(app: &mut App) {
    let mut q = app.world_mut().query_filtered::<Entity, With<Enemy>>();
    let ids: Vec<Entity> = q.iter(app.world()).collect();
    for e in ids {
        app.world_mut().despawn(e);
    }
}

fn spawn_demon(app: &mut App, kind: EnemyKind, pos: Vec3) -> Entity {
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>, mut mats: ResMut<Assets<StandardMaterial>>, run: Res<RunStats>| {
            spawn_enemy(&mut commands, &ea, &mut mats, kind, pos, run.floor, false)
        })
        .unwrap()
}

fn enemy(app: &App, e: Entity) -> Option<Enemy> {
    app.world().get::<Enemy>(e).cloned()
}

fn enemy_center(app: &App, e: Entity) -> Vec3 {
    let root = app.world().get::<Transform>(e).unwrap().translation;
    app.world().get::<Enemy>(e).unwrap().center(root)
}

/// Kill every living demon of `kind` except `keep` (scenario isolation).
fn cull_kind(app: &mut App, kind: EnemyKind, keep: Option<Entity>) {
    let mut q = app.world_mut().query::<(Entity, &mut Enemy)>();
    for (e, mut en) in q.iter_mut(app.world_mut()) {
        if en.kind == kind && Some(e) != keep && en.is_alive() {
            en.hp = 0.0;
        }
    }
}

fn install(app: &mut App, id: UpgradeId) {
    app.world_mut()
        .run_system_once(move |mut mods: ResMut<PlayerModifiers>, mut run: ResMut<RunStats>, mut q: Query<&mut Player>| {
            let mut p = q.single_mut().unwrap();
            apply_upgrade(id, &mut mods, &mut p, &mut run);
        })
        .unwrap();
}

/// Stand on the pedestal inside the Signal Ring and press `E` (SOS uplink).
fn trigger_uplink(app: &mut App) {
    let g = grid(app);
    let cell = ring_cell(&g);
    teleport(app, g.cell_center(cell), g.tower_world() + Vec3::Y * 2.0);
    step(app, 3);
    let s = signal(app);
    assert!(s.near_tower, "ring cell must be inside the Signal Ring");
    bot(app).interact = true;
    step(app, 2);
    assert!(matches!(extraction(app), ExtractionState::Uploading { .. }), "E inside the ring starts the uplink: {:?}", extraction(app));
}

/// The living Carrier Daemon, if any.
fn carrier(app: &mut App) -> Option<Entity> {
    let mut q = app.world_mut().query::<(Entity, &Enemy)>();
    q.iter(app.world()).find(|(_, e)| e.kind == EnemyKind::CarrierDaemon && e.is_alive()).map(|(e, _)| e)
}

fn boss_phase(app: &App, boss: Entity) -> BossPhase {
    app.world().get::<BossState>(boss).unwrap().phase
}

fn living_pylons(app: &mut App) -> Vec<Entity> {
    let mut q = app.world_mut().query::<(Entity, &BossPylon)>();
    q.iter(app.world()).filter(|(_, p)| p.is_alive()).map(|(e, _)| e).collect()
}

fn upload_progress(app: &App) -> f32 {
    extraction(app).progress().unwrap_or(-1.0)
}

/// Boss floor gate: THE CARRIER DAEMON landed with the uplink and parks it at
/// 99%; killing it locks extraction on the spot.
fn boss_floor_gate(app: &mut App) {
    let floor = grid(app).floor;
    let boss = carrier(app).unwrap_or_else(|| panic!("floor {floor}: the Carrier Daemon lands when the SOS starts"));
    assert_eq!(enemy(app, boss).unwrap().max_hp, segnale_debole::enemies::carrier_daemon_hp(floor));
    if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
        *progress = 0.985;
    }
    for _ in 0..FPS {
        godmode(app);
        cull_kind(app, EnemyKind::JammerFiend, None);
        step(app, 1);
    }
    assert_eq!(app_state(app), AppState::Playing, "floor {floor}: no extraction while the boss lives");
    assert!(matches!(extraction(app), ExtractionState::Uploading { boss_hold: true, .. }), "floor {floor}: {:?}", extraction(app));
    assert_eq!(upload_progress(app), BOSS_HOLD_PROGRESS);
    assert!(app.world().resource::<BossHud>().active, "boss plate up");
    kill(app, boss);
    step(app, 1);
    assert!(matches!(extraction(app), ExtractionState::Extracting { .. }), "floor {floor}: boss death locks extraction at once: {:?}", extraction(app));
    step(app, 2);
    assert!(!app.world().resource::<BossHud>().active, "boss plate gone");
    assert_eq!(count::<BossPylon>(app), 0);
}

/// A standing cell with line of sight to a jammer core.
fn jammer_vantage(g: &LevelGrid, j: IVec2) -> Option<(IVec2, Vec3)> {
    let core = g.cell_center(j) + Vec3::Y * 1.3;
    [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .flat_map(|o| [j + o * 2, j + o])
        .filter(|c| matches!(g.cell(*c), Cell::Floor { .. }) && g.is_reachable(g.spawn, *c))
        .find(|c| has_line_of_sight(g, g.cell_center(*c) + Vec3::Y * PLAYER_EYE_HEIGHT, core))
        .map(|c| (c, core))
}

fn jammer_at(app: &mut App, cell: IVec2) -> Option<Entity> {
    let g = grid(app);
    let mut q = app.world_mut().query::<(Entity, &DemonJammer, &Transform)>();
    q.iter(app.world()).find(|(_, _, tf)| g.world_to_cell(tf.translation) == cell).map(|(e, _, _)| e)
}

/// Hold the trigger until `done` or the frame budget runs out. Returns frames used.
fn fire_until(app: &mut App, max_frames: usize, mut aim: impl FnMut(&mut App), mut done: impl FnMut(&mut App) -> bool) -> usize {
    bot(app).fire = true;
    for f in 0..max_frames {
        aim(app);
        step(app, 1);
        if done(app) {
            bot(app).fire = false;
            step(app, 1);
            return f + 1;
        }
    }
    bot(app).fire = false;
    max_frames
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn full_floor_extraction_upgrade_and_floor2_transition() {
    let mut app = build_headless_test_app();
    assert_eq!(app_state(&app), AppState::MainMenu);
    begin_run(&mut app, 7);

    // Floor 1 spawned: player, tower, jammers, garrison.
    let g = grid(&app);
    assert_eq!(g.floor, 1);
    assert!(g.validate(), "labyrinth connectivity");
    assert_eq!(count::<Player>(&mut app), 1);
    assert_eq!(count::<SignalTower>(&mut app), 1);
    assert_eq!(count::<DemonJammer>(&mut app), jammer_count_for_floor(1));
    let garrison = count::<Enemy>(&mut app);
    assert!(garrison > 0 && garrison <= garrison_size(1), "garrison {garrison}");
    assert!(player_feet(&mut app).distance(g.spawn_world()) < 0.5, "player starts at the airlock");
    assert_eq!(extraction(&app), ExtractionState::Searching { hold: 0.0 });

    godmode(&mut app);
    // A randomly placed garrison Jammer Fiend near the ring would shave a bar
    // off the reading (jamming has its own test).
    cull_kind(&mut app, EnemyKind::JammerFiend, None);
    trigger_uplink(&mut app);
    assert_eq!(signal(&app).bars, 5);

    // Hold the ring (AI live; Jammer Fiends culled: jamming has its own test).
    let mut last = 0.0f32;
    let mut baron_at = None;
    let mut frames = 0;
    while let ExtractionState::Uploading { progress, in_ring, .. } = extraction(&app) {
        assert!(progress >= last, "upload progress never goes backwards");
        assert!(in_ring, "player stays in the ring");
        last = progress;
        if baron_at.is_none() {
            let mut q = app.world_mut().query::<&Enemy>();
            if q.iter(app.world()).any(|e| e.kind == EnemyKind::HellBaron) {
                baron_at = Some(progress);
            }
        }
        godmode(&mut app);
        cull_kind(&mut app, EnemyKind::JammerFiend, None);
        step(&mut app, 1);
        frames += 1;
        assert!(frames < 40 * FPS, "uplink must finish within 40 s at 100% rate");
    }
    let baron_at = baron_at.expect("Hell Baron drops in during the uplink");
    assert!((0.5..0.53).contains(&baron_at), "Hell Baron spawns at ~50% (was {baron_at})");
    // 24 s at 100% speed on floor 1.
    assert!((23 * FPS..26 * FPS).contains(&frames), "upload took {frames} frames");
    assert!(matches!(extraction(&app), ExtractionState::Extracting { .. }));

    // Orbital beam vaporizes every demon, then the floor completes.
    step(&mut app, 3);
    let alive = {
        let mut q = app.world_mut().query::<&Enemy>();
        q.iter(app.world()).filter(|e| e.is_alive()).count()
    };
    assert_eq!(alive, 0, "extraction beam vaporizes all demons");
    let mut guard = 0;
    while app_state(&app) == AppState::Playing {
        step(&mut app, 1);
        guard += 1;
        assert!(guard < 3 * FPS, "extraction completes in ~1.6 s");
    }
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let choices = app.world().resource::<UpgradeChoices>().0.clone();
    assert_eq!(choices.len(), 3);
    assert!(choices[0] != choices[1] && choices[1] != choices[2] && choices[0] != choices[2], "{choices:?}");

    // Damage the player so the between-floor patch is observable.
    with_player(&mut app, |p| {
        p.hp = 40.0;
        p.ammo_shells = 0;
    });
    bot(&mut app).pick_upgrade = Some(0);
    step(&mut app, 3);

    assert_eq!(app_state(&app), AppState::Playing);
    let run = run_stats(&app);
    assert_eq!(run.floor, 2);
    assert_eq!(run.upgrades, vec![choices[0]]);
    assert_ne!(*app.world().resource::<PlayerModifiers>(), PlayerModifiers::default(), "upgrade modifiers applied");
    let g2 = grid(&app);
    assert_eq!(g2.floor, 2);
    assert!(g2.validate());
    assert_eq!(count::<DemonJammer>(&mut app), jammer_count_for_floor(2));
    let p = player(&mut app);
    assert_eq!(p.hp, 65.0, "field repair +25 HP");
    assert!(p.ammo_shells >= p.max_shells / 2, "shells topped up to 50%");
    assert!(player_feet(&mut app).distance(g2.spawn_world()) < 0.5, "player moved to the Floor 2 airlock");
    assert_eq!(extraction(&app), ExtractionState::Searching { hold: 0.0 });
}

#[test]
fn weapons_destroy_jammer_and_boost_signal() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 11);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);
    assert!(g.jammers.len() >= 2);

    // --- Shotgun on jammer #0 ---
    let j0 = g.jammers[0];
    let (stand0, core0) = jammer_vantage(&g, j0).expect("vantage on jammer 0");
    teleport(&mut app, g.cell_center(stand0), core0);
    step(&mut app, 3);
    let raw_before = signal(&app).raw_strength;
    let jam0 = jammer_at(&mut app, j0).expect("jammer 0 entity");
    let base0 = g.cell_center(j0);
    let shells_before = player(&mut app).ammo_shells;
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Shotgun);
    let frames = fire_until(&mut app, 10 * FPS, |_| {}, |a| a.world().get_entity(jam0).is_err());
    assert!(frames < 10 * FPS, "shotgun destroys the jammer");
    assert!(player(&mut app).ammo_shells < shells_before, "shells were spent");
    assert_eq!(run_stats(&app).jammers_destroyed, 1);
    let drops = {
        let mut q = app.world_mut().query_filtered::<&Transform, With<Pickup>>();
        q.iter(app.world()).filter(|tf| tf.translation.with_y(0.0).distance(base0.with_y(0.0)) < 2.5).count()
    };
    assert!(drops >= 3, "jammer drops shells/health/plasma ({drops})");

    // --- Plasma Repeater on jammer #1 ---
    let j1 = g.jammers[1];
    let (stand1, core1) = jammer_vantage(&g, j1).expect("vantage on jammer 1");
    teleport(&mut app, g.cell_center(stand1), core1);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    step(&mut app, 30);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Plasma);
    let jam1 = jammer_at(&mut app, j1).expect("jammer 1 entity");
    let plasma_before = player(&mut app).ammo_plasma;
    let frames = fire_until(&mut app, 15 * FPS, |_| {}, |a| a.world().get_entity(jam1).is_err());
    assert!(frames < 15 * FPS, "plasma destroys the jammer");
    assert!(player(&mut app).ammo_plasma < plasma_before, "plasma was spent");
    assert_eq!(run_stats(&app).jammers_destroyed, 2);

    // --- Signal boost at the first vantage point ---
    teleport(&mut app, g.cell_center(stand0), core0);
    step(&mut app, 3);
    let raw_after = signal(&app).raw_strength;
    assert!(raw_after > raw_before, "destroying jammers boosts signal: {raw_before} -> {raw_after}");
}

#[test]
fn jammer_fiend_stalls_sos_until_killed_by_overload() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 21);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    trigger_uplink(&mut app);
    step(&mut app, FPS / 2);
    let g = grid(&app);
    let tower = g.tower_world();

    // A live Jammer Fiend on the far side of the pedestal, inside the ring.
    let me = ring_cell(&g);
    let far = g.tower - (me - g.tower);
    let fpos = spawn_pos_for(EnemyKind::JammerFiend, &g, far);
    assert!(Vec2::new(fpos.x - tower.x, fpos.z - tower.z).length() < TOWER_RING_RADIUS);
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, fpos);
    step(&mut app, 30);
    let ExtractionState::Uploading { progress: p1, jammed, .. } = extraction(&app) else { panic!("still uploading") };
    assert!(jammed, "fiend in the ring jams the uplink");
    for _ in 0..2 * FPS {
        cull_kind(&mut app, EnemyKind::JammerFiend, Some(fiend));
        godmode(&mut app);
        step(&mut app, 1);
    }
    let ExtractionState::Uploading { progress: p2, jammed, .. } = extraction(&app) else { panic!("still uploading") };
    assert!(jammed);
    assert_eq!(p1, p2, "jammed uplink does not advance");

    // 5G Overload (needs >= 3 bars even with the fiend's mobile dead zone).
    let bars = signal(&app).bars;
    assert!(bars >= 3, "ring signal under fiend jamming: {bars} bars");
    let hp0 = enemy(&app, fiend).unwrap().hp;
    bot(&mut app).overload = true;
    step(&mut app, 2);
    let f = enemy(&app, fiend).expect("fiend survives one overload");
    assert!(f.hp < hp0, "overload damages the fiend");
    assert!(f.stun_timer > 0.0, "overload stuns");
    assert!(app.world().resource::<PhoneApps>().overload_cooldown > 0.0);

    // Finish it with the shotgun.
    let frames = fire_until(
        &mut app,
        12 * FPS,
        |a| {
            cull_kind(a, EnemyKind::JammerFiend, Some(fiend));
            godmode(a);
            if a.world().get_entity(fiend).is_ok() {
                let c = enemy_center(a, fiend);
                aim_at(a, c);
            }
        },
        |a| a.world().get_entity(fiend).is_err(),
    );
    assert!(frames < 12 * FPS, "fiend killed");
    let ExtractionState::Uploading { progress: p3, jammed, .. } = extraction(&app) else { panic!("still uploading") };
    assert!(!jammed, "uplink un-jammed once the fiend is dead");
    for _ in 0..FPS {
        cull_kind(&mut app, EnemyKind::JammerFiend, None);
        godmode(&mut app);
        step(&mut app, 1);
    }
    let ExtractionState::Uploading { progress: p4, .. } = extraction(&app) else { panic!("still uploading") };
    assert!(p4 > p3 + 0.02, "upload resumes: {p3} -> {p4}");
}

#[test]
fn ping_scan_and_packet_sniffer_bonus_damage() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 31);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);
    let me = ring_cell(&g);
    teleport(&mut app, g.cell_center(me), g.tower_world());
    install(&mut app, UpgradeId::PacketSniffer);
    step(&mut app, 3);
    assert!(signal(&app).bars >= 1);

    // A stunned (stationary) Imp out in the atrium, 2 cells past the pedestal.
    let out = g.tower + (me - g.tower) * 3;
    assert!(!g.cell(out).is_wall());
    let imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, out));
    step(&mut app, 1);
    app.world_mut().get_mut::<Enemy>(imp).unwrap().stun_timer = 1.0e3;
    assert!(app.world().get::<PingMarked>(imp).is_none());

    bot(&mut app).ping = true;
    step(&mut app, 2);
    assert!(app.world().get::<PingMarked>(imp).is_some(), "Ping Scan marks nearby demons");
    let cd = app.world().resource::<PhoneApps>().ping_cooldown;
    assert!(cd > 0.0 && cd <= PING_COOLDOWN * 0.5, "Packet Sniffer halves ping cooldown ({cd})");

    // One plasma bolt on the marked Imp: +35% bonus damage.
    let c = enemy_center(&app, imp);
    aim_at(&mut app, c);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    step(&mut app, 30);
    let hp0 = enemy(&app, imp).unwrap().hp;
    bot(&mut app).fire = true;
    step(&mut app, 1);
    bot(&mut app).fire = false;
    step(&mut app, 30);
    let dealt = hp0 - enemy(&app, imp).unwrap().hp;
    assert!((dealt - PLASMA_DAMAGE * 1.35).abs() < 1e-3, "marked bolt dealt {dealt}, expected {}", PLASMA_DAMAGE * 1.35);
}

#[test]
fn dead_zone_enrage_and_signal_siphon_heal() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);

    // Imp right next to a Demon Jammer (0-bar Dead Zone) vs one inside the Signal Ring.
    let j = g.jammers[0];
    let dz_cell = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .map(|o| j + o)
        .find(|c| matches!(g.cell(*c), Cell::Floor { .. }))
        .expect("floor next to jammer");
    let dz_imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, dz_cell));
    let me = ring_cell(&g);
    let ring_imp_cell = g.tower + (me - g.tower) * 3;
    let ring_imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, ring_imp_cell));
    step(&mut app, FPS / 2);
    let dz = enemy(&app, dz_imp).unwrap();
    assert!(dz.enraged, "Static Imp in a Dead Zone is enraged");
    assert!((dz.move_speed() - dz.stats.speed * ENRAGE_SPEED_MULT).abs() < 1e-4);
    assert!(!enemy(&app, ring_imp).unwrap().enraged, "Imp inside the 5-bar ring is calm");
    app.world_mut().despawn(dz_imp);

    // Signal Siphon: kill inside a >= 3-bar zone heals the player.
    install(&mut app, UpgradeId::SignalSiphon);
    teleport(&mut app, g.cell_center(me), g.tower_world());
    step(&mut app, 3);
    app.world_mut().get_mut::<Enemy>(ring_imp).unwrap().stun_timer = 1.0e3;
    with_player(&mut app, |p| p.hp = 50.0);
    assert!(signal(&app).bars >= 3);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    let frames = fire_until(
        &mut app,
        10 * FPS,
        |a| {
            if a.world().get_entity(ring_imp).is_ok() {
                let c = enemy_center(a, ring_imp);
                aim_at(a, c);
            }
        },
        |a| a.world().get_entity(ring_imp).is_err(),
    );
    assert!(frames < 10 * FPS, "imp killed");
    assert_eq!(run_stats(&app).kills, 1);
    assert_eq!(player(&mut app).hp, 58.0, "Signal Siphon +8 HP on a kill in a 5-bar zone");
}

#[test]
fn player_death_game_over_and_clean_restart() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 51);
    install(&mut app, UpgradeId::HighCapacityCell);
    app.world_mut().resource_mut::<RunStats>().kills = 9;
    with_player(&mut app, |p| {
        p.invuln_timer = 0.0;
        p.apply_damage(1000.0);
    });
    assert!(player(&mut app).is_dead());
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::GameOver);

    // "MAIN MENU" then "START" (same transitions as the menu buttons).
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(count::<Player>(&mut app), 0, "player torn down");
    assert!(app.world().get_resource::<LevelGrid>().is_none(), "level torn down");
    begin_run(&mut app, 52);

    let run = run_stats(&app);
    assert_eq!(run.floor, 1);
    assert_eq!(run.kills, 0);
    assert!(run.upgrades.is_empty());
    assert_eq!(*app.world().resource::<PlayerModifiers>(), PlayerModifiers::default());
    let p = player(&mut app);
    assert_eq!(p.hp, p.max_hp);
    assert_eq!(p.max_shells, Player::default().max_shells, "capacity upgrades do not leak into a new run");
    assert_eq!(grid(&app).floor, 1);
    assert_eq!(extraction(&app), ExtractionState::Searching { hold: 0.0 });
    assert_eq!(count::<Player>(&mut app), 1);
}

#[test]
fn multi_seed_five_floor_victory_soak() {
    for seed in [3u64, 1337, 0xBEEF] {
        let mut app = build_headless_test_app();
        begin_run(&mut app, seed);
        let mut seen_floors = Vec::new();
        loop {
            let g = grid(&app);
            assert!(g.validate(), "seed {seed} floor {}: labyrinth connectivity", g.floor);
            assert!(g.is_reachable(g.spawn, ring_cell(&g)));
            assert_eq!(count::<DemonJammer>(&mut app), jammer_count_for_floor(g.floor));
            seen_floors.push(g.floor);
            godmode(&mut app);
            trigger_uplink(&mut app);
            // Fast-forward the uplink to 90%: waves + Hell Baron still spawn.
            if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
                *progress = 0.9;
            }
            if is_boss_floor(g.floor) {
                boss_floor_gate(&mut app);
            } else {
                assert!(carrier(&mut app).is_none(), "seed {seed} floor {}: no boss off boss floors", g.floor);
            }
            let mut guard = 0;
            while app_state(&app) == AppState::Playing {
                godmode(&mut app);
                cull_kind(&mut app, EnemyKind::JammerFiend, None);
                step(&mut app, 1);
                guard += 1;
                assert!(guard < 15 * FPS, "seed {seed} floor {}: floor never completed", g.floor);
            }
            if g.floor == FINAL_FLOOR {
                assert_eq!(app_state(&app), AppState::Victory, "floor 5 completion is a victory");
                break;
            }
            assert_eq!(app_state(&app), AppState::UpgradeSelect);
            bot(&mut app).pick_upgrade = Some(g.floor as usize % 3);
            step(&mut app, 3);
            assert_eq!(app_state(&app), AppState::Playing);
            assert_eq!(grid(&app).floor, g.floor + 1);
        }
        assert_eq!(seen_floors, vec![1, 2, 3, 4, 5]);
        assert_eq!(run_stats(&app).upgrades.len(), 4);
    }
}

/// The Layer-2 autonomous playtest bot, run headless (no screenshots) across
/// seeds: menu -> jammer -> tower -> SOS -> extraction -> upgrade -> Floor 2.
#[test]
fn autonomous_playtest_bot_clears_floor1_headless() {
    for seed in [1234u64, 99, 2026, 7, 31337] {
        let mut app = App::new();
        app.add_plugins(HeadlessGamePlugin).add_plugins(PlaytestBotPlugin { out_dir: None, seed, fixed_dt: Some(1.0 / 30.0) });
        let mut report = None;
        for _ in 0..(BOT_TIMEOUT_SECS as usize + 10) * 30 {
            app.update();
            if let Some(r) = app.world().resource::<PlaytestBot>().report.clone() {
                report = Some(r);
                break;
            }
        }
        let (passed, json) = report.unwrap_or_else(|| panic!("seed {seed}: bot never produced a report"));
        println!("seed {seed}: {json}");
        assert!(passed, "seed {seed}: autonomous playtest failed:\n{json}");
        assert!(json.contains("\"would_have_died\": 0,"), "seed {seed}: bot absorbed a fatal blow:\n{json}");
    }
}

// ---------------------------------------------------------------------------
// Quake / CPMA movement
// ---------------------------------------------------------------------------

/// Swap the floor for a 64 m open arena (every production system keeps
/// running; the floor-change hooks re-stage pickups / extraction) and stand
/// the player in the middle facing -Z. Returns the arena center.
fn open_arena(app: &mut App) -> Vec3 {
    let wall = "#".repeat(18);
    let row = format!("#{}#", ".".repeat(16));
    let rows: Vec<&str> = std::iter::once(wall.as_str()).chain(std::iter::repeat_n(row.as_str(), 16)).chain(std::iter::once(wall.as_str())).collect();
    app.world_mut().insert_resource(LevelGrid::from_ascii(&rows));
    step(app, 2);
    despawn_all_enemies(app);
    let c = Vec3::new(36.0, 0.0, 36.0);
    teleport(app, c, c + Vec3::new(0.0, PLAYER_EYE_HEIGHT, -10.0));
    step(app, 10);
    assert!(player(app).grounded);
    c
}

fn camera_fov_deg(app: &mut App) -> f32 {
    let mut q = app.world_mut().query_filtered::<&Projection, With<PlayerCamera>>();
    match q.single(app.world()).expect("player camera") {
        Projection::Perspective(pp) => pp.fov.to_degrees(),
        other => panic!("unexpected projection {other:?}"),
    }
}

fn horizontal_speed(p: &Player) -> f32 {
    Vec2::new(p.velocity.x, p.velocity.z).length()
}

/// Hold Space + D and sweep the mouse right: CPMA air-strafing over chained
/// zero-friction landing hops builds speed far past the run speed, counts the
/// chain and widens the camera FOV.
#[test]
fn bunny_hop_air_strafe_chain_builds_speed_and_widens_fov() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 2026);
    godmode(&mut app);
    open_arena(&mut app);
    // Run forward to full speed first.
    bot(&mut app).strafe_axes = Vec2::new(0.0, 1.0);
    step(&mut app, FPS / 2);
    let p = player(&mut app);
    assert!(p.grounded && (horizontal_speed(&p) - MOVE_SPEED).abs() < 0.3, "run speed {}", horizontal_speed(&p));
    let fov_at_run = camera_fov_deg(&mut app);
    assert!((fov_at_run - BASE_FOV_DEG).abs() < 0.5, "fov {fov_at_run}");

    {
        let mut b = bot(&mut app);
        b.jump = true;
        b.strafe_axes = Vec2::new(1.0, 0.0);
    }
    let mut yaw = 0.0f32;
    // Movement substeps can land and re-hop inside one frame, so a hop is
    // counted when vertical velocity turns upward.
    let (mut max_chain, mut hops, mut prev_vy) = (0, 0, 0.0f32);
    for _ in 0..4 * FPS {
        yaw -= 2.5 / FPS as f32;
        bot(&mut app).look_yaw = Some(yaw);
        step(&mut app, 1);
        let p = player(&mut app);
        if prev_vy <= 0.0 && p.velocity.y > 0.0 {
            hops += 1;
        }
        prev_vy = p.velocity.y;
        max_chain = max_chain.max(p.bhop_chain);
    }
    let p = player(&mut app);
    let speed = horizontal_speed(&p);
    println!("bhop: {hops} hops, chain {max_chain}, {speed:.1} m/s, fov {:.1}", camera_fov_deg(&mut app));
    assert!(hops >= 4, "only {hops} hops");
    assert!(speed > 16.0, "air-strafe chain only reached {speed} m/s");
    assert!(max_chain >= 2 && p.bhop_chain >= 2, "chain {max_chain} / {}", p.bhop_chain);
    assert!(camera_fov_deg(&mut app) > BASE_FOV_DEG + 5.0, "fov {}", camera_fov_deg(&mut app));
    assert!(p.hp >= p.max_hp, "took damage while hopping");

    // Release everything: friction stops the player and the chain breaks.
    bot(&mut app).release_all();
    step(&mut app, FPS);
    let p = player(&mut app);
    assert!(p.grounded && horizontal_speed(&p) < 0.5 && p.bhop_chain == 0, "{p:?}");
}

/// Dash + jump on the same tick launches the full Airplane Mode burst into a hop.
#[test]
fn dash_into_jump_carries_dash_speed_mid_air() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 7);
    godmode(&mut app);
    open_arena(&mut app);
    assert!(player(&mut app).dash_ready());
    {
        let mut b = bot(&mut app);
        b.dash = true;
        b.jump = true;
    }
    step(&mut app, 1);
    let p = player(&mut app);
    assert!(p.dash_started || p.dash_cooldown > 0.0, "dash never fired");
    assert!(p.dash_timer == 0.0 && p.velocity.y > 0.0, "dash not converted into a hop: {p:?}");
    assert!((horizontal_speed(&p) - DASH_SPEED).abs() < 0.2, "{}", horizontal_speed(&p));
    bot(&mut app).jump = false;
    step(&mut app, FPS / 5);
    let p = player(&mut app);
    assert!(!p.grounded, "landed too early");
    assert!(horizontal_speed(&p) > DASH_SPEED - 1.5, "dash speed bled mid-air: {}", horizontal_speed(&p));
    assert!(camera_fov_deg(&mut app) > BASE_FOV_DEG + 8.0, "fov {}", camera_fov_deg(&mut app));
}

/// Moving + turning and firing in the same update shoots from this frame's
/// eye pose (body `Transform` x camera local `Transform`), not from last
/// frame's `GlobalTransform` (only propagated in `PostUpdate`).
#[test]
fn shots_fire_from_this_frames_moved_and_turned_eye() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 11);
    godmode(&mut app);
    let c = open_arena(&mut app);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    step(&mut app, FPS);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Plasma);
    assert_eq!(count::<PlayerProjectile>(&mut app), 0);

    // One update: the body is relocated 14 m and runs east at 20 m/s, the view
    // turns 90 deg from -Z to +X, and the trigger is pulled.
    let moved = c + Vec3::new(-12.0, 0.0, 8.0);
    {
        let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
        let (mut p, mut tf) = q.single_mut(app.world_mut()).unwrap();
        tf.translation = moved;
        p.velocity = Vec3::new(20.0, 0.0, 0.0);
    }
    {
        let mut b = bot(&mut app);
        b.look_yaw = Some(-std::f32::consts::FRAC_PI_2);
        b.look_pitch = Some(0.0);
        b.fire = true;
    }
    step(&mut app, 1);
    bot(&mut app).fire = false;

    let mut q = app.world_mut().query::<(&PlayerProjectile, &Transform)>();
    let bolts: Vec<(Vec3, Vec3)> = q.iter(app.world()).map(|(b, tf)| (b.vel, tf.translation)).collect();
    assert_eq!(bolts.len(), 1, "exactly one plasma bolt");
    let (vel, pos) = bolts[0];
    let eye = player_feet(&mut app) + Vec3::Y * PLAYER_EYE_HEIGHT;
    let heading = vel.normalize();
    assert!(heading.dot(Vec3::X) > 0.97, "bolt flew along last frame's heading: {heading:?}");
    assert!((pos - eye).length() < 2.5, "bolt spawned {:.1} m from this frame's eye ({pos:?} vs {eye:?})", (pos - eye).length());
    let stale_eye = c + Vec3::Y * PLAYER_EYE_HEIGHT;
    assert!((pos - stale_eye).length() > 10.0, "bolt spawned at last frame's eye");
}

/// An adjacent walkable atrium cell pair (player, target) away from the pedestal.
fn open_pair(g: &LevelGrid) -> (IVec2, IVec2) {
    let me = ring_cell(g);
    let a = g.tower + (me - g.tower) * 3;
    let b = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .map(|o| a + o)
        .find(|c| matches!(g.cell(*c), Cell::AtriumFloor | Cell::Floor { .. }) && (g.cell_center(*c).y - g.cell_center(a).y).abs() < 0.1)
        .expect("open neighbour cell");
    (a, b)
}

/// Close-range Shotgun one-shots a Floor 1 Static Imp; Plasma needs several bolts.
#[test]
fn shotgun_one_shots_floor1_imp_plasma_needs_multiple_bolts() {
    use segnale_debole::weapons::PELLET_DAMAGE;
    let imp_hp = Enemy::new(EnemyKind::Imp, 1).hp;
    assert!(8.0 * PELLET_DAMAGE >= imp_hp, "8 pellets ({}) must out-damage a Floor 1 Imp ({imp_hp})", 8.0 * PELLET_DAMAGE);
    let bolts = (imp_hp / PLASMA_DAMAGE).ceil() as u32;
    assert!(bolts >= 3, "Plasma TTK must be multi-bolt ({bolts})");

    let mut app = build_headless_test_app();
    begin_run(&mut app, 4242);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    cull_kind(&mut app, EnemyKind::JammerFiend, None);
    let g = grid(&app);
    let (a, b) = open_pair(&g);

    let stunned_imp = |app: &mut App| {
        let imp = spawn_demon(app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, b));
        step(app, 1);
        app.world_mut().get_mut::<Enemy>(imp).unwrap().stun_timer = 1.0e3;
        imp
    };
    let dead = |app: &App, e: Entity| enemy(app, e).is_none_or(|en| !en.is_alive());

    // Shotgun: one clean blast at ~4 m.
    let imp = stunned_imp(&mut app);
    let c = enemy_center(&app, imp);
    teleport(&mut app, g.cell_center(a), c);
    bot(&mut app).switch_weapon = Some(Weapon::Shotgun);
    step(&mut app, 40);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Shotgun);
    let c = enemy_center(&app, imp);
    aim_at(&mut app, c);
    step(&mut app, 2);
    bot(&mut app).fire = true;
    step(&mut app, 1);
    bot(&mut app).fire = false;
    step(&mut app, 3);
    assert!(dead(&app, imp), "one close Shotgun blast kills a Floor 1 Imp (hp left {:?})", enemy(&app, imp).map(|e| e.hp));

    // Plasma: one bolt is not enough; a short burst is.
    step(&mut app, 60);
    let imp = stunned_imp(&mut app);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    step(&mut app, 40);
    let c = enemy_center(&app, imp);
    aim_at(&mut app, c);
    step(&mut app, 2);
    bot(&mut app).fire = true;
    step(&mut app, 1);
    bot(&mut app).fire = false;
    step(&mut app, 30);
    assert!(!dead(&app, imp), "a single plasma bolt must not kill a Floor 1 Imp");
    let used = fire_until(&mut app, 6 * FPS, |app| { let c = enemy_center(app, imp); aim_at(app, c); }, |app| dead(app, imp));
    assert!(used < 6 * FPS, "plasma burst eventually kills the Imp");
}

/// SOS start drops ring supplies; a fiend in the ring jams with alert + X-ray beacon.
#[test]
fn sos_supply_drops_and_jammer_fiend_alert_beacon() {
    use segnale_debole::fx::{Fx, HudFeed};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 77);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    // Clear floor loot so only SOS drops count.
    let ids: Vec<Entity> = { let mut q = app.world_mut().query_filtered::<Entity, With<Pickup>>(); q.iter(app.world()).collect() };
    for e in ids {
        app.world_mut().despawn(e);
    }
    trigger_uplink(&mut app);
    step(&mut app, 2);
    let g = grid(&app);
    let tower = g.tower_world();
    let drops: Vec<Vec3> = { let mut q = app.world_mut().query::<(&Pickup, &Transform)>(); q.iter(app.world()).map(|(_, t)| t.translation).collect() };
    assert_eq!(drops.len(), 2, "two SOS supply drops");
    for d in &drops {
        assert!(Vec2::new(d.x - tower.x, d.z - tower.z).length() <= TOWER_RING_RADIUS, "supply drop inside the Signal Ring: {d}");
    }

    // Jammer Fiend inside the ring: alert + beacons.
    let fx_before = count::<Fx>(&mut app);
    let me = ring_cell(&g);
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, g.tower + (me - g.tower) * 2));
    step(&mut app, 1);
    app.world_mut().get_mut::<Enemy>(fiend).unwrap().stun_timer = 1.0e3;
    step(&mut app, 3);
    assert!(matches!(extraction(&app), ExtractionState::Uploading { jammed: true, .. }), "fiend in the ring jams the uplink");
    let feed = app.world().resource::<HudFeed>().clone();
    assert!(format!("{feed:?}").contains("UPLINK JAMMED - KILL THE JAMMER FIEND!"), "jammed alert shown");
    assert!(count::<Fx>(&mut app) > fx_before, "X-ray beacon spawned on the fiend");
}

// ---------------------------------------------------------------------------
// Milestone 6: reworked + signature "Segnale debole" upgrades
// ---------------------------------------------------------------------------

/// Teleport next to Demon Jammers until the *natural* (pre-antenna) signal is
/// a 0-bar blackout. Returns the player's cell.
fn find_blackout(app: &mut App) -> IVec2 {
    let g = grid(app);
    for &j in &g.jammers {
        for r in 1..=2 {
            for o in [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
                let c = j + o * r;
                if !matches!(g.cell(c), Cell::Floor { .. } | Cell::AtriumFloor) || !g.is_reachable(g.spawn, c) {
                    continue;
                }
                teleport(app, g.cell_center(c), g.cell_center(j) + Vec3::Y * 1.3);
                step(app, 2);
                if bars_from_strength(signal(app).natural_raw_strength) == 0 {
                    return c;
                }
            }
        }
    }
    panic!("no natural 0-bar blackout cell next to any jammer");
}

/// A cell `k` straight walkable steps away from `from` (same direction), with
/// a matching floor height so a demon can walk the line directly.
fn straight_cell(g: &LevelGrid, from: IVec2, k: i32) -> Option<IVec2> {
    [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y].into_iter().find_map(|o| {
        let ok = (0..k).all(|i| g.in_bounds(from + o * (i + 1)) && g.can_walk(from + o * (i + 1), from + o * i) && g.can_walk(from + o * i, from + o * (i + 1)));
        ok.then_some(from + o * k)
    })
}

fn kill(app: &mut App, e: Entity) {
    app.world_mut().get_mut::<Enemy>(e).unwrap().hp = 0.0;
}

#[test]
fn high_capacity_cell_leaves_fire_rate_alone() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 51);
    install(&mut app, UpgradeId::HighCapacityCell);
    let m = app.world().resource::<PlayerModifiers>().clone();
    assert_eq!(m.fire_rate_mult, 1.0);
    assert!((m.pickup_mult - 1.3).abs() < 1e-6);
    let p = player(&mut app);
    assert_eq!((p.max_shells, p.max_plasma), (68, 270));
}

#[test]
fn sub_ghz_ping_projects_floor_route_to_tower() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 52);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    // Antenna guarantees >= 1 bar for Ping at spawn.
    install(&mut app, UpgradeId::CopperAntenna);
    step(&mut app, 3);
    assert!(signal(&app).bars >= 1);

    // Plain Ping: no route.
    bot(&mut app).ping = true;
    step(&mut app, 2);
    assert_eq!(count::<RouteBreadcrumb>(&mut app), 0, "no route without Sub-GHz");
    app.world_mut().resource_mut::<PhoneApps>().ping_cooldown = 0.0;

    install(&mut app, UpgradeId::SubGhzPenetration);
    let g = grid(&app);
    let me = g.world_to_cell(player_feet(&mut app));
    let expect = tower_route(&g, me, ROUTE_MAX_STEPS);
    assert!(!expect.is_empty(), "spawn has a route to the tower");
    bot(&mut app).ping = true;
    step(&mut app, 2);
    assert_eq!(count::<RouteBreadcrumb>(&mut app), expect.len());
    assert!(expect.len() <= ROUTE_MAX_STEPS);
    // Every breadcrumb sits on a route cell, and the first one is adjacent.
    let mut q = app.world_mut().query::<(&RouteBreadcrumb, &Transform)>();
    let mut crumbs: Vec<(usize, IVec2)> = q.iter(app.world()).map(|(b, tf)| (b.index, g.world_to_cell(tf.translation))).collect();
    crumbs.sort_by_key(|c| c.0);
    let cells: Vec<IVec2> = crumbs.iter().map(|c| c.1).collect();
    assert_eq!(cells, expect);
    assert_eq!((cells[0] - me).abs().element_sum(), 1);
    // Breadcrumbs fade out after ~5 s.
    step(&mut app, 6 * FPS);
    assert_eq!(count::<RouteBreadcrumb>(&mut app), 0);
}

#[test]
fn firewall_daemon_regen_stops_while_jammed() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 21);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::FirewallDaemon);
    trigger_uplink(&mut app);
    with_player(&mut app, |p| p.armor = 0.0);
    for _ in 0..FPS {
        cull_kind(&mut app, EnemyKind::JammerFiend, None);
        step(&mut app, 1);
    }
    let a1 = player(&mut app).armor;
    assert!(a1 > 4.0, "regen while SOS advances in the ring: {a1}");

    // A spared, stunned Jammer Fiend in the ring jams the uplink: no armor farm.
    let g = grid(&app);
    let me = ring_cell(&g);
    let far = g.tower - (me - g.tower);
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, far));
    for _ in 0..20 {
        app.world_mut().get_mut::<Enemy>(fiend).unwrap().stun_timer = 1.0e3;
        step(&mut app, 1);
    }
    assert!(matches!(extraction(&app), ExtractionState::Uploading { jammed: true, .. }));
    with_player(&mut app, |p| p.armor = 0.0);
    for _ in 0..FPS {
        cull_kind(&mut app, EnemyKind::JammerFiend, Some(fiend));
        step(&mut app, 1);
    }
    assert!(matches!(extraction(&app), ExtractionState::Uploading { jammed: true, .. }));
    assert_eq!(player(&mut app).armor, 0.0, "no Firewall regen while jammed");
}

#[test]
fn store_and_forward_queues_blackout_packets_and_fires_arcs() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    // Copper Antenna owners still earn packets: the natural signal is what counts.
    install(&mut app, UpgradeId::CopperAntenna);
    install(&mut app, UpgradeId::StoreAndForward);
    let dz = find_blackout(&mut app);
    let s = signal(&app);
    assert!(s.bars >= 1, "antenna floor lifts the displayed bars");
    assert_eq!(bars_from_strength(s.natural_raw_strength), 0);
    let g = grid(&app);
    for i in 0..4 {
        let imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, dz));
        step(&mut app, 1);
        kill(&mut app, imp);
        step(&mut app, 2);
        assert_eq!(app.world().resource::<PhoneApps>().queued_packets, (i + 1).min(3), "kill {i}");
    }
    // Still in the blackout: nothing fires.
    step(&mut app, 10);
    assert_eq!(app.world().resource::<PhoneApps>().queued_packets, 3);

    // Two stunned demons near the tower; walk back into 3+ bars.
    let me = ring_cell(&g);
    let out = g.tower + (me - g.tower) * 3;
    let a = spawn_demon(&mut app, EnemyKind::HellBaron, spawn_pos_for(EnemyKind::HellBaron, &g, out));
    let b = spawn_demon(&mut app, EnemyKind::HellBaron, spawn_pos_for(EnemyKind::HellBaron, &g, g.tower + (me - g.tower) * 4));
    step(&mut app, 1);
    for e in [a, b] {
        app.world_mut().get_mut::<Enemy>(e).unwrap().stun_timer = 1.0e3;
    }
    let hp0 = enemy(&app, a).unwrap().hp + enemy(&app, b).unwrap().hp;
    teleport(&mut app, g.cell_center(me), g.tower_world());
    step(&mut app, 3);
    assert!(signal(&app).bars >= 3);
    assert_eq!(app.world().resource::<PhoneApps>().queued_packets, 0, "packets discharged");
    let hp1 = enemy(&app, a).unwrap().hp + enemy(&app, b).unwrap().hp;
    assert!((hp0 - hp1 - 3.0 * STORE_FORWARD_DAMAGE).abs() < 1e-3, "3 arcs x 20 dmg, dealt {}", hp0 - hp1);
    // Nearest target takes the cycled extra packet.
    assert!(enemy(&app, a).unwrap().hp < enemy(&app, b).unwrap().hp);
}

#[test]
fn emergency_callback_unlocks_any_signal_overload() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::EmergencyCallback);
    let dz = find_blackout(&mut app);
    assert_eq!(signal(&app).bars, 0);
    let g = grid(&app);

    // Normally the Overload is gated at 0 bars.
    bot(&mut app).overload = true;
    step(&mut app, 2);
    assert_eq!(app.world().resource::<PhoneApps>().overload_cooldown, 0.0, "0 bars: denied");

    // An unmarked fiend kill does nothing.
    let f1 = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, dz));
    step(&mut app, 1);
    kill(&mut app, f1);
    step(&mut app, 2);
    assert_eq!(app.world().resource::<PhoneApps>().callback_timer, 0.0);

    // A ping-marked fiend kill arms the callback.
    let f2 = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, dz));
    step(&mut app, 1);
    app.world_mut().entity_mut(f2).insert(PingMarked { timer: 5.0, ring: None });
    kill(&mut app, f2);
    step(&mut app, 2);
    let cb = app.world().resource::<PhoneApps>().callback_timer;
    assert!(cb > CALLBACK_SECS - 0.2 && cb <= CALLBACK_SECS, "callback armed: {cb}");
    assert_eq!(signal(&app).bars, 0, "still no signal");

    bot(&mut app).overload = true;
    step(&mut app, 2);
    let apps = app.world().resource::<PhoneApps>().clone();
    assert!(apps.overload_cooldown > 0.0, "any-signal 5G Overload fired at 0 bars");
    assert_eq!(apps.callback_timer, 0.0, "callback consumed");
}

#[test]
fn missed_call_decoy_lures_imp_away_from_player() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 42); // M11 layouts: seed with a straight lane off a blackout cell
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::MissedCall);
    let dz = find_blackout(&mut app);
    assert_eq!(signal(&app).bars, 0);
    let g = grid(&app);

    bot(&mut app).ping = true;
    step(&mut app, 2);
    assert_eq!(count::<DecoyHandset>(&mut app), 1, "decoy deployed at 0 bars");
    assert!(app.world().resource::<PhoneApps>().ping_cooldown > 0.0);
    let decoy = {
        let mut q = app.world_mut().query_filtered::<&Transform, With<DecoyHandset>>();
        q.single(app.world()).unwrap().translation
    };

    // Imp 3 cells from the decoy; the player retreats to spawn, far away.
    let imp_cell = straight_cell(&g, dz, 3).or_else(|| straight_cell(&g, dz, 2)).expect("straight open line from blackout cell");
    let imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, imp_cell));
    teleport(&mut app, g.spawn_world(), g.tower_world());
    step(&mut app, 1);
    let flat = |a: Vec3, b: Vec3| Vec2::new(a.x - b.x, a.z - b.z).length();
    let d0 = flat(app.world().get::<Transform>(imp).unwrap().translation, decoy);
    let p0 = flat(app.world().get::<Transform>(imp).unwrap().translation, player_feet(&mut app));
    assert!(p0 > d0 + 6.0, "player is clearly elsewhere");
    for _ in 0..(1.5 * FPS as f32) as usize {
        godmode(&mut app);
        step(&mut app, 1);
    }
    let d1 = flat(app.world().get::<Transform>(imp).unwrap().translation, decoy);
    println!("imp->decoy {d0:.2} -> {d1:.2} (player {p0:.2} away)");
    assert!(d1 < d0 - 2.0 || d1 < 1.6, "imp drawn to the decoy: {d0} -> {d1}");
    assert!(enemy(&app, imp).unwrap().alerted);

    // The decoy rings for 2.5 s, then hangs up.
    step(&mut app, 2 * FPS);
    assert_eq!(count::<DecoyHandset>(&mut app), 0);
}

// ---------------------------------------------------------------------------
// Meta-progression: loadouts, contracts, signature apps, feats
// ---------------------------------------------------------------------------

/// Same as the main menu "START" with a profile loadout.
fn begin_run_with(app: &mut App, loadout: RunLoadout, seed: u64) {
    app.world_mut()
        .run_system_once(
            move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut cur: ResMut<RunLoadout>, mut next: ResMut<NextState<AppState>>| {
                start_run_with_loadout(&mut run, &mut mods, &mut cur, loadout.clone(), &mut next, seed);
            },
        )
        .unwrap();
    step(app, 3);
    assert_eq!(app_state(app), AppState::Playing);
}

fn chip_loadout(chip: StarterChip) -> RunLoadout {
    RunLoadout { chip, ..default() }
}

fn contract_loadout(contracts: &[CarrierContract]) -> RunLoadout {
    RunLoadout { contracts: contracts.to_vec(), ..default() }
}

fn profile(app: &App) -> ProfileStore {
    app.world().resource::<ProfileStore>().clone()
}

/// Fire a 20-damage demon bolt straight into the player's chest; returns HP lost.
fn take_bolt(app: &mut App) -> f32 {
    with_player(app, |p| {
        p.invuln_timer = 0.0;
        p.armor = 0.0;
        p.hp = p.max_hp;
    });
    let hp0 = player(app).hp;
    let feet = player_feet(app);
    app.world_mut()
        .run_system_once(move |mut commands: Commands, fx: Res<FxAssets>| {
            spawn_enemy_projectile(&mut commands, &fx, feet + Vec3::new(0.4, 0.9, 0.0), Vec3::new(-20.0, 0.0, 0.0), 20.0, 0.3, BurstColor::Fire);
        })
        .unwrap();
    step(app, 2);
    hp0 - player(app).hp
}

/// Fast-forward the current floor's SOS (uplink jumps to 90%) until the
/// floor completes. God mode; Jammer Fiends culled.
fn fast_clear_floor(app: &mut App) {
    godmode(app);
    trigger_uplink(app);
    if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
        *progress = 0.9;
    }
    if is_boss_floor(grid(app).floor) {
        boss_floor_gate(app);
    }
    let mut guard = 0;
    while app_state(app) == AppState::Playing {
        godmode(app);
        cull_kind(app, EnemyKind::JammerFiend, None);
        step(app, 1);
        guard += 1;
        assert!(guard < 15 * FPS, "floor never completed");
    }
}

fn die(app: &mut App) {
    with_player(app, |p| {
        p.invuln_timer = 0.0;
        p.armor = 0.0;
        p.apply_damage(1.0e6);
    });
    step(app, 2);
    assert_eq!(app_state(app), AppState::GameOver);
}

fn back_to_menu(app: &mut App) {
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
    step(app, 2);
    assert_eq!(app_state(app), AppState::MainMenu);
}

#[test]
fn starter_chips_set_vitals_cooldowns_and_dead_zone_armor() {
    // Survey OS: 85 / 85 HP, faster ping, wall penetration.
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, chip_loadout(StarterChip::SurveyOs), 61);
    let p = player(&mut app);
    assert_eq!((p.hp, p.max_hp), (85.0, 85.0), "Survey OS starts at 85/85");
    let m = app.world().resource::<PlayerModifiers>().clone();
    assert!((m.ping_cooldown_mult - 0.70).abs() < 1e-6 && (m.wall_penetration - 0.25).abs() < 1e-6);
    assert_eq!(app.world().resource::<RunLoadout>().chip, StarterChip::SurveyOs);

    // Overclock Radio: overclock from Floor 1, weaker pickups.
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, chip_loadout(StarterChip::OverclockRadio), 61);
    let m = app.world().resource::<PlayerModifiers>().clone();
    assert!((m.overclock_bonus - 0.20).abs() < 1e-6 && (m.overclock_move_bonus - 0.15).abs() < 1e-6);
    assert!((m.pickup_mult - 0.75).abs() < 1e-6);
    assert_eq!(player(&mut app).max_hp, 100.0);

    // Faraday Case: 50 armor, -25% damage in Dead Zones, slower 5G Overload.
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, chip_loadout(StarterChip::FaradayCase), 41);
    assert_eq!(player(&mut app).armor, 50.0);
    despawn_all_enemies(&mut app);
    find_blackout(&mut app);
    assert!(signal(&app).in_dead_zone);
    let lost_dz = take_bolt(&mut app);
    assert!((lost_dz - 15.0).abs() < 1e-3, "Faraday: 20 dmg -> 15 in a Dead Zone (lost {lost_dz})");
    let g = grid(&app);
    teleport(&mut app, g.cell_center(ring_cell(&g)), g.tower_world());
    step(&mut app, 3);
    assert!(!signal(&app).in_dead_zone);
    let lost_ring = take_bolt(&mut app);
    assert!((lost_ring - 20.0).abs() < 1e-3, "full damage outside Dead Zones (lost {lost_ring})");
    bot(&mut app).overload = true;
    step(&mut app, 2);
    let cd = app.world().resource::<PhoneApps>().overload_cooldown;
    assert!(cd > 12.3 && cd <= 12.5, "Faraday +25% Overload cooldown: {cd}");

    // Stock UAC in the same Dead Zone takes full damage.
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    despawn_all_enemies(&mut app);
    find_blackout(&mut app);
    assert!((take_bolt(&mut app) - 20.0).abs() < 1e-3);
    assert_eq!(player(&mut app).armor, 0.0);
}

#[test]
fn priority_jamming_and_overtime_uplink_reshape_the_sos() {
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, contract_loadout(&[CarrierContract::PriorityJamming, CarrierContract::OvertimeUplink]), 21);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    trigger_uplink(&mut app);
    step(&mut app, 1);
    let fiends = {
        let mut q = app.world_mut().query::<&Enemy>();
        q.iter(app.world()).filter(|e| e.kind == EnemyKind::JammerFiend && e.is_alive()).count()
    };
    assert_eq!(fiends, 1, "Priority Jamming: a Jammer Fiend crashes the uplink at once");
    match extraction(&app) {
        ExtractionState::Uploading { duration, .. } => {
            assert!((duration - upload_duration(1) * 1.35).abs() < 1e-3, "Overtime: +35% uplink ({duration})")
        }
        other => panic!("not uploading: {other:?}"),
    }
    let m = app.world().resource::<PlayerModifiers>().clone();
    assert!((m.wave_interval_mult - 0.8).abs() < 1e-6 && m.priority_jamming);

    // Without contracts: no fiend at uplink start, standard duration.
    let mut app = build_headless_test_app();
    begin_run(&mut app, 21);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    trigger_uplink(&mut app);
    step(&mut app, 1);
    assert_eq!(count::<Enemy>(&mut app), 0);
    assert!(matches!(extraction(&app), ExtractionState::Uploading { duration, .. } if (duration - upload_duration(1)).abs() < 1e-3));
}

#[test]
fn roaming_charges_harshen_the_signal_field() {
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, contract_loadout(&[CarrierContract::RoamingCharges]), 7);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let m = app.world().resource::<PlayerModifiers>().clone();
    assert!((m.jammer_radius_mult - 1.3).abs() < 1e-6 && (m.wall_attenuation_exp - 1.2).abs() < 1e-6);
    // Sample the walled spawn and a spot near a jammer, harsh vs relaxed.
    let g = grid(&app);
    let j = g.jammers[0];
    let near_jammer = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .flat_map(|o| (3..=5).map(move |r| j + o * r))
        .find(|c| matches!(g.cell(*c), Cell::Floor { .. }) && g.is_reachable(g.spawn, *c))
        .expect("floor near a jammer");
    let mut stricter = 0;
    for feet in [g.spawn_world(), g.cell_center(near_jammer)] {
        teleport(&mut app, feet, g.tower_world());
        step(&mut app, 2);
        let harsh = signal(&app);
        {
            let mut mods = app.world_mut().resource_mut::<PlayerModifiers>();
            mods.jammer_radius_mult = 1.0;
            mods.wall_attenuation_exp = 1.0;
        }
        step(&mut app, 2);
        let relaxed = signal(&app);
        assert!(harsh.natural_raw_strength <= relaxed.natural_raw_strength + 1e-6, "{harsh:?} vs {relaxed:?}");
        if harsh.natural_raw_strength < relaxed.natural_raw_strength - 1e-4 {
            stricter += 1;
        }
        let mut mods = app.world_mut().resource_mut::<PlayerModifiers>();
        mods.jammer_radius_mult = 1.3;
        mods.wall_attenuation_exp = 1.2;
    }
    assert!(stricter >= 1, "Roaming Charges lowered the signal somewhere");
}

#[test]
fn low_battery_mode_skips_floor_heal_and_sos_telemetry_counts() {
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, contract_loadout(&[CarrierContract::LowBatteryMode]), 7);
    fast_clear_floor(&mut app);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let run = run_stats(&app);
    assert_eq!((run.sos_completed, run.floors_cleared), (1, 1));
    with_player(&mut app, |p| {
        p.hp = 40.0;
        p.armor = 0.0;
        p.ammo_shells = 0;
    });
    bot(&mut app).pick_upgrade = Some(0);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    let p = player(&mut app);
    assert_eq!(p.hp, 40.0, "Low-Battery Mode: no HP field repair");
    assert!(p.armor >= 15.0, "armor still patched");
    assert!(p.ammo_shells >= p.max_shells / 2, "ammo still topped up");
}

#[test]
fn signal_spoofer_dash_spoofs_four_bars_and_lights_the_oled() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::SignalSpoofer);
    find_blackout(&mut app);
    assert_eq!(signal(&app).bars, 0);
    bot(&mut app).dash = true;
    step(&mut app, 2);
    let s = signal(&app);
    assert_eq!(s.bars, 4, "spoofed to 4 bars: {s:?}");
    assert!(s.spoofed && !s.in_dead_zone);
    assert!(s.natural_raw_strength < 4.0, "natural signal untouched ({})", s.natural_raw_strength);
    assert!(app.world().resource::<PhoneApps>().spoof_timer > SIGNAL_SPOOF_SECS - 0.1);
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text.contains("SPOOF")));
    // OLED status bar shows "SPOOF" in bright cyan.
    step(&mut app, 4);
    let tex = app.world().resource::<PhoneScreenTexture>().image.clone();
    let img = app.world().resource::<Assets<Image>>().get(&tex).unwrap();
    let data = img.data.as_ref().unwrap();
    let cyan_px = (6..15u32)
        .flat_map(|y| (6..56u32).map(move |x| ((y * SCREEN_W + x) * 4) as usize))
        .filter(|&i| data[i..i + 3] == SPOOF_CYAN)
        .count();
    assert!(cyan_px > 10, "SPOOF label lit on the phone OLED ({cyan_px} px)");

    // The window expires after 2.5 s.
    step(&mut app, (SIGNAL_SPOOF_SECS * FPS as f32) as usize);
    let s = signal(&app);
    assert!(!s.spoofed);
    assert_eq!(s.bars, bars_from_strength(s.raw_strength));
    assert_eq!(app.world().resource::<PhoneApps>().spoof_timer, 0.0);

    // Dashing with healthy signal spoofs nothing.
    let g = grid(&app);
    teleport(&mut app, g.cell_center(ring_cell(&g)), g.tower_world());
    step(&mut app, 3 * FPS);
    bot(&mut app).dash = true;
    step(&mut app, 2);
    assert_eq!(app.world().resource::<PhoneApps>().spoof_timer, 0.0, "no spoof at 5 bars");
}

#[test]
fn jammer_hijack_turns_kills_into_five_bar_hotspots() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::JammerHijack);
    let dz = find_blackout(&mut app);
    assert_eq!(signal(&app).bars, 0);
    let g = grid(&app);

    // A Jammer Fiend killed in the blackout leaves a hotspot under the player.
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, dz));
    step(&mut app, 1);
    kill(&mut app, fiend);
    step(&mut app, 3);
    assert_eq!(count::<HijackedHotspot>(&mut app), 1);
    let s = signal(&app);
    assert_eq!(s.bars, 5, "hijacked hotspot: 5 bars ({s:?})");
    assert!(s.hotspot_boosted && !s.in_dead_zone);
    assert_eq!(bars_from_strength(s.natural_raw_strength), 0, "natural blackout preserved");
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text == "JAMMER HIJACKED - 5-BAR HOTSPOT!"));
    let run = run_stats(&app);
    assert_eq!((run.fiends_killed, run.blackout_kills), (1, 1), "feat telemetry");

    // It expires after 8 s.
    step(&mut app, (HIJACK_SECS * FPS as f32) as usize + 5);
    assert_eq!(count::<HijackedHotspot>(&mut app), 0);
    assert!(signal(&app).bars <= 1);

    // Destroying a Demon Jammer hijacks it too.
    let jammer = {
        let mut q = app.world_mut().query_filtered::<Entity, With<DemonJammer>>();
        q.iter(app.world()).next().unwrap()
    };
    app.world_mut().get_mut::<DemonJammer>(jammer).unwrap().hp = 0.0;
    step(&mut app, 2);
    assert_eq!(count::<HijackedHotspot>(&mut app), 1);
    assert_eq!(run_stats(&app).jammers_destroyed, 1);

    // Without the app: no hotspot.
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let dz = find_blackout(&mut app);
    let g = grid(&app);
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, dz));
    step(&mut app, 1);
    kill(&mut app, fiend);
    step(&mut app, 3);
    assert_eq!(count::<HijackedHotspot>(&mut app), 0);
}

#[test]
fn app_store_drafts_only_from_the_active_catalog() {
    let catalog = vec![UpgradeId::SignalSpoofer, UpgradeId::JammerHijack, UpgradeId::CopperAntenna, UpgradeId::MissedCall];
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, RunLoadout { catalog: catalog.clone(), ..default() }, 3);
    fast_clear_floor(&mut app);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let choices = app.world().resource::<UpgradeChoices>().0.clone();
    assert_eq!(choices.len(), 3);
    assert!(choices.iter().all(|c| catalog.contains(c)), "{choices:?} outside {catalog:?}");

    // Default run: the 10 core apps only.
    let mut app = build_headless_test_app();
    begin_run(&mut app, 3);
    fast_clear_floor(&mut app);
    let choices = app.world().resource::<UpgradeChoices>().0.clone();
    assert!(choices.iter().all(|c| c.is_core()), "{choices:?}");
}

#[test]
fn handset_skin_styles_the_phone_viewmodel() {
    for skin in HandsetSkin::ALL {
        let mut app = build_headless_test_app();
        begin_run_with(&mut app, RunLoadout { skin, ..default() }, 5);
        let palette = skin.palette();
        let mut q = app.world_mut().query::<&PhoneSkin>();
        assert_eq!(q.iter(app.world()).map(|s| s.0).collect::<Vec<_>>(), vec![skin]);
        let mut q = app.world_mut().query::<(&PhoneSkinPart, &MeshMaterial3d<StandardMaterial>)>();
        let parts: Vec<(SkinSlot, Handle<StandardMaterial>)> = q.iter(app.world()).map(|(p, m)| (p.0, m.0.clone())).collect();
        // Bezel + frame + 4 bumpers + 4 bolts + PTT + coil.
        assert_eq!(parts.len(), 12, "{skin:?}");
        let mats = app.world().resource::<Assets<StandardMaterial>>();
        for (slot, h) in parts {
            let m = mats.get(&h).unwrap();
            let want = palette.slot(slot);
            assert_eq!(m.base_color, want.color, "{skin:?} {slot:?}");
            assert_eq!((m.perceptual_roughness, m.metallic), (want.roughness, want.metallic));
        }
    }
}

#[test]
fn feats_unlock_on_game_over_and_victory() {
    let mut app = build_headless_test_app();
    assert!(profile(&app).path.is_none(), "headless runs never touch the disk");

    // Run 1: die on Floor 1 -> recorded, nothing unlocked.
    begin_run(&mut app, 7);
    die(&mut app);
    step(&mut app, 5);
    let st = profile(&app);
    assert_eq!(st.profile.stats.runs_played, 1, "recorded exactly once");
    assert!(st.last_run_summary.as_ref().unwrap().is_empty());
    assert!(run_stats(&app).run_recorded);
    back_to_menu(&mut app);

    // Run 2: extract Floor 1, die on Floor 2 -> First Uplink.
    begin_run(&mut app, 8);
    assert!(!run_stats(&app).run_recorded);
    fast_clear_floor(&mut app);
    bot(&mut app).pick_upgrade = Some(0);
    step(&mut app, 3);
    die(&mut app);
    let st = profile(&app);
    let sum = st.last_run_summary.clone().unwrap();
    assert_eq!(sum.new_feats, vec![FeatId::FirstExtraction]);
    assert_eq!(sum.new_skins, vec![HandsetSkin::MarsHazard]);
    assert_eq!(sum.new_apps, vec![UpgradeId::MissedCall]);
    assert!(st.profile.is_skin_unlocked(HandsetSkin::MarsHazard));
    assert!(st.profile.is_upgrade_unlocked(UpgradeId::MissedCall));
    assert_eq!(st.profile.stats.total_sos_completed, 1);
    assert_eq!(st.profile.stats.best_floor, 2);
    assert_eq!(st.profile.history.len(), 2);
    back_to_menu(&mut app);
    assert_eq!(*app.world().resource::<RunLoadout>(), RunLoadout::default(), "menu clears the run loadout");

    // Run 3: all four contracts, five fast floors -> victory feats.
    begin_run_with(&mut app, contract_loadout(&CarrierContract::ALL), 9);
    for floor in 1..=5 {
        fast_clear_floor(&mut app);
        if floor < 5 {
            assert_eq!(app_state(&app), AppState::UpgradeSelect);
            bot(&mut app).pick_upgrade = Some(0);
            step(&mut app, 3);
        }
    }
    assert_eq!(app_state(&app), AppState::Victory);
    step(&mut app, 5);
    let st = profile(&app);
    let sum = st.last_run_summary.clone().unwrap();
    assert!(sum.victory);
    for f in [FeatId::BunkerCleared, FeatId::ContractCleared, FeatId::AllContractsCleared, FeatId::SpeedExtraction] {
        assert!(sum.new_feats.contains(&f), "{f:?} in {:?}", sum.new_feats);
    }
    assert!(sum.new_skins.contains(&HandsetSkin::StealthCarbon) && sum.new_skins.contains(&HandsetSkin::ChromeGold));
    assert!(sum.new_chips.contains(&StarterChip::OverclockRadio));
    assert!(sum.new_chips.contains(&StarterChip::SurveyOs), "reached Floor 3 -> Signal Scout");
    assert!(sum.new_chips.contains(&StarterChip::FaradayCase), "6 lifetime SOS uplinks -> Faraday Tested");
    let s = &st.profile.stats;
    assert_eq!((s.runs_played, s.victories, s.max_contracts_cleared), (3, 1, 4));
    assert!(s.fastest_victory_secs.unwrap() < 600.0);
    assert_eq!(st.profile.history.last().unwrap().contracts_count, 4);
    assert!(st.profile.history.last().unwrap().victory);
}

// ---------------------------------------------------------------------------
// Milestone B: pre-run menu flow (MainMenu / Loadout / Contracts & Feats /
// end screens) through the real HUD + meta UI plugins and keyboard input
// ---------------------------------------------------------------------------

/// Headless gameplay + the HUD / menu / meta UI plugins (no window: cursor
/// clicks are inert, keyboard shortcuts drive the menus). `store` replaces the
/// default in-memory profile.
fn build_menu_app(store: Option<ProfileStore>) -> App {
    let mut app = App::new();
    if let Some(s) = store {
        app.insert_resource(s);
    }
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    app
}

/// Press and release a key through the real input pipeline, then let the
/// state transition settle.
fn tap(app: &mut App, key: KeyCode) {
    for state in [ButtonState::Pressed, ButtonState::Released] {
        app.world_mut().write_message(KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
    }
    step(app, 2);
}

fn ui_texts(app: &mut App) -> Vec<String> {
    let mut q = app.world_mut().query::<&Text>();
    q.iter(app.world()).map(|t| t.0.clone()).collect()
}

fn edit(app: &mut App, t: MetaTarget) -> EditOutcome {
    app.world_mut().run_system_once(move |mut store: ResMut<ProfileStore>| commit_meta_edit(&mut store, t)).unwrap()
}

fn temp_profile(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-profiles").join(name);
    let _ = std::fs::remove_dir_all(&d);
    d.join("profile.json")
}

#[test]
fn menu_navigates_loadout_and_contracts_screens() {
    let mut app = build_menu_app(None);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert!(ui_texts(&mut app).iter().any(|t| t.starts_with("OS: STOCK OS")), "loadout summary bar");
    assert_eq!(count::<Camera3d>(&mut app), 1);

    tap(&mut app, KeyCode::KeyL);
    assert_eq!(app_state(&app), AppState::Loadout);
    // 4 chips + 4 skins + 15 apps + jam toggle + reset button.
    assert_eq!(count::<MetaCard>(&mut app), 4 + 4 + UpgradeId::ALL.len() + 2);
    assert_eq!(count::<Camera3d>(&mut app), 1, "screen camera replaces the menu camera");
    assert!(ui_texts(&mut app).contains(&"12/12 READY".to_string()));

    tap(&mut app, KeyCode::KeyC);
    assert_eq!(app_state(&app), AppState::ContractsFeats);
    assert_eq!(count::<MetaCard>(&mut app), CarrierContract::ALL.len());
    assert!(ui_texts(&mut app).contains(&"STANDARD".to_string()));

    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
    tap(&mut app, KeyCode::KeyC);
    tap(&mut app, KeyCode::KeyL);
    assert_eq!(app_state(&app), AppState::Loadout);
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(count::<MetaCard>(&mut app), 0, "meta screens despawn on exit");
    assert_eq!(count::<Camera3d>(&mut app), 1);

    // Live refresh: a contract toggle updates the risk badge + card in place.
    tap(&mut app, KeyCode::KeyC);
    edit(&mut app, MetaTarget::Contract(CarrierContract::RoamingCharges));
    edit(&mut app, MetaTarget::Contract(CarrierContract::LowBatteryMode));
    step(&mut app, 1);
    let texts = ui_texts(&mut app);
    assert!(texts.contains(&"RISK x2".to_string()), "{texts:?}");
    assert_eq!(texts.iter().filter(|t| *t == "[ACTIVE]").count(), 2);
}

#[test]
fn game_over_to_loadout_redeploys_a_fresh_player_with_the_new_chip() {
    let mut store = ProfileStore::in_memory(Default::default());
    store.profile.set_jam_demo_unlock_all(true);
    let mut app = build_menu_app(Some(store));

    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 2);
    assert_eq!(player(&mut app).max_hp, 100.0, "stock chip");
    die(&mut app);
    // The end screen spawned after the run was recorded (Jam Demo -> exhibition).
    let st = profile(&app);
    let sum = st.last_run_summary.as_ref().unwrap();
    assert!(sum.is_empty() && sum.skipped_reason.as_deref().is_some_and(|r| r.contains("JAM DEMO")));
    assert_eq!(st.profile.stats.runs_played, 0, "Jam Demo runs never pollute real stats");
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("JAM DEMO")));
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("CONTRACTS: NONE")));

    tap(&mut app, KeyCode::KeyL);
    assert_eq!(app_state(&app), AppState::Loadout);
    assert_eq!(count::<Player>(&mut app), 0, "previous run's player torn down");
    assert!(app.world().get_resource::<LevelGrid>().is_none(), "previous level torn down");
    assert!(matches!(edit(&mut app, MetaTarget::Chip(StarterChip::SurveyOs)), EditOutcome::Changed(_)));
    edit(&mut app, MetaTarget::Contract(CarrierContract::PriorityJamming));

    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 3);
    assert_eq!(count::<Player>(&mut app), 1);
    let p = player(&mut app);
    assert_eq!((p.hp, p.max_hp), (85.0, 85.0), "fresh Survey OS vitals");
    let l = app.world().resource::<RunLoadout>().clone();
    assert_eq!(l.chip, StarterChip::SurveyOs);
    assert!(l.has_contract(CarrierContract::PriorityJamming));
    assert!(app.world().resource::<PlayerModifiers>().priority_jamming);
    let texts = ui_texts(&mut app);
    assert!(texts.contains(&"SURVEY OS".to_string()) && texts.contains(&"PRIO".to_string()), "HUD loadout tag: {texts:?}");

    // Victory -> Loadout -> Faraday Case: armor bonus on the fresh player.
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| {
            run.floor = FINAL_FLOOR;
            complete_floor(&mut run, &mut next);
        })
        .unwrap();
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Victory);
    tap(&mut app, KeyCode::KeyL);
    assert_eq!(count::<Player>(&mut app), 0);
    edit(&mut app, MetaTarget::Chip(StarterChip::FaradayCase));
    tap(&mut app, KeyCode::Enter);
    step(&mut app, 3);
    let p = player(&mut app);
    assert_eq!((p.max_hp, p.armor), (100.0, 50.0), "fresh Faraday Case vitals");
}

#[test]
fn victory_screen_lists_the_unlocks_recorded_on_entry() {
    let mut app = build_menu_app(None);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| {
            run.floor = FINAL_FLOOR;
            run.floors_cleared = FINAL_FLOOR - 1;
            complete_floor(&mut run, &mut next);
        })
        .unwrap();
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Victory);
    let sum = profile(&app).last_run_summary.unwrap();
    assert!(sum.new_feats.contains(&FeatId::BunkerCleared));
    let texts = ui_texts(&mut app);
    assert!(texts.iter().any(|t| t.starts_with("NEW UNLOCKS THIS RUN")), "{texts:?}");
    assert!(texts.contains(&"[DONE] Full Signal".to_string()));
    assert!(texts.iter().any(|t| t.starts_with("OS CHIPS:") && t.contains("Overclock Radio")));
    assert!(!texts.contains(&NO_UNLOCKS_LINE.to_string()));
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("FEATS: ") && !t.contains("FEATS: 0/")), "menu strip counts the new feats");
}

#[test]
fn loadout_and_contract_edits_save_the_profile_immediately() {
    let path = temp_profile("meta-ui-save");
    let mut app = build_menu_app(Some(ProfileStore::open(Some(path.clone()))));
    tap(&mut app, KeyCode::KeyL);
    assert!(!path.exists(), "nothing saved before the first edit");

    assert!(matches!(edit(&mut app, MetaTarget::JamDemo), EditOutcome::Changed(_)));
    assert!(load_profile(&path).jam_demo_unlock_all, "jam toggle saved");
    edit(&mut app, MetaTarget::Chip(StarterChip::OverclockRadio));
    edit(&mut app, MetaTarget::Skin(HandsetSkin::StealthCarbon));
    edit(&mut app, MetaTarget::App(UpgradeId::CopperAntenna));
    edit(&mut app, MetaTarget::App(UpgradeId::JammerHijack));
    let disk = load_profile(&path);
    assert_eq!(disk.equipped_chip, StarterChip::OverclockRadio);
    assert_eq!(disk.equipped_skin, HandsetSkin::StealthCarbon);
    assert!(disk.active_catalog.contains(&UpgradeId::JammerHijack) && !disk.active_catalog.contains(&UpgradeId::CopperAntenna));

    tap(&mut app, KeyCode::KeyC);
    edit(&mut app, MetaTarget::Contract(CarrierContract::OvertimeUplink));
    assert!(load_profile(&path).active_contracts.contains(&CarrierContract::OvertimeUplink), "contract saved");

    // Refused edits never write.
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(edit(&mut app, MetaTarget::App(UpgradeId::SignalSpoofer)), EditOutcome::Denied(_)), "catalog full");
    assert!(matches!(edit(&mut app, MetaTarget::Chip(StarterChip::OverclockRadio)), EditOutcome::Unchanged(_)));
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn f9_debug_skip_and_jam_demo_never_award_permanent_feats() {
    // 1. F9 debug skip across all 5 floors with all 4 contracts -> exhibition only.
    let mut app = build_menu_app(None);
    for c in CarrierContract::ALL {
        edit(&mut app, MetaTarget::Contract(c));
    }
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    for floor in 1..=FINAL_FLOOR {
        tap(&mut app, KeyCode::F9);
        if floor < FINAL_FLOOR {
            assert_eq!(app_state(&app), AppState::UpgradeSelect);
            bot(&mut app).pick_upgrade = Some(0);
            step(&mut app, 3);
            assert_eq!(app_state(&app), AppState::Playing);
        }
    }
    assert_eq!(app_state(&app), AppState::Victory);
    let st = profile(&app);
    assert!(st.profile.unlocked_feats.is_empty(), "F9 skip must never grant feats: {:?}", st.profile.unlocked_feats);
    assert_eq!(st.profile.stats.runs_played, 0);
    assert_eq!(st.profile.stats.victories, 0);
    let sum = st.last_run_summary.as_ref().unwrap();
    assert!(sum.skipped_reason.as_deref().is_some_and(|r| r.contains("Debug skip")));
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("Debug skip")));

    // 2. Jam Demo victory with all 4 contracts -> exhibition only.
    let mut app = build_menu_app(None);
    edit(&mut app, MetaTarget::JamDemo);
    for c in CarrierContract::ALL {
        edit(&mut app, MetaTarget::Contract(c));
    }
    tap(&mut app, KeyCode::Enter);
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| {
            run.floor = FINAL_FLOOR;
            run.floors_cleared = FINAL_FLOOR - 1;
            complete_floor(&mut run, &mut next);
        })
        .unwrap();
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Victory);
    let st = profile(&app);
    assert!(st.profile.unlocked_feats.is_empty(), "Jam Demo must not grant permanent feats");
    assert_eq!(st.profile.stats.victories, 0);
    assert!(st.last_run_summary.as_ref().unwrap().skipped_reason.as_deref().is_some_and(|r| r.contains("JAM DEMO")));
}

#[test]
fn signal_spoofer_cooldown_prevents_infinite_chaining() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    install(&mut app, UpgradeId::SignalSpoofer);
    find_blackout(&mut app);
    assert_eq!(signal(&app).bars, 0);

    // First dash in blackout triggers spoof + sets 6s cooldown.
    bot(&mut app).dash = true;
    step(&mut app, 2);
    assert_eq!(signal(&app).bars, 4);
    assert!(app.world().resource::<PhoneApps>().spoof_cooldown > 5.8);

    // Wait 2.1s: dash is ready again (2.0s), spoof is still live, re-dashing must NOT refresh it.
    step(&mut app, (2.1 * FPS as f32) as usize);
    let rem_before = app.world().resource::<PhoneApps>().spoof_timer;
    assert!(rem_before > 0.1 && rem_before < 0.6);
    bot(&mut app).dash = true;
    step(&mut app, 2);
    assert!(app.world().resource::<PhoneApps>().spoof_timer < rem_before, "re-dash cannot refresh while on cooldown");

    // Once spoof expires, player drops back to Dead Zone until the 6s cooldown clears.
    step(&mut app, (0.6 * FPS as f32) as usize);
    assert!(!signal(&app).spoofed);
    assert!(app.world().resource::<PhoneApps>().spoof_cooldown > 2.0);
    // Finish cooldown and verify spoofer rearms cleanly.
    step(&mut app, (3.6 * FPS as f32) as usize);
    assert_eq!(app.world().resource::<PhoneApps>().spoof_cooldown, 0.0);
    find_blackout(&mut app);
    bot(&mut app).dash = true;
    step(&mut app, 2);
    assert!(signal(&app).spoofed && signal(&app).bars == 4, "spoofer rearms after 6s cooldown");
}

#[test]
fn faraday_case_reduces_hazard_damage_in_dead_zone() {
    let mut app = build_headless_test_app();
    begin_run_with(&mut app, chip_loadout(StarterChip::FaradayCase), 41);
    despawn_all_enemies(&mut app);
    find_blackout(&mut app);
    assert!(signal(&app).in_dead_zone);
    with_player(&mut app, |p| {
        p.armor = 0.0;
        p.hp = 100.0;
        p.invuln_timer = 0.0;
        p.hazard_timer = -0.01;
    });
    app.world_mut().run_system_once(hazard_damage).unwrap();
    let lost_dz = 100.0 - player(&mut app).hp;
    assert!((lost_dz - 7.5).abs() < 1e-3, "Faraday reduces 10 hazard dmg -> 7.5 in Dead Zone (lost {lost_dz})");

    // Outside a Dead Zone (tower ring), hazard damage stays full 10.0.
    let g = grid(&app);
    teleport(&mut app, g.cell_center(ring_cell(&g)), g.tower_world());
    step(&mut app, 3);
    assert!(!signal(&app).in_dead_zone);
    with_player(&mut app, |p| {
        p.armor = 0.0;
        p.hp = 100.0;
        p.invuln_timer = 0.0;
        p.hazard_timer = -0.01;
    });
    app.world_mut().run_system_once(hazard_damage).unwrap();
    let lost_ring = 100.0 - player(&mut app).hp;
    assert!((lost_ring - 10.0).abs() < 1e-3, "Full 10 hazard dmg outside Dead Zone (lost {lost_ring})");
}

#[test]
fn all_starter_chips_preserve_weak_signal_opening_across_seeds() {
    for chip in StarterChip::ALL {
        let mut dead_zone_starts = 0;
        for seed in 1..=12u64 {
            let mut app = build_headless_test_app();
            begin_run_with(&mut app, chip_loadout(chip), seed);
            step(&mut app, 2);
            let s = signal(&app);
            assert!(s.bars <= 2, "{chip:?} seed {seed} opened too hot at {} bars", s.bars);
            if s.in_dead_zone {
                dead_zone_starts += 1;
            }
        }
        assert!(dead_zone_starts >= 8, "{chip:?} only had {dead_zone_starts}/12 weak-signal starts");
    }
}

#[test]
fn corrupt_profile_is_backed_up_and_dirty_save_retries() {
    let path = temp_profile("corrupt-backup");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let raw = r#"{"schema_version":999,"equipped_chip":"FutureOs"}"#;
    std::fs::write(&path, raw).unwrap();

    let store = ProfileStore::open(Some(path.clone()));
    assert_eq!(store.profile, PlayerProfile::default());
    assert!(store.recovery_notice.as_deref().is_some_and(|m| m.contains(".bak")), "{:?}", store.recovery_notice);
    let bak = path.with_file_name("profile.json.bak");
    assert!(bak.exists(), "original incompatible profile preserved at .bak");
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), raw);

    // Dirty save retry: point store at an unwriteable path (a directory), edit fails -> dirty=true;
    // then restore valid file path and retry without mutating again.
    let dir_as_file = path.parent().unwrap().join("blocked-dir");
    std::fs::create_dir_all(&dir_as_file).unwrap();
    let mut store = ProfileStore::open(Some(dir_as_file.clone()));
    let res = commit_meta_edit(&mut store, MetaTarget::Contract(CarrierContract::RoamingCharges));
    assert!(matches!(res, EditOutcome::Denied(m) if m.contains("SAVE FAILED")));
    assert!(store.dirty && store.last_save_error.is_some());
    assert!(store.profile.active_contracts.contains(&CarrierContract::RoamingCharges), "choice kept in memory");

    store.path = Some(path.clone());
    assert!(store.try_save().is_ok());
    assert!(!store.dirty && store.last_save_error.is_none());
    assert!(load_profile(&path).active_contracts.contains(&CarrierContract::RoamingCharges));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn pause_menu_freezes_simulation_and_resumes_or_restarts_cleanly() {
    let mut app = build_menu_app(None);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 10);
    let pos_before = player_pos(&mut app);
    let t_before = run_stats(&app).time_elapsed;
    assert!(t_before > 0.0);

    // Esc opens the Pause overlay and freezes run time while keeping the world alive.
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    assert_eq!(count::<Player>(&mut app), 1);
    assert!(ui_texts(&mut app).contains(&"PAUSED".to_string()));
    let t_paused = run_stats(&app).time_elapsed;
    step(&mut app, 30);
    assert_eq!(run_stats(&app).time_elapsed, t_paused, "simulation stays frozen while paused");

    // Esc resumes Playing at the exact same floor/position.
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Playing);
    assert!(player_pos(&mut app).distance(pos_before) < 0.25);
    step(&mut app, 6);
    assert!(run_stats(&app).time_elapsed > t_paused);

    // P also pauses; R restarts a fresh Floor 1 run with a fresh player.
    with_player(&mut app, |p| p.hp = 37.0);
    tap(&mut app, KeyCode::KeyP);
    assert_eq!(app_state(&app), AppState::Paused);
    tap(&mut app, KeyCode::KeyR);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 3);
    assert_eq!(count::<Player>(&mut app), 1);
    assert_eq!(player(&mut app).hp, 100.0, "restarted run spawns a fresh full-HP player");

    // Esc -> M returns to MainMenu and tears down the run cleanly.
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    tap(&mut app, KeyCode::KeyM);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(count::<Player>(&mut app), 0);
}

// ---------------------------------------------------------------------------
// Performance budgets (headless benchmark + regression guards)
// ---------------------------------------------------------------------------

/// Static level mesh entities the pre-batching renderer spawned for `g`: one
/// per visible wall, floor slab, ramp wedge, ceiling slab, pillar and lamp,
/// 16 signal-tower parts + beacon, and 5 mesh parts per Demon Jammer.
fn legacy_static_mesh_entities(g: &LevelGrid) -> usize {
    let mut n = 0;
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            match g.cell(c) {
                Cell::Wall => {
                    if (-1..=1).any(|dz| (-1..=1).any(|dx| !g.cell(c + IVec2::new(dx, dz)).is_wall())) {
                        n += 1;
                    }
                }
                cell => {
                    n += 1;
                    n += matches!(cell, Cell::Ramp { .. }) as usize;
                    n += !g.is_roof_open(c) as usize;
                }
            }
        }
    }
    n + (g.pillars.len() - 1) + g.lights.len() + 17 + 5 * g.jammers.len()
}

/// Static level mesh entities spawned now: level batches, the tower beacon
/// and the per-jammer meshes.
fn static_mesh_entities(app: &mut App) -> usize {
    let batches = count::<LevelBatch>(app);
    let mut q = app.world_mut().query_filtered::<&Children, With<DemonJammer>>();
    let jammer_meshes: usize = {
        let kids: Vec<Entity> = q.iter(app.world()).flat_map(|c| c.iter()).collect();
        kids.into_iter().filter(|e| app.world().get::<Mesh3d>(*e).is_some()).count()
    };
    batches + count::<TowerBeacon>(app) + jammer_meshes
}

fn descendants(app: &App, root: Entity) -> usize {
    let mut n = 0;
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if let Some(ch) = app.world().get::<Children>(e) {
            for c in ch.iter() {
                n += 1;
                stack.push(c);
            }
        }
    }
    n
}

fn material_count(app: &App) -> usize {
    app.world().resource::<Assets<StandardMaterial>>().len()
}

fn mesh_count(app: &App) -> usize {
    app.world().resource::<Assets<Mesh>>().len()
}

/// `(visible, total)` point lights (culled lights are `Visibility::Hidden`).
fn light_counts(app: &mut App) -> (usize, usize) {
    let mut q = app.world_mut().query_filtered::<&Visibility, With<PointLight>>();
    let all: Vec<Visibility> = q.iter(app.world()).copied().collect();
    (all.iter().filter(|v| **v != Visibility::Hidden).count(), all.len())
}

fn timed_step(app: &mut App, frames: usize, ticks: &mut Vec<f64>) {
    for _ in 0..frames {
        let t0 = std::time::Instant::now();
        app.update();
        ticks.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Squad fight in the open atrium: shotgun, plasma, Ping / Sub-GHz route,
/// 5G Overload (+ chain lightning), dash shockwave, then wipe the squad.
fn combat_round(app: &mut App, ticks: &mut Vec<f64>) -> f32 {
    let g = grid(app);
    let stand = ring_cell(&g);
    teleport(app, g.cell_center(stand), g.tower_world());
    step(app, 2);
    let atrium = g.rooms[0];
    let mut cells: Vec<IVec2> = (0..atrium.size.y)
        .flat_map(|z| (0..atrium.size.x).map(move |x| atrium.min + IVec2::new(x, z)))
        .filter(|c| g.cell(*c) == Cell::AtriumFloor && (*c - stand).abs().element_sum() >= 3)
        .collect();
    cells.sort_by_key(|c| (*c - stand).abs().element_sum());
    let kinds = [EnemyKind::Imp, EnemyKind::Cacodemon, EnemyKind::Imp, EnemyKind::JammerFiend, EnemyKind::Imp, EnemyKind::HellBaron];
    let squad: Vec<Entity> = kinds.iter().zip(cells.iter().step_by(2)).map(|(k, c)| spawn_demon(app, *k, spawn_pos_for(*k, &g, *c))).collect();
    step(app, 2);
    let mut child_total = 0;
    for &e in &squad {
        child_total += descendants(app, e);
    }
    let avg_children = child_total as f32 / squad.len() as f32;

    // Shotgun then plasma at whatever is still standing.
    let aim_living = |app: &mut App| {
        let target = squad.iter().copied().find(|e| enemy(app, *e).is_some_and(|en| en.is_alive()));
        if let Some(e) = target {
            let c = enemy_center(app, e);
            aim_at(app, c);
        }
    };
    bot(app).switch_weapon = Some(Weapon::Shotgun);
    bot(app).fire = true;
    for _ in 0..150 {
        godmode(app);
        aim_living(app);
        timed_step(app, 1, ticks);
    }
    bot(app).switch_weapon = Some(Weapon::Plasma);
    for _ in 0..120 {
        godmode(app);
        aim_living(app);
        with_player(app, |p| p.ammo_plasma = p.max_plasma);
        timed_step(app, 1, ticks);
    }
    bot(app).fire = false;

    // Phone apps: Ping (+ Sub-GHz route & x-ray beacons), Overload, dash.
    for _ in 0..2 {
        {
            let mut apps = app.world_mut().resource_mut::<PhoneApps>();
            apps.ping_cooldown = 0.0;
            apps.overload_cooldown = 0.0;
        }
        bot(app).ping = true;
        timed_step(app, 2, ticks);
        bot(app).overload = true;
        timed_step(app, 2, ticks);
        bot(app).dash = true;
        timed_step(app, 30, ticks);
    }
    // Route from the airlock too (long Sub-GHz breadcrumb trail).
    teleport(app, g.spawn_world(), g.tower_world());
    step(app, 2);
    app.world_mut().resource_mut::<PhoneApps>().ping_cooldown = 0.0;
    bot(app).ping = true;
    timed_step(app, 60, ticks);
    for &e in &squad {
        if enemy(app, e).is_some() {
            kill(app, e);
        }
    }
    godmode(app);
    timed_step(app, 90, ticks);
    avg_children
}

#[test]
fn perf_multi_floor_budgets_and_tick_timing() {
    let mut app = build_menu_app(None);

    // F3 / F4 work from the menu and in play; overlay off by default.
    assert!(!app.world().resource::<PerfSettings>().overlay, "telemetry overlay is off by default");
    assert!(!app.world().resource::<PerfSettings>().vsync, "uncapped (no VSync) by default");
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    let overlay_display = |app: &mut App| {
        let mut q = app.world_mut().query_filtered::<&Node, With<PerfOverlay>>();
        q.single(app.world()).unwrap().display
    };
    assert_eq!(overlay_display(&mut app), Display::None);
    tap(&mut app, KeyCode::F3);
    assert!(app.world().resource::<PerfSettings>().overlay);
    step(&mut app, 20);
    assert_eq!(overlay_display(&mut app), Display::Flex);
    let overlay_text = {
        let mut q = app.world_mut().query_filtered::<&Text, With<PerfOverlayText>>();
        q.single(app.world()).unwrap().0.clone()
    };
    assert!(overlay_text.contains("FPS") && overlay_text.contains("NO-VSYNC [F4]") && overlay_text.contains("LGT"), "{overlay_text}");
    tap(&mut app, KeyCode::F4);
    assert!(app.world().resource::<PerfSettings>().vsync);
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text == "VSYNC: ON"));
    tap(&mut app, KeyCode::F4);
    assert!(!app.world().resource::<PerfSettings>().vsync);
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text == "VSYNC: OFF"));
    tap(&mut app, KeyCode::F3);
    step(&mut app, 2);
    assert_eq!(overlay_display(&mut app), Display::None, "F3 hides the overlay again");

    godmode(&mut app);
    step(&mut app, 10);
    let mats0 = material_count(&app);
    let fades0 = app.world().resource::<FxAssets>().fades.lock().unwrap().len();

    // Missed Call decoy at a natural blackout before the antenna lifts the floor.
    install(&mut app, UpgradeId::MissedCall);
    find_blackout(&mut app);
    bot(&mut app).ping = true;
    step(&mut app, 2);
    assert_eq!(count::<DecoyHandset>(&mut app), 1, "decoy deployed");
    step(&mut app, 3 * FPS);
    for id in [UpgradeId::CopperAntenna, UpgradeId::SubGhzPenetration, UpgradeId::EmpOvercharge, UpgradeId::AirplaneModePhase, UpgradeId::JammerHijack] {
        install(&mut app, id);
    }
    assert_eq!(material_count(&app), mats0, "decoy allocates no materials");

    let ea = app.world().resource::<EnemyAssets>().clone();
    let mut report = Vec::new();
    let mut static_meshes: Option<usize> = None;
    let mut ticks_all = Vec::new();
    loop {
        let g = grid(&app);
        let floor = g.floor;
        godmode(&mut app);
        step(&mut app, 3);
        // Assets<Mesh>: everything but this floor's own meshes is constant.
        let per_floor = count::<LevelBatch>(&mut app) + count::<DemonJammer>(&mut app) + count::<SignalRingFx>(&mut app);
        let fixed = mesh_count(&app) - per_floor;
        match static_meshes {
            None => static_meshes = Some(fixed),
            Some(s) => assert_eq!(fixed, s, "floor {floor}: Assets<Mesh> leaked across the floor transition"),
        }
        if [1, 3, 5].contains(&floor) {
            let legacy = legacy_static_mesh_entities(&g);
            let now = static_mesh_entities(&mut app);
            let (vis0, total0) = light_counts(&mut app);
            let mesh_before = mesh_count(&app);
            let mut ticks = Vec::new();
            let avg_children = combat_round(&mut app, &mut ticks);
            let (vis1, total1) = light_counts(&mut app);
            assert_eq!(material_count(&app), mats0, "floor {floor}: combat + abilities allocated StandardMaterials");
            assert!(now * 10 <= legacy, "floor {floor}: static mesh entities {now} vs legacy {legacy}");
            assert!(avg_children <= 10.0, "floor {floor}: {avg_children} child entities per demon");
            assert!(vis0 < total0, "floor {floor}: far lights are culled ({vis0}/{total0})");
            ticks.sort_by(f64::total_cmp);
            let avg = ticks.iter().sum::<f64>() / ticks.len() as f64;
            let p99 = ticks[(ticks.len() * 99 / 100).min(ticks.len() - 1)];
            report.push(format!(
                "floor {floor} ({}x{}): static level mesh entities {legacy} -> {now} ({} batches) | demon children avg {avg_children:.1} | \
                 point lights visible {vis0}/{total0} idle, {vis1}/{total1} after combat | Assets<Mesh> {mesh_before} -> {} | \
                 tick avg {avg:.3} ms p99 {p99:.3} ms over {} frames",
                g.width,
                g.depth,
                count::<LevelBatch>(&mut app),
                mesh_count(&app),
                ticks.len(),
            ));
            ticks_all.extend(ticks);
        }
        fast_clear_floor(&mut app);
        assert_eq!(material_count(&app), mats0, "floor {floor}: SOS / extraction allocated StandardMaterials");
        if floor == FINAL_FLOOR {
            assert_eq!(app_state(&app), AppState::Victory);
            break;
        }
        assert_eq!(app_state(&app), AppState::UpgradeSelect);
        bot(&mut app).pick_upgrade = Some(0);
        step(&mut app, 3);
        assert_eq!(app_state(&app), AppState::Playing);
    }
    ticks_all.sort_by(f64::total_cmp);
    let avg = ticks_all.iter().sum::<f64>() / ticks_all.len() as f64;
    let p99 = ticks_all[(ticks_all.len() * 99 / 100).min(ticks_all.len() - 1)];
    let fades = app.world().resource::<FxAssets>().fades.lock().unwrap().len();
    let parts: Vec<String> = EnemyKind::ALL
        .iter()
        .map(|k| format!("{:?} {} -> {}", k, ea.source_part_counts[k.index()], ea.models[k.index()].len()))
        .collect();
    println!("=== SEGNALE DEBOLE perf report (headless, {} fixed-step ticks) ===", ticks_all.len());
    println!("demon mesh parts (entities per model): {}", parts.join(", "));
    for line in &report {
        println!("{line}");
    }
    println!("Assets<StandardMaterial>: {mats0} at run start -> {} after 3 measured floors of combat, Ping/Sub-GHz/Overload/Decoy/dash and 5 floor transitions", material_count(&app));
    println!("fade palettes: {fades0} prewarmed -> {fades} after the run");
    println!("Assets<Mesh> outside per-floor geometry: {} (constant across transitions)", static_meshes.unwrap());
    println!("headless tick: avg {avg:.3} ms, p99 {p99:.3} ms");
    assert_eq!(fades, fades0, "every fade palette was prewarmed at startup");
    assert!(avg < 33.0, "headless tick avg {avg:.2} ms");

    // Cross-run lifecycle check: Victory -> MainMenu -> new run Floor 1 must
    // release every per-run mesh and material handle cleanly.
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
    step(&mut app, 6);
    assert_eq!(app_state(&app), AppState::MainMenu);
    tap(&mut app, KeyCode::Enter);
    step(&mut app, 6);
    assert_eq!(app_state(&app), AppState::Playing);
    let per_floor_run2 = count::<LevelBatch>(&mut app) + count::<DemonJammer>(&mut app) + count::<SignalRingFx>(&mut app);
    let fixed_run2 = mesh_count(&app) - per_floor_run2;
    assert_eq!(material_count(&app), mats0, "second run leaked StandardMaterials across MainMenu reset");
    assert_eq!(fixed_run2, static_meshes.unwrap(), "second run leaked Assets<Mesh> across MainMenu reset");
}

// ---------------------------------------------------------------------------
// Milestone 10: Signal-Mortar, rocket jumps, Doppler Overdrive
// ---------------------------------------------------------------------------

fn scroll(app: &mut App, y: f32) {
    use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
    use bevy::input::touch::TouchPhase;
    app.world_mut().write_message(MouseWheel { unit: MouseScrollUnit::Line, x: 0.0, y, window: Entity::PLACEHOLDER, phase: TouchPhase::Moved });
    app.update();
    step(app, 2);
}

fn weapon_state(app: &App) -> WeaponState {
    app.world().resource::<WeaponState>().clone()
}

/// Wait out the swap animation and fire cooldown.
fn settle_weapon(app: &mut App) {
    step(app, FPS);
}

fn fire_once(app: &mut App) {
    bot(app).fire = true;
    step(app, 1);
    bot(app).fire = false;
}

#[test]
fn three_weapon_deck_switches_with_digits_tab_and_scroll() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 31);
    godmode(&mut app);
    open_arena(&mut app);
    assert_eq!(weapon_state(&app).current, Weapon::Shotgun);
    assert_eq!(player(&mut app).ammo_rockets, 10);

    tap(&mut app, KeyCode::Digit3);
    let ws = weapon_state(&app);
    assert_eq!((ws.current, ws.previous), (Weapon::Rocket, Weapon::Shotgun));
    assert_eq!(ws.displayed(), Weapon::Shotgun, "outgoing shotgun is still lowering");
    settle_weapon(&mut app);
    assert_eq!(weapon_state(&app).displayed(), Weapon::Rocket);

    tap(&mut app, KeyCode::Digit2);
    assert_eq!(weapon_state(&app).current, Weapon::Plasma);
    assert_eq!(weapon_state(&app).displayed(), Weapon::Rocket, "the mortar lowers, not `other()`");
    settle_weapon(&mut app);
    tap(&mut app, KeyCode::Digit1);
    assert_eq!(weapon_state(&app).current, Weapon::Shotgun);
    settle_weapon(&mut app);

    // Tab cycles forward through all three.
    for want in [Weapon::Plasma, Weapon::Rocket, Weapon::Shotgun] {
        tap(&mut app, KeyCode::Tab);
        assert_eq!(weapon_state(&app).current, want);
        settle_weapon(&mut app);
    }
    // Wheel down = next, wheel up = prev.
    scroll(&mut app, -1.0);
    assert_eq!(weapon_state(&app).current, Weapon::Plasma);
    settle_weapon(&mut app);
    scroll(&mut app, 1.0);
    assert_eq!(weapon_state(&app).current, Weapon::Shotgun);
    settle_weapon(&mut app);
    scroll(&mut app, 1.0);
    assert_eq!(weapon_state(&app).current, Weapon::Rocket);
}

#[test]
fn empty_weapon_auto_swaps_to_the_next_loaded_one() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 32);
    godmode(&mut app);
    open_arena(&mut app);
    with_player(&mut app, |p| {
        p.ammo_shells = 0;
        p.ammo_plasma = 0;
        p.ammo_rockets = 3;
    });
    fire_once(&mut app);
    assert_eq!(weapon_state(&app).current, Weapon::Rocket, "skips the empty plasma");
    // No soft-lock trickle while rockets remain.
    step(&mut app, 3 * FPS);
    assert_eq!(player(&mut app).ammo_plasma, 0);
    with_player(&mut app, |p| p.ammo_rockets = 0);
    step(&mut app, 2 * FPS);
    assert!(player(&mut app).ammo_plasma > 0, "emergency trickle once every heavy weapon is dry");
}

/// Stunned Hell Baron at `pos` (huge HP, never attacks).
fn stunned_baron(app: &mut App, pos: Vec3) -> Entity {
    let e = spawn_demon(app, EnemyKind::HellBaron, pos);
    step(app, 1);
    let mut en = app.world_mut().get_mut::<Enemy>(e).unwrap();
    en.stun_timer = 1.0e3;
    en.hp = 5_000.0;
    e
}

#[test]
fn rocket_direct_hit_and_linear_splash_without_double_dip() {
    use segnale_debole::weapons::{ROCKET_DIRECT_DAMAGE, ROCKET_SPLASH_DAMAGE, ROCKET_SPLASH_RADIUS};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 33);
    godmode(&mut app);
    let c = open_arena(&mut app);
    tap(&mut app, KeyCode::Digit3);
    settle_weapon(&mut app);

    let direct = stunned_baron(&mut app, c + Vec3::new(0.0, 0.0, -14.0));
    let side = stunned_baron(&mut app, c + Vec3::new(4.0, 0.0, -14.0));
    let far = stunned_baron(&mut app, c + Vec3::new(-14.0, 0.0, -20.0));
    let aim = enemy_center(&app, direct);
    aim_at(&mut app, aim);
    step(&mut app, 2);
    let rockets_before = player(&mut app).ammo_rockets;
    fire_once(&mut app);
    assert_eq!(player(&mut app).ammo_rockets, rockets_before - 1);
    assert_eq!(count::<PlayerProjectile>(&mut app), 1, "one rocket in flight");
    step(&mut app, FPS);
    assert_eq!(count::<PlayerProjectile>(&mut app), 0, "rocket detonated");

    let lost = |app: &App, e: Entity| 5_000.0 - enemy(app, e).unwrap().hp;
    assert!((lost(&app, direct) - ROCKET_DIRECT_DAMAGE).abs() < 1e-2, "direct hit only, no splash double-dip: {}", lost(&app, direct));
    let side_lost = lost(&app, side);
    assert!(side_lost > 0.0 && side_lost < ROCKET_SPLASH_DAMAGE, "linear splash falloff: {side_lost}");
    assert_eq!(lost(&app, far), 0.0, "outside {ROCKET_SPLASH_RADIUS} m");
    // Player stood 14 m away: no rocket jump.
    assert_eq!(player(&mut app).hp, player(&mut app).max_hp);
}

/// Fire straight down at your feet and track the flight.
fn rocket_jump_straight_down(app: &mut App) -> (f32, f32) {
    let feet = player_feet(app);
    {
        let mut b = bot(app);
        b.look_pitch = Some(-1.5);
    }
    step(app, 2);
    let hp0 = player(app).hp;
    fire_once(app);
    let mut apex = feet.y;
    let mut left_ground = false;
    for _ in 0..FPS {
        step(app, 1);
        apex = apex.max(player_feet(app).y);
        left_ground |= !player(app).grounded && player(app).velocity.y > 3.0;
    }
    assert!(left_ground, "rocket jump detached from the ground");
    (apex - feet.y, hp0 - player(app).hp)
}

#[test]
fn rocket_jump_launches_with_forgiving_self_damage_and_respects_invuln() {
    use segnale_debole::physics::{GRAVITY, JUMP_VELOCITY};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 34);
    open_arena(&mut app);
    with_player(&mut app, |p| {
        p.armor = 0.0;
        p.hp = p.max_hp;
    });
    tap(&mut app, KeyCode::Digit3);
    settle_weapon(&mut app);
    let (apex, dmg) = rocket_jump_straight_down(&mut app);
    let plain_jump = JUMP_VELOCITY * JUMP_VELOCITY / (2.0 * GRAVITY);
    assert!(apex > plain_jump * 1.5, "rocket jump apex {apex:.2} m vs plain jump {plain_jump:.2} m");
    assert!(dmg > 3.0 && dmg <= 14.25 + 1e-3, "forgiving ~15% self-damage: {dmg}");

    // Land, then do it again while invulnerable: launched, unhurt.
    step(&mut app, 2 * FPS);
    assert!(player(&mut app).grounded);
    with_player(&mut app, |p| p.invuln_timer = 1.0e6);
    let (apex2, dmg2) = rocket_jump_straight_down(&mut app);
    assert!(apex2 > plain_jump * 1.5);
    assert_eq!(dmg2, 0.0);

    // Blast Thrusters: higher launch, half the self-damage.
    step(&mut app, 2 * FPS);
    with_player(&mut app, |p| {
        p.invuln_timer = 0.0;
        p.hp = p.max_hp;
        p.armor = 0.0;
    });
    install(&mut app, UpgradeId::BlastThrusters);
    let (apex3, dmg3) = rocket_jump_straight_down(&mut app);
    assert!(apex3 > apex * 1.3, "Blast Thrusters apex {apex3:.2} vs {apex:.2}");
    assert!(dmg3 < dmg * 0.6, "Blast Thrusters self-damage {dmg3} vs {dmg}");
}

#[test]
fn rocket_jump_behind_carries_momentum_into_air_strafing() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 35);
    godmode(&mut app);
    let c = open_arena(&mut app);
    tap(&mut app, KeyCode::Digit3);
    settle_weapon(&mut app);
    // Running east, fire at the floor just behind.
    let feet = player_feet(&mut app);
    teleport(&mut app, feet, c + Vec3::new(-1.6, 0.0, 0.0));
    step(&mut app, 2);
    with_player(&mut app, |p| p.velocity = Vec3::new(11.0, 0.0, 0.0));
    fire_once(&mut app);
    let mut peak = 0.0f32;
    let mut airborne_frames = 0;
    for _ in 0..30 {
        step(&mut app, 1);
        let p = player(&mut app);
        if !p.grounded {
            airborne_frames += 1;
            peak = peak.max(horizontal_speed(&p));
        }
    }
    assert!(airborne_frames > 10, "launched into the air ({airborne_frames} frames)");
    assert!(peak > MOVE_SPEED + 3.0, "radial push adds to the run: {peak:.1} m/s");
    assert!(player(&mut app).velocity.x > 0.0, "pushed forward, away from the blast");
}

#[test]
fn doppler_overdrive_captures_bonus_damage_at_fire_time() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 36);
    godmode(&mut app);
    let c = open_arena(&mut app);
    install(&mut app, UpgradeId::DopplerOverdrive);
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    settle_weapon(&mut app);

    let shoot_at_speed = |app: &mut App, speed: f32| -> f32 {
        teleport(app, c, c + Vec3::new(0.0, PLAYER_EYE_HEIGHT, -10.0));
        step(app, 20);
        with_player(app, |p| p.velocity = Vec3::new(speed, 0.0, 0.0));
        fire_once(app);
        let mut q = app.world_mut().query::<&PlayerProjectile>();
        let m = q.iter(app.world()).map(|b| b.damage_mult).next().expect("bolt");
        step(app, FPS);
        m
    };
    assert_eq!(shoot_at_speed(&mut app, 5.0), 1.0, "below 20 m/s: no bonus");
    assert!((shoot_at_speed(&mut app, 25.0) - 1.3).abs() < 1e-5, "above 20 m/s: +30%");
}

#[test]
fn high_capacity_cell_and_floor_patch_cover_rockets() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 37);
    install(&mut app, UpgradeId::HighCapacityCell);
    let p = player(&mut app);
    assert_eq!(p.max_rockets, 27);
    assert_eq!(UpgradeId::ALL.len(), 17);
    assert_eq!(segnale_debole::meta::default_catalog().len(), segnale_debole::meta::ACTIVE_CATALOG_SIZE);
    assert_eq!(segnale_debole::meta::ACTIVE_CATALOG_SIZE, 12);
}

// ---------------------------------------------------------------------------
// Milestone 11: signal jump pads, explosive barrels, sector themes
// ---------------------------------------------------------------------------

use segnale_debole::assets_gen::{FloorTheme, GameAssets};
use segnale_debole::level::{spawn_barrel, ExplosiveBarrel, JumpPad, JumpPadGlow};
use segnale_debole::weapons::{BARREL_DAMAGE, BARREL_SELF_DAMAGE_FRAC};

/// Remove every level jump pad and barrel (scenario isolation).
fn clear_props(app: &mut App) {
    let mut q = app.world_mut().query_filtered::<Entity, Or<(With<JumpPad>, With<ExplosiveBarrel>)>>();
    let ids: Vec<Entity> = q.iter(app.world()).collect();
    for e in ids {
        app.world_mut().entity_mut(e).despawn();
    }
}

fn spawn_test_barrel(app: &mut App, pos: Vec3) -> Entity {
    app.world_mut()
        .run_system_once(move |mut commands: Commands, a: Res<GameAssets>| spawn_barrel(&mut commands, &a, pos))
        .unwrap();
    let mut q = app.world_mut().query::<(Entity, &Transform, &ExplosiveBarrel)>();
    let e = q.iter(app.world()).find(|(_, tf, _)| tf.translation.distance(pos) < 1e-3).map(|(e, _, _)| e).expect("barrel spawned");
    step(app, 1);
    e
}

fn barrel_alive(app: &App, e: Entity) -> bool {
    app.world().get::<ExplosiveBarrel>(e).is_some()
}

fn pad_at(app: &mut App, pos: Vec3) -> (Entity, JumpPad) {
    let mut q = app.world_mut().query::<(Entity, &JumpPad)>();
    q.iter(app.world()).find(|(_, p)| p.pos.distance(pos) < 1e-3).map(|(e, p)| (e, p.clone())).expect("jump pad")
}

fn pad_glow_is(app: &mut App, pad: Entity, mat: &Handle<StandardMaterial>) -> bool {
    let children: Vec<Entity> = app.world().get::<Children>(pad).unwrap().iter().collect();
    children.into_iter().any(|c| {
        app.world().get::<JumpPadGlow>(c).is_some() && app.world().get::<MeshMaterial3d<StandardMaterial>>(c).is_some_and(|m| m.0 == *mat)
    })
}

#[test]
fn every_floor_spawns_its_jump_pads_and_barrels() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 2027);
    for floor in 1..=3u32 {
        let g = grid(&app);
        assert_eq!(g.floor, floor);
        assert_eq!(count::<JumpPad>(&mut app), g.jump_pads.len(), "floor {floor} pads");
        assert_eq!(count::<ExplosiveBarrel>(&mut app), g.barrels.len(), "floor {floor} barrels");
        assert!((2..=4).contains(&g.jump_pads.len()) && (4..=8).contains(&g.barrels.len()));
        // The camera fog and sky follow the sector theme.
        let spec = FloorTheme::for_floor(floor).spec();
        let mut fog = app.world_mut().query_filtered::<&bevy::pbr::DistanceFog, With<PlayerCamera>>();
        assert_eq!(fog.single(app.world()).unwrap().color, spec.fog_color, "floor {floor} fog");
        assert_eq!(app.world().resource::<ClearColor>().0, spec.sky_color, "floor {floor} sky");
        fast_clear_floor(&mut app);
        assert_eq!(app_state(&app), AppState::UpgradeSelect);
        bot(&mut app).pick_upgrade = Some(0);
        step(&mut app, 3);
    }
}

#[test]
fn jump_pad_dormant_under_jammer_then_powers_up_and_launches_onto_pedestal() {
    use segnale_debole::player::{JUMP_PAD_COOLDOWN, JUMP_PAD_EVAL_SECS};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 1234);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);
    let spec = g.jump_pads[0];
    assert_eq!((spec.cell - g.tower).abs().element_sum(), 2, "first pad sits by the tower pedestal");
    let pad_pos = g.cell_center(spec.cell);
    let (powered_mat, dormant_mat) = {
        let a = app.world().resource::<GameAssets>();
        (a.pad_powered.clone(), a.pad_dormant.clone())
    };
    step(&mut app, (JUMP_PAD_EVAL_SECS * 60.0) as usize + 2);
    let (pad, state) = pad_at(&mut app, pad_pos);
    assert!(state.powered, "5 bars inside the signal ring power the pad");
    assert!(pad_glow_is(&mut app, pad, &powered_mat));

    // A jammer parked on the pad drags it below 2 bars.
    let jam = app.world_mut().spawn((DemonJammer::for_floor(1), Transform::from_translation(pad_pos))).id();
    step(&mut app, (JUMP_PAD_EVAL_SECS * 60.0) as usize + 2);
    assert!(!pad_at(&mut app, pad_pos).1.powered, "jammed pad goes dormant");
    assert!(pad_glow_is(&mut app, pad, &dormant_mat));

    // Standing on the dormant pad: no launch, one rate-limited toast.
    teleport(&mut app, pad_pos, g.tower_world() + Vec3::Y * 2.0);
    step(&mut app, 10);
    let p = player(&mut app);
    assert!(p.grounded && p.velocity.y.abs() < 0.5, "dormant pad never launches: {:?}", p.velocity);
    let offline = |app: &App| app.world().resource::<HudFeed>().toasts.iter().filter(|t| t.text.contains("JUMP PAD OFFLINE")).count();
    assert_eq!(offline(&app), 1);
    step(&mut app, 30);
    assert_eq!(offline(&app), 1, "toast is rate-limited");

    // Clear the jammer: the pad powers up and launches the player standing on it.
    app.world_mut().get_mut::<DemonJammer>(jam).unwrap().hp = 0.0;
    let mut launched_at = None;
    for f in 0..30 {
        step(&mut app, 1);
        if player(&mut app).velocity.y > 10.0 {
            launched_at = Some(f);
            break;
        }
    }
    assert!(launched_at.is_some(), "cleared jammer powered the pad and it launched");
    assert!(pad_at(&mut app, pad_pos).1.powered);
    assert!(pad_glow_is(&mut app, pad, &powered_mat));
    let cd = pad_at(&mut app, pad_pos).1.cooldown;
    assert!(cd > 0.0 && cd <= JUMP_PAD_COOLDOWN, "cooldown armed: {cd}");
    assert!(!player(&mut app).grounded);
    let mut apex = 0.0f32;
    for _ in 0..(1.4 * FPS as f32) as usize {
        step(&mut app, 1);
        apex = apex.max(player_feet(&mut app).y);
    }
    let feet = player_feet(&mut app);
    assert!(apex > 3.5, "launched well above the pedestal: apex {apex}");
    assert!((feet.y - g.tower_world().y).abs() < 0.05 && player(&mut app).grounded, "landed on the pedestal: {feet}");
    assert_eq!(g.world_to_cell(feet).as_vec2().distance(g.tower.as_vec2()).round() as i32, 1, "on the pedestal ring");
}

#[test]
fn powered_jump_pad_keeps_a_fast_bhoppers_momentum() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 1234);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);
    let pad_pos = g.cell_center(g.jump_pads[0].cell);
    step(&mut app, 20);
    assert!(pad_at(&mut app, pad_pos).1.powered);
    // Mid-hop over the pad at 30 m/s, sideways to the pad's push.
    let push = g.jump_pads[0].launch_vel;
    let side = Vec3::new(-push.z, 0.0, push.x).normalize() * 30.0;
    teleport(&mut app, pad_pos + Vec3::Y * 0.3 - side.normalize() * 0.4, pad_pos + side * 5.0 + Vec3::Y * 1.6);
    with_player(&mut app, |p| {
        p.velocity = side;
        p.grounded = false;
    });
    step(&mut app, 1);
    let p = player(&mut app);
    assert!(p.velocity.y > 13.0, "launched: {:?}", p.velocity);
    assert!(horizontal_speed(&p) >= 29.5, "bhop speed kept: {}", horizontal_speed(&p));
    assert!(!p.grounded);
}

#[test]
fn shotgun_barrel_chain_reaction_hits_demons_and_jammers_in_line_of_sight() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 77);
    godmode(&mut app);
    let c = open_arena(&mut app);
    clear_props(&mut app);
    let a = spawn_test_barrel(&mut app, c + Vec3::new(0.0, 0.0, -7.0));
    let b = spawn_test_barrel(&mut app, c + Vec3::new(3.5, 0.0, -9.0));
    let far = spawn_test_barrel(&mut app, c + Vec3::new(-14.0, 0.0, -7.0));
    let baron = stunned_baron(&mut app, c + Vec3::new(-2.5, 0.0, -9.5));
    let jam = app.world_mut().spawn((DemonJammer::for_floor(1), Transform::from_translation(c + Vec3::new(6.5, 0.0, -10.0)))).id();
    step(&mut app, 2);
    let jam_hp0 = app.world().get::<DemonJammer>(jam).unwrap().hp;

    aim_at(&mut app, c + Vec3::new(0.0, 0.6, -7.0));
    step(&mut app, 2);
    fire_once(&mut app);
    step(&mut app, 1);
    assert!(!barrel_alive(&app, a), "one shotgun blast detonates a barrel");
    assert!(barrel_alive(&app, b), "neighbour waits out its staggered chain fuse");
    assert!(app.world().get::<ExplosiveBarrel>(b).unwrap().fuse.is_some(), "chain fuse armed");
    step(&mut app, 12);
    assert!(!barrel_alive(&app, b), "chain reaction");
    assert!(barrel_alive(&app, far), "out-of-range barrel untouched");
    let lost = 5_000.0 - enemy(&app, baron).unwrap().hp;
    assert!(lost > 0.0 && lost <= 2.0 * BARREL_DAMAGE, "baron caught in the blast: {lost}");
    assert!(app.world().get::<DemonJammer>(jam).unwrap().hp < jam_hp0, "jammer caught in the chain blast");
}

#[test]
fn barrel_blast_is_blocked_by_walls() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 78);
    godmode(&mut app);
    let wall = "#".repeat(18);
    let row = format!("#{}#", ".".repeat(16));
    let pillar = format!("#{}#{}#", ".".repeat(8), ".".repeat(7));
    let mut rows: Vec<&str> = vec![wall.as_str()];
    rows.extend(std::iter::repeat_n(row.as_str(), 6));
    rows.push(pillar.as_str()); // wall cell (9, 7): x 36..40, z 28..32
    rows.extend(std::iter::repeat_n(row.as_str(), 9));
    rows.push(wall.as_str());
    app.world_mut().insert_resource(LevelGrid::from_ascii(&rows));
    step(&mut app, 2);
    despawn_all_enemies(&mut app);
    clear_props(&mut app);
    teleport(&mut app, Vec3::new(10.0, 0.0, 62.0), Vec3::new(10.0, 1.6, 50.0));
    let barrel = spawn_test_barrel(&mut app, Vec3::new(38.0, 0.0, 27.0));
    let hidden = stunned_baron(&mut app, Vec3::new(38.0, 0.0, 33.0));
    let open = stunned_baron(&mut app, Vec3::new(43.0, 0.0, 27.0));
    step(&mut app, 2);
    app.world_mut().get_mut::<ExplosiveBarrel>(barrel).unwrap().fuse = Some(0.0);
    step(&mut app, 2);
    assert!(!barrel_alive(&app, barrel));
    assert_eq!(enemy(&app, hidden).unwrap().hp, 5_000.0, "wall blocks the blast");
    assert!(enemy(&app, open).unwrap().hp < 5_000.0, "open-air demon is hit");
}

#[test]
fn plasma_rocket_and_emp_all_detonate_barrels() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 79);
    godmode(&mut app);
    let c = open_arena(&mut app);
    clear_props(&mut app);

    // Plasma: two bolts.
    let p = spawn_test_barrel(&mut app, c + Vec3::new(0.0, 0.0, -10.0));
    bot(&mut app).switch_weapon = Some(Weapon::Plasma);
    settle_weapon(&mut app);
    aim_at(&mut app, c + Vec3::new(0.0, 0.6, -10.0));
    step(&mut app, 2);
    fire_once(&mut app);
    step(&mut app, 20);
    assert!(barrel_alive(&app, p), "one bolt is not enough");
    fire_once(&mut app);
    step(&mut app, 20);
    assert!(!barrel_alive(&app, p), "second bolt detonates it");

    // Rocket: direct hit.
    let r = spawn_test_barrel(&mut app, c + Vec3::new(10.0, 0.0, -10.0));
    bot(&mut app).switch_weapon = Some(Weapon::Rocket);
    settle_weapon(&mut app);
    aim_at(&mut app, c + Vec3::new(10.0, 0.6, -10.0));
    step(&mut app, 2);
    fire_once(&mut app);
    step(&mut app, FPS);
    assert!(!barrel_alive(&app, r), "rocket detonates it");

    // 5G Overload EMP (armed via Emergency Callback so the gate opens anywhere).
    let e = spawn_test_barrel(&mut app, c + Vec3::new(-5.0, 0.0, 3.0));
    app.world_mut().resource_mut::<PhoneApps>().callback_timer = CALLBACK_SECS;
    app.world_mut().resource_mut::<PhoneApps>().overload_cooldown = 0.0;
    bot(&mut app).overload = true;
    step(&mut app, 3);
    assert!(!barrel_alive(&app, e), "EMP cooks off the barrel");
}

#[test]
fn barrel_blast_jumps_the_player_with_forgiving_self_damage() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 80);
    let c = open_arena(&mut app);
    clear_props(&mut app);
    let barrel = spawn_test_barrel(&mut app, c + Vec3::new(1.2, 0.0, 0.0));
    let (hp0, armor0) = {
        let p = player(&mut app);
        (p.hp, p.armor)
    };
    app.world_mut().get_mut::<ExplosiveBarrel>(barrel).unwrap().fuse = Some(0.0);
    step(&mut app, 1);
    let p = player(&mut app);
    assert!(!barrel_alive(&app, barrel));
    assert!(p.velocity.y > 5.0 && !p.grounded, "blast jump: {:?}", p.velocity);
    assert!(p.velocity.x < -3.0, "pushed away from the barrel: {:?}", p.velocity);
    let taken = (hp0 - p.hp) + (armor0 - p.armor);
    assert!(taken > 0.0 && taken <= BARREL_DAMAGE * BARREL_SELF_DAMAGE_FRAC + 1e-3, "forgiving self damage: {taken}");
}

// ---------------------------------------------------------------------------
// Milestone 12: Settings screen, live user settings, Endless Overdrive
// ---------------------------------------------------------------------------

fn settings_edit(app: &mut App, t: SettingsTarget) -> EditOutcome {
    app.world_mut().run_system_once(move |mut store: ResMut<ProfileStore>| commit_settings_edit(&mut store, t)).unwrap()
}

fn hud_crosshair(app: &mut App) -> Option<HudCrosshair> {
    let mut q = app.world_mut().query::<&HudCrosshair>();
    q.iter(app.world()).next().copied()
}

/// Yaw change produced by `dx` pixels of real mouse motion this frame.
fn yaw_turn(app: &mut App, dx: f32) -> f32 {
    app.world_mut().resource_mut::<CursorCaptured>().0 = true;
    bot(app).look_yaw = None;
    let y0 = player(app).yaw;
    app.world_mut().write_message(MouseMotion { delta: Vec2::new(dx, 0.0) });
    step(app, 1);
    (player(app).yaw - y0).abs()
}

#[test]
fn settings_open_from_main_menu_and_mid_run_pause_and_apply_live() {
    let mut app = build_menu_app(None);
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("SETTINGS")), "main menu SETTINGS button");

    // Main menu -> Settings -> back to the main menu.
    tap(&mut app, KeyCode::KeyO);
    assert_eq!(app_state(&app), AppState::Settings);
    assert_eq!(app.world().resource::<SettingsReturn>().0, AppState::MainMenu);
    // 6 knobs x (-/+), 5 toggles (incl. SPEEDRUN SPLITS, GAMEPAD INVERT Y), the style pills, the color swatches, RESET.
    assert_eq!(count::<SettingsControl>(&mut app), 6 * 2 + 5 + CrosshairStyle::ALL.len() + CrosshairColor::ALL.len() + 1);
    assert!(ui_texts(&mut app).iter().any(|t| t == "SPEEDRUN SPLITS"), "speedrun splits toggle row");
    let texts = ui_texts(&mut app);
    assert!(texts.iter().any(|t| t == "GAMEPAD SENSITIVITY") && texts.iter().any(|t| t == "GAMEPAD INVERT Y"), "gamepad rows");
    assert_eq!(count::<ReticlePreview>(&mut app), 1);
    assert_eq!(count::<Camera3d>(&mut app), 1, "settings brings its own camera from the menu");
    let texts = ui_texts(&mut app);
    assert!(texts.iter().any(|t| t.contains("INPUT & CAMERA")) && texts.iter().any(|t| t.contains("AUDIO & RETICLE")), "{texts:?}");
    assert!(texts.iter().any(|t| t == "RESET DEFAULTS") && texts.iter().any(|t| t.contains("BACK  [ESC]")));
    for _ in 0..3 {
        assert!(matches!(settings_edit(&mut app, SettingsTarget::Up(SettingKnob::BaseFov)), EditOutcome::Changed(_)));
    }
    settings_edit(&mut app, SettingsTarget::Style(CrosshairStyle::Dot));
    step(&mut app, 2);
    let preview = {
        let mut q = app.world_mut().query::<&ReticlePreview>();
        *q.iter(app.world()).next().unwrap()
    };
    assert_eq!(preview.0, CrosshairStyle::Dot, "live preview rebuilt");
    assert!(ui_texts(&mut app).contains(&"100 DEG".to_string()));
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(count::<SettingsControl>(&mut app), 0);
    assert_eq!(count::<Camera3d>(&mut app), 1);

    // Deploy: the camera spawns at the user's base FOV with the chosen reticle.
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 30);
    assert!((camera_fov_deg(&mut app) - 100.0).abs() < 0.5, "fov {}", camera_fov_deg(&mut app));
    assert_eq!(hud_crosshair(&mut app).unwrap().0, CrosshairStyle::Dot);
    let base_turn = yaw_turn(&mut app, 100.0);
    assert!((base_turn - 100.0 * MOUSE_SENSITIVITY).abs() < 1e-4, "turn {base_turn}");

    // Mid-run: Pause -> Settings keeps the run alive, then back to the game.
    with_player(&mut app, |p| p.hp = 61.0);
    let floor_seed = grid(&app).seed;
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    assert!(ui_texts(&mut app).iter().any(|t| t == "SETTINGS  [O]"), "pause menu SETTINGS button");
    let run_cameras = count::<Camera3d>(&mut app);
    tap(&mut app, KeyCode::KeyO);
    assert_eq!(app_state(&app), AppState::Settings);
    assert_eq!(app.world().resource::<SettingsReturn>().0, AppState::Paused);
    assert_eq!(count::<Player>(&mut app), 1, "Settings never tears the run down");
    assert_eq!(grid(&app).seed, floor_seed, "level kept");
    assert_eq!(count::<Camera3d>(&mut app), run_cameras, "no extra camera over the frozen player + viewmodel cameras");
    assert!(ui_texts(&mut app).iter().any(|t| t.contains("MID-RUN")));
    for _ in 0..6 {
        settings_edit(&mut app, SettingsTarget::Down(SettingKnob::BaseFov));
    }
    for _ in 0..20 {
        settings_edit(&mut app, SettingsTarget::Up(SettingKnob::Sensitivity));
    }
    settings_edit(&mut app, SettingsTarget::Style(CrosshairStyle::Chevron));
    settings_edit(&mut app, SettingsTarget::Color(CrosshairColor::Amber));
    settings_edit(&mut app, SettingsTarget::Toggle(SettingToggle::Mute));
    let s = profile(&app).profile.settings;
    assert_eq!((s.base_fov_deg, s.mouse_sensitivity, s.audio_muted), (70.0, 2.0, true));
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    assert_eq!(player(&mut app).hp, 61.0, "vitals untouched");
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(grid(&app).seed, floor_seed, "same floor resumed");
    step(&mut app, 60);
    assert!((camera_fov_deg(&mut app) - 70.0).abs() < 0.5, "new base FOV live: {}", camera_fov_deg(&mut app));
    assert_eq!(hud_crosshair(&mut app), Some(HudCrosshair(CrosshairStyle::Chevron, CrosshairColor::Amber)), "HUD reticle rebuilt");
    let fast_turn = yaw_turn(&mut app, 100.0);
    assert!((fast_turn - 2.0 * base_turn).abs() < 1e-4, "sensitivity x2: {fast_turn} vs {base_turn}");

    // Reset defaults through Pause -> Settings again.
    tap(&mut app, KeyCode::Escape);
    tap(&mut app, KeyCode::KeyO);
    assert!(matches!(settings_edit(&mut app, SettingsTarget::ResetDefaults), EditOutcome::Changed(_)));
    assert_eq!(profile(&app).profile.settings, UserSettings::default());
}

#[test]
fn speed_fov_and_strafe_roll_toggles_flatten_the_camera() {
    let mut store = ProfileStore::in_memory(Default::default());
    store.profile.settings.speed_fov_enabled = false;
    store.profile.settings.strafe_roll_enabled = false;
    let mut app = build_headless_test_app();
    app.world_mut().insert_resource(store);
    begin_run(&mut app, 5);
    open_arena(&mut app);
    godmode(&mut app);
    // Dash + hold right: normally widens the FOV and rolls the camera.
    bot(&mut app).strafe_axes = Vec2::new(1.0, 0.0);
    bot(&mut app).dash = true;
    step(&mut app, 8);
    assert!((camera_fov_deg(&mut app) - BASE_FOV_DEG).abs() < 0.05, "speed FOV disabled: {}", camera_fov_deg(&mut app));
    assert_eq!(player(&mut app).strafe_tilt, 0.0, "strafe roll disabled");
    assert!(run_stats(&app).peak_speed > MOVE_SPEED, "dash speed tracked: {}", run_stats(&app).peak_speed);
}

#[test]
fn endless_overdrive_continues_past_victory_and_records_once() {
    let mut app = build_menu_app(None);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 2);
    // Floors 1-5 exactly as before: Floor 5 ends in Victory.
    for floor in 1..=FINAL_FLOOR {
        assert_eq!(grid(&app).floor, floor);
        if floor == FINAL_FLOOR {
            // Boss plate + 99% hold status on the HUD.
            godmode(&mut app);
            trigger_uplink(&mut app);
            if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
                *progress = 0.995;
            }
            step(&mut app, 3);
            let texts = ui_texts(&mut app);
            assert!(texts.iter().any(|t| t == "THE CARRIER DAEMON"), "boss plate header");
            assert!(texts.iter().any(|t| t == "PHASE I"), "boss phase tag");
            assert!(texts.iter().any(|t| t == "DEFEAT THE CARRIER DAEMON TO LOCK EXTRACTION"), "hold status: {texts:?}");
            assert!(texts.iter().any(|t| t.starts_with("UPLINK HELD 99%")));
        }
        fast_clear_floor(&mut app);
        if floor < FINAL_FLOOR {
            assert_eq!(app_state(&app), AppState::UpgradeSelect);
            bot(&mut app).pick_upgrade = Some(0);
            step(&mut app, 3);
        }
    }
    assert_eq!(app_state(&app), AppState::Victory);
    assert!(is_boss_floor(FINAL_FLOOR));
    let st = profile(&app);
    assert_eq!((st.profile.stats.runs_played, st.profile.stats.victories), (1, 1));
    let kills_at_victory = run_stats(&app).kills;
    assert_eq!(st.profile.stats.total_kills, kills_at_victory);
    assert!(ui_texts(&mut app).iter().any(|t| t == ENDLESS_CTA), "KEEP DESCENDING button");
    assert!(ui_texts(&mut app).iter().any(|t| t.starts_with("PEAK SPEED")), "movement telemetry on Victory");
    assert_eq!(count::<Player>(&mut app), 1, "the player survives Victory");
    with_player(&mut app, |p| p.hp = 40.0);
    let upgrades = run_stats(&app).upgrades.len();

    // E: keep descending -> Floor 5 -> 6 upgrade draft -> Floor 6.
    tap(&mut app, KeyCode::KeyE);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let r = run_stats(&app);
    assert!(r.endless && !r.run_recorded && r.recorded_snapshot.is_some());
    assert_eq!(r.floor, FINAL_FLOOR + 1);
    bot(&mut app).pick_upgrade = Some(1);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(grid(&app).floor, 6);
    assert_eq!(run_stats(&app).upgrades.len(), upgrades + 1, "upgrades kept + one more");
    assert_eq!(player(&mut app).hp, 65.0, "HP carried over (+25 field repair)");
    step(&mut app, 2);
    assert!(ui_texts(&mut app).iter().any(|t| t == "ENDLESS FL 6"), "endless floor plate");
    assert_eq!(count::<DemonJammer>(&mut app), jammer_count_for_floor(6));

    // Floors 6 -> 7 -> 8 never re-trigger Victory.
    for floor in 6..=7 {
        fast_clear_floor(&mut app);
        assert_eq!(app_state(&app), AppState::UpgradeSelect, "floor {floor} advances");
        bot(&mut app).pick_upgrade = Some(0);
        step(&mut app, 3);
        assert_eq!(grid(&app).floor, floor + 1);
    }
    assert_eq!(profile(&app).profile.stats.victories, 1);
    // Some kills after Floor 5, then die on Floor 8.
    let far = player_feet(&mut app) + Vec3::new(30.0, 0.0, 0.0);
    let e = spawn_demon(&mut app, EnemyKind::Imp, far);
    kill(&mut app, e);
    step(&mut app, 3);
    with_player(&mut app, |p| p.velocity = Vec3::new(36.0, 0.0, 0.0));
    step(&mut app, 1);
    die(&mut app);
    let r = run_stats(&app);
    assert!(r.kills > kills_at_victory, "post-victory kills: {} vs {kills_at_victory}", r.kills);
    let s = profile(&app).profile.stats;
    assert_eq!(s.runs_played, 1, "Endless never double-counts the run");
    assert_eq!(s.victories, 1);
    assert_eq!(s.total_kills, r.kills, "Floor 1-5 kills counted exactly once");
    assert_eq!(s.total_sos_completed, r.sos_completed);
    assert_eq!((s.best_floor, s.best_endless_floor), (8, 8));
    assert_eq!(s.best_speed, r.peak_speed);
    assert!(s.best_speed > 20.0, "peak speed recorded: {}", s.best_speed);
    assert_eq!(s.best_bhop_chain, r.max_bhop_chain);
    let h = profile(&app).profile.history;
    assert_eq!(h.len(), 1);
    assert!(h[0].endless && h[0].victory && h[0].floor == 8);
    assert!(ui_texts(&mut app).iter().any(|t| t.starts_with("ENDLESS FL 8")), "Game Over shows the endless floor");
}

/// Jump the run to `floor` (endless past 5) and rebuild through Paused -> Playing.
fn jump_to_floor(app: &mut App, floor: u32) {
    {
        let mut run = app.world_mut().resource_mut::<RunStats>();
        run.endless = floor > FINAL_FLOOR;
        run.floor = floor;
    }
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
    step(app, 2);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
    step(app, 3);
    assert_eq!(grid(app).floor, floor);
}

fn boss_hud(app: &App) -> BossHud {
    app.world().resource::<BossHud>().clone()
}

#[test]
fn endless_floor10_boss_rematch_is_the_carrier_daemon() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 77);
    jump_to_floor(&mut app, 10);
    let g = grid(&app);
    assert!(g.validate() && is_boss_floor(10));
    assert_eq!(count::<DemonJammer>(&mut app), jammer_count_for_floor(10));
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    trigger_uplink(&mut app);
    step(&mut app, 2);
    let alert = app.world().resource::<HudFeed>().alert.clone().map(|a| a.text).unwrap_or_default();
    assert_eq!(alert, BOSS_ARRIVAL_TEXT);
    let boss = carrier(&mut app).expect("Carrier Daemon on Endless Floor 10");
    assert_eq!(enemy(&app, boss).unwrap().max_hp, 2200.0);
    let atrium = g.rooms[0];
    assert!(atrium.contains(g.world_to_cell(app.world().get::<Transform>(boss).unwrap().translation)), "lands in the Signal Atrium");
    // Past 50%: no Hell Baron / rematch Baron wave on a boss floor.
    if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
        *progress = 0.6;
    }
    step(&mut app, 3);
    let barons = {
        let mut q = app.world_mut().query::<&Enemy>();
        q.iter(app.world()).filter(|e| e.kind == EnemyKind::HellBaron && e.is_alive()).count()
    };
    assert_eq!(barons, 0, "the Carrier Daemon replaces the Hell Barons");
    let h = boss_hud(&app);
    assert!(h.active && h.phase == BossPhase::Phase1 && h.tag() == "PHASE I");
    // Holds at 99%, then completes Floor 10 once the boss falls.
    boss_floor_gate(&mut app);
    let mut guard = 0;
    while app_state(&app) == AppState::Playing {
        godmode(&mut app);
        step(&mut app, 1);
        guard += 1;
        assert!(guard < 5 * FPS);
    }
    assert_eq!(app_state(&app), AppState::UpgradeSelect, "Floor 10 completes into the next draft");
    assert!(EnemyKind::HellBaron.stats(35).fire_cooldown >= 1.0);
    assert!(rush_cap(30) <= 26);
}

/// Floor 5: the full Carrier Daemon fight through production systems - both
/// Signal Shields, pylons destroyed by real weapon fire (shotgun + rocket) and
/// raw damage, stuns, escort, Overdrive, death, then orbital extraction.
#[test]
fn carrier_daemon_two_shield_phases_lock_extraction_until_killed() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 2026);
    jump_to_floor(&mut app, FINAL_FLOOR);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    trigger_uplink(&mut app);
    step(&mut app, 2);
    let boss = carrier(&mut app).expect("Carrier Daemon spawned on Floor 5");
    let max = enemy(&app, boss).unwrap().max_hp;
    assert_eq!(max, 1200.0);
    assert_eq!(boss_phase(&app, boss), BossPhase::Phase1);
    let mods = PlayerModifiers::default();
    let hit = |app: &mut App, amount: f32| -> f32 {
        let mut e = app.world_mut().get_mut::<Enemy>(boss).unwrap();
        damage_enemy(&mut e, amount, false, &mods)
    };
    let calm = |app: &mut App| {
        godmode(app);
        cull_kind(app, EnemyKind::JammerFiend, None);
        if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
            *progress = progress.max(0.5);
        }
    };

    // Phase I -> clamp at 65% -> Shielded I with two tethered pylons.
    hit(&mut app, 5000.0);
    assert_eq!(enemy(&app, boss).unwrap().hp, max * 0.65, "threshold clamps HP");
    calm(&mut app);
    step(&mut app, 3);
    assert_eq!(boss_phase(&app, boss), BossPhase::Shielded1);
    let pylons = living_pylons(&mut app);
    assert_eq!(pylons.len(), 2, "two Shield Pylons");
    assert_eq!(boss_hud(&app).tag(), "SHIELDED // 2 PYLONS");
    assert!(enemy(&app, boss).unwrap().shielded);
    assert_eq!(hit(&mut app, 400.0), 0.0, "full shield immunity while pylons stand");
    assert_eq!(enemy(&app, boss).unwrap().hp, max * 0.65);
    let (pa, pb) = (app.world().get::<Transform>(pylons[0]).unwrap().translation, app.world().get::<Transform>(pylons[1]).unwrap().translation);
    let t = grid(&app).tower_world();
    assert!((pa - t).with_y(0.0).normalize().dot((pb - t).with_y(0.0).normalize()) < 0.0, "pylons on opposite sides");

    // Pylon #1: shotgun fire (HitTarget::Pylon). Freeze the boss for a clean line.
    app.world_mut().get_mut::<Enemy>(boss).unwrap().stun_timer = 1.0e3;
    let g = grid(&app);
    let pc = g.world_to_cell(pa);
    let stand = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .flat_map(|o| [pc + o * 2, pc + o])
        .find(|c| matches!(g.cell(*c), Cell::Floor { .. } | Cell::AtriumFloor) && has_line_of_sight(&g, g.cell_center(*c) + Vec3::Y * PLAYER_EYE_HEIGHT, pa + Vec3::Y * 1.5))
        .expect("vantage on pylon");
    teleport(&mut app, g.cell_center(stand), pa + Vec3::Y * 1.5);
    step(&mut app, 2);
    bot(&mut app).switch_weapon = Some(Weapon::Shotgun);
    let target = pylons[0];
    let frames = fire_until(
        &mut app,
        12 * FPS,
        |a| {
            calm(a);
            with_player(a, |p| p.ammo_shells = p.max_shells);
            aim_at(a, pa + Vec3::Y * 1.5);
        },
        |a| a.world().get_entity(target).is_err(),
    );
    assert!(frames < 12 * FPS, "shotgun destroys a Shield Pylon");
    assert_eq!(living_pylons(&mut app).len(), 1);
    step(&mut app, 2);
    assert_eq!(boss_phase(&app, boss), BossPhase::Shielded1, "one pylon still shields the boss");
    assert_eq!(boss_hud(&app).tag(), "SHIELDED // 1 PYLON");

    // Pylon #2 down -> shatter: stun, Phase II, escort, supply drop.
    app.world_mut().get_mut::<Enemy>(boss).unwrap().stun_timer = 0.0;
    let pickups0 = count::<segnale_debole::weapons::Pickup>(&mut app);
    app.world_mut().get_mut::<BossPylon>(pylons[1]).unwrap().hp = 0.0;
    calm(&mut app);
    step(&mut app, 2);
    assert_eq!(boss_phase(&app, boss), BossPhase::Phase2);
    let e = enemy(&app, boss).unwrap();
    assert!(!e.shielded && e.stun_timer > BOSS_SHIELD_STUN - 0.1, "shatter stuns: {}", e.stun_timer);
    assert!(count::<segnale_debole::weapons::Pickup>(&mut app) > pickups0, "supply drop on the ring");
    let kinds: Vec<EnemyKind> = {
        let mut q = app.world_mut().query::<&Enemy>();
        q.iter(app.world()).filter(|e| e.is_alive()).map(|e| e.kind).collect()
    };
    assert!(kinds.contains(&EnemyKind::PhaseStalker) && kinds.contains(&EnemyKind::Imp), "Phase II escort: {kinds:?}");
    assert_eq!(boss_hud(&app).tag(), "PHASE II");

    // Phase II -> 30% -> Shielded II (fresh pylons at the atrium corners).
    hit(&mut app, 5000.0);
    assert!((enemy(&app, boss).unwrap().hp - max * 0.3).abs() < 1e-3);
    calm(&mut app);
    step(&mut app, 3);
    assert_eq!(boss_phase(&app, boss), BossPhase::Shielded2);
    let pylons = living_pylons(&mut app);
    assert_eq!(pylons.len(), 2);
    // Pylon #3: a direct Signal-Mortar rocket.
    app.world_mut().get_mut::<Enemy>(boss).unwrap().stun_timer = 1.0e3;
    let p3 = app.world().get::<Transform>(pylons[0]).unwrap().translation;
    let pc = g.world_to_cell(p3);
    let stand = [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y]
        .into_iter()
        .flat_map(|o| [pc + o * 2, pc + o * 3])
        .find(|c| matches!(g.cell(*c), Cell::Floor { .. } | Cell::AtriumFloor) && has_line_of_sight(&g, g.cell_center(*c) + Vec3::Y * PLAYER_EYE_HEIGHT, p3 + Vec3::Y * 1.5))
        .expect("vantage on pylon 3");
    teleport(&mut app, g.cell_center(stand), p3 + Vec3::Y * 1.5);
    step(&mut app, 2);
    bot(&mut app).switch_weapon = Some(Weapon::Rocket);
    let target = pylons[0];
    let frames = fire_until(
        &mut app,
        12 * FPS,
        |a| {
            calm(a);
            with_player(a, |p| p.ammo_rockets = p.max_rockets);
            aim_at(a, p3 + Vec3::Y * 1.5);
        },
        |a| a.world().get_entity(target).is_err(),
    );
    assert!(frames < 12 * FPS, "rockets destroy a Shield Pylon");
    app.world_mut().get_mut::<Enemy>(boss).unwrap().stun_timer = 0.0;
    app.world_mut().get_mut::<BossPylon>(pylons[1]).unwrap().hp = 0.0;
    calm(&mut app);
    step(&mut app, 2);
    assert_eq!(boss_phase(&app, boss), BossPhase::Overdrive);
    assert!(enemy(&app, boss).unwrap().stun_timer > 1.0);
    assert_eq!(boss_hud(&app).tag(), "OVERDRIVE");
    assert_eq!(count::<BossPylon>(&mut app), 0);

    // Overdrive fires crossfire once the stun wears off.
    teleport(&mut app, g.cell_center(ring_cell(&g)), g.tower_world());
    let proj0 = count::<EnemyProjectile>(&mut app);
    let mut max_proj = proj0;
    // 1.8 s shatter stun, then the Overdrive cadence (telegraph -> crossfire).
    for _ in 0..(7 * FPS) {
        calm(&mut app);
        step(&mut app, 1);
        max_proj = max_proj.max(count::<EnemyProjectile>(&mut app));
    }
    assert!(max_proj >= proj0 + 5, "Overdrive barrages: {proj0} -> {max_proj}");

    // Upload holds at 99% while the boss lives; its death locks extraction.
    if let ExtractionState::Uploading { progress, .. } = &mut *app.world_mut().resource_mut::<ExtractionState>() {
        *progress = 0.99;
    }
    calm(&mut app);
    step(&mut app, 30);
    assert!(matches!(extraction(&app), ExtractionState::Uploading { boss_hold: true, .. }));
    let kills0 = run_stats(&app).kills;
    assert!(hit(&mut app, 5000.0) > 0.0, "Overdrive has no HP floor");
    step(&mut app, 1);
    assert!(matches!(extraction(&app), ExtractionState::Extracting { .. }), "{:?}", extraction(&app));
    let feed = app.world().resource::<HudFeed>().clone();
    assert!(feed.toasts.iter().any(|t| t.text == BOSS_DEATH_TEXT));
    assert!(run_stats(&app).kills > kills0);
    step(&mut app, 3 * FPS);
    assert_eq!(app_state(&app), AppState::Victory, "Floor 5 completes into Victory");
}

#[test]
fn phase_stalker_blinks_through_a_dead_zone_onto_a_walkable_flank() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    jump_to_floor(&mut app, 2);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    let g = grid(&app);
    let mut blinked = false;
    'outer: for &j in &g.jammers {
        for o in [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
            let dz = j + o;
            if !matches!(g.cell(dz), Cell::Floor { .. }) {
                continue;
            }
            let Some(me) = straight_cell(&g, dz, 2) else { continue };
            let spos = spawn_pos_for(EnemyKind::PhaseStalker, &g, dz);
            let ppos = g.cell_center(me);
            if pick_phase_blink_target(&g, spos, ppos, 1.0).is_none() {
                continue;
            }
            despawn_all_enemies(&mut app);
            teleport(&mut app, ppos, spos + Vec3::Y * 1.2);
            let st = spawn_demon(&mut app, EnemyKind::PhaseStalker, spos);
            step(&mut app, 1);
            {
                let mut e = app.world_mut().get_mut::<Enemy>(st).unwrap();
                e.alerted = true;
                e.blink_cooldown = 0.0;
                e.attack_timer = 5.0;
            }
            let mut telegraphed = false;
            let mut last = spos;
            for _ in 0..(2 * FPS) {
                godmode(&mut app);
                step(&mut app, 1);
                let Some(e) = enemy(&app, st) else { break };
                let now = app.world().get::<Transform>(st).unwrap().translation;
                if e.blink_timer > 0.0 {
                    telegraphed = true;
                    assert!(e.enraged, "blinks only while Dead-Zone enraged");
                }
                let jump = Vec2::new(now.x - last.x, now.z - last.z).length();
                if jump > 4.0 {
                    assert!(telegraphed, "0.35 s shimmer telegraph precedes the blink");
                    assert!((4.9..=8.1).contains(&jump), "blink distance {jump}");
                    let c = g.world_to_cell(now);
                    assert!(g.in_bounds(c) && matches!(g.cell(c), Cell::Floor { .. } | Cell::AtriumFloor | Cell::Ramp { .. }), "lands on {c}: {:?}", g.cell(c));
                    blinked = true;
                    break 'outer;
                }
                last = now;
            }
        }
    }
    assert!(blinked, "a Phase Stalker blinked out of a Dead Zone");
}

#[test]
fn relay_sniper_locks_on_with_a_laser_then_fires_a_rail_bolt() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 99);
    godmode(&mut app);
    let c = open_arena(&mut app);
    let floor = run_stats(&app).floor;
    let sniper = spawn_demon(&mut app, EnemyKind::RelaySniper, c + Vec3::new(0.0, 0.05, -22.0));
    step(&mut app, 1);
    {
        let mut e = app.world_mut().get_mut::<Enemy>(sniper).unwrap();
        e.alerted = true;
        e.attack_timer = 0.0;
    }
    let laser_visible = |app: &mut App| {
        let mut q = app.world_mut().query_filtered::<&Visibility, With<SniperLaser>>();
        q.iter(app.world()).any(|v| *v != Visibility::Hidden)
    };
    let rail_bolts = |app: &mut App| {
        let mut q = app.world_mut().query::<&EnemyProjectile>();
        q.iter(app.world()).filter(|p| p.vel.length() > 40.0).map(|p| p.damage).collect::<Vec<f32>>()
    };
    let mut lock_frame = None;
    let mut fired_frame = None;
    for f in 0..(3 * FPS) {
        godmode(&mut app);
        teleport(&mut app, c, c + Vec3::new(0.0, 1.0, -22.0));
        step(&mut app, 1);
        let e = enemy(&app, sniper).unwrap();
        if lock_frame.is_none() && e.sniper_lock_timer > 0.0 {
            lock_frame = Some(f);
            assert!(laser_visible(&mut app), "laser sight shows during the lock-on");
            assert!(rail_bolts(&mut app).is_empty(), "no shot during the telegraph");
        }
        let bolts = rail_bolts(&mut app);
        if !bolts.is_empty() {
            fired_frame = Some(f);
            assert!((bolts[0] - (18.0 + floor as f32)).abs() < 1e-3, "rail damage {}", bolts[0]);
            break;
        }
    }
    let (lock, fired) = (lock_frame.expect("sniper locked on"), fired_frame.expect("rail bolt fired"));
    let telegraph = (fired - lock) as f32 / FPS as f32;
    assert!((telegraph - SNIPER_LOCK_SECS).abs() < 0.1, "lock-on telegraph {telegraph:.2}s");
    step(&mut app, 1);
    assert!(!laser_visible(&mut app), "laser drops after the shot");
}

// ---------------------------------------------------------------------------
// Milestone 14: adaptive soundtrack mix
// ---------------------------------------------------------------------------

fn music_weights(app: &App) -> [f32; 4] {
    app.world().resource::<segnale_debole::audio::MusicMixState>().weights
}

#[test]
fn adaptive_music_mix_follows_dead_zone_signal_sos_and_pause() {
    use segnale_debole::audio::{target_music_weights, MusicCues};
    let close = |w: [f32; 4], t: [f32; 4], tol: f32| (0..4).all(|i| (w[i] - t[i]).abs() < tol);

    // Main menu: tense drone bed.
    let mut app = build_headless_test_app();
    step(&mut app, 2 * FPS);
    let w = music_weights(&app);
    assert!(close(w, [0.45, 0.15, 0.0, 0.0], 0.01), "menu mix {w:?}");

    begin_run(&mut app, 41);
    godmode(&mut app);
    despawn_all_enemies(&mut app);

    // Dead Zone: the drone takes over.
    find_blackout(&mut app);
    assert!(signal(&app).bars <= 1);
    step(&mut app, 2 * FPS);
    let w = music_weights(&app);
    assert!(signal(&app).bars <= 1, "still in the dead zone");
    assert!(w[0] > 0.85 && w[1] < 0.15 && w[2] < 0.05, "dead-zone mix {w:?}");

    // Walk into the Signal Ring (strong signal, uplink not yet started): Signal Pulse leads.
    let g = grid(&app);
    teleport(&mut app, g.cell_center(ring_cell(&g)), g.tower_world() + Vec3::Y * 2.0);
    step(&mut app, FPS);
    assert!(matches!(extraction(&app), ExtractionState::Searching { .. }), "{:?}", extraction(&app));
    assert!(signal(&app).bars >= 4);
    let w = music_weights(&app);
    assert!(w[1] > 0.8 && w[1] > w[0] && w[2] < 0.3, "strong-signal mix {w:?}");

    // SOS upload: the Rush Metal stem slams in.
    bot(&mut app).interact = true;
    step(&mut app, 2);
    let mut up = 0;
    while !matches!(extraction(&app), ExtractionState::Uploading { .. }) && up < FPS {
        step(&mut app, 1);
        up += 1;
    }
    assert!(matches!(extraction(&app), ExtractionState::Uploading { .. }), "{:?}", extraction(&app));
    step(&mut app, 2 * FPS);
    let w = music_weights(&app);
    assert!(w[2] > 0.9 && w[2] > w[1] && w[1] > w[0], "SOS rush mix {w:?}");
    assert!(close(w, target_music_weights(MusicCues { state: AppState::Playing, bars: 5, uploading: true, ..Default::default() }), 0.05), "{w:?}");
    assert!(w[3] < 0.02, "no boss stem without the boss {w:?}");

    // Pause: everything ducks to a quiet drone.
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
    step(&mut app, 2 * FPS);
    assert_eq!(app_state(&app), AppState::Paused);
    let w = music_weights(&app);
    assert!(w[2] < 0.05 && w[1] < 0.02 && (w[0] - 0.25).abs() < 0.02, "paused mix {w:?}");
}

#[test]
fn adaptive_music_mix_goes_rush_during_boss_fight() {
    use segnale_debole::audio::{step_music_mix, MusicMixState};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 5);
    // A live boss snapshot drives the rush stem even before any SOS upload.
    app.world_mut().insert_resource(BossHud { active: true, phase: BossPhase::default(), hp_frac: 1.0, pylons: 0 });
    let before = music_weights(&app);
    app.world_mut().resource_mut::<MusicMixState>().weights = before;
    // The boss system rewrites BossHud each frame on non-boss floors, so check the
    // mix system's single-frame response from the staged snapshot directly.
    app.world_mut().run_system_once(segnale_debole::audio::update_music_mix).unwrap();
    let after = music_weights(&app);
    let dt = app.world().resource::<Time>().delta_secs();
    let expect = step_music_mix(before, [0.15, 0.30, 1.0, 0.75], dt);
    for i in 0..4 {
        assert!((after[i] - expect[i]).abs() < 1e-5, "{after:?} vs {expect:?}");
    }
    assert!(after[2] > before[2], "rush rises under the boss");
    assert!(after[3] > before[3], "the Boss Overdrive stem rises too");

    // Overdrive: the boss stem heads for full weight.
    app.world_mut().insert_resource(BossHud { active: true, phase: BossPhase::Overdrive, hp_frac: 0.2, pylons: 0 });
    let before = music_weights(&app);
    app.world_mut().run_system_once(segnale_debole::audio::update_music_mix).unwrap();
    let after = music_weights(&app);
    let dt = app.world().resource::<Time>().delta_secs();
    let expect = step_music_mix(before, [0.05, 0.20, 1.0, 1.0], dt);
    for i in 0..4 {
        assert!((after[i] - expect[i]).abs() < 1e-5, "{after:?} vs {expect:?}");
    }
}

#[test]
fn architecture_batches_are_static_rt_ready_and_hazards_flow() {
    use segnale_debole::assets_gen::GameAssets;
    use segnale_debole::raytracing::{rt_mesh_convertible, MAX_EMISSIVE_TRIANGLES};

    let mut app = build_menu_app(None);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    godmode(&mut app);
    step(&mut app, 5);
    let mats0 = material_count(&app);
    for _ in 0..3 {
        let g = grid(&app);
        let theme = app.world().resource::<GameAssets>().theme_for_floor(g.floor).clone();
        // Trim + accent are two static batches each (no per-prop entities).
        let mut q = app.world_mut().query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<LevelBatch>>();
        let batches: Vec<(Handle<Mesh>, AssetId<StandardMaterial>)> = q.iter(app.world()).map(|(m, mat)| (m.0.clone(), mat.0.id())).collect();
        assert!(batches.len() <= 16, "floor {}: {} level batches", g.floor, batches.len());
        for (slot, id) in [("trim", theme.trim.id()), ("accent", theme.accent_emissive.id())] {
            let mine: Vec<_> = batches.iter().filter(|(_, m)| *m == id).collect();
            assert_eq!(mine.len(), 1, "floor {}: one {slot} batch", g.floor);
            let meshes = app.world().resource::<Assets<Mesh>>();
            let mesh = meshes.get(&mine[0].0).expect("batch mesh");
            assert!(rt_mesh_convertible(mesh), "{slot} batch converts for Solari");
            let tris = mesh.indices().unwrap().len() / 3;
            if slot == "accent" {
                assert!(tris * 8 < MAX_EMISSIVE_TRIANGLES, "floor {}: {tris} emissive accent triangles", g.floor);
            }
        }
        // Hazard flow animates the shared lava + accent materials in place.
        let read = |app: &App| {
            let mats = app.world().resource::<Assets<StandardMaterial>>();
            let lava = mats.get(&theme.lava).unwrap();
            (lava.uv_transform.translation, lava.emissive, mats.get(&theme.accent_emissive).unwrap().emissive)
        };
        let a = read(&app);
        step(&mut app, 30);
        let b = read(&app);
        assert!(b.0.is_finite() && b.0 != a.0, "floor {}: lava UV drifts ({:?} -> {:?})", g.floor, a.0, b.0);
        assert!(b.1.red.is_finite() && b.2.red.is_finite());
        assert!(b.1.red <= theme.lava_emissive_base.red + 1e-4 && b.1.red >= theme.lava_emissive_base.red * 0.7);
        assert_eq!(material_count(&app), mats0, "animation allocates no materials");
        fast_clear_floor(&mut app);
        bot(&mut app).pick_upgrade = Some(0);
        step(&mut app, 3);
        assert_eq!(app_state(&app), AppState::Playing);
        godmode(&mut app);
        step(&mut app, 3);
    }
}

// ---------------------------------------------------------------------------
// Milestone 17: combat juice (enemy animation, decals, gibs, weapon feel)
// ---------------------------------------------------------------------------

/// Small walled arena: ground floor, a 3x2 catwalk block (`^`, 2.4 m) and a
/// hazard pit (`~`). Garrison cleared, player at the west end.
fn juice_arena(app: &mut App) -> LevelGrid {
    let rows = [
        "############",
        "#..........#",
        "#...^^^....#",
        "#...^^^..~~#",
        "#..........#",
        "#..........#",
        "############",
    ];
    app.world_mut().insert_resource(LevelGrid::from_ascii(&rows));
    step(app, 2);
    despawn_all_enemies(app);
    let g = grid(app);
    let feet = g.cell_center(IVec2::new(1, 5));
    teleport(app, feet, feet + Vec3::new(10.0, PLAYER_EYE_HEIGHT, 0.0));
    step(app, 5);
    g
}

fn body_pivot(app: &mut App, root: Entity) -> Transform {
    let kids: Vec<Entity> = app.world().get::<Children>(root).unwrap().iter().collect();
    let body = kids.into_iter().find(|k| app.world().get::<segnale_debole::enemies::EnemyBody>(*k).is_some()).expect("body pivot");
    *app.world().get::<Transform>(body).unwrap()
}

#[test]
fn enemy_hit_stagger_and_attack_pose_move_only_the_body_pivot() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 21);
    godmode(&mut app);
    let g = juice_arena(&mut app);
    app.world_mut().insert_resource(segnale_debole::dev::DevFreezeAi);
    let imp = spawn_demon(&mut app, EnemyKind::Imp, spawn_pos_for(EnemyKind::Imp, &g, IVec2::new(8, 1)));
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, spawn_pos_for(EnemyKind::HellBaron, &g, IVec2::new(8, 4)));
    step(&mut app, 3);
    let roots: Vec<Transform> = [imp, baron].iter().map(|e| *app.world().get::<Transform>(*e).unwrap()).collect();
    let probe = |app: &App, e: Entity, root: Vec3| {
        let en = app.world().get::<Enemy>(e).unwrap();
        let origin = root + Vec3::new(-6.0, en.stats.hit_spheres[0].0, 0.0);
        en.ray_hit(root, origin, Vec3::X, 50.0)
    };
    let hits0 = [probe(&app, imp, roots[0].translation), probe(&app, baron, roots[1].translation)];

    // A shot travelling +X staggers both; the root (and hit spheres) never move.
    for e in [imp, baron] {
        app.world_mut().get_mut::<Enemy>(e).unwrap().stagger(Vec3::new(3.0, 0.4, 0.0));
    }
    step(&mut app, 3);
    let tip = |tf: &Transform| (tf.rotation * Vec3::Y).x;
    let (imp_tip, baron_tip) = (tip(&body_pivot(&mut app, imp)), tip(&body_pivot(&mut app, baron)));
    assert!(imp_tip > 0.08, "imp body tips away from the hit: {imp_tip}");
    assert!(baron_tip > 0.01 && baron_tip < imp_tip * 0.6, "heavy baron staggers less: {baron_tip} vs {imp_tip}");
    for (i, e) in [imp, baron].into_iter().enumerate() {
        assert_eq!(*app.world().get::<Transform>(e).unwrap(), roots[i], "root transform untouched by animation");
        assert_eq!(probe(&app, e, roots[i].translation), hits0[i], "hit spheres unaffected");
    }
    step(&mut app, FPS / 2);
    assert!(tip(&body_pivot(&mut app, imp)).abs() < 0.02, "stagger recovers");

    // Ranged recoil pitches the torso back (+Z), a melee lunge forward (-Z).
    app.world_mut().get_mut::<Enemy>(imp).unwrap().trigger_attack_anim(false);
    step(&mut app, 3);
    let back = (body_pivot(&mut app, imp).rotation * Vec3::Y).z;
    app.world_mut().get_mut::<Enemy>(imp).unwrap().trigger_attack_anim(true);
    step(&mut app, 3);
    let fwd = (body_pivot(&mut app, imp).rotation * Vec3::Y).z;
    assert!(back > 0.05 && fwd < -0.05, "recoil {back} / lunge {fwd}");
    assert_eq!(*app.world().get::<Transform>(imp).unwrap(), roots[0]);
}

#[test]
fn decal_pool_is_fifo_bounded_level_scoped_and_allocation_free() {
    use segnale_debole::decals::{Decal, DecalKind, DecalPool, DecalQueue, DecalRequest, MAX_DECALS};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 5);
    godmode(&mut app);
    let g = juice_arena(&mut app);
    step(&mut app, 2);
    let (mats0, meshes0, images0) = (material_count(&app), mesh_count(&app), app.world().resource::<Assets<Image>>().len());
    assert_eq!(count::<Decal>(&mut app), 0);

    let ground = g.cell_center(IVec2::new(2, 1));
    // Two bursts (a single frame's queue is itself bounded by MAX_DECALS).
    for range in [0..MAX_DECALS, MAX_DECALS..MAX_DECALS + 34] {
        let mut q = app.world_mut().resource_mut::<DecalQueue>();
        for i in range {
            let kind = DecalKind::ALL[i % 5];
            q.push(DecalRequest::floor(kind, ground + Vec3::new((i % 7) as f32 * 0.3, 0.5, (i % 5) as f32 * 0.3), 1.0));
        }
        step(&mut app, 1);
    }
    step(&mut app, 1);
    assert_eq!(count::<Decal>(&mut app), MAX_DECALS, "ring buffer caps live decals");
    let pool = app.world().resource::<DecalPool>();
    assert_eq!(pool.ring.len(), MAX_DECALS);
    assert_eq!(pool.spawned, (MAX_DECALS + 34) as u64);
    let newest = *pool.ring.back().unwrap();
    let oldest_live = *pool.ring.front().unwrap();
    assert!(app.world().get_entity(newest).is_ok() && app.world().get_entity(oldest_live).is_ok());
    // Every decal sits flush (anti-z-fight lift) on the ground floor.
    let mut q = app.world_mut().query_filtered::<&Transform, With<Decal>>();
    for tf in q.iter(app.world()) {
        assert!((0.017..0.03).contains(&tf.translation.y), "decal at {:?}", tf.translation);
    }

    // Catwalk decal snaps to 2.4 m; pit / wall requests are skipped.
    let spawned = app.world().resource::<DecalPool>().spawned;
    {
        let mut q = app.world_mut().resource_mut::<DecalQueue>();
        q.push(DecalRequest::floor(DecalKind::Scorch, g.cell_center(IVec2::new(5, 2)) + Vec3::Y, 1.5));
        q.push(DecalRequest::floor(DecalKind::Scorch, g.cell_center(IVec2::new(9, 3)), 1.5));
        q.push(DecalRequest::floor(DecalKind::Scorch, g.cell_center(IVec2::new(0, 0)), 1.5));
    }
    step(&mut app, 2);
    let pool = app.world().resource::<DecalPool>();
    assert_eq!(pool.spawned, spawned + 1, "pit and wall requests were skipped");
    let cat = *app.world().get::<Transform>(*pool.ring.back().unwrap()).unwrap();
    assert!((cat.translation.y - 2.4).abs() < 0.03 && cat.translation.y > 2.4, "catwalk decal at {:?}", cat.translation);

    // A death leaves its archetype splatter.
    let fiend = spawn_demon(&mut app, EnemyKind::JammerFiend, spawn_pos_for(EnemyKind::JammerFiend, &g, IVec2::new(2, 4)));
    step(&mut app, 2);
    kill(&mut app, fiend);
    step(&mut app, 3);
    let mut q = app.world_mut().query::<&Decal>();
    assert!(q.iter(app.world()).any(|d| d.0 == DecalKind::IchorGreen), "Jammer Fiend ichor decal");
    assert_eq!(count::<Decal>(&mut app), MAX_DECALS);

    assert_eq!(material_count(&app), mats0, "decals + gibs allocated no StandardMaterials");
    assert_eq!(mesh_count(&app), meshes0, "decals + gibs allocated no meshes");
    assert_eq!(app.world().resource::<Assets<Image>>().len(), images0, "decal textures are startup-only");

    // Floor transition: every decal is a LevelEntity and the pool forgets them.
    fast_clear_floor(&mut app);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    bot(&mut app).pick_upgrade = Some(0);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(grid(&app).floor, 2);
    assert_eq!(count::<Decal>(&mut app), 0, "floor transition cleared decals");
    assert!(app.world().resource::<DecalPool>().ring.is_empty());
}

#[test]
fn debris_bounces_on_real_floor_height_catwalk_vs_ground() {
    use segnale_debole::fx::{Fx, FxMotion};
    use segnale_debole::level::LevelEntity;
    let mut app = build_headless_test_app();
    begin_run(&mut app, 9);
    godmode(&mut app);
    let g = juice_arena(&mut app);
    let drop = |app: &mut App, at: Vec3| {
        app.world_mut()
            .spawn((
                LevelEntity,
                Fx::new(5.0, FxMotion::Debris { vel: Vec3::new(0.2, 3.0, -0.1), spin: Vec3::new(8.0, 3.0, -6.0), gravity: 18.0, bounce: 0.45, clinks: 0 }, Vec3::ONE),
                Transform::from_translation(at + Vec3::Y * 1.8),
            ))
            .id()
    };
    let cat = drop(&mut app, g.cell_center(IVec2::new(5, 2)));
    let ground = drop(&mut app, g.cell_center(IVec2::new(2, 1)));
    let mut min_cat = f32::MAX;
    for _ in 0..(2 * FPS) {
        step(&mut app, 1);
        min_cat = min_cat.min(app.world().get::<Transform>(cat).unwrap().translation.y);
    }
    let y = |app: &App, e: Entity| app.world().get::<Transform>(e).unwrap().translation.y;
    assert!((y(&app, cat) - 2.43).abs() < 0.02, "catwalk debris rests at {}", y(&app, cat));
    assert!(min_cat > 2.4, "never sank through the catwalk ({min_cat})");
    assert!((y(&app, ground) - 0.03).abs() < 0.02, "ground debris rests at {}", y(&app, ground));
    let Fx { motion: FxMotion::Debris { vel, spin, .. }, .. } = app.world().get::<Fx>(cat).unwrap().clone() else { panic!() };
    assert_eq!((vel, spin), (Vec3::ZERO, Vec3::ZERO), "bounces damped to rest");
}

#[test]
fn weapon_inspect_cancels_on_fire_swap_and_pause_and_landing_dips_viewmodel() {
    use segnale_debole::weapons::{ViewmodelFeel, INSPECT_SECS, LANDING_DIP_MAX};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 13);
    godmode(&mut app);
    let _ = juice_arena(&mut app);
    let ws = |app: &App| app.world().resource::<WeaponState>().clone();
    step(&mut app, FPS);
    assert_eq!(ws(&app).inspect_timer, 0.0);

    tap(&mut app, KeyCode::KeyF);
    let t = ws(&app).inspect_timer;
    assert!(t > INSPECT_SECS * 0.9 && t <= INSPECT_SECS, "inspect started: {t}");
    let cd = ws(&app).cooldown;
    step(&mut app, 10);
    assert!(ws(&app).inspect_timer < t, "inspect plays out");
    assert!(ws(&app).cooldown <= cd, "inspect never touches fire cooldowns");
    // Firing cancels it immediately.
    bot(&mut app).fire = true;
    step(&mut app, 1);
    bot(&mut app).fire = false;
    assert_eq!(ws(&app).inspect_timer, 0.0, "fire cancels inspect");
    step(&mut app, FPS);
    // Swapping cancels it.
    tap(&mut app, KeyCode::KeyF);
    assert!(ws(&app).inspect_timer > 0.0);
    tap(&mut app, KeyCode::Digit2);
    assert_eq!(ws(&app).inspect_timer, 0.0, "swap cancels inspect");
    step(&mut app, FPS);
    // Pausing cancels it.
    tap(&mut app, KeyCode::KeyF);
    assert!(ws(&app).inspect_timer > 0.0);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
    step(&mut app, 2);
    assert_eq!(ws(&app).inspect_timer, 0.0, "pause cancels inspect");
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
    step(&mut app, 2);
    // Expires on its own.
    tap(&mut app, KeyCode::KeyF);
    step(&mut app, (INSPECT_SECS * FPS as f32) as usize + 2);
    assert_eq!(ws(&app).inspect_timer, 0.0);

    // Landing dip: drop the player from 4 m; the viewmodel dips (bounded) and recovers.
    let feet = player_feet(&mut app);
    {
        let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
        let (mut p, mut tf) = q.single_mut(app.world_mut()).unwrap();
        tf.translation = feet + Vec3::Y * 4.0;
        p.velocity = Vec3::ZERO;
        p.grounded = false;
    }
    let mut lowest: f32 = 0.0;
    for _ in 0..(2 * FPS) {
        step(&mut app, 1);
        let f = *app.world().resource::<ViewmodelFeel>();
        assert!(f.dip >= -LANDING_DIP_MAX - 1e-6 && f.dip.is_finite());
        lowest = lowest.min(f.dip);
    }
    assert!(lowest < -0.005, "landing dipped the viewmodel ({lowest})");
    assert!(app.world().resource::<ViewmodelFeel>().dip.abs() < 2e-3, "dip springs back");
}

// ---------------------------------------------------------------------------
// Milestone 21: shareable custom seeds + 3D Bestiary / Armory viewer
// ---------------------------------------------------------------------------

use segnale_debole::compendium::{
    format_seed_code, seed_from_code, Category, CompendiumEntity, CompendiumState, CustomSeedState, ExhibitId, ExhibitModel, TurntableRoot,
};

fn seed_state(app: &App) -> CustomSeedState {
    app.world().resource::<CustomSeedState>().clone()
}

#[test]
fn enemy_deaths_count_kills_per_kind() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 2101);
    godmode(&mut app);
    despawn_all_enemies(&mut app);
    step(&mut app, 2);
    let far = player_feet(&mut app);
    let demons = [EnemyKind::Imp, EnemyKind::Imp, EnemyKind::HellBaron, EnemyKind::RelaySniper].map(|k| spawn_demon(&mut app, k, far));
    step(&mut app, 1);
    let before = app.world().resource::<RunStats>().clone();
    for e in demons {
        kill(&mut app, e);
    }
    step(&mut app, 3);
    let run = app.world().resource::<RunStats>().clone();
    let k = |kind: EnemyKind| run.enemy_kills[kind.index()] - before.enemy_kills[kind.index()];
    assert_eq!((k(EnemyKind::Imp), k(EnemyKind::HellBaron), k(EnemyKind::RelaySniper), k(EnemyKind::Cacodemon)), (2, 1, 1, 0));
    assert_eq!(run.kills - before.kills, 4);
    assert_eq!(run.enemy_kills.iter().sum::<u32>(), run.kills, "per-kind table sums to the kill counter");
}

#[test]
fn seed_editor_mutes_menu_hotkeys_and_arms_the_run() {
    let mut app = build_menu_app(None);
    assert_eq!(app_state(&app), AppState::MainMenu);
    tap(&mut app, KeyCode::KeyS);
    assert!(seed_state(&app).editing, "S opens the inline seed editor");
    // Every main menu hotkey letter types instead of launching a screen.
    for key in [KeyCode::KeyD, KeyCode::KeyO, KeyCode::KeyO, KeyCode::KeyM, KeyCode::KeyB, KeyCode::KeyT, KeyCode::KeyL, KeyCode::KeyC] {
        tap(&mut app, key);
        assert_eq!(app_state(&app), AppState::MainMenu, "{key:?} must not leave the main menu while typing");
    }
    assert_eq!(seed_state(&app).code, "DOOMBTLC");
    for _ in 0..4 {
        tap(&mut app, KeyCode::Backspace);
    }
    tap(&mut app, KeyCode::Minus);
    tap(&mut app, KeyCode::Digit7);
    assert!(ui_texts(&mut app).iter().any(|t| t == "SEED: DOOM-7_  [S]"), "live editor readout on the SEED switch");
    tap(&mut app, KeyCode::Enter);
    let s = seed_state(&app);
    assert!(!s.editing);
    assert_eq!(app_state(&app), AppState::MainMenu, "Enter locks the code without starting a run");
    assert_eq!(s.armed(), seed_from_code("DOOM-7"));
    assert!(ui_texts(&mut app).iter().any(|t| t == "SEED: DOOM-7  [S]"));

    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    let run = app.world().resource::<RunStats>().clone();
    assert_eq!(Some(run.run_seed), seed_from_code("DOOM-7"));
    assert!(run.custom_seed);
    assert_eq!(run.seed_label.as_deref(), Some("DOOM-7"));
    step(&mut app, 2);
    assert!(ui_texts(&mut app).iter().any(|t| t == "SEED DOOM-7"), "HUD plate shows the custom seed");
    let grid_a = format!("{:?}", app.world().resource::<LevelGrid>());

    // Pause: the readout carries the seed and S re-arms it for sharing.
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    assert!(ui_texts(&mut app).iter().any(|t| t.ends_with("//  SEED DOOM-7")));
    // Restarting a custom-seed run rebuilds the very same bunker.
    tap(&mut app, KeyCode::KeyR);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    let run = app.world().resource::<RunStats>().clone();
    assert_eq!(Some(run.run_seed), seed_from_code("DOOM-7"));
    assert_eq!(format!("{:?}", app.world().resource::<LevelGrid>()), grid_a, "same code = same bunker");
}

#[test]
fn share_seed_from_pause_and_random_runs_get_round_trip_codes() {
    let mut app = build_menu_app(None);
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    let run = app.world().resource::<RunStats>().clone();
    assert!(!run.custom_seed);
    assert!(run.run_seed <= u32::MAX as u64, "random runs roll 32-bit seeds");
    let code = run.seed_label.clone().unwrap();
    assert_eq!(code, format_seed_code(run.run_seed));
    assert_eq!(seed_from_code(&code), Some(run.run_seed));
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    assert!(ui_texts(&mut app).iter().any(|t| t == "SHARE SEED  [S]"));
    tap(&mut app, KeyCode::KeyS);
    assert_eq!(seed_state(&app).code, code);
    assert!(ui_texts(&mut app).iter().any(|t| *t == format!("SEED {code} ARMED  [S]")));
    // Back on the menu the shared code is armed for the next deployment.
    tap(&mut app, KeyCode::KeyM);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert!(ui_texts(&mut app).iter().any(|t| *t == format!("SEED: {code}  [S]")));
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app.world().resource::<RunStats>().run_seed, run.run_seed, "replays the shared bunker");
}

fn visible_exhibits(app: &mut App) -> Vec<ExhibitId> {
    let mut q = app.world_mut().query::<(&ExhibitModel, &Visibility)>();
    q.iter(app.world()).filter(|(_, v)| **v != Visibility::Hidden).map(|(m, _)| m.0).collect()
}

fn count_with<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<Entity, F>();
    q.iter(app.world()).count()
}

#[test]
fn compendium_opens_cycles_exhibits_and_tears_down_cleanly() {
    let mut app = build_menu_app(None);
    assert_eq!(count_with::<With<Mesh3d>>(&mut app), 0, "the main menu has no 3D meshes");
    tap(&mut app, KeyCode::KeyB);
    assert_eq!(app_state(&app), AppState::Compendium);
    assert_eq!(count_with::<With<TurntableRoot>>(&mut app), 1);
    assert_eq!(count_with::<With<ExhibitModel>>(&mut app), ExhibitId::ALL.len());
    assert_eq!(count_with::<(With<PointLight>, With<CompendiumEntity>)>(&mut app), 2, "exactly two stage lights");
    assert_eq!(count_with::<With<PointLight>>(&mut app), 2);
    assert_eq!(visible_exhibits(&mut app), vec![ExhibitId::Imp]);
    // Every staged mesh is Solari-convertible.
    {
        let handles: Vec<Handle<Mesh>> = {
            let mut q = app.world_mut().query::<&Mesh3d>();
            q.iter(app.world()).map(|m| m.0.clone()).collect()
        };
        assert!(handles.len() > 30, "real demon + gun models are staged ({})", handles.len());
        let meshes = app.world().resource::<Assets<Mesh>>();
        for h in handles {
            assert!(segnale_debole::raytracing::rt_mesh_convertible(meshes.get(&h).expect("mesh loaded")));
        }
    }
    // No per-frame asset allocation while the turntable spins.
    let assets = |app: &App| (app.world().resource::<Assets<Mesh>>().len(), app.world().resource::<Assets<StandardMaterial>>().len());
    let before = assets(&app);
    let yaw0 = app.world().resource::<CompendiumState>().yaw;
    step(&mut app, 30);
    assert_eq!(assets(&app), before);
    assert_ne!(app.world().resource::<CompendiumState>().yaw, yaw0, "auto-rotates");

    tap(&mut app, KeyCode::KeyD);
    assert_eq!(app_state(&app), AppState::Compendium, "D cycles here, it never launches the Daily Run");
    assert_eq!(visible_exhibits(&mut app), vec![ExhibitId::Cacodemon]);
    tap(&mut app, KeyCode::KeyA);
    tap(&mut app, KeyCode::ArrowLeft);
    assert_eq!(visible_exhibits(&mut app), vec![ExhibitId::CarrierDaemon]);
    assert!(ui_texts(&mut app).iter().any(|t| t == "THE CARRIER DAEMON"));
    assert!(ui_texts(&mut app).iter().any(|t| t == "07/07"));
    tap(&mut app, KeyCode::Tab);
    let st = app.world().resource::<CompendiumState>().clone();
    assert_eq!((st.category, st.current()), (Category::Armory, ExhibitId::Shotgun));
    tap(&mut app, KeyCode::ArrowRight);
    tap(&mut app, KeyCode::KeyD);
    assert_eq!(visible_exhibits(&mut app), vec![ExhibitId::Mortar]);
    let texts = ui_texts(&mut app);
    assert!(texts.iter().any(|t| t == "UAC FIELD TERMINAL // ARMORY"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "03/03"));
    assert!(texts.iter().any(|t| t == "SIGNAL-MORTAR LAUNCHER"));
    assert!(assets(&app) == before, "switching exhibits only toggles visibility");

    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
    step(&mut app, 2);
    assert_eq!(count_with::<With<CompendiumEntity>>(&mut app), 0);
    assert_eq!(count_with::<With<ExhibitModel>>(&mut app), 0);
    assert_eq!(count_with::<With<TurntableRoot>>(&mut app), 0);
    assert_eq!(count_with::<With<Mesh3d>>(&mut app), 0, "the whole stage is gone");
    assert_eq!(count_with::<With<PointLight>>(&mut app), 0);

    // Re-entering rebuilds one stage (no duplicates) and remembers the exhibit.
    tap(&mut app, KeyCode::KeyB);
    assert_eq!(app_state(&app), AppState::Compendium);
    assert_eq!(count_with::<With<TurntableRoot>>(&mut app), 1);
    assert_eq!(count_with::<With<PointLight>>(&mut app), 2);
    assert_eq!(visible_exhibits(&mut app), vec![ExhibitId::Mortar]);
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
}

#[test]
fn compendium_is_a_pre_run_screen() {
    assert!(AppState::Compendium.is_pre_run());
    // Lifetime kills reach the dossier.
    let mut profile = PlayerProfile::default();
    profile.stats.enemy_kills[EnemyKind::PhaseStalker.index()] = 17;
    let mut app = build_menu_app(Some(ProfileStore::in_memory(profile)));
    app.world_mut().resource_mut::<CompendiumState>().select(ExhibitId::PhaseStalker);
    tap(&mut app, KeyCode::KeyB);
    let texts = ui_texts(&mut app);
    assert!(texts.iter().any(|t| t == "LIFETIME KILLS"));
    assert!(texts.iter().any(|t| t == "17"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "FLANKER // FLOOR 2+"));
}
