use super::{
    LightmapAtlas, MapAmbientLighting, MapCubemapProbe, MapDetailSprite, MapDoor,
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
    pub lightmap: Option<LightmapAtlas>,
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
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapMeshSurfaceOwner {
    pub index_start: u32,
    pub owner: MapSurfaceOwner,
    pub solid: bool,
    pub allows_decals: bool,
    pub plane_normal: Vec3,
    pub plane_distance: f32,
    pub displacement: bool,
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
    pub has_lightmap: bool,
    /// Horizontal atlas stride to each of the three directional lighting planes.
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
    pub visibility: MapPropVisibility,
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
