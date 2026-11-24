//! Optional GPU ray-traced lighting through Bevy Solari (`--features raytracing`).
//!
//! Solari traces hardware ray queries against a BLAS/TLAS of the world and
//! computes direct + indirect (ReSTIR GI) lighting from the sun and from every
//! emissive mesh. Lamps, lava, barrel cores, plasma bolts and demon eyes become
//! real area lights that cast soft ray-traced shadows and bounce light.
//!
//! How the raster game is adapted when ray tracing is active:
//! - **Player camera:** gets [`SolariLighting`], HDR, `Msaa::Off`, a storage
//!   main texture and unit exposure (Solari weights emissives by exposure,
//!   while the raster materials leave them unweighted, so exposure 1.0 keeps
//!   their look).
//! - **Light units:** the sun and the camera's ambient term are scaled by the
//!   raster exposure so lit surfaces keep their raster brightness at exposure
//!   1.0. Solari ignores point lights, so world point lights move to a render
//!   layer no camera draws (the viewmodel's own lights on the overlay layer are
//!   untouched). Ceiling lamp emission is boosted to stand in for the lamp
//!   point lights they replace, and the clear color becomes a sky environment
//!   map so the open atrium roof lets light in (Solari has no ambient term).
//! - **Denoising:** ReSTIR temporal + spatial reuse is on (there is no
//!   denoiser outside NVIDIA DLSS-RR), so expect some grain while it converges.
//! - **Scene:** opaque `StandardMaterial` meshes on the world layer get a
//!   [`RaytracingMesh3d`] while visible, and their materials switch to the
//!   deferred path (Solari shades the G-buffer). Everything else, including
//!   the viewmodel and translucent FX, keeps the forward path.
//!
//! The GPU needs ray-query + binding-array support (Vulkan / DX12 on RTX,
//! RDNA2+, Arc). Without it, or with `SEGNALE_RT=off`, the game renders exactly
//! as without the feature.

use std::collections::HashSet;

use bevy::camera::Exposure;
use bevy::camera::visibility::RenderLayers;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

/// Environment switch: `SEGNALE_RT=off` (or `0`) disables ray tracing in a
/// `raytracing` build.
pub const RT_ENV: &str = "SEGNALE_RT";
/// Tuning override for [`LAMP_RT_GAIN`].
pub const RT_LAMP_ENV: &str = "SEGNALE_RT_LAMP";
/// Tuning override for [`SKY_RT_NITS`].
pub const RT_SKY_ENV: &str = "SEGNALE_RT_SKY";

/// Positive finite `f32` from env var `key`, else `default`.
#[cfg_attr(not(feature = "raytracing"), allow(dead_code))]
fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(default)
}

/// EV100 at which `Exposure::exposure()` is 1.0 (`log2(1 / 1.2)`).
pub const UNITY_EV100: f32 = -0.263_034_4;

/// Extra emission for ceiling lamps: their 0.9 x 0.4 m panels must carry the
/// ~420 klm the raster lamp point lights emitted.
pub const LAMP_RT_GAIN: f32 = 30.0;

/// Radiance of the sky (the clear color) seen by rays leaving the level.
pub const SKY_RT_NITS: f32 = 4.0;

/// A render layer no camera draws; world point lights park here under RT.
pub const RT_HIDDEN_LAYER: usize = 31;

/// Solari's limit on triangles per emissive mesh instance (it panics above it).
pub const MAX_EMISSIVE_TRIANGLES: usize = 65_535;

/// Whether Solari ray-traced lighting runs this session.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaytracingActive(pub bool);

/// Run condition: ray tracing is active.
pub fn rt_active(active: Option<Res<RaytracingActive>>) -> bool {
    active.is_some_and(|a| a.0)
}

/// `SEGNALE_RT` value -> requested (anything but `off`/`0`/`false`).
pub fn rt_env_enabled(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some(v) if v.eq_ignore_ascii_case("off") || v == "0" || v.eq_ignore_ascii_case("false"))
}

