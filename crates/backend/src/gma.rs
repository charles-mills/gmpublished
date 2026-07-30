//! The GMA container: reading an addon archive, writing one, extracting
//! entries, and the whitelist that decides what may go in.
//!
//! [`vformats::gma`] is the canonical whole-buffer format implementation and
//! writer. This module adds the addon-shaped layer — identity (workshop id,
//! extracted name), safe-path rules, and a narrow owned-index reader that can
//! stream multi-gigabyte on-disk payloads without mapping or allocating them.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::WorkshopId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const GMA_HEADER: &[u8; 4] = b"GMAD";
/// GMA format version emitted by this writer and accepted as the newest
/// supported read version.
pub const GMA_VERSION: u8 = 3;

/// Zero is never a real Steam Workshop id; treat it as "no id" wherever a
/// digit-suffix or folder-name parse can produce it.
pub(crate) fn nonzero_workshop_id(id: u64) -> Option<WorkshopId> {
    WorkshopId::new(id)
}

/// Recovers a workshop id from a GMA file's name: `ds_`-prefixed folder ids,
/// bare numeric names, or a trailing digit suffix on a descriptive name.
pub fn ws_id_from_file_name<S: AsRef<str>>(file_name: S) -> Option<WorkshopId> {
    let file_name = file_name.as_ref();
    let file_name = file_name.strip_prefix("ds_").unwrap_or(file_name);

    if let Ok(id) = file_name.parse::<u64>() {
        return nonzero_workshop_id(id);
    }

    extract_suffix_ws_id(file_name)
}

// Deliberate divergence from upstream, which computes `(id + digit) * 10`
// per step and so returns the id multiplied by 10 for `name_123`-style
// suffixes (the pure-numeric fast path in ws_id_from_file_name hides this).
fn extract_suffix_ws_id<S: AsRef<str>>(file_name: S) -> Option<WorkshopId> {
    let file_name = file_name.as_ref();
    let start = file_name
        .as_bytes()
        .iter()
        .rposition(|byte| !byte.is_ascii_digit())
        .map_or(0, |index| index + 1);
    let digits = file_name.get(start..)?;
    (!digits.is_empty())
        .then(|| digits.parse::<u64>().ok())
        .flatten()
        .and_then(nonzero_workshop_id)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
/// Failures produced while reading, writing, or extracting GMA archives.
pub enum GmaError {
    #[error("GMA I/O failed")]
    IoError(#[source] crate::IoFailure),
    #[error("the GMA is malformed")]
    FormatError,
    #[error("the GMA header is not recognisable")]
    InvalidHeader,
    #[error("no such entry in the GMA")]
    EntryNotFound,
    #[error("LZMA decompression failed")]
    Lzma(#[source] crate::IoFailure),
    #[error("reading the compressed payload failed")]
    DecompressionInput(#[source] crate::IoFailure),
    #[error("writing the decompressed payload failed")]
    DecompressionOutput(#[source] crate::IoFailure),
    #[error("cancelled")]
    Cancelled,
    /// Extraction finished without writing everything it should have: at
    /// least one entry failed, or nothing was extracted at all (including a
    /// GMA whose every entry the whitelist rejected). Never raised for a
    /// partial success that's otherwise fine — only for outcomes that must
    /// not be reported as `Finished`.
    #[error("extraction failed")]
    ExtractionFailed {
        extracted: usize,
        failed: usize,
        rejected: usize,
        first_error: Option<std::sync::Arc<str>>,
    },
    /// Every numbered fallback name (` (1)` through ` (255)`) at the
    /// destination was already taken.
    #[error("the extraction destination is unavailable")]
    DestinationUnavailable,
    #[error("the Garry's Mod directory is required for Addons extraction")]
    GmodPathMissing,
    /// A filesystem operation failed while packing, and the path it failed on
    /// is the reportable part.
    ///
    /// Carried on the error rather than emitted alongside it: reporting is
    /// derived from the error the caller receives, so a path cannot reach the
    /// user through one route while the error it belongs to travels another
    /// and arrives empty.
    #[error("packing failed on {}", path.display())]
    PathIo { path: PathBuf },
}
impl From<std::io::Error> for GmaError {
    fn from(error: std::io::Error) -> Self {
        Self::IoError(error.into())
    }
}
impl crate::error_key::HasErrorKey for GmaError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::IoError(_) => keys::IO_ERROR,
            Self::FormatError => keys::GMA_FORMAT_ERROR,
            Self::InvalidHeader => keys::GMA_INVALID_HEADER,
            Self::EntryNotFound => keys::GMA_ENTRY_NOT_FOUND,
            Self::Lzma(_) => keys::LZMA,
            Self::DecompressionInput(_) | Self::DecompressionOutput(_) => keys::IO_ERROR,
            Self::Cancelled => keys::CANCELLED,
            Self::ExtractionFailed { .. } => keys::GMA_EXTRACTION_FAILED,
            Self::DestinationUnavailable => keys::GMA_DESTINATION_UNAVAILABLE,
            Self::GmodPathMissing => keys::GMOD_PATH_MISSING,
            Self::PathIo { .. } => keys::PATH_IO_ERROR,
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::IoError(source)
            | Self::Lzma(source)
            | Self::DecompressionInput(source)
            | Self::DecompressionOutput(source) => Some(source.to_string()),
            Self::PathIo { path } => crate::transactions::detail_from_serialize(path),
            Self::ExtractionFailed {
                extracted,
                failed,
                rejected,
                first_error,
            } => {
                let mut detail =
                    format!("{extracted} extracted, {failed} failed, {rejected} rejected");
                if let Some(first_error) = first_error {
                    detail.push_str(": ");
                    detail.push_str(first_error);
                }
                Some(detail)
            }
            _ => None,
        }
    }
}

/// Deserialized in code, not by `#[serde(untagged)]`: with every `Standard`
/// field defaulted, untagged matched `Standard` for *any* JSON object, so a
/// legacy description that happened to be one silently lost its text and
/// `Legacy` was unreachable. The archive reader's embedded-field projection
/// owns this compatibility distinction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum GmaMetadata {
    Standard {
        #[serde(default)]
        title: String,
        #[serde(default)]
        #[serde(rename = "type")]
        addon_type: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        ignore: Vec<String>,
    },
    Legacy {
        title: String,
        description: String,
    },
}
/// The `addon.json` shape newer GMAs serialize into the description field.
#[derive(Deserialize)]
pub(crate) struct StandardManifest {
    #[serde(rename = "type")]
    pub(crate) addon_type: Option<String>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) ignore: Option<Vec<String>>,
}

