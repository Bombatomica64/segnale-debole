//! Procedural Mars Bunker labyrinth: pure grid generation + Bevy geometry spawning.
//!
//! World mapping: cell `(x, z)` covers `[x*CELL_SIZE, (x+1)*CELL_SIZE]` on X and
//! `[z*CELL_SIZE, (z+1)*CELL_SIZE]` on Z. `y` is up. "North" is `-Z`, "East" is `+X`.

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use crate::assets_gen::{FloorTheme, GameAssets, ThemeMaterials};
use crate::mesh_batch::{MaterialBatches, MeshBatch};
use crate::state::{AppState, GameSet, RunStats};

pub const CELL_SIZE: f32 = 4.0;
/// Ceiling height of covered cells.
pub const CEILING_HEIGHT: f32 = 6.0;
/// Height of walls bordering the open-roof atrium.
pub const ATRIUM_WALL_HEIGHT: f32 = 11.0;
/// Raised catwalks / tower pedestal.
pub const CATWALK_HEIGHT: f32 = 2.4;
/// Floor of lava / plasma pits.
pub const PIT_DEPTH: f32 = -1.2;
/// Bottom of all floor slabs (so pit edges and platform sides look solid).
pub const SLAB_BOTTOM: f32 = -1.6;
/// Max height difference walkable without jumping (shared with player physics).
pub const STEP_HEIGHT: f32 = 0.5;
pub const PILLAR_RADIUS: f32 = 0.45;
pub const TOWER_MAST_RADIUS: f32 = 0.7;
pub const ATRIUM_SIZE: i32 = 7;
/// Vertical launch speed of an atrium jump pad (onto the tower pedestal).
pub const JUMP_PAD_ATRIUM_UP: f32 = 13.5;
/// Barrels placed on floor `f`: `BARREL_BASE_COUNT + min(f, 4)` (5..=8).
pub const BARREL_BASE_COUNT: usize = 4;

/// A signal-gated jump pad: cell + launch velocity (m/s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JumpPadSpec {
    pub cell: IVec2,
    pub launch_vel: Vec3,
}

/// Ascent direction of a ramp (low edge -> high edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampDir {
    /// Rises toward -Z.
    North,
    /// Rises toward +Z.
    South,
    /// Rises toward +X.
    East,
    /// Rises toward -X.
    West,
}

impl RampDir {
    pub const ALL: [RampDir; 4] = [RampDir::North, RampDir::South, RampDir::East, RampDir::West];

    /// Unit grid offset pointing up the ramp.
    pub fn offset(self) -> IVec2 {
        match self {
            RampDir::North => IVec2::new(0, -1),
            RampDir::South => IVec2::new(0, 1),
            RampDir::East => IVec2::new(1, 0),
            RampDir::West => IVec2::new(-1, 0),
        }
    }

    pub fn from_offset(o: IVec2) -> Option<RampDir> {
        RampDir::ALL.into_iter().find(|d| d.offset() == o)
    }

