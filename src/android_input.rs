//! Android controller + mouse bridge (Milestone 24).
//!
//! Bevy 0.20.1 on Android has no working controller or mouse path:
//! - gilrs 0.11 has no Android backend (`bevy_gilrs` builds against its dummy
//!   platform and never sees a device);
//! - GameActivity's default motion filter only forwards touchscreen events,
//!   and winit 0.30 turns the rest into `Touch` events (no joystick axes, no
//!   `CursorMoved` / `MouseButtonInput` / `MouseMotion`, no pointer lock).
//!
//! So `MainActivity.kt` intercepts gamepad keys / joystick motion and mouse
//! events in `dispatchKeyEvent` / `dispatchGenericMotionEvent` /
//! `dispatchTouchEvent` / pointer capture, and forwards them through the
//! `Java_dev_bombatomica64_segnaledebole_MainActivity_native*` JNI exports
//! below into a lock-protected [`AndroidInputEvent`] queue. In `PreUpdate`,
//! before Bevy's input systems, [`drain_android_input`] turns the queue into
//! Bevy's own messages: `GamepadConnectionEvent` + `RawGamepad*Event` (so
//! `src/gamepad.rs` sees ordinary `Gamepad` entities) and `MouseButtonInput`
//! / `MouseMotion` / `MouseWheel` / `CursorMoved` + the window cursor
//! position (so mouse look, firing and the menus work unchanged). Keyboards
//! need no bridge: winit maps Android key events to `KeyCode`s.
//!
//! Kotlin polls [`wants_pointer_capture`] to request / release Android
//! pointer capture (relative mouse) while a run holds the cursor (never in
//! touch mode: `CursorCaptured` is never set there).
//!
//! Milestone 37: real touchscreen fingers are left to GameActivity / winit,
//! which deliver them as Bevy `Touches` (see `src/touch.rs`). Kotlin also
//! forwards the system Back gesture ([`AndroidInputEvent::BackPressed`] ->
//! `touch::BackRequest`) and the display-cutout insets
//! ([`AndroidInputEvent::SafeArea`] -> `touch::SafeAreaInsets`); `ResetAll`
//! also interrupts every touch contact (`touch::TouchInterrupt`).
//!
//! Everything except the JNI exports is platform-neutral and unit-tested on
//! the desktop.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

use bevy::input::gamepad::{
    GamepadAxis, GamepadButton, GamepadConnection, GamepadConnectionEvent, RawGamepadAxisChangedEvent, RawGamepadButtonChangedEvent,
    RawGamepadEvent,
};
use bevy::input::mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::input::touch::TouchPhase;
use bevy::input::{ButtonState, InputSystems};
use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow};

use crate::player::CursorCaptured;

/// Android `KeyEvent` key codes the bridge understands.
pub mod keycode {
    pub const DPAD_UP: i32 = 19;
    pub const DPAD_DOWN: i32 = 20;
    pub const DPAD_LEFT: i32 = 21;
    pub const DPAD_RIGHT: i32 = 22;
    pub const DPAD_CENTER: i32 = 23;
    pub const BUTTON_A: i32 = 96;
    pub const BUTTON_B: i32 = 97;
    pub const BUTTON_C: i32 = 98;
    pub const BUTTON_X: i32 = 99;
    pub const BUTTON_Y: i32 = 100;
    pub const BUTTON_Z: i32 = 101;
    pub const BUTTON_L1: i32 = 102;
    pub const BUTTON_R1: i32 = 103;
    pub const BUTTON_L2: i32 = 104;
    pub const BUTTON_R2: i32 = 105;
    pub const BUTTON_THUMBL: i32 = 106;
    pub const BUTTON_THUMBR: i32 = 107;
    pub const BUTTON_START: i32 = 108;
    pub const BUTTON_SELECT: i32 = 109;
    pub const BUTTON_MODE: i32 = 110;
}

/// Android `MotionEvent.getButtonState()` bits.
pub mod mouse_bits {
    pub const PRIMARY: i32 = 1;
    pub const SECONDARY: i32 = 2;
    pub const TERTIARY: i32 = 4;
    pub const BACK: i32 = 8;
    pub const FORWARD: i32 = 16;
}

/// Android gamepad key code -> Bevy button (Android's `A`/`B`/`X`/`Y` follow
/// the Xbox layout: `A` = South).
pub fn map_gamepad_keycode(code: i32) -> Option<GamepadButton> {
    use keycode::*;
    Some(match code {
        BUTTON_A | DPAD_CENTER => GamepadButton::South,
        BUTTON_B => GamepadButton::East,
        BUTTON_X => GamepadButton::West,
        BUTTON_Y => GamepadButton::North,
        BUTTON_C => GamepadButton::C,
        BUTTON_Z => GamepadButton::Z,
        BUTTON_L1 => GamepadButton::LeftTrigger,
        BUTTON_R1 => GamepadButton::RightTrigger,
        BUTTON_L2 => GamepadButton::LeftTrigger2,
        BUTTON_R2 => GamepadButton::RightTrigger2,
        BUTTON_THUMBL => GamepadButton::LeftThumb,
        BUTTON_THUMBR => GamepadButton::RightThumb,
        BUTTON_START => GamepadButton::Start,
        BUTTON_SELECT => GamepadButton::Select,
        BUTTON_MODE => GamepadButton::Mode,
        DPAD_UP => GamepadButton::DPadUp,
        DPAD_DOWN => GamepadButton::DPadDown,
        DPAD_LEFT => GamepadButton::DPadLeft,
        DPAD_RIGHT => GamepadButton::DPadRight,
        _ => return None,
    })
}

/// Android `MotionEvent.getButtonState()` bit -> Bevy mouse button.
pub const MOUSE_BUTTONS: [(i32, MouseButton); 5] = [
    (mouse_bits::PRIMARY, MouseButton::Left),
    (mouse_bits::SECONDARY, MouseButton::Right),
    (mouse_bits::TERTIARY, MouseButton::Middle),
    (mouse_bits::BACK, MouseButton::Back),
    (mouse_bits::FORWARD, MouseButton::Forward),
];

