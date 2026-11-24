//! Milestone 16: sculpted architectural framing, overhead industrial structure
//! and flush wall-mounted sector props for the Mars bunker.
//!
//! Everything here is pure, deterministic geometry planning over a
//! [`LevelGrid`]: it bakes into three [`MeshBatch`]es (one per material slot)
//! that [`crate::level`] merges into the floor's static per-material batches.
//! No entities, no materials and no collision are created here.
//!
//! Safety envelope (the grid-based physics never sees any of this):
//! - wall-hugging parts (baseboards, crown trim, pilasters, racks, sector
//!   props) stay within [`WALL_TRIM_MAX_DEPTH`] of a wall face, wall props
//!   within [`WALL_PROP_MAX_DEPTH`] -- far inside the 0.4 m player radius;
//! - overhead parts (trusses, conduits, lamp cages) stay above
//!   [`OVERHEAD_MIN_Y`];
//! - catwalk curbs / girders / struts hug the raised slab they trim and skip
//!   every walkable ramp connection.

use std::collections::HashSet;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::math::Affine2;
use bevy::prelude::*;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use crate::assets_gen::FloorTheme;
use crate::level::{
    ATRIUM_WALL_HEIGHT, CATWALK_HEIGHT, CEILING_HEIGHT, CELL_SIZE, Cell, LevelGrid, PILLAR_RADIUS, PIT_DEPTH, RampDir, STEP_HEIGHT, TOWER_MAST_RADIUS,
    slab_top, spawned_wall_top,
};
use crate::mesh_batch::MeshBatch;
use crate::sculpt;

/// Max protrusion of any wall-mounted trim part from its wall face (m).
pub const WALL_TRIM_MAX_DEPTH: f32 = 0.32;
/// Max protrusion of wall props (server racks, sector props) from the wall (m).
pub const WALL_PROP_MAX_DEPTH: f32 = 0.30;
/// Lowest point of any overhead part (trusses, conduits, lamp cages).
pub const OVERHEAD_MIN_Y: f32 = 5.2;
/// Industrial skirting height above the floor.
pub const BASEBOARD_HEIGHT: f32 = 0.28;
/// Crown molding band height below the ceiling.
pub const CROWN_HEIGHT: f32 = 0.30;
/// Bottom of the transverse ceiling I-beams.
pub const TRUSS_BOTTOM: f32 = 5.65;
/// Axis height of the overhead conduit pipes.
pub const CONDUIT_Y: f32 = 5.55;
const CONDUIT_RADIUS: f32 = 0.09;
/// Height of the visual curb along open catwalk drop-offs.
pub const CURB_HEIGHT: f32 = 0.12;
/// Half-width (along the wall) of every wall prop.
pub const WALL_PROP_HALF_WIDTH: f32 = 0.85;
/// Max height of a wall prop above its floor.
pub const WALL_PROP_MAX_HEIGHT: f32 = 3.3;

/// Material slot of an architectural part (mapped to theme materials by the level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchSlot {
    /// Dark gunmetal structural trim (`ThemeMaterials::trim`).
    Trim,
    /// Theme-colored emissive accents (`ThemeMaterials::accent_emissive`).
    Accent,
    /// Dark themed faceplates (`ThemeMaterials::ceiling`).
    Plate,
}

/// Architectural geometry of one floor, one batch per material slot.
#[derive(Debug, Default, Clone)]
pub struct ArchBatches {
    pub trim: MeshBatch,
    pub accent: MeshBatch,
    pub plate: MeshBatch,
}

impl ArchBatches {
    pub fn slot(&mut self, s: ArchSlot) -> &mut MeshBatch {
        match s {
            ArchSlot::Trim => &mut self.trim,
            ArchSlot::Accent => &mut self.accent,
            ArchSlot::Plate => &mut self.plate,
        }
    }

    fn put(&mut self, s: ArchSlot, mesh: &Mesh, frame: &Transform, local: Transform) {
        self.slot(s).append(mesh, &frame.mul_transform(local));
    }
}

/// One open cell's face onto a wall neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WallFace {
    /// The open cell in front of the wall.
    pub cell: IVec2,
    /// Unit grid offset from `cell` toward the wall.
    pub side: IVec2,
}

impl WallFace {
    /// Grid direction along the wall (local +X of [`Self::frame`]).
    pub fn perp(self) -> IVec2 {
        IVec2::new(-self.side.y, self.side.x)
    }

    /// Unit vector out of the wall into the open cell.
    pub fn inward(self) -> Vec3 {
        Vec3::new(-self.side.x as f32, 0.0, -self.side.y as f32)
    }

    /// Middle of the face plane at `y = 0`.
    pub fn anchor(self) -> Vec3 {
        let c = (self.cell.as_vec2() + Vec2::splat(0.5) + self.side.as_vec2() * 0.5) * CELL_SIZE;
        Vec3::new(c.x, 0.0, c.y)
    }

    /// Local frame on the face: origin on the wall plane at height `y`,
    /// +Z out of the wall, +X along [`Self::perp`], +Y up.
    pub fn frame(self, y: f32) -> Transform {
        let n = self.inward();
        Transform::from_translation(self.anchor().with_y(y)).with_rotation(Quat::from_rotation_y(n.x.atan2(n.z)))
    }
}

/// Every face of an open cell onto a wall, in grid order.
pub fn wall_faces(g: &LevelGrid) -> Vec<WallFace> {
    let mut out = Vec::new();
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            if g.cell(c).is_wall() {
                continue;
            }
            for side in RampDir::ALL.map(RampDir::offset) {
                if g.cell(c + side).is_wall() {
                    out.push(WallFace { cell: c, side });
                }
            }
        }
    }
    out
}

/// Floor height a face's trim stands on (`None`: ramps and pits get none).
fn face_floor(g: &LevelGrid, f: WallFace) -> Option<f32> {
    match g.cell(f.cell) {
        Cell::Floor { height } => Some(height),
        Cell::AtriumFloor => Some(0.0),
        _ => None,
    }
}

/// Top of the wall surface a face sees: the ceiling, or the open-roof wall top.
fn face_top(g: &LevelGrid, f: WallFace) -> f32 {
    if g.is_roof_open(f.cell) { spawned_wall_top(g, f.cell + f.side).unwrap_or(ATRIUM_WALL_HEIGHT) } else { CEILING_HEIGHT }
}

