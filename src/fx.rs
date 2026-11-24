//! Shared transient visual effects (layered sparks, bouncing gibs/debris,
//! ejected brass shells, tapered tracers, branching dual-layer lightning,
//! expanding shockwave rings, light beams, flash lights) and the HUD feedback
//! channel (hitmarker, killfeed toasts, alerts).
//!
//! Every effect is a `LevelEntity` with an [`Fx`] lifetime, so floor teardown
//! cleans up anything still alive.

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use rand::Rng;

use std::sync::{Arc, Mutex};

use crate::audio::{is_clink_bounce, SfxEvent, SfxQueue};
use crate::level::{CullableLight, LevelEntity, LevelGrid};
use crate::physics::sample_floor_height;
use crate::state::GameSet;

/// Render layer drawn by the first-person overlay camera (depth cleared, so
/// anything on it shows through walls): viewmodels and Ping Scan x-ray beacons.
pub const OVERLAY_LAYER: usize = 1;

pub fn overlay_layer() -> RenderLayers {
    RenderLayers::layer(OVERLAY_LAYER)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxMotion {
    /// Ballistic particle; shrinks over its life and optionally bounces off floors.
    Particle { vel: Vec3, gravity: f32 },
    /// Tumbling debris / brass shell casing that bounces off level floors.
    /// `clinks` is the casing's remaining budget of positional
    /// [`SfxEvent::ShellClink`]s, spent one per real floor bounce (0 = silent).
    Debris { vel: Vec3, spin: Vec3, gravity: f32, bounce: f32, clinks: u8 },
    /// Rising expanding smoke puff.
    Smoke { vel: Vec3, grow: f32 },
    /// Uniform scale from `from` to `to` over its life (ease-out cubic).
    Grow { from: Vec3, to: Vec3 },
    /// Keeps its length; shrinks on X/Z (beams, tracers) over its life.
    Thin,
    /// Static (lights, flashes).
    Hold,
}

/// A transient effect entity.
#[derive(Component, Debug, Clone)]
pub struct Fx {
    pub age: f32,
    pub life: f32,
    pub motion: FxMotion,
    pub base_scale: Vec3,
}

impl Fx {
    pub fn new(life: f32, motion: FxMotion, base_scale: Vec3) -> Self {
        Fx { age: 0.0, life, motion, base_scale }
    }
    pub fn t(&self) -> f32 {
        (self.age / self.life).clamp(0.0, 1.0)
    }
}

/// Quantized fade steps of a shared additive-glow palette.
pub const FADE_STEPS: usize = 12;
/// Upper bound of distinct fade palettes (colors x alphas) ever allocated.
pub const MAX_FADE_PALETTES: usize = 48;
/// Simultaneous transient particles (sparks, gibs, debris, smoke, brass).
pub const MAX_FX_PARTICLES: usize = 180;
/// Simultaneous transient flash lights.
pub const MAX_FX_LIGHTS: usize = 6;

/// Shared materials of one additive glow at [`FADE_STEPS`] fade levels
/// (step 0 = full alpha / emissive, last step = fully faded).
pub type FadePalette = Arc<[Handle<StandardMaterial>; FADE_STEPS]>;

/// Bounded cache of fade palettes keyed by quantized `(color, alpha)`.
#[derive(Default)]
pub struct FadeCache {
    entries: Vec<([u8; 4], FadePalette)>,
}

impl FadeCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn fade_key(color: Color, alpha: f32) -> [u8; 4] {
    let c = color.to_srgba();
    [c.red, c.green, c.blue, alpha].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Fade factor of palette step `s`.
pub fn fade_factor(step: usize) -> f32 {
    1.0 - step as f32 / (FADE_STEPS - 1) as f32
}

/// Palette step for normalized effect time `t` (nearest of the quantized levels).
pub fn fade_step(t: f32) -> usize {
    ((t.clamp(0.0, 1.0) * (FADE_STEPS - 1) as f32) + 0.5) as usize
}

/// Fades an effect over its [`Fx`] life by swapping between the shared,
/// pre-allocated materials of its palette (no per-effect material).
#[derive(Component, Debug, Clone)]
pub struct FxFade {
    pub palette: FadePalette,
    pub step: usize,
}

impl FxFade {
    pub fn new(palette: FadePalette) -> Self {
        FxFade { palette, step: 0 }
    }

    /// Material for the first (unfaded) frame.
    pub fn first(&self) -> Handle<StandardMaterial> {
        self.palette[0].clone()
    }
}

/// Point light whose intensity decays to zero over the [`Fx`] life.
#[derive(Component, Debug, Clone, Copy)]
pub struct FxLight {
    pub intensity: f32,
}

/// Shared effect materials and meshes.
#[derive(Resource, Clone)]
pub struct FxAssets {
    pub spark: Handle<StandardMaterial>,
    pub fire: Handle<StandardMaterial>,
    pub fire_halo: Handle<StandardMaterial>,
    pub cyan: Handle<StandardMaterial>,
    pub cyan_core: Handle<StandardMaterial>,
    pub cyan_halo: Handle<StandardMaterial>,
    pub green: Handle<StandardMaterial>,
    pub green_halo: Handle<StandardMaterial>,
    pub gore: Handle<StandardMaterial>,
    pub gold: Handle<StandardMaterial>,
    pub smoke: Handle<StandardMaterial>,
    pub tracer: Handle<StandardMaterial>,
    pub tracer_core: Handle<StandardMaterial>,
    pub shell_red: Handle<StandardMaterial>,
    pub shell_brass: Handle<StandardMaterial>,
    pub cube: Handle<Mesh>,
    pub shard: Handle<Mesh>,
    pub sphere: Handle<Mesh>,
    pub beam: Handle<Mesh>,
    pub ring: Handle<Mesh>,
    pub shell_cyl: Handle<Mesh>,
    /// Chunky faceted brass Signal-Mortar sleeve (sculpted, shared).
    pub mortar_sleeve: Handle<Mesh>,
    /// Sculpted faceted death gibs (unit-ish scale; see [`spawn_death_gibs`]).
    pub gib_chunk: Handle<Mesh>,
    pub gib_horn: Handle<Mesh>,
    pub gib_plate: Handle<Mesh>,
    pub gib_shard: Handle<Mesh>,
    /// Shared gib materials (indexed by [`GibMat`]).
    pub gib_mats: [Handle<StandardMaterial>; GIB_MAT_COUNT],
    /// Missed Call decoy handset (shared by every decoy).
    pub decoy_body: Handle<StandardMaterial>,
    pub decoy_screen_amber: Handle<StandardMaterial>,
    pub decoy_screen_cyan: Handle<StandardMaterial>,
    pub decoy_ring: Handle<StandardMaterial>,
    /// Shared fade palettes (see [`FxAssets::fade_palette`]).
    pub fades: Arc<Mutex<FadeCache>>,
}

/// Shared death-gib material slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GibMat {
    Flesh,
    Bone,
    DarkMetal,
    FiendGreen,
    VioletObsidian,
    MoltenGold,
}

pub const GIB_MAT_COUNT: usize = 6;

impl GibMat {
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Sculpted gib mesh builders `(chunk, horn, plate, shard)`: every one is a
/// `TriangleList` with `{POSITION, NORMAL, UV_0}` and `Indices::U32`.
pub fn gib_meshes() -> [Mesh; 4] {
    [
        crate::sculpt::ridged_sphere(Vec3::new(1.0, 0.78, 0.9), 4, 6, 0.14, 3.0, true),
        crate::sculpt::curved_horn(0.32, 0.04, 1.7, 0.55, 0.2, 4, 5),
        crate::sculpt::wedge_plate(1.4, 0.32, 1.0, 0.7, 0.8, 0.12),
        crate::sculpt::tapered_prism(4, 0.42, 0.05, 1.6, true),
    ]
}

/// Faceted brass mortar sleeve (shared by every ejected Signal-Mortar casing).
pub fn mortar_sleeve_mesh() -> Mesh {
    crate::sculpt::tapered_prism(8, 0.034, 0.03, 0.115, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstColor {
    Spark,
    Fire,
    Cyan,
    Green,
    Gore,
    Gold,
    Smoke,
}

impl FxAssets {
    pub fn mat(&self, c: BurstColor) -> Handle<StandardMaterial> {
        match c {
            BurstColor::Spark => self.spark.clone(),
            BurstColor::Fire => self.fire.clone(),
            BurstColor::Cyan => self.cyan.clone(),
            BurstColor::Green => self.green.clone(),
            BurstColor::Gore => self.gore.clone(),
            BurstColor::Gold => self.gold.clone(),
            BurstColor::Smoke => self.smoke.clone(),
        }
    }

    /// Shared fade palette of an additive glow: built once per distinct
    /// `(color, alpha)` (bounded by [`MAX_FADE_PALETTES`]; beyond that the
    /// closest existing palette is reused), then only cloned.
    pub fn fade_palette(&self, materials: &mut Assets<StandardMaterial>, color: Color, alpha: f32) -> FadePalette {
        let key = fade_key(color, alpha);
        let mut cache = self.fades.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, p)) = cache.entries.iter().find(|(k, _)| *k == key) {
            return p.clone();
        }
        if cache.entries.len() >= MAX_FADE_PALETTES {
            let dist = |k: &[u8; 4]| k.iter().zip(key).map(|(a, b)| (*a as i32 - b as i32).pow(2)).sum::<i32>();
            let (_, p) = cache.entries.iter().min_by_key(|(k, _)| dist(k)).expect("non-empty cache");
            return p.clone();
        }
        let base = additive_glow(color, alpha);
        let palette: FadePalette = Arc::new(std::array::from_fn(|step| {
            let k = fade_factor(step);
            let mut m = base.clone();
            m.base_color.set_alpha(alpha * k);
            m.emissive = base.emissive * k;
            materials.add(m)
        }));
        cache.entries.push((key, palette.clone()));
        palette
    }

    /// Pre-build palettes at startup so gameplay never allocates materials.
    pub fn prewarm_fades(&self, materials: &mut Assets<StandardMaterial>, glows: &[(Color, f32)]) {
        for &(c, a) in glows {
            self.fade_palette(materials, c, a);
        }
    }

    pub fn light_color(c: BurstColor) -> Color {
        match c {
            BurstColor::Spark | BurstColor::Gold => Color::srgb(1.0, 0.82, 0.42),
            BurstColor::Fire => Color::srgb(1.0, 0.45, 0.1),
            BurstColor::Cyan => Color::srgb(0.25, 0.95, 1.0),
            BurstColor::Green => Color::srgb(0.4, 1.0, 0.25),
            BurstColor::Gore | BurstColor::Smoke => Color::srgb(1.0, 0.12, 0.06),
        }
    }
}

fn glow(color: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial { base_color: color, emissive, unlit: true, ..default() }
}

/// Additive translucent glow material (rings, beams, shockwaves).
pub fn additive_glow(color: Color, alpha: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: color.with_alpha(alpha),
        emissive: color.to_linear() * 2.4,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..default()
    }
}

fn create_fx_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let [gib_chunk, gib_horn, gib_plate, gib_shard] = gib_meshes();
    commands.insert_resource(FxAssets {
        spark: materials.add(glow(Color::srgb(1.0, 0.9, 0.5), LinearRgba::rgb(10.0, 6.5, 2.0))),
        fire: materials.add(glow(Color::srgb(1.0, 0.48, 0.12), LinearRgba::rgb(12.0, 3.6, 0.4))),
        fire_halo: materials.add(additive_glow(Color::srgb(1.0, 0.42, 0.08), 0.55)),
        cyan: materials.add(glow(Color::srgb(0.35, 0.98, 1.0), LinearRgba::rgb(1.5, 9.0, 12.0))),
        cyan_core: materials.add(glow(Color::srgb(0.92, 1.0, 1.0), LinearRgba::rgb(10.0, 18.0, 22.0))),
        cyan_halo: materials.add(additive_glow(Color::srgb(0.25, 0.92, 1.0), 0.55)),
        green: materials.add(glow(Color::srgb(0.5, 1.0, 0.3), LinearRgba::rgb(2.0, 10.0, 1.2))),
        green_halo: materials.add(additive_glow(Color::srgb(0.35, 1.0, 0.2), 0.55)),
        gore: materials.add(StandardMaterial {
            base_color: Color::srgb(0.42, 0.015, 0.015),
            emissive: LinearRgba::rgb(0.55, 0.01, 0.01),
            perceptual_roughness: 0.32,
            ..default()
        }),
        gold: materials.add(glow(Color::srgb(1.0, 0.8, 0.25), LinearRgba::rgb(10.0, 5.8, 0.6))),
        smoke: materials.add(StandardMaterial {
            base_color: Color::srgba(0.14, 0.12, 0.12, 0.65),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 1.0,
            ..default()
        }),
        tracer: materials.add(additive_glow(Color::srgb(1.0, 0.72, 0.3), 0.7)),
        tracer_core: materials.add(glow(Color::srgb(1.0, 0.96, 0.82), LinearRgba::rgb(12.0, 9.0, 4.0))),
        shell_red: materials.add(StandardMaterial { base_color: Color::srgb(0.75, 0.08, 0.05), perceptual_roughness: 0.45, ..default() }),
        shell_brass: materials.add(StandardMaterial { base_color: Color::srgb(0.9, 0.72, 0.25), metallic: 0.9, perceptual_roughness: 0.25, ..default() }),
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        shard: meshes.add(Cone::new(0.6, 1.5)),
        sphere: meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()),
        beam: meshes.add(Cylinder::new(1.0, 1.0)),
        ring: meshes.add(Torus::new(0.94, 1.0)),
        shell_cyl: meshes.add(Cylinder::new(0.014, 0.055)),
        mortar_sleeve: meshes.add(mortar_sleeve_mesh()),
        gib_chunk: meshes.add(gib_chunk),
        gib_horn: meshes.add(gib_horn),
        gib_plate: meshes.add(gib_plate),
        gib_shard: meshes.add(gib_shard),
        gib_mats: [
            // Flesh / gore: wet dark crimson.
            materials.add(StandardMaterial { base_color: Color::srgb(0.38, 0.03, 0.03), perceptual_roughness: 0.28, reflectance: 0.6, ..default() }),
            // Bone.
            materials.add(StandardMaterial { base_color: Color::srgb(0.82, 0.76, 0.62), perceptual_roughness: 0.6, ..default() }),
            // Dark metal.
            materials.add(StandardMaterial { base_color: Color::srgb(0.12, 0.13, 0.15), metallic: 0.85, perceptual_roughness: 0.38, ..default() }),
            // Fiend green (faint toxic glow).
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.22, 0.55, 0.1),
                emissive: LinearRgba::rgb(0.12, 0.6, 0.05),
                perceptual_roughness: 0.4,
                ..default()
            }),
            // Violet obsidian.
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.18, 0.05, 0.26),
                emissive: LinearRgba::rgb(0.35, 0.04, 0.5),
                metallic: 0.4,
                perceptual_roughness: 0.22,
                ..default()
            }),
            // Molten gold.
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.95, 0.66, 0.18),
                emissive: LinearRgba::rgb(1.4, 0.55, 0.06),
                metallic: 0.9,
                perceptual_roughness: 0.3,
                ..default()
            }),
        ],
        decoy_body: materials.add(StandardMaterial { base_color: Color::srgb(0.05, 0.06, 0.07), perceptual_roughness: 0.35, metallic: 0.6, ..default() }),
        decoy_screen_amber: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.7, 0.15),
            emissive: LinearRgba::rgb(6.0, 3.6, 0.6),
            unlit: true,
            ..default()
        }),
        decoy_screen_cyan: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.7, 0.15),
            emissive: LinearRgba::rgb(0.6, 4.5, 6.0),
            unlit: true,
            ..default()
        }),
        decoy_ring: materials.add(additive_glow(Color::srgb(1.0, 0.75, 0.2), 0.7)),
        fades: Arc::new(Mutex::new(FadeCache::default())),
    });
}

