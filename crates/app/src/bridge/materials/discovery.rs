use super::{
    ContentPath, LooseSourceDir, sibling_gma::SiblingGmaPath, source_discovery::Discovery,
};
use gmpublished_backend::source_discovery::{SourceCategory, SourceDiscoveryError};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn discover_loose_source_dirs_checked(
    gmod_dir: &Path,
    explicit: &[PathBuf],
    discovery: &mut Discovery<'_>,
) -> Result<Vec<LooseSourceDir>, SourceDiscoveryError> {
    discovery.check()?;
    let mut dirs = Vec::new();
    let garrysmod = gmod_dir.join("garrysmod");
    push_loose_source_dir(&mut dirs, garrysmod.clone(), discovery)?;
    for addon in discovery.children(&garrysmod.join("addons"))? {
        if discovery.child_dir(&addon)? {
            push_loose_source_dir(&mut dirs, addon.path, discovery)?;
        }
    }
    push_loose_source_dir(&mut dirs, garrysmod.join("download"), discovery)?;
    for path in explicit {
        push_loose_source_dir(&mut dirs, path.clone(), discovery)?;
    }
    Ok(dirs)
}
fn push_loose_source_dir(
    dirs: &mut Vec<LooseSourceDir>,
    path: PathBuf,
    discovery: &mut Discovery<'_>,
) -> Result<(), SourceDiscoveryError> {
    if discovery.is_dir(&path)? {
        discovery.reserve(&path, dirs)?;
        dirs.push(LooseSourceDir::new(path));
    }
    Ok(())
}
pub(super) fn discover_mounted_game_dirs_checked(
    gmod_dir: &Path,
    explicit: &[PathBuf],
    discovery: &mut Discovery<'_>,
) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    discovery.check()?;
    let mut dirs = Vec::new();
    let own_common = gmod_dir.parent();
    if let Some(common) = own_common {
        push_mountable_game_dirs(common, gmod_dir, &mut dirs, discovery)?;
    }
    if own_common.is_some_and(is_steam_common_dir) {
        let own_canonical = own_common
            .map(|dir| discovery.budget.canonical_path(discovery.category, dir))
            .transpose()?
            .flatten();
        for common in discovery
            .budget
            .steam_library_common_dirs(&|| discovery.cancellation.is_cancelled())?
        {
            let canonical = discovery
                .budget
                .canonical_path(discovery.category, &common)?;
            if own_canonical
                .as_ref()
                .zip(canonical.as_ref())
                .is_some_and(|(own, other)| own == other)
            {
                continue;
            }
            push_mountable_game_dirs(&common, gmod_dir, &mut dirs, discovery)?;
        }
    }
    for path in explicit {
        if discovery.is_dir(path)? && !dirs.contains(path) {
            discovery.reserve(path, &mut dirs)?;
            dirs.push(path.clone());
        }
    }
    Ok(dirs)
}
fn push_mountable_game_dirs(
    common: &Path,
    gmod_dir: &Path,
    dirs: &mut Vec<PathBuf>,
    discovery: &mut Discovery<'_>,
) -> Result<(), SourceDiscoveryError> {
    for install in discovery.children(common)? {
        if !discovery.child_dir(&install)? || install.path.file_name() == gmod_dir.file_name() {
            continue;
        }
        for child in discovery.children(&install.path)? {
            if discovery.child_dir(&child)?
                && discovery
                    .budget
                    .metadata(discovery.category, &child.path.join("gameinfo.txt"))?
                    .is_some_and(|metadata| metadata.is_file())
                && !dirs.contains(&child.path)
            {
                discovery.reserve(&child.path, dirs)?;
                dirs.push(child.path);
            }
        }
    }
    Ok(())
}
pub(super) fn is_steam_common_dir(dir: &Path) -> bool {
    fn named(dir: &Path, name: &str) -> bool {
        dir.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
    }
    named(dir, "common")
        && dir
            .parent()
            .is_some_and(|parent| named(parent, "steamapps"))
}
pub(super) fn mount_cfg_dirs_checked(
    gmod_dir: &Path,
    discovery: &mut Discovery<'_>,
) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let path = gmod_dir.join("garrysmod/cfg/mount.cfg");
    let Some(bytes) =
        discovery
            .budget
            .configuration(SourceCategory::MountedGames, &path, &|| {
                discovery.cancellation.is_cancelled()
            })?
    else {
        return Ok(Vec::new());
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(Vec::new());
    };
    let Ok(document) = vformats::keyvalues::parse(text, &vformats::Limits::default()) else {
        return Ok(Vec::new());
    };
    let mut dirs = Vec::new();
    for value in document
        .blocks("mountcfg")
        .flat_map(|block| &block.pairs)
        .filter_map(|pair| pair.value.as_str())
    {
        discovery.check()?;
        let path = Path::new(value);
        discovery.reserve(path, &mut dirs)?;
        dirs.push(path.to_owned());
    }
    Ok(dirs)
}
#[cfg(test)]
pub(super) fn discover_sibling_gma_paths(gmod_dir: &Path) -> Vec<SiblingGmaPath> {
    let limits = super::SourceDiscoveryLimits::default();
    let mut budget = gmpublished_backend::source_discovery::SourceDiscoveryBudget::new(
        limits,
        limits.metadata_bytes,
    );
    discover_sibling_gma_paths_checked(
        gmod_dir,
        &mut Discovery {
            budget: &mut budget,
            cancellation: &crate::preparation::PreparationCancellation::default(),
            category: SourceCategory::SiblingGma,
        },
    )
    .expect("fixture discovery")
}
pub(super) fn discover_sibling_gma_paths_checked(
    gmod_dir: &Path,
    discovery: &mut Discovery<'_>,
) -> Result<Vec<SiblingGmaPath>, SourceDiscoveryError> {
    discovery.check()?;
    let mut paths = Vec::new();
    let workshop = gmod_dir.join("../../workshop/content/4000");
    let workshop = match fs::canonicalize(&workshop) {
        Ok(path) => Some(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(
                gmpublished_backend::source_discovery::SourceDiscoveryFailure {
                    category: SourceCategory::SiblingGma,
                    path: workshop,
                    kind: super::source_discovery::DiscoveryFailureKind::Io {
                        kind: format!("{:?}", error.kind()),
                        message: error.to_string(),
                    },
                }
                .into(),
            );
        }
    };
    if let Some(workshop) = workshop {
        for item in discovery.children(&workshop)? {
            if !discovery.child_dir(&item)? {
                continue;
            }
            let children = discovery.children(&item.path)?;
            let mut has_plain = false;
            for child in &children {
                if is_plain_gma_path(&child.path) && discovery.child_file(child)? {
                    has_plain = true;
                }
            }
            for child in children {
                let selected = if has_plain {
                    is_plain_gma_path(&child.path)
                } else {
                    is_legacy_bin_path(&child.path)
                };
                if selected && discovery.child_file(&child)? {
                    discovery.reserve(&child.path, &mut paths)?;
                    paths.push(if has_plain {
                        SiblingGmaPath::plain(child.path)
                    } else {
                        SiblingGmaPath::legacy_bin(child.path)
                    });
                }
            }
        }
    }
    for child in discovery.children(&gmod_dir.join("garrysmod/addons"))? {
        if discovery.child_file(&child)? && is_plain_gma_path(&child.path) {
            discovery.reserve(&child.path, &mut paths)?;
            paths.push(SiblingGmaPath::plain(child.path));
        }
    }
    collect_download_gma_paths(
        &gmod_dir.join("garrysmod/download"),
        0,
        &mut paths,
        discovery,
    )?;
    Ok(paths)
}
fn collect_download_gma_paths(
    dir: &Path,
    depth: usize,
    paths: &mut Vec<SiblingGmaPath>,
    discovery: &mut Discovery<'_>,
) -> Result<(), SourceDiscoveryError> {
    discovery.check()?;
    if depth > 3 {
        return Ok(());
    }
    for child in discovery.children(dir)? {
        if discovery.child_file(&child)? && is_plain_gma_path(&child.path) {
            discovery.reserve(&child.path, paths)?;
            paths.push(SiblingGmaPath::plain(child.path));
        } else if discovery.child_dir(&child)? {
            collect_download_gma_paths(&child.path, depth + 1, paths, discovery)?;
        }
    }
    Ok(())
}

