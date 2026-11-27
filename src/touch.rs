//! Milestone 37: touch controls (Android APK + mobile web).
//!
//! One platform-neutral layer over Bevy's raw touch messages, so a phone
//! browser (itch.io) and the native APK behave identically:
//!
//! - [`TouchLayout`]: where every on-screen control sits (logical px), inside
//!   the display-cutout / CSS `env(safe-area-inset-*)` [`SafeAreaInsets`],
//!   scaled by `UserSettings::touch_button_scale` and width-bounded so a
//!   narrow portrait viewport never overlaps two controls.
//! - [`TouchRouter`]: a deterministic multi-touch router fed the frame's raw
//!   touch edges **in arrival order** (a lift retires the old binding before a
//!   reused finger id binds a new control). Each finger is bound on press (hit
//!   tested where it landed) to one role and keeps it until lift: a floating
//!   left-half thumbstick, right-half drag look, or one action button. The FIRE
//!   finger also aims (its drag feeds the look delta). Auto-bhop holds jump
//!   while the stick sits on its rim. A `Canceled` contact releases its
//!   binding without ever counting as a tap.
//! - [`merge_touch_into_pad`] folds the router's [`TouchFrame`] into
//!   [`GamepadInput`], so touch drives exactly the same movement, weapons,
//!   apps, punch, flash, SOS and pause code as a controller (no gameplay
//!   system needed a touch-specific branch). Auto-fire is evaluated in
//!   `Update` right before `fire_weapons`, on the very eye pose that fires.
//! - Focus loss, app suspension and the Android `ResetAll` interrupt every
//!   contact: bindings, the merged pad contribution and armed menu taps are
//!   dropped, and the stale contacts are ignored until they lift.
//! - [`resolve_input_mode`]: `InputMode::Touch` switching (Auto / On / Off),
//!   guarded against the synthetic mouse events browsers emit after a tap.
//! - [`UiPointer`]: one pointer for every menu click handler: the mouse, or
//!   a finger (press highlight while down, activation on lift inside the
//!   target it pressed, never after a drag owner claimed it, never both for
//!   the same tap). Armed taps are invalidated whenever the screen changes.
//! - [`BackRequest`]: the Android system Back gesture, replayed as a one-frame
//!   `Escape` press so every screen's existing Esc handling applies (pause in
//!   a run, close the controls overlay, back out of a submenu; nothing on the
//!   main menu, so Back never kills the app).
//!
//! Everything except the Bevy systems is pure and unit-tested on the desktop.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::input::touch::{TouchInput as TouchEvent, TouchPhase, Touches};
use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::{AppLifecycle, PrimaryWindow, WindowFocused};

use crate::boss::{BossPylon, PYLON_SPHERES};
use crate::enemies::{ray_sphere, Enemy};
use crate::extraction::ExtractionState;
use crate::gamepad::{apply_radial_deadzone, snap_move_axes, update_gamepad_input, GamepadInput, InputMode};
use crate::level::{DemonJammer, LevelGrid};
use crate::meta::{live_settings, ProfileStore, TouchMode, TOUCH_BUTTON_SCALE_RANGE, TOUCH_SENS_RANGE};
use crate::player::{live_eye_pose, silhouette_angle, CursorCaptured, Player, PlayerCamera};
use crate::signal::SignalState;
use crate::state::{AppState, GameSet, PlayerModifiers, RunMode, RunStats};
use crate::weapons::{rocket_splash_radius, Weapon, WeaponState, JAMMER_SPHERES};

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

/// Reference phone height (logical px) the layout is designed at (a 20:9
/// phone in landscape is ~390-412 dp tall).
pub const LAYOUT_REFERENCE_H: f32 = 412.0;
/// Design width (design px) the stick column plus the right-thumb cluster
/// need side by side: narrower safe areas (portrait phones) shrink the unit.
pub const LAYOUT_DESIGN_W: f32 = 400.0;
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
/// Extra distance (m) past the splash radius a rocket's first impact must
/// be before auto-fire squeezes the Signal-Mortar (never a self-blast).
pub const ROCKET_AUTO_FIRE_MARGIN: f32 = 1.0;

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

/// Display-cutout / rounded-corner insets in **physical** px. Android reports
/// them as such (`WindowInsetsCompat.Type.displayCutout()`); the web bridge
/// converts the CSS `env(safe-area-inset-*)` logical px once (x the window
/// scale factor), so both go through the same [`Self::to_logical`]. Zero on
/// the desktop. Touch HUD margins never place a control inside them.
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

    /// CSS `env(safe-area-inset-*)` values (CSS / logical px) -> the physical
    /// px this resource stores (`scale` = the window scale factor, i.e. the
    /// browser's `devicePixelRatio`).
    pub fn from_css(top: f32, right: f32, bottom: f32, left: f32, scale: f32) -> Self {
        let k = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let px = |v: f32| if v.is_finite() { v.max(0.0) * k } else { 0.0 };
        SafeAreaInsets { left: px(left), top: px(top), right: px(right), bottom: px(bottom) }
    }
}

/// Parse a computed CSS length (`"12px"`, `"0px"`, `"3.5px"`, `""`) as px.
pub fn parse_css_px(v: &str) -> f32 {
    v.trim().trim_end_matches("px").trim().parse::<f32>().ok().filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0)
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
    /// Logical px per design px (phone height reference x button scale,
    /// bounded by the safe width).
    pub unit: f32,
    pub stick_radius: f32,
    /// Where the stick rests (drawn dimmed) while no finger holds it.
    pub stick_rest: Vec2,
    pub buttons: Vec<ButtonRect>,
}

impl TouchLayout {
    /// Lay the controls out for a `viewport` (logical px) with logical
    /// `insets`. Right thumb: FIRE (large) in the bottom-right, JUMP to its
    /// left and the contextual SOS left of JUMP on the bottom row, DASH /
    /// PUNCH / SWAP above, the PING / FLASH / 5G phone-app row (1 -> 2 -> 3
    /// bars, like the phone OLED) above that; PAUSE (+ RST) in the top-right
    /// corner; the AUTO-BHOP pill above the stick's rest spot. The unit is
    /// bounded by the safe width ([`LAYOUT_DESIGN_W`]), so a portrait phone
    /// shrinks the plates instead of overlapping them.
    pub fn compute(viewport: Vec2, insets: SafeAreaInsets, button_scale: f32, ctx: &LayoutContext) -> Self {
        let viewport = if viewport.is_finite() && viewport.x > 1.0 && viewport.y > 1.0 { viewport } else { Vec2::new(1280.0, 720.0) };
        let insets = insets.sanitized(viewport);
        let scale = if button_scale.is_finite() { button_scale.clamp(TOUCH_BUTTON_SCALE_RANGE.0, TOUCH_BUTTON_SCALE_RANGE.1) } else { 1.0 };
        let base = (viewport.y / LAYOUT_REFERENCE_H).clamp(0.8, 1.4);
        let m = LAYOUT_MARGIN * base;
        let safe = Rect::new(insets.left + m, insets.top + m, viewport.x - insets.right - m, viewport.y - insets.bottom - m);
        let u = (base * scale).min(safe.width().max(1.0) / LAYOUT_DESIGN_W);
        let br = safe.max;
        let tr = Vec2::new(safe.max.x, safe.min.y);
        let mut buttons = Vec::with_capacity(16);
        let mut put = |button: TouchButton, center: Vec2, half: f32| buttons.push(ButtonRect { button, center, half: Vec2::splat(half * u) });
        // Right-thumb arc (design px, relative to the bottom-right corner).
        // Every plate is >= 44 px (half 22) with >= 6 px gaps.
        let at = |x: f32, y: f32| br + Vec2::new(x, y) * u;
        put(TouchButton::Fire, at(-46.0, -50.0), 40.0);
        put(TouchButton::Jump, at(-129.0, -40.0), 31.0);
        put(TouchButton::Dash, at(-140.0, -107.0), 24.0);
        put(TouchButton::Punch, at(-80.0, -126.0), 24.0);
        put(TouchButton::Swap, at(-28.0, -124.0), 22.0);
        // Phone-app row: 1 bar -> 2 bars -> 3 bars, left to right.
        put(TouchButton::Ping, at(-128.0, -186.0), 22.0);
        put(TouchButton::Flash, at(-78.0, -186.0), 22.0);
        put(TouchButton::Overload, at(-28.0, -186.0), 22.0);
        if ctx.sos {
            // Bottom row, left of JUMP. Never within 48 px of the reticle
            // (only matters on a viewport short enough to bring the bottom
            // row level with it).
            let half = 28.0 * u;
            let mut c = at(-204.0, -42.0);
            if (c.y - viewport.y * 0.5).abs() < half + 48.0 {
                c.x = c.x.max(viewport.x * 0.5 + 48.0 + half);
            }
            put(TouchButton::Sos, c, 28.0);
        }
        // Top-right system corner.
        put(TouchButton::Pause, tr + Vec2::new(-22.0, 22.0) * u, 22.0);
        if ctx.trial {
            put(TouchButton::Reset, tr + Vec2::new(-74.0, 22.0) * u, 22.0);
        }
        let stick_radius = STICK_RADIUS * u;
        let stick_rest = Vec2::new(safe.min.x + stick_radius + 8.0 * u, safe.max.y - stick_radius - 4.0 * u);
        if ctx.auto_bhop {
            // A 92x30 pill hugging the left safe edge, well clear of the ring.
            let c = Vec2::new(safe.min.x + 46.0 * u, stick_rest.y - stick_radius - 44.0 * u);
            buttons.push(ButtonRect { button: TouchButton::AutoBhop, center: c, half: Vec2::new(46.0, 15.0) * u });
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

    /// Where the stick's ring draws for an input anchor at `p`: fully inside
    /// the safe area and the left half. Input never uses this (a thumb landing
    /// in a corner starts at zero deflection, not pre-deflected toward the
    /// clamped ring).
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

/// One raw touch edge (or, for [`FingerPhase::Tapped`], a down + up pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerPhase {
    /// Touched down (`TouchPhase::Started`).
    Pressed,
    /// Moved while down (`TouchPhase::Moved`).
    Held,
    /// Lifted (`TouchPhase::Ended`).
    Released,
    /// Down and up at one spot (a fast tap in one sample).
    Tapped,
    /// The OS took the contact away (`TouchPhase::Canceled`): its binding is
    /// released, but it is never a tap, a click or a lift activation.
    Canceled,
}

impl FingerPhase {
    pub fn from_touch(phase: TouchPhase) -> Self {
        match phase {
            TouchPhase::Started => FingerPhase::Pressed,
            TouchPhase::Moved => FingerPhase::Held,
            TouchPhase::Ended => FingerPhase::Released,
            TouchPhase::Canceled => FingerPhase::Canceled,
        }
    }
}

/// One finger edge (logical px, `y` down).
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
    /// Touched down during the current [`TouchRouter::step`].
    fresh: bool,
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
    /// The live stick as drawn: `(ring center, puck center)` in logical px
    /// (the ring is clamped inside the safe area).
    pub stick: Option<(Vec2, Vec2)>,
    /// The live stick's input anchor (logical px, never clamped).
    pub stick_anchor: Option<Vec2>,
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
    /// Input anchor of the live stick: where the thumb landed (then
    /// follow-pulled). Never clamped; the drawn ring is clamped separately.
    stick_anchor: Option<Vec2>,
    stick_pos: Vec2,
    bhop_latch: bool,
    prev_jump: bool,
}

impl TouchRouter {
    /// A fresh run (entering `Playing`): every finger dropped and the
    /// AUTO-BHOP latch off, so a latch never carries into a new run.
    pub fn reset(&mut self) {
        *self = TouchRouter::default();
    }

