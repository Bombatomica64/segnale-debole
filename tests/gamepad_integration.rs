//! Milestone 23: headless controller integration tests.
//!
//! A virtual `Gamepad` entity (no gilrs backend, no window) drives the real
//! gameplay and menu systems through `gamepad::GamepadInput`. The headless app
//! never captures the cursor, so every gameplay test here also proves that
//! controller look / fire / movement need no `CursorCaptured`.

use bevy::ecs::system::RunSystemOnce;
use bevy::input::gamepad::{Gamepad, GamepadAxis, GamepadButton};
use bevy::prelude::*;

use segnale_debole::bot_input::BotControl;
use segnale_debole::enemies::Enemy;
use segnale_debole::gamepad::{GamepadInput, LOOK_PITCH_SPEED, LOOK_YAW_SPEED};
use segnale_debole::hud::HudPlugin;
use segnale_debole::level::LevelGrid;
use segnale_debole::meta::{PlayerProfile, ProfileStore, UserSettings};
use segnale_debole::meta_ui::{settings_row_index, SettingKnob, SettingToggle, SettingsPadFocus, SettingsRow};
use segnale_debole::player::{CursorCaptured, Player, MOVE_SPEED};
use segnale_debole::state::{complete_floor, start_run, AppState, PlayerModifiers, RunStats};
use segnale_debole::upgrades::{UpgradeChoices, UpgradeFocus};
use segnale_debole::weapons::{Weapon, WeaponState};
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

fn player(app: &mut App) -> Player {
    let mut q = app.world_mut().query::<&Player>();
    q.single(app.world()).expect("exactly one player").clone()
}

fn player_feet(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

fn horizontal_speed(p: &Player) -> f32 {
    Vec2::new(p.velocity.x, p.velocity.z).length()
}

/// Plug in a virtual controller.
fn plug_pad(app: &mut App) -> Entity {
    app.world_mut().spawn(Gamepad::default()).id()
}

fn pad(app: &mut App, e: Entity) -> Mut<'_, Gamepad> {
    app.world_mut().get_mut::<Gamepad>(e).expect("virtual pad")
}

fn hold(app: &mut App, e: Entity, b: GamepadButton) {
    pad(app, e).digital_mut().press(b);
}

fn release(app: &mut App, e: Entity, b: GamepadButton) {
    pad(app, e).digital_mut().release(b);
}

/// Press for one frame, release, and let any state transition settle.
fn tap(app: &mut App, e: Entity, b: GamepadButton) {
    hold(app, e, b);
    step(app, 1);
    release(app, e, b);
    step(app, 2);
}

