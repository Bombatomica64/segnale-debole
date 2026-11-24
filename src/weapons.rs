//! Right hand: Mars Pump Shotgun (hitscan pellets), UAC Plasma Repeater (3D
//! bolts) and the Signal-Mortar rocket launcher (splash + Quake rocket jumps).
//! Left hand: tactical smartphone viewmodel with a live signal screen.
//! Also destructible Demon Jammers and pickups.
//!
//! Viewmodels live on [`OVERLAY_LAYER`], drawn by a second camera (child of the
//! `PlayerCamera`) that clears depth, so they never clip into walls.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::camera::visibility::RenderLayers;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use rand::Rng;

use crate::apps::{spawn_hijack_hotspot, PhoneApps};
use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::BotControl;
use crate::enemies::{damage_enemy, ray_sphere, Enemy, EnemyKind, PingMarked};
use crate::fx::{
    additive_glow, overlay_layer, spawn_burst, spawn_ejected_shell, spawn_flash_light, spawn_particles, spawn_shockwave, spawn_tracer, BurstColor,
    FxAssets, HudFeed, OVERLAY_LAYER,
};
use crate::hud::signal_color;
use crate::level::{Cell, CullableLight, DemonJammer, JammerCore, LevelEntity, LevelGrid};
use crate::mesh_batch::{MaterialBatches, ParentedBatches};
use crate::meta::{HandsetSkin, RunLoadout, SkinSlot};
use crate::phone_screen::PhoneScreenTexture;
use crate::physics::{has_line_of_sight, raycast_grid};
use crate::player::{live_eye_pose, smooth_factor, CursorCaptured, Player, PlayerCamera, AIR_SPEED_HARD_CAP};
use crate::signal::{overclock_factor, SignalState};
use crate::state::{GameSet, PlayerModifiers, RunStats};

pub const SHOTGUN_COOLDOWN: f32 = 0.65;
pub const PLASMA_COOLDOWN: f32 = 0.14;
pub const SHOTGUN_PELLETS: u32 = 8;
pub const PELLET_DAMAGE: f32 = 10.5;
pub const PELLET_SPREAD: f32 = 0.085;
pub const PLASMA_DAMAGE: f32 = 16.0;
pub const PLASMA_SPEED: f32 = 55.0;
pub const HITSCAN_RANGE: f32 = 70.0;
pub const SWAP_SECS: f32 = 0.32;
pub const TRICKLE_SECS: f32 = 1.2;
pub const PICKUP_RADIUS: f32 = 1.5;

// --- Signal-Mortar (rocket launcher) -----------------------------------------
pub const ROCKET_COOLDOWN: f32 = 0.85;
pub const ROCKET_SPEED: f32 = 34.0;
pub const ROCKET_DIRECT_DAMAGE: f32 = 115.0;
pub const ROCKET_SPLASH_DAMAGE: f32 = 95.0;
pub const ROCKET_SPLASH_RADIUS: f32 = 5.5;
/// Fraction of splash damage the player takes from their own rocket.
pub const ROCKET_SELF_DAMAGE_FRAC: f32 = 0.15;
/// Upward rocket-jump boost (m/s) at point-blank.
pub const ROCKET_JUMP_UP: f32 = 13.0;
/// Radial horizontal rocket-jump push (m/s) at point-blank.
pub const ROCKET_JUMP_PUSH: f32 = 16.0;
pub const ROCKET_LIFE: f32 = 3.0;
/// Seconds between rocket smoke/ember trail puffs.
pub const ROCKET_TRAIL_SECS: f32 = 0.04;
/// Aim distance under which a rocket starts on the eye ray (never behind geometry).
pub const ROCKET_CLOSE_AIM: f32 = 2.0;

// --- Doppler Overdrive ---------------------------------------------------------
/// Horizontal speed (m/s) above which Doppler Overdrive kicks in.
pub const DOPPLER_SPEED_THRESHOLD: f32 = 20.0;

/// Whether Doppler Overdrive is live at this horizontal speed.
pub fn doppler_active(speed: f32, mods: &PlayerModifiers) -> bool {
    mods.doppler_damage_bonus > 0.0 && speed >= DOPPLER_SPEED_THRESHOLD
}

/// Weapon damage multiplier from Doppler Overdrive (1.0 when inactive).
pub fn doppler_damage_mult(speed: f32, mods: &PlayerModifiers) -> f32 {
    if doppler_active(speed, mods) { 1.0 + mods.doppler_damage_bonus } else { 1.0 }
}

/// Shotgun spread multiplier from Doppler Overdrive (1.0 when inactive).
pub fn doppler_spread_mult(speed: f32, mods: &PlayerModifiers) -> f32 {
    if doppler_active(speed, mods) { mods.doppler_spread_mult } else { 1.0 }
}

/// Weapons, apps and projectiles that deal damage; deaths run after this set.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CombatDamage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weapon {
    #[default]
    Shotgun,
    Plasma,
    Rocket,
}

impl Weapon {
    pub const ALL: [Weapon; 3] = [Weapon::Shotgun, Weapon::Plasma, Weapon::Rocket];

    pub fn index(self) -> usize {
        match self {
            Weapon::Shotgun => 0,
            Weapon::Plasma => 1,
            Weapon::Rocket => 2,
        }
    }
    /// Next weapon in the deck (wraps): Shotgun -> Plasma -> Rocket -> Shotgun.
    pub fn next(self) -> Weapon {
        Weapon::ALL[(self.index() + 1) % Weapon::ALL.len()]
    }
    /// Previous weapon in the deck (wraps).
    pub fn prev(self) -> Weapon {
        Weapon::ALL[(self.index() + Weapon::ALL.len() - 1) % Weapon::ALL.len()]
    }
    pub fn base_cooldown(self) -> f32 {
        match self {
            Weapon::Shotgun => SHOTGUN_COOLDOWN,
            Weapon::Plasma => PLASMA_COOLDOWN,
            Weapon::Rocket => ROCKET_COOLDOWN,
        }
    }
    pub fn has_ammo(self, p: &Player) -> bool {
        match self {
            Weapon::Shotgun => p.ammo_shells > 0,
            Weapon::Plasma => p.ammo_plasma > 0,
            Weapon::Rocket => p.ammo_rockets > 0,
        }
    }
    /// The first other weapon with ammo, scanning `next()` then `next().next()`.
    pub fn auto_swap_target(self, p: &Player) -> Option<Weapon> {
        [self.next(), self.next().next()].into_iter().find(|w| *w != self && w.has_ammo(p))
    }
}

/// Seconds between shots with all fire-rate modifiers (incl. 5G Overclock).
pub fn fire_cooldown(w: Weapon, mods: &PlayerModifiers, bars: u8) -> f32 {
    w.base_cooldown() / (mods.fire_rate_mult * overclock_factor(bars, mods))
}

#[derive(Resource, Debug, Clone, Default)]
pub struct WeaponState {
    pub current: Weapon,
    /// Weapon being lowered during a swap (shown for the first half of it).
    pub previous: Weapon,
    pub cooldown: f32,
    /// Remaining swap animation; the model switches at the halfway point.
    pub swap_timer: f32,
    pub recoil: f32,
    pub pump: f32,
    pub muzzle: f32,
    pub trickle: f32,
    /// Random roll of the shotgun star flash for the current shot.
    pub muzzle_roll: f32,
    /// Dev/screenshot hook: fire once regardless of input.
    pub force_fire: bool,
}

impl WeaponState {
    /// Weapon whose model is on screen (old one during the lowering half of a swap).
    pub fn displayed(&self) -> Weapon {
        if self.swap_timer > SWAP_SECS * 0.5 { self.previous } else { self.current }
    }

    pub fn switch_to(&mut self, w: Weapon) -> bool {
        if w == self.current {
            return false;
        }
        // Mid-swap re-targets keep lowering whatever is actually on screen
        // without popping back to the raised pose.
        self.previous = self.displayed();
        self.current = w;
        let half = SWAP_SECS * 0.5;
        self.swap_timer = if self.swap_timer > 0.0 {
            half + (self.swap_timer - half).abs()
        } else {
            SWAP_SECS
        };
        self.cooldown = self.cooldown.max(SWAP_SECS * 0.6);
        true
    }
}

/// What a hitscan ray struck first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitTarget {
    Nothing,
    Wall,
    Enemy(usize),
    Jammer(usize),
}

/// Jammer hit volumes relative to its base.
pub const JAMMER_SPHERES: [(f32, f32); 3] = [(0.5, 0.5), (1.3, 0.5), (2.7, 0.8)];

pub fn jammer_ray_hit(base: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    JAMMER_SPHERES
        .iter()
        .filter_map(|&(h, r)| ray_sphere(origin, dir, base + Vec3::Y * h, r))
        .reduce(f32::min)
}

/// Lightweight hitscan view of a demon (no `Enemy` clone per shot).
#[derive(Debug, Clone, Copy)]
pub struct HitTargetInfo {
    pub root: Vec3,
    pub alive: bool,
    /// Hit volumes `(height above root, radius)` (see [`Enemy::ray_hit`]).
    pub spheres: &'static [(f32, f32)],
}

impl HitTargetInfo {
    pub fn of(root: Vec3, e: &Enemy) -> Self {
        HitTargetInfo { root, alive: e.is_alive(), spheres: e.stats.hit_spheres }
    }

    /// Same result as [`Enemy::ray_hit`].
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<f32> {
        self.spheres
            .iter()
            .filter_map(|&(h, r)| ray_sphere(origin, dir, self.root + Vec3::Y * h, r))
            .filter(|&t| t <= max_dist)
            .reduce(f32::min)
    }
}

/// Nearest hit among walls, demons and jammers (walls block shots).
pub fn resolve_hitscan(
    grid: &LevelGrid,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    enemies: &[(Vec3, &Enemy)],
    jammers: &[Vec3],
) -> (f32, HitTarget) {
    let targets: Vec<HitTargetInfo> = enemies.iter().map(|(root, e)| HitTargetInfo::of(*root, e)).collect();
    resolve_hitscan_targets(grid, origin, dir, max_dist, &targets, jammers)
}

/// [`resolve_hitscan`] over pre-collected [`HitTargetInfo`]s.
pub fn resolve_hitscan_targets(
    grid: &LevelGrid,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    enemies: &[HitTargetInfo],
    jammers: &[Vec3],
) -> (f32, HitTarget) {
    let dir = dir.normalize_or_zero();
    let mut best = match raycast_grid(grid, origin, dir, max_dist) {
        Some(h) => (h.distance, HitTarget::Wall),
        None => (max_dist, HitTarget::Nothing),
    };
    for (i, e) in enemies.iter().enumerate() {
        if e.alive
            && let Some(t) = e.ray_hit(origin, dir, best.0)
            && t < best.0
        {
            best = (t, HitTarget::Enemy(i));
        }
    }
    for (i, base) in jammers.iter().enumerate() {
        if let Some(t) = jammer_ray_hit(*base, origin, dir)
            && t < best.0
        {
            best = (t, HitTarget::Jammer(i));
        }
    }
    best
}

/// Damage a jammer (weapons & EMP). Returns true if this destroyed it.
pub fn damage_jammer(j: &mut DemonJammer, amount: f32) -> bool {
    if !j.is_active() {
        return false;
    }
    j.hp -= amount;
    j.hit_flash = 0.15;
    !j.is_active()
}

pub fn knockback_scale(kind: EnemyKind) -> f32 {
    match kind {
        EnemyKind::Imp => 1.0,
        EnemyKind::Cacodemon => 0.7,
        EnemyKind::JammerFiend => 0.45,
        EnemyKind::HellBaron => 0.15,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectileKind {
    #[default]
    Plasma,
    Rocket,
}

#[derive(Component, Debug, Clone)]
pub struct PlayerProjectile {
    pub kind: ProjectileKind,
    pub vel: Vec3,
    /// Base damage (direct hit for rockets) before `damage_mult`.
    pub damage: f32,
    pub life: f32,
    /// Doppler Overdrive multiplier captured at fire time (enemy damage only).
    pub damage_mult: f32,
    /// Seconds until the next trail puff (rockets).
    pub trail: f32,
}

impl PlayerProjectile {
    pub fn plasma(vel: Vec3, damage_mult: f32) -> Self {
        PlayerProjectile { kind: ProjectileKind::Plasma, vel, damage: PLASMA_DAMAGE, life: 2.0, damage_mult, trail: 0.0 }
    }
    pub fn rocket(vel: Vec3, damage_mult: f32) -> Self {
        PlayerProjectile { kind: ProjectileKind::Rocket, vel, damage: ROCKET_DIRECT_DAMAGE, life: ROCKET_LIFE, damage_mult, trail: 0.0 }
    }
}

/// Linear splash falloff: 1 at the blast point, 0 at `radius` and beyond.
pub fn splash_falloff(dist: f32, radius: f32) -> f32 {
    if radius <= 0.0 { 0.0 } else { (1.0 - dist / radius).clamp(0.0, 1.0) }
}

/// Distance from a blast to the nearest hit sphere surface of a target.
pub fn splash_distance(blast: Vec3, root: Vec3, spheres: &[(f32, f32)]) -> f32 {
    spheres
        .iter()
        .map(|&(h, r)| ((root + Vec3::Y * h).distance(blast) - r).max(0.0))
        .reduce(f32::min)
        .unwrap_or_else(|| root.distance(blast))
}

/// Splash damage of a rocket on a non-direct-hit target `dist` away.
pub fn rocket_splash_damage(dist: f32, radius: f32, damage_mult: f32) -> f32 {
    ROCKET_SPLASH_DAMAGE * splash_falloff(dist, radius) * damage_mult
}

/// Effective rocket splash radius with Blast Thrusters.
pub fn rocket_splash_radius(mods: &PlayerModifiers) -> f32 {
    ROCKET_SPLASH_RADIUS * mods.rocket_splash_mult
}

/// The player's share of their own rocket blast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RocketJump {
    /// Self-damage before armor (forgiving ~15% of splash).
    pub self_damage: f32,
    /// Velocity impulse (m/s) added to the player.
    pub impulse: Vec3,
    pub falloff: f32,
}

/// Quake rocket jump: self-damage and launch impulse for a blast at `blast`
/// on a player whose feet are at `player_pos` (`None` = out of range).
pub fn compute_rocket_jump(player_pos: Vec3, blast: Vec3, splash_radius: f32, mods: &PlayerModifiers) -> Option<RocketJump> {
    let center = player_pos + Vec3::Y * 0.9;
    let d = center - blast;
    let dist = d.length();
    if dist > splash_radius {
        return None;
    }
    let falloff = splash_falloff(dist, splash_radius);
    // Scale horizontal thrust by the horizontal component of the 3D blast
    // vector so steep foot shots launch predominantly upward while wall/rear
    // blasts deliver full sideways/forward kick.
    let horiz = if dist > 1e-4 { Vec2::new(d.x, d.z) / dist } else { Vec2::ZERO };
    let k = falloff * mods.rocket_jump_mult;
    let impulse = Vec3::new(horiz.x * ROCKET_JUMP_PUSH * k, ROCKET_JUMP_UP * k, horiz.y * ROCKET_JUMP_PUSH * k);
    let self_damage = ROCKET_SPLASH_DAMAGE * falloff * ROCKET_SELF_DAMAGE_FRAC * mods.rocket_self_damage_mult;
    Some(RocketJump { self_damage, impulse, falloff })
}

/// Apply a rocket jump: damage through armor / invuln, add the launch impulse
/// (horizontal clamped to the hard cap so the CPMA over-cap bleed carries it)
/// and detach from the ground. Returns the HP lost.
pub fn apply_rocket_jump(player: &mut Player, jump: &RocketJump) -> f32 {
    let lost = player.apply_damage(jump.self_damage);
    let mut v = player.velocity + jump.impulse;
    let h = Vec2::new(v.x, v.z);
    if h.length() > AIR_SPEED_HARD_CAP {
        let h = h.normalize() * AIR_SPEED_HARD_CAP;
        v.x = h.x;
        v.z = h.y;
    }
    player.velocity = v;
    if player.velocity.y > 0.5 {
        player.grounded = false;
        player.coyote_timer = 0.0;
    }
    lost
}

/// Where a rocket starts and which way it flies. Starts at the muzzle unless
/// the aim point is point-blank or the eye->muzzle segment is blocked, then
/// it starts just in front of the eye on the aim ray, so it can never spawn
/// behind a wall or under the floor.
pub fn rocket_launch(grid: &LevelGrid, origin: Vec3, fwd: Vec3, muzzle: Vec3, aim_dist: f32) -> (Vec3, Vec3) {
    let to_muzzle = muzzle - origin;
    let blocked = raycast_grid(grid, origin, to_muzzle, to_muzzle.length() + 0.15).is_some();
    if aim_dist <= ROCKET_CLOSE_AIM || blocked {
        let start = origin + fwd * (aim_dist * 0.25).min(0.15);
        (start, fwd)
    } else {
        let aim = origin + fwd * aim_dist;
        (muzzle, (aim - muzzle).normalize_or(fwd))
    }
}

// ---------------------------------------------------------------------------
// Pickups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickupKind {
    Health,
    Armor,
    Shells,
    Plasma,
    Rockets,
}