    /// Drop every finger (router idle, focus loss, suspension): nothing stays
    /// held. The AUTO-BHOP latch survives.
    pub fn release_all(&mut self) {
        let latch = self.bhop_latch;
        *self = TouchRouter { bhop_latch: latch, ..default() };
    }

    /// Fingers currently bound.
    pub fn finger_count(&self) -> usize {
        self.fingers.len()
    }

    /// Whether finger `id` is bound.
    pub fn is_bound(&self, id: u64) -> bool {
        self.fingers.iter().any(|f| f.id == id)
    }

    /// Ids of every bound finger.
    pub fn bound_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.fingers.iter().map(|f| f.id)
    }

    /// Free finger `id`'s role. Retiring the stick thumb hands the stick to a
    /// thumb that landed in the stick zone this same frame (a one-frame thumb
    /// swap never drops movement for a frame or strands the new thumb).
    fn retire(&mut self, id: u64) {
        let Some(i) = self.fingers.iter().position(|f| f.id == id) else { return };
        let f = self.fingers.remove(i);
        if f.role == Role::Stick {
            self.stick_anchor = None;
            if let Some(n) = self.fingers.iter_mut().rev().find(|n| n.fresh && n.role == Role::Ignored) {
                n.role = Role::Stick;
                self.stick_anchor = Some(n.last);
                self.stick_pos = n.last;
            }
        }
    }

