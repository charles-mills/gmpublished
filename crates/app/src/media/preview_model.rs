use gmpublished_domain::math::Vec3;
use std::sync::Arc;

use iced::widget::image;

use crate::bridge::archive::{PreviewArchiveSource, PreviewArchiveSourceError};
use crate::bridge::materials::{RenderMode, ResolvedTexture};
use crate::bridge::tasks::ScheduleError;
use crate::generation::Generation;
#[path = "model_lighting.rs"]
mod model_lighting;
pub use gmpublished_domain::scene::map::{
    MapDoorClass, MapDoorMotion, MapMeshIndexRange, MapMeshVisibility, MapTrace, MapVisibility,
    MapVisibilityBucket, MapWalkCollision,
};
pub use model_lighting::{
    ModelLightKind, ModelLightingContext, ModelLightingHistory, ModelLightingSceneInputs,
    ModelLocalLight, placement_sample_point, placement_static_bounds,
};
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreviewLoadError {
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error(transparent)]
    Archive(#[from] PreviewArchiveSourceError),
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelVertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: [f32; 2],
    pub lightmap_uv: [f32; 2],
    pub lightmap_bump_step: f32,
    pub color: Vec3,
    pub blend_alpha: f32,
    /// Authored vertex opacity, separate from WorldVertexTransition blending.
    pub opacity: f32,
    /// Explicit linear illumination, independent of authored vertex colour.
    pub illumination: Vec3,
    /// Bit 0 selects separate linear illumination and raw instance RGB;
    /// bit 1 identifies the static-prop draw context's modulation overrides.
    /// Bit 2 enables a directional lighting context; bit 3 marks a model draw
    /// whose per-instance opacity replaces the material alpha constant.
    pub lighting_mode: u32,
    pub tangent_s: Vec3,
    pub tangent_t: Vec3,
    pub tangent_sign: f32,
    pub lighting_context: u32,
}

