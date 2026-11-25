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
/// Ceiling height of covered cells (generous bhop / jump-pad headroom).
pub const CEILING_HEIGHT: f32 = 7.2;
/// Height of walls bordering the open-roof atrium.
pub const ATRIUM_WALL_HEIGHT: f32 = 12.0;
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

/// Macro-grid pitch: a 2-cell (8 m) wide node / passage + a 1-cell wall line.
pub const MACRO_STEP: i32 = 3;
/// Macro nodes the spawn launch lane runs straight through (incl. the airlock).
pub const SPAWN_LANE_NODES: i32 = 3;
/// Chance a macro node stays solid rock (never carved by the maze).
const SOLID_NODE_CHANCE: f64 = 0.2;

/// Macro nodes per floor side: 8, 8, 9, 9, 10 (capped at Floor 5).
fn macro_nodes_for_floor(floor: u32) -> i32 {
    7 + (floor.clamp(1, 5) as i32 + 1) / 2
}

/// Grid side length for a floor: `MACRO_STEP * nodes + 1` (25, 25, 28, 28, 31).
pub fn grid_size_for_floor(floor: u32) -> i32 {
    MACRO_STEP * macro_nodes_for_floor(floor) + 1
}

/// Macro nodes along a grid side of `n` cells.
fn macro_nodes(n: i32) -> i32 {
    (n - 1) / MACRO_STEP
}

/// Macro nodes a room side of `size` cells spans (5 -> 2, 7 -> 3, 8 -> 3).
fn rect_nodes(size: i32) -> i32 {
    (size + MACRO_STEP) / MACRO_STEP
}

/// Open the 2x2 block of macro node `p`.
fn carve_macro_node(g: &mut LevelGrid, p: IVec2) {
    let base = p * MACRO_STEP + IVec2::ONE;
    for z in 0..2 {
        for x in 0..2 {
            g.set(base + IVec2::new(x, z), Cell::Floor { height: 0.0 });
        }
    }
}

/// Open the 2-cell wide passage from macro node `a` toward `a + o`.
fn carve_macro_link(g: &mut LevelGrid, a: IVec2, o: IVec2) {
    let base = a * MACRO_STEP + IVec2::ONE;
    // Wall line between the nodes, both lanes of the passage.
    let wall = if o.x + o.y > 0 { base + o * 2 } else { base - o.abs() };
    let along = o.perp().abs();
    for k in 0..2 {
        g.set(wall + along * k, Cell::Floor { height: 0.0 });
    }
}

/// A rect side ending mid-node (`size % MACRO_STEP == 1`, e.g. 7) leaves the
/// last node's far half as a 1-cell strip: wall it off, keeping only the
/// cells in front of an open passage (2-cell doorways; the chamfer pass
/// splays their inner corners).
fn seal_rect_strips(g: &mut LevelGrid, min: IVec2, size: IVec2) {
    let max = min + size;
    let (east, south) = (size.x % MACRO_STEP == 1, size.y % MACRO_STEP == 1);
    let mut strip: Vec<(IVec2, bool)> = Vec::new();
    let open = |g: &LevelGrid, c: IVec2| g.in_bounds(c) && !g.cell(c).is_wall();
    if east {
        for z in min.y..max.y + i32::from(south) {
            let c = IVec2::new(max.x, z);
            strip.push((c, open(g, c + IVec2::X) || (south && z == max.y && open(g, c + IVec2::Y))));
        }
    }
    if south {
        for x in min.x..max.x {
            let c = IVec2::new(x, max.y);
            strip.push((c, open(g, c + IVec2::Y)));
        }
    }
    for (c, keep) in strip {
        g.set(c, if keep { Cell::Floor { height: 0.0 } } else { Cell::Wall });
    }
}

/// Flat ground-level floor (corridors, rooms, atrium) a chamfer may extend.
fn is_flat_ground(cell: Cell) -> bool {
    matches!(cell, Cell::Floor { height } if height == 0.0) || matches!(cell, Cell::AtriumFloor)
}

/// One pass (on a snapshot, so it never cascades along a wall) that opens
/// every interior wall cell completing a 2x2 block of flat ground: the
/// inner-corner post of each bend, T-junction and doorway. Players arc
/// diagonally through 3x3 hubs instead of pixel-perfect 90 degree turns.
fn chamfer_inner_corners(g: &mut LevelGrid, posts_only: bool) {
    let snapshot = g.cells.clone();
    let flat = |c: IVec2| is_flat_ground(snapshot[(c.y * g.width + c.x) as usize]);
    let mut open = Vec::new();
    for z in 1..g.depth - 1 {
        for x in 1..g.width - 1 {
            let c = IVec2::new(x, z);
            let post = x % MACRO_STEP == 0 && z % MACRO_STEP == 0;
            if !g.cell(c).is_wall() || (posts_only && !post) {
                continue;
            }
            let corner = [(IVec2::X, IVec2::Y), (IVec2::Y, IVec2::NEG_X), (IVec2::NEG_X, IVec2::NEG_Y), (IVec2::NEG_Y, IVec2::X)]
                .into_iter()
                .any(|(a, b)| flat(c + a) && flat(c + b) && flat(c + a + b));
            if corner {
                open.push(c);
            }
        }
    }
    for c in open {
        g.set(c, Cell::Floor { height: 0.0 });
    }
}

