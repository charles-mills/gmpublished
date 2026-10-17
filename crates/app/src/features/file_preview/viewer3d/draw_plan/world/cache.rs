use super::super::{DrawItem, OverlayDrawItem};
use super::{
    DrawPlan, PartitionFilter, ReceiverVisibility, ViewerScene, WorldOpaquePlan, apply, build,
};
use gmpublished_domain::scene::map::MapSurfaceOwnerId;
use std::sync::{Arc, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Source {
    pub source: usize,
    pub first_source: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Input {
    pub meshes: Vec<DrawItem>,
    pub mesh_claims: Vec<bool>,
    pub sources: Vec<Source>,
    pub first_encounters: Vec<usize>,
    pub marks: Vec<OverlayDrawItem>,
    partition: PartitionFilter,
    world_only: bool,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            meshes: Vec::new(),
            mesh_claims: Vec::new(),
            sources: Vec::new(),
            first_encounters: Vec::new(),
            marks: Vec::new(),
            partition: PartitionFilter::World,
            world_only: false,
        }
    }
}
impl Input {
    pub(super) fn encounter_ranks(&self, count: usize) -> Vec<usize> {
        let mut ranks = vec![usize::MAX; count];
        for (rank, &source) in self.first_encounters.iter().enumerate() {
            ranks[source] = rank;
        }
        ranks
    }
    fn prepare(
        &mut self,
        plan: &DrawPlan,
        upload: &ViewerScene,
        visible: Option<&ReceiverVisibility>,
        partition: PartitionFilter,
        world_only: bool,
        first_sources: &mut Vec<bool>,
    ) -> bool {
        let Some(map) = upload.assets.map().filter(|map| map.visibility.is_some()) else {
            return false;
        };
        let index = &upload.assets.receiver_faces;
        self.partition = partition;
        self.world_only = world_only;
        self.meshes.clear();
        self.sources.clear();
        self.first_encounters.clear();
        self.marks.clear();
        self.mesh_claims.resize(upload.meshes.len(), false);
        self.mesh_claims.fill(false);
        first_sources.resize(index.source_count(), false);
        first_sources.fill(false);
        for &item in &plan.opaque {
            if index.world_mesh_partitions()[item.mesh_index]
                .is_some_and(|skybox| partition.accepts(skybox))
            {
                self.meshes.push(item);
                self.mesh_claims[item.mesh_index] = true;
            }
        }
        for source in index.selected_sources(visible) {
            let metadata = &map.surface_receivers[source];
            let Some(receiver) = index.for_source(source) else {
                continue;
            };
            if metadata.owner.id() != MapSurfaceOwnerId::World
                || !self.mesh_claims[receiver.mesh_index]
            {
                continue;
            }
            let first_source = visible
                .and_then(|visible| {
                    visible
                        .receiver_rank(index, receiver.mesh_index, metadata.range.face)
                        .map(|rank| visible.selected[rank])
                })
                .unwrap_or(source);
            self.sources.push(Source {
                source,
                first_source,
            });
            first_sources[first_source] = true;
        }
        if let Some(visible) = visible {
            self.first_encounters.extend(
                visible
                    .selected
                    .iter()
                    .copied()
                    .filter(|&source| first_sources[source]),
            );
        } else {
            self.first_encounters
                .extend((0..index.source_count()).filter(|&source| first_sources[source]));
        }
        self.marks
            .extend(plan.surface_marks.iter().copied().filter(|mark| {
                upload.overlays[mark.overlay_index].owner.id() == MapSurfaceOwnerId::World
                    && mark
                        .receiver_mesh_index
                        .is_some_and(|mesh| self.mesh_claims.get(mesh).copied().unwrap_or(false))
            }));
        true
    }
}

#[derive(Debug)]
struct Entry {
    input: Input,
    output: Arc<WorldOpaquePlan>,
}

#[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
struct Stats {
    hits: u64,
    misses: u64,
    uncached_builds: u64,
    key_ns: u64,
    lookup_ns: u64,
    build_ns: u64,
    reuse_ns: u64,
    apply_ns: u64,
    retired_entries: u64,
}

