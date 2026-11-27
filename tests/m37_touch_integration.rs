//! Milestone 37: headless touch-control integration tests.
//!
//! Real `bevy::input::touch::TouchInput` messages (exactly what winit sends
//! on Android and in mobile browsers) drive the real router, the gameplay
//! systems and the menus. No window exists, so the touch layer lays the HUD
//! out for its 1280x720 fallback viewport and the cursor is never captured.

use bevy::ecs::system::RunSystemOnce;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::MouseButtonInput;
use bevy::input::touch::{TouchInput as Finger, TouchPhase};
use bevy::input::ButtonState;
use bevy::prelude::*;

use segnale_debole::apps::PhoneApps;
use segnale_debole::bot_input::BotControl;
use segnale_debole::enemies::{spawn_enemy, Enemy, EnemyAssets, EnemyKind};
use segnale_debole::extraction::ExtractionState;
use segnale_debole::gamepad::InputMode;
use segnale_debole::hud::HudPlugin;
use segnale_debole::level::LevelGrid;
use segnale_debole::meta::{PlayerProfile, ProfileStore, TouchMode, UserSettings};
use segnale_debole::player::{CursorCaptured, Player, PLAYER_EYE_HEIGHT};
use segnale_debole::signal::SignalState;
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats, SettingsReturn};
use segnale_debole::touch::{BackRequest, TouchButton, TouchInput, UiPointer};
use segnale_debole::weapons::{Weapon, WeaponState};
use segnale_debole::{build_headless_test_app, HeadlessGamePlugin};

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

fn with_player(app: &mut App, f: impl FnOnce(&mut Player, &mut Transform)) {
    let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
    let (mut p, mut tf) = q.single_mut(app.world_mut()).expect("exactly one player");
    f(&mut p, &mut tf);
}

fn feet(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation
}

/// One finger event, as winit delivers it (logical px).
fn finger(app: &mut App, id: u64, phase: TouchPhase, position: Vec2) {
    app.world_mut().write_message(Finger { phase, position, window: Entity::PLACEHOLDER, force: None, id });
}

fn down(app: &mut App, id: u64, at: Vec2) {
    finger(app, id, TouchPhase::Started, at);
}

fn drag(app: &mut App, id: u64, to: Vec2) {
    finger(app, id, TouchPhase::Moved, to);
}

fn up(app: &mut App, id: u64, at: Vec2) {
    finger(app, id, TouchPhase::Ended, at);
}

fn button(app: &App, b: TouchButton) -> Vec2 {
    app.world().resource::<TouchInput>().layout.button(b).unwrap_or_else(|| panic!("{b:?} not on screen")).center
}

/// Tap a touch button (press, one frame, lift) and let it settle.
fn tap(app: &mut App, id: u64, b: TouchButton) {
    let at = button(app, b);
    down(app, id, at);
    step(app, 1);
    up(app, id, at);
    step(app, 2);
}