    /// Route one frame of finger edges, in arrival order.
    pub fn step(&mut self, samples: &[FingerSample], layout: &TouchLayout, cfg: RouterConfig, dt: f32) -> TouchFrame {
        let mut out = TouchFrame::default();
        let mut look_px = Vec2::ZERO;
        // Press edges of this frame (finger, button), dropped if the same
        // binding is canceled before the frame ends.
        let mut presses: Vec<(u64, TouchButton)> = Vec::new();
        let spike = TOUCH_LOOK_SPIKE * layout.viewport.y.max(1.0);
        for f in &mut self.fingers {
            f.fresh = false;
        }

        for s in samples {
            match s.phase {
                FingerPhase::Pressed | FingerPhase::Tapped => {
                    if !s.pos.is_finite() {
                        continue;
                    }
                    // A reused id: whatever it was bound to is over.
                    self.retire(s.id);
                    // Hit tested where the finger landed.
                    let role = match layout.hit(s.pos) {
                        Some(b) => Role::Button(b),
                        None if layout.in_stick_zone(s.pos) && self.stick_anchor.is_none() => Role::Stick,
                        None if layout.in_stick_zone(s.pos) => Role::Ignored,
                        None => Role::Look,
                    };
                    match role {
                        Role::Button(b) => presses.push((s.id, b)),
                        Role::Stick => {
                            self.stick_anchor = Some(s.pos);
                            self.stick_pos = s.pos;
                        }
                        _ => {}
                    }
                    self.fingers.push(Finger { id: s.id, role, last: s.pos, fresh: true });
                    if s.phase == FingerPhase::Tapped {
                        self.retire(s.id);
                    }
                }
                FingerPhase::Held | FingerPhase::Released => {
                    if s.pos.is_finite()
                        && let Some(f) = self.fingers.iter_mut().find(|f| f.id == s.id)
                    {
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
                        self.retire(s.id);
                    }
                }
                FingerPhase::Canceled => {
                    // Never a tap: a press edge of this same binding is dropped.
                    if self.fingers.iter().any(|f| f.id == s.id && f.fresh)
                        && let Some(i) = presses.iter().rposition(|(id, _)| *id == s.id)
                    {
                        presses.remove(i);
                    }
                    self.retire(s.id);
                }
            }
        }

        // Press edges that survived the frame.
        let mut taps: u32 = 0;
        for (_, b) in &presses {
            taps |= b.bit();
            self.press(*b, &mut out);
        }

        // Stick: the input reads the true anchor; the ring draws clamped.
        if let Some(anchor) = self.stick_anchor {
            let anchor = follow_stick_origin(anchor, self.stick_pos, layout.stick_radius, dt);
            self.stick_anchor = Some(anchor);
            let v = stick_vector(anchor, self.stick_pos, layout.stick_radius);
            out.stick_raw = v;
            out.move_axes = touch_move_axes(v);
            let ring = layout.clamp_stick_origin(anchor);
            let puck = ring + Vec2::new(v.x, -v.y) * layout.stick_radius;
            out.stick = Some((ring, puck));
            out.stick_anchor = Some(anchor);
        }

        // Held buttons.
        for f in &self.fingers {
            if let Role::Button(b) = f.role {
                out.pressed |= b.bit();
            }
        }
        // Held, or tapped within this one frame.
        out.fire = (out.pressed | taps) & TouchButton::Fire.bit() != 0;
        let jump_button = out.pressed & TouchButton::Jump.bit() != 0;

        // Auto-bhop: the stick rim (or the latch while moving) holds jump.
        if !cfg.auto_bhop {
            self.bhop_latch = false;
        }
        out.auto_bhop_latched = self.bhop_latch;
        let rim = cfg.auto_bhop && self.stick_anchor.is_some() && out.stick_raw.length() >= AUTO_BHOP_RIM;
        let latched = cfg.auto_bhop && self.bhop_latch && out.move_axes != Vec2::ZERO;
        out.jump = jump_button || rim || latched;
        out.jump_pressed |= out.jump && !self.prev_jump;
        self.prev_jump = out.jump;

        // Look.
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
            TouchButton::AutoBhop => {
                self.bhop_latch = !self.bhop_latch;
                out.auto_bhop_latched = self.bhop_latch;
            }
            TouchButton::Slot(i) => out.weapon_slot = Some(i as usize),
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-fire
// ---------------------------------------------------------------------------

/// Distance to the nearest `(center, radius)` target inside
/// [`AUTO_FIRE_CONE_RAD`] of the view ray, within `range` and with a clear
/// line of sight (`None`: hold fire).
pub fn auto_fire_target(grid: Option<&LevelGrid>, eye: Vec3, fwd: Vec3, range: f32, targets: impl Iterator<Item = (Vec3, f32)>) -> Option<f32> {
    if !eye.is_finite() || !fwd.is_finite() || fwd == Vec3::ZERO {
        return None;
    }
    targets
        .filter(|&(c, r)| {
            c.is_finite()
                && c.distance_squared(eye) <= range * range
                && silhouette_angle(eye, fwd, c, r) <= AUTO_FIRE_CONE_RAD
                && grid.is_none_or(|g| crate::physics::has_line_of_sight(g, eye, c))
        })
        .map(|(c, _)| c.distance(eye))
        .min_by(f32::total_cmp)
}

/// Where a shot along the view ray first lands (m): the nearest wall / floor
/// within `max`, or the nearest `blocker` sphere the ray crosses (whatever
/// it is: shielded demons stop rockets too).
pub fn auto_fire_first_impact(grid: Option<&LevelGrid>, eye: Vec3, fwd: Vec3, max: f32, blockers: impl Iterator<Item = (Vec3, f32)>) -> f32 {
    let Some(dir) = fwd.try_normalize() else { return 0.0 };
    let wall = grid.and_then(|g| crate::physics::raycast_grid(g, eye, dir, max)).map_or(max, |h| h.distance);
    blockers.filter_map(|(c, r)| ray_sphere(eye, dir, c, r)).fold(wall, f32::min)
}

/// The Signal-Mortar may auto-fire only when its first impact lands past
/// its own splash (manual FIRE stays unrestricted: rocket jumps).
pub fn rocket_auto_fire_safe(first_impact: f32, splash_radius: f32) -> bool {
    first_impact.is_finite() && first_impact > splash_radius.max(0.0) + ROCKET_AUTO_FIRE_MARGIN
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
    key || (mouse && mouse_is_real(fingers_down, since_touch))
}

/// The mouse is a real mouse (not a tap's compatibility replay): no finger
/// down and the [`TOUCH_MOUSE_GUARD_SECS`] guard expired.
pub fn mouse_is_real(fingers_down: usize, since_touch: f32) -> bool {
    fingers_down == 0 && since_touch >= TOUCH_MOUSE_GUARD_SECS
}

/// The `TouchMode::On` mouse-as-finger edge this frame. `buttons` =
/// `(just_pressed, pressed, just_released)` of the left button (`None`: no
/// cursor over the window). Only a real mouse ([`mouse_is_real`]) drives it,
/// so a tap's compatibility mouse never double-triggers a control; a mouse
/// finger still bound when the guard kicks in is canceled (never a tap).
pub fn mouse_finger_edge(real: bool, bound: bool, buttons: Option<(bool, bool, bool)>) -> Option<FingerPhase> {
    if !real {
        return bound.then_some(FingerPhase::Canceled);
    }
    match buttons? {
        (true, false, _) => Some(FingerPhase::Tapped),
        (true, true, _) => Some(FingerPhase::Pressed),
        (false, true, _) => Some(FingerPhase::Held),
        (false, false, true) => Some(FingerPhase::Released),
        _ => None,
    }
}

/// Whether the touch HUD draws: a run, touch not switched off, and touch mode
/// (or forced on).
pub fn overlay_visible(state: AppState, touch_mode: TouchMode, mode: InputMode) -> bool {
    state == AppState::Playing
        && match touch_mode {
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
// Interruptions (focus loss, suspension, Android ResetAll)
// ---------------------------------------------------------------------------

/// Contacts interrupted by a focus loss / suspension / Android `ResetAll`:
/// their later edges are ignored until the id touches down again.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaleContacts {
    ids: Vec<u64>,
    /// Focus is away: only a fresh touch-down (or focus coming back) resumes.
    pub suspended: bool,
}

impl StaleContacts {
    /// Mark every contact in `down` stale (and suspend until focus returns
    /// when `suspend`).
    pub fn interrupt(&mut self, down: impl IntoIterator<Item = u64>, suspend: bool) {
        for id in down {
            if !self.ids.contains(&id) {
                self.ids.push(id);
            }
        }
        self.suspended |= suspend;
    }

    /// Focus came back.
    pub fn resume(&mut self) {
        self.suspended = false;
    }

    /// Whether `s` passes. A touch-down always passes (and clears its id's
    /// stale mark and the suspension: the player is touching again); every
    /// other edge of a stale id is dropped (its lift retires the mark).
    pub fn admit(&mut self, s: &FingerSample) -> bool {
        let stale = self.ids.iter().position(|id| *id == s.id);
        match s.phase {
            FingerPhase::Pressed | FingerPhase::Tapped => {
                if let Some(i) = stale {
                    self.ids.remove(i);
                }
                self.suspended = false;
                true
            }
            FingerPhase::Released | FingerPhase::Canceled if stale.is_some() => {
                self.ids.retain(|id| *id != s.id);
                false
            }
            _ => stale.is_none() && !self.suspended,
        }
    }

    pub fn is_stale(&self, id: u64) -> bool {
        self.ids.contains(&id)
    }
}

/// An interruption queued by a platform bridge (the Android `ResetAll`).
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TouchInterrupt {
    pub pending: bool,
}

impl TouchInterrupt {
    pub fn request(&mut self) {
        self.pending = true;
    }
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
    /// Contacts cut off by a focus loss / suspension / `ResetAll`.
    pub stale: StaleContacts,
    /// An interruption landed this frame (menu taps are disarmed too).
    pub interrupted: bool,
    /// [`merge_touch_into_pad`] / auto-fire wrote into [`GamepadInput`] last frame.
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
            stale: StaleContacts::default(),
            interrupted: false,
            pad_owned: false,
        }
    }
}

impl TouchInput {
    /// Cut every contact off (focus loss, suspension, Android `ResetAll`):
    /// bindings and this frame's output are dropped (the latch survives) and
    /// the contacts in `down` are ignored until they touch down again.
    pub fn interrupt(&mut self, down: impl IntoIterator<Item = u64>, suspend: bool) {
        self.router.release_all();
        self.frame = TouchFrame { auto_bhop_latched: self.frame.auto_bhop_latched, ..default() };
        self.auto_fire = false;
        self.stale.interrupt(down, suspend);
        self.interrupted = true;
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
    /// Activation: mouse released, or an armed finger lifted (never after a
    /// cancel or a claimed drag).
    pub released: bool,
    /// A finger drives it this frame.
    pub touch: bool,
    /// Extra hit margin around targets (physical px; 0 for the mouse).
    pub slop: f32,
    /// Physical px where the press in progress (or just released) started:
    /// a click needs it on the same target.
    pub press_origin: Option<Vec2>,
    /// The finger driving it (`None` for the mouse).
    pub finger: Option<u64>,
    /// A drag owner (the Bestiary turntable) claimed this press: its lift
    /// never clicks.
    pub drag_claimed: bool,
    /// Window scale factor (logical -> physical px; 0 reads as 1).
    pub scale: f32,
}

impl UiPointer {
    fn k(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 }
    }

    /// Whether physical `p` is over `node`, a finger also within `slop`
    /// (physical px, per axis) of its edges.
    fn hits(&self, p: Vec2, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform, slop: Vec2) -> bool {
        node.contains_point(*tf, p) || (self.touch && slop.max_element() > 0.0 && within_slop_xy(p, tf.translation, node.size(), slop))
    }

    /// Whether the pointer is over `node` (a finger also within [`Self::slop`]
    /// of its edges).
    pub fn over(&self, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform) -> bool {
        self.position.is_some_and(|p| self.hits(p, node, tf, Vec2::splat(self.slop)))
    }

    /// [`Self::over`] with a per-axis finger slop in **logical** px.
    pub fn over_slop(&self, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform, slop: Vec2) -> bool {
        self.position.is_some_and(|p| self.hits(p, node, tf, slop * self.k()))
    }

    /// A click on `node`: released over it, and pressed on it too.
    pub fn clicks(&self, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform) -> bool {
        self.clicks_slop(node, tf, Vec2::splat(self.slop / self.k()))
    }

    /// [`Self::clicks`] with a per-axis finger slop in **logical** px.
    pub fn clicks_slop(&self, node: &bevy::ui::ComputedNode, tf: &bevy::ui::UiGlobalTransform, slop: Vec2) -> bool {
        let s = slop * self.k();
        self.released
            && !self.drag_claimed
            && self.position.is_some_and(|p| self.hits(p, node, tf, s))
            && self.press_origin.is_none_or(|o| self.hits(o, node, tf, s))
    }

    /// Squared distance (physical px) from the pointer to `tf`'s center.
    pub fn distance_sq(&self, tf: &bevy::ui::UiGlobalTransform) -> f32 {
        self.position.map_or(f32::INFINITY, |p| p.distance_squared(tf.translation))
    }

    /// Among `items`, the one under the pointer whose center is nearest
    /// (grown finger plates can overlap: the closest wins, never list order).
    pub fn nearest_over<'a, T>(&self, items: impl IntoIterator<Item = (T, &'a bevy::ui::ComputedNode, &'a bevy::ui::UiGlobalTransform)>, slop: Option<Vec2>) -> Option<T> {
        items
            .into_iter()
            .filter(|(_, n, tf)| match slop {
                Some(s) => self.over_slop(n, tf, s),
                None => self.over(n, tf),
            })
            .map(|(t, _, tf)| (self.distance_sq(tf), t))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, t)| t)
    }

    /// Among `items`, the one this frame's click activates (nearest center).
    pub fn nearest_click<'a, T>(&self, items: impl IntoIterator<Item = (T, &'a bevy::ui::ComputedNode, &'a bevy::ui::UiGlobalTransform)>, slop: Option<Vec2>) -> Option<T> {
        if !self.released || self.drag_claimed {
            return None;
        }
        items
            .into_iter()
            .filter(|(_, n, tf)| match slop {
                Some(s) => self.clicks_slop(n, tf, s),
                None => self.clicks(n, tf),
            })
            .map(|(t, _, tf)| (self.distance_sq(tf), t))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, t)| t)
    }

    /// A drag owner takes this press over: its lift never clicks anything.
    pub fn claim_drag(&mut self) {
        self.drag_claimed = true;
        self.released = false;
    }
}

