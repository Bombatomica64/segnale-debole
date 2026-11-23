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
    /// Some collision substep stood on / landed on the ground, even if the
    /// player left it again before the end of the tick (fast catwalk edges).
    pub touched_ground: bool,
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

/// Footprint radius (fraction of the collision radius) sampled for ground.
const FOOTPRINT: f32 = 0.85;

/// Highest floor under the player's footprint that is standable from `feet`
/// (i.e. not more than `step_height` above it). Nine samples: the center plus
/// four cardinal and four diagonal points on a `0.85 * radius` circle, so
/// grazing a catwalk corner diagonally supports the player like face-on contact.
fn ground_under(grid: &LevelGrid, p: Vec3, radius: f32, step_height: f32) -> Option<f32> {
    let r = radius * FOOTPRINT;
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    let offsets = [
        Vec2::ZERO,
        Vec2::new(r, 0.0),
        Vec2::new(-r, 0.0),
        Vec2::new(0.0, r),
        Vec2::new(0.0, -r),
        Vec2::new(d, d),
        Vec2::new(d, -d),
        Vec2::new(-d, d),
        Vec2::new(-d, -d),
    ];
    let mut best: Option<f32> = None;
    for o in offsets {
        if let Some(h) = sample_floor_height(grid, p.x + o.x, p.z + o.y)
            && h <= p.y + step_height {
                best = Some(best.map_or(h, |b: f32| b.max(h)));
            }
    }
    best
}

/// Distinct horizontal contact normals of one collision substep (allocation-free).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ContactPlanes {
    normals: [Vec2; 8],
    len: usize,
}

impl ContactPlanes {
    /// Record a unit normal unless an (almost) identical one is already known.
    pub fn add(&mut self, n: Vec2) {
        if self.len < self.normals.len() && !self.as_slice().iter().any(|m| m.dot(n) > 0.999) {
            self.normals[self.len] = n;
            self.len += 1;
        }
    }

