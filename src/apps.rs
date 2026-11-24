//! Signal-powered phone apps (left hand):
//! - Right-click **PING SCAN** (>= 1 bar): sonar pulse, marks demons, x-ray
//!   beacons over the Extraction Tower and every remaining Demon Jammer.
//! - `Q` **5G OVERLOAD** (>= 3 bars): EMP shockwave, stun, knockback, optional
//!   chain lightning (EMP Overcharge).
//! - Airplane Mode Phase: dash shockwave (upgrade).
//! - Sub-GHz Penetration: Ping projects a pulsing floor route to the Tower.
//! - Store & Forward: blackout-kill packets discharge as arcs at 3+ bars.
//! - Emergency Callback: ping-marked Fiend kills unlock an any-signal Overload.
//! - Missed Call: Ping at 0 bars drops a ringing decoy handset that lures Imps.
//! - Signal Spoofer: dashing in a 0-1 bar Dead Zone spoofs 4 bars for 2.5s.
//! - Jammer Hijack: destroyed Jammers / Fiends leave an 8s 5-bar hotspot.

use bevy::prelude::*;

use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::{take_flag, BotControl};
use crate::enemies::{damage_enemy, Enemy, EnemyAssets, EnemyKind, PingMarked, FIEND_FIELD_RADIUS};
use crate::fx::{overlay_layer, spawn_beam, Fx, FxFade, FxMotion, spawn_burst, spawn_flash_light, spawn_lightning, spawn_shockwave, BurstColor, FxAssets, HudFeed};
use crate::fx::beam_glows;
use crate::level::{CullableLight, DemonJammer, ExplosiveBarrel, LevelEntity, LevelGrid};
use crate::physics::sample_floor_height;
use crate::player::{CursorCaptured, Player};
use crate::signal::{bars_from_strength, SignalState};
use crate::state::PlayerModifiers;
use crate::weapons::{damage_barrel, damage_jammer, knockback_scale, CombatDamage, BARREL_CENTER_Y, BARREL_RADIUS};

pub const PING_COOLDOWN: f32 = 4.0;
pub const PING_MIN_BARS: u8 = 1;
pub const PING_MARK_SECS: f32 = 5.5;
pub const BEACON_SECS: f32 = 4.5;
pub const OVERLOAD_COOLDOWN: f32 = 10.0;
pub const OVERLOAD_MIN_BARS: u8 = 3;
pub const EMP_RADIUS: f32 = 12.0;
pub const EMP_DAMAGE: f32 = 55.0;
pub const EMP_STUN: f32 = 1.6;
pub const CHAIN_DAMAGE: f32 = 35.0;
pub const CHAIN_RANGE: f32 = 14.0;
pub const CHAIN_MAX: usize = 3;
pub const DASH_SHOCK_RADIUS: f32 = 5.5;
pub const DASH_SHOCK_DAMAGE: f32 = 28.0;
/// Sub-GHz route: breadcrumb nodes along the BFS path to the Tower.
pub const ROUTE_MAX_STEPS: usize = 14;
pub const ROUTE_LIFE: f32 = 5.0;
/// Store & Forward.
pub const STORE_FORWARD_MAX_PACKETS: u8 = 3;
pub const STORE_FORWARD_DAMAGE: f32 = 20.0;
pub const STORE_FORWARD_RANGE: f32 = 22.0;
pub const STORE_FORWARD_MIN_BARS: u8 = 3;
/// Emergency Callback window.
pub const CALLBACK_SECS: f32 = 6.0;
/// Missed Call decoy.
pub const DECOY_SECS: f32 = 2.5;
pub const DECOY_LURE_RADIUS: f32 = 18.0;
/// Signal Spoofer window, rearm cooldown and the spoofed signal strength (bars).
pub const SIGNAL_SPOOF_SECS: f32 = 2.5;
pub const SIGNAL_SPOOF_COOLDOWN: f32 = 6.0;
pub const SPOOF_STRENGTH: f32 = 4.0;
/// Highest (natural or displayed) bar count at which a dash can spoof.
pub const SPOOF_MAX_BARS: u8 = 1;
/// Jammer Hijack hotspot.
pub const HIJACK_RADIUS: f32 = 12.0;
pub const HIJACK_SECS: f32 = 8.0;
pub const HIJACK_STRENGTH: f32 = 5.0;

/// Phone app cooldowns and animation timers (reset every floor).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct PhoneApps {
    pub ping_cooldown: f32,
    pub overload_cooldown: f32,
    pub ping_active_timer: f32,
    pub overload_flash_timer: f32,
    /// Phone viewmodel "raise" animation (1 -> 0).
    pub cast_anim: f32,
    /// Dev/screenshot hook: cast Ping Scan once regardless of input.
    pub force_ping: bool,
    /// Store & Forward packets queued by natural 0-bar kills (0..=3).
    pub queued_packets: u8,
    /// Emergency Callback: seconds left on the any-signal 5G Overload.
    pub callback_timer: f32,
    /// Signal Spoofer: seconds left on the spoofed 4-bar signal.
    pub spoof_timer: f32,
    /// Signal Spoofer: seconds until another Dead-Zone dash can spoof again.
    pub spoof_cooldown: f32,
}

