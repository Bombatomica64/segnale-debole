//! Milestone 30: live 3D Mars Signal Relay Tower vista behind the main menu.
//!
//! A slowly orbiting camera (order -1, behind the UI camera) renders a crimson
//! dusk skyline on its own render layer: a sculpted lattice relay mast with a
//! rotating parabolic dish, a pulsing red / acid-green beacon orb with
//! expanding signal rings, dark bunker parapet monoliths, a distant ridge and
//! drifting glowing dust. The menu's own UI camera stops clearing so the vista
//! shows through the translucent scrim.
//!
//! Lifecycle: spawned on `OnEnter(MainMenu)`, every entity carries
//! `DespawnOnExit(MainMenu)`, so it is gone before `Playing` (the
//! `PlayerCamera`) or `Compendium` (its 3D viewer camera) ever spawn. Meshes
//! and materials are built once and cached in [`MenuSceneAssets`] (no churn
//! when the menu is re-entered). All meshes are u32-indexed with UVs
//! (Solari-convertible), the scene uses at most [`MENU_SCENE_MAX_LIGHTS`]
//! point lights, and the camera takes the session's [`crate::perf::RenderMsaa`]
//! on its first frame (`Msaa::Off` on WebGL2 / Android).
//!
//! Registered by `HudPlugin` only, so headless `CoreGameplayPlugin` tests
//! never see it.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::camera::visibility::RenderLayers;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::view::Msaa;

use crate::mesh_batch::MeshBatch;
use crate::sculpt;
use crate::state::AppState;

/// Render layer of the vista (no gameplay / overlay / Compendium camera draws it).
pub const MENU_SCENE_LAYER: usize = 6;
/// Point light budget of the vista.
pub const MENU_SCENE_MAX_LIGHTS: usize = 3;
/// Crimson dusk sky (the vista camera's clear color).
pub const MENU_DUSK: Color = Color::srgb(0.46, 0.09, 0.06);
/// Camera orbit: radius (m), height (m), angular speed (rad/s).
pub const ORBIT_RADIUS: f32 = 34.0;
pub const ORBIT_HEIGHT: f32 = 5.5;
pub const ORBIT_SPEED: f32 = 0.035;
/// Height of the beacon orb on the mast.
pub const BEACON_Y: f32 = 24.5;
const RINGS: usize = 3;
const RING_PERIOD: f32 = 3.6;

/// The vista camera.
#[derive(Component)]
pub struct MenuSceneCamera {
    pub angle: f32,
}

/// Every vista entity (camera, meshes, lights).
#[derive(Component, Clone, Copy)]
pub struct MenuSceneEntity;

#[derive(Component)]
struct DishPivot;

#[derive(Component)]
struct BeaconOrb;

#[derive(Component)]
struct BeaconLight;

#[derive(Component)]
struct SignalRing(usize);

#[derive(Component)]
struct DustField;

/// Cached vista meshes + materials (built once on the first main menu).
#[derive(Resource, Clone)]
pub struct MenuSceneAssets {
    pub parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    dish: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    beacon_mesh: Handle<Mesh>,
    beacon_mat: Handle<StandardMaterial>,
    ring_mesh: Handle<Mesh>,
    ring_mats: [Handle<StandardMaterial>; RINGS],
    dust: (Handle<Mesh>, Handle<StandardMaterial>),
}

pub struct MenuScenePlugin;

impl Plugin for MenuScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::MainMenu), spawn_menu_scene)
            .add_systems(Update, animate_menu_scene.run_if(in_state(AppState::MainMenu)));
    }
}

fn mat(base: Color, rough: f32, metal: f32) -> StandardMaterial {
    StandardMaterial { base_color: base, perceptual_roughness: rough, metallic: metal, ..default() }
}

fn glow(base: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial { base_color: base, emissive, unlit: true, ..default() }
}

/// Unit cube stretched from `a` to `b` with cross-section `w`.
fn strut(batch: &mut MeshBatch, cube: &Mesh, a: Vec3, b: Vec3, w: f32) {
    let d = b - a;
    let len = d.length();
    if len < 1e-4 {
        return;
    }
    let tf = Transform::from_translation(a + d * 0.5).with_rotation(Quat::from_rotation_arc(Vec3::Y, d / len)).with_scale(Vec3::new(w, len, w));
    batch.append(cube, &tf);
}

/// Corner `k` (0..4) of the lattice mast at height `y` (tapers toward the top).
fn mast_corner(k: usize, y: f32) -> Vec3 {
    let half = 1.7 - 1.1 * (y / BEACON_Y).clamp(0.0, 1.0);
    let a = k as f32 * FRAC_PI_2 + FRAC_PI_2 * 0.5;
    Vec3::new(a.cos() * half * std::f32::consts::SQRT_2, y, a.sin() * half * std::f32::consts::SQRT_2)
}

