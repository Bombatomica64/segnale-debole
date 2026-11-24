//! 100% procedural 64x64 retro pixel-art textures + shared materials and meshes.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::level::CELL_SIZE;

pub const TEX_SIZE: u32 = 64;

/// Shared render handles created once at startup.
#[derive(Resource, Clone)]
pub struct GameAssets {
    // Materials
    pub wall: Handle<StandardMaterial>,
    pub wall_tall: Handle<StandardMaterial>,
    pub floor: Handle<StandardMaterial>,
    pub regolith: Handle<StandardMaterial>,
    pub catwalk: Handle<StandardMaterial>,
    pub ceiling: Handle<StandardMaterial>,
    pub lava: Handle<StandardMaterial>,
    pub jammer: Handle<StandardMaterial>,
    pub jammer_core: Handle<StandardMaterial>,
    pub tower_panel: Handle<StandardMaterial>,
    pub beacon: Handle<StandardMaterial>,
    pub lamp: Handle<StandardMaterial>,
    // Textures (kept for UI / Milestone 2 viewmodels)
    pub tex_wall: Handle<Image>,
    pub tex_jammer: Handle<Image>,
    pub tex_tower: Handle<Image>,
    // Meshes
    pub beacon_mesh: Handle<Mesh>,
    pub jammer_core_mesh: Handle<Mesh>,
    /// Small reusable shapes for Milestone 2 (projectiles, gibs, pickups).
    pub sphere_small: Handle<Mesh>,
    pub cube_small: Handle<Mesh>,
}

pub struct AssetsGenPlugin;

impl Plugin for AssetsGenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, create_game_assets);
    }
}

// ---------------------------------------------------------------------------
// Pixel buffer helpers
// ---------------------------------------------------------------------------

/// Simple RGBA8 canvas.
pub struct Canvas {
    pub size: u32,
    pub data: Vec<u8>,
}

impl Canvas {
    pub fn new(size: u32) -> Self {
        Canvas { size, data: vec![0; (size * size * 4) as usize] }
    }

    pub fn set(&mut self, x: u32, y: u32, c: [u8; 3]) {
        let i = ((y % self.size) * self.size + (x % self.size)) as usize * 4;
        self.data[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
    }

    pub fn get(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y % self.size) * self.size + (x % self.size)) as usize * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn fill_with(&mut self, mut f: impl FnMut(u32, u32) -> [u8; 3]) {
        for y in 0..self.size {
            for x in 0..self.size {
                let c = f(x, y);
                self.set(x, y, c);
            }
        }
    }

    pub fn rect(&mut self, x0: u32, y0: u32, w: u32, h: u32, c: [u8; 3]) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.set(x, y, c);
            }
        }
    }

    pub fn into_image(self) -> Image {
        let mut img = Image::new(
            Extent3d { width: self.size, height: self.size, depth_or_array_layers: 1 },
            TextureDimension::D2,
            self.data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        );
        // Nearest-neighbour filtering (crunchy pixels) + repeat for tiling UVs.
        img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::nearest()
        });
        img
    }
}

/// Deterministic integer hash noise in `[0, 1)`.
pub fn hash_noise(x: u32, y: u32, salt: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8DA6_B343) ^ y.wrapping_mul(0xD816_3841) ^ salt.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5BD1_E995);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}

fn shade(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).clamp(0.0, 255.0) as u8)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t) as u8)
}

// ---------------------------------------------------------------------------
// Textures
// ---------------------------------------------------------------------------

/// Riveted bunker steel panels with a hazard stripe band and grime.
pub fn bunker_wall_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    let steel = [92, 88, 86];
    c.fill_with(|x, y| {
        let n = hash_noise(x, y, 1);
        let mut col = shade(steel, 0.8 + n * 0.3);
        // Panel seams every 32 px.
        if x % 32 == 0 || y % 32 == 0 {
            col = shade(steel, 0.45);
        } else if x % 32 == 1 || y % 32 == 1 {
            col = shade(steel, 1.25);
        }
        // Grime streaks dripping down.
        let streak = hash_noise(x, 0, 7);
        if streak > 0.82 && hash_noise(x, y / 6, 8) > 0.35 {
            col = mix(col, [60, 30, 20], 0.45);
        }
        col
    });
    // Hazard stripe band.
    for y in 44..52 {
        for x in 0..TEX_SIZE {
            let stripe = ((x + y) / 4) % 2 == 0;
            let base = if stripe { [222, 170, 20] } else { [24, 22, 20] };
            c.set(x, y, shade(base, 0.85 + hash_noise(x, y, 3) * 0.2));
        }
    }
    // Rivets at panel corners.
    for (rx, ry) in [(5, 5), (27, 5), (5, 27), (27, 27), (37, 5), (59, 5), (37, 27), (59, 27), (5, 37), (27, 37), (37, 37), (59, 37), (5, 59), (27, 59), (37, 59), (59, 59)] {
        c.set(rx, ry, [170, 168, 160]);
        c.set(rx + 1, ry, [120, 118, 112]);
        c.set(rx, ry + 1, [70, 68, 64]);
        c.set(rx + 1, ry + 1, [50, 48, 46]);
    }
    c
}