fn axis(app: &mut App, e: Entity, a: GamepadAxis, v: f32) {
    pad(app, e).analog_mut().set(a, v);
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

fn godmode(app: &mut App) {
    let mut q = app.world_mut().query::<&mut Player>();
    let mut p = q.single_mut(app.world_mut()).unwrap();
    p.invuln_timer = 1.0e6;
    p.hp = p.max_hp;
}

/// 16x16 empty arena, no demons, player centered facing -Z, standing still.
fn open_arena(app: &mut App) -> Vec3 {
    let wall = "#".repeat(18);
    let row = format!("#{}#", ".".repeat(16));
    let rows: Vec<&str> = std::iter::once(wall.as_str()).chain(std::iter::repeat_n(row.as_str(), 16)).chain(std::iter::once(wall.as_str())).collect();
    app.world_mut().insert_resource(LevelGrid::from_ascii(&rows));
    step(app, 2);
    let enemies: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    for e in enemies {
        app.world_mut().entity_mut(e).despawn();
    }
    let c = Vec3::new(36.0, 0.0, 36.0);
    {
        let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
        let (mut p, mut tf) = q.single_mut(app.world_mut()).unwrap();
        tf.translation = c;
        p.velocity = Vec3::ZERO;
        p.yaw = 0.0;
        p.pitch = 0.0;
    }
    step(app, 10);
    assert!(player(app).grounded);
    c
}

fn playing_app(seed: u64) -> (App, Entity) {
    let mut app = build_headless_test_app();
    begin_run(&mut app, seed);
    godmode(&mut app);
    open_arena(&mut app);
    let e = plug_pad(&mut app);
    step(&mut app, 1);
    assert!(app.world().resource::<GamepadInput>().connected);
    (app, e)
}

#[test]
fn left_stick_walks_the_player_without_cursor_capture() {
    let (mut app, e) = playing_app(11);
    assert!(!app.world().resource::<CursorCaptured>().0, "headless never captures the cursor");
    let start = player_feet(&mut app);
    axis(&mut app, e, GamepadAxis::LeftStickY, 1.0);
    step(&mut app, FPS / 2);
    let p = player(&mut app);
    let moved = player_feet(&mut app) - start;
    // Yaw 0 faces -Z: stick forward walks -Z at full run speed.
    assert!(moved.z < -2.0 && moved.x.abs() < 0.05, "moved {moved}");
    assert!((horizontal_speed(&p) - MOVE_SPEED).abs() < 0.3, "speed {}", horizontal_speed(&p));
    // Inside the deadzone: friction stops the player.
    axis(&mut app, e, GamepadAxis::LeftStickY, 0.1);
    step(&mut app, FPS);
    assert!(horizontal_speed(&player(&mut app)) < 0.5);
}

#[test]
fn right_stick_turns_and_pitches_the_camera_without_cursor_capture() {
    let (mut app, e) = playing_app(12);
    assert!(!app.world().resource::<CursorCaptured>().0);
    axis(&mut app, e, GamepadAxis::RightStickX, 1.0);
    step(&mut app, FPS / 2);
    let p = player(&mut app);
    // Full tilt for 0.5 s turns right (yaw decreases) at the base rate.
    let want = -LOOK_YAW_SPEED * 0.5;
    assert!((p.yaw - want).abs() < 0.08, "yaw {} want {want}", p.yaw);
    // Body + head camera follow in the same frame.
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    let body = q.single(app.world()).unwrap().rotation;
    assert!(body.angle_between(Quat::from_rotation_y(p.yaw)) < 2e-3);

    axis(&mut app, e, GamepadAxis::RightStickX, 0.0);
    axis(&mut app, e, GamepadAxis::RightStickY, 1.0);
    step(&mut app, FPS / 4);
    let p2 = player(&mut app);
    assert!((p2.yaw - p.yaw).abs() < 1e-5, "stick released: no drift");
    assert!((p2.pitch - LOOK_PITCH_SPEED * 0.25).abs() < 0.06, "stick up looks up: {}", p2.pitch);

    // GAMEPAD INVERT Y + sensitivity come from the profile settings.
    let settings = UserSettings { gamepad_invert_y: true, gamepad_sensitivity: 2.0, ..default() };
    app.world_mut().insert_resource(ProfileStore::in_memory(PlayerProfile { settings, ..default() }));
    step(&mut app, FPS / 4);
    let p3 = player(&mut app);
    let dp = p3.pitch - p2.pitch;
    assert!(dp < 0.0 && (dp + LOOK_PITCH_SPEED * 2.0 * 0.25).abs() < 0.06, "inverted 2x pitch delta {dp}");
}

#[test]
fn right_trigger_fires_the_shotgun_without_cursor_capture() {
    let (mut app, e) = playing_app(13);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Shotgun);
    let shells = player(&mut app).ammo_shells;
    // Analog travel past the threshold counts (some backends never report "pressed").
    pad(&mut app, e).analog_mut().set(GamepadButton::RightTrigger2, 0.8);
    step(&mut app, 1);
    assert!(app.world().resource::<GamepadInput>().fire);
    assert_eq!(player(&mut app).ammo_shells, shells - 1, "one shotgun blast");
    assert!(app.world().resource::<WeaponState>().cooldown > 0.0);
    // Light touch below the threshold does nothing.
    pad(&mut app, e).analog_mut().set(GamepadButton::RightTrigger2, 0.2);
    step(&mut app, FPS);
    let after = player(&mut app).ammo_shells;
    step(&mut app, FPS / 2);
    assert_eq!(player(&mut app).ammo_shells, after);
}

