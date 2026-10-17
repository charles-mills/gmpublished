use super::MapMeshIndexRange;
#[cfg(test)]
use super::ModelVertex;
use super::props::{
    PropBuildMesh, PropBuildMeshVisibility, PropBuildRenderableRange, PropModelAsset,
    bake_world_vertex, prop_skin_table, transform_prop_normal, transform_prop_position,
};
use super::{ModelData, SelectedPropPlacement, StaticPropPlacement};
pub(super) use crate::media::preview_model::{
    GeometryComponent, GeometryCounts, GeometryDiagnostic, GeometryError, PropGeometryReport,
};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub(super) struct Destination {
    pub material_index: usize,
    pub entity: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SourceMesh {
    pub mesh_index: usize,
    pub destination: Destination,
    pub counts: GeometryCounts,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PlannedPart {
    pub selected_index: usize,
    pub mesh_index: usize,
    pub vertex_start: u32,
    pub index_start: u32,
    pub index_count: u32,
    pub face: u32,
}

#[derive(Debug, Default)]
pub(super) struct BufferPlan {
    pub counts: GeometryCounts,
    pub parts: Vec<PlannedPart>,
}

#[derive(Debug, Default)]
pub(super) struct GeometryPlan {
    pub buffers: BTreeMap<Destination, BufferPlan>,
    pub admitted: Vec<usize>,
    pub report: PropGeometryReport,
}

#[derive(Clone, Copy, Debug)]
struct ValidatedMesh {
    mesh_index: usize,
    counts: GeometryCounts,
}

fn validate_mesh(
    mesh_index: usize,
    mesh: &super::MeshData,
) -> Result<ValidatedMesh, GeometryError> {
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(GeometryError::NonTriangular {
            mesh: mesh_index,
            indices: mesh.indices.len(),
        });
    }
    for (offset, &index) in mesh.indices.iter().enumerate() {
        if index as usize >= mesh.vertices.len() {
            return Err(GeometryError::IndexOutOfBounds {
                mesh: mesh_index,
                offset,
                index,
                vertices: mesh.vertices.len(),
            });
        }
    }
    for (vertex_index, vertex) in mesh.vertices.iter().enumerate() {
        if !vertex.position.is_finite()
            || !vertex.normal.is_finite()
            || !vertex.tangent_s.is_finite()
            || !vertex.tangent_t.is_finite()
            || !vertex.tangent_sign.is_finite()
            || !vertex.uv.iter().all(|value| value.is_finite())
        {
            return Err(GeometryError::NonFiniteVertex {
                mesh: mesh_index,
                vertex: vertex_index,
            });
        }
    }
    Ok(ValidatedMesh {
        mesh_index,
        counts: GeometryCounts::default().append_buffer(GeometryCounts::checked(
            mesh.vertices.len(),
            mesh.indices.len(),
            1,
        )?)?,
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum GeometryRole {
    World,
    Door,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RawMesh {
    pub mesh_index: usize,
    pub material_slot: usize,
    pub counts: GeometryCounts,
}

#[derive(Debug)]
pub(super) struct RawPropGeometry {
    pub source: Arc<ModelData>,
    pub meshes: Vec<RawMesh>,
    counts: GeometryCounts,
}

impl RawPropGeometry {
    pub fn used_material_slots(&self) -> std::collections::BTreeSet<usize> {
        self.meshes.iter().map(|mesh| mesh.material_slot).collect()
    }

    pub fn counts(&self) -> GeometryCounts {
        self.counts
    }
}

#[derive(Debug)]
pub(super) struct DoorGeometryPlan {
    pub geometry: Arc<RawPropGeometry>,
    pub bounds: gmpublished_domain::scene::map::MapBounds,
}

struct CachedTopology {
    source: Arc<ModelData>,
    meshes: Result<Arc<[ValidatedMesh]>, GeometryError>,
}

#[derive(Default)]
pub(super) struct RawGeometryPlanner {
    models: HashMap<(*const ModelData, GeometryRole), CachedTopology>,
}

impl RawGeometryPlanner {
    fn prepare(
        &mut self,
        placement: &StaticPropPlacement,
        model: &Arc<ModelData>,
        role: GeometryRole,
        has_material: impl Fn(usize) -> bool,
    ) -> Result<Arc<RawPropGeometry>, GeometryError> {
        if !placement.origin.is_finite()
            || !placement.angles.is_finite()
            || !placement.scale.is_finite()
        {
            return Err(GeometryError::NonFinitePlacement);
        }
        let cached = self
            .models
            .entry((Arc::as_ptr(model), role))
            .or_insert_with(|| {
                let meshes = model
                    .meshes
                    .iter()
                    .enumerate()
                    .filter(|(_, mesh)| {
                        mesh.bodygroup_choice == 0
                            && (role == GeometryRole::World
                                || (!mesh.vertices.is_empty() && !mesh.indices.is_empty()))
                    })
                    .map(|(index, mesh)| validate_mesh(index, mesh))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Arc::from);
                CachedTopology {
                    source: Arc::clone(model),
                    meshes,
                }
            });
        debug_assert!(Arc::ptr_eq(&cached.source, model));
        let validated = cached.meshes.as_ref().map_err(Clone::clone)?;
        let skin = prop_skin_table(model, placement.skin);
        let meshes = validated
            .iter()
            .map(|mesh| {
                let source = &model.meshes[mesh.mesh_index];
                for (vertex_index, vertex) in source.vertices.iter().enumerate() {
                    if !transform_prop_position(vertex.position, placement).is_finite()
                        || !transform_prop_normal(vertex.normal, placement).is_finite()
                        || !transform_prop_normal(vertex.tangent_s, placement).is_finite()
                        || !transform_prop_normal(vertex.tangent_t, placement).is_finite()
                    {
                        return Err(GeometryError::NonFiniteTransform {
                            mesh: mesh.mesh_index,
                            vertex: vertex_index,
                        });
                    }
                }
                let slot = skin
                    .and_then(|skin| skin.get(source.material_index))
                    .map_or(source.material_index, |index| usize::from(*index));
                if !has_material(slot) {
                    return Err(GeometryError::MissingMaterial {
                        mesh: mesh.mesh_index,
                        slot,
                    });
                }
                Ok(RawMesh {
                    mesh_index: mesh.mesh_index,
                    material_slot: slot,
                    counts: mesh.counts,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let counts = meshes
            .iter()
            .try_fold(GeometryCounts::default(), |counts, mesh| {
                counts.checked_add(mesh.counts)
            })?;
        let geometry = Arc::new(RawPropGeometry {
            source: Arc::clone(model),
            meshes,
            counts,
        });
        Ok(geometry)
    }

    pub fn world(
        &mut self,
        placement: &StaticPropPlacement,
        model: &Arc<ModelData>,
        has_material: impl Fn(usize) -> bool,
    ) -> Result<Arc<RawPropGeometry>, GeometryError> {
        self.prepare(placement, model, GeometryRole::World, has_material)
    }

    pub fn door(
        &mut self,
        placement: &StaticPropPlacement,
        model: &Arc<ModelData>,
        has_material: impl Fn(usize) -> bool,
    ) -> Result<DoorGeometryPlan, GeometryError> {
        let geometry = self.prepare(placement, model, GeometryRole::Door, has_material)?;
        let mut positions = geometry.meshes.iter().flat_map(|mesh| {
            model.meshes[mesh.mesh_index]
                .vertices
                .iter()
                .map(|vertex| vertex.position)
        });
        let first = positions.next().ok_or(GeometryError::EmptyGeometry)?;
        let (mut min, mut max) = (first, first);
        for position in positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
        Ok(DoorGeometryPlan {
            geometry,
            bounds: gmpublished_domain::scene::map::MapBounds { min, max },
        })
    }
}

pub(super) fn bind_geometry(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    raw: &RawPropGeometry,
) -> Result<Vec<SourceMesh>, GeometryError> {
    debug_assert!(Arc::ptr_eq(&raw.source, &model.model));
    raw.meshes
        .iter()
        .map(|mesh| {
            let material_index = model
                .material_indices
                .get(&mesh.material_slot)
                .copied()
                .ok_or(GeometryError::MissingMaterial {
                    mesh: mesh.mesh_index,
                    slot: mesh.material_slot,
                })?;
            Ok(SourceMesh {
                mesh_index: mesh.mesh_index,
                counts: mesh.counts,
                destination: Destination {
                    material_index,
                    entity: placement.entity,
                },
            })
        })
        .collect()
}

#[cfg(test)]
pub(super) fn world_fixture(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
) -> Result<Arc<RawPropGeometry>, GeometryError> {
    RawGeometryPlanner::default().world(placement, &model.model, |slot| {
        model.material_indices.contains_key(&slot)
    })
}

#[cfg(test)]
pub(super) fn plan_door(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
) -> Result<Vec<SourceMesh>, GeometryError> {
    let plan = RawGeometryPlanner::default().door(placement, &model.model, |slot| {
        model.material_indices.contains_key(&slot)
    })?;
    bind_geometry(placement, model, &plan.geometry)
}

pub(super) fn plan(selected: &[SelectedPropPlacement<'_>]) -> GeometryPlan {
    plan_indices(selected, 0..selected.len())
}

fn plan_indices(
    selected: &[SelectedPropPlacement<'_>],
    indices: impl Iterator<Item = usize>,
) -> GeometryPlan {
    let mut plan = GeometryPlan::default();
    for selected_index in indices {
        let placement = &selected[selected_index];
        let meshes = placement
            .geometry
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|geometry| bind_geometry(placement.placement, &placement.model, geometry));
        let result = meshes.and_then(|meshes| append_placement(&mut plan, selected_index, &meshes));
        if let Err(reason) = result {
            plan.report.rejected.push(GeometryDiagnostic {
                placement_index: placement.source_index,
                reason,
            });
        }
    }
    plan.report.planned = plan.report.admitted;
    plan
}

fn append_placement(
    plan: &mut GeometryPlan,
    selected_index: usize,
    meshes: &[SourceMesh],
) -> Result<(), GeometryError> {
    let mut pending = BTreeMap::<Destination, GeometryCounts>::new();
    let mut parts = Vec::with_capacity(meshes.len());
    let mut total = plan.report.admitted;
    for mesh in meshes {
        let prior = pending
            .get(&mesh.destination)
            .copied()
            .or_else(|| {
                plan.buffers
                    .get(&mesh.destination)
                    .map(|buffer| buffer.counts)
            })
            .unwrap_or_default();
        let next = prior.append_buffer(mesh.counts)?;
        total = total.checked_add(mesh.counts)?;
        parts.push((
            mesh.destination,
            PlannedPart {
                selected_index,
                mesh_index: mesh.mesh_index,
                vertex_start: u32::try_from(prior.vertices).expect("checked vertex count"),
                index_start: u32::try_from(prior.indices).expect("checked index count"),
                index_count: u32::try_from(mesh.counts.indices).expect("checked index count"),
                face: u32::try_from(prior.mesh_ranges).expect("checked face count"),
            },
        ));
        pending.insert(mesh.destination, next);
    }
    for (destination, counts) in pending {
        plan.buffers.entry(destination).or_default().counts = counts;
    }
    for (destination, part) in parts {
        plan.buffers
            .get_mut(&destination)
            .expect("planned destination")
            .parts
            .push(part);
    }
    plan.report.admitted = total;
    plan.admitted.push(selected_index);
    Ok(())
}

impl GeometryPlan {
    pub fn reject_failed_destinations(
        self,
        selected: &[SelectedPropPlacement<'_>],
        failed: &BTreeMap<Destination, GeometryError>,
    ) -> Self {
        if failed.is_empty() {
            return self;
        }
        let mut rejected = BTreeMap::new();
        for (destination, reason) in failed {
            for part in &self.buffers[destination].parts {
                rejected
                    .entry(part.selected_index)
                    .or_insert_with(|| reason.clone());
            }
        }
        let mut surviving = plan_indices(
            selected,
            self.admitted
                .iter()
                .copied()
                .filter(|index| !rejected.contains_key(index)),
        );
        assert!(
            surviving.report.rejected.is_empty(),
            "validated survivor geometry changed"
        );
        surviving.report.planned = self.report.planned;
        surviving.report.rejected = self.report.rejected;
        surviving
            .report
            .rejected
            .extend(
                rejected
                    .into_iter()
                    .map(|(index, reason)| GeometryDiagnostic {
                        placement_index: selected[index].source_index,
                        reason,
                    }),
            );
        surviving
            .report
            .rejected
            .sort_by_key(|issue| issue.placement_index);
        surviving
    }
}

#[derive(Debug)]
pub(super) struct BakedGeometry {
    pub(super) meshes: BTreeMap<(usize, bool), PropBuildMesh>,
    pub(super) geometry: PropGeometryReport,
    pub(super) admitted: Vec<usize>,
}

pub(super) fn bake_selected_prop_placements_parallel(
    selected: &[SelectedPropPlacement<'_>],
) -> BakedGeometry {
    if rayon::current_num_threads() <= 1 {
        bake_selected_prop_placements_serial(selected)
    } else {
        bake_geometry(selected, true, reserve_geometry_mesh)
    }
}

pub(super) fn bake_selected_prop_placements_serial(
    selected: &[SelectedPropPlacement<'_>],
) -> BakedGeometry {
    bake_geometry(selected, false, reserve_geometry_mesh)
}

fn reserve_geometry_mesh(
    destination: Destination,
    counts: GeometryCounts,
) -> Result<PropBuildMesh, GeometryError> {
    let mut mesh = PropBuildMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material_index: destination.material_index,
        visibility: PropBuildMeshVisibility::default(),
        renderable_ranges: Vec::new(),
    };
    let failure =
        |component, bytes, error: std::collections::TryReserveError| GeometryError::Allocation {
            material_index: destination.material_index,
            entity: destination.entity,
            component,
            bytes,
            detail: error.to_string(),
        };
    mesh.vertices
        .try_reserve_exact(counts.vertices)
        .map_err(|error| failure(GeometryComponent::Vertices, counts.vertex_bytes, error))?;
    mesh.indices
        .try_reserve_exact(counts.indices)
        .map_err(|error| failure(GeometryComponent::Indices, counts.index_bytes, error))?;
    let range_bytes = counts
        .mesh_ranges
        .checked_mul(size_of::<PropBuildRenderableRange>())
        .ok_or(GeometryError::Overflow {
            component: GeometryComponent::MeshRanges,
        })?;
    mesh.renderable_ranges
        .try_reserve_exact(counts.mesh_ranges)
        .map_err(|error| failure(GeometryComponent::MeshRanges, range_bytes, error))?;
    Ok(mesh)
}

fn bake_geometry(
    selected: &[SelectedPropPlacement<'_>],
    parallel: bool,
    mut reserve: impl FnMut(Destination, GeometryCounts) -> Result<PropBuildMesh, GeometryError>,
) -> BakedGeometry {
    let initial = plan(selected);
    let mut allocated = BTreeMap::new();
    let mut failed = BTreeMap::new();
    for (&destination, buffer) in &initial.buffers {
        match reserve(destination, buffer.counts) {
            Ok(mesh) => {
                allocated.insert(destination, mesh);
            }
            Err(reason) => {
                failed.insert(destination, reason);
            }
        }
    }
    let plan = initial.reject_failed_destinations(selected, &failed);
    allocated.retain(|destination, _| plan.buffers.contains_key(destination));
    let inputs = plan
        .buffers
        .into_iter()
        .map(|(destination, buffer)| {
            let mesh = allocated
                .remove(&destination)
                .expect("reserved surviving destination");
            (destination, buffer, mesh)
        })
        .collect::<Vec<_>>();
    let bake = |(destination, buffer, mut target): (Destination, BufferPlan, PropBuildMesh)| {
        for part in buffer.parts {
            let placement = &selected[part.selected_index];
            let source = &placement
                .geometry
                .as_ref()
                .expect("admitted raw geometry")
                .source
                .meshes[part.mesh_index];
            assert_eq!(target.vertices.len(), part.vertex_start as usize);
            assert_eq!(target.indices.len(), part.index_start as usize);
            target
                .vertices
                .extend(
                    source
                        .vertices
                        .iter()
                        .enumerate()
                        .map(|(vertex_index, vertex)| {
                            bake_world_vertex(placement, part.mesh_index, vertex_index, vertex)
                        }),
                );
            target.indices.extend(
                source
                    .indices
                    .iter()
                    .map(|&index| part.vertex_start + index),
            );
            target.visibility.push(
                &placement.placement.visibility,
                MapMeshIndexRange {
                    face: part.face,
                    start: part.index_start,
                    count: part.index_count,
                },
            );
            target.renderable_ranges.push(PropBuildRenderableRange {
                source_index: placement.source_index,
                draw_order: part.mesh_index,
                start: part.index_start,
                count: part.index_count,
            });
        }
        assert_eq!(target.vertices.len(), buffer.counts.vertices);
        assert_eq!(target.indices.len(), buffer.counts.indices);
        assert_eq!(target.renderable_ranges.len(), buffer.counts.mesh_ranges);
        ((destination.material_index, destination.entity), target)
    };
    let meshes = if parallel {
        inputs.into_par_iter().map(bake).collect()
    } else {
        inputs.into_iter().map(bake).collect()
    };
    BakedGeometry {
        meshes,
        geometry: plan.report,
        admitted: plan.admitted,
    }
}

#[cfg(test)]
mod tests;