impl From<&vformats::mdl::ModelVertex> for ModelVertex {
    fn from(vertex: &vformats::mdl::ModelVertex) -> Self {
        Self {
            position: Vec3::from(vertex.position),
            normal: Vec3::from(vertex.normal),
            tangent_s: Vec3::from(vertex.tangent_s),
            tangent_t: Vec3::from(vertex.tangent_t),
            tangent_sign: vertex.tangent_sign,
            lighting_context: 0,
            uv: vertex.uv,
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MeshGeometry {
    #[default]
    Brush,
    Model,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshData {
    pub geometry: MeshGeometry,
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub material_index: usize,
    pub bodygroup: usize,
    pub bodygroup_choice: usize,
}

impl From<&vformats::mdl::MeshData> for MeshData {
    fn from(mesh: &vformats::mdl::MeshData) -> Self {
        Self {
            geometry: MeshGeometry::Model,
            vertices: mesh.vertices.iter().map(ModelVertex::from).collect(),
            indices: mesh.indices.clone(),
            material_index: mesh.material_index,
            bodygroup: mesh.bodygroup,
            bodygroup_choice: mesh.bodygroup_choice,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelData {
    pub mdl_flags: u32,
    pub vertex_compression: vformats::mdl::VertexCompression,
    pub source_frames: Vec<vformats::mdl::ModelSourceVertexFrame>,
    pub illumination_position: Vec3,
    pub hull_min: Vec3,
    pub hull_max: Vec3,
    pub illumination_attachment_index: Option<usize>,
    pub illumination_attachment_position: Option<Vec3>,
    pub lighting: Option<Arc<ModelLightingData>>,
    pub meshes: Vec<MeshData>,
    pub material_names: Vec<String>,
    pub material_dirs: Vec<String>,
    pub skin_tables: Vec<Vec<u16>>,
    pub bodygroups: Vec<usize>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub bone_count: u32,
    pub sequence_count: u32,
    pub vertex_count: u32,
    pub triangle_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLightingData {
    pub checksum: u32,
    pub compatible: bool,
    pub compiler_layout_verified: bool,
    pub compiler_aliases: Vec<(
        vformats::mdl::ModelLightingVertex,
        vformats::mdl::ModelLightingVertex,
    )>,
    pub meshes: Vec<vformats::mdl::ModelLightingMesh>,
    pub vertices: Vec<Vec<vformats::mdl::ModelLightingVertex>>,
}

impl From<vformats::mdl::ModelData> for ModelData {
    fn from(model: vformats::mdl::ModelData) -> Self {
        Self {
            mdl_flags: model.mdl_flags,
            illumination_position: Vec3::from(model.illumination_position),
            hull_min: Vec3::from(model.hull_min),
            hull_max: Vec3::from(model.hull_max),
            illumination_attachment_index: model.illumination_attachment_index,
            illumination_attachment_position: model
                .illumination_attachment_position
                .map(Vec3::from),
            vertex_compression: model.vertex_compression,
            source_frames: model.source_frames,
            lighting: Some(Arc::new(ModelLightingData {
                checksum: model.checksum,
                compatible: model.lighting_compatible,
                compiler_layout_verified: false,
                compiler_aliases: Vec::new(),
                meshes: model.lighting_meshes,
                vertices: model
                    .meshes
                    .iter()
                    .map(|mesh| mesh.vertex_lighting.clone())
                    .collect(),
            })),
            meshes: model.meshes.iter().map(MeshData::from).collect(),
            material_names: model.material_names,
            material_dirs: model.material_dirs,
            skin_tables: model.skin_tables,
            bodygroups: model.bodygroups,
            bounds_min: Vec3::from(model.bounds_min),
            bounds_max: Vec3::from(model.bounds_max),
            bone_count: model.bone_count,
            sequence_count: model.sequence_count,
            vertex_count: model.vertex_count,
            triangle_count: model.triangle_count,
        }
    }
}

pub const PHY_DEBUG_MATERIAL_NAME: &str = "__debug/phy_collision";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewRequest {
    pub(crate) request_id: Generation,
    pub(crate) archive: Arc<PreviewArchiveSource>,
    pub(crate) entry_path: String,
    pub(crate) display_name: String,
    pub(crate) size_bytes: u64,
    pub(crate) crc32: u32,
    pub(crate) bypass_size_limits: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewData {
    pub(crate) entry_path: String,
    pub(crate) display_name: String,
    pub(crate) size_bytes: u64,
    pub(crate) crc32: u32,
    pub(crate) related_preview: Option<RelatedPreviewTarget>,
    pub(crate) content: PreviewContent,
}

impl PreviewData {
    pub(crate) fn content_id(&self) -> u64 {
        u64::from(self.crc32) | (self.size_bytes << 32)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedPreviewTarget {
    pub(crate) entry_path: String,
    pub(crate) kind: RelatedPreviewKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelatedPreviewKind {
    Material,
    Texture,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PreviewContent {
    Code {
        lines: Vec<CodeLine>,
        truncated: bool,
    },
    Image {
        handle: image::Handle,
        width: u32,
        height: u32,
    },
    Font {
        handle: image::Handle,
        family: String,
    },
    Audio {
        bytes: Arc<[u8]>,
        duration_secs: Option<f32>,
    },
    Model(Arc<ModelPreview>),
    Map {
        scene: Arc<MapPreview>,
        stats: MapStats,
        fog: Option<MapFog>,
        sky_camera: Option<MapSkyCamera>,
        spawn: Option<MapSpawn>,
    },
    Particle(Arc<ParticlePreview>),
    Info {
        reason: InfoReason,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticlePreview {
    pub(crate) file: gmpublished_domain::scene::pcf::PcfFile,
    pub(crate) systems: Vec<ParticleSystemInfo>,
    pub(crate) materials: Vec<ParticleMaterialSlot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticleSystemInfo {
    pub(crate) name: String,
    pub(crate) coverage: Vec<gmpublished_domain::particles::CoverageEntry>,
    pub(crate) highest_control_point: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticleMaterialSlot {
    pub(crate) name: String,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) additive: bool,
    pub(crate) sheet: Option<Arc<vformats::vtf::SpriteSheet>>,
}

/// PCF material references are inconsistent ("effects\\spark.vmt",
/// "particle/foo"); loader keys and renderer lookups share this form.
pub fn normalize_particle_material(name: &str) -> String {
    gmpublished_domain::particles::normalize_material_name(name)
}

#[cfg(test)]
mod particle_material_tests {
    use super::normalize_particle_material;

    #[test]
    fn normalizes_every_pcf_material_spelling() {
        for (raw, expected) in [
            ("effects\\Gunshipmuzzle.VMT", "effects/gunshipmuzzle"),
            ("materials/particle/foo.vmt", "particle/foo"),
            ("particle/particle_glow_04", "particle/particle_glow_04"),
            (" vgui/white ", "vgui/white"),
        ] {
            assert_eq!(normalize_particle_material(raw), expected);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderScene {
    pub(crate) meshes: Vec<MeshData>,
    pub(crate) materials: Vec<MaterialSlot>,
    pub(crate) phy_debug_meshes: Vec<MeshData>,
    pub(crate) stats: ModelStats,
    pub(crate) bounds_min: Vec3,
    pub(crate) bounds_max: Vec3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelPreview {
    pub(crate) scene: Arc<RenderScene>,
    pub(crate) skin_tables: Vec<Vec<u16>>,
    pub(crate) bodygroups: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapPreview {
    pub(crate) water_volumes: Option<gmpublished_domain::scene::map::MapWaterVolumes>,
    pub(crate) entity_meshes: Vec<bool>,
    pub(crate) skybox_entity_meshes: Vec<bool>,
    pub(crate) scene: Arc<RenderScene>,
    pub(crate) mesh_visibility: Vec<MapMeshVisibility>,
    pub(crate) map_skybox_meshes: Vec<MeshData>,
    pub(crate) model_lighting: Vec<ModelLightingContext>,
    pub(crate) model_lighting_inputs: Option<Arc<ModelLightingSceneInputs>>,
    pub(crate) lightmap: Option<LightmapSlot>,
    pub(crate) skybox: Option<Skybox>,
    pub(crate) suns: Vec<gmpublished_domain::scene::map::MapSun>,
    pub(crate) cubemap_probes: Vec<gmpublished_domain::scene::map::MapCubemapProbe>,
    pub(crate) sun_material_slots: Vec<[usize; 2]>,
    pub(crate) detail_sprites: Vec<DetailSprite>,
    pub(crate) map_skybox_detail_sprites: Vec<DetailSprite>,
    pub(crate) overlays: Vec<OverlayPrimitive>,
    pub(crate) map_skybox_overlays: Vec<OverlayPrimitive>,
    pub(crate) doors: Vec<DoorInstance>,
    pub(crate) visibility: Option<MapVisibility>,
    pub(crate) walk_collision: Option<MapWalkCollision>,
}

impl MapPreview {
    pub(crate) fn immersion_material(
        &self,
        eye: Vec3,
    ) -> Option<&crate::bridge::materials::WaterMaterial> {
        let sample = self.water_volumes.as_ref()?.at(eye)?;
        let above = self
            .scene
            .materials
            .iter()
            .find(|slot| slot.name == sample.body.material_name)?
            .shader
            .water()?;
        above
            .below_material
            .as_ref()
            .and_then(|name| {
                self.scene
                    .materials
                    .iter()
                    .find(|slot| &slot.name == name)?
                    .shader
                    .water()
            })
            .or(Some(above))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DoorInstance {
    pub(crate) brush_collision: Option<gmpublished_domain::scene::map::MapBrushCollision>,
    pub(crate) class: MapDoorClass,
    pub(crate) origin: Vec3,
    /// Source QAngle order: pitch, yaw, roll.
    pub(crate) angles: Vec3,
    pub(crate) local_bounds_min: Vec3,
    pub(crate) local_bounds_max: Vec3,
    pub(crate) visibility: MapVisibilityBucket,
    pub(crate) initial_progress: f32,
    /// Seconds an opened door waits before closing itself, or `None` for a
    /// door that stays open until triggered again.
    pub(crate) auto_close_after: Option<f32>,
    pub(crate) motion: MapDoorMotion,
    pub(crate) sounds: DoorSounds,
    pub(crate) meshes: Vec<MeshData>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoorSounds {
    pub(crate) move_sound: Option<DoorSound>,
    pub(crate) stop_sound: Option<DoorSound>,
    pub(crate) open_sound: Option<DoorSound>,
    pub(crate) close_sound: Option<DoorSound>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DoorSound {
    pub(crate) reference: String,
    pub(crate) sound_level: f32,
    pub(crate) volume: f32,
    pub(crate) waves: Vec<DoorSoundWave>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoorSoundWave {
    pub(crate) path: String,
    pub(crate) source_tier: DoorSoundSourceTier,
    pub(crate) bytes: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DoorSoundSourceTier {
    Pakfile,
    Addon,
    Loose,
    SiblingGma,
    GameVpk,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoorAudioEvent {
    pub(crate) content_id: u64,
    pub(crate) door_index: usize,
    pub(crate) kind: DoorAudioEventKind,
    pub(crate) gain: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoorAudioEventKind {
    MoveStarted,
    MoveLoopVolumeChanged,
    MotionEnded { open: bool },
    Parked,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorldVisibilityPlan {
    pub(crate) mesh_indices: Vec<Vec<u32>>,
    pub(crate) overlay_visible: Vec<bool>,
    pub(crate) detail_sprite_visible: Vec<bool>,
    pub(crate) visible_clusters: Vec<bool>,
    pub(crate) visible_cluster_count: u32,
}

impl WorldVisibilityPlan {
    pub(crate) fn from_visible_clusters(scene: &MapPreview, visible_clusters: &[bool]) -> Self {
        Self {
            mesh_indices: scene
                .scene
                .meshes
                .iter()
                .enumerate()
                .map(|(index, mesh)| {
                    let visibility = scene.mesh_visibility.get(index);
                    visibility.map_or_else(
                        || mesh.indices.clone(),
                        |visibility| {
                            visible_mesh_indices(
                                mesh.indices.as_slice(),
                                visibility,
                                visible_clusters,
                            )
                        },
                    )
                })
                .collect(),
            overlay_visible: scene
                .overlays
                .iter()
                .map(|overlay| bucket_visible(overlay.visibility, visible_clusters))
                .collect(),
            detail_sprite_visible: scene
                .detail_sprites
                .iter()
                .map(|sprite| bucket_visible(sprite.visibility, visible_clusters))
                .collect(),
            visible_clusters: visible_clusters.to_vec(),
            visible_cluster_count: visible_clusters.iter().filter(|visible| **visible).count()
                as u32,
        }
    }

    pub(crate) fn mesh_visible(&self, mesh_index: usize) -> bool {
        self.mesh_indices
            .get(mesh_index)
            .is_some_and(|indices| !indices.is_empty())
    }

    pub(crate) fn overlay_visible(&self, overlay_index: usize) -> bool {
        self.overlay_visible
            .get(overlay_index)
            .copied()
            .unwrap_or(true)
    }

    pub(crate) fn bucket_visible(&self, bucket: MapVisibilityBucket) -> bool {
        bucket_visible(bucket, &self.visible_clusters)
    }

    #[cfg(test)]
    pub(crate) fn visible_world_index_count(&self) -> usize {
        self.mesh_indices.iter().map(Vec::len).sum()
    }
}

fn visible_mesh_indices(
    source_indices: &[u32],
    visibility: &MapMeshVisibility,
    visible_clusters: &[bool],
) -> Vec<u32> {
    if visibility.always_visible.is_empty() && visibility.clusters.is_empty() {
        return source_indices.to_vec();
    }

    let max_face = visibility
        .always_visible
        .iter()
        .chain(
            visibility
                .clusters
                .iter()
                .flat_map(|cluster| &cluster.ranges),
        )
        .map(|range| range.face)
        .max()
        .unwrap_or(0);
    let mut emitted = vec![false; max_face as usize + 1];
    let mut indices = Vec::new();

    for range in &visibility.always_visible {
        push_visible_range(source_indices, range, &mut emitted, &mut indices);
    }
    for cluster in &visibility.clusters {
        let Some(true) = visible_clusters.get(cluster.cluster as usize).copied() else {
            continue;
        };
        for range in &cluster.ranges {
            push_visible_range(source_indices, range, &mut emitted, &mut indices);
        }
    }

    indices
}

fn push_visible_range(
    source_indices: &[u32],
    range: &MapMeshIndexRange,
    emitted: &mut [bool],
    indices: &mut Vec<u32>,
) {
    let Some(slot) = emitted.get_mut(range.face as usize) else {
        return;
    };
    if *slot {
        return;
    }
    let start = range.start as usize;
    let end = start.saturating_add(range.count as usize);
    let Some(slice) = source_indices.get(start..end) else {
        return;
    };
    *slot = true;
    indices.extend_from_slice(slice);
}

fn bucket_visible(bucket: MapVisibilityBucket, visible_clusters: &[bool]) -> bool {
    match bucket {
        MapVisibilityBucket::Always => true,
        MapVisibilityBucket::Cluster(cluster) => visible_clusters
            .get(cluster as usize)
            .copied()
            .unwrap_or(false),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialSlot {
    pub(crate) shader: crate::bridge::materials::MaterialShader,
    pub(crate) name: String,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) force_opaque: bool,
    pub(crate) render_mode: RenderMode,
}

pub type LightmapSlot = gmpublished_domain::scene::map::LightmapAtlas;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailSprite {
    pub(crate) origin: Vec3,
    pub(crate) angles: Vec3,
    pub(crate) orientation: u8,
    pub(crate) underwater_leaf: bool,
    pub(crate) upper_left: [f32; 2],
    pub(crate) lower_right: [f32; 2],
    pub(crate) tex_upper_left: [f32; 2],
    pub(crate) tex_lower_right: [f32; 2],
    pub(crate) material_index: usize,
    pub(crate) visibility: MapVisibilityBucket,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayPrimitive {
    pub(crate) vertices: [OverlayVertex; 4],
    pub(crate) material_index: usize,
    pub(crate) visibility: MapVisibilityBucket,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayVertex {
    pub(crate) position: Vec3,
    pub(crate) normal: Vec3,
    pub(crate) uv: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skybox {
    pub(crate) faces: [Option<Arc<ResolvedTexture>>; SKYBOX_FACE_COUNT],
    pub(crate) materials: [Option<crate::bridge::materials::SkyMaterial>; SKYBOX_FACE_COUNT],
    pub(crate) paint: Option<gmpublished_domain::scene::map::MapSkyPaint>,
}

enum_with_all! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(usize)]
    pub enum SkyboxFace {
        Rt,
        Lf,
        Bk,
        Ft,
        Up,
        Dn,
    }
}

pub const SKYBOX_FACE_COUNT: usize = SkyboxFace::ALL.len();

impl SkyboxFace {
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn suffix(self) -> &'static str {
        match self {
            Self::Rt => "rt",
            Self::Lf => "lf",
            Self::Bk => "bk",
            Self::Ft => "ft",
            Self::Up => "up",
            Self::Dn => "dn",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelStats {
    pub(crate) bone_count: u32,
    pub(crate) sequence_count: u32,
    pub(crate) vertex_count: u32,
    pub(crate) triangle_count: u32,
    pub(crate) mesh_count: u32,
    pub(crate) material_count: u32,
    pub(crate) resolved_material_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapStats {
    pub(crate) face_count: u32,
    pub(crate) displacement_count: u32,
    pub(crate) entity_count: u32,
    pub(crate) material_count: u32,
    pub(crate) resolved_material_count: u32,
    pub(crate) static_prop_count: u32,
    pub(crate) placed_prop_count: u32,
    pub(crate) skipped_prop_count: u32,
    pub(crate) detail_sprite_count: u32,
    pub(crate) overlay_count: u32,
    pub(crate) skybox_face_count: u32,
    pub(crate) skybox_prop_count: u32,
    pub(crate) skybox_detail_sprite_count: u32,
    pub(crate) skybox_overlay_count: u32,
    pub(crate) cluster_count: u32,
    pub(crate) version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapFog {
    pub(crate) color_linear: Vec3,
    pub(crate) start: f32,
    pub(crate) end: f32,
    pub(crate) max_density: f32,
    pub(crate) radial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSkyCamera {
    pub(crate) origin: Vec3,
    pub(crate) scale: f32,
    pub(crate) fog: Option<MapFog>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSpawn {
    pub(crate) origin: Vec3,
    /// Source QAngle order: pitch, yaw, roll.
    pub(crate) angles: Vec3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewLoadStage {
    ReadingArchive,
    ReadingBsp,
    ResolvingMaterials,
    PlacingProps,
    BakingLightmap,
}

impl PreviewLoadStage {
    pub(crate) const fn i18n_key(self) -> &'static str {
        match self {
            Self::ReadingArchive => "file-preview-stage-reading-archive",
            Self::ReadingBsp => "file-preview-stage-reading-bsp",
            Self::ResolvingMaterials => "file-preview-stage-resolving-materials",
            Self::PlacingProps => "file-preview-stage-placing-props",
            Self::BakingLightmap => "file-preview-stage-baking-lightmap",
        }
    }
}

pub const MAX_PREVIEW_LINES: usize = 2_000;

pub type CodeLine = Vec<CodeSpan>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeSpan {
    pub(crate) text: String,
    pub(crate) color: Option<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InfoReason {
    Binary,
    TooLarge,
    DecodeFailed,
}

impl PreviewData {
    pub(crate) fn from_request(request: &PreviewRequest, content: PreviewContent) -> Self {
        Self {
            entry_path: request.entry_path.clone(),
            display_name: request.display_name.clone(),
            size_bytes: request.size_bytes,
            crc32: request.crc32,
            related_preview: None,
            content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::RenderMode;

    fn vertex(x: f32) -> ModelVertex {
        ModelVertex {
            position: Vec3::new(x, 0.0, 0.0),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 0,
        }
    }

    fn mesh(material_index: usize) -> MeshData {
        MeshData {
            geometry: MeshGeometry::Model,
            vertices: vec![vertex(0.0), vertex(1.0), vertex(2.0)],
            indices: vec![0, 1, 2],
            material_index,
            bodygroup: 0,
            bodygroup_choice: 0,
        }
    }

    fn cluster_visibility(cluster: u32, face: u32) -> MapMeshVisibility {
        MapMeshVisibility {
            always_visible: Vec::new(),
            clusters: vec![gmpublished_domain::scene::map::MapMeshClusterRanges {
                cluster,
                ranges: vec![MapMeshIndexRange {
                    face,
                    start: 0,
                    count: 3,
                }],
            }],
        }
    }

    fn preview(mesh_visibility: Vec<MapMeshVisibility>) -> MapPreview {
        MapPreview {
            suns: Vec::new(),
            cubemap_probes: Vec::new(),
            sun_material_slots: Vec::new(),
            water_volumes: None,
            entity_meshes: Vec::new(),
            skybox_entity_meshes: Vec::new(),
            scene: Arc::new(RenderScene {
                meshes: vec![mesh(0), mesh(1)],
                materials: vec![
                    MaterialSlot {
                        shader: Default::default(),
                        name: "a".to_owned(),
                        texture: None,
                        texture2: None,
                        force_opaque: true,
                        render_mode: RenderMode::Opaque,
                    },
                    MaterialSlot {
                        shader: Default::default(),
                        name: "b".to_owned(),
                        texture: None,
                        texture2: None,
                        force_opaque: true,
                        render_mode: RenderMode::Opaque,
                    },
                ],
                phy_debug_meshes: Vec::new(),
                stats: ModelStats {
                    bone_count: 0,
                    sequence_count: 0,
                    vertex_count: 6,
                    triangle_count: 2,
                    mesh_count: 2,
                    material_count: 2,
                    resolved_material_count: 0,
                },
                bounds_min: Vec3::splat(0.0),
                bounds_max: Vec3::new(2.0, 0.0, 0.0),
            }),
            mesh_visibility,
            map_skybox_meshes: Vec::new(),
            model_lighting: Vec::new(),
            model_lighting_inputs: None,
            lightmap: None,
            skybox: None,
            detail_sprites: vec![DetailSprite {
                angles: Vec3::ZERO,
                orientation: 2,
                underwater_leaf: false,
                origin: Vec3::splat(0.0),
                upper_left: [0.0; 2],
                lower_right: [1.0; 2],
                tex_upper_left: [0.0; 2],
                tex_lower_right: [1.0; 2],
                material_index: 0,
                visibility: MapVisibilityBucket::Cluster(1),
            }],
            map_skybox_detail_sprites: Vec::new(),
            overlays: vec![OverlayPrimitive {
                vertices: [OverlayVertex {
                    position: Vec3::splat(0.0),
                    normal: Vec3::new(0.0, 0.0, 1.0),
                    uv: [0.0; 2],
                }; 4],
                material_index: 0,
                visibility: MapVisibilityBucket::Cluster(1),
            }],
            map_skybox_overlays: Vec::new(),
            doors: Vec::new(),
            visibility: None,
            walk_collision: None,
        }
    }

    #[test]
    fn pvs_plan_hides_and_reveals_second_cluster_content() {
        let scene = preview(vec![cluster_visibility(0, 0), cluster_visibility(1, 1)]);

        let cluster_a = WorldVisibilityPlan::from_visible_clusters(&scene, &[true, false]);
        assert_eq!(cluster_a.mesh_indices[0], vec![0, 1, 2]);
        assert!(cluster_a.mesh_indices[1].is_empty());
        assert!(!cluster_a.overlay_visible[0]);
        assert!(!cluster_a.detail_sprite_visible[0]);
        assert_eq!(cluster_a.visible_world_index_count(), 3);

        let both = WorldVisibilityPlan::from_visible_clusters(&scene, &[true, true]);
        assert_eq!(both.mesh_indices[1], vec![0, 1, 2]);
        assert!(both.overlay_visible[0]);
        assert!(both.detail_sprite_visible[0]);
        assert_eq!(both.visible_world_index_count(), 6);
    }

    #[test]
    fn face_ranges_visible_through_multiple_clusters_emit_once() {
        let mut visibility = cluster_visibility(0, 42);
        visibility
            .clusters
            .push(gmpublished_domain::scene::map::MapMeshClusterRanges {
                cluster: 1,
                ranges: vec![MapMeshIndexRange {
                    face: 42,
                    start: 0,
                    count: 3,
                }],
            });
        let scene = preview(vec![visibility, cluster_visibility(1, 7)]);

        let plan = WorldVisibilityPlan::from_visible_clusters(&scene, &[true, true]);

        assert_eq!(plan.mesh_indices[0], vec![0, 1, 2]);
        assert_eq!(plan.mesh_indices[0].len(), 3);
    }
}
