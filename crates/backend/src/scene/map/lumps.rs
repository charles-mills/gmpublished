use super::{
    BTreeSet, Brush, BrushSide, BspError, BspModel, ColorRgbExp, Cow, DetailProp, DetailProps,
    DispInfo, DispVert, Face, KvValue, Leaf, LeafAmbientIndex, LeafAmbientSample, Limits, Node,
    Overlay, Plane, StaticProp, StaticProps, TexData, TexInfo, Visibility, bsp, fmt, pakfile_error,
    texture_flags,
};
use crate::math::Vec3;

/// `SURF_TRIGGER`/`SURF_HINT`/`SURF_SKIP` (`bspfile.h`): not in
/// [`texture_flags`] because they're rarely needed, but
/// [`MapBsp::face_is_visible`] checks them alongside SKY/SKY2D/NODRAW.
const SURF_TRIGGER: i32 = 0x0040;
const SURF_HINT: i32 = 0x0100;
const SURF_SKIP: i32 = 0x0200;

const BSP_MAGIC: &[u8; 4] = b"VBSP";

/// One entity's flat string keyvalue pairs (real Source entity lumps
/// have no nested blocks, so [`vformats::keyvalues::KvValue::Block`]
/// pairs are dropped). Lookup is exact-match, first-occurrence-wins,
/// matching vbsp's own `RawEntity::prop` (vformats' `KvDocument::get_str`
/// is case-insensitive, which is not the same lookup). Both keys and
/// values are ASCII-lowercased at construction: vbsp's GMod-tolerance
/// fork lowercases the whole entities lump text before parsing it
/// (`reader::LumpReader::read_entities`), so wild content's inconsistent
/// `WorldSpawn`/`worldspawn` classname casing (etc.) still matches the
/// lowercase literal keys/values every lookup across `scene::map`
/// compares against. Lowercasing per-pair after parsing is equivalent: ASCII
/// case-folding never changes token boundaries or UTF-8 validity.
#[derive(Debug, Clone, Default)]
pub(super) struct MapEntity {
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

