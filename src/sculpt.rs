//! Procedural sculpted 3D mesh builders for SEGNALE DEBOLE enemies and weapons.
//!
//! Every builder returns a valid `TriangleList` [`Mesh`] with `ATTRIBUTE_POSITION`,
//! `ATTRIBUTE_NORMAL`, `ATTRIBUTE_UV_0`, and `Indices::U32`, with guaranteed outward
//! counter-clockwise winding and finite unit-length normals so [`crate::mesh_batch::MaterialBatches`]
//! and [`crate::mesh_batch::ParentedBatches`] can merge them per material at startup
//! with zero runtime allocations.

use std::f32::consts::{PI, TAU};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

struct Builder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl Builder {
    fn new() -> Self {
        Self { positions: Vec::new(), normals: Vec::new(), uvs: Vec::new(), indices: Vec::new() }
    }

    fn push_vert(&mut self, p: Vec3, n: Vec3, uv: [f32; 2]) -> u32 {
        let idx = self.positions.len() as u32;
        self.positions.push(p.to_array());
        self.normals.push(n.normalize_or(Vec3::Y).to_array());
        self.uvs.push(uv);
        idx
    }

    /// Push a flat-shaded triangle whose normal is computed from CCW `(a, b, c)`.
    /// Uses a scale-safe area threshold so millimeter-scale viewmodel bevels stay watertight.
    fn push_flat_tri(&mut self, a: Vec3, b: Vec3, c: Vec3, uva: [f32; 2], uvb: [f32; 2], uvc: [f32; 2]) {
        let cross = (b - a).cross(c - a);
        let len_sq = cross.length_squared();
        let edge_scale = (b - a).length_squared().max((c - a).length_squared()).max(1e-24);
        if len_sq <= edge_scale * 1e-12 || len_sq <= 1e-28 {
            return;
        }
        let n = cross / len_sq.sqrt();
        let ia = self.push_vert(a, n, uva);
        let ib = self.push_vert(b, n, uvb);
        let ic = self.push_vert(c, n, uvc);
        self.indices.extend_from_slice(&[ia, ib, ic]);
    }

