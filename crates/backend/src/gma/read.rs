//! Safe, bounded-memory GMA reads.
//!
//! In-memory decompression results are read directly. On-disk addons and
//! decompression spill files use positional reads, so another process can
//! replace or truncate a Steam-managed file without invalidating borrowed
//! memory. Only the owned header and entry index are retained; payloads are
//! copied or streamed from checked ranges on demand.

mod format;

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
};

use tempfile::TempPath;

use crate::ArcBytes;

use super::{GmaEntry, GmaError, GmaFile, GmaHeader, GmaMetadata, is_unsafe_entry_path};
use crate::util::main_thread_forbidden;
use format::ParsedGma;

/// Where a GMA's decompressed bytes live.
enum GmaSource {
    Mem(ArcBytes),
    File(FileSource),
    /// `_guard` unlinks the spill once the view no longer needs it.
    TempBacked {
        source: FileSource,
        _guard: TempPath,
    },
}

struct FileSource {
    file: File,
    len: u64,
    #[cfg(not(any(unix, windows)))]
    cursor: parking_lot::Mutex<()>,
}

impl FileSource {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            #[cfg(not(any(unix, windows)))]
            cursor: parking_lot::Mutex::new(()),
        })
    }

    #[cfg(unix)]
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, bytes, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(&self.file, bytes, offset)
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::io::{Seek, SeekFrom};

        let _guard = self.cursor.lock();
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))?;
        file.read(bytes)
    }
}

impl GmaSource {
    fn len(&self) -> u64 {
        match self {
            Self::Mem(bytes) => bytes.len() as u64,
            Self::File(source) | Self::TempBacked { source, .. } => source.len,
        }
    }

    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let available = self.len().checked_sub(offset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "GMA range starts past EOF")
        })?;
        if available == 0 {
            return Ok(0);
        }
        let requested = usize::try_from(available.min(bytes.len() as u64))
            .expect("requested range is bounded by a usize buffer");

        match self {
            Self::Mem(source) => {
                let start = usize::try_from(offset).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "GMA offset is not addressable",
                    )
                })?;
                let end = start.checked_add(requested).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "GMA range overflow")
                })?;
                let source = source.get(start..end).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "GMA buffer was truncated")
                })?;
                bytes[..requested].copy_from_slice(source);
                Ok(requested)
            }
            Self::File(source) | Self::TempBacked { source, .. } => {
                source.read_at(&mut bytes[..requested], offset)
            }
        }
    }
}

/// A reader confined to one payload extent.
pub(super) struct GmaRangeReader<'a> {
    source: &'a GmaSource,
    offset: u64,
    remaining: u64,
}

impl Read for GmaRangeReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || bytes.is_empty() {
            return Ok(0);
        }
        let requested = usize::try_from(self.remaining.min(bytes.len() as u64))
            .expect("requested range is bounded by a usize buffer");
        let read = self.source.read_at(&mut bytes[..requested], self.offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "GMA changed while it was being read",
            ));
        }
        self.offset = self
            .offset
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("GMA range offset overflow"))?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// The source for one GMA read/extract operation. Opening a disk-backed view
/// holds a file handle and its initial length, but does not borrow file-backed
/// memory or load payloads eagerly.
pub struct GmaView {
    source: GmaSource,
}

impl GmaView {
    pub(crate) fn open_file(path: &Path) -> Result<Self, GmaError> {
        main_thread_forbidden!();
        Ok(Self {
            source: GmaSource::File(FileSource::open(path)?),
        })
    }

    /// A GMA decompressed into memory (workshop download); `path` names
    /// the original compressed payload for identity purposes. Also the
    /// door in-memory test fixtures come through.
    pub fn from_membuffer(bytes: ArcBytes) -> Self {
        Self {
            source: GmaSource::Mem(bytes),
        }
    }

    /// A GMA decompressed to a spill file; `path` keeps naming the
    /// original payload so the addon's identity (extracted-name
    /// fallback, dedup by path) is unchanged.
    pub(crate) fn from_temp_backing(temp_path: TempPath) -> Result<Self, GmaError> {
        let source = FileSource::open(&temp_path)?;
        Ok(Self {
            source: GmaSource::TempBacked {
                source,
                _guard: temp_path,
            },
        })
    }

    #[cfg(feature = "test-support")]
    /// Whether this view's bytes are a decompressed-to-disk spill file
    /// rather than an in-memory buffer or a directly opened addon file.
    /// Exposed for tests asserting `GmaFile::decompress`'s memory-vs-spill
    /// threshold; production code never branches on it.
    pub fn is_temp_backed(&self) -> bool {
        matches!(self.source, GmaSource::TempBacked { .. })
    }

