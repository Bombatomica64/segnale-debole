//! Unified gamepad / controller input (Milestone 23).
//!
//! Exactly one connected [`Gamepad`] (the active pad, see
//! [`choose_active_pad`]) is folded into one [`GamepadInput`] resource in
//! `PreUpdate`, right after Bevy's own input systems; other pads are ignored
//! until they press a button (Milestone 28). Gameplay and
//! menu systems read that resource next to keyboard / mouse, so a controller
//! drives exactly the same physics, weapons, apps and state machine.
//!
//! `Gamepad`, `GamepadButton` and `GamepadAxis` live in `bevy_input`, which is
//! always compiled: this module builds and is testable on every target. Real
//! devices only appear with a backend: the optional native `gamepad` feature
//! (`bevy_gilrs`, needs `libudev` on Linux) or the web build (`bevy_gilrs` on
//! the browser Gamepad API, no native dependencies). Tests spawn virtual
//! `Gamepad` entities and set their buttons / axes directly.
//!
//! Gamepad look, fire, movement and apps never require `CursorCaptured`, so a
//! controller works immediately on Steam Deck, Android and the web.
//!
//! Menus: punk switches and meta cards carry [`GamepadFocusable`]; the D-Pad /
//! Left Stick move a [`GamepadFocus`] spatially across them and `A` activates
//! the focused one. Screens with their own layout logic (App Store, Settings,
//! Compendium) read the navigation one-shots directly.

use bevy::input::gamepad::{Gamepad, GamepadAxis, GamepadButton};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::ui::{ComputedNode, UiGlobalTransform};

use crate::audio::{SfxEvent, SfxQueue, UiSfxGate};
use crate::meta::{live_settings, ProfileStore, GAMEPAD_SENS_RANGE};
use crate::state::AppState;

/// Left Stick radial deadzone (movement and menu navigation).
pub const MOVE_DEADZONE: f32 = 0.16;
/// Right Stick radial deadzone (look).
pub const LOOK_DEADZONE: f32 = 0.14;
/// Response curve exponent: fine micro-aim near the center, fast at full tilt.
pub const LOOK_EXPONENT: f32 = 1.8;
/// Full-deflection yaw rate (rad/s) at sensitivity 1.0.
pub const LOOK_YAW_SPEED: f32 = 2.8;
/// Full-deflection pitch rate (rad/s) at sensitivity 1.0.
pub const LOOK_PITCH_SPEED: f32 = 2.0;
/// Analog trigger travel that counts as pressed.
pub const TRIGGER_THRESHOLD: f32 = 0.35;
/// Left Stick deflection that counts as a menu direction.
pub const NAV_STICK_THRESHOLD: f32 = 0.55;
/// Held menu direction: first repeat after this delay...
pub const NAV_REPEAT_DELAY: f32 = 0.35;
/// ...then one step every `NAV_REPEAT_RATE` seconds.
pub const NAV_REPEAT_RATE: f32 = 0.22;
/// Longest frame the look curve integrates (a hitch never spins the view).
const MAX_LOOK_DT: f32 = 0.1;
/// Stick deflection that lets an unowned pad claim input (stick drift never does).
pub const CLAIM_STICK_THRESHOLD: f32 = 0.5;
/// Movement components below this share of the stick magnitude snap to zero,
/// so a near-pure sideways push reads as a pure strafe (CPMA air-strafe) and a
/// near-pure forward push as W-only air control.
pub const MOVE_AXIS_SNAP: f32 = 0.3;

/// Rescale `v` so its length maps `[deadzone, 1] -> [0, 1]` (zero inside the
/// deadzone, direction preserved, length clamped to 1). NaN / infinite input
/// reads as centered.
pub fn apply_radial_deadzone(v: Vec2, deadzone: f32) -> Vec2 {
    if !v.is_finite() {
        return Vec2::ZERO;
    }
    let dz = if deadzone.is_finite() { deadzone.clamp(0.0, 0.99) } else { 0.0 };
    let len = v.length();
    if len <= dz || len <= f32::EPSILON {
        return Vec2::ZERO;
    }
    let scaled = ((len - dz) / (1.0 - dz)).min(1.0);
    v * (scaled / len)
}

/// This frame's look delta in radians from a raw Right Stick reading:
/// [`LOOK_DEADZONE`] radial deadzone, `mag^1.8` response curve, base turn
/// speed ([`LOOK_YAW_SPEED`] / [`LOOK_PITCH_SPEED`]) x `sensitivity` x `dt`.
///
/// `x` = yaw to the right, `y` = pitch up (stick up looks up unless
/// `invert_y`). Never NaN: bad sensitivity falls back to 1.0, bad `dt` to 0.
pub fn apply_look_curve(stick: Vec2, sensitivity: f32, invert_y: bool, dt: f32) -> Vec2 {
    let v = apply_radial_deadzone(stick, LOOK_DEADZONE);
    let mag = v.length();
    if mag <= f32::EPSILON || !dt.is_finite() || dt <= 0.0 {
        return Vec2::ZERO;
    }
    let sens = if sensitivity.is_finite() { sensitivity.clamp(GAMEPAD_SENS_RANGE.0, GAMEPAD_SENS_RANGE.1) } else { 1.0 };
    let curved = v * (mag.powf(LOOK_EXPONENT) / mag);
    let y = if invert_y { -curved.y } else { curved.y };
    Vec2::new(curved.x * LOOK_YAW_SPEED, y * LOOK_PITCH_SPEED) * sens * dt.min(MAX_LOOK_DT)
}

/// Left Stick -> movement axes (`x` = right, `y` = forward): [`MOVE_DEADZONE`]
/// radial deadzone, then minor components snapped to zero (see
/// [`MOVE_AXIS_SNAP`]) so analog input keeps the keyboard's pure-strafe and
/// air-control movement modes.
pub fn stick_move_axes(stick: Vec2) -> Vec2 {
    snap_move_axes(apply_radial_deadzone(stick, MOVE_DEADZONE))
}

/// Snap the minor component of an already-deadzoned movement vector to zero
/// (see [`MOVE_AXIS_SNAP`]). Shared by the Left Stick and the touch stick.
pub fn snap_move_axes(v: Vec2) -> Vec2 {
    let mag = v.length();
    if !v.is_finite() || mag <= f32::EPSILON {
        return Vec2::ZERO;
    }
    let snap = |c: f32| if c.abs() < mag * MOVE_AXIS_SNAP { 0.0 } else { c };
    Vec2::new(snap(v.x), snap(v.y))
}

// ---------------------------------------------------------------------------
// Raw per-frame snapshot
// ---------------------------------------------------------------------------

/// Bits of [`PadFrame::buttons`].
pub mod bits {
    pub const SOUTH: u32 = 1 << 0;
    pub const EAST: u32 = 1 << 1;
    pub const NORTH: u32 = 1 << 2;
    pub const WEST: u32 = 1 << 3;
    pub const LB: u32 = 1 << 4;
    pub const RB: u32 = 1 << 5;
    pub const LT: u32 = 1 << 6;
    pub const RT: u32 = 1 << 7;
    pub const SELECT: u32 = 1 << 8;
    pub const START: u32 = 1 << 9;
    pub const LTHUMB: u32 = 1 << 10;
    pub const RTHUMB: u32 = 1 << 11;
    pub const DPAD_UP: u32 = 1 << 12;
    pub const DPAD_DOWN: u32 = 1 << 13;
    pub const DPAD_LEFT: u32 = 1 << 14;
    pub const DPAD_RIGHT: u32 = 1 << 15;
}

