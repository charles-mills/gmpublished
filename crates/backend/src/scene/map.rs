//! Preview-quality BSP map decoding: geometry, static/detail props,
//! lighting, visibility, and walk collision, built on
//! [`vformats::bsp`]. A handful of small algorithms here (the leaf
//! tree walk in `walk_to_leaf`, the invalid-vertex face tolerance in
//! `discard_faces_with_invalid_vertices`, and the displacement
//! corner-rotation logic) are ported from vbsp (MIT, © icewind1991),
//! since vformats deliberately leaves scene-assembly concerns like
//! these to its callers.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
};

use thiserror::Error;
use vformats::{
    Limits,
    bsp::{
        self, Brush, BrushSide, BspModel, ColorRgbExp, DetailProp, DetailProps, DispInfo, DispVert,
        Face, Leaf, LeafAmbientIndex, LeafAmbientSample, Node, Overlay, Plane, StaticProp,
        StaticProps, TexData, TexInfo, Visibility, ZipReader, contents_flags, texture_flags,
    },
    keyvalues::KvValue,
};

mod lighting;
mod mesh;
mod visibility;
mod walk;

pub use lighting::{
    AmbientCube, AmbientLightSource, LightmapAtlas, LightmapSource, MapAmbientLighting,
    MapEnvironmentLighting, MapSunLighting,
};
#[cfg(test)]
use lighting::{
    AmbientSampleRange, MapAmbientSample, brush_lightmap_uv_from_transforms,
    decode_ambient_sample_linear, decode_light_sample, decode_light_sample_linear,
    pack_lightmap_blocks,
};
use lighting::{
    PendingLightmapBlock, bake_lightmap_atlas, brush_lightmap_uv, displacement_lightmap_uv,
    extract_face_lightmap, map_ambient_lighting, map_environment_lighting,
    selected_lightmap_samples,
};
use mesh::{
    BuildMesh, BuildMeshes, FaceAppendContext, GeometryPartition, append_face,
    displacement_vertices, texture_coord,
};
#[cfg(test)]
use mesh::{DisplacementGridVertex, displacement_blend_alpha, tessellate_displacement_grid};
pub use visibility::MapVisibility;
use visibility::{
    FaceAttributions, MapFaceVisibility, PROP_AABB_MAX_DEPTH, PROP_AABB_MAX_EXTENT,
    PROP_AABB_MAX_LEAVES, SkyboxPartition, cluster_in_range, model_face_range, point_cluster,
    visibility_bucket,
};
use walk::{
    BoundsBuilder, MapLeafLocator, SKYBOX_COMPLETION_AABB_EXPANSION,
    SKYBOX_COMPLETION_MAX_WORLD_VOLUME_FRACTION, bounds_contains_point, bounds_from_points_iter,
    bounds_volume, bsp_world_bounds, expand_bounds,
};
pub use walk::{
    ConvexHull, MapTrace, MapWalkCollision, MapWalkPropCollisionSource, MapWalkPropModel,
    MapWalkPropModelPlacement, WaterVolume,
};
#[cfg(test)]
use walk::{
    MapWalkBrush, MapWalkBrushPlane, MapWalkDisplacement, MapWalkPropCollision, MapWalkTriangle,
    TRACE_PLANE_EPSILON, brush_side_sky_from_texture_flags, local_prop_brush_from_hull,
    prop_brush_from_local, trace_brush_aabb, walk_brush_from_brush_planes, walk_brush_from_planes,
};

const BSP_MAGIC: &[u8; 4] = b"VBSP";
/// Advisory threshold: callers warn (and ask) above this rather than
/// load unprompted. `load_map` itself decodes any size handed to it.
pub const MAX_BSP_BYTES: usize = 1024 * 1024 * 1024;
pub const MAX_PAKFILE_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const PROP_DOOR_DEFAULT_MOVE_SOUND: &str = "DoorSound.DefaultMove";
const PROP_DOOR_DEFAULT_ARRIVE_SOUND: &str = "DoorSound.DefaultArrive";

/// `SURF_TRIGGER`/`SURF_HINT`/`SURF_SKIP` (`bspfile.h`): not in
/// [`texture_flags`] because they're rarely needed, but
/// [`MapBsp::face_is_visible`] checks them alongside SKY/SKY2D/NODRAW.
const SURF_TRIGGER: i32 = 0x0040;
const SURF_HINT: i32 = 0x0100;
const SURF_SKIP: i32 = 0x0200;

/// `SolidType::Physics` (the vphysics collision mode) in the static prop
/// game lump's `solid` byte.
const STATIC_PROP_SOLID_PHYSICS: u8 = 6;

/// One entity's flat string keyvalue pairs (real Source entity lumps
/// have no nested blocks, so [`vformats::keyvalues::KvValue::Block`]
/// pairs are dropped). Lookup is exact-match, first-occurrence-wins,
/// matching vbsp's own `RawEntity::prop` (vformats' `KvDocument::get_str`
/// is case-insensitive, which is not the same lookup). Both keys and
/// values are ASCII-lowercased at construction: vbsp's GMod-tolerance
/// fork lowercases the whole entities lump text before parsing it
/// (`reader::LumpReader::read_entities`), so wild content's inconsistent
/// `WorldSpawn`/`worldspawn` classname casing (etc.) still matches the
/// lowercase literal keys/values every lookup in this module compares
/// against. Lowercasing per-pair after parsing is equivalent: ASCII
/// case-folding never changes token boundaries or UTF-8 validity.
#[derive(Debug, Clone, Default)]
struct MapEntity {
    pairs: Vec<(String, String)>,
}

impl MapEntity {
    fn from_document(document: &vformats::keyvalues::KvDocument<'_>) -> Self {
        Self {
            pairs: document
                .pairs
                .iter()
                .filter_map(|pair| match &pair.value {
                    KvValue::String(value) => Some((
                        pair.key.as_ref().to_ascii_lowercase(),
                        value.as_ref().to_ascii_lowercase(),
                    )),
                    KvValue::Block(_) => None,
                })
                .collect(),
        }
    }