/// Android `MotionEvent` axis ids used for right-stick pair selection.
pub mod motion_axis {
    pub const Z: i32 = 11;
    pub const RX: i32 = 12;
    pub const RY: i32 = 13;
    pub const RZ: i32 = 14;
    pub const LTRIGGER: i32 = 17;
    pub const RTRIGGER: i32 = 18;
    pub const GAS: i32 = 22;
    pub const BRAKE: i32 = 23;
}

/// Mirror of `MainActivity.selectRightStickPair` (Milestone 28): the
/// right-stick look axes given which candidate pairs report centered stick
/// ranges (`min < -0.5 && max > 0.5`). `Z / RZ` first, then `RX / RY`, else
/// `None` (zero look) so uncentered trigger axes on `Z / RZ` never rotate the
/// camera.
pub fn select_right_stick_pair(z_rz_centered: bool, rx_ry_centered: bool) -> Option<(i32, i32)> {
    if z_rz_centered {
        Some((motion_axis::Z, motion_axis::RZ))
    } else if rx_ry_centered {
        Some((motion_axis::RX, motion_axis::RY))
    } else {
        None
    }
}

/// Mirror of `MainActivity.isCenteredStick` on a `(min, max)` motion range.
pub fn is_centered_stick(range: Option<(f32, f32)>) -> bool {
    range.is_some_and(|(min, max)| min < -0.5 && max > 0.5)
}

/// Hat (`AXIS_HAT_X/Y`) travel that counts as a pressed D-Pad direction.
pub const HAT_THRESHOLD: f32 = 0.5;

/// One joystick `MotionEvent` as Android reports it: sticks are `+y` = down,
/// triggers `0..1`, hat `-1..1` (`+y` = down).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AndroidAxes {
    pub left: Vec2,
    pub right: Vec2,
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub hat: Vec2,
}

/// Bevy stick convention: `+y` = up. Non-finite readings center.
fn bevy_stick(v: Vec2) -> Vec2 {
    if v.is_finite() { Vec2::new(v.x, -v.y).clamp(Vec2::splat(-1.0), Vec2::splat(1.0)) } else { Vec2::ZERO }
}

fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

impl AndroidAxes {
    /// Bevy axis values, `+y` = up.
    pub fn bevy_axes(&self) -> [(GamepadAxis, f32); 4] {
        let l = bevy_stick(self.left);
        let r = bevy_stick(self.right);
        [
            (GamepadAxis::LeftStickX, l.x),
            (GamepadAxis::LeftStickY, l.y),
            (GamepadAxis::RightStickX, r.x),
            (GamepadAxis::RightStickY, r.y),
        ]
    }

    /// Analog trigger travel as Bevy's `LeftTrigger2` / `RightTrigger2` values.
    pub fn triggers(&self) -> [(GamepadButton, f32); 2] {
        [(GamepadButton::LeftTrigger2, unit(self.left_trigger)), (GamepadButton::RightTrigger2, unit(self.right_trigger))]
    }

    /// D-Pad buttons held by the hat.
    pub fn hat_buttons(&self) -> [(GamepadButton, bool); 4] {
        let h = if self.hat.is_finite() { self.hat } else { Vec2::ZERO };
        [
            (GamepadButton::DPadUp, h.y <= -HAT_THRESHOLD),
            (GamepadButton::DPadDown, h.y >= HAT_THRESHOLD),
            (GamepadButton::DPadLeft, h.x <= -HAT_THRESHOLD),
            (GamepadButton::DPadRight, h.x >= HAT_THRESHOLD),
        ]
    }
}

/// One input event forwarded from Kotlin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AndroidInputEvent {
    GamepadConnected { device: i32, vendor_id: Option<u16>, product_id: Option<u16> },
    GamepadDisconnected { device: i32 },
    GamepadKey { device: i32, keycode: i32, pressed: bool },
    GamepadAxes { device: i32, axes: AndroidAxes },
    /// Absolute pointer position in physical pixels (hover / drag, uncaptured).
    MouseMove { x: f32, y: f32 },
    /// Relative motion while pointer capture is on.
    MouseDelta { dx: f32, dy: f32 },
    /// Full `getButtonState()` bitmask.
    MouseButtons { state: i32 },
    /// Wheel lines (`+y` = away from the user).
    MouseScroll { x: f32, y: f32 },
    /// Focus loss / pause: release every held gamepad/mouse input and zero axes.
    ResetAll,
    /// The system Back gesture / nav-bar Back (Milestone 37).
    BackPressed,
    /// Display-cutout insets in physical px (Milestone 37).
    SafeArea { left: f32, top: f32, right: f32, bottom: f32 },
}

impl AndroidInputEvent {
    fn is_motion(&self) -> bool {
        matches!(
            self,
            Self::GamepadAxes { .. } | Self::MouseMove { .. } | Self::MouseDelta { .. } | Self::MouseScroll { .. }
        )
    }
}

const MAX_QUEUE_LEN: usize = 4096;