    /// World-space unit vector pointing up the ramp (horizontal).
    pub fn world_dir(self) -> Vec3 {
        let o = self.offset();
        Vec3::new(o.x as f32, 0.0, o.y as f32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    Wall,
    Floor { height: f32 },
    Ramp { low_height: f32, high_height: f32, dir: RampDir },
    /// Lava / plasma trench at [`PIT_DEPTH`]; damages the player.
    HazardPit,
    /// Ground-level floor of the open-roof atrium (height 0).
    AtriumFloor,
}

impl Cell {
    pub fn is_wall(self) -> bool {
        matches!(self, Cell::Wall)
    }

    /// Floor height at the given fractional position inside the cell
    /// (`frac` in `[0,1]^2`, x along +X, y along +Z). `None` for walls.
    pub fn floor_height_at_frac(self, frac: Vec2) -> Option<f32> {
        match self {
            Cell::Wall => None,
            Cell::Floor { height } => Some(height),
            Cell::AtriumFloor => Some(0.0),
            Cell::HazardPit => Some(PIT_DEPTH),
            Cell::Ramp { low_height, high_height, dir } => {
                let f = frac.clamp(Vec2::ZERO, Vec2::ONE);
                let t = match dir {
                    RampDir::North => 1.0 - f.y,
                    RampDir::South => f.y,
                    RampDir::East => f.x,
                    RampDir::West => 1.0 - f.x,
                };
                Some(low_height + (high_height - low_height) * t)
            }
        }
    }

    /// Height at the middle of the edge facing `side` (unit grid offset), or `None`
    /// if that edge is not a walkable connection (walls, ramp side edges).
    pub fn edge_height(self, side: IVec2) -> Option<f32> {
        match self {
            Cell::Ramp { low_height, high_height, dir } => {
                if side == dir.offset() {
                    Some(high_height)
                } else if side == -dir.offset() {
                    Some(low_height)
                } else {
                    None
                }
            }
            other => {
                let frac = Vec2::splat(0.5) + side.as_vec2() * 0.5;
                other.floor_height_at_frac(frac)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomFeature {
    None,
    Catwalk,
    LavaPit,
    Pillars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub min: IVec2,
    pub size: IVec2,
    pub feature: RoomFeature,
}

impl Room {
    pub fn center(&self) -> IVec2 {
        self.min + self.size / 2
    }
    pub fn contains(&self, c: IVec2) -> bool {
        c.x >= self.min.x && c.y >= self.min.y && c.x < self.min.x + self.size.x && c.y < self.min.y + self.size.y
    }
}

/// The labyrinth of the current floor. Physics, signal and AI all query it.
#[derive(Resource, Debug, Clone)]
pub struct LevelGrid {
    pub width: i32,
    pub depth: i32,
    pub cells: Vec<Cell>,
    /// Cells with no ceiling (atrium): blood-red sky above.
    pub roof_open: Vec<bool>,
    pub spawn: IVec2,
    /// Yaw (radians, around +Y, 0 = facing -Z) the player starts with.
    pub spawn_yaw: f32,
    pub tower: IVec2,
    pub jammers: Vec<IVec2>,
    pub lights: Vec<IVec2>,
    /// Round collision columns, world XZ positions (radius in `pillar_radii`).
    pub pillars: Vec<Vec2>,
    pub pillar_radii: Vec<f32>,
    pub rooms: Vec<Room>,
    /// Signal-gated jump pads (non-blocking: never affect walking / BFS).
    pub jump_pads: Vec<JumpPadSpec>,
    /// Volatile server-coolant barrels (non-blocking shootable props).
    pub barrels: Vec<IVec2>,
    pub floor: u32,
    pub seed: u64,
    /// Run seed this grid was built for (used to detect a fresh run).
    pub run_seed: u64,
}

impl LevelGrid {
    /// Bare grid (all cells given) for tests and tools.
    pub fn from_cells(width: i32, depth: i32, cells: Vec<Cell>) -> Self {
        assert_eq!(cells.len(), (width * depth) as usize);
        LevelGrid {
            width,
            depth,
            cells,
            roof_open: vec![false; (width * depth) as usize],
            spawn: IVec2::ONE,
            spawn_yaw: 0.0,
            tower: IVec2::ONE,
            jammers: Vec::new(),
            lights: Vec::new(),
            pillars: Vec::new(),
            pillar_radii: Vec::new(),
            rooms: Vec::new(),
            jump_pads: Vec::new(),
            barrels: Vec::new(),
            floor: 1,
            seed: 0,
            run_seed: 0,
        }
    }

    /// Parse an ASCII map: `#` wall, `.` floor 0, `^` floor at catwalk height,
    /// `~` hazard pit, `A` atrium floor (roof open), `N/S/E/W` ramps 0 -> catwalk height.
    pub fn from_ascii(rows: &[&str]) -> Self {
        let depth = rows.len() as i32;
        let width = rows[0].len() as i32;
        let mut cells = Vec::with_capacity((width * depth) as usize);
        let mut open = Vec::with_capacity(cells.capacity());
        for row in rows {
            assert_eq!(row.len() as i32, width, "ragged ascii map");
            for ch in row.chars() {
                let ramp = |dir| Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir };
                cells.push(match ch {
                    '#' => Cell::Wall,
                    '.' => Cell::Floor { height: 0.0 },
                    '^' => Cell::Floor { height: CATWALK_HEIGHT },
                    '~' => Cell::HazardPit,
                    'A' => Cell::AtriumFloor,
                    'N' => ramp(RampDir::North),
                    'S' => ramp(RampDir::South),
                    'E' => ramp(RampDir::East),
                    'W' => ramp(RampDir::West),
                    other => panic!("unknown map char {other:?}"),
                });
                open.push(ch == 'A');
            }
        }
        let mut g = Self::from_cells(width, depth, cells);
        g.roof_open = open;
        g
    }

    pub fn in_bounds(&self, c: IVec2) -> bool {
        c.x >= 0 && c.y >= 0 && c.x < self.width && c.y < self.depth
    }

    fn idx(&self, c: IVec2) -> usize {
        (c.y * self.width + c.x) as usize
    }

    /// Cell at grid coords; out of bounds is solid wall.
    pub fn cell(&self, c: IVec2) -> Cell {
        if self.in_bounds(c) { self.cells[self.idx(c)] } else { Cell::Wall }
    }

    pub fn set(&mut self, c: IVec2, cell: Cell) {
        if self.in_bounds(c) {
            let i = self.idx(c);
            self.cells[i] = cell;
        }
    }

    pub fn is_roof_open(&self, c: IVec2) -> bool {
        self.in_bounds(c) && self.roof_open[self.idx(c)]
    }

    fn set_roof_open(&mut self, c: IVec2, open: bool) {
        if self.in_bounds(c) {
            let i = self.idx(c);
            self.roof_open[i] = open;
        }
    }

    pub fn world_to_cell(&self, p: Vec3) -> IVec2 {
        IVec2::new((p.x / CELL_SIZE).floor() as i32, (p.z / CELL_SIZE).floor() as i32)
    }

    /// Center of a cell at its floor height (0 for walls).
    pub fn cell_center(&self, c: IVec2) -> Vec3 {
        let h = self.cell(c).floor_height_at_frac(Vec2::splat(0.5)).unwrap_or(0.0);
        Vec3::new((c.x as f32 + 0.5) * CELL_SIZE, h, (c.y as f32 + 0.5) * CELL_SIZE)
    }

    pub fn spawn_world(&self) -> Vec3 {
        self.cell_center(self.spawn)
    }

    pub fn tower_world(&self) -> Vec3 {
        self.cell_center(self.tower)
    }

    /// Ceiling height above a cell (`None` = open sky).
    pub fn ceiling_at(&self, c: IVec2) -> Option<f32> {
        if self.is_roof_open(c) { None } else { Some(CEILING_HEIGHT) }
    }

    /// Whether a walker can move from `a` to the 4-neighbour `b` without jumping.
    /// Drops of any height are allowed; climbs are limited to [`STEP_HEIGHT`];
    /// hazard pits are avoided.
    pub fn can_walk(&self, a: IVec2, b: IVec2) -> bool {
        let side = b - a;
        let (ca, cb) = (self.cell(a), self.cell(b));
        if matches!(cb, Cell::HazardPit) || cb.is_wall() || ca.is_wall() {
            return false;
        }
        match (ca.edge_height(side), cb.edge_height(-side)) {
            (Some(ha), Some(hb)) => hb - ha <= STEP_HEIGHT,
            _ => false,
        }
    }

    /// BFS walking distances (in cells) from `start`; `None` = unreachable.
    pub fn walk_distances(&self, start: IVec2) -> Vec<Option<u32>> {
        let mut dist = vec![None; self.cells.len()];
        if !self.in_bounds(start) || self.cell(start).is_wall() {
            return dist;
        }
        let mut q = VecDeque::new();
        dist[self.idx(start)] = Some(0);
        q.push_back(start);
        while let Some(c) = q.pop_front() {
            let d = dist[self.idx(c)].unwrap();
            for o in RampDir::ALL.map(RampDir::offset) {
                let n = c + o;
                if self.in_bounds(n) && dist[self.idx(n)].is_none() && self.can_walk(c, n) {
                    dist[self.idx(n)] = Some(d + 1);
                    q.push_back(n);
                }
            }
        }
        dist
    }

    /// Shortest walkable cell route `from -> to` (both ends included), built on
    /// [`Self::walk_distances`] by backtracking from `to`. `None` = unreachable.
    pub fn walk_route(&self, from: IVec2, to: IVec2) -> Option<Vec<IVec2>> {
        if !self.in_bounds(to) {
            return None;
        }
        let dist = self.walk_distances(from);
        let mut d = dist[self.idx(to)]?;
        let mut route = vec![to];
        let mut c = to;
        while d > 0 {
            c = RampDir::ALL
                .map(RampDir::offset)
                .into_iter()
                .map(|o| c + o)
                .find(|&p| self.in_bounds(p) && dist[self.idx(p)] == Some(d - 1) && self.can_walk(p, c))?;
            d -= 1;
            route.push(c);
        }
        route.reverse();
        Some(route)
    }

    pub fn is_reachable(&self, from: IVec2, to: IVec2) -> bool {
        self.in_bounds(to) && self.walk_distances(from)[self.idx(to)].is_some()
    }

    /// Spawn reaches the tower and every jammer.
    pub fn validate(&self) -> bool {
        let d = self.walk_distances(self.spawn);
        let ok = |c: IVec2| self.in_bounds(c) && d[self.idx(c)].is_some();
        ok(self.tower) && self.jammers.iter().all(|&j| ok(j))
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Grid side length for a floor (21 on floor 1, +2 per floor, capped at 29).
pub fn grid_size_for_floor(floor: u32) -> i32 {
    21 + 2 * (floor.clamp(1, 5) as i32 - 1)
}

/// Demon Jammers on a floor: 3 on Floor 1, +1 per floor, capped at
/// [`MAX_JAMMERS`] so deep Endless floors (whose grid stops growing at Floor 5)
/// can always place them.
pub fn jammer_count_for_floor(floor: u32) -> usize {
    (2 + floor.max(1) as usize).min(MAX_JAMMERS)
}

/// Jammer cap for Endless Overdrive floors.
pub const MAX_JAMMERS: usize = 10;

/// Generate a fully validated labyrinth. Deterministic for a given `(floor, seed)`.
pub fn generate_level(floor: u32, seed: u64) -> LevelGrid {
    for attempt in 0..48u64 {
        let s = seed.wrapping_add(attempt.wrapping_mul(0x2545_F491_4F6C_DD1D));
        // Later attempts drop vertical room features to guarantee success.
        let features = attempt < 40;
        if let Some(g) = try_generate(floor, s, features)
            && g.validate() {
                return g;
            }
    }
    // Plain maze + atrium is always connected.
    try_generate(floor, seed, false).expect("featureless generation cannot fail")
}

fn try_generate(floor: u32, seed: u64, features: bool) -> Option<LevelGrid> {
    let mut rng = StdRng::seed_from_u64(seed);
    let n = grid_size_for_floor(floor);
    let mut g = LevelGrid::from_cells(n, n, vec![Cell::Wall; (n * n) as usize]);
    g.floor = floor;
    g.seed = seed;
    let ground = Cell::Floor { height: 0.0 };

    // 1. Perfect maze over odd cells (iterative backtracker).
    let start = IVec2::ONE;
    g.set(start, ground);
    let mut stack = vec![start];
    while let Some(&c) = stack.last() {
        let mut dirs = RampDir::ALL.map(RampDir::offset);
        dirs.shuffle(&mut rng);
        let next = dirs.into_iter().find(|&o| {
            let t = c + o * 2;
            t.x > 0 && t.y > 0 && t.x < n - 1 && t.y < n - 1 && g.cell(t).is_wall()
        });
        match next {
            Some(o) => {
                g.set(c + o, ground);
                g.set(c + o * 2, ground);
                stack.push(c + o * 2);
            }
            None => {
                stack.pop();
            }
        }
    }

    // 2. Braid: knock out some separating walls to create loops (less dead ends).
    // High-speed flow: generous loops so bhop routes circle back instead of
    // dead-ending (capped so deep Endless floors never overflow random_bool).
    let loop_chance = (0.16 + 0.015 * floor as f64).min(0.45);
    for z in 1..n - 1 {
        for x in 1..n - 1 {
            let c = IVec2::new(x, z);
            if !g.cell(c).is_wall() || (x + z) % 2 == 0 {
                continue;
            }
            let horiz = !g.cell(c + IVec2::X).is_wall() && !g.cell(c - IVec2::X).is_wall();
            let vert = !g.cell(c + IVec2::Y).is_wall() && !g.cell(c - IVec2::Y).is_wall();
            if (horiz ^ vert) && rng.random_bool(loop_chance) {
                g.set(c, ground);
            }
        }
    }

    // 3. Atrium in the quadrant opposite the spawn (odd-aligned so it touches maze cells).
    let max_a = n - 1 - ATRIUM_SIZE; // last valid odd min coordinate
    let lo = (n / 2) | 1;
    let pick = |rng: &mut StdRng| {
        let v = rng.random_range(lo..=max_a.max(lo));
        if v % 2 == 0 { v - 1 } else { v }
    };
    let amin = IVec2::new(pick(&mut rng), pick(&mut rng));
    let atrium = Room { min: amin, size: IVec2::splat(ATRIUM_SIZE), feature: RoomFeature::None };
    for z in 0..ATRIUM_SIZE {
        for x in 0..ATRIUM_SIZE {
            let c = amin + IVec2::new(x, z);
            g.set(c, Cell::AtriumFloor);
            g.set_roof_open(c, true);
        }
    }
    let tower = atrium.center();
    g.tower = tower;
    for z in -1..=1 {
        for x in -1..=1 {
            g.set(tower + IVec2::new(x, z), Cell::Floor { height: CATWALK_HEIGHT });
        }
    }
    // Twin ramps (opposite or perpendicular) on the pedestal's mid-edges, so
    // kiting during the SOS upload flows up/down from several angles. Mid-edge
    // cells never hold an atrium pillar (those sit on the diagonals).
    let ramp_dir = RampDir::ALL[rng.random_range(0..4)];
    let second_dir = {
        let o = ramp_dir.offset();
        let choices = [-o, o.perp(), -o.perp()];
        RampDir::from_offset(choices[rng.random_range(0..3)]).expect("unit offset")
    };
    for dir in [ramp_dir, second_dir] {
        // Ramp sits outside the pedestal and rises toward it.
        g.set(tower - dir.offset() * 2, Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir });
    }
    g.pillars.push(tower_world_xz(tower));
    g.pillar_radii.push(TOWER_MAST_RADIUS);
    for (dx, dz) in [(1, 1), (5, 1), (1, 5), (5, 5)] {
        let c = amin + IVec2::new(dx, dz);
        if matches!(g.cell(c), Cell::AtriumFloor) {
            g.pillars.push(tower_world_xz(c));
            g.pillar_radii.push(PILLAR_RADIUS);
        }
    }
    g.rooms.push(atrium);

    // 4. Rooms (odd-aligned, non-overlapping with a 1-cell margin, away from spawn).
    let room_target = 3 + floor as usize;
    let mut tries = 0;
    while g.rooms.len() < room_target + 1 && tries < 200 {
        tries += 1;
        let size = IVec2::new(
            if rng.random_bool(0.6) { 5 } else { 3 },
            if rng.random_bool(0.6) { 5 } else { 3 },
        );
        let rx = rng.random_range(0..=(n - 1 - size.x) / 2) * 2 + 1;
        let rz = rng.random_range(0..=(n - 1 - size.y) / 2) * 2 + 1;
        let min = IVec2::new(rx, rz);
        if min.x + size.x > n - 1 || min.y + size.y > n - 1 {
            continue;
        }
        // Keep the spawn airlock area clear.
        if min.x <= 3 && min.y <= 3 {
            continue;
        }
        let overlaps = g.rooms.iter().any(|r| {
            min.x - 1 < r.min.x + r.size.x
                && r.min.x - 1 < min.x + size.x
                && min.y - 1 < r.min.y + r.size.y
                && r.min.y - 1 < min.y + size.y
        });
        if overlaps {
            continue;
        }
        let feature = if !features {
            RoomFeature::None
        } else if size.x == 5 && size.y == 5 {
            match rng.random_range(0..4) {
                0 => RoomFeature::None,
                1 => RoomFeature::LavaPit,
                2 => RoomFeature::Pillars,
                _ => RoomFeature::Catwalk,
            }
        } else if rng.random_bool(0.3) {
            RoomFeature::LavaPit
        } else {
            RoomFeature::None
        };
        let room = Room { min, size, feature };
        for z in 0..size.y {
            for x in 0..size.x {
                g.set(min + IVec2::new(x, z), ground);
            }
        }
        apply_room_feature(&mut g, &room, &mut rng);
        g.rooms.push(room);
    }

    // 5. Spawn airlock, facing an open neighbour.
    g.spawn = start;
    g.set(start, ground);
    g.spawn_yaw = [IVec2::X, IVec2::Y, IVec2::NEG_X, IVec2::NEG_Y]
        .into_iter()
        .find(|&o| !g.cell(start + o).is_wall())
        .map(|o| yaw_facing(o.as_vec2()))
        .unwrap_or(0.0);

    let dist = g.walk_distances(g.spawn);
    dist[g.idx(g.tower)]?;

    // 6. Demon jammers: prefer room centers / catwalk tops, then far corridor cells.
    let want = jammer_count_for_floor(floor);
    let mut candidates: Vec<IVec2> = Vec::new();
    for room in g.rooms.iter().skip(1) {
        candidates.push(match room.feature {
            RoomFeature::Catwalk => room.min + IVec2::new(3, 2),
            RoomFeature::LavaPit => room.min + IVec2::new(0, room.size.y / 2),
            RoomFeature::Pillars | RoomFeature::None => room.center(),
        });
    }
    candidates.shuffle(&mut rng);
    let mut corridor: Vec<IVec2> = (0..n * n)
        .map(|i| IVec2::new(i % n, i / n))
        .filter(|&c| matches!(g.cell(c), Cell::Floor { height } if height == 0.0))
        .collect();
    corridor.shuffle(&mut rng);
    candidates.extend(corridor);
    let min_spawn_dist = 6;
    for c in candidates {
        if g.jammers.len() >= want {
            break;
        }
        let Some(d) = dist[g.idx(c)] else { continue };
        let standable = matches!(g.cell(c), Cell::Floor { .. });
        let far_from_others = g.jammers.iter().all(|j| (*j - c).abs().element_sum() >= 5);
        let far_from_tower = (g.tower - c).abs().element_sum() >= 5;
        if standable && d >= min_spawn_dist && far_from_others && far_from_tower && !atrium.contains(c) {
            g.jammers.push(c);
        }
    }
    if g.jammers.len() < want {
        return None;
    }

    // 7. Industrial lights: greedy spacing over walkable cells.
    let mut floor_cells: Vec<IVec2> = (0..n * n)
        .map(|i| IVec2::new(i % n, i / n))
        .filter(|&c| !g.cell(c).is_wall() && !g.is_roof_open(c))
        .collect();
    floor_cells.shuffle(&mut rng);
    // Room centers first so rooms are always lit.
    let mut ordered: Vec<IVec2> = g.rooms.iter().skip(1).map(Room::center).collect();
    ordered.extend(floor_cells);
    for c in ordered {
        if g.lights.len() >= 48 {
            break;
        }
        if g.cell(c).is_wall() || g.is_roof_open(c) {
            continue;
        }
        if g.lights.iter().all(|l| (*l - c).abs().element_sum() >= 5) {
            g.lights.push(c);
        }
    }

    // 8. Signal jump pads, 9. explosive barrels (non-blocking props).
    place_jump_pads(&mut g, &mut rng);
    place_barrels(&mut g, &mut rng);

    Some(g)
}

/// Cells a prop must never occupy: spawn airlock area, tower, jammers, room
/// centers (pickups), ramps and pits.
fn prop_cell_free(g: &LevelGrid, c: IVec2) -> bool {
    let standable = matches!(g.cell(c), Cell::Floor { height } if height == 0.0) || matches!(g.cell(c), Cell::AtriumFloor);
    standable
        && (c - g.spawn).abs().element_sum() >= 4
        && c != g.tower
        && !g.jammers.contains(&c)
        && !g.rooms.iter().skip(1).any(|r| r.center() == c)
        && !g.jump_pads.iter().any(|p| p.cell == c)
}

fn place_jump_pads(g: &mut LevelGrid, rng: &mut StdRng) {
    let tower = g.tower;
    // Atrium: a free pedestal mid-edge (not a ramp), launching up and onto
    // the pedestal toward the mast.
    let mut atrium_slots: Vec<IVec2> =
        RampDir::ALL.map(RampDir::offset).into_iter().filter(|&o| matches!(g.cell(tower + o * 2), Cell::AtriumFloor)).collect();
    atrium_slots.shuffle(rng);
    let atrium_pad = |o: IVec2| JumpPadSpec { cell: tower + o * 2, launch_vel: Vec3::new(-o.x as f32 * 5.0, JUMP_PAD_ATRIUM_UP, -o.y as f32 * 5.0) };
    if let Some(&o) = atrium_slots.first() {
        g.jump_pads.push(atrium_pad(o));
    }

    // Rooms: catwalk launchers and across-the-room launchers.
    let want_rooms = (1 + g.floor as usize / 2).min(3);
    let mut rooms: Vec<Room> = g.rooms.iter().skip(1).copied().collect();
    rooms.shuffle(rng);
    let mut placed = 0;
    for room in rooms {
        if placed >= want_rooms {
            break;
        }
        let m = room.min;
        let spec = match room.feature {
            RoomFeature::Catwalk => {
                // In front of the raised plates, launching north onto them.
                let c = m + IVec2::new(2, 3);
                (matches!(g.cell(c + IVec2::NEG_Y), Cell::Floor { height } if height > 0.1))
                    .then_some(JumpPadSpec { cell: c, launch_vel: Vec3::new(0.0, 11.0, -5.0) })
            }
            RoomFeature::None | RoomFeature::Pillars => {
                // West edge of the middle row, launching east across the room.
                let c = m + IVec2::new(0, room.size.y / 2);
                let path_clear = (1..room.size.x).all(|k| matches!(g.cell(c + IVec2::X * k), Cell::Floor { height } if height == 0.0));
                path_clear.then_some(JumpPadSpec { cell: c, launch_vel: Vec3::new(9.0, 10.0, 0.0) })
            }
            RoomFeature::LavaPit => None,
        };
        if let Some(spec) = spec
            && prop_cell_free(g, spec.cell)
        {
            g.jump_pads.push(spec);
            placed += 1;
        }
    }
    // Always at least two pads: fall back to a second atrium launcher.
    if g.jump_pads.len() < 2
        && let Some(&o) = atrium_slots.get(1)
    {
        g.jump_pads.push(atrium_pad(o));
    }
}

/// Straight 1-cell corridor or dead end (props there would read as blockers).
fn is_chokepoint(g: &LevelGrid, c: IVec2) -> bool {
    let open = |o: IVec2| !g.cell(c + o).is_wall();
    let n = RampDir::ALL.map(RampDir::offset).into_iter().filter(|&o| open(o)).count();
    n <= 1 || (n == 2 && ((open(IVec2::X) && open(IVec2::NEG_X)) || (open(IVec2::Y) && open(IVec2::NEG_Y))))
}

fn place_barrels(g: &mut LevelGrid, rng: &mut StdRng) {
    let want = BARREL_BASE_COUNT + (g.floor.max(1) as usize).min(4);
    let mut candidates: Vec<IVec2> = Vec::new();
    // Room corners.
    for room in g.rooms.iter().skip(1) {
        let s = room.size - IVec2::ONE;
        candidates.extend([IVec2::ZERO, IVec2::new(s.x, 0), IVec2::new(0, s.y), s].map(|o| room.min + o));
    }
    // Atrium perimeter.
    let amin = g.rooms[0].min;
    for k in 0..ATRIUM_SIZE {
        for o in [IVec2::new(k, 0), IVec2::new(k, ATRIUM_SIZE - 1), IVec2::new(0, k), IVec2::new(ATRIUM_SIZE - 1, k)] {
            candidates.push(amin + o);
        }
    }
    candidates.sort_by_key(|c| (c.y, c.x));
    candidates.dedup();
    candidates.shuffle(rng);
    let near_blocked = |g: &LevelGrid, c: IVec2| {
        RampDir::ALL.map(RampDir::offset).into_iter().any(|o| {
            let n = c + o;
            matches!(g.cell(n), Cell::Ramp { .. } | Cell::HazardPit) || g.jump_pads.iter().any(|p| p.cell == n)
        })
    };
    for c in candidates {
        if g.barrels.len() >= want {
            break;
        }
        if prop_cell_free(g, c) && !g.barrels.contains(&c) && !is_chokepoint(g, c) && !near_blocked(g, c) {
            g.barrels.push(c);
        }
    }
}

fn apply_room_feature(g: &mut LevelGrid, room: &Room, rng: &mut StdRng) {
    let m = room.min;
    match room.feature {
        RoomFeature::None => {}
        RoomFeature::Catwalk => {
            // Interior row: ramp (rising east) + two raised catwalk plates.
            g.set(m + IVec2::new(1, 2), Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir: RampDir::East });
            g.set(m + IVec2::new(2, 2), Cell::Floor { height: CATWALK_HEIGHT });
            g.set(m + IVec2::new(3, 2), Cell::Floor { height: CATWALK_HEIGHT });
        }
        RoomFeature::LavaPit => {
            let c = room.center();
            g.set(c, Cell::HazardPit);
            if room.size.x == 5 && rng.random_bool(0.5) {
                g.set(c + IVec2::X, Cell::HazardPit);
            }
        }
        RoomFeature::Pillars => {
            for (dx, dz) in [(1, 1), (4, 1), (1, 4), (4, 4)] {
                let p = Vec2::new((m.x + dx) as f32 * CELL_SIZE, (m.y + dz) as f32 * CELL_SIZE);
                g.pillars.push(p);
                g.pillar_radii.push(PILLAR_RADIUS);
            }
        }
    }
}

fn tower_world_xz(c: IVec2) -> Vec2 {
    Vec2::new((c.x as f32 + 0.5) * CELL_SIZE, (c.y as f32 + 0.5) * CELL_SIZE)
}

/// Yaw (around +Y) that makes a `-Z`-forward entity face the XZ direction `dir`.
pub fn yaw_facing(dir: Vec2) -> f32 {
    (-dir.x).atan2(-dir.y)
}

// ---------------------------------------------------------------------------
// Bevy side
// ---------------------------------------------------------------------------

/// Everything owned by the current floor; despawned on floor change / restart.
#[derive(Component)]
pub struct LevelEntity;

#[derive(Component, Debug, Clone)]
pub struct SignalTower {
    /// World position of the beacon (top of the mast).
    pub beacon: Vec3,
}

#[derive(Component)]
pub struct TowerBeacon;

#[derive(Component, Debug, Clone)]
pub struct DemonJammer {
    pub hp: f32,
    pub max_hp: f32,
    pub suppression_radius: f32,
    /// Bars removed at the jammer's center (falls off with distance).
    pub suppression_strength: f32,
    /// Seconds of hit-flash remaining (weapons / EMP).
    pub hit_flash: f32,
}

impl DemonJammer {
    pub fn for_floor(floor: u32) -> Self {
        let f = floor.max(1) as f32;
        DemonJammer {
            hp: 90.0 + 25.0 * f,
            max_hp: 90.0 + 25.0 * f,
            suppression_radius: 14.0 + 1.5 * f,
            suppression_strength: 3.0 + 0.25 * f,
            hit_flash: 0.0,
        }
    }
    pub fn is_active(&self) -> bool {
        self.hp > 0.0
    }
}

/// Signal-gated jump pad (powered at >= 2 bars at its position).
#[derive(Component, Debug, Clone)]
pub struct JumpPad {
    /// World position of the pad (center, floor height).
    pub pos: Vec3,
    pub launch_vel: Vec3,
    /// Seconds until the pad can launch again.
    pub cooldown: f32,
    pub powered: bool,
    /// Power state evaluated at least once (no power-up burst on spawn).
    pub primed: bool,
}

/// Emissive ring + core of a jump pad (material swapped with power).
#[derive(Component)]
pub struct JumpPadGlow {
    pub phase: f32,
}

/// Point light of a jump pad (color swapped with power).
#[derive(Component)]
pub struct JumpPadLight;

pub const PAD_POWERED_LIGHT: Color = Color::srgb(0.2, 1.0, 0.7);
pub const PAD_DORMANT_LIGHT: Color = Color::srgb(0.9, 0.22, 0.08);

/// Volatile server-coolant barrel (shootable, chain-detonating).
#[derive(Component, Debug, Clone, PartialEq)]
pub struct ExplosiveBarrel {
    pub hp: f32,
    pub max_hp: f32,
    /// Seconds until detonation once lethal damage landed.
    pub fuse: Option<f32>,
    pub exploded: bool,
    pub hit_flash: f32,
}

pub const BARREL_HP: f32 = 28.0;

impl Default for ExplosiveBarrel {
    fn default() -> Self {
        ExplosiveBarrel { hp: BARREL_HP, max_hp: BARREL_HP, fuse: None, exploded: false, hit_flash: 0.0 }
    }
}

impl ExplosiveBarrel {
    /// Still on the map and not yet counting down.
    pub fn is_intact(&self) -> bool {
        !self.exploded && self.fuse.is_none()
    }
}

/// World position (base, floor height) of the barrel on cell `c`: nudged
/// toward adjacent walls so drums sit against them.
pub fn barrel_world(g: &LevelGrid, c: IVec2) -> Vec3 {
    let mut p = g.cell_center(c);
    for o in RampDir::ALL.map(RampDir::offset) {
        if g.cell(c + o).is_wall() {
            p += Vec3::new(o.x as f32, 0.0, o.y as f32) * 1.1;
        }
    }
    p
}

/// Pulsing visual part of a jammer (scaled by `animate_props`).
#[derive(Component)]
pub struct JammerCore {
    pub phase: f32,
}

pub struct LevelPlugin;

impl Plugin for LevelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), build_level_if_needed)
            .add_systems(OnEnter(AppState::MainMenu), teardown_level)
            .add_systems(OnEnter(AppState::Loadout), teardown_level)
            .add_systems(OnEnter(AppState::ContractsFeats), teardown_level)
            .add_systems(Update, animate_props.in_set(GameSet::World))
            .add_systems(Update, cull_world_lights.in_set(GameSet::Ui));
    }
}

/// (Re)build the labyrinth when the run's floor differs from the current grid.
pub fn build_level_if_needed(
    mut commands: Commands,
    run: Res<RunStats>,
    grid: Option<Res<LevelGrid>>,
    existing: Query<Entity, With<LevelEntity>>,
    assets: Res<GameAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if let Some(g) = &grid
        && g.floor == run.floor && g.run_seed == run.run_seed {
            return;
        }
    for e in &existing {
        commands.entity(e).despawn();
    }
    let mut g = if run.mode == crate::state::RunMode::TimeTrial {
        crate::trials::build_time_trial_grid()
    } else {
        generate_level(run.floor, run.floor_seed())
    };
    g.run_seed = run.run_seed;
    spawn_level_geometry(&mut commands, &g, &assets, &mut meshes);
    info!(
        "Floor {} built: {}x{} cells, {} jammers, {} lights (seed {:#x})",
        g.floor,
        g.width,
        g.depth,
        g.jammers.len(),
        g.lights.len(),
        g.seed
    );
    commands.insert_resource(g);
}

pub fn teardown_level(mut commands: Commands, existing: Query<Entity, With<LevelEntity>>) {
    for e in &existing {
        commands.entity(e).despawn();
    }
    commands.remove_resource::<LevelGrid>();
}

/// Distance-culled world light: fades smoothly over `fade_band` approaching
/// `cull_dist`, then hides once beyond `cull_dist` (or while its base
/// intensity is zero). Never on viewmodel lights.
///
/// [`cull_world_lights`] is the sole writer of [`PointLight::intensity`] for
/// entities carrying this component: static lights leave `base_intensity`
/// unset at spawn (captured once on the first pass), while dynamic writers
/// (`EnemyAura`, `AlarmLight`, `FxLight`) update the unfaded target through
/// [`Self::set_intensity`].
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct CullableLight {
    pub cull_dist: f32,
    pub fade_band: f32,
    base_intensity: Option<f32>,
}

impl CullableLight {
    /// Margin past the light's own range before full cull (smoothly faded over
    /// [`Self::FADE_BAND`] so lights never pop on open sightlines).
    pub const MARGIN: f32 = 32.0;
    /// Width of the linear fade-out band immediately inside `cull_dist`.
    pub const FADE_BAND: f32 = 14.0;

