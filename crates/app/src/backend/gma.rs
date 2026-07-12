use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use gmpublished_backend::{
    GMAFile, Transaction,
    gma::{GMAEntry, read::GmaView},
};

pub use gmpublished_backend::{
    GMAError as GmaError,
    gma::{ExtractDestination, ExtractOptions, ExtractionOverwriteMode, Whitelist, whitelist},
};

#[cfg(test)]
pub const GMA_VERSION: u8 = 3;

/// Safe, already-validated path for one file entry inside a GMA archive.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchiveEntryPath(String);

impl ArchiveEntryPath {
    pub(crate) fn from_validated(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (!is_unsafe_entry_path(&path)).then_some(Self(path))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }

    pub(crate) fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.0.as_str(), |(_, file_name)| file_name)
    }

    pub(crate) fn parent(&self) -> ArchiveDirectoryPath {
        self.0
            .rsplit_once('/')
            .map_or_else(ArchiveDirectoryPath::root, |(parent, _)| {
                ArchiveDirectoryPath(parent.to_owned())
            })
    }

    pub(crate) fn directory_chain(&self) -> Vec<ArchiveDirectoryPath> {
        let mut directories = Vec::new();
        let mut current = ArchiveDirectoryPath::root();
        let mut components = self.0.split('/').peekable();

        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            current = current
                .join_child(component)
                .expect("validated archive entry paths contain only safe directory segments");
            directories.push(current.clone());
        }

        directories
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum GmaMetadata {
    Standard {
        title: String,
        addon_type: String,
        tags: Vec<String>,
        ignore: Vec<String>,
    },
    Legacy {
        title: String,
        description: String,
    },
}

impl GmaMetadata {
    pub(crate) fn title(&self) -> &str {
        match self {
            Self::Standard { title, .. } | Self::Legacy { title, .. } => title,
        }
    }

    pub(crate) fn addon_type(&self) -> Option<&str> {
        match self {
            Self::Standard { addon_type, .. } => Some(addon_type.as_str()),
            Self::Legacy { .. } => None,
        }
    }

    pub(crate) fn tags(&self) -> Option<&Vec<String>> {
        match self {
            Self::Standard { tags, .. } => Some(tags),
            Self::Legacy { .. } => None,
        }
    }
}

impl From<gmpublished_backend::GMAMetadata> for GmaMetadata {
    fn from(metadata: gmpublished_backend::GMAMetadata) -> Self {
        match metadata {
            gmpublished_backend::GMAMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title,
                addon_type,
                tags,
                ignore,
            },
            gmpublished_backend::GMAMetadata::Legacy { title, description } => {
                Self::Legacy { title, description }
            }
        }
    }
}