/// Beam layers `(radius multiplier, alpha)` of [`spawn_beam`].
pub const BEAM_LAYERS: [(f32, f32); 2] = [(0.45, 0.72), (1.0, 0.32)];

/// Fade palettes for beams of `color` (both layers).
pub fn beam_glows(color: Color) -> [(Color, f32); 2] {
    BEAM_LAYERS.map(|(_, a)| (color, a))
}

/// Burst of varied ballistic shards/gibs + short core flash + point light.
pub fn spawn_burst(commands: &mut Commands, fx: &FxAssets, pos: Vec3, color: BurstColor, count: u32, speed: f32, size: f32) {
    spawn_particles(commands, fx, pos, color, count, speed, size);
    // Crisp hot core sphere that pops & fades quickly on non-smoke bursts.
    if !matches!(color, BurstColor::Smoke) && count >= 5 {
        let core_size = (size * 2.2).clamp(0.16, 0.65);
        commands.spawn((
            LevelEntity,
            Fx::new(
                0.14,
                FxMotion::Grow { from: Vec3::splat(core_size * 0.4), to: Vec3::splat(core_size * 1.9) },
                Vec3::ONE,
            ),
            Mesh3d(fx.sphere.clone()),
            MeshMaterial3d(fx.mat(color)),
            Transform::from_translation(pos).with_scale(Vec3::splat(core_size * 0.4)),
        ));
    }
    spawn_flash_light(commands, pos, FxAssets::light_color(color), 240_000.0 * (count as f32 / 8.0).clamp(0.5, 4.5), 8.0, 0.14);
}

