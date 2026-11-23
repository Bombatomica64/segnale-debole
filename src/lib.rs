//! SEGNALE DEBOLE — Doom-style retro boomer shooter roguelike (Bevy 0.20).

pub mod apps;
pub mod assets_gen;
pub mod audio;
pub mod bot_input;
pub mod dev;
pub mod enemies;
pub mod extraction;
pub mod fx;
pub mod hud;
pub mod ui_kit;
pub mod level;
pub mod meta;
pub mod meta_ui;
pub mod phone_screen;
pub mod physics;
pub mod player;
pub mod playtest_bot;
pub mod signal;
pub mod state;
pub mod upgrades;
pub mod weapons;

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

/// Every gameplay plugin (simulation only: no UI, no dev hooks), in dependency
/// order. Runs both in the windowed game and in the headless test app.
pub struct CoreGameplayPlugin;

impl Plugin for CoreGameplayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.32, 0.04, 0.03)))
            .insert_resource(GlobalAmbientLight {
                color: Color::srgb(1.0, 0.55, 0.5),
                brightness: 90.0,
                ..default()
            })
            .add_plugins((
                state::StatePlugin,
                assets_gen::AssetsGenPlugin,
                level::LevelPlugin,
                player::PlayerPlugin,
                signal::SignalPlugin,
                phone_screen::PhoneScreenPlugin,
                fx::FxPlugin,
                weapons::WeaponsPlugin,
                apps::AppsPlugin,
                enemies::EnemiesPlugin,
                extraction::ExtractionPlugin,
                upgrades::UpgradesPlugin,
                audio::GameAudioPlugin,
                meta::MetaPlugin,
            ));
    }
}

/// The full game: simulation + HUD/menus + env-driven dev hooks (screenshot
/// tour, autonomous playtest bot). Windowed launches load the persistent
/// on-disk profile unless a bot / screenshot run or `SEGNALE_PROFILE` says
/// otherwise (see [`meta::resolve_profile_path`]).
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(meta::ProfileStore::open(meta::resolve_profile_path(true)))
            .add_plugins((CoreGameplayPlugin, hud::HudPlugin, dev::DevPlugin));
    }
}

/// Fixed simulation step used by the headless test app.
pub const HEADLESS_DT: f32 = 1.0 / 60.0;

/// Engine pieces the gameplay needs, without a window, renderer or GPU:
/// time, states, transforms, input resources and the mesh/material/image asset
/// stores (assets are generated procedurally, never loaded from disk).
pub struct HeadlessGamePlugin;

impl Plugin for HeadlessGamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            AssetPlugin::default(),
            bevy::transform::TransformPlugin,
            bevy::input::InputPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .init_asset::<Image>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(HEADLESS_DT)))
        .add_plugins(CoreGameplayPlugin);
    }
}

/// A headless gameplay app stepping exactly [`HEADLESS_DT`] per `update()`,
/// with a [`bot_input::BotControl`] for virtual input. Starts in `MainMenu`.
pub fn build_headless_test_app() -> App {
    let mut app = App::new();
    app.add_plugins(HeadlessGamePlugin).init_resource::<bot_input::BotControl>();
    // Run startup schedules (procedural assets) and the first MainMenu frame.
    app.update();
    app
}
