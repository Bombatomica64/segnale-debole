//! The demon archetypes: Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell
//! Baron, Phase Stalker, Catwalk Relay Sniper and the Carrier Daemon boss. Multi-part procedural meshes, flow-field chasing over the
//! `LevelGrid`, projectiles, Dead-Zone enrage, mobile jammer fields, hit-flash,
//! Ping marks and death gibs/drops.

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::math::Affine2;
use bevy::prelude::*;
use rand::Rng;
use rand::seq::SliceRandom;

use crate::audio::{SfxEvent, SfxQueue};
use crate::decals::{DecalKind, DecalQueue, DecalRequest};
use crate::fx::{needle_collar_transform, spawn_burst, spawn_death_gibs, spawn_flash_light, BurstColor, FxAssets, GibMat, HudFeed, ShotShape};
use crate::apps::spawn_hijack_hotspot;
use crate::level::{Cell, CullableLight, DemonJammer, LevelEntity, LevelGrid, RampDir, SignalTower, CEILING_HEIGHT};
use crate::mesh_batch::{MaterialBatches, MeshBatch};
use crate::physics::{count_wall_occlusions, has_line_of_sight, raycast_grid, resolve_player_movement, sample_floor_height, PLAYER_RADIUS};
use crate::player::{Player, PLAYER_EYE_HEIGHT};
use crate::sculpt;
use crate::apps::{DecoyHandset, PhoneApps, CALLBACK_SECS, DECOY_LURE_RADIUS, STORE_FORWARD_MAX_PACKETS};
use crate::signal::{active_jammer_fields_iter, bars_from_strength, compute_signal_at, JammerField, SignalState};
use crate::state::{GameSet, PlayerModifiers, RunStats};
use crate::weapons::{spawn_pickup, PickupAssets, PickupKind};

/// Mobile Dead Zone projected by every living Jammer Fiend.
pub const FIEND_FIELD_RADIUS: f32 = 13.0;
pub const FIEND_FIELD_STRENGTH: f32 = 2.5;
/// Static Enrage speed multiplier (Imps inside Dead Zones).
pub const ENRAGE_SPEED_MULT: f32 = 1.35;
pub const ENRAGE_FIRE_MULT: f32 = 0.65;
/// Demons never jump, so walkers always count as grounded for the physics
/// step: they stay glued to ramp descents / step-downs (only the player's
/// airborne snap-down is gated on `was_grounded`).
const WALKER_GLUE: bool = true;
/// Crisp, brief hit-flash so demons never look washed out.
pub const HIT_FLASH_SECS: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnemyKind {
    Imp,
    Cacodemon,
    JammerFiend,
    HellBaron,
    /// Floor 2+ flanker: blinks sideways through 0-1 bar Dead Zones.
    PhaseStalker,
    /// Floor 2+ catwalk sentinel: telegraphed rail lock-on.
    RelaySniper,
    /// Floor 5 / Endless boss-floor finale (see [`crate::boss`]).
    CarrierDaemon,
}

/// Number of demon kinds (size of every per-kind asset table).
pub const KIND_COUNT: usize = 7;

impl EnemyKind {
    pub const ALL: [EnemyKind; KIND_COUNT] = [
        EnemyKind::Imp,
        EnemyKind::Cacodemon,
        EnemyKind::JammerFiend,
        EnemyKind::HellBaron,
        EnemyKind::PhaseStalker,
        EnemyKind::RelaySniper,
        EnemyKind::CarrierDaemon,
    ];

    /// Dense index (`0..KIND_COUNT`) for per-kind asset tables.
    pub fn index(self) -> usize {
        match self {
            EnemyKind::Imp => 0,
            EnemyKind::Cacodemon => 1,
            EnemyKind::JammerFiend => 2,
            EnemyKind::HellBaron => 3,
            EnemyKind::PhaseStalker => 4,
            EnemyKind::RelaySniper => 5,
            EnemyKind::CarrierDaemon => 6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EnemyKind::Imp => "STATIC IMP",
            EnemyKind::Cacodemon => "DEAD-ZONE CACODEMON",
            EnemyKind::JammerFiend => "JAMMER FIEND",
            EnemyKind::HellBaron => "HELL BARON",
            EnemyKind::PhaseStalker => "PHASE STALKER",
            EnemyKind::RelaySniper => "RELAY SNIPER",
            EnemyKind::CarrierDaemon => "THE CARRIER DAEMON",
        }
    }

    pub fn flying(self) -> bool {
        matches!(self, EnemyKind::Cacodemon)
    }

    pub fn stats(self, floor: u32) -> EnemyStats {
        const HELL_BARON_MIN_COOLDOWN: f32 = 1.1;
        let f = floor.max(1) as f32;
        match self {
            EnemyKind::Imp => EnemyStats {
                hp: 50.0 + 6.0 * f,
                speed: 5.2,
                radius: 0.45,
                height: 1.9,
                eye_height: 1.65,
                fire_cooldown: 2.3,
                proj_speed: 15.0,
                proj_damage: 9.0,
                proj_radius: 0.28,
                range: 30.0,
                melee_damage: 10.0,
                hit_spheres: &[(0.55, 0.42), (1.15, 0.5), (1.62, 0.4)],
            },
            EnemyKind::Cacodemon => EnemyStats {
                hp: 110.0 + 12.0 * f,
                speed: 3.6,
                radius: 0.9,
                height: 1.8,
                eye_height: 0.1,
                fire_cooldown: 2.7,
                proj_speed: 12.0,
                proj_damage: 8.0,
                proj_radius: 0.32,
                range: 34.0,
                melee_damage: 12.0,
                hit_spheres: &[(0.0, 0.98)],
            },
            EnemyKind::JammerFiend => EnemyStats {
                hp: 150.0 + 15.0 * f,
                speed: 3.0,
                radius: 0.6,
                height: 2.4,
                eye_height: 2.05,
                fire_cooldown: 2.1,
                proj_speed: 18.0,
                proj_damage: 11.0,
                proj_radius: 0.26,
                range: 30.0,
                melee_damage: 14.0,
                hit_spheres: &[(0.6, 0.55), (1.4, 0.7), (2.05, 0.42)],
            },
            EnemyKind::HellBaron => EnemyStats {
                hp: 600.0 + 90.0 * f,
                speed: 4.4,
                radius: 0.95,
                height: 3.3,
                eye_height: 2.85,
                // Floored so deep Endless floors never reach a 0 s cooldown.
                fire_cooldown: (2.9 - 0.1 * f).max(HELL_BARON_MIN_COOLDOWN),
                proj_speed: 15.0,
                // Floor-scaled: a survivable Floor 1 boss, a brute deeper down.
                proj_damage: 11.0 + 1.0 * f,
                proj_radius: 0.4,
                range: 40.0,
                melee_damage: 20.0 + 2.0 * f,
                hit_spheres: &[(0.8, 0.8), (1.9, 0.95), (2.85, 0.6)],
            },
            EnemyKind::PhaseStalker => EnemyStats {
                hp: 85.0 + 10.0 * f,
                speed: 6.4,
                radius: 0.45,
                height: 2.1,
                eye_height: 1.8,
                fire_cooldown: 2.0,
                proj_speed: 20.0,
                proj_damage: 7.0 + 0.5 * f,
                proj_radius: 0.24,
                range: 30.0,
                melee_damage: 12.0,
                hit_spheres: &[(0.6, 0.42), (1.28, 0.50), (1.78, 0.42)],
            },
            EnemyKind::RelaySniper => EnemyStats {
                hp: 95.0 + 12.0 * f,
                speed: 2.6,
                radius: 0.55,
                height: 2.7,
                eye_height: 2.35,
                fire_cooldown: 3.1,
                proj_speed: 44.0,
                proj_damage: 18.0 + 1.0 * f,
                proj_radius: 0.2,
                range: 44.0,
                melee_damage: 10.0,
                hit_spheres: &[(0.7, 0.52), (1.65, 0.62), (2.35, 0.50)],
            },
            EnemyKind::CarrierDaemon => EnemyStats {
                hp: carrier_daemon_hp(floor),
                speed: 3.0,
                radius: 1.2,
                height: 4.4,
                eye_height: 3.75,
                // Boss attacks are pattern-driven (see `crate::boss`).
                fire_cooldown: 2.2,
                proj_speed: 17.0,
                proj_damage: 10.0 + 0.5 * f,
                proj_radius: 0.42,
                range: 46.0,
                melee_damage: 24.0 + 2.0 * f,
                hit_spheres: &[(0.9, 1.0), (2.1, 1.2), (3.45, 0.9)],
            },
        }
    }
}

/// Carrier Daemon HP: 1200 on Floor 5, +200 per floor beyond (Endless 10/15/20...).
pub fn carrier_daemon_hp(floor: u32) -> f32 {
    1200.0 + 200.0 * floor.saturating_sub(5) as f32
}

/// Phase Stalker shimmer telegraph before a Dead-Zone blink.
pub const PHASE_BLINK_TELEGRAPH: f32 = 0.35;
/// Horizontal blink distance range (m).
pub const PHASE_BLINK_MIN: f32 = 5.0;
pub const PHASE_BLINK_MAX: f32 = 8.0;
/// Relay Sniper lock-on telegraph (laser sight) before the rail shot.
pub const SNIPER_LOCK_SECS: f32 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnemyStats {
    pub hp: f32,
    pub speed: f32,
    pub radius: f32,
    pub height: f32,
    pub eye_height: f32,
    pub fire_cooldown: f32,
    pub proj_speed: f32,
    pub proj_damage: f32,
    pub proj_radius: f32,
    pub range: f32,
    pub melee_damage: f32,
    /// Hit volumes `(height above root, radius)` for ray tests.
    pub hit_spheres: &'static [(f32, f32)],
}

#[derive(Component, Debug, Clone)]
pub struct Enemy {
    pub kind: EnemyKind,
    pub stats: EnemyStats,
    pub hp: f32,
    pub max_hp: f32,
    pub hit_flash: f32,
    pub stun_timer: f32,
    pub attack_timer: f32,
    pub melee_timer: f32,
    pub velocity: Vec3,
    pub knockback: Vec3,
    pub enraged: bool,
    pub enrage_check: f32,
    pub alerted: bool,
    pub sees_player: bool,
    pub los_timer: f32,
    pub phase: f32,
    pub grounded: bool,
    /// Killed by the extraction beam: no drops.
    pub vaporized: bool,
    /// Move speed multiplier (Carrier Daemon phases).
    pub speed_mult: f32,
    /// Signal Shield: immune to every direct damage source.
    pub shielded: bool,
    /// Damage never takes HP below this (boss phase thresholds; 0 = off).
    pub hp_floor: f32,
    /// Hits absorbed by the Signal Shield (feedback counter).
    pub shield_hits: u32,
    /// Phase Stalker: seconds until the next Dead-Zone blink is allowed.
    pub blink_cooldown: f32,
    /// Phase Stalker: remaining shimmer telegraph (> 0 while charging a blink).
    pub blink_timer: f32,
    /// Phase Stalker: flank side of the next blink (+1 / -1).
    pub blink_side: f32,
    /// Relay Sniper: remaining lock-on telegraph (> 0 while the laser shows).
    pub sniper_lock_timer: f32,
    /// Relay Sniper: world point the laser sight is locked on.
    pub sniper_aim: Vec3,
    /// Attack pose envelope (1 on a shot / strike, decays to 0; purely visual).
    pub attack_anim: f32,
    /// The current attack pose is a forward melee / blink lunge (else a ranged recoil).
    pub attack_lunge: bool,
    /// Horizontal unit direction of the last directional hit (world XZ).
    pub stagger_dir: Vec2,
    /// Remaining hit-stagger (seconds; see [`stagger_secs`]).
    pub stagger_timer: f32,
    /// Walk-cycle phase (radians), advanced proportionally to ground speed.
    pub stride_phase: f32,
    /// How the killing blow landed (Milestone 30 fatalities; set by weapons).
    pub kill_cause: crate::fatality::KillCause,
    /// What last damaged this demon (Milestone 31 style freshness).
    pub style_source: crate::style::StyleSource,
}

impl Enemy {
    pub fn new(kind: EnemyKind, floor: u32) -> Self {
        let stats = kind.stats(floor);
        let mut rng = rand::rng();
        Enemy {
            kind,
            stats,
            hp: stats.hp,
            max_hp: stats.hp,
            hit_flash: 0.0,
            stun_timer: 0.0,
            attack_timer: rng.random_range(0.8..2.0),
            melee_timer: 0.0,
            velocity: Vec3::ZERO,
            knockback: Vec3::ZERO,
            enraged: false,
            enrage_check: rng.random_range(0.0..0.3),
            alerted: false,
            sees_player: false,
            los_timer: rng.random_range(0.0..0.25),
            phase: rng.random_range(0.0..10.0),
            grounded: false,
            vaporized: false,
            speed_mult: 1.0,
            shielded: false,
            hp_floor: 0.0,
            shield_hits: 0,
            blink_cooldown: rng.random_range(0.4..1.2),
            blink_timer: 0.0,
            blink_side: if rng.random::<bool>() { 1.0 } else { -1.0 },
            sniper_lock_timer: 0.0,
            sniper_aim: Vec3::ZERO,
            attack_anim: 0.0,
            attack_lunge: false,
            stagger_dir: Vec2::ZERO,
            stagger_timer: 0.0,
            stride_phase: rng.random_range(0.0..TAU),
            kill_cause: crate::fatality::KillCause::Generic,
            style_source: crate::style::StyleSource::Other,
        }
    }

    /// Record a directional hit (`dir` = travel direction of the hit / push,
    /// world space) for the visual hit-stagger. Ignores vertical-only hits.
    pub fn stagger(&mut self, dir: Vec3) {
        if !self.is_alive() {
            return;
        }
        if let Some(d) = Vec2::new(dir.x, dir.z).try_normalize() {
            self.stagger_dir = d;
            self.stagger_timer = stagger_secs(self.kind);
        }
    }

    /// Trigger the visual attack pose (ranged recoil, or forward `lunge`).
    pub fn trigger_attack_anim(&mut self, lunge: bool) {
        self.attack_anim = 1.0;
        self.attack_lunge = lunge;
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0.0
    }

    /// Current move speed including Static Enrage.
    pub fn move_speed(&self) -> f32 {
        self.stats.speed * self.speed_mult * if self.enraged { ENRAGE_SPEED_MULT } else { 1.0 }
    }

    pub fn fire_cooldown(&self) -> f32 {
        self.stats.fire_cooldown * if self.enraged { ENRAGE_FIRE_MULT } else { 1.0 }
    }

    /// Center of mass (for area effects).
    pub fn center(&self, root: Vec3) -> Vec3 {
        let mid = self.stats.hit_spheres[self.stats.hit_spheres.len() / 2].0;
        root + Vec3::Y * mid
    }

    /// Nearest ray hit distance against this enemy's hit spheres.
    pub fn ray_hit(&self, root: Vec3, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<f32> {
        self.stats
            .hit_spheres
            .iter()
            .filter_map(|&(h, r)| ray_sphere(origin, dir, root + Vec3::Y * h, r))
            .filter(|&t| t <= max_dist)
            .reduce(f32::min)
    }
}

/// Ray (normalized `dir`) vs sphere; distance to first intersection in front
/// of the origin (0 if the origin is inside).
pub fn ray_sphere(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = origin - center;
    let b = oc.dot(dir);
    let c = oc.length_squared() - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    if b > 0.0 {
        return None;
    }
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    Some(-b - disc.sqrt())
}

/// Apply player damage to a demon (Packet Sniffer bonus on pinged targets).
/// Returns the damage dealt.
pub fn damage_enemy(enemy: &mut Enemy, base: f32, marked: bool, mods: &PlayerModifiers) -> f32 {
    if !enemy.is_alive() || base <= 0.0 {
        return 0.0;
    }
    enemy.alerted = true;
    if enemy.shielded {
        // Signal Shield: full immunity (the boss system sparks + toasts).
        enemy.shield_hits += 1;
        return 0.0;
    }
    let dmg = if marked { base * (1.0 + mods.ping_mark_damage_bonus) } else { base };
    let before = enemy.hp;
    enemy.hp = if enemy.hp_floor > 0.0 { (enemy.hp - dmg).max(enemy.hp_floor.min(before)) } else { enemy.hp - dmg };
    enemy.hit_flash = HIT_FLASH_SECS;
    before - enemy.hp
}

/// Multiplier on demon damage dealt to the player: Faraday Case shields the
/// player while in a 0-1 bar Dead Zone.
pub fn incoming_damage_scale(player_in_dead_zone: bool, mods: &PlayerModifiers) -> f32 {
    if player_in_dead_zone { mods.dead_zone_damage_mult } else { 1.0 }
}

/// Static Enrage rule: Imps and Phase Stalkers inside a Dead Zone (0-1 bars
/// at their position).
pub fn should_enrage(kind: EnemyKind, in_dead_zone: bool) -> bool {
    reacts_to_dead_zones(kind) && in_dead_zone
}

/// Kinds that sample the signal field at their position (Static Enrage).
pub fn reacts_to_dead_zones(kind: EnemyKind) -> bool {
    matches!(kind, EnemyKind::Imp | EnemyKind::PhaseStalker)
}

/// Whether a demon or pylon could stand at `p` (feet): 9-point footprint
/// (center + 4 cardinals + 4 diagonals) is in bounds, clear of walls, hazard
/// pits and round pillars, and on a level surface.
pub fn standable(grid: &LevelGrid, p: Vec3, radius: f32) -> Option<f32> {
    let h = sample_floor_height(grid, p.x, p.z)?;
    let d = radius * std::f32::consts::FRAC_1_SQRT_2;
    for (dx, dz) in [
        (0.0, 0.0),
        (radius, 0.0),
        (-radius, 0.0),
        (0.0, radius),
        (0.0, -radius),
        (d, d),
        (d, -d),
        (-d, d),
        (-d, -d),
    ] {
        let c = grid.world_to_cell(Vec3::new(p.x + dx, 0.0, p.z + dz));
        if !grid.in_bounds(c) || matches!(grid.cell(c), Cell::Wall | Cell::HazardPit) {
            return None;
        }
        let hh = sample_floor_height(grid, p.x + dx, p.z + dz)?;
        if (hh - h).abs() > crate::level::STEP_HEIGHT {
            return None;
        }
    }
    let xz = Vec2::new(p.x, p.z);
    if grid.pillars.iter().zip(&grid.pillar_radii).any(|(pillar, pr)| xz.distance(*pillar) < pr + radius) {
        return None;
    }
    Some(h)
}

/// Phase Stalker blink destination: 5-8 m sideways (flanking, slightly
/// toward the player) on a validated walkable spot with line of sight from
/// the stalker - never into walls, hazard pits or out of bounds, and never on
/// top of the player. Tries `side_sign` first, then the mirrored flank.
pub fn pick_phase_blink_target(grid: &LevelGrid, enemy_pos: Vec3, player_pos: Vec3, side_sign: f32) -> Option<Vec3> {
    let to_p = Vec3::new(player_pos.x - enemy_pos.x, 0.0, player_pos.z - enemy_pos.z);
    let fwd = to_p.normalize_or(Vec3::NEG_Z);
    let side = if side_sign < 0.0 { -1.0 } else { 1.0 };
    let chest = enemy_pos + Vec3::Y * 1.2;
    for s in [side, -side] {
        let perp = Vec3::new(-fwd.z, 0.0, fwd.x) * s;
        for blend in [0.45f32, 0.25, 0.7, 0.0] {
            let dir = (perp + fwd * blend).normalize();
            for dist in [6.5f32, 7.5, 5.5, PHASE_BLINK_MAX, PHASE_BLINK_MIN] {
                let mut p = enemy_pos + dir * dist;
                let Some(h) = standable(grid, p, 0.55) else { continue };
                if (h - enemy_pos.y).abs() > 1.2 {
                    continue;
                }
                p.y = h + 0.05;
                if Vec2::new(p.x - player_pos.x, p.z - player_pos.z).length() < 2.5 {
                    continue;
                }
                if !has_line_of_sight(grid, chest, p + Vec3::Y * 1.2) {
                    continue;
                }
                return Some(p);
            }
        }
    }
    None
}

/// Relay Sniper aim at the end of a lock-on: the target plus half of its
/// horizontal travel during the rail bolt's flight (slight lead / snap).
pub fn sniper_lead_aim(from: Vec3, target: Vec3, target_vel: Vec3, proj_speed: f32) -> Vec3 {
    let flight = from.distance(target) / proj_speed.max(1.0);
    target + Vec3::new(target_vel.x, 0.0, target_vel.z) * flight * 0.5
}

/// Jammer fields projected by every living Jammer Fiend.
pub fn fiend_jammer_fields<'a>(enemies: impl Iterator<Item = (&'a Enemy, &'a GlobalTransform)>) -> Vec<JammerField> {
    fiend_jammer_fields_iter(enemies).collect()
}

/// Allocation-free form of [`fiend_jammer_fields`] (extend a reused buffer).
pub fn fiend_jammer_fields_iter<'a>(enemies: impl Iterator<Item = (&'a Enemy, &'a GlobalTransform)>) -> impl Iterator<Item = JammerField> {
    enemies
        .filter(|(e, _)| e.kind == EnemyKind::JammerFiend && e.is_alive())
        .map(|(_, t)| JammerField { pos: t.translation() + Vec3::Y * 1.6, radius: FIEND_FIELD_RADIUS, strength: FIEND_FIELD_STRENGTH })
}

// ---------------------------------------------------------------------------
// Procedural combat animation (pure pose math; applied to the `EnemyBody`
// visual pivot only - the root `Transform` and hit spheres never move).
// ---------------------------------------------------------------------------

/// Base hit-stagger duration (light demons).
pub const STAGGER_SECS: f32 = 0.18;
/// Attack pose duration (seconds for `attack_anim` to decay 1 -> 0).
pub const ATTACK_ANIM_SECS: f32 = 0.32;
/// Largest body pivot tilt any pose combination may produce (radians).
pub const MAX_BODY_TILT: f32 = 0.6;

/// How strongly a demon reacts to hits (1 = light; heavies stagger less).
pub fn stagger_weight(kind: EnemyKind) -> f32 {
    match kind {
        EnemyKind::HellBaron => 0.45,
        EnemyKind::CarrierDaemon => 0.25,
        EnemyKind::Cacodemon => 0.8,
        _ => 1.0,
    }
}

/// Hit-stagger duration of a demon kind (shorter for heavies).
pub fn stagger_secs(kind: EnemyKind) -> f32 {
    STAGGER_SECS * (0.6 + 0.4 * stagger_weight(kind))
}

/// Fast-in / slow-out envelope over `u` in `0..1` (peaks 1 at `attack`).
pub fn snap_envelope(u: f32, attack: f32) -> f32 {
    let u = if u.is_finite() { u.clamp(0.0, 1.0) } else { 1.0 };
    if u < attack {
        let k = u / attack;
        k * (2.0 - k)
    } else {
        let k = 1.0 - (u - attack) / (1.0 - attack);
        k * k * (3.0 - 2.0 * k)
    }
}

/// Visual body pivot pose: local offset + tilt (pitch about X, roll about Z,
/// in the demon's local frame; `-Z` is forward).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BodyPose {
    pub offset: Vec3,
    pub pitch: f32,
    pub roll: f32,
}

impl BodyPose {
    pub fn is_finite(&self) -> bool {
        self.offset.is_finite() && self.pitch.is_finite() && self.roll.is_finite()
    }

    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_x(self.pitch) * Quat::from_rotation_z(self.roll)
    }

    fn add(self, o: BodyPose) -> BodyPose {
        BodyPose { offset: self.offset + o.offset, pitch: self.pitch + o.pitch, roll: self.roll + o.roll }
    }

    /// Clamp to the bounded tilt / offset range.
    pub fn bounded(self) -> BodyPose {
        let f = |v: f32| if v.is_finite() { v.clamp(-MAX_BODY_TILT, MAX_BODY_TILT) } else { 0.0 };
        let o = if self.offset.is_finite() { self.offset.clamp(Vec3::splat(-0.4), Vec3::splat(0.4)) } else { Vec3::ZERO };
        BodyPose { offset: o, pitch: f(self.pitch), roll: f(self.roll) }
    }
}

/// Grounded walker stride: speed-proportional bob, hip sway and forward lean.
/// `local_vel` is the horizontal velocity in the demon's local frame.
pub fn stride_pose(local_vel: Vec2, max_speed: f32, stride_phase: f32) -> BodyPose {
    let speed = local_vel.length();
    let k = if max_speed > 0.01 && speed.is_finite() { (speed / max_speed).clamp(0.0, 1.3) } else { 0.0 };
    let s = stride_phase.sin();
    let idle = 1.0 - k.min(1.0);
    // Forward component (local -Z) leans the torso into the run.
    let fwd = if speed > 1e-3 { (-local_vel.y / speed).clamp(-1.0, 1.0) } else { 0.0 };
    BodyPose {
        offset: Vec3::new(s * 0.035 * k, s.abs() * 0.085 * k + (stride_phase * 0.25).sin() * 0.015 * idle, 0.0),
        pitch: -0.09 * k * fwd,
        roll: s * 0.065 * k,
    }
}

/// Cacodemon hover: smooth bob plus gentle bank / tilt into its horizontal velocity.
pub fn hover_pose(local_vel: Vec2, max_speed: f32, phase: f32) -> BodyPose {
    let m = max_speed.max(0.01);
    let v = if local_vel.is_finite() { local_vel / m } else { Vec2::ZERO };
    BodyPose {
        offset: Vec3::Y * ((phase * 2.0).sin() * 0.08),
        // Sideways drift (local +X) banks the top toward it (-roll); forward (-Z) dips the nose.
        pitch: (v.y * 0.16).clamp(-0.2, 0.2),
        roll: (-v.x * 0.28).clamp(-0.3, 0.3),
    }
}

/// Attack windup / recoil pose of `attack_anim` (1 -> 0): a ranged shot snaps
/// the torso back, a melee / blink lunge pitches it forward.
pub fn attack_pose(kind: EnemyKind, attack_anim: f32, lunge: bool) -> BodyPose {
    if attack_anim <= 0.0 || !attack_anim.is_finite() {
        return BodyPose::default();
    }
    let e = snap_envelope(1.0 - attack_anim.min(1.0), 0.18);
    let w = 0.5 + 0.5 * stagger_weight(kind);
    if lunge {
        BodyPose { offset: Vec3::new(0.0, -0.03 * e, -0.16 * e * w), pitch: -0.3 * e * w, roll: 0.0 }
    } else {
        BodyPose { offset: Vec3::new(0.0, 0.0, 0.07 * e * w), pitch: 0.2 * e * w, roll: 0.0 }
    }
}