/// Build every vista mesh batch (pure: no ECS), keyed by material role.
pub struct VistaBatches {
    pub steel: MeshBatch,
    pub rock: MeshBatch,
    pub ridge: MeshBatch,
    pub accent: MeshBatch,
    pub dish: MeshBatch,
    pub dust: MeshBatch,
}

pub fn build_vista() -> VistaBatches {
    let cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
    let mut steel = MeshBatch::new();
    let mut rock = MeshBatch::new();
    let mut ridge = MeshBatch::new();
    let mut accent = MeshBatch::new();
    let mut dish = MeshBatch::new();
    let mut dust = MeshBatch::new();

    // Ground: a wide regolith slab.
    rock.append(&cube, &Transform::from_xyz(0.0, -0.15, 0.0).with_scale(Vec3::new(220.0, 0.3, 220.0)));
    // Relay pedestal + bunker plinth.
    rock.append(&sculpt::tapered_prism(8, 4.4, 3.6, 1.4, true), &Transform::from_xyz(0.0, 0.7, 0.0));
    steel.append(&sculpt::beveled_box(Vec3::new(4.6, 0.4, 4.6), 0.08), &Transform::from_xyz(0.0, 1.6, 0.0));

    // Lattice mast: 4 tapered legs, crossbars and X braces every 2.5 m.
    let levels = 9;
    let step = (BEACON_Y - 2.0) / levels as f32;
    for i in 0..levels {
        let (y0, y1) = (1.8 + i as f32 * step, 1.8 + (i + 1) as f32 * step);
        for k in 0..4 {
            let (a0, a1) = (mast_corner(k, y0), mast_corner(k, y1));
            let (b0, b1) = (mast_corner((k + 1) % 4, y0), mast_corner((k + 1) % 4, y1));
            strut(&mut steel, &cube, a0, a1, 0.2);
            strut(&mut steel, &cube, a1, b1, 0.1);
            strut(&mut steel, &cube, a0, b1, 0.06);
            strut(&mut steel, &cube, b0, a1, 0.06);
        }
    }
    // Two service platforms with glowing edge strips.
    for y in [9.0f32, 16.5] {
        steel.append(&sculpt::beveled_box(Vec3::new(4.2, 0.18, 4.2), 0.05), &Transform::from_xyz(0.0, y, 0.0));
        for k in 0..4 {
            let a = Quat::from_rotation_y(k as f32 * FRAC_PI_2);
            accent.append(&cube, &Transform::from_translation(a * Vec3::new(0.0, y + 0.1, 2.12)).with_rotation(a).with_scale(Vec3::new(4.0, 0.05, 0.04)));
        }
    }
    // Twin cross antennas + beacon cradle at the crown.
    for s in [-1.0f32, 1.0] {
        let foot = Vec3::new(s * 0.9, BEACON_Y - 3.0, 0.0);
        strut(&mut steel, &cube, foot, foot + Vec3::Y * 5.5, 0.09);
        for (k, w) in [(0, 1.6f32), (1, 1.1), (2, 0.6)] {
            let y = foot.y + 2.2 + k as f32 * 1.0;
            steel.append(&cube, &Transform::from_xyz(foot.x, y, 0.0).with_scale(Vec3::new(0.06, 0.06, w)));
        }
        accent.append(&cube, &Transform::from_translation(foot + Vec3::Y * 5.6).with_scale(Vec3::splat(0.18)));
    }
    steel.append(&sculpt::armored_ring(0.9, 0.08, 0.1, 16, true), &Transform::from_xyz(0.0, BEACON_Y - 0.9, 0.0));

    // Rotating parabolic relay dish (pivot local: mast axis at the origin).
    let dish_axis = Vec3::new(0.0, 0.5, 1.0).normalize();
    let rot = Quat::from_rotation_arc(Vec3::Y, dish_axis);
    let hub = Vec3::new(0.0, 0.0, 2.3);
    dish.append(&cube, &Transform::from_xyz(0.0, 0.0, 1.1).with_scale(Vec3::new(0.24, 0.24, 2.2)));
    dish.append(&sculpt::tapered_prism(24, 0.35, 2.8, 0.7, false), &Transform::from_translation(hub + dish_axis * 0.3).with_rotation(rot));
    dish.append(&sculpt::armored_ring(2.8, 0.06, 0.06, 24, true), &Transform::from_translation(hub + dish_axis * 0.66).with_rotation(rot));
    strut(&mut dish, &cube, hub + dish_axis * 0.5, hub + dish_axis * 2.4, 0.07);
    dish.append(&cube, &Transform::from_translation(hub + dish_axis * 2.5).with_rotation(rot).with_scale(Vec3::splat(0.28)));

    // Dark bunker parapet monoliths in a broken ring + crimson seams.
    let mono = sculpt::wedge_plate(2.4, 1.0, 1.2, 0.7, 0.8, 0.0);
    for i in 0..9 {
        let a = i as f32 * TAU / 9.0 + 0.3;
        let r = 13.0 + (i % 3) as f32 * 2.5;
        let h = 3.0 + ((i * 5) % 4) as f32 * 1.4;
        let pos = Vec3::new(a.cos() * r, h * 0.5, a.sin() * r);
        let yaw = Quat::from_rotation_y(-a + FRAC_PI_2 + 0.2 * (i as f32).sin());
        rock.append(&mono, &Transform::from_translation(pos).with_rotation(yaw).with_scale(Vec3::new(1.0, h, 1.0)));
        accent.append(&cube, &Transform::from_translation(pos + Vec3::Y * (h * 0.32)).with_rotation(yaw).with_scale(Vec3::new(1.9, 0.06, 1.25)));
    }

    // Distant jagged ridge (fog hazes it into the dusk).
    let peak = sculpt::tapered_prism(5, 1.0, 0.0, 1.0, true);
    for i in 0..30 {
        let a = i as f32 * TAU / 30.0;
        let r = 85.0 + ((i * 37) % 11) as f32 * 3.0;
        let h = 10.0 + ((i * 13) % 7) as f32 * 3.5;
        let w = 9.0 + ((i * 7) % 5) as f32 * 2.0;
        ridge.append(&peak, &Transform::from_xyz(a.cos() * r, h * 0.5 - 0.5, a.sin() * r).with_rotation(Quat::from_rotation_y(a)).with_scale(Vec3::new(w, h, w)));
    }

    // Drifting glowing Mars dust / embers (one mesh; the field drifts as a whole).
    let mut seed = 0x5EED_u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 10_000) as f32 / 10_000.0
    };
    for _ in 0..140 {
        let a = rnd() * TAU;
        let r = 4.0 + rnd() * 26.0;
        let p = Vec3::new(a.cos() * r, 0.5 + rnd() * 22.0, a.sin() * r);
        let s = 0.05 + rnd() * 0.09;
        dust.append(&cube, &Transform::from_translation(p).with_rotation(Quat::from_rotation_y(rnd() * TAU)).with_scale(Vec3::splat(s)));
    }
    VistaBatches { steel, rock, ridge, accent, dish, dust }
}

