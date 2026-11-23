//! Real-time cell-signal field: distance to the Extraction Tower, wall
//! attenuation, Demon Jammer suppression and upgrade modifiers.

use bevy::prelude::*;

use crate::enemies::{fiend_jammer_fields, Enemy};
use crate::level::{DemonJammer, LevelGrid, SignalTower};
use crate::physics::count_wall_occlusions;
use crate::player::{Player, PLAYER_EYE_HEIGHT};
use crate::state::{GameSet, PlayerModifiers};

pub const MAX_BARS: f32 = 5.0;
/// Inside this radius of the tower the signal is a clean 5 bars ("Signal Ring").
pub const TOWER_RING_RADIUS: f32 = 9.0;
/// Distance (beyond the ring) at which the unobstructed signal halves.
pub const FALLOFF_HALF_DIST: f32 = 24.0;
pub const FALLOFF_EXPONENT: f32 = 1.2;
/// Multiplicative signal loss per wall cell crossed (before penetration upgrades).
pub const WALL_ATTENUATION: f32 = 0.86;
pub const DBM_MIN: i32 = -118;
pub const DBM_MAX: i32 = -55;

/// A jammer's suppression field as seen by the solver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JammerField {
    pub pos: Vec3,
    pub radius: f32,
    pub strength: f32,
}

/// Result of a single signal evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalReading {
    pub raw_strength: f32,
    pub bars: u8,
    pub dbm: i32,
    pub in_dead_zone: bool,
    pub near_tower: bool,
}

/// Live signal state of the player, refreshed every frame while `Playing`.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct SignalState {
    pub raw_strength: f32,
    pub bars: u8,
    pub dbm: i32,
    pub in_dead_zone: bool,
    pub near_tower: bool,
    /// Signed yaw (radians, -PI..PI) from the player's facing to the tower:
    /// negative = tower is to the left, positive = to the right.
    pub tower_dir_angle: f32,
    pub tower_distance: f32,
    pub nearest_jammer_dist: Option<f32>,
    pub wall_occlusions: u32,
}

impl Default for SignalState {
    fn default() -> Self {
        SignalState {
            raw_strength: 0.0,
            bars: 0,
            dbm: DBM_MIN,
            in_dead_zone: true,
            near_tower: false,
            tower_dir_angle: 0.0,
            tower_distance: f32::INFINITY,
            nearest_jammer_dist: None,
            wall_occlusions: 0,
        }
    }
}

/// Unobstructed tower signal (bars) at a given distance.
pub fn distance_strength(dist: f32) -> f32 {
    let beyond = (dist - TOWER_RING_RADIUS).max(0.0);
    MAX_BARS / (1.0 + (beyond / FALLOFF_HALF_DIST).powf(FALLOFF_EXPONENT))
}

/// Bars removed by jammers at `pos` (sum of all fields, linear-ish falloff).
pub fn jammer_suppression(pos: Vec3, jammers: &[JammerField]) -> f32 {
    jammers
        .iter()
        .map(|j| {
            let d = pos.distance(j.pos);
            if d >= j.radius {
                0.0
            } else {
                j.strength * (1.0 - d / j.radius).powf(0.6)
            }
        })
        .sum()
}

pub fn bars_from_strength(raw: f32) -> u8 {
    raw.clamp(0.0, MAX_BARS).floor() as u8
}

pub fn dbm_from_strength(raw: f32) -> i32 {
    let t = (raw / MAX_BARS).clamp(0.0, 1.0);
    (DBM_MIN as f32 + (DBM_MAX - DBM_MIN) as f32 * t).round() as i32
}

/// Pure signal solver, reusable for enemies (Dead-Zone enrage) and tests.
pub fn compute_signal_at(
    pos: Vec3,
    tower_pos: Vec3,
    jammers: &[JammerField],
    wall_occlusions: u32,
    modifiers: &PlayerModifiers,
) -> SignalReading {
    let dist = pos.distance(tower_pos);
    let near_tower = dist <= TOWER_RING_RADIUS;
    let mut raw = distance_strength(dist);
    if !near_tower {
        let pen = modifiers.wall_penetration.clamp(0.0, 1.0);
        let effective_walls = wall_occlusions as f32 * (1.0 - pen);
        raw *= WALL_ATTENUATION.powf(effective_walls);
    }
    raw -= jammer_suppression(pos, jammers);
    raw = raw.max(modifiers.min_signal_floor).clamp(0.0, MAX_BARS);
    let bars = bars_from_strength(raw);
    SignalReading { raw_strength: raw, bars, dbm: dbm_from_strength(raw), in_dead_zone: bars <= 1, near_tower }
}

