//! Right hand: Mars Pump Shotgun (hitscan pellets), UAC Plasma Repeater (3D
//! bolts) and the Signal-Mortar rocket launcher (splash + Quake rocket jumps).
//! Left hand: tactical smartphone viewmodel with a live signal screen.
//! Also destructible Demon Jammers and pickups.
//!
//! Viewmodels live on [`OVERLAY_LAYER`], drawn by a second camera (child of the
//! `PlayerCamera`) that clears depth, so they never clip into walls.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

use bevy::camera::visibility::RenderLayers;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use rand::Rng;

use crate::apps::{spawn_hijack_hotspot, PhoneApps};
use crate::assets_gen::{engraved_gold_texture, gunmetal_plate_texture, hazard_stripe_texture, obsidian_tech_texture, wood_grain_texture};
use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::BotControl;
use crate::decals::{DecalKind, DecalQueue, DecalRequest};
use crate::boss::{damage_pylon, pylon_ray_hit, BossPylon, PYLON_SPHERES};
use crate::enemies::{damage_enemy, ray_sphere, Enemy, EnemyKind, PingMarked};
use crate::fx::{
    additive_glow, overlay_layer, spawn_burst, spawn_ejected_shell, spawn_flash_light, spawn_mortar_casing, spawn_particles, spawn_plasma_vent,
    spawn_shockwave, spawn_tracer, BurstColor, FxAssets, HudFeed, OVERLAY_LAYER,
};
use crate::hud::signal_color;
use crate::level::{Cell, CullableLight, DemonJammer, ExplosiveBarrel, JammerCore, LevelEntity, LevelGrid};
use crate::mesh_batch::{MaterialBatches, ParentedBatches};
use crate::meta::{HandsetSkin, RunLoadout, SkinSlot};
use crate::phone_screen::PhoneScreenTexture;
use crate::physics::{has_line_of_sight, raycast_grid};
use crate::sculpt;
use crate::player::{live_eye_pose, smooth_factor, CursorCaptured, Player, PlayerCamera, AIR_SPEED_HARD_CAP};
use crate::signal::{overclock_factor, SignalState};
use crate::state::{AppState, GameSet, PlayerModifiers, RunStats};

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

// --- Explosive server-rack barrels ---------------------------------------------
/// Soft hit cylinder of a barrel (non-blocking for movement).
pub const BARREL_RADIUS: f32 = 0.55;
pub const BARREL_HEIGHT: f32 = 1.25;
/// Height of a barrel's blast / LoS center above its base.
pub const BARREL_CENTER_Y: f32 = 0.6;
pub const BARREL_BLAST_RADIUS: f32 = 6.0;
pub const BARREL_DAMAGE: f32 = 120.0;
/// Staggered fuse of a barrel set off by another barrel's blast.
pub const BARREL_CHAIN_FUSE: f32 = 0.12;
/// Forgiving share of a barrel blast the player takes.
pub const BARREL_SELF_DAMAGE_FRAC: f32 = 0.15;
/// Barrel blast jumps launch a bit harder than rocket jumps.
pub const BARREL_JUMP_MULT: f32 = 1.15;

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
    /// Full cooldown armed by the last shot (after fire-rate / 5G Overclock
    /// modifiers), so reload-cycle animations normalise against the real cycle.
    /// Zeroed when a swap (not a shot) arms the cooldown.
    pub last_cooldown: f32,
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
    /// Remaining `F` inspect flourish (purely visual; 0 = off).
    pub inspect_timer: f32,
    /// Cant direction of the current swap (+1 toward `next()`, -1 toward `prev()`).
    pub swap_dir: f32,
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
        self.swap_dir = if w == self.current.next() { 1.0 } else { -1.0 };
        self.inspect_timer = 0.0;
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
        let swap_cd = SWAP_SECS * 0.6;
        if swap_cd > self.cooldown {
            // The swap, not a shot, now owns the cooldown: no reload cycle to animate.
            self.cooldown = swap_cd;
            self.last_cooldown = 0.0;
        }
        true
    }

    /// 0..1 progress through the current shot's reload cycle (1.0 = ready, or no
    /// shot cycle in flight), normalised by the cooldown that shot actually armed.
    pub fn cycle_progress(&self) -> f32 {
        if self.cooldown <= 0.0 || self.last_cooldown <= 0.0 {
            return 1.0;
        }
        1.0 - (self.cooldown / self.last_cooldown.max(0.05)).clamp(0.0, 1.0)
    }
}

/// What a hitscan ray struck first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitTarget {
    Nothing,
    Wall,
    Enemy(usize),
    Jammer(usize),
    Barrel(usize),
    /// Carrier Daemon Shield Pylon (index into the pylon list).
    Pylon(usize),
}

/// Jammer hit volumes relative to its base.
pub const JAMMER_SPHERES: [(f32, f32); 3] = [(0.5, 0.5), (1.3, 0.5), (2.7, 0.8)];

pub fn jammer_ray_hit(base: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    JAMMER_SPHERES
        .iter()
        .filter_map(|&(h, r)| ray_sphere(origin, dir, base + Vec3::Y * h, r))
        .reduce(f32::min)
}

/// Distance along `dir` to a barrel's hit cylinder standing on `base`.
pub fn barrel_ray_hit(base: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    let dir = dir.normalize_or_zero();
    let o = origin - base;
    let r2 = BARREL_RADIUS * BARREL_RADIUS;
    let mut best = f32::INFINITY;
    // Curved side.
    let a = dir.x * dir.x + dir.z * dir.z;
    if a > 1e-8 {
        let b = o.x * dir.x + o.z * dir.z;
        let c = o.x * o.x + o.z * o.z - r2;
        let disc = b * b - a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            for t in [(-b - sq) / a, (-b + sq) / a] {
                let y = o.y + dir.y * t;
                if t >= 0.0 && (0.0..=BARREL_HEIGHT).contains(&y) {
                    best = best.min(t);
                }
            }
        }
    }
    // Caps.
    if dir.y.abs() > 1e-6 {
        for cap in [0.0, BARREL_HEIGHT] {
            let t = (cap - o.y) / dir.y;
            let p = o + dir * t;
            if t >= 0.0 && p.x * p.x + p.z * p.z <= r2 {
                best = best.min(t);
            }
        }
    }
    best.is_finite().then_some(best)
}

/// Damage a barrel. Lethal damage arms its fuse: instant for weapons, a
/// short staggered [`BARREL_CHAIN_FUSE`] for another barrel's blast. Returns
/// true if this armed it.
pub fn damage_barrel(b: &mut ExplosiveBarrel, amount: f32, chain: bool) -> bool {
    if !b.is_intact() || amount <= 0.0 {
        return false;
    }
    b.hp -= amount;
    b.hit_flash = 0.15;
    if b.hp <= 0.0 {
        b.fuse = Some(if chain { BARREL_CHAIN_FUSE } else { 0.0 });
        return true;
    }
    false
}

/// Barrel blast damage on a target whose nearest hit surface is `dist` away.
pub fn barrel_blast_damage(dist: f32) -> f32 {
    BARREL_DAMAGE * splash_falloff(dist, BARREL_BLAST_RADIUS)
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

/// [`resolve_hitscan_targets`] plus explosive barrels (bases in `barrels`).
pub fn resolve_hitscan_full(
    grid: &LevelGrid,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    enemies: &[HitTargetInfo],
    jammers: &[Vec3],
    barrels: &[Vec3],
) -> (f32, HitTarget) {
    let mut best = resolve_hitscan_targets(grid, origin, dir, max_dist, enemies, jammers);
    for (i, base) in barrels.iter().enumerate() {
        if let Some(t) = barrel_ray_hit(*base, origin, dir)
            && t < best.0
        {
            best = (t, HitTarget::Barrel(i));
        }
    }
    best
}

/// [`resolve_hitscan_full`] plus Carrier Daemon Shield Pylons (bases in `pylons`).
#[allow(clippy::too_many_arguments)]
pub fn resolve_hitscan_all(
    grid: &LevelGrid,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    enemies: &[HitTargetInfo],
    jammers: &[Vec3],
    barrels: &[Vec3],
    pylons: &[Vec3],
) -> (f32, HitTarget) {
    let mut best = resolve_hitscan_full(grid, origin, dir, max_dist, enemies, jammers, barrels);
    let d = dir.normalize_or_zero();
    for (i, base) in pylons.iter().enumerate() {
        if let Some(t) = pylon_ray_hit(*base, origin, d)
            && t < best.0
        {
            best = (t, HitTarget::Pylon(i));
        }
    }
    best
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
        EnemyKind::PhaseStalker => 0.9,
        EnemyKind::RelaySniper => 0.6,
        EnemyKind::CarrierDaemon => 0.05,
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
    let self_damage = ROCKET_SPLASH_DAMAGE * ROCKET_SELF_DAMAGE_FRAC * mods.rocket_self_damage_mult;
    compute_blast_jump(player_pos, blast, splash_radius, self_damage, mods.rocket_jump_mult)
}

/// Quake blast boost from an exploding barrel: forgiving ~15% self-damage
/// and a strong launch impulse (`None` = out of range).
pub fn compute_barrel_jump(player_pos: Vec3, blast: Vec3, mods: &PlayerModifiers) -> Option<RocketJump> {
    let self_damage = BARREL_DAMAGE * BARREL_SELF_DAMAGE_FRAC * mods.rocket_self_damage_mult;
    compute_blast_jump(player_pos, blast, BARREL_BLAST_RADIUS, self_damage, mods.rocket_jump_mult * BARREL_JUMP_MULT)
}

/// Shared blast-jump math: linear falloff of `self_damage_at_center` and of
/// the [`ROCKET_JUMP_UP`] / [`ROCKET_JUMP_PUSH`] impulse scaled by `jump_mult`.
pub fn compute_blast_jump(player_pos: Vec3, blast: Vec3, splash_radius: f32, self_damage_at_center: f32, jump_mult: f32) -> Option<RocketJump> {
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
    let k = falloff * jump_mult;
    let impulse = Vec3::new(horiz.x * ROCKET_JUMP_PUSH * k, ROCKET_JUMP_UP * k, horiz.y * ROCKET_JUMP_PUSH * k);
    let self_damage = self_damage_at_center * falloff;
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
/// Shotgun bolt carrier over the right-side ejection port (slides back on the pump).
#[derive(Component)]
pub struct ShotgunBolt;
/// Plasma Repeater top flux slide (snaps back on every shot, exposing the capacitor).
#[derive(Component)]
pub struct PlasmaFluxSlide;
/// Signal-Mortar open breech cradle + warhead (kicks back on fire, vents on reload).
#[derive(Component)]
pub struct RocketBreech;
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
    /// Signal-Mortar warhead / ammo window / heat-vent glow (flares on fire, recharges).
    pub warhead: Handle<StandardMaterial>,
}

/// A phone chassis part styled by the run's [`HandsetSkin`] palette slot.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneSkinPart(pub SkinSlot);

/// The handset skin the current phone viewmodel was built with.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneSkin(pub HandsetSkin);

/// Gun rest pose: nudged left/up so the sculpted receiver, walnut wrist and rear
/// breech clear the bottom-right ammo panel while staying wide of the crosshair.
pub const WEAPON_REST: Vec3 = Vec3::new(0.215, -0.175, -0.39);
pub const PHONE_REST: Vec3 = Vec3::new(-0.265, -0.135, -0.37);
/// Rest pose of the animated mechanical pivots (local to their gun model).
pub const SHOTGUN_BOLT_REST: Vec3 = Vec3::new(0.0445, 0.014, -0.047);
pub const PLASMA_SLIDE_REST: Vec3 = Vec3::new(0.0, 0.081, -0.15);
pub const ROCKET_BREECH_REST: Vec3 = Vec3::new(0.0, 0.068, -0.03);
/// Idle emissive of the plasma coils / conduit / canister: vivid #00C8FF electric cyan,
/// never white.
const PLASMA_GLOW: LinearRgba = LinearRgba::rgb(0.02, 0.85, 1.75);
/// Plasma emissive at the peak of a shot (blends back to [`PLASMA_GLOW`]).
const PLASMA_FLARE: LinearRgba = LinearRgba::rgb(0.12, 2.2, 4.2);
/// Idle emissive of the Signal-Mortar warhead, ammo windows + heat vents: hot #FF8A1F
/// amber (flares on a shot, dims while venting, recharges by ready).
const ROCKET_GLOW: LinearRgba = LinearRgba::rgb(2.0, 0.62, 0.03);
/// Camera-space depth of the world-space muzzle origin of tracers / bolts / rockets
/// (also the eye->muzzle span `rocket_launch` checks for a blocking wall).
pub const MUZZLE_DEPTH: f32 = 0.85;
/// Shotgun right-side ejection port, local to the gun model.
pub const SHOTGUN_PORT_LOCAL: Vec3 = Vec3::new(0.036, 0.014, -0.045);
/// Plasma Repeater cooling vent (viewmodel local, relative to `WEAPON_REST`).
pub const PLASMA_VENT_LOCAL: Vec3 = Vec3::new(0.0, 0.09, -0.12);
/// Signal-Mortar breech ejection port (viewmodel local, relative to `WEAPON_REST`).
pub const MORTAR_PORT_LOCAL: Vec3 = Vec3::new(0.05, 0.07, -0.03);

/// Barrel tip of each gun viewmodel, local to its [`GunModel`] (where its flash sits).
pub fn viewmodel_muzzle_local(w: Weapon) -> Vec3 {
    match w {
        Weapon::Shotgun => Vec3::new(0.0, 0.030, -0.598),
        Weapon::Plasma => Vec3::new(0.0, 0.0, -0.585),
        Weapon::Rocket => Vec3::new(0.0, 0.012, -0.565),
    }
}

/// Camera-space muzzle origin of world tracers / bolts / rockets: the gun's barrel tip
/// slid along its own view ray to [`MUZZLE_DEPTH`]. The viewmodel overlay camera shares
/// the eye's pose and FOV, so shots leave exactly where the barrel is drawn on screen.
pub fn muzzle_offset(w: Weapon) -> Vec3 {
    let tip = WEAPON_REST + viewmodel_muzzle_local(w);
    tip * (MUZZLE_DEPTH / -tip.z)
}

/// Multiply a mesh's UVs (per-part texture tiling while sharing one material).
fn uv_scaled(mut m: Mesh, s: Vec2) -> Mesh {
    if let Some(VertexAttributeValues::Float32x2(uvs)) = m.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        for uv in uvs.iter_mut() {
            uv[0] *= s.x;
            uv[1] *= s.y;
        }
    }
    m
}

/// Flat `size` decal quad facing +Z with UVs scaled by `uv`.
fn decal(size: Vec2, uv: Vec2) -> Mesh {
    uv_scaled(Mesh::from(Rectangle::new(size.x, size.y)), uv)
}

fn vm_part(parent: Entity, mesh: &Handle<Mesh>, mat: &Handle<StandardMaterial>, t: Vec3, s: Vec3, r: Quat) -> impl Bundle {
    (ChildOf(parent), Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::from_translation(t).with_scale(s).with_rotation(r), overlay_layer())
}

/// Viewmodel cylinder: 12 sides is plenty at viewmodel scale (Bevy's default is 32).
fn vm_cylinder() -> Mesh {
    Cylinder::new(1.0, 1.0).mesh().resolution(12).build()
}