static QUEUE: Mutex<Vec<AndroidInputEvent>> = Mutex::new(Vec::new());
static WANTS_POINTER_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Queue an event (called from the Android UI thread through JNI).
///
/// Motion events coalesce with the tail so bursts never drown out releases or
/// disconnects; `ResetAll` strips stale queued input (preserving connect/
/// disconnect order); critical state edges evict old motion if the cap is hit.
pub fn push_event(ev: AndroidInputEvent) {
    let Ok(mut q) = QUEUE.lock() else { return };
    match ev {
        AndroidInputEvent::ResetAll => {
            q.retain(|e| {
                matches!(
                    e,
                    AndroidInputEvent::GamepadConnected { .. }
                        | AndroidInputEvent::GamepadDisconnected { .. }
                        | AndroidInputEvent::SafeArea { .. }
                        | AndroidInputEvent::BackPressed
                )
            });
            if q.len() >= MAX_QUEUE_LEN {
                let drop_n = q.len() + 1 - MAX_QUEUE_LEN;
                q.drain(..drop_n);
            }
            q.push(AndroidInputEvent::ResetAll);
            return;
        }
        AndroidInputEvent::MouseMove { .. } => {
            if let Some(last @ AndroidInputEvent::MouseMove { .. }) = q.last_mut() {
                *last = ev;
                return;
            }
        }
        AndroidInputEvent::MouseDelta { dx, dy } => {
            if let Some(AndroidInputEvent::MouseDelta { dx: ldx, dy: ldy }) = q.last_mut()
                && dx.is_finite()
                && dy.is_finite()
                && ldx.is_finite()
                && ldy.is_finite()
            {
                *ldx += dx;
                *ldy += dy;
                return;
            }
        }
        AndroidInputEvent::GamepadAxes { device, axes } => {
            if let Some(AndroidInputEvent::GamepadAxes { device: last_dev, axes: last_axes }) = q.last_mut()
                && *last_dev == device
            {
                *last_axes = axes;
                return;
            }
        }
        AndroidInputEvent::SafeArea { .. } => {
            // Only the newest insets matter.
            if let Some(slot) = q.iter_mut().rev().find(|e| matches!(e, AndroidInputEvent::SafeArea { .. })) {
                *slot = ev;
                return;
            }
        }
        _ => {}
    }

    if q.len() >= MAX_QUEUE_LEN {
        if ev.is_motion() {
            // Coalesce into the latest matching motion entry if present; otherwise drop motion.
            match ev {
                AndroidInputEvent::GamepadAxes { device, .. } => {
                    if let Some(slot) = q.iter_mut().rev().find(|e| matches!(e, AndroidInputEvent::GamepadAxes { device: d, .. } if *d == device)) {
                        *slot = ev;
                    }
                }
                AndroidInputEvent::MouseMove { .. } => {
                    if let Some(slot) = q.iter_mut().rev().find(|e| matches!(e, AndroidInputEvent::MouseMove { .. })) {
                        *slot = ev;
                    }
                }
                AndroidInputEvent::MouseDelta { dx, dy } if dx.is_finite() && dy.is_finite() => {
                    if let Some(AndroidInputEvent::MouseDelta { dx: ldx, dy: ldy }) =
                        q.iter_mut().rev().find(|e| matches!(e, AndroidInputEvent::MouseDelta { .. }))
                        && ldx.is_finite()
                        && ldy.is_finite()
                    {
                        *ldx += dx;
                        *ldy += dy;
                    }
                }
                _ => {}
            }
            return;
        }
        // State-critical edge (press/release/connect/disconnect): evict oldest motion first.
        if let Some(pos) = q.iter().position(AndroidInputEvent::is_motion) {
            q.remove(pos);
        } else {
            q.remove(0);
        }
    }
    q.push(ev);
}

/// Take every queued event, oldest first.
pub fn take_events(out: &mut Vec<AndroidInputEvent>) {
    if let Ok(mut q) = QUEUE.lock() {
        out.append(&mut q);
    }
}

/// Whether the game holds the cursor (Kotlin mirrors it as pointer capture).
pub fn wants_pointer_capture() -> bool {
    WANTS_POINTER_CAPTURE.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Milestone 31: controller rumble command slot (polled by Kotlin)
// ---------------------------------------------------------------------------

/// Latest rumble command for `MainActivity.kt` (`0` = nothing pending).
static PENDING_RUMBLE: AtomicU64 = AtomicU64::new(0);

/// Command kinds in bits 32..40 of a packed rumble command.
pub const RUMBLE_KIND_PLAY: u64 = 1;
pub const RUMBLE_KIND_STOP: u64 = 2;

/// Pack a rumble command for the JNI poll: bits 0..8 strong (0-255), 8..16
/// weak (0-255), 16..32 duration in ms, 32..40 kind (1 = play, 2 = stop).
/// Mirrored by `MainActivity.applyRumble`.
pub fn encode_rumble(cmd: crate::rumble::RumbleCommand) -> u64 {
    use crate::rumble::RumbleCommand;
    let byte = |v: f32| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() as u64 } else { 0 };
    match cmd {
        RumbleCommand::Stop => RUMBLE_KIND_STOP << 32,
        RumbleCommand::Play(m) => (RUMBLE_KIND_PLAY << 32) | ((m.duration_ms.min(u16::MAX as u32) as u64) << 16) | (byte(m.weak) << 8) | byte(m.strong),
    }
}

/// Inverse of [`encode_rumble`] (`None` for `0` / unknown kinds).
pub fn decode_rumble(v: u64) -> Option<crate::rumble::RumbleCommand> {
    use crate::rumble::{RumbleCommand, RumbleMix};
    match v >> 32 {
        RUMBLE_KIND_STOP => Some(RumbleCommand::Stop),
        RUMBLE_KIND_PLAY => Some(RumbleCommand::Play(RumbleMix {
            strong: (v & 0xFF) as f32 / 255.0,
            weak: ((v >> 8) & 0xFF) as f32 / 255.0,
            duration_ms: ((v >> 16) & 0xFFFF) as u32,
        })),
        _ => None,
    }
}

/// Active controller Android device id for rumble (`-1` = none).
static ACTIVE_RUMBLE_DEVICE: AtomicI32 = AtomicI32::new(-1);

/// Publish the authoritative Android device id of the owned controller.
pub fn set_active_rumble_device(device_id: i32) {
    ACTIVE_RUMBLE_DEVICE.store(device_id, Ordering::Relaxed);
}

/// Read the authoritative Android device id of the owned controller (`-1` = none).
pub fn active_rumble_device() -> i32 {
    ACTIVE_RUMBLE_DEVICE.load(Ordering::Relaxed)
}

/// Publish a rumble command (the newest one wins until Kotlin takes it).
pub fn queue_rumble(cmd: crate::rumble::RumbleCommand) {
    PENDING_RUMBLE.store(encode_rumble(cmd), Ordering::Relaxed);
}