/// Ballistic shards/gibs/smoke (no light): varied shapes, floor bounce on gore/debris.
pub fn spawn_particles(commands: &mut Commands, fx: &FxAssets, pos: Vec3, color: BurstColor, count: u32, speed: f32, size: f32) {
    let mut rng = rand::rng();
    for i in 0..count {
        let dir = Vec3::new(rng.random_range(-1.0..1.0), rng.random_range(-0.2..1.0), rng.random_range(-1.0..1.0)).normalize_or_zero();
        let vel = dir * speed * rng.random_range(0.35..1.1) + Vec3::Y * speed * rng.random_range(0.18..0.45);
        let s = size * rng.random_range(0.55..1.35);

        if color == BurstColor::Smoke {
            commands.spawn((
                LevelEntity,
                Fx::new(
                    rng.random_range(0.45..0.95),
                    FxMotion::Smoke { vel: vel * 0.3 + Vec3::Y * 1.4, grow: rng.random_range(1.6..2.6) },
                    Vec3::splat(s),
                ),
                Mesh3d(fx.sphere.clone()),
                MeshMaterial3d(fx.smoke.clone()),
                Transform::from_translation(pos).with_scale(Vec3::splat(s)),
            ));
            continue;
        }

        let is_gore = color == BurstColor::Gore;
        let mesh = if i % 3 == 0 {
            fx.shard.clone()
        } else if is_gore && i % 2 == 0 {
            fx.sphere.clone()
        } else {
            fx.cube.clone()
        };
        let scale = if matches!(color, BurstColor::Spark | BurstColor::Cyan | BurstColor::Fire) {
            // Elongated hot spark streaks!
            Vec3::new(s * 0.45, s * 1.65, s * 0.45)
        } else {
            Vec3::new(s * rng.random_range(0.7..1.3), s * rng.random_range(0.7..1.3), s * rng.random_range(0.7..1.3))
        };
        let motion = if is_gore {
            FxMotion::Debris {
                vel,
                spin: Vec3::new(rng.random_range(-10.0..10.0), rng.random_range(-10.0..10.0), rng.random_range(-10.0..10.0)),
                gravity: 20.0,
                bounce: 0.35,
                clinks: 0,
            }
        } else {
            FxMotion::Particle { vel, gravity: 11.0 }
        };
        commands.spawn((
            LevelEntity,
            Fx::new(rng.random_range(0.32..0.78), motion, scale),
            Mesh3d(mesh),
            MeshMaterial3d(fx.mat(color)),
            Transform::from_translation(pos)
                .with_scale(scale)
                .with_rotation(Quat::from_euler(EulerRot::XYZ, rng.random(), rng.random(), rng.random())),
        ));
    }
}

