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
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

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
/// Lowest point of any overhead part (trusses, conduits, lamp cages); tracks
/// the ceiling so the 7.2 m bays keep their full bhop headroom.
pub const OVERHEAD_MIN_Y: f32 = CEILING_HEIGHT - 0.8;
/// Industrial skirting height above the floor.
pub const BASEBOARD_HEIGHT: f32 = 0.28;
/// Crown molding band height below the ceiling.
pub const CROWN_HEIGHT: f32 = 0.30;
/// Bottom of the transverse ceiling I-beams.
pub const TRUSS_BOTTOM: f32 = CEILING_HEIGHT - 0.35;
/// Axis height of the overhead conduit pipes.
pub const CONDUIT_Y: f32 = CEILING_HEIGHT - 0.45;
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
    /// Glowing green signal screens (`GameAssets::tower_panel`, already a
    /// per-floor batch through the tower panels: no extra draw call).
    Screen,
    /// Warm amber lamp glow (`ThemeMaterials::lamp`, the ceiling lamp batch).
    Lamp,
}

/// Architectural geometry of one floor, one batch per material slot.
#[derive(Debug, Default, Clone)]
pub struct ArchBatches {
    pub trim: MeshBatch,
    pub accent: MeshBatch,
    pub plate: MeshBatch,
    pub screen: MeshBatch,
    pub lamp: MeshBatch,
}

