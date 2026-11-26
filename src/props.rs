//! Milestone 31: shootable CRT monitors and explosive Lithium UPS server racks.
//!
//! Placement comes from [`plan_interactive_props`] (deterministic per floor,
//! capped, wall-flush within [`INTERACTIVE_MAX_DEPTH`], never narrowing a
//! lane). Milestone 32: the interactive plan runs BEFORE the denser static
//! wall furniture / corner nooks of [`crate::architecture`], which keep >= 2
//! cells clear of every CRT / UPS rack, so the CRT + UPS budgets (and the
//! chain-reaction pair) are always met and never overlap static furniture.
//! Every prop shares the prewarmed [`PropAssets`] meshes / materials
//! (zero per-prop allocation) and is a [`LevelEntity`] (torn down with the
//! floor). Props have no collision: they hug the wall inside the 0.4 m
//! player radius band, so nothing snags.
//!
//! Damage arrives through [`PropHits`]: weapons log every hitscan pellet and
//! projectile segment as a [`PropRay`], explosions (mortar, barrels, 5G
//! Overload) as a [`PropBlast`]. [`apply_prop_damage`] resolves them, ticks UPS
//! fuses and detonates racks:
//!
//! - **CRT** (`hp` [`CRT_HP`]): green-phosphor sparks + glass debris, a
//!   cyan-green flash, the screen swaps to the cracked husk material,
//!   [`SfxEvent::CrtShatter`] and a small `CRT SHATTERED` style tick.
//! - **UPS** (`hp` [`UPS_HP`]): non-lethal hits spark and hiss; lethal damage
//!   detonates an EMP + blast ([`UPS_BLAST_RADIUS`]): heavy demon damage + a
//!   [`UPS_EMP_STUN`] stun, clamped self-damage only point blank, barrels cook
//!   off and neighbouring racks chain after [`UPS_CHAIN_FUSE`].

use bevy::prelude::*;

use crate::architecture::{plan_interactive_props, InteractiveKind, WallFace, INTERACTIVE_MAX_DEPTH};
use crate::audio::{SfxEvent, SfxQueue};
use crate::decals::{DecalKind, DecalQueue, DecalRequest};
use crate::enemies::{damage_enemy, Enemy, PingMarked};
use crate::fx::{spawn_burst, spawn_flash_light, spawn_particles, spawn_shockwave, BurstColor, CameraPunch, FxAssets, HudFeed};
use crate::level::{DemonJammer, ExplosiveBarrel, LevelEntity, LevelGrid};
use crate::physics::has_line_of_sight;
use crate::player::Player;
use crate::rumble::{RumblePulse, RumbleQueue};
use crate::state::{GameSet, PlayerModifiers};
use crate::style::{StyleAction, StyleQueue, StyleSource};
use crate::weapons::{damage_barrel, damage_jammer, knockback_scale, splash_distance, splash_falloff, BARREL_CENTER_Y, BARREL_RADIUS, JAMMER_SPHERES};

pub const CRT_HP: f32 = 1.0;
pub const UPS_HP: f32 = 25.0;
/// Detonation radius of a UPS rack (m).
pub const UPS_BLAST_RADIUS: f32 = 7.5;
/// Peak demon damage at the rack.
pub const UPS_BLAST_DAMAGE: f32 = 170.0;
/// EMP stun on demons that survive the blast (s).
pub const UPS_EMP_STUN: f32 = 0.9;
/// Staggered fuse of a rack set off by another blast (s).
pub const UPS_CHAIN_FUSE: f32 = 0.12;
/// The player only takes self-damage this close to the blast (m).
pub const UPS_SELF_RADIUS: f32 = 2.6;
/// Max self-damage of one blast (before armor) ...
pub const UPS_SELF_DAMAGE: f32 = 16.0;
/// ... and of a whole chain inside [`UPS_SELF_WINDOW`] seconds.
pub const UPS_SELF_DAMAGE_CAP: f32 = 22.0;
pub const UPS_SELF_WINDOW: f32 = 0.8;
/// Minimum seconds between two warning hisses of one rack.
pub const UPS_HISS_GAP: f32 = 0.45;

/// Shockwave glows (prewarmed at startup: detonations never allocate).
pub const UPS_CRIMSON_GLOW: (Color, f32) = (Color::srgb(1.0, 0.12, 0.14), 0.85);
pub const UPS_CYAN_GLOW: (Color, f32) = (Color::srgb(0.3, 0.95, 1.0), 0.75);

