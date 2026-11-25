//! Live diegetic 160x240 pixel-art OLED screen texture rendered directly onto
//! the left-hand 3D tactical smartphone viewmodel.
//!
//! Designed for instant at-a-glance combat readability at 1280x720:
//! big high-contrast 5-bar signal meter, large 2x dBm & distance readouts,
//! bold 2px oscilloscope waveform, large directional radar arrow, chunky
//! color-coded `[RMB] PING` / `[Q] 5G` app status tiles, bold SOS bar, and
//! subtle in-screen Dead-Zone static snow.

use std::fmt::Write as _;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::apps::{app_block_reason, overload_cooldown, ping_cooldown, PhoneApps, OVERLOAD_MIN_BARS, PING_MIN_BARS};
use crate::extraction::ExtractionState;
use crate::audio::{SfxEvent, SfxQueue};
use crate::fx::HudFeed;
use crate::signal::SignalState;
use crate::state::{AppState, GameSet, PlayerModifiers, RunMode, RunStats};

pub const SCREEN_W: u32 = 160;
pub const SCREEN_H: u32 = 240;
/// Carrier-label colors for the Signal Spoofer / Jammer Hijack overrides.
pub const SPOOF_CYAN: [u8; 3] = [90, 255, 255];
pub const HIJACK_EMERALD: [u8; 3] = [70, 255, 160];

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
    /// Store & Forward blackout packets waiting to fire (0..=3).
    pub queued_packets: u8,
    /// Emergency Callback armed and the Overload is off cooldown.
    pub callback_ready: bool,
    /// Missed Call: Ping at 0 bars would drop a decoy right now.
    pub decoy_ready: bool,
    /// Signal Spoofer active: carrier label reads "SPOOF" in bright cyan.
    pub spoof_active: bool,
    /// Standing in a Jammer Hijack hotspot: carrier label reads "HIJK".
    pub hijack_boost: bool,
    /// Controller is the active input: tiles read `LT PNG` / `RB EMP` and the
    /// tower prompt `PRESS [X]` instead of `RMB` / `Q` / `[E]`.
    pub pad_prompts: bool,
    /// Active diegetic notification banner (drawn over the radar panel).
    pub notif: Option<(NotifKind, &'static str)>,
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
            queued_packets: 0,
            callback_ready: false,
            decoy_ready: false,
            spoof_active: false,
            hijack_boost: false,
            pad_prompts: false,
            notif: None,
        }
    }
}

/// Ping / 5G Overload tile labels per input mode.
pub const fn app_tile_labels(pad: bool) -> (&'static str, &'static str) {
    if pad { ("LT PNG", "RB EMP") } else { ("RMB PNG", "Q  EMP") }
}

pub struct PhoneScreenPlugin;

impl Plugin for PhoneScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhoneNotifState>()
            .add_systems(PreStartup, init_phone_screen_texture)
            .add_systems(OnEnter(AppState::Playing), deploy_cold_open)
            .add_systems(Update, (drive_phone_notifs, update_phone_screen).chain().in_set(GameSet::Ui));
        for s in [
            AppState::MainMenu,
            AppState::Loadout,
            AppState::ContractsFeats,
            AppState::Leaderboard,
            AppState::Compendium,
            AppState::GameOver,
            AppState::Victory,
        ] {
            app.add_systems(OnEnter(s), reset_phone_notifs);
        }
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
    mode: Option<Res<crate::gamepad::InputMode>>,
    notifs: Res<PhoneNotifState>,
    mut screen: ResMut<PhoneScreenTexture>,
    mut images: ResMut<Assets<Image>>,
    (mut frame, mut shown): (Local<Vec<u8>>, Local<Vec<u8>>),
) {
    screen.timer -= time.delta_secs();
    if screen.timer > 0.0 {
        return;
    }
    screen.timer = 1.0 / 24.0;
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
        overload_max_cd: overload_cooldown(&mods).max(0.1),
        ping_flash: apps.ping_active_timer,
        overload_flash: apps.overload_flash_timer,
        sos_progress,
        sos_jammed,
        sos_in_ring,
        sos_extracting,
        time: time.elapsed_secs(),
        queued_packets: apps.queued_packets,
        callback_ready: apps.callback_ready(),
        decoy_ready: signal.bars == 0 && mods.missed_call && apps.ping_cooldown <= 0.0,
        spoof_active: apps.spoof_active(),
        hijack_boost: signal.hotspot_boosted,
        pad_prompts: mode.is_some_and(|m| m.is_pad()),
        notif: notifs.banner(),
    };
    // Render off-texture; only touch the image (which marks it for GPU
    // re-upload) when the pixels actually differ from the last upload.
    frame.resize((SCREEN_W * SCREEN_H * 4) as usize, 0);
    render_phone_screen(&mut frame, &snap);
    if *frame == *shown {
        return;
    }
    let Some(mut img) = images.get_mut(&screen.image) else {
        return;
    };
    if let Some(data) = img.data.as_mut() {
        data.copy_from_slice(&frame);
        shown.clone_from(&frame);
    }
}

