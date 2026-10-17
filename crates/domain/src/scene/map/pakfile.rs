use super::{BspError, Limits, ZipArchive, fmt, normalize_source_path};
use std::sync::Arc;

pub const MAX_PAKFILE_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapPakFile {
    archive: ZipArchive,
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
            archive: ZipArchive::new(bytes),
        }
    }

    pub fn index_cost(&self) -> Result<vformats::ArchiveIndexCost, vformats::bsp::ZipError> {
        self.archive.index_cost()
    }

    pub fn indexed_entries_with_limits(
        &self,
        limits: vformats::ArchiveIndexLimits,
    ) -> Result<Vec<MapPakFileEntry>, vformats::bsp::ZipError> {
        let reader = self.archive.reader_with_index_limits(limits)?;
        Ok(reader
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let path = normalize_pakfile_path(&entry.path)?;
                is_pakfile_retained_entry(&path).then_some(MapPakFileEntry {
                    index,
                    path,
                    size: entry.uncompressed_size,
                })
            })
            .collect())
    }

    pub fn read_error(&self) -> Option<String> {
        self.archive.reader().err().map(|error| error.to_string())
    }

    pub fn indexed_entries(&self) -> Result<Vec<MapPakFileEntry>, BspError> {
        let reader = self.archive.reader().map_err(pakfile_error)?;
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
        let reader = self.archive.reader().map_err(pakfile_error)?;
        reader
            .entries()
            .get(index)
            .map(|entry| reader.entry_prefix(entry, max_bytes).map_err(pakfile_error))
            .transpose()
    }

    pub fn entry_decoder_working_bytes(&self, index: usize) -> Result<Option<u64>, BspError> {
        let reader = self.archive.reader().map_err(pakfile_error)?;
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
        let reader = self.archive.reader().map_err(pakfile_error)?;
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
        let reader = self.archive.reader().map_err(pakfile_error)?;
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
        let reader = self.archive.reader().map_err(pakfile_error)?;
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
    fn cloned_pakfiles_reuse_the_nonempty_directory() {
        let name = b"materials/a.vmt";
        let mut bytes = vec![0; 46];
        bytes[..4].copy_from_slice(&0x02014b50_u32.to_le_bytes());
        bytes[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name);
        let directory_len = bytes.len();
        let mut end = [0; 22];
        end[..4].copy_from_slice(&0x06054b50_u32.to_le_bytes());
        end[8..10].copy_from_slice(&1_u16.to_le_bytes());
        end[10..12].copy_from_slice(&1_u16.to_le_bytes());
        end[12..16].copy_from_slice(&(directory_len as u32).to_le_bytes());
        bytes.extend_from_slice(&end);
        let pak = MapPakFile::from_pak_bytes(bytes);
        let clone = pak.clone();
        let first = pak.archive.reader().unwrap();
        let second = clone.archive.reader().unwrap();
        assert_eq!(first.entries().len(), 1);
        assert_eq!(first.entries().as_ptr(), second.entries().as_ptr());
        assert_eq!(pak.indexed_entries().unwrap()[0].path, "materials/a.vmt");
        assert_eq!(
            pak.indexed_entries().unwrap(),
            clone.indexed_entries().unwrap()
        );
        assert!(!format!("{pak:?}").contains("materials/a.vmt"));
    }

    #[test]
    fn pakfile_handles_share_the_decoded_byte_allocation() {
        let bytes: Arc<[u8]> = Arc::from([1, 2, 3]);
        let pakfile = MapPakFile::from_pak_bytes(bytes.clone());
        let other = pakfile.clone();
        assert_eq!(Arc::strong_count(&bytes), 2);
        assert_eq!(pakfile, other);
        assert!(pakfile.read_error().is_some());
        assert_eq!(pakfile, other);
        assert_eq!(Arc::strong_count(&bytes), 2);
    }
}