/// Digital buttons and their [`bits`] flag (triggers also read analog travel).
const BUTTON_BITS: [(GamepadButton, u32); 16] = [
    (GamepadButton::South, bits::SOUTH),
    (GamepadButton::East, bits::EAST),
    (GamepadButton::North, bits::NORTH),
    (GamepadButton::West, bits::WEST),
    (GamepadButton::LeftTrigger, bits::LB),
    (GamepadButton::RightTrigger, bits::RB),
    (GamepadButton::LeftTrigger2, bits::LT),
    (GamepadButton::RightTrigger2, bits::RT),
    (GamepadButton::Select, bits::SELECT),
    (GamepadButton::Start, bits::START),
    (GamepadButton::LeftThumb, bits::LTHUMB),
    (GamepadButton::RightThumb, bits::RTHUMB),
    (GamepadButton::DPadUp, bits::DPAD_UP),
    (GamepadButton::DPadDown, bits::DPAD_DOWN),
    (GamepadButton::DPadLeft, bits::DPAD_LEFT),
    (GamepadButton::DPadRight, bits::DPAD_RIGHT),
];

/// Every connected pad merged into one plain snapshot (no allocation).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadFrame {
    pub connected: bool,
    /// Held [`bits`].
    pub buttons: u32,
    /// Digital buttons with a `just_pressed` edge this frame (preserves fast
    /// press+release taps during a slow frame).
    pub tapped: u32,
    pub left_stick: Vec2,
    pub right_stick: Vec2,
}

impl PadFrame {
    /// Fold one gamepad in: buttons OR together, the strongest stick wins.
    pub fn merge(&mut self, pad: &Gamepad) {
        self.connected = true;
        for (button, bit) in BUTTON_BITS {
            if pad.pressed(button) {
                self.buttons |= bit;
            }
            if pad.just_pressed(button) {
                self.tapped |= bit;
            }
        }
        // Analog triggers (some backends only report travel, never "pressed").
        if pad.get(GamepadButton::LeftTrigger2).is_some_and(|v| v > TRIGGER_THRESHOLD) {
            self.buttons |= bits::LT;
        }
        if pad.get(GamepadButton::RightTrigger2).is_some_and(|v| v > TRIGGER_THRESHOLD) {
            self.buttons |= bits::RT;
        }
        let stick = |x: GamepadAxis, y: GamepadAxis| Vec2::new(pad.get(x).unwrap_or(0.0), pad.get(y).unwrap_or(0.0));
        let l = stick(GamepadAxis::LeftStickX, GamepadAxis::LeftStickY);
        if l.is_finite() && l.length_squared() > self.left_stick.length_squared() {
            self.left_stick = l;
        }
        let r = stick(GamepadAxis::RightStickX, GamepadAxis::RightStickY);
        if r.is_finite() && r.length_squared() > self.right_stick.length_squared() {
            self.right_stick = r;
        }
    }
}

// ---------------------------------------------------------------------------
// Single active controller ownership (Milestone 28)
// ---------------------------------------------------------------------------

/// What one connected pad did this frame, for [`choose_active_pad`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadClaim {
    /// A digital button `just_pressed` this frame (deliberate edge).
    pub edge: bool,
    /// Any button held (or analog trigger past [`TRIGGER_THRESHOLD`]).
    pub held: bool,
    /// Strongest raw stick deflection (either stick).
    pub stick: f32,
}

impl PadClaim {
    /// Classify one pad's snapshot; `prev_buttons` = its held [`bits`] last frame.
    pub fn of(frame: &PadFrame, prev_buttons: u32) -> Self {
        PadClaim {
            edge: (frame.buttons & !prev_buttons) | frame.tapped != 0,
            held: frame.buttons != 0,
            stick: frame.left_stick.length().max(frame.right_stick.length()),
        }
    }

    /// Deliberate enough to claim an unowned seat.
    fn claims_free_seat(&self) -> bool {
        self.edge || self.held || self.stick >= CLAIM_STICK_THRESHOLD
    }
}

/// The pad that owns input this frame. The current owner keeps it while
/// connected unless another pad sends a deliberate button edge (never stick
/// drift). With no (connected) owner, the first pad pressing a button or
/// pushing a stick past [`CLAIM_STICK_THRESHOLD`] claims it.
pub fn choose_active_pad(current: Option<Entity>, pads: &[(Entity, PadClaim)]) -> Option<Entity> {
    let owner = current.filter(|o| pads.iter().any(|(e, _)| e == o));
    if let Some(o) = owner {
        return pads.iter().find(|(e, c)| *e != o && c.edge).map(|(e, _)| *e).or(Some(o));
    }
    pads.iter().find(|(_, c)| c.claims_free_seat()).map(|(e, _)| *e)
}

// ---------------------------------------------------------------------------
// GamepadInput
// ---------------------------------------------------------------------------

/// The merged controller state for this frame. Held fields stay set while
/// held; one-shots are true for exactly one frame (rising edge).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct GamepadInput {
    /// Any `Gamepad` entity exists.
    pub connected: bool,
    /// The one pad that drives everything (see [`choose_active_pad`]); other
    /// connected pads are ignored entirely.
    pub active_pad: Option<Entity>,
    /// The player is navigating with a controller: set on any gamepad input,
    /// cleared on mouse / keyboard activity (gates the menu focus highlight).
    pub active: bool,
    /// Left Stick: `x` = right, `y` = forward (deadzoned, axis-snapped).
    pub move_axes: Vec2,
    /// Right Stick look this frame (radians): `x` = yaw right, `y` = pitch up.
    pub look_delta: Vec2,
    /// Raw Right Stick X past the look deadzone (Compendium turntable).
    pub spin_x: f32,
    /// Deadzoned Right Stick magnitude `[0, 1]` (aim friction fades with it).
    pub look_mag: f32,

    // Held.
    /// `RT` (button or analog travel).
    pub fire: bool,
    /// `RT` rising edge (a fresh trigger squeeze: empty-gun dry click).
    pub fire_just_pressed: bool,
    /// `A` / Cross or `L3`: holding it chains zero-friction bhops like
    /// holding Space. `L3` lets the left thumb bhop while the right thumb
    /// keeps air-strafe aiming on the Right Stick.
    pub jump: bool,

    // One-shots (gameplay).
    /// `A` / `L3` rising edge (jump buffer).
    pub jump_pressed: bool,
    /// `LB`: Airplane Mode dash.
    pub dash: bool,
    /// `LT`: Ping Scan.
    pub ping: bool,
    /// `RB`: 5G Overload.
    pub overload: bool,
    /// `X` / Square: SOS uplink.
    pub interact: bool,
    /// `Y` / Triangle or D-Pad Right.
    pub weapon_next: bool,
    /// D-Pad Left.
    pub weapon_prev: bool,
    /// `Start`.
    pub pause: bool,
    /// `Select` or `R3`: Time Trial instant reset.
    pub trial_reset: bool,
    /// D-Pad Down: weapon inspect flourish.
    pub inspect: bool,
    /// `B` / Circle (gameplay): Phone Punch / parry.
    pub punch: bool,
    /// D-Pad Up (gameplay): Camera Flash Strobe.
    pub flash: bool,
    /// Direct weapon slot pick (Milestone 37: a tap on a touch weapon-deck
    /// slot; a controller never sets it).
    pub weapon_slot: Option<usize>,

    // One-shots (menus; directions repeat while held).
    pub nav_up: bool,
    pub nav_down: bool,
    pub nav_left: bool,
    pub nav_right: bool,
    /// `A`.
    pub confirm: bool,
    /// `B` / Circle.
    pub back: bool,
    /// `LB`.
    pub tab_prev: bool,
    /// `RB`.
    pub tab_next: bool,
    /// `Y` / Triangle.
    pub alt_action: bool,
    /// `LT` (menus: previous page).
    pub page_prev: bool,
    /// `RT` (menus: next page).
    pub page_next: bool,

    /// Last frame's held [`bits`] (edge detection).
    held: u32,
    /// Suppress gameplay jump until `A` is released after confirming a menu
    /// action that enters `Playing` (prevents an unwanted jump on resume/deploy).
    jump_wait_release: bool,
    /// Held menu direction and its repeat countdown.
    nav_dir: IVec2,
    nav_timer: f32,
}

