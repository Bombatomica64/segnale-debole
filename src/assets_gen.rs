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
    /// Per-sector static level materials, indexed by [`FloorTheme::index`].
    pub themes: [ThemeMaterials; 5],
    /// Pre-baked volatile server-coolant drum (one merged mesh per material,
    /// base at y = 0) shared by every barrel.
    pub barrel_model: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    /// Pre-baked jump pad base (plate, bevel, rim, vents), base at y = 0.
    pub pad_base_model: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    /// Glow ring + core disc of a jump pad (material swapped by power state).
    pub pad_glow_mesh: Handle<Mesh>,
    /// Shared jump pad glow materials: powered (cyan/acid-green) and dormant (dim crimson/amber).
    pub pad_powered: Handle<StandardMaterial>,
    pub pad_dormant: Handle<StandardMaterial>,
}

impl GameAssets {
    /// Static level materials of the sector theme for `floor`.
    pub fn theme_for_floor(&self, floor: u32) -> &ThemeMaterials {
        &self.themes[FloorTheme::for_floor(floor).index()]
    }
}

/// Static level materials of one sector theme.
#[derive(Clone)]
pub struct ThemeMaterials {
    pub theme: FloorTheme,
    pub wall: Handle<StandardMaterial>,
    pub wall_tall: Handle<StandardMaterial>,
    pub floor: Handle<StandardMaterial>,
    pub regolith: Handle<StandardMaterial>,
    pub catwalk: Handle<StandardMaterial>,
    pub ceiling: Handle<StandardMaterial>,
    pub lava: Handle<StandardMaterial>,
    pub lamp: Handle<StandardMaterial>,
}

// ---------------------------------------------------------------------------
// Floor 1-5 sector themes
// ---------------------------------------------------------------------------

/// Visual identity of a floor (Endless floors 6+ cycle through all five).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloorTheme {
    RustBunker,
    ToxicCoolant,
    CryoRelay,
    MagmaFoundry,
    ObsidianCore,
}

/// Palette + lighting of a sector theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloorThemeSpec {
    pub name: &'static str,
    /// Open-roof sky (camera clear color).
    pub sky_color: Color,
    pub fog_color: Color,
    /// Fog tint toward the directional light.
    pub fog_dir_color: Color,
    pub sun_color: Color,
    pub sun_illuminance: f32,
    /// Most ceiling lamps.
    pub lamp_warm: Color,
    /// Every third ceiling lamp.
    pub lamp_accent: Color,
    pub hazard_light_color: Color,
    /// Texture tints (sRGB multipliers) for the steel panels.
    pub wall_tint: [f32; 3],
    pub floor_tint: [f32; 3],
    pub regolith_tint: [f32; 3],
    /// Hazard stripe / trim accent on walls and catwalks.
    pub trim: [u8; 3],
    /// Hazard pit gradient: crust, mid glow, hottest.
    pub pit_ramp: [[u8; 3]; 3],
    /// Hazard pit emissive strength (linear).
    pub pit_emissive: [f32; 3],
}

impl FloorTheme {
    pub const ALL: [FloorTheme; 5] =
        [FloorTheme::RustBunker, FloorTheme::ToxicCoolant, FloorTheme::CryoRelay, FloorTheme::MagmaFoundry, FloorTheme::ObsidianCore];

    /// Theme of a floor: 1..=5 map in order, 6+ wrap around.
    pub fn for_floor(floor: u32) -> FloorTheme {
        FloorTheme::ALL[(floor.saturating_sub(1) as usize) % 5]
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        self.spec().name
    }

    pub fn sky_color(self) -> Color {
        self.spec().sky_color
    }

