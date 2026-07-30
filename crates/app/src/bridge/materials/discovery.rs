//! Where Source content lives on disk, and how its paths are spelled.
//!
//! Two halves that always travel together: walking a Garry's Mod install for
//! loose directories and sibling GMAs, and normalising the material/texture
//! names a VMT refers to into the archive paths that can be looked up.

use std::fs;
use std::path::{Path, PathBuf};

use super::sibling_gma::SiblingGmaPath;
use super::{ContentPath, LooseSourceDir};

pub(super) fn discover_loose_source_dirs(gmod_dir: &Path) -> Vec<LooseSourceDir> {
    let mut dirs = Vec::new();
    let garrysmod = gmod_dir.join("garrysmod");
    push_loose_source_dir(&mut dirs, garrysmod.clone());

    let addons = garrysmod.join("addons");
    for addon in sorted_children(&addons) {
        if addon.is_dir() {
            push_loose_source_dir(&mut dirs, addon.path);
        }
    }

    push_loose_source_dir(&mut dirs, garrysmod.join("download"));
    dirs
}

fn push_loose_source_dir(dirs: &mut Vec<LooseSourceDir>, path: PathBuf) {
    if path.is_dir() {
        dirs.push(LooseSourceDir::new(path));
    }
}

pub(super) fn discover_sibling_gma_paths(gmod_dir: &Path) -> Vec<SiblingGmaPath> {
    let mut paths = Vec::new();
    if let Ok(workshop_dir) = fs::canonicalize(gmod_dir.join("../../workshop/content/4000")) {
        for workshop_item in sorted_children(&workshop_dir) {
            if workshop_item.is_dir() {
                let children = sorted_children(&workshop_item.path);
                let plain_gmas = children
                    .iter()
                    .filter(|child| child.is_file() && is_plain_gma_path(&child.path))
                    .map(|child| child.path.clone())
                    .collect::<Vec<_>>();
                if plain_gmas.is_empty() {
                    paths.extend(
                        children
                            .into_iter()
                            .filter(|child| child.is_file() && is_legacy_bin_path(&child.path))
                            .map(|child| SiblingGmaPath::legacy_bin(child.path)),
                    );
                } else {
                    paths.extend(plain_gmas.into_iter().map(SiblingGmaPath::plain));
                }
            }
        }
    }

    for child in sorted_children(&gmod_dir.join("garrysmod/addons")) {
        if child.is_file() && is_plain_gma_path(&child.path) {
            paths.push(SiblingGmaPath::plain(child.path));
        }
    }

    collect_download_gma_paths(&gmod_dir.join("garrysmod/download"), 0, &mut paths);

    paths
}

fn collect_download_gma_paths(dir: &Path, depth: usize, paths: &mut Vec<SiblingGmaPath>) {
    if depth > 3 {
        return;
    }
    for child in sorted_children(dir) {
        if child.is_file() && is_plain_gma_path(&child.path) {
            paths.push(SiblingGmaPath::plain(child.path));
        } else if child.is_dir() {
            collect_download_gma_paths(&child.path, depth + 1, paths);
        }
    }
}

struct SortedChild {
    path: PathBuf,
    file_type: Option<fs::FileType>,
}

impl SortedChild {
    fn is_file(&self) -> bool {
        self.file_type.as_ref().map_or_else(
            || self.path.is_file(),
            |file_type| {
                if file_type.is_symlink() {
                    self.path.is_file()
                } else {
                    file_type.is_file()
                }
            },
        )
    }

    fn is_dir(&self) -> bool {
        self.file_type.as_ref().map_or_else(
            || self.path.is_dir(),
            |file_type| {
                if file_type.is_symlink() {
                    self.path.is_dir()
                } else {
                    file_type.is_dir()
                }
            },
        )
    }
}

fn sorted_children(dir: &Path) -> Vec<SortedChild> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut children = read_dir
        .filter_map(Result::ok)
        .map(|entry| SortedChild {
            file_type: entry.file_type().ok(),
            path: entry.path(),
        })
        .collect::<Vec<_>>();
    children.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    children
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

fn normalize_material_name(material_name: &str) -> Option<String> {
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
