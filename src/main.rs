use bevy::prelude::*;
use bevy::window::PresentMode;

use segnale_debole::GamePlugin;

/// Native desktop window.
#[cfg(not(target_arch = "wasm32"))]
fn primary_window() -> Window {
    Window {
        title: "SEGNALE DEBOLE".into(),
        resolution: native_window_size().into(),
        // Uncapped by default; F4 toggles VSync live (see `perf`).
        present_mode: PresentMode::AutoNoVsync,
        ..default()
    }
}

/// Native window size: `SEGNALE_WINDOW_SIZE=<w>x<h>` (e.g. `960x540` to
/// check the itch.io embed layout), else 1280x720.
#[cfg(not(target_arch = "wasm32"))]
fn native_window_size() -> (u32, u32) {
    std::env::var("SEGNALE_WINDOW_SIZE").ok().as_deref().and_then(segnale_debole::parse_window_size).unwrap_or((1280, 720))
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