/// Diamond-grated dark metal floor.
pub fn grated_floor_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let n = hash_noise(x, y, 11);
        let gx = (x % 8) as i32 - 4;
        let gy = (y % 8) as i32 - 4;
        let bar = (gx.abs() + gy.abs()) >= 4;
        let base = if bar { [78, 74, 70] } else { [22, 18, 18] };
        let mut col = shade(base, 0.8 + n * 0.35);
        if x % 32 == 0 || y % 32 == 0 {
            col = [40, 36, 34];
        }
        // Rust spots.
        if hash_noise(x / 3, y / 3, 12) > 0.9 {
            col = mix(col, [120, 50, 22], 0.6);
        }
        col
    });
    c
}

/// Martian rock / regolith: ochre-red speckled dirt with pebbles.
pub fn regolith_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let n1 = hash_noise(x, y, 21);
        let n2 = hash_noise(x / 4, y / 4, 22);
        let n3 = hash_noise(x / 8, y / 8, 23);
        let v = n1 * 0.25 + n2 * 0.4 + n3 * 0.35;
        let mut col = mix([92, 34, 20], [168, 78, 42], v);
        if n1 > 0.96 {
            col = [200, 120, 80];
        } else if n1 < 0.04 {
            col = [50, 18, 12];
        }
        col
    });
    c
}

/// Raised catwalk plate: tread plate with yellow edge trim.
pub fn catwalk_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let n = hash_noise(x, y, 31);
        let mut col = shade([110, 106, 98], 0.75 + n * 0.3);
        // Tread bumps (diagonal pairs).
        let (cx, cy) = (x % 10, y % 10);
        if (cx == 2 && cy < 4 && cy > 0) || (cx > 5 && cx < 9 && cy == 7) {
            col = [160, 156, 146];
        }
        if !(3..=60).contains(&x) || !(3..=60).contains(&y) {
            col = if ((x + y) / 3) % 2 == 0 { [210, 160, 24] } else { [30, 28, 24] };
        }
        col
    });
    c
}

/// Demonic flesh-wire: veiny meat with embedded black cables and glowing nodes.
pub fn flesh_wire_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let n = hash_noise(x / 2, y / 2, 41);
        let vein = ((x as f32 * 0.35 + (y as f32 * 0.21).sin() * 3.0).sin() * 0.5 + 0.5) > 0.88;
        let mut col = mix([96, 14, 22], [176, 52, 58], n);
        if vein {
            col = [220, 40, 50];
        }
        // Black cables running vertically with a wobble.
        let wob = ((y as f32 * 0.3).sin() * 2.0) as i32;
        let cx = (x as i32 + wob).rem_euclid(16);
        if cx == 0 || cx == 1 {
            col = [18, 12, 14];
        }
        if hash_noise(x / 4, y / 4, 42) > 0.94 {
            col = [255, 120, 40];
        }
        col
    });
    c
}

/// Molten lava / plasma: bright orange cells with dark cooled crust.
pub fn lava_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let fx = x as f32 / 64.0 * std::f32::consts::TAU;
        let fy = y as f32 / 64.0 * std::f32::consts::TAU;
        let w = ((fx * 2.0).sin() + (fy * 3.0 + fx).sin() + (fy * 2.0 - fx * 3.0).cos()) / 3.0;
        let n = hash_noise(x, y, 51) * 0.2;
        let v = (w * 0.5 + 0.5 + n).clamp(0.0, 1.0);
        if v > 0.62 {
            mix([255, 140, 20], [255, 240, 120], (v - 0.62) * 2.5)
        } else if v > 0.4 {
            mix([150, 20, 6], [255, 90, 10], (v - 0.4) * 4.5)
        } else {
            mix([30, 8, 6], [80, 14, 8], v * 2.5)
        }
    });
    c
}

/// Signal tower emissive panel: dark glass with glowing green signal bars and grid.
pub fn signal_panel_texture() -> Canvas {
    let mut c = Canvas::new(TEX_SIZE);
    c.fill_with(|x, y| {
        let n = hash_noise(x, y, 61);
        let mut col = shade([10, 30, 28], 0.8 + n * 0.4);
        if x % 8 == 0 || y % 8 == 0 {
            col = [20, 70, 60];
        }
        col
    });
    // Five ascending bars.
    for i in 0..5u32 {
        let h = 10 + i * 9;
        let x0 = 6 + i * 11;
        c.rect(x0, 58 - h, 8, h, [60, 255, 170]);
        c.rect(x0, 58 - h, 8, 1, [200, 255, 230]);
    }
    c
}