impl PickupKind {
    pub const ALL: [PickupKind; 5] = [PickupKind::Health, PickupKind::Armor, PickupKind::Shells, PickupKind::Plasma, PickupKind::Rockets];

    pub fn index(self) -> usize {
        match self {
            PickupKind::Health => 0,
            PickupKind::Armor => 1,
            PickupKind::Shells => 2,
            PickupKind::Plasma => 3,
            PickupKind::Rockets => 4,
        }
    }
}

impl PickupKind {
    pub fn base_amount(self) -> f32 {
        match self {
            PickupKind::Health => 25.0,
            PickupKind::Armor => 25.0,
            PickupKind::Shells => 8.0,
            PickupKind::Plasma => 40.0,
            PickupKind::Rockets => 4.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PickupKind::Health => "MEDKIT",
            PickupKind::Armor => "ARMOR",
            PickupKind::Shells => "SHELLS",
            PickupKind::Plasma => "PLASMA CELL",
            PickupKind::Rockets => "ROCKETS",
        }
    }
}

#[derive(Component, Debug, Clone)]
pub struct Pickup {
    pub kind: PickupKind,
    pub base_y: f32,
    pub phase: f32,
}

/// Apply a pickup; returns the amount gained (0 = not needed, stays on the floor).
pub fn apply_pickup(kind: PickupKind, p: &mut Player, mods: &PlayerModifiers) -> f32 {
    let amt = kind.base_amount() * mods.pickup_mult;
    match kind {
        PickupKind::Health => {
            let before = p.hp;
            p.heal(amt);
            p.hp - before
        }
        PickupKind::Armor => {
            let before = p.armor;
            p.armor = (p.armor + amt).min(p.max_armor);
            p.armor - before
        }
        PickupKind::Shells => {
            let before = p.ammo_shells;
            p.ammo_shells = (p.ammo_shells + amt.round() as u32).min(p.max_shells);
            (p.ammo_shells - before) as f32
        }
        PickupKind::Plasma => {
            let before = p.ammo_plasma;
            p.ammo_plasma = (p.ammo_plasma + amt.round() as u32).min(p.max_plasma);
            (p.ammo_plasma - before) as f32
        }
        PickupKind::Rockets => {
            let before = p.ammo_rockets;
            p.ammo_rockets = (p.ammo_rockets + amt.round() as u32).min(p.max_rockets).max(before);
            (p.ammo_rockets - before) as f32
        }
    }
}

#[derive(Resource, Clone)]
pub struct PickupAssets {
    pub cube: Handle<Mesh>,
    pub cyl: Handle<Mesh>,
    pub white: Handle<StandardMaterial>,
    pub red: Handle<StandardMaterial>,
    pub armor: Handle<StandardMaterial>,
    pub shell: Handle<StandardMaterial>,
    pub brass: Handle<StandardMaterial>,
    pub plasma: Handle<StandardMaterial>,
    pub steel: Handle<StandardMaterial>,
    /// Glowing amber warhead tip of the rocket crate.
    pub warhead: Handle<StandardMaterial>,
    /// Hazard-striped olive rocket crate.
    pub crate_olive: Handle<StandardMaterial>,
    /// Per-kind models baked at startup: one merged mesh per material
    /// (indexed by [`PickupKind::index`]).
    pub models: [Vec<(Handle<Mesh>, Handle<StandardMaterial>)>; 5],
}

#[derive(Component)]
struct PickupSpin;

/// Mesh part of a pickup model: (mesh, material, offset, scale).
type PickupPart = (Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3);

/// Glow light color of a pickup.
pub fn pickup_light(kind: PickupKind) -> Color {
    match kind {
        PickupKind::Health => Color::srgb(1.0, 0.2, 0.2),
        PickupKind::Armor => Color::srgb(0.2, 1.0, 0.4),
        PickupKind::Shells => Color::srgb(1.0, 0.6, 0.2),
        PickupKind::Plasma => Color::srgb(0.2, 0.9, 1.0),
        PickupKind::Rockets => Color::srgb(1.0, 0.5, 0.12),
    }
}

/// Light color and rigid parts of a pickup model.
fn pickup_parts(a: &PickupAssets, kind: PickupKind) -> (Color, Vec<PickupPart>) {
    match kind {
        PickupKind::Health => (
            Color::srgb(1.0, 0.2, 0.2),
            vec![
                (a.cube.clone(), a.white.clone(), Vec3::ZERO, Vec3::new(0.5, 0.36, 0.36)),
                (a.cube.clone(), a.red.clone(), Vec3::ZERO, Vec3::new(0.14, 0.3, 0.38)),
                (a.cube.clone(), a.red.clone(), Vec3::ZERO, Vec3::new(0.36, 0.1, 0.38)),
            ],
        ),
        PickupKind::Armor => (
            Color::srgb(0.2, 1.0, 0.4),
            vec![
                (a.cube.clone(), a.armor.clone(), Vec3::ZERO, Vec3::new(0.5, 0.5, 0.16)),
                (a.cube.clone(), a.armor.clone(), Vec3::new(0.0, 0.3, 0.0), Vec3::new(0.62, 0.12, 0.2)),
                (a.cube.clone(), a.steel.clone(), Vec3::new(0.0, 0.0, -0.09), Vec3::new(0.22, 0.3, 0.03)),
            ],
        ),
        PickupKind::Shells => (
            Color::srgb(1.0, 0.6, 0.2),
            (0..4)
                .flat_map(|i| {
                    let x = -0.18 + i as f32 * 0.12;
                    [
                        (a.cyl.clone(), a.shell.clone(), Vec3::new(x, 0.02, 0.0), Vec3::new(0.05, 0.3, 0.05)),
                        (a.cyl.clone(), a.brass.clone(), Vec3::new(x, -0.16, 0.0), Vec3::new(0.055, 0.07, 0.055)),
                    ]
                })
                .collect(),
        ),
        PickupKind::Plasma => (
            Color::srgb(0.2, 0.9, 1.0),
            vec![
                (a.cyl.clone(), a.plasma.clone(), Vec3::ZERO, Vec3::new(0.16, 0.42, 0.16)),
                (a.cyl.clone(), a.steel.clone(), Vec3::new(0.0, 0.24, 0.0), Vec3::new(0.19, 0.07, 0.19)),
                (a.cyl.clone(), a.steel.clone(), Vec3::new(0.0, -0.24, 0.0), Vec3::new(0.19, 0.07, 0.19)),
            ],
        ),
        PickupKind::Rockets => {
            // Open olive crate with two hazard bands, cradling two upright
            // warheads (steel casing + glowing amber nose + stubby fins).
            let mut parts = vec![
                (a.cube.clone(), a.crate_olive.clone(), Vec3::new(0.0, -0.17, 0.0), Vec3::new(0.52, 0.16, 0.3)),
                (a.cube.clone(), a.shell.clone(), Vec3::new(-0.18, -0.17, 0.0), Vec3::new(0.05, 0.165, 0.305)),
                (a.cube.clone(), a.shell.clone(), Vec3::new(0.18, -0.17, 0.0), Vec3::new(0.05, 0.165, 0.305)),
            ];
            for x in [-0.12f32, 0.12] {
                parts.push((a.cyl.clone(), a.steel.clone(), Vec3::new(x, 0.04, 0.0), Vec3::new(0.075, 0.34, 0.075)));
                parts.push((a.cyl.clone(), a.warhead.clone(), Vec3::new(x, 0.26, 0.0), Vec3::new(0.06, 0.1, 0.06)));
                parts.push((a.cube.clone(), a.brass.clone(), Vec3::new(x, -0.06, 0.0), Vec3::new(0.2, 0.06, 0.02)));
                parts.push((a.cube.clone(), a.brass.clone(), Vec3::new(x, -0.06, 0.0), Vec3::new(0.02, 0.06, 0.2)));
            }
            (Color::srgb(1.0, 0.5, 0.12), parts)
        }
    }
}

/// Spawn a pickup: root (bob) + light + spinning pivot carrying the
/// pre-baked per-material meshes (no per-pickup assets).
pub fn spawn_pickup(commands: &mut Commands, a: &PickupAssets, kind: PickupKind, pos: Vec3, dropped: bool) {
    let mut rng = rand::rng();
    let base_y = pos.y + 0.55;
    let light = pickup_light(kind);
    let root = commands
        .spawn((
            LevelEntity,
            Pickup { kind, base_y, phase: rng.random_range(0.0..TAU) },
            Transform::from_translation(pos.with_y(base_y)),
            Visibility::default(),
        ))
        .id();
    commands.spawn((
        ChildOf(root),
        CullableLight::for_range(4.0),
        PointLight { color: light, intensity: if dropped { 50_000.0 } else { 80_000.0 }, range: 4.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, 0.3, 0.0),
    ));
    let models = &a.models[kind.index()];
    let mut spin = commands.spawn((ChildOf(root), PickupSpin, Transform::default(), Visibility::default()));
    if let Some((mesh, mat)) = models.first() {
        spin.insert((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone())));
    }
    let spin = spin.id();
    for (mesh, mat) in models.iter().skip(1) {
        commands.spawn((ChildOf(spin), Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::IDENTITY));
    }
}

fn create_pickup_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let glow = |c: Color, e: LinearRgba| StandardMaterial { base_color: c, emissive: e, perceptual_roughness: 0.4, ..default() };
    let mut a = PickupAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        cyl: meshes.add(Cylinder::new(1.0, 1.0)),
        white: materials.add(glow(Color::srgb(0.9, 0.9, 0.9), LinearRgba::rgb(0.6, 0.6, 0.6))),
        red: materials.add(glow(Color::srgb(1.0, 0.1, 0.1), LinearRgba::rgb(4.0, 0.2, 0.2))),
        armor: materials.add(glow(Color::srgb(0.2, 0.7, 0.35), LinearRgba::rgb(0.3, 2.5, 0.8))),
        shell: materials.add(glow(Color::srgb(0.8, 0.1, 0.05), LinearRgba::rgb(1.5, 0.2, 0.05))),
        brass: materials.add(glow(Color::srgb(0.9, 0.7, 0.2), LinearRgba::rgb(1.2, 0.8, 0.1))),
        plasma: materials.add(glow(Color::srgb(0.3, 0.9, 1.0), LinearRgba::rgb(0.5, 5.0, 7.0))),
        steel: materials.add(StandardMaterial { base_color: Color::srgb(0.3, 0.32, 0.35), metallic: 0.8, perceptual_roughness: 0.35, ..default() }),
        warhead: materials.add(glow(Color::srgb(1.0, 0.55, 0.12), LinearRgba::rgb(6.0, 2.4, 0.3))),
        crate_olive: materials.add(StandardMaterial { base_color: Color::srgb(0.24, 0.27, 0.16), perceptual_roughness: 0.75, ..default() }),
        models: Default::default(),
    };
    let src_cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
    let src_cyl = Mesh::from(Cylinder::new(1.0, 1.0));
    for kind in PickupKind::ALL {
        let mut batches = MaterialBatches::new();
        for (mesh, mat, t, s) in pickup_parts(&a, kind).1 {
            let src = if mesh.id() == a.cube.id() { &src_cube } else { &src_cyl };
            batches.add(src, &mat, Transform::from_translation(t).with_scale(s));
        }
        a.models[kind.index()] = batches.finish(&mut meshes);
    }
    commands.insert_resource(a);
}

/// Floor loot: a pickup in the middle of a few bunker rooms.
pub fn floor_pickup_plan(grid: &LevelGrid) -> Vec<(PickupKind, IVec2)> {
    // Floors carry only a handful of loot spots: odd floors lead with shells,
    // even floors with a rocket crate (the between-floor patch tops shells up).
    let (lead, second) = if grid.floor.is_multiple_of(2) { (PickupKind::Rockets, PickupKind::Shells) } else { (PickupKind::Shells, PickupKind::Rockets) };
    let kinds = [lead, PickupKind::Health, second, PickupKind::Plasma, PickupKind::Armor];
    grid.rooms
        .iter()
        .skip(1) // rooms[0] is the atrium
        .map(|r| r.center())
        .filter(|&c| matches!(grid.cell(c), Cell::Floor { .. }) && c != grid.spawn && c != grid.tower && !grid.jammers.contains(&c))
        .take(5 + grid.floor as usize / 2)
        .enumerate()
        .map(|(i, c)| (kinds[i % kinds.len()], c))
        .collect()
}

fn spawn_floor_pickups(mut commands: Commands, grid: Res<LevelGrid>, a: Res<PickupAssets>, mut ws: ResMut<WeaponState>) {
    ws.cooldown = 0.0;
    ws.swap_timer = 0.0;
    for (k, c) in floor_pickup_plan(&grid) {
        spawn_pickup(&mut commands, &a, k, grid.cell_center(c), false);
    }
}

