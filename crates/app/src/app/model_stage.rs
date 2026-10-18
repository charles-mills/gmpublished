use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::bridge::archive::PreviewArchiveSource;
use crate::bridge::content_path::normalize_archive_path;
use crate::generation::Generation;
use crate::media::preview_model::PreviewRequest;

use super::model_manager;

mod installed;
pub(super) use installed::{install_model, list_models, remove_model};

pub(super) fn prepare_imported_model_preview(
    generation: Generation,
    source: &Path,
) -> Result<(PreviewRequest, model_manager::ExportResult), String> {
    if !source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mdl"))
    {
        return Err("Select an MDL model file".to_owned());
    }
    let source = std::fs::canonicalize(source).map_err(|error| error.to_string())?;
    let parent = source
        .parent()
        .ok_or_else(|| "The model has no parent directory".to_owned())?;
    let root = source
        .ancestors()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("models"))
        })
        .and_then(Path::parent)
        .unwrap_or(parent)
        .to_path_buf();
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let names = [
        "mdl", "vvd", "dx90.vtx", "dx80.vtx", "sw.vtx", "vtx", "phy", "ani",
    ]
    .map(|suffix| format!("{stem}.{suffix}"));
    let mut files = Vec::new();
    for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if names
            .iter()
            .any(|name| entry.file_name().eq_ignore_ascii_case(name))
            && entry.path().is_file()
        {
            files.push(entry.path());
        }
    }
    if !root.join("gameinfo.txt").is_file() {
        collect_material_files(&root.join("materials"), 0, &mut files)?;
    }
    files.sort();
    let mut indexed = Vec::with_capacity(files.len());
    let mut bytes = 0_u64;
    for path in &files {
        let size = std::fs::metadata(path)
            .map_err(|error| error.to_string())?
            .len();
        bytes = bytes.saturating_add(size);
        indexed.push((asset_path(&root, path)?, size, path.clone()));
    }
    let mut content_hash = crc32fast::Hasher::new();
    for (name, size, path) in &indexed {
        content_hash.update(&(name.len() as u64).to_le_bytes());
        content_hash.update(name.as_bytes());
        content_hash.update(&size.to_le_bytes());
        content_hash.update(&file_crc32(path)?.to_le_bytes());
    }
    let entry_path = asset_path(&root, &source)?;
    let size_bytes = std::fs::metadata(&source)
        .map_err(|error| error.to_string())?
        .len();
    let request = PreviewRequest {
        cancellation: Default::default(),
        map_texture_quality: Default::default(),
        request_id: generation,
        archive: PreviewArchiveSource::from_folder(indexed),
        entry_path,
        display_name: source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        size_bytes,
        crc32: content_hash.finalize(),
        bypass_size_limits: false,
    };
    Ok((
        request,
        model_manager::ExportResult {
            directory: root,
            files,
            bytes,
            warnings: Vec::new(),
            compiled: true,
        },
    ))
}

fn collect_material_files(
    directory: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if depth > 16 {
        return Err("The material folder is nested too deeply".to_owned());
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            collect_material_files(&entry.path(), depth + 1, files)?;
        } else if kind.is_file()
            && entry.path().extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("vmt") || extension.eq_ignore_ascii_case("vtf")
            })
        {
            if files.len() >= 20_000 {
                return Err("The material folder contains too many files".to_owned());
            }
            files.push(entry.path());
        }
    }
    Ok(())
}

fn asset_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    let relative = if !relative.contains('/') {
        format!("models/{relative}")
    } else {
        relative
    };
    normalize_archive_path(&relative).ok_or_else(|| "Invalid model asset path".to_owned())
}

pub(super) fn list_maps(gmod_dir: &Path) -> Vec<PathBuf> {
    let Some(game) = child_directory(gmod_dir, "garrysmod") else {
        return Vec::new();
    };
    let mut maps = Vec::new();
    collect_maps(&game.join("maps"), &mut maps);
    collect_maps(&game.join("download/maps"), &mut maps);
    if let Ok(addons) = std::fs::read_dir(game.join("addons")) {
        for addon in addons.flatten() {
            collect_maps(&addon.path().join("maps"), &mut maps);
        }
    }
    maps.sort_by_cached_key(|path| {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        (name != "gm_construct.bsp", name, path.clone())
    });
    maps.dedup();
    maps
}

pub(super) fn prepare_map_preview(
    generation: Generation,
    path: &Path,
) -> Result<PreviewRequest, String> {
    if !is_bsp(path) {
        return Err("Select a BSP map file".to_owned());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Select a BSP map file".to_owned());
    }
    let display_name = path
        .file_name()
        .ok_or_else(|| "The map has no file name".to_owned())?
        .to_string_lossy()
        .into_owned();
    let entry_path = normalize_archive_path(&format!("maps/{display_name}"))
        .ok_or_else(|| "The map file name is invalid".to_owned())?;
    drop(file);
    Ok(PreviewRequest {
        cancellation: Default::default(),
        map_texture_quality: Default::default(),
        request_id: generation,
        archive: PreviewArchiveSource::from_folder([(
            entry_path.clone(),
            metadata.len(),
            path.to_path_buf(),
        )]),
        entry_path,
        display_name,
        size_bytes: metadata.len(),
        crc32: file_crc32(path)?,
        bypass_size_limits: false,
    })
}

fn file_crc32(path: &Path) -> Result<u32, String> {
    let mut hash = crc32fast::Hasher::new();
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize())
}

