//! Milestone 31 "Overclock & Chaos": headless integration tests for the
//! Bandwidth Style Rank meter, the shootable CRT / explosive UPS props, the
//! Time Trial PB Signal Ghost and controller rumble gating.

use bevy::ecs::system::RunSystemOnce;
use bevy::input::ButtonState;
use bevy::input::gamepad::{Gamepad, GamepadButton, GamepadRumbleRequest};
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::prelude::*;

use segnale_debole::architecture::{plan_interactive_props, telecom_slot_ok, InteractiveKind, INTERACTIVE_MAX_DEPTH};
use segnale_debole::bot_input::BotControl;
use segnale_debole::enemies::{spawn_enemy, Enemy, EnemyAssets, EnemyKind};
use segnale_debole::fx::FxAssets;
use segnale_debole::gamepad::InputMode;
use segnale_debole::hud::{HudPlugin, StyleEmblem, StyleEmblemArt, StyleHud};
use segnale_debole::level::{LevelEntity, LevelGrid, CELL_SIZE};
use segnale_debole::meta::{ProfileStore, RunLoadout};
use segnale_debole::player::{Player, PLAYER_EYE_HEIGHT};
use segnale_debole::props::{spawn_ups, CrtMonitor, PropAssets, UpsRack, UPS_CHAIN_FUSE, UPS_EMP_STUN, UPS_SELF_DAMAGE_CAP};
use segnale_debole::rumble::{RumbleCommand, RumblePulse, RumbleState};
use segnale_debole::state::{complete_floor, start_run, AppState, PlayerModifiers, RunMode, RunStats};
use segnale_debole::style::{StyleMeter, StyleRank};
use segnale_debole::style_emblem::{EMBLEM_BYTES, EMBLEM_PX};
use segnale_debole::trials::{start_time_trial, time_trial_gates, SignalGhost, TimeTrialState, TrialGhostStore};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin};

const FPS: usize = 60;

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn app_state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn meter(app: &App) -> StyleMeter {
    app.world().resource::<StyleMeter>().clone()
}

fn with_player(app: &mut App, f: impl FnOnce(&mut Player, &mut Transform)) {
    let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
    let (mut p, mut tf) = q.single_mut(app.world_mut()).expect("exactly one player");
    f(&mut p, &mut tf);
}

fn player(app: &mut App) -> Player {
    let mut q = app.world_mut().query::<&Player>();
    q.single(app.world()).expect("exactly one player").clone()
}

fn player_pos(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

fn godmode(app: &mut App) {
    with_player(app, |p, _| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
}

fn teleport(app: &mut App, feet: Vec3) {
    with_player(app, |p, tf| {
        tf.translation = feet;
        p.velocity = Vec3::ZERO;
        p.prev_pos = Some(feet);
    });
}

fn bot(app: &mut App) -> Mut<'_, BotControl> {
    app.world_mut().resource_mut::<BotControl>()
}

fn aim_at(app: &mut App, target: Vec3) {
    let eye = player_pos(app) + Vec3::Y * PLAYER_EYE_HEIGHT;
    let d = target - eye;
    let yaw = segnale_debole::level::yaw_facing(Vec2::new(d.x, d.z));
    let pitch = d.y.atan2(Vec2::new(d.x, d.z).length());
    let mut b = bot(app);
    b.look_yaw = Some(yaw);
    b.look_pitch = Some(pitch);
}

fn count<C: Component>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<(), With<C>>();
    q.iter(app.world()).count()
}

fn begin_run(app: &mut App, seed: u64) {
    app.world_mut()
        .run_system_once(move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, seed);
        })
        .unwrap();
    step(app, 3);
    assert_eq!(app_state(app), AppState::Playing);
}

fn set_state(app: &mut App, s: AppState) {
    app.world_mut().resource_mut::<NextState<AppState>>().set(s);
    step(app, 2);
    assert_eq!(app_state(app), s);
}

fn tap_key(app: &mut App, key: KeyCode) {
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

fn spawn_demon(app: &mut App, kind: EnemyKind, pos: Vec3) -> Entity {
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>, mut mats: ResMut<Assets<StandardMaterial>>, run: Res<RunStats>| {
            spawn_enemy(&mut commands, &ea, &mut mats, kind, pos, run.floor, false)
        })
        .unwrap()
}

/// Kill a demon as if `source` dealt the blow (it dies in this frame's `enemy_deaths`).
fn kill(app: &mut App, e: Entity, source: segnale_debole::style::StyleSource) {
    let mut en = app.world_mut().get_mut::<Enemy>(e).unwrap();
    en.hp = 0.0;
    en.style_source = source;
}

