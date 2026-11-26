//! Milestone 31: cross-platform controller rumble / haptics.
//!
//! Gameplay pushes semantic [`RumblePulse`]s into the [`RumbleQueue`] (UPS
//! blasts, style rank-ups) and [`rumble_from_frame`] derives the rest from the
//! frame's [`SfxQueue`] (shotgun / mortar fire, nearby mortar hits,
//! fatalities, jammer kills, Carrier Daemon roars) plus the player's vitals
//! (damage taken). [`rumble_dispatch`] merges the frame into one
//! [`RumbleMix`] `(strong, weak, duration_ms)` and sends it to the platform
//! backend, strictly gated by [`rumble_allowed`]: the `RUMBLE` setting is on,
//! the player is on a controller ([`InputMode::Gamepad`], never a pad resting
//! on the desk during mouse & keyboard play), the run is `Playing`, the window
//! has focus and audio is not suspended. Leaving that state (Pause, menus,
//! end cards, focus loss) stops any running rumble at once.
//!
//! Backends:
//! - desktop: Bevy's `GamepadRumbleRequest::Add` / `Stop` on the active pad
//!   (gilrs with the `gamepad` feature; a no-op without it);
//! - web: `navigator.getGamepads()` -> the most recently used pad's
//!   `vibrationActuator.playEffect("dual-rumble", ...)` via `js_sys::Reflect`,
//!   feature-detected and panic-free;
//! - Android: a lock-free command slot polled by `MainActivity.kt`
//!   (`nativeTakeRumble`), which vibrates the controller's own vibrator.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::audio::{AudioSuspended, SfxEvent, SfxQueue};
use crate::gamepad::{GamepadInput, InputMode};
use crate::meta::{live_settings, ProfileStore};
use crate::player::Player;
use crate::state::AppState;

/// Semantic haptic pulses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RumblePulse {
    Shotgun,
    Rocket,
    MortarHit,
    Fatality,
    UpsBlast,
    DamageTaken,
    JammerDestroy,
    BossRoar,
    StyleRankUp,
}

impl RumblePulse {
    pub const ALL: [RumblePulse; 9] = [
        RumblePulse::Shotgun,
        RumblePulse::Rocket,
        RumblePulse::MortarHit,
        RumblePulse::Fatality,
        RumblePulse::UpsBlast,
        RumblePulse::DamageTaken,
        RumblePulse::JammerDestroy,
        RumblePulse::BossRoar,
        RumblePulse::StyleRankUp,
    ];

    /// `(strong motor, weak motor, duration ms)`.
    pub fn profile(self) -> RumbleMix {
        let (strong, weak, duration_ms) = match self {
            RumblePulse::Shotgun => (0.45, 0.55, 110),
            RumblePulse::Rocket => (0.35, 0.3, 120),
            RumblePulse::MortarHit => (0.7, 0.45, 220),
            RumblePulse::Fatality => (0.55, 0.65, 200),
            RumblePulse::UpsBlast => (0.95, 0.7, 380),
            RumblePulse::DamageTaken => (0.6, 0.2, 160),
            RumblePulse::JammerDestroy => (0.6, 0.5, 260),
            RumblePulse::BossRoar => (0.85, 0.25, 520),
            RumblePulse::StyleRankUp => (0.15, 0.6, 90),
        };
        RumbleMix { strong, weak, duration_ms }
    }
}

/// One frame's merged rumble.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RumbleMix {
    pub strong: f32,
    pub weak: f32,
    pub duration_ms: u32,
}

/// Merge pulses: each motor sums saturating at 1, the duration is the longest.
pub fn merge_pulses(pulses: &[RumblePulse]) -> Option<RumbleMix> {
    pulses.iter().map(|p| p.profile()).reduce(|a, b| RumbleMix {
        strong: (a.strong + b.strong).min(1.0),
        weak: (a.weak + b.weak).min(1.0),
        duration_ms: a.duration_ms.max(b.duration_ms),
    })
}

/// Frame queue of rumble pulses.
#[derive(Resource, Debug, Default)]
pub struct RumbleQueue(pub Vec<RumblePulse>);

impl RumbleQueue {
    pub fn push(&mut self, p: RumblePulse) {
        if self.0.len() < 32 {
            self.0.push(p);
        }
    }
}

/// What the backend was told.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RumbleCommand {
    Play(RumbleMix),
    Stop,
}

