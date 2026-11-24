//! Pooled floor & wall combat decals: blood / ichor splatter, scorch marks and
//! plasma burns.
//!
//! Gameplay pushes [`DecalRequest`]s into the [`DecalQueue`] (a reused buffer,
//! no per-hit allocation); [`spawn_decals`] places them flush on the real level
//! surface and recycles the oldest beyond [`MAX_DECALS`] (FIFO ring buffer in
//! [`DecalPool`]). Every decal shares one unit quad mesh and one of five
//! startup materials (64x64 procedural pixel-art textures, `AlphaMode::Blend`),
//! so spawning a decal never touches `Assets<Mesh>` / `Assets<StandardMaterial>`.
//! Decals are [`LevelEntity`]s: floor transitions and restarts clear them.

use std::collections::VecDeque;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::assets_gen::hash_noise;
use crate::level::{Cell, LevelEntity, LevelGrid};
use crate::physics::sample_floor_height;
use crate::state::GameSet;

/// Live decals at most (oldest recycled first).
pub const MAX_DECALS: usize = 96;
/// Procedural decal texture edge (pixels).
pub const DECAL_TEX: u32 = 64;
/// Anti-z-fight lift of floor decals above the floor (base; see [`floor_lift`]).
pub const DECAL_FLOOR_LIFT_MIN: f32 = 0.018;
/// Upper bound of the staggered floor lift.
pub const DECAL_FLOOR_LIFT_MAX: f32 = 0.028;
/// Offset of wall decals along the surface normal.
pub const DECAL_WALL_OFFSET: f32 = 0.022;
/// Floor requests more than this above the floor (e.g. a mid-air blast) are skipped.
pub const DECAL_MAX_FLOOR_GAP: f32 = 2.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecalKind {
    /// Imps, Cacodemons, general gore.
    BloodCrimson,
    /// Jammer Fiend.
    IchorGreen,
    /// Phase Stalker.
    IchorViolet,
    /// Rockets, barrels, Hell Baron / Carrier Daemon blasts.
    Scorch,
    /// Plasma impacts, 5G Overload.
    PlasmaBurn,
}

impl DecalKind {
    pub const ALL: [DecalKind; 5] = [DecalKind::BloodCrimson, DecalKind::IchorGreen, DecalKind::IchorViolet, DecalKind::Scorch, DecalKind::PlasmaBurn];

    pub fn index(self) -> usize {
        self as usize
    }

    fn is_splatter(self) -> bool {
        matches!(self, DecalKind::BloodCrimson | DecalKind::IchorGreen | DecalKind::IchorViolet)
    }

    /// `(core, rim)` palette of the splatter / burn.
    fn palette(self) -> ([u8; 3], [u8; 3]) {
        match self {
            DecalKind::BloodCrimson => ([92, 6, 8], [150, 14, 12]),
            DecalKind::IchorGreen => ([40, 110, 18], [120, 230, 50]),
            DecalKind::IchorViolet => ([70, 12, 96], [190, 60, 230]),
            DecalKind::Scorch => ([12, 10, 9], [48, 34, 26]),
            DecalKind::PlasmaBurn => ([10, 26, 34], [80, 230, 255]),
        }
    }
}

/// Where a decal sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecalSurface {
    /// Snap onto the level floor under `pos` (skipped over pits / walls / far above the floor).
    Floor,
    /// Flush on a wall (or any raycast surface) with this outward unit normal.
    Wall(Vec3),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecalRequest {
    pub kind: DecalKind,
    pub pos: Vec3,
    pub surface: DecalSurface,
    /// Edge length in meters.
    pub size: f32,
}

impl DecalRequest {
    pub fn floor(kind: DecalKind, pos: Vec3, size: f32) -> Self {
        DecalRequest { kind, pos, surface: DecalSurface::Floor, size }
    }

    /// A raycast surface hit: walls get a wall decal, upward-facing hits snap to the floor.
    pub fn surface(kind: DecalKind, point: Vec3, normal: Vec3, size: f32) -> Self {
        let n = normal.normalize_or(Vec3::Y);
        let surface = if n.y > 0.7 { DecalSurface::Floor } else { DecalSurface::Wall(n) };
        DecalRequest { kind, pos: point, surface, size }
    }
}

