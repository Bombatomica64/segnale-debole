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

use crate::level::{LevelEntity, LevelGrid};
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
    Debris { vel: Vec3, spin: Vec3, gravity: f32, bounce: f32 },
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

/// Fades a per-entity material's alpha (and emissive) over the [`Fx`] life.
#[derive(Component, Debug, Clone)]
pub struct FxFade {
    pub material: Handle<StandardMaterial>,
    pub alpha: f32,
    pub emissive: LinearRgba,
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
    });
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

/// Spinning red-and-brass shotgun shell ejected from the breach on fire.
pub fn spawn_ejected_shell(commands: &mut Commands, fx: &FxAssets, pos: Vec3, right: Vec3, up: Vec3, fwd: Vec3) {
    let mut rng = rand::rng();
    let vel = right * rng.random_range(2.6..3.8) + up * rng.random_range(1.8..2.7) - fwd * rng.random_range(0.3..1.0);
    let spin = Vec3::new(rng.random_range(-16.0..16.0), rng.random_range(-12.0..12.0), rng.random_range(-20.0..-10.0));
    let root = commands
        .spawn((
            LevelEntity,
            Fx::new(1.15, FxMotion::Debris { vel, spin, gravity: 16.0, bounce: 0.45 }, Vec3::ONE),
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

pub fn spawn_flash_light(commands: &mut Commands, pos: Vec3, color: Color, intensity: f32, range: f32, life: f32) {
    commands.spawn((
        LevelEntity,
        Fx::new(life, FxMotion::Hold, Vec3::ONE),
        FxLight { intensity },
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
    let mat = additive_glow(color, alpha);
    let emissive = mat.emissive;
    let handle = materials.add(mat);
    let (mesh, from, to) = if ring {
        (fx.ring.clone(), Vec3::new(0.3, 0.8, 0.3), Vec3::new(radius, radius * 0.1 + 0.8, radius))
    } else {
        (fx.sphere.clone(), Vec3::splat(0.3), Vec3::splat(radius))
    };
    commands.spawn((
        LevelEntity,
        Fx::new(life, FxMotion::Grow { from, to }, Vec3::ONE),
        FxFade { material: handle.clone(), alpha, emissive },
        Mesh3d(mesh),
        MeshMaterial3d(handle),
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
    for (r_mul, a_mul) in [(0.45, 0.72), (1.0, 0.32)] {
        let mat = additive_glow(color, a_mul);
        let emissive = mat.emissive;
        let handle = materials.add(mat);
        let scale = Vec3::new(radius * r_mul, height, radius * r_mul);
        let mut e = commands.spawn((
            LevelEntity,
            Fx::new(life, FxMotion::Thin, scale),
            FxFade { material: handle.clone(), alpha: a_mul, emissive },
            Mesh3d(fx.beam.clone()),
            MeshMaterial3d(handle),
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
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut q: Query<(Entity, &mut Fx, &mut Transform, Option<&FxFade>, Option<&FxLight>, Option<&mut PointLight>)>,
) {
    let dt = time.delta_secs();
    for (e, mut fx, mut tf, fade, flight, light) in &mut q {
        fx.age += dt;
        if fx.age >= fx.life {
            commands.entity(e).try_despawn();
            continue;
        }
        let t = fx.t();
        match &mut fx.motion {
            FxMotion::Particle { vel, gravity } => {
                vel.y -= *gravity * dt;
                tf.translation += *vel * dt;
                tf.scale = fx.base_scale * (1.0 - t * t);
                tf.rotate_x(dt * 8.0);
            }
            FxMotion::Debris { vel, spin, gravity, bounce } => {
                vel.y -= *gravity * dt;
                tf.translation += *vel * dt;
                let floor_y = grid
                    .as_deref()
                    .and_then(|g| sample_floor_height(g, tf.translation.x, tf.translation.z))
                    .unwrap_or(0.0)
                    + 0.03;
                if tf.translation.y < floor_y {
                    tf.translation.y = floor_y;
                    if vel.y < -0.5 {
                        vel.y = -vel.y * *bounce;
                        vel.x *= 0.65;
                        vel.z *= 0.65;
                        *spin *= 0.6;
                    } else {
                        *vel = Vec3::ZERO;
                        *spin = Vec3::ZERO;
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
        if let Some(f) = fade
            && let Some(mut m) = materials.get_mut(&f.material)
        {
            let k = 1.0 - t;
            m.base_color.set_alpha(f.alpha * k);
            m.emissive = f.emissive * k;
        }
        if let (Some(fl), Some(mut l)) = (flight, light) {
            l.intensity = fl.intensity * (1.0 - t);
        }
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
}
