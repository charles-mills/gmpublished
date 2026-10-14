use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mdlc::model::ModelDesc;
use mdlc::pipeline::{self, PipelineOptions};
use parking_lot::Mutex;

use crate::model_project::{ModelSkin, SkinMaterial};
use crate::{ErrorKey, HasErrorKey, error_keys, model_materials};

#[path = "model_compile/authoring.rs"]
pub(crate) mod authoring;
#[path = "model_compile/bodygroups.rs"]
mod bodygroups;
#[path = "model_compile/collision.rs"]
mod collision;
pub use bodygroups::{BodygroupChoice, ModelBodygroup, validate_bodygroups};
pub(crate) use collision::collision_hulls;
pub use collision::{CollisionDetail, CollisionPhysics, ModelCollision, collision_parts};
#[path = "model_compile/lods.rs"]
mod lods;
mod surfaces;
pub use lods::{ModelLod, validate_lods};
pub use mdlc::fbx::SourcePart;
pub use mdlc::model::SurfacePhysics;
pub use surfaces::load_surface_properties;

pub(crate) static EXPORT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "compile options are independent toggles"
)]
pub struct ModelCompileRequest {
    pub surface_properties: Vec<SurfacePhysics>,
    pub qc_text: Option<String>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub model_name: Option<String>,
    pub material_directory: Option<String>,
    pub static_prop: bool,
    pub static_prop_override: Option<bool>,
    pub scale: f32,
    pub overwrite: bool,
    pub export_materials: bool,
    pub automatic_axes: bool,
    pub orientation: [f32; 3],
    pub skins: Vec<ModelSkin>,
    pub bodygroups: Vec<ModelBodygroup>,
    pub lods: Vec<ModelLod>,
    pub animation_settings: Vec<AnimationSettings>,
    pub collision: ModelCollision,
    pub collision_mass: f32,
    pub surface_property: Option<String>,
}