#[allow(clippy::type_complexity)]
fn update_pickups(
    mut commands: Commands,
    time: Res<Time>,
    mods: Res<PlayerModifiers>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    mut players: Query<(&Transform, &mut Player)>,
    mut pickups: Query<(Entity, &mut Pickup, &mut Transform, &Children), Without<Player>>,
    mut spins: Query<&mut Transform, (With<PickupSpin>, Without<Player>, Without<Pickup>)>,
) {
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    if player.is_dead() {
        return;
    }
    let dt = time.delta_secs();
    for (e, mut pk, mut tf, children) in &mut pickups {
        pk.phase += dt;
        tf.translation.y = pk.base_y + (pk.phase * 2.5).sin() * 0.12;
        for c in children.iter() {
            if let Ok(mut s) = spins.get_mut(c) {
                s.rotation = Quat::from_rotation_y(pk.phase * 2.0);
            }
        }
        let d = tf.translation - (ptf.translation + Vec3::Y * 0.6);
        if d.length() < PICKUP_RADIUS {
            let got = apply_pickup(pk.kind, &mut player, &mods);
            if got > 0.0 {
                commands.entity(e).try_despawn();
                sfx.play(SfxEvent::Pickup);
                feed.toast(format!("+{} {}", got.round() as i32, pk.kind.label()), Color::srgb(0.6, 1.0, 0.6));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Viewmodels
// ---------------------------------------------------------------------------

#[derive(Component)]
pub struct OverlayCamera;
#[derive(Component)]
pub struct WeaponViewmodel;
#[derive(Component)]
pub struct GunModel(pub Weapon);
#[derive(Component)]
pub struct PumpForend;
#[derive(Component)]
pub struct MuzzleFlash(pub Weapon);
#[derive(Component)]
pub struct MuzzleLight;
/// Shotgun star-burst flash root (rolled + scaled per shot).
#[derive(Component)]
pub struct MuzzleStar;
#[derive(Component)]
pub struct PlasmaCoil(pub usize);
#[derive(Component)]
pub struct PhoneViewmodel;
#[derive(Component)]
pub struct PhoneScreen;
#[derive(Component)]
pub struct PhoneLed;

/// Unique handles of dynamic viewmodel materials (animated every frame).
#[derive(Resource, Clone)]
pub struct PhoneMaterials {
    pub screen: Handle<StandardMaterial>,
    pub led: Handle<StandardMaterial>,
    pub coil: Handle<StandardMaterial>,
}

/// A phone chassis part styled by the run's [`HandsetSkin`] palette slot.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneSkinPart(pub SkinSlot);

/// The handset skin the current phone viewmodel was built with.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneSkin(pub HandsetSkin);

pub const WEAPON_REST: Vec3 = Vec3::new(0.24, -0.20, -0.40);
pub const PHONE_REST: Vec3 = Vec3::new(-0.265, -0.135, -0.37);

fn vm_part(parent: Entity, mesh: &Handle<Mesh>, mat: &Handle<StandardMaterial>, t: Vec3, s: Vec3, r: Quat) -> impl Bundle {
    (ChildOf(parent), Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::from_translation(t).with_scale(s).with_rotation(r), overlay_layer())
}

/// Source primitives of the viewmodel batches (same shapes as the handles).
struct VmSourceMeshes {
    cube: Mesh,
    cyl: Mesh,
    cap: Mesh,
    thin_ring: Mesh,
    sphere: Mesh,
    cone: Mesh,
}

impl VmSourceMeshes {
    fn new() -> Self {
        VmSourceMeshes {
            cube: Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
            cyl: Mesh::from(Cylinder::new(1.0, 1.0)),
            cap: Mesh::from(Capsule3d::new(0.5, 1.0)),
            thin_ring: Mesh::from(Torus::new(0.78, 1.0)),
            sphere: Sphere::new(1.0).mesh().uv(16, 12),
            cone: Mesh::from(Cone::new(1.0, 1.0)),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_viewmodels(
    mut commands: Commands,
    cams: Query<(Entity, &Projection), Added<PlayerCamera>>,
    phone_tex: Option<Res<PhoneScreenTexture>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    loadout: Option<Res<RunLoadout>>,
    render_msaa: Option<Res<crate::perf::RenderMsaa>>,
) {
    let Ok((cam, proj)) = cams.single() else { return };
    let msaa = render_msaa.map(|m| m.0).unwrap_or(Msaa::Sample2);
    // Adapter-validated MSAA on the world camera (the player module stays untouched).
    commands.entity(cam).insert(msaa);
    let skin = loadout.map(|l| l.skin).unwrap_or_default();
    let palette = skin.palette();
    let fov = match proj {
        Projection::Perspective(p) => p.fov,
        _ => 85f32.to_radians(),
    };
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let cyl = meshes.add(Cylinder::new(1.0, 1.0));
    let ring = meshes.add(Torus::new(0.68, 1.0));
    let thin_ring = meshes.add(Torus::new(0.78, 1.0));
    let sphere = meshes.add(Sphere::new(1.0).mesh().uv(16, 12));
    let screen_quad = meshes.add(Rectangle::new(0.118, 0.177));
    // Rigid untagged parts are baked per (pivot, material) into a few meshes;
    // tagged / animated parts (skin slots, LEDs, coils, screen) stay separate.
    let src = VmSourceMeshes::new();
    let mut vb = ParentedBatches::new();

    let mat = |m: &mut Assets<StandardMaterial>, c: Color, rough: f32, metal: f32| {
        m.add(StandardMaterial { base_color: c, perceptual_roughness: rough, metallic: metal, ..default() })
    };
    let glow = |m: &mut Assets<StandardMaterial>, c: Color, e: LinearRgba| m.add(StandardMaterial { base_color: c, emissive: e, unlit: true, ..default() });

    // Rich metallic / composite / tactical palette.
    let steel = mat(&mut materials, Color::srgb(0.46, 0.49, 0.54), 0.28, 0.9);
    let brushed = mat(&mut materials, Color::srgb(0.32, 0.35, 0.39), 0.38, 0.85);
    let dark_steel = mat(&mut materials, Color::srgb(0.12, 0.13, 0.15), 0.44, 0.78);
    let heat_metal = mat(&mut materials, Color::srgb(0.24, 0.19, 0.22), 0.32, 0.88);
    let titanium = mat(&mut materials, Color::srgb(0.22, 0.24, 0.27), 0.42, 0.82);
    let copper = mat(&mut materials, Color::srgb(0.62, 0.34, 0.16), 0.35, 0.85);
    let brass = mat(&mut materials, Color::srgb(0.86, 0.68, 0.22), 0.25, 0.92);
    let shell_red = mat(&mut materials, Color::srgb(0.72, 0.08, 0.05), 0.45, 0.1);
    let wood = mat(&mut materials, Color::srgb(0.29, 0.14, 0.06), 0.72, 0.05);
    let grip = mat(&mut materials, Color::srgb(0.055, 0.055, 0.06), 0.92, 0.05);
    let glove = mat(&mut materials, Color::srgb(0.17, 0.20, 0.14), 0.88, 0.05);
    let glove_plate = mat(&mut materials, Color::srgb(0.26, 0.29, 0.22), 0.55, 0.45);
    let hazard = mat(&mut materials, Color::srgb(0.88, 0.52, 0.04), 0.5, 0.2);
    // Handset skin: unique materials for every skinned phone chassis slot.
    let mut skin_mat = |slot: SkinSlot| materials.add(palette.slot(slot).to_material());
    let (bezel, phone_frame, bumper, phone_ptt, phone_coil, phone_bolts) = (
        skin_mat(SkinSlot::Bezel),
        skin_mat(SkinSlot::Frame),
        skin_mat(SkinSlot::Bumper),
        skin_mat(SkinSlot::Ptt),
        skin_mat(SkinSlot::Coil),
        skin_mat(SkinSlot::Bolts),
    );
    let bolt_silver = mat(&mut materials, Color::srgb(0.68, 0.72, 0.76), 0.25, 0.95);

    let sight_green = glow(&mut materials, Color::srgb(0.3, 1.0, 0.4), LinearRgba::rgb(1.0, 9.0, 1.5));
    let sight_red = glow(&mut materials, Color::srgb(1.0, 0.25, 0.12), LinearRgba::rgb(10.0, 1.5, 0.4));
    let vent_amber = glow(&mut materials, Color::srgb(1.0, 0.55, 0.15), LinearRgba::rgb(6.0, 2.2, 0.2));
    let flash_core = glow(&mut materials, Color::srgb(1.0, 0.98, 0.88), LinearRgba::rgb(22.0, 18.0, 10.0));
    let flash_fire = glow(&mut materials, Color::srgb(1.0, 0.65, 0.2), LinearRgba::rgb(16.0, 6.0, 0.8));
    let flash_cyan = glow(&mut materials, Color::srgb(0.55, 1.0, 1.0), LinearRgba::rgb(4.0, 16.0, 20.0));
    let flash_cyan_halo = materials.add(additive_glow(Color::srgb(0.2, 0.92, 1.0), 0.65));
    let warhead_glow = glow(&mut materials, Color::srgb(1.0, 0.62, 0.18), LinearRgba::rgb(9.0, 3.6, 0.5));
    let flash_amber_halo = materials.add(additive_glow(Color::srgb(1.0, 0.5, 0.1), 0.6));
    let coil = materials.add(StandardMaterial { base_color: Color::srgb(0.35, 1.0, 1.0), emissive: LinearRgba::rgb(0.8, 6.0, 8.5), unlit: true, ..default() });
    let led = materials.add(StandardMaterial { base_color: Color::WHITE, emissive: LinearRgba::rgb(6.0, 1.0, 0.8), unlit: true, ..default() });
    let screen = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: phone_tex.as_ref().map(|t| t.image.clone()),
        unlit: true,
        ..default()
    });
    commands.insert_resource(PhoneMaterials { screen: screen.clone(), led: led.clone(), coil: coil.clone() });

    let overlay = commands
        .spawn((
            ChildOf(cam),
            OverlayCamera,
            Camera3d::default(),
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            msaa,
            Projection::Perspective(PerspectiveProjection { fov, near: 0.01, far: 400.0, ..default() }),
            RenderLayers::layer(OVERLAY_LAYER),
            Transform::default(),
        ))
        .id();
    // Dual key + cool rim lighting for sculpted metallic highlights on the hands.
    commands.spawn((
        ChildOf(overlay),
        PointLight { color: Color::srgb(1.0, 0.82, 0.7), intensity: 11_000.0, range: 3.5, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.18, 0.42, 0.12),
        overlay_layer(),
    ));
    commands.spawn((
        ChildOf(overlay),
        PointLight { color: Color::srgb(0.82, 0.9, 1.0), intensity: 6_800.0, range: 3.2, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(-0.35, 0.22, 0.02),
        overlay_layer(),
    ));
    commands.spawn((
        ChildOf(overlay),
        MuzzleLight,
        PointLight { color: Color::srgb(1.0, 0.72, 0.35), intensity: 0.0, range: 3.5, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.24, -0.08, -0.88),
        overlay_layer(),
    ));

    // =========================================================================
    // RIGHT HAND: ELABORATE WEAPON VIEWMODELS
    // =========================================================================
    let wroot = commands.spawn((ChildOf(cam), WeaponViewmodel, Transform::from_translation(WEAPON_REST), Visibility::default())).id();
    let rx = Quat::from_rotation_x(FRAC_PI_2);
    let rz = Quat::from_rotation_z(FRAC_PI_2);
    let id = Quat::IDENTITY;

    // ---- Weapon 1: Mars Combat Pump Shotgun ----
    let sg = commands.spawn((ChildOf(wroot), GunModel(Weapon::Shotgun), Transform::default(), Visibility::default())).id();
    // Stepped sculpted receiver body + chamfered top dust cover + bottom trigger housing.
    vb.part(sg, &src.cube, &dark_steel, Vec3::new(0.0, 0.0, -0.01), Vec3::new(0.072, 0.088, 0.25), id);
    vb.part(sg, &src.cube, &brushed, Vec3::new(0.0, 0.044, -0.01), Vec3::new(0.056, 0.016, 0.235), id);
    vb.part(sg, &src.cube, &titanium, Vec3::new(0.0, -0.046, 0.01), Vec3::new(0.052, 0.018, 0.18), id);
    // Side receiver detail: recessed ejection port on right showing chambered red/brass shell + rivets.
    vb.part(sg, &src.cube, &grip, Vec3::new(0.036, 0.014, -0.05), Vec3::new(0.006, 0.028, 0.092), id);
    vb.part(sg, &src.cyl, &shell_red, Vec3::new(0.034, 0.014, -0.06), Vec3::new(0.01, 0.048, 0.01), rx);
    vb.part(sg, &src.cyl, &brass, Vec3::new(0.034, 0.014, -0.032), Vec3::new(0.0105, 0.016, 0.0105), rx);
    vb.part(sg, &src.cube, &hazard, Vec3::new(-0.037, 0.008, -0.02), Vec3::new(0.004, 0.016, 0.11), id);
    for z in [-0.09f32, -0.01, 0.06] {
        for s in [-1.0f32, 1.0] {
            vb.part(sg, &src.cyl, &bolt_silver, Vec3::new(0.037 * s, -0.022, z), Vec3::new(0.005, 0.004, 0.005), rz);
        }
    }
    // Top Picatinny rail blocks + rear green tritium ghost-ring sight + front red fiber-optic post.
    for k in 0..7 {
        vb.part(sg, &src.cube, &dark_steel, Vec3::new(0.0, 0.056, -0.105 + k as f32 * 0.026), Vec3::new(0.028, 0.01, 0.015), id);
    }
    vb.part(sg, &src.thin_ring, &steel, Vec3::new(0.0, 0.072, 0.055), Vec3::new(0.014, 0.008, 0.014), rx);
    for s in [-1.0f32, 1.0] {
        vb.part(sg, &src.sphere, &sight_green, Vec3::new(0.018 * s, 0.072, 0.058), Vec3::splat(0.0038), id);
    }
    // Heavy upper barrel + ventilated perforated heat-shield shroud + ported muzzle choke.
    vb.part(sg, &src.cyl, &steel, Vec3::new(0.0, 0.030, -0.35), Vec3::new(0.020, 0.48, 0.020), rx);
    vb.part(sg, &src.cyl, &grip, Vec3::new(0.0, 0.030, -0.592), Vec3::new(0.015, 0.008, 0.015), rx); // bore opening
    vb.part(sg, &src.cyl, &heat_metal, Vec3::new(0.0, 0.030, -0.565), Vec3::new(0.024, 0.052, 0.024), rx); // muzzle brake
    vb.part(sg, &src.cube, &brushed, Vec3::new(0.0, 0.050, -0.34), Vec3::new(0.018, 0.008, 0.39), id); // ventilated top rib
    for k in 0..8 {
        vb.part(
            sg, &src.thin_ring,
            &brushed,
            Vec3::new(0.0, 0.030, -0.165 - k as f32 * 0.048),
            Vec3::new(0.026, 0.014, 0.026),
            rx,
        );
    }
    // Front sight ramp + glowing red tritium bead.
    vb.part(sg, &src.cube, &dark_steel, Vec3::new(0.0, 0.057, -0.555), Vec3::new(0.012, 0.018, 0.028), id);
    vb.part(sg, &src.sphere, &sight_red, Vec3::new(0.0, 0.071, -0.552), Vec3::splat(0.0048), id);
    // Underbarrel magazine tube + barrel clamp + knurled front cap.
    vb.part(sg, &src.cyl, &dark_steel, Vec3::new(0.0, -0.012, -0.32), Vec3::new(0.016, 0.42, 0.016), rx);
    vb.part(sg, &src.cyl, &brushed, Vec3::new(0.0, -0.012, -0.535), Vec3::new(0.0185, 0.028, 0.0185), rx);
    vb.part(sg, &src.cube, &steel, Vec3::new(0.0, 0.008, -0.49), Vec3::new(0.032, 0.056, 0.024), id);
    // Sculpted rear stock + grip + trigger guard + trigger.
    vb.part(sg, &src.cube, &wood, Vec3::new(0.0, -0.048, 0.20), Vec3::new(0.054, 0.078, 0.21), Quat::from_rotation_x(0.14));
    vb.part(sg, &src.cube, &grip, Vec3::new(0.0, -0.062, 0.305), Vec3::new(0.056, 0.088, 0.02), Quat::from_rotation_x(0.14));
    vb.part(sg, &src.cap, &grip, Vec3::new(0.0, -0.095, 0.075), Vec3::new(0.046, 0.07, 0.052), Quat::from_rotation_x(-0.38));
    vb.part(sg, &src.cube, &dark_steel, Vec3::new(0.0, -0.076, -0.015), Vec3::new(0.014, 0.006, 0.075), id); // trigger guard bottom
    vb.part(sg, &src.cube, &steel, Vec3::new(0.0, -0.060, 0.004), Vec3::new(0.006, 0.024, 0.01), Quat::from_rotation_x(0.35)); // trigger
    // Articulated gloved marine right hand + armored wrist cuff gripping shotgun.
    vb.part(sg, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.082), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.35));
    vb.part(sg, &src.cube, &glove_plate, Vec3::new(0.036, -0.096, 0.065), Vec3::new(0.022, 0.048, 0.068), Quat::from_rotation_x(-0.3));
    vb.part(sg, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.20), Vec3::new(0.088, 0.11, 0.088), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2));
    vb.part(sg, &src.cyl, &glove_plate, Vec3::new(0.048, -0.155, 0.165), Vec3::new(0.052, 0.036, 0.052), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2));

    // Sliding pump forend (contoured wood/polymer ribs + dual steel action bars).
    let pump = commands.spawn((ChildOf(sg), PumpForend, Transform::from_xyz(0.0, -0.012, -0.30), Visibility::default())).id();
    vb.part(pump, &src.cyl, &wood, Vec3::ZERO, Vec3::new(0.032, 0.175, 0.030), rx);
    vb.part(pump, &src.cube, &wood, Vec3::new(0.0, -0.014, 0.0), Vec3::new(0.058, 0.034, 0.165), id);
    for k in 0..6 {
        vb.part(pump, &src.cyl, &grip, Vec3::new(0.0, -0.002, -0.068 + k as f32 * 0.027), Vec3::new(0.0335, 0.011, 0.0335), rx);
    }
    for s in [-1.0f32, 1.0] {
        vb.part(pump, &src.cube, &steel, Vec3::new(0.027 * s, 0.005, 0.13), Vec3::new(0.005, 0.008, 0.18), id);
    }
    // Layered volumetric shotgun muzzle star-burst (compact & punchy so targets stay visible).
    let star = commands
        .spawn((ChildOf(sg), MuzzleFlash(Weapon::Shotgun), MuzzleStar, Transform::from_xyz(0.0, 0.028, -0.63), Visibility::Hidden))
        .id();
    let q = std::f32::consts::FRAC_PI_4;
    for (a, len, width) in [(0.0f32, 0.24f32, 0.018f32), (2.0 * q, 0.21, 0.016), (q, 0.14, 0.014), (3.0 * q, 0.13, 0.014)] {
        vb.part(star, &src.cube, &flash_fire, Vec3::ZERO, Vec3::new(len, width, 0.005), Quat::from_rotation_z(a));
        vb.part(star, &src.cube, &flash_core, Vec3::new(0.0, 0.0, 0.003), Vec3::new(len * 0.52, width * 0.55, 0.005), Quat::from_rotation_z(a));
    }
    vb.part(star, &src.cone, &flash_fire, Vec3::new(0.0, 0.0, -0.075), Vec3::new(0.025, 0.15, 0.025), Quat::from_rotation_x(-FRAC_PI_2));
    vb.part(star, &src.sphere, &flash_core, Vec3::ZERO, Vec3::splat(0.034), id);

    // ---- Weapon 2: UAC Plasma Repeater (elaborate twin-rail sci-fi rifle) ----
    let pl = commands.spawn((ChildOf(wroot), GunModel(Weapon::Plasma), Transform::default(), Visibility::Hidden)).id();
    // Layered dark titanium lower receiver + brushed upper shroud + rear stock.
    vb.part(pl, &src.cube, &titanium, Vec3::new(0.0, -0.008, -0.10), Vec3::new(0.088, 0.092, 0.34), id);
    vb.part(pl, &src.cube, &brushed, Vec3::new(0.0, 0.042, -0.14), Vec3::new(0.072, 0.034, 0.32), id);
    vb.part(pl, &src.cube, &dark_steel, Vec3::new(0.0, 0.062, -0.08), Vec3::new(0.046, 0.018, 0.20), id);
    vb.part(pl, &src.cube, &hazard, Vec3::new(0.0, 0.018, 0.04), Vec3::new(0.092, 0.014, 0.05), id);
    vb.part(pl, &src.cube, &dark_steel, Vec3::new(0.0, -0.02, 0.14), Vec3::new(0.068, 0.084, 0.16), id);
    // Rear holographic sight frame + glowing cyan reticle dot.
    vb.part(pl, &src.cube, &dark_steel, Vec3::new(-0.022, 0.086, -0.01), Vec3::new(0.007, 0.036, 0.034), id);
    vb.part(pl, &src.cube, &dark_steel, Vec3::new(0.022, 0.086, -0.01), Vec3::new(0.007, 0.036, 0.034), id);
    vb.part(pl, &src.cube, &dark_steel, Vec3::new(0.0, 0.102, -0.01), Vec3::new(0.052, 0.007, 0.034), id);
    vb.part(pl, &src.sphere, &coil, Vec3::new(0.0, 0.086, -0.012), Vec3::splat(0.0042), id);
    // Twin parallel steel accelerator rails + central glowing plasma conduit + 5 pulsing coils.
    vb.part(pl, &src.cube, &steel, Vec3::new(0.0, 0.034, -0.42), Vec3::new(0.034, 0.016, 0.30), id);
    vb.part(pl, &src.cube, &steel, Vec3::new(0.0, -0.034, -0.42), Vec3::new(0.034, 0.016, 0.30), id);
    vb.part(pl, &src.cyl, &coil, Vec3::new(0.0, 0.0, -0.40), Vec3::new(0.016, 0.32, 0.016), rx);
    for k in 0..5 {
        commands.spawn((
            PlasmaCoil(k),
            vm_part(pl, &ring, &coil, Vec3::new(0.0, 0.0, -0.29 - k as f32 * 0.054), Vec3::new(0.044, 0.052, 0.044), rx),
        ));
    }
    // Flared triple-pronged plasma emitter crown around muzzle lens.
    for a in [0.0f32, TAU / 3.0, 2.0 * TAU / 3.0] {
        let off = Vec3::new(a.cos() * 0.034, a.sin() * 0.034, -0.575);
        vb.part(pl, &src.cube, &steel, off, Vec3::new(0.012, 0.012, 0.06), Quat::from_rotation_z(a));
    }
    vb.part(pl, &src.sphere, &coil, Vec3::new(0.0, 0.0, -0.555), Vec3::splat(0.022), id);
    // Side copper cooling radiator fins with glowing heat vents.
    for k in 0..5 {
        let z = -0.22 + k as f32 * 0.042;
        vb.part(pl, &src.cube, &copper, Vec3::new(0.047, 0.012, z), Vec3::new(0.012, 0.052, 0.014), id);
        vb.part(pl, &src.cube, &vent_amber, Vec3::new(0.045, 0.012, z + 0.021), Vec3::new(0.006, 0.036, 0.008), id);
    }
    // Side-mounted pressurized glowing cyan plasma cell canister + dual steel clamp rings + conduit.
    vb.part(pl, &src.cyl, &coil, Vec3::new(-0.062, -0.008, -0.07), Vec3::new(0.022, 0.18, 0.022), rx);
    for z in [-0.135f32, -0.005] {
        vb.part(pl, &src.cyl, &dark_steel, Vec3::new(-0.062, -0.008, z), Vec3::new(0.026, 0.022, 0.026), rx);
    }
    vb.part(pl, &src.cap, &brushed, Vec3::new(-0.052, -0.012, -0.20), Vec3::new(0.012, 0.06, 0.012), Quat::from_rotation_x(1.3) * Quat::from_rotation_z(-0.45));
    // Grip + gloved right hand.
    vb.part(pl, &src.cap, &grip, Vec3::new(0.0, -0.095, 0.035), Vec3::new(0.046, 0.072, 0.052), Quat::from_rotation_x(-0.32));
    vb.part(pl, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.045), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.3));
    vb.part(pl, &src.cube, &glove_plate, Vec3::new(0.036, -0.096, 0.03), Vec3::new(0.022, 0.048, 0.068), Quat::from_rotation_x(-0.28));
    vb.part(pl, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.17), Vec3::new(0.088, 0.11, 0.088), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2));
    // Layered plasma muzzle flash (bright core + additive outer halo).
    let pl_flash = commands
        .spawn((ChildOf(pl), MuzzleFlash(Weapon::Plasma), Transform::from_xyz(0.0, 0.0, -0.60), Visibility::Hidden))
        .id();
    vb.part(pl_flash, &src.sphere, &flash_cyan, Vec3::ZERO, Vec3::splat(0.045), id);
    vb.part(pl_flash, &src.sphere, &flash_cyan_halo, Vec3::ZERO, Vec3::splat(0.095), id);
    vb.part(pl_flash, &src.thin_ring, &flash_cyan, Vec3::new(0.0, 0.0, -0.01), Vec3::new(0.075, 0.015, 0.075), rx);

    // ---- Weapon 3: Signal-Mortar (chunky industrial Mars rocket tube) ----
    let rk = commands.spawn((ChildOf(wroot), GunModel(Weapon::Rocket), Transform::default(), Visibility::Hidden)).id();
    // Main launch tube + dark titanium barrel shroud + heavy muzzle collar & bore.
    vb.part(rk, &src.cyl, &titanium, Vec3::new(0.0, 0.012, -0.20), Vec3::new(0.052, 0.62, 0.052), rx);
    vb.part(rk, &src.cyl, &dark_steel, Vec3::new(0.0, 0.012, -0.43), Vec3::new(0.059, 0.20, 0.059), rx);
    vb.part(rk, &src.cyl, &heat_metal, Vec3::new(0.0, 0.012, -0.535), Vec3::new(0.066, 0.045, 0.066), rx);
    vb.part(rk, &src.cyl, &grip, Vec3::new(0.0, 0.012, -0.559), Vec3::new(0.044, 0.008, 0.044), rx);
    for k in 0..4 {
        let a = k as f32 * TAU / 4.0 + std::f32::consts::FRAC_PI_4;
        vb.part(rk, &src.cube, &dark_steel, Vec3::new(a.cos() * 0.062, 0.012 + a.sin() * 0.062, -0.43), Vec3::new(0.012, 0.012, 0.18), Quat::from_rotation_z(a));
    }
    // Rear exhaust bell + dark venturi.
    vb.part(rk, &src.cyl, &steel, Vec3::new(0.0, 0.012, 0.125), Vec3::new(0.06, 0.05, 0.06), rx);
    vb.part(rk, &src.cyl, &grip, Vec3::new(0.0, 0.012, 0.151), Vec3::new(0.046, 0.006, 0.046), rx);
    // Hazard-striped casing bands.
    for k in 0..5 {
        let m = if k % 2 == 0 { &hazard } else { &dark_steel };
        vb.part(rk, &src.cyl, m, Vec3::new(0.0, 0.012, -0.30 + k as f32 * 0.04), Vec3::new(0.0545, 0.02, 0.0545), rx);
    }
    // Glowing amber warhead chamber: dark cradle on top with a lit window + side port.
    vb.part(rk, &src.cube, &dark_steel, Vec3::new(0.0, 0.058, -0.05), Vec3::new(0.05, 0.03, 0.15), id);
    vb.part(rk, &src.cyl, &warhead_glow, Vec3::new(0.0, 0.066, -0.05), Vec3::new(0.017, 0.12, 0.017), rx);
    vb.part(rk, &src.cube, &warhead_glow, Vec3::new(0.054, 0.012, -0.06), Vec3::new(0.006, 0.026, 0.09), id);
    vb.part(rk, &src.cube, &warhead_glow, Vec3::new(-0.054, 0.012, -0.06), Vec3::new(0.006, 0.026, 0.09), id);
    // Rear optical sight frame + amber reticle.
    for x in [-0.024f32, 0.024] {
        vb.part(rk, &src.cube, &dark_steel, Vec3::new(x, 0.098, 0.03), Vec3::new(0.007, 0.05, 0.03), id);
    }
    vb.part(rk, &src.cube, &dark_steel, Vec3::new(0.0, 0.122, 0.03), Vec3::new(0.055, 0.007, 0.03), id);
    vb.part(rk, &src.sphere, &warhead_glow, Vec3::new(0.0, 0.098, 0.028), Vec3::splat(0.0045), id);
    vb.part(rk, &src.sphere, &sight_red, Vec3::new(0.0, 0.07, -0.53), Vec3::splat(0.005), id);
    // Braced stabilizer rails under the tube + cross braces + bolts.
    for x in [-0.052f32, 0.052] {
        vb.part(rk, &src.cube, &steel, Vec3::new(x, -0.038, -0.25), Vec3::new(0.01, 0.014, 0.42), id);
    }
    for z in [-0.42f32, -0.25, -0.08] {
        vb.part(rk, &src.cube, &brushed, Vec3::new(0.0, -0.038, z), Vec3::new(0.114, 0.01, 0.018), id);
        for x in [-0.058f32, 0.058] {
            vb.part(rk, &src.cyl, &bolt_silver, Vec3::new(x, -0.038, z), Vec3::new(0.005, 0.004, 0.005), rz);
        }
    }
    // Front vertical grip + rear pistol grip + trigger guard + trigger.
    vb.part(rk, &src.cap, &grip, Vec3::new(0.0, -0.09, -0.30), Vec3::new(0.034, 0.07, 0.04), Quat::from_rotation_x(0.12));
    vb.part(rk, &src.cap, &grip, Vec3::new(0.0, -0.095, 0.035), Vec3::new(0.046, 0.072, 0.052), Quat::from_rotation_x(-0.32));
    vb.part(rk, &src.cube, &dark_steel, Vec3::new(0.0, -0.064, -0.03), Vec3::new(0.014, 0.006, 0.075), id);
    vb.part(rk, &src.cube, &steel, Vec3::new(0.0, -0.05, -0.01), Vec3::new(0.006, 0.022, 0.01), Quat::from_rotation_x(0.35));
    // Gloved right hand + armored cuff.
    vb.part(rk, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.045), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.3));
    vb.part(rk, &src.cube, &glove_plate, Vec3::new(0.036, -0.096, 0.03), Vec3::new(0.022, 0.048, 0.068), Quat::from_rotation_x(-0.28));
    vb.part(rk, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.17), Vec3::new(0.088, 0.11, 0.088), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2));
    vb.part(rk, &src.cyl, &glove_plate, Vec3::new(0.048, -0.155, 0.135), Vec3::new(0.052, 0.036, 0.052), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2));
    // Fiery mortar muzzle flash (cone + hot core + additive halo + ring).
    let rk_flash = commands
        .spawn((ChildOf(rk), MuzzleFlash(Weapon::Rocket), Transform::from_xyz(0.0, 0.012, -0.60), Visibility::Hidden))
        .id();
    vb.part(rk_flash, &src.cone, &flash_fire, Vec3::new(0.0, 0.0, -0.06), Vec3::new(0.06, 0.16, 0.06), Quat::from_rotation_x(-FRAC_PI_2));
    vb.part(rk_flash, &src.sphere, &flash_core, Vec3::ZERO, Vec3::splat(0.05), id);
    vb.part(rk_flash, &src.sphere, &flash_amber_halo, Vec3::ZERO, Vec3::splat(0.11), id);
    vb.part(rk_flash, &src.thin_ring, &flash_fire, Vec3::new(0.0, 0.0, 0.01), Vec3::new(0.085, 0.015, 0.085), rx);

    // =========================================================================
    // LEFT HAND: ELABORATE TACTICAL SMARTPHONE WITH LIVE 160x240 OLED DISPLAY
    // =========================================================================
    let phone_rot = Quat::from_rotation_y(0.24) * Quat::from_rotation_x(0.11) * Quat::from_rotation_z(-0.04);
    let proot = commands
        .spawn((ChildOf(cam), PhoneViewmodel, PhoneSkin(skin), Transform::from_translation(PHONE_REST).with_rotation(phone_rot), Visibility::default()))
        .id();
    // Main ruggedized anodized chassis + recessed stepped titanium frame rails.
    commands.spawn((PhoneSkinPart(SkinSlot::Bezel), vm_part(proot, &cube, &bezel, Vec3::ZERO, Vec3::new(0.136, 0.214, 0.020), id)));
    commands.spawn((PhoneSkinPart(SkinSlot::Frame), vm_part(proot, &cube, &phone_frame, Vec3::new(0.0, 0.0, -0.002), Vec3::new(0.142, 0.200, 0.014), id)));
    // Rubberized grip ribs along left and right chassis edges.
    for k in 0..6 {
        let y = -0.062 + k as f32 * 0.024;
        for s in [-1.0f32, 1.0] {
            vb.part(proot, &src.cube, &grip, Vec3::new(0.072 * s, y, 0.0), Vec3::new(0.005, 0.010, 0.016), id);
        }
    }
    // 4 sculpted rugged corner armor bumpers with silver hex bolts.
    for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        commands.spawn((PhoneSkinPart(SkinSlot::Bumper), vm_part(proot, &cube, &bumper, Vec3::new(x * 0.066, y * 0.104, 0.001), Vec3::new(0.018, 0.018, 0.024), id)));
        commands.spawn((
            PhoneSkinPart(SkinSlot::Bolts),
            vm_part(proot, &cyl, &phone_bolts, Vec3::new(x * 0.064, y * 0.102, 0.0135), Vec3::new(0.0032, 0.003, 0.0032), rx),
        ));
    }
    // Top bulkhead: speaker grille slot + front-facing lens + blinking status LED.
    vb.part(proot, &src.cube, &dark_steel, Vec3::new(0.0, 0.097, 0.0105), Vec3::new(0.036, 0.005, 0.003), id);
    vb.part(proot, &src.cyl, &brushed, Vec3::new(-0.032, 0.097, 0.0105), Vec3::new(0.0045, 0.003, 0.0045), rx);
    commands.spawn((PhoneLed, vm_part(proot, &sphere, &led, Vec3::new(0.034, 0.097, 0.0115), Vec3::splat(0.0038), id)));
    // Bottom chin tactile home bar + lanyard loop.
    vb.part(proot, &src.cube, &brushed, Vec3::new(0.0, -0.098, 0.0105), Vec3::new(0.044, 0.006, 0.003), id);
    // Side hardware controls: orange PTT button on left, volume rockers on right.
    commands.spawn((PhoneSkinPart(SkinSlot::Ptt), vm_part(proot, &cube, &phone_ptt, Vec3::new(-0.074, 0.036, 0.0), Vec3::new(0.006, 0.026, 0.010), id)));
    vb.part(proot, &src.cube, &steel, Vec3::new(0.074, 0.046, 0.0), Vec3::new(0.005, 0.018, 0.008), id);
    vb.part(proot, &src.cube, &steel, Vec3::new(0.074, 0.020, 0.0), Vec3::new(0.005, 0.018, 0.008), id);
    // Knurled antenna collar + telescoping tactical whip antenna + helical coil + glowing beacon bead.
    vb.part(proot, &src.cyl, &brushed, Vec3::new(0.048, 0.114, -0.001), Vec3::new(0.011, 0.018, 0.011), id);
    vb.part(proot, &src.cyl, &dark_steel, Vec3::new(0.048, 0.145, -0.001), Vec3::new(0.0048, 0.052, 0.0048), id);
    commands.spawn((PhoneSkinPart(SkinSlot::Coil), vm_part(proot, &thin_ring, &phone_coil, Vec3::new(0.048, 0.132, -0.001), Vec3::new(0.008, 0.006, 0.008), id)));
    commands.spawn((PhoneLed, vm_part(proot, &sphere, &led, Vec3::new(0.048, 0.174, -0.001), Vec3::splat(0.0062), id)));
    // Raised screen surround lip + LIVE 160x240 pixel-art OLED screen quad!
    vb.part(proot, &src.cube, &grip, Vec3::new(0.0, 0.0, 0.0096), Vec3::new(0.123, 0.182, 0.002), id);
    commands.spawn((
        PhoneScreen,
        ChildOf(proot),
        Mesh3d(screen_quad),
        MeshMaterial3d(screen),
        Transform::from_xyz(0.0, 0.0, 0.0112),
        overlay_layer(),
    ));
    // Articulated gloved marine left hand & armored cuff gripping bottom & sides (never blocking the screen!).
    vb.part(proot, &src.cap, &glove, Vec3::new(-0.012, -0.118, -0.022), Vec3::new(0.088, 0.058, 0.058), rz); // palm base
    vb.part(proot, &src.cap, &glove, Vec3::new(-0.072, -0.042, -0.014), Vec3::new(0.026, 0.088, 0.032), Quat::from_rotation_z(-0.08)); // left hand heel
    // 3 wrapped armored fingers gripping the right spine from behind.
    for k in 0..3 {
        let y = -0.058 + k as f32 * 0.030;
        vb.part(proot, &src.cap, &glove, Vec3::new(0.072, y, -0.004), Vec3::new(0.018, 0.022, 0.022), rz);
        vb.part(proot, &src.cube, &glove_plate, Vec3::new(0.076, y, 0.004), Vec3::new(0.012, 0.014, 0.012), id);
    }
    // Curled thumb gripping the lower-left armor bumper cleanly clear of the OLED screen!
    vb.part(proot, &src.cap, &glove, Vec3::new(-0.080, -0.098, 0.011), Vec3::new(0.018, 0.030, 0.018), Quat::from_rotation_z(-0.42));
    // Left armored forearm descending into bottom-left corner.
    vb.part(proot, &src.cap, &glove, Vec3::new(-0.045, -0.20, -0.065), Vec3::new(0.082, 0.11, 0.072), Quat::from_rotation_x(0.45) * Quat::from_rotation_z(-0.22));
    vb.part(proot, &src.cyl, &glove_plate, Vec3::new(-0.036, -0.165, -0.05), Vec3::new(0.048, 0.034, 0.048), Quat::from_rotation_x(0.45) * Quat::from_rotation_z(-0.22));
    for (parent, mesh, mat) in vb.finish(&mut meshes) {
        commands.spawn(vm_part(parent, &mesh, &mat, Vec3::ZERO, Vec3::ONE, Quat::IDENTITY));
    }
}