/// The single menu direction of the D-Pad (wins) or the Left Stick.
pub fn nav_direction(buttons: u32, left_stick: Vec2) -> IVec2 {
    let dpad = IVec2::new(
        (buttons & bits::DPAD_RIGHT != 0) as i32 - (buttons & bits::DPAD_LEFT != 0) as i32,
        (buttons & bits::DPAD_UP != 0) as i32 - (buttons & bits::DPAD_DOWN != 0) as i32,
    );
    if dpad != IVec2::ZERO {
        // Diagonals resolve vertically (menus are mostly columns).
        return if dpad.y != 0 { IVec2::new(0, dpad.y) } else { dpad };
    }
    let s = apply_radial_deadzone(left_stick, MOVE_DEADZONE);
    if s.length() < NAV_STICK_THRESHOLD {
        IVec2::ZERO
    } else if s.y.abs() >= s.x.abs() {
        IVec2::new(0, s.y.signum() as i32)
    } else {
        IVec2::new(s.x.signum() as i32, 0)
    }
}

impl GamepadInput {
    /// Fold this frame's snapshot in. `kbm_activity`: any mouse / keyboard
    /// input this frame (drops [`Self::active`] unless the pad also moved).
    pub fn update(&mut self, frame: PadFrame, dt: f32, sensitivity: f32, invert_y: bool, kbm_activity: bool) {
        let active = self.active && !kbm_activity;
        if !frame.connected {
            *self = GamepadInput::default();
            return;
        }

        let held = frame.buttons;
        // Accept a same-frame press+release tap only when the button was
        // previously up, so a slow analog RT squeeze crossing 0.35 and later
        // firing digital `just_pressed` never double-triggers a dry click.
        let edge = (held | frame.tapped) & !self.held;
        let on = |b: u32| held & b != 0;
        let hit = |b: u32| edge & b != 0;
        if !on(bits::SOUTH) {
            self.jump_wait_release = false;
        }

        self.connected = true;
        self.move_axes = stick_move_axes(frame.left_stick);
        self.look_delta = apply_look_curve(frame.right_stick, sensitivity, invert_y, dt);
        let look_stick = apply_radial_deadzone(frame.right_stick, LOOK_DEADZONE);
        self.spin_x = look_stick.x;
        self.look_mag = look_stick.length();

        self.fire = on(bits::RT);
        self.fire_just_pressed = hit(bits::RT);
        // `A` honours the resume / deploy latch; `L3` is never latched (it
        // cannot confirm a menu, so it never carries over into `Playing`).
        self.jump = (on(bits::SOUTH) && !self.jump_wait_release) || on(bits::LTHUMB);
        self.jump_pressed = (hit(bits::SOUTH) && !self.jump_wait_release) || hit(bits::LTHUMB);
        self.dash = hit(bits::LB);
        self.ping = hit(bits::LT);
        self.overload = hit(bits::RB);
        self.interact = hit(bits::WEST);
        self.weapon_next = hit(bits::NORTH | bits::DPAD_RIGHT);
        self.weapon_prev = hit(bits::DPAD_LEFT);
        self.pause = hit(bits::START);
        self.trial_reset = hit(bits::SELECT | bits::RTHUMB);
        self.inspect = hit(bits::DPAD_DOWN);
        self.punch = hit(bits::EAST);
        self.flash = hit(bits::DPAD_UP);
        self.weapon_slot = None;

        self.confirm = hit(bits::SOUTH);
        self.back = hit(bits::EAST);
        self.tab_prev = hit(bits::LB);
        self.tab_next = hit(bits::RB);
        self.alt_action = hit(bits::NORTH);
        self.page_prev = hit(bits::LT);
        self.page_next = hit(bits::RT);

        // Menu directions: fire on press, then repeat while held.
        let dir = nav_direction(held, frame.left_stick);
        let mut step = false;
        if dir != self.nav_dir {
            step = dir != IVec2::ZERO;
            self.nav_timer = NAV_REPEAT_DELAY;
        } else if dir != IVec2::ZERO {
            self.nav_timer -= dt.max(0.0);
            if self.nav_timer <= 0.0 {
                step = true;
                self.nav_timer += NAV_REPEAT_RATE;
                // A long hitch never queues a burst of repeats.
                self.nav_timer = self.nav_timer.max(0.0);
            }
        }
        self.nav_dir = dir;
        self.nav_up = step && dir.y > 0;
        self.nav_down = step && dir.y < 0;
        self.nav_left = step && dir.x < 0;
        self.nav_right = step && dir.x > 0;

        let pad_activity = edge != 0
            || self.move_axes != Vec2::ZERO
            || self.look_delta != Vec2::ZERO
            || apply_radial_deadzone(frame.right_stick, LOOK_DEADZONE) != Vec2::ZERO;
        self.active = active || pad_activity;
        self.held = held;
    }

    /// Any menu direction this frame.
    pub fn nav_any(&self) -> bool {
        self.nav_up || self.nav_down || self.nav_left || self.nav_right
    }

    /// Horizontal menu step (`-1`, `0`, `1`) from Left / Right.
    pub fn nav_x(&self) -> i32 {
        self.nav_right as i32 - self.nav_left as i32
    }

    /// Vertical menu step (`-1` = up, `1` = down: list order).
    pub fn nav_y(&self) -> i32 {
        self.nav_down as i32 - self.nav_up as i32
    }