fn child_directory(parent: &Path, name: &str) -> Option<PathBuf> {
    let direct = parent.join(name);
    if direct.is_dir() {
        return Some(direct);
    }
    std::fs::read_dir(parent).ok()?.flatten().find_map(|entry| {
        (entry.file_name().eq_ignore_ascii_case(name) && entry.path().is_dir())
            .then(|| entry.path())
    })
}

fn collect_maps(directory: &Path, maps: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(directory) {
        maps.extend(entries.flatten().filter_map(|entry| {
            let path = entry.path();
            (is_bsp(&path) && path.is_file()).then_some(path)
        }));
    }
}

fn is_bsp(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bsp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_base_and_loose_addon_maps_with_construct_first() {
        let root = tempfile::tempdir().expect("root");
        let game = root.path().join("GarrysMod");
        let expected = [
            game.join("maps/gm_construct.bsp"),
            game.join("addons/example/maps/custom.BSP"),
            game.join("download/maps/downloaded.bsp"),
            game.join("maps/gm_flatgrass.bsp"),
        ];
        for path in &expected {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, b"VBSP").expect("map");
        }
        std::fs::write(game.join("maps/readme.txt"), b"text").expect("non-map");
        std::fs::create_dir(game.join("maps/directory.bsp")).expect("directory");
        assert_eq!(list_maps(root.path()), expected);
    }

    #[test]
    fn preview_indexes_custom_map_with_real_content_identity() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("Custom.BSP");
        std::fs::write(&path, b"VBSP fixture").expect("map");
        let generation = Generation::INITIAL.next();
        let request = prepare_map_preview(generation, &path).expect("preview");
        assert_eq!(request.request_id, generation);
        assert_eq!(request.entry_path, "maps/custom.bsp");
        assert_eq!(request.display_name, "Custom.BSP");
        assert_eq!(request.crc32, crc32fast::hash(b"VBSP fixture"));
        assert_eq!(request.size_bytes, 12);
        assert_eq!(
            request
                .archive
                .entry_bytes(&request.entry_path)
                .expect("bytes"),
            b"VBSP fixture"
        );
        assert!(!request.archive.supports_entry_extraction());
    }

    #[test]
    fn missing_install_or_non_map_does_not_produce_a_preview() {
        let root = tempfile::tempdir().expect("root");
        assert!(list_maps(root.path()).is_empty());
        let path = root.path().join("script.lua");
        std::fs::write(&path, b"print('test')").expect("file");
        assert!(prepare_map_preview(Generation::INITIAL, &path).is_err());
    }

    #[test]
    fn imported_model_indexes_case_insensitive_companions_and_addon_materials() {
        let root = tempfile::tempdir().expect("root");
        let model = root.path().join("models/vehicles/Car.MDL");
        let companion = root.path().join("models/vehicles/CAR.DX90.VTX");
        let material = root.path().join("materials/vehicles/paint.VMT");
        for path in [&model, &companion, &material] {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, b"fixture").expect("asset");
        }
        let unrelated = root.path().join("models/vehicles/other.mdl");
        std::fs::write(&unrelated, b"other").expect("other model");
        let (request, result) =
            prepare_imported_model_preview(Generation::INITIAL, &model).expect("preview");
        assert_eq!(request.entry_path, "models/vehicles/car.mdl");
        assert!(
            request
                .archive
                .entry("models/vehicles/car.dx90.vtx")
                .is_ok()
        );
        assert!(
            request
                .archive
                .entry("materials/vehicles/paint.vmt")
                .is_ok()
        );
        assert!(request.archive.entry("models/vehicles/other.mdl").is_err());
        assert_eq!(result.files.len(), 3);
        assert_eq!(
            result.directory,
            std::fs::canonicalize(root.path()).expect("canonical root")
        );
    }

    #[test]
    fn loose_model_gets_virtual_models_directory_without_scanning_game_materials() {
        let root = tempfile::tempdir().expect("root");
        let model = root.path().join("prop.mdl");
        std::fs::write(&model, b"model").expect("model");
        std::fs::write(root.path().join("gameinfo.txt"), b"game").expect("game marker");
        std::fs::create_dir(root.path().join("materials")).expect("materials");
        std::fs::write(root.path().join("materials/huge.vtf"), b"texture").expect("game texture");
        let (request, result) =
            prepare_imported_model_preview(Generation::INITIAL, &model).expect("preview");
        assert_eq!(request.entry_path, "models/prop.mdl");
        assert!(request.archive.entry("models/prop.mdl").is_ok());
        assert_eq!(
            result.files,
            [std::fs::canonicalize(model).expect("canonical model")]
        );
    }
    #[test]
    fn imported_material_edit_invalidates_preview_identity() {
        let root = tempfile::tempdir().expect("root");
        let model = root.path().join("models/prop.mdl");
        let material = root.path().join("materials/paint.vmt");
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::create_dir_all(material.parent().unwrap()).unwrap();
        std::fs::write(&model, b"model").unwrap();
        std::fs::write(&material, b"white").unwrap();
        let (before, _) = prepare_imported_model_preview(Generation::INITIAL, &model).unwrap();
        std::fs::write(&material, b"black").unwrap();
        let (after, _) = prepare_imported_model_preview(Generation::INITIAL, &model).unwrap();
        assert_ne!(before.crc32, after.crc32);
        assert_eq!(before.size_bytes, after.size_bytes);
    }
}