fn is_plain_gma_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gma"))
}

fn is_legacy_bin_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
}

pub(super) fn material_paths(material_dirs: &[String], material_name: &str) -> Vec<ContentPath> {
    let Some(name) = normalize_material_name(material_name) else {
        return Vec::new();
    };
    let dirs = normalized_material_dirs(material_dirs);
    let mut paths = Vec::with_capacity(dirs.len() + 1);
    for dir in dirs {
        let path = if dir.is_empty() {
            format!("materials/{name}.vmt")
        } else {
            format!("materials/{dir}/{name}.vmt")
        };
        if let Some(path) = ContentPath::new(&path) {
            push_unique(&mut paths, path);
        }
    }
    if let Some(depatched) = cubemap_depatched_material_name(&name)
        && let Some(path) = ContentPath::new(&format!("materials/{depatched}.vmt"))
    {
        push_unique(&mut paths, path);
    }
    paths
}

pub(super) fn texture_path(base_texture: &str) -> Option<ContentPath> {
    let texture = normalize_texture_name(base_texture)?;
    ContentPath::new(&format!("materials/{texture}.vtf"))
}

fn normalized_material_dirs(material_dirs: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();
    if material_dirs.is_empty() {
        dirs.push(String::new());
        return dirs;
    }

    for dir in material_dirs {
        let normalized = normalize_source_path(dir, None).unwrap_or_default();
        push_unique(&mut dirs, normalized);
    }
    if dirs.is_empty() {
        dirs.push(String::new());
    }
    dirs
}

