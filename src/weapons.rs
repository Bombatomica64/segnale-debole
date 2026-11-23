//! Right hand: Mars Pump Shotgun (hitscan pellets) + UAC Plasma Repeater (3D
//! bolts). Left hand: tactical smartphone viewmodel with a live signal screen.
//! Also destructible Demon Jammers and pickups.
//!
//! Viewmodels live on [`OVERLAY_LAYER`], drawn by a second camera (child of the
//! `PlayerCamera`) that clears depth, so they never clip into walls.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::camera::visibility::RenderLayers;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
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
use crate::level::{Cell, DemonJammer, JammerCore, LevelEntity, LevelGrid};
use crate::meta::{HandsetSkin, RunLoadout, SkinSlot};
use crate::phone_screen::PhoneScreenTexture;
use crate::physics::raycast_grid;
use crate::player::{CursorCaptured, Player, PlayerCamera};
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

/// Weapons, apps and projectiles that deal damage; deaths run after this set.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CombatDamage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weapon {
    #[default]
    Shotgun,
    Plasma,
}

impl Weapon {
    pub fn other(self) -> Weapon {
        match self {
            Weapon::Shotgun => Weapon::Plasma,
            Weapon::Plasma => Weapon::Shotgun,
        }
    }
    pub fn base_cooldown(self) -> f32 {
        match self {
            Weapon::Shotgun => SHOTGUN_COOLDOWN,
            Weapon::Plasma => PLASMA_COOLDOWN,
        }
    }
    pub fn has_ammo(self, p: &Player) -> bool {
        match self {
            Weapon::Shotgun => p.ammo_shells > 0,
            Weapon::Plasma => p.ammo_plasma > 0,
        }
    }
}

/// Seconds between shots with all fire-rate modifiers (incl. 5G Overclock).
pub fn fire_cooldown(w: Weapon, mods: &PlayerModifiers, bars: u8) -> f32 {
    w.base_cooldown() / (mods.fire_rate_mult * overclock_factor(bars, mods))
}