    pub(super) fn prop(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    /// Whether this is an entity of `classname`.
    ///
    /// Named rather than spelled `prop("classname") == Some(..)` at each use:
    /// `classname` is the one key every lookup starts from, and a misspelling
    /// of it silently matches nothing rather than failing.
    pub(super) fn is_class(&self, classname: &str) -> bool {
        self.prop("classname") == Some(classname)
    }
}

/// Every BSP lump the scene builder needs, decoded once at load time from
/// [`bsp::parse`] into owned storage (no borrow of the source bytes) so
/// it can be threaded through the whole `load_map` pipeline by reference.
pub(super) struct MapBsp {
    pub(super) vertices: Vec<Vec3>,
    pub(super) planes: Vec<Plane>,
    pub(super) edges: Vec<[u16; 2]>,
    pub(super) surfedges: Vec<i32>,
    pub(super) faces: Vec<Face>,
    pub(super) texinfos: Vec<TexInfo>,
    pub(super) texdatas: Vec<TexData>,
    pub(super) texdata_strings: Vec<String>,
    pub(super) models: Vec<BspModel>,
    pub(super) brushes: Vec<Brush>,
    pub(super) brush_sides: Vec<BrushSide>,
    pub(super) nodes: Vec<Node>,
    pub(super) leaves: Vec<Leaf>,
    pub(super) leaf_faces: Vec<u16>,
    pub(super) leaf_brushes: Vec<u16>,
    pub(super) displacements: Vec<DispInfo>,
    pub(super) displacement_verts: Vec<DispVert>,
    pub(super) lighting: Vec<ColorRgbExp>,
    pub(super) lighting_hdr: Vec<ColorRgbExp>,
    pub(super) leaf_ambient_lighting: Vec<LeafAmbientSample>,
    pub(super) leaf_ambient_lighting_hdr: Vec<LeafAmbientSample>,
    pub(super) leaf_ambient_indices: Vec<LeafAmbientIndex>,
    pub(super) leaf_ambient_indices_hdr: Vec<LeafAmbientIndex>,
    pub(super) overlays: Vec<Overlay>,
    pub(super) entities: Vec<MapEntity>,
    pub(super) visibility: Option<Visibility<'static>>,
    pub(super) static_props: Option<StaticProps>,
    pub(super) detail_props: Option<DetailProps>,
    /// The raw pakfile lump bytes; parsed on demand via
    /// [`ZipReader`](vformats::bsp::ZipReader)
    /// (its reader borrows, so it cannot be stored on `Self`).
    pub(super) pakfile_bytes: Vec<u8>,
}

impl MapBsp {
    /// Decodes every lump the scene builder needs. The per-lump decodes are
    /// independent passes over disjoint byte ranges — and on repacked
    /// workshop maps each one is an LZMA decompression that dominates load
    /// time — so they run in one `rayon::scope`. Only the *selected*
    /// lighting set is decoded: `selected_lightmap_samples` /
    /// `selected_ambient_lighting` prefer LDR and use exactly one of each
    /// pair, and the preference is decidable from raw lump lengths (an
    /// empty raw lump decodes to an empty vec) before decompressing
    /// anything.
    pub(super) fn parse(bytes: &[u8], limits: &Limits) -> Result<Self, BspError> {
        use bsp::lump_ids as ids;
        let bsp = bsp::parse(bytes, limits).map_err(decode_error)?;

        let raw_len = |id: usize| bsp.lump(id).map_or(0, <[u8]>::len);
        let use_ldr_lightmap = raw_len(ids::LIGHTING) > 0;
        let want_lighting_hdr = !use_ldr_lightmap && raw_len(ids::LIGHTING_HDR) > 0;
        let ldr_ambient =
            raw_len(ids::LEAF_AMBIENT_LIGHTING) > 0 && raw_len(ids::LEAF_AMBIENT_INDEX) > 0;
        let want_ambient_hdr = !ldr_ambient
            && raw_len(ids::LEAF_AMBIENT_LIGHTING_HDR) > 0
            && raw_len(ids::LEAF_AMBIENT_INDEX_HDR) > 0;

        macro_rules! par_decode {
            ($( $slot:ident = $decode:expr; )+) => {
                $( let mut $slot = None; )+
                rayon::scope(|s| {
                    $( s.spawn(|_| $slot = Some($decode)); )+
                });
                $( let $slot = $slot.expect("rayon scope runs every spawn to completion"); )+
            };
        }

        par_decode! {
            entities = bsp.entities(limits);
            visibility = bsp.visibility(limits);
            faces = bsp.faces(limits);
            edges = bsp.edges(limits);
            surfedges = bsp.surfedges(limits);
            vertices = bsp.vertices(limits);
            planes = bsp.planes(limits);
            texinfos = bsp.texinfos(limits);
            texdatas = bsp.texdatas(limits);
            texdata_strings = bsp.texdata_strings(limits);
            models = bsp.models(limits);
            brushes = bsp.brushes(limits);
            brush_sides = bsp.brush_sides(limits);
            nodes = bsp.nodes(limits);
            leaves = bsp.leafs(limits);
            leaf_faces = bsp.leaf_faces(limits);
            leaf_brushes = bsp.leaf_brushes(limits);
            displacements = bsp.displacement_infos(limits);
            displacement_verts = bsp.displacement_verts(limits);
            lighting = if use_ldr_lightmap { bsp.lighting(limits) } else { Ok(Vec::new()) };
            lighting_hdr = if want_lighting_hdr { bsp.lighting_hdr(limits) } else { Ok(Vec::new()) };
            leaf_ambient_lighting = if ldr_ambient { bsp.leaf_ambient_lighting(limits) } else { Ok(Vec::new()) };
            leaf_ambient_indices = if ldr_ambient { bsp.leaf_ambient_indices(limits) } else { Ok(Vec::new()) };
            leaf_ambient_lighting_hdr = if want_ambient_hdr { bsp.leaf_ambient_lighting_hdr(limits) } else { Ok(Vec::new()) };
            leaf_ambient_indices_hdr = if want_ambient_hdr { bsp.leaf_ambient_indices_hdr(limits) } else { Ok(Vec::new()) };
            overlays = bsp.overlays(limits);
            static_props = bsp.static_props(limits);
            detail_props = bsp.detail_props(limits);
            pakfile_bytes = Ok::<_, BspError>(bsp.lump(ids::PAKFILE).unwrap_or_default().to_vec());
        }

        let entities = entities
            .map_err(decode_error)?
            .iter()
            .map(MapEntity::from_document)
            .collect();
        let visibility = visibility
            .map_err(decode_error)?
            .map(Visibility::into_owned);
        let mut faces = faces.map_err(decode_error)?;
        let edges = edges.map_err(decode_error)?;
        let surfedges = surfedges.map_err(decode_error)?;
        let vertices = vertices.map_err(decode_error)?;
        discard_faces_with_invalid_vertices(&mut faces, &surfedges, &edges, vertices.len());

        Ok(Self {
            planes: planes.map_err(decode_error)?,
            texinfos: texinfos.map_err(decode_error)?,
            texdatas: texdatas.map_err(decode_error)?,
            texdata_strings: texdata_strings
                .map_err(decode_error)?
                .into_iter()
                .map(Cow::into_owned)
                .collect(),
            models: models.map_err(decode_error)?,
            brushes: brushes.map_err(decode_error)?,
            brush_sides: brush_sides.map_err(decode_error)?,
            nodes: nodes.map_err(decode_error)?,
            leaves: leaves.map_err(decode_error)?,
            leaf_faces: leaf_faces.map_err(decode_error)?,
            leaf_brushes: leaf_brushes.map_err(decode_error)?,
            displacements: displacements.map_err(decode_error)?,
            displacement_verts: displacement_verts.map_err(decode_error)?,
            lighting: lighting.map_err(decode_error)?,
            lighting_hdr: lighting_hdr.map_err(decode_error)?,
            leaf_ambient_lighting: leaf_ambient_lighting.map_err(decode_error)?,
            leaf_ambient_lighting_hdr: leaf_ambient_lighting_hdr.map_err(decode_error)?,
            leaf_ambient_indices: leaf_ambient_indices.map_err(decode_error)?,
            leaf_ambient_indices_hdr: leaf_ambient_indices_hdr.map_err(decode_error)?,
            overlays: overlays.map_err(decode_error)?,
            static_props: static_props.map_err(decode_error)?,
            detail_props: detail_props.map_err(decode_error)?,
            vertices: vertices.into_iter().map(Vec3::from).collect(),
            faces,
            edges,
            surfedges,
            entities,
            visibility,
            pakfile_bytes: pakfile_bytes.map_err(pakfile_error)?,
        })
    }

