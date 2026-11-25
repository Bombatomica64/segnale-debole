//! Milestone 26: headless leaderboard / account / end-card submission tests.
//!
//! The headless app inserts `NetworkDisabled`, so every request resolves as
//! offline on the next frame: these tests prove the UI and state machine
//! never need (or touch) the network.

use bevy::ecs::system::RunSystemOnce;
use bevy::input::gamepad::{Gamepad, GamepadButton};
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::ButtonState;
use bevy::prelude::*;

use segnale_debole::bot_input::BotControl;
use segnale_debole::gamepad::PadFocusLock;
use segnale_debole::hud::HudPlugin;
use segnale_debole::leaderboard::{Account, Board, LbTab, LeaderboardClient, LeaderboardState, SubmitBadge, SubmitStatus};
use segnale_debole::level::LevelGrid;
use segnale_debole::meta::{ProfileStore, RunLoadout};
use segnale_debole::player::Player;
use segnale_debole::state::{AppState, PlayerModifiers, RunStats};
use segnale_debole::trials::{start_daily_run, start_time_trial, time_trial_gates, DailyDate, TimeTrialState};
use segnale_debole::HeadlessGamePlugin;

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn app_state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn lb(app: &App) -> &LeaderboardState {
    app.world().resource::<LeaderboardState>()
}

fn menu_app() -> App {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).add_plugins(HudPlugin).init_resource::<BotControl>();
    app.update();
    step(&mut app, 2);
    app
}

fn key(app: &mut App, key: KeyCode, state: ButtonState) {
    app.world_mut().write_message(KeyboardInput {
        key_code: key,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
}

/// Press and release a key through the real input pipeline.
fn tap(app: &mut App, k: KeyCode) {
    key(app, k, ButtonState::Pressed);
    app.update();
    key(app, k, ButtonState::Released);
    app.update();
    step(app, 2);
}

fn pad_tap(app: &mut App, e: Entity, b: GamepadButton) {
    app.world_mut().get_mut::<Gamepad>(e).unwrap().digital_mut().press(b);
    step(app, 1);
    app.world_mut().get_mut::<Gamepad>(e).unwrap().digital_mut().release(b);
    step(app, 2);
}

#[test]
fn headless_app_never_touches_the_network_and_reports_offline() {
    let app = menu_app();
    let client = app.world().resource::<LeaderboardClient>();
    assert!(!client.enabled, "headless / test apps are always offline");
    assert!(client.sent >= 1, "MainMenu asked /auth/me once");
    assert_eq!(lb(&app).account, Account::Offline);
    assert!(lb(&app).me_requested);
}

#[test]
fn keyboard_drives_the_leaderboard_screen_and_callsign_editor() {
    let mut app = menu_app();
    tap(&mut app, KeyCode::KeyK);
    assert_eq!(app_state(&app), AppState::Leaderboard);
    assert_eq!((&lb(&app).daily, &lb(&app).trial), (&Board::Offline, &Board::Offline));
    assert_eq!(lb(&app).daily_date, DailyDate::today_utc().format_iso());

    // Tab / Q / E switch boards.
    tap(&mut app, KeyCode::Tab);
    assert_eq!(lb(&app).tab, LbTab::TimeTrial);
    tap(&mut app, KeyCode::KeyQ);
    assert_eq!(lb(&app).tab, LbTab::Daily);
    tap(&mut app, KeyCode::KeyE);
    assert_eq!(lb(&app).tab, LbTab::TimeTrial);

    // R refreshes (offline again on the next frame, never stuck loading).
    let sent = app.world().resource::<LeaderboardClient>().sent;
    tap(&mut app, KeyCode::KeyR);
    assert_eq!(app.world().resource::<LeaderboardClient>().sent, sent + 2);
    assert_eq!(lb(&app).trial, Board::Offline);

    // C opens the editor: hotkeys are suppressed while typing (Q, E, R, K).
    tap(&mut app, KeyCode::KeyC);
    assert_eq!(lb(&app).editing.as_deref(), Some("MARINE"));
    assert!(app.world().resource::<PadFocusLock>().0, "pad focus locked while typing");
    for _ in 0..6 {
        tap(&mut app, KeyCode::Backspace);
    }
    for k in [KeyCode::KeyQ, KeyCode::KeyE, KeyCode::KeyR, KeyCode::KeyK, KeyCode::Minus, KeyCode::Digit7] {
        tap(&mut app, k);
    }
    assert_eq!(lb(&app).editing.as_deref(), Some("QERK-7"));
    assert_eq!((lb(&app).tab, app_state(&app)), (LbTab::TimeTrial, AppState::Leaderboard));
    tap(&mut app, KeyCode::Enter);
    assert!(lb(&app).editing.is_none());
    assert_eq!(app.world().resource::<ProfileStore>().profile.callsign, "QERK-7");
    assert!(!app.world().resource::<PadFocusLock>().0);

    // Esc cancels an edit first, then leaves the screen.
    tap(&mut app, KeyCode::KeyC);
    tap(&mut app, KeyCode::KeyX);
    tap(&mut app, KeyCode::Escape);
    assert!(lb(&app).editing.is_none());
    assert_eq!(app.world().resource::<ProfileStore>().profile.callsign, "QERK-7");
    assert_eq!(app_state(&app), AppState::Leaderboard);
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
}

#[test]
fn controller_switches_boards_refreshes_and_backs_out() {
    let mut app = menu_app();
    let e = app.world_mut().spawn(Gamepad::default()).id();
    step(&mut app, 1);
    tap(&mut app, KeyCode::KeyK);
    assert_eq!(app_state(&app), AppState::Leaderboard);
    pad_tap(&mut app, e, GamepadButton::RightTrigger);
    assert_eq!(lb(&app).tab, LbTab::TimeTrial);
    pad_tap(&mut app, e, GamepadButton::LeftTrigger);
    assert_eq!(lb(&app).tab, LbTab::Daily);
    let sent = app.world().resource::<LeaderboardClient>().sent;
    pad_tap(&mut app, e, GamepadButton::North);
    assert_eq!(app.world().resource::<LeaderboardClient>().sent, sent + 2, "Y refreshes");
    pad_tap(&mut app, e, GamepadButton::East);
    assert_eq!(app_state(&app), AppState::MainMenu);
}

fn count<C: Component>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<(), With<C>>();
    q.iter(app.world()).count()
}

fn badge_text(app: &mut App) -> String {
    let mut q = app.world_mut().query_filtered::<&Text, With<SubmitBadge>>();
    q.iter(app.world()).map(|t| t.0.clone()).next().unwrap_or_default()
}

#[test]
fn daily_game_over_submits_offline_and_opens_the_board_from_the_end_card() {
    let mut app = menu_app();
    let date = DailyDate::today_utc();
    app.world_mut()
        .run_system_once(
            move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut loadout: ResMut<RunLoadout>, mut next: ResMut<NextState<AppState>>| {
                start_daily_run(&mut run, &mut mods, &mut loadout, &mut next, date, Default::default());
            },
        )
        .unwrap();
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    app.world_mut().resource_mut::<RunStats>().time_elapsed = 95.0;
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::GameOver);
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::GameOver);
    assert_eq!(lb(&app).submit, SubmitStatus::Offline);
    assert_eq!(badge_text(&mut app), "OFFLINE - SAVED LOCALLY");

    // K from the end card: the finished run is torn down.
    tap(&mut app, KeyCode::KeyK);
    assert_eq!(app_state(&app), AppState::Leaderboard);
    step(&mut app, 2);
    assert_eq!(count::<Player>(&mut app), 0);
    assert!(app.world().get_resource::<LevelGrid>().is_none());
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert_eq!(lb(&app).submit, SubmitStatus::Idle);
}