/// Commands kept in [`RumbleState::log`] (tests / telemetry).
pub const RUMBLE_LOG: usize = 64;

/// Dispatcher state.
#[derive(Resource, Debug, Default)]
pub struct RumbleState {
    /// Seconds of rumble still running on the pad.
    pub remaining: f32,
    /// The gamepad entity that most recently received `Play` (so a handoff
    /// stops the *previous* controller instead of leaving it buzzing).
    pub vibrating_pad: Option<Entity>,
    /// Last commands sent (oldest first, capped at [`RUMBLE_LOG`]).
    pub log: Vec<RumbleCommand>,
}

impl RumbleState {
    fn record(&mut self, c: RumbleCommand) {
        if self.log.len() >= RUMBLE_LOG {
            self.log.remove(0);
        }
        self.log.push(c);
    }

    pub fn plays(&self) -> usize {
        self.log.iter().filter(|c| matches!(c, RumbleCommand::Play(_))).count()
    }

    pub fn stops(&self) -> usize {
        self.log.iter().filter(|c| matches!(c, RumbleCommand::Stop)).count()
    }
}

/// Strict gate: setting on, controller active, live run, focused, audible.
pub fn rumble_allowed(setting: bool, mode: InputMode, state: AppState, focused: bool, suspended: bool) -> bool {
    setting && mode.is_pad() && state == AppState::Playing && focused && !suspended
}

/// Map this frame's sound cues (and the player's vitals) to pulses.
/// Positional cues only rumble when close to the player.
pub fn rumble_from_frame(
    sfx: Res<SfxQueue>,
    mut queue: ResMut<RumbleQueue>,
    players: Query<(&Player, &Transform)>,
    mut last_hits: Local<Option<u32>>,
) {
    let me = players.single().ok();
    let near = |pos: Option<Vec3>, r: f32| match (pos, me) {
        (None, _) => true,
        (Some(p), Some((_, t))) => p.distance(t.translation) <= r,
        (Some(_), None) => false,
    };
    for req in &sfx.0 {
        let pulse = match req.event {
            SfxEvent::Shotgun => Some(RumblePulse::Shotgun),
            SfxEvent::RocketFire => Some(RumblePulse::Rocket),
            SfxEvent::RocketExplode if near(req.pos, 16.0) => Some(RumblePulse::MortarHit),
            SfxEvent::Fatality if near(req.pos, 25.0) => Some(RumblePulse::Fatality),
            SfxEvent::JammerDestroyed if near(req.pos, 30.0) => Some(RumblePulse::JammerDestroy),
            SfxEvent::BossShield => Some(RumblePulse::BossRoar),
            _ => None,
        };
        if let Some(p) = pulse
            && !queue.0.contains(&p)
        {
            queue.push(p);
        }
    }
    if let Some((p, _)) = me {
        let prev = last_hits.unwrap_or(0);
        if p.damage_hits > prev {
            queue.push(RumblePulse::DamageTaken);
        }
        *last_hits = Some(p.damage_hits);
    } else {
        *last_hits = None;
    }
}