/// Jammer Hijack: a temporary 5-bar signal hotspot left by a destroyed
/// Jammer / Jammer Fiend (read by the signal solver, ticked by [`tick_hijack_hotspots`]).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct HijackedHotspot {
    pub radius: f32,
    pub timer: f32,
}

impl Default for HijackedHotspot {
    fn default() -> Self {
        HijackedHotspot { radius: HIJACK_RADIUS, timer: HIJACK_SECS }
    }
}

/// Sub-GHz route node: pulsing floor beacon on the path to the Tower.
#[derive(Component, Debug, Clone, Copy)]
pub struct RouteBreadcrumb {
    /// Route index (drives the travelling pulse).
    pub index: usize,
}

/// Missed Call: ringing decoy handset that lures nearby Imps.
#[derive(Component, Debug, Clone)]
pub struct DecoyHandset {
    pub timer: f32,
}

/// Decoy parts animated by [`tick_decoys`].
#[derive(Component, Debug, Clone)]
pub struct DecoyScreen;
#[derive(Component, Debug, Clone, Copy)]
pub struct DecoyRing;
#[derive(Component, Debug, Clone, Copy)]
pub struct DecoyLight;

pub fn ping_radius(bars: u8) -> f32 {
    24.0 + 6.0 * bars as f32
}

pub fn ping_cooldown(mods: &PlayerModifiers) -> f32 {
    PING_COOLDOWN * mods.ping_cooldown_mult
}

/// 5G Overload cooldown after a cast (Faraday Case lengthens it).
pub fn overload_cooldown(mods: &PlayerModifiers) -> f32 {
    OVERLOAD_COOLDOWN * mods.emp_cooldown_mult
}

/// Signal Spoofer trigger: the upgrade is installed, off cooldown, a dash just
/// began and the player is in a 0-1 bar Dead Zone (displayed or natural signal).
pub fn spoof_triggers(mods: &PlayerModifiers, dash_started: bool, bars: u8, natural_raw: f32, spoof_cooldown: f32) -> bool {
    mods.signal_spoofer
        && dash_started
        && spoof_cooldown <= 0.0
        && (bars <= SPOOF_MAX_BARS || bars_from_strength(natural_raw) <= SPOOF_MAX_BARS)
}

pub fn emp_radius(mods: &PlayerModifiers) -> f32 {
    EMP_RADIUS * mods.emp_radius_mult
}

pub fn emp_damage(mods: &PlayerModifiers) -> f32 {
    EMP_DAMAGE * mods.emp_damage_mult
}

/// Why an app cannot fire right now (`None` = ready).
pub fn app_block_reason(bars: u8, min_bars: u8, cooldown: f32) -> Option<String> {
    if bars < min_bars {
        Some(format!("NEED {min_bars} BAR{}", if min_bars == 1 { "" } else { "S" }))
    } else if cooldown > 0.0 {
        Some(format!("{cooldown:.1}s"))
    } else {
        None
    }
}

/// Chain-lightning picks: up to `max` extra targets (not already hit) within
/// `range` of any hit target. Returns `(source_pos, target_index)` pairs.
pub fn chain_targets(hit: &[Vec3], candidates: &[(usize, Vec3)], max: usize, range: f32) -> Vec<(Vec3, usize)> {
    let mut picks: Vec<(f32, Vec3, usize)> = candidates
        .iter()
        .filter_map(|&(i, p)| {
            hit.iter().map(|h| (h.distance(p), *h)).filter(|(d, _)| *d <= range).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(d, h)| (d, h, i))
        })
        .collect();
    picks.sort_by(|a, b| a.0.total_cmp(&b.0));
    picks.into_iter().take(max).map(|(_, h, i)| (h, i)).collect()
}

impl PhoneApps {
    pub fn tick(&mut self, dt: f32) {
        self.ping_cooldown = (self.ping_cooldown - dt).max(0.0);
        self.overload_cooldown = (self.overload_cooldown - dt).max(0.0);
        self.ping_active_timer = (self.ping_active_timer - dt).max(0.0);
        self.overload_flash_timer = (self.overload_flash_timer - dt).max(0.0);
        self.cast_anim = (self.cast_anim - dt * 2.0).max(0.0);
        self.callback_timer = (self.callback_timer - dt).max(0.0);
        self.spoof_timer = (self.spoof_timer - dt).max(0.0);
        self.spoof_cooldown = (self.spoof_cooldown - dt).max(0.0);
    }

    pub fn spoof_active(&self) -> bool {
        self.spoof_timer > 0.0
    }

    /// Store & Forward: queue one blackout packet (capped). Returns the new count.
    pub fn queue_packet(&mut self) -> u8 {
        self.queued_packets = (self.queued_packets + 1).min(STORE_FORWARD_MAX_PACKETS);
        self.queued_packets
    }

    /// Emergency Callback armed and the Overload is off cooldown.
    pub fn callback_ready(&self) -> bool {
        self.callback_timer > 0.0 && self.overload_cooldown <= 0.0
    }
}

/// Bars the 5G Overload gate sees: an armed Emergency Callback satisfies it.
pub fn overload_gate_bars(bars: u8, callback_timer: f32) -> u8 {
    if callback_timer > 0.0 { bars.max(OVERLOAD_MIN_BARS) } else { bars }
}