/// Thin faceted ring spanning radius 0.78..1.0 in the XZ plane (192 tris; a default
/// Bevy `Torus` of the same footprint costs 1,536).
fn vm_thin_ring() -> Mesh {
    sculpt::armored_ring(0.89, 0.11, 0.11, 12, true)
}

/// Tiny sight beads / LEDs: a few millimetres on screen, 8x6 is visually round.
fn vm_bead() -> Mesh {
    Sphere::new(1.0).mesh().uv(8, 6)
}

/// Source primitives of the viewmodel batches (same shapes as the handles).
/// Tessellation is tuned for viewmodel scale: a default `Capsule3d` is 1,024 tris and a
/// default `Torus` 1,536, which alone pushed each gun past 9k triangles.
struct VmSourceMeshes {
    cube: Mesh,
    cyl: Mesh,
    cap: Mesh,
    thin_ring: Mesh,
    sphere: Mesh,
    bead: Mesh,
    cone: Mesh,
}

impl VmSourceMeshes {
    fn new() -> Self {
        VmSourceMeshes {
            cube: Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
            cyl: vm_cylinder(),
            cap: Capsule3d::new(0.5, 1.0).mesh().latitudes(6).longitudes(12).build(),
            thin_ring: vm_thin_ring(),
            sphere: Sphere::new(1.0).mesh().uv(10, 8),
            bead: vm_bead(),
            cone: Cone::new(1.0, 1.0).mesh().resolution(12).build(),
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
    mut images: ResMut<Assets<Image>>,
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
    let kit = vm_kit(&mut meshes, &mut materials, &mut images);
    let VmKit { cube, cyl, thin_ring, bead, steel, brushed, dark_steel, grip, glove, glove_plate, coil, warhead_glow, .. } = kit.clone();
    let screen_quad = meshes.add(Rectangle::new(0.118, 0.177));
    // Rigid untagged parts are baked per (pivot, material) into a few meshes;
    // tagged / animated parts (skin slots, LEDs, coils, screen) stay separate.
    let src = VmSourceMeshes::new();
    let mut vb = ParentedBatches::new();
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
    // Status LED / antenna beacon: LIT (unlit ignores emissive), dark hued base +
    // saturated emissive, both rewritten every frame by `animate_viewmodels`.
    let (led_base, led_emissive) = phone_led_glow(5, false, false, 0.0);
    let led = materials.add(StandardMaterial { base_color: led_base, emissive: led_emissive, perceptual_roughness: 0.7, metallic: 0.0, reflectance: 0.08, ..default() });
    let screen = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: phone_tex.as_ref().map(|t| t.image.clone()),
        unlit: true,
        ..default()
    });
    commands.insert_resource(PhoneMaterials { screen: screen.clone(), led: led.clone(), coil: coil.clone(), warhead: warhead_glow.clone() });

    let overlay = commands
        .spawn((
            ChildOf(cam),
            OverlayCamera,
            Camera3d::default(),
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            msaa,
            Projection::Perspective(PerspectiveProjection { fov, near: 0.01, far: 400.0, ..default() }),
            RenderLayers::layer(OVERLAY_LAYER),
            // Per-camera ambient: lifts the shadowed left/top faces of metal so the
            // chamfers read instead of crushing to black (the world keeps its own).
            AmbientLight { color: Color::srgb(1.0, 0.86, 0.8), brightness: 220.0, ..default() },
            Transform::default(),
        ))
        .id();
    // Studio rig on OVERLAY_LAYER: warm key above-left of the gun (lights the faces the
    // camera sees), cool rim from ahead-right for edge specular, low warm fill from below,
    // and a soft key for the phone chassis. The OLED screen is unlit, so it never washes out.
    for (color, intensity, range, pos) in [
        (Color::srgb(1.0, 0.88, 0.76), 13_000.0, 3.5, Vec3::new(0.06, 0.36, -0.18)),
        (Color::srgb(0.66, 0.82, 1.0), 9_500.0, 3.0, Vec3::new(0.52, 0.22, -0.95)),
        (Color::srgb(1.0, 0.72, 0.55), 2_600.0, 2.5, Vec3::new(0.12, -0.42, -0.25)),
        (Color::srgb(0.82, 0.9, 1.0), 5_200.0, 3.2, Vec3::new(-0.35, 0.22, 0.02)),
    ] {
        commands.spawn((
            ChildOf(overlay),
            PointLight { color, intensity, range, shadow_maps_enabled: false, ..default() },
            Transform::from_translation(pos),
            overlay_layer(),
        ));
    }
    commands.spawn((
        ChildOf(overlay),
        MuzzleLight,
        PointLight { color: Color::srgb(1.0, 0.72, 0.35), intensity: 0.0, range: 3.5, shadow_maps_enabled: false, ..default() },
        Transform::from_translation(WEAPON_REST + Vec3::new(0.0, 0.1, -0.48)),
        overlay_layer(),
    ));

    // =========================================================================
    // RIGHT HAND: SCULPTED WEAPON VIEWMODELS
    // =========================================================================
    let wroot = commands.spawn((ChildOf(cam), WeaponViewmodel, Transform::from_translation(WEAPON_REST), Visibility::default())).id();
    spawn_gun_models(&mut commands, wroot, &kit, &src, &mut vb, true);
    let rx = Quat::from_rotation_x(FRAC_PI_2);
    let rz = Quat::from_rotation_z(FRAC_PI_2);
    let id = Quat::IDENTITY;

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
    commands.spawn((PhoneLed, vm_part(proot, &bead, &led, Vec3::new(0.034, 0.097, 0.0115), Vec3::splat(0.0038), id)));
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
    commands.spawn((PhoneLed, vm_part(proot, &bead, &led, Vec3::new(0.048, 0.174, -0.001), Vec3::splat(0.0062), id)));
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

/// Shared viewmodel meshes + materials: built once per player viewmodel (or
/// Armory showcase) and shared by the gun models and the phone chassis.
#[derive(Clone)]
struct VmKit {
    cube: Handle<Mesh>,
    cyl: Handle<Mesh>,
    thin_ring: Handle<Mesh>,
    bead: Handle<Mesh>,
    coil_ring: Handle<Mesh>,
    steel: Handle<StandardMaterial>,
    brushed: Handle<StandardMaterial>,
    dark_steel: Handle<StandardMaterial>,
    heat_blued: Handle<StandardMaterial>,
    shell_red: Handle<StandardMaterial>,
    grip: Handle<StandardMaterial>,
    glove: Handle<StandardMaterial>,
    glove_plate: Handle<StandardMaterial>,
    bolt_silver: Handle<StandardMaterial>,
    gm_steel: Handle<StandardMaterial>,
    gm_titanium: Handle<StandardMaterial>,
    gm_dark: Handle<StandardMaterial>,
    wood: Handle<StandardMaterial>,
    hazard: Handle<StandardMaterial>,
    rk_hazard: Handle<StandardMaterial>,
    sg_top: Handle<StandardMaterial>,
    sg_oxide: Handle<StandardMaterial>,
    pl_slate: Handle<StandardMaterial>,
    pl_titanium: Handle<StandardMaterial>,
    pl_edge: Handle<StandardMaterial>,
    brass: Handle<StandardMaterial>,
    copper: Handle<StandardMaterial>,
    sight_green: Handle<StandardMaterial>,
    sight_red: Handle<StandardMaterial>,
    vent_amber: Handle<StandardMaterial>,
    flash_core: Handle<StandardMaterial>,
    flash_fire: Handle<StandardMaterial>,
    flash_cyan: Handle<StandardMaterial>,
    flash_cyan_halo: Handle<StandardMaterial>,
    flash_amber_halo: Handle<StandardMaterial>,
    coil: Handle<StandardMaterial>,
    plasma_core: Handle<StandardMaterial>,
    warhead_glow: Handle<StandardMaterial>,
    bore_glow: Handle<StandardMaterial>,
}

/// Bake the [`VmKit`] (procedural gun textures included).
fn vm_kit(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>, images: &mut Assets<Image>) -> VmKit {
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let cyl = meshes.add(vm_cylinder());
    let thin_ring = meshes.add(vm_thin_ring());
    let bead = meshes.add(vm_bead());
    // Beveled faceted accelerator ring for the animated plasma coils (same unit
    // footprint as the old 0.68..1.0 torus so the ripple scale is unchanged).
    let coil_ring = meshes.add(sculpt::armored_ring(0.84, 0.16, 0.16, 20, true));

    let mat = |m: &mut Assets<StandardMaterial>, c: Color, rough: f32, metal: f32| {
        m.add(StandardMaterial { base_color: c, perceptual_roughness: rough, metallic: metal, ..default() })
    };
    // Muzzle flashes: lit but matte/non-reflective so the HDR emissive drives them
    // (an unlit material ignores emissive and tonemaps a 1.0 base to flat beige/grey).
    let glow = |m: &mut Assets<StandardMaterial>, c: Color, e: LinearRgba| {
        m.add(StandardMaterial { base_color: c, emissive: e, perceptual_roughness: 1.0, metallic: 0.0, reflectance: 0.0, ..default() })
    };
    // Lit emissive: the (moderate) emissive carries a saturated hue that never clips
    // to white, the base colour still catches the key light, and the emissive stays
    // animatable (unlit materials ignore emissive entirely).
    let lit_glow =
        |m: &mut Assets<StandardMaterial>, c: Color, e: LinearRgba| m.add(StandardMaterial { base_color: c, emissive: e, perceptual_roughness: 0.6, metallic: 0.0, reflectance: 0.2, ..default() });
    // Retro pixel-art surfaces (neutral-bright, tinted by base_colour).
    let gunmetal_img = images.add(gunmetal_plate_texture().into_image());
    let wood_img = images.add(wood_grain_texture().into_image());
    let hazard_img = images.add(hazard_stripe_texture().into_image());
    let obsidian_img = images.add(obsidian_tech_texture().into_image());
    let gold_img = images.add(engraved_gold_texture().into_image());
    let tex = |m: &mut Assets<StandardMaterial>, img: &Handle<Image>, c: Color, rough: f32, metal: f32| {
        m.add(StandardMaterial { base_color: c, base_color_texture: Some(img.clone()), perceptual_roughness: rough, metallic: metal, ..default() })
    };

    // Rich metallic / composite / tactical palette.
    let steel = mat(materials, Color::srgb(0.46, 0.49, 0.54), 0.28, 0.9);
    let brushed = mat(materials, Color::srgb(0.32, 0.35, 0.39), 0.38, 0.85);
    let dark_steel = mat(materials, Color::srgb(0.12, 0.13, 0.15), 0.44, 0.78);
    // Heat-blued muzzle hardware (cool violet-blue temper).
    let heat_blued = mat(materials, Color::srgb(0.20, 0.22, 0.36), 0.3, 0.88);
    let shell_red = mat(materials, Color::srgb(0.72, 0.08, 0.05), 0.45, 0.1);
    let grip = mat(materials, Color::srgb(0.055, 0.055, 0.06), 0.92, 0.05);
    let glove = mat(materials, Color::srgb(0.17, 0.20, 0.14), 0.88, 0.05);
    let glove_plate = mat(materials, Color::srgb(0.26, 0.29, 0.22), 0.55, 0.45);
    let bolt_silver = mat(materials, Color::srgb(0.68, 0.72, 0.76), 0.25, 0.95);
    // Textured gun surfaces.
    let gm_steel = tex(materials, &gunmetal_img, Color::srgb(0.62, 0.65, 0.70), 0.3, 0.82);
    let gm_titanium = tex(materials, &gunmetal_img, Color::srgb(0.43, 0.46, 0.51), 0.34, 0.78);
    let gm_dark = tex(materials, &gunmetal_img, Color::srgb(0.25, 0.27, 0.31), 0.4, 0.72);
    let wood = tex(materials, &wood_img, Color::srgb(1.0, 0.94, 0.9), 0.6, 0.0);
    let hazard = tex(materials, &hazard_img, Color::WHITE, 0.5, 0.15);
    // Signal-Mortar shroud: warm tint + a hazard-textured #F2A20C self-glow so the amber
    // stripes stay punchy on dark floors (the black stripes stay black).
    let rk_hazard = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.84, 0.55),
        base_color_texture: Some(hazard_img.clone()),
        emissive: LinearRgba::from(Color::srgb(0.949, 0.635, 0.047)) * 0.32,
        emissive_texture: Some(hazard_img.clone()),
        perceptual_roughness: 0.42,
        metallic: 0.15,
        ..default()
    });
    // Mars Combat Pump: light brushed-gunmetal top planes over a black-oxide receiver
    // side (gunmetal texture averages ~0.78, so tints are pre-compensated).
    let sg_top = tex(materials, &gunmetal_img, Color::srgb(0.49, 0.54, 0.60), 0.30, 0.85);
    let sg_oxide = tex(materials, &gunmetal_img, Color::srgb(0.14, 0.155, 0.18), 0.42, 0.78);
    // UAC Plasma Repeater: dark industrial slate chassis + dark brushed titanium accents,
    // so the cyan coils are the brightest thing on the gun.
    let pl_slate = tex(materials, &obsidian_img, Color::srgb(0.16, 0.18, 0.23), 0.36, 0.74);
    let pl_titanium = tex(materials, &gunmetal_img, Color::srgb(0.30, 0.34, 0.41), 0.32, 0.82);
    let pl_edge = mat(materials, Color::srgb(0.52, 0.56, 0.62), 0.22, 0.95);
    let brass = tex(materials, &gold_img, Color::srgb(1.0, 0.9, 0.72), 0.28, 0.9);
    let copper = tex(materials, &gold_img, Color::srgb(0.95, 0.56, 0.38), 0.32, 0.88);

    // Saturated glow accents (lit + emissive so the hue survives tonemapping).
    let sight_green = lit_glow(materials, Color::srgb(0.12, 0.95, 0.3), LinearRgba::rgb(0.25, 3.2, 0.55));
    let sight_red = lit_glow(materials, Color::srgb(1.0, 0.1, 0.05), LinearRgba::rgb(3.8, 0.32, 0.1));
    let vent_amber = lit_glow(materials, Color::srgb(1.0, 0.54, 0.12), ROCKET_GLOW);
    // Incandescent yellow-white core, saturated orange-amber flame tongues.
    let flash_core = glow(materials, Color::srgb(1.0, 0.96, 0.72), LinearRgba::rgb(4.2, 3.2, 0.9));
    let flash_fire = glow(materials, Color::srgb(1.0, 0.48, 0.06), LinearRgba::rgb(1.45, 0.34, 0.006));
    let flash_cyan = glow(materials, Color::srgb(0.3, 0.9, 1.0), LinearRgba::rgb(0.5, 4.2, 7.5));
    let flash_cyan_halo = materials.add(additive_glow(Color::srgb(0.2, 0.92, 1.0), 0.65));
    let flash_amber_halo = materials.add(additive_glow(Color::srgb(1.0, 0.5, 0.1), 0.6));
    // Animated (per-shot reactive) glows live in PhoneMaterials, replaced every run.
    let coil = lit_glow(materials, Color::srgb(0.05, 0.62, 0.95), PLASMA_GLOW);
    // Static brighter inner plasma core (conduit tube / capacitor) over the deeper coils.
    let plasma_core = lit_glow(materials, Color::srgb(0.45, 0.9, 1.0), LinearRgba::rgb(0.18, 1.6, 2.6));
    let warhead_glow = lit_glow(materials, Color::srgb(1.0, 0.54, 0.12), ROCKET_GLOW);
    // Recessed ignition ring deep in the muzzle brake (static, a touch hotter/redder).
    let bore_glow = lit_glow(materials, Color::srgb(1.0, 0.42, 0.05), LinearRgba::rgb(2.4, 0.55, 0.02));
    VmKit { cube, cyl, thin_ring, bead, coil_ring, steel, brushed, dark_steel, heat_blued, shell_red, grip, glove, glove_plate, bolt_silver, gm_steel, gm_titanium, gm_dark, wood, hazard, rk_hazard, sg_top, sg_oxide, pl_slate, pl_titanium, pl_edge, brass, copper, sight_green, sight_red, vent_amber, flash_core, flash_fire, flash_cyan, flash_cyan_halo, flash_amber_halo, coil, plasma_core, warhead_glow, bore_glow }
}

