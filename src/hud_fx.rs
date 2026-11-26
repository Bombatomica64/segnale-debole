//! Frameless HUD kit: the in-game HUD floats over the scene with no plates,
//! like the DMC-style rank emblem. Legibility comes from hard ink text
//! shadows, dark tracks under the gauges and razor-thin glowing accent rails;
//! life comes from short decaying envelopes ([`HudPulse`]) that pop a readout
//! when its value changes.
//!
//! Everything here is pure math or a spawn-time bundle: the per-frame HUD
//! systems only write `UiTransform` / colors that really changed, and never
//! allocate.

use bevy::prelude::*;
use bevy::ui::Val2;

/// Screen gutter of every floating HUD cluster (logical px).
pub const HUD_GUTTER: f32 = 16.0;
/// Top inset of the top-row clusters.
pub const HUD_TOP: f32 = 14.0;
/// Secondary readout tone: an opaque warm grey that still clears a hard
/// shadow over bright geometry (the menus' ash is too dark without a plate).
pub const HUD_SUB: Color = Color::srgb(0.80, 0.75, 0.67);
/// Opacity of the ink text shadow under fully opaque text.
pub const SHADOW_ALPHA: f32 = 0.96;
/// Width of the glowing accent rails / pips.
pub const RAIL_WIDTH: f32 = 3.0;

/// Drop-shadow offset for `font` px text: ~9% of the size, never thinner than
/// a full pixel at the smallest UI scale.
pub fn shadow_offset(font: f32) -> f32 {
    (font * 0.09).clamp(1.4, 2.6)
}

/// Hard ink shadow sized to the font.
pub fn hud_shadow(font: f32) -> TextShadow {
    TextShadow { offset: Vec2::splat(shadow_offset(font)), color: Color::BLACK.with_alpha(SHADOW_ALPHA) }
}

/// Shadow color under text of opacity `alpha` (fading text takes its shadow along).
pub fn shadow_color(alpha: f32) -> Color {
    Color::BLACK.with_alpha(SHADOW_ALPHA * alpha.clamp(0.0, 1.0))
}

/// Floating HUD label: one never-wrapping line on a hard ink shadow.
pub fn hud_text(s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (Text::new(s), TextFont::from_font_size(size), TextColor(color), hud_shadow(size), TextLayout::no_wrap())
}

/// Index of the colored halo in a [`glow`] shadow stack.
const GLOW_LAYER: usize = 0;

/// Accent glow for a thin rail: a soft colored halo over a crisp 1 px ink
/// edge, so the rail reads on dark walls and bright floors alike.
pub fn glow(color: Color) -> BoxShadow {
    BoxShadow(vec![
        ShadowStyle { color: glow_color(color), x_offset: Val::ZERO, y_offset: Val::ZERO, spread_radius: Val::Px(1.0), blur_radius: Val::Px(7.0) },
        ShadowStyle { color: Color::BLACK.with_alpha(0.9 * color.alpha()), x_offset: Val::Px(1.0), y_offset: Val::Px(1.0), spread_radius: Val::ZERO, blur_radius: Val::ZERO },
    ])
}

fn glow_color(color: Color) -> Color {
    color.with_alpha(0.6 * color.alpha())
}

/// Retint a [`glow`] in place (change-guarded, no allocation).
pub fn set_glow(shadow: &mut Mut<BoxShadow>, color: Color) {
    let (halo, ink) = (glow_color(color), Color::BLACK.with_alpha(0.9 * color.alpha()));
    if shadow.0.len() == 2 && (shadow.0[GLOW_LAYER].color != halo || shadow.0[1].color != ink) {
        shadow.0[GLOW_LAYER].color = halo;
        shadow.0[1].color = ink;
    }
}

