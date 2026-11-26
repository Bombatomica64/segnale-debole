//! Procedural **Bandwidth Style Rank emblem** rasteriser (Milestone 31 HUD).
//!
//! A DMC-flavoured floating rank badge with zero external font / image files:
//!
//! - **Rank lettering** (`D`, `C`, `B`, `A`, `S`, `SS`, `SSS`): hand-authored
//!   forward-slanted blade-gothic vector outlines ([`GLYPHS`]). [`EmblemAtlas::bake`]
//!   turns every rank lockup into exact polygon signed-distance layers **once at
//!   startup** and stores four coverage bytes per texel (`ink` outline, `fill`,
//!   metallic `tone`, outer `glow`). `SS` / `SSS` are staggered, tilted,
//!   overlapping letters with their own outlines and contact shadows.
//! - **Radial meter**: a clockwise segmented gauge ring with a hot spark at
//!   the fill tip, rotating signal-notch ticks and (at `S` and above) orbiting
//!   6G electric arcs, all analytic from the shared polar tables.
//! - **Animation**: [`EmblemAtlas::render`] composites one frame from an
//!   [`EmblemFrame`] (time, rank-up `pop`, `action_pop`, `hit_shake`) straight
//!   into a caller-owned RGBA8 buffer: metallic sheen sweep, neon breathing,
//!   shockwave ring + blade-slash streak on a rank-up, ring surge on every
//!   action, crimson scanline glitch on a hit.
//!
//! The module is pure math over `f32` / `u8` slices (no Bevy types, no clocks,
//! no threads, no allocation in [`EmblemAtlas::render`]), so it is wasm32-safe
//! and unit-testable headlessly. `hud.rs` owns the single `Image` it draws to.

use core::f32::consts::TAU;

/// Side of the square emblem texture (texels): 2x supersampled for the
/// 96 logical px HUD node.
pub const EMBLEM_PX: usize = 192;
/// Byte length of the RGBA8 emblem buffer.
pub const EMBLEM_BYTES: usize = EMBLEM_PX * EMBLEM_PX * 4;
/// Ranks baked into the atlas (`D` .. `SSS`).
pub const EMBLEM_RANKS: usize = 7;

const HALF: f32 = EMBLEM_PX as f32 * 0.5;
/// Italic shear of every glyph (x per unit of cap height).
const SLANT: f32 = 0.24;
/// Thick ink outline around the lettering (texels).
const OUTLINE: f32 = 5.0;
/// Outer neon aura reach beyond the outline (texels).
const GLOW_REACH: f32 = 11.0;
/// Bevel depth of the metallic lettering (texels).
const BEVEL: f32 = 6.5;
/// Gauge ring band (emblem units, 1 = half the texture).
const RING_IN: f32 = 0.765;
const RING_OUT: f32 = 0.875;
const RING_MID: f32 = (RING_IN + RING_OUT) * 0.5;
/// Ink lip on both sides of the gauge band.
const RING_INK: f32 = 0.022;
/// Gauge segments around the ring.
const RING_SEGMENTS: f32 = 24.0;
/// Signal-notch ticks around the ring (every [`TICK_MAJOR`]th is a long notch).
const TICKS: f32 = 36.0;
const TICK_MAJOR: u32 = 12;

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const CRIMSON: [f32; 3] = [1.0, 0.07, 0.13];
const ELECTRIC: [f32; 3] = [0.72, 0.95, 1.0];

type Contour = &'static [[f32; 2]];

/// One letter outline in design space: x right, y down, cap height 1.
/// Contours fill with the even-odd rule (later contours are counters).
struct Glyph {
    contours: &'static [Contour],
}

const GLYPH_D: usize = 0;
const GLYPH_C: usize = 1;
const GLYPH_B: usize = 2;
const GLYPH_A: usize = 3;
const GLYPH_S: usize = 4;

