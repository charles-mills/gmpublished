use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bridge::archive::PreviewArchiveSource;
use crate::bridge::content_path::normalize_archive_path;
use crate::generation::Generation;
use crate::media::preview_model::PreviewRequest;

use super::model_manager;

const TEST_ADDON_PREFIX: &str = "gmpublished_model_test_";
const TEST_MANIFEST: &str = "gmpublished-model-test.json";

#[derive(Clone, Debug)]
pub(super) struct EngineTestLaunch {
    pub url: String,
    pub addon_directory: PathBuf,
    pub console_command: String,
}

#[derive(Deserialize, Serialize)]
struct TestManifest {
    version: u32,
    files: Vec<TestFile>,
}

#[derive(Deserialize, Serialize)]
struct TestFile {
    path: String,
    size: u64,
    crc32: u32,
}

pub(super) fn prepare_engine_test(
    gmod_dir: &Path,
    result: &model_manager::ExportResult,
    map: &str,
    skin: usize,
    bodygroups: &[usize],
    placement: Option<([f32; 3], [f32; 3])>,
) -> Result<EngineTestLaunch, String> {
    if !result.compiled {
        return Err("Compile a model before testing it in Garry's Mod".to_owned());
    }
    if map.is_empty()
        || map.len() > 128
        || !map
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"_-".contains(&character))
    {
        return Err("The map name is invalid".to_owned());
    }
    if skin > u16::MAX as usize
        || bodygroups.len() > 1024
        || bodygroups.iter().any(|choice| *choice > u16::MAX as usize)
    {
        return Err("The skin or bodygroup selection is invalid".to_owned());
    }
    if placement.is_some_and(|(position, angles)| {
        position
            .into_iter()
            .chain(angles)
            .any(|value| !value.is_finite())
    }) {
        return Err("The model placement is invalid".to_owned());
    }
    let game = child_directory(gmod_dir, "garrysmod")
        .ok_or_else(|| "Garry's Mod is not installed at the selected location".to_owned())?;
    let addons = child_directory(&game, "addons")
        .ok_or_else(|| "The Garry's Mod addons directory was not found".to_owned())?;
    if !list_maps(gmod_dir).iter().any(|path| {
        path.file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case(map))
    }) {
        return Err("The selected map is not installed in Garry's Mod".to_owned());
    }
    let root = std::fs::canonicalize(&result.directory).map_err(|error| error.to_string())?;
    let mut assets = std::collections::BTreeMap::new();
    for path in &result.files {
        let relative = path
            .strip_prefix(&result.directory)
            .map_err(|_| "A model asset is outside the export directory".to_owned())?;
        validate_relative_file(&root, relative)?;
        let name = asset_path(&result.directory, path)?;
        if !valid_test_asset(&name) {
            return Err(format!("Unsupported model test asset: {name}"));
        }
        if assets.insert(name.clone(), root.join(relative)).is_some() {
            return Err(format!("Duplicate model test asset: {name}"));
        }
    }
    let model = assets
        .keys()
        .find(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mdl"))
        })
        .cloned()
        .ok_or_else(|| "The export contains no model file".to_owned())?;
    let previous = previous_test_addons(&addons)?;
    let stage = tempfile::Builder::new()
        .prefix(TEST_ADDON_PREFIX)
        .tempdir_in(&addons)
        .map_err(|error| error.to_string())?;
    let token = test_id(stage.path());
    let console_command = format!("gmpublished_test_{token}");
    let mut manifest = TestManifest {
        version: 1,
        files: Vec::with_capacity(assets.len() + 2),
    };
    for (name, source) in assets {
        let target = stage.path().join(&name);
        std::fs::create_dir_all(target.parent().expect("asset parent"))
            .map_err(|error| error.to_string())?;
        std::fs::copy(&source, &target).map_err(|error| error.to_string())?;
        manifest.files.push(test_file(stage.path(), &target)?);
    }
    let lua = engine_test_lua(&model, map, skin, bodygroups, placement, &console_command);
    let lua_path = stage.path().join(format!("lua/autorun/server/{token}.lua"));
    std::fs::create_dir_all(lua_path.parent().expect("Lua parent"))
        .map_err(|error| error.to_string())?;
    std::fs::write(&lua_path, lua).map_err(|error| error.to_string())?;
    manifest.files.push(test_file(stage.path(), &lua_path)?);
    let addon_json = stage.path().join("addon.json");
    std::fs::write(
        &addon_json,
        br#"{"title":"gmpublished Model Test","type":"model","tags":["build"]}"#,
    )
    .map_err(|error| error.to_string())?;
    manifest.files.push(test_file(stage.path(), &addon_json)?);
    std::fs::write(
        stage.path().join(TEST_MANIFEST),
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    for path in previous {
        let command = format!("gmpublished_test_{}", test_id(&path));
        std::fs::remove_dir_all(path).map_err(|error| error.to_string())?;
        cleanup_test_marker(&game, &command);
    }
    let addon_directory = stage.keep();
    let arguments = format!("+gamemode sandbox +maxplayers 1 +map {map}");
    let encoded = arguments.replace(' ', "%20").replace('+', "%2B");
    Ok(EngineTestLaunch {
        url: format!("steam://run/4000//{encoded}/"),
        addon_directory,
        console_command,
    })
}

