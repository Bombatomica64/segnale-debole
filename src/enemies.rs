//! The four demon archetypes: Static Imp, Dead-Zone Cacodemon, Jammer Fiend and
//! Hell Baron. Multi-part procedural meshes, flow-field chasing over the
//! `LevelGrid`, projectiles, Dead-Zone enrage, mobile jammer fields, hit-flash,
//! Ping marks and death gibs/drops.

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI};

use bevy::prelude::*;
use rand::Rng;
use rand::seq::SliceRandom;

use crate::audio::{SfxEvent, SfxQueue};
use crate::fx::{spawn_burst, BurstColor, FxAssets, HudFeed};
use crate::level::{Cell, DemonJammer, LevelEntity, LevelGrid, RampDir, SignalTower, CEILING_HEIGHT};
use crate::physics::{count_wall_occlusions, has_line_of_sight, raycast_grid, resolve_player_movement, sample_floor_height, PLAYER_RADIUS};
use crate::player::{Player, PLAYER_EYE_HEIGHT};
use crate::signal::{active_jammer_fields, compute_signal_at, JammerField, SignalState};
use crate::state::{GameSet, PlayerModifiers, RunStats};
use crate::weapons::{spawn_pickup, PickupAssets, PickupKind};

/// Mobile Dead Zone projected by every living Jammer Fiend.
pub const FIEND_FIELD_RADIUS: f32 = 13.0;
pub const FIEND_FIELD_STRENGTH: f32 = 2.5;
/// Static Enrage speed multiplier (Imps inside Dead Zones).
pub const ENRAGE_SPEED_MULT: f32 = 1.35;
pub const ENRAGE_FIRE_MULT: f32 = 0.65;
/// Crisp, brief hit-flash so demons never look washed out.
pub const HIT_FLASH_SECS: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnemyKind {
    Imp,
    Cacodemon,
    JammerFiend,
    HellBaron,
}

impl EnemyKind {
    pub fn name(self) -> &'static str {
        match self {
            EnemyKind::Imp => "STATIC IMP",
            EnemyKind::Cacodemon => "DEAD-ZONE CACODEMON",
            EnemyKind::JammerFiend => "JAMMER FIEND",
            EnemyKind::HellBaron => "HELL BARON",
        }
    }

    pub fn flying(self) -> bool {
        matches!(self, EnemyKind::Cacodemon)
    }

    pub fn stats(self, floor: u32) -> EnemyStats {
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
                fire_cooldown: 2.9 - 0.1 * f,
                proj_speed: 15.0,
                // Floor-scaled: a survivable Floor 1 boss, a brute deeper down.
                proj_damage: 11.0 + 1.0 * f,
                proj_radius: 0.4,
                range: 40.0,
                melee_damage: 20.0 + 2.0 * f,
                hit_spheres: &[(0.8, 0.8), (1.9, 0.95), (2.85, 0.6)],
            },
        }
    }
}

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
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0.0
    }

    /// Current move speed including Static Enrage.
    pub fn move_speed(&self) -> f32 {
        self.stats.speed * if self.enraged { ENRAGE_SPEED_MULT } else { 1.0 }
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
    let dmg = if marked { base * (1.0 + mods.ping_mark_damage_bonus) } else { base };
    enemy.hp -= dmg;
    enemy.hit_flash = HIT_FLASH_SECS;
    enemy.alerted = true;
    dmg
}

/// Static Enrage rule: Imps inside a Dead Zone (0-1 bars at their position).
pub fn should_enrage(kind: EnemyKind, in_dead_zone: bool) -> bool {
    kind == EnemyKind::Imp && in_dead_zone
}

/// Jammer fields projected by every living Jammer Fiend.
pub fn fiend_jammer_fields<'a>(enemies: impl Iterator<Item = (&'a Enemy, &'a GlobalTransform)>) -> Vec<JammerField> {
    enemies
        .filter(|(e, _)| e.kind == EnemyKind::JammerFiend && e.is_alive())
        .map(|(_, t)| JammerField { pos: t.translation() + Vec3::Y * 1.6, radius: FIEND_FIELD_RADIUS, strength: FIEND_FIELD_STRENGTH })
        .collect()
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

