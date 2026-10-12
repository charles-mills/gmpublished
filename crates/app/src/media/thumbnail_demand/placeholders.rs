use std::{collections::HashMap, fmt, sync::Arc};

use iced::widget::image;
use quick_cache::unsync::Cache;

use crate::media::thumbnail_worker::normalized_url;

const PLACEHOLDER_CACHE_ITEMS: usize = 512;

type PlaceholderCache = Cache<Arc<str>, PlaceholderImage>;

pub(super) struct PlaceholderStore {
    /// Keep all hashes; the snapshot is only read once, and each is about 25 bytes.
    thumbhashes: HashMap<Arc<str>, Arc<[u8]>>,
    decoded: PlaceholderCache,
}

impl Default for PlaceholderStore {
    fn default() -> Self {
        Self {
            thumbhashes: HashMap::new(),
            decoded: PlaceholderCache::new(PLACEHOLDER_CACHE_ITEMS),
        }
    }
}

impl PlaceholderStore {
    /// Records a URL's ThumbHash under its normalized key. Seeded URLs come
    /// straight off Steam metadata and are not normalized yet; completions
    /// arrive through a `ThumbnailKey`, which already did it.
    pub(super) fn remember(&mut self, url: &str, hash: Arc<[u8]>) {
        let url = normalized_url(url);
        if !self.thumbhashes.contains_key(url) {
            self.thumbhashes.insert(Arc::from(url), hash);
        }
    }

    pub(super) fn get(&mut self, url: &str) -> Option<PlaceholderImage> {
        let url = normalized_url(url);
        if let Some(placeholder) = self.decoded.get(url) {
            return Some(placeholder.clone());
        }
        let (key, hash) = self.thumbhashes.get_key_value(url)?;
        let (key, hash) = (Arc::clone(key), Arc::clone(hash));
        let placeholder = decode_placeholder(&hash)?;
        self.decoded.insert(key, placeholder.clone());
        Some(placeholder)
    }
}

#[derive(Clone)]
pub struct PlaceholderImage {
    handle: image::Handle,
    width: u32,
    height: u32,
}

impl PlaceholderImage {
    pub fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    #[cfg(test)]
    pub fn for_test(width: u32, height: u32) -> Self {
        Self {
            handle: image::Handle::from_rgba(
                width,
                height,
                vec![32; (width * height * 4) as usize],
            ),
            width,
            height,
        }
    }
}

impl fmt::Debug for PlaceholderImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaceholderImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

fn decode_placeholder(hash: &[u8]) -> Option<PlaceholderImage> {
    let (width, height, rgba) = crate::media::thumbhash::decode(hash)?;
    Some(PlaceholderImage {
        handle: image::Handle::from_rgba(width, height, rgba),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_capped_while_the_hashes_they_derive_from_are_kept() {
        let mut store = PlaceholderStore::default();
        let hash: Arc<[u8]> = Arc::from(
            crate::media::thumbhash::encode(4, 4, &[128; 4 * 4 * 4]).expect("hash encodes"),
        );
        let url = |index: usize| format!("https://example.invalid/{index}.jpg");
        let seeded = PLACEHOLDER_CACHE_ITEMS * 2;

        for index in 0..seeded {
            store.remember(&url(index), Arc::clone(&hash));
        }
        for index in 0..seeded {
            assert!(
                store.get(&url(index)).is_some(),
                "every seeded URL should paint a placeholder"
            );
        }

        assert_eq!(store.thumbhashes.len(), seeded);
        assert!(
            store.decoded.len() <= PLACEHOLDER_CACHE_ITEMS,
            "placeholder cache held {} entries",
            store.decoded.len()
        );
        assert!(store.get(&url(0)).is_some());
    }

    #[test]
    fn a_seeded_url_is_found_however_it_was_padded() {
        let mut store = PlaceholderStore::default();
        let hash = crate::media::thumbhash::encode(4, 4, &[128; 4 * 4 * 4]).expect("hash encodes");
        store.remember("  https://example.invalid/poster.jpg\n", Arc::from(hash));

        assert!(store.get("https://example.invalid/poster.jpg").is_some());
        assert_eq!(store.thumbhashes.len(), 1);
    }
}