impl ArchBatches {
    pub fn slot(&mut self, s: ArchSlot) -> &mut MeshBatch {
        match s {
            ArchSlot::Trim => &mut self.trim,
            ArchSlot::Accent => &mut self.accent,
            ArchSlot::Plate => &mut self.plate,
            ArchSlot::Screen => &mut self.screen,
            ArchSlot::Lamp => &mut self.lamp,
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
    crt: Mesh,
    cabinet: Mesh,
    dish: Mesh,
    /// Milestone 32: spare antenna coil / cable loop (ring in the XZ plane).
    coil: Mesh,
    /// Milestone 32: cable reel flange disc (axis +Y).
    reel: Mesh,
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
            crt: sculpt::beveled_box(Vec3::new(0.52, 0.44, 0.4), 0.05),
            cabinet: sculpt::beveled_box(Vec3::new(0.72, 2.2, 0.46), 0.035),
            // Shallow parabolic-ish relay dish: wide rim, small hub (axis +Y).
            dish: sculpt::tapered_prism(16, 0.22, 1.7, 0.5, false),
            coil: sculpt::armored_ring(0.13, 0.028, 0.03, 10, true),
            reel: sculpt::tapered_prism(12, 0.26, 0.26, 0.04, true),
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

/// Bake every architectural part of a floor into per-slot batches: the
/// Milestone 16 framing + wall props, then the Milestone 30 wall-flush
/// telecom clutter and the Atrium relay dish crown.
pub fn build_architecture(g: &LevelGrid) -> ArchBatches {
    let m = ArchMeshes::new();
    let mut b = build_architecture_base(g, &m);
    let interactive = plan_interactive_props(g);
    let telecom = plan_telecom_props_with(g, &interactive);
    for p in &telecom {
        let y = face_floor(g, p.face).unwrap_or(0.0);
        build_telecom_prop(&mut b, &m, &p.face.frame(y), p.kind, p.variant);
    }
    // Milestone 32: corner equipment nooks + overhead ducts / cable trays.
    for n in plan_corner_nooks_with(g, &interactive, &telecom) {
        build_corner_nook(&mut b, &m, &n.frame(), n.variant);
    }
    build_ceiling_ducts(g, &m, &mut b);
    build_tower_crown(g, &m, &mut b);
    b
}

/// Milestone 16 framing, overhead structure and flush wall props.
fn build_architecture_base(g: &LevelGrid, m: &ArchMeshes) -> ArchBatches {
    let mut b = ArchBatches::default();
    build_wall_trim(g, m, &mut b);
    build_overhead(g, m, &mut b);
    build_lamp_cages(g, m, &mut b);
    build_pillar_detail(g, m, &mut b);
    build_catwalk_structure(g, m, &mut b);
    build_atrium_conduits(g, m, &mut b);
    for p in plan_wall_props(g) {
        let y = face_floor(g, p.face).unwrap_or(0.0);
        build_wall_prop(&mut b, m, &p.face.frame(y), p.kind, FloorTheme::for_floor(g.floor));
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

/// Corridor axis of a covered cell: `Some(true)` runs along X, `Some(false)`
/// along Z. A lane of a 2-cell wide corridor counts too: open straight
/// through along the axis, walled on one side, its twin lane on the other.
fn corridor_axis(g: &LevelGrid, c: IVec2) -> Option<bool> {
    let open = |o: IVec2| !g.cell(c + o).is_wall();
    let ox = open(IVec2::X) || open(IVec2::NEG_X);
    let oz = open(IVec2::Y) || open(IVec2::NEG_Y);
    match (ox, oz) {
        (true, false) => return Some(true),
        (false, true) => return Some(false),
        _ => {}
    }
    let through_x = open(IVec2::X) && open(IVec2::NEG_X);
    let through_z = open(IVec2::Y) && open(IVec2::NEG_Y);
    let one_wall_z = open(IVec2::Y) != open(IVec2::NEG_Y);
    let one_wall_x = open(IVec2::X) != open(IVec2::NEG_X);
    match (through_x && one_wall_z, through_z && one_wall_x) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// Lateral unit offset (grid) from `c` toward the wall flanking a corridor
/// run along `along_x`, if exactly one side is walled.
fn walled_side(g: &LevelGrid, c: IVec2, along_x: bool) -> Option<IVec2> {
    let lat = if along_x { IVec2::Y } else { IVec2::X };
    match (g.cell(c + lat).is_wall(), g.cell(c - lat).is_wall()) {
        (true, false) => Some(lat),
        (false, true) => Some(-lat),
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
            // Conduits run along straight corridors, hugging the flanking wall
            // (1-wide runs walled on both sides keep the +lateral offset).
            if let Some(along_x) = axis {
                let (rot, lateral, run) = if along_x {
                    (Quat::from_rotation_z(FRAC_PI_2), Vec3::Z, Vec3::X)
                } else {
                    (Quat::from_rotation_x(FRAC_PI_2), Vec3::X, Vec3::Z)
                };
                let lateral = match walled_side(g, c, along_x) {
                    Some(w) => Vec3::new(w.x as f32, 0.0, w.y as f32),
                    None => lateral,
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
// Milestone 30: wall-flush Mars-telecom clutter + Atrium dish crown
// ---------------------------------------------------------------------------

/// Max protrusion of any telecom / wall furniture part from its backing wall
/// face (m): 0.55 m out of a 8 m (2-cell) corridor leaves the lanes wide open.
pub const TELECOM_MAX_DEPTH: f32 = 0.55;
/// Half-width (along the wall) of every telecom prop: clear of the corner
/// pilasters on the cell edges (|x| >= 1.87).
pub const TELECOM_HALF_WIDTH: f32 = 1.6;
/// Max height of a telecom prop above its floor.
pub const TELECOM_MAX_HEIGHT: f32 = 3.4;
/// Height band of the Atrium relay dish crown on the mast.
pub const CROWN_MIN_Y: f32 = 11.0;

/// What a telecom / wall furniture slot holds (Milestone 30 + 32 archetypes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelecomKind {
    /// Wall ledge desk with a crooked, cracked CRT, keyboard slab, tower and
    /// a swivel chair tucked in beside it.
    PcStation,
    /// Tall flush UAC 19-inch server cabinet with LED status strips.
    WallRack,
    /// "NO SIGNAL" RF status light box + floor-edge cable duct.
    NoSignalBox,
    /// Wide dual-monitor dispatch desk: twin angled telemetry screens, sloped
    /// button console, task lamp bar and filing drawers (2.2 m x 0.54 m).
    OpsConsole,
    /// Retro UAC ration / beverage vending machine (0.95 x 2.15 x 0.52 m).
    VendingMachine,
    /// 4-tier industrial steel shelving with crates, coils and canisters
    /// (1.8 x 2.2 x 0.50 m).
    SupplyShelf,
    /// Triple technician locker bay, one door ajar, bench rail in front
    /// (1.65 x 2.15 x 0.48 m, bench <= 0.54 m).
    LockerBank,
    /// Signal-research workbench: light bar, oscilloscope, canisters and a
    /// tool pegboard (2.0 m x 0.54 m).
    LabBench,
}

impl TelecomKind {
    /// Every archetype, in placement cycle order.
    pub const ALL: [TelecomKind; 8] = [
        TelecomKind::PcStation,
        TelecomKind::OpsConsole,
        TelecomKind::WallRack,
        TelecomKind::VendingMachine,
        TelecomKind::SupplyShelf,
        TelecomKind::NoSignalBox,
        TelecomKind::LockerBank,
        TelecomKind::LabBench,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelecomProp {
    pub face: WallFace,
    pub kind: TelecomKind,
    /// Small deterministic look variation (screen color, lean).
    pub variant: u8,
}

/// Telecom / wall furniture pieces wanted on a floor (26..=42; Endless
/// floors keep the Floor 5 count).
pub fn telecom_prop_target(floor: u32) -> usize {
    22 + 4 * floor.clamp(1, 5) as usize
}

/// Min Manhattan spacing (cells) between two static wall furniture pieces.
pub const TELECOM_SPACING: i32 = 2;

/// Whether `face` can host a wall-flush telecom prop: a flat ground-level
/// cell of a room or a corridor at least 2 cells (8 m) wide, mid-way along an
/// unbroken straight wall (no doorway / corner / jamb beside it), clear of
/// ramps, pits, raised plates, the spawn bay, the tower, jammers, jump pads,
/// barrels and room-center pickups.
pub fn telecom_slot_ok(g: &LevelGrid, face: WallFace) -> bool {
    let (c, side, perp) = (face.cell, face.side, face.perp());
    if !matches!(g.cell(c), Cell::Floor { height } if height == 0.0) || !g.cell(c + side).is_wall() {
        return false;
    }
    // Straight wall: open floor both ways along it, unbroken wall behind, and
    // the lane in front stays open (never a 1-cell squeeze).
    if g.cell(c + perp).is_wall()
        || g.cell(c - perp).is_wall()
        || !g.cell(c + side + perp).is_wall()
        || !g.cell(c + side - perp).is_wall()
        || g.cell(c - side).is_wall()
    {
        return false;
    }
    for dz in -1..=1 {
        for dx in -1..=1 {
            match g.cell(c + IVec2::new(dx, dz)) {
                Cell::Ramp { .. } | Cell::HazardPit | Cell::AtriumFloor => return false,
                Cell::Floor { height } if height > 0.1 => return false,
                _ => {}
            }
        }
    }
    let near = |p: IVec2| (p - c).abs().max_element() <= 1;
    (c - g.spawn).abs().element_sum() >= 3
        && (g.tower - c).abs().max_element() >= 3
        && !g.jammers.iter().any(|&j| near(j))
        && !g.jump_pads.iter().any(|p| near(p.cell))
        && !g.barrels.iter().any(|&b| near(b))
        && !g.rooms.iter().any(|r| r.center() == c)
}

/// Deterministic static wall furniture placement (rooms + corridors), spaced
/// at least [`TELECOM_SPACING`] cells apart and 2 cells (Chebyshev) from the
/// Milestone 16 wall props AND the Milestone 31 interactive CRTs / UPS racks
/// (planned first, so denser furniture never crowds them out), cycling the 8
/// [`TelecomKind`] archetypes from a per-floor offset.
pub fn plan_telecom_props(g: &LevelGrid) -> Vec<TelecomProp> {
    let interactive = plan_interactive_props(g);
    plan_telecom_props_with(g, &interactive)
}

/// [`plan_telecom_props`] with the floor's interactive plan already computed.
pub fn plan_telecom_props_with(g: &LevelGrid, interactive: &[InteractiveProp]) -> Vec<TelecomProp> {
    let taken: Vec<IVec2> = plan_wall_props(g).iter().map(|p| p.face.cell).chain(interactive.iter().map(|p| p.face.cell)).collect();
    let mut cands: Vec<WallFace> = wall_faces(g)
        .into_iter()
        .filter(|f| telecom_slot_ok(g, *f) && taken.iter().all(|w| (*w - f.cell).abs().max_element() >= 2))
        .collect();
    let mut rng = StdRng::seed_from_u64(g.seed ^ 0x7E1E_C030_5161_A1ED);
    cands.shuffle(&mut rng);
    // Checkerboard parity first (stable: the shuffle survives within each
    // half): along a straight wall that packs every other cell, and across a
    // 2-cell corridor the two walls stagger into a zig-zag.
    let parity = (g.seed & 1) as i32;
    cands.sort_by_key(|f| (f.cell.x + f.cell.y + parity).rem_euclid(2));
    let want = telecom_prop_target(g.floor);
    let kinds = TelecomKind::ALL;
    let offset = (g.seed % kinds.len() as u64) as usize;
    let mut out: Vec<TelecomProp> = Vec::new();
    for face in cands {
        if out.len() >= want {
            break;
        }
        if out.iter().all(|p| (p.face.cell - face.cell).abs().element_sum() >= TELECOM_SPACING) {
            let i = out.len();
            out.push(TelecomProp { face, kind: kinds[(i + offset) % kinds.len()], variant: ((face.cell.x * 7 + face.cell.y * 13).rem_euclid(3)) as u8 });
        }
    }
    out
}

/// Bake one telecom prop in its face frame (origin on the wall plane at floor
/// height, +Z out of the wall). Every vertex stays within
/// `|x| <= TELECOM_HALF_WIDTH`, `0 <= z <= TELECOM_MAX_DEPTH`,
/// `0 <= y <= TELECOM_MAX_HEIGHT`.
fn build_telecom_prop(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, kind: TelecomKind, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    let glow = if variant == 1 { Lamp } else { Screen };
    match kind {
        TelecomKind::PcStation => {
            // Wall ledge desk on two braces.
            put_box(b, Trim, m, fr, Vec3::new(1.4, 0.06, 0.5), Vec3::new(0.0, 0.78, 0.25));
            put_box(b, Accent, m, fr, Vec3::new(1.3, 0.012, 0.012), Vec3::new(0.0, 0.76, 0.505));
            for s in [-1.0f32, 1.0] {
                put_box(b, Trim, m, fr, Vec3::new(0.06, 0.6, 0.04), Vec3::new(s * 0.6, 0.48, 0.02));
                let brace = Transform::from_xyz(s * 0.6, 0.6, 0.2).with_rotation(Quat::from_rotation_x(-0.7)).with_scale(Vec3::new(0.04, 0.5, 0.04));
                b.put(Trim, &m.cube, fr, brace);
            }
            // Tower case under the desk with a status LED.
            put_box(b, Plate, m, fr, Vec3::new(0.24, 0.5, 0.42), Vec3::new(0.42, 0.26, 0.22));
            put_box(b, Accent, m, fr, Vec3::new(0.03, 0.03, 0.01), Vec3::new(0.42, 0.44, 0.435));
            // Slightly crooked CRT: shell, bezel, glowing screen, crack lines, stand.
            let lean = if variant == 2 { -0.12 } else { 0.12 };
            let crt = fr.mul_transform(Transform::from_xyz(-0.18, 0.81, 0.24).with_rotation(Quat::from_rotation_y(lean) * Quat::from_rotation_z(lean * 0.25)));
            b.put(Trim, &m.cube, &crt, bx(Vec3::new(0.26, 0.04, 0.2), Vec3::new(0.0, 0.02, 0.0)));
            b.put(Trim, &m.crt, &crt, Transform::from_xyz(0.0, 0.26, -0.01));
            b.put(Plate, &m.cube, &crt, bx(Vec3::new(0.46, 0.38, 0.02), Vec3::new(0.0, 0.26, 0.19)));
            b.put(glow, &m.cube, &crt, bx(Vec3::new(0.38, 0.3, 0.012), Vec3::new(0.0, 0.26, 0.203)));
            for (x, a, h) in [(-0.06f32, 0.7f32, 0.26f32), (0.05, -0.45, 0.18), (0.1, 1.1, 0.12)] {
                let crack = Transform::from_xyz(x, 0.27, 0.211).with_rotation(Quat::from_rotation_z(a)).with_scale(Vec3::new(0.008, h, 0.005));
                b.put(Trim, &m.cube, &crt, crack);
            }
            // Tilted keyboard slab with a key-row glow.
            let kb = fr.mul_transform(Transform::from_xyz(-0.12, 0.83, 0.4).with_rotation(Quat::from_rotation_y(-0.08) * Quat::from_rotation_x(0.12)));
            b.put(Trim, &m.cube, &kb, bx(Vec3::new(0.48, 0.025, 0.15), Vec3::ZERO));
            for k in 0..3 {
                b.put(Plate, &m.cube, &kb, bx(Vec3::new(0.42, 0.008, 0.03), Vec3::new(0.0, 0.016, -0.045 + k as f32 * 0.045)));
            }
            // Milestone 32: swivel chair tucked in beside the desk (backrest to the wall).
            let cx = if variant == 2 { -1.12 } else { 1.12 };
            let chair = fr.mul_transform(Transform::from_xyz(cx, 0.0, 0.3).with_rotation(Quat::from_rotation_y(0.2)));
            for a in [FRAC_PI_4, -FRAC_PI_4] {
                b.put(Trim, &m.cube, &chair, Transform::from_xyz(0.0, 0.05, 0.0).with_rotation(Quat::from_rotation_y(a)).with_scale(Vec3::new(0.46, 0.04, 0.05)));
            }
            b.put(Trim, &m.thin_pipe, &chair, Transform::from_xyz(0.0, 0.27, 0.0).with_scale(Vec3::new(0.8, 0.4, 0.8)));
            b.put(Plate, &m.cube, &chair, bx(Vec3::new(0.44, 0.08, 0.4), Vec3::new(0.0, 0.5, 0.0)));
            b.put(Plate, &m.cube, &chair, Transform::from_xyz(0.0, 0.86, -0.17).with_rotation(Quat::from_rotation_x(-0.12)).with_scale(Vec3::new(0.42, 0.56, 0.06)));
            for sx in [-0.2f32, 0.2] {
                put_box(b, Trim, m, &chair, Vec3::new(0.04, 0.04, 0.28), Vec3::new(sx, 0.68, 0.02));
                put_box(b, Trim, m, &chair, Vec3::new(0.03, 0.14, 0.03), Vec3::new(sx, 0.6, 0.06));
            }
        }
        TelecomKind::WallRack => {
            // 19-inch cabinet on a plinth, front door, vent slats, 1U blades.
            put_box(b, Trim, m, fr, Vec3::new(0.78, 0.08, 0.48), Vec3::new(0.0, 0.04, 0.24));
            b.put(Trim, &m.cabinet, fr, Transform::from_xyz(0.0, 1.18, 0.23));
            put_box(b, Plate, m, fr, Vec3::new(0.62, 2.0, 0.02), Vec3::new(0.0, 1.18, 0.47));
            for k in 0..8 {
                put_box(b, Trim, m, fr, Vec3::new(0.5, 0.025, 0.02), Vec3::new(0.0, 1.66 + k as f32 * 0.06, 0.49));
            }
            for k in 0..8 {
                let y = 0.34 + k as f32 * 0.15;
                put_box(b, Trim, m, fr, Vec3::new(0.58, 0.08, 0.02), Vec3::new(0.0, y, 0.49));
                // Status LED strip: red / green / amber (theme accent, screen, lamp).
                for (i, (slot, x)) in [(Accent, -0.22f32), (Screen, 0.17), (Lamp, 0.23)].into_iter().enumerate() {
                    if i > 0 && (k + i).is_multiple_of(4) {
                        continue; // a few dark LEDs read as "blinking"
                    }
                    put_box(b, slot, m, fr, Vec3::new(0.035, 0.025, 0.012), Vec3::new(x, y, 0.506));
                }
            }
            // Cable bundle rising from the cabinet head.
            for x in [-0.18f32, 0.18] {
                b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(x, 2.75, 0.14).with_scale(Vec3::new(0.7, 0.9, 0.7)));
            }
        }
        TelecomKind::NoSignalBox => {
            // RF status light box with a crossed-out signal icon.
            put_box(b, Trim, m, fr, Vec3::new(0.95, 0.42, 0.14), Vec3::new(0.0, 2.55, 0.07));
            put_box(b, Lamp, m, fr, Vec3::new(0.85, 0.32, 0.012), Vec3::new(0.0, 2.55, 0.146));
            for k in 0..4 {
                let h = 0.07 + k as f32 * 0.05;
                put_box(b, Trim, m, fr, Vec3::new(0.06, h, 0.012), Vec3::new(-0.3 + k as f32 * 0.1, 2.43 + h * 0.5, 0.158));
            }
            b.put(Trim, &m.cube, fr, Transform::from_xyz(-0.15, 2.55, 0.165).with_rotation(Quat::from_rotation_z(0.85)).with_scale(Vec3::new(0.03, 0.36, 0.012)));
            put_box(b, Accent, m, fr, Vec3::new(0.24, 0.05, 0.012), Vec3::new(0.24, 2.6, 0.158));
            put_box(b, Accent, m, fr, Vec3::new(0.24, 0.05, 0.012), Vec3::new(0.24, 2.48, 0.158));
            // Conduit down to the floor-edge cable duct.
            b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(0.55, 1.24, 0.06).with_scale(Vec3::new(0.6, 2.2, 0.6)));
            put_box(b, Trim, m, fr, Vec3::new(3.2, 0.12, 0.22), Vec3::new(0.0, 0.06, 0.11));
            put_box(b, Plate, m, fr, Vec3::new(3.2, 0.02, 0.24), Vec3::new(0.0, 0.13, 0.12));
            put_box(b, Accent, m, fr, Vec3::new(3.0, 0.015, 0.01), Vec3::new(0.0, 0.08, 0.225));
        }
        TelecomKind::OpsConsole => build_ops_console(b, m, fr, glow, variant),
        TelecomKind::VendingMachine => build_vending_machine(b, m, fr, glow, variant),
        TelecomKind::SupplyShelf => build_supply_shelf(b, m, fr, variant),
        TelecomKind::LockerBank => build_locker_bank(b, m, fr, variant),
        TelecomKind::LabBench => build_lab_bench(b, m, fr, variant),
    }
}

/// Wide dual-monitor dispatch desk (2.2 m x 0.54 m) in the face frame.
fn build_ops_console(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, glow: ArchSlot, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    // Desk slab + front accent lip.
    put_box(b, Trim, m, fr, Vec3::new(2.2, 0.06, 0.54), Vec3::new(0.0, 0.78, 0.27));
    put_box(b, Accent, m, fr, Vec3::new(2.0, 0.012, 0.012), Vec3::new(0.0, 0.76, 0.53));
    // Side filing drawer pedestals with drawer seams + pulls.
    for sx in [-0.84f32, 0.84] {
        put_box(b, Plate, m, fr, Vec3::new(0.46, 0.72, 0.5), Vec3::new(sx, 0.38, 0.25));
        for k in 0..3 {
            let y = 0.2 + k as f32 * 0.22;
            put_box(b, Trim, m, fr, Vec3::new(0.42, 0.015, 0.01), Vec3::new(sx, y + 0.1, 0.505));
            put_box(b, Trim, m, fr, Vec3::new(0.14, 0.025, 0.02), Vec3::new(sx, y, 0.51));
        }
    }
    // Twin telemetry monitors angled in toward the operator, on wall arms.
    for (sx, yaw) in [(-0.38f32, 0.2f32), (0.38, -0.2)] {
        put_box(b, Trim, m, fr, Vec3::new(0.06, 0.06, 0.1), Vec3::new(sx, 1.2, 0.05));
        let mon = fr.mul_transform(Transform::from_xyz(sx, 1.2, 0.14).with_rotation(Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-0.08)));
        b.put(Trim, &m.cube, &mon, bx(Vec3::new(0.62, 0.42, 0.05), Vec3::ZERO));
        let screen = if sx < 0.0 { glow } else { Screen };
        b.put(screen, &m.cube, &mon, bx(Vec3::new(0.54, 0.34, 0.01), Vec3::new(0.0, 0.0, 0.03)));
        // Telemetry trace lines over the glow.
        for k in 0..2 {
            b.put(Trim, &m.cube, &mon, bx(Vec3::new(0.46, 0.008, 0.004), Vec3::new(0.0, -0.06 + k as f32 * 0.1, 0.037)));
        }
    }
    // Sloped button console on the desk front.
    let deck = fr.mul_transform(Transform::from_xyz(0.0, 0.86, 0.36).with_rotation(Quat::from_rotation_x(-0.45)));
    b.put(Trim, &m.cube, &deck, bx(Vec3::new(1.0, 0.16, 0.04), Vec3::ZERO));
    b.put(Plate, &m.cube, &deck, bx(Vec3::new(0.92, 0.1, 0.01), Vec3::new(0.0, 0.0, 0.024)));
    for k in 0..4 {
        let slot = if (k + variant as usize).is_multiple_of(3) { Lamp } else { Accent };
        b.put(slot, &m.cube, &deck, bx(Vec3::new(0.06, 0.035, 0.012), Vec3::new(-0.3 + k as f32 * 0.2, 0.01, 0.034)));
    }
    // Overhead task lamp bar on two wall brackets.
    put_box(b, Trim, m, fr, Vec3::new(1.9, 0.06, 0.22), Vec3::new(0.0, 1.78, 0.13));
    put_box(b, Lamp, m, fr, Vec3::new(1.7, 0.012, 0.08), Vec3::new(0.0, 1.744, 0.15));
    for sx in [-0.8f32, 0.8] {
        put_box(b, Trim, m, fr, Vec3::new(0.05, 0.3, 0.05), Vec3::new(sx, 1.95, 0.03));
    }
}

/// Retro UAC ration / beverage vending machine (0.95 x 2.15 x 0.52 m).
fn build_vending_machine(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, glow: ArchSlot, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    put_box(b, Trim, m, fr, Vec3::new(0.9, 0.06, 0.48), Vec3::new(0.0, 0.03, 0.24));
    put_box(b, Plate, m, fr, Vec3::new(0.95, 2.09, 0.5), Vec3::new(0.0, 1.105, 0.25));
    // Header with a glowing UAC logo strip.
    put_box(b, Trim, m, fr, Vec3::new(0.95, 0.2, 0.02), Vec3::new(0.0, 2.0, 0.51));
    put_box(b, Accent, m, fr, Vec3::new(0.7, 0.06, 0.01), Vec3::new(0.0, 2.0, 0.522));
    // Display window: frame, glow pane, shelves of colorful cans in front of it.
    let (wx, ww, wh, wy) = (-0.11f32, 0.6f32, 1.12f32, 1.28f32);
    put_box(b, Trim, m, fr, Vec3::new(ww + 0.06, wh + 0.06, 0.01), Vec3::new(wx, wy, 0.503));
    put_box(b, glow, m, fr, Vec3::new(ww, wh, 0.008), Vec3::new(wx, wy, 0.51));
    let can_slots = [Accent, Plate, Lamp, Trim, Screen];
    for row in 0..4 {
        let y = wy - wh * 0.5 + 0.06 + row as f32 * 0.27;
        put_box(b, Trim, m, fr, Vec3::new(ww, 0.02, 0.012), Vec3::new(wx, y, 0.518));
        for k in 0..4 {
            let slot = can_slots[(row + k + variant as usize) % can_slots.len()];
            put_box(b, slot, m, fr, Vec3::new(0.08, 0.13, 0.008), Vec3::new(wx - 0.21 + k as f32 * 0.14, y + 0.08, 0.52));
        }
    }
    // Illuminated keypad strip + coin slot on the right.
    put_box(b, Trim, m, fr, Vec3::new(0.2, 0.52, 0.012), Vec3::new(0.34, 1.45, 0.506));
    put_box(b, Lamp, m, fr, Vec3::new(0.13, 0.32, 0.008), Vec3::new(0.34, 1.5, 0.514));
    for k in 0..3 {
        put_box(b, Trim, m, fr, Vec3::new(0.14, 0.012, 0.004), Vec3::new(0.34, 1.4 + k as f32 * 0.1, 0.52));
    }
    put_box(b, Trim, m, fr, Vec3::new(0.004, 0.32, 0.004), Vec3::new(0.34, 1.5, 0.52));
    put_box(b, Accent, m, fr, Vec3::new(0.08, 0.02, 0.01), Vec3::new(0.34, 1.12, 0.51));
    // Recessed pickup bay: lips around a dark flap.
    put_box(b, Trim, m, fr, Vec3::new(0.62, 0.04, 0.03), Vec3::new(-0.05, 0.5, 0.515));
    put_box(b, Trim, m, fr, Vec3::new(0.62, 0.04, 0.03), Vec3::new(-0.05, 0.26, 0.515));
    for sx in [-0.34f32, 0.24] {
        put_box(b, Trim, m, fr, Vec3::new(0.04, 0.28, 0.03), Vec3::new(sx, 0.38, 0.515));
    }
    b.put(Trim, &m.cube, fr, Transform::from_xyz(-0.05, 0.38, 0.505).with_rotation(Quat::from_rotation_x(0.12)).with_scale(Vec3::new(0.54, 0.18, 0.01)));
}

/// 4-tier industrial steel shelving rack (1.8 x 2.2 x 0.50 m).
fn build_supply_shelf(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    for sx in [-0.87f32, 0.87] {
        for z in [0.03f32, 0.47] {
            put_box(b, Trim, m, fr, Vec3::new(0.05, 2.2, 0.05), Vec3::new(sx, 1.1, z));
        }
    }
    // Rear X brace.
    for a in [0.85f32, -0.85] {
        b.put(Trim, &m.cube, fr, Transform::from_xyz(0.0, 1.1, 0.02).with_rotation(Quat::from_rotation_z(a)).with_scale(Vec3::new(0.03, 2.3, 0.02)));
    }
    let decks = [0.12f32, 0.68, 1.24, 1.8];
    for y in decks {
        put_box(b, Trim, m, fr, Vec3::new(1.78, 0.04, 0.48), Vec3::new(0.0, y, 0.25));
        put_box(b, Plate, m, fr, Vec3::new(1.78, 0.06, 0.02), Vec3::new(0.0, y - 0.01, 0.49));
    }
    put_box(b, Trim, m, fr, Vec3::new(1.8, 0.04, 0.5), Vec3::new(0.0, 2.18, 0.25));
    let shift = if variant == 1 { 0.18 } else { -0.12 };
    // Tier 1: heavy supply crates with hazard stripes.
    let deck = decks[0] + 0.02;
    for (x, w, h) in [(-0.45f32 + shift, 0.6f32, 0.46f32), (0.35 + shift, 0.5, 0.38)] {
        put_box(b, Plate, m, fr, Vec3::new(w, h, 0.42), Vec3::new(x, deck + h * 0.5, 0.25));
        put_box(b, Accent, m, fr, Vec3::new(w * 0.8, 0.03, 0.01), Vec3::new(x, deck + h * 0.75, 0.465));
    }
    // Tier 2: staggered ammo boxes.
    let deck = decks[1] + 0.02;
    for (i, x) in [-0.6f32, -0.17, 0.24, 0.62].into_iter().enumerate() {
        let h = if i % 2 == 0 { 0.26 } else { 0.2 };
        put_box(b, Plate, m, fr, Vec3::new(0.34, h, 0.3), Vec3::new(x - shift * 0.5, deck + h * 0.5, 0.2 + (i % 2) as f32 * 0.08));
        put_box(b, Trim, m, fr, Vec3::new(0.12, 0.03, 0.02), Vec3::new(x - shift * 0.5, deck + h - 0.01, 0.37 + (i % 2) as f32 * 0.08));
    }
    // Tier 3: spare antenna coils stood on edge.
    let deck = decks[2] + 0.02;
    for x in [-0.55f32, -0.22, 0.12] {
        b.put(Trim, &m.coil, fr, Transform::from_xyz(x + shift, deck + 0.16, 0.24).with_rotation(Quat::from_rotation_z(FRAC_PI_2)));
    }
    b.put(Plate, &m.cube, fr, bx(Vec3::new(0.36, 0.3, 0.36), Vec3::new(0.6, deck + 0.15, 0.24)));
    // Tier 4: hazard canisters with glowing bands.
    let deck = decks[3] + 0.02;
    for (i, x) in [-0.6f32, -0.3, 0.0].into_iter().enumerate() {
        let tf = Transform::from_xyz(x - shift, deck + 0.17, 0.24).with_scale(Vec3::new(0.9, 0.22, 0.9));
        b.put(Plate, &m.canister, fr, tf);
        b.put(if i == 1 { Lamp } else { Accent }, &m.cube, fr, bx(Vec3::new(0.24, 0.03, 0.24), Vec3::new(x - shift, deck + 0.24, 0.24)));
    }
    // Status LED tag on the upright.
    put_box(b, Plate, m, fr, Vec3::new(0.14, 0.09, 0.012), Vec3::new(0.78, 1.5, 0.5));
    put_box(b, Screen, m, fr, Vec3::new(0.05, 0.035, 0.008), Vec3::new(0.78, 1.5, 0.509));
}

/// Triple UAC technician locker bay (1.65 x 2.15 x 0.48 m) + bench rail.
fn build_locker_bank(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Trim};
    put_box(b, Trim, m, fr, Vec3::new(1.65, 0.1, 0.4), Vec3::new(0.0, 0.05, 0.2));
    put_box(b, Plate, m, fr, Vec3::new(1.65, 2.0, 0.42), Vec3::new(0.0, 1.1, 0.21));
    put_box(b, Trim, m, fr, Vec3::new(1.65, 0.05, 0.44), Vec3::new(0.0, 2.125, 0.22));
    let ajar = (variant % 3) as usize;
    for i in 0..3usize {
        let x = -0.55 + i as f32 * 0.55;
        // Doors hinge on their left edge; the ajar one swings out ~10 deg.
        let open = if i == ajar { 0.1 } else { 0.0 };
        if i == ajar {
            // Dark interior + hung gear peeking out of the gap.
            put_box(b, Trim, m, fr, Vec3::new(0.48, 1.86, 0.004), Vec3::new(x, 1.1, 0.422));
            put_box(b, Plate, m, fr, Vec3::new(0.22, 0.7, 0.03), Vec3::new(x + 0.1, 1.25, 0.44));
            put_box(b, Lamp, m, fr, Vec3::new(0.12, 0.04, 0.01), Vec3::new(x + 0.1, 1.72, 0.45));
        }
        let door = fr.mul_transform(Transform::from_xyz(x - 0.25, 0.0, 0.42).with_rotation(Quat::from_rotation_y(-open)));
        b.put(Plate, &m.cube, &door, bx(Vec3::new(0.5, 1.9, 0.02), Vec3::new(0.25, 1.1, 0.01)));
        for k in 0..4 {
            b.put(Trim, &m.cube, &door, bx(Vec3::new(0.32, 0.02, 0.008), Vec3::new(0.25, 1.75 + k as f32 * 0.06, 0.024)));
        }
        b.put(Accent, &m.cube, &door, bx(Vec3::new(0.03, 0.12, 0.01), Vec3::new(0.44, 1.1, 0.025)));
        b.put(Trim, &m.cube, &door, bx(Vec3::new(0.18, 0.05, 0.006), Vec3::new(0.25, 1.45, 0.023)));
    }
    // Narrow bench rail tucked flush in front (<= 0.54 m), clear of the ajar door.
    put_box(b, Trim, m, fr, Vec3::new(1.4, 0.05, 0.044), Vec3::new(0.0, 0.45, 0.518));
    for sx in [-0.6f32, 0.6] {
        put_box(b, Trim, m, fr, Vec3::new(0.03, 0.42, 0.03), Vec3::new(sx, 0.21, 0.518));
    }
}

/// Mars signal-research workbench (2.0 m x 0.54 m) + pegboard + light bar.
fn build_lab_bench(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    put_box(b, Trim, m, fr, Vec3::new(2.0, 0.07, 0.54), Vec3::new(0.0, 0.9, 0.27));
    put_box(b, Plate, m, fr, Vec3::new(1.9, 0.03, 0.46), Vec3::new(0.0, 0.25, 0.25));
    for sx in [-0.95f32, 0.95] {
        for z in [0.04f32, 0.5] {
            put_box(b, Trim, m, fr, Vec3::new(0.06, 0.87, 0.06), Vec3::new(sx, 0.435, z));
        }
    }
    // Tool pegboard with hung tools.
    put_box(b, Plate, m, fr, Vec3::new(1.8, 0.9, 0.02), Vec3::new(0.0, 1.62, 0.01));
    for (x, y, a, h) in [(-0.7f32, 1.6f32, 0.3f32, 0.4f32), (-0.45, 1.65, -0.2, 0.32), (0.1, 1.55, 0.0, 0.45), (0.45, 1.7, 0.6, 0.3), (0.7, 1.6, -0.4, 0.36)] {
        b.put(Trim, &m.cube, fr, Transform::from_xyz(x, y, 0.035).with_rotation(Quat::from_rotation_z(a)).with_scale(Vec3::new(0.04, h, 0.03)));
    }
    put_box(b, Accent, m, fr, Vec3::new(0.16, 0.08, 0.03), Vec3::new(0.1, 1.82, 0.035));
    // Overhead light bar.
    put_box(b, Trim, m, fr, Vec3::new(1.9, 0.06, 0.2), Vec3::new(0.0, 2.35, 0.12));
    put_box(b, Lamp, m, fr, Vec3::new(1.7, 0.012, 0.08), Vec3::new(0.0, 2.314, 0.14));
    for sx in [-0.85f32, 0.85] {
        put_box(b, Trim, m, fr, Vec3::new(0.04, 0.3, 0.04), Vec3::new(sx, 2.5, 0.03));
    }
    // Oscilloscope with a glowing trace screen + knobs.
    let ox = if variant == 2 { 0.5 } else { -0.5 };
    put_box(b, Trim, m, fr, Vec3::new(0.44, 0.28, 0.36), Vec3::new(ox, 1.075, 0.24));
    put_box(b, Screen, m, fr, Vec3::new(0.22, 0.16, 0.01), Vec3::new(ox - 0.06, 1.09, 0.424));
    put_box(b, Trim, m, fr, Vec3::new(0.18, 0.008, 0.004), Vec3::new(ox - 0.06, 1.1, 0.431));
    for k in 0..2 {
        put_box(b, Accent, m, fr, Vec3::new(0.04, 0.04, 0.02), Vec3::new(ox + 0.14, 1.04 + k as f32 * 0.09, 0.43));
    }
    // Specimen / battery canisters, one glowing.
    for (i, x) in [0.05f32, 0.22, 0.39].into_iter().enumerate() {
        let x = -ox * 0.2 + x - 0.2;
        b.put(Plate, &m.canister, fr, Transform::from_xyz(x, 1.085, 0.32).with_scale(Vec3::new(0.5, 0.2, 0.5)));
        b.put(if i == 1 { Screen } else { Accent }, &m.cube, fr, bx(Vec3::new(0.1, 0.03, 0.1), Vec3::new(x, 1.1, 0.32)));
        b.put(Trim, &m.canister_cap, fr, Transform::from_xyz(x, 1.24, 0.32).with_scale(Vec3::new(0.5, 0.5, 0.5)));
    }
}

// ---------------------------------------------------------------------------
// Milestone 32: corner equipment nooks + overhead ducts / cable trays
// ---------------------------------------------------------------------------

/// Max reach of a corner nook part from either of its two walls (m).
pub const NOOK_MAX_DEPTH: f32 = 0.72;
/// Max height of a corner nook part above its floor (m).
pub const NOOK_MAX_HEIGHT: f32 = 2.2;
/// Lowest point of any overhead duct / cable tray part (m above floor 0).
pub const DUCT_MIN_Y: f32 = 5.6;
/// Highest point of the duct / tray bodies themselves (hangers reach the ceiling).
pub const DUCT_MAX_Y: f32 = 6.8;

/// A compact equipment cluster tucked into an interior 90 degree room corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CornerNook {
    /// The floor cell of the corner.
    pub cell: IVec2,
    /// Unit grid offsets from `cell` to its two solid wall neighbours.
    pub a: IVec2,
    pub b: IVec2,
    pub variant: u8,
}

impl CornerNook {
    /// The corner point on the floor (where both wall planes meet).
    pub fn corner(&self) -> Vec3 {
        let c = (self.cell.as_vec2() + Vec2::splat(0.5) + (self.a + self.b).as_vec2() * 0.5) * CELL_SIZE;
        Vec3::new(c.x, 0.0, c.y)
    }

    /// Local right-handed frame: origin at the corner, +Y up, +X / +Z out of
    /// the two walls (`a` / `b`, swapped where needed to stay right-handed).
    pub fn frame(&self) -> Transform {
        let u = Vec3::new(-self.a.x as f32, 0.0, -self.a.y as f32);
        let v = Vec3::new(-self.b.x as f32, 0.0, -self.b.y as f32);
        let rot = Quat::from_mat3(&Mat3::from_cols(u, Vec3::Y, v));
        // A left-handed (u, Y, v) basis mirrors: flip to a proper rotation by
        // swapping the roles of the walls (the cluster is symmetric enough).
        let rot = if u.cross(Vec3::Y).dot(v) > 0.0 { rot } else { Quat::from_mat3(&Mat3::from_cols(v, Vec3::Y, u)) };
        Transform::from_translation(self.corner()).with_rotation(rot)
    }
}

/// Corner nooks wanted per floor (7..=12).
pub fn corner_nook_target(floor: u32) -> usize {
    6 + floor.clamp(1, 6) as usize
}

/// Whether `cell` is a clean interior 90 degree corner for a nook: a flat
/// ground-level cell of a room or a >= 2-cell-wide corridor bend / dead end
/// (never the Atrium) with solid walls on two orthogonal sides `a` / `b`
/// that both continue past the corner (no doorway jamb), a 2x2 open block
/// diagonally out from it (never a 1-cell squeeze), clear of ramps, pits,
/// raised plates, the spawn, tower, jammers, jump pads, barrels, pickups and
/// room centers.
pub fn corner_nook_ok(g: &LevelGrid, cell: IVec2, a: IVec2, b: IVec2) -> bool {
    if a.dot(b) != 0 || !matches!(g.cell(cell), Cell::Floor { height } if height == 0.0) || g.is_roof_open(cell) {
        return false;
    }
    let wall = |o: IVec2| g.cell(cell + o).is_wall();
    if !(wall(a) && wall(b) && wall(a + b) && wall(a - b) && wall(b - a)) || wall(-a) || wall(-b) || wall(-a - b) {
        return false;
    }
    if g.rooms.first().is_some_and(|r| r.contains(cell)) || g.rooms.iter().any(|r| r.center() == cell) {
        return false;
    }
    for dz in -1..=1 {
        for dx in -1..=1 {
            match g.cell(cell + IVec2::new(dx, dz)) {
                Cell::Ramp { .. } | Cell::HazardPit | Cell::AtriumFloor => return false,
                Cell::Floor { height } if height > 0.1 => return false,
                _ => {}
            }
        }
    }
    let far = |p: IVec2| (p - cell).abs().max_element() >= 2;
    (cell - g.spawn).abs().element_sum() >= 3
        && (g.tower - cell).abs().max_element() >= 3
        && g.jammers.iter().all(|&j| far(j))
        && g.jump_pads.iter().all(|p| far(p.cell))
        && g.barrels.iter().all(|&b| far(b))
        && g.rooms.iter().skip(1).filter_map(|r| crate::level::room_pickup_cell(g, r)).all(far)
}

/// Deterministic corner nook placement (capped by [`corner_nook_target`]),
/// clear of the interactive CRTs / UPS racks (>= 2 cells) and of every
/// static wall prop / furniture cell.
pub fn plan_corner_nooks(g: &LevelGrid) -> Vec<CornerNook> {
    let interactive = plan_interactive_props(g);
    let telecom = plan_telecom_props_with(g, &interactive);
    plan_corner_nooks_with(g, &interactive, &telecom)
}

fn plan_corner_nooks_with(g: &LevelGrid, interactive: &[InteractiveProp], telecom: &[TelecomProp]) -> Vec<CornerNook> {
    let statics: Vec<IVec2> = plan_wall_props(g).iter().map(|p| p.face.cell).chain(telecom.iter().map(|p| p.face.cell)).collect();
    let mut cands = Vec::new();
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            for (a, b) in [(IVec2::X, IVec2::Y), (IVec2::Y, IVec2::NEG_X), (IVec2::NEG_X, IVec2::NEG_Y), (IVec2::NEG_Y, IVec2::X)] {
                if corner_nook_ok(g, c, a, b) && interactive.iter().all(|p| (p.face.cell - c).abs().max_element() >= 2) && !statics.contains(&c) {
                    cands.push((c, a, b));
                }
            }
        }
    }
    let mut rng = StdRng::seed_from_u64(g.seed ^ 0xC0E2_4E00_0032_B0C5);
    cands.shuffle(&mut rng);
    // Room corners first (furnished rooms), then corridor bends / dead ends.
    cands.sort_by_key(|(c, _, _)| !g.rooms.iter().skip(1).any(|r| r.contains(*c)));
    let want = corner_nook_target(g.floor);
    let mut out: Vec<CornerNook> = Vec::new();
    for (cell, a, b) in cands {
        if out.len() >= want {
            break;
        }
        if out.iter().all(|n| n.cell != cell) {
            out.push(CornerNook { cell, a, b, variant: ((cell.x * 5 + cell.y * 3).rem_euclid(3)) as u8 });
        }
    }
    out
}

/// Bake one corner nook in its frame (origin at the corner, +X / +Z out of
/// the two walls): stacked crates, a wall-hung cable reel and a diagnostic
/// beacon, every vertex within `0..=NOOK_MAX_DEPTH` of both walls.
fn build_corner_nook(b: &mut ArchBatches, m: &ArchMeshes, fr: &Transform, variant: u8) {
    use ArchSlot::{Accent, Lamp, Plate, Screen, Trim};
    // Stacked metal crates (clear of the corner pilasters, < 0.16 m).
    put_box(b, Trim, m, fr, Vec3::new(0.5, 0.06, 0.5), Vec3::new(0.44, 0.03, 0.44));
    put_box(b, Plate, m, fr, Vec3::new(0.48, 0.42, 0.48), Vec3::new(0.44, 0.27, 0.44));
    put_box(b, Accent, m, fr, Vec3::new(0.4, 0.03, 0.01), Vec3::new(0.44, 0.36, 0.685));
    put_box(b, Trim, m, fr, Vec3::new(0.5, 0.03, 0.5), Vec3::new(0.44, 0.49, 0.44));
    let top = fr.mul_transform(Transform::from_xyz(0.44, 0.67, 0.44).with_rotation(Quat::from_rotation_y(0.3 + variant as f32 * 0.1)));
    b.put(Plate, &m.cube, &top, bx(Vec3::new(0.34, 0.32, 0.34), Vec3::ZERO));
    b.put(Trim, &m.cube, &top, bx(Vec3::new(0.36, 0.03, 0.08), Vec3::new(0.0, 0.17, 0.0)));
    // Coiled cable reel hung flat on wall `a`.
    let reel = fr.mul_transform(Transform::from_xyz(0.12, 1.35, 0.42).with_rotation(Quat::from_rotation_z(FRAC_PI_2)));
    for y in [-0.08f32, 0.08] {
        b.put(Trim, &m.reel, &reel, Transform::from_xyz(0.0, y, 0.0));
    }
    b.put(Plate, &m.coil, &reel, Transform::IDENTITY.with_scale(Vec3::new(1.1, 2.4, 1.1)));
    put_box(b, Trim, m, fr, Vec3::new(0.06, 0.08, 0.08), Vec3::new(0.03, 1.35, 0.42));
    // Diagnostic beacon on wall `b`, blinking.
    put_box(b, Trim, m, fr, Vec3::new(0.16, 0.22, 0.12), Vec3::new(0.42, 1.95, 0.06));
    put_box(b, if variant == 1 { Screen } else { Lamp }, m, fr, Vec3::new(0.1, 0.08, 0.02), Vec3::new(0.42, 1.98, 0.13));
    // Conduit from the beacon down to the crate stack.
    b.put(Trim, &m.thin_pipe, fr, Transform::from_xyz(0.58, 1.3, 0.05).with_scale(Vec3::new(0.5, 1.1, 0.5)));
}

/// Floor cells an overhead duct run may hang over: covered (closed roof),
/// flat ground level (no raised plates / ramps underneath).
fn duct_cell_ok(g: &LevelGrid, c: IVec2) -> bool {
    let flat = matches!(g.cell(c), Cell::Floor { height } if height <= 0.1) || matches!(g.cell(c), Cell::HazardPit);
    flat && covered(g, c)
        && !g.jammers.contains(&c)
        && !g.pillars.iter().any(|p| g.world_to_cell(Vec3::new(p.x, 0.0, p.y)) == c)
}

/// Overhead HVAC ducts + cable trays: along every straight corridor lane and
/// down the long axis of every closed-roof room, all between
/// [`DUCT_MIN_Y`] and the ceiling (hangers), never over open-roof cells.
fn build_ceiling_ducts(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    let mut runs: Vec<(IVec2, bool, IVec2)> = Vec::new();
    for z in 0..g.depth {
        for x in 0..g.width {
            let c = IVec2::new(x, z);
            if !duct_cell_ok(g, c) {
                continue;
            }
            if let Some(along_x) = corridor_axis(g, c) {
                let lat = walled_side(g, c, along_x).unwrap_or(if along_x { IVec2::Y } else { IVec2::X });
                runs.push((c, along_x, lat));
            }
        }
    }
    for room in g.rooms.iter().skip(1) {
        let along_x = room.size.x >= room.size.y;
        let mid = room.min + room.size / 2;
        let n = if along_x { room.size.x } else { room.size.y };
        for k in 0..n {
            let c = if along_x { IVec2::new(room.min.x + k, mid.y) } else { IVec2::new(mid.x, room.min.y + k) };
            if duct_cell_ok(g, c) && corridor_axis(g, c).is_none() {
                runs.push((c, along_x, if along_x { IVec2::Y } else { IVec2::X }));
            }
        }
    }
    // Tops of the duct bodies face the ceiling: never seen, skip them.
    let below = |n: Vec3| n.y < 0.5;
    for (c, along_x, lat) in runs {
        let center = g.cell_center(c).with_y(0.0);
        let lat = Vec3::new(lat.x as f32, 0.0, lat.y as f32);
        debug_assert!(lat.dot(if along_x { Vec3::Z } else { Vec3::X }).abs() > 0.5);
        // Local frame: +X along the run (either way: the run is symmetric),
        // +Y up, +Z toward the walled side (right-handed).
        let fr = Transform::from_translation(center).with_rotation(Quat::from_mat3(&Mat3::from_cols(Vec3::Y.cross(lat), Vec3::Y, lat)));
        let mut put = |slot: ArchSlot, size: Vec3, at: Vec3, filter: bool| {
            let tf = fr.mul_transform(bx(size, at));
            if filter { b.slot(slot).append_filtered(&m.cube, &tf, below) } else { b.slot(slot).append(&m.cube, &tf) }
        };
        let half = CELL_SIZE * 0.5;
        // HVAC duct on the open side, its hangers and joint straps.
        put(ArchSlot::Plate, Vec3::new(CELL_SIZE, 0.46, 0.72), Vec3::new(0.0, 6.2, -1.0), true);
        put(ArchSlot::Trim, Vec3::new(0.1, 0.52, 0.78), Vec3::new(-half + 0.12, 6.2, -1.0), true);
        // Strap + hangers sit 1.3 m along the run: clear of the ceiling lamp
        // cages on the cell center and the I-beams on the cell edge.
        put(ArchSlot::Trim, Vec3::new(0.1, 0.52, 0.78), Vec3::new(1.3, 6.2, -1.0), true);
        for zz in [-1.36f32, -0.64] {
            put(ArchSlot::Trim, Vec3::new(0.04, CEILING_HEIGHT - 6.43, 0.04), Vec3::new(1.3, (CEILING_HEIGHT + 6.43) * 0.5, zz), false);
        }
        // Louvered diffuser grille on the underside.
        put(ArchSlot::Trim, Vec3::new(0.9, 0.02, 0.5), Vec3::new(-0.9, 5.96, -1.0), false);
        // Cable tray toward the wall, under the M16 conduit.
        put(ArchSlot::Trim, Vec3::new(CELL_SIZE, 0.03, 0.42), Vec3::new(0.0, 5.76, 1.48), false);
        for zz in [1.27f32, 1.69] {
            put(ArchSlot::Trim, Vec3::new(CELL_SIZE, 0.12, 0.025), Vec3::new(0.0, 5.82, zz), false);
        }
        b.put(ArchSlot::Plate, &m.thin_pipe, &fr, Transform::from_xyz(0.0, 5.83, 1.42).with_rotation(Quat::from_rotation_z(FRAC_PI_2)).with_scale(Vec3::new(0.9, CELL_SIZE, 0.9)));
        b.put(ArchSlot::Plate, &m.thin_pipe, &fr, Transform::from_xyz(0.0, 5.82, 1.56).with_rotation(Quat::from_rotation_z(FRAC_PI_2)).with_scale(Vec3::new(0.7, CELL_SIZE, 0.7)));
        let mut put = |slot: ArchSlot, size: Vec3, at: Vec3| b.put(slot, &m.cube, &fr, bx(size, at));
        put(ArchSlot::Trim, Vec3::new(0.04, CEILING_HEIGHT - 5.85, 0.04), Vec3::new(-1.2, (CEILING_HEIGHT + 5.85) * 0.5, 1.72));
        // Subtle status LED on every fourth tray segment (keeps Solari RT emissive budget roomy).
        if (c.x + c.y).rem_euclid(4) == 0 {
            put(ArchSlot::Accent, Vec3::new(0.06, 0.03, 0.01), Vec3::new(0.9, 5.82, 1.25));
        }
    }
}

/// Bold angled relay dish + twin cross antennas near the top of the open-roof
/// Atrium tower mast (all above [`CROWN_MIN_Y`]).
fn build_tower_crown(g: &LevelGrid, m: &ArchMeshes, b: &mut ArchBatches) {
    if !g.is_roof_open(g.tower) {
        return;
    }
    let base = g.tower_world();
    let to_spawn = g.spawn_world() - base;
    let out = Vec3::new(to_spawn.x, 0.0, to_spawn.z).normalize_or(Vec3::X);
    let side = Vec3::Y.cross(out).normalize_or(Vec3::Z);
    let yaw = Quat::from_rotation_y(out.x.atan2(out.z));
    let id = Transform::IDENTITY;
    // Mounting collar + boom arm, sized to the rendered mast cylinder (`TOWER_MAST_RADIUS * 0.8`).
    let mast_r = TOWER_MAST_RADIUS * 0.8;
    let collar_y = base.y + 12.2;
    for k in 0..4 {
        let a = Quat::from_rotation_y(k as f32 * FRAC_PI_2);
        b.put(
            ArchSlot::Trim,
            &m.cube,
            &id,
            Transform::from_translation(base.with_y(collar_y) + a * Vec3::Z * (mast_r + 0.04))
                .with_rotation(a)
                .with_scale(Vec3::new(mast_r * 2.0 + 0.24, 0.35, 0.16)),
        );
    }
    let hub = base.with_y(collar_y + 0.4) + out * (mast_r + 1.3);
    let arm_mid = base.with_y(collar_y + 0.2) + out * (mast_r + 0.6);
    b.put(ArchSlot::Trim, &m.cube, &id, Transform::from_translation(arm_mid).with_rotation(yaw).with_scale(Vec3::new(0.22, 0.22, 1.4)));
    // Dish aimed out and up (35 deg), feed boom along its axis + glowing horn.
    let axis = (out * 0.82 + Vec3::Y * 0.57).normalize();
    let dish_rot = Quat::from_rotation_arc(Vec3::Y, axis);
    b.put(ArchSlot::Trim, &m.dish, &id, Transform::from_translation(hub + axis * 0.25).with_rotation(dish_rot));
    b.put(ArchSlot::Accent, &m.cube, &id, Transform::from_translation(hub + axis * 0.52).with_rotation(dish_rot).with_scale(Vec3::new(3.3, 0.02, 0.05)));
    b.put(ArchSlot::Trim, &m.thin_pipe, &id, Transform::from_translation(hub + axis * 1.05).with_rotation(dish_rot).with_scale(Vec3::new(0.7, 1.2, 0.7)));
    b.put(ArchSlot::Accent, &m.cube, &id, Transform::from_translation(hub + axis * 1.7).with_rotation(dish_rot).with_scale(Vec3::splat(0.2)));
    // Twin cross antennas on either flank of the mast.
    for s in [-1.0f32, 1.0] {
        let foot = base.with_y(collar_y - 0.6) + side * s * (mast_r + 0.45);
        b.put(ArchSlot::Trim, &m.thin_pipe, &id, Transform::from_translation(foot + Vec3::Y * 1.9).with_scale(Vec3::new(0.8, 3.8, 0.8)));
        b.put(
            ArchSlot::Trim,
            &m.cube,
            &id,
            Transform::from_translation(base.with_y(collar_y - 0.4) + side * s * (mast_r + 0.22))
                .with_rotation(yaw)
                .with_scale(Vec3::new(0.65, 0.14, 0.22)),
        );
        for (k, w) in [(0, 1.4f32), (1, 1.0), (2, 0.6)] {
            let y = 2.0 + k as f32 * 0.7;
            b.put(ArchSlot::Trim, &m.cube, &id, Transform::from_translation(foot + Vec3::Y * y).with_rotation(yaw).with_scale(Vec3::new(w, 0.06, 0.06)));
            b.put(ArchSlot::Trim, &m.cube, &id, Transform::from_translation(foot + Vec3::Y * y).with_rotation(yaw).with_scale(Vec3::new(0.06, 0.06, w)));
        }
        b.put(ArchSlot::Accent, &m.cube, &id, Transform::from_translation(foot + Vec3::Y * 3.85).with_scale(Vec3::splat(0.16)));
    }
}

// ---------------------------------------------------------------------------
// Milestone 31: interactive (shootable) wall props
// ---------------------------------------------------------------------------

/// Max protrusion of an interactive prop (CRT desk, UPS rack) from its wall (m).
pub const INTERACTIVE_MAX_DEPTH: f32 = 0.55;

/// What an interactive wall slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveKind {
    /// Shootable CRT monitor on a wall-flush desk.
    Crt,
    /// Explosive crimson-LED Lithium UPS server rack.
    Ups,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveProp {
    pub face: WallFace,
    pub kind: InteractiveKind,
}

/// Shootable CRTs wanted on a floor (4..=8).
pub fn crt_target(floor: u32) -> usize {
    3 + floor.clamp(1, 5) as usize
}

/// Explosive UPS racks wanted on a floor (3..=6).
pub fn ups_target(floor: u32) -> usize {
    2 + floor.clamp(1, 4) as usize
}

/// Deterministic, capped interactive prop placement: the same wall-flush
/// slot rules as the telecom props (flat cells of rooms or >= 2-cell-wide
/// corridors, mid-way along unbroken straight walls, never a 1-cell squeeze),
/// at least 2 cells from every Milestone 16 wall prop and 3 cells apart.
/// UPS racks prefer room cells (combat rooms), then wide halls; CRTs take
/// the remaining slots, hallways first. Milestone 32: interactive props are
/// planned BEFORE the static wall furniture ([`plan_telecom_props`] keeps
/// at least 2 cells clear of them), so denser furniture never eats their budget.
pub fn plan_interactive_props(g: &LevelGrid) -> Vec<InteractiveProp> {
    let taken: Vec<IVec2> = plan_wall_props(g).iter().map(|p| p.face.cell).collect();
    let mut cands: Vec<WallFace> = wall_faces(g)
        .into_iter()
        .filter(|f| telecom_slot_ok(g, *f) && taken.iter().all(|w| (*w - f.cell).abs().max_element() >= 2))
        .collect();
    let mut rng = StdRng::seed_from_u64(g.seed ^ 0x0C1A_0531_DEAD_BEEF);
    cands.shuffle(&mut rng);
    let in_room = |c: IVec2| g.rooms.iter().any(|r| c.x >= r.min.x && c.y >= r.min.y && c.x < r.min.x + r.size.x && c.y < r.min.y + r.size.y);
    // Room slots first (stable partition keeps the shuffle otherwise).
    cands.sort_by_key(|f| !in_room(f.cell));
    let (want_ups, want_crt) = (ups_target(g.floor), crt_target(g.floor));
    let mut out: Vec<InteractiveProp> = Vec::new();
    let spaced = |out: &[InteractiveProp], f: &WallFace| out.iter().all(|p| (p.face.cell - f.cell).abs().element_sum() >= 3);
    // Reserve one chain-reaction UPS pair along an adjacent flat wall (4 m apart,
    // well inside the 7.5 m EMP blast radius) so generated floors actually chain.
    if want_ups >= 2 {
        'pair: for (i, a) in cands.iter().enumerate() {
            for b in &cands[i + 1..] {
                if a.side == b.side && (a.cell - b.cell).abs().element_sum() == 1 {
                    out.push(InteractiveProp { face: *a, kind: InteractiveKind::Ups });
                    out.push(InteractiveProp { face: *b, kind: InteractiveKind::Ups });
                    break 'pair;
                }
            }
        }
    }
    // Remaining UPS racks: combat rooms first, then the wide halls.
    for f in &cands {
        if out.len() >= want_ups {
            break;
        }
        if spaced(&out, f) {
            out.push(InteractiveProp { face: *f, kind: InteractiveKind::Ups });
        }
    }
    let mut crts = 0;
    for f in cands.iter().rev() {
        if crts >= want_crt {
            break;
        }
        if spaced(&out, f) {
            out.push(InteractiveProp { face: *f, kind: InteractiveKind::Crt });
            crts += 1;
        }
    }
    out
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
            let b = build_architecture_base(&g, &ArchMeshes::new());
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
            let (sc, l) = (b.screen.triangle_count(), b.lamp.triangle_count());
            assert!(sc > 0 && l > 0, "floor {floor} seed {seed}: telecom screens / LEDs placed");
            assert!(sc * 8 < MAX_EMISSIVE_TRIANGLES && l * 8 < MAX_EMISSIVE_TRIANGLES, "floor {floor}: {sc} screen / {l} lamp triangles");
            worst_accent = worst_accent.max(a);
            assert!(a * 8 < MAX_EMISSIVE_TRIANGLES, "floor {floor} seed {seed}: {a} emissive accent triangles");
            assert!(t < 250_000, "floor {floor}: {t} trim triangles");
            for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                let mesh = batch.build().unwrap();
                assert!(rt_mesh_convertible(&mesh));
                assert!(matches!(mesh.indices(), Some(bevy::mesh::Indices::U32(_))));
                assert!(mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0));
                assert_eq!(mesh.attributes().count(), 3, "exactly POSITION, NORMAL, UV_0");
            }
        }
        // Also stress-test max-size Floor 5 & Endless Floor 10 across 12 seeds for Solari RT budgets.
        for floor in [5u32, 10] {
            for s in 0..12u64 {
                let g = generate_level(floor, 0xE4D1_E550 ^ (s * 65537) ^ floor as u64);
                let b = build_architecture(&g);
                let (t, a) = (b.trim.triangle_count(), b.accent.triangle_count());
                worst_accent = worst_accent.max(a);
                assert!(a * 8 < MAX_EMISSIVE_TRIANGLES, "floor {floor} seed {s}: {a} emissive accent triangles");
                assert!(t < 250_000, "floor {floor} seed {s}: {t} trim triangles");
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
    fn telecom_props_are_flush_compact_and_rt_ready_in_their_face_frame() {
        let m = ArchMeshes::new();
        const { assert!(TELECOM_MAX_DEPTH <= 0.56) };
        // Milestone 32 footprints: (max |x|, max y, min z reach) per archetype.
        let footprint = |k: TelecomKind| match k {
            TelecomKind::OpsConsole => Some((1.1, 2.1, 0.5)),
            TelecomKind::VendingMachine => Some((0.475, 2.15, 0.5)),
            TelecomKind::SupplyShelf => Some((0.9, 2.2, 0.48)),
            TelecomKind::LockerBank => Some((0.825, 2.15, 0.48)),
            TelecomKind::LabBench => Some((1.0, 2.65, 0.5)),
            _ => None,
        };
        assert_eq!(TelecomKind::ALL.len(), 8);
        for (i, a) in TelecomKind::ALL.iter().enumerate() {
            assert!(!TelecomKind::ALL[i + 1..].contains(a), "{a:?} listed twice");
        }
        for kind in TelecomKind::ALL {
            for variant in 0..3u8 {
                let mut b = ArchBatches::default();
                build_telecom_prop(&mut b, &m, &Transform::IDENTITY, kind, variant);
                let mut n = 0;
                let (mut max_x, mut max_y, mut max_z) = (0.0f32, 0.0f32, 0.0f32);
                for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                    let Some(mesh) = batch.build() else { continue };
                    assert!(rt_mesh_convertible(&mesh), "{kind:?}");
                    assert!(matches!(mesh.indices(), Some(bevy::mesh::Indices::U32(_))));
                    assert!(mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0));
                    for p in positions(&mesh) {
                        n += 1;
                        assert!(p.z >= -1e-4 && p.z <= TELECOM_MAX_DEPTH + 1e-4, "{kind:?}: protrudes {} m", p.z);
                        assert!(p.x.abs() <= TELECOM_HALF_WIDTH + 1e-4, "{kind:?}: width {}", p.x);
                        assert!(p.y >= -1e-4 && p.y <= TELECOM_MAX_HEIGHT + 1e-4, "{kind:?}: height {}", p.y);
                        (max_x, max_y, max_z) = (max_x.max(p.x.abs()), max_y.max(p.y), max_z.max(p.z));
                    }
                }
                assert!(n > 0, "{kind:?} builds geometry");
                if let Some((w, h, reach)) = footprint(kind) {
                    assert!(max_x <= w + 1e-3 && max_x >= w - 0.15, "{kind:?} v{variant}: half-width {max_x} (want ~{w})");
                    assert!(max_y <= h + 1e-3 && max_y >= h - 0.25, "{kind:?} v{variant}: height {max_y} (want ~{h})");
                    assert!(max_z >= reach && max_z <= 0.55 + 1e-3, "{kind:?} v{variant}: depth {max_z}");
                }
                if kind == TelecomKind::LockerBank {
                    assert!(max_z <= 0.54 + 1e-3, "locker bench rail stays tucked: {max_z}");
                }
                if kind == TelecomKind::PcStation {
                    assert!(max_x > 1.25, "PC station has its swivel chair beside the desk ({max_x})");
                }
            }
        }
    }

