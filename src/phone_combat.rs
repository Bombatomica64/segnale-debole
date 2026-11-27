//! Milestone 33: tactical phone abilities that complete the signal ladder.
//!
//! - **0 bars: PHONE PUNCH** (`V` / middle mouse / pad `B`): an ULTRAKILL-style
//!   left-hand smartphone jab. In priority order it **parries** enemy
//!   projectiles back along the crosshair as blazing gold
//!   [`ProjectileKind::Parried`](crate::weapons::ProjectileKind::Parried)
//!   bolts (+HP, dash reset, big style), **launches** an intact coolant barrel
//!   forward as a flying bomb ([`LaunchedBarrel`]), or lands a melee blow on
//!   demons / jammers / pylons / props (overcharged in a 0-1 bar Dead Zone).
//! - **2+ bars: CAMERA FLASH STROBE** (`C` / pad D-Pad Up): a forward cone that
//!   blinds + stuns demons in line of sight and vaporizes weak incoming shots
//!   (heavy hellfire orbs survive).

use bevy::prelude::*;

use crate::apps::{app_block_reason, PhoneApps};
use crate::audio::{SfxEvent, SfxQueue};
use crate::boss::{damage_pylon, BossPylon, PYLON_SPHERES};
use crate::bot_input::{take_flag, BotControl};
use crate::enemies::{damage_enemy, segment_point_distance, Enemy, EnemyProjectile, PingMarked, HEAVY_BOLT_RADIUS};
use crate::fatality::KillCause;
use crate::fx::{spawn_burst, spawn_flash_light, spawn_lightning, spawn_particles, spawn_shockwave, BurstColor, CameraPunch, FxAssets, HudFeed};
use crate::gamepad::GamepadInput;
use crate::level::{DemonJammer, ExplosiveBarrel, LevelGrid};
use crate::physics::{has_line_of_sight, raycast_grid, sample_floor_height};
use crate::player::{live_eye_pose, CursorCaptured, Player, PlayerCamera};
use crate::props::PropHits;
use crate::rumble::{RumblePulse, RumbleQueue};
use crate::signal::SignalState;
use crate::state::PlayerModifiers;
use crate::style::{StyleAction, StyleQueue, StyleSource};
use crate::weapons::{
    barrel_center, damage_jammer, knockback_scale, parried_speed, spawn_parried_projectile, CombatDamage, ParryAssets, BARREL_RADIUS, JAMMER_SPHERES,
};

// ---------------------------------------------------------------------------
// Phone Punch
// ---------------------------------------------------------------------------

pub const PUNCH_COOLDOWN: f32 = 0.65;
/// Viewmodel jab length (the `punch_anim` envelope runs 1 -> 0 over this).
pub const PUNCH_ANIM_SECS: f32 = 0.32;
/// Melee reach (m) from the eye to a target's hit-volume surface.
pub const PUNCH_REACH: f32 = 3.4;
/// cos(40 deg): generous melee cone so fast bhop punches connect.
pub const PUNCH_CONE_COS: f32 = 0.766;
pub const PUNCH_DAMAGE: f32 = 45.0;
/// Dead-Zone static-arc overcharge damage (0-1 bars).
pub const PUNCH_OVERCHARGE_DAMAGE: f32 = 65.0;
/// Highest bar count that still overcharges the punch.
pub const PUNCH_OVERCHARGE_MAX_BARS: u8 = 1;
pub const PUNCH_KNOCKBACK: f32 = 18.0;
pub const PUNCH_STUN: f32 = 0.45;
/// Most demons one punch connects with (nearest first).
pub const PUNCH_MAX_TARGETS: usize = 3;
/// Interactive props (CRT / UPS) blast radius at the punch point.
pub const PUNCH_PROP_RADIUS: f32 = 2.2;
/// Distance of the punch point in front of the eye (clipped by walls).
pub const PUNCH_POINT_DIST: f32 = 1.6;

/// Projectile parry window: reach (m) and cos(48 deg) cone.
pub const PARRY_REACH: f32 = 4.4;
pub const PARRY_CONE_COS: f32 = 0.669;
/// Most projectiles a single punch returns (rewards are granted once).
pub const PARRY_MAX: usize = 3;
pub const PARRY_HEAL: f32 = 15.0;
/// Seconds of the golden parry accent (phone OLED frame + LED).
pub const PARRY_FLASH_SECS: f32 = 0.6;

/// Barrel launch: reach (m to the barrel surface), speed, lift and flight.
pub const BARREL_PUNCH_REACH: f32 = 3.6;
pub const BARREL_LAUNCH_SPEED: f32 = 28.0;
pub const BARREL_LAUNCH_LIFT: f32 = 0.14;
pub const BARREL_FLIGHT_SECS: f32 = 2.2;
/// A launched drum flies flat and heavy (half of the player's gravity).
pub const BARREL_LAUNCH_GRAVITY: f32 = 11.0;
/// Tumble rate (rad/s) of a launched barrel.
pub const BARREL_SPIN: f32 = 13.0;

/// Damage of a punch at `bars` signal: Dead-Zone (0-1 bars) static-arc overcharge.
pub fn punch_damage(bars: u8) -> f32 {
    if punch_overcharged(bars) { PUNCH_OVERCHARGE_DAMAGE } else { PUNCH_DAMAGE }
}