    pub(super) fn face(&self, index: usize) -> Option<&Face> {
        self.faces.get(index)
    }

    pub(super) fn face_texinfo(&self, face: &Face) -> Option<&TexInfo> {
        usize::try_from(face.texinfo)
            .ok()
            .and_then(|index| self.texinfos.get(index))
    }

    pub(super) fn texinfo_texdata(&self, texinfo: &TexInfo) -> Option<&TexData> {
        usize::try_from(texinfo.texdata)
            .ok()
            .and_then(|index| self.texdatas.get(index))
    }

    /// The resolved texture name for `texinfo`'s texdata, or `""` when
    /// either lookup misses (matches the empty-string fallback the
    /// preview mesh builder already tolerates for stray content).
    pub(super) fn texinfo_name(&self, texinfo: &TexInfo) -> &str {
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
    pub(super) fn face_vertex_positions(&self, face: &Face) -> Vec<Vec3> {
        self.face_vertex_indices(face)
            .filter_map(|index| self.vertices.get(usize::from(index)).copied())
            .collect()
    }

    pub(super) fn face_vertex_indices(&self, face: &Face) -> impl Iterator<Item = u16> + '_ {
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

    pub(super) fn face_displacement(&self, face: &Face) -> Option<&DispInfo> {
        usize::try_from(face.displacement)
            .ok()
            .and_then(|index| self.displacements.get(index))
    }

    /// The plane normal for `face`'s own side (not yet flipped for
    /// `face.side`; see the free function `face_normal`).
    pub(super) fn face_plane_normal(&self, face: &Face) -> Vec3 {
        self.planes
            .get(usize::from(face.plane))
            .map_or(Vec3::ZERO, |plane| Vec3::from(plane.normal))
    }

    /// vbsp `Handle<Face>::is_visible`: false for sky, 2D-sky, trigger,
    /// hint, skip, and nodraw surfaces. A face whose texinfo is out of
    /// range (never happens for validated vbsp input, but this crate
    /// does not cross-validate at parse) is treated as not visible
    /// rather than assumed drawable.
    pub(super) fn face_is_visible(&self, face: &Face) -> bool {
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
    pub(super) fn displacement_vertices(&self, disp: &DispInfo) -> impl Iterator<Item = &DispVert> {
        let start = i64::from(disp.vert_start);
        let count = displacement_vertex_count(disp.power);
        (start..start.saturating_add(count))
            .filter_map(|index| self.displacement_verts.get(usize::try_from(index).ok()?))
    }

    /// The static prop placements (absent game lump yields none).
    pub(super) fn static_props_iter(&self) -> impl Iterator<Item = &StaticProp> {
        self.static_props
            .iter()
            .flat_map(|props| props.props.iter())
    }

    pub(super) fn static_prop_model<'s>(&'s self, prop: &StaticProp) -> &'s str {
        self.static_props
            .as_ref()
            .and_then(|props| props.models.get(usize::from(prop.model_index)))
            .map_or("", String::as_str)
    }

