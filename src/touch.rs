//! Milestone 37: touch controls (Android APK + mobile web).
//!
//! One platform-neutral layer over Bevy's [`Touches`] resource, so a phone
//! browser (itch.io) and the native APK behave identically:
//!
//! - [`TouchLayout`]: where every on-screen control sits (logical px), inside
//!   the display-cutout [`SafeAreaInsets`] and scaled by
//!   `UserSettings::touch_button_scale`.
//! - [`TouchRouter`]: a deterministic multi-touch router. Each finger is bound
//!   on press to one role and keeps it until lift: a floating left-half
//!   thumbstick, right-half drag look, or one action button. The FIRE finger
//!   also aims (its drag feeds the look delta). Auto-bhop holds jump while
//!   the stick sits on its rim.
//! - [`merge_touch_into_pad`] folds the router's [`TouchFrame`] into
//!   [`GamepadInput`], so touch drives exactly the same movement, weapons,
//!   apps, punch, flash, SOS and pause code as a controller (no gameplay
//!   system needed a touch-specific branch).
//! - [`resolve_input_mode`]: `InputMode::Touch` switching (Auto / On / Off),
//!   guarded against the synthetic mouse events browsers emit after a tap.
//! - [`UiPointer`]: one pointer for every menu click handler: the mouse, or
//!   a finger (press highlight while down, activation on lift inside the
//!   target), never both for the same tap.
//! - [`BackRequest`]: the Android system Back gesture, replayed as a one-frame
//!   `Escape` press so every screen's existing Esc handling applies (pause in
//!   a run, close the controls overlay, back out of a submenu; nothing on the
//!   main menu, so Back never kills the app).
//!
//! Everything except the Bevy systems is pure and unit-tested on the desktop.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::input::touch::Touches;
use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::enemies::Enemy;
use crate::extraction::ExtractionState;
use crate::gamepad::{apply_radial_deadzone, snap_move_axes, update_gamepad_input, GamepadInput, InputMode};
use crate::level::{DemonJammer, LevelGrid};
use crate::meta::{live_settings, ProfileStore, TouchMode, TOUCH_BUTTON_SCALE_RANGE, TOUCH_SENS_RANGE};
use crate::player::{live_eye_pose, silhouette_angle, CursorCaptured, Player, PlayerCamera};
use crate::signal::SignalState;
use crate::state::{AppState, RunMode, RunStats};
use crate::weapons::{Weapon, WeaponState, JAMMER_SPHERES};

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

/// Reference phone height (logical px) the layout is designed at (a 20:9
/// phone in landscape is ~390-412 dp tall).
pub const LAYOUT_REFERENCE_H: f32 = 412.0;
/// Edge margin (design px) kept inside the safe area.
pub const LAYOUT_MARGIN: f32 = 14.0;
/// Floating stick radius (design px): full deflection at this distance.
pub const STICK_RADIUS: f32 = 54.0;
/// Radial deadzone of the touch stick (a resting thumb never creeps).
pub const TOUCH_STICK_DEADZONE: f32 = 0.12;
/// The anchor starts following the finger past this share of the radius...
pub const STICK_FOLLOW_START: f32 = 1.1;
/// ...closing the excess at this rate (1/s), so dragging far past the rim and
/// back reverses the strafe at once instead of crossing a dead stretch.
pub const STICK_FOLLOW_RATE: f32 = 18.0;
/// Stick deflection that holds jump with auto-bhop on.
pub const AUTO_BHOP_RIM: f32 = 0.85;
/// Look gain: radians turned by a drag across one full viewport height at
/// sensitivity 1.0 (normalising by height makes every DPI feel the same).
pub const TOUCH_LOOK_GAIN: f32 = 2.8;
/// Largest drag one finger contributes per frame (viewport heights): a
/// digitizer glitch never spins the view.
pub const TOUCH_LOOK_SPIKE: f32 = 0.22;
/// Turn rate (rad/s) that counts as a full flick: aim friction is gone.
pub const TOUCH_FLICK_SPEED: f32 = 2.4;
/// Seconds after the last touch during which mouse events are treated as the
/// browser's synthetic compatibility mouse (never a real mouse).
pub const TOUCH_MOUSE_GUARD_SECS: f32 = 0.8;
/// Extra hit margin around every button (logical px): fat-finger forgiveness.
pub const BUTTON_SLOP: f32 = 6.0;
/// Extra hit margin (logical px) a finger gets around menu buttons: fat-finger
/// forgiveness on scaled-down phone menus (the mouse stays pixel exact).
pub const TOUCH_UI_SLOP: f32 = 5.0;
/// Extra vertical hit margin around the (thin) weapon-deck slots.
pub const SLOT_SLOP: f32 = 12.0;
/// Finger id of the mouse acting as a finger (`TouchMode::On` previews).
pub const MOUSE_FINGER_ID: u64 = u64::MAX - 7;
/// Auto-fire cone half-angle around the view ray (radians, ~2.9 degrees).
pub const AUTO_FIRE_CONE_RAD: f32 = 0.05;

/// Auto-fire reach of each weapon (meters): the shotgun only fires close.
pub fn auto_fire_range(w: Weapon) -> f32 {
    match w {
        Weapon::Shotgun => 16.0,
        Weapon::Plasma => 38.0,
        Weapon::Rocket => 34.0,
    }
}

// ---------------------------------------------------------------------------
// Safe area
// ---------------------------------------------------------------------------

/// Display-cutout / rounded-corner insets in **physical** px, as Android
/// reports them (`WindowInsetsCompat.Type.displayCutout()`). Zero on the web
/// and the desktop. Touch HUD margins never place a control inside them.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct SafeAreaInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl SafeAreaInsets {
    pub fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        SafeAreaInsets { left, top, right, bottom }
    }

    /// Non-negative, finite, and each axis pair at most 40% of `viewport`
    /// (a bogus OEM report can never squeeze the HUD into nothing).
    pub fn sanitized(self, viewport: Vec2) -> Self {
        let side = |v: f32, span: f32| if v.is_finite() { v.clamp(0.0, (span * 0.4).max(0.0)) } else { 0.0 };
        let (w, h) = (if viewport.x.is_finite() { viewport.x } else { 0.0 }, if viewport.y.is_finite() { viewport.y } else { 0.0 });
        let mut s = SafeAreaInsets { left: side(self.left, w), top: side(self.top, h), right: side(self.right, w), bottom: side(self.bottom, h) };
        // Each pair together: at most 40% of the span as well.
        let fit = |a: &mut f32, b: &mut f32, span: f32| {
            let max = span * 0.4;
            if *a + *b > max && *a + *b > 0.0 {
                let k = max / (*a + *b);
                *a *= k;
                *b *= k;
            }
        };
        fit(&mut s.left, &mut s.right, w);
        fit(&mut s.top, &mut s.bottom, h);
        s
    }

    /// Physical -> logical px (`scale` = the window scale factor).
    pub fn to_logical(self, scale: f32) -> Self {
        let k = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
        SafeAreaInsets { left: self.left * k, top: self.top * k, right: self.right * k, bottom: self.bottom * k }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// One on-screen touch control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchButton {
    Fire,
    Jump,
    Dash,
    Punch,
    /// Next weapon.
    Swap,
    Ping,
    /// 5G Overload.
    Overload,
    /// Camera Flash Strobe.
    Flash,
    /// SOS uplink (only near the tower).
    Sos,
    Pause,
    /// Time Trial instant reset (Time Trial only).
    Reset,
    /// Latches auto-bhop on while moving.
    AutoBhop,
    /// A weapon-deck slot (0-based).
    Slot(u8),
}

impl TouchButton {
    /// Every drawn control (deck slots are the HUD's own weapon tabs).
    pub const HUD: [TouchButton; 12] = [
        TouchButton::Fire,
        TouchButton::Jump,
        TouchButton::Dash,
        TouchButton::Punch,
        TouchButton::Swap,
        TouchButton::Ping,
        TouchButton::Overload,
        TouchButton::Flash,
        TouchButton::Sos,
        TouchButton::Pause,
        TouchButton::Reset,
        TouchButton::AutoBhop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TouchButton::Fire => "FIRE",
            TouchButton::Jump => "JUMP",
            TouchButton::Dash => "DASH",
            TouchButton::Punch => "PUNCH",
            TouchButton::Swap => "SWAP",
            TouchButton::Ping => "PING",
            TouchButton::Overload => "5G",
            TouchButton::Flash => "FLASH",
            TouchButton::Sos => "SOS",
            TouchButton::Pause => "||",
            TouchButton::Reset => "RST",
            TouchButton::AutoBhop => "AUTO-BHOP",
            TouchButton::Slot(_) => "",
        }
    }

    /// Bit in [`TouchFrame::pressed`].
    pub fn bit(self) -> u32 {
        let i = match self {
            TouchButton::Slot(i) => 12 + i.min(7) as u32,
            b => TouchButton::HUD.iter().position(|h| *h == b).unwrap_or(0) as u32,
        };
        1 << i
    }

    /// Whether this button's finger also aims (drag-aim FIRE).
    pub fn drag_looks(self) -> bool {
        self == TouchButton::Fire
    }
}

/// A button's square hit plate (logical px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonRect {
    pub button: TouchButton,
    pub center: Vec2,
    pub half: Vec2,
}

impl ButtonRect {
    pub fn rect(&self) -> Rect {
        Rect::from_center_half_size(self.center, self.half)
    }

    /// `p` inside the plate grown by `slop` (x) / `slop_y` (y).
    pub fn contains(&self, p: Vec2, slop: Vec2) -> bool {
        let d = (p - self.center).abs();
        d.x <= self.half.x + slop.x && d.y <= self.half.y + slop.y
    }
}

/// Game context that changes which controls exist.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LayoutContext {
    /// Time Trial run: the RST button shows.
    pub trial: bool,
    /// At the tower with the uplink idle: the SOS button shows.
    pub sos: bool,
    /// `touch_auto_bhop` on: the AUTO-BHOP latch pill shows.
    pub auto_bhop: bool,
    /// The weapon deck's slot tabs (logical px), published by the HUD.
    pub slots: [Option<Rect>; 3],
}