/// Store & Forward targets: one per packet, nearest first, cycling over the
/// in-range targets when there are fewer targets than packets.
pub fn packet_targets(distances: &[(usize, f32)], packets: u8, range: f32) -> Vec<usize> {
    let mut inr: Vec<(usize, f32)> = distances.iter().copied().filter(|(_, d)| *d <= range).collect();
    inr.sort_by(|a, b| a.1.total_cmp(&b.1));
    if inr.is_empty() {
        return Vec::new();
    }
    (0..packets as usize).map(|i| inr[i % inr.len()].0).collect()
}

/// Ping Scan with Sub-GHz: the next `max_steps` cells of the walkable route
/// from `from` toward the Tower (start cell excluded).
pub fn tower_route(grid: &LevelGrid, from: IVec2, max_steps: usize) -> Vec<IVec2> {
    grid.walk_route(from, grid.tower).map(|r| r.into_iter().skip(1).take(max_steps).collect()).unwrap_or_default()
}

fn spawn_route_breadcrumbs(commands: &mut Commands, fx: &FxAssets, materials: &mut Assets<StandardMaterial>, grid: &LevelGrid, cells: &[IVec2]) {
    let cyan = Color::srgb(0.2, 1.0, 0.95);
    for (i, &c) in cells.iter().enumerate() {
        let mut p = grid.cell_center(c);
        p.y = sample_floor_height(grid, p.x, p.z).unwrap_or(p.y) + 0.06;
        // Short glowing post + flat pulsing floor disc.
        spawn_beam(commands, fx, materials, p, 0.45, 0.32, ROUTE_LIFE, cyan, false);
        let fade = FxFade::new(fx.fade_palette(materials, cyan, 0.75));
        let scale = Vec3::new(0.55, 0.05, 0.55);
        commands.spawn((
            LevelEntity,
            RouteBreadcrumb { index: i },
            Fx::new(ROUTE_LIFE, FxMotion::Hold, scale),
            MeshMaterial3d(fade.first()),
            fade,
            Mesh3d(fx.ring.clone()),
            Transform::from_translation(p).with_scale(scale),
        ));
    }
}

/// Travelling pulse along the Sub-GHz route (toward the Tower).
fn animate_breadcrumbs(time: Res<Time>, mut q: Query<(&RouteBreadcrumb, &Fx, &mut Transform)>) {
    let t = time.elapsed_secs();
    for (b, fx, mut tf) in &mut q {
        let wave = ((t * 7.0 - b.index as f32 * 0.7).sin() * 0.5 + 0.5).powi(2);
        let s = 1.0 + wave * 0.7;
        tf.scale = Vec3::new(fx.base_scale.x * s, fx.base_scale.y, fx.base_scale.z * s);
    }
}

/// Missed Call decoy: shared pre-allocated materials (no per-decoy materials).
fn spawn_decoy(commands: &mut Commands, fx: &FxAssets, pos: Vec3) -> Entity {
    let (body, screen, ring) = (fx.decoy_body.clone(), fx.decoy_screen_amber.clone(), fx.decoy_ring.clone());
    commands
        .spawn((
            LevelEntity,
            DecoyHandset { timer: DECOY_SECS },
            Transform::from_translation(pos),
            Visibility::default(),
            children![
                (Mesh3d(fx.cube.clone()), MeshMaterial3d(body), Transform::from_scale(Vec3::new(0.34, 0.06, 0.66))),
                (DecoyScreen, Mesh3d(fx.cube.clone()), MeshMaterial3d(screen), Transform::from_xyz(0.0, 0.035, 0.0).with_scale(Vec3::new(0.28, 0.02, 0.56))),
                (DecoyRing, Mesh3d(fx.ring.clone()), MeshMaterial3d(ring), Transform::from_xyz(0.0, -0.15, 0.0).with_scale(Vec3::new(0.6, 0.1, 0.6))),
                (
                    DecoyLight,
                    CullableLight::for_range(9.0),
                    PointLight { color: Color::srgb(1.0, 0.7, 0.2), intensity: 250_000.0, range: 9.0, shadow_maps_enabled: false, ..default() },
                    Transform::from_xyz(0.0, 0.5, 0.0),
                ),
            ],
        ))
        .id()
}

/// Missed Call decoys: vibrate, blink amber/cyan, pulse the ring, expire.
#[allow(clippy::type_complexity)]
pub fn tick_decoys(
    mut commands: Commands,
    time: Res<Time>,
    fx: Res<FxAssets>,
    mut decoys: Query<(Entity, &mut DecoyHandset, &mut Transform, &Children)>,
    mut screens: Query<&mut MeshMaterial3d<StandardMaterial>, With<DecoyScreen>>,
    mut rings: Query<&mut Transform, (With<DecoyRing>, Without<DecoyHandset>)>,
    mut lights: Query<&mut PointLight, With<DecoyLight>>,
) {
    let dt = time.delta_secs();
    for (e, mut d, mut tf, children) in &mut decoys {
        d.timer -= dt;
        if d.timer <= 0.0 {
            commands.entity(e).try_despawn();
            continue;
        }
        let age = DECOY_SECS - d.timer;
        // Ringing buzz: rapid yaw wobble in bursts.
        let buzz = if (age * 2.5).fract() < 0.6 { (age * 70.0).sin() * 0.16 } else { 0.0 };
        tf.rotation = Quat::from_rotation_y(buzz);
        let cyan_phase = (age * 6.0) as u32 % 2 == 1;
        for c in children.iter() {
            if let Ok(mut m) = screens.get_mut(c) {
                let want = if cyan_phase { &fx.decoy_screen_cyan } else { &fx.decoy_screen_amber };
                if m.0.id() != want.id() {
                    m.0 = want.clone();
                }
            }
            if let Ok(mut r) = rings.get_mut(c) {
                let k = (age * 1.8).fract();
                let s = 0.5 + k * 2.4;
                r.scale = Vec3::new(s, 0.1, s);
            }
            if let Ok(mut l) = lights.get_mut(c) {
                l.intensity = 120_000.0 + 260_000.0 * ((age * 12.0).sin() * 0.5 + 0.5);
                l.color = if cyan_phase { Color::srgb(0.3, 0.95, 1.0) } else { Color::srgb(1.0, 0.7, 0.2) };
            }
        }
    }
}

