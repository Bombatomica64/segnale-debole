//! Milestone 33 "Tactical Phone Abilities": headless integration tests for
//! the Phone Punch (melee, projectile parry, barrel launch) and the Camera
//! Flash Strobe, driven through `BotControl` exactly like a player.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use segnale_debole::apps::PhoneApps;
use segnale_debole::bot_input::BotControl;
use segnale_debole::enemies::{spawn_enemy, Enemy, EnemyAssets, EnemyKind, EnemyProjectile, HEAVY_BOLT_RADIUS};
use segnale_debole::fatality::KillCause;
use segnale_debole::fx::{BurstColor, HudFeed};
use segnale_debole::level::{ExplosiveBarrel, LevelEntity, LevelGrid};
use segnale_debole::phone_combat::{
    punch_damage, LaunchedBarrel, FLASH_COOLDOWN, FLASH_DAMAGE, FLASH_STUN, PARRY_HEAL, PUNCH_COOLDOWN, PUNCH_STUN,
};
use segnale_debole::physics::raycast_grid;
use segnale_debole::player::{Player, PLAYER_EYE_HEIGHT};
use segnale_debole::signal::SignalState;
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats};
use segnale_debole::style::{StyleMeter, StyleSource};
use segnale_debole::weapons::{PlayerProjectile, ProjectileKind, PARRY_MIN_SPEED};
use segnale_debole::build_headless_test_app;

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn with_player(app: &mut App, f: impl FnOnce(&mut Player, &mut Transform)) {
    let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
    let (mut p, mut tf) = q.single_mut(app.world_mut()).expect("exactly one player");
    f(&mut p, &mut tf);
}

fn player(app: &mut App) -> Player {
    let mut q = app.world_mut().query::<&Player>();
    q.single(app.world()).unwrap().clone()
}

fn feet(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

fn eye(app: &mut App) -> Vec3 {
    feet(app) + Vec3::Y * PLAYER_EYE_HEIGHT
}

fn bot(app: &mut App) -> Mut<'_, BotControl> {
    app.world_mut().resource_mut::<BotControl>()
}

fn apps(app: &App) -> PhoneApps {
    app.world().resource::<PhoneApps>().clone()
}

fn aim_at(app: &mut App, target: Vec3) {
    let d = target - eye(app);
    let yaw = segnale_debole::level::yaw_facing(Vec2::new(d.x, d.z));
    let pitch = d.y.atan2(Vec2::new(d.x, d.z).length());
    let mut b = bot(app);
    b.look_yaw = Some(yaw);
    b.look_pitch = Some(pitch);
}

fn spawn_demon(app: &mut App, kind: EnemyKind, pos: Vec3) -> Entity {
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>, mut mats: ResMut<Assets<StandardMaterial>>, run: Res<RunStats>| {
            spawn_enemy(&mut commands, &ea, &mut mats, kind, pos, run.floor, false)
        })
        .unwrap()
}

fn spawn_shot(app: &mut App, pos: Vec3, vel: Vec3, radius: f32) -> Entity {
    app.world_mut()
        .spawn((LevelEntity, EnemyProjectile { vel, damage: 12.0, radius, life: 6.0, color: BurstColor::Fire }, Transform::from_translation(pos)))
        .id()
}

fn enemy(app: &App, e: Entity) -> Enemy {
    app.world().get::<Enemy>(e).expect("demon alive").clone()
}

/// A fresh run with an empty garrison, godmode off-switchable, controls idle.
fn arena(seed: u64) -> App {
    let mut app = build_headless_test_app();
    app.world_mut()
        .run_system_once(move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, seed);
        })
        .unwrap();
    step(&mut app, 3);
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
    with_player(&mut app, |p, _| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
    let demons: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    for e in demons {
        app.world_mut().despawn(e);
    }
    step(&mut app, 2);
    app
}