/// Decals requested this frame (reused buffer, drained by [`spawn_decals`]).
#[derive(Resource, Debug)]
pub struct DecalQueue {
    pub pending: Vec<DecalRequest>,
}

impl Default for DecalQueue {
    fn default() -> Self {
        Self { pending: Vec::with_capacity(MAX_DECALS) }
    }
}

impl DecalQueue {
    pub fn push(&mut self, r: DecalRequest) {
        // Bound a single frame's burst (a pool turnover is the most that can show).
        if self.pending.len() < MAX_DECALS {
            self.pending.push(r);
        }
    }
}

/// FIFO ring buffer of live decal entities.
#[derive(Resource, Debug)]
pub struct DecalPool {
    pub ring: VecDeque<Entity>,
    /// Decals spawned so far (staggers the anti-z-fight lift).
    pub spawned: u64,
}

impl Default for DecalPool {
    fn default() -> Self {
        Self { ring: VecDeque::with_capacity(MAX_DECALS), spawned: 0 }
    }
}

impl DecalPool {
    /// Register a new decal; returns the evicted oldest one once the pool is full.
    pub fn push(&mut self, e: Entity) -> Option<Entity> {
        let evicted = if self.ring.len() >= MAX_DECALS { self.ring.pop_front() } else { None };
        self.ring.push_back(e);
        self.spawned += 1;
        evicted
    }
}

/// Shared decal quad + one material per [`DecalKind`].
#[derive(Resource, Clone)]
pub struct DecalAssets {
    pub quad: Handle<Mesh>,
    pub materials: [Handle<StandardMaterial>; 5],
}

impl DecalAssets {
    pub fn material(&self, k: DecalKind) -> Handle<StandardMaterial> {
        self.materials[k.index()].clone()
    }
}

/// Unit quad on the XZ plane facing `+Y` (`TriangleList`, `{POSITION, NORMAL, UV_0}`, `U32`).
pub fn decal_quad_mesh() -> Mesh {
    let p = [[-0.5, 0.0, -0.5], [0.5, 0.0, -0.5], [0.5, 0.0, 0.5], [-0.5, 0.0, 0.5]];
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, p.to_vec())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
        // CCW seen from +Y.
        .with_inserted_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]))
}

/// RGBA texel of a decal texture (deterministic; transparent outer border).
pub fn decal_texel(kind: DecalKind, x: u32, y: u32) -> [u8; 4] {
    let n = DECAL_TEX;
    if x < 2 || y < 2 || x >= n - 2 || y >= n - 2 {
        return [0, 0, 0, 0];
    }
    let salt = 31 + kind.index() as u32 * 977;
    let c = (n as f32 - 1.0) * 0.5;
    let (dx, dy) = (x as f32 - c, y as f32 - c);
    let r = (dx * dx + dy * dy).sqrt() / c; // 0 center .. ~1.41 corner
    let ang = dy.atan2(dx);
    // Lumpy outline: low-frequency angular wobble + per-pixel grain.
    let lobes = 0.08 * (ang * 5.0 + kind.index() as f32).sin() + 0.06 * (ang * 9.0 + 1.3).sin();
    let grain = hash_noise(x, y, salt);
    let (core, rim) = kind.palette();
    let mix = |a: [u8; 3], b: [u8; 3], t: f32| -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        [0, 1, 2].map(|i| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t) as u8)
    };
    let shade = |c: [u8; 3], f: f32| c.map(|v| (v as f32 * f).clamp(0.0, 255.0) as u8);
    let edge = if kind.is_splatter() { 0.62 } else { 0.78 } + lobes;
    let (rgb, alpha) = if r < edge {
        let t = r / edge;
        match kind {
            DecalKind::Scorch => {
                // Charred core with radial soot streaks fading out.
                let streak = 0.5 + 0.5 * (ang * 13.0).sin();
                (shade(mix(core, rim, t * 0.8 + streak * 0.2), 0.8 + grain * 0.4), 0.92 - t * t * 0.55 - grain * 0.08)
            }
            DecalKind::PlasmaBurn => {
                // Dark glassy core, bright cyan crackle rim.
                let rim_k = ((t - 0.62) / 0.3).clamp(0.0, 1.0);
                (shade(mix(core, rim, rim_k * (0.7 + 0.3 * grain)), 0.85 + grain * 0.3), 0.55 + rim_k * 0.4)
            }
            _ => (shade(mix(core, rim, t * 0.7 + grain * 0.3), 0.75 + grain * 0.4), 0.95 - t * 0.2),
        }
    } else if kind.is_splatter() {
        // Satellite droplets out to the border.
        let drop = hash_noise(x / 3, y / 3, salt + 7);
        if r < 0.95 && drop > 0.86 { (shade(rim, 0.6 + grain * 0.3), 0.85) } else { ([0, 0, 0], 0.0) }
    } else {
        ([0, 0, 0], 0.0)
    };
    // Crunchy quantized alpha (pixel-art edges).
    let a = ((alpha.clamp(0.0, 1.0) * 4.0).round() / 4.0 * 255.0) as u8;
    [rgb[0], rgb[1], rgb[2], a]
}