fn build_assets(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> MenuSceneAssets {
    let v = build_vista();
    let steel = materials.add(mat(Color::srgb(0.2, 0.15, 0.14), 0.5, 0.6));
    let rock = materials.add(mat(Color::srgb(0.34, 0.12, 0.08), 0.95, 0.0));
    let ridge = materials.add(mat(Color::srgb(0.22, 0.07, 0.06), 1.0, 0.0));
    let accent = materials.add(glow(Color::srgb(1.0, 0.25, 0.16), LinearRgba::rgb(9.0, 1.4, 0.8)));
    let dish = materials.add(mat(Color::srgb(0.55, 0.5, 0.48), 0.35, 0.85));
    let dust = materials.add(glow(Color::srgb(1.0, 0.55, 0.25), LinearRgba::rgb(6.0, 2.2, 0.7)));
    let mut add = |b: MeshBatch, m: &Handle<StandardMaterial>| (meshes.add(b.build().expect("vista batch")), m.clone());
    let parts = vec![add(v.rock, &rock), add(v.ridge, &ridge), add(v.steel, &steel), add(v.accent, &accent)];
    let dish_part = vec![add(v.dish, &dish)];
    let dust_part = add(v.dust, &dust);
    let mut ring = MeshBatch::new();
    ring.append(&sculpt::armored_ring(1.0, 0.04, 0.03, 32, false), &Transform::IDENTITY);
    let ring_mesh = meshes.add(ring.build().expect("ring"));
    let mut orb = MeshBatch::new();
    orb.append(&Mesh::from(Sphere::new(1.25)), &Transform::IDENTITY);
    let ring_mats = std::array::from_fn(|_| {
        materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.3, 0.2, 0.8),
            emissive: LinearRgba::rgb(6.0, 1.2, 0.8),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        })
    });
    MenuSceneAssets {
        parts,
        dish: dish_part,
        beacon_mesh: meshes.add(orb.build().expect("orb")),
        beacon_mat: materials.add(glow(Color::srgb(1.0, 0.2, 0.15), LinearRgba::rgb(30.0, 4.0, 2.0))),
        ring_mesh,
        ring_mats,
        dust: dust_part,
    }
}