impl ModelCompileRequest {
    #[must_use]
    pub fn new(source: PathBuf, destination: PathBuf) -> Self {
        Self {
            surface_properties: Vec::new(),
            qc_text: None,
            source,
            destination,
            model_name: None,
            material_directory: None,
            static_prop: false,
            static_prop_override: None,
            scale: 1.0,
            overwrite: false,
            export_materials: true,
            automatic_axes: true,
            orientation: [0.0; 3],
            skins: Vec::new(),
            bodygroups: Vec::new(),
            lods: Vec::new(),
            animation_settings: Vec::new(),
            collision: ModelCollision::Existing,
            collision_mass: 1.0,
            surface_property: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelOutputKind {
    Mdl,
    Vvd,
    Vtx,
    Phy,
    Ani,
    Material,
    Texture,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOutputFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub kind: ModelOutputKind,
}

#[derive(Clone, Debug)]
pub struct ModelCompileOutput {
    pub model_name: String,
    pub checksum: i32,
    pub files: Vec<ModelOutputFile>,
    pub materials: Vec<String>,
    pub material_directories: Vec<String>,
    pub warnings: Vec<String>,
    pub bones: usize,
    pub vertices: usize,
    pub triangles: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelCompileError {
    #[error("unsupported model source {0}; use QC, TOML, SMD, DMX, FBX, glTF or GLB")]
    UnsupportedSource(PathBuf),
    #[error("invalid compile option: {0}")]
    InvalidOption(String),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Description(String),
    #[error(transparent)]
    Pipeline(#[from] mdlc::pipeline::PipelineError),
    #[error(transparent)]
    Materials(#[from] model_materials::MaterialExportError),
    #[error("export already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("unsafe export path: {0}")]
    UnsafePath(PathBuf),
    #[error("model compilation cancelled")]
    Cancelled,
    #[error(
        "cannot publish {path}: {source}; recovery directory: {recovery_directory:?}; rollback errors: {rollback_errors:?}"
    )]
    Publish {
        path: PathBuf,
        #[source]
        source: std::io::Error,
        recovery_directory: Option<PathBuf>,
        rollback_errors: Vec<String>,
    },
}

impl HasErrorKey for ModelCompileError {
    fn error_key(&self) -> ErrorKey {
        if matches!(self, Self::Cancelled) {
            error_keys::CANCELLED
        } else {
            error_keys::MODEL_COMPILE
        }
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub fn compile_model(
    request: &ModelCompileRequest,
) -> Result<ModelCompileOutput, ModelCompileError> {
    compile_model_with_cancel(request, &AtomicBool::new(false))
}

/// Runs on a worker thread. Cancellation is honored until publication begins;
/// publication then completes or rolls back all model and material files.
pub fn compile_model_with_cancel(
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<ModelCompileOutput, ModelCompileError> {
    let (result, warnings) = mdlc::diag::capture_warnings(|| {
        mdlc::fbx::with_scene_cache(|| compile_model_inner(request, cancel))
    });
    result.map(|mut output| {
        for warning in warnings {
            if !output.warnings.contains(&warning) {
                output.warnings.push(warning);
            }
        }
        output
    })
}

fn compile_model_inner(
    request: &ModelCompileRequest,
    cancel: &AtomicBool,
) -> Result<ModelCompileOutput, ModelCompileError> {
    crate::util::main_thread_forbidden!();
    check_cancel(cancel)?;
    let source = fs::canonicalize(&request.source).map_err(|source| ModelCompileError::Io {
        path: request.source.clone(),
        source,
    })?;
    let base = source
        .parent()
        .ok_or_else(|| ModelCompileError::InvalidOption("source must be a file".into()))?;
    let mut desc = load_description(request, &source, base)?;
    let _authoring_assets = authoring::prepare_assets(&mut desc, &source)?;
    desc.model.version = Some(48);
    if let Some(name) = &request.model_name {
        desc.model.name = normalize_model_name(name)?;
    } else {
        desc.model.name = normalize_model_name(&desc.model.name)?;
    }
    if let Some(directory) = &request.material_directory {
        desc.materials.search_paths = vec![directory.replace('\\', "/")];
    }
    check_cancel(cancel)?;
    if request.orientation.iter().any(|value| !value.is_finite()) {
        return Err(ModelCompileError::InvalidOption(
            "orientation angles must be finite".into(),
        ));
    }
    if !request.export_materials
        && request
            .skins
            .iter()
            .flat_map(|skin| skin.replacements.values())
            .any(|material| !matches!(material, SkinMaterial::Material { .. }))
    {
        return Err(ModelCompileError::InvalidOption(
            "authored skin textures and tints require material export".into(),
        ));
    }
    let mut compiled =
        mdlc::compile::compile(&desc, base).map_err(mdlc::pipeline::PipelineError::Compile)?;
    authoring::restore_animation_labels(&mut compiled, &source)?;
    apply_orientation(&mut compiled, &source, request.orientation)?;
    let variants = apply_skins(&mut compiled.desc, &request.skins)?;
    let mut output = pipeline::build_compiled(compiled, base, PipelineOptions::default())?;
    collision::apply(&mut output, request, cancel)?;
    if let Some(bytes) = &output.phy {
        let phy = vformats::phy::parse(bytes, &vformats::Limits::default()).map_err(|error| {
            ModelCompileError::Description(format!("collision output failed validation: {error}"))
        })?;
        if phy.header.mdl_checksum != output.checksum as u32 {
            return Err(ModelCompileError::Description(
                "collision checksum does not match the compiled model".into(),
            ));
        }
    }
    check_cancel(cancel)?;
    let materials = output
        .compiled
        .desc
        .materials
        .textures
        .iter()
        .map(|texture| texture.name.clone())
        .collect::<Vec<_>>();
    let material_directories = output.compiled.desc.materials.search_paths.clone();
    let prepared = if request.export_materials {
        let mut sources = vec![source.clone()];
        for model in desc.bodyparts.iter().flat_map(|body| &body.models) {
            for reference in std::iter::once(Some(&model.smd))
                .chain(model.lods.iter().map(|lod| lod.smd.as_ref()))
                .flatten()
            {
                if reference.is_empty() {
                    continue;
                }
                let path = mdlc::compile::resolve_smd_path(base, reference)
                    .map_err(|error| ModelCompileError::Description(error.to_string()))?;
                if !sources.contains(&path) {
                    sources.push(path);
                }
            }
        }
        let imported_materials = materials
            .iter()
            .filter(|name| !variants.iter().any(|variant| variant.name == **name))
            .cloned()
            .collect::<Vec<_>>();
        let mut prepared = model_materials::prepare_materials_for_sources(
            &imported_materials,
            &material_directories,
            base,
            &sources,
        )?;
        model_materials::prepare_skin_materials(
            &mut prepared,
            &variants,
            &material_directories,
            base,
        )?;
        for skin in &request.skins {
            for replacement in skin.replacements.values() {
                if let SkinMaterial::Material { name } = replacement {
                    model_materials::require_prepared_material(
                        &prepared,
                        name,
                        &material_directories,
                    )?;
                }
            }
        }
        Some(prepared)
    } else {
        None
    };
    check_cancel(cancel)?;
    let _export = EXPORT_LOCK.lock();
    check_cancel(cancel)?;
    fs::create_dir_all(&request.destination).map_err(|source| ModelCompileError::Io {
        path: request.destination.clone(),
        source,
    })?;
    let destination =
        fs::canonicalize(&request.destination).map_err(|source| ModelCompileError::Io {
            path: request.destination.clone(),
            source,
        })?;
    let stage = tempfile::Builder::new()
        .prefix(".gmpublished-model-")
        .tempdir_in(&destination)
        .map_err(|source| ModelCompileError::Io {
            path: destination.clone(),
            source,
        })?;
    let files_root = stage.path().join("files");
    let paths = pipeline::write_files(&output, &files_root)?;
    let mut staged = vec![
        (paths.mdl, ModelOutputKind::Mdl),
        (paths.vvd, ModelOutputKind::Vvd),
        (paths.vtx, ModelOutputKind::Vtx),
    ];
    if let Some(path) = paths.phy {
        staged.push((path, ModelOutputKind::Phy));
    }
    if let Some(path) = paths.ani {
        staged.push((path, ModelOutputKind::Ani));
    }
    let mut warnings = Vec::new();
    if let Some(prepared) = prepared {
        for asset in &prepared.assets {
            validate_relative_path(&asset.relative_path)?;
        }
        prepared.write_to(&files_root)?;
        for asset in prepared.assets {
            let kind = if asset
                .relative_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vmt"))
            {
                ModelOutputKind::Material
            } else {
                ModelOutputKind::Texture
            };
            staged.push((files_root.join(asset.relative_path), kind));
        }
        warnings = prepared.warnings;
    }
    let mut files = Vec::with_capacity(staged.len());
    for (path, kind) in &staged {
        let relative = path
            .strip_prefix(&files_root)
            .map_err(|_| ModelCompileError::UnsafePath(path.clone()))?;
        validate_relative_path(relative)?;
        let bytes = fs::metadata(path)
            .map_err(|source| ModelCompileError::Io {
                path: path.clone(),
                source,
            })?
            .len();
        files.push(ModelOutputFile {
            path: destination.join(relative),
            bytes,
            kind: *kind,
        });
    }
    let model_path = files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .expect("model output exists")
        .path
        .clone();
    let mut obsolete = Vec::new();
    for (extension, present) in [("phy", output.phy.is_some()), ("ani", output.ani.is_some())] {
        if !present {
            obsolete.push(model_path.with_extension(extension));
        }
    }
    check_cancel(cancel)?;
    publish(
        stage,
        &files_root,
        &destination,
        &files,
        &obsolete,
        request.overwrite,
        cancel,
    )?;
    Ok(ModelCompileOutput {
        model_name: desc.model.name,
        checksum: output.checksum,
        files,
        materials,
        material_directories,
        warnings,
        bones: output.compiled.desc.bones.len(),
        vertices: output.compiled.total_vertices(),
        triangles: output.compiled.total_triangles(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationEvent {
    pub frame: u32,
    pub event: String,
    pub options: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnimationSettings {
    pub source: Option<String>,
    pub included: bool,
    pub name: String,
    pub fps: f32,
    pub looping: bool,
    pub frame_count: usize,
    pub activity: Option<String>,
    pub activity_weight: i32,
    pub events: Vec<AnimationEvent>,
}

pub fn validate_animation_settings(
    settings: &[AnimationSettings],
) -> Result<(), ModelCompileError> {
    if settings.is_empty() {
        return Ok(());
    }
    if !settings.iter().any(|setting| setting.included) {
        return Err(ModelCompileError::InvalidOption(
            "include at least one animation".into(),
        ));
    }
    let mut names = std::collections::HashSet::new();
    let mut sources = std::collections::HashSet::new();
    for setting in settings {
        if !sources.insert(&setting.source) {
            return Err(ModelCompileError::InvalidOption(
                "duplicate animation source".into(),
            ));
        }
        if !setting.included {
            continue;
        }
        if setting.name.is_empty()
            || setting.name.len() > 63
            || !setting
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || !names.insert(setting.name.to_ascii_lowercase())
        {
            return Err(ModelCompileError::InvalidOption("animation names must be unique and use 1–63 ASCII letters, numbers, underscores, hyphens or dots".into()));
        }
        if let Some(activity) = &setting.activity {
            if !activity.starts_with("ACT_")
                || activity.len() <= 4
                || activity.len() > 63
                || !activity
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(ModelCompileError::InvalidOption(
                    "activities must use an ACT_ identifier of at most 63 ASCII characters".into(),
                ));
            }
        } else if setting.activity_weight != 0 {
            return Err(ModelCompileError::InvalidOption(
                "an activity weight requires an activity".into(),
            ));
        }
        if setting.events.len() > 1024 {
            return Err(ModelCompileError::InvalidOption(
                "an animation supports at most 1024 events".into(),
            ));
        }
        for event in &setting.events {
            let numeric = event.event.bytes().all(|byte| byte.is_ascii_digit());
            let valid_name = !event.event.is_empty()
                && event.event.len() <= 63
                && if numeric {
                    event.event.parse::<i32>().is_ok()
                } else {
                    event
                        .event
                        .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && event
                            .event
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                };
            if !valid_name {
                return Err(ModelCompileError::InvalidOption("event codes must be nonnegative integers or ASCII event identifiers of at most 63 characters".into()));
            }
            if event.options.len() > 63
                || event.options.contains(['"', '\r', '\n', '\0'])
                || event.options.starts_with('}')
            {
                return Err(ModelCompileError::InvalidOption("event options must fit 63 UTF-8 bytes, contain no quotes or line breaks, and cannot begin with a closing brace".into()));
            }
            if event.frame > i32::MAX as u32
                || (setting.frame_count > 0 && event.frame as usize >= setting.frame_count)
            {
                return Err(ModelCompileError::InvalidOption(
                    "event frames must lie within the sampled animation".into(),
                ));
            }
        }
        if !setting.fps.is_finite() || setting.fps <= 0.0 || setting.fps > 1000.0 {
            return Err(ModelCompileError::InvalidOption(
                "animation FPS must be greater than zero and at most 1000".into(),
            ));
        }
    }
    Ok(())
}

fn apply_animation_settings(
    desc: &mut ModelDesc,
    settings: &[AnimationSettings],
    base: &Path,
) -> Result<(), ModelCompileError> {
    validate_animation_settings(settings)?;
    if settings.is_empty() {
        return Ok(());
    }
    if settings.len() != desc.sequences.len()
        || settings.iter().any(|setting| {
            !desc
                .sequences
                .iter()
                .any(|sequence| sequence.src_stack == setting.source)
        })
    {
        return Err(ModelCompileError::InvalidOption(
            "source animations changed; import the source again before exporting".into(),
        ));
    }
    desc.sequences.retain(|sequence| {
        settings
            .iter()
            .find(|setting| setting.source == sequence.src_stack)
            .unwrap()
            .included
    });
    for sequence in &mut desc.sequences {
        let setting = settings
            .iter()
            .find(|setting| setting.source == sequence.src_stack)
            .unwrap();
        let count = if setting.frame_count > 0 || !setting.events.is_empty() {
            let count = sampled_animation_frames(sequence, base)?;
            if setting.frame_count != 0 && setting.frame_count != count {
                return Err(ModelCompileError::InvalidOption(
                    "source animation length changed; import the source again before exporting"
                        .into(),
                ));
            }
            if setting
                .events
                .iter()
                .any(|event| event.frame as usize >= count)
            {
                return Err(ModelCompileError::InvalidOption(
                    "event frames must lie within the sampled animation".into(),
                ));
            }
            count
        } else {
            1
        };
        sequence.name.clone_from(&setting.name);
        sequence.fps = Some(setting.fps);
        sequence.looping = setting.looping;
        sequence.activity.clone_from(&setting.activity);
        sequence.activity_weight = setting.activity_weight;
        sequence.events = setting
            .events
            .iter()
            .map(|event| {
                let numeric = event.event.parse::<i32>().ok();
                mdlc::model::SequenceEvent {
                    cycle: event.frame as f32 / (count.saturating_sub(1).max(1)) as f32,
                    event_type: if numeric.is_some() { 0 } else { 1024 },
                    event: numeric.unwrap_or(0),
                    name: if numeric.is_some() {
                        String::new()
                    } else {
                        event.event.clone()
                    },
                    options: event.options.clone(),
                }
            })
            .collect();
    }
    Ok(())
}

fn sampled_animation_frames(
    sequence: &mdlc::model::Sequence,
    base: &Path,
) -> Result<usize, ModelCompileError> {
    let opts = mdlc::compile::SrcOpts::default()
        .with_anim(sequence.src_stack.as_deref(), sequence.src_fps);
    let animation =
        mdlc::compile::read_source_frames(&base.join(&sequence.smd), &sequence.name, &opts)
            .map_err(|error| ModelCompileError::Description(error.to_string()))?;
    if animation.frames.is_empty() {
        return Err(ModelCompileError::InvalidOption(
            "animation contains no sampled frames".into(),
        ));
    }
    Ok(animation.frames.len())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedModelSettings {
    pub model_name: String,
    pub static_prop: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInspection {
    pub source_model: Option<ImportedModelSettings>,
    pub collision_physics: Option<CollisionPhysics>,
    pub collision_parts: Vec<String>,
    pub parts: Vec<mdlc::fbx::SourcePart>,
    pub animations: Vec<AnimationSettings>,
    pub materials: Vec<String>,
    pub material_directories: Vec<String>,
    pub skins: Vec<Vec<String>>,
}

pub fn read_qc_text(source: &Path) -> Result<String, ModelCompileError> {
    crate::util::main_thread_forbidden!();
    let length = fs::metadata(source)
        .map_err(|error| ModelCompileError::Io {
            path: source.to_path_buf(),
            source: error,
        })?
        .len();
    if length > 8 * 1024 * 1024 {
        return Err(ModelCompileError::InvalidOption(
            "QC text exceeds the 8 MiB editor limit".into(),
        ));
    }
    fs::read_to_string(source).map_err(|error| ModelCompileError::Io {
        path: source.to_path_buf(),
        source: error,
    })
}

pub fn inspect_qc_text(source: &Path, text: &str) -> Result<ModelInspection, ModelCompileError> {
    if !source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("qc"))
    {
        return Err(ModelCompileError::InvalidOption(
            "edited QC text requires a QC source".into(),
        ));
    }
    inspect_source(source, Some(text))
}

pub fn inspect_model(source: &Path) -> Result<ModelInspection, ModelCompileError> {
    inspect_source(source, None)
}

fn inspect_source(
    source: &Path,
    qc_text: Option<&str>,
) -> Result<ModelInspection, ModelCompileError> {
    mdlc::fbx::with_scene_cache(|| inspect_source_inner(source, qc_text))
}

fn inspect_source_inner(
    source: &Path,
    qc_text: Option<&str>,
) -> Result<ModelInspection, ModelCompileError> {
    crate::util::main_thread_forbidden!();
    let source = fs::canonicalize(source).map_err(|error| ModelCompileError::Io {
        path: source.to_path_buf(),
        source: error,
    })?;
    let base = source.parent().expect("canonical source has parent");
    if source
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case("mdl"))
    {
        let mut bytes = fs::read(&source).map_err(|error| ModelCompileError::Io {
            path: source.clone(),
            source: error,
        })?;
        if let Some(count) = bytes.get_mut(180..184) {
            count.fill(0);
        }
        let model = vformats::mdl::parse_mdl(&bytes, &vformats::Limits::default())
            .map_err(|error| ModelCompileError::Description(error.to_string()))?;
        let materials = model.textures.clone();
        let skins = if model.skin_reference_count == 0 {
            vec![materials.clone()]
        } else {
            model
                .skin_table
                .chunks(model.skin_reference_count)
                .map(|family| {
                    family
                        .iter()
                        .map(|&index| {
                            materials.get(usize::from(index)).cloned().ok_or_else(|| {
                                ModelCompileError::Description("invalid model skin table".into())
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        return Ok(ModelInspection {
            source_model: None,
            collision_physics: None,
            collision_parts: Vec::new(),
            parts: Vec::new(),
            animations: Vec::new(),
            materials,
            material_directories: model.texture_paths,
            skins,
        });
    }
    let mut request = ModelCompileRequest::new(source.clone(), PathBuf::new());
    request.qc_text = qc_text.map(str::to_owned);
    let desc = load_description(&request, &source, base)?;
    let materials = desc
        .materials
        .textures
        .iter()
        .map(|texture| {
            let normalized = texture.name.replace('\\', "/");
            desc.model
                .material_renames
                .iter()
                .find(|[old, _]| old.replace('\\', "/").eq_ignore_ascii_case(&normalized))
                .map_or_else(|| texture.name.clone(), |[_, new]| new.replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    let families = if desc.materials.skin_families.is_empty() {
        vec![(0..materials.len()).map(|index| index as i32).collect()]
    } else {
        desc.materials.skin_families.clone()
    };
    let mut skins = Vec::new();
    for family in families {
        let mut names = Vec::new();
        for index in family {
            let name = usize::try_from(index)
                .ok()
                .and_then(|index| materials.get(index))
                .ok_or_else(|| {
                    ModelCompileError::InvalidOption("invalid source skin material index".into())
                })?;
            names.push(name.clone());
        }
        skins.push(names);
    }
    let extension = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let source_model = authoring::model_settings(&source)?;
    let animations = if source_model.is_some() {
        authoring::inspection_animations(&desc, &source)?
    } else {
        desc.sequences
            .iter()
            .map(|sequence| {
                Ok(AnimationSettings {
                    source: sequence.src_stack.clone(),
                    included: true,
                    name: sequence.name.clone(),
                    fps: sequence.fps.unwrap_or(30.0),
                    looping: sequence.looping,
                    frame_count: if matches!(
                        extension.as_str(),
                        "smd" | "dmx" | "fbx" | "gltf" | "glb"
                    ) {
                        sampled_animation_frames(sequence, base)?
                    } else {
                        0
                    },
                    activity: sequence.activity.clone(),
                    activity_weight: sequence.activity_weight,
                    events: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, ModelCompileError>>()?
    };
    Ok(ModelInspection {
        source_model,
        collision_physics: collision::collision_physics(&source)?,
        collision_parts: collision_parts(&source)?,
        parts: bodygroups::source_parts(&source)?,
        animations,
        materials,
        material_directories: desc.materials.search_paths,
        skins,
    })
}

pub(crate) fn apply_skins(
    desc: &mut ModelDesc,
    skins: &[ModelSkin],
) -> Result<Vec<model_materials::SkinVariant>, ModelCompileError> {
    if skins.is_empty() {
        return Ok(Vec::new());
    }
    let original = desc.materials.textures.clone();
    let count = original.len();
    let mut families = if desc.materials.skin_families.is_empty() {
        vec![(0..count).map(|index| index as i32).collect::<Vec<_>>()]
    } else {
        desc.materials.skin_families.clone()
    };
    if families.iter().any(|family| family.len() != count) {
        return Err(ModelCompileError::InvalidOption(
            "source skin families must cover every material slot".into(),
        ));
    }
    let base = families[0].clone();
    let mut names = std::collections::BTreeSet::new();
    let mut variants = Vec::new();
    for (index, skin) in skins.iter().enumerate() {
        if skin.name.trim().is_empty() || !names.insert(skin.name.trim().to_lowercase()) {
            return Err(ModelCompileError::InvalidOption(
                "skin names must be nonempty and unique".into(),
            ));
        }
        let mut row = base.clone();
        for (material, replacement) in &skin.replacements {
            let slots = base
                .iter()
                .enumerate()
                .filter_map(|(slot, &texture)| {
                    original
                        .get(texture as usize)
                        .filter(|texture| texture.name.eq_ignore_ascii_case(material))
                        .map(|_| slot)
                })
                .collect::<Vec<_>>();
            if slots.is_empty() {
                return Err(ModelCompileError::InvalidOption(format!(
                    "skin '{}' references missing default material '{material}'; reconcile the material after reimport",
                    skin.name
                )));
            }
            let name = match replacement {
                SkinMaterial::Material { name } => {
                    model_materials::validate_material_name(name)?;
                    name.replace('\\', "/")
                }
                SkinMaterial::Tint { .. } | SkinMaterial::Texture { .. } => {
                    let name = format!("__skins/skin_{}/{material}", index + 1);
                    model_materials::validate_material_name(&name)?;
                    if desc
                        .materials
                        .textures
                        .iter()
                        .any(|texture| texture.name.eq_ignore_ascii_case(&name))
                    {
                        return Err(ModelCompileError::InvalidOption(format!(
                            "generated skin material conflicts with '{name}'"
                        )));
                    }
                    variants.push(model_materials::SkinVariant {
                        name: name.clone(),
                        base: material.clone(),
                        replacement: replacement.clone(),
                    });
                    name
                }
            };
            let texture = if let Some(index) = desc
                .materials
                .textures
                .iter()
                .position(|texture| texture.name.eq_ignore_ascii_case(&name))
            {
                index
            } else {
                let texture = desc.materials.textures.len();
                desc.materials
                    .textures
                    .push(mdlc::model::Texture { name, flags: None });
                texture
            };
            for slot in slots {
                row[slot] = texture as i32;
            }
        }
        families.push(row);
    }
    let total = desc.materials.textures.len();
    for family in &mut families {
        family.extend((family.len()..total).map(|index| index as i32));
    }
    desc.materials.skin_families = families;
    Ok(variants)
}

pub(crate) fn apply_orientation(
    compiled: &mut mdlc::model::CompiledModelDesc,
    source: &Path,
    angles: [f32; 3],
) -> Result<(), ModelCompileError> {
    if angles == [0.0; 3] {
        return Ok(());
    }
    let direct = source.extension().is_some_and(|extension| {
        ["smd", "dmx", "fbx", "gltf", "glb"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
    });
    if !direct || !compiled.desc.model.static_prop || compiled.desc.physics.smd.is_some() {
        return Err(ModelCompileError::InvalidOption("export orientation currently requires a direct mesh compiled as a static prop; rigged models and QC/TOML descriptions must define orientation in their source".into()));
    }
    let matrix = mdlc::bone_math::angle_matrix(angles.map(f32::to_radians));
    let rotate = |vector: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|row| {
            (0..3)
                .map(|column| matrix[row * 4 + column] * vector[column])
                .sum()
        })
    };
    for hitbox in &mut compiled.desc.hitboxes.boxes {
        let mut minimum = [f32::INFINITY; 3];
        let mut maximum = [f32::NEG_INFINITY; 3];
        for corner in 0..8 {
            let point = std::array::from_fn(|axis| {
                if corner & (1 << axis) == 0 {
                    hitbox.bbmin[axis]
                } else {
                    hitbox.bbmax[axis]
                }
            });
            let point = rotate(point);
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(point[axis]);
                maximum[axis] = maximum[axis].max(point[axis]);
            }
        }
        hitbox.bbmin = minimum;
        hitbox.bbmax = maximum;
    }
    for model in compiled
        .bodyparts
        .iter_mut()
        .flat_map(|body| &mut body.models)
    {
        for vertex in model.meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
            vertex.pos = rotate(vertex.pos);
            vertex.normal = rotate(vertex.normal);
        }
        if let Some(lods) = &mut model.lods {
            for vertex in lods.meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
                vertex.pos = rotate(vertex.pos);
                vertex.normal = rotate(vertex.normal);
            }
        }
        for vertex in model
            .mesh_flexes
            .iter_mut()
            .flatten()
            .flat_map(|flex| &mut flex.vertanims)
        {
            vertex.delta = rotate(vertex.delta);
            vertex.ndelta = rotate(vertex.ndelta);
        }
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), ModelCompileError> {
    if cancel.load(Ordering::Acquire) {
        Err(ModelCompileError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn load_description(
    request: &ModelCompileRequest,
    source: &Path,
    base: &Path,
) -> Result<ModelDesc, ModelCompileError> {
    validate_lods(&request.lods, !request.bodygroups.is_empty())?;
    if !request.animation_settings.is_empty()
        && (request.static_prop
            || source.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("qc") || extension.eq_ignore_ascii_case("toml")
            }))
    {
        return Err(ModelCompileError::InvalidOption("animation settings apply to non-static mesh imports; authored QC defines its own sequences".into()));
    }
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Some(text) = &request.qc_text {
        if extension != "qc" {
            return Err(ModelCompileError::InvalidOption(
                "edited QC text requires a QC source".into(),
            ));
        }
        if text.len() > 8 * 1024 * 1024 || text.contains('\0') {
            return Err(ModelCompileError::InvalidOption(
                "QC text must contain no NUL bytes and be at most 8 MiB".into(),
            ));
        }
    }
    if !request.scale.is_finite() || request.scale <= 0.0 {
        return Err(ModelCompileError::InvalidOption(
            "scale must be finite and positive".into(),
        ));
    }
    if matches!(
        request.collision,
        ModelCollision::Automatic(_) | ModelCollision::Custom { .. }
    ) && (!request.collision_mass.is_finite() || request.collision_mass <= 0.0)
    {
        return Err(ModelCompileError::InvalidOption(
            "collision mass must be finite and positive".into(),
        ));
    }
    if request.surface_property.as_ref().is_some_and(|property| {
        property.is_empty()
            || !property
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) {
        return Err(ModelCompileError::InvalidOption(
            "surface property must contain only ASCII letters, numbers, underscores or hyphens"
                .into(),
        ));
    }
    let result = match extension.as_str() {
        "qc" | "toml" => {
            if !request.lods.is_empty() {
                return Err(ModelCompileError::InvalidOption(
                    "authored QC defines its own LODs".into(),
                ));
            }
            if !request.bodygroups.is_empty() {
                return Err(ModelCompileError::InvalidOption(
                    "authored QC defines its own bodygroups".into(),
                ));
            }
            if matches!(
                request.collision,
                ModelCollision::Automatic(_) | ModelCollision::Custom { .. }
            ) || request.surface_property.is_some()
            {
                return Err(ModelCompileError::InvalidOption(
                    "set collision and surface properties inside the QC or TOML description".into(),
                ));
            }
            if request.scale != 1.0 {
                return Err(ModelCompileError::InvalidOption(
                    "set scale inside the QC or TOML description".into(),
                ));
            }
            if extension == "qc" {
                request
                    .qc_text
                    .as_deref()
                    .map_or_else(
                        || mdlc::qc::parse_qc_file(source),
                        |text| mdlc::qc::parse_qc_text(text, source),
                    )
                    .map_err(|errors| {
                        ModelCompileError::Description(
                            errors
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    })
            } else {
                let text =
                    fs::read_to_string(source).map_err(|source_error| ModelCompileError::Io {
                        path: source.to_path_buf(),
                        source: source_error,
                    })?;
                ModelDesc::from_toml(&text)
                    .map_err(|error| ModelCompileError::Description(error.to_string()))
            }
        }
        "smd" | "dmx" | "fbx" | "gltf" | "glb" => {
            if let Some(description) = authoring::restore(request, source)? {
                return Ok(description);
            }
            let filename = source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    ModelCompileError::InvalidOption("mesh filename must be UTF-8".into())
                })?;
            if filename.contains(['"', '\n', '\r']) {
                return Err(ModelCompileError::InvalidOption(
                    "mesh filename cannot contain quotes or newlines".into(),
                ));
            }
            let stem = source
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("model");
            let name = request
                .model_name
                .clone()
                .unwrap_or_else(|| format!("models/{stem}.mdl"));
            let name = normalize_model_name(&name)?;
            if name.contains('"') {
                return Err(ModelCompileError::InvalidOption(
                    "model name cannot contain quotes".into(),
                ));
            }
            let static_prop = if request.static_prop {
                "$staticprop\n"
            } else {
                ""
            };
            let axes = if request.automatic_axes && (extension != "smd" || !request.lods.is_empty())
            {
                " srcaxis auto"
            } else {
                ""
            };
            let actions = if request.static_prop {
                Vec::new()
            } else {
                match extension.as_str() {
                    "dmx" => mdlc::dmx::animation_names(source)
                        .map_err(ModelCompileError::Description)?,
                    "fbx" => mdlc::fbx::animation_names(source, filename)
                        .map_err(|error| ModelCompileError::Description(error.to_string()))?,
                    "gltf" | "glb" => mdlc::gltf::animation_names(source, filename)
                        .map_err(|error| ModelCompileError::Description(error.to_string()))?,
                    _ => Vec::new(),
                }
            };
            let sequence_names = imported_sequence_names(&actions);
            let mut qc = format!(
                "$modelname \"{name}\"\n$scale {}\n{static_prop}$body body \"{filename}\"{axes}\n",
                request.scale
            );
            if sequence_names.is_empty() {
                writeln!(qc, "$sequence idle \"./{filename}\"").unwrap();
            } else {
                for sequence in &sequence_names {
                    writeln!(qc, "$sequence \"{sequence}\" \"./{filename}\"").unwrap();
                }
            }
            let mut desc = mdlc::qc::parse_qc_str(&qc, base).map_err(|errors| {
                ModelCompileError::Description(
                    errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            })?;
            for (sequence, action) in desc.sequences.iter_mut().zip(actions) {
                sequence.src_stack = Some(action);
            }
            bodygroups::apply(&mut desc, request, source)?;
            lods::apply(&mut desc, request, source)?;
            collision::exclude_render_parts(&mut desc, request, source)?;
            apply_animation_settings(&mut desc, &request.animation_settings, base)?;
            if matches!(
                request.collision,
                ModelCollision::Automatic(_) | ModelCollision::Custom { .. }
            ) {
                desc.physics.mass = Some(request.collision_mass);
            }
            desc.model
                .surface_prop
                .clone_from(&request.surface_property);
            Ok(desc)
        }
        _ => Err(ModelCompileError::UnsupportedSource(source.to_path_buf())),
    };
    result.map(|mut description| {
        if request.collision == ModelCollision::None {
            description.physics = mdlc::model::Physics::default();
        }
        if description.physics.auto_mass && !request.surface_properties.is_empty() {
            let authored = std::mem::replace(
                &mut description.physics.surface_properties,
                request.surface_properties.clone(),
            );
            for surface in authored {
                if let Some(existing) = description
                    .physics
                    .surface_properties
                    .iter_mut()
                    .find(|existing| existing.name.eq_ignore_ascii_case(&surface.name))
                {
                    *existing = surface;
                } else {
                    description.physics.surface_properties.push(surface);
                }
            }
        }
        description
    })
}

fn imported_sequence_names(actions: &[String]) -> Vec<String> {
    let mut used = std::collections::HashSet::new();
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let mut base = action
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
                        character
                    } else {
                        '_'
                    }
                })
                .take(63)
                .collect::<String>();
            if base.bytes().all(|byte| matches!(byte, b'_' | b'-' | b'.')) {
                base = format!("animation_{index}");
            }
            let mut name = base.clone();
            let mut suffix = 1;
            while !used.insert(name.to_ascii_lowercase()) {
                let ending = format!("_{suffix}");
                name = format!("{}{ending}", &base[..base.len().min(63 - ending.len())]);
                suffix += 1;
            }
            name
        })
        .collect()
}

pub(crate) fn normalize_model_name(name: &str) -> Result<String, ModelCompileError> {
    let mut name = name.replace('\\', "/");
    if name.trim().is_empty() || name.contains(['\n', '\r', '\0', ':']) {
        return Err(ModelCompileError::InvalidOption(
            "model name is invalid".into(),
        ));
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ModelCompileError::UnsafePath(PathBuf::from(name)));
    }
    validate_relative_path(Path::new(&name))?;
    if !name.starts_with("models/") {
        name = format!("models/{name}");
    }
    if name.to_ascii_lowercase().ends_with(".mdl") {
        name.truncate(name.len() - 4);
    }
    name.push_str(".mdl");
    Ok(name)
}

fn validate_relative_path(path: &Path) -> Result<(), ModelCompileError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(ModelCompileError::UnsafePath(path.to_path_buf()))
    } else {
        Ok(())
    }
}

fn checked_target(root: &Path, relative: &Path) -> Result<PathBuf, ModelCompileError> {
    validate_relative_path(relative)?;
    let mut path = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        path.push(component.as_os_str());
        let last = index + 1 == components.len();
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || if last {
                        !metadata.is_file()
                    } else {
                        !metadata.is_dir()
                    } =>
            {
                return Err(ModelCompileError::UnsafePath(path));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !last {
                    fs::create_dir(&path).map_err(|source| ModelCompileError::Io {
                        path: path.clone(),
                        source,
                    })?;
                }
            }
            Err(source) => return Err(ModelCompileError::Io { path, source }),
        }
    }
    Ok(path)
}

pub(crate) fn publish(
    stage: tempfile::TempDir,
    files_root: &Path,
    destination: &Path,
    files: &[ModelOutputFile],
    obsolete: &[PathBuf],
    overwrite: bool,
    cancel: &AtomicBool,
) -> Result<(), ModelCompileError> {
    let mut targets = Vec::new();
    for file in files {
        let relative = file
            .path
            .strip_prefix(destination)
            .map_err(|_| ModelCompileError::UnsafePath(file.path.clone()))?;
        let target = checked_target(destination, relative)?;
        if target.exists() && !overwrite {
            return Err(ModelCompileError::AlreadyExists(target));
        }
        targets.push((relative.to_path_buf(), target, true));
    }
    for path in obsolete {
        let relative = path
            .strip_prefix(destination)
            .map_err(|_| ModelCompileError::UnsafePath(path.clone()))?;
        let target = checked_target(destination, relative)?;
        if target.exists() {
            if !overwrite {
                return Err(ModelCompileError::AlreadyExists(target));
            }
            targets.push((relative.to_path_buf(), target, false));
        }
    }
    check_cancel(cancel)?;
    let backup = stage.path().join("backups");
    fs::create_dir(&backup).map_err(|source| ModelCompileError::Io {
        path: backup.clone(),
        source,
    })?;
    let mut changes = Vec::new();
    for (index, (relative, target, write)) in targets.into_iter().enumerate() {
        let saved = backup.join(index.to_string());
        let mut backed_up = false;
        let result = (|| {
            if target.exists() {
                fs::rename(&target, &saved)?;
                backed_up = true;
            }
            if write {
                fs::rename(files_root.join(relative), &target)?;
            }
            Ok::<_, std::io::Error>(())
        })();
        if let Err(source) = result {
            let mut rollback_errors = Vec::new();
            if backed_up && let Err(error) = fs::rename(&saved, &target) {
                rollback_errors.push(error.to_string());
            }
            for (target, saved, written) in changes.into_iter().rev() {
                if written && let Err(error) = fs::remove_file(&target) {
                    rollback_errors.push(error.to_string());
                }
                if let Some(saved) = saved
                    && let Err(error) = fs::rename(saved, &target)
                {
                    rollback_errors.push(error.to_string());
                }
            }
            let recovery_directory = if rollback_errors.is_empty() {
                None
            } else {
                Some(stage.keep())
            };
            return Err(ModelCompileError::Publish {
                path: target,
                source,
                recovery_directory,
                rollback_errors,
            });
        }
        changes.push((target, backed_up.then_some(saved), write));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_gltf_import_preserves_every_action_and_its_motion() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for distance in [3.0_f32, 6.0, 9.0, 12.0, 15.0, 18.0, 21.0, 24.0] {
            for value in [0.0, 0.0, 0.0, distance, 0.0, 0.0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut views = vec![
            serde_json::json!({"buffer": 0, "byteOffset": 0, "byteLength": 36}),
            serde_json::json!({"buffer": 0, "byteOffset": 36, "byteLength": 8}),
        ];
        let mut accessors = vec![
            serde_json::json!({"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]}),
            serde_json::json!({"bufferView": 1, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0], "max": [1]}),
        ];
        let mut animations = Vec::new();
        let long_name = "x".repeat(100);
        let long_duplicate = format!("{long_name} distinct");
        for (index, name) in [
            Some("Walk Cycle"),
            Some("walk cycle"),
            None,
            Some("animation_2"),
            Some("走る"),
            Some(&long_name),
            Some(&long_duplicate),
            Some("Walk_Cycle"),
        ]
        .into_iter()
        .enumerate()
        {
            views.push(
                serde_json::json!({"buffer": 0, "byteOffset": 44 + index * 24, "byteLength": 24}),
            );
            accessors.push(serde_json::json!({"bufferView": index + 2, "componentType": 5126, "count": 2, "type": "VEC3"}));
            let mut animation = serde_json::json!({
                "samplers": [{"input": 1, "output": index + 2}],
                "channels": [{"sampler": 0, "target": {"node": 0, "path": "translation"}}]
            });
            if let Some(name) = name {
                animation["name"] = name.into();
            }
            animations.push(animation);
        }
        let source = root.path().join("weights.gltf");
        fs::write(&source, serde_json::to_vec(&serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": format!("data:application/octet-stream;base64,{}", base64::encode(&bytes)), "byteLength": bytes.len()}],
            "bufferViews": views, "accessors": accessors, "animations": animations,
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
        })).unwrap()).unwrap();
        let mut request = ModelCompileRequest::new(source.clone(), root.path().join("addon"));
        request.automatic_axes = false;
        let desc = load_description(&request, &source, root.path()).unwrap();
        assert_eq!(
            desc.sequences
                .iter()
                .map(|sequence| sequence.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Walk_Cycle".to_owned(),
                "animation_1".to_owned(),
                "animation_2_1".to_owned(),
                "animation_2".to_owned(),
                "animation_4".to_owned(),
                "x".repeat(63),
                format!("{}_1", "x".repeat(61)),
                "Walk_Cycle_1".to_owned(),
            ]
        );
        assert_eq!(
            desc.sequences
                .iter()
                .map(|sequence| sequence.src_stack.as_deref().unwrap())
                .collect::<Vec<_>>(),
            [
                "Walk Cycle",
                "animation_1",
                "animation_2_1",
                "animation_2",
                "走る",
                &long_name,
                &long_duplicate,
                "Walk_Cycle",
            ]
        );
        let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
        assert_eq!(compiled.sequences.len(), 8);
        for (index, sequence) in compiled.sequences.iter().enumerate() {
            assert_eq!(sequence.frames.len(), 31);
            let position = sequence.frames.last().unwrap()[0].position;
            let distance = position
                .into_iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            assert!(
                (distance - 3.0 * (index + 1) as f32).abs() < 0.0001,
                "{index}: {position:?}"
            );
        }
        request.static_prop = true;
        let desc = load_description(&request, &source, root.path()).unwrap();
        assert_eq!(desc.sequences.len(), 1);
        assert_eq!(desc.sequences[0].name, "idle");
        assert!(desc.sequences[0].src_stack.is_none());
    }

    #[test]
    fn static_orientation_transforms_lod_vertices_and_flex_deltas() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("cube.smd");
        fs::write(
            &source,
            include_bytes!("../../vendor/mdlc/tests/fixtures/cube.smd"),
        )
        .unwrap();
        let mut request = ModelCompileRequest::new(source.clone(), root.path().join("addon"));
        request.static_prop = true;
        let desc = load_description(&request, &source, root.path()).unwrap();
        let mut compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
        let model = &mut compiled.bodyparts[0].models[0];
        let mut vertex = model.meshes[0].vertices[0].clone();
        vertex.pos = [1.0, 2.0, 3.0];
        vertex.normal = [1.0, 0.0, 0.0];
        model.lods = Some(mdlc::model::ModelLods {
            meshes: vec![mdlc::lod::MeshLods::single(vec![vertex], Vec::new())],
            num_lods: 1,
            switch_points: vec![0.0],
            bone_lod_usage: vec![1],
            no_facial: vec![false],
        });
        model.mesh_flexes = vec![vec![mdlc::flex::ResolvedFlex {
            flexdesc: 0,
            targets: [0.0; 4],
            flexpair: 0,
            vertanimtype: 0,
            vertanims: vec![mdlc::flex::ResolvedVertAnim {
                wrinkle: 0.0,
                index: 0,
                speed: 0,
                side: 0,
                delta: [1.0, 2.0, 3.0],
                ndelta: [1.0, 0.0, 0.0],
            }],
        }]];
        apply_orientation(&mut compiled, &source, [0.0, 0.0, 90.0]).unwrap();
        let model = &compiled.bodyparts[0].models[0];
        let vertex = &model.lods.as_ref().unwrap().meshes[0].vertices[0];
        for (actual, expected) in vertex.pos.iter().zip([-2.0, 1.0, 3.0]) {
            assert!((actual - expected).abs() < 0.0001);
        }
        for (actual, expected) in vertex.normal.iter().zip([0.0, 1.0, 0.0]) {
            assert!((actual - expected).abs() < 0.0001);
        }
        let flex = &model.mesh_flexes[0][0].vertanims[0];
        for (actual, expected) in flex.delta.iter().zip([-2.0, 1.0, 3.0]) {
            assert!((actual - expected).abs() < 0.0001);
        }
        for (actual, expected) in flex.ndelta.iter().zip([0.0, 1.0, 0.0]) {
            assert!((actual - expected).abs() < 0.0001);
        }
    }

    #[test]
    fn failed_publication_restores_prior_model_and_material_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("models")).unwrap();
        let first = root.path().join("models/first.mdl");
        let second = root.path().join("models/second.mdl");
        fs::write(&first, b"original-first").unwrap();
        fs::write(&second, b"original-second").unwrap();
        let stage = tempfile::tempdir_in(root.path()).unwrap();
        let files_root = stage.path().join("files");
        fs::create_dir_all(files_root.join("models")).unwrap();
        fs::write(files_root.join("models/first.mdl"), b"replacement").unwrap();
        let files = vec![
            ModelOutputFile {
                path: first.clone(),
                bytes: 11,
                kind: ModelOutputKind::Mdl,
            },
            ModelOutputFile {
                path: second.clone(),
                bytes: 0,
                kind: ModelOutputKind::Mdl,
            },
        ];
        assert!(matches!(
            publish(
                stage,
                &files_root,
                root.path(),
                &files,
                &[],
                true,
                &AtomicBool::new(false)
            ),
            Err(ModelCompileError::Publish { .. })
        ));
        assert_eq!(fs::read(first).unwrap(), b"original-first");
        assert_eq!(fs::read(second).unwrap(), b"original-second");
    }
}