fn sync_overlay_fov(main: Query<&Projection, (With<PlayerCamera>, Without<OverlayCamera>)>, mut overlay: Query<&mut Projection, With<OverlayCamera>>) {
    let (Ok(Projection::Perspective(m)), Ok(mut o)) = (main.single(), overlay.single_mut()) else { return };
    if let Projection::Perspective(op) = o.as_mut()
        && (op.fov - m.fov).abs() > 1e-4
    {
        op.fov = m.fov;
    }
}

/// Damping rate (1/s) of the viewmodel walk-bob amplitude.
const VIEWMODEL_BOB_RATE: f32 = 8.0;

/// Target viewmodel walk-bob amplitude: speed-scaled on the ground, faint in the air.
pub fn viewmodel_bob_target(grounded: bool, horizontal_speed: f32) -> f32 {
    if grounded { (horizontal_speed / crate::player::MOVE_SPEED).min(1.3) } else { 0.15 }
}

/// Damp the bob amplitude toward its target, so a one-frame bhop touchdown
/// barely moves it instead of spiking the gun for a frame.
pub fn damp_bob_amp(current: f32, target: f32, dt: f32) -> f32 {
    current + (target - current) * smooth_factor(VIEWMODEL_BOB_RATE, dt)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn animate_viewmodels(
    mut bob_amp: Local<f32>,
    time: Res<Time>,
    ws: Res<WeaponState>,
    apps: Res<PhoneApps>,
    signal: Res<SignalState>,
    pm: Option<Res<PhoneMaterials>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    players: Query<&Player>,
    mut set: ParamSet<(
        Query<&mut Transform, With<WeaponViewmodel>>,
        Query<&mut Transform, With<PhoneViewmodel>>,
        Query<&mut Transform, With<PumpForend>>,
        Query<(&mut Transform, &PlasmaCoil)>,
        Query<&mut Transform, With<MuzzleStar>>,
    )>,
    mut models: Query<(&GunModel, &mut Visibility), Without<MuzzleFlash>>,
    mut flashes: Query<(&MuzzleFlash, &mut Visibility), Without<GunModel>>,
    mut mlight: Query<&mut PointLight, With<MuzzleLight>>,
) {
    let Ok(p) = players.single() else { return };
    let t = time.elapsed_secs();
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    *bob_amp = damp_bob_amp(*bob_amp, viewmodel_bob_target(p.grounded, speed), time.delta_secs());
    let amp = *bob_amp;
    let bob = Vec3::new((p.bob_phase * 0.5).cos() * 0.011 * amp, -(p.bob_phase.sin().abs()) * 0.013 * amp, 0.0)
        + Vec3::new(0.0, (t * 1.6).sin() * 0.0025, 0.0);
    let half = SWAP_SECS * 0.5;
    let swap = if ws.swap_timer > 0.0 { 1.0 - ((ws.swap_timer - half).abs() / half).min(1.0) } else { 0.0 };
    let shown = ws.displayed();

    if let Ok(mut tf) = set.p0().single_mut() {
        let kick = ws.recoil;
        tf.translation = WEAPON_REST + bob + Vec3::new(0.0, -0.3 * swap + kick * 0.022, kick * 0.095);
        tf.rotation = Quat::from_rotation_x(kick * 0.36 - swap * 0.6) * Quat::from_rotation_z(-swap * 0.3);
    }
    let cast = apps.cast_anim.clamp(0.0, 1.0);
    if let Ok(mut tf) = set.p1().single_mut() {
        tf.translation = PHONE_REST + bob * 0.55 + Vec3::new(0.045 * cast, 0.055 * cast, 0.03 * cast);
        tf.rotation = Quat::from_rotation_y(0.24 + 0.10 * cast) * Quat::from_rotation_x(0.11 + 0.18 * cast) * Quat::from_rotation_z(-0.04);
    }
    if let Ok(mut tf) = set.p2().single_mut() {
        let k = (ws.pump * std::f32::consts::PI).sin().max(0.0);
        tf.translation.z = -0.30 + k * 0.095;
    }
    for (mut tf, coil) in &mut set.p3() {
        let s = 1.0 + 0.14 * ((t * 14.0) - coil.0 as f32 * 0.9).sin() + ws.recoil * 0.45;
        tf.scale = Vec3::new(0.044 * s, 0.052, 0.044 * s);
    }
    if let Ok(mut tf) = set.p4().single_mut() {
        let k = (ws.muzzle / 0.06).clamp(0.0, 1.0);
        tf.rotation = Quat::from_rotation_z(ws.muzzle_roll);
        tf.scale = Vec3::splat(0.55 + 0.78 * k);
    }
    for (m, mut v) in &mut models {
        *v = if m.0 == shown { Visibility::Inherited } else { Visibility::Hidden };
    }
    for (f, mut v) in &mut flashes {
        *v = if f.0 == shown && ws.muzzle > 0.0 { Visibility::Inherited } else { Visibility::Hidden };
    }
    if let Ok(mut l) = mlight.single_mut() {
        l.intensity = if ws.muzzle > 0.0 { 70_000.0 } else { 0.0 };
        l.color = match shown {
            Weapon::Plasma => Color::srgb(0.3, 0.95, 1.0),
            Weapon::Rocket => Color::srgb(1.0, 0.5, 0.15),
            Weapon::Shotgun => Color::srgb(1.0, 0.72, 0.35),
        };
    }

    // Animate phone status LED + antenna beacon + plasma coils.
    let Some(pm) = pm else { return };
    let mut c = signal_color(signal.bars).to_linear();
    let mut pulse = 4.0 + 3.0 * (t * (3.0 + signal.bars as f32 * 1.5)).sin().max(0.0);
    if signal.in_dead_zone {
        pulse = if (t * 8.0).sin() > 0.2 { 6.0 } else { 0.4 };
        c = LinearRgba::rgb(1.0, 0.12, 0.08);
    }
    if apps.ping_active_timer > 0.0 || apps.overload_flash_timer > 0.0 {
        c = LinearRgba::rgb(0.3, 1.0, 1.0);
        pulse = 10.0;
    }
    if let Some(mut m) = materials.get_mut(&pm.led) {
        m.emissive = LinearRgba::rgb(c.red * pulse, c.green * pulse, c.blue * pulse);
    }
    if let Some(mut m) = materials.get_mut(&pm.coil) {
        let k = if ws.displayed() == Weapon::Plasma { 1.0 + ws.recoil * 3.5 } else { 1.0 };
        m.emissive = LinearRgba::rgb(0.8, 6.0, 8.5) * k;
    }
}

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

#[derive(Resource, Clone)]
pub struct BoltAssets {
    pub core_mesh: Handle<Mesh>,
    pub halo_mesh: Handle<Mesh>,
    pub core_mat: Handle<StandardMaterial>,
    pub halo_mat: Handle<StandardMaterial>,
}

fn create_bolt_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(BoltAssets {
        core_mesh: meshes.add(Capsule3d::new(0.045, 0.48)),
        halo_mesh: meshes.add(Capsule3d::new(0.11, 0.62)),
        core_mat: materials.add(StandardMaterial {
            base_color: Color::srgb(0.92, 1.0, 1.0),
            emissive: LinearRgba::rgb(8.0, 18.0, 22.0),
            unlit: true,
            ..default()
        }),
        halo_mat: materials.add(additive_glow(Color::srgb(0.18, 0.9, 1.0), 0.55)),
    });
}