/// Procedural 64x64 decal texture (nearest sampling, clamped edges).
pub fn decal_image(kind: DecalKind) -> Image {
    let mut data = Vec::with_capacity((DECAL_TEX * DECAL_TEX * 4) as usize);
    for y in 0..DECAL_TEX {
        for x in 0..DECAL_TEX {
            data.extend_from_slice(&decal_texel(kind, x, y));
        }
    }
    let mut img = Image::new(
        Extent3d { width: DECAL_TEX, height: DECAL_TEX, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::nearest()
    });
    img
}

/// Shared translucent material of one decal kind.
pub fn decal_material(kind: DecalKind, tex: Handle<Image>) -> StandardMaterial {
    let emissive = match kind {
        // Faint cooling glow only where the texture is bright (rim crackle).
        DecalKind::PlasmaBurn => LinearRgba::rgb(0.25, 0.9, 1.2),
        DecalKind::IchorGreen => LinearRgba::rgb(0.05, 0.22, 0.02),
        _ => LinearRgba::BLACK,
    };
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(tex.clone()),
        emissive,
        emissive_texture: (emissive != LinearRgba::BLACK).then_some(tex),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: if kind.is_splatter() { 0.35 } else { 0.95 },
        reflectance: 0.3,
        depth_bias: 4.0,
        ..default()
    }
}

fn create_decal_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>, mut images: ResMut<Assets<Image>>) {
    let quad = meshes.add(decal_quad_mesh());
    let materials = DecalKind::ALL.map(|k| {
        let tex = images.add(decal_image(k));
        materials.add(decal_material(k, tex))
    });
    commands.insert_resource(DecalAssets { quad, materials });
}

/// Floor lift of the `n`-th spawned decal: staggered inside
/// `[DECAL_FLOOR_LIFT_MIN, DECAL_FLOOR_LIFT_MAX]` so overlapping decals never z-fight.
pub fn floor_lift(n: u64) -> f32 {
    let steps = 6;
    DECAL_FLOOR_LIFT_MIN + (DECAL_FLOOR_LIFT_MAX - DECAL_FLOOR_LIFT_MIN) * (n % steps) as f32 / (steps - 1) as f32
}

/// Whether a floor decal may land on the cell under `(x, z)`.
fn decal_floor_cell_ok(grid: &LevelGrid, x: f32, z: f32) -> bool {
    let c = grid.world_to_cell(Vec3::new(x, 0.0, z));
    grid.in_bounds(c) && !matches!(grid.cell(c), Cell::Wall | Cell::HazardPit)
}

