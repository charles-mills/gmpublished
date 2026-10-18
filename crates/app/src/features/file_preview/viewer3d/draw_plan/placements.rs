use super::{DrawPlan, PartitionFilter, ViewerScene, WorldVisibilityPlan};
use crate::features::file_preview::viewer3d::pipeline::UploadedMesh;
use crate::media::preview_model::{
    MapMeshVisibility, MapPreview, MapRenderableSource, MeshGeometry, ModelVertex, RenderScene,
    vertex_lighting,
};
use gmpublished_domain::scene::map::{MapBounds, MapPreparedBounds};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Debug, Default)]
pub(in super::super) struct OpaqueRanges {
    pub(in super::super) indices: Vec<Range<u32>>,
    pub(in super::super) contexts: Vec<usize>,
}

#[derive(Debug, Default)]
pub(in super::super) struct OpaquePlacementIndex {
    meshes: Vec<Option<MeshPlacements>>,
    owner_count: usize,
    static_prop_frustum_bounds: Vec<Option<MapBounds>>,
}

#[derive(Debug)]
struct MeshPlacements {
    parts: Vec<Part>,
    pvs_ordered: bool,
}

#[derive(Debug)]
struct Part {
    owner: usize,
    indices: Range<u32>,
    bounds: Option<MapBounds>,
    contexts: Vec<usize>,
    visibility: PartVisibility,
}

#[derive(Debug)]
enum PartVisibility {
    Always(usize),
    Clusters(Vec<(u32, usize)>),
    Never,
}

impl PartVisibility {
    fn encounter(&self, clusters: &[bool]) -> Option<usize> {
        match self {
            Self::Always(order) => Some(*order),
            Self::Never => None,
            Self::Clusters(indices) => indices
                .iter()
                .filter_map(|&(cluster, order)| {
                    clusters
                        .get(cluster as usize)
                        .copied()
                        .unwrap_or(false)
                        .then_some(order)
                })
                .min(),
        }
    }
}

impl OpaquePlacementIndex {
    pub(in super::super) fn prepare(
        meshes: &[UploadedMesh],
        renderables: &[crate::features::file_preview::viewer3d::pipeline::UploadedMapRenderable],
        scene: &RenderScene,
        map: Option<&MapPreview>,
    ) -> Self {
        let Some(map) = map else {
            return Self::default();
        };
        let mut parts = (0..meshes.len()).map(|_| Vec::new()).collect::<Vec<_>>();
        // Assets::upload builds mesh_instances from these same immutable source
        // bindings and each UploadedMesh::receiver_source. Include every binding,
        // even when an animation has not produced a viewer buffer yet.
        let animated_sources: HashSet<_> = map
            .model_animations
            .iter()
            .flat_map(|instance| {
                instance
                    .bindings
                    .iter()
                    .map(|binding| binding.receiver_mesh)
            })
            .collect();
        let mut static_prop_frustum_bounds = Vec::with_capacity(renderables.len());
        for (owner, renderable) in renderables.iter().enumerate() {
            let sources = if renderable.map_skybox {
                &map.map_skybox_renderables
            } else {
                &map.renderables
            };
            let source = &sources[renderable.source_index];
            let mut geometry = Vec::new();
            for range in &renderable.ranges {
                let uploaded = &meshes[range.mesh_index];
                let source_meshes = if uploaded.map_skybox {
                    &map.map_skybox_meshes
                } else {
                    &scene.meshes
                };
                let mesh = source_meshes.get(uploaded.scene_mesh_index);
                let data = mesh.and_then(|mesh| {
                    let indices = mesh
                        .indices
                        .get(range.indices.start as usize..range.indices.end as usize)?;
                    Some(geometry_references(&mesh.vertices, indices))
                });
                geometry.push((range, data));
            }
            let bounds = owner_bounds(
                geometry
                    .iter()
                    .map(|(_, data)| data.as_ref().and_then(|(bounds, _)| *bounds)),
                source.bounds.as_ref().ok().map(|bounds| bounds.world),
            );
            static_prop_frustum_bounds.push(static_prop_owner_bounds(
                source.source,
                source.bounds.as_ref().ok().map(|bounds| bounds.world),
                bounds,
                renderable.ranges.iter().map(|range| {
                    meshes.get(range.mesh_index).map(|mesh| StaticPropGeometry {
                        geometry: mesh.geometry,
                        door: mesh.door_index.is_some(),
                        animated: animated_sources.contains(&mesh.receiver_source()),
                    })
                }),
            ));
            for (range, data) in geometry {
                if !range.indices.is_empty()
                    && let Some((_, contexts)) = data
                {
                    parts[range.mesh_index].push(Part {
                        owner,
                        indices: range.indices.clone(),
                        bounds,
                        contexts,
                        visibility: PartVisibility::Always(0),
                    });
                }
            }
        }
        Self {
            owner_count: renderables.len(),
            static_prop_frustum_bounds,
            meshes: meshes
                .iter()
                .zip(parts)
                .map(|(uploaded, parts)| {
                    if uploaded.geometry != MeshGeometry::Model || uploaded.door_index.is_some() {
                        return None;
                    }
                    let visibility = (!uploaded.map_skybox)
                        .then(|| map.mesh_visibility.get(uploaded.scene_mesh_index))
                        .flatten();
                    MeshPlacements::prepare(uploaded.index_count, parts, visibility)
                })
                .collect(),
        }
    }