fn test_id(addon: &Path) -> String {
    addon
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('-', "_")
}

fn cleanup_test_marker(game: &Path, command: &str) {
    let relative = Path::new("data/gmpublished_model_tests").join(format!("{command}.txt"));
    if validate_relative_file(game, &relative).is_ok()
        && let Ok(bytes) = std::fs::read(game.join(&relative))
        && bytes == command.as_bytes()
        && let Err(error) = std::fs::remove_file(game.join(relative))
    {
        log::debug!("failed to remove previous model test marker: {error}");
    }
}

fn valid_test_asset(name: &str) -> bool {
    let path = Path::new(name);
    if name.starts_with("materials/") {
        path.extension()
            .is_some_and(|extension| extension == "vmt" || extension == "vtf")
    } else if name.starts_with("models/") {
        path.extension().is_some_and(|extension| {
            ["mdl", "vvd", "vtx", "phy", "ani"]
                .iter()
                .any(|expected| extension == *expected)
        })
    } else {
        false
    }
}

fn validate_relative_file(root: &Path, relative: &Path) -> Result<(), String> {
    let mut current = root.to_path_buf();
    if relative.as_os_str().is_empty() {
        return Err("Invalid model asset path".to_owned());
    }
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err("A model asset is outside the export directory".to_owned());
        };
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Model test assets cannot contain symbolic links".to_owned());
        }
    }
    if !current.is_file() {
        return Err("A model test asset is not a file".to_owned());
    }
    Ok(())
}

fn test_file(root: &Path, path: &Path) -> Result<TestFile, String> {
    Ok(TestFile {
        path: path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/"),
        size: std::fs::metadata(path)
            .map_err(|error| error.to_string())?
            .len(),
        crc32: file_crc32(path)?,
    })
}

fn previous_test_addons(addons: &Path) -> Result<Vec<PathBuf>, String> {
    let mut previous = Vec::new();
    for entry in std::fs::read_dir(addons).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(TEST_ADDON_PREFIX)
            || !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
        {
            continue;
        }
        let root = entry.path();
        let marker = root.join(TEST_MANIFEST);
        let Ok(metadata) = std::fs::symlink_metadata(&marker) else {
            continue;
        };
        let changed = || {
            format!(
                "A previous model test addon was modified; move or remove it before testing again: {}",
                root.display()
            )
        };
        if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
            return Err(changed());
        }
        let manifest: TestManifest =
            serde_json::from_slice(&std::fs::read(&marker).map_err(|error| error.to_string())?)
                .map_err(|_| changed())?;
        if manifest.version != 1 || manifest.files.len() > 20_002 {
            return Err(changed());
        }
        let mut expected = std::collections::BTreeSet::from([TEST_MANIFEST.to_owned()]);
        for file in manifest.files {
            let relative = Path::new(&file.path);
            validate_relative_file(&root, relative).map_err(|_| changed())?;
            let actual = test_file(&root, &root.join(relative))?;
            if actual.size != file.size || actual.crc32 != file.crc32 {
                return Err(changed());
            }
            for parent in relative.ancestors().skip(1) {
                if !parent.as_os_str().is_empty() {
                    expected.insert(format!("{}/", parent.to_string_lossy().replace('\\', "/")));
                }
            }
            expected.insert(file.path);
        }
        let mut actual = std::collections::BTreeSet::new();
        collect_test_files(&root, &root, 0, &mut actual).map_err(|_| changed())?;
        if actual != expected {
            return Err(changed());
        }
        previous.push(root);
    }
    Ok(previous)
}