pub(super) fn normalize_material_name(material_name: &str) -> Option<String> {
    normalize_source_path(material_name, Some(".vmt"))
}

pub(super) fn normalize_texture_name(texture_name: &str) -> Option<String> {
    normalize_source_path(texture_name, Some(".vtf"))
}

fn cubemap_depatched_material_name(material_name: &str) -> Option<String> {
    let without_maps = strip_prefix_ascii_case(material_name, "maps/")?;
    let (_, original) = without_maps.split_once('/')?;
    let suffix_start = cubemap_suffix_start(original)?;
    let original = &original[..suffix_start];
    (!original.is_empty()).then(|| original.to_owned())
}

fn cubemap_suffix_start(value: &str) -> Option<usize> {
    let z_start = trailing_group_start(value)?;
    let y_start = trailing_group_start(value.get(..z_start)?)?;
    let x_start = trailing_group_start(value.get(..y_start)?)?;
    Some(x_start)
}

fn trailing_group_start(value: &str) -> Option<usize> {
    let (prefix, group) = value.rsplit_once('_')?;
    (is_signed_integer(group) && !prefix.is_empty()).then_some(prefix.len())
}

fn is_signed_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn normalize_source_path(path: &str, extension: Option<&str>) -> Option<String> {
    let mut path = path
        .trim()
        .trim_matches(|character| matches!(character, '/' | '\\'));
    if path
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("materials"))
        && let Some(rest) = path.get(9..)
        && let Some(stripped) = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\'))
    {
        path = stripped;
    }
    if let Some(extension) = extension {
        let suffix_start = path.len().saturating_sub(extension.len());
        if let (Some(prefix), Some(suffix)) = (path.get(..suffix_start), path.get(suffix_start..))
            && suffix.eq_ignore_ascii_case(extension)
        {
            path = prefix;
        }
    }

    let mut normalized = String::with_capacity(path.len());
    for segment in path.split(['/', '\\']) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(segment);
    }
    if normalized.is_empty() && extension.is_some() {
        return None;
    }
    normalized.make_ascii_lowercase();
    Some(normalized)
}

pub(super) fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}