fn material_count(app: &App) -> usize {
    app.world().resource::<Assets<StandardMaterial>>().len()
}

/// The style widget's display and the rank letter its emblem last drew.
fn style_hud_letter(app: &mut App) -> (Display, String) {
    let mut roots = app.world_mut().query::<(&StyleHud, &Node)>();
    let display = roots.iter(app.world()).find(|(h, _)| **h == StyleHud::Root).map(|(_, n)| n.display).unwrap();
    let mut emblems = app.world_mut().query::<&StyleEmblem>();
    let letter = emblems.single(app.world()).unwrap().rank.map(|r| r.letter().to_string()).unwrap_or_default();
    (display, letter)
}

/// `(texels with any alpha, alpha of the four corners)` of the live emblem texture.
fn emblem_coverage(app: &App) -> (usize, [u8; 4]) {
    let art = app.world().resource::<StyleEmblemArt>();
    let img = app.world().resource::<Assets<Image>>().get(&art.image).expect("emblem texture");
    let data = img.data.as_ref().expect("cpu-side texels");
    assert_eq!(data.len(), EMBLEM_BYTES);
    let alpha = |x: usize, y: usize| data[(y * EMBLEM_PX + x) * 4 + 3];
    let last = EMBLEM_PX - 1;
    (data.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count(), [alpha(0, 0), alpha(last, 0), alpha(0, last), alpha(last, last)])
}

// ---------------------------------------------------------------------------
// Pillar 1: Bandwidth Style Rank
// ---------------------------------------------------------------------------

#[test]
fn style_meter_climbs_on_mixed_kills_drops_on_damage_and_is_run_scoped() {
    use segnale_debole::style::StyleSource::*;
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    tap_key(&mut app, KeyCode::Enter);
    assert_eq!(app_state(&app), AppState::Playing);
    godmode(&mut app);
    step(&mut app, 5);
    assert_eq!(meter(&app).points, 0.0);
    assert_eq!(style_hud_letter(&mut app).0, Display::None, "hidden at zero points");
    assert_eq!(emblem_coverage(&app).0, 0, "nothing drawn while hidden");
    let images_before = app.world().resource::<Assets<Image>>().len();

    // Nonstop mixed-weapon multi-kills climb the ladder to 6G / 6G+.
    let feet = player_pos(&mut app);
    let sources = [Shotgun, Rocket, Plasma, Overload, Prop];
    let mut climbed = Vec::new();
    for i in 0..18 {
        let e = spawn_demon(&mut app, EnemyKind::HellBaron, feet + Vec3::new(3.0, 0.0, 3.0));
        step(&mut app, 1);
        kill(&mut app, e, sources[i % sources.len()]);
        step(&mut app, 6);
        if let Some(r) = meter(&app).rank()
            && climbed.last() != Some(&r)
        {
            climbed.push(r);
        }
    }
    let m = meter(&app);
    assert!(m.rank() >= Some(StyleRank::SS), "{climbed:?} {}", m.points);
    assert!(climbed.windows(2).all(|w| w[0] < w[1]), "monotonic climb {climbed:?}");
    assert!(m.feed.iter().flatten().count() > 0, "style feed lines");
    let run = app.world().resource::<RunStats>().clone();
    assert!(run.peak_style_rank >= Some(StyleRank::SS));
    let (display, letter) = style_hud_letter(&mut app);
    assert_eq!(display, Display::Flex);
    assert_eq!(letter, m.rank().unwrap().letter());
    // The procedural emblem animates in place: one texture, redrawn, never reallocated.
    let (drawn, corners) = emblem_coverage(&app);
    assert!(drawn > EMBLEM_PX * EMBLEM_PX / 5, "emblem texels drawn: {drawn}");
    assert_eq!(corners, [0; 4], "floating emblem: transparent corners, no plate");
    let mut emblems = app.world_mut().query::<&StyleEmblem>();
    let redraws = emblems.single(app.world()).unwrap().redraws;
    assert!(redraws > 20, "emblem redrawn across the climb: {redraws}");
    assert_eq!(app.world().resource::<Assets<Image>>().len(), images_before, "zero new Image assets");
    let mut plates = app.world_mut().query::<(&StyleHud, Option<&BackgroundColor>, Option<&BorderColor>)>();
    for (part, bg, border) in plates.iter(app.world()) {
        assert!(bg.is_none_or(|b| b.0.alpha() == 0.0), "{part:?}: no background box");
        assert!(border.is_none_or(|b| *b == BorderColor::default() || [b.top, b.right, b.bottom, b.left].iter().all(|c| c.alpha() == 0.0)), "{part:?}: no border");
    }

    // DMC damage penalty: one clean rank down.
    let before = meter(&app).rank().unwrap();
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.apply_damage(5.0);
    });
    step(&mut app, 1);
    assert_eq!(meter(&app).rank(), before.prev(), "damage demotes exactly one tier");
    assert!(meter(&app).hit_shake > 0.5, "the hit starts the glitch shake");
    assert_eq!(style_hud_letter(&mut app).1, before.prev().unwrap().letter(), "the emblem redraws the demoted rank at once");
    godmode(&mut app);

    // Pause <-> Playing and UpgradeSelect -> Playing keep the meter.
    let banked = meter(&app).points;
    tap_key(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    tap_key(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Playing);
    assert!(meter(&app).points > 0.0 && meter(&app).points <= banked);
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| {
            complete_floor(&mut run, &mut next);
        })
        .unwrap();
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    bot(&mut app).pick_upgrade = Some(0);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(app.world().resource::<RunStats>().floor, 2);
    assert!(meter(&app).points > 0.0, "the next floor keeps the style");

    // Pause -> RESTART RUN [R] resets it (fresh Floor 1 at t = 0).
    tap_key(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::Paused);
    tap_key(&mut app, KeyCode::KeyR);
    assert_eq!(app_state(&app), AppState::Playing);
    assert_eq!(app.world().resource::<RunStats>().floor, 1);
    assert_eq!(meter(&app).points, 0.0, "restart clears the meter");
    assert_eq!(app.world().resource::<RunStats>().peak_style_rank, None);

    // Main menu resets it too.
    let at = player_pos(&mut app) + Vec3::new(3.0, 0.0, 0.0);
    let e = spawn_demon(&mut app, EnemyKind::Imp, at);
    step(&mut app, 1);
    kill(&mut app, e, Shotgun);
    step(&mut app, 2);
    assert!(meter(&app).points > 0.0);
    set_state(&mut app, AppState::MainMenu);
    assert_eq!(meter(&app).points, 0.0, "main menu clears the meter");
}