#[test]
fn exhibition_and_too_short_dailies_are_never_ranked() {
    let mut app = menu_app();
    let date = DailyDate::today_utc();
    let start = move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut loadout: ResMut<RunLoadout>, mut next: ResMut<NextState<AppState>>| {
        start_daily_run(&mut run, &mut mods, &mut loadout, &mut next, date, Default::default());
    };
    app.world_mut().run_system_once(start).unwrap();
    step(&mut app, 3);
    {
        let mut run = app.world_mut().resource_mut::<RunStats>();
        run.time_elapsed = 2.0;
    }
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::GameOver);
    step(&mut app, 3);
    assert_eq!(lb(&app).submit, SubmitStatus::NotRanked("RUN TOO SHORT - NOT RANKED".into()));

    tap(&mut app, KeyCode::Enter);
    app.world_mut().run_system_once(start).unwrap();
    step(&mut app, 3);
    {
        let mut run = app.world_mut().resource_mut::<RunStats>();
        run.time_elapsed = 300.0;
        run.debug_skipped = true;
    }
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::GameOver);
    step(&mut app, 3);
    assert_eq!(lb(&app).submit, SubmitStatus::NotRanked("EXHIBITION - NOT RANKED".into()));
    assert_eq!(badge_text(&mut app), "EXHIBITION - NOT RANKED");
}

#[test]
fn time_trial_finish_submits_and_offers_the_leaderboard() {
    let mut app = menu_app();
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
    step(&mut app, 3);
    assert_eq!(app_state(&app), AppState::Playing);
    for (i, gate) in time_trial_gates().iter().enumerate() {
        // Space the gates out so the course time clears the 5 s minimum.
        step(&mut app, 100 + i);
        let feet = app.world().resource::<LevelGrid>().cell_center(gate.cells[0]) + Vec3::Y * 0.02;
        let mut q = app.world_mut().query::<(&mut Player, &mut Transform)>();
        let (mut p, mut tf) = q.single_mut(app.world_mut()).unwrap();
        tf.translation = feet;
        p.velocity = Vec3::ZERO;
        p.prev_pos = Some(feet);
        step(&mut app, 2);
    }
    assert_eq!(app_state(&app), AppState::Victory);
    step(&mut app, 2);
    assert_eq!(lb(&app).submit, SubmitStatus::Offline, "valid finish was sent (offline in tests)");
    tap(&mut app, KeyCode::KeyK);
    assert_eq!(app_state(&app), AppState::Leaderboard);
}

