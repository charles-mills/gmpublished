use super::{
    ModelCompileError, ModelCompileRequest, Path, PathBuf, SourceCopier, canonical, fs, invalid,
    io_error, model_compile, quote_path, stage_asset, text_tokens,
};
use std::collections::BTreeSet;

pub(super) fn normalize_relative(path: &Path) -> Result<PathBuf, ModelCompileError> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => result.push(name),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if result.pop() => {}
            _ => return Err(ModelCompileError::UnsafePath(path.into())),
        }
    }
    if result.as_os_str().is_empty() {
        return Err(ModelCompileError::UnsafePath(path.into()));
    }
    Ok(result)
}

pub(super) fn copy(
    request: &ModelCompileRequest,
    desc: &mdlc::model::ModelDesc,
    source: &Path,
    files_root: &Path,
    assets: &str,
    writes: &mut Vec<PathBuf>,
    destination: &Path,
) -> Result<String, ModelCompileError> {
    let base = source.parent().unwrap();
    let original = request.qc_text.as_deref().map_or_else(
        || model_compile::read_qc_text(source),
        |text| Ok(text.to_owned()),
    )?;
    let includes = include_files(&original, source)?;
    if includes.len() > 64 {
        return Err(invalid("QC includes exceed 64 files"));
    }
    let sources = source_files(desc, base)?;
    let mut root = base.to_path_buf();
    for file in includes.iter().chain(&sources) {
        while !file.starts_with(&root) {
            if !root.pop() {
                return Err(invalid(
                    "QC dependencies on separate filesystem roots cannot be relocated",
                ));
            }
        }
    }
    for file in &sources {
        for dependency in gltf_dependencies(file)? {
            while !dependency.starts_with(&root) {
                if !root.pop() {
                    return Err(invalid(
                        "QC source dependencies cannot be relocated across filesystem roots",
                    ));
                }
            }
        }
    }
    let mapped_base = Path::new(assets).join(base.strip_prefix(&root).unwrap());
    for include in &includes {
        let text = model_compile::read_qc_text(include)?;
        let text = rewrite(
            &text,
            &root,
            &mapped_base,
            assets,
            request,
            &desc.model.source_search_paths,
        )?;
        let target = Path::new(assets).join(include.strip_prefix(&root).unwrap());
        stage_asset(files_root, &target, text.as_bytes(), writes)?;
    }
    let mut copier = SourceCopier::new(&root, files_root, assets, writes);
    for source in sources {
        copier.copy(&source)?;
    }
    let mut protected = includes;
    protected.insert(source.to_path_buf());
    protected.extend(copier.copied.values().cloned());
    drop(copier);
    let text = rewrite(
        &original,
        &root,
        &mapped_base,
        assets,
        request,
        &desc.model.source_search_paths,
    )?;
    let main = Path::new(assets).join(source.strip_prefix(&root).unwrap());
    stage_asset(files_root, &main, text.as_bytes(), writes)?;
    for relative in writes.iter() {
        let target = destination.join(relative);
        if target.exists() && protected.contains(&canonical(&target)?) {
            return Err(invalid(
                "saved QC assets would overwrite an imported source; choose a different destination or QC filename",
            ));
        }
    }
    Ok(format!(
        "$cd {}\n$include {}\n",
        quote_path(&mapped_base)?,
        quote_path(&main)?
    ))
}

/// Resolve inputs independently of whether saving bakes geometry or copies its source.
pub(super) fn input_files(
    request: &ModelCompileRequest,
    desc: &mdlc::model::ModelDesc,
    source: &Path,
) -> Result<BTreeSet<PathBuf>, ModelCompileError> {
    let mut inputs = BTreeSet::from([source.to_owned()]);
    if source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("qc"))
    {
        let text = request.qc_text.as_deref().map_or_else(
            || model_compile::read_qc_text(source),
            |text| Ok(text.to_owned()),
        )?;
        inputs.extend(include_files(&text, source)?);
    }
    if super::model_compile::authoring::model_settings(source)?.is_none() {
        inputs.extend(source_files(desc, source.parent().unwrap())?);
    }
    for lod in &request.lods {
        inputs.insert(canonical(&lod.source)?);
    }
    if let model_compile::ModelCollision::Custom { source, .. } = &request.collision {
        inputs.insert(canonical(source)?);
    }
    for replacement in request
        .skins
        .iter()
        .flat_map(|skin| skin.replacements.values())
    {
        if let super::SkinMaterial::Texture { path } = replacement {
            inputs.insert(canonical(&source.parent().unwrap().join(path))?);
        }
    }
    let mut pending: Vec<_> = inputs.iter().cloned().collect();
    while let Some(file) = pending.pop() {
        for dependency in gltf_dependencies(&file)?
            .into_iter()
            .chain(wavefront_dependencies(&file)?)
            .chain(fbx_dependencies(&file)?)
        {
            if inputs.insert(dependency.clone()) {
                if inputs.len() > 4096 {
                    return Err(invalid("source dependencies exceed the project size limit"));
                }
                pending.push(dependency);
            }
        }
    }
    Ok(inputs)
}

