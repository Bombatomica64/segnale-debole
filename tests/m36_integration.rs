//! Milestone 36 "Articulated Enemy Animations, Combat Telegraphs & Bestiary
//! Preview": headless integration tests for the limb / jaw / cannon rigs, the
//! pre-fire wind-up telegraph (and its stun cancel), whole-demon tinting and
//! the Bestiary `ATTACK` preview.

use bevy::ecs::system::RunSystemOnce;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::ButtonState;
use bevy::prelude::*;

use segnale_debole::apps::PhoneApps;
use segnale_debole::bot_input::BotControl;
use segnale_debole::compendium::{CompendiumState, ExhibitAnim, ExhibitId, ExhibitModel, ATTACK_PROMPT_KBM};
use segnale_debole::enemies::{
    attack_windup_secs, spawn_enemy, Enemy, EnemyAssets, EnemyBody, EnemyKind, EnemyLook, EnemyProjectile, EnemyRig, RigKind, RigPivot,
    HIT_FLASH_SECS, LOOK_ENRAGED,
};
use segnale_debole::hud::HudPlugin;
use segnale_debole::level::LevelGrid;
use segnale_debole::phone_combat::FLASH_STUN;
use segnale_debole::physics::raycast_grid;
use segnale_debole::player::{Player, PLAYER_EYE_HEIGHT};
use segnale_debole::signal::SignalState;
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin};

const FPS: f32 = 60.0;

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