    pub fn spec(self) -> FloorThemeSpec {
        match self {
            FloorTheme::RustBunker => FloorThemeSpec {
                name: "RUST BUNKER",
                sky_color: Color::srgb(0.32, 0.04, 0.03),
                fog_color: Color::srgb(0.16, 0.025, 0.02),
                fog_dir_color: Color::srgba(1.0, 0.3, 0.2, 0.4),
                sun_color: Color::srgb(1.0, 0.35, 0.25),
                sun_illuminance: 1800.0,
                lamp_warm: Color::srgb(1.0, 0.62, 0.35),
                lamp_accent: Color::srgb(0.9, 0.25, 0.2),
                hazard_light_color: Color::srgb(1.0, 0.3, 0.05),
                wall_tint: [1.0, 1.0, 1.0],
                floor_tint: [1.0, 1.0, 1.0],
                regolith_tint: [1.0, 1.0, 1.0],
                trim: [222, 170, 20],
                pit_ramp: [[30, 8, 6], [255, 90, 10], [255, 240, 120]],
                pit_emissive: [6.0, 3.0, 1.0],
            },
            FloorTheme::ToxicCoolant => FloorThemeSpec {
                name: "TOXIC COOLANT",
                sky_color: Color::srgb(0.2, 0.1, 0.035),
                fog_color: Color::srgb(0.045, 0.085, 0.075),
                fog_dir_color: Color::srgba(0.45, 0.9, 0.55, 0.35),
                sun_color: Color::srgb(0.75, 0.95, 0.5),
                sun_illuminance: 1500.0,
                lamp_warm: Color::srgb(0.35, 1.0, 0.8),
                lamp_accent: Color::srgb(1.0, 0.7, 0.25),
                hazard_light_color: Color::srgb(0.4, 1.0, 0.15),
                wall_tint: [0.78, 0.88, 0.62],
                floor_tint: [0.8, 0.92, 0.72],
                regolith_tint: [0.62, 0.78, 0.5],
                trim: [120, 230, 40],
                pit_ramp: [[8, 28, 6], [50, 190, 20], [200, 255, 100]],
                pit_emissive: [1.6, 6.0, 0.8],
            },
            FloorTheme::CryoRelay => FloorThemeSpec {
                name: "CRYO RELAY",
                sky_color: Color::srgb(0.15, 0.05, 0.17),
                fog_color: Color::srgb(0.05, 0.07, 0.12),
                fog_dir_color: Color::srgba(0.55, 0.75, 1.0, 0.35),
                sun_color: Color::srgb(0.65, 0.75, 1.0),
                sun_illuminance: 1700.0,
                lamp_warm: Color::srgb(0.78, 0.92, 1.0),
                lamp_accent: Color::srgb(0.3, 0.85, 1.0),
                hazard_light_color: Color::srgb(0.3, 0.7, 1.0),
                wall_tint: [0.72, 0.84, 1.08],
                floor_tint: [0.78, 0.88, 1.06],
                regolith_tint: [0.7, 0.78, 1.0],
                trim: [200, 235, 255],
                pit_ramp: [[6, 14, 40], [30, 120, 230], [210, 245, 255]],
                pit_emissive: [1.0, 3.6, 7.0],
            },
            FloorTheme::MagmaFoundry => FloorThemeSpec {
                name: "MAGMA FOUNDRY",
                sky_color: Color::srgb(0.4, 0.11, 0.02),
                fog_color: Color::srgb(0.2, 0.065, 0.02),
                fog_dir_color: Color::srgba(1.0, 0.55, 0.15, 0.45),
                sun_color: Color::srgb(1.0, 0.55, 0.2),
                sun_illuminance: 2000.0,
                lamp_warm: Color::srgb(1.0, 0.72, 0.25),
                lamp_accent: Color::srgb(1.0, 0.42, 0.1),
                hazard_light_color: Color::srgb(1.0, 0.45, 0.08),
                wall_tint: [0.6, 0.54, 0.5],
                floor_tint: [0.66, 0.58, 0.54],
                regolith_tint: [0.55, 0.42, 0.38],
                trim: [255, 140, 20],
                pit_ramp: [[40, 6, 2], [255, 80, 0], [255, 225, 90]],
                pit_emissive: [9.0, 3.6, 0.6],
            },
            FloorTheme::ObsidianCore => FloorThemeSpec {
                name: "OBSIDIAN CORE",
                sky_color: Color::srgb(0.13, 0.01, 0.035),
                fog_color: Color::srgb(0.07, 0.02, 0.09),
                fog_dir_color: Color::srgba(0.7, 0.25, 0.9, 0.4),
                sun_color: Color::srgb(0.75, 0.3, 0.85),
                sun_illuminance: 1500.0,
                lamp_warm: Color::srgb(1.0, 0.22, 0.3),
                lamp_accent: Color::srgb(0.55, 0.45, 1.0),
                hazard_light_color: Color::srgb(0.85, 0.2, 0.95),
                wall_tint: [0.4, 0.36, 0.46],
                floor_tint: [0.46, 0.42, 0.52],
                regolith_tint: [0.42, 0.3, 0.4],
                trim: [200, 30, 70],
                pit_ramp: [[20, 4, 24], [170, 20, 100], [255, 130, 235]],
                pit_emissive: [5.0, 1.0, 6.0],
            },
        }
    }
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

/// Multiply every pixel by an sRGB tint.
pub fn tint_canvas(mut c: Canvas, tint: [f32; 3]) -> Canvas {
    for y in 0..c.size {
        for x in 0..c.size {
            let p = c.get(x, y);
            c.set(x, y, [0, 1, 2].map(|i| (p[i] as f32 * tint[i]).clamp(0.0, 255.0) as u8));
        }
    }
    c
}

/// Themed bunker wall: tinted steel with the theme's trim stripe band and
/// (Obsidian Core) glowing circuitry traces.
pub fn themed_wall_texture(spec: &FloorThemeSpec) -> Canvas {
    let mut c = tint_canvas(bunker_wall_texture(), spec.wall_tint);
    let dark = shade(spec.trim, 0.12);
    for y in 44..52 {
        for x in 0..TEX_SIZE {
            let stripe = ((x + y) / 4) % 2 == 0;
            let base = if stripe { spec.trim } else { dark };
            c.set(x, y, shade(base, 0.85 + hash_noise(x, y, 3) * 0.2));
        }
    }
    if spec.wall_tint[0] < 0.5 {
        // Circuitry trim: thin glowing traces with nodes.
        for x in 8..56 {
            c.set(x, 18, shade(spec.trim, 0.9));
        }
        for y in 8..18 {
            c.set(8, y, shade(spec.trim, 0.9));
            c.set(55, y + 4, shade(spec.trim, 0.9));
        }
        c.rect(7, 7, 3, 3, [255, 200, 230]);
        c.rect(54, 21, 3, 3, [180, 160, 255]);
    }
    c
}

/// Themed catwalk: tinted tread plate with the theme's trim edge.
pub fn themed_catwalk_texture(spec: &FloorThemeSpec) -> Canvas {
    let mut c = tint_canvas(catwalk_texture(), spec.floor_tint);
    let dark = shade(spec.trim, 0.12);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            if !(3..=60).contains(&x) || !(3..=60).contains(&y) {
                c.set(x, y, if ((x + y) / 3) % 2 == 0 { spec.trim } else { dark });
            }
        }
    }
    c
}