/// Discard any queued rumble command (used on pause / focus loss).
pub fn clear_rumble() {
    PENDING_RUMBLE.store(0, Ordering::Relaxed);
}

/// Take the pending rumble command (`0` = none).
pub fn take_rumble() -> u64 {
    PENDING_RUMBLE.swap(0, Ordering::Relaxed)
}

/// Digital + analog trigger values tracked separately so stick-motion axis
/// samples (which report `0.0` for absent analog triggers) cannot clear a
/// held `KEYCODE_BUTTON_L2`/`R2` press.
#[derive(Debug, Clone, Copy, Default)]
struct TriggerState {
    digital_l: bool,
    digital_r: bool,
    analog_l: f32,
    analog_r: f32,
}

impl TriggerState {
    fn left(self) -> f32 {
        self.analog_l.max(if self.digital_l { 1.0 } else { 0.0 })
    }

    fn right(self) -> f32 {
        self.analog_r.max(if self.digital_r { 1.0 } else { 0.0 })
    }
}

/// Device id -> Bevy gamepad entity, plus per-device edge state.
#[derive(Resource, Debug, Default)]
pub struct AndroidInputBridge {
    pub pads: HashMap<i32, Entity>,
    hats: HashMap<i32, [bool; 4]>,
    triggers: HashMap<i32, TriggerState>,
    held_buttons: HashMap<i32, HashSet<GamepadButton>>,
    mouse_state: i32,
    scratch: Vec<AndroidInputEvent>,
}

impl AndroidInputBridge {
    /// Reverse-lookup the Android `InputDevice` id for a Bevy gamepad entity (`-1` if none).
    pub fn device_for_pad(&self, pad: Option<Entity>) -> i32 {
        let Some(target) = pad else { return -1 };
        self.pads.iter().find_map(|(&dev, &e)| (e == target).then_some(dev)).unwrap_or(-1)
    }
}

/// Every Bevy message the bridge writes.
#[derive(bevy::ecs::system::SystemParam)]
pub struct BridgeWriters<'w> {
    raw: MessageWriter<'w, RawGamepadEvent>,
    connections: MessageWriter<'w, GamepadConnectionEvent>,
    buttons: MessageWriter<'w, RawGamepadButtonChangedEvent>,
    axes: MessageWriter<'w, RawGamepadAxisChangedEvent>,
    mouse_buttons: MessageWriter<'w, MouseButtonInput>,
    motion: MessageWriter<'w, MouseMotion>,
    wheel: MessageWriter<'w, MouseWheel>,
    cursor: MessageWriter<'w, CursorMoved>,
}

impl BridgeWriters<'_> {
    fn button(&mut self, pad: Entity, button: GamepadButton, value: f32) {
        let ev = RawGamepadButtonChangedEvent::new(pad, button, value);
        self.raw.write(ev.into());
        self.buttons.write(ev);
    }

    fn axis(&mut self, pad: Entity, axis: GamepadAxis, value: f32) {
        let ev = RawGamepadAxisChangedEvent::new(pad, axis, value);
        self.raw.write(ev.into());
        self.axes.write(ev);
    }

    fn connection(&mut self, ev: GamepadConnectionEvent) {
        self.raw.write(ev.clone().into());
        self.connections.write(ev);
    }
}