fn feet(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

fn eye(app: &mut App) -> Vec3 {
    feet(app) + Vec3::Y * PLAYER_EYE_HEIGHT
}

fn godmode(app: &mut App) {
    with_player(app, |p, _| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
}

fn aim_at(app: &mut App, target: Vec3) {
    let d = target - eye(app);
    let yaw = segnale_debole::level::yaw_facing(Vec2::new(d.x, d.z));
    let pitch = d.y.atan2(Vec2::new(d.x, d.z).length());
    let mut b = app.world_mut().resource_mut::<BotControl>();
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

fn enemy(app: &App, e: Entity) -> Enemy {
    app.world().get::<Enemy>(e).expect("demon alive").clone()
}

fn edit(app: &mut App, e: Entity, f: impl FnOnce(&mut Enemy)) {
    f(&mut app.world_mut().get_mut::<Enemy>(e).unwrap());
}

fn projectiles(app: &mut App) -> usize {
    app.world_mut().query::<&EnemyProjectile>().iter(app.world()).count()
}

fn clear_projectiles(app: &mut App) {
    let shots: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<EnemyProjectile>>().iter(app.world()).collect();
    for s in shots {
        app.world_mut().despawn(s);
    }
}

fn despawn_demons(app: &mut App) {
    let demons: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    for e in demons {
        app.world_mut().despawn(e);
    }
    clear_projectiles(app);
}

/// A fresh run with an empty garrison and an invulnerable player.
fn arena(seed: u64) -> App {
    let mut app = build_headless_test_app();
    app.world_mut()
        .run_system_once(move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, seed);
        })
        .unwrap();
    step(&mut app, 3);
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
    godmode(&mut app);
    despawn_demons(&mut app);
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
            let free = raycast_grid(&grid, e, d, 40.0).map_or(40.0, |h| h.distance);
            let low = raycast_grid(&grid, e - Vec3::Y * 1.2, d, 40.0).map_or(40.0, |h| h.distance);
            (d, free.min(low))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

/// Current local transform of a demon's rig pivot of `kind`.
fn rig_tf(app: &mut App, demon: Entity, kind: RigKind) -> Transform {
    let rig = *app.world().get::<EnemyRig>(demon).expect("articulated demon");
    for pe in rig.pivots.iter().flatten() {
        let rp = app.world().get::<RigPivot>(*pe).unwrap();
        if rp.kind == kind {
            return *app.world().get::<Transform>(*pe).unwrap();
        }
    }
    panic!("{kind:?} rig missing");
}

fn rig_rest(app: &mut App, demon: Entity, kind: RigKind) -> Transform {
    let rig = *app.world().get::<EnemyRig>(demon).unwrap();
    rig.pivots.iter().flatten().map(|pe| *app.world().get::<RigPivot>(*pe).unwrap()).find(|rp| rp.kind == kind).unwrap().rest
}

fn body_pivot(app: &mut App, demon: Entity) -> Transform {
    let children: Vec<Entity> = app.world().get::<Children>(demon).unwrap().iter().collect();
    children.into_iter().find_map(|c| app.world().get::<EnemyBody>(c).map(|_| *app.world().get::<Transform>(c).unwrap())).unwrap()
}

fn swing(a: Transform, b: Transform) -> f32 {
    a.rotation.angle_between(b.rotation) + a.translation.distance(b.translation)
}

/// The rig that best shows each kind's attack telegraph.
fn telegraph_rig(kind: EnemyKind) -> RigKind {
    match kind {
        EnemyKind::Cacodemon => RigKind::CacoJaw,
        EnemyKind::RelaySniper => RigKind::SniperCannon,
        _ => RigKind::ArmRight,
    }
}

#[test]
fn live_demons_stride_wind_up_release_and_slump_with_articulated_rigs() {
    let mut app = arena(361);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 14.0, "need a firing lane: {free}");
    let f = feet(&mut app);
    for kind in [EnemyKind::Imp, EnemyKind::Cacodemon, EnemyKind::JammerFiend, EnemyKind::HellBaron, EnemyKind::PhaseStalker] {
        despawn_demons(&mut app);
        godmode(&mut app);
        let pos = segnale_debole::enemies::spawn_pos_for(kind, &app.world().resource::<LevelGrid>().clone(), app.world().resource::<LevelGrid>().world_to_cell(f + dir * 12.0));
        let d = spawn_demon(&mut app, kind, pos);
        step(&mut app, 2);
        edit(&mut app, d, |e| {
            e.alerted = true;
            e.attack_timer = 0.8;
        });
        // Movement: the legs (walkers) / tendrils (Cacodemon) cycle while it closes in.
        let moving = if kind == EnemyKind::Cacodemon { RigKind::CacoTendrilsRight } else { RigKind::LegRight };
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for _ in 0..30 {
            godmode(&mut app);
            step(&mut app, 1);
            let p = rig_tf(&mut app, d, moving).rotation.to_euler(EulerRot::YXZ).1;
            lo = lo.min(p);
            hi = hi.max(p);
            assert_eq!(enemy(&app, d).attack_windup, 0.0, "{kind:?}: no wind-up before the cooldown");
        }
        assert!(hi - lo > 0.12, "{kind:?}: {moving:?} animates while moving ({lo:.2}..{hi:.2})");

        // Wind-up -> release: the shot only leaves after the telegraph.
        clear_projectiles(&mut app);
        edit(&mut app, d, |e| {
            e.attack_timer = 0.0;
            e.blink_cooldown = 10.0;
            e.blink_timer = 0.0;
        });
        let rig = telegraph_rig(kind);
        let rest = rig_rest(&mut app, d, rig);
        let mut start = None;
        let mut fired = None;
        let mut coiled = 0.0f32;
        let mut coiled_pose = Transform::IDENTITY;
        for frame in 0..90 {
            godmode(&mut app);
            step(&mut app, 1);
            let e = enemy(&app, d);
            if start.is_none() && e.attack_windup > 0.0 {
                start = Some(frame);
            }
            if e.attack_windup > 0.0 {
                assert_eq!(projectiles(&mut app), 0, "{kind:?}: nothing fires during the wind-up");
                let tf = rig_tf(&mut app, d, rig);
                if swing(tf, rest) > coiled {
                    coiled = swing(tf, rest);
                    coiled_pose = tf;
                }
            }
            if start.is_some() && projectiles(&mut app) > 0 {
                fired = Some(frame);
                break;
            }
        }
        let (s, fr) = (start.unwrap_or_else(|| panic!("{kind:?} wound up")), fired.unwrap_or_else(|| panic!("{kind:?} fired")));
        let secs = (fr - s) as f32 / FPS;
        assert!((secs - attack_windup_secs(kind)).abs() < 0.06, "{kind:?}: wind-up {secs:.3}s vs {}", attack_windup_secs(kind));
        assert!(coiled > 0.35, "{kind:?}: {rig:?} coils visibly ({coiled:.2})");
        let e = enemy(&app, d);
        assert!(e.attack_anim > 0.8 && !e.attack_lunge, "{kind:?}: release follow-through");
        assert!(e.attack_timer >= 0.4, "{kind:?}: cadence keeps a cooldown");
        step(&mut app, 4);
        assert!(swing(rig_tf(&mut app, d, rig), coiled_pose) > 0.2, "{kind:?}: release snaps out of the coil");

        // Stun: hunched / sagging slump on the body pivot and drooping limbs.
        let calm = rig_tf(&mut app, d, rig);
        edit(&mut app, d, |e| e.stun_timer = 1.0);
        step(&mut app, 8);
        assert!(swing(rig_tf(&mut app, d, rig), calm) > 0.1, "{kind:?}: {rig:?} droops while stunned");
        let tip = body_pivot(&mut app, d).rotation * Vec3::Y;
        assert!(tip.z < -0.05, "{kind:?}: body slumps forward while stunned ({tip})");
    }
}

#[test]
fn camera_flash_mid_windup_cancels_the_shot() {
    let mut app = arena(362);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 12.0, "need a lane: {free}");
    app.world_mut().resource_mut::<PhoneApps>().spoof_timer = 1.0e4;
    step(&mut app, 2);
    assert!(app.world().resource::<SignalState>().bars >= 2);
    let f = feet(&mut app);
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, f + dir * 8.0);
    aim_at(&mut app, f + dir * 8.0 + Vec3::Y * 1.6);
    step(&mut app, 2);
    edit(&mut app, baron, |e| {
        e.alerted = true;
        e.attack_timer = 0.0;
    });
    // Let the hellfire heave start, then strobe it halfway through.
    let mut wound = false;
    for _ in 0..40 {
        godmode(&mut app);
        step(&mut app, 1);
        let e = enemy(&app, baron);
        if e.attack_windup > 0.0 && e.windup_progress() > 0.3 {
            wound = true;
            break;
        }
    }
    assert!(wound, "baron started its wind-up");
    let arm_coiled = rig_tf(&mut app, baron, RigKind::ArmRight);
    app.world_mut().resource_mut::<BotControl>().flash = true;
    step(&mut app, 2);
    let e = enemy(&app, baron);
    assert!(e.stun_timer > FLASH_STUN - 0.2, "blinded: {}", e.stun_timer);
    assert_eq!(e.attack_windup, 0.0, "stun cancels the wind-up");
    for _ in 0..(FPS as usize / 2) {
        godmode(&mut app);
        step(&mut app, 1);
        assert_eq!(projectiles(&mut app), 0, "interrupted shot never fires");
    }
    assert!(swing(rig_tf(&mut app, baron, RigKind::ArmRight), arm_coiled) > 0.5, "gauntlets drop out of the heave");

    // A raw stun (Phone Punch / EMP path) cancels the same way.
    let imp = spawn_demon(&mut app, EnemyKind::Imp, f + dir * 10.0);
    step(&mut app, 2);
    edit(&mut app, imp, |e| {
        e.alerted = true;
        e.stun_timer = 0.0;
        e.attack_timer = 0.0;
    });
    for _ in 0..30 {
        godmode(&mut app);
        step(&mut app, 1);
        if enemy(&app, imp).attack_windup > 0.0 {
            break;
        }
    }
    assert!(enemy(&app, imp).attack_windup > 0.0, "imp wound up");
    clear_projectiles(&mut app);
    edit(&mut app, imp, |e| e.stun_timer = 0.6);
    step(&mut app, 1);
    assert_eq!(enemy(&app, imp).attack_windup, 0.0);
    step(&mut app, 20);
    assert_eq!(projectiles(&mut app), 0, "stunned imp never releases the fireball");
}

