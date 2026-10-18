use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::model_manager::{ExportResult, InstalledModel};

const PREFIX: &str = "gmpublished_model_";
const MANIFEST: &str = "gmpublished-model.json";
const LEGACY_PREFIX: &str = "gmpublished_model_test_";
const LEGACY_MANIFEST: &str = "gmpublished-model-test.json";
const MAX_FILES: usize = 20_002;

#[derive(Deserialize, Serialize)]
struct Manifest {
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
    files: Vec<Asset>,
}

#[derive(Deserialize, Serialize)]
struct Asset {
    path: String,
    size: u64,
    crc32: u32,
}

pub(in crate::app) fn install_model(
    gmod: &Path,
    result: &ExportResult,
) -> Result<InstalledModel, String> {
    if !result.compiled {
        return Err("Compile a model before installing it in Garry's Mod".to_owned());
    }
    if result.files.len() > MAX_FILES - 1 {
        return Err("The model contains too many files".to_owned());
    }
    let addons = addons_directory(gmod)?;
    let root = fs::canonicalize(&result.directory).map_err(|error| error.to_string())?;
    let mut assets = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for source in &result.files {
        let relative = source
            .strip_prefix(&result.directory)
            .map_err(|_| "A model asset is outside the export directory".to_owned())?;
        let original = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_file(&root, &original)?;
        let name = super::asset_path(&result.directory, source)?;
        if !valid_asset(&name) {
            return Err(format!("Unsupported model asset: {name}"));
        }
        let mut asset = describe_file(&root, &original)?;
        asset.path.clone_from(&name);
        sources.insert(name.to_ascii_lowercase(), root.join(relative));
        if assets.insert(name.to_ascii_lowercase(), asset).is_some() {
            return Err(format!("Duplicate model asset: {name}"));
        }
    }
    let models: Vec<_> = assets
        .values()
        .filter(|asset| asset.path.to_ascii_lowercase().ends_with(".mdl"))
        .collect();
    if models.len() != 1 {
        return Err("The export must contain exactly one model file".to_owned());
    }
    let model = models[0].path.clone();
    let name = model_name(&model);
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let target = addons.join(format!(
        "{PREFIX}{slug}_{:08x}",
        crc32fast::hash(model.to_ascii_lowercase().as_bytes())
    ));
    let replacing = match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("The model addon location is occupied by another file".to_owned());
            }
            let (manifest, marker) = owned_manifest(&target)?;
            if manifest.model != model || is_modified(&target, &manifest, marker)? {
                return Err(format!(
                    "The installed addon was modified. Remove it or choose a different model path: {}",
                    target.display()
                ));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.to_string()),
    };
    reject_conflicts(&addons, &target, &assets)?;
    let stage = tempfile::Builder::new()
        .prefix(".gmpublished-model-install-")
        .tempdir_in(&addons)
        .map_err(|error| error.to_string())?;
    let mut manifest = Manifest {
        version: 1,
        name: name.clone(),
        model,
        files: Vec::with_capacity(assets.len() + 1),
    };
    for asset in assets.into_values() {
        let destination = stage.path().join(&asset.path);
        fs::create_dir_all(destination.parent().expect("asset parent"))
            .map_err(|error| error.to_string())?;
        fs::copy(&sources[&asset.path.to_ascii_lowercase()], &destination)
            .map_err(|error| error.to_string())?;
        let copied = describe_file(stage.path(), &asset.path)?;
        if copied.size != asset.size || copied.crc32 != asset.crc32 {
            return Err("A model asset changed during installation. Try again".to_owned());
        }
        manifest.files.push(copied);
    }
    fs::write(
        stage.path().join("addon.json"),
        serde_json::to_vec(&serde_json::json!({
            "title": name, "type": "model", "tags": ["build"]
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest
        .files
        .push(describe_file(stage.path(), "addon.json")?);
    fs::write(
        stage.path().join(MANIFEST),
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if replacing {
        let (previous_manifest, marker) = owned_manifest(&target)?;
        if is_modified(&target, &previous_manifest, marker)? {
            return Err("The installed addon changed during installation. Try again".to_owned());
        }
        let backup = tempfile::Builder::new()
            .prefix(".gmpublished-model-backup-")
            .tempdir_in(&addons)
            .map_err(|error| error.to_string())?;
        let previous = backup.path().join("previous");
        fs::rename(&target, &previous).map_err(|error| error.to_string())?;
        if let Err(error) = fs::rename(stage.path(), &target) {
            if let Err(restore) = fs::rename(&previous, &target) {
                let recovery = backup.keep();
                return Err(format!(
                    "Installation failed ({error}); restore failed ({restore}). The previous addon is preserved at {}",
                    recovery.join("previous").display()
                ));
            }
            return Err(error.to_string());
        }
    } else {
        if fs::symlink_metadata(&target).is_ok() {
            return Err("The model addon location became occupied during installation".to_owned());
        }
        fs::rename(stage.path(), &target).map_err(|error| error.to_string())?;
    }
    Ok(installed(target, &manifest, false))
}

pub(in crate::app) fn list_models(gmod: &Path) -> Result<Vec<InstalledModel>, String> {
    let addons = addons_directory(gmod)?;
    let mut models = Vec::new();
    for entry in fs::read_dir(addons).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
            || !entry.file_name().to_string_lossy().starts_with(PREFIX)
        {
            continue;
        }
        let directory = entry.path();
        let Ok((manifest, marker)) = owned_manifest(&directory) else {
            continue;
        };
        let modified = is_modified(&directory, &manifest, marker).unwrap_or(true);
        models.push(installed(directory, &manifest, modified));
    }
    models.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.directory.cmp(&right.directory))
    });
    Ok(models)
}