/// Spawn the three sculpted gun models (Shotgun, Plasma Repeater, Signal-Mortar) under
/// `wroot`, batching rigid parts into `vb`. Returns the [`GunModel`] pivots; only the
/// Shotgun starts visible. `hands` adds the armored marine glove gripping each gun
/// (the first-person viewmodel); the Armory showcase leaves it out.
fn spawn_gun_models(commands: &mut Commands, wroot: Entity, kit: &VmKit, src: &VmSourceMeshes, vb: &mut ParentedBatches, hands: bool) -> [Entity; 3] {
    let VmKit { coil_ring, steel, brushed, dark_steel, heat_blued, shell_red, grip, glove, glove_plate, bolt_silver, gm_steel, gm_titanium, gm_dark, wood, hazard, rk_hazard, sg_top, sg_oxide, pl_slate, pl_titanium, pl_edge, brass, copper, sight_green, sight_red, vent_amber, flash_core, flash_fire, flash_cyan, flash_cyan_halo, flash_amber_halo, coil, plasma_core, warhead_glow, bore_glow, .. } = kit.clone();
    let rx = Quat::from_rotation_x(FRAC_PI_2);
    let rz = Quat::from_rotation_z(FRAC_PI_2);
    let id = Quat::IDENTITY;
    let one = Vec3::ONE;
    // Prism / ring axis (+Y) pointing at the muzzle (-Z) or the rear (+Z), rotated about
    // that axis so an n-gon has a flat face on top.
    let fwd = Quat::from_rotation_x(-FRAC_PI_2);
    let fwd_flat = |n: usize| fwd * Quat::from_rotation_y(PI / n as f32);
    let aft_flat = |n: usize| rx * Quat::from_rotation_y(PI / n as f32);
    // Shared small sculpted hardware.
    let hex_bolt = sculpt::tapered_prism(6, 0.0048, 0.0048, 0.0045, true);
    let trigger_blade = sculpt::curved_horn(0.0032, 0.0018, 0.022, 0.0, 0.007, 5, 6);
    let trigger_guard = sculpt::armored_ring(0.019, 0.0025, 0.0035, 16, true);
    let wrist_cuff = sculpt::armored_ring(0.046, 0.008, 0.017, 12, true);
    let knuckle_plate = sculpt::beveled_box(Vec3::new(0.022, 0.048, 0.068), 0.006);
    let guard_rot = Quat::from_rotation_z(FRAC_PI_2);
    let guard_scale = Vec3::new(1.0, 1.0, 1.5);
    let arm_rot = Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2);

    // ---- Weapon 1: Mars Combat Pump Shotgun ----
    let sg = commands.spawn((ChildOf(wroot), GunModel(Weapon::Shotgun), Transform::default(), Visibility::default())).id();
    // Chamfered heavy receiver + sloped dust-cover shroud + beveled trigger housing.
    // Black-oxide receiver sides vs light brushed-gunmetal top planes: the value split
    // keeps the beveled form readable against dark walls.
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.072, 0.088, 0.25), 0.009), &sg_oxide, Vec3::new(0.0, 0.0, -0.01), one, id);
    vb.part(sg, &sculpt::wedge_plate(0.066, 0.016, 0.24, 0.62, 0.95, 0.0), &sg_top, Vec3::new(0.0, 0.052, -0.01), one, id);
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.054, 0.022, 0.17), 0.006), &gm_titanium, Vec3::new(0.0, -0.052, 0.01), one, id);
    // Hex rivets on both receiver flanks.
    for z in [-0.11f32, -0.01, 0.07] {
        for s in [-1.0f32, 1.0] {
            vb.part(sg, &hex_bolt, &bolt_silver, Vec3::new(0.0372 * s, -0.026, z), one, rz);
        }
    }
    // Recessed left-side Mars/UAC hazard stripe (dark inlay + diagonal stripe decal).
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.003, 0.024, 0.125), 0.001), &grip, Vec3::new(-0.0362, 0.010, -0.03), one, id);
    vb.part(sg, &decal(Vec2::new(0.115, 0.016), Vec2::new(1.8, 0.25)), &hazard, Vec3::new(-0.0379, 0.010, -0.03), one, Quat::from_rotation_y(-FRAC_PI_2));
    // Right-side recessed ejection port with the chambered red/brass shell + bolt guide rails.
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.004, 0.030, 0.076), 0.0012), &grip, Vec3::new(0.0352, 0.014, -0.046), one, id);
    vb.part(sg, &src.cyl, &shell_red, Vec3::new(0.0335, 0.014, -0.053), Vec3::new(0.0075, 0.046, 0.0075), rx);
    vb.part(sg, &src.cyl, &brass, Vec3::new(0.0335, 0.014, -0.025), Vec3::new(0.0082, 0.010, 0.0082), rx);
    for y in [0.0305f32, -0.0025] {
        vb.part(sg, &sculpt::beveled_box(Vec3::new(0.004, 0.003, 0.16), 0.001), &sg_top, Vec3::new(0.0378, y, -0.02), one, id);
    }
    // Beveled Picatinny rail + slots on the dust cover.
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.026, 0.006, 0.17), 0.0015), &gm_dark, Vec3::new(0.0, 0.063, -0.02), one, id);
    let rail_slot = sculpt::beveled_box(Vec3::new(0.029, 0.005, 0.011), 0.0015);
    for k in 0..7 {
        vb.part(sg, &rail_slot, &dark_steel, Vec3::new(0.0, 0.068, -0.095 + k as f32 * 0.025), one, id);
    }
    // Rear ghost-ring aperture on a sloped base, guarded by ears with green tritium dots.
    vb.part(sg, &sculpt::wedge_plate(0.034, 0.014, 0.018, 0.6, 0.8, 0.0), &gm_dark, Vec3::new(0.0, 0.067, 0.08), one, id);
    vb.part(sg, &sculpt::armored_ring(0.0105, 0.0028, 0.0032, 16, true), &gm_steel, Vec3::new(0.0, 0.087, 0.08), one, rx);
    for s in [-1.0f32, 1.0] {
        vb.part(sg, &sculpt::beveled_box(Vec3::new(0.006, 0.022, 0.014), 0.0015), &gm_dark, Vec3::new(0.017 * s, 0.081, 0.08), one, id);
        vb.part(sg, &src.bead, &sight_green, Vec3::new(0.017 * s, 0.086, 0.0875), Vec3::splat(0.0032), id);
    }
    // Heavy 12-facet barrel + dark bore.
    vb.part(sg, &sculpt::tapered_prism(12, 0.019, 0.0175, 0.46, true), &gm_steel, Vec3::new(0.0, 0.030, -0.36), one, fwd_flat(12));
    vb.part(sg, &sculpt::tapered_prism(12, 0.012, 0.012, 0.004, true), &grip, Vec3::new(0.0, 0.030, -0.5965), one, fwd_flat(12));
    // Chamfered heat-shield: two angled vented roof plates, beveled cooling collars,
    // raised ventilated top rib on posts.
    let shield_plate = sculpt::beveled_box(Vec3::new(0.022, 0.0035, 0.30), 0.001);
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let vent_hole = Vec3::new(0.011, 0.0012, 0.014);
    for s in [-1.0f32, 1.0] {
        let q = Quat::from_rotation_z(-0.55 * s);
        let c = Vec3::new(0.0123 * s, 0.050, -0.34);
        vb.part(sg, &shield_plate, &gm_dark, c, one, q);
        for k in 0..6 {
            vb.part(sg, &src.cube, &grip, c + q * Vec3::new(0.0, 0.0019, -0.11 + k as f32 * 0.044), vent_hole, q);
        }
    }
    let collar = sculpt::armored_ring(0.024, 0.0035, 0.006, 12, true);
    for z in [-0.17f32, -0.34, -0.51] {
        vb.part(sg, &collar, &brushed, Vec3::new(0.0, 0.030, z), one, rx);
    }
    let rib_post = sculpt::beveled_box(Vec3::new(0.006, 0.007, 0.008), 0.0015);
    for k in 0..7 {
        vb.part(sg, &rib_post, &dark_steel, Vec3::new(0.0, 0.0565, -0.175 - k as f32 * 0.06), one, id);
    }
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.012, 0.004, 0.40), 0.0012), &sg_top, Vec3::new(0.0, 0.062, -0.355), one, id);
    // Sloped front sight ramp + red fiber-optic bead.
    vb.part(sg, &sculpt::wedge_plate(0.012, 0.016, 0.03, 0.5, 0.4, 0.006), &gm_dark, Vec3::new(0.0, 0.072, -0.548), one, id);
    vb.part(sg, &src.bead, &sight_red, Vec3::new(0.0, 0.0825, -0.545), Vec3::splat(0.0045), id);
    // Ported heat-blued compensator with dark slots.
    vb.part(sg, &sculpt::tapered_prism(8, 0.0245, 0.0225, 0.062, true), &heat_blued, Vec3::new(0.0, 0.030, -0.564), one, fwd_flat(8));
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let port_slot = Vec3::new(0.010, 0.003, 0.007);
    for k in 0..3 {
        let z = -0.549 - k as f32 * 0.014;
        vb.part(sg, &src.cube, &grip, Vec3::new(0.0, 0.0528, z), port_slot, id);
        for s in [-1.0f32, 1.0] {
            vb.part(sg, &src.cube, &grip, Vec3::new(0.0228 * s, 0.030, z), port_slot, rz);
        }
    }
    // Under-barrel tubular magazine + heavy beveled clamp + knurled front cap.
    vb.part(sg, &sculpt::tapered_prism(16, 0.0155, 0.0155, 0.40, false), &gm_dark, Vec3::new(0.0, -0.012, -0.33), one, fwd);
    vb.part(sg, &sculpt::tapered_prism(12, 0.018, 0.0165, 0.026, true), &brushed, Vec3::new(0.0, -0.012, -0.535), one, fwd_flat(12));
    let knurl = sculpt::armored_ring(0.0175, 0.0015, 0.002, 12, true);
    for z in [-0.527f32, -0.543] {
        vb.part(sg, &knurl, &grip, Vec3::new(0.0, -0.012, z), one, rx);
    }
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.034, 0.066, 0.024), 0.005), &gm_steel, Vec3::new(0.0, 0.010, -0.49), one, id);
    vb.part(sg, &hex_bolt, &bolt_silver, Vec3::new(0.0175, 0.010, -0.49), one, rz);
    // Contoured walnut stock + raised comb + rubber buttpad.
    let stock_rot = Quat::from_rotation_x(0.14);
    let stock_c = Vec3::new(0.0, -0.048, 0.20);
    vb.part(sg, &uv_scaled(sculpt::beveled_box(Vec3::new(0.052, 0.074, 0.21), 0.010), Vec2::new(2.0, 1.0)), &wood, stock_c, one, stock_rot);
    vb.part(sg, &sculpt::wedge_plate(0.046, 0.02, 0.16, 0.72, 0.85, 0.01), &wood, stock_c + stock_rot * Vec3::new(0.0, 0.047, -0.01), one, stock_rot);
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.056, 0.084, 0.022), 0.006), &grip, Vec3::new(0.0, -0.062, 0.31), one, stock_rot);
    // Walnut stock wrist + raised cheek-weld crest right behind the receiver (above the
    // HUD panel, so the wood+steel palette reads up close, not only on the pump), with a
    // black-oxide receiver tang strap + bolt over the wood.
    let wrist_rot = Quat::from_rotation_x(0.07);
    vb.part(sg, &uv_scaled(sculpt::beveled_box(Vec3::new(0.054, 0.066, 0.085), 0.011), Vec2::new(1.5, 1.0)), &wood, Vec3::new(0.0, 0.004, 0.132), one, wrist_rot);
    vb.part(sg, &uv_scaled(sculpt::wedge_plate(0.046, 0.026, 0.13, 0.6, 0.78, -0.012), Vec2::new(2.0, 1.0)), &wood, Vec3::new(0.0, 0.041, 0.172), one, Quat::from_rotation_x(0.11));
    vb.part(sg, &sculpt::beveled_box(Vec3::new(0.024, 0.005, 0.058), 0.0015), &sg_oxide, Vec3::new(0.0, 0.0385, 0.128), one, wrist_rot);
    vb.part(sg, &hex_bolt, &bolt_silver, Vec3::new(0.0, 0.0425, 0.142), one, id);
    // Ergonomic pistol grip + sculpted trigger guard + curved trigger.
    vb.part(sg, &src.cap, &grip, Vec3::new(0.0, -0.095, 0.075), Vec3::new(0.046, 0.07, 0.052), Quat::from_rotation_x(-0.38));
    vb.part(sg, &trigger_guard, &gm_dark, Vec3::new(0.0, -0.066, -0.01), guard_scale, guard_rot);
    vb.part(sg, &trigger_blade, &steel, Vec3::new(0.0, -0.058, -0.006), one, Quat::from_rotation_x(PI));
    // Armored marine glove + knuckle plate + wrist cuff.
    if hands {
        vb.part(sg, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.082), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.35));
        vb.part(sg, &knuckle_plate, &glove_plate, Vec3::new(0.036, -0.096, 0.065), one, Quat::from_rotation_x(-0.3));
        vb.part(sg, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.20), Vec3::new(0.088, 0.11, 0.088), arm_rot);
        vb.part(sg, &wrist_cuff, &glove_plate, Vec3::new(0.048, -0.155, 0.165), one, arm_rot);
    }

    // Sliding pump forend: contoured walnut with dark rubber grip grooves + dual action bars.
    let pump = commands.spawn((ChildOf(sg), PumpForend, Transform::from_xyz(0.0, -0.012, -0.30), Visibility::default())).id();
    vb.part(pump, &uv_scaled(sculpt::tapered_prism(12, 0.031, 0.029, 0.17, true), Vec2::new(3.0, 1.0)), &wood, Vec3::ZERO, one, fwd_flat(12));
    vb.part(pump, &uv_scaled(sculpt::beveled_box(Vec3::new(0.056, 0.030, 0.16), 0.009), Vec2::new(2.0, 1.0)), &wood, Vec3::new(0.0, -0.016, 0.0), one, id);
    let groove = sculpt::armored_ring(0.0305, 0.0028, 0.0035, 12, true);
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let side_groove = Vec3::new(0.002, 0.018, 0.006);
    for k in 0..6 {
        let z = -0.065 + k as f32 * 0.026;
        vb.part(pump, &groove, &grip, Vec3::new(0.0, 0.0, z), one, fwd_flat(12));
        for s in [-1.0f32, 1.0] {
            vb.part(pump, &src.cube, &grip, Vec3::new(0.0285 * s, -0.016, z), side_groove, id);
        }
    }
    for s in [-1.0f32, 1.0] {
        vb.part(pump, &sculpt::beveled_box(Vec3::new(0.005, 0.008, 0.18), 0.0015), &gm_steel, Vec3::new(0.027 * s, 0.005, 0.13), one, id);
    }
    // Articulated bolt carrier over the ejection port (slides back on the pump stroke).
    let bolt = commands.spawn((ChildOf(sg), ShotgunBolt, Transform::from_translation(SHOTGUN_BOLT_REST), Visibility::default())).id();
    vb.part(bolt, &sculpt::beveled_box(Vec3::new(0.006, 0.028, 0.066), 0.0018), &gm_steel, Vec3::ZERO, one, id);
    for k in 0..4 {
        vb.part(bolt, &src.cube, &dark_steel, Vec3::new(0.0032, 0.0, 0.016 + k as f32 * 0.0055), Vec3::new(0.0015, 0.02, 0.002), id);
    }
    vb.part(bolt, &sculpt::beveled_box(Vec3::new(0.004, 0.008, 0.008), 0.001), &steel, Vec3::new(0.0035, 0.0, -0.026), one, id);
    // Doom-style volumetric muzzle blast: incandescent yellow-white core, a forward
    // muzzle cone and asymmetric jagged orange-amber flame tongues of uneven length and
    // splay (rolled + scaled per shot, so no two flashes match).
    let star = commands
        .spawn((ChildOf(sg), MuzzleFlash(Weapon::Shotgun), MuzzleStar, Transform::from_translation(viewmodel_muzzle_local(Weapon::Shotgun) - Vec3::Z * 0.02), Visibility::Hidden))
        .id();
    // (angle about the bore, length, base width, forward tilt)
    for (a, len, w, tilt) in [
        (0.15f32, 0.20f32, 0.030f32, 0.32f32),
        (0.95, 0.11, 0.021, 0.52),
        (1.70, 0.17, 0.026, 0.28),
        (2.55, 0.09, 0.019, 0.60),
        (3.30, 0.19, 0.029, 0.38),
        (4.10, 0.12, 0.022, 0.46),
        (4.95, 0.15, 0.025, 0.30),
        (5.70, 0.08, 0.018, 0.62),
    ] {
        let dir = Vec3::new(a.cos() * tilt.cos(), a.sin() * tilt.cos(), -tilt.sin());
        let q = Quat::from_rotation_arc(Vec3::Y, dir);
        vb.part(star, &src.cone, &flash_fire, dir * (len * 0.5), Vec3::new(w, len, w * 0.55), q);
        if len > 0.14 {
            vb.part(star, &src.cone, &flash_core, dir * (len * 0.28), Vec3::new(w * 0.5, len * 0.52, w * 0.3), q);
        }
    }
    let fwd_cone = Quat::from_rotation_x(-FRAC_PI_2);
    vb.part(star, &src.cone, &flash_fire, Vec3::new(0.0, 0.0, -0.085), Vec3::new(0.036, 0.17, 0.036), fwd_cone);
    vb.part(star, &src.cone, &flash_core, Vec3::new(0.0, 0.0, -0.05), Vec3::new(0.02, 0.10, 0.02), fwd_cone);
    vb.part(star, &src.sphere, &flash_core, Vec3::ZERO, Vec3::splat(0.032), id);

    // ---- Weapon 2: UAC Plasma Repeater (twin-rail sci-fi accelerator) ----
    let pl = commands.spawn((ChildOf(wroot), GunModel(Weapon::Plasma), Transform::default(), Visibility::Hidden)).id();
    // Chamfered dark-slate lower receiver + swept dark-titanium upper shroud (dark chassis
    // so the electric-cyan plasma is the hottest value on the gun), with bright machined
    // edge strips on the receiver chamfers.
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.088, 0.090, 0.34), 0.011), &pl_slate, Vec3::new(0.0, -0.008, -0.10), one, id);
    vb.part(pl, &sculpt::wedge_plate(0.074, 0.036, 0.30, 0.72, 0.88, -0.012), &pl_titanium, Vec3::new(0.0, 0.055, -0.13), one, id);
    for s in [-1.0f32, 1.0] {
        let q = Quat::from_rotation_z(-s * FRAC_PI_4);
        vb.part(pl, &src.cube, &pl_edge, Vec3::new(0.0389 * s, 0.0319, -0.10), Vec3::new(0.0062, 0.0012, 0.316), q);
        vb.part(pl, &src.cube, &pl_edge, Vec3::new(0.0389 * s, -0.0479, -0.10), Vec3::new(0.0062, 0.0012, 0.316), Quat::from_rotation_z(s * FRAC_PI_4));
    }
    // Hot capacitor core under the flux slide (exposed when it snaps back).
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.024, 0.006, 0.12), 0.002), &coil, Vec3::new(0.0, 0.076, -0.15), one, id);
    // Hazard-striped battery bulkhead band.
    vb.part(pl, &uv_scaled(sculpt::beveled_box(Vec3::new(0.094, 0.018, 0.05), 0.004), Vec2::new(2.0, 0.5)), &hazard, Vec3::new(0.0, 0.016, 0.045), one, id);
    // Angular rear stock + cheek rest + rubber buttplate.
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.066, 0.084, 0.16), 0.010), &pl_slate, Vec3::new(0.0, -0.02, 0.15), one, id);
    vb.part(pl, &sculpt::wedge_plate(0.054, 0.022, 0.13, 0.7, 0.8, 0.012), &pl_titanium, Vec3::new(0.0, 0.033, 0.15), one, id);
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.07, 0.092, 0.016), 0.004), &grip, Vec3::new(0.0, -0.02, 0.235), one, id);
    // Holographic sight: beveled base, posts and hood, cyan reticle dot + ticks.
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.05, 0.008, 0.036), 0.002), &gm_dark, Vec3::new(0.0, 0.077, -0.002), one, id);
    for s in [-1.0f32, 1.0] {
        vb.part(pl, &sculpt::beveled_box(Vec3::new(0.006, 0.030, 0.028), 0.0015), &gm_dark, Vec3::new(0.023 * s, 0.096, -0.002), one, id);
        vb.part(pl, &src.cube, &coil, Vec3::new(0.011 * s, 0.096, -0.004), Vec3::new(0.006, 0.0012, 0.0012), id);
    }
    vb.part(pl, &sculpt::wedge_plate(0.054, 0.008, 0.032, 0.8, 0.85, 0.0), &gm_dark, Vec3::new(0.0, 0.115, -0.002), one, id);
    vb.part(pl, &src.bead, &coil, Vec3::new(0.0, 0.096, -0.004), Vec3::splat(0.0034), id);
    vb.part(pl, &src.cube, &coil, Vec3::new(0.0, 0.0875, -0.004), Vec3::new(0.0012, 0.005, 0.0012), id);
    // Twin chamfered accelerator rails (dark centre groove) + flared emitter tines.
    let rail = sculpt::beveled_box(Vec3::new(0.034, 0.016, 0.30), 0.004);
    let tine_plate = |s: f32| sculpt::wedge_plate(0.034, 0.045, 0.016, 1.25, 0.55, 0.010 * s);
    for s in [-1.0f32, 1.0] {
        vb.part(pl, &rail, &pl_titanium, Vec3::new(0.0, 0.034 * s, -0.42), one, id);
        vb.part(pl, &src.cube, &grip, Vec3::new(0.0, 0.034 * s, -0.42), Vec3::new(0.0352, 0.003, 0.26), id);
        vb.part(pl, &src.cube, &pl_edge, Vec3::new(0.0, 0.034 * s + 0.0082 * s, -0.42), Vec3::new(0.026, 0.0008, 0.29), id);
        vb.part(pl, &tine_plate(s), &pl_titanium, Vec3::new(0.0, 0.034 * s, -0.5925), one, fwd);
    }
    // Energised plasma conduit (brighter inner core tube inside the deeper coils) + end
    // collars + muzzle lens.
    vb.part(pl, &sculpt::tapered_prism(10, 0.0145, 0.0145, 0.32, false), &plasma_core, Vec3::new(0.0, 0.0, -0.40), one, fwd);
    let conduit_collar = sculpt::armored_ring(0.017, 0.003, 0.005, 12, true);
    for z in [-0.255f32, -0.545] {
        vb.part(pl, &conduit_collar, &gm_dark, Vec3::new(0.0, 0.0, z), one, rx);
    }
    vb.part(pl, &src.sphere, &coil, Vec3::new(0.0, 0.0, -0.562), Vec3::splat(0.019), id);
    for k in 0..5 {
        commands.spawn((
            PlasmaCoil(k),
            vm_part(pl, &coil_ring, &coil, Vec3::new(0.0, 0.0, -0.29 - k as f32 * 0.054), Vec3::new(0.044, 0.052, 0.044), rx),
        ));
    }
    // Left pressurised plasma canister: glowing cell, beveled end caps, containment cage,
    // mount bracket and an angled braided power feed.
    let can = Vec3::new(-0.062, -0.008, 0.0);
    vb.part(pl, &sculpt::tapered_prism(14, 0.0215, 0.0215, 0.15, false), &coil, can + Vec3::Z * -0.07, one, fwd);
    let can_cap = sculpt::tapered_prism(8, 0.025, 0.019, 0.02, true);
    vb.part(pl, &can_cap, &gm_dark, can + Vec3::Z * -0.155, one, fwd_flat(8));
    vb.part(pl, &can_cap, &gm_dark, can + Vec3::Z * 0.015, one, aft_flat(8));
    let cage_ring = sculpt::armored_ring(0.023, 0.0028, 0.004, 14, true);
    for z in [-0.12f32, -0.07, -0.02] {
        vb.part(pl, &cage_ring, &gm_dark, can + Vec3::Z * z, one, rx);
    }
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let cage_bar = Vec3::new(0.004, 0.004, 0.15);
    for a in [PI * 0.72, PI, PI * 1.28] {
        vb.part(pl, &src.cube, &gm_dark, can + Vec3::new(a.cos() * 0.0235, a.sin() * 0.0235, -0.07), cage_bar, Quat::from_rotation_z(a));
    }
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.02, 0.012, 0.03), 0.003), &gm_dark, Vec3::new(-0.047, -0.008, -0.07), one, id);
    let feed_rot = Quat::from_rotation_x(1.3) * Quat::from_rotation_z(-0.45);
    let feed_c = Vec3::new(-0.052, -0.012, -0.20);
    vb.part(pl, &src.cap, &dark_steel, feed_c, Vec3::new(0.012, 0.06, 0.012), feed_rot);
    let braid = sculpt::armored_ring(0.0068, 0.0016, 0.0022, 10, true);
    for t in [-0.03f32, -0.015, 0.0, 0.015, 0.03] {
        vb.part(pl, &braid, &brushed, feed_c + feed_rot * Vec3::Y * t, one, feed_rot);
    }
    // Right side: angled copper radiator fins over glowing warm-amber thermal vents.
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.004, 0.058, 0.20), 0.0015), &grip, Vec3::new(0.0435, 0.010, -0.15), one, id);
    let fin = sculpt::wedge_plate(0.05, 0.016, 0.007, 0.7, 1.0, 0.0);
    let fin_rot = Quat::from_rotation_y(0.35) * Quat::from_rotation_z(-FRAC_PI_2);
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let thermal_vent = Vec3::new(0.003, 0.034, 0.008);
    for k in 0..6 {
        let z = -0.235 + k as f32 * 0.034;
        vb.part(pl, &fin, &copper, Vec3::new(0.052, 0.010, z), one, fin_rot);
        if k < 5 {
            vb.part(pl, &src.cube, &vent_amber, Vec3::new(0.0462, 0.010, z + 0.017), thermal_vent, id);
        }
    }
    // Angled sci-fi grip + trigger guard/trigger + armored marine glove.
    vb.part(pl, &sculpt::beveled_box(Vec3::new(0.040, 0.088, 0.05), 0.009), &grip, Vec3::new(0.0, -0.092, 0.032), one, Quat::from_rotation_x(-0.32));
    vb.part(pl, &trigger_guard, &gm_dark, Vec3::new(0.0, -0.060, -0.025), guard_scale, guard_rot);
    vb.part(pl, &trigger_blade, &steel, Vec3::new(0.0, -0.052, -0.022), one, Quat::from_rotation_x(PI));
    if hands {
        vb.part(pl, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.045), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.3));
        vb.part(pl, &knuckle_plate, &glove_plate, Vec3::new(0.036, -0.096, 0.03), one, Quat::from_rotation_x(-0.28));
        vb.part(pl, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.17), Vec3::new(0.088, 0.11, 0.088), arm_rot);
        vb.part(pl, &wrist_cuff, &glove_plate, Vec3::new(0.048, -0.155, 0.135), one, arm_rot);
    }
    // Reciprocating flux slide on the top shroud (snaps back on every shot).
    let slide = commands.spawn((ChildOf(pl), PlasmaFluxSlide, Transform::from_translation(PLASMA_SLIDE_REST), Visibility::default())).id();
    vb.part(slide, &sculpt::beveled_box(Vec3::new(0.046, 0.016, 0.13), 0.004), &pl_titanium, Vec3::ZERO, one, id);
    for k in 0..4 {
        vb.part(slide, &src.cube, &grip, Vec3::new(0.0, 0.001, 0.038 + k as f32 * 0.007), Vec3::new(0.0475, 0.010, 0.0025), id);
    }
    for s in [-1.0f32, 1.0] {
        vb.part(slide, &src.cube, &coil, Vec3::new(0.0233 * s, -0.002, -0.02), Vec3::new(0.0015, 0.0035, 0.07), id);
    }
    vb.part(slide, &uv_scaled(sculpt::beveled_box(Vec3::new(0.022, 0.003, 0.018), 0.0008), Vec2::new(1.0, 0.4)), &hazard, Vec3::new(0.0, 0.0085, 0.04), one, id);
    // Layered plasma muzzle flash (bright core + additive outer halo).
    let pl_flash = commands
        .spawn((ChildOf(pl), MuzzleFlash(Weapon::Plasma), Transform::from_xyz(0.0, 0.0, -0.60), Visibility::Hidden))
        .id();
    vb.part(pl_flash, &src.sphere, &flash_cyan, Vec3::ZERO, Vec3::splat(0.045), id);
    vb.part(pl_flash, &src.sphere, &flash_cyan_halo, Vec3::ZERO, Vec3::splat(0.095), id);
    vb.part(pl_flash, &src.thin_ring, &flash_cyan, Vec3::new(0.0, 0.0, -0.01), Vec3::new(0.075, 0.015, 0.075), rx);

    // ---- Weapon 3: Signal-Mortar (heavy Mars industrial launcher) ----
    let rk = commands.spawn((ChildOf(wroot), GunModel(Weapon::Rocket), Transform::default(), Visibility::Hidden)).id();
    let axis = Vec3::new(0.0, 0.012, 0.0);
    // Faceted octagonal titanium launch tube.
    vb.part(rk, &sculpt::tapered_prism(8, 0.052, 0.050, 0.60, true), &gm_titanium, axis + Vec3::Z * -0.20, one, fwd_flat(8));
    // Stepped dark-steel forward jacket with 4 beveled octagonal cooling ribs.
    vb.part(rk, &sculpt::tapered_prism(8, 0.059, 0.057, 0.16, true), &gm_dark, axis + Vec3::Z * -0.43, one, fwd_flat(8));
    let rib = sculpt::armored_ring(0.061, 0.0045, 0.006, 8, true);
    for z in [-0.37f32, -0.41, -0.45, -0.49] {
        vb.part(rk, &rib, &gm_steel, axis + Vec3::Z * z, one, fwd_flat(8));
    }
    // Bold diagonal hazard stabilizer shroud with dark trim rings.
    vb.part(rk, &uv_scaled(sculpt::tapered_prism(8, 0.0555, 0.0555, 0.13, true), Vec2::new(3.0, 1.0)), &rk_hazard, axis + Vec3::Z * -0.25, one, fwd_flat(8));
    let trim = sculpt::armored_ring(0.0565, 0.003, 0.005, 8, true);
    for z in [-0.185f32, -0.315] {
        vb.part(rk, &trim, &gm_dark, axis + Vec3::Z * z, one, fwd_flat(8));
    }
    // Massive flared heat-blued muzzle brake + blast-deflector fins + deep dark bore.
    vb.part(rk, &sculpt::flared_dish(0.05, 0.078, 0.06, 0.012, 16), &heat_blued, axis + Vec3::Z * -0.505, one, fwd);
    vb.part(rk, &sculpt::tapered_prism(16, 0.036, 0.036, 0.004, false), &grip, axis + Vec3::Z * -0.52, one, fwd);
    // Recessed glowing amber ignition/pilot ring deep in the brake throat.
    vb.part(rk, &sculpt::armored_ring(0.041, 0.0035, 0.0025, 16, true), &bore_glow, axis + Vec3::Z * -0.5232, one, rx);
    vb.part(rk, &sculpt::armored_ring(0.022, 0.002, 0.0015, 12, true), &bore_glow, axis + Vec3::Z * -0.5225, one, rx);
    let deflector = sculpt::beveled_box(Vec3::new(0.006, 0.026, 0.045), 0.0015);
    for k in 0..4 {
        let a = FRAC_PI_4 + k as f32 * FRAC_PI_2;
        vb.part(rk, &deflector, &heat_blued, axis + Vec3::new(a.cos() * 0.074, a.sin() * 0.074, -0.54), one, Quat::from_rotation_z(a - FRAC_PI_2));
    }
    // Flared rear backblast venturi bell (opens to +Z) with dark exhaust nozzle + collar.
    vb.part(rk, &sculpt::flared_dish(0.044, 0.062, 0.045, 0.009, 16), &gm_dark, axis + Vec3::Z * 0.095, one, rx);
    vb.part(rk, &sculpt::tapered_prism(16, 0.031, 0.031, 0.004, false), &grip, axis + Vec3::Z * 0.1065, one, rx);
    vb.part(rk, &sculpt::armored_ring(0.05, 0.004, 0.008, 8, true), &gm_dark, axis + Vec3::Z * 0.095, one, fwd_flat(8));
    // Seen end-on from the eye: brushed steel bell lip, a stator hub and six slotted
    // dark exhaust vanes inside the venturi.
    vb.part(rk, &sculpt::armored_ring(0.0615, 0.003, 0.004, 16, true), &brushed, axis + Vec3::Z * 0.139, one, rx);
    vb.part(rk, &sculpt::tapered_prism(8, 0.015, 0.008, 0.024, true), &gm_dark, axis + Vec3::Z * 0.112, one, aft_flat(8));
    let stator = sculpt::beveled_box(Vec3::new(0.0035, 0.028, 0.02), 0.001);
    for k in 0..6 {
        let a = PI / 6.0 + k as f32 * PI / 3.0;
        vb.part(rk, &stator, &dark_steel, axis + Vec3::new(a.cos() * 0.031, a.sin() * 0.031, 0.117), one, Quat::from_rotation_z(a - FRAC_PI_2) * Quat::from_rotation_y(0.35));
    }
    // Rear butt detail (the tube top carries the breech cradle, so this wraps the side
    // and lower faces only): dark beveled vent-collar panels with slotted vents, slotted
    // exhaust fins on the octagon corners, and a brushed steel clamping ring with bolted
    // lugs just behind the breech, so the rear tube reads as hardware, not a blank cylinder.
    let collar_panel = sculpt::beveled_box(Vec3::new(0.034, 0.005, 0.06), 0.0015);
    for k in 0..5 {
        // Right side, lower-right, bottom, lower-left, left faces.
        let a = -(k as f32) * FRAC_PI_4;
        let n = Vec3::new(a.cos(), a.sin(), 0.0);
        let q = Quat::from_rotation_z(a - FRAC_PI_2);
        vb.part(rk, &collar_panel, &gm_dark, axis + n * 0.0505 + Vec3::Z * 0.03, one, q);
        for z in [0.012f32, 0.026, 0.040] {
            vb.part(rk, &src.cube, &grip, axis + n * 0.0532 + Vec3::Z * z, Vec3::new(0.024, 0.0016, 0.0055), q);
        }
    }
    let exhaust_fin = sculpt::beveled_box(Vec3::new(0.0045, 0.013, 0.05), 0.0012);
    for k in 0..6 {
        let a = PI / 8.0 - k as f32 * FRAC_PI_4;
        vb.part(rk, &exhaust_fin, &dark_steel, axis + Vec3::new(a.cos() * 0.060, a.sin() * 0.060, 0.03), one, Quat::from_rotation_z(a - FRAC_PI_2));
    }
    vb.part(rk, &sculpt::armored_ring(0.0535, 0.0035, 0.0075, 8, true), &steel, axis + Vec3::Z * 0.078, one, fwd_flat(8));
    for s in [-1.0f32, 1.0] {
        vb.part(rk, &sculpt::beveled_box(Vec3::new(0.008, 0.014, 0.016), 0.002), &steel, axis + Vec3::new(0.058 * s, 0.0, 0.078), one, id);
        vb.part(rk, &hex_bolt, &bolt_silver, axis + Vec3::new(0.0625 * s, 0.0, 0.078), one, rz);
    }
    // Glowing segmented ammo-status windows on both tube flanks.
    for s in [-1.0f32, 1.0] {
        vb.part(rk, &sculpt::beveled_box(Vec3::new(0.005, 0.03, 0.085), 0.0015), &gm_dark, axis + Vec3::new(0.0495 * s, 0.0, -0.07), one, id);
        vb.part(rk, &sculpt::beveled_box(Vec3::new(0.004, 0.018, 0.068), 0.001), &warhead_glow, axis + Vec3::new(0.0515 * s, 0.0, -0.07), one, id);
        for k in 0..4 {
            vb.part(rk, &src.cube, &grip, axis + Vec3::new(0.0537 * s, 0.0, -0.096 + k as f32 * 0.017), Vec3::new(0.001, 0.02, 0.003), id);
        }
    }
    // Glowing amber heat-vent slits on the lower facets over dark recessed backing.
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let slit = Vec3::new(0.016, 0.003, 0.0045);
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let vent_back = Vec3::new(0.024, 0.0015, 0.07);
    for s in [-1.0f32, 1.0] {
        let q = Quat::from_rotation_z(s * FRAC_PI_4);
        let n = Vec3::new(s * std::f32::consts::FRAC_1_SQRT_2, -std::f32::consts::FRAC_1_SQRT_2, 0.0);
        vb.part(rk, &src.cube, &grip, axis + n * 0.0482 + Vec3::Z * -0.14, vent_back, q);
        for k in 0..5 {
            vb.part(rk, &src.cube, &vent_amber, axis + n * 0.0492 + Vec3::Z * (-0.115 - k as f32 * 0.012), slit, q);
        }
    }
    // Optical rangefinder: mount, beveled housing, flared lens hood, eyepiece with amber
    // targeting reticle and a range readout strip.
    let rf = Vec3::new(-0.046, 0.098, 0.0);
    vb.part(rk, &sculpt::beveled_box(Vec3::new(0.012, 0.04, 0.05), 0.003), &gm_dark, Vec3::new(-0.040, 0.064, 0.02), one, id);
    vb.part(rk, &sculpt::beveled_box(Vec3::new(0.032, 0.03, 0.095), 0.005), &gm_dark, rf + Vec3::Z * 0.01, one, id);
    vb.part(rk, &sculpt::tapered_prism(8, 0.012, 0.0145, 0.026, true), &grip, rf + Vec3::Z * -0.05, one, fwd_flat(8));
    vb.part(rk, &sculpt::tapered_prism(8, 0.011, 0.009, 0.012, true), &grip, rf + Vec3::Z * 0.064, one, aft_flat(8));
    vb.part(rk, &src.bead, &warhead_glow, rf + Vec3::Z * 0.0705, Vec3::splat(0.0042), id);
    for s in [-1.0f32, 1.0] {
        vb.part(rk, &src.cube, &warhead_glow, rf + Vec3::new(0.0075 * s, 0.0, 0.0705), Vec3::new(0.0055, 0.0012, 0.0012), id);
    }
    vb.part(rk, &src.cube, &warhead_glow, rf + Vec3::new(-0.0165, 0.006, 0.01), Vec3::new(0.0015, 0.005, 0.03), id);
    // Front red aiming post on the jacket.
    vb.part(rk, &sculpt::wedge_plate(0.008, 0.018, 0.012, 0.5, 0.6, 0.0), &gm_dark, Vec3::new(0.0, 0.0755, -0.47), one, id);
    vb.part(rk, &src.bead, &sight_red, Vec3::new(0.0, 0.087, -0.47), Vec3::splat(0.0045), id);
    // Braced under-barrel stabilizer rails + cross braces + silver hex bolts.
    for x in [-0.052f32, 0.052] {
        vb.part(rk, &sculpt::beveled_box(Vec3::new(0.010, 0.014, 0.42), 0.003), &gm_steel, Vec3::new(x, -0.038, -0.25), one, id);
    }
    let brace = sculpt::beveled_box(Vec3::new(0.114, 0.010, 0.018), 0.003);
    for z in [-0.42f32, -0.25, -0.08] {
        vb.part(rk, &brace, &gm_titanium, Vec3::new(0.0, -0.038, z), one, id);
        for x in [-0.059f32, 0.059] {
            vb.part(rk, &hex_bolt, &bolt_silver, Vec3::new(x, -0.038, z), one, rz);
        }
    }
    // Front vertical foregrip with finger grooves + rear pistol grip, guard and trigger.
    vb.part(rk, &sculpt::beveled_box(Vec3::new(0.034, 0.012, 0.05), 0.003), &gm_dark, Vec3::new(0.0, -0.049, -0.30), one, id);
    let fg_rot = Quat::from_rotation_x(0.12);
    let fg_c = Vec3::new(0.0, -0.088, -0.30);
    vb.part(rk, &sculpt::beveled_box(Vec3::new(0.03, 0.07, 0.036), 0.007), &grip, fg_c, one, fg_rot);
    for k in 0..3 {
        vb.part(rk, &src.cube, &gm_dark, fg_c + fg_rot * Vec3::new(0.0, -0.018 + k as f32 * 0.016, 0.0), Vec3::new(0.0315, 0.003, 0.037), fg_rot);
    }
    vb.part(rk, &src.cap, &grip, Vec3::new(0.0, -0.095, 0.035), Vec3::new(0.046, 0.072, 0.052), Quat::from_rotation_x(-0.32));
    vb.part(rk, &trigger_guard, &gm_dark, Vec3::new(0.0, -0.058, -0.025), guard_scale, guard_rot);
    vb.part(rk, &trigger_blade, &steel, Vec3::new(0.0, -0.05, -0.02), one, Quat::from_rotation_x(PI));
    // Armored marine glove + knuckle plate + wrist cuff.
    if hands {
        vb.part(rk, &src.cap, &glove, Vec3::new(0.012, -0.112, 0.045), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.3));
        vb.part(rk, &knuckle_plate, &glove_plate, Vec3::new(0.036, -0.096, 0.03), one, Quat::from_rotation_x(-0.28));
        vb.part(rk, &src.cap, &glove, Vec3::new(0.042, -0.17, 0.17), Vec3::new(0.088, 0.11, 0.088), arm_rot);
        vb.part(rk, &wrist_cuff, &glove_plate, Vec3::new(0.048, -0.155, 0.135), one, arm_rot);
    }
    // Open-frame breech cradle carrying the exposed finned warhead: kicks back on fire
    // and lifts/vents through the reload cycle.
    let breech = commands.spawn((ChildOf(rk), RocketBreech, Transform::from_translation(ROCKET_BREECH_REST), Visibility::default())).id();
    vb.part(breech, &sculpt::beveled_box(Vec3::new(0.056, 0.008, 0.15), 0.003), &gm_dark, Vec3::new(0.0, -0.012, 0.0), one, id);
    for s in [-1.0f32, 1.0] {
        vb.part(breech, &sculpt::beveled_box(Vec3::new(0.008, 0.028, 0.15), 0.0025), &gm_titanium, Vec3::new(0.025 * s, 0.004, 0.0), one, id);
        for z in [-0.05f32, 0.05] {
            vb.part(breech, &hex_bolt, &bolt_silver, Vec3::new(0.0295 * s, 0.004, z), one, rz);
        }
    }
    let strap = sculpt::beveled_box(Vec3::new(0.058, 0.006, 0.012), 0.0015);
    for z in [-0.05f32, 0.05] {
        vb.part(breech, &strap, &gm_steel, Vec3::new(0.0, 0.021, z), one, id);
    }
    vb.part(breech, &sculpt::tapered_prism(12, 0.0125, 0.0125, 0.07, false), &brass, Vec3::new(0.0, 0.004, 0.01), one, fwd);
    vb.part(breech, &sculpt::tapered_prism(12, 0.0125, 0.0015, 0.045, false), &warhead_glow, Vec3::new(0.0, 0.004, -0.0475), one, fwd);
    vb.part(breech, &sculpt::armored_ring(0.0128, 0.002, 0.003, 12, true), &warhead_glow, Vec3::new(0.0, 0.004, -0.012), one, rx);
    // Sub-millimetre bevel is invisible at this size: a 12-tri cube, not a 64-tri bevel.
    let tail_fin = Vec3::new(0.002, 0.012, 0.024);
    for k in 0..4 {
        let a = FRAC_PI_4 + k as f32 * FRAC_PI_2;
        vb.part(breech, &src.cube, &gm_dark, Vec3::new(a.cos() * 0.017, 0.004 + a.sin() * 0.017, 0.04), tail_fin, Quat::from_rotation_z(a - FRAC_PI_2));
    }
    vb.part(breech, &sculpt::beveled_box(Vec3::new(0.05, 0.03, 0.02), 0.004), &gm_dark, Vec3::new(0.0, 0.004, 0.085), one, id);
    vb.part(breech, &uv_scaled(sculpt::beveled_box(Vec3::new(0.04, 0.004, 0.016), 0.001), Vec2::new(1.0, 0.4)), &rk_hazard, Vec3::new(0.0, 0.021, 0.085), one, id);
    // Fiery mortar muzzle flash (cone + hot core + additive halo + ring).
    let rk_flash = commands
        .spawn((ChildOf(rk), MuzzleFlash(Weapon::Rocket), Transform::from_xyz(0.0, 0.012, -0.60), Visibility::Hidden))
        .id();
    vb.part(rk_flash, &src.cone, &flash_fire, Vec3::new(0.0, 0.0, -0.06), Vec3::new(0.06, 0.16, 0.06), Quat::from_rotation_x(-FRAC_PI_2));
    vb.part(rk_flash, &src.sphere, &flash_core, Vec3::ZERO, Vec3::splat(0.05), id);
    vb.part(rk_flash, &src.sphere, &flash_amber_halo, Vec3::ZERO, Vec3::splat(0.11), id);
    vb.part(rk_flash, &src.thin_ring, &flash_fire, Vec3::new(0.0, 0.0, 0.01), Vec3::new(0.085, 0.015, 0.085), rx);
    [sg, pl, rk]
}