    pub(in super::super) fn static_prop_frustum_bounds(&self, owner: usize) -> Option<MapBounds> {
        self.static_prop_frustum_bounds
            .get(owner)
            .copied()
            .flatten()
    }
}

impl MeshPlacements {
    fn prepare(
        index_count: u32,
        mut parts: Vec<Part>,
        visibility: Option<&MapMeshVisibility>,
    ) -> Option<Self> {
        if parts.is_empty() {
            return None;
        }
        parts.sort_by_key(|part| part.indices.start);
        let mut end = 0;
        for part in &parts {
            if part.indices.start != end
                || part.indices.end <= end
                || part.indices.end > index_count
            {
                return None;
            }
            end = part.indices.end;
        }
        if end != index_count {
            return None;
        }
        let visibility = visibility.filter(|visibility| {
            !visibility.always_visible.is_empty() || !visibility.clusters.is_empty()
        });
        if let Some(visibility) = visibility {
            let lookup = parts
                .iter()
                .enumerate()
                .map(|(index, part)| ((part.indices.start, part.indices.end), index))
                .collect::<HashMap<_, _>>();
            let mut faces = HashMap::new();
            let mut range_faces = HashMap::new();
            for part in &mut parts {
                part.visibility = PartVisibility::Never;
            }
            for (order, (range, cluster)) in visibility
                .always_visible
                .iter()
                .map(|range| (range, None))
                .chain(visibility.clusters.iter().flat_map(|cluster| {
                    cluster
                        .ranges
                        .iter()
                        .map(|range| (range, Some(cluster.cluster)))
                }))
                .enumerate()
            {
                let end = range.start.checked_add(range.count)?;
                let key = (range.start, end);
                let &index = lookup.get(&key)?;
                if faces
                    .insert(range.face, key)
                    .is_some_and(|previous| previous != key)
                {
                    return None;
                }
                if range_faces
                    .insert(key, range.face)
                    .is_some_and(|previous| previous != range.face)
                {
                    return None;
                }
                match (&mut parts[index].visibility, cluster) {
                    (PartVisibility::Always(_), _) => {}
                    (visibility, None) => *visibility = PartVisibility::Always(order),
                    (PartVisibility::Clusters(clusters), Some(cluster)) => {
                        clusters.push((cluster, order));
                    }
                    (visibility @ PartVisibility::Never, Some(cluster)) => {
                        *visibility = PartVisibility::Clusters(vec![(cluster, order)]);
                    }
                }
            }
            for part in &mut parts {
                if let PartVisibility::Clusters(clusters) = &mut part.visibility {
                    clusters.sort_unstable();
                    clusters.dedup_by_key(|entry| entry.0);
                }
            }
        }
        Some(Self {
            parts,
            pvs_ordered: visibility.is_some(),
        })
    }

    fn select(
        &self,
        clusters: Option<&[bool]>,
        mut intersects: impl FnMut(usize, MapBounds) -> bool,
    ) -> OpaqueRanges {
        let mut selected = OpaqueRanges::default();
        let mut visit = |part: &Part| {
            if part
                .bounds
                .is_some_and(|bounds| !intersects(part.owner, bounds))
            {
                return;
            }
            match selected.indices.last_mut() {
                Some(previous) if previous.end == part.indices.start => {
                    previous.end = part.indices.end;
                }
                _ => selected.indices.push(part.indices.clone()),
            }
            selected.contexts.extend_from_slice(&part.contexts);
        };
        if let Some(clusters) = clusters.filter(|_| self.pvs_ordered) {
            let mut order = self
                .parts
                .iter()
                .filter_map(|part| {
                    part.visibility
                        .encounter(clusters)
                        .map(|order| (order, part))
                })
                .collect::<Vec<_>>();
            order.sort_unstable_by_key(|(order, _)| *order);
            for (_, part) in order {
                visit(part);
            }
        } else {
            for part in &self.parts {
                visit(part);
            }
        }
        selected.contexts.sort_unstable();
        selected.contexts.dedup();
        selected
    }
}

