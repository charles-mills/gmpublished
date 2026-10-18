use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

fn next_touch() -> u64 {
    static CLOCK: AtomicU64 = AtomicU64::new(0);
    CLOCK
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |clock| {
            clock.checked_add(1)
        })
        .expect("sampled-image cache epoch exhausted")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InactiveImageKind {
    Material,
    Lightmap,
}

pub(super) fn oldest_pool(first: Option<u64>, second: Option<u64>) -> Option<InactiveImageKind> {
    match (first, second) {
        (None, None) => None,
        (Some(_), None) => Some(InactiveImageKind::Material),
        (None, Some(_)) => Some(InactiveImageKind::Lightmap),
        (Some(first), Some(second)) if second < first => Some(InactiveImageKind::Lightmap),
        (Some(_), Some(_)) => Some(InactiveImageKind::Material),
    }
}

#[derive(Debug)]
struct Entry<A, S, I> {
    aliases: HashMap<A, Weak<S>>,
    image: Arc<I>,
    bytes: u64,
    touched: u64,
}

#[derive(Debug)]
pub(in crate::features::file_preview::viewer3d::pipeline) struct WarmImages<K, A, S, I> {
    entries: HashMap<K, Entry<A, S, I>>,
    recency: BTreeMap<u64, K>,
    logical_entries: usize,
    limit: usize,
    hits: u64,
    evictions: u64,
}

pub(in crate::features::file_preview::viewer3d::pipeline) struct Activated<A, S, I> {
    pub aliases: Vec<(A, Weak<S>)>,
    pub image: Arc<I>,
}

pub(in crate::features::file_preview::viewer3d::pipeline) struct Evicted<A, I> {
    pub aliases: Vec<A>,
    pub bytes: u64,
    pub image: Weak<I>,
}