/// Exposure the raster light intensities are tuned for (Bevy's default camera).
pub fn raster_exposure() -> f32 {
    Exposure::default().exposure()
}

/// Make `mesh` loadable by Solari in place: u32 triangle-list indices and
/// exactly `{POSITION, NORMAL, UV_0, TANGENT}`. Missing UVs / tangents are
/// filled with zeros (the game uses no normal maps, so tangents are never
/// read). Returns `false` for meshes that cannot be converted without
/// changing their raster look (other topologies or extra attributes).
pub fn prepare_rt_mesh(mesh: &mut Mesh) -> bool {
    if !rt_mesh_convertible(mesh) {
        return false;
    }
    let n = mesh.count_vertices();
    if !mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; n]);
    }
    if !mesh.contains_attribute(Mesh::ATTRIBUTE_TANGENT) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[0.0f32, 0.0, 0.0, 0.0]; n]);
    }
    match mesh.indices() {
        Some(Indices::U32(_)) => {}
        Some(Indices::U16(v)) => {
            let v: Vec<u32> = v.iter().map(|&i| i as u32).collect();
            mesh.insert_indices(Indices::U32(v));
        }
        None => mesh.insert_indices(Indices::U32((0..n as u32).collect())),
    }
    mesh.enable_raytracing = true;
    rt_mesh_ready(mesh)
}

/// Whether [`prepare_rt_mesh`] can convert `mesh`.
pub fn rt_mesh_convertible(mesh: &Mesh) -> bool {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return false;
    }
    let has_pos = matches!(mesh.attribute(Mesh::ATTRIBUTE_POSITION), Some(VertexAttributeValues::Float32x3(_)));
    let has_nrm = matches!(mesh.attribute(Mesh::ATTRIBUTE_NORMAL), Some(VertexAttributeValues::Float32x3(_)));
    let allowed = [Mesh::ATTRIBUTE_POSITION.id, Mesh::ATTRIBUTE_NORMAL.id, Mesh::ATTRIBUTE_UV_0.id, Mesh::ATTRIBUTE_TANGENT.id];
    has_pos && has_nrm && mesh.count_vertices() > 0 && mesh.attributes().all(|(a, _)| allowed.contains(&a.id))
}

/// Whether `mesh` already satisfies Solari's layout.
pub fn rt_mesh_ready(mesh: &Mesh) -> bool {
    rt_mesh_convertible(mesh)
        && mesh.enable_raytracing
        && mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0)
        && mesh.contains_attribute(Mesh::ATTRIBUTE_TANGENT)
        && matches!(mesh.indices(), Some(Indices::U32(i)) if !i.is_empty() && i.len() % 3 == 0)
}

/// Triangle count of a (prepared) triangle-list mesh.
pub fn triangle_count(mesh: &Mesh) -> usize {
    mesh.indices().map_or(mesh.count_vertices(), |i| i.len()) / 3
}

/// Whether an entity on `layers` is part of the world (not the viewmodel overlay).
pub fn on_world_layer(layers: Option<&RenderLayers>) -> bool {
    layers.is_none_or(|l| l.intersects(&RenderLayers::layer(0)))
}

/// Entity whose mesh was checked for the ray-tracing scene.
#[derive(Component)]
pub struct RtChecked;

/// Entity that belongs in the ray-tracing scene while visible.
#[derive(Component)]
pub struct RtCandidate;

/// Material bookkeeping for the deferred / forward split.
#[derive(Resource, Default)]
#[cfg_attr(not(feature = "raytracing"), allow(dead_code))]
pub struct RtMaterials {
    /// Materials switched to the deferred (ray-traced) path.
    deferred: HashSet<AssetId<StandardMaterial>>,
    /// Materials the viewmodel overlay uses: they must stay forward, since the
    /// overlay camera has no deferred prepass and would drop them.
    forward_only: HashSet<AssetId<StandardMaterial>>,
}

pub struct RaytracingPlugin;

