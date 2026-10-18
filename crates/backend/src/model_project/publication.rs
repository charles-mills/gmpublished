use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use serde::{Deserialize, Serialize};

use super::{
    ModelCompileError, ModelOutputFile, ModelOutputKind, invalid, io_error, model_compile,
};

const REGISTRY: &str = ".gmpublished-qc-projects.json";
const FORMAT: &str = "gmpublished-qc-projects";
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 20_000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    format: String,
    version: u32,
    projects: BTreeMap<String, BTreeMap<String, Asset>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            format: FORMAT.into(),
            version: 1,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    size: u64,
    crc32: u32,
    /// Only files created by this project or retained from its prior ownership may change.
    managed: bool,
}

fn relative_name(path: &Path) -> Result<String, ModelCompileError> {
    if super::portable_qc::normalize_relative(path)? != path {
        return Err(ModelCompileError::UnsafePath(path.into()));
    }
    let name = path
        .to_str()
        .ok_or_else(|| invalid("QC asset paths must be UTF-8"))?
        .replace(std::path::MAIN_SEPARATOR, "/");
    if name.contains(['\\', ':', '\0']) || name == REGISTRY {
        return Err(ModelCompileError::UnsafePath(path.into()));
    }
    Ok(name)
}

fn project_name(filename: &OsStr) -> Result<&str, ModelCompileError> {
    let path = Path::new(filename);
    let name = filename
        .to_str()
        .ok_or_else(|| invalid("QC filename must be UTF-8"))?;
    if path.components().count() != 1
        || path.file_name() != Some(filename)
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("qc"))
    {
        return Err(ModelCompileError::UnsafePath(path.into()));
    }
    relative_name(path)?;
    Ok(name)
}

/// Check every existing component before reading an output; the publisher checks again before writing.
fn existing(root: &Path, relative: &Path) -> Result<Option<Asset>, ModelCompileError> {
    let mut path = root.to_owned();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        path.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&path, error)),
        };
        let last = index + 1 == components.len();
        if metadata.file_type().is_symlink()
            || if last {
                !metadata.is_file()
            } else {
                !metadata.is_dir()
            }
        {
            return Err(ModelCompileError::UnsafePath(path));
        }
    }
    fingerprint(&path).map(Some)
}

fn fingerprint(path: &Path) -> Result<Asset, ModelCompileError> {
    use std::io::Read as _;
    let mut file = fs::File::open(path).map_err(|error| io_error(path, error))?;
    let mut crc32 = crc32fast::Hasher::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_error(path, error))?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| invalid("QC asset exceeds the size limit"))?;
        crc32.update(&buffer[..count]);
    }
    Ok(Asset {
        size,
        crc32: crc32.finalize(),
        managed: false,
    })
}

fn same(a: &Asset, b: &Asset) -> bool {
    a.size == b.size && a.crc32 == b.crc32
}

fn is_input(target: &Path, inputs: &BTreeSet<PathBuf>) -> Result<bool, ModelCompileError> {
    if inputs.contains(&super::canonical(target)?) {
        return Ok(true);
    }
    for input in inputs {
        if same_file::is_same_file(target, input).map_err(|error| io_error(target, error))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_registry(destination: &Path) -> Result<Registry, ModelCompileError> {
    let Some(file) = existing(destination, Path::new(REGISTRY))? else {
        return Ok(Registry::default());
    };
    let path = destination.join(REGISTRY);
    if file.size > MAX_REGISTRY_BYTES {
        return Err(invalid("QC ownership registry exceeds the size limit"));
    }
    let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
    let registry: Registry = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("QC ownership registry is invalid; existing files were preserved"))?;
    if registry.format != FORMAT || registry.version != 1 || registry.projects.len() > 4096 {
        return Err(invalid(
            "QC ownership registry is invalid; existing files were preserved",
        ));
    }
    for (project, assets) in &registry.projects {
        project_name(OsStr::new(project))?;
        if assets.len() > MAX_FILES {
            return Err(invalid("QC ownership registry contains too many files"));
        }
        for name in assets.keys() {
            relative_name(Path::new(name))?;
        }
    }
    Ok(registry)
}

pub(super) fn publish(
    stage: tempfile::TempDir,
    files_root: &Path,
    destination: &Path,
    writes: Vec<PathBuf>,
    filename: &OsStr,
    inputs: &BTreeSet<PathBuf>,
) -> Result<(), ModelCompileError> {
    let project = project_name(filename)?;
    let _export = model_compile::EXPORT_LOCK.lock();
    let mut registry = read_registry(destination)?;
    let registry_path = destination.join(REGISTRY);
    if registry_path.exists() && is_input(&registry_path, inputs)? {
        return Err(invalid(
            "QC ownership registry would overwrite an imported source dependency",
        ));
    }
    let previous = registry.projects.get(project);
    let mut files = Vec::new();
    let mut assets = BTreeMap::new();
    for relative in writes {
        let name = relative_name(&relative)?;
        if assets.contains_key(&name) {
            return Err(invalid("conflicting saved QC asset paths"));
        }
        let mut candidate = fingerprint(&files_root.join(&relative))?;
        let current = existing(destination, &relative)?;
        let prior = previous.and_then(|assets| assets.get(&name));
        let managed = prior.is_some_and(|asset| asset.managed);
        if let Some(current) = current {
            candidate.managed = managed;
            // Identical existing assets are shared without rewriting or claiming ownership.
            if same(&candidate, &current) {
                assets.insert(name, candidate);
                continue;
            }
            let target = destination.join(&relative);
            if is_input(&target, inputs)? {
                return Err(invalid(
                    "saved QC assets would overwrite an imported source; choose a different destination or QC filename",
                ));
            }
            if !managed || !prior.is_some_and(|prior| same(prior, &current)) {
                return Err(invalid(&format!(
                    "saved QC would overwrite an unowned or edited file: {name}; choose a different destination, QC filename or materials directory"
                )));
            }
            let shared = registry.projects.iter().any(|(owner, assets)| {
                owner != project && assets.keys().any(|path| path.eq_ignore_ascii_case(&name))
            });
            if shared {
                return Err(invalid(&format!(
                    "saved QC would change an asset used by another project: {name}; choose a different destination or materials directory"
                )));
            }
        } else {
            candidate.managed = true;
        }
        files.push(ModelOutputFile {
            path: destination.join(&relative),
            bytes: candidate.size,
            kind: ModelOutputKind::Material,
        });
        assets.insert(name, candidate);
    }
    if assets.len() > MAX_FILES {
        return Err(invalid("saved QC contains too many files"));
    }
    registry.projects.insert(project.to_owned(), assets);
    let bytes =
        serde_json::to_vec_pretty(&registry).map_err(|error| invalid(&error.to_string()))?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(invalid("QC ownership registry exceeds the size limit"));
    }
    fs::write(files_root.join(REGISTRY), &bytes)
        .map_err(|error| io_error(&registry_path, error))?;
    files.push(ModelOutputFile {
        path: registry_path,
        bytes: bytes.len() as u64,
        kind: ModelOutputKind::Material,
    });
    model_compile::publish(
        stage,
        files_root,
        destination,
        &files,
        &[],
        true,
        &AtomicBool::new(false),
    )
}

#[cfg(test)]
#[path = "publication/tests.rs"]
mod tests;