/// Directional hit-stagger: tips the body top along the hit direction
/// (`local_dir` = hit travel direction in the demon's local XZ frame).
pub fn stagger_pose(kind: EnemyKind, local_dir: Vec2, timer: f32) -> BodyPose {
    let total = stagger_secs(kind);
    if timer <= 0.0 || !timer.is_finite() || !local_dir.is_finite() {
        return BodyPose::default();
    }
    let e = snap_envelope(1.0 - (timer / total).min(1.0), 0.22);
    let a = 0.32 * stagger_weight(kind) * e;
    let d = local_dir.normalize_or_zero();
    // Rx(+) tips +Y toward +Z; Rz(+) tips +Y toward -X.
    BodyPose { offset: Vec3::new(d.x, 0.0, d.y) * 0.05 * stagger_weight(kind) * e, pitch: a * d.y, roll: -a * d.x }
}

/// Full body pivot pose of a demon facing `root_rot`, before the stun
/// tremor / blink jitter overlays.
pub fn body_pose(e: &Enemy, root_rot: Quat) -> BodyPose {
    let inv = root_rot.inverse();
    let lv = inv * Vec3::new(e.velocity.x, 0.0, e.velocity.z);
    let local_vel = Vec2::new(lv.x, lv.z);
    let base = if e.kind.flying() { hover_pose(local_vel, e.move_speed(), e.phase) } else { stride_pose(local_vel, e.move_speed(), e.stride_phase) };
    let ld = inv * Vec3::new(e.stagger_dir.x, 0.0, e.stagger_dir.y);
    base.add(attack_pose(e.kind, e.attack_anim, e.attack_lunge))
        .add(stagger_pose(e.kind, Vec2::new(ld.x, ld.z), e.stagger_timer))
        .bounded()
}

/// Advance the purely visual animation clocks of one demon.
pub fn tick_enemy_anim(e: &mut Enemy, dt: f32) {
    let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.0 };
    e.attack_anim = (e.attack_anim - dt / ATTACK_ANIM_SECS).max(0.0);
    e.stagger_timer = (e.stagger_timer - dt).max(0.0);
    if !e.kind.flying() {
        // ~1 stride (two footfalls) per 1.6 m travelled, scaled by size.
        let speed = Vec2::new(e.velocity.x, e.velocity.z).length();
        let stride_len = 0.8 + e.stats.radius * 0.8;
        e.stride_phase = (e.stride_phase + speed / stride_len * PI * dt) % TAU;
    }
}

/// Pinged by the phone's Ping Scan: glows, shows through walls, takes Packet Sniffer bonus.
#[derive(Component, Debug, Clone)]
pub struct PingMarked {
    pub timer: f32,
    /// X-ray ring entity floating over the demon.
    pub ring: Option<Entity>,
}

/// Bobbing visual pivot (child of the enemy root).
#[derive(Component)]
pub struct EnemyBody;

/// The demon's tintable baked parts (their `MeshMaterial3d` handles are
/// swapped between the shared per-kind state materials) and its aura light.
#[derive(Component)]
pub struct EnemyLook {
    pub body_part: Entity,
    pub eye_part: Entity,
    pub aura: Entity,
}

#[derive(Component)]
pub struct EnemyAura;

/// Pulsing electric-violet static halo shown on Static-Enraged Imps so they pop
/// unmistakably against the red Mars bunker.
#[derive(Component)]
pub struct EnrageHalo;

/// Ground-level toxic-green interference ring attached to every Jammer Fiend.
#[derive(Component)]
pub struct FiendFieldRing;

/// Relay Sniper lock-on laser sight (child of the sniper root; shown while
/// `sniper_lock_timer > 0`, stretched from the scope to `sniper_aim`).
#[derive(Component)]
pub struct SniperLaser;

/// Link from a Relay Sniper root to its laser sight child.
#[derive(Component)]
pub struct SniperLaserRef(pub Entity);

#[derive(Component, Debug, Clone)]
pub struct EnemyProjectile {
    pub vel: Vec3,
    pub damage: f32,
    pub radius: f32,
    pub life: f32,
    pub color: BurstColor,
}

/// Body visual state bits (index into [`EnemyLookMaterials::body`]).
pub const LOOK_ENRAGED: usize = 1;
pub const LOOK_MARKED: usize = 2;
pub const LOOK_STUNNED: usize = 4;
/// Quantized hit-flash fade steps.
pub const FLASH_STEPS: usize = 3;

/// Shared, pre-allocated state materials of one demon kind. Demons swap
/// handles on state changes instead of owning (and mutating) materials; the
/// pulsing enraged / stunned variants are re-tinted at most once per frame.
#[derive(Clone)]
pub struct EnemyLookMaterials {
    pub body_emissive: LinearRgba,
    pub eye_emissive: LinearRgba,
    /// Indexed by `LOOK_ENRAGED | LOOK_MARKED | LOOK_STUNNED` bits.
    pub body: [Handle<StandardMaterial>; 8],
    /// Hit flash: `flash[state][step]` preserves full enraged/marked/stunned tint (step 0 = full flash).
    pub flash: [[Handle<StandardMaterial>; FLASH_STEPS]; 8],
    pub eye: Handle<StandardMaterial>,
    pub eye_enraged: Handle<StandardMaterial>,
}

/// Which tintable slot a baked demon part fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartRole {
    Static,
    Body,
    Eye,
}

/// One baked (per-material merged) rigid part of a demon model.
#[derive(Clone)]
pub struct EnemyPart {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
    pub role: PartRole,
}

/// Shared enemy meshes, materials and pre-baked models.
#[derive(Resource, Clone)]
pub struct EnemyAssets {
    pub cube: Handle<Mesh>,
    pub sphere: Handle<Mesh>,
    pub capsule: Handle<Mesh>,
    pub cone: Handle<Mesh>,
    pub cylinder: Handle<Mesh>,
    pub ring: Handle<Mesh>,
    pub bone: Handle<StandardMaterial>,
    /// Dark charred horn (Doom-style silhouettes against lit walls).
    pub horn: Handle<StandardMaterial>,
    pub dark: Handle<StandardMaterial>,
    pub metal_plate: Handle<StandardMaterial>,
    pub imp_dark: Handle<StandardMaterial>,
    /// Electric-cyan Cacodemon iris (lit, saturated glow).
    pub caco_iris: Handle<StandardMaterial>,
    pub caco_maw: Handle<StandardMaterial>,
    pub fiend_green: Handle<StandardMaterial>,
    /// Engraved molten-gold armor (Hell Baron, Carrier Daemon).
    pub baron_gold: Handle<StandardMaterial>,
    /// Saturated orange-gold hellfire (Hell Baron gauntlets, maw, buckle gem).
    pub baron_hellfire: Handle<StandardMaterial>,
    /// Black-and-amber industrial hazard stripes.
    pub hazard: Handle<StandardMaterial>,
    /// Dark engraved obsidian-iron (pylon shafts).
    pub obsidian: Handle<StandardMaterial>,
    /// Bright brushed titanium (Relay Sniper dish rig; static twin of its body).
    pub sniper_ti: Handle<StandardMaterial>,
    pub ping_ring: Handle<StandardMaterial>,
    pub enrage_halo: Handle<StandardMaterial>,
    pub fiend_ring: Handle<StandardMaterial>,
    /// Phase Stalker obsidian-violet plating + glowing magenta phase core.
    pub phase_obsidian: Handle<StandardMaterial>,
    pub phase_core: Handle<StandardMaterial>,
    /// Relay Sniper crimson scope lens / rail glow.
    pub sniper_lens: Handle<StandardMaterial>,
    /// Near-black (`#14161A`) armored dish liner / dark-metal tripod struts.
    pub sniper_liner: Handle<StandardMaterial>,
    /// Relay Sniper lock-on laser sight (additive crimson rod).
    pub sniper_laser: Handle<StandardMaterial>,
    /// Carrier Daemon pulsing crimson-violet Signal Core reactor.
    pub carrier_core: Handle<StandardMaterial>,
    /// Translucent cyan-violet Signal Shield shell (boss, while shielded).
    pub boss_shield: Handle<StandardMaterial>,
    /// Faceted geodesic shield shell + 2 tilted containment bands (unit radius).
    pub boss_shield_mesh: Handle<Mesh>,
    /// Cyan-violet tether beam from a Shield Pylon to the boss.
    pub boss_tether: Handle<StandardMaterial>,
    /// Shield Pylon glowing emitter.
    pub pylon_core: Handle<StandardMaterial>,
    /// Per-kind state materials (indexed by [`EnemyKind::index`]).
    pub looks: [EnemyLookMaterials; KIND_COUNT],
    /// Per-kind models: rigid sculpted parts merged per material (5-6 per demon).
    pub models: [Vec<EnemyPart>; KIND_COUNT],
    /// Per-kind articulated sub-pivots (Fiend emitter, Sniper dish, boss halo + cannons).
    pub rigs: [Vec<RigModel>; KIND_COUNT],
    /// Rigid parts each model was baked from (one entity each before batching).
    pub source_part_counts: [usize; KIND_COUNT],
    /// Pre-baked Shield Pylon model (all `PartRole::Static`).
    pub pylon_model: Vec<EnemyPart>,
    /// Pre-baked orbiting Shield Pylon ring rig.
    pub pylon_ring: RigModel,
}

impl EnemyAssets {
    pub fn look(&self, kind: EnemyKind) -> &EnemyLookMaterials {
        &self.looks[kind.index()]
    }
}

/// Body emissive of a demon in a steady (non-flash) visual state.
pub fn body_state_emissive(base: LinearRgba, state: usize, pulse: f32) -> LinearRgba {
    let mut body = base;
    if state & LOOK_ENRAGED != 0 {
        // High-contrast electric violet-magenta static aura (pops against red Mars walls!).
        body += LinearRgba::rgb(0.9, 0.08, 1.35) * (0.45 + 0.55 * pulse);
    }
    if state & LOOK_MARKED != 0 {
        body += LinearRgba::rgb(0.0, 0.18, 0.3);
    }
    if state & LOOK_STUNNED != 0 {
        body += LinearRgba::rgb(0.4, 1.2, 1.6) * pulse;
    }
    body
}

/// Body emissive at hit-flash fade `step` (0 = fresh hit), preserving the full
/// active state (enraged / marked / stunned) and its pulse underneath.
pub fn body_flash_emissive(base: LinearRgba, state: usize, pulse: f32, step: usize) -> LinearRgba {
    let k = 1.0 - step as f32 / FLASH_STEPS as f32;
    body_state_emissive(base, state, pulse) + LinearRgba::rgb(4.2, 3.4, 2.6) * k
}

/// Quantized flash step for a remaining `hit_flash` time.
pub fn flash_step(hit_flash: f32) -> usize {
    let k = (hit_flash / HIT_FLASH_SECS).clamp(0.0, 1.0);
    (((1.0 - k) * FLASH_STEPS as f32) as usize).min(FLASH_STEPS - 1)
}

/// Pulse shared by every tinted demon this frame.
pub fn look_pulse(t: f32) -> f32 {
    (t * 10.0).sin() * 0.5 + 0.5
}

// ---------------------------------------------------------------------------
// Materials, sculpted models and articulated rigs (all baked once at startup)
// ---------------------------------------------------------------------------

fn solid(r: f32, g: f32, b: f32, rough: f32, metal: f32) -> StandardMaterial {
    StandardMaterial { base_color: Color::srgb(r, g, b), perceptual_roughness: rough, metallic: metal, ..default() }
}

/// Lit, saturated glowing material: a deep `base` hue plus a channel-pure
/// `emissive`. (An `unlit` material outputs only its `base_color` and ignores
/// `emissive`, which is what turned every white-based glow into flat chalk.)
fn glow(base: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial { base_color: base, emissive, perceptual_roughness: 0.3, reflectance: 0.45, ..default() }
}

/// Tiny glowing eye / optic (Eye role): saturated hue, brighter emissive.
fn eye(base: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial { perceptual_roughness: 0.2, ..glow(base, emissive) }
}

/// Pixel-art texture (neutral-bright) multiplied by `tint`, tiled 2x over the sculpt UVs.
fn skinned(img: &Handle<Image>, tint: Color, rough: f32, metal: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: tint,
        base_color_texture: Some(img.clone()),
        perceptual_roughness: rough,
        metallic: metal,
        uv_transform: Affine2::from_scale(Vec2::splat(2.0)),
        ..default()
    }
}

/// Articulated sub-pivot kinds (animated in [`enemy_visuals`] and, for the
/// pylon ring, in [`crate::boss::carrier_daemon_system`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigKind {
    FiendEmitter,
    SniperDish,
    BossHalo,
    BossCannon,
    PylonRing,
}

/// An animated sub-pivot (child of a demon's [`EnemyBody`], or of a pylon).
#[derive(Component, Debug, Clone, Copy)]
pub struct RigPivot {
    pub kind: RigKind,
    pub rest: Transform,
    /// Accumulated spin (halo).
    pub angle: f32,
}

/// Jammer Fiend rotating transceiver ring + pulsing broadcast antenna.
#[derive(Component)]
pub struct FiendEmitterPivot;

/// Relay Sniper scanning parabolic targeting dish head.
#[derive(Component)]
pub struct SniperDishPivot;

/// A demon's articulated pivots (for the per-frame animation).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EnemyRig {
    pub pivots: [Option<Entity>; 2],
}

/// Pre-baked articulated sub-model. The pivot entity carries the first baked
/// part itself (saves an entity); any further materials are its children.
#[derive(Clone)]
pub struct RigModel {
    pub kind: RigKind,
    pub rest: Transform,
    pub parts: Vec<EnemyPart>,
}

/// Spawn a pre-baked rig under `parent` (no asset allocation).
pub fn spawn_rig(commands: &mut Commands, parent: Entity, rig: &RigModel) -> Entity {
    let mut ec = commands.spawn((ChildOf(parent), RigPivot { kind: rig.kind, rest: rig.rest, angle: 0.0 }, rig.rest, Visibility::default()));
    match rig.kind {
        RigKind::FiendEmitter => ec.insert(FiendEmitterPivot),
        RigKind::SniperDish => ec.insert(SniperDishPivot),
        RigKind::BossHalo => ec.insert(crate::boss::BossHaloPivot),
        RigKind::BossCannon => ec.insert(crate::boss::BossCannonPivot),
        RigKind::PylonRing => ec.insert(crate::boss::PylonRingPivot),
    };
    if let Some(first) = rig.parts.first() {
        ec.insert((Mesh3d(first.mesh.clone()), MeshMaterial3d(first.material.clone())));
    }
    let id = ec.id();
    for part in rig.parts.iter().skip(1) {
        commands.spawn((ChildOf(id), Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), Transform::IDENTITY));
    }
    id
}

type Mat = Handle<StandardMaterial>;

/// Place a part once (centered) ...
const MID: bool = false;
/// ... or on the right (+X) and mirrored onto the left.
const SYM: bool = true;

fn rx(a: f32) -> Quat {
    Quat::from_rotation_x(a)
}

fn ry(a: f32) -> Quat {
    Quat::from_rotation_y(a)
}

fn rz(a: f32) -> Quat {
    Quat::from_rotation_z(a)
}

fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// `tf` mirrored across the YZ plane (`M * tf` with `M = diag(-1, 1, 1)`):
/// the batcher flips the winding of the resulting negative-determinant transform.
fn mirror_x(tf: Transform) -> Transform {
    let q = tf.rotation;
    Transform {
        translation: tf.translation * v3(-1.0, 1.0, 1.0),
        rotation: Quat::from_xyzw(q.x, -q.y, -q.z, q.w),
        scale: tf.scale * v3(-1.0, 1.0, 1.0),
    }
}

/// Rotation taking local +Y to `up` and local -Z toward `toward`
/// (orthogonalized), so `curved_horn` bends and `curved_blade` edges curl
/// toward `toward`.
fn orient(up: Vec3, toward: Vec3) -> Quat {
    let y = up.normalize_or(Vec3::Y);
    let f = (toward - y * toward.dot(y)).try_normalize().unwrap_or_else(|| y.any_orthonormal_vector());
    let z = -f;
    Quat::from_mat3(&Mat3::from_cols(y.cross(z), y, z))
}

/// Rotation taking local +Y along `dir`.
fn along(dir: Vec3) -> Quat {
    Quat::from_rotation_arc(Vec3::Y, dir.normalize_or(Vec3::Y))
}

/// Smooth ellipsoid (muscle, eyeball, knob).
fn blob(r: Vec3) -> Mesh {
    sculpt::ridged_sphere(r, 8, 12, 0.0, 0.0, false)
}

/// Faceted glowing gem / node.
fn gem(r: f32) -> Mesh {
    sculpt::ridged_sphere(Vec3::splat(r), 6, 8, 0.1, 4.0, true)
}

fn bbox(x: f32, y: f32, z: f32, bevel: f32) -> Mesh {
    sculpt::beveled_box(v3(x, y, z), bevel)
}

/// Startup sculpting context: arbitrary meshes appended per material into
/// [`MaterialBatches`], counting every rigid source part.
#[derive(Default)]
struct Sculptor {
    batches: MaterialBatches,
    parts: usize,
}

impl Sculptor {
    fn tf(&mut self, mesh: &Mesh, mat: &Mat, tf: Transform, sym: bool) {
        self.batches.add(mesh, mat, tf);
        self.parts += 1;
        if sym {
            self.batches.add(mesh, mat, mirror_x(tf));
            self.parts += 1;
        }
    }

    fn at(&mut self, mesh: &Mesh, mat: &Mat, pos: Vec3, rot: Quat, sym: bool) {
        self.tf(mesh, mat, Transform::from_translation(pos).with_rotation(rot), sym);
    }

    fn scaled(&mut self, mesh: &Mesh, mat: &Mat, pos: Vec3, scale: Vec3, rot: Quat, sym: bool) {
        self.tf(mesh, mat, Transform::from_translation(pos).with_rotation(rot).with_scale(scale), sym);
    }

    /// Tapered limb / strut joining joint `a` (radius `r.0`) to joint `b`
    /// (radius `r.1`): faceted below 10 sides, smooth from 10 up.
    fn limb(&mut self, mat: &Mat, a: Vec3, b: Vec3, r: (f32, f32), sides: usize, sym: bool) {
        let d = b - a;
        let len = d.length();
        if len < 1e-3 {
            return;
        }
        let mesh = sculpt::tapered_prism(sides, r.0, r.1, len, sides < 10);
        self.at(&mesh, mat, (a + b) * 0.5, along(d), sym);
    }

    /// Curved horn / claw / fang rooted at `base`, growing along `up` and
    /// curling by `bend` toward `toward`.
    #[allow(clippy::too_many_arguments)]
    fn horn(&mut self, mat: &Mat, base: Vec3, up: Vec3, toward: Vec3, r: (f32, f32), len: f32, bend: f32, sym: bool) {
        let mesh = sculpt::curved_horn(r.0, r.1, len, 0.0, -bend, 8, 7);
        self.at(&mesh, mat, base, orient(up, toward), sym);
    }

    /// Two-tone sweeping horn rooted at `base`: a continuous cubic-Bezier spine
    /// `0 -> p[0] -> p[1] -> p[2]` (model-space offsets from `base`), split at
    /// `split` (de Casteljau) into a `mat` root and a `tip_mat` tip.
    #[allow(clippy::too_many_arguments)]
    fn sweep(&mut self, mat: &Mat, tip_mat: &Mat, base: Vec3, r: (f32, f32), p: [Vec3; 3], split: f32, sym: bool) {
        let [p1, p2, p3] = p;
        let id = Quat::IDENTITY;
        if split >= 1.0 {
            self.at(&sculpt::curved_horn_sweep(r.0, r.1, p1, p2, p3, 12, 9), mat, base, id, sym);
            return;
        }
        let t = split.clamp(0.05, 0.95);
        let (q0, q1, q2) = (Vec3::ZERO.lerp(p1, t), p1.lerp(p2, t), p2.lerp(p3, t));
        let (r0, r1) = (q0.lerp(q1, t), q1.lerp(q2, t));
        let m = r0.lerp(r1, t);
        let r_mid = r.0 * (1.0 - t).powf(0.85) + r.1 * t;
        self.at(&sculpt::curved_horn_sweep(r.0, r_mid, q0, r0, m, 9, 9), mat, base, id, sym);
        self.at(&sculpt::curved_horn_sweep(r_mid, r.1, r1 - m, q2 - m, p3 - m, 6, 9), tip_mat, base + m, id, sym);
    }

    /// Curved blade rooted at `base`, extending along `up`, edge curling toward `toward`.
    #[allow(clippy::too_many_arguments)]
    fn blade(&mut self, mat: &Mat, base: Vec3, up: Vec3, toward: Vec3, len: f32, width: f32, thick: f32, curve: f32, sym: bool) {
        let mesh = sculpt::curved_blade(len, width, thick, curve, 6);
        self.at(&mesh, mat, base, orient(up, toward), sym);
    }

    /// Bake one merged mesh per material (Body / Eye roles from `look`).
    fn bake(self, meshes: &mut Assets<Mesh>, look: Option<&EnemyLookMaterials>) -> Vec<EnemyPart> {
        self.batches
            .finish(meshes)
            .into_iter()
            .map(|(mesh, material)| {
                let role = match look {
                    Some(l) if material.id() == l.body[0].id() => PartRole::Body,
                    Some(l) if material.id() == l.eye.id() => PartRole::Eye,
                    _ => PartRole::Static,
                };
                EnemyPart { mesh, material, role }
            })
            .collect()
    }
}

/// One rig under construction: kind, rest pose (in body-pivot space), parts (in pivot space).
type RigBuild = (RigKind, Transform, Sculptor);

