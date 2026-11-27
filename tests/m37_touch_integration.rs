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

// ---------------------------------------------------------------------------
// M37 follow-up: edge cases (cancel, id reuse, interruptions, assists, menus)
// ---------------------------------------------------------------------------

fn pad(app: &App) -> segnale_debole::gamepad::GamepadInput {
    app.world().resource::<segnale_debole::gamepad::GamepadInput>().clone()
}

fn cancel(app: &mut App, id: u64, at: Vec2) {
    finger(app, id, TouchPhase::Canceled, at);
}

/// Spawn a frozen demon of `kind` at `offset` from the player's feet; returns it.
fn spawn_demon(app: &mut App, kind: EnemyKind, offset: Vec3) -> Entity {
    app.world_mut().insert_resource(segnale_debole::dev::DevFreezeAi);
    let pos = feet(app) + offset;
    let before: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>, mut mats: ResMut<Assets<StandardMaterial>>, run: Res<RunStats>| {
            spawn_enemy(&mut commands, &ea, &mut mats, kind, pos, run.floor, false);
        })
        .unwrap();
    app.update();
    app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).find(|e| !before.contains(e)).expect("spawned")
}

/// Freeze the view on `target` (world point) through the bot look override.
fn aim_at(app: &mut App, target: Vec3) {
    let eye = feet(app) + Vec3::Y * PLAYER_EYE_HEIGHT;
    let d = target - eye;
    let mut bot = app.world_mut().resource_mut::<BotControl>();
    bot.look_yaw = Some((-d.x).atan2(-d.z));
    bot.look_pitch = Some(d.y.atan2(Vec2::new(d.x, d.z).length()));
}

#[test]
fn canceled_touches_never_fire_and_a_reused_finger_id_rebinds_in_order() {
    let mut app = playing_app(70);
    let fire = button(&app, TouchButton::Fire);
    let jump = button(&app, TouchButton::Jump);
    let shells = player(&mut app).ammo_shells;
    // Down + Canceled on FIRE before the next frame: never a tap.
    down(&mut app, 1, fire);
    cancel(&mut app, 1, fire);
    step(&mut app, 3);
    assert_eq!(player(&mut app).ammo_shells, shells, "a canceled contact never shoots");
    assert!(!pad(&app).fire && !app.world().resource::<TouchInput>().frame.fire);
    // A held FIRE canceled by the OS lets go without a new edge.
    down(&mut app, 2, fire);
    step(&mut app, 1);
    assert!(app.world().resource::<TouchInput>().frame.fire);
    cancel(&mut app, 2, fire);
    step(&mut app, 1);
    assert!(!app.world().resource::<TouchInput>().frame.fire && !pad(&app).fire);
    // FIRE lifts and the same id lands on JUMP within one frame: the lift
    // retires FIRE first, then the press binds JUMP.
    down(&mut app, 3, fire);
    step(&mut app, 2);
    up(&mut app, 3, fire);
    down(&mut app, 3, jump);
    step(&mut app, 1);
    let f = app.world().resource::<TouchInput>().frame.clone();
    assert!(f.is_pressed(TouchButton::Jump) && !f.is_pressed(TouchButton::Fire) && !f.fire, "{f:?}");
    step(&mut app, 2);
    assert!(player(&mut app).velocity.y > 0.0 || !player(&mut app).grounded, "the reused id jumped");
    up(&mut app, 3, jump);
    step(&mut app, 1);
    assert_eq!(app.world().resource::<TouchInput>().router.finger_count(), 0);
}

#[test]
fn a_thumb_landing_in_the_corner_starts_the_stick_neutral() {
    let mut app = playing_app(71);
    let start = feet(&mut app);
    // Bottom-left corner of the 1280x720 fallback viewport.
    let corner = Vec2::new(2.0, 718.0);
    down(&mut app, 9, corner);
    step(&mut app, 20);
    let f = app.world().resource::<TouchInput>().frame.clone();
    assert_eq!(f.stick_anchor, Some(corner), "the input anchor is the touch point");
    assert!(f.move_axes == Vec2::ZERO && !f.jump, "no phantom strafe or auto-bhop: {f:?}");
    let ring = f.stick.unwrap().0;
    assert!(ring.x > corner.x && ring.y < corner.y, "the drawn ring is clamped inside the safe area: {ring}");
    assert!((feet(&mut app) - start).length() < 0.05 && player(&mut app).grounded);
    // Pushing up from there walks forward.
    drag(&mut app, 9, corner + Vec2::new(0.0, -60.0));
    step(&mut app, 20);
    assert!((feet(&mut app) - start).length() > 0.5);
    up(&mut app, 9, corner);
}

