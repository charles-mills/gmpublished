use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::model_compile::{
    self, ModelCompileError, ModelCompileRequest, ModelOutputFile, ModelOutputKind,
};
use crate::model_materials;

#[path = "model_project/portable_qc.rs"]
mod portable_qc;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelStage {
    pub map: PathBuf,
    pub position: [f32; 3],
    pub angles: [f32; 3],
    pub selected_skin: usize,
    pub bodygroups: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSkin {
    pub name: String,
    pub replacements: BTreeMap<String, SkinMaterial>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkinMaterial {
    Material { name: String },
    Texture { path: PathBuf },
    Tint { color: [u8; 3] },
}

pub fn save_model_qc(request: &ModelCompileRequest, path: &Path) -> Result<(), ModelCompileError> {
    crate::util::main_thread_forbidden!();
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("qc"))
    {
        return Err(invalid("model definitions must be saved as QC"));
    }
    let source = canonical(&request.source)?;
    let base = source.parent().expect("canonical source has parent");
    let destination = canonical(
        path.parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let filename = path
        .file_name()
        .ok_or_else(|| invalid("QC filename is required"))?;
    let final_path = destination.join(filename);
    if final_path.exists() && canonical(&final_path)? == source {
        return Err(invalid(
            "save edited QC to a different file to preserve the imported source",
        ));
    }
    let source_extension = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if source_extension == "toml" {
        return Err(invalid(
            "saving TOML as QC is not supported; keep the original TOML definition",
        ));
    }
    if !request.orientation.iter().all(|angle| angle.is_finite()) {
        return Err(invalid("orientation angles must be finite"));
    }
    if !request.export_materials
        && request
            .skins
            .iter()
            .flat_map(|skin| skin.replacements.values())
            .any(|replacement| !matches!(replacement, SkinMaterial::Material { .. }))
    {
        return Err(invalid(
            "authored skin textures and tints require material export",
        ));
    }
    let mut desc = model_compile::load_description(request, &source, base)?;
    desc.model.version = Some(48);
    desc.model.name = model_compile::normalize_model_name(
        request.model_name.as_deref().unwrap_or(&desc.model.name),
    )?;
    if let Some(directory) = &request.material_directory {
        desc.materials.search_paths = vec![directory.replace('\\', "/")];
    }
    let mut compiled =
        mdlc::compile::compile(&desc, base).map_err(mdlc::pipeline::PipelineError::Compile)?;
    model_compile::apply_orientation(&mut compiled, &source, request.orientation)?;
    let variants = model_compile::apply_skins(&mut compiled.desc, &request.skins)?;
    let stage = tempfile::Builder::new()
        .prefix(".gmpublished-qc-")
        .tempdir_in(&destination)
        .map_err(|error| io_error(&final_path, error))?;
    let files_root = stage.path().join("files");
    fs::create_dir(&files_root).map_err(|error| io_error(&files_root, error))?;
    let assets = asset_namespace(path);
    quote(&assets)?;
    let mut writes = Vec::new();
    let mut baked = false;
    let mut qc = if source_extension == "qc" {
        if request.generate_collision {
            return Err(invalid("QC descriptions define their own collision model"));
        }
        portable_qc::copy(
            request,
            &desc,
            &source,
            &files_root,
            &assets,
            &mut writes,
            &destination,
        )?
    } else {
        let flexes = compiled
            .bodyparts
            .iter()
            .flat_map(|body| &body.models)
            .any(|model| model.mesh_flexes.iter().any(|flexes| !flexes.is_empty()));
        let animated = compiled
            .sequences
            .iter()
            .any(|sequence| sequence.frames.len() > 1);
        let portable = !flexes
            && !animated
            && request.bodygroups.is_empty()
            && !desc
                .sequences
                .iter()
                .any(|sequence| sequence.src_stack.is_some());
        let bake_groups = request.static_prop && !flexes && !request.bodygroups.is_empty();
        let mut converted_sources = false;
        let (mesh, animation, axes, scale) = if bake_groups {
            baked = true;
            let idle = PathBuf::from(&assets).join("idle.smd");
            stage_asset(
                &files_root,
                &idle,
                (smd_header() + "end\n").as_bytes(),
                &mut writes,
            )?;
            (idle.clone(), idle, "", 1.0)
        } else if request.static_prop && portable {
            baked = true;
            let relative = PathBuf::from(&assets).join("mesh.smd");
            stage_asset(
                &files_root,
                &relative,
                static_mesh_smd(&compiled)?.as_bytes(),
                &mut writes,
            )?;
            (relative.clone(), relative, "", 1.0)
        } else if request.orientation != [0.0; 3] {
            if flexes {
                return Err(invalid(
                    "saving additional rotation for a mesh with flexes requires baking the rotation in its source",
                ));
            }
            return Err(invalid(
                "saving additional rotation for an animated mesh requires baking the rotation in its source",
            ));
        } else if source_extension == "smd" {
            let relative = PathBuf::from(&assets).join("mesh.smd");
            stage_asset(
                &files_root,
                &relative,
                &fs::read(&source).map_err(|error| io_error(&source, error))?,
                &mut writes,
            )?;
            (
                relative.clone(),
                relative,
                if request.automatic_axes && !request.lods.is_empty() {
                    " srcaxis auto"
                } else {
                    ""
                },
                request.scale,
            )
        } else if portable && compiled.desc.bones.len() <= 1 {
            converted_sources = true;
            let model = &desc.bodyparts[0].models[0];
            let options = mdlc::compile::SrcOpts::of_model(model)
                .map_err(|error| ModelCompileError::Description(error.to_string()))?;
            let geometry = mdlc::compile::read_source(&source, "QC geometry", &options)
                .map_err(|error| ModelCompileError::Description(error.to_string()))?;
            let animation = mdlc::compile::read_source_frames(&source, "QC animation", &options)
                .map_err(|error| ModelCompileError::Description(error.to_string()))?;
            let relative = PathBuf::from(&assets).join("mesh.smd");
            let idle = PathBuf::from(&assets).join("idle.smd");
            stage_asset(
                &files_root,
                &relative,
                source_smd(&geometry)?.as_bytes(),
                &mut writes,
            )?;
            stage_asset(
                &files_root,
                &idle,
                source_smd(&animation)?.as_bytes(),
                &mut writes,
            )?;
            (relative, idle, "", 1.0)
        } else {
            (
                SourceCopier::new(base, &files_root, &assets, &mut writes).copy(&source)?,
                PathBuf::new(),
                if request.automatic_axes && source_extension != "smd" {
                    " srcaxis auto"
                } else {
                    ""
                },
                request.scale,
            )
        };
        let mut text = format!(
            "$modelname {}\n$scale {scale}\n{}",
            quote(&desc.model.name)?,
            if request.static_prop {
                "$staticprop\n"
            } else {
                ""
            },
        );
        if request.bodygroups.is_empty() {
            writeln!(text, "$body body {}{axes}", quote_path(&mesh)?).unwrap();
        } else {
            for (body_index, body) in desc.bodyparts.iter().enumerate() {
                writeln!(text, "$bodygroup {} {{", quote(&body.name)?).unwrap();
                for (model_index, model) in body.models.iter().enumerate() {
                    if model.smd.is_empty() {
                        text.push_str(" blank\n");
                    } else {
                        let selected_mesh = if bake_groups {
                            let relative = PathBuf::from(&assets)
                                .join(format!("body-{body_index}-{model_index}.smd"));
                            stage_asset(
                                &files_root,
                                &relative,
                                mesh_smd(
                                    &compiled,
                                    compiled.bodyparts[body_index].models[model_index]
                                        .meshes
                                        .iter(),
                                )?
                                .as_bytes(),
                                &mut writes,
                            )?;
                            relative
                        } else {
                            mesh.clone()
                        };
                        write!(text, " studio {}{axes}", quote_path(&selected_mesh)?).unwrap();
                        write!(
                            text,
                            " srcname {}",
                            quote(&compiled.bodyparts[body_index].models[model_index].name)?
                        )
                        .unwrap();
                        if !bake_groups {
                            for part in &model.src_parts {
                                write!(text, " srcpart {}", quote(part)?).unwrap();
                            }
                        }
                        text.push('\n');
                    }
                }
                text.push_str("}\n");
            }
        }
        for (index, lod) in request.lods.iter().enumerate() {
            let relative = if baked {
                let relative = PathBuf::from(&assets).join(format!("lod-{}.smd", index + 1));
                stage_asset(
                    &files_root,
                    &relative,
                    static_lod_smd(&compiled, index + 1)?.as_bytes(),
                    &mut writes,
                )?;
                relative
            } else if converted_sources {
                let relative = PathBuf::from(&assets).join(format!("lod-{}.smd", index + 1));
                let options = mdlc::compile::SrcOpts::of_model(&desc.bodyparts[0].models[0])
                    .map_err(|error| ModelCompileError::Description(error.to_string()))?;
                let geometry = mdlc::compile::read_source(
                    &canonical(&lod.source)?,
                    "QC LOD geometry",
                    &options,
                )
                .map_err(|error| ModelCompileError::Description(error.to_string()))?;
                stage_asset(
                    &files_root,
                    &relative,
                    source_smd(&geometry)?.as_bytes(),
                    &mut writes,
                )?;
                relative
            } else {
                let lod_source = canonical(&lod.source)?;
                let namespace = format!("{assets}/lod-{}", index + 1);
                SourceCopier::new(
                    lod_source.parent().expect("canonical source has parent"),
                    &files_root,
                    &namespace,
                    &mut writes,
                )
                .copy(&lod_source)?
            };
            writeln!(
                text,
                "$lod {}\n{{\n replacemodel {} {}\n nofacial\n}}",
                lod.switch_point,
                quote_path(&mesh)?,
                quote_path(&relative)?
            )
            .unwrap();
        }
        let animation = if animation.as_os_str().is_empty() {
            &mesh
        } else {
            &animation
        };
        for (sequence_index, sequence) in desc.sequences.iter().enumerate() {
            write!(
                text,
                "$sequence {} {}",
                quote(&sequence.name)?,
                quote_path(&std::path::Path::new(".").join(animation))?
            )
            .unwrap();
            if let Some(stack) = &sequence.src_stack {
                write!(text, " srcstack {}", quote(stack)?).unwrap();
            }
            if let Some(fps) = sequence.fps {
                write!(text, " fps {fps}").unwrap();
            }
            if sequence.looping {
                text.push_str(" loop");
            }
            if let Some(fps) = sequence.src_fps {
                write!(text, " srcfps {fps}").unwrap();
            }
            if let Some(activity) = &sequence.activity {
                write!(
                    text,
                    " activity {} {}",
                    quote(activity)?,
                    sequence.activity_weight
                )
                .unwrap();
            }
            if sequence.events.is_empty() {
                text.push('\n');
            } else {
                let frame_count = compiled
                    .sequences
                    .get(sequence_index)
                    .map(|sequence| sequence.frames.len())
                    .ok_or_else(|| invalid("compiled sequence animation is missing"))?;
                text.push_str(" {\n");
                for event in &sequence.events {
                    let frame = (event.cycle * frame_count.saturating_sub(1) as f32).round() as u32;
                    let code = if event.name.is_empty() {
                        event.event.to_string()
                    } else {
                        event.name.clone()
                    };
                    writeln!(
                        text,
                        "    event {} {frame} {}",
                        quote(&code)?,
                        quote(&event.options)?
                    )
                    .unwrap();
                }
                text.push_str("}\n");
            }
        }
        text
    };
    if source_extension != "qc"
        && let Some(surface) = &desc.model.surface_prop
    {
        writeln!(qc, "$surfaceprop {}", quote(surface)?).unwrap();
    }
    if source_extension == "qc" {
        writeln!(qc, "$modelname {}", quote(&desc.model.name)?).unwrap();
    }
    if source_extension != "qc" || request.material_directory.is_some() {
        for directory in &desc.materials.search_paths {
            writeln!(qc, "$cdmaterials {}", quote(directory)?).unwrap();
        }
    }
    if !request.skins.is_empty() {
        qc.push_str(&skin_group(&compiled.desc)?);
    }
    if request.generate_collision {
        if !compiled.desc.model.static_prop && compiled.desc.bones.len() > 1 {
            return Err(invalid(
                "automatic collision generation requires a rigid prop or authored collision",
            ));
        }
        let relative = PathBuf::from(&assets).join("collision.smd");
        stage_asset(
            &files_root,
            &relative,
            collision_smd(&compiled, baked)?.as_bytes(),
            &mut writes,
        )?;
        writeln!(
            qc,
            "$scale 1\n$collisionmodel {}\n{{\n\t$mass {}\n}}",
            quote_path(&relative)?,
            compiled.desc.physics.effective_mass()
        )
        .unwrap();
    }
    if request.export_materials {
        let mut sources = vec![source.clone()];
        for reference in desc
            .bodyparts
            .iter()
            .flat_map(|body| &body.models)
            .flat_map(|model| {
                std::iter::once(Some(&model.smd))
                    .chain(model.lods.iter().map(|lod| lod.smd.as_ref()))
            })
            .flatten()
            .filter(|source| !source.is_empty())
        {
            sources.push(
                mdlc::compile::resolve_smd_path(base, reference)
                    .map_err(|error| ModelCompileError::Description(error.to_string()))?,
            );
        }
        let directories = &compiled.desc.materials.search_paths;
        let imported = compiled
            .desc
            .materials
            .textures
            .iter()
            .filter(|material| !variants.iter().any(|variant| variant.name == material.name))
            .map(|material| material.name.clone())
            .collect::<Vec<_>>();
        let mut prepared =
            model_materials::prepare_materials_for_sources(&imported, directories, base, &sources)?;
        model_materials::prepare_skin_materials(&mut prepared, &variants, directories, base)?;
        for replacement in request
            .skins
            .iter()
            .flat_map(|skin| skin.replacements.values())
        {
            if let SkinMaterial::Material { name } = replacement {
                model_materials::require_prepared_material(&prepared, name, directories)?;
            }
        }
        prepared.write_to(&files_root)?;
        for asset in prepared.assets {
            writes.push(asset.relative_path);
        }
    }
    stage_asset(&files_root, Path::new(filename), qc.as_bytes(), &mut writes)?;
    let candidate = files_root.join(filename);
    let parsed = mdlc::qc::parse_qc_file(&candidate).map_err(|errors| {
        ModelCompileError::Description(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    if source_extension == "qc" {
        portable_qc::validate_dependencies(&qc, &candidate, &parsed, &desc)?;
    }
    let validated = mdlc::pipeline::build(
        &parsed,
        &files_root,
        mdlc::pipeline::PipelineOptions::default(),
    )?;
    if let Some(bytes) = &validated.phy {
        let phy = vformats::phy::parse(bytes, &vformats::Limits::default()).map_err(|error| {
            ModelCompileError::Description(format!("collision output failed validation: {error}"))
        })?;
        if phy.header.mdl_checksum != validated.checksum as u32 {
            return Err(invalid(
                "collision checksum does not match the compiled model",
            ));
        }
    }
    let files = writes
        .into_iter()
        .map(|relative| ModelOutputFile {
            path: destination.join(&relative),
            bytes: 0,
            kind: ModelOutputKind::Material,
        })
        .collect::<Vec<_>>();
    let _export = model_compile::EXPORT_LOCK.lock();
    model_compile::publish(
        stage,
        &files_root,
        &destination,
        &files,
        &[],
        true,
        &AtomicBool::new(false),
    )
}

fn skin_group(desc: &mdlc::model::ModelDesc) -> Result<String, ModelCompileError> {
    let mut result = String::from("$texturegroup skinfamilies\n{\n");
    for family in &desc.materials.skin_families {
        result.push_str("\t{ ");
        for &index in family {
            let name = usize::try_from(index)
                .ok()
                .and_then(|index| desc.materials.textures.get(index))
                .ok_or_else(|| invalid("invalid skin material index"))?;
            write!(result, "{} ", quote(&name.name)?).unwrap();
        }
        result.push_str("}\n");
    }
    result.push_str("}\n");
    Ok(result)
}

fn static_mesh_smd(compiled: &mdlc::model::CompiledModelDesc) -> Result<String, ModelCompileError> {
    mesh_smd(
        compiled,
        compiled
            .bodyparts
            .iter()
            .flat_map(|body| &body.models)
            .flat_map(|model| &model.meshes),
    )
}

fn static_lod_smd(
    compiled: &mdlc::model::CompiledModelDesc,
    level: usize,
) -> Result<String, ModelCompileError> {
    let mut meshes = Vec::new();
    for model in compiled.bodyparts.iter().flat_map(|body| &body.models) {
        let lods = model
            .lods
            .as_ref()
            .ok_or_else(|| invalid("compiled LOD geometry is missing"))?;
        for (mesh, lod) in model.meshes.iter().zip(&lods.meshes) {
            let mut mesh = mesh.clone();
            mesh.vertices.clone_from(&lod.vertices);
            mesh.triangles.clone_from(
                lod.triangles
                    .get(level)
                    .ok_or_else(|| invalid("compiled LOD level is missing"))?,
            );
            meshes.push(mesh);
        }
    }
    mesh_smd(compiled, meshes.iter())
}

fn mesh_smd<'a>(
    compiled: &mdlc::model::CompiledModelDesc,
    meshes: impl Iterator<Item = &'a mdlc::model::Mesh>,
) -> Result<String, ModelCompileError> {
    let mut result = smd_header();
    for mesh in meshes {
        let name = &compiled.desc.materials.textures[mesh.material].name;
        smd_material(name)?;
        for triangle in &mesh.triangles {
            writeln!(result, "{name}").unwrap();
            for index in [triangle[0], triangle[2], triangle[1]] {
                let vertex = &mesh.vertices[index as usize];
                let p = inverse_source_rotation(vertex.pos);
                let n = inverse_source_rotation(vertex.normal);
                writeln!(
                    result,
                    "0 {} {} {} {} {} {} {} {} 1 0 1",
                    p[0],
                    p[1],
                    p[2],
                    n[0],
                    n[1],
                    n[2],
                    vertex.uv[0],
                    1.0 - vertex.uv[1]
                )
                .unwrap();
            }
        }
    }
    result.push_str("end\n");
    Ok(result)
}

fn source_smd(source: &mdlc::smd::Smd) -> Result<String, ModelCompileError> {
    let mut result = String::from("version 1\nnodes\n");
    for node in &source.nodes {
        writeln!(
            result,
            "{} {} {}",
            node.index,
            quote(&node.name)?,
            node.parent
        )
        .unwrap();
    }
    result.push_str("end\nskeleton\n");
    for frame in &source.frames {
        writeln!(result, "time {}", frame.time).unwrap();
        for pose in &frame.poses {
            writeln!(
                result,
                "{} {} {} {} {} {} {}",
                pose.bone,
                pose.position[0],
                pose.position[1],
                pose.position[2],
                pose.rotation[0],
                pose.rotation[1],
                pose.rotation[2]
            )
            .unwrap();
        }
    }
    result.push_str("end\ntriangles\n");
    for triangle in &source.triangles {
        smd_material(&triangle.material)?;
        writeln!(result, "{}", triangle.material).unwrap();
        for vertex in &triangle.vertices {
            write!(
                result,
                "{} {} {} {} {} {} {} {} {} {}",
                vertex.parent_bone,
                vertex.position[0],
                vertex.position[1],
                vertex.position[2],
                vertex.normal[0],
                vertex.normal[1],
                vertex.normal[2],
                vertex.uv[0],
                1.0 - vertex.uv[1],
                vertex.links.len()
            )
            .unwrap();
            for link in &vertex.links {
                write!(result, " {} {}", link.bone, link.weight).unwrap();
            }
            result.push('\n');
        }
    }
    result.push_str("end\n");
    Ok(result)
}

fn collision_smd(
    compiled: &mdlc::model::CompiledModelDesc,
    baked: bool,
) -> Result<String, ModelCompileError> {
    let hull = model_compile::collision_hull(compiled)?;
    // PHY compilation applies sequence_world * inverse(collision_rest). Matching
    // those poses keeps the generated hull in its corrected source coordinates.
    let mut result = if baked {
        smd_header()
    } else {
        let bone = compiled
            .desc
            .bones
            .first()
            .ok_or_else(|| invalid("collision requires a root bone"))?;
        let pose = compiled
            .sequences
            .first()
            .and_then(|sequence| sequence.frames.first())
            .and_then(|frame| frame.first());
        let position = pose.map_or_else(|| bone.position.unwrap_or([0.0; 3]), |pose| pose.position);
        let rotation = pose.map_or_else(
            || bone.rotation.unwrap_or([0.0; 3]).map(f32::to_radians),
            |pose| pose.rotation,
        );
        format!(
            "version 1\nnodes\n0 {} -1\nend\nskeleton\ntime 0\n0 {} {} {} {} {} {}\nend\ntriangles\n",
            quote(&bone.name)?,
            position[0],
            position[1],
            position[2],
            rotation[0],
            rotation[1],
            rotation[2]
        )
    };
    let material = compiled
        .desc
        .materials
        .textures
        .first()
        .ok_or_else(|| invalid("collision requires a model material"))?;
    smd_material(&material.name)?;
    for triangle in &hull.faces {
        writeln!(result, "{}", material.name).unwrap();
        for &index in triangle {
            let p = hull.vertices[index as usize];
            writeln!(result, "0 {} {} {} 0 0 1 0 0 1 0 1", p[0], p[1], p[2]).unwrap();
        }
    }
    result.push_str("end\n");
    Ok(result)
}

fn smd_header() -> String {
    String::from(
        "version 1\nnodes\n0 \"static_prop\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
    )
}

fn inverse_source_rotation(point: [f32; 3]) -> [f32; 3] {
    [point[1], -point[0], point[2]]
}

fn smd_material(name: &str) -> Result<(), ModelCompileError> {
    if name.is_empty() || name.contains(['\r', '\n', '\0']) {
        return Err(invalid("invalid SMD material name"));
    }
    Ok(())
}

struct SourceCopier<'a> {
    base: &'a Path,
    files_root: &'a Path,
    assets: &'a str,
    writes: &'a mut Vec<PathBuf>,
    copied: BTreeMap<PathBuf, PathBuf>,
    bytes: u64,
}

impl<'a> SourceCopier<'a> {
    fn new(
        base: &'a Path,
        files_root: &'a Path,
        assets: &'a str,
        writes: &'a mut Vec<PathBuf>,
    ) -> Self {
        Self {
            base,
            files_root,
            assets,
            writes,
            copied: BTreeMap::new(),
            bytes: 0,
        }
    }

    fn copy(&mut self, source: &Path) -> Result<PathBuf, ModelCompileError> {
        let relative = source
            .strip_prefix(self.base)
            .map_err(|_| ModelCompileError::UnsafePath(source.into()))?;
        let bytes = self.copy_file(relative, source)?;
        let extension = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        match extension.as_str() {
            "gltf" | "glb" => {
                let json = if extension == "glb" {
                    if bytes.len() < 20
                        || &bytes[..4] != b"glTF"
                        || &bytes[16..20] != b"JSON"
                        || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 2
                        || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize
                            != bytes.len()
                    {
                        return Err(invalid("invalid GLB JSON header"));
                    }
                    let size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
                    let end = 20_usize
                        .checked_add(size)
                        .ok_or_else(|| invalid("GLB JSON size overflows"))?;
                    bytes
                        .get(20..end)
                        .ok_or_else(|| invalid("GLB JSON exceeds its file"))?
                } else {
                    &bytes
                };
                let json: serde_json::Value = serde_json::from_slice(json)
                    .map_err(|error| ModelCompileError::Description(error.to_string()))?;
                for entries in ["buffers", "images"] {
                    if let Some(entries) = json[entries].as_array() {
                        for entry in entries {
                            if let Some(uri) = entry["uri"].as_str() {
                                if uri.starts_with("data:") {
                                    continue;
                                }
                                let relative = source
                                    .parent()
                                    .unwrap()
                                    .strip_prefix(self.base)
                                    .unwrap()
                                    .join(decode_source_uri(uri)?);
                                let path = self.find(&relative)?.ok_or_else(|| {
                                    invalid(&format!("source dependency was not found: {uri}"))
                                })?;
                                self.copy_file(&relative, &path)?;
                            }
                        }
                    }
                }
            }
            "fbx" => {
                let scene =
                    ufbx::load_memory(&bytes, ufbx::LoadOpts::default()).map_err(|error| {
                        ModelCompileError::Description(format!(
                            "cannot read FBX source dependencies: {error:?}"
                        ))
                    })?;
                for texture in &scene.textures {
                    if !texture.content.is_empty()
                        || texture
                            .video
                            .as_ref()
                            .is_some_and(|video| !video.content.is_empty())
                    {
                        continue;
                    }
                    let embedded = format!(
                        "{}.fbm",
                        source.file_stem().unwrap_or_default().to_string_lossy()
                    );
                    'texture: for name in [
                        texture.relative_filename.as_ref(),
                        texture.filename.as_ref(),
                    ] {
                        if name.is_empty() {
                            continue;
                        }
                        let name = name.replace('\\', "/");
                        let basename = name.rsplit('/').next().unwrap();
                        for candidate in [
                            name.clone(),
                            basename.into(),
                            format!("textures/{basename}"),
                            format!("{embedded}/{basename}"),
                            format!("{embedded}/{name}"),
                        ] {
                            let candidate_path = Path::new(&candidate);
                            if candidate_path.is_absolute() || candidate.contains(':') {
                                continue;
                            }
                            let relative = source
                                .parent()
                                .unwrap()
                                .strip_prefix(self.base)
                                .unwrap()
                                .join(candidate_path);
                            if let Some(path) = self.find(&relative)? {
                                self.copy_file(&relative, &path)?;
                                break 'texture;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(Path::new(self.assets).join(relative))
    }

    fn copy_file(&mut self, relative: &Path, source: &Path) -> Result<Vec<u8>, ModelCompileError> {
        let source = canonical(source)?;
        if !source.starts_with(self.base) || !source.is_file() {
            return Err(ModelCompileError::UnsafePath(source));
        }
        let length = fs::metadata(&source)
            .map_err(|error| io_error(&source, error))?
            .len();
        if length > 128 * 1024 * 1024 {
            return Err(invalid("source file exceeds the 128 MiB limit"));
        }
        let bytes = fs::read(&source).map_err(|error| io_error(&source, error))?;
        let relative = portable_qc::normalize_relative(relative)?;
        let target = Path::new(self.assets).join(relative);
        if let Some(previous) = self.copied.get(&target) {
            if previous != &source {
                return Err(invalid("conflicting source dependency paths"));
            }
            return Ok(bytes);
        }
        self.bytes += length;
        if self.bytes > 512 * 1024 * 1024 || self.copied.len() >= 4096 {
            return Err(invalid("source dependencies exceed the project size limit"));
        }
        stage_asset(self.files_root, &target, &bytes, self.writes)?;
        self.copied.insert(target, source);
        Ok(bytes)
    }

    fn find(&self, relative: &Path) -> Result<Option<PathBuf>, ModelCompileError> {
        let mut current = self.base.to_owned();
        for component in relative.components() {
            let name = match component {
                std::path::Component::CurDir => continue,
                std::path::Component::ParentDir => {
                    if current == self.base {
                        return Err(ModelCompileError::UnsafePath(relative.into()));
                    }
                    current.pop();
                    continue;
                }
                std::path::Component::Normal(name) => name,
                _ => return Err(ModelCompileError::UnsafePath(relative.into())),
            };
            let exact = current.join(name);
            if exact.exists() {
                current = exact;
            } else {
                if !current.is_dir() {
                    return Ok(None);
                }
                let mut matched = None;
                for entry in fs::read_dir(&current).map_err(|error| io_error(&current, error))? {
                    let entry = entry.map_err(|error| io_error(&current, error))?;
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&name.to_string_lossy())
                    {
                        if matched.is_some() {
                            return Err(invalid("ambiguous source dependency filename"));
                        }
                        matched = Some(entry.path());
                    }
                }
                let Some(found) = matched else {
                    return Ok(None);
                };
                current = found;
            }
            let real = canonical(&current)?;
            if !real.starts_with(self.base) {
                return Err(ModelCompileError::UnsafePath(current));
            }
        }
        Ok(current.is_file().then_some(current))
    }
}

fn decode_source_uri(uri: &str) -> Result<PathBuf, ModelCompileError> {
    let mut result = Vec::with_capacity(uri.len());
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let low = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(invalid("invalid percent escape in source URI"));
            };
            result.push((high * 16 + low) as u8);
        } else {
            result.push(byte);
        }
    }
    let path = String::from_utf8(result).map_err(|_| invalid("source URI is not UTF-8"))?;
    if path.is_empty() || path.contains([':', '\\', '\0']) {
        return Err(ModelCompileError::UnsafePath(path.into()));
    }
    Ok(path.into())
}

struct TextToken {
    quoted: bool,
    start: usize,
    end: usize,
    text: String,
}

fn text_tokens(text: &str) -> Result<Vec<TextToken>, ModelCompileError> {
    let bytes = text.as_bytes();
    let mut index = usize::from(text.starts_with('\u{feff}')) * 3;
    let mut result = Vec::new();
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if matches!(bytes[index], b';' | b'#') || bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index += 2;
            while index + 1 < bytes.len() && !bytes[index..].starts_with(b"*/") {
                index += 1;
            }
            if index + 1 >= bytes.len() {
                return Err(invalid("unterminated QC comment"));
            }
            index += 2;
            continue;
        }
        let start = index;
        let quoted = bytes[index] == b'"';
        if quoted {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                index += 1;
            }
            if index == bytes.len() {
                return Err(invalid("unterminated QC string"));
            }
            index += 1;
        } else if matches!(bytes[index], b'{' | b'}') {
            index += 1;
        } else {
            while index < bytes.len()
                && !bytes[index].is_ascii_whitespace()
                && !matches!(bytes[index], b'{' | b'}' | b';')
                && !bytes[index..].starts_with(b"//")
                && !bytes[index..].starts_with(b"/*")
            {
                index += 1;
            }
        }
        result.push(TextToken {
            quoted,
            start,
            end: index,
            text: text[start + usize::from(quoted)..index - usize::from(quoted)].to_owned(),
        });
    }
    Ok(result)
}

fn stage_asset(
    root: &Path,
    relative: &Path,
    bytes: &[u8],
    writes: &mut Vec<PathBuf>,
) -> Result<(), ModelCompileError> {
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ModelCompileError::UnsafePath(relative.to_path_buf()));
    }
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("staged asset has parent"))
        .map_err(|error| io_error(&path, error))?;
    fs::write(&path, bytes).map_err(|error| io_error(&path, error))?;
    writes.push(relative.to_path_buf());
    Ok(())
}

fn asset_namespace(path: &Path) -> String {
    let mut namespace = String::new();
    for &byte in path.file_stem().unwrap_or_default().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') {
            namespace.push(char::from(byte));
        } else {
            write!(namespace, "~{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    namespace.push_str("_sources");
    namespace
}

fn quote_path(path: &Path) -> Result<String, ModelCompileError> {
    quote(
        &path
            .to_str()
            .ok_or_else(|| invalid("QC paths must be UTF-8"))?
            .replace('\\', "/"),
    )
}

fn quote(text: &str) -> Result<String, ModelCompileError> {
    if text.contains(['"', '\r', '\n', '\0']) {
        return Err(invalid("QC values cannot contain quotes or newlines"));
    }
    Ok(format!("\"{text}\""))
}

fn canonical(path: &Path) -> Result<PathBuf, ModelCompileError> {
    fs::canonicalize(path).map_err(|error| io_error(path, error))
}

fn io_error(path: &Path, source: std::io::Error) -> ModelCompileError {
    ModelCompileError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn invalid(message: &str) -> ModelCompileError {
    ModelCompileError::InvalidOption(message.into())
}

#[cfg(test)]
#[path = "model_project/tests.rs"]
mod tests;