// ---------------------------------------------------------------------------
// Milestone 30: diegetic phone notifications
// ---------------------------------------------------------------------------

/// Ambient carrier texts: one per climb back to 2+ bars, never repeated in a run.
pub const AMBIENT_NOTIFS: [&str; 8] = [
    "ROAMING: HELL IS OUTSIDE EU ZONE (19.99 EUR/MB)",
    "MOM (14 MISSED): HAI MANGIATO?",
    "TIM-MARS: 80% OF YOUR 5G DATA USED",
    "UAC-HR: Q3 FIRE SAFETY TRAINING OVERDUE",
    "2% BATTERY // LOW POWER? [SHOOTING DEMONS]",
    "WIFI: HELL_GUEST_5G (NO INTERNET)",
    "CARRIER UPDATE: RESTART PHONE NOW",
    "DELIVEROO MARS: RIDER LOST IN SECTOR 7",
];
/// Carrier Daemon arrival: the phone rings.
pub const INCOMING_CALL_TEXT: &str = "INCOMING CALL // SPAM LIKELY";
/// Carrier Daemon killed.
pub const CALL_DECLINED_TEXT: &str = "CALL DECLINED.";
/// Deploy cold open flash.
pub const SEARCHING_TEXT: &str = "SEARCHING...";
/// Minimum seconds between two ambient carrier texts.
pub const AMBIENT_COOLDOWN: f32 = 14.0;
/// Quiet period after a deploy before the first ambient text may land.
pub const DEPLOY_QUIET_SECS: f32 = 6.0;
/// Bars a climb must reach (from below) to trigger an ambient text.
pub const NOTIF_MIN_BARS: u8 = 2;
/// On-screen seconds per banner kind.
pub const AMBIENT_SECS: f32 = 4.5;
pub const CALL_SECS: f32 = 6.0;
pub const DECLINED_SECS: f32 = 3.5;
pub const SEARCHING_SECS: f32 = 1.6;
/// Characters per line of the 1x banner body (140 px / 6 px glyphs).
pub const BANNER_LINE_CHARS: usize = 23;
/// HUD toast colors for phone texts (cyan ambient, amber call, acid-green decline).
pub const NOTIF_CYAN: Color = Color::srgb(0.35, 0.95, 1.0);
pub const NOTIF_AMBER: Color = Color::srgb(1.0, 0.72, 0.18);
pub const NOTIF_ACID: Color = Color::srgb(0.55, 1.0, 0.25);

/// Floor sector subtitles for the deploy cold open (Floors 1-5).
pub const SECTOR_SUBTITLES: [&str; 5] = [
    "SECTOR 01 // BASEMENT GARAGE",
    "SECTOR 02 // SERVER ANNEX",
    "SECTOR 03 // COOLING CORE",
    "SECTOR 04 // BLACKOUT VAULT",
    "SECTOR 05 // ROOFTOP RELAY",
];