/// Placement transform of a decal (unit quad scaled to `size`), or `None`
/// when it must be skipped (pits, walls, out of bounds, far above the floor).
pub fn decal_transform(grid: Option<&LevelGrid>, req: &DecalRequest, seq: u64) -> Option<Transform> {
    let spin = Quat::from_rotation_y(hash_noise(seq as u32, 17, 5) * std::f32::consts::TAU);
    let size = req.size.clamp(0.05, 8.0);
    let scale = Vec3::new(size, 1.0, size);
    match req.surface {
        DecalSurface::Floor => {
            let (x, z) = (req.pos.x, req.pos.z);
            let (h, normal) = match grid {
                Some(g) => {
                    if !decal_floor_cell_ok(g, x, z) {
                        return None;
                    }
                    let h = sample_floor_height(g, x, z)?;
                    let c = g.world_to_cell(Vec3::new(x, 0.0, z));
                    // Use the struck cell's exact ramp slope (never cross-cell seams).
                    let n = match g.cell(c) {
                        Cell::Ramp { dir, low_height, high_height } => {
                            let o = dir.offset();
                            let slope = (high_height - low_height) / crate::level::CELL_SIZE;
                            Vec3::new(-(o.x as f32) * slope, 1.0, -(o.y as f32) * slope).normalize()
                        }
                        _ => Vec3::Y,
                    };
                    (h, n)
                }
                None => (0.0, Vec3::Y),
            };
            if req.pos.y - h > DECAL_MAX_FLOOR_GAP || req.pos.y < h - 1.0 {
                return None;
            }
            let pos = Vec3::new(x, h + floor_lift(seq), z);
            Some(Transform::from_translation(pos).with_rotation(Quat::from_rotation_arc(Vec3::Y, normal) * spin).with_scale(scale))
        }
        DecalSurface::Wall(n) => {
            let n = n.try_normalize()?;
            let mut wall_size = size;
            if let Some(g) = grid
                && n.y.abs() < 0.7
            {
                // Shrink or skip wall decals near doorway corners so they never hang into openings.
                let tangent = Vec3::new(-n.z, 0.0, n.x).normalize_or_zero();
                let backing = req.pos - n * 0.15;
                let has_wall = |p: Vec3| {
                    let c = g.world_to_cell(p);
                    g.in_bounds(c) && g.cell(c).is_wall()
                };
                if has_wall(backing) {
                    let half = wall_size * 0.5;
                    if !has_wall(backing + tangent * half) || !has_wall(backing - tangent * half) {
                        let smaller = wall_size * 0.5;
                        let sh = smaller * 0.5;
                        if smaller >= 0.12 && has_wall(backing + tangent * sh) && has_wall(backing - tangent * sh) {
                            wall_size = smaller;
                        } else {
                            return None;
                        }
                    }
                }
            }
            let pos = req.pos + n * (DECAL_WALL_OFFSET + 0.002 * (seq % 4) as f32);
            if !pos.is_finite() {
                return None;
            }
            let wscale = Vec3::new(wall_size, 1.0, wall_size);
            Some(Transform::from_translation(pos).with_rotation(Quat::from_rotation_arc(Vec3::Y, n) * spin).with_scale(wscale))
        }
    }
}

/// Marker of a pooled decal entity.
#[derive(Component, Debug, Clone, Copy)]
pub struct Decal(pub DecalKind);

/// Drain the queue: place, spawn and FIFO-recycle decals (no asset allocation).
pub fn spawn_decals(mut commands: Commands, grid: Option<Res<LevelGrid>>, assets: Option<Res<DecalAssets>>, mut queue: ResMut<DecalQueue>, mut pool: ResMut<DecalPool>) {
    let Some(assets) = assets else {
        queue.pending.clear();
        return;
    };
    for req in queue.pending.drain(..) {
        let Some(tf) = decal_transform(grid.as_deref(), &req, pool.spawned) else { continue };
        let e = commands.spawn((LevelEntity, Decal(req.kind), Mesh3d(assets.quad.clone()), MeshMaterial3d(assets.material(req.kind)), tf)).id();
        if let Some(old) = pool.push(e) {
            commands.entity(old).try_despawn();
        }
    }
}

/// A new floor (or restart) despawned every `LevelEntity`: forget stale entries.
fn reset_decal_pool(mut pool: ResMut<DecalPool>, mut queue: ResMut<DecalQueue>) {
    pool.ring.clear();
    queue.pending.clear();
}

pub struct DecalsPlugin;

