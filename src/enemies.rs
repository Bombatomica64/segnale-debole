//! The demon archetypes: Static Imp, Dead-Zone Cacodemon, Jammer Fiend, Hell
//! Baron, Phase Stalker, Catwalk Relay Sniper and the Carrier Daemon boss. Multi-part procedural meshes, flow-field chasing over the
//! `LevelGrid`, projectiles, Dead-Zone enrage, mobile jammer fields, hit-flash,
//! Ping marks and death gibs/drops.

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::prelude::*;
use rand::Rng;
use rand::seq::SliceRandom;

use crate::audio::{SfxEvent, SfxQueue};
use crate::fx::{spawn_burst, spawn_flash_light, BurstColor, FxAssets, HudFeed};
use crate::apps::spawn_hijack_hotspot;
use crate::level::{Cell, CullableLight, DemonJammer, LevelEntity, LevelGrid, RampDir, SignalTower, CEILING_HEIGHT};
use crate::mesh_batch::MaterialBatches;
use crate::physics::{count_wall_occlusions, has_line_of_sight, raycast_grid, resolve_player_movement, sample_floor_height, PLAYER_RADIUS};
use crate::player::{Player, PLAYER_EYE_HEIGHT};
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
                hit_spheres: &[(0.55, 0.42), (1.15, 0.48), (1.65, 0.34)],
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
                hit_spheres: &[(0.6, 0.4), (1.25, 0.46), (1.8, 0.32)],
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
                hit_spheres: &[(0.7, 0.5), (1.55, 0.55), (2.35, 0.42)],
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
        }
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
    pub caco_iris: Handle<StandardMaterial>,
    pub caco_maw: Handle<StandardMaterial>,
    pub fiend_green: Handle<StandardMaterial>,
    pub baron_gold: Handle<StandardMaterial>,
    pub ping_ring: Handle<StandardMaterial>,
    pub enrage_halo: Handle<StandardMaterial>,
    pub fiend_ring: Handle<StandardMaterial>,
    /// Phase Stalker obsidian-violet plating + glowing magenta phase core.
    pub phase_obsidian: Handle<StandardMaterial>,
    pub phase_core: Handle<StandardMaterial>,
    /// Relay Sniper crimson scope lens / rail glow.
    pub sniper_lens: Handle<StandardMaterial>,
    /// Relay Sniper lock-on laser sight (additive crimson rod).
    pub sniper_laser: Handle<StandardMaterial>,
    /// Carrier Daemon pulsing crimson-violet Signal Core reactor.
    pub carrier_core: Handle<StandardMaterial>,
    /// Translucent cyan-violet Signal Shield shell (boss, while shielded).
    pub boss_shield: Handle<StandardMaterial>,
    /// Cyan-violet tether beam from a Shield Pylon to the boss.
    pub boss_tether: Handle<StandardMaterial>,
    /// Shield Pylon glowing emitter.
    pub pylon_core: Handle<StandardMaterial>,
    /// Per-kind state materials (indexed by [`EnemyKind::index`]).
    pub looks: [EnemyLookMaterials; KIND_COUNT],
    /// Per-kind models: rigid parts merged per material (~5 per demon).
    pub models: [Vec<EnemyPart>; KIND_COUNT],
    /// Rigid parts each model was baked from (one entity each before batching).
    pub source_part_counts: [usize; KIND_COUNT],
    /// Pre-baked Shield Pylon model (all `PartRole::Static`).
    pub pylon_model: Vec<EnemyPart>,
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

fn solid(r: f32, g: f32, b: f32, rough: f32, metal: f32) -> StandardMaterial {
    StandardMaterial { base_color: Color::srgb(r, g, b), perceptual_roughness: rough, metallic: metal, ..default() }
}

fn eye(e: LinearRgba) -> StandardMaterial {
    StandardMaterial { base_color: Color::WHITE, emissive: e, unlit: true, ..default() }
}