impl Plugin for RaytracingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RaytracingActive>().init_resource::<RtMaterials>();
        #[cfg(feature = "raytracing")]
        solari::build(app);
    }

    fn finish(&self, _app: &mut App) {
        #[cfg(feature = "raytracing")]
        solari::finish(_app);
    }
}

#[cfg(feature = "raytracing")]
mod solari {
    use super::*;
    use crate::assets_gen::GameAssets;
    use crate::player::PlayerCamera;
    use crate::weapons::OverlayCamera;
    use bevy::camera::visibility::VisibilitySystems;
    use bevy::camera::{CameraMainTextureUsages, Hdr};
    use bevy::material::OpaqueRendererMethod;
    use bevy::pbr::DefaultOpaqueRendererMethod;
    use bevy::render::render_resource::TextureUsages;
    use bevy::render::renderer::RenderDevice;
    use bevy::render::view::Msaa;
    use bevy::render::RenderApp;
    use bevy::solari::prelude::{RaytracingMesh3d, SolariLighting, SolariPlugins};

    /// Solari was added this session (requested and a renderer exists).
    #[derive(Resource)]
    struct SolariAdded;

    pub fn build(app: &mut App) {
        if !rt_env_enabled(std::env::var(RT_ENV).ok().as_deref()) {
            info!("raytracing: disabled by {RT_ENV}");
            return;
        }
        if app.get_sub_app(RenderApp).is_none() {
            return; // headless
        }
        app.add_plugins(SolariPlugins)
            // Solari makes every opaque material deferred; the overlay and the
            // menus have no deferred prepass, so default to forward and switch
            // only ray-traced world materials.
            .insert_resource(DefaultOpaqueRendererMethod::forward())
            .insert_resource(SolariAdded)
            .add_systems(
                PostUpdate,
                (
                    (setup_rt_cameras, convert_raster_lights, boost_lamps),
                    (keep_overlay_materials_forward, recheck_changed, register_rt_meshes, sync_rt_visibility)
                        .chain()
                        .after(VisibilitySystems::VisibilityPropagate),
                )
                    .run_if(rt_active),
            );
    }

    pub fn finish(app: &mut App) {
        if !app.world().contains_resource::<SolariAdded>() {
            return;
        }
        let Some(features) = app.get_sub_app(RenderApp).and_then(|r| r.world().get_resource::<RenderDevice>()).map(|d| d.features()) else {
            return;
        };
        let required = SolariPlugins::required_wgpu_features();
        if features.contains(required) {
            info!("raytracing: Solari ray-traced lighting ACTIVE");
            app.insert_resource(RaytracingActive(true));
        } else {
            warn!("raytracing: GPU lacks {:?}; falling back to raster lighting", required.difference(features));
        }
    }

    #[allow(clippy::type_complexity)]
    fn setup_rt_cameras(
        mut commands: Commands,
        ambient: Option<Res<GlobalAmbientLight>>,
        clear: Option<Res<ClearColor>>,
        mut images: ResMut<Assets<Image>>,
        players: Query<Entity, Added<PlayerCamera>>,
        overlays: Query<Entity, Added<OverlayCamera>>,
    ) {
        let usages = CameraMainTextureUsages::default().with(TextureUsages::STORAGE_BINDING);
        let k = raster_exposure();
        for cam in &players {
            let (color, brightness) = ambient.as_deref().map_or((Color::WHITE, 0.0), |a| (a.color, a.brightness));
            commands.entity(cam).insert((
                // No denoiser outside DLSS-RR: ReSTIR's temporal + spatial
                // reuse is what keeps the 1-spp output from sparkling.
                SolariLighting { restir: true, ..default() },
                Msaa::Off,
                usages,
                Exposure { ev100: UNITY_EV100 },
                // Only forward leftovers (translucent FX) read ambient here.
                AmbientLight { color, brightness: brightness * k, ..default() },
                // Rays escaping the atrium roof see the Mars sky.
                EnvironmentMapLight {
                    intensity: env_f32(RT_SKY_ENV, SKY_RT_NITS),
                    ..EnvironmentMapLight::solid_color(&mut images, clear.as_deref().map_or(Color::BLACK, |c| c.0))
                },
            ));
        }
        // The viewmodel overlay draws into the same HDR main texture.
        for cam in &overlays {
            commands.entity(cam).insert((Hdr, Msaa::Off, usages));
        }
    }