/// Turn queued Android events into Bevy input messages (see module docs).
#[allow(clippy::type_complexity)]
pub fn drain_android_input(
    mut commands: Commands,
    mut bridge: ResMut<AndroidInputBridge>,
    mut out: BridgeWriters,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    (mut back, mut insets, mut touch_reset): (
        Option<ResMut<crate::touch::BackRequest>>,
        Option<ResMut<crate::touch::SafeAreaInsets>>,
        Option<ResMut<crate::touch::TouchInterrupt>>,
    ),
) {
    let mut events = std::mem::take(&mut bridge.scratch);
    take_events(&mut events);
    let mut window = windows.single_mut().ok();
    for ev in events.drain(..) {
        match ev {
            AndroidInputEvent::GamepadConnected { device, vendor_id, product_id } => {
                if let std::collections::hash_map::Entry::Vacant(slot) = bridge.pads.entry(device) {
                    let pad = *slot.insert(commands.spawn_empty().id());
                    let name = format!("Android gamepad {device}");
                    out.connection(GamepadConnectionEvent::new(pad, GamepadConnection::Connected { name, vendor_id, product_id }));
                }
            }
            AndroidInputEvent::GamepadDisconnected { device } => {
                bridge.hats.remove(&device);
                bridge.triggers.remove(&device);
                bridge.held_buttons.remove(&device);
                if let Some(pad) = bridge.pads.remove(&device) {
                    out.connection(GamepadConnectionEvent::new(pad, GamepadConnection::Disconnected));
                }
            }
            AndroidInputEvent::GamepadKey { device, keycode, pressed } => {
                let Some(button) = map_gamepad_keycode(keycode) else { continue };
                let pad = pad_for(&mut commands, &mut bridge, &mut out, device);
                match button {
                    GamepadButton::LeftTrigger2 => {
                        let t = bridge.triggers.entry(device).or_default();
                        t.digital_l = pressed;
                        out.button(pad, button, t.left());
                    }
                    GamepadButton::RightTrigger2 => {
                        let t = bridge.triggers.entry(device).or_default();
                        t.digital_r = pressed;
                        out.button(pad, button, t.right());
                    }
                    _ => {
                        let held = bridge.held_buttons.entry(device).or_default();
                        if pressed {
                            held.insert(button);
                        } else {
                            held.remove(&button);
                        }
                        out.button(pad, button, if pressed { 1.0 } else { 0.0 });
                    }
                }
            }
            AndroidInputEvent::GamepadAxes { device, axes } => {
                let pad = pad_for(&mut commands, &mut bridge, &mut out, device);
                for (axis, v) in axes.bevy_axes() {
                    out.axis(pad, axis, v);
                }
                let t = bridge.triggers.entry(device).or_default();
                let [(lt_btn, lt_val), (rt_btn, rt_val)] = axes.triggers();
                t.analog_l = lt_val;
                t.analog_r = rt_val;
                out.button(pad, lt_btn, t.left());
                out.button(pad, rt_btn, t.right());
                let held = axes.hat_buttons();
                let last = bridge.hats.insert(device, held.map(|(_, h)| h)).unwrap_or_default();
                for (i, (button, h)) in held.into_iter().enumerate() {
                    if h != last[i] {
                        out.button(pad, button, if h { 1.0 } else { 0.0 });
                    }
                }
            }
            AndroidInputEvent::MouseMove { x, y } => {
                let Some((entity, w)) = window.as_mut() else { continue };
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let scale = w.scale_factor().max(f32::EPSILON);
                let position = Vec2::new(x, y) / scale;
                let delta = w.cursor_position().map(|p| position - p);
                // `bypass_change_detection`: bevy_winit would otherwise try to
                // warp the OS cursor, which Android does not support.
                w.bypass_change_detection().set_physical_cursor_position(Some(DVec2::new(x as f64, y as f64)));
                out.cursor.write(CursorMoved { window: *entity, position, delta });
            }
            AndroidInputEvent::MouseDelta { dx, dy } => {
                if dx.is_finite() && dy.is_finite() {
                    out.motion.write(MouseMotion { delta: Vec2::new(dx, dy) });
                }
            }
            AndroidInputEvent::MouseButtons { state } => {
                let Some((entity, _)) = window.as_ref() else { continue };
                let entity = *entity;
                let before = std::mem::replace(&mut bridge.mouse_state, state);
                for (button, pressed) in mouse_button_changes(before, state) {
                    let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
                    out.mouse_buttons.write(MouseButtonInput { button, state, window: entity });
                }
            }
            AndroidInputEvent::MouseScroll { x, y } => {
                let Some((entity, _)) = window.as_ref() else { continue };
                if x.is_finite() && y.is_finite() {
                    out.wheel.write(MouseWheel { unit: MouseScrollUnit::Line, x, y, window: *entity, phase: TouchPhase::Moved });
                }
            }
            AndroidInputEvent::BackPressed => {
                if let Some(b) = back.as_deref_mut() {
                    b.request();
                }
            }
            AndroidInputEvent::SafeArea { left, top, right, bottom } => {
                if let Some(i) = insets.as_deref_mut() {
                    let want = crate::touch::SafeAreaInsets::new(left, top, right, bottom);
                    if *i != want {
                        *i = want;
                    }
                }
            }
            AndroidInputEvent::ResetAll => {
                const HAT_BTNS: [GamepadButton; 4] =
                    [GamepadButton::DPadUp, GamepadButton::DPadDown, GamepadButton::DPadLeft, GamepadButton::DPadRight];
                const STICKS: [GamepadAxis; 4] =
                    [GamepadAxis::LeftStickX, GamepadAxis::LeftStickY, GamepadAxis::RightStickX, GamepadAxis::RightStickY];
                let b = &mut *bridge;
                for (&device, &pad) in &b.pads {
                    for axis in STICKS {
                        out.axis(pad, axis, 0.0);
                    }
                    if let Some(t) = b.triggers.remove(&device) {
                        if t.left() > 0.0 {
                            out.button(pad, GamepadButton::LeftTrigger2, 0.0);
                        }
                        if t.right() > 0.0 {
                            out.button(pad, GamepadButton::RightTrigger2, 0.0);
                        }
                    }
                    if let Some(last_hat) = b.hats.remove(&device) {
                        for (btn, held) in HAT_BTNS.into_iter().zip(last_hat) {
                            if held {
                                out.button(pad, btn, 0.0);
                            }
                        }
                    }
                    if let Some(held) = b.held_buttons.remove(&device) {
                        for btn in held {
                            out.button(pad, btn, 0.0);
                        }
                    }
                }
                b.triggers.clear();
                b.hats.clear();
                b.held_buttons.clear();
                // Touch: every contact is stale (bindings, the merged pad
                // contribution and armed menu taps are dropped).
                if let Some(t) = touch_reset.as_deref_mut() {
                    t.request();
                }
                let before = std::mem::replace(&mut b.mouse_state, 0);
                if before != 0
                    && let Some((entity, _)) = window.as_ref()
                {
                    let entity = *entity;
                    for (button, pressed) in mouse_button_changes(before, 0) {
                        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
                        out.mouse_buttons.write(MouseButtonInput { button, state, window: entity });
                    }
                }
            }
        }
    }
    bridge.scratch = events;
}

/// The gamepad entity for `device`, connecting it on first input (a pad that
/// was paired before launch announces itself with its first press).
fn pad_for(commands: &mut Commands, bridge: &mut AndroidInputBridge, out: &mut BridgeWriters, device: i32) -> Entity {
    if let Some(&pad) = bridge.pads.get(&device) {
        return pad;
    }
    let pad = commands.spawn_empty().id();
    bridge.pads.insert(device, pad);
    let name = format!("Android gamepad {device}");
    out.connection(GamepadConnectionEvent::new(pad, GamepadConnection::Connected { name, vendor_id: None, product_id: None }));
    pad
}

/// `(button, pressed)` edges between two `getButtonState()` masks.
pub fn mouse_button_changes(before: i32, after: i32) -> impl Iterator<Item = (MouseButton, bool)> {
    MOUSE_BUTTONS.into_iter().filter(move |(bit, _)| (before ^ after) & bit != 0).map(move |(bit, b)| (b, after & bit != 0))
}

/// Whether Kotlin should hold Android pointer capture: the game holds the
/// cursor and touch mode is off (touch never captures the pointer).
pub fn pointer_capture_wanted(captured: bool, mode: crate::gamepad::InputMode) -> bool {
    captured && !mode.is_touch()
}

/// Mirror [`CursorCaptured`] for Kotlin's pointer-capture poll.
pub fn publish_pointer_capture(captured: Option<Res<CursorCaptured>>, mode: Option<Res<crate::gamepad::InputMode>>) {
    let want = pointer_capture_wanted(captured.is_some_and(|c| c.0), mode.map_or(crate::gamepad::InputMode::KeyboardMouse, |m| *m));
    WANTS_POINTER_CAPTURE.store(want, Ordering::Relaxed);
}