fn create_enemy_assets(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let hide = images.add(crate::assets_gen::demon_hide_texture().into_image());
    let gunmetal = images.add(crate::assets_gen::gunmetal_plate_texture().into_image());
    let hazard = images.add(crate::assets_gen::hazard_stripe_texture().into_image());
    let obsidian = images.add(crate::assets_gen::obsidian_tech_texture().into_image());
    let engraved = images.add(crate::assets_gen::engraved_gold_texture().into_image());

    let mut ping = crate::fx::additive_glow(Color::srgb(0.25, 1.0, 1.0), 0.92);
    ping.emissive = LinearRgba::rgb(1.5, 7.0, 9.0);
    let mut enrage_halo = crate::fx::additive_glow(Color::srgb(1.0, 0.22, 0.95), 0.85);
    enrage_halo.emissive = LinearRgba::rgb(6.0, 0.8, 7.5);
    let mut fiend_ring = crate::fx::additive_glow(Color::srgb(0.35, 1.0, 0.18), 0.72);
    fiend_ring.emissive = LinearRgba::rgb(1.0, 5.5, 0.4);
    let mut sniper_laser = crate::fx::additive_glow(Color::srgb(1.0, 0.12, 0.12), 0.85);
    sniper_laser.emissive = LinearRgba::rgb(14.0, 0.6, 0.5);
    // Crisp sci-fi containment barrier: lit additive so the faceted shell and its
    // bands shade, while the boss's sculpted details stay readable inside.
    let boss_shield = StandardMaterial {
        base_color: Color::srgba(0.18, 0.45, 1.0, 0.11),
        emissive: LinearRgba::rgb(0.28, 0.65, 1.8),
        perceptual_roughness: 0.2,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..default()
    };
    // Slim saturated electric-cobalt tether (no white-hot washout over the boss).
    let mut boss_tether = crate::fx::additive_glow(Color::srgb(0.15, 0.55, 1.0), 0.85);
    boss_tether.emissive = LinearRgba::rgb(0.25, 1.1, 2.8);

    // Per-kind body / eye templates -> shared state material tables.
    let titanium = StandardMaterial {
        emissive: LinearRgba::rgb(0.035, 0.038, 0.045),
        ..skinned(&gunmetal, Color::srgb(0.58, 0.62, 0.68), 0.34, 0.72)
    };
    let templates = [
        // Static Imp: charred demonic umber-crimson hide (separates from the red
        // Rust Bunker floor), crisp ember-orange eyes.
        (skinned(&hide, Color::srgb(0.24, 0.085, 0.05), 0.62, 0.05), eye(Color::srgb(1.0, 0.36, 0.02), LinearRgba::rgb(3.6, 0.75, 0.02))),
        // Cacodemon: ridged crimson carapace, deep emerald sclera.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.12, 0.0, 0.025), ..skinned(&hide, Color::srgb(0.72, 0.06, 0.13), 0.45, 0.05) },
            eye(Color::srgb(0.05, 0.55, 0.12), LinearRgba::rgb(0.12, 1.6, 0.18)),
        ),
        // Jammer Fiend: dark obsidian-tech chassis, toxic-green optics.
        (skinned(&obsidian, Color::srgb(0.24, 0.27, 0.28), 0.4, 0.65), eye(Color::srgb(0.3, 1.0, 0.1), LinearRgba::rgb(0.5, 5.0, 0.1))),
        // Hell Baron: muscular crimson-mahogany hide, molten eyes.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.03, 0.0, 0.0), ..skinned(&hide, Color::srgb(0.6, 0.13, 0.07), 0.62, 0.05) },
            eye(Color::srgb(1.0, 0.55, 0.05), LinearRgba::rgb(6.0, 2.2, 0.08)),
        ),
        // Phase Stalker: rich deep obsidian-violet hide, razor violet-magenta visor.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.05, 0.0, 0.09), ..skinned(&obsidian, Color::srgb(0.11, 0.03, 0.18), 0.35, 0.45) },
            eye(Color::srgb(0.62, 0.06, 0.85), LinearRgba::rgb(2.4, 0.1, 3.6)),
        ),
        // Relay Sniper: bright brushed titanium (pops on dark Floor 5 walls), deep laser-crimson optic.
        (titanium.clone(), eye(Color::srgb(0.68, 0.03, 0.02), LinearRgba::rgb(2.4, 0.06, 0.03))),
        // Carrier Daemon: blackened iron chassis, molten-gold eyes.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.08, 0.0, 0.03), ..skinned(&obsidian, Color::srgb(0.2, 0.11, 0.11), 0.45, 0.7) },
            eye(Color::srgb(1.0, 0.6, 0.08), LinearRgba::rgb(6.0, 2.6, 0.1)),
        ),
    ];
    let looks = templates.map(|(body, eye_tpl)| {
        let base = body.emissive;
        let with = |e: LinearRgba| StandardMaterial { emissive: e, ..body.clone() };
        EnemyLookMaterials {
            body_emissive: base,
            eye_emissive: eye_tpl.emissive,
            body: std::array::from_fn(|state| materials.add(with(body_state_emissive(base, state, 1.0)))),
            flash: std::array::from_fn(|state| {
                std::array::from_fn(|step| materials.add(with(body_flash_emissive(base, state, 1.0, step))))
            }),
            eye: materials.add(eye_tpl.clone()),
            eye_enraged: materials.add(StandardMaterial {
                base_color: Color::srgb(0.8, 0.12, 1.0),
                emissive: LinearRgba::rgb(4.0, 0.35, 6.0),
                ..eye_tpl
            }),
        }
    });

    // Faceted geodesic shield shell + 2 tilted equatorial containment bands (one mesh).
    let mut shell = MeshBatch::new();
    shell.append(&sculpt::ridged_sphere(Vec3::ONE, 8, 12, 0.03, 6.0, true), &Transform::IDENTITY);
    let band = sculpt::armored_ring(1.02, 0.025, 0.035, 24, true);
    shell.append(&band, &Transform::from_rotation(rx(0.35)));
    shell.append(&band, &Transform::from_rotation(rz(-0.35)));
    // Crisp equatorial orbital ring.
    shell.append(&sculpt::armored_ring(1.05, 0.035, 0.05, 32, true), &Transform::IDENTITY);
    let boss_shield_mesh = meshes.add(shell.build().expect("shield shell has triangles"));

    let mut a = EnemyAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        sphere: meshes.add(Sphere::new(1.0).mesh().uv(20, 14)),
        capsule: meshes.add(Capsule3d::new(0.5, 1.0)),
        cone: meshes.add(Cone::new(1.0, 1.0)),
        cylinder: meshes.add(Cylinder::new(1.0, 1.0)),
        ring: meshes.add(Torus::new(0.8, 1.0)),
        bone: materials.add(solid(0.78, 0.7, 0.54, 0.5, 0.0)),
        horn: materials.add(solid(0.11, 0.06, 0.05, 0.42, 0.15)),
        dark: materials.add(solid(0.035, 0.01, 0.012, 0.88, 0.05)),
        metal_plate: materials.add(skinned(&gunmetal, Color::srgb(0.46, 0.48, 0.52), 0.36, 0.8)),
        imp_dark: materials.add(skinned(&hide, Color::srgb(0.2, 0.05, 0.035), 0.8, 0.0)),
        caco_iris: materials.add(glow(Color::srgb(0.02, 0.42, 0.68), LinearRgba::rgb(0.04, 1.15, 1.75))),
        caco_maw: materials.add(glow(Color::srgb(0.14, 0.01, 0.03), LinearRgba::rgb(1.2, 0.03, 0.16))),
        fiend_green: materials.add(StandardMaterial { metallic: 0.2, ..glow(Color::srgb(0.08, 0.6, 0.04), LinearRgba::rgb(0.1, 1.9, 0.04)) }),
        baron_gold: materials.add(StandardMaterial {
            emissive: LinearRgba::rgb(0.45, 0.22, 0.02),
            ..skinned(&engraved, Color::srgb(1.0, 0.86, 0.62), 0.3, 0.85)
        }),
        baron_hellfire: materials.add(glow(Color::srgb(0.75, 0.14, 0.01), LinearRgba::rgb(2.1, 0.38, 0.02))),
        hazard: materials.add(skinned(&hazard, Color::WHITE, 0.55, 0.3)),
        obsidian: materials.add(skinned(&obsidian, Color::srgb(0.2, 0.19, 0.25), 0.4, 0.6)),
        sniper_ti: materials.add(titanium),
        ping_ring: materials.add(ping),
        enrage_halo: materials.add(enrage_halo),
        fiend_ring: materials.add(fiend_ring),
        // Polished metallic-obsidian armor plates (catch speculars against the dark violet hide).
        phase_obsidian: materials.add(StandardMaterial {
            emissive: LinearRgba::rgb(0.03, 0.0, 0.05),
            ..skinned(&obsidian, Color::srgb(0.3, 0.24, 0.38), 0.22, 0.88)
        }),
        phase_core: materials.add(glow(Color::srgb(0.42, 0.04, 0.58), LinearRgba::rgb(1.20, 0.06, 1.75))),
        sniper_lens: materials.add(glow(Color::srgb(0.68, 0.03, 0.02), LinearRgba::rgb(2.1, 0.05, 0.03))),
        sniper_liner: materials.add(solid(0.078, 0.086, 0.102, 0.38, 0.75)),
        sniper_laser: materials.add(sniper_laser),
        // Matte reactor surface: no white specular sheen from the boss's own aura light.
        carrier_core: materials.add(StandardMaterial {
            perceptual_roughness: 0.62,
            reflectance: 0.18,
            ..glow(Color::srgb(0.45, 0.02, 0.24), LinearRgba::rgb(1.45, 0.03, 0.72))
        }),
        boss_shield: materials.add(boss_shield),
        boss_tether: materials.add(boss_tether),
        pylon_core: materials.add(glow(Color::srgb(0.03, 0.22, 0.72), LinearRgba::rgb(0.05, 0.58, 1.85))),
        boss_shield_mesh,
        looks,
        models: Default::default(),
        rigs: Default::default(),
        source_part_counts: [0; KIND_COUNT],
        pylon_model: Vec::new(),
        pylon_ring: RigModel { kind: RigKind::PylonRing, rest: Transform::IDENTITY, parts: Vec::new() },
    };
    for kind in EnemyKind::ALL {
        let i = kind.index();
        let look = a.looks[i].clone();
        let (body, rigs) = sculpt_model(kind, &a, &look.body[0], &look.eye);
        a.source_part_counts[i] = body.parts + rigs.iter().map(|(_, _, s)| s.parts).sum::<usize>();
        a.models[i] = body.bake(&mut meshes, Some(&look));
        a.rigs[i] = rigs.into_iter().map(|(kind, rest, s)| RigModel { kind, rest, parts: s.bake(&mut meshes, Some(&look)) }).collect();
    }
    let (pylon, ring) = sculpt_pylon(&a);
    a.pylon_model = pylon.bake(&mut meshes, None);
    a.pylon_ring = RigModel { kind: ring.0, rest: ring.1, parts: ring.2.bake(&mut meshes, None) };
    commands.insert_resource(a);
}

/// Shield Pylon: armored relay obelisk + its orbiting ring rig.
fn sculpt_pylon(a: &EnemyAssets) -> (Sculptor, RigBuild) {
    let (mp, hz, ob, core, horn, bone) = (&a.metal_plate, &a.hazard, &a.obsidian, &a.pylon_core, &a.horn, &a.bone);
    let id = Quat::IDENTITY;
    let oct = ry(PI / 8.0);
    let diag = |k: usize| {
        let ang = k as f32 * FRAC_PI_2 + PI / 4.0;
        v3(ang.cos(), 0.0, ang.sin())
    };
    let mut p = Sculptor::default();
    // Multi-tiered octagonal base + hazard warning collar.
    p.at(&sculpt::tapered_prism(8, 0.86, 0.76, 0.22, true), mp, v3(0.0, 0.11, 0.0), oct, MID);
    p.at(&sculpt::tapered_prism(8, 0.66, 0.56, 0.2, true), ob, v3(0.0, 0.32, 0.0), oct, MID);
    p.at(&sculpt::tapered_prism(8, 0.5, 0.46, 0.12, true), hz, v3(0.0, 0.48, 0.0), oct, MID);
    // Four sloped armored buttress fins leaning into the shaft.
    let fin = sculpt::wedge_plate(0.14, 0.8, 0.5, 0.6, 0.35, -0.14);
    for k in 0..4 {
        let o = diag(k);
        p.at(&fin, mp, o * 0.52 + Vec3::Y * 0.62, orient(Vec3::Y, -o), MID);
    }
    // Tapered obsidian-iron obelisk: lower shaft, containment cage, upper shaft, emitter cap.
    p.at(&sculpt::tapered_prism(8, 0.32, 0.27, 0.52, true), ob, v3(0.0, 0.8, 0.0), oct, MID);
    let collar = sculpt::armored_ring(0.31, 0.06, 0.06, 8, true);
    p.at(&collar, mp, v3(0.0, 1.06, 0.0), oct, MID);
    p.at(&collar, mp, v3(0.0, 2.1, 0.0), oct, MID);
    // Thick beveled armor cage struts (wedged, wider at the collars) + mid clamp bands.
    let strut = sculpt::wedge_plate(0.13, 1.04, 0.1, 0.72, 0.8, 0.0);
    let clamp = bbox(0.17, 0.07, 0.13, 0.02);
    for k in 0..4 {
        let o = diag(k);
        p.at(&strut, ob, o * 0.3 + Vec3::Y * 1.58, orient(Vec3::Y, -o), MID);
        p.at(&clamp, mp, o * 0.31 + Vec3::Y * 1.58, orient(Vec3::Y, -o), MID);
    }
    // Deep electric-cobalt faceted crystal core inside the cage.
    p.at(&sculpt::ridged_sphere(v3(0.2, 0.5, 0.2), 6, 6, 0.0, 0.0, true), core, v3(0.0, 1.58, 0.0), id, MID);
    p.at(&sculpt::tapered_prism(8, 0.26, 0.2, 0.36, true), ob, v3(0.0, 2.28, 0.0), oct, MID);
    p.at(&sculpt::tapered_prism(8, 0.22, 0.1, 0.18, true), mp, v3(0.0, 2.55, 0.0), oct, MID);
    p.at(&gem(0.09), core, v3(0.0, 2.68, 0.0), id, MID);
    // Crown of four smoothly curved charred spikes with bone tips.
    for k in 0..4 {
        let o = diag(k);
        p.sweep(horn, bone, o * 0.16 + Vec3::Y * 2.56, (0.05, 0.006), [o * 0.12 + Vec3::Y * 0.12, o * 0.3 + Vec3::Y * 0.26, o * 0.24 + Vec3::Y * 0.48], 0.66, MID);
    }
    // Orbiting beveled ring (spun in the boss system).
    let mut ring = Sculptor::default();
    ring.at(&sculpt::armored_ring(0.52, 0.05, 0.07, 16, true), core, Vec3::ZERO, id, MID);
    (p, (RigKind::PylonRing, Transform::from_xyz(0.0, 2.55, 0.0), ring))
}

/// Sculpt one demon kind: the rigid body (baked under the body pivot) plus its
/// articulated rigs. Heights / widths follow [`EnemyStats::hit_spheres`];
/// forward is `-Z`, feet at `y = 0` (hover center for the Cacodemon).
fn sculpt_model(kind: EnemyKind, a: &EnemyAssets, body: &Mat, eye_m: &Mat) -> (Sculptor, Vec<RigBuild>) {
    let mut s = Sculptor::default();
    let mut rigs: Vec<RigBuild> = Vec::new();
    let id = Quat::IDENTITY;
    let y = Vec3::Y;
    match kind {
        EnemyKind::Imp => sculpt_imp(&mut s, a, body, eye_m),
        EnemyKind::Cacodemon => sculpt_cacodemon(&mut s, a, body, eye_m),
        EnemyKind::JammerFiend => {
            sculpt_fiend(&mut s, a, body, eye_m);
            // Rotating transceiver ring + glowing broadcast antenna (one entity, one material).
            let g = &a.fiend_green;
            let mut r = Sculptor::default();
            r.at(&sculpt::armored_ring(0.24, 0.035, 0.045, 16, true), g, v3(0.0, 0.34, 0.0), rx(0.28), MID);
            r.at(&sculpt::tapered_prism(6, 0.035, 0.018, 0.62, true), g, v3(0.0, 0.31, 0.0), id, MID);
            r.at(&sculpt::armored_ring(0.12, 0.02, 0.03, 10, true), g, v3(0.0, 0.52, 0.0), rx(-0.35), MID);
            r.at(&gem(0.075), g, v3(0.0, 0.66, 0.0), id, MID);
            rigs.push((RigKind::FiendEmitter, Transform::from_xyz(0.0, 1.99, 0.44), r));
        }
        EnemyKind::HellBaron => sculpt_baron(&mut s, a, body, eye_m),
        EnemyKind::PhaseStalker => sculpt_stalker(&mut s, a, body, eye_m),
        EnemyKind::RelaySniper => {
            sculpt_sniper(&mut s, a, body);
            // Parabolic relay targeting dish (opens toward -Z) on a scanning neck pivot.
            // Bright brushed-titanium shell + rim, near-black armored concave liner,
            // ONE large deep-crimson targeting optic dead centre.
            let (ti, liner) = (&a.sniper_ti, &a.sniper_liner);
            let face = rx(-FRAC_PI_2);
            let mut r = Sculptor::default();
            r.at(&sculpt::flared_dish(0.08, 0.36, 0.17, 0.03, 16), ti, v3(0.0, 0.16, 0.0), face, MID);
            r.at(&sculpt::flared_dish(0.07, 0.3, 0.125, 0.02, 16), liner, v3(0.0, 0.16, -0.032), face, MID);
            r.at(&sculpt::armored_ring(0.36, 0.026, 0.03, 16, true), ti, v3(0.0, 0.16, -0.17), rx(FRAC_PI_2), MID);
            r.blade(ti, v3(0.12, 0.24, 0.12), v3(0.35, 1.0, 0.45), v3(0.0, 0.0, 1.0), 0.3, 0.1, 0.025, 0.06, SYM);
            r.at(&bbox(0.22, 0.22, 0.18, 0.04), liner, v3(0.0, 0.16, 0.08), id, MID);
            r.at(&sculpt::tapered_prism(8, 0.06, 0.08, 0.08, true), liner, v3(0.0, 0.03, 0.02), id, MID);
            r.at(&sculpt::armored_ring(0.105, 0.022, 0.03, 12, true), ti, v3(0.0, 0.16, -0.07), rx(FRAC_PI_2), MID);
            r.at(&sculpt::ridged_sphere(v3(0.1, 0.1, 0.06), 6, 10, 0.04, 5.0, true), eye_m, v3(0.0, 0.16, -0.075), id, MID);
            r.at(&sculpt::tapered_prism(6, 0.024, 0.012, 0.2, true), liner, v3(0.0, 0.16, -0.2), face, MID);
            rigs.push((RigKind::SniperDish, Transform::from_xyz(0.0, 2.2, 0.0), r));
        }
        EnemyKind::CarrierDaemon => {
            sculpt_carrier(&mut s, a, body, eye_m);
            let (gold, core, dk) = (&a.baron_gold, &a.carrier_core, &a.dark);
            // Twin shoulder plasma cannons on one breathing / recoiling pivot.
            let mut c = Sculptor::default();
            c.at(&sculpt::wedge_plate(0.72, 0.34, 0.84, 0.62, 0.8, 0.0), gold, v3(1.08, 0.06, 0.0), rz(-0.32), SYM);
            let cooling = sculpt::armored_ring(0.2, 0.04, 0.05, 8, true);
            for z in [-0.5, -0.82] {
                c.at(&cooling, gold, v3(1.16, 0.3, z), rx(FRAC_PI_2), SYM);
            }
            c.at(&sculpt::armored_ring(0.15, 0.035, 0.06, 8, true), gold, v3(1.16, 0.3, -1.14), rx(FRAC_PI_2), SYM);
            c.at(&bbox(0.42, 0.38, 0.5, 0.07), dk, v3(1.16, 0.3, 0.12), id, SYM);
            c.at(&sculpt::tapered_prism(8, 0.17, 0.14, 1.1, true), dk, v3(1.16, 0.3, -0.62), rx(-FRAC_PI_2), SYM);
            c.at(&gem(0.12), core, v3(1.16, 0.3, -1.16), id, SYM);
            // Rolled slightly forward so the twin barrels aim at the player.
            rigs.push((RigKind::BossCannon, Transform::from_xyz(0.0, 3.42, -0.04).with_rotation(rx(-0.09)), c));
            // Multi-tiered octagonal broadcast halo behind the head (spins about its axis).
            let mut h = Sculptor::default();
            h.at(&sculpt::armored_ring(0.95, 0.07, 0.09, 8, true), gold, Vec3::ZERO, id, MID);
            h.at(&sculpt::armored_ring(0.68, 0.04, 0.06, 8, true), gold, Vec3::ZERO, ry(PI / 8.0), MID);
            let node = gem(0.1);
            for k in 0..6 {
                let ang = k as f32 * TAU / 6.0;
                let o = v3(ang.cos(), 0.0, -ang.sin());
                h.blade(gold, o * 0.98, o, y.cross(o), 0.48, 0.16, 0.05, 0.12, MID);
                h.limb(gold, o * 0.7, o * 0.92, (0.03, 0.03), 6, MID);
                let n = ang + PI / 6.0;
                h.at(&node, core, v3(n.cos(), 0.0, -n.sin()) * 0.82, id, MID);
            }
            rigs.push((RigKind::BossHalo, Transform::from_xyz(0.0, 4.0, 0.74).with_rotation(rx(FRAC_PI_2 - 0.3)), h));
        }
    }
    (s, rigs)
}

/// Hunched, sinewy predatory Mars demon (height ~1.9): torso pitched forward,
/// snarling snout thrust out, clawed arms held out and forward.
fn sculpt_imp(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (dark, horn, bone, maw) = (&a.imp_dark, &a.horn, &a.bone, &a.dark);
    let id = Quat::IDENTITY;
    let hunch = rx(-0.45);
    // Pelvis, forward-slanted abdomen, V-tapered ribcage + pecs + rib bands.
    s.at(&bbox(0.34, 0.2, 0.25, 0.05), dark, v3(0.0, 0.9, 0.05), rx(-0.15), MID);
    s.limb(body, v3(0.0, 0.95, 0.05), v3(0.0, 1.2, -0.05), (0.15, 0.19), 10, MID);
    s.at(&sculpt::wedge_plate(0.36, 0.42, 0.27, 1.55, 1.1, -0.08), body, v3(0.0, 1.3, -0.1), hunch, MID);
    s.at(&sculpt::angled_shield(0.44, 0.2, 0.07, 0.07), body, v3(0.0, 1.33, -0.27), hunch, MID);
    let rib = bbox(0.13, 0.025, 0.04, 0.008);
    for k in 0..3 {
        let kf = k as f32;
        s.at(&rib, dark, v3(0.13, 1.13 + kf * 0.07, -0.2 - kf * 0.04), ry(-0.45) * rz(-0.25) * hunch, SYM);
    }
    // Dorsal bone spine spikes along the hunched back.
    for k in 0..4 {
        let kf = k as f32;
        s.horn(bone, v3(0.0, 1.14 + kf * 0.11, 0.1 - kf * 0.06), v3(0.0, 0.7, 1.0), Vec3::Y, (0.038, 0.006), 0.18, 0.05, MID);
    }
    // Sloped predatory shoulders + arms held out and forward, spread bone talons.
    s.at(&sculpt::wedge_plate(0.2, 0.14, 0.24, 0.6, 0.8, 0.0), body, v3(0.26, 1.44, -0.2), rz(-0.4) * hunch, SYM);
    s.at(&blob(v3(0.1, 0.1, 0.11)), body, v3(0.31, 1.39, -0.2), id, SYM);
    let (sh, el, wr) = (v3(0.31, 1.38, -0.2), v3(0.47, 1.2, -0.34), v3(0.52, 1.12, -0.6));
    s.limb(body, sh, el, (0.078, 0.062), 10, SYM);
    s.at(&blob(Vec3::splat(0.058)), dark, el, id, SYM);
    s.horn(bone, el, v3(0.3, 0.1, 1.0), Vec3::Y, (0.026, 0.005), 0.14, 0.03, SYM);
    s.limb(dark, el, wr, (0.062, 0.05), 8, SYM);
    s.at(&bbox(0.12, 0.07, 0.12, 0.02), dark, wr + v3(0.0, -0.01, -0.04), rx(-0.2), SYM);
    let talon = sculpt::curved_horn(0.022, 0.004, 0.22, 0.0, -0.09, 8, 6);
    for c in [-0.05f32, -0.017, 0.017, 0.05] {
        s.at(&talon, bone, wr + v3(c, -0.02, -0.09), orient(v3(c * 5.0, -0.12, -1.0), Vec3::NEG_Y), SYM);
    }
    // Bent digitigrade legs: thick umber thighs, bone kneecaps, charred shins + hooves, bone toe talons.
    let (hip, knee, hock, foot) = (v3(0.15, 0.88, 0.05), v3(0.21, 0.56, -0.2), v3(0.21, 0.26, 0.12), v3(0.21, 0.07, -0.04));
    s.limb(body, hip, knee, (0.115, 0.08), 10, SYM);
    s.at(&blob(v3(0.12, 0.18, 0.13)), body, hip.lerp(knee, 0.42), along(knee - hip), SYM);
    s.at(&blob(Vec3::splat(0.07)), bone, knee + v3(0.0, 0.0, -0.03), id, SYM);
    s.limb(horn, knee, hock, (0.065, 0.05), 8, SYM);
    s.limb(horn, hock, foot, (0.05, 0.044), 8, SYM);
    s.at(&blob(Vec3::splat(0.05)), horn, hock, id, SYM);
    let hoof = sculpt::wedge_plate(0.07, 0.08, 0.16, 0.7, 0.75, -0.02);
    for dx in [-0.038f32, 0.038] {
        s.at(&hoof, horn, v3(0.21 + dx, 0.04, -0.08), id, SYM);
    }
    let toe = sculpt::curved_horn(0.018, 0.003, 0.11, 0.0, -0.04, 6, 6);
    for dx in [-0.045f32, 0.0, 0.045] {
        s.at(&toe, bone, v3(0.21 + dx, 0.035, -0.15), orient(v3(dx * 3.0, -0.05, -1.0), Vec3::NEG_Y), SYM);
    }
    // Snarling head thrust forward: dark brow, snout, open fanged jaw, ember eyes.
    s.limb(body, v3(0.0, 1.46, -0.14), v3(0.0, 1.55, -0.2), (0.08, 0.07), 10, MID);
    s.at(&sculpt::ridged_sphere(v3(0.14, 0.13, 0.16), 8, 10, 0.04, 5.0, true), body, v3(0.0, 1.6, -0.22), id, MID);
    s.at(&sculpt::angled_shield(0.3, 0.06, 0.07, 0.08), dark, v3(0.0, 1.66, -0.31), rx(-0.25), MID);
    s.at(&bbox(0.15, 0.085, 0.14, 0.025), body, v3(0.0, 1.565, -0.34), rx(0.12), MID);
    s.at(&bbox(0.13, 0.035, 0.1, 0.01), maw, v3(0.0, 1.515, -0.35), id, MID);
    s.at(&sculpt::wedge_plate(0.15, 0.05, 0.14, 0.75, 0.8, 0.0), dark, v3(0.0, 1.475, -0.33), rx(0.22), MID);
    s.horn(bone, v3(0.04, 1.535, -0.4), v3(0.0, -1.0, -0.15), Vec3::Z, (0.014, 0.002), 0.08, 0.012, SYM);
    s.horn(bone, v3(0.055, 1.49, -0.38), v3(0.1, 1.0, -0.2), Vec3::NEG_Z, (0.013, 0.002), 0.075, 0.015, SYM);
    s.at(&blob(v3(0.036, 0.02, 0.02)), eye_m, v3(0.062, 1.615, -0.355), rz(-0.3), SYM);
    // Short aggressive horns swept cleanly back and slightly outward (charred -> bone).
    s.sweep(horn, bone, v3(0.08, 1.69, -0.25), (0.05, 0.006), [v3(0.03, 0.09, 0.04), v3(0.08, 0.14, 0.17), v3(0.13, 0.12, 0.33)], 0.6, SYM);
}

/// Fierce ridged floating carapace with a cyclops eye and a fanged maw (radius ~0.9).
fn sculpt_cacodemon(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (maw, iris, horn, bone) = (&a.caco_maw, &a.caco_iris, &a.horn, &a.bone);
    let id = Quat::IDENTITY;
    s.at(&sculpt::ridged_sphere(v3(0.92, 0.86, 0.90), 12, 18, 0.085, 7.0, true), body, Vec3::ZERO, id, MID);
    // Heavy overhanging armored brow crest + ridged cheek jowls.
    s.at(&sculpt::angled_shield(1.04, 0.2, 0.2, 0.2), body, v3(0.0, 0.44, -0.7), rx(-0.5), MID);
    s.at(&sculpt::wedge_plate(0.9, 0.14, 0.34, 0.75, 0.6, 0.1), body, v3(0.0, 0.56, -0.56), rx(-0.35), MID);
    s.at(&sculpt::ridged_sphere(v3(0.24, 0.2, 0.3), 6, 8, 0.08, 4.0, true), body, v3(0.5, -0.18, -0.56), ry(-0.4), SYM);
    // Recessed dark socket, deep emerald sclera, electric-cyan iris, crisp dark slit pupil.
    s.at(&sculpt::ridged_sphere(v3(0.38, 0.34, 0.2), 8, 12, 0.05, 6.0, true), maw, v3(0.0, 0.12, -0.74), id, MID);
    s.at(&blob(v3(0.27, 0.26, 0.16)), eye_m, v3(0.0, 0.12, -0.84), id, MID);
    s.at(&blob(v3(0.15, 0.15, 0.07)), iris, v3(0.0, 0.12, -0.97), id, MID);
    s.at(&bbox(0.045, 0.22, 0.03, 0.008), horn, v3(0.0, 0.12, -1.04), id, MID);
    // Cavernous maw: dark glowing crimson throat, beveled gum ridges, huge interlocking curved fangs.
    s.at(&blob(v3(0.54, 0.22, 0.3)), maw, v3(0.0, -0.38, -0.64), id, MID);
    s.at(&sculpt::angled_shield(0.92, 0.07, 0.12, 0.12), body, v3(0.0, -0.24, -0.78), id, MID);
    s.at(&sculpt::angled_shield(0.86, 0.07, 0.12, 0.12), body, v3(0.0, -0.53, -0.72), id, MID);
    let (fang_big, fang_small) = (sculpt::curved_horn(0.08, 0.006, 0.38, 0.0, -0.07, 8, 7), sculpt::curved_horn(0.055, 0.005, 0.24, 0.0, -0.045, 8, 7));
    for k in 0..5 {
        let x = -0.32 + k as f32 * 0.16;
        let z = -0.84 - 0.12 * (1.0 - x.abs() / 0.46) + 0.03;
        s.at(if k % 2 == 0 { &fang_big } else { &fang_small }, bone, v3(x, -0.25, z), orient(v3(x * 0.25, -1.0, 0.08), Vec3::Z), MID);
    }
    let lower_big = sculpt::curved_horn(0.07, 0.006, 0.3, 0.0, -0.06, 8, 7);
    for k in 0..4 {
        let x = -0.24 + k as f32 * 0.16;
        let z = -0.78 - 0.12 * (1.0 - x.abs() / 0.43) + 0.03;
        s.at(if k % 3 == 0 { &fang_small } else { &lower_big }, bone, v3(x, -0.52, z), orient(v3(x * 0.25, 1.0, -0.05), Vec3::Z), MID);
    }
    // Sweeping crown of curved charred horns with bone tips: primary, dorsal, cheek, jowl + crest.
    s.sweep(horn, bone, v3(0.42, 0.64, -0.08), (0.13, 0.012), [v3(0.22, 0.2, 0.05), v3(0.52, 0.5, 0.02), v3(0.46, 0.82, -0.36)], 0.62, SYM);
    s.sweep(horn, bone, v3(0.3, 0.7, 0.32), (0.085, 0.01), [v3(0.1, 0.18, 0.1), v3(0.26, 0.38, 0.3), v3(0.2, 0.48, 0.56)], 0.6, SYM);
    s.sweep(horn, bone, v3(0.82, 0.05, -0.2), (0.09, 0.01), [v3(0.2, 0.04, 0.0), v3(0.42, 0.1, -0.04), v3(0.55, 0.34, -0.26)], 0.62, SYM);
    s.sweep(horn, bone, v3(0.62, -0.5, -0.25), (0.07, 0.008), [v3(0.12, -0.12, 0.0), v3(0.26, -0.3, -0.04), v3(0.2, -0.48, -0.3)], 0.62, SYM);
    for k in 0..3 {
        let kf = k as f32;
        s.horn(horn, v3(0.0, 0.84 - kf * 0.12, 0.12 + kf * 0.22), v3(0.0, 0.7, 0.7), Vec3::Y, (0.07, 0.01), 0.32 - kf * 0.04, 0.1, MID);
    }
    // Tapered fleshy underside tendrils.
    for k in 0..5 {
        let ang = -0.9 + k as f32 * 0.45;
        let (sx, cz) = (ang.sin(), ang.cos());
        s.horn(body, v3(sx * 0.38, -0.7, cz * 0.25 + 0.05), v3(sx * 0.3, -1.0, cz * 0.25 + 0.15), Vec3::Z, (0.065, 0.008), 0.45, 0.16, MID);
    }
}