    #[cfg(feature = "test-support")]
    /// The spill file's path, when [`Self::is_temp_backed`]. Exposed for
    /// tests asserting the spill file is deleted once nothing holds this
    /// view anymore.
    pub fn temp_backing_path(&self) -> Option<&Path> {
        match &self.source {
            GmaSource::TempBacked { _guard, .. } => Some(_guard.as_ref()),
            _ => None,
        }
    }

    fn range_reader(&self, offset: u64, len: u64) -> Result<GmaRangeReader<'_>, GmaError> {
        let end = offset.checked_add(len).ok_or(GmaError::FormatError)?;
        if end > self.source.len() {
            return Err(GmaError::FormatError);
        }
        Ok(GmaRangeReader {
            source: &self.source,
            offset,
            remaining: len,
        })
    }

    fn parse_index(&self) -> Result<ParsedGma, GmaError> {
        main_thread_forbidden!();
        let len = self.source.len();
        format::parse(self.range_reader(0, len)?, len)
    }

    /// Opens `path` for safe positional reads. The initial file size bounds
    /// every later range even if the path is replaced or the file is changed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        Self::open_file(path.as_ref())
    }

    /// Parses the header + metadata and builds the identity handle for
    /// this view's content; `path` names the addon for identity purposes
    /// (see the constructors above).
    pub fn handle(&self, path: impl AsRef<Path>) -> Result<GmaFile, GmaError> {
        let parsed = self.parse_index()?;
        Ok(self.handle_from_parsed(&parsed, path))
    }

    fn handle_from_parsed(&self, parsed: &ParsedGma, path: impl AsRef<Path>) -> GmaFile {
        let meta = &parsed.metadata;
        let mut gma = GmaFile {
            path: path.as_ref().to_owned(),
            size: self.source.len(),
            id: None,
            metadata: metadata_from_embedded_fields(meta.name.clone(), meta.description.clone()),
            version: meta.version,
            extracted_name: String::new(),
            modified: None,
        };
        gma.adopt_path_id_and_name();
        gma
    }

    /// Identity handle, header, and safe-path entry list from a **single**
    /// parse. [`Self::handle`], [`Self::header`] and [`Self::entries`]
    /// each re-walk the whole entry table; index and discovery reads use
    /// this instead.
    pub fn meta(&self, path: impl AsRef<Path>) -> Result<GmaMetaBundle, GmaError> {
        let parsed = self.parse_index()?;
        let handle = self.handle_from_parsed(&parsed, path);
        let header = GmaHeader {
            version: parsed.metadata.version,
            timestamp: parsed.metadata.timestamp,
            metadata: handle.metadata.clone(),
            author: parsed.metadata.author.clone(),
            addon_version: parsed.metadata.addon_version,
        };
        let entries = indexed_entries_from_parsed(&parsed);
        Ok(GmaMetaBundle {
            handle,
            header,
            entries,
        })
    }

    /// Header plus safe-path entry extents for library indexing, without
    /// constructing the extraction handle that preview/extraction needs.
    pub fn index_meta(&self) -> Result<GmaIndexBundle, GmaError> {
        let parsed = self.parse_index()?;
        Ok(GmaIndexBundle {
            header: header_from_parsed(&parsed),
            entries: indexed_entries_from_parsed(&parsed),
        })
    }

    pub fn header(&self) -> Result<GmaHeader, GmaError> {
        let parsed = self.parse_index()?;
        Ok(header_from_parsed(&parsed))
    }

    /// Safe-path-filtered entry projection, computed fresh on every call.
    /// Callers that need it persistently own the result (it is not a
    /// populated field).
    pub fn entries(&self) -> Result<HashMap<String, GmaEntry>, GmaError> {
        let parsed = self.parse_index()?;
        Ok(entries_from_parsed(&parsed))
    }

    /// Reads one entry's payload by path. Rejects unsafe paths the same
    /// way [`Self::entries`] filters them out of its projection.
    pub fn read_entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, GmaError> {
        if is_unsafe_entry_path(entry_path) {
            return Err(GmaError::EntryNotFound);
        }
        let parsed = self.parse_index()?;
        let entry = parsed
            .entries
            .iter()
            .find(|entry| entry.path == entry_path)
            .ok_or(GmaError::EntryNotFound)?;
        self.read_payload_bytes(entry.data_offset, entry.size)
    }

    /// Copies a payload extent recorded by [`Self::meta`] without reparsing
    /// the archive. Every access is checked against the backing bytes again.
    pub fn read_payload_bytes(&self, offset: u64, len: u64) -> Result<Vec<u8>, GmaError> {
        let len = usize::try_from(len).map_err(|_| GmaError::FormatError)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| GmaError::FormatError)?;
        bytes.resize(len, 0);
        self.range_reader(offset, len as u64)?
            .read_exact(&mut bytes)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    GmaError::FormatError
                } else {
                    error.into()
                }
            })?;
        Ok(bytes)
    }

    pub(super) fn payload_reader(
        &self,
        entry: &GmaIndexedEntry,
    ) -> Result<BufReader<GmaRangeReader<'_>>, GmaError> {
        Ok(BufReader::with_capacity(
            64 * 1024,
            self.range_reader(entry.data_offset, entry.size)?,
        ))
    }

    pub(super) fn extraction_entries(&self) -> Result<Vec<GmaIndexedEntry>, GmaError> {
        Ok(indexed_entries_from_parsed(&self.parse_index()?))
    }
}