/// The horizontal direction from the eye with the most free space (and that space).
fn open_dir(app: &mut App) -> (Vec3, f32) {
    let grid = app.world().resource::<LevelGrid>().clone();
    let e = eye(app);
    (0..16)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 16.0;
            let d = Vec3::new(a.cos(), 0.0, a.sin());
            // Free at eye height and at knee height (barrels / imps).
            let free = raycast_grid(&grid, e, d, 40.0).map_or(40.0, |h| h.distance);
            let low = raycast_grid(&grid, e - Vec3::Y * 1.0, d, 40.0).map_or(40.0, |h| h.distance);
            (d, free.min(low))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

fn punch(app: &mut App) {
    bot(app).punch = true;
    step(app, 1);
}

fn flash(app: &mut App) {
    bot(app).flash = true;
    step(app, 1);
}

#[test]
fn phone_punch_hits_staggers_stuns_and_respects_its_cooldown() {
    let mut app = arena(331);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 4.0, "need room to punch: {free}");
    let f = feet(&mut app);
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, f + dir * 2.2);
    step(&mut app, 1);
    aim_at(&mut app, f + dir * 2.2 + Vec3::Y * 1.4);
    step(&mut app, 2);
    let hp0 = enemy(&app, baron).hp;
    let bars = app.world().resource::<SignalState>().bars;
    punch(&mut app);
    let en = enemy(&app, baron);
    let expected = punch_damage(bars);
    assert!((hp0 - en.hp - expected).abs() < 0.01, "punch dealt {} (expected {expected} at {bars} bars)", hp0 - en.hp);
    assert!(en.stun_timer > PUNCH_STUN - 0.05, "brief stun: {}", en.stun_timer);
    assert!(en.stagger_timer > 0.0, "staggered");
    assert_eq!(en.style_source, StyleSource::PhonePunch);
    assert_eq!(en.kill_cause, KillCause::Generic, "only a killing punch marks the fatality cause");
    let a = apps(&app);
    assert!(a.punch_hit && a.punch_anim > 0.8, "viewmodel jab armed: {a:?}");
    assert!(a.punch_cooldown > PUNCH_COOLDOWN - 0.05);
    // Mashing inside the cooldown does nothing.
    let hp1 = en.hp;
    punch(&mut app);
    assert_eq!(enemy(&app, baron).hp, hp1, "cooldown");
    // After the cooldown it lands again; the jab animation settles.
    step(&mut app, (PUNCH_COOLDOWN * 60.0) as usize + 2);
    assert_eq!(apps(&app).punch_anim, 0.0);
    let bpos = app.world().get::<GlobalTransform>(baron).unwrap().translation();
    aim_at(&mut app, bpos + Vec3::Y * 1.4);
    step(&mut app, 1);
    let hp2 = enemy(&app, baron).hp;
    punch(&mut app);
    assert!(enemy(&app, baron).hp < hp2, "second punch after the cooldown");

    // A punch that kills marks the guaranteed-fatality cause.
    let f = feet(&mut app);
    let imp = spawn_demon(&mut app, EnemyKind::Imp, f + dir * 1.8);
    step(&mut app, 1);
    app.world_mut().get_mut::<Enemy>(imp).unwrap().hp = 5.0;
    aim_at(&mut app, f + dir * 1.8 + Vec3::Y * 0.9);
    step(&mut app, (PUNCH_COOLDOWN * 60.0) as usize + 2);
    punch(&mut app);
    if let Some(e) = app.world().get::<Enemy>(imp) {
        assert!(!e.is_alive());
        assert_eq!((e.kill_cause, e.style_source), (KillCause::PhonePunch, StyleSource::PhonePunch));
    }
    step(&mut app, 4);
    assert!(app.world().resource::<StyleMeter>().points > 0.0, "punch kill scored style");
}