/// Per-enemy material handles for hit-flash / enrage / ping tinting.
#[derive(Component)]
pub struct EnemyLook {
    pub body: Handle<StandardMaterial>,
    pub eye: Handle<StandardMaterial>,
    pub body_emissive: LinearRgba,
    pub eye_emissive: LinearRgba,
    pub aura: Entity,
    pub dirty_key: u32,
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

#[derive(Component, Debug, Clone)]
pub struct EnemyProjectile {
    pub vel: Vec3,
    pub damage: f32,
    pub radius: f32,
    pub life: f32,
    pub color: BurstColor,
}

/// Shared enemy meshes and template materials.
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
    pub imp_body: StandardMaterial,
    pub imp_eye: StandardMaterial,
    pub imp_dark: Handle<StandardMaterial>,
    pub caco_body: StandardMaterial,
    pub caco_eye: StandardMaterial,
    pub caco_iris: Handle<StandardMaterial>,
    pub caco_maw: Handle<StandardMaterial>,
    pub fiend_body: StandardMaterial,
    pub fiend_green: Handle<StandardMaterial>,
    pub fiend_eye: StandardMaterial,
    pub baron_body: StandardMaterial,
    pub baron_gold: Handle<StandardMaterial>,
    pub baron_eye: StandardMaterial,
    pub ping_ring: Handle<StandardMaterial>,
    pub enrage_halo: Handle<StandardMaterial>,
    pub fiend_ring: Handle<StandardMaterial>,
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
    commands.insert_resource(EnemyAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        sphere: meshes.add(Sphere::new(1.0).mesh().uv(20, 14)),
        capsule: meshes.add(Capsule3d::new(0.5, 1.0)),
        cone: meshes.add(Cone::new(1.0, 1.0)),
        cylinder: meshes.add(Cylinder::new(1.0, 1.0)),
        ring: meshes.add(Torus::new(0.8, 1.0)),
        bone: materials.add(solid(0.76, 0.68, 0.52, 0.55, 0.0)),
        horn: materials.add(solid(0.08, 0.045, 0.04, 0.48, 0.15)),
        dark: materials.add(solid(0.035, 0.01, 0.012, 0.88, 0.05)),
        metal_plate: materials.add(solid(0.24, 0.26, 0.28, 0.36, 0.85)),
        imp_body: solid(0.32, 0.055, 0.028, 0.72, 0.0),
        imp_eye: eye(LinearRgba::rgb(18.0, 5.5, 0.35)),
        imp_dark: materials.add(solid(0.09, 0.025, 0.018, 0.85, 0.05)),
        caco_body: caco,
        caco_eye: eye(LinearRgba::rgb(3.5, 14.0, 1.4)),
        caco_iris: materials.add(eye(LinearRgba::rgb(0.5, 7.0, 12.0))),
        caco_maw: materials.add(caco_maw),
        fiend_body: solid(0.15, 0.17, 0.18, 0.4, 0.78),
        fiend_green: materials.add(green),
        fiend_eye: eye(LinearRgba::rgb(2.0, 15.0, 1.2)),
        baron_body: solid(0.38, 0.055, 0.028, 0.65, 0.05),
        baron_gold: materials.add(gold),
        baron_eye: eye(LinearRgba::rgb(18.0, 12.5, 1.2)),
        ping_ring: materials.add(ping),
        enrage_halo: materials.add(enrage_halo),
        fiend_ring: materials.add(fiend_ring),
    });
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
    }
    p
}

/// Spawn one demon. `pos` is the feet position (hover center for Cacodemons).
pub fn spawn_enemy(
    commands: &mut Commands,
    a: &EnemyAssets,
    materials: &mut Assets<StandardMaterial>,
    kind: EnemyKind,
    pos: Vec3,
    floor: u32,
    alerted: bool,
) -> Entity {
    let (body_tpl, eye_tpl) = match kind {
        EnemyKind::Imp => (&a.imp_body, &a.imp_eye),
        EnemyKind::Cacodemon => (&a.caco_body, &a.caco_eye),
        EnemyKind::JammerFiend => (&a.fiend_body, &a.fiend_eye),
        EnemyKind::HellBaron => (&a.baron_body, &a.baron_eye),
    };
    let body = materials.add(body_tpl.clone());
    let eye = materials.add(eye_tpl.clone());
    let parts = build_parts(kind, a, &body, &eye);
    let mut enemy = Enemy::new(kind, floor);
    enemy.alerted = alerted;
    let aura_h = enemy.stats.eye_height;
    let aura_color = match kind {
        EnemyKind::JammerFiend => Color::srgb(0.4, 1.0, 0.2),
        EnemyKind::HellBaron => Color::srgb(1.0, 0.6, 0.1),
        _ => Color::srgb(1.0, 0.1, 0.05),
    };
    let base_aura = match kind {
        EnemyKind::JammerFiend => 120_000.0,
        EnemyKind::HellBaron => 200_000.0,
        _ => 0.0,
    };
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
            PointLight { color: aura_color, intensity: base_aura, range: 7.0, shadow_maps_enabled: false, ..default() },
            Transform::from_xyz(0.0, aura_h, -0.6),
        ))
        .id();
    let pivot = commands.spawn((ChildOf(root), EnemyBody, Transform::default(), Visibility::default())).id();
    for (mesh, mat, t, s, r) in parts {
        commands.spawn((ChildOf(pivot), Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(t).with_scale(s).with_rotation(r)));
    }
    if kind == EnemyKind::Imp {
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
    let body_emissive = body_tpl.emissive;
    let eye_emissive = eye_tpl.emissive;
    commands.entity(root).insert((enemy, EnemyLook { body, eye, body_emissive, eye_emissive, aura, dirty_key: u32::MAX }));
    root
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
    (floor.max(1) as usize).div_ceil(2)
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
    let cacos = 1 + floor as usize / 2;
    let mut picked: Vec<IVec2> = Vec::new();
    for c in cands {
        if picked.len() >= total {
            break;
        }
        if picked.iter().all(|p| (*p - c).abs().max_element() >= 2) {
            picked.push(c);
        }
    }
    picked
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let kind = if i < fiends {
                EnemyKind::JammerFiend
            } else if i < fiends + cacos {
                EnemyKind::Cacodemon
            } else {
                EnemyKind::Imp
            };
            (kind, c)
        })
        .collect()
}

