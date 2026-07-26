#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

pub const THUMBNAIL_CACHE_FILE_EXTENSION: &str = "rgba";
/// Extension for the source tier: the bytes as fetched, before any decode.
pub const THUMBNAIL_SOURCE_FILE_EXTENSION: &str = "src";

const CACHE_HASH_VERSION: &[u8] = b"gmpublished-thumbnail-v1";
const FNV1A64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ThumbnailMode {
    #[default]
    Animated,
    Static,
}

/// Cache key identifying a thumbnail source and requested output size.
///
/// The hash is computed once at construction and stored, because this key is
/// hashed far more often than it is built: the scheduler's candidate scan
/// probes `active_jobs` once per entry per pump, and hashing a ~180 byte Steam
/// CDN URL each time made that scan dominated by string traversal. Measured at
/// 74% of the scan's cost.
#[derive(Clone, Debug, Eq)]
pub struct ThumbnailKey {
    /// Source identity component for the thumbnail.
    pub source: ThumbnailSourceKey,
    /// Requested maximum output width or height.
    pub max_edge: u32,
    pub mode: ThumbnailMode,
    /// Precomputed [`stable_cache_hash`]. Derived purely from the other
    /// fields, so it is an optimisation rather than part of the identity.
    hash: u64,
}

impl std::hash::Hash for ThumbnailKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // O(1) regardless of URL length. Sound because `hash` is a pure
        // function of the fields `PartialEq` compares.
        state.write_u64(self.hash);
    }
}

impl PartialEq for ThumbnailKey {
    fn eq(&self, other: &Self) -> bool {
        // The hash rejects non-equal keys without touching the URL; the field
        // comparison then keeps equality exact under collision.
        self.hash == other.hash
            && self.max_edge == other.max_edge
            && self.mode == other.mode
            && self.source == other.source
    }
}

impl ThumbnailKey {
    #[cfg(test)]
    #[must_use]
    pub fn for_bytes(id: impl Into<String>, max_edge: u32) -> Self {
        Self::build(
            ThumbnailSourceKey::Bytes { id: id.into() },
            max_edge,
            ThumbnailMode::Animated,
        )
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_file(path: impl Into<PathBuf>, max_edge: u32) -> Self {
        Self::build(
            ThumbnailSourceKey::File { path: path.into() },
            max_edge,
            ThumbnailMode::Animated,
        )
    }

    #[must_use]
    pub fn for_url(url: impl Into<String>, max_edge: u32) -> Self {
        Self::for_url_with_mode(url, max_edge, ThumbnailMode::Animated)
    }

    #[must_use]
    pub fn for_url_with_mode(url: impl Into<String>, max_edge: u32, mode: ThumbnailMode) -> Self {
        Self::build(
            ThumbnailSourceKey::Url {
                url: Arc::from(normalize_url(url).as_str()),
            },
            max_edge,
            mode,
        )
    }

    /// The one place the cached hash is produced, so it cannot drift out of
    /// step with the fields it summarises.
    fn build(source: ThumbnailSourceKey, max_edge: u32, mode: ThumbnailMode) -> Self {
        let mut key = Self {
            source,
            max_edge,
            mode,
            hash: 0,
        };
        key.hash = stable_cache_hash(&key);
        key
    }

    #[must_use]
    pub(crate) fn with_max_edge_and_mode(&self, max_edge: u32, mode: ThumbnailMode) -> Self {
        // Cloning the source is an `Arc` bump for URLs, so this no longer
        // copies the string.
        Self::build(self.source.clone(), max_edge, mode)
    }

    #[must_use]
    pub const fn mode(&self) -> ThumbnailMode {
        self.mode
    }

    #[cfg(test)]
    #[must_use]
    pub fn max_edge(&self) -> u32 {
        self.max_edge
    }

    /// Returns the source URL when this key identifies a non-empty URL
    /// thumbnail. Used to map a delivery back to its Workshop preview URL for
    /// ThumbHash recording.
    #[must_use]
    pub fn source_url(&self) -> Option<&str> {
        match &self.source {
            ThumbnailSourceKey::Url { url } => (!url.is_empty()).then(|| &**url),
            #[cfg(test)]
            ThumbnailSourceKey::Bytes { .. } | ThumbnailSourceKey::File { .. } => None,
        }
    }

    /// Returns a deterministic on-disk cache filename for this key.
    #[must_use]
    pub fn disk_file_name(&self) -> String {
        format!(
            "{:016x}-{}.{}",
            self.hash, self.max_edge, THUMBNAIL_CACHE_FILE_EXTENSION
        )
    }
}

/// Source identity component of a [`ThumbnailKey`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ThumbnailSourceKey {
    /// Caller-stable identity for in-memory bytes.
    #[cfg(test)]
    Bytes { id: String },
    /// Local image file path.
    #[cfg(test)]
    File { path: PathBuf },
    /// HTTP(S) source image URL.
    ///
    /// `Arc<str>` rather than `String`: keys are cloned constantly (into
    /// deliveries, index entries, and interest keys) and never mutated, so a
    /// clone should be a refcount bump rather than a copy of the URL.
    Url { url: Arc<str> },
}

