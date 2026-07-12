use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use thiserror::Error;

const VPK_VERSION_1_HEADER_SIZE: u64 = 12;
const VPK_VERSION_2_HEADER_SIZE: u64 = 28;

#[derive(Debug, Clone, Error)]
pub enum VpkError {
    #[error("ERR_IO_ERROR")]
    IOError(#[source] Option<Arc<std::io::Error>>),
    #[error("ERR_VPK_FORMAT_ERROR")]
    FormatError,
    #[error("ERR_VPK_INVALID_HEADER")]
    InvalidHeader,
    #[error("ERR_VPK_ENTRY_NOT_FOUND")]
    EntryNotFound,
    #[error("ERR_VPK_UNSAFE_PATH")]
    UnsafePath,
    #[error("ERR_VPK_MISSING_ARCHIVE")]
    MissingArchive,
}

impl PartialEq for VpkError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}
impl Eq for VpkError {}

impl From<std::io::Error> for VpkError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(Some(Arc::new(error)))
    }
}

impl crate::error_key::HasErrorKey for VpkError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::IOError(_) => keys::IO_ERROR,
            Self::FormatError => keys::VPK_FORMAT_ERROR,
            Self::InvalidHeader => keys::VPK_INVALID_HEADER,
            Self::EntryNotFound => keys::VPK_ENTRY_NOT_FOUND,
            Self::UnsafePath => keys::VPK_UNSAFE_PATH,
            Self::MissingArchive => keys::VPK_MISSING_ARCHIVE,
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::IOError(Some(source)) => Some(source.to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,

    location: vformats::vpk::VpkLocation,
    preload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkFile {
    pub path: PathBuf,
    pub version: u8,

    entries: HashMap<String, VpkEntry>,
}

fn map_parse_error(error: &vformats::vpk::VpkError) -> VpkError {
    use vformats::vpk::VpkError as Parse;
    match error {
        Parse::BadMagic | Parse::UnsupportedVersion(_) => VpkError::InvalidHeader,
        Parse::UnsafePath(_) => VpkError::UnsafePath,
        _ => VpkError::FormatError,
    }
}

impl VpkFile {
    pub fn open(dir_vpk_path: impl AsRef<Path>) -> Result<Self, VpkError> {
        main_thread_forbidden!();

        let path = dir_vpk_path.as_ref();
        let mut file = File::open(path)?;
        // Read only header + tree: big directory files (garrysmod_dir)
        // embed gigabytes of payload after the tree.
        let mut head = [0_u8; 12];
        file.read_exact(&mut head)
            .map_err(|_| VpkError::InvalidHeader)?;
        let version = u32::from_le_bytes(head[4..8].try_into().expect("4 bytes"));
        let tree_size = u32::from_le_bytes(head[8..12].try_into().expect("4 bytes"));
        let header_size = match version {
            1 => VPK_VERSION_1_HEADER_SIZE,
            2 => VPK_VERSION_2_HEADER_SIZE,
            // Signature errors surface via parse below; unknown versions
            // here would mis-size the prefix read.
            _ => return Err(VpkError::InvalidHeader),
        };
        let prefix_len = usize::try_from(header_size + u64::from(tree_size))
            .map_err(|_| VpkError::FormatError)?;
        let mut prefix = vec![0_u8; prefix_len];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut prefix)
            .map_err(|_| VpkError::FormatError)?;

        // Entry payload extents are locations for us to read later, so
        // the header+tree prefix is a complete parse input.
        let directory = vformats::vpk::parse(&prefix, &vformats::Limits::default())
            .map_err(|error| map_parse_error(&error))?;
        let entries = directory
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    VpkEntry {
                        path: entry.path.clone(),
                        size: entry.size(),
                        crc: entry.crc32,
                        location: entry.location,
                        preload: entry.preload.to_vec(),
                    },
                )
            })
            .collect();

        Ok(Self {
            path: path.to_owned(),
            version: version as u8,
            entries,
        })
    }

    pub fn entries(&self) -> &HashMap<String, VpkEntry> {
        &self.entries
    }

    pub fn read_entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, VpkError> {
        main_thread_forbidden!();

        let entry = self
            .entries
            .get(entry_path)
            .ok_or(VpkError::EntryNotFound)?;
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
        let mut bytes = Vec::with_capacity(
            entry
                .preload
                .len()
                .checked_add(external_len)
                .ok_or(VpkError::FormatError)?,
        );
        bytes.extend_from_slice(&entry.preload);
        if external_len == 0 {
            return Ok(bytes);
        }

        archive.seek(SeekFrom::Start(offset))?;
        let mut external = vec![0_u8; external_len];
        archive.read_exact(&mut external)?;
        bytes.extend_from_slice(&external);

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
                VpkError::IOError(Some(Arc::new(err)))
            }
        })
    }
}

pub fn discover_game_vpks(gmod_dir: &Path) -> Vec<PathBuf> {
    let mut vpks = Vec::new();
    discover_game_vpks_inner(gmod_dir, 0, &mut vpks);
    vpks.sort_unstable_by(|left, right| {
        vpk_mount_sort_key(gmod_dir, left).cmp(&vpk_mount_sort_key(gmod_dir, right))
    });
    vpks
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

fn discover_game_vpks_inner(dir: &Path, depth: usize, vpks: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if depth < 3 {
                discover_game_vpks_inner(&path, depth + 1, vpks);
            }
        } else if file_type.is_file() && is_dir_vpk_path(&path) {
            vpks.push(path);
        }
    }
}

fn is_dir_vpk_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.ends_with("_dir.vpk"))
}

#[cfg(test)]
mod tests;