/// Razor-thin glowing accent bar (a rail when tall, a rule when wide).
pub fn accent_bar(node: Node, color: Color) -> impl Bundle {
    (Node { flex_shrink: 0.0, ..node }, BackgroundColor(color), glow(color), UiTransform::IDENTITY)
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

fn env(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Two-leg keyframe curve over a decaying envelope (`1` = just triggered):
/// `from` snaps to `through` with a cubic ease-out, then settles on `rest`.
pub fn slam_curve(envelope: f32, from: f32, through: f32, rest: f32) -> f32 {
    if !envelope.is_finite() || envelope <= 0.0 {
        return rest;
    }
    let u = 1.0 - envelope.min(1.0);
    const HIT: f32 = 0.42;
    if u < HIT {
        let k = 1.0 - u / HIT;
        from + (through - from) * (1.0 - k * k * k)
    } else {
        let k = (u - HIT) / (1.0 - HIT);
        through + (rest - through) * (k * k * (3.0 - 2.0 * k))
    }
}

/// Crisp one-shot scale pop: `1 + amp` at the trigger, one small undershoot,
/// exactly `1` once the envelope is spent.
pub fn pop_scale(envelope: f32, amp: f32) -> f32 {
    let e = env(envelope);
    1.0 + amp * e * ((1.0 - e) * 3.8).cos()
}

/// Flash toward hot white while the envelope is fresh (quadratic falloff).
pub fn flash(color: Color, envelope: f32) -> Color {
    let e = env(envelope);
    if e <= 0.0 { color } else { color.mix(&Color::WHITE, 0.8 * e * e).with_alpha(color.alpha()) }
}

/// Scale a node about one of its own edges instead of its center, so a pop on
/// edge-anchored text never crosses the gutter. `anchor` is `-1` (left / top),
/// `0` (center) or `1` (right / bottom) per axis.
pub fn pop_transform(scale: Vec2, anchor: Vec2) -> UiTransform {
    if !scale.is_finite() || scale == Vec2::ONE {
        return UiTransform::IDENTITY;
    }
    let shift = -anchor * (scale - Vec2::ONE) * 50.0;
    UiTransform { translation: Val2::percent(shift.x, shift.y), scale, rotation: Rot2::IDENTITY }
}

/// `0..1` throb for critical readouts (low HP, dry magazine): ~1.4 beats a second.
pub fn danger_pulse(t: f32) -> f32 {
    if t.is_finite() { 0.5 + 0.5 * (t * 9.0).sin() } else { 0.0 }
}

// ---------------------------------------------------------------------------
// Status banner
// ---------------------------------------------------------------------------

/// What the top-center status line is saying (drives its underline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusKind {
    #[default]
    Searching,
    DeadZone,
    /// 5-bar signal: the extraction tower is in range.
    Locked,
    /// SOS uplink running (DEMON RUSH).
    Rush,
    /// Upload held at 99% by the living boss.
    BossHold,
    Extracting,
    Trial,
    TrialFinal,
}

/// This frame's look of the status underline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuleLook {
    pub alpha: f32,
    /// Horizontal scale about the center (`1` = the full text width).
    pub scale_x: f32,
}

/// Cheap deterministic hash of a step index into `0..1`.
fn step_noise(step: f32) -> f32 {
    ((step * 12.9898).sin() * 43_758.547).fract().abs()
}

/// Status underline: steady while searching, a smooth swell on a signal
/// lock, a sharp broken flicker in a dead zone, an urgent beat during the
/// rush, and a snap out from the center whenever the line changes (`pop`).
pub fn status_rule_look(kind: StatusKind, t: f32, pop: f32) -> RuleLook {
    let t = if t.is_finite() { t } else { 0.0 };
    let (alpha, width) = match kind {
        StatusKind::DeadZone => {
            // 18 Hz steps: most are lit, a third drop out, some tear the width.
            let n = step_noise((t * 18.0).floor());
            (if n < 0.34 { 0.18 } else { 1.0 }, if n > 0.82 { 0.86 } else { 1.0 })
        }
        StatusKind::Locked => (0.72 + 0.28 * (t * 3.2).sin(), 1.0 + 0.03 * (t * 3.2).sin()),
        StatusKind::Rush | StatusKind::BossHold => (0.5 + 0.5 * (t * 6.5).sin().abs(), 1.0),
        StatusKind::Extracting => (0.75 + 0.25 * (t * 5.0).sin(), 1.0),
        StatusKind::Searching | StatusKind::Trial | StatusKind::TrialFinal => (0.9, 1.0),
    };
    RuleLook { alpha, scale_x: width * slam_curve(pop, 0.3, 1.1, 1.0) }
}

// ---------------------------------------------------------------------------
// Toasts
// ---------------------------------------------------------------------------

/// Seconds a toast takes to snap in.
pub const TOAST_IN_SECS: f32 = 0.22;
/// Seconds a toast takes to fade out at the end of its life.
pub const TOAST_OUT_SECS: f32 = 0.6;

/// Pose of a killfeed toast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToastPose {
    pub alpha: f32,
    pub scale: f32,
    /// Offset toward the right screen edge (logical px).
    pub slide: f32,
}