/// Open cells (any non-wall) that are not part of a fully open 2x2 block:
/// 1-cell (4 m) tunnels, pockets and nubs. The wide macro layout has none.
pub fn cramped_cells(g: &LevelGrid) -> Vec<IVec2> {
    let open = |c: IVec2| g.in_bounds(c) && !g.cell(c).is_wall();
    (0..g.width * g.depth)
        .map(|i| IVec2::new(i % g.width, i / g.width))
        .filter(|&c| open(c))
        .filter(|&c| {
            ![IVec2::ZERO, IVec2::NEG_X, IVec2::NEG_Y, IVec2::NEG_ONE].into_iter().any(|o| {
                let b = c + o;
                open(b) && open(b + IVec2::X) && open(b + IVec2::Y) && open(b + IVec2::ONE)
            })
        })
        .collect()
}

/// Lateral clearance (cells) of the open run through `c` across `axis`:
/// the length of the contiguous non-wall line through `c` perpendicular to it.
pub fn cross_width(g: &LevelGrid, c: IVec2, axis: IVec2) -> i32 {
    let side = axis.perp();
    let run = |dir: IVec2| (1..).take_while(|&k| { let p = c + dir * k; g.in_bounds(p) && !g.cell(p).is_wall() }).count() as i32;
    if g.cell(c).is_wall() { 0 } else { 1 + run(side) + run(-side) }
}

/// Demon Jammers on a floor: 3 on Floor 1, +1 per floor, capped at
/// [`MAX_JAMMERS`] so deep Endless floors (whose grid stops growing at Floor 5)
/// can always place them.
pub fn jammer_count_for_floor(floor: u32) -> usize {
    (2 + floor.max(1) as usize).min(MAX_JAMMERS)
}

/// Jammer cap for Endless Overdrive floors.
pub const MAX_JAMMERS: usize = 10;

/// Preferred floor-loot cell inside a non-atrium room: its center when
/// standable, or a guaranteed flat perimeter cell when the center holds lava.
pub fn room_pickup_cell(grid: &LevelGrid, room: &Room) -> Option<IVec2> {
    let center = room.center();
    if matches!(grid.cell(center), Cell::Floor { .. }) && center != grid.spawn && center != grid.tower {
        return Some(center);
    }
    let side = room.min + IVec2::new(0, room.size.y / 2);
    if matches!(grid.cell(side), Cell::Floor { .. }) && side != grid.spawn && side != grid.tower {
        return Some(side);
    }
    for z in 0..room.size.y {
        for x in 0..room.size.x {
            let c = room.min + IVec2::new(x, z);
            if matches!(grid.cell(c), Cell::Floor { .. }) && c != grid.spawn && c != grid.tower {
                return Some(c);
            }
        }
    }
    None
}

/// Generate a fully validated labyrinth. Deterministic for a given `(floor, seed)`.
pub fn generate_level(floor: u32, seed: u64) -> LevelGrid {
    for attempt in 0..48u64 {
        let s = seed.wrapping_add(attempt.wrapping_mul(0x2545_F491_4F6C_DD1D));
        // Later attempts drop vertical room features to guarantee success.
        let features = attempt < 40;
        if let Some(g) = try_generate(floor, s, features, false)
            && g.validate() {
                return g;
            }
    }
    // Deterministic all-connected fallback (no solid-node gaps, relaxed jammer spacing).
    try_generate(floor, seed, false, true).expect("guaranteed connected fallback cannot fail")
}

