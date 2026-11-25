//! Gritty industrial-punk UI kit: a battered UAC Mars-bunker field-terminal
//! look shared by every menu and the in-game HUD.
//!
//! Rules of the look:
//! - **Zero rounded corners.** Every panel, card, button and tag is a hard
//!   rectangle (`BorderRadius::ZERO`, Bevy's default).
//! - **Asymmetric bolted-plate borders.** A heavy colored *left rail*, a
//!   chunky dark *bottom bevel* and thin top/right edges, so boxes read as
//!   steel plates and physical switches.
//! - **Hard ink depth.** Interactive plates (buttons, cards, the terminal
//!   chassis) throw a crisp zero-blur offset shadow ([`hard_shadow`]); readouts
//!   sit in a [`recessed_bezel`] instead, so "pressable" and "inset" read apart.
//! - **Real hardware, not wallpaper.** The steel surface is one big brushed
//!   plate (no repeating rivet grid); rivets and L-brackets are bolted onto the
//!   outer chassis corners explicitly with [`corner_brackets`].
//! - **Stamped tape labels.** Section headers are solid `HAZARD_YELLOW` /
//!   `PUNK_RED` label-maker strips with `INK_BLACK` text.
//! - **Stencil titles.** Big titles sit on hard `+px` offset shadow copies with
//!   a slash bar underneath.
//! - **Punk hover snap.** Hovered buttons flip to a solid accent fill with
//!   black text.
//! - **Procedural grit.** Every texture ([`UiKitTextures`]) is generated at
//!   `PreStartup` (nearest-neighbor, no files on disk), so headless test apps
//!   get them too. Every helper takes `Option<&UiKitTextures>` and falls back
//!   to flat colors when the textures are missing.
//!
//! The default Bevy font is ASCII-only: labels use `-`, `|`, `/`, `>`, `[ ]`
//! and small colored `Node` squares, never Unicode bullets or arrows.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::widget::NodeImageMode;
use bevy::ui::VisualBox;

use crate::assets_gen::hash_noise;
use crate::hud::{text, MenuAction, MenuButton};
use crate::meta_ui::MetaCard;
use crate::meta_ui::MetaTarget;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Deep oil-soot black (tape text, bevels, hard shadows).
pub const INK_BLACK: Color = Color::srgb(0.04, 0.035, 0.032);
/// Rusted gunmetal plate (terminal chassis).
pub const IRON_DARK: Color = Color::srgb(0.085, 0.072, 0.066);
/// Worn steel card surface.
pub const IRON_MID: Color = Color::srgb(0.13, 0.11, 0.10);
/// Raw scrap-steel edge.
pub const STEEL_BORDER: Color = Color::srgb(0.32, 0.28, 0.25);
/// Bare bright steel (corner brackets, keycap lips).
pub const STEEL_LIGHT: Color = Color::srgb(0.50, 0.45, 0.40);
/// Warm distressed bone / paper white (never sterile pure white).
pub const BONE_TEXT: Color = Color::srgb(0.92, 0.87, 0.78);
/// Weathered muted ash.
pub const ASH_DIM: Color = Color::srgb(0.56, 0.49, 0.44);
/// Blood vermilion / spray red.
pub const PUNK_RED: Color = Color::srgb(1.0, 0.17, 0.10);
/// Dried-blood button / danger fill.
pub const BLOOD_DARK: Color = Color::srgb(0.24, 0.03, 0.025);
/// Industrial caution ochre.
pub const HAZARD_YELLOW: Color = Color::srgb(0.97, 0.68, 0.08);
/// Scorched amber fill.
pub const HAZARD_DARK: Color = Color::srgb(0.22, 0.14, 0.02);
/// Raw CRT phosphor / hacked-signal green.
pub const ACID_GREEN: Color = Color::srgb(0.25, 1.0, 0.45);
/// Dark phosphor fill.
pub const ACID_DARK: Color = Color::srgb(0.03, 0.18, 0.08);
/// High-voltage signal highlight (use sparingly).
pub const ARC_CYAN: Color = Color::srgb(0.20, 0.92, 1.0);
/// Dead rusted slot (locked cards).
pub const LOCKED_IRON: Color = Color::srgb(0.055, 0.04, 0.04);

/// Asymmetric plate border widths `(left, right, top, bottom)` of cards.
pub const CARD_BORDER: (f32, f32, f32, f32) = (4.0, 1.0, 1.0, 3.0);
/// Asymmetric border widths of the terminal chassis.
pub const CHASSIS_BORDER: (f32, f32, f32, f32) = (5.0, 2.0, 2.0, 5.0);

/// `UiRect` from `(left, right, top, bottom)` pixel widths.
pub fn plate_rect((l, r, t, b): (f32, f32, f32, f32)) -> UiRect {
    UiRect { left: Val::Px(l), right: Val::Px(r), top: Val::Px(t), bottom: Val::Px(b) }
}

/// Bolted-plate border colors: colored left rail, `edge` top/right and a dark
/// bottom bevel.
pub fn plate_border(rail: Color, edge: Color) -> BorderColor {
    BorderColor { left: rail, top: edge, right: edge, bottom: INK_BLACK }
}

/// Recessed-slot border colors: shadowed top/left lips and a faint lit
/// bottom/right lip, the inverse of a raised plate.
pub fn inset_border() -> BorderColor {
    let lit = STEEL_BORDER.with_alpha(0.55);
    BorderColor { left: INK_BLACK, top: INK_BLACK, right: lit, bottom: lit }
}

/// Crisp zero-blur offset shadow: the punk-zine "ink slab" a plate sits on.
pub fn hard_shadow(offset: f32) -> BoxShadow {
    BoxShadow::new(Color::srgba(0.0, 0.0, 0.0, 0.78), Val::Px(offset), Val::Px(offset), Val::Px(0.0), Val::Px(0.0))
}

/// Relative luminance (sRGB -> linear, Rec. 709 weights).
pub fn luminance(c: Color) -> f32 {
    let l = c.to_linear();
    0.2126 * l.red + 0.7152 * l.green + 0.0722 * l.blue
}

/// WCAG-style contrast ratio between two colors (1.0 ..= 21.0).
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

// ---------------------------------------------------------------------------
// Procedural textures
// ---------------------------------------------------------------------------

/// Plain RGBA8 pixel buffer (always opaque) used to build the kit textures.
#[derive(Debug, Clone)]
pub struct PixelBuf {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl PixelBuf {
    pub fn new(w: u32, h: u32) -> Self {
        PixelBuf { w, h, data: vec![255; (w * h * 4) as usize] }
    }

    pub fn set(&mut self, x: u32, y: u32, c: [u8; 3]) {
        let i = ((y * self.w + x) * 4) as usize;
        self.data[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
    }

    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.w + x) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn fill_with(&mut self, mut f: impl FnMut(u32, u32) -> [u8; 3]) {
        for y in 0..self.h {
            for x in 0..self.w {
                let c = f(x, y);
                self.set(x, y, c);
            }
        }
    }

    /// Crisp nearest-neighbor, repeat-addressed GPU image.
    pub fn into_image(self) -> Image {
        let mut img = Image::new(
            Extent3d { width: self.w, height: self.h, depth_or_array_layers: 1 },
            TextureDimension::D2,
            self.data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        );
        img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::nearest()
        });
        img
    }
}

fn shade(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).clamp(0.0, 255.0) as u8)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

/// Lattice period that never wraps (for textures that are stretched, not tiled).
const NO_WRAP: (u32, u32) = (u32::MAX, u32::MAX);

/// Bilinear value noise in `[0, 1)` on a `cell`-pixel lattice. The lattice
/// wraps every `period` cells per axis, so a texture `period * cell` pixels
/// wide tiles seamlessly.
fn value_noise(x: u32, y: u32, cell: u32, period: (u32, u32), salt: u32) -> f32 {
    let (gx, gy) = (x / cell, y / cell);
    let (fx, fy) = ((x % cell) as f32 / cell as f32, (y % cell) as f32 / cell as f32);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let n = |i: u32, j: u32| hash_noise((gx + i) % period.0, (gy + j) % period.1, salt);
    let top = n(0, 0) + (n(1, 0) - n(0, 0)) * sx;
    let bot = n(0, 1) + (n(1, 1) - n(0, 1)) * sx;
    top + (bot - top) * sy
}

/// Which stripes a tape strip carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tape {
    /// Ochre / oil-black caution stripes.
    Caution,
    /// Crimson / soot-black danger stripes (dead zones, signed contracts).
    Danger,
    /// Acid-phosphor / black uplink stripes (FULL SIGNAL).
    Signal,
}

