use super::{PartitionFilter, WorldPreparationIssue, validate_rendering};
use crate::features::file_preview::viewer3d::pipeline::{UploadedMesh, UploadedReceiver};
use crate::media::preview_model::MapPreview;
use gmpublished_domain::scene::map::MapSurfaceOwnerId;
use std::collections::HashMap;

#[derive(Debug, Default)]
struct FaceReceivers {
    face: u32,
    receivers: Vec<UploadedReceiver>,
    world_partitions: u8,
}

#[derive(Debug, Default)]
pub struct ReceiverIndex {
    faces: Vec<FaceReceivers>,
    sources: Vec<Option<(usize, usize)>>,
    world_meshes: Vec<Option<bool>>,
    world_faces_by_mesh: Vec<Vec<u32>>,
    canonical_world_meshes: Vec<bool>,
    issues: Vec<(usize, WorldPreparationIssue)>,
}

impl ReceiverIndex {
    pub(super) fn from_faces(
        faces: HashMap<u32, Vec<UploadedReceiver>>,
        source_count: usize,
    ) -> Self {
        let face_count = faces
            .keys()
            .copied()
            .max()
            .map_or(0, |face| face as usize + 1);
        let mut index = Self {
            faces: (0..face_count)
                .map(|face| FaceReceivers {
                    face: face as u32,
                    ..FaceReceivers::default()
                })
                .collect(),
            sources: vec![None; source_count],
            ..Self::default()
        };
        for (face, receivers) in faces {
            for (offset, receiver) in receivers.iter().enumerate() {
                index.sources[receiver.source_index] = Some((face as usize, offset));
            }
            index.faces[face as usize].receivers = receivers;
        }
        index
    }

    pub(in super::super) fn prepare(
        faces: HashMap<u32, Vec<UploadedReceiver>>,
        map: Option<&MapPreview>,
        meshes: &[UploadedMesh],
    ) -> Self {
        let Some(map) = map else {
            return Self::default();
        };
        let mut index = Self::from_faces(faces, map.surface_receivers.len());
        index.world_meshes = vec![None; meshes.len()];
        index.world_faces_by_mesh = vec![Vec::new(); meshes.len()];
        let mut ownership = vec![Vec::new(); meshes.len()];
        for (source_index, source) in map.surface_receivers.iter().enumerate() {
            let Some(receiver) = index.for_source(source_index) else {
                continue;
            };
            let mesh_index = receiver.mesh_index;
            let world = source.owner.id() == MapSurfaceOwnerId::World;
            ownership[mesh_index].push((
                world,
                source.range.start..source.range.start.saturating_add(source.range.count),
            ));
            if !world {
                continue;
            }
            let skybox = meshes[mesh_index].map_skybox;
            index.record_world_face(source.range.face, mesh_index, skybox);
            if let Some(topology) = map.visibility.as_ref()
                && let Err(issue) = validate_rendering(
                    source.range.face,
                    topology.surface_rendering(source.range.face),
                    map.displacement_batches.contains_key(&source.range.face),
                )
            {
                index.issues.push((mesh_index, issue));
            }
        }
        index.canonical_world_meshes = ownership
            .into_iter()
            .zip(meshes)
            .map(|(ranges, mesh)| canonical_world_ranges(mesh.index_count, ranges))
            .collect();
        for faces in &mut index.world_faces_by_mesh {
            faces.sort_unstable();
            faces.dedup();
        }
        index
    }

    fn record_world_face(&mut self, face: u32, mesh: usize, skybox: bool) {
        self.world_meshes[mesh] = Some(skybox);
        self.world_faces_by_mesh[mesh].push(face);
        self.faces[face as usize].world_partitions |= if skybox { 2 } else { 1 };
    }

    pub(in super::super) fn world_faces_for_meshes(
        &self,
        meshes: impl IntoIterator<Item = usize>,
    ) -> Vec<u32> {
        let mut faces = meshes
            .into_iter()
            .filter_map(|mesh| self.world_faces_by_mesh.get(mesh))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        faces.sort_unstable();
        faces.dedup();
        faces
    }

    pub(in super::super) fn get(&self, face: &u32) -> Option<&Vec<UploadedReceiver>> {
        let receivers = &self.faces.get(*face as usize)?.receivers;
        (!receivers.is_empty()).then_some(receivers)
    }

    pub(in super::super) fn for_source(&self, source: usize) -> Option<&UploadedReceiver> {
        let (face, offset) = self.sources.get(source).copied().flatten()?;
        self.faces.get(face)?.receivers.get(offset)
    }

    pub(in super::super) fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub(in super::super) fn world_mesh_partitions(&self) -> &[Option<bool>] {
        &self.world_meshes
    }

    pub(in super::super) fn canonical_world_mesh(&self, mesh: usize) -> bool {
        self.canonical_world_meshes
            .get(mesh)
            .copied()
            .unwrap_or(false)
    }