/// Denied-toast text for 5G Overload: names the Jammer Fiend when it is the
/// reason the player is short of bars.
pub fn overload_denied_text(why: &str, bars: u8, fiend_near: bool) -> String {
    if bars < OVERLOAD_MIN_BARS && fiend_near {
        "5G BLOCKED BY JAMMER FIEND! KILL IT FOR 3+ BARS".into()
    } else {
        format!("5G OVERLOAD: {why}")
    }
}

fn reset_apps(mut apps: ResMut<PhoneApps>) {
    *apps = PhoneApps::default();
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn phone_apps(
    mut commands: Commands,
    time: Res<Time>,
    (keys, mouse, captured): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<CursorCaptured>),
    grid: Res<LevelGrid>,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    mut apps: ResMut<PhoneApps>,
    fx: Res<FxAssets>,
    ea: Res<EnemyAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    (players, decoys): (Query<&Transform, With<Player>>, Query<Entity, With<DecoyHandset>>),
    mut enemies: Query<(Entity, &mut Enemy, &GlobalTransform, Option<&mut PingMarked>)>,
    (mut jammers, mut barrels): (Query<(&mut DemonJammer, &GlobalTransform)>, Query<(&mut ExplosiveBarrel, &GlobalTransform)>),
    mut bot: Option<ResMut<BotControl>>,
) {
    apps.tick(time.delta_secs());
    let Ok(ptf) = players.single() else { return };
    let center = ptf.translation + Vec3::Y * 1.0;

    // ---- PING SCAN ----
    let bot_ping = take_flag(&mut bot, |b| &mut b.ping);
    let ping_pressed = (captured.0 && mouse.just_pressed(MouseButton::Right)) || std::mem::take(&mut apps.force_ping) || bot_ping;
    if ping_pressed && signal.bars == 0 && mods.missed_call {
        // ---- MISSED CALL decoy (0 bars) ----
        if apps.ping_cooldown > 0.0 {
            sfx.play(SfxEvent::Denied);
            feed.toast(format!("MISSED CALL: {:.1}s", apps.ping_cooldown), Color::srgb(1.0, 0.5, 0.2));
        } else {
            for d in &decoys {
                commands.entity(d).try_despawn();
            }
            spawn_decoy(&mut commands, &fx, ptf.translation + Vec3::Y * 0.25);
            apps.ping_cooldown = ping_cooldown(&mods);
            apps.cast_anim = 1.0;
            sfx.play(SfxEvent::Ping);
            feed.toast(format!("MISSED CALL DECOY DEPLOYED ({DECOY_SECS:.1}s)"), Color::srgb(1.0, 0.75, 0.2));
        }
    } else if ping_pressed {
        match app_block_reason(signal.bars, PING_MIN_BARS, apps.ping_cooldown) {
            Some(why) => {
                sfx.play(SfxEvent::Denied);
                feed.toast(format!("PING SCAN: {why}"), Color::srgb(1.0, 0.5, 0.2));
            }
            None => {
                let radius = ping_radius(signal.bars);
                apps.ping_cooldown = ping_cooldown(&mods);
                apps.ping_active_timer = 1.2;
                apps.cast_anim = 1.0;
                sfx.play(SfxEvent::Ping);
                let cyan = Color::srgb(0.2, 0.95, 1.0);
                // Crisp expanding radar rings (no milky full-screen dome washout!).
                spawn_shockwave(&mut commands, &fx, &mut materials, ptf.translation + Vec3::Y * 0.14, radius, 0.95, cyan, 0.85, true);
                spawn_shockwave(&mut commands, &fx, &mut materials, ptf.translation + Vec3::Y * 0.45, radius * 0.72, 1.15, Color::srgb(0.55, 1.0, 1.0), 0.42, true);
                let mut marked = 0;
                for (e, en, g, mark) in &mut enemies {
                    if !en.is_alive() || g.translation().distance(center) > radius {
                        continue;
                    }
                    marked += 1;
                    match mark {
                        Some(mut m) => m.timer = PING_MARK_SECS,
                        None => {
                            let top = en.stats.hit_spheres.last().map_or(2.0, |(h, r)| h + r) + 0.55;
                            let sz = 0.24 * en.stats.radius.max(0.65);
                            let diamond = commands
                                .spawn((
                                    ChildOf(e),
                                    Mesh3d(ea.cube.clone()),
                                    MeshMaterial3d(ea.ping_ring.clone()),
                                    Transform::from_xyz(0.0, top, 0.0)
                                        .with_rotation(Quat::from_euler(EulerRot::XYZ, 0.785, 0.785, 0.785))
                                        .with_scale(Vec3::splat(sz)),
                                    overlay_layer(),
                                ))
                                .id();
                            commands.entity(e).try_insert(PingMarked { timer: PING_MARK_SECS, ring: Some(diamond) });
                        }
                    }
                }
                // X-ray beacons: tower + remaining jammers.
                spawn_beam(&mut commands, &fx, &mut materials, grid.tower_world(), 70.0, 0.7, BEACON_SECS, Color::srgb(0.2, 1.0, 0.8), true);
                let mut jcount = 0;
                for (j, g) in &jammers {
                    if j.is_active() {
                        jcount += 1;
                        spawn_beam(&mut commands, &fx, &mut materials, g.translation(), 45.0, 0.45, BEACON_SECS, Color::srgb(1.0, 0.1, 0.35), true);
                    }
                }
                feed.toast(format!("PING: {marked} DEMONS / {jcount} JAMMERS"), Color::srgb(0.3, 0.95, 1.0));
                if mods.sub_ghz_route {
                    let route = tower_route(&grid, grid.world_to_cell(ptf.translation), ROUTE_MAX_STEPS);
                    spawn_route_breadcrumbs(&mut commands, &fx, &mut materials, &grid, &route);
                }
            }
        }
    }

    // ---- 5G OVERLOAD ----
    if keys.just_pressed(KeyCode::KeyQ) || take_flag(&mut bot, |b| &mut b.overload) {
        match app_block_reason(overload_gate_bars(signal.bars, apps.callback_timer), OVERLOAD_MIN_BARS, apps.overload_cooldown) {
            Some(why) => {
                sfx.play(SfxEvent::Denied);
                let fiend_near = enemies
                    .iter()
                    .any(|(_, e, g, _)| e.kind == EnemyKind::JammerFiend && e.is_alive() && g.translation().distance(center) <= FIEND_FIELD_RADIUS);
                feed.toast(overload_denied_text(&why, signal.bars, fiend_near), Color::srgb(1.0, 0.5, 0.2));
            }
            None => {
                let radius = emp_radius(&mods);
                let dmg = emp_damage(&mods);
                if apps.callback_timer > 0.0 {
                    apps.callback_timer = 0.0;
                    feed.toast("EMERGENCY CALLBACK: 5G OVERLOAD FIRED", Color::srgb(1.0, 0.85, 0.25));
                }
                apps.overload_cooldown = overload_cooldown(&mods);
                apps.overload_flash_timer = 0.8;
                apps.cast_anim = 1.0;
                sfx.play(SfxEvent::Overload);
                let cyan = Color::srgb(0.35, 0.9, 1.0);
                // Razor-sharp twin concentric EMP rings + electric spark burst (preserves enemy contrast!).
                spawn_shockwave(&mut commands, &fx, &mut materials, ptf.translation + Vec3::Y * 0.18, radius, 0.58, cyan, 0.95, true);
                spawn_shockwave(&mut commands, &fx, &mut materials, ptf.translation + Vec3::Y * 1.1, radius * 0.78, 0.44, Color::srgb(0.85, 1.0, 1.0), 0.72, true);
                // (No beam column on the player: the camera would sit inside it -> full-screen cyan wash.)
                spawn_burst(&mut commands, &fx, center, BurstColor::Cyan, 18, 9.5, 0.08);
                spawn_flash_light(&mut commands, center, cyan, 2_800_000.0, radius * 1.4, 0.26);
                let mut hit_pos: Vec<Vec3> = Vec::new();
                let mut candidates: Vec<(usize, Vec3)> = Vec::new();
                for (i, (_, mut en, g, mark)) in enemies.iter_mut().enumerate() {
                    if !en.is_alive() {
                        continue;
                    }
                    let c = en.center(g.translation());
                    let d = c.distance(center) - en.stats.radius;
                    if d <= radius {
                        damage_enemy(&mut en, dmg, mark.is_some(), &mods);
                        let away = Vec3::new(c.x - center.x, 0.0, c.z - center.z).normalize_or_zero();
                        let kind = en.kind;
                        en.knockback += away * 14.0 * knockback_scale(kind);
                        en.stun_timer = EMP_STUN;
                        hit_pos.push(c);
                        spawn_lightning(&mut commands, &fx, center, c);
                    } else {
                        candidates.push((i, c));
                    }
                }
                let mut jhit = 0;
                for (mut j, g) in &mut jammers {
                    if j.is_active() && (g.translation() + Vec3::Y * 2.0).distance(center) <= radius + 0.8 {
                        damage_jammer(&mut j, dmg);
                        spawn_lightning(&mut commands, &fx, center, g.translation() + Vec3::Y * 2.7);
                        jhit += 1;
                    }
                }
                // Volatile coolant barrels in the EMP blast cook off.
                for (mut b, g) in &mut barrels {
                    let c = g.translation() + Vec3::Y * BARREL_CENTER_Y;
                    if b.is_intact() && c.distance(center) <= radius + BARREL_RADIUS && damage_barrel(&mut b, dmg, false) {
                        spawn_lightning(&mut commands, &fx, center, c);
                    }
                }
                if mods.emp_chains && !hit_pos.is_empty() {
                    let picks = chain_targets(&hit_pos, &candidates, CHAIN_MAX, CHAIN_RANGE);
                    for (src, idx) in picks {
                        if let Some((_, mut en, g, mark)) = enemies.iter_mut().nth(idx) {
                            damage_enemy(&mut en, CHAIN_DAMAGE, mark.is_some(), &mods);
                            en.stun_timer = en.stun_timer.max(EMP_STUN * 0.6);
                            spawn_lightning(&mut commands, &fx, src, en.center(g.translation()));
                        }
                    }
                }
                if !hit_pos.is_empty() || jhit > 0 {
                    feed.hit(false);
                }
                feed.toast(format!("5G OVERLOAD: {} HIT", hit_pos.len() + jhit), Color::srgb(0.3, 0.95, 1.0));
            }
        }
    }
}