/// `p` inside the `size` box centered at `center`, grown by `slop` (all px).
pub fn within_slop(p: Vec2, center: Vec2, size: Vec2, slop: f32) -> bool {
    within_slop_xy(p, center, size, Vec2::splat(slop))
}

/// [`within_slop`] with a per-axis slop.
pub fn within_slop_xy(p: Vec2, center: Vec2, size: Vec2, slop: Vec2) -> bool {
    size.x > 0.0 && size.y > 0.0 && (p - center).abs().cmple(size * 0.5 + slop.max(Vec2::ZERO)).all()
}

/// The mouse half of a [`resolve_ui_pointer`] input.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MousePointer {
    /// Physical px.
    pub position: Option<Vec2>,
    pub pressed: bool,
    pub just_pressed: bool,
    pub just_released: bool,
    /// Physical px where the press in progress started.
    pub origin: Option<Vec2>,
    /// A drag owner claimed the press in progress.
    pub claimed: bool,
}

/// The finger half of a [`resolve_ui_pointer`] input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FingerPointer {
    pub id: u64,
    /// Logical px (Bevy's `Touch::position`).
    pub position: Vec2,
    pub phase: FingerPhase,
    /// It touched down on this screen (a finger carried over from gameplay
    /// or a previous screen never activates anything when it lifts).
    pub armed: bool,
    /// Logical px where it touched down (on this screen).
    pub origin: Option<Vec2>,
    /// A drag owner claimed it.
    pub claimed: bool,
}

impl FingerPointer {
    /// An armed finger pressed and lifted at `position` (tests, tours).
    pub fn at(id: u64, position: Vec2, phase: FingerPhase) -> Self {
        FingerPointer { id, position, phase, armed: true, origin: Some(position), claimed: false }
    }
}

/// One frame of the menu pointer. A finger wins over the mouse; within the
/// synthetic-mouse `guard` the mouse is ignored entirely (no hover, no click),
/// so one tap never fires twice. Touch positions are logical px: they are
/// multiplied by the window `scale` to match the physical cursor space.
pub fn resolve_ui_pointer(mouse: MousePointer, finger: Option<FingerPointer>, guard: bool, scale: f32) -> UiPointer {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    if let Some(f) = finger {
        let down = matches!(f.phase, FingerPhase::Pressed | FingerPhase::Held);
        let lift = matches!(f.phase, FingerPhase::Released | FingerPhase::Tapped);
        return UiPointer {
            position: Some(f.position * scale),
            logical: Some(f.position),
            pressed: down,
            just_pressed: matches!(f.phase, FingerPhase::Pressed | FingerPhase::Tapped),
            released: f.armed && lift && !f.claimed,
            touch: true,
            slop: TOUCH_UI_SLOP * scale,
            press_origin: f.origin.map(|o| o * scale),
            finger: Some(f.id),
            drag_claimed: f.claimed,
            scale,
        };
    }
    if guard {
        return UiPointer { scale, ..default() };
    }
    UiPointer {
        position: mouse.position,
        logical: mouse.position.map(|p| p / scale),
        pressed: mouse.pressed,
        just_pressed: mouse.just_pressed,
        released: mouse.just_released && !mouse.claimed,
        touch: false,
        slop: 0.0,
        press_origin: mouse.origin,
        finger: None,
        drag_claimed: mouse.claimed,
        scale,
    }
}

/// Which screen a menu tap belongs to: armed taps die when it changes (an
/// `AppState`, a Settings page, the controls overlay).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiScreenKey {
    pub state: Option<AppState>,
    pub settings_tab: Option<crate::meta_ui::SettingsTab>,
    pub controls_open: bool,
}

/// One armed menu finger.
#[derive(Debug, Clone, Copy, PartialEq)]
struct UiArm {
    id: u64,
    /// Logical px where it touched down.
    start: Vec2,
    claimed: bool,
}

/// Bookkeeping behind [`UiPointer`] (armed fingers, the mouse press).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct UiPointerTrack {
    screen: UiScreenKey,
    arms: Vec<UiArm>,
    mouse_origin: Option<Vec2>,
    mouse_claimed: bool,
}

impl UiPointerTrack {
    /// Disarm every finger and the mouse press (screen change, interruption).
    pub fn disarm(&mut self) {
        self.arms.clear();
        self.mouse_origin = None;
        self.mouse_claimed = false;
    }

    /// Whether finger `id` is armed.
    pub fn is_armed(&self, id: u64) -> bool {
        self.arms.iter().any(|a| a.id == id)
    }