impl Tape {
    pub const ALL: [Tape; 3] = [Tape::Caution, Tape::Danger, Tape::Signal];

    /// Flat accent of the tape (rails, texture-less fallback).
    pub fn accent(self) -> Color {
        match self {
            Tape::Caution => HAZARD_YELLOW,
            Tape::Danger => PUNK_RED,
            Tape::Signal => ACID_GREEN,
        }
    }
}

/// `true` is danger tape, `false` plain caution tape.
impl From<bool> for Tape {
    fn from(danger: bool) -> Self {
        if danger { Tape::Danger } else { Tape::Caution }
    }
}

/// Width of every tape texture.
pub const TAPE_W: u32 = 192;
/// Stripe half-period (pixels along `x + y`); `TAPE_W` is a multiple of
/// `2 * TAPE_STRIPE`, so the tape tiles seamlessly along x.
pub const TAPE_STRIPE: u32 = 6;
/// Strip heights a tape is authored at. Each one is drawn 1:1 (never scaled),
/// which keeps the 45 degree stripes and the paint chips pixel-crisp.
pub const TAPE_HEIGHTS: [u32; 6] = [6, 8, 10, 12, 14, 16];

/// Whether pixel `(x, y)` of a caution tape is on a bright stripe.
pub fn tape_band(x: u32, y: u32) -> bool {
    ((x + y) / TAPE_STRIPE).is_multiple_of(2)
}

/// `192 x h` diagonal caution tape with flaked paint (2-4px chips showing the
/// primer, denser along the worn top / bottom edges), hairline scuffs,
/// overspray specks on the black stripes and a dark cut line on both edges.
pub fn gen_tape(tape: Tape, h: u32) -> PixelBuf {
    let (bright, dark, salt): ([u8; 3], [u8; 3], u32) = match tape {
        Tape::Caution => ([238, 168, 22], [18, 15, 12], 37),
        Tape::Danger => ([214, 30, 18], [20, 11, 10], 71),
        Tape::Signal => ([56, 236, 110], [8, 20, 12], 113),
    };
    // Primer / bare steel under the chipped paint.
    let primer: [u8; 3] = [58, 51, 45];
    let mut b = PixelBuf::new(TAPE_W, h);
    b.fill_with(|x, y| {
        let band = tape_band(x, y);
        let worn = if y <= 1 || y + 2 >= h { 0.1 } else { 0.0 };
        let flake = value_noise(x, y, 3, (TAPE_W / 3, u32::MAX), salt) * 0.72 + hash_noise(x / 2, y / 2, salt + 2) * 0.28 + worn;
        let mut c = if band { bright } else { dark };
        if band {
            if flake > 0.8 {
                c = primer;
            } else if flake > 0.75 {
                // Lifted rim around a chip.
                c = shade(bright, 0.7);
            } else if hash_noise(x / 9, y, salt + 3) > 0.94 {
                // Hairline scuff.
                c = shade(bright, 0.82);
            }
        } else if hash_noise(x, y, salt + 4) > 0.975 {
            // Overspray specks on the black stripe.
            c = shade(bright, 0.4);
        }
        c = shade(c, 0.93 + 0.09 * hash_noise(x, y, salt + 1));
        if y == 0 {
            c = shade(c, 0.72);
        } else if y == h - 1 {
            c = shade(c, 0.5);
        }
        c
    });
    b
}

pub const GRUNGE_W: u32 = 640;
pub const GRUNGE_H: u32 = 360;
/// Registration tick inset `(x, y)` from each backdrop corner, in texels.
pub const GRUNGE_TICK_INSET: (u32, u32) = (20, 30);

/// `640x360` scorched CRT bunker backdrop (2x at 720p): deep vignette,
/// concrete / soot blotches, burn marks, red-oxide corner grime, faint
/// horizontal RF interference bands with torn scan rows, grit specks, 2px
/// scanlines and crisp corner registration ticks + edge rulers.
pub fn gen_grunge_bg() -> PixelBuf {
    let base: [u8; 3] = [40, 27, 22];
    let oxide: [u8; 3] = [96, 26, 12];
    let soot: [u8; 3] = [9, 7, 7];
    let ink: [u8; 3] = [138, 112, 90];
    let (w, h) = (GRUNGE_W, GRUNGE_H);
    let (ix, iy) = GRUNGE_TICK_INSET;
    let mut b = PixelBuf::new(w, h);
    b.fill_with(|x, y| {
        let u = x as f32 / (w - 1) as f32 - 0.5;
        let v = (y as f32 / (h - 1) as f32 - 0.5) * (h as f32 / w as f32);
        let r = (u * u + v * v).sqrt() / 0.574;
        let vign = (1.0 - 0.9 * r.powf(1.6)).clamp(0.07, 1.0);
        let blot = value_noise(x, y, 40, NO_WRAP, 5) * 0.5 + value_noise(x, y, 13, NO_WRAP, 9) * 0.3 + value_noise(x, y, 4, NO_WRAP, 10) * 0.2;
        let grain = 0.84 + 0.28 * hash_noise(x, y, 3);
        let mut c = shade(base, vign * (0.6 + 0.66 * blot) * grain);
        // Burn marks: soft soot blooms.
        let scorch = value_noise(x, y, 56, NO_WRAP, 31);
        if scorch > 0.64 {
            c = mix(c, soot, ((scorch - 0.64) / 0.2).min(1.0) * 0.7);
        }
        // Red-oxide grime creeping in from the corners.
        let corner = (u.abs() * 2.0).max(v.abs() * 3.55).powf(3.0) * value_noise(x, y, 24, NO_WRAP, 21);
        c = mix(c, shade(oxide, vign.max(0.4)), corner * 0.85);
        // RF interference: a few soft horizontal bands with streaky gain.
        let band = value_noise(0, y, 18, NO_WRAP, 61);
        if band > 0.7 {
            let k = (band - 0.7) / 0.3;
            let streak = hash_noise(x / 6, y / 2, 62);
            c = shade(c, 1.0 + k * (0.3 + 0.55 * streak));
            c = mix(c, [120, 40, 24], k * 0.12 * streak);
        }
        // Torn scan rows: sparse dashed bright lines.
        if hash_noise(0, y / 2, 63) > 0.985 && hash_noise(x / 24, y / 2, 64) > 0.45 {
            c = mix(c, [150, 110, 90], 0.2 * vign.max(0.3));
        }
        // Grit: pale concrete specks and dark pits.
        if hash_noise(x, y, 17) > 0.9975 {
            c = mix(c, [112, 94, 80], 0.42);
        } else if hash_noise(x, y, 18) > 0.996 {
            c = shade(c, 0.45);
        }
        // Registration: an L tick in every corner, a cross on both side
        // centers and a ruler down each side edge.
        let (ex, ey) = (x.min(w - 1 - x), y.min(h - 1 - y));
        let corner_tick = (ex == ix && (iy..iy + 13).contains(&ey)) || (ey == iy && (ix..ix + 13).contains(&ex));
        let (cy, cdx) = (y.abs_diff(h / 2), ex.abs_diff(ix));
        let side_cross = (cdx == 0 && cy <= 4 && cy != 0) || (cy == 0 && cdx <= 4 && cdx != 0);
        let ruler = (60..h - 60).contains(&y) && y % 10 == 0 && ex >= 5 && ex < if y % 50 == 0 { 12 } else { 8 };
        if corner_tick || side_cross {
            c = mix(c, ink, 0.7);
        } else if ruler {
            c = mix(c, ink, 0.38);
        }
        // Scanlines every 2px.
        if y % 2 == 1 {
            c = shade(c, 0.76);
        }
        c
    });
    b
}

pub const PLATE_SIZE: u32 = 256;
/// Hairline panel seams of the plate: one horizontal row, one vertical column.
pub const PLATE_SEAM: (u32, u32) = (201, 151);