/// Floor bounces of an ejected casing that clink (the rest rattle silently).
pub const CASING_CLINKS: u8 = 2;

/// Spinning red-and-brass shotgun shell ejected from the breach on fire.
pub fn spawn_ejected_shell(commands: &mut Commands, fx: &FxAssets, pos: Vec3, right: Vec3, up: Vec3, fwd: Vec3) {
    let mut rng = rand::rng();
    let vel = right * rng.random_range(2.6..3.8) + up * rng.random_range(1.8..2.7) - fwd * rng.random_range(0.3..1.0);
    let spin = Vec3::new(rng.random_range(-16.0..16.0), rng.random_range(-12.0..12.0), rng.random_range(-20.0..-10.0));
    let root = commands
        .spawn((
            LevelEntity,
            Fx::new(1.15, FxMotion::Debris { vel, spin, gravity: 16.0, bounce: 0.45, clinks: CASING_CLINKS }, Vec3::ONE),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
            Visibility::default(),
        ))
        .id();
    commands.spawn((
        ChildOf(root),
        Mesh3d(fx.shell_cyl.clone()),
        MeshMaterial3d(fx.shell_red.clone()),
        Transform::from_xyz(0.0, 0.006, 0.0),
    ));
    commands.spawn((
        ChildOf(root),
        Mesh3d(fx.shell_cyl.clone()),
        MeshMaterial3d(fx.shell_brass.clone()),
        Transform::from_xyz(0.0, -0.022, 0.0).with_scale(Vec3::new(1.08, 0.36, 1.08)),
    ));
}

