//! Static mesh batching: bake many rigid `(mesh, transform)` parts sharing a
//! material into one combined [`Mesh`], so the level, the demons, the
//! viewmodels and the pickups each draw with a handful of entities instead of
//! hundreds.
//!
//! Baking is exact: positions go through the full affine transform, normals
//! through the inverse-transpose of its linear part (re-normalized), UVs are
//! copied verbatim (materials keep their own `uv_transform`), and triangle
//! winding is flipped for mirroring transforms so faces stay outward / CCW.
//! Triangles can be culled by their world-space normal (hidden faces).

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

/// Accumulates transformed triangles of rigid parts into one mesh.
#[derive(Debug, Default, Clone)]
pub struct MeshBatch {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    /// Source vertex -> batch vertex (scratch, reused between appends).
    remap: Vec<u32>,
}

impl MeshBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Append every triangle of `src` placed by `transform`.
    pub fn append(&mut self, src: &Mesh, transform: &Transform) {
        self.append_filtered(src, transform, |_| true);
    }

    /// Append the triangles of `src` placed by `transform` whose world-space
    /// face normal passes `keep` (e.g. drop the hidden bottom of a slab).
    pub fn append_filtered(&mut self, src: &Mesh, transform: &Transform, mut keep: impl FnMut(Vec3) -> bool) {
        debug_assert_eq!(src.primitive_topology(), PrimitiveTopology::TriangleList);
        let Some(VertexAttributeValues::Float32x3(pos)) = src.attribute(Mesh::ATTRIBUTE_POSITION) else {
            return;
        };
        let nrm = match src.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(n)) => Some(n),
            _ => None,
        };
        let uv = match src.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(VertexAttributeValues::Float32x2(u)) => Some(u),
            _ => None,
        };
        let affine = transform.compute_affine();
        let linear = Mat3::from(affine.matrix3);
        let det = linear.determinant();
        let normal_mat = (det.abs() > 1e-12).then(|| linear.inverse().transpose());
        let flip = det < 0.0;
        let use_src_normals = nrm.is_some() && normal_mat.is_some();

        if use_src_normals {
            self.remap.clear();
            self.remap.resize(pos.len(), u32::MAX);
        }
        let tri_count = match src.indices() {
            Some(i) => i.len() / 3,
            None => pos.len() / 3,
        };
        let index_at = |k: usize| -> usize {
            match src.indices() {
                Some(Indices::U16(v)) => v[k] as usize,
                Some(Indices::U32(v)) => v[k] as usize,
                None => k,
            }
        };
        let world_normal = |i: usize| -> Vec3 {
            match (nrm, normal_mat) {
                (Some(n), Some(nm)) => (nm * Vec3::from(n[i])).normalize_or_zero(),
                _ => Vec3::ZERO,
            }
        };
        for t in 0..tri_count {
            let tri = [index_at(3 * t), index_at(3 * t + 1), index_at(3 * t + 2)];
            let order = if flip { [tri[0], tri[2], tri[1]] } else { tri };
            let [a, b, c] = order.map(|i| affine.transform_point3(Vec3::from(pos[i])));
            let cross = (b - a).cross(c - a);
            if cross.length_squared() <= 1e-16 {
                continue;
            }
            let geo_n = cross.normalize();
            let face_n = if use_src_normals {
                let sum = world_normal(order[0]) + world_normal(order[1]) + world_normal(order[2]);
                sum.try_normalize().unwrap_or(geo_n)
            } else {
                geo_n
            };
            if !keep(face_n) {
                continue;
            }
            if use_src_normals {
                for i in order {
                    if self.remap[i] == u32::MAX {
                        self.remap[i] = self.positions.len() as u32;
                        self.positions.push(affine.transform_point3(Vec3::from(pos[i])).to_array());
                        let wn = world_normal(i).try_normalize().unwrap_or(geo_n);
                        self.normals.push(wn.to_array());
                        self.uvs.push(uv.map(|u| u[i]).unwrap_or([0.0, 0.0]));
                    }
                    self.indices.push(self.remap[i]);
                }
            } else {
                // Fallback flat path (no source normals or singular transform):
                // emit unshared vertices so adjacent flat faces keep distinct normals.
                let pts = [a, b, c];
                for (k, i) in order.into_iter().enumerate() {
                    let idx = self.positions.len() as u32;
                    self.positions.push(pts[k].to_array());
                    self.normals.push(geo_n.to_array());
                    self.uvs.push(uv.map(|u| u[i]).unwrap_or([0.0, 0.0]));
                    self.indices.push(idx);
                }
            }
        }
    }

    /// Finish into a renderable mesh (`None` when nothing was appended).
    pub fn build(self) -> Option<Mesh> {
        if self.is_empty() {
            return None;
        }
        Some(
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
                .with_inserted_indices(Indices::U32(self.indices)),
        )
    }
}

