use std::{
    collections::HashSet,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use vformats::mdl::{self, Mdl, MdlModel, Vvd, VvdVertex};

#[path = "model_decompile/blender.rs"]
mod blender;
pub use blender::{BlenderExportReport, export_blender_model, export_blender_model_with_cancel};

#[path = "model_decompile/animations.rs"]
mod animations;
#[path = "model_decompile/flexes.rs"]
mod flexes;
#[path = "model_decompile/lods.rs"]
mod lods;
#[path = "model_decompile/metadata.rs"]
mod metadata;
#[path = "model_decompile/physics.rs"]
mod physics;
pub use physics::{CollisionMesh, load_collision_meshes};
#[path = "model_decompile/preview_lods.rs"]
mod preview_lods;
pub use preview_lods::{ModelLodPreview, load_model_lod_preview};

const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecompileReport {
    pub output_directory: PathBuf,
    pub qc_path: PathBuf,
    pub files: Vec<PathBuf>,
    pub bytes_written: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecompileError {
    #[error("Model export was cancelled")]
    Cancelled,
    #[error("The destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("Invalid model data: {0}")]
    InvalidModel(String),
    #[error("The model exceeds the export size limit")]
    SizeLimit,
    #[error("Cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        source: crate::IoFailure,
    },
}

impl crate::HasErrorKey for DecompileError {
    fn error_key(&self) -> crate::ErrorKey {
        crate::error_keys::IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub fn decompile_model(
    mdl_path: &Path,
    destination: &Path,
) -> Result<DecompileReport, DecompileError> {
    decompile_model_with_cancel(mdl_path, destination, &AtomicBool::new(false))
}

pub fn decompile_model_with_cancel(
    mdl_path: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<DecompileReport, DecompileError> {
    check_cancelled(cancelled)?;
    if !mdl_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"))
    {
        return Err(invalid(
            "Choose a .mdl file with its matching .vvd and .vtx files",
        ));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(DecompileError::DestinationExists(destination.to_path_buf()));
    }
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("Choose a new destination directory"))?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
    let destination = parent.join(name);
    let LoadedModel {
        model,
        vertices,
        topology,
        raw_mdl,
        raw_vvd,
        raw_vtx,
        ani: raw_ani,
    } = load_model(mdl_path, cancelled)?;
    let lod_metadata =
        lods::Metadata::read(&raw_mdl, &raw_vvd, &raw_vtx, &model, &vertices, &topology)?;
    if model.body_parts.len() != topology.body_parts.len() {
        return Err(invalid("The MDL and VTX bodygroups do not match"));
    }
    let temporary = tempfile::Builder::new()
        .prefix(".model-export-")
        .tempdir_in(&parent)
        .map_err(|error| io_error(&parent, error))?;
    let mut export = ExportFiles {
        directory: temporary.path(),
        bytes: 0,
    };
    let mut qc = format!("$modelname {}\n", quoted(&safe_model_name(&model.name)?)?);
    if model.flags & mdl::FLAG_STATIC_PROP != 0 {
        qc.push_str("$staticprop\n");
    }
    for directory in &model.texture_paths {
        writeln!(qc, "$cdmaterials {}", quoted(directory)?).unwrap();
    }
    write_bounds(&mut qc, &raw_mdl)?;
    write_header_metadata(&mut qc, &raw_mdl)?;
    let metadata = metadata::write_metadata(&mut qc, &raw_mdl, &model)?;
    let procedural_warnings = metadata::write_procedural(&mut qc, &mut export, &raw_mdl, &model)?;
    let flexes = flexes::write(
        &mut export,
        &raw_mdl,
        &model,
        &vertices,
        &lod_metadata,
        cancelled,
    )?;
    let mut any_geometry = false;
    for (group, (body_part, vtx_part)) in model
        .body_parts
        .iter()
        .zip(&topology.body_parts)
        .enumerate()
    {
        check_cancelled(cancelled)?;
        if body_part.models.len() != vtx_part.models.len() {
            return Err(invalid("The MDL and VTX bodygroup choices do not match"));
        }
        let group_name =
            bodygroup_name(&raw_mdl, group).unwrap_or_else(|| format!("bodygroup_{group}"));
        let model_block = body_part.models.len() == 1 && flexes.blocks[group][0].is_some();
        if !model_block {
            writeln!(qc, "$bodygroup {}\n{{", quoted(&group_name)?).unwrap();
        }
        for (choice, (source, vtx)) in body_part.models.iter().zip(&vtx_part.models).enumerate() {
            check_cancelled(cancelled)?;
            if source.meshes.is_empty() {
                qc.push_str("    blank\n");
                continue;
            }
            let filename = format!("reference_{group}_{choice}.smd");
            let smd = reference_smd(
                &model,
                source,
                &vtx.lods[0],
                &vertices,
                &lod_metadata.mesh_vertices[group][choice],
                cancelled,
            )?;
            export.write(&filename, smd.as_bytes())?;
            if model_block {
                writeln!(
                    qc,
                    "$model {} \"{filename}\" srcname {} {{\n{}}}",
                    quoted(&group_name)?,
                    quoted(&source.name)?,
                    flexes.blocks[group][choice].as_deref().unwrap()
                )
                .unwrap();
            } else {
                writeln!(
                    qc,
                    "    studio \"{filename}\" srcname {}",
                    quoted(&source.name)?
                )
                .unwrap();
            }
            any_geometry = true;
        }
        if !model_block {
            qc.push_str("}\n");
        }
    }
    if !any_geometry {
        return Err(invalid("The model contains no exportable render geometry"));
    }
    lods::write(
        &mut qc,
        &mut export,
        &model,
        &vertices,
        &topology,
        &lod_metadata,
        cancelled,
    )?;
    write_skins(&mut qc, &model)?;
    let animations = animations::write_animations(
        &mut qc,
        &mut export,
        &raw_mdl,
        raw_ani.as_deref(),
        &model,
        cancelled,
    )?;
    let raw_phy = sibling(mdl_path, "phy")
        .ok()
        .map(|path| read_input(&path))
        .transpose()?;
    let physics = physics::write_physics(
        &mut qc,
        &mut export,
        &raw_mdl,
        raw_phy.as_deref(),
        &model,
        Some(&animations.first_sequence_pose),
    )?;
    mdlc::qc::parse_qc_str(&qc, temporary.path()).map_err(|errors| {
        invalid(format!(
            "Reconstructed QC is invalid: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    export.write("model.qc", qc.as_bytes())?;
    let preserved: Vec<_> = metadata
        .preserved
        .iter()
        .chain(&flexes.preserved)
        .copied()
        .collect();
    let mut warnings = export_warnings(&raw_mdl, mdl_path, &preserved);
    warnings.extend(metadata.warnings);
    warnings.extend(procedural_warnings);
    warnings.extend(animations.warnings);
    warnings.extend(physics.warnings);
    if physics.preserved {
        warnings.retain(|warning| !warning.starts_with("Physics and ragdoll"));
    }
    check_cancelled(cancelled)?;
    let material_root = material_source_root(mdl_path);
    let mut texture_names = model.textures.clone();
    for pair in lod_metadata.material_replacements.iter().flatten() {
        if !texture_names.contains(&pair[1]) {
            texture_names.push(pair[1].clone());
        }
    }
    let materials = crate::model_materials::prepare_materials(
        &texture_names,
        &model.texture_paths,
        &material_root,
    )
    .map_err(|error| invalid(format!("Cannot export model materials: {error}")))?;
    warnings.extend(
        materials
            .write_decompiled_to(temporary.path())
            .map_err(|error| invalid(format!("Cannot export model materials: {error}")))?,
    );
    check_cancelled(cancelled)?;
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(DecompileError::DestinationExists(destination));
    }
    let mut files = Vec::new();
    let mut bytes_written = 0u64;
    for entry in walkdir::WalkDir::new(temporary.path()).min_depth(1) {
        let entry = entry.map_err(|error| invalid(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        bytes_written = bytes_written
            .checked_add(
                entry
                    .metadata()
                    .map_err(|error| invalid(error.to_string()))?
                    .len(),
            )
            .filter(|bytes| *bytes <= MAX_OUTPUT_BYTES as u64)
            .ok_or(DecompileError::SizeLimit)?;
        files.push(
            destination.join(
                entry
                    .path()
                    .strip_prefix(temporary.path())
                    .map_err(|error| invalid(error.to_string()))?,
            ),
        );
    }
    files.sort();
    check_cancelled(cancelled)?;
    fs::rename(temporary.path(), &destination).map_err(|error| io_error(&destination, error))?;
    Ok(DecompileReport {
        qc_path: destination.join("model.qc"),
        output_directory: destination,
        files,
        bytes_written,
        warnings,
    })
}

struct LoadedModel {
    model: Mdl,
    vertices: Vvd,
    topology: mdl::Vtx,
    raw_mdl: Vec<u8>,
    raw_vvd: Vec<u8>,
    raw_vtx: Vec<u8>,
    ani: Option<Vec<u8>>,
}

fn load_model(mdl_path: &Path, cancelled: &AtomicBool) -> Result<LoadedModel, DecompileError> {
    check_cancelled(cancelled)?;
    let raw_mdl = read_input(mdl_path)?;
    let vvd_path = sibling(mdl_path, "vvd")?;
    let vtx_path = ["dx90.vtx", "vtx", "dx80.vtx", "sw.vtx"]
        .into_iter()
        .find_map(|extension| sibling(mdl_path, extension).ok())
        .ok_or_else(|| invalid("The matching .dx90.vtx (or .vtx) file is missing"))?;
    let limits = vformats::Limits {
        max_input_bytes: MAX_INPUT_BYTES,
        max_entries: 1_000_000,
        ..vformats::Limits::default()
    };
    check_cancelled(cancelled)?;
    let mut geometry_header = raw_mdl.clone();
    // Animation sections and external .ani blocks require a separate decoder.
    // Do not feed them to the geometry parser's partial animation implementation.
    geometry_header
        .get_mut(180..184)
        .ok_or_else(|| invalid("Truncated MDL header"))?
        .fill(0);
    let mut model =
        mdl::parse_mdl(&geometry_header, &limits).map_err(|error| invalid(error.to_string()))?;
    normalize_material_names(&mut model);
    let raw_vvd = read_input(&vvd_path)?;
    let raw_vtx = read_input(&vtx_path)?;
    let ani = if u32_at(&raw_mdl, 352).unwrap_or(0) > 1 {
        sibling(mdl_path, "ani")
            .ok()
            .map(|path| read_input(&path))
            .transpose()?
    } else {
        None
    };
    if let Some(ani) = &ani
        && (ani.get(..4) != Some(&mdlc::ani_writer::ANI_ID.to_le_bytes())
            || !matches!(u32_at(ani, 4), Some(48 | 49))
            || u32_at(ani, 76) != u32::try_from(ani.len()).ok()
            || !u32_at(ani, 8)
                .is_some_and(|checksum| checksum == 0 || checksum == model.checksum as u32))
    {
        return Err(invalid("The ANI file does not match the MDL"));
    }
    let vertices = mdl::parse_vvd(&raw_vvd, &limits).map_err(|error| invalid(error.to_string()))?;
    let topology = mdl::parse_vtx(&raw_vtx, &limits).map_err(|error| invalid(error.to_string()))?;
    if model.checksum != vertices.checksum || model.checksum != topology.checksum {
        return Err(invalid("The MDL, VVD and VTX checksums do not match"));
    }
    validate_skeleton(&model)?;
    Ok(LoadedModel {
        model,
        vertices,
        topology,
        raw_mdl,
        raw_vvd,
        raw_vtx,
        ani,
    })
}

fn normalize_material_names(model: &mut Mdl) {
    for name in &mut model.textures {
        *name = mdlc::mdl_writer::normalize_texture_name(name, &[]);
    }
}

struct ExportFiles<'a> {
    directory: &'a Path,
    bytes: usize,
}

impl ExportFiles<'_> {
    fn write(&mut self, name: &str, contents: &[u8]) -> Result<(), DecompileError> {
        self.bytes = self
            .bytes
            .checked_add(contents.len())
            .filter(|bytes| *bytes <= MAX_OUTPUT_BYTES)
            .ok_or(DecompileError::SizeLimit)?;
        let file = self.directory.join(name);
        fs::write(&file, contents).map_err(|error| io_error(&file, error))?;
        Ok(())
    }
}

fn reference_smd(
    model: &Mdl,
    source: &MdlModel,
    lod: &mdl::VtxLod,
    vertices: &Vvd,
    mesh_vertex_counts: &[usize],
    cancelled: &AtomicBool,
) -> Result<String, DecompileError> {
    if source.meshes.len() != lod.meshes.len() {
        return Err(invalid("The MDL and VTX mesh counts do not match"));
    }
    let mut text = skeleton_smd(model)?;
    text.push_str("triangles\n");
    for ((mesh, vtx_mesh), &mesh_vertices) in source
        .meshes
        .iter()
        .zip(&lod.meshes)
        .zip(mesh_vertex_counts)
    {
        let slot = usize::try_from(mesh.material).map_err(|_| invalid("Negative material slot"))?;
        let texture = if model.skin_reference_count > 0 {
            usize::from(
                *model
                    .skin_table
                    .get(slot)
                    .filter(|_| slot < model.skin_reference_count)
                    .ok_or_else(|| invalid("Mesh material slot is outside the skin table"))?,
            )
        } else {
            slot
        };
        let material = model
            .textures
            .get(texture)
            .ok_or_else(|| invalid("Mesh material is outside the material table"))?;
        validate_material(material)?;
        let offset = i64::from(source.vertex_offset) + i64::from(mesh.vertex_offset);
        let offset = usize::try_from(offset).map_err(|_| invalid("Negative vertex offset"))?;
        for strip_group in &vtx_mesh.strip_groups {
            check_cancelled(cancelled)?;
            for strip in &strip_group.strips {
                if strip.flags & 3 == 0 || strip.flags & 3 == 3 {
                    return Err(invalid("Unsupported VTX strip topology"));
                }
                let mut triangle = Vec::with_capacity(3);
                // VTX emits Source winding; its parser reverses each triangle to SMD winding.
                for pool_index in strip.triangle_index_positions() {
                    let index = strip_group
                        .indices
                        .get(pool_index)
                        .ok_or_else(|| invalid("Invalid triangle index range"))?;
                    let vertex = strip_group
                        .vertices
                        .get(usize::from(*index))
                        .ok_or_else(|| invalid("Invalid strip vertex"))?;
                    if usize::from(vertex.original_mesh_vertex_id) >= mesh_vertices {
                        return Err(invalid("A VTX vertex is outside its MDL mesh"));
                    }
                    let index = offset
                        .checked_add(usize::from(vertex.original_mesh_vertex_id))
                        .ok_or(DecompileError::SizeLimit)?;
                    let vertex = vertices
                        .vertices
                        .get(index)
                        .ok_or_else(|| invalid("A triangle references an absent vertex"))?;
                    triangle.push(vertex);
                    if triangle.len() == 3 {
                        writeln!(text, "{}", quoted(material)?).unwrap();
                        for vertex in &triangle {
                            write_vertex(
                                &mut text,
                                vertex,
                                model.bones.len().max(1),
                                model.flags & mdl::FLAG_STATIC_PROP != 0,
                            )?;
                        }
                        triangle.clear();
                        if text.len() > MAX_OUTPUT_BYTES {
                            return Err(DecompileError::SizeLimit);
                        }
                    }
                }
                if !triangle.is_empty() {
                    return Err(invalid(
                        "A VTX triangle list contains an incomplete triangle",
                    ));
                }
            }
        }
    }
    text.push_str("end\n");
    Ok(text)
}

fn write_vertex(
    text: &mut String,
    vertex: &VvdVertex,
    bone_count: usize,
    static_prop: bool,
) -> Result<(), DecompileError> {
    if !vertex
        .position
        .iter()
        .chain(&vertex.normal)
        .chain(&vertex.uv)
        .all(|value| value.is_finite())
    {
        return Err(invalid("A vertex contains nonfinite coordinates"));
    }
    let count = usize::from(vertex.bone_count);
    if count > 3 {
        return Err(invalid("A vertex has more than three bone weights"));
    }
    let weights = &vertex.weights[..count];
    let bones = &vertex.bones[..count];
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || *weight < 0.0)
        || bones.iter().any(|bone| usize::from(*bone) >= bone_count)
        || (count > 0 && (weights.iter().sum::<f32>() - 1.0).abs() > 0.01)
    {
        return Err(invalid("A vertex has invalid bone weights"));
    }
    let (position, normal) = if static_prop {
        // $staticprop compilation applies +90 degrees around Z to source geometry.
        let inverse = |v: [f32; 3]| [v[1], -v[0], v[2]];
        (inverse(vertex.position), inverse(vertex.normal))
    } else {
        (vertex.position, vertex.normal)
    };
    let parent = bones.first().copied().unwrap_or(0);
    write!(
        text,
        "{parent} {} {} {} {} {} {} {} {} {}",
        position[0],
        position[1],
        position[2],
        normal[0],
        normal[1],
        normal[2],
        vertex.uv[0],
        1.0 - vertex.uv[1],
        count.max(1)
    )
    .unwrap();
    if count == 0 {
        text.push_str(" 0 1");
    }
    for (&bone, &weight) in bones.iter().zip(weights) {
        write!(text, " {bone} {weight}").unwrap();
    }
    text.push('\n');
    Ok(())
}

fn skeleton_smd(model: &Mdl) -> Result<String, DecompileError> {
    let mut text = String::from("version 1\nnodes\n");
    if model.bones.is_empty() {
        text.push_str("0 \"root\" -1\n");
    }
    for (index, bone) in model.bones.iter().enumerate() {
        writeln!(text, "{index} {} {}", quoted(&bone.name)?, bone.parent).unwrap();
    }
    text.push_str("end\nskeleton\ntime 0\n");
    if model.bones.is_empty() {
        text.push_str("0 0 0 0 0 0 0\n");
    }
    for (index, bone) in model.bones.iter().enumerate() {
        writeln!(
            text,
            "{index} {} {} {} {} {} {}",
            bone.position[0],
            bone.position[1],
            bone.position[2],
            bone.rotation[0],
            bone.rotation[1],
            bone.rotation[2]
        )
        .unwrap();
    }
    text.push_str("end\n");
    Ok(text)
}

fn validate_skeleton(model: &Mdl) -> Result<(), DecompileError> {
    let mut names = HashSet::new();
    for (index, bone) in model.bones.iter().enumerate() {
        if bone.name.is_empty() || !names.insert(bone.name.to_lowercase()) {
            return Err(invalid("Skeleton bone names must be nonempty and unique"));
        }
        if !bone
            .position
            .iter()
            .chain(&bone.rotation)
            .chain(&bone.quaternion)
            .chain(bone.pose_to_bone.iter().flatten())
            .all(|value| value.is_finite())
        {
            return Err(invalid("A bone contains nonfinite coordinates"));
        }
        let mut current = index;
        for step in 0..=model.bones.len() {
            let parent = model.bones[current].parent;
            if parent == -1 {
                break;
            }
            current = usize::try_from(parent)
                .ok()
                .filter(|parent| *parent < model.bones.len())
                .ok_or_else(|| invalid("A bone references an absent parent"))?;
            if step == model.bones.len() {
                return Err(invalid("The skeleton contains a parent cycle"));
            }
        }
    }
    Ok(())
}

fn write_skins(qc: &mut String, model: &Mdl) -> Result<(), DecompileError> {
    if model.skin_reference_count == 0 || model.skin_table.len() <= model.skin_reference_count {
        return Ok(());
    }
    qc.push_str("$texturegroup \"skins\"\n{\n");
    for family in model.skin_table.chunks(model.skin_reference_count) {
        qc.push_str("    {");
        for &texture in family {
            let name = model
                .textures
                .get(usize::from(texture))
                .ok_or_else(|| invalid("A skin references an absent material"))?;
            validate_material(name)?;
            write!(qc, " {}", quoted(name)?).unwrap();
        }
        qc.push_str(" }\n");
    }
    qc.push_str("}\n");
    Ok(())
}

fn write_bounds(qc: &mut String, bytes: &[u8]) -> Result<(), DecompileError> {
    for (command, offset) in [("$bbox", 104), ("$cbox", 128)] {
        let mut bounds = [0.0f32; 6];
        for (index, value) in bounds.iter_mut().enumerate() {
            let field = bytes
                .get(offset + index * 4..offset + index * 4 + 4)
                .ok_or_else(|| invalid("Truncated bounds"))?;
            *value = f32::from_le_bytes(field.try_into().unwrap());
            if !value.is_finite() {
                return Err(invalid("Model bounds contain nonfinite coordinates"));
            }
        }
        writeln!(
            qc,
            "{command} {} {} {} {} {} {}",
            bounds[0], bounds[1], bounds[2], bounds[3], bounds[4], bounds[5]
        )
        .unwrap();
    }
    Ok(())
}

fn write_header_metadata(qc: &mut String, bytes: &[u8]) -> Result<(), DecompileError> {
    for (command, offset) in [("$eyeposition", 80), ("$illumposition", 92)] {
        let mut value = [0.0f32; 3];
        for (axis, coordinate) in value.iter_mut().enumerate() {
            let field = bytes
                .get(offset + axis * 4..offset + axis * 4 + 4)
                .ok_or_else(|| invalid("Truncated model position"))?;
            *coordinate = f32::from_le_bytes(field.try_into().unwrap());
            if !coordinate.is_finite() {
                return Err(invalid("A model position contains nonfinite coordinates"));
            }
        }
        // QC positions rotate +90 degrees around Z when compiled.
        writeln!(qc, "{command} {} {} {}", value[1], -value[0], value[2]).unwrap();
    }
    if let Some(offset) = u32_at(bytes, 308).filter(|offset| *offset > 0) {
        let tail = bytes
            .get(offset as usize..)
            .ok_or_else(|| invalid("Invalid surface property offset"))?;
        let name = tail
            .get(
                ..tail
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or_else(|| invalid("Unterminated surface property"))?,
            )
            .and_then(|name| std::str::from_utf8(name).ok())
            .ok_or_else(|| invalid("Invalid surface property name"))?;
        if !name.is_empty() {
            writeln!(qc, "$surfaceprop {}", quoted(name)?).unwrap();
        }
    }
    Ok(())
}

fn export_warnings(bytes: &[u8], source: &Path, preserved: &[usize]) -> Vec<String> {
    let mut warnings = vec!["The QC was reconstructed from compiled model data; original source names and authoring commands cannot be recovered exactly.".to_owned(), "Materials and textures must be supplied separately when they are stored in game archives or another addon.".to_owned()];
    for (offset, description) in [
        (172, "Hitbox sets"),
        (240, "Attachments"),
        (260, "Flexes"),
        (284, "IK chains"),
        (292, "Mouth controllers"),
        (300, "Pose parameters"),
        (336, "Included models"),
    ] {
        if !preserved.contains(&offset) && u32_at(bytes, offset).unwrap_or(0) > 0 {
            warnings.push(format!("{description} were not reconstructed."));
        }
    }
    if source.with_extension("phy").exists() {
        warnings.push("Physics and ragdoll constraints were not reconstructed.".to_owned());
    }
    warnings
}

fn material_source_root(source: &Path) -> PathBuf {
    for ancestor in source.ancestors().skip(1) {
        if ancestor
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("models"))
        {
            return ancestor
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
        }
    }
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn bodygroup_name(bytes: &[u8], index: usize) -> Option<String> {
    let base = usize::try_from(u32_at(bytes, 236)?)
        .ok()?
        .checked_add(index.checked_mul(16)?)?;
    let offset = i32::from_le_bytes(bytes.get(base..base + 4)?.try_into().ok()?);
    let start = usize::try_from(i64::try_from(base).ok()? + i64::from(offset)).ok()?;
    let tail = bytes.get(start..)?;
    let name = std::str::from_utf8(tail.get(..tail.iter().position(|byte| *byte == 0)?)?).ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

fn safe_model_name(name: &str) -> Result<String, DecompileError> {
    let name = name.replace('\\', "/");
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(':')
        || name.split('/').any(|part| matches!(part, ".." | "." | ""))
    {
        return Err(invalid("The model name is not a safe relative path"));
    }
    Ok(name)
}

fn validate_material(name: &str) -> Result<(), DecompileError> {
    quoted(name)?;
    if name.trim().is_empty() || name.contains("//") {
        return Err(invalid(
            "A material name cannot be represented safely in SMD",
        ));
    }
    Ok(())
}

fn quoted(value: &str) -> Result<String, DecompileError> {
    if value.chars().any(|ch| ch.is_control() || ch == '"') {
        return Err(invalid(
            "A model name contains unsupported control characters or quotes",
        ));
    }
    Ok(format!("\"{}\"", value.replace('\\', "/")))
}

fn sibling(path: &Path, extension: &str) -> Result<PathBuf, DecompileError> {
    let expected = path.with_extension(extension);
    if expected.is_file() {
        return Ok(expected);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = expected
        .file_name()
        .ok_or_else(|| invalid("Invalid model filename"))?
        .to_string_lossy();
    for entry in fs::read_dir(parent).map_err(|error| io_error(parent, error))? {
        let entry = entry.map_err(|error| io_error(parent, error))?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(&name)
            && entry.path().is_file()
        {
            return Ok(entry.path());
        }
    }
    Err(invalid(format!(
        "Missing companion file: {}",
        expected.display()
    )))
}

fn read_input(path: &Path) -> Result<Vec<u8>, DecompileError> {
    let file = fs::File::open(path).map_err(|error| io_error(path, error))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(DecompileError::SizeLimit);
    }
    Ok(bytes)
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), DecompileError> {
    if cancelled.load(Ordering::Relaxed) {
        Err(DecompileError::Cancelled)
    } else {
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> DecompileError {
    DecompileError::InvalidModel(message.into())
}
fn io_error(path: &Path, error: io::Error) -> DecompileError {
    DecompileError::Io {
        path: path.to_path_buf(),
        source: error.into(),
    }
}

#[cfg(test)]
#[path = "model_decompile/tests.rs"]
mod tests;

pub use animations::preview::{
    AnimatedVertex, ModelAnimationPreview, PreviewPose, PreviewPoseParameter, PreviewSequence,
    load_animation_preview,
};
