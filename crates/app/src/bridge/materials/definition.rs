use super::{
    Arc, ContentPath, EffectiveMaterial, HashMap, Limits, MaterialResolver, Mutex, OnceLock,
    PATCH_INCLUDE_LIMIT, profile,
};

type DocumentCell = Arc<OnceLock<Result<Arc<SelectedDocument>, MaterialSourceFailure>>>;
type DefinitionCell = Arc<OnceLock<Option<Arc<MaterialDefinition>>>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MaterialSourceFailure {
    Missing,
    Malformed(ContentPath),
}

#[derive(Debug, Default)]
pub(super) struct MaterialDefinitionCache {
    queries: Mutex<HashMap<Vec<ContentPath>, DocumentCell>>,
    documents: Mutex<HashMap<ContentPath, DocumentCell>>,
    definitions: Mutex<HashMap<(ContentPath, profile::HardwareProfile), DefinitionCell>>,
}

#[derive(Debug)]
struct SelectedDocument {
    path: ContentPath,
    document: vformats::vmt::VmtDocument<'static>,
}

#[derive(Debug)]
pub(super) struct MaterialDefinition {
    pub(super) path: ContentPath,
    pub(super) material: EffectiveMaterial,
}

impl MaterialDefinition {
    pub(super) fn canonical_name(&self) -> String {
        self.path
            .as_str()
            .strip_prefix("materials/")
            .and_then(|path| path.strip_suffix(".vmt"))
            .unwrap_or(self.path.as_str())
            .to_owned()
    }
}

impl MaterialResolver {
    pub(super) fn material_definition(
        &self,
        paths: &[ContentPath],
    ) -> Option<Arc<MaterialDefinition>> {
        if paths.is_empty() {
            return None;
        }
        let selected = self.selected_material_document(paths).ok()?;
        let cell = self
            .shared
            .material_definitions
            .definitions
            .lock()
            .entry((selected.path.clone(), self.config.hardware_profile))
            .or_default()
            .clone();
        cell.get_or_init(|| {
            let material = self.effective_material_document(
                &selected.document,
                0,
                &mut vec![selected.path.clone()],
            );
            Some(Arc::new(MaterialDefinition {
                path: selected.path.clone(),
                material,
            }))
        })
        .clone()
    }

    pub(super) fn material_source_failure(
        &self,
        paths: &[ContentPath],
    ) -> Option<MaterialSourceFailure> {
        self.selected_material_document(paths).err()
    }

    fn selected_material_document(
        &self,
        paths: &[ContentPath],
    ) -> Result<Arc<SelectedDocument>, MaterialSourceFailure> {
        let cell = self
            .shared
            .material_definitions
            .queries
            .lock()
            .entry(paths.to_vec())
            .or_default()
            .clone();
        cell.get_or_init(|| {
            let selected = self
                .find_content_bytes(paths)
                .ok_or(MaterialSourceFailure::Missing)?;
            let path = ContentPath::new(&selected.path).ok_or(MaterialSourceFailure::Missing)?;
            let document = self
                .shared
                .material_definitions
                .documents
                .lock()
                .entry(path.clone())
                .or_default()
                .clone();
            document
                .get_or_init(|| {
                    let text = String::from_utf8_lossy(&selected.bytes);
                    let document = match vformats::vmt::parse(&text, &Limits::default()) {
                        Ok(document) => document,
                        Err(error) => {
                            log::warn!(
                                "selected material {} from {:?} is malformed: {error}",
                                selected.path,
                                selected.tier
                            );
                            return Err(MaterialSourceFailure::Malformed(path.clone()));
                        }
                    };
                    Ok(Arc::new(SelectedDocument {
                        path,
                        document: vformats::vmt::VmtDocument {
                            shader: document.shader.into_owned().into(),
                            kv: profile::owned(&document.kv),
                        },
                    }))
                })
                .clone()
        })
        .clone()
    }

    pub(super) fn effective_material_document(
        &self,
        document: &vformats::vmt::VmtDocument<'_>,
        depth: usize,
        visited: &mut Vec<ContentPath>,
    ) -> EffectiveMaterial {
        if let Some(patch) = document.patch() {
            let mut material = self
                .effective_patch_include(&patch.include, depth, visited)
                .unwrap_or_else(|| {
                    EffectiveMaterial::with_profile(document, self.config.hardware_profile)
                });
            material.apply_patch_values(document, &patch);
            material
        } else {
            EffectiveMaterial::with_profile(document, self.config.hardware_profile)
        }
    }

    fn effective_patch_include(
        &self,
        include: &str,
        depth: usize,
        visited: &mut Vec<ContentPath>,
    ) -> Option<EffectiveMaterial> {
        if depth >= PATCH_INCLUDE_LIMIT {
            log::debug!("material patch include recursion limit reached at {include}");
            return None;
        }
        let Some(path) = ContentPath::new(include) else {
            log::debug!("material patch include path rejected: {include}");
            return None;
        };
        if visited.contains(&path) {
            log::debug!("material patch include cycle rejected at {path}");
            return None;
        }
        visited.push(path.clone());
        // Includes bypass effective-result cells to avoid cross-thread cycle waits.
        let material = self
            .selected_material_document(std::slice::from_ref(&path))
            .ok()
            .map(|selected| {
                self.effective_material_document(&selected.document, depth + 1, visited)
            });
        visited.pop();
        material
    }
}

#[cfg(test)]
mod tests;