fn set_settings(app: &mut App, f: impl FnOnce(&mut UserSettings)) {
    let mut settings = UserSettings::default();
    f(&mut settings);
    app.world_mut().insert_resource(ProfileStore::in_memory(PlayerProfile { settings, ..default() }));
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

/// 16x16 empty arena, no demons, player centered facing -Z, grounded.
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
    with_player(app, |p, tf| {
        tf.translation = c;
        p.velocity = Vec3::ZERO;
        p.yaw = 0.0;
        p.pitch = 0.0;
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
    step(app, 10);
    assert!(player(app).grounded);
    c
}

fn playing_app(seed: u64) -> App {
    let mut app = build_headless_test_app();
    begin_run(&mut app, seed);
    open_arena(&mut app);
    app
}

/// A free spot in the left half (stick zone) and one in the right half (look zone).
const STICK_AT: Vec2 = Vec2::new(330.0, 420.0);
const LOOK_AT: Vec2 = Vec2::new(880.0, 300.0);

#[test]
fn stick_moves_and_right_drag_looks_in_touch_mode_without_cursor_capture() {
    let mut app = playing_app(37);
    let start = feet(&mut app);
    // The first touch switches to touch mode and anchors the stick at once.
    down(&mut app, 1, STICK_AT);
    step(&mut app, 1);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Touch);
    assert!(!app.world().resource::<CursorCaptured>().0, "touch never captures the cursor");
    drag(&mut app, 1, STICK_AT + Vec2::new(0.0, -90.0));
    step(&mut app, 30);
    let moved = feet(&mut app) - start;
    assert!(moved.z < -2.0 && moved.x.abs() < 0.5, "pushed up = forward (-Z at yaw 0): {moved}");
    // Second thumb looks right while the first keeps moving.
    let yaw0 = player(&mut app).yaw;
    down(&mut app, 2, LOOK_AT);
    step(&mut app, 1);
    drag(&mut app, 2, LOOK_AT + Vec2::new(72.0, 0.0));
    step(&mut app, 1);
    let yaw1 = player(&mut app).yaw;
    assert!(yaw1 < yaw0 - 0.2, "dragging right turns right: {yaw0} -> {yaw1}");
    // Holding still never drifts.
    step(&mut app, 5);
    assert!((player(&mut app).yaw - yaw1).abs() < 1e-5);
    // Lift both: the player coasts to a stop on the ground.
    up(&mut app, 1, STICK_AT);
    up(&mut app, 2, LOOK_AT);
    step(&mut app, 60);
    let p = player(&mut app);
    assert!(Vec2::new(p.velocity.x, p.velocity.z).length() < 0.5, "stopped after release: {}", p.velocity);
}

#[test]
fn fire_button_shoots_and_its_drag_aims_while_shooting() {
    let mut app = playing_app(38);
    let shells0 = player(&mut app).ammo_shells;
    let yaw0 = player(&mut app).yaw;
    let fire = button(&app, TouchButton::Fire);
    down(&mut app, 3, fire);
    step(&mut app, 2);
    assert!(player(&mut app).ammo_shells < shells0, "FIRE shot the shotgun");
    drag(&mut app, 3, fire + Vec2::new(-60.0, 0.0));
    step(&mut app, 1);
    assert!(player(&mut app).yaw > yaw0 + 0.1, "dragging the FIRE finger left aims left");
    up(&mut app, 3, fire);
    step(&mut app, 2);
    assert!(!app.world().resource::<segnale_debole::gamepad::GamepadInput>().fire, "released: trigger up");
}

#[test]
fn jump_button_and_auto_bhop_rim_chain_hops() {
    let mut app = playing_app(39);
    tap(&mut app, 4, TouchButton::Jump);
    assert!(!player(&mut app).grounded, "JUMP hopped");
    step(&mut app, 90);
    assert!(player(&mut app).grounded);
    // Stick pushed to the rim: auto-bhop holds jump while moving.
    let r = app.world().resource::<TouchInput>().layout.stick_radius;
    down(&mut app, 1, STICK_AT);
    step(&mut app, 1);
    drag(&mut app, 1, STICK_AT + Vec2::new(0.0, -r));
    // A zero-friction bhop re-jumps on the landing substep, so count take-offs
    // (vertical velocity turning upward), not frames spent grounded.
    let mut airborne = 0;
    let mut hops = 0;
    let mut last_vy = 0.0;
    for _ in 0..150 {
        step(&mut app, 1);
        let p = player(&mut app);
        airborne += (!p.grounded) as u32;
        hops += (p.velocity.y > 0.5 && last_vy <= 0.5) as u32;
        last_vy = p.velocity.y;
    }
    assert!(airborne > 100 && hops >= 2, "chained hops with one thumb: airborne {airborne} hops {hops}");
    // Pull back inside the rim: no more hopping.
    drag(&mut app, 1, STICK_AT + Vec2::new(0.0, -r * 0.5));
    step(&mut app, 90);
    let grounded_frames = (0..30).filter(|_| {
        step(&mut app, 1);
        player(&mut app).grounded
    });
    assert!(grounded_frames.count() >= 28, "walking, not hopping");
    up(&mut app, 1, STICK_AT);
    // Auto-bhop off: the rim never jumps.
    set_settings(&mut app, |s| s.touch_auto_bhop = false);
    step(&mut app, 30);
    down(&mut app, 5, STICK_AT);
    step(&mut app, 1);
    drag(&mut app, 5, STICK_AT + Vec2::new(0.0, -r));
    let hopped = (0..60).any(|_| {
        step(&mut app, 1);
        !player(&mut app).grounded
    });
    assert!(!hopped, "auto-bhop off");
}

#[test]
fn dash_punch_flash_ping_and_overload_fire_from_their_buttons() {
    let mut app = playing_app(40);
    // Spoof 4 bars so every app gate is open.
    app.world_mut().resource_mut::<PhoneApps>().spoof_timer = 1.0e4;
    step(&mut app, 2);
    assert!(app.world().resource::<SignalState>().bars >= 3);
    tap(&mut app, 10, TouchButton::Dash);
    assert!(player(&mut app).dash_cooldown > 0.0, "DASH");
    tap(&mut app, 11, TouchButton::Punch);
    assert!(app.world().resource::<PhoneApps>().punch_cooldown > 0.0, "PUNCH");
    tap(&mut app, 12, TouchButton::Flash);
    assert!(app.world().resource::<PhoneApps>().flash_cooldown > 0.0, "FLASH");
    tap(&mut app, 13, TouchButton::Ping);
    assert!(app.world().resource::<PhoneApps>().ping_cooldown > 0.0, "PING");
    tap(&mut app, 14, TouchButton::Overload);
    assert!(app.world().resource::<PhoneApps>().overload_cooldown > 0.0, "5G OVERLOAD");
}

#[test]
fn swap_button_and_deck_slot_taps_switch_weapons() {
    let mut app = playing_app(41);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Shotgun);
    tap(&mut app, 20, TouchButton::Swap);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Plasma);
    // The HUD publishes the deck's slot tabs; a tap on slot 3 equips the mortar.
    app.world_mut().resource_mut::<TouchInput>().slot_rects =
        [Some(Rect::new(300.0, 660.0, 380.0, 680.0)), Some(Rect::new(390.0, 660.0, 470.0, 680.0)), Some(Rect::new(480.0, 660.0, 560.0, 680.0))];
    step(&mut app, 1);
    tap(&mut app, 21, TouchButton::Slot(2));
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Rocket);
    tap(&mut app, 22, TouchButton::Slot(0));
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Shotgun);
}