fn create_enemy_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let mut caco = solid(0.42, 0.025, 0.10, 0.42, 0.05);
    caco.emissive = LinearRgba::rgb(0.14, 0.0, 0.03);
    let mut caco_maw = solid(0.16, 0.01, 0.04, 0.5, 0.0);
    caco_maw.emissive = LinearRgba::rgb(0.6, 0.02, 0.12);
    let mut green = solid(0.28, 0.65, 0.12, 0.4, 0.25);
    green.emissive = LinearRgba::rgb(0.35, 1.8, 0.08);
    let mut gold = solid(0.95, 0.68, 0.16, 0.28, 0.85);
    gold.emissive = LinearRgba::rgb(1.1, 0.55, 0.05);
    let mut ping = crate::fx::additive_glow(Color::srgb(0.25, 1.0, 1.0), 0.92);
    ping.emissive = LinearRgba::rgb(1.5, 7.0, 9.0);
    let mut enrage_halo = crate::fx::additive_glow(Color::srgb(1.0, 0.22, 0.95), 0.85);
    enrage_halo.emissive = LinearRgba::rgb(6.0, 0.8, 7.5);
    let mut fiend_ring = crate::fx::additive_glow(Color::srgb(0.35, 1.0, 0.18), 0.72);
    fiend_ring.emissive = LinearRgba::rgb(1.0, 5.5, 0.4);
    let mut phase_obsidian = solid(0.07, 0.03, 0.1, 0.25, 0.7);
    phase_obsidian.emissive = LinearRgba::rgb(0.08, 0.0, 0.14);
    let mut sniper_laser = crate::fx::additive_glow(Color::srgb(1.0, 0.12, 0.12), 0.85);
    sniper_laser.emissive = LinearRgba::rgb(14.0, 0.6, 0.5);
    let mut boss_shield = crate::fx::additive_glow(Color::srgb(0.45, 0.6, 1.0), 0.28);
    boss_shield.emissive = LinearRgba::rgb(1.2, 1.8, 4.2);
    let mut boss_tether = crate::fx::additive_glow(Color::srgb(0.5, 0.8, 1.0), 0.9);
    boss_tether.emissive = LinearRgba::rgb(3.0, 6.0, 12.0);

    // Per-kind body / eye templates -> shared state material tables.
    let templates = [
        (solid(0.32, 0.055, 0.028, 0.72, 0.0), eye(LinearRgba::rgb(18.0, 5.5, 0.35))),
        (caco, eye(LinearRgba::rgb(3.5, 14.0, 1.4))),
        (solid(0.15, 0.17, 0.18, 0.4, 0.78), eye(LinearRgba::rgb(2.0, 15.0, 1.2))),
        (solid(0.38, 0.055, 0.028, 0.65, 0.05), eye(LinearRgba::rgb(18.0, 12.5, 1.2))),
        // Phase Stalker: gaunt obsidian-violet wraith, razor static visor.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.12, 0.0, 0.2), ..solid(0.16, 0.06, 0.22, 0.3, 0.55) },
            eye(LinearRgba::rgb(10.0, 2.0, 16.0)),
        ),
        // Relay Sniper: gunmetal industrial sentinel, crimson optic.
        (solid(0.22, 0.2, 0.19, 0.38, 0.82), eye(LinearRgba::rgb(20.0, 1.2, 0.8))),
        // Carrier Daemon: blackened iron chassis, molten-gold eyes.
        (
            StandardMaterial { emissive: LinearRgba::rgb(0.1, 0.0, 0.03), ..solid(0.12, 0.05, 0.05, 0.45, 0.7) },
            eye(LinearRgba::rgb(20.0, 9.0, 1.0)),
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
            eye_enraged: materials.add(StandardMaterial { emissive: LinearRgba::rgb(16.0, 4.0, 20.0), ..eye_tpl }),
        }
    });

    // Source primitives (identical to the shared mesh handles below).
    let src = [
        Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
        Sphere::new(1.0).mesh().uv(20, 14),
        Mesh::from(Capsule3d::new(0.5, 1.0)),
        Mesh::from(Cone::new(1.0, 1.0)),
        Mesh::from(Cylinder::new(1.0, 1.0)),
        Mesh::from(Torus::new(0.8, 1.0)),
    ];
    let mut a = EnemyAssets {
        cube: meshes.add(src[0].clone()),
        sphere: meshes.add(src[1].clone()),
        capsule: meshes.add(src[2].clone()),
        cone: meshes.add(src[3].clone()),
        cylinder: meshes.add(src[4].clone()),
        ring: meshes.add(src[5].clone()),
        bone: materials.add(solid(0.76, 0.68, 0.52, 0.55, 0.0)),
        horn: materials.add(solid(0.08, 0.045, 0.04, 0.48, 0.15)),
        dark: materials.add(solid(0.035, 0.01, 0.012, 0.88, 0.05)),
        metal_plate: materials.add(solid(0.24, 0.26, 0.28, 0.36, 0.85)),
        imp_dark: materials.add(solid(0.09, 0.025, 0.018, 0.85, 0.05)),
        caco_iris: materials.add(eye(LinearRgba::rgb(0.5, 7.0, 12.0))),
        caco_maw: materials.add(caco_maw),
        fiend_green: materials.add(green),
        baron_gold: materials.add(gold),
        ping_ring: materials.add(ping),
        enrage_halo: materials.add(enrage_halo),
        fiend_ring: materials.add(fiend_ring),
        phase_obsidian: materials.add(phase_obsidian),
        phase_core: materials.add(eye(LinearRgba::rgb(9.0, 0.8, 12.0))),
        sniper_lens: materials.add(eye(LinearRgba::rgb(22.0, 1.5, 1.0))),
        sniper_laser: materials.add(sniper_laser),
        carrier_core: materials.add(eye(LinearRgba::rgb(14.0, 1.0, 9.0))),
        boss_shield: materials.add(boss_shield),
        boss_tether: materials.add(boss_tether),
        pylon_core: materials.add(eye(LinearRgba::rgb(4.0, 7.0, 16.0))),
        looks,
        models: Default::default(),
        source_part_counts: [0; KIND_COUNT],
        pylon_model: Vec::new(),
    };
    for kind in EnemyKind::ALL {
        a.source_part_counts[kind.index()] = build_parts(kind, &a, &a.looks[kind.index()].body[0], &a.looks[kind.index()].eye).len();
        a.models[kind.index()] = bake_model(kind, &a, &src, &mut meshes);
    }
    a.pylon_model = bake_parts(pylon_parts(&a), &a, &src, &mut meshes, None);
    commands.insert_resource(a);
}

/// Shield Pylon: armored base, glowing emitter column, crown ring + spikes.
fn pylon_parts(a: &EnemyAssets) -> Vec<Part> {
    let id = Quat::IDENTITY;
    let (core, mp, dark) = (a.pylon_core.clone(), a.metal_plate.clone(), a.dark.clone());
    let mut p: Vec<Part> = vec![
        (a.cylinder.clone(), mp.clone(), Vec3::new(0.0, 0.2, 0.0), Vec3::new(0.85, 0.4, 0.85), id),
        (a.cylinder.clone(), dark.clone(), Vec3::new(0.0, 1.2, 0.0), Vec3::new(0.32, 1.9, 0.32), id),
        (a.sphere.clone(), core.clone(), Vec3::new(0.0, 1.6, 0.0), Vec3::new(0.38, 0.62, 0.38), id),
        (a.ring.clone(), a.boss_tether.clone(), Vec3::new(0.0, 2.55, 0.0), Vec3::new(0.55, 0.12, 0.55), id),
        (a.sphere.clone(), core, Vec3::new(0.0, 2.6, 0.0), Vec3::splat(0.26), id),
    ];
    for k in 0..4 {
        let ang = k as f32 * FRAC_PI_2;
        let o = Vec3::new(ang.cos(), 0.0, ang.sin());
        p.push((a.cube.clone(), mp.clone(), o * 0.62 + Vec3::Y * 0.65, Vec3::new(0.18, 0.9, 0.18), id));
        p.push((a.cone.clone(), a.horn.clone(), o * 0.45 + Vec3::Y * 2.95, Vec3::new(0.08, 0.5, 0.08), Quat::from_rotation_arc(Vec3::Y, (o * 0.5 + Vec3::Y).normalize())));
    }
    p
}

/// Merge a kind's rigid parts per material into a few baked meshes (same
/// placement under the body pivot, same UVs / normals).
fn bake_model(kind: EnemyKind, a: &EnemyAssets, src: &[Mesh; 6], meshes: &mut Assets<Mesh>) -> Vec<EnemyPart> {
    let look = a.look(kind);
    bake_parts(build_parts(kind, a, &look.body[0], &look.eye), a, src, meshes, Some(look))
}