/// Pre-baked rocket projectile model (one merged mesh per material, built
/// along +Y with the nose up) shared by every rocket in flight.
#[derive(Resource, Clone)]
pub struct RocketAssets {
    pub models: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
}

fn create_rocket_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let casing = materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.34, 0.36), metallic: 0.85, perceptual_roughness: 0.35, ..default() });
    let fins = materials.add(StandardMaterial { base_color: Color::srgb(0.12, 0.13, 0.14), metallic: 0.7, perceptual_roughness: 0.5, ..default() });
    let core = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.7, 0.3),
        emissive: LinearRgba::rgb(14.0, 5.5, 0.8),
        unlit: true,
        ..default()
    });
    let flame = materials.add(additive_glow(Color::srgb(1.0, 0.45, 0.08), 0.7));
    let cyl = Mesh::from(Cylinder::new(1.0, 1.0));
    let cone = Mesh::from(Cone::new(1.0, 1.0));
    let cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
    let sphere = Sphere::new(1.0).mesh().uv(12, 8);
    let mut b = MaterialBatches::new();
    let t = |p: Vec3, s: Vec3| Transform::from_translation(p).with_scale(s);
    b.add(&cyl, &casing, t(Vec3::ZERO, Vec3::new(0.07, 0.42, 0.07)));
    b.add(&cone, &core, t(Vec3::new(0.0, 0.29, 0.0), Vec3::new(0.07, 0.16, 0.07)));
    b.add(&cyl, &core, t(Vec3::new(0.0, 0.12, 0.0), Vec3::new(0.074, 0.05, 0.074)));
    for a in [0.0f32, FRAC_PI_2] {
        b.add(&cube, &fins, Transform::from_xyz(0.0, -0.17, 0.0).with_rotation(Quat::from_rotation_y(a)).with_scale(Vec3::new(0.24, 0.1, 0.012)));
    }
    b.add(&cone, &flame, Transform::from_xyz(0.0, -0.33, 0.0).with_rotation(Quat::from_rotation_x(std::f32::consts::PI)).with_scale(Vec3::new(0.06, 0.24, 0.06)));
    b.add(&sphere, &flame, t(Vec3::new(0.0, -0.24, 0.0), Vec3::splat(0.12)));
    commands.insert_resource(RocketAssets { models: b.finish(&mut meshes) });
}

