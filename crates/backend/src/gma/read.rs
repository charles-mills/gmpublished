//! Read-side plumbing: the GMA wire format lives in [`vformats::gma`];
//! this adapts it to the app's [`GmaView`] (bytes provider) and
//! [`GMAFile`] (parsed identity/summary). Bytes come from a workshop
//! decompression buffer, a decompressed spill file, or a read-only
//! memory map of the addon on disk (never loaded whole — addons reach
//! gigabytes; the map pages in only what parsing and extraction
//! actually touch).

use std::{collections::HashMap, fs::File, path::Path, sync::Arc};

use tempfile::TempPath;

use crate::ArcBytes;

use super::{GMAEntry, GMAError, GMAFile, GMAHeader, GMAMetadata, is_unsafe_entry_path};

/// Where a GMA's (decompressed) bytes live for parsing.
enum GmaBytes {
    Mem(ArcBytes),
    Mapped(memmap2::Mmap),
    /// A decompressed-to-disk spill file, memory-mapped; `_guard` keeps
    /// it alive (and deletes it) for as long as any clone of the owning
    /// view can still read from it.
    TempBacked {
        map: memmap2::Mmap,
        _guard: Arc<TempPath>,
    },
}

impl GmaBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mem(bytes) => bytes.as_slice(),
            Self::Mapped(map) | Self::TempBacked { map, .. } => map,
        }
    }
}

fn map_parse_error(error: &vformats::gma::GmaError) -> GMAError {
    use vformats::gma::GmaError as Parse;
    match error {
        Parse::BadMagic | Parse::UnsupportedVersion(_) => GMAError::InvalidHeader,
        _ => GMAError::FormatError,
    }
}

/// The bytes provider for one GMA read/extract operation, created when an
/// operation actually needs entry data and dropped after. Cheap to
/// construct for on-disk addons ([`GMAFile::view`] mmaps on demand); the
/// membuffer/spill variants are constructed once by the download/decompress
/// flow that produced the bytes and carried alongside the [`GMAFile`]
/// handle derived from them, since there is no on-disk GMA to re-view.
pub struct GmaView {
    bytes: GmaBytes,
}

