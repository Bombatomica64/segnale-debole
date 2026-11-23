//! Deterministic, allocation-free FPS collision against the [`LevelGrid`].
//!
//! The player is a vertical cylinder (`radius`, `height`) whose `pos` is the
//! center of its feet. A cell blocks horizontal movement when it is a wall or
//! its floor (at the closest point to the player) is more than `step_height`
//! above the feet. Pillars are circles.

use bevy::prelude::*;

use crate::level::{Cell, LevelGrid, CELL_SIZE};

pub const GRAVITY: f32 = 22.0;
pub const PLAYER_RADIUS: f32 = 0.4;
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_STEP_HEIGHT: f32 = crate::level::STEP_HEIGHT;
pub const JUMP_VELOCITY: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveResult {
    pub pos: Vec3,
    pub vel: Vec3,
    pub grounded: bool,
    /// Standing in a hazard pit (lava).
    pub on_hazard: bool,
    pub hit_wall: bool,
    pub hit_ceiling: bool,
}

/// Floor height at a world XZ position (`None` inside walls / out of bounds).
pub fn sample_floor_height(grid: &LevelGrid, x: f32, z: f32) -> Option<f32> {
    let c = IVec2::new((x / CELL_SIZE).floor() as i32, (z / CELL_SIZE).floor() as i32);
    let frac = Vec2::new(x / CELL_SIZE - c.x as f32, z / CELL_SIZE - c.y as f32);
    grid.cell(c).floor_height_at_frac(frac)
}

fn cell_of(x: f32, z: f32) -> IVec2 {
    IVec2::new((x / CELL_SIZE).floor() as i32, (z / CELL_SIZE).floor() as i32)
}

/// Highest floor under the player's footprint that is standable from `feet`
/// (i.e. not more than `step_height` above it).
fn ground_under(grid: &LevelGrid, p: Vec3, radius: f32, step_height: f32) -> Option<f32> {
    let r = radius * 0.7;
    let offsets = [Vec2::ZERO, Vec2::new(r, 0.0), Vec2::new(-r, 0.0), Vec2::new(0.0, r), Vec2::new(0.0, -r)];
    let mut best: Option<f32> = None;
    for o in offsets {
        if let Some(h) = sample_floor_height(grid, p.x + o.x, p.z + o.y)
            && h <= p.y + step_height {
                best = Some(best.map_or(h, |b: f32| b.max(h)));
            }
    }
    best
}