/// Heavy cyber-armored industrial demon with a router backpack (height ~2.4).
fn sculpt_fiend(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (mp, g, hz, horn) = (&a.metal_plate, &a.fiend_green, &a.hazard, &a.horn);
    let id = Quat::IDENTITY;
    // Hips, hazard waist band, beveled torso.
    s.at(&bbox(0.6, 0.22, 0.42, 0.05), body, v3(0.0, 0.98, 0.0), id, MID);
    s.at(&bbox(0.66, 0.1, 0.46, 0.02), hz, v3(0.0, 1.12, 0.0), id, MID);
    s.at(&bbox(0.92, 0.66, 0.6, 0.09), body, v3(0.0, 1.5, 0.02), id, MID);
    // Chest armor flanking a recessed cage around the toxic-green reactor core.
    s.at(&bbox(0.3, 0.5, 0.1, 0.03), mp, v3(0.27, 1.53, -0.3), ry(-0.3), SYM);
    s.at(&sculpt::angled_shield(0.84, 0.16, 0.09, 0.1), mp, v3(0.0, 1.76, -0.3), id, MID);
    s.at(&sculpt::armored_ring(0.17, 0.045, 0.06, 12, true), mp, v3(0.0, 1.5, -0.33), rx(FRAC_PI_2), MID);
    s.at(&sculpt::ridged_sphere(Vec3::splat(0.14), 8, 10, 0.08, 5.0, true), g, v3(0.0, 1.5, -0.28), id, MID);
    let cage = bbox(0.025, 0.34, 0.03, 0.006);
    for x in [-0.06f32, 0.06] {
        s.at(&cage, mp, v3(x, 1.5, -0.45), id, MID);
    }
    // Sloped heavy pauldrons with hazard trim + glowing vents.
    s.at(&sculpt::wedge_plate(0.42, 0.26, 0.52, 0.55, 0.75, 0.0), mp, v3(0.6, 1.95, 0.0), rz(-0.42), SYM);
    s.at(&sculpt::wedge_plate(0.46, 0.05, 0.56, 0.92, 0.95, 0.0), hz, v3(0.57, 1.8, 0.0), rz(-0.42), SYM);
    s.at(&bbox(0.18, 0.05, 0.04, 0.01), g, v3(0.6, 1.9, -0.27), rz(-0.42), SYM);
    // Hydraulic armored arms with glowing energy knuckles.
    let (sh, el, wr) = (v3(0.62, 1.78, 0.0), v3(0.7, 1.36, -0.04), v3(0.72, 1.0, -0.16));
    s.limb(body, sh, el, (0.13, 0.11), 8, SYM);
    s.limb(mp, v3(0.76, 1.72, 0.1), v3(0.78, 1.4, 0.06), (0.035, 0.03), 6, SYM);
    s.at(&sculpt::armored_ring(0.11, 0.03, 0.05, 10, true), mp, el, along(sh - el), SYM);
    s.at(&bbox(0.24, 0.4, 0.26, 0.05), mp, (el + wr) * 0.5, along(el - wr), SYM);
    s.at(&bbox(0.26, 0.2, 0.26, 0.05), body, v3(0.72, 0.88, -0.2), rx(0.25), SYM);
    s.at(&bbox(0.22, 0.05, 0.06, 0.012), g, v3(0.72, 0.9, -0.33), rx(0.25), SYM);
    // Hydraulic piston legs + beveled stomper boots (hazard toe caps) at y = 0.
    let (hip, knee, ank) = (v3(0.27, 0.96, 0.0), v3(0.29, 0.56, -0.07), v3(0.29, 0.16, 0.02));
    s.limb(body, hip, knee, (0.17, 0.14), 8, SYM);
    s.at(&sculpt::wedge_plate(0.24, 0.26, 0.12, 0.7, 0.8, -0.03), mp, knee + v3(0.0, 0.02, -0.13), id, SYM);
    s.limb(mp, knee, ank, (0.12, 0.1), 8, SYM);
    s.limb(g, v3(0.29, 0.5, 0.1), v3(0.29, 0.2, 0.1), (0.025, 0.025), 6, SYM);
    s.at(&bbox(0.34, 0.16, 0.46, 0.04), body, v3(0.29, 0.08, -0.06), id, SYM);
    s.at(&bbox(0.35, 0.07, 0.1, 0.015), hz, v3(0.29, 0.1, -0.27), id, SYM);
    // Horned cyber-skull: DARK face plate + jaw grille so the twin green optics pop.
    s.limb(body, v3(0.0, 1.82, -0.02), v3(0.0, 1.95, -0.05), (0.1, 0.09), 8, MID);
    s.at(&bbox(0.36, 0.3, 0.36, 0.06), body, v3(0.0, 2.08, -0.05), id, MID);
    s.at(&bbox(0.3, 0.2, 0.04, 0.012), horn, v3(0.0, 2.06, -0.235), id, MID);
    s.at(&sculpt::angled_shield(0.4, 0.09, 0.06, 0.08), mp, v3(0.0, 2.17, -0.25), rx(-0.15), MID);
    s.at(&blob(v3(0.065, 0.03, 0.025)), eye_m, v3(0.08, 2.07, -0.26), rz(-0.25), SYM);
    s.at(&bbox(0.28, 0.12, 0.1, 0.02), horn, v3(0.0, 1.97, -0.22), id, MID);
    let slot = bbox(0.2, 0.016, 0.02, 0.004);
    for k in 0..3 {
        s.at(&slot, mp, v3(0.0, 1.94 + k as f32 * 0.03, -0.275), id, MID);
    }
    // Smoothly curved charred horn-antennas, tipped with glowing transmitter spines.
    s.sweep(horn, g, v3(0.13, 2.2, -0.06), (0.06, 0.008), [v3(0.12, 0.12, 0.05), v3(0.3, 0.24, 0.02), v3(0.36, 0.44, -0.2)], 0.8, SYM);
    // Demonic router backpack: server chassis, signal-bar LEDs, twin heat vents, power cables.
    s.at(&bbox(0.62, 0.66, 0.32, 0.05), mp, v3(0.0, 1.56, 0.44), id, MID);
    s.at(&bbox(0.5, 0.5, 0.04, 0.01), body, v3(0.0, 1.56, 0.61), id, MID);
    let led = bbox(0.36, 0.035, 0.03, 0.006);
    for k in 0..4 {
        s.at(&led, g, v3(0.0, 1.38 + k as f32 * 0.12, 0.635), id, MID);
    }
    s.at(&sculpt::tapered_prism(8, 0.09, 0.11, 0.24, true), mp, v3(0.2, 1.98, 0.5), id, SYM);
    s.at(&sculpt::armored_ring(0.09, 0.025, 0.03, 8, true), hz, v3(0.2, 2.09, 0.5), id, SYM);
    s.at(&sculpt::tapered_prism(8, 0.07, 0.07, 0.02, true), g, v3(0.2, 2.1, 0.5), id, SYM);
    s.limb(g, v3(0.22, 1.86, 0.32), v3(0.5, 1.98, 0.1), (0.03, 0.03), 6, SYM);
    s.at(&bbox(0.16, 0.1, 0.16, 0.03), mp, v3(0.0, 1.94, 0.44), id, MID);
}

/// Towering muscular bruiser in engraved molten-gold armor (height ~3.3).
fn sculpt_baron(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (gold, horn, bone, fire) = (&a.baron_gold, &a.horn, &a.bone, &a.baron_hellfire);
    let id = Quat::IDENTITY;
    // Charred waist, engraved belt with demonic buckle + hellfire gem.
    s.at(&bbox(0.86, 0.3, 0.6, 0.06), horn, v3(0.0, 1.42, 0.0), id, MID);
    s.at(&bbox(1.0, 0.15, 0.7, 0.04), gold, v3(0.0, 1.45, 0.0), id, MID);
    s.at(&sculpt::angled_shield(0.32, 0.26, 0.07, 0.08), gold, v3(0.0, 1.44, -0.38), id, MID);
    s.at(&gem(0.06), fire, v3(0.0, 1.44, -0.48), id, MID);
    // Massive V-tapered torso: abdomen, thorax, pecs, gold gorget + plackart, traps, spine spikes.
    s.scaled(&sculpt::tapered_prism(10, 0.36, 0.44, 0.42, false), body, v3(0.0, 1.76, 0.0), v3(1.0, 1.0, 0.78), id, MID);
    s.at(&sculpt::wedge_plate(0.92, 0.7, 0.6, 1.45, 1.05, -0.03), body, v3(0.0, 2.27, 0.0), rx(-0.06), MID);
    s.at(&blob(v3(0.28, 0.19, 0.14)), body, v3(0.23, 2.33, -0.27), id, SYM);
    s.at(&sculpt::angled_shield(0.92, 0.18, 0.08, 0.16), gold, v3(0.0, 2.55, -0.3), rx(-0.2), MID);
    s.at(&sculpt::angled_shield(0.66, 0.28, 0.06, 0.1), gold, v3(0.0, 2.02, -0.31), rx(0.08), MID);
    s.at(&sculpt::wedge_plate(0.62, 0.24, 0.44, 0.5, 0.7, 0.0), body, v3(0.0, 2.7, 0.03), id, MID);
    for k in 0..5 {
        let kf = k as f32;
        s.horn(horn, v3(0.0, 1.72 + kf * 0.2, 0.28), v3(0.0, 0.6, 1.0), Vec3::Y, (0.06, 0.01), 0.3 - kf * 0.02, 0.1, MID);
    }
    // Flared angular spiked molten-gold pauldrons.
    s.at(&sculpt::wedge_plate(0.6, 0.3, 0.72, 0.55, 0.8, 0.0), gold, v3(0.86, 2.6, 0.0), rz(-0.45), SYM);
    s.at(&sculpt::angled_shield(0.66, 0.24, 0.07, 0.12), gold, v3(1.02, 2.38, 0.0), orient(v3(0.4, 1.0, 0.0), v3(1.0, -0.3, 0.0)), SYM);
    for (dz, up, len) in [(-0.2f32, v3(0.3, 1.0, -0.25), 0.42f32), (0.0, v3(0.55, 0.9, 0.0), 0.55), (0.2, v3(0.3, 1.0, 0.25), 0.42)] {
        s.blade(gold, v3(0.9, 2.72, dz), up, Vec3::X, len, 0.14, 0.05, 0.12, SYM);
    }
    // Muscular arms, gold wrist bracers, blazing molten-ember gauntlets with splayed claws.
    let (sh, el, wr) = (v3(0.94, 2.32, 0.0), v3(1.02, 1.8, -0.02), v3(1.05, 1.38, -0.2));
    s.at(&blob(v3(0.24, 0.22, 0.25)), body, v3(0.92, 2.36, 0.0), id, SYM);
    s.limb(body, sh, el, (0.2, 0.16), 10, SYM);
    s.at(&blob(v3(0.16, 0.22, 0.16)), body, v3(0.99, 2.05, -0.07), rz(0.1), SYM);
    s.limb(body, el, wr, (0.16, 0.13), 10, SYM);
    s.at(&sculpt::armored_ring(0.15, 0.04, 0.09, 10, true), gold, wr.lerp(el, 0.18), along(el - wr), SYM);
    s.at(&bbox(0.3, 0.26, 0.3, 0.06), fire, v3(1.05, 1.22, -0.25), rx(0.25), SYM);
    let claw = sculpt::curved_horn(0.03, 0.006, 0.2, 0.0, -0.07, 8, 6);
    for c in [-0.09f32, -0.03, 0.03, 0.09] {
        s.at(&claw, horn, v3(1.05 + c, 1.12, -0.36), orient(v3(c * 2.0, -0.6, -1.0), v3(0.0, -1.0, 0.3)), SYM);
    }
    for (dx, dz) in [(-0.1f32, 0.05f32), (0.1, -0.05), (0.0, 0.1)] {
        s.blade(fire, v3(1.05 + dx, 1.3, -0.25 + dz), v3(dx * 2.0, 1.0, dz), Vec3::NEG_Z, 0.32, 0.1, 0.03, 0.08, SYM);
    }
    // Digitigrade satyr legs: thick haunches, shaggy charred shins, cloven hooves at y = 0.
    let (hip, knee, hock, foot) = (v3(0.38, 1.32, 0.0), v3(0.42, 0.84, -0.22), v3(0.42, 0.38, 0.14), v3(0.42, 0.12, -0.04));
    s.at(&blob(v3(0.28, 0.36, 0.32)), body, v3(0.38, 1.12, 0.02), id, SYM);
    s.limb(body, hip, knee, (0.25, 0.18), 10, SYM);
    s.at(&sculpt::ridged_sphere(v3(0.17, 0.22, 0.17), 6, 8, 0.18, 9.0, true), horn, v3(0.42, 0.66, -0.06), id, SYM);
    s.limb(horn, knee, hock, (0.15, 0.1), 8, SYM);
    s.limb(horn, hock, foot, (0.1, 0.09), 8, SYM);
    let hoof = sculpt::wedge_plate(0.12, 0.14, 0.28, 0.72, 0.78, -0.03);
    for dx in [-0.065f32, 0.065] {
        s.at(&hoof, bone, v3(0.42 + dx, 0.07, -0.1), id, SYM);
    }
    // Bull-demon skull: DARK face mask + muzzle so the two molten eyes and the angular
    // brow / jaw pop; glowing maw, gold fangs + tusks.
    s.limb(body, v3(0.0, 2.62, 0.0), v3(0.0, 2.82, -0.08), (0.2, 0.17), 10, MID);
    s.at(&sculpt::ridged_sphere(v3(0.3, 0.27, 0.32), 8, 12, 0.05, 5.0, true), body, v3(0.0, 2.95, -0.1), id, MID);
    s.at(&sculpt::angled_shield(0.44, 0.24, 0.08, 0.1), horn, v3(0.0, 2.9, -0.33), rx(0.05), MID);
    s.at(&sculpt::angled_shield(0.56, 0.1, 0.13, 0.13), body, v3(0.0, 3.05, -0.38), rx(-0.3), MID);
    s.at(&bbox(0.3, 0.18, 0.24, 0.05), horn, v3(0.0, 2.82, -0.36), rx(0.1), MID);
    s.at(&bbox(0.26, 0.04, 0.2, 0.01), fire, v3(0.0, 2.725, -0.38), id, MID);
    s.at(&sculpt::wedge_plate(0.32, 0.08, 0.26, 0.8, 0.8, 0.0), horn, v3(0.0, 2.68, -0.36), rx(0.12), MID);
    for x in [0.05f32, 0.11] {
        s.horn(gold, v3(x, 2.74, -0.47), v3(0.0, -1.0, -0.1), Vec3::Z, (0.02, 0.004), 0.1, 0.02, SYM);
    }
    s.horn(gold, v3(0.13, 2.7, -0.44), v3(0.3, 1.0, -0.25), Vec3::X, (0.03, 0.006), 0.18, 0.06, SYM);
    s.at(&blob(v3(0.055, 0.03, 0.03)), eye_m, v3(0.1, 2.95, -0.44), rz(-0.3), SYM);
    // Massive Doom bull horns: wide outward, then sweeping forward and up (charred -> bone-ivory).
    s.at(&sculpt::armored_ring(0.13, 0.035, 0.045, 8, true), gold, v3(0.3, 3.1, -0.08), along(v3(1.0, 0.1, 0.1)), SYM);
    s.sweep(horn, bone, v3(0.24, 3.1, -0.08), (0.14, 0.012), [v3(0.5, 0.02, 0.08), v3(0.98, 0.34, -0.04), v3(0.86, 0.8, -0.5)], 0.6, SYM);
}

/// Razor-sharp hunched obsidian-violet cyber-wraith (height ~2.1) with giant
/// raised crescent scythes splayed outward in a mantis slash posture.
fn sculpt_stalker(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (ob, core, horn) = (&a.phase_obsidian, &a.phase_core, &a.horn);
    let id = Quat::IDENTITY;
    let hunch = rx(-0.45);
    // Hip armor, forward-slanted spine, hunched thorax, rib plates, phase core in a containment ring.
    s.at(&bbox(0.3, 0.14, 0.2, 0.03), ob, v3(0.0, 0.98, 0.04), rx(-0.2), MID);
    s.at(&sculpt::wedge_plate(0.12, 0.2, 0.18, 0.6, 0.8, 0.0), ob, v3(0.17, 0.96, 0.0), rz(-0.35), SYM);
    s.limb(body, v3(0.0, 1.02, 0.04), v3(0.0, 1.24, -0.04), (0.1, 0.13), 6, MID);
    s.at(&sculpt::wedge_plate(0.3, 0.42, 0.24, 1.6, 1.1, -0.1), body, v3(0.0, 1.42, -0.08), hunch, MID);
    let strut = sculpt::wedge_plate(0.16, 0.03, 0.05, 0.5, 1.0, 0.0);
    for k in 0..3 {
        let kf = k as f32;
        s.at(&strut, ob, v3(0.11, 1.28 + kf * 0.09, -0.2 - kf * 0.045), ry(-0.5) * rz(-0.35), SYM);
    }
    s.at(&sculpt::armored_ring(0.1, 0.025, 0.035, 12, true), ob, v3(0.0, 1.4, -0.25), rx(FRAC_PI_2 - 0.45), MID);
    s.at(&gem(0.085), core, v3(0.0, 1.4, -0.24), id, MID);
    s.at(&sculpt::angled_shield(0.54, 0.12, 0.05, 0.12), ob, v3(0.0, 1.6, -0.2), rx(-0.5), MID);
    // Swept-back dorsal blade fins along the hunched spine.
    for k in 0..4 {
        let kf = k as f32;
        s.blade(ob, v3(0.0, 1.18 + kf * 0.12, 0.08 - kf * 0.07), v3(0.0, 0.6, 1.0), Vec3::Y, 0.32 + kf * 0.04, 0.12, 0.025, 0.08, MID);
    }
    // Bent reverse-jointed raptor legs with talon-blade feet at y = 0.
    let (hip, knee, hock, ank) = (v3(0.13, 0.96, 0.04), v3(0.18, 0.64, -0.22), v3(0.17, 0.3, 0.12), v3(0.17, 0.08, -0.02));
    s.limb(body, hip, knee, (0.08, 0.058), 6, SYM);
    s.horn(horn, knee, v3(0.1, 0.5, -1.0), Vec3::Y, (0.032, 0.005), 0.17, 0.05, SYM);
    s.limb(ob, knee, hock, (0.05, 0.04), 6, SYM);
    s.limb(ob, hock, ank, (0.04, 0.035), 6, SYM);
    for yaw in [-0.3f32, 0.3] {
        s.blade(ob, ank, v3(yaw.sin(), -0.12, -yaw.cos()), Vec3::NEG_Y, 0.24, 0.07, 0.03, 0.05, SYM);
    }
    s.blade(ob, ank, v3(0.0, -0.2, 1.0), Vec3::NEG_Y, 0.12, 0.05, 0.025, 0.03, SYM);
    // Angular shoulder plates, forearms RAISED, giant crescent scythes splayed ~45 deg
    // outward and hooking inward over the head, with a glowing plasma cutting edge.
    s.at(&sculpt::wedge_plate(0.18, 0.13, 0.22, 0.55, 0.8, 0.0), ob, v3(0.23, 1.62, -0.2), rz(-0.5), SYM);
    let (sh, el, wr) = (v3(0.25, 1.58, -0.2), v3(0.38, 1.36, -0.3), v3(0.46, 1.6, -0.42));
    s.limb(body, sh, el, (0.052, 0.042), 6, SYM);
    s.at(&blob(Vec3::splat(0.045)), ob, el, id, SYM);
    s.limb(ob, el, wr, (0.048, 0.036), 6, SYM);
    s.at(&bbox(0.08, 0.1, 0.09, 0.018), ob, wr, along(wr - el), SYM);
    let q = orient(v3(0.66, 0.72, -0.2), v3(-1.0, 0.25, -0.15));
    s.at(&sculpt::curved_blade(0.95, 0.24, 0.045, 0.36, 7), ob, wr, q, SYM);
    s.at(&sculpt::curved_blade(0.93, 0.27, 0.014, 0.36, 7), core, wr + q * v3(0.0, 0.0, -0.02), q, SYM);
    // Blade-crested skull pulled in over the chest, faceplate, V-angled glowing visor, swept antennae.
    s.limb(body, v3(0.0, 1.6, -0.1), v3(0.0, 1.72, -0.12), (0.05, 0.045), 6, MID);
    s.at(&sculpt::ridged_sphere(v3(0.11, 0.11, 0.16), 6, 10, 0.05, 5.0, true), body, v3(0.0, 1.79, -0.14), id, MID);
    s.at(&sculpt::angled_shield(0.22, 0.1, 0.04, 0.09), ob, v3(0.0, 1.77, -0.25), rx(0.1), MID);
    s.at(&bbox(0.08, 0.018, 0.02, 0.004), eye_m, v3(0.045, 1.805, -0.31), ry(-0.69) * rz(0.3), SYM);
    s.blade(ob, v3(0.0, 1.9, -0.2), v3(0.0, 0.35, 1.0), Vec3::Y, 0.5, 0.12, 0.03, 0.1, MID);
    s.horn(horn, v3(0.06, 1.88, -0.18), v3(0.3, 0.55, 0.8), Vec3::Y, (0.025, 0.004), 0.48, 0.12, SYM);
}

/// High-contrast titanium tripod sentinel with a twin-rail cannon (height ~2.7;
/// the dish head is a separate rig).
fn sculpt_sniper(s: &mut Sculptor, a: &EnemyAssets, body: &Mat) {
    let (mp, hz, lens, dk) = (&a.metal_plate, &a.hazard, &a.sniper_lens, &a.sniper_liner);
    let id = Quat::IDENTITY;
    // Turret hub + hazard collar.
    s.at(&sculpt::tapered_prism(8, 0.3, 0.24, 0.32, true), mp, v3(0.0, 1.22, 0.0), id, MID);
    s.at(&sculpt::armored_ring(0.3, 0.04, 0.05, 8, true), hz, v3(0.0, 1.1, 0.0), id, MID);
    // Three splayed dark-metal hydraulic legs with brushed knee joints: hub -> knee -> planted foot.
    let (actuator, knee_box, band, pad) = (
        bbox(0.14, 0.14, 0.16, 0.03),
        bbox(0.17, 0.17, 0.17, 0.04),
        sculpt::armored_ring(0.075, 0.02, 0.03, 8, true),
        bbox(0.26, 0.09, 0.32, 0.03),
    );
    for k in 0..3 {
        let ang = k as f32 * TAU / 3.0 + FRAC_PI_2;
        let o = v3(ang.cos(), 0.0, ang.sin());
        let (hip, knee, foot) = (o * 0.22 + Vec3::Y * 1.16, o * 0.62 + Vec3::Y * 0.68, o * 0.82 + Vec3::Y * 0.08);
        s.at(&actuator, mp, hip, orient(Vec3::Y, o), MID);
        s.limb(dk, hip, knee, (0.075, 0.062), 6, MID);
        s.at(&band, hz, hip.lerp(knee, 0.7), along(knee - hip), MID);
        s.limb(mp, o * 0.12 + Vec3::Y * 1.02, hip.lerp(knee, 0.8) - Vec3::Y * 0.06, (0.025, 0.022), 6, MID);
        s.at(&knee_box, mp, knee, orient(Vec3::Y, o), MID);
        s.limb(dk, knee, foot, (0.062, 0.05), 6, MID);
        s.at(&pad, dk, o * 0.84 + Vec3::Y * 0.045, orient(Vec3::Y, o), MID);
    }
    // Sentinel torso + angled front armor with saturated crimson vent slats + shoulder mounts.
    s.at(&sculpt::tapered_prism(8, 0.2, 0.26, 0.16, true), dk, v3(0.0, 1.42, 0.0), id, MID);
    s.at(&bbox(0.62, 0.5, 0.46, 0.06), body, v3(0.0, 1.72, 0.0), id, MID);
    s.at(&sculpt::angled_shield(0.6, 0.4, 0.07, 0.1), body, v3(0.0, 1.74, -0.27), id, MID);
    let slat = bbox(0.14, 0.03, 0.03, 0.005);
    for k in 0..3 {
        s.at(&slat, lens, v3(0.1, 1.64 + k as f32 * 0.075, -0.375), ry(-0.32), SYM);
    }
    s.at(&bbox(0.2, 0.22, 0.3, 0.04), mp, v3(0.4, 1.86, 0.0), id, SYM);
    s.at(&sculpt::wedge_plate(0.22, 0.06, 0.32, 0.8, 0.9, 0.0), hz, v3(0.4, 2.0, 0.0), id, SYM);
    s.limb(body, v3(0.42, 1.76, -0.04), v3(0.48, 1.48, -0.16), (0.06, 0.05), 6, SYM);
    s.limb(dk, v3(0.48, 1.48, -0.16), v3(0.44, 1.44, -0.42), (0.05, 0.045), 6, SYM);
    s.at(&bbox(0.1, 0.09, 0.12, 0.02), mp, v3(0.44, 1.44, -0.47), id, SYM);
    s.at(&sculpt::tapered_prism(6, 0.08, 0.06, 0.26, true), dk, v3(0.0, 2.08, 0.0), id, MID);
    // Bold shoulder-mounted twin-rail cannon (right): wide-set dark-steel rails, four
    // glowing crimson accelerator coils, muzzle brake; hazard counterweight (left).
    let (cx, cy) = (0.48, 2.16);
    s.at(&bbox(0.26, 0.24, 0.72, 0.05), mp, v3(cx, cy, -0.1), id, MID);
    s.at(&bbox(0.27, 0.25, 0.08, 0.02), hz, v3(cx, cy, 0.2), id, MID);
    s.at(&bbox(0.07, 0.07, 0.34, 0.012), mp, v3(cx, cy + 0.16, -0.12), id, MID);
    s.limb(dk, v3(0.4, 2.0, 0.0), v3(cx, cy - 0.06, -0.05), (0.06, 0.06), 6, MID);
    let rail = bbox(0.05, 0.06, 1.34, 0.01);
    for dx in [-0.075f32, 0.075] {
        s.at(&rail, dk, v3(cx + dx, cy, -1.08), id, MID);
    }
    let coil = sculpt::armored_ring(0.12, 0.026, 0.04, 8, true);
    for k in 0..4 {
        s.at(&coil, lens, v3(cx, cy, -0.52 - k as f32 * 0.3), rx(FRAC_PI_2), MID);
    }
    s.at(&bbox(0.26, 0.15, 0.1, 0.025), dk, v3(cx, cy, -1.76), id, MID);
    s.at(&bbox(0.16, 0.16, 0.24, 0.03), hz, v3(-cx + 0.04, 2.1, 0.08), id, MID);
}

