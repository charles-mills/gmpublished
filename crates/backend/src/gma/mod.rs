use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use steamworks::PublishedFileId;
use thiserror::Error;

const GMA_HEADER: &[u8; 4] = b"GMAD";

/// Zero is never a real Steam Workshop id; treat it as "no id" wherever a
/// digit-suffix or folder-name parse can produce it.
pub(crate) fn nonzero_workshop_id(id: u64) -> Option<PublishedFileId> {
    (id != 0).then_some(PublishedFileId(id))
}

/// Recovers a workshop id from a GMA file's name: `ds_`-prefixed folder ids,
/// bare numeric names, or a trailing digit suffix on a descriptive name.
pub fn ws_id_from_file_name<S: AsRef<str>>(file_name: S) -> Option<PublishedFileId> {
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
fn extract_suffix_ws_id<S: AsRef<str>>(file_name: S) -> Option<PublishedFileId> {
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

#[derive(Debug, Clone, Error)]
pub enum GmaError {
    #[error("GMA I/O failed")]
    IOError(#[source] Option<std::sync::Arc<std::io::Error>>),
    #[error("the GMA is malformed")]
    FormatError,
    #[error("the GMA header is not recognisable")]
    InvalidHeader,
    #[error("no such entry in the GMA")]
    EntryNotFound,
    #[error("LZMA decompression failed")]
    LZMA,
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
}
impl From<std::io::Error> for GmaError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(Some(std::sync::Arc::new(error)))
    }
}
impl crate::error_key::HasErrorKey for GmaError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        use crate::error_key::keys;
        match self {
            Self::IOError(_) => keys::IO_ERROR,
            Self::FormatError => keys::GMA_FORMAT_ERROR,
            Self::InvalidHeader => keys::GMA_INVALID_HEADER,
            Self::EntryNotFound => keys::GMA_ENTRY_NOT_FOUND,
            Self::LZMA => keys::LZMA,
            Self::Cancelled => keys::CANCELLED,
            Self::ExtractionFailed { .. } => keys::GMA_EXTRACTION_FAILED,
            Self::DestinationUnavailable => keys::GMA_DESTINATION_UNAVAILABLE,
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::IOError(Some(source)) => Some(source.to_string()),
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
/// `Legacy` was unreachable. See [`metadata_from_embedded_fields`].
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
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
    pub fn title(&self) -> &str {
        match &self {
            Self::Standard { title, .. } => title,
            Self::Legacy { title, .. } => title,
        }
        .as_str()
    }

    pub fn addon_type(&self) -> Option<&str> {
        match &self {
            Self::Standard { addon_type, .. } => Some(addon_type.as_str()),
            Self::Legacy { .. } => None,
        }
    }

    pub fn tags(&self) -> Option<&Vec<String>> {
        match &self {
            Self::Standard { tags, .. } => Some(tags),
            Self::Legacy { .. } => None,
        }
    }

    pub fn ignore(&self) -> Option<&Vec<String>> {
        match &self {
            Self::Standard { ignore, .. } => Some(ignore),
            Self::Legacy { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GmaHeader {
    pub version: u8,
    pub timestamp: u64,
    pub metadata: GmaMetadata,
    pub author: String,
    pub addon_version: i32,
}
impl GmaHeader {
    pub fn title(&self) -> &str {
        self.metadata.title()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GmaEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,

    #[serde(skip)]
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
    pub path: PathBuf,
    pub size: u64,

    pub id: Option<PublishedFileId>,

    pub metadata: GmaMetadata,

    pub version: u8,

    pub extracted_name: String,

    pub modified: Option<u64>,
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
    /// Carries [`read::GmaView::mmap`]'s accepted risk for the duration of the
    /// call.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, GmaError> {
        read::GmaView::mmap(path.as_ref())?.handle(path)
    }

    pub fn set_ws_id(&mut self, id: PublishedFileId) {
        self.id = Some(id);
        self.extracted_name = self.derive_extracted_name();
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
            let id_str = id.0.to_string();
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
fn id_from_path(path: &Path) -> Option<PublishedFileId> {
    ws_id_from_file_name(path.file_stem()?.to_string_lossy())
}

pub mod whitelist;

pub mod extract;
pub use extract::{ExtractDestination, ExtractOptions, ExtractionOverwriteMode, Whitelist};

pub mod read;

pub mod write;

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
        assert_eq!(
            super::ws_id_from_file_name("123"),
            Some(steamworks::PublishedFileId(123))
        );
    }

    use super::*;

    #[test]
    fn ws_id_from_file_name_fixes_the_upstream_suffix_off_by_ten() {
        assert_eq!(ws_id_from_file_name("12345"), Some(PublishedFileId(12345)));
        assert_eq!(
            ws_id_from_file_name("ds_12345"),
            Some(PublishedFileId(12345))
        );
        // Upstream returns 123450 here (its call sites divide by 10 to
        // compensate); we parse the suffix correctly instead.
        assert_eq!(
            ws_id_from_file_name("addon_12345"),
            Some(PublishedFileId(12345))
        );
        assert_eq!(
            extract_suffix_ws_id("addon_12345"),
            Some(PublishedFileId(12345))
        );
        assert_eq!(ws_id_from_file_name("addon_without_digits"), None);
    }
}