    pub fn for_range(range: f32) -> Self {
        CullableLight {
            cull_dist: range + Self::MARGIN,
            fade_band: Self::FADE_BAND,
            base_intensity: None,
        }
    }

    /// Update the unfaded base intensity for a dynamically driven world light.
    pub fn set_intensity(&mut self, intensity: f32) {
        if self.base_intensity != Some(intensity) {
            self.base_intensity = Some(intensity);
        }
    }

    /// Distance attenuation factor in `[0.0, 1.0]` for a light at `dist`.
    pub fn fade_factor(self, dist: f32) -> f32 {
        if dist >= self.cull_dist {
            return 0.0;
        }
        let fade_start = (self.cull_dist - self.fade_band).max(0.0);
        if dist <= fade_start || self.fade_band <= 0.0 {
            1.0
        } else {
            ((self.cull_dist - dist) / self.fade_band).clamp(0.0, 1.0)
        }
    }
}

/// Smoothly attenuate and hide distant world lights. Writes are change-guarded
/// so `Changed<Visibility>` / `Changed<PointLight>` only fire when needed.
pub fn cull_world_lights(
    players: Query<&Transform, With<crate::player::Player>>,
    mut lights: Query<(&mut CullableLight, Ref<GlobalTransform>, &mut PointLight, &mut Visibility)>,
) {
    let Ok(ptf) = players.single() else { return };
    let eye = ptf.translation + Vec3::Y * crate::player::PLAYER_EYE_HEIGHT;
    for (mut c, g, mut light, mut vis) in &mut lights {
        // Spawned since the last pass: its GlobalTransform may not be
        // propagated yet, so keep its spawn visibility for this frame.
        if g.is_added() {
            continue;
        }
        let base = *c.base_intensity.get_or_insert(light.intensity);
        let dist = g.translation().distance(eye);
        let fade = c.fade_factor(dist);
        let target_vis = if fade > 0.0 && base > 0.0 { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != target_vis {
            *vis = target_vis;
        }
        let target_intensity = if fade > 0.0 && base > 0.0 { base * fade } else { 0.0 };
        if (light.intensity - target_intensity).abs() > 1e-3 || (target_intensity == 0.0 && light.intensity != 0.0) {
            light.intensity = target_intensity;
        }
    }
}

/// Top of the wall block spawned at `c` (`None`: not a wall, or a wall fully
/// enclosed by walls, which is never spawned).
pub(crate) fn spawned_wall_top(g: &LevelGrid, c: IVec2) -> Option<f32> {
    if !g.in_bounds(c) || !g.cell(c).is_wall() {
        return None;
    }
    let visible = (-1..=1).any(|dz| (-1..=1).any(|dx| !g.cell(c + IVec2::new(dx, dz)).is_wall()));
    if !visible {
        return None;
    }
    let near_atrium = (-1..=1).any(|dz| (-1..=1).any(|dx| g.is_roof_open(c + IVec2::new(dx, dz))));
    Some(if near_atrium { ATRIUM_WALL_HEIGHT } else { CEILING_HEIGHT })
}

/// Top of the floor slab of a non-wall cell.
pub(crate) fn slab_top(cell: Cell) -> Option<f32> {
    match cell {
        Cell::Wall => None,
        Cell::Floor { height } => Some(height),
        Cell::AtriumFloor => Some(0.0),
        Cell::HazardPit => Some(PIT_DEPTH),
        Cell::Ramp { low_height, .. } => Some(low_height),
    }
}

/// Grid side a world-space face normal points to (`None`: vertical faces).
fn side_of(n: Vec3) -> Option<IVec2> {
    if n.x > 0.5 {
        Some(IVec2::X)
    } else if n.x < -0.5 {
        Some(IVec2::NEG_X)
    } else if n.z > 0.5 {
        Some(IVec2::Y)
    } else if n.z < -0.5 {
        Some(IVec2::NEG_Y)
    } else {
        None
    }
}

/// Source meshes for the static level batches (identical to the per-entity
/// primitives the geometry used before batching).
struct LevelSourceMeshes {
    block: Mesh,
    ceiling: Mesh,
    pillar: Mesh,
    lamp: Mesh,
    ramp: Mesh,
    tower_mast: Mesh,
    tower_panel: Mesh,
    tower_arm: Mesh,
    jammer_stalk: Mesh,
    antenna: Mesh,
}

impl LevelSourceMeshes {
    fn new() -> Self {
        LevelSourceMeshes {
            block: Mesh::from(Cuboid::new(CELL_SIZE, 1.0, CELL_SIZE)),
            ceiling: Mesh::from(Cuboid::new(CELL_SIZE, 0.3, CELL_SIZE)),
            pillar: Mesh::from(Cylinder::new(PILLAR_RADIUS, 1.0)),
            lamp: Mesh::from(Cuboid::new(0.9, 0.12, 0.4)),
            ramp: ramp_wedge_mesh(CELL_SIZE, CATWALK_HEIGHT),
            tower_mast: Mesh::from(Cylinder::new(TOWER_MAST_RADIUS * 0.8, 1.0)),
            tower_panel: Mesh::from(Cuboid::new(1.0, 2.4, 0.12)),
            tower_arm: Mesh::from(Cuboid::new(4.0, 0.2, 0.2)),
            jammer_stalk: Mesh::from(Cylinder::new(0.35, 2.4)),
            antenna: Mesh::from(Cylinder::new(0.05, 1.6)),
        }
    }
}

/// Static level geometry batched per material (one entity per material).
#[derive(Component)]
pub struct LevelBatch;

fn spawn_level_geometry(commands: &mut Commands, g: &LevelGrid, assets: &GameAssets, meshes: &mut Assets<Mesh>) {
    let theme = FloorTheme::for_floor(g.floor).spec();
    let a = assets.theme_for_floor(g.floor);
    // Atmosphere: sector-themed moonlight + ambient. (Fog lives on the camera.)
    commands.spawn((
        LevelEntity,
        DirectionalLight { color: theme.sun_color, illuminance: theme.sun_illuminance, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, 50.0, 0.0).looking_at(Vec3::new(30.0, 0.0, 45.0), Vec3::Y),
    ));

    let src = LevelSourceMeshes::new();
    let mut batches = MaterialBatches::new();

    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            let center = Vec3::new((x as f32 + 0.5) * CELL_SIZE, 0.0, (z as f32 + 0.5) * CELL_SIZE);
            match g.cell(c) {
                Cell::Wall => {
                    // Walls fully enclosed by walls are never visible (not spawned).
                    let Some(top) = spawned_wall_top(g, c) else { continue };
                    let h = top - SLAB_BOTTOM;
                    let mat = if top > CEILING_HEIGHT { &a.wall_tall } else { &a.wall };
                    let tf = Transform::from_translation(center.with_y(SLAB_BOTTOM + h * 0.5)).with_scale(Vec3::new(1.0, h, 1.0));
                    // Hidden: the bottom, and sides pressed against an equal-or-taller wall.
                    batches.batch(mat).append_filtered(&src.block, &tf, |n| {
                        if n.y < -0.5 {
                            return false;
                        }
                        match side_of(n) {
                            Some(o) => !spawned_wall_top(g, c + o).is_some_and(|t| t >= top),
                            None => true,
                        }
                    });
                }
                cell => {
                    let top = slab_top(cell).expect("non-wall cell");
                    let mat = match cell {
                        Cell::Floor { height } if height > 0.1 => &a.catwalk,
                        Cell::Floor { .. } | Cell::Ramp { .. } => &a.floor,
                        Cell::AtriumFloor => &a.regolith,
                        Cell::HazardPit => &a.lava,
                        Cell::Wall => unreachable!(),
                    };
                    let is_ramp = matches!(cell, Cell::Ramp { .. });
                    let slab_h = top - SLAB_BOTTOM;
                    let tf = Transform::from_translation(center.with_y(SLAB_BOTTOM + slab_h * 0.5)).with_scale(Vec3::new(1.0, slab_h, 1.0));
                    // Hidden: the bottom, a ramp slab's top (under the wedge), and sides
                    // against walls or equal-or-taller slabs.
                    batches.batch(mat).append_filtered(&src.block, &tf, |n| {
                        if n.y < -0.5 || (n.y > 0.5 && is_ramp) {
                            return false;
                        }
                        match side_of(n) {
                            Some(o) => {
                                let nb = g.cell(c + o);
                                !(nb.is_wall() || slab_top(nb).is_some_and(|t| t >= top))
                            }
                            None => true,
                        }
                    });
                    if let Cell::Ramp { low_height, high_height, dir } = cell {
                        let rise = high_height - low_height;
                        let custom;
                        let wedge = if (rise - CATWALK_HEIGHT).abs() < 1e-3 {
                            &src.ramp
                        } else {
                            custom = ramp_wedge_mesh(CELL_SIZE, rise);
                            &custom
                        };
                        // Mesh rises toward -Z; rotate so it rises along `dir`.
                        let yaw = yaw_facing(Vec2::new(dir.world_dir().x, dir.world_dir().z));
                        let tf = Transform::from_translation(center.with_y(low_height)).with_rotation(Quat::from_rotation_y(yaw));
                        batches.batch(&a.catwalk).append_filtered(wedge, &tf, |n| n.y > -0.5);
                    }
                    if matches!(cell, Cell::HazardPit) {
                        commands.spawn((
                            LevelEntity,
                            CullableLight::for_range(9.0),
                            PointLight {
                                color: theme.hazard_light_color,
                                intensity: 250_000.0,
                                range: 9.0,
                                shadow_maps_enabled: false,
                                ..default()
                            },
                            Transform::from_translation(center.with_y(PIT_DEPTH + 0.6)),
                        ));
                    }
                    if !g.is_roof_open(c) {
                        let tf = Transform::from_translation(center.with_y(CEILING_HEIGHT + 0.15));
                        // Hidden: the top, and sides shared with neighbouring ceilings
                        // or buried in tall walls.
                        batches.batch(&a.ceiling).append_filtered(&src.ceiling, &tf, |n| {
                            if n.y > 0.5 {
                                return false;
                            }
                            match side_of(n) {
                                Some(o) => {
                                    let n = c + o;
                                    let covered = if g.cell(n).is_wall() {
                                        spawned_wall_top(g, n).is_some_and(|t| t >= CEILING_HEIGHT + 0.3)
                                    } else {
                                        !g.is_roof_open(n)
                                    };
                                    !covered
                                }
                                None => true,
                            }
                        });
                    }
                }
            }
        }
    }