/// Doom-industrial broadcast colossus (height ~4.4; halo + cannons are rigs):
/// long grounded legs, wedged gorget + gold ribcage around a recessed reactor,
/// an enlarged forward-thrust dread-skull crowned by thick ram horns.
fn sculpt_carrier(s: &mut Sculptor, a: &EnemyAssets, body: &Mat, eye_m: &Mat) {
    let (gold, core, mp, horn) = (&a.baron_gold, &a.carrier_core, &a.metal_plate, &a.horn);
    let id = Quat::IDENTITY;
    // Pelvis + gold belt plate + abdomen.
    s.at(&bbox(1.4, 0.42, 0.9, 0.08), body, v3(0.0, 1.95, 0.0), id, MID);
    s.at(&sculpt::angled_shield(1.0, 0.28, 0.08, 0.12), gold, v3(0.0, 1.95, -0.48), id, MID);
    s.scaled(&sculpt::tapered_prism(8, 0.58, 0.72, 0.38, true), body, v3(0.0, 2.3, 0.0), v3(1.0, 1.0, 0.72), id, MID);
    // Wedged V-thorax (broad at the shoulders) + back hump.
    s.at(&sculpt::wedge_plate(1.5, 1.04, 1.04, 1.34, 1.0, -0.04), body, v3(0.0, 2.88, 0.08), id, MID);
    s.at(&sculpt::ridged_sphere(v3(0.9, 0.55, 0.5), 8, 12, 0.06, 5.0, true), body, v3(0.0, 3.2, 0.48), id, MID);
    // Sculpted layered gorget: heavy upper plate, gold lip, lower gold band, sternum trim.
    s.at(&sculpt::angled_shield(1.62, 0.3, 0.14, 0.24), body, v3(0.0, 3.27, -0.46), rx(-0.2), MID);
    s.at(&sculpt::angled_shield(1.68, 0.07, 0.15, 0.24), gold, v3(0.0, 3.45, -0.5), rx(-0.2), MID);
    s.at(&sculpt::angled_shield(1.34, 0.08, 0.12, 0.2), gold, v3(0.0, 3.13, -0.53), rx(-0.15), MID);
    s.at(&sculpt::angled_shield(1.2, 0.07, 0.1, 0.16), gold, v3(0.0, 2.3, -0.5), id, MID);
    // Recessed deep magenta-crimson Signal Core: dark iron iris baffles, gold containment
    // ring, dark outer ring, overlapping curved gold ribcage.
    let ch = 2.74;
    s.at(&sculpt::ridged_sphere(Vec3::splat(0.36), 10, 14, 0.08, 6.0, true), core, v3(0.0, ch, -0.44), id, MID);
    s.at(&sculpt::armored_ring(0.64, 0.05, 0.08, 16, true), body, v3(0.0, ch, -0.5), rx(FRAC_PI_2), MID);
    s.at(&sculpt::armored_ring(0.52, 0.06, 0.09, 16, true), gold, v3(0.0, ch, -0.6), rx(FRAC_PI_2), MID);
    let baffle = sculpt::wedge_plate(0.17, 0.2, 0.05, 0.45, 1.0, 0.0);
    for k in 0..8 {
        let ang = k as f32 * TAU / 8.0 + PI / 8.0;
        let o = v3(ang.cos(), ang.sin(), 0.0);
        s.at(&baffle, horn, v3(0.0, ch, -0.62) + o * 0.41, orient(-o + Vec3::Z * 0.45, Vec3::NEG_Z), MID);
    }
    for ry_ in [2.28f32, 2.5, 2.98, 3.18] {
        let tilt = (ch - ry_) * 0.5;
        s.blade(gold, v3(0.84, ry_, -0.3), v3(-0.55, tilt, -1.0), Vec3::NEG_X, 0.6, 0.2, 0.05, 0.15, SYM);
    }
    // Long armored dread-mech legs in a wide grounded stance: gold knee guards,
    // hydraulic shins + pistons, clawed war-boots at y = 0.
    let (hip, knee, ank) = (v3(0.74, 1.85, 0.02), v3(0.8, 1.15, -0.2), v3(0.82, 0.36, 0.0));
    s.limb(body, hip, knee, (0.36, 0.28), 8, SYM);
    s.at(&sculpt::wedge_plate(0.46, 0.42, 0.22, 0.7, 0.8, -0.05), gold, v3(0.8, 1.17, -0.44), id, SYM);
    s.limb(mp, knee, ank, (0.25, 0.21), 8, SYM);
    s.limb(mp, v3(0.8, 1.05, 0.2), v3(0.82, 0.42, 0.18), (0.055, 0.055), 6, SYM);
    s.at(&bbox(0.66, 0.3, 0.88, 0.06), body, v3(0.82, 0.15, -0.12), id, SYM);
    s.at(&bbox(0.68, 0.08, 0.2, 0.02), gold, v3(0.82, 0.28, -0.48), id, SYM);
    let talon = sculpt::curved_horn(0.065, 0.01, 0.26, 0.0, -0.08, 8, 6);
    for c in [-0.2f32, 0.0, 0.2] {
        s.at(&talon, horn, v3(0.82 + c, 0.15, -0.54), orient(v3(0.0, -0.12, -1.0), Vec3::NEG_Y), SYM);
    }
    // Hunched armored arms: pauldrons, biceps, gold elbow rings + cuffs, forward-curled talon fists.
    let (sh, el, wr) = (v3(1.18, 3.2, 0.0), v3(1.4, 2.55, 0.08), v3(1.44, 2.02, -0.42));
    s.at(&sculpt::ridged_sphere(v3(0.42, 0.38, 0.44), 8, 10, 0.05, 5.0, true), body, v3(1.16, 3.24, 0.0), id, SYM);
    s.limb(body, sh, el, (0.3, 0.24), 10, SYM);
    s.at(&blob(v3(0.22, 0.3, 0.22)), body, v3(1.3, 2.85, -0.04), id, SYM);
    s.at(&sculpt::armored_ring(0.24, 0.05, 0.07, 10, true), gold, el, along(sh - el), SYM);
    s.limb(mp, el, wr, (0.25, 0.22), 8, SYM);
    s.at(&bbox(0.48, 0.44, 0.5, 0.08), body, v3(1.44, 1.86, -0.52), rx(0.35), SYM);
    s.at(&sculpt::armored_ring(0.27, 0.05, 0.12, 10, true), gold, wr, along(el - wr), SYM);
    let finger = sculpt::curved_horn(0.058, 0.01, 0.38, 0.0, -0.16, 8, 6);
    for c in [-0.15f32, -0.05, 0.05, 0.15] {
        s.at(&finger, horn, v3(1.44 + c, 1.76, -0.74), orient(v3(0.0, -0.45, -1.0), v3(0.0, -1.0, 0.6)), SYM);
    }
    // Enlarged forward-thrust dread-skull: neck block, DARK iron face plate, gold brow,
    // blazing molten eyes + maw slit, angular jaw, gold mandible tusks, rear crest.
    s.at(&bbox(0.56, 0.3, 0.52, 0.06), body, v3(0.0, 3.5, -0.1), id, MID);
    s.at(&sculpt::ridged_sphere(v3(0.5, 0.45, 0.48), 8, 12, 0.06, 5.0, true), body, v3(0.0, 3.82, -0.2), id, MID);
    s.at(&sculpt::angled_shield(0.66, 0.36, 0.1, 0.1), horn, v3(0.0, 3.78, -0.6), rx(0.05), MID);
    s.at(&sculpt::angled_shield(0.84, 0.14, 0.13, 0.1), gold, v3(0.0, 3.98, -0.58), rx(-0.25), MID);
    s.at(&blob(v3(0.1, 0.05, 0.035)), eye_m, v3(0.17, 3.86, -0.72), rz(-0.25), SYM);
    s.at(&bbox(0.3, 0.04, 0.04, 0.01), eye_m, v3(0.0, 3.64, -0.72), id, MID);
    s.at(&sculpt::wedge_plate(0.52, 0.14, 0.34, 0.8, 0.8, 0.0), horn, v3(0.0, 3.52, -0.5), rx(0.1), MID);
    s.horn(gold, v3(0.22, 3.52, -0.6), v3(0.2, -0.9, -0.5), Vec3::NEG_X, (0.06, 0.01), 0.42, 0.14, SYM);
    s.horn(horn, v3(0.14, 4.18, -0.02), v3(0.25, 0.9, 0.5), Vec3::Z, (0.08, 0.015), 0.46, 0.15, SYM);
    // Thick heavy ram/bull curl: out from the temples, back and down, then curling
    // forward and up around the skull, ringed by gold bands.
    let (h0, rr) = (v3(0.42, 3.92, -0.18), (0.32f32, 0.03f32));
    let hp = [v3(0.45, 0.35, 0.25), v3(1.2, 0.05, 0.35), v3(0.95, 0.5, -0.55)];
    s.sweep(horn, horn, h0, rr, hp, 1.0, SYM);
    let bez = |t: f32| {
        let u = 1.0 - t;
        hp[0] * (3.0 * u * u * t) + hp[1] * (3.0 * u * t * t) + hp[2] * (t * t * t)
    };
    for t in [0.14f32, 0.34] {
        let r = rr.0 * (1.0 - t).powf(0.85) + rr.1 * t;
        let dir = bez(t + 0.02) - bez(t - 0.02);
        s.at(&sculpt::armored_ring(r + 0.015, 0.035, 0.05, 10, true), gold, h0 + bez(t), along(dir), SYM);
    }
}

/// Farthest the player can be seen (line of sight still required).
pub const SIGHT_MAX_DIST: f32 = 45.0;
/// An unalerted demon that sees the player within this range wakes up
/// (Milestone 28: 36 m covers the 32 m straight corridors, so nobody idles
/// in plain sight down a long lane; wall-screened demons stay asleep).
pub const SIGHT_AGGRO_DIST: f32 = 36.0;
/// Close enough to hear the player through walls.
pub const PROXIMITY_AGGRO_DIST: f32 = 8.0;

/// Whether an unalerted demon wakes this sense tick.
pub fn wakes_on_senses(sees_player: bool, dist: f32) -> bool {
    (sees_player && dist < SIGHT_AGGRO_DIST) || dist < PROXIMITY_AGGRO_DIST
}

/// Spawn one demon. `pos` is the feet position (hover center for Cacodemons).
/// Allocates no materials or meshes: the model and its state materials are
/// pre-baked in [`EnemyAssets`].
pub fn spawn_enemy(
    commands: &mut Commands,
    a: &EnemyAssets,
    _materials: &mut Assets<StandardMaterial>,
    kind: EnemyKind,
    pos: Vec3,
    floor: u32,
    alerted: bool,
) -> Entity {
    let mut enemy = Enemy::new(kind, floor);
    enemy.alerted = alerted;
    let aura_h = enemy.stats.eye_height;
    let aura_color = match kind {
        EnemyKind::JammerFiend => Color::srgb(0.4, 1.0, 0.2),
        EnemyKind::HellBaron => Color::srgb(1.0, 0.6, 0.1),
        EnemyKind::CarrierDaemon => Color::srgb(1.0, 0.25, 0.7),
        _ => Color::srgb(1.0, 0.1, 0.05),
    };
    let base_aura = base_aura_intensity(kind);
    let root = commands
        .spawn((
            LevelEntity,
            Transform::from_translation(pos),
            Visibility::default(),
        ))
        .id();
    let aura = commands
        .spawn((
            ChildOf(root),
            EnemyAura,
            CullableLight::for_range(7.0),
            PointLight { color: aura_color, intensity: base_aura, range: 7.0, shadow_maps_enabled: false, ..default() },
            // Dark auras (calm Imps / Cacodemons) stay hidden until they glow.
            if base_aura > 0.0 { Visibility::Inherited } else { Visibility::Hidden },
            Transform::from_xyz(0.0, aura_h, -0.6),
        ))
        .id();
    let pivot = commands.spawn((ChildOf(root), EnemyBody, Transform::default(), Visibility::default())).id();
    let (mut body_part, mut eye_part) = (Entity::PLACEHOLDER, Entity::PLACEHOLDER);
    let model = &a.models[kind.index()];
    // The body pivot carries the first static (never re-tinted) part itself:
    // one entity fewer per demon. Tintable Body / Eye parts stay children.
    let pivot_part = model.iter().position(|p| p.role == PartRole::Static);
    for (i, part) in model.iter().enumerate() {
        if Some(i) == pivot_part {
            commands.entity(pivot).insert((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone())));
            continue;
        }
        let e = commands.spawn((ChildOf(pivot), Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), Transform::IDENTITY)).id();
        match part.role {
            PartRole::Body => body_part = e,
            PartRole::Eye => eye_part = e,
            PartRole::Static => {}
        }
    }
    let mut rig = EnemyRig::default();
    for (slot, model) in rig.pivots.iter_mut().zip(&a.rigs[kind.index()]) {
        *slot = Some(spawn_rig(commands, pivot, model));
    }
    if reacts_to_dead_zones(kind) {
        // Distinct crackling electric-violet halo shown only while Static-Enraged!
        commands.spawn((
            ChildOf(root),
            EnrageHalo,
            Mesh3d(a.ring.clone()),
            MeshMaterial3d(a.enrage_halo.clone()),
            Transform::from_xyz(0.0, 1.15, 0.0).with_scale(Vec3::new(0.68, 0.25, 0.68)).with_rotation(Quat::from_rotation_x(0.45)),
            Visibility::Hidden,
        ));
    }
    if kind == EnemyKind::JammerFiend {
        // Unmistakable toxic-green mobile jamming perimeter ring around every Jammer Fiend.
        commands.spawn((
            ChildOf(root),
            FiendFieldRing,
            Mesh3d(a.ring.clone()),
            MeshMaterial3d(a.fiend_ring.clone()),
            Transform::from_xyz(0.0, 0.14, 0.0).with_scale(Vec3::new(2.8, 0.35, 2.8)),
        ));
    }
    if kind == EnemyKind::RelaySniper {
        let laser = commands
            .spawn((ChildOf(root), SniperLaser, Mesh3d(a.cube.clone()), MeshMaterial3d(a.sniper_laser.clone()), Transform::default(), Visibility::Hidden))
            .id();
        commands.entity(root).insert(SniperLaserRef(laser));
    }
    if kind == EnemyKind::CarrierDaemon {
        crate::boss::attach_boss(commands, a, root, &mut enemy);
    }
    commands.entity(root).insert((enemy, EnemyLook { body_part, eye_part, aura }));
    if rig.pivots[0].is_some() {
        commands.entity(root).insert(rig);
    }
    root
}

/// Steady aura light of a calm demon (0 = hidden until enraged).
pub fn base_aura_intensity(kind: EnemyKind) -> f32 {
    match kind {
        EnemyKind::JammerFiend => 120_000.0,
        EnemyKind::HellBaron => 200_000.0,
        EnemyKind::CarrierDaemon => 320_000.0,
        _ => 0.0,
    }
}

/// Spawn height for a kind at a cell (hover height for flyers).
pub fn spawn_pos_for(kind: EnemyKind, grid: &LevelGrid, c: IVec2) -> Vec3 {
    let p = grid.cell_center(c);
    if kind.flying() { p + Vec3::Y * 2.3 } else { p + Vec3::Y * 0.05 }
}

// ---------------------------------------------------------------------------
// Garrison placement
// ---------------------------------------------------------------------------

pub fn garrison_size(floor: u32) -> usize {
    (5 + 2 * floor.max(1) as usize).min(16)
}

pub fn garrison_fiends(floor: u32) -> usize {
    (floor.max(1) as usize).div_ceil(2).min(5)
}

/// Phase Stalkers in a floor's garrison (none on Floor 1).
pub fn garrison_stalkers(floor: u32) -> usize {
    if floor < 2 { 0 } else { (1 + floor as usize / 3).min(3) }
}

/// Relay Snipers in a floor's garrison (none on Floor 1).
pub fn garrison_snipers(floor: u32) -> usize {
    if floor < 2 { 0 } else { (1 + (floor as usize - 2) / 2).min(3) }
}

/// Straight open cells visible from `c` along the 4 grid axes (sightline score).
pub fn sightline_cells(grid: &LevelGrid, c: IVec2) -> i32 {
    RampDir::ALL
        .map(RampDir::offset)
        .into_iter()
        .map(|o| (1..24).take_while(|k| !grid.cell(c + o * *k).is_wall()).count() as i32)
        .sum()
}

/// Relay Sniper perch preference: elevated catwalk cells first, then the
/// longest sightlines. Lower is better.
pub fn sniper_perch_rank(grid: &LevelGrid, c: IVec2) -> (u8, i32) {
    let catwalk = matches!(grid.cell(c), Cell::Floor { height } if (height - crate::level::CATWALK_HEIGHT).abs() < 1e-3);
    (if catwalk { 0 } else { 1 }, -sightline_cells(grid, c))
}

/// Initial demons of a floor: walkable cells far from the airlock, spread out.
pub fn garrison_plan(grid: &LevelGrid, rng: &mut impl Rng) -> Vec<(EnemyKind, IVec2)> {
    let floor = grid.floor;
    let dist = grid.walk_distances(grid.spawn);
    let mut cands: Vec<IVec2> = (0..grid.depth)
        .flat_map(|z| (0..grid.width).map(move |x| IVec2::new(x, z)))
        .filter(|&c| matches!(grid.cell(c), Cell::Floor { .. } | Cell::AtriumFloor))
        .filter(|&c| c != grid.tower && dist[(c.y * grid.width + c.x) as usize].is_some_and(|d| d >= 7))
        .collect();
    cands.shuffle(rng);
    let total = garrison_size(floor);
    let fiends = garrison_fiends(floor);
    let cacos = (1 + floor as usize / 2).min(5);
    let stalkers = garrison_stalkers(floor);
    let snipers = garrison_snipers(floor);
    let spread = |picked: &[IVec2], c: IVec2| picked.iter().all(|p| (*p - c).abs().max_element() >= 2);
    // Relay Snipers claim their perches first: catwalks, then long sightlines
    // (kept clear of the tower pedestal so the uplink stays fair).
    let mut sniper_cells: Vec<IVec2> = Vec::new();
    if snipers > 0 {
        let mut perches: Vec<IVec2> = cands.iter().copied().filter(|c| (*c - grid.tower).abs().max_element() > 3).collect();
        perches.sort_by_key(|c| sniper_perch_rank(grid, *c));
        for c in perches {
            if sniper_cells.len() >= snipers {
                break;
            }
            if spread(&sniper_cells, c) {
                sniper_cells.push(c);
            }
        }
    }
    let mut picked: Vec<IVec2> = Vec::new();
    for c in cands {
        if picked.len() + sniper_cells.len() >= total {
            break;
        }
        if spread(&picked, c) && spread(&sniper_cells, c) {
            picked.push(c);
        }
    }
    let mut plan: Vec<(EnemyKind, IVec2)> = picked
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let kind = if i < fiends {
                EnemyKind::JammerFiend
            } else if i < fiends + cacos {
                EnemyKind::Cacodemon
            } else if i < fiends + cacos + stalkers {
                EnemyKind::PhaseStalker
            } else {
                EnemyKind::Imp
            };
            (kind, c)
        })
        .collect();
    plan.extend(sniper_cells.into_iter().map(|c| (EnemyKind::RelaySniper, c)));
    plan
}

fn spawn_garrison(
    mut commands: Commands,
    grid: Res<LevelGrid>,
    a: Res<EnemyAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut flow: ResMut<FlowField>,
) {
    flow.reset();
    let mut rng = rand::rng();
    for (kind, c) in garrison_plan(&grid, &mut rng) {
        spawn_enemy(&mut commands, &a, &mut materials, kind, spawn_pos_for(kind, &grid, c), grid.floor, false);
    }
}

// ---------------------------------------------------------------------------
// Flow field pathing
// ---------------------------------------------------------------------------

/// BFS distances toward the player's cell (walkers honour ramps/steps,
/// flyers only avoid walls). Rebuilt in place: the distance buffers and the
/// BFS queue are reused, so a player changing cells allocates nothing.
#[derive(Resource, Debug, Default, Clone)]
pub struct FlowField {
    pub width: i32,
    pub target: Option<IVec2>,
    pub walk: Vec<Option<u32>>,
    pub fly: Vec<Option<u32>>,
    queue: VecDeque<IVec2>,
}

fn bfs_into(grid: &LevelGrid, target: IVec2, d: &mut Vec<Option<u32>>, q: &mut VecDeque<IVec2>, passable: impl Fn(IVec2, IVec2) -> bool) {
    d.clear();
    d.resize((grid.width * grid.depth) as usize, None);
    q.clear();
    if !grid.in_bounds(target) || grid.cell(target).is_wall() {
        return;
    }
    let idx = |c: IVec2| (c.y * grid.width + c.x) as usize;
    d[idx(target)] = Some(0);
    q.push_back(target);
    while let Some(c) = q.pop_front() {
        let dc = d[idx(c)].unwrap();
        for o in RampDir::ALL.map(RampDir::offset) {
            let n = c + o;
            if grid.in_bounds(n) && d[idx(n)].is_none() && passable(n, c) {
                d[idx(n)] = Some(dc + 1);
                q.push_back(n);
            }
        }
    }
}

impl FlowField {
    pub fn build(grid: &LevelGrid, target: IVec2) -> Self {
        let mut f = FlowField::default();
        f.rebuild(grid, target);
        f
    }

    /// Recompute toward `target`, reusing every buffer.
    pub fn rebuild(&mut self, grid: &LevelGrid, target: IVec2) {
        self.width = grid.width;
        self.target = Some(target);
        // Enemy at `from` must be able to step into `to`.
        bfs_into(grid, target, &mut self.walk, &mut self.queue, |from, to| grid.can_walk(from, to));
        bfs_into(grid, target, &mut self.fly, &mut self.queue, |from, _| !grid.cell(from).is_wall());
    }

    /// Forget the target (new floor) while keeping the buffers' capacity.
    pub fn reset(&mut self) {
        self.width = 0;
        self.target = None;
        self.walk.clear();
        self.fly.clear();
        self.queue.clear();
    }

    fn get(&self, field: &[Option<u32>], c: IVec2) -> Option<u32> {
        if c.x < 0 || c.y < 0 || c.x >= self.width {
            return None;
        }
        field.get((c.y * self.width + c.x) as usize).copied().flatten()
    }

    /// Next cell to step toward the target, or `None` (no path / already there).
    pub fn next_step(&self, grid: &LevelGrid, from: IVec2, flying: bool) -> Option<IVec2> {
        let field = if flying { &self.fly } else { &self.walk };
        let here = self.get(field, from)?;
        if here == 0 {
            return None;
        }
        RampDir::ALL
            .map(RampDir::offset)
            .into_iter()
            .map(|o| from + o)
            .filter(|&n| flying || grid.can_walk(from, n))
            .filter_map(|n| self.get(field, n).map(|d| (d, n)))
            .filter(|(d, _)| *d < here)
            .min_by_key(|(d, _)| *d)
            .map(|(_, n)| n)
    }
}

fn update_flow_field(grid: Res<LevelGrid>, mut flow: ResMut<FlowField>, player: Query<&Transform, With<Player>>) {
    let Ok(ptf) = player.single() else { return };
    let c = grid.world_to_cell(ptf.translation);
    if grid.cell(c).is_wall() {
        return;
    }
    if flow.target != Some(c) || flow.width != grid.width {
        flow.rebuild(&grid, c);
    }
}