// ---------------------------------------------------------------------------
// Wall props
// ---------------------------------------------------------------------------

/// What a wall prop slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallPropKind {
    /// Shared UAC server rack / field terminal (theme-colored status LEDs).
    ServerRack,
    /// The sector's signature accent prop.
    Sector(FloorTheme),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallProp {
    pub face: WallFace,
    pub kind: WallPropKind,
}

/// Wall props wanted on a floor (Endless floors keep the Floor 5 count).
pub fn wall_prop_target(floor: u32) -> usize {
    6 + 2 * floor.clamp(1, 5) as usize
}

/// Whether `face` can host a flush wall prop: a flat ground-level room cell
/// in the middle of an unbroken wall (no corner, no doorway beside it), clear
/// of ramps, pits, raised plates, the spawn airlock, the tower, jammers, jump
/// pads, barrels and room-center pickups.
pub fn wall_prop_slot_ok(g: &LevelGrid, face: WallFace) -> bool {
    let (c, side, perp) = (face.cell, face.side, face.perp());
    let flat = matches!(g.cell(c), Cell::Floor { height } if height == 0.0) || matches!(g.cell(c), Cell::AtriumFloor);
    if !flat || !g.cell(c + side).is_wall() {
        return false;
    }
    // Mid-wall: open floor on both sides along it, unbroken wall behind.
    if g.cell(c + perp).is_wall() || g.cell(c - perp).is_wall() || !g.cell(c + side + perp).is_wall() || !g.cell(c + side - perp).is_wall() {
        return false;
    }
    for dz in -1..=1 {
        for dx in -1..=1 {
            match g.cell(c + IVec2::new(dx, dz)) {
                Cell::Ramp { .. } | Cell::HazardPit => return false,
                Cell::Floor { height } if height > 0.1 => return false,
                _ => {}
            }
        }
    }
    let near = |p: IVec2| (p - c).abs().max_element() <= 1;
    (c - g.spawn).abs().element_sum() >= 3
        && !near(g.tower)
        && !g.jammers.iter().any(|&j| near(j))
        && !g.jump_pads.iter().any(|p| near(p.cell))
        && !g.barrels.iter().any(|&b| near(b))
        && !g.rooms.iter().skip(1).any(|r| r.center() == c)
}