/// `256x256` weathered brushed steel, seamless in both axes: broad heat / oil
/// staining, fine horizontal brushing, short diagonal micro-scratches, rust
/// blooms, tiny pits and one hairline seam per axis. No rivets: hardware is
/// bolted on explicitly with [`corner_brackets`].
pub fn gen_plate_bg() -> PixelBuf {
    let metal: [u8; 3] = [37, 33, 31];
    let rust: [u8; 3] = [86, 38, 20];
    let oil: [u8; 3] = [13, 13, 15];
    let s = PLATE_SIZE;
    let (seam_x, seam_y) = PLATE_SEAM;
    let mut b = PixelBuf::new(s, s);
    b.fill_with(|x, y| {
        // Broad tonal drift + brushing (4px runs along x) + dither.
        let stain = value_noise(x, y, 64, (4, 4), 51) * 0.6 + value_noise(x, y, 32, (8, 8), 52) * 0.4;
        let mut c = shade(metal, (0.8 + 0.36 * stain) * (0.94 + 0.12 * hash_noise(x / 4, y, 53)) * (0.97 + 0.06 * hash_noise(x, y, 54)));
        // Diagonal micro-scratches: 8-32px segments along x - y diagonals.
        let diag = (x + s - y) % s;
        let along = (x + y) % s;
        let len = [16, 32, 64][(hash_noise(diag, 0, 55) * 3.0) as usize % 3];
        let pick = hash_noise(diag, along / len, 56);
        if pick > 0.957 {
            c = shade(c, 1.5);
        } else if pick < 0.03 {
            c = shade(c, 0.62);
        }
        // Rust blooms and oil stains.
        let bloom = value_noise(x, y, 32, (8, 8), 57) * 0.55 + value_noise(x, y, 8, (32, 32), 58) * 0.45;
        if bloom > 0.6 {
            c = mix(c, rust, ((bloom - 0.6) / 0.22).min(1.0) * 0.5);
        }
        let slick = value_noise(x, y, 64, (4, 4), 59);
        if slick < 0.28 {
            c = mix(c, oil, (0.28 - slick) / 0.28 * 0.6);
        }
        // Pits: a dark pixel with a lit lower-right lip.
        if hash_noise(x, y, 60) > 0.9985 {
            c = shade(c, 0.4);
        } else if hash_noise((x + s - 1) % s, (y + s - 1) % s, 60) > 0.9985 {
            c = shade(c, 1.5);
        }
        // Hairline seams: a dark cut with a lit lip.
        if x == seam_x || y == seam_y {
            c = shade(metal, 0.52);
        } else if x == seam_x + 1 || y == seam_y + 1 {
            c = shade(c, 1.16);
        }
        c
    });
    b
}

pub const NOISE_SIZE: u32 = 64;

/// `64x64` photocopier / RF static grain with a few smeared scan rows.
pub fn gen_noise_strip() -> PixelBuf {
    let mut b = PixelBuf::new(NOISE_SIZE, NOISE_SIZE);
    b.fill_with(|x, y| {
        let row = hash_noise(0, y, 41);
        let mut v = 70.0 + 140.0 * hash_noise(x, y, 43);
        if row > 0.86 {
            v = v * 0.5 + 120.0;
        }
        if y % 2 == 1 {
            v *= 0.78;
        }
        let v = v.clamp(0.0, 255.0) as u8;
        [v, (v as f32 * 0.94) as u8, (v as f32 * 0.88) as u8]
    });
    b
}

/// Handles of the procedural UI textures (created at `PreStartup`).
#[derive(Resource, Debug, Clone)]
pub struct UiKitTextures {
    /// `192 x h` tapes indexed by [`Tape`] then by [`TAPE_HEIGHTS`] (tile
    /// along x); pick one with [`UiKitTextures::tape`].
    tapes: [[Handle<Image>; TAPE_HEIGHTS.len()]; Tape::ALL.len()],
    /// `640x360` CRT bunker backdrop (stretch full-screen).
    pub grunge_bg: Handle<Image>,
    /// `256x256` weathered brushed steel (tile behind panels).
    pub plate_bg: Handle<Image>,
    /// `64x64` RF static grain (tile, low alpha, on card surfaces).
    pub noise_strip: Handle<Image>,
}

pub struct UiKitPlugin;

impl Plugin for UiKitPlugin {
    fn build(&self, app: &mut App) {
        // The initial `OnEnter(MainMenu)` runs before `PreStartup` commands are
        // applied, so insert the textures right away when the image store
        // already exists; `PreStartup` is the fallback for late asset setup.
        if let Some(mut images) = app.world_mut().get_resource_mut::<Assets<Image>>() {
            let tex = UiKitTextures::create(&mut images);
            app.insert_resource(tex);
        }
        app.add_systems(PreStartup, init_ui_kit_textures.run_if(not(resource_exists::<UiKitTextures>)));
    }
}

impl UiKitTextures {
    /// Generate every kit texture into `images`.
    pub fn create(images: &mut Assets<Image>) -> Self {
        UiKitTextures {
            tapes: Tape::ALL.map(|t| TAPE_HEIGHTS.map(|h| images.add(gen_tape(t, h).into_image()))),
            grunge_bg: images.add(gen_grunge_bg().into_image()),
            plate_bg: images.add(gen_plate_bg().into_image()),
            noise_strip: images.add(gen_noise_strip().into_image()),
        }
    }

    /// The tape texture authored closest to a `height`-px strip, and the
    /// height it was authored at.
    pub fn tape(&self, tape: Tape, height: f32) -> (Handle<Image>, u32) {
        let i = (0..TAPE_HEIGHTS.len()).min_by(|a, b| (TAPE_HEIGHTS[*a] as f32 - height).abs().total_cmp(&(TAPE_HEIGHTS[*b] as f32 - height).abs())).unwrap_or(0);
        let t = Tape::ALL.iter().position(|k| *k == tape).unwrap_or(0);
        (self.tapes[t][i].clone(), TAPE_HEIGHTS[i])
    }
}

pub fn init_ui_kit_textures(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    commands.insert_resource(UiKitTextures::create(&mut images));
}

/// Tiled, tinted background image filling a node's padding box.
pub fn tiled_image(handle: Handle<Image>, tint: Color, scale: f32) -> ImageNode {
    ImageNode {
        color: tint,
        image_mode: NodeImageMode::Tiled { tile_x: true, tile_y: true, stretch_value: scale },
        visual_box: VisualBox::PaddingBox,
        ..ImageNode::new(handle)
    }
}

/// Tape image tiled along x only. Strips at one of the [`TAPE_HEIGHTS`] draw
/// pixel-perfect; any other height scales the closest authored tape.
pub fn tape_image(tex: &UiKitTextures, height: f32, tape: impl Into<Tape>) -> ImageNode {
    let (handle, authored) = tex.tape(tape.into(), height);
    ImageNode {
        image_mode: NodeImageMode::Tiled { tile_x: true, tile_y: false, stretch_value: (height / authored as f32).max(0.25) },
        visual_box: VisualBox::PaddingBox,
        ..ImageNode::new(handle)
    }
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// Visual family of a [`punk_button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The one call to action of a screen: a solid phosphor slab with ink
    /// text, already "lit" while idle.
    HeroAcid,
    /// Go / deploy / resume: phosphor green.
    PrimaryAcid,
    /// Destructive / exit: blood red.
    DangerRed,
    /// Secondary caution action: ochre.
    HazardAmber,
    /// Neutral: raw scrap steel.
    ScrapSteel,
}

/// Idle / hovered colors of a punk button. Hover inverts the text (black on a
/// solid accent fill).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct PunkButtonColors {
    pub idle_bg: Color,
    pub hover_bg: Color,
    pub idle_border: Color,
    pub hover_border: Color,
    pub idle_text: Color,
    pub hover_text: Color,
    /// Heavy left accent rail while idle.
    pub rail: Color,
}

impl PunkButtonColors {
    pub fn of(style: ButtonStyle) -> Self {
        if style == ButtonStyle::HeroAcid {
            // Solid while idle; hover flashes to hot bone.
            return PunkButtonColors {
                idle_bg: ACID_GREEN,
                hover_bg: BONE_TEXT,
                idle_border: ACID_GREEN.mix(&BONE_TEXT, 0.55),
                hover_border: ACID_GREEN,
                idle_text: INK_BLACK,
                hover_text: INK_BLACK,
                rail: BONE_TEXT,
            };
        }
        let (fill, accent, text_idle) = match style {
            ButtonStyle::DangerRed => (BLOOD_DARK, PUNK_RED, BONE_TEXT),
            ButtonStyle::HazardAmber => (HAZARD_DARK, HAZARD_YELLOW, HAZARD_YELLOW),
            ButtonStyle::ScrapSteel => (IRON_MID, BONE_TEXT, BONE_TEXT),
            ButtonStyle::PrimaryAcid | ButtonStyle::HeroAcid => (ACID_DARK, ACID_GREEN, ACID_GREEN),
        };
        PunkButtonColors {
            idle_bg: fill,
            hover_bg: accent,
            idle_border: STEEL_BORDER,
            hover_border: BONE_TEXT,
            idle_text: text_idle,
            hover_text: INK_BLACK,
            rail: if style == ButtonStyle::ScrapSteel { ASH_DIM } else { accent },
        }
    }

    pub fn bg(&self, hovered: bool) -> Color {
        if hovered { self.hover_bg } else { self.idle_bg }
    }

    pub fn text(&self, hovered: bool) -> Color {
        if hovered { self.hover_text } else { self.idle_text }
    }