/// Added by `GamePlugin` on Android only.
pub struct AndroidInputPlugin;

impl Plugin for AndroidInputPlugin {
    fn build(&self, app: &mut App) {
        // `WindowPlugin` registers it in the game; tests run without it.
        if !app.world().contains_resource::<Messages<CursorMoved>>() {
            app.add_message::<CursorMoved>();
        }
        app.init_resource::<AndroidInputBridge>()
            .add_systems(PreUpdate, drain_android_input.before(InputSystems))
            .add_systems(Last, publish_pointer_capture);
    }
}

/// JNI exports for `dev.bombatomica64.segnaledebole.MainActivity`'s
/// `external fun native*` declarations. Only primitive arguments: no `jni`
/// crate, no `JNIEnv` calls.
#[cfg(target_os = "android")]
#[allow(non_snake_case)]
mod jni_exports {
    use std::ffi::c_void;

    use super::{active_rumble_device, clear_rumble, push_event, take_rumble, wants_pointer_capture, AndroidAxes, AndroidInputEvent};
    use bevy::math::Vec2;

    type Env = *mut c_void;
    type Obj = *mut c_void;

    fn id16(v: i32) -> Option<u16> {
        u16::try_from(v).ok().filter(|&v| v != 0)
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeGamepadConnected(
        _env: Env,
        _this: Obj,
        device: i32,
        vendor: i32,
        product: i32,
    ) {
        push_event(AndroidInputEvent::GamepadConnected { device, vendor_id: id16(vendor), product_id: id16(product) });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeGamepadDisconnected(_env: Env, _this: Obj, device: i32) {
        push_event(AndroidInputEvent::GamepadDisconnected { device });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeGamepadKey(
        _env: Env,
        _this: Obj,
        device: i32,
        keycode: i32,
        pressed: u8,
    ) {
        push_event(AndroidInputEvent::GamepadKey { device, keycode, pressed: pressed != 0 });
    }

    #[unsafe(no_mangle)]
    #[allow(clippy::too_many_arguments)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeGamepadAxes(
        _env: Env,
        _this: Obj,
        device: i32,
        lx: f32,
        ly: f32,
        rx: f32,
        ry: f32,
        lt: f32,
        rt: f32,
        hat_x: f32,
        hat_y: f32,
    ) {
        let axes = AndroidAxes {
            left: Vec2::new(lx, ly),
            right: Vec2::new(rx, ry),
            left_trigger: lt,
            right_trigger: rt,
            hat: Vec2::new(hat_x, hat_y),
        };
        push_event(AndroidInputEvent::GamepadAxes { device, axes });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeMouseMove(_env: Env, _this: Obj, x: f32, y: f32) {
        push_event(AndroidInputEvent::MouseMove { x, y });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeMouseDelta(_env: Env, _this: Obj, dx: f32, dy: f32) {
        push_event(AndroidInputEvent::MouseDelta { dx, dy });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeMouseButtons(_env: Env, _this: Obj, state: i32) {
        push_event(AndroidInputEvent::MouseButtons { state });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeMouseScroll(_env: Env, _this: Obj, x: f32, y: f32) {
        push_event(AndroidInputEvent::MouseScroll { x, y });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeResetInputs(_env: Env, _this: Obj) {
        clear_rumble();
        push_event(AndroidInputEvent::ResetAll);
    }

    /// Kotlin `external fun nativeBackPressed()` (system Back gesture).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeBackPressed(_env: Env, _this: Obj) {
        push_event(AndroidInputEvent::BackPressed);
    }

    /// Kotlin `external fun nativeSafeAreaInsets(l: Float, t: Float, r: Float, b: Float)`
    /// (display-cutout insets, physical px).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeSafeAreaInsets(
        _env: Env,
        _this: Obj,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        push_event(AndroidInputEvent::SafeArea { left, top, right, bottom });
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeWantsPointerCapture(_env: Env, _this: Obj) -> u8 {
        u8::from(wants_pointer_capture())
    }

    /// Kotlin `external fun nativeTakeRumble(): Long` (packed, see `encode_rumble`).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeTakeRumble(_env: Env, _this: Obj) -> i64 {
        take_rumble() as i64
    }

    /// Kotlin `external fun nativeActiveRumbleDevice(): Int` (`-1` if none).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_bombatomica64_segnaledebole_MainActivity_nativeActiveRumbleDevice(_env: Env, _this: Obj) -> i32 {
        active_rumble_device()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::gamepad::Gamepad;

    #[test]
    fn rumble_commands_pack_for_kotlin_and_round_trip() {
        use crate::rumble::{RumbleCommand, RumbleMix};
        assert_eq!(decode_rumble(0), None);
        assert_eq!(decode_rumble(encode_rumble(RumbleCommand::Stop)), Some(RumbleCommand::Stop));
        let play = RumbleCommand::Play(RumbleMix { strong: 1.0, weak: 0.2, duration_ms: 380 });
        let v = encode_rumble(play);
        assert_eq!(v >> 32, RUMBLE_KIND_PLAY);
        assert_eq!((v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFFFF), (255, 51, 380));
        match decode_rumble(v) {
            Some(RumbleCommand::Play(m)) => assert!((m.strong - 1.0).abs() < 0.01 && (m.weak - 0.2).abs() < 0.01 && m.duration_ms == 380),
            other => panic!("{other:?}"),
        }
        let wild = encode_rumble(RumbleCommand::Play(RumbleMix { strong: f32::NAN, weak: 7.0, duration_ms: 999_999 }));
        assert_eq!((wild & 0xFF, (wild >> 8) & 0xFF, (wild >> 16) & 0xFFFF), (0, 255, 65535), "sanitized");
        queue_rumble(play);
        queue_rumble(RumbleCommand::Stop);
        assert_eq!(decode_rumble(take_rumble()), Some(RumbleCommand::Stop), "newest wins");
        assert_eq!(take_rumble(), 0, "taken once");
    }

    #[test]
    fn keycodes_map_to_xbox_layout() {
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_A), Some(GamepadButton::South));
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_B), Some(GamepadButton::East));
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_X), Some(GamepadButton::West));
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_Y), Some(GamepadButton::North));
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_START), Some(GamepadButton::Start));
        assert_eq!(map_gamepad_keycode(keycode::BUTTON_R2), Some(GamepadButton::RightTrigger2));
        assert_eq!(map_gamepad_keycode(keycode::DPAD_LEFT), Some(GamepadButton::DPadLeft));
        assert_eq!(map_gamepad_keycode(4 /* BACK */), None);
        assert_eq!(map_gamepad_keycode(29 /* A key */), None);
    }

    #[test]
    fn axes_flip_y_and_sanitize() {
        let a = AndroidAxes {
            left: Vec2::new(0.5, -1.0),
            right: Vec2::new(f32::NAN, 0.3),
            left_trigger: 2.0,
            right_trigger: f32::INFINITY,
            hat: Vec2::new(-1.0, 1.0),
        };
        let ax = a.bevy_axes();
        assert_eq!(ax[0], (GamepadAxis::LeftStickX, 0.5));
        assert_eq!(ax[1], (GamepadAxis::LeftStickY, 1.0), "stick up reads +y in Bevy");
        assert_eq!(ax[2].1, 0.0);
        assert_eq!(ax[3].1, 0.0);
        assert_eq!(a.triggers(), [(GamepadButton::LeftTrigger2, 1.0), (GamepadButton::RightTrigger2, 0.0)]);
        assert_eq!(
            a.hat_buttons(),
            [(GamepadButton::DPadUp, false), (GamepadButton::DPadDown, true), (GamepadButton::DPadLeft, true), (GamepadButton::DPadRight, false)]
        );
    }

    #[test]
    fn mouse_mask_edges() {
        let v: Vec<_> = mouse_button_changes(0, mouse_bits::PRIMARY | mouse_bits::SECONDARY).collect();
        assert_eq!(v, [(MouseButton::Left, true), (MouseButton::Right, true)]);
        let v: Vec<_> = mouse_button_changes(mouse_bits::PRIMARY | mouse_bits::SECONDARY, mouse_bits::SECONDARY).collect();
        assert_eq!(v, [(MouseButton::Left, false)]);
        assert_eq!(mouse_button_changes(5, 5).count(), 0);
    }

    /// The queue drains into real Bevy gamepad / mouse state.
    /// Serializes the tests that drive the process-wide input bridge /
    /// pointer-capture flag (the test harness runs tests in parallel).
    static BRIDGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn bridge_feeds_bevy_input() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::input::InputPlugin, AndroidInputPlugin));
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.update();
        // Only this test touches the global queue's contents.
        push_event(AndroidInputEvent::GamepadKey { device: 7, keycode: keycode::BUTTON_A, pressed: true });
        push_event(AndroidInputEvent::GamepadAxes {
            device: 7,
            axes: AndroidAxes { left: Vec2::new(0.0, -1.0), right_trigger: 1.0, hat: Vec2::new(1.0, 0.0), ..default() },
        });
        push_event(AndroidInputEvent::MouseButtons { state: mouse_bits::PRIMARY });
        push_event(AndroidInputEvent::MouseDelta { dx: 3.0, dy: -2.0 });
        push_event(AndroidInputEvent::MouseMove { x: 40.0, y: 30.0 });
        app.update();
        let w = app.world_mut();
        let mut pads = w.query::<&Gamepad>();
        let pad = pads.single(w).expect("one bridged gamepad");
        assert!(pad.pressed(GamepadButton::South));
        assert!(pad.pressed(GamepadButton::RightTrigger2));
        assert!(pad.pressed(GamepadButton::DPadRight));
        assert!(pad.get(GamepadAxis::LeftStickY).unwrap() > 0.9, "stick up");
        assert!(w.resource::<ButtonInput<MouseButton>>().just_pressed(MouseButton::Left));
        assert_eq!(w.resource::<bevy::input::mouse::AccumulatedMouseMotion>().delta, Vec2::new(3.0, -2.0));
        let mut win = w.query_filtered::<&Window, With<PrimaryWindow>>();
        assert_eq!(win.single(w).unwrap().physical_cursor_position(), Some(Vec2::new(40.0, 30.0)));

        push_event(AndroidInputEvent::GamepadDisconnected { device: 7 });
        app.update();
        let w = app.world_mut();
        assert_eq!(w.query::<&Gamepad>().iter(w).count(), 0);
        assert!(w.resource::<AndroidInputBridge>().pads.is_empty());

        // Digital R2/L2 stays held even when subsequent stick motion reports 0.0 analog triggers.
        push_event(AndroidInputEvent::GamepadKey { device: 9, keycode: keycode::BUTTON_R2, pressed: true });
        push_event(AndroidInputEvent::GamepadAxes {
            device: 9,
            axes: AndroidAxes { left: Vec2::new(0.6, -0.4), right_trigger: 0.0, ..default() },
        });
        app.update();
        {
            let w = app.world_mut();
            let mut pads = w.query::<&Gamepad>();
            let pad = pads.single(w).expect("bridged gamepad 9");
            assert!(pad.pressed(GamepadButton::RightTrigger2), "stick axes must not cancel digital R2");
            assert!(pad.get(GamepadAxis::LeftStickX).unwrap() > 0.5);
        }

        // Focus loss / pause ResetAll releases every held gamepad button, trigger, hat, stick, and mouse button.
        push_event(AndroidInputEvent::GamepadKey { device: 9, keycode: keycode::BUTTON_A, pressed: true });
        push_event(AndroidInputEvent::GamepadAxes {
            device: 9,
            axes: AndroidAxes { left: Vec2::new(1.0, -1.0), hat: Vec2::new(0.0, -1.0), ..default() },
        });
        push_event(AndroidInputEvent::MouseButtons { state: mouse_bits::PRIMARY | mouse_bits::SECONDARY });
        app.update();
        push_event(AndroidInputEvent::ResetAll);
        app.update();
        {
            let w = app.world_mut();
            let mut pads = w.query::<&Gamepad>();
            let pad = pads.single(w).expect("pad stays connected after ResetAll");
            assert!(!pad.pressed(GamepadButton::South), "A released on ResetAll");
            assert!(!pad.pressed(GamepadButton::RightTrigger2), "digital R2 released on ResetAll");
            assert!(!pad.pressed(GamepadButton::DPadUp), "hat released on ResetAll");
            assert_eq!(pad.get(GamepadAxis::LeftStickX).unwrap(), 0.0);
            assert_eq!(pad.get(GamepadAxis::LeftStickY).unwrap(), 0.0);
            let mouse = w.resource::<ButtonInput<MouseButton>>();
            assert!(!mouse.pressed(MouseButton::Left));
            assert!(!mouse.pressed(MouseButton::Right));
        }

        // Motion coalescing keeps bursts tiny and never drops a subsequent disconnect.
        for i in 0..5000 {
            push_event(AndroidInputEvent::MouseDelta { dx: 1.0, dy: 0.0 });
            push_event(AndroidInputEvent::GamepadAxes {
                device: 9,
                axes: AndroidAxes { left: Vec2::new((i % 10) as f32 * 0.1, 0.0), ..default() },
            });
        }
        push_event(AndroidInputEvent::GamepadDisconnected { device: 9 });
        app.update();
        let w = app.world_mut();
        assert_eq!(w.query::<&Gamepad>().iter(w).count(), 0, "disconnect survived motion flood");
        assert!(w.resource::<AndroidInputBridge>().pads.is_empty());
    }

    #[test]
    fn pointer_capture_mirrors_cursor_captured() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(CursorCaptured(true)).add_systems(Update, publish_pointer_capture);
        app.update();
        assert!(wants_pointer_capture());
        app.insert_resource(CursorCaptured(false));
        app.update();
        assert!(!wants_pointer_capture());
    }

    #[test]
    fn pointer_capture_is_never_wanted_in_touch_mode() {
        use crate::gamepad::InputMode;
        assert!(pointer_capture_wanted(true, InputMode::KeyboardMouse));
        assert!(pointer_capture_wanted(true, InputMode::Gamepad));
        assert!(!pointer_capture_wanted(true, InputMode::Touch), "touch never captures the pointer");
        assert!(!pointer_capture_wanted(false, InputMode::KeyboardMouse));
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(CursorCaptured(true))
            .insert_resource(InputMode::Touch)
            .add_systems(Update, publish_pointer_capture);
        app.update();
        assert!(!wants_pointer_capture());
        app.insert_resource(InputMode::KeyboardMouse);
        app.update();
        assert!(wants_pointer_capture());
        app.insert_resource(CursorCaptured(false));
        app.update();
        assert!(!wants_pointer_capture());
    }

    #[test]
    fn back_gesture_and_safe_area_reach_the_touch_layer() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::input::InputPlugin, AndroidInputPlugin))
            .init_resource::<crate::touch::BackRequest>()
            .init_resource::<crate::touch::SafeAreaInsets>()
            .init_resource::<crate::touch::TouchInterrupt>();
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.update();
        push_event(AndroidInputEvent::SafeArea { left: 10.0, top: 0.0, right: 0.0, bottom: 0.0 });
        push_event(AndroidInputEvent::SafeArea { left: 96.0, top: 0.0, right: 48.0, bottom: 0.0 });
        push_event(AndroidInputEvent::BackPressed);
        // A focus-loss reset never drops the Back or the insets.
        push_event(AndroidInputEvent::ResetAll);
        app.update();
        let w = app.world();
        assert!(w.resource::<crate::touch::BackRequest>().pending, "Back queued for the touch layer");
        assert!(w.resource::<crate::touch::TouchInterrupt>().pending, "ResetAll interrupts every touch contact");
        assert_eq!(*w.resource::<crate::touch::SafeAreaInsets>(), crate::touch::SafeAreaInsets::new(96.0, 0.0, 48.0, 0.0), "newest insets win");
    }

    #[test]
    fn right_stick_pair_prefers_z_rz_then_rx_ry_and_never_triggers() {
        use super::motion_axis as a;
        let stick = Some((-1.0, 1.0));
        let trigger = Some((0.0, 1.0));
        assert!(is_centered_stick(stick));
        assert!(!is_centered_stick(trigger), "0..1 trigger ranges are not sticks");
        assert!(!is_centered_stick(None));
        assert!(!is_centered_stick(Some((-0.4, 0.4))));
        // Standard mapping (Z / RZ sticks).
        assert_eq!(select_right_stick_pair(true, true), Some((a::Z, a::RZ)));
        assert_eq!(select_right_stick_pair(true, false), Some((a::Z, a::RZ)));
        // Z / RZ are triggers (0..1) on this pad: the RX / RY sticks look.
        let z_rz = is_centered_stick(trigger) && is_centered_stick(trigger);
        let rx_ry = is_centered_stick(stick) && is_centered_stick(stick);
        assert_eq!(select_right_stick_pair(z_rz, rx_ry), Some((a::RX, a::RY)));
        // A half-centered or trigger-only pad resolves to None (zero look) so
        // triggers on Z/RZ can never rotate the camera.
        let half = is_centered_stick(stick) && is_centered_stick(None);
        assert_eq!(select_right_stick_pair(half, false), None);
        assert_eq!(select_right_stick_pair(false, false), None);
        for pair in [select_right_stick_pair(false, true), select_right_stick_pair(true, false), select_right_stick_pair(true, true)] {
            let (x, y) = pair.unwrap();
            for t in [a::LTRIGGER, a::RTRIGGER, a::GAS, a::BRAKE] {
                assert!(x != t && y != t, "never a trigger axis");
            }
        }
    }
}
