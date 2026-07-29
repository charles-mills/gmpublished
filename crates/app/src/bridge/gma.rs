use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use gmpublished_backend::{
    GmaFile, Transaction,
    gma::{is_unsafe_entry_path, read::GmaView},
};

pub use gmpublished_backend::{
    GmaError,
    gma::{ExtractDestination, ExtractOptions, ExtractionOverwriteMode, Whitelist, whitelist},
};

#[cfg(test)]
pub const GMA_VERSION: u8 = 3;

/// Safe, already-validated path for one file entry inside a GMA archive.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchiveEntryPath(Arc<String>);

impl ArchiveEntryPath {
    pub(crate) fn from_validated(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (!is_unsafe_entry_path(&path)).then(|| Self(Arc::new(path)))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn shared_string(&self) -> Arc<String> {
        Arc::clone(&self.0)
    }

    pub(crate) fn into_string(self) -> String {
        Arc::try_unwrap(self.0).unwrap_or_else(|path| path.as_ref().clone())
    }

    pub(crate) fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.0.as_str(), |(_, file_name)| file_name)
    }
}

impl AsRef<str> for ArchiveEntryPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ArchiveEntryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<ArchiveEntryPath> for String {
    fn from(path: ArchiveEntryPath) -> Self {
        path.into_string()
    }
}

/// Safe archive directory path used by the archive-browser presentation model.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchiveDirectoryPath(String);

impl ArchiveDirectoryPath {
    pub(crate) fn root() -> Self {
        Self(String::new())
    }

    pub(crate) fn from_validated(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (path.is_empty() || !is_unsafe_entry_path(&path)).then_some(Self(path))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }

    pub(crate) fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            Some(
                self.0
                    .rsplit_once('/')
                    .map_or(self.0.as_str(), |(_, file_name)| file_name),
            )
        }
    }

    pub(crate) fn join_child(&self, child: &str) -> Option<Self> {
        if !is_safe_archive_path_segment(child) {
            return None;
        }
        if self.is_root() {
            Some(Self(child.to_owned()))
        } else {
            Some(Self(format!("{}/{child}", self.0)))
        }
    }
}

impl Default for ArchiveDirectoryPath {
    fn default() -> Self {
        Self::root()
    }
}

impl AsRef<str> for ArchiveDirectoryPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ArchiveDirectoryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<ArchiveDirectoryPath> for String {
    fn from(path: ArchiveDirectoryPath) -> Self {
        path.into_string()
    }
}