    /// Hover: razor-bright frame; idle: accent rail over thin steel edges.
    pub fn border(&self, hovered: bool) -> BorderColor {
        if hovered { plate_border(self.hover_border, self.hover_border) } else { plate_border(self.rail, self.idle_border) }
    }
}

/// Angular industrial switch: heavy left rail + bottom bevel on a hard ink
/// shadow, punk hover snap. Carries [`MenuButton`] + the [`MenuAction`] so
/// `hud::menu_buttons` hit-tests and restyles it.
pub fn punk_button(label: &str, action: MenuAction, font: f32, style: ButtonStyle) -> impl Bundle {
    let colors = PunkButtonColors::of(style);
    let hero = style == ButtonStyle::HeroAcid;
    (
        MenuButton,
        crate::gamepad::GamepadFocusable,
        action,
        colors,
        Node {
            padding: UiRect::axes(Val::Px(font * if hero { 1.3 } else { 1.0 }), Val::Px(font * 0.4)),
            border: plate_rect(if hero { (8.0, 2.0, 2.0, 5.0) } else { (6.0, 1.0, 1.0, 4.0) }),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        colors.border(false),
        BackgroundColor(colors.idle_bg),
        hard_shadow(if hero { 7.0 } else { 4.0 }),
        children![text(label, font, colors.idle_text)],
    )
}

// ---------------------------------------------------------------------------
// Tags, plates, strips
// ---------------------------------------------------------------------------

/// Crisp stamped metal / tape tag (`[SIG]`, `[DONE] ...`, `JAM DEMO`).
pub fn stamped_tag(label: &str, bg: Color, fg: Color, font: f32) -> impl Bundle {
    (
        Node {
            padding: UiRect::axes(Val::Px((font * 0.5).round()), Val::Px((font * 0.14).round())),
            border: UiRect::bottom(Val::Px(2.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            flex_shrink: 0.0,
            ..default()
        },
        BorderColor::all(INK_BLACK.with_alpha(0.85)),
        BackgroundColor(bg),
        children![text(label, font, fg)],
    )
}

/// Keyboard keycap chip: a lit top lip, a heavy bottom lip and bone legend.
pub fn keycap(label: &str, font: f32) -> impl Bundle {
    (
        Node {
            padding: UiRect::axes(Val::Px((font * 0.45).round()), Val::Px(0.0)),
            border: plate_rect((1.0, 1.0, 1.0, 3.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            flex_shrink: 0.0,
            ..default()
        },
        BorderColor { left: STEEL_BORDER, top: STEEL_LIGHT, right: STEEL_BORDER, bottom: INK_BLACK },
        BackgroundColor(Color::srgb(0.2, 0.175, 0.155)),
        children![text(label, font, BONE_TEXT)],
    )
}

/// Angular HUD plate: colored left rail, dark translucent iron, thin steel edge.
pub fn hud_plate(node: Node, rail_color: Color) -> impl Bundle {
    (
        Node { padding: UiRect::axes(Val::Px(12.0), Val::Px(7.0)), border: plate_rect((4.0, 1.0, 1.0, 2.0)), ..node },
        plate_border(rail_color, STEEL_BORDER.with_alpha(0.45)),
        BackgroundColor(IRON_DARK.with_alpha(0.8)),
    )
}

/// Recessed readout slot: ink well with shadowed top / left lips (the inverse
/// of a raised plate), for stats and telemetry that are read, not pressed.
pub fn recessed_bezel(node: Node) -> impl Bundle {
    (Node { border: UiRect::all(Val::Px(2.0)), ..node }, inset_border(), BackgroundColor(INK_BLACK.with_alpha(0.72)))
}

/// Tiled tape bar spanning the parent (`false` caution / `true` danger, or any
/// [`Tape`]).
pub fn hazard_strip(commands: &mut Commands, parent: Entity, tex: Option<&UiKitTextures>, height: f32, tape: impl Into<Tape>) -> Entity {
    let tape = tape.into();
    let e = commands
        .spawn((
            ChildOf(parent),
            Node { width: Val::Percent(100.0), height: Val::Px(height), flex_shrink: 0.0, ..default() },
            BackgroundColor(if tex.is_some() { INK_BLACK } else { tape.accent() }),
        ))
        .id();
    if let Some(t) = tex {
        commands.entity(e).insert(tape_image(t, height, tape));
    }
    e
}

/// Full-width tape rails pinned to the top and bottom edges of a screen root.
pub fn screen_rails(commands: &mut Commands, root: Entity, tex: Option<&UiKitTextures>, height: f32, tape: impl Into<Tape>) {
    let tape = tape.into();
    for top in [true, false] {
        let e = hazard_strip(commands, root, tex, height, tape);
        commands.entity(e).insert(Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: if top { Val::Px(0.0) } else { Val::Auto },
            bottom: if top { Val::Auto } else { Val::Px(0.0) },
            width: Val::Percent(100.0),
            height: Val::Px(height),
            ..default()
        });
    }
}

/// Bolt an L-bracket with a rivet onto each outer corner of `parent`.
///
/// Brackets are absolutely positioned (they never disturb the parent's
/// layout) and must be spawned after the parent's other children so they draw
/// on top. `outset` is the parent's `(left, right, top, bottom)` border width,
/// which pushes each bracket out flush with the border-box corner.
pub fn corner_brackets(commands: &mut Commands, parent: Entity, color: Color, arm: f32, thick: f32, outset: (f32, f32, f32, f32)) {
    let (l, r, t, b) = outset;
    for (is_left, is_top) in [(true, true), (false, true), (true, false), (false, false)] {
        let at = |dx: f32, dy: f32, w: f32, h: f32| Node {
            position_type: PositionType::Absolute,
            left: if is_left { Val::Px(dx - l) } else { Val::Auto },
            right: if is_left { Val::Auto } else { Val::Px(dx - r) },
            top: if is_top { Val::Px(dy - t) } else { Val::Auto },
            bottom: if is_top { Val::Auto } else { Val::Px(dy - b) },
            width: Val::Px(w),
            height: Val::Px(h),
            ..default()
        };
        // An L is a box with only its two outer borders painted. The ink copy
        // is 1px fatter, which outlines the steel one on its inner edges.
        let ell = |grow: f32| {
            let w = Val::Px(thick + grow);
            Node {
                border: UiRect {
                    left: if is_left { w } else { Val::ZERO },
                    right: if is_left { Val::ZERO } else { w },
                    top: if is_top { w } else { Val::ZERO },
                    bottom: if is_top { Val::ZERO } else { w },
                },
                ..at(0.0, 0.0, arm + grow, arm + grow)
            }
        };
        commands.spawn((ChildOf(parent), CornerBracket, ell(1.0), BorderColor::all(INK_BLACK)));
        commands.spawn((ChildOf(parent), CornerBracket, ell(0.0), BorderColor::all(color)));
        // Rivet head sunk into the elbow.
        let inset = ((thick - 3.0) / 2.0).max(0.0).floor();
        commands.spawn((ChildOf(parent), CornerBracket, at(inset, inset, 3.0, 3.0), BackgroundColor(INK_BLACK)));
    }
}

/// A piece of a [`corner_brackets`] L-bracket (two L plates + a rivet per corner).
#[derive(Component, Debug, Clone, Copy)]
pub struct CornerBracket;

/// Five ascending signal bars: the first `lit` are solid `color`, the rest are
/// hollow dead slots. `unit` is the bar width (the tallest bar is `4 * unit`).
pub fn signal_bars(commands: &mut Commands, parent: Entity, lit: u8, color: Color, unit: f32) -> Entity {
    let row = commands
        .spawn((ChildOf(parent), Node { align_items: AlignItems::FlexEnd, column_gap: Val::Px((unit * 0.5).max(2.0)), flex_shrink: 0.0, ..default() }))
        .id();
    for i in 0..5u8 {
        let on = i < lit;
        commands.spawn((
            ChildOf(row),
            Node { width: Val::Px(unit), height: Val::Px(unit * (0.8 + 0.8 * i as f32)), border: UiRect::all(Val::Px(if unit >= 8.0 { 2.0 } else { 1.0 })), ..default() },
            BorderColor::all(if on { color } else { color.with_alpha(0.45) }),
            BackgroundColor(if on { color } else { INK_BLACK.with_alpha(0.6) }),
        ));
    }
    row
}

/// Full-screen menu container: flat `bg_tint` + the grunge CRT backdrop at
/// `overlay_alpha` (flat only without textures), plus faint bunker telemetry
/// stamps in the top corners. Children stack in a centered column.
pub fn punk_screen_root(commands: &mut Commands, state: AppState, tex: Option<&UiKitTextures>, bg_tint: Color, overlay_alpha: f32) -> Entity {
    punk_screen_root_tinted(commands, state, tex, bg_tint, Color::WHITE.with_alpha(overlay_alpha), ("RF-LINK: UNSTABLE", PUNK_RED))
}

/// [`punk_screen_root`] with the backdrop multiplied by `backdrop` (its alpha
/// is the overlay strength): crimson for dead zones, phosphor for uplinks.
/// `link` is the top-right RF stamp `(label, lamp color)`.
pub fn punk_screen_root_tinted(
    commands: &mut Commands,
    state: AppState,
    tex: Option<&UiKitTextures>,
    bg_tint: Color,
    backdrop: Color,
    link: (&str, Color),
) -> Entity {
    let root = commands
        .spawn((
            DespawnOnExit(state),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(10.0),
                ..default()
            },
            BackgroundColor(bg_tint),
        ))
        .id();
    if let Some(t) = tex
        && backdrop.alpha() > 0.0
    {
        commands.entity(root).insert(ImageNode {
            color: backdrop,
            image_mode: NodeImageMode::Stretch,
            visual_box: VisualBox::PaddingBox,
            ..ImageNode::new(t.grunge_bg.clone())
        });
    }
    for (left, label, lamp) in [(true, "UAC-B7 // SEC-07", HAZARD_YELLOW), (false, link.0, link.1)] {
        commands.spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(20.0),
                left: if left { Val::Px(18.0) } else { Val::Auto },
                right: if left { Val::Auto } else { Val::Px(18.0) },
                column_gap: Val::Px(6.0),
                align_items: AlignItems::Center,
                ..default()
            },
            children![
                (Node { width: Val::Px(6.0), height: Val::Px(6.0), ..default() }, BackgroundColor(lamp)),
                text(label, 11.0, ASH_DIM.with_alpha(0.8)),
            ],
        ));
    }
    root
}