    fn prop(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Every BSP lump the scene builder needs, decoded once at load time from
/// [`bsp::parse`] into owned storage (no borrow of the source bytes) so
/// it can be threaded through the whole `load_map` pipeline by reference.
struct MapBsp {
    vertices: Vec<[f32; 3]>,
    planes: Vec<Plane>,
    edges: Vec<[u16; 2]>,
    surfedges: Vec<i32>,
    faces: Vec<Face>,
    texinfos: Vec<TexInfo>,
    texdatas: Vec<TexData>,
    texdata_strings: Vec<String>,
    models: Vec<BspModel>,
    brushes: Vec<Brush>,
    brush_sides: Vec<BrushSide>,
    nodes: Vec<Node>,
    leaves: Vec<Leaf>,
    leaf_faces: Vec<u16>,
    leaf_brushes: Vec<u16>,
    displacements: Vec<DispInfo>,
    displacement_verts: Vec<DispVert>,
    lighting: Vec<ColorRgbExp>,
    lighting_hdr: Vec<ColorRgbExp>,
    leaf_ambient_lighting: Vec<LeafAmbientSample>,
    leaf_ambient_lighting_hdr: Vec<LeafAmbientSample>,
    leaf_ambient_indices: Vec<LeafAmbientIndex>,
    leaf_ambient_indices_hdr: Vec<LeafAmbientIndex>,
    overlays: Vec<Overlay>,
    entities: Vec<MapEntity>,
    visibility: Option<Visibility<'static>>,
    static_props: Option<StaticProps>,
    detail_props: Option<DetailProps>,
    /// The raw pakfile lump bytes; parsed on demand via [`ZipReader`]
    /// (its reader borrows, so it cannot be stored on `Self`).
    pakfile_bytes: Vec<u8>,
}

impl MapBsp {
    fn parse(bytes: &[u8], limits: &Limits) -> Result<Self, BspError> {
        let bsp = bsp::parse(bytes, limits).map_err(decode_error)?;
        let entities = bsp
            .entities(limits)
            .map_err(decode_error)?
            .iter()
            .map(MapEntity::from_document)
            .collect();
        let visibility = bsp
            .visibility(limits)
            .map_err(decode_error)?
            .map(Visibility::into_owned);
        let mut faces = bsp.faces(limits).map_err(decode_error)?;
        let edges = bsp.edges(limits).map_err(decode_error)?;
        let surfedges = bsp.surfedges(limits).map_err(decode_error)?;
        let vertices = bsp.vertices(limits).map_err(decode_error)?;
        discard_faces_with_invalid_vertices(&mut faces, &surfedges, &edges, vertices.len());
        let pakfile_bytes = bsp
            .lump(bsp::lump_ids::PAKFILE)
            .unwrap_or_default()
            .to_vec();

        Ok(Self {
            planes: bsp.planes(limits).map_err(decode_error)?,
            texinfos: bsp.texinfos(limits).map_err(decode_error)?,
            texdatas: bsp.texdatas(limits).map_err(decode_error)?,
            texdata_strings: bsp
                .texdata_strings(limits)
                .map_err(decode_error)?
                .into_iter()
                .map(Cow::into_owned)
                .collect(),
            models: bsp.models(limits).map_err(decode_error)?,
            brushes: bsp.brushes(limits).map_err(decode_error)?,
            brush_sides: bsp.brush_sides(limits).map_err(decode_error)?,
            nodes: bsp.nodes(limits).map_err(decode_error)?,
            leaves: bsp.leafs(limits).map_err(decode_error)?,
            leaf_faces: bsp.leaf_faces(limits).map_err(decode_error)?,
            leaf_brushes: bsp.leaf_brushes(limits).map_err(decode_error)?,
            displacements: bsp.displacement_infos(limits).map_err(decode_error)?,
            displacement_verts: bsp.displacement_verts(limits).map_err(decode_error)?,
            lighting: bsp.lighting(limits).map_err(decode_error)?,
            lighting_hdr: bsp.lighting_hdr(limits).map_err(decode_error)?,
            leaf_ambient_lighting: bsp.leaf_ambient_lighting(limits).map_err(decode_error)?,
            leaf_ambient_lighting_hdr: bsp
                .leaf_ambient_lighting_hdr(limits)
                .map_err(decode_error)?,
            leaf_ambient_indices: bsp.leaf_ambient_indices(limits).map_err(decode_error)?,
            leaf_ambient_indices_hdr: bsp.leaf_ambient_indices_hdr(limits).map_err(decode_error)?,
            overlays: bsp.overlays(limits).map_err(decode_error)?,
            static_props: bsp.static_props(limits).map_err(decode_error)?,
            detail_props: bsp.detail_props(limits).map_err(decode_error)?,
            vertices,
            faces,
            edges,
            surfedges,
            entities,
            visibility,
            pakfile_bytes,
        })
    }

    fn face(&self, index: usize) -> Option<&Face> {
        self.faces.get(index)
    }

    fn face_texinfo(&self, face: &Face) -> Option<&TexInfo> {
        usize::try_from(face.texinfo)
            .ok()
            .and_then(|index| self.texinfos.get(index))
    }

    fn texinfo_texdata(&self, texinfo: &TexInfo) -> Option<&TexData> {
        usize::try_from(texinfo.texdata)
            .ok()
            .and_then(|index| self.texdatas.get(index))
    }

    /// The resolved texture name for `texinfo`'s texdata, or `""` when
    /// either lookup misses (matches the empty-string fallback the
    /// preview mesh builder already tolerates for stray content).
    fn texinfo_name(&self, texinfo: &TexInfo) -> &str {
        self.texinfo_texdata(texinfo)
            .and_then(|texdata| {
                usize::try_from(texdata.name_index)
                    .ok()
                    .and_then(|index| self.texdata_strings.get(index))
            })
            .map_or("", String::as_str)
    }

    /// Vertex positions in face winding order, following surfedges
    /// through their edges (vbsp `Handle<Face>::vertices`). Missing
    /// indices are dropped rather than the whole face failing — matches
    /// vbsp's own tolerance (a `Handle` there assumes validated indices,
    /// but the fallible pieces this crate cannot pre-validate degrade the
    /// same way its `Option`-returning lookups already did downstream).
    fn face_vertex_positions(&self, face: &Face) -> Vec<[f32; 3]> {
        self.face_vertex_indices(face)
            .filter_map(|index| self.vertices.get(usize::from(index)).copied())
            .collect()
    }

    fn face_vertex_indices(&self, face: &Face) -> impl Iterator<Item = u16> + '_ {
        let first = i64::from(face.first_edge);
        let count = i64::from(face.edge_count).max(0);
        (first..first.saturating_add(count)).filter_map(move |surfedge_index| {
            let surfedge = *self.surfedges.get(usize::try_from(surfedge_index).ok()?)?;
            let edge = self
                .edges
                .get(usize::try_from(surfedge.unsigned_abs()).ok()?)?;
            Some(if surfedge >= 0 { edge[0] } else { edge[1] })
        })
    }

    fn face_displacement(&self, face: &Face) -> Option<&DispInfo> {
        usize::try_from(face.displacement)
            .ok()
            .and_then(|index| self.displacements.get(index))
    }

    /// The plane normal for `face`'s own side (not yet flipped for
    /// `face.side`; see the free function `face_normal`).
    fn face_plane_normal(&self, face: &Face) -> [f32; 3] {
        self.planes
            .get(usize::from(face.plane))
            .map_or([0.0; 3], |plane| plane.normal)
    }

    /// vbsp `Handle<Face>::is_visible`: false for sky, 2D-sky, trigger,
    /// hint, skip, and nodraw surfaces. A face whose texinfo is out of
    /// range (never happens for validated vbsp input, but this crate
    /// does not cross-validate at parse) is treated as not visible
    /// rather than assumed drawable.
    fn face_is_visible(&self, face: &Face) -> bool {
        let Some(texinfo) = self.face_texinfo(face) else {
            return false;
        };
        texinfo.flags
            & (texture_flags::SKY2D
                | texture_flags::SKY
                | SURF_TRIGGER
                | SURF_HINT
                | SURF_SKIP
                | texture_flags::NODRAW)
            == 0
    }

    /// Displacement vertices for `disp`'s `(2^power + 1)^2` grid, in
    /// row-major order. Indices past the end of the lump are dropped
    /// (matches vbsp's `Option`-filtering `displacement_vertices`
    /// iterator, which silently shortens the sequence rather than
    /// zero-filling it).
    fn displacement_vertices(&self, disp: &DispInfo) -> impl Iterator<Item = &DispVert> {
        let start = i64::from(disp.vert_start);
        let count = displacement_vertex_count(disp.power);
        (start..start.saturating_add(count))
            .filter_map(|index| self.displacement_verts.get(usize::try_from(index).ok()?))
    }

    /// The static prop placements (absent game lump yields none).
    fn static_props_iter(&self) -> impl Iterator<Item = &StaticProp> {
        self.static_props
            .iter()
            .flat_map(|props| props.props.iter())
    }

    fn static_prop_model<'s>(&'s self, prop: &StaticProp) -> &'s str {
        self.static_props
            .as_ref()
            .and_then(|props| props.models.get(usize::from(prop.model_index)))
            .map_or("", String::as_str)
    }

    /// The prop's leaf span into the game lump's shared leaf table (used
    /// to derive multi-cluster visibility).
    fn static_prop_leaves(&self, prop: &StaticProp) -> Option<&[u16]> {
        let props = self.static_props.as_ref()?;
        let start = usize::from(prop.first_leaf);
        let end = start.checked_add(usize::from(prop.leaf_count))?;
        props.leaves.get(start..end)
    }

    fn detail_sprites(&self) -> &[vformats::bsp::DetailSprite] {
        self.detail_props
            .as_ref()
            .map_or(&[], |props| props.sprites.as_slice())
    }

    fn detail_props_iter(&self) -> &[DetailProp] {
        self.detail_props
            .as_ref()
            .map_or(&[], |props| props.props.as_slice())
    }

    /// The map's visibility cluster count, or 0 when the map has no
    /// visibility lump (fullbright/unvised).
    fn cluster_count(&self) -> u32 {
        self.visibility
            .as_ref()
            .map_or(0, |vis| vis.cluster_count() as u32)
    }

    /// Every cluster reachable from `from` by following PVS edges
    /// (treated as an undirected reachability graph, matching the flood
    /// fill vbsp's `LazyVisData::reachable_clusters` used for skybox
    /// partitioning — ported here since visibility derivation is scene
    /// assembly, not something `vformats::bsp` does itself).
    fn reachable_clusters(&self, from: i16) -> Vec<bool> {
        let Some(vis) = self.visibility.as_ref() else {
            return Vec::new();
        };
        let cluster_count = vis.cluster_count();
        let Some(start) = usize::try_from(from)
            .ok()
            .filter(|cluster| *cluster < cluster_count)
        else {
            return vec![false; cluster_count];
        };
        let mut reached = vec![false; cluster_count];
        reached[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(cluster) = queue.pop_front() {
            let Some(row) = vis.pvs(cluster) else {
                continue;
            };
            for (next, visible) in row.into_iter().enumerate() {
                if visible && !reached[next] {
                    reached[next] = true;
                    queue.push_back(next);
                }
            }
        }
        reached
    }

    /// Find the leaf containing `point` (see [`walk_to_leaf`]).
    fn leaf_at(&self, point: [f32; 3]) -> Option<usize> {
        walk_to_leaf(
            point,
            |index| {
                self.nodes
                    .get(index)
                    .map(|node| (node.plane, node.children))
            },
            |index| {
                let plane = self.planes.get(usize::try_from(index).ok()?)?;
                Some((plane.normal, plane.dist))
            },
        )
    }
}

/// Walk a BSP tree from the root to the leaf containing `point`
/// (ported from vbsp's `Bsp::leaf_at`, MIT, © icewind1991). Node and
/// plane lookup are closures because the two callers store the tree
/// differently: [`MapBsp`] as the decoded lumps, [`MapLeafLocator`] as
/// a retained projection of them.
fn walk_to_leaf(
    point: [f32; 3],
    node: impl Fn(usize) -> Option<(i32, [i32; 2])>,
    plane: impl Fn(i32) -> Option<([f32; 3], f32)>,
) -> Option<usize> {
    let mut current_index = 0usize;
    loop {
        let (plane_index, children) = node(current_index)?;
        let (normal, dist) = plane(plane_index)?;
        let distance = point[0] * normal[0] + point[1] * normal[1] + point[2] * normal[2];
        let [front, back] = children;
        let next = if distance < dist { back } else { front };
        if next < 0 {
            return Some((!next) as usize);
        }
        current_index = usize::try_from(next).ok()?;
    }
}