#[test]
fn hit_flash_and_enrage_tint_every_articulated_body_part() {
    let mut app = arena(363);
    let (dir, _) = open_dir(&mut app);
    let f = feet(&mut app);
    let ea = app.world().resource::<EnemyAssets>().clone();
    for (i, kind) in EnemyKind::ALL.into_iter().enumerate() {
        if kind == EnemyKind::CarrierDaemon {
            continue; // boss tint follows the same path; it needs its arena.
        }
        let d = spawn_demon(&mut app, kind, f + dir * (4.0 + i as f32 * 2.5) + Vec3::Y * if kind.flying() { 2.0 } else { 0.0 });
        step(&mut app, 2);
        let look = *app.world().get::<EnemyLook>(d).unwrap();
        let bodies: Vec<Entity> = look.bodies().collect();
        let rig_bodies = ea.rigs[kind.index()].iter().filter(|r| r.parts.iter().any(|p| p.role == segnale_debole::enemies::PartRole::Body)).count();
        assert_eq!(bodies.len(), 1 + rig_bodies, "{kind:?}: torso + every articulated hide part registered");
        if matches!(kind, EnemyKind::Imp | EnemyKind::JammerFiend | EnemyKind::HellBaron | EnemyKind::PhaseStalker) {
            assert_eq!(bodies.len(), 5, "{kind:?}: torso + 2 arms + 2 legs");
        }
        let mats = ea.look(kind);
        let mat = |app: &App, e: Entity| app.world().get::<MeshMaterial3d<StandardMaterial>>(e).unwrap().0.id();
        // Every entity wearing the body material is tracked by the look.
        let mut wearing = 0;
        let mut stack: Vec<Entity> = vec![d];
        while let Some(e) = stack.pop() {
            if let Some(m) = app.world().get::<MeshMaterial3d<StandardMaterial>>(e)
                && mats.body.iter().any(|h| h.id() == m.0.id())
            {
                wearing += 1;
                assert!(bodies.contains(&e), "{kind:?}: untracked body part {e}");
            }
            if let Some(c) = app.world().get::<Children>(e) {
                stack.extend(c.iter());
            }
        }
        assert_eq!(wearing, bodies.len(), "{kind:?}");
        // Hit-flash flashes the whole demon.
        edit(&mut app, d, |e| e.hit_flash = HIT_FLASH_SECS);
        step(&mut app, 1);
        for b in &bodies {
            let id = mat(&app, *b);
            assert!(mats.flash.iter().flatten().any(|h| h.id() == id), "{kind:?}: limb {b} flashes with the torso");
        }
        // Dead-Zone enrage glows violet across torso AND limbs.
        edit(&mut app, d, |e| {
            e.hit_flash = 0.0;
            e.enraged = true;
            e.enrage_check = 100.0;
            e.stun_timer = 0.0;
        });
        step(&mut app, 2);
        for b in &bodies {
            assert_eq!(mat(&app, *b), mats.body[LOOK_ENRAGED].id(), "{kind:?}: enraged violet on every body part");
        }
        app.world_mut().despawn(d);
    }
}

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

