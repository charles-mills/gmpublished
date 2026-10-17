use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use crate::util::main_thread_forbidden;
use thiserror::Error;

const VPK_VERSION_1_HEADER_SIZE: u64 = 12;
const VPK_VERSION_2_HEADER_SIZE: u64 = 28;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum VpkError {
    #[error(transparent)]
    IndexLimit(vformats::ArchiveIndexLimit),
    #[error("VPK payload requires {required} bytes; limit {limit}")]
    EntryLimit { required: u64, limit: u64 },
    #[error("VPK I/O failed")]
    IoError(#[source] crate::IoFailure),
    #[error("the VPK is malformed")]
    FormatError,
    #[error("the VPK header is not recognisable")]
    InvalidHeader,
    #[error("no such entry in the VPK")]
    EntryNotFound,
    #[error("the VPK entry path escapes its root")]
    UnsafePath,
    #[error("a VPK split archive is missing")]
    MissingArchive,
}

impl From<std::io::Error> for VpkError {
    fn from(error: std::io::Error) -> Self {
        Self::IoError(error.into())
    }
}

impl crate::error_key::HasErrorKey for VpkError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::IoError(_) => keys::IO_ERROR,
            Self::FormatError | Self::IndexLimit(_) | Self::EntryLimit { .. } => {
                keys::VPK_FORMAT_ERROR
            }
            Self::InvalidHeader => keys::VPK_INVALID_HEADER,
            Self::EntryNotFound => keys::VPK_ENTRY_NOT_FOUND,
            Self::UnsafePath => keys::VPK_UNSAFE_PATH,
            Self::MissingArchive => keys::VPK_MISSING_ARCHIVE,
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::IoError(source) => Some(source.to_string()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VpkEntry {
    pub size: u64,
    pub crc: u32,

    location: vformats::vpk::VpkLocation,
    preload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VpkFile {
    pub path: PathBuf,
    pub version: u8,

    entries: HashMap<String, VpkEntry>,
    index_cost: vformats::ArchiveIndexCost,
}

fn validated_prefix_len(
    header_size: u64,
    tree_size: u32,
    file_len: u64,
    directory_limit: u64,
) -> Result<usize, VpkError> {
    let prefix_len = header_size + u64::from(tree_size);
    if prefix_len > file_len {
        return Err(VpkError::FormatError);
    }
    vformats::ArchiveIndexLimits {
        directory_bytes: directory_limit,
        ..vformats::ArchiveIndexLimits::default()
    }
    .check(vformats::ArchiveIndexCost {
        directory_bytes: prefix_len,
        ..vformats::ArchiveIndexCost::default()
    })
    .map_err(VpkError::IndexLimit)?;
    usize::try_from(prefix_len).map_err(|_| VpkError::FormatError)
}

fn map_parse_error(error: &vformats::vpk::VpkError) -> VpkError {
    use vformats::vpk::VpkError as Parse;
    match error {
        Parse::BadMagic | Parse::UnsupportedVersion(_) => VpkError::InvalidHeader,
        Parse::IndexLimit(error) => VpkError::IndexLimit(*error),
        Parse::UnsafePath(_) => VpkError::UnsafePath,
        _ => VpkError::FormatError,
    }
}

#[derive(Debug)]
pub struct VpkIndexSource {
    path: PathBuf,
    file: File,
    version: u8,
    prefix_len: usize,
    limits: vformats::ArchiveIndexLimits,
}

impl VpkIndexSource {
    pub fn open(
        dir_vpk_path: impl AsRef<Path>,
        limits: vformats::ArchiveIndexLimits,
    ) -> Result<Self, VpkError> {
        main_thread_forbidden!();

        let path = dir_vpk_path.as_ref();
        let mut file = File::open(path)?;
        let mut head = [0_u8; 12];
        file.read_exact(&mut head)
            .map_err(|_| VpkError::InvalidHeader)?;
        if u32::from_le_bytes(head[..4].try_into().expect("4 bytes")) != 0x55aa_1234 {
            return Err(VpkError::InvalidHeader);
        }
        let version = u32::from_le_bytes(head[4..8].try_into().expect("4 bytes"));
        let tree_size = u32::from_le_bytes(head[8..12].try_into().expect("4 bytes"));
        let header_size = match version {
            1 => VPK_VERSION_1_HEADER_SIZE,
            2 => VPK_VERSION_2_HEADER_SIZE,
            _ => return Err(VpkError::InvalidHeader),
        };
        let prefix_len = validated_prefix_len(
            header_size,
            tree_size,
            file.metadata()?.len(),
            limits.directory_bytes,
        )?;
        Ok(Self {
            path: path.to_owned(),
            file,
            version: version as u8,
            prefix_len,
            limits,
        })
    }

    #[must_use]
    pub fn prefix_bytes(&self) -> usize {
        self.prefix_len
    }

    pub fn read(mut self) -> Result<PreparedVpkIndex, VpkError> {
        let mut prefix = vec![0; self.prefix_len];
        self.file.seek(SeekFrom::Start(0))?;
        self.file
            .read_exact(&mut prefix)
            .map_err(|_| VpkError::FormatError)?;
        Ok(PreparedVpkIndex {
            path: self.path,
            version: self.version,
            prefix,
            limits: self.limits,
        })
    }
}

pub struct PreparedVpkIndex {
    path: PathBuf,
    version: u8,
    prefix: Vec<u8>,
    limits: vformats::ArchiveIndexLimits,
}

impl std::fmt::Debug for PreparedVpkIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedVpkIndex")
            .field("path", &self.path)
            .field("version", &self.version)
            .field("prefix_bytes", &self.prefix.len())
            .finish_non_exhaustive()
    }
}

impl PreparedVpkIndex {
    pub fn index_cost(&self) -> Result<vformats::ArchiveIndexCost, VpkError> {
        let parse_limits = vformats::Limits {
            max_input_bytes: self.limits.directory_bytes,
            max_entries: usize::MAX,
            max_entry_bytes: u64::MAX,
            ..vformats::Limits::default()
        };
        let mut index_cost = vformats::vpk::index_cost(&self.prefix, &parse_limits)
            .map_err(|error| map_parse_error(&error))?;
        index_cost.record_bytes = index_cost
            .record_bytes
            .checked_add(
                (index_cost.entries as u64)
                    .checked_mul(std::mem::size_of::<(String, VpkEntry)>() as u64)
                    .ok_or(VpkError::FormatError)?,
            )
            .ok_or(VpkError::FormatError)?;
        index_cost.string_bytes = index_cost
            .string_bytes
            .checked_mul(2)
            .ok_or(VpkError::FormatError)?;
        self.limits
            .check(index_cost)
            .map_err(VpkError::IndexLimit)?;
        Ok(index_cost)
    }

    pub fn materialize(self) -> Result<VpkFile, VpkError> {
        let index_cost = self.index_cost()?;
        let parse_limits = vformats::Limits {
            max_input_bytes: self.limits.directory_bytes,
            max_entries: usize::MAX,
            max_entry_bytes: u64::MAX,
            ..vformats::Limits::default()
        };
        let directory =
            vformats::vpk::parse_with_index_limits(&self.prefix, &parse_limits, self.limits)
                .map_err(|error| map_parse_error(&error))?;
        let entries = directory
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    VpkEntry {
                        size: entry.size(),
                        crc: entry.crc32,
                        location: entry.location,
                        preload: entry.preload.to_vec(),
                    },
                )
            })
            .collect();

        Ok(VpkFile {
            path: self.path,
            version: self.version,
            entries,
            index_cost,
        })
    }
}