fn try_generate(floor: u32, seed: u64, features: bool, guaranteed: bool) -> Option<LevelGrid> {
    let mut rng = StdRng::seed_from_u64(seed);
    let n = grid_size_for_floor(floor);
    let mut g = LevelGrid::from_cells(n, n, vec![Cell::Wall; (n * n) as usize]);
    g.floor = floor;
    g.seed = seed;
    let ground = Cell::Floor { height: 0.0 };

    // 1. Perfect maze over a macro grid (iterative backtracker): every macro
    //    node is a 2x2 open block, every link a 2-cell (8 m) wide passage, and
    //    walls stay 1 cell thick on the `x % MACRO_STEP == 0` lines.
    let m = macro_nodes(n);
    let node_idx = |p: IVec2| (p.y * m + p.x) as usize;
    let mut links: Vec<[bool; 4]> = vec![[false; 4]; (m * m) as usize];
    let dir_idx = |o: IVec2| RampDir::ALL.iter().position(|d| d.offset() == o).expect("unit offset");
    let in_macro = |p: IVec2| p.x >= 0 && p.y >= 0 && p.x < m && p.y < m;
    let link = |g: &mut LevelGrid, links: &mut Vec<[bool; 4]>, a: IVec2, o: IVec2| {
        links[node_idx(a)][dir_idx(o)] = true;
        links[node_idx(a + o)][dir_idx(-o)] = true;
        carve_macro_link(g, a, o);
    };
    // Atrium nodes (quadrant opposite the spawn) and the spawn launch lane are
    // picked first; ~1 in 5 other nodes stay solid rock, so the wide
    // corridors wind around chunky wall masses instead of a flat open field.
    let start_node = IVec2::ZERO;
    let lane = if rng.random_bool(0.5) { IVec2::X } else { IVec2::Y };
    let atrium_nodes = rect_nodes(ATRIUM_SIZE);
    let lo = m / 2;
    let hi = (m - atrium_nodes).max(lo);
    let atrium_node = IVec2::new(rng.random_range(lo..=hi), rng.random_range(lo..=hi));
    let mut visited = vec![false; (m * m) as usize];
    for z in 0..m {
        for x in 0..m {
            let p = IVec2::new(x, z);
            let in_atrium = (p - atrium_node).cmpge(IVec2::ZERO).all() && (p - atrium_node).cmplt(IVec2::splat(atrium_nodes)).all();
            let on_lane = (0..SPAWN_LANE_NODES).any(|k| start_node + lane * k == p) || p == start_node + lane.perp().abs();
            // Solid nodes count as visited: the backtracker never enters them.
            // The guaranteed fallback skips solid nodes so the whole macro grid stays connected.
            visited[node_idx(p)] = !guaranteed && !in_atrium && !on_lane && rng.random_bool(SOLID_NODE_CHANCE);
        }
    }
    carve_macro_node(&mut g, start_node);
    visited[node_idx(start_node)] = true;
    let mut stack = vec![start_node];
    while let Some(&c) = stack.last() {
        let mut dirs = RampDir::ALL.map(RampDir::offset);
        dirs.shuffle(&mut rng);
        let next = dirs.into_iter().find(|&o| in_macro(c + o) && !visited[node_idx(c + o)]);
        match next {
            Some(o) => {
                visited[node_idx(c + o)] = true;
                carve_macro_node(&mut g, c + o);
                link(&mut g, &mut links, c, o);
                stack.push(c + o);
            }
            None => {
                stack.pop();
            }
        }
    }

    // 2. Braid: generous extra links so high-speed circuits loop around the
    //    facility (capped so deep Endless floors never overflow random_bool),
    //    then most remaining dead ends get a second exit.
    let carved: Vec<bool> = links.iter().map(|l| l.iter().any(|&b| b)).collect();
    let loop_chance = (0.1 + 0.01 * floor as f64).min(0.3);
    for z in 0..m {
        for x in 0..m {
            let a = IVec2::new(x, z);
            for o in [IVec2::X, IVec2::Y] {
                if in_macro(a + o) && carved[node_idx(a)] && carved[node_idx(a + o)] && !links[node_idx(a)][dir_idx(o)] && rng.random_bool(loop_chance) {
                    link(&mut g, &mut links, a, o);
                }
            }
        }
    }
    for z in 0..m {
        for x in 0..m {
            let a = IVec2::new(x, z);
            if links[node_idx(a)].iter().filter(|&&l| l).count() != 1 || !rng.random_bool(0.5) {
                continue;
            }
            let mut dirs = RampDir::ALL.map(RampDir::offset);
            dirs.shuffle(&mut rng);
            if let Some(o) = dirs.into_iter().find(|&o| in_macro(a + o) && carved[node_idx(a + o)] && !links[node_idx(a)][dir_idx(o)]) {
                link(&mut g, &mut links, a, o);
            }
        }
    }

    // Spawn launch bay: the airlock node opens east and south (the chamfer
    // pass below knocks out the inner corner post => 3x3 bay), and a
    // straight 2-wide lane runs >= 3 nodes (32 m) toward the facility so
    // momentum starts building from frame 1.
    for o in [IVec2::X, IVec2::Y] {
        if !links[node_idx(start_node)][dir_idx(o)] {
            link(&mut g, &mut links, start_node, o);
        }
    }
    for k in 1..SPAWN_LANE_NODES - 1 {
        let a = start_node + lane * k;
        if !links[node_idx(a)][dir_idx(lane)] {
            link(&mut g, &mut links, a, lane);
        }
    }

    // 3. Atrium in the quadrant opposite the spawn (macro-node aligned).
    let amin = atrium_node * MACRO_STEP + IVec2::ONE;
    let atrium = Room { min: amin, size: IVec2::splat(ATRIUM_SIZE), feature: RoomFeature::None };
    for z in 0..ATRIUM_SIZE {
        for x in 0..ATRIUM_SIZE {
            let c = amin + IVec2::new(x, z);
            g.set(c, Cell::AtriumFloor);
            g.set_roof_open(c, true);
        }
    }
    seal_rect_strips(&mut g, amin, atrium.size);
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
    let far = ATRIUM_SIZE - 2;
    for (dx, dz) in [(1, 1), (far, 1), (1, far), (far, far)] {
        let c = amin + IVec2::new(dx, dz);
        if matches!(g.cell(c), Cell::AtriumFloor) {
            g.pillars.push(tower_world_xz(c));
            g.pillar_radii.push(PILLAR_RADIUS);
        }
    }
    g.rooms.push(atrium);

    // 4. Rooms: 5x5 / 5x7 / 7x5 / 7x7, macro-node aligned (so every exit is a
    //    full 2-cell passage), non-overlapping with a 1-cell wall between
    //    footprints, clear of the spawn launch bay.
    let room_target = 3 + floor.clamp(1, 5) as usize / 2;
    let mut tries = 0;
    while g.rooms.len() < room_target + 1 && tries < 200 {
        tries += 1;
        let size = IVec2::new(
            if rng.random_bool(0.5) { 7 } else { 5 },
            if rng.random_bool(0.5) { 7 } else { 5 },
        );
        let span = IVec2::new(rect_nodes(size.x), rect_nodes(size.y));
        if span.x > m || span.y > m {
            continue;
        }
        let node = IVec2::new(rng.random_range(0..=m - span.x), rng.random_range(0..=m - span.y));
        // Keep the spawn airlock / launch bay clear.
        if node.x <= 1 && node.y <= 1 {
            continue;
        }
        let lane_hit = (0..SPAWN_LANE_NODES).any(|k| {
            let p = start_node + lane * k;
            p.x >= node.x && p.y >= node.y && p.x < node.x + span.x && p.y < node.y + span.y
        });
        // Only over nodes the maze carved: every room is wired into the flow.
        let all_carved = (0..span.y).all(|z| (0..span.x).all(|x| carved[node_idx(node + IVec2::new(x, z))]));
        if lane_hit || !all_carved {
            continue;
        }
        let min = node * MACRO_STEP + IVec2::ONE;
        let foot = span * MACRO_STEP - IVec2::ONE;
        let overlaps = g.rooms.iter().any(|r| {
            let rf = IVec2::new(rect_nodes(r.size.x), rect_nodes(r.size.y)) * MACRO_STEP - IVec2::ONE;
            min.x - 1 < r.min.x + rf.x && r.min.x - 1 < min.x + foot.x && min.y - 1 < r.min.y + rf.y && r.min.y - 1 < min.y + foot.y
        });
        if overlaps {
            continue;
        }
        let feature = if !features {
            RoomFeature::None
        } else {
            match rng.random_range(0..4) {
                0 => RoomFeature::None,
                1 => RoomFeature::LavaPit,
                2 => RoomFeature::Pillars,
                _ => RoomFeature::Catwalk,
            }
        };
        let room = Room { min, size, feature };
        for z in 0..size.y {
            for x in 0..size.x {
                g.set(min + IVec2::new(x, z), ground);
            }
        }
        seal_rect_strips(&mut g, min, size);
        apply_room_feature(&mut g, &room, &mut rng);
        g.rooms.push(room);
    }

    // Chamfer: knock out every inner-corner wall nub at bends, T-junctions
    // and room / atrium mouths so turns open into 3x3 hubs.
    // A second, posts-only pass catches wall-line posts whose lanes the first
    // pass splayed (it only touches lattice posts, so it cannot cascade).
    chamfer_inner_corners(&mut g, false);
    chamfer_inner_corners(&mut g, true);

    // 5. Spawn airlock: launch bay corner, facing down the straight lane.
    let start = IVec2::ONE;
    g.spawn = start;
    g.set(start, ground);
    g.spawn_yaw = yaw_facing(lane.as_vec2());

    let dist = g.walk_distances(g.spawn);
    dist[g.idx(g.tower)]?;

    // 6. Demon jammers: reserve up to 2 reachable room pickup cells first so
    //    guaranteed floor loot is never overwritten by a room OR corridor jammer.
    let want = jammer_count_for_floor(floor);
    let mut pickup_rooms: Vec<IVec2> = g
        .rooms
        .iter()
        .skip(1)
        .filter_map(|r| room_pickup_cell(&g, r))
        .filter(|&c| dist[g.idx(c)].is_some())
        .collect();
    pickup_rooms.shuffle(&mut rng);
    let reserved_pickups: Vec<IVec2> = pickup_rooms.into_iter().take(2).collect();

    let mut candidates: Vec<IVec2> = Vec::new();
    for room in g.rooms.iter().skip(1) {
        let spot = match room.feature {
            RoomFeature::Catwalk => catwalk_origin(room) + IVec2::new(2, 0),
            RoomFeature::LavaPit => room.min + IVec2::new(0, room.size.y / 2),
            RoomFeature::Pillars | RoomFeature::None => room.center(),
        };
        if !reserved_pickups.contains(&spot) {
            candidates.push(spot);
        }
    }
    candidates.shuffle(&mut rng);
    let mut corridor: Vec<IVec2> = (0..n * n)
        .map(|i| IVec2::new(i % n, i / n))
        .filter(|&c| matches!(g.cell(c), Cell::Floor { height } if height == 0.0) && !reserved_pickups.contains(&c))
        .collect();
    corridor.shuffle(&mut rng);
    candidates.extend(corridor);
    let min_spawn_dist = 6;
    let spacings: &[i32] = if guaranteed { &[5, 3, 2, 1] } else { &[5] };
    for &min_gap in spacings {
        for &c in &candidates {
            if g.jammers.len() >= want {
                break;
            }
            if g.jammers.contains(&c) || reserved_pickups.contains(&c) {
                continue;
            }
            let Some(d) = dist[g.idx(c)] else { continue };
            let standable = matches!(g.cell(c), Cell::Floor { .. });
            let far_from_others = g.jammers.iter().all(|j| (*j - c).abs().element_sum() >= min_gap);
            let far_from_tower = (g.tower - c).abs().element_sum() >= min_gap.min(5);
            if standable && d >= min_spawn_dist.min((min_gap + 1) as u32) && far_from_others && far_from_tower && !atrium.contains(c) {
                g.jammers.push(c);
            }
        }
        if g.jammers.len() >= want {
            break;
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
                let c = catwalk_origin(&room) + IVec2::new(1, 1);
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
        // Tucked: barrels hug a wall (nudged toward it), never mid-lane.
        let against_wall = RampDir::ALL.map(RampDir::offset).into_iter().any(|o| g.cell(c + o).is_wall());
        if prop_cell_free(g, c) && against_wall && !g.barrels.contains(&c) && !is_chokepoint(g, c) && !near_blocked(g, c) {
            g.barrels.push(c);
        }
    }
}

fn apply_room_feature(g: &mut LevelGrid, room: &Room, rng: &mut StdRng) {
    let m = room.min;
    match room.feature {
        RoomFeature::None => {}
        RoomFeature::Catwalk => {
            // Middle row: ramp (rising east) + two raised catwalk plates, with
            // at least one free floor cell on every side (entries stay open).
            let o = catwalk_origin(room);
            g.set(o, Cell::Ramp { low_height: 0.0, high_height: CATWALK_HEIGHT, dir: RampDir::East });
            g.set(o + IVec2::X, Cell::Floor { height: CATWALK_HEIGHT });
            g.set(o + IVec2::X * 2, Cell::Floor { height: CATWALK_HEIGHT });
        }
        RoomFeature::LavaPit => {
            // Central trench ringed by >= 2 cells of floor: always avoidable.
            let c = room.center();
            g.set(c, Cell::HazardPit);
            if rng.random_bool(0.5) {
                g.set(c + IVec2::X, Cell::HazardPit);
            }
        }
        RoomFeature::Pillars => {
            // Columns on interior grid vertices, inset from the walls so the
            // doorway lanes along every edge stay clear.
            let inset = |s: i32| if s >= 7 { 2 } else { 1 };
            let (ix, iz) = (inset(room.size.x), inset(room.size.y));
            let (fx, fz) = (room.size.x - ix, room.size.y - iz);
            for (dx, dz) in [(ix, iz), (fx, iz), (ix, fz), (fx, fz)] {
                let p = Vec2::new((m.x + dx) as f32 * CELL_SIZE, (m.y + dz) as f32 * CELL_SIZE);
                g.pillars.push(p);
                g.pillar_radii.push(PILLAR_RADIUS);
            }
        }
    }
}

/// West end (the ramp) of a Catwalk room's raised run: centered in the room.
fn catwalk_origin(room: &Room) -> IVec2 {
    room.min + IVec2::new(room.size.x / 2 - 1, room.size.y / 2)
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

/// Concurrent world point-light budget. WebGL2 has no storage buffers, so
/// Bevy's clustered forward path packs lights into small uniform buffers and
/// every lit fragment loops the cluster's lights: the web build keeps only the
/// nearest lit [`CullableLight`]s. Android shares the same budget (tiled
/// mobile GPUs pay per light per fragment and thermally throttle). Desktop
/// stays uncapped.
pub const WEB_POINT_LIGHT_CAP: usize = 12;
/// Light budget of the constrained platforms (web and Android).
pub const MAX_CONSTRAINED_LIGHTS: usize = WEB_POINT_LIGHT_CAP;

/// `constrained` = [`crate::perf::IS_MOBILE_OR_WEB`] at runtime.
pub const fn point_light_cap(constrained: bool) -> Option<usize> {
    if constrained { Some(MAX_CONSTRAINED_LIGHTS) } else { None }
}

/// Override for the concurrent world-light budget (tests / tuning). Absent:
/// [`point_light_cap`] for the current platform.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointLightBudget(pub Option<usize>);

/// Stable distance + tie-breaker key ordering for [`light_cap_key_cutoff`].
fn cmp_light_key<K: Ord>(a: &(f32, K), b: &(f32, K)) -> std::cmp::Ordering {
    a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1))
}