impl Plugin for DecalsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DecalQueue>()
            .init_resource::<DecalPool>()
            .add_systems(Startup, create_decal_assets)
            .add_systems(Update, reset_decal_pool.run_if(resource_exists_and_changed::<LevelGrid>).in_set(GameSet::World))
            .add_systems(Update, spawn_decals.in_set(GameSet::Ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raytracing::rt_mesh_convertible;

    #[test]
    fn quad_mesh_is_rt_convertible_u32_with_uvs() {
        let m = decal_quad_mesh();
        assert!(rt_mesh_convertible(&m));
        assert!(matches!(m.indices(), Some(Indices::U32(i)) if i.len() == 6));
        assert!(m.contains_attribute(Mesh::ATTRIBUTE_UV_0));
    }

    #[test]
    fn decal_textures_have_transparent_borders_and_opaque_cores() {
        for k in DecalKind::ALL {
            for i in 0..DECAL_TEX {
                for (x, y) in [(i, 0), (0, i), (i, DECAL_TEX - 1), (DECAL_TEX - 1, i), (i, 1), (1, i)] {
                    assert_eq!(decal_texel(k, x, y)[3], 0, "{k:?} border texel ({x},{y}) must be transparent");
                }
            }
            assert!(decal_texel(k, 32, 32)[3] > 100, "{k:?} core is visible");
            let img = decal_image(k);
            assert_eq!(img.width(), DECAL_TEX);
        }
    }

    #[test]
    fn pool_is_fifo_bounded() {
        let mut pool = DecalPool::default();
        let mut world = World::new();
        let ids: Vec<Entity> = (0..MAX_DECALS + 5).map(|_| world.spawn_empty().id()).collect();
        let mut evicted = Vec::new();
        for &e in &ids {
            if let Some(o) = pool.push(e) {
                evicted.push(o);
            }
        }
        assert_eq!(pool.ring.len(), MAX_DECALS);
        assert_eq!(evicted, ids[..5].to_vec(), "oldest evicted first");
        assert_eq!(pool.ring.front(), Some(&ids[5]));
    }

    #[test]
    fn floor_lift_is_staggered_in_range() {
        let lifts: Vec<f32> = (0..12).map(floor_lift).collect();
        assert!(lifts.iter().all(|l| (DECAL_FLOOR_LIFT_MIN..=DECAL_FLOOR_LIFT_MAX).contains(l)));
        assert!(lifts[0] != lifts[1]);
    }

    #[test]
    fn placement_snaps_floor_skips_pits_and_offsets_walls() {
        let g = LevelGrid::from_ascii(&["#####", "#...#", "#.~.#", "#...#", "#####"]);
        let cell = |x: i32, z: i32| g.cell_center(IVec2::new(x, z));
        // Floor: snapped onto the floor, flat, lifted.
        let tf = decal_transform(Some(&g), &DecalRequest::floor(DecalKind::BloodCrimson, cell(1, 1) + Vec3::Y * 1.0, 1.0), 0).unwrap();
        assert!((tf.translation.y - floor_lift(0)).abs() < 1e-4, "{:?}", tf.translation);
        assert!((tf.rotation * Vec3::Y).dot(Vec3::Y) > 0.999);
        // Pit and wall cells are skipped.
        assert!(decal_transform(Some(&g), &DecalRequest::floor(DecalKind::Scorch, cell(2, 2), 1.0), 0).is_none());
        assert!(decal_transform(Some(&g), &DecalRequest::floor(DecalKind::Scorch, cell(0, 0), 1.0), 0).is_none());
        // Far above the floor: skipped.
        assert!(decal_transform(Some(&g), &DecalRequest::floor(DecalKind::Scorch, cell(1, 1) + Vec3::Y * 5.0, 1.0), 0).is_none());
        // Wall: offset along the normal, quad normal == wall normal.
        let hit = Vec3::new(4.0, 1.5, 6.0);
        let tf = decal_transform(Some(&g), &DecalRequest::surface(DecalKind::PlasmaBurn, hit, Vec3::X, 0.6), 3).unwrap();
        let off = (tf.translation - hit).dot(Vec3::X);
        assert!((0.015..0.04).contains(&off), "wall offset {off}");
        assert!((tf.rotation * Vec3::Y).dot(Vec3::X) > 0.999);
        // Upward surface hits become floor decals.
        assert_eq!(DecalRequest::surface(DecalKind::Scorch, hit, Vec3::Y, 1.0).surface, DecalSurface::Floor);
    }
}