/// Blade-gothic rank letters: heavy chamfered strokes, fang serifs and
/// slash-cut terminals (sheared forward by [`SLANT`] at bake time).
const GLYPHS: [Glyph; 5] = [
    // D: fang spur top-left, chamfered bowl, dagger foot.
    Glyph {
        contours: &[
            &[[-0.16, 0.0], [0.50, 0.0], [0.80, 0.24], [0.80, 0.72], [0.52, 1.0], [0.22, 1.0], [0.0, 1.12], [0.0, 0.17]],
            &[[0.27, 0.21], [0.43, 0.21], [0.55, 0.31], [0.55, 0.67], [0.44, 0.79], [0.27, 0.79]],
        ],
    },
    // C: open chamfered hook with two blade terminals.
    Glyph {
        contours: &[&[
            [0.24, 0.0],
            [0.64, 0.0],
            [0.86, 0.10],
            [0.75, 0.38],
            [0.56, 0.22],
            [0.35, 0.22],
            [0.27, 0.30],
            [0.27, 0.70],
            [0.35, 0.78],
            [0.54, 0.78],
            [0.74, 0.61],
            [0.86, 0.86],
            [0.62, 1.0],
            [0.24, 1.0],
            [0.0, 0.78],
            [0.0, 0.22],
        ]],
    },
    // B: fang spur, two chamfered bowls with a pinched waist, dagger foot.
    Glyph {
        contours: &[
            &[[-0.16, 0.0], [0.48, 0.0], [0.72, 0.15], [0.72, 0.37], [0.61, 0.475], [0.80, 0.60], [0.80, 0.83], [0.56, 1.0], [0.22, 1.0], [0.0, 1.12], [0.0, 0.17]],
            &[[0.27, 0.185], [0.42, 0.185], [0.48, 0.24], [0.48, 0.345], [0.42, 0.40], [0.27, 0.40]],
            &[[0.27, 0.575], [0.46, 0.575], [0.54, 0.64], [0.54, 0.77], [0.47, 0.825], [0.27, 0.825]],
        ],
    },
    // A: spear apex, heavy right leg, blade crossbar poking out to the left.
    Glyph {
        contours: &[
            &[[0.40, -0.06], [0.60, 0.0], [0.94, 1.0], [0.66, 1.0], [0.60, 0.80], [0.31, 0.80], [0.21, 1.06], [-0.06, 1.0], [0.03, 0.80], [-0.12, 0.84], [0.10, 0.62]],
            &[[0.445, 0.33], [0.545, 0.62], [0.34, 0.62]],
        ],
    },
    // S: angular lightning spine with slash-cut terminals.
    Glyph {
        contours: &[&[
            [0.20, 0.0],
            [0.64, 0.0],
            [0.90, 0.06],
            [0.77, 0.33],
            [0.58, 0.215],
            [0.33, 0.215],
            [0.27, 0.27],
            [0.27, 0.33],
            [0.34, 0.385],
            [0.66, 0.415],
            [0.82, 0.54],
            [0.82, 0.80],
            [0.60, 1.0],
            [0.18, 1.0],
            [-0.08, 0.94],
            [0.05, 0.67],
            [0.24, 0.785],
            [0.49, 0.785],
            [0.55, 0.73],
            [0.55, 0.67],
            [0.48, 0.615],
            [0.16, 0.585],
            [0.0, 0.46],
            [0.0, 0.20],
        ]],
    },
];

/// One placed letter of a rank lockup (emblem units, drawn in order).
struct Placed {
    glyph: usize,
    /// Center of the letter's sheared bounding box.
    cx: f32,
    cy: f32,
    /// Cap height.
    h: f32,
    /// Horizontal condense factor (1 = as drawn).
    wx: f32,
    /// Clockwise tilt (degrees).
    rot: f32,
}

const fn place(glyph: usize, cx: f32, cy: f32, h: f32, wx: f32, rot: f32) -> Placed {
    Placed { glyph, cx, cy, h, wx, rot }
}

/// A rank lockup: its letters and the ink outline thickness (texels; the
/// crowded `SS` / `SSS` lockups carry a finer line so every `S` stays open).
struct Lockup {
    letters: &'static [Placed],
    outline: f32,
}

/// Rank lockups, `D` .. `SSS`. Single letters sit inside the gauge ring; the
/// `SS` / `SSS` lockups are condensed, stagger down-right, grow and burst
/// over it.
const LOCKUPS: [Lockup; EMBLEM_RANKS] = [
    Lockup { letters: &[place(GLYPH_D, 0.02, -0.01, 1.14, 1.0, 0.0)], outline: OUTLINE },
    Lockup { letters: &[place(GLYPH_C, 0.0, 0.0, 1.16, 1.0, 0.0)], outline: OUTLINE },
    Lockup { letters: &[place(GLYPH_B, 0.02, -0.01, 1.14, 1.0, 0.0)], outline: OUTLINE },
    Lockup { letters: &[place(GLYPH_A, 0.0, -0.01, 1.14, 1.0, 0.0)], outline: OUTLINE },
    Lockup { letters: &[place(GLYPH_S, 0.0, 0.0, 1.18, 1.0, 0.0)], outline: OUTLINE },
    Lockup { letters: &[place(GLYPH_S, -0.31, -0.07, 1.0, 0.78, -3.0), place(GLYPH_S, 0.30, 0.07, 1.08, 0.78, 2.0)], outline: 4.2 },
    Lockup {
        letters: &[place(GLYPH_S, -0.48, -0.10, 0.76, 0.74, -4.0), place(GLYPH_S, -0.03, 0.0, 0.83, 0.74, 0.0), place(GLYPH_S, 0.45, 0.10, 0.91, 0.74, 3.0)],
        outline: 3.6,
    },
];

/// Everything that animates one emblem frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmblemFrame {
    /// Rank index, `0 = D` .. `6 = SSS` (clamped).
    pub rank: usize,
    /// Rank neon color (sRGB, 0..1).
    pub color: [f32; 3],
    /// Tier fill of the radial gauge, 0..1.
    pub fill: f32,
    /// Idle-motion clock (seconds).
    pub time: f32,
    /// Rank-up slam envelope (1 on a climb, decays to 0).
    pub pop: f32,
    /// Action / kill pulse envelope (1 on any gain, decays to 0).
    pub action_pop: f32,
    /// Take-hit glitch envelope (1 on a demotion, decays to 0).
    pub hit_shake: f32,
}

impl EmblemFrame {
    /// A resting frame of `rank` (no envelopes running).
    pub fn idle(rank: usize, color: [f32; 3], fill: f32, time: f32) -> Self {
        EmblemFrame { rank, color, fill, time, pop: 0.0, action_pop: 0.0, hit_shake: 0.0 }
    }
}

/// Pre-baked lettering layers + polar tables (built once, read every frame).
pub struct EmblemAtlas {
    /// Per rank, per texel: `[ink, fill, tone, glow]` coverage bytes.
    ranks: [Vec<[u8; 4]>; EMBLEM_RANKS],
    /// Distance from the emblem center (units, 1 = half the texture).
    rad: Vec<f32>,
    /// Clockwise angle from 12 o'clock, 0..1 turns.
    ang: Vec<f32>,
}

fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(13);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / 16_777_216.0
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn sat(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Antialiased coverage of a signed texel distance (negative = inside).
fn cover(d: f32) -> f32 {
    sat(0.5 - d)
}

/// Chrome ramp along a letter's height (0 top .. 1 bottom): white-hot crown,
/// neon core, a hard horizon glint, deep shadow base.
fn chrome(v: f32) -> f32 {
    if v < 0.36 {
        mix(0.97, 0.50, smooth(v / 0.36))
    } else if v < 0.56 {
        mix(0.50, 0.74, smooth((v - 0.36) / 0.20))
    } else {
        mix(0.40, 0.13, smooth((v - 0.56) / 0.44))
    }
}

/// Metallic-to-neon color of a `tone` (0 shadow .. 0.5 neon .. 1 white-hot).
fn ramp(tone: f32, neon: [f32; 3]) -> [f32; 3] {
    if tone < 0.5 {
        let dark = [neon[0] * 0.13 + 0.015, neon[1] * 0.10, neon[2] * 0.13 + 0.03];
        mix3(dark, neon, smooth(tone * 2.0))
    } else {
        let t = (tone - 0.5) * 2.0;
        mix3(neon, WHITE, t * t * 0.4 + t * 0.6)
    }
}

/// A placed letter's contours in texel space plus its bounding box.
struct Layer {
    contours: Vec<Vec<[f32; 2]>>,
    min: [f32; 2],
    max: [f32; 2],
}

impl Layer {
    fn new(p: &Placed) -> Layer {
        let glyph = &GLYPHS[p.glyph];
        let shear = |q: [f32; 2]| [q[0] * p.wx - (q[1] - 0.5) * SLANT, q[1]];
        // Center of the sheared design-space bounding box.
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for q in glyph.contours.iter().flat_map(|c| c.iter()) {
            let s = shear(*q);
            for k in 0..2 {
                lo[k] = lo[k].min(s[k]);
                hi[k] = hi[k].max(s[k]);
            }
        }
        let mid = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let (sin, cos) = p.rot.to_radians().sin_cos();
        let scale = p.h * HALF;
        let mut layer = Layer { contours: Vec::with_capacity(glyph.contours.len()), min: [f32::MAX; 2], max: [f32::MIN; 2] };
        for c in glyph.contours {
            let mut pts = Vec::with_capacity(c.len());
            for q in c.iter() {
                let s = shear(*q);
                let (x, y) = ((s[0] - mid[0]) * scale, (s[1] - mid[1]) * scale);
                let t = [x * cos - y * sin + p.cx * HALF + HALF, x * sin + y * cos + p.cy * HALF + HALF];
                for k in 0..2 {
                    layer.min[k] = layer.min[k].min(t[k]);
                    layer.max[k] = layer.max[k].max(t[k]);
                }
                pts.push(t);
            }
            layer.contours.push(pts);
        }
        layer
    }

    /// Exact signed distance (texels, negative inside) of the even-odd outline.
    fn sdf(&self, px: f32, py: f32) -> f32 {
        let mut d2 = f32::MAX;
        let mut inside = false;
        for c in &self.contours {
            let n = c.len();
            for i in 0..n {
                let (a, b) = (c[i], c[(i + 1) % n]);
                let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
                let (wx, wy) = (px - a[0], py - a[1]);
                let t = sat((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-9));
                let (dx, dy) = (wx - ex * t, wy - ey * t);
                d2 = d2.min(dx * dx + dy * dy);
                if (a[1] > py) != (b[1] > py) && px < a[0] + (py - a[1]) / (b[1] - a[1]) * ex {
                    inside = !inside;
                }
            }
        }
        if inside { -d2.sqrt() } else { d2.sqrt() }
    }
}

/// Bake one rank lockup into `[ink, fill, tone, glow]` texels.
fn bake_rank(lockup: &Lockup) -> Vec<[u8; 4]> {
    let outline = lockup.outline;
    let n = EMBLEM_PX * EMBLEM_PX;
    let reach = outline + GLOW_REACH + 2.0;
    let mut ink = vec![0.0f32; n];
    let mut fill = vec![0.0f32; n];
    let mut tone = vec![0.0f32; n];
    let mut near = vec![reach; n];
    let mut dist = vec![reach; n];
    // Light from the upper left (unit vector toward the light).
    let light = [-0.50f32, -0.866f32];
    for p in lockup.letters {
        let layer = Layer::new(p);
        let x0 = (layer.min[0] - reach).floor().max(0.0) as usize;
        let y0 = (layer.min[1] - reach).floor().max(0.0) as usize;
        let x1 = ((layer.max[0] + reach).ceil() as usize).min(EMBLEM_PX - 1);
        let y1 = ((layer.max[1] + reach).ceil() as usize).min(EMBLEM_PX - 1);
        dist.fill(reach);
        for y in y0..=y1 {
            for x in x0..=x1 {
                dist[y * EMBLEM_PX + x] = layer.sdf(x as f32 + 0.5, y as f32 + 0.5).min(reach);
            }
        }
        let height = (layer.max[1] - layer.min[1]).max(1.0);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let i = y * EMBLEM_PX + x;
                let d = dist[i];
                near[i] = near[i].min(d);
                let ink_l = cover(d - outline);
                if ink_l <= 0.0 {
                    // Contact shadow of this letter on the ones beneath it.
                    tone[i] *= 1.0 - 0.55 * sat(1.0 - (d - outline) / 9.0);
                    continue;
                }
                let fill_l = cover(d);
                // This letter's outline cuts whatever sits beneath it.
                fill[i] = fill[i] * (1.0 - ink_l) + fill_l;
                ink[i] = ink[i] + ink_l * (1.0 - ink[i]);
                if fill_l > 0.0 {
                    let v = sat((y as f32 + 0.5 - layer.min[1]) / height);
                    // Bevel: rim texels lit by the outward normal.
                    let at = |xx: usize, yy: usize| dist[yy.min(EMBLEM_PX - 1) * EMBLEM_PX + xx.min(EMBLEM_PX - 1)];
                    let gx = at(x + 1, y) - at(x.saturating_sub(1), y);
                    let gy = at(x, y + 1) - at(x, y.saturating_sub(1));
                    let gl = (gx * gx + gy * gy).sqrt().max(1e-4);
                    let lit = (gx * light[0] + gy * light[1]) / gl;
                    let rim = sat(1.0 - (-d) / BEVEL);
                    let t = chrome(v) + rim * (lit * 0.46 + 0.04);
                    tone[i] = mix(tone[i], sat(t), sat(fill_l));
                }
            }
        }
    }
    (0..n)
        .map(|i| {
            let glow = sat(1.0 - (near[i] - outline) / GLOW_REACH);
            let q = |v: f32| (sat(v) * 255.0 + 0.5) as u8;
            [q(ink[i]), q(fill[i].min(ink[i])), q(tone[i]), q(glow * glow)]
        })
        .collect()
}