    #[test]
    fn telecom_props_hug_straight_walls_and_never_cramp_the_lanes() {
        let mut total = 0;
        let mut kinds_seen = HashSet::new();
        let m = ArchMeshes::new();
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let props = plan_telecom_props(&g);
            assert_eq!(props, plan_telecom_props(&generate_level(floor, seed)), "deterministic");
            assert!(props.len() <= telecom_prop_target(floor));
            total += props.len();
            for p in &props {
                kinds_seen.insert(format!("{:?}", p.kind));
            }
            let walls = plan_wall_props(&g);
            let interactive = plan_interactive_props(&g);
            for (i, p) in props.iter().enumerate() {
                let (c, side, perp) = (p.face.cell, p.face.side, p.face.perp());
                assert!(telecom_slot_ok(&g, p.face));
                // Straight solid wall behind, open lane in front, no doorway / corner.
                assert!(g.cell(c + side).is_wall() && g.cell(c + side + perp).is_wall() && g.cell(c + side - perp).is_wall());
                assert!(!g.cell(c + perp).is_wall() && !g.cell(c - perp).is_wall() && !g.cell(c - side).is_wall());
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        assert!(!matches!(g.cell(c + IVec2::new(dx, dz)), Cell::Ramp { .. } | Cell::HazardPit), "prop {c} beside a ramp / pit");
                    }
                }
                assert!((c - g.spawn).abs().element_sum() >= 3, "spawn bay stays clear");
                for q in g.jammers.iter().copied().chain(g.barrels.iter().copied()).chain(g.jump_pads.iter().map(|p| p.cell)) {
                    assert!((q - c).abs().max_element() >= 2, "prop {c} crowds {q}");
                }
                for w in &walls {
                    assert!((w.face.cell - c).abs().max_element() >= 2, "prop {c} stacks on a wall prop");
                }
                for q in &interactive {
                    assert!((q.face.cell - c).abs().max_element() >= 2, "static furniture {c} crowds an interactive prop at {}", q.face.cell);
                }
                for o in &props[..i] {
                    assert!((o.face.cell - c).abs().element_sum() >= TELECOM_SPACING, "props spaced");
                }
                // World-space clearance: every vertex within 0.55 m of its
                // backing wall plane, inside its own cell, out of the lane.
                let mut b = ArchBatches::default();
                build_telecom_prop(&mut b, &m, &p.face.frame(0.0), p.kind, p.variant);
                let inward = p.face.inward();
                let plane = p.face.anchor();
                for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                    let Some(mesh) = batch.build() else { continue };
                    for v in positions(&mesh) {
                        let depth = (v - plane).dot(inward);
                        assert!((-1e-3..=TELECOM_MAX_DEPTH + 1e-3).contains(&depth), "floor {floor} seed {seed}: {v} is {depth} m off the wall");
                        assert_eq!(g.world_to_cell(v + inward * 2e-3), c, "vertex {v} leaves its cell {c}");
                        // A 2-cell (8 m) lane keeps >= 7.45 m of clear width.
                        assert!(CELL_SIZE * 2.0 - depth >= 7.45);
                    }
                }
            }
        }
        let avg = total as f32 / floors_and_seeds().len() as f32;
        println!("static wall furniture per floor: {avg:.1}");
        assert!(avg >= 15.0, "Milestone 32 density: {avg} pieces per floor");
        assert_eq!(kinds_seen.len(), TelecomKind::ALL.len(), "every archetype appears: {kinds_seen:?}");
    }

    #[test]
    fn m32_density_keeps_full_interactive_budgets_and_the_chain_pair_on_48_floors() {
        let (mut tel, mut nooks, mut floors) = (0usize, 0usize, 0usize);
        for floor in 1..=6u32 {
            for seed in 0..8u64 {
                let g = generate_level(floor, 0xC0FFEE ^ (seed * 7919) ^ floor as u64);
                let plan = plan_interactive_props(&g);
                let crts = plan.iter().filter(|p| p.kind == InteractiveKind::Crt).count();
                let ups = plan.iter().filter(|p| p.kind == InteractiveKind::Ups).count();
                assert_eq!((crts, ups), (crt_target(floor), ups_target(floor)), "floor {floor} seed {seed}: interactive budgets met");
                // The reserved chain-reaction pair: two adjacent UPS racks on one flat wall.
                let pair = plan.iter().enumerate().any(|(i, a)| {
                    plan[i + 1..].iter().any(|b| a.kind == InteractiveKind::Ups && b.kind == InteractiveKind::Ups && a.face.side == b.face.side && (a.face.cell - b.face.cell).abs().element_sum() == 1)
                });
                assert!(pair, "floor {floor} seed {seed}: UPS chain pair");
                // Zero overlap: no static wall furniture / wall prop / nook within a cell of an interactive prop.
                let telecom = plan_telecom_props(&g);
                let corner = plan_corner_nooks(&g);
                let statics: Vec<IVec2> = telecom.iter().map(|p| p.face.cell).chain(plan_wall_props(&g).iter().map(|p| p.face.cell)).chain(corner.iter().map(|n| n.cell)).collect();
                for p in &plan {
                    for c in &statics {
                        assert!((p.face.cell - *c).abs().max_element() >= 2, "floor {floor} seed {seed}: {:?} at {} overlaps static furniture at {c}", p.kind, p.face.cell);
                    }
                }
                assert!(telecom.len() <= telecom_prop_target(floor) && corner.len() <= corner_nook_target(floor));
                tel += telecom.len();
                nooks += corner.len();
                floors += 1;
            }
        }
        let (t, n) = (tel as f32 / floors as f32, nooks as f32 / floors as f32);
        println!("M32 per floor over {floors} floors: {t:.1} wall furniture pieces, {n:.1} corner nooks");
        assert!(t >= 18.0 && n >= 6.0, "{t} furniture / {n} nooks per floor");
    }

    #[test]
    fn corner_nooks_tuck_into_dead_corners_clear_of_doorways_pickups_and_spawn() {
        let m = ArchMeshes::new();
        // In its own frame: within NOOK_MAX_DEPTH of both walls, on the floor.
        for variant in 0..3u8 {
            let mut b = ArchBatches::default();
            build_corner_nook(&mut b, &m, &Transform::IDENTITY, variant);
            let mut n = 0;
            for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                let Some(mesh) = batch.build() else { continue };
                assert!(rt_mesh_convertible(&mesh));
                for p in positions(&mesh) {
                    n += 1;
                    assert!((-1e-4..=NOOK_MAX_DEPTH + 1e-4).contains(&p.x) && (-1e-4..=NOOK_MAX_DEPTH + 1e-4).contains(&p.z), "nook vertex {p}");
                    assert!((-1e-4..=NOOK_MAX_HEIGHT).contains(&p.y), "nook vertex {p}");
                }
            }
            assert!(n > 100);
        }
        let mut total = 0;
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let nooks = plan_corner_nooks(&g);
            assert_eq!(nooks, plan_corner_nooks(&generate_level(floor, seed)), "deterministic");
            assert!(nooks.len() <= corner_nook_target(floor));
            total += nooks.len();
            let pickups: Vec<IVec2> = g.rooms.iter().skip(1).filter_map(|r| crate::level::room_pickup_cell(&g, r)).collect();
            for (i, nk) in nooks.iter().enumerate() {
                let c = nk.cell;
                assert!(corner_nook_ok(&g, c, nk.a, nk.b));
                // A real corner: both walls continue past it (no doorway jamb), 2x2 open block.
                for o in [nk.a, nk.b, nk.a + nk.b, nk.a - nk.b, nk.b - nk.a] {
                    assert!(g.cell(c + o).is_wall(), "floor {floor} seed {seed}: nook {c} next to a doorway");
                }
                for o in [-nk.a, -nk.b, -nk.a - nk.b] {
                    assert!(!g.cell(c + o).is_wall(), "nook {c} in a 1-cell squeeze");
                }
                assert!(!g.rooms[0].contains(c) && !g.is_roof_open(c), "never in the atrium");
                assert!((c - g.spawn).abs().element_sum() >= 3, "spawn stays clear");
                for q in pickups.iter().chain(&g.jammers).chain(&g.barrels).copied().chain(g.jump_pads.iter().map(|p| p.cell)) {
                    assert!((q - c).abs().max_element() >= 2, "nook {c} crowds {q}");
                }
                for o in &nooks[..i] {
                    assert_ne!(o.cell, c);
                }
                // World space: every vertex hugs both walls of its own cell.
                let mut b = ArchBatches::default();
                build_corner_nook(&mut b, &m, &nk.frame(), nk.variant);
                let corner = nk.corner();
                let (wa, wb) = (Vec3::new(nk.a.x as f32, 0.0, nk.a.y as f32), Vec3::new(nk.b.x as f32, 0.0, nk.b.y as f32));
                for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                    let Some(mesh) = batch.build() else { continue };
                    for v in positions(&mesh) {
                        let (da, db) = ((corner - v).dot(wa), (corner - v).dot(wb));
                        assert!((-1e-3..=NOOK_MAX_DEPTH + 1e-3).contains(&da) && (-1e-3..=NOOK_MAX_DEPTH + 1e-3).contains(&db), "floor {floor} seed {seed}: {v} is {da}/{db} m off the walls");
                        assert_eq!(g.world_to_cell(v - (wa + wb) * 2e-3), c, "nook vertex {v} leaves its cell");
                    }
                }
            }
        }
        assert!(total >= floors_and_seeds().len() * 5, "nooks are actually placed ({total})");
    }

    #[test]
    fn ceiling_ducts_hang_high_and_never_over_open_roof_cells() {
        let m = ArchMeshes::new();
        const { assert!(DUCT_MIN_Y >= 5.2 && DUCT_MAX_Y <= CEILING_HEIGHT) };
        for (floor, seed) in floors_and_seeds() {
            let g = generate_level(floor, seed);
            let mut b = ArchBatches::default();
            build_ceiling_ducts(&g, &m, &mut b);
            let mut n = 0;
            for batch in [b.trim, b.accent, b.plate, b.screen, b.lamp] {
                let Some(mesh) = batch.build() else { continue };
                assert!(rt_mesh_convertible(&mesh));
                for v in positions(&mesh) {
                    n += 1;
                    assert!(v.is_finite());
                    assert!(v.y >= DUCT_MIN_Y - 1e-3 && v.y <= CEILING_HEIGHT + 1e-3, "floor {floor} seed {seed}: duct vertex {v} hangs into play space");
                    let c = g.world_to_cell(v);
                    if g.is_roof_open(c) || g.cell(c).is_wall() {
                        // Only allowed exactly on the seam with a covered cell.
                        let lo = c.as_vec2() * CELL_SIZE;
                        let q = Vec2::new(v.x, v.z) - lo;
                        let edge = q.min_element().min((Vec2::splat(CELL_SIZE) - q).min_element());
                        assert!(edge <= 1e-3, "floor {floor} seed {seed}: duct vertex {v} over open-roof / wall cell {c}");
                    }
                }
            }
            assert!(n > 1000, "floor {floor} seed {seed}: ducts placed ({n} vertices)");
            // Duct / tray bodies stay in the 5.6..6.8 m band; only thin hangers reach the ceiling.
            let mut b = ArchBatches::default();
            build_ceiling_ducts(&g, &m, &mut b);
            let plate = b.plate.build().expect("duct bodies");
            assert!(positions(&plate).iter().all(|v| v.y <= DUCT_MAX_Y + 1e-3 && v.y >= DUCT_MIN_Y - 1e-3));
        }
        // The open-roof Atrium gets none, even when it is the whole map.
        let mut g = generate_level(1, 1234);
        g.roof_open = vec![true; g.roof_open.len()];
        let mut b = ArchBatches::default();
        build_ceiling_ducts(&g, &m, &mut b);
        assert!(b.trim.build().is_none() && b.plate.build().is_none());
    }

    #[test]
    fn atrium_tower_wears_a_relay_dish_crown_high_above_play() {
        for (floor, seed) in [(1u32, 1234u64), (3, 99), (5, 2026)] {
            let g = generate_level(floor, seed);
            assert!(g.is_roof_open(g.tower));
            let m = ArchMeshes::new();
            let mut b = ArchBatches::default();
            build_tower_crown(&g, &m, &mut b);
            let base = g.tower_world();
            let mut n = 0;
            for batch in [b.trim, b.accent] {
                let mesh = batch.build().expect("crown parts");
                assert!(rt_mesh_convertible(&mesh));
                for v in positions(&mesh) {
                    n += 1;
                    assert!(v.y >= base.y + CROWN_MIN_Y, "crown vertex {v} hangs into play space");
                    assert!(Vec2::new(v.x - base.x, v.z - base.z).length() < 4.0, "crown stays on the mast");
                }
            }
            assert!(n > 100);
        }
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

    #[test]
    fn interactive_props_are_capped_spaced_wall_flush_and_clear_of_other_props() {
        let (mut crt_total, mut ups_total, mut floors) = (0, 0, 0);
        for floor in 1..=6u32 {
            for seed in 0..8u64 {
                let g = crate::level::generate_level(floor, 0xC0FFEE ^ (seed * 7919) ^ floor as u64);
                let plan = plan_interactive_props(&g);
                assert_eq!(plan, plan_interactive_props(&g), "deterministic");
                let crts = plan.iter().filter(|p| p.kind == InteractiveKind::Crt).count();
                let ups = plan.iter().filter(|p| p.kind == InteractiveKind::Ups).count();
                assert!(crts <= crt_target(floor) && ups <= ups_target(floor));
                assert!((4..=8).contains(&crt_target(floor)) && (3..=6).contains(&ups_target(floor)));
                let others: Vec<IVec2> = plan_wall_props(&g).iter().map(|p| p.face.cell).chain(plan_telecom_props(&g).iter().map(|p| p.face.cell)).collect();
                let mut chain_pairs = 0usize;
                for (i, p) in plan.iter().enumerate() {
                    assert!(telecom_slot_ok(&g, p.face), "wide lane, straight wall");
                    assert!(others.iter().all(|o| (*o - p.face.cell).abs().max_element() >= 2), "clear of M16 / M30 props");
                    for q in &plan[i + 1..] {
                        let d = (q.face.cell - p.face.cell).abs().element_sum();
                        if d == 1 && p.kind == InteractiveKind::Ups && q.kind == InteractiveKind::Ups && p.face.side == q.face.side {
                            chain_pairs += 1;
                        } else {
                            assert!(d >= 3, "non-pair props stay >= 3 cells apart (got {d})");
                        }
                    }
                }
                assert!(chain_pairs <= 1, "at most one reserved UPS chain pair per floor");
                crt_total += crts;
                ups_total += ups;
                floors += 1;
            }
        }
        let (c, u) = (crt_total as f32 / floors as f32, ups_total as f32 / floors as f32);
        println!("interactive props per floor: {c:.1} CRTs, {u:.1} UPS racks");
        assert!(c >= 3.0 && u >= 2.5, "budgets mostly filled: {c} CRTs, {u} UPS");
    }
}
