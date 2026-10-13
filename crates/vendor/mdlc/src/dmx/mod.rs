use crate::bone_math::{self, Matrix3x4};
use crate::fbx::{FbxOpts, ForcedAxis};
use crate::smd::{Smd, SmdBoneLink, SmdFrame, SmdNode, SmdPose, SmdTriangle, SmdVertex};
use std::collections::{HashMap, HashSet};
use std::path::Path;

mod animation;
mod data;
mod flex;
pub mod flex_rules;

use data::{Document, Node, Value};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metadata {
    pub shapes: Vec<Shape>,
    pub controls: Vec<Control>,
    pub dominators: Vec<Dominator>,
    pub attachments: Vec<crate::model::Attachment>,
    pub has_combination: bool,
    pub active_shapes: Vec<String>,
    pub explicit_rules: Option<flex_rules::ExplicitRules>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub name: String,
    pub vertices: Vec<Delta>,
    pub wrinkle: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub vertex: u32,
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub wrinkle: f32,
    pub speed: f32,
    pub balance: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub name: String,
    pub raw: Vec<String>,
    pub stereo: bool,
    pub eyelid: bool,
    pub eyes_up_down: String,
    pub wrinkle_scales: Vec<f32>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dominator {
    pub controls: Vec<String>,
    pub suppressed: Vec<String>,
}

#[derive(Debug)]
pub struct Geometry {
    pub smd: Smd,
    pub metadata: Metadata,
    pub animation_names: Vec<String>,
}

type Result<T> = std::result::Result<T, String>;

fn node(document: &Document, id: usize) -> Result<&Node> {
    document
        .nodes
        .get(id)
        .ok_or_else(|| format!("DMX element index {id} is out of range"))
}

fn value<'a>(node: &'a Node, key: &str) -> Option<&'a Value> {
    node.attributes.get(key)
}

fn element(node: &Node, key: &str) -> Result<Option<usize>> {
    match value(node, key) {
        None | Some(Value::Element(None)) => Ok(None),
        Some(Value::Element(id)) => Ok(*id),
        _ => Err(format!("DMX {}.{key} must be an element", node.name)),
    }
}

fn elements(node: &Node, key: &str) -> Result<Vec<usize>> {
    match value(node, key) {
        None => Ok(Vec::new()),
        Some(Value::ElementArray(ids)) => Ok(ids.iter().flatten().copied().collect()),
        _ => Err(format!("DMX {}.{key} must be an element array", node.name)),
    }
}

fn required_elements(node: &Node, key: &str) -> Result<Vec<usize>> {
    match value(node, key) {
        None => Ok(Vec::new()),
        Some(Value::ElementArray(ids)) => ids
            .iter()
            .map(|id| {
                id.ok_or_else(|| format!("DMX {}.{key} contains a null transform", node.name))
            })
            .collect(),
        _ => Err(format!("DMX {}.{key} must be an element array", node.name)),
    }
}

fn string(node: &Node, key: &str, default: &str) -> Result<String> {
    match value(node, key) {
        None => Ok(default.into()),
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(format!("DMX {}.{key} must be a string", node.name)),
    }
}

fn boolean(node: &Node, key: &str, default: bool) -> Result<bool> {
    match value(node, key) {
        None => Ok(default),
        Some(Value::Bool(v)) => Ok(*v),
        _ => Err(format!("DMX {}.{key} must be a boolean", node.name)),
    }
}

fn integer(node: &Node, key: &str, default: i32) -> Result<i32> {
    match value(node, key) {
        None => Ok(default),
        Some(Value::Int(v)) => Ok(*v),
        _ => Err(format!("DMX {}.{key} must be an integer", node.name)),
    }
}

fn number(node: &Node, key: &str, default: f32) -> Result<f32> {
    match value(node, key) {
        None => Ok(default),
        Some(Value::Float(v)) if v.is_finite() => Ok(*v),
        _ => Err(format!("DMX {}.{key} must be a finite float", node.name)),
    }
}

fn ints<'a>(node: &'a Node, key: &str) -> Result<&'a [i32]> {
    match value(node, key) {
        None => Ok(&[]),
        Some(Value::IntArray(v)) => Ok(v),
        _ => Err(format!("DMX {}.{key} must be an integer array", node.name)),
    }
}