fn texts(app: &mut App) -> Vec<String> {
    let mut q = app.world_mut().query::<&Text>();
    q.iter(app.world()).map(|t| t.0.clone()).collect()
}

/// Milestone 27: the Top 25 pages 10 rows at a time with `[` / `]`, the
/// arrows and LT / RT; switching boards starts on page 1 again.
#[test]
fn top_25_pages_with_keys_and_triggers() {
    use segnale_debole::dev::{showcase_daily_board, showcase_trial_board};
    use segnale_debole::leaderboard::page_label;
    let mut app = menu_app();
    let e = app.world_mut().spawn(Gamepad::default()).id();
    step(&mut app, 1);
    tap(&mut app, KeyCode::KeyK);
    assert_eq!(app_state(&app), AppState::Leaderboard);
    {
        let mut lb = app.world_mut().resource_mut::<LeaderboardState>();
        lb.daily = Board::Loaded(showcase_daily_board());
        lb.trial = Board::Loaded(showcase_trial_board());
    }
    step(&mut app, 2);
    let t = texts(&mut app);
    assert!(t.iter().any(|s| s == "PAGE 1/3") && t.iter().any(|s| s == "ZERO_BARS"), "page 1 shows rank 1");
    assert!(!t.iter().any(|s| s == "BHOP_GOD"), "rank 11 waits for page 2");
    tap(&mut app, KeyCode::BracketRight);
    assert_eq!(lb(&app).page, 1);
    let t = texts(&mut app);
    assert!(t.iter().any(|s| s == "BHOP_GOD") && t.iter().any(|s| s == "11") && !t.iter().any(|s| s == "ZERO_BARS"), "page 2 = ranks 11-20");
    tap(&mut app, KeyCode::ArrowRight);
    tap(&mut app, KeyCode::PageDown);
    assert_eq!(page_label(lb(&app)), "PAGE 3/3", "never past the last page");
    assert!(texts(&mut app).iter().any(|s| s == "LASTBAR"));
    // Controller: LT pages back, RT forward.
    pad_tap(&mut app, e, GamepadButton::LeftTrigger2);
    assert_eq!(lb(&app).page, 1);
    pad_tap(&mut app, e, GamepadButton::RightTrigger2);
    assert_eq!(lb(&app).page, 2);
    tap(&mut app, KeyCode::BracketLeft);
    tap(&mut app, KeyCode::ArrowLeft);
    tap(&mut app, KeyCode::ArrowLeft);
    assert_eq!(lb(&app).page, 0, "never before page 1");
    tap(&mut app, KeyCode::BracketRight);
    tap(&mut app, KeyCode::Tab);
    assert_eq!((lb(&app).tab, lb(&app).page), (LbTab::TimeTrial, 0));
}

/// Milestone 27: with a controller active the callsign editor shows the real
/// pad controls and edits through them; the footer and key hints swap live.
#[test]
fn controller_callsign_editor_shows_pad_controls() {
    use segnale_debole::gamepad::{InputMode, PAD_TEXT_HINT};
    use segnale_debole::hud::MenuAction;
    use segnale_debole::leaderboard::{LB_HINTS, LB_PAD_HINTS};
    let mut app = menu_app();
    let e = app.world_mut().spawn(Gamepad::default()).id();
    step(&mut app, 1);
    tap(&mut app, KeyCode::KeyK);
    assert!(texts(&mut app).iter().any(|s| s == LB_HINTS));
    pad_tap(&mut app, e, GamepadButton::DPadDown);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::Gamepad);
    let t = texts(&mut app);
    assert!(t.iter().any(|s| s == LB_PAD_HINTS) && t.iter().any(|s| s == "BACK  [B]") && t.iter().any(|s| s == "NEXT  [RT]"), "{t:?}");
    // Open the editor (as the focused CALLSIGN switch + A would).
    app.world_mut().resource_mut::<LeaderboardState>().pending.push(MenuAction::LbCallsign);
    step(&mut app, 2);
    assert!(texts(&mut app).iter().any(|s| s == PAD_TEXT_HINT));
    pad_tap(&mut app, e, GamepadButton::DPadRight);
    pad_tap(&mut app, e, GamepadButton::DPadUp);
    assert_eq!(lb(&app).editing.as_deref(), Some("MARINEB"));
    pad_tap(&mut app, e, GamepadButton::DPadLeft);
    pad_tap(&mut app, e, GamepadButton::South);
    assert!(lb(&app).editing.is_none());
    assert_eq!(app.world().resource::<ProfileStore>().profile.callsign, "MARINE");
    // A key press flips every prompt back to mouse & keyboard.
    tap(&mut app, KeyCode::ShiftLeft);
    assert_eq!(*app.world().resource::<InputMode>(), InputMode::KeyboardMouse);
    assert!(texts(&mut app).iter().any(|s| s == "BACK  [ESC]"));
}
