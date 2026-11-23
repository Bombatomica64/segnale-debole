//! SOS Extraction: `Searching -> Uploading -> Extracting -> Complete`.
//!
//! Trigger the uplink at the Signal Tower (`E` inside the Signal Ring, or stand
//! by the mast for 1.5s). While uploading, a Demon Rush converges, a Hell Baron
//! drops in at 50%, progress runs at 100% inside the ring, 20% outside and 0%
//! while a Jammer Fiend stands in the ring. At 100% an orbital beam vaporizes
//! every demon and the floor is completed.

use bevy::prelude::*;
use rand::Rng;
use rand::seq::IndexedRandom;

use crate::audio::{SfxEvent, SfxQueue};
use crate::bot_input::{take_flag, BotControl};
use crate::enemies::{spawn_enemy, spawn_pos_for, Enemy, EnemyAssets, EnemyKind, EnemyProjectile};
use crate::fx::{additive_glow, spawn_beam, spawn_burst, spawn_flash_light, BurstColor, FxAssets, HudFeed};
use crate::level::{Cell, LevelEntity, LevelGrid, TowerBeacon};
use crate::physics::has_line_of_sight;
use crate::player::{Player, PLAYER_EYE_HEIGHT};
use crate::signal::{SignalState, TOWER_RING_RADIUS};
use crate::state::{complete_floor, AppState, GameSet, PlayerModifiers, RunStats};
use crate::weapons::{spawn_pickup, CombatDamage, PickupAssets, PickupKind};

pub const UPLINK_HOLD_RADIUS: f32 = 4.5;
pub const UPLINK_HOLD_SECS: f32 = 1.5;
pub const OUTSIDE_RING_RATE: f32 = 0.2;
pub const EXTRACT_SECS: f32 = 1.6;
pub const SKY_COLOR: Color = Color::srgb(0.32, 0.04, 0.03);

#[derive(Resource, Debug, Clone, PartialEq)]
pub enum ExtractionState {
    Searching {
        /// Seconds spent standing by the mast (auto-trigger at 1.5s).
        hold: f32,
    },
    Uploading {
        progress: f32,
        duration: f32,
        in_ring: bool,
        jammed: bool,
        wave_timer: f32,
        baron_spawned: bool,
    },
    Extracting {
        timer: f32,
    },
    Complete,
}

impl Default for ExtractionState {
    fn default() -> Self {
        ExtractionState::Searching { hold: 0.0 }
    }
}

/// Seconds of uplink at 100% rate for a floor.
pub fn upload_duration(floor: u32) -> f32 {
    24.0 + (floor.max(1) - 1) as f32 * 5.0
}

/// Upload speed multiplier.
/// Firewall Daemon armor regen runs only while the SOS advances in the ring.
pub fn firewall_regen_active(in_ring: bool, jammed: bool) -> bool {
    in_ring && !jammed
}

pub fn upload_rate(in_ring: bool, jammed: bool) -> f32 {
    if jammed {
        0.0
    } else if in_ring {
        1.0
    } else {
        OUTSIDE_RING_RATE
    }
}

/// A living Jammer Fiend inside the Signal Ring jams the uplink.
pub fn fiend_jams_uplink(fiends: &[Vec3], tower: Vec3) -> bool {
    fiends.iter().any(|f| Vec2::new(f.x - tower.x, f.z - tower.z).length() <= TOWER_RING_RADIUS)
}

/// Max living demons during the rush (FPS + fairness cap).
pub fn rush_cap(floor: u32) -> usize {
    9 + 2 * floor.max(1) as usize
}

/// Seconds between Demon Rush waves: 5.0..6.5s on Floor 1, tighter deeper down.
pub fn wave_interval(floor: u32) -> std::ops::Range<f32> {
    let tighten = (floor.max(1) - 1).min(4) as f32 * 0.25;
    (5.0 - tighten)..(6.5 - tighten)
}

/// [`wave_interval`] scaled by the Overtime Uplink contract (< 1 = faster waves).
pub fn scaled_wave_interval(floor: u32, mult: f32) -> std::ops::Range<f32> {
    let r = wave_interval(floor);
    let m = mult.max(0.05);
    (r.start * m)..(r.end * m)
}

