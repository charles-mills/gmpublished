use super::{
    BoundsBuilder, BspModel, GeometryPartition, MapBounds, MapBsp, MapLeafLocator, MapPlayerStart,
    MapPropVisibility, MapSkyCamera, MapVisibilityBucket, ModelIndex,
    SKYBOX_COMPLETION_AABB_EXPANSION, SKYBOX_COMPLETION_MAX_WORLD_VOLUME_FRACTION, Visibility,
    bounds_contains_point, bounds_volume, bsp_world_bounds, expand_bounds,
    normalize_static_prop_model_path,
};
use crate::math::Vec3;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct MapVisibility {
    pub(super) cluster_count: u32,
    pub(super) locator: Arc<MapLeafLocator>,
    pub(super) vis: Option<Visibility<'static>>,
    ordering: Arc<FaceOrdering>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapDecalProjection {
    locator: Arc<MapLeafLocator>,
    ordering: Arc<FaceOrdering>,
    root: i32,
    origin: Vec3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapBrushModelRendering {
    locator: Arc<MapLeafLocator>,
    ordering: Arc<FaceOrdering>,
    model: order::BrushModel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapBrushSurfaceGroup {
    pub faces: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapWorldSurfaceGroup {
    pub leaf_index: u32,
    pub faces: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MapWorldMaterialGroup {
    AboveWater,
    Underwater,
    IntersectsWater,
    Water,
}

impl MapWorldMaterialGroup {
    pub const fn index(self) -> usize {
        match self {
            Self::AboveWater => 0,
            Self::Underwater => 1,
            Self::IntersectsWater => 2,
            Self::Water => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapWorldSurfaceRendering {
    pub material_group: MapWorldMaterialGroup,
    pub displacement: Option<MapDisplacementRendering>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapDisplacementRendering {
    pub ordinal: u32,
    pub full_grid_vertices: u32,
    pub full_grid_indices: u32,
}

impl MapBrushModelRendering {
    /// Model-local compiled bounds, including omitted surfaces.
    pub const fn bounds(&self) -> MapBounds {
        self.model.bounds
    }

    /// Back-to-front node groups; callers select and order their materials.
    pub fn translucent_groups(&self, model_local_eye: Vec3) -> Vec<MapBrushSurfaceGroup> {
        self.translucent_groups_with_culling(model_local_eye, |_| false)
    }

    pub fn translucent_groups_with_culling(
        &self,
        model_local_eye: Vec3,
        two_sided: impl Fn(u32) -> bool,
    ) -> Vec<MapBrushSurfaceGroup> {
        self.ordering
            .brush_surface_groups(&self.locator, &self.model, model_local_eye, two_sided)
    }
}

impl MapDecalProjection {
    pub(super) fn visit<E>(
        &self,
        radius: f32,
        project: impl FnMut(u32) -> Result<bool, E>,
    ) -> Result<(), E> {
        self.ordering
            .visit_decal_faces(&self.locator, self.root, self.origin, radius, project)
    }
}

#[path = "visibility_order.rs"]
mod order;
use order::FaceOrdering;

pub(super) const PROP_AABB_MAX_EXTENT: f32 = 8192.0;
pub(super) const PROP_AABB_MAX_LEAVES: usize = 4096;
pub(super) const PROP_AABB_MAX_DEPTH: usize = 256;

impl MapVisibility {
    pub(super) fn from_bsp(
        bsp: &MapBsp,
        displacement_bounds: impl Iterator<Item = (u32, MapBounds)>,
    ) -> Option<Self> {
        (!bsp.leaves.is_empty()).then(|| {
            let locator = Arc::new(MapLeafLocator::from_bsp(bsp));
            let ordering = Arc::new(FaceOrdering::from_bsp(bsp, &locator, displacement_bounds));
            Self {
                cluster_count: bsp.cluster_count(),
                locator,
                vis: bsp.visibility.clone(),
                ordering,
            }
        })
    }

    pub fn leaf_is_underwater(&self, leaf: u32) -> Option<bool> {
        self.ordering.leaf_is_underwater(leaf)
    }

    pub fn surface_rendering(&self, face: u32) -> Option<MapWorldSurfaceRendering> {
        self.ordering.surface_rendering(face)
    }

    pub fn displacement_surfaces(
        &self,
    ) -> impl Iterator<Item = (u32, MapDisplacementRendering)> + '_ {
        self.ordering.displacement_surfaces()
    }

    pub(super) fn decal_projection(&self, root: i32, origin: Vec3) -> MapDecalProjection {
        MapDecalProjection {
            locator: Arc::clone(&self.locator),
            ordering: Arc::clone(&self.ordering),
            root,
            origin,
        }
    }

    pub fn brush_model(&self, model_index: ModelIndex) -> Option<MapBrushModelRendering> {
        Some(MapBrushModelRendering {
            locator: Arc::clone(&self.locator),
            ordering: Arc::clone(&self.ordering),
            model: self.ordering.brush_model(model_index)?.clone(),
        })
    }

    pub const fn cluster_count(&self) -> u32 {
        self.cluster_count
    }

    pub fn cluster_at(&self, point: Vec3) -> Option<i16> {
        let leaf = self.locator.leaf_at(point)?;
        let cluster = self.locator.leaves.get(leaf)?.cluster;
        (cluster >= 0 && cluster_in_range(cluster, self.cluster_count)).then_some(cluster)
    }

    pub fn visible_clusters(&self, cluster: i16) -> Option<Vec<bool>> {
        if !cluster_in_range(cluster, self.cluster_count) {
            return None;
        }
        self.vis.as_ref()?.pvs(usize::try_from(cluster).ok()?)
    }

    pub fn clusters_for_aabb(&self, bounds_min: Vec3, bounds_max: Vec3) -> MapPropVisibility {
        self.locator
            .clusters_for_aabb(bounds_min, bounds_max, self.cluster_count)
    }

    pub fn leaves_for_aabb(&self, bounds: MapBounds) -> Option<Vec<u32>> {
        self.ordering.leaves_for_aabb(&self.locator, bounds)
    }

    pub fn world_surface_groups_in_view(
        &self,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        bounds_visible: impl Fn(MapBounds) -> bool,
        cluster_visible: impl Fn(i16) -> bool,
    ) -> Vec<MapWorldSurfaceGroup> {
        self.ordering.world_surface_groups(
            &self.locator,
            eye,
            visible,
            bounds_visible,
            cluster_visible,
        )
    }

    pub fn ordered_faces_in_view(
        &self,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        bounds_visible: impl Fn(MapBounds) -> bool,
        cluster_visible: impl Fn(i16) -> bool,
    ) -> Vec<u32> {
        self.ordering
            .ordered_faces(&self.locator, eye, visible, bounds_visible, cluster_visible)
    }

    pub fn ordered_faces(&self, eye: Vec3, visible: impl Fn(u32) -> bool) -> Vec<u32> {
        self.ordering
            .ordered_faces(&self.locator, eye, visible, |_| true, |_| true)
    }
}

#[derive(Clone, Debug)]
pub(super) enum SkyboxPartition {
    Inactive {
        sky_camera_present: bool,
    },
    Active {
        camera_reachable: Vec<bool>,
        sky_reachable: Vec<bool>,
        completion_bounds: Option<MapBounds>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FaceAttribution {
    Unknown,
    Skybox,
    Visible,
}

impl SkyboxPartition {
    pub(super) const fn inactive(sky_camera_present: bool) -> Self {
        Self::Inactive { sky_camera_present }
    }

    pub(super) const fn sky_camera_present(&self) -> bool {
        match *self {
            Self::Inactive { sky_camera_present } => sky_camera_present,
            Self::Active { .. } => true,
        }
    }

    pub(super) fn cluster_partition(&self, cluster: i16) -> GeometryPartition {
        let Self::Active {
            camera_reachable,
            sky_reachable,
            ..
        } = self
        else {
            return GeometryPartition::Visible;
        };
        cluster_partition(cluster, camera_reachable, sky_reachable)
    }

    pub(super) fn from_bsp(
        bsp: &MapBsp,
        player_start: Option<&MapPlayerStart>,
        sky_camera: Option<MapSkyCamera>,
    ) -> Option<Self> {
        let sky_camera_origin = sky_camera?.origin;
        let cluster_count = bsp.cluster_count();
        if cluster_count == 0 {
            log::debug!("bsp skybox partition disabled: missing or empty visibility data");
            return None;
        }

        let Some(camera_seed) = camera_seed_cluster(bsp, player_start) else {
            log::debug!("bsp skybox partition disabled: no camera-side cluster seed");
            return None;
        };
        let Some(sky_seed) = point_cluster(bsp, sky_camera_origin) else {
            log::debug!("bsp skybox partition disabled: sky_camera is outside clustered leaves");
            return None;
        };
        if !cluster_in_range(camera_seed, cluster_count)
            || !cluster_in_range(sky_seed, cluster_count)
        {
            log::debug!(
                "bsp skybox partition disabled: seed cluster outside PVS rows (camera {camera_seed}, sky {sky_seed}, rows {cluster_count})"
            );
            return None;
        }

        let camera_reachable = bsp.reachable_clusters(camera_seed);
        let sky_reachable = bsp.reachable_clusters(sky_seed);
        if camera_reachable.is_empty() || sky_reachable.is_empty() {
            log::debug!("bsp skybox partition disabled: empty reachable cluster sets");
            return None;
        }

        Some(Self::Active {
            camera_reachable,
            sky_reachable,
            completion_bounds: None,
        })
    }

    pub(super) fn point_partition(&self, bsp: &MapBsp, point: Vec3) -> GeometryPartition {
        if self.completion_contains_point(point) {
            return GeometryPartition::Skybox;
        }
        self.flood_point_partition(bsp, point)
    }

    pub(super) fn flood_point_partition(&self, bsp: &MapBsp, point: Vec3) -> GeometryPartition {
        let Some(cluster) = point_cluster(bsp, point) else {
            return GeometryPartition::Visible;
        };
        self.cluster_partition(cluster)
    }

    pub(super) fn face_partition(
        &self,
        bsp: &MapBsp,
        face_index: usize,
        flood_partition: GeometryPartition,
    ) -> GeometryPartition {
        if flood_partition == GeometryPartition::Visible
            && self.face_inside_completion_bounds(bsp, face_index)
        {
            GeometryPartition::Skybox
        } else {
            flood_partition
        }
    }

    pub(super) fn apply_completion_bounds(
        &mut self,
        bsp: &MapBsp,
        face_attributions: &FaceAttributions,
        player_start: Option<&MapPlayerStart>,
    ) {
        let Self::Active {
            camera_reachable,
            sky_reachable,
            completion_bounds,
        } = self
        else {
            return;
        };
        *completion_bounds = derive_completion_bounds(
            bsp,
            face_attributions,
            player_start,
            camera_reachable,
            sky_reachable,
        );
    }

    pub(super) fn completion_contains_point(&self, point: Vec3) -> bool {
        self.completion_bounds()
            .is_some_and(|bounds| bounds_contains_point(bounds, point))
    }

    pub(super) fn face_inside_completion_bounds(&self, bsp: &MapBsp, face_index: usize) -> bool {
        let Some(bounds) = self.completion_bounds() else {
            return false;
        };
        let Some(face) = bsp.face(face_index) else {
            return false;
        };
        let vertices = bsp.face_vertex_positions(face);
        !vertices.is_empty()
            && vertices
                .iter()
                .all(|vertex| bounds_contains_point(bounds, *vertex))
    }

    pub(super) const fn completion_bounds(&self) -> Option<MapBounds> {
        match *self {
            Self::Inactive { .. } => None,
            Self::Active {
                completion_bounds, ..
            } => completion_bounds,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FaceAttributions {
    pub(super) partitions: Vec<GeometryPartition>,
    pub(super) visibility: Vec<MapFaceVisibility>,
    pub(super) completion_reattributed_face_count: usize,
}

impl FaceAttributions {
    pub(super) fn from_bsp(bsp: &MapBsp, skybox_partition: &SkyboxPartition) -> Self {
        let mut attributions = vec![FaceAttribution::Unknown; bsp.faces.len()];
        let mut visibility = (0..bsp.faces.len())
            .map(|_| MapFaceVisibilityBuilder::default())
            .collect::<Vec<_>>();
        for leaf in &bsp.leaves {
            let partition = skybox_partition.cluster_partition(leaf.cluster);
            let bucket = visibility_bucket(leaf.cluster, bsp.cluster_count());
            let start = usize::from(leaf.first_leaf_face);
            let end = start.saturating_add(usize::from(leaf.leaf_face_count));
            let Some(leaf_faces) = bsp.leaf_faces.get(start..end) else {
                continue;
            };
            for leaf_face in leaf_faces {
                let face_index = usize::from(*leaf_face);
                let Some(attribution) = attributions.get_mut(face_index) else {
                    continue;
                };
                match partition {
                    GeometryPartition::Skybox if *attribution != FaceAttribution::Visible => {
                        *attribution = FaceAttribution::Skybox;
                    }
                    GeometryPartition::Visible => {
                        *attribution = FaceAttribution::Visible;
                    }
                    GeometryPartition::Skybox => {}
                }
                if let Some(face_visibility) = visibility.get_mut(face_index) {
                    face_visibility.push(bucket);
                }
            }
        }

        let mut completion_reattributed_face_count = 0_usize;
        let partitions = attributions
            .into_iter()
            .enumerate()
            .map(|(face_index, attribution)| {
                let flood_partition = match attribution {
                    FaceAttribution::Skybox => GeometryPartition::Skybox,
                    FaceAttribution::Unknown | FaceAttribution::Visible => {
                        GeometryPartition::Visible
                    }
                };
                let partition = skybox_partition.face_partition(bsp, face_index, flood_partition);
                if flood_partition == GeometryPartition::Visible
                    && partition == GeometryPartition::Skybox
                {
                    completion_reattributed_face_count =
                        completion_reattributed_face_count.saturating_add(1);
                }
                partition
            })
            .collect();
        let visibility = visibility
            .into_iter()
            .map(MapFaceVisibilityBuilder::finish)
            .collect();

        Self {
            partitions,
            visibility,
            completion_reattributed_face_count,
        }
    }

    pub(super) fn partition(&self, face_index: usize) -> GeometryPartition {
        self.partitions
            .get(face_index)
            .copied()
            .unwrap_or(GeometryPartition::Visible)
    }

    pub(super) fn visibility(&self, face_index: usize) -> MapFaceVisibility {
        self.visibility
            .get(face_index)
            .cloned()
            .unwrap_or_else(MapFaceVisibility::always)
    }

    pub(super) fn skybox_face_count(&self) -> usize {
        self.partitions
            .iter()
            .filter(|partition| **partition == GeometryPartition::Skybox)
            .count()
    }

    pub(super) fn completion_reattributed_face_count(&self) -> usize {
        self.completion_reattributed_face_count
    }
}

/// No clusters means always visible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MapFaceVisibility {
    pub(super) always_visible: bool,
    pub(super) clusters: Vec<u32>,
}

impl MapFaceVisibility {
    pub(super) fn always() -> Self {
        Self {
            always_visible: true,
            clusters: Vec::new(),
        }
    }

    pub(super) fn from_bucket(bucket: MapVisibilityBucket) -> Self {
        let mut builder = MapFaceVisibilityBuilder::default();
        builder.push(bucket);
        builder.finish()
    }
}

#[derive(Debug, Default)]
pub(super) struct MapFaceVisibilityBuilder {
    always_visible: bool,
    clusters: Vec<u32>,
}

impl MapFaceVisibilityBuilder {
    pub(super) fn push(&mut self, bucket: MapVisibilityBucket) {
        match bucket {
            MapVisibilityBucket::Always => self.always_visible = true,
            MapVisibilityBucket::Cluster(cluster) => {
                if !self.clusters.contains(&cluster) {
                    self.clusters.push(cluster);
                }
            }
        }
    }

    pub(super) fn finish(mut self) -> MapFaceVisibility {
        if self.clusters.is_empty() {
            self.always_visible = true;
        } else {
            self.clusters.sort_unstable();
        }
        MapFaceVisibility {
            always_visible: self.always_visible,
            clusters: self.clusters,
        }
    }
}

pub(super) fn visibility_bucket(cluster: i16, cluster_count: u32) -> MapVisibilityBucket {
    if cluster_in_range(cluster, cluster_count) {
        MapVisibilityBucket::Cluster(
            u32::try_from(cluster).expect("cluster_in_range guarantees a nonnegative cluster"),
        )
    } else {
        MapVisibilityBucket::Always
    }
}

fn derive_completion_bounds(
    bsp: &MapBsp,
    face_attributions: &FaceAttributions,
    player_start: Option<&MapPlayerStart>,
    camera_reachable: &[bool],
    sky_reachable: &[bool],
) -> Option<MapBounds> {
    let seed_bounds =
        seed_completion_bounds(bsp, face_attributions, camera_reachable, sky_reachable)?;
    let completion_bounds =
        expand_bounds(seed_bounds, Vec3::splat(SKYBOX_COMPLETION_AABB_EXPANSION));
    if player_start.is_some_and(|start| bounds_contains_point(completion_bounds, start.origin)) {
        log::debug!("bsp skybox completion disabled: completion AABB contains player spawn");
        return None;
    }
    let Some(world_bounds) = bsp_world_bounds(bsp) else {
        log::debug!("bsp skybox completion disabled: unable to derive world bounds");
        return None;
    };
    let completion_volume = bounds_volume(completion_bounds);
    let world_volume = bounds_volume(world_bounds);
    if !completion_volume.is_finite()
        || !world_volume.is_finite()
        || world_volume <= 0.0
        || completion_volume > world_volume * SKYBOX_COMPLETION_MAX_WORLD_VOLUME_FRACTION
    {
        log::debug!(
            "bsp skybox completion disabled: completion volume {completion_volume}, world volume {world_volume}"
        );
        return None;
    }
    Some(completion_bounds)
}

fn seed_completion_bounds(
    bsp: &MapBsp,
    face_attributions: &FaceAttributions,
    camera_reachable: &[bool],
    sky_reachable: &[bool],
) -> Option<MapBounds> {
    let mut bounds = BoundsBuilder::default();
    // Entity-model bounds are local; only model zero can seed the world volume.
    for face_index in bsp
        .models
        .first()
        .and_then(|model| model_face_range(model, bsp.faces.len()))
        .into_iter()
        .flatten()
    {
        if face_attributions.partition(face_index) != GeometryPartition::Skybox {
            continue;
        }
        let Some(face) = bsp.face(face_index) else {
            continue;
        };
        for vertex in bsp.face_vertex_positions(face) {
            bounds.push(vertex);
        }
    }
    for prop in bsp.static_props_iter() {
        let origin = prop.origin;
        let partition = point_cluster(bsp, Vec3::from(origin))
            .map_or(GeometryPartition::Visible, |cluster| {
                cluster_partition(cluster, camera_reachable, sky_reachable)
            });
        if normalize_static_prop_model_path(bsp.static_prop_model(prop)).is_some()
            && partition == GeometryPartition::Skybox
        {
            bounds.push(Vec3::from(origin));
        }
    }
    bounds.finish()
}

pub(super) fn cluster_partition(
    cluster: i16,
    camera_reachable: &[bool],
    sky_reachable: &[bool],
) -> GeometryPartition {
    let Some(cluster) = usize::try_from(cluster).ok() else {
        return GeometryPartition::Visible;
    };
    if sky_reachable.get(cluster).copied().unwrap_or(false)
        && !camera_reachable.get(cluster).copied().unwrap_or(false)
    {
        GeometryPartition::Skybox
    } else {
        GeometryPartition::Visible
    }
}

pub(super) fn camera_seed_cluster(
    bsp: &MapBsp,
    player_start: Option<&MapPlayerStart>,
) -> Option<i16> {
    player_start
        .and_then(|start| point_cluster(bsp, start.origin))
        .or_else(|| {
            bsp.leaves
                .iter()
                .find(|leaf| leaf.cluster >= 0)
                .map(|leaf| leaf.cluster)
        })
}

pub(super) fn point_cluster(bsp: &MapBsp, point: Vec3) -> Option<i16> {
    let leaf_index = bsp.leaf_at(point)?;
    let cluster = bsp.leaves.get(leaf_index)?.cluster;
    (cluster >= 0).then_some(cluster)
}

pub(super) fn cluster_in_range(cluster: i16, cluster_count: u32) -> bool {
    usize::try_from(cluster)
        .ok()
        .is_some_and(|cluster| cluster < cluster_count as usize)
}

pub(super) fn model_face_range(
    model: &BspModel,
    face_count: usize,
) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(model.first_face).ok()?;
    let count = usize::try_from(model.face_count).ok()?;
    let end = start.checked_add(count)?;
    (end <= face_count).then_some(start..end)
}

pub(super) fn point_visibility_bucket(
    bsp: &MapBsp,
    point: Vec3,
    cluster_count: u32,
) -> MapVisibilityBucket {
    point_cluster(bsp, point).map_or(MapVisibilityBucket::Always, |cluster| {
        visibility_bucket(cluster, cluster_count)
    })
}