/// Deterministic wall prop placement for a floor (rooms + atrium walls only),
/// alternating the sector prop and the shared server rack, spaced >= 3 cells.
pub fn plan_wall_props(g: &LevelGrid) -> Vec<WallProp> {
    let theme = FloorTheme::for_floor(g.floor);
    let mut cands = Vec::new();
    for room in &g.rooms {
        for z in 0..room.size.y {
            for x in 0..room.size.x {
                let c = room.min + IVec2::new(x, z);
                for side in RampDir::ALL.map(RampDir::offset) {
                    let f = WallFace { cell: c, side };
                    if wall_prop_slot_ok(g, f) {
                        cands.push(f);
                    }
                }
            }
        }
    }
    let mut rng = StdRng::seed_from_u64(g.seed ^ 0x5EC7_0016_A2C4_B00C);
    cands.shuffle(&mut rng);
    let want = wall_prop_target(g.floor);
    let mut out: Vec<WallProp> = Vec::new();
    for face in cands {
        if out.len() >= want {
            break;
        }
        if out.iter().all(|p| (p.face.cell - face.cell).abs().element_sum() >= 3) {
            let kind = if out.len().is_multiple_of(2) { WallPropKind::Sector(theme) } else { WallPropKind::ServerRack };
            out.push(WallProp { face, kind });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Source shapes
// ---------------------------------------------------------------------------

struct ArchMeshes {
    cube: Mesh,
    rib: Mesh,
    plinth: Mesh,
    collar: Mesh,
    capital: Mesh,
    pipe: Mesh,
    flange: Mesh,
    rack: Mesh,
    canister: Mesh,
    canister_cap: Mesh,
    gauge: Mesh,
    thin_pipe: Mesh,
    valve: Mesh,
    housing: Mesh,
    louver_box: Mesh,
    cap_bar: Mesh,
    monolith: Mesh,
    obsidian_core: Mesh,
    plinth_bar: Mesh,
}

impl ArchMeshes {
    fn new() -> Self {
        let r = PILLAR_RADIUS;
        ArchMeshes {
            cube: Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
            // Unit-height pilaster rib (scaled on Y): back face on z = 0.
            rib: sculpt::beveled_box(Vec3::new(0.26, 1.0, 0.14), 0.035),
            plinth: sculpt::tapered_prism(8, r + 0.2, r + 0.1, 0.36, true),
            collar: sculpt::armored_ring(r + 0.03, 0.06, 0.09, 12, true),
            capital: sculpt::tapered_prism(8, r + 0.04, r + 0.2, 0.4, true),
            pipe: sculpt::tapered_prism(8, CONDUIT_RADIUS, CONDUIT_RADIUS, 1.0, false),
            flange: sculpt::tapered_prism(8, CONDUIT_RADIUS + 0.05, CONDUIT_RADIUS + 0.05, 0.1, true),
            rack: sculpt::beveled_box(Vec3::new(1.2, 2.3, 0.24), 0.03),
            canister: sculpt::tapered_prism(10, 0.13, 0.13, 1.5, false),
            canister_cap: sculpt::tapered_prism(10, 0.13, 0.07, 0.14, true),
            gauge: sculpt::tapered_prism(6, 0.025, 0.025, 1.1, false),
            thin_pipe: sculpt::tapered_prism(8, 0.05, 0.05, 1.0, false),
            valve: sculpt::beveled_box(Vec3::splat(0.2), 0.03),
            housing: sculpt::beveled_box(Vec3::new(1.6, 1.15, 0.26), 0.04),
            louver_box: sculpt::beveled_box(Vec3::new(1.5, 1.3, 0.2), 0.035),
            cap_bar: sculpt::beveled_box(Vec3::new(1.6, 0.14, 0.28), 0.03),
            monolith: sculpt::wedge_plate(0.5, 2.9, 0.26, 0.4, 0.5, 0.0),
            obsidian_core: sculpt::beveled_box(Vec3::new(0.42, 2.6, 0.16), 0.04),
            plinth_bar: sculpt::beveled_box(Vec3::new(1.7, 0.2, 0.28), 0.04),
        }
    }
}

fn bx(size: Vec3, at: Vec3) -> Transform {
    Transform::from_translation(at).with_scale(size)
}

fn put_box(b: &mut ArchBatches, s: ArchSlot, m: &ArchMeshes, frame: &Transform, size: Vec3, at: Vec3) {
    b.put(s, &m.cube, frame, bx(size, at));
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Bake every architectural part of a floor into per-slot batches.
pub fn build_architecture(g: &LevelGrid) -> ArchBatches {
    let m = ArchMeshes::new();
    let mut b = ArchBatches::default();
    build_wall_trim(g, &m, &mut b);
    build_overhead(g, &m, &mut b);
    build_lamp_cages(g, &m, &mut b);
    build_pillar_detail(g, &m, &mut b);
    build_catwalk_structure(g, &m, &mut b);
    build_atrium_conduits(g, &m, &mut b);
    for p in plan_wall_props(g) {
        let y = face_floor(g, p.face).unwrap_or(0.0);
        build_wall_prop(&mut b, &m, &p.face.frame(y), p.kind, FloorTheme::for_floor(g.floor));
    }
    b
}

/// Pilaster slots: `(face, end)` with `end` in `{-1, 1}` along [`WallFace::perp`],
/// one per wall vertex and facing (two faces meeting on a flat wall share one).
pub fn pilaster_slots(g: &LevelGrid) -> Vec<(WallFace, i32)> {
    let mut seen: HashSet<(IVec2, IVec2)> = HashSet::new();
    let mut out = Vec::new();
    for f in wall_faces(g) {
        if face_floor(g, f).is_none() {
            continue;
        }
        for s in [-1, 1] {
            // Vertex in half-cell units.
            let vertex = f.cell * 2 + IVec2::ONE + f.side + f.perp() * s;
            if seen.insert((vertex, f.side)) {
                out.push((f, s));
            }
        }
    }
    out
}

/// Baseboards, crown trim / atrium parapets and corner pilasters / relay ribs.
fn build_wall_trim(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    for f in wall_faces(g) {
        let Some(y0) = face_floor(g, f) else { continue };
        let top = face_top(g, f);
        let fr = f.frame(y0);
        let into_wall = Vec3::new(f.side.x as f32, 0.0, f.side.y as f32);
        // Skirting: back faces against the wall and bottoms are never seen.
        let hidden = |n: Vec3| n.dot(into_wall) < 0.5 && n.y > -0.5;
        b.trim.append_filtered(&m.cube, &fr.mul_transform(bx(Vec3::new(CELL_SIZE, 0.22, 0.07), Vec3::new(0.0, 0.11, 0.035))), hidden);
        b.trim.append_filtered(&m.cube, &fr.mul_transform(bx(Vec3::new(CELL_SIZE, 0.06, 0.045), Vec3::new(0.0, 0.25, 0.0225))), hidden);
        let open_roof = g.is_roof_open(f.cell);
        let ft = f.frame(top);
        if open_roof {
            // Heavy roof-edge parapet girder + lower flange, mid-height ledge belt.
            b.trim.append_filtered(&m.cube, &ft.mul_transform(bx(Vec3::new(CELL_SIZE, 0.5, 0.3), Vec3::new(0.0, -0.25, 0.15))), hidden);
            b.trim.append_filtered(&m.cube, &ft.mul_transform(bx(Vec3::new(CELL_SIZE, 0.1, 0.2), Vec3::new(0.0, -0.62, 0.1))), hidden);
            let belt = f.frame(CEILING_HEIGHT);
            b.trim.append_filtered(&m.cube, &belt.mul_transform(bx(Vec3::new(CELL_SIZE, 0.2, 0.14), Vec3::new(0.0, 0.0, 0.07))), hidden);
        } else {
            // Stepped crown molding: CEILING_HEIGHT - 0.30 .. CEILING_HEIGHT.
            b.trim.append_filtered(&m.cube, &ft.mul_transform(bx(Vec3::new(CELL_SIZE, 0.18, 0.16), Vec3::new(0.0, -0.09, 0.08))), hidden);
            b.trim.append_filtered(&m.cube, &ft.mul_transform(bx(Vec3::new(CELL_SIZE, 0.12, 0.09), Vec3::new(0.0, -0.24, 0.045))), hidden);
        }
    }
    for (f, s) in pilaster_slots(g) {
        let y0 = face_floor(g, f).unwrap_or(0.0);
        let open_roof = g.is_roof_open(f.cell);
        let top = face_top(g, f);
        let rib_top = if open_roof { top - 0.75 } else { top - CROWN_HEIGHT };
        let h = rib_top - (y0 + BASEBOARD_HEIGHT);
        if h <= 0.2 {
            continue;
        }
        let into_wall = Vec3::new(f.side.x as f32, 0.0, f.side.y as f32);
        let fr = f.frame(y0);
        let u = s as f32 * CELL_SIZE * 0.5;
        let tf = fr.mul_transform(bx(Vec3::new(1.0, h, 1.0), Vec3::new(u, BASEBOARD_HEIGHT + h * 0.5, 0.07)));
        b.trim.append_filtered(&m.rib, &tf, |n| n.dot(into_wall) < 0.5);
        if open_roof {
            // Tall relay rib: glowing signal seam up its face.
            let seam = h - 1.2;
            put_box(b, ArchSlot::Accent, m, &fr, Vec3::new(0.06, seam, 0.02), Vec3::new(u, BASEBOARD_HEIGHT + 0.6 + seam * 0.5, 0.15));
        }
    }
}

/// Whether `c` is open floor under a ceiling.
fn covered(g: &LevelGrid, c: IVec2) -> bool {
    !g.cell(c).is_wall() && !g.is_roof_open(c)
}

/// Corridor axis of a covered cell: `Some(true)` runs along X, `Some(false)` along Z.
fn corridor_axis(g: &LevelGrid, c: IVec2) -> Option<bool> {
    let open = |o: IVec2| !g.cell(c + o).is_wall();
    let ox = open(IVec2::X) || open(IVec2::NEG_X);
    let oz = open(IVec2::Y) || open(IVec2::NEG_Y);
    match (ox, oz) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// Transverse ceiling I-beams on cell edges + corridor conduit runs with flanges.
fn build_overhead(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    let id = Transform::IDENTITY;
    let fl = 0.06;
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            if !covered(g, c) {
                continue;
            }
            let axis = corridor_axis(g, c);
            let center = Vec3::new((x as f32 + 0.5) * CELL_SIZE, 0.0, (z as f32 + 0.5) * CELL_SIZE);
            // I-beam across the travel direction, on the cell's min edge.
            let along_x_beam = axis == Some(false);
            let prev = if along_x_beam { c - IVec2::Y } else { c - IVec2::X };
            if covered(g, prev) {
                let edge = if along_x_beam { center.with_z(z as f32 * CELL_SIZE) } else { center.with_x(x as f32 * CELL_SIZE) };
                let size = |w: f32, h: f32| if along_x_beam { Vec3::new(CELL_SIZE, h, w) } else { Vec3::new(w, h, CELL_SIZE) };
                put_box(b, ArchSlot::Trim, m, &id, size(0.32, fl), edge.with_y(TRUSS_BOTTOM + fl * 0.5));
                put_box(b, ArchSlot::Trim, m, &id, size(0.06, 0.35 - 2.0 * fl), edge.with_y(TRUSS_BOTTOM + 0.175));
                put_box(b, ArchSlot::Trim, m, &id, size(0.32, fl), edge.with_y(TRUSS_BOTTOM + 0.35 - fl * 0.5));
            }
            // Conduits run along straight corridors, offset toward one wall.
            if let Some(along_x) = axis {
                let (rot, lateral, run) = if along_x {
                    (Quat::from_rotation_z(FRAC_PI_2), Vec3::Z, Vec3::X)
                } else {
                    (Quat::from_rotation_x(FRAC_PI_2), Vec3::X, Vec3::Z)
                };
                let main = center + lateral * 1.25 + Vec3::Y * CONDUIT_Y;
                b.put(ArchSlot::Trim, &m.pipe, &id, Transform::from_translation(main).with_rotation(rot).with_scale(Vec3::new(1.0, CELL_SIZE, 1.0)));
                let thin = center + lateral * 1.55 + Vec3::Y * (CONDUIT_Y + 0.02);
                b.put(ArchSlot::Plate, &m.thin_pipe, &id, Transform::from_translation(thin).with_rotation(rot).with_scale(Vec3::new(0.8, CELL_SIZE, 0.8)));
                // Flange collars: one on the cell seam, one mid-run support.
                for u in [-CELL_SIZE * 0.5, 0.9] {
                    let at = main + run * u;
                    b.put(ArchSlot::Trim, &m.flange, &id, Transform::from_translation(at).with_rotation(rot));
                }
            }
        }
    }
}

/// Dark metal bezel + cage around every ceiling lamp strip.
fn build_lamp_cages(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    for l in &g.lights {
        let f = Transform::from_translation(Vec3::new((l.x as f32 + 0.5) * CELL_SIZE, CEILING_HEIGHT - 0.15, (l.y as f32 + 0.5) * CELL_SIZE));
        // Mounting plate flush under the ceiling, bezel rails, cage bars.
        put_box(b, ArchSlot::Trim, m, &f, Vec3::new(1.24, 0.05, 0.64), Vec3::new(0.0, 0.125, 0.0));
        for s in [-1.0f32, 1.0] {
            put_box(b, ArchSlot::Trim, m, &f, Vec3::new(1.12, 0.18, 0.06), Vec3::new(0.0, 0.0, s * 0.24));
            put_box(b, ArchSlot::Trim, m, &f, Vec3::new(0.06, 0.18, 0.54), Vec3::new(s * 0.56, 0.0, 0.0));
        }
        for k in -1..=1 {
            put_box(b, ArchSlot::Trim, m, &f, Vec3::new(0.04, 0.04, 0.54), Vec3::new(k as f32 * 0.3, -0.11, 0.0));
        }
    }
}

/// Chamfered plinths, collar rings and capitals on every column (not the mast).
fn build_pillar_detail(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    for (p, r) in g.pillars.iter().zip(&g.pillar_radii).skip(1) {
        let c = g.world_to_cell(Vec3::new(p.x, 0.0, p.y));
        let top = if g.is_roof_open(c) { ATRIUM_WALL_HEIGHT - 2.0 } else { CEILING_HEIGHT };
        let base = slab_top(g.cell(c)).unwrap_or(0.0).max(0.0);
        let k = r / PILLAR_RADIUS;
        let at = |y: f32| Transform::from_xyz(p.x, y, p.y).with_rotation(Quat::from_rotation_y(PI / 8.0)).with_scale(Vec3::new(k, 1.0, k));
        b.put(ArchSlot::Trim, &m.plinth, &Transform::IDENTITY, at(base + 0.18));
        let mid = base + (top - base) * 0.45;
        b.put(ArchSlot::Trim, &m.collar, &Transform::IDENTITY, at(mid));
        // Protrude slightly past the armored collar's outer radius (PILLAR_RADIUS + 0.09)
        // on both horizontal axes so the emissive collar band reads from every angle.
        let band = 2.0 * (PILLAR_RADIUS + 0.11);
        b.put(ArchSlot::Accent, &m.cube, &Transform::IDENTITY, at(mid).mul_transform(bx(Vec3::new(band, 0.035, 0.06), Vec3::ZERO)));
        b.put(ArchSlot::Accent, &m.cube, &Transform::IDENTITY, at(mid).mul_transform(bx(Vec3::new(0.06, 0.035, band), Vec3::ZERO)));
        b.put(ArchSlot::Trim, &m.capital, &Transform::IDENTITY, at(top - 0.2));
    }
}

/// Edge girders, X-braced struts and low curbs along exposed raised-plate edges.
fn build_catwalk_structure(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            let Cell::Floor { height: h } = g.cell(c) else { continue };
            if h <= 0.1 {
                continue;
            }
            for o in RampDir::ALL.map(RampDir::offset) {
                let nb = g.cell(c + o);
                if nb.is_wall() {
                    continue;
                }
                // Walkable connection (ramp top, same-height plate): leave open.
                if nb.edge_height(-o).is_some_and(|e| h - e <= STEP_HEIGHT) {
                    continue;
                }
                let base = slab_top(nb).unwrap_or(0.0).max(PIT_DEPTH);
                // Frame on the slab side: +Z out of the plate.
                let face = WallFace { cell: c + o, side: -o };
                let fr = face.frame(0.0);
                put_box(b, ArchSlot::Trim, m, &fr, Vec3::new(CELL_SIZE, 0.32, 0.08), Vec3::new(0.0, h - 0.18, 0.04));
                put_box(b, ArchSlot::Accent, m, &fr, Vec3::new(CELL_SIZE - 0.3, 0.03, 0.02), Vec3::new(0.0, h - 0.06, 0.09));
                let span = (h - 0.34) - base;
                if span > 0.6 {
                    for (u, a) in [(-1.0f32, 0.42f32), (1.0, -0.42)] {
                        let len = span / a.cos();
                        let tf = Transform::from_translation(Vec3::new(u, base + span * 0.5, 0.035))
                            .with_rotation(Quat::from_rotation_z(a))
                            .with_scale(Vec3::new(0.08, len, 0.07));
                        b.put(ArchSlot::Trim, &m.cube, &fr, tf);
                    }
                }
                // Curb on the deck, just inside the edge.
                let deck = WallFace { cell: c, side: o };
                put_box(b, ArchSlot::Trim, m, &deck.frame(h), Vec3::new(CELL_SIZE, CURB_HEIGHT, 0.1), Vec3::new(0.0, CURB_HEIGHT * 0.5, 0.05));
            }
        }
    }
}

/// Armored cable conduits across the tower pedestal toward the mast (diagonals:
/// ramps and atrium jump pads sit on the mid-edges).
fn build_atrium_conduits(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    let t = g.tower_world();
    let deck = CATWALK_HEIGHT;
    for k in [IVec2::new(1, 1), IVec2::new(1, -1), IVec2::new(-1, 1), IVec2::new(-1, -1)] {
        if !matches!(g.cell(g.tower + k), Cell::Floor { height } if (height - deck).abs() < 1e-3) {
            continue;
        }
        let dir = Vec3::new(k.x as f32, 0.0, k.y as f32).normalize();
        let p0 = t.with_y(deck) + Vec3::new(k.x as f32, 0.0, k.y as f32) * (CELL_SIZE * 1.5 - 0.45);
        let p1 = t.with_y(deck) + dir * (TOWER_MAST_RADIUS + 0.05);
        let len = p0.distance(p1);
        let rot = Quat::from_rotation_y(dir.x.atan2(dir.z));
        let mid = (p0 + p1) * 0.5;
        let fr = Transform::from_translation(mid).with_rotation(rot);
        put_box(b, ArchSlot::Trim, m, &fr, Vec3::new(0.26, 0.1, len), Vec3::new(0.0, 0.05, 0.0));
        put_box(b, ArchSlot::Accent, m, &fr, Vec3::new(0.06, 0.02, len - 0.2), Vec3::new(0.0, 0.11, 0.0));
        let clamps = (len / 1.2).floor() as i32;
        for i in 0..=clamps {
            let zc = -len * 0.5 + 0.2 + i as f32 * ((len - 0.4) / clamps.max(1) as f32);
            put_box(b, ArchSlot::Plate, m, &fr, Vec3::new(0.38, 0.14, 0.1), Vec3::new(0.0, 0.07, zc));
        }
    }
}

/// Bake one wall prop in the face frame (`frame` origin on the wall plane at
/// floor height, +Z out of the wall). Every part stays within
/// `|x| <= WALL_PROP_HALF_WIDTH`, `0 <= z <= WALL_PROP_MAX_DEPTH`.
fn build_wall_prop(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, kind: WallPropKind, theme: FloorTheme) {
    use ArchSlot::{Accent, Plate, Trim};
    let at = |p: Vec3| Transform::from_translation(p);
    match kind {
        WallPropKind::ServerRack => {
            b.put(Trim, &m.rack, fr, at(Vec3::new(0.0, 1.15, 0.12)));
            for s in [-1.0f32, 1.0] {
                put_box(b, Trim, m, fr, Vec3::new(0.06, 2.4, 0.28), Vec3::new(s * 0.63, 1.2, 0.14));
            }
            put_box(b, Plate, m, fr, Vec3::new(1.04, 2.05, 0.03), Vec3::new(0.0, 1.17, 0.255));
            for k in 0..7 {
                let y = 0.5 + k as f32 * 0.25;
                put_box(b, Accent, m, fr, Vec3::new(0.46, 0.035, 0.02), Vec3::new(-0.2, y, 0.28));
                put_box(b, Accent, m, fr, Vec3::new(0.06, 0.035, 0.02), Vec3::new(0.34, y, 0.28));
            }
            for k in 0..4 {
                put_box(b, Trim, m, fr, Vec3::new(0.9, 0.03, 0.02), Vec3::new(0.0, 0.18 + k as f32 * 0.06, 0.28));
            }
            if theme == FloorTheme::RustBunker {
                // Heavy amber hazard stripes across the rack head.
                for k in 0..5 {
                    let tf = Transform::from_xyz(-0.4 + k as f32 * 0.2, 2.24, 0.28).with_rotation(Quat::from_rotation_z(0.6)).with_scale(Vec3::new(0.07, 0.2, 0.02));
                    b.put(Accent, &m.cube, fr, tf);
                }
            }
        }
        WallPropKind::Sector(FloorTheme::RustBunker) => {
            // Slatted industrial exhaust louver with amber hazard sill.
            b.put(Trim, &m.louver_box, fr, at(Vec3::new(0.0, 1.55, 0.1)));
            for k in 0..7 {
                let tf = Transform::from_xyz(0.0, 1.03 + k as f32 * 0.16, 0.22).with_rotation(Quat::from_rotation_x(-0.6)).with_scale(Vec3::new(1.36, 0.14, 0.03));
                b.put(Plate, &m.cube, fr, tf);
            }
            put_box(b, Accent, m, fr, Vec3::new(1.5, 0.05, 0.02), Vec3::new(0.0, 0.86, 0.21));
            for k in 0..6u32 {
                let tf = Transform::from_xyz(-0.62 + k as f32 * 0.25, 0.5, 0.05).with_rotation(Quat::from_rotation_z(0.6)).with_scale(Vec3::new(0.08, 0.3, 0.1));
                b.put(if k.is_multiple_of(2) { Accent } else { Trim }, &m.cube, fr, tf);
            }
        }
        WallPropKind::Sector(FloorTheme::ToxicCoolant) => {
            // Pressurized coolant canisters with glowing gauge tubes + pipe junction.
            for s in [0.55f32, 1.55] {
                put_box(b, Trim, m, fr, Vec3::new(1.5, 0.1, 0.05), Vec3::new(0.0, s, 0.025));
            }
            for x in [-0.5f32, 0.0, 0.5] {
                b.put(Plate, &m.canister, fr, at(Vec3::new(x, 1.05, 0.15)));
                b.put(Trim, &m.canister_cap, fr, at(Vec3::new(x, 1.87, 0.15)));
                b.put(Trim, &m.canister_cap, fr, Transform::from_xyz(x, 0.23, 0.15).with_rotation(Quat::from_rotation_x(PI)));
                b.put(Accent, &m.gauge, fr, at(Vec3::new(x, 1.05, 0.27)));
                b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(x, 2.0, 0.1).with_scale(Vec3::new(0.8, 0.18, 0.8)));
            }
            b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(0.0, 2.1, 0.1).with_rotation(Quat::from_rotation_z(FRAC_PI_2)).with_scale(Vec3::new(1.0, 1.3, 1.0)));
            b.put(Trim, &m.valve, fr, at(Vec3::new(0.0, 2.1, 0.12)));
            b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(0.65, 2.7, 0.09).with_scale(Vec3::new(1.0, 1.2, 1.0)));
            b.put(Trim, &m.flange, fr, Transform::from_xyz(0.65, 3.2, 0.09).with_scale(Vec3::new(0.6, 1.0, 0.6)));
        }
        WallPropKind::Sector(FloorTheme::CryoRelay) => {
            // Frosted condenser fins with ice-cyan emitter strips.
            put_box(b, Trim, m, fr, Vec3::new(1.5, 2.5, 0.05), Vec3::new(0.0, 1.35, 0.025));
            for k in 0..7 {
                put_box(b, Plate, m, fr, Vec3::new(0.035, 2.3, 0.22), Vec3::new(-0.6 + k as f32 * 0.2, 1.35, 0.16));
            }
            for x in [-0.5f32, -0.1, 0.1, 0.5] {
                put_box(b, Accent, m, fr, Vec3::new(0.04, 2.1, 0.02), Vec3::new(x, 1.35, 0.21));
            }
            for y in [0.08f32, 2.62] {
                b.put(Trim, &m.cap_bar, fr, at(Vec3::new(0.0, y, 0.14)));
            }
        }
        WallPropKind::Sector(FloorTheme::MagmaFoundry) => {
            // Blast-furnace heat vent: molten slag behind a heavy grille, exhaust stack.
            b.put(Trim, &m.housing, fr, at(Vec3::new(0.0, 0.6, 0.13)));
            put_box(b, Accent, m, fr, Vec3::new(1.3, 0.75, 0.02), Vec3::new(0.0, 0.62, 0.27));
            for k in 0..6 {
                put_box(b, Trim, m, fr, Vec3::new(1.36, 0.04, 0.02), Vec3::new(0.0, 0.3 + k as f32 * 0.13, 0.29));
            }
            for s in [-1.0f32, 1.0] {
                put_box(b, Trim, m, fr, Vec3::new(0.05, 0.8, 0.02), Vec3::new(s * 0.68, 0.62, 0.29));
            }
            put_box(b, Trim, m, fr, Vec3::new(1.7, 0.18, 0.28), Vec3::new(0.0, 1.27, 0.14));
            put_box(b, Trim, m, fr, Vec3::new(0.4, 1.4, 0.18), Vec3::new(0.0, 2.06, 0.09));
            put_box(b, Accent, m, fr, Vec3::new(0.06, 1.1, 0.02), Vec3::new(0.0, 2.06, 0.19));
        }
        WallPropKind::Sector(FloorTheme::ObsidianCore) => {
            // Angular obsidian relay monolith with crimson-violet conduit bars.
            b.put(Trim, &m.plinth_bar, fr, at(Vec3::new(0.0, 0.1, 0.14)));
            for s in [-1.0f32, 1.0] {
                b.put(Trim, &m.monolith, fr, at(Vec3::new(s * 0.55, 1.65, 0.13)));
            }
            b.put(Plate, &m.obsidian_core, fr, at(Vec3::new(0.0, 1.5, 0.08)));
            for s in [-1.0f32, 1.0] {
                put_box(b, Accent, m, fr, Vec3::new(0.05, 2.4, 0.03), Vec3::new(s * 0.25, 1.5, 0.175));
            }
            for y in [0.7f32, 1.5, 2.3] {
                put_box(b, Accent, m, fr, Vec3::new(0.46, 0.04, 0.03), Vec3::new(0.0, y, 0.175));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hazard flow / accent pulse
// ---------------------------------------------------------------------------

/// Per-frame animation state of the active theme's shared hazard + accent materials.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HazardPulse {
    /// Slow diagonal drift of the hazard pit texture.
    pub uv: Affine2,
    /// Multiplier on the hazard pit's base emissive.
    pub lava_gain: f32,
    /// Multiplier on the accent LEDs' base emissive.
    pub accent_gain: f32,
}

/// Seamless loop length of [`hazard_pulse`] (all rates are whole cycles per loop).
pub const HAZARD_LOOP_SECS: f32 = 3600.0;

/// Hazard flow + accent pulse at time `t` (seconds). Always finite, the UV
/// offset stays in `[0, 1)`, and the loop is seamless every [`HAZARD_LOOP_SECS`].
pub fn hazard_pulse(t: f32) -> HazardPulse {
    let t = if t.is_finite() { t.rem_euclid(HAZARD_LOOP_SECS) } else { 0.0 };
    let offset = Vec2::new((t * 0.025).rem_euclid(1.0), (t * 0.015).rem_euclid(1.0));
    HazardPulse {
        uv: Affine2::from_translation(offset),
        lava_gain: 0.86 + 0.14 * (t * TAU / 5.0).sin(),
        accent_gain: 0.8 + 0.2 * (0.5 + 0.5 * (t * TAU / 3.0).sin()),
    }
}

/// `c` with its RGB scaled by `k` (alpha kept).
pub fn scale_rgb(c: LinearRgba, k: f32) -> LinearRgba {
    LinearRgba::new(c.red * k, c.green * k, c.blue * k, c.alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::generate_level;
    use crate::raytracing::{MAX_EMISSIVE_TRIANGLES, rt_mesh_convertible};
    use bevy::mesh::VertexAttributeValues;

    const BOT_SEEDS: [u64; 5] = [1234, 99, 2026, 7, 31337];

    fn positions(mesh: &Mesh) -> Vec<Vec3> {
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(VertexAttributeValues::Float32x3(p)) => p.iter().map(|&v| Vec3::from(v)).collect(),
            _ => Vec::new(),
        }
    }

    fn floors_and_seeds() -> Vec<(u32, u64)> {
        let mut v = Vec::new();
        for floor in [1u32, 2, 3, 4, 5, 7, 12, 25] {
            for seed in BOT_SEEDS.into_iter().chain([0u64, 4242]) {
                v.push((floor, seed));
            }
        }
        v
    }

    #[test]
    fn wall_props_are_flush_and_compact_in_their_face_frame() {
        let m = ArchMeshes::new();
        let kinds = [WallPropKind::ServerRack].into_iter().chain(FloorTheme::ALL.map(WallPropKind::Sector));
        for kind in kinds {
            for theme in FloorTheme::ALL {
                let mut b = ArchBatches::default();
                build_wall_prop(&mut b, &m, &Transform::IDENTITY, kind, theme);
                let mut n = 0;
                for batch in [b.trim, b.accent, b.plate] {
                    let Some(mesh) = batch.build() else { continue };
                    for p in positions(&mesh) {
                        n += 1;
                        assert!(p.z >= -1e-4 && p.z <= WALL_PROP_MAX_DEPTH + 1e-4, "{kind:?}: depth {}", p.z);
                        assert!(p.x.abs() <= WALL_PROP_HALF_WIDTH + 1e-4, "{kind:?}: width {}", p.x);
                        assert!(p.y >= -1e-4 && p.y <= WALL_PROP_MAX_HEIGHT + 1e-4, "{kind:?}: height {}", p.y);
                    }
                }
                assert!(n > 0, "{kind:?} builds geometry");
            }
        }
    }

    #[test]
    fn wall_prop_placement_is_deterministic_and_safe() {
        let mut total = 0;
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let props = plan_wall_props(&g);
            assert_eq!(props, plan_wall_props(&generate_level(floor, seed)), "floor {floor} seed {seed}: deterministic");
            assert!(props.len() <= wall_prop_target(floor));
            total += props.len();
            let theme = FloorTheme::for_floor(floor);
            for (i, p) in props.iter().enumerate() {
                let (c, side, perp) = (p.face.cell, p.face.side, p.face.perp());
                assert!(wall_prop_slot_ok(&g, p.face));
                assert!(g.rooms.iter().any(|r| r.contains(c)), "prop {c} sits in a room / the atrium");
                assert!(g.cell(c + side).is_wall(), "backed by a wall");
                // Not a doorway jamb or a corner.
                assert!(!g.cell(c + perp).is_wall() && !g.cell(c - perp).is_wall());
                assert!(g.cell(c + side + perp).is_wall() && g.cell(c + side - perp).is_wall());
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        assert!(!matches!(g.cell(c + IVec2::new(dx, dz)), Cell::Ramp { .. } | Cell::HazardPit), "prop {c} beside a ramp / pit");
                    }
                }
                assert!((c - g.spawn).abs().element_sum() >= 3, "prop {c} crowds the spawn");
                for q in g.jammers.iter().copied().chain(g.barrels.iter().copied()).chain(g.jump_pads.iter().map(|p| p.cell)) {
                    assert!((q - c).abs().max_element() >= 2, "prop {c} crowds a jammer / barrel / pad at {q}");
                }
                for o in &props[..i] {
                    assert!((o.face.cell - c).abs().element_sum() >= 3, "props spaced");
                }
                let want = if i.is_multiple_of(2) { WallPropKind::Sector(theme) } else { WallPropKind::ServerRack };
                assert_eq!(p.kind, want);
            }
        }
        assert!(total >= floors_and_seeds().len() * 3, "props are actually placed ({total})");
    }

    /// Distance in XZ from `p` to the box of cell `c`.
    fn dist_to_cell(p: Vec3, c: IVec2) -> f32 {
        let lo = c.as_vec2() * CELL_SIZE;
        let hi = lo + Vec2::splat(CELL_SIZE);
        let q = Vec2::new(p.x, p.z);
        (q - q.clamp(lo, hi)).length()
    }

    #[test]
    fn every_low_part_hugs_solid_geometry_and_overhead_stays_high() {
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let b = build_architecture(&g);
            for batch in [b.trim, b.accent, b.plate] {
                let mesh = batch.build().expect("non-empty batch");
                for p in positions(&mesh) {
                    assert!(p.is_finite());
                    if p.y >= OVERHEAD_MIN_Y {
                        continue;
                    }
                    let here = g.world_to_cell(p);
                    let floor_y = slab_top(g.cell(here)).unwrap_or(f32::INFINITY);
                    if p.y <= floor_y + 0.15 {
                        continue; // lying on (or inside) the floor slab
                    }
                    let near_solid = (-1..=1).any(|dz| {
                        (-1..=1).any(|dx| {
                            let n = here + IVec2::new(dx, dz);
                            let solid = g.cell(n).is_wall() || slab_top(g.cell(n)).is_some_and(|t| t >= p.y - CURB_HEIGHT - 0.05);
                            solid && dist_to_cell(p, n) <= WALL_TRIM_MAX_DEPTH + 1e-3
                        })
                    });
                    let near_pillar = g.pillars.iter().zip(&g.pillar_radii).any(|(c, r)| Vec2::new(p.x, p.z).distance(*c) <= r + WALL_TRIM_MAX_DEPTH);
                    assert!(near_solid || near_pillar, "floor {floor} seed {seed}: part vertex {p} floats in walkable space");
                }
            }
        }
    }

    #[test]
    fn overhead_parts_clear_player_and_demon_height() {
        let g = generate_level(3, 2026);
        let m = ArchMeshes::new();
        let mut b = ArchBatches::default();
        build_overhead(&g, &m, &mut b);
        build_lamp_cages(&g, &m, &mut b);
        for batch in [b.trim, b.accent, b.plate] {
            if let Some(mesh) = batch.build() {
                for p in positions(&mesh) {
                    assert!(p.y >= OVERHEAD_MIN_Y && p.y <= CEILING_HEIGHT + 1e-3, "overhead vertex {p}");
                }
            }
        }
    }

    #[test]
    fn catwalk_curbs_never_block_ramp_connections() {
        let g = LevelGrid::from_ascii(&[
            "#######", //
            "#..E^^#", //
            "#...^.#", //
            "#######",
        ]);
        let m = ArchMeshes::new();
        let mut b = ArchBatches::default();
        build_catwalk_structure(&g, &m, &mut b);
        let mesh = b.trim.build().expect("catwalk trim");
        // Ramp top edge: x = 4 * CELL_SIZE (between E at x=3 and ^ at x=4), row z=1.
        let ramp_edge_x = 4.0 * CELL_SIZE;
        for p in positions(&mesh) {
            let on_ramp_edge = (p.x - ramp_edge_x).abs() < 0.2 && p.z > CELL_SIZE && p.z < 2.0 * CELL_SIZE && p.y > CATWALK_HEIGHT;
            assert!(!on_ramp_edge, "curb across the ramp connection at {p}");
        }
        // Exposed drop-offs get curbs: the south edge of (4,2) toward (4,3) is wall, the
        // west edge of (4,2) toward (3,2) is a drop.
        let has_west_curb = positions(&mesh).iter().any(|p| (p.x - 4.0 * CELL_SIZE).abs() < 0.15 && p.z > 2.0 * CELL_SIZE && p.y > CATWALK_HEIGHT + 0.05);
        assert!(has_west_curb, "drop-off edge is curbed");
    }

    #[test]
    fn architecture_batches_are_raytracing_ready_and_emissive_stays_small() {
        let mut worst_accent = 0;
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let b = build_architecture(&g);
            let (t, a, p) = (b.trim.triangle_count(), b.accent.triangle_count(), b.plate.triangle_count());
            assert!(t > 0 && a > 0 && p > 0, "floor {floor} seed {seed}: all slots used");
            worst_accent = worst_accent.max(a);
            assert!(a * 8 < MAX_EMISSIVE_TRIANGLES, "floor {floor} seed {seed}: {a} emissive accent triangles");
            assert!(t < 250_000, "floor {floor}: {t} trim triangles");
            for batch in [b.trim, b.accent, b.plate] {
                let mesh = batch.build().unwrap();
                assert!(rt_mesh_convertible(&mesh));
                assert!(matches!(mesh.indices(), Some(bevy::mesh::Indices::U32(_))));
                assert!(mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0));
                assert_eq!(mesh.attributes().count(), 3, "exactly POSITION, NORMAL, UV_0");
            }
        }
        assert!(worst_accent > 0);
    }

    #[test]
    fn architecture_is_deterministic() {
        for floor in [1u32, 5, 9] {
            let (a, b) = (build_architecture(&generate_level(floor, 31337)), build_architecture(&generate_level(floor, 31337)));
            assert_eq!(
                (a.trim.triangle_count(), a.accent.triangle_count(), a.plate.triangle_count()),
                (b.trim.triangle_count(), b.accent.triangle_count(), b.plate.triangle_count())
            );
            assert_eq!(a.trim.build().unwrap().attribute(Mesh::ATTRIBUTE_POSITION).map(|v| v.len()), b.trim.build().unwrap().attribute(Mesh::ATTRIBUTE_POSITION).map(|v| v.len()));
        }
    }

    #[test]
    fn pilasters_dedupe_per_wall_vertex() {
        let g = LevelGrid::from_ascii(&[
            "#####", //
            "#...#", //
            "#####",
        ]);
        // 3 cells: two long walls with 4 vertices each, two end walls with 2 each.
        let slots = pilaster_slots(&g);
        assert_eq!(slots.len(), 12, "{slots:?}");
        assert_eq!(wall_faces(&g).len(), 8);
        // Ramps and pits get no pilasters / skirting.
        let g = LevelGrid::from_ascii(&["#####", "#E~.#", "#####"]);
        assert!(pilaster_slots(&g).iter().all(|(f, _)| f.cell == IVec2::new(3, 1)));
    }

    #[test]
    fn hazard_pulse_advances_and_stays_finite() {
        let mut prev = hazard_pulse(0.0);
        for k in 1..400 {
            let t = k as f32 * 0.37;
            let p = hazard_pulse(t);
            let off = p.uv.translation;
            assert!(off.is_finite() && off.x >= 0.0 && off.x < 1.0 && off.y >= 0.0 && off.y < 1.0);
            assert!(p.lava_gain.is_finite() && (0.7..=1.0).contains(&p.lava_gain));
            assert!(p.accent_gain.is_finite() && (0.79..=1.01).contains(&p.accent_gain));
            assert_ne!(p.uv, prev.uv, "texture keeps flowing");
            prev = p;
        }
        for t in [f32::NAN, f32::INFINITY, -5.0, 1e9, HAZARD_LOOP_SECS * 3.0 + 0.5] {
            let p = hazard_pulse(t);
            assert!(p.uv.translation.is_finite() && p.lava_gain.is_finite() && p.accent_gain.is_finite(), "t = {t}");
        }
        // Seamless loop.
        let (a, b) = (hazard_pulse(0.25), hazard_pulse(HAZARD_LOOP_SECS + 0.25));
        assert!((a.uv.translation - b.uv.translation).length() < 1e-3 && (a.lava_gain - b.lava_gain).abs() < 1e-3);
        let c = scale_rgb(LinearRgba::new(1.0, 2.0, 3.0, 0.5), 2.0);
        assert_eq!(c, LinearRgba::new(2.0, 4.0, 6.0, 0.5));
    }
}