impl StandardManifest {
    /// An object carrying none of these keys is free text that happens to be
    /// JSON, not a manifest.
    pub(crate) const fn is_manifest(&self) -> bool {
        self.addon_type.is_some() || self.tags.is_some() || self.ignore.is_some()
    }
}

impl GmaMetadata {
    /// Display title stored in either metadata representation.
    #[must_use]
    pub fn title(&self) -> &str {
        match &self {
            Self::Standard { title, .. } => title,
            Self::Legacy { title, .. } => title,
        }
        .as_str()
    }

    /// Standard-manifest addon type, absent for legacy metadata.
    #[must_use]
    pub fn addon_type(&self) -> Option<&str> {
        match &self {
            Self::Standard { addon_type, .. } => Some(addon_type.as_str()),
            Self::Legacy { .. } => None,
        }
    }

    /// Standard-manifest Workshop tags, absent for legacy metadata.
    #[must_use]
    pub fn tags(&self) -> Option<&[String]> {
        match &self {
            Self::Standard { tags, .. } => Some(tags),
            Self::Legacy { .. } => None,
        }
    }

    /// Standard-manifest ignore globs, absent for legacy metadata.
    #[must_use]
    pub fn ignore(&self) -> Option<&[String]> {
        match &self {
            Self::Standard { ignore, .. } => Some(ignore),
            Self::Legacy { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Parsed GMA header independent of its payload index.
pub struct GmaHeader {
    /// On-disk GMA format version.
    pub version: u8,
    /// Archive timestamp field.
    pub timestamp: u64,
    /// Parsed standard or legacy addon metadata.
    pub metadata: GmaMetadata,
    /// Author field stored by the archive.
    pub author: String,
    /// Addon revision stored by the archive.
    pub addon_version: i32,
}
impl GmaHeader {
    /// Display title projected from the parsed metadata.
    #[must_use]
    pub fn title(&self) -> &str {
        self.metadata.title()
    }
}

#[derive(Clone, Debug, Serialize)]
/// Indexed GMA payload entry.
pub struct GmaEntry {
    /// Normalized archive-relative path.
    pub path: String,
    /// Payload size in bytes.
    pub size: u64,
    /// CRC-32 of the payload.
    pub crc: u32,

    #[serde(skip)]
    /// Source entry ordinal.
    pub index: u64,
}

/// Whether a GMA entry path could escape an extraction root, or otherwise
/// name something the archive has no business naming.
///
/// Exported because the app crate validates the same paths at its own
/// boundary. It is deliberately the single definition: a second copy that
/// drifted would leave one of the two boundaries accepting traversals.
pub fn is_unsafe_entry_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if path.bytes().any(|b| b == 0 || b == b':' || b == b'\\') {
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

/// `PartialEq` is structural, and deliberately so: keying it on the discovery
/// list's order would make two different archives at one path compare equal
/// whenever their mtimes match.
#[derive(Clone, Eq, PartialEq)]
pub struct GmaFile {
    path: PathBuf,
    size: u64,

    id: Option<WorkshopId>,

    metadata: GmaMetadata,

    version: u8,

    extracted_name: String,

    modified: Option<u64>,
}
impl std::fmt::Debug for GmaFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmaFile")
            .field("path", &self.path)
            .field("size", &self.size)
            .field("id", &self.id)
            .field("metadata", &self.metadata)
            .field("version", &self.version)
            .field("extracted_name", &self.extracted_name)
            .field("modified", &self.modified)
            .finish()
    }
}
impl GmaFile {
    /// Creates an archive handle for content that will be packed to `path`.
    ///
    /// Identity derived from an existing archive belongs to the read path;
    /// newly packed content starts without a Workshop id or filesystem
    /// timestamp, and always writes the current GMA version.
    #[must_use]
    pub fn for_creation(path: impl Into<PathBuf>, metadata: GmaMetadata) -> Self {
        let mut file = Self {
            path: path.into(),
            size: 0,
            id: None,
            metadata,
            version: GMA_VERSION,
            extracted_name: String::new(),
            modified: None,
        };
        file.refresh_extracted_name();
        file
    }

    /// Opens and indexes an existing GMA archive.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, GmaError> {
        read::GmaView::open_file(path.as_ref())?.handle(path)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    #[must_use]
    pub const fn workshop_id(&self) -> Option<WorkshopId> {
        self.id
    }

    #[must_use]
    pub const fn metadata(&self) -> &GmaMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn version(&self) -> u8 {
        self.version
    }

    #[must_use]
    pub fn extracted_name(&self) -> &str {
        &self.extracted_name
    }

    #[must_use]
    pub const fn modified(&self) -> Option<u64> {
        self.modified
    }

    /// Assigns Workshop identity and refreshes every value derived from it.
    pub fn set_workshop_id(&mut self, id: WorkshopId) {
        self.id = Some(id);
        self.refresh_extracted_name();
    }

    #[cfg(test)]
    pub(crate) fn set_modified_for_test(&mut self, modified: Option<u64>) {
        self.modified = modified;
    }

    /// Takes the workshop id from the file name when the archive itself did not
    /// carry one, then derives the extraction name.
    ///
    /// Steam names a downloaded addon after its published file id, so a file on
    /// disk identifies itself even when its embedded metadata does not — but
    /// that is an identity change, not a naming one, which is why it is in this
    /// method's name rather than hidden inside the derive.
    pub(crate) fn adopt_path_id_and_name(&mut self) {
        self.id = self.id.or_else(|| id_from_path(&self.path));
        self.refresh_extracted_name();
    }

    fn refresh_extracted_name(&mut self) {
        self.extracted_name = self.derive_extracted_name();
    }

    fn derive_extracted_name(&self) -> String {
        let mut extracted_name = String::new();
        let mut underscored = false;

        {
            let name = self.metadata.title().to_lowercase();

            extracted_name.reserve(name.len());

            let mut first = true;
            for char in name.chars() {
                if char.is_alphanumeric() {
                    underscored = false;
                    extracted_name.push(char);
                } else if !underscored && !first {
                    underscored = true;
                    extracted_name.push('_');
                }
                first = false;
            }
        }

        if let Some(id) = self.id {
            let id_str = id.get().to_string();
            if !underscored {
                extracted_name.reserve(id_str.len() + 1);
                extracted_name.push('_');
                extracted_name.push_str(&id_str);
            } else {
                extracted_name.reserve(id_str.len());
                extracted_name.push_str(&id_str);
            }
        } else if underscored {
            extracted_name.pop();
        }

        if extracted_name.is_empty() {
            extracted_name = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or_else(
                    |_| "gmpublisher_extracted".into(),
                    |unix| format!("gmpublisher_extracted_{}", unix.as_secs()),
                );
        }

        extracted_name
    }
}

/// The workshop id a `.gma`'s own file name carries, if any.
fn id_from_path(path: &Path) -> Option<WorkshopId> {
    ws_id_from_file_name(path.file_stem()?.to_string_lossy())
}

pub(crate) mod whitelist;

pub(crate) mod extract;
pub(crate) use extract::{
    ExtractDestination, ExtractOptions, ExtractionContext, ExtractionOverwriteMode, Whitelist,
};

pub(crate) mod read;

pub(crate) mod write;

#[cfg(test)]
mod tests {
    /// `0.gma` is a legal filename, and the app turns every discovered id into
    /// a `NonZeroU64` — so a zero reaching the bridge panics library discovery
    /// rather than being ignored. Every parse path has to filter it, including
    /// the whole-name-is-a-number fast path.
    #[test]
    fn a_zero_workshop_id_is_no_id_on_every_parse_path() {
        assert_eq!(super::ws_id_from_file_name("0"), None);
        assert_eq!(super::ws_id_from_file_name("ds_0"), None);
        assert_eq!(super::ws_id_from_file_name("addon_0"), None);
        assert_eq!(super::ws_id_from_file_name("00"), None);
        assert_eq!(super::ws_id_from_file_name("123"), WorkshopId::new(123));
    }

    use super::*;

    #[test]
    fn ws_id_from_file_name_fixes_the_upstream_suffix_off_by_ten() {
        assert_eq!(ws_id_from_file_name("12345"), WorkshopId::new(12345));
        assert_eq!(ws_id_from_file_name("ds_12345"), WorkshopId::new(12345));
        // Upstream returns 123450 here (its call sites divide by 10 to
        // compensate); we parse the suffix correctly instead.
        assert_eq!(ws_id_from_file_name("addon_12345"), WorkshopId::new(12345));
        assert_eq!(extract_suffix_ws_id("addon_12345"), WorkshopId::new(12345));
        assert_eq!(ws_id_from_file_name("addon_without_digits"), None);
    }
}
