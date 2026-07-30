//! Source path normalization for map assets.

pub(super) fn normalize_skyname(value: &str) -> Option<String> {
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

pub(super) fn is_preview_material_visible(material: &str) -> bool {
    !material.starts_with("tools/")
        && !material.contains("skybox/")
        && !matches!(material, "sky" | "skybox")
}

pub(super) fn normalize_material_name(path: &str) -> Option<String> {
    normalize_source_path(path, Some(".vmt"))
}

pub(super) fn normalize_static_prop_model_path(path: &str) -> Option<String> {
    let path = normalize_source_path(path, None)?;
    let is_mdl = std::path::Path::new(&path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"));
    is_mdl.then_some(path)
}

pub(super) fn normalize_entity_prop_model_path(path: &str) -> Option<String> {
    let path = normalize_static_prop_model_path(path)?;
    path.starts_with("models/").then_some(path)
}

pub(super) fn normalize_source_path(path: &str, extension: Option<&str>) -> Option<String> {
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
