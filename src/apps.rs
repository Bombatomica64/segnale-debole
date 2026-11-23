//! Signal-powered phone apps (left hand):
//! - Right-click **PING SCAN** (>= 1 bar): sonar pulse, marks demons, x-ray
//!   beacons over the Extraction Tower and every remaining Demon Jammer.
//! - `Q` **5G OVERLOAD** (>= 3 bars): EMP shockwave, stun, knockback, optional
//!   chain lightning (EMP Overcharge).
//! - Airplane Mode Phase: dash shockwave (upgrade).

use bevy::prelude::*;

use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::{take_flag, BotControl};
use crate::enemies::{damage_enemy, Enemy, EnemyAssets, EnemyKind, PingMarked, FIEND_FIELD_RADIUS};
use crate::fx::{overlay_layer, spawn_beam, spawn_burst, spawn_flash_light, spawn_lightning, spawn_shockwave, BurstColor, FxAssets, HudFeed};
use crate::level::{DemonJammer, LevelGrid};
use crate::player::{CursorCaptured, Player};
use crate::signal::SignalState;
use crate::state::PlayerModifiers;
use crate::weapons::{damage_jammer, knockback_scale, CombatDamage};

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
}

pub fn ping_radius(bars: u8) -> f32 {
    24.0 + 6.0 * bars as f32
}

pub fn ping_cooldown(mods: &PlayerModifiers) -> f32 {
    PING_COOLDOWN * mods.ping_cooldown_mult
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
    players: Query<&Transform, With<Player>>,
    mut enemies: Query<(Entity, &mut Enemy, &GlobalTransform, Option<&mut PingMarked>)>,
    mut jammers: Query<(&mut DemonJammer, &GlobalTransform)>,
    mut bot: Option<ResMut<BotControl>>,
) {
    apps.tick(time.delta_secs());
    let Ok(ptf) = players.single() else { return };
    let center = ptf.translation + Vec3::Y * 1.0;

    // ---- PING SCAN ----
    let bot_ping = take_flag(&mut bot, |b| &mut b.ping);
    if (captured.0 && mouse.just_pressed(MouseButton::Right)) || std::mem::take(&mut apps.force_ping) || bot_ping {
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
            }
        }
    }

    // ---- 5G OVERLOAD ----
    if keys.just_pressed(KeyCode::KeyQ) || take_flag(&mut bot, |b| &mut b.overload) {
        match app_block_reason(signal.bars, OVERLOAD_MIN_BARS, apps.overload_cooldown) {
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
                apps.overload_cooldown = OVERLOAD_COOLDOWN;
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

pub struct AppsPlugin;

impl Plugin for AppsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhoneApps>()
            .add_systems(Update, reset_apps.run_if(resource_exists_and_changed::<LevelGrid>).in_set(crate::state::GameSet::World))
            .add_systems(Update, (phone_apps, dash_shockwave).chain().in_set(CombatDamage).run_if(resource_exists::<LevelGrid>));
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
    fn apps_tick_down() {
        let mut a = PhoneApps { ping_cooldown: 1.0, overload_cooldown: 5.0, cast_anim: 1.0, ..default() };
        a.tick(2.0);
        assert_eq!(a.ping_cooldown, 0.0);
        assert_eq!(a.overload_cooldown, 3.0);
        assert_eq!(a.cast_anim, 0.0);
    }
}