    /// The input mode these readings imply (see [`InputMode`]).
    pub fn input_mode(&self) -> InputMode {
        if self.active { InputMode::Gamepad } else { InputMode::KeyboardMouse }
    }
}

// ---------------------------------------------------------------------------
// Active input mode + dual prompts
// ---------------------------------------------------------------------------

/// The device the player used last: drives every on-screen prompt (keycaps,
/// footers, HUD hints). Flips to [`InputMode::Gamepad`] on any stick / button
/// input and back to [`InputMode::KeyboardMouse`] on any key, click or mouse
/// motion (mirrors [`GamepadInput::active`]). Milestone 37 adds
/// [`InputMode::Touch`] (see [`crate::touch::resolve_input_mode`]). Only
/// written when it actually changes, so `is_changed()` means "the player
/// switched devices".
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    KeyboardMouse,
    Gamepad,
    /// Touchscreen: the on-screen touch HUD drives the run.
    Touch,
}

impl InputMode {
    pub fn is_pad(self) -> bool {
        self == InputMode::Gamepad
    }

    pub fn is_touch(self) -> bool {
        self == InputMode::Touch
    }

    /// `kbm` or `pad`, whichever matches this mode (touch reads the mouse &
    /// keyboard text: taps act like clicks).
    pub fn pick<'a>(self, kbm: &'a str, pad: &'a str) -> &'a str {
        if self.is_pad() { pad } else { kbm }
    }

    /// `kbm`, `pad` or `touch`, whichever matches this mode.
    pub fn pick3<'a>(self, kbm: &'a str, pad: &'a str, touch: &'a str) -> &'a str {
        match self {
            InputMode::KeyboardMouse => kbm,
            InputMode::Gamepad => pad,
            InputMode::Touch => touch,
        }
    }
}

/// Follow the last device (runs right after [`update_gamepad_input`] and the
/// touch router): see [`crate::touch::resolve_input_mode`]. A touch clears
/// [`GamepadInput::active`], so the pad must move again to win back.
pub fn sync_input_mode(
    mut pad: ResMut<GamepadInput>,
    activity: Option<Res<crate::touch::TouchActivity>>,
    store: Option<Res<ProfileStore>>,
    mut mode: ResMut<InputMode>,
) {
    let touch_mode = live_settings(store.as_deref()).touch_mode;
    let (touched, real_kbm) = activity.map_or((false, false), |a| (a.touched, a.real_kbm));
    let want = crate::touch::resolve_input_mode(*mode, touch_mode, touched, pad.active, real_kbm);
    if touched && touch_mode != crate::meta::TouchMode::Off && pad.active {
        pad.active = false;
    }
    if *mode != want {
        *mode = want;
    }
}

/// A label with a mouse & keyboard and a controller variant (plus an optional
/// touch variant, else touch shows the mouse & keyboard text). On a `Text`
/// entity it rewrites that text; on a button (no `Text`) it rewrites the
/// button's direct `Text` children. [`refresh_dual_prompts`] swaps it live
/// whenever [`InputMode`] flips.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct DualPrompt {
    pub kbm: String,
    pub pad: String,
    pub touch: Option<String>,
}

impl DualPrompt {
    pub fn new(kbm: impl Into<String>, pad: impl Into<String>) -> Self {
        DualPrompt { kbm: kbm.into(), pad: pad.into(), touch: None }
    }

    /// With a dedicated touch label.
    pub fn with_touch(mut self, touch: impl Into<String>) -> Self {
        self.touch = Some(touch.into());
        self
    }

    pub fn get(&self, mode: InputMode) -> &str {
        match (&self.touch, mode) {
            (Some(t), InputMode::Touch) => t,
            _ => mode.pick(&self.kbm, &self.pad),
        }
    }
}

fn write_text(t: &mut Mut<Text>, s: &str) {
    if t.0 != s {
        t.0.clear();
        t.0.push_str(s);
    }
}

/// Apply [`DualPrompt`]s: freshly spawned ones always, every one when the
/// input mode flipped.
#[allow(clippy::type_complexity)]
pub fn refresh_dual_prompts(
    mode: Option<Res<InputMode>>,
    mut direct: Query<(Ref<DualPrompt>, &mut Text)>,
    parents: Query<(Ref<DualPrompt>, &Children), Without<Text>>,
    mut texts: Query<&mut Text, Without<DualPrompt>>,
) {
    let Some(mode) = mode else { return };
    let all = mode.is_changed();
    for (prompt, mut t) in &mut direct {
        if all || prompt.is_changed() {
            write_text(&mut t, prompt.get(*mode));
        }
    }
    for (prompt, children) in &parents {
        if !(all || prompt.is_changed()) {
            continue;
        }
        for c in children {
            if let Ok(mut t) = texts.get_mut(*c) {
                write_text(&mut t, prompt.get(*mode));
            }
        }
    }
}

/// Controller text-entry controls (callsign / run seed editors).
pub const PAD_TEXT_HINT: &str = "UP/DN: CHAR  R: ADD  L: DEL  A: SAVE  B: CANCEL";

/// Cycle `c` by `step` through `charset` (unknown characters start at its head).
pub fn cycle_char(charset: &str, c: char, step: i32) -> char {
    let set: Vec<char> = charset.chars().collect();
    if set.is_empty() {
        return c;
    }
    let i = set.iter().position(|x| *x == c).unwrap_or(0) as i32;
    set[(i + step).rem_euclid(set.len() as i32) as usize]
}

/// One frame of controller text entry on `buf` ([`PAD_TEXT_HINT`]): Left
/// deletes, Right appends the charset's first character, Up / Down cycle the
/// last character (an empty buffer starts one). `A` / `B` are the caller's.
/// Returns whether `buf` changed.
pub fn pad_edit_text(buf: &mut String, pad: &GamepadInput, charset: &str, max: usize) -> bool {
    let before = buf.clone();
    let first = charset.chars().next().unwrap_or('A');
    if pad.nav_left {
        buf.pop();
    }
    if pad.nav_right && buf.chars().count() < max {
        buf.push(first);
    }
    if pad.nav_up || pad.nav_down {
        let step = if pad.nav_up { 1 } else { -1 };
        match buf.pop() {
            Some(c) => buf.push(cycle_char(charset, c, step)),
            None if max > 0 => buf.push(first),
            None => {}
        }
    }
    *buf != before
}

/// Pads tracked for ownership (more are ignored).
const MAX_PADS: usize = 8;

