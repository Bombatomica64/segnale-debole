//! Milestone 24: OS app lifecycle (Android backgrounding) on the headless app.
//!
//! `AppLifecycle` messages are what `bevy_winit` emits when Android suspends
//! and resumes the GameActivity; the headless app receives the same messages,
//! so the suspend -> pause and audio mute / unmute policy is verified here on
//! Linux.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy::window::AppLifecycle;

use segnale_debole::audio::AudioSuspended;
use segnale_debole::build_headless_test_app;
use segnale_debole::player::{CursorCaptured, Player};
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats};

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn app_state(app: &App) -> AppState {
    *app.world().resource::<State<AppState>>().get()
}

fn send(app: &mut App, ev: AppLifecycle) {
    app.world_mut().write_message(ev);
}

fn suspended(app: &App) -> bool {
    app.world().resource::<AudioSuspended>().0
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

fn player_pos(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).expect("exactly one player").translation
}

#[test]
fn suspend_pauses_the_run_mutes_audio_and_resume_restores_it() {
    let mut app = build_headless_test_app();
    begin_run(&mut app, 24);
    step(&mut app, 10);
    app.world_mut().insert_resource(CursorCaptured(true));
    let floor = app.world().resource::<RunStats>().floor;
    assert!(!suspended(&app));

    send(&mut app, AppLifecycle::WillSuspend);
    step(&mut app, 1);
    assert_eq!(app_state(&app), AppState::Paused, "backgrounding pauses the run");
    assert!(suspended(&app), "audio suspended");
    assert!(!app.world().resource::<CursorCaptured>().0, "cursor capture released");

    // Suspended: the run is frozen and intact (same floor, same player).
    send(&mut app, AppLifecycle::Suspended);
    step(&mut app, 1);
    let frozen = player_pos(&mut app);
    step(&mut app, 30);
    assert_eq!(app_state(&app), AppState::Paused);
    assert_eq!(player_pos(&mut app), frozen);
    assert_eq!(app.world().resource::<RunStats>().floor, floor);

    // WillResume keeps audio muted for its extra frame; Running restores it.
    send(&mut app, AppLifecycle::WillResume);
    step(&mut app, 1);
    assert!(suspended(&app));
    send(&mut app, AppLifecycle::Running);
    step(&mut app, 1);
    assert!(!suspended(&app), "audio restored");
    // The player resumes from the Pause menu, never automatically.
    assert_eq!(app_state(&app), AppState::Paused);
}

#[test]
fn suspend_outside_a_run_only_mutes_audio() {
    let mut app = build_headless_test_app();
    assert_eq!(app_state(&app), AppState::MainMenu);
    send(&mut app, AppLifecycle::Suspended);
    step(&mut app, 2);
    assert_eq!(app_state(&app), AppState::MainMenu);
    assert!(suspended(&app));
    send(&mut app, AppLifecycle::Running);
    step(&mut app, 1);
    assert!(!suspended(&app));
}