    pub(in super::super) fn world_face(&self, face: u32, partition: PartitionFilter) -> bool {
        let bit = match partition {
            PartitionFilter::World => 1,
            PartitionFilter::MapSkybox => 2,
        };
        self.faces
            .get(face as usize)
            .is_some_and(|face| face.world_partitions & bit != 0)
    }

    pub(in super::super) fn selected_sources<'a>(
        &'a self,
        visible: Option<&'a ReceiverVisibility>,
    ) -> impl Iterator<Item = usize> + 'a {
        visible
            .into_iter()
            .flat_map(|visible| visible.selected.iter().copied())
            .chain(
                visible
                    .is_none()
                    .then_some(0..self.source_count())
                    .into_iter()
                    .flatten(),
            )
    }

    pub(in super::super) fn issues(&self) -> &[(usize, WorldPreparationIssue)] {
        &self.issues
    }
}

fn canonical_world_ranges(index_count: u32, mut ranges: Vec<(bool, std::ops::Range<u32>)>) -> bool {
    if ranges.is_empty() || ranges.iter().any(|(world, _)| !world) {
        return false;
    }
    ranges.sort_unstable_by_key(|(_, range)| range.start);
    let mut covered = 0;
    for (_, range) in ranges {
        if range.start != covered || range.end < range.start || range.end > index_count {
            return false;
        }
        covered = range.end;
    }
    covered == index_count
}

fn occupied(face: &FaceReceivers) -> Option<(&u32, &Vec<UploadedReceiver>)> {
    (!face.receivers.is_empty()).then_some((&face.face, &face.receivers))
}

pub struct ReceiverIter<'a> {
    inner: std::slice::Iter<'a, FaceReceivers>,
}

impl<'a> Iterator for ReceiverIter<'a> {
    type Item = (&'a u32, &'a Vec<UploadedReceiver>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.find_map(occupied)
    }
}

impl<'a> IntoIterator for &'a ReceiverIndex {
    type Item = (&'a u32, &'a Vec<UploadedReceiver>);
    type IntoIter = ReceiverIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        ReceiverIter {
            inner: self.faces.iter(),
        }
    }
}

#[derive(Debug, Default)]
pub(in super::super) struct ReceiverVisibility {
    ranks: Vec<Option<usize>>,
    pub(super) selected: Vec<usize>,
    pub(super) meshes: Vec<Option<bool>>,
}

impl ReceiverVisibility {
    pub(in super::super) fn new(
        index: &ReceiverIndex,
        partition: PartitionFilter,
        topology: bool,
    ) -> Self {
        Self {
            ranks: vec![None; index.source_count()],
            selected: Vec::new(),
            meshes: index
                .world_mesh_partitions()
                .iter()
                .map(|skybox| {
                    topology
                        .then_some(*skybox)
                        .flatten()
                        .filter(|skybox| partition.accepts(*skybox))
                        .map(|_| false)
                })
                .collect(),
        }
    }

    pub(in super::super) fn admit(&mut self, receiver: &UploadedReceiver) {
        if self.ranks[receiver.source_index].is_none() {
            self.ranks[receiver.source_index] = Some(self.selected.len());
            self.selected.push(receiver.source_index);
        }
        if let Some(visible @ Some(_)) = self.meshes.get_mut(receiver.mesh_index) {
            *visible = Some(true);
        }
    }

    pub(in super::super) fn receiver_rank(
        &self,
        index: &ReceiverIndex,
        mesh: usize,
        face: u32,
    ) -> Option<usize> {
        index
            .get(&face)?
            .iter()
            .filter(|receiver| receiver.mesh_index == mesh)
            .filter_map(|receiver| self.ranks[receiver.source_index])
            .min()
    }