    /// Fold one frame of finger edges (arrival order) into the arms, and pick
    /// the menu finger: a lifting one first (its lift is the click; armed
    /// before unarmed), else the lowest id in `down` (fingers still down,
    /// with logical positions), else a canceled one (which never clicks).
    pub fn fold(&mut self, edges: &[FingerSample], down: &[(u64, Vec2)]) -> Option<FingerPointer> {
        let mut started: Vec<u64> = Vec::new();
        let mut lift: Option<FingerPointer> = None;
        let mut canceled: Option<FingerPointer> = None;
        for s in edges {
            match s.phase {
                FingerPhase::Pressed | FingerPhase::Tapped => {
                    self.arms.retain(|a| a.id != s.id);
                    self.arms.push(UiArm { id: s.id, start: s.pos, claimed: false });
                    started.push(s.id);
                    if s.phase == FingerPhase::Tapped {
                        let arm = self.arms.pop();
                        let f = FingerPointer { id: s.id, position: s.pos, phase: FingerPhase::Tapped, armed: true, origin: arm.map(|a| a.start), claimed: false };
                        if lift.is_none_or(|l| !l.armed) {
                            lift = Some(f);
                        }
                    }
                }
                FingerPhase::Released => {
                    let arm = self.arms.iter().position(|a| a.id == s.id).map(|i| self.arms.remove(i));
                    let phase = if started.contains(&s.id) { FingerPhase::Tapped } else { FingerPhase::Released };
                    let f = FingerPointer {
                        id: s.id,
                        position: s.pos,
                        phase,
                        armed: arm.is_some(),
                        origin: arm.map(|a| a.start),
                        claimed: arm.is_some_and(|a| a.claimed),
                    };
                    if lift.is_none_or(|l| !l.armed) {
                        lift = Some(f);
                    }
                }
                FingerPhase::Canceled => {
                    self.arms.retain(|a| a.id != s.id);
                    if canceled.is_none() {
                        canceled = Some(FingerPointer { id: s.id, position: s.pos, phase: FingerPhase::Canceled, armed: false, origin: None, claimed: false });
                    }
                }
                FingerPhase::Held => {}
            }
        }
        if lift.is_some() {
            return lift;
        }
        if let Some(&(id, pos)) = down.iter().min_by_key(|(id, _)| *id) {
            let arm = self.arms.iter().find(|a| a.id == id).copied();
            let phase = if started.contains(&id) { FingerPhase::Pressed } else { FingerPhase::Held };
            return Some(FingerPointer { id, position: pos, phase, armed: arm.is_some(), origin: arm.map(|a| a.start), claimed: arm.is_some_and(|a| a.claimed) });
        }
        canceled.map(|c| FingerPointer { position: if c.position.is_finite() { c.position } else { Vec2::ZERO }, ..c })
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

/// This frame's raw touch messages as finger edges, in arrival order.
pub fn finger_edges<'a>(events: impl IntoIterator<Item = &'a TouchEvent>, out: &mut Vec<FingerSample>) {
    out.clear();
    out.extend(events.into_iter().map(|e| FingerSample { id: e.id, pos: e.position, phase: FingerPhase::from_touch(e.phase) }));
}

/// Window logical size + scale factor, or the headless fallback.
fn viewport_of(window: Option<&Window>, fallback: Vec2) -> (Vec2, f32) {
    match window {
        Some(w) if w.width() > 1.0 && w.height() > 1.0 => (Vec2::new(w.width(), w.height()), w.scale_factor()),
        _ => (fallback, 1.0),
    }
}

/// This frame's focus / lifecycle messages as `(lost, back)`: focus went
/// away (window blur, `WillSuspend` / `Suspended`), or came back (and stayed).
pub fn focus_edge(focused: impl IntoIterator<Item = bool>, lifecycle: impl IntoIterator<Item = AppLifecycle>) -> (bool, bool) {
    let mut lost = false;
    let mut back = false;
    for f in focused {
        if f {
            back = true;
        } else {
            lost = true;
            back = false;
        }
    }
    for l in lifecycle {
        match l {
            AppLifecycle::WillSuspend | AppLifecycle::Suspended => {
                lost = true;
                back = false;
            }
            AppLifecycle::Running | AppLifecycle::WillResume => back = true,
            AppLifecycle::Idle => {}
        }
    }
    (lost, back)
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
    (mut events, mut focus, mut lifecycle, mut bridge): (MessageReader<TouchEvent>, MessageReader<WindowFocused>, MessageReader<AppLifecycle>, Option<ResMut<TouchInterrupt>>),
    windows: Query<&Window, With<PrimaryWindow>>,
    (state, store, mode, insets): (Option<Res<State<AppState>>>, Option<Res<ProfileStore>>, Option<Res<InputMode>>, Option<Res<SafeAreaInsets>>),
    (run, signal, ext, captured): (Option<Res<RunStats>>, Option<Res<SignalState>>, Option<Res<ExtractionState>>, Option<Res<CursorCaptured>>),
    mut input: ResMut<TouchInput>,
    mut activity: ResMut<TouchActivity>,
    mut samples: Local<Vec<FingerSample>>,
) {
    let dt = time.delta_secs();
    let down_ids = || touches.as_deref().map(|t| t.iter().map(|t| t.id()).collect::<Vec<_>>()).unwrap_or_default();

    // Interruptions first: whatever was down is stale from here on.
    input.interrupted = false;
    let (lost, back) = focus_edge(focus.read().map(|f| f.focused), lifecycle.read().copied());
    let reset_all = bridge.as_deref_mut().is_some_and(|b| std::mem::take(&mut b.pending));
    if lost || reset_all {
        let down = down_ids();
        input.interrupt(down, lost);
    }
    if back {
        input.stale.resume();
    }

    finger_edges(events.read(), &mut samples);
    let stale = &mut input.stale;
    samples.retain(|s| stale.admit(s));
    // A bound contact that vanished without an edge (a lost message): canceled.
    let present = down_ids();
    let missing: Vec<u64> = input.router.bound_ids().filter(|id| *id != MOUSE_FINGER_ID && !present.contains(id) && !samples.iter().any(|s| s.id == *id)).collect();
    samples.extend(missing.into_iter().map(|id| FingerSample { id, pos: Vec2::NAN, phase: FingerPhase::Canceled }));

    let fingers = present.iter().filter(|id| !input.stale.is_stale(**id)).count();
    // A finger resting still sends no edges but is still touching.
    let touched = !samples.is_empty() || fingers > 0;
    activity.touched = touched;
    activity.fingers = fingers;
    activity.since_touch = if touched || fingers > 0 { 0.0 } else { activity.since_touch + dt.max(0.0) };
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
    // whenever the cursor is free (it never is captured in touch mode) and it
    // is a real mouse: never while a finger is down or inside the
    // compatibility-mouse guard after a real touch (no double triggers).
    if settings.touch_mode == TouchMode::On && !captured.as_deref().is_some_and(|c| c.0) {
        let real = mouse_is_real(fingers, activity.since_touch);
        let pos = window.and_then(|w| w.cursor_position());
        let buttons = mouse.as_deref().filter(|_| pos.is_some()).map(|m| (m.just_pressed(MouseButton::Left), m.pressed(MouseButton::Left), m.just_released(MouseButton::Left)));
        if let Some(phase) = mouse_finger_edge(real, input.router.is_bound(MOUSE_FINGER_ID), buttons) {
            samples.push(FingerSample { id: MOUSE_FINGER_ID, pos: pos.unwrap_or(Vec2::NAN), phase });
        }
    } else if input.router.is_bound(MOUSE_FINGER_ID) {
        samples.push(FingerSample { id: MOUSE_FINGER_ID, pos: Vec2::NAN, phase: FingerPhase::Canceled });
    }

    input.active = !input.stale.suspended && router_active(state, settings.touch_mode, mode, touched);
    if input.active {
        let cfg = RouterConfig { sensitivity: settings.touch_sensitivity, auto_bhop: settings.touch_auto_bhop };
        let layout = input.layout.clone();
        input.frame = input.router.step(&samples, &layout, cfg, dt);
    } else {
        input.router.release_all();
        input.frame = TouchFrame { auto_bhop_latched: input.router.bhop_latch && settings.touch_auto_bhop, ..default() };
    }
}

/// A new run (or any return to `Playing`) starts with no touch state and the
/// AUTO-BHOP latch off.
pub fn reset_touch_router(mut input: ResMut<TouchInput>) {
    input.router.reset();
    input.frame = TouchFrame::default();
    input.auto_fire = false;
}

/// World-space `(center, radius)` of `spheres` (`(height, radius)` above `t`).
fn at_spheres(t: &Transform, spheres: &'static [(f32, f32)]) -> impl Iterator<Item = (Vec3, f32)> + use<> {
    let base = t.translation;
    spheres.iter().map(move |&(h, r)| (base + Vec3::Y * h, r))
}

type AutoFireEnemies<'w, 's> = Query<'w, 's, (&'static Transform, &'static Enemy), (Without<Player>, Without<PlayerCamera>)>;
type AutoFireJammers<'w, 's> = Query<'w, 's, (&'static Transform, &'static DemonJammer), (Without<Player>, Without<PlayerCamera>, Without<Enemy>)>;
type AutoFirePylons<'w, 's> = Query<'w, 's, (&'static Transform, &'static BossPylon), (Without<Player>, Without<PlayerCamera>, Without<Enemy>, Without<DemonJammer>)>;

/// Optional touch auto-fire (`touch_auto_fire`): a live, hittable demon /
/// jammer / Shield Pylon inside the view cone, in weapon range, in line of
/// sight. Runs in `Update` right before `fire_weapons`, after `mouse_look` /
/// `move_player` moved the eye, so it tests the exact pose that fires.
/// Shielded (immune) demons never trigger it; the Signal-Mortar never fires
/// into its own splash.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_touch_auto_fire(
    mut input: ResMut<TouchInput>,
    mut pad: ResMut<GamepadInput>,
    (store, ws, mods): (Option<Res<ProfileStore>>, Option<Res<WeaponState>>, Option<Res<PlayerModifiers>>),
    grid: Option<Res<LevelGrid>>,
    players: Query<(&Player, &Transform), Without<PlayerCamera>>,
    cams: Query<&Transform, With<PlayerCamera>>,
    (enemies, jammers, pylons): (AutoFireEnemies, AutoFireJammers, AutoFirePylons),
) {
    input.auto_fire = false;
    if !input.active || input.stale.suspended || !live_settings(store.as_deref()).touch_auto_fire {
        return;
    }
    let (Ok((p, body)), Some(ws)) = (players.single(), ws) else { return };
    if !auto_fire_allowed(!p.is_dead(), ws.current.has_ammo(p), ws.swap_timer) {
        return;
    }
    let eye_tf = live_eye_pose(body, &cams.single().copied().unwrap_or_default());
    let (eye, fwd) = (eye_tf.translation, eye_tf.rotation * Vec3::NEG_Z);
    let range = auto_fire_range(ws.current);
    // Shielded (immune) demons are never targets; live pylons are.
    let demons = enemies.iter().filter(|(_, e)| e.is_alive() && !e.shielded).flat_map(|(t, e)| at_spheres(t, e.stats.hit_spheres));
    let towers = jammers.iter().filter(|(_, j)| j.hp > 0.0).flat_map(|(t, _)| at_spheres(t, &JAMMER_SPHERES));
    let shields = pylons.iter().filter(|(_, py)| py.is_alive()).flat_map(|(t, _)| at_spheres(t, &PYLON_SPHERES));
    if auto_fire_target(grid.as_deref(), eye, fwd, range, demons.chain(towers).chain(shields)).is_none() {
        return;
    }
    if ws.current == Weapon::Rocket {
        let default_mods = PlayerModifiers::default();
        let splash = rocket_splash_radius(mods.as_deref().unwrap_or(&default_mods));
        // Anything alive stops a rocket, shielded or not.
        let demons = enemies.iter().filter(|(_, e)| e.is_alive()).flat_map(|(t, e)| at_spheres(t, e.stats.hit_spheres));
        let towers = jammers.iter().filter(|(_, j)| j.hp > 0.0).flat_map(|(t, _)| at_spheres(t, &JAMMER_SPHERES));
        let shields = pylons.iter().filter(|(_, py)| py.is_alive()).flat_map(|(t, _)| at_spheres(t, &PYLON_SPHERES));
        let impact = auto_fire_first_impact(grid.as_deref(), eye, fwd, range, demons.chain(towers).chain(shields));
        if !rocket_auto_fire_safe(impact, splash) {
            return;
        }
    }
    input.auto_fire = true;
    // Holds the trigger without a press edge (never an empty-gun dry click).
    pad.fire = true;
    input.pad_owned = true;
}