/// Axis-aligned world box (props sit on grid-aligned walls).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropBox {
    pub min: Vec3,
    pub max: Vec3,
}

impl PropBox {
    /// World box of the face-local box `x in [-hx, hx]`, `y in [y0, y1]`,
    /// `z in [0, depth]` (z out of the wall) on `face` at floor height `floor_y`.
    pub fn on_face(face: WallFace, floor_y: f32, hx: f32, (y0, y1): (f32, f32), depth: f32) -> Self {
        let fr = face.frame(floor_y);
        let a = fr.transform_point(Vec3::new(-hx, y0, 0.0));
        let b = fr.transform_point(Vec3::new(hx, y1, depth));
        PropBox { min: a.min(b), max: a.max(b) }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Distance from `p` to the box (0 inside).
    pub fn distance(&self, p: Vec3) -> f32 {
        p.clamp(self.min, self.max).distance(p)
    }

    /// Slab test: distance along `dir` (normalized) to the first hit (0 inside).
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        let mut t0 = 0.0f32;
        let mut t1 = f32::INFINITY;
        for i in 0..3 {
            let (o, d, lo, hi) = (origin[i], dir[i], self.min[i], self.max[i]);
            if d.abs() < 1e-8 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let (mut a, mut b) = ((lo - o) / d, (hi - o) / d);
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }
}

/// A shootable CRT monitor.
#[derive(Component, Debug, Clone)]
pub struct CrtMonitor {
    pub hp: f32,
    pub shattered: bool,
    pub bounds: PropBox,
    /// Screen part (material swapped to the husk on shatter).
    pub screen: Entity,
    /// Unit vector out of the wall.
    pub normal: Vec3,
}

/// An explosive Lithium UPS server rack.
#[derive(Component, Debug, Clone)]
pub struct UpsRack {
    pub hp: f32,
    /// Seconds until detonation once armed.
    pub fuse: Option<f32>,
    /// Armed by another blast (chain reaction).
    pub chained: bool,
    pub detonated: bool,
    pub bounds: PropBox,
    pub normal: Vec3,
    /// Glowing parts (cells / stripes / LEDs) charred on detonation.
    pub glow_parts: Vec<Entity>,
    pub hiss_cooldown: f32,
}

impl UpsRack {
    pub fn is_intact(&self) -> bool {
        !self.detonated && self.fuse.is_none()
    }

    /// Damage the rack; lethal damage arms its fuse (instant for weapons,
    /// [`UPS_CHAIN_FUSE`] for chained blasts). Returns true if this armed it.
    pub fn damage(&mut self, amount: f32, chain: bool) -> bool {
        if !self.is_intact() || amount.is_nan() || amount <= 0.0 {
            return false;
        }
        self.hp -= amount;
        if self.hp <= 0.0 {
            self.fuse = Some(if chain { UPS_CHAIN_FUSE } else { 0.0 });
            self.chained = chain;
            return true;
        }
        false
    }

    /// Blast center (chassis middle, slightly out of the wall).
    pub fn blast_point(&self) -> Vec3 {
        let c = self.bounds.center();
        Vec3::new(c.x, self.bounds.min.y + 1.0, c.z) + self.normal * 0.35
    }
}

/// A weapon ray this frame: hitscan pellet or projectile segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropRay {
    pub origin: Vec3,
    /// Normalized.
    pub dir: Vec3,
    /// Where the ray stopped (wall / demon / segment end).
    pub len: f32,
    pub damage: f32,
    pub source: StyleSource,
}

/// An explosion this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropBlast {
    pub pos: Vec3,
    pub radius: f32,
    pub damage: f32,
    /// Set off by another explosion (UPS racks arm with the chain fuse).
    pub chain: bool,
}

/// Weapon rays + explosions logged this frame for the props.
#[derive(Resource, Debug, Default)]
pub struct PropHits {
    pub rays: Vec<PropRay>,
    pub blasts: Vec<PropBlast>,
}

impl PropHits {
    pub fn ray(&mut self, origin: Vec3, dir: Vec3, len: f32, damage: f32, source: StyleSource) {
        if len.is_finite() && damage > 0.0 {
            self.rays.push(PropRay { origin, dir: dir.normalize_or_zero(), len, damage, source });
        }
    }