#[test]
fn style_meter_stays_off_in_the_time_trial() {
    let mut app = build_headless_test_app();
    begin_time_trial(&mut app);
    step(&mut app, 10);
    app.world_mut().resource_mut::<segnale_debole::style::StyleQueue>().push(segnale_debole::style::StyleAction::JammerDestroyed);
    step(&mut app, 2);
    assert_eq!(meter(&app).points, 0.0, "no style scoring in the Time Trial");
}

// ---------------------------------------------------------------------------
// Pillar 2: shootable CRTs + explosive UPS racks
// ---------------------------------------------------------------------------

fn props_app(seed: u64) -> (App, LevelGrid) {
    let mut app = build_headless_test_app();
    begin_run(&mut app, seed);
    godmode(&mut app);
    step(&mut app, 3);
    let g = app.world().resource::<LevelGrid>().clone();
    // Clear the garrison: these scenarios stage their own demons.
    let demons: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    for e in demons {
        app.world_mut().despawn(e);
    }
    (app, g)
}

#[test]
fn props_are_capped_wall_flush_shared_and_torn_down() {
    let (mut app, g) = props_app(31);
    let plan = plan_interactive_props(&g);
    let crts = plan.iter().filter(|p| p.kind == InteractiveKind::Crt).count();
    let upss = plan.iter().filter(|p| p.kind == InteractiveKind::Ups).count();
    assert!((1..=8).contains(&crts) && (1..=6).contains(&upss), "crts {crts} ups {upss}");
    assert_eq!(count::<CrtMonitor>(&mut app), crts);
    assert_eq!(count::<UpsRack>(&mut app), upss);
    assert_eq!(plan, plan_interactive_props(&g), "deterministic per floor");
    for p in &plan {
        assert!(telecom_slot_ok(&g, p.face), "{p:?}: lane stays open (>= 2-cell corridor or room)");
    }
    // Every prop box stays within 0.55 m of its wall face.
    let mut q = app.world_mut().query::<&CrtMonitor>();
    let mut boxes: Vec<_> = q.iter(app.world()).map(|c| (c.bounds, c.normal)).collect();
    let mut q = app.world_mut().query::<&UpsRack>();
    boxes.extend(q.iter(app.world()).map(|r| (r.bounds, r.normal)));
    for (b, n) in &boxes {
        let along = |v: Vec3| v.dot(*n);
        let depth = along(b.max - b.min).abs();
        assert!(depth <= INTERACTIVE_MAX_DEPTH + 1e-3, "protrudes {depth} m");
        // The back face sits on a wall cell boundary.
        let back = if along(b.min) < along(b.max) { b.min } else { b.max };
        let k = if n.x.abs() > 0.5 { back.x } else { back.z } / CELL_SIZE;
        assert!((k - k.round()).abs() < 1e-3, "back face on the wall plane ({k})");
    }
    // Every prop is a LevelEntity and goes away with the floor.
    let mut q = app.world_mut().query_filtered::<(), (With<CrtMonitor>, Without<LevelEntity>)>();
    assert_eq!(q.iter(app.world()).count(), 0);
    set_state(&mut app, AppState::MainMenu);
    assert_eq!(count::<CrtMonitor>(&mut app) + count::<UpsRack>(&mut app), 0, "torn down");
    // The Time Trial course gets none.
    begin_time_trial(&mut app);
    step(&mut app, 5);
    assert_eq!(count::<CrtMonitor>(&mut app) + count::<UpsRack>(&mut app), 0, "speedrun course stays clean");
}