/// Armory showcase (Compendium turntable): the real sculpted viewmodel guns, built
/// by the same code as the player viewmodel, parented under `parent` on the
/// [`OVERLAY_LAYER`]. Returns `[shotgun, plasma, mortar]` pivots (only the shotgun
/// starts visible). Allocates once per call, never per frame.
pub(crate) fn spawn_gun_showcase(
    commands: &mut Commands,
    parent: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) -> [Entity; 3] {
    let kit = vm_kit(meshes, materials, images);
    let src = VmSourceMeshes::new();
    let mut vb = ParentedBatches::new();
    let guns = spawn_gun_models(commands, parent, &kit, &src, &mut vb, false);
    for (p, mesh, mat) in vb.finish(meshes) {
        commands.spawn(vm_part(p, &mesh, &mat, Vec3::ZERO, Vec3::ONE, Quat::IDENTITY));
    }
    guns
}


fn sync_overlay_fov(main: Query<&Projection, (With<PlayerCamera>, Without<OverlayCamera>)>, mut overlay: Query<&mut Projection, With<OverlayCamera>>) {
    let (Ok(Projection::Perspective(m)), Ok(mut o)) = (main.single(), overlay.single_mut()) else { return };
    if let Projection::Perspective(op) = o.as_mut()
        && (op.fov - m.fov).abs() > 1e-4
    {
        op.fov = m.fov;
    }
}