pub(in crate::app) fn remove_model(gmod: &Path, directory: &Path) -> Result<(), String> {
    let addons = addons_directory(gmod)?;
    if directory.parent() != Some(addons.as_path()) {
        return Err(
            "Only managed addons in the Garry's Mod addons directory can be removed".to_owned(),
        );
    }
    owned_manifest(directory)?;
    let mut files = BTreeSet::new();
    collect_tree(directory, directory, 0, &mut files)?;
    fs::remove_dir_all(directory).map_err(|error| error.to_string())
}

fn addons_directory(gmod: &Path) -> Result<PathBuf, String> {
    let root = fs::canonicalize(gmod).map_err(|error| error.to_string())?;
    let game = child_directory(&root, "garrysmod")?;
    child_directory(&game, "addons")
}

fn child_directory(parent: &Path, name: &str) -> Result<PathBuf, String> {
    for entry in fs::read_dir(parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name().eq_ignore_ascii_case(name) {
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                return Err(format!(
                    "The Garry's Mod {name} directory is not a regular directory"
                ));
            }
            return Ok(entry.path());
        }
    }
    Err(format!("The Garry's Mod {name} directory was not found"))
}

fn owned_manifest(directory: &Path) -> Result<(Manifest, &'static str), String> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    let name = directory.file_name().unwrap_or_default().to_string_lossy();
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !name.starts_with(PREFIX) {
        return Err("This directory is not a managed model addon".to_owned());
    }
    let marker = if directory.join(MANIFEST).exists() {
        MANIFEST
    } else if name.starts_with(LEGACY_PREFIX) {
        LEGACY_MANIFEST
    } else {
        return Err("The model addon ownership manifest is missing".to_owned());
    };
    validate_file(directory, marker)?;
    let metadata = fs::metadata(directory.join(marker)).map_err(|error| error.to_string())?;
    if metadata.len() > 4 * 1024 * 1024 {
        return Err("Invalid model addon manifest".to_owned());
    }
    let mut manifest: Manifest = serde_json::from_slice(
        &fs::read(directory.join(marker)).map_err(|error| error.to_string())?,
    )
    .map_err(|_| "Invalid model addon manifest".to_owned())?;
    if manifest.version != 1 || manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err("Unsupported model addon manifest".to_owned());
    }
    let mut paths = BTreeSet::new();
    for asset in &manifest.files {
        if !relative_path(&asset.path)
            || asset.path == marker
            || !paths.insert(asset.path.to_ascii_lowercase())
        {
            return Err("Invalid model addon manifest paths".to_owned());
        }
    }
    if marker == LEGACY_MANIFEST {
        manifest.model = manifest
            .files
            .iter()
            .find(|asset| {
                asset.path.starts_with("models/")
                    && asset.path.to_ascii_lowercase().ends_with(".mdl")
            })
            .map(|asset| asset.path.clone())
            .ok_or_else(|| "The model addon contains no model".to_owned())?;
        manifest.name = model_name(&manifest.model);
    } else if manifest.name.is_empty()
        || !valid_asset(&manifest.model)
        || !manifest.model.to_ascii_lowercase().ends_with(".mdl")
        || !manifest
            .files
            .iter()
            .any(|asset| asset.path == manifest.model)
        || manifest
            .files
            .iter()
            .any(|asset| asset.path != "addon.json" && !valid_asset(&asset.path))
    {
        return Err("Invalid model addon manifest".to_owned());
    }
    Ok((manifest, marker))
}