pub(super) fn attach(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    partition: PartitionFilter,
    visibility: Option<&WorldVisibilityPlan>,
    view: Option<&crate::features::file_preview::viewer3d::Uniforms>,
) {
    let query = view.map(crate::features::file_preview::viewer3d::Uniforms::bounds_query);
    let clusters = (partition == PartitionFilter::World)
        .then_some(visibility)
        .flatten()
        .map(|visibility| visibility.visible_clusters.as_slice());
    let mut owners = vec![None; upload.assets.opaque_placements.owner_count];
    for item in &plan.opaque {
        if let Some(mesh) = upload
            .assets
            .opaque_placements
            .meshes
            .get(item.mesh_index)
            .and_then(Option::as_ref)
        {
            plan.opaque_ranges.insert(
                item.mesh_index,
                mesh.select(clusters, |owner, bounds| {
                    *owners[owner].get_or_insert_with(|| {
                        query.as_ref().is_none_or(|query| query.intersects(bounds))
                    })
                }),
            );
        }
    }
    plan.opaque.retain(|item| {
        plan.opaque_ranges
            .get(&item.mesh_index)
            .is_none_or(|ranges| !ranges.indices.is_empty())
    });
}

#[derive(Clone, Copy)]
struct StaticPropGeometry {
    geometry: MeshGeometry,
    door: bool,
    animated: bool,
}

fn static_prop_owner_bounds(
    source: MapRenderableSource,
    compiled: Option<MapBounds>,
    combined: Option<MapBounds>,
    geometry: impl IntoIterator<Item = Option<StaticPropGeometry>>,
) -> Option<MapBounds> {
    if !matches!(source, MapRenderableSource::StaticProp { .. })
        || !MapPreparedBounds::new(compiled?).is_valid()
    {
        return None;
    }
    let bounds = combined?;
    if !MapPreparedBounds::new(bounds).is_valid() {
        return None;
    }
    let mut any = false;
    for part in geometry {
        let part = part?;
        if part.geometry != MeshGeometry::Model || part.door || part.animated {
            return None;
        }
        any = true;
    }
    any.then_some(bounds)
}

fn owner_bounds(
    geometry: impl IntoIterator<Item = Option<MapBounds>>,
    compiled: Option<MapBounds>,
) -> Option<MapBounds> {
    let mut bounds = None;
    for part in geometry {
        bounds = Some(union(bounds, part?));
    }
    bounds.map(|bounds| compiled.map_or(bounds, |compiled| union(Some(bounds), compiled)))
}

fn union(previous: Option<MapBounds>, next: MapBounds) -> MapBounds {
    previous.map_or(next, |previous| MapBounds {
        min: gmpublished_domain::math::Vec3::from(std::array::from_fn(|axis| {
            previous.min[axis].min(next.min[axis])
        })),
        max: gmpublished_domain::math::Vec3::from(std::array::from_fn(|axis| {
            previous.max[axis].max(next.max[axis])
        })),
    })
}

fn geometry_references(
    vertices: &[ModelVertex],
    indices: &[u32],
) -> (Option<MapBounds>, Vec<usize>) {
    let mut bounds = None;
    let mut contexts = BTreeSet::new();
    let mut complete_bounds = true;
    for &index in indices {
        let Some(vertex) = vertices.get(index as usize) else {
            complete_bounds = false;
            continue;
        };
        let position = vertex.position;
        if position.is_finite() {
            bounds = Some(union(
                bounds,
                MapBounds {
                    min: position,
                    max: position,
                },
            ));
        } else {
            complete_bounds = false;
        }
        if vertex.lighting_mode & vertex_lighting::RUNTIME_CONTEXT != 0
            && let Some(context) = crate::media::preview_model::ModelLightingContextId::from_gpu(
                vertex.lighting_context,
            )
        {
            contexts.insert(context.index());
        }
    }
    (
        complete_bounds.then_some(bounds).flatten(),
        contexts.into_iter().collect(),
    )
}

#[cfg(test)]
mod tests;