/// Camera pose at orbit angle `angle`: the tower sits right of center so the
/// menu column never hides the mast.
pub fn orbit_camera_transform(angle: f32) -> Transform {
    let pos = Vec3::new(angle.cos() * ORBIT_RADIUS, ORBIT_HEIGHT, angle.sin() * ORBIT_RADIUS);
    let to_tower = (Vec3::new(0.0, pos.y, 0.0) - pos).normalize();
    let right = to_tower.cross(Vec3::Y).normalize();
    // Mast top lands in the menu's empty upper-right corner.
    let target = Vec3::new(0.0, 16.5, 0.0) - right * 17.0;
    Transform::from_translation(pos).looking_at(target, Vec3::Y)
}

#[allow(clippy::too_many_arguments)]
fn spawn_menu_scene(
    mut commands: Commands,
    cached: Option<Res<MenuSceneAssets>>,
    msaa: Option<Res<crate::perf::RenderMsaa>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let assets = match cached {
        Some(a) => a.clone(),
        None => {
            let a = build_assets(&mut meshes, &mut materials);
            commands.insert_resource(a.clone());
            a
        }
    };
    let layer = RenderLayers::layer(MENU_SCENE_LAYER);
    let msaa = msaa.map(|m| m.0).unwrap_or(Msaa::Sample2);
    let tag = (MenuSceneEntity, DespawnOnExit(AppState::MainMenu), layer.clone());
    let angle = 0.6;
    commands.spawn((
        tag.clone(),
        MenuSceneCamera { angle },
        Camera3d::default(),
        // Behind the UI camera (order 0), which stops clearing in the menu.
        Camera { order: -1, clear_color: ClearColorConfig::Custom(MENU_DUSK), ..default() },
        msaa,
        Projection::Perspective(PerspectiveProjection { fov: 52f32.to_radians(), ..default() }),
        DistanceFog { color: MENU_DUSK, falloff: FogFalloff::Linear { start: 40.0, end: 150.0 }, ..default() },
        AmbientLight { color: Color::srgb(1.0, 0.55, 0.45), brightness: 320.0, ..default() },
        orbit_camera_transform(angle),
    ));
    for (mesh, m) in &assets.parts {
        commands.spawn((tag.clone(), Mesh3d(mesh.clone()), MeshMaterial3d(m.clone()), Transform::IDENTITY));
    }
    commands.spawn((tag.clone(), DustField, Mesh3d(assets.dust.0.clone()), MeshMaterial3d(assets.dust.1.clone()), Transform::IDENTITY));
    commands
        .spawn((tag.clone(), DishPivot, Transform::from_xyz(0.0, BEACON_Y - 6.0, 0.0), Visibility::default()))
        .with_children(|p| {
            for (mesh, m) in &assets.dish {
                p.spawn((MenuSceneEntity, layer.clone(), Mesh3d(mesh.clone()), MeshMaterial3d(m.clone()), Transform::IDENTITY));
            }
        });
    commands.spawn((tag.clone(), BeaconOrb, Mesh3d(assets.beacon_mesh.clone()), MeshMaterial3d(assets.beacon_mat.clone()), Transform::from_xyz(0.0, BEACON_Y, 0.0)));
    for (i, m) in assets.ring_mats.iter().enumerate() {
        commands.spawn((tag.clone(), SignalRing(i), Mesh3d(assets.ring_mesh.clone()), MeshMaterial3d(m.clone()), Transform::from_xyz(0.0, BEACON_Y, 0.0)));
    }
    // Exactly MENU_SCENE_MAX_LIGHTS point lights: beacon, crimson key, cool rim.
    commands.spawn((
        tag.clone(),
        BeaconLight,
        PointLight { color: Color::srgb(1.0, 0.25, 0.15), intensity: 9_000_000.0, range: 60.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, BEACON_Y, 0.0),
    ));
    commands.spawn((
        tag.clone(),
        PointLight { color: Color::srgb(1.0, 0.45, 0.28), intensity: 260_000_000.0, range: 180.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(-45.0, 30.0, 40.0),
    ));
    commands.spawn((
        tag,
        PointLight { color: Color::srgb(0.35, 0.55, 1.0), intensity: 25_000_000.0, range: 120.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(40.0, 18.0, -45.0),
    ));
}

/// Beacon color at time `t`: crimson most of the cycle, an acid-green "lock"
/// pulse once per [`RING_PERIOD`].
pub fn beacon_color(t: f32) -> LinearRgba {
    let phase = (t / RING_PERIOD).rem_euclid(1.0);
    let lock = (1.0 - (phase - 0.08).abs() / 0.08).clamp(0.0, 1.0);
    let pulse = 0.65 + 0.35 * (t * TAU * 0.9).sin().abs();
    let red = LinearRgba::rgb(30.0, 4.0, 2.0) * pulse;
    let green = LinearRgba::rgb(6.0, 34.0, 8.0);
    LinearRgba::rgb(red.red + (green.red - red.red) * lock, red.green + (green.green - red.green) * lock, red.blue + (green.blue - red.blue) * lock)
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn animate_menu_scene(
    time: Res<Time>,
    assets: Option<Res<MenuSceneAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cams: Query<(&mut MenuSceneCamera, &mut Transform), (Without<DishPivot>, Without<SignalRing>, Without<DustField>)>,
    mut dish: Query<&mut Transform, (With<DishPivot>, Without<SignalRing>, Without<DustField>)>,
    mut rings: Query<(&SignalRing, &mut Transform), Without<DustField>>,
    mut dust: Query<&mut Transform, With<DustField>>,
    mut light: Query<&mut PointLight, With<BeaconLight>>,
) {
    let (dt, t) = (time.delta_secs(), time.elapsed_secs());
    for (mut cam, mut tf) in &mut cams {
        cam.angle = (cam.angle + ORBIT_SPEED * dt).rem_euclid(TAU);
        *tf = orbit_camera_transform(cam.angle);
    }
    for mut tf in &mut dish {
        tf.rotation = Quat::from_rotation_y(t * 0.25);
    }
    for mut tf in &mut dust {
        tf.rotation = Quat::from_rotation_y(t * 0.02);
        tf.translation.y = (t * 0.35).sin() * 0.6;
    }
    let Some(assets) = assets else { return };
    let beacon = beacon_color(t);
    if let Some(mut m) = materials.get_mut(&assets.beacon_mat) {
        m.emissive = beacon;
    }
    for mut l in &mut light {
        l.color = Color::LinearRgba(beacon / beacon.red.max(beacon.green).max(1e-3));
    }
    for (ring, mut tf) in &mut rings {
        let phase = (t / RING_PERIOD + ring.0 as f32 / RINGS as f32).rem_euclid(1.0);
        let r = 1.0 + phase * 14.0;
        tf.scale = Vec3::new(r, 1.0 + phase * 2.0, r);
        if let Some(mut m) = materials.get_mut(&assets.ring_mats[ring.0]) {
            let fade = (1.0 - phase).powf(1.5);
            m.emissive = LinearRgba::rgb(6.0, 1.2, 0.8) * fade + LinearRgba::rgb(0.5, 5.0, 1.0) * (fade * (1.0 - phase) * 0.6);
            m.base_color = Color::srgba(1.0, 0.3, 0.2, 0.8 * fade);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raytracing::rt_mesh_convertible;

    #[test]
    fn vista_meshes_are_rt_ready_with_u32_indices_and_uvs() {
        let v = build_vista();
        for (name, b) in [("steel", v.steel), ("rock", v.rock), ("ridge", v.ridge), ("accent", v.accent), ("dish", v.dish), ("dust", v.dust)] {
            let mesh = b.build().unwrap_or_else(|| panic!("{name} batch"));
            assert!(rt_mesh_convertible(&mesh), "{name}");
            assert!(matches!(mesh.indices(), Some(bevy::mesh::Indices::U32(_))), "{name}");
            assert!(mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0), "{name}");
        }
    }

    #[test]
    fn orbit_keeps_the_tower_in_frame_and_beacon_pulses_red_to_green() {
        for k in 0..24 {
            let tf = orbit_camera_transform(k as f32 * TAU / 24.0);
            let fwd = tf.forward();
            let to_mast = (Vec3::new(0.0, 12.0, 0.0) - tf.translation).normalize();
            // Tower stays within ~37 deg of the view axis (right of center).
            assert!(fwd.dot(to_mast) > 0.8, "angle {k}");
            assert!(tf.right().dot(to_mast) > 0.0, "tower sits right of center");
        }
        let colors: Vec<LinearRgba> = (0..72).map(|i| beacon_color(i as f32 * 0.05)).collect();
        assert!(colors.iter().any(|c| c.red > c.green * 3.0), "crimson phase");
        assert!(colors.iter().any(|c| c.green > c.red * 2.0), "acid-green lock pulse");
        assert!(colors.iter().all(|c| c.red.is_finite() && c.green.is_finite()));
    }
}