/// Toast `age` seconds old with `left` seconds to live: snaps in from the
/// right edge with a small overshoot, holds, then fades while drifting back out.
pub fn toast_pose(age: f32, left: f32) -> ToastPose {
    let age = if age.is_finite() { age.max(0.0) } else { 0.0 };
    let left = if left.is_finite() { left.max(0.0) } else { 0.0 };
    let enter = 1.0 - (age / TOAST_IN_SECS).min(1.0);
    let exit = 1.0 - (left / TOAST_OUT_SECS).min(1.0);
    ToastPose {
        alpha: (1.0 - enter * enter).min(1.0 - exit),
        scale: slam_curve(enter, 1.18, 0.97, 1.0),
        slide: 26.0 * enter * enter * enter + 8.0 * exit * exit,
    }
}

// ---------------------------------------------------------------------------
// Pulse envelopes
// ---------------------------------------------------------------------------

/// The values the HUD watches for changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HudSnapshot {
    /// The two top-left counters (kills / jammers, or checkpoints / rescues).
    pub stats: [u32; 2],
    pub status: StatusKind,
    /// Checkpoint / floor splits banked so far.
    pub splits: u32,
    /// Ahead of the PB ghost (`None` without a live delta).
    pub ghost_ahead: Option<bool>,
    /// Equipped weapon slot.
    pub weapon: u8,
    /// Rounds left in the equipped weapon.
    pub ammo: u32,
}

/// Decaying `1 -> 0` envelopes, re-armed whenever a watched value changes.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct HudPulse {
    /// One per top-left counter: it went up.
    pub stats: [f32; 2],
    /// The status line changed.
    pub status: f32,
    /// A checkpoint / floor split was banked.
    pub split: f32,
    /// The PB ghost delta changed sign (ahead <-> behind).
    pub ghost: f32,
    /// A round left the equipped weapon.
    pub ammo: f32,
    /// The equipped weapon changed.
    pub swap: f32,
    seen: Option<HudSnapshot>,
}

/// Envelope lifetimes (seconds).
pub const STAT_PULSE_SECS: f32 = 0.45;
pub const STATUS_PULSE_SECS: f32 = 0.55;
pub const SPLIT_PULSE_SECS: f32 = 0.7;
pub const AMMO_PULSE_SECS: f32 = 0.16;
pub const SWAP_PULSE_SECS: f32 = 0.3;

impl HudPulse {
    /// Compare with the last snapshot and re-arm the envelopes of whatever
    /// changed. The first snapshot only primes the watcher (no pop on spawn).
    pub fn observe(&mut self, now: HudSnapshot) {
        if let Some(was) = self.seen {
            for i in 0..2 {
                if now.stats[i] > was.stats[i] {
                    self.stats[i] = 1.0;
                }
            }
            if now.status != was.status {
                self.status = 1.0;
            }
            if now.splits > was.splits {
                self.split = 1.0;
            }
            if matches!((was.ghost_ahead, now.ghost_ahead), (Some(a), Some(b)) if a != b) {
                self.ghost = 1.0;
            }
            if now.weapon != was.weapon {
                self.swap = 1.0;
            } else if now.ammo < was.ammo {
                self.ammo = 1.0;
            }
        }
        self.seen = Some(now);
    }