    /// Sun to unit-exposure units; world point / spot lights off (Solari only
    /// traces the sun and emissive meshes, so they would only light the
    /// forward leftovers, 1000x too bright at unit exposure).
    #[allow(clippy::type_complexity)]
    fn convert_raster_lights(
        mut commands: Commands,
        mut suns: Query<&mut DirectionalLight, Added<DirectionalLight>>,
        points: Query<Entity, (Or<(Added<PointLight>, Added<SpotLight>)>, Without<RenderLayers>)>,
    ) {
        let k = raster_exposure();
        for mut sun in &mut suns {
            sun.illuminance *= k;
        }
        for e in &points {
            commands.entity(e).insert(RenderLayers::layer(RT_HIDDEN_LAYER));
        }
    }

    fn boost_lamps(assets: Option<Res<GameAssets>>, mut materials: ResMut<Assets<StandardMaterial>>, mut done: Local<bool>) {
        let Some(assets) = assets else { return };
        if *done {
            return;
        }
        *done = true;
        let mut lamps: Vec<_> = assets.themes.iter().map(|t| t.lamp.id()).collect();
        lamps.push(assets.lamp.id());
        lamps.sort();
        lamps.dedup();
        for id in lamps {
            if let Some(mut m) = materials.get_mut(id) {
                m.emissive *= env_f32(RT_LAMP_ENV, LAMP_RT_GAIN);
            }
        }
    }

    /// Materials seen on the overlay never go (or stay) deferred.
    fn keep_overlay_materials_forward(
        overlay: Query<(&MeshMaterial3d<StandardMaterial>, &RenderLayers)>,
        mut state: ResMut<RtMaterials>,
        mut materials: ResMut<Assets<StandardMaterial>>,
    ) {
        for (mat, layers) in &overlay {
            if on_world_layer(Some(layers)) || !state.forward_only.insert(mat.id()) {
                continue;
            }
            if state.deferred.remove(&mat.id())
                && let Some(mut m) = materials.get_mut(mat.id())
            {
                m.opaque_render_method = OpaqueRendererMethod::Forward;
            }
        }
    }

    /// A swapped mesh or material is re-evaluated from scratch.
    #[allow(clippy::type_complexity)]
    fn recheck_changed(
        mut commands: Commands,
        changed: Query<Entity, (With<RtChecked>, Or<(Changed<Mesh3d>, Changed<MeshMaterial3d<StandardMaterial>>)>)>,
    ) {
        for e in &changed {
            commands.entity(e).remove::<(RtChecked, RtCandidate, RaytracingMesh3d)>();
        }
    }

    #[allow(clippy::type_complexity)]
    fn register_rt_meshes(
        mut commands: Commands,
        new: Query<(Entity, &Mesh3d, &MeshMaterial3d<StandardMaterial>, Option<&RenderLayers>, &InheritedVisibility), Without<RtChecked>>,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
        mut state: ResMut<RtMaterials>,
    ) {
        for (e, mesh, mat, layers, vis) in &new {
            let (Some(_), Some(m)) = (meshes.get(mesh.id()), materials.get(mat.id())) else {
                continue; // not loaded yet: retry next frame
            };
            commands.entity(e).insert(RtChecked);
            if !on_world_layer(layers) || m.alpha_mode != AlphaMode::Opaque || state.forward_only.contains(&mat.id()) {
                continue;
            }
            let emissive = m.emissive != LinearRgba::BLACK || m.emissive_texture.is_some();
            let ready = rt_mesh_ready(meshes.get(mesh.id()).unwrap());
            if !ready && !meshes.get_mut(mesh.id()).is_some_and(|mut m| prepare_rt_mesh(&mut m)) {
                continue;
            }
            if emissive && meshes.get(mesh.id()).is_some_and(|m| triangle_count(m) > MAX_EMISSIVE_TRIANGLES) {
                continue;
            }
            if state.deferred.insert(mat.id())
                && let Some(mut m) = materials.get_mut(mat.id())
            {
                m.opaque_render_method = OpaqueRendererMethod::Deferred;
            }
            commands.entity(e).insert(RtCandidate);
            if vis.get() {
                commands.entity(e).insert(RaytracingMesh3d(mesh.0.clone()));
            }
        }
    }

