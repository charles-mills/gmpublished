use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LookupError {
    #[error("cannot read {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid relative path: {}", .0.display())]
    InvalidPath(PathBuf),
    #[error("path is ambiguous ignoring case: {}", .0.display())]
    Ambiguous(PathBuf),
    #[error("material directory entry limit exceeded")]
    EntryLimit,
    #[error("path points outside its source directory: {}", .0.display())]
    OutsideRoot(PathBuf),
}

/// Exact names win over case-insensitive matches. `root` must be canonical when it exists.
pub(crate) fn find_file(
    root: &Path,
    relative: &Path,
    allow_parent: bool,
    directory_limit: Option<usize>,
) -> Result<Option<PathBuf>, LookupError> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let name = match component {
            Component::CurDir if allow_parent => continue,
            Component::ParentDir if allow_parent && current != root => {
                current.pop();
                continue;
            }
            Component::Normal(name) => name,
            _ => return Err(LookupError::InvalidPath(relative.into())),
        };
        let exact = current.join(name);
        if exact.exists() {
            current = exact;
        } else {
            if !current.is_dir() {
                return Ok(None);
            }
            let entries = fs::read_dir(&current).map_err(|source| LookupError::Io {
                path: current.clone(),
                source,
            })?;
            let mut matched = None;
            for (index, entry) in entries.enumerate() {
                if directory_limit.is_some_and(|limit| index >= limit) {
                    return Err(LookupError::EntryLimit);
                }
                let entry = entry.map_err(|source| LookupError::Io {
                    path: current.clone(),
                    source,
                })?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
                {
                    if matched.is_some() {
                        return Err(LookupError::Ambiguous(exact));
                    }
                    matched = Some(entry.path());
                }
            }
            let Some(found) = matched else {
                return Ok(None);
            };
            current = found;
        }
        let real = fs::canonicalize(&current).map_err(|source| LookupError::Io {
            path: current.clone(),
            source,
        })?;
        if !real.starts_with(root) {
            return Err(LookupError::OutsideRoot(current));
        }
    }
    Ok(current.is_file().then_some(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_preserves_case_and_limits_parent_traversal_to_its_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Textures")).unwrap();
        let file = root.path().join("Textures/Paint.PNG");
        fs::write(&file, b"pixels").unwrap();
        assert_eq!(
            find_file(root.path(), Path::new("textures/paint.png"), false, None).unwrap(),
            Some(file.clone())
        );
        assert_eq!(
            find_file(
                root.path(),
                Path::new("Textures/../Textures/Paint.PNG"),
                true,
                None
            )
            .unwrap(),
            Some(file)
        );
        for allow_parent in [false, true] {
            assert!(matches!(
                find_file(root.path(), Path::new("../escape"), allow_parent, None),
                Err(LookupError::InvalidPath(_))
            ));
        }
        assert!(matches!(
            find_file(root.path(), Path::new("textures/paint.png"), false, Some(0)),
            Err(LookupError::EntryLimit)
        ));
        assert!(
            find_file(root.path(), Path::new("absent"), false, None)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_names_win_ambiguous_aliases_fail_and_symlinks_stay_contained() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Paint.PNG"), b"a").unwrap();
        fs::write(root.path().join("paint.png"), b"b").unwrap();
        assert_eq!(
            find_file(root.path(), Path::new("Paint.PNG"), false, None).unwrap(),
            Some(root.path().join("Paint.PNG"))
        );
        assert!(matches!(
            find_file(root.path(), Path::new("PAINT.PNG"), false, None),
            Err(LookupError::Ambiguous(_))
        ));
        fs::write(outside.path().join("secret"), b"private").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(matches!(
            find_file(root.path(), Path::new("escape/secret"), false, None),
            Err(LookupError::OutsideRoot(_))
        ));
    }
}