/// World velocity of a freshly fired rocket: inherits half of the shooter's
/// forward velocity along `dir` so high-speed bhoppers (up to 48 m/s) can never
/// overtake their own forward rockets.
pub fn rocket_velocity(dir: Vec3, shooter_vel: Vec3) -> Vec3 {
    let dir = dir.normalize_or_zero();
    let forward_carry = shooter_vel.dot(dir).max(0.0) * 0.5;
    dir * (ROCKET_SPEED + forward_carry)
}

/// Spawn a rocket in flight at `start` heading along `dir`.
pub fn spawn_rocket(commands: &mut Commands, assets: &RocketAssets, start: Vec3, dir: Vec3, shooter_vel: Vec3, damage_mult: f32) {
    let mut root = commands.spawn((
        LevelEntity,
        PlayerProjectile::rocket(rocket_velocity(dir, shooter_vel), damage_mult),
        Transform::from_translation(start).with_rotation(Quat::from_rotation_arc(Vec3::Y, dir)),
        Visibility::default(),
    ));
    if let Some((mesh, mat)) = assets.models.first() {
        root.insert((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone())));
    }
    let root = root.id();
    for (mesh, mat) in assets.models.iter().skip(1) {
        commands.spawn((ChildOf(root), Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::IDENTITY));
    }
    commands.spawn((
        ChildOf(root),
        CullableLight::for_range(7.0),
        PointLight { color: Color::srgb(1.0, 0.55, 0.15), intensity: 120_000.0, range: 7.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, -0.3, 0.0),
    ));
}