#[test]
fn punch_parries_projectiles_back_as_supercharged_gold_bolts() {
    let mut app = arena(332);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 12.0, "need a long lane: {free}");
    let e0 = eye(&mut app);
    aim_at(&mut app, e0 + dir * 10.0);
    step(&mut app, 2);
    with_player(&mut app, |p, _| {
        p.hp = 50.0;
        p.dash_cooldown = 2.0;
    });
    // A tanky Baron downrange, outside punch reach, and an incoming bolt.
    let f = feet(&mut app);
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, f + dir * 10.0);
    step(&mut app, 1);
    let hp0 = enemy(&app, baron).hp;
    let e1 = eye(&mut app);
    let shot = spawn_shot(&mut app, e1 + dir * 2.0, -dir * 1.0, 0.2);
    step(&mut app, 1);
    punch(&mut app);
    assert!(app.world().get_entity(shot).is_err(), "the bolt was swatted");
    let parried: Vec<PlayerProjectile> = app.world_mut().query::<&PlayerProjectile>().iter(app.world()).cloned().collect();
    assert_eq!(parried.len(), 1, "one reflected bolt");
    assert_eq!(parried[0].kind, ProjectileKind::Parried);
    assert!(parried[0].vel.length() >= PARRY_MIN_SPEED - 0.01);
    assert!(parried[0].vel.normalize().dot(dir) > 0.97, "flies along the crosshair");
    let p = player(&mut app);
    assert!((p.hp - (50.0 + PARRY_HEAL)).abs() < 0.01, "parry heal: {}", p.hp);
    assert_eq!(p.dash_cooldown, 0.0, "dash reset");
    assert!(apps(&app).parry_flash > 0.0);
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text.contains("PARRY")));
    step(&mut app, 2);
    let m = app.world().resource::<StyleMeter>().clone();
    assert!(m.points >= segnale_debole::style::PARRY_POINTS - 1.0, "parry style: {}", m.points);
    assert!(m.feed.iter().flatten().any(|(l, _)| *l == "PARRY"));
    // The bolt detonates on the Baron: direct 115 (or heavy splash).
    for _ in 0..40 {
        step(&mut app, 1);
        if app.world_mut().query::<&PlayerProjectile>().iter(app.world()).next().is_none() {
            break;
        }
    }
    assert!(app.world_mut().query::<&PlayerProjectile>().iter(app.world()).next().is_none(), "bolt detonated");
    let en = enemy(&app, baron);
    assert!(hp0 - en.hp > 80.0, "parry blast damage {} -> {}", hp0, en.hp);
    assert_eq!(en.style_source, StyleSource::Parry);
    // Heal never exceeds max HP.
    let mut app = arena(333);
    let (dir, _) = open_dir(&mut app);
    let e0 = eye(&mut app);
    aim_at(&mut app, e0 + dir * 10.0);
    step(&mut app, 2);
    let e1 = eye(&mut app);
    spawn_shot(&mut app, e1 + dir * 1.5, Vec3::ZERO, 0.2);
    step(&mut app, 1);
    punch(&mut app);
    let p = player(&mut app);
    assert!(p.hp <= p.max_hp + 1e-3);
}