    pub fn blast(&mut self, pos: Vec3, radius: f32, damage: f32, chain: bool) {
        if pos.is_finite() && radius > 0.0 {
            self.blasts.push(PropBlast { pos, radius, damage, chain });
        }
    }
}

/// Shared prop meshes + materials (built once at startup).
#[derive(Resource, Clone)]
pub struct PropAssets {
    pub desk_top: Handle<Mesh>,
    pub desk_leg: Handle<Mesh>,
    pub crt_body: Handle<Mesh>,
    pub plate: Handle<Mesh>,
    pub chassis: Handle<Mesh>,
    pub vent_cap: Handle<Mesh>,
    pub desk_mat: Handle<StandardMaterial>,
    pub crt_mat: Handle<StandardMaterial>,
    pub screen_mat: Handle<StandardMaterial>,
    pub husk_mat: Handle<StandardMaterial>,
    pub chassis_mat: Handle<StandardMaterial>,
    pub amber_mat: Handle<StandardMaterial>,
    pub red_mat: Handle<StandardMaterial>,
    pub cell_mat: Handle<StandardMaterial>,
    pub led_mat: Handle<StandardMaterial>,
    pub charred_mat: Handle<StandardMaterial>,
}

/// Base emissive of the pulsing battery cells.
pub const CELL_EMISSIVE: LinearRgba = LinearRgba::rgb(3.4, 0.12, 0.08);

/// Source meshes of the props (checked for ray-tracing compatibility in tests).
pub fn prop_meshes() -> [Mesh; 6] {
    [
        crate::sculpt::beveled_box(Vec3::new(1.5, 0.08, 0.5), 0.02),
        Mesh::from(Cuboid::new(0.08, 0.84, 0.42)),
        crate::sculpt::beveled_box(Vec3::new(0.58, 0.48, 0.44), 0.05),
        Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
        crate::sculpt::beveled_box(Vec3::new(1.0, 2.1, 0.48), 0.03),
        crate::sculpt::beveled_box(Vec3::new(1.04, 0.08, 0.52), 0.02),
    ]
}

fn create_prop_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let [desk_top, desk_leg, crt_body, plate, chassis, vent_cap] = prop_meshes().map(|m| meshes.add(m));
    let emissive = |base: Color, e: LinearRgba| StandardMaterial { base_color: base, emissive: e, perceptual_roughness: 0.4, ..default() };
    commands.insert_resource(PropAssets {
        desk_top,
        desk_leg,
        crt_body,
        plate,
        chassis,
        vent_cap,
        desk_mat: materials.add(StandardMaterial { base_color: Color::srgb(0.13, 0.13, 0.14), metallic: 0.6, perceptual_roughness: 0.55, ..default() }),
        crt_mat: materials.add(StandardMaterial { base_color: Color::srgb(0.52, 0.5, 0.44), perceptual_roughness: 0.82, ..default() }),
        screen_mat: materials.add(emissive(Color::srgb(0.06, 0.3, 0.12), LinearRgba::rgb(0.25, 2.4, 0.7))),
        husk_mat: materials.add(StandardMaterial {
            base_color: Color::srgb(0.03, 0.04, 0.035),
            emissive: LinearRgba::rgb(0.0, 0.05, 0.02),
            metallic: 0.2,
            perceptual_roughness: 0.18,
            ..default()
        }),
        chassis_mat: materials.add(StandardMaterial { base_color: Color::srgb(0.06, 0.06, 0.07), metallic: 0.7, perceptual_roughness: 0.42, ..default() }),
        amber_mat: materials.add(emissive(Color::srgb(1.0, 0.62, 0.08), LinearRgba::rgb(3.2, 1.5, 0.1))),
        red_mat: materials.add(emissive(Color::srgb(0.9, 0.06, 0.04), LinearRgba::rgb(3.0, 0.14, 0.08))),
        cell_mat: materials.add(emissive(Color::srgb(0.6, 0.03, 0.03), CELL_EMISSIVE)),
        led_mat: materials.add(emissive(Color::srgb(0.2, 0.8, 1.0), LinearRgba::rgb(0.2, 1.8, 2.4))),
        charred_mat: materials.add(StandardMaterial {
            base_color: Color::srgb(0.02, 0.018, 0.016),
            emissive: LinearRgba::rgb(0.1, 0.025, 0.0),
            perceptual_roughness: 0.95,
            ..default()
        }),
    });
}