#[test]
fn focus_loss_suspension_and_android_reset_drop_every_touch() {
    use bevy::window::{AppLifecycle, WindowFocused};
    use segnale_debole::touch::TouchInterrupt;
    let mut app = playing_app(72);
    // Stick held forward and FIRE held.
    let fire = button(&app, TouchButton::Fire);
    down(&mut app, 1, STICK_AT);
    down(&mut app, 2, fire);
    step(&mut app, 1);
    drag(&mut app, 1, STICK_AT + Vec2::new(0.0, -80.0));
    step(&mut app, 2);
    assert!(pad(&app).move_axes.y > 0.5 && pad(&app).fire);
    // The window loses focus: nothing stays held, stale moves are ignored.
    app.world_mut().write_message(WindowFocused { window: Entity::PLACEHOLDER, focused: false });
    step(&mut app, 1);
    let t = app.world().resource::<TouchInput>();
    assert!(t.frame.move_axes == Vec2::ZERO && !t.frame.fire && t.router.finger_count() == 0 && t.stale.suspended);
    assert!(pad(&app).move_axes == Vec2::ZERO && !pad(&app).fire, "the merged pad contribution is gone");
    drag(&mut app, 1, STICK_AT + Vec2::new(30.0, -80.0));
    step(&mut app, 2);
    assert_eq!(pad(&app).move_axes, Vec2::ZERO, "a stale contact never moves the player");
    // Focus back + a fresh touch-down works again; the stale finger's lift is not a tap.
    app.world_mut().write_message(WindowFocused { window: Entity::PLACEHOLDER, focused: true });
    up(&mut app, 2, fire);
    step(&mut app, 1);
    assert!(!app.world().resource::<TouchInput>().frame.fire_just_pressed);
    up(&mut app, 1, STICK_AT);
    down(&mut app, 5, STICK_AT);
    step(&mut app, 1);
    drag(&mut app, 5, STICK_AT + Vec2::new(0.0, -80.0));
    step(&mut app, 1);
    assert!(pad(&app).move_axes.y > 0.5, "a fresh touch drives again");
    // Android ResetAll (via the bridge resource) cuts it off too.
    app.world_mut().resource_mut::<TouchInterrupt>().request();
    step(&mut app, 1);
    assert_eq!(pad(&app).move_axes, Vec2::ZERO);
    assert!(app.world().resource::<TouchInput>().stale.is_stale(5));
    drag(&mut app, 5, STICK_AT + Vec2::new(0.0, -90.0));
    step(&mut app, 1);
    assert_eq!(pad(&app).move_axes, Vec2::ZERO);
    up(&mut app, 5, STICK_AT);
    // App suspension pauses the run and drops the contacts as well.
    down(&mut app, 6, STICK_AT);
    step(&mut app, 1);
    app.world_mut().write_message(AppLifecycle::Suspended);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Paused);
    assert!(app.world().resource::<TouchInput>().stale.is_stale(6));
    up(&mut app, 6, STICK_AT);
    step(&mut app, 1);
    assert!(!app.world().resource::<UiPointer>().released, "the interrupted finger's lift never clicks a pause switch");
}

#[test]
fn a_menu_finger_interrupted_by_focus_loss_never_clicks() {
    use bevy::window::WindowFocused;
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    let at = Vec2::new(400.0, 300.0);
    down(&mut app, 80, at);
    step(&mut app, 1);
    assert!(app.world().resource::<UiPointer>().pressed);
    app.world_mut().write_message(WindowFocused { window: Entity::PLACEHOLDER, focused: false });
    step(&mut app, 1);
    up(&mut app, 80, at);
    step(&mut app, 1);
    assert!(!app.world().resource::<UiPointer>().released, "disarmed by the focus loss");
    // A Canceled menu finger never clicks either.
    down(&mut app, 81, at);
    step(&mut app, 1);
    cancel(&mut app, 81, at);
    step(&mut app, 1);
    let p = *app.world().resource::<UiPointer>();
    assert!(!p.released && !p.pressed);
    // A screen change disarms a finger that is still down.
    down(&mut app, 82, at);
    step(&mut app, 1);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Loadout);
    step(&mut app, 2);
    up(&mut app, 82, at);
    step(&mut app, 1);
    assert!(!app.world().resource::<UiPointer>().released, "armed on the previous screen");
}

#[test]
fn the_auto_bhop_latch_never_carries_into_a_new_run() {
    let mut app = playing_app(73);
    tap(&mut app, 11, TouchButton::AutoBhop);
    assert!(app.world().resource::<TouchInput>().frame.auto_bhop_latched);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::MainMenu);
    step(&mut app, 2);
    begin_run(&mut app, 74);
    step(&mut app, 1);
    let t = app.world().resource::<TouchInput>();
    assert!(!t.frame.auto_bhop_latched, "a new run starts unlatched");
}

