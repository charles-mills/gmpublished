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
fn nonzero_workshop_id(id: u64) -> Option<PublishedFileId> {
    (id != 0).then_some(PublishedFileId(id))
}

/// Recovers a workshop id from a GMA file's name: `ds_`-prefixed folder ids,
/// bare numeric names, or a trailing digit suffix on a descriptive name.
pub fn ws_id_from_file_name<S: AsRef<str>>(file_name: S) -> Option<PublishedFileId> {
    let file_name = file_name.as_ref();
    let file_name = file_name.strip_prefix("ds_").unwrap_or(file_name);

    if let Ok(id) = file_name.parse::<u64>() {
        return Some(PublishedFileId(id));
    }

    extract_suffix_ws_id(file_name)
}

// Deliberate divergence from upstream, which computes `(id + digit) * 10`
// per step and so returns the id multiplied by 10 for `name_123`-style
// suffixes (the pure-numeric fast path in ws_id_from_file_name hides this).
fn extract_suffix_ws_id<S: AsRef<str>>(file_name: S) -> Option<PublishedFileId> {
    let mut id = 0u64;
    for char in file_name
        .as_ref()
        .chars()
        .rev() // Reverse iterator so we're looking at the suffix (the PublishedFileId)
        .take_while(char::is_ascii_digit)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
    {
        id = 10_u64
            .checked_mul(id)?
            .checked_add(char::to_digit(char, 10).unwrap() as u64)?;
    }
    nonzero_workshop_id(id)
}

#[derive(Debug, Clone, Error)]
pub enum GMAError {
    #[error("ERR_IO_ERROR")]
    IOError(#[source] Option<std::sync::Arc<std::io::Error>>),
    #[error("ERR_GMA_FORMAT_ERROR")]
    FormatError,
    #[error("ERR_GMA_INVALID_HEADER")]
    InvalidHeader,
    #[error("ERR_GMA_ENTRY_NOT_FOUND")]
    EntryNotFound,
    #[error("ERR_LZMA")]
    LZMA,
    #[error("ERR_CANCELLED")]
    Cancelled,
    /// Extraction finished without writing everything it should have: at
    /// least one entry failed, or nothing was extracted at all (including a
    /// GMA whose every entry the whitelist rejected). Never raised for a
    /// partial success that's otherwise fine — only for outcomes that must
    /// not be reported as `Finished`.
    #[error("ERR_GMA_EXTRACTION_FAILED")]
    ExtractionFailed {
        extracted: usize,
        failed: usize,
        rejected: usize,
        first_error: Option<std::sync::Arc<str>>,
    },
    /// Every numbered fallback name (` (1)` through ` (255)`) at the
    /// destination was already taken.
    #[error("ERR_GMA_DESTINATION_UNAVAILABLE")]
    DestinationUnavailable,
}
impl From<std::io::Error> for GMAError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(Some(std::sync::Arc::new(error)))
    }
}
impl crate::error_key::HasErrorKey for GMAError {
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

/// Test-only ledger of GMA content reads (header opens and metadata parses),
/// so discovery-snapshot tests can assert that hydrated files are never read.
#[cfg(test)]
pub(crate) mod parse_observation {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static PARSED_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub fn record(path: &Path) {
        PARSED_PATHS
            .lock()
            .expect("parse observation lock")
            .push(path.to_path_buf());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GMAMetadata {
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
impl GMAMetadata {
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

#[derive(Debug, Clone)]
pub struct GMAHeader {
    pub version: u8,
    pub timestamp: u64,
    pub metadata: GMAMetadata,
    pub author: String,
    pub addon_version: i32,
}
impl GMAHeader {
    pub fn title(&self) -> &str {
        self.metadata.title()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GMAEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,

    #[serde(skip)]
    pub index: u64,
}

pub(crate) fn is_unsafe_entry_path(path: &str) -> bool {
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GMAFile {
    #[serde(serialize_with = "serde_canonicalize")]
    pub path: PathBuf,
    pub size: u64,

    pub id: Option<PublishedFileId>,

    #[serde(flatten)]
    pub metadata: GMAMetadata,

    #[serde(skip)]
    pub version: u8,

    pub extracted_name: String,

    #[serde(skip)]
    pub modified: Option<u64>,
}
impl std::fmt::Debug for GMAFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GMAFile")
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
impl GMAFile {
    /// Newest-modified first, then path, so `Ord`/`Eq` agree with each
    /// other and with the discovery list's presentation order.
    fn sort_key(&self) -> (std::cmp::Reverse<Option<u64>>, &Path) {
        (std::cmp::Reverse(self.modified), self.path.as_path())
    }
}
impl PartialEq for GMAFile {
    fn eq(&self, other: &Self) -> bool {
        self.sort_key() == other.sort_key()
    }
}
impl Eq for GMAFile {}
impl PartialOrd for GMAFile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for GMAFile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl GMAFile {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, GMAError> {
        read::GmaView::mmap(path.as_ref())?.handle(path)
    }

    pub fn set_ws_id(&mut self, id: PublishedFileId) {
        self.id = Some(id);
        self.compute_extracted_name();
    }

    pub(crate) fn compute_extracted_name(&mut self) {
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

        if self.id.is_none()
            && let Some(stem) = self.path.file_stem()
        {
            let stem = stem.to_string_lossy().to_lowercase();
            let found_id = ws_id_from_file_name(&stem);
            if found_id.is_some() {
                self.id = found_id;
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

        self.extracted_name = extracted_name;
    }
}

fn serde_canonicalize<S>(path: &PathBuf, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match dunce::canonicalize(path) {
        Ok(path) => path.serialize(serializer),
        Err(_) => path.serialize(serializer),
    }
}

pub mod whitelist;

pub mod extract;
pub use extract::{ExtractDestination, ExtractOptions, ExtractionOverwriteMode, Whitelist};

pub mod read;

pub mod write;

#[cfg(test)]
mod tests {
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