#[test]
fn sos_button_appears_at_the_tower_and_starts_the_uplink() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 42);
    with_player(&mut app, |p, _| {
        p.invuln_timer = 1.0e6;
        p.hp = p.max_hp;
    });
    down(&mut app, 30, LOOK_AT);
    step(&mut app, 1);
    up(&mut app, 30, LOOK_AT);
    step(&mut app, 1);
    assert!(app.world().resource::<TouchInput>().layout.button(TouchButton::Sos).is_none(), "no SOS away from the tower");
    let tower = app.world().resource::<LevelGrid>().tower_world();
    with_player(&mut app, |p, tf| {
        tf.translation = Vec3::new(tower.x + 1.0, tower.y, tower.z);
        p.velocity = Vec3::ZERO;
    });
    step(&mut app, 4);
    assert!(app.world().resource::<SignalState>().near_tower);
    tap(&mut app, 31, TouchButton::Sos);
    assert!(matches!(*app.world().resource::<ExtractionState>(), ExtractionState::Uploading { .. }), "SOS started the uplink");
    step(&mut app, 1);
    assert!(app.world().resource::<TouchInput>().layout.button(TouchButton::Sos).is_none(), "SOS hides once uploading");
}

#[test]
fn auto_fire_shoots_a_visible_demon_only_when_enabled() {
    let mut app = playing_app(43);
    // Posed: the demon stands still in the crosshair.
    app.world_mut().insert_resource(segnale_debole::dev::DevFreezeAi);
    let c = feet(&mut app);
    let pos = c + Vec3::new(0.0, 0.0, -8.0);
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>, mut mats: ResMut<Assets<StandardMaterial>>, run: Res<RunStats>| {
            spawn_enemy(&mut commands, &ea, &mut mats, EnemyKind::Imp, pos, run.floor, false);
        })
        .unwrap();
    // Aim at its chest and freeze the camera there.
    let eye = c + Vec3::Y * PLAYER_EYE_HEIGHT;
    let d = pos + Vec3::Y * 1.0 - eye;
    app.world_mut().resource_mut::<BotControl>().look_pitch = Some(d.y.atan2(Vec2::new(d.x, d.z).length()));
    app.world_mut().resource_mut::<BotControl>().look_yaw = Some(0.0);
    step(&mut app, 2);
    // Touch mode, auto-fire off: nothing fires.
    down(&mut app, 40, LOOK_AT);
    step(&mut app, 1);
    let shells0 = player(&mut app).ammo_shells;
    step(&mut app, 30);
    assert_eq!(player(&mut app).ammo_shells, shells0, "auto-fire is opt-in");
    set_settings(&mut app, |s| s.touch_auto_fire = true);
    step(&mut app, 30);
    assert!(player(&mut app).ammo_shells < shells0, "auto-fire shot the demon in the cone");
    up(&mut app, 40, LOOK_AT);
}

#[test]
fn pause_button_pauses_and_android_back_navigates_like_escape() {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    let back = |app: &mut App| {
        app.world_mut().resource_mut::<BackRequest>().request();
        step(app, 3);
    };
    // Back on the main menu never leaves it (and never kills the app).
    back(&mut app);
    assert_eq!(app_state(&app), AppState::MainMenu);
    // Back from a submenu returns to the main menu.
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Loadout);
    step(&mut app, 2);
    back(&mut app);
    assert_eq!(app_state(&app), AppState::MainMenu);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Settings);
    step(&mut app, 2);
    assert_eq!(app.world().resource::<SettingsReturn>().0, AppState::MainMenu);
    back(&mut app);
    assert_eq!(app_state(&app), AppState::MainMenu, "Back closes Settings");
    // In a run: the PAUSE plate pauses, Back resumes, Back pauses again.
    begin_run(&mut app, 44);
    tap(&mut app, 50, TouchButton::Pause);
    assert_eq!(app_state(&app), AppState::Paused, "|| paused the run");
    back(&mut app);
    assert_eq!(app_state(&app), AppState::Playing, "Back resumed");
    back(&mut app);
    assert_eq!(app_state(&app), AppState::Paused, "Back paused");
    // The synthetic Escape never flips the input mode out of touch.
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Touch);
}