/// Where every touch control sits this frame (logical px).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TouchLayout {
    pub viewport: Vec2,
    /// The viewport minus insets and margin.
    pub safe: Rect,
    /// Logical px per design px (phone height reference x button scale).
    pub unit: f32,
    pub stick_radius: f32,
    /// Where the stick rests (drawn dimmed) while no finger holds it.
    pub stick_rest: Vec2,
    pub buttons: Vec<ButtonRect>,
}

impl TouchLayout {
    /// Lay the controls out for a `viewport` (logical px) with logical
    /// `insets`. Right thumb: FIRE (large) in the bottom-right, JUMP to its
    /// left, DASH / PUNCH / SWAP above, the PING / 5G / FLASH phone-app row
    /// above that and the contextual SOS beside it; PAUSE (+ RST) in the
    /// top-right corner; the AUTO-BHOP pill above the stick's rest spot.
    pub fn compute(viewport: Vec2, insets: SafeAreaInsets, button_scale: f32, ctx: &LayoutContext) -> Self {
        let viewport = if viewport.is_finite() && viewport.x > 1.0 && viewport.y > 1.0 { viewport } else { Vec2::new(1280.0, 720.0) };
        let insets = insets.sanitized(viewport);
        let scale = if button_scale.is_finite() { button_scale.clamp(TOUCH_BUTTON_SCALE_RANGE.0, TOUCH_BUTTON_SCALE_RANGE.1) } else { 1.0 };
        let base = (viewport.y / LAYOUT_REFERENCE_H).clamp(0.8, 1.4);
        let u = base * scale;
        let m = LAYOUT_MARGIN * base;
        let safe = Rect::new(insets.left + m, insets.top + m, viewport.x - insets.right - m, viewport.y - insets.bottom - m);
        let br = safe.max;
        let tr = Vec2::new(safe.max.x, safe.min.y);
        let mut buttons = Vec::with_capacity(16);
        let mut put = |button: TouchButton, center: Vec2, half: f32| buttons.push(ButtonRect { button, center, half: Vec2::splat(half * u) });
        // Right-thumb arc (design px, relative to the bottom-right corner).
        let at = |x: f32, y: f32| br + Vec2::new(x, y) * u;
        put(TouchButton::Fire, at(-46.0, -50.0), 40.0);
        put(TouchButton::Jump, at(-129.0, -40.0), 31.0);
        put(TouchButton::Dash, at(-140.0, -107.0), 24.0);
        put(TouchButton::Punch, at(-74.0, -126.0), 24.0);
        put(TouchButton::Swap, at(-22.0, -120.0), 18.0);
        // Phone-app row.
        put(TouchButton::Ping, at(-126.0, -184.0), 20.0);
        put(TouchButton::Overload, at(-78.0, -184.0), 20.0);
        put(TouchButton::Flash, at(-30.0, -184.0), 20.0);
        if ctx.sos {
            // Left of the app row, never closer than 48 px to the reticle.
            let mut c = at(-188.0, -184.0);
            c.x = c.x.max(viewport.x * 0.5 + 48.0 + 28.0 * u);
            put(TouchButton::Sos, c, 28.0);
        }
        // Top-right system corner.
        put(TouchButton::Pause, tr + Vec2::new(-18.0, 18.0) * u, 18.0);
        if ctx.trial {
            put(TouchButton::Reset, tr + Vec2::new(-66.0, 18.0) * u, 18.0);
        }
        let stick_radius = STICK_RADIUS * u;
        let stick_rest = Vec2::new(safe.min.x + stick_radius + 8.0 * u, safe.max.y - stick_radius - 4.0 * u);
        if ctx.auto_bhop {
            let c = Vec2::new(stick_rest.x, stick_rest.y - stick_radius - 26.0 * u);
            buttons.push(ButtonRect { button: TouchButton::AutoBhop, center: c, half: Vec2::new(40.0, 12.0) * u });
        }
        for (i, r) in ctx.slots.iter().enumerate() {
            if let Some(r) = r.filter(|r| r.width() > 0.0 && r.height() > 0.0 && r.min.is_finite() && r.max.is_finite()) {
                buttons.push(ButtonRect { button: TouchButton::Slot(i as u8), center: r.center(), half: r.half_size() });
            }
        }
        TouchLayout { viewport, safe, unit: u, stick_radius, stick_rest, buttons }
    }

    pub fn button(&self, b: TouchButton) -> Option<&ButtonRect> {
        self.buttons.iter().find(|r| r.button == b)
    }

    /// The control under `p` (the nearest center when grown plates overlap).
    pub fn hit(&self, p: Vec2) -> Option<TouchButton> {
        self.buttons
            .iter()
            .filter(|r| {
                let slop = if matches!(r.button, TouchButton::Slot(_)) { Vec2::new(2.0, SLOT_SLOP) } else { Vec2::splat(BUTTON_SLOP) };
                r.contains(p, slop)
            })
            .min_by(|a, b| a.center.distance_squared(p).total_cmp(&b.center.distance_squared(p)))
            .map(|r| r.button)
    }

    /// Left half: a touch here (not on a button) anchors the floating stick.
    pub fn in_stick_zone(&self, p: Vec2) -> bool {
        p.x < self.viewport.x * 0.5
    }

    /// Keep the stick base fully inside the safe area and the left half.
    pub fn clamp_stick_origin(&self, p: Vec2) -> Vec2 {
        let r = self.stick_radius;
        let max_x = (self.viewport.x * 0.5 - r).max(self.safe.min.x + r);
        let max_y = (self.safe.max.y - r).max(self.safe.min.y + r);
        Vec2::new(p.x.clamp(self.safe.min.x + r, max_x), p.y.clamp(self.safe.min.y + r, max_y))
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// What a finger did this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerPhase {
    /// Touched down this frame.
    Pressed,
    /// Still down.
    Held,
    /// Lifted (or canceled) this frame.
    Released,
    /// Down and up within one frame (a fast tap).
    Tapped,
}

/// One finger this frame (logical px, `y` down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FingerSample {
    pub id: u64,
    pub pos: Vec2,
    pub phase: FingerPhase,
}

/// A finger's role, fixed at touch-down.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Stick,
    Look,
    Button(TouchButton),
    /// A second finger in the stick zone while the stick is taken.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Finger {
    id: u64,
    role: Role,
    last: Vec2,
}

/// Router tuning from the player's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouterConfig {
    pub sensitivity: f32,
    pub auto_bhop: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        RouterConfig { sensitivity: 1.0, auto_bhop: true }
    }
}

/// The touch controls' output for one frame. Held fields stay set while held;
/// one-shots fire on the press edge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TouchFrame {
    /// Stick movement: `x` = right, `y` = forward (deadzoned, axis-snapped).
    pub move_axes: Vec2,
    /// Stick deflection before the deadzone (length `0..=1`).
    pub stick_raw: Vec2,
    /// Look this frame (radians): `x` = yaw right, `y` = pitch up.
    pub look_delta: Vec2,
    /// Turn speed as a pseudo stick magnitude `[0, 1]` (aim friction fades with it).
    pub look_mag: f32,
    pub fire: bool,
    pub fire_just_pressed: bool,
    pub jump: bool,
    pub jump_pressed: bool,
    pub dash: bool,
    pub punch: bool,
    pub swap: bool,
    pub ping: bool,
    pub overload: bool,
    pub flash: bool,
    pub sos: bool,
    pub pause: bool,
    pub reset: bool,
    pub weapon_slot: Option<usize>,
    /// The AUTO-BHOP pill is latched on.
    pub auto_bhop_latched: bool,
    /// The live stick: `(base center, puck center)` in logical px.
    pub stick: Option<(Vec2, Vec2)>,
    /// [`TouchButton::bit`]s of every button held down.
    pub pressed: u32,
    /// Fingers bound by the router.
    pub fingers: usize,
}

impl TouchFrame {
    pub fn is_pressed(&self, b: TouchButton) -> bool {
        self.pressed & b.bit() != 0
    }

    /// Anything for [`GamepadInput`] this frame.
    pub fn any_action(&self) -> bool {
        self.move_axes != Vec2::ZERO
            || self.look_delta != Vec2::ZERO
            || self.fire
            || self.fire_just_pressed
            || self.jump
            || self.jump_pressed
            || self.dash
            || self.punch
            || self.swap
            || self.ping
            || self.overload
            || self.flash
            || self.sos
            || self.pause
            || self.reset
            || self.weapon_slot.is_some()
    }
}

/// Raw stick vector of a finger at `pos` on a base at `origin` (`y` up,
/// length clamped to 1).
pub fn stick_vector(origin: Vec2, pos: Vec2, radius: f32) -> Vec2 {
    let r = radius.max(1.0);
    let v = Vec2::new(pos.x - origin.x, origin.y - pos.y) / r;
    if !v.is_finite() {
        return Vec2::ZERO;
    }
    v.clamp_length_max(1.0)
}

/// Touch stick -> movement axes: [`TOUCH_STICK_DEADZONE`] radial deadzone,
/// then the same minor-axis snap as the Left Stick, so a near-pure sideways
/// drag is a pure CPMA air-strafe and a near-pure forward one W-only air control.
pub fn touch_move_axes(stick: Vec2) -> Vec2 {
    snap_move_axes(apply_radial_deadzone(stick, TOUCH_STICK_DEADZONE))
}

/// Pull the stick base toward a finger dragged past [`STICK_FOLLOW_START`]
/// radii (rate [`STICK_FOLLOW_RATE`]).
pub fn follow_stick_origin(origin: Vec2, pos: Vec2, radius: f32, dt: f32) -> Vec2 {
    let d = pos - origin;
    let len = d.length();
    let start = radius * STICK_FOLLOW_START;
    if !len.is_finite() || len <= start || !dt.is_finite() || dt <= 0.0 {
        return origin;
    }
    let k = (STICK_FOLLOW_RATE * dt).min(1.0);
    origin + d / len * (len - start) * k
}