fn spawn_garrison(
    mut commands: Commands,
    grid: Res<LevelGrid>,
    a: Res<EnemyAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut flow: ResMut<FlowField>,
) {
    *flow = FlowField::default();
    let mut rng = rand::rng();
    for (kind, c) in garrison_plan(&grid, &mut rng) {
        spawn_enemy(&mut commands, &a, &mut materials, kind, spawn_pos_for(kind, &grid, c), grid.floor, false);
    }
}

// ---------------------------------------------------------------------------
// Flow field pathing
// ---------------------------------------------------------------------------

/// BFS distances toward the player's cell (walkers honour ramps/steps,
/// flyers only avoid walls).
#[derive(Resource, Debug, Default, Clone)]
pub struct FlowField {
    pub width: i32,
    pub target: Option<IVec2>,
    pub walk: Vec<Option<u32>>,
    pub fly: Vec<Option<u32>>,
}

fn bfs_to(grid: &LevelGrid, target: IVec2, passable: impl Fn(IVec2, IVec2) -> bool) -> Vec<Option<u32>> {
    let mut d = vec![None; (grid.width * grid.depth) as usize];
    if !grid.in_bounds(target) || grid.cell(target).is_wall() {
        return d;
    }
    let idx = |c: IVec2| (c.y * grid.width + c.x) as usize;
    d[idx(target)] = Some(0);
    let mut q = VecDeque::from([target]);
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
    d
}