fn ui_texts(app: &mut App) -> Vec<(String, bool)> {
    let mut q = app.world_mut().query::<(Entity, &Text)>();
    let found: Vec<(Entity, String)> = q.iter(app.world()).map(|(e, t)| (e, t.0.clone())).collect();
    found
        .into_iter()
        .map(|(e, t)| {
            // Visible unless an ancestor node is `Display::None`.
            let mut shown = true;
            let mut cur = Some(e);
            while let Some(c) = cur {
                if app.world().get::<Node>(c).is_some_and(|n| n.display == Display::None) {
                    shown = false;
                }
                cur = app.world().get::<ChildOf>(c).map(|p| p.parent());
            }
            (t, shown)
        })
        .collect()
}

fn exhibit_pivots(app: &mut App, id: ExhibitId) -> Vec<(RigKind, Transform)> {
    let mut q = app.world_mut().query::<(&ExhibitModel, &ExhibitAnim)>();
    let anim = *q.iter(app.world()).find(|(m, _)| m.0 == id).unwrap().1;
    anim.pivots.iter().flatten().map(|pe| (app.world().get::<RigPivot>(*pe).unwrap().kind, *app.world().get::<Transform>(*pe).unwrap())).collect()
}

fn preview(app: &mut App, id: ExhibitId) -> Option<f32> {
    let mut q = app.world_mut().query::<(&ExhibitModel, &ExhibitAnim)>();
    q.iter(app.world()).find(|(m, _)| m.0 == id).unwrap().1.preview
}