#[test]
fn auto_fire_tests_this_frames_aim_and_skips_shielded_demons() {
    let mut app = playing_app(75);
    set_settings(&mut app, |s| s.touch_auto_fire = true);
    let demon = spawn_demon(&mut app, EnemyKind::Imp, Vec3::new(0.0, 0.0, -8.0));
    // Sturdy enough to survive the shots below.
    app.world_mut().get_mut::<Enemy>(demon).unwrap().hp = 1.0e6;
    let chest = feet(&mut app) + Vec3::new(0.0, 1.0, -8.0);
    // Touch mode on, aimed away from the demon: nothing fires.
    down(&mut app, 40, LOOK_AT);
    aim_at(&mut app, chest + Vec3::new(0.0, 0.0, 16.0));
    step(&mut app, 5);
    let shells = player(&mut app).ammo_shells;
    step(&mut app, 5);
    assert_eq!(player(&mut app).ammo_shells, shells);
    // Turn onto it: the very frame the eye lands on it fires (auto-fire runs
    // after the look, on the pose that fires).
    aim_at(&mut app, chest);
    step(&mut app, 1);
    assert!(player(&mut app).ammo_shells < shells, "fired on the post-look pose");
    // A shielded (immune) demon is never a target.
    app.world_mut().get_mut::<Enemy>(demon).unwrap().shielded = true;
    step(&mut app, 40);
    let shells = player(&mut app).ammo_shells;
    step(&mut app, 90);
    assert_eq!(player(&mut app).ammo_shells, shells, "never wastes ammo on a shield");
    assert!(!app.world().resource::<TouchInput>().auto_fire);
    up(&mut app, 40, LOOK_AT);
}

#[test]
fn auto_fire_targets_live_shield_pylons() {
    use segnale_debole::boss::{spawn_pylon, BossPylon};
    let mut app = playing_app(76);
    set_settings(&mut app, |s| s.touch_auto_fire = true);
    // The Carrier Daemon behind the player keeps its pylon alive.
    let boss = spawn_demon(&mut app, EnemyKind::CarrierDaemon, Vec3::new(0.0, 0.0, 12.0));
    let pos = feet(&mut app) + Vec3::new(0.0, 0.0, -9.0);
    app.world_mut()
        .run_system_once(move |mut commands: Commands, ea: Res<EnemyAssets>| {
            spawn_pylon(&mut commands, &ea, pos, boss, 5);
        })
        .unwrap();
    app.update();
    assert_eq!(app.world_mut().query::<&BossPylon>().iter(app.world()).count(), 1);
    let shells = player(&mut app).ammo_shells;
    aim_at(&mut app, pos + Vec3::Y * 1.45);
    down(&mut app, 41, LOOK_AT);
    step(&mut app, 30);
    assert!(player(&mut app).ammo_shells < shells, "auto-fire shoots a live pylon");
    up(&mut app, 41, LOOK_AT);
}

#[test]
fn rocket_auto_fire_never_self_blasts_but_manual_fire_still_rocket_jumps() {
    let mut app = playing_app(77);
    set_settings(&mut app, |s| s.touch_auto_fire = true);
    app.world_mut().resource_mut::<WeaponState>().switch_to(Weapon::Rocket);
    step(&mut app, 60);
    assert_eq!(app.world().resource::<WeaponState>().current, Weapon::Rocket);
    let near = spawn_demon(&mut app, EnemyKind::Imp, Vec3::new(0.0, 0.0, -3.0));
    let at = feet(&mut app) + Vec3::new(0.0, 1.0, -3.0);
    aim_at(&mut app, at);
    down(&mut app, 42, LOOK_AT);
    step(&mut app, 2);
    let rockets = player(&mut app).ammo_rockets;
    step(&mut app, 60);
    assert_eq!(player(&mut app).ammo_rockets, rockets, "point-blank: the Signal-Mortar holds fire");
    assert!(!app.world().resource::<TouchInput>().auto_fire);
    // Far enough past the splash: it fires.
    app.world_mut().entity_mut(near).despawn();
    spawn_demon(&mut app, EnemyKind::Imp, Vec3::new(0.0, 0.0, -14.0));
    let at = feet(&mut app) + Vec3::new(0.0, 1.0, -14.0);
    aim_at(&mut app, at);
    step(&mut app, 30);
    assert!(player(&mut app).ammo_rockets < rockets, "a distant target is fair game");
    // Manual FIRE stays unrestricted (rocket jumps at your feet).
    let rockets = player(&mut app).ammo_rockets;
    set_settings(&mut app, |s| s.touch_auto_fire = false);
    app.world_mut().resource_mut::<BotControl>().look_pitch = Some(-1.4);
    step(&mut app, 60);
    let fire = button(&app, TouchButton::Fire);
    down(&mut app, 43, fire);
    step(&mut app, 3);
    up(&mut app, 43, fire);
    step(&mut app, 2);
    assert!(player(&mut app).ammo_rockets < rockets, "manual FIRE never checks the splash");
    up(&mut app, 42, LOOK_AT);
}