/// `(2^power + 1)^2`: a displacement's vertex grid side squared.
fn displacement_vertex_count(power: i32) -> i64 {
    let side = 2_i64
        .saturating_pow(power.clamp(0, 32) as u32)
        .saturating_add(1);
    side.saturating_mul(side)
}

/// vbsp's `discard_faces_with_invalid_vertices` (MIT, © icewind1991):
/// wild content sometimes has faces whose surfedges reference
/// out-of-range vertices. Rather than let every downstream face-vertex
/// lookup degrade independently, zero the edge count up front so the
/// face is skipped everywhere a valid face would be processed (mirrors
/// `append_face`'s existing `num_edges < 3` skip).
fn discard_faces_with_invalid_vertices(
    faces: &mut [Face],
    surfedges: &[i32],
    edges: &[[u16; 2]],
    vertex_count: usize,
) {
    for face in faces {
        let start = i64::from(face.first_edge);
        let count = i64::from(face.edge_count).max(0);
        let references_invalid_vertex = (start..start.saturating_add(count)).any(|index| {
            let Some(surfedge) = usize::try_from(index)
                .ok()
                .and_then(|index| surfedges.get(index))
            else {
                return true;
            };
            let Some(edge) = usize::try_from(surfedge.unsigned_abs())
                .ok()
                .and_then(|index| edges.get(index))
            else {
                return true;
            };
            edge.iter()
                .any(|vertex| usize::from(*vertex) >= vertex_count)
        });
        if references_invalid_vertex {
            face.edge_count = 0;
            face.displacement = -1;
        }
    }
}

#[derive(Debug, Clone)]
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
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub stats: MapStatsRaw,
    pub skybox_partition: MapSkyboxPartitionStats,
    pub visibility: Option<MapVisibility>,
    pub walk_collision: Option<MapWalkCollision>,
    pub pakfile: MapPakFile,
}

impl PartialEq for MapData {
    fn eq(&self, other: &Self) -> bool {
        self.meshes == other.meshes
            && self.skybox_meshes == other.skybox_meshes
            && self.material_names == other.material_names
            && self.static_props == other.static_props
            && self.skybox_static_props == other.skybox_static_props
            && self.doors == other.doors
            && self.detail_material_name == other.detail_material_name
            && self.detail_sprites == other.detail_sprites
            && self.skybox_detail_sprites == other.skybox_detail_sprites
            && self.overlays == other.overlays
            && self.skybox_overlays == other.skybox_overlays
            && self.ambient == other.ambient
            && self.environment_lighting == other.environment_lighting
            && self.player_start == other.player_start
            && self.skyname == other.skyname
            && self.fog == other.fog
            && self.sky_camera == other.sky_camera
            && self.skybox_completion_bounds == other.skybox_completion_bounds
            && self.lightmap == other.lightmap
            && self.bounds_min == other.bounds_min
            && self.bounds_max == other.bounds_max
            && self.stats == other.stats
            && self.skybox_partition == other.skybox_partition
            && self.visibility == other.visibility
            && self.walk_collision == other.walk_collision
    }
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
    pub fn from_clusters(mut clusters: Vec<u32>) -> Self {
        clusters.sort_unstable();
        clusters.dedup();
        if clusters.is_empty() {
            Self::Always
        } else {
            Self::Clusters(clusters)
        }
    }

    pub const fn always() -> Self {
        Self::Always
    }

    pub fn clusters(&self) -> &[u32] {
        match self {
            Self::Always => &[],
            Self::Clusters(clusters) => clusters,
        }
    }