// ---------------------------------------------------------------------------
// AI
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn enemy_ai(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    flow: Res<FlowField>,
    fx: Res<FxAssets>,
    mut sfx: ResMut<SfxQueue>,
    mut players: Query<(&Transform, &mut Player)>,
    mut enemies: Query<(Entity, &mut Enemy, &mut Transform, &GlobalTransform), Without<Player>>,
    jammers: Query<(&DemonJammer, &GlobalTransform)>,
    tower: Query<&SignalTower>,
    decoys: Query<&GlobalTransform, (With<DecoyHandset>, Without<Enemy>)>,
    (freeze, signal, mods): (Option<Res<crate::dev::DevFreezeAi>>, Res<SignalState>, Res<PlayerModifiers>),
    (mut snapshot, mut fields): (Local<Vec<(Entity, Vec3, f32)>>, Local<Vec<JammerField>>),
) {
    if freeze.is_some() {
        return;
    }
    let incoming = incoming_damage_scale(signal.in_dead_zone, &mods);
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let ppos = ptf.translation;
    let p_eye = ppos + Vec3::Y * PLAYER_EYE_HEIGHT;
    let p_chest = ppos + Vec3::Y * 1.1;
    let mut rng = rand::rng();

    // Snapshot for separation + jammer fields (static jammers + fiends).
    snapshot.clear();
    snapshot.extend(enemies.iter().map(|(e, en, tf, _)| (e, tf.translation, en.stats.radius)));
    fields.clear();
    fields.extend(active_jammer_fields_iter(jammers.iter()));
    fields.extend(fiend_jammer_fields_iter(enemies.iter().map(|(_, e, _, g)| (e, g))));
    let tower_ref = grid.tower_world() + Vec3::Y * 1.5;
    let beacon = tower.single().map(|t| t.beacon).unwrap_or(tower_ref);
    let neutral = PlayerModifiers::default();
    let decoy = decoys.iter().next().map(|g| g.translation());

    for (entity, mut e, mut tf, _) in &mut enemies {
        if !e.is_alive() {
            continue;
        }
        e.phase += dt;
        e.hit_flash = (e.hit_flash - dt).max(0.0);
        e.melee_timer = (e.melee_timer - dt).max(0.0);
        let pos = tf.translation;
        let eye = pos + Vec3::Y * e.stats.eye_height;
        let to_p = ppos - pos;
        let flat = Vec3::new(to_p.x, 0.0, to_p.z);
        let dist = flat.length();

        // Senses (staggered).
        e.los_timer -= dt;
        if e.los_timer <= 0.0 {
            e.los_timer = 0.25;
            e.sees_player = dist < SIGHT_MAX_DIST && has_line_of_sight(&grid, eye, p_eye);
            if !e.alerted && wakes_on_senses(e.sees_player, dist) {
                e.alerted = true;
            }
        }
        e.enrage_check -= dt;
        if e.enrage_check <= 0.0 {
            e.enrage_check = 0.3;
            if reacts_to_dead_zones(e.kind) {
                let walls = count_wall_occlusions(&grid, eye, beacon);
                let r = compute_signal_at(eye, tower_ref, &fields, walls, &neutral);
                e.enraged = should_enrage(e.kind, r.in_dead_zone);
            }
        }

        // Knockback decays.
        let kb = e.knockback;
        e.knockback = kb * (1.0 - (dt * 6.0).min(1.0));

        if e.stun_timer > 0.0 {
            e.stun_timer -= dt;
            // Stun interrupts any active sniper lock or phase-blink windup.
            e.sniper_lock_timer = 0.0;
            e.blink_timer = 0.0;
            let mut vel = Vec3::new(kb.x, e.velocity.y, kb.z);
            if !e.kind.flying() {
                let r = resolve_player_movement(&grid, pos, vel, dt, e.stats.radius, e.stats.height, crate::level::STEP_HEIGHT, WALKER_GLUE);
                tf.translation = r.pos;
                vel = r.vel;
                e.grounded = r.grounded;
            } else {
                tf.translation = fly_move(&grid, pos, vel * dt, e.stats.radius);
            }
            e.velocity = vel;
            continue;
        }

        // Missed Call: a ringing decoy pulls nearby Imps away from the player.
        if let Some(dpos) = decoy_lure(e.kind, pos, decoy) {
            e.alerted = true;
            let to_d = Vec3::new(dpos.x - pos.x, 0.0, dpos.z - pos.z);
            let dir = if to_d.length() > e.stats.radius + 0.4 { to_d.normalize_or_zero() } else { Vec3::ZERO };
            let face = to_d.normalize_or_zero();
            if face != Vec3::ZERO {
                let want = Quat::from_rotation_y(crate::level::yaw_facing(Vec2::new(face.x, face.z)));
                tf.rotation = tf.rotation.slerp(want, (dt * 10.0).min(1.0));
            }
            let speed = e.move_speed();
            let k = (dt * 8.0).min(1.0);
            let mut vel = e.velocity;
            vel.x += (dir.x * speed - vel.x) * k;
            vel.z += (dir.z * speed - vel.z) * k;
            let r = resolve_player_movement(&grid, pos, vel + Vec3::new(kb.x, 0.0, kb.z), dt, e.stats.radius, e.stats.height, crate::level::STEP_HEIGHT, WALKER_GLUE);
            tf.translation = r.pos;
            vel.y = r.vel.y;
            e.grounded = r.grounded;
            e.velocity = vel;
            continue;
        }

        // Phase Stalker: Dead-Zone Phase Blink (shimmer telegraph -> flank blink).
        if e.kind == EnemyKind::PhaseStalker {
            e.blink_cooldown -= dt;
            if e.blink_timer > 0.0 {
                e.blink_timer -= dt;
                e.velocity = Vec3::new(e.velocity.x * 0.8, e.velocity.y, e.velocity.z * 0.8);
                if e.blink_timer <= 0.0 {
                    e.blink_timer = 0.0;
                    e.blink_cooldown = rng.random_range(2.4..3.4);
                    if let Some(dest) = pick_phase_blink_target(&grid, pos, ppos, e.blink_side) {
                        spawn_burst(&mut commands, &fx, pos + Vec3::Y * 1.2, BurstColor::Gore, 12, 6.0, 0.12);
                        tf.translation = dest;
                        e.velocity = Vec3::ZERO;
                        spawn_burst(&mut commands, &fx, dest + Vec3::Y * 1.2, BurstColor::Gore, 10, 5.0, 0.12);
                        // Arrives ready to fire its twin bolts, lunging out of the blink.
                        e.attack_timer = e.attack_timer.min(0.25);
                        e.trigger_attack_anim(true);
                    }
                    e.blink_side = -e.blink_side;
                }
                continue;
            }
            if should_start_blink(&e, dist) {
                e.blink_timer = PHASE_BLINK_TELEGRAPH;
                spawn_burst(&mut commands, &fx, pos + Vec3::Y * 1.3, BurstColor::Gore, 8, 3.0, 0.1);
                sfx.play_at(SfxEvent::PhaseBlink, pos);
                continue;
            }
        }

        // Desired horizontal direction.
        let mut desired = Vec3::ZERO;
        if e.alerted {
            let dir_p = flat.normalize_or_zero();
            let strafe = Vec3::new(-dir_p.z, 0.0, dir_p.x) * (e.phase * 0.9).sin();
            let direct = match e.kind {
                EnemyKind::Imp | EnemyKind::HellBaron | EnemyKind::CarrierDaemon => {
                    if dist > e.stats.radius + PLAYER_RADIUS + 0.6 { dir_p + strafe * 0.25 } else { Vec3::ZERO }
                }
                EnemyKind::PhaseStalker => {
                    if dist > 7.0 { dir_p + strafe * 0.6 } else { strafe * 1.1 + dir_p * 0.2 }
                }
                EnemyKind::RelaySniper => {
                    // Holds its perch at range; backs off when rushed.
                    if e.sniper_lock_timer > 0.0 {
                        Vec3::ZERO
                    } else if dist > 38.0 {
                        dir_p
                    } else if dist < 9.0 {
                        -dir_p + strafe * 0.4
                    } else {
                        Vec3::ZERO
                    }
                }
                EnemyKind::Cacodemon => {
                    if dist > 11.0 {
                        dir_p + strafe * 0.5
                    } else if dist < 6.0 {
                        -dir_p + strafe
                    } else {
                        strafe * 1.2
                    }
                }
                EnemyKind::JammerFiend => {
                    if dist > 6.0 { dir_p + strafe * 0.3 } else { strafe * 0.6 }
                }
            };
            let cell = grid.world_to_cell(pos);
            desired = if e.sees_player || e.sniper_lock_timer > 0.0 {
                direct
            } else if let Some(n) = flow.next_step(&grid, cell, e.kind.flying()) {
                let tgt = grid.cell_center(n);
                Vec3::new(tgt.x - pos.x, 0.0, tgt.z - pos.z).normalize_or_zero()
            } else {
                direct
            };
            desired = desired.normalize_or_zero();
            // Face the player (or the path).
            let face = if e.sees_player || desired == Vec3::ZERO { dir_p } else { desired };
            if face != Vec3::ZERO {
                let want = Quat::from_rotation_y(crate::level::yaw_facing(Vec2::new(face.x, face.z)));
                tf.rotation = tf.rotation.slerp(want, (dt * 8.0).min(1.0));
            }
        }

        // Separation from other demons.
        let mut push = Vec3::ZERO;
        for &(other, opos, orad) in snapshot.iter() {
            if other == entity {
                continue;
            }
            let d = Vec3::new(pos.x - opos.x, 0.0, pos.z - opos.z);
            let min = e.stats.radius + orad;
            let l = d.length();
            if l < min && l > 1e-4 {
                push += d / l * (min - l);
            }
        }

        let speed = e.move_speed();
        let target_h = desired * speed + push * 4.0;
        let k = (dt * 8.0).min(1.0);
        let mut vel = e.velocity;
        vel.x += (target_h.x - vel.x) * k;
        vel.z += (target_h.z - vel.z) * k;
        if e.kind.flying() {
            // Hover: track the player's eye height, stay under ceilings.
            let floor_y = sample_floor_height(&grid, pos.x, pos.z).unwrap_or(0.0);
            let ceil = grid.ceiling_at(grid.world_to_cell(pos)).unwrap_or(CEILING_HEIGHT + 6.0);
            let bob = (e.phase * 1.7).sin() * 0.6;
            let want_y = (ppos.y + 2.2 + bob).clamp(floor_y + 1.2, ceil - 1.1);
            vel.y = (want_y - pos.y) * 2.0;
            let step = (vel + kb) * dt;
            tf.translation = fly_move(&grid, pos, step, e.stats.radius);
        } else {
            let r = resolve_player_movement(&grid, pos, vel + Vec3::new(kb.x, 0.0, kb.z), dt, e.stats.radius, e.stats.height, crate::level::STEP_HEIGHT, WALKER_GLUE);
            tf.translation = r.pos;
            vel.y = r.vel.y;
            e.grounded = r.grounded;
        }
        e.velocity = vel;

        // Attacks.
        if !e.alerted {
            continue;
        }
        let melee_reach = e.stats.radius + PLAYER_RADIUS + 0.9;
        if dist < melee_reach && (to_p.y).abs() < 2.5 && e.melee_timer <= 0.0 {
            e.melee_timer = if e.kind == EnemyKind::HellBaron { 1.4 } else { 1.0 };
            e.trigger_attack_anim(true);
            let hurt = player.invuln_timer <= 0.0;
            player.apply_damage(e.stats.melee_damage * if e.enraged { 1.3 } else { 1.0 } * incoming);
            if hurt {
                sfx.play(SfxEvent::Hurt);
            }
            continue;
        }
        e.attack_timer -= dt;
        match e.kind {
            // Boss patterns live in `crate::boss`.
            EnemyKind::CarrierDaemon => continue,
            EnemyKind::RelaySniper => {
                if e.sniper_lock_timer > 0.0 {
                    // Breaking sight drops the lock immediately so peeking back
                    // out always grants a fresh laser warning.
                    if !has_line_of_sight(&grid, eye, p_chest) {
                        e.sniper_lock_timer = 0.0;
                        e.attack_timer = e.attack_timer.max(0.6);
                    } else {
                        e.sniper_lock_timer -= dt;
                        e.sniper_aim = p_chest;
                        if e.sniper_lock_timer <= 0.0 {
                            e.sniper_lock_timer = 0.0;
                            e.attack_timer = e.fire_cooldown() * rng.random_range(0.9..1.15);
                            let aim = sniper_lead_aim(eye, p_chest, player.velocity, e.stats.proj_speed);
                            let dir = (aim - eye).normalize_or_zero();
                            let start = eye + dir * (e.stats.radius + 0.4);
                            spawn_enemy_projectile(&mut commands, &fx, start, dir * e.stats.proj_speed, e.stats.proj_damage, e.stats.proj_radius, BurstColor::Spark);
                            spawn_burst(&mut commands, &fx, start, BurstColor::Spark, 6, 4.0, 0.08);
                            e.trigger_attack_anim(false);
                            sfx.play_at(SfxEvent::EnemyFire, start);
                        }
                    }
                } else if e.attack_timer <= 0.0 && e.sees_player && dist < e.stats.range && dist > 2.0 {
                    e.sniper_lock_timer = SNIPER_LOCK_SECS;
                    e.sniper_aim = p_chest;
                    sfx.play_at(SfxEvent::SniperLock, eye);
                }
                continue;
            }
            _ => {}
        }
        // The Baron's triple spread needs room to fan out: no point-blank volleys.
        let min_range = if e.kind == EnemyKind::HellBaron { 5.0 } else { 2.0 };
        if e.attack_timer <= 0.0 && e.sees_player && dist < e.stats.range && dist > min_range {
            e.attack_timer = e.fire_cooldown() * rng.random_range(0.8..1.25);
            let fwd = (p_chest - eye).normalize_or_zero();
            let side = Vec3::new(-fwd.z, 0.0, fwd.x).normalize_or_zero();
            // (offset, direction) per projectile: fixed stack array, no allocation.
            let none = (Vec3::ZERO, Vec3::ZERO);
            let (color, dirs, n): (BurstColor, [(Vec3, Vec3); 3], usize) = match e.kind {
                EnemyKind::Imp => (BurstColor::Fire, [(Vec3::ZERO, fwd), none, none], 1),
                EnemyKind::Cacodemon => (BurstColor::Cyan, [(side * 0.45, fwd), (-side * 0.45, fwd), none], 2),
                EnemyKind::JammerFiend => (BurstColor::Green, [(Vec3::ZERO, fwd), none, none], 1),
                EnemyKind::HellBaron => (BurstColor::Fire, [-0.21f32, 0.0, 0.21].map(|a| (Vec3::ZERO, Quat::from_rotation_y(a) * fwd)), 3),
                // Fast twin violet static bolts.
                EnemyKind::PhaseStalker => (BurstColor::Gore, [(side * 0.3, fwd), (-side * 0.3, fwd), none], 2),
                EnemyKind::RelaySniper | EnemyKind::CarrierDaemon => (BurstColor::Spark, [(Vec3::ZERO, fwd), none, none], 1),
            };
            let caco_purple = matches!(e.kind, EnemyKind::Cacodemon | EnemyKind::PhaseStalker);
            for &(off, d) in &dirs[..n] {
                let jitter = Vec3::new(rng.random_range(-0.03..0.03), rng.random_range(-0.02..0.02), rng.random_range(-0.03..0.03));
                let start = eye + fwd * (e.stats.radius + 0.3) + off;
                spawn_enemy_projectile(
                    &mut commands,
                    &fx,
                    start,
                    (d + jitter).normalize_or_zero() * e.stats.proj_speed,
                    e.stats.proj_damage,
                    e.stats.proj_radius,
                    if caco_purple { BurstColor::Gore } else { color },
                );
            }
            e.trigger_attack_anim(false);
            sfx.play_at(SfxEvent::EnemyFire, eye);
        }
    }
}

/// Phase Stalker blink trigger: enraged (in a 0-1 bar Dead Zone), alerted,
/// seeing the player or close, and off cooldown.
pub fn should_start_blink(e: &Enemy, dist: f32) -> bool {
    e.kind == EnemyKind::PhaseStalker && e.enraged && e.alerted && (e.sees_player || dist < 12.0) && e.blink_cooldown <= 0.0 && e.blink_timer <= 0.0
}

/// Missed Call lure target for a demon at `pos`: only Imps within
/// [`DECOY_LURE_RADIUS`] of an active decoy are drawn to it.
pub fn decoy_lure(kind: EnemyKind, pos: Vec3, decoy: Option<Vec3>) -> Option<Vec3> {
    let d = decoy?;
    (kind == EnemyKind::Imp && Vec2::new(d.x - pos.x, d.z - pos.z).length() <= DECOY_LURE_RADIUS).then_some(d)
}

/// Flying movement with per-axis wall sliding.
fn fly_move(grid: &LevelGrid, pos: Vec3, step: Vec3, radius: f32) -> Vec3 {
    let blocked = |p: Vec3| {
        let probe = |dx: f32, dz: f32| {
            let c = grid.world_to_cell(Vec3::new(p.x + dx, 0.0, p.z + dz));
            grid.cell(c).is_wall() || sample_floor_height(grid, p.x + dx, p.z + dz).is_some_and(|h| h > p.y - 0.6)
        };
        probe(radius, 0.0) || probe(-radius, 0.0) || probe(0.0, radius) || probe(0.0, -radius)
    };
    let mut p = pos;
    let nx = Vec3::new(p.x + step.x, p.y, p.z);
    if !blocked(nx) {
        p = nx;
    }
    let nz = Vec3::new(p.x, p.y, p.z + step.z);
    if !blocked(nz) {
        p = nz;
    }
    p.y += step.y;
    p
}

pub fn spawn_enemy_projectile(commands: &mut Commands, fx: &FxAssets, pos: Vec3, vel: Vec3, damage: f32, radius: f32, color: BurstColor) {
    let look = fx.shot_look(color);
    let light = match color {
        BurstColor::Gore => Color::srgb(1.0, 0.2, 0.6),
        c => FxAssets::light_color(c),
    };
    let mut root = commands.spawn((
        LevelEntity,
        EnemyProjectile { vel, damage, radius, life: 5.0, color },
        MeshMaterial3d(look.core.clone()),
        Transform::from_translation(pos).with_rotation(enemy_shot_rotation(color, vel)).with_scale(Vec3::splat(radius * 0.65)),
    ));
    let glow = (PointLight { color: light, intensity: 110_000.0, range: 6.0, shadow_maps_enabled: false, ..default() }, Transform::default());
    match ShotShape::for_color(color) {
        ShotShape::Needle => {
            root.insert((
                Mesh3d(fx.proj_needle_mesh.clone()),
                children![
                    (Mesh3d(fx.proj_needle_sheath.clone()), MeshMaterial3d(look.halo.clone()), Transform::default()),
                    (Mesh3d(fx.ring.clone()), MeshMaterial3d(look.ring.clone()), needle_collar_transform()),
                    glow,
                ],
            ));
        }
        ShotShape::Orb => {
            root.insert((
                Mesh3d(fx.proj_orb_mesh.clone()),
                children![
                    (Mesh3d(fx.proj_orb_mesh.clone()), MeshMaterial3d(look.halo.clone()), Transform::from_scale(Vec3::splat(1.75))),
                    (Mesh3d(fx.ring.clone()), MeshMaterial3d(look.ring.clone()), Transform::from_scale(Vec3::new(1.85, 0.55, 1.85)).with_rotation(Quat::from_rotation_x(0.7))),
                    glow,
                ],
            ));
        }
    }
}

/// Initial orientation of an enemy shot: rail lances point along `vel`.
pub fn enemy_shot_rotation(color: BurstColor, vel: Vec3) -> Quat {
    match ShotShape::for_color(color) {
        ShotShape::Needle => Quat::from_rotation_arc(Vec3::Y, vel.normalize_or(Vec3::Y)),
        ShotShape::Orb => Quat::IDENTITY,
    }
}

/// Per-frame flight spin: orbs tumble/churn, rail lances spin about their
/// (velocity-aligned) local +Y.
pub fn spin_enemy_shot(tf: &mut Transform, color: BurstColor, dt: f32) {
    match ShotShape::for_color(color) {
        ShotShape::Needle => tf.rotate_local_y(dt * 14.0),
        ShotShape::Orb => {
            tf.rotate_local_y(dt * 5.5);
            tf.rotate_local_x(dt * 3.8);
        }
    }
}

/// Enemy bolts at least this big (Hell Baron / boss hellfire) scorch walls.
pub const HEAVY_BOLT_RADIUS: f32 = 0.38;

/// Shortest distance between segment `a..b` and point `p`.
pub fn segment_point_distance(a: Vec3, b: Vec3, p: Vec3) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-8 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
    (a + ab * t).distance(p)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn move_enemy_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    fx: Res<FxAssets>,
    mut sfx: ResMut<SfxQueue>,
    mut players: Query<(&Transform, &mut Player)>,
    mut q: Query<(Entity, &mut EnemyProjectile, &mut Transform), Without<Player>>,
    (signal, mods, mut decals): (Res<SignalState>, Res<PlayerModifiers>, ResMut<DecalQueue>),
) {
    let incoming = incoming_damage_scale(signal.in_dead_zone, &mods);
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let pfeet = ptf.translation;
    for (e, mut p, mut tf) in &mut q {
        p.life -= dt;
        let step = p.vel * dt;
        let from = tf.translation;
        let to = from + step;
        // Clip the player sweep at the first terrain hit so fast rail/boss
        // bolts can never damage the player through solid cover.
        let wall_hit = raycast_grid(&grid, from, step, step.length());
        let clip_to = wall_hit.map_or(to, |h| h.point);
        let hit_player = [0.3f32, 0.9, 1.5].iter().any(|h| segment_point_distance(from, clip_to, pfeet + Vec3::Y * *h) < p.radius + PLAYER_RADIUS);
        if hit_player {
            let hurt = player.invuln_timer <= 0.0;
            player.apply_damage(p.damage * incoming);
            if hurt {
                sfx.play(SfxEvent::Hurt);
            }
            spawn_burst(&mut commands, &fx, clip_to, p.color, 6, 4.0, 0.12);
            commands.entity(e).try_despawn();
            continue;
        }
        if let Some(hit) = wall_hit {
            spawn_burst(&mut commands, &fx, hit.point + hit.normal * 0.1, p.color, 6, 4.0, 0.12);
            // Heavy hellfire (Hell Baron / Carrier Daemon) chars the wall it hits.
            if p.radius >= HEAVY_BOLT_RADIUS && p.color == BurstColor::Fire {
                decals.push(DecalRequest::surface(DecalKind::Scorch, hit.point, hit.normal, p.radius * 3.2));
            }
            commands.entity(e).try_despawn();
            continue;
        }
        if p.life <= 0.0 {
            commands.entity(e).try_despawn();
            continue;
        }
        tf.translation = to;
        spin_enemy_shot(&mut tf, p.color, dt);
    }
}