    /// Solari ignores `Visibility`: hidden meshes leave the ray-traced scene.
    #[allow(clippy::type_complexity)]
    fn sync_rt_visibility(
        mut commands: Commands,
        q: Query<(Entity, &Mesh3d, &InheritedVisibility, Has<RaytracingMesh3d>), (With<RtCandidate>, Changed<InheritedVisibility>)>,
    ) {
        for (e, mesh, vis, traced) in &q {
            match (vis.get(), traced) {
                (true, false) => {
                    commands.entity(e).insert(RaytracingMesh3d(mesh.0.clone()));
                }
                (false, true) => {
                    commands.entity(e).remove::<RaytracingMesh3d>();
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;

    #[test]
    fn env_switch() {
        assert!(rt_env_enabled(None));
        assert!(rt_env_enabled(Some("on")));
        for off in ["off", "OFF", "0", "false", " off "] {
            assert!(!rt_env_enabled(Some(off)), "{off}");
        }
    }

    #[test]
    fn unity_ev100_gives_unit_exposure() {
        assert!((Exposure { ev100: UNITY_EV100 }.exposure() - 1.0).abs() < 1e-5);
        assert!(raster_exposure() < 0.01);
    }

    #[test]
    fn primitive_meshes_become_solari_compatible() {
        for mut m in [Mesh::from(Cuboid::new(1.0, 2.0, 3.0)), Mesh::from(Sphere::new(0.5)), Mesh::from(Cylinder::new(0.3, 1.0))] {
            let pos_before = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap().to_vec();
            assert!(prepare_rt_mesh(&mut m));
            assert!(rt_mesh_ready(&m));
            assert_eq!(m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap(), pos_before.as_slice());
        }
    }

    #[test]
    fn batched_meshes_are_ready_after_prepare() {
        let mut b = crate::mesh_batch::MeshBatch::new();
        b.append(&Mesh::from(Cuboid::new(1.0, 1.0, 1.0)), &Transform::IDENTITY);
        let mut m = b.build().unwrap();
        assert!(!rt_mesh_ready(&m), "no tangents yet");
        assert!(prepare_rt_mesh(&mut m));
        assert_eq!(triangle_count(&m), 12);
    }

    #[test]
    fn u16_and_unindexed_meshes_get_u32_indices() {
        let tri = || {
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; 3])
        };
        let mut a = tri().with_inserted_indices(Indices::U16(vec![0, 1, 2]));
        let mut b = tri();
        for m in [&mut a, &mut b] {
            assert!(prepare_rt_mesh(m));
            assert!(matches!(m.indices(), Some(Indices::U32(i)) if i == &vec![0, 1, 2]));
        }
    }

    #[test]
    fn meshes_with_extra_attributes_or_lines_are_rejected() {
        let mut colored = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        let n = colored.count_vertices();
        colored.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32, 1.0, 1.0, 1.0]; n]);
        assert!(!prepare_rt_mesh(&mut colored));
        assert!(colored.attribute(Mesh::ATTRIBUTE_TANGENT).is_none(), "rejected meshes stay untouched");

        let mut lines = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0]])
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 2]);
        assert!(!prepare_rt_mesh(&mut lines));
    }

    #[test]
    fn overlay_layer_is_not_world() {
        assert!(on_world_layer(None));
        assert!(on_world_layer(Some(&RenderLayers::layer(0))));
        assert!(!on_world_layer(Some(&crate::fx::overlay_layer())));
        assert!(!on_world_layer(Some(&RenderLayers::layer(RT_HIDDEN_LAYER))));
    }
}