/// Themed hazard pit: the lava pattern re-ramped through the theme gradient.
pub fn themed_pit_texture(spec: &FloorThemeSpec) -> Canvas {
    let base = lava_texture();
    let mut c = Canvas::new(TEX_SIZE);
    let [lo, mid, hi] = spec.pit_ramp;
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let p = base.get(x, y);
            let l = (p[0] as f32 * 0.5 + p[1] as f32 * 0.4 + p[2] as f32 * 0.1) / 255.0;
            let col = if l < 0.45 { mix(lo, mid, l / 0.45) } else { mix(mid, hi, (l - 0.45) / 0.55) };
            c.set(x, y, col);
        }
    }
    c
}

/// Darkened themed ceiling plates.
pub fn themed_ceiling_texture(spec: &FloorThemeSpec) -> Canvas {
    let mut c = themed_wall_texture(spec);
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let p = c.get(x, y);
            c.set(x, y, shade(mix(p, [40, 30, 30], 0.3), 0.55));
        }
    }
    c
}

fn linear_scaled(c: Color, k: f32) -> LinearRgba {
    let l = c.to_linear();
    LinearRgba::rgb(l.red * k, l.green * k, l.blue * k)
}

fn build_theme_materials(theme: FloorTheme, images: &mut Assets<Image>, materials: &mut Assets<StandardMaterial>) -> ThemeMaterials {
    let spec = theme.spec();
    let tex_wall = images.add(themed_wall_texture(&spec).into_image());
    let tex_floor = images.add(tint_canvas(grated_floor_texture(), spec.floor_tint).into_image());
    let tex_regolith = images.add(tint_canvas(regolith_texture(), spec.regolith_tint).into_image());
    let tex_catwalk = images.add(themed_catwalk_texture(&spec).into_image());
    let tex_pit = images.add(themed_pit_texture(&spec).into_image());
    let tex_ceiling = images.add(themed_ceiling_texture(&spec).into_image());

    // Wall blocks are unit-height cuboids scaled on Y; tile the texture so a
    // 4m-wide face shows one 64px tile per 4m of height too.
    let wall_block_h = crate::level::CEILING_HEIGHT - crate::level::SLAB_BOTTOM;
    let tall_block_h = crate::level::ATRIUM_WALL_HEIGHT - crate::level::SLAB_BOTTOM;
    // Darker themes get a touch more metal sheen so silhouettes still read.
    let (rough, metal) = if spec.wall_tint[0] < 0.5 { (0.45, 0.7) } else { (0.75, 0.35) };
    let mut wall = textured(tex_wall.clone(), rough, metal);
    wall.uv_transform = Affine2::from_scale(Vec2::new(1.0, wall_block_h / CELL_SIZE));
    let mut wall_tall = textured(tex_wall, rough, metal);
    wall_tall.uv_transform = Affine2::from_scale(Vec2::new(1.0, tall_block_h / CELL_SIZE));
    let [er, eg, eb] = spec.pit_emissive;
    let lava = StandardMaterial {
        base_color_texture: Some(tex_pit.clone()),
        emissive_texture: Some(tex_pit),
        emissive: LinearRgba::rgb(er, eg, eb),
        perceptual_roughness: 0.9,
        ..default()
    };
    let lamp = StandardMaterial {
        base_color: spec.lamp_warm,
        emissive: linear_scaled(spec.lamp_warm, 12.0),
        unlit: true,
        ..default()
    };
    ThemeMaterials {
        theme,
        wall: materials.add(wall),
        wall_tall: materials.add(wall_tall),
        floor: materials.add(textured(tex_floor, 0.85, 0.4)),
        regolith: materials.add(textured(tex_regolith, 0.95, 0.0)),
        catwalk: materials.add(textured(tex_catwalk, 0.6, 0.6)),
        ceiling: materials.add(textured(tex_ceiling, 0.9, 0.2)),
        lava: materials.add(lava),
        lamp: materials.add(lamp),
    }
}