impl VpkFile {
    pub fn open(dir_vpk_path: impl AsRef<Path>) -> Result<Self, VpkError> {
        Self::open_with_index_limits(dir_vpk_path, vformats::ArchiveIndexLimits::default())
    }

    pub fn open_with_index_limits(
        dir_vpk_path: impl AsRef<Path>,
        limits: vformats::ArchiveIndexLimits,
    ) -> Result<Self, VpkError> {
        VpkIndexSource::open(dir_vpk_path, limits)?
            .read()?
            .materialize()
    }

    #[must_use]
    pub fn index_cost(&self) -> vformats::ArchiveIndexCost {
        self.index_cost
    }

    #[must_use]
    pub fn entries(&self) -> &HashMap<String, VpkEntry> {
        &self.entries
    }

    pub fn read_entry_prefix(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>, VpkError> {
        let entry = self.entries.get(path).ok_or(VpkError::EntryNotFound)?;
        let expected =
            usize::try_from(entry.size.min(max_bytes as u64)).map_err(|_| VpkError::FormatError)?;
        let preload = entry.preload.len().min(expected);
        let mut bytes = vec![0; expected];
        bytes[..preload].copy_from_slice(&entry.preload[..preload]);
        if preload < expected {
            let (mut archive, offset) = match entry.location {
                vformats::vpk::VpkLocation::InDirectory { offset, .. } => {
                    (File::open(&self.path)?, offset)
                }
                vformats::vpk::VpkLocation::InArchive {
                    archive, offset, ..
                } => (self.open_sibling_archive(archive)?, offset),
            };
            archive.seek(SeekFrom::Start(offset))?;
            archive.read_exact(&mut bytes[preload..])?;
        }
        Ok(bytes)
    }

    pub fn read_entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, VpkError> {
        self.read_entry_bytes_with_limit(entry_path, vformats::Limits::default().max_entry_bytes)
    }

    pub fn read_entry_bytes_with_limit(
        &self,
        entry_path: &str,
        limit: u64,
    ) -> Result<Vec<u8>, VpkError> {
        main_thread_forbidden!();

        let entry = self
            .entries
            .get(entry_path)
            .ok_or(VpkError::EntryNotFound)?;
        if entry.size > limit {
            return Err(VpkError::EntryLimit {
                required: entry.size,
                limit,
            });
        }
        let (mut archive, offset, len) = match entry.location {
            vformats::vpk::VpkLocation::InDirectory { offset, len } => {
                (File::open(&self.path)?, offset, len)
            }
            vformats::vpk::VpkLocation::InArchive {
                archive,
                offset,
                len,
            } => (self.open_sibling_archive(archive)?, offset, len),
        };
        let external_len = usize::try_from(len).map_err(|_| VpkError::FormatError)?;
        let preload_len = entry.preload.len();
        let total_len = preload_len
            .checked_add(external_len)
            .ok_or(VpkError::FormatError)?;
        let mut bytes = vec![0_u8; total_len];
        bytes[..preload_len].copy_from_slice(&entry.preload);
        if external_len == 0 {
            return Ok(bytes);
        }

        archive.seek(SeekFrom::Start(offset))?;
        archive.read_exact(&mut bytes[preload_len..])?;

        Ok(bytes)
    }

    fn open_sibling_archive(&self, archive_index: u16) -> Result<File, VpkError> {
        let file_name = self
            .path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .ok_or(VpkError::MissingArchive)?;
        let sibling = vformats::vpk::sibling_archive_name(file_name, archive_index)
            .ok_or(VpkError::MissingArchive)?;
        File::open(self.path.with_file_name(sibling)).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                VpkError::MissingArchive
            } else {
                VpkError::IoError((&err).into())
            }
        })
    }
}