    /// Push a flat-shaded CCW quad `(a, b, c, d)` split into two triangles.
    fn push_flat_quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, uv_scale: Vec2) {
        let e1 = b - a;
        let e2 = d - a;
        let w = e1.length() * uv_scale.x;
        let h = e2.length() * uv_scale.y;
        self.push_flat_tri(a, b, c, [0.0, 0.0], [w, 0.0], [w, h]);
        self.push_flat_tri(a, c, d, [0.0, 0.0], [w, h], [0.0, h]);
    }

    fn finish(self) -> Mesh {
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
            .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// Connect two closed rings of vertices (same length `n`, ordered so that
/// `(r0[j] - r0[i]).cross(r1[j] - r0[i])` faces outward).
fn stitch_rings(b: &mut Builder, r0: &[Vec3], r1: &[Vec3], v0: f32, v1: f32, flat: bool) {
    let n = r0.len();
    assert_eq!(n, r1.len());
    if n < 3 {
        return;
    }
    if flat {
        for i in 0..n {
            let j = (i + 1) % n;
            let u0 = i as f32 / n as f32;
            let u1 = (i + 1) as f32 / n as f32;
            b.push_flat_tri(r0[i], r0[j], r1[j], [u0, v0], [u1, v0], [u1, v1]);
            b.push_flat_tri(r0[i], r1[j], r1[i], [u0, v0], [u1, v1], [u0, v1]);
        }
    } else {
        let base = b.positions.len() as u32;
        let c0: Vec3 = r0.iter().copied().sum::<Vec3>() / n as f32;
        let c1: Vec3 = r1.iter().copied().sum::<Vec3>() / n as f32;
        for k in 0..=n {
            let i = k % n;
            let u = k as f32 / n as f32;
            let prev = (i + n - 1) % n;
            let next = (i + 1) % n;
            let tang0 = (r0[next] - r0[prev]).normalize_or(Vec3::X);
            let up0 = (r1[i] - r0[i]).normalize_or(Vec3::Y);
            let n0 = tang0.cross(up0).try_normalize().unwrap_or_else(|| (r0[i] - c0).normalize_or(Vec3::X));
            let tang1 = (r1[next] - r1[prev]).normalize_or(Vec3::X);
            let n1 = tang1.cross(up0).try_normalize().unwrap_or_else(|| (r1[i] - c1).normalize_or(Vec3::X));
            b.push_vert(r0[i], n0, [u, v0]);
            b.push_vert(r1[i], n1, [u, v1]);
        }
        for k in 0..n as u32 {
            let i0 = base + 2 * k;
            let i1 = i0 + 1;
            let i2 = i0 + 2;
            let i3 = i0 + 3;
            b.indices.extend_from_slice(&[i0, i2, i3, i0, i3, i1]);
        }
    }
}

/// Stitch a full sequence of equal-length rings with continuous smooth normals across
/// neighboring bands (eliminating band-seam shading creases).
fn stitch_smooth_loft(b: &mut Builder, rings: &[Vec<Vec3>], vs: &[f32]) {
    if rings.len() < 2 {
        return;
    }
    let m = rings.len();
    let n = rings[0].len();
    if n < 3 {
        return;
    }
    let centers: Vec<Vec3> = rings.iter().map(|r| r.iter().copied().sum::<Vec3>() / n as f32).collect();
    let stride = (n + 1) as u32;
    let base = b.positions.len() as u32;
    for r_i in 0..m {
        let v = vs[r_i];
        let prev_r = r_i.saturating_sub(1);
        let next_r = (r_i + 1).min(m - 1);
        for k in 0..=n {
            let i = k % n;
            let u = k as f32 / n as f32;
            let prev_i = (i + n - 1) % n;
            let next_i = (i + 1) % n;
            let tang = (rings[r_i][next_i] - rings[r_i][prev_i]).normalize_or(Vec3::X);
            let up = (rings[next_r][i] - rings[prev_r][i]).normalize_or(Vec3::Y);
            let nrm = tang.cross(up).try_normalize().unwrap_or_else(|| (rings[r_i][i] - centers[r_i]).normalize_or(Vec3::X));
            b.push_vert(rings[r_i][i], nrm, [u, v]);
        }
    }
    for r_i in 0..(m - 1) as u32 {
        for k in 0..n as u32 {
            let i0 = base + r_i * stride + k;
            let i1 = i0 + 1;
            let i2 = i0 + stride;
            let i3 = i2 + 1;
            b.indices.extend_from_slice(&[i0, i1, i3, i0, i3, i2]);
        }
    }
}

/// Cap a closed ring with a fan around `center` in the plane perpendicular to `outward_normal`.
fn cap_ring_oriented(b: &mut Builder, ring: &[Vec3], center: Vec3, outward_normal: Vec3) {
    let n = ring.len();
    if n < 3 {
        return;
    }
    let normal = outward_normal.normalize_or(Vec3::Y);
    let ref_v = if normal.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let u_axis = normal.cross(ref_v).normalize();
    let v_axis = normal.cross(u_axis).normalize();
    let max_r = ring.iter().map(|p| (*p - center).length()).fold(1e-5f32, f32::max);
    let uv_of = |p: Vec3| -> [f32; 2] {
        let d = p - center;
        [0.5 + 0.5 * d.dot(u_axis) / max_r, 0.5 + 0.5 * d.dot(v_axis) / max_r]
    };
    let c_idx = b.push_vert(center, normal, [0.5, 0.5]);
    let start = b.positions.len() as u32;
    for p in ring {
        b.push_vert(*p, normal, uv_of(*p));
    }
    for i in 0..n as u32 {
        let j = (i + 1) % n as u32;
        let p0 = ring[i as usize];
        let p1 = ring[j as usize];
        if (p0 - center).cross(p1 - center).dot(normal) >= 0.0 {
            b.indices.extend_from_slice(&[c_idx, start + i, start + j]);
        } else {
            b.indices.extend_from_slice(&[c_idx, start + j, start + i]);
        }
    }
}

fn cap_ring(b: &mut Builder, ring: &[Vec3], center: Vec3, top: bool) {
    cap_ring_oriented(b, ring, center, if top { Vec3::Y } else { Vec3::NEG_Y });
}

/// Chamfered octagonal or rectangular cross-section for half-extents `(hx, hz)`.
/// `chamfered` is chosen once per box so adjacent rings always share identical vertex count.
fn chamfer_rect_ring(hx: f32, hz: f32, c: f32, y: f32, chamfered: bool) -> Vec<Vec3> {
    let c = c.clamp(0.0, hx.min(hz) * 0.48);
    let pts = if chamfered {
        vec![
            Vec2::new(hx, hz - c),
            Vec2::new(hx, -(hz - c)),
            Vec2::new(hx - c, -hz),
            Vec2::new(-(hx - c), -hz),
            Vec2::new(-hx, -(hz - c)),
            Vec2::new(-hx, hz - c),
            Vec2::new(-(hx - c), hz),
            Vec2::new(hx - c, hz),
        ]
    } else {
        vec![
            Vec2::new(hx, hz),
            Vec2::new(hx, -hz),
            Vec2::new(-hx, -hz),
            Vec2::new(-hx, hz),
        ]
    };
    pts.into_iter().map(|p| Vec3::new(p.x, y, p.y)).collect()
}

/// Beveled industrial armor block / gun receiver / chassis housing centered at origin.
/// All 12 edges are chamfered by `bevel` so specular highlights catch every edge cleanly.
pub fn beveled_box(size: Vec3, bevel: f32) -> Mesh {
    let hx = (size.x * 0.5).max(1e-4);
    let hy = (size.y * 0.5).max(1e-4);
    let hz = (size.z * 0.5).max(1e-4);
    let min_h = hx.min(hy).min(hz);
    let c = bevel.clamp(0.0, min_h * 0.42);
    // Decide ring topology ONCE based on scale-relative bevel size so adjacent rings always match.
    let chamfered = c > min_h * 0.01 && c > 1e-6;
    let mut b = Builder::new();
    if chamfered {
        let r_bot = chamfer_rect_ring(hx - c, hz - c, c * 0.65, -hy, true);
        let r_lo = chamfer_rect_ring(hx, hz, c, -hy + c, true);
        let r_hi = chamfer_rect_ring(hx, hz, c, hy - c, true);
        let r_top = chamfer_rect_ring(hx - c, hz - c, c * 0.65, hy, true);
        stitch_rings(&mut b, &r_bot, &r_lo, 0.0, 0.15, true);
        stitch_rings(&mut b, &r_lo, &r_hi, 0.15, 0.85, true);
        stitch_rings(&mut b, &r_hi, &r_top, 0.85, 1.0, true);
        cap_ring(&mut b, &r_bot, Vec3::new(0.0, -hy, 0.0), false);
        cap_ring(&mut b, &r_top, Vec3::new(0.0, hy, 0.0), true);
    } else {
        let r0 = chamfer_rect_ring(hx, hz, 0.0, -hy, false);
        let r1 = chamfer_rect_ring(hx, hz, 0.0, hy, false);
        stitch_rings(&mut b, &r0, &r1, 0.0, 1.0, true);
        cap_ring(&mut b, &r0, Vec3::new(0.0, -hy, 0.0), false);
        cap_ring(&mut b, &r1, Vec3::new(0.0, hy, 0.0), true);
    }
    b.finish()
}

/// Faceted or smooth tapered prism / frustum / pyramid centered at origin along Y (`-h/2 .. +h/2`).
/// Handles zero top or bottom radius cleanly with a watertight apex fan.
pub fn tapered_prism(sides: usize, r_bottom: f32, r_top: f32, height: f32, flat_shaded: bool) -> Mesh {
    let sides = sides.max(3);
    let hy = (height * 0.5).max(1e-5);
    let rb = r_bottom.max(0.0);
    let rt = r_top.max(0.0);
    let ring = |r: f32, y: f32| -> Vec<Vec3> {
        (0..sides)
            .map(|i| {
                let a = i as f32 * TAU / sides as f32;
                Vec3::new(a.cos() * r, y, -a.sin() * r)
            })
            .collect()
    };
    let mut b = Builder::new();
    if rt <= 1e-5 && rb > 1e-5 {
        let r0 = ring(rb, -hy);
        let apex = Vec3::new(0.0, hy, 0.0);
        for i in 0..sides {
            let j = (i + 1) % sides;
            let u0 = i as f32 / sides as f32;
            let u1 = (i + 1) as f32 / sides as f32;
            b.push_flat_tri(r0[i], r0[j], apex, [u0, 0.0], [u1, 0.0], [(u0 + u1) * 0.5, 1.0]);
        }
        cap_ring(&mut b, &r0, Vec3::new(0.0, -hy, 0.0), false);
    } else if rb <= 1e-5 && rt > 1e-5 {
        let r1 = ring(rt, hy);
        let apex = Vec3::new(0.0, -hy, 0.0);
        for i in 0..sides {
            let j = (i + 1) % sides;
            let u0 = i as f32 / sides as f32;
            let u1 = (i + 1) as f32 / sides as f32;
            b.push_flat_tri(apex, r1[j], r1[i], [(u0 + u1) * 0.5, 0.0], [u1, 1.0], [u0, 1.0]);
        }
        cap_ring(&mut b, &r1, Vec3::new(0.0, hy, 0.0), true);
    } else {
        let r0 = ring(rb.max(1e-4), -hy);
        let r1 = ring(rt.max(1e-4), hy);
        stitch_rings(&mut b, &r0, &r1, 0.0, 1.0, flat_shaded);
        cap_ring(&mut b, &r0, Vec3::new(0.0, -hy, 0.0), false);
        cap_ring(&mut b, &r1, Vec3::new(0.0, hy, 0.0), true);
    }
    b.finish()
}

/// Sweep a tapered horn / claw / tusk along a cubic Bezier spine `(0 -> p1 -> p2 -> p3)` using
/// a continuous parallel-transport frame (zero twist discontinuities at any bend angle!).
pub fn curved_horn_sweep(
    radius_base: f32,
    radius_tip: f32,
    p1: Vec3,
    p2: Vec3,
    p3: Vec3,
    segments: usize,
    sides: usize,
) -> Mesh {
    let segs = segments.max(4);
    let sides = sides.max(5);
    let p0 = Vec3::ZERO;
    let eval = |t: f32| -> Vec3 {
        let u = 1.0 - t;
        p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t)
    };
    let tang = |t: f32| -> Vec3 {
        let u = 1.0 - t;
        let d = (p1 - p0) * (3.0 * u * u) + (p2 - p1) * (6.0 * u * t) + (p3 - p2) * (3.0 * t * t);
        d.try_normalize().unwrap_or(Vec3::Y)
    };

    // Continuous parallel-transport frame along the curve.
    let mut prev_up = tang(0.0);
    let init_ref = if prev_up.z.abs() < 0.85 { Vec3::Z } else { Vec3::X };
    let mut right = prev_up.cross(init_ref).normalize();
    let mut fwd = right.cross(prev_up).normalize();

    let mut rings: Vec<Vec<Vec3>> = Vec::with_capacity(segs + 1);
    let mut vs: Vec<f32> = Vec::with_capacity(segs + 1);
    let has_blunt_tip = radius_tip > 1e-4;
    let ring_count = if has_blunt_tip { segs } else { segs - 1 };

    for s in 0..=ring_count {
        let t = s as f32 / segs as f32;
        let center = eval(t);
        let up = tang(t);
        if s > 0 {
            let rot = Quat::from_rotation_arc(prev_up, up);
            right = (rot * right).normalize();
            // Re-orthogonalize to prevent numerical drift.
            fwd = right.cross(up).normalize();
            right = up.cross(fwd).normalize();
            prev_up = up;
        }
        let r = (radius_base * (1.0 - t).powf(0.85) + radius_tip * t).max(1e-4);
        let mut ring = Vec::with_capacity(sides);
        for i in 0..sides {
            let a = i as f32 * TAU / sides as f32;
            let rib = 1.0 + 0.055 * (t * PI * 6.0).sin() * (1.0 - t);
            ring.push(center + (right * a.cos() - fwd * a.sin()) * (r * rib));
        }
        rings.push(ring);
        vs.push(t);
    }

    let mut b = Builder::new();
    stitch_smooth_loft(&mut b, &rings, &vs);
    cap_ring_oriented(&mut b, &rings[0], p0, -tang(0.0));
    // Pointed watertight apex fan.
    let tip = if has_blunt_tip {
        p3 + tang(1.0) * (radius_tip * 1.4).max(0.015)
    } else {
        p3
    };
    let last = rings.last().unwrap();
    let v_last = *vs.last().unwrap();
    for i in 0..sides {
        let j = (i + 1) % sides;
        let u0 = i as f32 / sides as f32;
        let u1 = (i + 1) as f32 / sides as f32;
        b.push_flat_tri(last[i], last[j], tip, [u0, v_last], [u1, v_last], [(u0 + u1) * 0.5, 1.0]);
    }
    b.finish()
}

