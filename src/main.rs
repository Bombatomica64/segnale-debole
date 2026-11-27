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

/// First Ctrl+C asks Bevy to exit gracefully; the second force-quits with
/// `_exit`. Bevy's own second-press path calls `std::process::exit`, which runs
/// libc `atexit` hooks: when the main thread is wedged inside the GPU driver
/// (Vulkan adapter setup on the NVIDIA dGPU), the driver's hooks wait on locks
/// that thread holds and the process never dies.
#[cfg(not(target_arch = "wasm32"))]
fn install_ctrl_c_handler() {
    use bevy::app::{TerminalCtrlCHandlerPlugin, ctrlc};
    use std::sync::atomic::{AtomicBool, Ordering};

    static PRESSED: AtomicBool = AtomicBool::new(false);
    let result = ctrlc::try_set_handler(|| {
        if PRESSED.swap(true, Ordering::SeqCst) {
            eprintln!("Second Ctrl+C: force quitting.");
            force_quit();
        }
        TerminalCtrlCHandlerPlugin::gracefully_exit();
    });
    if let Err(err) = result {
        eprintln!("Failed to install Ctrl+C handler: {err}");
    }
}

#[cfg(unix)]
fn force_quit() -> ! {
    // SAFETY: `_exit` only terminates the process; it runs no user code.
    unsafe { libc::_exit(130) }
}

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn force_quit() -> ! {
    std::process::exit(130)
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    install_ctrl_c_handler();

    let plugins = DefaultPlugins.set(WindowPlugin { primary_window: Some(primary_window()), ..default() });
    let mut app = App::new();
    #[cfg(target_arch = "wasm32")]
    app.add_plugins(plugins);
    // Our handler replaces the plugin's; keep the system that turns the
    // graceful-exit flag into `AppExit`.
    #[cfg(not(target_arch = "wasm32"))]
    app.add_plugins(plugins.disable::<bevy::app::TerminalCtrlCHandlerPlugin>())
        .add_systems(Update, bevy::app::TerminalCtrlCHandlerPlugin::exit_on_flag);
    app.add_plugins(GamePlugin).run();
}