fn entries_from_parsed(parsed: &ParsedGma) -> HashMap<String, GmaEntry> {
    let mut entries = HashMap::with_capacity(parsed.entries.len());
    for (index, entry) in parsed.entries.iter().enumerate() {
        // An entry whose path could escape an extraction root is skipped,
        // not fatal — real workshop archives contain them.
        if is_unsafe_entry_path(&entry.path) {
            log::warn!("Illegal GMA entry: {}", entry.path);
            continue;
        }
        entries.insert(
            entry.path.clone(),
            GmaEntry {
                path: entry.path.clone(),
                size: entry.size,
                crc: entry.crc,
                index: index as u64,
            },
        );
    }
    entries
}

fn header_from_parsed(parsed: &ParsedGma) -> GmaHeader {
    let meta = &parsed.metadata;
    GmaHeader {
        version: meta.version,
        timestamp: meta.timestamp,
        metadata: metadata_from_embedded_fields(meta.name.clone(), meta.description.clone()),
        author: meta.author.clone(),
        addon_version: meta.addon_version,
    }
}

fn indexed_entries_from_parsed(parsed: &ParsedGma) -> Vec<GmaIndexedEntry> {
    let mut entries = Vec::with_capacity(parsed.entries.len());
    for entry in &parsed.entries {
        if is_unsafe_entry_path(&entry.path) {
            log::warn!("Illegal GMA entry: {}", entry.path);
            continue;
        }
        entries.push(GmaIndexedEntry {
            path: entry.path.clone(),
            size: entry.size,
            crc: entry.crc,
            data_offset: entry.data_offset,
        });
    }
    entries
}

/// Everything a single [`GmaView::meta`] parse yields: the identity
/// handle, the header, and the safe-path entry list in table order.
#[derive(Clone, Debug)]
pub struct GmaMetaBundle {
    pub handle: GmaFile,
    pub header: GmaHeader,
    pub entries: Vec<GmaIndexedEntry>,
}

#[derive(Clone, Debug)]
pub struct GmaIndexBundle {
    pub header: GmaHeader,
    pub entries: Vec<GmaIndexedEntry>,
}

#[derive(Clone, Debug)]
pub struct GmaIndexedEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,
    pub data_offset: u64,
}

impl GmaFile {
    /// Opens this addon's bytes for one bounded, positional read/extract
    /// operation. Membuffer/spill flows keep their original view instead.
    pub fn view(&self) -> Result<GmaView, GmaError> {
        GmaView::open_file(&self.path)
    }

    pub fn header(&self) -> Result<GmaHeader, GmaError> {
        self.view()?.header()
    }

    /// One-open, one-parse read: handle + header + entry list together.
    pub fn open_meta<P: AsRef<Path>>(path: P) -> Result<GmaMetaBundle, GmaError> {
        GmaView::open_file(path.as_ref())?.meta(path)
    }

    /// One-open, one-parse library index without an extraction handle.
    pub fn open_index<P: AsRef<Path>>(path: P) -> Result<GmaIndexBundle, GmaError> {
        GmaView::open_file(path.as_ref())?.index_meta()
    }

    /// One-open, one-parse header read without projecting the entry table.
    pub fn open_header<P: AsRef<Path>>(path: P) -> Result<GmaHeader, GmaError> {
        GmaView::open_file(path.as_ref())?.header()
    }
}