/// Chunky spinning brass Signal-Mortar sleeve kicked out of the breech on fire.
pub fn spawn_mortar_casing(commands: &mut Commands, fx: &FxAssets, pos: Vec3, right: Vec3, up: Vec3, fwd: Vec3) {
    let mut rng = rand::rng();
    let vel = right * rng.random_range(1.8..2.6) + up * rng.random_range(2.2..3.0) - fwd * rng.random_range(0.6..1.2);
    let spin = Vec3::new(rng.random_range(-9.0..9.0), rng.random_range(-6.0..6.0), rng.random_range(-14.0..-7.0));
    commands.spawn((
        LevelEntity,
        Fx::new(1.6, FxMotion::Debris { vel, spin, gravity: 15.0, bounce: 0.32, clinks: CASING_CLINKS }, Vec3::ONE),
        Mesh3d(fx.mortar_sleeve.clone()),
        MeshMaterial3d(fx.shell_brass.clone()),
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
    ));
}

/// Subtle hot cyan vent sparks puffed from the Plasma Repeater's cooling port.
pub fn spawn_plasma_vent(commands: &mut Commands, fx: &FxAssets, pos: Vec3, up: Vec3, right: Vec3) {
    let mut rng = rand::rng();
    for _ in 0..2 {
        let vel = up * rng.random_range(0.9..1.6) + right * rng.random_range(-0.4..0.6);
        let s = rng.random_range(0.006..0.011);
        let scale = Vec3::new(s * 0.5, s * 1.8, s * 0.5);
        commands.spawn((
            LevelEntity,
            Fx::new(rng.random_range(0.12..0.22), FxMotion::Particle { vel, gravity: -1.5 }, scale),
            Mesh3d(fx.cube.clone()),
            MeshMaterial3d(fx.cyan.clone()),
            Transform::from_translation(pos).with_scale(scale),
        ));
    }
}

/// Burst of chunky sculpted spinning gibs (archetype-flavored by `mats`, cycled
/// per piece). Every gib is a bouncing [`FxMotion::Debris`] particle, so the
/// [`MAX_FX_PARTICLES`] cap bounds them like every other transient particle.
pub fn spawn_death_gibs(commands: &mut Commands, fx: &FxAssets, center: Vec3, mats: &[GibMat], count: u32, size: f32, speed: f32) {
    if mats.is_empty() {
        return;
    }
    let mut rng = rand::rng();
    let meshes = [&fx.gib_chunk, &fx.gib_horn, &fx.gib_plate, &fx.gib_shard];
    for i in 0..count as usize {
        let dir = Vec3::new(rng.random_range(-1.0..1.0), rng.random_range(0.1..1.0), rng.random_range(-1.0..1.0)).normalize_or(Vec3::Y);
        let vel = dir * speed * rng.random_range(0.45..1.0) + Vec3::Y * speed * rng.random_range(0.25..0.5);
        let spin = Vec3::new(rng.random_range(-12.0..12.0), rng.random_range(-9.0..9.0), rng.random_range(-12.0..12.0));
        let s = size * rng.random_range(0.7..1.3);
        let scale = Vec3::splat(s);
        let offset = Vec3::new(rng.random_range(-0.3..0.3), rng.random_range(-0.3..0.3), rng.random_range(-0.3..0.3)) * size * 3.0;
        commands.spawn((
            LevelEntity,
            Fx::new(rng.random_range(1.4..2.2), FxMotion::Debris { vel, spin, gravity: 19.0, bounce: 0.3, clinks: 0 }, scale),
            Mesh3d(meshes[i % meshes.len()].clone()),
            MeshMaterial3d(fx.gib_mats[mats[i % mats.len()].index()].clone()),
            Transform::from_translation(center + offset)
                .with_scale(scale)
                .with_rotation(Quat::from_euler(EulerRot::XYZ, rng.random::<f32>() * 6.0, rng.random::<f32>() * 6.0, rng.random::<f32>() * 6.0)),
        ));
    }
}

/// Contact radius of bouncing debris above the floor.
pub const DEBRIS_RADIUS: f32 = 0.03;
/// Highest floor rise debris rolls up onto (ramps); taller ledges block it like walls.
pub const DEBRIS_STEP: f32 = 0.25;

/// One integration step of tumbling debris against the real floor heights
/// `floor_at(x, z)` (`None` = solid wall): gravity, per-axis ledge / wall
/// rebounds, floor bounce with velocity + spin damping, and smooth rolling
/// friction while resting. Returns `(pos, vel, spin)`.
#[allow(clippy::too_many_arguments)]
pub fn debris_step(pos: Vec3, vel: Vec3, spin: Vec3, gravity: f32, bounce: f32, dt: f32, floor_at: impl Fn(f32, f32) -> Option<f32>) -> (Vec3, Vec3, Vec3) {
    if !dt.is_finite() || dt <= 0.0 {
        return (pos, vel, spin);
    }
    let mut vel = vel;
    let mut spin = spin;
    vel.y -= gravity * dt;
    let here = floor_at(pos.x, pos.z);
    // Blocked moving into a wall, or into a ledge rising above the debris.
    let blocked = |x: f32, z: f32| here.is_some() && floor_at(x, z).is_none_or(|h| h + DEBRIS_RADIUS > pos.y + DEBRIS_STEP);
    let mut p = pos;
    let nx = p.x + vel.x * dt;
    if blocked(nx, p.z) {
        vel.x = -vel.x * bounce;
        spin *= 0.7;
    } else {
        p.x = nx;
    }
    let nz = p.z + vel.z * dt;
    if blocked(p.x, nz) {
        vel.z = -vel.z * bounce;
        spin *= 0.7;
    } else {
        p.z = nz;
    }
    p.y += vel.y * dt;
    let floor_y = floor_at(p.x, p.z).or(here).unwrap_or(0.0) + DEBRIS_RADIUS;
    if p.y <= floor_y {
        p.y = floor_y;
        if vel.y < -0.5 {
            vel.y = -vel.y * bounce;
            vel.x *= 0.65;
            vel.z *= 0.65;
            spin *= 0.6;
        } else {
            // Resting / rolling: smooth exponential friction down to a stop.
            vel.y = 0.0;
            let f = (-8.0 * dt).exp();
            vel.x *= f;
            vel.z *= f;
            spin *= (-6.0 * dt).exp();
            if vel.x.abs() + vel.z.abs() < 0.02 {
                vel.x = 0.0;
                vel.z = 0.0;
            }
            if spin.length_squared() < 0.01 {
                spin = Vec3::ZERO;
            }
        }
    }
    (p, vel, spin)
}