/// A finger drag of `px` (logical px, `y` down) as look radians (`x` = yaw
/// right, `y` = pitch up): normalised by the viewport height, spike-clamped
/// to [`TOUCH_LOOK_SPIKE`] heights, scaled by [`TOUCH_LOOK_GAIN`] x
/// sensitivity. Never NaN.
pub fn touch_look_delta(px: Vec2, viewport_h: f32, sensitivity: f32) -> Vec2 {
    if !px.is_finite() || !viewport_h.is_finite() || viewport_h <= 1.0 {
        return Vec2::ZERO;
    }
    let sens = if sensitivity.is_finite() { sensitivity.clamp(TOUCH_SENS_RANGE.0, TOUCH_SENS_RANGE.1) } else { 1.0 };
    let screens = (px / viewport_h).clamp_length_max(TOUCH_LOOK_SPIKE);
    Vec2::new(screens.x, -screens.y) * TOUCH_LOOK_GAIN * sens
}

/// Turn speed as a pseudo stick magnitude for `player::aim_friction_scale`:
/// slow tracking reads low (full friction), a flick reads 1 (none).
pub fn touch_look_mag(look: Vec2, dt: f32) -> f32 {
    if !look.is_finite() || !dt.is_finite() || dt <= 0.0 || look == Vec2::ZERO {
        return 0.0;
    }
    (look.length() / dt / TOUCH_FLICK_SPEED).clamp(0.0, 1.0)
}

/// The deterministic multi-touch router (see the module docs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TouchRouter {
    fingers: Vec<Finger>,
    stick_origin: Option<Vec2>,
    stick_pos: Vec2,
    bhop_latch: bool,
    prev_jump: bool,
}

impl TouchRouter {
    /// Drop every finger (leaving `Playing`, focus loss): nothing stays held.
    pub fn reset(&mut self) {
        let latch = self.bhop_latch;
        *self = TouchRouter { bhop_latch: latch, ..default() };
    }

    /// Fingers currently bound.
    pub fn finger_count(&self) -> usize {
        self.fingers.len()
    }

    /// Route one frame of finger samples. A bound finger missing from
    /// `samples` counts as lifted.
    pub fn step(&mut self, samples: &[FingerSample], layout: &TouchLayout, cfg: RouterConfig, dt: f32) -> TouchFrame {
        let mut out = TouchFrame::default();
        let mut look_px = Vec2::ZERO;
        let mut taps: u32 = 0;
        let spike = TOUCH_LOOK_SPIKE * layout.viewport.y.max(1.0);

        // 1. Touch-downs bind a role.
        for s in samples.iter().filter(|s| matches!(s.phase, FingerPhase::Pressed | FingerPhase::Tapped)) {
            if self.fingers.iter().any(|f| f.id == s.id) || !s.pos.is_finite() {
                continue;
            }
            let role = match layout.hit(s.pos) {
                Some(b) => Role::Button(b),
                None if layout.in_stick_zone(s.pos) && self.stick_origin.is_none() => Role::Stick,
                None if layout.in_stick_zone(s.pos) => Role::Ignored,
                None => Role::Look,
            };
            match role {
                Role::Button(b) => {
                    taps |= b.bit();
                    self.press(b, &mut out);
                }
                Role::Stick if s.phase == FingerPhase::Pressed => {
                    self.stick_origin = Some(layout.clamp_stick_origin(s.pos));
                    self.stick_pos = s.pos;
                }
                _ => {}
            }
            if s.phase == FingerPhase::Pressed {
                self.fingers.push(Finger { id: s.id, role, last: s.pos });
            }
        }

        // 2. Moves (and the final position of lifted fingers).
        let mut lifted = Vec::new();
        for f in &mut self.fingers {
            let sample = samples.iter().find(|s| s.id == f.id && matches!(s.phase, FingerPhase::Held | FingerPhase::Released | FingerPhase::Pressed));
            let Some(s) = sample else {
                lifted.push(f.id);
                continue;
            };
            if s.pos.is_finite() {
                let delta = (s.pos - f.last).clamp_length_max(spike);
                f.last = s.pos;
                match f.role {
                    Role::Look => look_px += delta,
                    Role::Button(b) if b.drag_looks() => look_px += delta,
                    Role::Stick => self.stick_pos = s.pos,
                    _ => {}
                }
            }
            if s.phase == FingerPhase::Released {
                lifted.push(f.id);
            }
        }

        // 3. Lifts free the role.
        for id in lifted {
            if let Some(i) = self.fingers.iter().position(|f| f.id == id) {
                if self.fingers[i].role == Role::Stick {
                    self.stick_origin = None;
                }
                self.fingers.remove(i);
            }
        }

        // 4. Stick.
        if let Some(origin) = self.stick_origin {
            let origin = layout.clamp_stick_origin(follow_stick_origin(origin, self.stick_pos, layout.stick_radius, dt));
            self.stick_origin = Some(origin);
            let v = stick_vector(origin, self.stick_pos, layout.stick_radius);
            out.stick_raw = v;
            out.move_axes = touch_move_axes(v);
            let puck = origin + Vec2::new(v.x, -v.y) * layout.stick_radius;
            out.stick = Some((origin, puck));
        }

        // 5. Held buttons.
        for f in &self.fingers {
            if let Role::Button(b) = f.role {
                out.pressed |= b.bit();
            }
        }
        // Held, or tapped within this one frame.
        out.fire = (out.pressed | taps) & TouchButton::Fire.bit() != 0;
        let jump_button = out.pressed & TouchButton::Jump.bit() != 0;

        // 6. Auto-bhop: the stick rim (or the latch while moving) holds jump.
        if !cfg.auto_bhop {
            self.bhop_latch = false;
        }
        out.auto_bhop_latched = self.bhop_latch;
        let rim = cfg.auto_bhop && self.stick_origin.is_some() && out.stick_raw.length() >= AUTO_BHOP_RIM;
        let latched = cfg.auto_bhop && self.bhop_latch && out.move_axes != Vec2::ZERO;
        out.jump = jump_button || rim || latched;
        out.jump_pressed |= out.jump && !self.prev_jump;
        self.prev_jump = out.jump;

        // 7. Look.
        out.look_delta = touch_look_delta(look_px, layout.viewport.y, cfg.sensitivity);
        out.look_mag = touch_look_mag(out.look_delta, dt);
        out.fingers = self.fingers.len();
        out
    }