/// Darker steel ceiling plates.
pub fn ceiling_texture() -> Canvas {
    let mut c = bunker_wall_texture();
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let p = c.get(x, y);
            c.set(x, y, shade(mix(p, [40, 30, 30], 0.3), 0.55));
        }
    }
    c
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

fn textured(img: Handle<Image>, roughness: f32, metallic: f32) -> StandardMaterial {
    StandardMaterial {
        base_color_texture: Some(img),
        perceptual_roughness: roughness,
        metallic,
        reflectance: 0.3,
        ..default()
    }
}

pub fn create_game_assets(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let tex_wall = images.add(bunker_wall_texture().into_image());
    let tex_floor = images.add(grated_floor_texture().into_image());
    let tex_regolith = images.add(regolith_texture().into_image());
    let tex_catwalk = images.add(catwalk_texture().into_image());
    let tex_jammer = images.add(flesh_wire_texture().into_image());
    let tex_lava = images.add(lava_texture().into_image());
    let tex_tower = images.add(signal_panel_texture().into_image());
    let tex_ceiling = images.add(ceiling_texture().into_image());

    // Wall blocks are unit-height cuboids scaled on Y; tile the texture so a
    // 4m-wide face shows one 64px tile per 4m of height too.
    let wall_block_h = crate::level::CEILING_HEIGHT - crate::level::SLAB_BOTTOM;
    let tall_block_h = crate::level::ATRIUM_WALL_HEIGHT - crate::level::SLAB_BOTTOM;
    let mut wall = textured(tex_wall.clone(), 0.75, 0.35);
    wall.uv_transform = Affine2::from_scale(Vec2::new(1.0, wall_block_h / CELL_SIZE));
    let mut wall_tall = textured(tex_wall.clone(), 0.75, 0.35);
    wall_tall.uv_transform = Affine2::from_scale(Vec2::new(1.0, tall_block_h / CELL_SIZE));

    let lava = StandardMaterial {
        base_color_texture: Some(tex_lava.clone()),
        emissive_texture: Some(tex_lava.clone()),
        emissive: LinearRgba::rgb(6.0, 3.0, 1.0),
        perceptual_roughness: 0.9,
        ..default()
    };
    let jammer_core = StandardMaterial {
        base_color_texture: Some(tex_jammer.clone()),
        emissive_texture: Some(tex_jammer.clone()),
        emissive: LinearRgba::rgb(8.0, 0.8, 1.0),
        ..default()
    };
    let tower_panel = StandardMaterial {
        base_color_texture: Some(tex_tower.clone()),
        emissive_texture: Some(tex_tower.clone()),
        emissive: LinearRgba::rgb(3.0, 8.0, 6.0),
        ..default()
    };
    let beacon = StandardMaterial {
        base_color: Color::srgb(0.6, 1.0, 0.85),
        emissive: LinearRgba::rgb(10.0, 40.0, 30.0),
        unlit: true,
        ..default()
    };
    let lamp = StandardMaterial {
        base_color: Color::srgb(1.0, 0.7, 0.4),
        emissive: LinearRgba::rgb(12.0, 6.0, 2.5),
        unlit: true,
        ..default()
    };

    let assets = GameAssets {
        wall: materials.add(wall),
        wall_tall: materials.add(wall_tall),
        floor: materials.add(textured(tex_floor, 0.85, 0.4)),
        regolith: materials.add(textured(tex_regolith, 0.95, 0.0)),
        catwalk: materials.add(textured(tex_catwalk, 0.6, 0.6)),
        ceiling: materials.add(textured(tex_ceiling, 0.9, 0.2)),
        lava: materials.add(lava),
        jammer: materials.add(textured(tex_jammer.clone(), 0.5, 0.0)),
        jammer_core: materials.add(jammer_core),
        tower_panel: materials.add(tower_panel),
        beacon: materials.add(beacon),
        lamp: materials.add(lamp),
        tex_wall,
        tex_jammer,
        tex_tower,
        beacon_mesh: meshes.add(Sphere::new(0.55)),
        jammer_core_mesh: meshes.add(Sphere::new(0.6)),
        sphere_small: meshes.add(Sphere::new(0.2)),
        cube_small: meshes.add(Cuboid::new(0.3, 0.3, 0.3)),
    };
    commands.insert_resource(assets);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_are_64x64_opaque_and_deterministic() {
        for make in [bunker_wall_texture, grated_floor_texture, regolith_texture, catwalk_texture, flesh_wire_texture, lava_texture, signal_panel_texture, ceiling_texture] {
            let a = make();
            let b = make();
            assert_eq!(a.data.len(), 64 * 64 * 4);
            assert_eq!(a.data, b.data);
            assert!(a.data.chunks(4).all(|p| p[3] == 255));
        }
    }
}