#[test]
fn hold_a_chains_bhops_with_stick_air_strafe() {
    let (mut app, e) = playing_app(2026);
    axis(&mut app, e, GamepadAxis::LeftStickY, 1.0);
    step(&mut app, FPS / 2);
    assert!((horizontal_speed(&player(&mut app)) - MOVE_SPEED).abs() < 0.3);
    // Hold A, push the stick fully right (pure strafe) and turn right with
    // the right stick: CPMA air-strafing over chained zero-friction hops.
    hold(&mut app, e, GamepadButton::South);
    axis(&mut app, e, GamepadAxis::LeftStickY, 0.05);
    axis(&mut app, e, GamepadAxis::LeftStickX, 1.0);
    axis(&mut app, e, GamepadAxis::RightStickX, 0.95);
    let (mut hops, mut prev_vy, mut max_chain) = (0, 0.0f32, 0);
    for _ in 0..4 * FPS {
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
    assert!(hops >= 4, "only {hops} hops");
    assert!(speed > 16.0, "stick air-strafe chain only reached {speed} m/s");
    assert!(max_chain >= 2, "chain {max_chain}");
    // Let go of everything: friction stops the player.
    release(&mut app, e, GamepadButton::South);
    for a in [GamepadAxis::LeftStickX, GamepadAxis::LeftStickY, GamepadAxis::RightStickX] {
        axis(&mut app, e, a, 0.0);
    }
    step(&mut app, FPS);
    let p = player(&mut app);
    assert!(p.grounded && horizontal_speed(&p) < 0.5, "{p:?}");
}

#[test]
fn face_and_dpad_buttons_cycle_weapons() {
    let (mut app, e) = playing_app(14);
    let current = |app: &App| app.world().resource::<WeaponState>().current;
    tap(&mut app, e, GamepadButton::North);
    assert_eq!(current(&app), Weapon::Shotgun.next());
    tap(&mut app, e, GamepadButton::DPadRight);
    assert_eq!(current(&app), Weapon::Shotgun.next().next());
    tap(&mut app, e, GamepadButton::DPadLeft);
    assert_eq!(current(&app), Weapon::Shotgun.next());
    // Holding Y switches exactly once.
    hold(&mut app, e, GamepadButton::North);
    step(&mut app, FPS / 2);
    assert_eq!(current(&app), Weapon::Shotgun.next().next());
    release(&mut app, e, GamepadButton::North);
}

#[test]
fn start_toggles_pause_and_lb_dashes() {
    let (mut app, e) = playing_app(15);
    tap(&mut app, e, GamepadButton::LeftTrigger);
    let p = player(&mut app);
    assert!(p.dash_cooldown > 0.0, "LB fired the Airplane Mode dash");
    assert!(horizontal_speed(&p) > MOVE_SPEED * 1.5, "dash burst {}", horizontal_speed(&p));

    // Holding Start pauses once and does not bounce straight back.
    hold(&mut app, e, GamepadButton::Start);
    step(&mut app, 10);
    assert_eq!(app_state(&app), AppState::Paused);
    release(&mut app, e, GamepadButton::Start);
    step(&mut app, 1);
    tap(&mut app, e, GamepadButton::Start);
    assert_eq!(app_state(&app), AppState::Playing);
}

#[test]
fn dpad_and_a_install_the_focused_app_store_card() {
    let (mut app, e) = playing_app(16);
    app.world_mut()
        .run_system_once(|mut run: ResMut<RunStats>, mut next: ResMut<NextState<AppState>>| complete_floor(&mut run, &mut next))
        .unwrap();
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::UpgradeSelect);
    let offers = app.world().resource::<UpgradeChoices>().0.clone();
    assert_eq!(offers.len(), 3);
    assert_eq!(app.world().resource::<UpgradeFocus>().0, 0);
    tap(&mut app, e, GamepadButton::DPadRight);
    tap(&mut app, e, GamepadButton::RightTrigger);
    assert_eq!(app.world().resource::<UpgradeFocus>().0, 2, "Right + RB");
    tap(&mut app, e, GamepadButton::DPadRight);
    assert_eq!(app.world().resource::<UpgradeFocus>().0, 0, "wraps");
    tap(&mut app, e, GamepadButton::LeftTrigger);
    assert_eq!(app.world().resource::<UpgradeFocus>().0, 2, "LB wraps back");
    tap(&mut app, e, GamepadButton::DPadLeft);
    assert_eq!(app.world().resource::<UpgradeFocus>().0, 1);
    tap(&mut app, e, GamepadButton::South);
    let run = app.world().resource::<RunStats>().clone();
    assert_eq!(run.upgrades.last(), Some(&offers[1]), "installed the focused card");
    assert_eq!(app_state(&app), AppState::Playing);
}

/// Headless gameplay + HUD / menus (no window, so no UI layout: the
/// layout-free controller paths are the shortcuts and Settings rows).
fn menu_app() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    let e = plug_pad(&mut app);
    step(&mut app, 1);
    (app, e)
}