/// Curved, tapering horn / claw / tusk starting at origin along `+Y`, curving by `(bend_x, bend_z)` at the tip.
/// Built on [`curved_horn_sweep`] with continuous parallel-transport framing.
pub fn curved_horn(
    radius_base: f32,
    radius_tip: f32,
    length: f32,
    bend_x: f32,
    bend_z: f32,
    segments: usize,
    sides: usize,
) -> Mesh {
    let p1 = Vec3::new(bend_x * 0.18, length * 0.42, bend_z * 0.18);
    let p2 = Vec3::new(bend_x * 0.68, length * 0.80, bend_z * 0.68);
    let p3 = Vec3::new(bend_x, length, bend_z);
    curved_horn_sweep(radius_base, radius_tip, p1, p2, p3, segments, sides)
}

/// Swept diamond-cross-section scythe / mantis blade extending along `+Y` (`0..length`),
/// curving toward `-Z` (cutting edge forward) by `curve`, with razor beveled edges.
pub fn curved_blade(length: f32, width: f32, thickness: f32, curve: f32, segments: usize) -> Mesh {
    let segs = segments.max(3);
    let mut rings: Vec<Vec<Vec3>> = Vec::with_capacity(segs + 1);
    for s in 0..=segs {
        let t = s as f32 / segs as f32;
        let w_scale = if t < 0.30 {
            0.72 + (t / 0.30) * 0.28
        } else {
            ((1.0 - t) / 0.70).powf(0.72)
        };
        let hw = (width * 0.5 * w_scale).max(0.002);
        let ht = (thickness * 0.5 * (1.0 - t * 0.78)).max(0.0015);
        let y = t * length;
        let z = -curve * t * t;
        rings.push(vec![
            Vec3::new(ht, y, z),
            Vec3::new(0.0, y, z - hw),
            Vec3::new(-ht, y, z),
            Vec3::new(0.0, y, z + hw * 0.72),
        ]);
    }

    let mut b = Builder::new();
    for s in 0..segs {
        let v0 = s as f32 / segs as f32;
        let v1 = (s + 1) as f32 / segs as f32;
        stitch_rings(&mut b, &rings[s], &rings[s + 1], v0, v1, true);
    }
    cap_ring(&mut b, &rings[0], Vec3::ZERO, false);
    let tip = Vec3::new(0.0, length + width * 0.14, -curve - width * 0.10);
    let last = &rings[segs];
    for i in 0..4 {
        let j = (i + 1) % 4;
        b.push_flat_tri(last[i], last[j], tip, [0.0, 0.9], [1.0, 0.9], [0.5, 1.0]);
    }
    b.finish()
}