    pub const fn is_always(&self) -> bool {
        matches!(self, Self::Always)
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
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Raw Source texel-space S coordinate: dot(position, s_axis) + s_offset.
    pub tex_s: f32,
    /// Raw Source texel-space T coordinate: dot(position, t_axis) + t_offset.
    pub tex_t: f32,
    pub lightmap_uv: [f32; 2],
    pub blend_alpha: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticPropPlacement {
    pub model_path: String,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MapDoorClass {
    FuncDoor,
    FuncMoveLinear,
    FuncDoorRotating,
    PropDoorRotating,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MapDoorOpenDirection {
    Both,
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapDoor {
    pub class: MapDoorClass,
    pub origin: [f32; 3],
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: [f32; 3],
    pub local_bounds_min: [f32; 3],
    pub local_bounds_max: [f32; 3],
    pub visibility: MapVisibilityBucket,
    pub wait: f32,
    pub initial_progress: f32,
    pub motion: MapDoorMotion,
    pub sounds: MapDoorSounds,
    pub geometry: MapDoorGeometry,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MapDoorSounds {
    pub move_sound: Option<String>,
    pub stop_sound: Option<String>,
    pub open_sound: Option<String>,
    pub close_sound: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapDoorGeometry {
    Brush {
        model_index: u32,
        meshes: Vec<MapMesh>,
    },
    Prop {
        placement: StaticPropPlacement,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MapDoorMotion {
    Linear {
        direction: [f32; 3],
        distance: f32,
        speed: f32,
    },
    Rotating {
        angle_delta: [f32; 3],
        degrees: f32,
        speed: f32,
        open_direction: MapDoorOpenDirection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapDetailSprite {
    pub origin: [f32; 3],
    pub upper_left: [f32; 2],
    pub lower_right: [f32; 2],
    pub tex_upper_left: [f32; 2],
    pub tex_lower_right: [f32; 2],
    pub visibility: MapVisibilityBucket,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapOverlay {
    pub id: i32,
    pub material_name: String,
    pub positions: [[f32; 3]; 4],
    pub normal: [f32; 3],
    pub u: [f32; 2],
    pub v: [f32; 2],
    pub face_count: u16,
    pub visibility: MapVisibilityBucket,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct MapPlane {
    normal: [f32; 3],
    dist: f32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct MapNode {
    plane_index: i32,
    children: [i32; 2],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct MapLeaf {
    cluster: i16,
    mins: [i16; 3],
    maxs: [i16; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapPlayerStart {
    pub origin: [f32; 3],
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapFog {
    pub color_srgb: [u8; 3],
    pub start: f32,
    pub end: f32,
    pub max_density: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSkyCamera {
    pub origin: [f32; 3],
    pub scale: f32,
    pub fog: Option<MapFog>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
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

/// The embedded pakfile lump's raw bytes. Parsed on demand via
/// [`ZipReader`] (its reader borrows, so it cannot be cached across
/// calls the way vbsp's `Packfile` cached an owned `zip::ZipArchive`).
#[derive(Debug, Clone)]
pub struct MapPakFile {
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MapPakFileEntry {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

impl MapPakFile {
    pub fn from_pak_bytes(bytes: Vec<u8>) -> Result<Self, BspError> {
        Ok(Self { bytes })
    }

    /// A malformed central directory, if the lump is not a readable ZIP
    /// archive (tolerated: an absent/corrupt pakfile just means no
    /// bundled content, not a `load_map` failure).
    pub fn read_error(&self) -> Option<String> {
        ZipReader::parse(&self.bytes)
            .err()
            .map(|error| error.to_string())
    }

    pub fn indexed_entries(&self) -> Result<Vec<MapPakFileEntry>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(decode_error)?;
        let mut entries = Vec::new();
        for (index, entry) in reader.entries().iter().enumerate() {
            let Some(path) = normalize_pakfile_path(&entry.path) else {
                continue;
            };
            if !is_pakfile_retained_entry(&path) {
                continue;
            }
            if is_pakfile_entry_oversized(entry.uncompressed_size) {
                log::debug!(
                    "bsp pakfile entry {} skipped over {} MiB cap ({} bytes)",
                    path,
                    MAX_PAKFILE_ENTRY_BYTES / 1024 / 1024,
                    entry.uncompressed_size
                );
                continue;
            }
            entries.push(MapPakFileEntry {
                index,
                path,
                size: entry.uncompressed_size,
            });
        }
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }

    pub fn entry_bytes_by_index(&self, index: usize) -> Result<Option<Vec<u8>>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(decode_error)?;
        let Some(entry) = reader.entries().get(index) else {
            return Ok(None);
        };
        let limits = Limits {
            max_entry_bytes: MAX_PAKFILE_ENTRY_BYTES,
            ..Limits::default()
        };
        reader
            .entry_bytes(entry, &limits)
            .map(|bytes| Some(bytes.into_owned()))
            .map_err(decode_error)
    }
}

const DEFAULT_DETAIL_MATERIAL: &str = "detail/detailsprites";
const DETAIL_PROP_TYPE_SPRITE: u8 = 1;

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum BspError {
    #[error("ERR_BSP_DECODE_FAILED")]
    Decode { message: String },
    #[error("ERR_BSP_UNSUPPORTED_VERSION")]
    UnsupportedVersion { version: u32 },
    #[error("ERR_BSP_TOO_LARGE")]
    TooLarge { item: &'static str },
}

pub fn load_map(bytes: &[u8]) -> Result<MapData, BspError> {
    load_map_with_skybox_partition(bytes, true)
}

fn load_map_with_skybox_partition(
    bytes: &[u8],
    partition_skybox: bool,
) -> Result<MapData, BspError> {
    let version = bsp_version(bytes)?;
    if !matches!(version, 19 | 20) {
        return Err(BspError::UnsupportedVersion { version });
    }

    // No whole-input cap: the caller gates size (and gets the user's
    // consent above MAX_BSP_BYTES); these bytes are already in memory.
    let limits = Limits {
        max_input_bytes: u64::MAX,
        ..Limits::default()
    };
    let bsp = MapBsp::parse(bytes, &limits)?;
    let mut stats = MapStatsRaw {
        face_count: count_to_u32(bsp.faces.len()),
        displacement_count: count_to_u32(bsp.displacements.len()),
        entity_count: count_to_u32(bsp.entities.len()),
        model_count: count_to_u32(bsp.models.len()),
        static_prop_count: count_to_u32(bsp.static_props_iter().count()),
        world_static_prop_count: 0,
        skybox_static_prop_count: 0,
        entity_prop_count: 0,
        world_entity_prop_count: 0,
        skybox_entity_prop_count: 0,
        cluster_count: bsp.cluster_count(),
        version,
    };
    let player_start = info_player_start(&bsp.entities);
    let sky_camera = map_sky_camera(&bsp.entities);
    let mut skybox_partition = partition_skybox
        .then(|| SkyboxPartition::from_bsp(&bsp, player_start.as_ref(), sky_camera))
        .flatten()
        .unwrap_or_else(|| SkyboxPartition::inactive(sky_camera.is_some()));
    if partition_skybox {
        let initial_face_attributions = FaceAttributions::from_bsp(&bsp, &skybox_partition);
        skybox_partition.apply_completion_bounds(
            &bsp,
            &initial_face_attributions,
            player_start.as_ref(),
        );
    }
    let face_attributions = FaceAttributions::from_bsp(&bsp, &skybox_partition);
    let pending_doors = pending_map_doors(&bsp);
    let door_model_indices = pending_doors
        .iter()
        .filter_map(PendingMapDoor::brush_model_index)
        .collect::<BTreeSet<_>>();

    let mut material_names = Vec::<String>::new();
    let mut material_indexes = HashMap::<String, usize>::new();
    let mut meshes = BuildMeshes::default();
    let mut door_builds = Vec::<PendingDoorBuild>::new();
    let mut lightmap_blocks = Vec::<PendingLightmapBlock>::new();
    let (lightmap_samples, lightmap_source) = selected_lightmap_samples(&bsp);

    {
        let mut face_context = FaceAppendContext {
            meshes: &mut meshes,
            material_names: &mut material_names,
            material_indexes: &mut material_indexes,
            lightmap_samples,
            lightmap_blocks: &mut lightmap_blocks,
        };
        for (model_index, model) in bsp.models.iter().enumerate() {
            if door_model_indices.contains(&model_index) {
                continue;
            }
            let Some(face_range) = model_face_range(model, bsp.faces.len()) else {
                continue;
            };
            for face_index in face_range {
                let Some(face) = bsp.face(face_index) else {
                    continue;
                };
                append_face(
                    &bsp,
                    face,
                    face_index,
                    face_attributions.partition(face_index),
                    &face_attributions.visibility(face_index),
                    &mut face_context,
                )?;
            }
        }
        for pending in pending_doors {
            door_builds.push(extract_pending_door_meshes(
                pending,
                &bsp,
                &face_attributions,
                &mut face_context,
            )?);
        }
    }

    let (mut static_props, mut skybox_static_props) =
        partitioned_static_prop_placements(&bsp, &skybox_partition);
    stats.world_static_prop_count = count_to_u32(static_props.len());
    stats.skybox_static_prop_count = count_to_u32(skybox_static_props.len());
    let (mut entity_props, mut skybox_entity_props) =
        partitioned_entity_prop_placements(&bsp, &skybox_partition);
    stats.world_entity_prop_count = count_to_u32(entity_props.len());
    stats.skybox_entity_prop_count = count_to_u32(skybox_entity_props.len());
    stats.entity_prop_count =
        count_to_u32(entity_props.len().saturating_add(skybox_entity_props.len()));
    static_props.append(&mut entity_props);
    skybox_static_props.append(&mut skybox_entity_props);
    let detail_material_name = worldspawn_detail_material_name(&bsp.entities);
    let (detail_sprites, skybox_detail_sprites) =
        partitioned_detail_sprite_placements(&bsp, &skybox_partition);
    let (overlays, skybox_overlays) = partitioned_map_overlays(&bsp, &skybox_partition);
    let ambient = map_ambient_lighting(&bsp);
    let environment_lighting = map_environment_lighting(&bsp.entities);
    let skyname = worldspawn_skyname(&bsp.entities);
    let fog = map_fog(&bsp.entities);
    let lightmap = bake_lightmap_atlas(
        meshes.iter_mut().chain(
            door_builds
                .iter_mut()
                .flat_map(|door| door.meshes.iter_mut()),
        ),
        &lightmap_blocks,
        lightmap_source,
    );
    let doors = door_builds
        .into_iter()
        .filter_map(PendingDoorBuild::into_map_door)
        .collect::<Vec<_>>();
    let (meshes, skybox_meshes) = split_build_meshes(meshes.into_inner());
    let meshes = meshes
        .into_iter()
        .map(|mesh| MapMesh {
            vertices: mesh
                .vertices
                .into_iter()
                .map(|vertex| vertex.vertex)
                .collect(),
            indices: mesh.indices,
            material_index: mesh.material_index,
            visibility: mesh.visibility.into_map_visibility(),
        })
        .collect::<Vec<_>>();
    let skybox_meshes = skybox_meshes
        .into_iter()
        .map(|mesh| MapMesh {
            vertices: mesh
                .vertices
                .into_iter()
                .map(|vertex| vertex.vertex)
                .collect(),
            indices: mesh.indices,
            material_index: mesh.material_index,
            visibility: mesh.visibility.into_map_visibility(),
        })
        .collect::<Vec<_>>();
    let (bounds_min, bounds_max) = bounds_from_meshes(&meshes);
    let visibility = MapVisibility::from_bsp(&bsp);
    let door_brush_indices = door_model_indices
        .iter()
        .flat_map(|model_index| brush_indices_for_model(&bsp, *model_index))
        .collect::<BTreeSet<_>>();
    let walk_collision = MapWalkCollision::from_bsp_excluding(&bsp, &door_brush_indices);
    let pakfile = MapPakFile::from_pak_bytes(bsp.pakfile_bytes)
        .expect("from_pak_bytes never fails: it only stores the raw bytes");
    let skybox_completion_bounds = skybox_partition.completion_bounds();
    let skybox_partition = MapSkyboxPartitionStats {
        sky_camera_present: skybox_partition.sky_camera_present,
        face_count: count_to_u32(face_attributions.skybox_face_count()),
        completion_reattributed_face_count: count_to_u32(
            face_attributions.completion_reattributed_face_count(),
        ),
        static_prop_count: count_to_u32(skybox_static_props.len()),
        detail_sprite_count: count_to_u32(skybox_detail_sprites.len()),
        overlay_count: count_to_u32(skybox_overlays.len()),
    };

    Ok(MapData {
        meshes,
        skybox_meshes,
        material_names,
        static_props,
        skybox_static_props,
        doors,
        detail_material_name,
        detail_sprites,
        skybox_detail_sprites,
        overlays,
        skybox_overlays,
        ambient,
        environment_lighting,
        player_start,
        skyname,
        fog,
        sky_camera,
        skybox_completion_bounds,
        lightmap,
        bounds_min,
        bounds_max,
        stats,
        skybox_partition,
        visibility,
        walk_collision,
        pakfile,
    })
}

fn bsp_version(bytes: &[u8]) -> Result<u32, BspError> {
    let Some(header) = bytes.get(..8) else {
        return Err(BspError::Decode {
            message: "header too short".to_owned(),
        });
    };
    if &header[..4] != BSP_MAGIC {
        return Err(BspError::Decode {
            message: "missing VBSP magic".to_owned(),
        });
    }
    Ok(u32::from_le_bytes(
        header[4..8]
            .try_into()
            .expect("slice length was checked above"),
    ))
}

#[derive(Debug)]
struct PendingMapDoor {
    class: MapDoorClass,
    origin: [f32; 3],
    angles: [f32; 3],
    visibility: MapVisibilityBucket,
    wait: f32,
    initial_progress: f32,
    motion: MapDoorMotion,
    sounds: MapDoorSounds,
    geometry: PendingDoorGeometry,
}

#[derive(Debug)]
enum PendingDoorGeometry {
    Brush { model_index: usize },
    Prop { placement: StaticPropPlacement },
}

impl PendingMapDoor {
    fn brush_model_index(&self) -> Option<usize> {
        match self.geometry {
            PendingDoorGeometry::Brush { model_index } => Some(model_index),
            PendingDoorGeometry::Prop { .. } => None,
        }
    }
}

#[derive(Debug)]
struct PendingDoorBuild {
    door: PendingMapDoor,
    meshes: Vec<BuildMesh>,
}

impl PendingDoorBuild {
    fn into_map_door(self) -> Option<MapDoor> {
        match self.door.geometry {
            PendingDoorGeometry::Brush { model_index } => {
                let meshes = self
                    .meshes
                    .into_iter()
                    .map(|mesh| build_mesh_to_map_mesh_local(mesh, self.door.origin))
                    .collect::<Vec<_>>();
                let (local_bounds_min, local_bounds_max) = bounds_from_map_meshes(&meshes)?;
                Some(MapDoor {
                    class: self.door.class,
                    origin: self.door.origin,
                    angles: self.door.angles,
                    local_bounds_min,
                    local_bounds_max,
                    visibility: self.door.visibility,
                    wait: self.door.wait,
                    initial_progress: self.door.initial_progress,
                    motion: self.door.motion,
                    sounds: self.door.sounds,
                    geometry: MapDoorGeometry::Brush {
                        model_index: u32::try_from(model_index).unwrap_or(u32::MAX),
                        meshes,
                    },
                })
            }
            PendingDoorGeometry::Prop { placement } => Some(MapDoor {
                class: self.door.class,
                origin: self.door.origin,
                angles: self.door.angles,
                local_bounds_min: [0.0; 3],
                local_bounds_max: [0.0; 3],
                visibility: self.door.visibility,
                wait: self.door.wait,
                initial_progress: self.door.initial_progress,
                motion: self.door.motion,
                sounds: self.door.sounds,
                geometry: MapDoorGeometry::Prop { placement },
            }),
        }
    }
}

fn build_mesh_to_map_mesh_local(mesh: BuildMesh, origin: [f32; 3]) -> MapMesh {
    MapMesh {
        vertices: mesh
            .vertices
            .into_iter()
            .map(|mut vertex| {
                vertex.vertex.position = sub(vertex.vertex.position, origin);
                vertex.vertex
            })
            .collect(),
        indices: mesh.indices,
        material_index: mesh.material_index,
        visibility: mesh.visibility.into_map_visibility(),
    }
}

fn bounds_from_map_meshes(meshes: &[MapMesh]) -> Option<([f32; 3], [f32; 3])> {
    bounds_from_points_iter(
        meshes
            .iter()
            .flat_map(|mesh| mesh.vertices.iter().map(|vertex| vertex.position)),
    )
}

fn pending_map_doors(bsp: &MapBsp) -> Vec<PendingMapDoor> {
    let cluster_count = bsp.cluster_count();
    bsp.entities
        .iter()
        .filter_map(|entity| pending_map_door(entity, bsp, cluster_count))
        .collect()
}

fn pending_map_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    match entity.prop("classname")? {
        "func_door" => {
            pending_linear_brush_door(entity, bsp, cluster_count, MapDoorClass::FuncDoor)
        }
        "func_movelinear" => {
            pending_linear_brush_door(entity, bsp, cluster_count, MapDoorClass::FuncMoveLinear)
        }
        "func_door_rotating" => pending_rotating_brush_door(entity, bsp, cluster_count),
        "prop_door_rotating" => pending_prop_door(entity, bsp, cluster_count),
        _ => None,
    }
}

fn pending_linear_brush_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
    class: MapDoorClass,
) -> Option<PendingMapDoor> {
    let model_index = parse_bmodel_index(entity.prop("model")?)?;
    let Some(model) = bsp.models.get(model_index) else {
        log::debug!("bsp {class:?} skipped: invalid bmodel index {model_index}");
        return None;
    };
    let origin = entity_origin_or_model_origin(entity, model, class);
    let direction = entity
        .prop("movedir")
        .or_else(|| entity.prop("angles"))
        .and_then(parse_entity_vec3)
        .map_or([1.0, 0.0, 0.0], angle_vectors_forward);
    let speed = parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed");
    let lip = parse_entity_float_default(entity.prop("lip"), 0.0, class, "lip");
    let model_distance = linear_door_distance(model, direction, lip);
    let distance = if class == MapDoorClass::FuncMoveLinear {
        entity
            .prop("MoveDistance")
            .or_else(|| entity.prop("movedistance"))
            .and_then(parse_entity_float)
            .filter(|distance| *distance > 0.0)
            .unwrap_or(model_distance)
    } else {
        model_distance
    };
    let initial_progress = if class == MapDoorClass::FuncMoveLinear {
        entity
            .prop("StartPosition")
            .or_else(|| entity.prop("startposition"))
            .and_then(parse_entity_float)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    } else if door_spawn_starts_open(entity) {
        1.0
    } else {
        0.0
    };
    Some(PendingMapDoor {
        class,
        origin,
        angles: [0.0; 3],
        visibility: point_visibility_bucket(bsp, origin, cluster_count),
        wait: parse_entity_float_default(entity.prop("wait"), 3.0, class, "wait"),
        initial_progress,
        motion: MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        },
        sounds: brush_door_sounds(entity),
        geometry: PendingDoorGeometry::Brush { model_index },
    })
}

fn pending_rotating_brush_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    let class = MapDoorClass::FuncDoorRotating;
    let model_index = parse_bmodel_index(entity.prop("model")?)?;
    let Some(model) = bsp.models.get(model_index) else {
        log::debug!("bsp {class:?} skipped: invalid bmodel index {model_index}");
        return None;
    };
    let origin = entity_origin_or_model_origin(entity, model, class);
    let angles = entity
        .prop("angles")
        .and_then(parse_entity_vec3)
        .unwrap_or([0.0; 3]);
    let spawnflags = parse_entity_spawnflags(entity);
    let degrees = parse_entity_float_default(entity.prop("distance"), 90.0, class, "distance")
        .abs()
        .max(0.0);
    let mut angle_delta = rotation_axis_delta(spawnflags, degrees);
    if spawnflags & SF_DOOR_ROTATE_BACKWARDS != 0 {
        angle_delta = mul(angle_delta, -1.0);
    }
    Some(PendingMapDoor {
        class,
        origin,
        angles,
        visibility: point_visibility_bucket(bsp, origin, cluster_count),
        wait: parse_entity_float_default(entity.prop("wait"), 3.0, class, "wait"),
        initial_progress: if door_spawn_starts_open(entity) {
            1.0
        } else {
            0.0
        },
        motion: MapDoorMotion::Rotating {
            angle_delta,
            degrees,
            speed: parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed"),
            open_direction: MapDoorOpenDirection::Both,
        },
        sounds: brush_door_sounds(entity),
        geometry: PendingDoorGeometry::Brush { model_index },
    })
}

fn pending_prop_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    let class = MapDoorClass::PropDoorRotating;
    let model = entity.prop("model")?;
    let Some(model_path) = normalize_entity_prop_model_path(model) else {
        log::debug!("bsp prop_door_rotating skipped: invalid model {model:?}");
        return None;
    };
    let Some(origin) = entity.prop("origin").and_then(parse_entity_vec3) else {
        log::debug!("bsp prop_door_rotating skipped: missing/invalid origin");
        return None;
    };
    let angles = entity
        .prop("angles")
        .and_then(parse_entity_vec3)
        .unwrap_or([0.0; 3]);
    let skin = entity.prop("skin").and_then(parse_entity_i32).unwrap_or(0);
    let degrees = parse_entity_float_default(entity.prop("distance"), 90.0, class, "distance")
        .abs()
        .max(0.0);
    let open_direction = match entity
        .prop("opendir")
        .and_then(parse_entity_i32)
        .unwrap_or(0)
    {
        1 => MapDoorOpenDirection::Forward,
        2 => MapDoorOpenDirection::Backward,
        _ => MapDoorOpenDirection::Both,
    };
    let visibility = point_visibility_bucket(bsp, origin, cluster_count);
    let placement = StaticPropPlacement {
        model_path,
        origin,
        angles,
        skin,
        scale: 1.0,
        solid: MapPropSolid::None,
        visibility: visibility.into(),
    };
    Some(PendingMapDoor {
        class,
        origin,
        angles,
        visibility,
        wait: parse_entity_float_default(entity.prop("returndelay"), -1.0, class, "returndelay"),
        initial_progress: prop_door_initial_progress(entity),
        motion: MapDoorMotion::Rotating {
            angle_delta: [0.0, degrees, 0.0],
            degrees,
            speed: parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed"),
            open_direction,
        },
        sounds: prop_door_sounds(entity),
        geometry: PendingDoorGeometry::Prop { placement },
    })
}

fn brush_door_sounds(entity: &MapEntity) -> MapDoorSounds {
    MapDoorSounds {
        move_sound: normalized_sound_keyvalue(entity.prop("noise1")),
        stop_sound: normalized_sound_keyvalue(entity.prop("noise2")),
        open_sound: None,
        close_sound: None,
    }
}

fn prop_door_sounds(entity: &MapEntity) -> MapDoorSounds {
    // Source SDK 2013 `CBasePropDoor` fills missing sound overrides from
    // model `door_options` skin blocks before validating to script names.
    // The current `vmdl` path does not expose the embedded MDL keyvalues, so
    // unresolved prop-door overrides fall back to the mounted script defaults
    // used by GMod content. This keeps the runtime soundful and records the
    // model-keyvalues gap without touching raw-offset MDL parsing here.
    MapDoorSounds {
        move_sound: normalized_sound_keyvalue(entity.prop("soundmoveoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_MOVE_SOUND.to_owned())),
        stop_sound: None,
        open_sound: normalized_sound_keyvalue(entity.prop("soundopenoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_ARRIVE_SOUND.to_owned())),
        close_sound: normalized_sound_keyvalue(entity.prop("soundcloseoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_ARRIVE_SOUND.to_owned())),
    }
}

fn normalized_sound_keyvalue(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn extract_pending_door_meshes(
    pending: PendingMapDoor,
    bsp: &MapBsp,
    face_attributions: &FaceAttributions,
    context: &mut FaceAppendContext<'_>,
) -> Result<PendingDoorBuild, BspError> {
    let model_index = match pending.geometry {
        PendingDoorGeometry::Brush { model_index } => model_index,
        PendingDoorGeometry::Prop { .. } => {
            return Ok(PendingDoorBuild {
                door: pending,
                meshes: Vec::new(),
            });
        }
    };
    let mut meshes = BuildMeshes::default();
    let Some(model) = bsp.models.get(model_index) else {
        return Ok(PendingDoorBuild {
            door: pending,
            meshes: Vec::new(),
        });
    };
    if let Some(face_range) = model_face_range(model, bsp.faces.len()) {
        let visibility = MapFaceVisibility::from_bucket(pending.visibility);
        let mut door_context = FaceAppendContext {
            meshes: &mut meshes,
            material_names: context.material_names,
            material_indexes: context.material_indexes,
            lightmap_samples: context.lightmap_samples,
            lightmap_blocks: context.lightmap_blocks,
        };
        for face_index in face_range {
            let Some(face) = bsp.face(face_index) else {
                continue;
            };
            append_face(
                bsp,
                face,
                face_index,
                face_attributions.partition(face_index),
                &visibility,
                &mut door_context,
            )?;
        }
    }
    Ok(PendingDoorBuild {
        door: pending,
        meshes: meshes.into_inner(),
    })
}

const SF_DOOR_START_OPEN_OBSOLETE: u32 = 1;
const SF_DOOR_ROTATE_BACKWARDS: u32 = 2;
const SF_DOOR_ROTATE_ROLL: u32 = 64;
const SF_DOOR_ROTATE_PITCH: u32 = 128;

fn parse_bmodel_index(model: &str) -> Option<usize> {
    let index = model.strip_prefix('*')?.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

fn entity_origin_or_model_origin(
    entity: &MapEntity,
    model: &BspModel,
    class: MapDoorClass,
) -> [f32; 3] {
    if let Some(origin) = entity.prop("origin") {
        if let Some(parsed) = parse_entity_vec3(origin) {
            return parsed;
        }
        log::debug!("bsp {class:?} invalid origin {origin:?}: using bmodel origin");
    }
    model.origin
}

fn linear_door_distance(model: &BspModel, direction: [f32; 3], lip: f32) -> f32 {
    let mins = model.mins;
    let maxs = model.maxs;
    let size = std::array::from_fn(|axis| (maxs[axis] - mins[axis] - 2.0).max(0.0));
    (dot_abs(direction, size) - lip).max(0.0)
}

fn angle_vectors_forward(angles: [f32; 3]) -> [f32; 3] {
    let pitch = angles[0].to_radians();
    let yaw = angles[1].to_radians();
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    normalize([cos_pitch * cos_yaw, cos_pitch * sin_yaw, -sin_pitch])
}

fn parse_entity_float_default(
    value: Option<&str>,
    default: f32,
    class: MapDoorClass,
    field: &'static str,
) -> f32 {
    value.map_or(default, |value| {
        parse_entity_float(value).unwrap_or_else(|| {
            log::debug!("bsp {class:?} {field} invalid: defaulting to {default}");
            default
        })
    })
}

fn parse_entity_spawnflags(entity: &MapEntity) -> u32 {
    entity
        .prop("spawnflags")
        .and_then(parse_entity_i32)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0)
}

fn rotation_axis_delta(spawnflags: u32, degrees: f32) -> [f32; 3] {
    // Source SDK 2013 CBaseToggle::AxisDir checks roll before pitch; yaw is
    // the default. Preserve that priority so mixed wild flags degrade like
    // the engine instead of inventing a new axis.
    if spawnflags & SF_DOOR_ROTATE_ROLL != 0 {
        [0.0, 0.0, degrees]
    } else if spawnflags & SF_DOOR_ROTATE_PITCH != 0 {
        [degrees, 0.0, 0.0]
    } else {
        [0.0, degrees, 0.0]
    }
}

fn door_spawn_starts_open(entity: &MapEntity) -> bool {
    parse_entity_spawnflags(entity) & SF_DOOR_START_OPEN_OBSOLETE != 0
        || entity.prop("spawnpos").and_then(parse_entity_i32) == Some(1)
}

fn prop_door_initial_progress(entity: &MapEntity) -> f32 {
    if parse_entity_spawnflags(entity) & SF_DOOR_START_OPEN_OBSOLETE != 0 {
        return 1.0;
    }
    matches!(
        entity.prop("spawnpos").and_then(parse_entity_i32),
        Some(1 | 2)
    )
    .then_some(1.0)
    .unwrap_or(0.0)
}

fn brush_indices_for_model(bsp: &MapBsp, model_index: usize) -> BTreeSet<usize> {
    let mut brushes = BTreeSet::new();
    let Some(model) = bsp.models.get(model_index) else {
        return brushes;
    };
    // Iterative with a visited set: wild content can encode a cycle in the
    // node children, and unbounded recursion aborts past catch_unwind.
    let mut visited_nodes = BTreeSet::new();
    let mut stack = vec![model.head_node];
    while let Some(node_index) = stack.pop() {
        if node_index < 0 {
            let leaf_index = (!node_index) as usize;
            let Some(leaf) = bsp.leaves.get(leaf_index) else {
                continue;
            };
            let start = usize::from(leaf.first_leaf_brush);
            let end = start.saturating_add(usize::from(leaf.leaf_brush_count));
            let Some(leaf_brushes) = bsp.leaf_brushes.get(start..end) else {
                continue;
            };
            brushes.extend(
                leaf_brushes
                    .iter()
                    .map(|leaf_brush| usize::from(*leaf_brush)),
            );
            continue;
        }
        let Some(index) = usize::try_from(node_index).ok() else {
            continue;
        };
        if !visited_nodes.insert(index) {
            continue;
        }
        let Some(node) = bsp.nodes.get(index) else {
            continue;
        };
        stack.extend(node.children);
    }
    brushes
}

fn split_build_meshes(meshes: Vec<BuildMesh>) -> (Vec<BuildMesh>, Vec<BuildMesh>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    for mesh in meshes {
        match mesh.partition {
            GeometryPartition::Visible => visible.push(mesh),
            GeometryPartition::Skybox => skybox.push(mesh),
        }
    }
    (visible, skybox)
}

fn partitioned_static_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for prop in bsp.static_props_iter().filter_map(|prop| {
        let origin = prop.origin;
        let visibility = static_prop_leaf_visibility(bsp, prop, cluster_count);
        Some(StaticPropPlacement {
            model_path: normalize_static_prop_model_path(bsp.static_prop_model(prop))?,
            origin,
            angles: prop.angles,
            skin: prop.skin,
            scale: 1.0,
            solid: static_prop_solid(prop.solid),
            visibility,
        })
    }) {
        match partition.point_partition(bsp, prop.origin) {
            GeometryPartition::Visible => visible.push(prop),
            GeometryPartition::Skybox => skybox.push(prop),
        }
    }
    (visible, skybox)
}

fn static_prop_leaf_visibility(
    bsp: &MapBsp,
    prop: &StaticProp,
    cluster_count: u32,
) -> MapPropVisibility {
    let model = bsp.static_prop_model(prop);
    let leaf_count = usize::from(prop.leaf_count);
    if leaf_count == 0 {
        log::debug!(
            "bsp static prop {model} at {:?} has empty leaf list: using Always",
            prop.origin
        );
        return MapPropVisibility::Always;
    }
    let Some(prop_leaves) = bsp.static_prop_leaves(prop) else {
        log::debug!(
            "bsp static prop {model} leaf list out of range first={} count={leaf_count}: using Always",
            prop.first_leaf
        );
        return MapPropVisibility::Always;
    };
    let mut clusters = BTreeSet::<u32>::new();
    let mut invalid_clusters = 0_usize;
    for leaf_index in prop_leaves {
        let Some(leaf) = bsp.leaves.get(usize::from(*leaf_index)) else {
            log::debug!(
                "bsp static prop {model} leaf {leaf_index} missing from BSP leaves: using Always"
            );
            return MapPropVisibility::Always;
        };
        if cluster_in_range(leaf.cluster, cluster_count) {
            clusters.insert(u32::try_from(leaf.cluster).unwrap_or(0));
        } else {
            invalid_clusters = invalid_clusters.saturating_add(1);
        }
    }
    if clusters.is_empty() {
        log::debug!(
            "bsp static prop {model} leaf list first={} count={leaf_count} had no valid clusters (invalid leaves {invalid_clusters}): using Always",
            prop.first_leaf
        );
        MapPropVisibility::Always
    } else {
        MapPropVisibility::Clusters(clusters.into_iter().collect())
    }
}

const ENTITY_PROP_CLASSNAMES: &[&str] = &[
    "prop_dynamic",
    "prop_dynamic_override",
    "prop_physics",
    "prop_physics_multiplayer",
    "prop_physics_override",
];

fn partitioned_entity_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for entity in bsp.entities.iter() {
        let Some(classname) = entity.prop("classname").filter(|classname| {
            ENTITY_PROP_CLASSNAMES
                .iter()
                .any(|expected| classname.eq_ignore_ascii_case(expected))
        }) else {
            continue;
        };
        let Some(placement) = entity_prop_placement(entity, classname, bsp, cluster_count) else {
            continue;
        };
        if entity.prop("parentname").is_some()
            || entity.prop("parentattachment").is_some()
            || entity.prop("moveparent").is_some()
        {
            log::debug!(
                "bsp entity prop {classname} has parent/attachment fields; rendering at own origin"
            );
        }
        match partition.point_partition(bsp, placement.origin) {
            GeometryPartition::Visible => visible.push(placement),
            GeometryPartition::Skybox => skybox.push(placement),
        }
    }
    (visible, skybox)
}

fn entity_prop_placement(
    entity: &MapEntity,
    classname: &str,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<StaticPropPlacement> {
    let Some(model) = entity.prop("model") else {
        log::debug!("bsp entity prop {classname} skipped: missing model");
        return None;
    };
    let Some(model_path) = normalize_entity_prop_model_path(model) else {
        log::debug!("bsp entity prop {classname} skipped: invalid model {model:?}");
        return None;
    };
    let Some(origin) = entity.prop("origin") else {
        log::debug!("bsp entity prop {classname} skipped: missing origin");
        return None;
    };
    let Some(origin) = parse_entity_vec3(origin) else {
        log::debug!("bsp entity prop {classname} skipped: invalid origin");
        return None;
    };
    let angles = entity.prop("angles").map_or([0.0; 3], |value| {
        parse_entity_vec3(value).unwrap_or_else(|| {
            log::debug!("bsp entity prop {classname} angles invalid: defaulting to 0 0 0");
            [0.0; 3]
        })
    });
    let skin = entity.prop("skin").map_or(0, |value| {
        parse_entity_i32(value).unwrap_or_else(|| {
            log::debug!("bsp entity prop {classname} skin invalid: defaulting to 0");
            0
        })
    });
    let scale = entity
        .prop("modelscale")
        .map_or(1.0, |value| parse_entity_prop_model_scale(value, classname));
    let solid = entity_prop_solid(entity);

    Some(StaticPropPlacement {
        model_path,
        origin,
        angles,
        skin,
        scale,
        solid,
        visibility: point_visibility_bucket(bsp, origin, cluster_count).into(),
    })
}

fn static_prop_solid(solid: u8) -> MapPropSolid {
    match solid {
        STATIC_PROP_SOLID_PHYSICS => MapPropSolid::Physics,
        _ => MapPropSolid::None,
    }
}

fn entity_prop_solid(entity: &MapEntity) -> MapPropSolid {
    if entity
        .prop("startdisabled")
        .is_some_and(|value| parse_entity_bool(value).unwrap_or(false))
    {
        return MapPropSolid::None;
    }
    match entity.prop("solid").and_then(parse_entity_i32) {
        Some(6) => MapPropSolid::Physics,
        _ => MapPropSolid::None,
    }
}

fn parse_entity_prop_model_scale(value: &str, classname: &str) -> f32 {
    let Some(scale) = parse_entity_float(value).filter(|scale| *scale > 0.0) else {
        log::debug!("bsp entity prop {classname} modelscale invalid: defaulting to 1.0");
        return 1.0;
    };
    scale
}

fn partitioned_detail_sprite_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<MapDetailSprite>, Vec<MapDetailSprite>) {
    let sprites = bsp.detail_sprites();
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for prop in bsp.detail_props_iter() {
        // vformats names the kind byte `prop_type` and the dict index
        // `model_index` — the reverse of vbsp's `detail_type`/`prop_type`.
        if prop.prop_type != DETAIL_PROP_TYPE_SPRITE {
            continue;
        }
        let Some(sprite) = sprites.get(usize::from(prop.model_index)) else {
            log::debug!(
                "bsp detail sprite placement skipped: sprite dict index {} missing",
                prop.model_index
            );
            continue;
        };
        let placement = MapDetailSprite {
            origin: prop.origin,
            upper_left: sprite.upper_left,
            lower_right: sprite.lower_right,
            tex_upper_left: sprite.tex_upper_left,
            tex_lower_right: sprite.tex_lower_right,
            visibility: point_visibility_bucket(bsp, prop.origin, cluster_count),
        };
        match partition.point_partition(bsp, placement.origin) {
            GeometryPartition::Visible => visible.push(placement),
            GeometryPartition::Skybox => skybox.push(placement),
        }
    }
    (visible, skybox)
}

fn partitioned_map_overlays(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<MapOverlay>, Vec<MapOverlay>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for (overlay, mapped) in bsp.overlays.iter().filter_map(|overlay| {
        let texinfo = usize::try_from(overlay.texinfo)
            .ok()
            .and_then(|index| bsp.texinfos.get(index))?;
        let material_name = normalize_material_name(bsp.texinfo_name(texinfo))?;
        if !is_preview_material_visible(&material_name) {
            return None;
        }
        Some((
            overlay,
            MapOverlay {
                id: overlay.id,
                material_name,
                positions: overlay_quad_positions(OverlayBasis::from_overlay(overlay))?,
                normal: normalize(overlay.basis_normal),
                u: overlay.u,
                v: overlay.v,
                face_count: overlay.face_count().try_into().unwrap_or(u16::MAX),
                visibility: point_visibility_bucket(bsp, overlay.origin, cluster_count),
            },
        ))
    }) {
        match partition.point_partition(bsp, overlay.origin) {
            GeometryPartition::Visible => visible.push(mapped),
            GeometryPartition::Skybox => skybox.push(mapped),
        }
    }
    (visible, skybox)
}

fn point_visibility_bucket(
    bsp: &MapBsp,
    point: [f32; 3],
    cluster_count: u32,
) -> MapVisibilityBucket {
    point_cluster(bsp, point).map_or(MapVisibilityBucket::Always, |cluster| {
        visibility_bucket(cluster, cluster_count)
    })
}

/// The overlay projection basis: just the fields
/// [`overlay_quad_positions`] needs, decoupled from
/// [`vformats::bsp::Overlay`] (whose packed face-table fields are
/// private) so tests can build fixtures with a plain struct literal.
#[derive(Debug, Clone, Copy)]
struct OverlayBasis {
    id: i32,
    basis_normal: [f32; 3],
    uv_points: [[f32; 3]; 4],
    origin: [f32; 3],
}

impl OverlayBasis {
    fn from_overlay(overlay: &Overlay) -> Self {
        Self {
            id: overlay.id,
            basis_normal: overlay.basis_normal,
            uv_points: overlay.uv_points,
            origin: overlay.origin,
        }
    }
}

fn overlay_quad_positions(overlay: OverlayBasis) -> Option<[[f32; 3]; 4]> {
    let normal = normalize(overlay.basis_normal);
    if !vector_is_finite_nonzero(normal) {
        log::debug!("bsp overlay {} skipped: invalid basis normal", overlay.id);
        return None;
    }
    // vbsp packs the overlay's real U basis into the z components of the
    // first three UV points and flags a flipped V basis via
    // uv_points[3].z == 1.0 (source-sdk-2013 utils/vbsp/overlay.cpp:
    // vecUVPoints[i].z = vecBasis[0][i]; [3].z = 1.0 when
    // cross(normal, basisU) . basisV < 0). The xy pairs are corner
    // coordinates in that basis — z is NOT a normal offset.
    let uv_points = overlay.uv_points;
    let u_axis = normalize([uv_points[0][2], uv_points[1][2], uv_points[2][2]]);
    if !vector_is_finite_nonzero(u_axis) {
        log::debug!(
            "bsp overlay {} skipped: degenerate packed U basis",
            overlay.id
        );
        return None;
    }
    let mut v_axis = normalize(cross(normal, u_axis));
    if uv_points[3][2] == 1.0 {
        v_axis = mul(v_axis, -1.0);
    }
    let origin = overlay.origin;
    // Preview simplification: render one quad from the overlay plane points
    // and do not clip it back to the referenced faces.
    Some(uv_points.map(|point| add(origin, add(mul(u_axis, point[0]), mul(v_axis, point[1])))))
}

fn worldspawn_detail_material_name(entities: &[MapEntity]) -> String {
    entities
        .iter()
        .find(|entity| entity.prop("classname") == Some("worldspawn"))
        .and_then(|entity| entity.prop("detailmaterial"))
        .and_then(normalize_material_name)
        .unwrap_or_else(|| DEFAULT_DETAIL_MATERIAL.to_owned())
}

fn info_player_start(entities: &[MapEntity]) -> Option<MapPlayerStart> {
    entities
        .iter()
        .filter(|entity| entity.prop("classname") == Some("info_player_start"))
        .find_map(|entity| {
            Some(MapPlayerStart {
                origin: parse_entity_vec3(entity.prop("origin")?)?,
                angles: entity
                    .prop("angles")
                    .and_then(parse_entity_vec3)
                    .or_else(|| entity.prop("angle").and_then(parse_entity_yaw_angle))
                    .unwrap_or([0.0; 3]),
            })
        })
}

fn map_sky_camera(entities: &[MapEntity]) -> Option<MapSkyCamera> {
    entities
        .iter()
        .filter(|entity| entity.prop("classname") == Some("sky_camera"))
        .find_map(|entity| {
            let origin = parse_entity_vec3(entity.prop("origin")?)?;
            let scale = entity
                .prop("scale")
                .and_then(parse_entity_float)
                .filter(|scale| *scale > 0.0)
                .unwrap_or(16.0);
            Some(MapSkyCamera {
                origin,
                scale,
                fog: parse_map_fog(
                    entity.prop("fogenable"),
                    entity.prop("fogcolor"),
                    entity.prop("fogstart"),
                    entity.prop("fogend"),
                    entity.prop("fogmaxdensity"),
                    sky_camera_fog_enabled,
                ),
            })
        })
}

fn worldspawn_skyname(entities: &[MapEntity]) -> Option<String> {
    entities
        .iter()
        .find(|entity| entity.prop("classname") == Some("worldspawn"))
        .and_then(|entity| entity.prop("skyname"))
        .and_then(normalize_skyname)
}

fn map_fog(entities: &[MapEntity]) -> Option<MapFog> {
    entities
        .iter()
        .find(|entity| {
            entity.prop("classname") == Some("env_fog_controller")
                && entity.prop("fogenable").is_some_and(fog_bool_enabled)
        })
        .and_then(|entity| {
            parse_map_fog(
                entity.prop("fogenable"),
                entity.prop("fogcolor"),
                entity.prop("fogstart"),
                entity.prop("fogend"),
                entity.prop("fogmaxdensity"),
                fog_bool_enabled,
            )
        })
}

fn parse_map_fog(
    fogenable: Option<&str>,
    fogcolor: Option<&str>,
    fogstart: Option<&str>,
    fogend: Option<&str>,
    fogmaxdensity: Option<&str>,
    enabled: fn(&str) -> bool,
) -> Option<MapFog> {
    fogenable.filter(|value| enabled(value))?;
    let fog = MapFog {
        color_srgb: parse_fog_color(fogcolor?)?,
        start: parse_fog_float(fogstart?)?,
        end: parse_fog_float(fogend?)?,
        max_density: fogmaxdensity
            .and_then(parse_fog_float)
            .filter(|density| (0.0..=1.0).contains(density))
            .unwrap_or(1.0),
    };
    (fog.end > fog.start).then_some(fog)
}

fn parse_entity_vec3(value: &str) -> Option<[f32; 3]> {
    let mut components = value.split_ascii_whitespace().map(parse_entity_float);
    let x = components.next()??;
    let y = components.next()??;
    let z = components.next()??;
    components.next().is_none().then_some([x, y, z])
}

fn parse_entity_yaw_angle(value: &str) -> Option<[f32; 3]> {
    Some([0.0, parse_entity_float(value)?, 0.0])
}

fn parse_entity_float(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_entity_i32(value: &str) -> Option<i32> {
    value.trim().parse::<i32>().ok()
}

fn parse_entity_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        return Some(false);
    }
    parse_entity_i32(value).map(|value| value != 0)
}

fn fog_bool_enabled(value: &str) -> bool {
    value
        .trim()
        .parse::<f32>()
        .is_ok_and(|value| value.is_finite() && value != 0.0)
}

fn sky_camera_fog_enabled(value: &str) -> bool {
    value
        .trim()
        .parse::<f32>()
        .is_ok_and(|value| value.is_finite() && value == 1.0)
}

fn parse_fog_float(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_fog_color(value: &str) -> Option<[u8; 3]> {
    let mut components = value.split_ascii_whitespace().map(str::parse::<u8>).take(4);
    let red = components.next()?.ok()?;
    let green = components.next()?.ok()?;
    let blue = components.next()?.ok()?;
    components.next().is_none().then_some([red, green, blue])
}

fn normalize_skyname(value: &str) -> Option<String> {
    let value = value.trim().replace('\\', "/");
    let value = value.trim_matches('/');
    let mut segments = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        segments.push(segment);
    }
    let value = segments.join("/");
    (!value.is_empty()).then_some(value.to_ascii_lowercase())
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn mul(vector: [f32; 3], scalar: f32) -> [f32; 3] {
    [vector[0] * scalar, vector[1] * scalar, vector[2] * scalar]
}

fn lerp(start: [f32; 3], end: [f32; 3], fraction: f32) -> [f32; 3] {
    add(start, mul(sub(end, start), fraction))
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn dot_abs(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].abs() * right[0] + left[1].abs() * right[1] + left[2].abs() * right[2]
}

fn length_squared(vector: [f32; 3]) -> f32 {
    vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]
}

fn distance_squared(left: [f32; 3], right: [f32; 3]) -> f32 {
    length_squared(sub(left, right))
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = length_squared(vector).sqrt();
    if length <= f32::EPSILON {
        [0.0; 3]
    } else {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    }
}

fn vector_is_finite_nonzero(vector: [f32; 3]) -> bool {
    vector.into_iter().all(f32::is_finite) && length_squared(vector) > f32::EPSILON
}

fn fan_indices(vertex_count: usize) -> Result<Vec<u32>, BspError> {
    if vertex_count < 3 {
        return Ok(Vec::new());
    }

    let mut indices = Vec::with_capacity((vertex_count - 2) * 3);
    for index in 1..vertex_count - 1 {
        indices.push(0);
        indices.push(u32::try_from(index).map_err(|_| BspError::TooLarge { item: "indices" })?);
        indices.push(u32::try_from(index + 1).map_err(|_| BspError::TooLarge { item: "indices" })?);
    }
    Ok(indices)
}

fn face_normal(bsp: &MapBsp, face: &Face) -> [f32; 3] {
    let normal = bsp.face_plane_normal(face);
    if face.side == 0 {
        normal
    } else {
        [-normal[0], -normal[1], -normal[2]]
    }
}

fn is_water_underside_face(texinfo: &TexInfo, normal: [f32; 3]) -> bool {
    texinfo.flags & texture_flags::WARP != 0 && normal[2] < 0.0
}

fn is_preview_material_visible(material: &str) -> bool {
    !material.starts_with("tools/")
        && !material.contains("skybox/")
        && !matches!(material, "sky" | "skybox")
}

fn normalize_material_name(path: &str) -> Option<String> {
    normalize_source_path(path, Some(".vmt"))
}

fn normalize_static_prop_model_path(path: &str) -> Option<String> {
    let path = normalize_source_path(path, None)?;
    let is_mdl = std::path::Path::new(&path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"));
    is_mdl.then_some(path)
}

fn normalize_entity_prop_model_path(path: &str) -> Option<String> {
    let path = normalize_static_prop_model_path(path)?;
    path.starts_with("models/").then_some(path)
}

fn normalize_pakfile_path(path: &str) -> Option<String> {
    normalize_source_path(path, None)
}

fn normalize_source_path(path: &str, extension: Option<&str>) -> Option<String> {
    let mut path = path.trim().replace('\\', "/");
    path = path.trim_matches('/').to_owned();
    if extension.is_some()
        && let Some(stripped) = strip_prefix_ascii_case(&path, "materials/")
    {
        path = stripped.to_owned();
    }
    if let Some(extension) = extension
        && path
            .get(path.len().saturating_sub(extension.len())..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
    {
        path.truncate(path.len() - extension.len());
    }

    let mut normalized = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        normalized.push(segment);
    }

    let path = normalized.join("/");
    (!path.is_empty()).then_some(path.to_ascii_lowercase())
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn is_pakfile_material_entry(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vmt") || ext.eq_ignore_ascii_case("vtf"))
}

fn is_pakfile_model_entry(path: &str) -> bool {
    path.starts_with("models/")
        && std::path::Path::new(path).extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("mdl")
                || ext.eq_ignore_ascii_case("vvd")
                || ext.eq_ignore_ascii_case("vtx")
                || ext.eq_ignore_ascii_case("phy")
        })
}

fn is_pakfile_retained_entry(path: &str) -> bool {
    is_pakfile_material_entry(path) || is_pakfile_model_entry(path)
}

fn is_pakfile_entry_oversized(size_bytes: u64) -> bool {
    size_bytes > MAX_PAKFILE_ENTRY_BYTES
}

fn bounds_from_meshes(meshes: &[MapMesh]) -> ([f32; 3], [f32; 3]) {
    let Some(first) = meshes
        .iter()
        .flat_map(|mesh| mesh.vertices.iter())
        .map(|vertex| vertex.position)
        .next()
    else {
        return ([0.0; 3], [0.0; 3]);
    };

    let mut min = first;
    let mut max = first;
    for position in meshes
        .iter()
        .flat_map(|mesh| mesh.vertices.iter())
        .map(|vertex| vertex.position)
    {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    (min, max)
}

fn decode_error(error: impl fmt::Display) -> BspError {
    BspError::Decode {
        message: error.to_string(),
    }
}

fn count_to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