/// Reused per-shot buffers of [`fire_weapons`].
#[derive(Default)]
struct FireScratch {
    targets: Vec<HitTargetInfo>,
    jammers: Vec<Vec3>,
    enemy_dmg: Vec<f32>,
    enemy_kb: Vec<Vec3>,
    jam_dmg: Vec<f32>,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn fire_weapons(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    (mouse, scroll, captured): (Res<ButtonInput<MouseButton>>, Res<AccumulatedMouseScroll>, Res<CursorCaptured>),
    grid: Res<LevelGrid>,
    mods: Res<PlayerModifiers>,
    signal: Res<SignalState>,
    mut ws: ResMut<WeaponState>,
    (fx, bolts, rockets): (Res<FxAssets>, Res<BoltAssets>, Res<RocketAssets>),
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    cams: Query<&Transform, With<PlayerCamera>>,
    mut players: Query<(&mut Player, &Transform), Without<PlayerCamera>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    mut jammers: Query<(&mut DemonJammer, &GlobalTransform)>,
    mut bot: Option<ResMut<BotControl>>,
    mut scratch: Local<FireScratch>,
) {
    let dt = time.delta_secs();
    let Ok((mut player, body)) = players.single_mut() else { return };
    let Ok(cam_local) = cams.single() else { return };
    // This frame's eye pose (moved + turned earlier in `Update`), not last
    // frame's `GlobalTransform`.
    let cam = live_eye_pose(body, cam_local);
    ws.cooldown = (ws.cooldown - dt).max(0.0);
    ws.swap_timer = (ws.swap_timer - dt).max(0.0);
    ws.recoil = (ws.recoil - dt * 5.0).max(0.0);
    ws.muzzle = (ws.muzzle - dt).max(0.0);
    if ws.pump > 0.0 {
        ws.pump = (ws.pump - dt / 0.35).max(0.0);
    }

    // Emergency plasma trickle: combat can never soft-lock.
    if player.ammo_shells == 0 && player.ammo_rockets == 0 && player.ammo_plasma < 10 {
        ws.trickle += dt;
        if ws.trickle >= TRICKLE_SECS {
            ws.trickle -= TRICKLE_SECS;
            player.ammo_plasma += 1;
        }
    } else {
        ws.trickle = 0.0;
    }

    // Weapon switching.
    let mut want = None;
    if keys.just_pressed(KeyCode::Digit1) {
        want = Some(Weapon::Shotgun);
    } else if keys.just_pressed(KeyCode::Digit2) {
        want = Some(Weapon::Plasma);
    } else if keys.just_pressed(KeyCode::Digit3) {
        want = Some(Weapon::Rocket);
    } else if keys.just_pressed(KeyCode::Tab) || scroll.delta.y < 0.0 {
        want = Some(ws.current.next());
    } else if scroll.delta.y > 0.0 {
        want = Some(ws.current.prev());
    } else if let Some(w) = bot.as_mut().and_then(|b| b.switch_weapon.take()) {
        want = Some(w);
    }
    if let Some(w) = want
        && ws.switch_to(w)
    {
        sfx.play(SfxEvent::WeaponSwap);
    }

    let bot_fire = bot.as_ref().is_some_and(|b| b.fire);
    let trigger = (captured.0 && mouse.pressed(MouseButton::Left)) || ws.force_fire || bot_fire;
    if !trigger || ws.cooldown > 0.0 || ws.swap_timer > SWAP_SECS * 0.4 {
        return;
    }
    if !ws.current.has_ammo(&player) {
        if let Some(other) = ws.current.auto_swap_target(&player)
            && ws.switch_to(other)
        {
            sfx.play(SfxEvent::WeaponSwap);
        } else if mouse.just_pressed(MouseButton::Left) {
            sfx.play(SfxEvent::Denied);
        }
        return;
    }
    ws.force_fire = false;
    ws.cooldown = fire_cooldown(ws.current, &mods, signal.bars);

    let origin = cam.translation;
    let rot = cam.rotation;
    let fwd = rot * Vec3::NEG_Z;
    let muzzle = origin + rot * Vec3::new(0.2, -0.17, -0.85);
    // Doppler Overdrive: speed at the moment of the shot sets the bonus.
    let speed = Vec2::new(player.velocity.x, player.velocity.z).length();
    let dmult = doppler_damage_mult(speed, &mods);

    // Snapshot lightweight targets for ray tests (reused buffers).
    let FireScratch { targets: refs, jammers: jam_list, enemy_dmg, enemy_kb, jam_dmg } = &mut *scratch;
    refs.clear();
    refs.extend(enemies.iter().map(|(e, g, _)| HitTargetInfo::of(g.translation(), e)));
    jam_list.clear();
    jam_list.extend(jammers.iter().filter(|(j, _)| j.is_active()).map(|(_, g)| g.translation()));

    match ws.current {
        Weapon::Shotgun => {
            player.ammo_shells -= 1;
            ws.recoil = 1.0;
            ws.muzzle = 0.06;
            ws.muzzle_roll = rand::rng().random_range(0.0..std::f32::consts::FRAC_PI_2);
            ws.pump = 1.0;
            sfx.play(SfxEvent::Shotgun);
            spawn_flash_light(&mut commands, muzzle, Color::srgb(1.0, 0.72, 0.35), 820_000.0, 13.0, 0.08);
            let right_dir = rot * Vec3::X;
            let up_dir = rot * Vec3::Y;
            let breach = origin + rot * Vec3::new(0.29, -0.18, -0.42);
            spawn_ejected_shell(&mut commands, &fx, breach, right_dir, up_dir, fwd);
            spawn_particles(&mut commands, &fx, muzzle, BurstColor::Smoke, 2, 1.2, 0.06);
            let mut rng = rand::rng();
            let pellets = SHOTGUN_PELLETS + mods.extra_pellets;
            let spread = PELLET_SPREAD * mods.pellet_spread_mult * doppler_spread_mult(speed, &mods);
            enemy_dmg.clear();
            enemy_dmg.resize(refs.len(), 0.0);
            enemy_kb.clear();
            enemy_kb.resize(refs.len(), Vec3::ZERO);
            jam_dmg.clear();
            jam_dmg.resize(jam_list.len(), 0.0);
            let mut wall_sparks = 0;
            for i in 0..pellets {
                let a = rng.random_range(0.0..TAU);
                let r = spread * rng.random::<f32>().sqrt();
                let local = Vec3::new(a.cos() * r, a.sin() * r, -1.0).normalize();
                let dir = rot * local;
                let (dist, target) = resolve_hitscan_targets(&grid, origin, dir, HITSCAN_RANGE, refs, jam_list);
                let hit = origin + dir * dist;
                let falloff = (1.0 - (dist - 18.0).max(0.0) / 40.0).clamp(0.4, 1.0);
                if i % 2 == 0 {
                    spawn_tracer(&mut commands, &fx, muzzle, hit);
                }
                match target {
                    HitTarget::Enemy(k) => {
                        enemy_dmg[k] += PELLET_DAMAGE * falloff * dmult;
                        enemy_kb[k] += dir * 1.1;
                        spawn_particles(&mut commands, &fx, hit, BurstColor::Gore, 3, 4.2, 0.075);
                    }
                    HitTarget::Jammer(k) => {
                        jam_dmg[k] += PELLET_DAMAGE * falloff * dmult;
                        spawn_particles(&mut commands, &fx, hit, BurstColor::Spark, 4, 5.5, 0.07);
                    }
                    HitTarget::Wall => {
                        spawn_particles(&mut commands, &fx, hit - dir * 0.05, BurstColor::Spark, 3, 3.8, 0.055);
                        if wall_sparks % 3 == 0 {
                            spawn_particles(&mut commands, &fx, hit - dir * 0.06, BurstColor::Smoke, 1, 0.8, 0.09);
                        }
                        wall_sparks += 1;
                    }
                    HitTarget::Nothing => {}
                }
            }
            let _ = wall_sparks;
            let mut any_hit = false;
            for (idx, (mut e, _, marked)) in enemies.iter_mut().enumerate() {
                if enemy_dmg[idx] > 0.0 {
                    let kb = enemy_kb[idx] * knockback_scale(e.kind);
                    damage_enemy(&mut e, enemy_dmg[idx], marked.is_some(), &mods);
                    e.knockback += Vec3::new(kb.x, 0.0, kb.z) * 3.0;
                    any_hit = true;
                }
            }
            let mut j = 0;
            for (mut jm, _) in &mut jammers {
                if !jm.is_active() {
                    continue;
                }
                if jam_dmg[j] > 0.0 {
                    damage_jammer(&mut jm, jam_dmg[j]);
                    any_hit = true;
                }
                j += 1;
            }
            if any_hit {
                feed.hit(false);
                sfx.play(SfxEvent::Hit);
            }
        }
        Weapon::Plasma => {
            player.ammo_plasma -= 1;
            ws.recoil = 0.35;
            ws.muzzle = 0.04;
            sfx.play(SfxEvent::Plasma);
            spawn_flash_light(&mut commands, muzzle, Color::srgb(0.3, 0.9, 1.0), 250_000.0, 7.0, 0.05);
            // Converge the bolt onto whatever the crosshair points at.
            let (dist, _) = resolve_hitscan_targets(&grid, origin, fwd, HITSCAN_RANGE, refs, jam_list);
            let aim = origin + fwd * dist.max(2.0);
            let dir = (aim - muzzle).normalize_or_zero();
            commands.spawn((
                LevelEntity,
                PlayerProjectile::plasma(dir * PLASMA_SPEED, dmult),
                Mesh3d(bolts.core_mesh.clone()),
                MeshMaterial3d(bolts.core_mat.clone()),
                Transform::from_translation(muzzle).with_rotation(Quat::from_rotation_arc(Vec3::Y, dir)),
                children![
                    (Mesh3d(bolts.halo_mesh.clone()), MeshMaterial3d(bolts.halo_mat.clone()), Transform::default()),
                    (
                        CullableLight::for_range(6.0),
                        PointLight { color: Color::srgb(0.3, 0.95, 1.0), intensity: 85_000.0, range: 6.0, shadow_maps_enabled: false, ..default() },
                        Transform::default(),
                    ),
                ],
            ));
        }
        Weapon::Rocket => {
            player.ammo_rockets -= 1;
            ws.recoil = 1.4;
            ws.muzzle = 0.09;
            sfx.play(SfxEvent::RocketFire);
            spawn_flash_light(&mut commands, muzzle, Color::srgb(1.0, 0.55, 0.15), 600_000.0, 10.0, 0.09);
            spawn_particles(&mut commands, &fx, muzzle, BurstColor::Smoke, 3, 1.4, 0.08);
            let (dist, _) = resolve_hitscan_targets(&grid, origin, fwd, HITSCAN_RANGE, refs, jam_list);
            let (start, dir) = rocket_launch(&grid, origin, fwd, muzzle, dist);
            spawn_rocket(&mut commands, &rockets, start, dir, player.velocity, dmult);
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn move_player_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    mods: Res<PlayerModifiers>,
    fx: Res<FxAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sfx: ResMut<SfxQueue>,
    mut feed: ResMut<HudFeed>,
    mut bolts: Query<(Entity, &mut PlayerProjectile, &mut Transform), Without<Player>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    mut jammers: Query<(&mut DemonJammer, &GlobalTransform)>,
    mut players: Query<(&mut Player, &Transform)>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    for (e, mut b, mut tf) in &mut bolts {
        b.life -= dt;
        let from = tf.translation;
        let step = b.vel * dt;
        let len = step.length();
        let dir = step / len.max(1e-6);
        let mut best = (len, HitTarget::Nothing);
        if let Some(h) = raycast_grid(&grid, from, dir, len) {
            best = (h.distance, HitTarget::Wall);
        }
        for (i, (en, g, _)) in enemies.iter().enumerate() {
            if en.is_alive()
                && let Some(t) = en.ray_hit(g.translation(), from, dir, best.0)
                && t < best.0
            {
                best = (t, HitTarget::Enemy(i));
            }
        }
        for (i, (j, g)) in jammers.iter().enumerate() {
            if j.is_active()
                && let Some(t) = jammer_ray_hit(g.translation(), from, dir)
                && t < best.0
            {
                best = (t, HitTarget::Jammer(i));
            }
        }
        let hit = from + dir * best.0;
        if b.kind == ProjectileKind::Rocket {
            if best.1 == HitTarget::Nothing && b.life > 0.0 {
                tf.translation = from + step;
                b.trail -= dt;
                if b.trail <= 0.0 {
                    b.trail += ROCKET_TRAIL_SECS;
                    let tail = tf.translation - dir * 0.35;
                    spawn_particles(&mut commands, &fx, tail, BurstColor::Smoke, 1, 0.6, 0.09);
                    spawn_particles(&mut commands, &fx, tail, BurstColor::Fire, 1, 1.2, 0.045);
                }
                continue;
            }
            let blast = if best.1 == HitTarget::Nothing { from } else { hit - dir * 0.08 };
            let any_hit = detonate_rocket(
                &mut commands,
                &grid,
                &fx,
                &mut materials,
                &mut sfx,
                blast,
                dir,
                best.1,
                b.damage_mult,
                &mods,
                &mut enemies,
                &mut jammers,
                &mut players,
            );
            if any_hit {
                feed.hit(false);
                sfx.play(SfxEvent::Hit);
            }
            commands.entity(e).try_despawn();
            continue;
        }
        let damage = b.damage * b.damage_mult;
        match best.1 {
            HitTarget::Nothing => {
                if b.life <= 0.0 {
                    commands.entity(e).try_despawn();
                } else {
                    tf.translation = from + step;
                }
                continue;
            }
            HitTarget::Enemy(i) => {
                if let Some((mut en, _, marked)) = enemies.iter_mut().nth(i) {
                    let kb = dir * 1.5 * knockback_scale(en.kind);
                    damage_enemy(&mut en, damage, marked.is_some(), &mods);
                    en.knockback += Vec3::new(kb.x, 0.0, kb.z);
                    feed.hit(false);
                    sfx.play(SfxEvent::Hit);
                }
            }
            HitTarget::Jammer(i) => {
                if let Some((mut j, _)) = jammers.iter_mut().nth(i) {
                    damage_jammer(&mut j, damage);
                    feed.hit(false);
                    sfx.play(SfxEvent::Hit);
                }
            }
            HitTarget::Wall => {}
        }
        spawn_burst(&mut commands, &fx, hit - dir * 0.1, BurstColor::Cyan, 7, 4.8, 0.065);
        spawn_shockwave(&mut commands, &fx, &mut materials, hit - dir * 0.12, 0.85, 0.18, BOLT_IMPACT_GLOW.0, BOLT_IMPACT_GLOW.1, false);
        commands.entity(e).try_despawn();
    }
}

/// Rocket detonation: direct hit (no splash double-dip on that target),
/// linear-falloff splash on demons and active jammers, Quake rocket jump on
/// the player and the explosion VFX. Returns whether any target was hurt.
#[allow(clippy::too_many_arguments)]
fn detonate_rocket(
    commands: &mut Commands,
    grid: &LevelGrid,
    fx: &FxAssets,
    materials: &mut Assets<StandardMaterial>,
    sfx: &mut SfxQueue,
    blast: Vec3,
    dir: Vec3,
    direct: HitTarget,
    damage_mult: f32,
    mods: &PlayerModifiers,
    enemies: &mut Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    jammers: &mut Query<(&mut DemonJammer, &GlobalTransform)>,
    players: &mut Query<(&mut Player, &Transform)>,
) -> bool {
    let radius = rocket_splash_radius(mods);
    let mut any_hit = false;
    for (i, (mut en, g, marked)) in enemies.iter_mut().enumerate() {
        if !en.is_alive() {
            continue;
        }
        let root = g.translation();
        let (dmg, falloff) = if direct == HitTarget::Enemy(i) {
            (ROCKET_DIRECT_DAMAGE * damage_mult, 1.0)
        } else {
            let d = splash_distance(blast, root, en.stats.hit_spheres);
            if d > radius || !has_line_of_sight(grid, blast, en.center(root)) {
                (0.0, 0.0)
            } else {
                (rocket_splash_damage(d, radius, damage_mult), splash_falloff(d, radius))
            }
        };
        if dmg <= 0.0 {
            continue;
        }
        let away = Vec2::new(root.x - blast.x, root.z - blast.z).normalize_or(Vec2::new(dir.x, dir.z).normalize_or_zero());
        let kb = 14.0 * falloff * knockback_scale(en.kind);
        damage_enemy(&mut en, dmg, marked.is_some(), mods);
        en.knockback += Vec3::new(away.x * kb, kb * 0.15, away.y * kb);
        any_hit = true;
    }
    for (i, (mut j, g)) in jammers.iter_mut().enumerate() {
        if !j.is_active() {
            continue;
        }
        let dmg = if direct == HitTarget::Jammer(i) {
            ROCKET_DIRECT_DAMAGE * damage_mult
        } else {
            let base = g.translation();
            let d = splash_distance(blast, base, &JAMMER_SPHERES);
            if d > radius || !has_line_of_sight(grid, blast, base + Vec3::Y * 1.3) {
                0.0
            } else {
                rocket_splash_damage(d, radius, damage_mult)
            }
        };
        if dmg > 0.0 {
            damage_jammer(&mut j, dmg);
            any_hit = true;
        }
    }
    if let Ok((mut p, ptf)) = players.single_mut()
        && has_line_of_sight(grid, blast, ptf.translation + Vec3::Y * 0.9)
        && let Some(jump) = compute_rocket_jump(ptf.translation, blast, radius, mods)
    {
        apply_rocket_jump(&mut p, &jump);
    }
    spawn_flash_light(commands, blast, Color::srgb(1.0, 0.5, 0.15), 2_600_000.0, 16.0, 0.28);
    spawn_shockwave(commands, fx, materials, blast, radius * 0.9, 0.32, ROCKET_BLAST_GLOW.0, ROCKET_BLAST_GLOW.1, true);
    spawn_shockwave(commands, fx, materials, blast, radius * 0.45, 0.2, ROCKET_CORE_GLOW.0, ROCKET_CORE_GLOW.1, false);
    spawn_burst(commands, fx, blast, BurstColor::Fire, 14, 9.0, 0.16);
    spawn_particles(commands, fx, blast, BurstColor::Spark, 8, 11.0, 0.06);
    spawn_particles(commands, fx, blast + Vec3::Y * 0.3, BurstColor::Smoke, 5, 2.4, 0.3);
    sfx.play(SfxEvent::RocketExplode);
    any_hit
}

#[allow(clippy::too_many_arguments)]
fn jammer_deaths(
    mut commands: Commands,
    fx: Res<FxAssets>,
    pickups: Res<PickupAssets>,
    mut run: ResMut<RunStats>,
    mut sfx: ResMut<SfxQueue>,
    mut feed: ResMut<HudFeed>,
    time: Res<Time>,
    mut jammers: Query<(Entity, &mut DemonJammer, &GlobalTransform, &Children)>,
    mut cores: Query<&mut Transform, With<JammerCore>>,
    (mods, mut materials): (Res<PlayerModifiers>, ResMut<Assets<StandardMaterial>>),
) {
    for (e, mut j, g, children) in &mut jammers {
        if j.is_active() {
            j.hit_flash = (j.hit_flash - time.delta_secs()).max(0.0);
            if j.hit_flash > 0.0 {
                for c in children.iter() {
                    if let Ok(mut tf) = cores.get_mut(c) {
                        tf.scale *= 1.0 + j.hit_flash * 3.0;
                    }
                }
            }
            continue;
        }
        let base = g.translation();
        commands.entity(e).try_despawn();
        spawn_burst(&mut commands, &fx, base + Vec3::Y * 2.7, BurstColor::Fire, 26, 10.0, 0.25);
        spawn_burst(&mut commands, &fx, base + Vec3::Y * 1.5, BurstColor::Gore, 16, 6.0, 0.2);
        spawn_burst(&mut commands, &fx, base + Vec3::Y * 2.0, BurstColor::Smoke, 10, 3.0, 0.4);
        spawn_flash_light(&mut commands, base + Vec3::Y * 2.5, Color::srgb(1.0, 0.5, 0.2), 3_000_000.0, 18.0, 0.4);
        run.jammers_destroyed += 1;
        sfx.play(SfxEvent::JammerDestroyed);
        feed.toast("JAMMER DESTROYED - SIGNAL BOOSTED", Color::srgb(0.3, 1.0, 0.8));
        feed.hit(true);
        if mods.jammer_hijack {
            spawn_hijack_hotspot(&mut commands, &fx, &mut materials, &mut feed, base);
        }
        for (i, k) in [PickupKind::Shells, PickupKind::Health, PickupKind::Plasma, PickupKind::Rockets].into_iter().enumerate() {
            let a = i as f32 * TAU / 4.0;
            spawn_pickup(&mut commands, &pickups, k, base + Vec3::new(a.cos(), 0.0, a.sin()) * 1.2, true);
        }
    }
}

/// Plasma bolt impact shockwave glow `(color, alpha)`.
const BOLT_IMPACT_GLOW: (Color, f32) = (Color::srgb(0.3, 0.95, 1.0), 0.7);
/// Rocket explosion shockwave ring glow `(color, alpha)`.
const ROCKET_BLAST_GLOW: (Color, f32) = (Color::srgb(1.0, 0.55, 0.12), 0.75);
/// Rocket explosion fireball core glow `(color, alpha)`.
const ROCKET_CORE_GLOW: (Color, f32) = (Color::srgb(1.0, 0.78, 0.35), 0.8);

fn prewarm_weapon_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[BOLT_IMPACT_GLOW, ROCKET_BLAST_GLOW, ROCKET_CORE_GLOW]);
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponState>()
            .configure_sets(Update, CombatDamage.in_set(GameSet::Combat))
            .add_systems(Startup, (create_pickup_assets, create_bolt_assets, create_rocket_assets, prewarm_weapon_fades))
            .add_systems(Update, spawn_viewmodels)
            .add_systems(Update, (spawn_floor_pickups.run_if(resource_exists_and_changed::<LevelGrid>)).in_set(GameSet::World))
            .add_systems(
                Update,
                (fire_weapons, move_player_projectiles).chain().in_set(CombatDamage).run_if(resource_exists::<LevelGrid>),
            )
            .add_systems(Update, (jammer_deaths, update_pickups).in_set(GameSet::Combat).after(CombatDamage))
            .add_systems(Update, (sync_overlay_fov, animate_viewmodels).in_set(GameSet::Ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corridor() -> LevelGrid {
        LevelGrid::from_ascii(&[
            "##########", //
            "#........#", //
            "#....#...#", //
            "##########",
        ])
    }

    #[test]
    fn viewmodel_bob_is_damped_across_single_frame_landings() {
        let dt = 1.0 / 60.0;
        // Steady bhop: airborne amplitude, then a one-frame touchdown at 30 m/s.
        let mut amp = viewmodel_bob_target(false, 30.0);
        let air = amp;
        amp = damp_bob_amp(amp, viewmodel_bob_target(true, 30.0), dt);
        assert!(amp - air < 0.2, "one-frame landing spiked the bob from {air} to {amp}");
        amp = damp_bob_amp(amp, viewmodel_bob_target(false, 30.0), dt);
        assert!(amp < 0.4);
        // Sustained running still converges on the full amplitude.
        for _ in 0..120 {
            amp = damp_bob_amp(amp, viewmodel_bob_target(true, 30.0), dt);
        }
        assert!((amp - 1.3).abs() < 0.01, "{amp}");
    }

    #[test]
    fn hitscan_hits_enemy_in_open() {
        let g = corridor();
        let imp = Enemy::new(EnemyKind::Imp, 1);
        let origin = Vec3::new(5.0, 1.15, 6.0);
        let root = Vec3::new(20.0, 0.0, 6.0);
        let (d, t) = resolve_hitscan(&g, origin, Vec3::X, 60.0, &[(root, &imp)], &[]);
        assert_eq!(t, HitTarget::Enemy(0));
        assert!(d > 14.0 && d < 15.0, "{d}");
    }

    #[test]
    fn walls_block_hitscan() {
        let g = corridor();
        let imp = Enemy::new(EnemyKind::Imp, 1);
        // Row z=2 has a wall at x=5 (world 20..24).
        let origin = Vec3::new(6.0, 1.15, 10.0);
        let root = Vec3::new(30.0, 0.0, 10.0);
        let (d, t) = resolve_hitscan(&g, origin, Vec3::X, 60.0, &[(root, &imp)], &[root + Vec3::X]);
        assert_eq!(t, HitTarget::Wall);
        assert!((d - 14.0).abs() < 1e-3);
    }

    #[test]
    fn nearest_target_wins_and_jammers_take_damage() {
        let g = corridor();
        let imp = Enemy::new(EnemyKind::Imp, 1);
        let origin = Vec3::new(5.0, 2.7, 6.0);
        let jam = Vec3::new(10.0, 0.0, 6.0);
        let (_, t) = resolve_hitscan(&g, origin, Vec3::X, 60.0, &[(Vec3::new(30.0, 0.0, 6.0), &imp)], &[jam]);
        assert_eq!(t, HitTarget::Jammer(0));
        let mut j = DemonJammer::for_floor(1);
        let hp = j.hp;
        assert!(!damage_jammer(&mut j, 30.0));
        assert_eq!(j.hp, hp - 30.0);
        assert!(j.hit_flash > 0.0);
        assert!(damage_jammer(&mut j, 1000.0));
        assert!(!j.is_active());
        assert!(!damage_jammer(&mut j, 10.0));
    }

    #[test]
    fn overclock_and_fire_rate_mods() {
        let base = PlayerModifiers::default();
        assert!((fire_cooldown(Weapon::Shotgun, &base, 5) - SHOTGUN_COOLDOWN).abs() < 1e-6);
        let oc = PlayerModifiers { overclock_bonus: 0.35, ..default() };
        assert!((fire_cooldown(Weapon::Plasma, &oc, 4) - PLASMA_COOLDOWN / 1.35).abs() < 1e-6);
        assert!((fire_cooldown(Weapon::Plasma, &oc, 3) - PLASMA_COOLDOWN).abs() < 1e-6);
        let cell = PlayerModifiers { fire_rate_mult: 1.15, ..default() };
        assert!(fire_cooldown(Weapon::Shotgun, &cell, 0) < SHOTGUN_COOLDOWN);
    }

    #[test]
    fn pickups_scale_and_cap() {
        let mut p = Player { hp: 50.0, ammo_shells: 0, ..default() };
        let m = PlayerModifiers { pickup_mult: 1.3, ..default() };
        assert!((apply_pickup(PickupKind::Health, &mut p, &m) - 32.5).abs() < 1e-4);
        assert_eq!(apply_pickup(PickupKind::Shells, &mut p, &m), 10.0);
        p.hp = p.max_hp;
        assert_eq!(apply_pickup(PickupKind::Health, &mut p, &m), 0.0);
        p.ammo_plasma = p.max_plasma - 5;
        assert_eq!(apply_pickup(PickupKind::Plasma, &mut p, &m), 5.0);
    }

    #[test]
    fn weapon_swap_animates_and_switches() {
        let mut ws = WeaponState::default();
        assert!(!ws.switch_to(Weapon::Shotgun));
        assert!(ws.switch_to(Weapon::Plasma));
        assert_eq!(ws.displayed(), Weapon::Shotgun);
        ws.swap_timer = SWAP_SECS * 0.2;
        assert_eq!(ws.displayed(), Weapon::Plasma);
    }

    #[test]
    fn three_weapon_deck_cycles_both_ways() {
        assert_eq!(Weapon::ALL, [Weapon::Shotgun, Weapon::Plasma, Weapon::Rocket]);
        assert_eq!(Weapon::Shotgun.next(), Weapon::Plasma);
        assert_eq!(Weapon::Plasma.next(), Weapon::Rocket);
        assert_eq!(Weapon::Rocket.next(), Weapon::Shotgun);
        assert_eq!(Weapon::Shotgun.prev(), Weapon::Rocket);
        assert_eq!(Weapon::Rocket.prev(), Weapon::Plasma);
        for w in Weapon::ALL {
            assert_eq!(w.next().prev(), w);
            assert_eq!(w.next().next().next(), w);
        }
        assert!((fire_cooldown(Weapon::Rocket, &PlayerModifiers::default(), 0) - ROCKET_COOLDOWN).abs() < 1e-6);
    }

    #[test]
    fn swap_shows_the_actual_outgoing_weapon() {
        let mut ws = WeaponState::default();
        assert!(ws.switch_to(Weapon::Rocket));
        assert_eq!(ws.previous, Weapon::Shotgun);
        assert_eq!(ws.displayed(), Weapon::Shotgun, "lowering the shotgun, not `other()`");
        ws.swap_timer = SWAP_SECS * 0.3;
        assert_eq!(ws.displayed(), Weapon::Rocket);
        ws.swap_timer = 0.0;
        assert!(ws.switch_to(Weapon::Plasma));
        assert_eq!(ws.displayed(), Weapon::Rocket);
        // Re-target mid-lowering: keep lowering what is on screen.
        assert!(ws.switch_to(Weapon::Shotgun));
        assert_eq!(ws.previous, Weapon::Rocket);
        assert_eq!(ws.displayed(), Weapon::Rocket);
    }

    #[test]
    fn auto_swap_scans_the_deck_for_ammo() {
        let p = Player { ammo_shells: 0, ammo_plasma: 0, ammo_rockets: 3, ..default() };
        assert_eq!(Weapon::Shotgun.auto_swap_target(&p), Some(Weapon::Rocket), "skips the empty plasma");
        let p = Player { ammo_shells: 4, ammo_plasma: 0, ammo_rockets: 0, ..default() };
        assert_eq!(Weapon::Plasma.auto_swap_target(&p), Some(Weapon::Shotgun));
        assert_eq!(Weapon::Rocket.auto_swap_target(&p), Some(Weapon::Shotgun));
        let p = Player { ammo_shells: 0, ammo_plasma: 0, ammo_rockets: 0, ..default() };
        assert_eq!(Weapon::Shotgun.auto_swap_target(&p), None);
        let p = Player { ammo_shells: 1, ammo_plasma: 1, ammo_rockets: 0, ..default() };
        assert_eq!(Weapon::Rocket.auto_swap_target(&p), Some(Weapon::Shotgun), "next() first");
    }

    #[test]
    fn doppler_overdrive_scales_above_twenty_mps_only() {
        let none = PlayerModifiers::default();
        assert!(!doppler_active(40.0, &none));
        assert_eq!(doppler_damage_mult(40.0, &none), 1.0);
        assert_eq!(doppler_spread_mult(40.0, &none), 1.0);
        let m = PlayerModifiers { doppler_damage_bonus: 0.30, doppler_spread_mult: 0.80, ..default() };
        assert!(!doppler_active(19.9, &m));
        assert_eq!(doppler_damage_mult(19.9, &m), 1.0);
        assert_eq!(doppler_spread_mult(19.9, &m), 1.0);
        assert!(doppler_active(DOPPLER_SPEED_THRESHOLD, &m));
        assert!((doppler_damage_mult(25.0, &m) - 1.30).abs() < 1e-6);
        assert!((doppler_spread_mult(25.0, &m) - 0.80).abs() < 1e-6);
        // Captured into projectiles at fire time.
        let bolt = PlayerProjectile::plasma(Vec3::X, doppler_damage_mult(25.0, &m));
        assert!((bolt.damage * bolt.damage_mult - PLASMA_DAMAGE * 1.3).abs() < 1e-4);
        let rocket = PlayerProjectile::rocket(Vec3::X, doppler_damage_mult(25.0, &m));
        assert_eq!(rocket.kind, ProjectileKind::Rocket);
        assert!((rocket.damage * rocket.damage_mult - ROCKET_DIRECT_DAMAGE * 1.3).abs() < 1e-3);
    }

    #[test]
    fn rocket_splash_falls_off_linearly() {
        assert_eq!(splash_falloff(0.0, 5.5), 1.0);
        assert!((splash_falloff(2.75, 5.5) - 0.5).abs() < 1e-6);
        assert_eq!(splash_falloff(5.5, 5.5), 0.0);
        assert_eq!(splash_falloff(9.0, 5.5), 0.0);
        assert!((rocket_splash_damage(0.0, 5.5, 1.0) - ROCKET_SPLASH_DAMAGE).abs() < 1e-6);
        assert!((rocket_splash_damage(2.75, 5.5, 1.3) - ROCKET_SPLASH_DAMAGE * 0.5 * 1.3).abs() < 1e-4);
        assert_eq!(rocket_splash_damage(6.0, 5.5, 1.0), 0.0);
        // Direct hits out-damage the best possible splash (no double-dip needed).
        const { assert!(ROCKET_DIRECT_DAMAGE > ROCKET_SPLASH_DAMAGE) };
        // Distance is measured to the nearest hit-sphere surface.
        let imp = Enemy::new(EnemyKind::Imp, 1);
        let root = Vec3::new(10.0, 0.0, 0.0);
        let d = splash_distance(root + Vec3::new(-3.0, 1.0, 0.0), root, imp.stats.hit_spheres);
        assert!(d < 3.0 && d > 1.5, "{d}");
        assert_eq!(splash_distance(Vec3::new(0.0, 1.3, 0.0), Vec3::ZERO, &JAMMER_SPHERES), 0.0);
        let r = rocket_splash_radius(&PlayerModifiers { rocket_splash_mult: 1.2, ..default() });
        assert!((r - ROCKET_SPLASH_RADIUS * 1.2).abs() < 1e-6);
    }

    #[test]
    fn rocket_jump_launches_with_forgiving_self_damage() {
        let m = PlayerModifiers::default();
        let feet = Vec3::new(10.0, 0.0, 10.0);
        // Straight down at your feet: pure vertical launch.
        let j = compute_rocket_jump(feet, feet, ROCKET_SPLASH_RADIUS, &m).expect("in range");
        let f = 1.0 - 0.9 / ROCKET_SPLASH_RADIUS;
        assert!((j.falloff - f).abs() < 1e-5);
        assert!((j.impulse.y - ROCKET_JUMP_UP * f).abs() < 1e-4);
        assert!(Vec2::new(j.impulse.x, j.impulse.z).length() < 1e-4);
        assert!((j.self_damage - ROCKET_SPLASH_DAMAGE * f * ROCKET_SELF_DAMAGE_FRAC).abs() < 1e-4);
        assert!(j.self_damage <= ROCKET_SPLASH_DAMAGE * ROCKET_SELF_DAMAGE_FRAC + 1e-4, "max ~14.25");
        // Steep near-foot shot (e.g. pitch -1.5 rad, blast 0.12 m ahead): mostly vertical, no giant sideways kick.
        let j_steep = compute_rocket_jump(feet, feet + Vec3::new(0.12, 0.0, 0.0), ROCKET_SPLASH_RADIUS, &m).unwrap();
        assert!(j_steep.impulse.y > 10.0 && j_steep.impulse.x.abs() < 2.5, "{:?}", j_steep.impulse);
        // Blast behind (-X) pushes forward (+X) and up.
        let j = compute_rocket_jump(feet, feet + Vec3::new(-1.5, 0.0, 0.0), ROCKET_SPLASH_RADIUS, &m).unwrap();
        assert!(j.impulse.x > 5.0 && j.impulse.y > 5.0 && j.impulse.z.abs() < 1e-4, "{:?}", j.impulse);
        // Rocket velocity inherits forward speed so 48 m/s bhoppers never overtake their own rockets.
        let fast_v = rocket_velocity(Vec3::X, Vec3::new(AIR_SPEED_HARD_CAP, 0.0, 0.0));
        assert!(fast_v.x > AIR_SPEED_HARD_CAP + 5.0, "{fast_v:?}");
        let back_v = rocket_velocity(Vec3::NEG_X, Vec3::new(AIR_SPEED_HARD_CAP, 0.0, 0.0));
        assert!((back_v.length() - ROCKET_SPEED).abs() < 1e-4, "backward shots never slow down: {back_v:?}");
        // Out of range: nothing.
        assert!(compute_rocket_jump(feet, feet + Vec3::new(8.0, 0.0, 0.0), ROCKET_SPLASH_RADIUS, &m).is_none());

        // Applying: armor takes half, ground detached.
        let mut p = Player { armor: 50.0, hp: 100.0, grounded: true, coyote_timer: 0.1, ..default() };
        let j = compute_rocket_jump(feet, feet, ROCKET_SPLASH_RADIUS, &m).unwrap();
        let lost = apply_rocket_jump(&mut p, &j);
        assert!((lost - j.self_damage * 0.5).abs() < 1e-4);
        assert!(!p.grounded && p.coyote_timer == 0.0);
        assert!(p.velocity.y > 10.0);
        // Invulnerable: launched, unhurt.
        let mut p = Player { invuln_timer: 1.0, grounded: true, ..default() };
        assert_eq!(apply_rocket_jump(&mut p, &j), 0.0);
        assert_eq!(p.hp, p.max_hp);
        assert!(!p.grounded && p.velocity.y > 10.0);
    }

    #[test]
    fn rocket_jump_keeps_momentum_and_clamps_only_horizontal() {
        let m = PlayerModifiers::default();
        let feet = Vec3::ZERO;
        let j = compute_rocket_jump(feet, Vec3::new(-1.0, 0.0, 0.0), ROCKET_SPLASH_RADIUS, &m).unwrap();
        // Bhopping at 30 m/s: the push adds on top of the carried speed.
        let mut p = Player { velocity: Vec3::new(30.0, 0.0, 0.0), ..default() };
        apply_rocket_jump(&mut p, &j);
        assert!(p.velocity.x > 30.0 + 5.0, "{:?}", p.velocity);
        // Already at the hard cap: horizontal clamps, vertical stays whole.
        let mut p = Player { velocity: Vec3::new(AIR_SPEED_HARD_CAP, 0.0, 0.0), ..default() };
        apply_rocket_jump(&mut p, &j);
        assert!((Vec2::new(p.velocity.x, p.velocity.z).length() - AIR_SPEED_HARD_CAP).abs() < 1e-3);
        assert!((p.velocity.y - j.impulse.y).abs() < 1e-4);
        // The launch then flies through the Milestone 8 air model (no ground friction).
        let h = crate::player::step_horizontal(
            Vec2::new(p.velocity.x, p.velocity.z),
            Vec2::ZERO,
            crate::player::MoveInput::default(),
            false,
            crate::player::MOVE_SPEED,
            1.0 / 120.0,
        );
        assert!(h.length() > AIR_SPEED_HARD_CAP - 1.0, "air bleed is smooth: {}", h.length());
    }

    #[test]
    fn blast_thrusters_scale_the_rocket_jump() {
        let base = PlayerModifiers::default();
        let bt = PlayerModifiers { rocket_jump_mult: 1.25, rocket_splash_mult: 1.2, rocket_self_damage_mult: 0.5, ..default() };
        let feet = Vec3::ZERO;
        let blast = Vec3::new(-1.0, 0.0, 0.0);
        let a = compute_rocket_jump(feet, blast, rocket_splash_radius(&base), &base).unwrap();
        let b = compute_rocket_jump(feet, blast, rocket_splash_radius(&bt), &bt).unwrap();
        assert!(b.impulse.y > a.impulse.y * 1.25 - 1e-4, "bigger boost (and wider radius)");
        assert!(b.self_damage < a.self_damage * 0.6);
        // Wider radius reaches further.
        let far = Vec3::new(-5.8, 0.9, 0.0);
        assert!(compute_rocket_jump(feet, far, rocket_splash_radius(&base), &base).is_none());
        assert!(compute_rocket_jump(feet, far, rocket_splash_radius(&bt), &bt).is_some());
    }

    #[test]
    fn rockets_never_spawn_behind_geometry() {
        let g = corridor();
        // Open corridor, aiming far: starts at the muzzle, converges on the aim.
        let origin = Vec3::new(8.0, 1.6, 6.0);
        let fwd = Vec3::X;
        let muzzle = origin + Vec3::new(0.85, -0.17, 0.2);
        let (start, dir) = rocket_launch(&g, origin, fwd, muzzle, 20.0);
        assert_eq!(start, muzzle);
        assert!(dir.dot(fwd) > 0.98);
        // Straight down at your feet: starts on the eye ray, above the floor.
        let down = Vec3::NEG_Y;
        let (start, dir) = rocket_launch(&g, origin, down, origin + Vec3::new(0.2, -0.85, 0.17), 1.6);
        assert_eq!(dir, down);
        assert!(start.y > 1.0 && (start.x - origin.x).abs() < 1e-5, "{start:?}");
        assert!(raycast_grid(&g, start, dir, 3.0).is_some_and(|h| h.distance > 1.0), "floor still ahead");
        // Point-blank wall (x=4..? wall at world x<4): muzzle pokes through it.
        let origin = Vec3::new(4.4, 1.6, 6.0);
        let muzzle = origin + Vec3::new(-0.85, -0.17, 0.2);
        let (start, dir) = rocket_launch(&g, origin, Vec3::NEG_X, muzzle, 0.4);
        assert!(start.x > 4.0, "start stays in the open: {start:?}");
        assert_eq!(dir, Vec3::NEG_X);
    }

    #[test]
    fn rocket_pickups_scale_cap_and_join_the_floor_loot() {
        let m = PlayerModifiers { pickup_mult: 1.3, ..default() };
        let mut p = Player { ammo_rockets: 0, ..default() };
        assert_eq!(PickupKind::Rockets.label(), "ROCKETS");
        assert_eq!(PickupKind::ALL.len(), 5);
        assert_eq!(apply_pickup(PickupKind::Rockets, &mut p, &PlayerModifiers::default()), 4.0);
        assert_eq!(apply_pickup(PickupKind::Rockets, &mut p, &m), 5.0);
        p.ammo_rockets = p.max_rockets - 1;
        assert_eq!(apply_pickup(PickupKind::Rockets, &mut p, &m), 1.0);
        assert_eq!(apply_pickup(PickupKind::Rockets, &mut p, &m), 0.0, "full rack leaves it on the floor");
        assert_eq!(p.ammo_rockets, p.max_rockets);
        assert_ne!(pickup_light(PickupKind::Rockets), pickup_light(PickupKind::Plasma));
        let lead = |f: u32| (1..40u64).find_map(|sd| floor_pickup_plan(&crate::level::generate_level(f, sd)).first().map(|(k, _)| *k));
        assert_eq!(lead(1), Some(PickupKind::Shells));
        assert_eq!(lead(2), Some(PickupKind::Rockets), "even floors lead with a rocket crate");
    }

    #[test]
    fn floor_pickups_placed_on_floor_cells() {
        let g = crate::level::generate_level(2, 11);
        let plan = floor_pickup_plan(&g);
        assert!(!plan.is_empty());
        for (_, c) in plan {
            assert!(matches!(g.cell(c), Cell::Floor { .. }));
        }
    }
}