    // Pillars (the tower mast is pillar 0 and gets its own mesh below).
    for (i, (p, r)) in g.pillars.iter().zip(&g.pillar_radii).enumerate() {
        if i == 0 {
            continue;
        }
        let c = g.world_to_cell(Vec3::new(p.x, 0.0, p.y));
        let top = if g.is_roof_open(c) { ATRIUM_WALL_HEIGHT - 2.0 } else { CEILING_HEIGHT };
        let h = top - SLAB_BOTTOM;
        let tf = Transform::from_xyz(p.x, SLAB_BOTTOM + h * 0.5, p.y).with_scale(Vec3::new(*r / PILLAR_RADIUS, h, *r / PILLAR_RADIUS));
        batches.batch(&a.wall).append_filtered(&src.pillar, &tf, |n| n.y > -0.5);
    }

    // Industrial bunker lights (amber, flicker-free baseline).
    for (i, l) in g.lights.iter().enumerate() {
        let pos = Vec3::new((l.x as f32 + 0.5) * CELL_SIZE, CEILING_HEIGHT - 0.6, (l.y as f32 + 0.5) * CELL_SIZE);
        let warm = i % 3 != 0;
        commands.spawn((
            LevelEntity,
            CullableLight::for_range(16.0),
            PointLight {
                color: if warm { theme.lamp_warm } else { theme.lamp_accent },
                intensity: 420_000.0,
                range: 16.0,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(pos),
        ));
        batches.add(&src.lamp, &a.lamp, Transform::from_translation(pos + Vec3::Y * 0.45));
    }

    // Milestone 16 architecture: pilasters, skirting, crown trim, trusses,
    // conduits, lamp cages, column + catwalk detail, atrium framing and flush
    // wall props, merged into the static per-material batches (two extra
    // batches per floor: trim + accent; plates join the ceiling batch).
    let arch = crate::architecture::build_architecture(g);
    batches.merge(&a.trim, arch.trim);
    batches.merge(&a.accent_emissive, arch.accent);
    batches.merge(&a.ceiling, arch.plate);

    spawn_signal_tower(commands, g, assets, a, &src, &mut batches);
    for (mesh, mat) in batches.finish(meshes) {
        commands.spawn((LevelEntity, LevelBatch, Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY));
    }
    for &j in &g.jammers {
        spawn_jammer(commands, g, assets, &src, meshes, j);
    }
    for pad in &g.jump_pads {
        spawn_jump_pad(commands, g, assets, pad);
    }
    for &c in &g.barrels {
        spawn_barrel(commands, assets, barrel_world(g, c));
    }
}

fn spawn_jump_pad(commands: &mut Commands, g: &LevelGrid, a: &GameAssets, spec: &JumpPadSpec) {
    let pos = g.cell_center(spec.cell);
    let phase = (spec.cell.x * 5 + spec.cell.y * 11) as f32 * 0.41;
    commands
        .spawn((
            LevelEntity,
            JumpPad { pos, launch_vel: spec.launch_vel, cooldown: 0.0, powered: false, primed: false },
            Transform::from_translation(pos),
            Visibility::default(),
        ))
        .with_children(|p| {
            for (mesh, mat) in &a.pad_base_model {
                p.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::IDENTITY));
            }
            p.spawn((JumpPadGlow { phase }, Mesh3d(a.pad_glow_mesh.clone()), MeshMaterial3d(a.pad_dormant.clone()), Transform::IDENTITY));
            p.spawn((
                JumpPadLight,
                CullableLight::for_range(7.0),
                PointLight { color: PAD_DORMANT_LIGHT, intensity: 90_000.0, range: 7.0, shadow_maps_enabled: false, ..default() },
                Transform::from_xyz(0.0, 0.9, 0.0),
            ));
        });
}