pub fn spawn_flash_light(commands: &mut Commands, pos: Vec3, color: Color, intensity: f32, range: f32, life: f32) {
    commands.spawn((
        LevelEntity,
        Fx::new(life, FxMotion::Hold, Vec3::ONE),
        FxLight { intensity },
        CullableLight::for_range(range),
        PointLight { color, intensity, range, shadow_maps_enabled: false, ..default() },
        Transform::from_translation(pos),
    ));
}

/// Thin glowing segment from `a` to `b` (tracers, lightning).
pub fn spawn_segment(commands: &mut Commands, fx: &FxAssets, a: Vec3, b: Vec3, width: f32, life: f32, material: Handle<StandardMaterial>) {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 {
        return;
    }
    let rot = Quat::from_rotation_arc(Vec3::Y, d / len);
    let scale = Vec3::new(width, len, width);
    commands.spawn((
        LevelEntity,
        Fx::new(life, FxMotion::Thin, scale),
        Mesh3d(fx.beam.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(a + d * 0.5).with_rotation(rot).with_scale(scale),
    ));
}

/// Dual-layer shotgun pellet tracer (hot bright core + outer warm glow sheath).
pub fn spawn_tracer(commands: &mut Commands, fx: &FxAssets, a: Vec3, b: Vec3) {
    spawn_segment(commands, fx, a, b, 0.008, 0.065, fx.tracer_core.clone());
    spawn_segment(commands, fx, a, b, 0.022, 0.08, fx.tracer.clone());
}

/// Jagged dual-layer chain-lightning bolt with crackling side branches.
pub fn spawn_lightning(commands: &mut Commands, fx: &FxAssets, a: Vec3, b: Vec3) {
    let mut rng = rand::rng();
    let segs = 7;
    let mut prev = a;
    for i in 1..=segs {
        let t = i as f32 / segs as f32;
        let mut p = a.lerp(b, t);
        if i < segs {
            p += Vec3::new(rng.random_range(-0.45..0.45), rng.random_range(-0.45..0.45), rng.random_range(-0.45..0.45));
        }
        // Bright white-cyan core + wider electric sheath.
        spawn_segment(commands, fx, prev, p, 0.026, 0.24, fx.cyan_core.clone());
        spawn_segment(commands, fx, prev, p, 0.075, 0.28, fx.cyan.clone());
        // Occasional small side arc branch.
        if i > 1 && i < segs && rng.random_bool(0.45) {
            let fork = p + Vec3::new(rng.random_range(-0.9..0.9), rng.random_range(-0.6..0.8), rng.random_range(-0.9..0.9));
            spawn_segment(commands, fx, p, fork, 0.022, 0.18, fx.cyan.clone());
        }
        prev = p;
    }
    spawn_particles(commands, fx, b, BurstColor::Cyan, 6, 5.5, 0.06);
    spawn_flash_light(commands, b, Color::srgb(0.3, 0.92, 1.0), 480_000.0, 9.0, 0.22);
}

/// Expanding translucent sphere / ring with its own fading material.
#[allow(clippy::too_many_arguments)]
pub fn spawn_shockwave(
    commands: &mut Commands,
    fx: &FxAssets,
    materials: &mut Assets<StandardMaterial>,
    pos: Vec3,
    radius: f32,
    life: f32,
    color: Color,
    alpha: f32,
    ring: bool,
) {
    let fade = FxFade::new(fx.fade_palette(materials, color, alpha));
    let (mesh, from, to) = if ring {
        (fx.ring.clone(), Vec3::new(0.3, 0.8, 0.3), Vec3::new(radius, radius * 0.1 + 0.8, radius))
    } else {
        (fx.sphere.clone(), Vec3::splat(0.3), Vec3::splat(radius))
    };
    commands.spawn((
        LevelEntity,
        Fx::new(life, FxMotion::Grow { from, to }, Vec3::ONE),
        MeshMaterial3d(fade.first()),
        fade,
        Mesh3d(mesh),
        Transform::from_translation(pos).with_scale(from),
    ));
}

/// Tall vertical light beam (core + soft outer halo). `xray` beams render on
/// the overlay layer and are visible through walls (Ping Scan beacons).
#[allow(clippy::too_many_arguments)]
pub fn spawn_beam(
    commands: &mut Commands,
    fx: &FxAssets,
    materials: &mut Assets<StandardMaterial>,
    base: Vec3,
    height: f32,
    radius: f32,
    life: f32,
    color: Color,
    xray: bool,
) {
    for (r_mul, a_mul) in BEAM_LAYERS {
        let fade = FxFade::new(fx.fade_palette(materials, color, a_mul));
        let scale = Vec3::new(radius * r_mul, height, radius * r_mul);
        let mut e = commands.spawn((
            LevelEntity,
            Fx::new(life, FxMotion::Thin, scale),
            MeshMaterial3d(fade.first()),
            fade,
            Mesh3d(fx.beam.clone()),
            Transform::from_translation(base + Vec3::Y * height * 0.5).with_scale(scale),
        ));
        if xray {
            e.insert(overlay_layer());
        }
    }
}

// ---------------------------------------------------------------------------
// HUD feedback channel
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub color: Color,
    pub timer: f32,
}