    /// Press-edge effects of a button.
    fn press(&mut self, b: TouchButton, out: &mut TouchFrame) {
        match b {
            TouchButton::Fire => out.fire_just_pressed = true,
            TouchButton::Jump => out.jump_pressed = true,
            TouchButton::Dash => out.dash = true,
            TouchButton::Punch => out.punch = true,
            TouchButton::Swap => out.swap = true,
            TouchButton::Ping => out.ping = true,
            TouchButton::Overload => out.overload = true,
            TouchButton::Flash => out.flash = true,
            TouchButton::Sos => out.sos = true,
            TouchButton::Pause => out.pause = true,
            TouchButton::Reset => out.reset = true,
            TouchButton::AutoBhop => self.bhop_latch = !self.bhop_latch,
            TouchButton::Slot(i) => out.weapon_slot = Some(i as usize),
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-fire
// ---------------------------------------------------------------------------

/// Whether any `(center, radius)` target sits inside [`AUTO_FIRE_CONE_RAD`]
/// of the view ray, within `range` and with a clear line of sight.
pub fn auto_fire_target(grid: Option<&LevelGrid>, eye: Vec3, fwd: Vec3, range: f32, mut targets: impl Iterator<Item = (Vec3, f32)>) -> bool {
    if !eye.is_finite() || !fwd.is_finite() || fwd == Vec3::ZERO {
        return false;
    }
    targets.any(|(c, r)| {
        c.is_finite()
            && c.distance_squared(eye) <= range * range
            && silhouette_angle(eye, fwd, c, r) <= AUTO_FIRE_CONE_RAD
            && grid.is_none_or(|g| crate::physics::has_line_of_sight(g, eye, c))
    })
}

/// Auto-fire may squeeze the trigger: alive, a loaded gun (never empty-gun
/// dry-click spam) and not mid-swap.
pub fn auto_fire_allowed(alive: bool, has_ammo: bool, swap_timer: f32) -> bool {
    alive && has_ammo && swap_timer <= 0.0
}

// ---------------------------------------------------------------------------
// Input mode
// ---------------------------------------------------------------------------

/// The input mode after this frame's activity. `touch` = a real finger event,
/// `pad_active` = [`GamepadInput::active`] (cleared on every touch, so it
/// means "the pad moved since"), `real_kbm` = a key press or mouse input
/// outside the synthetic-mouse guard (see [`kbm_is_real`]).
///
/// - `Off`: the pre-touch behaviour (controller vs mouse & keyboard).
/// - `On`: always [`InputMode::Touch`].
/// - `Auto`: a touch switches to Touch; from Touch only a controller or a
///   real key / mouse input switches away.
pub fn resolve_input_mode(current: InputMode, touch_mode: TouchMode, touch: bool, pad_active: bool, real_kbm: bool) -> InputMode {
    let legacy = if pad_active { InputMode::Gamepad } else { InputMode::KeyboardMouse };
    match touch_mode {
        TouchMode::Off => legacy,
        TouchMode::On => InputMode::Touch,
        TouchMode::Auto if touch => InputMode::Touch,
        TouchMode::Auto if current == InputMode::Touch => {
            if pad_active {
                InputMode::Gamepad
            } else if real_kbm {
                InputMode::KeyboardMouse
            } else {
                InputMode::Touch
            }
        }
        TouchMode::Auto => legacy,
    }
}

/// Mouse input counts as a real mouse only with no finger down and at least
/// [`TOUCH_MOUSE_GUARD_SECS`] since the last touch (browsers replay taps as
/// compatibility mouse events). Keys always count.
pub fn kbm_is_real(key: bool, mouse: bool, fingers_down: usize, since_touch: f32) -> bool {
    key || (mouse && fingers_down == 0 && since_touch >= TOUCH_MOUSE_GUARD_SECS)
}

/// Whether the touch HUD draws: a run, touch not switched off, and touch mode
/// (or forced on).
pub fn overlay_visible(state: AppState, touch_mode: TouchMode, mode: InputMode) -> bool {
    state == AppState::Playing && match touch_mode {
        TouchMode::Off => false,
        TouchMode::On => true,
        TouchMode::Auto => mode == InputMode::Touch,
    }
}

/// Whether the router runs: a run with touch allowed, and either touch mode
/// already or a touch arriving right now (the first touch acts at once).
pub fn router_active(state: AppState, touch_mode: TouchMode, mode: InputMode, touched: bool) -> bool {
    state == AppState::Playing && touch_mode != TouchMode::Off && (touch_mode == TouchMode::On || mode == InputMode::Touch || touched)
}

/// Whether the cursor / browser pointer lock / Android pointer capture may be
/// taken (never in touch mode).
pub fn wants_cursor_capture(mode: InputMode, touch_mode: TouchMode) -> bool {
    !(mode == InputMode::Touch || touch_mode == TouchMode::On)
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Device activity this frame (written before `sync_input_mode`).
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct TouchActivity {
    /// A real finger is down, landed or lifted this frame.
    pub touched: bool,
    /// Fingers down.
    pub fingers: usize,
    /// Seconds since the last touch (infinite before the first).
    pub since_touch: f32,
    /// A key press, or mouse input outside the synthetic-mouse guard.
    pub real_kbm: bool,
}

impl Default for TouchActivity {
    fn default() -> Self {
        TouchActivity { touched: false, fingers: 0, since_touch: f32::INFINITY, real_kbm: false }
    }
}

/// The touch controls' live state.
#[derive(Resource, Debug, Clone)]
pub struct TouchInput {
    pub router: TouchRouter,
    pub frame: TouchFrame,
    pub layout: TouchLayout,
    /// The router ran this frame.
    pub active: bool,
    /// Auto-fire found a target this frame.
    pub auto_fire: bool,
    /// Weapon-deck slot tabs (logical px), published by the HUD.
    pub slot_rects: [Option<Rect>; 3],
    /// Viewport used without a window (headless tests): logical px.
    pub fallback_viewport: Vec2,
    /// [`merge_touch_into_pad`] wrote into [`GamepadInput`] last frame.
    pad_owned: bool,
}

impl Default for TouchInput {
    fn default() -> Self {
        TouchInput {
            router: TouchRouter::default(),
            frame: TouchFrame::default(),
            layout: TouchLayout::default(),
            active: false,
            auto_fire: false,
            slot_rects: [None; 3],
            fallback_viewport: Vec2::new(1280.0, 720.0),
            pad_owned: false,
        }
    }
}

/// One menu pointer: the mouse, or a finger.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct UiPointer {
    /// Physical px (matches `ComputedNode::contains_point`).
    pub position: Option<Vec2>,
    /// Logical px.
    pub logical: Option<Vec2>,
    /// Mouse button / finger down.
    pub pressed: bool,
    pub just_pressed: bool,
    /// Activation: mouse released, or an armed finger lifted.
    pub released: bool,
    /// A finger drives it this frame.
    pub touch: bool,
    /// Extra hit margin around targets (physical px; 0 for the mouse).
    pub slop: f32,
}

impl UiPointer {
    /// Whether the pointer is over `node` (a finger also within [`Self::slop`]
    /// of its edges).
    pub fn over(&self, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform) -> bool {
        let Some(p) = self.position else { return false };
        node.contains_point(*tf, p) || (self.slop > 0.0 && within_slop(p, tf.translation, node.size(), self.slop))
    }
}

/// `p` inside the `size` box centered at `center`, grown by `slop` (all px).
pub fn within_slop(p: Vec2, center: Vec2, size: Vec2, slop: f32) -> bool {
    size.x > 0.0 && size.y > 0.0 && (p - center).abs().cmple(size * 0.5 + Vec2::splat(slop.max(0.0))).all()
}

/// The mouse half of a [`resolve_ui_pointer`] input.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MousePointer {
    /// Physical px.
    pub position: Option<Vec2>,
    pub pressed: bool,
    pub just_pressed: bool,
    pub just_released: bool,
}

/// The finger half of a [`resolve_ui_pointer`] input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FingerPointer {
    /// Logical px (Bevy's `Touch::position`).
    pub position: Vec2,
    pub phase: FingerPhase,
    /// It touched down on this screen (a finger carried over from gameplay
    /// or a previous screen never activates anything when it lifts).
    pub armed: bool,
}

/// One frame of the menu pointer. A finger wins over the mouse; within the
/// synthetic-mouse `guard` the mouse is ignored entirely (no hover, no click),
/// so one tap never fires twice. Touch positions are logical px: they are
/// multiplied by the window `scale` to match the physical cursor space.
pub fn resolve_ui_pointer(mouse: MousePointer, finger: Option<FingerPointer>, guard: bool, scale: f32) -> UiPointer {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    if let Some(f) = finger {
        let down = matches!(f.phase, FingerPhase::Pressed | FingerPhase::Held);
        return UiPointer {
            position: Some(f.position * scale),
            logical: Some(f.position),
            pressed: down,
            just_pressed: matches!(f.phase, FingerPhase::Pressed | FingerPhase::Tapped),
            released: f.armed && matches!(f.phase, FingerPhase::Released | FingerPhase::Tapped),
            touch: true,
            slop: TOUCH_UI_SLOP * scale,
        };
    }
    if guard {
        return UiPointer::default();
    }
    UiPointer {
        position: mouse.position,
        logical: mouse.position.map(|p| p / scale),
        pressed: mouse.pressed,
        just_pressed: mouse.just_pressed,
        released: mouse.just_released,
        touch: false,
        slop: 0.0,
    }
}

/// Android system Back (gesture / nav bar): replayed as a one-frame `Escape`.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackRequest {
    /// A Back arrived; consumed on the next `PreUpdate`.
    pub pending: bool,
    /// The synthetic `Escape` is held (released next frame).
    injected: bool,
}