fn collect_test_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if depth > 32 || files.len() > 20_003 {
        return Err("Invalid model test addon".to_owned());
    }
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err("Invalid model test addon".to_owned());
        }
        if kind.is_dir() {
            files.insert(format!(
                "{}/",
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/")
            ));
            collect_test_files(root, &entry.path(), depth + 1, files)?;
        } else {
            files.insert(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn engine_test_lua(
    model: &str,
    map: &str,
    skin: usize,
    bodygroups: &[usize],
    placement: Option<([f32; 3], [f32; 3])>,
    command: &str,
) -> String {
    let model = lua_string(model);
    let map = lua_string(map);
    let marker = lua_string(&format!("gmpublished_model_tests/{command}.txt"));
    let command = lua_string(command);
    let positioning = placement.map_or_else(
        || "    local hit = util.TraceLine({start = ply:EyePos(), endpos = ply:EyePos() + ply:GetAimVector() * 2048, filter = ply})\n    ent:SetPos(hit.HitPos)\n    ent:SetAngles(Angle(0, ply:EyeAngles().y, 0))\n".to_owned(),
        |(position, angles)| format!(
            "    ent:SetPos(Vector({}, {}, {}))\n    ent:SetAngles(Angle({}, {}, {}))\n",
            position[0], position[1], position[2], angles[0], angles[1], angles[2],
        ),
    );
    let floor = if placement.is_none() {
        "    local minimum = ent:WorldSpaceAABB()\n    ent:SetPos(ent:GetPos() + Vector(0, 0, ent:GetPos().z - minimum.z + 4))\n"
    } else {
        ""
    };
    let mut groups = String::new();
    for (group, choice) in bodygroups.iter().enumerate() {
        use std::fmt::Write;
        let _ = writeln!(groups, "    ent:SetBodygroup({group}, {choice})");
    }
    format!(
        "local marker = {marker}\nlocal requested = not file.Exists(marker, \"DATA\")\nlocal function spawnModel(ply)\n    if not IsValid(ply) or not ply:Alive() then return false end\n    local model = {model}\n    local ent = ents.Create(util.IsValidProp(model) and \"prop_physics_override\" or \"prop_dynamic_override\")\n    if not IsValid(ent) then return true end\n    ent:SetModel(model)\n{positioning}    ent:Spawn()\n    ent:Activate()\n    if not IsValid(ent) then return true end\n{floor}    ent:SetSkin({skin})\n{groups}    local physics = ent:GetPhysicsObject()\n    if IsValid(physics) then physics:Wake() end\n    undo.Create(\"gmpublished Model Test\")\n    undo.AddEntity(ent)\n    undo.SetPlayer(ply)\n    undo.Finish()\n    return true\nend\nconcommand.Add({command}, function()\n    if game.SinglePlayer() and game.GetMap() == {map} then requested = true end\nend)\nhook.Add(\"StartCommand\", {command}, function(ply, cmd)\n    if not game.SinglePlayer() or game.GetMap() ~= {map} then return end\n    if requested and not cmd:IsForced() and spawnModel(ply) then\n        requested = false\n        file.CreateDir(\"gmpublished_model_tests\")\n        file.Write(marker, {command})\n    end\nend)\n"
    )
}

fn lua_string(value: &str) -> String {
    use std::fmt::Write;
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                for byte in character.to_string().bytes() {
                    let _ = write!(escaped, "\\{byte:03}");
                }
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

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

    fn engine_fixture(root: &Path) -> (PathBuf, model_manager::ExportResult) {
        let game = root.join("GarrysMod");
        std::fs::create_dir_all(game.join("garrysmod/addons")).expect("addons");
        std::fs::create_dir_all(game.join("garrysmod/maps")).expect("maps");
        std::fs::write(game.join("garrysmod/maps/gm_construct.bsp"), b"map").expect("map");
        let export = root.join("export");
        let model = export.join("models/test.mdl");
        let material = export.join("materials/test.vmt");
        for path in [&model, &material] {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, b"fixture").expect("asset");
        }
        (
            game,
            model_manager::ExportResult {
                directory: export,
                files: vec![model, material],
                bytes: 14,
                warnings: Vec::new(),
                compiled: true,
            },
        )
    }

    fn test_lua(addon: &Path) -> String {
        let path = std::fs::read_dir(addon.join("lua/autorun/server"))
            .expect("Lua directory")
            .next()
            .expect("Lua file")
            .expect("entry")
            .path();
        std::fs::read_to_string(path).expect("Lua")
    }

    #[test]
    fn engine_test_copies_only_export_assets_and_preserves_visual_selections() {
        let root = tempfile::tempdir().expect("root");
        let (game, result) = engine_fixture(root.path());
        let launch = prepare_engine_test(
            &game,
            &result,
            "gm_construct",
            2,
            &[0, 3],
            Some(([10.0, 20.0, 30.0], [0.0, 90.0, 0.0])),
        )
        .expect("engine test");
        assert!(
            launch
                .addon_directory
                .starts_with(game.join("garrysmod/addons"))
        );
        assert_eq!(
            std::fs::read(launch.addon_directory.join("models/test.mdl")).expect("model"),
            b"fixture"
        );
        assert_eq!(
            test_id(Path::new("gmpublished_model_test_AbC-XyZ")),
            "gmpublished_model_test_abc_xyz"
        );
        assert_eq!(
            launch.console_command,
            launch.console_command.to_ascii_lowercase()
        );
        let lua = test_lua(&launch.addon_directory);
        assert!(lua.contains(&format!(
            "gmpublished_model_tests/{}.txt",
            launch.console_command
        )));
        assert!(lua.contains("ent:SetSkin(2)"));
        assert!(lua.contains("ent:SetBodygroup(1, 3)"));
        assert!(lua.contains("ent:SetPos(Vector(10, 20, 30))"));
        assert!(lua.contains("ent:SetAngles(Angle(0, 90, 0))"));
        assert!(lua.contains("game.SinglePlayer()"));
        assert!(lua.contains(&launch.console_command));
        assert!(lua.contains("hook.Add(\"StartCommand\""));
        assert!(lua.contains("\"prop_physics_override\" or \"prop_dynamic_override\""));
        assert!(lua.contains("local requested = not file.Exists(marker, \"DATA\")"));
        assert!(lua.contains("file.Write(marker,"));
        assert!(launch.url.starts_with("steam://run/4000//"));
        assert!(launch.url.contains("%2Bmap%20gm_construct"));
        assert!(!launch.url.contains(&launch.console_command));
        assert!(!launch.url.contains(' '));
        assert_eq!(std::fs::read(&result.files[0]).expect("source"), b"fixture");
    }

    #[test]
    fn next_engine_test_removes_only_unchanged_app_owned_addons() {
        let root = tempfile::tempdir().expect("root");
        let (game, result) = engine_fixture(root.path());
        let unrelated = game.join("garrysmod/addons/my_addon");
        std::fs::create_dir(&unrelated).expect("unrelated addon");
        let first =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("first test");
        let second =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("second test");
        assert!(!first.addon_directory.exists());
        assert!(second.addon_directory.exists());
        assert!(unrelated.exists());
        assert!(test_lua(&second.addon_directory).contains("ply:GetAimVector() * 2048"));
        std::fs::write(second.addon_directory.join("custom.txt"), b"keep").expect("custom file");
        assert!(prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).is_err());
        assert_eq!(
            std::fs::read(second.addon_directory.join("custom.txt")).expect("preserved"),
            b"keep"
        );
    }

    #[test]
    fn modified_asset_or_extra_directory_prevents_test_addon_cleanup() {
        let root = tempfile::tempdir().expect("root");
        let (game, result) = engine_fixture(root.path());
        let first =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("first test");
        let model = first.addon_directory.join("models/test.mdl");
        std::fs::write(&model, b"changed").expect("modified asset");
        assert!(prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).is_err());
        assert!(first.addon_directory.exists());
        std::fs::write(&model, b"fixture").expect("restore asset");
        std::fs::create_dir(first.addon_directory.join("keep_directory")).expect("extra directory");
        assert!(prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).is_err());
        assert!(first.addon_directory.join("keep_directory").exists());
    }

    #[test]
    fn engine_test_marker_cleanup_preserves_modified_data() {
        let root = tempfile::tempdir().expect("root");
        let (game, result) = engine_fixture(root.path());
        let data = game.join("garrysmod/data/gmpublished_model_tests");
        std::fs::create_dir_all(&data).expect("data");
        let first =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("first test");
        let first_marker = data.join(format!("{}.txt", first.console_command));
        std::fs::write(&first_marker, &first.console_command).expect("first marker");
        let second =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("second test");
        assert!(!first_marker.exists());
        let second_marker = data.join(format!("{}.txt", second.console_command));
        std::fs::write(&second_marker, b"custom content").expect("modified marker");
        prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("third test");
        assert_eq!(
            std::fs::read(second_marker).expect("preserved"),
            b"custom content"
        );
    }

    #[test]
    fn unsafe_engine_input_fails_before_creating_an_addon() {
        let root = tempfile::tempdir().expect("root");
        let (game, mut result) = engine_fixture(root.path());
        let addons = game.join("garrysmod/addons");
        assert!(prepare_engine_test(&game, &result, "gm_construct;quit", 0, &[], None).is_err());
        assert!(prepare_engine_test(&game, &result, "missing", 0, &[], None).is_err());
        assert!(
            prepare_engine_test(
                &game,
                &result,
                "gm_construct",
                0,
                &[],
                Some(([f32::NAN; 3], [0.0; 3])),
            )
            .is_err()
        );
        let outside = root.path().join("outside.mdl");
        std::fs::write(&outside, b"outside").expect("outside model");
        result.files.push(outside);
        assert!(prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).is_err());
        assert_eq!(std::fs::read_dir(addons).expect("addons").count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_assets_cannot_follow_symbolic_links_outside_export() {
        let root = tempfile::tempdir().expect("root");
        let (game, mut result) = engine_fixture(root.path());
        let outside = root.path().join("outside.vtf");
        std::fs::write(&outside, b"outside").expect("outside asset");
        let link = result.directory.join("materials/link.vtf");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        result.files.push(link);
        assert!(prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).is_err());
        assert_eq!(
            std::fs::read_dir(game.join("garrysmod/addons"))
                .expect("addons")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_marker_cleanup_cannot_follow_data_directory_symbolic_links() {
        let root = tempfile::tempdir().expect("root");
        let (game, result) = engine_fixture(root.path());
        let first =
            prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("first test");
        let outside = root.path().join("outside_data");
        std::fs::create_dir(&outside).expect("outside data");
        let marker = outside.join(format!("{}.txt", first.console_command));
        std::fs::write(&marker, &first.console_command).expect("outside marker");
        let data = game.join("garrysmod/data");
        std::fs::create_dir(&data).expect("game data");
        std::os::unix::fs::symlink(&outside, data.join("gmpublished_model_tests"))
            .expect("data symlink");
        prepare_engine_test(&game, &result, "gm_construct", 0, &[], None).expect("second test");
        assert_eq!(
            std::fs::read(marker).expect("preserved marker"),
            first.console_command.as_bytes()
        );
    }

    #[test]
    fn lua_strings_escape_control_characters_and_quotes_without_losing_unicode() {
        assert_eq!(lua_string("model\"\\\n\0é"), "\"model\\\"\\\\\\n\\000é\"");
    }

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