#[test]
fn bestiary_exhibits_idle_live_and_preview_attacks() {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    tap(&mut app, KeyCode::KeyB);
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Compendium);
    let assets = |app: &App| (app.world().resource::<Assets<Mesh>>().len(), app.world().resource::<Assets<StandardMaterial>>().len());
    let before = assets(&app);
    // Every demon exhibit carries its articulated rigs.
    for id in ExhibitId::BESTIARY {
        let kind = id.enemy_kind().unwrap();
        let n = app.world().resource::<EnemyAssets>().rigs[kind.index()].len();
        assert_eq!(exhibit_pivots(&mut app, id).len(), n, "{id:?}: every rig pivot is animated");
    }
    // The visible exhibit idles live (breathing / sway).
    let p0 = exhibit_pivots(&mut app, ExhibitId::Imp);
    step(&mut app, 40);
    let p1 = exhibit_pivots(&mut app, ExhibitId::Imp);
    assert!(p0.iter().zip(&p1).any(|(a, b)| swing(a.1, b.1) > 0.01), "imp exhibit breathes on the turntable");
    assert!(ui_texts(&mut app).iter().any(|(t, shown)| t == ATTACK_PROMPT_KBM && *shown), "ATTACK prompt on the Bestiary");

    // SPACE plays the full wind-up -> attack cycle for every demon.
    for (i, id) in ExhibitId::BESTIARY.into_iter().enumerate() {
        app.world_mut().resource_mut::<CompendiumState>().select(id);
        step(&mut app, 2);
        let kind = id.enemy_kind().unwrap();
        let idle = exhibit_pivots(&mut app, id);
        let seq = app.world().resource::<CompendiumState>().attack_preview;
        tap(&mut app, KeyCode::Space);
        assert_eq!(app.world().resource::<CompendiumState>().attack_preview, seq + 1, "{id:?}: SPACE requests the preview");
        assert!(preview(&mut app, id).is_some(), "{id:?}: preview running");
        let mut peak = 0.0f32;
        let total = segnale_debole::enemies::attack_preview_len(kind);
        for _ in 0..((total * FPS) as usize + 4) {
            step(&mut app, 1);
            let now = exhibit_pivots(&mut app, id);
            for ((ka, a), (kb, b)) in idle.iter().zip(&now) {
                assert_eq!(ka, kb);
                assert!(b.is_finite(), "{id:?} {kb:?}");
                if *kb != RigKind::BossHalo && *kb != RigKind::FiendEmitter {
                    peak = peak.max(swing(*a, *b));
                }
            }
        }
        assert!(peak > 0.3, "{id:?}: attack preview animates the rigs ({peak:.2})");
        assert!(preview(&mut app, id).is_none(), "{id:?}: preview finishes and returns to idle");
        if i == 0 {
            assert_eq!(assets(&app), before, "previews allocate no meshes or materials");
        }
    }

    // Armory: no ATTACK prompt and SPACE does nothing.
    tap(&mut app, KeyCode::Tab);
    assert!(ui_texts(&mut app).iter().all(|(t, shown)| t != ATTACK_PROMPT_KBM || !*shown), "ATTACK hidden on weapon exhibits");
    let seq = app.world().resource::<CompendiumState>().attack_preview;
    tap(&mut app, KeyCode::Space);
    assert_eq!(app.world().resource::<CompendiumState>().attack_preview, seq);
    assert_eq!(assets(&app), before);
}