fn metadata_from_embedded_fields(
    embedded_title: String,
    embedded_description: String,
) -> GmaMetadata {
    match serde_json::de::from_str::<super::StandardManifest>(&embedded_description) {
        Ok(manifest) if manifest.is_manifest() => GmaMetadata::Standard {
            title: embedded_title,
            addon_type: manifest.addon_type.unwrap_or_default(),
            tags: manifest.tags.unwrap_or_default(),
            ignore: manifest.ignore.unwrap_or_default(),
        },
        _ => GmaMetadata::Legacy {
            title: embedded_title,
            description: embedded_description,
        },
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::metadata_from_embedded_fields;
    use crate::gma::GmaMetadata;

    #[test]
    fn a_manifest_description_becomes_standard() {
        let metadata = metadata_from_embedded_fields(
            "Addon".to_owned(),
            r#"{"type":"map","tags":["scenic"]}"#.to_owned(),
        );

        assert!(matches!(metadata, GmaMetadata::Standard { .. }));
        assert_eq!(metadata.addon_type(), Some("map"));
        assert_eq!(metadata.tags(), Some(&["scenic".to_owned()][..]));
        assert_eq!(metadata.title(), "Addon");
    }

    /// Free text that happens to parse as JSON is not a manifest. Untagged
    /// deserialization swallows these into an empty `Standard` and discards
    /// the description, which is why the manifest keys are checked explicitly.
    #[test]
    fn a_json_shaped_description_without_manifest_keys_stays_legacy() {
        for description in ["{}", r#"{"note":"see the workshop page"}"#] {
            let metadata =
                metadata_from_embedded_fields("Addon".to_owned(), description.to_owned());

            let GmaMetadata::Legacy {
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

        assert!(matches!(metadata, GmaMetadata::Legacy { .. }));
        assert_eq!(metadata.title(), "Addon");
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::gma::{GmaError, is_unsafe_entry_path};

    use super::GmaView;

    fn raw_gma(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn c_string(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GMAD");
        bytes.push(3);
        bytes.extend_from_slice(&76561198000000000u64.to_le_bytes());
        bytes.extend_from_slice(&123456789u64.to_le_bytes());
        c_string(&mut bytes, "required-game");
        c_string(&mut bytes, "");
        c_string(&mut bytes, "Streaming fixture");
        c_string(&mut bytes, "fixture description");
        c_string(&mut bytes, "Fixture author");
        bytes.extend_from_slice(&7i32.to_le_bytes());

        for (index, (path, payload)) in entries.iter().enumerate() {
            bytes.extend_from_slice(&u32::try_from(index + 1).unwrap().to_le_bytes());
            c_string(&mut bytes, path);
            bytes.extend_from_slice(&i64::try_from(payload.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(&vformats::crc32_ieee(payload).to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for (_, payload) in entries {
            bytes.extend_from_slice(payload);
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn payload_extent_reads_are_bounds_checked() {
        let view = GmaView::from_membuffer(vec![1, 2, 3, 4].into());

        assert_eq!(view.read_payload_bytes(1, 2).unwrap(), vec![2, 3]);
        assert_eq!(view.read_payload_bytes(4, 0).unwrap(), Vec::<u8>::new());
        assert!(matches!(
            view.read_payload_bytes(4, 1),
            Err(GmaError::FormatError)
        ));
        assert!(matches!(
            view.read_payload_bytes(u64::MAX, 2),
            Err(GmaError::FormatError)
        ));
    }

    #[test]
    fn streaming_index_matches_the_canonical_slice_parser() {
        let bytes = raw_gma(&[
            ("lua/autorun/fixture.lua", b"print('fixture')"),
            ("materials/fixture.vmt", b"VertexLitGeneric {}"),
        ]);
        let canonical = vformats::gma::parse(
            &bytes,
            &vformats::Limits {
                max_input_bytes: u64::MAX,
                max_entry_bytes: u64::MAX,
                ..vformats::Limits::default()
            },
        )
        .unwrap();
        let view = GmaView::from_membuffer(bytes.clone().into());
        let parsed = view.parse_index().unwrap();

        assert_eq!(parsed.metadata.version, canonical.metadata.version);
        assert_eq!(parsed.metadata.timestamp, canonical.metadata.timestamp);
        assert_eq!(parsed.metadata.name, canonical.metadata.name);
        assert_eq!(parsed.metadata.description, canonical.metadata.description);
        assert_eq!(parsed.metadata.author, canonical.metadata.author);
        assert_eq!(
            parsed.metadata.addon_version,
            canonical.metadata.addon_version
        );
        assert_eq!(parsed.entries.len(), canonical.entries().len());
        for (index, (entry, canonical_entry)) in
            parsed.entries.iter().zip(canonical.entries()).enumerate()
        {
            assert_eq!(entry.path, canonical_entry.path);
            assert_eq!(entry.size, canonical_entry.size);
            assert_eq!(entry.crc, canonical_entry.crc32);
            assert_eq!(
                view.read_payload_bytes(entry.data_offset, entry.size)
                    .unwrap(),
                canonical.entry_bytes(index).unwrap()
            );
        }
    }

    #[test]
    fn truncating_an_open_archive_is_a_normal_error() {
        let bytes = raw_gma(&[("lua/autorun/fixture.lua", b"payload")]);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();

        let view = GmaView::open(file.path()).unwrap();
        let entry = view
            .meta(file.path())
            .unwrap()
            .entries
            .into_iter()
            .next()
            .unwrap();
        file.as_file()
            .set_len(entry.data_offset + entry.size - 1)
            .unwrap();

        assert!(matches!(
            view.read_payload_bytes(entry.data_offset, entry.size),
            Err(GmaError::FormatError)
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