/// Jammer Fiend odds in a Demon Rush wave: `(max living fiends, roll chance)`.
/// Priority Jamming raises both.
pub fn rush_fiend_odds(priority_jamming: bool) -> (usize, f32) {
    if priority_jamming { (3, 0.45) } else { (2, 0.3) }
}

/// Seconds between X-ray beacons on a jamming Jammer Fiend.
pub const FIEND_BEACON_SECS: f32 = 1.1;

/// Walkable atrium cells just inside the Signal Ring perimeter (supply drops),
/// spread around the tower: ordered so consecutive picks sit far apart.
pub fn ring_supply_cells(grid: &LevelGrid) -> Vec<IVec2> {
    let tower = grid.tower_world();
    let r = ring_radius_at(0.0, tower.y);
    let mut cells: Vec<(IVec2, f32)> = (0..grid.depth)
        .flat_map(|z| (0..grid.width).map(move |x| IVec2::new(x, z)))
        .filter(|&c| c != grid.tower && matches!(grid.cell(c), Cell::AtriumFloor | Cell::Floor { height: 0.0 }))
        .filter_map(|c| {
            let p = grid.cell_center(c);
            let d = Vec2::new(p.x - tower.x, p.z - tower.z);
            (d.length() > 3.5 && d.length() <= r - 0.5).then_some((c, d.y.atan2(d.x)))
        })
        .collect();
    cells.sort_by(|a, b| a.1.total_cmp(&b.1));
    // Interleave halves: angle-sorted ring -> opposite sides alternate.
    let half = cells.len().div_ceil(2);
    let (a, b) = cells.split_at(half);
    a.iter().zip(b.iter().map(Some).chain(std::iter::repeat(None))).flat_map(|(x, y)| std::iter::once(x.0).chain(y.map(|y| y.0))).collect()
}

impl ExtractionState {
    pub fn start_upload(floor: u32) -> Self {
        Self::start_upload_scaled(floor, 1.0)
    }

    /// Start the uplink with its duration scaled (Overtime Uplink contract).
    pub fn start_upload_scaled(floor: u32, duration_mult: f32) -> Self {
        ExtractionState::Uploading {
            progress: 0.0,
            duration: upload_duration(floor) * duration_mult.max(0.05),
            in_ring: true,
            jammed: false,
            wave_timer: 2.5,
            baron_spawned: false,
        }
    }

    /// Upload progress 0..1 (`None` unless uploading).
    pub fn progress(&self) -> Option<f32> {
        match self {
            ExtractionState::Uploading { progress, .. } => Some(*progress),
            _ => None,
        }
    }

    /// Advance the uplink. Returns true on the frame it completes (now `Extracting`).
    pub fn advance_upload(&mut self, dt: f32, ring: bool, jam: bool) -> bool {
        if let ExtractionState::Uploading { progress, duration, in_ring, jammed, .. } = self {
            *in_ring = ring;
            *jammed = jam;
            *progress = (*progress + dt / *duration * upload_rate(ring, jam)).min(1.0);
            if *progress >= 1.0 {
                *self = ExtractionState::Extracting { timer: EXTRACT_SECS };
                return true;
            }
        }
        false
    }
}

/// Pulsing floor ring marking `TOWER_RING_RADIUS`.
#[derive(Component)]
pub struct SignalRingFx {
    pub material: Handle<StandardMaterial>,
}

#[derive(Component)]
pub struct AlarmLight {
    pub phase: f32,
}

/// Cells the Demon Rush may spawn from (computed at uplink start).
#[derive(Resource, Debug, Default, Clone)]
pub struct RushSpawns(pub Vec<IVec2>);

/// Horizontal radius of the (spherical) signal ring at a floor height.
pub fn ring_radius_at(floor_y: f32, tower_floor_y: f32) -> f32 {
    let dy = (floor_y + PLAYER_EYE_HEIGHT) - (tower_floor_y + 1.5);
    (TOWER_RING_RADIUS * TOWER_RING_RADIUS - dy * dy).max(1.0).sqrt()
}