// ---------------------------------------------------------------------------
// Prop models (barrels, jump pads)
// ---------------------------------------------------------------------------

/// Volatile server-coolant drum: red hazard-striped canister, glowing molten
/// core band and top vents. Base at y = 0, ~1.25 m tall, ~0.5 m radius.
fn bake_barrel_model(materials: &mut Assets<StandardMaterial>, meshes: &mut Assets<Mesh>) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
    let shell = materials.add(StandardMaterial { base_color: Color::srgb(0.62, 0.08, 0.05), metallic: 0.55, perceptual_roughness: 0.45, ..default() });
    let stripe = materials.add(StandardMaterial {
        base_color: Color::srgb(0.95, 0.72, 0.08),
        emissive: LinearRgba::rgb(0.35, 0.22, 0.0),
        metallic: 0.3,
        perceptual_roughness: 0.5,
        ..default()
    });
    let steel = materials.add(StandardMaterial { base_color: Color::srgb(0.18, 0.18, 0.2), metallic: 0.85, perceptual_roughness: 0.35, ..default() });
    let core = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.6, 0.15),
        emissive: LinearRgba::rgb(14.0, 5.0, 0.6),
        unlit: true,
        ..default()
    });
    let cyl = Mesh::from(Cylinder::new(1.0, 1.0));
    let cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
    let mut b = crate::mesh_batch::MaterialBatches::new();
    let t = |y: f32, r: f32, h: f32| Transform::from_xyz(0.0, y, 0.0).with_scale(Vec3::new(r, h, r));
    // Canister body + rolled rims.
    b.add(&cyl, &shell, t(0.6, 0.48, 1.1));
    for y in [0.06, 1.14] {
        b.add(&cyl, &steel, t(y, 0.515, 0.08));
    }
    // Hazard stripe bands (alternating segments) above and below the core.
    for (k, y) in [0.28f32, 0.92].into_iter().enumerate() {
        for seg in 0..8 {
            let a = seg as f32 * std::f32::consts::TAU / 8.0 + k as f32 * 0.4;
            let mat = if seg % 2 == 0 { &stripe } else { &steel };
            b.add(
                &cube,
                mat,
                Transform::from_xyz(a.cos() * 0.49, y, a.sin() * 0.49)
                    .with_rotation(Quat::from_rotation_y(-a))
                    .with_scale(Vec3::new(0.06, 0.14, 0.39)),
            );
        }
    }
    // Molten coolant core band.
    b.add(&cyl, &core, t(0.6, 0.5, 0.16));
    // Top cap + vents + valve.
    b.add(&cyl, &steel, t(1.2, 0.42, 0.06));
    for k in 0..3 {
        b.add(&cube, &steel, Transform::from_xyz(-0.18 + k as f32 * 0.18, 1.25, 0.12).with_scale(Vec3::new(0.08, 0.05, 0.22)));
    }
    b.add(&cyl, &core, t(1.27, 0.07, 0.06));
    b.finish(meshes)
}