/// Merge the frame's pulses and drive the platform backend (or stop it).
#[allow(clippy::too_many_arguments)]
pub fn rumble_dispatch(
    time: Res<Time>,
    mut queue: ResMut<RumbleQueue>,
    mut state: ResMut<RumbleState>,
    app_state: Option<Res<State<AppState>>>,
    mode: Res<InputMode>,
    store: Option<Res<ProfileStore>>,
    suspended: Option<Res<AudioSuspended>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    pad: Option<Res<GamepadInput>>,
    #[cfg(target_os = "android")] android_bridge: Option<Res<crate::android_input::AndroidInputBridge>>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))] mut requests: Option<
        ResMut<Messages<bevy::input::gamepad::GamepadRumbleRequest>>,
    >,
) {
    let settings = live_settings(store.as_deref());
    let current = app_state.map(|s| *s.get()).unwrap_or_default();
    let focused = windows.iter().next().is_none_or(|w| w.focused);
    let allowed = rumble_allowed(settings.rumble, *mode, current, focused, suspended.is_some_and(|s| s.0));
    let active_pad = pad.and_then(|p| p.active_pad);
    let prev_pad = state.vibrating_pad;
    let handoff = state.remaining > 0.0 && prev_pad.is_some() && prev_pad != active_pad;
    let mix = merge_pulses(&queue.0);
    queue.0.clear();
    let cmd = if !allowed {
        (state.remaining > 0.0).then_some(RumbleCommand::Stop)
    } else if let Some(m) = mix {
        Some(RumbleCommand::Play(m))
    } else if handoff {
        Some(RumbleCommand::Stop)
    } else {
        None
    };
    match cmd {
        Some(RumbleCommand::Stop) => {
            state.remaining = 0.0;
            state.vibrating_pad = None;
        }
        Some(RumbleCommand::Play(m)) => {
            state.remaining = state.remaining.max(m.duration_ms as f32 / 1000.0);
            state.vibrating_pad = active_pad;
        }
        None => {
            state.remaining = (state.remaining - time.delta_secs()).max(0.0);
            if state.remaining == 0.0 {
                state.vibrating_pad = None;
            }
        }
    }
    let Some(cmd) = cmd else { return };
    state.record(cmd);
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    if let Some(msgs) = requests.as_mut() {
        use bevy::input::gamepad::{GamepadRumbleIntensity, GamepadRumbleRequest};
        // Always stop the previously vibrating pad first on Stop or handoff.
        if let Some(prev) = prev_pad
            && (matches!(cmd, RumbleCommand::Stop) || Some(prev) != active_pad)
        {
            msgs.write(GamepadRumbleRequest::Stop { gamepad: prev });
        }
        if let Some(gamepad) = active_pad {
            if Some(gamepad) != prev_pad || matches!(cmd, RumbleCommand::Play(_)) {
                msgs.write(GamepadRumbleRequest::Stop { gamepad });
            }
            if let RumbleCommand::Play(m) = cmd {
                msgs.write(GamepadRumbleRequest::Add {
                    gamepad,
                    intensity: GamepadRumbleIntensity { strong_motor: m.strong, weak_motor: m.weak },
                    duration: std::time::Duration::from_millis(m.duration_ms as u64),
                });
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (active_pad, prev_pad);
        web::send(cmd);
    }
    #[cfg(target_os = "android")]
    {
        let dev = android_bridge.as_deref().map_or(-1, |b| b.device_for_pad(active_pad));
        crate::android_input::set_active_rumble_device(dev);
        crate::android_input::queue_rumble(cmd);
    }
}

/// Browser Gamepad API haptics (`vibrationActuator`), feature-detected.
#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::Cell;

    use js_sys::{Function, Object, Promise, Reflect};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    use super::RumbleCommand;

    thread_local! {
        /// Shared no-op rejection handler (some pads reject `playEffect`).
        static SWALLOW: Closure<dyn FnMut(JsValue)> = Closure::new(|_: JsValue| {});
        /// Gamepad array index that most recently received `Play` so `Stop` or
        /// a pad switch cancels the exact motor we started.
        static LAST_PAD_INDEX: Cell<Option<u32>> = const { Cell::new(None) };
    }

    fn get(o: &JsValue, k: &str) -> Option<JsValue> {
        Reflect::get(o, &JsValue::from_str(k)).ok().filter(|v| !v.is_undefined() && !v.is_null())
    }

    fn pad_list() -> Option<(JsValue, u32)> {
        let nav = get(&js_sys::global().into(), "navigator")?;
        let get_pads = get(&nav, "getGamepads")?.dyn_into::<Function>().ok()?;
        let pads = get_pads.call0(&nav).ok()?;
        let n = get(&pads, "length").and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
        Some((pads, n.min(8)))
    }

    fn actuator_at(pads: &JsValue, idx: u32) -> Option<JsValue> {
        let p = Reflect::get_u32(pads, idx).ok().filter(|v| !v.is_undefined() && !v.is_null())?;
        if get(&p, "connected").and_then(|v| v.as_bool()) == Some(false) {
            return None;
        }
        get(&p, "vibrationActuator")
    }

    fn reset_actuator(act: &JsValue) {
        if let Some(reset) = get(act, "reset").and_then(|f| f.dyn_into::<Function>().ok())
            && let Ok(p) = reset.call0(act)
        {
            swallow(p);
        }
    }

    /// The most recently used connected pad's `(index, vibrationActuator)`.
    fn active_actuator(pads: &JsValue, n: u32) -> Option<(u32, JsValue)> {
        let mut best: Option<(f64, u32, JsValue)> = None;
        for i in 0..n {
            let Some(p) = Reflect::get_u32(pads, i).ok().filter(|v| !v.is_undefined() && !v.is_null()) else { continue };
            if get(&p, "connected").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }
            let Some(act) = get(&p, "vibrationActuator") else { continue };
            let ts = get(&p, "timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if best.as_ref().is_none_or(|(b, _, _)| ts > *b) {
                best = Some((ts, i, act));
            }
        }
        best.map(|(_, i, a)| (i, a))
    }

    fn swallow(v: JsValue) {
        if let Ok(p) = v.dyn_into::<Promise>() {
            SWALLOW.with(|f| {
                let _ = p.catch(f);
            });
        }
    }

    pub fn send(cmd: RumbleCommand) {
        let Some((pads, n)) = pad_list() else { return };
        let prev_idx = LAST_PAD_INDEX.with(Cell::get);
        match cmd {
            RumbleCommand::Play(m) => {
                let Some((idx, act)) = active_actuator(&pads, n) else { return };
                if let Some(prev) = prev_idx
                    && prev != idx
                    && let Some(old_act) = actuator_at(&pads, prev)
                {
                    reset_actuator(&old_act);
                }
                let Some(play) = get(&act, "playEffect").and_then(|f| f.dyn_into::<Function>().ok()) else { return };
                let params = Object::new();
                let set = |k: &str, v: f64| {
                    let _ = Reflect::set(&params, &JsValue::from_str(k), &JsValue::from_f64(v));
                };
                set("startDelay", 0.0);
                set("duration", m.duration_ms as f64);
                set("strongMagnitude", m.strong.clamp(0.0, 1.0) as f64);
                set("weakMagnitude", m.weak.clamp(0.0, 1.0) as f64);
                if let Ok(p) = play.call2(&act, &JsValue::from_str("dual-rumble"), &params) {
                    swallow(p);
                }
                LAST_PAD_INDEX.with(|c| c.set(Some(idx)));
            }
            RumbleCommand::Stop => {
                if let Some(prev) = prev_idx
                    && let Some(old_act) = actuator_at(&pads, prev)
                {
                    reset_actuator(&old_act);
                }
                for i in 0..n {
                    if Some(i) != prev_idx
                        && let Some(act) = actuator_at(&pads, i)
                    {
                        reset_actuator(&act);
                    }
                }
                LAST_PAD_INDEX.with(|c| c.set(None));
            }
        }
    }
}