/// Duration of the `F` weapon inspect flourish.
pub const INSPECT_SECS: f32 = 1.1;
/// Deepest landing dip of the viewmodel (meters, downward).
pub const LANDING_DIP_MAX: f32 = 0.045;
/// Landing dip spring stiffness (1/s^2) and damping ratio.
const DIP_STIFFNESS: f32 = 240.0;
const DIP_DAMPING_RATIO: f32 = 0.55;

/// Spring-damped viewmodel landing dip + touchdown tracking.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ViewmodelFeel {
    /// Vertical dip offset (<= 0 while dipping).
    pub dip: f32,
    pub dip_vel: f32,
    pub was_grounded: bool,
    /// Most negative vertical speed of the current airtime.
    pub air_vy: f32,
}

impl ViewmodelFeel {
    /// Track grounded transitions and integrate the dip spring for one frame.
    pub fn update(&mut self, grounded: bool, vy: f32, dt: f32) {
        if !grounded {
            self.air_vy = self.air_vy.min(vy);
        } else if !self.was_grounded {
            self.dip_vel -= landing_dip_impulse(-self.air_vy);
            self.air_vy = 0.0;
        }
        if grounded {
            self.air_vy = 0.0;
        }
        self.was_grounded = grounded;
        (self.dip, self.dip_vel) = dip_spring_step(self.dip, self.dip_vel, dt);
    }
}