fn floats<'a>(node: &'a Node, key: &str) -> Result<&'a [f32]> {
    match value(node, key) {
        None => Ok(&[]),
        Some(Value::FloatArray(v)) if v.iter().all(|v| v.is_finite()) => Ok(v),
        _ => Err(format!(
            "DMX {}.{key} must be a finite float array",
            node.name
        )),
    }
}

fn vectors<'a>(node: &'a Node, key: &str) -> Result<&'a [[f32; 3]]> {
    match value(node, key) {
        None => Ok(&[]),
        Some(Value::Vector3Array(v)) if v.iter().flatten().all(|v| v.is_finite()) => Ok(v),
        _ => Err(format!(
            "DMX {}.{key} must be a finite vector3 array",
            node.name
        )),
    }
}

fn strings(node: &Node, key: &str) -> Result<Vec<String>> {
    match value(node, key) {
        None => Ok(Vec::new()),
        Some(Value::StringArray(v)) => Ok(v.clone()),
        _ => Err(format!("DMX {}.{key} must be a string array", node.name)),
    }
}

fn transform(document: &Document, id: usize) -> Result<Matrix3x4> {
    let n = node(document, id)?;
    let position = match value(n, "position") {
        None => [0.; 3],
        Some(Value::Vector3(v)) if v.iter().all(|v| v.is_finite()) => *v,
        _ => return Err(format!("DMX transform {} has an invalid position", n.name)),
    };
    let q = match value(n, "orientation") {
        None => [0., 0., 0., 1.],
        Some(Value::Quaternion(q)) if q.iter().all(|v| v.is_finite()) => *q,
        _ => {
            return Err(format!(
                "DMX transform {} has an invalid orientation",
                n.name
            ));
        }
    };
    let largest = q.iter().copied().map(f32::abs).fold(0., f32::max);
    if largest < 1e-12 {
        return Err(format!("DMX transform {} has a zero quaternion", n.name));
    }
    if (number(n, "scale", 1.)? - 1.).abs() > 1e-6 {
        return Err(format!(
            "DMX skeletal transform {} has scale that Source1 cannot represent",
            n.name
        ));
    }
    Ok(bone_math::quaternion_local_transform(
        bone_math::quaternion_normalize(q.map(|v| v / largest)),
        position,
    ))
}

fn vector(matrix: &Matrix3x4, v: [f32; 3], point: bool) -> [f32; 3] {
    std::array::from_fn(|r| {
        matrix[r * 4] * v[0]
            + matrix[r * 4 + 1] * v[1]
            + matrix[r * 4 + 2] * v[2]
            + if point { matrix[r * 4 + 3] } else { 0. }
    })
}

fn normal(v: [f32; 3]) -> Result<[f32; 3]> {
    let length = v.iter().map(|v| v * v).sum::<f32>().sqrt();
    if length < 1e-12 || !length.is_finite() {
        return Err("DMX mesh contains an invalid normal".into());
    }
    Ok(v.map(|v| v / length))
}

struct Skeleton {
    nodes: Vec<SmdNode>,
    transforms: Vec<Option<usize>>,
    by_transform: HashMap<usize, usize>,
    by_dag: HashMap<usize, usize>,
    joints: Vec<usize>,
    bind: Vec<Matrix3x4>,
    default_bone: usize,
}

impl Skeleton {
    fn load(document: &Document, root: usize, model: Option<usize>) -> Result<Self> {
        let mut result = Self {
            nodes: Vec::new(),
            transforms: Vec::new(),
            by_transform: HashMap::new(),
            by_dag: HashMap::new(),
            joints: Vec::new(),
            bind: Vec::new(),
            default_bone: 0,
        };
        let mut visited = HashSet::new();
        for child in elements(node(document, root)?, "children")? {
            result.dag(document, child, -1, &mut visited, 0)?;
        }
        if let Some(model) = model {
            let model = node(document, model)?;
            let mut joint_transforms = required_elements(model, "jointTransforms")?;
            if joint_transforms.is_empty() {
                joint_transforms = required_elements(model, "jointList")?
                    .iter()
                    .map(|&id| {
                        element(node(document, id)?, "transform")?
                            .ok_or_else(|| "DMX joint has no transform".into())
                    })
                    .collect::<Result<Vec<_>>>()?;
            }
            for &id in &joint_transforms {
                let bone = if let Some(&bone) = result.by_transform.get(&id) {
                    bone
                } else {
                    result.push(document, id, -1)?
                };
                result.joints.push(bone);
            }
            let bind_state = elements(model, "baseStates")?.into_iter().find(|&id| {
                document
                    .nodes
                    .get(id)
                    .is_some_and(|n| n.name.eq_ignore_ascii_case("bind"))
            });
            let bind_transforms = if let Some(id) = bind_state {
                required_elements(node(document, id)?, "transforms")?
            } else {
                joint_transforms
            };
            if bind_transforms.len() != result.joints.len() {
                return Err("DMX bind transform count differs from joint count".into());
            }
            for (i, id) in bind_transforms.into_iter().enumerate() {
                result.bind[result.joints[i]] = transform(document, id)?;
            }
        }
        result.default_bone = result.nodes.len();
        result.nodes.push(SmdNode {
            index: result.nodes.len() as i32,
            name: "defaultRoot".into(),
            parent: -1,
        });
        result.transforms.push(None);
        result.bind.push(bone_math::IDENTITY);
        Ok(result)
    }