#[allow(missing_docs)]
pub fn discover_game_vpks(
    gmod_dir: &Path,
) -> Result<Vec<PathBuf>, crate::source_discovery::SourceDiscoveryError> {
    discover_game_vpks_with_mounts(gmod_dir, &[])
}
#[allow(missing_docs)]
pub fn discover_game_vpks_with_mounts(
    gmod_dir: &Path,
    mount_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, crate::source_discovery::SourceDiscoveryError> {
    let limits = crate::source_discovery::SourceDiscoveryLimits::default();
    let mut budget =
        crate::source_discovery::SourceDiscoveryBudget::new(limits, limits.metadata_bytes);
    discover_game_vpks_checked(gmod_dir, mount_dirs, &mut budget, &|| false)
}

#[allow(missing_docs)]
pub fn discover_game_vpks_checked(
    gmod_dir: &Path,
    mount_dirs: &[PathBuf],
    budget: &mut crate::source_discovery::SourceDiscoveryBudget,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<PathBuf>, crate::source_discovery::SourceDiscoveryError> {
    let mut vpks = Vec::new();
    discover_game_vpks_bounded_inner(gmod_dir, 0, &mut vpks, budget, cancelled)?;
    for mount in mount_dirs {
        discover_game_vpks_bounded_inner(mount, 0, &mut vpks, budget, cancelled)?;
    }
    vpks.sort_unstable();
    vpks.dedup();
    let key_bytes = vpks
        .iter()
        .try_fold(0_u64, |sum, path| {
            let lossy_utf8_bytes = (path.as_os_str().len() as u64).checked_mul(3)?;
            let normalization_working_bytes = lossy_utf8_bytes.checked_mul(3)?;
            sum.checked_add(normalization_working_bytes)
                .and_then(|sum| sum.checked_add(size_of::<((u8, String), usize)>() as u64))
        })
        .ok_or_else(|| crate::source_discovery::SourceDiscoveryFailure {
            category: crate::source_discovery::SourceCategory::GameVpk,
            path: gmod_dir.to_owned(),
            kind: crate::source_discovery::DiscoveryFailureKind::ArithmeticOverflow,
        })?;
    budget.charge_metadata(
        crate::source_discovery::SourceCategory::GameVpk,
        gmod_dir,
        key_bytes,
    )?;
    vpks.sort_by_cached_key(|path| vpk_mount_sort_key(gmod_dir, path));
    Ok(vpks)
}
fn discover_game_vpks_bounded_inner(
    dir: &Path,
    depth: usize,
    vpks: &mut Vec<PathBuf>,
    budget: &mut crate::source_discovery::SourceDiscoveryBudget,
    cancelled: &impl Fn() -> bool,
) -> Result<(), crate::source_discovery::SourceDiscoveryError> {
    use crate::source_discovery::SourceCategory;
    for child in budget.sorted_children(SourceCategory::GameVpk, dir, cancelled)? {
        if budget.child_is_dir(SourceCategory::GameVpk, &child)? {
            if depth < 3 {
                discover_game_vpks_bounded_inner(&child.path, depth + 1, vpks, budget, cancelled)?;
            }
        } else if budget.child_is_file(SourceCategory::GameVpk, &child)?
            && is_dir_vpk_path(&child.path)
            && !vpks.contains(&child.path)
        {
            budget.reserve_source(SourceCategory::GameVpk, &child.path, vpks)?;
            vpks.push(child.path);
        }
    }
    Ok(())
}

fn vpk_mount_sort_key(gmod_dir: &Path, path: &Path) -> (u8, String) {
    let relative = path
        .strip_prefix(gmod_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let priority = if relative == "garrysmod/garrysmod_dir.vpk" {
        0
    } else if relative.starts_with("sourceengine/") {
        1
    } else if relative.starts_with("platform/") {
        2
    } else if relative == "garrysmod/fallbacks_dir.vpk" {
        4
    } else {
        3
    };
    (priority, relative)
}

fn is_dir_vpk_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.ends_with("_dir.vpk"))
}

#[cfg(test)]
mod tests;