/// Downward velocity kick of the viewmodel for a touchdown at `impact_speed`
/// (m/s, positive down): nothing for tiny steps, capped for huge falls.
pub fn landing_dip_impulse(impact_speed: f32) -> f32 {
    if !impact_speed.is_finite() || impact_speed <= 2.0 { 0.0 } else { ((impact_speed - 2.0) * 0.05).min(0.65) }
}

/// One semi-implicit, sub-stepped step of the underdamped dip spring
/// (target 0); the offset is clamped to `[-LANDING_DIP_MAX, LANDING_DIP_MAX * 0.4]`.
pub fn dip_spring_step(x: f32, v: f32, dt: f32) -> (f32, f32) {
    let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.0 };
    let (mut x, mut v) = (if x.is_finite() { x } else { 0.0 }, if v.is_finite() { v } else { 0.0 });
    let c = 2.0 * DIP_STIFFNESS.sqrt() * DIP_DAMPING_RATIO;
    let n = ((dt / (1.0 / 240.0)).ceil() as usize).max(1);
    let h = dt / n as f32;
    for _ in 0..n {
        v += (-DIP_STIFFNESS * x - c * v) * h;
        x += v * h;
        let lo = -LANDING_DIP_MAX;
        let hi = LANDING_DIP_MAX * 0.4;
        if x < lo || x > hi {
            x = x.clamp(lo, hi);
            v = 0.0;
        }
    }
    (x, v)
}

/// Extra roll (cant) of the viewmodel during a swap: swings toward the
/// direction of travel through the deck, peaking mid-swap.
pub fn swap_cant(swap_timer: f32, dir: f32) -> f32 {
    if swap_timer <= 0.0 || !swap_timer.is_finite() {
        return 0.0;
    }
    let u = (1.0 - swap_timer / SWAP_SECS).clamp(0.0, 1.0);
    (u * PI).sin() * 0.34 * dir.clamp(-1.0, 1.0)
}

/// Inspect flourish pose `(offset, rotation)` for the remaining `timer`:
/// eases in, tilts and turns the gun to show its side, then eases back.
pub fn inspect_pose(timer: f32) -> (Vec3, Quat) {
    if timer <= 0.0 || !timer.is_finite() {
        return (Vec3::ZERO, Quat::IDENTITY);
    }
    let u = (1.0 - timer / INSPECT_SECS).clamp(0.0, 1.0);
    let smooth = |k: f32| {
        let k = k.clamp(0.0, 1.0);
        k * k * (3.0 - 2.0 * k)
    };
    let env = smooth(u / 0.25) * smooth((1.0 - u) / 0.25);
    let yaw = env * (0.95 + 0.12 * (u * TAU).sin());
    let roll = env * (0.42 + 0.1 * (u * PI * 3.0).sin());
    let pitch = env * 0.16;
    (Vec3::new(-0.075, 0.035, 0.025) * env, Quat::from_rotation_y(yaw) * Quat::from_rotation_z(roll) * Quat::from_rotation_x(pitch))
}