#[derive(Resource, Debug, Clone, Default)]
pub struct WeaponState {
    pub current: Weapon,
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
        if self.swap_timer > SWAP_SECS * 0.5 { self.current.other() } else { self.current }
    }

    pub fn switch_to(&mut self, w: Weapon) -> bool {
        if w == self.current {
            return false;
        }
        self.current = w;
        self.swap_timer = SWAP_SECS;
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
pub fn jammer_ray_hit(base: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    [(0.5, 0.5), (1.3, 0.5), (2.7, 0.8)]
        .iter()
        .filter_map(|&(h, r)| ray_sphere(origin, dir, base + Vec3::Y * h, r))
        .reduce(f32::min)
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
    let dir = dir.normalize_or_zero();
    let mut best = match raycast_grid(grid, origin, dir, max_dist) {
        Some(h) => (h.distance, HitTarget::Wall),
        None => (max_dist, HitTarget::Nothing),
    };
    for (i, (root, e)) in enemies.iter().enumerate() {
        if e.is_alive()
            && let Some(t) = e.ray_hit(*root, origin, dir, best.0)
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

#[derive(Component, Debug, Clone)]
pub struct PlayerProjectile {
    pub vel: Vec3,
    pub damage: f32,
    pub life: f32,
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
}

impl PickupKind {
    pub fn base_amount(self) -> f32 {
        match self {
            PickupKind::Health => 25.0,
            PickupKind::Armor => 25.0,
            PickupKind::Shells => 8.0,
            PickupKind::Plasma => 40.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PickupKind::Health => "MEDKIT",
            PickupKind::Armor => "ARMOR",
            PickupKind::Shells => "SHELLS",
            PickupKind::Plasma => "PLASMA CELL",
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
}

#[derive(Component)]
struct PickupSpin;

/// Mesh part of a pickup model: (mesh, material, offset, scale).
type PickupPart = (Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3);

pub fn spawn_pickup(commands: &mut Commands, a: &PickupAssets, kind: PickupKind, pos: Vec3, dropped: bool) {
    let mut rng = rand::rng();
    let base_y = pos.y + 0.55;
    let (light, parts): (Color, Vec<PickupPart>) = match kind {
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
    };
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
        PointLight { color: light, intensity: if dropped { 50_000.0 } else { 80_000.0 }, range: 4.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, 0.3, 0.0),
    ));
    let spin = commands.spawn((ChildOf(root), PickupSpin, Transform::default(), Visibility::default())).id();
    for (mesh, mat, t, s) in parts {
        commands.spawn((ChildOf(spin), Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(t).with_scale(s)));
    }
}

fn create_pickup_assets(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let glow = |c: Color, e: LinearRgba| StandardMaterial { base_color: c, emissive: e, perceptual_roughness: 0.4, ..default() };
    commands.insert_resource(PickupAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        cyl: meshes.add(Cylinder::new(1.0, 1.0)),
        white: materials.add(glow(Color::srgb(0.9, 0.9, 0.9), LinearRgba::rgb(0.6, 0.6, 0.6))),
        red: materials.add(glow(Color::srgb(1.0, 0.1, 0.1), LinearRgba::rgb(4.0, 0.2, 0.2))),
        armor: materials.add(glow(Color::srgb(0.2, 0.7, 0.35), LinearRgba::rgb(0.3, 2.5, 0.8))),
        shell: materials.add(glow(Color::srgb(0.8, 0.1, 0.05), LinearRgba::rgb(1.5, 0.2, 0.05))),
        brass: materials.add(glow(Color::srgb(0.9, 0.7, 0.2), LinearRgba::rgb(1.2, 0.8, 0.1))),
        plasma: materials.add(glow(Color::srgb(0.3, 0.9, 1.0), LinearRgba::rgb(0.5, 5.0, 7.0))),
        steel: materials.add(StandardMaterial { base_color: Color::srgb(0.3, 0.32, 0.35), metallic: 0.8, perceptual_roughness: 0.35, ..default() }),
    });
}

/// Floor loot: a pickup in the middle of a few bunker rooms.
pub fn floor_pickup_plan(grid: &LevelGrid) -> Vec<(PickupKind, IVec2)> {
    let kinds = [PickupKind::Shells, PickupKind::Health, PickupKind::Plasma, PickupKind::Armor];
    grid.rooms
        .iter()
        .skip(1) // rooms[0] is the atrium
        .map(|r| r.center())
        .filter(|&c| matches!(grid.cell(c), Cell::Floor { .. }) && c != grid.spawn && c != grid.tower && !grid.jammers.contains(&c))
        .take(4 + grid.floor as usize / 2)
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

#[allow(clippy::too_many_arguments)]
fn spawn_viewmodels(
    mut commands: Commands,
    cams: Query<(Entity, &Projection), Added<PlayerCamera>>,
    phone_tex: Option<Res<PhoneScreenTexture>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    loadout: Option<Res<RunLoadout>>,
) {
    let Ok((cam, proj)) = cams.single() else { return };
    let skin = loadout.map(|l| l.skin).unwrap_or_default();
    let palette = skin.palette();
    let fov = match proj {
        Projection::Perspective(p) => p.fov,
        _ => 85f32.to_radians(),
    };
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let cyl = meshes.add(Cylinder::new(1.0, 1.0));
    let cap = meshes.add(Capsule3d::new(0.5, 1.0));
    let ring = meshes.add(Torus::new(0.68, 1.0));
    let thin_ring = meshes.add(Torus::new(0.78, 1.0));
    let sphere = meshes.add(Sphere::new(1.0).mesh().uv(16, 12));
    let cone = meshes.add(Cone::new(1.0, 1.0));
    let screen_quad = meshes.add(Rectangle::new(0.118, 0.177));

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
    commands.spawn(vm_part(sg, &cube, &dark_steel, Vec3::new(0.0, 0.0, -0.01), Vec3::new(0.072, 0.088, 0.25), id));
    commands.spawn(vm_part(sg, &cube, &brushed, Vec3::new(0.0, 0.044, -0.01), Vec3::new(0.056, 0.016, 0.235), id));
    commands.spawn(vm_part(sg, &cube, &titanium, Vec3::new(0.0, -0.046, 0.01), Vec3::new(0.052, 0.018, 0.18), id));
    // Side receiver detail: recessed ejection port on right showing chambered red/brass shell + rivets.
    commands.spawn(vm_part(sg, &cube, &grip, Vec3::new(0.036, 0.014, -0.05), Vec3::new(0.006, 0.028, 0.092), id));
    commands.spawn(vm_part(sg, &cyl, &shell_red, Vec3::new(0.034, 0.014, -0.06), Vec3::new(0.01, 0.048, 0.01), rx));
    commands.spawn(vm_part(sg, &cyl, &brass, Vec3::new(0.034, 0.014, -0.032), Vec3::new(0.0105, 0.016, 0.0105), rx));
    commands.spawn(vm_part(sg, &cube, &hazard, Vec3::new(-0.037, 0.008, -0.02), Vec3::new(0.004, 0.016, 0.11), id));
    for z in [-0.09f32, -0.01, 0.06] {
        for s in [-1.0f32, 1.0] {
            commands.spawn(vm_part(sg, &cyl, &bolt_silver, Vec3::new(0.037 * s, -0.022, z), Vec3::new(0.005, 0.004, 0.005), rz));
        }
    }
    // Top Picatinny rail blocks + rear green tritium ghost-ring sight + front red fiber-optic post.
    for k in 0..7 {
        commands.spawn(vm_part(sg, &cube, &dark_steel, Vec3::new(0.0, 0.056, -0.105 + k as f32 * 0.026), Vec3::new(0.028, 0.01, 0.015), id));
    }
    commands.spawn(vm_part(sg, &thin_ring, &steel, Vec3::new(0.0, 0.072, 0.055), Vec3::new(0.014, 0.008, 0.014), rx));
    for s in [-1.0f32, 1.0] {
        commands.spawn(vm_part(sg, &sphere, &sight_green, Vec3::new(0.018 * s, 0.072, 0.058), Vec3::splat(0.0038), id));
    }
    // Heavy upper barrel + ventilated perforated heat-shield shroud + ported muzzle choke.
    commands.spawn(vm_part(sg, &cyl, &steel, Vec3::new(0.0, 0.030, -0.35), Vec3::new(0.020, 0.48, 0.020), rx));
    commands.spawn(vm_part(sg, &cyl, &grip, Vec3::new(0.0, 0.030, -0.592), Vec3::new(0.015, 0.008, 0.015), rx)); // bore opening
    commands.spawn(vm_part(sg, &cyl, &heat_metal, Vec3::new(0.0, 0.030, -0.565), Vec3::new(0.024, 0.052, 0.024), rx)); // muzzle brake
    commands.spawn(vm_part(sg, &cube, &brushed, Vec3::new(0.0, 0.050, -0.34), Vec3::new(0.018, 0.008, 0.39), id)); // ventilated top rib
    for k in 0..8 {
        commands.spawn(vm_part(
            sg,
            &thin_ring,
            &brushed,
            Vec3::new(0.0, 0.030, -0.165 - k as f32 * 0.048),
            Vec3::new(0.026, 0.014, 0.026),
            rx,
        ));
    }
    // Front sight ramp + glowing red tritium bead.
    commands.spawn(vm_part(sg, &cube, &dark_steel, Vec3::new(0.0, 0.057, -0.555), Vec3::new(0.012, 0.018, 0.028), id));
    commands.spawn(vm_part(sg, &sphere, &sight_red, Vec3::new(0.0, 0.071, -0.552), Vec3::splat(0.0048), id));
    // Underbarrel magazine tube + barrel clamp + knurled front cap.
    commands.spawn(vm_part(sg, &cyl, &dark_steel, Vec3::new(0.0, -0.012, -0.32), Vec3::new(0.016, 0.42, 0.016), rx));
    commands.spawn(vm_part(sg, &cyl, &brushed, Vec3::new(0.0, -0.012, -0.535), Vec3::new(0.0185, 0.028, 0.0185), rx));
    commands.spawn(vm_part(sg, &cube, &steel, Vec3::new(0.0, 0.008, -0.49), Vec3::new(0.032, 0.056, 0.024), id));
    // Sculpted rear stock + grip + trigger guard + trigger.
    commands.spawn(vm_part(sg, &cube, &wood, Vec3::new(0.0, -0.048, 0.20), Vec3::new(0.054, 0.078, 0.21), Quat::from_rotation_x(0.14)));
    commands.spawn(vm_part(sg, &cube, &grip, Vec3::new(0.0, -0.062, 0.305), Vec3::new(0.056, 0.088, 0.02), Quat::from_rotation_x(0.14)));
    commands.spawn(vm_part(sg, &cap, &grip, Vec3::new(0.0, -0.095, 0.075), Vec3::new(0.046, 0.07, 0.052), Quat::from_rotation_x(-0.38)));
    commands.spawn(vm_part(sg, &cube, &dark_steel, Vec3::new(0.0, -0.076, -0.015), Vec3::new(0.014, 0.006, 0.075), id)); // trigger guard bottom
    commands.spawn(vm_part(sg, &cube, &steel, Vec3::new(0.0, -0.060, 0.004), Vec3::new(0.006, 0.024, 0.01), Quat::from_rotation_x(0.35))); // trigger
    // Articulated gloved marine right hand + armored wrist cuff gripping shotgun.
    commands.spawn(vm_part(sg, &cap, &glove, Vec3::new(0.012, -0.112, 0.082), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.35)));
    commands.spawn(vm_part(sg, &cube, &glove_plate, Vec3::new(0.036, -0.096, 0.065), Vec3::new(0.022, 0.048, 0.068), Quat::from_rotation_x(-0.3)));
    commands.spawn(vm_part(sg, &cap, &glove, Vec3::new(0.042, -0.17, 0.20), Vec3::new(0.088, 0.11, 0.088), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2)));
    commands.spawn(vm_part(sg, &cyl, &glove_plate, Vec3::new(0.048, -0.155, 0.165), Vec3::new(0.052, 0.036, 0.052), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2)));

    // Sliding pump forend (contoured wood/polymer ribs + dual steel action bars).
    let pump = commands.spawn((ChildOf(sg), PumpForend, Transform::from_xyz(0.0, -0.012, -0.30), Visibility::default())).id();
    commands.spawn(vm_part(pump, &cyl, &wood, Vec3::ZERO, Vec3::new(0.032, 0.175, 0.030), rx));
    commands.spawn(vm_part(pump, &cube, &wood, Vec3::new(0.0, -0.014, 0.0), Vec3::new(0.058, 0.034, 0.165), id));
    for k in 0..6 {
        commands.spawn(vm_part(pump, &cyl, &grip, Vec3::new(0.0, -0.002, -0.068 + k as f32 * 0.027), Vec3::new(0.0335, 0.011, 0.0335), rx));
    }
    for s in [-1.0f32, 1.0] {
        commands.spawn(vm_part(pump, &cube, &steel, Vec3::new(0.027 * s, 0.005, 0.13), Vec3::new(0.005, 0.008, 0.18), id));
    }
    // Layered volumetric shotgun muzzle star-burst (compact & punchy so targets stay visible).
    let star = commands
        .spawn((ChildOf(sg), MuzzleFlash(Weapon::Shotgun), MuzzleStar, Transform::from_xyz(0.0, 0.028, -0.63), Visibility::Hidden))
        .id();
    let q = std::f32::consts::FRAC_PI_4;
    for (a, len, width) in [(0.0f32, 0.24f32, 0.018f32), (2.0 * q, 0.21, 0.016), (q, 0.14, 0.014), (3.0 * q, 0.13, 0.014)] {
        commands.spawn(vm_part(star, &cube, &flash_fire, Vec3::ZERO, Vec3::new(len, width, 0.005), Quat::from_rotation_z(a)));
        commands.spawn(vm_part(star, &cube, &flash_core, Vec3::new(0.0, 0.0, 0.003), Vec3::new(len * 0.52, width * 0.55, 0.005), Quat::from_rotation_z(a)));
    }
    commands.spawn(vm_part(star, &cone, &flash_fire, Vec3::new(0.0, 0.0, -0.075), Vec3::new(0.025, 0.15, 0.025), Quat::from_rotation_x(-FRAC_PI_2)));
    commands.spawn(vm_part(star, &sphere, &flash_core, Vec3::ZERO, Vec3::splat(0.034), id));

    // ---- Weapon 2: UAC Plasma Repeater (elaborate twin-rail sci-fi rifle) ----
    let pl = commands.spawn((ChildOf(wroot), GunModel(Weapon::Plasma), Transform::default(), Visibility::Hidden)).id();
    // Layered dark titanium lower receiver + brushed upper shroud + rear stock.
    commands.spawn(vm_part(pl, &cube, &titanium, Vec3::new(0.0, -0.008, -0.10), Vec3::new(0.088, 0.092, 0.34), id));
    commands.spawn(vm_part(pl, &cube, &brushed, Vec3::new(0.0, 0.042, -0.14), Vec3::new(0.072, 0.034, 0.32), id));
    commands.spawn(vm_part(pl, &cube, &dark_steel, Vec3::new(0.0, 0.062, -0.08), Vec3::new(0.046, 0.018, 0.20), id));
    commands.spawn(vm_part(pl, &cube, &hazard, Vec3::new(0.0, 0.018, 0.04), Vec3::new(0.092, 0.014, 0.05), id));
    commands.spawn(vm_part(pl, &cube, &dark_steel, Vec3::new(0.0, -0.02, 0.14), Vec3::new(0.068, 0.084, 0.16), id));
    // Rear holographic sight frame + glowing cyan reticle dot.
    commands.spawn(vm_part(pl, &cube, &dark_steel, Vec3::new(-0.022, 0.086, -0.01), Vec3::new(0.007, 0.036, 0.034), id));
    commands.spawn(vm_part(pl, &cube, &dark_steel, Vec3::new(0.022, 0.086, -0.01), Vec3::new(0.007, 0.036, 0.034), id));
    commands.spawn(vm_part(pl, &cube, &dark_steel, Vec3::new(0.0, 0.102, -0.01), Vec3::new(0.052, 0.007, 0.034), id));
    commands.spawn(vm_part(pl, &sphere, &coil, Vec3::new(0.0, 0.086, -0.012), Vec3::splat(0.0042), id));
    // Twin parallel steel accelerator rails + central glowing plasma conduit + 5 pulsing coils.
    commands.spawn(vm_part(pl, &cube, &steel, Vec3::new(0.0, 0.034, -0.42), Vec3::new(0.034, 0.016, 0.30), id));
    commands.spawn(vm_part(pl, &cube, &steel, Vec3::new(0.0, -0.034, -0.42), Vec3::new(0.034, 0.016, 0.30), id));
    commands.spawn(vm_part(pl, &cyl, &coil, Vec3::new(0.0, 0.0, -0.40), Vec3::new(0.016, 0.32, 0.016), rx));
    for k in 0..5 {
        commands.spawn((
            PlasmaCoil(k),
            vm_part(pl, &ring, &coil, Vec3::new(0.0, 0.0, -0.29 - k as f32 * 0.054), Vec3::new(0.044, 0.052, 0.044), rx),
        ));
    }
    // Flared triple-pronged plasma emitter crown around muzzle lens.
    for a in [0.0f32, TAU / 3.0, 2.0 * TAU / 3.0] {
        let off = Vec3::new(a.cos() * 0.034, a.sin() * 0.034, -0.575);
        commands.spawn(vm_part(pl, &cube, &steel, off, Vec3::new(0.012, 0.012, 0.06), Quat::from_rotation_z(a)));
    }
    commands.spawn(vm_part(pl, &sphere, &coil, Vec3::new(0.0, 0.0, -0.555), Vec3::splat(0.022), id));
    // Side copper cooling radiator fins with glowing heat vents.
    for k in 0..5 {
        let z = -0.22 + k as f32 * 0.042;
        commands.spawn(vm_part(pl, &cube, &copper, Vec3::new(0.047, 0.012, z), Vec3::new(0.012, 0.052, 0.014), id));
        commands.spawn(vm_part(pl, &cube, &vent_amber, Vec3::new(0.045, 0.012, z + 0.021), Vec3::new(0.006, 0.036, 0.008), id));
    }
    // Side-mounted pressurized glowing cyan plasma cell canister + dual steel clamp rings + conduit.
    commands.spawn(vm_part(pl, &cyl, &coil, Vec3::new(-0.062, -0.008, -0.07), Vec3::new(0.022, 0.18, 0.022), rx));
    for z in [-0.135f32, -0.005] {
        commands.spawn(vm_part(pl, &cyl, &dark_steel, Vec3::new(-0.062, -0.008, z), Vec3::new(0.026, 0.022, 0.026), rx));
    }
    commands.spawn(vm_part(pl, &cap, &brushed, Vec3::new(-0.052, -0.012, -0.20), Vec3::new(0.012, 0.06, 0.012), Quat::from_rotation_x(1.3) * Quat::from_rotation_z(-0.45)));
    // Grip + gloved right hand.
    commands.spawn(vm_part(pl, &cap, &grip, Vec3::new(0.0, -0.095, 0.035), Vec3::new(0.046, 0.072, 0.052), Quat::from_rotation_x(-0.32)));
    commands.spawn(vm_part(pl, &cap, &glove, Vec3::new(0.012, -0.112, 0.045), Vec3::new(0.076, 0.052, 0.078), Quat::from_rotation_x(-0.3)));
    commands.spawn(vm_part(pl, &cube, &glove_plate, Vec3::new(0.036, -0.096, 0.03), Vec3::new(0.022, 0.048, 0.068), Quat::from_rotation_x(-0.28)));
    commands.spawn(vm_part(pl, &cap, &glove, Vec3::new(0.042, -0.17, 0.17), Vec3::new(0.088, 0.11, 0.088), Quat::from_rotation_x(1.15) * Quat::from_rotation_z(-0.2)));
    // Layered plasma muzzle flash (bright core + additive outer halo).
    let pl_flash = commands
        .spawn((ChildOf(pl), MuzzleFlash(Weapon::Plasma), Transform::from_xyz(0.0, 0.0, -0.60), Visibility::Hidden))
        .id();
    commands.spawn(vm_part(pl_flash, &sphere, &flash_cyan, Vec3::ZERO, Vec3::splat(0.045), id));
    commands.spawn(vm_part(pl_flash, &sphere, &flash_cyan_halo, Vec3::ZERO, Vec3::splat(0.095), id));
    commands.spawn(vm_part(pl_flash, &thin_ring, &flash_cyan, Vec3::new(0.0, 0.0, -0.01), Vec3::new(0.075, 0.015, 0.075), rx));

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
            commands.spawn(vm_part(proot, &cube, &grip, Vec3::new(0.072 * s, y, 0.0), Vec3::new(0.005, 0.010, 0.016), id));
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
    commands.spawn(vm_part(proot, &cube, &dark_steel, Vec3::new(0.0, 0.097, 0.0105), Vec3::new(0.036, 0.005, 0.003), id));
    commands.spawn(vm_part(proot, &cyl, &brushed, Vec3::new(-0.032, 0.097, 0.0105), Vec3::new(0.0045, 0.003, 0.0045), rx));
    commands.spawn((PhoneLed, vm_part(proot, &sphere, &led, Vec3::new(0.034, 0.097, 0.0115), Vec3::splat(0.0038), id)));
    // Bottom chin tactile home bar + lanyard loop.
    commands.spawn(vm_part(proot, &cube, &brushed, Vec3::new(0.0, -0.098, 0.0105), Vec3::new(0.044, 0.006, 0.003), id));
    // Side hardware controls: orange PTT button on left, volume rockers on right.
    commands.spawn((PhoneSkinPart(SkinSlot::Ptt), vm_part(proot, &cube, &phone_ptt, Vec3::new(-0.074, 0.036, 0.0), Vec3::new(0.006, 0.026, 0.010), id)));
    commands.spawn(vm_part(proot, &cube, &steel, Vec3::new(0.074, 0.046, 0.0), Vec3::new(0.005, 0.018, 0.008), id));
    commands.spawn(vm_part(proot, &cube, &steel, Vec3::new(0.074, 0.020, 0.0), Vec3::new(0.005, 0.018, 0.008), id));
    // Knurled antenna collar + telescoping tactical whip antenna + helical coil + glowing beacon bead.
    commands.spawn(vm_part(proot, &cyl, &brushed, Vec3::new(0.048, 0.114, -0.001), Vec3::new(0.011, 0.018, 0.011), id));
    commands.spawn(vm_part(proot, &cyl, &dark_steel, Vec3::new(0.048, 0.145, -0.001), Vec3::new(0.0048, 0.052, 0.0048), id));
    commands.spawn((PhoneSkinPart(SkinSlot::Coil), vm_part(proot, &thin_ring, &phone_coil, Vec3::new(0.048, 0.132, -0.001), Vec3::new(0.008, 0.006, 0.008), id)));
    commands.spawn((PhoneLed, vm_part(proot, &sphere, &led, Vec3::new(0.048, 0.174, -0.001), Vec3::splat(0.0062), id)));
    // Raised screen surround lip + LIVE 160x240 pixel-art OLED screen quad!
    commands.spawn(vm_part(proot, &cube, &grip, Vec3::new(0.0, 0.0, 0.0096), Vec3::new(0.123, 0.182, 0.002), id));
    commands.spawn((
        PhoneScreen,
        ChildOf(proot),
        Mesh3d(screen_quad),
        MeshMaterial3d(screen),
        Transform::from_xyz(0.0, 0.0, 0.0112),
        overlay_layer(),
    ));
    // Articulated gloved marine left hand & armored cuff gripping bottom & sides (never blocking the screen!).
    commands.spawn(vm_part(proot, &cap, &glove, Vec3::new(-0.012, -0.118, -0.022), Vec3::new(0.088, 0.058, 0.058), rz)); // palm base
    commands.spawn(vm_part(proot, &cap, &glove, Vec3::new(-0.072, -0.042, -0.014), Vec3::new(0.026, 0.088, 0.032), Quat::from_rotation_z(-0.08))); // left hand heel
    // 3 wrapped armored fingers gripping the right spine from behind.
    for k in 0..3 {
        let y = -0.058 + k as f32 * 0.030;
        commands.spawn(vm_part(proot, &cap, &glove, Vec3::new(0.072, y, -0.004), Vec3::new(0.018, 0.022, 0.022), rz));
        commands.spawn(vm_part(proot, &cube, &glove_plate, Vec3::new(0.076, y, 0.004), Vec3::new(0.012, 0.014, 0.012), id));
    }
    // Curled thumb gripping the lower-left armor bumper cleanly clear of the OLED screen!
    commands.spawn(vm_part(proot, &cap, &glove, Vec3::new(-0.080, -0.098, 0.011), Vec3::new(0.018, 0.030, 0.018), Quat::from_rotation_z(-0.42)));
    // Left armored forearm descending into bottom-left corner.
    commands.spawn(vm_part(proot, &cap, &glove, Vec3::new(-0.045, -0.20, -0.065), Vec3::new(0.082, 0.11, 0.072), Quat::from_rotation_x(0.45) * Quat::from_rotation_z(-0.22)));
    commands.spawn(vm_part(proot, &cyl, &glove_plate, Vec3::new(-0.036, -0.165, -0.05), Vec3::new(0.048, 0.034, 0.048), Quat::from_rotation_x(0.45) * Quat::from_rotation_z(-0.22)));
}