fn reset_extraction(
    mut commands: Commands,
    grid: Res<LevelGrid>,
    mut state: ResMut<ExtractionState>,
    mut rush: ResMut<RushSpawns>,
    mut clear: ResMut<ClearColor>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    *state = ExtractionState::default();
    rush.0.clear();
    clear.0 = SKY_COLOR;
    let tower = grid.tower_world();
    // Rings on the pedestal and on the atrium floor around it.
    for floor_y in [tower.y, 0.0] {
        let r = ring_radius_at(floor_y, tower.y);
        let mat = materials.add(additive_glow(Color::srgb(0.2, 1.0, 0.85), 0.7));
        commands.spawn((
            LevelEntity,
            SignalRingFx { material: mat.clone() },
            Mesh3d(meshes.add(Torus::new(r - 0.12, r))),
            MeshMaterial3d(mat),
            Transform::from_translation(tower.with_y(floor_y + 0.06)),
        ));
    }
}

/// Drop a glowing supply pickup on the next ring-perimeter cell.
fn drop_supply(
    commands: &mut Commands,
    grid: &LevelGrid,
    pickups: Option<&PickupAssets>,
    fx: &FxAssets,
    materials: &mut Assets<StandardMaterial>,
    kind: PickupKind,
    idx: &mut usize,
) {
    let cells = ring_supply_cells(grid);
    let Some(a) = pickups.filter(|_| !cells.is_empty()) else { return };
    let c = cells[*idx % cells.len()];
    *idx += 1;
    let pos = grid.cell_center(c);
    spawn_pickup(commands, a, kind, pos, true);
    spawn_beam(commands, fx, materials, pos, 6.0, 0.35, 1.6, Color::srgb(0.3, 1.0, 0.5), false);
}