/// Fold the touch controls into [`GamepadInput`] (see the module docs). With
/// no controller connected the touch fields are owned here: last frame's
/// contribution (incl. auto-fire's) is cleared first, so nothing sticks after
/// a finger lifts or focus is lost.
pub fn merge_touch_into_pad(mut input: ResMut<TouchInput>, mut pad: ResMut<GamepadInput>) {
    if !pad.connected && input.pad_owned {
        clear_touch_fields(&mut pad);
    }
    let t = &input.frame;
    let contributes = input.active && !input.stale.suspended && t.any_action();
    if contributes {
        let p = &mut *pad;
        p.move_axes = (p.move_axes + t.move_axes).clamp(Vec2::splat(-1.0), Vec2::splat(1.0));
        p.look_delta += t.look_delta;
        p.look_mag = p.look_mag.max(t.look_mag);
        p.fire |= t.fire;
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
/// only when they touch down on the current screen (state, Settings page,
/// controls overlay), a click needs the press to have started on the same
/// target, and a cancel / interruption / claimed drag never clicks.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_ui_pointer(
    (touches, mouse): (Option<Res<Touches>>, Option<Res<ButtonInput<MouseButton>>>),
    mut events: MessageReader<TouchEvent>,
    windows: Query<&Window, With<PrimaryWindow>>,
    (state, tab, controls): (Option<Res<State<AppState>>>, Option<Res<crate::meta_ui::SettingsTab>>, Option<Res<crate::hud::ControlsOverlayOpen>>),
    (activity, input): (Res<TouchActivity>, Res<TouchInput>),
    mut ptr: ResMut<UiPointer>,
    mut track: ResMut<UiPointerTrack>,
    mut edges: Local<Vec<FingerSample>>,
) {
    // A drag owner claimed last frame's press: remember it until the lift.
    if ptr.drag_claimed {
        match ptr.finger {
            Some(id) => {
                if let Some(a) = track.arms.iter_mut().find(|a| a.id == id) {
                    a.claimed = true;
                }
            }
            None => track.mouse_claimed = true,
        }
    }
    let screen = UiScreenKey { state: state.map(|s| *s.get()), settings_tab: tab.map(|t| *t), controls_open: controls.is_some_and(|c| c.0) };
    if track.screen != screen || input.interrupted {
        track.screen = screen;
        track.disarm();
    }
    finger_edges(events.read(), &mut edges);
    edges.retain(|s| !input.stale.is_stale(s.id) || matches!(s.phase, FingerPhase::Pressed | FingerPhase::Tapped));
    let down: Vec<(u64, Vec2)> = touches.as_deref().map(|t| t.iter().filter(|t| !input.stale.is_stale(t.id())).map(|t| (t.id(), t.position())).collect()).unwrap_or_default();
    let finger = track.fold(&edges, &down);

    let window = windows.single().ok();
    let m = mouse.as_deref();
    let position = window.and_then(|w| w.physical_cursor_position());
    let just_pressed = m.is_some_and(|m| m.just_pressed(MouseButton::Left));
    if just_pressed {
        track.mouse_origin = position;
        track.mouse_claimed = false;
    }
    let pressed = m.is_some_and(|m| m.pressed(MouseButton::Left));
    let just_released = m.is_some_and(|m| m.just_released(MouseButton::Left));
    let mouse = MousePointer { position, pressed, just_pressed, just_released, origin: track.mouse_origin, claimed: track.mouse_claimed };
    if !pressed && !just_pressed {
        track.mouse_origin = None;
        track.mouse_claimed = false;
    }
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

/// Mobile web: CSS `env(safe-area-inset-*)` (read from the zero-size
/// `#safe-area-probe` element in `index.html`) -> [`SafeAreaInsets`],
/// converted once to physical px (x the window scale factor, the browser's
/// `devicePixelRatio`), like Android's cutout insets. Re-read on resize /
/// orientation change (and twice a second for late `viewport-fit` updates).
#[cfg(target_arch = "wasm32")]
pub fn sync_web_safe_area(
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut resized: MessageReader<bevy::window::WindowResized>,
    mut insets: ResMut<SafeAreaInsets>,
    mut since: Local<f32>,
) {
    let changed = resized.read().count() > 0;
    *since += time.delta_secs();
    if !changed && *since < 0.5 {
        return;
    }
    *since = 0.0;
    let Some(w) = web_sys::window() else { return };
    let Some(el) = w.document().and_then(|d| d.get_element_by_id("safe-area-probe")) else { return };
    let Ok(Some(style)) = w.get_computed_style(&el) else { return };
    let get = |p: &str| style.get_property_value(p).map(|v| parse_css_px(&v)).unwrap_or(0.0);
    let scale = windows.single().map_or(1.0, |w| w.scale_factor());
    let want = SafeAreaInsets::from_css(get("padding-top"), get("padding-right"), get("padding-bottom"), get("padding-left"), scale);
    if *insets != want {
        *insets = want;
    }
}

/// Registered by `CoreGameplayPlugin` (simulation + headless tests).
pub struct TouchPlugin;

impl Plugin for TouchPlugin {
    fn build(&self, app: &mut App) {
        // `bevy_input` / `bevy_window` / `bevy_winit` register these in the
        // game; bare test apps may lack them.
        if !app.world().contains_resource::<Messages<TouchEvent>>() {
            app.add_message::<TouchEvent>();
        }
        if !app.world().contains_resource::<Messages<WindowFocused>>() {
            app.add_message::<WindowFocused>();
        }
        if !app.world().contains_resource::<Messages<AppLifecycle>>() {
            app.add_message::<AppLifecycle>();
        }
        app.init_resource::<TouchInput>()
            .init_resource::<TouchActivity>()
            .init_resource::<UiPointer>()
            .init_resource::<UiPointerTrack>()
            .init_resource::<SafeAreaInsets>()
            .init_resource::<BackRequest>()
            .init_resource::<TouchInterrupt>()
            .add_systems(
                PreUpdate,
                (update_touch_input, update_ui_pointer, merge_touch_into_pad)
                    .chain()
                    .after(InputSystems)
                    .after(update_gamepad_input)
                    .before(crate::gamepad::sync_input_mode),
            )
            .add_systems(PreUpdate, apply_back_request.after(crate::gamepad::sync_input_mode).after(InputSystems))
            .add_systems(OnEnter(AppState::Playing), reset_touch_router)
            .add_systems(Update, update_touch_auto_fire.in_set(GameSet::Combat).before(crate::weapons::CombatDamage));
        #[cfg(target_arch = "wasm32")]
        app.add_systems(PreUpdate, sync_web_safe_area.before(update_touch_input));
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
            // Mobile web portrait (iPhone 12-15) with its notch / home bar.
            (Vec2::new(390.0, 844.0), SafeAreaInsets::new(0.0, 47.0, 0.0, 34.0)),
            (Vec2::new(390.0, 844.0), SafeAreaInsets::default()),
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
                // The stick ring at rest never touches a plate either.
                let ring = Rect::from_center_half_size(l.stick_rest, Vec2::splat(l.stick_radius));
                for b in &l.buttons {
                    assert!(ring.intersect(b.rect()).is_empty(), "{:?} on the stick ring in {vp} @{scale}", b.button);
                }
                // Fingers: every plate >= 44 logical px at the default scale on a landscape phone.
                if scale == 1.0 && vp.x > vp.y {
                    for b in l.buttons.iter().filter(|b| b.button != TouchButton::AutoBhop) {
                        assert!(b.half.min_element() * 2.0 >= 44.0 * l.unit - 1e-3, "{:?} too small in {vp}", b.button);
                    }
                }
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
        // Anchoring near an edge clamps the drawn ring inside the safe area...
        let corner = Vec2::new(2.0, 410.0);
        let f = r.step(&[press(2, corner)], &l, RouterConfig::default(), DT);
        let o = f.stick.unwrap().0;
        assert!(o.x - l.stick_radius >= l.safe.min.x - 1e-3 && o.y + l.stick_radius <= l.safe.max.y + 1e-3, "{o}");
        // ...but the input anchor stays where the thumb landed: no strafe, no auto-bhop.
        assert_eq!(f.stick_anchor, Some(corner));
        assert!(f.move_axes == Vec2::ZERO && f.stick_raw == Vec2::ZERO && !f.jump, "{} {}", f.move_axes, f.stick_raw);
        let f = r.step(&[held(2, corner)], &l, RouterConfig::default(), DT);
        assert!(f.move_axes == Vec2::ZERO && !f.jump && !f.jump_pressed);
        // Pushing up from the corner moves forward from there.
        let f = r.step(&[held(2, corner + Vec2::new(0.0, -40.0))], &l, RouterConfig::default(), DT);
        assert!(f.move_axes.y > 0.5 && f.move_axes.x == 0.0, "{}", f.move_axes);
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
        // A finger resting still sends no edges and stays bound.
        let f = r.step(&[], &l, cfg, DT);
        assert!(f.move_axes.x > 0.5 && r.finger_count() == 2);
        // Focus loss drops everything.
        r.release_all();
        let f = r.step(&[], &l, cfg, DT);
        assert!(f.stick.is_none() && r.finger_count() == 0 && f.move_axes == Vec2::ZERO);
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
        // Focus loss keeps the latch; a new run (reset) never carries it over.
        r.step(&[press(4, pill), lift(4, pill)], &l, on, DT);
        r.release_all();
        assert!(r.step(&[], &l, on, DT).auto_bhop_latched, "release_all keeps the latch");
        r.reset();
        assert!(!r.step(&[], &l, on, DT).auto_bhop_latched, "reset clears the latch");
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
        let d = auto_fire_target(Some(&grid), eye, east, 30.0, std::iter::once(ahead));
        assert!(d.is_some_and(|d| (d - 10.0).abs() < 1e-3), "{d:?}");
        assert!(auto_fire_target(Some(&grid), eye, east, 8.0, std::iter::once(ahead)).is_none(), "out of weapon range");
        assert!(auto_fire_target(Some(&grid), eye, east, 30.0, std::iter::once((eye + Vec3::new(10.0, 0.0, 3.0), 0.6))).is_none(), "outside the cone");
        assert!(auto_fire_target(Some(&grid), eye, -east, 30.0, std::iter::once(ahead)).is_none(), "behind");
        assert!(auto_fire_target(Some(&grid), eye, east, 300.0, std::iter::once((eye + Vec3::X * 60.0, 0.6))).is_none(), "through the wall");
        assert!(auto_fire_target(None, eye, Vec3::ZERO, 30.0, std::iter::once(ahead)).is_none());
        // The nearest of several.
        let near = (eye + Vec3::X * 4.0, 0.5);
        assert!(auto_fire_target(Some(&grid), eye, east, 30.0, [ahead, near].into_iter()).is_some_and(|d| (d - 4.0).abs() < 1e-3));
        // First impact: the nearest blocker on the ray, else the wall.
        let wall = auto_fire_first_impact(Some(&grid), eye, east, 300.0, std::iter::empty());
        assert!(wall > 5.0 && wall < 300.0, "{wall}");
        let hit = auto_fire_first_impact(Some(&grid), eye, east, 300.0, [ahead, near].into_iter());
        assert!((hit - 3.5).abs() < 1e-3, "{hit}");
        // The Signal-Mortar never auto-fires into its own splash.
        assert!(!rocket_auto_fire_safe(hit, 5.5) && rocket_auto_fire_safe(10.0, 5.5));
        assert!(!rocket_auto_fire_safe(5.5 + ROCKET_AUTO_FIRE_MARGIN, 5.5) && !rocket_auto_fire_safe(f32::NAN, 5.5));
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
        let mouse = MousePointer { position: Some(Vec2::new(10.0, 10.0)), just_released: true, ..default() };
        // A finger lifting at logical (100, 50) on a 2.625x phone.
        let f = FingerPointer::at(1, Vec2::new(100.0, 50.0), FingerPhase::Released);
        let p = resolve_ui_pointer(mouse, Some(f), true, 2.625);
        assert_eq!(p.position, Some(Vec2::new(262.5, 131.25)), "logical * scale = physical");
        assert_eq!(p.press_origin, Some(Vec2::new(262.5, 131.25)));
        assert!((p.slop - TOUCH_UI_SLOP * 2.625).abs() < 1e-4, "finger slop in physical px");
        assert!(within_slop(Vec2::new(112.0, 50.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), 3.0));
        assert!(!within_slop(Vec2::new(114.0, 50.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), 3.0));
        assert!(within_slop_xy(Vec2::new(123.0, 50.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), Vec2::new(14.0, 8.0)));
        assert!(!within_slop_xy(Vec2::new(100.0, 64.0), Vec2::new(100.0, 50.0), Vec2::new(20.0, 10.0), Vec2::new(14.0, 8.0)), "less vertical slop");
        assert!(!within_slop(Vec2::ZERO, Vec2::ZERO, Vec2::ZERO, 3.0), "an unlaid-out node is never hit");
        assert!(p.released && p.touch && !p.pressed);
        // While the finger is down: press highlight, no activation.
        let p = resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Held, ..f }), true, 2.625);
        assert!(p.pressed && !p.released);
        // An unarmed finger (carried over from gameplay) never activates.
        assert!(!resolve_ui_pointer(mouse, Some(FingerPointer { armed: false, ..f }), true, 1.0).released);
        // A canceled finger never activates.
        assert!(!resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Canceled, ..f }), true, 1.0).released);
        // A claimed drag never activates.
        assert!(!resolve_ui_pointer(mouse, Some(FingerPointer { claimed: true, ..f }), true, 1.0).released);
        // A fast tap activates.
        assert!(resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Tapped, ..f }), true, 1.0).released);
        // The synthetic mouse release right after the tap is swallowed.
        assert_eq!(resolve_ui_pointer(mouse, None, true, 1.0), UiPointer { scale: 1.0, ..default() });
        // A real mouse outside the guard works as before.
        let p = resolve_ui_pointer(mouse, None, false, 2.0);
        assert!(p.released && !p.touch && p.position == Some(Vec2::new(10.0, 10.0)) && p.logical == Some(Vec2::new(5.0, 5.0)));
        assert_eq!(p.slop, 0.0, "the mouse stays pixel exact");
        assert!(!resolve_ui_pointer(MousePointer { claimed: true, ..mouse }, None, false, 1.0).released);
        // `claim_drag` kills this frame's click too.
        let mut p = resolve_ui_pointer(mouse, Some(FingerPointer { phase: FingerPhase::Tapped, ..f }), true, 1.0);
        p.claim_drag();
        assert!(!p.released && p.drag_claimed);
    }

    #[test]
    fn ui_track_arms_taps_and_never_clicks_on_cancel_or_carried_fingers() {
        let mut tr = UiPointerTrack::default();
        let a = Vec2::new(100.0, 40.0);
        // Down then up a frame later: armed, origin where it landed.
        assert_eq!(tr.fold(&[press(1, a)], &[(1, a)]).map(|f| (f.phase, f.armed)), Some((FingerPhase::Pressed, true)));
        let up = tr.fold(&[lift(1, a + Vec2::new(3.0, 0.0))], &[]).unwrap();
        assert!(up.armed && up.phase == FingerPhase::Released && up.origin == Some(a));
        // Down + up in one frame: a tap.
        let tap = tr.fold(&[press(2, a), lift(2, a)], &[]).unwrap();
        assert!(tap.armed && tap.phase == FingerPhase::Tapped);
        // Down + Canceled in one frame: never a tap.
        let c = tr.fold(&[press(3, a), FingerSample { id: 3, pos: a, phase: FingerPhase::Canceled }], &[]).unwrap();
        assert!(c.phase == FingerPhase::Canceled && !c.armed);
        assert!(!resolve_ui_pointer(MousePointer::default(), Some(c), true, 1.0).released);
        // Canceled a frame after the press: disarmed, no click.
        tr.fold(&[press(4, a)], &[(4, a)]);
        let c = tr.fold(&[FingerSample { id: 4, pos: a, phase: FingerPhase::Canceled }], &[]).unwrap();
        assert!(!resolve_ui_pointer(MousePointer::default(), Some(c), true, 1.0).released);
        // A screen change (or an interruption) disarms a finger still down.
        tr.fold(&[press(5, a)], &[(5, a)]);
        tr.disarm();
        let up = tr.fold(&[lift(5, a)], &[]).unwrap();
        assert!(!up.armed && !resolve_ui_pointer(MousePointer::default(), Some(up), true, 1.0).released);
        // A reused id lifts the old contact, then the new one arms.
        tr.fold(&[press(6, a)], &[(6, a)]);
        let f = tr.fold(&[lift(6, a), press(6, a + Vec2::X * 200.0)], &[(6, a + Vec2::X * 200.0)]).unwrap();
        assert!(f.phase == FingerPhase::Released && f.armed, "the lift is this frame's click");
        assert!(tr.is_armed(6));
    }

    #[test]
    fn stale_contacts_are_ignored_until_they_touch_down_again() {
        let mut s = StaleContacts::default();
        s.interrupt([1, 2], true);
        assert!(s.suspended && s.is_stale(1));
        assert!(!s.admit(&held(1, Vec2::ZERO)), "a stale move is dropped");
        assert!(!s.admit(&lift(1, Vec2::ZERO)), "a stale lift is dropped (never a tap)");
        assert!(!s.is_stale(1), "...and retires the mark");
        assert!(!s.admit(&held(9, Vec2::ZERO)), "suspended: nothing but a fresh touch passes");
        assert!(s.admit(&press(2, Vec2::ZERO)) && !s.suspended && !s.is_stale(2), "a fresh touch-down resumes");
        let mut s = StaleContacts::default();
        s.interrupt([3], true);
        s.resume();
        assert!(s.admit(&held(4, Vec2::ZERO)) && !s.admit(&held(3, Vec2::ZERO)), "focus back: live contacts pass, stale ones still not");
        // Focus / lifecycle edges.
        assert_eq!(focus_edge([false], []), (true, false));
        assert_eq!(focus_edge([false, true], []), (true, true));
        assert_eq!(focus_edge([], [AppLifecycle::WillSuspend]), (true, false));
        assert_eq!(focus_edge([], [AppLifecycle::Running]), (false, true));
        assert_eq!(focus_edge([], [AppLifecycle::Idle]), (false, false));
    }

    #[test]
    fn finger_edges_keep_arrival_order_and_cancels() {
        use bevy::input::touch::TouchInput as Ev;
        let w = Entity::PLACEHOLDER;
        let ev = |id, phase, x: f32| Ev { phase, position: Vec2::new(x, 10.0), window: w, force: None, id };
        let evs = [ev(2, TouchPhase::Started, 1.0), ev(1, TouchPhase::Started, 5.0), ev(2, TouchPhase::Ended, 2.0), ev(2, TouchPhase::Started, 9.0), ev(1, TouchPhase::Canceled, 5.0)];
        let mut out = Vec::new();
        finger_edges(evs.iter(), &mut out);
        assert_eq!(
            out.iter().map(|s| (s.id, s.phase)).collect::<Vec<_>>(),
            [(2, FingerPhase::Pressed), (1, FingerPhase::Pressed), (2, FingerPhase::Released), (2, FingerPhase::Pressed), (1, FingerPhase::Canceled)]
        );
        assert_eq!(out[2].pos, Vec2::new(2.0, 10.0));
        assert_eq!(FingerPhase::from_touch(TouchPhase::Moved), FingerPhase::Held);
    }

    #[test]
    fn canceled_contacts_release_without_ever_tapping() {
        let l = TouchLayout::compute(PHONE, SafeAreaInsets::default(), 1.0, &LayoutContext { auto_bhop: true, trial: true, ..default() });
        let cancel = |id, pos| FingerSample { id, pos, phase: FingerPhase::Canceled };
        for b in [TouchButton::Fire, TouchButton::Jump, TouchButton::Reset, TouchButton::Pause, TouchButton::AutoBhop, TouchButton::Overload] {
            let mut r = TouchRouter::default();
            let p = at(&l, b);
            let f = r.step(&[press(1, p), cancel(1, p)], &l, RouterConfig::default(), DT);
            assert!(!f.any_action() && f.pressed == 0 && !f.auto_bhop_latched && r.finger_count() == 0, "{b:?}: same-frame cancel is no tap: {f:?}");
        }
        // Held FIRE then canceled: the trigger lets go, no new edge.
        let mut r = TouchRouter::default();
        let fire = at(&l, TouchButton::Fire);
        assert!(r.step(&[press(1, fire)], &l, RouterConfig::default(), DT).fire);
        let f = r.step(&[cancel(1, fire)], &l, RouterConfig::default(), DT);
        assert!(!f.fire && !f.fire_just_pressed && f.pressed == 0);
        // A canceled stick thumb stops moving at once.
        let s = Vec2::new(180.0, 260.0);
        r.step(&[press(2, s), held(2, s + Vec2::new(40.0, 0.0))], &l, RouterConfig { auto_bhop: false, ..default() }, DT);
        let f = r.step(&[cancel(2, Vec2::NAN)], &l, RouterConfig::default(), DT);
        assert!(f.stick.is_none() && f.move_axes == Vec2::ZERO);
    }

    #[test]
    fn reused_finger_ids_and_same_frame_stick_handoff_route_in_order() {
        let l = layout();
        let cfg = RouterConfig { auto_bhop: false, ..default() };
        let (fire, jump) = (at(&l, TouchButton::Fire), at(&l, TouchButton::Jump));
        // FIRE lifts and the same id lands on JUMP before the next frame.
        let mut r = TouchRouter::default();
        r.step(&[press(5, fire)], &l, cfg, DT);
        let f = r.step(&[lift(5, fire), press(5, jump)], &l, cfg, DT);
        assert!(f.jump && f.jump_pressed && f.is_pressed(TouchButton::Jump) && !f.fire && !f.is_pressed(TouchButton::Fire), "{f:?}");
        assert_eq!(r.finger_count(), 1);
        // A press of an id still bound (its lift was lost) retires the old binding.
        let f = r.step(&[press(5, fire)], &l, cfg, DT);
        assert!(f.fire && f.fire_just_pressed && !f.jump);
        // A tap on JUMP + a lift of FIRE in one frame with reused ids.
        let f = r.step(&[lift(5, fire), FingerSample { id: 5, pos: jump, phase: FingerPhase::Tapped }], &l, cfg, DT);
        assert!(!f.fire && f.jump_pressed && r.finger_count() == 0);
        // Same-frame thumb swap on the stick, either arrival order: the new
        // thumb takes the stick at zero deflection.
        let (a, b) = (Vec2::new(150.0, 300.0), Vec2::new(260.0, 250.0));
        for order in [0, 1] {
            let mut r = TouchRouter::default();
            r.step(&[press(1, a), held(1, a + Vec2::new(45.0, 0.0))], &l, cfg, DT);
            let edges = if order == 0 { [lift(1, a), press(2, b)] } else { [press(2, b), lift(1, a)] };
            let f = r.step(&edges, &l, cfg, DT);
            assert_eq!(f.stick_anchor, Some(b), "order {order}");
            assert_eq!(f.move_axes, Vec2::ZERO, "order {order}");
            let f = r.step(&[held(2, b + Vec2::new(0.0, -40.0))], &l, cfg, DT);
            assert!(f.move_axes.y > 0.5, "order {order}: the new thumb drives: {}", f.move_axes);
        }
        // A resting second thumb (not this frame) is never promoted.
        let mut r = TouchRouter::default();
        r.step(&[press(1, a), press(2, b)], &l, cfg, DT);
        r.step(&[], &l, cfg, DT);
        let f = r.step(&[lift(1, a)], &l, cfg, DT);
        assert!(f.stick.is_none());
    }

    #[test]
    fn forced_on_mouse_finger_waits_out_the_compat_mouse_guard() {
        // Inside the guard (a real touch just happened): no edge at all.
        assert!(!mouse_is_real(0, 0.1) && !mouse_is_real(1, 9.0) && mouse_is_real(0, TOUCH_MOUSE_GUARD_SECS));
        assert_eq!(mouse_finger_edge(false, false, Some((true, true, false))), None, "the compat click never presses");
        assert_eq!(mouse_finger_edge(false, true, Some((false, true, false))), Some(FingerPhase::Canceled), "a held mouse finger is canceled, never tapped");
        // A real mouse drives it like a finger.
        assert_eq!(mouse_finger_edge(true, false, Some((true, true, false))), Some(FingerPhase::Pressed));
        assert_eq!(mouse_finger_edge(true, false, Some((true, false, true))), Some(FingerPhase::Tapped));
        assert_eq!(mouse_finger_edge(true, true, Some((false, true, false))), Some(FingerPhase::Held));
        assert_eq!(mouse_finger_edge(true, true, Some((false, false, true))), Some(FingerPhase::Released));
        assert_eq!(mouse_finger_edge(true, false, None), None, "no cursor over the window");
        // Routed: a real tap toggles the latch once; the compat click after it
        // (guarded, so no edge) leaves it latched.
        let l = layout();
        let pill = at(&l, TouchButton::AutoBhop);
        let mut r = TouchRouter::default();
        let f = r.step(&[FingerSample { id: 1, pos: pill, phase: FingerPhase::Tapped }], &l, RouterConfig::default(), DT);
        assert!(f.auto_bhop_latched);
        let compat: Vec<FingerSample> = mouse_finger_edge(mouse_is_real(0, 0.05), false, Some((true, false, true)))
            .map(|phase| FingerSample { id: MOUSE_FINGER_ID, pos: pill, phase })
            .into_iter()
            .collect();
        assert!(r.step(&compat, &l, RouterConfig::default(), DT).auto_bhop_latched, "no double toggle");
    }

    #[test]
    fn css_safe_area_converts_once_like_android() {
        assert_eq!(parse_css_px("47px"), 47.0);
        assert_eq!(parse_css_px(" 3.5px "), 3.5);
        assert_eq!(parse_css_px(""), 0.0);
        assert_eq!(parse_css_px("-4px"), 0.0);
        assert_eq!(parse_css_px("calc(1px)"), 0.0);
        // CSS px x DPR = physical; to_logical(DPR) gives the CSS px back.
        let s = SafeAreaInsets::from_css(47.0, 0.0, 34.0, 12.0, 3.0);
        assert_eq!(s, SafeAreaInsets::new(36.0, 141.0, 0.0, 102.0));
        assert_eq!(s.to_logical(3.0), SafeAreaInsets::new(12.0, 47.0, 0.0, 34.0));
        assert_eq!(SafeAreaInsets::from_css(f32::NAN, 1.0, 1.0, 1.0, 0.0), SafeAreaInsets::new(1.0, 0.0, 1.0, 1.0));
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
        // An interruption (focus loss) clears the merged contribution at once.
        world.resource_mut::<TouchInput>().frame = TouchFrame { fire: true, ..default() };
        world.resource_mut::<GamepadInput>().fire = false;
        sys.run((), &mut world).unwrap();
        assert!(world.resource::<GamepadInput>().fire);
        world.resource_mut::<TouchInput>().interrupt([1], true);
        sys.run((), &mut world).unwrap();
        assert!(!world.resource::<GamepadInput>().fire, "focus loss drops the held trigger");
    }
}
