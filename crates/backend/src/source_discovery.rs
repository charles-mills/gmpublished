use serde::Serialize;
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SourceDiscoveryLimits {
    pub entries: usize,
    pub metadata_bytes: u64,
    pub sources: usize,
    pub configuration_bytes: u64,
}
impl Default for SourceDiscoveryLimits {
    fn default() -> Self {
        Self {
            entries: 1_000_000,
            metadata_bytes: 256 * 1024 * 1024,
            sources: 4096,
            configuration_bytes: 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum SourceCategory {
    MountedGames,
    Loose,
    SiblingGma,
    GameVpk,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum DiscoveryResource {
    Entries,
    MetadataBytes,
    Sources,
    ConfigurationBytes,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum DiscoveryFailureKind {
    Limit {
        resource: DiscoveryResource,
        required: u64,
        limit: u64,
    },
    Io {
        kind: String,
        message: String,
    },
    Allocation,
    ArithmeticOverflow,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceDiscoveryFailure {
    pub category: SourceCategory,
    pub path: PathBuf,
    pub kind: DiscoveryFailureKind,
}
impl std::fmt::Display for SourceDiscoveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} source discovery at {}: {:?}",
            self.category,
            self.path.display(),
            self.kind
        )
    }
}
impl std::error::Error for SourceDiscoveryFailure {}
#[derive(Debug, thiserror::Error)]
pub enum SourceDiscoveryError {
    #[error("source discovery cancelled")]
    Cancelled,
    #[error(transparent)]
    Unavailable(#[from] SourceDiscoveryFailure),
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SourceDiscoveryStats {
    pub examined_entries: usize,
    pub planned_source_records: usize,
    pub cumulative_allocation_bytes: u64,
    pub planned_path_bytes: u64,
    pub working_reservation_bytes: u64,
    pub failures: Vec<SourceDiscoveryFailure>,
}
#[derive(Debug)]
pub struct SourceDiscoveryBudget {
    limits: SourceDiscoveryLimits,
    applied_metadata_bytes: u64,
    stats: SourceDiscoveryStats,
}
#[derive(Debug)]
pub struct SourceChild {
    pub path: PathBuf,
    pub file_type: fs::FileType,
}
impl SourceDiscoveryBudget {
    pub fn new(limits: SourceDiscoveryLimits, working_reservation_bytes: u64) -> Self {
        Self {
            limits,
            applied_metadata_bytes: limits.metadata_bytes.min(working_reservation_bytes),
            stats: SourceDiscoveryStats {
                working_reservation_bytes,
                ..Default::default()
            },
        }
    }
    pub fn stats(&self) -> &SourceDiscoveryStats {
        &self.stats
    }
    pub fn fail(&mut self, failure: SourceDiscoveryFailure) {
        self.stats.failures.push(failure);
    }
    fn error(
        category: SourceCategory,
        path: &Path,
        kind: DiscoveryFailureKind,
    ) -> SourceDiscoveryError {
        SourceDiscoveryFailure {
            category,
            path: path.to_owned(),
            kind,
        }
        .into()
    }
    fn io(category: SourceCategory, path: &Path, error: &io::Error) -> SourceDiscoveryError {
        Self::error(
            category,
            path,
            DiscoveryFailureKind::Io {
                kind: format!("{:?}", error.kind()),
                message: error.to_string(),
            },
        )
    }
    fn limit(
        category: SourceCategory,
        path: &Path,
        resource: DiscoveryResource,
        required: u64,
        limit: u64,
    ) -> SourceDiscoveryError {
        Self::error(
            category,
            path,
            DiscoveryFailureKind::Limit {
                resource,
                required,
                limit,
            },
        )
    }
    pub fn charge_metadata(
        &mut self,
        category: SourceCategory,
        path: &Path,
        bytes: u64,
    ) -> Result<(), SourceDiscoveryError> {
        let required = self
            .stats
            .cumulative_allocation_bytes
            .checked_add(bytes)
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        if required > self.applied_metadata_bytes {
            return Err(Self::limit(
                category,
                path,
                DiscoveryResource::MetadataBytes,
                required,
                self.applied_metadata_bytes,
            ));
        }
        self.stats.cumulative_allocation_bytes = required;
        Ok(())
    }
    pub fn check(cancelled: &impl Fn() -> bool) -> Result<(), SourceDiscoveryError> {
        if cancelled() {
            Err(SourceDiscoveryError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn canonical_path(
        &self,
        category: SourceCategory,
        path: &Path,
    ) -> Result<Option<PathBuf>, SourceDiscoveryError> {
        match fs::canonicalize(path) {
            Ok(path) => Ok(Some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Self::io(category, path, &error)),
        }
    }
    pub fn metadata(
        &self,
        category: SourceCategory,
        path: &Path,
    ) -> Result<Option<fs::Metadata>, SourceDiscoveryError> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Self::io(category, path, &error)),
        }
    }
    pub fn child_is_dir(
        &self,
        category: SourceCategory,
        child: &SourceChild,
    ) -> Result<bool, SourceDiscoveryError> {
        if child.file_type.is_symlink() {
            self.metadata(category, &child.path)
                .map(|value| value.is_some_and(|metadata| metadata.is_dir()))
        } else {
            Ok(child.file_type.is_dir())
        }
    }
    pub fn child_is_file(
        &self,
        category: SourceCategory,
        child: &SourceChild,
    ) -> Result<bool, SourceDiscoveryError> {
        if child.file_type.is_symlink() {
            self.metadata(category, &child.path)
                .map(|value| value.is_some_and(|metadata| metadata.is_file()))
        } else {
            Ok(child.file_type.is_file())
        }
    }
    pub fn sorted_children(
        &mut self,
        category: SourceCategory,
        dir: &Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Vec<SourceChild>, SourceDiscoveryError> {
        Self::check(cancelled)?;
        let read_dir = match fs::read_dir(dir) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Self::io(category, dir, &error)),
        };
        self.collect_children(category, dir, read_dir, cancelled)
    }
    fn collect_children(
        &mut self,
        category: SourceCategory,
        dir: &Path,
        read_dir: impl IntoIterator<Item = io::Result<fs::DirEntry>>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Vec<SourceChild>, SourceDiscoveryError> {
        let mut children = Vec::new();
        for entry in read_dir {
            Self::check(cancelled)?;
            let entry = entry.map_err(|error| Self::io(category, dir, &error))?;
            let count = self.stats.examined_entries.checked_add(1).ok_or_else(|| {
                Self::error(category, dir, DiscoveryFailureKind::ArithmeticOverflow)
            })?;
            if count > self.limits.entries {
                return Err(Self::limit(
                    category,
                    dir,
                    DiscoveryResource::Entries,
                    count as u64,
                    self.limits.entries as u64,
                ));
            }
            let name = entry.file_name();
            let bytes = dir
                .as_os_str()
                .len()
                .checked_add(1)
                .and_then(|value| value.checked_add(name.len()))
                .ok_or_else(|| {
                    Self::error(category, dir, DiscoveryFailureKind::ArithmeticOverflow)
                })?;
            self.charge_metadata(category, dir, bytes as u64)?;
            self.stats.examined_entries = count;
            if children.len() == children.capacity() {
                let next = children
                    .capacity()
                    .checked_mul(2)
                    .map(|value| value.max(4))
                    .ok_or_else(|| {
                        Self::error(category, dir, DiscoveryFailureKind::ArithmeticOverflow)
                    })?;
                let additional = next.checked_mul(size_of::<SourceChild>()).ok_or_else(|| {
                    Self::error(category, dir, DiscoveryFailureKind::ArithmeticOverflow)
                })?;
                self.charge_metadata(category, dir, additional as u64)?;
                children
                    .try_reserve_exact(next - children.len())
                    .map_err(|_| Self::error(category, dir, DiscoveryFailureKind::Allocation))?;
            }
            let path = dir.join(name);
            let file_type = entry
                .file_type()
                .map_err(|error| Self::io(category, &path, &error))?;
            children.push(SourceChild { path, file_type });
        }
        Self::check(cancelled)?;
        children.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(children)
    }
    pub fn admit_source(
        &mut self,
        category: SourceCategory,
        path: &Path,
    ) -> Result<(), SourceDiscoveryError> {
        self.admit_source_record(category, path, size_of::<PathBuf>())
    }
    pub fn admit_source_record(
        &mut self,
        category: SourceCategory,
        path: &Path,
        record_bytes: usize,
    ) -> Result<(), SourceDiscoveryError> {
        let count = self
            .stats
            .planned_source_records
            .checked_add(1)
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        if count > self.limits.sources {
            return Err(Self::limit(
                category,
                path,
                DiscoveryResource::Sources,
                count as u64,
                self.limits.sources as u64,
            ));
        }
        let bytes = path.as_os_str().len() as u64;
        let retained = self
            .stats
            .planned_path_bytes
            .checked_add(bytes)
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        let required = bytes
            .checked_add(record_bytes as u64)
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        self.charge_metadata(category, path, required)?;
        self.stats.planned_path_bytes = retained;
        self.stats.planned_source_records = count;
        Ok(())
    }
    pub fn reserve_source<T>(
        &mut self,
        category: SourceCategory,
        path: &Path,
        sources: &mut Vec<T>,
    ) -> Result<(), SourceDiscoveryError> {
        let next_capacity = if sources.len() == sources.capacity() {
            Some(
                sources
                    .capacity()
                    .checked_mul(2)
                    .map(|next| next.max(4))
                    .ok_or_else(|| {
                        Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow)
                    })?,
            )
        } else {
            None
        };
        let allocation_bytes = next_capacity
            .map_or(Some(0), |next| next.checked_mul(size_of::<T>()))
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        self.admit_source_record(category, path, allocation_bytes)?;
        if let Some(next) = next_capacity {
            sources
                .try_reserve_exact(next - sources.len())
                .map_err(|_| Self::error(category, path, DiscoveryFailureKind::Allocation))?;
        }
        Ok(())
    }
    pub fn steam_library_common_dirs(
        &mut self,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
        Self::check(cancelled)?;
        let steam = match steamlocate::SteamDir::locate() {
            Ok(steam) => steam,
            Err(steamlocate::Error::Io { inner, path }) => {
                return Err(Self::io(SourceCategory::MountedGames, &path, &inner));
            }
            Err(_) => return Ok(Vec::new()),
        };
        let config = steam.path().join("steamapps/libraryfolders.vdf");
        let Some(bytes) = self.configuration(SourceCategory::MountedGames, &config, cancelled)?
        else {
            return Ok(Vec::new());
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Ok(Vec::new());
        };
        if !steam_configuration_depth(text, vformats::Limits::default().max_kv_depth) {
            return Ok(Vec::new());
        }
        let Ok(document) = keyvalues_parser::parse(text).map(keyvalues_parser::Vdf::from) else {
            return Ok(Vec::new());
        };
        let Some(object) = document.value.get_obj() else {
            return Ok(Vec::new());
        };
        let mut dirs = Vec::new();
        for (key, values) in object.iter() {
            Self::check(cancelled)?;
            if key.parse::<u32>().is_err() {
                continue;
            }
            let Some(path) = values
                .first()
                .and_then(keyvalues_parser::Value::get_obj)
                .and_then(|object| object.get("path"))
                .and_then(|values| values.first())
                .and_then(keyvalues_parser::Value::get_str)
            else {
                return Ok(Vec::new());
            };
            let common = Path::new(path).join("steamapps/common");
            if self
                .metadata(SourceCategory::MountedGames, &common)?
                .is_some_and(|metadata| metadata.is_dir())
            {
                self.reserve_source(SourceCategory::MountedGames, &common, &mut dirs)?;
                dirs.push(common);
            }
        }
        Ok(dirs)
    }
    pub fn configuration(
        &mut self,
        category: SourceCategory,
        path: &Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Vec<u8>>, SourceDiscoveryError> {
        Self::check(cancelled)?;
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Self::io(category, path, &error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| Self::io(category, path, &error))?;
        if !metadata.is_file() {
            return Err(Self::io(
                category,
                path,
                &io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "mount configuration is not a regular file",
                ),
            ));
        }
        let size = metadata.len();
        if size > self.limits.configuration_bytes {
            return Err(Self::limit(
                category,
                path,
                DiscoveryResource::ConfigurationBytes,
                size,
                self.limits.configuration_bytes,
            ));
        }
        let record_bound = (size_of::<vformats::keyvalues::KvPair<'static>>() as u64)
            .checked_mul(2)
            .and_then(|records| records.checked_add(2))
            .and_then(|factor| factor.checked_mul(size))
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        self.charge_metadata(category, path, record_bound)?;
        let maximum = size
            .checked_add(1)
            .ok_or_else(|| Self::error(category, path, DiscoveryFailureKind::ArithmeticOverflow))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(
                usize::try_from(maximum)
                    .map_err(|_| Self::error(category, path, DiscoveryFailureKind::Allocation))?,
            )
            .map_err(|_| Self::error(category, path, DiscoveryFailureKind::Allocation))?;
        file.by_ref()
            .take(maximum)
            .read_to_end(&mut bytes)
            .map_err(|error| Self::io(category, path, &error))?;
        Self::check(cancelled)?;
        if bytes.len() as u64 != size {
            return Err(Self::io(
                category,
                path,
                &io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mount configuration changed during read",
                ),
            ));
        }
        Ok(Some(bytes))
    }
}

fn steam_configuration_depth(text: &str, maximum: usize) -> bool {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '/' if chars.peek() == Some(&'/') => {
                comment = true;
                chars.next();
            }
            '{' => {
                depth += 1;
                if depth > maximum {
                    return false;
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn budget(limits: SourceDiscoveryLimits) -> SourceDiscoveryBudget {
        SourceDiscoveryBudget::new(limits, limits.metadata_bytes)
    }
    #[test]
    fn enumeration_failure_rejects_the_complete_result() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("first"), []).unwrap();
        let first = fs::read_dir(root.path()).unwrap().next().unwrap();
        let mut budget = budget(SourceDiscoveryLimits::default());
        let error = budget
            .collect_children(
                SourceCategory::Loose,
                root.path(),
                [
                    first,
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                ],
                &|| false,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            SourceDiscoveryError::Unavailable(SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::Io { .. },
                ..
            })
        ));
        assert_eq!(budget.stats().examined_entries, 1);
        assert_eq!(budget.stats().planned_source_records, 0);
    }
    #[test]
    fn missing_directory_is_empty_but_non_directory_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let mut budget = budget(SourceDiscoveryLimits::default());
        assert!(
            budget
                .sorted_children(SourceCategory::Loose, &root.path().join("absent"), &|| {
                    false
                })
                .unwrap()
                .is_empty()
        );
        fs::write(root.path().join("file"), []).unwrap();
        assert!(matches!(
            budget.sorted_children(SourceCategory::Loose, &root.path().join("file"), &|| false),
            Err(SourceDiscoveryError::Unavailable(_))
        ));
    }
    #[test]
    fn complete_sort_and_count_limit_do_not_select_a_filesystem_prefix() {
        for names in [["z", "a", "m"], ["m", "a", "z"]] {
            let root = tempfile::tempdir().unwrap();
            for name in names {
                fs::write(root.path().join(name), []).unwrap();
            }
            let mut wide = budget(SourceDiscoveryLimits::default());
            let children = wide
                .sorted_children(SourceCategory::GameVpk, root.path(), &|| false)
                .unwrap();
            assert_eq!(
                children
                    .iter()
                    .map(|child| child.path.file_name().unwrap().to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["a", "m", "z"]
            );
            let mut strict = budget(SourceDiscoveryLimits {
                entries: 2,
                ..Default::default()
            });
            assert!(matches!(
                strict.sorted_children(SourceCategory::GameVpk, root.path(), &|| false),
                Err(SourceDiscoveryError::Unavailable(SourceDiscoveryFailure {
                    kind: DiscoveryFailureKind::Limit {
                        resource: DiscoveryResource::Entries,
                        required: 3,
                        limit: 2
                    },
                    ..
                }))
            ));
            assert_eq!(strict.stats().planned_source_records, 0);
        }
    }
    #[test]
    fn metadata_and_source_limits_precede_retained_records() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("one"), []).unwrap();
        let mut zero = budget(SourceDiscoveryLimits {
            metadata_bytes: 0,
            ..Default::default()
        });
        assert!(matches!(
            zero.sorted_children(SourceCategory::Loose, root.path(), &|| false),
            Err(SourceDiscoveryError::Unavailable(_))
        ));
        assert_eq!(zero.stats().cumulative_allocation_bytes, 0);
        let mut strict = budget(SourceDiscoveryLimits {
            sources: 1,
            ..Default::default()
        });
        let mut paths = Vec::<PathBuf>::new();
        strict
            .reserve_source(SourceCategory::SiblingGma, root.path(), &mut paths)
            .unwrap();
        paths.push(root.path().to_owned());
        let before = strict.stats().clone();
        assert!(
            strict
                .reserve_source(SourceCategory::SiblingGma, root.path(), &mut paths)
                .is_err()
        );
        assert_eq!(strict.stats(), &before);
        assert_eq!(paths.len(), 1);
    }
    #[test]
    fn configuration_rejects_declared_size_before_allocation_and_bounds_depth() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mount.cfg");
        fs::write(&path, [0; 32]).unwrap();
        let mut strict = budget(SourceDiscoveryLimits {
            configuration_bytes: 31,
            ..Default::default()
        });
        assert!(matches!(
            strict.configuration(SourceCategory::MountedGames, &path, &|| false),
            Err(SourceDiscoveryError::Unavailable(SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::Limit {
                    resource: DiscoveryResource::ConfigurationBytes,
                    required: 32,
                    limit: 31
                },
                ..
            }))
        ));
        assert_eq!(strict.stats().cumulative_allocation_bytes, 0);
        assert!(steam_configuration_depth(
            r#""path" "C:\\{\\folder" // {{{
        {}"#,
            2
        ));
        assert!(!steam_configuration_depth("{{{}}}", 2));
        assert!(matches!(
            strict.configuration(SourceCategory::MountedGames, &path, &|| true),
            Err(SourceDiscoveryError::Cancelled)
        ));
    }
    #[test]
    fn arithmetic_overflow_cannot_relax_an_unbounded_profile() {
        let mut budget = budget(SourceDiscoveryLimits {
            metadata_bytes: u64::MAX,
            ..Default::default()
        });
        budget.stats.cumulative_allocation_bytes = u64::MAX;
        assert!(matches!(
            budget.admit_source(SourceCategory::Loose, Path::new("a")),
            Err(SourceDiscoveryError::Unavailable(SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::ArithmeticOverflow,
                ..
            }))
        ));
        assert_eq!(budget.stats().planned_source_records, 0);
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;
    #[test]
    fn enumeration_declines_before_the_old_and_new_capacity_peak_exceeds_policy() {
        let root = tempfile::tempdir().unwrap();
        for name in ["0", "1", "2", "3", "4"] {
            fs::write(root.path().join(name), []).unwrap();
        }
        let path_bytes = 5 * (root.path().as_os_str().len() as u64 + 2);
        let limit = path_bytes + 12 * size_of::<SourceChild>() as u64 - 1;
        let mut budget = SourceDiscoveryBudget::new(
            SourceDiscoveryLimits {
                metadata_bytes: limit,
                ..Default::default()
            },
            limit,
        );
        assert!(matches!(
            budget.sorted_children(SourceCategory::Loose, root.path(), &|| false),
            Err(SourceDiscoveryError::Unavailable(SourceDiscoveryFailure {
                kind: DiscoveryFailureKind::Limit {
                    resource: DiscoveryResource::MetadataBytes,
                    ..
                },
                ..
            }))
        ));
        assert_eq!(budget.stats().planned_source_records, 0);
    }
    #[test]
    fn source_vector_growth_reserves_peak_records_before_allocation() {
        let path = Path::new("same-path");
        let limit = 5 * path.as_os_str().len() as u64 + 12 * size_of::<PathBuf>() as u64 - 1;
        let mut budget = SourceDiscoveryBudget::new(
            SourceDiscoveryLimits {
                metadata_bytes: limit,
                ..Default::default()
            },
            limit,
        );
        let mut records = Vec::new();
        for _ in 0..4 {
            budget
                .reserve_source(SourceCategory::SiblingGma, path, &mut records)
                .unwrap();
            records.push(path.to_owned());
        }
        assert!(
            budget
                .reserve_source(SourceCategory::SiblingGma, path, &mut records)
                .is_err()
        );
        assert_eq!(records.len(), 4);
        assert_eq!(records.capacity(), 4);
        assert_eq!(budget.stats().planned_source_records, 4);
    }
}