/// Organic ridged / faceted sphere (Cacodemon carapace, demonic cranium, armored pauldron dome).
/// `ridge_amp` modulates the surface radially along `ridge_freq` ribs.
pub fn ridged_sphere(radius: Vec3, rings: usize, sectors: usize, ridge_amp: f32, ridge_freq: f32, flat_shaded: bool) -> Mesh {
    let rings = rings.max(4);
    let sectors = sectors.max(6);
    let mut b = Builder::new();
    let sample = |r_i: usize, s_i: usize| -> Vec3 {
        let v = r_i as f32 / rings as f32;
        let phi = -PI * 0.5 + v * PI;
        let u = s_i as f32 / sectors as f32;
        let theta = u * TAU;
        let cp = phi.cos();
        let sp = phi.sin();
        let ridge = 1.0 + ridge_amp * cp * ((theta * ridge_freq).cos() * 0.65 + (phi * ridge_freq * 0.7).cos() * 0.35);
        Vec3::new(cp * theta.cos() * radius.x * ridge, sp * radius.y * ridge, -cp * theta.sin() * radius.z * ridge)
    };

    let mut lat_rings: Vec<Vec<Vec3>> = Vec::with_capacity(rings - 1);
    let mut vs: Vec<f32> = Vec::with_capacity(rings - 1);
    for r_i in 1..rings {
        let mut ring = Vec::with_capacity(sectors);
        for s_i in 0..sectors {
            ring.push(sample(r_i, s_i));
        }
        lat_rings.push(ring);
        vs.push(r_i as f32 / rings as f32);
    }
    let south = Vec3::new(0.0, -radius.y, 0.0);
    let north = Vec3::new(0.0, radius.y, 0.0);

    // South pole fan (uses sector midpoint U to avoid wrap stretching).
    for s_i in 0..sectors {
        let next = (s_i + 1) % sectors;
        let p0 = lat_rings[0][s_i];
        let p1 = lat_rings[0][next];
        let u0 = s_i as f32 / sectors as f32;
        let u1 = (s_i + 1) as f32 / sectors as f32;
        let um = (u0 + u1) * 0.5;
        if flat_shaded {
            b.push_flat_tri(south, p1, p0, [um, 0.0], [u1, 1.0 / rings as f32], [u0, 1.0 / rings as f32]);
        } else {
            let n_s = Vec3::NEG_Y;
            let n0 = (p0 / radius.max(Vec3::splat(1e-4))).normalize_or(Vec3::NEG_Y);
            let n1 = (p1 / radius.max(Vec3::splat(1e-4))).normalize_or(Vec3::NEG_Y);
            let i_s = b.push_vert(south, n_s, [um, 0.0]);
            let i1 = b.push_vert(p1, n1, [u1, 1.0 / rings as f32]);
            let i0 = b.push_vert(p0, n0, [u0, 1.0 / rings as f32]);
            b.indices.extend_from_slice(&[i_s, i1, i0]);
        }
    }

    // Middle bands: continuous smooth loft when smooth-shaded.
    if flat_shaded {
        for band in 0..lat_rings.len().saturating_sub(1) {
            stitch_rings(&mut b, &lat_rings[band], &lat_rings[band + 1], vs[band], vs[band + 1], true);
        }
    } else {
        stitch_smooth_loft(&mut b, &lat_rings, &vs);
    }

    // North pole fan (uses sector midpoint U).
    let top_ring = lat_rings.last().unwrap();
    let v_top = (rings - 1) as f32 / rings as f32;
    for s_i in 0..sectors {
        let next = (s_i + 1) % sectors;
        let p0 = top_ring[s_i];
        let p1 = top_ring[next];
        let u0 = s_i as f32 / sectors as f32;
        let u1 = (s_i + 1) as f32 / sectors as f32;
        let um = (u0 + u1) * 0.5;
        if flat_shaded {
            b.push_flat_tri(p0, p1, north, [u0, v_top], [u1, v_top], [um, 1.0]);
        } else {
            let n_n = Vec3::Y;
            let n0 = (p0 / radius.max(Vec3::splat(1e-4))).normalize_or(Vec3::Y);
            let n1 = (p1 / radius.max(Vec3::splat(1e-4))).normalize_or(Vec3::Y);
            let i0 = b.push_vert(p0, n0, [u0, v_top]);
            let i1 = b.push_vert(p1, n1, [u1, v_top]);
            let i_n = b.push_vert(north, n_n, [um, 1.0]);
            b.indices.extend_from_slice(&[i0, i1, i_n]);
        }
    }
    b.finish()
}