/// Merge rigid parts per material (body / eye roles from `look`, if any).
fn bake_parts(parts: Vec<Part>, a: &EnemyAssets, src: &[Mesh; 6], meshes: &mut Assets<Mesh>, look: Option<&EnemyLookMaterials>) -> Vec<EnemyPart> {
    let handles = [&a.cube, &a.sphere, &a.capsule, &a.cone, &a.cylinder, &a.ring];
    let mut batches = MaterialBatches::new();
    for (mesh, mat, t, s, r) in parts {
        let i = handles.iter().position(|h| h.id() == mesh.id()).expect("demon part uses a shared primitive");
        batches.add(&src[i], &mat, Transform::from_translation(t).with_scale(s).with_rotation(r));
    }
    batches
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

/// Mesh part: (mesh, material, translation, scale, rotation).
type Part = (Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3, Quat);

fn rz(a: f32) -> Quat {
    Quat::from_rotation_z(a)
}

fn rx(a: f32) -> Quat {
    Quat::from_rotation_x(a)
}

fn build_parts(kind: EnemyKind, a: &EnemyAssets, body: &Handle<StandardMaterial>, eye: &Handle<StandardMaterial>) -> Vec<Part> {
    let (cube, sph, cap, cone, cyl, ring) = (
        a.cube.clone(),
        a.sphere.clone(),
        a.capsule.clone(),
        a.cone.clone(),
        a.cylinder.clone(),
        a.ring.clone(),
    );
    let b = body.clone();
    let e = eye.clone();
    let id = Quat::IDENTITY;
    let mut p: Vec<Part> = Vec::new();
    match kind {
        EnemyKind::Imp => {
            let d = a.imp_dark.clone();
            // Sculpted muscular hunched torso + rib/chest plates + waist.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 1.15, -0.02), Vec3::new(0.56, 0.36, 0.36), rx(0.14)));
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 1.32, -0.06), Vec3::new(0.34, 0.24, 0.24), id));
            p.push((cap.clone(), d.clone(), Vec3::new(0.0, 0.84, 0.02), Vec3::new(0.42, 0.22, 0.28), id));
            for k in 0..3 {
                let y = 1.02 + k as f32 * 0.14;
                p.push((cube.clone(), d.clone(), Vec3::new(0.0, y, -0.19), Vec3::new(0.34 - k as f32 * 0.03, 0.045, 0.06), id));
                p.push((cone.clone(), a.bone.clone(), Vec3::new(0.0, y + 0.05, 0.21), Vec3::new(0.055, 0.22, 0.055), rx(1.25)));
            }
            // Digitigrade legs (thigh + angled shin + dark hooves) & muscular clawed arms + curved two-segment horns.
            for s in [-1.0f32, 1.0] {
                // Leg
                p.push((cap.clone(), b.clone(), Vec3::new(0.18 * s, 0.58, -0.05), Vec3::new(0.19, 0.26, 0.20), rx(-0.26)));
                p.push((cap.clone(), d.clone(), Vec3::new(0.18 * s, 0.26, 0.02), Vec3::new(0.15, 0.26, 0.16), rx(0.2)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.18 * s, 0.06, -0.06), Vec3::new(0.11, 0.12, 0.14), id));
                // Shoulder + upper arm + forearm + splayed bone claws
                p.push((sph.clone(), b.clone(), Vec3::new(0.36 * s, 1.38, -0.04), Vec3::splat(0.15), id));
                p.push((cap.clone(), b.clone(), Vec3::new(0.45 * s, 1.15, -0.08), Vec3::new(0.13, 0.22, 0.13), rz(0.28 * s) * rx(0.25)));
                p.push((cap.clone(), d.clone(), Vec3::new(0.48 * s, 0.88, -0.18), Vec3::new(0.11, 0.20, 0.11), rz(-0.1 * s) * rx(0.55)));
                for c_off in [-0.045f32, 0.0, 0.045] {
                    p.push((
                        cone.clone(),
                        a.bone.clone(),
                        Vec3::new(0.48 * s + c_off, 0.72, -0.25),
                        Vec3::new(0.028, 0.18, 0.028),
                        rx(PI - 0.35),
                    ));
                }
                // Swept two-segment dark horns + recessed glowing eyes
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.14 * s, 1.84, 0.0), Vec3::new(0.065, 0.28, 0.065), rz(-0.55 * s) * rx(-0.2)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.25 * s, 2.02, 0.06), Vec3::new(0.04, 0.22, 0.04), rz(-0.2 * s) * rx(-0.45)));
                p.push((sph.clone(), e.clone(), Vec3::new(0.075 * s, 1.67, -0.20), Vec3::new(0.05, 0.036, 0.035), id));
            }
            // Sculpted demonic skull + brow ridge + fanged jaw.
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 1.65, -0.06), Vec3::new(0.21, 0.22, 0.22), id));
            p.push((cube.clone(), d.clone(), Vec3::new(0.0, 1.74, -0.19), Vec3::new(0.26, 0.055, 0.10), rx(0.2)));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 1.53, -0.21), Vec3::new(0.19, 0.06, 0.08), id));
            for k in 0..3 {
                p.push((cone.clone(), a.bone.clone(), Vec3::new(-0.06 + k as f32 * 0.06, 1.54, -0.24), Vec3::new(0.018, 0.055, 0.018), rx(PI)));
            }
        }
        EnemyKind::Cacodemon => {
            // Iconic ridged crimson floating sphere + brow crest + side jowls.
            p.push((sph.clone(), b.clone(), Vec3::ZERO, Vec3::new(0.92, 0.88, 0.90), id));
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 0.36, -0.62), Vec3::new(0.56, 0.22, 0.28), rx(0.25))); // heavy brow ridge
            // Multi-layered cyclops eye: dark socket, glowing green sclera, cyan iris, slit pupil.
            p.push((sph.clone(), a.dark.clone(), Vec3::new(0.0, 0.14, -0.74), Vec3::splat(0.35), id));
            p.push((sph.clone(), e.clone(), Vec3::new(0.0, 0.14, -0.79), Vec3::splat(0.29), id));
            p.push((sph.clone(), a.caco_iris.clone(), Vec3::new(0.0, 0.14, -0.97), Vec3::splat(0.15), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 0.14, -1.09), Vec3::new(0.045, 0.19, 0.05), id));
            // Wide gaping cavern maw with glowing inner throat + upper & lower rows of fangs.
            p.push((sph.clone(), a.caco_maw.clone(), Vec3::new(0.0, -0.36, -0.64), Vec3::new(0.54, 0.24, 0.32), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, -0.36, -0.76), Vec3::new(0.76, 0.22, 0.18), id));
            for k in 0..6 {
                let x = -0.32 + k as f32 * 0.128;
                let curve_z = -0.84 + (x * x) * 0.6;
                p.push((cone.clone(), a.bone.clone(), Vec3::new(x, -0.25, curve_z), Vec3::new(0.045, 0.13, 0.045), rx(PI)));
                p.push((cone.clone(), a.bone.clone(), Vec3::new(x * 0.88, -0.47, curve_z + 0.02), Vec3::new(0.038, 0.10, 0.038), id));
            }
            // Sweeping upper horns, cheek spikes & dorsal horn crown + dangling under-tendrils.
            for s in [-1.0f32, 1.0] {
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.52 * s, 0.78, -0.05), Vec3::new(0.15, 0.58, 0.15), rz(-0.55 * s)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.78 * s, 1.06, 0.08), Vec3::new(0.08, 0.36, 0.08), rz(-0.18 * s) * rx(-0.3)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.84 * s, 0.18, -0.08), Vec3::new(0.11, 0.40, 0.11), rz(-1.35 * s)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.74 * s, -0.32, 0.04), Vec3::new(0.09, 0.32, 0.09), rz(-2.1 * s)));
            }
            for k in 0..5 {
                let ang = -0.9 + k as f32 * 0.45;
                p.push((
                    cone.clone(),
                    a.horn.clone(),
                    Vec3::new(ang.sin() * 0.48, 0.86, 0.25 + ang.cos() * 0.22),
                    Vec3::new(0.075, 0.30, 0.075),
                    rx(-0.35),
                ));
                p.push((
                    cone.clone(),
                    b.clone(),
                    Vec3::new(ang.sin() * 0.42, -0.88, ang.cos() * 0.25),
                    Vec3::new(0.07, 0.32, 0.07),
                    rx(PI),
                ));
            }
        }
        EnemyKind::JammerFiend => {
            let g = a.fiend_green.clone();
            let mp = a.metal_plate.clone();
            // Hulking beveled armored chest + exposed glowing toxic reactor core + steel rib cage.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 1.42, 0.0), Vec3::new(0.95, 0.48, 0.62), id));
            p.push((cube.clone(), mp.clone(), Vec3::new(0.0, 1.56, -0.26), Vec3::new(0.82, 0.46, 0.22), id));
            p.push((sph.clone(), g.clone(), Vec3::new(0.0, 1.52, -0.35), Vec3::splat(0.22), id));
            p.push((ring.clone(), mp.clone(), Vec3::new(0.0, 1.52, -0.37), Vec3::new(0.26, 0.08, 0.26), rx(FRAC_PI_2)));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 0.96, 0.0), Vec3::new(0.76, 0.22, 0.50), id));
            for s in [-1.0f32, 1.0] {
                // Armored hydraulic legs + heavy boots
                p.push((cap.clone(), b.clone(), Vec3::new(0.28 * s, 0.66, 0.0), Vec3::new(0.28, 0.30, 0.30), id));
                p.push((cyl.clone(), mp.clone(), Vec3::new(0.28 * s, 0.28, 0.02), Vec3::new(0.16, 0.46, 0.16), id));
                p.push((cube.clone(), a.dark.clone(), Vec3::new(0.28 * s, 0.07, -0.05), Vec3::new(0.32, 0.14, 0.40), id));
                // Massive sloped shoulder pauldrons + glowing side vents + hydraulic arms + claw gauntlets
                p.push((sph.clone(), mp.clone(), Vec3::new(0.66 * s, 1.84, 0.0), Vec3::new(0.32, 0.24, 0.34), rz(0.2 * s)));
                p.push((cube.clone(), g.clone(), Vec3::new(0.72 * s, 1.86, -0.18), Vec3::new(0.18, 0.12, 0.08), rz(0.2 * s)));
                p.push((cap.clone(), b.clone(), Vec3::new(0.68 * s, 1.38, -0.03), Vec3::new(0.22, 0.34, 0.22), rz(0.14 * s)));
                p.push((cyl.clone(), mp.clone(), Vec3::new(0.72 * s, 0.96, -0.10), Vec3::new(0.15, 0.36, 0.15), rx(0.35)));
                p.push((cube.clone(), g.clone(), Vec3::new(0.72 * s, 0.76, -0.16), Vec3::new(0.26, 0.18, 0.26), id));
                // Cyber-visor head horns + optics
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.20 * s, 2.34, -0.02), Vec3::new(0.07, 0.32, 0.07), rz(-0.4 * s)));
                p.push((sph.clone(), e.clone(), Vec3::new(0.095 * s, 2.10, -0.24), Vec3::new(0.06, 0.04, 0.04), id));
                // Power conduits linking backpack to shoulders
                p.push((cap.clone(), g.clone(), Vec3::new(0.36 * s, 1.90, 0.26), Vec3::new(0.07, 0.34, 0.07), rx(0.95) * rz(0.4 * s)));
            }
            // Armored cyber-demon head + jaw grille
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 2.08, -0.06), Vec3::new(0.24, 0.22, 0.24), id));
            p.push((cube.clone(), mp.clone(), Vec3::new(0.0, 2.00, -0.22), Vec3::new(0.28, 0.11, 0.12), id));
            // Intricate demonic router backpack with LED rack, twin antennas, glowing emitter orbs + jammer halo ring!
            p.push((cube.clone(), mp.clone(), Vec3::new(0.0, 1.56, 0.44), Vec3::new(0.68, 0.68, 0.36), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 1.56, 0.63), Vec3::new(0.54, 0.48, 0.04), id));
            for k in 0..3 {
                p.push((cube.clone(), g.clone(), Vec3::new(0.0, 1.40 + k as f32 * 0.16, 0.66), Vec3::new(0.42, 0.05, 0.03), id));
            }
            p.push((cyl.clone(), mp.clone(), Vec3::new(0.20, 2.58, 0.48), Vec3::new(0.042, 1.6, 0.042), rz(-0.14)));
            p.push((cyl.clone(), mp.clone(), Vec3::new(-0.20, 2.38, 0.48), Vec3::new(0.042, 1.2, 0.042), rz(0.20)));
            p.push((sph.clone(), e.clone(), Vec3::new(0.31, 3.36, 0.48), Vec3::splat(0.14), id));
            p.push((sph.clone(), e.clone(), Vec3::new(-0.32, 2.96, 0.48), Vec3::splat(0.11), id));
            p.push((ring.clone(), g.clone(), Vec3::new(0.31, 3.36, 0.48), Vec3::new(0.28, 0.06, 0.28), rx(0.4)));
        }
        EnemyKind::HellBaron => {
            let gold = a.baron_gold.clone();
            // Towering muscular torso + sculpted pectoral armor + glowing molten belt + spine ridge.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 2.02, 0.0), Vec3::new(1.25, 0.68, 0.78), rx(0.08)));
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 2.34, -0.10), Vec3::new(0.72, 0.42, 0.46), id));
            p.push((cube.clone(), gold.clone(), Vec3::new(0.0, 2.18, -0.44), Vec3::new(0.96, 0.42, 0.14), id));
            p.push((cube.clone(), gold.clone(), Vec3::new(0.0, 1.38, 0.0), Vec3::new(1.06, 0.20, 0.82), id));
            p.push((sph.clone(), e.clone(), Vec3::new(0.0, 1.38, -0.44), Vec3::splat(0.14), id)); // glowing belt buckle
            for k in 0..4 {
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.0, 1.6 + k as f32 * 0.28, 0.44), Vec3::new(0.09, 0.34, 0.09), rx(1.2)));
            }
            for s in [-1.0f32, 1.0] {
                // Muscular digitigrade satyr legs + heavy cloven hooves
                p.push((cap.clone(), b.clone(), Vec3::new(0.38 * s, 0.96, -0.06), Vec3::new(0.42, 0.42, 0.44), rx(-0.22)));
                p.push((cap.clone(), a.imp_dark.clone(), Vec3::new(0.38 * s, 0.42, 0.04), Vec3::new(0.32, 0.40, 0.34), rx(0.18)));
                p.push((cube.clone(), a.dark.clone(), Vec3::new(0.38 * s, 0.10, -0.08), Vec3::new(0.44, 0.20, 0.56), id));
                // Molten-gold spiked pauldrons + massive arms + blazing fists
                p.push((sph.clone(), gold.clone(), Vec3::new(0.86 * s, 2.48, 0.0), Vec3::new(0.44, 0.34, 0.46), rz(0.22 * s)));
                p.push((cone.clone(), gold.clone(), Vec3::new(0.94 * s, 2.86, 0.0), Vec3::new(0.12, 0.48, 0.12), rz(-0.35 * s)));
                p.push((cap.clone(), b.clone(), Vec3::new(0.96 * s, 1.92, -0.05), Vec3::new(0.34, 0.52, 0.36), rz(0.15 * s)));
                p.push((cap.clone(), b.clone(), Vec3::new(1.02 * s, 1.38, -0.16), Vec3::new(0.30, 0.42, 0.32), rx(0.38)));
                p.push((ring.clone(), gold.clone(), Vec3::new(1.02 * s, 1.30, -0.20), Vec3::new(0.24, 0.10, 0.24), rx(0.38)));
                p.push((sph.clone(), e.clone(), Vec3::new(1.02 * s, 1.06, -0.26), Vec3::splat(0.26), id)); // blazing hellfire fists!
                // Sweeping 3-segment curved bull-demon horns
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.34 * s, 3.16, -0.04), Vec3::new(0.13, 0.56, 0.13), rz(-0.85 * s)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.66 * s, 3.44, -0.08), Vec3::new(0.095, 0.52, 0.095), rz(-0.32 * s) * rx(0.25)));
                p.push((cone.clone(), a.bone.clone(), Vec3::new(0.76 * s, 3.78, -0.20), Vec3::new(0.06, 0.36, 0.06), rz(0.12 * s) * rx(0.45)));
                p.push((sph.clone(), e.clone(), Vec3::new(0.13 * s, 2.94, -0.32), Vec3::new(0.08, 0.055, 0.05), id));
            }
            // Ferocious horned skull + brow + snarling fanged jaw
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 2.90, -0.08), Vec3::new(0.34, 0.32, 0.34), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 3.04, -0.28), Vec3::new(0.42, 0.09, 0.16), rx(0.18)));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 2.72, -0.32), Vec3::new(0.32, 0.11, 0.14), id));
            for k in 0..4 {
                p.push((cone.clone(), a.bone.clone(), Vec3::new(-0.12 + k as f32 * 0.08, 2.75, -0.37), Vec3::new(0.032, 0.09, 0.032), rx(PI)));
            }
        }
        EnemyKind::PhaseStalker => {
            let ob = a.phase_obsidian.clone();
            let core = a.phase_core.clone();
            // Gaunt, forward-leaning wraith torso + exposed magenta phase core.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 1.3, 0.0), Vec3::new(0.42, 0.42, 0.28), rx(0.22)));
            p.push((cube.clone(), ob.clone(), Vec3::new(0.0, 1.36, -0.16), Vec3::new(0.36, 0.34, 0.1), rx(0.22)));
            p.push((sph.clone(), core.clone(), Vec3::new(0.0, 1.34, -0.22), Vec3::splat(0.12), id));
            p.push((ring.clone(), ob.clone(), Vec3::new(0.0, 1.34, -0.23), Vec3::new(0.16, 0.05, 0.16), rx(FRAC_PI_2)));
            p.push((cap.clone(), ob.clone(), Vec3::new(0.0, 0.92, 0.04), Vec3::new(0.26, 0.2, 0.2), id));
            for k in 0..4 {
                p.push((cone.clone(), ob.clone(), Vec3::new(0.0, 1.0 + k as f32 * 0.2, 0.22), Vec3::new(0.05, 0.22, 0.05), rx(1.1)));
            }
            for s in [-1.0f32, 1.0] {
                // Stilted reverse-jointed legs ending in blade feet.
                p.push((cap.clone(), b.clone(), Vec3::new(0.15 * s, 0.68, -0.04), Vec3::new(0.12, 0.3, 0.12), rx(-0.3)));
                p.push((cap.clone(), ob.clone(), Vec3::new(0.15 * s, 0.3, 0.05), Vec3::new(0.09, 0.3, 0.09), rx(0.28)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.15 * s, 0.06, -0.1), Vec3::new(0.07, 0.16, 0.12), rx(-1.2)));
                // Thin shoulders + long forearms + scythe blades.
                p.push((sph.clone(), ob.clone(), Vec3::new(0.32 * s, 1.56, -0.02), Vec3::new(0.12, 0.1, 0.12), id));
                p.push((cap.clone(), b.clone(), Vec3::new(0.38 * s, 1.3, -0.12), Vec3::new(0.08, 0.26, 0.08), rz(0.25 * s) * rx(0.5)));
                p.push((cap.clone(), ob.clone(), Vec3::new(0.42 * s, 1.0, -0.42), Vec3::new(0.07, 0.28, 0.07), rx(1.1)));
                p.push((cone.clone(), a.bone.clone(), Vec3::new(0.44 * s, 0.98, -0.88), Vec3::new(0.05, 0.62, 0.02), rx(-1.9) * rz(0.12 * s)));
                p.push((cube.clone(), core.clone(), Vec3::new(0.44 * s, 1.02, -0.74), Vec3::new(0.012, 0.02, 0.4), rx(0.3)));
                // Swept horns + visor eye slits.
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.12 * s, 2.04, 0.04), Vec3::new(0.05, 0.34, 0.05), rz(-0.35 * s) * rx(-0.6)));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.2 * s, 2.24, 0.22), Vec3::new(0.03, 0.24, 0.03), rx(-1.1)));
                p.push((cube.clone(), e.clone(), Vec3::new(0.06 * s, 1.83, -0.17), Vec3::new(0.08, 0.018, 0.03), rz(0.18 * s)));
            }
            // Narrow skull + razor static visor band.
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 1.82, -0.04), Vec3::new(0.15, 0.2, 0.17), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 1.83, -0.15), Vec3::new(0.24, 0.05, 0.06), id));
        }
        EnemyKind::RelaySniper => {
            let mp = a.metal_plate.clone();
            let lens = a.sniper_lens.clone();
            // Armored tripod: three splayed hydraulic legs under a turret hub.
            p.push((cyl.clone(), mp.clone(), Vec3::new(0.0, 1.25, 0.0), Vec3::new(0.3, 0.5, 0.3), id));
            for k in 0..3 {
                let ang = k as f32 * TAU / 3.0 + FRAC_PI_2;
                let o = Vec3::new(ang.cos(), 0.0, ang.sin());
                let tilt = Quat::from_rotation_arc(Vec3::Y, (Vec3::Y * 1.0 + o * 0.6).normalize());
                p.push((cap.clone(), b.clone(), o * 0.3 + Vec3::Y * 0.75, Vec3::new(0.12, 0.42, 0.12), tilt));
                p.push((cyl.clone(), mp.clone(), o * 0.58 + Vec3::Y * 0.28, Vec3::new(0.08, 0.5, 0.08), tilt));
                p.push((cube.clone(), a.dark.clone(), o * 0.72 + Vec3::Y * 0.05, Vec3::new(0.22, 0.1, 0.22), id));
            }
            // Armored torso + chest vent.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 1.7, 0.0), Vec3::new(0.55, 0.42, 0.42), id));
            p.push((cube.clone(), mp.clone(), Vec3::new(0.0, 1.72, -0.2), Vec3::new(0.46, 0.36, 0.08), id));
            p.push((cube.clone(), lens.clone(), Vec3::new(0.0, 1.6, -0.25), Vec3::new(0.3, 0.03, 0.02), id));
            // Elevated parabolic relay dish (head) + crimson scope lens.
            p.push((cyl.clone(), mp.clone(), Vec3::new(0.0, 2.12, 0.0), Vec3::new(0.08, 0.3, 0.08), id));
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 2.36, 0.02), Vec3::new(0.24, 0.2, 0.22), id));
            p.push((cone.clone(), mp.clone(), Vec3::new(0.0, 2.36, -0.18), Vec3::new(0.34, 0.12, 0.34), rx(-FRAC_PI_2)));
            p.push((sph.clone(), e.clone(), Vec3::new(0.0, 2.36, -0.3), Vec3::splat(0.08), id));
            p.push((cyl.clone(), lens.clone(), Vec3::new(0.0, 2.36, -0.42), Vec3::new(0.035, 0.22, 0.035), rx(FRAC_PI_2)));
            for s in [-1.0f32, 1.0] {
                // Twin stabilizing antenna fins.
                p.push((cube.clone(), mp.clone(), Vec3::new(0.28 * s, 2.56, 0.12), Vec3::new(0.03, 0.5, 0.18), rz(-0.3 * s)));
                p.push((sph.clone(), lens.clone(), Vec3::new(0.4 * s, 2.8, 0.12), Vec3::splat(0.04), id));
                // Shoulders + arms.
                p.push((sph.clone(), mp.clone(), Vec3::new(0.48 * s, 1.95, 0.0), Vec3::new(0.18, 0.15, 0.2), id));
                p.push((cap.clone(), b.clone(), Vec3::new(0.52 * s, 1.62, -0.1), Vec3::new(0.1, 0.24, 0.1), rx(0.6)));
            }
            // Shoulder rail barrel (right) with glowing coils.
            p.push((cyl.clone(), a.dark.clone(), Vec3::new(0.5, 2.1, -0.42), Vec3::new(0.07, 1.1, 0.07), rx(FRAC_PI_2)));
            for k in 0..3 {
                p.push((ring.clone(), lens.clone(), Vec3::new(0.5, 2.1, -0.2 - k as f32 * 0.26), Vec3::new(0.1, 0.04, 0.1), rx(FRAC_PI_2)));
            }
        }
        EnemyKind::CarrierDaemon => {
            let gold = a.baron_gold.clone();
            let core = a.carrier_core.clone();
            let mp = a.metal_plate.clone();
            // Massive blackened-iron torso + molten-gold ribcage around the Signal Core.
            p.push((cap.clone(), b.clone(), Vec3::new(0.0, 2.55, 0.0), Vec3::new(1.7, 1.0, 1.1), id));
            p.push((sph.clone(), core.clone(), Vec3::new(0.0, 2.6, -0.42), Vec3::splat(0.46), id));
            p.push((ring.clone(), gold.clone(), Vec3::new(0.0, 2.6, -0.5), Vec3::new(0.6, 0.16, 0.6), rx(FRAC_PI_2)));
            for k in 0..4 {
                let y = 2.05 + k as f32 * 0.34;
                for s in [-1.0f32, 1.0] {
                    p.push((cube.clone(), gold.clone(), Vec3::new(0.52 * s, y, -0.5), Vec3::new(0.46, 0.08, 0.12), rz(0.35 * s)));
                }
            }
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 1.62, 0.0), Vec3::new(1.3, 0.3, 0.9), id));
            p.push((cube.clone(), gold.clone(), Vec3::new(0.0, 1.62, -0.46), Vec3::new(0.7, 0.2, 0.06), id));
            for s in [-1.0f32, 1.0] {
                // Pillar legs + armored hooves.
                p.push((cap.clone(), b.clone(), Vec3::new(0.55 * s, 1.1, 0.0), Vec3::new(0.5, 0.55, 0.52), id));
                p.push((cyl.clone(), mp.clone(), Vec3::new(0.55 * s, 0.45, 0.02), Vec3::new(0.34, 0.75, 0.34), id));
                p.push((cube.clone(), a.dark.clone(), Vec3::new(0.55 * s, 0.12, -0.1), Vec3::new(0.62, 0.24, 0.78), id));
                // Shoulder plasma cannons: gold mantles + iron barrels + glowing muzzles.
                p.push((sph.clone(), gold.clone(), Vec3::new(1.2 * s, 3.25, 0.0), Vec3::new(0.55, 0.42, 0.55), id));
                p.push((cyl.clone(), a.dark.clone(), Vec3::new(1.25 * s, 3.4, -0.55), Vec3::new(0.2, 1.0, 0.2), rx(FRAC_PI_2)));
                p.push((ring.clone(), gold.clone(), Vec3::new(1.25 * s, 3.4, -0.95), Vec3::new(0.26, 0.1, 0.26), rx(FRAC_PI_2)));
                p.push((sph.clone(), core.clone(), Vec3::new(1.25 * s, 3.4, -1.06), Vec3::splat(0.16), id));
                // Heavy arms + gold-clawed fists.
                p.push((cap.clone(), b.clone(), Vec3::new(1.3 * s, 2.45, -0.05), Vec3::new(0.4, 0.6, 0.42), rz(0.12 * s)));
                p.push((cap.clone(), b.clone(), Vec3::new(1.38 * s, 1.75, -0.22), Vec3::new(0.34, 0.5, 0.36), rx(0.4)));
                for c in [-0.12f32, 0.0, 0.12] {
                    p.push((cone.clone(), gold.clone(), Vec3::new(1.38 * s + c, 1.3, -0.42), Vec3::new(0.05, 0.3, 0.05), rx(PI - 0.4)));
                }
            }
            // Horned skull + molten eyes.
            p.push((sph.clone(), b.clone(), Vec3::new(0.0, 3.72, -0.1), Vec3::new(0.42, 0.38, 0.42), id));
            p.push((cube.clone(), a.dark.clone(), Vec3::new(0.0, 3.86, -0.42), Vec3::new(0.5, 0.1, 0.16), rx(0.2)));
            for s in [-1.0f32, 1.0] {
                p.push((sph.clone(), e.clone(), Vec3::new(0.15 * s, 3.74, -0.44), Vec3::new(0.09, 0.06, 0.05), id));
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.38 * s, 4.0, -0.04), Vec3::new(0.13, 0.55, 0.13), rz(-0.7 * s)));
            }
            // 4-spoked demonic satellite crown / halo.
            p.push((ring.clone(), gold.clone(), Vec3::new(0.0, 4.32, 0.25), Vec3::new(0.95, 0.14, 0.95), rx(1.2)));
            for k in 0..4 {
                let ang = k as f32 * FRAC_PI_2 + 0.785;
                let o = Quat::from_rotation_x(1.2) * Vec3::new(ang.cos(), 0.0, ang.sin());
                p.push((cone.clone(), a.horn.clone(), Vec3::new(0.0, 4.32, 0.25) + o * 1.05, Vec3::new(0.09, 0.48, 0.09), Quat::from_rotation_arc(Vec3::Y, o)));
                p.push((sph.clone(), core.clone(), Vec3::new(0.0, 4.32, 0.25) + o * 0.78, Vec3::splat(0.1), id));
            }
        }
    }
    p
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
    for part in &a.models[kind.index()] {
        let e = commands.spawn((ChildOf(pivot), Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), Transform::IDENTITY)).id();
        match part.role {
            PartRole::Body => body_part = e,
            PartRole::Eye => eye_part = e,
            PartRole::Static => {}
        }
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
fn enemy_ai(
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
            e.sees_player = dist < 45.0 && has_line_of_sight(&grid, eye, p_eye);
            if !e.alerted && ((e.sees_player && dist < 26.0) || dist < 8.0) {
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
                        // Arrives ready to fire its twin bolts.
                        e.attack_timer = e.attack_timer.min(0.25);
                    }
                    e.blink_side = -e.blink_side;
                }
                continue;
            }
            if should_start_blink(&e, dist) {
                e.blink_timer = PHASE_BLINK_TELEGRAPH;
                spawn_burst(&mut commands, &fx, pos + Vec3::Y * 1.3, BurstColor::Gore, 8, 3.0, 0.1);
                sfx.play(SfxEvent::PhaseBlink);
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
                            sfx.play(SfxEvent::EnemyFire);
                        }
                    }
                } else if e.attack_timer <= 0.0 && e.sees_player && dist < e.stats.range && dist > 2.0 {
                    e.sniper_lock_timer = SNIPER_LOCK_SECS;
                    e.sniper_aim = p_chest;
                    sfx.play(SfxEvent::SniperLock);
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
            sfx.play(SfxEvent::EnemyFire);
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
    let (mat, halo) = match color {
        BurstColor::Cyan => (fx.cyan.clone(), fx.cyan_halo.clone()),
        BurstColor::Green => (fx.green.clone(), fx.green_halo.clone()),
        _ => (fx.fire.clone(), fx.fire_halo.clone()),
    };
    let core_mat = match color {
        BurstColor::Cyan => fx.cyan_core.clone(),
        _ => fx.tracer_core.clone(),
    };
    let light = match color {
        BurstColor::Gore => Color::srgb(1.0, 0.2, 0.6),
        c => FxAssets::light_color(c),
    };
    commands.spawn((
        LevelEntity,
        EnemyProjectile { vel, damage, radius, life: 5.0, color },
        Mesh3d(fx.sphere.clone()),
        MeshMaterial3d(core_mat),
        Transform::from_translation(pos).with_scale(Vec3::splat(radius * 0.65)),
        children![
            (Mesh3d(fx.sphere.clone()), MeshMaterial3d(halo), Transform::from_scale(Vec3::splat(1.75))),
            (Mesh3d(fx.ring.clone()), MeshMaterial3d(mat), Transform::from_scale(Vec3::new(1.85, 0.55, 1.85)).with_rotation(Quat::from_rotation_x(0.7))),
            (PointLight { color: light, intensity: 110_000.0, range: 6.0, shadow_maps_enabled: false, ..default() }, Transform::default()),
        ],
    ));
}