/// Heavy-steel terminal box on a hard ink shadow: top + bottom tape strips
/// (`false` caution / `true` danger, or any [`Tape`]), asymmetric thick
/// borders, a brushed-steel surface, a recessed inner bezel line and riveted
/// L-brackets on the four outer corners. Returns the padded **body** entity to
/// fill with content.
pub fn terminal_chassis(commands: &mut Commands, parent: Entity, tex: Option<&UiKitTextures>, width: f32, tape: impl Into<Tape>) -> Entity {
    let tape = tape.into();
    let chassis = commands
        .spawn((
            ChildOf(parent),
            TerminalChassis,
            Node {
                width: Val::Px(width),
                max_width: Val::Percent(97.0),
                max_height: Val::Percent(98.0),
                flex_direction: FlexDirection::Column,
                border: plate_rect(CHASSIS_BORDER),
                ..default()
            },
            BorderColor { left: tape.accent(), top: STEEL_BORDER, right: STEEL_BORDER, bottom: INK_BLACK },
            BackgroundColor(IRON_DARK.with_alpha(0.97)),
            hard_shadow(9.0),
        ))
        .id();
    if let Some(t) = tex {
        commands.entity(chassis).insert(tiled_image(t.plate_bg.clone(), Color::WHITE.with_alpha(0.62), 2.0));
    }
    let top = hazard_strip(commands, chassis, tex, CHASSIS_TAPE.0, tape);
    commands.entity(top).insert(ChassisStrip(CHASSIS_TAPE.0));
    let body = commands
        .spawn((
            ChildOf(chassis),
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                margin: UiRect::all(Val::Px(4.0)),
                border: UiRect::all(Val::Px(1.0)),
                padding: UiRect::axes(Val::Px(13.0), Val::Px(8.0)),
                row_gap: Val::Px(9.0),
                flex_shrink: 1.0,
                ..default()
            },
            // Recessed bezel line just inside the chassis frame.
            BorderColor { left: INK_BLACK, top: INK_BLACK, right: STEEL_BORDER.with_alpha(0.4), bottom: STEEL_BORDER.with_alpha(0.4) },
        ))
        .id();
    let bottom = hazard_strip(commands, chassis, tex, CHASSIS_TAPE.1, tape);
    commands.entity(bottom).insert(ChassisStrip(CHASSIS_TAPE.1));
    corner_brackets(commands, chassis, STEEL_LIGHT, 20.0, 5.0, CHASSIS_BORDER);
    body
}

/// Heights of the `(top, bottom)` tape strips of a [`terminal_chassis`].
pub const CHASSIS_TAPE: (f32, f32) = (10.0, 6.0);

/// Root node of a [`terminal_chassis`] (its left rail tracks the stripe mode).
#[derive(Component, Debug, Clone, Copy)]
pub struct TerminalChassis;

/// Top / bottom tape strip of a [`terminal_chassis`] and its height in px
/// (swap with [`set_strip_tape`]).
#[derive(Component, Debug, Clone, Copy)]
pub struct ChassisStrip(pub f32);

/// Switch a `height`-px tape strip to another [`Tape`] in place.
pub fn set_strip_tape(image: &mut ImageNode, tex: &UiKitTextures, height: f32, tape: impl Into<Tape>) {
    let (want, _) = tex.tape(tape.into(), height);
    if image.image != want {
        image.image = want;
    }
}

/// Stacked stencil title: a hard `+offset` shadow copy behind the main text
/// and a thick slash bar underneath. Returns the wrapper entity.
pub fn stencil_title(commands: &mut Commands, parent: Entity, title: &str, size: f32, color: Color, shadow: Color) -> Entity {
    let off = (size * 0.05).round().clamp(2.0, 5.0);
    stencil_title_layers(commands, parent, title, size, color, slash_bar_color(color, shadow), &[(off, shadow)])
}

/// Slash bar under a stencil title: the shadow color when it reads on a dark
/// backdrop, else the title color.
pub fn slash_bar_color(title: Color, shadow: Color) -> Color {
    if luminance(shadow) > 0.02 { shadow } else { title }
}

/// Hero stencil lockup: the title over a `mid` slab copy over a deep ink slab,
/// so the wordmark stands two steps proud of the backdrop.
pub fn hero_title(commands: &mut Commands, parent: Entity, title: &str, size: f32, color: Color, mid: Color) -> Entity {
    let step = (size * 0.045).round().clamp(3.0, 5.0);
    stencil_title_layers(commands, parent, title, size, color, slash_bar_color(color, mid), &[(step * 2.0, Color::BLACK), (step, mid)])
}

/// A stencil title over any number of `(offset, color)` shadow copies, listed
/// back to front, with a `bar`-colored slash bar underneath.
pub fn stencil_title_layers(
    commands: &mut Commands,
    parent: Entity,
    title: &str,
    size: f32,
    color: Color,
    bar: Color,
    shadows: &[(f32, Color)],
) -> Entity {
    let deepest = shadows.iter().map(|(o, _)| *o).fold(0.0, f32::max);
    let wrap = commands
        .spawn((ChildOf(parent), Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Stretch, row_gap: Val::Px(2.0), ..default() }))
        .id();
    let stack = commands
        .spawn((ChildOf(wrap), Node { padding: UiRect { right: Val::Px(deepest), bottom: Val::Px(deepest), ..default() }, ..default() }))
        .id();
    for (off, shadow) in shadows {
        commands.spawn((
            ChildOf(stack),
            Node { position_type: PositionType::Absolute, left: Val::Px(*off), top: Val::Px(*off), ..default() },
            children![text(title, size, *shadow)],
        ));
    }
    commands.spawn((ChildOf(stack), text(title, size, color)));
    let bar_h = (size * 0.075).round().clamp(3.0, 7.0);
    let bar_row = commands
        .spawn((ChildOf(wrap), Node { width: Val::Percent(100.0), height: Val::Px(bar_h), column_gap: Val::Px(bar_h), ..default() }))
        .id();
    // Caution-yellow slash blocks at the end (red on yellow titles).
    let block = if color == HAZARD_YELLOW { PUNK_RED } else { HAZARD_YELLOW };
    commands.spawn((ChildOf(bar_row), Node { flex_grow: 1.0, ..default() }, BackgroundColor(bar)));
    for _ in 0..3 {
        commands.spawn((ChildOf(bar_row), Node { width: Val::Px(bar_h * 2.2), ..default() }, BackgroundColor(block)));
    }
    wrap
}

