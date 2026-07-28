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
use crate::util::main_thread_forbidden;

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
            Self::Mem(bytes) => bytes.as_ref(),
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
    /// # Accepted risk
    /// `Mmap::map` requires that the file is not modified or truncated while
    /// mapped. That cannot be guaranteed here: addons live in a user-writable
    /// directory that Steam also updates, so another process truncating one
    /// mid-read is undefined behaviour, not an I/O error the caller can
    /// handle. Bounds-checking parse extents does not address it — the
    /// mapping itself is what becomes invalid.
    ///
    /// This is accepted deliberately, for the same reason upstream
    /// gmpublisher accepted it: the alternative is re-reading every addon
    /// through a seeking reader on every preview. Every public constructor
    /// below inherits the risk and repeats it.
    pub(crate) fn mmap(path: &Path) -> Result<Self, GMAError> {
        main_thread_forbidden!();

        let file = File::open(path)?;
        // SAFETY: see the accepted-risk note above.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self {
            bytes: GmaBytes::Mapped(map),
        })
    }

    /// A GMA decompressed into memory (workshop download); `path` names
    /// the original compressed payload for identity purposes. Also the
    /// door in-memory test fixtures come through.
    pub fn from_membuffer(bytes: ArcBytes) -> Self {
        Self {
            bytes: GmaBytes::Mem(bytes),
        }
    }

    /// A GMA decompressed to a spill file; `path` keeps naming the
    /// original payload so the addon's identity (extracted-name
    /// fallback, dedup by path) is unchanged.
    pub(crate) fn from_temp_backing(temp_path: TempPath) -> Result<Self, GMAError> {
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

    /// Memory-maps `path` read-only, like [`Self::mmap`], for callers
    /// outside the crate that keep the view alive across several reads
    /// (the preview modal holds it for entry fetches).
    ///
    /// Carries [`Self::mmap`]'s accepted risk: truncating the file while the
    /// returned view is alive is undefined behaviour.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GMAError> {
        Self::mmap(path.as_ref())
    }

    /// Parses the header + metadata and builds the identity handle for
    /// this view's content; `path` names the addon for identity purposes
    /// (see the constructors above).
    pub fn handle(&self, path: impl AsRef<Path>) -> Result<GMAFile, GMAError> {
        let parsed = self.parse()?;
        Ok(self.handle_from_parsed(&parsed, path))
    }

    fn handle_from_parsed(
        &self,
        parsed: &vformats::gma::Gma<'_>,
        path: impl AsRef<Path>,
    ) -> GMAFile {
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
        gma
    }

    /// Identity handle, header, and safe-path entry list from a **single**
    /// parse. [`Self::handle`], [`Self::header`] and [`Self::entries`]
    /// each re-walk the whole entry table; index and discovery reads use
    /// this instead.
    pub fn meta(&self, path: impl AsRef<Path>) -> Result<GmaMetaBundle, GMAError> {
        let parsed = self.parse()?;
        let handle = self.handle_from_parsed(&parsed, path);
        let header = GMAHeader {
            version: parsed.metadata.version,
            timestamp: parsed.metadata.timestamp,
            metadata: handle.metadata.clone(),
            author: parsed.metadata.author.to_string(),
            addon_version: parsed.metadata.addon_version,
        };
        let entries = self.indexed_entries_from_parsed(&parsed)?;
        Ok(GmaMetaBundle {
            handle,
            header,
            entries,
        })
    }

    /// Header plus safe-path entry extents for library indexing, without
    /// constructing the extraction handle that preview/extraction needs.
    pub fn index_meta(&self) -> Result<GmaIndexBundle, GMAError> {
        let parsed = self.parse()?;
        Ok(GmaIndexBundle {
            header: header_from_parsed(&parsed),
            entries: self.indexed_entries_from_parsed(&parsed)?,
        })
    }

    pub fn header(&self) -> Result<GMAHeader, GMAError> {
        let parsed = self.parse()?;
        Ok(header_from_parsed(&parsed))
    }

    /// Safe-path-filtered entry projection, computed fresh on every call.
    /// Callers that need it persistently own the result (it is not a
    /// populated field).
    pub fn entries(&self) -> Result<HashMap<String, GMAEntry>, GMAError> {
        let parsed = self.parse()?;
        Ok(entries_from_parsed(&parsed))
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

    /// Copies a payload extent recorded by [`Self::meta`] without reparsing
    /// the archive. Every access is checked against the backing bytes again.
    pub fn read_payload_bytes(&self, offset: u64, len: u64) -> Result<Vec<u8>, GMAError> {
        let start = usize::try_from(offset).map_err(|_| GMAError::FormatError)?;
        let len = usize::try_from(len).map_err(|_| GMAError::FormatError)?;
        let end = start.checked_add(len).ok_or(GMAError::FormatError)?;
        self.bytes
            .as_slice()
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(GMAError::FormatError)
    }
}