/// 5G Overclock fire-rate multiplier (only at >= 4 bars).
pub fn overclock_factor(bars: u8, modifiers: &PlayerModifiers) -> f32 {
    if bars >= 4 && modifiers.overclock_bonus > 0.0 { 1.0 + modifiers.overclock_bonus } else { 1.0 }
}

/// 5G Overclock move-speed multiplier (only at >= 4 bars).
pub fn overclock_move_factor(bars: u8, modifiers: &PlayerModifiers) -> f32 {
    if bars >= 4 && modifiers.overclock_move_bonus > 0.0 { 1.0 + modifiers.overclock_move_bonus } else { 1.0 }
}

/// Signed angle from `forward` to `to_target` around +Y (positive = right).
pub fn relative_yaw(forward: Vec3, to_target: Vec3) -> f32 {
    let f = Vec2::new(forward.x, forward.z).normalize_or_zero();
    let t = Vec2::new(to_target.x, to_target.z).normalize_or_zero();
    if f == Vec2::ZERO || t == Vec2::ZERO {
        return 0.0;
    }
    // In XZ with y-down-the-screen convention (+Z toward viewer), a positive
    // perp_dot means clockwise when seen from above = to the right.
    f.perp_dot(t).atan2(f.dot(t))
}

/// Collect active jammer fields from the ECS.
pub fn active_jammer_fields<'a>(jammers: impl Iterator<Item = (&'a DemonJammer, &'a GlobalTransform)>) -> Vec<JammerField> {
    jammers
        .filter(|(j, _)| j.is_active())
        .map(|(j, t)| JammerField { pos: t.translation() + Vec3::Y * 2.0, radius: j.suppression_radius, strength: j.suppression_strength })
        .collect()
}

pub struct SignalPlugin;

impl Plugin for SignalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SignalState>().add_systems(Update, update_signal.in_set(GameSet::Signal));
    }
}