fn prewarm_prop_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[UPS_CRIMSON_GLOW, UPS_CYAN_GLOW]);
}

/// Face-local part placement `(mesh, material, translation, scale, roll)`.
type Part = (Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3, f32);

fn spawn_part(commands: &mut Commands, parent: Entity, (mesh, mat, t, s, roll): Part) -> Entity {
    commands
        .spawn((ChildOf(parent), Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(t).with_scale(s).with_rotation(Quat::from_rotation_z(roll))))
        .id()
}

/// Spawn one CRT desk on `face` (floor height 0).
pub fn spawn_crt(commands: &mut Commands, a: &PropAssets, face: WallFace) -> Entity {
    let fr = face.frame(0.0);
    let root = commands.spawn((LevelEntity, fr, Visibility::default())).id();
    spawn_part(commands, root, (a.desk_top.clone(), a.desk_mat.clone(), Vec3::new(0.0, 0.86, 0.25), Vec3::ONE, 0.0));
    for x in [-0.62, 0.62] {
        spawn_part(commands, root, (a.desk_leg.clone(), a.desk_mat.clone(), Vec3::new(x, 0.42, 0.21), Vec3::ONE, 0.0));
    }
    spawn_part(commands, root, (a.crt_body.clone(), a.crt_mat.clone(), Vec3::new(0.0, 1.14, 0.24), Vec3::ONE, 0.0));
    let screen = spawn_part(commands, root, (a.plate.clone(), a.screen_mat.clone(), Vec3::new(0.0, 1.15, 0.47), Vec3::new(0.46, 0.36, 0.02), 0.0));
    // Keyboard slab beside the monitor.
    spawn_part(commands, root, (a.plate.clone(), a.desk_mat.clone(), Vec3::new(0.5, 0.915, 0.3), Vec3::new(0.36, 0.03, 0.16), 0.0));
    let bounds = PropBox::on_face(face, 0.0, 0.3, (0.9, 1.4), 0.5);
    commands.entity(root).insert(CrtMonitor { hp: CRT_HP, shattered: false, bounds, screen, normal: face.inward() });
    root
}

/// Spawn one UPS rack on `face` (floor height 0).
pub fn spawn_ups(commands: &mut Commands, a: &PropAssets, face: WallFace) -> Entity {
    let fr = face.frame(0.0);
    let root = commands.spawn((LevelEntity, fr, Visibility::default())).id();
    spawn_part(commands, root, (a.chassis.clone(), a.chassis_mat.clone(), Vec3::new(0.0, 1.05, 0.24), Vec3::ONE, 0.0));
    spawn_part(commands, root, (a.vent_cap.clone(), a.chassis_mat.clone(), Vec3::new(0.0, 2.14, 0.26), Vec3::ONE, 0.0));
    let mut glow = Vec::with_capacity(17);
    // Diagonal red / amber hazard stripes, top and bottom bands.
    for y in [1.9, 0.22] {
        for i in 0..6 {
            let mat = if i % 2 == 0 { a.amber_mat.clone() } else { a.red_mat.clone() };
            let x = -0.4 + i as f32 * 0.16;
            glow.push(spawn_part(commands, root, (a.plate.clone(), mat, Vec3::new(x, y, 0.495), Vec3::new(0.07, 0.15, 0.03), 0.6)));
        }
    }
    // Four pulsing lithium cells.
    for i in 0..4 {
        let x = -0.33 + i as f32 * 0.22;
        glow.push(spawn_part(commands, root, (a.plate.clone(), a.cell_mat.clone(), Vec3::new(x, 1.05, 0.49), Vec3::new(0.16, 0.9, 0.04), 0.0)));
    }
    glow.push(spawn_part(commands, root, (a.plate.clone(), a.led_mat.clone(), Vec3::new(0.0, 1.68, 0.5), Vec3::new(0.8, 0.04, 0.02), 0.0)));
    let bounds = PropBox::on_face(face, 0.0, 0.52, (0.0, 2.18), 0.53);
    commands.entity(root).insert(UpsRack {
        hp: UPS_HP,
        fuse: None,
        chained: false,
        detonated: false,
        bounds,
        normal: face.inward(),
        glow_parts: glow,
        hiss_cooldown: 0.0,
    });
    root
}