impl GmaView {
    /// Memory-maps `path` read-only.
    ///
    /// # Safety-adjacent note
    /// The mapped file could in principle be replaced mid-read (Steam
    /// updating content). Parsing validates every extent against the
    /// mapped length up front and slices never exceed it; a concurrent
    /// truncation can fault the process, the same failure the previous
    /// seek-based reader surfaced as I/O errors. Upstream gmpublisher
    /// shipped memory-mapped GMAs the same way.
    pub(crate) fn mmap(path: &Path) -> Result<Self, GMAError> {
        main_thread_forbidden!();
        #[cfg(test)]
        super::parse_observation::record(path);

        let file = File::open(path)?;
        // SAFETY: see doc comment above.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self {
            bytes: GmaBytes::Mapped(map),
        })
    }

    /// A GMA decompressed into memory (workshop download); `path` names
    /// the original compressed payload for identity purposes. Also the
    /// door in-memory test fixtures come through.
    pub fn from_membuffer(bytes: ArcBytes, _path: impl AsRef<Path>) -> Self {
        #[cfg(test)]
        super::parse_observation::record(_path.as_ref());

        Self {
            bytes: GmaBytes::Mem(bytes),
        }
    }

    /// A GMA decompressed to a spill file; `path` keeps naming the
    /// original payload so the addon's identity (extracted-name
    /// fallback, dedup by path) is unchanged.
    pub(crate) fn from_temp_backing(
        temp_path: TempPath,
        _path: impl AsRef<Path>,
    ) -> Result<Self, GMAError> {
        #[cfg(test)]
        super::parse_observation::record(_path.as_ref());

        let file = File::open(&temp_path)?;
        // SAFETY: see `mmap`'s doc comment; this spill file is exclusively
        // owned by the decompression that produced it.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self {
            bytes: GmaBytes::TempBacked {
                map,
                _guard: Arc::new(temp_path),
            },
        })
    }

    /// Whether this view's bytes are a decompressed-to-disk spill file
    /// rather than an in-memory buffer or a direct on-disk mapping.
    /// Exposed for tests asserting `GMAFile::decompress`'s memory-vs-spill
    /// threshold; production code never branches on it.
    pub fn is_temp_backed(&self) -> bool {
        matches!(self.bytes, GmaBytes::TempBacked { .. })
    }

    /// The spill file's path, when [`Self::is_temp_backed`]. Exposed for
    /// tests asserting the spill file is deleted once nothing holds this
    /// view anymore.
    pub fn temp_backing_path(&self) -> Option<&Path> {
        match &self.bytes {
            GmaBytes::TempBacked { _guard, .. } => Some(_guard.as_ref().as_ref()),
            _ => None,
        }
    }

    pub fn parse(&self) -> Result<vformats::gma::Gma<'_>, GMAError> {
        main_thread_forbidden!();
        // No whole-input or per-entry cap: these bytes are already
        // materialized (a buffer or a file mapping) and parsing plus
        // entry access are zero-copy, so the caps would only reject
        // legitimately large addons (multi-GB map GMAs with BSP entries
        // past any fixed threshold are common on the workshop).
        let limits = vformats::Limits {
            max_input_bytes: u64::MAX,
            max_entry_bytes: u64::MAX,
            ..vformats::Limits::default()
        };
        vformats::gma::parse(self.bytes.as_slice(), &limits)
            .map_err(|error| map_parse_error(&error))
    }

    /// Parses the header + metadata and builds the identity handle for
    /// this view's content; `path` names the addon for identity purposes
    /// (see the constructors above).
    pub fn handle(&self, path: impl AsRef<Path>) -> Result<GMAFile, GMAError> {
        let parsed = self.parse()?;
        let meta = &parsed.metadata;

        let mut gma = GMAFile {
            path: path.as_ref().to_owned(),
            size: self.bytes.as_slice().len() as u64,
            id: None,
            metadata: metadata_from_embedded_fields(
                meta.name.to_string(),
                meta.description.to_string(),
            ),
            version: meta.version,
            extracted_name: String::new(),
            modified: None,
        };
        gma.compute_extracted_name();
        Ok(gma)
    }

    pub fn header(&self) -> Result<GMAHeader, GMAError> {
        let parsed = self.parse()?;
        let meta = &parsed.metadata;
        Ok(GMAHeader {
            version: meta.version,
            timestamp: meta.timestamp,
            metadata: metadata_from_embedded_fields(
                meta.name.to_string(),
                meta.description.to_string(),
            ),
            author: meta.author.to_string(),
            addon_version: meta.addon_version,
        })
    }

    /// Safe-path-filtered entry projection, computed fresh on every call.
    /// Callers that need it persistently own the result (it is not a
    /// populated field).
    pub fn entries(&self) -> Result<HashMap<String, GMAEntry>, GMAError> {
        let parsed = self.parse()?;
        let mut entries = HashMap::with_capacity(parsed.entries().len());
        for (index, entry) in parsed.entries().iter().enumerate() {
            // An entry whose path could escape an extraction root is
            // skipped, not fatal — real workshop archives contain them.
            if is_unsafe_entry_path(&entry.path) {
                log::warn!("Illegal GMA entry: {}", entry.path);
                continue;
            }
            entries.insert(
                entry.path.to_string(),
                GMAEntry {
                    path: entry.path.to_string(),
                    size: entry.size,
                    crc: entry.crc32,
                    index: index as u64,
                },
            );
        }
        Ok(entries)
    }

    /// Reads one entry's payload by path. Rejects unsafe paths the same
    /// way [`Self::entries`] filters them out of its projection.
    pub fn read_entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, GMAError> {
        if is_unsafe_entry_path(entry_path) {
            return Err(GMAError::EntryNotFound);
        }
        let parsed = self.parse()?;
        let (_, payload) = parsed.get(entry_path).ok_or(GMAError::EntryNotFound)?;
        Ok(payload.to_vec())
    }
}