fn installed(directory: PathBuf, manifest: &Manifest, modified: bool) -> InstalledModel {
    InstalledModel {
        directory,
        name: manifest.name.clone(),
        model: manifest.model.clone(),
        bytes: manifest
            .files
            .iter()
            .fold(0_u64, |bytes, asset| bytes.saturating_add(asset.size)),
        modified,
    }
}

fn is_modified(root: &Path, manifest: &Manifest, marker: &str) -> Result<bool, String> {
    let mut expected = BTreeSet::from([marker.to_owned()]);
    for asset in &manifest.files {
        if validate_file(root, &asset.path).is_err() {
            return Ok(true);
        }
        let actual = describe_file(root, &asset.path)?;
        if actual.size != asset.size || actual.crc32 != asset.crc32 {
            return Ok(true);
        }
        expected.insert(asset.path.clone());
        for parent in Path::new(&asset.path).ancestors().skip(1) {
            if !parent.as_os_str().is_empty() {
                expected.insert(format!(
                    "{}/",
                    parent
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                ));
            }
        }
    }
    let mut actual = BTreeSet::new();
    collect_tree(root, root, 0, &mut actual)?;
    Ok(actual != expected)
}

fn reject_conflicts(
    addons: &Path,
    target: &Path,
    assets: &BTreeMap<String, Asset>,
) -> Result<(), String> {
    for entry in fs::read_dir(addons).map_err(|error| error.to_string())? {
        let directory = entry.map_err(|error| error.to_string())?.path();
        if directory == target {
            continue;
        }
        if owned_manifest(&directory).is_err() {
            continue;
        }
        let mut paths = BTreeSet::new();
        collect_tree(&directory, &directory, 0, &mut paths)?;
        for path in paths {
            let Some(asset) = assets.get(&path.to_ascii_lowercase()) else {
                continue;
            };
            validate_file(&directory, &path)?;
            let actual = describe_file(&directory, &path)?;
            if actual.size != asset.size || actual.crc32 != asset.crc32 {
                return Err(format!(
                    "{} conflicts with installed addon {}. Choose a different model path or materials directory",
                    asset.path,
                    directory.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    Ok(())
}

fn relative_path(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_asset(name: &str) -> bool {
    if !relative_path(name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let extension = Path::new(&lower)
        .extension()
        .and_then(|value| value.to_str());
    if lower.starts_with("models/") {
        matches!(extension, Some("mdl" | "vvd" | "vtx" | "phy" | "ani"))
    } else {
        lower.starts_with("materials/") && matches!(extension, Some("vmt" | "vtf"))
    }
}

fn validate_file(root: &Path, name: &str) -> Result<(), String> {
    if !relative_path(name) {
        return Err("Invalid model addon path".to_owned());
    }
    let mut path = root.to_path_buf();
    for part in Path::new(name).components() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Model addons cannot contain symbolic links".to_owned());
        }
    }
    if !fs::symlink_metadata(&path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("A model addon asset is not a regular file".to_owned());
    }
    Ok(())
}

fn describe_file(root: &Path, name: &str) -> Result<Asset, String> {
    let mut file = fs::File::open(root.join(name)).map_err(|error| error.to_string())?;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    let mut hash = crc32fast::Hasher::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(Asset {
        path: name.to_owned(),
        size,
        crc32: hash.finalize(),
    })
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    depth: usize,
    paths: &mut BTreeSet<String>,
) -> Result<(), String> {
    if depth > 32 || paths.len() > MAX_FILES * 2 {
        return Err("The model addon directory contains too many entries".to_owned());
    }
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if kind.is_dir() {
            paths.insert(format!("{name}/"));
            collect_tree(root, &path, depth + 1, paths)?;
        } else if kind.is_file() {
            paths.insert(name);
        } else {
            return Err("Model addons cannot contain links or special files".to_owned());
        }
        if paths.len() > MAX_FILES * 2 {
            return Err("The model addon directory contains too many entries".to_owned());
        }
    }
    Ok(())
}

fn model_name(model: &str) -> String {
    Path::new(model)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(root: &Path) -> PathBuf {
        let game = root.join("game");
        fs::create_dir_all(game.join("garrysmod/addons")).unwrap();
        game
    }

    fn export(root: &Path, model: &str, bytes: &[u8]) -> ExportResult {
        let directory = root.join("export");
        let path = directory.join(model);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        ExportResult {
            directory,
            files: vec![path],
            bytes: bytes.len() as u64,
            warnings: Vec::new(),
            compiled: true,
        }
    }

    #[test]
    fn installing_and_updating_one_model_preserves_other_addons() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let first_export = export(root.path(), "models/props/first.mdl", b"first");
        let first = install_model(&game, &first_export).unwrap();
        let second_export = export(root.path(), "models/props/second.mdl", b"second");
        let second = install_model(&game, &second_export).unwrap();
        fs::write(&first_export.files[0], b"updated").unwrap();
        let updated = install_model(&game, &first_export).unwrap();
        assert_eq!(updated.directory, first.directory);
        assert_eq!(list_models(&game).unwrap().len(), 2);
        assert_eq!(
            fs::read(first.directory.join("models/props/first.mdl")).unwrap(),
            b"updated"
        );
        assert_eq!(
            fs::read(second.directory.join("models/props/second.mdl")).unwrap(),
            b"second"
        );
        assert!(!first.directory.join("lua").exists());
    }

    #[test]
    fn edited_addons_are_listed_but_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let source = export(root.path(), "models/prop.mdl", b"original");
        let addon = install_model(&game, &source).unwrap();
        let installed = addon.directory.join("models/prop.mdl");
        fs::write(&installed, b"edited").unwrap();
        assert!(list_models(&game).unwrap()[0].modified);
        assert!(
            install_model(&game, &source)
                .unwrap_err()
                .contains("modified")
        );
        assert_eq!(fs::read(&installed).unwrap(), b"edited");
        fs::write(&installed, b"original").unwrap();
        fs::write(addon.directory.join("keep.txt"), b"personal").unwrap();
        assert!(list_models(&game).unwrap()[0].modified);
        assert!(install_model(&game, &source).is_err());
        fs::remove_file(addon.directory.join("keep.txt")).unwrap();
        fs::remove_file(&installed).unwrap();
        assert!(list_models(&game).unwrap()[0].modified);
    }

    #[test]
    fn removing_a_modified_managed_addon_preserves_unrelated_directories() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let source = export(root.path(), "models/prop.mdl", b"model");
        let addon = install_model(&game, &source).unwrap();
        fs::write(addon.directory.join("extra.txt"), b"added").unwrap();
        let unrelated = game.join("garrysmod/addons/unrelated");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("keep.txt"), b"keep").unwrap();
        assert!(remove_model(&game, &unrelated).is_err());
        remove_model(&game, &addon.directory).unwrap();
        assert!(!addon.directory.exists());
        assert_eq!(fs::read(unrelated.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn legacy_test_addons_are_listed_and_removable() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let addon = game.join("garrysmod/addons/gmpublished_model_test_previous");
        fs::create_dir_all(addon.join("models")).unwrap();
        fs::create_dir_all(addon.join("lua/autorun/server")).unwrap();
        fs::write(addon.join("models/previous.mdl"), b"model").unwrap();
        fs::write(addon.join("lua/autorun/server/test.lua"), b"legacy").unwrap();
        let manifest = serde_json::json!({
            "version": 1,
            "files": [
                {"path": "models/previous.mdl", "size": 5, "crc32": crc32fast::hash(b"model")},
                {"path": "lua/autorun/server/test.lua", "size": 6, "crc32": crc32fast::hash(b"legacy")}
            ]
        });
        fs::write(
            addon.join(LEGACY_MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let models = list_models(&game).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "models/previous.mdl");
        assert!(!models[0].modified);
        remove_model(&game, &addon).unwrap();
        assert!(!addon.exists());
    }

    #[test]
    fn shared_materials_must_have_identical_contents() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let mut first = export(root.path(), "models/first.mdl", b"first");
        let material = first.directory.join("materials/shared.vmt");
        fs::create_dir_all(material.parent().unwrap()).unwrap();
        fs::write(&material, b"same").unwrap();
        first.files.push(material.clone());
        let installed = install_model(&game, &first).unwrap();
        let mut second = export(root.path(), "models/second.mdl", b"second");
        second.files.push(material.clone());
        let second_installed = install_model(&game, &second).unwrap();
        fs::write(material, b"different").unwrap();
        assert!(
            install_model(&game, &second)
                .unwrap_err()
                .contains("conflicts")
        );
        assert_eq!(
            fs::read(installed.directory.join("materials/shared.vmt")).unwrap(),
            b"same"
        );
        assert_eq!(
            fs::read(second_installed.directory.join("materials/shared.vmt")).unwrap(),
            b"same"
        );
    }

    #[test]
    fn invalid_sources_and_manifest_traversal_do_not_replace_an_addon() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let source = export(root.path(), "models/prop.mdl", b"original");
        let addon = install_model(&game, &source).unwrap();
        fs::remove_file(&source.files[0]).unwrap();
        assert!(install_model(&game, &source).is_err());
        assert_eq!(
            fs::read(addon.directory.join("models/prop.mdl")).unwrap(),
            b"original"
        );
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(addon.directory.join(MANIFEST)).unwrap()).unwrap();
        manifest.files[0].path = "../outside".to_owned();
        fs::write(
            addon.directory.join(MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(list_models(&game).unwrap().is_empty());
        assert!(remove_model(&game, &addon.directory).is_err());
    }

    #[test]
    fn loose_model_files_are_installed_under_models() {
        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let mut source = export(root.path(), "prop.mdl", b"model");
        let vertices = source.directory.join("prop.vvd");
        fs::write(&vertices, b"vertices").unwrap();
        source.files.push(vertices);
        let addon = install_model(&game, &source).unwrap();
        assert_eq!(addon.model, "models/prop.mdl");
        assert_eq!(
            fs::read(addon.directory.join("models/prop.mdl")).unwrap(),
            b"model"
        );
        assert_eq!(
            fs::read(addon.directory.join("models/prop.vvd")).unwrap(),
            b"vertices"
        );
        assert!(!list_models(&game).unwrap()[0].modified);
    }

    #[cfg(unix)]
    #[test]
    fn links_in_sources_addons_and_addon_roots_are_never_followed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let game = game(root.path());
        let source = export(root.path(), "models/prop.mdl", b"original");
        let addon = install_model(&game, &source).unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"safe").unwrap();
        symlink(&outside, addon.directory.join("link")).unwrap();
        assert!(list_models(&game).unwrap()[0].modified);
        assert!(remove_model(&game, &addon.directory).is_err());
        fs::remove_file(addon.directory.join("link")).unwrap();
        let root_link = game.join("garrysmod/addons/gmpublished_model_link");
        symlink(&addon.directory, &root_link).unwrap();
        assert!(remove_model(&game, &root_link).is_err());
        assert_eq!(list_models(&game).unwrap().len(), 1);
        fs::remove_file(&source.files[0]).unwrap();
        symlink(outside.join("keep"), &source.files[0]).unwrap();
        assert!(install_model(&game, &source).is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"safe");
    }
}
