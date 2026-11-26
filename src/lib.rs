//! SEGNALE DEBOLE — Doom-style retro boomer shooter roguelike (Bevy 0.20).

pub mod android_input;
pub mod apps;
pub mod architecture;
pub mod assets_gen;
pub mod audio;
pub mod boss;
pub mod compendium;
pub mod decals;
pub mod bot_input;
pub mod dev;
pub mod enemies;
pub mod extraction;
pub mod fatality;
pub mod fx;
pub mod gamepad;
pub mod hud;
pub mod hud_fx;
pub mod leaderboard;
pub mod ui_kit;
pub mod level;
pub mod lifecycle;
pub mod mesh_batch;
pub mod perf;
pub mod meta;
pub mod menu_scene;
pub mod meta_ui;
pub mod phone_screen;
pub mod physics;
pub mod player;
pub mod playtest_bot;
pub mod props;
pub mod raytracing;
pub mod rumble;
pub mod sculpt;
pub mod signal;
pub mod state;
pub mod style;
pub mod style_emblem;
pub mod trials;
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
            // Before PerfPlugin: its finish() reads RaytracingActive.
            .add_plugins(raytracing::RaytracingPlugin)
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
                perf::PerfPlugin,
            ))
            .add_plugins(boss::BossPlugin)
            .add_plugins(decals::DecalsPlugin)
            .add_plugins(trials::TrialsPlugin)
            .add_plugins(compendium::CompendiumPlugin)
            .add_plugins(gamepad::GamepadPlugin)
            // Separate calls: the tuple above is at Bevy's 15-plugin limit.
            .add_plugins(lifecycle::LifecyclePlugin)
            .add_plugins(leaderboard::LeaderboardPlugin)
            // Milestone 31: style rank, shootable props, controller rumble.
            .add_plugins(style::StylePlugin)
            .add_plugins(props::PropsPlugin)
            .add_plugins(rumble::RumblePlugin);
    }
}

/// The full game: simulation + HUD/menus + env-driven dev hooks (screenshot
/// tour, autonomous playtest bot). Windowed launches load the persistent
/// on-disk profile unless a bot / screenshot run or `SEGNALE_PROFILE` says
/// otherwise (see [`meta::resolve_profile_path`]).
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        // Web build: browser `localStorage`; Android: `profile.json` in the
        // app's internal storage; desktop: `profile.json` on disk.
        #[cfg(target_arch = "wasm32")]
        let store = meta::ProfileStore::open_web();
        #[cfg(target_os = "android")]
        let store = meta::ProfileStore::open(meta::resolve_android_profile_path());
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        let store = meta::ProfileStore::open(meta::resolve_profile_path(true));
        app.insert_resource(store).add_plugins((CoreGameplayPlugin, hud::HudPlugin));
        // Env-driven dev hooks (PNG screenshot tours, playtest bot) are
        // desktop tooling; Android and wasm32 have no env or `png` feature.
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        app.add_plugins(dev::DevPlugin);
        // Controllers and the mouse reach Bevy through the JNI bridge.
        #[cfg(target_os = "android")]
        app.add_plugins(android_input::AndroidInputPlugin);
    }
}

/// Android entry point (`android_main`, called by GameActivity through
/// `android-activity`): borderless fullscreen landscape, always vsynced.
/// Input is a controller or mouse & keyboard; there are no touch controls.
#[cfg(target_os = "android")]
#[bevy::prelude::bevy_main]
fn main() {
    use bevy::window::{MonitorSelection, PresentMode, WindowMode};
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "SEGNALE DEBOLE".into(),
                present_mode: PresentMode::AutoVsync,
                resizable: false,
                mode: WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(GamePlugin)
        .run();
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
        // Tests never touch the network (leaderboards / cloud sync stay offline).
        .insert_resource(leaderboard::NetworkDisabled)
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

/// Parse a `<w>x<h>` window size (`SEGNALE_WINDOW_SIZE`), e.g. `960x540`.
/// Each side must be 320..=7680 px.
pub fn parse_window_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().to_ascii_lowercase().split_once('x').map(|(w, h)| (w.trim().parse().ok(), h.trim().parse().ok()))?;
    let ok = |v: u32| (320..=7680).contains(&v);
    match (w, h) {
        (Some(w), Some(h)) if ok(w) && ok(h) => Some((w, h)),
        _ => None,
    }
}

#[cfg(test)]
mod window_size_tests {
    use super::parse_window_size;

    #[test]
    fn window_size_env_parses_and_rejects_junk() {
        assert_eq!(parse_window_size("960x540"), Some((960, 540)));
        assert_eq!(parse_window_size(" 1024X640 "), Some((1024, 640)));
        assert_eq!(parse_window_size("1920 x 1080"), Some((1920, 1080)));
        for bad in ["", "960", "x540", "0x0", "99999x540", "axb", "960x540x2"] {
            assert_eq!(parse_window_size(bad), None, "{bad}");
        }
    }
}