/// Store & Forward: once back at 3+ bars, every queued blackout packet fires
/// a 20-damage arc at the nearest demons within range.
#[allow(clippy::too_many_arguments)]
pub fn store_and_forward_discharge(
    mut commands: Commands,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    mut apps: ResMut<PhoneApps>,
    fx: Res<FxAssets>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    players: Query<&Transform, With<Player>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
) {
    if apps.queued_packets == 0 || signal.bars < STORE_FORWARD_MIN_BARS {
        return;
    }
    let Ok(ptf) = players.single() else { return };
    let src = ptf.translation + Vec3::Y * 1.2;
    let dists: Vec<(usize, f32)> = enemies
        .iter()
        .enumerate()
        .filter(|(_, (e, _, _))| e.is_alive())
        .map(|(i, (e, g, _))| (i, e.center(g.translation()).distance(src)))
        .collect();
    let targets = packet_targets(&dists, apps.queued_packets, STORE_FORWARD_RANGE);
    if targets.is_empty() {
        return;
    }
    let mut list: Vec<_> = enemies.iter_mut().collect();
    for &i in &targets {
        let (en, g, mark) = &mut list[i];
        damage_enemy(en, STORE_FORWARD_DAMAGE, mark.is_some(), &mods);
        let c = en.center(g.translation());
        spawn_lightning(&mut commands, &fx, src, c);
    }
    sfx.play(SfxEvent::Overload);
    feed.hit(false);
    feed.toast(format!("STORE & FORWARD: {} ARCS FIRED!", targets.len()), Color::srgb(0.55, 0.75, 1.0));
    apps.queued_packets = 0;
}