/// Exact `(distance, key)` cutoff keeping strictly at most `cap` nearest lights
/// even when multiple lights are equidistant (`None` = uncapped, `Some(None)` =
/// keep zero). Reorders `entries`.
pub fn light_cap_key_cutoff<K: Copy + Ord>(entries: &mut [(f32, K)], cap: Option<usize>) -> Option<Option<(f32, K)>> {
    match cap {
        None => None,
        Some(0) => Some(None),
        Some(n) if entries.len() > n => Some(Some(*entries.select_nth_unstable_by(n - 1, cmp_light_key).1)),
        Some(_) => None,
    }
}

/// Distance cutoff that keeps the `cap` nearest of `dists` (`INFINITY` when
/// uncapped or within budget). Reorders `dists`.
pub fn light_cap_cutoff(dists: &mut [f32], cap: Option<usize>) -> f32 {
    match cap {
        Some(0) => f32::NEG_INFINITY,
        Some(n) if dists.len() > n => *dists.select_nth_unstable_by(n - 1, f32::total_cmp).1,
        _ => f32::INFINITY,
    }
}

/// Smoothly attenuate and hide distant world lights. Writes are change-guarded
/// so `Changed<Visibility>` / `Changed<PointLight>` only fire when needed. On
/// the web build only the nearest [`WEB_POINT_LIGHT_CAP`] lit lights stay on.
pub fn cull_world_lights(
    players: Query<&Transform, With<crate::player::Player>>,
    mut lights: Query<(Entity, &mut CullableLight, Ref<GlobalTransform>, &mut PointLight, &mut Visibility)>,
    budget: Option<Res<PointLightBudget>>,
    mut scratch: Local<Vec<(f32, Entity)>>,
) {
    let Ok(ptf) = players.single() else { return };
    let eye = ptf.translation + Vec3::Y * crate::player::PLAYER_EYE_HEIGHT;
    let cap = budget.map_or(point_light_cap(crate::perf::IS_MOBILE_OR_WEB), |b| b.0);
    let cutoff = match cap {
        None => None,
        cap => {
            scratch.clear();
            for (entity, mut c, g, light, _) in &mut lights {
                if g.is_added() {
                    continue;
                }
                let base = *c.base_intensity.get_or_insert(light.intensity);
                let dist = g.translation().distance(eye);
                if base > 0.0 && c.fade_factor(dist) > 0.0 {
                    scratch.push((dist, entity));
                }
            }
            light_cap_key_cutoff(&mut scratch, cap)
        }
    };
    for (entity, mut c, g, mut light, mut vis) in &mut lights {
        // Spawned since the last pass: its GlobalTransform may not be
        // propagated yet. When a strict light cap is active (WebGL2), hide
        // newly spawned lights for that first frame so a floor build never
        // exceeds the light budget on frame 1.
        if g.is_added() {
            if cap.is_some() && *vis != Visibility::Hidden {
                let _ = c.base_intensity.get_or_insert(light.intensity);
                *vis = Visibility::Hidden;
                light.intensity = 0.0;
            }
            continue;
        }
        let base = *c.base_intensity.get_or_insert(light.intensity);
        let dist = g.translation().distance(eye);
        let within_cap = match cutoff {
            None => true,
            Some(None) => false,
            Some(Some(limit)) => cmp_light_key(&(dist, entity), &limit).is_le(),
        };
        let fade = if within_cap { c.fade_factor(dist) } else { 0.0 };
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
    fn light_cap_cutoff_keeps_nearest() {
        assert_eq!(point_light_cap(false), None);
        assert_eq!(point_light_cap(true), Some(WEB_POINT_LIGHT_CAP));
        let mut d = [9.0, 1.0, 5.0, 3.0, 7.0];
        assert_eq!(light_cap_cutoff(&mut d, None), f32::INFINITY);
        assert_eq!(light_cap_cutoff(&mut d, Some(5)), f32::INFINITY);
        assert_eq!(light_cap_cutoff(&mut d, Some(2)), 3.0);
        assert_eq!(light_cap_cutoff(&mut d, Some(4)), 7.0);
        assert_eq!(light_cap_cutoff(&mut d, Some(0)), f32::NEG_INFINITY);
        assert_eq!(light_cap_cutoff(&mut [], Some(3)), f32::INFINITY);
    }

    #[test]
    fn web_light_budget_hides_farthest_and_native_stays_uncapped() {
        let run = |budget: Option<PointLightBudget>| {
            let mut world = World::new();
            if let Some(b) = budget {
                world.insert_resource(b);
            }
            let mut sched = Schedule::default();
            sched.add_systems(cull_world_lights);
            world.spawn((crate::player::Player::default(), Transform::default()));
            let lights: Vec<Entity> = (0..5)
                .map(|i| {
                    world
                        .spawn((
                            CullableLight::for_range(7.0),
                            PointLight { intensity: 5_000.0, range: 7.0, ..default() },
                            GlobalTransform::from_xyz(2.0 + i as f32, crate::player::PLAYER_EYE_HEIGHT, 0.0),
                            Visibility::Inherited,
                        ))
                        .id()
                })
                .collect();
            sched.run(&mut world);
            sched.run(&mut world);
            lights.iter().map(|e| *world.entity(*e).get::<Visibility>().unwrap() == Visibility::Inherited).collect::<Vec<_>>()
        };
        assert_eq!(run(Some(PointLightBudget(Some(3)))), [true, true, true, false, false]);
        assert_eq!(run(Some(PointLightBudget(None))), [true; 5]);
        // Without an override the native test build is uncapped.
        assert_eq!(run(None), [true; 5]);
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

    /// Floors / seeds the openness checks sweep (incl. the playtest bot seeds
    /// and deep Endless floors whose grid stops growing at Floor 5).
    fn wide_sweep() -> Vec<(u32, u64)> {
        let mut v = Vec::new();
        for floor in [1u32, 2, 3, 4, 5, 8, 15] {
            for seed in [1234u64, 99, 2026, 7, 31337, 0, 42, 555, 4242, 0xBEEF] {
                v.push((floor, seed));
            }
        }
        v
    }

    #[test]
    fn grid_sizes_follow_the_macro_pitch() {
        assert_eq!((1..=6).map(grid_size_for_floor).collect::<Vec<_>>(), vec![25, 25, 28, 28, 31, 31]);
        for floor in 1..=40 {
            assert_eq!((grid_size_for_floor(floor) - 1) % MACRO_STEP, 0);
        }
        assert_eq!((rect_nodes(5), rect_nodes(7), rect_nodes(ATRIUM_SIZE)), (2, 3, 3));
    }

    #[test]
    fn corridors_are_two_cells_wide_with_no_cramped_tunnels() {
        let (mut open_total, mut cells_total) = (0usize, 0usize);
        for (floor, seed) in wide_sweep() {
            let g = generate_level(floor, seed);
            assert!(g.validate(), "floor {floor} seed {seed}");
            // Every open cell sits in a fully open 2x2 block: no 1-cell (4 m)
            // tunnels, pockets or doorways anywhere.
            let cramped = cramped_cells(&g);
            assert!(cramped.is_empty(), "floor {floor} seed {seed}: cramped cells {cramped:?}");
            // ... so the lateral clearance across either axis is >= 2 cells (8 m).
            for i in 0..g.width * g.depth {
                let c = IVec2::new(i % g.width, i / g.width);
                if !g.cell(c).is_wall() {
                    assert!(cross_width(&g, c, IVec2::X) >= 2 || cross_width(&g, c, IVec2::Y) >= 2, "floor {floor} seed {seed}: {c}");
                    assert!(cross_width(&g, c, IVec2::X).max(cross_width(&g, c, IVec2::Y)) >= 2);
                }
            }
            // Still a facility (corridors around solid masses), not one open field.
            let open = g.cells.iter().filter(|c| !c.is_wall()).count();
            let frac = open as f32 / g.cells.len() as f32;
            assert!((0.4..0.75).contains(&frac), "floor {floor} seed {seed}: open fraction {frac}");
            open_total += open;
            cells_total += g.cells.len();
        }
        let mean = open_total as f32 / cells_total as f32;
        assert!((0.45..0.7).contains(&mean), "mean open fraction {mean}");
    }

    #[test]
    fn bends_and_junctions_open_into_chamfered_hubs() {
        for (floor, seed) in wide_sweep() {
            let g = generate_level(floor, seed);
            let flat = |c: IVec2| is_flat_ground(g.cell(c));
            // No wall-line post is left as an inner-corner nub between two
            // perpendicular flat lanes: every turn is a 3x3 hub.
            let mut hubs = 0;
            for z in (MACRO_STEP..g.depth - 1).step_by(MACRO_STEP as usize) {
                for x in (MACRO_STEP..g.width - 1).step_by(MACRO_STEP as usize) {
                    let c = IVec2::new(x, z);
                    let nub = [(IVec2::X, IVec2::Y), (IVec2::Y, IVec2::NEG_X), (IVec2::NEG_X, IVec2::NEG_Y), (IVec2::NEG_Y, IVec2::X)]
                        .into_iter()
                        .any(|(a, b)| flat(c + a) && flat(c + b) && flat(c + a + b));
                    if g.cell(c).is_wall() {
                        assert!(!nub, "floor {floor} seed {seed}: inner-corner nub left at {c}");
                    } else {
                        hubs += 1;
                    }
                }
            }
            assert!(hubs >= 10, "floor {floor} seed {seed}: only {hubs} open hubs");
        }
    }

    #[test]
    fn spawn_launch_bay_faces_a_long_wide_lane() {
        for (floor, seed) in wide_sweep() {
            let g = generate_level(floor, seed);
            assert_eq!(g.spawn, IVec2::ONE);
            // 3x3 open launch bay around the airlock corner.
            for z in 1..=3 {
                for x in 1..=3 {
                    assert!(is_flat_ground(g.cell(IVec2::new(x, z))), "floor {floor} seed {seed}: bay cell ({x},{z})");
                }
            }
            // Looking down a straight 2-lane corridor >= 8 cells (32 m) long.
            let fwd = Vec2::new(-g.spawn_yaw.sin(), -g.spawn_yaw.cos());
            let dir = IVec2::new(fwd.x.round() as i32, fwd.y.round() as i32);
            assert_eq!(dir.abs().element_sum(), 1, "spawn faces a grid axis");
            let lanes = [g.spawn, g.spawn + dir.perp().abs()];
            for lane in lanes {
                let run = (0..).take_while(|&k| is_flat_ground(g.cell(lane + dir * k))).count() as i32;
                assert!(run >= MACRO_STEP * SPAWN_LANE_NODES - 1, "floor {floor} seed {seed}: lane {lane} runs {run}");
            }
            assert!(!g.is_roof_open(g.spawn));
        }
    }

    #[test]
    fn rooms_are_large_open_and_fully_reachable() {
        let mut sizes = std::collections::HashSet::new();
        let mut features = std::collections::HashSet::new();
        for (floor, seed) in wide_sweep() {
            let g = generate_level(floor, seed);
            let dist = g.walk_distances(g.spawn);
            assert_eq!(g.rooms[0].size, IVec2::splat(ATRIUM_SIZE));
            for room in g.rooms.iter().skip(1) {
                assert!([5, 7].contains(&room.size.x) && [5, 7].contains(&room.size.y), "room {room:?}");
                sizes.insert((room.size.x, room.size.y));
                features.insert(format!("{:?}", room.feature));
                // Feature never blocks the edges: the whole perimeter ring is
                // flat floor, and every flat cell of the room is reachable.
                for z in 0..room.size.y {
                    for x in 0..room.size.x {
                        let c = room.min + IVec2::new(x, z);
                        let edge = x == 0 || z == 0 || x == room.size.x - 1 || z == room.size.y - 1;
                        if edge {
                            assert!(is_flat_ground(g.cell(c)), "floor {floor} seed {seed}: {:?} room edge {c} is {:?}", room.feature, g.cell(c));
                        }
                        if is_flat_ground(g.cell(c)) {
                            assert!(dist[g.idx(c)].is_some(), "floor {floor} seed {seed}: room cell {c} unreachable");
                        }
                    }
                }
                // At least one wide (>= 2 consecutive cells on a single straight side,
                // excluding diagonal corners) mouth into the room.
                let sides: [Vec<IVec2>; 4] = [
                    (0..room.size.x).map(|x| room.min + IVec2::new(x, -1)).collect(),
                    (0..room.size.x).map(|x| room.min + IVec2::new(x, room.size.y)).collect(),
                    (0..room.size.y).map(|z| room.min + IVec2::new(-1, z)).collect(),
                    (0..room.size.y).map(|z| room.min + IVec2::new(room.size.x, z)).collect(),
                ];
                let has_wide_mouth = sides
                    .iter()
                    .any(|side| side.windows(2).any(|pair| !g.cell(pair[0]).is_wall() && !g.cell(pair[1]).is_wall()));
                assert!(has_wide_mouth, "floor {floor} seed {seed}: room {room:?} lacks a >=2-cell contiguous doorway");
            }
        }
        assert_eq!(sizes.len(), 4, "5x5, 5x7, 7x5 and 7x7 rooms all occur: {sizes:?}");
        assert_eq!(features.len(), 4, "all room features occur: {features:?}");
    }

    #[test]
    fn fallback_generation_succeeds_on_floor_1_and_100() {
        for floor in [1u32, 100] {
            for seed in [1u64, 42, 1234] {
                let g = try_generate(floor, seed, false, true).expect("guaranteed fallback must succeed");
                assert!(g.validate(), "floor {floor} seed {seed}: fallback failed validate()");
                assert_eq!(g.jammers.len(), jammer_count_for_floor(floor));
            }
        }
    }

    #[test]
    fn equidistant_lights_never_exceed_web_cap() {
        let mut entries: Vec<(f32, u32)> = (0..13).map(|id| (5.0f32, id)).collect();
        let cutoff = light_cap_key_cutoff(&mut entries, Some(WEB_POINT_LIGHT_CAP)).unwrap().unwrap();
        let kept = entries.iter().filter(|e| cmp_light_key(e, &cutoff).is_le()).count();
        assert_eq!(kept, WEB_POINT_LIGHT_CAP, "strictly {WEB_POINT_LIGHT_CAP} of 13 equidistant lights survive");
    }
}