impl EmblemAtlas {
    /// Bake every rank lockup and the polar tables (startup only).
    pub fn bake() -> EmblemAtlas {
        let n = EMBLEM_PX * EMBLEM_PX;
        let mut rad = Vec::with_capacity(n);
        let mut ang = Vec::with_capacity(n);
        for y in 0..EMBLEM_PX {
            for x in 0..EMBLEM_PX {
                let (dx, dy) = ((x as f32 + 0.5 - HALF) / HALF, (y as f32 + 0.5 - HALF) / HALF);
                rad.push((dx * dx + dy * dy).sqrt());
                ang.push((dx.atan2(-dy) / TAU).rem_euclid(1.0));
            }
        }
        EmblemAtlas { ranks: core::array::from_fn(|r| bake_rank(&LOCKUPS[r])), rad, ang }
    }

    /// Baked `[ink, fill, tone, glow]` texel of `rank` (tests / tooling).
    pub fn texel(&self, rank: usize, x: usize, y: usize) -> [u8; 4] {
        self.ranks[rank.min(EMBLEM_RANKS - 1)][y.min(EMBLEM_PX - 1) * EMBLEM_PX + x.min(EMBLEM_PX - 1)]
    }

    /// Composite one frame into `out` (straight-alpha sRGB RGBA8,
    /// [`EMBLEM_BYTES`] long). Allocation-free; a wrong-sized buffer is left
    /// untouched.
    pub fn render(&self, f: &EmblemFrame, out: &mut [u8]) {
        if out.len() != EMBLEM_BYTES {
            return;
        }
        let rank = f.rank.min(EMBLEM_RANKS - 1);
        let texels = &self.ranks[rank];
        let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
        let time = fin(f.time);
        let pop = sat(fin(f.pop));
        let act = sat(fin(f.action_pop));
        let hit = sat(fin(f.hit_shake));
        let fill = sat(fin(f.fill));
        // Overclock energy: S / SS / SSS crackle harder and harder.
        let energy = [0.0, 0.0, 0.0, 0.0, 0.4, 0.75, 1.0][rank];
        let arcs = [0u32, 0, 0, 0, 2, 3, 5][rank];
        let neon = mix3(f.color.map(|c| sat(fin(c))), CRIMSON, hit * 0.85);
        let hot = mix3(neon, WHITE, 0.5);
        let bolt = mix3(neon, ELECTRIC, 0.6);
        let slam = 1.0 - pop;
        let surge = (act * 0.85 + pop * 0.6).min(1.0);
        let breathe = 0.5 + 0.5 * (time * (2.6 + energy * 7.0)).sin();
        let crackle = if energy > 0.0 { hash((time * 22.0) as u32, 91) } else { 0.5 };
        let glow_gain = 0.36 + 0.10 * breathe + energy * 0.18 * crackle + 0.28 * surge;
        let tick_turn = time * (0.028 + 0.012 * rank as f32);
        // Sheen: a diagonal glint crosses the lettering, then rests off-glyph.
        let sheen_period = 2.9 - 1.3 * energy;
        let sheen_at = -1.5 + (time / sheen_period).rem_euclid(1.0) * 4.6;
        let flash = pop.powi(4) * 0.75 + act * 0.16;
        // Gauge tip.
        let tip_a = fill * TAU;
        let (tip_x, tip_y) = (tip_a.sin() * RING_MID, -tip_a.cos() * RING_MID);
        let spark = 0.8 + 0.2 * (time * 31.0).sin();
        // Rank-up shockwave + blade slash.
        let wave_r = 0.20 + 0.80 * (1.0 - pop * pop);
        let wave_w = (0.07 * pop + 0.03) * HALF;
        let slash_head = -1.25 + 2.7 * sat(slam / 0.26);
        let slash_tail = -1.25 + 2.7 * sat((slam - 0.10) / 0.50);
        let slash_fade = sat(1.0 - slam / 0.62);
        let glitch_frame = (time * 42.0) as u32;
        let w = EMBLEM_PX;
        for y in 0..w {
            // Take-hit glitch: scanline bands tear sideways.
            let shift = if hit > 0.0 {
                let band = (y / 9) as u32;
                let h = hash(band, glitch_frame);
                if h > 0.45 { ((hash(band + 7, glitch_frame) - 0.5) * 2.0 * hit * 15.0) as isize } else { 0 }
            } else {
                0
            };
            let yn = (y as f32 + 0.5 - HALF) / HALF;
            for x in 0..w {
                let o = (y * w + x) * 4;
                let sx = x as isize + shift;
                if sx < 0 || sx >= w as isize {
                    out[o..o + 4].fill(0);
                    continue;
                }
                let i = y * w + sx as usize;
                let rad = self.rad[i];
                let [ink, fil, tone, glow] = texels[i];
                if rad >= 1.0 && ink == 0 && glow == 0 {
                    out[o..o + 4].fill(0);
                    continue;
                }
                let xn = (sx as f32 + 0.5 - HALF) / HALF;
                // Premultiplied accumulator.
                let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
                let over = |c: &mut [f32; 3], a: &mut f32, src: [f32; 3], sa: f32| {
                    let sa = sat(sa);
                    for k in 0..3 {
                        c[k] = src[k] * sa + c[k] * (1.0 - sa);
                    }
                    *a = sa + *a * (1.0 - sa);
                };
                let add = |c: &mut [f32; 3], a: &mut f32, src: [f32; 3], amt: f32| {
                    let amt = amt.max(0.0);
                    for k in 0..3 {
                        c[k] += src[k] * amt;
                    }
                    *a += sat(amt) * (1.0 - *a);
                };
                let edge = sat((1.0 - rad) * HALF / 7.0);
                if rad < 1.0 {
                    let ang = self.ang[i];
                    let arc = TAU * rad * HALF;
                    // Rotating signal-notch ticks.
                    if rad > 0.86 {
                        let ta = (ang - tick_turn).rem_euclid(1.0) * TICKS;
                        let major = (ta as u32).is_multiple_of(TICK_MAJOR);
                        let off = ((ta - ta.floor()) - 0.5).abs() / TICKS * arc;
                        let (lo, hi) = if major { (0.895, 0.985) } else { (0.915, 0.955) };
                        let band = |pad: f32| sat((rad - lo) * HALF + 0.5 + pad) * sat((hi - rad) * HALF + 0.5 + pad);
                        let half = if major { 1.7 } else { 1.0 };
                        over(&mut c, &mut a, [0.0; 3], cover(off - half - 1.4) * band(1.4) * 0.6);
                        let lit = if major { 0.95 } else { 0.55 + 0.25 * energy };
                        over(&mut c, &mut a, if major { hot } else { neon }, cover(off - half) * band(0.0) * lit);
                    }
                    // Segmented gauge ring.
                    if rad > RING_IN - 0.14 && rad < RING_OUT + 0.14 {
                        let band = |lo: f32, hi: f32| sat((rad - lo) * HALF + 0.5) * sat((hi - rad) * HALF + 0.5);
                        over(&mut c, &mut a, [0.0; 3], band(RING_IN - RING_INK, RING_OUT + RING_INK) * 0.84);
                        let seg = ang * RING_SEGMENTS;
                        let gap = (seg - seg.floor()).min(seg.ceil() - seg) / RING_SEGMENTS * arc;
                        let cell = band(RING_IN, RING_OUT) * sat(gap - 0.9);
                        let lit = if fill > 0.002 { sat((fill - ang) * arc + 0.5) } else { 0.0 };
                        over(&mut c, &mut a, [neon[0] * 0.24, neon[1] * 0.24, neon[2] * 0.24], cell * (1.0 - lit) * 0.9);
                        if lit > 0.0 {
                            let across = (rad - RING_IN) / (RING_OUT - RING_IN) * 2.0 - 1.0;
                            let tube = sat(1.0 - across * across);
                            let heat = sat(1.0 - (fill - ang) / 0.16);
                            let body = [neon[0] * (0.72 + 0.28 * tube), neon[1] * (0.72 + 0.28 * tube), neon[2] * (0.72 + 0.28 * tube)];
                            over(&mut c, &mut a, mix3(body, WHITE, (heat * heat * 0.9).max(tube * 0.22 + surge * 0.55).min(1.0)), cell * lit);
                            // Bloom spilling off the lit arc.
                            let off = (RING_IN - rad).max(rad - RING_OUT).max(0.0) * HALF;
                            add(&mut c, &mut a, neon, (-off / 5.0).exp() * lit * (0.34 + 0.2 * breathe * energy + 0.5 * surge) * sat(off) * edge);
                        }
                    }
                    // Hot spark at the fill tip.
                    if fill > 0.002 {
                        let (dx, dy) = ((xn - tip_x) * HALF, (yn - tip_y) * HALF);
                        let d = (dx * dx + dy * dy).sqrt();
                        if d < 26.0 {
                            add(&mut c, &mut a, hot, (-d / 5.2).exp() * 0.85 * spark * edge);
                            add(&mut c, &mut a, WHITE, (-dx.abs().min(dy.abs()) / 1.1).exp() * (-d / 7.5).exp() * 0.75 * spark * edge);
                            add(&mut c, &mut a, WHITE, sat(4.0 + 1.6 * surge - d + 0.5));
                        }
                    }
                    // 6G electric arcs orbiting the ring.
                    if arcs > 0 && rad > 0.70 {
                        for k in 0..arcs {
                            let kf = k as f32;
                            let dir = if k % 2 == 0 { 1.0 } else { -1.0 };
                            let center = kf / arcs as f32 + time * (0.11 + 0.045 * kf) * dir;
                            let mut da = ang - center;
                            da -= da.round();
                            let span = 0.05 + 0.012 * kf;
                            if da.abs() >= span {
                                continue;
                            }
                            let seed = (time * 15.0 + kf * 5.3) as u32;
                            if hash(seed, k + 17) < 0.2 {
                                continue;
                            }
                            let u = da / span;
                            let env = 1.0 - u * u;
                            let q = (u * 0.5 + 0.5) * 7.0;
                            let qi = q.floor();
                            let zig = mix(hash(seed.wrapping_mul(31) + k, qi as u32), hash(seed.wrapping_mul(31) + k, qi as u32 + 1), q - qi) * 2.0 - 1.0;
                            let path = 0.905 + 0.03 * (kf * 2.1).sin() + 0.07 * env * zig;
                            let d = (rad - path).abs() * HALF;
                            add(&mut c, &mut a, bolt, (-d / 3.6).exp() * 0.6 * env * edge);
                            add(&mut c, &mut a, WHITE, sat(1.5 - d) * env.sqrt() * edge);
                        }
                    }
                }
                // Lettering: neon aura, ink outline, metallic fill.
                if glow > 0 {
                    add(&mut c, &mut a, neon, glow as f32 / 255.0 * glow_gain);
                }
                if ink > 0 {
                    over(&mut c, &mut a, [0.0; 3], ink as f32 / 255.0);
                }
                if fil > 0 {
                    let mut col = ramp(tone as f32 / 255.0, neon);
                    let s = xn * 0.8 + yn * 0.6;
                    let glint = (-((s - sheen_at) / 0.13).powi(2)).exp() * 0.85;
                    let shimmer = if energy > 0.5 { energy * 0.16 * (yn * 38.0 - time * 27.0).sin().max(0.0) } else { 0.0 };
                    col = mix3(col, WHITE, (glint + shimmer + flash).min(1.0));
                    over(&mut c, &mut a, col, fil as f32 / 255.0);
                }
                if pop > 0.0 && rad < 1.0 {
                    // Expanding shockwave ring.
                    let off = (rad - wave_r).abs() * HALF;
                    let wave = pop.sqrt() * edge;
                    add(&mut c, &mut a, neon, (-off / (wave_w * 1.6)).exp() * 0.6 * wave);
                    add(&mut c, &mut a, mix3(WHITE, hot, slam), smooth(1.0 - off / wave_w) * 1.2 * wave);
                    // Diagonal white blade-slash streak (lower left to upper right).
                    let along = xn * 0.866 - yn * 0.5;
                    if slash_fade > 0.0 && along < slash_head && along > slash_tail {
                        let (mid, half) = ((slash_head + slash_tail) * 0.5, (slash_head - slash_tail) * 0.5);
                        let lens = 1.0 - ((along - mid) / half.max(1e-3)).powi(2);
                        let across = (xn * 0.5 + yn * 0.866).abs() * HALF;
                        let width = 0.05 * lens * HALF + 0.3;
                        add(&mut c, &mut a, neon, (-across / (width + 3.0)).exp() * 0.55 * lens * slash_fade * edge);
                        add(&mut c, &mut a, WHITE, sat(width - across + 0.5) * slash_fade * edge);
                    }
                }
                if a <= 0.004 {
                    out[o..o + 4].fill(0);
                } else {
                    let inv = 255.0 / a;
                    out[o] = (c[0] * inv).min(255.0) as u8;
                    out[o + 1] = (c[1] * inv).min(255.0) as u8;
                    out[o + 2] = (c[2] * inv).min(255.0) as u8;
                    out[o + 3] = (a * 255.0 + 0.5) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEON: [[f32; 3]; EMBLEM_RANKS] = [[0.62, 0.58, 0.55], [0.35, 0.95, 1.0], [0.4, 1.0, 0.45], [1.0, 0.82, 0.25], [1.0, 0.5, 0.12], [1.0, 0.18, 0.32], [1.0, 0.35, 0.95]];

    fn atlas() -> &'static EmblemAtlas {
        static ATLAS: std::sync::OnceLock<EmblemAtlas> = std::sync::OnceLock::new();
        ATLAS.get_or_init(EmblemAtlas::bake)
    }

    fn draw(f: &EmblemFrame) -> Vec<u8> {
        let mut buf = vec![0u8; EMBLEM_BYTES];
        atlas().render(f, &mut buf);
        buf
    }

    fn px(buf: &[u8], x: usize, y: usize) -> [u8; 4] {
        let o = (y * EMBLEM_PX + x) * 4;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
    }

    /// Texels that differ between two frames.
    fn diff(a: &[u8], b: &[u8]) -> usize {
        a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).filter(|(p, q)| p != q).count()
    }

    /// Alpha-weighted luma sum (how much light the frame throws).
    fn light(buf: &[u8]) -> f64 {
        buf.as_chunks::<4>().0.iter().map(|p| (p[0] as f64 * 0.3 + p[1] as f64 * 0.59 + p[2] as f64 * 0.11) * p[3] as f64 / 255.0).sum()
    }

    /// Texel in the middle of the gauge band at `turn` (clockwise from 12 o'clock).
    fn ring_px(buf: &[u8], turn: f32) -> [u8; 4] {
        let a = turn * TAU;
        px(buf, (HALF + a.sin() * RING_MID * HALF) as usize, (HALF - a.cos() * RING_MID * HALF) as usize)
    }

    fn luma(p: [u8; 4]) -> f32 {
        p[0] as f32 * 0.3 + p[1] as f32 * 0.59 + p[2] as f32 * 0.11
    }

    #[test]
    fn every_rank_renders_a_distinct_non_empty_emblem_with_transparent_corners() {
        let total = EMBLEM_PX * EMBLEM_PX;
        let frames: Vec<Vec<u8>> = (0..EMBLEM_RANKS).map(|r| draw(&EmblemFrame::idle(r, NEON[r], 0.6, 1.0))).collect();
        for (r, buf) in frames.iter().enumerate() {
            assert_eq!(buf.len(), EMBLEM_BYTES);
            let drawn = buf.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
            let solid = buf.as_chunks::<4>().0.iter().filter(|p| p[3] == 255).count();
            assert!(drawn > total / 4 && drawn < total * 9 / 10, "rank {r}: {drawn} texels drawn");
            assert!(solid > total / 10, "rank {r}: {solid} opaque texels (ink + lettering)");
            // Floating emblem: no plate, every corner block fully transparent.
            let last = EMBLEM_PX - 12;
            for (cx, cy) in [(0, 0), (last, 0), (0, last), (last, last)] {
                for y in cy..cy + 12 {
                    for x in cx..cx + 12 {
                        assert_eq!(px(buf, x, y), [0; 4], "rank {r}: corner texel {x},{y}");
                    }
                }
            }
            // Thick ink outline: pure black opaque texels hug the lettering.
            let ink = buf.as_chunks::<4>().0.iter().filter(|p| p[3] == 255 && p[0] < 6 && p[1] < 6 && p[2] < 6).count();
            assert!(ink > 1500, "rank {r}: {ink} ink texels");
            // Metallic-to-neon gradient: white-hot highlights and deep shadow inside the fill.
            let fill: Vec<[u8; 4]> = (0..total).filter(|i| atlas().ranks[r][*i][1] == 255).map(|i| px(buf, i % EMBLEM_PX, i / EMBLEM_PX)).collect();
            assert!(fill.len() > total / 12, "rank {r}: lettering fill {}", fill.len());
            assert!(fill.iter().any(|p| luma(*p) > 225.0) && fill.iter().any(|p| luma(*p) < 70.0), "rank {r}: gradient range");
        }
        // Seven different lockups (compared on the baked lettering, so color alone never counts).
        for a in 0..EMBLEM_RANKS {
            for b in a + 1..EMBLEM_RANKS {
                let letters = (0..total).filter(|i| (atlas().ranks[a][*i][1] > 127) != (atlas().ranks[b][*i][1] > 127)).count();
                assert!(letters > 1200, "ranks {a} / {b} lettering differs by {letters} texels");
                assert!(diff(&frames[a], &frames[b]) > 4000, "ranks {a} / {b}");
            }
        }
        // SS / SSS are wider multi-letter lockups that burst over the ring.
        let width = |r: usize| {
            let xs: Vec<usize> = (0..total).filter(|i| atlas().ranks[r][*i][1] > 127).map(|i| i % EMBLEM_PX).collect();
            xs.iter().max().unwrap() - xs.iter().min().unwrap()
        };
        assert!(width(4) < width(5) && width(5) < width(6), "S {} < SS {} < SSS {}", width(4), width(5), width(6));
        assert!(width(6) as f32 > RING_IN * EMBLEM_PX as f32, "SSS crosses the gauge ring");
        assert_eq!(atlas().texel(6, 0, 0), [0; 4]);
    }

    #[test]
    fn gauge_ring_fills_clockwise_with_a_hot_tip() {
        // Mid-segment sample angles clear of the `C` lettering.
        let (east, south, west) = (6.5 / 24.0, 12.5 / 24.0, 17.5 / 24.0);
        let lit = |fill: f32, turn: f32| luma(ring_px(&draw(&EmblemFrame::idle(1, NEON[1], fill, 0.4)), turn));
        for turn in [east, south, west] {
            let dark = lit(0.0, turn);
            assert!(dark > 5.0 && dark < 90.0, "empty gauge track is dim but present: {dark}");
        }
        assert!(lit(0.4, east) > 130.0 && lit(0.4, south) < 90.0 && lit(0.4, west) < 90.0);
        assert!(lit(0.6, east) > 130.0 && lit(0.6, south) > 130.0 && lit(0.6, west) < 90.0);
        assert!(lit(0.9, west) > 130.0);
        // The track is an opaque ink-backed band; the ring's inside stays see-through.
        let buf = draw(&EmblemFrame::idle(1, NEON[1], 0.0, 0.4));
        assert!(ring_px(&buf, south)[3] > 200);
        let clear = (0..EMBLEM_PX * EMBLEM_PX).filter(|i| atlas().rad[*i] < RING_IN - 0.06 && buf[i * 4 + 3] == 0).count();
        assert!(clear > 600, "no backing disc inside the ring: {clear} see-through texels");
        // White-hot spark right at the fill tip.
        let tip = ring_px(&draw(&EmblemFrame::idle(1, NEON[1], south, 0.4)), south);
        assert!(tip[0] > 240 && tip[1] > 240 && tip[2] > 240 && tip[3] == 255, "{tip:?}");
    }

    #[test]
    fn emblem_animates_across_time_pop_action_pop_and_hit_shake() {
        let total = EMBLEM_PX * EMBLEM_PX;
        for (r, neon) in NEON.iter().enumerate() {
            let rest = EmblemFrame::idle(r, *neon, 0.5, 2.0);
            let base = draw(&rest);
            assert_eq!(base, draw(&rest), "rank {r}: deterministic");
            // Living idle motion: rotating ticks + sheen sweep.
            let later = draw(&EmblemFrame { time: 2.6, ..rest });
            assert!(diff(&base, &later) > 300, "rank {r}: idle motion {}", diff(&base, &later));
            // Rank-up slam: white flash, shockwave ring, blade slash.
            let slam = draw(&EmblemFrame { pop: 0.9, ..rest });
            assert!(diff(&base, &slam) > total / 8 && light(&slam) > light(&base) * 1.25, "rank {r}: slam");
            let fading = draw(&EmblemFrame { pop: 0.4, ..rest });
            assert!(diff(&slam, &fading) > total / 10 && diff(&base, &fading) > 1500, "rank {r}: shockwave travels");
            // Action pulse: the gauge surges brighter.
            let pulse = draw(&EmblemFrame { action_pop: 1.0, ..rest });
            assert!(diff(&base, &pulse) > 3000 && light(&pulse) > light(&base) * 1.04, "rank {r}: action pulse");
            // Take-hit: crimson stain + torn scanlines.
            let hit = draw(&EmblemFrame { hit_shake: 1.0, ..rest });
            assert!(diff(&base, &hit) > total / 8, "rank {r}: hit glitch");
            let red_lead = |buf: &[u8]| buf.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).map(|p| p[0] as i64 * 2 - p[1] as i64 - p[2] as i64).sum::<i64>();
            assert!(red_lead(&hit) > red_lead(&base) + 200_000, "rank {r}: crimson tint {} vs {}", red_lead(&hit), red_lead(&base));
            // Corners stay transparent through every effect.
            for buf in [&later, &slam, &fading, &pulse, &hit] {
                assert_eq!((px(buf, 2, 2), px(buf, EMBLEM_PX - 3, EMBLEM_PX - 3)), ([0; 4], [0; 4]), "rank {r}");
            }
        }
        // The sheen glint sweeps across the lettering once per period.
        let sheen: Vec<f64> = (0..29).map(|i| light(&draw(&EmblemFrame::idle(3, NEON[3], 0.0, i as f32 * 0.1)))).collect();
        let (lo, hi) = (sheen.iter().copied().fold(f64::MAX, f64::min), sheen.iter().copied().fold(0.0, f64::max));
        assert!(hi > lo * 1.03, "sheen sweep: {lo} .. {hi}");
    }

    #[test]
    fn electric_arcs_orbit_only_at_overclock_ranks() {
        // White-hot texels in the outer orbit (beyond the ring and tick notches' neon).
        let bolts = |r: usize| {
            (0..24)
                .map(|i| {
                    let buf = draw(&EmblemFrame::idle(r, NEON[r], 0.0, i as f32 * 0.21));
                    (0..EMBLEM_PX * EMBLEM_PX).filter(|i| atlas().rad[*i] > 0.93 && atlas().rad[*i] < 0.99 && atlas().ranks[r][*i][3] == 0).filter(|i| buf[i * 4..i * 4 + 3].iter().all(|c| *c > 236) && buf[i * 4 + 3] > 200).count()
                })
                .sum::<usize>()
        };
        let counts: Vec<usize> = (0..EMBLEM_RANKS).map(bolts).collect();
        assert!(counts[..4].iter().all(|c| *c == 0), "no arcs below S: {counts:?}");
        assert!(counts[4] > 20 && counts[5] > counts[4] && counts[6] > counts[5], "S < SS < SSS crackle: {counts:?}");
    }

    #[test]
    fn render_survives_garbage_and_never_touches_a_wrong_sized_buffer() {
        let mut small = vec![7u8; 64];
        atlas().render(&EmblemFrame::idle(6, NEON[6], 1.0, 1.0), &mut small);
        assert!(small.iter().all(|b| *b == 7));
        let wild = EmblemFrame { rank: 99, color: [f32::NAN, 9.0, -3.0], fill: f32::INFINITY, time: f32::NAN, pop: f32::NAN, action_pop: -4.0, hit_shake: 77.0 };
        let buf = draw(&wild);
        assert!(buf.as_chunks::<4>().0.iter().any(|p| p[3] > 0), "clamped to SSS and still drawn");
        // Extreme clocks stay bounded too.
        for time in [-5.0, 0.0, 3599.99, 1.0e7] {
            let buf = draw(&EmblemFrame { pop: 0.5, action_pop: 0.5, hit_shake: 0.5, ..EmblemFrame::idle(5, NEON[5], 0.5, time) });
            assert_eq!(px(&buf, 0, 0), [0; 4]);
            assert!(buf.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count() > 5000, "t = {time}");
        }
    }
}