fn sync_overlay_fov(main: Query<&Projection, (With<PlayerCamera>, Without<OverlayCamera>)>, mut overlay: Query<&mut Projection, With<OverlayCamera>>) {
    let (Ok(Projection::Perspective(m)), Ok(mut o)) = (main.single(), overlay.single_mut()) else { return };
    if let Projection::Perspective(op) = o.as_mut()
        && (op.fov - m.fov).abs() > 1e-4
    {
        op.fov = m.fov;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn animate_viewmodels(
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
    let amp = if p.grounded { (speed / crate::player::MOVE_SPEED).min(1.3) } else { 0.15 };
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
        l.color = if shown == Weapon::Plasma { Color::srgb(0.3, 0.95, 1.0) } else { Color::srgb(1.0, 0.72, 0.35) };
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
    (fx, bolts): (Res<FxAssets>, Res<BoltAssets>),
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    mut players: Query<&mut Player>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    mut jammers: Query<(&mut DemonJammer, &GlobalTransform)>,
    mut bot: Option<ResMut<BotControl>>,
) {
    let dt = time.delta_secs();
    let Ok(mut player) = players.single_mut() else { return };
    let Ok(cam) = cams.single() else { return };
    ws.cooldown = (ws.cooldown - dt).max(0.0);
    ws.swap_timer = (ws.swap_timer - dt).max(0.0);
    ws.recoil = (ws.recoil - dt * 5.0).max(0.0);
    ws.muzzle = (ws.muzzle - dt).max(0.0);
    if ws.pump > 0.0 {
        ws.pump = (ws.pump - dt / 0.35).max(0.0);
    }

    // Emergency plasma trickle: combat can never soft-lock.
    if player.ammo_shells == 0 && player.ammo_plasma < 10 {
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
    } else if keys.just_pressed(KeyCode::Tab) || scroll.delta.y.abs() > 0.0 {
        want = Some(ws.current.other());
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
        let other = ws.current.other();
        if other.has_ammo(&player) && ws.switch_to(other) {
            sfx.play(SfxEvent::WeaponSwap);
        } else if mouse.just_pressed(MouseButton::Left) {
            sfx.play(SfxEvent::Denied);
        }
        return;
    }
    ws.force_fire = false;
    ws.cooldown = fire_cooldown(ws.current, &mods, signal.bars);

    let origin = cam.translation();
    let rot = cam.rotation();
    let fwd = rot * Vec3::NEG_Z;
    let muzzle = origin + rot * Vec3::new(0.2, -0.17, -0.85);

    // Snapshot targets for ray tests.
    let snapshot: Vec<(Vec3, Enemy, bool)> = enemies.iter().map(|(e, g, m)| (g.translation(), e.clone(), m.is_some())).collect();
    let refs: Vec<(Vec3, &Enemy)> = snapshot.iter().map(|(p, e, _)| (*p, e)).collect();
    let jam_list: Vec<Vec3> = jammers.iter().filter(|(j, _)| j.is_active()).map(|(_, g)| g.translation()).collect();

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
            let spread = PELLET_SPREAD * mods.pellet_spread_mult;
            let mut enemy_dmg = vec![0.0f32; refs.len()];
            let mut enemy_kb = vec![Vec3::ZERO; refs.len()];
            let mut jam_dmg = vec![0.0f32; jam_list.len()];
            let mut wall_sparks = 0;
            for i in 0..pellets {
                let a = rng.random_range(0.0..TAU);
                let r = spread * rng.random::<f32>().sqrt();
                let local = Vec3::new(a.cos() * r, a.sin() * r, -1.0).normalize();
                let dir = rot * local;
                let (dist, target) = resolve_hitscan(&grid, origin, dir, HITSCAN_RANGE, &refs, &jam_list);
                let hit = origin + dir * dist;
                let falloff = (1.0 - (dist - 18.0).max(0.0) / 40.0).clamp(0.4, 1.0);
                if i % 2 == 0 {
                    spawn_tracer(&mut commands, &fx, muzzle, hit);
                }
                match target {
                    HitTarget::Enemy(k) => {
                        enemy_dmg[k] += PELLET_DAMAGE * falloff;
                        enemy_kb[k] += dir * 1.1;
                        spawn_particles(&mut commands, &fx, hit, BurstColor::Gore, 3, 4.2, 0.075);
                    }
                    HitTarget::Jammer(k) => {
                        jam_dmg[k] += PELLET_DAMAGE * falloff;
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
            let (dist, _) = resolve_hitscan(&grid, origin, fwd, HITSCAN_RANGE, &refs, &jam_list);
            let aim = origin + fwd * dist.max(2.0);
            let dir = (aim - muzzle).normalize_or_zero();
            commands.spawn((
                LevelEntity,
                PlayerProjectile { vel: dir * PLASMA_SPEED, damage: PLASMA_DAMAGE, life: 2.0 },
                Mesh3d(bolts.core_mesh.clone()),
                MeshMaterial3d(bolts.core_mat.clone()),
                Transform::from_translation(muzzle).with_rotation(Quat::from_rotation_arc(Vec3::Y, dir)),
                children![
                    (Mesh3d(bolts.halo_mesh.clone()), MeshMaterial3d(bolts.halo_mat.clone()), Transform::default()),
                    (PointLight { color: Color::srgb(0.3, 0.95, 1.0), intensity: 85_000.0, range: 6.0, shadow_maps_enabled: false, ..default() }, Transform::default()),
                ],
            ));
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
    mut bolts: Query<(Entity, &mut PlayerProjectile, &mut Transform)>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    mut jammers: Query<(&mut DemonJammer, &GlobalTransform)>,
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
                    damage_enemy(&mut en, b.damage, marked.is_some(), &mods);
                    en.knockback += Vec3::new(kb.x, 0.0, kb.z);
                    feed.hit(false);
                    sfx.play(SfxEvent::Hit);
                }
            }
            HitTarget::Jammer(i) => {
                if let Some((mut j, _)) = jammers.iter_mut().nth(i) {
                    damage_jammer(&mut j, b.damage);
                    feed.hit(false);
                    sfx.play(SfxEvent::Hit);
                }
            }
            HitTarget::Wall => {}
        }
        spawn_burst(&mut commands, &fx, hit - dir * 0.1, BurstColor::Cyan, 7, 4.8, 0.065);
        spawn_shockwave(&mut commands, &fx, &mut materials, hit - dir * 0.12, 0.85, 0.18, Color::srgb(0.3, 0.95, 1.0), 0.7, false);
        commands.entity(e).try_despawn();
    }
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
        for (i, k) in [PickupKind::Shells, PickupKind::Health, PickupKind::Plasma].into_iter().enumerate() {
            let a = i as f32 * TAU / 3.0;
            spawn_pickup(&mut commands, &pickups, k, base + Vec3::new(a.cos(), 0.0, a.sin()) * 1.2, true);
        }
    }
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponState>()
            .configure_sets(Update, CombatDamage.in_set(GameSet::Combat))
            .add_systems(Startup, (create_pickup_assets, create_bolt_assets))
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
    fn floor_pickups_placed_on_floor_cells() {
        let g = crate::level::generate_level(2, 11);
        let plan = floor_pickup_plan(&g);
        assert!(!plan.is_empty());
        for (_, c) in plan {
            assert!(matches!(g.cell(c), Cell::Floor { .. }));
        }
    }
}