/// Push a circle at `xz` out of every blocking cell and pillar.
/// Returns the corrected position and the accumulated push normal (zero if none).
fn push_out(grid: &LevelGrid, mut xz: Vec2, feet: f32, radius: f32, step_height: f32) -> (Vec2, Vec2) {
    let mut normal_sum = Vec2::ZERO;
    for _ in 0..3 {
        let mut moved = false;
        let min = cell_of(xz.x - radius, xz.y - radius);
        let max = cell_of(xz.x + radius, xz.y + radius);
        for cz in min.y..=max.y {
            for cx in min.x..=max.x {
                let c = IVec2::new(cx, cz);
                let lo = Vec2::new(cx as f32, cz as f32) * CELL_SIZE;
                let hi = lo + Vec2::splat(CELL_SIZE);
                let closest = xz.clamp(lo, hi);
                let blocking = match grid.cell(c) {
                    Cell::Wall => true,
                    cell => {
                        let frac = (closest - lo) / CELL_SIZE;
                        cell.floor_height_at_frac(frac).is_some_and(|h| h > feet + step_height)
                    }
                };
                if !blocking {
                    continue;
                }
                let d = xz - closest;
                let dist = d.length();
                if dist >= radius {
                    continue;
                }
                let n = if dist > 1e-5 {
                    d / dist
                } else {
                    // Center inside the block: exit through the nearest face.
                    let to_lo = xz - lo;
                    let to_hi = hi - xz;
                    let m = to_lo.x.min(to_lo.y).min(to_hi.x).min(to_hi.y);
                    if m == to_lo.x {
                        Vec2::NEG_X
                    } else if m == to_hi.x {
                        Vec2::X
                    } else if m == to_lo.y {
                        Vec2::NEG_Y
                    } else {
                        Vec2::Y
                    }
                };
                let target = if dist > 1e-5 { closest + n * radius } else { xz + n * (radius + m_face(xz, lo, hi, n)) };
                xz = target;
                normal_sum += n;
                moved = true;
            }
        }
        for (p, pr) in grid.pillars.iter().zip(&grid.pillar_radii) {
            let d = xz - *p;
            let min_d = radius + pr;
            let dist = d.length();
            if dist < min_d {
                let n = if dist > 1e-5 { d / dist } else { Vec2::X };
                xz = *p + n * min_d;
                normal_sum += n;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    (xz, normal_sum.normalize_or_zero())
}

/// Distance from `xz` to the face of `[lo,hi]` in direction `n` (center-inside case).
fn m_face(xz: Vec2, lo: Vec2, hi: Vec2, n: Vec2) -> f32 {
    if n.x < 0.0 {
        xz.x - lo.x
    } else if n.x > 0.0 {
        hi.x - xz.x
    } else if n.y < 0.0 {
        xz.y - lo.y
    } else {
        hi.y - xz.y
    }
}

/// Integrate one physics step: gravity, wall sliding, ramp/step tracking,
/// ceiling bump and hazard detection. Jumping = caller sets `vel.y` while grounded.
pub fn resolve_player_movement(
    grid: &LevelGrid,
    pos: Vec3,
    vel: Vec3,
    dt: f32,
    radius: f32,
    height: f32,
    step_height: f32,
) -> MoveResult {
    let mut vel = vel;
    vel.y -= GRAVITY * dt;
    let horiz_len = Vec2::new(vel.x, vel.z).length() * dt;
    let steps = ((horiz_len / (radius * 0.5)).ceil() as u32).clamp(1, 32);
    let sub_dt = dt / steps as f32;

    let mut p = pos;
    let mut grounded = false;
    let mut hit_wall = false;
    let mut hit_ceiling = false;

    for _ in 0..steps {
        // Horizontal.
        let target = Vec2::new(p.x + vel.x * sub_dt, p.z + vel.z * sub_dt);
        let (xz, n) = push_out(grid, target, p.y, radius, step_height);
        p.x = xz.x;
        p.z = xz.y;
        if n != Vec2::ZERO {
            hit_wall = true;
            let v = Vec2::new(vel.x, vel.z);
            let into = v.dot(n);
            if into < 0.0 {
                let slid = v - n * into;
                vel.x = slid.x;
                vel.z = slid.y;
            }
        }

        // Vertical.
        p.y += vel.y * sub_dt;
        grounded = false;
        if let Some(g) = ground_under(grid, p, radius, step_height) {
            if p.y <= g {
                // Below the surface (stepping up a ramp / landing): snap up.
                p.y = g;
                vel.y = vel.y.max(0.0);
                grounded = vel.y <= 0.0;
            } else if vel.y <= 0.0 && p.y - g <= step_height * 0.5 + (-vel.y * sub_dt) {
                // Small drop while walking (ramp descent / step down): stick to ground.
                p.y = g;
                vel.y = 0.0;
                grounded = true;
            }
        }

        // Ceiling.
        let c = cell_of(p.x, p.z);
        if let Some(ceil) = grid.ceiling_at(c)
            && p.y + height > ceil {
                p.y = ceil - height;
                if vel.y > 0.0 {
                    vel.y = 0.0;
                }
                hit_ceiling = true;
            }
    }

    let on_hazard = grounded && matches!(grid.cell(cell_of(p.x, p.z)), Cell::HazardPit);
    MoveResult { pos: p, vel, grounded, on_hazard, hit_wall, hit_ceiling }
}

// ---------------------------------------------------------------------------
// Raycasting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Wall,
    Floor,
    Ceiling,
    /// Side of a raised floor block / ramp.
    Ledge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    pub distance: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub cell: IVec2,
    pub kind: HitKind,
    /// Number of open cells the ray crossed before hitting (for falloff/tracers).
    pub cells_traversed: u32,
}

/// 2D DDA iterator over the cells crossed by the XZ segment.
/// Calls `f(cell, t_enter, t_exit, entry_normal)` with t in units of `dir` length;
/// stops when `f` returns `true` or `max_t` is reached.
fn dda_xz(origin: Vec3, dir: Vec3, max_t: f32, mut f: impl FnMut(IVec2, f32, f32, Vec3) -> bool) {
    let mut cell = cell_of(origin.x, origin.z);
    let step = IVec2::new(
        if dir.x > 0.0 { 1 } else if dir.x < 0.0 { -1 } else { 0 },
        if dir.z > 0.0 { 1 } else if dir.z < 0.0 { -1 } else { 0 },
    );
    let next_boundary = |c: i32, s: i32| (c + if s > 0 { 1 } else { 0 }) as f32 * CELL_SIZE;
    let mut t_max_x = if step.x != 0 { (next_boundary(cell.x, step.x) - origin.x) / dir.x } else { f32::INFINITY };
    let mut t_max_z = if step.y != 0 { (next_boundary(cell.y, step.y) - origin.z) / dir.z } else { f32::INFINITY };
    let t_delta_x = if step.x != 0 { CELL_SIZE / dir.x.abs() } else { f32::INFINITY };
    let t_delta_z = if step.y != 0 { CELL_SIZE / dir.z.abs() } else { f32::INFINITY };
    let mut t_enter = 0.0;
    let mut normal = Vec3::ZERO;
    for _ in 0..4096 {
        let t_exit = t_max_x.min(t_max_z).min(max_t);
        if f(cell, t_enter, t_exit, normal) || t_exit >= max_t {
            return;
        }
        if t_max_x < t_max_z {
            cell.x += step.x;
            t_enter = t_max_x;
            t_max_x += t_delta_x;
            normal = Vec3::new(-step.x as f32, 0.0, 0.0);
        } else {
            cell.y += step.y;
            t_enter = t_max_z;
            t_max_z += t_delta_z;
            normal = Vec3::new(0.0, 0.0, -step.y as f32);
        }
    }
}

fn ramp_normal(cell: Cell) -> Vec3 {
    match cell {
        Cell::Ramp { low_height, high_height, dir } => {
            let s = (high_height - low_height) / CELL_SIZE;
            (Vec3::Y - dir.world_dir() * s).normalize()
        }
        _ => Vec3::Y,
    }
}

/// Cast a ray through the level (walls, floors incl. ramps, ceilings).
/// `dir` need not be normalized. Returns the first solid hit within `max_dist`.
pub fn raycast_grid(grid: &LevelGrid, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<RayHit> {
    let dir = dir.try_normalize()?;
    let mut result = None;
    let mut traversed = 0u32;
    dda_xz(origin, dir, max_dist, |c, t0, t1, entry_n| {
        let cell = grid.cell(c);
        let at = |t: f32| origin + dir * t;
        if cell.is_wall() {
            let n = if entry_n == Vec3::ZERO { -dir } else { entry_n };
            result = Some(RayHit { distance: t0, point: at(t0), normal: n, cell: c, kind: HitKind::Wall, cells_traversed: traversed });
            return true;
        }
        let lo = c.as_vec2() * CELL_SIZE;
        let floor_at = |t: f32| {
            let p = at(t);
            let frac = (Vec2::new(p.x, p.z) - lo) / CELL_SIZE;
            cell.floor_height_at_frac(frac).unwrap_or(0.0)
        };
        let mut best: Option<(f32, Vec3, HitKind)> = None;
        // Floor: g(t) = y(t) - floor(t), linear inside the cell.
        let g0 = at(t0).y - floor_at(t0);
        let g1 = at(t1).y - floor_at(t1);
        if g0 < 0.0 {
            let (n, kind) = if entry_n == Vec3::ZERO { (Vec3::Y, HitKind::Floor) } else { (entry_n, HitKind::Ledge) };
            best = Some((t0, n, kind));
        } else if g1 < 0.0 {
            let t = t0 + (t1 - t0) * g0 / (g0 - g1);
            best = Some((t, ramp_normal(cell), HitKind::Floor));
        }
        // Ceiling.
        if let Some(ceil) = grid.ceiling_at(c) {
            let h0 = ceil - at(t0).y;
            let h1 = ceil - at(t1).y;
            if h0 >= 0.0 && h1 < 0.0 {
                let t = t0 + (t1 - t0) * h0 / (h0 - h1);
                if best.is_none_or(|(bt, _, _)| t < bt) {
                    best = Some((t, Vec3::NEG_Y, HitKind::Ceiling));
                }
            }
        }
        if let Some((t, n, kind)) = best {
            result = Some(RayHit { distance: t, point: at(t), normal: n, cell: c, kind, cells_traversed: traversed });
            return true;
        }
        traversed += 1;
        false
    });
    result
}

/// Number of wall cells crossed by the XZ segment `from -> to`
/// (excluding the endpoint cells). Used for signal attenuation and LOS.
pub fn count_wall_occlusions(grid: &LevelGrid, from: Vec3, to: Vec3) -> u32 {
    let delta = to - from;
    let flat = Vec3::new(delta.x, 0.0, delta.z);
    let len = flat.length();
    if len < 1e-4 {
        return 0;
    }
    let start = cell_of(from.x, from.z);
    let end = cell_of(to.x, to.z);
    let mut count = 0;
    dda_xz(from, flat / len, len, |c, _, _, _| {
        if c != start && c != end && grid.cell(c).is_wall() {
            count += 1;
        }
        c == end
    });
    count
}

/// Clear line of sight between two points (no walls in between).
pub fn has_line_of_sight(grid: &LevelGrid, from: Vec3, to: Vec3) -> bool {
    let d = to - from;
    let dist = d.length();
    match raycast_grid(grid, from, d, dist) {
        Some(hit) => hit.distance >= dist - 1e-3,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{CATWALK_HEIGHT, PIT_DEPTH};

    fn test_grid() -> LevelGrid {
        // x: 0..8, z: 0..5
        LevelGrid::from_ascii(&[
            "########", //
            "#....E^#", //
            "#.#....#", //
            "#..~...#", //
            "########",
        ])
    }

    fn center(x: i32, z: i32) -> Vec3 {
        Vec3::new((x as f32 + 0.5) * CELL_SIZE, 0.0, (z as f32 + 0.5) * CELL_SIZE)
    }

    fn sim(grid: &LevelGrid, mut pos: Vec3, vel_xz: Vec2, secs: f32) -> MoveResult {
        let dt = 1.0 / 60.0;
        let mut vel = Vec3::new(vel_xz.x, 0.0, vel_xz.y);
        let mut r = MoveResult { pos, vel, grounded: false, on_hazard: false, hit_wall: false, hit_ceiling: false };
        for _ in 0..(secs / dt) as u32 {
            r = resolve_player_movement(grid, pos, vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT);
            pos = r.pos;
            vel = Vec3::new(vel_xz.x, r.vel.y, vel_xz.y);
        }
        r
    }

    #[test]
    fn floor_height_sampling_flat_and_ramp() {
        let g = test_grid();
        assert_eq!(sample_floor_height(&g, 6.0, 6.0), Some(0.0));
        assert_eq!(sample_floor_height(&g, 2.0, 2.0), None); // wall
        assert_eq!(sample_floor_height(&g, 26.0, 6.0), Some(CATWALK_HEIGHT));
        assert_eq!(sample_floor_height(&g, 14.0, 14.0), Some(PIT_DEPTH));
        // Ramp at x=5 rising east: x in [20,24].
        assert!((sample_floor_height(&g, 20.0, 6.0).unwrap() - 0.0).abs() < 1e-4);
        assert!((sample_floor_height(&g, 22.0, 6.0).unwrap() - 1.2).abs() < 1e-4);
        assert!((sample_floor_height(&g, 23.999, 6.0).unwrap() - 2.4).abs() < 1e-2);
    }

    #[test]
    fn standing_still_is_stable_and_grounded() {
        let g = test_grid();
        let r = sim(&g, center(1, 1), Vec2::ZERO, 1.0);
        assert!(r.grounded);
        assert!(r.pos.y.abs() < 1e-4);
        assert!((r.pos - center(1, 1)).length() < 1e-4);
    }

    #[test]
    fn wall_sliding_keeps_tangential_motion() {
        let g = test_grid();
        // Start at (1,2) cell... row z=2 col 1 is floor, col 2 is wall. Move diagonally +x,+z.
        let start = center(1, 2) + Vec3::X;
        let r = sim(&g, start, Vec2::new(8.0, 8.0), 0.2);
        // Blocked on X by the wall cell (2,2) whose -X face is x=8.
        assert!(r.pos.x <= 8.0 - PLAYER_RADIUS + 1e-3, "x = {}", r.pos.x);
        // But kept sliding along Z.
        assert!(r.pos.z > start.z + 1.2, "z = {}", r.pos.z);
        assert!(r.hit_wall);
    }

    #[test]
    fn no_tunneling_at_dash_speed() {
        let g = test_grid();
        let r = sim(&g, center(1, 1), Vec2::new(0.0, -60.0), 0.5);
        assert!(r.pos.z >= CELL_SIZE + PLAYER_RADIUS - 1e-3, "z = {}", r.pos.z);
    }

    #[test]
    fn climbs_ramp_onto_catwalk_but_not_ledge() {
        let g = test_grid();
        // Walk east along row 1 from (4,1): up the ramp (5,1) onto the catwalk (6,1).
        let r = sim(&g, center(4, 1), Vec2::new(8.0, 0.0), 1.2);
        assert!((r.pos.y - CATWALK_HEIGHT).abs() < 1e-3, "y = {}", r.pos.y);
        assert!(r.grounded);
        // Walking north from (6,2) into the catwalk side is blocked by the 2.4m ledge.
        let r = sim(&g, center(6, 2), Vec2::new(0.0, -8.0), 1.0);
        assert!(r.pos.z >= 2.0 * CELL_SIZE + PLAYER_RADIUS - 1e-3, "z = {}", r.pos.z);
        assert!(r.pos.y.abs() < 1e-3);
    }

    #[test]
    fn gravity_jump_and_ceiling_bump() {
        let g = test_grid();
        let dt = 1.0 / 60.0;
        let mut pos = center(1, 1);
        let mut vel = Vec3::new(0.0, JUMP_VELOCITY, 0.0);
        let mut max_y: f32 = 0.0;
        let mut landed = false;
        for i in 0..120 {
            let r = resolve_player_movement(&g, pos, vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT);
            pos = r.pos;
            vel = r.vel;
            max_y = max_y.max(pos.y);
            if i > 5 && r.grounded {
                landed = true;
                break;
            }
        }
        assert!(landed);
        let apex = JUMP_VELOCITY * JUMP_VELOCITY / (2.0 * GRAVITY);
        assert!((max_y - apex).abs() < 0.2, "apex {max_y} vs {apex}");
        // Huge upward velocity bumps the ceiling.
        let r = resolve_player_movement(&g, center(1, 1), Vec3::new(0.0, 400.0, 0.0), dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT);
        assert!(r.hit_ceiling);
        assert!((r.pos.y + PLAYER_HEIGHT - crate::level::CEILING_HEIGHT).abs() < 1e-3);
        assert!(r.vel.y <= 0.0);
    }

    #[test]
    fn hazard_pit_detected() {
        let g = test_grid();
        let r = sim(&g, center(3, 3), Vec2::ZERO, 0.5);
        assert!(r.on_hazard);
        assert!((r.pos.y - PIT_DEPTH).abs() < 1e-3);
        let r = sim(&g, center(1, 3), Vec2::ZERO, 0.2);
        assert!(!r.on_hazard);
    }

    #[test]
    fn pillars_block() {
        let mut g = test_grid();
        g.pillars.push(Vec2::new(20.0, 10.0));
        g.pillar_radii.push(0.5);
        let r = sim(&g, Vec3::new(14.0, 0.0, 10.0), Vec2::new(8.0, 0.0), 0.5);
        assert!(r.pos.x <= 20.0 - 0.9 + 1e-3, "x = {}", r.pos.x);
    }

    #[test]
    fn raycast_hits_wall_with_normal() {
        let g = test_grid();
        let o = center(1, 1) + Vec3::Y * 1.5;
        let hit = raycast_grid(&g, o, Vec3::NEG_X, 50.0).unwrap();
        assert_eq!(hit.kind, HitKind::Wall);
        assert!((hit.distance - 2.0).abs() < 1e-4);
        assert_eq!(hit.normal, Vec3::X);
        assert!((hit.point.x - 4.0).abs() < 1e-4);
        // Ray east along row 1 at y=1.0 meets the ramp surface at x = 20 + 4/2.4.
        let hit = raycast_grid(&g, center(1, 1) + Vec3::Y * 1.0, Vec3::X, 50.0).unwrap();
        assert!((hit.distance - (14.0 + 4.0 / 2.4)).abs() < 1e-3, "ramp at {}", hit.distance);
        assert_eq!(hit.kind, HitKind::Floor); // hits the ramp surface first
        assert!(hit.normal.x < 0.0 && hit.normal.y > 0.0);
        // Straight down hits the floor; straight up the ceiling.
        let down = raycast_grid(&g, o, Vec3::NEG_Y, 10.0).unwrap();
        assert_eq!(down.kind, HitKind::Floor);
        assert!((down.distance - 1.5).abs() < 1e-4);
        let up = raycast_grid(&g, o, Vec3::Y, 10.0).unwrap();
        assert_eq!(up.kind, HitKind::Ceiling);
        // Miss when out of range.
        assert!(raycast_grid(&g, o, Vec3::NEG_X, 1.0).is_none());
    }

    #[test]
    fn wall_occlusion_counting() {
        let g = LevelGrid::from_ascii(&[
            "#########", //
            "#.#.#.#.#", //
            "#.......#", //
            "#########",
        ]);
        let a = center(1, 1) + Vec3::Y;
        let b = center(7, 1) + Vec3::Y;
        assert_eq!(count_wall_occlusions(&g, a, b), 3);
        assert_eq!(count_wall_occlusions(&g, center(1, 2), center(7, 2)), 0);
        assert_eq!(count_wall_occlusions(&g, a, a), 0);
        assert!(!has_line_of_sight(&g, a, b));
        assert!(has_line_of_sight(&g, center(1, 2) + Vec3::Y, center(7, 2) + Vec3::Y));
    }
}
