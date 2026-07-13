#[cfg(test)]
use std::path::PathBuf;

pub const THUMBNAIL_CACHE_FILE_EXTENSION: &str = "rgba";

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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ThumbnailKey {
    /// Source identity component for the thumbnail.
    pub source: ThumbnailSourceKey,
    /// Requested maximum output width or height.
    pub max_edge: u32,
    pub mode: ThumbnailMode,
}

impl ThumbnailKey {
    #[cfg(test)]
    #[must_use]
    pub fn for_bytes(id: impl Into<String>, max_edge: u32) -> Self {
        Self {
            source: ThumbnailSourceKey::Bytes { id: id.into() },
            max_edge,
            mode: ThumbnailMode::Animated,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_file(path: impl Into<PathBuf>, max_edge: u32) -> Self {
        Self {
            source: ThumbnailSourceKey::File { path: path.into() },
            max_edge,
            mode: ThumbnailMode::Animated,
        }
    }

    #[must_use]
    pub fn for_url(url: impl Into<String>, max_edge: u32) -> Self {
        Self::for_url_with_mode(url, max_edge, ThumbnailMode::Animated)
    }

    #[must_use]
    pub fn for_url_with_mode(url: impl Into<String>, max_edge: u32, mode: ThumbnailMode) -> Self {
        Self {
            source: ThumbnailSourceKey::Url {
                url: normalize_url(url),
            },
            max_edge,
            mode,
        }
    }

    #[must_use]
    pub(crate) fn with_max_edge_and_mode(&self, max_edge: u32, mode: ThumbnailMode) -> Self {
        Self {
            source: self.source.clone(),
            max_edge,
            mode,
        }
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
            ThumbnailSourceKey::Url { url } => (!url.is_empty()).then_some(url.as_str()),
            #[cfg(test)]
            ThumbnailSourceKey::Bytes { .. } | ThumbnailSourceKey::File { .. } => None,
        }
    }

    /// Returns a deterministic on-disk cache filename for this key.
    #[must_use]
    pub fn disk_file_name(&self) -> String {
        format!(
            "{:016x}-{}.{}",
            stable_cache_hash(self),
            self.max_edge,
            THUMBNAIL_CACHE_FILE_EXTENSION
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
    Url { url: String },
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
