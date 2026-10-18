use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use mdlc::model::{CompiledModelDesc, ModelDesc};
use mdlc::phy::PhyHull;
use mdlc::pipeline;

use super::{ModelCompileError, ModelCompileRequest, check_cancel};

const MAX_FACES: usize = 1_000_000;
const MAX_PARTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollisionDetail {
    Simple,
    Balanced,
    Detailed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollisionPhysics {
    pub mass: f32,
    pub surface_property: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelCollision {
    Existing,
    None,
    Automatic(CollisionDetail),
    Custom {
        source: PathBuf,
        parts: Vec<String>,
        repair: Option<CollisionDetail>,
    },
}

pub(super) fn apply(
    output: &mut pipeline::PipelineOutput,
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<(), ModelCompileError> {
    match request.collision {
        ModelCollision::Existing => return Ok(()),
        ModelCollision::None => {
            output.phy = None;
            return Ok(());
        }
        _ => (),
    }
    let hulls = collision_hulls(&output.compiled, request, cancel)?;
    let name = Path::new(&output.stem)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("model");
    let mut params = mdlc::phy::PhyParams::new(name, output.checksum as u32);
    params.total_mass = output.compiled.desc.physics.effective_mass();
    params.concave = hulls.len() > 1;
    params.surface_prop = output
        .compiled
        .desc
        .model
        .surface_prop
        .as_deref()
        .unwrap_or("default");
    let bytes = mdlc::phy::write_phy_multi(&[hulls], &[mdlc::phy::PhySolid::prop(name)], &params)
        .map_err(pipeline::PipelineError::BuildPhy)?;
    mdlc::phy::check_invariants(&bytes).map_err(pipeline::PipelineError::CheckPhy)?;
    check_cancel(cancel)?;
    output.phy = Some(bytes);
    Ok(())
}

pub(crate) fn collision_hulls(
    compiled: &CompiledModelDesc,
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<Vec<PhyHull>, ModelCompileError> {
    check_cancel(cancel)?;
    if compiled.desc.physics.joints
        && matches!(
            request.collision,
            ModelCollision::Automatic(_) | ModelCollision::Custom { .. }
        )
    {
        return Err(invalid(
            "keep authored collision joints when compiling a ragdoll",
        ));
    }
    if let ModelCollision::Custom {
        source,
        parts,
        repair,
    } = &request.collision
    {
        return custom_hulls(source, parts, *repair, request, cancel);
    }
    if request.collision == ModelCollision::None {
        return Ok(Vec::new());
    }
    let (vertices, faces) = mesh_geometry(compiled, cancel)?;
    if vertices.len() < 4 || faces.is_empty() {
        return Err(invalid(
            "collision requires a non-flat mesh with at least four vertices; select visible default bodygroup choices or provide a custom collision mesh",
        ));
    }
    match request.collision {
        ModelCollision::Automatic(detail) => automatic_hulls(&vertices, &faces, detail, cancel),
        ModelCollision::Custom { .. } => unreachable!(),
        ModelCollision::None => Ok(Vec::new()),
        ModelCollision::Existing => Err(invalid(
            "existing collision is retained from the authored model description",
        )),
    }
}

fn automatic_hulls(
    vertices: &[[f32; 3]],
    faces: &[[u32; 3]],
    detail: CollisionDetail,
    cancel: &AtomicBool,
) -> Result<Vec<PhyHull>, ModelCompileError> {
    check_cancel(cancel)?;
    if detail == CollisionDetail::Simple {
        return Ok(vec![hull(vertices)?]);
    }
    let (resolution, limit): (u32, u32) = match detail {
        CollisionDetail::Balanced => (32, 16),
        CollisionDetail::Detailed => (64, 32),
        CollisionDetail::Simple => unreachable!(),
    };
    let mut components = connected_components(vertices, faces, cancel)?;
    let before = components.len();
    components.retain(|(points, triangles)| has_volume(points, triangles));
    if components.len() != before {
        mdlc::warning!(
            "Automatic collision omitted {} flat decorative mesh pieces",
            before - components.len()
        );
    }
    if components.is_empty() {
        return Err(invalid(
            "automatic collision requires a mesh with volume; provide a closed custom collision mesh",
        ));
    }
    if components.len() > limit as usize {
        let mut points = Vec::new();
        let mut triangles = Vec::new();
        for (part_points, part_faces) in components {
            let offset = points.len() as u32;
            points.extend(part_points);
            triangles.extend(
                part_faces
                    .into_iter()
                    .map(|face| face.map(|index| index + offset)),
            );
        }
        return decompose(&points, &triangles, resolution, limit, cancel);
    }
    let count = components.len();
    let mut result = Vec::new();
    for (index, (points, triangles)) in components.into_iter().enumerate() {
        check_cancel(cancel)?;
        if let Ok(convex) = validate_convex(&points, &triangles) {
            result.push(convex);
        } else {
            let reserved = (count - index - 1) as u32;
            let remaining = limit.saturating_sub(result.len() as u32 + reserved);
            if remaining == 0 {
                return Err(invalid(
                    "automatic collision exceeds the selected piece limit; increase detail or provide a custom collision mesh",
                ));
            }
            result.extend(decompose(
                &points, &triangles, resolution, remaining, cancel,
            )?);
        }
        if result.len() > limit as usize {
            return Err(invalid(
                "automatic collision exceeds the selected piece limit; increase detail or provide a custom collision mesh",
            ));
        }
    }
    check_cancel(cancel)?;
    Ok(result)
}

fn custom_hulls(
    source: &Path,
    selected: &[String],
    repair: Option<CollisionDetail>,
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<Vec<PhyHull>, ModelCompileError> {
    let available = super::bodygroups::source_parts(source)?;
    let mut unique = HashSet::new();
    if selected.iter().any(|part| !unique.insert(part)) {
        return Err(invalid("custom collision meshes are repeated"));
    }
    let selectors: Vec<_> = if selected.is_empty() {
        available.into_iter().map(|part| part.selector).collect()
    } else {
        selected.to_vec()
    };
    let selections = if selectors.is_empty() {
        vec![Vec::new()]
    } else {
        selectors.into_iter().map(|part| vec![part]).collect()
    };
    if selections.len() > MAX_PARTS {
        return Err(invalid(
            "custom collision must contain at most 64 convex pieces",
        ));
    }
    let mut hulls = Vec::new();
    for parts in selections {
        check_cancel(cancel)?;
        let custom = import_custom(source, &parts, request, cancel)?;
        if let Some(detail) = repair {
            let (vertices, faces) = mesh_geometry(&custom, cancel)?;
            hulls.extend(automatic_hulls(&vertices, &faces, detail, cancel)?);
            if hulls.len() > MAX_PARTS {
                return Err(invalid(
                    "repaired collision must contain at most 64 convex pieces; reduce detail",
                ));
            }
            continue;
        }
        // SMD smoothing boundaries identify pieces only when they form complete convex shells.
        let (normal_vertices, normal_faces) = mesh_geometry_with_normals(&custom, cancel, true)?;
        let normal_components = connected_components(&normal_vertices, &normal_faces, cancel)?;
        let separate = normal_components
            .iter()
            .map(|(points, triangles)| validate_convex(points, triangles))
            .collect::<Result<Vec<_>, _>>();
        if let Ok(pieces) = separate
            && !pieces.is_empty()
        {
            if hulls.len() + pieces.len() > MAX_PARTS {
                return Err(invalid(
                    "custom collision must contain at most 64 convex pieces",
                ));
            }
            hulls.extend(pieces);
            continue;
        }
        let (vertices, faces) = mesh_geometry(&custom, cancel)?;
        let components = connected_components(&vertices, &faces, cancel)?;
        if components.is_empty() {
            return Err(invalid(
                "custom collision requires a closed mesh with volume",
            ));
        }
        for (points, triangles) in components {
            let index = hulls.len() + 1;
            if index > MAX_PARTS {
                return Err(invalid(
                    "custom collision must contain at most 64 convex pieces",
                ));
            }
            hulls.push(validate_convex(&points, &triangles).map_err(|message| invalid(&format!("custom collision piece {index} {message}; use closed, separate convex pieces, or enable custom mesh repair")))?);
        }
    }
    Ok(hulls)
}

pub fn collision_parts(source: &Path) -> Result<Vec<String>, ModelCompileError> {
    if source.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb")
    }) {
        mdlc::gltf::collision_parts(source, "collision parts")
            .map_err(|error| ModelCompileError::Description(error.to_string()))
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn collision_physics(
    source: &Path,
) -> Result<Option<CollisionPhysics>, ModelCompileError> {
    if !source.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb")
    }) {
        return Ok(None);
    }
    let metadata = mdlc::gltf::source_metadata(source, "collision physics")
        .map_err(|error| ModelCompileError::Description(error.to_string()))?;
    Ok(metadata
        .iter()
        .find_map(|text| parse_collision_physics(text)))
}

fn parse_collision_physics(text: &str) -> Option<CollisionPhysics> {
    if text.len() > 8 * 1024 * 1024 {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_str(text).ok()?;
    let physics = metadata.get("sourcePhysics").unwrap_or(&metadata);
    let keyvalues = physics.get("sourcePhysicsKeyvalues")?.as_str()?;
    let limits = vformats::Limits {
        max_input_bytes: 1024 * 1024,
        max_entries: 8192,
        max_kv_depth: 16,
        ..vformats::Limits::default()
    };
    let document = vformats::keyvalues::parse(keyvalues, &limits).ok()?;
    let mass: f32 = document
        .blocks("editparams")
        .next()?
        .get_str("totalmass")?
        .parse()
        .ok()?;
    if !mass.is_finite() || mass <= 0.0 {
        return None;
    }
    let surface_property = document
        .blocks("solid")
        .next()
        .and_then(|solid| solid.get_str("surfaceprop"))
        .filter(|property| {
            !property.is_empty()
                && property
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map(str::to_owned);
    Some(CollisionPhysics {
        mass,
        surface_property,
    })
}

fn invalid(message: &str) -> ModelCompileError {
    ModelCompileError::InvalidOption(message.into())
}

fn import_custom(
    source: &Path,
    parts: &[String],
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<CompiledModelDesc, ModelCompileError> {
    let source = std::fs::canonicalize(source).map_err(|error| ModelCompileError::Io {
        path: source.into(),
        source: error,
    })?;
    let base = source
        .parent()
        .ok_or_else(|| invalid("collision source must be a mesh file"))?;
    if !source.extension().is_some_and(|extension| {
        ["smd", "dmx", "fbx", "gltf", "glb"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
    }) {
        return Err(invalid(
            "custom collision requires SMD, DMX, FBX, glTF or GLB",
        ));
    }
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("collision filename must be UTF-8"))?;
    if filename.contains(['"', '\n', '\r']) {
        return Err(invalid(
            "collision filename cannot contain quotes or newlines",
        ));
    }
    let axes = if request.automatic_axes
        && !source
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("smd"))
    {
        " srcaxis auto"
    } else {
        ""
    };
    let text = format!(
        "$modelname collision.mdl\n$staticprop\n$scale {}\n$body collision \"{filename}\"{axes}\n$sequence idle \"{filename}\"\n",
        request.scale
    );
    let mut description = mdlc::qc::parse_qc_str(&text, base).map_err(|errors| {
        ModelCompileError::Description(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    if !parts.is_empty() {
        let available = super::bodygroups::source_parts(&source)?;
        let mut seen = HashSet::new();
        if parts
            .iter()
            .any(|part| !seen.insert(part) || !available.iter().any(|item| item.selector == *part))
        {
            return Err(invalid(
                "custom collision meshes are missing or repeated; import the source again",
            ));
        }
        description.bodyparts[0].models[0].src_parts = parts.to_vec();
    }
    check_cancel(cancel)?;
    let mut compiled =
        mdlc::compile::compile(&description, base).map_err(pipeline::PipelineError::Compile)?;
    super::apply_orientation(&mut compiled, &source, request.orientation)?;
    check_cancel(cancel)?;
    Ok(compiled)
}

pub(super) fn exclude_render_parts(
    description: &mut ModelDesc,
    request: &ModelCompileRequest,
    source: &Path,
) -> Result<(), ModelCompileError> {
    let available = super::bodygroups::source_parts(source)?;
    let mut excluded = collision_parts(source)?;
    if let ModelCollision::Custom {
        source: collision_source,
        parts,
        ..
    } = &request.collision
        && !parts.is_empty()
        && std::fs::canonicalize(collision_source).ok().as_deref() == Some(source)
    {
        let selected: HashSet<_> = parts.iter().collect();
        if selected.len() != parts.len()
            || parts
                .iter()
                .any(|part| !available.iter().any(|item| item.selector == *part))
        {
            return Err(invalid(
                "custom collision meshes are missing or repeated; import the source again",
            ));
        }
        excluded.extend(parts.iter().cloned());
    }
    if excluded.is_empty() {
        return Ok(());
    }
    let selected: HashSet<_> = excluded.iter().map(String::as_str).collect();
    for body in &mut description.bodyparts {
        for model in &mut body.models {
            if model.smd.is_empty() {
                continue;
            }
            if model.src_parts.is_empty() {
                model.src_parts = available
                    .iter()
                    .filter(|part| !selected.contains(part.selector.as_str()))
                    .map(|part| part.selector.clone())
                    .collect();
                if model.src_parts.is_empty() {
                    return Err(invalid(
                        "select at least one visible mesh outside the custom collision selection",
                    ));
                }
            } else if body.name == "body" {
                model
                    .src_parts
                    .retain(|part| !selected.contains(part.as_str()));
                if model.src_parts.is_empty() {
                    model.smd.clear();
                }
            } else if model
                .src_parts
                .iter()
                .any(|part| selected.contains(part.as_str()))
            {
                return Err(invalid(
                    "a custom collision mesh cannot also belong to a visible bodygroup",
                ));
            }
        }
    }
    description.bodyparts.retain(|body| {
        body.name != "body" || body.models.iter().any(|model| !model.smd.is_empty())
    });
    if description.bodyparts.is_empty() {
        return Err(invalid(
            "select at least one visible mesh outside the custom collision selection",
        ));
    }
    Ok(())
}

type MeshGeometry = (Vec<[f32; 3]>, Vec<[u32; 3]>);

fn mesh_geometry(
    compiled: &CompiledModelDesc,
    cancel: &AtomicBool,
) -> Result<MeshGeometry, ModelCompileError> {
    mesh_geometry_with_normals(compiled, cancel, false)
}

fn mesh_geometry_with_normals(
    compiled: &CompiledModelDesc,
    cancel: &AtomicBool,
    preserve_normals: bool,
) -> Result<MeshGeometry, ModelCompileError> {
    let transforms = (!compiled.desc.model.static_prop).then(|| initial_pose_transforms(compiled));
    let mut points = Vec::new();
    let mut faces = Vec::new();
    let mut seen = HashMap::new();
    for model in compiled
        .bodyparts
        .iter()
        .filter_map(|body| body.models.first())
    {
        for mesh in &model.meshes {
            check_cancel(cancel)?;
            if faces.len().saturating_add(mesh.triangles.len()) > MAX_FACES {
                return Err(invalid(
                    "collision generation supports at most 1,000,000 triangles; import a simpler collision mesh",
                ));
            }
            for triangle in &mesh.triangles {
                let mut face = [0; 3];
                for (target, index) in face.iter_mut().zip(triangle) {
                    let vertex = mesh.vertices.get(*index as usize).ok_or_else(|| {
                        invalid("collision source contains an invalid triangle index")
                    })?;
                    // Prop collision is fixed in the initial pose, regardless of the visual skeleton.
                    let mut point = vertex.pos;
                    if let Some(transforms) = &transforms {
                        point = [0.0; 3];
                        for &[bone, weight] in &vertex.bones {
                            let transform = transforms.get(bone as usize).ok_or_else(|| {
                                invalid("collision source contains an invalid bone index")
                            })?;
                            for axis in 0..3 {
                                let row = &transform[axis * 4..axis * 4 + 4];
                                point[axis] += weight
                                    * (row[0] * vertex.pos[0]
                                        + row[1] * vertex.pos[1]
                                        + row[2] * vertex.pos[2]
                                        + row[3]);
                            }
                        }
                    }
                    // The PHY writer applies StudioMDL's source-to-engine yaw.
                    let point = [point[1], -point[0], point[2]];
                    let position =
                        point.map(|value| if value == 0.0 { 0 } else { value.to_bits() });
                    let normal = if preserve_normals {
                        vertex
                            .normal
                            .map(|value| if value == 0.0 { 0 } else { value.to_bits() })
                    } else {
                        [0; 3]
                    };
                    let key = (position, normal);
                    *target = *seen.entry(key).or_insert_with(|| {
                        let next = points.len() as u32;
                        points.push(point);
                        next
                    });
                }
                if face[0] != face[1] && face[1] != face[2] && face[2] != face[0] {
                    faces.push(face);
                }
            }
        }
    }
    if points.iter().flatten().any(|value| !value.is_finite()) {
        return Err(invalid("collision source contains non-finite positions"));
    }
    Ok((points, faces))
}

fn initial_pose_transforms(compiled: &CompiledModelDesc) -> Vec<mdlc::bone_math::Matrix3x4> {
    let desc = &compiled.desc;
    let parents = mdlc::compile::bone_parents(desc);
    let (positions, rotations): (Vec<_>, Vec<_>) = (0..desc.bones.len())
        .map(|index| mdlc::compile::resolve_bone_pose(desc, compiled, index))
        .unzip();
    let inverse_bind = mdlc::bone_math::compute_pose_to_bone(&positions, &rotations, &parents);
    let reference: Vec<_> = positions
        .into_iter()
        .zip(rotations)
        .enumerate()
        .map(|(bone, (position, rotation))| mdlc::smd::SmdPose {
            bone: bone as i32,
            position,
            rotation,
        })
        .collect();
    let frame = compiled
        .sequences
        .first()
        .and_then(|sequence| sequence.frames.first())
        .unwrap_or(&reference);
    mdlc::compile::frame_worlds(desc, &parents, frame)
        .iter()
        .zip(inverse_bind)
        .map(|(world, bind)| mdlc::bone_math::concat(world, &bind))
        .collect()
}

fn hull(vertices: &[[f32; 3]]) -> Result<PhyHull, ModelCompileError> {
    PhyHull::from_points(vertices)
        .map(|hull| hull.compact_outer_hull())
        .map_err(|error| invalid(&format!("collision mesh has no usable volume: {error}")))
}

fn connected_components(
    vertices: &[[f32; 3]],
    faces: &[[u32; 3]],
    cancel: &AtomicBool,
) -> Result<Vec<MeshGeometry>, ModelCompileError> {
    let mut adjacency = vec![Vec::new(); vertices.len()];
    for (index, face) in faces.iter().enumerate() {
        for point in face {
            adjacency[*point as usize].push(index);
        }
    }
    let mut visited = vec![false; faces.len()];
    let mut components = Vec::new();
    for first in 0..faces.len() {
        if visited[first] {
            continue;
        }
        check_cancel(cancel)?;
        let mut pending = vec![first];
        visited[first] = true;
        let mut points = Vec::new();
        let mut triangles = Vec::new();
        let mut remap = HashMap::new();
        while let Some(index) = pending.pop() {
            let mut triangle = [0; 3];
            for (target, point) in triangle.iter_mut().zip(faces[index]) {
                *target = *remap.entry(point).or_insert_with(|| {
                    let next = points.len() as u32;
                    points.push(vertices[point as usize]);
                    next
                });
                for neighbor in &adjacency[point as usize] {
                    if !std::mem::replace(&mut visited[*neighbor], true) {
                        pending.push(*neighbor);
                    }
                }
            }
            triangles.push(triangle);
        }
        components.push((points, triangles));
    }
    Ok(components)
}

fn decompose(
    vertices: &[[f32; 3]],
    faces: &[[u32; 3]],
    resolution: u32,
    limit: u32,
    cancel: &AtomicBool,
) -> Result<Vec<PhyHull>, ModelCompileError> {
    check_cancel(cancel)?;
    let mut hulls: Vec<_> = mdlc::phy::decompose_concave(vertices, faces, resolution, limit)
        .map_err(pipeline::PipelineError::BuildPhy)?
        .into_iter()
        .map(|part| part.compact_outer_hull())
        .collect();
    let before = hulls.len();
    hulls.retain(|part| has_volume(&part.vertices, &part.faces));
    if hulls.len() != before {
        mdlc::warning!(
            "Automatic collision discarded {} zero-volume decomposition artifacts",
            before - hulls.len()
        );
    }
    if hulls.is_empty() {
        return Err(invalid(
            "automatic decomposition produced no collision pieces with volume; provide a closed custom collision mesh",
        ));
    }
    check_cancel(cancel)?;
    Ok(hulls)
}

fn has_volume(vertices: &[[f32; 3]], faces: &[[u32; 3]]) -> bool {
    if vertices.len() < 4 {
        return false;
    }
    let a = vertices[0].map(f64::from);
    let extent = vertices
        .iter()
        .flat_map(|point| (0..3).map(move |axis| (f64::from(point[axis]) - a[axis]).abs()))
        .fold(0.0_f64, f64::max);
    for face in faces {
        let [a, b, c] = face.map(|index| vertices[index as usize].map(f64::from));
        let u: [f64; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
        let v: [f64; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
        let normal = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
        if length <= extent * extent * 1.0e-12 {
            continue;
        }
        return vertices.iter().any(|point| {
            (0..3)
                .map(|axis| normal[axis] * (f64::from(point[axis]) - a[axis]))
                .sum::<f64>()
                .abs()
                > length * extent * 1.0e-7
        });
    }
    false
}

fn validate_convex(vertices: &[[f32; 3]], faces: &[[u32; 3]]) -> Result<PhyHull, &'static str> {
    if vertices.len() < 4 || faces.len() < 4 {
        return Err("has no usable volume");
    }
    if faces.len() > mdlc::phy::MAX_TRIANGLES_PER_HULL {
        return Err("has too many triangles for a custom collision piece");
    }
    let mut edges = HashMap::<[u32; 2], usize>::new();
    for face in faces {
        for [a, b] in [[face[0], face[1]], [face[1], face[2]], [face[2], face[0]]] {
            *edges.entry([a.min(b), a.max(b)]).or_default() += 1;
        }
    }
    if edges.values().any(|count| *count != 2) {
        return Err("is open or non-manifold");
    }
    let center: [f64; 3] = std::array::from_fn(|axis| {
        vertices
            .iter()
            .map(|point| f64::from(point[axis]))
            .sum::<f64>()
            / vertices.len() as f64
    });
    let extent = vertices
        .iter()
        .map(|point| {
            (0..3)
                .map(|axis| (f64::from(point[axis]) - center[axis]).abs())
                .fold(0.0_f64, f64::max)
        })
        .fold(0.0_f64, f64::max);
    let tolerance = extent * 1.0e-5;
    for face in faces {
        let [a, b, c] = face.map(|index| vertices[index as usize].map(f64::from));
        let u: [f64; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
        let v: [f64; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
        let normal = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
        if length <= extent * extent * 1.0e-12 {
            return Err("contains degenerate triangles");
        }
        let distance = |point: [f64; 3]| {
            (0..3)
                .map(|axis| normal[axis] * (point[axis] - a[axis]))
                .sum::<f64>()
                / length
        };
        let inside = distance(center);
        if inside.abs() <= tolerance {
            return Err("has no usable volume");
        }
        if vertices
            .iter()
            .any(|point| distance(point.map(f64::from)) * inside.signum() < -tolerance)
        {
            return Err("is concave");
        }
    }
    hull(vertices).map_err(|_| "has no usable volume")
}

#[cfg(test)]
#[path = "collision_tests.rs"]
mod tests;
