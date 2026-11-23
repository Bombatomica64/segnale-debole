//! Live diegetic 160x240 pixel-art OLED screen texture rendered directly onto
//! the left-hand 3D tactical smartphone viewmodel.
//!
//! Designed for instant at-a-glance combat readability at 1280x720:
//! big high-contrast 5-bar signal meter, large 2x dBm & distance readouts,
//! bold 2px oscilloscope waveform, large directional radar arrow, chunky
//! color-coded `[RMB] PING` / `[Q] 5G` app status tiles, bold SOS bar, and
//! subtle in-screen Dead-Zone static snow.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::apps::{app_block_reason, ping_cooldown, PhoneApps, OVERLOAD_COOLDOWN, OVERLOAD_MIN_BARS, PING_MIN_BARS};
use crate::extraction::ExtractionState;
use crate::signal::SignalState;
use crate::state::{GameSet, PlayerModifiers};

pub const SCREEN_W: u32 = 160;
pub const SCREEN_H: u32 = 240;

/// Handle to the live RGBA8 phone OLED texture + redraw throttle timer.
#[derive(Resource, Clone)]
pub struct PhoneScreenTexture {
    pub image: Handle<Image>,
    pub timer: f32,
}

/// Pure data snapshot fed into [`render_phone_screen`] (keeps rendering testable).
#[derive(Debug, Clone)]
pub struct PhoneScreenSnapshot {
    pub bars: u8,
    pub raw_strength: f32,
    pub dbm: i32,
    pub in_dead_zone: bool,
    pub near_tower: bool,
    pub tower_dir_angle: f32,
    pub tower_distance: f32,
    pub ping_cd: f32,
    pub ping_max_cd: f32,
    pub overload_cd: f32,
    pub overload_max_cd: f32,
    pub ping_flash: f32,
    pub overload_flash: f32,
    pub sos_progress: Option<f32>,
    pub sos_jammed: bool,
    pub sos_in_ring: bool,
    pub sos_extracting: bool,
    pub time: f32,
}

impl Default for PhoneScreenSnapshot {
    fn default() -> Self {
        Self {
            bars: 0,
            raw_strength: 0.0,
            dbm: -118,
            in_dead_zone: true,
            near_tower: false,
            tower_dir_angle: 0.0,
            tower_distance: 42.0,
            ping_cd: 0.0,
            ping_max_cd: 4.0,
            overload_cd: 0.0,
            overload_max_cd: 10.0,
            ping_flash: 0.0,
            overload_flash: 0.0,
            sos_progress: None,
            sos_jammed: false,
            sos_in_ring: false,
            sos_extracting: false,
            time: 0.0,
        }
    }
}

pub struct PhoneScreenPlugin;

impl Plugin for PhoneScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, init_phone_screen_texture)
            .add_systems(Update, update_phone_screen.in_set(GameSet::Ui));
    }
}

fn init_phone_screen_texture(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut buf = vec![0u8; (SCREEN_W * SCREEN_H * 4) as usize];
    render_phone_screen(&mut buf, &PhoneScreenSnapshot::default());
    let mut img = Image::new(
        Extent3d { width: SCREEN_W, height: SCREEN_H, depth_or_array_layers: 1 },
        TextureDimension::D2,
        buf,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::nearest()
    });
    let handle = images.add(img);
    commands.insert_resource(PhoneScreenTexture { image: handle, timer: 0.0 });
}