    fn push(&mut self, document: &Document, id: usize, parent: i32) -> Result<usize> {
        let name = node(document, id)?.name.clone();
        if name.is_empty()
            || self
                .nodes
                .iter()
                .any(|n| n.name.eq_ignore_ascii_case(&name))
        {
            return Err(format!("DMX has an empty or duplicate bone name {name:?}"));
        }
        if self.nodes.len() >= 4096 {
            return Err("DMX exceeds 4096 source bones".into());
        }
        let bone = self.nodes.len();
        self.nodes.push(SmdNode {
            index: bone as i32,
            name,
            parent,
        });
        self.transforms.push(Some(id));
        self.bind.push(bone_math::IDENTITY);
        self.by_transform.insert(id, bone);
        Ok(bone)
    }

    fn dag(
        &mut self,
        document: &Document,
        id: usize,
        parent: i32,
        visited: &mut HashSet<usize>,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 || !visited.insert(id) {
            return Err(
                "DMX skeleton contains a cycle, shared DAG node, or excessive depth".into(),
            );
        }
        let n = node(document, id)?;
        let transform = element(n, "transform")?
            .ok_or_else(|| format!("DMX DAG {} has no transform", n.name))?;
        if self.by_transform.contains_key(&transform) {
            return Err("DMX skeleton shares a bone transform".into());
        }
        let bone = self.push(document, transform, parent)?;
        self.by_dag.insert(id, bone);
        for child in elements(n, "children")? {
            self.dag(document, child, bone as i32, visited, depth + 1)?;
        }
        Ok(())
    }

    fn poses(&self, matrices: &[Matrix3x4], opts: &FbxOpts) -> Vec<SmdPose> {
        let axis = if opts.axis == Some(ForcedAxis::Y) {
            bone_math::angle_matrix([std::f32::consts::FRAC_PI_2, 0., 0.])
        } else {
            bone_math::IDENTITY
        };
        matrices
            .iter()
            .enumerate()
            .map(|(bone, matrix)| {
                let matrix = if self.nodes[bone].parent < 0 {
                    bone_math::concat(&axis, matrix)
                } else {
                    *matrix
                };
                SmdPose {
                    bone: bone as i32,
                    position: [matrix[3], matrix[7], matrix[11]].map(|v| v * opts.scale),
                    rotation: bone_math::matrix_angles(&matrix),
                }
            })
            .collect()
    }
}