/// Spawn a barrel from the pre-baked drum model (shared meshes + materials).
pub fn spawn_barrel(commands: &mut Commands, a: &GameAssets, pos: Vec3) {
    commands
        .spawn((LevelEntity, ExplosiveBarrel::default(), Transform::from_translation(pos), Visibility::default()))
        .with_children(|b| {
            for (mesh, mat) in &a.barrel_model {
                b.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::IDENTITY));
            }
        });
}

fn spawn_signal_tower(commands: &mut Commands, g: &LevelGrid, assets: &GameAssets, a: &ThemeMaterials, src: &LevelSourceMeshes, batches: &mut MaterialBatches) {
    let base = g.tower_world();
    let mast_h = 14.0;
    let beacon = base + Vec3::Y * (mast_h + 0.6);
    // The static mast, signal panels and cross arms join the level batches
    // (baked at their world placement under the tower root).
    let at = |local: Transform| Transform::from_translation(base).mul_transform(local);
    batches.add(&src.tower_mast, &a.wall, at(Transform::from_xyz(0.0, mast_h * 0.5, 0.0).with_scale(Vec3::new(1.0, mast_h, 1.0))));
    // Emissive signal panels around the mast.
    for k in 0..4 {
        let ang = k as f32 * FRAC_PI_2;
        for level in 0..3 {
            let y = 2.5 + level as f32 * 4.0;
            batches.add(
                &src.tower_panel,
                &assets.tower_panel,
                at(Transform::from_translation(Quat::from_rotation_y(ang) * Vec3::new(0.0, y, -0.85)).with_rotation(Quat::from_rotation_y(ang))),
            );
        }
    }
    // Cross arms.
    for level in 0..3 {
        batches.add(
            &src.tower_arm,
            &a.catwalk,
            at(Transform::from_xyz(0.0, 4.5 + level as f32 * 4.0, 0.0).with_rotation(Quat::from_rotation_y(level as f32 * PI / 3.0))),
        );
    }
    commands
        .spawn((
            LevelEntity,
            SignalTower { beacon },
            Transform::from_translation(base),
            Visibility::default(),
        ))
        .with_children(|t| {
            t.spawn((
                TowerBeacon,
                Mesh3d(assets.beacon_mesh.clone()),
                MeshMaterial3d(assets.beacon.clone()),
                Transform::from_xyz(0.0, mast_h + 0.6, 0.0),
                PointLight {
                    color: Color::srgb(0.3, 1.0, 0.75),
                    intensity: 4_000_000.0,
                    range: 40.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
            ));
            // Uplink pad glow at the base.
            t.spawn((
                PointLight {
                    color: Color::srgb(0.2, 0.9, 1.0),
                    intensity: 600_000.0,
                    range: 12.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(0.0, 1.5, 0.0),
            ));
        });
}

fn spawn_jammer(commands: &mut Commands, g: &LevelGrid, a: &GameAssets, src: &LevelSourceMeshes, meshes: &mut Assets<Mesh>, c: IVec2) {
    let base = g.cell_center(c);
    let phase = (c.x * 7 + c.y * 13) as f32 * 0.37;
    // Stalk + 3 antennas share the flesh material: one baked mesh per jammer
    // (its handle lives on the jammer's child and drops with it).
    let mut shell = MeshBatch::new();
    shell.append(&src.jammer_stalk, &Transform::from_xyz(0.0, 1.2, 0.0));
    for k in 0..3 {
        let ang = k as f32 * 2.0 * PI / 3.0 + phase;
        shell.append(
            &src.antenna,
            &Transform::from_translation(Vec3::new(ang.cos() * 0.35, 3.6, ang.sin() * 0.35))
                .with_rotation(Quat::from_rotation_z(0.35 * ang.cos()) * Quat::from_rotation_x(0.35 * ang.sin())),
        );
    }
    let shell = meshes.add(shell.build().expect("jammer shell"));
    commands
        .spawn((
            LevelEntity,
            DemonJammer::for_floor(g.floor),
            Transform::from_translation(base),
            Visibility::default(),
        ))
        .with_children(|j| {
            j.spawn((Mesh3d(shell), MeshMaterial3d(a.jammer.clone()), Transform::IDENTITY));
            j.spawn((
                JammerCore { phase },
                Mesh3d(a.jammer_core_mesh.clone()),
                MeshMaterial3d(a.jammer_core.clone()),
                Transform::from_xyz(0.0, 2.7, 0.0),
            ));
            j.spawn((
                CullableLight::for_range(12.0),
                PointLight {
                    color: Color::srgb(1.0, 0.05, 0.12),
                    intensity: 500_000.0,
                    range: 12.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(0.0, 2.7, 0.0),
            ));
        });
}

/// Active theme assets + grid + material store for the hazard / accent pulse.
type HazardMaterials<'w> = (Option<Res<'w, GameAssets>>, Option<Res<'w, LevelGrid>>, Option<ResMut<'w, Assets<StandardMaterial>>>);

fn animate_props(
    time: Res<Time>,
    mut cores: Query<(&JammerCore, &mut Transform), Without<JumpPadGlow>>,
    mut beacons: Query<&mut PointLight, With<TowerBeacon>>,
    mut pads: Query<(&JumpPadGlow, &mut Transform), Without<JammerCore>>,
    hazard: HazardMaterials,
) {
    let t = time.elapsed_secs();
    // Hazard pit flow + accent LED pulse: only the active theme's two shared
    // startup materials are touched (no per-frame allocations). Note: Solari RT
    // ignores `uv_transform` on reflected hits, so emissive pulsing carries the
    // animation there while rasterized pits also scroll their UVs.
    if let (Some(assets), Some(grid), Some(mut materials)) = hazard {
        let theme = assets.theme_for_floor(grid.floor);
        let pulse = crate::architecture::hazard_pulse(t);
        if let Some(mut m) = materials.get_mut(theme.lava.id()) {
            m.uv_transform = pulse.uv;
            m.emissive = crate::architecture::scale_rgb(theme.lava_emissive_base, pulse.lava_gain);
        }
        if let Some(mut m) = materials.get_mut(theme.accent_emissive.id()) {
            m.emissive = crate::architecture::scale_rgb(theme.accent_emissive_base, pulse.accent_gain);
        }
    }
    for (glow, mut tf) in &mut pads {
        let s = 1.0 + 0.07 * (t * 4.0 + glow.phase).sin();
        tf.scale = Vec3::new(s, 1.0, s);
    }
    for (core, mut tf) in &mut cores {
        let s = 1.0 + 0.18 * (t * 5.0 + core.phase).sin();
        tf.scale = Vec3::splat(s);
        tf.rotate_y(time.delta_secs() * 1.3);
    }
    for mut light in &mut beacons {
        light.intensity = 2_500_000.0 + 2_000_000.0 * (0.5 + 0.5 * (t * 3.0).sin());
    }
}

/// Wedge (triangular prism) rising from y=0 at +Z edge to y=`rise` at -Z edge,
/// centered on X/Z, with sloped top, vertical back face, and side triangles.
pub fn ramp_wedge_mesh(size: f32, rise: f32) -> Mesh {
    let h = size * 0.5;
    // Corners.
    let fl = [-h, 0.0, h]; // front-left (low)
    let fr = [h, 0.0, h];
    let bl = [-h, 0.0, -h];
    let br = [h, 0.0, -h];
    let tl = [-h, rise, -h]; // top back-left
    let tr = [h, rise, -h];
    let slope_n = Vec3::new(0.0, size, rise).normalize().to_array();
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut nrm: Vec<[f32; 3]> = Vec::new();
    let mut uv: Vec<[f32; 2]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut quad = |p: [[f32; 3]; 4], n: [f32; 3]| {
        let b = pos.len() as u32;
        pos.extend(p);
        nrm.extend([n; 4]);
        uv.extend([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    };
    quad([fl, fr, tr, tl], slope_n); // slope
    quad([br, bl, tl, tr], [0.0, 0.0, -1.0]); // back
    quad([bl, br, fr, fl], [0.0, -1.0, 0.0]); // bottom
    let b = pos.len() as u32;
    // Left side triangle (normal -X) and right side (+X).
    pos.extend([fl, tl, bl]);
    nrm.extend([[-1.0, 0.0, 0.0]; 3]);
    uv.extend([[1.0, 1.0], [0.0, 0.0], [0.0, 1.0]]);
    pos.extend([fr, br, tr]);
    nrm.extend([[1.0, 0.0, 0.0]; 3]);
    uv.extend([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
    idx.extend([b, b + 1, b + 2, b + 3, b + 4, b + 5]);
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_indices(Indices::U32(idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_reachable_across_seeds_and_floors() {
        for floor in 1..=5 {
            for seed in 0..12u64 {
                let g = generate_level(floor, seed * 7_919 + floor as u64);
                let n = grid_size_for_floor(floor);
                assert_eq!((g.width, g.depth), (n, n));
                assert!(g.is_reachable(g.spawn, g.tower), "tower unreachable floor {floor} seed {seed}");
                assert_eq!(g.jammers.len(), jammer_count_for_floor(floor));
                for &j in &g.jammers {
                    assert!(g.is_reachable(g.spawn, j), "jammer {j} unreachable floor {floor} seed {seed}");
                    assert!(matches!(g.cell(j), Cell::Floor { .. }));
                }
                assert!(g.validate());
            }
        }
    }

    #[test]
    fn deep_endless_floors_generate_and_validate() {
        assert_eq!(jammer_count_for_floor(5), 7, "standard floors unchanged");
        assert_eq!(jammer_count_for_floor(8), MAX_JAMMERS);
        assert_eq!(jammer_count_for_floor(40), MAX_JAMMERS);
        for floor in [6u32, 10, 15, 25, 40] {
            for seed in 0..12u64 {
                let g = generate_level(floor, seed * 104_729 + floor as u64);
                assert!(g.validate(), "floor {floor} seed {seed}");
                assert_eq!(g.jammers.len(), jammer_count_for_floor(floor), "floor {floor} seed {seed}");
                assert!(g.is_reachable(g.spawn, g.tower));
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let a = generate_level(3, 42);
        let b = generate_level(3, 42);
        assert_eq!(a.cells, b.cells);
        assert_eq!(a.jammers, b.jammers);
        assert_eq!(a.tower, b.tower);
    }

    #[test]
    fn tower_sits_on_open_roof_raised_pedestal() {
        for seed in 0..10 {
            let g = generate_level(1, seed);
            assert!(g.is_roof_open(g.tower));
            assert_eq!(g.cell(g.tower), Cell::Floor { height: CATWALK_HEIGHT });
            assert!(g.tower_world().y > 2.0);
            // Spawn is enclosed (has a ceiling) and walkable.
            assert!(!g.is_roof_open(g.spawn));
            assert!(!g.cell(g.spawn).is_wall());
        }
    }

    #[test]
    fn walk_route_is_shortest_and_contiguous() {
        for seed in 0..6u64 {
            let g = generate_level(1, seed);
            let route = g.walk_route(g.spawn, g.tower).expect("spawn reaches tower");
            assert_eq!(route.first(), Some(&g.spawn));
            assert_eq!(route.last(), Some(&g.tower));
            assert_eq!(route.len() as u32 - 1, g.walk_distances(g.spawn)[g.idx(g.tower)].unwrap());
            for w in route.windows(2) {
                assert!(g.can_walk(w[0], w[1]), "step {:?} -> {:?}", w[0], w[1]);
            }
        }
        let g = generate_level(1, 3);
        assert!(g.walk_route(g.spawn, IVec2::ZERO).is_none(), "border wall is unreachable");
        assert_eq!(g.walk_route(g.spawn, g.spawn), Some(vec![g.spawn]));
    }

    #[test]
    fn border_is_solid() {
        let g = generate_level(5, 99);
        for i in 0..g.width {
            assert!(g.cell(IVec2::new(i, 0)).is_wall());
            assert!(g.cell(IVec2::new(i, g.depth - 1)).is_wall());
            assert!(g.cell(IVec2::new(0, i)).is_wall());
            assert!(g.cell(IVec2::new(g.width - 1, i)).is_wall());
        }
    }

    #[test]
    fn walking_rules_respect_heights_and_ramps() {
        let g = LevelGrid::from_ascii(&[
            "#######", //
            "#..E^.#", //
            "#.^~..#", //
            "#######",
        ]);
        // Up the ramp onto the catwalk.
        assert!(g.can_walk(IVec2::new(2, 1), IVec2::new(3, 1)));
        assert!(g.can_walk(IVec2::new(3, 1), IVec2::new(4, 1)));
        // Can't climb the catwalk side directly.
        assert!(!g.can_walk(IVec2::new(1, 2), IVec2::new(2, 2)));
        // Can drop off it.
        assert!(g.can_walk(IVec2::new(2, 2), IVec2::new(1, 2)));
        // Pits are avoided, ramp sides are not connections.
        assert!(!g.can_walk(IVec2::new(4, 2), IVec2::new(3, 2)));
        assert!(!g.can_walk(IVec2::new(3, 2), IVec2::new(3, 1)));
        assert!(g.is_reachable(IVec2::new(1, 1), IVec2::new(5, 1)));
    }

    #[test]
    fn ramp_heights_interpolate() {
        let r = Cell::Ramp { low_height: 0.0, high_height: 2.4, dir: RampDir::North };
        assert_eq!(r.floor_height_at_frac(Vec2::new(0.5, 1.0)), Some(0.0));
        assert_eq!(r.floor_height_at_frac(Vec2::new(0.5, 0.0)), Some(2.4));
        assert!((r.floor_height_at_frac(Vec2::splat(0.5)).unwrap() - 1.2).abs() < 1e-5);
        assert_eq!(r.edge_height(IVec2::new(0, -1)), Some(2.4));
        assert_eq!(r.edge_height(IVec2::new(1, 0)), None);
    }

    #[test]
    fn cullable_light_fades_smoothly_without_pop() {
        let c = CullableLight::for_range(12.0);
        assert_eq!(c.fade_factor(0.0), 1.0);
        assert_eq!(c.fade_factor(c.cull_dist - c.fade_band), 1.0);
        let mid = c.fade_factor(c.cull_dist - c.fade_band * 0.5);
        assert!((mid - 0.5).abs() < 1e-5, "midpoint fade {mid}");
        assert!(c.fade_factor(c.cull_dist - 0.1) > 0.0);
        assert_eq!(c.fade_factor(c.cull_dist), 0.0);
        assert_eq!(c.fade_factor(c.cull_dist + 20.0), 0.0);
    }

    #[test]
    fn extinguished_aura_near_fade_boundary_stays_dark_when_approached() {
        let mut world = World::new();
        let mut sched = Schedule::default();
        sched.add_systems(cull_world_lights);

        let cull = CullableLight::for_range(7.0);
        // Place the light just inside the outer edge of the fade band so the
        // faded intensity is tiny (~0.005).
        let near_edge = cull.cull_dist - 1e-5;
        let player = world
            .spawn((crate::player::Player::default(), Transform::from_xyz(0.0, 0.0, 0.0)))
            .id();
        let light = world
            .spawn((
                cull,
                PointLight { intensity: 7_000.0, range: 7.0, ..default() },
                GlobalTransform::from_xyz(near_edge, crate::player::PLAYER_EYE_HEIGHT, 0.0),
                Visibility::Inherited,
            ))
            .id();

        // First run skips `is_added`; second run evaluates the fade band near the edge.
        sched.run(&mut world);
        sched.run(&mut world);
        assert_eq!(*world.entity(light).get::<Visibility>().unwrap(), Visibility::Inherited);
        let faded = world.entity(light).get::<PointLight>().unwrap().intensity;
        assert!(faded > 0.0 && faded < 0.01, "expected tiny faded intensity, got {faded}");

        // Owner turns the aura off, then player walks right next to the light.
        world.entity_mut(light).get_mut::<CullableLight>().unwrap().set_intensity(0.0);
        world.entity_mut(player).get_mut::<Transform>().unwrap().translation = Vec3::new(near_edge, 0.0, 0.0);
        sched.run(&mut world);

        assert_eq!(*world.entity(light).get::<Visibility>().unwrap(), Visibility::Hidden);
        assert_eq!(world.entity(light).get::<PointLight>().unwrap().intensity, 0.0);
    }

    fn atrium_ramps(g: &LevelGrid) -> Vec<(IVec2, RampDir)> {
        RampDir::ALL
            .map(RampDir::offset)
            .into_iter()
            .map(|o| g.tower + o * 2)
            .filter_map(|c| match g.cell(c) {
                Cell::Ramp { dir, .. } => Some((c, dir)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn atrium_has_twin_ramps_onto_the_pedestal_clear_of_pillars() {
        for floor in 1..=5 {
            for seed in 0..12u64 {
                let g = generate_level(floor, seed * 104_729 + floor as u64);
                let ramps = atrium_ramps(&g);
                assert_eq!(ramps.len(), 2, "floor {floor} seed {seed}: twin ramps");
                for &(c, dir) in &ramps {
                    // Rises toward the tower, low end opens onto the atrium floor.
                    assert_eq!(c + dir.offset() * 2, g.tower);
                    assert!(g.can_walk(c - dir.offset(), c) && g.can_walk(c, c + dir.offset()), "walk up ramp {c}");
                    assert!(g.is_reachable(g.spawn, c + dir.offset()));
                    let (lo, hi) = (tower_world_xz(c) - Vec2::splat(CELL_SIZE * 0.5), tower_world_xz(c) + Vec2::splat(CELL_SIZE * 0.5));
                    for (p, r) in g.pillars.iter().zip(&g.pillar_radii) {
                        let inside = p.x + r > lo.x && p.x - r < hi.x && p.y + r > lo.y && p.y - r < hi.y;
                        assert!(!inside, "pillar {p} overlaps ramp {c}");
                    }
                }
                let (a, b) = (ramps[0].1.offset(), ramps[1].1.offset());
                assert!(a == -b || a.dot(b) == 0, "opposite or perpendicular");
            }
        }
    }

    #[test]
    fn jump_pads_and_barrels_are_placed_safely_and_reachably() {
        for floor in 1..=5 {
            for seed in [0u64, 7, 99, 1234, 2026, 31337, 0xBEEF] {
                let g = generate_level(floor, seed);
                assert!(g.validate());
                assert!((2..=4).contains(&g.jump_pads.len()), "floor {floor} seed {seed}: {} pads", g.jump_pads.len());
                let n_barrels = g.barrels.len();
                assert_eq!(n_barrels, BARREL_BASE_COUNT + (floor as usize).min(4), "floor {floor} seed {seed} barrels");
                assert!((4..=8).contains(&n_barrels));
                // At least one pad beside the pedestal, launching up and toward the tower.
                let atrium = g.jump_pads.iter().find(|p| (p.cell - g.tower).abs().element_sum() == 2 && g.is_roof_open(p.cell));
                let atrium = atrium.expect("atrium pad");
                assert!((atrium.launch_vel.y - JUMP_PAD_ATRIUM_UP).abs() < 1e-4);
                let to_tower = (g.tower - atrium.cell).as_vec2().normalize();
                assert!(Vec2::new(atrium.launch_vel.x, atrium.launch_vel.z).normalize().dot(to_tower) > 0.99);
                let mut used: Vec<IVec2> = Vec::new();
                for c in g.jump_pads.iter().map(|p| p.cell).chain(g.barrels.iter().copied()) {
                    assert!(!used.contains(&c), "prop cells unique: {c}");
                    used.push(c);
                    assert!(matches!(g.cell(c), Cell::Floor { height } if height == 0.0) || matches!(g.cell(c), Cell::AtriumFloor), "{c}: {:?}", g.cell(c));
                    assert!(c != g.spawn && c != g.tower && !g.jammers.contains(&c));
                    assert!(g.is_reachable(g.spawn, c), "prop {c} reachable");
                }
                for p in &g.jump_pads {
                    assert!(p.launch_vel.y >= 10.0 && p.launch_vel.y <= JUMP_PAD_ATRIUM_UP);
                    // Launch path clears hazard pits.
                    let step = IVec2::new(p.launch_vel.x.signum() as i32, p.launch_vel.z.signum() as i32);
                    for k in 1..=2 {
                        assert!(!matches!(g.cell(p.cell + step * k), Cell::HazardPit), "pad {} launches into lava", p.cell);
                    }
                }
                for &b in &g.barrels {
                    assert!(!is_chokepoint(&g, b), "barrel {b} in a 1-cell chokepoint");
                    for o in RampDir::ALL.map(RampDir::offset) {
                        assert!(!matches!(g.cell(b + o), Cell::Ramp { .. } | Cell::HazardPit), "barrel {b} crowds a ramp/pit");
                    }
                    let w = barrel_world(&g, b);
                    assert_eq!(g.world_to_cell(w), b, "nudged barrel stays in its cell");
                }
            }
        }
    }

    #[test]
    fn props_are_deterministic_per_floor_and_seed() {
        for floor in 1..=5 {
            let (a, b) = (generate_level(floor, 4242), generate_level(floor, 4242));
            assert_eq!(a.jump_pads, b.jump_pads);
            assert_eq!(a.barrels, b.barrels);
            assert_eq!(a.cells, b.cells);
            let c = generate_level(floor, 4243);
            assert!(c.cells != a.cells || c.barrels != a.barrels);
        }
    }

    #[test]
    fn playtest_bot_seeds_keep_valid_high_speed_layouts() {
        for seed in [1234u64, 99, 2026, 7, 31337] {
            for floor in 1..=5 {
                let g = generate_level(floor, seed);
                assert!(g.validate(), "seed {seed} floor {floor}");
                assert_eq!(g.jammers.len(), jammer_count_for_floor(floor));
                assert_eq!(atrium_ramps(&g).len(), 2);
            }
        }
    }

    #[test]
    fn braid_loops_cut_dead_ends() {
        // Higher loop chance: few dead-end corridor cells on every floor.
        for floor in 1..=5 {
            let g = generate_level(floor, 555);
            let open: Vec<IVec2> = (0..g.width * g.depth).map(|i| IVec2::new(i % g.width, i / g.width)).filter(|&c| !g.cell(c).is_wall()).collect();
            let dead_ends = open.iter().filter(|&&c| RampDir::ALL.map(RampDir::offset).into_iter().filter(|&o| !g.cell(c + o).is_wall()).count() == 1).count();
            assert!(dead_ends * 8 < open.len(), "floor {floor}: {dead_ends} dead ends of {} cells", open.len());
        }
    }

    #[test]
    fn barrel_defaults_and_intact_state() {
        let mut b = ExplosiveBarrel::default();
        assert_eq!((b.hp, b.max_hp), (BARREL_HP, BARREL_HP));
        assert!(b.is_intact());
        b.fuse = Some(0.1);
        assert!(!b.is_intact());
        b.fuse = None;
        b.exploded = true;
        assert!(!b.is_intact());
    }

    #[test]
    fn extreme_endless_floors_never_panic_on_loop_chance() {
        for floor in [58u32, 60, 100] {
            let g = generate_level(floor, 777);
            assert!(g.validate(), "floor {floor} invalid");
            assert_eq!(g.jammers.len(), jammer_count_for_floor(floor));
        }
    }
}