/// One [`MeshBatch`] per material, in first-use order.
#[derive(Debug, Default, Clone)]
pub struct MaterialBatches {
    batches: Vec<(Handle<StandardMaterial>, MeshBatch)>,
}

impl MaterialBatches {
    pub fn new() -> Self {
        Self::default()
    }

    /// The batch collecting parts drawn with `material`.
    pub fn batch(&mut self, material: &Handle<StandardMaterial>) -> &mut MeshBatch {
        let i = match self.batches.iter().position(|(m, _)| m.id() == material.id()) {
            Some(i) => i,
            None => {
                self.batches.push((material.clone(), MeshBatch::new()));
                self.batches.len() - 1
            }
        };
        &mut self.batches[i].1
    }

    pub fn add(&mut self, src: &Mesh, material: &Handle<StandardMaterial>, transform: Transform) {
        self.batch(material).append(src, &transform);
    }

    pub fn len(&self) -> usize {
        self.batches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Bake every non-empty batch: `(combined mesh, material)` pairs.
    pub fn finish(self, meshes: &mut Assets<Mesh>) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
        self.batches.into_iter().filter_map(|(mat, b)| b.build().map(|m| (meshes.add(m), mat))).collect()
    }
}

/// Rigid parts parented to different entities, batched per `(parent, material)`.
#[derive(Debug, Default)]
pub struct ParentedBatches {
    groups: Vec<(Entity, MaterialBatches)>,
}

impl ParentedBatches {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn part(&mut self, parent: Entity, src: &Mesh, material: &Handle<StandardMaterial>, t: Vec3, s: Vec3, r: Quat) {
        let i = match self.groups.iter().position(|(p, _)| *p == parent) {
            Some(i) => i,
            None => {
                self.groups.push((parent, MaterialBatches::new()));
                self.groups.len() - 1
            }
        };
        self.groups[i].1.add(src, material, Transform::from_translation(t).with_scale(s).with_rotation(r));
    }