#[test]
fn bestiary_drag_released_over_a_button_never_clicks() {
    use bevy::window::PrimaryWindow;
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.world_mut().spawn((Window::default(), PrimaryWindow));
    app.update();
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Compendium);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Compendium);
    // A finger spins the turntable from the left half and lifts far away
    // (e.g. over BACK or an entry button).
    let from = Vec2::new(200.0, 400.0);
    down(&mut app, 90, from);
    step(&mut app, 1);
    drag(&mut app, 90, from + Vec2::new(60.0, 0.0));
    step(&mut app, 1);
    assert!(app.world().resource::<UiPointer>().drag_claimed, "the turntable owns the drag");
    drag(&mut app, 90, Vec2::new(1100.0, 650.0));
    step(&mut app, 1);
    up(&mut app, 90, Vec2::new(1100.0, 650.0));
    step(&mut app, 1);
    let p = *app.world().resource::<UiPointer>();
    assert!(!p.released, "a claimed drag never clicks on lift");
    assert_eq!(app_state(&app), AppState::Compendium);
    // A plain tap is still a click.
    down(&mut app, 91, Vec2::new(1100.0, 650.0));
    step(&mut app, 1);
    up(&mut app, 91, Vec2::new(1100.0, 650.0));
    step(&mut app, 1);
    assert!(app.world().resource::<UiPointer>().released);
}

#[test]
fn settings_stationary_click_takes_the_highlight_back_from_the_keyboard() {
    use segnale_debole::meta_ui::SettingsKeyFocus;
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Settings);
    step(&mut app, 2);
    for state in [ButtonState::Pressed, ButtonState::Released] {
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::ArrowDown,
            logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        step(&mut app, 1);
    }
    assert!(app.world().resource::<SettingsKeyFocus>().0, "arrows own the row focus");
    // A click without any mouse motion.
    app.world_mut().write_message(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window: Entity::PLACEHOLDER });
    step(&mut app, 1);
    assert!(!app.world().resource::<SettingsKeyFocus>().0, "a stationary press hands the highlight to the pointer");
    app.world_mut().write_message(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window: Entity::PLACEHOLDER });
    step(&mut app, 1);
}

#[test]
fn android_back_returns_from_game_over_and_the_time_trial_finish() {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    let back = |app: &mut App| {
        app.world_mut().resource_mut::<BackRequest>().request();
        step(app, 3);
    };
    begin_run(&mut app, 78);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::GameOver);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::GameOver);
    back(&mut app);
    assert_eq!(app_state(&app), AppState::MainMenu, "Back = controller B on NO SERVICE");
    begin_run(&mut app, 79);
    app.world_mut().resource_mut::<RunStats>().mode = segnale_debole::state::RunMode::TimeTrial;
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Victory);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::Victory);
    back(&mut app);
    assert_eq!(app_state(&app), AppState::MainMenu, "Back leaves the Time Trial finish card");
}

#[test]
fn touch_players_can_edit_and_save_their_callsign() {
    use segnale_debole::hud::MenuAction;
    use segnale_debole::leaderboard::{CallsignEdit, LeaderboardState};
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    set_settings(&mut app, |s| s.touch_mode = TouchMode::On);
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Leaderboard);
    step(&mut app, 3);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Touch);
    let push = |app: &mut App, a: MenuAction| {
        app.world_mut().resource_mut::<LeaderboardState>().pending.push(a);
        step(app, 1);
    };
    // The strip exists, hidden until the editor opens.
    let strip_shown = |app: &mut App| {
        let mut q = app.world_mut().query::<(&MenuAction, &ChildOf)>();
        let parent = q.iter(app.world()).find(|(a, _)| **a == MenuAction::LbEdit(CallsignEdit::Done)).map(|(_, c)| c.parent()).expect("touch editor strip");
        app.world().get::<Node>(parent).unwrap().display == Display::Flex
    };
    assert!(!strip_shown(&mut app));
    push(&mut app, MenuAction::LbCallsign);
    step(&mut app, 1);
    assert!(strip_shown(&mut app), "the strip shows while editing in touch mode");
    assert_eq!(app.world().resource::<LeaderboardState>().editing.as_deref(), Some("MARINE"));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Add));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Next));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Next));
    assert_eq!(app.world().resource::<LeaderboardState>().editing.as_deref(), Some("MARINEC"));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Del));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Add));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Prev));
    push(&mut app, MenuAction::LbEdit(CallsignEdit::Done));
    assert!(app.world().resource::<LeaderboardState>().editing.is_none());
    assert_eq!(app.world().resource::<ProfileStore>().profile.callsign, "MARINE-");
    step(&mut app, 1);
    assert!(!strip_shown(&mut app));
}