/// Solid thick-walled parabolic relay dish / flared exhaust bell opening toward `+Y`.
/// Base hub sits at `y = 0`, flared rim reaches `y = depth` at radius `r_outer`.
pub fn flared_dish(r_inner: f32, r_outer: f32, depth: f32, thickness: f32, sides: usize) -> Mesh {
    let sides = sides.max(8);
    let r_in = r_inner.max(0.01);
    let r_out = r_outer.max(r_in + 0.02);
    let thick = thickness.max(0.008);
    let ring = |r: f32, y: f32| -> Vec<Vec3> {
        (0..sides)
            .map(|i| {
                let a = i as f32 * TAU / sides as f32;
                Vec3::new(a.cos() * r, y, -a.sin() * r)
            })
            .collect()
    };
    let r_mid = r_in + (r_out - r_in) * 0.55;
    let d_mid = depth * 0.36;

    let out0 = ring(r_in, 0.0);
    let out1 = ring(r_mid, d_mid);
    let out2 = ring(r_out, depth);
    let in2 = ring((r_out - thick).max(r_in * 0.6), depth);
    let in1 = ring((r_mid - thick).max(r_in * 0.5), d_mid + thick * 0.7);
    let in0 = ring((r_in * 0.75).max(0.008), thick);

    let mut b = Builder::new();
    stitch_rings(&mut b, &out0, &out1, 0.0, 0.45, true);
    stitch_rings(&mut b, &out1, &out2, 0.45, 0.9, true);
    stitch_rings(&mut b, &out2, &in2, 0.9, 1.0, true);
    stitch_rings(&mut b, &in2, &in1, 0.8, 0.4, true);
    stitch_rings(&mut b, &in1, &in0, 0.4, 0.0, true);
    cap_ring(&mut b, &out0, Vec3::ZERO, false);
    cap_ring(&mut b, &in0, Vec3::new(0.0, thick, 0.0), true);
    b.finish()
}