    pub fn as_slice(&self) -> &[Vec2] {
        &self.normals[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Remove the component of `v` going into the plane with unit normal `n`.
fn clip_plane(v: Vec2, n: Vec2) -> Vec2 {
    let into = v.dot(n);
    if into < 0.0 { v - n * into } else { v }
}

/// Quake `PM_SlideMove` crease rule in the horizontal plane: clip `v` against
/// each blocking plane in turn (never against an averaged normal). If clipping
/// against a second, non-parallel plane drives the velocity back into the
/// first one, the player is wedged in an inside corner: the only free
/// direction is the vertical crease, so horizontal velocity becomes zero
/// (instead of bouncing backwards out of the corner).
pub fn clip_velocity_to_planes(v: Vec2, planes: &[Vec2]) -> Vec2 {
    const EPS: f32 = -1e-4;
    let mut v = v;
    for (i, &ni) in planes.iter().enumerate() {
        if v.dot(ni) >= EPS {
            continue;
        }
        let mut c = clip_plane(v, ni);
        for (j, &nj) in planes.iter().enumerate() {
            if j == i || c.dot(nj) >= EPS {
                continue;
            }
            c = clip_plane(c, nj);
            if c.dot(ni) < EPS {
                return Vec2::ZERO;
            }
        }
        v = c;
    }
    v
}

/// Push a circle at `xz` out of every blocking cell and pillar.
/// Returns the corrected position and every distinct push normal.
fn push_out(grid: &LevelGrid, mut xz: Vec2, feet: f32, radius: f32, step_height: f32) -> (Vec2, ContactPlanes) {
    let mut planes = ContactPlanes::default();
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
                planes.add(n);
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
                planes.add(n);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    (xz, planes)
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
///
/// `was_grounded` is whether the body stood on the ground at the start of the
/// step: only a grounded body is glued down small drops (ramp descent / step
/// down); an airborne one falls freely onto whatever is below.
#[allow(clippy::too_many_arguments)]
pub fn resolve_player_movement(
    grid: &LevelGrid,
    pos: Vec3,
    vel: Vec3,
    dt: f32,
    radius: f32,
    height: f32,
    step_height: f32,
    was_grounded: bool,
) -> MoveResult {
    let mut vel = vel;
    vel.y -= GRAVITY * dt;
    // Substep so no tick moves more than half a radius horizontally (no wall
    // tunneling at bhop + dash + overclock speeds) or half a step vertically
    // (a fast fall at a low frame rate cannot skip through a catwalk top).
    let horiz_len = Vec2::new(vel.x, vel.z).length() * dt;
    let vert_len = vel.y.abs() * dt;
    let steps = ((horiz_len / (radius * 0.5)).max(vert_len / (step_height * 0.5).max(0.05)).ceil() as u32).clamp(1, 32);
    let sub_dt = dt / steps as f32;

    let mut p = pos;
    let mut grounded = false;
    let mut hit_wall = false;
    let mut hit_ceiling = false;
    let mut touched_ground = false;
    // Grounded at the start of the current substep.
    let mut on_ground = was_grounded;

    for _ in 0..steps {
        // Horizontal.
        let target = Vec2::new(p.x + vel.x * sub_dt, p.z + vel.z * sub_dt);
        let (xz, planes) = push_out(grid, target, p.y, radius, step_height);
        p.x = xz.x;
        p.z = xz.y;
        if !planes.is_empty() {
            hit_wall = true;
            let slid = clip_velocity_to_planes(Vec2::new(vel.x, vel.z), planes.as_slice());
            vel.x = slid.x;
            vel.z = slid.y;
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
            } else if on_ground && vel.y <= 0.0 && p.y - g <= step_height * 0.5 + (-vel.y * sub_dt) {
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
        touched_ground |= grounded;
        on_ground = grounded;
    }

    let on_hazard = grounded && matches!(grid.cell(cell_of(p.x, p.z)), Cell::HazardPit);
    MoveResult { pos: p, vel, grounded, on_hazard, hit_wall, hit_ceiling, touched_ground }
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
        let mut r = MoveResult { pos, vel, grounded: pos.y.abs() < 1e-4, on_hazard: false, hit_wall: false, hit_ceiling: false, touched_ground: false };
        for _ in 0..(secs / dt) as u32 {
            r = resolve_player_movement(grid, pos, vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, r.grounded);
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

    /// Step `secs` at `dt` with a constant horizontal velocity (vertical from physics).
    fn run(grid: &LevelGrid, pos: Vec3, vel: Vec3, dt: f32, secs: f32, mut each: impl FnMut(&MoveResult)) -> MoveResult {
        let mut r = MoveResult { pos, vel, grounded: false, on_hazard: false, hit_wall: false, hit_ceiling: false, touched_ground: false };
        for _ in 0..(secs / dt).round() as u32 {
            r = resolve_player_movement(grid, r.pos, r.vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, r.grounded);
            each(&r);
        }
        r
    }

    #[test]
    fn no_tunneling_at_peak_bhop_speed_walls_corners_pillars_ledges() {
        let g = test_grid();
        for dt in [1.0 / 60.0, 1.0 / 30.0, 1.0 / 20.0] {
            // Straight into the north wall.
            let r = run(&g, center(1, 1), Vec3::new(0.0, 0.0, -48.0), dt, 0.5, |_| {});
            assert!(r.pos.z >= CELL_SIZE + PLAYER_RADIUS - 1e-3, "wall z = {} (dt {dt})", r.pos.z);
            // Diagonally into the north-west room corner.
            let r = run(&g, center(1, 3), Vec3::new(-34.0, 0.0, -34.0), dt, 0.5, |_| {});
            assert!(r.pos.x >= CELL_SIZE + PLAYER_RADIUS - 1e-3 && r.pos.z >= CELL_SIZE + PLAYER_RADIUS - 1e-3, "corner {:?} (dt {dt})", r.pos);
            // Into the inner wall block (2,2) from the west, slightly off-axis.
            let r = run(&g, center(1, 2) + Vec3::Z * 0.7, Vec3::new(48.0, 0.0, 3.0), dt, 0.3, |_| {});
            assert!(r.pos.x <= 2.0 * CELL_SIZE - PLAYER_RADIUS + 1e-3, "block x = {} (dt {dt})", r.pos.x);
            // Into the side of the 2.4 m catwalk ledge (6,1) from the south.
            let r = run(&g, center(6, 3), Vec3::new(0.0, 0.0, -48.0), dt, 0.4, |_| {});
            assert!(r.pos.z >= 2.0 * CELL_SIZE + PLAYER_RADIUS - 1e-3, "ledge z = {} (dt {dt})", r.pos.z);
            assert!(r.pos.y.abs() < 1e-3, "stayed on the floor below the ledge");
            // Through a thin pillar.
            let mut gp = test_grid();
            gp.pillars.push(Vec2::new(20.0, 14.0));
            gp.pillar_radii.push(0.3);
            let r = run(&gp, Vec3::new(14.0, 0.0, 14.0), Vec3::new(48.0, 0.0, 0.0), dt, 0.3, |_| {});
            assert!(r.pos.x <= 20.0 - 0.7 + 1e-3, "pillar x = {} (dt {dt})", r.pos.x);
        }
    }

    #[test]
    fn fast_fall_at_low_frame_rate_lands_on_catwalk() {
        let g = test_grid();
        // A >1 m drop in one 20 FPS tick used to skip past the catwalk top when
        // the tick started just above it (the player then fell inside the block).
        for above in [0.05, 0.3, 0.8, 1.2] {
            let start = center(6, 1) + Vec3::Y * (CATWALK_HEIGHT + above);
            let r = run(&g, start, Vec3::new(0.0, -20.0, 0.0), 1.0 / 20.0, 0.5, |_| {});
            assert!(r.grounded);
            assert!((r.pos.y - CATWALK_HEIGHT).abs() < 1e-3, "fell through the catwalk from +{above}: y = {}", r.pos.y);
        }
    }

    #[test]
    fn ramp_climb_and_jump_launch_at_bhop_speed_without_snagging() {
        // Long east-rising ramp run: floor, ramp (x 20..24), catwalk (x 24..36).
        let g = LevelGrid::from_ascii(&[
            "##########", //
            "#....E^^^#", //
            "##########",
        ]);
        let dt = 1.0 / 60.0;
        // Running up the ramp at 30 m/s: no wall contact, no horizontal loss,
        // feet glued to the ramp surface, onto the catwalk.
        let r = run(&g, center(1, 1), Vec3::new(30.0, 0.0, 0.0), dt, 0.75, |r| {
            assert!(!r.hit_wall, "snagged at x = {}", r.pos.x);
            assert!((r.vel.x - 30.0).abs() < 1e-4);
            let floor = sample_floor_height(&g, r.pos.x, r.pos.z).unwrap();
            // Feet ride the highest point of the 0.85r footprint (0.6 slope * 0.34 m).
            assert!((r.pos.y - floor).abs() < 0.25, "left the ramp at x = {}: y {} vs {floor}", r.pos.x, r.pos.y);
        });
        assert!(r.pos.x > 26.0 && (r.pos.y - CATWALK_HEIGHT).abs() < 1e-3 && r.grounded, "{r:?}");

        // Bunny hop launched at the ramp foot: the upward velocity survives the
        // ramp riding up under the player and the hop clears onto the catwalk.
        let mut max_vy_drop: f32 = 0.0;
        let mut last_vy = JUMP_VELOCITY;
        let r = run(&g, Vec3::new(19.0, 0.0, 6.0), Vec3::new(30.0, JUMP_VELOCITY, 0.0), dt, 0.45, |r| {
            assert!(!r.hit_wall, "snagged at x = {}", r.pos.x);
            assert!((r.vel.x - 30.0).abs() < 1e-4);
            if !r.grounded {
                max_vy_drop = max_vy_drop.max(last_vy - r.vel.y);
                last_vy = r.vel.y;
            }
        });
        assert!(max_vy_drop <= GRAVITY * dt + 1e-3, "vertical velocity snapped down by {max_vy_drop}");
        assert!(r.pos.x > 30.0 && r.pos.y > CATWALK_HEIGHT - 1e-3, "{r:?}");
    }

    #[test]
    fn inside_corner_clips_per_plane_and_never_bounces_back() {
        // Pure crease rule: two perpendicular walls stop horizontal motion
        // instead of averaging the normals into a backwards kick.
        assert_eq!(clip_velocity_to_planes(Vec2::new(-30.0, -6.0), &[Vec2::X, Vec2::Y]), Vec2::ZERO);
        assert_eq!(clip_velocity_to_planes(Vec2::new(-30.0, -6.0), &[Vec2::Y, Vec2::X]), Vec2::ZERO);
        // One wall: slide along it.
        assert_eq!(clip_velocity_to_planes(Vec2::new(-30.0, -6.0), &[Vec2::X]), Vec2::new(0.0, -6.0));
        // Obtuse (135 deg) inside corner: slides along the second wall, never back.
        let diag = Vec2::new(1.0, 1.0).normalize();
        let v = clip_velocity_to_planes(Vec2::new(-30.0, -6.0), &[Vec2::X, diag]);
        assert!(v.dot(Vec2::X) >= -1e-3 && v.dot(diag) >= -1e-3, "{v:?}");
        // Moving away from both walls is untouched.
        assert_eq!(clip_velocity_to_planes(Vec2::new(5.0, 2.0), &[Vec2::X, Vec2::Y]), Vec2::new(5.0, 2.0));

        // Integrated: asymmetric bhop-speed impact into the north-west room
        // corner (walls at x = 4 and z = 4), both contacts in one substep.
        let g = test_grid();
        for dt in [1.0 / 144.0, 1.0 / 60.0, 1.0 / 20.0] {
            let start = Vec3::new(4.45, 0.0, 4.45);
            let mut r = MoveResult { pos: start, vel: Vec3::new(-30.0, 0.0, -6.0), grounded: true, on_hazard: false, hit_wall: false, hit_ceiling: false, touched_ground: false };
            let mut hit = false;
            for _ in 0..10 {
                r = resolve_player_movement(&g, r.pos, r.vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, r.grounded);
                hit |= r.hit_wall;
                assert!(r.vel.x <= 1e-3 && r.vel.z <= 1e-3, "corner bounced the player back: {:?} (dt {dt})", r.vel);
                assert!(r.pos.x >= CELL_SIZE + PLAYER_RADIUS - 1e-3 && r.pos.z >= CELL_SIZE + PLAYER_RADIUS - 1e-3, "{:?}", r.pos);
            }
            assert!(hit);
        }
    }

    #[test]
    fn airborne_descent_is_not_snapped_onto_a_catwalk_early() {
        let g = test_grid();
        let dt = 1.0 / 60.0;
        // Falling slowly 0.2 m above the catwalk top: still airborne after a tick...
        let start = center(6, 1) + Vec3::Y * (CATWALK_HEIGHT + 0.2);
        let r = resolve_player_movement(&g, start, Vec3::new(0.0, -1.0, 0.0), dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, false);
        assert!(!r.grounded && !r.touched_ground && r.pos.y > CATWALK_HEIGHT + 0.1, "{r:?}");
        // ...while a walking (grounded) body still glues down small drops.
        let r = resolve_player_movement(&g, start, Vec3::new(0.0, -1.0, 0.0), dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, true);
        assert!(r.grounded && (r.pos.y - CATWALK_HEIGHT).abs() < 1e-4, "{r:?}");
    }

    #[test]
    fn fast_catwalk_edge_crossing_reports_touchdown() {
        // Catwalk x 4..16; landing at 30 m/s 0.15 m past its edge touches down
        // and runs off again inside one 1/120 s substep.
        let g = LevelGrid::from_ascii(&["###########", "#^^^......#", "###########"]);
        let start = Vec3::new(16.15, CATWALK_HEIGHT + 0.01, 6.0);
        let r = resolve_player_movement(&g, start, Vec3::new(30.0, -3.0, 0.0), 1.0 / 120.0, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, false);
        assert!(!r.grounded && r.touched_ground, "{r:?}");
        assert!(r.pos.y > CATWALK_HEIGHT - 0.1, "{r:?}");
    }

    #[test]
    fn diagonal_footprint_grazes_a_catwalk_corner() {
        // Single catwalk cell [4,8] x [4,8]; feet just past its south-east
        // corner on the diagonal: only the diagonal footprint sample is on it.
        let g = LevelGrid::from_ascii(&["#####", "#^..#", "#...#", "#####"]);
        let feet = Vec3::new(8.2, CATWALK_HEIGHT, 8.2);
        assert_eq!(ground_under(&g, feet, PLAYER_RADIUS, PLAYER_STEP_HEIGHT), Some(CATWALK_HEIGHT));
        let r = resolve_player_movement(&g, feet, Vec3::ZERO, 1.0 / 60.0, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, true);
        assert!(r.grounded && (r.pos.y - CATWALK_HEIGHT).abs() < 1e-4, "{r:?}");
    }

    #[test]
    fn gravity_jump_and_ceiling_bump() {
        let g = test_grid();
        let dt = 1.0 / 60.0;
        let mut pos = center(1, 1);
        let mut vel = Vec3::new(0.0, JUMP_VELOCITY, 0.0);
        let mut max_y: f32 = 0.0;
        let mut landed = false;
        let mut grounded = true;
        for i in 0..120 {
            let r = resolve_player_movement(&g, pos, vel, dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, grounded);
            grounded = r.grounded;
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
        let r = resolve_player_movement(&g, center(1, 1), Vec3::new(0.0, 400.0, 0.0), dt, PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_STEP_HEIGHT, true);
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