/// Fold the active pad (see [`choose_active_pad`]) into [`GamepadInput`]
/// (zero allocations). Every other connected pad is ignored.
#[allow(clippy::too_many_arguments)]
pub fn update_gamepad_input(
    time: Res<Time>,
    pads: Query<(Entity, &Gamepad)>,
    store: Option<Res<ProfileStore>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    scroll: Option<Res<bevy::input::mouse::AccumulatedMouseScroll>>,
    mut input: ResMut<GamepadInput>,
    mut focus: ResMut<GamepadFocus>,
    mut prev_held: Local<[(Option<Entity>, u32); MAX_PADS]>,
) {
    focus.activate = None;
    // Up to 8 pads tracked without allocating; extra pads are ignored.
    let mut claims = [(Entity::PLACEHOLDER, PadClaim::default()); MAX_PADS];
    let mut held_now = [(None, 0u32); MAX_PADS];
    let mut n = 0;
    for (e, pad) in &pads {
        if n < MAX_PADS {
            let mut f = PadFrame::default();
            f.merge(pad);
            let prev = prev_held.iter().find(|(p, _)| *p == Some(e)).map_or(0, |(_, b)| *b);
            claims[n] = (e, PadClaim::of(&f, prev));
            held_now[n] = (Some(e), f.buttons);
            n += 1;
        }
    }
    let prev_snapshot = *prev_held;
    *prev_held = held_now;
    let prev = input.active_pad;
    let owner = choose_active_pad(prev, &claims[..n]);
    if prev.is_some() && !claims[..n].iter().any(|(e, _)| Some(*e) == prev) {
        // The owner was unplugged: release every held action this frame.
        *input = GamepadInput { connected: n > 0, ..default() };
    }
    let mut frame = PadFrame { connected: n > 0, ..default() };
    if let Some(pad) = owner.and_then(|o| pads.get(o).ok().map(|(_, p)| p)) {
        frame.merge(pad);
    }
    if owner != input.active_pad {
        // Seed `input.held` from the incoming owner's *previous-frame* button
        // mask (both on active handover and post-unplug takeover) so already-
        // held buttons don't invent phantom edges while fresh analog/digital
        // presses on this frame keep their edge.
        input.held = owner
            .and_then(|o| prev_snapshot.iter().find(|(p, _)| *p == Some(o)).map(|(_, b)| *b))
            .unwrap_or(0);
        input.active_pad = owner;
    }
    let kbm = keys.is_some_and(|k| k.get_just_pressed().next().is_some())
        || mouse.is_some_and(|m| m.get_just_pressed().next().is_some())
        || motion.is_some_and(|m| m.delta != Vec2::ZERO)
        || scroll.is_some_and(|s| s.delta != Vec2::ZERO);
    if !frame.connected && !input.connected {
        return;
    }
    let settings = live_settings(store.as_deref());
    input.update(frame, time.delta_secs(), settings.gamepad_sensitivity, settings.gamepad_invert_y, kbm);
}