/// Airplane Mode Phase: dashing releases a static shockwave.
#[allow(clippy::too_many_arguments)]
fn dash_shockwave(
    mut commands: Commands,
    mods: Res<PlayerModifiers>,
    fx: Res<FxAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut feed: ResMut<HudFeed>,
    mut sfx: ResMut<SfxQueue>,
    players: Query<(&Transform, &Player)>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
) {
    let Ok((ptf, p)) = players.single() else { return };
    if !p.dash_started || !mods.dash_shockwave {
        return;
    }
    let c = ptf.translation + Vec3::Y * 0.9;
    spawn_shockwave(&mut commands, &fx, &mut materials, ptf.translation + Vec3::Y * 0.2, DASH_SHOCK_RADIUS, 0.4, Color::srgb(0.3, 0.9, 1.0), 0.9, true);
    spawn_burst(&mut commands, &fx, c, BurstColor::Cyan, 10, 6.0, 0.08);
    let mut hits = 0;
    for (mut e, g, mark) in &mut enemies {
        let ec = e.center(g.translation());
        if e.is_alive() && ec.distance(c) - e.stats.radius <= DASH_SHOCK_RADIUS {
            damage_enemy(&mut e, DASH_SHOCK_DAMAGE, mark.is_some(), &mods);
            let away = Vec3::new(ec.x - c.x, 0.0, ec.z - c.z).normalize_or_zero();
            let kind = e.kind;
            e.knockback += away * 10.0 * knockback_scale(kind);
            hits += 1;
        }
    }
    if hits > 0 {
        feed.hit(false);
        sfx.play(SfxEvent::Hit);
    }
}

/// Signal Spoofer: a dash in a 0-1 bar Dead Zone spoofs 4 bars for 2.5s.
#[allow(clippy::too_many_arguments)]
fn signal_spoofer(
    mut commands: Commands,
    mods: Res<PlayerModifiers>,
    signal: Res<SignalState>,
    mut apps: ResMut<PhoneApps>,
    fx: Res<FxAssets>,
    mut feed: ResMut<HudFeed>,
    mut sfx: ResMut<SfxQueue>,
    players: Query<(&Transform, &Player)>,
) {
    let Ok((ptf, p)) = players.single() else { return };
    if !spoof_triggers(&mods, p.dash_started, signal.bars, signal.natural_raw_strength, apps.spoof_cooldown) {
        return;
    }
    apps.spoof_timer = SIGNAL_SPOOF_SECS;
    apps.spoof_cooldown = SIGNAL_SPOOF_COOLDOWN;
    let c = ptf.translation + Vec3::Y * 1.0;
    let cyan = Color::srgb(0.25, 1.0, 1.0);
    spawn_flash_light(&mut commands, c, cyan, 900_000.0, 10.0, 0.3);
    spawn_burst(&mut commands, &fx, c, BurstColor::Cyan, 12, 6.5, 0.07);
    sfx.play(SfxEvent::Ping);
    feed.toast(format!("SIGNAL SPOOFED - 4 BARS ({SIGNAL_SPOOF_SECS:.1}s)"), cyan);
}