#[allow(clippy::too_many_arguments)]
fn update_phone_screen(
    time: Res<Time>,
    signal: Res<SignalState>,
    apps: Res<PhoneApps>,
    mods: Res<PlayerModifiers>,
    extraction: Res<ExtractionState>,
    mut screen: ResMut<PhoneScreenTexture>,
    mut images: ResMut<Assets<Image>>,
) {
    screen.timer -= time.delta_secs();
    if screen.timer > 0.0 {
        return;
    }
    screen.timer = 1.0 / 24.0;
    let Some(mut img) = images.get_mut(&screen.image) else {
        return;
    };
    let (sos_progress, sos_jammed, sos_in_ring, sos_extracting) = match *extraction {
        ExtractionState::Uploading { progress, jammed, in_ring, .. } => (Some(progress), jammed, in_ring, false),
        ExtractionState::Extracting { .. } | ExtractionState::Complete => (Some(1.0), false, true, true),
        ExtractionState::Searching { .. } => (None, false, signal.near_tower, false),
    };
    let snap = PhoneScreenSnapshot {
        bars: signal.bars,
        raw_strength: signal.raw_strength,
        dbm: signal.dbm,
        in_dead_zone: signal.in_dead_zone,
        near_tower: signal.near_tower,
        tower_dir_angle: signal.tower_dir_angle,
        tower_distance: signal.tower_distance,
        ping_cd: apps.ping_cooldown,
        ping_max_cd: ping_cooldown(&mods).max(0.1),
        overload_cd: apps.overload_cooldown,
        overload_max_cd: OVERLOAD_COOLDOWN,
        ping_flash: apps.ping_active_timer,
        overload_flash: apps.overload_flash_timer,
        sos_progress,
        sos_jammed,
        sos_in_ring,
        sos_extracting,
        time: time.elapsed_secs(),
    };
    if let Some(data) = img.data.as_mut() {
        render_phone_screen(data, &snap);
    }
}

// ---------------------------------------------------------------------------
// Pixel framebuffer primitives
// ---------------------------------------------------------------------------

struct ScreenBuf<'a> {
    data: &'a mut [u8],
}

impl<'a> ScreenBuf<'a> {
    #[inline]
    fn px(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= SCREEN_W as i32 || y >= SCREEN_H as i32 {
            return;
        }
        let i = ((y as u32 * SCREEN_W + x as u32) * 4) as usize;
        if i + 3 < self.data.len() {
            self.data[i] = c[0];
            self.data[i + 1] = c[1];
            self.data[i + 2] = c[2];
            self.data[i + 3] = 255;
        }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], alpha: f32) {
        if x < 0 || y < 0 || x >= SCREEN_W as i32 || y >= SCREEN_H as i32 {
            return;
        }
        let i = ((y as u32 * SCREEN_W + x as u32) * 4) as usize;
        if i + 3 < self.data.len() {
            let a = alpha.clamp(0.0, 1.0);
            let inv = 1.0 - a;
            self.data[i] = (self.data[i] as f32 * inv + c[0] as f32 * a) as u8;
            self.data[i + 1] = (self.data[i + 1] as f32 * inv + c[1] as f32 * a) as u8;
            self.data[i + 2] = (self.data[i + 2] as f32 * inv + c[2] as f32 * a) as u8;
            self.data[i + 3] = 255;
        }
    }

    fn clear(&mut self, c: [u8; 3]) {
        for chunk in self.data.as_chunks_mut::<4>().0 {
            chunk[0] = c[0];
            chunk[1] = c[1];
            chunk[2] = c[2];
            chunk[3] = 255;
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3]) {
        for py in y..(y + h) {
            for px in x..(x + w) {
                self.px(px, py, c);
            }
        }
    }

    fn rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3]) {
        for px in x..(x + w) {
            self.px(px, y, c);
            self.px(px, y + h - 1, c);
        }
        for py in y..(y + h) {
            self.px(x, py, c);
            self.px(x + w - 1, py, c);
        }
    }

    fn rect_thick_outline(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3]) {
        self.rect_outline(x, y, w, h, c);
        self.rect_outline(x + 1, y + 1, w - 2, h - 2, c);
    }

    fn line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.px(x0, y0, c);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    fn thick_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
        for ox in -1i32..=1 {
            for oy in -1i32..=1 {
                if ox.abs() + oy.abs() <= 1 {
                    self.line(x0 + ox, y0 + oy, x1 + ox, y1 + oy, c);
                }
            }
        }
    }

    fn circle_outline(&mut self, cx: i32, cy: i32, r: i32, c: [u8; 3]) {
        let mut x = r;
        let mut y = 0;
        let mut err = 1 - r;
        while x >= y {
            for (dx, dy) in [(x, y), (y, x), (-y, x), (-x, y), (-x, -y), (-y, -x), (y, -x), (x, -y)] {
                self.px(cx + dx, cy + dy, c);
            }
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, scale: i32, c: [u8; 3]) {
        let mut cx = x;
        for ch in s.chars() {
            let g = glyph(ch.to_ascii_uppercase());
            for (row, &bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if (bits >> (4 - col)) & 1 != 0 {
                        self.rect(cx + col * scale, y + row as i32 * scale, scale, scale, c);
                    }
                }
            }
            cx += 6 * scale;
        }
    }
}