/// Spawn the floor's interactive props (never in the Time Trial).
fn spawn_interactive_props(mut commands: Commands, grid: Res<LevelGrid>, assets: Option<Res<PropAssets>>, run: Res<crate::state::RunStats>) {
    let Some(a) = assets else { return };
    if run.mode.is_time_trial() {
        return;
    }
    for p in plan_interactive_props(&grid) {
        match p.kind {
            InteractiveKind::Crt => spawn_crt(&mut commands, &a, p.face),
            InteractiveKind::Ups => spawn_ups(&mut commands, &a, p.face),
        };
    }
}

// Wall-flush envelope: interactive props never protrude more than 0.55 m.
const _: () = assert!(INTERACTIVE_MAX_DEPTH <= 0.55);

/// Pulse the shared battery-cell glow while any rack still stands.
fn animate_props(time: Res<Time>, assets: Option<Res<PropAssets>>, racks: Query<&UpsRack>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let Some(a) = assets else { return };
    if !racks.iter().any(|r| !r.detonated) {
        return;
    }
    let t = time.elapsed_secs();
    let k = 0.55 + 0.45 * (0.5 + 0.5 * (t * 5.2).sin());
    if let Some(mut m) = materials.get_mut(&a.cell_mat) {
        m.emissive = crate::architecture::scale_rgb(CELL_EMISSIVE, k);
    }
}

/// Whether a blast at `pos` reaches a prop box (radius + line of sight to
/// the prop's open face).
fn blast_reaches(grid: Option<&LevelGrid>, pos: Vec3, radius: f32, bounds: &PropBox, normal: Vec3) -> Option<f32> {
    let d = bounds.distance(pos);
    if d > radius {
        return None;
    }
    let face_pt = bounds.center() + normal * 0.45;
    match grid {
        Some(g) if d > 0.6 && !has_line_of_sight(g, pos, face_pt) => None,
        _ => Some(d),
    }
}

/// Self-damage budget inside a chain (never one-shots a healthy player).
#[derive(Debug, Default)]
pub struct SelfDamageWindow {
    pub taken: f32,
    pub timer: f32,
}

impl SelfDamageWindow {
    /// Clamp a blast's self-damage against the running chain budget.
    pub fn allow(&mut self, want: f32) -> f32 {
        if self.timer <= 0.0 {
            self.taken = 0.0;
        }
        let d = want.clamp(0.0, UPS_SELF_DAMAGE).min((UPS_SELF_DAMAGE_CAP - self.taken).max(0.0));
        self.taken += d;
        self.timer = UPS_SELF_WINDOW;
        d
    }
}