pub(super) fn entries_from_parsed(parsed: &vformats::gma::Gma<'_>) -> HashMap<String, GMAEntry> {
    let mut entries = HashMap::with_capacity(parsed.entries().len());
    for (index, entry) in parsed.entries().iter().enumerate() {
        // An entry whose path could escape an extraction root is skipped,
        // not fatal — real workshop archives contain them.
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
    entries
}

fn header_from_parsed(parsed: &vformats::gma::Gma<'_>) -> GMAHeader {
    let meta = &parsed.metadata;
    GMAHeader {
        version: meta.version,
        timestamp: meta.timestamp,
        metadata: metadata_from_embedded_fields(
            meta.name.to_string(),
            meta.description.to_string(),
        ),
        author: meta.author.to_string(),
        addon_version: meta.addon_version,
    }
}

impl GmaView {
    /// Records each entry's payload as an offset into this view's bytes, so a
    /// later [`Self::read_payload_bytes`] can re-slice it without re-parsing.
    ///
    /// A method rather than a free function taking the slice separately: the
    /// offsets are only meaningful against the buffer `parsed` borrows from,
    /// and reading `self.bytes` here is what ties the two together.
    fn indexed_entries_from_parsed(
        &self,
        parsed: &vformats::gma::Gma<'_>,
    ) -> Result<Vec<GmaIndexedEntry>, GMAError> {
        let bytes = self.bytes.as_slice();
        let mut entries = Vec::with_capacity(parsed.entries().len());
        for (index, entry) in parsed.entries().iter().enumerate() {
            if is_unsafe_entry_path(&entry.path) {
                log::warn!("Illegal GMA entry: {}", entry.path);
                continue;
            }
            let payload = parsed
                .entry_bytes(index)
                .map_err(|_| GMAError::FormatError)?;
            let offset = payload
                .as_ptr()
                .addr()
                .checked_sub(bytes.as_ptr().addr())
                .ok_or(GMAError::FormatError)?;
            // The payload must lie wholly inside this view, or the offset
            // names bytes from some other buffer.
            if offset
                .checked_add(payload.len())
                .is_none_or(|end| end > bytes.len())
            {
                return Err(GMAError::FormatError);
            }
            entries.push(GmaIndexedEntry {
                path: entry.path.to_string(),
                size: entry.size,
                crc: entry.crc32,
                data_offset: u64::try_from(offset).map_err(|_| GMAError::FormatError)?,
            });
        }
        Ok(entries)
    }
}

pub(super) fn safe_entry_indices_from_parsed(
    parsed: &vformats::gma::Gma<'_>,
) -> Vec<(String, usize)> {
    let mut entries = Vec::with_capacity(parsed.entries().len());
    for (index, entry) in parsed.entries().iter().enumerate() {
        if is_unsafe_entry_path(&entry.path) {
            log::warn!("Illegal GMA entry: {}", entry.path);
            continue;
        }
        entries.push((entry.path.to_string(), index));
    }
    entries
}

/// Everything a single [`GmaView::meta`] parse yields: the identity
/// handle, the header, and the safe-path entry list in table order.
#[derive(Debug, Clone)]
pub struct GmaMetaBundle {
    pub handle: GMAFile,
    pub header: GMAHeader,
    pub entries: Vec<GmaIndexedEntry>,
}

#[derive(Debug, Clone)]
pub struct GmaIndexBundle {
    pub header: GMAHeader,
    pub entries: Vec<GmaIndexedEntry>,
}

#[derive(Debug, Clone)]
pub struct GmaIndexedEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,
    pub data_offset: u64,
}