/// `Start` toggles `Playing <-> Paused` (a transition already queued this
/// frame, e.g. death or a floor clear, wins over the pause).
pub fn gamepad_pause_toggle(pad: Res<GamepadInput>, state: Res<State<AppState>>, mut next: ResMut<NextState<AppState>>) {
    if !pad.pause {
        return;
    }
    match state.get() {
        AppState::Playing if matches!(*next, NextState::Unchanged) => next.set(AppState::Paused),
        AppState::Paused => next.set(AppState::Playing),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Menu focus
// ---------------------------------------------------------------------------

/// A UI control the D-Pad / Left Stick can focus (punk switches, meta cards).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct GamepadFocusable;

/// While `true` the D-Pad / Left Stick / `A` drive a screen's own text
/// entry (the Leaderboard callsign editor) instead of moving the menu focus.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PadFocusLock(pub bool);

/// Preferred initial focus of a screen (else the top-left-most control).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DefaultFocus;

/// The controller's focused control and this frame's activation.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GamepadFocus {
    pub focused: Option<Entity>,
    /// `A` on the focused control this frame (consumed like a click).
    pub activate: Option<Entity>,
}

impl GamepadFocus {
    /// Whether `e` should draw the focus highlight.
    pub fn shows(&self, pad: &GamepadInput, e: Entity) -> bool {
        pad.active && self.focused == Some(e)
    }
}

/// Screen-space direction (UI `y` grows downward) of a menu step.
pub fn nav_screen_dir(pad: &GamepadInput) -> Option<Vec2> {
    if pad.nav_up {
        Some(Vec2::NEG_Y)
    } else if pad.nav_down {
        Some(Vec2::Y)
    } else if pad.nav_left {
        Some(Vec2::NEG_X)
    } else if pad.nav_right {
        Some(Vec2::X)
    } else {
        None
    }
}

/// Spatial navigation: the nearest candidate center ahead of `from` along
/// `dir` (sideways offset weighs 2.5x), or, when nothing is ahead, wrap to the
/// farthest one behind in the same lane.
pub fn spatial_pick(from: Vec2, dir: Vec2, candidates: impl Iterator<Item = (Entity, Vec2)> + Clone) -> Option<Entity> {
    let score = |c: Vec2| {
        let d = c - from;
        let along = d.dot(dir);
        let ortho = (d - dir * along).length();
        (along, ortho)
    };
    let ahead = candidates
        .clone()
        .filter_map(|(e, c)| {
            let (along, ortho) = score(c);
            (along > 4.0).then_some((e, along + ortho * 2.5))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((e, _)) = ahead {
        return Some(e);
    }
    candidates
        .filter_map(|(e, c)| {
            let (along, ortho) = score(c);
            (along < -4.0).then_some((e, along + ortho * 2.5))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(e, _)| e)
}

type FocusData = (Entity, &'static ComputedNode, &'static UiGlobalTransform, Option<&'static InheritedVisibility>, Has<DefaultFocus>);

/// Laid-out, visible focusable controls.
fn focus_ready(n: &ComputedNode, vis: Option<&InheritedVisibility>) -> bool {
    n.size().x > 0.0 && n.size().y > 0.0 && vis.is_none_or(|v| v.get())
}

/// Move the menu focus with the D-Pad / Left Stick and turn `A` into an
/// activation of the focused control. Runs on every screen except gameplay
/// and the screens with bespoke controller logic.
#[allow(clippy::type_complexity)]
pub fn gamepad_menu_focus(
    pad: Res<GamepadInput>,
    mut focus: ResMut<GamepadFocus>,
    lock: Option<Res<PadFocusLock>>,
    items: Query<FocusData, With<GamepadFocusable>>,
    (time, gate, sfx): (Option<Res<Time>>, Option<ResMut<UiSfxGate>>, Option<ResMut<SfxQueue>>),
) {
    if lock.is_some_and(|l| l.0) {
        return;
    }
    if !pad.connected {
        focus.focused = None;
        return;
    }
    let current = focus.focused.and_then(|e| items.get(e).ok()).filter(|(_, n, _, v, _)| focus_ready(n, *v));
    let ready = || items.iter().filter(|(_, n, _, v, _)| focus_ready(n, *v));
    let mut focused = current.map(|(e, ..)| e);
    // Only a real D-Pad / stick step ticks (a fresh screen's initial pick is silent).
    let mut moved = false;
    if let Some((_, _, tf, _, _)) = current {
        if let Some(dir) = nav_screen_dir(&pad) {
            let from = tf.translation;
            let cands = ready().filter(|(e, ..)| Some(*e) != focused).map(|(e, _, t, _, _)| (e, t.translation));
            if let Some(e) = spatial_pick(from, dir, cands) {
                moved = focused != Some(e);
                focused = Some(e);
            }
        }
    } else {
        // Fresh screen: the marked default, else the top-left-most control.
        focused = ready()
            .min_by(|(_, _, ta, _, da), (_, _, tb, _, db)| {
                let row = |t: &UiGlobalTransform| (t.translation.y / 4.0).round() as i32;
                (!*da, row(ta)).cmp(&(!*db, row(tb))).then(ta.translation.x.total_cmp(&tb.translation.x))
            })
            .map(|(e, ..)| e);
    }
    if focus.focused != focused {
        focus.focused = focused;
    }
    let mut cue = moved.then_some(SfxEvent::UiTick);
    if pad.confirm
        && let Some(e) = focused
    {
        focus.activate = Some(e);
        cue = Some(SfxEvent::UiConfirm);
    }
    if let (Some(cue), Some(time), Some(mut gate), Some(mut sfx)) = (cue, time, gate, sfx) {
        gate.play(&mut sfx, cue, time.elapsed_secs());
    }
}

/// Entering `Playing` while `A` is still held from confirming Resume / Deploy /
/// Install Upgrade waits for `A` to be released before triggering gameplay jump.
fn latch_jump_on_play_enter(mut pad: ResMut<GamepadInput>) {
    if pad.held & bits::SOUTH != 0 {
        pad.jump_wait_release = true;
        pad.jump = false;
        pad.jump_pressed = false;
    }
}

pub struct GamepadPlugin;

impl Plugin for GamepadPlugin {
    fn build(&self, app: &mut App) {
        let bespoke_screen = in_state(AppState::Playing)
            .or_else(in_state(AppState::UpgradeSelect))
            .or_else(in_state(AppState::Settings))
            .or_else(in_state(AppState::Compendium));
        app.init_resource::<GamepadInput>()
            .init_resource::<GamepadFocus>()
            .init_resource::<PadFocusLock>()
            .init_resource::<InputMode>()
            .init_resource::<UiSfxGate>()
            .add_systems(OnEnter(AppState::Playing), latch_jump_on_play_enter)
            .add_systems(
                PreUpdate,
                (update_gamepad_input, sync_input_mode, gamepad_menu_focus.run_if(not(bespoke_screen))).chain().after(InputSystems),
            )
            .add_systems(Update, refresh_dual_prompts)
            .add_systems(Update, gamepad_pause_toggle.run_if(in_state(AppState::Playing).or_else(in_state(AppState::Paused))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn radial_deadzone_zeroes_center_and_rescales_ramp() {
        assert_eq!(apply_radial_deadzone(Vec2::new(0.1, 0.1), 0.16), Vec2::ZERO);
        assert_eq!(apply_radial_deadzone(Vec2::new(0.16, 0.0), 0.16), Vec2::ZERO);
        // Just past the deadzone is tiny (no jump to 0.16), full tilt is 1.
        let edge = apply_radial_deadzone(Vec2::new(0.17, 0.0), 0.16);
        assert!(edge.x > 0.0 && edge.x < 0.02, "{edge}");
        assert!(approx(apply_radial_deadzone(Vec2::new(1.0, 0.0), 0.16), Vec2::X));
        // Halfway through the live range maps to 0.5, direction kept.
        let mid = apply_radial_deadzone(Vec2::new(0.0, -0.58), 0.16);
        assert!(approx(mid, Vec2::new(0.0, -0.5)), "{mid}");
        let diag = apply_radial_deadzone(Vec2::new(0.6, 0.6), 0.16);
        assert!((diag.x - diag.y).abs() < 1e-6);
        // Monotonic ramp.
        let mut last = 0.0;
        for i in 0..=100 {
            let l = apply_radial_deadzone(Vec2::new(i as f32 / 100.0, 0.0), 0.2).length();
            assert!(l >= last);
            last = l;
        }
    }

    #[test]
    fn radial_deadzone_clamps_and_survives_bad_input() {
        // Square-gate corners (length ~1.41) clamp to 1.
        let corner = apply_radial_deadzone(Vec2::new(1.0, 1.0), 0.16);
        assert!((corner.length() - 1.0).abs() < 1e-5, "{corner}");
        assert!((apply_radial_deadzone(Vec2::new(30.0, 0.0), 0.16).length() - 1.0).abs() < 1e-5);
        assert_eq!(apply_radial_deadzone(Vec2::new(f32::NAN, 0.5), 0.16), Vec2::ZERO);
        assert_eq!(apply_radial_deadzone(Vec2::new(f32::INFINITY, 0.0), 0.16), Vec2::ZERO);
        assert_eq!(apply_radial_deadzone(Vec2::ZERO, 0.0), Vec2::ZERO);
        let v = apply_radial_deadzone(Vec2::new(0.5, 0.0), f32::NAN);
        assert!(approx(v, Vec2::new(0.5, 0.0)));
        assert!(apply_radial_deadzone(Vec2::new(0.995, 0.0), 5.0).is_finite());
    }

    #[test]
    fn look_curve_is_precise_near_center_and_fast_at_full_tilt() {
        let dt = 1.0 / 60.0;
        assert_eq!(apply_look_curve(Vec2::new(0.09, 0.09), 1.0, false, dt), Vec2::ZERO);
        let full = apply_look_curve(Vec2::X, 1.0, false, dt);
        assert!((full.x - LOOK_YAW_SPEED * dt).abs() < 1e-6 && full.y == 0.0, "{full}");
        let up = apply_look_curve(Vec2::Y, 1.0, false, dt);
        assert!((up.y - LOOK_PITCH_SPEED * dt).abs() < 1e-6, "stick up looks up: {up}");
        // Half tilt turns far less than half speed (curve exponent 1.8).
        let half = apply_look_curve(Vec2::new(0.57, 0.0), 1.0, false, dt).x;
        assert!(half > 0.0 && half < 0.35 * full.x, "{half} vs {}", full.x);
        // Sensitivity scales linearly, invert flips only Y, dt is per frame.
        assert!(approx(apply_look_curve(Vec2::X, 2.0, false, dt), full * 2.0));
        let inv = apply_look_curve(Vec2::new(0.7, 0.7), 1.0, true, dt);
        let norm = apply_look_curve(Vec2::new(0.7, 0.7), 1.0, false, dt);
        assert!(approx(inv, Vec2::new(norm.x, -norm.y)));
        assert!(approx(apply_look_curve(Vec2::X, 1.0, false, dt * 2.0), full * 2.0));
    }

    #[test]
    fn look_curve_clamps_and_survives_bad_input() {
        let dt = 1.0 / 60.0;
        let full = apply_look_curve(Vec2::X, 1.0, false, dt);
        // Over-range stick and sensitivity clamp; NaN everything reads zero / default.
        assert!(approx(apply_look_curve(Vec2::new(9.0, 0.0), 1.0, false, dt), full));
        assert!(approx(apply_look_curve(Vec2::X, 99.0, false, dt), full * GAMEPAD_SENS_RANGE.1));
        assert!(approx(apply_look_curve(Vec2::X, 0.0, false, dt), full * GAMEPAD_SENS_RANGE.0));
        assert!(approx(apply_look_curve(Vec2::X, f32::NAN, false, dt), full));
        assert_eq!(apply_look_curve(Vec2::new(f32::NAN, 1.0), 1.0, false, dt), Vec2::ZERO);
        assert_eq!(apply_look_curve(Vec2::X, 1.0, false, f32::NAN), Vec2::ZERO);
        assert_eq!(apply_look_curve(Vec2::X, 1.0, false, -1.0), Vec2::ZERO);
        // A 2 s hitch never spins more than MAX_LOOK_DT worth.
        assert!(apply_look_curve(Vec2::X, 1.0, false, 2.0).x <= LOOK_YAW_SPEED * MAX_LOOK_DT + 1e-6);
    }

    #[test]
    fn move_axes_snap_minor_components_for_pure_strafe() {
        assert_eq!(stick_move_axes(Vec2::new(0.1, 0.1)), Vec2::ZERO);
        let strafe = stick_move_axes(Vec2::new(0.95, 0.12));
        assert!(strafe.y == 0.0 && strafe.x > 0.9, "{strafe}");
        let fwd = stick_move_axes(Vec2::new(-0.1, 1.0));
        assert!(fwd.x == 0.0 && fwd.y > 0.95, "{fwd}");
        let diag = stick_move_axes(Vec2::new(0.7, 0.7));
        assert!(diag.x > 0.5 && diag.y > 0.5, "{diag}");
    }

    fn frame(buttons: u32) -> PadFrame {
        PadFrame { connected: true, buttons, ..default() }
    }

    #[test]
    fn edges_fire_once_and_held_buttons_stay() {
        let mut g = GamepadInput::default();
        g.update(frame(bits::SOUTH | bits::RT | bits::START), 1.0 / 60.0, 1.0, false, false);
        assert!(g.connected && g.active && g.jump && g.jump_pressed && g.confirm && g.fire && g.pause);
        g.update(frame(bits::SOUTH | bits::RT | bits::START), 1.0 / 60.0, 1.0, false, false);
        assert!(g.jump && g.fire, "held stays");
        assert!(!g.jump_pressed && !g.confirm && !g.pause, "edges fire once");
        g.update(frame(bits::LB | bits::RB | bits::LT | bits::WEST | bits::NORTH | bits::SELECT | bits::EAST), 1.0 / 60.0, 1.0, false, false);
        assert!(g.dash && g.tab_prev && g.overload && g.tab_next && g.ping && g.interact && g.weapon_next && g.alt_action && g.trial_reset && g.back);
        assert!(!g.jump && !g.fire);
        g.update(frame(bits::RTHUMB), 1.0 / 60.0, 1.0, false, false);
        assert!(g.trial_reset);
        g.update(frame(bits::DPAD_LEFT), 1.0 / 60.0, 1.0, false, false);
        assert!(g.weapon_prev && g.nav_left && !g.weapon_next);
        g.update(frame(bits::DPAD_DOWN), 1.0 / 60.0, 1.0, false, false);
        assert!(g.inspect && g.nav_down);
        g.update(PadFrame::default(), 1.0 / 60.0, 1.0, false, false);
        assert_eq!(g, GamepadInput::default(), "unplugged resets everything");
    }

    #[test]
    fn menu_directions_repeat_while_held() {
        let mut g = GamepadInput::default();
        let dt = 0.01;
        let mut steps = 0;
        // 1.0 s of a held D-Pad Down: press + repeats after 0.35 s every 0.22 s.
        for _ in 0..100 {
            g.update(frame(bits::DPAD_DOWN), dt, 1.0, false, false);
            steps += g.nav_down as u32;
            assert!(!g.nav_up && !g.nav_left && !g.nav_right);
        }
        assert_eq!(steps, 4, "press at 0, repeats at ~0.35 / 0.57 / 0.79");
        // Left Stick drives the same, and releasing re-arms an instant step.
        g.update(PadFrame { connected: true, left_stick: Vec2::new(0.0, 0.9), ..default() }, dt, 1.0, false, false);
        assert!(g.nav_up);
        g.update(PadFrame { connected: true, left_stick: Vec2::new(0.0, 0.2), ..default() }, dt, 1.0, false, false);
        assert!(!g.nav_any());
        g.update(PadFrame { connected: true, left_stick: Vec2::new(-0.9, 0.3), ..default() }, dt, 1.0, false, false);
        assert!(g.nav_left && g.nav_x() == -1);
    }

    #[test]
    fn mouse_and_keyboard_clear_active_until_the_pad_moves() {
        let mut g = GamepadInput::default();
        g.update(frame(bits::SOUTH), 0.016, 1.0, false, false);
        assert!(g.active);
        g.update(frame(bits::SOUTH), 0.016, 1.0, false, true);
        assert!(!g.active, "mouse moved while A merely held");
        g.update(frame(0), 0.016, 1.0, false, false);
        assert!(!g.active);
        g.update(PadFrame { connected: true, right_stick: Vec2::X, ..default() }, 0.016, 1.0, false, false);
        assert!(g.active && g.look_delta.x > 0.0 && g.spin_x > 0.9);
    }

    #[test]
    fn l3_jumps_and_bhops_alongside_a_and_ignores_the_a_latch() {
        let dt = 1.0 / 60.0;
        let mut g = GamepadInput::default();
        g.update(frame(bits::LTHUMB), dt, 1.0, false, false);
        assert!(g.jump && g.jump_pressed, "L3 press jumps");
        assert!(!g.confirm, "L3 never confirms a menu");
        g.update(frame(bits::LTHUMB), dt, 1.0, false, false);
        assert!(g.jump && !g.jump_pressed, "held L3 keeps bhopping");
        // L3 held with the right stick deflected: jump + look together.
        g.update(PadFrame { connected: true, buttons: bits::LTHUMB, right_stick: Vec2::X, ..default() }, dt, 1.0, false, false);
        assert!(g.jump && g.look_delta.x > 0.0);
        // The A latch from a menu confirm (resume / deploy) blocks A only.
        let mut g = GamepadInput::default();
        g.update(frame(bits::SOUTH), dt, 1.0, false, false);
        g.held = bits::SOUTH;
        g.jump_wait_release = true;
        g.update(frame(bits::SOUTH), dt, 1.0, false, false);
        assert!(!g.jump && !g.jump_pressed, "A still latched");
        g.update(frame(bits::SOUTH | bits::LTHUMB), dt, 1.0, false, false);
        assert!(g.jump && g.jump_pressed, "L3 jumps through the A latch");
        g.update(frame(0), dt, 1.0, false, false);
        g.update(frame(bits::SOUTH), dt, 1.0, false, false);
        assert!(g.jump && g.jump_pressed, "releasing A clears the latch");
    }

    #[test]
    fn triggers_page_menus_and_input_mode_follows_the_last_device() {
        let dt = 1.0 / 60.0;
        let mut g = GamepadInput::default();
        assert_eq!(g.input_mode(), InputMode::KeyboardMouse);
        g.update(frame(bits::LT), dt, 1.0, false, false);
        assert!(g.page_prev && !g.page_next && g.ping);
        g.update(frame(bits::RT), dt, 1.0, false, false);
        assert!(g.page_next && !g.page_prev && g.fire);
        g.update(frame(bits::RT), dt, 1.0, false, false);
        assert!(!g.page_next, "one page per pull");
        assert_eq!(g.input_mode(), InputMode::Gamepad);
        g.update(frame(0), dt, 1.0, false, true);
        assert_eq!(g.input_mode(), InputMode::KeyboardMouse, "a key / mouse move switches back");
        g.update(PadFrame { connected: true, left_stick: Vec2::new(0.0, 0.9), ..default() }, dt, 1.0, false, false);
        assert_eq!(g.input_mode(), InputMode::Gamepad, "the stick switches to the pad");
        assert_eq!(InputMode::Gamepad.pick("[E]", "[X]"), "[X]");
        assert_eq!(InputMode::KeyboardMouse.pick("[E]", "[X]"), "[E]");
        let p = DualPrompt::new("BACK  [ESC]", "BACK  [B]");
        assert_eq!((p.get(InputMode::KeyboardMouse), p.get(InputMode::Gamepad)), ("BACK  [ESC]", "BACK  [B]"));
        // Touch: the mouse & keyboard text unless a touch label exists.
        assert_eq!(p.get(InputMode::Touch), "BACK  [ESC]");
        assert_eq!(p.clone().with_touch("BACK").get(InputMode::Touch), "BACK");
        assert_eq!(InputMode::Touch.pick("[E]", "[X]"), "[E]");
        assert_eq!(InputMode::Touch.pick3("[E]", "[X]", "TAP SOS"), "TAP SOS");
        assert_eq!(InputMode::Gamepad.pick3("[E]", "[X]", "TAP SOS"), "[X]");
        assert!(InputMode::Touch.is_touch() && !InputMode::Touch.is_pad());
    }

    #[test]
    fn pad_text_entry_cycles_adds_and_deletes() {
        let set = "AB-";
        let with = |f: fn(&mut GamepadInput)| {
            let mut p = GamepadInput::default();
            f(&mut p);
            p
        };
        let mut buf = String::new();
        assert!(pad_edit_text(&mut buf, &with(|p| p.nav_up = true), set, 3));
        assert_eq!(buf, "A", "up on an empty buffer starts a character");
        pad_edit_text(&mut buf, &with(|p| p.nav_up = true), set, 3);
        assert_eq!(buf, "B");
        pad_edit_text(&mut buf, &with(|p| p.nav_down = true), set, 3);
        pad_edit_text(&mut buf, &with(|p| p.nav_down = true), set, 3);
        assert_eq!(buf, "-", "down wraps");
        for _ in 0..5 {
            pad_edit_text(&mut buf, &with(|p| p.nav_right = true), set, 3);
        }
        assert_eq!(buf, "-AA", "right adds up to the cap");
        assert!(pad_edit_text(&mut buf, &with(|p| p.nav_left = true), set, 3));
        assert_eq!(buf, "-A");
        assert!(!pad_edit_text(&mut buf, &GamepadInput::default(), set, 3), "no input, no change");
        assert_eq!(cycle_char(set, 'Z', 1), 'B', "unknown chars start at the head");
        assert!(PAD_TEXT_HINT.contains("UP/DN: CHAR") && PAD_TEXT_HINT.contains("B: CANCEL"));
    }

    #[test]
    fn spatial_pick_prefers_aligned_neighbours_and_wraps() {
        let e = |i: u32| Entity::from_raw_u32(i).unwrap();
        // A 2x3 grid, rows 60 px apart, columns 200 px apart.
        let grid: Vec<(Entity, Vec2)> = (0..6).map(|i| (e(i + 1), Vec2::new((i % 2) as f32 * 200.0, (i / 2) as f32 * 60.0))).collect();
        let from = Vec2::new(0.0, 60.0); // e(3), left column middle row
        let others = || grid.iter().copied().filter(|(x, _)| *x != e(3));
        assert_eq!(spatial_pick(from, Vec2::Y, others()), Some(e(5)));
        assert_eq!(spatial_pick(from, Vec2::NEG_Y, others()), Some(e(1)));
        assert_eq!(spatial_pick(from, Vec2::X, others()), Some(e(4)));
        // Nothing left of the left column: wrap to the far right of the row.
        assert_eq!(spatial_pick(from, Vec2::NEG_X, others()), Some(e(4)));
        // Bottom of a column wraps to its top.
        let others5 = grid.iter().copied().filter(|(x, _)| *x != e(5));
        assert_eq!(spatial_pick(Vec2::new(0.0, 120.0), Vec2::Y, others5), Some(e(1)));
        assert_eq!(spatial_pick(Vec2::ZERO, Vec2::Y, std::iter::empty()), None);
    }

    #[test]
    fn active_pad_ownership_claims_keeps_and_transfers_on_button_edges_only() {
        let a = Entity::from_raw_u32(1).unwrap();
        let b = Entity::from_raw_u32(2).unwrap();
        let idle = PadClaim::default();
        let drift = PadClaim { stick: 0.3, ..default() };
        let full_stick = PadClaim { stick: 1.0, ..default() };
        let press = PadClaim { edge: true, held: true, ..default() };
        let held = PadClaim { held: true, ..default() };
        // Nobody owns: drift never claims, a deliberate stick or button does.
        assert_eq!(choose_active_pad(None, &[(a, idle), (b, drift)]), None);
        assert_eq!(choose_active_pad(None, &[(a, idle), (b, full_stick)]), Some(b));
        assert_eq!(choose_active_pad(None, &[(a, held), (b, idle)]), Some(a));
        // A owns: B's full stick or a merely held button never steals it.
        assert_eq!(choose_active_pad(Some(a), &[(a, idle), (b, full_stick)]), Some(a));
        assert_eq!(choose_active_pad(Some(a), &[(a, idle), (b, held)]), Some(a));
        // A fresh button edge on B transfers ownership.
        assert_eq!(choose_active_pad(Some(a), &[(a, full_stick), (b, press)]), Some(b));
        // The owner unplugged: the seat frees (and only a deliberate pad reclaims it).
        assert_eq!(choose_active_pad(Some(a), &[(b, drift)]), None);
        assert_eq!(choose_active_pad(Some(a), &[]), None);
    }

    #[test]
    fn fire_just_pressed_is_a_single_rising_edge() {
        let dt = 1.0 / 60.0;
        let mut g = GamepadInput::default();
        g.update(frame(bits::RT), dt, 1.0, false, false);
        assert!(g.fire && g.fire_just_pressed);
        g.update(frame(bits::RT), dt, 1.0, false, false);
        assert!(g.fire && !g.fire_just_pressed, "held trigger is not a new squeeze");
        // Slow squeeze: analog crossed 0.35 on frame 1, then digital just_pressed fires on frame 2.
        let delayed_digital = PadFrame { connected: true, buttons: bits::RT, tapped: bits::RT, ..default() };
        g.update(delayed_digital, dt, 1.0, false, false);
        assert!(g.fire && !g.fire_just_pressed, "delayed digital tap while already held must not double-trigger");
        g.update(frame(0), dt, 1.0, false, false);
        g.update(frame(bits::RT), dt, 1.0, false, false);
        assert!(g.fire_just_pressed);
    }
}