impl GMAFile {
    /// Memory-maps this addon's bytes for one read/extract operation.
    /// Only valid for on-disk addons; membuffer/spill flows hold the view
    /// they constructed instead of re-viewing through a handle.
    pub fn view(&self) -> Result<GmaView, GMAError> {
        GmaView::mmap(&self.path)
    }

    pub fn header(&self) -> Result<GMAHeader, GMAError> {
        self.view()?.header()
    }
}

fn metadata_from_embedded_fields(
    embedded_title: String,
    embedded_description: String,
) -> GMAMetadata {
    match serde_json::de::from_str::<GMAMetadata>(&embedded_description) {
        Ok(mut metadata) => {
            match &mut metadata {
                GMAMetadata::Standard { title, .. } => *title = embedded_title,
                GMAMetadata::Legacy { title, description } => {
                    *title = embedded_title;
                    *description = embedded_description;
                }
            }
            metadata
        }
        Err(_) => GMAMetadata::Legacy {
            title: embedded_title,
            description: embedded_description,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::gma::is_unsafe_entry_path;

    #[test]
    fn rejects_absolute_unix() {
        assert!(is_unsafe_entry_path("/etc/passwd"));
        assert!(is_unsafe_entry_path("/"));
    }

    #[test]
    fn rejects_absolute_windows_root() {
        assert!(is_unsafe_entry_path(
            "\\Program Files (x86)\\Steam\\steamapps\\common\\GarrysMod\\garrysmod\\lua\\bin\\evil.dll"
        ));
        assert!(is_unsafe_entry_path("\\evil.dll"));
    }

    #[test]
    fn rejects_embedded_backslash() {
        assert!(is_unsafe_entry_path(
            "Program Files (x86)\\Steam\\steamapps\\common\\GarrysMod\\garrysmod\\lua\\bin\\haha.dll"
        ));
        assert!(is_unsafe_entry_path("lua\\autorun\\evil.lua"));
        assert!(is_unsafe_entry_path("foo\\bar"));
    }

    #[test]
    fn rejects_segment_whitespace() {
        assert!(is_unsafe_entry_path(" Files (x86)/Steam/foo"));
        assert!(is_unsafe_entry_path("lua/ autorun/foo.lua"));
        assert!(is_unsafe_entry_path("lua/autorun /foo.lua"));
        assert!(is_unsafe_entry_path("\tfoo/bar"));
    }

    #[test]
    fn rejects_drive_letter() {
        assert!(is_unsafe_entry_path("C:\\evil.dll"));
        assert!(is_unsafe_entry_path("c:evil.dll"));
        assert!(is_unsafe_entry_path("file.txt:stream"));
    }

    #[test]
    fn rejects_unc_and_long_paths() {
        assert!(is_unsafe_entry_path("\\\\server\\share\\evil"));
        assert!(is_unsafe_entry_path("\\\\?\\C:\\evil"));
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(is_unsafe_entry_path("../etc/passwd"));
        assert!(is_unsafe_entry_path("..\\evil.dll"));
        assert!(is_unsafe_entry_path("foo/../../bar"));
        assert!(is_unsafe_entry_path("foo\\..\\bar"));
        assert!(is_unsafe_entry_path(".."));
    }

    #[test]
    fn rejects_current_dir_segments() {
        assert!(is_unsafe_entry_path("./foo"));
        assert!(is_unsafe_entry_path("foo/./bar"));
        assert!(is_unsafe_entry_path("."));
    }

    #[test]
    fn rejects_empty_or_null() {
        assert!(is_unsafe_entry_path(""));
        assert!(is_unsafe_entry_path("foo\0bar"));
        assert!(is_unsafe_entry_path("foo//bar"));
    }

    #[test]
    fn accepts_normal_entries() {
        assert!(!is_unsafe_entry_path("lua/autorun/foo.lua"));
        assert!(!is_unsafe_entry_path("materials/models/foo.vmt"));
        assert!(!is_unsafe_entry_path("addon.json"));
        assert!(!is_unsafe_entry_path("foo..bar/baz"));
    }
}