impl BackRequest {
    pub fn request(&mut self) {
        self.pending = true;
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Every finger in `touches`, sorted by id (deterministic routing).
pub fn finger_samples(touches: &Touches, out: &mut Vec<FingerSample>) {
    out.clear();
    for t in touches.iter() {
        let phase = if touches.just_pressed(t.id()) { FingerPhase::Pressed } else { FingerPhase::Held };
        out.push(FingerSample { id: t.id(), pos: t.position(), phase });
    }
    for t in touches.iter_just_released().chain(touches.iter_just_canceled()) {
        if out.iter().any(|s| s.id == t.id()) {
            continue;
        }
        let phase = if touches.just_pressed(t.id()) { FingerPhase::Tapped } else { FingerPhase::Released };
        out.push(FingerSample { id: t.id(), pos: t.position(), phase });
    }
    out.sort_by_key(|s| s.id);
}

/// Window logical size + scale factor, or the headless fallback.
fn viewport_of(window: Option<&Window>, fallback: Vec2) -> (Vec2, f32) {
    match window {
        Some(w) if w.width() > 1.0 && w.height() > 1.0 => (Vec2::new(w.width(), w.height()), w.scale_factor()),
        _ => (fallback, 1.0),
    }
}

/// Track device activity, route fingers and lay the controls out.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_touch_input(
    time: Res<Time>,
    (touches, mouse, keys, motion, scroll): (
        Option<Res<Touches>>,
        Option<Res<ButtonInput<MouseButton>>>,
        Option<Res<ButtonInput<KeyCode>>>,
        Option<Res<AccumulatedMouseMotion>>,
        Option<Res<AccumulatedMouseScroll>>,
    ),
    windows: Query<&Window, With<PrimaryWindow>>,
    state: Option<Res<State<AppState>>>,
    store: Option<Res<ProfileStore>>,
    mode: Option<Res<InputMode>>,
    insets: Option<Res<SafeAreaInsets>>,
    (run, signal, ext, captured): (Option<Res<RunStats>>, Option<Res<SignalState>>, Option<Res<ExtractionState>>, Option<Res<CursorCaptured>>),
    mut input: ResMut<TouchInput>,
    mut activity: ResMut<TouchActivity>,
    mut samples: Local<Vec<FingerSample>>,
) {
    let dt = time.delta_secs();
    match touches.as_deref() {
        Some(t) => finger_samples(t, &mut samples),
        None => samples.clear(),
    }
    let fingers = touches.as_deref().map_or(0, |t| t.iter().count());
    let touched = !samples.is_empty();
    activity.touched = touched;
    activity.fingers = fingers;
    activity.since_touch = if touched { 0.0 } else { activity.since_touch + dt.max(0.0) };
    let key = keys.as_deref().is_some_and(|k| k.get_just_pressed().next().is_some());
    let mouse_moved = mouse.as_deref().is_some_and(|m| m.get_just_pressed().next().is_some())
        || motion.as_deref().is_some_and(|m| m.delta != Vec2::ZERO)
        || scroll.as_deref().is_some_and(|s| s.delta != Vec2::ZERO);
    activity.real_kbm = kbm_is_real(key, mouse_moved, fingers, activity.since_touch);

    let settings = live_settings(store.as_deref());
    let state = state.map_or(AppState::MainMenu, |s| *s.get());
    let mode = mode.map_or(InputMode::KeyboardMouse, |m| *m);
    let window = windows.single().ok();
    let (viewport, scale) = viewport_of(window, input.fallback_viewport);
    let ctx = LayoutContext {
        trial: run.as_deref().is_some_and(|r| r.mode == RunMode::TimeTrial),
        sos: signal.as_deref().is_some_and(|s| s.near_tower) && ext.as_deref().is_some_and(|e| matches!(e, ExtractionState::Searching { .. })),
        auto_bhop: settings.touch_auto_bhop,
        slots: input.slot_rects,
    };
    let logical_insets = insets.as_deref().copied().unwrap_or_default().to_logical(scale);
    input.layout = TouchLayout::compute(viewport, logical_insets, settings.touch_button_scale, &ctx);

    // `TouchMode::On` desktop preview: the left mouse button acts as a finger
    // whenever the cursor is free (it never is captured in touch mode).
    if settings.touch_mode == TouchMode::On
        && !captured.as_deref().is_some_and(|c| c.0)
        && let (Some(m), Some(pos)) = (mouse.as_deref(), window.and_then(|w| w.cursor_position()))
    {
        let phase = match (m.just_pressed(MouseButton::Left), m.pressed(MouseButton::Left), m.just_released(MouseButton::Left)) {
            (true, false, _) => Some(FingerPhase::Tapped),
            (true, true, _) => Some(FingerPhase::Pressed),
            (false, true, _) => Some(FingerPhase::Held),
            (false, false, true) => Some(FingerPhase::Released),
            _ => None,
        };
        if let Some(phase) = phase {
            samples.push(FingerSample { id: MOUSE_FINGER_ID, pos, phase });
        }
    }

    input.active = router_active(state, settings.touch_mode, mode, touched);
    if input.active {
        let cfg = RouterConfig { sensitivity: settings.touch_sensitivity, auto_bhop: settings.touch_auto_bhop };
        let layout = input.layout.clone();
        input.frame = input.router.step(&samples, &layout, cfg, dt);
    } else {
        input.router.reset();
        input.frame = TouchFrame { auto_bhop_latched: input.frame.auto_bhop_latched && settings.touch_auto_bhop, ..default() };
    }
}

type AutoFireEnemies<'w, 's> = Query<'w, 's, (&'static Transform, &'static Enemy), (Without<Player>, Without<PlayerCamera>)>;
type AutoFireJammers<'w, 's> = Query<'w, 's, (&'static Transform, &'static DemonJammer), (Without<Player>, Without<PlayerCamera>, Without<Enemy>)>;

/// Optional touch auto-fire (`touch_auto_fire`): a live demon / jammer / boss
/// inside the view cone, in weapon range, in line of sight.
#[allow(clippy::too_many_arguments)]
pub fn update_touch_auto_fire(
    mut input: ResMut<TouchInput>,
    store: Option<Res<ProfileStore>>,
    ws: Option<Res<WeaponState>>,
    grid: Option<Res<LevelGrid>>,
    players: Query<(&Player, &Transform), Without<PlayerCamera>>,
    cams: Query<&Transform, With<PlayerCamera>>,
    (enemies, jammers): (AutoFireEnemies, AutoFireJammers),
) {
    input.auto_fire = false;
    if !input.active || !live_settings(store.as_deref()).touch_auto_fire {
        return;
    }
    let (Ok((p, body)), Some(ws)) = (players.single(), ws) else { return };
    if !auto_fire_allowed(!p.is_dead(), ws.current.has_ammo(p), ws.swap_timer) {
        return;
    }
    let eye_tf = live_eye_pose(body, &cams.single().copied().unwrap_or_default());
    let (eye, fwd) = (eye_tf.translation, eye_tf.rotation * Vec3::NEG_Z);
    let demons = enemies
        .iter()
        .filter(|(_, e)| e.is_alive())
        .flat_map(|(t, e)| e.stats.hit_spheres.iter().map(move |&(h, r)| (t.translation + Vec3::Y * h, r)));
    let towers = jammers.iter().filter(|(_, j)| j.hp > 0.0).flat_map(|(t, _)| JAMMER_SPHERES.iter().map(move |&(h, r)| (t.translation + Vec3::Y * h, r)));
    input.auto_fire = auto_fire_target(grid.as_deref(), eye, fwd, auto_fire_range(ws.current), demons.chain(towers));
}

/// Fold the touch controls into [`GamepadInput`] (see the module docs). With
/// no controller connected the touch fields are owned here: last frame's
/// contribution is cleared first, so nothing sticks after a finger lifts.
pub fn merge_touch_into_pad(mut input: ResMut<TouchInput>, mut pad: ResMut<GamepadInput>) {
    if !pad.connected && input.pad_owned {
        clear_touch_fields(&mut pad);
    }
    let t = &input.frame;
    let auto = input.auto_fire;
    let contributes = input.active && (t.any_action() || auto);
    if contributes {
        let p = &mut *pad;
        p.move_axes = (p.move_axes + t.move_axes).clamp(Vec2::splat(-1.0), Vec2::splat(1.0));
        p.look_delta += t.look_delta;
        p.look_mag = p.look_mag.max(t.look_mag);
        p.fire |= t.fire || auto;
        p.fire_just_pressed |= t.fire_just_pressed;
        p.jump |= t.jump;
        p.jump_pressed |= t.jump_pressed;
        p.dash |= t.dash;
        p.punch |= t.punch;
        p.weapon_next |= t.swap;
        p.ping |= t.ping;
        p.overload |= t.overload;
        p.flash |= t.flash;
        p.interact |= t.sos;
        p.pause |= t.pause;
        p.trial_reset |= t.reset;
        if t.weapon_slot.is_some() {
            p.weapon_slot = t.weapon_slot;
        }
    }
    input.pad_owned = contributes;
}

fn clear_touch_fields(p: &mut GamepadInput) {
    p.move_axes = Vec2::ZERO;
    p.look_delta = Vec2::ZERO;
    p.look_mag = 0.0;
    p.fire = false;
    p.fire_just_pressed = false;
    p.jump = false;
    p.jump_pressed = false;
    p.dash = false;
    p.punch = false;
    p.weapon_next = false;
    p.ping = false;
    p.overload = false;
    p.flash = false;
    p.interact = false;
    p.pause = false;
    p.trial_reset = false;
    p.weapon_slot = None;
}

/// Build the [`UiPointer`]: the mouse, or the menu finger. Fingers are armed
/// only when they touch down on the current screen (state).
#[allow(clippy::type_complexity)]
pub fn update_ui_pointer(
    (touches, mouse): (Option<Res<Touches>>, Option<Res<ButtonInput<MouseButton>>>),
    windows: Query<&Window, With<PrimaryWindow>>,
    state: Option<Res<State<AppState>>>,
    activity: Res<TouchActivity>,
    mut ptr: ResMut<UiPointer>,
    mut armed: Local<(Option<AppState>, Vec<u64>)>,
    mut samples: Local<Vec<FingerSample>>,
) {
    let st = state.map(|s| *s.get());
    if armed.0 != st {
        armed.0 = st;
        armed.1.clear();
    }
    match touches.as_deref() {
        Some(t) => finger_samples(t, &mut samples),
        None => samples.clear(),
    }
    for s in samples.iter().filter(|s| matches!(s.phase, FingerPhase::Pressed | FingerPhase::Tapped)) {
        if !armed.1.contains(&s.id) {
            armed.1.push(s.id);
        }
    }
    // The menu finger: a lifting one first (its lift is the click), else the
    // lowest id still down.
    let pick = samples
        .iter()
        .find(|s| matches!(s.phase, FingerPhase::Released | FingerPhase::Tapped))
        .or_else(|| samples.iter().find(|s| matches!(s.phase, FingerPhase::Pressed | FingerPhase::Held)))
        .copied();
    let finger = pick.map(|s| FingerPointer { position: s.pos, phase: s.phase, armed: armed.1.contains(&s.id) });
    for s in samples.iter().filter(|s| matches!(s.phase, FingerPhase::Released | FingerPhase::Tapped)) {
        armed.1.retain(|id| *id != s.id);
    }
    let window = windows.single().ok();
    let m = mouse.as_deref();
    let mouse = MousePointer {
        position: window.and_then(|w| w.physical_cursor_position()),
        pressed: m.is_some_and(|m| m.pressed(MouseButton::Left)),
        just_pressed: m.is_some_and(|m| m.just_pressed(MouseButton::Left)),
        just_released: m.is_some_and(|m| m.just_released(MouseButton::Left)),
    };
    let guard = activity.fingers > 0 || activity.since_touch < TOUCH_MOUSE_GUARD_SECS;
    let scale = window.map_or(1.0, |w| w.scale_factor());
    let want = resolve_ui_pointer(mouse, finger, guard, scale);
    if *ptr != want {
        *ptr = want;
    }
}

/// Replay a pending Android Back as a one-frame `Escape` press (released the
/// next frame). Runs after `sync_input_mode`, so it never flips the input mode.
pub fn apply_back_request(mut req: ResMut<BackRequest>, keys: Option<ResMut<ButtonInput<KeyCode>>>) {
    let Some(mut keys) = keys else {
        req.pending = false;
        return;
    };
    if req.injected {
        keys.release(KeyCode::Escape);
        req.injected = false;
    }
    if req.pending {
        req.pending = false;
        keys.press(KeyCode::Escape);
        req.injected = true;
    }
}

/// Registered by `CoreGameplayPlugin` (simulation + headless tests).
pub struct TouchPlugin;

impl Plugin for TouchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TouchInput>()
            .init_resource::<TouchActivity>()
            .init_resource::<UiPointer>()
            .init_resource::<SafeAreaInsets>()
            .init_resource::<BackRequest>()
            .add_systems(
                PreUpdate,
                (update_touch_input, update_ui_pointer, update_touch_auto_fire, merge_touch_into_pad)
                    .chain()
                    .after(InputSystems)
                    .after(update_gamepad_input)
                    .before(crate::gamepad::sync_input_mode),
            )
            .add_systems(PreUpdate, apply_back_request.after(crate::gamepad::sync_input_mode).after(InputSystems));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: Vec2 = Vec2::new(915.0, 412.0);