fn update_signal(
    grid: Option<Res<LevelGrid>>,
    modifiers: Res<PlayerModifiers>,
    mut state: ResMut<SignalState>,
    player: Query<&Transform, With<Player>>,
    tower: Query<&SignalTower>,
    jammers: Query<(&DemonJammer, &GlobalTransform)>,
    fiends: Query<(&Enemy, &GlobalTransform)>,
) {
    let (Some(grid), Ok(ptf), Ok(tower)) = (grid, player.single(), tower.single()) else {
        return;
    };
    let head = ptf.translation + Vec3::Y * PLAYER_EYE_HEIGHT;
    let mut fields = active_jammer_fields(jammers.iter());
    // Jammer Fiends carry a mobile Dead Zone.
    fields.extend(fiend_jammer_fields(fiends.iter()));
    let walls = count_wall_occlusions(&grid, head, tower.beacon);
    let r = compute_signal_at(head, grid.tower_world() + Vec3::Y * 1.5, &fields, walls, &modifiers);
    let forward = ptf.rotation * Vec3::NEG_Z;
    *state = SignalState {
        raw_strength: r.raw_strength,
        bars: r.bars,
        dbm: r.dbm,
        in_dead_zone: r.in_dead_zone,
        near_tower: r.near_tower,
        tower_dir_angle: relative_yaw(forward, tower.beacon - head),
        tower_distance: head.distance(grid.tower_world()),
        nearest_jammer_dist: fields.iter().map(|f| f.pos.distance(head)).reduce(f32::min),
        wall_occlusions: walls,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> PlayerModifiers {
        PlayerModifiers::default()
    }

    #[test]
    fn five_bars_next_to_tower() {
        let tower = Vec3::new(50.0, 1.5, 50.0);
        let r = compute_signal_at(tower + Vec3::new(3.0, 0.0, 0.0), tower, &[], 0, &m());
        assert_eq!(r.bars, 5);
        assert_eq!(r.dbm, DBM_MAX);
        assert!(r.near_tower);
        assert!(!r.in_dead_zone);
    }

    #[test]
    fn signal_decreases_with_distance() {
        let tower = Vec3::ZERO;
        let mut last = f32::INFINITY;
        for d in [5.0, 15.0, 30.0, 60.0, 100.0] {
            let r = compute_signal_at(Vec3::new(d, 0.0, 0.0), tower, &[], 0, &m());
            assert!(r.raw_strength <= last);
            last = r.raw_strength;
        }
        // Mid range unobstructed is "moderate", far away is weak.
        let mid = compute_signal_at(Vec3::new(30.0, 0.0, 0.0), tower, &[], 0, &m());
        assert!((2..=4).contains(&mid.bars), "mid bars {}", mid.bars);
        let far = compute_signal_at(Vec3::new(100.0, 0.0, 0.0), tower, &[], 0, &m());
        assert!(far.bars <= 1);
    }

    #[test]
    fn walls_attenuate_and_penetration_helps() {
        let tower = Vec3::ZERO;
        let p = Vec3::new(30.0, 0.0, 0.0);
        let open = compute_signal_at(p, tower, &[], 0, &m());
        let walled = compute_signal_at(p, tower, &[], 6, &m());
        assert!(walled.raw_strength < open.raw_strength * 0.5);
        assert!(walled.dbm < open.dbm);
        let sub_ghz = PlayerModifiers { wall_penetration: 0.45, ..m() };
        let pen = compute_signal_at(p, tower, &[], 6, &sub_ghz);
        assert!(pen.raw_strength > walled.raw_strength);
        assert!(pen.raw_strength < open.raw_strength);
        // Walls do not matter inside the tower ring.
        let ring = compute_signal_at(Vec3::new(4.0, 0.0, 0.0), tower, &[], 3, &m());
        assert_eq!(ring.bars, 5);
    }

    #[test]
    fn jammer_creates_dead_zone() {
        let tower = Vec3::ZERO;
        let p = Vec3::new(20.0, 0.0, 0.0);
        let clean = compute_signal_at(p, tower, &[], 0, &m());
        assert!(clean.bars >= 2);
        let jam = JammerField { pos: p + Vec3::new(1.0, 0.0, 0.0), radius: 15.0, strength: 3.5 };
        let r = compute_signal_at(p, tower, &[jam], 0, &m());
        assert!(r.in_dead_zone, "bars {}", r.bars);
        assert!(r.raw_strength < clean.raw_strength);
        // Out of the jammer radius: no effect.
        let far_jam = JammerField { pos: p + Vec3::new(40.0, 0.0, 0.0), ..jam };
        assert_eq!(compute_signal_at(p, tower, &[far_jam], 0, &m()), clean);
    }

    #[test]
    fn antenna_floor_modifier() {
        let tower = Vec3::ZERO;
        let p = Vec3::new(200.0, 0.0, 0.0);
        let jam = JammerField { pos: p, radius: 10.0, strength: 5.0 };
        let base = compute_signal_at(p, tower, &[jam], 12, &m());
        assert_eq!(base.bars, 0);
        assert_eq!(base.dbm, DBM_MIN);
        let antenna = PlayerModifiers { min_signal_floor: 1.25, ..m() };
        let r = compute_signal_at(p, tower, &[jam], 12, &antenna);
        assert_eq!(r.bars, 1);
        assert!((r.raw_strength - 1.25).abs() < 1e-6);
    }

    #[test]
    fn dbm_and_bars_ranges() {
        assert_eq!(dbm_from_strength(0.0), DBM_MIN);
        assert_eq!(dbm_from_strength(5.0), DBM_MAX);
        assert_eq!(dbm_from_strength(99.0), DBM_MAX);
        assert_eq!(bars_from_strength(4.99), 4);
        assert_eq!(bars_from_strength(-3.0), 0);
        assert_eq!(bars_from_strength(7.0), 5);
    }

    #[test]
    fn overclock_needs_four_bars() {
        let oc = PlayerModifiers { overclock_bonus: 0.35, overclock_move_bonus: 0.25, ..m() };
        assert_eq!(overclock_factor(5, &m()), 1.0);
        assert_eq!(overclock_factor(3, &oc), 1.0);
        assert!((overclock_factor(4, &oc) - 1.35).abs() < 1e-6);
        assert_eq!(overclock_move_factor(3, &oc), 1.0);
        assert!((overclock_move_factor(5, &oc) - 1.25).abs() < 1e-6);
    }

    #[test]
    fn relative_yaw_sign() {
        let fwd = Vec3::NEG_Z;
        assert!(relative_yaw(fwd, Vec3::NEG_Z).abs() < 1e-5);
        // Facing -Z, +X is to the right.
        assert!((relative_yaw(fwd, Vec3::X) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((relative_yaw(fwd, Vec3::NEG_X) + std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((relative_yaw(fwd, Vec3::Z).abs() - std::f32::consts::PI).abs() < 1e-5);
    }
}