/// Faceted industrial beveled containment ring / halo lying in the XZ plane around origin.
/// Cross-section is a chamfered 8-sided band with half-width `tube_r` and half-height `tube_h`.
pub fn armored_ring(r_major: f32, tube_r: f32, tube_h: f32, segments: usize, flat_facets: bool) -> Mesh {
    let segs = segments.max(8);
    let profile = [
        Vec2::new(tube_r, -tube_h * 0.55),
        Vec2::new(tube_r, tube_h * 0.55),
        Vec2::new(tube_r * 0.45, tube_h),
        Vec2::new(-tube_r * 0.45, tube_h),
        Vec2::new(-tube_r, tube_h * 0.55),
        Vec2::new(-tube_r, -tube_h * 0.55),
        Vec2::new(-tube_r * 0.45, -tube_h),
        Vec2::new(tube_r * 0.45, -tube_h),
    ];
    let p_len = profile.len();
    // Exact outward 2D profile normals derived from adjacent profile edge tangents.
    let prof_normals: Vec<Vec2> = (0..p_len)
        .map(|i| {
            let prev = profile[(i + p_len - 1) % p_len];
            let next = profile[(i + 1) % p_len];
            let t = next - prev;
            Vec2::new(t.y, -t.x).normalize_or(profile[i].normalize_or(Vec2::X))
        })
        .collect();
    let mut b = Builder::new();

    let pt = |seg: usize, pi: usize| -> Vec3 {
        let a = (seg % segs) as f32 * TAU / segs as f32;
        let radial = Vec3::new(a.cos(), 0.0, -a.sin());
        let p = profile[pi % p_len];
        radial * (r_major + p.x) + Vec3::Y * p.y
    };

    for s in 0..segs {
        let u0 = s as f32 / segs as f32;
        let u1 = (s + 1) as f32 / segs as f32;
        for pi in 0..p_len {
            let pj = (pi + 1) % p_len;
            let v0 = pi as f32 / p_len as f32;
            let v1 = (pi + 1) as f32 / p_len as f32;
            let a = pt(s, pi);
            let b_pt = pt(s + 1, pi);
            let c = pt(s + 1, pj);
            let d = pt(s, pj);
            if flat_facets {
                b.push_flat_tri(a, b_pt, c, [u0, v0], [u1, v0], [u1, v1]);
                b.push_flat_tri(a, c, d, [u0, v0], [u1, v1], [u0, v1]);
            } else {
                let n_of = |seg: usize, k: usize| -> Vec3 {
                    let ang = (seg % segs) as f32 * TAU / segs as f32;
                    let rad = Vec3::new(ang.cos(), 0.0, -ang.sin());
                    let pn = prof_normals[k % p_len];
                    (rad * pn.x + Vec3::Y * pn.y).normalize_or(rad)
                };
                let ia = b.push_vert(a, n_of(s, pi), [u0, v0]);
                let ib = b.push_vert(b_pt, n_of(s + 1, pi), [u1, v0]);
                let ic = b.push_vert(c, n_of(s + 1, pj), [u1, v1]);
                let id = b.push_vert(d, n_of(s, pj), [u0, v1]);
                b.indices.extend_from_slice(&[ia, ib, ic, ia, ic, id]);
            }
        }
    }
    b.finish()
}