impl From<GmaMetadata> for gmpublished_backend::GMAMetadata {
    fn from(metadata: GmaMetadata) -> Self {
        match metadata {
            GmaMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title,
                addon_type,
                tags,
                ignore,
            },
            GmaMetadata::Legacy { title, description } => Self::Legacy { title, description },
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaHeader {
    pub(crate) version: u8,
    pub(crate) timestamp: u64,
    pub(crate) metadata: GmaMetadata,
    pub(crate) author: String,
    pub(crate) addon_version: i32,
}

impl GmaHeader {
    pub(crate) fn title(&self) -> &str {
        self.metadata.title()
    }
}

impl From<gmpublished_backend::GMAHeader> for GmaHeader {
    fn from(header: gmpublished_backend::GMAHeader) -> Self {
        Self {
            version: header.version,
            timestamp: header.timestamp,
            metadata: header.metadata.into(),
            author: header.author,
            addon_version: header.addon_version,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaIndex {
    pub(crate) header: GmaHeader,
    pub(crate) entries: Vec<GmaIndexEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaIndexEntry {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
    pub(crate) data_offset: u64,
}

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
    pub(crate) entries: Vec<GmaMetaEntry>,
}

impl GmaMeta {
    pub(crate) fn title(&self) -> &str {
        self.header.title()
    }

    #[cfg(test)]
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let archive = PreviewArchive::open(path.as_ref())?;
        Ok(Self::from_index(
            path.as_ref().to_path_buf(),
            archive.index().clone(),
        ))
    }

    /// Opens only the archive header for library discovery. `PreviewArchive`
    /// deliberately remains the full-entry path for the preview modal.
    #[cfg(any(test, not(feature = "asset-studio")))]
    pub(crate) fn open_header_only(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let path = path.as_ref();
        let gma = GMAFile::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            header: gma.header()?.into(),
            entries: Vec::new(),
        })
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn open_index(path: impl AsRef<Path>) -> Result<Self, GmaError> {
        let path = path.as_ref();
        let gma = GMAFile::open(path)?;
        let header = gma.header()?.into();
        let entries = index_entries_from_backend(&gma.view()?.entries()?)
            .into_iter()
            .map(|entry| GmaMetaEntry {
                path: entry.path,
                size: entry.size,
                crc32: entry.crc32,
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries,
        })
    }

    #[cfg(test)]
    fn from_index(path: PathBuf, index: GmaIndex) -> Self {
        Self {
            path,
            header: index.header,
            entries: index
                .entries
                .into_iter()
                .map(|entry| GmaMetaEntry {
                    path: entry.path,
                    size: entry.size,
                    crc32: entry.crc32,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEntry {
    pub(crate) path: ArchiveEntryPath,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
    pub(crate) index: u32,
    pub(crate) data_offset: u64,
}

/// Bytes provider and parsed identity for one open preview archive. `view`
/// is wrapped in `Arc` purely so the struct stays `Clone` (a memory map
/// cannot be); it plays no part in the archive's identity, so `Debug` and
/// `PartialEq` are hand-written to skip it.
#[derive(Clone)]
pub struct PreviewArchive {
    gma: GMAFile,
    view: Arc<GmaView>,
    index: GmaIndex,
    entries: Vec<PreviewEntry>,
}

impl fmt::Debug for PreviewArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreviewArchive")
            .field("gma", &self.gma)
            .field("index", &self.index)
            .field("entries", &self.entries)
            .finish()
    }
}
impl PartialEq for PreviewArchive {
    fn eq(&self, other: &Self) -> bool {
        self.gma == other.gma && self.index == other.index && self.entries == other.entries
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
        let mut gma = GMAFile::open(path)?;
        if let Some(id) = workshop_id.or_else(|| workshop_id_from_path(path)) {
            // Set the id before fetching the header so the recomputed
            // extracted_name includes both title and id.
            gma.set_ws_id(gmpublished_backend::appdata::SettingsPublishedFileId(id));
        }
        let header = gma.header()?.into();
        let view = gma.view()?;
        let index_entries = index_entries_from_backend(&view.entries()?);
        let index = GmaIndex {
            header,
            entries: index_entries,
        };
        let entries = preview_entries_from_index(&index);

        Ok(Self {
            gma,
            view: Arc::new(view),
            index,
            entries,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_gma(gma: FixtureGmaFile) -> Result<Self, GmaError> {
        preview_archive_from_fixture(gma)
    }

    pub(crate) const fn index(&self) -> &GmaIndex {
        &self.index
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

    pub(crate) fn entry(&self, path: &str) -> Result<&GmaIndexEntry, GmaError> {
        if is_unsafe_entry_path(path) {
            return Err(GmaError::FormatError);
        }
        self.index
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .ok_or(GmaError::EntryNotFound)
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn entry_bytes(&self, entry_path: &str) -> Result<Vec<u8>, GmaError> {
        self.entry(entry_path)?;
        self.view.read_entry_bytes(entry_path)
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
            false,
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
    let file_name = file_name.as_ref();
    let file_name = file_name.strip_prefix("ds_").unwrap_or(file_name);

    if let Ok(id) = file_name.parse::<u64>() {
        return Some(id);
    }

    let id = extract_suffix_workshop_id(file_name);
    (id != 0).then_some(id)
}

#[cfg(test)]
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

fn index_entries_from_backend(entries: &HashMap<String, GMAEntry>) -> Vec<GmaIndexEntry> {
    let mut entries = entries
        .values()
        .map(|entry| GmaIndexEntry {
            path: entry.path.clone(),
            size: entry.size,
            crc32: entry.crc,
            data_offset: entry.index,
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn preview_entries_from_index(index: &GmaIndex) -> Vec<PreviewEntry> {
    let mut entries = Vec::with_capacity(index.entries.len());
    for (position, entry) in index.entries.iter().enumerate() {
        if let Some(path) = ArchiveEntryPath::from_validated(entry.path.clone()) {
            entries.push(PreviewEntry {
                path,
                size: entry.size,
                crc32: entry.crc32,
                index: u32::try_from(position + 1).unwrap_or(u32::MAX),
                data_offset: entry.data_offset,
            });
        }
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    entries
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

fn is_unsafe_entry_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if path.bytes().any(|byte| matches!(byte, 0 | b':' | b'\\')) {
        return true;
    }
    if path.starts_with('/') {
        return true;
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment != segment.trim() {
            return true;
        }
    }
    false
}

// Deliberate divergence: upstream's extract_suffix_ws_id computes
// `(id + digit) * 10` per step, returning the id multiplied by 10 for
// `name_123`-style suffixes (its pure-numeric fast path hides this).
fn extract_suffix_workshop_id(file_name: &str) -> u64 {
    let mut id = 0_u64;
    for digit in file_name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        match 10_u64.checked_mul(id) {
            None => return 0,
            Some(next) => match next.checked_add(u64::from(digit.to_digit(10).unwrap())) {
                None => return 0,
                Some(next) => id = next,
            },
        }
    }
    id
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
    let backend_metadata: gmpublished_backend::GMAMetadata = gma.header.metadata.clone().into();
    let (title, description) = match &backend_metadata {
        gmpublished_backend::GMAMetadata::Legacy { title, description } => {
            (title.clone(), description.clone())
        }
        gmpublished_backend::GMAMetadata::Standard { title, .. } => (
            title.clone(),
            serde_json::to_string(&backend_metadata).expect("fixture metadata serializes"),
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
    let view = GmaView::from_membuffer(bytes.into(), &path);
    let backend = view.handle(&path).map_err(|_| GmaError::FormatError)?;
    let view_entries = view.entries().map_err(|_| GmaError::FormatError)?;

    let index = GmaIndex {
        header: gma.header,
        entries: index_entries_from_backend(&view_entries),
    };
    let entries = preview_entries_from_index(&index);
    Ok(PreviewArchive {
        gma: backend,
        view: Arc::new(view),
        index,
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