fn load(path: &Path) -> Result<Document> {
    let length = std::fs::metadata(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?
        .len();
    if length > 128 * 1024 * 1024 {
        return Err("DMX file exceeds 128 MiB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let document = data::parse(&bytes)?;
    if document.format != "model" || document.version > 21 {
        return Err(format!(
            "DMX {} version {} is not a Source1 model",
            document.format, document.version
        ));
    }
    Ok(document)
}

pub fn read(path: &Path, opts: &FbxOpts) -> Result<Geometry> {
    from_document(&load(path)?, opts)
}

fn from_document(document: &Document, opts: &FbxOpts) -> Result<Geometry> {
    if !opts.scale.is_finite() || opts.scale <= 0. {
        return Err("DMX import scale must be positive and finite".into());
    }
    let root = node(document, document.root)?;
    let model = element(root, "model")?;
    let skeleton_root = element(root, "skeleton")?
        .or(model)
        .ok_or("DMX has no skeleton or model")?;
    let skeleton = Skeleton::load(document, skeleton_root, model)?;
    let mut geometry = Geometry {
        smd: Smd {
            version: 1,
            nodes: skeleton.nodes.clone(),
            frames: vec![SmdFrame {
                time: 0,
                poses: skeleton.poses(&skeleton.bind, opts),
            }],
            triangles: Vec::new(),
        },
        metadata: Metadata::default(),
        animation_names: animation::names(document)?,
    };
    flex::read_controls(document, &mut geometry.metadata)?;
    let mut next_vertex = 0u32;
    let mut matched = HashSet::new();
    if let Some(model) = model {
        let mut visited = HashSet::new();
        for child in elements(node(document, model)?, "children")? {
            load_meshes(
                document,
                child,
                &skeleton,
                &bone_math::IDENTITY,
                skeleton.default_bone,
                opts,
                &mut geometry,
                &mut next_vertex,
                &mut matched,
                &mut visited,
                0,
            )?;
        }
    }
    for part in &opts.parts {
        if !matched.contains(part) {
            return Err(format!("DMX mesh {part:?} was not found"));
        }
    }
    let finite = geometry
        .smd
        .frames
        .iter()
        .flat_map(|frame| &frame.poses)
        .all(|pose| {
            pose.position
                .iter()
                .chain(&pose.rotation)
                .all(|v| v.is_finite())
        })
        && geometry
            .smd
            .triangles
            .iter()
            .flat_map(|triangle| &triangle.vertices)
            .all(|v| {
                v.position
                    .iter()
                    .chain(&v.normal)
                    .chain(&v.uv)
                    .all(|x| x.is_finite())
            })
        && geometry
            .metadata
            .shapes
            .iter()
            .flat_map(|shape| &shape.vertices)
            .all(|delta| {
                delta
                    .position
                    .iter()
                    .chain(&delta.normal)
                    .chain([&delta.wrinkle, &delta.speed, &delta.balance])
                    .all(|v| v.is_finite())
            });
    if !finite {
        return Err("DMX model transforms produce non-finite geometry or morph data".into());
    }
    Ok(geometry)
}

#[allow(clippy::too_many_arguments)]
fn load_meshes(
    document: &Document,
    dag: usize,
    skeleton: &Skeleton,
    parent: &Matrix3x4,
    mut assigned: usize,
    opts: &FbxOpts,
    geometry: &mut Geometry,
    next_vertex: &mut u32,
    matched: &mut HashSet<String>,
    visited: &mut HashSet<usize>,
    depth: usize,
) -> Result<()> {
    if depth > 128 || !visited.insert(dag) {
        return Err("DMX geometry contains a cycle or shared DAG node".into());
    }
    let n = node(document, dag)?;
    let transform_id =
        element(n, "transform")?.ok_or_else(|| format!("DMX DAG {} has no transform", n.name))?;
    let bone = skeleton.by_transform.get(&transform_id).copied();
    let local = bone.filter(|b| skeleton.joints.contains(b)).map_or_else(
        || transform(document, transform_id),
        |bone| Ok(skeleton.bind[bone]),
    )?;
    let world = bone_math::concat(parent, &local);
    if let Some(bone) = bone
        && (n.class == "DmeJoint" || depth == 0)
    {
        assigned = bone;
    }
    if let Some(shape) = element(n, "shape")? {
        let shape_node = node(document, shape)?;
        match shape_node.class.as_str() {
            "DmeMesh" => {
                let selected = opts.parts.is_empty()
                    || opts
                        .parts
                        .iter()
                        .any(|p| p == &n.name || p == &shape_node.name);
                if selected {
                    matched.insert(n.name.clone());
                    matched.insert(shape_node.name.clone());
                    mesh(
                        document,
                        shape,
                        &world,
                        assigned,
                        skeleton,
                        opts,
                        geometry,
                        next_vertex,
                    )?;
                }
            }
            "DmeAttachment" => {
                let bone = skeleton
                    .by_dag
                    .get(&dag)
                    .ok_or("DMX attachment has no bone")?;
                geometry
                    .metadata
                    .attachments
                    .push(crate::model::Attachment {
                        name: shape_node.name.clone(),
                        bone: skeleton.nodes[*bone].name.clone(),
                        position: Some([0.; 3]),
                        rotation: Some([0.; 3]),
                        absolute: false,
                        absolute_rotation: None,
                        rigid: boolean(shape_node, "isRigid", false)?,
                        flags: Some(if boolean(shape_node, "isWorldAligned", false)? {
                            0x10000
                        } else {
                            0
                        }),
                        synthetic: false,
                        resolved: None,
                    });
            }
            _ => {
                return Err(format!(
                    "DMX shape type {} is not supported by the Source1 model loader",
                    shape_node.class
                ));
            }
        }
    }
    for child in elements(n, "children")? {
        load_meshes(
            document,
            child,
            skeleton,
            &world,
            assigned,
            opts,
            geometry,
            next_vertex,
            matched,
            visited,
            depth + 1,
        )?;
    }
    Ok(())
}

fn indexed(indices: &[i32], corner: usize, count: usize, name: &str) -> Result<usize> {
    let i = if indices.is_empty() {
        corner
    } else {
        usize::try_from(
            *indices
                .get(corner)
                .ok_or_else(|| format!("DMX {name} index count is too short"))?,
        )
        .map_err(|_| format!("DMX {name} contains a negative index"))?
    };
    if i >= count {
        return Err(format!("DMX {name} index {i} exceeds {count} values"));
    }
    Ok(i)
}

#[allow(clippy::too_many_arguments)]
fn mesh(
    document: &Document,
    mesh_id: usize,
    world: &Matrix3x4,
    assigned: usize,
    skeleton: &Skeleton,
    opts: &FbxOpts,
    geometry: &mut Geometry,
    next_vertex: &mut u32,
) -> Result<()> {
    let mesh = node(document, mesh_id)?;
    let bind_id = match element(mesh, "bindState")? {
        Some(id) => Some(id),
        None => elements(mesh, "baseStates")?.into_iter().find(|&i| {
            document
                .nodes
                .get(i)
                .is_some_and(|n| n.name.eq_ignore_ascii_case("bind"))
        }),
    }
    .ok_or_else(|| format!("DMX mesh {} has no bind state", mesh.name))?;
    let bind = node(document, bind_id)?;
    let positions = vectors(bind, "positions")?;
    let normals = vectors(bind, "normals")?;
    let uvs = match value(bind, "textureCoordinates") {
        Some(Value::Vector2Array(v)) if v.iter().flatten().all(|v| v.is_finite()) => v.as_slice(),
        None => &[],
        _ => return Err("DMX textureCoordinates must be a finite vector2 array".into()),
    };
    let pindices = ints(bind, "positionsIndices")?;
    let nindices = ints(bind, "normalsIndices")?;
    let tindices = ints(bind, "textureCoordinatesIndices")?;
    if positions.is_empty() || normals.is_empty() {
        return Err(format!(
            "DMX mesh {} has no positions or normals",
            mesh.name
        ));
    }
    let skin_count = usize::try_from(integer(bind, "jointCount", 0)?)
        .map_err(|_| "DMX has negative jointCount")?;
    if skin_count > 3 {
        return Err("DMX jointCount exceeds Source1's three skin influences per vertex".into());
    }
    let weights = floats(bind, "jointWeights")?;
    let joint_indices = ints(bind, "jointIndices")?;
    if skin_count > 0
        && (weights.len() != positions.len() * skin_count || joint_indices.len() != weights.len())
    {
        return Err("DMX skinning array sizes do not match positions and jointCount".into());
    }
    let mut matrix = *world;
    if opts.axis == Some(ForcedAxis::Y) {
        matrix = bone_math::concat(
            &bone_math::angle_matrix([std::f32::consts::FRAC_PI_2, 0., 0.]),
            &matrix,
        );
    }
    let flip = boolean(bind, "flipVCoordinates", false)?;
    let mut source_ids = HashMap::<(usize, usize, usize, usize, usize, usize), u32>::new();
    let mut source_keys = Vec::new();
    let balance = floats(bind, "balance")?;
    let speeds = floats(bind, "speed")?;
    let balance_indices = ints(bind, "balanceIndices")?;
    let speed_indices = ints(bind, "speedIndices")?;
    let base_wrinkles = floats(bind, "wrinkle")?;
    let wrinkle_indices = ints(bind, "wrinkleIndices")?;
    for face in elements(mesh, "faceSets")? {
        let face = node(document, face)?;
        let material = if let Some(id) = element(face, "material")? {
            let mat = node(document, id)?;
            string(mat, "mtlName", &mat.name)?
        } else {
            opts.material
                .clone()
                .unwrap_or_else(|| "debug/debugempty".into())
        };
        if material
            .trim_end_matches(".vmt")
            .eq_ignore_ascii_case("null")
        {
            continue;
        }
        let faces = ints(face, "faces")?;
        for polygon in faces.split(|&i| i == -1).filter(|p| !p.is_empty()) {
            if polygon.len() < 3 {
                return Err(format!(
                    "DMX face in {} has fewer than 3 vertices",
                    mesh.name
                ));
            }
            if polygon.len() > 4096 {
                return Err("DMX polygon exceeds 4096 corners".into());
            }
            let corners = polygon
                .iter()
                .map(|&i| {
                    usize::try_from(i).map_err(|_| "DMX face has a negative corner index".into())
                })
                .collect::<Result<Vec<_>>>()?;
            let mut vertices = Vec::with_capacity(corners.len());
            for &corner in &corners {
                let p = indexed(pindices, corner, positions.len(), "position")?;
                let n = indexed(nindices, corner, normals.len(), "normal")?;
                let t = if uvs.is_empty() {
                    0
                } else {
                    indexed(tindices, corner, uvs.len(), "texture coordinate")?
                };
                let b = if balance.is_empty() {
                    0
                } else {
                    indexed(balance_indices, corner, balance.len(), "balance")?
                };
                let s = if speeds.is_empty() {
                    0
                } else {
                    indexed(speed_indices, corner, speeds.len(), "speed")?
                };
                let w = if wrinkle_indices.is_empty() {
                    t
                } else {
                    indexed(wrinkle_indices, corner, base_wrinkles.len(), "wrinkle")?
                };
                let id = *source_ids.entry((p, n, t, b, s, w)).or_insert_with(|| {
                    let id = *next_vertex;
                    *next_vertex += 1;
                    source_keys.push((id, p, n, t, b, s, w));
                    id
                });
                let mut links = Vec::new();
                if skin_count == 0 {
                    links.push(SmdBoneLink {
                        bone: assigned as i32,
                        weight: 1.,
                    });
                } else {
                    for i in p * skin_count..(p + 1) * skin_count {
                        let weight = weights[i];
                        if weight < 0. {
                            return Err("DMX skin weight is negative".into());
                        }
                        if weight == 0. {
                            continue;
                        }
                        let joint = usize::try_from(joint_indices[i])
                            .map_err(|_| "DMX skin joint is negative")?;
                        let bone = *skeleton.joints.get(joint).ok_or_else(|| {
                            format!("DMX skin joint {joint} is outside the joint list")
                        })?;
                        if let Some(link) = links.iter_mut().find(|l| l.bone == bone as i32) {
                            link.weight += weight;
                        } else {
                            links.push(SmdBoneLink {
                                bone: bone as i32,
                                weight,
                            });
                        }
                    }
                    links.sort_by(|a, b| b.weight.total_cmp(&a.weight));
                    while links.len() > 1 && links.last().is_some_and(|link| link.weight < 0.05) {
                        links.pop();
                    }
                    if links.is_empty() {
                        let joint = usize::try_from(joint_indices[p * skin_count])
                            .map_err(|_| "DMX skin joint is negative")?;
                        let bone = *skeleton
                            .joints
                            .get(joint)
                            .ok_or("DMX skin joint is outside the joint list")?;
                        links.push(SmdBoneLink {
                            bone: bone as i32,
                            weight: 1.,
                        });
                    }
                    let sum: f32 = links.iter().map(|l| l.weight).sum();
                    if !sum.is_finite() {
                        return Err("DMX skin weight sum overflows".into());
                    }
                    for link in &mut links {
                        link.weight /= sum;
                    }
                }
                let uv = uvs.get(t).copied().unwrap_or([0.; 2]);
                vertices.push(SmdVertex {
                    parent_bone: assigned as i32,
                    position: vector(&matrix, positions[p], true).map(|v| v * opts.scale),
                    normal: normal(vector(&matrix, normals[n], false))?,
                    uv: [uv[0], if flip { 1. - uv[1] } else { uv[1] }],
                    links,
                    src_index: id,
                });
            }
            if geometry
                .smd
                .triangles
                .len()
                .saturating_add(vertices.len() - 2)
                > 1_000_000
            {
                return Err("DMX triangulated geometry exceeds one million triangles".into());
            }
            for triangle in triangulate(&vertices)? {
                geometry.smd.triangles.push(SmdTriangle {
                    material: material.clone(),
                    vertices: triangle.map(|i| vertices[i].clone()),
                });
            }
        }
    }
    for delta_id in elements(mesh, "deltaStates")? {
        let delta = node(document, delta_id)?;
        let pd = sparse_vectors(delta, "positions", positions.len())?;
        let nd = sparse_vectors(delta, "normals", normals.len())?;
        let wrinkle = floats(delta, "wrinkle")?;
        let wi = ints(delta, "wrinkleIndices")?;
        if wrinkle.len() != wi.len() {
            return Err("DMX wrinkle deltas and indices have different lengths".into());
        }
        let mut wd = HashMap::new();
        for (i, &v) in wi.iter().zip(wrinkle) {
            let i = usize::try_from(*i).map_err(|_| "DMX wrinkle delta index is negative")?;
            if i >= uvs.len() {
                return Err("DMX wrinkle delta index is out of range".into());
            }
            if wd.insert(i, v).is_some() {
                return Err("DMX duplicate wrinkle delta index".into());
            }
        }
        let wrinkle_scale = geometry
            .metadata
            .controls
            .iter()
            .find_map(|control| {
                control
                    .raw
                    .iter()
                    .position(|name| name == &delta.name)
                    .map(|i| control.wrinkle_scales.get(i).copied().unwrap_or(0.))
            })
            .unwrap_or(0.);
        if value(delta, "wrinkle").is_none() && wrinkle_scale != 0. && !uvs.is_empty() {
            let max = pd
                .values()
                .map(|v| v.iter().map(|x| x * x).sum::<f32>().sqrt())
                .fold(0., f32::max);
            if max > 0. {
                for &(_id, p, _n, t, _b, _s, _w) in &source_keys {
                    if let Some(delta) = pd.get(&p) {
                        wd.entry(t).or_insert_with(|| {
                            delta.iter().map(|x| x * x).sum::<f32>().sqrt() / max * wrinkle_scale
                        });
                    }
                }
            }
        }
        let mut shape = Shape {
            name: delta.name.clone(),
            vertices: Vec::new(),
            wrinkle: value(delta, "wrinkle").is_some() || !wd.is_empty(),
        };
        for &(id, p, n, _t, b, s, corner_wrinkle) in &source_keys {
            if !pd.contains_key(&p) && !nd.contains_key(&n) && !wd.contains_key(&corner_wrinkle) {
                continue;
            }
            shape.vertices.push(Delta {
                vertex: id,
                position: vector(&matrix, pd.get(&p).copied().unwrap_or([0.; 3]), false)
                    .map(|v| v * opts.scale),
                normal: vector(&matrix, nd.get(&n).copied().unwrap_or([0.; 3]), false),
                wrinkle: wd.get(&corner_wrinkle).copied().unwrap_or(0.),
                balance: balance.get(b).copied().unwrap_or(1.),
                speed: speeds.get(s).copied().unwrap_or(1.),
            });
        }
        if let Some(existing) = geometry
            .metadata
            .shapes
            .iter_mut()
            .find(|s| s.name == shape.name)
        {
            existing.wrinkle |= shape.wrinkle;
            existing.vertices.extend(shape.vertices);
        } else {
            geometry.metadata.shapes.push(shape);
        }
    }
    Ok(())
}

fn sparse_vectors(node: &Node, key: &str, base_count: usize) -> Result<HashMap<usize, [f32; 3]>> {
    let values = vectors(node, key)?;
    let indices = ints(node, &format!("{key}Indices"))?;
    if values.len() != indices.len() {
        return Err(format!(
            "DMX delta {} has mismatched {key} values and indices",
            node.name
        ));
    }
    let mut result = HashMap::new();
    for (&i, &v) in indices.iter().zip(values) {
        let i = usize::try_from(i).map_err(|_| format!("DMX {key} delta has a negative index"))?;
        if i >= base_count || result.insert(i, v).is_some() {
            return Err(format!(
                "DMX {key} delta contains a duplicate or out-of-range index"
            ));
        }
    }
    Ok(result)
}

fn triangulate(vertices: &[SmdVertex]) -> Result<Vec<[usize; 3]>> {
    let mut axis = [0.; 3];
    for i in 0..vertices.len() {
        let a = vertices[i].position;
        let b = vertices[(i + 1) % vertices.len()].position;
        axis[0] += (a[1] - b[1]) * (a[2] + b[2]);
        axis[1] += (a[2] - b[2]) * (a[0] + b[0]);
        axis[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let drop = (0..3)
        .max_by(|&a, &b| axis[a].abs().total_cmp(&axis[b].abs()))
        .unwrap();
    let points: Vec<[f32; 2]> = vertices
        .iter()
        .map(|v| [v.position[(drop + 1) % 3], v.position[(drop + 2) % 3]])
        .collect();
    let cross = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let area: f32 = (0..points.len())
        .map(|i| {
            points[i][0] * points[(i + 1) % points.len()][1]
                - points[(i + 1) % points.len()][0] * points[i][1]
        })
        .sum();
    if area.abs() < 1e-10 {
        return Err("DMX polygon has zero area".into());
    }
    let sign = area.signum();
    let mut pending: Vec<usize> = (0..vertices.len()).collect();
    let mut output = Vec::new();
    while pending.len() > 3 {
        let mut ear = None;
        for i in 0..pending.len() {
            let a = pending[(i + pending.len() - 1) % pending.len()];
            let b = pending[i];
            let c = pending[(i + 1) % pending.len()];
            if cross(points[a], points[b], points[c]) * sign <= 1e-10 {
                continue;
            }
            if pending
                .iter()
                .copied()
                .filter(|&p| p != a && p != b && p != c)
                .any(|p| {
                    cross(points[a], points[b], points[p]) * sign >= 0.
                        && cross(points[b], points[c], points[p]) * sign >= 0.
                        && cross(points[c], points[a], points[p]) * sign >= 0.
                })
            {
                continue;
            }
            ear = Some((i, [a, b, c]));
            break;
        }
        let (i, triangle) =
            ear.ok_or("DMX polygon is self-intersecting or cannot be triangulated")?;
        output.push(triangle);
        pending.remove(i);
    }
    output.push([pending[0], pending[1], pending[2]]);
    Ok(output)
}

pub fn read_frames(
    path: &Path,
    stack: Option<&str>,
    fps: Option<f32>,
    opts: &FbxOpts,
) -> Result<Smd> {
    let document = load(path)?;
    let root = node(&document, document.root)?;
    let model = element(root, "model")?;
    let skeleton_root = element(root, "skeleton")?
        .or(model)
        .ok_or("DMX has no skeleton or model")?;
    let skeleton = Skeleton::load(&document, skeleton_root, model)?;
    animation::read(&document, skeleton, stack, fps, opts)
}

pub(crate) use flex::{
    register as register_flexes, remap as remap_flexes, resolve as resolve_flexes,
};

pub(crate) fn merge_metadata(base: &mut Metadata, appended: Metadata) -> Result<()> {
    for control in appended.controls {
        if let Some(existing) = base.controls.iter().find(|c| c.name == control.name) {
            if existing != &control {
                return Err(format!(
                    "Appended DMX control {} has a conflicting definition",
                    control.name
                ));
            }
        } else {
            base.controls.push(control);
        }
    }
    for shape in appended.shapes {
        if let Some(existing) = base.shapes.iter_mut().find(|s| s.name == shape.name) {
            existing.wrinkle |= shape.wrinkle;
            existing.vertices.extend(shape.vertices);
        } else {
            base.shapes.push(shape);
        }
    }
    base.dominators.extend(appended.dominators);
    base.attachments.extend(appended.attachments);
    base.active_shapes.extend(appended.active_shapes);
    base.has_combination |= appended.has_combination;
    if let Some(mut rules) = appended.explicit_rules {
        if let Some(existing) = &mut base.explicit_rules {
            existing.controls.append(&mut rules.controls);
            existing.rules.append(&mut rules.rules);
        } else {
            base.explicit_rules = Some(rules);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

pub fn animation_names(path: &Path) -> Result<Vec<String>> {
    animation::names(&load(path)?)
}

pub fn mesh_names(path: &Path) -> Result<Vec<String>> {
    let document = load(path)?;
    let root = node(&document, document.root)?;
    let Some(model) = element(root, "model")? else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    let mut pending = elements(node(&document, model)?, "children")?;
    pending.reverse();
    let mut visited = HashSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            return Err("DMX geometry contains a cycle or shared DAG node".into());
        }
        let dag = node(&document, id)?;
        if let Some(shape) = element(dag, "shape")? {
            let shape = node(&document, shape)?;
            if shape.class == "DmeMesh" && !names.contains(&dag.name) {
                names.push(dag.name.clone());
            }
        }
        pending.extend(elements(dag, "children")?.into_iter().rev());
    }
    Ok(names)
}