/// Jammer Hijack: spawn a 5-bar signal hotspot (glowing floor ring + beam).
pub fn spawn_hijack_hotspot(commands: &mut Commands, fx: &FxAssets, materials: &mut Assets<StandardMaterial>, feed: &mut HudFeed, pos: Vec3) -> Entity {
    let ground = pos.with_y(pos.y.max(0.0) + 0.07);
    let emerald = Color::srgb(0.2, 1.0, 0.7);
    spawn_beam(commands, fx, materials, ground, 7.0, 0.55, HIJACK_SECS, emerald, false);
    let fade = FxFade::new(fx.fade_palette(materials, HIJACK_RING_COLOR, 0.7));
    let scale = Vec3::new(HIJACK_RADIUS, 0.08, HIJACK_RADIUS);
    commands.spawn((
        LevelEntity,
        Fx::new(HIJACK_SECS, FxMotion::Hold, scale),
        MeshMaterial3d(fade.first()),
        fade,
        Mesh3d(fx.ring.clone()),
        Transform::from_translation(ground).with_scale(scale),
    ));
    feed.toast("JAMMER HIJACKED - 5-BAR HOTSPOT!", emerald);
    commands.spawn((LevelEntity, HijackedHotspot::default(), Transform::from_translation(ground))).id()
}

/// Expire Jammer Hijack hotspots.
pub fn tick_hijack_hotspots(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut HijackedHotspot)>) {
    for (e, mut h) in &mut q {
        h.timer -= time.delta_secs();
        if h.timer <= 0.0 {
            commands.entity(e).try_despawn();
        }
    }
}

const HIJACK_RING_COLOR: Color = Color::srgb(0.25, 1.0, 0.85);

/// Every glow the phone apps fade (Ping / Overload / dash shockwaves, beacon
/// and route beams, route discs, hijack hotspots): palettes built at startup.
fn prewarm_app_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mut glows = vec![
        (Color::srgb(0.2, 0.95, 1.0), 0.85),
        (Color::srgb(0.55, 1.0, 1.0), 0.42),
        (Color::srgb(0.35, 0.9, 1.0), 0.95),
        (Color::srgb(0.85, 1.0, 1.0), 0.72),
        (Color::srgb(0.3, 0.9, 1.0), 0.9),
        (Color::srgb(0.2, 1.0, 0.95), 0.75),
        (HIJACK_RING_COLOR, 0.7),
    ];
    for beam in [Color::srgb(0.2, 1.0, 0.95), Color::srgb(0.2, 1.0, 0.8), Color::srgb(1.0, 0.1, 0.35), Color::srgb(0.2, 1.0, 0.7)] {
        glows.extend(beam_glows(beam));
    }
    fx.prewarm_fades(&mut materials, &glows);
}

pub struct AppsPlugin;