fn fbx_dependencies(source: &Path) -> Result<Vec<PathBuf>, ModelCompileError> {
    if !source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fbx"))
    {
        return Ok(Vec::new());
    }
    let scene = mdlc::fbx::load_scene(source, "QC source dependencies")
        .map_err(|error| invalid(&error.to_string()))?;
    let mut files = Vec::new();
    for texture in &scene.textures {
        if let Some((_, path)) = super::model_materials::resolve_fbx_texture_file(source, texture)?
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn include_files(text: &str, source: &Path) -> Result<BTreeSet<PathBuf>, ModelCompileError> {
    let mut lexer = mdlc::qc::lexer::Lexer::new(source.parent().unwrap().to_path_buf());
    lexer.load_from_memory(text, &source.display().to_string());
    while let Some(token) = lexer
        .next_token(true)
        .map_err(|error| invalid(&error.to_string()))?
    {
        if token.text.eq_ignore_ascii_case("$addsearchdir") {
            let directory = lexer
                .next_token(false)
                .map_err(|error| invalid(&error.to_string()))?
                .ok_or_else(|| invalid("missing QC search directory"))?;
            lexer
                .search_paths
                .push(lexer.qdir.join(directory.text.replace('\\', "/")));
        }
    }
    lexer
        .loaded_files
        .iter()
        .map(|path| canonical(Path::new(path)))
        .collect()
}

fn source_files(
    desc: &mdlc::model::ModelDesc,
    base: &Path,
) -> Result<BTreeSet<PathBuf>, ModelCompileError> {
    let mut sources = BTreeSet::new();
    let virtual_paths: BTreeSet<_> = desc
        .model
        .virtual_vtas
        .iter()
        .map(|source| {
            mdlc::compile::resolve_vta_path(base, &source.name)
                .map_err(|error| invalid(&error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let mut add = |name: &str, vta: bool| -> Result<(), ModelCompileError> {
        if name.is_empty() {
            return Ok(());
        }
        let path = if vta {
            mdlc::compile::resolve_vta_path(base, name)
        } else {
            mdlc::compile::resolve_smd_path(base, name)
        }
        .map_err(|error| invalid(&error.to_string()))?;
        if vta && virtual_paths.contains(&path) {
            return Ok(());
        }
        sources.insert(canonical(&path)?);
        Ok(())
    };
    for model in desc.bodyparts.iter().flat_map(|body| &body.models) {
        add(&model.smd, false)?;
        for lod in &model.lods {
            if let Some(source) = &lod.smd {
                add(source, false)?;
            }
        }
        for flex in &model.flexes {
            add(&flex.vta, true)?;
        }
    }
    for animation in &desc.animations {
        if !animation.forward_declared {
            add(&animation.smd, false)?;
        }
    }
    for sequence in &desc.sequences {
        for reference in std::iter::once(&sequence.smd).chain(&sequence.blends) {
            if !desc
                .animations
                .iter()
                .any(|animation| animation.name.eq_ignore_ascii_case(reference))
            {
                add(reference, false)?;
            }
        }
    }
    if let Some(source) = &desc.physics.smd {
        add(source, false)?;
    }
    for source in &desc.model.append_sources {
        add(&source.source, false)?;
    }
    for source in &desc.model.virtual_vtas {
        for frame in &source.frames {
            add(&frame.source, false)?;
        }
    }
    for source in &desc.model.source_dependencies {
        sources.insert(canonical(source)?);
    }
    let mut pending: Vec<_> = sources.iter().cloned().collect();
    while let Some(source) = pending.pop() {
        for dependency in wavefront_dependencies(&source)? {
            if sources.insert(dependency.clone()) {
                pending.push(dependency);
            }
        }
    }
    Ok(sources)
}

fn wavefront_dependencies(source: &Path) -> Result<Vec<PathBuf>, ModelCompileError> {
    let extension = source.extension().unwrap_or_default();
    let obj = extension.eq_ignore_ascii_case("obj");
    let mtl = extension.eq_ignore_ascii_case("mtl");
    if !obj && !mtl {
        return Ok(Vec::new());
    }
    if fs::metadata(source)
        .map_err(|error| io_error(source, error))?
        .len()
        > 128 * 1024 * 1024
    {
        return Err(invalid("source file exceeds the 128 MiB limit"));
    }
    let text = fs::read_to_string(source).map_err(|error| io_error(source, error))?;
    let mut result = Vec::new();
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        let Some(command) = words.next() else {
            continue;
        };
        let names: Vec<_> = if obj && command == "mtllib" {
            words.collect()
        } else if mtl
            && (command.starts_with("map_")
                || matches!(command, "bump" | "disp" | "decal" | "refl"))
        {
            words.next_back().into_iter().collect()
        } else {
            Vec::new()
        };
        for name in names {
            let normalized = name.replace('\\', "/");
            if Path::new(&normalized).is_absolute() {
                return Err(invalid(
                    "portable QC requires relative Wavefront material and texture paths",
                ));
            }
            let path = source.parent().unwrap().join(normalized);
            if path.is_file() {
                result.push(canonical(&path)?);
            }
        }
    }
    Ok(result)
}

fn gltf_dependencies(source: &Path) -> Result<Vec<PathBuf>, ModelCompileError> {
    let extension = source.extension().unwrap_or_default();
    if !extension.eq_ignore_ascii_case("gltf") && !extension.eq_ignore_ascii_case("glb") {
        return Ok(Vec::new());
    }
    let bytes = fs::read(source).map_err(|error| io_error(source, error))?;
    let filesystem_root = source.ancestors().last().unwrap();
    let mut files = Vec::new();
    super::visit_gltf_dependencies(source, &bytes, filesystem_root, |_, path| {
        files.push(canonical(path)?);
        Ok(())
    })?;
    Ok(files)
}

fn mapped_path(
    value: &str,
    root: &Path,
    mapped_base: &Path,
    assets: &str,
    quoted: bool,
) -> Result<String, ModelCompileError> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    let mapped = if path.is_absolute() {
        Path::new(assets).join(
            path.strip_prefix(root)
                .map_err(|_| ModelCompileError::UnsafePath(path.into()))?,
        )
    } else {
        mapped_base.join(path)
    };
    if !quoted && value.contains('$') {
        let value = mapped
            .to_str()
            .ok_or_else(|| invalid("QC paths must be UTF-8"))?
            .replace('\\', "/");
        if value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b';' | b'"' | 0))
        {
            return Err(invalid(
                "a variable QC dependency path cannot be relocated through a directory containing spaces or token delimiters",
            ));
        }
        Ok(value)
    } else {
        quote_path(&mapped)
    }
}

fn rewrite(
    text: &str,
    root: &Path,
    mapped_base: &Path,
    assets: &str,
    request: &ModelCompileRequest,
    search_paths: &[String],
) -> Result<String, ModelCompileError> {
    let original_base = root.join(
        mapped_base
            .strip_prefix(assets)
            .map_err(|_| invalid("invalid relocated QC base"))?,
    );
    let tokens = text_tokens(text)?;
    let mut edits = Vec::new();
    let mut index = 0;
    let mut depth = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.quoted {
            index += 1;
            continue;
        }
        if depth == 0
            && matches!(request.collision, model_compile::ModelCollision::None)
            && ["$collisionmodel", "$collisionjoints", "$collisiontext"]
                .iter()
                .any(|command| token.text.eq_ignore_ascii_case(command))
        {
            let mut end = index + 1;
            while end < tokens.len()
                && (tokens[end].quoted
                    || tokens[end].text != "{" && !tokens[end].text.starts_with('$'))
            {
                end += 1;
            }
            if end < tokens.len() && !tokens[end].quoted && tokens[end].text == "{" {
                let mut block_depth = 0usize;
                loop {
                    let current = tokens
                        .get(end)
                        .ok_or_else(|| invalid("unterminated QC collision block"))?;
                    if !current.quoted {
                        match current.text.as_str() {
                            "{" => block_depth += 1,
                            "}" => block_depth -= 1,
                            _ => {}
                        }
                    }
                    end += 1;
                    if block_depth == 0 {
                        break;
                    }
                }
            }
            edits.push((token.start, tokens[end - 1].end, String::new()));
            index = end;
            continue;
        }
        if token.text == "{" {
            depth += 1;
        }
        if token.text == "}" {
            depth = depth.saturating_sub(1);
        }
        if token.text.eq_ignore_ascii_case("$include")
            || depth == 0
                && (token.text.eq_ignore_ascii_case("$cd")
                    || token.text.eq_ignore_ascii_case("$addsearchdir"))
        {
            let next = tokens
                .get(index + 1)
                .ok_or_else(|| invalid("missing QC dependency path"))?;
            let normalized = next.text.replace('\\', "/");
            let included = if token.text.eq_ignore_ascii_case("$include")
                && !Path::new(&normalized).is_absolute()
            {
                std::iter::once(original_base.join(&normalized))
                    .chain(
                        search_paths
                            .iter()
                            .map(|directory| original_base.join(directory).join(&normalized)),
                    )
                    .find(|path| path.is_file())
            } else {
                None
            };
            let value = included.as_ref().map(|path| path.to_string_lossy());
            edits.push((
                next.start,
                next.end,
                mapped_path(
                    value.as_deref().unwrap_or(&next.text),
                    root,
                    mapped_base,
                    assets,
                    next.quoted,
                )?,
            ));
            index += 2;
            continue;
        }
        if depth == 0
            && request.material_directory.is_some()
            && token.text.eq_ignore_ascii_case("$cdmaterials")
        {
            let mut end = index + 1;
            while end < tokens.len()
                && !text[tokens[end - 1].end..tokens[end].start].contains(['\n', '\r'])
                && (tokens[end].quoted
                    || !tokens[end].text.starts_with('$')
                    || tokens[end].text[1..].contains('$'))
            {
                end += 1;
            }
            let remove_end =
                if end > index + 1 && !tokens[end - 1].quoted && tokens[end - 1].text == "\\\\" {
                    end - 1
                } else {
                    end
                };
            edits.push((token.start, tokens[remove_end - 1].end, String::new()));
            index = end;
            continue;
        }
        if depth == 0
            && !request.skins.is_empty()
            && token.text.eq_ignore_ascii_case("$texturegroup")
        {
            let mut end = index + 1;
            while end < tokens.len() && tokens[end].text != "{" {
                end += 1;
            }
            let mut depth = 0usize;
            while end < tokens.len() {
                match tokens[end].text.as_str() {
                    "{" => depth += 1,
                    "}" => {
                        depth = depth
                            .checked_sub(1)
                            .ok_or_else(|| invalid("invalid QC texture group"))?;
                    }
                    _ => {}
                }
                end += 1;
                if depth == 0 {
                    break;
                }
            }
            if depth != 0
                || end == tokens.len() && tokens.last().is_none_or(|token| token.text != "}")
            {
                return Err(invalid("unterminated QC texture group"));
            }
            edits.push((token.start, tokens[end - 1].end, String::new()));
            index = end;
            continue;
        }
        index += 1;
    }
    let mut result = text.to_owned();
    for (start, end, replacement) in edits.into_iter().rev() {
        result.replace_range(start..end, &replacement);
    }
    Ok(result)
}

pub(super) fn validate_dependencies(
    text: &str,
    candidate: &Path,
    desc: &mdlc::model::ModelDesc,
    original: &mdlc::model::ModelDesc,
) -> Result<(), ModelCompileError> {
    if desc.model.source_dependencies.len() != original.model.source_dependencies.len()
        || desc.model.append_sources.len() != original.model.append_sources.len()
        || desc.model.virtual_vtas.len() != original.model.virtual_vtas.len()
    {
        return Err(invalid(
            "a QC procedural source dependency was lost during relocation",
        ));
    }
    let root = candidate.parent().unwrap();
    for path in include_files(text, candidate)?
        .into_iter()
        .chain(source_files(desc, root)?)
    {
        if !path.starts_with(root) {
            return Err(invalid(
                "QC contains a dependency that cannot be relocated safely; use relative include and source paths",
            ));
        }
    }
    Ok(())
}