pub struct RumblePlugin;

impl Plugin for RumblePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RumbleQueue>()
            .init_resource::<RumbleState>()
            .add_systems(PostUpdate, (rumble_from_frame, rumble_dispatch).chain());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulses_merge_saturating_with_longest_duration() {
        assert_eq!(merge_pulses(&[]), None);
        let one = merge_pulses(&[RumblePulse::Shotgun]).unwrap();
        assert_eq!(one, RumblePulse::Shotgun.profile());
        let m = merge_pulses(&[RumblePulse::UpsBlast, RumblePulse::MortarHit, RumblePulse::DamageTaken]).unwrap();
        assert_eq!(m.strong, 1.0, "strong motor saturates");
        assert!(m.weak <= 1.0 && m.weak > RumblePulse::UpsBlast.profile().weak);
        assert_eq!(m.duration_ms, RumblePulse::UpsBlast.profile().duration_ms);
        for p in RumblePulse::ALL {
            let r = p.profile();
            assert!((0.0..=1.0).contains(&r.strong) && (0.0..=1.0).contains(&r.weak), "{p:?}");
            assert!(r.duration_ms > 0 && r.duration_ms <= 600, "{p:?} short pulse");
        }
        assert!(RumblePulse::UpsBlast.profile().strong > RumblePulse::Shotgun.profile().strong);
    }

    #[test]
    fn gate_requires_setting_controller_play_focus_and_audio() {
        use AppState::*;
        assert!(rumble_allowed(true, InputMode::Gamepad, Playing, true, false));
        assert!(!rumble_allowed(false, InputMode::Gamepad, Playing, true, false), "setting off");
        assert!(!rumble_allowed(true, InputMode::KeyboardMouse, Playing, true, false), "pad on the desk during M&K");
        for st in [Paused, MainMenu, GameOver, Victory, UpgradeSelect, Settings] {
            assert!(!rumble_allowed(true, InputMode::Gamepad, st, true, false), "{st:?}");
        }
        assert!(!rumble_allowed(true, InputMode::Gamepad, Playing, false, false), "focus loss");
        assert!(!rumble_allowed(true, InputMode::Gamepad, Playing, true, true), "suspended");
    }
}