/// On-disk file name for a URL's fetched bytes.
///
/// Keyed by the URL alone — deliberately not by size or mode. The derived tier
/// is size-specific, so moving a window between a 1x and a 2x display misses
/// every entry and re-fetches the library; with the source kept, the same move
/// re-derives from local bytes instead.
///
/// Sources are also smaller than the decoded output they produce, which is what
/// lets a large library fit the disk budget — but by a margin that depends
/// entirely on what was banked. Measured: a warm-banked 512px CDN variant is
/// 32 KB against a 1024 KB derived entry (**31x**, so 20k addons is 0.63 GB and
/// fits the 2 GB clamp comfortably), while an interactive fetch banks the bare
/// original, which at 1024² is 127 KB (8x, 2.44 GB at 20k) and at 1920x1080 is
/// 246 KB (2.3x, 4.69 GB). A warm-dominated tier fits easily; one filled by
/// heavy scrolling on a huge library does not, and relies on eviction — which
/// is ordered by use, not by bank date, precisely so the pressure falls on
/// sources nobody opens.
#[must_use]
pub fn source_file_name(url: &str) -> String {
    let mut hash = FNV1A64_OFFSET;
    write_hash_bytes(&mut hash, CACHE_HASH_VERSION);
    write_hash_byte(&mut hash, 2);
    write_len_prefixed(&mut hash, url.trim().as_bytes());
    format!("{hash:016x}.{THUMBNAIL_SOURCE_FILE_EXTENSION}")
}

/// Normalizes a URL string for thumbnail cache-key identity.
///
/// Keep this string-based and only trim outer whitespace. URL parsing remains
/// part of the fetch boundary, not cache identity.
#[must_use]
pub fn normalize_url(url: impl Into<String>) -> String {
    url.into().trim().to_owned()
}

fn stable_cache_hash(key: &ThumbnailKey) -> u64 {
    let mut hash = FNV1A64_OFFSET;
    write_hash_bytes(&mut hash, CACHE_HASH_VERSION);

    match &key.source {
        #[cfg(test)]
        ThumbnailSourceKey::Bytes { id } => {
            write_hash_byte(&mut hash, 0);
            write_len_prefixed(&mut hash, id.as_bytes());
        }
        #[cfg(test)]
        ThumbnailSourceKey::File { path } => {
            write_hash_byte(&mut hash, 1);
            write_len_prefixed(&mut hash, path.to_string_lossy().as_bytes());
        }
        ThumbnailSourceKey::Url { url } => {
            write_hash_byte(&mut hash, 2);
            write_len_prefixed(&mut hash, url.as_bytes());
        }
    }

    write_hash_bytes(&mut hash, &key.max_edge.to_le_bytes());
    if key.mode == ThumbnailMode::Static {
        write_hash_byte(&mut hash, 3);
    }
    hash
}

fn write_len_prefixed(hash: &mut u64, bytes: &[u8]) {
    write_hash_bytes(hash, &(bytes.len() as u64).to_le_bytes());
    write_hash_bytes(hash, bytes);
}

fn write_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        write_hash_byte(hash, *byte);
    }
}

fn write_hash_byte(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(FNV1A64_PRIME);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_cache_file_names_are_stable_for_existing_sources() {
        let bytes_key = ThumbnailKey::for_bytes("avatar:76561198000000000", 96);
        let file_key = ThumbnailKey::for_file("/tmp/source.png", 128);
        let url_key = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 128);

        assert_eq!(bytes_key.disk_file_name(), "29d36f33527fe33e-96.rgba");
        assert_eq!(file_key.disk_file_name(), "c9249628c039637d-128.rgba");
        assert_eq!(url_key.disk_file_name(), "a5d4ac9462ab4731-128.rgba");
    }

    #[test]
    fn source_kind_is_part_of_key_identity() {
        let id = "https://example.invalid/preview.jpg";

        assert_ne!(
            ThumbnailKey::for_bytes(id, 128),
            ThumbnailKey::for_file(id, 128)
        );
        assert_ne!(
            ThumbnailKey::for_bytes(id, 128),
            ThumbnailKey::for_url(id, 128)
        );
        assert_ne!(
            ThumbnailKey::for_file(id, 128),
            ThumbnailKey::for_url(id, 128)
        );
    }

    #[test]
    fn max_edge_is_part_of_key_identity() {
        let small = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 128);
        let large = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 256);

        assert_eq!(small.max_edge(), 128);
        assert_eq!(large.max_edge(), 256);
        assert_ne!(small, large);
        assert_eq!(large.disk_file_name(), "9fd52d6942e50006-256.rgba");
    }

    #[test]
    fn static_mode_is_distinct_without_changing_existing_animated_names() {
        let animated = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 128);
        let static_key = ThumbnailKey::for_url_with_mode(
            "https://example.invalid/preview.jpg",
            128,
            ThumbnailMode::Static,
        );

        assert_eq!(animated.disk_file_name(), "a5d4ac9462ab4731-128.rgba");
        assert_ne!(animated, static_key);
        assert_ne!(animated.disk_file_name(), static_key.disk_file_name());
    }

    #[test]
    fn url_keys_trim_outer_whitespace_only() {
        let trimmed = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 128);
        let padded = ThumbnailKey::for_url(" https://example.invalid/preview.jpg \n", 128);
        let interior = normalize_url(" https://example.invalid/a path/preview.jpg ");

        assert_eq!(trimmed, padded);
        assert_eq!(interior, "https://example.invalid/a path/preview.jpg");
    }
}