/// Jump pad base: beveled circular plate with a steel rim and vents.
fn bake_pad_base(materials: &mut Assets<StandardMaterial>, meshes: &mut Assets<Mesh>) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
    let plate = materials.add(StandardMaterial { base_color: Color::srgb(0.16, 0.17, 0.19), metallic: 0.8, perceptual_roughness: 0.4, ..default() });
    let trim = materials.add(StandardMaterial { base_color: Color::srgb(0.85, 0.65, 0.1), metallic: 0.5, perceptual_roughness: 0.5, ..default() });
    let bevel = Mesh::from(ConicalFrustum { radius_top: 1.05, radius_bottom: 1.3, height: 0.16 });
    let cyl = Mesh::from(Cylinder::new(1.0, 1.0));
    let cube = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
    let mut b = crate::mesh_batch::MaterialBatches::new();
    b.add(&bevel, &plate, Transform::from_xyz(0.0, 0.08, 0.0));
    b.add(&cyl, &plate, Transform::from_xyz(0.0, 0.17, 0.0).with_scale(Vec3::new(0.95, 0.04, 0.95)));
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::TAU / 8.0;
        b.add(
            &cube,
            &trim,
            Transform::from_xyz(a.cos() * 1.12, 0.17, a.sin() * 1.12).with_rotation(Quat::from_rotation_y(-a)).with_scale(Vec3::new(0.12, 0.03, 0.3)),
        );
    }
    b.finish(meshes)
}