/// Sloped trapezoidal armor plate / pauldron / breastplate / sight ramp centered at origin.
pub fn wedge_plate(width: f32, height: f32, depth: f32, top_width_ratio: f32, top_depth_ratio: f32, slant_z: f32) -> Mesh {
    let hw0 = (width * 0.5).max(1e-4);
    let hd0 = (depth * 0.5).max(1e-4);
    let hw1 = (hw0 * top_width_ratio).max(1e-4);
    let hd1 = (hd0 * top_depth_ratio).max(1e-4);
    let hy = (height * 0.5).max(1e-4);

    let r0 = vec![
        Vec3::new(hw0, -hy, hd0),
        Vec3::new(hw0, -hy, -hd0),
        Vec3::new(-hw0, -hy, -hd0),
        Vec3::new(-hw0, -hy, hd0),
    ];
    let r1 = vec![
        Vec3::new(hw1, hy, hd1 + slant_z),
        Vec3::new(hw1, hy, -hd1 + slant_z),
        Vec3::new(-hw1, hy, -hd1 + slant_z),
        Vec3::new(-hw1, hy, hd1 + slant_z),
    ];
    let mut b = Builder::new();
    stitch_rings(&mut b, &r0, &r1, 0.0, 1.0, true);
    cap_ring(&mut b, &r0, Vec3::new(0.0, -hy, 0.0), false);
    cap_ring(&mut b, &r1, Vec3::new(0.0, hy, slant_z), true);
    b.finish()
}