/// Resolve the frame's weapon rays / explosions on the props, tick UPS
/// fuses and detonate.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_prop_damage(
    mut commands: Commands,
    time: Res<Time>,
    grid: Option<Res<LevelGrid>>,
    (fx, assets, mods): (Res<FxAssets>, Option<Res<PropAssets>>, Res<PlayerModifiers>),
    mut hits: ResMut<PropHits>,
    (mut sfx, mut feed, mut decals, mut punch): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<DecalQueue>, ResMut<CameraPunch>),
    (mut style, mut rumble, mut materials): (ResMut<StyleQueue>, ResMut<RumbleQueue>, ResMut<Assets<StandardMaterial>>),
    mut crts: Query<&mut CrtMonitor>,
    mut racks: Query<&mut UpsRack>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    (mut barrels, mut jammers): (Query<(&mut ExplosiveBarrel, &GlobalTransform)>, Query<(&mut DemonJammer, &GlobalTransform)>),
    mut players: Query<(&mut Player, &Transform)>,
    mut parts: Query<&mut MeshMaterial3d<StandardMaterial>>,
    (mut self_dmg, mut blasts): (Local<SelfDamageWindow>, Local<Vec<(Vec3, bool, Vec3)>>),
) {
    let dt = time.delta_secs();
    let grid = grid.as_deref();
    self_dmg.timer = (self_dmg.timer - dt).max(0.0);
    let mut shatter: Vec<(Vec3, Vec3)> = Vec::new();
    // ---- Weapon rays.
    for r in &hits.rays {
        for mut crt in &mut crts {
            if !crt.shattered
                && let Some(t) = crt.bounds.ray_hit(r.origin, r.dir)
                && t <= r.len + 0.05
            {
                crt.hp -= r.damage;
                if crt.hp <= 0.0 {
                    crt.shattered = true;
                    shatter.push((r.origin + r.dir * t, crt.normal));
                    if let Ok(mut m) = parts.get_mut(crt.screen)
                        && let Some(a) = assets.as_deref()
                    {
                        m.0 = a.husk_mat.clone();
                    }
                }
            }
        }
        for mut rack in &mut racks {
            if rack.is_intact()
                && let Some(t) = rack.bounds.ray_hit(r.origin, r.dir)
                && t <= r.len + 0.05
            {
                let p = r.origin + r.dir * t;
                if !rack.damage(r.damage, false) {
                    spawn_particles(&mut commands, &fx, p, BurstColor::Spark, 4, 6.0, 0.06);
                    if rack.hiss_cooldown <= 0.0 {
                        rack.hiss_cooldown = UPS_HISS_GAP;
                        sfx.play_at(SfxEvent::UpsHiss, p);
                        spawn_particles(&mut commands, &fx, p + rack.normal * 0.2, BurstColor::Smoke, 2, 1.2, 0.12);
                    }
                }
            }
        }
    }
    // ---- Explosions (mortar, barrels, 5G Overload).
    for b in &hits.blasts {
        for mut crt in &mut crts {
            if !crt.shattered && blast_reaches(grid, b.pos, b.radius, &crt.bounds, crt.normal).is_some() {
                crt.shattered = true;
                shatter.push((crt.bounds.center() + crt.normal * 0.3, crt.normal));
                if let Ok(mut m) = parts.get_mut(crt.screen)
                    && let Some(a) = assets.as_deref()
                {
                    m.0 = a.husk_mat.clone();
                }
            }
        }
        for mut rack in &mut racks {
            let (bounds, normal) = (rack.bounds, rack.normal);
            if let Some(d) = blast_reaches(grid, b.pos, b.radius, &bounds, normal) {
                rack.damage(b.damage * splash_falloff(d, b.radius).max(0.25), b.chain);
            }
        }
    }
    hits.rays.clear();
    hits.blasts.clear();
    for (p, n) in shatter.drain(..) {
        spawn_burst(&mut commands, &fx, p, BurstColor::Green, 14, 6.5, 0.07);
        spawn_particles(&mut commands, &fx, p + n * 0.1, BurstColor::Cyan, 8, 5.0, 0.045);
        spawn_particles(&mut commands, &fx, p, BurstColor::Spark, 5, 7.0, 0.04);
        spawn_flash_light(&mut commands, p + n * 0.4, Color::srgb(0.35, 1.0, 0.75), 380_000.0, 7.0, 0.16);
        sfx.play_at(SfxEvent::CrtShatter, p);
        style.push(StyleAction::CrtShatter);
    }
    // ---- UPS fuses.
    blasts.clear();
    for mut rack in &mut racks {
        rack.hiss_cooldown = (rack.hiss_cooldown - dt).max(0.0);
        if rack.detonated {
            continue;
        }
        if let Some(f) = rack.fuse {
            let f = f - dt;
            if f <= 0.0 {
                rack.detonated = true;
                blasts.push((rack.blast_point(), rack.chained, rack.normal));
                if let Some(a) = assets.as_deref() {
                    for &e in &rack.glow_parts {
                        if let Ok(mut m) = parts.get_mut(e) {
                            m.0 = a.charred_mat.clone();
                        }
                    }
                }
            } else {
                rack.fuse = Some(f);
            }
        }
    }
    let player_pos = players.single().ok().map(|(_, t)| t.translation);
    for &(pos, chained, normal) in blasts.iter() {
        // Demons: heavy blast + EMP stun on survivors.
        let mut hurt = 0;
        for (mut en, g, marked) in &mut enemies {
            if !en.is_alive() {
                continue;
            }
            let root = g.translation();
            let d = splash_distance(pos, root, en.stats.hit_spheres);
            if d > UPS_BLAST_RADIUS || grid.is_some_and(|g| !has_line_of_sight(g, pos, en.center(root))) {
                continue;
            }
            let falloff = splash_falloff(d, UPS_BLAST_RADIUS);
            damage_enemy(&mut en, UPS_BLAST_DAMAGE * (0.35 + 0.65 * falloff), marked.is_some(), &mods);
            en.style_source = StyleSource::Prop;
            let away = Vec2::new(root.x - pos.x, root.z - pos.z).normalize_or(Vec2::new(normal.x, normal.z));
            let kb = 15.0 * falloff * knockback_scale(en.kind);
            en.knockback += Vec3::new(away.x * kb, kb * 0.2, away.y * kb);
            en.stagger(Vec3::new(away.x, 0.0, away.y));
            if en.is_alive() {
                en.stun_timer = en.stun_timer.max(UPS_EMP_STUN);
            }
            hurt += 1;
        }
        for (mut j, g) in &mut jammers {
            let base = g.translation();
            if j.is_active() && splash_distance(pos, base, &JAMMER_SPHERES) <= UPS_BLAST_RADIUS && grid.is_none_or(|g| has_line_of_sight(g, pos, base + Vec3::Y * 1.3)) {
                damage_jammer(&mut j, UPS_BLAST_DAMAGE * 0.5);
                hurt += 1;
            }
        }
        // Coolant barrels cook off.
        for (mut b, g) in &mut barrels {
            let c = g.translation() + Vec3::Y * BARREL_CENTER_Y;
            let d = (c.distance(pos) - BARREL_RADIUS).max(0.0);
            if b.is_intact() && d <= UPS_BLAST_RADIUS && grid.is_none_or(|g| has_line_of_sight(g, pos, c)) {
                damage_barrel(&mut b, UPS_BLAST_DAMAGE, true);
            }
        }
        // Chain reaction: neighbouring racks arm with the short fuse.
        for mut rack in &mut racks {
            let (bounds, n) = (rack.bounds, rack.normal);
            if rack.is_intact() && blast_reaches(grid, pos, UPS_BLAST_RADIUS, &bounds, n).is_some() {
                rack.damage(UPS_HP * 4.0, true);
            }
        }
        // CRTs in the blast pop too.
        for mut crt in &mut crts {
            if !crt.shattered && blast_reaches(grid, pos, UPS_BLAST_RADIUS, &crt.bounds, crt.normal).is_some() {
                crt.shattered = true;
                if let Ok(mut m) = parts.get_mut(crt.screen)
                    && let Some(a) = assets.as_deref()
                {
                    m.0 = a.husk_mat.clone();
                }
                let p = crt.bounds.center() + crt.normal * 0.3;
                spawn_burst(&mut commands, &fx, p, BurstColor::Green, 10, 6.0, 0.06);
                sfx.play_at(SfxEvent::CrtShatter, p);
                style.push(StyleAction::CrtShatter);
            }
        }
        // Mild, clamped self-damage only right on top of it + a blast shove.
        if let Ok((mut p, ptf)) = players.single_mut()
            && !p.is_dead()
            && grid.is_none_or(|g| has_line_of_sight(g, pos, ptf.translation + Vec3::Y * 0.9))
            && let Some(mut jump) = crate::weapons::compute_blast_jump(ptf.translation, pos, UPS_SELF_RADIUS, UPS_SELF_DAMAGE * mods.rocket_self_damage_mult, 0.8)
        {
            jump.self_damage = self_dmg.allow(jump.self_damage);
            crate::weapons::apply_rocket_jump(&mut p, &jump);
        }
        // VFX / SFX / feel.
        decals.push(DecalRequest::floor(DecalKind::Scorch, pos.with_y(0.0) + normal * 0.6, 3.0));
        decals.push(DecalRequest::surface(DecalKind::Scorch, pos - normal * 0.3 + Vec3::Y * 0.4, normal, 2.2));
        decals.push(DecalRequest::floor(DecalKind::PlasmaBurn, pos.with_y(0.0) + normal * 1.6, 2.0));
        spawn_flash_light(&mut commands, pos, Color::srgb(1.0, 0.18, 0.2), 3_600_000.0, 20.0, 0.34);
        spawn_shockwave(&mut commands, &fx, &mut materials, pos, UPS_BLAST_RADIUS * 0.9, 0.38, UPS_CRIMSON_GLOW.0, UPS_CRIMSON_GLOW.1, true);
        spawn_shockwave(&mut commands, &fx, &mut materials, pos, UPS_BLAST_RADIUS * 0.55, 0.3, UPS_CYAN_GLOW.0, UPS_CYAN_GLOW.1, true);
        spawn_burst(&mut commands, &fx, pos, BurstColor::Fire, 16, 10.0, 0.18);
        spawn_burst(&mut commands, &fx, pos, BurstColor::Cyan, 12, 9.0, 0.08);
        spawn_particles(&mut commands, &fx, pos, BurstColor::Spark, 10, 12.0, 0.06);
        spawn_particles(&mut commands, &fx, pos + Vec3::Y * 0.4, BurstColor::Smoke, 6, 2.6, 0.32);
        sfx.play_at(SfxEvent::UpsDetonate, pos);
        let near = player_pos.map_or(f32::INFINITY, |pp| pp.distance(pos));
        punch.kick(if near < 12.0 { 0.85 } else { 0.45 });
        if near < 30.0 {
            rumble.push(RumblePulse::UpsBlast);
        }
        style.push(StyleAction::UpsDetonation { chain: chained });
        feed.toast(if chained { "CHAIN REACTION // LITHIUM CASCADE" } else { "UPS DETONATION // EMP BURST" }, Color::srgb(1.0, 0.3, 0.25));
        if hurt > 0 {
            feed.hit(false);
        }
    }
}