#[test]
fn shotgun_shatters_a_crt_with_a_style_tick_and_no_material_allocation() {
    let (mut app, g) = props_app(32);
    let plan = plan_interactive_props(&g);
    let crt_face = plan.iter().find(|p| p.kind == InteractiveKind::Crt).expect("a CRT on the floor").face;
    let mats0 = material_count(&app);
    let target = {
        let mut q = app.world_mut().query::<&CrtMonitor>();
        q.iter(app.world()).find(|c| c.normal == crt_face.inward() && (c.bounds.center() - crt_face.anchor()).length() < 2.5).unwrap().bounds.center()
    };
    teleport(&mut app, g.cell_center(crt_face.cell) + crt_face.inward() * 0.5);
    aim_at(&mut app, target);
    step(&mut app, 3);
    bot(&mut app).fire = true;
    step(&mut app, 2);
    bot(&mut app).fire = false;
    step(&mut app, 2);
    let mut q = app.world_mut().query::<&CrtMonitor>();
    let shattered: Vec<_> = q.iter(app.world()).filter(|c| c.shattered).cloned().collect();
    assert_eq!(shattered.len(), 1, "exactly the aimed CRT shattered");
    let husk = app.world().resource::<PropAssets>().husk_mat.clone();
    let screen_mat = app.world().get::<MeshMaterial3d<StandardMaterial>>(shattered[0].screen).unwrap().0.clone();
    assert_eq!(screen_mat, husk, "screen swapped to the prewarmed cracked husk");
    assert!(meter(&app).feed.iter().flatten().any(|(l, _)| *l == "CRT SHATTERED"));
    assert!(meter(&app).points > 0.0);
    assert_eq!(material_count(&app), mats0, "no per-prop material allocation");
}

