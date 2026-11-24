use bevy::prelude::*;
use bevy::window::PresentMode;

use segnale_debole::GamePlugin;

/// Native desktop window.
#[cfg(not(target_arch = "wasm32"))]
fn primary_window() -> Window {
    Window {
        title: "SEGNALE DEBOLE".into(),
        resolution: (1280, 720).into(),
        // Uncapped by default; F4 toggles VSync live (see `perf`).
        present_mode: PresentMode::AutoNoVsync,
        ..default()
    }
}

/// Web / itch.io build: render into `index.html`'s `<canvas id="bevy">`,
/// sized to its parent, with browser shortcuts (right-click menu, Tab focus,
/// F-keys) swallowed so Ping Scan and the hotkeys reach the game. The browser
/// owns frame pacing, so presentation is always vsynced.
#[cfg(target_arch = "wasm32")]
fn primary_window() -> Window {
    Window {
        title: "SEGNALE DEBOLE".into(),
        canvas: Some("#bevy".into()),
        fit_canvas_to_parent: true,
        prevent_default_event_handling: true,
        present_mode: PresentMode::AutoVsync,
        ..default()
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(primary_window()), ..default() }))
        .add_plugins(GamePlugin)
        .run();
}