/// Angular steel section with a stamped label-maker tape header (`INK_BLACK`
/// text on solid `tape_color`). Returns `(section, header_row)`; append
/// counters / hints / toggles to the header row.
pub fn section_crate(commands: &mut Commands, parent: Entity, label: &str, tape_color: Color, node: Node) -> (Entity, Entity) {
    let sec = commands
        .spawn((
            ChildOf(parent),
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect { left: Val::Px(9.0), right: Val::Px(9.0), top: Val::Px(0.0), bottom: Val::Px(8.0) },
                row_gap: Val::Px(6.0),
                border: plate_rect((3.0, 1.0, 1.0, 3.0)),
                ..node
            },
            plate_border(tape_color.with_alpha(0.75), STEEL_BORDER.with_alpha(0.7)),
            BackgroundColor(INK_BLACK.with_alpha(0.62)),
        ))
        .id();
    let header = commands
        .spawn((
            ChildOf(sec),
            Node { width: Val::Percent(100.0), align_items: AlignItems::Center, column_gap: Val::Px(10.0), ..default() },
        ))
        .id();
    commands.spawn((
        ChildOf(header),
        Node {
            margin: UiRect::left(Val::Px(-9.0)),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)),
            border: UiRect::bottom(Val::Px(2.0)),
            flex_shrink: 0.0,
            ..default()
        },
        BorderColor::all(INK_BLACK),
        BackgroundColor(tape_color),
        children![text(format!("[ {label} ]"), 13.0, INK_BLACK)],
    ));
    (sec, header)
}