/// Multi-segment chamfered armor shield plate (convex V-shaped chestplate / visor / pauldron brow).
pub fn angled_shield(width: f32, height: f32, thickness: f32, fold_z: f32) -> Mesh {
    let hw = width * 0.5;
    let hh = height * 0.5;
    let ht = thickness * 0.5;
    let mut b = Builder::new();
    let fl = Vec3::new(-hw, -hh, ht);
    let fm = Vec3::new(0.0, -hh, ht - fold_z);
    let fr = Vec3::new(hw, -hh, ht);
    let bl = Vec3::new(-hw, -hh, -ht);
    let bm = Vec3::new(0.0, -hh, -ht - fold_z);
    let br = Vec3::new(hw, -hh, -ht);

    let t = Vec3::Y * height;
    let uv = Vec2::splat(2.0);
    b.push_flat_quad(bm, bl, bl + t, bm + t, uv);
    b.push_flat_quad(br, bm, bm + t, br + t, uv);
    b.push_flat_quad(fl, fm, fm + t, fl + t, uv);
    b.push_flat_quad(fm, fr, fr + t, fm + t, uv);
    b.push_flat_quad(bl, fl, fl + t, bl + t, uv);
    b.push_flat_quad(fr, br, br + t, fr + t, uv);
    b.push_flat_quad(fl + t, fm + t, bm + t, bl + t, uv);
    b.push_flat_quad(fm + t, fr + t, br + t, bm + t, uv);
    b.push_flat_quad(bl, bm, fm, fl, uv);
    b.push_flat_quad(bm, br, fr, fm, uv);
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid_outward_mesh(name: &str, m: &Mesh) {
        let pos = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap();
        let nrm = m.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap().as_float3().unwrap();
        let uvs = match m.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(bevy::mesh::VertexAttributeValues::Float32x2(v)) => v,
            _ => panic!("{name}: missing uv0"),
        };
        let Some(Indices::U32(idx)) = m.indices() else { panic!("{name}: missing u32 indices") };
        assert!(!pos.is_empty() && !idx.is_empty(), "{name}: empty mesh");
        assert_eq!(pos.len(), nrm.len(), "{name}: normal count mismatch");
        assert_eq!(pos.len(), uvs.len(), "{name}: uv count mismatch");
        assert_eq!(idx.len() % 3, 0, "{name}: non-triangle indices");
        for &n in nrm {
            let v = Vec3::from(n);
            assert!(v.is_finite() && (v.length() - 1.0).abs() < 1e-3, "{name}: non-unit normal {v}");
        }
        let mut signed_vol = 0.0f32;
        let mut aligned = 0usize;
        let mut total = 0usize;
        for tri in idx.chunks(3) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| Vec3::from(pos[i as usize]));
            signed_vol += a.dot(b.cross(c)) / 6.0;
            let geo = (b - a).cross(c - a);
            assert!(geo.length_squared() > 0.0, "{name}: zero-area triangle emitted");
            let avg_n = (Vec3::from(nrm[tri[0] as usize]) + Vec3::from(nrm[tri[1] as usize]) + Vec3::from(nrm[tri[2] as usize])).normalize_or_zero();
            if geo.normalize().dot(avg_n) > 0.0 {
                aligned += 1;
            }
            total += 1;
        }
        assert!(total > 0 && aligned == total, "{name}: {aligned}/{total} triangles match outward vertex normals");
        assert!(signed_vol > 0.0, "{name}: non-positive signed volume {signed_vol}");
    }

    #[test]
    fn all_sculpt_primitives_have_valid_ccw_winding_and_unit_normals() {
        assert_valid_outward_mesh("beveled_box", &beveled_box(Vec3::new(1.0, 0.8, 1.2), 0.12));
        assert_valid_outward_mesh("beveled_box_tiny_bevel", &beveled_box(Vec3::ONE, 0.000012));
        // Millimeter-scale shotgun vent tile: must emit all 64 watertight triangles with 0 dropouts!
        let tiny_vent = beveled_box(Vec3::new(0.008, 0.004, 0.012), 0.001);
        assert_valid_outward_mesh("beveled_box_mm_vent", &tiny_vent);
        assert_eq!(tiny_vent.indices().unwrap().len() / 3, 64, "mm-scale beveled box must keep all 64 triangles");
        assert_valid_outward_mesh("tapered_prism_flat", &tapered_prism(8, 0.5, 0.3, 1.4, true));
        assert_valid_outward_mesh("tapered_prism_smooth", &tapered_prism(10, 0.4, 0.25, 1.0, false));
        assert_valid_outward_mesh("tapered_prism_zero_tip", &tapered_prism(8, 0.4, 0.0, 1.0, true));
        assert_valid_outward_mesh("curved_horn", &curved_horn(0.14, 0.02, 0.9, 0.35, -0.15, 7, 8));
        assert_valid_outward_mesh("curved_horn_zero_tip", &curved_horn(0.14, 0.0, 0.9, 0.35, -0.15, 7, 7));
        // Strong sweeping bull/ram horn crossing abs(z) > 0.9: parallel transport keeps zero twist inversion!
        assert_valid_outward_mesh(
            "curved_horn_sweep_ram",
            &curved_horn_sweep(0.16, 0.01, Vec3::new(0.35, 0.15, -0.05), Vec3::new(0.55, 0.1, -0.45), Vec3::new(0.45, 0.55, -0.55), 10, 8),
        );
        assert_valid_outward_mesh("curved_blade", &curved_blade(0.8, 0.16, 0.04, 0.22, 6));
        assert_valid_outward_mesh("ridged_sphere_flat", &ridged_sphere(Vec3::splat(0.9), 10, 14, 0.09, 6.0, true));
        assert_valid_outward_mesh("ridged_sphere_smooth", &ridged_sphere(Vec3::new(0.8, 0.9, 0.85), 10, 14, 0.07, 5.0, false));
        assert_valid_outward_mesh("flared_dish", &flared_dish(0.08, 0.36, 0.16, 0.03, 12));
        assert_valid_outward_mesh("armored_ring_flat", &armored_ring(0.8, 0.08, 0.06, 16, true));
        assert_valid_outward_mesh("armored_ring_smooth", &armored_ring(0.8, 0.12, 0.04, 16, false));
        assert_valid_outward_mesh("wedge_plate", &wedge_plate(0.8, 0.4, 0.3, 0.65, 0.7, -0.08));
        assert_valid_outward_mesh("angled_shield", &angled_shield(0.6, 0.4, 0.08, 0.14));
    }
}
