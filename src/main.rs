use bevy::prelude::*;
use bevy::window::PresentMode;

use segnale_debole::GamePlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "SEGNALE DEBOLE".into(),
                resolution: (1280, 720).into(),
                // Uncapped by default; F4 toggles VSync live (see `perf`).
                present_mode: PresentMode::AutoNoVsync,
                ..default()
            }),
            ..default()
        }))
        .add_plugins(GamePlugin)
        .run();
}