impl<K: Eq + Hash + Clone, A: Eq + Hash + Clone, S, I> WarmImages<K, A, S, I> {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: BTreeMap::new(),
            logical_entries: 0,
            limit,
            hits: 0,
            evictions: 0,
        }
    }
    pub fn insert(
        &mut self,
        key: K,
        alias: A,
        source: Weak<S>,
        image: Arc<I>,
        bytes: u64,
    ) -> Vec<A> {
        let touched = next_touch();
        let entry = self.entries.entry(key.clone()).or_insert_with(|| Entry {
            aliases: HashMap::new(),
            image,
            bytes,
            touched,
        });
        self.recency.remove(&entry.touched);
        entry.touched = touched;
        self.recency.insert(touched, key);
        if entry.aliases.insert(alias, source).is_none() {
            self.logical_entries += 1;
        }
        let mut removed = Vec::new();
        while self.logical_entries > self.limit {
            removed.extend(self.evict_oldest().expect("warm image entries").aliases);
        }
        removed
    }
    pub fn take(&mut self, key: &K) -> Option<Activated<A, S, I>> {
        let entry = self.entries.remove(key)?;
        self.recency.remove(&entry.touched);
        self.hits = self.hits.saturating_add(1);
        self.logical_entries -= entry.aliases.len();
        Some(Activated {
            aliases: entry.aliases.into_iter().collect(),
            image: entry.image,
        })
    }
    pub fn activate(&mut self, key: &K) -> Vec<A> {
        self.take(key).map_or_else(Vec::new, |activated| {
            activated
                .aliases
                .into_iter()
                .map(|(alias, _)| alias)
                .collect()
        })
    }
    pub fn oldest_touch(&self) -> Option<u64> {
        self.recency.first_key_value().map(|(touch, _)| *touch)
    }
    pub fn evict_oldest(&mut self) -> Option<Evicted<A, I>> {
        let (_, oldest) = self.recency.pop_first()?;
        let entry = self.entries.remove(&oldest).expect("oldest warm image");
        self.logical_entries -= entry.aliases.len();
        self.evictions = self.evictions.saturating_add(1);
        Some(Evicted {
            image: Arc::downgrade(&entry.image),
            aliases: entry.aliases.into_keys().collect(),
            bytes: entry.bytes,
        })
    }
    pub fn retain_sources(&mut self, is_open: impl Fn(&S) -> bool) -> Vec<A> {
        let mut removed = Vec::new();
        let recency = &mut self.recency;
        self.entries.retain(|_, entry| {
            entry.aliases.retain(|alias, source| {
                if source.upgrade().is_none_or(|source| !is_open(&source)) {
                    removed.push(alias.clone());
                    false
                } else {
                    true
                }
            });
            if entry.aliases.is_empty() {
                recency.remove(&entry.touched);
                false
            } else {
                true
            }
        });
        self.logical_entries -= removed.len();
        removed
    }
    #[cfg(test)]
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({ "allocations": self.entries.len(),
            "estimated_bytes": self.entries.values().map(|entry| entry.bytes).sum::<u64>(),
            "logical_entries": self.logical_entries, "logical_limit": self.limit,
            "hits": self.hits, "evictions": self.evictions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_resource_classes_share_recency_and_exclude_active_leases() {
        for older in [InactiveImageKind::Lightmap, InactiveImageKind::Material] {
            let source = Arc::new(());
            let mut materials = WarmImages::new(8);
            let mut lightmaps = WarmImages::new(8);
            let material = Arc::new("material");
            let lightmap = Arc::new("lightmap");
            let material_weak = Arc::downgrade(&material);
            let lightmap_weak = Arc::downgrade(&lightmap);
            let active = Arc::new("active");
            let active_weak = Arc::downgrade(&active);
            materials.insert(99, "active", Arc::downgrade(&source), active, 64);
            let active_lease = materials.take(&99).unwrap();
            if older == InactiveImageKind::Lightmap {
                lightmaps.insert(0, "lightmap", Arc::downgrade(&source), lightmap, 64);
                lightmaps.insert(
                    1,
                    "extra-lightmap",
                    Arc::downgrade(&source),
                    Arc::new("extra-lightmap"),
                    64,
                );
                materials.insert(0, "material", Arc::downgrade(&source), material, 64);
            } else {
                materials.insert(0, "material", Arc::downgrade(&source), material, 64);
                materials.insert(
                    1,
                    "extra-material",
                    Arc::downgrade(&source),
                    Arc::new("extra-material"),
                    64,
                );
                lightmaps.insert(0, "lightmap", Arc::downgrade(&source), lightmap, 64);
            }
            let selected = oldest_pool(materials.oldest_touch(), lightmaps.oldest_touch()).unwrap();
            assert_eq!(selected, older);
            let evicted = match selected {
                InactiveImageKind::Material => materials.evict_oldest(),
                InactiveImageKind::Lightmap => lightmaps.evict_oldest(),
            }
            .unwrap();
            assert_eq!(evicted.bytes, 64);
            assert!(evicted.image.upgrade().is_none());
            assert_eq!(
                material_weak.strong_count() == 0,
                older == InactiveImageKind::Material
            );
            assert_eq!(
                lightmap_weak.strong_count() == 0,
                older == InactiveImageKind::Lightmap
            );
            assert!(active_weak.upgrade().is_some());
            drop(active_lease);
            assert!(active_weak.upgrade().is_none());
        }
    }

    #[test]
    fn alias_refresh_and_source_removal_keep_the_recency_index_consistent() {
        let source = Arc::new(());
        let image = Arc::new(1);
        let mut first = WarmImages::new(8);
        let mut second = WarmImages::new(8);
        first.insert(1, "first", Arc::downgrade(&source), Arc::clone(&image), 64);
        second.insert(2, "second", Arc::downgrade(&source), Arc::new(2), 64);
        first.insert(1, "alias", Arc::downgrade(&source), image, 64);
        assert_eq!(
            oldest_pool(first.oldest_touch(), second.oldest_touch()),
            Some(InactiveImageKind::Lightmap)
        );
        assert_eq!(first.stats()["allocations"], 1);
        assert_eq!(first.stats()["logical_entries"], 2);
        first.retain_sources(|_| false);
        assert!(first.oldest_touch().is_none());
        assert!(first.evict_oldest().is_none());
        assert_eq!(
            oldest_pool(first.oldest_touch(), second.oldest_touch()),
            Some(InactiveImageKind::Lightmap)
        );
    }

    #[test]
    fn aliases_share_physical_ownership_and_retire_together_in_lru_order() {
        let source = Arc::new(());
        let mut warm = WarmImages::new(4);
        let first = Arc::new(1);
        let first_weak = Arc::downgrade(&first);
        warm.insert(1, "a", Arc::downgrade(&source), Arc::clone(&first), 64);
        warm.insert(1, "alias", Arc::downgrade(&source), first, 64);
        let second = Arc::new(2);
        warm.insert(2, "b", Arc::downgrade(&source), Arc::clone(&second), 128);
        assert_eq!(warm.stats()["allocations"], 2);
        assert_eq!(warm.stats()["estimated_bytes"], 192);
        let evicted = warm.evict_oldest().unwrap();
        assert_eq!(evicted.bytes, 64);
        assert_eq!(evicted.aliases.len(), 2);
        assert!(evicted.aliases.contains(&"a"));
        assert!(evicted.aliases.contains(&"alias"));
        assert!(first_weak.upgrade().is_none());
        assert_eq!(Arc::strong_count(&second), 2);
        assert_eq!(warm.stats()["logical_entries"], 1);
    }

    #[test]
    fn activation_pins_before_warm_owner_retires_and_metadata_is_bounded() {
        let source = Arc::new(());
        let mut warm = WarmImages::new(2);
        let active = Arc::new(1);
        warm.insert(1, "a", Arc::downgrade(&source), Arc::clone(&active), 64);
        assert_eq!(warm.activate(&1), vec!["a"]);
        assert_eq!(Arc::strong_count(&active), 1);
        assert!(warm.evict_oldest().is_none());
        for (key, alias) in [(2, "b"), (3, "c"), (4, "d")] {
            warm.insert(key, alias, Arc::downgrade(&source), Arc::new(key), 64);
        }
        assert_eq!(warm.stats()["logical_entries"], 2);
        assert_eq!(warm.stats()["evictions"], 1);
        assert_eq!(warm.evict_oldest().unwrap().aliases, vec!["c"]);
    }

    #[test]
    fn weak_provider_does_not_retain_closed_source_or_image() {
        let source = Arc::new(());
        let source_weak = Arc::downgrade(&source);
        let image = Arc::new(1);
        let image_weak = Arc::downgrade(&image);
        let mut warm = WarmImages::new(2);
        warm.insert(1, "a", source_weak.clone(), image, 64);
        drop(source);
        assert!(source_weak.upgrade().is_none());
        assert_eq!(warm.retain_sources(|_| true), vec!["a"]);
        assert!(image_weak.upgrade().is_none());
        assert_eq!(warm.stats()["allocations"], 0);
    }
    #[test]
    fn closed_source_universe_releases_images_even_when_provider_is_still_alive() {
        let source = Arc::new(());
        let image = Arc::new(1);
        let image_weak = Arc::downgrade(&image);
        let mut warm = WarmImages::new(2);
        warm.insert(1, "a", Arc::downgrade(&source), image, 64);
        assert_eq!(warm.retain_sources(|_| false), vec!["a"]);
        assert_eq!(Arc::strong_count(&source), 1);
        assert!(image_weak.upgrade().is_none());
    }
}