fn rush_cells(grid: &LevelGrid) -> Vec<IVec2> {
    let d = grid.walk_distances(grid.tower);
    (0..grid.depth)
        .flat_map(|z| (0..grid.width).map(move |x| IVec2::new(x, z)))
        .filter(|&c| matches!(grid.cell(c), Cell::Floor { .. } | Cell::AtriumFloor))
        .filter(|&c| d[(c.y * grid.width + c.x) as usize].is_some_and(|v| (4..=12).contains(&v)))
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_extraction(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    grid: Res<LevelGrid>,
    signal: Res<SignalState>,
    mods: Res<PlayerModifiers>,
    (mut run, mut next): (ResMut<RunStats>, ResMut<NextState<AppState>>),
    mut state: ResMut<ExtractionState>,
    mut rush: ResMut<RushSpawns>,
    (ea, fx): (Res<EnemyAssets>, Res<FxAssets>),
    mut materials: ResMut<Assets<StandardMaterial>>,
    (mut sfx, mut feed): (ResMut<SfxQueue>, ResMut<HudFeed>),
    mut players: Query<(&Transform, &mut Player)>,
    mut enemies: Query<(&mut Enemy, &GlobalTransform)>,
    projectiles: Query<Entity, With<EnemyProjectile>>,
    (mut bot, pickups, mut beacon_timer, mut supply_idx, mut second_wind): (Option<ResMut<BotControl>>, Option<Res<PickupAssets>>, Local<f32>, Local<usize>, Local<bool>),
) {
    let dt = time.delta_secs();
    let Ok((ptf, mut player)) = players.single_mut() else { return };
    let pickups = pickups.as_deref();
    let tower = grid.tower_world();
    let ppos = ptf.translation;
    let mut rng = rand::rng();

    match &mut *state {
        ExtractionState::Searching { hold } => {
            let flat = Vec2::new(ppos.x - tower.x, ppos.z - tower.z).length();
            let by_mast = flat <= UPLINK_HOLD_RADIUS && (ppos.y - tower.y).abs() < 2.0;
            *hold = if by_mast { *hold + dt } else { 0.0 };
            let pressed = (keys.just_pressed(KeyCode::KeyE) || take_flag(&mut bot, |b| &mut b.interact)) && signal.near_tower;
            if pressed || *hold >= UPLINK_HOLD_SECS {
                *state = ExtractionState::start_upload_scaled(run.floor, mods.upload_duration_mult);
                rush.0 = rush_cells(&grid);
                // Priority Jamming: a Jammer Fiend crashes the uplink immediately.
                if mods.priority_jamming {
                    let far: Vec<IVec2> = rush.0.iter().copied().filter(|c| grid.cell_center(*c).distance(ppos) > 11.0).collect();
                    let pool = if far.is_empty() { &rush.0 } else { &far };
                    if let Some(&c) = pool.choose(&mut rng) {
                        let pos = spawn_pos_for(EnemyKind::JammerFiend, &grid, c);
                        spawn_enemy(&mut commands, &ea, &mut materials, EnemyKind::JammerFiend, pos, run.floor, true);
                        spawn_burst(&mut commands, &fx, pos + Vec3::Y, BurstColor::Green, 12, 5.0, 0.15);
                        feed.toast("PRIORITY JAMMING: JAMMER FIEND INBOUND", Color::srgb(0.4, 1.0, 0.25));
                    }
                }
                sfx.play(SfxEvent::UplinkStart);
                feed.alert("SOS UPLINK ONLINE - HOLD THE SIGNAL RING", Color::srgb(0.3, 1.0, 0.85), 3.0);
                // Tactical supply drops on the ring perimeter: kiting the tower pays.
                *supply_idx = 0;
                *second_wind = false;
                *beacon_timer = 0.0;
                drop_supply(&mut commands, &grid, pickups, &fx, &mut materials, PickupKind::Health, &mut supply_idx);
                drop_supply(&mut commands, &grid, pickups, &fx, &mut materials, if run.floor <= 1 { PickupKind::Armor } else { PickupKind::Shells }, &mut supply_idx);
                for (mut e, _) in &mut enemies {
                    e.alerted = true;
                }
                // Alarm beacons around the atrium.
                if let Some(atrium) = grid.rooms.first() {
                    let lo = atrium.min;
                    let hi = atrium.min + atrium.size - IVec2::ONE;
                    for (i, c) in [lo, IVec2::new(hi.x, lo.y), hi, IVec2::new(lo.x, hi.y)].into_iter().enumerate() {
                        let p = grid.cell_center(c) + Vec3::Y * 4.5;
                        commands.spawn((
                            LevelEntity,
                            AlarmLight { phase: i as f32 * 1.3 },
                            PointLight { color: Color::srgb(1.0, 0.1, 0.05), intensity: 0.0, range: 22.0, shadow_maps_enabled: false, ..default() },
                            Transform::from_translation(p),
                        ));
                    }
                }
            }
        }
        ExtractionState::Uploading { in_ring, wave_timer, baron_spawned, progress, jammed, .. } => {
            let was_jammed = *jammed;
            let ring = signal.near_tower;
            let fiends: Vec<Vec3> = enemies
                .iter()
                .filter(|(e, _)| e.kind == EnemyKind::JammerFiend && e.is_alive())
                .map(|(_, g)| g.translation())
                .collect();
            let jam = fiend_jams_uplink(&fiends, tower);
            if jam && !was_jammed {
                feed.alert("UPLINK JAMMED - KILL THE JAMMER FIEND!", Color::srgb(1.0, 0.2, 0.15), 2.5);
                sfx.play(SfxEvent::Denied);
                *beacon_timer = 0.0;
            }
            if jam {
                *beacon_timer -= dt;
                if *beacon_timer <= 0.0 {
                    *beacon_timer = FIEND_BEACON_SECS;
                    for f in fiends.iter().filter(|f| Vec2::new(f.x - tower.x, f.z - tower.z).length() <= TOWER_RING_RADIUS) {
                        let base = f.with_y(f.y.min(tower.y.max(0.0)));
                        spawn_beam(&mut commands, &fx, &mut materials, base, 40.0, 0.6, FIEND_BEACON_SECS + 0.15, Color::srgb(0.35, 1.0, 0.25), true);
                        spawn_beam(&mut commands, &fx, &mut materials, base, 40.0, 0.25, FIEND_BEACON_SECS + 0.15, Color::srgb(1.0, 0.12, 0.2), true);
                    }
                }
            }
            // Firewall Daemon: only while the SOS actually advances (in ring, not
            // jammed) - no farming armor off a deliberately spared Jammer Fiend.
            if firewall_regen_active(ring, jam) && mods.armor_regen_while_uploading > 0.0 {
                player.armor = (player.armor + mods.armor_regen_while_uploading * dt).min(player.max_armor);
            }
            let _ = in_ring;
            // Demon Rush waves.
            *wave_timer -= dt;
            let alive = enemies.iter().filter(|(e, _)| e.is_alive()).count();
            let want_baron = *progress >= 0.5 && !*baron_spawned;
            if want_baron {
                *baron_spawned = true;
            }
            let spawn_wave = *wave_timer <= 0.0;
            if spawn_wave {
                *wave_timer = rng.random_range(scaled_wave_interval(run.floor, mods.wave_interval_mult));
            }
            let floor = run.floor;
            let p_eye = ppos + Vec3::Y * PLAYER_EYE_HEIGHT;
            let pick_cell = |rng: &mut rand::rngs::ThreadRng| -> Option<IVec2> {
                // Prefer cells away from the player and out of view.
                for _ in 0..12 {
                    let c = *rush.0.choose(rng)?;
                    let p = grid.cell_center(c);
                    if p.distance(ppos) > 11.0 && !has_line_of_sight(&grid, p + Vec3::Y * 1.5, p_eye) {
                        return Some(c);
                    }
                }
                rush.0.choose(rng).copied()
            };
            if spawn_wave && alive < rush_cap(floor) {
                let living_fiends = fiends.len();
                let (fiend_cap, fiend_chance) = rush_fiend_odds(mods.priority_jamming);
                let n = (2 + floor as usize / 2).min(rush_cap(floor) - alive);
                for i in 0..n {
                    let roll: f32 = rng.random();
                    let kind = if i == 0 && living_fiends < fiend_cap && roll < fiend_chance {
                        EnemyKind::JammerFiend
                    } else if roll < 0.62 {
                        EnemyKind::Imp
                    } else {
                        EnemyKind::Cacodemon
                    };
                    if let Some(c) = pick_cell(&mut rng) {
                        let pos = spawn_pos_for(kind, &grid, c);
                        spawn_enemy(&mut commands, &ea, &mut materials, kind, pos, floor, true);
                        spawn_burst(&mut commands, &fx, pos + Vec3::Y, BurstColor::Fire, 8, 4.0, 0.15);
                    }
                }
            }
            // Floor 1 second wind: medkit + armor on the ring edge for the final push.
            if run.floor <= 1 && *progress >= 0.75 && !*second_wind {
                *second_wind = true;
                drop_supply(&mut commands, &grid, pickups, &fx, &mut materials, PickupKind::Health, &mut supply_idx);
                drop_supply(&mut commands, &grid, pickups, &fx, &mut materials, PickupKind::Armor, &mut supply_idx);
                feed.toast("SUPPLY DROP ON THE RING", Color::srgb(0.3, 1.0, 0.5));
            }
            if want_baron && let Some(c) = pick_cell(&mut rng) {
                let pos = spawn_pos_for(EnemyKind::HellBaron, &grid, c);
                spawn_enemy(&mut commands, &ea, &mut materials, EnemyKind::HellBaron, pos, floor, true);
                spawn_burst(&mut commands, &fx, pos + Vec3::Y * 1.5, BurstColor::Gold, 24, 6.0, 0.2);
                sfx.play(SfxEvent::BaronAlert);
                feed.alert("WARNING: HELL BARON INBOUND!", Color::srgb(1.0, 0.55, 0.1), 3.5);
                // Emergency medkit on the ring edge.
                drop_supply(&mut commands, &grid, pickups, &fx, &mut materials, PickupKind::Health, &mut supply_idx);
            }
            if state.advance_upload(dt, ring, jam) {
                // Orbital extraction: beam + vaporize.
                sfx.play(SfxEvent::Extract);
                feed.alert("ORBITAL EXTRACTION LOCKED", Color::srgb(0.3, 1.0, 1.0), 2.0);
                spawn_beam(&mut commands, &fx, &mut materials, tower, 90.0, 2.8, EXTRACT_SECS + 0.6, Color::srgb(0.4, 1.0, 1.0), false);
                spawn_beam(&mut commands, &fx, &mut materials, tower, 90.0, 1.0, EXTRACT_SECS + 0.6, Color::srgb(0.9, 1.0, 1.0), false);
                spawn_flash_light(&mut commands, tower + Vec3::Y * 3.0, Color::srgb(0.4, 1.0, 1.0), 40_000_000.0, 60.0, EXTRACT_SECS);
                player.invuln_timer = player.invuln_timer.max(EXTRACT_SECS + 1.0);
                for (mut e, _) in &mut enemies {
                    if e.is_alive() {
                        e.hp = 0.0;
                        e.vaporized = true;
                    }
                }
                for p in &projectiles {
                    commands.entity(p).try_despawn();
                }
            }
        }
        ExtractionState::Extracting { timer } => {
            *timer -= dt;
            if *timer <= 0.0 {
                *state = ExtractionState::Complete;
                run.sos_completed += 1;
                complete_floor(&mut run, &mut next);
            }
        }
        ExtractionState::Complete => {}
    }
}

#[allow(clippy::type_complexity)]
fn alarm_atmosphere(
    time: Res<Time>,
    state: Res<ExtractionState>,
    mut clear: ResMut<ClearColor>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    rings: Query<(&SignalRingFx, &mut Transform)>,
    mut beacons: Query<&mut PointLight, (With<TowerBeacon>, Without<AlarmLight>)>,
    mut alarms: Query<(&AlarmLight, &mut PointLight), Without<TowerBeacon>>,
) {
    let t = time.elapsed_secs();
    let uploading = matches!(*state, ExtractionState::Uploading { .. });
    let jammed = matches!(*state, ExtractionState::Uploading { jammed: true, .. });
    let pulse = 0.5 + 0.5 * (t * 6.0).sin();
    clear.0 = if uploading { SKY_COLOR.mix(&Color::srgb(0.6, 0.02, 0.02), pulse * 0.7) } else { SKY_COLOR };
    for mut l in &mut beacons {
        if uploading {
            l.color = if (t * 2.0) as i32 % 2 == 0 { Color::srgb(1.0, 0.1, 0.05) } else { Color::srgb(1.0, 0.6, 0.1) };
            l.intensity = 3_000_000.0 + 4_000_000.0 * pulse;
        } else {
            l.color = Color::srgb(0.3, 1.0, 0.75);
        }
    }
    for (a, mut l) in &mut alarms {
        let k = (0.5 + 0.5 * (t * 7.0 + a.phase).sin()).powi(2);
        l.intensity = if uploading { 2_500_000.0 * k } else { 0.0 };
    }
    for (ring, mut tf) in rings {
        let s = 1.0 + 0.03 * (t * 3.0).sin();
        tf.scale = Vec3::new(s, 1.0 + 2.0 * pulse, s);
        if let Some(mut m) = materials.get_mut(&ring.material) {
            let c = match *state {
                _ if jammed => LinearRgba::rgb(4.0, 0.2, 0.2),
                ExtractionState::Uploading { in_ring: false, .. } => LinearRgba::rgb(4.0, 2.4, 0.2),
                ExtractionState::Uploading { .. } => LinearRgba::rgb(0.4, 5.0, 4.0),
                _ => LinearRgba::rgb(0.3, 2.5, 2.0),
            };
            m.emissive = c * (0.6 + 0.6 * pulse);
        }
    }
}

pub struct ExtractionPlugin;

impl Plugin for ExtractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExtractionState>()
            .init_resource::<RushSpawns>()
            .add_systems(Update, reset_extraction.run_if(resource_exists_and_changed::<LevelGrid>).in_set(GameSet::World))
            .add_systems(
                Update,
                (update_extraction, alarm_atmosphere).chain().in_set(GameSet::Combat).after(CombatDamage).run_if(resource_exists::<LevelGrid>),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_rates() {
        assert_eq!(upload_duration(1), 24.0);
        assert_eq!(upload_duration(5), 44.0);
        assert_eq!(upload_rate(true, false), 1.0);
        assert_eq!(upload_rate(false, false), OUTSIDE_RING_RATE);
        assert_eq!(upload_rate(true, true), 0.0);
        assert_eq!(upload_rate(false, true), 0.0);
    }

    #[test]
    fn overtime_uplink_and_priority_jamming_scale_the_sos() {
        let s = ExtractionState::start_upload_scaled(1, 1.35);
        assert!(matches!(s, ExtractionState::Uploading { duration, .. } if (duration - 32.4).abs() < 1e-4));
        assert_eq!(ExtractionState::start_upload_scaled(3, 1.0), ExtractionState::start_upload(3));
        let fast = scaled_wave_interval(1, 0.8);
        assert!((fast.start - 4.0).abs() < 1e-5 && (fast.end - 5.2).abs() < 1e-5);
        assert_eq!(scaled_wave_interval(2, 1.0), wave_interval(2));
        assert_eq!(rush_fiend_odds(false), (2, 0.3));
        assert_eq!(rush_fiend_odds(true), (3, 0.45));
        // Overtime: the same in-ring time covers less of the uplink.
        let mut a = ExtractionState::start_upload(1);
        let mut b = ExtractionState::start_upload_scaled(1, 1.35);
        a.advance_upload(12.0, true, false);
        b.advance_upload(12.0, true, false);
        assert!(b.progress().unwrap() < a.progress().unwrap());
    }

    #[test]
    fn firewall_regen_only_while_sos_advances_in_ring() {
        assert!(firewall_regen_active(true, false));
        assert!(!firewall_regen_active(true, true), "a living Jammer Fiend is not an armor farm");
        assert!(!firewall_regen_active(false, false));
        assert!(!firewall_regen_active(false, true));
    }

    #[test]
    fn upload_progresses_to_extraction() {
        let mut s = ExtractionState::start_upload(1);
        assert_eq!(s.progress(), Some(0.0));
        // Half the duration inside the ring = 50%.
        assert!(!s.advance_upload(upload_duration(1) * 0.5, true, false));
        assert!((s.progress().unwrap() - 0.5).abs() < 1e-5);
        // Jammed: no progress.
        assert!(!s.advance_upload(10.0, true, true));
        assert!((s.progress().unwrap() - 0.5).abs() < 1e-5);
        assert!(matches!(s, ExtractionState::Uploading { jammed: true, .. }));
        // Outside the ring: 20%.
        s.advance_upload(10.0, false, false);
        assert!((s.progress().unwrap() - (0.5 + 2.0 / upload_duration(1))).abs() < 1e-5);
        assert!(matches!(s, ExtractionState::Uploading { in_ring: false, .. }));
        // Finish.
        assert!(s.advance_upload(100.0, true, false));
        assert_eq!(s, ExtractionState::Extracting { timer: EXTRACT_SECS });
        assert_eq!(s.progress(), None);
        assert!(!s.advance_upload(1.0, true, false));
    }

    #[test]
    fn fiend_in_ring_jams() {
        let tower = Vec3::new(50.0, 2.4, 50.0);
        assert!(fiend_jams_uplink(&[Vec3::new(55.0, 0.0, 50.0)], tower));
        assert!(!fiend_jams_uplink(&[Vec3::new(70.0, 0.0, 50.0)], tower));
        assert!(!fiend_jams_uplink(&[], tower));
    }

    #[test]
    fn ring_radius_shrinks_off_level() {
        let on = ring_radius_at(2.4, 2.4);
        let below = ring_radius_at(0.0, 2.4);
        assert!(on <= TOWER_RING_RADIUS && on > 8.0);
        assert!(below < on);
    }

    #[test]
    fn rush_cells_exist_around_tower() {
        let g = crate::level::generate_level(1, 5);
        let cells = rush_cells(&g);
        assert!(cells.len() > 10);
        assert!(rush_cap(5) > rush_cap(1));
        assert_eq!(rush_cap(1), 11);
    }

    #[test]
    fn floor1_wave_cadence_is_gentler() {
        assert_eq!(wave_interval(1), 5.0..6.5);
        let deep = wave_interval(5);
        assert!(deep.start < 5.0 && deep.end < 6.5 && deep.start < deep.end);
    }

    #[test]
    fn supply_cells_ring_the_tower() {
        for floor in 1..=5 {
            let g = crate::level::generate_level(floor, 11 + floor as u64);
            let cells = ring_supply_cells(&g);
            assert!(cells.len() >= 3, "floor {floor}: {} supply cells", cells.len());
            let t = g.tower_world();
            for c in &cells {
                let p = g.cell_center(*c);
                assert!(Vec2::new(p.x - t.x, p.z - t.z).length() <= TOWER_RING_RADIUS && g.is_reachable(g.spawn, *c));
            }
            // First two drops on different sides of the tower.
            let (a, b) = (g.cell_center(cells[0]) - t, g.cell_center(cells[1]) - t);
            assert!(a.with_y(0.0).normalize().dot(b.with_y(0.0).normalize()) < 0.2);
        }
    }
}
