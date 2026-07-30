//! BSP pakfile data and extraction limits.

use super::{BspError, Limits, ZipReader, fmt, normalize_source_path};

pub const MAX_PAKFILE_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// The embedded pakfile lump's raw bytes. Parsed on demand via
/// [`ZipReader`] (its reader borrows, so it cannot be cached across
/// calls the way vbsp's `Packfile` cached an owned `zip::ZipArchive`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapPakFile {
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapPakFileEntry {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

impl MapPakFile {
    pub fn from_pak_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// A malformed central directory, if the lump is not a readable ZIP
    /// archive (tolerated: an absent/corrupt pakfile just means no
    /// bundled content, not a `load_map` failure).
    pub fn read_error(&self) -> Option<String> {
        ZipReader::parse(&self.bytes)
            .err()
            .map(|error| error.to_string())
    }

    pub fn indexed_entries(&self) -> Result<Vec<MapPakFileEntry>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        let mut entries = Vec::new();
        for (index, entry) in reader.entries().iter().enumerate() {
            let Some(path) = normalize_pakfile_path(&entry.path) else {
                continue;
            };
            if !is_pakfile_retained_entry(&path) {
                continue;
            }
            if is_pakfile_entry_oversized(entry.uncompressed_size) {
                log::debug!(
                    "bsp pakfile entry {} skipped over {} MiB cap ({} bytes)",
                    path,
                    MAX_PAKFILE_ENTRY_BYTES / 1024 / 1024,
                    entry.uncompressed_size
                );
                continue;
            }
            entries.push(MapPakFileEntry {
                index,
                path,
                size: entry.uncompressed_size,
            });
        }
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }

    pub fn entry_bytes_by_index(&self, index: usize) -> Result<Option<Vec<u8>>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        let Some(entry) = reader.entries().get(index) else {
            return Ok(None);
        };
        let limits = Limits {
            max_entry_bytes: MAX_PAKFILE_ENTRY_BYTES,
            ..Limits::default()
        };
        reader
            .entry_bytes(entry, &limits)
            .map(|bytes| Some(bytes.into_owned()))
            .map_err(pakfile_error)
    }
}

pub(super) fn normalize_pakfile_path(path: &str) -> Option<String> {
    normalize_source_path(path, None)
}

fn is_pakfile_material_entry(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vmt") || ext.eq_ignore_ascii_case("vtf"))
}

fn is_pakfile_model_entry(path: &str) -> bool {
    path.starts_with("models/")
        && std::path::Path::new(path).extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("mdl")
                || ext.eq_ignore_ascii_case("vvd")
                || ext.eq_ignore_ascii_case("vtx")
                || ext.eq_ignore_ascii_case("phy")
        })
}

pub(super) fn is_pakfile_retained_entry(path: &str) -> bool {
    is_pakfile_material_entry(path) || is_pakfile_model_entry(path)
}

pub(super) fn is_pakfile_entry_oversized(size_bytes: u64) -> bool {
    size_bytes > MAX_PAKFILE_ENTRY_BYTES
}

/// The pakfile reader has its own error type; it carries no structure worth
/// preserving beyond its message.
pub(super) fn pakfile_error(error: impl fmt::Display) -> BspError {
    BspError::Pakfile {
        message: error.to_string(),
    }
}
