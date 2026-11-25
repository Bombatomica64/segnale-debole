use bevy::prelude::*;
use bevy::window::PresentMode;

use segnale_debole::GamePlugin;

/// Web / itch.io entrypoint built by Trunk (`index.html`).
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

#[cfg(not(target_arch = "wasm32"))]
fn primary_window() -> Window {
    Window {
        title: "SEGNALE DEBOLE".into(),
        resolution: (1280, 720).into(),
        present_mode: PresentMode::AutoNoVsync,
        ..default()
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(primary_window()), ..default() }))
        .add_plugins(GamePlugin)
        .run();
}