/// Gameplay -> HUD messages (hitmarker, toasts, big alerts).
#[derive(Resource, Debug, Default, Clone)]
pub struct HudFeed {
    /// Seconds remaining on the crosshair hitmarker.
    pub hitmarker: f32,
    /// Hitmarker was a kill (red X).
    pub hit_kill: bool,
    pub toasts: Vec<Toast>,
    /// Large center alert (e.g. Hell Baron warning).
    pub alert: Option<Toast>,
}

pub const TOAST_SECONDS: f32 = 3.2;
pub const MAX_TOASTS: usize = 5;

impl HudFeed {
    pub fn hit(&mut self, kill: bool) {
        self.hitmarker = 0.18;
        self.hit_kill |= kill;
    }

    pub fn toast(&mut self, text: impl Into<String>, color: Color) {
        self.toasts.push(Toast { text: text.into(), color, timer: TOAST_SECONDS });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
    }

    pub fn alert(&mut self, text: impl Into<String>, color: Color, secs: f32) {
        self.alert = Some(Toast { text: text.into(), color, timer: secs });
    }

    pub fn tick(&mut self, dt: f32) {
        self.hitmarker = (self.hitmarker - dt).max(0.0);
        if self.hitmarker <= 0.0 {
            self.hit_kill = false;
        }
        for t in &mut self.toasts {
            t.timer -= dt;
        }
        self.toasts.retain(|t| t.timer > 0.0);
        if let Some(a) = &mut self.alert {
            a.timer -= dt;
            if a.timer <= 0.0 {
                self.alert = None;
            }
        }
    }
}

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudFeed>()
            .add_systems(PreStartup, create_fx_assets)
            .add_systems(Update, (animate_fx, tick_feed).in_set(GameSet::World));
    }
}

fn tick_feed(time: Res<Time>, mut feed: ResMut<HudFeed>) {
    feed.tick(time.delta_secs());
}

#[allow(clippy::type_complexity)]
fn animate_fx(
    mut commands: Commands,
    time: Res<Time>,
    grid: Option<Res<LevelGrid>>,
    mut sfx: Option<ResMut<SfxQueue>>,
    mut q: Query<(
        Entity,
        &mut Fx,
        &mut Transform,
        Option<(&mut FxFade, &mut MeshMaterial3d<StandardMaterial>)>,
        Option<&FxLight>,
        Option<&mut CullableLight>,
        Option<&mut PointLight>,
    )>,
    (mut particles, mut lights): (Local<Vec<(f32, Entity)>>, Local<Vec<(f32, Entity)>>),
) {
    let dt = time.delta_secs();
    particles.clear();
    lights.clear();
    for (e, mut fx, mut tf, fade, flight, cull_light, light) in &mut q {
        fx.age += dt;
        if fx.age >= fx.life {
            commands.entity(e).try_despawn();
            continue;
        }
        let t = fx.t();
        if matches!(fx.motion, FxMotion::Particle { .. } | FxMotion::Debris { .. } | FxMotion::Smoke { .. }) {
            particles.push((t, e));
        }
        if flight.is_some() {
            lights.push((t, e));
        }
        match &mut fx.motion {
            FxMotion::Particle { vel, gravity } => {
                vel.y -= *gravity * dt;
                tf.translation += *vel * dt;
                tf.scale = fx.base_scale * (1.0 - t * t);
                tf.rotate_x(dt * 8.0);
            }
            FxMotion::Debris { vel, spin, gravity, bounce, clinks } => {
                let prev_vy = vel.y;
                let (p, v, s) = match grid.as_deref() {
                    Some(g) => debris_step(tf.translation, *vel, *spin, *gravity, *bounce, dt, |x, z| sample_floor_height(g, x, z)),
                    None => debris_step(tf.translation, *vel, *spin, *gravity, *bounce, dt, |_, _| Some(0.0)),
                };
                tf.translation = p;
                *vel = v;
                *spin = s;
                if *clinks > 0 && is_clink_bounce(prev_vy, v.y) {
                    *clinks -= 1;
                    if let Some(q) = sfx.as_deref_mut() {
                        q.play_at(SfxEvent::ShellClink, p);
                    }
                }
                tf.rotate_x(spin.x * dt);
                tf.rotate_y(spin.y * dt);
                tf.rotate_z(spin.z * dt);
                if t > 0.72 {
                    let shrink = 1.0 - ((t - 0.72) / 0.28);
                    tf.scale = fx.base_scale * shrink.max(0.0);
                }
            }
            FxMotion::Smoke { vel, grow } => {
                *vel *= (1.0 - dt * 2.2).max(0.0);
                tf.translation += (*vel + Vec3::Y * 0.4) * dt;
                let s = 1.0 + (*grow - 1.0) * t;
                let shrink = if t > 0.75 { 1.0 - (t - 0.75) / 0.25 } else { 1.0 };
                tf.scale = fx.base_scale * s * shrink;
            }
            FxMotion::Grow { from, to } => {
                let k = 1.0 - (1.0 - t).powi(3);
                tf.scale = from.lerp(*to, k);
            }
            FxMotion::Thin => {
                let w = 1.0 - t;
                tf.scale = Vec3::new(fx.base_scale.x * w, fx.base_scale.y, fx.base_scale.z * w);
            }
            FxMotion::Hold => {}
        }
        if let Some((mut f, mut mat)) = fade {
            let step = fade_step(t);
            if step != f.step {
                f.step = step;
                mat.0 = f.palette[step].clone();
            }
        }
        if let Some(fl) = flight {
            let intensity = fl.intensity * (1.0 - t);
            if let Some(mut cull) = cull_light {
                cull.set_intensity(intensity);
            } else if let Some(mut l) = light {
                l.intensity = intensity;
            }
        }
    }
    // Bounded transient FX: recycle the oldest beyond the caps.
    recycle_oldest(&mut commands, &mut particles, MAX_FX_PARTICLES);
    recycle_oldest(&mut commands, &mut lights, MAX_FX_LIGHTS);
}