pub fn punch_overcharged(bars: u8) -> bool {
    bars <= PUNCH_OVERCHARGE_MAX_BARS
}

/// Whether a sphere (`center`, `radius`) is inside a forward cone from
/// `origin` along unit `fwd`: its surface within `reach`, and its center
/// inside the cone **or** the aim ray passing through it (big targets right
/// on the crosshair always count). Returns the surface distance.
pub fn sphere_in_cone(origin: Vec3, fwd: Vec3, center: Vec3, radius: f32, reach: f32, cone_cos: f32) -> Option<f32> {
    let to = center - origin;
    let d = to.length();
    let surface = (d - radius).max(0.0);
    if !surface.is_finite() || surface > reach {
        return None;
    }
    if d <= radius {
        return Some(0.0);
    }
    let along = to.dot(fwd);
    if along <= 0.0 {
        return None;
    }
    let cos = along / d;
    let off_axis = (to - fwd * along).length();
    (cos >= cone_cos || off_axis <= radius).then_some(surface)
}

/// Nearest-surface cone test over a target's hit spheres (`root` + `(height, radius)`).
pub fn target_in_cone(origin: Vec3, fwd: Vec3, root: Vec3, spheres: &[(f32, f32)], reach: f32, cone_cos: f32) -> Option<(f32, Vec3)> {
    spheres
        .iter()
        .filter_map(|&(h, r)| {
            let c = root + Vec3::Y * h;
            sphere_in_cone(origin, fwd, c, r, reach, cone_cos).map(|d| (d, c))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Nearest **visible** in-cone hit sphere so a step or catwalk lip blocking
/// one sphere never masks an exposed upper/lower sphere on the same target.
pub fn visible_target_in_cone(
    grid: &LevelGrid,
    origin: Vec3,
    fwd: Vec3,
    root: Vec3,
    spheres: &[(f32, f32)],
    reach: f32,
    cone_cos: f32,
) -> Option<(f32, Vec3)> {
    spheres
        .iter()
        .filter_map(|&(h, r)| {
            let c = root + Vec3::Y * h;
            let d = sphere_in_cone(origin, fwd, c, r, reach, cone_cos)?;
            has_line_of_sight(grid, origin, c).then_some((d, c))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Whether a drum of radius `BARREL_RADIUS` centered at `pos` grazes a wall,
/// floor, ramp or ceiling (catches corner/floor/ceiling skims the center ray misses).
fn barrel_grazes_terrain(grid: &LevelGrid, pos: Vec3) -> bool {
    let r = BARREL_RADIUS;
    for (ox, oz) in [(0.0, 0.0), (r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)] {
        match sample_floor_height(grid, pos.x + ox, pos.z + oz) {
            None => return true,
            Some(fy) if pos.y - r <= fy => return true,
            Some(_) => {}
        }
    }
    raycast_grid(grid, pos, Vec3::Y, r).is_some()
}

/// Parry targets among `(position, radius)` projectiles: inside the parry
/// window, closest + most centered first (score = distance x (2 - cos)), at
/// most [`PARRY_MAX`]. Returns indices into `shots`.
pub fn parry_picks(origin: Vec3, fwd: Vec3, shots: &[(Vec3, f32)]) -> Vec<usize> {
    let mut picks: Vec<(f32, usize)> = shots
        .iter()
        .enumerate()
        .filter_map(|(i, &(p, r))| {
            sphere_in_cone(origin, fwd, p, r, PARRY_REACH, PARRY_CONE_COS).map(|_| {
                let to = p - origin;
                let d = to.length().max(1e-3);
                let cos = (to.dot(fwd) / d).clamp(-1.0, 1.0);
                (d * (2.0 - cos), i)
            })
        })
        .collect();
    picks.sort_by(|a, b| a.0.total_cmp(&b.0));
    picks.into_iter().take(PARRY_MAX).map(|(_, i)| i).collect()
}

/// Launch velocity of a punched barrel: along the aim, never into the floor
/// (downward aim is flattened), plus a slight lift.
pub fn barrel_launch_velocity(aim: Vec3) -> Vec3 {
    let a = aim.normalize_or(Vec3::NEG_Z);
    let level = Vec3::new(a.x, a.y.max(0.0), a.z).normalize_or(Vec3::NEG_Z);
    (level + Vec3::Y * BARREL_LAUNCH_LIFT).normalize() * BARREL_LAUNCH_SPEED
}

/// A coolant barrel sent flying by a Phone Punch. Detonates on its first
/// impact (terrain, demon, jammer, pylon, another barrel) or after
/// [`BARREL_FLIGHT_SECS`], through the regular barrel blast (chain reactions
/// included) credited to [`StyleSource::PhonePunch`], with no self-damage.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct LaunchedBarrel {
    pub vel: Vec3,
    pub life: f32,
    /// Tumble axis (unit, world space).
    pub spin_axis: Vec3,
    /// Seconds until the next fire-trail puff.
    pub trail: f32,
}

impl LaunchedBarrel {
    pub fn new(aim: Vec3) -> Self {
        let vel = barrel_launch_velocity(aim);
        let spin_axis = Vec3::Y.cross(vel).normalize_or(Vec3::X);
        LaunchedBarrel { vel, life: BARREL_FLIGHT_SECS, spin_axis, trail: 0.0 }
    }
}

// ---------------------------------------------------------------------------
// Camera Flash Strobe
// ---------------------------------------------------------------------------

pub const FLASH_MIN_BARS: u8 = 2;
pub const FLASH_COOLDOWN: f32 = 7.5;
/// Viewmodel lift + white-hot LED length (the `flash_anim` envelope).
pub const FLASH_ANIM_SECS: f32 = 0.35;
pub const FLASH_RANGE: f32 = 16.0;
/// cos(60 deg) half-angle.
pub const FLASH_CONE_COS: f32 = 0.5;
pub const FLASH_STUN: f32 = 2.6;
pub const FLASH_DAMAGE: f32 = 18.0;

/// Camera Flash cooldown after a strobe (Survey OS / Packet Sniffer speed it
/// up like the other tactical phone tools).
pub fn flash_cooldown(mods: &PlayerModifiers) -> f32 {
    FLASH_COOLDOWN * mods.ping_cooldown_mult
}

/// Why the flash cannot fire (`None` = ready).
pub fn flash_block_reason(bars: u8, cooldown: f32) -> Option<String> {
    app_block_reason(bars, FLASH_MIN_BARS, cooldown)
}

/// Weak shots the strobe vaporizes; heavy hellfire orbs survive it.
pub fn flash_clears(radius: f32) -> bool {
    radius < HEAVY_BOLT_RADIUS
}

/// Result toast of a strobe.
pub fn flash_toast(blinded: u32, cleared: u32) -> String {
    match (blinded, cleared) {
        (b, 0) => format!("CAMERA FLASH: {b} BLINDED"),
        (b, c) => format!("CAMERA FLASH: {b} BLINDED + {c} SHOTS CLEARED"),
    }
}

/// Flash cone / ring glows `(color, alpha)` (prewarmed).
const FLASH_CORE_GLOW: (Color, f32) = (Color::srgb(1.0, 0.97, 0.86), 0.55);
const FLASH_WASH_GLOW: (Color, f32) = (Color::srgb(1.0, 0.84, 0.55), 0.22);
/// Punch impact ring / overcharged static ring.
const PUNCH_RING_GLOW: (Color, f32) = (Color::srgb(1.0, 0.72, 0.35), 0.75);
const OVERCHARGE_RING_GLOW: (Color, f32) = (Color::srgb(0.35, 0.95, 1.0), 0.8);
const PARRY_RING_GLOW: (Color, f32) = (Color::srgb(1.0, 0.86, 0.35), 0.85);

fn prewarm_phone_combat_fades(fx: Res<FxAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    fx.prewarm_fades(&mut materials, &[FLASH_CORE_GLOW, FLASH_WASH_GLOW, PUNCH_RING_GLOW, OVERCHARGE_RING_GLOW, PARRY_RING_GLOW]);
}

/// A point `dist` in front of `eye` along `fwd`, pulled back off any wall.
fn clipped_point(grid: &LevelGrid, eye: Vec3, fwd: Vec3, dist: f32) -> Vec3 {
    let d = raycast_grid(grid, eye, fwd, dist).map_or(dist, |h| (h.distance - 0.1).max(0.05));
    eye + fwd * d
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Phone Punch: parry > barrel launch > melee, on this frame's eye pose.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn phone_punch(
    mut commands: Commands,
    (keys, mouse, captured): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<CursorCaptured>),
    grid: Res<LevelGrid>,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    mut apps: ResMut<PhoneApps>,
    (fx, parry_assets, mut materials): (Res<FxAssets>, Res<ParryAssets>, ResMut<Assets<StandardMaterial>>),
    (mut sfx, mut feed, mut style, mut rumble, mut cam_punch): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<StyleQueue>, ResMut<RumbleQueue>, ResMut<CameraPunch>),
    (mut bot, pad, mut prop_hits): (Option<ResMut<BotControl>>, Option<Res<GamepadInput>>, ResMut<PropHits>),
    cams: Query<&Transform, With<PlayerCamera>>,
    mut players: Query<(&mut Player, &Transform), Without<PlayerCamera>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    shots: Query<(Entity, &EnemyProjectile, &Transform)>,
    (mut jammers, mut pylons, barrels): (
        Query<(&mut DemonJammer, &GlobalTransform)>,
        Query<(&mut BossPylon, &GlobalTransform)>,
        Query<(Entity, &ExplosiveBarrel, &Transform), Without<LaunchedBarrel>>,
    ),
) {
    let bot_punch = take_flag(&mut bot, |b| &mut b.punch);
    let pressed = keys.just_pressed(KeyCode::KeyV) || (captured.0 && mouse.just_pressed(MouseButton::Middle)) || pad.is_some_and(|g| g.punch) || bot_punch;
    if !pressed || apps.punch_cooldown > 0.0 {
        return;
    }
    let Ok((mut player, body)) = players.single_mut() else { return };
    if player.is_dead() {
        return;
    }
    let Ok(cam_local) = cams.single() else { return };
    // This frame's eye pose (moved + turned earlier in `Update`).
    let cam = live_eye_pose(body, cam_local);
    let eye = cam.translation;
    let fwd = (cam.rotation * Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
    let right = cam.rotation * Vec3::X;
    apps.punch_cooldown = PUNCH_COOLDOWN;
    apps.punch_anim = 1.0;
    apps.punch_hit = false;
    sfx.play(SfxEvent::PhonePunch);
    let fist = clipped_point(&grid, eye, fwd, 0.85);

    // ---- 1. PARRY (highest priority) ----
    let list: Vec<(Entity, Vec3, f32, f32)> = shots
        .iter()
        .filter(|(_, _, tf)| has_line_of_sight(&grid, eye, tf.translation))
        .map(|(e, p, tf)| (e, tf.translation, p.radius, p.vel.length()))
        .collect();
    let picks = parry_picks(eye, fwd, &list.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>());
    if !picks.is_empty() {
        let n = picks.len();
        let start = clipped_point(&grid, eye, fwd, 0.9);
        for (k, &i) in picks.iter().enumerate() {
            let (e, pos, _, speed) = list[i];
            commands.entity(e).try_despawn();
            let spread = right * ((k as f32 - (n as f32 - 1.0) * 0.5) * 0.22);
            spawn_parried_projectile(&mut commands, &parry_assets, start + spread, fwd, parried_speed(speed));
            spawn_burst(&mut commands, &fx, pos, BurstColor::Gold, 12, 7.5, 0.08);
        }
        spawn_shockwave(&mut commands, &fx, &mut materials, start, 0.9, 0.16, PARRY_RING_GLOW.0, PARRY_RING_GLOW.1, false);
        spawn_flash_light(&mut commands, start, Color::srgb(1.0, 0.85, 0.4), 900_000.0, 10.0, 0.18);
        // Instant dopamine: heal, dash reset, golden flash, style, rumble.
        player.heal(PARRY_HEAL);
        player.dash_cooldown = 0.0;
        apps.punch_hit = true;
        apps.parry_flash = PARRY_FLASH_SECS;
        feed.hit(true);
        feed.toast("PARRY // RETURN TO SENDER!", Color::srgb(1.0, 0.85, 0.3));
        cam_punch.kick(0.55);
        rumble.push(RumblePulse::Parry);
        sfx.play(SfxEvent::PhoneParry);
        style.push(StyleAction::Parry);
        return;
    }

    // ---- 2. VOLATILE BARREL LAUNCH ----
    let barrel = barrels
        .iter()
        .filter(|(_, b, _)| b.is_intact())
        .filter_map(|(e, _, tf)| {
            let c = barrel_center(tf);
            sphere_in_cone(eye, fwd, c, BARREL_RADIUS, BARREL_PUNCH_REACH, PUNCH_CONE_COS).filter(|_| has_line_of_sight(&grid, eye, c)).map(|d| (d, e, c))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, e, c)) = barrel {
        commands.entity(e).try_insert(LaunchedBarrel::new(fwd));
        spawn_burst(&mut commands, &fx, c, BurstColor::Spark, 10, 6.0, 0.07);
        spawn_shockwave(&mut commands, &fx, &mut materials, c, 0.8, 0.14, PUNCH_RING_GLOW.0, PUNCH_RING_GLOW.1, false);
        apps.punch_hit = true;
        feed.hit(false);
        feed.toast("BARREL LAUNCHED!", Color::srgb(1.0, 0.55, 0.2));
        cam_punch.kick(0.3);
        rumble.push(RumblePulse::PhonePunch);
        sfx.play(SfxEvent::Hit);
        return;
    }

    // ---- 3. MELEE ----
    let overcharged = punch_overcharged(signal.bars);
    let dmg = punch_damage(signal.bars);
    let mut targets: Vec<(f32, usize, Vec3)> = enemies
        .iter()
        .enumerate()
        .filter(|(_, (e, _, _))| e.is_alive())
        .filter_map(|(i, (e, g, _))| {
            visible_target_in_cone(&grid, eye, fwd, g.translation(), e.stats.hit_spheres, PUNCH_REACH, PUNCH_CONE_COS)
                .map(|(d, c)| (d, i, c))
        })
        .collect();
    targets.sort_by(|a, b| a.0.total_cmp(&b.0));
    targets.truncate(PUNCH_MAX_TARGETS);
    let mut hits = 0u32;
    let mut kills = 0u32;
    for &(_, i, c) in &targets {
        let Some((mut en, g, marked)) = enemies.iter_mut().nth(i) else { continue };
        let root = g.translation();
        let away = Vec3::new(root.x - eye.x, 0.0, root.z - eye.z).normalize_or(Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z));
        damage_enemy(&mut en, dmg, marked.is_some(), &mods);
        en.style_source = StyleSource::PhonePunch;
        if !en.is_alive() {
            en.kill_cause = KillCause::PhonePunch;
            kills += 1;
        }
        let kb = PUNCH_KNOCKBACK * knockback_scale(en.kind);
        en.knockback += Vec3::new(away.x * kb, kb * 0.12, away.z * kb);
        en.stagger(away);
        en.stun_timer = en.stun_timer.max(PUNCH_STUN);
        hits += 1;
        spawn_burst(&mut commands, &fx, c, BurstColor::Spark, 9, 6.5, 0.07);
        spawn_particles(&mut commands, &fx, c, BurstColor::Gore, 5, 4.5, 0.08);
        if overcharged {
            // Dead-Zone static arc: cyan / red sparks + a crackling arc.
            spawn_lightning(&mut commands, &fx, fist, c);
            spawn_particles(&mut commands, &fx, c, BurstColor::Cyan, 8, 7.0, 0.06);
            spawn_particles(&mut commands, &fx, c, BurstColor::Fire, 4, 6.0, 0.05);
        }
    }
    for (mut j, g) in &mut jammers {
        if j.is_active()
            && let Some((_, c)) = visible_target_in_cone(&grid, eye, fwd, g.translation(), &JAMMER_SPHERES, PUNCH_REACH, PUNCH_CONE_COS)
        {
            damage_jammer(&mut j, dmg);
            spawn_burst(&mut commands, &fx, c, BurstColor::Spark, 8, 6.0, 0.07);
            hits += 1;
        }
    }
    for (mut p, g) in &mut pylons {
        if p.is_alive()
            && let Some((_, c)) = visible_target_in_cone(&grid, eye, fwd, g.translation(), &PYLON_SPHERES, PUNCH_REACH, PUNCH_CONE_COS)
        {
            damage_pylon(&mut p, dmg);
            spawn_burst(&mut commands, &fx, c, BurstColor::Cyan, 8, 6.0, 0.07);
            hits += 1;
        }
    }
    // CRTs shatter / UPS racks trip at the punch point.
    prop_hits.blast(clipped_point(&grid, eye, fwd, PUNCH_POINT_DIST), PUNCH_PROP_RADIUS, dmg, false);
    if hits > 0 {
        apps.punch_hit = true;
        feed.hit(kills > 0);
        sfx.play(SfxEvent::Hit);
        rumble.push(RumblePulse::PhonePunch);
        cam_punch.kick(if overcharged { 0.4 } else { 0.28 });
        let ring = if overcharged { OVERCHARGE_RING_GLOW } else { PUNCH_RING_GLOW };
        spawn_shockwave(&mut commands, &fx, &mut materials, targets.first().map_or(fist, |t| t.2), 0.75, 0.14, ring.0, ring.1, false);
        if overcharged && kills > 0 {
            feed.toast("DEAD-ZONE KNOCKOUT! // STATIC OVERCHARGE", Color::srgb(0.35, 0.95, 1.0));
        }
    }
}

/// Fly punched barrels: gravity, tumble, fire trail; detonate on first impact.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn move_launched_barrels(
    mut commands: Commands,
    time: Res<Time>,
    grid: Res<LevelGrid>,
    fx: Res<FxAssets>,
    mut flying: Query<(Entity, &mut LaunchedBarrel, &mut ExplosiveBarrel, &mut Transform)>,
    others: Query<(Entity, &ExplosiveBarrel, &Transform), Without<LaunchedBarrel>>,
    enemies: Query<(&Enemy, &GlobalTransform)>,
    (jammers, pylons): (Query<(&DemonJammer, &GlobalTransform)>, Query<(&BossPylon, &GlobalTransform)>),
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    // Snapshot every intact barrel (stationary and airborne) so two flying drums also collide.
    let mut barrel_centers: Vec<(Entity, Vec3)> = Vec::new();
    for (e, _, b, tf) in &flying {
        if b.is_intact() {
            barrel_centers.push((e, barrel_center(tf)));
        }
    }
    for (e, b, tf) in &others {
        if b.is_intact() {
            barrel_centers.push((e, barrel_center(tf)));
        }
    }
    for (e, mut lb, mut b, mut tf) in &mut flying {
        if !b.is_intact() {
            // Shot mid-air: its fuse is already burning; let it drop where it is.
            continue;
        }
        lb.life -= dt;
        lb.vel.y -= BARREL_LAUNCH_GRAVITY * dt;
        let from = barrel_center(&tf);
        let step = lb.vel * dt;
        let len = step.length();
        let dir = step / len.max(1e-6);
        let to = from + step;
        // Terrain (walls, floor, ramps, ceiling), including corner / shallow-skim grazing.
        let mut impact = raycast_grid(&grid, from, dir, len + BARREL_RADIUS)
            .map(|h| from + dir * (h.distance - BARREL_RADIUS * 0.8).max(0.0))
            .or_else(|| barrel_grazes_terrain(&grid, to).then_some(to));
        let touches = |c: Vec3, r: f32| segment_point_distance(from, to, c) <= r + BARREL_RADIUS;
        let mut hit_target = enemies
            .iter()
            .any(|(en, g)| en.is_alive() && en.stats.hit_spheres.iter().any(|&(h, r)| touches(g.translation() + Vec3::Y * h, r)));
        hit_target |= jammers.iter().any(|(j, g)| j.is_active() && JAMMER_SPHERES.iter().any(|&(h, r)| touches(g.translation() + Vec3::Y * h, r)));
        hit_target |= pylons.iter().any(|(p, g)| p.is_alive() && PYLON_SPHERES.iter().any(|&(h, r)| touches(g.translation() + Vec3::Y * h, r)));
        hit_target |= barrel_centers.iter().any(|&(o, oc)| o != e && touches(oc, BARREL_RADIUS));
        if hit_target && impact.is_none() {
            impact = Some(to);
        }
        let expired = lb.life <= 0.0;
        let center = impact.unwrap_or(to);
        tf.rotation = (Quat::from_axis_angle(lb.spin_axis, BARREL_SPIN * dt) * tf.rotation).normalize();
        tf.translation = center - tf.rotation * (Vec3::Y * crate::weapons::BARREL_CENTER_Y);
        if impact.is_some() || expired {
            // Through the regular barrel blast (same frame, chain reactions,
            // PhonePunch credit, no self-damage).
            b.hp = 0.0;
            b.fuse = Some(0.0);
            continue;
        }
        lb.trail -= dt;
        if lb.trail <= 0.0 {
            lb.trail += 0.04;
            spawn_particles(&mut commands, &fx, center - dir * 0.4, BurstColor::Fire, 1, 1.2, 0.06);
            spawn_particles(&mut commands, &fx, center - dir * 0.5, BurstColor::Smoke, 1, 0.6, 0.1);
        }
    }
}

/// Camera Flash Strobe (`C`, >= 2 bars): blind + stun demons in a forward
/// cone with line of sight and vaporize weak incoming shots.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn camera_flash(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    grid: Res<LevelGrid>,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    mut apps: ResMut<PhoneApps>,
    (fx, mut materials): (Res<FxAssets>, ResMut<Assets<StandardMaterial>>),
    (mut sfx, mut feed, mut style): (ResMut<SfxQueue>, ResMut<HudFeed>, ResMut<StyleQueue>),
    (mut bot, pad): (Option<ResMut<BotControl>>, Option<Res<GamepadInput>>),
    cams: Query<&Transform, With<PlayerCamera>>,
    players: Query<(&Player, &Transform), Without<PlayerCamera>>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform, Option<&PingMarked>)>,
    shots: Query<(Entity, &EnemyProjectile, &Transform)>,
) {
    let bot_flash = take_flag(&mut bot, |b| &mut b.flash);
    if !(keys.just_pressed(KeyCode::KeyC) || pad.is_some_and(|g| g.flash) || bot_flash) {
        return;
    }
    let Ok((player, body)) = players.single() else { return };
    if player.is_dead() {
        return;
    }
    if let Some(why) = flash_block_reason(signal.bars, apps.flash_cooldown) {
        sfx.play(SfxEvent::Denied);
        feed.toast(format!("CAMERA FLASH: {why}"), Color::srgb(1.0, 0.5, 0.2));
        return;
    }
    let Ok(cam_local) = cams.single() else { return };
    let cam = live_eye_pose(body, cam_local);
    let eye = cam.translation;
    let fwd = (cam.rotation * Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
    apps.flash_cooldown = flash_cooldown(&mods);
    apps.flash_anim = 1.0;
    sfx.play(SfxEvent::CameraFlash);

    let mut blinded = 0u32;
    for (mut en, g, marked) in &mut enemies {
        if !en.is_alive() {
            continue;
        }
        let root = g.translation();
        let Some((_, c)) = visible_target_in_cone(&grid, eye, fwd, root, en.stats.hit_spheres, FLASH_RANGE, FLASH_CONE_COS) else { continue };
        damage_enemy(&mut en, FLASH_DAMAGE, marked.is_some(), &mods);
        en.style_source = StyleSource::CameraFlash;
        en.stun_timer = en.stun_timer.max(FLASH_STUN);
        // Blinded: drop any Relay Sniper lock-on / Phase Stalker blink charge.
        en.sniper_lock_timer = 0.0;
        en.blink_timer = 0.0;
        en.stagger(root - eye);
        spawn_particles(&mut commands, &fx, c, BurstColor::Spark, 4, 3.5, 0.05);
        blinded += 1;
    }
    let mut cleared = 0u32;
    for (e, p, tf) in &shots {
        let pos = tf.translation;
        if flash_clears(p.radius) && sphere_in_cone(eye, fwd, pos, p.radius, FLASH_RANGE, FLASH_CONE_COS).is_some() && has_line_of_sight(&grid, eye, pos) {
            commands.entity(e).try_despawn();
            spawn_burst(&mut commands, &fx, pos, BurstColor::Spark, 6, 5.0, 0.06);
            cleared += 1;
        }
    }

    // Blinding white-amber flare + expanding flash cone in front of the phone.
    let near = clipped_point(&grid, eye, fwd, 1.4);
    let mid = clipped_point(&grid, eye, fwd, 5.5);
    spawn_flash_light(&mut commands, near, Color::srgb(1.0, 0.93, 0.78), 4_200_000.0, FLASH_RANGE + 4.0, 0.2);
    spawn_shockwave(&mut commands, &fx, &mut materials, near, 0.9, 0.16, FLASH_CORE_GLOW.0, FLASH_CORE_GLOW.1, false);
    if mid.distance(eye) > 3.5 {
        spawn_shockwave(&mut commands, &fx, &mut materials, mid, 3.0, 0.22, FLASH_WASH_GLOW.0, FLASH_WASH_GLOW.1, false);
    }
    spawn_particles(&mut commands, &fx, near, BurstColor::Spark, 8, 7.0, 0.04);

    if blinded > 0 || cleared > 0 {
        feed.hit(false);
        style.push(StyleAction::CameraFlash { blinded, cleared });
    }
    feed.toast(flash_toast(blinded, cleared), Color::srgb(1.0, 0.95, 0.75));
}

pub struct PhoneCombatPlugin;

impl Plugin for PhoneCombatPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, prewarm_phone_combat_fades).add_systems(
            Update,
            (phone_punch, camera_flash, move_launched_barrels)
                .chain()
                .after(crate::apps::phone_apps)
                .before(crate::enemies::enemy_ai)
                .before(crate::weapons::move_player_projectiles)
                .in_set(CombatDamage)
                .run_if(resource_exists::<LevelGrid>)
                .run_if(crate::trials::not_time_trial),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punch_damage_overcharges_in_the_dead_zone_and_never_needs_signal() {
        assert_eq!(punch_damage(0), PUNCH_OVERCHARGE_DAMAGE);
        assert_eq!(punch_damage(1), PUNCH_OVERCHARGE_DAMAGE);
        for bars in 2..=5 {
            assert_eq!(punch_damage(bars), PUNCH_DAMAGE, "{bars} bars");
        }
        const { assert!(PUNCH_OVERCHARGE_DAMAGE > PUNCH_DAMAGE) };
        assert_eq!((PUNCH_DAMAGE, PUNCH_OVERCHARGE_DAMAGE, PUNCH_COOLDOWN), (45.0, 65.0, 0.65));
    }

    #[test]
    fn cone_tests_are_generous_but_respect_reach_and_facing() {
        let o = Vec3::ZERO;
        let f = Vec3::NEG_Z;
        // Dead ahead, in reach.
        assert!(sphere_in_cone(o, f, Vec3::new(0.0, 0.0, -3.0), 0.5, PUNCH_REACH, PUNCH_CONE_COS).is_some());
        // Out of reach.
        assert!(sphere_in_cone(o, f, Vec3::new(0.0, 0.0, -4.5), 0.5, PUNCH_REACH, PUNCH_CONE_COS).is_none());
        // Behind the player.
        assert!(sphere_in_cone(o, f, Vec3::new(0.0, 0.0, 2.0), 0.5, PUNCH_REACH, PUNCH_CONE_COS).is_none());
        // 35 deg off-axis is inside the 40 deg punch cone; 55 deg is not.
        let at = |deg: f32, d: f32| Vec3::new(deg.to_radians().sin(), 0.0, -deg.to_radians().cos()) * d;
        assert!(sphere_in_cone(o, f, at(35.0, 2.5), 0.1, PUNCH_REACH, PUNCH_CONE_COS).is_some());
        assert!(sphere_in_cone(o, f, at(55.0, 2.5), 0.1, PUNCH_REACH, PUNCH_CONE_COS).is_none());
        // A big body the aim ray passes through counts even off the cone.
        assert!(sphere_in_cone(o, f, Vec3::new(1.4, 0.0, -1.0), 1.5, PUNCH_REACH, PUNCH_CONE_COS).is_some());
        // Overlapping the eye.
        assert_eq!(sphere_in_cone(o, f, Vec3::new(0.1, 0.0, 0.1), 0.5, PUNCH_REACH, PUNCH_CONE_COS), Some(0.0));
        // The parry window is wider + longer than the punch.
        const { assert!(PARRY_REACH > PUNCH_REACH && PARRY_CONE_COS < PUNCH_CONE_COS) };
        assert!(sphere_in_cone(o, f, at(45.0, 4.2), 0.1, PARRY_REACH, PARRY_CONE_COS).is_some());
        // Camera flash: 60 deg / 16 m.
        assert!(sphere_in_cone(o, f, at(58.0, 15.0), 0.3, FLASH_RANGE, FLASH_CONE_COS).is_some());
        assert!(sphere_in_cone(o, f, at(70.0, 6.0), 0.3, FLASH_RANGE, FLASH_CONE_COS).is_none());
        assert!(sphere_in_cone(o, f, at(0.0, 17.5), 0.3, FLASH_RANGE, FLASH_CONE_COS).is_none());
        // NaN-safe.
        assert!(sphere_in_cone(o, f, Vec3::NAN, 0.3, FLASH_RANGE, FLASH_CONE_COS).is_none());
    }

    #[test]
    fn parry_prefers_the_closest_most_centered_shots_and_caps_the_count() {
        let o = Vec3::ZERO;
        let f = Vec3::NEG_Z;
        let shots = [
            (Vec3::new(0.0, 0.0, -4.0), 0.2), // far but centered
            (Vec3::new(1.2, 0.0, -1.6), 0.2), // near, off-axis
            (Vec3::new(0.0, 0.0, 3.0), 0.2),  // behind
            (Vec3::new(0.0, 0.0, -9.0), 0.2), // out of reach
            (Vec3::new(0.0, 0.1, -1.0), 0.2), // near + centered
        ];
        let picks = parry_picks(o, f, &shots);
        assert_eq!(picks[0], 4, "near + centered first");
        assert!(!picks.contains(&2) && !picks.contains(&3));
        assert_eq!(picks.len(), 3);
        let many: Vec<_> = (0..8).map(|i| (Vec3::new(0.0, 0.0, -1.0 - i as f32 * 0.3), 0.2)).collect();
        assert_eq!(parry_picks(o, f, &many).len(), PARRY_MAX);
        assert!(parry_picks(o, f, &[]).is_empty());
    }

    #[test]
    fn parried_bolts_are_supercharged() {
        use crate::weapons::{parried_speed, BlastSpec, PARRY_DIRECT_DAMAGE, PARRY_MIN_SPEED, PARRY_SPLASH_DAMAGE, PARRY_SPLASH_RADIUS};
        assert_eq!(parried_speed(10.0), PARRY_MIN_SPEED, "slow imp bolts get the floor speed");
        assert!((parried_speed(40.0) - 88.0).abs() < 1e-3, "2.2x a fast shot");
        assert_eq!(parried_speed(f32::NAN), PARRY_MIN_SPEED);
        let spec = BlastSpec::parried();
        assert_eq!((spec.direct, spec.splash, spec.radius), (PARRY_DIRECT_DAMAGE, PARRY_SPLASH_DAMAGE, PARRY_SPLASH_RADIUS));
        assert_eq!((spec.direct, spec.splash, spec.radius), (115.0, 95.0, 4.5));
        assert!(!spec.player_jump, "the parrier never eats their own bolt");
        assert_eq!(spec.source, StyleSource::Parry);
        assert_eq!(spec.direct_cause, KillCause::ParryDirect);
        assert!(spec.direct_cause.guaranteed() && KillCause::PhonePunch.guaranteed());
        assert!((spec.splash_damage(0.0, 1.0) - 95.0).abs() < 1e-3);
        assert_eq!(spec.splash_damage(PARRY_SPLASH_RADIUS, 1.0), 0.0);
    }

    #[test]
    fn barrel_launch_flies_forward_with_lift_and_never_into_the_floor() {
        let v = barrel_launch_velocity(Vec3::NEG_Z);
        assert!((v.length() - BARREL_LAUNCH_SPEED).abs() < 1e-3);
        assert!(v.z < -27.0 && v.y > 3.0, "{v}");
        // Aiming down at a barrel by the boots still sends it forward.
        let down = barrel_launch_velocity(Vec3::new(0.0, -0.8, -0.6));
        assert!(down.y > 0.0 && down.z < -20.0, "{down}");
        // Aiming up keeps the arc.
        let up = barrel_launch_velocity(Vec3::new(0.0, 0.5, -0.86));
        assert!(up.y > v.y);
        let lb = LaunchedBarrel::new(Vec3::X);
        assert!((lb.spin_axis.length() - 1.0).abs() < 1e-4 && lb.life == BARREL_FLIGHT_SECS);
        assert!(LaunchedBarrel::new(Vec3::ZERO).vel.is_finite());
    }

    #[test]
    fn camera_flash_gate_cooldown_and_heavy_bolt_rule() {
        assert_eq!(flash_block_reason(0, 0.0).as_deref(), Some("NEED 2 BARS"));
        assert_eq!(flash_block_reason(1, 0.0).as_deref(), Some("NEED 2 BARS"));
        assert_eq!(flash_block_reason(2, 0.0), None);
        assert_eq!(flash_block_reason(5, 3.25).as_deref(), Some("3.2s"));
        let mods = PlayerModifiers::default();
        assert!((flash_cooldown(&mods) - FLASH_COOLDOWN * mods.ping_cooldown_mult).abs() < 1e-6);
        let fast = PlayerModifiers { ping_cooldown_mult: 0.5, ..default() };
        assert!(flash_cooldown(&fast) < flash_cooldown(&mods), "Survey OS / Packet Sniffer speed it up");
        assert!(flash_clears(0.18) && flash_clears(HEAVY_BOLT_RADIUS - 0.01));
        assert!(!flash_clears(HEAVY_BOLT_RADIUS) && !flash_clears(0.6), "hellfire orbs survive");
        assert_eq!(flash_toast(3, 0), "CAMERA FLASH: 3 BLINDED");
        assert_eq!(flash_toast(1, 2), "CAMERA FLASH: 1 BLINDED + 2 SHOTS CLEARED");
        assert_eq!((FLASH_MIN_BARS, FLASH_COOLDOWN, FLASH_STUN, FLASH_DAMAGE), (2, 7.5, 2.6, 18.0));
    }

    #[test]
    fn phone_strike_pose_jabs_forward_then_settles_and_flash_lifts() {
        use crate::weapons::{phone_strike_pose, punch_thrust};
        assert_eq!(punch_thrust(0.0), 0.0);
        assert!((punch_thrust(0.25) - 1.0).abs() < 1e-4, "full extension fast");
        assert!(punch_thrust(0.12) > 0.7, "snappy ease-out");
        assert!(punch_thrust(1.0).abs() < 1e-4, "settles home");
        assert!(punch_thrust(0.6) < punch_thrust(0.3));
        let (rest, rot) = phone_strike_pose(0.0, false, 0.0);
        assert_eq!(rest, Vec3::ZERO);
        assert!(rot.angle_between(Quat::IDENTITY) < 1e-5);
        // 1 - 0.25 progress = peak thrust: forward (-Z) and toward center (+X).
        let (peak, prot) = phone_strike_pose(0.75, true, 0.0);
        assert!(peak.z < -0.2 && peak.x > 0.1, "{peak}");
        assert!(prot.angle_between(Quat::IDENTITY) > 0.5, "visible knuckle twist");
        let (lift, _) = phone_strike_pose(0.0, false, 0.6);
        assert!(lift.y > 0.05, "flash lifts the phone: {lift}");
    }
}
