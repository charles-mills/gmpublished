use std::sync::Arc;

use iced::widget::image;

use crate::backend::archive::PreviewArchiveSource;
#[cfg(feature = "asset-studio")]
use crate::backend::materials::{RenderMode, ResolvedTexture};
#[cfg(feature = "asset-studio")]
pub use gmpublished_backend::scene::map::{
    MapDoorClass, MapDoorMotion, MapMeshIndexRange, MapMeshVisibility, MapTrace, MapVisibility,
    MapVisibilityBucket, MapWalkCollision,
};
/// GPU-ready preview vertex: models and map geometry share one
/// 14-float layout. `vformats` loads lean position/normal/uv vertices;
/// the extra lanes are the app's (prop lighting bakes into `color`,
/// debug meshes tint it, maps use `lightmap_uv`/`blend_alpha`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub lightmap_uv: [f32; 2],
    pub color: [f32; 3],
    pub blend_alpha: f32,
}

impl From<&vformats::mdl::ModelVertex> for ModelVertex {
    fn from(vertex: &vformats::mdl::ModelVertex) -> Self {
        Self {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
            lightmap_uv: [0.0; 2],
            color: [1.0; 3],
            blend_alpha: 0.0,
        }
    }
}

/// One renderable preview mesh (see [`ModelVertex`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MeshData {
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub material_index: usize,
    pub bodygroup: usize,
    pub bodygroup_choice: usize,
}

impl From<&vformats::mdl::MeshData> for MeshData {
    fn from(mesh: &vformats::mdl::MeshData) -> Self {
        Self {
            vertices: mesh.vertices.iter().map(ModelVertex::from).collect(),
            indices: mesh.indices.clone(),
            material_index: mesh.material_index,
            bodygroup: mesh.bodygroup,
            bodygroup_choice: mesh.bodygroup_choice,
        }
    }
}

/// Loaded model geometry in the app's preview shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelData {
    pub meshes: Vec<MeshData>,
    pub material_names: Vec<String>,
    pub material_dirs: Vec<String>,
    pub skin_tables: Vec<Vec<u16>>,
    pub bodygroups: Vec<usize>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub bone_count: u32,
    pub sequence_count: u32,
    pub vertex_count: u32,
    pub triangle_count: u32,
}

impl From<vformats::mdl::ModelData> for ModelData {
    fn from(model: vformats::mdl::ModelData) -> Self {
        Self {
            meshes: model.meshes.iter().map(MeshData::from).collect(),
            material_names: model.material_names,
            material_dirs: model.material_dirs,
            skin_tables: model.skin_tables,
            bodygroups: model.bodygroups,
            bounds_min: model.bounds_min,
            bounds_max: model.bounds_max,
            bone_count: model.bone_count,
            sequence_count: model.sequence_count,
            vertex_count: model.vertex_count,
            triangle_count: model.triangle_count,
        }
    }
}

#[cfg(feature = "asset-studio")]
pub const PHY_DEBUG_MATERIAL_NAME: &str = "__debug/phy_collision";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRequest {
    pub(crate) request_id: u64,
    pub(crate) archive: Arc<PreviewArchiveSource>,
    pub(crate) entry_path: String,
    pub(crate) display_name: String,
    pub(crate) size_bytes: u64,
    pub(crate) crc32: u32,
    /// The user pressed "Load anyway" on the very-large-file warning:
    /// skip the per-kind size gates and decode the entry regardless.
    pub(crate) bypass_size_limits: bool,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "asset-studio"), derive(Eq))]
pub struct PreviewData {
    pub(crate) entry_path: String,
    pub(crate) display_name: String,
    pub(crate) size_bytes: u64,
    pub(crate) crc32: u32,
    pub(crate) related_preview: Option<RelatedPreviewTarget>,
    pub(crate) content: PreviewContent,
}

impl PreviewData {
    /// Content-addressed identity shared by GPU upload reuse, camera-pose
    /// keying, and door audio event routing — one derivation so those
    /// consumers can never drift apart.
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
#[cfg_attr(not(feature = "asset-studio"), derive(Eq))]
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
    #[cfg(feature = "asset-studio")]
    Audio {
        bytes: Arc<Vec<u8>>,
        duration_secs: Option<f32>,
    },
    #[cfg(feature = "asset-studio")]
    Model(Arc<ModelPreview>),
    #[cfg(feature = "asset-studio")]
    Map {
        scene: Arc<ModelPreview>,
        stats: MapStats,
        fog: Option<MapFog>,
        sky_camera: Option<MapSkyCamera>,
        spawn: Option<MapSpawn>,
    },
    #[cfg(feature = "asset-studio")]
    Particle(Arc<ParticlePreview>),
    Info {
        reason: InfoReason,
    },
}