impl GMAFile {
    /// Memory-maps this addon's bytes for one read/extract operation.
    /// Only valid for on-disk addons; membuffer/spill flows hold the view
    /// they constructed instead of re-viewing through a handle.
    ///
    /// Carries [`GmaView::mmap`]'s accepted risk: truncating the file while
    /// the returned view is alive is undefined behaviour.
    pub fn view(&self) -> Result<GmaView, GMAError> {
        GmaView::mmap(&self.path)
    }

    pub fn header(&self) -> Result<GMAHeader, GMAError> {
        self.view()?.header()
    }

    /// One-mmap, one-parse open: handle + header + entry list together.
    ///
    /// Carries [`GmaView::mmap`]'s accepted risk for the duration of the call.
    pub fn open_meta<P: AsRef<Path>>(path: P) -> Result<GmaMetaBundle, GMAError> {
        GmaView::mmap(path.as_ref())?.meta(path)
    }

    /// One-mmap, one-parse library index without an unused extraction handle.
    pub fn open_index<P: AsRef<Path>>(path: P) -> Result<GmaIndexBundle, GMAError> {
        GmaView::mmap(path.as_ref())?.index_meta()
    }

    /// One-mmap, one-parse header read without projecting the entry table.
    pub fn open_header<P: AsRef<Path>>(path: P) -> Result<GMAHeader, GMAError> {
        GmaView::mmap(path.as_ref())?.header()
    }
}

fn metadata_from_embedded_fields(
    embedded_title: String,
    embedded_description: String,
) -> GMAMetadata {
    match serde_json::de::from_str::<super::StandardManifest>(&embedded_description) {
        Ok(manifest) if manifest.is_manifest() => GMAMetadata::Standard {
            title: embedded_title,
            addon_type: manifest.addon_type.unwrap_or_default(),
            tags: manifest.tags.unwrap_or_default(),
            ignore: manifest.ignore.unwrap_or_default(),
        },
        _ => GMAMetadata::Legacy {
            title: embedded_title,
            description: embedded_description,
        },
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::metadata_from_embedded_fields;
    use crate::gma::GMAMetadata;

    #[test]
    fn a_manifest_description_becomes_standard() {
        let metadata = metadata_from_embedded_fields(
            "Addon".to_owned(),
            r#"{"type":"map","tags":["scenic"]}"#.to_owned(),
        );

        assert!(matches!(metadata, GMAMetadata::Standard { .. }));
        assert_eq!(metadata.addon_type(), Some("map"));
        assert_eq!(
            metadata.tags().map(Vec::as_slice),
            Some(&["scenic".to_owned()][..])
        );
        assert_eq!(metadata.title(), "Addon");
    }

    /// Free text that happens to parse as JSON is not a manifest. Untagged
    /// deserialization used to swallow these into an empty `Standard`,
    /// discarding the description.
    #[test]
    fn a_json_shaped_description_without_manifest_keys_stays_legacy() {
        for description in ["{}", r#"{"note":"see the workshop page"}"#] {
            let metadata =
                metadata_from_embedded_fields("Addon".to_owned(), description.to_owned());

            let GMAMetadata::Legacy {
                description: kept, ..
            } = &metadata
            else {
                panic!("{description} should stay Legacy, got {metadata:?}");
            };
            assert_eq!(kept, description, "the description must survive");
        }
    }

    #[test]
    fn plain_text_stays_legacy() {
        let metadata =
            metadata_from_embedded_fields("Addon".to_owned(), "just a description".to_owned());

        assert!(matches!(metadata, GMAMetadata::Legacy { .. }));
        assert_eq!(metadata.title(), "Addon");
    }
}

#[cfg(test)]
mod tests {
    use crate::gma::{GMAError, is_unsafe_entry_path};

    use super::GmaView;

    #[test]
    fn payload_extent_reads_are_bounds_checked() {
        let view = GmaView::from_membuffer(vec![1, 2, 3, 4].into());

        assert_eq!(view.read_payload_bytes(1, 2).unwrap(), vec![2, 3]);
        assert_eq!(view.read_payload_bytes(4, 0).unwrap(), Vec::<u8>::new());
        assert!(matches!(
            view.read_payload_bytes(4, 1),
            Err(GMAError::FormatError)
        ));
        assert!(matches!(
            view.read_payload_bytes(u64::MAX, 2),
            Err(GMAError::FormatError)
        ));
    }

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