fn accent_rgb(bars: u8) -> [u8; 3] {
    match bars {
        0..=1 => [255, 58, 44],
        2..=3 => [255, 192, 40],
        _ => [50, 255, 200],
    }
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// Render the entire 160x240 high-legibility tactical smartphone OLED display.
pub fn render_phone_screen(rgba: &mut [u8], s: &PhoneScreenSnapshot) {
    let mut g = ScreenBuf { data: rgba };
    let accent = if s.overload_flash > 0.0 || s.ping_flash > 0.0 {
        [80, 250, 255]
    } else {
        accent_rgb(s.bars)
    };
    let bg = if s.in_dead_zone { [14, 5, 6] } else { [4, 11, 14] };
    let panel_bg = if s.in_dead_zone { [24, 9, 10] } else { [8, 19, 23] };
    let dim = [110, 128, 134];
    let bright = [242, 250, 248];
    let red = [255, 65, 50];
    let cyan = [65, 242, 255];
    let green = [65, 255, 190];
    let amber = [255, 192, 42];

    g.clear(bg);

    // ---- 1. Top Status Bar (y = 0..22) ----
    g.rect(0, 0, SCREEN_W as i32, 22, [10, 20, 24]);
    g.line(0, 22, SCREEN_W as i32 - 1, 22, [38, 66, 74]);
    let carrier = if s.sos_jammed {
        "JAMMED"
    } else if s.bars >= 4 {
        "5G LOCK"
    } else if s.bars >= 1 {
        "LTE"
    } else {
        "NO SVC"
    };
    let carrier_c = if s.sos_jammed { red } else { accent };
    g.text(6, 7, carrier, 1, carrier_c);
    // Bold bar count + battery top-right.
    g.text(90, 5, &format!("{}/5", s.bars), 2, bright);
    g.rect_outline(134, 6, 18, 10, dim);
    g.rect(152, 8, 2, 6, dim);
    g.rect(136, 8, 14, 6, green);

    // ---- 2. BIG Signal Bars + Large 2x dBm Readout (y = 26..84) ----
    g.rect(5, 26, 150, 58, panel_bg);
    g.rect_outline(5, 26, 150, 58, [34, 58, 66]);
    let flicker = s.in_dead_zone && (s.time * 24.0).sin() > 0.65;
    for i in 0..5u8 {
        let bx = 12 + i as i32 * 14;
        let bh = 12 + i as i32 * 8;
        let by = 78 - bh;
        let on = i < s.bars && !flicker;
        let bc = if on { accent } else { [30, 40, 44] };
        g.rect(bx, by, 10, bh, bc);
        if on {
            g.rect(bx, by, 10, 3, bright);
        }
    }
    // Large 2x dBm number right beside the bars.
    let dbm_str = format!("{}", s.dbm);
    g.text(88, 34, &dbm_str, 2, accent);
    g.text(94, 54, "DBM", 2, dim);

    // ---- 3. Bold 2px Oscilloscope Waveform (y = 88..112) ----
    g.rect(5, 88, 150, 24, [4, 9, 11]);
    g.rect_outline(5, 88, 150, 24, [28, 52, 58]);
    let mut prev_y = 100;
    let seed_t = (s.time * 18.0) as u32;
    for dx in 0..144i32 {
        let x = 8 + dx;
        let norm = dx as f32 / 144.0;
        let wy = if s.in_dead_zone {
            let n = (hash_u32(seed_t.wrapping_mul(131) ^ (dx as u32).wrapping_mul(17)) & 0xFF) as f32 / 255.0;
            let spike = if n > 0.85 { (n - 0.5) * 18.0 } else { (n - 0.5) * 6.0 };
            100 + spike as i32
        } else {
            let freq = 2.0 + s.bars as f32 * 1.1;
            let amp = 4.0 + s.bars as f32 * 1.4;
            let wave = ((norm * freq * std::f32::consts::TAU) - s.time * (5.0 + s.bars as f32 * 1.6)).sin();
            let env = (norm * std::f32::consts::PI).sin().powf(0.45);
            100 - (wave * amp * env) as i32
        };
        let wy = wy.clamp(91, 109);
        if dx > 0 {
            g.line(x - 1, prev_y, x, wy, accent);
            g.line(x - 1, prev_y + 1, x, wy + 1, accent);
        }
        prev_y = wy;
    }

    // ---- 4. Big High-Contrast Directional Radar + 2x Distance (y = 116..166) ----
    g.rect(5, 116, 150, 50, panel_bg);
    g.rect_outline(5, 116, 150, 50, [34, 58, 66]);
    let rcx = 34;
    let rcy = 141;
    g.circle_outline(rcx, rcy, 20, [44, 86, 92]);
    g.circle_outline(rcx, rcy, 19, [30, 62, 68]);
    g.line(rcx - 20, rcy, rcx + 20, rcy, [20, 42, 46]);
    g.line(rcx, rcy - 20, rcx, rcy + 20, [20, 42, 46]);
    if s.bars == 0 {
        let ang = s.time * 4.5;
        let tx = rcx + (ang.sin() * 17.0) as i32;
        let ty = rcy - (ang.cos() * 17.0) as i32;
        g.thick_line(rcx, rcy, tx, ty, red);
        g.text(64, 124, "NO", 2, red);
        g.text(64, 144, "LOCK", 2, red);
    } else {
        let ang = s.tower_dir_angle;
        let dir_x = ang.sin();
        let dir_y = -ang.cos();
        let tip_x = rcx + (dir_x * 16.0) as i32;
        let tip_y = rcy + (dir_y * 16.0) as i32;
        let tail_x = rcx - (dir_x * 7.0) as i32;
        let tail_y = rcy - (dir_y * 7.0) as i32;
        g.thick_line(tail_x, tail_y, tip_x, tip_y, accent);
        let left_ang = ang - 2.4;
        let right_ang = ang + 2.4;
        let lx = tip_x + (left_ang.sin() * 8.0) as i32;
        let ly = tip_y - (left_ang.cos() * 8.0) as i32;
        let rx = tip_x + (right_ang.sin() * 8.0) as i32;
        let ry = tip_y - (right_ang.cos() * 8.0) as i32;
        g.thick_line(tip_x, tip_y, lx, ly, bright);
        g.thick_line(tip_x, tip_y, rx, ry, bright);

        if s.near_tower {
            g.text(64, 124, "RING!", 2, green);
            let hint = if s.sos_progress.is_some() { "HOLD" } else { "[E]SOS" };
            g.text(64, 145, hint, 2, cyan);
        } else {
            let d = s.tower_distance.round().min(999.0) as i32;
            g.text(64, 124, &format!("{d}M"), 2, accent);
            let dir_lbl = if ang.abs() < 0.35 {
                "AHEAD"
            } else if ang.abs() > 2.6 {
                "BACK"
            } else if ang > 0.0 {
                "RIGHT"
            } else {
                "LEFT"
            };
            g.text(64, 146, dir_lbl, 2, bright);
        }
    }

    // ---- 5. Chunky At-a-Glance App Tiles (y = 170..212) ----
    draw_app_tile(
        &mut g,
        5,
        170,
        73,
        42,
        "RMB PNG",
        s.bars,
        PING_MIN_BARS,
        s.ping_cd,
        s.ping_max_cd,
        s.ping_flash > 0.0,
    );
    draw_app_tile(
        &mut g,
        82,
        170,
        73,
        42,
        "Q  EMP",
        s.bars,
        OVERLOAD_MIN_BARS,
        s.overload_cd,
        s.overload_max_cd,
        s.overload_flash > 0.0,
    );

    // ---- 6. Bold Bottom SOS Status Bar (y = 216..236) ----
    g.rect(5, 216, 150, 20, [8, 14, 18]);
    g.rect_outline(5, 216, 150, 20, [36, 62, 70]);
    if s.sos_extracting {
        g.rect(7, 218, 146, 16, [18, 130, 140]);
        g.text(14, 219, "EXTRACT!", 2, bright);
    } else if let Some(p) = s.sos_progress {
        let fill_w = (p.clamp(0.0, 1.0) * 146.0) as i32;
        let fill_c = if s.sos_jammed {
            [200, 28, 24]
        } else if s.sos_in_ring {
            [20, 140, 155]
        } else {
            [175, 115, 16]
        };
        if fill_w > 0 {
            g.rect(7, 218, fill_w, 16, fill_c);
        }
        let pct = (p * 100.0).round() as i32;
        let lbl = if s.sos_jammed {
            format!("JAM! {pct}%")
        } else {
            format!("SOS {pct}%")
        };
        g.text(28, 219, &lbl, 2, bright);
    } else {
        let (msg, col) = if s.near_tower { ("PRESS [E]", green) } else { ("FIND 5-BAR", amber) };
        g.text(22, 219, msg, 2, col);
    }

    // ---- 7. Subtle In-Screen Dead-Zone Static + Cracked Glass ----
    if s.in_dead_zone {
        let frame = (s.time * 28.0) as u32;
        let specks: u32 = if s.bars == 0 { 160 } else { 85 };
        for i in 0..specks {
            let h = hash_u32(frame.wrapping_mul(997) ^ i.wrapping_mul(613));
            let sx = (h % SCREEN_W) as i32;
            let sy = ((h >> 8) % SCREEN_H) as i32;
            let c = if (h >> 16) & 3 == 0 { [255, 70, 60] } else { [200, 210, 215] };
            g.blend(sx, sy, c, 0.45);
        }
    }
    g.line(130, 2, 147, 19, [75, 92, 96]);
    g.line(147, 19, 158, 27, [60, 75, 80]);

    // Active Ping / EMP cyan frame pulse.
    if s.ping_flash > 0.0 || s.overload_flash > 0.0 {
        g.rect_thick_outline(1, 1, SCREEN_W as i32 - 2, SCREEN_H as i32 - 2, cyan);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_app_tile(
    g: &mut ScreenBuf<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    title: &str,
    bars: u8,
    min_bars: u8,
    cd: f32,
    max_cd: f32,
    flashing: bool,
) {
    let blocked = app_block_reason(bars, min_bars, cd);
    let ready = blocked.is_none();
    let (bg, border, title_c, status_c) = if flashing {
        ([22, 86, 98], [120, 255, 255], [245, 255, 255], [160, 255, 255])
    } else if ready {
        ([12, 48, 52], [65, 255, 210], [235, 255, 250], [75, 255, 200])
    } else if bars < min_bars {
        ([34, 11, 13], [150, 42, 36], [190, 140, 138], [255, 76, 62])
    } else {
        ([18, 26, 30], [95, 130, 140], [195, 210, 215], [255, 198, 48])
    };
    g.rect(x, y, w, h, bg);
    // Chunky bottom progress bar (full bright green/cyan when ready, filling on CD).
    if cd > 0.0 && max_cd > 0.0 {
        let frac = (1.0 - (cd / max_cd).clamp(0.0, 1.0)).clamp(0.0, 1.0);
        let fw = ((w - 6) as f32 * frac) as i32;
        g.rect(x + 3, y + h - 8, fw, 5, [70, 210, 225]);
    } else if ready {
        g.rect(x + 3, y + h - 8, w - 6, 5, [65, 255, 205]);
    }
    g.rect_thick_outline(x, y, w, h, border);
    g.text(x + 6, y + 5, title, 1, title_c);
    // Large 2x status so player reads READY / 1B / 3B / 2.3 at a single peripheral glance!
    let status = match blocked {
        None => "READY".to_string(),
        Some(_) if bars < min_bars => format!("NO {min_bars}B"),
        Some(_) => format!("{cd:.1}S"),
    };
    g.text(x + 6, y + 17, &status, 2, status_c);
}

// ---------------------------------------------------------------------------
// Crisp 5x7 uppercase bitmap font
// ---------------------------------------------------------------------------

fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        '+' => [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        ':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100],
        '?' => [0b01110, 0b10001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100],
        '%' => [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011],
        '[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
        ']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
        '<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        '>' => [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_screen_renders_distinct_states() {
        let mut dead = vec![0u8; (SCREEN_W * SCREEN_H * 4) as usize];
        let mut full = vec![0u8; (SCREEN_W * SCREEN_H * 4) as usize];
        render_phone_screen(&mut dead, &PhoneScreenSnapshot::default());
        render_phone_screen(
            &mut full,
            &PhoneScreenSnapshot {
                bars: 5,
                raw_strength: 5.0,
                dbm: -55,
                in_dead_zone: false,
                near_tower: true,
                tower_dir_angle: 0.4,
                tower_distance: 6.0,
                sos_progress: Some(0.55),
                sos_in_ring: true,
                time: 2.5,
                ..Default::default()
            },
        );
        assert_ne!(dead, full);
        assert!(full.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
    }
}