/// A parsed .pcf plus everything resolved up front for its systems: per-
/// system coverage/metadata and the de-duplicated sprite materials.
#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct ParticlePreview {
    pub(crate) file: gmpublished_backend::scene::pcf::PcfFile,
    pub(crate) systems: Vec<ParticleSystemInfo>,
    pub(crate) materials: Vec<ParticleMaterialSlot>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticleSystemInfo {
    pub(crate) name: String,
    /// Deduplicated operator coverage across the system and its children.
    pub(crate) coverage: Vec<gmpublished_backend::particles::CoverageEntry>,
    /// Highest control point index the compiled effect reads.
    pub(crate) highest_control_point: usize,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleMaterialSlot {
    /// Normalized via [`normalize_particle_material`].
    pub(crate) name: String,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) additive: bool,
    /// Sprite sheet from the texture's VTF resource block, when present.
    pub(crate) sheet: Option<Arc<vformats::vtf::SpriteSheet>>,
}

/// PCF material references are inconsistent ("effects\\spark.vmt",
/// "particle/foo"); loader keys and renderer lookups share this form.
#[cfg(feature = "asset-studio")]
pub fn normalize_particle_material(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase().replace('\\', "/");
    let name = name.strip_prefix("materials/").unwrap_or(&name);
    name.strip_suffix(".vmt").unwrap_or(name).to_owned()
}

