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

use crate::assets_gen::GameAssets;
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

pub fn jammer_count_for_floor(floor: u32) -> usize {
    2 + floor.max(1) as usize
}

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
    let loop_chance = 0.10 + 0.01 * floor as f64;
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
    let ramp_dir = RampDir::ALL[rng.random_range(0..4)];
    // Ramp sits outside the pedestal and rises toward it.
    let ramp_cell = tower - ramp_dir.offset() * 2;
    g.set(ramp_cell, Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir: ramp_dir });
    g.pillars.push(tower_world_xz(tower));
    g.pillar_radii.push(TOWER_MAST_RADIUS);
    for (dx, dz) in [(1, 1), (5, 1), (1, 5), (5, 5)] {
        let c = amin + IVec2::new(dx, dz);
        if c != ramp_cell {
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

    Some(g)
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
            .add_systems(Update, animate_props.in_set(GameSet::World));
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
    let mut g = generate_level(run.floor, run.floor_seed());
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

fn spawn_level_geometry(commands: &mut Commands, g: &LevelGrid, a: &GameAssets, meshes: &mut Assets<Mesh>) {
    // Atmosphere: crimson Martian moonlight + ambient. (Fog lives on the camera.)
    commands.spawn((
        LevelEntity,
        DirectionalLight { color: Color::srgb(1.0, 0.35, 0.25), illuminance: 1800.0, shadow_maps_enabled: false, ..default() },
        Transform::from_xyz(0.0, 50.0, 0.0).looking_at(Vec3::new(30.0, 0.0, 45.0), Vec3::Y),
    ));

    let wall_mesh = meshes.add(Cuboid::new(CELL_SIZE, 1.0, CELL_SIZE));
    let ramp_mesh = meshes.add(ramp_wedge_mesh(CELL_SIZE, CATWALK_HEIGHT));

    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            let center = Vec3::new((x as f32 + 0.5) * CELL_SIZE, 0.0, (z as f32 + 0.5) * CELL_SIZE);
            match g.cell(c) {
                Cell::Wall => {
                    // Skip walls fully enclosed by walls (never visible).
                    let visible = (-1..=1).any(|dz| (-1..=1).any(|dx| !g.cell(c + IVec2::new(dx, dz)).is_wall()));
                    if !visible {
                        continue;
                    }
                    let near_atrium =
                        (-1..=1).any(|dz| (-1..=1).any(|dx| g.is_roof_open(c + IVec2::new(dx, dz))));
                    let top = if near_atrium { ATRIUM_WALL_HEIGHT } else { CEILING_HEIGHT };
                    let h = top - SLAB_BOTTOM;
                    commands.spawn((
                        LevelEntity,
                        Mesh3d(wall_mesh.clone()),
                        MeshMaterial3d(if near_atrium { a.wall_tall.clone() } else { a.wall.clone() }),
                        Transform::from_translation(center.with_y(SLAB_BOTTOM + h * 0.5))
                            .with_scale(Vec3::new(1.0, h, 1.0)),
                    ));
                }
                cell => {
                    let (top, mat) = match cell {
                        Cell::Floor { height } if height > 0.1 => (height, a.catwalk.clone()),
                        Cell::Floor { height } => (height, a.floor.clone()),
                        Cell::AtriumFloor => (0.0, a.regolith.clone()),
                        Cell::HazardPit => (PIT_DEPTH, a.lava.clone()),
                        Cell::Ramp { low_height, .. } => (low_height, a.floor.clone()),
                        Cell::Wall => unreachable!(),
                    };
                    let slab_h = top - SLAB_BOTTOM;
                    commands.spawn((
                        LevelEntity,
                        Mesh3d(a.slab_mesh.clone()),
                        MeshMaterial3d(mat),
                        Transform::from_translation(center.with_y(SLAB_BOTTOM + slab_h * 0.5))
                            .with_scale(Vec3::new(1.0, slab_h, 1.0)),
                    ));
                    if let Cell::Ramp { low_height, high_height, dir } = cell {
                        let wedge_mesh = if (high_height - low_height - CATWALK_HEIGHT).abs() < 1e-3 {
                            ramp_mesh.clone()
                        } else {
                            meshes.add(ramp_wedge_mesh(CELL_SIZE, high_height - low_height))
                        };
                        // Mesh rises toward -Z; rotate so it rises along `dir`.
                        let yaw = yaw_facing(Vec2::new(dir.world_dir().x, dir.world_dir().z));
                        commands.spawn((
                            LevelEntity,
                            Mesh3d(wedge_mesh),
                            MeshMaterial3d(a.catwalk.clone()),
                            Transform::from_translation(center.with_y(low_height))
                                .with_rotation(Quat::from_rotation_y(yaw)),
                        ));
                    }
                    if matches!(cell, Cell::HazardPit) {
                        commands.spawn((
                            LevelEntity,
                            PointLight {
                                color: Color::srgb(1.0, 0.3, 0.05),
                                intensity: 250_000.0,
                                range: 9.0,
                                shadow_maps_enabled: false,
                                ..default()
                            },
                            Transform::from_translation(center.with_y(PIT_DEPTH + 0.6)),
                        ));
                    }
                    if !g.is_roof_open(c) {
                        commands.spawn((
                            LevelEntity,
                            Mesh3d(a.ceiling_mesh.clone()),
                            MeshMaterial3d(a.ceiling.clone()),
                            Transform::from_translation(center.with_y(CEILING_HEIGHT + 0.15)),
                        ));
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
        commands.spawn((
            LevelEntity,
            Mesh3d(a.pillar_mesh.clone()),
            MeshMaterial3d(a.wall.clone()),
            Transform::from_xyz(p.x, SLAB_BOTTOM + h * 0.5, p.y).with_scale(Vec3::new(*r / PILLAR_RADIUS, h, *r / PILLAR_RADIUS)),
        ));
    }

    // Industrial bunker lights (amber, flicker-free baseline).
    for (i, l) in g.lights.iter().enumerate() {
        let pos = Vec3::new((l.x as f32 + 0.5) * CELL_SIZE, CEILING_HEIGHT - 0.6, (l.y as f32 + 0.5) * CELL_SIZE);
        let warm = i % 3 != 0;
        commands.spawn((
            LevelEntity,
            PointLight {
                color: if warm { Color::srgb(1.0, 0.62, 0.35) } else { Color::srgb(0.9, 0.25, 0.2) },
                intensity: 420_000.0,
                range: 16.0,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(pos),
        ));
        commands.spawn((
            LevelEntity,
            Mesh3d(a.lamp_mesh.clone()),
            MeshMaterial3d(a.lamp.clone()),
            Transform::from_translation(pos + Vec3::Y * 0.45),
        ));
    }

    spawn_signal_tower(commands, g, a);
    for &j in &g.jammers {
        spawn_jammer(commands, g, a, j);
    }
}

fn spawn_signal_tower(commands: &mut Commands, g: &LevelGrid, a: &GameAssets) {
    let base = g.tower_world();
    let mast_h = 14.0;
    let beacon = base + Vec3::Y * (mast_h + 0.6);
    commands
        .spawn((
            LevelEntity,
            SignalTower { beacon },
            Transform::from_translation(base),
            Visibility::default(),
        ))
        .with_children(|t| {
            t.spawn((
                Mesh3d(a.tower_mast_mesh.clone()),
                MeshMaterial3d(a.wall.clone()),
                Transform::from_xyz(0.0, mast_h * 0.5, 0.0).with_scale(Vec3::new(1.0, mast_h, 1.0)),
            ));
            // Emissive signal panels around the mast.
            for k in 0..4 {
                let ang = k as f32 * FRAC_PI_2;
                for level in 0..3 {
                    let y = 2.5 + level as f32 * 4.0;
                    t.spawn((
                        Mesh3d(a.tower_panel_mesh.clone()),
                        MeshMaterial3d(a.tower_panel.clone()),
                        Transform::from_translation(Quat::from_rotation_y(ang) * Vec3::new(0.0, y, -0.85))
                            .with_rotation(Quat::from_rotation_y(ang)),
                    ));
                }
            }
            // Cross arms.
            for level in 0..3 {
                t.spawn((
                    Mesh3d(a.tower_arm_mesh.clone()),
                    MeshMaterial3d(a.catwalk.clone()),
                    Transform::from_xyz(0.0, 4.5 + level as f32 * 4.0, 0.0)
                        .with_rotation(Quat::from_rotation_y(level as f32 * PI / 3.0)),
                ));
            }
            t.spawn((
                TowerBeacon,
                Mesh3d(a.beacon_mesh.clone()),
                MeshMaterial3d(a.beacon.clone()),
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

fn spawn_jammer(commands: &mut Commands, g: &LevelGrid, a: &GameAssets, c: IVec2) {
    let base = g.cell_center(c);
    let phase = (c.x * 7 + c.y * 13) as f32 * 0.37;
    commands
        .spawn((
            LevelEntity,
            DemonJammer::for_floor(g.floor),
            Transform::from_translation(base),
            Visibility::default(),
        ))
        .with_children(|j| {
            j.spawn((
                Mesh3d(a.jammer_stalk_mesh.clone()),
                MeshMaterial3d(a.jammer.clone()),
                Transform::from_xyz(0.0, 1.2, 0.0),
            ));
            j.spawn((
                JammerCore { phase },
                Mesh3d(a.jammer_core_mesh.clone()),
                MeshMaterial3d(a.jammer_core.clone()),
                Transform::from_xyz(0.0, 2.7, 0.0),
            ));
            for k in 0..3 {
                let ang = k as f32 * 2.0 * PI / 3.0 + phase;
                j.spawn((
                    Mesh3d(a.antenna_mesh.clone()),
                    MeshMaterial3d(a.jammer.clone()),
                    Transform::from_translation(Vec3::new(ang.cos() * 0.35, 3.6, ang.sin() * 0.35))
                        .with_rotation(Quat::from_rotation_z(0.35 * ang.cos()) * Quat::from_rotation_x(0.35 * ang.sin())),
                ));
            }
            j.spawn((
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

fn animate_props(
    time: Res<Time>,
    mut cores: Query<(&JammerCore, &mut Transform)>,
    mut beacons: Query<&mut PointLight, With<TowerBeacon>>,
) {
    let t = time.elapsed_secs();
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
}