    /// The prop's leaf span into the game lump's shared leaf table (used
    /// to derive multi-cluster visibility).
    pub(super) fn static_prop_leaves(&self, prop: &StaticProp) -> Option<&[u16]> {
        let props = self.static_props.as_ref()?;
        let start = usize::from(prop.first_leaf);
        let end = start.checked_add(usize::from(prop.leaf_count))?;
        props.leaves.get(start..end)
    }

    pub(super) fn detail_sprites(&self) -> &[vformats::bsp::DetailSprite] {
        self.detail_props
            .as_ref()
            .map_or(&[], |props| props.sprites.as_slice())
    }

    pub(super) fn detail_props_iter(&self) -> &[DetailProp] {
        self.detail_props
            .as_ref()
            .map_or(&[], |props| props.props.as_slice())
    }

    /// The map's visibility cluster count, or 0 when the map has no
    /// visibility lump (fullbright/unvised).
    pub(super) fn cluster_count(&self) -> u32 {
        self.visibility
            .as_ref()
            .map_or(0, |vis| vis.cluster_count() as u32)
    }

    /// Every cluster reachable from `from` by following PVS edges
    /// (treated as an undirected reachability graph, matching the flood
    /// fill vbsp's `LazyVisData::reachable_clusters` used for skybox
    /// partitioning — ported here since visibility derivation is scene
    /// assembly, not something `vformats::bsp` does itself).
    pub(super) fn reachable_clusters(&self, from: i16) -> Vec<bool> {
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
        // u64-bitset fill: the BFS visits ~every cluster, and a Vec<bool>
        // row per visit is O(clusters²) byte traffic plus an allocation per
        // cluster. Word-wise rows into a reusable buffer are ~64× less
        // traffic; expand to Vec<bool> once at the end for the consumers.
        let words = cluster_count.div_ceil(64);
        let mut reached = vec![0u64; words];
        let mut row = vec![0u64; words];
        reached[start / 64] |= 1u64 << (start % 64);
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(cluster) = queue.pop_front() {
            if vis.pvs_into(cluster, &mut row).is_none() {
                continue;
            }
            for (word, row_word) in row.iter().enumerate() {
                let mut new = row_word & !reached[word];
                reached[word] |= new;
                while new != 0 {
                    let bit = new.trailing_zeros() as usize;
                    queue.push_back(word * 64 + bit);
                    new &= new - 1;
                }
            }
        }
        (0..cluster_count)
            .map(|cluster| (reached[cluster / 64] >> (cluster % 64)) & 1 == 1)
            .collect()
    }