#[derive(Debug)]
pub(in crate::features::file_preview::viewer3d) struct Cache {
    assets: Weak<super::super::super::pipeline::UploadedModel>,
    entries: [Option<Entry>; 2],
    input: Input,
    first_sources: Vec<bool>,
    enabled: bool,
    #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
    stats: [Stats; 2],
}
impl Default for Cache {
    fn default() -> Self {
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        let enabled =
            !matches!(std::env::var("GMPUBLISHED_SEMANTIC_WORLD_CACHE").as_deref(), Ok("0"));
        #[cfg(not(any(feature = "gui-benchmark", feature = "map-capture")))]
        let enabled = true;
        Self::new(enabled)
    }
}
impl Cache {
    fn new(enabled: bool) -> Self {
        Self {
            assets: Weak::new(),
            entries: [None, None],
            input: Input::default(),
            first_sources: Vec::new(),
            enabled,
            #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
            stats: [Stats::default(); 2],
        }
    }
    fn slot(partition: PartitionFilter) -> usize {
        match partition {
            PartitionFilter::World => 0,
            PartitionFilter::MapSkybox => 1,
        }
    }
    pub fn retire(&mut self, partition: PartitionFilter) {
        let slot = Self::slot(partition);
        if self.entries[slot].take().is_some() {
            #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
            {
                self.stats[slot].retired_entries += 1;
            }
        }
    }
    fn reuse(&self, slot: usize) -> Option<Arc<WorldOpaquePlan>> {
        self.enabled
            .then(|| self.entries[slot].as_ref())
            .flatten()
            .filter(|entry| entry.input == self.input)
            .map(|entry| Arc::clone(&entry.output))
    }
    pub(super) fn attach(
        &mut self,
        plan: &mut DrawPlan,
        upload: &ViewerScene,
        visible: Option<&ReceiverVisibility>,
        partition: PartitionFilter,
        world_only: bool,
    ) {
        let slot = Self::slot(partition);
        if !self.assets.ptr_eq(&Arc::downgrade(&upload.assets)) {
            self.retire(PartitionFilter::World);
            self.retire(PartitionFilter::MapSkybox);
            self.assets = Arc::downgrade(&upload.assets);
        }
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        let key_start = std::time::Instant::now();
        let prepared = self.input.prepare(
            plan,
            upload,
            visible,
            partition,
            world_only,
            &mut self.first_sources,
        );
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        {
            self.stats[slot].key_ns += elapsed(key_start);
        }
        if !prepared {
            self.retire(partition);
            return;
        }
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        let lookup_start = std::time::Instant::now();
        let reused = self.reuse(slot);
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        let hit = reused.is_some();
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        {
            self.stats[slot].lookup_ns += elapsed(lookup_start);
        }
        let output = if let Some(output) = reused {
            #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
            {
                self.stats[slot].hits += 1;
            }
            output
        } else {
            #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
            let build_start = std::time::Instant::now();
            let output = Arc::new(build(upload, &self.input));
            #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
            {
                self.stats[slot].build_ns += elapsed(build_start);
                if self.enabled {
                    self.stats[slot].misses += 1;
                } else {
                    self.stats[slot].uncached_builds += 1;
                }
            }
            if self.enabled {
                let old = self.entries[slot].take();
                #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
                if old.is_some() {
                    self.stats[slot].retired_entries += 1;
                }
                let input = std::mem::replace(
                    &mut self.input,
                    old.map_or_else(Input::default, |entry| entry.input),
                );
                self.entries[slot] = Some(Entry {
                    input,
                    output: Arc::clone(&output),
                });
            }
            output
        };
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        let apply_start = std::time::Instant::now();
        apply(output, plan);
        #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
        {
            let duration = elapsed(apply_start);
            self.stats[slot].apply_ns += duration;
            if hit {
                self.stats[slot].reuse_ns += duration;
            }
        }
    }
    #[cfg(feature = "map-capture")]
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"enabled":self.enabled,"partitions":self.stats,"retained_entries":self.entries.iter().flatten().count()})
    }
}
#[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
fn elapsed(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
impl Drop for Cache {
    fn drop(&mut self) {
        self.retire(PartitionFilter::World);
        self.retire(PartitionFilter::MapSkybox);
        log::info!(
            "semantic world cache enabled={} world={:?} sky={:?}",
            self.enabled,
            self.stats[0],
            self.stats[1]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            meshes: vec![DrawItem {
                mesh_index: 2,
                material_slot: 4,
            }],
            mesh_claims: vec![false, false, true],
            sources: vec![
                Source {
                    source: 7,
                    first_source: 7,
                },
                Source {
                    source: 9,
                    first_source: 7,
                },
            ],
            first_encounters: vec![7],
            ..Input::default()
        }
    }

    #[test]
    fn exact_semantics_reuse_cpu_arc_and_every_changed_dependency_misses() {
        let original = input();
        let output = Arc::new(WorldOpaquePlan::default());
        let mut cache = Cache::new(true);
        cache.input = original.clone();
        cache.entries[0] = Some(Entry {
            input: original.clone(),
            output: Arc::clone(&output),
        });
        assert!(Arc::ptr_eq(&cache.reuse(0).unwrap(), &output));
        let mut changes = Vec::new();
        let mut changed = original.clone();
        changed.meshes[0].material_slot = 3;
        changes.push(changed);
        let mut changed = original.clone();
        changed.mesh_claims[1] = true;
        changes.push(changed);
        let mut changed = original.clone();
        changed.sources.reverse();
        changes.push(changed);
        let mut changed = original.clone();
        changed.sources[1].first_source = 9;
        changes.push(changed);
        let mut changed = original.clone();
        changed.first_encounters.push(9);
        changes.push(changed);
        let mut changed = original.clone();
        changed.partition = PartitionFilter::MapSkybox;
        changes.push(changed);
        let mut changed = original.clone();
        changed.world_only = true;
        changes.push(changed);
        for changed in changes {
            cache.input = changed;
            assert!(cache.reuse(0).is_none());
        }
        cache.input = original;
        cache.enabled = false;
        assert!(cache.reuse(0).is_none());
    }

    #[test]
    fn mark_membership_material_range_and_order_are_semantic_dependencies() {
        let mark = |overlay_index| OverlayDrawItem {
            blend: crate::bridge::materials::RenderMode::Opaque,
            receiver_mesh_index: Some(2),
            receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 7,
                start: 0,
                count: 3,
            },
            overlay_index,
            material_slot: 4,
        };
        let mut original = input();
        original.marks = vec![mark(0), mark(1)];
        let mut cache = Cache::new(true);
        cache.input = original.clone();
        cache.entries[0] = Some(Entry {
            input: original.clone(),
            output: Arc::new(WorldOpaquePlan::default()),
        });
        assert!(cache.reuse(0).is_some());
        cache.input.marks.reverse();
        assert!(cache.reuse(0).is_none());
        cache.input = original.clone();
        cache.input.marks.pop();
        assert!(cache.reuse(0).is_none());
        cache.input = original.clone();
        cache.input.marks[0].material_slot = 1;
        assert!(cache.reuse(0).is_none());
        cache.input = original;
        cache.input.marks[0].receiver_range.start = 3;
        assert!(cache.reuse(0).is_none());
    }

    #[test]
    fn encounter_compression_preserves_ties_without_absolute_rank_holes() {
        let mut input = input();
        input.sources = vec![
            Source {
                source: 7,
                first_source: 7,
            },
            Source {
                source: 9,
                first_source: 9,
            },
            Source {
                source: 11,
                first_source: 7,
            },
        ];
        input.first_encounters = vec![7, 9];
        let ranks = input.encounter_ranks(12);
        let keys = input
            .sources
            .iter()
            .map(|source| ranks[source.first_source])
            .collect::<Vec<_>>();
        assert_eq!(keys, [0, 1, 0]);
        let mut order = [0, 1, 2];
        order.sort_by_key(|&index| keys[index]);
        assert_eq!(order, [0, 2, 1]);
    }

    #[test]
    fn partition_retirement_releases_only_its_cpu_payload() {
        let world = Arc::new(WorldOpaquePlan::default());
        let sky = Arc::new(WorldOpaquePlan::default());
        let world_guard = Arc::downgrade(&world);
        let sky_guard = Arc::downgrade(&sky);
        let mut cache = Cache::default();
        cache.entries[0] = Some(Entry {
            input: input(),
            output: world,
        });
        cache.entries[1] = Some(Entry {
            input: input(),
            output: sky,
        });
        cache.retire(PartitionFilter::World);
        assert!(world_guard.upgrade().is_none());
        assert!(sky_guard.upgrade().is_some());
        cache.retire(PartitionFilter::MapSkybox);
        assert!(sky_guard.upgrade().is_none());
    }
}
