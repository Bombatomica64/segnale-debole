//! Virtual input for automated play.
//!
//! [`BotControl`] is an *optional* resource: it is only inserted by the live
//! playtest bot (`SEGNALE_BOT_TEST`) and by the headless integration tests.
//! When present, the real gameplay systems (`player`, `weapons`, `apps`,
//! `extraction`, `upgrades`) read it alongside keyboard/mouse input, so the bot
//! exercises exactly the same physics, weapons, apps and state machine a human
//! does. Held inputs (`wish_dir`, `strafe_axes`, `fire`, `jump`) stay set until cleared;
//! one-shot inputs (`dash`, `ping`, `overload`, `interact`, `switch_weapon`,
//! `pick_upgrade`) are consumed by the system that handles them.

use bevy::prelude::*;

use crate::weapons::Weapon;

#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct BotControl {
    /// Desired horizontal move direction in world space (normalized by the
    /// player controller; zero = no movement intent).
    pub wish_dir: Vec3,
    /// Held local movement keys like WASD (`x` = right/D, `y` = forward/W,
    /// each -1..1), so automated play can air-strafe and air-steer exactly
    /// like a keyboard. Ignored while `wish_dir` is set.
    pub strafe_axes: Vec2,
    /// Absolute look yaw (radians, 0 = facing -Z) applied before movement.
    pub look_yaw: Option<f32>,
    /// Absolute look pitch (radians, + = up).
    pub look_pitch: Option<f32>,
    /// Hold the trigger.
    pub fire: bool,
    /// Hold jump.
    pub jump: bool,
    /// One-shot: switch to this weapon.
    pub switch_weapon: Option<Weapon>,
    /// One-shot: Airplane Mode dash.
    pub dash: bool,
    /// One-shot: Ping Scan.
    pub ping: bool,
    /// One-shot: 5G Overload.
    pub overload: bool,
    /// One-shot: `E` (trigger SOS uplink at the tower).
    pub interact: bool,
    /// One-shot: App Store card to install (0..3).
    pub pick_upgrade: Option<usize>,
}

impl BotControl {
    /// Release every held input (one-shots are left untouched).
    pub fn release_all(&mut self) {
        self.wish_dir = Vec3::ZERO;
        self.strafe_axes = Vec2::ZERO;
        self.fire = false;
        self.jump = false;
    }
}

/// Consume a one-shot flag on an optional `BotControl`.
pub fn take_flag(bot: &mut Option<ResMut<BotControl>>, f: impl FnOnce(&mut BotControl) -> &mut bool) -> bool {
    bot.as_mut().is_some_and(|b| std::mem::take(f(b)))
}