/// Prop damage resolution (after weapons, before demon deaths).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropDamageSet;

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PropHits>()
            .add_systems(Startup, (create_prop_assets, prewarm_prop_fades))
            .add_systems(Update, spawn_interactive_props.in_set(GameSet::World).run_if(resource_exists_and_changed::<LevelGrid>))
            .add_systems(Update, animate_props.in_set(GameSet::World))
            .configure_sets(Update, PropDamageSet.in_set(GameSet::Combat).after(crate::weapons::CombatDamage).before(crate::enemies::EnemyDeathsSet))
            .add_systems(Update, apply_prop_damage.in_set(PropDamageSet));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_box_ray_and_distance() {
        let b = PropBox { min: Vec3::new(-1.0, 0.0, -1.0), max: Vec3::new(1.0, 2.0, 1.0) };
        assert!((b.ray_hit(Vec3::new(0.0, 1.0, -5.0), Vec3::Z).unwrap() - 4.0).abs() < 1e-5);
        assert_eq!(b.ray_hit(Vec3::new(0.0, 1.0, 0.0), Vec3::Z), Some(0.0), "inside");
        assert!(b.ray_hit(Vec3::new(0.0, 3.0, -5.0), Vec3::Z).is_none());
        assert!(b.ray_hit(Vec3::new(0.0, 1.0, -5.0), Vec3::NEG_Z).is_none(), "behind");
        assert!(b.ray_hit(Vec3::new(5.0, 1.0, 0.0), Vec3::Y).is_none(), "parallel outside");
        assert!((b.distance(Vec3::new(4.0, 1.0, 0.0)) - 3.0).abs() < 1e-5);
        assert_eq!(b.distance(Vec3::new(0.5, 1.0, 0.5)), 0.0);
    }

    #[test]
    fn ups_damage_arms_fuse_instant_or_chained() {
        let mut r = UpsRack {
            hp: UPS_HP,
            fuse: None,
            chained: false,
            detonated: false,
            bounds: PropBox { min: Vec3::ZERO, max: Vec3::ONE },
            normal: Vec3::Z,
            glow_parts: vec![],
            hiss_cooldown: 0.0,
        };
        assert!(!r.damage(10.0, false), "non-lethal");
        assert!(r.is_intact());
        assert!(!r.damage(f32::NAN, false) && !r.damage(-4.0, false));
        assert!(r.damage(20.0, false));
        assert_eq!(r.fuse, Some(0.0));
        assert!(!r.damage(100.0, true), "already armed");
        let mut c = UpsRack { hp: UPS_HP, fuse: None, chained: false, ..r.clone() };
        assert!(c.damage(UPS_HP * 4.0, true));
        assert_eq!(c.fuse, Some(UPS_CHAIN_FUSE));
        assert!(c.chained);
    }

    #[test]
    fn self_damage_window_never_one_shots() {
        let mut w = SelfDamageWindow::default();
        assert_eq!(w.allow(99.0), UPS_SELF_DAMAGE);
        let second = w.allow(99.0);
        assert!((second - (UPS_SELF_DAMAGE_CAP - UPS_SELF_DAMAGE)).abs() < 1e-5);
        assert_eq!(w.allow(99.0), 0.0, "a long chain caps out");
        w.timer = 0.0;
        assert_eq!(w.allow(5.0), 5.0, "a fresh window resets the budget");
        const { assert!(UPS_SELF_DAMAGE_CAP < 100.0 * 0.5, "even a full chain leaves a healthy player standing") };
    }

    #[test]
    fn prop_meshes_are_raytracing_convertible() {
        for m in prop_meshes() {
            assert!(crate::raytracing::rt_mesh_convertible(&m));
        }
    }
}