/// Angular clickable card (left 4 / bottom 3 / top 1 / right 1 framing) on a
/// hard ink shadow; its colors are restyled live by
/// `meta_ui::refresh_meta_ui`.
pub fn rivet_card(commands: &mut Commands, parent: Entity, target: MetaTarget, pad: UiRect, gap: f32) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            MetaCard(target),
            crate::gamepad::GamepadFocusable,
            Node { flex_direction: FlexDirection::Column, padding: pad, row_gap: Val::Px(gap), border: plate_rect(CARD_BORDER), ..default() },
            plate_border(STEEL_BORDER, STEEL_BORDER.with_alpha(0.6)),
            BackgroundColor(IRON_MID),
            hard_shadow(3.0),
        ))
        .id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn distinct(b: &PixelBuf) -> usize {
        (0..b.h).flat_map(|y| (0..b.w).map(move |x| (x, y))).map(|(x, y)| b.get(x, y)).collect::<HashSet<_>>().len()
    }

    fn all_opaque(b: &PixelBuf) -> bool {
        b.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255)
    }

    fn mean_luma(b: &PixelBuf) -> f32 {
        let n = (b.w * b.h) as f32;
        b.data.as_chunks::<4>().0.iter().map(|p| (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722) / 255.0).sum::<f32>() / n
    }

    fn sum(b: &PixelBuf, x: u32, y: u32) -> u32 {
        let p = b.get(x, y);
        p[0] as u32 + p[1] as u32 + p[2] as u32
    }

    #[test]
    fn textures_have_exact_sizes_opaque_alpha_and_texture() {
        for (b, w, h, min_colors) in [
            (gen_tape(Tape::Caution, 10), 192, 10, 20),
            (gen_tape(Tape::Danger, 16), 192, 16, 20),
            (gen_tape(Tape::Signal, 6), 192, 6, 20),
            (gen_grunge_bg(), 640, 360, 200),
            (gen_plate_bg(), 256, 256, 60),
            (gen_noise_strip(), 64, 64, 60),
        ] {
            assert_eq!((b.w, b.h), (w, h));
            assert_eq!(b.data.len(), (w * h * 4) as usize);
            assert!(all_opaque(&b), "{w}x{h} texture must be fully opaque");
            assert!(distinct(&b) >= min_colors, "{w}x{h} texture is too uniform: {} colors", distinct(&b));
        }
    }

    #[test]
    fn tape_stripes_are_diagonal_and_tile_seamlessly() {
        assert_eq!(TAPE_W % (2 * TAPE_STRIPE), 0);
        for y in 0..16 {
            for x in 0..TAPE_W {
                assert_eq!(tape_band(x, y), tape_band(x + TAPE_W, y), "seam at ({x},{y})");
            }
        }
        // Diagonal: a one-pixel step right is the same as one step down.
        assert_eq!(tape_band(3, 0), tape_band(2, 1));
        assert_ne!(tape_band(0, 0), tape_band(TAPE_STRIPE, 0));
        // Mean (r+g+b, green) of the bright and dark stripes, edge rows excluded.
        let stripes = |t: &PixelBuf| {
            let mut acc = [(0.0f32, 0.0f32, 0u32); 2];
            for y in 1..t.h - 1 {
                for x in 0..t.w {
                    let p = t.get(x, y);
                    let a = &mut acc[usize::from(tape_band(x, y))];
                    *a = (a.0 + p[0] as f32 + p[1] as f32 + p[2] as f32, a.1 + p[1] as f32, a.2 + 1);
                }
            }
            acc.map(|(l, g, n)| (l / n as f32, g / n as f32))
        };
        for tape in Tape::ALL {
            for h in TAPE_HEIGHTS {
                let [dark, bright] = stripes(&gen_tape(tape, h));
                // Chipped, but the paint still reads at every authored height.
                assert!(bright.0 > 4.0 * dark.0, "{tape:?} h={h}: {bright:?} vs {dark:?}");
            }
        }
        // Caution is ochre, danger crimson (green low), signal phosphor (green high).
        let green = |t| stripes(&gen_tape(t, 12))[1].1;
        assert!(green(Tape::Caution) > 120.0 && green(Tape::Danger) < 60.0 && green(Tape::Signal) > 180.0);
        assert_eq!((Tape::from(false), Tape::from(true)), (Tape::Caution, Tape::Danger));
        assert_eq!(Tape::Signal.accent(), ACID_GREEN);
    }

    #[test]
    fn tape_paint_chips_are_flakes_not_salt_and_pepper() {
        // A chip pixel shows primer on a bright stripe. Chips must exist, stay
        // a minority, and mostly touch another chip (flakes, not 1px noise).
        let t = gen_tape(Tape::Caution, 12);
        let chipped = |x: u32, y: u32| tape_band(x, y) && t.get(x, y)[0] < 90;
        let (mut chips, mut clustered, mut bright) = (0, 0, 0);
        for y in 1..t.h - 1 {
            for x in 1..t.w - 1 {
                bright += u32::from(tape_band(x, y));
                if chipped(x, y) {
                    chips += 1;
                    clustered += u32::from(chipped(x - 1, y) || chipped(x + 1, y) || chipped(x, y - 1) || chipped(x, y + 1));
                }
            }
        }
        assert!(chips > 20, "tape needs visible chips: {chips}");
        assert!(chips * 5 < bright, "tape is mostly paint: {chips} of {bright}");
        assert!(clustered * 2 > chips, "chips cluster into flakes: {clustered} of {chips}");
    }

    #[test]
    fn grunge_backdrop_is_dark_with_vignette_scanlines_and_registration_ticks() {
        let g = gen_grunge_bg();
        assert!(mean_luma(&g) < 0.12, "backdrop must stay dark behind text: {}", mean_luma(&g));
        // Vignette: the center outshines the corners on average.
        let patch = |x0: u32, y0: u32| (0..24).flat_map(|dy| (0..24).map(move |dx| (dx, dy))).map(|(dx, dy)| sum(&g, x0 + dx, y0 + dy)).sum::<u32>();
        let center = patch(GRUNGE_W / 2 - 12, GRUNGE_H / 2 - 12);
        assert!(center > 2 * patch(0, 0), "vignette {center} vs {}", patch(0, 0));
        assert!(center > 2 * patch(GRUNGE_W - 24, GRUNGE_H - 24));
        // Scanlines: odd rows darker than even rows overall.
        let row = |y| (0..GRUNGE_W).map(|x| sum(&g, x, y)).sum::<u32>();
        let even: u32 = (0..GRUNGE_H).step_by(2).map(row).sum();
        let odd: u32 = (1..GRUNGE_H).step_by(2).map(row).sum();
        assert!(even > odd);
        // Registration ticks: an L in each corner pops off its surroundings and
        // stays clear of the screen center (no crosshair behind the content).
        let (ix, iy) = GRUNGE_TICK_INSET;
        for (x, y) in [(ix, iy), (GRUNGE_W - 1 - ix, iy), (ix, GRUNGE_H - 1 - iy), (GRUNGE_W - 1 - ix, GRUNGE_H - 1 - iy)] {
            let inward = (if x < GRUNGE_W / 2 { x + 3 } else { x - 3 }, if y < GRUNGE_H / 2 { y + 3 } else { y - 3 });
            assert!(sum(&g, x, y) > sum(&g, inward.0, inward.1) + 40, "tick at ({x},{y})");
        }
    }

    #[test]
    fn plate_is_seamless_brushed_steel_without_a_rivet_grid() {
        let p = gen_plate_bg();
        assert!(mean_luma(&p) < 0.2, "plate stays dark behind cards: {}", mean_luma(&p));
        // No bright rivet heads anywhere: hardware is bolted on with nodes.
        let brightest = (0..PLATE_SIZE).flat_map(|y| (0..PLATE_SIZE).map(move |x| (x, y))).map(|(x, y)| sum(&p, x, y)).max().unwrap();
        assert!(brightest < 300, "no rivet highlights baked into the plate: {brightest}");
        // Seamless: the wrap-around columns / rows differ no more than
        // ordinary neighbours do (the old plate had a hard dark frame here).
        let s = PLATE_SIZE;
        let col_step = |a: u32, b: u32| (0..s).map(|y| sum(&p, a, y).abs_diff(sum(&p, b, y))).sum::<u32>() as f32 / s as f32;
        let row_step = |a: u32, b: u32| (0..s).map(|x| sum(&p, x, a).abs_diff(sum(&p, x, b))).sum::<u32>() as f32 / s as f32;
        assert!(col_step(s - 1, 0) < 1.6 * col_step(100, 101) + 2.0, "x wrap {} vs {}", col_step(s - 1, 0), col_step(100, 101));
        assert!(row_step(s - 1, 0) < 1.6 * row_step(100, 101) + 2.0, "y wrap {} vs {}", row_step(s - 1, 0), row_step(100, 101));
        // Hairline seams: one dark cut per axis.
        let (sx, sy) = PLATE_SEAM;
        let col = |x: u32| (0..s).map(|y| sum(&p, x, y)).sum::<u32>();
        let row = |y: u32| (0..s).map(|x| sum(&p, x, y)).sum::<u32>();
        assert!(col(sx) * 10 < col(sx - 3) * 7 && row(sy) * 10 < row(sy - 3) * 7);
        // Diagonal micro-scratches: bright scuffs run along x - y diagonals, so
        // a scuffed pixel's diagonal neighbour is scuffed far more often than
        // its horizontal neighbour.
        let mean = (0..s).flat_map(|y| (0..s).map(move |x| (x, y))).map(|(x, y)| sum(&p, x, y)).sum::<u32>() as f32 / (s * s) as f32;
        let scuffed = |x: u32, y: u32| sum(&p, x % s, y % s) as f32 > mean * 1.45;
        let (mut diag, mut horiz) = (0, 0);
        for y in 0..s {
            for x in 0..s {
                if scuffed(x, y) {
                    diag += u32::from(scuffed(x + 1, y + 1));
                    horiz += u32::from(scuffed(x + 1, y));
                }
            }
        }
        assert!(diag > 200 && diag > 3 * horiz, "scratches are diagonal: {diag} vs {horiz}");
    }

    #[test]
    fn palette_is_warm_and_buttons_snap_with_contrast() {
        // Bone text, never sterile pure white; ink is near black.
        assert_ne!(BONE_TEXT, Color::WHITE);
        let bone = BONE_TEXT.to_srgba();
        assert!(bone.red > bone.blue);
        assert!(luminance(INK_BLACK) < 0.01);
        assert!(contrast_ratio(BONE_TEXT, IRON_DARK) > 10.0);
        for style in [ButtonStyle::PrimaryAcid, ButtonStyle::DangerRed, ButtonStyle::HazardAmber, ButtonStyle::ScrapSteel] {
            let c = PunkButtonColors::of(style);
            assert!(contrast_ratio(c.idle_text, c.idle_bg) >= 4.5, "{style:?} idle");
            assert!(contrast_ratio(c.hover_text, c.hover_bg) >= 4.5, "{style:?} hover");
            // Hover snaps: inverted (dark text on a much brighter fill).
            assert_eq!(c.hover_text, INK_BLACK);
            assert!(luminance(c.hover_bg) > 4.0 * luminance(c.idle_bg), "{style:?} hover fill must pop");
            assert_ne!(c.border(true), c.border(false));
            assert_eq!(c.bg(true), c.hover_bg);
            assert_eq!(c.text(false), c.idle_text);
        }
        let all: HashSet<String> = [ButtonStyle::PrimaryAcid, ButtonStyle::DangerRed, ButtonStyle::HazardAmber, ButtonStyle::ScrapSteel]
            .iter()
            .map(|s| format!("{:?}", PunkButtonColors::of(*s).hover_bg))
            .collect();
        assert_eq!(all.len(), 4, "every style has its own hover color");
    }

    #[test]
    fn hero_button_is_already_lit_and_outranks_the_secondary_switches() {
        let hero = PunkButtonColors::of(ButtonStyle::HeroAcid);
        // Solid phosphor slab with ink text while idle: the loudest thing on
        // the row, far brighter than any secondary switch's idle fill.
        assert_eq!((hero.idle_bg, hero.idle_text), (ACID_GREEN, INK_BLACK));
        assert!(contrast_ratio(hero.idle_text, hero.idle_bg) >= 7.0);
        assert!(contrast_ratio(hero.hover_text, hero.hover_bg) >= 7.0);
        for style in [ButtonStyle::PrimaryAcid, ButtonStyle::DangerRed, ButtonStyle::HazardAmber, ButtonStyle::ScrapSteel] {
            assert!(luminance(hero.idle_bg) > 10.0 * luminance(PunkButtonColors::of(style).idle_bg), "{style:?}");
        }
        // Hover still changes fill and frame.
        assert_ne!(hero.bg(true), hero.bg(false));
        assert_ne!(hero.border(true), hero.border(false));
    }

    #[test]
    fn plate_borders_are_asymmetric_and_bezels_are_inset() {
        let b = plate_border(ACID_GREEN, STEEL_BORDER);
        assert_eq!((b.left, b.top, b.right, b.bottom), (ACID_GREEN, STEEL_BORDER, STEEL_BORDER, INK_BLACK));
        let r = plate_rect(CARD_BORDER);
        assert_eq!((r.left, r.right, r.top, r.bottom), (Val::Px(4.0), Val::Px(1.0), Val::Px(1.0), Val::Px(3.0)));
        let c = plate_rect(CHASSIS_BORDER);
        assert_eq!((c.left, c.bottom), (Val::Px(5.0), Val::Px(5.0)));
        // A recessed slot is lit from the opposite side of a raised plate:
        // shadowed top / left, lit bottom / right.
        let i = inset_border();
        assert_eq!((i.top, i.left), (INK_BLACK, INK_BLACK));
        assert!(luminance(i.bottom) > luminance(i.top) && i.bottom == i.right);
        // Hard shadows are offset down-right with zero blur and zero spread.
        let s = hard_shadow(6.0);
        assert_eq!(s.0.len(), 1);
        assert_eq!((s.0[0].x_offset, s.0[0].y_offset, s.0[0].blur_radius, s.0[0].spread_radius), (Val::Px(6.0), Val::Px(6.0), Val::Px(0.0), Val::Px(0.0)));
    }

    fn kit_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default())).init_asset::<Image>().add_plugins(UiKitPlugin);
        app.update();
        app
    }

    #[test]
    fn plugin_registers_textures_and_helpers_build_angular_trees() {
        let mut app = kit_app();
        let tex = app.world().resource::<UiKitTextures>().clone();
        {
            let images = app.world().resource::<Assets<Image>>();
            for tape in Tape::ALL {
                for h in TAPE_HEIGHTS {
                    let (handle, authored) = tex.tape(tape, h as f32);
                    assert_eq!(authored, h);
                    assert_eq!(images.get(&handle).unwrap().size(), UVec2::new(192, h));
                }
            }
            assert_eq!(images.get(&tex.grunge_bg).unwrap().size(), UVec2::new(640, 360));
            assert_eq!(images.get(&tex.plate_bg).unwrap().size(), UVec2::new(256, 256));
        }
        let world = app.world_mut();
        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut commands_queue, world);
        let root = punk_screen_root(&mut commands, AppState::MainMenu, Some(&tex), INK_BLACK, 0.9);
        let body = terminal_chassis(&mut commands, root, Some(&tex), 700.0, true);
        let title = stencil_title(&mut commands, body, "PAUSED", 64.0, BONE_TEXT, PUNK_RED);
        let hero = hero_title(&mut commands, body, "SEGNALE", 96.0, BONE_TEXT, PUNK_RED);
        let (sec, header) = section_crate(&mut commands, body, "STARTER OS CHIP", HAZARD_YELLOW, Node::default());
        let card = rivet_card(&mut commands, sec, MetaTarget::JamDemo, UiRect::all(Val::Px(6.0)), 3.0);
        let strip = hazard_strip(&mut commands, header, None, 8.0, false);
        let bars = signal_bars(&mut commands, body, 1, PUNK_RED, 6.0);
        commands.spawn((ChildOf(body), punk_button("RESUME  [ESC]", MenuAction::Continue, 20.0, ButtonStyle::PrimaryAcid)));
        commands.spawn((ChildOf(body), stamped_tag("SIG", HAZARD_YELLOW, INK_BLACK, 10.0)));
        commands.spawn((ChildOf(body), keycap("WASD", 12.0)));
        commands.spawn((ChildOf(body), hud_plate(Node::default(), ACID_GREEN)));
        commands.spawn((ChildOf(body), recessed_bezel(Node::default())));
        commands_queue.apply(world);

        // Grunge backdrop on the root, tape images on the chassis strips.
        assert!(world.get::<ImageNode>(root).is_some_and(|i| i.image == tex.grunge_bg));
        let mut strips = world.query::<(&ImageNode, &ChassisStrip)>();
        let s: Vec<_> = strips.iter(world).collect();
        assert_eq!(s.len(), 2);
        for (image, strip) in &s {
            // Danger tape, authored at the strip's own height: drawn 1:1.
            assert_eq!(image.image, tex.tape(Tape::Danger, strip.0).0);
            assert!(matches!(image.image_mode, NodeImageMode::Tiled { tile_x: true, tile_y: false, stretch_value } if stretch_value == 1.0));
        }
        // Fallback without textures: flat caution color, no image.
        assert!(world.get::<ImageNode>(strip).is_none());
        assert_eq!(world.get::<BackgroundColor>(strip).unwrap().0, HAZARD_YELLOW);
        // Stencil: shadow + main copies of the title; the hero adds a slab.
        let mut texts = world.query::<&Text>();
        assert_eq!(texts.iter(world).filter(|t| t.0 == "PAUSED").count(), 2);
        assert_eq!(texts.iter(world).filter(|t| t.0 == "SEGNALE").count(), 3);
        assert!(texts.iter(world).any(|t| t.0 == "[ STARTER OS CHIP ]"));
        assert!(world.get::<Children>(title).is_some_and(|c| c.len() == 2));
        assert!(world.get::<Children>(hero).is_some_and(|c| c.len() == 2));
        // Cards and buttons: hard corners, asymmetric borders, hit-test tags,
        // and a hard ink shadow under everything pressable.
        let n = world.get::<Node>(card).unwrap();
        assert_eq!(n.border_radius, BorderRadius::ZERO);
        assert_eq!((n.border.left, n.border.bottom), (Val::Px(4.0), Val::Px(3.0)));
        assert_eq!(world.get::<MetaCard>(card).unwrap().0, MetaTarget::JamDemo);
        assert!(world.get::<BoxShadow>(card).is_some());
        let mut buttons = world.query_filtered::<(&Node, &PunkButtonColors, &MenuAction, &BoxShadow), With<MenuButton>>();
        let (bn, colors, action, shadow) = buttons.single(world).unwrap();
        assert_eq!(bn.border_radius, BorderRadius::ZERO);
        assert_eq!(*action, MenuAction::Continue);
        assert_eq!(colors.hover_bg, ACID_GREEN);
        assert_eq!(shadow.0[0].blur_radius, Val::Px(0.0));
        // Signal bars: five ascending slots, only the first lit.
        let kids: Vec<Entity> = world.get::<Children>(bars).unwrap().iter().collect();
        assert_eq!(kids.len(), 5);
        let lit: Vec<bool> = kids.iter().map(|e| world.get::<BackgroundColor>(*e).unwrap().0 == PUNK_RED).collect();
        assert_eq!(lit, [true, false, false, false, false]);
        let heights: Vec<f32> = kids.iter().map(|e| if let Val::Px(h) = world.get::<Node>(*e).unwrap().height { h } else { 0.0 }).collect();
        assert!(heights.windows(2).all(|w| w[0] < w[1]), "{heights:?}");
        // Every node the kit spawned has hard corners.
        let mut nodes = world.query::<&Node>();
        assert!(nodes.iter(world).all(|n| n.border_radius == BorderRadius::ZERO));
    }

    #[test]
    fn chassis_bolts_an_l_bracket_and_rivet_onto_each_outer_corner() {
        let mut app = kit_app();
        let world = app.world_mut();
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut queue, world);
        let root = commands.spawn(Node::default()).id();
        let body = terminal_chassis(&mut commands, root, None, 700.0, false);
        queue.apply(world);
        let chassis = world.get::<ChildOf>(body).unwrap().parent();
        assert!(world.get::<TerminalChassis>(chassis).is_some() && world.get::<BoxShadow>(chassis).is_some());

        let mut q = world.query_filtered::<(&Node, &ChildOf, &BorderColor), With<CornerBracket>>();
        let parts: Vec<_> = q.iter(world).collect();
        // Per corner: ink outline L + steel L + rivet.
        assert_eq!(parts.len(), 12);
        let px = |v: Val| if let Val::Px(p) = v { Some(p) } else { None };
        let mut corners = HashSet::new();
        let (mut plates, mut rivets) = (0, 0);
        for (node, parent, _) in &parts {
            assert_eq!(parent.parent(), chassis, "brackets hang off the chassis, not the body");
            assert_eq!(node.position_type, PositionType::Absolute, "brackets never disturb the layout");
            // Anchored to exactly one horizontal and one vertical edge.
            assert!(px(node.left).is_some() != px(node.right).is_some() && px(node.top).is_some() != px(node.bottom).is_some());
            corners.insert((px(node.left).is_some(), px(node.top).is_some()));
            let painted = [node.border.left, node.border.right, node.border.top, node.border.bottom].map(|b| px(b).is_some_and(|w| w > 0.0));
            if painted.contains(&true) {
                // An L plate: exactly the two borders facing its corner are
                // painted, and it sits flush on the border-box corner (outset
                // by the chassis border widths).
                plates += 1;
                assert_eq!(painted, [px(node.left).is_some(), px(node.right).is_some(), px(node.top).is_some(), px(node.bottom).is_some()]);
                let (l, r, t, b) = CHASSIS_BORDER;
                let flush = [(node.left, -l), (node.right, -r), (node.top, -t), (node.bottom, -b)];
                assert!(flush.iter().all(|(v, want)| px(*v).is_none_or(|p| p == *want)), "{node:?}");
            } else {
                // A rivet head: a small solid square sunk into the elbow.
                rivets += 1;
                assert_eq!((node.width, node.height), (Val::Px(3.0), Val::Px(3.0)));
            }
        }
        assert_eq!(corners.len(), 4, "one bracket per corner");
        assert_eq!((plates, rivets), (8, 4), "ink outline + steel plate + rivet per corner");
        let steel = parts.iter().filter(|(_, _, b)| b.left == STEEL_LIGHT).count();
        assert_eq!(steel, 4);
    }

    #[test]
    fn textures_exist_before_the_first_frame() {
        // Initial `OnEnter` systems run before PreStartup commands apply.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default())).init_asset::<Image>().add_plugins(UiKitPlugin);
        assert!(app.world().contains_resource::<UiKitTextures>());
        let before = app.world().resource::<UiKitTextures>().plate_bg.clone();
        app.update();
        assert_eq!(app.world().resource::<UiKitTextures>().plate_bg, before, "PreStartup fallback must not regenerate");
    }

    #[test]
    fn strips_swap_tapes_and_stay_pixel_perfect() {
        let app = kit_app();
        let tex = app.world().resource::<UiKitTextures>().clone();
        let mut img = tape_image(&tex, 10.0, false);
        assert_eq!(img.image, tex.tape(Tape::Caution, 10.0).0);
        set_strip_tape(&mut img, &tex, 10.0, true);
        assert_eq!(img.image, tex.tape(Tape::Danger, 10.0).0);
        set_strip_tape(&mut img, &tex, 10.0, Tape::Signal);
        assert_eq!(img.image, tex.tape(Tape::Signal, 10.0).0);
        assert_ne!(tex.tape(Tape::Signal, 10.0).0, tex.tape(Tape::Signal, 12.0).0, "one texture per authored height");
        // Authored heights draw 1:1; in-between heights scale the nearest one.
        for h in TAPE_HEIGHTS {
            assert!(matches!(tape_image(&tex, h as f32, false).image_mode, NodeImageMode::Tiled { stretch_value, .. } if stretch_value == 1.0));
        }
        assert_eq!(tex.tape(Tape::Caution, 5.0).1, 6);
        assert!(matches!(tape_image(&tex, 5.0, false).image_mode, NodeImageMode::Tiled { stretch_value, .. } if (stretch_value - 5.0 / 6.0).abs() < 1e-6));
        assert_eq!(tex.tape(Tape::Caution, 40.0).1, 16);
    }
}
