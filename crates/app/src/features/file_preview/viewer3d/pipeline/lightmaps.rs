use super::texture_upload::{Input, SharedUploader, UploadedView};
use super::{PreviewScene, SceneKey, wgpu};
use crate::bridge::materials::TextureStorage;
use gmpublished_domain::scene::map::{LightmapPageId, LightmapSet};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("lightmap animation time or style value is not finite")]
    InvalidTime,
    #[error("lightmap page {0:?} is absent from the prepared scene")]
    MissingPage(LightmapPageId),
    #[error("lightmap residency size overflows")]
    Overflow,
    #[error(transparent)]
    Render(#[from] gmpublished_domain::scene::map::LightmapPageError),
    #[error(transparent)]
    Upload(#[from] super::texture_upload::UploadError),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Key {
    scene: SceneKey,
    page: LightmapPageId,
    styles: Vec<(u8, u32)>,
}

#[derive(Debug)]
pub(super) struct Request {
    source: PreviewScene,
    weights: [f32; 256],
    keys: Vec<Key>,
}

impl Request {
    pub fn new(
        source: PreviewScene,
        pages: impl IntoIterator<Item = LightmapPageId>,
        time: f32,
    ) -> Result<Self, Error> {
        if !time.is_finite() {
            return Err(Error::InvalidTime);
        }
        let lightmaps = source.map().and_then(|map| map.lightmap.as_ref());
        let weights = lightmaps.map_or([1.0; 256], |set| set.style_values(time));
        let pages: BTreeSet<_> = pages.into_iter().collect();
        let mut keys = Vec::with_capacity(pages.len());
        for page in pages {
            let metadata = lightmaps
                .and_then(|set| set.page(page))
                .ok_or(Error::MissingPage(page))?;
            let mut styles = Vec::with_capacity(metadata.active_styles().len());
            for &style in metadata.active_styles() {
                let value = weights[usize::from(style)];
                if !value.is_finite() {
                    return Err(Error::InvalidTime);
                }
                styles.push((style, value.to_bits()));
            }
            keys.push(Key {
                scene: source.key(),
                page,
                styles,
            });
        }
        Ok(Self {
            source,
            weights,
            keys,
        })
    }

    pub fn working_set(&self) -> Result<super::bindings::WorkingSet, Error> {
        let mut cost = super::bindings::WorkingSet::new();
        for key in &self.keys {
            let page = self
                .lightmaps()
                .and_then(|set| set.page(key.page))
                .ok_or(Error::MissingPage(key.page))?;
            let bytes = u64::from(page.width)
                .checked_mul(u64::from(page.height))
                .and_then(|bytes| bytes.checked_mul(4))
                .ok_or(Error::Overflow)?;
            cost.insert(
                super::bindings::SampledAllocation::Lightmap(key.clone()),
                bytes,
                true,
            );
        }
        Ok(cost)
    }

    fn lightmaps(&self) -> Option<&LightmapSet> {
        self.source.map().and_then(|map| map.lightmap.as_ref())
    }
}

#[derive(Debug)]
pub(super) struct Page {
    pub binding: wgpu::BindGroup,
    _srgb: UploadedView,
    _raw: UploadedView,
    bytes: u64,
    sampler: wgpu::Sampler,
}

impl Page {
    fn upload(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uploader: &SharedUploader,
        sampler: &wgpu::Sampler,
        dimensions: [u32; 2],
        bytes: &[u8],
        sampled: bool,
    ) -> Result<Self, Error> {
        let input = Input {
            storage: TextureStorage::Rgba8,
            dimensions,
            bytes,
        };
        let mut uploader = uploader.lock().expect("texture uploader lock");
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let plan = uploader.plan(input, format)?;
        let descriptor = wgpu::TextureDescriptor {
            label: Some("file_preview.lightmap.page"),
            size: wgpu::Extent3d {
                width: dimensions[0],
                height: dimensions[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        };
        let texture = if sampled {
            uploader.allocate_sampled(&descriptor)?
        } else {
            uploader.allocate(&descriptor)?
        };
        uploader.write_planned(&texture, 0, wgpu::Origin3d::ZERO, input, plan)?;
        drop(uploader);
        let srgb = texture.create_view(&Default::default());
        let raw = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.lightmap.page"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&srgb),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Ok(Self {
            binding,
            _srgb: srgb,
            _raw: raw,
            bytes: u64::from(dimensions[0]) * u64::from(dimensions[1]) * 4,
            sampler: sampler.clone(),
        })
    }

    pub fn white(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uploader: &SharedUploader,
        sampler: &wgpu::Sampler,
    ) -> Result<Arc<Self>, Error> {
        Self::upload(device, layout, uploader, sampler, [1, 1], &[255; 4], false).map(Arc::new)
    }
}

#[derive(Debug)]
pub(super) struct Snapshot {
    white: Arc<Page>,
    pages: HashMap<LightmapPageId, (Key, Arc<Page>)>,
}

impl Snapshot {
    pub fn empty(white: Arc<Page>) -> Arc<Self> {
        Arc::new(Self {
            white,
            pages: HashMap::new(),
        })
    }

    pub fn binding(&self, page: Option<LightmapPageId>) -> &wgpu::BindGroup {
        page.map_or_else(
            || &self.white.binding,
            |page| {
                &self
                    .pages
                    .get(&page)
                    .expect("prepared lightmap page is resident")
                    .1
                    .binding
            },
        )
    }

    pub fn matches(&self, request: &Request) -> bool {
        self.pages.len() == request.keys.len()
            && request.keys.iter().all(|key| {
                self.pages
                    .get(&key.page)
                    .is_some_and(|(prepared, _)| prepared == key)
            })
    }

    #[cfg(feature = "map-capture")]
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({
            "pages": self.pages.len(),
            "estimated_bytes": self.pages.values().map(|(_, page)| page.bytes).sum::<u64>(),
            "virtual_pages": self.pages.keys().map(|page| page.0).collect::<BTreeSet<_>>(),
        })
    }
}

