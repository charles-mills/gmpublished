use super::{
    LightmapSet, MapAmbientLighting, MapCubemapProbe, MapDetailSprite, MapDoor,
    MapEnvironmentLighting, MapFog, MapLightingCollision, MapOverlay, MapPakFile, MapPlayerStart,
    MapSkyCamera, MapSkyPaint, MapSun, MapVisibility, MapWalkCollision, MapWorldLighting,
};
use crate::math::Vec3;

#[derive(Clone, Debug, PartialEq)]
pub struct MapData {
    pub meshes: Vec<MapMesh>,
    pub skybox_meshes: Vec<MapMesh>,
    pub material_names: Vec<String>,
    pub static_props: Vec<StaticPropPlacement>,
    pub skybox_static_props: Vec<StaticPropPlacement>,
    pub doors: Vec<MapDoor>,
    pub detail_material_name: String,
    pub detail_sprites: Vec<MapDetailSprite>,
    pub skybox_detail_sprites: Vec<MapDetailSprite>,
    pub overlays: Vec<MapOverlay>,
    pub decals: Vec<super::MapDecal>,
    pub skybox_overlays: Vec<MapOverlay>,
    pub ambient: MapAmbientLighting,
    pub world_lighting: MapWorldLighting,
    pub environment_lighting: Option<MapEnvironmentLighting>,
    pub player_start: Option<MapPlayerStart>,
    pub skyname: Option<String>,
    pub sky_paint: Option<MapSkyPaint>,
    pub suns: Vec<MapSun>,
    pub cubemap_probes: Vec<MapCubemapProbe>,
    pub fog: Option<MapFog>,
    pub sky_camera: Option<MapSkyCamera>,
    pub skybox_completion_bounds: Option<MapBounds>,
    pub lightmap: Option<LightmapSet>,
    pub lighting_source: Option<super::LightmapSource>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub stats: MapStatsRaw,
    pub skybox_partition: MapSkyboxPartitionStats,
    pub visibility: Option<MapVisibility>,
    pub walk_collision: Option<MapWalkCollision>,
    pub water_volumes: Option<super::MapWaterVolumes>,
    pub lighting_collision: Option<MapLightingCollision>,
    pub radiosity_surfaces: Option<super::MapRadiositySurfaces>,
    pub pakfile: MapPakFile,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapMesh {
    /// Geometry belonging to a brush entity, separate from world brushes.
    pub entity: bool,
    pub vertices: Vec<MapVertex>,
    pub indices: Vec<u32>,
    pub material_index: usize,
    pub visibility: MapMeshVisibility,
    pub surface_owners: Vec<MapMeshSurfaceOwner>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MapSurfaceOwner {
    #[default]
    World,
    Brush {
        entity_index: usize,
        model_index: super::ModelIndex,
        origin: Vec3,
        angles: Vec3,
    },
    Door {
        door_index: usize,
        model_index: super::ModelIndex,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MapSurfaceOwnerId {
    World,
    Brush(usize),
    Door(usize),
}

impl MapSurfaceOwner {
    pub const fn id(self) -> MapSurfaceOwnerId {
        match self {
            Self::World => MapSurfaceOwnerId::World,
            Self::Brush { entity_index, .. } => MapSurfaceOwnerId::Brush(entity_index),
            Self::Door { door_index, .. } => MapSurfaceOwnerId::Door(door_index),
        }
    }

    pub const fn model_index(self) -> Option<super::ModelIndex> {
        match self {
            Self::World => None,
            Self::Brush { model_index, .. } | Self::Door { model_index, .. } => Some(model_index),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapMeshSurfaceOwner {
    pub index_start: u32,
    pub owner: MapSurfaceOwner,
    pub solid: bool,
    pub allows_decals: bool,
    pub plane_normal: Vec3,
    pub plane_distance: f32,
    pub displacement: Option<MapDisplacementProjection>,
    pub texture_projection: Option<MapSurfaceTextureProjection>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSurfaceTextureProjection {
    pub vectors: [[f32; 4]; 2],
    pub minimum: [i32; 2],
    pub extent: [i16; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapDisplacementProjection {
    pub base_corners: [Vec3; 4],
    pub steps: usize,
    pub positions: std::sync::Arc<[Vec3]>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MapReceiverMesh {
    World(usize),
    Skybox(usize),
    Door {
        door_index: usize,
        mesh_index: usize,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapMeshVisibility {
    pub always_visible: Vec<MapMeshIndexRange>,
    pub clusters: Vec<MapMeshClusterRanges>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapMeshClusterRanges {
    pub cluster: u32,
    pub ranges: Vec<MapMeshIndexRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapMeshIndexRange {
    pub face: u32,
    pub start: u32,
    pub count: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MapVisibilityBucket {
    Always,
    Cluster(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MapPropVisibility {
    Always,
    Clusters(Vec<u32>),
}

impl MapPropVisibility {
    pub const fn always() -> Self {
        Self::Always
    }

    pub fn clusters(&self) -> &[u32] {
        match self {
            Self::Always => &[],
            Self::Clusters(clusters) => clusters,
        }
    }
}

impl From<MapVisibilityBucket> for MapPropVisibility {
    fn from(bucket: MapVisibilityBucket) -> Self {
        match bucket {
            MapVisibilityBucket::Always => Self::Always,
            MapVisibilityBucket::Cluster(cluster) => Self::Clusters(vec![cluster]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapVertex {
    pub position: Vec3,
    /// Parent surface position before displacement deformation.
    pub base_position: Vec3,
    pub normal: Vec3,
    pub tangent_s: Vec3,
    pub tangent_t: Vec3,
    /// Raw Source texel-space S coordinate: position.dot(s_axis) + s_offset.
    pub tex_s: f32,
    /// Raw Source texel-space T coordinate: position.dot(t_axis) + t_offset.
    pub tex_t: f32,
    /// Compiled texdata dimensions, independent of replacement/fallback textures.
    pub texture_size: [u32; 2],
    pub lightmap_uv: [f32; 2],
    pub lightmap_page: Option<crate::scene::map::LightmapPageId>,
    /// Horizontal page stride to each directional lighting plane.
    pub lightmap_bump_step: f32,
    pub blend_alpha: f32,
    /// Source vertex opacity: ordinary brushes use one; displacements retain authored alpha.
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StaticPropPlacement {
    pub entity: bool,
    /// Original game-lump index used by `sp_N.vhv`, unaffected by filtering/partitioning.
    pub static_prop_index: Option<usize>,
    pub lighting_origin: Option<Vec3>,
    pub lighting_origin_reference: Option<super::placements::MapLightingOriginReference>,
    pub diffuse_modulation: [u8; 4],
    pub render_mode: i32,
    pub flags: u8,
    pub flags_ex: Option<u32>,
    pub extra_flags: Option<u32>,
    pub model_path: String,
    pub origin: Vec3,
    pub angles: Vec3,
    pub skin: i32,
    pub scale: f32,
    pub solid: MapPropSolid,
    pub leaf_membership: MapPropLeafMembership,
    pub visibility: MapPropVisibility,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MapPropLeafMembership {
    Compiled(Vec<u16>),
    SpatialBounds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapPropSolid {
    None,
    Physics,
}

impl MapPropSolid {
    pub const fn is_physics(self) -> bool {
        matches!(self, Self::Physics)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapBounds {
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapStatsRaw {
    pub face_count: u32,
    pub displacement_count: u32,
    pub entity_count: u32,
    pub model_count: u32,
    pub static_prop_count: u32,
    pub world_static_prop_count: u32,
    pub skybox_static_prop_count: u32,
    pub entity_prop_count: u32,
    pub world_entity_prop_count: u32,
    pub skybox_entity_prop_count: u32,
    pub cluster_count: u32,
    pub version: u32,
    pub lightmap: MapLightmapStats,
    pub invalid_detail_leaves: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Prepared receiver payloads; shared-model placements count independently.
pub struct MapLightmapStats {
    pub unlightmapped_surfaces: u32,
    pub valid_payloads: u32,
    pub missing_payloads: u32,
    pub invalid_payloads: u32,
    pub atlas_dropped_blocks: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MapSkyboxPartitionStats {
    pub sky_camera_present: bool,
    pub face_count: u32,
    pub completion_reattributed_face_count: u32,
    pub static_prop_count: u32,
    pub detail_sprite_count: u32,
    pub overlay_count: u32,
}