#[test]
fn ups_detonation_emp_stuns_damages_demons_and_chains_with_a_short_fuse() {
    let (mut app, g) = props_app(33);
    let plan = plan_interactive_props(&g);
    let face = plan.iter().find(|p| p.kind == InteractiveKind::Ups).expect("a UPS on the floor").face;
    // A second rack one cell along the same wall (4 m away) for the chain.
    let neighbour = segnale_debole::architecture::WallFace { cell: face.cell + face.perp(), side: face.side };
    assert!(g.cell(neighbour.cell + neighbour.side).is_wall());
    let second = app.world_mut().run_system_once(move |mut c: Commands, a: Res<PropAssets>| spawn_ups(&mut c, &a, neighbour)).unwrap();
    step(&mut app, 1);
    let mats0 = material_count(&app);
    let fades0 = app.world().resource::<FxAssets>().fades.lock().unwrap().len();
    let first = {
        let mut q = app.world_mut().query::<(Entity, &UpsRack)>();
        q.iter(app.world()).find(|(e, r)| *e != second && (r.bounds.center() - face.anchor()).length() < 2.5).unwrap().0
    };
    // A tanky Hell Baron right in front of the rack; the player well clear.
    let front = g.cell_center(face.cell) + face.inward() * 0.8;
    let baron = spawn_demon(&mut app, EnemyKind::HellBaron, front);
    let perp = Vec3::new(face.perp().x as f32, 0.0, face.perp().y as f32);
    let far = g.cell_center(face.cell) + face.inward() * 1.8 - perp * 3.8;
    teleport(&mut app, far);
    step(&mut app, 2);
    let hp0 = app.world().get::<Enemy>(baron).unwrap().hp;
    let player_hp0 = {
        with_player(&mut app, |p, _| p.invuln_timer = 0.0);
        player(&mut app).hp + player(&mut app).armor
    };
    // Non-lethal hits only spark.
    app.world_mut().get_mut::<UpsRack>(first).unwrap().damage(5.0, false);
    step(&mut app, 1);
    assert!(app.world().get::<UpsRack>(first).unwrap().is_intact());
    // Lethal weapon damage: instant EMP + blast.
    app.world_mut().get_mut::<UpsRack>(first).unwrap().damage(100.0, false);
    step(&mut app, 1);
    let r1 = app.world().get::<UpsRack>(first).unwrap().clone();
    assert!(r1.detonated);
    let baron_now = app.world().get::<Enemy>(baron).unwrap().clone();
    assert!(baron_now.hp < hp0 - 60.0, "heavy blast damage: {} -> {}", hp0, baron_now.hp);
    assert!(baron_now.is_alive() && baron_now.stun_timer >= UPS_EMP_STUN - 0.05, "EMP stun on survivors: {}", baron_now.stun_timer);
    let r2 = app.world().get::<UpsRack>(second).unwrap().clone();
    assert!(r2.chained && !r2.detonated, "neighbour armed, not yet blown");
    assert!(r2.fuse.is_some_and(|f| f > 0.0 && f <= UPS_CHAIN_FUSE));
    assert!(meter(&app).feed.iter().flatten().any(|(l, _)| *l == "UPS CHAIN BLAST"));
    // ~0.12 s later the chain goes off.
    step(&mut app, (UPS_CHAIN_FUSE * FPS as f32).ceil() as usize + 1);
    assert!(app.world().get::<UpsRack>(second).unwrap().detonated, "chain reaction");
    assert!(meter(&app).feed.iter().flatten().any(|(l, _)| *l == "CHAIN REACTION"));
    let p = player(&mut app);
    assert!(p.hp + p.armor >= player_hp0 - 0.01, "no self-damage from 4+ m away");
    assert_eq!(material_count(&app), mats0, "detonations allocate no materials");
    assert_eq!(app.world().resource::<FxAssets>().fades.lock().unwrap().len(), fades0, "shockwave fades were prewarmed");

    // Point blank: mild, clamped self-damage only.
    let (mut app, g) = props_app(34);
    let face = plan_interactive_props(&g).into_iter().find(|p| p.kind == InteractiveKind::Ups).unwrap().face;
    teleport(&mut app, g.cell_center(face.cell) + face.inward() * -1.2);
    step(&mut app, 2);
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.armor = 0.0;
        p.hp = p.max_hp;
    });
    let mut q = app.world_mut().query::<&mut UpsRack>();
    for mut r in q.iter_mut(app.world_mut()) {
        r.damage(100.0, false);
    }
    step(&mut app, 3);
    let p = player(&mut app);
    assert!(p.hp < p.max_hp, "point blank stings");
    assert!(p.max_hp - p.hp <= UPS_SELF_DAMAGE_CAP + 0.01, "never one-shots: lost {}", p.max_hp - p.hp);
}

// ---------------------------------------------------------------------------
// Pillar 3a: Time Trial PB Signal Ghost
// ---------------------------------------------------------------------------

fn begin_time_trial(app: &mut App) {
    app.world_mut()
        .run_system_once(
            |mut run: ResMut<RunStats>,
             mut mods: ResMut<PlayerModifiers>,
             mut loadout: ResMut<RunLoadout>,
             mut trial: ResMut<TimeTrialState>,
             mut next: ResMut<NextState<AppState>>| {
                start_time_trial(&mut run, &mut mods, &mut loadout, &mut trial, &mut next, Default::default());
            },
        )
        .unwrap();
    step(app, 3);
    assert_eq!(app_state(app), AppState::Playing);
}

fn ghost_pos(app: &mut App) -> Option<Vec3> {
    let mut q = app.world_mut().query_filtered::<&Transform, With<SignalGhost>>();
    q.iter(app.world()).next().map(|t| t.translation)
}