/// Deploy subtitle of `floor` (Endless floors get an Overdrive sector tag).
pub fn sector_subtitle(floor: u32) -> String {
    match floor {
        1..=5 => SECTOR_SUBTITLES[floor as usize - 1].to_string(),
        f => format!("SECTOR {f:02} // OVERDRIVE DEPTHS"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifKind {
    /// Cyan carrier / family / HR text.
    Ambient,
    /// Pulsing red / amber Carrier Daemon call.
    IncomingCall,
    /// Acid-green boss kill.
    CallDeclined,
    /// Deploy cold open "SEARCHING..." flash.
    Searching,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhoneNotif {
    pub kind: NotifKind,
    pub text: &'static str,
    pub timer: f32,
}

/// Non-blocking diegetic notification state (one banner at a time, ambient
/// texts rate limited and never repeated within a run). Reset on every run
/// start / teardown.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct PhoneNotifState {
    pub active: Option<PhoneNotif>,
    /// Seconds until the next ambient text may land.
    pub cooldown: f32,
    /// Shuffled ambient order for this run, consumed front to back.
    order: [u8; 8],
    next: usize,
    prev_bars: u8,
    boss_active: bool,
    boss_kills: u32,
    /// `(run seed, floor)` of the last deploy cold open.
    pub deployed: Option<(u64, u32)>,
    /// Ambient texts already shown this run (in order).
    pub sent: Vec<&'static str>,
}

impl Default for PhoneNotifState {
    fn default() -> Self {
        PhoneNotifState::for_seed(0)
    }
}

impl PhoneNotifState {
    /// Fresh state with the ambient order shuffled deterministically by `seed`.
    pub fn for_seed(seed: u64) -> Self {
        let mut order = [0u8, 1, 2, 3, 4, 5, 6, 7];
        let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
        for i in (1..order.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            order.swap(i, (x % (i as u64 + 1)) as usize);
        }
        PhoneNotifState {
            active: None,
            cooldown: 0.0,
            order,
            next: 0,
            prev_bars: 0,
            boss_active: false,
            boss_kills: 0,
            deployed: None,
            sent: Vec::new(),
        }
    }

    /// Banner currently shown on the phone.
    pub fn banner(&self) -> Option<(NotifKind, &'static str)> {
        self.active.map(|n| (n.kind, n.text))
    }

    fn show(&mut self, kind: NotifKind, text: &'static str, secs: f32) {
        self.active = Some(PhoneNotif { kind, text, timer: secs });
    }

    /// Advance timers by `dt` seconds.
    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.cooldown = (self.cooldown - dt).max(0.0);
        if let Some(n) = &mut self.active {
            n.timer -= dt;
            if n.timer <= 0.0 {
                self.active = None;
            }
        }
    }

    /// Feed this frame's bars; returns the ambient text that just landed (if
    /// any): a climb from `< 2` to `>= 2` bars, off cooldown, with an unused
    /// text left and no boss call on screen.
    pub fn on_bars(&mut self, bars: u8) -> Option<&'static str> {
        let climbed = self.prev_bars < NOTIF_MIN_BARS && bars >= NOTIF_MIN_BARS;
        self.prev_bars = bars;
        let call_up = matches!(self.active, Some(PhoneNotif { kind: NotifKind::IncomingCall | NotifKind::CallDeclined, .. }));
        if !climbed || self.cooldown > 0.0 || call_up || self.next >= self.order.len() {
            return None;
        }
        let text = AMBIENT_NOTIFS[self.order[self.next] as usize];
        self.next += 1;
        self.cooldown = AMBIENT_COOLDOWN;
        self.sent.push(text);
        self.show(NotifKind::Ambient, text, AMBIENT_SECS);
        Some(text)
    }

    /// Carrier Daemon spawned: the banner switches to the incoming call.
    pub fn incoming_call(&mut self) -> &'static str {
        self.show(NotifKind::IncomingCall, INCOMING_CALL_TEXT, CALL_SECS);
        INCOMING_CALL_TEXT
    }

    /// Carrier Daemon killed.
    pub fn call_declined(&mut self) -> &'static str {
        self.show(NotifKind::CallDeclined, CALL_DECLINED_TEXT, DECLINED_SECS);
        CALL_DECLINED_TEXT
    }

    /// Deploy cold open: brief "SEARCHING..." flash and a quiet period.
    pub fn searching(&mut self) {
        self.show(NotifKind::Searching, SEARCHING_TEXT, SEARCHING_SECS);
        self.cooldown = self.cooldown.max(DEPLOY_QUIET_SECS);
        self.prev_bars = 0;
    }

    /// Feed the boss HUD flag + Carrier Daemon kill count; returns the boss
    /// banner that just switched on (call on spawn, decline on a kill).
    pub fn on_boss(&mut self, active: bool, daemon_kills: u32) -> Option<NotifKind> {
        let mut out = None;
        if active && !self.boss_active {
            self.incoming_call();
            out = Some(NotifKind::IncomingCall);
        }
        if daemon_kills > self.boss_kills {
            self.call_declined();
            out = Some(NotifKind::CallDeclined);
        }
        self.boss_active = active;
        self.boss_kills = daemon_kills;
        out
    }
}