    pub(in super::super) fn contains(&self, index: &ReceiverIndex, mesh: usize, face: u32) -> bool {
        index.get(&face).is_some_and(|receivers| {
            receivers.iter().any(|receiver| {
                receiver.mesh_index == mesh && self.ranks[receiver.source_index].is_some()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::SourceLightmapPage;

    #[test]
    fn canonical_world_coverage_replaces_only_fully_owned_index_buffers() {
        assert!(canonical_world_ranges(9, vec![(true, 3..9), (true, 0..3)]));
        for ranges in [
            vec![],
            vec![(true, 0..3)],
            vec![(true, 0..3), (true, 6..9)],
            vec![(true, 0..6), (true, 3..9)],
            vec![(true, 0..12)],
            vec![(true, 0..3), (false, 3..9)],
        ] {
            assert!(!canonical_world_ranges(9, ranges));
        }
    }

    fn receiver(source_index: usize, mesh_index: usize) -> UploadedReceiver {
        UploadedReceiver {
            source_index,
            mesh_index,
            material_group: 9,
            lightmap_page: SourceLightmapPage::White,
        }
    }

    #[test]
    fn source_lookup_preserves_multiple_owners_on_one_compiled_face() {
        let index = ReceiverIndex::from_faces(
            HashMap::from([
                (7, vec![receiver(2, 5), receiver(0, 3)]),
                (1, vec![receiver(1, 1)]),
            ]),
            4,
        );
        assert_eq!(index.for_source(0).unwrap().mesh_index, 3);
        assert_eq!(index.for_source(2).unwrap().mesh_index, 5);
        assert!(index.for_source(3).is_none());
        assert!(index.get(&6).is_none());
        assert_eq!(
            (&index)
                .into_iter()
                .map(|(&face, _)| face)
                .collect::<Vec<_>>(),
            [1, 7]
        );
        let mut visible = ReceiverVisibility::new(&index, PartitionFilter::World, false);
        visible.admit(index.for_source(2).unwrap());
        visible.admit(index.for_source(2).unwrap());
        assert!(visible.contains(&index, 5, 7));
        assert!(!visible.contains(&index, 3, 7));
        assert_eq!(visible.selected, [2]);
        assert_eq!(
            index.selected_sources(Some(&visible)).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            index.selected_sources(None).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn transparent_mesh_candidates_match_full_world_scan_and_preserve_receiver_order() {
        let mut index = ReceiverIndex::from_faces(
            HashMap::from([
                (1, vec![receiver(0, 1), receiver(1, 1)]),
                (5, vec![receiver(5, 1), receiver(2, 2)]),
                (9, vec![receiver(3, 3)]),
                (18, vec![receiver(4, 4)]),
            ]),
            6,
        );
        index.world_meshes = vec![None; 5];
        index.world_faces_by_mesh = vec![Vec::new(); 5];
        for (face, mesh) in [(18, 4), (5, 2), (5, 1), (1, 1), (5, 1)] {
            index.record_world_face(face, mesh, false);
        }
        let world_sources = [0, 2, 4, 5];
        let admitted = [2, 1];
        let selected_sources = |receivers: &[UploadedReceiver]| {
            receivers
                .iter()
                .filter(|receiver| {
                    world_sources.contains(&receiver.source_index)
                        && admitted.contains(&receiver.mesh_index)
                })
                .map(|receiver| receiver.source_index)
                .collect::<Vec<_>>()
        };
        let old = (&index)
            .into_iter()
            .filter_map(|(&face, receivers)| {
                let sources = selected_sources(receivers);
                (!sources.is_empty()).then_some((face, sources))
            })
            .collect::<Vec<_>>();
        let candidates = index.world_faces_for_meshes([2, 1, 3, 1, 99]);
        assert_eq!(candidates, [1, 5]);
        let selected = candidates
            .into_iter()
            .map(|face| (face, selected_sources(index.get(&face).unwrap())))
            .collect::<Vec<_>>();
        assert_eq!(selected, old);
        assert_eq!(selected, [(1, vec![0]), (5, vec![5, 2])]);
        assert!(index.world_faces_for_meshes([0, 3, 99]).is_empty());
        assert!(index.world_faces_for_meshes([]).is_empty());
        assert!(index.world_face(18, PartitionFilter::World));
    }

    #[test]
    fn split_ranges_share_first_receiver_encounter_without_merging_other_owners() {
        let index = ReceiverIndex::from_faces(
            HashMap::from([(3, vec![receiver(0, 2), receiver(1, 2), receiver(2, 8)])]),
            3,
        );
        let mut visible = ReceiverVisibility::new(&index, PartitionFilter::World, false);
        visible.admit(index.for_source(2).unwrap());
        visible.admit(index.for_source(1).unwrap());
        visible.admit(index.for_source(0).unwrap());
        assert_eq!(visible.receiver_rank(&index, 2, 3), Some(1));
        assert_eq!(visible.receiver_rank(&index, 8, 3), Some(0));
    }

    #[test]
    fn canonical_world_meshes_remain_claimed_when_no_face_survives() {
        let mut index = ReceiverIndex::from_faces(HashMap::from([(0, vec![receiver(0, 0)])]), 1);
        index.world_meshes = vec![Some(false), Some(true), None];
        index.faces[0].world_partitions = 1;
        let mut visible = ReceiverVisibility::new(&index, PartitionFilter::World, true);
        assert_eq!(visible.meshes, [Some(false), None, None]);
        assert!(index.world_face(0, PartitionFilter::World));
        assert!(!index.world_face(0, PartitionFilter::MapSkybox));
        visible.admit(index.for_source(0).unwrap());
        assert_eq!(visible.meshes, [Some(true), None, None]);
        assert_eq!(
            ReceiverVisibility::new(&index, PartitionFilter::World, false).meshes,
            [None, None, None]
        );
    }
}