#[test]
fn signal_ghost_records_the_pb_and_replays_it_on_retries() {
    let mut app = build_headless_test_app();
    begin_time_trial(&mut app);
    assert!(app.world().resource::<TrialGhostStore>().pb.is_none());
    assert_eq!(count::<SignalGhost>(&mut app), 0, "no ghost before a PB");
    let g = app.world().resource::<LevelGrid>().clone();
    // Run the course gate by gate (the recorder samples the player at 10 Hz).
    for gate in time_trial_gates().iter() {
        step(&mut app, 30);
        teleport(&mut app, g.cell_center(gate.cells[0]) + Vec3::Y * 0.02);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    let final_time = app.world().resource::<TimeTrialState>().final_time().unwrap();
    let pb = app.world().resource::<TrialGhostStore>().pb.clone().expect("first finish sets the PB ghost");
    assert!(pb.is_valid());
    assert!((pb.time - final_time).abs() < 1e-4);
    let expect = (final_time / 0.1) as usize;
    assert!(pb.samples.len().abs_diff(expect) <= 2, "10 Hz: {} samples for {final_time:.2}s", pb.samples.len());
    assert!(pb.samples.windows(2).all(|w| w[1].t - w[0].t <= 0.1 + 1.0 / 60.0 + 1e-3));
    // Cloud-sync JSON never carries the ghost.
    let profile_json = serde_json::to_string(&app.world().resource::<ProfileStore>().profile).unwrap();
    assert!(!profile_json.contains("samples"));

    // Retry: the ghost spawns at the start and runs on the attempt clock.
    begin_time_trial(&mut app);
    step(&mut app, 2);
    assert_eq!(count::<SignalGhost>(&mut app), 1);
    let start = pb.sample_at(0.0).unwrap().0;
    assert!(ghost_pos(&mut app).unwrap().distance(start) < 1.0, "ghost restarts at t = 0");
    step(&mut app, 45);
    let t = app.world().resource::<TimeTrialState>().elapsed;
    let want = pb.sample_at(t).unwrap().0;
    assert!(ghost_pos(&mut app).unwrap().distance(want) < 0.6, "ghost follows the trace at the attempt clock");
    assert!(ghost_pos(&mut app).unwrap().distance(start) > 0.5, "...and actually moves");
    // Standing at the start while the ghost runs = behind it (positive delta).
    let d = app.world().resource::<TrialGhostStore>().delta;
    assert!(d.is_some_and(|d| d > 0.0), "live delta {d:?}");
    // Instant R: ghost snaps back to t = 0.
    tap_key(&mut app, KeyCode::KeyR);
    assert!(ghost_pos(&mut app).unwrap().distance(start) < 1.0, "R resets the ghost");
    assert_eq!(count::<SignalGhost>(&mut app), 1, "one ghost, never duplicated");
    // A slower finish keeps the old PB ghost.
    step(&mut app, (final_time * FPS as f32) as usize + 30);
    for gate in time_trial_gates().iter() {
        teleport(&mut app, g.cell_center(gate.cells[0]) + Vec3::Y * 0.02);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    assert_eq!(app.world().resource::<TrialGhostStore>().pb.as_ref().map(|p| p.time), Some(pb.time), "slower run keeps the PB ghost");
    // Leaving the trial despawns it; standard runs never show one.
    set_state(&mut app, AppState::MainMenu);
    assert_eq!(count::<SignalGhost>(&mut app), 0);
    begin_run(&mut app, 7);
    step(&mut app, 10);
    assert_eq!(count::<SignalGhost>(&mut app), 0);
}

// ---------------------------------------------------------------------------
// Pillar 3b: controller rumble
// ---------------------------------------------------------------------------

fn rumble(app: &App) -> (usize, usize) {
    let s = app.world().resource::<RumbleState>();
    (s.plays(), s.stops())
}

fn rumble_requests(app: &App) -> Vec<GamepadRumbleRequest> {
    app.world().resource::<Messages<GamepadRumbleRequest>>().iter_current_update_messages().cloned().collect()
}

#[test]
fn rumble_needs_the_setting_an_active_pad_and_stops_on_pause() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 41);
    godmode(&mut app);
    let pad = app.world_mut().spawn(Gamepad::default()).id();
    step(&mut app, 1);
    // Fire with RT: the pad becomes the active input -> shotgun rumble.
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.9);
    app.update();
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Gamepad);
    let reqs = rumble_requests(&app);
    assert!(
        reqs.iter().any(|r| matches!(r, GamepadRumbleRequest::Add { gamepad, intensity, .. } if *gamepad == pad && intensity.strong_motor > 0.0)),
        "desktop backend targets the active pad"
    );
    let (plays, _) = rumble(&app);
    assert!(plays >= 1);
    let last = *app.world().resource::<RumbleState>().log.last().unwrap();
    assert_eq!(last, RumbleCommand::Play(RumblePulse::Shotgun.profile()));
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.0);
    step(&mut app, 2);

    // Pause stops the motors at once.
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.9);
    step(&mut app, 1);
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.0);
    let (_, stops0) = rumble(&app);
    set_state(&mut app, AppState::Paused);
    assert_eq!(rumble(&app).1, stops0 + 1, "Paused sends one Stop");
    assert_eq!(app.world().resource::<RumbleState>().remaining, 0.0);
    set_state(&mut app, AppState::Playing);

    // Mouse & keyboard play: a pad resting on the desk never rumbles.
    step(&mut app, FPS);
    tap_key(&mut app, KeyCode::KeyW);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::KeyboardMouse);
    let (plays_kbm, _) = rumble(&app);
    bot(&mut app).fire = true;
    step(&mut app, 2);
    bot(&mut app).fire = false;
    step(&mut app, FPS);
    assert_eq!(rumble(&app).0, plays_kbm, "no rumble while on mouse & keyboard");

    // RUMBLE: OFF in Settings silences the pad too.
    app.world_mut().resource_mut::<ProfileStore>().profile.settings.rumble = false;
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.9);
    step(&mut app, 2);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Gamepad);
    assert_eq!(rumble(&app).0, plays_kbm, "setting off: no rumble");

    // Damage taken rumbles (setting back on, pad active).
    app.world_mut().resource_mut::<ProfileStore>().profile.settings.rumble = true;
    app.world_mut().get_mut::<Gamepad>(pad).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.0);
    step(&mut app, FPS);
    let before = rumble(&app).0;
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.apply_damage(4.0);
    });
    app.update();
    assert_eq!(rumble(&app).0, before + 1);
    assert_eq!(*app.world().resource::<RumbleState>().log.last().unwrap(), RumbleCommand::Play(RumblePulse::DamageTaken.profile()));

    // Main menu: nothing plays.
    set_state(&mut app, AppState::MainMenu);
    let plays_menu = rumble(&app).0;
    app.world_mut().resource_mut::<segnale_debole::rumble::RumbleQueue>().push(RumblePulse::UpsBlast);
    step(&mut app, 2);
    assert_eq!(rumble(&app).0, plays_menu);
}