fn tick_ping_marks(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut PingMarked)>, mut rings: Query<&mut Transform>) {
    let dt = time.delta_secs();
    for (e, mut m) in &mut q {
        m.timer -= dt;
        if let Some(r) = m.ring
            && let Ok(mut tf) = rings.get_mut(r)
        {
            tf.rotate_y(dt * 3.0);
        }
        if m.timer <= 0.0 {
            if let Some(r) = m.ring {
                commands.entity(r).try_despawn();
            }
            commands.entity(e).try_remove::<PingMarked>();
        }
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn enemy_visuals(
    time: Res<Time>,
    ea: Res<EnemyAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut enemies: Query<(&mut Enemy, &Transform, &EnemyLook, Option<&PingMarked>, &Children, Option<&EnemyRig>, Option<&crate::boss::BossState>)>,
    mut bodies: Query<&mut Transform, (With<EnemyBody>, Without<EnrageHalo>, Without<FiendFieldRing>, Without<Enemy>)>,
    mut rigs: Query<(&mut Transform, &mut RigPivot), (Without<EnemyBody>, Without<EnrageHalo>, Without<FiendFieldRing>, Without<Enemy>)>,
    mut halos: Query<(&mut Transform, &mut Visibility), (With<EnrageHalo>, Without<EnemyBody>, Without<FiendFieldRing>, Without<Enemy>)>,
    mut fiend_rings: Query<&mut Transform, (With<FiendFieldRing>, Without<EnemyBody>, Without<EnrageHalo>, Without<Enemy>)>,
    mut auras: Query<(&mut PointLight, &mut CullableLight), With<EnemyAura>>,
    mut parts: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    let t = time.elapsed_secs();
    let dt = time.delta_secs().min(0.1);
    let pulse = look_pulse(t);
    // Pulsing (enraged / stunned) shared materials in use this frame.
    let mut pulsing = [[false; 8]; KIND_COUNT];
    let mut pulsing_flash = [[[false; FLASH_STEPS]; 8]; KIND_COUNT];
    let mut swap = |e: Entity, want: &Handle<StandardMaterial>| {
        if let Ok(mut m) = parts.get_mut(e)
            && m.0.id() != want.id()
        {
            m.0 = want.clone();
        }
    };
    for (mut e, root_tf, look, marked, children, rig, boss) in &mut enemies {
        if e.is_alive() {
            tick_enemy_anim(&mut e, dt);
        }
        let e = &*e;
        if let Some(rig) = rig {
            for pe in rig.pivots.iter().flatten() {
                if let Ok((mut tf, mut rp)) = rigs.get_mut(*pe) {
                    animate_rig(&mut tf, &mut rp, e, boss, t, dt);
                }
            }
        }
        // Stride / hover + attack recoil + directional hit-stagger on the
        // visual body pivot (never the root: hit spheres stay put).
        let pose = body_pose(e, root_tf.rotation);
        for c in children.iter() {
            if let Ok(mut b) = bodies.get_mut(c) {
                let stun = if e.stun_timer > 0.0 { (t * 40.0).sin() * 0.08 } else { 0.0 };
                let mut offset = pose.offset;
                let mut scale = Vec3::ONE;
                // Phase Stalker shimmer: glitchy jitter + stretch while charging a blink.
                if e.blink_timer > 0.0 {
                    let k = 1.0 - e.blink_timer / PHASE_BLINK_TELEGRAPH;
                    offset.x += (t * 70.0).sin() * 0.12 * (0.4 + k);
                    scale = Vec3::new(1.0 - 0.25 * k, 1.0 + 0.2 * k, 1.0 - 0.25 * k);
                }
                b.translation = offset;
                b.rotation = pose.rotation() * Quat::from_rotation_z(stun);
                if b.scale != scale {
                    b.scale = scale;
                }
            }
            if let Ok((mut htf, mut hvis)) = halos.get_mut(c) {
                let want = if e.enraged { Visibility::Inherited } else { Visibility::Hidden };
                if *hvis != want {
                    *hvis = want;
                }
                if e.enraged {
                    htf.rotation = Quat::from_euler(EulerRot::YXZ, t * 5.0 + e.phase, 0.45, (t * 8.0).sin() * 0.35);
                    let s = 0.64 + 0.14 * (t * 14.0).sin().abs();
                    htf.scale = Vec3::new(s, 0.28, s);
                }
            }
            if let Ok(mut rtf) = fiend_rings.get_mut(c) {
                let s = 2.6 + 0.45 * ((t * 3.5 + e.phase).sin() * 0.5 + 0.5);
                rtf.scale = Vec3::new(s, 0.32, s);
                rtf.rotation = Quat::from_rotation_y(t * 1.6);
            }
        }
        // Material tinting: swap to the shared state material (no per-demon materials).
        let mats = ea.look(e.kind);
        let state = (e.enraged as usize * LOOK_ENRAGED) | (marked.is_some() as usize * LOOK_MARKED) | ((e.stun_timer > 0.0) as usize * LOOK_STUNNED);
        let body = if e.hit_flash > 0.0 {
            let step = flash_step(e.hit_flash);
            if state & (LOOK_ENRAGED | LOOK_STUNNED) != 0 {
                pulsing_flash[e.kind.index()][state][step] = true;
            }
            &mats.flash[state][step]
        } else {
            if state & (LOOK_ENRAGED | LOOK_STUNNED) != 0 {
                pulsing[e.kind.index()][state] = true;
            }
            &mats.body[state]
        };
        swap(look.body_part, body);
        swap(look.eye_part, if e.enraged { &mats.eye_enraged } else { &mats.eye });
        if let Ok((mut l, mut cull)) = auras.get_mut(look.aura) {
            let intensity = match e.kind {
                _ if e.blink_timer > 0.0 => 600_000.0,
                _ if e.sniper_lock_timer > 0.0 => 260_000.0 + 200_000.0 * pulse,
                _ if e.enraged => 220_000.0 + 180_000.0 * pulse,
                EnemyKind::JammerFiend => 140_000.0 + 90_000.0 * pulse,
                EnemyKind::CarrierDaemon => 260_000.0 + 160_000.0 * pulse,
                k => base_aura_intensity(k),
            };
            cull.set_intensity(intensity);
            let violet = Color::srgb(1.0, 0.2, 0.95);
            if e.enraged && l.color != violet {
                l.color = violet;
            }
        }
    }
    // Re-tint each active pulsing material once per frame.
    for kind in EnemyKind::ALL {
        let mats = ea.look(kind);
        for (state, used) in pulsing[kind.index()].iter().enumerate() {
            if *used && let Some(mut m) = materials.get_mut(&mats.body[state]) {
                m.emissive = body_state_emissive(mats.body_emissive, state, pulse);
            }
        }
        for (state, steps) in pulsing_flash[kind.index()].iter().enumerate() {
            for (step, used) in steps.iter().enumerate() {
                if *used && let Some(mut m) = materials.get_mut(&mats.flash[state][step]) {
                    m.emissive = body_flash_emissive(mats.body_emissive, state, pulse, step);
                }
            }
        }
    }
}

/// Rig recoil kick strength (0..1) right after firing: snaps in, eases out.
pub fn rig_recoil(e: &Enemy) -> f32 {
    if e.attack_lunge || e.attack_anim <= 0.0 { 0.0 } else { snap_envelope(1.0 - e.attack_anim.min(1.0), 0.08) }
}

/// Per-frame pose of one articulated sub-pivot.
fn animate_rig(tf: &mut Transform, rp: &mut RigPivot, e: &Enemy, boss: Option<&crate::boss::BossState>, t: f32, dt: f32) {
    let rest = rp.rest;
    let ease = |k: f32| 1.0 - (-k * dt).exp();
    match rp.kind {
        RigKind::FiendEmitter => {
            // Continuous transceiver spin + pulsing broadcast emitter.
            let beat = (t * 6.0 + e.phase).sin() * 0.5 + 0.5;
            tf.rotation = rest.rotation * Quat::from_rotation_y(t * 2.6 + e.phase);
            tf.scale = Vec3::splat(1.0 + 0.08 * beat);
            tf.translation = rest.translation + Vec3::Y * (0.025 * beat);
        }
        RigKind::SniperDish => {
            // Idle: slow left/right scan. Lock-on: snap straight forward and tilt aggressively.
            let locking = e.sniper_lock_timer > 0.0;
            let target = if locking {
                Quat::from_euler(EulerRot::YXZ, 0.0, -0.22, (t * 45.0).sin() * 0.03)
            } else {
                Quat::from_euler(EulerRot::YXZ, (t * 0.6 + e.phase).sin() * 0.65, 0.05 * (t * 0.9 + e.phase).sin(), 0.0)
            };
            // Snappy rail-fire recoil kick (pitch up + shove back) right after the shot.
            let kick = rig_recoil(e);
            let rate = if kick > 0.0 { 30.0 } else if locking { 18.0 } else { 4.0 };
            tf.rotation = tf.rotation.slerp(rest.rotation * target * Quat::from_rotation_x(0.35 * kick), ease(rate));
            tf.translation = rest.translation + Vec3::Z * (0.08 * kick);
        }
        RigKind::BossHalo => {
            // Broadcast halo spins about its axis; faster in Overdrive and while telegraphing.
            let mut speed = 0.9;
            if let Some(st) = boss {
                if st.phase == crate::boss::BossPhase::Overdrive {
                    speed *= 2.8;
                }
                if st.telegraph > 0.0 {
                    speed += 3.0;
                }
            }
            if e.stun_timer > 0.0 {
                speed *= 0.2;
            }
            rp.angle = (rp.angle + speed * dt) % TAU;
            tf.rotation = rest.rotation * Quat::from_rotation_y(rp.angle);
        }
        RigKind::BossCannon => {
            // Idle breathing pitch; recoil backward (and pitch up) while charging an attack.
            let charging = boss.is_some_and(|st| st.telegraph > 0.0);
            let breathe = (t * 1.7 + e.phase).sin();
            let (offset, pitch) = if charging { (Vec3::new(0.0, 0.04, 0.2), 0.12) } else { (Vec3::Y * (0.03 * breathe), 0.035 * breathe) };
            // Hard barrel kick right after a barrage fires (decays with `attack_anim`).
            let kick = rig_recoil(e);
            let (offset, pitch) = (offset + Vec3::new(0.0, 0.03, 0.32) * kick, pitch + 0.28 * kick);
            let rate = if kick > 0.0 { 32.0 } else if charging { 14.0 } else { 6.0 };
            tf.translation = tf.translation.lerp(rest.translation + offset, ease(rate));
            tf.rotation = tf.rotation.slerp(rest.rotation * Quat::from_rotation_x(pitch), ease(if kick > 0.0 { 32.0 } else { 8.0 }));
        }
        RigKind::PylonRing => {}
    }
}

/// Demon death processing (drops, fatalities, style + kill telemetry).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnemyDeathsSet;

#[allow(clippy::too_many_arguments)]
fn enemy_deaths(
    mut commands: Commands,
    fx: Res<FxAssets>,
    pickups: Res<PickupAssets>,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    mut run: ResMut<RunStats>,
    mut sfx: ResMut<SfxQueue>,
    mut feed: ResMut<HudFeed>,
    mut apps: ResMut<PhoneApps>,
    mut players: Query<&mut Player>,
    enemies: Query<(Entity, &Enemy, &Transform, Option<&PingMarked>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut decals: ResMut<DecalQueue>,
    (mut fatal, mut punch, grid): (ResMut<crate::fatality::FatalityState>, ResMut<crate::fx::CameraPunch>, Option<Res<LevelGrid>>),
    mut style: ResMut<crate::style::StyleQueue>,
) {
    let mut rng = rand::rng();
    // Player motion for style callouts (airborne / dashing / speed).
    let (airborne, speed) = players.single().map_or((false, 0.0), |p| (!p.grounded || p.dash_timer > 0.0, Vec2::new(p.velocity.x, p.velocity.z).length()));
    for (entity, e, tf, marked) in &enemies {
        if e.is_alive() {
            continue;
        }
        commands.entity(entity).try_despawn();
        let center = e.center(tf.translation);
        let (a, b, n) = match e.kind {
            EnemyKind::Imp => (BurstColor::Gore, BurstColor::Fire, 14),
            EnemyKind::Cacodemon => (BurstColor::Gore, BurstColor::Cyan, 18),
            EnemyKind::JammerFiend => (BurstColor::Green, BurstColor::Smoke, 18),
            EnemyKind::HellBaron => (BurstColor::Gold, BurstColor::Gore, 34),
            EnemyKind::PhaseStalker => (BurstColor::Gore, BurstColor::Spark, 16),
            EnemyKind::RelaySniper => (BurstColor::Spark, BurstColor::Smoke, 18),
            EnemyKind::CarrierDaemon => (BurstColor::Gold, BurstColor::Gore, 48),
        };
        spawn_burst(&mut commands, &fx, center, a, n, 7.0, 0.22);
        spawn_burst(&mut commands, &fx, center, b, n / 2, 5.0, 0.16);
        // Chunky sculpted gibs + archetype splatter / scorch on the floor beneath.
        let (gibs, count) = death_gibs(e.kind);
        spawn_death_gibs(&mut commands, &fx, center, gibs, count, 0.07 + e.stats.radius * 0.06, 6.5);
        let feet = tf.translation.with_y(tf.translation.y - if e.kind.flying() { 2.0 } else { 0.0 });
        for &(kind, size) in death_decals(e.kind, e.vaporized).iter().flatten() {
            decals.push(DecalRequest::floor(kind, feet, size * (0.9 + e.stats.radius)));
        }
        run.kills += 1;
        run.enemy_kills[e.kind.index()] += 1;
        sfx.play_at(SfxEvent::EnemyDeath, center);
        if e.kind == EnemyKind::CarrierDaemon {
            // Massive golden / crimson multi-burst.
            for (k, off) in [Vec3::new(0.0, 1.6, 0.0), Vec3::new(1.2, 0.4, 0.3), Vec3::new(-1.1, -0.6, -0.4), Vec3::new(0.2, -1.4, 0.8)].into_iter().enumerate() {
                let c = if k % 2 == 0 { BurstColor::Gold } else { BurstColor::Fire };
                spawn_burst(&mut commands, &fx, center + off, c, 30, 11.0, 0.3);
            }
            spawn_burst(&mut commands, &fx, center, BurstColor::Gore, 36, 9.0, 0.26);
            spawn_flash_light(&mut commands, center, Color::srgb(1.0, 0.55, 0.2), 9_000_000.0, 40.0, 0.8);
            sfx.play_at(SfxEvent::JammerDestroyed, center);
            sfx.play_at(SfxEvent::BossShield, center);
        }
        if e.vaporized {
            continue;
        }
        feed.hit(true);
        // Milestone 30: random fatality (guaranteed on point-blank overkills,
        // direct mortar hits and the Carrier Daemon), else a style callout.
        let boss = e.kind == EnemyKind::CarrierDaemon;
        let fatality = fatal.roll(e.kill_cause, boss, rng.random(), rng.random());
        let source = if e.kill_cause == crate::fatality::KillCause::MortarDirect { crate::style::StyleSource::Rocket } else { e.style_source };
        style.push(crate::style::StyleAction::Kill { kind: e.kind, source, airborne, speed, marked: marked.is_some(), fatality: fatality.is_some() });
        match fatality {
            Some(kind) => {
                let dir = Vec3::new(e.stagger_dir.x, 0.0, e.stagger_dir.y);
                let wall = grid
                    .as_deref()
                    .filter(|_| kind == crate::fatality::FatalityKind::NoService && dir != Vec3::ZERO)
                    .and_then(|g| raycast_grid(g, center, dir.normalize(), 9.0))
                    .map(|h| (h.point, h.normal));
                crate::fatality::spawn_fatality_fx(&mut commands, &fx, &mut materials, &mut decals, kind, center, feet, dir, wall);
                sfx.play_at(SfxEvent::Fatality, center);
                feed.toast(kind.callout(), kind.color());
                punch.kick(if boss { 1.0 } else { 0.7 });
            }
            None => {
                if let Some(style) = fatal.style(airborne, speed, signal.bars) {
                    feed.toast(format!("STYLE // {}", style.text()), Color::srgb(0.35, 0.95, 1.0));
                }
            }
        }
        if e.kind == EnemyKind::CarrierDaemon {
            feed.toast(crate::boss::BOSS_DEATH_TEXT, Color::srgb(1.0, 0.8, 0.3));
            feed.alert(crate::boss::BOSS_DEATH_TEXT, Color::srgb(1.0, 0.8, 0.3), 3.5);
        } else {
            feed.toast(format!("+ {} DOWN", e.kind.name()), Color::srgb(1.0, 0.55, 0.35));
        }
        // Feat telemetry: natural 0-bar blackout kills and Jammer Fiend kills.
        if signal.natural_raw_strength < 1.0 {
            run.blackout_kills += 1;
        }
        if e.kind == EnemyKind::JammerFiend {
            run.fiends_killed += 1;
            if mods.jammer_hijack {
                let foot = tf.translation.with_y(tf.translation.y - if e.kind.flying() { 2.0 } else { 0.0 });
                spawn_hijack_hotspot(&mut commands, &fx, &mut materials, &mut feed, foot);
            }
        }
        if mods.siphon_heal > 0.0
            && signal.bars >= 3
            && let Ok(mut p) = players.single_mut()
        {
            p.heal(mods.siphon_heal);
            feed.toast(format!("SIGNAL SIPHON +{} HP", mods.siphon_heal as i32), Color::srgb(0.3, 1.0, 0.7));
        }
        // Store & Forward: natural (pre-antenna) 0-bar blackout kills queue packets.
        if mods.store_and_forward && bars_from_strength(signal.natural_raw_strength) == 0 {
            let n = apps.queue_packet();
            feed.toast(format!("PACKET QUEUED ({n}/{STORE_FORWARD_MAX_PACKETS})"), Color::srgb(0.6, 0.5, 1.0));
        }
        // Emergency Callback: a ping-marked Jammer Fiend's death opens the line.
        if mods.emergency_callback && e.kind == EnemyKind::JammerFiend && marked.is_some() {
            apps.callback_timer = CALLBACK_SECS;
            feed.alert("EMERGENCY CALLBACK! ANY-SIGNAL 5G EMP READY (6s)", Color::srgb(1.0, 0.85, 0.2), 2.5);
            feed.toast("EMERGENCY CALLBACK! ANY-SIGNAL 5G EMP READY (6s)", Color::srgb(1.0, 0.85, 0.2));
        }
        // Drops.
        let ground = tf.translation.with_y(tf.translation.y - if e.kind.flying() { 2.0 } else { 0.0 });
        let drops: Vec<PickupKind> = match e.kind {
            EnemyKind::HellBaron => vec![PickupKind::Health, PickupKind::Armor, PickupKind::Shells, PickupKind::Plasma, PickupKind::Rockets],
            EnemyKind::CarrierDaemon => vec![
                PickupKind::Health,
                PickupKind::Armor,
                PickupKind::Health,
                PickupKind::Shells,
                PickupKind::Plasma,
                PickupKind::Rockets,
                PickupKind::Rockets,
            ],
            EnemyKind::RelaySniper => vec![PickupKind::Rockets],
            EnemyKind::JammerFiend => vec![PickupKind::Plasma, PickupKind::Armor, PickupKind::Rockets],
            _ => {
                let roll: f32 = rng.random();
                if roll < 0.22 {
                    vec![PickupKind::Shells]
                } else if roll < 0.4 {
                    vec![PickupKind::Plasma]
                } else if roll < 0.52 {
                    vec![PickupKind::Health]
                } else if roll < 0.58 {
                    vec![PickupKind::Rockets]
                } else {
                    vec![]
                }
            }
        };
        let spread = if e.kind == EnemyKind::CarrierDaemon { 1.6 } else { 0.8 };
        for (i, k) in drops.into_iter().enumerate() {
            let off = Vec3::new((i as f32 * 2.1).cos(), 0.0, (i as f32 * 2.1).sin()) * if i == 0 { 0.0 } else { spread };
            spawn_pickup(&mut commands, &pickups, k, ground + off, true);
        }
    }
}

/// Death gib palette and count of a demon kind.
pub fn death_gibs(kind: EnemyKind) -> (&'static [GibMat], u32) {
    use GibMat::*;
    match kind {
        EnemyKind::Imp => (&[Flesh, Bone, Flesh], 6),
        EnemyKind::Cacodemon => (&[Flesh, Flesh, Bone], 7),
        EnemyKind::JammerFiend => (&[FiendGreen, DarkMetal, FiendGreen, Bone], 7),
        EnemyKind::HellBaron => (&[Flesh, MoltenGold, Bone, Flesh], 10),
        EnemyKind::PhaseStalker => (&[VioletObsidian, VioletObsidian, Flesh], 7),
        EnemyKind::RelaySniper => (&[DarkMetal, DarkMetal, Bone], 7),
        EnemyKind::CarrierDaemon => (&[MoltenGold, DarkMetal, Flesh, VioletObsidian], 16),
    }
}

/// Floor decals `(kind, size factor)` left by a demon's death (vaporized
/// demons only leave a scorch).
pub fn death_decals(kind: EnemyKind, vaporized: bool) -> [Option<(DecalKind, f32)>; 2] {
    if vaporized {
        return [Some((DecalKind::Scorch, 1.0)), None];
    }
    match kind {
        EnemyKind::Imp | EnemyKind::Cacodemon => [Some((DecalKind::BloodCrimson, 1.0)), None],
        EnemyKind::JammerFiend => [Some((DecalKind::IchorGreen, 1.1)), None],
        EnemyKind::PhaseStalker => [Some((DecalKind::IchorViolet, 1.0)), None],
        EnemyKind::RelaySniper => [Some((DecalKind::Scorch, 0.8)), None],
        EnemyKind::HellBaron => [Some((DecalKind::Scorch, 1.3)), Some((DecalKind::BloodCrimson, 1.0))],
        EnemyKind::CarrierDaemon => [Some((DecalKind::Scorch, 1.6)), Some((DecalKind::BloodCrimson, 1.1))],
    }
}

/// Stretch each locking Relay Sniper's laser sight from its scope to the
/// locked aim point (hidden otherwise).
#[allow(clippy::type_complexity)]
fn sniper_lasers(
    snipers: Query<(&Enemy, &Transform, &SniperLaserRef), Without<SniperLaser>>,
    mut lasers: Query<(&mut Transform, &mut Visibility), (With<SniperLaser>, Without<Enemy>)>,
) {
    for (e, tf, laser) in &snipers {
        let Ok((mut ltf, mut vis)) = lasers.get_mut(laser.0) else { continue };
        let on = e.is_alive() && e.sniper_lock_timer > 0.0;
        let want = if on { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
        if !on {
            continue;
        }
        let scope = tf.transform_point(Vec3::new(0.0, e.stats.eye_height, -0.22));
        let d = e.sniper_aim - scope;
        let len = d.length();
        // Degenerate aim (target inside the scope): no direction to point along.
        let Some(dir) = d.try_normalize().filter(|_| len >= 0.05) else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        };
        let inv = tf.rotation.inverse();
        let mid_world = scope + d * 0.5;
        // Thin (and thinning as the lock completes) glowing crimson rod.
        let w = 0.03 + 0.04 * (e.sniper_lock_timer / SNIPER_LOCK_SECS);
        ltf.translation = inv * (mid_world - tf.translation);
        ltf.rotation = inv * Quat::from_rotation_arc(Vec3::Z, dir);
        ltf.scale = Vec3::new(w, w, len);
    }
}

pub struct EnemiesPlugin;

impl Plugin for EnemiesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlowField>()
            .add_systems(Startup, create_enemy_assets)
            .add_systems(
                Update,
                spawn_garrison.run_if(resource_exists_and_changed::<LevelGrid>).run_if(crate::trials::not_time_trial).in_set(GameSet::World),
            )
            .add_systems(
                Update,
                (update_flow_field, enemy_ai, move_enemy_projectiles, tick_ping_marks, enemy_visuals, sniper_lasers)
                    .chain()
                    .in_set(GameSet::Combat)
                    .run_if(resource_exists::<LevelGrid>),
            )
            .init_resource::<crate::fatality::FatalityState>()
            .add_systems(Startup, crate::fatality::prewarm_fatality_fades)
            .add_systems(OnEnter(crate::state::AppState::MainMenu), crate::fatality::reset_fatality_state)
            .add_systems(OnEnter(crate::state::AppState::Loadout), crate::fatality::reset_fatality_state)
            .add_systems(OnEnter(crate::state::AppState::ContractsFeats), crate::fatality::reset_fatality_state)
            .add_systems(OnEnter(crate::state::AppState::Leaderboard), crate::fatality::reset_fatality_state)
            .add_systems(OnEnter(crate::state::AppState::Compendium), crate::fatality::reset_fatality_state)
            .add_systems(OnEnter(crate::state::AppState::Playing), crate::fatality::reset_fatality_on_fresh_run)
            .add_systems(Update, crate::fatality::tick_fatality_state.in_set(GameSet::World))
            .configure_sets(Update, EnemyDeathsSet.in_set(GameSet::Combat))
            .add_systems(Update, enemy_deaths.in_set(EnemyDeathsSet).after(enemy_visuals).after(crate::weapons::CombatDamage));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enemy_shots_tumble_or_spin_along_flight_without_nans() {
        let vel = Vec3::new(3.0, -1.0, 7.0);
        for c in [BurstColor::Fire, BurstColor::Cyan, BurstColor::Green, BurstColor::Gore, BurstColor::Spark] {
            let mut tf = Transform::from_rotation(enemy_shot_rotation(c, vel));
            let r0 = tf.rotation;
            for _ in 0..120 {
                spin_enemy_shot(&mut tf, c, 1.0 / 60.0);
            }
            assert!(tf.rotation.is_finite() && tf.rotation.is_normalized(), "{c:?}");
            assert!(r0.angle_between(tf.rotation) > 0.1, "{c:?} animates");
            if c == BurstColor::Spark {
                assert!((tf.rotation * Vec3::Y).dot(vel.normalize()) > 0.999, "rail lance stays on its flight axis");
            }
        }
        // Degenerate velocity never yields NaNs.
        assert!(enemy_shot_rotation(BurstColor::Spark, Vec3::ZERO).is_finite());
    }
    use crate::level::generate_level;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn ray_sphere_hits_and_misses() {
        let t = ray_sphere(Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.0, 0.0, -10.0), 1.0).unwrap();
        assert!((t - 9.0).abs() < 1e-4);
        assert!(ray_sphere(Vec3::ZERO, Vec3::Z, Vec3::new(0.0, 0.0, -10.0), 1.0).is_none());
        assert!(ray_sphere(Vec3::ZERO, Vec3::NEG_Z, Vec3::new(3.0, 0.0, -10.0), 1.0).is_none());
        assert_eq!(ray_sphere(Vec3::ZERO, Vec3::NEG_Z, Vec3::ZERO, 1.0), Some(0.0));
    }

    #[test]
    fn enemy_ray_hit_uses_spheres() {
        let e = Enemy::new(EnemyKind::Imp, 1);
        let root = Vec3::new(0.0, 0.0, -8.0);
        let t = e.ray_hit(root, Vec3::new(0.0, 1.15, 0.0), Vec3::NEG_Z, 50.0).unwrap();
        assert!(t > 7.0 && t < 8.0);
        assert!(e.ray_hit(root, Vec3::new(0.0, 4.0, 0.0), Vec3::NEG_Z, 50.0).is_none());
        assert!(e.ray_hit(root, Vec3::new(0.0, 1.15, 0.0), Vec3::NEG_Z, 5.0).is_none());
    }

    #[test]
    fn damage_applies_ping_bonus_and_alerts() {
        let mods = PlayerModifiers { ping_mark_damage_bonus: 0.25, ..default() };
        let mut e = Enemy::new(EnemyKind::Cacodemon, 1);
        let hp0 = e.hp;
        assert_eq!(damage_enemy(&mut e, 20.0, false, &mods), 20.0);
        assert_eq!(damage_enemy(&mut e, 20.0, true, &mods), 25.0);
        assert_eq!(e.hp, hp0 - 45.0);
        assert!(e.alerted && e.hit_flash > 0.0);
        e.hp = 0.0;
        assert_eq!(damage_enemy(&mut e, 20.0, false, &mods), 0.0);
    }

    #[test]
    fn faraday_case_scales_dead_zone_damage_only() {
        let faraday = PlayerModifiers { dead_zone_damage_mult: 0.75, ..default() };
        assert_eq!(incoming_damage_scale(true, &faraday), 0.75);
        assert_eq!(incoming_damage_scale(false, &faraday), 1.0);
        assert_eq!(incoming_damage_scale(true, &PlayerModifiers::default()), 1.0);
    }

    #[test]
    fn imps_enrage_only_in_dead_zones() {
        let tower = Vec3::ZERO;
        let m = PlayerModifiers::default();
        let pos = Vec3::new(20.0, 0.0, 0.0);
        let clean = compute_signal_at(pos, tower, &[], 0, &m);
        assert!(!should_enrage(EnemyKind::Imp, clean.in_dead_zone));
        let jam = JammerField { pos, radius: 14.0, strength: 4.0 };
        let dead = compute_signal_at(pos, tower, &[jam], 0, &m);
        assert!(dead.in_dead_zone);
        assert!(should_enrage(EnemyKind::Imp, dead.in_dead_zone));
        assert!(!should_enrage(EnemyKind::Cacodemon, dead.in_dead_zone));
        let mut imp = Enemy::new(EnemyKind::Imp, 1);
        let base = imp.move_speed();
        let cd = imp.fire_cooldown();
        imp.enraged = true;
        assert!((imp.move_speed() / base - ENRAGE_SPEED_MULT).abs() < 1e-5);
        assert!(imp.fire_cooldown() < cd);
    }

    #[test]
    fn jammer_fiend_suppresses_signal() {
        let tower = Vec3::ZERO;
        let m = PlayerModifiers::default();
        let p = Vec3::new(18.0, 1.6, 0.0);
        let clean = compute_signal_at(p, tower, &[], 0, &m);
        let fiend = Enemy::new(EnemyKind::JammerFiend, 1);
        let imp = Enemy::new(EnemyKind::Imp, 1);
        let ft = GlobalTransform::from_translation(p + Vec3::new(2.0, -1.6, 0.0));
        let it = GlobalTransform::from_translation(p);
        let fields = fiend_jammer_fields([(&fiend, &ft), (&imp, &it)].into_iter());
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].radius, FIEND_FIELD_RADIUS);
        let jammed = compute_signal_at(p, tower, &fields, 0, &m);
        assert!(jammed.raw_strength < clean.raw_strength - 1.5, "{} vs {}", jammed.raw_strength, clean.raw_strength);
        let mut dead = fiend.clone();
        dead.hp = 0.0;
        assert!(fiend_jammer_fields([(&dead, &ft)].into_iter()).is_empty());
    }

    #[test]
    fn stats_scale_with_floor_and_baron_is_tanky() {
        for k in [EnemyKind::Imp, EnemyKind::Cacodemon, EnemyKind::JammerFiend, EnemyKind::HellBaron] {
            assert!(k.stats(5).hp > k.stats(1).hp);
            assert!(!k.stats(1).hit_spheres.is_empty());
        }
        assert!(EnemyKind::HellBaron.stats(1).hp > 4.0 * EnemyKind::Cacodemon.stats(1).hp);
        assert!((EnemyKind::HellBaron.stats(5).fire_cooldown - 2.4).abs() < 1e-5, "standard floors unchanged");
        for floor in [10u32, 18, 29, 35, 200] {
            for k in [EnemyKind::Imp, EnemyKind::Cacodemon, EnemyKind::JammerFiend, EnemyKind::HellBaron] {
                let st = k.stats(floor);
                assert!(st.fire_cooldown >= 1.0 && st.fire_cooldown.is_finite(), "{k:?} floor {floor}");
                assert!(st.hp.is_finite() && st.hp > 0.0);
            }
        }
        assert!(garrison_fiends(40) <= 5 && garrison_size(40) <= 16);
    }

    #[test]
    fn garrison_is_far_reachable_and_scales() {
        let mut rng = StdRng::seed_from_u64(3);
        for floor in 1..=5 {
            let g = generate_level(floor, 77 + floor as u64);
            let plan = garrison_plan(&g, &mut rng);
            assert!(plan.len() >= garrison_size(floor) - 2, "floor {floor}: {}", plan.len());
            let d = g.walk_distances(g.spawn);
            for (_, c) in &plan {
                assert!(d[(c.y * g.width + c.x) as usize].unwrap() >= 7);
            }
            let fiends = plan.iter().filter(|(k, _)| *k == EnemyKind::JammerFiend).count();
            assert_eq!(fiends, garrison_fiends(floor));
        }
        assert!(garrison_size(5) > garrison_size(1));
    }

    #[test]
    fn flow_field_leads_to_target() {
        let g = LevelGrid::from_ascii(&[
            "#######", //
            "#.....#", //
            "#.###.#", //
            "#.#...#", //
            "#######",
        ]);
        let target = IVec2::new(3, 3);
        let f = FlowField::build(&g, target);
        let mut c = IVec2::new(1, 3);
        for _ in 0..20 {
            match f.next_step(&g, c, false) {
                Some(n) => c = n,
                None => break,
            }
        }
        assert_eq!(c, target);
    }

    #[test]
    fn decoy_lures_only_nearby_imps() {
        let d = Some(Vec3::new(10.0, 0.25, 0.0));
        assert_eq!(decoy_lure(EnemyKind::Imp, Vec3::ZERO, d), d);
        assert_eq!(decoy_lure(EnemyKind::Imp, Vec3::ZERO, None), None);
        assert_eq!(decoy_lure(EnemyKind::Imp, Vec3::new(40.0, 0.0, 0.0), d), None);
        assert_eq!(decoy_lure(EnemyKind::Cacodemon, Vec3::ZERO, d), None);
        assert_eq!(decoy_lure(EnemyKind::HellBaron, Vec3::ZERO, d), None);
    }

    #[test]
    fn segment_point_distance_works() {
        let d = segment_point_distance(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(5.0, 2.0, 0.0));
        assert!((d - 2.0).abs() < 1e-5);
        let d = segment_point_distance(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(-3.0, 0.0, 0.0));
        assert!((d - 3.0).abs() < 1e-5);
    }

    #[test]
    fn phase_stalkers_enrage_in_dead_zones_like_imps() {
        assert!(should_enrage(EnemyKind::PhaseStalker, true));
        assert!(!should_enrage(EnemyKind::PhaseStalker, false));
        for k in [EnemyKind::RelaySniper, EnemyKind::CarrierDaemon, EnemyKind::HellBaron, EnemyKind::JammerFiend] {
            assert!(!should_enrage(k, true), "{k:?}");
        }
        let mut st = Enemy::new(EnemyKind::PhaseStalker, 2);
        st.alerted = true;
        st.sees_player = true;
        st.blink_cooldown = 0.0;
        assert!(!should_start_blink(&st, 10.0), "calm (outside a Dead Zone): no blink");
        st.enraged = true;
        assert!(should_start_blink(&st, 10.0));
        st.blink_cooldown = 0.5;
        assert!(!should_start_blink(&st, 10.0), "cooldown gates the blink");
        let s = EnemyKind::PhaseStalker.stats(3);
        assert_eq!((s.hp, s.speed, s.proj_speed), (115.0, 6.4, 20.0));
        let r = EnemyKind::RelaySniper.stats(3);
        assert_eq!((r.proj_speed, r.proj_damage, r.range), (44.0, 21.0, 44.0));
    }

    #[test]
    fn phase_blink_targets_are_walkable_flanks_away_from_walls_and_pits() {
        let g = LevelGrid::from_ascii(&[
            "##########",
            "#........#",
            "#..~~~...#",
            "#..~~~...#",
            "#........#",
            "#........#",
            "#........#",
            "##########",
        ]);
        let enemy = g.cell_center(IVec2::new(4, 5)) + Vec3::Y * 0.05;
        let player = g.cell_center(IVec2::new(4, 1));
        for side in [1.0f32, -1.0] {
            let p = pick_phase_blink_target(&g, enemy, player, side).expect("open room has a flank");
            let flat = Vec2::new(p.x - enemy.x, p.z - enemy.z).length();
            assert!((PHASE_BLINK_MIN - 0.01..=PHASE_BLINK_MAX + 0.01).contains(&flat), "{flat}");
            assert!(standable(&g, p, 0.5).is_some(), "{p}");
            let c = g.world_to_cell(p);
            assert!(matches!(g.cell(c), Cell::Floor { .. }), "{c}");
            assert!(Vec2::new(p.x - player.x, p.z - player.z).length() >= 2.5);
            // Sideways onto the requested flank (player axis is -Z, so +side = +X).
            assert!((p.x - enemy.x) * side > 3.0, "side {side}: {p}");
            assert!(p.z < enemy.z + 0.1, "never retreats away from the player");
        }
        // Boxed in by walls: nowhere to blink.
        let boxed = LevelGrid::from_ascii(&["###", "#.#", "###"]);
        let e = boxed.cell_center(IVec2::new(1, 1));
        assert_eq!(pick_phase_blink_target(&boxed, e, e + Vec3::new(0.0, 0.0, -20.0), 1.0), None);
        // A pit-ringed perch: never into the pit or out of bounds.
        let pits = LevelGrid::from_ascii(&["~~~~~~~", "~~~~~~~", "~~~.~~~", "~~~~~~~", "~~~~~~~"]);
        let e = pits.cell_center(IVec2::new(3, 2));
        assert_eq!(pick_phase_blink_target(&pits, e, e + Vec3::new(0.0, 0.0, -12.0), -1.0), None);
    }

    #[test]
    fn blink_targets_on_generated_floors_are_always_valid() {
        let mut checked = 0;
        for floor in 2..=5 {
            let g = generate_level(floor, 900 + floor as u64);
            for z in (1..g.depth - 1).step_by(3) {
                for x in (1..g.width - 1).step_by(3) {
                    let c = IVec2::new(x, z);
                    if !matches!(g.cell(c), Cell::Floor { .. } | Cell::AtriumFloor) {
                        continue;
                    }
                    let pos = g.cell_center(c);
                    let player = pos + Vec3::new(9.0, 0.0, 3.0);
                    if let Some(p) = pick_phase_blink_target(&g, pos, player, 1.0) {
                        let pc = g.world_to_cell(p);
                        assert!(g.in_bounds(pc) && !matches!(g.cell(pc), Cell::Wall | Cell::HazardPit), "floor {floor}: {c} -> {pc}");
                        assert!(has_line_of_sight(&g, pos + Vec3::Y * 1.2, p + Vec3::Y * 1.2));
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 20, "{checked}");
    }

    #[test]
    fn relay_snipers_prefer_catwalk_perches_and_long_sightlines() {
        let g = LevelGrid::from_ascii(&[
            "###########",
            "#^^^......#",
            "#....#....#",
            "#.........#",
            "###########",
        ]);
        let cat = IVec2::new(2, 1);
        let open = IVec2::new(1, 3);
        let tight = IVec2::new(6, 2);
        assert!(sniper_perch_rank(&g, cat) < sniper_perch_rank(&g, open));
        assert!(sightline_cells(&g, open) > sightline_cells(&g, tight));
        assert!(sniper_perch_rank(&g, open) < sniper_perch_rank(&g, tight));
        let lead = sniper_lead_aim(Vec3::ZERO, Vec3::new(0.0, 0.0, -44.0), Vec3::new(10.0, 5.0, 0.0), 44.0);
        assert!((lead - Vec3::new(5.0, 0.0, -44.0)).length() < 1e-4, "half-lead on horizontal motion: {lead}");
    }

    #[test]
    fn garrisons_add_stalkers_and_catwalk_snipers_from_floor_two() {
        assert_eq!((garrison_stalkers(1), garrison_snipers(1)), (0, 0));
        assert!(garrison_stalkers(2) >= 1 && garrison_snipers(2) >= 1);
        assert!(garrison_stalkers(40) <= 3 && garrison_snipers(40) <= 3);
        let mut rng = StdRng::seed_from_u64(9);
        let g1 = generate_level(1, 70);
        let plan1 = garrison_plan(&g1, &mut rng);
        assert!(plan1.iter().all(|(k, _)| matches!(k, EnemyKind::Imp | EnemyKind::Cacodemon | EnemyKind::JammerFiend)), "Floor 1 roster untouched");
        let mut catwalk_snipers = 0;
        for floor in 2..=5 {
            for seed in [70u64, 71, 72] {
                let g = generate_level(floor, seed);
                let plan = garrison_plan(&g, &mut rng);
                let stalkers = plan.iter().filter(|(k, _)| *k == EnemyKind::PhaseStalker).count();
                let snipers: Vec<IVec2> = plan.iter().filter(|(k, _)| *k == EnemyKind::RelaySniper).map(|(_, c)| *c).collect();
                assert_eq!(stalkers, garrison_stalkers(floor), "floor {floor}");
                assert_eq!(snipers.len(), garrison_snipers(floor), "floor {floor}");
                assert!(plan.len() <= garrison_size(floor));
                // Best perches first: no unused catwalk cell outranks a chosen one.
                let d = g.walk_distances(g.spawn);
                for c in &snipers {
                    assert!(d[(c.y * g.width + c.x) as usize].unwrap() >= 7);
                    if matches!(g.cell(*c), Cell::Floor { height } if height > 1.0) {
                        catwalk_snipers += 1;
                    }
                }
            }
        }
        assert!(catwalk_snipers > 0, "snipers take elevated catwalk perches");
    }

    #[test]
    fn new_kinds_have_tables_and_scale() {
        assert_eq!(EnemyKind::ALL.len(), KIND_COUNT);
        for (i, k) in EnemyKind::ALL.into_iter().enumerate() {
            assert_eq!(k.index(), i);
            assert!(!k.stats(2).hit_spheres.is_empty());
        }
        for k in [EnemyKind::PhaseStalker, EnemyKind::RelaySniper] {
            assert!(k.stats(5).hp > k.stats(2).hp);
            assert!(!k.flying());
        }
        assert!(EnemyKind::CarrierDaemon.stats(5).hp > EnemyKind::HellBaron.stats(5).hp);
        assert!(EnemyKind::CarrierDaemon.stats(10).hp > EnemyKind::HellBaron.stats(10).hp);
    }

    #[test]
    fn hp_floor_clamps_damage_and_shield_blocks() {
        let mods = PlayerModifiers::default();
        let mut e = Enemy::new(EnemyKind::Imp, 1);
        e.hp_floor = 20.0;
        let dealt = damage_enemy(&mut e, 1000.0, false, &mods);
        assert_eq!(e.hp, 20.0);
        assert_eq!(dealt, e.max_hp - 20.0);
        e.shielded = true;
        assert_eq!(damage_enemy(&mut e, 10.0, false, &mods), 0.0);
        assert_eq!((e.hp, e.shield_hits), (20.0, 1));
    }

    #[test]
    fn hit_flash_preserves_enraged_marked_and_stunned_tint() {
        let base = LinearRgba::rgb(0.1, 0.0, 0.0);
        let state = LOOK_ENRAGED | LOOK_MARKED | LOOK_STUNNED;
        let pulse = 0.75;
        let steady = body_state_emissive(base, state, pulse);
        let last_flash = body_flash_emissive(base, state, pulse, FLASH_STEPS - 1);
        let plain_last_flash = body_flash_emissive(base, 0, pulse, FLASH_STEPS - 1);
        // Flash adds on top of the active state's emissive rather than replacing it.
        assert!(last_flash.blue > steady.blue);
        assert!(last_flash.blue > plain_last_flash.blue + 1.0);
        assert!(last_flash.green > plain_last_flash.green + 0.5);
    }

    /// World-space AABB of baked parts placed by `tf`, merged into `out`.
    fn grow_bounds(meshes: &Assets<Mesh>, parts: &[EnemyPart], tf: Transform, out: &mut (Vec3, Vec3)) {
        for part in parts {
            let m = meshes.get(&part.mesh).expect("baked mesh");
            for p in m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap() {
                let w = tf.transform_point(Vec3::from(*p));
                out.0 = out.0.min(w);
                out.1 = out.1.max(w);
            }
        }
    }

    #[test]
    fn sculpted_models_bake_few_materials_and_fit_their_hit_spheres() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(Assets::<Image>::default());
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(Assets::<StandardMaterial>::default());
        world.run_system_once(create_enemy_assets).unwrap();
        let a = world.resource::<EnemyAssets>();
        let meshes = world.resource::<Assets<Mesh>>();
        for kind in EnemyKind::ALL {
            let model = &a.models[kind.index()];
            let roles = |r: PartRole| model.iter().filter(|p| p.role == r).count();
            assert!((4..=6).contains(&model.len()), "{kind:?}: {} baked materials", model.len());
            assert_eq!(roles(PartRole::Body), 1, "{kind:?}: one tintable body part");
            assert!(roles(PartRole::Eye) <= 1, "{kind:?}: at most one eye part");
            assert!(a.source_part_counts[kind.index()] > 3 * model.len(), "{kind:?}: sculpted from many parts");
            let mut b = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            grow_bounds(meshes, model, Transform::IDENTITY, &mut b);
            for rig in &a.rigs[kind.index()] {
                assert!(!rig.parts.is_empty() && rig.parts.len() <= 3, "{kind:?}: rig {:?} has {} parts", rig.kind, rig.parts.len());
                grow_bounds(meshes, &rig.parts, rig.rest, &mut b);
            }
            let st = kind.stats(1);
            println!("{kind:?}: {} parts -> {} materials + {} rigs, bounds {:.2} .. {:.2}", a.source_part_counts[kind.index()], model.len(), a.rigs[kind.index()].len(), b.0, b.1);
            if kind.flying() {
                let r = st.hit_spheres[0].1;
                assert!(b.0.y > -r * 1.4 && b.1.y < r * 1.9, "{kind:?}: vertical extent {:.2}..{:.2}", b.0.y, b.1.y);
            } else {
                assert!(b.0.y > -0.03 && b.0.y < 0.06, "{kind:?}: feet grounded at y = 0 (min y {:.3})", b.0.y);
                assert!(b.1.y > st.height * 0.92 && b.1.y < st.height * 1.3, "{kind:?}: top {:.2} vs height {}", b.1.y, st.height);
            }
            let widest = st.hit_spheres.iter().map(|h| h.1).fold(0.0, f32::max);
            assert!(b.1.x.max(-b.0.x) < widest * 2.2 + 0.6, "{kind:?}: half-width {:.2}", b.1.x.max(-b.0.x));
        }
        // Articulated rigs where the design calls for them.
        let rig_kinds = |k: EnemyKind| a.rigs[k.index()].iter().map(|r| r.kind).collect::<Vec<_>>();
        assert_eq!(rig_kinds(EnemyKind::JammerFiend), vec![RigKind::FiendEmitter]);
        assert_eq!(a.rigs[EnemyKind::JammerFiend.index()][0].parts.len(), 1, "emitter is a single entity");
        assert_eq!(rig_kinds(EnemyKind::RelaySniper), vec![RigKind::SniperDish]);
        assert_eq!(rig_kinds(EnemyKind::CarrierDaemon), vec![RigKind::BossCannon, RigKind::BossHalo]);
        for k in [EnemyKind::Imp, EnemyKind::Cacodemon, EnemyKind::HellBaron, EnemyKind::PhaseStalker] {
            assert!(a.rigs[k.index()].is_empty(), "{k:?} stays rigid");
        }
        // Pylon obelisk: grounded, crowned above the tether origin.
        let mut b = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        grow_bounds(meshes, &a.pylon_model, Transform::IDENTITY, &mut b);
        println!("pylon: {} materials, bounds {:.2} .. {:.2}", a.pylon_model.len(), b.0, b.1);
        assert!(b.0.y.abs() < 0.02 && b.1.y > 2.6 && b.1.y < 3.4 && a.pylon_model.len() <= 6);
        assert_eq!(a.pylon_ring.parts.len(), 1);
    }

    #[test]
    fn glowing_materials_stay_lit_and_saturated() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(Assets::<Image>::default());
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(Assets::<StandardMaterial>::default());
        world.run_system_once(create_enemy_assets).unwrap();
        let a = world.resource::<EnemyAssets>();
        let mats = world.resource::<Assets<StandardMaterial>>();
        let mut glows = vec![&a.pylon_core, &a.carrier_core, &a.phase_core, &a.sniper_lens, &a.caco_iris, &a.baron_hellfire, &a.fiend_green];
        glows.extend(a.looks.iter().flat_map(|l| [&l.eye, &l.eye_enraged]));
        for h in glows {
            let m = mats.get(h).unwrap();
            // Unlit materials output only `base_color` (emissive ignored) -> chalky white.
            assert!(!m.unlit, "glow must be lit so emissive applies");
            let c = m.base_color.to_srgba();
            let (hi, lo) = (c.red.max(c.green).max(c.blue), c.red.min(c.green).min(c.blue));
            // Deep, richly saturated base (dark is fine: chroma relative to the peak channel).
            assert!(hi - lo > 0.4 && (hi - lo) / hi > 0.75, "saturated base color, got {c:?}");
            let e = m.emissive;
            let (ehi, elo) = (e.red.max(e.green).max(e.blue), e.red.min(e.green).min(e.blue));
            assert!((0.8..=8.0).contains(&ehi) && elo < ehi * 0.12, "channel-pure emissive, got {e:?}");
        }
    }

    // --- Milestone 17: procedural combat animation -------------------------

    fn pose_ok(p: BodyPose) {
        assert!(p.is_finite(), "{p:?}");
        assert!(p.pitch.abs() <= MAX_BODY_TILT + 1e-5 && p.roll.abs() <= MAX_BODY_TILT + 1e-5, "{p:?}");
        assert!(p.offset.abs().max_element() <= 0.4 + 1e-5, "{p:?}");
    }

    #[test]
    fn pose_helpers_are_finite_and_bounded_for_every_kind() {
        let dirs = [Vec2::X, Vec2::NEG_Y, Vec2::new(0.6, 0.8), Vec2::ZERO, Vec2::new(f32::NAN, 1.0)];
        for kind in EnemyKind::ALL {
            let mut e = Enemy::new(kind, 5);
            for speed in [0.0f32, 1.0, 6.0, 40.0, f32::INFINITY] {
                for phase in [0.0f32, 1.3, 7.7, 1e4] {
                    pose_ok(stride_pose(Vec2::new(0.3, -1.0) * speed, e.move_speed(), phase).bounded());
                    pose_ok(hover_pose(Vec2::new(1.0, 0.5) * speed, e.move_speed(), phase).bounded());
                }
            }
            for a in [-1.0f32, 0.0, 0.01, 0.5, 0.9, 1.0, 2.0, f32::NAN] {
                pose_ok(attack_pose(kind, a, true).bounded());
                pose_ok(attack_pose(kind, a, false).bounded());
            }
            for d in dirs {
                for t in [0.0f32, 0.01, 0.09, 0.18, 1.0, f32::NAN] {
                    pose_ok(stagger_pose(kind, d, t).bounded());
                }
            }
            // Everything at once, with nonsense velocities.
            e.velocity = Vec3::new(1e6, -3.0, f32::NAN);
            e.attack_anim = 1.0;
            e.stagger(Vec3::new(1.0, 5.0, -1.0));
            pose_ok(body_pose(&e, Quat::from_rotation_y(1.1)));
            e.velocity = Vec3::new(4.0, 0.0, -3.0);
            for _ in 0..40 {
                tick_enemy_anim(&mut e, 1.0 / 60.0);
                pose_ok(body_pose(&e, Quat::from_rotation_y(0.4)));
            }
            assert_eq!(e.attack_anim, 0.0);
            assert_eq!(e.stagger_timer, 0.0);
            assert!(e.stride_phase.is_finite() && (0.0..TAU).contains(&e.stride_phase));
        }
    }

    #[test]
    fn stagger_tips_body_along_the_hit_and_heavies_stagger_less() {
        // Hit travelling local +Z (shot from the front): the top tips back (+pitch).
        let peak = |kind: EnemyKind, dir: Vec2| {
            let total = stagger_secs(kind);
            (0..=40).map(|i| stagger_pose(kind, dir, total * (1.0 - i as f32 / 40.0))).fold(BodyPose::default(), |m, p| {
                BodyPose { offset: m.offset, pitch: if p.pitch.abs() > m.pitch.abs() { p.pitch } else { m.pitch }, roll: if p.roll.abs() > m.roll.abs() { p.roll } else { m.roll } }
            })
        };
        let back = peak(EnemyKind::Imp, Vec2::Y);
        assert!(back.pitch > 0.2 && back.roll.abs() < 1e-6, "{back:?}");
        // Hit travelling local +X: the top tips toward +X (negative roll about Z).
        let side = peak(EnemyKind::Imp, Vec2::X);
        assert!(side.roll < -0.2 && side.pitch.abs() < 1e-6, "{side:?}");
        let imp = peak(EnemyKind::Imp, Vec2::Y).pitch;
        let baron = peak(EnemyKind::HellBaron, Vec2::Y).pitch;
        let boss = peak(EnemyKind::CarrierDaemon, Vec2::Y).pitch;
        assert!(baron < imp * 0.6 && boss < baron, "imp {imp} baron {baron} boss {boss}");
        assert!(stagger_secs(EnemyKind::HellBaron) < stagger_secs(EnemyKind::Imp));
        assert!(stagger_secs(EnemyKind::CarrierDaemon) < stagger_secs(EnemyKind::HellBaron));
        assert!((stagger_secs(EnemyKind::Imp) - STAGGER_SECS).abs() < 1e-6);
        // Fast in, slow out: peak well before the midpoint.
        let total = stagger_secs(EnemyKind::Imp);
        let at = |u: f32| stagger_pose(EnemyKind::Imp, Vec2::Y, total * (1.0 - u)).pitch;
        assert!(at(0.2) > at(0.6) && at(0.6) > at(0.95) && at(0.05) > 0.0);
        // The world-space hit is rotated into the demon's local frame.
        let mut e = Enemy::new(EnemyKind::Imp, 1);
        e.stagger(Vec3::X * 3.0);
        assert_eq!(e.stagger_dir, Vec2::X);
        e.stagger_timer = total * 0.8;
        let p = body_pose(&e, Quat::from_rotation_y(FRAC_PI_2));
        // Facing rotated 90 deg about Y: world +X is local +Z -> pitch back.
        assert!(p.pitch > 0.1 && p.roll.abs() < 0.08, "{p:?}");
    }

    #[test]
    fn stagger_ignores_vertical_hits_and_corpses() {
        let mut e = Enemy::new(EnemyKind::Imp, 1);
        e.stagger(Vec3::Y);
        assert_eq!(e.stagger_timer, 0.0);
        e.hp = 0.0;
        e.stagger(Vec3::X);
        assert_eq!(e.stagger_timer, 0.0);
    }

    #[test]
    fn stride_leans_into_the_run_and_hover_banks_into_velocity() {
        let idle = stride_pose(Vec2::ZERO, 6.0, 1.0);
        assert!(idle.pitch == 0.0 && idle.roll == 0.0 && idle.offset.y.abs() < 0.02);
        let run = stride_pose(Vec2::new(0.0, -6.0), 6.0, FRAC_PI_2);
        assert!(run.pitch < -0.05, "forward lean {run:?}");
        assert!(run.offset.y > 0.06 && run.roll > 0.03, "stride bob / hip sway {run:?}");
        // Bob scales with speed.
        assert!(stride_pose(Vec2::new(0.0, -3.0), 6.0, FRAC_PI_2).offset.y < run.offset.y);
        let right = hover_pose(Vec2::new(5.0, 0.0), 5.0, 0.0);
        assert!(right.roll < -0.1, "banks into +X drift {right:?}");
        let fwd = hover_pose(Vec2::new(0.0, -5.0), 5.0, 0.0);
        assert!(fwd.pitch < -0.1, "dips into forward flight {fwd:?}");
    }

    #[test]
    fn attack_pose_recoils_back_or_lunges_forward_and_rigs_kick() {
        let shot = attack_pose(EnemyKind::Imp, 0.85, false);
        assert!(shot.pitch > 0.1 && shot.offset.z > 0.03, "{shot:?}");
        let lunge = attack_pose(EnemyKind::Imp, 0.85, true);
        assert!(lunge.pitch < -0.1 && lunge.offset.z < -0.05, "{lunge:?}");
        assert_eq!(attack_pose(EnemyKind::Imp, 0.0, false), BodyPose::default());
        let mut e = Enemy::new(EnemyKind::RelaySniper, 3);
        assert_eq!(rig_recoil(&e), 0.0);
        e.trigger_attack_anim(false);
        tick_enemy_anim(&mut e, 0.02);
        let k0 = rig_recoil(&e);
        assert!(k0 > 0.5, "snappy kick right after firing: {k0}");
        for _ in 0..30 {
            tick_enemy_anim(&mut e, 1.0 / 60.0);
        }
        assert_eq!(rig_recoil(&e), 0.0);
        e.trigger_attack_anim(true);
        assert_eq!(rig_recoil(&e), 0.0, "lunges never kick the cannon / dish rigs");
    }

    #[test]
    fn death_gibs_and_decals_cover_every_archetype() {
        for kind in EnemyKind::ALL {
            let (mats, n) = death_gibs(kind);
            assert!(!mats.is_empty() && (4..=16).contains(&n));
            assert!(death_decals(kind, false)[0].is_some());
            assert_eq!(death_decals(kind, true)[0].unwrap().0, crate::decals::DecalKind::Scorch);
        }
        assert_eq!(death_decals(EnemyKind::JammerFiend, false)[0].unwrap().0, crate::decals::DecalKind::IchorGreen);
        assert_eq!(death_decals(EnemyKind::PhaseStalker, false)[0].unwrap().0, crate::decals::DecalKind::IchorViolet);
        assert!(death_decals(EnemyKind::HellBaron, false)[1].is_some() && death_decals(EnemyKind::CarrierDaemon, false)[1].is_some());
    }

    #[test]
    fn wide_lane_sight_aggro_reaches_36_m_but_needs_line_of_sight() {
        const { assert!(SIGHT_AGGRO_DIST < SIGHT_MAX_DIST) };
        assert!(wakes_on_senses(true, 32.0), "visible down a 32 m corridor");
        assert!(wakes_on_senses(true, 35.9));
        assert!(!wakes_on_senses(true, 36.5), "past the aggro range");
        assert!(!wakes_on_senses(false, 20.0), "wall-screened stays idle");
        assert!(wakes_on_senses(false, 7.0), "close enough to hear");
    }
}
