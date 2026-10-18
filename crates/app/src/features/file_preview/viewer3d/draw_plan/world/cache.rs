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

#[derive(Debug)]
pub(in crate::features::file_preview::viewer3d) struct Cache {
    assets: Weak<super::super::super::pipeline::UploadedModel>,
    entries: [Option<Entry>; 2],
    input: Input,
    first_sources: Vec<bool>,
}
impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}
impl Cache {
    fn new() -> Self {
        Self {
            assets: Weak::new(),
            entries: [None, None],
            input: Input::default(),
            first_sources: Vec::new(),
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
        self.entries[slot] = None;
    }
    fn reuse(&self, slot: usize) -> Option<Arc<WorldOpaquePlan>> {
        self.entries[slot]
            .as_ref()
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

        let prepared = self.input.prepare(
            plan,
            upload,
            visible,
            partition,
            world_only,
            &mut self.first_sources,
        );
        if !prepared {
            self.retire(partition);
            return;
        }

        let reused = self.reuse(slot);
        let output = if let Some(output) = reused {
            output
        } else {
            let output = Arc::new(build(upload, &self.input));
            let old = self.entries[slot].take();
            let input = std::mem::replace(
                &mut self.input,
                old.map_or_else(Input::default, |entry| entry.input),
            );
            self.entries[slot] = Some(Entry {
                input,
                output: Arc::clone(&output),
            });
            output
        };

        apply(output, plan);
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
        let mut cache = Cache::new();
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
        let mut cache = Cache::new();
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
