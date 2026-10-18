use super::{
    ModelCompileError, ModelCompileRequest, ModelDesc, Path, PathBuf, SkinMaterial,
    apply_orientation, apply_skins, authoring, check_cancel, model_materials, normalize_model_name,
};
use std::sync::atomic::AtomicBool;

pub(crate) struct PreparedModel {
    pub(crate) desc: ModelDesc,
    pub(crate) compiled: mdlc::model::CompiledModelDesc,
    pub(crate) variants: Vec<model_materials::SkinVariant>,
    // Referenced SMDs remain inputs until compilation and material preparation finish.
    pub(crate) authoring_assets: Option<tempfile::TempDir>,
}

pub(crate) fn validate_export_options(
    request: &ModelCompileRequest,
) -> Result<(), ModelCompileError> {
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
    Ok(())
}

pub(crate) fn prepare_model(
    request: &ModelCompileRequest,
    source: &Path,
    base: &Path,
    mut desc: ModelDesc,
    cancel: &AtomicBool,
) -> Result<PreparedModel, ModelCompileError> {
    let authoring_assets = authoring::prepare_assets(&mut desc, source)?;
    desc.model.version = Some(48);
    desc.model.name =
        normalize_model_name(request.model_name.as_deref().unwrap_or(&desc.model.name))?;
    if let Some(directory) = &request.material_directory {
        desc.materials.search_paths = vec![directory.replace('\\', "/")];
    }
    check_cancel(cancel)?;
    validate_export_options(request)?;
    let mut compiled =
        mdlc::compile::compile(&desc, base).map_err(mdlc::pipeline::PipelineError::Compile)?;
    apply_orientation(&mut compiled, source, request.orientation)?;
    let variants = apply_skins(&mut compiled.desc, &request.skins)?;
    Ok(PreparedModel {
        desc,
        compiled,
        variants,
        authoring_assets,
    })
}

pub(crate) fn prepare_model_materials(
    request: &ModelCompileRequest,
    source: &Path,
    base: &Path,
    desc: &ModelDesc,
    compiled: &ModelDesc,
    variants: &[model_materials::SkinVariant],
) -> Result<Option<model_materials::PreparedMaterials>, ModelCompileError> {
    if !request.export_materials {
        return Ok(None);
    }
    let mut sources = vec![source.to_owned()];
    for reference in desc
        .bodyparts
        .iter()
        .flat_map(|body| &body.models)
        .flat_map(|model| {
            std::iter::once(Some(&model.smd)).chain(model.lods.iter().map(|lod| lod.smd.as_ref()))
        })
        .flatten()
        .filter(|reference| !reference.is_empty())
    {
        let path: PathBuf = mdlc::compile::resolve_smd_path(base, reference)
            .map_err(|error| ModelCompileError::Description(error.to_string()))?;
        if !sources.contains(&path) {
            sources.push(path);
        }
    }
    let directories = &compiled.materials.search_paths;
    let imported = model_materials::source_material_names(compiled, variants);
    let mut prepared =
        model_materials::prepare_materials_for_sources(&imported, directories, base, &sources)?;
    model_materials::prepare_skin_materials(&mut prepared, variants, directories, base)?;
    for replacement in request
        .skins
        .iter()
        .flat_map(|skin| skin.replacements.values())
    {
        if let SkinMaterial::Material { name } = replacement {
            model_materials::require_prepared_material(&prepared, name, directories)?;
        }
    }
    Ok(Some(prepared))
}