impl Plugin for AppsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhoneApps>()
            .add_systems(Startup, prewarm_app_fades)
            .add_systems(Update, reset_apps.run_if(resource_exists_and_changed::<LevelGrid>).in_set(crate::state::GameSet::World))
            .add_systems(
                Update,
                (phone_apps, dash_shockwave, store_and_forward_discharge).chain().in_set(CombatDamage).run_if(resource_exists::<LevelGrid>),
            )
            .add_systems(Update, signal_spoofer.after(phone_apps).in_set(CombatDamage).run_if(resource_exists::<LevelGrid>))
            .add_systems(Update, (tick_decoys, animate_breadcrumbs, tick_hijack_hotspots).in_set(crate::state::GameSet::World));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_requirements() {
        assert_eq!(app_block_reason(0, PING_MIN_BARS, 0.0), Some("NEED 1 BAR".into()));
        assert_eq!(app_block_reason(1, PING_MIN_BARS, 0.0), None);
        assert_eq!(app_block_reason(2, OVERLOAD_MIN_BARS, 0.0), Some("NEED 3 BARS".into()));
        assert_eq!(app_block_reason(5, OVERLOAD_MIN_BARS, 2.5), Some("2.5s".into()));
        assert_eq!(overload_denied_text("NEED 3 BARS", 1, true), "5G BLOCKED BY JAMMER FIEND! KILL IT FOR 3+ BARS");
        assert_eq!(overload_denied_text("NEED 3 BARS", 1, false), "5G OVERLOAD: NEED 3 BARS");
        assert_eq!(overload_denied_text("2.5s", 4, true), "5G OVERLOAD: 2.5s");
    }

    #[test]
    fn ping_scales_with_signal_and_sniffer() {
        assert_eq!(ping_radius(1), 30.0);
        assert_eq!(ping_radius(5), 54.0);
        let sniff = PlayerModifiers { ping_cooldown_mult: 0.5, ..default() };
        assert_eq!(ping_cooldown(&sniff), 2.0);
        let emp = PlayerModifiers { emp_radius_mult: 1.4, emp_damage_mult: 1.35, ..default() };
        assert!((emp_radius(&emp) - 16.8).abs() < 1e-4);
        assert!(emp_damage(&emp) > EMP_DAMAGE);
    }

    #[test]
    fn chain_picks_nearest_three_in_range() {
        let hit = [Vec3::ZERO];
        let cands = [(0, Vec3::new(3.0, 0.0, 0.0)), (1, Vec3::new(20.0, 0.0, 0.0)), (2, Vec3::new(1.0, 0.0, 0.0)), (3, Vec3::new(10.0, 0.0, 0.0)), (4, Vec3::new(13.0, 0.0, 0.0))];
        let picks = chain_targets(&hit, &cands, CHAIN_MAX, CHAIN_RANGE);
        let idx: Vec<usize> = picks.iter().map(|p| p.1).collect();
        assert_eq!(idx, vec![2, 0, 3]);
        assert!(chain_targets(&[], &cands, 3, 14.0).is_empty());
    }

    #[test]
    fn callback_opens_overload_gate() {
        assert_eq!(overload_gate_bars(0, 0.0), 0);
        assert_eq!(overload_gate_bars(0, 2.0), OVERLOAD_MIN_BARS);
        assert_eq!(overload_gate_bars(5, 2.0), 5);
        assert_eq!(app_block_reason(overload_gate_bars(0, 3.0), OVERLOAD_MIN_BARS, 0.0), None);
        assert!(app_block_reason(overload_gate_bars(0, 3.0), OVERLOAD_MIN_BARS, 4.0).is_some(), "cooldown still applies");
        let mut a = PhoneApps { callback_timer: CALLBACK_SECS, ..default() };
        assert!(a.callback_ready());
        a.overload_cooldown = 1.0;
        assert!(!a.callback_ready());
        a.tick(CALLBACK_SECS);
        assert_eq!(a.callback_timer, 0.0);
    }

    #[test]
    fn packets_cap_and_cycle_targets() {
        let mut a = PhoneApps::default();
        for _ in 0..5 {
            a.queue_packet();
        }
        assert_eq!(a.queued_packets, STORE_FORWARD_MAX_PACKETS);
        let d = [(0, 30.0), (1, 5.0), (2, 12.0)];
        assert_eq!(packet_targets(&d, 3, STORE_FORWARD_RANGE), vec![1, 2, 1]);
        assert_eq!(packet_targets(&[(4, 8.0)], 3, STORE_FORWARD_RANGE), vec![4, 4, 4]);
        assert!(packet_targets(&[(0, 30.0)], 3, STORE_FORWARD_RANGE).is_empty());
    }

    #[test]
    fn spoofer_triggers_only_on_dash_in_dead_zone() {
        let on = PlayerModifiers { signal_spoofer: true, ..default() };
        assert!(spoof_triggers(&on, true, 0, 0.0, 0.0));
        assert!(spoof_triggers(&on, true, 1, 1.5, 0.0));
        assert!(!spoof_triggers(&on, true, 3, 3.2, 0.0), "healthy signal: nothing to spoof");
        assert!(!spoof_triggers(&on, false, 0, 0.0, 0.0), "needs the dash");
        assert!(!spoof_triggers(&PlayerModifiers::default(), true, 0, 0.0, 0.0), "needs the app");
        // Copper Antenna lifts the display, the natural Dead Zone still counts once off cooldown.
        assert!(spoof_triggers(&on, true, 2, 0.4, 0.0));
        // Rearm cooldown prevents chaining dashes into permanent 4-bar uptime.
        assert!(!spoof_triggers(&on, true, 0, 0.0, 1.2), "blocked while cooling down");
        assert!(!spoof_triggers(&on, true, 4, 0.4, SIGNAL_SPOOF_COOLDOWN - 2.0), "re-dash cannot refresh early");
        let mut a = PhoneApps { spoof_timer: SIGNAL_SPOOF_SECS, spoof_cooldown: SIGNAL_SPOOF_COOLDOWN, ..default() };
        assert!(a.spoof_active());
        a.tick(SIGNAL_SPOOF_SECS);
        assert!(!a.spoof_active());
        assert!(a.spoof_cooldown > 0.0, "rearm delay remains after spoof ends");
        a.tick(SIGNAL_SPOOF_COOLDOWN);
        assert_eq!(a.spoof_cooldown, 0.0);
    }

    #[test]
    fn faraday_lengthens_overload_cooldown() {
        assert_eq!(overload_cooldown(&PlayerModifiers::default()), OVERLOAD_COOLDOWN);
        let f = PlayerModifiers { emp_cooldown_mult: 1.25, ..default() };
        assert!((overload_cooldown(&f) - 12.5).abs() < 1e-5);
        assert_eq!(HijackedHotspot::default(), HijackedHotspot { radius: HIJACK_RADIUS, timer: HIJACK_SECS });
    }

    #[test]
    fn apps_tick_down() {
        let mut a = PhoneApps { ping_cooldown: 1.0, overload_cooldown: 5.0, cast_anim: 1.0, ..default() };
        a.tick(2.0);
        assert_eq!(a.ping_cooldown, 0.0);
        assert_eq!(a.overload_cooldown, 3.0);
        assert_eq!(a.cast_anim, 0.0);
    }
}