#[test]
fn menu_taps_activate_once_and_carried_over_fingers_never_click() {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    let at = Vec2::new(400.0, 300.0);
    down(&mut app, 60, at);
    step(&mut app, 1);
    let p = *app.world().resource::<UiPointer>();
    assert!(p.pressed && p.touch && !p.released && p.position == Some(at), "press highlight while the finger is down (scale 1)");
    up(&mut app, 60, at);
    step(&mut app, 1);
    assert!(app.world().resource::<UiPointer>().released, "lift = click");
    // The browser's compatibility mouse click right after the tap is swallowed.
    app.world_mut().write_message(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window: Entity::PLACEHOLDER });
    step(&mut app, 1);
    app.world_mut().write_message(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window: Entity::PLACEHOLDER });
    step(&mut app, 1);
    assert!(!app.world().resource::<UiPointer>().released, "no double fire from the synthetic mouse");
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Touch, "synthetic mouse never leaves touch mode");
    // A finger held from gameplay into the pause menu never clicks on lift.
    begin_run(&mut app, 45);
    let pause = button(&app, TouchButton::Pause);
    down(&mut app, 61, pause);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Paused);
    up(&mut app, 61, pause);
    step(&mut app, 1);
    assert!(!app.world().resource::<UiPointer>().released, "carried-over finger is not armed");
    assert_eq!(app_state(&app), AppState::Paused);
    // A real key press switches back to mouse & keyboard.
    app.world_mut().write_message(KeyboardInput {
        key_code: KeyCode::KeyW,
        logical_key: Key::Character("w".into()),
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
    step(&mut app, 1);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::KeyboardMouse);
}

#[test]
fn touch_mode_off_ignores_fingers_and_on_forces_touch() {
    let mut app = playing_app(46);
    set_settings(&mut app, |s| s.touch_mode = TouchMode::Off);
    step(&mut app, 1);
    let start = feet(&mut app);
    down(&mut app, 70, STICK_AT);
    step(&mut app, 1);
    drag(&mut app, 70, STICK_AT + Vec2::new(0.0, -90.0));
    step(&mut app, 30);
    assert!((feet(&mut app) - start).length() < 0.05, "Off: touches never move the player");
    assert_ne!(*app.world().resource::<InputMode>(), InputMode::Touch);
    up(&mut app, 70, STICK_AT);
    set_settings(&mut app, |s| s.touch_mode = TouchMode::On);
    step(&mut app, 2);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Touch, "On forces touch mode without a touch");
}

#[test]
fn settings_touch_page_switches_with_keys_and_edits_touch_options() {
    use segnale_debole::meta_ui::{SettingsControl, SettingsPadFocus, SettingsTab};
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Settings);
    step(&mut app, 2);
    let controls = |app: &mut App| app.world_mut().query::<&SettingsControl>().iter(app.world()).count();
    let general = controls(&mut app);
    let press = |app: &mut App, key_code: KeyCode| {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            app.world_mut().write_message(KeyboardInput {
                key_code,
                logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
                state,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
            step(app, 1);
        }
        step(app, 1);
    };
    // E (or RB / a tap on the tab) opens the TOUCH CONTROLS page.
    press(&mut app, KeyCode::KeyE);
    assert_eq!(*app.world().resource::<SettingsTab>(), SettingsTab::Touch);
    // Mode pills x3, three knobs x (-/+), two toggles, RESET, two tabs.
    assert_eq!(controls(&mut app), 3 + 3 * 2 + 2 + 1 + 2, "touch page rebuilt (general had {general})");
    // Row 0 is the TOUCH HUD mode picker: Right cycles AUTO -> ON.
    assert_eq!(app.world().resource::<SettingsPadFocus>().0, 0);
    press(&mut app, KeyCode::ArrowRight);
    assert_eq!(app.world().resource::<ProfileStore>().profile.settings.touch_mode, TouchMode::On);
    // Down to AUTO-FIRE and flip it with Enter.
    for _ in 0..5 {
        press(&mut app, KeyCode::ArrowDown);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.world().resource::<ProfileStore>().profile.settings.touch_auto_fire);
    // Q returns to the GENERAL page with its original controls.
    press(&mut app, KeyCode::KeyQ);
    assert_eq!(*app.world().resource::<SettingsTab>(), SettingsTab::General);
    assert_eq!(controls(&mut app), general);
    // Back (Android) leaves Settings; re-entering starts on GENERAL.
    app.world_mut().resource_mut::<BackRequest>().request();
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::MainMenu);
}