/// Jump pad glow: emissive ring + core disc + chevron, one mesh.
fn bake_pad_glow() -> Mesh {
    let mut m = crate::mesh_batch::MeshBatch::new();
    m.append(&Mesh::from(Torus::new(0.72, 0.86)), &Transform::from_xyz(0.0, 0.2, 0.0));
    m.append(&Mesh::from(Cylinder::new(0.42, 0.03)), &Transform::from_xyz(0.0, 0.2, 0.0));
    m.build().expect("pad glow mesh")
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
    let tex_jammer = images.add(flesh_wire_texture().into_image());
    let tex_tower = images.add(signal_panel_texture().into_image());

    let themes = FloorTheme::ALL.map(|t| build_theme_materials(t, &mut images, &mut materials));
    let rust = themes[0].clone();

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
    let pad_powered = StandardMaterial {
        base_color: Color::srgb(0.4, 1.0, 0.85),
        emissive: LinearRgba::rgb(2.0, 14.0, 10.0),
        unlit: true,
        ..default()
    };
    let pad_dormant = StandardMaterial {
        base_color: Color::srgb(0.45, 0.1, 0.06),
        emissive: LinearRgba::rgb(1.4, 0.3, 0.08),
        unlit: true,
        ..default()
    };

    let barrel_model = bake_barrel_model(&mut materials, &mut meshes);
    let pad_base_model = bake_pad_base(&mut materials, &mut meshes);
    let assets = GameAssets {
        wall: rust.wall,
        wall_tall: rust.wall_tall,
        floor: rust.floor,
        regolith: rust.regolith,
        catwalk: rust.catwalk,
        ceiling: rust.ceiling,
        lava: rust.lava,
        jammer: materials.add(textured(tex_jammer.clone(), 0.5, 0.0)),
        jammer_core: materials.add(jammer_core),
        tower_panel: materials.add(tower_panel),
        beacon: materials.add(beacon),
        lamp: rust.lamp,
        tex_wall,
        tex_jammer,
        tex_tower,
        beacon_mesh: meshes.add(Sphere::new(0.55)),
        jammer_core_mesh: meshes.add(Sphere::new(0.6)),
        sphere_small: meshes.add(Sphere::new(0.2)),
        cube_small: meshes.add(Cuboid::new(0.3, 0.3, 0.3)),
        themes,
        barrel_model,
        pad_base_model,
        pad_glow_mesh: meshes.add(bake_pad_glow()),
        pad_powered: materials.add(pad_powered),
        pad_dormant: materials.add(pad_dormant),
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

    #[test]
    fn five_floor_themes_are_distinct_and_endless_floors_wrap() {
        let names: Vec<&str> = FloorTheme::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["RUST BUNKER", "TOXIC COOLANT", "CRYO RELAY", "MAGMA FOUNDRY", "OBSIDIAN CORE"]);
        for (i, a) in FloorTheme::ALL.iter().enumerate() {
            assert_eq!(a.index(), i);
            assert_eq!(FloorTheme::for_floor(i as u32 + 1), *a);
            assert_eq!(FloorTheme::for_floor(i as u32 + 6), *a, "floor {} wraps", i + 6);
            for b in FloorTheme::ALL.iter().skip(i + 1) {
                let (sa, sb) = (a.spec(), b.spec());
                assert_ne!(sa.sky_color, sb.sky_color);
                assert_ne!(sa.fog_color, sb.fog_color);
                assert_ne!(sa.lamp_warm, sb.lamp_warm);
                assert_ne!(sa.hazard_light_color, sb.hazard_light_color);
                assert_ne!(sa.pit_ramp, sb.pit_ramp);
                assert_ne!(themed_wall_texture(&sa).data, themed_wall_texture(&sb).data);
                assert_ne!(themed_pit_texture(&sa).data, themed_pit_texture(&sb).data);
            }
        }
        assert_eq!(FloorTheme::for_floor(0), FloorTheme::RustBunker);
        assert_eq!(FloorTheme::RustBunker.sky_color(), Color::srgb(0.32, 0.04, 0.03));
    }

    #[test]
    fn themed_textures_are_opaque_deterministic_and_fog_stays_dark() {
        for t in FloorTheme::ALL {
            let spec = t.spec();
            for make in [themed_wall_texture, themed_catwalk_texture, themed_pit_texture, themed_ceiling_texture] {
                let a = make(&spec);
                assert_eq!(a.data.len(), 64 * 64 * 4);
                assert_eq!(a.data, make(&spec).data);
                assert!(a.data.chunks(4).all(|p| p[3] == 255));
            }
            // Dark, moody fog/sky keep enemy silhouettes and projectiles readable.
            let fog = spec.fog_color.to_srgba();
            assert!(fog.red.max(fog.green).max(fog.blue) < 0.25, "{t:?} fog too bright");
            let sky = spec.sky_color.to_srgba();
            assert!(sky.red.max(sky.green).max(sky.blue) < 0.45, "{t:?} sky too bright");
        }
    }
}