    /// Decay every envelope by `dt` seconds.
    pub fn tick(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let fall = |v: &mut f32, secs: f32| {
            if *v > 0.0 {
                *v = (*v - dt / secs).max(0.0);
            }
        };
        fall(&mut self.stats[0], STAT_PULSE_SECS);
        fall(&mut self.stats[1], STAT_PULSE_SECS);
        fall(&mut self.status, STATUS_PULSE_SECS);
        fall(&mut self.split, SPLIT_PULSE_SECS);
        fall(&mut self.ghost, SPLIT_PULSE_SECS);
        fall(&mut self.ammo, AMMO_PULSE_SECS);
        fall(&mut self.swap, SWAP_PULSE_SECS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadows_scale_with_the_font_and_fade_with_the_text() {
        assert_eq!(shadow_offset(10.0), 1.4);
        assert!(shadow_offset(20.0) > shadow_offset(13.0));
        assert_eq!(shadow_offset(64.0), 2.6);
        assert_eq!(hud_shadow(28.0).color.alpha(), SHADOW_ALPHA);
        assert_eq!(shadow_color(0.0).alpha(), 0.0);
        assert_eq!(shadow_color(7.0).alpha(), SHADOW_ALPHA);
        // Secondary text stays bright enough to float without a plate.
        assert!(HUD_SUB.to_srgba().green > 0.7 && HUD_SUB.alpha() == 1.0);
    }

    #[test]
    fn glow_stack_is_a_halo_over_an_ink_edge() {
        let g = glow(Color::srgb(0.2, 0.9, 1.0));
        assert_eq!(g.0.len(), 2);
        assert!(g.0[0].blur_radius != Val::ZERO && g.0[1].blur_radius == Val::ZERO);
        assert!(g.0[0].color.alpha() > 0.4 && g.0[1].color.to_srgba().red == 0.0);
        // A hidden rail casts nothing.
        assert!(glow(Color::NONE).0.iter().all(|s| s.color.alpha() == 0.0));
    }

    #[test]
    fn pops_start_big_settle_exactly_and_keep_their_anchor() {
        assert!((pop_scale(1.0, 0.2) - 1.2).abs() < 1e-5);
        assert_eq!(pop_scale(0.0, 0.2), 1.0);
        assert_eq!((pop_scale(f32::NAN, 0.2), pop_scale(-3.0, 0.2)), (1.0, 1.0));
        let low = (0..=100).map(|i| pop_scale(i as f32 / 100.0, 0.2)).fold(f32::MAX, f32::min);
        assert!((0.93..1.0).contains(&low), "one small undershoot: {low}");
        assert_eq!(pop_transform(Vec2::ONE, Vec2::new(-1.0, 0.0)), UiTransform::IDENTITY);
        assert_eq!(pop_transform(Vec2::NAN, Vec2::ONE), UiTransform::IDENTITY);
        // Left-anchored: the node shifts right by half of what it grew.
        let left = pop_transform(Vec2::splat(1.2), Vec2::new(-1.0, 0.0));
        assert_eq!(left.scale, Vec2::splat(1.2));
        assert!(matches!(left.translation.x, Val::Percent(p) if (p - 10.0).abs() < 1e-4));
        // Bottom-right anchored: up and left.
        let br = pop_transform(Vec2::splat(1.1), Vec2::ONE);
        assert!(matches!((br.translation.x, br.translation.y), (Val::Percent(x), Val::Percent(y)) if x < 0.0 && y < 0.0));
        // Flash: hot at the trigger, the plain color once spent, alpha kept.
        let c = Color::srgba(1.0, 0.2, 0.1, 0.5);
        assert_eq!(flash(c, 0.0), c);
        let hot = flash(c, 1.0).to_srgba();
        assert!(hot.green > 0.7 && hot.alpha == 0.5);
        assert_eq!(slam_curve(0.0, 3.0, 2.0, 1.0), 1.0);
        assert!((slam_curve(1.0, 3.0, 2.0, 1.0) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn status_rule_flickers_in_dead_zones_and_swells_on_a_lock() {
        let sample = |kind| (0..600).map(|i| status_rule_look(kind, i as f32 / 60.0, 0.0)).collect::<Vec<_>>();
        // Searching / trial: rock steady.
        for kind in [StatusKind::Searching, StatusKind::Trial, StatusKind::TrialFinal] {
            assert!(sample(kind).iter().all(|l| l.alpha == 0.9 && l.scale_x == 1.0), "{kind:?}");
        }
        // Dead zone: two-level sharp flicker, dark roughly a third of the time.
        let dead = sample(StatusKind::DeadZone);
        assert!(dead.iter().all(|l| l.alpha == 1.0 || l.alpha == 0.18));
        let dark = dead.iter().filter(|l| l.alpha < 0.5).count();
        assert!((90..330).contains(&dark), "{dark} dark frames of 600");
        assert!(dead.windows(2).filter(|w| w[0].alpha != w[1].alpha).count() > 20, "it really flickers");
        assert!(dead.iter().any(|l| l.scale_x < 1.0));
        // Signal lock: a smooth swell, never a jump.
        let lock = sample(StatusKind::Locked);
        assert!(lock.windows(2).all(|w| (w[0].alpha - w[1].alpha).abs() < 0.05));
        let (lo, hi) = lock.iter().fold((f32::MAX, f32::MIN), |(lo, hi), l| (lo.min(l.alpha), hi.max(l.alpha)));
        assert!(lo < 0.5 && hi > 0.95, "{lo}..{hi}");
        // Rush: beats down to half.
        assert!(sample(StatusKind::Rush).iter().any(|l| l.alpha < 0.55));
        // A change snaps the rule out from the center.
        assert!((status_rule_look(StatusKind::Searching, 0.0, 1.0).scale_x - 0.3).abs() < 1e-4);
        assert!(status_rule_look(StatusKind::Searching, 0.0, 0.58).scale_x > 1.05);
        assert!(status_rule_look(StatusKind::Locked, f32::NAN, f32::NAN).alpha.is_finite());
    }

    #[test]
    fn toasts_snap_in_hold_and_fade_out() {
        let born = toast_pose(0.0, 3.2);
        assert!(born.alpha == 0.0 && born.slide > 20.0 && born.scale > 1.1, "{born:?}");
        let held = toast_pose(1.0, 2.2);
        assert_eq!(held, ToastPose { alpha: 1.0, scale: 1.0, slide: 0.0 });
        let dying = toast_pose(2.9, 0.3);
        assert!((dying.alpha - 0.5).abs() < 1e-4 && dying.slide > 0.0 && dying.scale == 1.0, "{dying:?}");
        assert_eq!(toast_pose(3.2, 0.0).alpha, 0.0);
        // Monotonic entry, no pops.
        let alphas: Vec<f32> = (0..=22).map(|i| toast_pose(i as f32 / 100.0, 3.0).alpha).collect();
        assert!(alphas.windows(2).all(|w| w[1] >= w[0]));
        let nan = toast_pose(f32::NAN, f32::NAN);
        assert!(nan.alpha == 0.0 && nan.scale.is_finite() && nan.slide.is_finite());
    }

    #[test]
    fn pulses_arm_on_change_and_decay_on_their_own_clocks() {
        let base = HudSnapshot { stats: [3, 1], status: StatusKind::Searching, splits: 0, ghost_ahead: None, weapon: 0, ammo: 32 };
        let mut p = HudPulse::default();
        // The first snapshot primes the watcher: nothing pops when the HUD spawns.
        p.observe(base);
        assert_eq!(HudPulse { seen: None, ..p }, HudPulse::default());
        // Only the counter that went up pulses.
        p.observe(HudSnapshot { stats: [4, 1], ..base });
        assert_eq!(p.stats, [1.0, 0.0]);
        p.observe(HudSnapshot { stats: [4, 2], ..base });
        assert_eq!(p.stats, [1.0, 1.0]);
        // A reset (counters dropping) is silent.
        let mut q = HudPulse::default();
        q.observe(base);
        q.observe(HudSnapshot { stats: [0, 0], ammo: 50, ..base });
        assert_eq!(HudPulse { seen: None, ..q }, HudPulse::default());
        // Status change, split banked, ghost sign flip.
        q.observe(HudSnapshot { stats: [0, 0], ammo: 50, status: StatusKind::DeadZone, ..base });
        assert_eq!(q.status, 1.0);
        q.observe(HudSnapshot { stats: [0, 0], ammo: 50, status: StatusKind::DeadZone, splits: 1, ..base });
        assert_eq!(q.split, 1.0);
        let mut g = HudPulse::default();
        g.observe(HudSnapshot { ghost_ahead: None, ..base });
        g.observe(HudSnapshot { ghost_ahead: Some(true), ..base });
        assert_eq!(g.ghost, 0.0, "a delta appearing is not a transition");
        g.observe(HudSnapshot { ghost_ahead: Some(false), ..base });
        assert_eq!((g.ghost, g.split), (1.0, 0.0));
        // Firing kicks the ammo readout; a swap kicks the swap envelope instead.
        let mut w = HudPulse::default();
        w.observe(base);
        w.observe(HudSnapshot { ammo: 31, ..base });
        assert_eq!((w.ammo, w.swap), (1.0, 0.0));
        w.tick(1.0);
        w.observe(HudSnapshot { weapon: 1, ammo: 80, ..base });
        assert_eq!((w.ammo, w.swap), (0.0, 1.0));
        // Each envelope runs out on its own clock and stops at zero.
        w.tick(SWAP_PULSE_SECS / 2.0);
        assert!((w.swap - 0.5).abs() < 1e-5);
        w.tick(10.0);
        assert_eq!(w.swap, 0.0);
        p.tick(STAT_PULSE_SECS * 0.25);
        assert!((p.stats[0] - 0.75).abs() < 1e-5);
        p.tick(f32::NAN);
        p.tick(-1.0);
        assert!((p.stats[0] - 0.75).abs() < 1e-5);
        assert!((0.0..=1.0).contains(&danger_pulse(0.37)) && danger_pulse(f32::NAN) == 0.0);
    }
}