/// The backend's own types.
///
/// Not mirrored: both were duplicated field-for-field *and* accessor-for-
/// accessor, with identity `From` impls in each direction. A newtype earns its
/// conversions by refining something — see [`PublishedFileId`], which enforces
/// non-zero where the backend's does not. These refined nothing.
pub use gmpublished_backend::{GmaHeader, GmaMetadata};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaMetaEntry {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaMeta {
    pub(crate) path: PathBuf,
    pub(crate) header: GmaHeader,
    /// Immutable archive tables are shared between the persistent header
    /// cache and each published library snapshot. Refreshes clone the `Arc`,
    /// not every path string in the archive.
    pub(crate) entries: Arc<[GmaMetaEntry]>,
}

impl GmaMeta {
    pub(crate) fn title(&self) -> &str {
        self.header.title()
    }

    #[cfg(test)]
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let archive = PreviewArchive::open(path.as_ref())?;
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            header: archive.header.clone(),
            entries: archive
                .entries
                .iter()
                .map(|entry| GmaMetaEntry {
                    path: entry.path.as_str().to_owned(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
                .collect::<Vec<_>>()
                .into(),
        })
    }

    /// Opens only the archive header for library discovery. `PreviewArchive`
    /// deliberately remains the full-entry path for the preview modal.
    #[cfg(test)]
    pub(crate) fn open_header_only(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let path = path.as_ref();
        Ok(Self {
            path: path.to_path_buf(),
            header: GmaFile::open_header(path)?,
            entries: Arc::from([]),
        })
    }

    pub(crate) fn open_index(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let path = path.as_ref();
        // One mmap + one parse; the previous open/header/entries chain
        // re-parsed the whole entry table three times.
        let bundle = GmaFile::open_index(path)?;
        let mut entries: Vec<GmaMetaEntry> = bundle
            .entries
            .into_iter()
            .map(|entry| GmaMetaEntry {
                path: entry.path,
                size: entry.size,
                crc32: entry.crc,
            })
            .collect();
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(Self {
            path: path.to_path_buf(),
            header: bundle.header,
            entries: entries.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEntry {
    pub(crate) path: ArchiveEntryPath,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
    data_offset: u64,
}

/// Bytes provider and parsed identity for one open preview archive. `view`
/// is wrapped in `Arc` purely so the struct stays `Clone` (a memory map
/// cannot be); it plays no part in the archive's identity, so `Debug` and
/// `PartialEq` are hand-written to skip it.
#[derive(Clone)]
pub struct PreviewArchive {
    gma: GmaFile,
    view: Arc<GmaView>,
    header: GmaHeader,
    entries: Vec<PreviewEntry>,
}

impl fmt::Debug for PreviewArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreviewArchive")
            .field("gma", &self.gma)
            .field("header", &self.header)
            .field("entries", &self.entries)
            .finish()
    }
}
impl PartialEq for PreviewArchive {
    fn eq(&self, other: &Self) -> bool {
        self.gma == other.gma && self.header == other.header && self.entries == other.entries
    }
}
impl Eq for PreviewArchive {}

impl PreviewArchive {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        Self::open_with_workshop_id(path, None)
    }

    /// Opens the archive and stamps its workshop id (explicit id from the
    /// caller, else derived from the path) so `extracted_name` carries the
    /// `<title>_<id>` suffix that extraction folders are named by.
    pub(crate) fn open_with_workshop_id(
        path: impl AsRef<Path>,
        workshop_id: Option<u64>,
    ) -> Result<Self, GmaError> {
        let path = path.as_ref();
        // One mmap + one parse for handle, header and entries together;
        // the view is kept for entry fetches during the preview session.
        let view = GmaView::open(path)?;
        let bundle = view.meta(path)?;
        let mut gma = bundle.handle;
        if let Some(id) = workshop_id.or_else(|| workshop_id_from_path(path)) {
            // Recomputes extracted_name so it includes both title and id.
            gma.set_ws_id(gmpublished_backend::appdata::SettingsPublishedFileId(id));
        }
        let header = bundle.header;
        let entries = preview_entries_from_backend(bundle.entries);

        Ok(Self {
            gma,
            view: Arc::new(view),
            header,
            entries,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_gma(gma: FixtureGmaFile) -> Result<Self, GmaError> {
        preview_archive_from_fixture(gma)
    }

    pub(crate) const fn header(&self) -> &GmaHeader {
        &self.header
    }

    /// Sanitized folder name the archive extracts into (backend
    /// `extracted_name`); empty when the metadata carried no usable name.
    pub(crate) fn extracted_name(&self) -> &str {
        &self.gma.extracted_name
    }

    pub(crate) fn entries(&self) -> &[PreviewEntry] {
        &self.entries
    }

    /// Owned snapshot of the entry list, for callers that need `self` to
    /// keep living alongside a moved-from copy of its entries (e.g.
    /// building another owner of `self` from the same entries).
    pub(crate) fn entries_owned(&self) -> Vec<PreviewEntry> {
        self.entries.clone()
    }

    pub(crate) fn entry(&self, path: &str) -> Result<&PreviewEntry, GmaError> {
        if is_unsafe_entry_path(path) {
            return Err(GmaError::FormatError);
        }
        self.entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .map(|index| &self.entries[index])
            .map_err(|_| GmaError::EntryNotFound)
    }

    pub(crate) fn entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, GmaError> {
        let entry = self.entry(entry_path)?;
        self.view.read_payload_bytes(entry.data_offset, entry.size)
    }

    pub(crate) fn extract_entry_with_transaction(
        &self,
        entry_path: &str,
        transaction: &Transaction,
        backend: &gmpublished_backend::Backend,
    ) -> Result<PathBuf, GmaError> {
        self.entry(entry_path)?;
        self.view.extract_entry(
            &self.gma,
            entry_path.to_owned(),
            transaction,
            ExtractOptions {
                open_after: false,
                whitelist: Whitelist::Ignore,
            },
            &backend.app_data,
            &backend.steam,
        )
    }

    pub(crate) fn extract_all_with_transaction(
        &self,
        destination: ExtractDestination,
        options: &PreviewExtractOptions,
        transaction: &Transaction,
        backend: &gmpublished_backend::Backend,
    ) -> Result<PathBuf, GmaError> {
        self.view.extract(
            &self.gma,
            destination,
            transaction,
            ExtractOptions {
                open_after: false,
                whitelist: if options.ignore_whitelist {
                    Whitelist::Ignore
                } else {
                    Whitelist::Enforce
                },
            },
            &backend.whitelist,
            &backend.app_data,
            &backend.steam,
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PreviewExtractRequest {
    pub(crate) destination: ExtractDestination,
    pub(crate) options: PreviewExtractOptions,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PreviewExtractOptions {
    pub(crate) ignore_whitelist: bool,
}

impl Default for PreviewExtractOptions {
    fn default() -> Self {
        Self {
            ignore_whitelist: true,
        }
    }
}

pub fn build_preview_extract_request(
    mut settings: super::Settings,
    paths: &super::AppPaths,
) -> PreviewExtractRequest {
    settings.sanitize(paths);
    PreviewExtractRequest {
        destination: settings.extract_destination,
        options: PreviewExtractOptions::default(),
    }
}

pub fn is_gma_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gma"))
}

/// Derives the owning workshop id from a numeric-suffixed file stem
/// (`dachi_2575621404.gma`, `ds_123.gma`), else from the numeric
/// workshop-content folder holding the archive
/// (`.../content/4000/2575621404/temp.gma`).
fn workshop_id_from_path(path: &Path) -> Option<u64> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(workshop_id_from_filename)
        .or_else(|| {
            path.parent()
                .and_then(|dir| dir.file_name())
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u64>().ok())
        })
}

pub fn workshop_id_from_filename(file_name: impl AsRef<str>) -> Option<u64> {
    gmpublished_backend::gma::ws_id_from_file_name(file_name).map(|id| id.0)
}

#[cfg(test)]
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

fn preview_entries_from_backend(
    entries: Vec<gmpublished_backend::GmaIndexedEntry>,
) -> Vec<PreviewEntry> {
    let mut preview = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(path) = ArchiveEntryPath::from_validated(entry.path) {
            preview.push(PreviewEntry {
                path,
                size: entry.size,
                crc32: entry.crc,
                data_offset: entry.data_offset,
            });
        }
    }
    preview.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    preview
}

fn is_safe_archive_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment == segment.trim()
        && !segment
            .bytes()
            .any(|byte| matches!(byte, 0 | b':' | b'/' | b'\\'))
}

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FixtureGmaEntry {
    pub(crate) path: String,
    pub(crate) crc32: u32,
}

#[cfg(test)]
impl FixtureGmaEntry {
    pub(crate) fn new(path: impl Into<String>, crc32: u32) -> Self {
        Self {
            path: path.into(),
            crc32,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FixtureGmaFile {
    pub(crate) path: Option<PathBuf>,
    pub(crate) header: GmaHeader,
    pub(crate) entries: Vec<FixtureGmaEntry>,
    pub(crate) data: Vec<Vec<u8>>,
    pub(crate) trailer_crc32: u32,
}

#[cfg(test)]
fn preview_archive_from_fixture(gma: FixtureGmaFile) -> Result<PreviewArchive, GmaError> {
    // Serialize the fixture into a real GMA byte stream so the backend
    // parses it exactly like production content (including skipping
    // entries with unsafe paths, which the fixture places on purpose —
    // the reason this cannot go through the safety-enforcing writer).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GMAD");
    bytes.push(gma.header.version);
    bytes.extend_from_slice(&0_u64.to_le_bytes()); // steamid
    bytes.extend_from_slice(&gma.header.timestamp.to_le_bytes());
    if gma.header.version > 1 {
        bytes.push(0); // required content
    }
    let (title, description) = match &gma.header.metadata {
        gmpublished_backend::GmaMetadata::Legacy { title, description } => {
            (title.clone(), description.clone())
        }
        gmpublished_backend::GmaMetadata::Standard { title, .. } => (
            title.clone(),
            serde_json::to_string(&gma.header.metadata).expect("fixture metadata serializes"),
        ),
    };
    for field in [&title, &description, &gma.header.author] {
        bytes.extend_from_slice(field.as_bytes());
        bytes.push(0);
    }
    bytes.extend_from_slice(&gma.header.addon_version.to_le_bytes());
    for (number, (entry, contents)) in gma.entries.iter().zip(&gma.data).enumerate() {
        bytes.extend_from_slice(&u32::try_from(number + 1).unwrap().to_le_bytes());
        bytes.extend_from_slice(entry.path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&(contents.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&entry.crc32.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for contents in &gma.data {
        bytes.extend_from_slice(contents);
    }
    bytes.extend_from_slice(&gma.trailer_crc32.to_le_bytes());

    let path = gma.path.unwrap_or_else(|| PathBuf::from("fixture.gma"));
    let view = GmaView::from_membuffer(bytes.into());
    let bundle = view.meta(&path).map_err(|_| GmaError::FormatError)?;
    let backend = bundle.handle;
    let entries = preview_entries_from_backend(bundle.entries);
    let header = gma.header;
    Ok(PreviewArchive {
        gma: backend,
        view: Arc::new(view),
        header,
        entries,
    })
}

#[cfg(test)]
mod workshop_id_tests {
    use super::workshop_id_from_path;
    use std::path::Path;

    #[test]
    fn workshop_id_derives_from_stem_then_content_folder() {
        assert_eq!(
            workshop_id_from_path(Path::new("/tmp/dachi_2575621404.gma")),
            Some(2575621404)
        );
        assert_eq!(
            workshop_id_from_path(Path::new("/tmp/ds_123456.gma")),
            Some(123456)
        );
        // Installed workshop layout: the numeric content folder carries the id.
        assert_eq!(
            workshop_id_from_path(Path::new(
                "/Steam/steamapps/workshop/content/4000/2575621404/temp.gma"
            )),
            Some(2575621404)
        );
        assert_eq!(workshop_id_from_path(Path::new("/tmp/my addon.gma")), None);
    }
}

#[cfg(test)]
mod tests {
    use super::{GmaMeta, GmaMetadata};
    use crate::test_support::{GmaFixtureBuilder, TestDir, write_gma_fixture};

    #[test]
    fn open_header_only_matches_full_open_for_standard_fixture() {
        let mut fixture = GmaFixtureBuilder::new("Standard Fixture")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .entry("materials/example.vmt", b"material".to_vec())
            .build();
        fixture.header.metadata = GmaMetadata::Standard {
            title: "Standard Fixture".to_owned(),
            addon_type: "servercontent".to_owned(),
            tags: vec!["build".to_owned(), "fun".to_owned()],
            ignore: vec!["*.psd".to_owned()],
        };
        assert_header_only_matches_full_open(&fixture, "standard.gma");
    }

    #[test]
    fn open_header_only_matches_full_open_for_legacy_fixture() {
        let mut fixture = GmaFixtureBuilder::new("Legacy Fixture")
            .entry(
                "lua/autorun/client/cl_init.lua",
                b"print('legacy')\n".to_vec(),
            )
            .build();
        fixture.header.metadata = GmaMetadata::Legacy {
            title: "Legacy Fixture".to_owned(),
            description: "A legacy addon description".to_owned(),
        };
        assert_header_only_matches_full_open(&fixture, "legacy.gma");
    }

    fn assert_header_only_matches_full_open(fixture: &super::FixtureGmaFile, file_name: &str) {
        let dir = TestDir::new("gmpublished-gma-header-only");
        let path = write_gma_fixture(dir.join(file_name), fixture);

        let full = GmaMeta::open(&path).expect("full gma open");
        let header_only = GmaMeta::open_header_only(&path).expect("header-only gma open");

        assert_eq!(header_only.header, full.header);
        assert!(header_only.entries.is_empty());
        assert_eq!(full.entries.len(), fixture.entries.len());
    }
}