    /// Bake: `(parent, combined mesh, material)` triples.
    pub fn finish(self, meshes: &mut Assets<Mesh>) -> Vec<(Entity, Handle<Mesh>, Handle<StandardMaterial>)> {
        self.groups
            .into_iter()
            .flat_map(|(p, b)| b.finish(meshes).into_iter().map(move |(m, mat)| (p, m, mat)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(m: &Mesh) -> Vec<[f32; 3]> {
        m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap().to_vec()
    }

    #[test]
    fn baked_cuboid_matches_transformed_source() {
        let src = Mesh::from(Cuboid::new(4.0, 1.0, 4.0));
        let tf = Transform::from_xyz(2.0, 3.0, -1.0).with_scale(Vec3::new(1.0, 7.6, 1.0)).with_rotation(Quat::from_rotation_y(0.3));
        let mut b = MeshBatch::new();
        b.append(&src, &tf);
        assert_eq!(b.triangle_count(), 12);
        let m = b.build().unwrap();
        let src_pos = positions(&src);
        let out = positions(&m);
        assert_eq!(out.len(), src_pos.len());
        for (o, s) in out.iter().zip(&src_pos) {
            let want = tf.transform_point(Vec3::from(*s));
            assert!(Vec3::from(*o).distance(want) < 1e-4);
        }
        // Normals stay unit length and point outward from the center.
        let n = m.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap().as_float3().unwrap();
        for (p, n) in out.iter().zip(n) {
            let n = Vec3::from(*n);
            assert!((n.length() - 1.0).abs() < 1e-4);
            assert!((Vec3::from(*p) - tf.translation).dot(n) > 0.0);
        }
        // UVs are copied untouched.
        let uv_src = src.attribute(Mesh::ATTRIBUTE_UV_0).unwrap();
        let uv_out = m.attribute(Mesh::ATTRIBUTE_UV_0).unwrap();
        assert_eq!(format!("{uv_src:?}"), format!("{uv_out:?}"));
    }

    #[test]
    fn winding_stays_outward_ccw_under_mirroring() {
        let src = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        for scale in [Vec3::ONE, Vec3::new(-1.0, 1.0, 1.0)] {
            let mut b = MeshBatch::new();
            b.append(&src, &Transform::from_scale(scale));
            let m = b.build().unwrap();
            let p = positions(&m);
            let n = m.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap().as_float3().unwrap().to_vec();
            let Some(Indices::U32(idx)) = m.indices() else { panic!() };
            for t in idx.chunks(3) {
                let [a, b2, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(p[i as usize]));
                let geo = (b2 - a).cross(c - a);
                assert!(geo.dot(Vec3::from(n[t[0] as usize])) > 0.0, "scale {scale}");
            }
        }
    }

    #[test]
    fn culling_by_normal_drops_faces() {
        let src = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        let mut b = MeshBatch::new();
        b.append_filtered(&src, &Transform::IDENTITY, |n| n.y > -0.5);
        assert_eq!(b.triangle_count(), 10);
        assert_eq!(b.vertex_count(), 20);
    }

    #[test]
    fn material_batches_group_by_handle() {
        let mut mats = Assets::<StandardMaterial>::default();
        let mut meshes = Assets::<Mesh>::default();
        let (a, c) = (mats.add(StandardMaterial::default()), mats.add(StandardMaterial::default()));
        let src = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        let mut mb = MaterialBatches::new();
        for i in 0..10 {
            mb.add(&src, if i % 2 == 0 { &a } else { &c }, Transform::from_xyz(i as f32, 0.0, 0.0));
        }
        assert_eq!(mb.len(), 2);
        let out = mb.finish(&mut meshes);
        assert_eq!(out.len(), 2);
        assert_eq!(meshes.len(), 2);
    }

    #[test]
    fn mirrored_mesh_without_normals_keeps_outward_flat_normals() {
        // Two perpendicular triangles sharing an edge, with no ATTRIBUTE_NORMAL.
        let src = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            )
            .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        let mut b = MeshBatch::new();
        b.append(&src, &Transform::from_scale(Vec3::new(-1.0, 1.0, 1.0)));
        let m = b.build().unwrap();
        let p = positions(&m);
        let n = m.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap().as_float3().unwrap();
        let Some(Indices::U32(idx)) = m.indices() else { panic!() };
        assert_eq!(idx.len(), 6);
        // Distinct flat normals for the two perpendicular faces.
        let n0 = Vec3::from(n[idx[0] as usize]);
        let n1 = Vec3::from(n[idx[3] as usize]);
        assert!(n0.dot(n1).abs() < 1e-4, "shared vertices must not overwrite flat normals: {n0} vs {n1}");
        for t in idx.chunks(3) {
            let [a, b2, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(p[i as usize]));
            let geo = (b2 - a).cross(c - a).normalize();
            for &vi in t {
                let vn = Vec3::from(n[vi as usize]);
                assert!(geo.dot(vn) > 0.999, "fallback normal {vn} must match mirrored winding {geo}");
            }
        }
    }

    #[test]
    fn singular_transform_skips_zero_area_and_keeps_valid_normals() {
        let src = Mesh::from(Cuboid::new(2.0, 2.0, 2.0));
        let mut b = MeshBatch::new();
        // Squashing Y to 0 collapses the 4 side walls (8 tris) and keeps top + bottom (4 tris).
        b.append(&src, &Transform::from_scale(Vec3::new(1.0, 0.0, 1.0)));
        assert_eq!(b.triangle_count(), 4);
        let m = b.build().unwrap();
        let n = m.attribute(Mesh::ATTRIBUTE_NORMAL).unwrap().as_float3().unwrap();
        for &vn in n {
            let v = Vec3::from(vn);
            assert!(v.is_finite() && (v.length() - 1.0).abs() < 1e-4, "invalid normal {v}");
        }
    }
}
