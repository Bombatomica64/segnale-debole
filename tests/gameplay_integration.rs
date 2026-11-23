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
use bevy::prelude::*;

use segnale_debole::apps::{PhoneApps, PING_COOLDOWN};
use segnale_debole::bot_input::BotControl;
use segnale_debole::playtest_bot::{PlaytestBot, PlaytestBotPlugin, BOT_TIMEOUT_SECS};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin};
use segnale_debole::enemies::{garrison_size, spawn_enemy, spawn_pos_for, Enemy, EnemyAssets, EnemyKind, PingMarked, ENRAGE_SPEED_MULT};
use segnale_debole::extraction::ExtractionState;
use segnale_debole::level::{jammer_count_for_floor, Cell, DemonJammer, LevelGrid, SignalTower};
use segnale_debole::physics::has_line_of_sight;
use segnale_debole::player::{Player, PLAYER_EYE_HEIGHT};
use segnale_debole::signal::{SignalState, TOWER_RING_RADIUS};
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats, UpgradeId, FINAL_FLOOR};
use segnale_debole::upgrades::{apply_upgrade, UpgradeChoices};
use segnale_debole::weapons::{Pickup, Weapon, WeaponState, PLASMA_DAMAGE};

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