impl FlowField {
    pub fn build(grid: &LevelGrid, target: IVec2) -> Self {
        FlowField {
            width: grid.width,
            target: Some(target),
            // Enemy at `from` must be able to step into `to`.
            walk: bfs_to(grid, target, |from, to| grid.can_walk(from, to)),
            fly: bfs_to(grid, target, |from, _| !grid.cell(from).is_wall()),
        }
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
        *flow = FlowField::build(&grid, c);
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
    freeze: Option<Res<crate::dev::DevFreezeAi>>,
) {
    if freeze.is_some() {
        return;
    }
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let ppos = ptf.translation;
    let p_eye = ppos + Vec3::Y * PLAYER_EYE_HEIGHT;
    let p_chest = ppos + Vec3::Y * 1.1;
    let mut rng = rand::rng();

    // Snapshot for separation + jammer fields (static jammers + fiends).
    let snapshot: Vec<(Entity, Vec3, f32)> = enemies.iter().map(|(e, en, tf, _)| (e, tf.translation, en.stats.radius)).collect();
    let mut fields = active_jammer_fields(jammers.iter());
    fields.extend(fiend_jammer_fields(enemies.iter().map(|(_, e, _, g)| (e, g))));
    let tower_ref = grid.tower_world() + Vec3::Y * 1.5;
    let beacon = tower.single().map(|t| t.beacon).unwrap_or(tower_ref);
    let neutral = PlayerModifiers::default();

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
            if e.kind == EnemyKind::Imp {
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
            let mut vel = Vec3::new(kb.x, e.velocity.y, kb.z);
            if !e.kind.flying() {
                let r = resolve_player_movement(&grid, pos, vel, dt, e.stats.radius, e.stats.height, crate::level::STEP_HEIGHT);
                tf.translation = r.pos;
                vel = r.vel;
                e.grounded = r.grounded;
            } else {
                tf.translation = fly_move(&grid, pos, vel * dt, e.stats.radius);
            }
            e.velocity = vel;
            continue;
        }

        // Desired horizontal direction.
        let mut desired = Vec3::ZERO;
        if e.alerted {
            let dir_p = flat.normalize_or_zero();
            let strafe = Vec3::new(-dir_p.z, 0.0, dir_p.x) * (e.phase * 0.9).sin();
            let direct = match e.kind {
                EnemyKind::Imp | EnemyKind::HellBaron => {
                    if dist > e.stats.radius + PLAYER_RADIUS + 0.6 { dir_p + strafe * 0.25 } else { Vec3::ZERO }
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
            desired = if e.sees_player {
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
        for &(other, opos, orad) in &snapshot {
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
            let r = resolve_player_movement(&grid, pos, vel + Vec3::new(kb.x, 0.0, kb.z), dt, e.stats.radius, e.stats.height, crate::level::STEP_HEIGHT);
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
            player.apply_damage(e.stats.melee_damage * if e.enraged { 1.3 } else { 1.0 });
            if hurt {
                sfx.play(SfxEvent::Hurt);
            }
            continue;
        }
        e.attack_timer -= dt;
        // The Baron's triple spread needs room to fan out: no point-blank volleys.
        let min_range = if e.kind == EnemyKind::HellBaron { 5.0 } else { 2.0 };
        if e.attack_timer <= 0.0 && e.sees_player && dist < e.stats.range && dist > min_range {
            e.attack_timer = e.fire_cooldown() * rng.random_range(0.8..1.25);
            let fwd = (p_chest - eye).normalize_or_zero();
            let side = Vec3::new(-fwd.z, 0.0, fwd.x).normalize_or_zero();
            let (color, dirs): (BurstColor, Vec<(Vec3, Vec3)>) = match e.kind {
                EnemyKind::Imp => (BurstColor::Fire, vec![(Vec3::ZERO, fwd)]),
                EnemyKind::Cacodemon => (BurstColor::Cyan, vec![(side * 0.45, fwd), (-side * 0.45, fwd)]),
                EnemyKind::JammerFiend => (BurstColor::Green, vec![(Vec3::ZERO, fwd)]),
                EnemyKind::HellBaron => (
                    BurstColor::Fire,
                    [-0.21f32, 0.0, 0.21].iter().map(|a| (Vec3::ZERO, Quat::from_rotation_y(*a) * fwd)).collect(),
                ),
            };
            let caco_purple = e.kind == EnemyKind::Cacodemon;
            for (off, d) in dirs {
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
) {
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let pfeet = ptf.translation;
    for (e, mut p, mut tf) in &mut q {
        p.life -= dt;
        let step = p.vel * dt;
        let from = tf.translation;
        let to = from + step;
        // Player: test the segment against the player's vertical axis samples.
        let hit_player = [0.3f32, 0.9, 1.5].iter().any(|h| segment_point_distance(from, to, pfeet + Vec3::Y * *h) < p.radius + PLAYER_RADIUS);
        if hit_player {
            let hurt = player.invuln_timer <= 0.0;
            player.apply_damage(p.damage);
            if hurt {
                sfx.play(SfxEvent::Hurt);
            }
            spawn_burst(&mut commands, &fx, to, p.color, 6, 4.0, 0.12);
            commands.entity(e).try_despawn();
            continue;
        }
        if let Some(hit) = raycast_grid(&grid, from, step, step.length()) {
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

#[allow(clippy::type_complexity)]
fn enemy_visuals(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut enemies: Query<(&Enemy, &mut EnemyLook, Option<&PingMarked>, &Children)>,
    mut bodies: Query<&mut Transform, (With<EnemyBody>, Without<EnrageHalo>, Without<FiendFieldRing>)>,
    mut halos: Query<(&mut Transform, &mut Visibility), (With<EnrageHalo>, Without<EnemyBody>, Without<FiendFieldRing>)>,
    mut fiend_rings: Query<&mut Transform, (With<FiendFieldRing>, Without<EnemyBody>, Without<EnrageHalo>)>,
    mut auras: Query<&mut PointLight, With<EnemyAura>>,
) {
    let t = time.elapsed_secs();
    for (e, mut look, marked, children) in &mut enemies {
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
            }
            if let Ok((mut htf, mut hvis)) = halos.get_mut(c) {
                *hvis = if e.enraged { Visibility::Inherited } else { Visibility::Hidden };
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
        // Material tinting (only rewritten when the visual state changes).
        let pulse = (t * 10.0).sin() * 0.5 + 0.5;
        let flashing = e.hit_flash > 0.0 || e.stun_timer > 0.0 || e.enraged;
        let key = (e.enraged as u32) | ((marked.is_some() as u32) << 1) | ((e.stun_timer > 0.0) as u32) << 2;
        if flashing || key != look.dirty_key {
            look.dirty_key = key;
            let mut body = look.body_emissive;
            if e.enraged {
                // High-contrast electric violet-magenta static aura (pops against red Mars walls!).
                body += LinearRgba::rgb(0.9, 0.08, 1.35) * (0.45 + 0.55 * pulse);
            }
            if marked.is_some() {
                body += LinearRgba::rgb(0.0, 0.18, 0.3);
            }
            if e.stun_timer > 0.0 {
                body += LinearRgba::rgb(0.4, 1.2, 1.6) * pulse;
            }
            if e.hit_flash > 0.0 {
                body += LinearRgba::rgb(4.2, 3.4, 2.6) * (e.hit_flash / HIT_FLASH_SECS);
            }
            if let Some(mut m) = materials.get_mut(&look.body) {
                m.emissive = body;
            }
            let eye = if e.enraged { LinearRgba::rgb(16.0, 4.0, 20.0) } else { look.eye_emissive };
            if let Some(mut m) = materials.get_mut(&look.eye) {
                m.emissive = eye;
            }
        }
        if let Ok(mut l) = auras.get_mut(look.aura) {
            l.intensity = match e.kind {
                _ if e.enraged => 220_000.0 + 180_000.0 * pulse,
                EnemyKind::JammerFiend => 140_000.0 + 90_000.0 * pulse,
                EnemyKind::HellBaron => 200_000.0,
                _ => 0.0,
            };
            if e.enraged {
                l.color = Color::srgb(1.0, 0.2, 0.95);
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
    mut players: Query<&mut Player>,
    enemies: Query<(Entity, &Enemy, &Transform)>,
) {
    let mut rng = rand::rng();
    for (entity, e, tf) in &enemies {
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
        };
        spawn_burst(&mut commands, &fx, center, a, n, 7.0, 0.22);
        spawn_burst(&mut commands, &fx, center, b, n / 2, 5.0, 0.16);
        run.kills += 1;
        sfx.play(SfxEvent::EnemyDeath);
        if e.vaporized {
            continue;
        }
        feed.hit(true);
        feed.toast(format!("+ {} DOWN", e.kind.name()), Color::srgb(1.0, 0.55, 0.35));
        if mods.siphon_heal > 0.0
            && signal.bars >= 3
            && let Ok(mut p) = players.single_mut()
        {
            p.heal(mods.siphon_heal);
            feed.toast(format!("SIGNAL SIPHON +{} HP", mods.siphon_heal as i32), Color::srgb(0.3, 1.0, 0.7));
        }
        // Drops.
        let ground = tf.translation.with_y(tf.translation.y - if e.kind.flying() { 2.0 } else { 0.0 });
        let drops: Vec<PickupKind> = match e.kind {
            EnemyKind::HellBaron => vec![PickupKind::Health, PickupKind::Armor, PickupKind::Shells, PickupKind::Plasma],
            EnemyKind::JammerFiend => vec![PickupKind::Plasma, PickupKind::Armor],
            _ => {
                let roll: f32 = rng.random();
                if roll < 0.22 {
                    vec![PickupKind::Shells]
                } else if roll < 0.4 {
                    vec![PickupKind::Plasma]
                } else if roll < 0.52 {
                    vec![PickupKind::Health]
                } else {
                    vec![]
                }
            }
        };
        for (i, k) in drops.into_iter().enumerate() {
            let off = Vec3::new((i as f32 * 2.1).cos(), 0.0, (i as f32 * 2.1).sin()) * if i == 0 { 0.0 } else { 0.8 };
            spawn_pickup(&mut commands, &pickups, k, ground + off, true);
        }
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
                (update_flow_field, enemy_ai, move_enemy_projectiles, tick_ping_marks, enemy_visuals)
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
    fn segment_point_distance_works() {
        let d = segment_point_distance(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(5.0, 2.0, 0.0));
        assert!((d - 2.0).abs() < 1e-5);
        let d = segment_point_distance(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), Vec3::new(-3.0, 0.0, 0.0));
        assert!((d - 3.0).abs() < 1e-5);
    }
}