#[cfg(all(test, feature = "asset-studio"))]
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

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct ModelPreview {
    pub(crate) meshes: Vec<MeshData>,
    pub(crate) mesh_visibility: Vec<MapMeshVisibility>,
    pub(crate) map_skybox_meshes: Vec<MeshData>,
    pub(crate) materials: Vec<MaterialSlot>,
    pub(crate) lightmap: Option<LightmapSlot>,
    pub(crate) skybox: Option<Skybox>,
    pub(crate) detail_sprites: Vec<DetailSprite>,
    pub(crate) map_skybox_detail_sprites: Vec<DetailSprite>,
    pub(crate) overlays: Vec<OverlayPrimitive>,
    pub(crate) map_skybox_overlays: Vec<OverlayPrimitive>,
    pub(crate) doors: Vec<DoorInstance>,
    pub(crate) phy_debug_meshes: Vec<MeshData>,
    pub(crate) skin_tables: Vec<Vec<u16>>,
    /// Choice count per bodygroup, in bodypart order.
    pub(crate) bodygroups: Vec<usize>,
    pub(crate) stats: ModelStats,
    pub(crate) bounds_min: [f32; 3],
    pub(crate) bounds_max: [f32; 3],
    pub(crate) visibility: Option<MapVisibility>,
    pub(crate) walk_collision: Option<MapWalkCollision>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct DoorInstance {
    pub(crate) class: MapDoorClass,
    pub(crate) origin: [f32; 3],
    /// Source QAngle order: pitch, yaw, roll.
    pub(crate) angles: [f32; 3],
    pub(crate) local_bounds_min: [f32; 3],
    pub(crate) local_bounds_max: [f32; 3],
    pub(crate) visibility: MapVisibilityBucket,
    pub(crate) initial_progress: f32,
    pub(crate) motion: MapDoorMotion,
    pub(crate) sounds: DoorSounds,
    pub(crate) meshes: Vec<MeshData>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoorSounds {
    pub(crate) move_sound: Option<DoorSound>,
    pub(crate) stop_sound: Option<DoorSound>,
    pub(crate) open_sound: Option<DoorSound>,
    pub(crate) close_sound: Option<DoorSound>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct DoorSound {
    pub(crate) reference: String,
    pub(crate) sound_level: f32,
    pub(crate) volume: f32,
    pub(crate) waves: Vec<DoorSoundWave>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoorSoundWave {
    pub(crate) path: String,
    pub(crate) source_tier: DoorSoundSourceTier,
    pub(crate) bytes: Arc<Vec<u8>>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DoorSoundSourceTier {
    Pakfile,
    Addon,
    Loose,
    SiblingGma,
    GameVpk,
    Prepended,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoorAudioEvent {
    pub(crate) content_id: u64,
    pub(crate) door_index: usize,
    pub(crate) kind: DoorAudioEventKind,
    pub(crate) gain: f32,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoorAudioEventKind {
    MoveStarted,
    MoveLoopVolumeChanged,
    MotionEnded { open: bool },
    Parked,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorldVisibilityPlan {
    pub(crate) mesh_indices: Vec<Vec<u32>>,
    pub(crate) overlay_visible: Vec<bool>,
    pub(crate) detail_sprite_visible: Vec<bool>,
    pub(crate) visible_clusters: Vec<bool>,
    pub(crate) visible_cluster_count: u32,
}

#[cfg(feature = "asset-studio")]
impl WorldVisibilityPlan {
    pub(crate) fn from_visible_clusters(scene: &ModelPreview, visible_clusters: &[bool]) -> Self {
        Self {
            mesh_indices: scene
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

#[cfg(feature = "asset-studio")]
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

#[cfg(feature = "asset-studio")]
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

#[cfg(feature = "asset-studio")]
fn bucket_visible(bucket: MapVisibilityBucket, visible_clusters: &[bool]) -> bool {
    match bucket {
        MapVisibilityBucket::Always => true,
        MapVisibilityBucket::Cluster(cluster) => visible_clusters
            .get(cluster as usize)
            .copied()
            .unwrap_or(false),
    }
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialSlot {
    pub(crate) name: String,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) force_opaque: bool,
    pub(crate) render_mode: RenderMode,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapSlot {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailSprite {
    pub(crate) origin: [f32; 3],
    pub(crate) upper_left: [f32; 2],
    pub(crate) lower_right: [f32; 2],
    pub(crate) tex_upper_left: [f32; 2],
    pub(crate) tex_lower_right: [f32; 2],
    pub(crate) material_index: usize,
    pub(crate) visibility: MapVisibilityBucket,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayPrimitive {
    pub(crate) vertices: [OverlayVertex; 4],
    pub(crate) material_index: usize,
    pub(crate) visibility: MapVisibilityBucket,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayVertex {
    pub(crate) position: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) uv: [f32; 2],
}

#[cfg(feature = "asset-studio")]
pub const SKYBOX_FACE_COUNT: usize = 6;

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skybox {
    pub(crate) faces: [Option<Arc<ResolvedTexture>>; SKYBOX_FACE_COUNT],
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkyboxFace {
    Rt,
    Lf,
    Bk,
    Ft,
    Up,
    Dn,
}

#[cfg(feature = "asset-studio")]
impl SkyboxFace {
    pub(crate) const ALL: [Self; SKYBOX_FACE_COUNT] =
        [Self::Rt, Self::Lf, Self::Bk, Self::Ft, Self::Up, Self::Dn];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Rt => 0,
            Self::Lf => 1,
            Self::Bk => 2,
            Self::Ft => 3,
            Self::Up => 4,
            Self::Dn => 5,
        }
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

#[cfg(feature = "asset-studio")]
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

#[cfg(feature = "asset-studio")]
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

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapFog {
    pub(crate) color_linear: [f32; 3],
    pub(crate) start: f32,
    pub(crate) end: f32,
    pub(crate) max_density: f32,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSkyCamera {
    pub(crate) origin: [f32; 3],
    pub(crate) scale: f32,
    pub(crate) fog: Option<MapFog>,
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSpawn {
    pub(crate) origin: [f32; 3],
    /// Source QAngle order: pitch, yaw, roll.
    pub(crate) angles: [f32; 3],
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

/// Line cap for code previews; larger files render truncated with a banner.
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

#[cfg(all(test, feature = "asset-studio"))]
mod tests {
    use super::*;
    use crate::backend::materials::RenderMode;

    fn vertex(x: f32) -> ModelVertex {
        ModelVertex {
            position: [x, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            color: [1.0; 3],
            blend_alpha: 0.0,
        }
    }

    fn mesh(material_index: usize) -> MeshData {
        MeshData {
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
            clusters: vec![gmpublished_backend::scene::map::MapMeshClusterRanges {
                cluster,
                ranges: vec![MapMeshIndexRange {
                    face,
                    start: 0,
                    count: 3,
                }],
            }],
        }
    }

    fn preview(mesh_visibility: Vec<MapMeshVisibility>) -> ModelPreview {
        ModelPreview {
            meshes: vec![mesh(0), mesh(1)],
            mesh_visibility,
            map_skybox_meshes: Vec::new(),
            materials: vec![
                MaterialSlot {
                    name: "a".to_owned(),
                    texture: None,
                    texture2: None,
                    force_opaque: true,
                    render_mode: RenderMode::Opaque,
                },
                MaterialSlot {
                    name: "b".to_owned(),
                    texture: None,
                    texture2: None,
                    force_opaque: true,
                    render_mode: RenderMode::Opaque,
                },
            ],
            lightmap: None,
            skybox: None,
            detail_sprites: vec![DetailSprite {
                origin: [0.0; 3],
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
                    position: [0.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0; 2],
                }; 4],
                material_index: 0,
                visibility: MapVisibilityBucket::Cluster(1),
            }],
            map_skybox_overlays: Vec::new(),
            doors: Vec::new(),
            phy_debug_meshes: Vec::new(),
            skin_tables: vec![vec![0, 1]],
            bodygroups: Vec::new(),
            stats: ModelStats {
                bone_count: 0,
                sequence_count: 0,
                vertex_count: 6,
                triangle_count: 2,
                mesh_count: 2,
                material_count: 2,
                resolved_material_count: 0,
            },
            bounds_min: [0.0; 3],
            bounds_max: [2.0, 0.0, 0.0],
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
            .push(gmpublished_backend::scene::map::MapMeshClusterRanges {
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