#[test]
fn punched_barrel_flies_along_the_aim_and_detonates_on_a_demon() {
    let mut app = arena(334);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 12.0, "need a long lane: {free}");
    let f = feet(&mut app);
    let barrel = app.world_mut().spawn((LevelEntity, ExplosiveBarrel::default(), Transform::from_translation(f + dir * 2.2), Visibility::default())).id();
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, f + dir * 9.0);
    step(&mut app, 1);
    aim_at(&mut app, f + dir * 2.2 + Vec3::Y * 0.6);
    step(&mut app, 2);
    let hp0 = enemy(&app, baron).hp;
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.armor = 0.0;
        p.hp = p.max_hp;
    });
    punch(&mut app);
    let lb = *app.world().get::<LaunchedBarrel>(barrel).expect("barrel launched, not detonated in the face");
    assert!(app.world().get::<ExplosiveBarrel>(barrel).unwrap().is_intact());
    let h = Vec3::new(lb.vel.x, 0.0, lb.vel.z).normalize();
    assert!(h.dot(dir) > 0.9, "flies along the aim: {h} vs {dir}");
    assert!(lb.vel.y > 0.0, "with a slight lift");
    let start = app.world().get::<Transform>(barrel).unwrap().translation;
    step(&mut app, 3);
    let moved = app.world().get::<Transform>(barrel).map(|t| t.translation).unwrap_or(start);
    assert!((moved - start).dot(dir) > 0.5, "in flight");
    for _ in 0..90 {
        step(&mut app, 1);
        if app.world().get_entity(barrel).is_err() {
            break;
        }
    }
    assert!(app.world().get_entity(barrel).is_err(), "detonated on impact");
    let en = enemy(&app, baron);
    assert!(hp0 - en.hp > 40.0, "barrel blast on the Baron: {hp0} -> {}", en.hp);
    assert_eq!(en.style_source, StyleSource::PhonePunch, "the punch gets the credit");
    let p = player(&mut app);
    assert!(p.hp >= p.max_hp - 0.01, "no self-damage from the thrown barrel: {}", p.hp);

    // Without a demon in the lane, firing into a nearby wall detonates well
    // before the flight-fuse timeout expires.
    let mut app = arena(335);
    let grid = app.world().resource::<LevelGrid>().clone();
    let e0 = eye(&mut app);
    let wall_dir = (0..16)
        .filter_map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 16.0;
            let d = Vec3::new(a.cos(), 0.0, a.sin());
            raycast_grid(&grid, e0, d, 14.0).filter(|h| h.distance >= 3.5).map(|h| (d, h.distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("a wall within 3.5..14m")
        .0;
    let f = feet(&mut app);
    let barrel = app.world_mut().spawn((LevelEntity, ExplosiveBarrel::default(), Transform::from_translation(f + wall_dir * 2.0), Visibility::default())).id();
    step(&mut app, 1);
    aim_at(&mut app, f + wall_dir * 2.0 + Vec3::Y * 0.6);
    step(&mut app, 2);
    punch(&mut app);
    assert!(app.world().get::<LaunchedBarrel>(barrel).is_some());
    // Must detonate on terrain impact strictly before the 2.2 s timeout.
    step(&mut app, 45);
    assert!(app.world().get_entity(barrel).is_err(), "detonated on wall impact well before fuse timeout");
}

#[test]
fn camera_flash_blinds_the_cone_clears_weak_shots_and_spares_heavy_bolts() {
    let mut app = arena(336);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 10.0, "need a lane: {free}");
    // Spoof 4 bars so the 2-bar gate is open.
    app.world_mut().resource_mut::<PhoneApps>().spoof_timer = 1.0e4;
    step(&mut app, 2);
    assert!(app.world().resource::<SignalState>().bars >= 2);
    let f = feet(&mut app);
    let e0 = eye(&mut app);
    aim_at(&mut app, e0 + dir * 8.0);
    let front = spawn_demon(&mut app, EnemyKind::HellBaron, f + dir * 8.0);
    let sniper = spawn_demon(&mut app, EnemyKind::RelaySniper, f + dir * 6.5);
    let stalker = spawn_demon(&mut app, EnemyKind::PhaseStalker, f + dir * 7.2);
    let behind = spawn_demon(&mut app, EnemyKind::HellBaron, f - dir * 5.0);
    step(&mut app, 2);
    // Actively arm Sniper lock-on and Stalker blink before flashing.
    app.world_mut().get_mut::<Enemy>(sniper).unwrap().sniper_lock_timer = 0.85;
    app.world_mut().get_mut::<Enemy>(stalker).unwrap().blink_timer = 0.85;
    let e1 = eye(&mut app);
    let weak = spawn_shot(&mut app, e1 + dir * 6.0, Vec3::ZERO, 0.2);
    let heavy = spawn_shot(&mut app, e1 + dir * 6.0 + Vec3::Y * 0.8, Vec3::ZERO, HEAVY_BOLT_RADIUS + 0.1);
    step(&mut app, 1);
    let hp_front = enemy(&app, front).hp;
    let hp_behind = enemy(&app, behind).hp;
    flash(&mut app);
    let fe = enemy(&app, front);
    assert!(fe.stun_timer > FLASH_STUN - 0.1, "blinded: {}", fe.stun_timer);
    assert!((hp_front - fe.hp - FLASH_DAMAGE).abs() < 0.01, "flash burn {}", hp_front - fe.hp);
    assert_eq!(fe.style_source, StyleSource::CameraFlash);
    assert_eq!(enemy(&app, sniper).sniper_lock_timer, 0.0, "sniper lock-on cancelled");
    assert_eq!(enemy(&app, stalker).blink_timer, 0.0, "stalker blink cancelled");
    let be = enemy(&app, behind);
    assert!(be.stun_timer <= 0.0 && be.hp == hp_behind, "behind the player: untouched");
    assert!(app.world().get_entity(weak).is_err(), "weak shot vaporized");
    assert!(app.world().get_entity(heavy).is_ok(), "heavy hellfire orb survives");
    let a = apps(&app);
    assert!(a.flash_cooldown > FLASH_COOLDOWN * app.world().resource::<PlayerModifiers>().ping_cooldown_mult - 0.1);
    assert!(a.flash_anim > 0.8);
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text.contains("BLINDED + 1 SHOTS CLEARED")), "toast");
    step(&mut app, 2);
    assert!(app.world().resource::<StyleMeter>().feed.iter().flatten().any(|(l, _)| *l == "FLASHBANGED"));
    // Cooling down: a second strobe is denied and changes nothing.
    let stun_before = enemy(&app, front).hp;
    flash(&mut app);
    assert_eq!(enemy(&app, front).hp, stun_before, "denied while cooling down");
    assert!(app.world().resource::<HudFeed>().toasts.iter().any(|t| t.text.starts_with("CAMERA FLASH: ") && t.text.ends_with('s')));

    // Dedicated solid-wall occlusion check: place a solid wall cell directly
    // between the player and a demon inside flash range and confirm the flash
    // does not penetrate cover.
    let mut app = arena(337);
    let (wdir, wfree) = open_dir(&mut app);
    assert!(wfree > 10.0, "need a lane for wall occlusion check: {wfree}");
    app.world_mut().resource_mut::<PhoneApps>().spoof_timer = 1.0e4;
    step(&mut app, 2);
    let wf = feet(&mut app);
    let we0 = eye(&mut app);
    let walled_pos = wf + wdir * 9.0;
    let wall_cell = app.world().resource::<LevelGrid>().world_to_cell(wf + wdir * 4.5);
    app.world_mut().resource_mut::<LevelGrid>().set(wall_cell, segnale_debole::level::Cell::Wall);
    {
        let grid = app.world().resource::<LevelGrid>();
        assert!(
            EnemyKind::HellBaron.stats(1).hit_spheres.iter().all(|&(hy, _)| !segnale_debole::physics::has_line_of_sight(grid, we0, walled_pos + Vec3::Y * hy)),
            "inserted wall must block every hit sphere"
        );
    }
    aim_at(&mut app, walled_pos + Vec3::Y * 1.0);
    let walled = spawn_demon(&mut app, EnemyKind::HellBaron, walled_pos);
    step(&mut app, 1);
    let hp_walled = enemy(&app, walled).hp;
    flash(&mut app);
    let we = enemy(&app, walled);
    assert!(we.stun_timer <= 0.0 && we.hp == hp_walled, "solid wall blocks camera flash");
}
