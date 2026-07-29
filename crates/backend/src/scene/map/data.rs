use super::{
    LightmapAtlas, MapAmbientLighting, MapDetailSprite, MapDoor, MapEnvironmentLighting, MapFog,
    MapOverlay, MapPakFile, MapPlayerStart, MapSkyCamera, MapVisibility, MapWalkCollision,
};
use crate::math::Vec3;

#[derive(Debug, Clone, PartialEq)]
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
    pub skybox_overlays: Vec<MapOverlay>,
    pub ambient: MapAmbientLighting,
    pub environment_lighting: Option<MapEnvironmentLighting>,
    pub player_start: Option<MapPlayerStart>,
    pub skyname: Option<String>,
    pub fog: Option<MapFog>,
    pub sky_camera: Option<MapSkyCamera>,
    pub skybox_completion_bounds: Option<MapBounds>,
    pub lightmap: Option<LightmapAtlas>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub stats: MapStatsRaw,
    pub skybox_partition: MapSkyboxPartitionStats,
    pub visibility: Option<MapVisibility>,
    pub walk_collision: Option<MapWalkCollision>,
    pub pakfile: MapPakFile,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapMesh {
    pub vertices: Vec<MapVertex>,
    pub indices: Vec<u32>,
    pub material_index: usize,
    pub visibility: MapMeshVisibility,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MapMeshVisibility {
    pub always_visible: Vec<MapMeshIndexRange>,
    pub clusters: Vec<MapMeshClusterRanges>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapMeshClusterRanges {
    pub cluster: u32,
    pub ranges: Vec<MapMeshIndexRange>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct MapMeshIndexRange {
    pub face: u32,
    pub start: u32,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum MapVisibilityBucket {
    Always,
    Cluster(u32),
}

#[derive(Debug, Clone, Eq, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapVertex {
    pub position: Vec3,
    pub normal: Vec3,
    /// Raw Source texel-space S coordinate: position.dot(s_axis) + s_offset.
    pub tex_s: f32,
    /// Raw Source texel-space T coordinate: position.dot(t_axis) + t_offset.
    pub tex_t: f32,
    pub lightmap_uv: [f32; 2],
    pub blend_alpha: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticPropPlacement {
    pub model_path: String,
    pub origin: Vec3,
    pub angles: Vec3,
    pub skin: i32,
    pub scale: f32,
    pub solid: MapPropSolid,
    pub visibility: MapPropVisibility,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MapPropSolid {
    None,
    Physics,
}

impl MapPropSolid {
    pub const fn is_physics(self) -> bool {
        matches!(self, Self::Physics)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapBounds {
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct MapSkyboxPartitionStats {
    pub sky_camera_present: bool,
    pub face_count: u32,
    pub completion_reattributed_face_count: u32,
    pub static_prop_count: u32,
    pub detail_sprite_count: u32,
    pub overlay_count: u32,
}