/// Despawn the entries closest to the end of their life beyond `cap`.
fn recycle_oldest(commands: &mut Commands, live: &mut [(f32, Entity)], cap: usize) {
    if live.len() <= cap {
        return;
    }
    let excess = live.len() - cap;
    live.select_nth_unstable_by(excess - 1, |a, b| b.0.total_cmp(&a.0));
    for &(_, e) in &live[..excess] {
        commands.entity(e).try_despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_toasts_expire_and_cap() {
        let mut f = HudFeed::default();
        for i in 0..8 {
            f.toast(format!("t{i}"), Color::WHITE);
        }
        assert_eq!(f.toasts.len(), MAX_TOASTS);
        assert_eq!(f.toasts[0].text, "t3");
        f.hit(true);
        f.alert("BOSS", Color::WHITE, 1.0);
        f.tick(0.5);
        assert!(f.hitmarker == 0.0 && !f.hit_kill);
        assert!(f.alert.is_some());
        f.tick(TOAST_SECONDS);
        assert!(f.toasts.is_empty());
        assert!(f.alert.is_none());
    }


    fn run_debris(start: Vec3, vel: Vec3, floor_at: impl Fn(f32, f32) -> Option<f32>) -> (Vec3, Vec3, Vec3) {
        let (mut p, mut v, mut s) = (start, vel, Vec3::new(9.0, -6.0, 12.0));
        for _ in 0..240 {
            (p, v, s) = debris_step(p, v, s, 18.0, 0.4, 1.0 / 60.0, &floor_at);
            assert!(p.is_finite() && v.is_finite() && s.is_finite());
        }
        (p, v, s)
    }

    #[test]
    fn debris_bounces_on_real_floor_heights_and_settles() {
        let ground = run_debris(Vec3::new(0.0, 1.5, 0.0), Vec3::new(1.0, 2.0, 0.5), |_, _| Some(0.0));
        assert!((ground.0.y - DEBRIS_RADIUS).abs() < 1e-4, "ground rest {:?}", ground.0);
        assert_eq!(ground.1, Vec3::ZERO, "settled");
        assert_eq!(ground.2, Vec3::ZERO, "spin damped out");
        let cat = crate::level::CATWALK_HEIGHT;
        let catwalk = run_debris(Vec3::new(0.0, cat + 1.5, 0.0), Vec3::new(1.0, 2.0, 0.5), |_, _| Some(cat));
        assert!((catwalk.0.y - (cat + DEBRIS_RADIUS)).abs() < 1e-4, "catwalk rest {:?}", catwalk.0);
        // First contact bounces (velocity reversed, damped).
        let (_, v, s) = debris_step(Vec3::new(0.0, cat + 0.05, 0.0), Vec3::new(2.0, -6.0, 0.0), Vec3::splat(10.0), 18.0, 0.4, 1.0 / 60.0, |_, _| Some(cat));
        assert!(v.y > 2.0 && v.y < 6.0 && v.x < 2.0 && s.x < 10.0, "bounce {v:?} {s:?}");
    }

    #[test]
    fn debris_rebounds_off_ledges_and_walls_instead_of_snapping_up() {
        // Ground at x < 1, a 2.4 m catwalk ledge beyond: rolling debris bounces back.
        let cat = crate::level::CATWALK_HEIGHT;
        let floor = |x: f32, _z: f32| if x < 1.0 { Some(0.0) } else { Some(cat) };
        let (p, _, _) = run_debris(Vec3::new(0.5, 0.2, 0.0), Vec3::new(4.0, 0.0, 0.0), floor);
        assert!(p.x < 1.0 && p.y < 0.1, "stayed below the ledge {p:?}");
        // Falling from above lands on the catwalk.
        let (p, _, _) = run_debris(Vec3::new(1.5, cat + 1.0, 0.0), Vec3::new(0.5, 0.0, 0.0), floor);
        assert!((p.y - (cat + DEBRIS_RADIUS)).abs() < 1e-4, "{p:?}");
        // Walls (None) block horizontally.
        let wall = |_x: f32, z: f32| if z < 1.0 { Some(0.0) } else { None };
        let (p, _, _) = run_debris(Vec3::new(0.0, 0.5, 0.5), Vec3::new(0.0, 0.0, 5.0), wall);
        assert!(p.z < 1.0, "{p:?}");
    }

    #[test]
    fn gib_and_casing_meshes_are_rt_convertible() {
        use bevy::mesh::Indices;
        for m in gib_meshes().iter().chain([mortar_sleeve_mesh()].iter()) {
            assert!(crate::raytracing::rt_mesh_convertible(m));
            assert!(matches!(m.indices(), Some(Indices::U32(i)) if !i.is_empty() && i.len().is_multiple_of(3)));
            assert!(m.contains_attribute(Mesh::ATTRIBUTE_UV_0));
            assert!(crate::raytracing::triangle_count(m) < 2_000);
        }
    }
}