/// Greedy word wrap of a banner body into lines of at most `width` chars
/// (over-long words are hard-split at UTF-8 character boundaries).
pub fn wrap_banner(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_chars = 0usize;
    for mut word in text.split_whitespace() {
        let mut word_chars = word.chars().count();
        while word_chars > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_chars = 0;
            }
            let split_byte = word.char_indices().nth(width).map_or(word.len(), |(i, _)| i);
            let (a, b) = word.split_at(split_byte);
            lines.push(a.to_string());
            word = b;
            word_chars -= width;
        }
        if cur.is_empty() {
            cur.push_str(word);
            cur_chars = word_chars;
        } else if cur_chars + 1 + word_chars <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_chars += 1 + word_chars;
        } else {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
            cur_chars = word_chars;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn reset_phone_notifs(mut notifs: ResMut<PhoneNotifState>) {
    *notifs = PhoneNotifState::default();
}

/// Deploy cold open on a fresh floor (not Time Trial, not a resume): tactile
/// deploy cue, "SEARCHING..." phone flash and the witty sector subtitle.
fn deploy_cold_open(run: Res<RunStats>, mut notifs: ResMut<PhoneNotifState>, mut sfx: ResMut<SfxQueue>, mut feed: ResMut<HudFeed>) {
    let key = (run.run_seed, run.floor);
    if notifs.deployed == Some(key) {
        return;
    }
    if notifs.deployed.is_none_or(|(seed, _)| seed != run.run_seed) {
        *notifs = PhoneNotifState::for_seed(run.run_seed);
    }
    notifs.deployed = Some(key);
    if run.mode == RunMode::TimeTrial {
        return;
    }
    notifs.searching();
    sfx.play(SfxEvent::WeaponSwap);
    sfx.play(SfxEvent::PhonePing);
    feed.toast(sector_subtitle(run.floor), NOTIF_AMBER);
}

/// Per-frame notification driver: timers, ambient texts on a signal climb,
/// Carrier Daemon call / decline.
fn drive_phone_notifs(
    time: Res<Time>,
    signal: Res<SignalState>,
    run: Res<RunStats>,
    boss: Option<Res<crate::boss::BossHud>>,
    mut notifs: ResMut<PhoneNotifState>,
    mut sfx: ResMut<SfxQueue>,
    mut feed: ResMut<HudFeed>,
) {
    notifs.tick(time.delta_secs());
    let daemon = run.enemy_kills[crate::enemies::EnemyKind::CarrierDaemon.index()];
    match notifs.on_boss(boss.is_some_and(|b| b.active), daemon) {
        Some(NotifKind::IncomingCall) => {
            sfx.play(SfxEvent::PhonePing);
            feed.toast(format!("PHONE // {INCOMING_CALL_TEXT}"), NOTIF_AMBER);
        }
        Some(NotifKind::CallDeclined) => {
            feed.toast(format!("PHONE // {CALL_DECLINED_TEXT}"), NOTIF_ACID);
        }
        _ => {}
    }
    // Time Trial runs stay clean: no carrier texts over the speedrun HUD.
    let bars = if run.mode == RunMode::TimeTrial { 0 } else { signal.bars };
    if let Some(text) = notifs.on_bars(bars) {
        sfx.play(SfxEvent::PhonePing);
        feed.toast(format!("SMS // {text}"), NOTIF_CYAN);
    }
}

/// Phone banner card over the radar panel (y = 116..166).
fn draw_notif_banner(g: &mut ScreenBuf<'_>, kind: NotifKind, text: &str, time: f32) {
    let (x, y, w, h) = (5, 116, 150, 50);
    let bright = [242, 250, 248];
    match kind {
        NotifKind::Ambient => {
            g.rect(x, y, w, h, [6, 28, 36]);
            g.rect_thick_outline(x, y, w, h, [65, 242, 255]);
            g.rect(x + 2, y + 2, w - 4, 10, [20, 92, 108]);
            g.text(x + 6, y + 4, "NEW MESSAGE", 1, bright);
            for (i, line) in wrap_banner(text, BANNER_LINE_CHARS).iter().take(3).enumerate() {
                g.text(x + 5, y + 16 + i as i32 * 10, line, 1, [190, 248, 255]);
            }
        }
        NotifKind::IncomingCall => {
            let red = (time * 6.0).sin() > 0.0;
            let (bg, edge) = if red { ([96, 10, 8], [255, 70, 50]) } else { ([88, 58, 4], [255, 196, 48]) };
            g.rect(x, y, w, h, bg);
            g.rect_thick_outline(x, y, w, h, edge);
            let (head, body) = text.split_once(" // ").unwrap_or((text, ""));
            g.text(x + 6, y + 6, head, 1, bright);
            g.text(x + 8, y + 22, body, 2, edge);
        }
        NotifKind::CallDeclined => {
            g.rect(x, y, w, h, [16, 60, 8]);
            g.rect_thick_outline(x, y, w, h, [150, 255, 70]);
            let (a, b) = text.split_once(' ').unwrap_or((text, ""));
            g.text(x + 8, y + 6, a, 2, [190, 255, 110]);
            g.text(x + 8, y + 26, b, 2, bright);
        }
        NotifKind::Searching => {
            g.rect(x, y, w, h, [8, 16, 20]);
            g.rect_thick_outline(x, y, w, h, [255, 192, 42]);
            let dots = ((time * 4.0) as usize % 3) + 1;
            let shown = &text[..text.len() - 3 + dots.min(3)];
            g.text(x + 4, y + 18, shown, 2, [255, 214, 90]);
        }
    }
}

// ---------------------------------------------------------------------------
// Pixel framebuffer primitives
// ---------------------------------------------------------------------------

/// Tiny fixed-capacity string for OLED labels (no heap allocation).
struct StackStr {
    buf: [u8; 24],
    len: usize,
}

impl StackStr {
    fn new() -> Self {
        StackStr { buf: [0; 24], len: 0 }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl std::fmt::Write for StackStr {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

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
    let carrier = if s.spoof_active {
        "SPOOF"
    } else if s.hijack_boost {
        "HIJK"
    } else if s.sos_jammed {
        "JAMMED"
    } else if s.bars >= 4 {
        "5G LOCK"
    } else if s.bars >= 1 {
        "LTE"
    } else {
        "NO SVC"
    };
    let carrier_c = if s.spoof_active {
        SPOOF_CYAN
    } else if s.hijack_boost {
        HIJACK_EMERALD
    } else if s.sos_jammed {
        red
    } else {
        accent
    };
    g.text(6, 7, carrier, 1, carrier_c);
    // Store & Forward packet pips (electric cyan/violet, pulsing).
    for i in 0..s.queued_packets.min(3) as i32 {
        let pulse = ((s.time * 6.0 + i as f32).sin() * 0.5 + 0.5) > 0.5;
        let c = if pulse { [120, 235, 255] } else { [190, 110, 255] };
        g.rect(58 + i * 9, 7, 7, 7, c);
        g.rect_outline(57 + i * 9, 6, 9, 9, [60, 40, 120]);
    }
    // Bold bar count + battery top-right.
    let mut lbl = StackStr::new();
    let _ = write!(lbl, "{}/5", s.bars);
    g.text(90, 5, lbl.as_str(), 2, bright);
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
    let mut dbm_str = StackStr::new();
    let _ = write!(dbm_str, "{}", s.dbm);
    g.text(88, 34, dbm_str.as_str(), 2, accent);
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
            let mut dist = StackStr::new();
            let _ = write!(dist, "{d}M");
            g.text(64, 124, dist.as_str(), 2, accent);
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

    // Milestone 30: an active notification banner covers the radar panel.
    if let Some((kind, text)) = s.notif {
        draw_notif_banner(&mut g, kind, text, s.time);
    }

    // ---- 5. Chunky At-a-Glance App Tiles (y = 170..212) ----
    let (ping_lbl, emp_lbl) = app_tile_labels(s.pad_prompts);
    if s.decoy_ready {
        draw_special_tile(&mut g, 5, 170, 73, 42, ping_lbl, "DECOY", [44, 26, 4], [255, 196, 48], [255, 214, 90], s.time);
    } else {
        draw_app_tile(
        &mut g,
        5,
        170,
        73,
        42,
        ping_lbl,
        s.bars,
        PING_MIN_BARS,
        s.ping_cd,
        s.ping_max_cd,
        s.ping_flash > 0.0,
        );
    }
    if s.callback_ready {
        draw_special_tile(&mut g, 82, 170, 73, 42, emp_lbl, "CALL!", [40, 34, 6], [255, 222, 70], [120, 250, 255], s.time);
    } else {
        draw_app_tile(
        &mut g,
        82,
        170,
        73,
        42,
        emp_lbl,
        s.bars,
        OVERLOAD_MIN_BARS,
        s.overload_cd,
        s.overload_max_cd,
        s.overload_flash > 0.0,
        );
    }

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
        let mut lbl = StackStr::new();
        let _ = if s.sos_jammed { write!(lbl, "JAM! {pct}%") } else { write!(lbl, "SOS {pct}%") };
        g.text(28, 219, lbl.as_str(), 2, bright);
    } else {
        let (msg, col) = if s.near_tower { (if s.pad_prompts { "PRESS [X]" } else { "PRESS [E]" }, green) } else { ("FIND 5-BAR", amber) };
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

/// Highlighted upgrade-override tile (Missed Call DECOY / Emergency Callback CALL!).
#[allow(clippy::too_many_arguments)]
fn draw_special_tile(g: &mut ScreenBuf<'_>, x: i32, y: i32, w: i32, h: i32, title: &str, status: &str, bg: [u8; 3], border: [u8; 3], status_c: [u8; 3], time: f32) {
    let blink = (time * 8.0).sin() > 0.0;
    g.rect(x, y, w, h, bg);
    g.rect(x + 3, y + h - 8, w - 6, 5, if blink { border } else { status_c });
    g.rect_thick_outline(x, y, w, h, if blink { border } else { status_c });
    g.text(x + 6, y + 5, title, 1, [255, 245, 220]);
    g.text(x + 6, y + 17, status, 2, status_c);
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
    let mut status = StackStr::new();
    let _ = match blocked {
        None => write!(status, "READY"),
        Some(_) if bars < min_bars => write!(status, "NO {min_bars}B"),
        Some(_) => write!(status, "{cd:.1}S"),
    };
    g.text(x + 6, y + 17, status.as_str(), 2, status_c);
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
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        ',' => [0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000],
        '_' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111],
        '\'' => [0b00100, 0b00100, 0b01000, 0b00000, 0b00000, 0b00000, 0b00000],
        '&' => [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101],
        '#' => [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010],
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

    fn render(s: &PhoneScreenSnapshot) -> Vec<u8> {
        let mut buf = vec![0u8; (SCREEN_W * SCREEN_H * 4) as usize];
        render_phone_screen(&mut buf, s);
        buf
    }

    fn px(buf: &[u8], x: u32, y: u32) -> [u8; 3] {
        let i = ((y * SCREEN_W + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2]]
    }

    /// Clean (non-dead-zone) baseline so static snow never touches the probes.
    fn calm(bars: u8) -> PhoneScreenSnapshot {
        PhoneScreenSnapshot { bars, in_dead_zone: false, time: 0.1, ..Default::default() }
    }

    #[test]
    fn packet_pips_glow_in_status_bar() {
        let none = render(&calm(0));
        for n in 1..=3u8 {
            let b = render(&PhoneScreenSnapshot { queued_packets: n, ..calm(0) });
            for i in 0..3u32 {
                let lit = px(&b, 61 + i * 9, 10) != px(&none, 61 + i * 9, 10);
                assert_eq!(lit, i < n as u32, "{n} packets, pip {i}");
            }
        }
    }

    /// Pixels of the carrier label area (status bar, left of the packet pips).
    fn carrier_pixels(buf: &[u8]) -> Vec<[u8; 3]> {
        (6..15).flat_map(|y| (6..56).map(move |x| px(buf, x, y))).collect()
    }

    #[test]
    fn spoof_and_hijack_carrier_labels() {
        let base = render(&calm(0));
        let spoof = render(&PhoneScreenSnapshot { spoof_active: true, ..calm(4) });
        assert!(carrier_pixels(&spoof).contains(&SPOOF_CYAN), "SPOOF drawn in bright cyan");
        assert!(!carrier_pixels(&base).contains(&SPOOF_CYAN));
        let plain4 = render(&calm(4));
        assert_ne!(carrier_pixels(&spoof), carrier_pixels(&plain4), "label differs from 5G LOCK");
        let hijack = render(&PhoneScreenSnapshot { hijack_boost: true, ..calm(5) });
        assert!(carrier_pixels(&hijack).contains(&HIJACK_EMERALD), "HIJK badge drawn in emerald");
        assert!(!carrier_pixels(&hijack).contains(&SPOOF_CYAN));
        // Spoof wins when both are active.
        let both = render(&PhoneScreenSnapshot { spoof_active: true, hijack_boost: true, ..calm(5) });
        assert_eq!(carrier_pixels(&both), carrier_pixels(&render(&PhoneScreenSnapshot { spoof_active: true, ..calm(5) })));
    }

    #[test]
    fn app_tiles_follow_the_input_mode() {
        assert_eq!(app_tile_labels(false), ("RMB PNG", "Q  EMP"));
        assert_eq!(app_tile_labels(true), ("LT PNG", "RB EMP"));
        let tiles = |b: &[u8]| (170..212).flat_map(|y| (5..155).map(move |x| px(b, x, y))).collect::<Vec<_>>();
        let kbm = render(&calm(3));
        let pad = render(&PhoneScreenSnapshot { pad_prompts: true, ..calm(3) });
        assert_ne!(tiles(&kbm), tiles(&pad), "tile labels redraw for the controller");
        // Near the tower the prompt swaps [E] -> [X].
        let near = |pad_prompts| render(&PhoneScreenSnapshot { near_tower: true, pad_prompts, ..calm(5) });
        assert_ne!(near(false), near(true));
    }

    #[test]
    fn callback_and_decoy_override_tiles() {
        // Without the upgrade states: Q tile is blocked (red) at 1 bar.
        let base = render(&calm(1));
        let call = render(&PhoneScreenSnapshot { callback_ready: true, ..calm(1) });
        assert_ne!(base[..], call[..], "callback tile differs");
        // Tile background turns electric gold.
        assert_eq!(px(&call, 150, 200), [40, 34, 6]);
        let zero = render(&calm(0));
        let decoy = render(&PhoneScreenSnapshot { decoy_ready: true, ..calm(0) });
        assert_eq!(px(&decoy, 70, 200), [44, 26, 4], "DECOY tile is amber");
        assert_ne!(px(&zero, 70, 200), [44, 26, 4]);
    }

    #[test]
    fn ambient_texts_fire_on_a_climb_to_two_bars_with_cooldown_and_never_repeat() {
        let mut n = PhoneNotifState::for_seed(1234);
        assert_eq!(n.on_bars(0), None);
        assert_eq!(n.on_bars(1), None, "1 bar is not enough");
        let first = n.on_bars(2).expect("climb to 2 bars texts");
        assert_eq!(n.banner(), Some((NotifKind::Ambient, first)));
        assert_eq!(n.on_bars(3), None, "staying up is not a climb");
        assert_eq!(n.on_bars(0), None);
        assert_eq!(n.on_bars(4), None, "cooldown blocks a quick second text");
        n.tick(AMBIENT_COOLDOWN);
        assert_eq!(n.banner(), None, "banner expires back to telemetry");
        assert_eq!(n.on_bars(1), None);
        let second = n.on_bars(2).expect("off cooldown");
        assert_ne!(first, second);
        // Exhaust the run's pool: all 8 texts exactly once, then silence.
        for _ in 0..20 {
            n.tick(AMBIENT_COOLDOWN);
            n.on_bars(0);
            n.on_bars(5);
        }
        let mut sent = n.sent.clone();
        assert_eq!(sent.len(), AMBIENT_NOTIFS.len());
        sent.sort();
        sent.dedup();
        assert_eq!(sent.len(), AMBIENT_NOTIFS.len(), "never repeats within a run");
        // Seeded order: deterministic per run, varies across runs.
        let order = |seed| {
            let mut s = PhoneNotifState::for_seed(seed);
            s.on_bars(2)
        };
        assert_eq!(order(7), order(7));
        assert!((0..16u64).map(order).collect::<std::collections::HashSet<_>>().len() > 1);
    }

    #[test]
    fn boss_call_rings_then_declines_and_blocks_ambient_spam() {
        let mut n = PhoneNotifState::for_seed(3);
        assert_eq!(n.on_boss(false, 0), None);
        assert_eq!(n.on_boss(true, 0), Some(NotifKind::IncomingCall));
        assert_eq!(n.banner(), Some((NotifKind::IncomingCall, INCOMING_CALL_TEXT)));
        assert_eq!(n.on_boss(true, 0), None, "rings once per arrival");
        assert_eq!(n.on_bars(0), None);
        assert_eq!(n.on_bars(5), None, "an incoming call is never overwritten by a text");
        assert_eq!(n.on_boss(false, 1), Some(NotifKind::CallDeclined));
        assert_eq!(n.banner(), Some((NotifKind::CallDeclined, CALL_DECLINED_TEXT)));
        n.tick(DECLINED_SECS + 0.01);
        assert_eq!(n.banner(), None);
        // Deploy flash: brief, quiet period after it.
        n.searching();
        assert_eq!(n.banner(), Some((NotifKind::Searching, SEARCHING_TEXT)));
        assert_eq!(n.on_bars(5), None, "quiet right after a deploy");
        assert_eq!(sector_subtitle(1), "SECTOR 01 // BASEMENT GARAGE");
        assert_eq!(sector_subtitle(5), "SECTOR 05 // ROOFTOP RELAY");
        assert!(sector_subtitle(7).starts_with("SECTOR 07"));
    }

    #[test]
    fn notification_banners_render_over_the_radar_and_fit_the_lcd() {
        for t in AMBIENT_NOTIFS {
            let lines = wrap_banner(t, BANNER_LINE_CHARS);
            assert!(!lines.is_empty() && lines.len() <= 3, "{t}: {lines:?}");
            assert!(lines.iter().all(|l| l.len() <= BANNER_LINE_CHARS));
            assert!(t.chars().all(|c| c == ' ' || glyph(c.to_ascii_uppercase()) != [0; 7]), "{t}: every glyph drawable");
        }
        for t in [INCOMING_CALL_TEXT, CALL_DECLINED_TEXT, SEARCHING_TEXT] {
            assert!(t.chars().all(|c| c == ' ' || glyph(c) != [0; 7]), "{t}");
        }
        let radar = |b: &[u8]| (116..166).flat_map(|y| (5..155).map(move |x| px(b, x, y))).collect::<Vec<_>>();
        let tiles = |b: &[u8]| (170..236).flat_map(|y| (5..155).map(move |x| px(b, x, y))).collect::<Vec<_>>();
        let base = render(&calm(3));
        let mut prev = radar(&base);
        for kind in [NotifKind::Ambient, NotifKind::IncomingCall, NotifKind::CallDeclined, NotifKind::Searching] {
            let text = match kind {
                NotifKind::Ambient => AMBIENT_NOTIFS[0],
                NotifKind::IncomingCall => INCOMING_CALL_TEXT,
                NotifKind::CallDeclined => CALL_DECLINED_TEXT,
                NotifKind::Searching => SEARCHING_TEXT,
            };
            let b = render(&PhoneScreenSnapshot { notif: Some((kind, text)), ..calm(3) });
            assert_ne!(radar(&b), radar(&base), "{kind:?} banner drawn");
            assert_ne!(radar(&b), prev, "{kind:?} looks distinct");
            assert_eq!(tiles(&b), tiles(&base), "{kind:?} keeps the app tiles + SOS bar");
            prev = radar(&b);
        }
    }

    #[test]
    fn wrap_banner_handles_multibyte_utf8_and_zero_width() {
        assert!(wrap_banner("hello world", 0).is_empty());
        assert_eq!(wrap_banner("éé", 3), vec!["éé"]);
        assert_eq!(wrap_banner("éééé", 3), vec!["ééé", "é"]);
        assert_eq!(wrap_banner("caffè è pronto", 6), vec!["caffè", "è", "pronto"]);
    }
}