    /// Find the leaf containing `point` (see [`walk_to_leaf`]).
    pub(super) fn leaf_at(&self, point: Vec3) -> Option<usize> {
        walk_to_leaf(
            point,
            self.nodes.len(),
            |index| {
                self.nodes
                    .get(index)
                    .map(|node| (node.plane, node.children))
            },
            |index| {
                let plane = self.planes.get(usize::try_from(index).ok()?)?;
                Some((Vec3::from(plane.normal), plane.dist))
            },
        )
    }
}

/// Walk a BSP tree from the root to the leaf containing `point`
/// (ported from vbsp's `Bsp::leaf_at`, MIT, © icewind1991). Node and
/// plane lookup are closures because the two callers store the tree
/// differently: [`MapBsp`] as the decoded lumps,
/// [`MapLeafLocator`](super::MapLeafLocator) as
/// a retained projection of them.
///
/// `node_count` bounds the descent. Node children are read verbatim from
/// the file — `vformats` validates nothing about them — so wild content can
/// point a child back up the tree, and an unbounded walk spins forever. That
/// is not merely a stalled worker: `leaf_at` is reached per frame from the
/// map previewer's camera (`MapVisibility::cluster_at`), so a cycle wedges
/// the render thread. An acyclic walk descends into a distinct node each
/// step and so cannot visit more nodes than exist; needing more than that
/// means a node repeated, and `None` (leaf unknown) is the same answer
/// callers already handle for an out-of-range index.
pub(super) fn walk_to_leaf(
    point: Vec3,
    node_count: usize,
    node: impl Fn(usize) -> Option<(i32, [i32; 2])>,
    plane: impl Fn(i32) -> Option<(Vec3, f32)>,
) -> Option<usize> {
    let mut current_index = 0usize;
    for _ in 0..node_count {
        let (plane_index, children) = node(current_index)?;
        let (normal, dist) = plane(plane_index)?;
        let distance = point[0] * normal[0] + point[1] * normal[1] + point[2] * normal[2];
        let [front, back] = children;
        let next = if distance < dist { back } else { front };
        match NodeChild::decode(next)? {
            NodeChild::Leaf(leaf_index) => return Some(leaf_index),
            NodeChild::Node(node_index) => current_index = node_index,
        }
    }
    None
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

/// Reads the container version from the first eight bytes.
///
/// `bsp::parse` also reports a version, but only after a full 1036-byte header
/// and lump directory. Anything shorter than that fails there as `Truncated`,
/// so a truncated map of a version this build cannot open would be diagnosed as
/// a broken file rather than an unsupported one. Eight bytes is all the version
/// costs, and it is read before the parse for exactly that reason.
pub(super) fn bsp_version(bytes: &[u8]) -> Result<u32, BspError> {
    let Some(header) = bytes.get(..8) else {
        return Err(BspError::Malformed {
            message: "header too short",
        });
    };
    if &header[..4] != BSP_MAGIC {
        return Err(BspError::Malformed {
            message: "missing VBSP magic",
        });
    }
    Ok(u32::from_le_bytes(
        header[4..8]
            .try_into()
            .expect("slice length was checked above"),
    ))
}

/// Index into [`MapBsp::models`] — a brush model ("bmodel"), the geometry a
/// brush entity like a door refers to as `*3`.
///
/// Distinct from [`BrushIndex`] and from a static prop's model index, which
/// names an entry in the game lump's model table. All three are `usize`-shaped
/// and none is interchangeable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelIndex(usize);

impl ModelIndex {
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for ModelIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A BSP node's child link, decoded.
///
/// The file stores one `i32` per child: non-negative is an index into
/// [`MapBsp::nodes`], negative is the bitwise complement of an index into
/// [`MapBsp::leaves`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NodeChild {
    Node(usize),
    Leaf(usize),
}

impl NodeChild {
    /// `None` when a node index does not fit `usize`. Leaf links always fit —
    /// the complement of a negative `i32` is non-negative.
    pub(super) fn decode(raw: i32) -> Option<Self> {
        if raw < 0 {
            usize::try_from(!raw).ok().map(Self::Leaf)
        } else {
            usize::try_from(raw).ok().map(Self::Node)
        }
    }
}

/// Index into [`MapBsp::brushes`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BrushIndex(usize);

impl BrushIndex {
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MapPlane {
    pub(super) normal: Vec3,
    pub(super) dist: f32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct MapNode {
    pub(super) plane_index: i32,
    pub(super) children: [i32; 2],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct MapLeaf {
    pub(super) cluster: i16,
    pub(super) mins: [i16; 3],
    pub(super) maxs: [i16; 3],
}

pub(super) fn brush_indices_for_model(
    bsp: &MapBsp,
    model_index: ModelIndex,
) -> BTreeSet<BrushIndex> {
    let mut brushes = BTreeSet::new();
    let Some(model) = bsp.models.get(model_index.get()) else {
        return brushes;
    };
    // Iterative with a visited set: wild content can encode a cycle in the
    // node children, and unbounded recursion aborts past catch_unwind.
    let mut visited_nodes = BTreeSet::new();
    let mut stack = vec![model.head_node];
    while let Some(child) = stack.pop() {
        match NodeChild::decode(child) {
            Some(NodeChild::Leaf(leaf_index)) => {
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
                        .map(|leaf_brush| BrushIndex::new(usize::from(*leaf_brush))),
                );
            }
            Some(NodeChild::Node(node_index)) => {
                if !visited_nodes.insert(node_index) {
                    continue;
                }
                let Some(node) = bsp.nodes.get(node_index) else {
                    continue;
                };
                stack.extend(node.children);
            }
            None => {}
        }
    }
    brushes
}

fn decode_error(error: bsp::BspError) -> BspError {
    BspError::Decode(error)
}