#[test]
fn style_rank_ups_play_stingers_and_drive_the_6g_music_layer() {
    use segnale_debole::audio::{MusicMixState, SfxEvent, SfxQueue};
    let mut app = build_headless_test_app();
    begin_run(&mut app, 51);
    godmode(&mut app);
    step(&mut app, 2);
    // Capture the frame's SFX before the backend drains them.
    #[derive(Resource, Default)]
    struct Heard(Vec<SfxEvent>);
    app.init_resource::<Heard>();
    app.add_systems(PostUpdate, |q: Res<SfxQueue>, mut h: ResMut<Heard>| h.0.extend(q.events()));
    app.world_mut().resource_mut::<StyleMeter>().points = 500.0; // A / 5G
    step(&mut app, 2);
    assert!(app.world().resource::<MusicMixState>().overclock < 0.01, "no 6G layer below S");
    let feet = player_pos(&mut app);
    for src in [segnale_debole::style::StyleSource::Shotgun, segnale_debole::style::StyleSource::Rocket, segnale_debole::style::StyleSource::Prop] {
        let e = spawn_demon(&mut app, EnemyKind::HellBaron, feet + Vec3::new(4.0, 0.0, 0.0));
        step(&mut app, 1);
        kill(&mut app, e, src);
        step(&mut app, 3);
    }
    assert!(meter(&app).rank() >= Some(StyleRank::S), "{}", meter(&app).points);
    assert!(app.world().resource::<Heard>().0.contains(&SfxEvent::StylePeak), "crisp peak stinger at S+");
    step(&mut app, FPS);
    assert!(app.world().resource::<MusicMixState>().overclock > 0.3, "6G Overclock layer swells in");
    assert_eq!(app.world().resource::<RunStats>().mode, RunMode::Standard);
}