fn cancel_inspect(mut ws: ResMut<WeaponState>) {
    ws.inspect_timer = 0.0;
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
    mut feel: ResMut<ViewmodelFeel>,
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
        Query<&mut Transform, With<ShotgunBolt>>,
        Query<&mut Transform, With<PlasmaFluxSlide>>,
        Query<&mut Transform, With<RocketBreech>>,
    )>,
    mut models: Query<(&GunModel, &mut Visibility), Without<MuzzleFlash>>,
    mut flashes: Query<(&MuzzleFlash, &mut Visibility), Without<GunModel>>,
    mut mlight: Query<&mut PointLight, With<MuzzleLight>>,
) {
    let Ok(p) = players.single() else { return };
    let t = time.elapsed_secs();
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    *bob_amp = damp_bob_amp(*bob_amp, viewmodel_bob_target(p.grounded, speed), time.delta_secs());
    feel.update(p.grounded, p.velocity.y, time.delta_secs());
    let amp = *bob_amp;
    let bob = Vec3::new((p.bob_phase * 0.5).cos() * 0.011 * amp, -(p.bob_phase.sin().abs()) * 0.013 * amp, 0.0)
        + Vec3::new(0.0, (t * 1.6).sin() * 0.0025, 0.0);
    let half = SWAP_SECS * 0.5;
    let swap = if ws.swap_timer > 0.0 { 1.0 - ((ws.swap_timer - half).abs() / half).min(1.0) } else { 0.0 };
    let shown = ws.displayed();

    if let Ok(mut tf) = set.p0().single_mut() {
        let kick = ws.recoil;
        // (back, lift, pitch, roll): the Signal-Mortar shoves deep into the shoulder with an
        // eased (held) kick and a brief authoritative roll that settles back to level.
        let (back, lift, pitch, roll) = match shown {
            Weapon::Rocket => {
                let k = (kick / 1.4).clamp(0.0, 1.0);
                let e = k * k * (3.0 - 2.0 * k);
                (e * 0.15, e * 0.02, e * 0.32, (k * PI).sin() * 0.07)
            }
            _ => (kick * 0.095, kick * 0.022, kick * 0.36, 0.0),
        };
        let (insp_off, insp_rot) = inspect_pose(ws.inspect_timer);
        let cant = swap_cant(ws.swap_timer, ws.swap_dir);
        tf.translation = WEAPON_REST + bob + Vec3::new(0.0, -0.3 * swap + lift + feel.dip, back) + insp_off;
        tf.rotation = Quat::from_rotation_x(pitch - swap * 0.6 + feel.dip * 1.6)
            * Quat::from_rotation_z(-swap * 0.3 - roll - cant)
            * Quat::from_rotation_y(cant * 0.35)
            * insp_rot;
    }
    let cast = apps.cast_anim.clamp(0.0, 1.0);
    if let Ok(mut tf) = set.p1().single_mut() {
        tf.translation = PHONE_REST + bob * 0.55 + Vec3::new(0.045 * cast, 0.055 * cast + feel.dip * 0.7, 0.03 * cast);
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
    // Shotgun bolt rides back with the pump stroke, exposing the chambered shell.
    if let Ok(mut tf) = set.p5().single_mut() {
        let k = (ws.pump * PI).sin().max(0.0);
        tf.translation = SHOTGUN_BOLT_REST + Vec3::Z * (k * 0.066);
    }
    // Plasma flux slide snaps fully back on each shot and springs home.
    if let Ok(mut tf) = set.p6().single_mut() {
        let k = if shown == Weapon::Plasma { (ws.recoil / 0.35).clamp(0.0, 1.0) } else { 0.0 };
        tf.translation = PLASMA_SLIDE_REST + Vec3::Z * (k.sqrt() * 0.05);
    }
    // Mortar breech: hard rearward kick on fire, then a lift-and-tilt vent through the reload.
    if let Ok(mut tf) = set.p7().single_mut() {
        // Normalised by the cooldown the shot actually armed, so fire-rate upgrades and
        // 5G Overclock run the full lift-and-vent cycle instead of starting it midway.
        let (kick, cyc) = if shown == Weapon::Rocket {
            ((ws.recoil / 1.4).clamp(0.0, 1.0), (ws.cycle_progress() * PI).sin().max(0.0))
        } else {
            (0.0, 0.0)
        };
        tf.translation = ROCKET_BREECH_REST + Vec3::new(0.0, cyc * 0.006, kick * 0.042 + cyc * 0.01);
        tf.rotation = Quat::from_rotation_x(-cyc * 0.09);
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
    let (base, emissive) = phone_led_glow(signal.bars, signal.in_dead_zone, apps.ping_active_timer > 0.0 || apps.overload_flash_timer > 0.0, t);
    if let Some(mut m) = materials.get_mut(&pm.led) {
        m.base_color = base;
        m.emissive = emissive;
    }
    // Saturated plasma: gentle idle shimmer, flares on each shot without clipping to white
    // (peaks at ~PLASMA_FLARE on a fresh shot).
    if let Some(mut m) = materials.get_mut(&pm.coil) {
        let k = if shown == Weapon::Plasma { (0.06 * (t * 9.0).sin() + (ws.recoil / 0.35).clamp(0.0, 1.0)).clamp(0.0, 1.0) } else { 0.0 };
        m.emissive = PLASMA_GLOW.mix(&PLASMA_FLARE, k);
    }
    // Mortar warhead/vents: flare on fire, dim while venting, recharge to full by ready.
    if let Some(mut m) = materials.get_mut(&pm.warhead) {
        let k = if shown == Weapon::Rocket { 0.45 + 0.55 * ws.cycle_progress() + 1.0 * (ws.recoil / 1.4).clamp(0.0, 1.0) } else { 1.0 };
        m.emissive = ROCKET_GLOW * k;
    }
}

/// Phone status LED / antenna beacon colour for the current signal state:
/// `(base_color, emissive)`. A dark-but-hued base plus a saturated emissive kept in
/// ~1.0..3.2, so the bars hue / dead-zone red / ping cyan reads instead of clipping white.
pub fn phone_led_glow(bars: u8, dead_zone: bool, pinging: bool, t: f32) -> (Color, LinearRgba) {
    let (c, pulse) = if pinging {
        (LinearRgba::rgb(0.08, 0.85, 1.0), 3.2)
    } else if dead_zone {
        (LinearRgba::rgb(1.0, 0.05, 0.03), if (t * 8.0).sin() > 0.2 { 3.0 } else { 0.35 })
    } else {
        let c = signal_color(bars).to_linear();
        (c, 1.6 + 1.4 * (t * (3.0 + bars as f32 * 1.5)).sin().max(0.0))
    };
    // Normalise so the brightest channel is 1.0: pulse alone sets the intensity.
    let peak = c.red.max(c.green).max(c.blue).max(1e-3);
    let at = |k: f32| LinearRgba::rgb(c.red / peak * k, c.green / peak * k, c.blue / peak * k);
    (Color::LinearRgba(at(0.35)), at(pulse))
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
    barrels: Vec<Vec3>,
    enemy_dmg: Vec<f32>,
    enemy_kb: Vec<Vec3>,
    jam_dmg: Vec<f32>,
    bar_dmg: Vec<f32>,
    pylons: Vec<Vec3>,
    pyl_dmg: Vec<f32>,
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
    (mut sfx, mut feed, mut decals): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<DecalQueue>),
    cams: Query<&Transform, With<PlayerCamera>>,
    mut players: Query<(&mut Player, &Transform), Without<PlayerCamera>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    (mut jammers, mut barrels, mut pylons): (
        Query<(&mut DemonJammer, &GlobalTransform)>,
        Query<(&mut ExplosiveBarrel, &GlobalTransform)>,
        Query<(&mut BossPylon, &GlobalTransform)>,
    ),
    mut bot: Option<ResMut<BotControl>>,
    (mut scratch, pad): (Local<FireScratch>, Option<Res<crate::gamepad::GamepadInput>>),
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
    ws.inspect_timer = (ws.inspect_timer - dt).max(0.0);
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
    } else if pad.as_ref().is_some_and(|g| g.weapon_next) {
        want = Some(ws.current.next());
    } else if pad.as_ref().is_some_and(|g| g.weapon_prev) {
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
    // `RT` fires without cursor capture (controllers need no mouse click).
    let pad_fire = pad.as_ref().is_some_and(|g| g.fire);
    let trigger = (captured.0 && mouse.pressed(MouseButton::Left)) || ws.force_fire || bot_fire || pad_fire;
    // `F` / D-Pad Down inspect flourish (purely visual): pulling the trigger cancels it at once.
    if trigger {
        ws.inspect_timer = 0.0;
    } else if (keys.just_pressed(KeyCode::KeyF) || pad.as_ref().is_some_and(|g| g.inspect)) && ws.swap_timer <= 0.0 {
        ws.inspect_timer = INSPECT_SECS;
    }
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
    ws.last_cooldown = ws.cooldown;

    let origin = cam.translation;
    let rot = cam.rotation;
    let fwd = rot * Vec3::NEG_Z;
    // On the drawn barrel tip's view ray (see `muzzle_offset`).
    let muzzle = origin + rot * muzzle_offset(ws.current);
    // Doppler Overdrive: speed at the moment of the shot sets the bonus.
    let speed = Vec2::new(player.velocity.x, player.velocity.z).length();
    let dmult = doppler_damage_mult(speed, &mods);

    // Snapshot lightweight targets for ray tests (reused buffers).
    let FireScratch { targets: refs, jammers: jam_list, barrels: bar_list, enemy_dmg, enemy_kb, jam_dmg, bar_dmg, pylons: pyl_list, pyl_dmg } = &mut *scratch;
    refs.clear();
    refs.extend(enemies.iter().map(|(e, g, _)| HitTargetInfo::of(g.translation(), e)));
    jam_list.clear();
    jam_list.extend(jammers.iter().filter(|(j, _)| j.is_active()).map(|(_, g)| g.translation()));
    bar_list.clear();
    bar_list.extend(barrels.iter().filter(|(b, _)| b.is_intact()).map(|(_, g)| g.translation()));
    pyl_list.clear();
    pyl_list.extend(pylons.iter().filter(|(p, _)| p.is_alive()).map(|(_, g)| g.translation()));

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
            // Exactly at the viewmodel's right-side ejection port.
            let breach = origin + rot * (WEAPON_REST + SHOTGUN_PORT_LOCAL);
            spawn_ejected_shell(&mut commands, &fx, breach, right_dir, up_dir, fwd);
            let mut wall_decals = 0;
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
            bar_dmg.clear();
            bar_dmg.resize(bar_list.len(), 0.0);
            pyl_dmg.clear();
            pyl_dmg.resize(pyl_list.len(), 0.0);
            let mut wall_sparks = 0;
            for i in 0..pellets {
                let a = rng.random_range(0.0..TAU);
                let r = spread * rng.random::<f32>().sqrt();
                let local = Vec3::new(a.cos() * r, a.sin() * r, -1.0).normalize();
                let dir = rot * local;
                let (dist, target) = resolve_hitscan_all(&grid, origin, dir, HITSCAN_RANGE, refs, jam_list, bar_list, pyl_list);
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
                    HitTarget::Barrel(k) => {
                        bar_dmg[k] += PELLET_DAMAGE * falloff * dmult;
                        spawn_particles(&mut commands, &fx, hit, BurstColor::Spark, 3, 5.0, 0.06);
                    }
                    HitTarget::Pylon(k) => {
                        pyl_dmg[k] += PELLET_DAMAGE * falloff * dmult;
                        spawn_particles(&mut commands, &fx, hit, BurstColor::Cyan, 4, 5.5, 0.07);
                    }
                    HitTarget::Wall => {
                        spawn_particles(&mut commands, &fx, hit - dir * 0.05, BurstColor::Spark, 3, 3.8, 0.055);
                        // Up to two small pellet scorch marks per blast so blood & craters linger longer.
                        if wall_decals < 2
                            && let Some(h) = raycast_grid(&grid, origin, dir, dist + 0.1)
                        {
                            decals.push(DecalRequest::surface(DecalKind::Scorch, h.point, h.normal, 0.2));
                            wall_decals += 1;
                        }
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
            for (idx, (mut e, g, marked)) in enemies.iter_mut().enumerate() {
                if enemy_dmg[idx] > 0.0 {
                    let kb = enemy_kb[idx] * knockback_scale(e.kind);
                    damage_enemy(&mut e, enemy_dmg[idx], marked.is_some(), &mods);
                    e.knockback += Vec3::new(kb.x, 0.0, kb.z) * 3.0;
                    e.stagger(enemy_kb[idx]);
                    // Heavy close-range blast: a subtle splatter on the floor beneath.
                    let root = g.translation();
                    if enemy_dmg[idx] >= PELLET_DAMAGE * 5.0 && root.distance(origin) < 7.0 {
                        let kind = crate::enemies::death_decals(e.kind, false)[0].map_or(DecalKind::BloodCrimson, |d| d.0);
                        let feet = root - Vec3::Y * if e.kind.flying() { 2.0 } else { 0.0 };
                        decals.push(DecalRequest::floor(kind, feet + Vec3::new(fwd.x, 0.0, fwd.z) * 0.6, 0.7));
                    }
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
            let mut k = 0;
            for (mut b, _) in &mut barrels {
                if !b.is_intact() {
                    continue;
                }
                if bar_dmg[k] > 0.0 {
                    damage_barrel(&mut b, bar_dmg[k], false);
                }
                k += 1;
            }
            let mut k = 0;
            for (mut p, _) in &mut pylons {
                if !p.is_alive() {
                    continue;
                }
                if pyl_dmg[k] > 0.0 {
                    damage_pylon(&mut p, pyl_dmg[k]);
                    any_hit = true;
                }
                k += 1;
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
            // Hot cyan vent sparks from the cooling port on top of the receiver.
            spawn_plasma_vent(&mut commands, &fx, origin + rot * (WEAPON_REST + PLASMA_VENT_LOCAL), rot * Vec3::Y, rot * Vec3::X);
            spawn_flash_light(&mut commands, muzzle, Color::srgb(0.3, 0.9, 1.0), 250_000.0, 7.0, 0.05);
            // Converge the bolt onto whatever the crosshair points at.
            let (dist, _) = resolve_hitscan_all(&grid, origin, fwd, HITSCAN_RANGE, refs, jam_list, bar_list, pyl_list);
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
            sfx.play(SfxEvent::RocketWhoosh);
            spawn_flash_light(&mut commands, muzzle, Color::srgb(1.0, 0.55, 0.15), 600_000.0, 10.0, 0.09);
            spawn_particles(&mut commands, &fx, muzzle, BurstColor::Smoke, 3, 1.4, 0.08);
            spawn_mortar_casing(&mut commands, &fx, origin + rot * (WEAPON_REST + MORTAR_PORT_LOCAL), rot * Vec3::X, rot * Vec3::Y, fwd);
            let (dist, _) = resolve_hitscan_all(&grid, origin, fwd, HITSCAN_RANGE, refs, jam_list, bar_list, pyl_list);
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
    mut barrels: Query<(&mut ExplosiveBarrel, &GlobalTransform)>,
    mut players: Query<(&mut Player, &Transform)>,
    mut pylons: Query<(&mut BossPylon, &GlobalTransform)>,
    mut decals: ResMut<DecalQueue>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    for (e, mut b, mut tf) in &mut bolts {
        b.life -= dt;
        let from = tf.translation;
        let step = b.vel * dt;
        let len = step.length();
        let dir = step / len.max(1e-6);
        let mut best = (len, HitTarget::Nothing);
        let mut wall_normal = None;
        if let Some(h) = raycast_grid(&grid, from, dir, len) {
            best = (h.distance, HitTarget::Wall);
            wall_normal = Some(h.normal);
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
        for (i, (b, g)) in barrels.iter().enumerate() {
            if b.is_intact()
                && let Some(t) = barrel_ray_hit(g.translation(), from, dir)
                && t < best.0
            {
                best = (t, HitTarget::Barrel(i));
            }
        }
        for (i, (p, g)) in pylons.iter().enumerate() {
            if p.is_alive()
                && let Some(t) = pylon_ray_hit(g.translation(), from, dir)
                && t < best.0
            {
                best = (t, HitTarget::Pylon(i));
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
                &mut barrels,
                &mut pylons,
                &mut players,
                &mut decals,
                wall_normal.filter(|_| best.1 == HitTarget::Wall),
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
                    en.stagger(dir);
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
            HitTarget::Barrel(i) => {
                if let Some((mut b, _)) = barrels.iter_mut().nth(i) {
                    damage_barrel(&mut b, damage, false);
                }
            }
            HitTarget::Pylon(i) => {
                if let Some((mut p, _)) = pylons.iter_mut().nth(i) {
                    damage_pylon(&mut p, damage);
                    feed.hit(false);
                    sfx.play(SfxEvent::Hit);
                }
            }
            HitTarget::Wall => {
                if let Some(n) = wall_normal {
                    decals.push(DecalRequest::surface(DecalKind::PlasmaBurn, hit, n, 0.42));
                }
            }
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
    barrels: &mut Query<(&mut ExplosiveBarrel, &GlobalTransform)>,
    pylons: &mut Query<(&mut BossPylon, &GlobalTransform)>,
    players: &mut Query<(&mut Player, &Transform)>,
    decals: &mut DecalQueue,
    wall_normal: Option<Vec3>,
) -> bool {
    let radius = rocket_splash_radius(mods);
    // Charred scorch on the impact wall and / or the floor under the blast.
    if let Some(n) = wall_normal.filter(|n| n.y <= 0.7) {
        decals.push(DecalRequest::surface(DecalKind::Scorch, blast + dir * 0.08, n, 1.5));
    }
    decals.push(DecalRequest::floor(DecalKind::Scorch, blast, 2.2));
    for (i, (mut b, g)) in barrels.iter_mut().enumerate() {
        if !b.is_intact() {
            continue;
        }
        let dmg = if direct == HitTarget::Barrel(i) {
            ROCKET_DIRECT_DAMAGE * damage_mult
        } else {
            let c = g.translation() + Vec3::Y * BARREL_CENTER_Y;
            let d = (c.distance(blast) - BARREL_RADIUS).max(0.0);
            if d > radius || !has_line_of_sight(grid, blast, c) { 0.0 } else { rocket_splash_damage(d, radius, damage_mult) }
        };
        damage_barrel(&mut b, dmg, false);
    }
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
        en.stagger(Vec3::new(away.x, 0.0, away.y));
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
    for (i, (mut p, g)) in pylons.iter_mut().enumerate() {
        if !p.is_alive() {
            continue;
        }
        let dmg = if direct == HitTarget::Pylon(i) {
            ROCKET_DIRECT_DAMAGE * damage_mult
        } else {
            let base = g.translation();
            let d = splash_distance(blast, base, &PYLON_SPHERES);
            if d > radius || !has_line_of_sight(grid, blast, base + Vec3::Y * 1.5) { 0.0 } else { rocket_splash_damage(d, radius, damage_mult) }
        };
        if dmg > 0.0 {
            damage_pylon(&mut p, dmg);
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
    sfx.play_at(SfxEvent::RocketExplode, blast);
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
        sfx.play_at(SfxEvent::JammerDestroyed, base + Vec3::Y * 2.0);
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

/// Tick barrel fuses and detonate the armed ones: LoS-respecting linear
/// falloff blast on demons, jammers and the player (forgiving blast jump),
/// staggered chain fuses on nearby barrels, and the explosion VFX.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn barrel_detonations(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    mods: Res<PlayerModifiers>,
    fx: Res<FxAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    (mut sfx, mut feed, mut decals): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<DecalQueue>),
    mut barrels: Query<(Entity, &mut ExplosiveBarrel, &GlobalTransform, &mut Transform), Without<Player>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    (mut jammers, mut pylons): (Query<(&mut DemonJammer, &GlobalTransform)>, Query<(&mut BossPylon, &GlobalTransform)>),
    mut players: Query<(&mut Player, &Transform), Without<ExplosiveBarrel>>,
    mut blasts: Local<Vec<(Entity, Vec3)>>,
) {
    let dt = time.delta_secs();
    blasts.clear();
    for (e, mut b, g, mut tf) in &mut barrels {
        if b.exploded {
            continue;
        }
        if b.hit_flash > 0.0 {
            b.hit_flash = (b.hit_flash - dt).max(0.0);
            let s = 1.0 + b.hit_flash * 0.8;
            tf.scale = Vec3::new(s, 1.0 + b.hit_flash * 0.3, s);
        } else if tf.scale != Vec3::ONE {
            tf.scale = Vec3::ONE;
        }
        if let Some(f) = b.fuse {
            let f = f - dt;
            if f <= 0.0 {
                b.exploded = true;
                blasts.push((e, g.translation() + Vec3::Y * BARREL_CENTER_Y));
            } else {
                b.fuse = Some(f);
            }
        }
    }
    let mut any_hit = false;
    for &(e, blast) in blasts.iter() {
        commands.entity(e).try_despawn();
        for (mut en, g, marked) in &mut enemies {
            if !en.is_alive() {
                continue;
            }
            let root = g.translation();
            let d = splash_distance(blast, root, en.stats.hit_spheres);
            if d > BARREL_BLAST_RADIUS || !has_line_of_sight(&grid, blast, en.center(root)) {
                continue;
            }
            let falloff = splash_falloff(d, BARREL_BLAST_RADIUS);
            let away = Vec2::new(root.x - blast.x, root.z - blast.z).normalize_or(Vec2::X);
            let kb = 16.0 * falloff * knockback_scale(en.kind);
            damage_enemy(&mut en, barrel_blast_damage(d), marked.is_some(), &mods);
            en.knockback += Vec3::new(away.x * kb, kb * 0.2, away.y * kb);
            en.stagger(Vec3::new(away.x, 0.0, away.y));
            any_hit = true;
        }
        for (mut j, g) in &mut jammers {
            if !j.is_active() {
                continue;
            }
            let base = g.translation();
            let d = splash_distance(blast, base, &JAMMER_SPHERES);
            if d <= BARREL_BLAST_RADIUS && has_line_of_sight(&grid, blast, base + Vec3::Y * 1.3) {
                damage_jammer(&mut j, barrel_blast_damage(d));
                any_hit = true;
            }
        }
        for (mut p, g) in &mut pylons {
            let base = g.translation();
            let d = splash_distance(blast, base, &PYLON_SPHERES);
            if p.is_alive() && d <= BARREL_BLAST_RADIUS && has_line_of_sight(&grid, blast, base + Vec3::Y * 1.5) {
                damage_pylon(&mut p, barrel_blast_damage(d));
                any_hit = true;
            }
        }
        for (_, mut b, g, _) in &mut barrels {
            let c = g.translation() + Vec3::Y * BARREL_CENTER_Y;
            let d = (c.distance(blast) - BARREL_RADIUS).max(0.0);
            if b.is_intact() && d <= BARREL_BLAST_RADIUS && has_line_of_sight(&grid, blast, c) {
                damage_barrel(&mut b, barrel_blast_damage(d), true);
            }
        }
        if let Ok((mut p, ptf)) = players.single_mut()
            && !p.is_dead()
            && has_line_of_sight(&grid, blast, ptf.translation + Vec3::Y * 0.9)
            && let Some(jump) = compute_barrel_jump(ptf.translation, blast, &mods)
        {
            apply_rocket_jump(&mut p, &jump);
        }
        decals.push(DecalRequest::floor(DecalKind::Scorch, blast - Vec3::Y * BARREL_CENTER_Y, 2.8));
        spawn_flash_light(&mut commands, blast, Color::srgb(1.0, 0.42, 0.12), 3_200_000.0, 18.0, 0.32);
        spawn_shockwave(&mut commands, &fx, &mut materials, blast, BARREL_BLAST_RADIUS * 0.9, 0.36, BARREL_BLAST_GLOW.0, BARREL_BLAST_GLOW.1, true);
        spawn_shockwave(&mut commands, &fx, &mut materials, blast, BARREL_BLAST_RADIUS * 0.45, 0.22, ROCKET_CORE_GLOW.0, ROCKET_CORE_GLOW.1, false);
        spawn_burst(&mut commands, &fx, blast, BurstColor::Fire, 18, 10.0, 0.18);
        spawn_particles(&mut commands, &fx, blast, BurstColor::Spark, 10, 12.0, 0.06);
        spawn_particles(&mut commands, &fx, blast + Vec3::Y * 0.4, BurstColor::Smoke, 6, 2.6, 0.32);
        sfx.play_at(SfxEvent::RocketExplode, blast);
    }
    if any_hit {
        feed.hit(false);
    }
}

/// Barrel explosion shockwave ring glow `(color, alpha)`.
const BARREL_BLAST_GLOW: (Color, f32) = (Color::srgb(1.0, 0.32, 0.08), 0.8);

/// Plasma bolt impact shockwave glow `(color, alpha)`.
const BOLT_IMPACT_GLOW: (Color, f32) = (Color::srgb(0.3, 0.95, 1.0), 0.7);
/// Rocket explosion shockwave ring glow `(color, alpha)`.
const ROCKET_BLAST_GLOW: (Color, f32) = (Color::srgb(1.0, 0.55, 0.12), 0.75);
/// Rocket explosion fireball core glow `(color, alpha)`.
const ROCKET_CORE_GLOW: (Color, f32) = (Color::srgb(1.0, 0.78, 0.35), 0.8);

fn prewarm_weapon_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[BOLT_IMPACT_GLOW, ROCKET_BLAST_GLOW, ROCKET_CORE_GLOW, BARREL_BLAST_GLOW]);
}

fn reset_viewmodel_feel(mut feel: ResMut<ViewmodelFeel>) {
    *feel = ViewmodelFeel { was_grounded: true, ..default() };
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponState>()
            .init_resource::<ViewmodelFeel>()
            .add_systems(OnExit(AppState::Playing), cancel_inspect)
            .configure_sets(Update, CombatDamage.in_set(GameSet::Combat))
            .add_systems(Startup, (create_pickup_assets, create_bolt_assets, create_rocket_assets, prewarm_weapon_fades))
            .add_systems(Update, spawn_viewmodels)
            .add_systems(Update, (spawn_floor_pickups, reset_viewmodel_feel).run_if(resource_exists_and_changed::<LevelGrid>).in_set(GameSet::World))
            .add_systems(
                Update,
                (fire_weapons, move_player_projectiles).chain().in_set(CombatDamage).run_if(resource_exists::<LevelGrid>),
            )
            .add_systems(
                Update,
                barrel_detonations.in_set(GameSet::Combat).after(CombatDamage).before(jammer_deaths).run_if(resource_exists::<LevelGrid>),
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

    fn tris(m: &Mesh) -> usize {
        m.indices().map_or(m.count_vertices(), |i| i.len()) / 3
    }

    #[test]
    fn rocket_cycle_spans_the_cooldown_the_shot_actually_armed() {
        // A fire-rate-boosted shot: the cycle starts at 0, not partway through.
        let fast = ROCKET_COOLDOWN * 0.5;
        let mut ws = WeaponState { current: Weapon::Rocket, cooldown: fast, last_cooldown: fast, ..default() };
        assert!(ws.cycle_progress().abs() < 1e-6, "fresh shot starts the cycle at 0");
        ws.cooldown = fast * 0.5;
        assert!((ws.cycle_progress() - 0.5).abs() < 1e-6, "halfway through the real cooldown");
        ws.cooldown = 0.0;
        assert_eq!(ws.cycle_progress(), 1.0, "ready");
        // A swap (not a shot) arming the cooldown runs no reload cycle.
        let mut ws = WeaponState { current: Weapon::Shotgun, last_cooldown: SHOTGUN_COOLDOWN, ..default() };
        assert!(ws.switch_to(Weapon::Rocket));
        assert!(ws.cooldown > 0.0);
        assert_eq!(ws.cycle_progress(), 1.0);
        // ...but swapping mid-cycle keeps the shot's cycle.
        let mut ws = WeaponState { current: Weapon::Rocket, cooldown: 0.6, last_cooldown: 0.85, ..default() };
        ws.switch_to(Weapon::Plasma);
        assert_eq!((ws.cooldown, ws.last_cooldown), (0.6, 0.85));
    }

    #[test]
    fn muzzle_origin_sits_on_the_drawn_barrel_tip_ray() {
        for w in Weapon::ALL {
            let m = muzzle_offset(w);
            let tip = WEAPON_REST + viewmodel_muzzle_local(w);
            assert!((m.z + MUZZLE_DEPTH).abs() < 1e-5, "{w:?} depth");
            assert!(m.normalize().angle_between(tip.normalize()) < 1e-4, "{w:?} on the tip's view ray");
            assert!(m.x > 0.1 && m.y < -0.08, "{w:?} lower-right of the crosshair");
        }
        // The ejection port is on the right flank of the shotgun receiver.
        assert!(SHOTGUN_PORT_LOCAL.x > 0.03 && (WEAPON_REST + SHOTGUN_PORT_LOCAL).z < -0.4);
    }

    #[test]
    fn phone_led_shows_a_saturated_signal_hue() {
        let green = phone_led_glow(5, false, false, 0.3).1;
        let red = phone_led_glow(5, true, false, 0.2).1;
        let cyan = phone_led_glow(1, true, true, 0.0).1;
        assert!(green.green > 1.0 && green.green > 1.5 * green.blue && green.green > 5.0 * green.red, "{green:?}");
        assert_eq!(green.alpha, 1.0);
        assert!(red.red > 1.0 && red.red > 5.0 * red.green.max(red.blue), "{red:?}");
        assert!(cyan.blue > 1.0 && cyan.green > 1.0 && cyan.red < 0.5, "{cyan:?}");
        for bars in 0..=5 {
            for k in 0..40 {
                let (base, e) = phone_led_glow(bars, k % 3 == 0, k % 7 == 0, k as f32 * 0.13);
                let peak = e.red.max(e.green).max(e.blue);
                assert!((0.3..=3.2 + 1e-4).contains(&peak), "emissive never clips to white: {e:?}");
                assert!(base.to_linear().red.max(base.to_linear().green).max(base.to_linear().blue) < 0.5, "dark hued base");
            }
        }
    }

    #[test]
    fn viewmodel_primitives_stay_low_poly() {
        let src = VmSourceMeshes::new();
        for (name, m, max) in [
            ("cyl", &src.cyl, 48),
            ("cap", &src.cap, 200),
            ("thin_ring", &src.thin_ring, 192),
            ("sphere", &src.sphere, 160),
            ("bead", &src.bead, 96),
            ("cone", &src.cone, 24),
        ] {
            assert!(tris(m) <= max, "{name}: {} tris", tris(m));
        }
        assert!(tris(&Mesh::from(Torus::new(0.78, 1.0))) > 5 * tris(&src.thin_ring));
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

    #[test]
    fn barrel_hit_cylinder_ray_tests() {
        let base = Vec3::new(10.0, 0.0, 10.0);
        // Side hit from the front at mid height.
        let t = barrel_ray_hit(base, Vec3::new(10.0, 0.6, 20.0), Vec3::NEG_Z).unwrap();
        assert!((t - (10.0 - BARREL_RADIUS)).abs() < 1e-4, "{t}");
        // Over the top and beside: miss.
        assert!(barrel_ray_hit(base, Vec3::new(10.0, 1.5, 20.0), Vec3::NEG_Z).is_none());
        assert!(barrel_ray_hit(base, Vec3::new(10.7, 0.6, 20.0), Vec3::NEG_Z).is_none());
        // Behind the ray origin: miss.
        assert!(barrel_ray_hit(base, Vec3::new(10.0, 0.6, 5.0), Vec3::NEG_Z).is_none());
        // Straight down onto the lid.
        let t = barrel_ray_hit(base, base + Vec3::Y * 5.0, Vec3::NEG_Y).unwrap();
        assert!((t - (5.0 - BARREL_HEIGHT)).abs() < 1e-4, "{t}");
    }

    #[test]
    fn hitscan_reaches_barrels_but_walls_and_nearer_targets_block() {
        let g = corridor();
        let origin = Vec3::new(6.0, 0.6, 6.0);
        let barrel = Vec3::new(18.0, 0.0, 6.0);
        let (d, t) = resolve_hitscan_full(&g, origin, Vec3::X, HITSCAN_RANGE, &[], &[], &[barrel]);
        assert_eq!(t, HitTarget::Barrel(0));
        assert!((d - (12.0 - BARREL_RADIUS)).abs() < 1e-3);
        // A jammer in front wins.
        let (_, t) = resolve_hitscan_full(&g, origin, Vec3::X, HITSCAN_RANGE, &[], &[Vec3::new(12.0, 0.0, 6.0)], &[barrel]);
        assert_eq!(t, HitTarget::Jammer(0));
        // Behind the wall at x=20..24 on row z=1 (cell (5,2)).
        let (_, t) = resolve_hitscan_full(&g, Vec3::new(6.0, 0.6, 10.0), Vec3::X, HITSCAN_RANGE, &[], &[], &[Vec3::new(30.0, 0.0, 10.0)]);
        assert_eq!(t, HitTarget::Wall);
    }

    #[test]
    fn barrel_damage_arms_instant_or_staggered_fuse() {
        let mut b = ExplosiveBarrel::default();
        // One shotgun blast (8 pellets) / two plasma bolts / one rocket / EMP.
        assert!(PELLET_DAMAGE * SHOTGUN_PELLETS as f32 >= b.hp);
        assert!(PLASMA_DAMAGE < b.hp && 2.0 * PLASMA_DAMAGE >= b.hp);
        assert!(crate::apps::EMP_DAMAGE >= b.hp, "EMP (rockets hit harder still)");
        assert!(!damage_barrel(&mut b, PLASMA_DAMAGE, false));
        assert!(b.is_intact() && b.hit_flash > 0.0);
        assert!(damage_barrel(&mut b, PLASMA_DAMAGE, false));
        assert_eq!(b.fuse, Some(0.0));
        assert!(!damage_barrel(&mut b, 50.0, false), "already armed");
        let mut c = ExplosiveBarrel::default();
        assert!(damage_barrel(&mut c, BARREL_DAMAGE, true));
        assert_eq!(c.fuse, Some(BARREL_CHAIN_FUSE));
        assert!(!damage_barrel(&mut ExplosiveBarrel { exploded: true, ..default() }, 100.0, false));
    }

    #[test]
    fn barrel_blast_falloff_and_forgiving_player_boost() {
        assert_eq!(barrel_blast_damage(0.0), BARREL_DAMAGE);
        assert!((barrel_blast_damage(BARREL_BLAST_RADIUS * 0.5) - BARREL_DAMAGE * 0.5).abs() < 1e-3);
        assert_eq!(barrel_blast_damage(BARREL_BLAST_RADIUS + 0.1), 0.0);
        let m = PlayerModifiers::default();
        let feet = Vec3::ZERO;
        let j = compute_barrel_jump(feet, Vec3::new(1.0, 0.6, 0.0), &m).unwrap();
        assert!(j.self_damage > 0.0 && j.self_damage <= BARREL_DAMAGE * BARREL_SELF_DAMAGE_FRAC);
        assert!(j.impulse.y > 8.0 && j.impulse.x < -5.0, "up and away: {:?}", j.impulse);
        // Barrel jumps launch harder than an equal rocket jump.
        let r = compute_rocket_jump(feet, Vec3::new(1.0, 0.6, 0.0), BARREL_BLAST_RADIUS, &m).unwrap();
        assert!(j.impulse.y > r.impulse.y);
        assert!(compute_barrel_jump(feet, Vec3::new(8.0, 0.6, 0.0), &m).is_none());
        // The rocket-jump refactor is unchanged.
        let rj = compute_rocket_jump(feet, Vec3::new(0.0, 0.0, 0.0), ROCKET_SPLASH_RADIUS, &m).unwrap();
        let f = splash_falloff(0.9, ROCKET_SPLASH_RADIUS);
        assert!((rj.self_damage - ROCKET_SPLASH_DAMAGE * f * ROCKET_SELF_DAMAGE_FRAC).abs() < 1e-4);
    }


    #[test]
    fn landing_dip_is_spring_damped_and_capped() {
        assert_eq!(landing_dip_impulse(1.0), 0.0, "tiny steps do not dip");
        assert_eq!(landing_dip_impulse(f32::NAN), 0.0);
        assert!(landing_dip_impulse(12.0) > landing_dip_impulse(6.0));
        assert_eq!(landing_dip_impulse(500.0), landing_dip_impulse(1000.0), "capped");
        for impact in [6.0f32, 8.0, 20.0, 80.0, 1e6] {
            let mut f = ViewmodelFeel { was_grounded: false, ..default() };
            f.update(false, -impact, 1.0 / 60.0);
            let mut lowest: f32 = 0.0;
            for i in 0..120 {
                f.update(true, 0.0, 1.0 / 60.0);
                assert!(f.dip.is_finite() && f.dip >= -LANDING_DIP_MAX - 1e-6 && f.dip <= LANDING_DIP_MAX * 0.4 + 1e-6, "{f:?}");
                lowest = lowest.min(f.dip);
                if i == 119 {
                    assert!(f.dip.abs() < 2e-3, "settles back: {f:?}");
                }
            }
            assert!(lowest < -0.004, "impact {impact} dipped only {lowest}");
        }
        // A bhop chain (one grounded frame per hop) stays bounded.
        let mut f = ViewmodelFeel::default();
        for hop in 0..60 {
            f.update(hop % 20 == 0, if hop % 20 == 0 { 0.0 } else { -9.0 }, 1.0 / 60.0);
            assert!(f.dip >= -LANDING_DIP_MAX - 1e-6);
        }
        assert_eq!(dip_spring_step(f32::NAN, f32::INFINITY, 1.0 / 60.0), (0.0, 0.0));
    }

    #[test]
    fn inspect_and_swap_cant_poses_are_bounded_and_rest_at_the_ends() {
        assert_eq!(inspect_pose(0.0), (Vec3::ZERO, Quat::IDENTITY));
        let (o, r) = inspect_pose(INSPECT_SECS);
        assert!(o.length() < 1e-4 && r.angle_between(Quat::IDENTITY) < 1e-3, "starts from rest");
        let (o, r) = inspect_pose(INSPECT_SECS * 0.5);
        assert!(r.angle_between(Quat::IDENTITY) > 0.8 && o.length() < 0.12, "showcases mid-way");
        for i in 0..=50 {
            let (o, r) = inspect_pose(INSPECT_SECS * i as f32 / 50.0);
            assert!(o.is_finite() && r.is_finite() && r.angle_between(Quat::IDENTITY) < 1.6);
        }
        assert_eq!(swap_cant(0.0, 1.0), 0.0);
        assert!(swap_cant(SWAP_SECS * 0.5, 1.0) > 0.3 && swap_cant(SWAP_SECS * 0.5, -1.0) < -0.3);
        assert!(swap_cant(SWAP_SECS, 1.0).abs() < 1e-5);
    }

    #[test]
    fn swapping_cancels_inspect_and_sets_cant_direction() {
        let mut ws = WeaponState { inspect_timer: INSPECT_SECS, ..default() };
        assert!(ws.switch_to(Weapon::Plasma));
        assert_eq!(ws.inspect_timer, 0.0);
        assert_eq!(ws.swap_dir, 1.0);
        ws.swap_timer = 0.0;
        assert!(ws.switch_to(Weapon::Shotgun));
        assert_eq!(ws.swap_dir, -1.0);
    }
}
