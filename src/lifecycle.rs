//! OS app lifecycle (Android backgrounding, Home / app switcher, screen off).
//!
//! Bevy's `bevy_winit` reports [`AppLifecycle`] transitions; this module reacts
//! to them on every platform (so the headless test app verifies it on Linux):
//!
//! - `WillSuspend` / `Suspended` while [`AppState::Playing`] pauses the run
//!   (`OnExit(Playing)` releases cursor capture; the run state is untouched),
//!   so a backgrounded run never keeps simulating or dies off-screen.
//! - Audio is suspended ([`AudioSuspended`]: every sink paused, new SFX
//!   dropped, music volumes forced to 0) from `WillSuspend` until the app is
//!   `Running` again. The player resumes the run from the Pause menu.

use bevy::prelude::*;
use bevy::window::AppLifecycle;

use crate::audio::AudioSuspended;
use crate::state::AppState;

/// What one lifecycle message asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleResponse {
    /// Queue `Playing -> Paused`.
    pub pause_run: bool,
    /// New [`AudioSuspended`] value (`None` = unchanged).
    pub audio_suspended: Option<bool>,
}

/// Pure lifecycle policy: suspending pauses an active run and mutes audio;
/// only `Running` restores audio (`WillResume` still has a frame to go and
/// `Idle` precedes the first `Running`).
pub fn lifecycle_response(event: AppLifecycle, state: AppState) -> LifecycleResponse {
    match event {
        AppLifecycle::WillSuspend | AppLifecycle::Suspended => {
            LifecycleResponse { pause_run: state == AppState::Playing, audio_suspended: Some(true) }
        }
        AppLifecycle::Running => LifecycleResponse { pause_run: false, audio_suspended: Some(false) },
        AppLifecycle::Idle | AppLifecycle::WillResume => LifecycleResponse { pause_run: false, audio_suspended: None },
    }
}

/// Apply [`lifecycle_response`] to every [`AppLifecycle`] message this frame.
/// A transition already queued this frame (floor clear, death, ...) wins over
/// the pause, like the browser pointer-lock pause.
pub fn handle_app_lifecycle(
    mut events: MessageReader<AppLifecycle>,
    state: Option<Res<State<AppState>>>,
    mut next: Option<ResMut<NextState<AppState>>>,
    mut audio: ResMut<AudioSuspended>,
) {
    let current = state.map(|s| *s.get()).unwrap_or_default();
    for &event in events.read() {
        let r = lifecycle_response(event, current);
        if r.pause_run
            && let Some(next) = next.as_deref_mut()
            && matches!(*next, NextState::Unchanged)
        {
            info!("app lifecycle {event:?} - pausing the run");
            next.set(AppState::Paused);
        }
        if let Some(s) = r.audio_suspended
            && audio.0 != s
        {
            audio.0 = s;
        }
    }
}

pub struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn build(&self, app: &mut App) {
        // `bevy_winit` registers the message in the windowed game; the
        // headless test app has no window plugins.
        if !app.world().contains_resource::<Messages<AppLifecycle>>() {
            app.add_message::<AppLifecycle>();
        }
        app.init_resource::<AudioSuspended>().add_systems(PreUpdate, handle_app_lifecycle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_pauses_only_an_active_run() {
        for ev in [AppLifecycle::WillSuspend, AppLifecycle::Suspended] {
            assert_eq!(lifecycle_response(ev, AppState::Playing), LifecycleResponse { pause_run: true, audio_suspended: Some(true) });
            for st in [AppState::MainMenu, AppState::Paused, AppState::UpgradeSelect, AppState::Settings, AppState::GameOver] {
                assert_eq!(lifecycle_response(ev, st), LifecycleResponse { pause_run: false, audio_suspended: Some(true) }, "{st:?}");
            }
        }
    }

    #[test]
    fn only_running_restores_audio() {
        assert_eq!(lifecycle_response(AppLifecycle::Running, AppState::Paused).audio_suspended, Some(false));
        assert_eq!(lifecycle_response(AppLifecycle::WillResume, AppState::Paused).audio_suspended, None);
        assert_eq!(lifecycle_response(AppLifecycle::Idle, AppState::MainMenu).audio_suspended, None);
        assert!(!lifecycle_response(AppLifecycle::Running, AppState::Playing).pause_run);
    }
}