#[test]
fn review_regressions_generated_chain_single_bolt_handoff_heal_and_stale_ghost() {
    // 1. Generated floor UPS racks actually contain a directly chainable pair.
    let (mut app, g) = props_app(33);
    let plan = plan_interactive_props(&g);
    let ups_faces: Vec<_> = plan.iter().filter(|p| p.kind == InteractiveKind::Ups).map(|p| p.face).collect();
    let pair = ups_faces
        .iter()
        .enumerate()
        .find_map(|(i, a)| ups_faces[i + 1..].iter().find(|b| a.side == b.side && (a.cell - b.cell).abs().element_sum() == 1).map(|b| (*a, *b)))
        .expect("generated floor includes an adjacent chain-reaction UPS pair");
    let (r_a, r_b) = {
        let mut q = app.world_mut().query::<(Entity, &UpsRack)>();
        let mut find = |f: segnale_debole::architecture::WallFace| {
            q.iter(app.world()).find(|(_, r)| (r.bounds.center() - f.anchor()).length() < 2.5).unwrap().0
        };
        (find(pair.0), find(pair.1))
    };
    app.world_mut().get_mut::<UpsRack>(r_a).unwrap().damage(100.0, false);
    step(&mut app, 1);
    assert!(app.world().get::<UpsRack>(r_a).unwrap().detonated);
    assert!(app.world().get::<UpsRack>(r_b).unwrap().chained, "generated partner armed by blast");
    step(&mut app, (UPS_CHAIN_FUSE * FPS as f32).ceil() as usize + 2);
    assert!(app.world().get::<UpsRack>(r_b).unwrap().detonated, "generated partner chain-detonated");

    // 2. One plasma bolt damages a 25-HP UPS rack exactly once and is consumed (never one-shots).
    let (mut app, g) = props_app(35);
    let ups_face = plan_interactive_props(&g).into_iter().find(|p| p.kind == InteractiveKind::Ups).unwrap().face;
    let (rack_e, rack_center, hp0) = {
        let mut q = app.world_mut().query::<(Entity, &UpsRack)>();
        let (e, r) = q.iter(app.world()).find(|(_, r)| (r.bounds.center() - ups_face.anchor()).length() < 2.5).unwrap();
        (e, r.bounds.center(), r.hp)
    };
    teleport(&mut app, g.cell_center(ups_face.cell) + ups_face.inward() * 1.0);
    aim_at(&mut app, rack_center);
    app.world_mut().resource_mut::<segnale_debole::weapons::WeaponState>().current = segnale_debole::weapons::Weapon::Plasma;
    step(&mut app, 2);
    bot(&mut app).fire = true;
    step(&mut app, 1);
    bot(&mut app).fire = false;
    step(&mut app, 15);
    let r = app.world().get::<UpsRack>(rack_e).unwrap();
    assert!(r.is_intact(), "single 16-dmg plasma bolt must not double-hit and detonate a 25-HP rack (hp={})", r.hp);
    assert!(r.hp < hp0 && r.hp > 0.0, "bolt hit once: {hp0} -> {}", r.hp);

    // 3. Same-frame damage + healing still demotes style rank and fires DamageTaken rumble.
    let mut app = build_headless_test_app();
    begin_run(&mut app, 52);
    let pad1 = app.world_mut().spawn(Gamepad::default()).id();
    let pad2 = app.world_mut().spawn(Gamepad::default()).id();
    step(&mut app, 1);
    app.world_mut().get_mut::<Gamepad>(pad1).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.9);
    app.update();
    app.world_mut().get_mut::<Gamepad>(pad1).unwrap().analog_mut().set(GamepadButton::RightTrigger2, 0.0);
    step(&mut app, FPS);
    app.world_mut().resource_mut::<StyleMeter>().points = 500.0;
    let pts_before = meter(&app).points;
    let plays_before = rumble(&app).0;
    with_player(&mut app, |p, _| {
        p.invuln_timer = 0.0;
        p.hp = 80.0;
        p.apply_damage(10.0);
        p.heal(20.0); // net HP+armor increased on the same frame!
    });
    app.update();
    assert!(meter(&app).points < pts_before - 50.0, "same-frame heal must not mask style demotion");
    assert_eq!(rumble(&app).0, plays_before + 1, "same-frame heal must not mask DamageTaken rumble");

    // 4. Controller handoff during active rumble sends Stop to the PREVIOUS pad.
    assert_eq!(app.world().resource::<RumbleState>().vibrating_pad, Some(pad1));
    app.world_mut().get_mut::<Gamepad>(pad2).unwrap().digital_mut().press(GamepadButton::South);
    app.update();
    let reqs = rumble_requests(&app);
    assert!(
        reqs.iter().any(|r| matches!(r, GamepadRumbleRequest::Stop { gamepad } if *gamepad == pad1)),
        "handoff stops previous vibrating pad {pad1:?} ({} requests)",
        reqs.len()
    );

    // 5. Stale ghost trace whose finish time disagrees with ProfileStore PB is hidden.
    let mut app = build_headless_test_app();
    begin_time_trial(&mut app);
    let g = app.world().resource::<LevelGrid>().clone();
    for gate in time_trial_gates().iter() {
        step(&mut app, 30);
        teleport(&mut app, g.cell_center(gate.cells[0]) + Vec3::Y * 0.02);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    // Simulate cloud sync improving the PB without updating the local ghost trace.
    app.world_mut().resource_mut::<ProfileStore>().profile.time_trial_best_secs = Some(1.0);
    begin_time_trial(&mut app);
    step(&mut app, 5);
    assert_eq!(count::<SignalGhost>(&mut app), 0, "stale ghost hidden when PB mismatches");
    assert_eq!(app.world().resource::<TrialGhostStore>().delta, None, "stale ghost delta suppressed");
}