#[test]
fn settings_rows_adjust_sliders_toggles_and_go_back_with_a_controller() {
    let (mut app, e) = menu_app();
    assert_eq!(app_state(&app), AppState::MainMenu);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Settings);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Settings);
    let settings = |app: &App| app.world().resource::<ProfileStore>().profile.settings.clone();
    let before = settings(&app);

    // Row 0 is MOUSE SENSITIVITY: Right bumps it by one step.
    tap(&mut app, e, GamepadButton::DPadRight);
    assert!((settings(&app).mouse_sensitivity - (before.mouse_sensitivity + 0.05)).abs() < 1e-4);

    // Walk down to GAMEPAD SENSITIVITY with the Left Stick and the D-Pad.
    let target = settings_row_index(SettingsRow::Knob(SettingKnob::GamepadSensitivity));
    axis(&mut app, e, GamepadAxis::LeftStickY, -1.0);
    step(&mut app, 1);
    axis(&mut app, e, GamepadAxis::LeftStickY, 0.0);
    step(&mut app, 1);
    while app.world().resource::<SettingsPadFocus>().0 != target {
        tap(&mut app, e, GamepadButton::DPadDown);
    }
    tap(&mut app, e, GamepadButton::DPadRight);
    tap(&mut app, e, GamepadButton::DPadRight);
    assert!((settings(&app).gamepad_sensitivity - 1.2).abs() < 1e-4, "{}", settings(&app).gamepad_sensitivity);
    tap(&mut app, e, GamepadButton::DPadLeft);
    assert!((settings(&app).gamepad_sensitivity - 1.1).abs() < 1e-4);

    // Next row: GAMEPAD INVERT Y flips with A.
    tap(&mut app, e, GamepadButton::DPadDown);
    assert_eq!(app.world().resource::<SettingsPadFocus>().0, settings_row_index(SettingsRow::Toggle(SettingToggle::GamepadInvertY)));
    assert!(!settings(&app).gamepad_invert_y);
    tap(&mut app, e, GamepadButton::South);
    assert!(settings(&app).gamepad_invert_y);

    // Up from the first row wraps to BACK; A there leaves Settings.
    while app.world().resource::<SettingsPadFocus>().0 != 0 {
        tap(&mut app, e, GamepadButton::DPadUp);
    }
    tap(&mut app, e, GamepadButton::DPadUp);
    assert_eq!(app.world().resource::<SettingsPadFocus>().0, settings_row_index(SettingsRow::Back));
    tap(&mut app, e, GamepadButton::South);
    assert_eq!(app_state(&app), AppState::MainMenu);

    // B also backs out, and re-entering starts on the first row again.
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Settings);
    step(&mut app, 2);
    assert_eq!(app.world().resource::<SettingsPadFocus>().0, 0);
    tap(&mut app, e, GamepadButton::East);
    assert_eq!(app_state(&app), AppState::MainMenu);
}

#[test]
fn start_and_b_shortcuts_drive_the_menus() {
    let (mut app, e) = menu_app();
    // Loadout: B back to the main menu.
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Loadout);
    step(&mut app, 2);
    tap(&mut app, e, GamepadButton::East);
    assert_eq!(app_state(&app), AppState::MainMenu);
    // Main menu: Start launches PLAY.
    tap(&mut app, e, GamepadButton::Start);
    assert_eq!(app_state(&app), AppState::Playing);
    step(&mut app, 2);
    // In-run Start pauses; B on the pause menu resumes.
    tap(&mut app, e, GamepadButton::Start);
    assert_eq!(app_state(&app), AppState::Paused);
    tap(&mut app, e, GamepadButton::East);
    assert_eq!(app_state(&app), AppState::Playing);
}

#[test]
fn unplugging_the_pad_releases_every_input() {
    let (mut app, e) = playing_app(17);
    hold(&mut app, e, GamepadButton::RightTrigger2);
    axis(&mut app, e, GamepadAxis::LeftStickY, 1.0);
    step(&mut app, 2);
    assert!(app.world().resource::<GamepadInput>().fire);
    app.world_mut().entity_mut(e).despawn();
    step(&mut app, 2);
    assert_eq!(*app.world().resource::<GamepadInput>(), GamepadInput::default());
    step(&mut app, FPS);
    let p = player(&mut app);
    assert!(horizontal_speed(&p) < 0.5, "stops once the pad is gone");
}