    fn layout() -> TouchLayout {
        TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { auto_bhop: true, ..default() })
    }

    fn at(l: &TouchLayout, b: TouchButton) -> Vec2 {
        l.button(b).unwrap_or_else(|| panic!("{b:?} missing")).center
    }

    fn press(id: u64, pos: Vec2) -> FingerSample {
        FingerSample { id, pos, phase: FingerPhase::Pressed }
    }

    fn held(id: u64, pos: Vec2) -> FingerSample {
        FingerSample { id, pos, phase: FingerPhase::Held }
    }

    fn lift(id: u64, pos: Vec2) -> FingerSample {
        FingerSample { id, pos, phase: FingerPhase::Released }
    }

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn layout_keeps_every_control_inside_the_safe_area_and_apart() {
        for (vp, insets) in [
            (PHONE, SafeAreaInsets::default()),
            (Vec2::new(844.0, 390.0), SafeAreaInsets::new(44.0, 0.0, 34.0, 12.0)),
            (Vec2::new(1280.0, 720.0), SafeAreaInsets::default()),
            (Vec2::new(640.0, 360.0), SafeAreaInsets::default()),
        ] {
            for scale in [0.75, 1.0, 1.35] {
                let ctx = LayoutContext { trial: true, sos: true, auto_bhop: true, ..default() };
                let l = TouchLayout::compute(vp, insets, scale, &ctx);
                for b in &l.buttons {
                    let r = b.rect();
                    assert!(r.min.x >= insets.left && r.max.x <= vp.x - insets.right + 0.5, "{b:?} x in {vp} {insets:?} @{scale}");
                    assert!(r.min.y >= insets.top - 0.5 && r.max.y <= vp.y - insets.bottom + 0.5, "{b:?} y in {vp} @{scale}");
                    // Never on the center reticle.
                    assert!(!r.inflate(16.0).contains(vp * 0.5), "{b:?} covers the reticle");
                }
                for (i, a) in l.buttons.iter().enumerate() {
                    for b in &l.buttons[i + 1..] {
                        let o = a.rect().intersect(b.rect());
                        assert!(o.is_empty(), "{:?} overlaps {:?} in {vp} @{scale}", a.button, b.button);
                    }
                }
                // The right cluster stays right, the stick rest left.
                assert!(at(&l, TouchButton::Fire).x > vp.x * 0.75 && l.stick_rest.x < vp.x * 0.25);
                assert!(l.stick_rest.x - l.stick_radius >= insets.left);
            }
        }
    }

    #[test]
    fn contextual_buttons_follow_the_game_state() {
        let base = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext::default());
        assert!(base.button(TouchButton::Sos).is_none() && base.button(TouchButton::Reset).is_none() && base.button(TouchButton::AutoBhop).is_none());
        let rich = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { trial: true, sos: true, auto_bhop: true, ..default() });
        assert!(rich.button(TouchButton::Sos).is_some() && rich.button(TouchButton::Reset).is_some() && rich.button(TouchButton::AutoBhop).is_some());
        // Bigger buttons with the scale setting, clamped to its range.
        let big = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 9.0, &LayoutContext::default());
        assert!((big.button(TouchButton::Fire).unwrap().half.x - 40.0 * TOUCH_BUTTON_SCALE_RANGE.1).abs() < 1e-3);
        // Deck slots become buttons.
        let slots = [Some(Rect::new(200.0, 380.0, 250.0, 396.0)), None, Some(Rect::new(f32::NAN, 0.0, 1.0, 1.0))];
        let l = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { slots, ..default() });
        assert!(l.button(TouchButton::Slot(0)).is_some() && l.button(TouchButton::Slot(1)).is_none() && l.button(TouchButton::Slot(2)).is_none());
        // Thin slots get extra vertical slop.
        assert_eq!(l.hit(Vec2::new(225.0, 372.0)), Some(TouchButton::Slot(0)));
    }

    #[test]
    fn safe_area_insets_clamp_and_convert() {
        let vp = Vec2::new(2400.0, 1080.0);
        assert_eq!(SafeAreaInsets::new(-5.0, f32::NAN, 120.0, 0.0).sanitized(vp), SafeAreaInsets::new(0.0, 0.0, 120.0, 0.0));
        let huge = SafeAreaInsets::new(2000.0, 900.0, 2000.0, 900.0).sanitized(vp);
        assert!(huge.left + huge.right <= vp.x * 0.4 + 1e-3 && huge.top + huge.bottom <= vp.y * 0.4 + 1e-3, "{huge:?}");
        assert_eq!(SafeAreaInsets::new(90.0, 0.0, 30.0, 0.0).to_logical(3.0), SafeAreaInsets::new(30.0, 0.0, 10.0, 0.0));
        assert_eq!(SafeAreaInsets::new(90.0, 0.0, 30.0, 0.0).to_logical(0.0), SafeAreaInsets::new(90.0, 0.0, 30.0, 0.0));
        // A notch on the left pushes the stick rest and the left controls right.
        let plain = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { auto_bhop: true, ..default() });
        let notch = TouchLayout::compute(PHONE, SafeAreaInsets::new(40.0, 0.0, 30.0, 0.0), 1.0, &LayoutContext { auto_bhop: true, ..default() });
        assert!((notch.stick_rest.x - plain.stick_rest.x - 40.0).abs() < 1e-3);
        assert!((at(&plain, TouchButton::Fire).x - at(&notch, TouchButton::Fire).x - 30.0).abs() < 1e-3);
    }

    #[test]
    fn floating_stick_anchors_deadzones_snaps_and_releases() {
        let l = layout();
        let mut r = TouchRouter::default();
        let origin = Vec2::new(200.0, 250.0);
        let f = r.step(&[press(1, origin)], &l, RouterConfig::default(), DT);
        assert_eq!(f.stick.map(|s| s.0), Some(origin), "anchored where the thumb landed");
        assert_eq!(f.move_axes, Vec2::ZERO);
        // Inside the deadzone: nothing.
        let f = r.step(&[held(1, origin + Vec2::new(5.0, 0.0))], &l, RouterConfig::default(), DT);
        assert_eq!(f.move_axes, Vec2::ZERO);
        // Near-pure sideways: a pure strafe (forward snapped to exactly 0).
        let f = r.step(&[held(1, origin + Vec2::new(40.0, -6.0))], &l, RouterConfig::default(), DT);
        assert!(f.move_axes.x > 0.5 && f.move_axes.y == 0.0, "{}", f.move_axes);
        // Up on screen = forward.
        let f = r.step(&[held(1, origin + Vec2::new(-4.0, -40.0))], &l, RouterConfig::default(), DT);
        assert!(f.move_axes.y > 0.5 && f.move_axes.x == 0.0, "{}", f.move_axes);
        // Diagonal keeps both.
        let f = r.step(&[held(1, origin + Vec2::new(30.0, 30.0))], &l, RouterConfig::default(), DT);
        assert!(f.move_axes.x > 0.3 && f.move_axes.y < -0.3);
        // Lift: stick gone, no movement.
        let f = r.step(&[lift(1, origin)], &l, RouterConfig::default(), DT);
        assert!(f.stick.is_none() && f.move_axes == Vec2::ZERO && r.finger_count() == 0);
        // Anchoring near an edge clamps the base inside the safe area.
        let f = r.step(&[press(2, Vec2::new(2.0, 410.0))], &l, RouterConfig::default(), DT);
        let o = f.stick.unwrap().0;
        assert!(o.x - l.stick_radius >= l.safe.min.x - 1e-3 && o.y + l.stick_radius <= l.safe.max.y + 1e-3, "{o}");
    }

    #[test]
    fn stick_follow_pull_lets_strafes_reverse_instantly() {
        let l = layout();
        let mut r = TouchRouter::default();
        let origin = Vec2::new(200.0, 250.0);
        r.step(&[press(1, origin)], &l, RouterConfig::default(), DT);
        // Drag far right and hold: the base follows to ~1.1 radii.
        let far = origin + Vec2::new(l.stick_radius * 3.0, 0.0);
        let mut f = TouchFrame::default();
        for _ in 0..60 {
            f = r.step(&[held(1, far)], &l, RouterConfig::default(), DT);
        }
        let base = f.stick.unwrap().0;
        assert!((far.x - base.x - l.stick_radius * STICK_FOLLOW_START).abs() < 1.0, "base {base} finger {far}");
        // Moving back by just over one radius now reads full left at once.
        let back = Vec2::new(base.x - l.stick_radius, far.y);
        let f = r.step(&[held(1, back)], &l, RouterConfig::default(), DT);
        assert!(f.move_axes.x < -0.9, "{}", f.move_axes);
        assert_eq!(follow_stick_origin(origin, origin + Vec2::X * 10.0, 50.0, DT), origin, "inside the rim the base stays");
        assert_eq!(follow_stick_origin(origin, Vec2::new(f32::NAN, 0.0), 50.0, DT), origin);
    }

    #[test]
    fn right_half_drag_looks_normalised_by_viewport_height() {
        let l = layout();
        let mut r = TouchRouter::default();
        let p = Vec2::new(650.0, 150.0);
        let f = r.step(&[press(1, p)], &l, RouterConfig::default(), DT);
        assert_eq!(f.look_delta, Vec2::ZERO, "touch-down never turns");
        let f = r.step(&[held(1, p + Vec2::new(41.2, 0.0))], &l, RouterConfig::default(), DT);
        // 41.2 px = 0.1 heights -> 0.28 rad yaw right.
        assert!((f.look_delta.x - 0.1 * TOUCH_LOOK_GAIN).abs() < 1e-4 && f.look_delta.y == 0.0, "{}", f.look_delta);
        // Drag up looks up.
        let f = r.step(&[held(1, p + Vec2::new(41.2, -20.6))], &l, RouterConfig::default(), DT);
        assert!(f.look_delta.y > 0.0 && f.look_delta.x == 0.0, "{}", f.look_delta);
        // A held still finger never drifts (no stale deltas).
        let f = r.step(&[held(1, p + Vec2::new(41.2, -20.6))], &l, RouterConfig::default(), DT);
        assert_eq!(f.look_delta, Vec2::ZERO);
        // DPI independence: the same share of the screen turns the same.
        let a = touch_look_delta(Vec2::new(41.2, 0.0), 412.0, 1.0);
        let b = touch_look_delta(Vec2::new(108.0, 0.0), 1080.0, 1.0);
        assert!((a - b).length() < 1e-4);
        // Sensitivity scales; a spike clamps; garbage reads zero.
        assert!((touch_look_delta(Vec2::new(41.2, 0.0), 412.0, 2.0).x - 2.0 * a.x).abs() < 1e-5);
        assert!(touch_look_delta(Vec2::new(5000.0, 0.0), 412.0, 1.0).x <= TOUCH_LOOK_SPIKE * TOUCH_LOOK_GAIN + 1e-4);
        assert_eq!(touch_look_delta(Vec2::new(f32::NAN, 0.0), 412.0, 1.0), Vec2::ZERO);
        assert_eq!(touch_look_delta(Vec2::X, 0.0, 1.0), Vec2::ZERO);
        assert!((touch_look_delta(Vec2::new(41.2, 0.0), 412.0, f32::NAN).x - a.x).abs() < 1e-5);
    }

    #[test]
    fn slow_tracking_reads_as_fine_aim_and_flicks_as_full_stick() {
        use crate::player::{aim_friction_scale, AIM_FRICTION_MAX_SLOW};
        let slow = touch_look_mag(Vec2::new(0.01, 0.0), DT); // 0.6 rad/s
        let flick = touch_look_mag(Vec2::new(0.12, 0.0), DT); // 7.2 rad/s
        assert!(slow < 0.5 && flick == 1.0, "{slow} {flick}");
        assert!((aim_friction_scale(0.0, slow) - (1.0 - AIM_FRICTION_MAX_SLOW)).abs() < 1e-5, "slow tracking over a target is slowed");
        assert_eq!(aim_friction_scale(0.0, flick), 1.0, "a flick is never slowed");
        assert_eq!(touch_look_mag(Vec2::ZERO, DT), 0.0);
        assert_eq!(touch_look_mag(Vec2::X, 0.0), 0.0);
    }

    #[test]
    fn fire_button_holds_fire_and_its_drag_aims() {
        let l = layout();
        let fire = at(&l, TouchButton::Fire);
        let mut r = TouchRouter::default();
        let f = r.step(&[press(7, fire)], &l, RouterConfig::default(), DT);
        assert!(f.fire && f.fire_just_pressed && f.is_pressed(TouchButton::Fire));
        let f = r.step(&[held(7, fire + Vec2::new(-20.6, 0.0))], &l, RouterConfig::default(), DT);
        assert!(f.fire && !f.fire_just_pressed, "held fire is not a new squeeze");
        assert!(f.look_delta.x < 0.0, "the fire finger aims while shooting: {}", f.look_delta);
        // Dragged off the plate it keeps firing (the role sticks to the finger).
        let f = r.step(&[held(7, fire + Vec2::new(-200.0, -100.0))], &l, RouterConfig::default(), DT);
        assert!(f.fire);
        let f = r.step(&[lift(7, fire)], &l, RouterConfig::default(), DT);
        assert!(!f.fire && f.pressed == 0);
        // A one-frame tap still fires once.
        let f = r.step(&[FingerSample { id: 8, pos: fire, phase: FingerPhase::Tapped }], &l, RouterConfig::default(), DT);
        assert!(f.fire && f.fire_just_pressed && r.finger_count() == 0);
    }

    #[test]
    fn multi_touch_move_look_fire_jump_and_apps_stay_isolated() {
        let l = layout();
        let mut r = TouchRouter::default();
        let stick = Vec2::new(180.0, 260.0);
        let look = Vec2::new(560.0, 120.0);
        let cfg = RouterConfig { auto_bhop: false, ..default() };
        r.step(&[press(1, stick), press(2, look)], &l, cfg, DT);
        let f = r.step(&[held(1, stick + Vec2::new(0.0, -50.0)), held(2, look + Vec2::new(20.0, 0.0)), press(3, at(&l, TouchButton::Fire))], &l, cfg, DT);
        assert!(f.move_axes.y > 0.8 && f.look_delta.x > 0.0 && f.fire && f.fingers == 3);
        let f = r.step(
            &[held(1, stick + Vec2::new(0.0, -50.0)), held(2, look + Vec2::new(20.0, 0.0)), held(3, at(&l, TouchButton::Fire)), press(4, at(&l, TouchButton::Jump))],
            &l,
            cfg,
            DT,
        );
        assert!(f.jump && f.jump_pressed && f.fire && f.move_axes.y > 0.8 && f.look_delta == Vec2::ZERO);
        // A one-shot app press on top of everything.
        let f = r.step(
            &[
                held(1, stick + Vec2::new(0.0, -50.0)),
                held(2, look),
                held(3, at(&l, TouchButton::Fire)),
                held(4, at(&l, TouchButton::Jump)),
                press(5, at(&l, TouchButton::Overload)),
            ],
            &l,
            cfg,
            DT,
        );
        assert!(f.overload && !f.ping && !f.flash && !f.dash && !f.punch && f.jump && !f.jump_pressed && f.fire);
        assert!(f.look_delta.x < 0.0, "look finger moved back");
        // Next frame the one-shot is gone but the held ones remain.
        let f = r.step(&[held(1, stick), held(2, look), held(3, at(&l, TouchButton::Fire)), held(4, at(&l, TouchButton::Jump)), held(5, at(&l, TouchButton::Overload))], &l, cfg, DT);
        assert!(!f.overload && f.jump && f.fire);
        // Every other button maps to its own one-shot.
        for (b, get) in [
            (TouchButton::Dash, (|f: &TouchFrame| f.dash) as fn(&TouchFrame) -> bool),
            (TouchButton::Punch, |f| f.punch),
            (TouchButton::Ping, |f| f.ping),
            (TouchButton::Flash, |f| f.flash),
            (TouchButton::Swap, |f| f.swap),
            (TouchButton::Pause, |f| f.pause),
        ] {
            let mut r = TouchRouter::default();
            let f = r.step(&[press(9, at(&l, b))], &l, cfg, DT);
            assert!(get(&f) && f.pressed == b.bit(), "{b:?}");
        }
        // A second finger in the stick zone never steals the stick.
        let mut r = TouchRouter::default();
        r.step(&[press(1, stick)], &l, cfg, DT);
        let f = r.step(&[held(1, stick + Vec2::new(40.0, 0.0)), press(2, Vec2::new(100.0, 100.0))], &l, cfg, DT);
        assert!(f.move_axes.x > 0.5 && f.look_delta == Vec2::ZERO);
        // A finger vanishing from the samples (focus loss) counts as lifted.
        let f = r.step(&[], &l, cfg, DT);
        assert!(f.stick.is_none() && r.finger_count() == 0);
    }

    #[test]
    fn auto_bhop_holds_jump_on_the_rim_and_the_pill_latches() {
        let l = layout();
        let stick = Vec2::new(180.0, 260.0);
        let rim = l.stick_radius * AUTO_BHOP_RIM;
        let on = RouterConfig { auto_bhop: true, ..default() };
        let mut r = TouchRouter::default();
        r.step(&[press(1, stick)], &l, on, DT);
        let f = r.step(&[held(1, stick + Vec2::new(0.0, -(rim - 4.0)))], &l, on, DT);
        assert!(!f.jump, "inside 85%: no jump");
        let f = r.step(&[held(1, stick + Vec2::new(0.0, -(rim + 2.0)))], &l, on, DT);
        assert!(f.jump && f.jump_pressed, "rim: jump held");
        let f = r.step(&[held(1, stick + Vec2::new(0.0, -(rim + 2.0)))], &l, on, DT);
        assert!(f.jump && !f.jump_pressed, "held jump chains bhops like holding Space");
        let f = r.step(&[held(1, stick + Vec2::new(0.0, -(rim - 10.0)))], &l, on, DT);
        assert!(!f.jump, "pulling back stops");
        // Off: the rim never jumps.
        let mut r = TouchRouter::default();
        let off = RouterConfig { auto_bhop: false, ..default() };
        r.step(&[press(1, stick)], &l, off, DT);
        assert!(!r.step(&[held(1, stick + Vec2::new(0.0, -l.stick_radius))], &l, off, DT).jump);
        // The pill latches jump while moving (any deflection past the deadzone).
        let mut r = TouchRouter::default();
        let pill = at(&l, TouchButton::AutoBhop);
        let f = r.step(&[press(2, pill)], &l, on, DT);
        assert!(f.auto_bhop_latched && !f.jump, "latched, but standing still");
        r.step(&[lift(2, pill), press(1, stick)], &l, on, DT);
        let f = r.step(&[held(1, stick + Vec2::new(25.0, 0.0))], &l, on, DT);
        assert!(f.jump && f.move_axes.x > 0.0);
        let f = r.step(&[lift(1, stick)], &l, on, DT);
        assert!(!f.jump && f.auto_bhop_latched, "stops when the thumb lifts, latch stays");
        r.step(&[press(3, pill)], &l, on, DT);
        assert!(!r.step(&[lift(3, pill)], &l, on, DT).auto_bhop_latched, "tap again to unlatch");
    }

    #[test]
    fn weapon_slots_and_reset_route_their_one_shots() {
        let slots = [Some(Rect::new(200.0, 380.0, 250.0, 396.0)), Some(Rect::new(256.0, 380.0, 306.0, 396.0)), Some(Rect::new(312.0, 380.0, 362.0, 396.0))];
        let l = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { trial: true, slots, ..default() });
        let mut r = TouchRouter::default();
        let f = r.step(&[press(1, Vec2::new(337.0, 388.0))], &l, RouterConfig::default(), DT);
        assert_eq!(f.weapon_slot, Some(2));
        assert!(f.stick.is_none(), "a slot tap in the left half never anchors the stick");
        let f = r.step(&[press(2, at(&l, TouchButton::Reset))], &l, RouterConfig::default(), DT);
        assert!(f.reset && f.weapon_slot.is_none());
    }

    #[test]
    fn auto_fire_needs_the_cone_range_and_line_of_sight() {
        let rows: Vec<String> = (0..12).map(|z| if z == 0 || z == 11 { "#".repeat(12) } else { format!("#{}#", ".".repeat(10)) }).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let grid = LevelGrid::from_ascii(&rows);
        let eye = grid.cell_center(IVec2::new(2, 5)) + Vec3::Y * 1.6;
        let east = Vec3::X;
        let ahead = (eye + Vec3::X * 10.0, 0.6);
        assert!(auto_fire_target(Some(&grid), eye, east, 30.0, std::iter::once(ahead)));
        assert!(!auto_fire_target(Some(&grid), eye, east, 8.0, std::iter::once(ahead)), "out of weapon range");
        assert!(!auto_fire_target(Some(&grid), eye, east, 30.0, std::iter::once((eye + Vec3::new(10.0, 0.0, 3.0), 0.6))), "outside the cone");
        assert!(!auto_fire_target(Some(&grid), eye, -east, 30.0, std::iter::once(ahead)), "behind");
        assert!(!auto_fire_target(Some(&grid), eye, east, 300.0, std::iter::once((eye + Vec3::X * 60.0, 0.6))), "through the wall");
        assert!(!auto_fire_target(None, eye, Vec3::ZERO, 30.0, std::iter::once(ahead)));
        assert!(auto_fire_allowed(true, true, 0.0));
        assert!(!auto_fire_allowed(false, true, 0.0) && !auto_fire_allowed(true, false, 0.0) && !auto_fire_allowed(true, true, 0.1));
        assert!(auto_fire_range(Weapon::Shotgun) < auto_fire_range(Weapon::Plasma));
    }

    #[test]
    fn input_mode_switches_on_touch_and_ignores_synthetic_mouse() {
        use InputMode::*;
        use TouchMode::*;
        assert_eq!(resolve_input_mode(KeyboardMouse, Auto, true, false, false), Touch);
        assert_eq!(resolve_input_mode(Gamepad, Auto, true, true, false), Touch, "a touch beats a still-active pad flag");
        assert_eq!(resolve_input_mode(Touch, Auto, false, false, false), Touch, "stays without other input");
        assert_eq!(resolve_input_mode(Touch, Auto, false, true, false), Gamepad, "the pad moved");
        assert_eq!(resolve_input_mode(Touch, Auto, false, false, true), KeyboardMouse, "a real key / mouse");
        assert_eq!(resolve_input_mode(KeyboardMouse, Auto, false, true, false), Gamepad);
        assert_eq!(resolve_input_mode(Touch, Off, true, false, false), KeyboardMouse, "Off never touches");
        assert_eq!(resolve_input_mode(Touch, Off, true, true, false), Gamepad);
        assert_eq!(resolve_input_mode(Gamepad, On, false, true, true), Touch, "On forces touch");
        // The guard: mouse right after a touch is synthetic, keys always count.
        assert!(!kbm_is_real(false, true, 0, 0.1));
        assert!(!kbm_is_real(false, true, 1, 10.0));
        assert!(kbm_is_real(false, true, 0, TOUCH_MOUSE_GUARD_SECS));
        assert!(kbm_is_real(true, false, 2, 0.0));
        assert!(!kbm_is_real(false, false, 0, 99.0));
        // Overlay, router and cursor capture gates.
        assert!(overlay_visible(AppState::Playing, Auto, Touch) && !overlay_visible(AppState::Playing, Auto, KeyboardMouse));
        assert!(overlay_visible(AppState::Playing, On, Gamepad) && !overlay_visible(AppState::Playing, Off, Touch));
        assert!(!overlay_visible(AppState::Paused, On, Touch));
        assert!(router_active(AppState::Playing, Auto, KeyboardMouse, true) && !router_active(AppState::Playing, Auto, KeyboardMouse, false));
        assert!(!router_active(AppState::Playing, Off, Touch, true) && !router_active(AppState::MainMenu, On, Touch, true));
        assert!(!wants_cursor_capture(Touch, Auto) && !wants_cursor_capture(KeyboardMouse, On) && wants_cursor_capture(KeyboardMouse, Auto));
    }

    #[test]
    fn ui_pointer_converts_touch_to_physical_px_and_never_double_fires() {
        let mouse = MousePointer { position: Some(Vec2::new(10.0, 10.0)), pressed: false, just_pressed: false, just_released: true };
        // A finger lifting at logical (100, 50) on a 2.625x phone.
        let f = FingerPointer { position: Vec2::new(100.0, 50.0), phase: FingerPhase::Released, armed: true };
        let p = resolve_ui_pointer(mouse, Some(f), true, 2.625);
        assert_eq!(p.position, Some(Vec2::new(262.5, 131.25)), "logical * scale = physical");
        assert!((p.slop - TOUCH_UI_SLOP * 2.625).abs() < 1e-4, "finger slop in physical px");
        assert!(within_slop(Vec2::new(112.0, 50.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), 3.0));
        assert!(!within_slop(Vec2::new(114.0, 50.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), 3.0));
        assert!(!within_slop(Vec2::ZERO, Vec2::ZERO, Vec2::ZERO, 3.0), "an unlaid-out node is never hit");
        assert!(p.released && p.touch && !p.pressed);
        // While the finger is down: press highlight, no activation.
        let p = resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Held, ..f }), true, 2.625);
        assert!(p.pressed && !p.released);
        // An unarmed finger (carried over from gameplay) never activates.
        assert!(!resolve_ui_pointer(mouse, Some(FingerPointer { armed: false, ..f }), true, 1.0).released);
        // A fast tap activates.
        assert!(resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Tapped, ..f }), true, 1.0).released);
        // The synthetic mouse release right after the tap is swallowed.
        assert_eq!(resolve_ui_pointer(mouse, None, true, 1.0), UiPointer::default());
        // A real mouse outside the guard works as before.
        let p = resolve_ui_pointer(mouse, None, false, 2.0);
        assert!(p.released && !p.touch && p.position == Some(Vec2::new(10.0, 10.0)) && p.logical == Some(Vec2::new(5.0, 5.0)));
        assert_eq!(p.slop, 0.0, "the mouse stays pixel exact");
    }

    #[test]
    fn finger_samples_classify_presses_holds_lifts_and_taps() {
        use bevy::input::touch::{TouchInput, TouchPhase};
        let mut app = App::new();
        app.add_plugins(bevy::input::InputPlugin);
        let w = Entity::PLACEHOLDER;
        let ev = |id, phase, x: f32| TouchInput { phase, position: Vec2::new(x, 10.0), window: w, force: None, id };
        app.world_mut().write_message(ev(2, TouchPhase::Started, 1.0));
        app.world_mut().write_message(ev(1, TouchPhase::Started, 5.0));
        app.world_mut().write_message(ev(3, TouchPhase::Started, 9.0));
        app.world_mut().write_message(ev(3, TouchPhase::Ended, 9.0));
        app.update();
        let mut out = Vec::new();
        finger_samples(app.world().resource::<Touches>(), &mut out);
        assert_eq!(out.iter().map(|s| (s.id, s.phase)).collect::<Vec<_>>(), [(1, FingerPhase::Pressed), (2, FingerPhase::Pressed), (3, FingerPhase::Tapped)]);
        app.world_mut().write_message(ev(2, TouchPhase::Moved, 4.0));
        app.world_mut().write_message(ev(1, TouchPhase::Canceled, 5.0));
        app.update();
        finger_samples(app.world().resource::<Touches>(), &mut out);
        assert_eq!(out.iter().map(|s| (s.id, s.phase)).collect::<Vec<_>>(), [(1, FingerPhase::Released), (2, FingerPhase::Held)]);
        assert_eq!(out[1].pos, Vec2::new(4.0, 10.0));
    }

    #[test]
    fn back_request_replays_one_escape_press() {
        let mut app = App::new();
        app.add_plugins(bevy::input::InputPlugin).init_resource::<BackRequest>().add_systems(PreUpdate, apply_back_request.after(InputSystems));
        app.world_mut().resource_mut::<BackRequest>().request();
        app.update();
        let keys = app.world().resource::<ButtonInput<KeyCode>>();
        assert!(keys.just_pressed(KeyCode::Escape));
        app.update();
        let keys = app.world().resource::<ButtonInput<KeyCode>>();
        assert!(!keys.pressed(KeyCode::Escape) && !keys.just_pressed(KeyCode::Escape), "released the next frame");
        app.update();
        assert!(!app.world().resource::<BackRequest>().pending);
    }

    #[test]
    fn merge_owns_touch_fields_without_a_pad_and_ors_with_one() {
        let mut world = World::new();
        world.insert_resource(GamepadInput::default());
        let mut t = TouchInput { active: true, ..default() };
        t.frame = TouchFrame { fire: true, jump: true, move_axes: Vec2::new(0.0, 1.0), weapon_slot: Some(1), ..default() };
        world.insert_resource(t);
        let mut sys = IntoSystem::into_system(merge_touch_into_pad);
        sys.initialize(&mut world);
        sys.run((), &mut world).unwrap();
        let p = world.resource::<GamepadInput>().clone();
        assert!(p.fire && p.jump && p.move_axes.y == 1.0 && p.weapon_slot == Some(1) && !p.active, "touch never claims the pad flag");
        // Fingers lift: everything clears (no pad recomputes these fields).
        world.resource_mut::<TouchInput>().frame = TouchFrame::default();
        sys.run((), &mut world).unwrap();
        let p = world.resource::<GamepadInput>().clone();
        assert!(!p.fire && !p.jump && p.move_axes == Vec2::ZERO && p.weapon_slot.is_none());
        // Values staged by someone else (dev tours, tests) survive while touch is idle.
        world.resource_mut::<GamepadInput>().fire = true;
        sys.run((), &mut world).unwrap();
        assert!(world.resource::<GamepadInput>().fire);
        // Auto-fire alone holds the trigger without a dry-click edge.
        world.resource_mut::<GamepadInput>().fire = false;
        world.resource_mut::<TouchInput>().auto_fire = true;
        sys.run((), &mut world).unwrap();
        let p = world.resource::<GamepadInput>().clone();
        assert!(p.fire && !p.fire_just_pressed);
    }
}