/// Shortest distance between segment `a..b` and point `p`.
pub fn segment_point_distance(a: Vec3, b: Vec3, p: Vec3) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-8 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
    (a + ab * t).distance(p)
}

#[allow(clippy::too_many_arguments)]
fn move_enemy_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    fx: Res<FxAssets>,
    mut sfx: ResMut<SfxQueue>,
    mut players: Query<(&Transform, &mut Player)>,
    mut q: Query<(Entity, &mut EnemyProjectile, &mut Transform), Without<Player>>,
    (signal, mods): (Res<SignalState>, Res<PlayerModifiers>),
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
            commands.entity(e).try_despawn();
            continue;
        }
        if p.life <= 0.0 {
            commands.entity(e).try_despawn();
            continue;
        }
        tf.translation = to;
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
    enemies: Query<(&Enemy, &EnemyLook, Option<&PingMarked>, &Children)>,
    mut bodies: Query<&mut Transform, (With<EnemyBody>, Without<EnrageHalo>, Without<FiendFieldRing>)>,
    mut halos: Query<(&mut Transform, &mut Visibility), (With<EnrageHalo>, Without<EnemyBody>, Without<FiendFieldRing>)>,
    mut fiend_rings: Query<&mut Transform, (With<FiendFieldRing>, Without<EnemyBody>, Without<EnrageHalo>)>,
    mut auras: Query<(&mut PointLight, &mut CullableLight), With<EnemyAura>>,
    mut parts: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    let t = time.elapsed_secs();
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
    for (e, look, marked, children) in &enemies {
        // Bob / stagger animation on the body pivot.
        let moving = Vec2::new(e.velocity.x, e.velocity.z).length() > 0.5;
        let bob = if e.kind.flying() {
            (e.phase * 2.0).sin() * 0.08
        } else if moving {
            (e.phase * 9.0).sin().abs() * 0.08
        } else {
            (e.phase * 2.0).sin() * 0.02
        };
        for c in children.iter() {
            if let Ok(mut b) = bodies.get_mut(c) {
                b.translation.y = bob;
                let sway = if moving && !e.kind.flying() { (e.phase * 9.0).sin() * 0.06 } else { 0.0 };
                let stun = if e.stun_timer > 0.0 { (t * 40.0).sin() * 0.08 } else { 0.0 };
                b.rotation = Quat::from_rotation_z(sway + stun);
                // Phase Stalker shimmer: glitchy jitter + stretch while charging a blink.
                if e.blink_timer > 0.0 {
                    let k = 1.0 - e.blink_timer / PHASE_BLINK_TELEGRAPH;
                    b.translation.x = (t * 70.0).sin() * 0.12 * (0.4 + k);
                    b.scale = Vec3::new(1.0 - 0.25 * k, 1.0 + 0.2 * k, 1.0 - 0.25 * k);
                } else if b.scale != Vec3::ONE || b.translation.x != 0.0 {
                    b.scale = Vec3::ONE;
                    b.translation.x = 0.0;
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
) {
    let mut rng = rand::rng();
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
        run.kills += 1;
        sfx.play(SfxEvent::EnemyDeath);
        if e.kind == EnemyKind::CarrierDaemon {
            // Massive golden / crimson multi-burst.
            for (k, off) in [Vec3::new(0.0, 1.6, 0.0), Vec3::new(1.2, 0.4, 0.3), Vec3::new(-1.1, -0.6, -0.4), Vec3::new(0.2, -1.4, 0.8)].into_iter().enumerate() {
                let c = if k % 2 == 0 { BurstColor::Gold } else { BurstColor::Fire };
                spawn_burst(&mut commands, &fx, center + off, c, 30, 11.0, 0.3);
            }
            spawn_burst(&mut commands, &fx, center, BurstColor::Gore, 36, 9.0, 0.26);
            spawn_flash_light(&mut commands, center, Color::srgb(1.0, 0.55, 0.2), 9_000_000.0, 40.0, 0.8);
            sfx.play(SfxEvent::JammerDestroyed);
            sfx.play(SfxEvent::BossShield);
        }
        if e.vaporized {
            continue;
        }
        feed.hit(true);
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
        let scope = tf.transform_point(Vec3::new(0.0, e.stats.eye_height, -0.45));
        let d = e.sniper_aim - scope;
        let len = d.length().max(0.1);
        let inv = tf.rotation.inverse();
        let mid_world = scope + d * 0.5;
        // Thin (and thinning as the lock completes) glowing crimson rod.
        let w = 0.03 + 0.04 * (e.sniper_lock_timer / SNIPER_LOCK_SECS);
        ltf.translation = inv * (mid_world - tf.translation);
        ltf.rotation = inv * Quat::from_rotation_arc(Vec3::Z, d / len);
        ltf.scale = Vec3::new(w, w, len);
    }
}

pub struct EnemiesPlugin;

impl Plugin for EnemiesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlowField>()
            .add_systems(Startup, create_enemy_assets)
            .add_systems(Update, spawn_garrison.run_if(resource_exists_and_changed::<LevelGrid>).in_set(GameSet::World))
            .add_systems(
                Update,
                (update_flow_field, enemy_ai, move_enemy_projectiles, tick_ping_marks, enemy_visuals, sniper_lasers)
                    .chain()
                    .in_set(GameSet::Combat)
                    .run_if(resource_exists::<LevelGrid>),
            )
            .add_systems(Update, enemy_deaths.in_set(GameSet::Combat).after(enemy_visuals).after(crate::weapons::CombatDamage));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