#[derive(Debug)]
struct Entry {
    source: Weak<super::MapPreview>,
    page: Arc<Page>,
}

#[derive(Debug)]
pub(super) struct WarmPages {
    images: super::bindings::warm::WarmImages<Key, Key, super::MapPreview, Page>,
}

impl Default for WarmPages {
    fn default() -> Self {
        Self {
            images: super::bindings::warm::WarmImages::new(4096),
        }
    }
}

impl WarmPages {
    fn insert(&mut self, key: Key, entry: Entry) {
        let bytes = entry.page.bytes;
        self.images
            .insert(key.clone(), key, entry.source, entry.page, bytes);
    }

    fn activate(&mut self, key: &Key) -> Option<Entry> {
        let activated = self.images.take(key)?;
        let (_, source) = activated
            .aliases
            .into_iter()
            .next()
            .expect("lightmap source alias");
        Some(Entry {
            source,
            page: activated.image,
        })
    }

    fn retain_sources(&mut self, open_scenes: &HashSet<SceneKey>) {
        self.images.retain_sources(|map| {
            open_scenes.contains(&SceneKey::Map(map as *const super::MapPreview as usize))
        });
    }

    pub fn oldest_touch(&self) -> Option<u64> {
        self.images.oldest_touch()
    }

    pub fn evict_oldest(&mut self) -> Option<(u64, Weak<Page>)> {
        self.images
            .evict_oldest()
            .map(|evicted| (evicted.bytes, evicted.image))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct MissingCost {
    pub bytes: u64,
    pub allocations: u64,
}

#[derive(Debug)]
pub(super) struct Pool {
    uploader: SharedUploader,
    white: Arc<Page>,
    pages: HashMap<Key, Entry>,
    active_cost: MissingCost,
    warm: Arc<Mutex<WarmPages>>,
    #[cfg(feature = "map-capture")]
    uploads: u64,
    #[cfg(feature = "map-capture")]
    cache_hits: u64,
}

impl Pool {
    pub fn new(uploader: &SharedUploader, white: Arc<Page>) -> Self {
        Self {
            uploader: Arc::clone(uploader),
            white,
            pages: HashMap::new(),
            active_cost: MissingCost::default(),
            warm: Arc::new(Mutex::new(WarmPages::default())),
            #[cfg(feature = "map-capture")]
            uploads: 0,
            #[cfg(feature = "map-capture")]
            cache_hits: 0,
        }
    }

    pub fn empty_snapshot(&self) -> Arc<Snapshot> {
        Snapshot::empty(Arc::clone(&self.white))
    }

    pub fn demand(&mut self, requests: &[&Request]) -> Result<MissingCost, Error> {
        let active: HashSet<_> = requests
            .iter()
            .flat_map(|request| &request.keys)
            .cloned()
            .collect();
        let open_scenes: HashSet<_> = requests
            .iter()
            .map(|request| request.source.key())
            .collect();
        let mut warm = self.warm.lock().expect("lightmap cache lock");
        warm.retain_sources(&open_scenes);
        for key in &active {
            if !self.pages.contains_key(key)
                && let Some(entry) = warm.activate(key)
            {
                self.pages.insert(key.clone(), entry);
            }
        }
        self.pages.retain(|key, entry| {
            if active.contains(key) && entry.source.strong_count() != 0 {
                return true;
            }
            if open_scenes.contains(&key.scene) && entry.source.strong_count() != 0 {
                warm.insert(
                    key.clone(),
                    Entry {
                        source: entry.source.clone(),
                        page: Arc::clone(&entry.page),
                    },
                );
            }
            false
        });
        drop(warm);
        let mut missing = HashSet::new();
        let mut total = HashSet::new();
        let mut active_cost = MissingCost::default();
        let mut cost = MissingCost::default();
        for request in requests {
            for key in &request.keys {
                let page = request
                    .lightmaps()
                    .and_then(|set| set.page(key.page))
                    .ok_or(Error::MissingPage(key.page))?;
                let bytes = u64::from(page.width)
                    .checked_mul(u64::from(page.height))
                    .and_then(|bytes| bytes.checked_mul(4))
                    .ok_or(Error::Overflow)?;
                if total.insert(key.clone()) {
                    active_cost.bytes = active_cost
                        .bytes
                        .checked_add(bytes)
                        .ok_or(Error::Overflow)?;
                    active_cost.allocations = active_cost
                        .allocations
                        .checked_add(1)
                        .ok_or(Error::Overflow)?;
                }
                if self.pages.contains_key(key) || !missing.insert(key.clone()) {
                    continue;
                }
                cost.bytes = cost.bytes.checked_add(bytes).ok_or(Error::Overflow)?;
                cost.allocations = cost.allocations.checked_add(1).ok_or(Error::Overflow)?;
            }
        }
        self.active_cost = active_cost;
        Ok(cost)
    }

    pub fn warm_cache(&self) -> Arc<Mutex<WarmPages>> {
        Arc::clone(&self.warm)
    }

    pub fn active_cost(&self) -> MissingCost {
        self.active_cost
    }

    pub fn cached(&self, request: &Request) -> Option<Arc<Snapshot>> {
        let pages = request
            .keys
            .iter()
            .map(|key| {
                let page = Arc::clone(&self.pages.get(key)?.page);
                Some((key.page, (key.clone(), page)))
            })
            .collect::<Option<HashMap<_, _>>>()?;
        Some(Arc::new(Snapshot {
            white: Arc::clone(&self.white),
            pages,
        }))
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        request: &Request,
    ) -> Result<Arc<Snapshot>, Error> {
        let mut pages = HashMap::with_capacity(request.keys.len());
        for key in &request.keys {
            let page = if let Some(entry) = self.pages.get(key) {
                #[cfg(feature = "map-capture")]
                {
                    self.cache_hits = self.cache_hits.saturating_add(1);
                }
                Arc::clone(&entry.page)
            } else {
                let set = request.lightmaps().ok_or(Error::MissingPage(key.page))?;
                let metadata = set.page(key.page).ok_or(Error::MissingPage(key.page))?;
                let bytes = set.render_page(key.page, &request.weights)?;
                let page = Arc::new(Page::upload(
                    device,
                    layout,
                    &self.uploader,
                    &self.white.sampler,
                    [metadata.width, metadata.height],
                    &bytes,
                    true,
                )?);
                let PreviewScene::Map(map) = &request.source else {
                    return Err(Error::MissingPage(key.page));
                };
                self.pages.insert(
                    key.clone(),
                    Entry {
                        source: Arc::downgrade(map),
                        page: Arc::clone(&page),
                    },
                );
                #[cfg(feature = "map-capture")]
                {
                    self.uploads = self.uploads.saturating_add(1);
                }
                page
            };
            pages.insert(key.page, (key.clone(), page));
        }
        self.uploader.lock().expect("texture uploader lock").flush();
        Ok(Arc::new(Snapshot {
            white: Arc::clone(&self.white),
            pages,
        }))
    }

    #[cfg(feature = "map-capture")]
    pub fn stats(&self) -> serde_json::Value {
        let warm = self.warm.lock().expect("lightmap cache lock");
        let warm_stats = warm.images.stats();
        drop(warm);
        serde_json::json!({
            "resident_pages": self.pages.len(),
            "estimated_bytes": self.pages.values().map(|entry| entry.page.bytes).sum::<u64>(),
            "uploads": self.uploads, "cache_hits": self.cache_hits,
            "warm_pages": warm_stats["allocations"], "warm_bytes": warm_stats["estimated_bytes"],
            "warm_hits": warm_stats["hits"], "warm_evictions": warm_stats["evictions"],
            "scope": "demand union across live viewers and render passes; immutable style versions",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::math::Vec3;
    use gmpublished_domain::scene::map::LightmapSource;

    fn scene() -> PreviewScene {
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        map.lightmap = Some(LightmapSet::style_pages_for_tests(
            LightmapSource::Ldr,
            &[
                vec![(0, [32, 64, 96, 0])],
                vec![(0, [32, 64, 96, 0]), (32, [16, 8, 4, 0])],
            ],
            &[(32, "am")],
        ));
        PreviewScene::Map(Arc::new(map))
    }

    #[test]
    fn requested_page_versions_depend_on_their_own_styles_and_scene() {
        let source = scene();
        let before =
            Request::new(source.clone(), [LightmapPageId(0), LightmapPageId(1)], 0.0).unwrap();
        let after =
            Request::new(source.clone(), [LightmapPageId(1), LightmapPageId(0)], 0.1).unwrap();
        assert_eq!(before.keys[0], after.keys[0]);
        assert_ne!(before.keys[1], after.keys[1]);
        let repeated = Request::new(source, [LightmapPageId(1), LightmapPageId(1)], 0.2).unwrap();
        assert_eq!(repeated.keys, vec![before.keys[1].clone()]);
        let other = Request::new(scene(), [LightmapPageId(0)], 0.0).unwrap();
        assert_ne!(before.keys[0], other.keys[0]);
    }

    #[test]
    fn request_validation_rejects_missing_pages_and_nonfinite_clocks() {
        let source = scene();
        assert!(matches!(
            Request::new(source.clone(), [LightmapPageId(2)], 0.0),
            Err(Error::MissingPage(LightmapPageId(2)))
        ));
        for time in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
            assert!(matches!(
                Request::new(source.clone(), [], time),
                Err(Error::InvalidTime)
            ));
        }
    }
}
