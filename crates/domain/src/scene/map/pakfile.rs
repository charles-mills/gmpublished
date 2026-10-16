use super::{BspError, Limits, ZipReader, fmt, normalize_source_path};
use std::sync::Arc;

pub const MAX_PAKFILE_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapPakFile {
    bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapPakFileEntry {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

impl MapPakFile {
    pub fn from_pak_bytes(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

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
            entries.push(MapPakFileEntry {
                index,
                path,
                size: entry.uncompressed_size,
            });
        }
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }

    pub fn entry_prefix_by_index(
        &self,
        index: usize,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        reader
            .entries()
            .get(index)
            .map(|entry| reader.entry_prefix(entry, max_bytes).map_err(pakfile_error))
            .transpose()
    }

    pub fn entry_decoder_working_bytes(&self, index: usize) -> Result<Option<u64>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        reader
            .entries()
            .get(index)
            .map(|entry| reader.decoder_working_bytes(entry).map_err(pakfile_error))
            .transpose()
    }

    pub fn entry_prefix_by_index_with_decoder_limit(
        &self,
        index: usize,
        max_bytes: usize,
        decoder_limit: u64,
    ) -> Result<Option<Vec<u8>>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        reader
            .entries()
            .get(index)
            .map(|entry| {
                reader
                    .entry_prefix_with_decoder_limit(entry, max_bytes, decoder_limit)
                    .map_err(pakfile_error)
            })
            .transpose()
    }

    pub fn entry_bytes_by_index_with_limit(
        &self,
        index: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, BspError> {
        self.entry_bytes_by_index_with_limits(index, max_bytes, Limits::default().max_input_bytes)
    }

    pub fn entry_bytes_by_index_with_limits(
        &self,
        index: usize,
        max_bytes: u64,
        decoder_limit: u64,
    ) -> Result<Option<Vec<u8>>, BspError> {
        let reader = ZipReader::parse(&self.bytes).map_err(pakfile_error)?;
        let Some(entry) = reader.entries().get(index) else {
            return Ok(None);
        };
        reader
            .entry_bytes_with_decoder_limit(
                entry,
                &Limits {
                    max_entry_bytes: max_bytes,
                    ..Limits::default()
                },
                decoder_limit,
            )
            .map(|bytes| Some(bytes.into_owned()))
            .map_err(pakfile_error)
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
    is_pakfile_material_entry(path) || is_pakfile_model_entry(path) || is_prop_lighting_entry(path)
}

fn is_prop_lighting_entry(path: &str) -> bool {
    let Some(stem) = path.strip_suffix(".vhv") else {
        return false;
    };
    let Some(index) = stem
        .strip_prefix("sp_hdr_")
        .or_else(|| stem.strip_prefix("sp_"))
    else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn pakfile_error(error: impl fmt::Display) -> BspError {
    BspError::Pakfile {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pakfile_handles_share_the_decoded_byte_allocation() {
        let bytes: Arc<[u8]> = Arc::from([1, 2, 3]);
        let pakfile = MapPakFile::from_pak_bytes(bytes.clone());
        let other = pakfile.clone();
        assert!(Arc::ptr_eq(&bytes, &pakfile.bytes));
        assert!(Arc::ptr_eq(&pakfile.bytes, &other.bytes));
    }
}
