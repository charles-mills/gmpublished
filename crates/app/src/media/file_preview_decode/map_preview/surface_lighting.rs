use crate::bridge::materials::{RenderMode, ResolvedLightmapLayoutMaterial};
use crate::media::preview_model::{
    MaterialSlot, MeshData, ModelVertex, OverlayPrimitive, vertex_lighting,
};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapMesh, MapRadiositySurfaces, MapSurfaceMarkSource, MapSurfaceOwner, MapVisibility,
    MapWorldSurfaceRendering, SourceLightmapLayout, SourceLightmapLayoutError,
    SourceLightmapMaterial, SourceLightmapPageProfile, SourceLightmapRequirement,
    SourceLightmapSurface,
};

pub(in super::super) struct PreparedSurfaceLighting {
    pub layout: Result<SourceLightmapLayout, SourceLightmapLayoutError>,
    faces: Vec<ReceiverContract>,
}

#[derive(Clone, Copy)]
struct ReceiverContract {
    requirement: SourceLightmapRequirement,
    white_page: bool,
    missing_samples: bool,
}

impl ReceiverContract {
    fn bind(self, vertex: &mut ModelVertex, mark: bool) {
        vertex.lighting_mode &= !vertex_lighting::DIRECTIONAL_LIGHTMAP_BLACK;
        let sampled = vertex.lighting_mode
            & (vertex_lighting::LIGHTMAP | vertex_lighting::RECEIVER_LIGHTMAP)
            != 0;
        let constant = if self.white_page {
            Some(Vec3::splat(1.0))
        } else if mark && self.requirement == SourceLightmapRequirement::None {
            Some(Vec3::ZERO)
        } else if self.missing_samples || !sampled {
            Some(Vec3::splat(1.0))
        } else {
            None
        };
        if let Some(illumination) = constant {
            vertex.lighting_mode &= !(vertex_lighting::LIGHTMAP
                | vertex_lighting::RECEIVER_LIGHTMAP
                | vertex_lighting::WORLD_CONSTANT
                | vertex_lighting::RECEIVER_CONSTANT
                | vertex_lighting::PREVIEW);
            vertex.lighting_mode |= if mark {
                vertex_lighting::RECEIVER_CONSTANT
            } else {
                vertex_lighting::WORLD_CONSTANT
            };
            vertex.illumination = illumination;
            vertex.lightmap_bump_step = 0.0;
            vertex.lightmap_page = 0;
            if self.missing_samples
                && !self.white_page
                && self.requirement == SourceLightmapRequirement::Bumped
            {
                vertex.lighting_mode |= vertex_lighting::DIRECTIONAL_LIGHTMAP_BLACK;
            }
        } else if self.requirement != SourceLightmapRequirement::Bumped {
            vertex.lightmap_bump_step = 0.0;
        }
    }

    fn bind_mark(
        self,
        mark: &mut OverlayPrimitive,
        receiver_mode: Option<RenderMode>,
        rendering: Option<MapWorldSurfaceRendering>,
    ) {
        let immediate = mark.owner == MapSurfaceOwner::World
            && rendering.is_some_and(|surface| surface.displacement.is_none())
            && matches!(
                receiver_mode,
                Some(RenderMode::Translucent | RenderMode::Additive | RenderMode::Modulate)
            );
        let batched = matches!(mark.source, MapSurfaceMarkSource::Decal { .. }) && !immediate;
        for vertex in &mut mark.vertices {
            vertex.lighting_mode &= !vertex_lighting::BATCHED_STATIC_DECAL;
            if batched {
                vertex.lighting_mode |= vertex_lighting::BATCHED_STATIC_DECAL;
            }
            self.bind(vertex, true);
        }
    }
}

impl PreparedSurfaceLighting {
    pub(in super::super) fn bind_marks(
        &self,
        marks: &mut [OverlayPrimitive],
        materials: &[MaterialSlot],
        visibility: Option<&MapVisibility>,
    ) {
        for mark in marks {
            let face = mark.receiver_range.face as usize;
            let Some(&contract) = self.faces.get(face) else {
                log::warn!(
                    "surface mark {:?}: receiver face {face} has no lighting contract",
                    mark.source
                );
                continue;
            };
            let mode = materials
                .get(mark.receiver_material_index)
                .map(|slot| slot.render_mode);
            let rendering =
                visibility.and_then(|vis| vis.surface_rendering(mark.receiver_range.face));
            if mode.is_none() || (mark.owner == MapSurfaceOwner::World && rendering.is_none()) {
                log::warn!(
                    "surface mark {:?}: receiver face {face} has incomplete draw metadata",
                    mark.source
                );
            }
            contract.bind_mark(mark, mode, rendering);
        }
    }

    pub(in super::super) fn bind_mesh(&self, raw: &MapMesh, gpu: &mut MeshData) {
        if raw.vertices.len() != gpu.vertices.len() {
            log::warn!("map mesh: lighting contract vertex count differs from prepared geometry");
            return;
        }
        let mut visited = vec![false; gpu.vertices.len()];
        let ranges = raw.visibility.always_visible.iter().chain(
            raw.visibility
                .clusters
                .iter()
                .flat_map(|cluster| &cluster.ranges),
        );
        for range in ranges {
            let face = range.face as usize;
            let Some(&contract) = self.faces.get(face) else {
                log::warn!("map mesh: face {face} has no lighting contract");
                continue;
            };
            let Some(end) = range.start.checked_add(range.count) else {
                log::warn!("map mesh: face {face} lighting index range overflows");
                continue;
            };
            let Some(indices) = raw.indices.get(range.start as usize..end as usize) else {
                log::warn!("map mesh: face {face} lighting index range exceeds geometry");
                continue;
            };
            for &index in indices {
                let index = index as usize;
                let Some(vertex) = gpu.vertices.get_mut(index) else {
                    log::warn!("map mesh: face {face} references invalid lighting vertex {index}");
                    continue;
                };
                if !visited[index] {
                    contract.bind(vertex, false);
                    visited[index] = true;
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum LayoutError {
    #[error("compiled surface metadata is unavailable")]
    MissingSurfaces,
    #[error("expected {expected} effective materials, received {actual}")]
    MaterialCount { expected: usize, actual: usize },
    #[error("face {face} has invalid texture info {texinfo}")]
    InvalidTextureInfo { face: usize, texinfo: i16 },
    #[error("face {face} has invalid texture data {texdata}")]
    InvalidTextureData { face: usize, texdata: i32 },
}

pub(super) fn prepare(
    surfaces: Option<&MapRadiositySurfaces>,
    materials: &[ResolvedLightmapLayoutMaterial],
) -> Result<PreparedSurfaceLighting, LayoutError> {
    let surfaces = surfaces.ok_or(LayoutError::MissingSurfaces)?;
    if materials.len() != surfaces.texdatas.len() {
        return Err(LayoutError::MaterialCount {
            expected: surfaces.texdatas.len(),
            actual: materials.len(),
        });
    }
    let inputs = surfaces
        .faces
        .iter()
        .enumerate()
        .map(|(face, input)| {
            let texinfo = usize::try_from(input.texinfo)
                .ok()
                .and_then(|index| surfaces.texinfos.get(index))
                .ok_or(LayoutError::InvalidTextureInfo {
                    face,
                    texinfo: input.texinfo,
                })?;
            let material = usize::try_from(texinfo.texdata)
                .ok()
                .filter(|&index| index < materials.len())
                .ok_or(LayoutError::InvalidTextureData {
                    face,
                    texdata: texinfo.texdata,
                })?;
            Ok(SourceLightmapSurface {
                material,
                extents: input.lightmap_size,
                no_light: texinfo.flags & vformats::bsp::texture_flags::NOLIGHT != 0,
                styles: input.styles,
            })
        })
        .collect::<Result<Vec<_>, LayoutError>>()?;
    let contracts = materials
        .iter()
        .map(|material| SourceLightmapMaterial {
            canonical_name: &material.canonical_name,
            requirement: material.requirement,
        })
        .collect::<Vec<_>>();
    let layout =
        SourceLightmapLayout::allocate(&inputs, &contracts, SourceLightmapPageProfile::DX9);
    let faces = inputs
        .iter()
        .zip(&surfaces.faces)
        .map(|(surface, face)| ReceiverContract {
            requirement: materials[surface.material].requirement,
            white_page: surface.no_light || surface.extents.contains(&0),
            missing_samples: face.light_offset == -1 || surfaces.lighting_source().is_none(),
        })
        .collect();
    Ok(PreparedSurfaceLighting { layout, faces })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use gmpublished_domain::scene::map::{
        MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility, MapPropVisibility,
        MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceOwner, MapVertex,
    };

    fn vertex(sampled: bool, step: f32, mark: bool) -> ModelVertex {
        let mut vertex = ModelVertex::zeroed();
        vertex.lighting_mode = match (sampled, mark) {
            (true, false) => vertex_lighting::LIGHTMAP,
            (true, true) => vertex_lighting::RECEIVER_LIGHTMAP,
            (false, false) => vertex_lighting::WORLD_CONSTANT,
            (false, true) => vertex_lighting::RECEIVER_CONSTANT,
        };
        vertex.illumination = Vec3::splat(0.3);
        vertex.lightmap_uv = [0.25, 0.75];
        vertex.lightmap_bump_step = step;
        vertex.lightmap_page = u32::from(sampled);
        vertex
    }

    fn prepared(faces: Vec<ReceiverContract>) -> PreparedSurfaceLighting {
        PreparedSurfaceLighting {
            layout: Err(SourceLightmapLayoutError::SurfaceTooLarge {
                face: 0,
                width: 1024,
                height: 8,
            }),
            faces,
        }
    }

    fn mark(face: u32, vertex: ModelVertex) -> OverlayPrimitive {
        OverlayPrimitive {
            source: MapSurfaceMarkSource::Overlay { index: 0, id: 0 },
            vertices: vec![vertex],
            indices: vec![0],
            material_index: 0,
            visibility: MapPropVisibility::Always,
            owner: MapSurfaceOwner::World,
            receiver_mesh: MapReceiverMesh::World(0),
            receiver_range: MapMeshIndexRange {
                face,
                start: 0,
                count: 1,
            },
            receiver_material_index: 0,
            render_order: 0,
            fade: None,
            fade_origin: Vec3::ZERO,
            decal_sort: None,
        }
    }

    #[test]
    fn static_fullbright_binding_follows_receiver_draw_path() {
        use gmpublished_domain::scene::map::{
            MapDecalPermanence, MapDisplacementRendering, MapWorldMaterialGroup, ModelIndex,
        };
        let planar = MapWorldSurfaceRendering {
            material_group: MapWorldMaterialGroup::AboveWater,
            displacement: None,
        };
        let displacement = MapWorldSurfaceRendering {
            displacement: Some(MapDisplacementRendering {
                ordinal: 0,
                full_grid_vertices: 25,
                full_grid_indices: 96,
            }),
            ..planar
        };
        let brush = MapSurfaceOwner::Brush {
            entity_index: 4,
            model_index: ModelIndex::new(1),
            origin: Vec3::ZERO,
            angles: Vec3::ZERO,
        };
        let door = MapSurfaceOwner::Door {
            door_index: 2,
            model_index: ModelIndex::new(1),
        };
        let contract = ReceiverContract {
            requirement: SourceLightmapRequirement::Lightmapped,
            white_page: false,
            missing_samples: false,
        };
        for (owner, mode, rendering, expected_batched) in [
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Opaque),
                Some(planar),
                true,
            ),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Cutout),
                Some(planar),
                true,
            ),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Translucent),
                Some(planar),
                false,
            ),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Additive),
                Some(planar),
                false,
            ),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Modulate),
                Some(planar),
                false,
            ),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Translucent),
                Some(displacement),
                true,
            ),
            (brush, Some(RenderMode::Translucent), None, true),
            (door, Some(RenderMode::Translucent), None, true),
            (
                MapSurfaceOwner::World,
                Some(RenderMode::Translucent),
                None,
                true,
            ),
            (MapSurfaceOwner::World, None, Some(planar), true),
        ] {
            for permanence in [
                MapDecalPermanence::Permanent,
                MapDecalPermanence::NonPermanent,
            ] {
                let mut mark = mark(0, vertex(true, 0.0, true));
                mark.source = MapSurfaceMarkSource::Decal {
                    entity_index: 8,
                    permanence,
                };
                mark.owner = owner;
                contract.bind_mark(&mut mark, mode, rendering);
                assert_eq!(
                    mark.vertices[0].lighting_mode,
                    vertex_lighting::RECEIVER_LIGHTMAP
                        | if expected_batched {
                            vertex_lighting::BATCHED_STATIC_DECAL
                        } else {
                            0
                        },
                    "{owner:?} {mode:?} {rendering:?} {permanence:?}"
                );
                assert_eq!(mark.vertices[0].illumination, Vec3::splat(0.3));
                assert_eq!(mark.vertices[0].lightmap_uv, [0.25, 0.75]);
                mark.source = MapSurfaceMarkSource::Overlay { index: 7, id: 9 };
                contract.bind_mark(&mut mark, mode, rendering);
                assert_eq!(
                    mark.vertices[0].lighting_mode,
                    vertex_lighting::RECEIVER_LIGHTMAP
                );
            }
        }
    }

    #[test]
    fn sampled_scalar_and_directional_bindings_follow_effective_receiver_material() {
        for mark in [false, true] {
            for (requirement, input_step, expected_step) in [
                (SourceLightmapRequirement::Lightmapped, 0.0, 0.0),
                (SourceLightmapRequirement::Lightmapped, 0.125, 0.0),
                (SourceLightmapRequirement::Bumped, 0.125, 0.125),
            ] {
                let mut vertex = vertex(true, input_step, mark);
                ReceiverContract {
                    requirement,
                    white_page: false,
                    missing_samples: false,
                }
                .bind(&mut vertex, mark);
                let expected_mode = if mark {
                    vertex_lighting::RECEIVER_LIGHTMAP
                } else {
                    vertex_lighting::LIGHTMAP
                };
                assert_eq!(vertex.lighting_mode, expected_mode);
                assert_eq!(vertex.lightmap_bump_step, expected_step);
                assert_eq!(vertex.lightmap_page, 1);
                assert_eq!(vertex.illumination, Vec3::splat(0.3));
                assert_eq!(vertex.lightmap_uv, [0.25, 0.75]);
            }
        }
    }

    #[test]
    fn white_pages_override_baked_samples_and_effective_material_requirement() {
        for requirement in [
            SourceLightmapRequirement::None,
            SourceLightmapRequirement::Lightmapped,
            SourceLightmapRequirement::Bumped,
        ] {
            for mark in [false, true] {
                let mut vertex = vertex(true, 0.125, mark);
                ReceiverContract {
                    requirement,
                    white_page: true,
                    missing_samples: false,
                }
                .bind(&mut vertex, mark);
                assert_eq!(vertex.illumination, Vec3::splat(1.0));
                assert_eq!(vertex.lightmap_bump_step, 0.0);
                assert_eq!(vertex.lightmap_page, 0);
                assert_eq!(
                    vertex.lighting_mode,
                    if mark {
                        vertex_lighting::RECEIVER_CONSTANT
                    } else {
                        vertex_lighting::WORLD_CONSTANT
                    }
                );
            }
        }
    }

    #[test]
    fn constant_bindings_distinguish_missing_samples_and_unlightmapped_receivers() {
        for requirement in [
            SourceLightmapRequirement::Lightmapped,
            SourceLightmapRequirement::Bumped,
        ] {
            for mark in [false, true] {
                let mut vertex = vertex(false, 0.0, mark);
                ReceiverContract {
                    requirement,
                    white_page: false,
                    missing_samples: false,
                }
                .bind(&mut vertex, mark);
                assert_eq!(vertex.illumination, Vec3::splat(1.0));
                assert_eq!(
                    vertex.lighting_mode,
                    if mark {
                        vertex_lighting::RECEIVER_CONSTANT
                    } else {
                        vertex_lighting::WORLD_CONSTANT
                    }
                );
            }
        }
        let contract = ReceiverContract {
            requirement: SourceLightmapRequirement::None,
            white_page: false,
            missing_samples: false,
        };
        let mut mark = vertex(true, 0.125, true);
        contract.bind(&mut mark, true);
        assert_eq!(mark.illumination, Vec3::ZERO);
        assert_eq!(mark.lighting_mode, vertex_lighting::RECEIVER_CONSTANT);
        assert_eq!(mark.lightmap_bump_step, 0.0);
        let mut world = vertex(true, 0.125, false);
        contract.bind(&mut world, false);
        assert_eq!(world.lighting_mode, vertex_lighting::LIGHTMAP);
        assert_eq!(world.illumination, Vec3::splat(0.3));
        assert_eq!(world.lightmap_bump_step, 0.0);
    }

    #[test]
    fn missing_physical_samples_keep_white_scalar_and_black_directional_lighting() {
        for mark in [false, true] {
            for white_page in [false, true] {
                let mut vertex = vertex(true, 0.125, mark);
                vertex.lighting_mode |= vertex_lighting::BATCHED_STATIC_DECAL;
                ReceiverContract {
                    requirement: SourceLightmapRequirement::Bumped,
                    white_page,
                    missing_samples: true,
                }
                .bind(&mut vertex, mark);
                let expected = if mark {
                    vertex_lighting::RECEIVER_CONSTANT
                } else {
                    vertex_lighting::WORLD_CONSTANT
                } | vertex_lighting::BATCHED_STATIC_DECAL
                    | if white_page {
                        0
                    } else {
                        vertex_lighting::DIRECTIONAL_LIGHTMAP_BLACK
                    };
                assert_eq!(vertex.lighting_mode, expected);
                assert_eq!(vertex.lightmap_page, 0);
                assert_eq!(vertex.illumination, Vec3::splat(1.0));
                assert_eq!(vertex.lightmap_bump_step, 0.0);
            }
        }
    }

    #[test]
    fn marks_bind_without_successful_logical_page_allocation() {
        let prepared = prepared(vec![
            ReceiverContract {
                requirement: SourceLightmapRequirement::Lightmapped,
                white_page: false,
                missing_samples: false,
            },
            ReceiverContract {
                requirement: SourceLightmapRequirement::Bumped,
                white_page: false,
                missing_samples: false,
            },
            ReceiverContract {
                requirement: SourceLightmapRequirement::None,
                white_page: true,
                missing_samples: false,
            },
        ]);
        let mut marks = vec![
            mark(0, vertex(true, 0.125, true)),
            mark(1, vertex(true, 0.125, true)),
            mark(2, vertex(true, 0.125, true)),
        ];
        prepared.bind_marks(&mut marks, &[], None);
        assert!(prepared.layout.is_err());
        assert_eq!(marks[0].vertices[0].lightmap_bump_step, 0.0);
        assert_eq!(
            marks[1].vertices[0].lighting_mode,
            vertex_lighting::RECEIVER_LIGHTMAP
        );
        assert_eq!(
            marks[2].vertices[0].lighting_mode,
            vertex_lighting::RECEIVER_CONSTANT
        );
        assert_eq!(marks[2].vertices[0].illumination, Vec3::splat(1.0));
    }

    #[test]
    fn mesh_binding_visits_all_faces_despite_repeated_cluster_ranges() {
        let prepared = prepared(vec![
            ReceiverContract {
                requirement: SourceLightmapRequirement::Lightmapped,
                white_page: false,
                missing_samples: false,
            },
            ReceiverContract {
                requirement: SourceLightmapRequirement::Bumped,
                white_page: false,
                missing_samples: false,
            },
            ReceiverContract {
                requirement: SourceLightmapRequirement::Bumped,
                white_page: true,
                missing_samples: false,
            },
        ]);
        let raw_vertex = MapVertex {
            position: Vec3::ZERO,
            base_position: Vec3::ZERO,
            normal: Vec3::new(0.0, 0.0, 1.0),
            tangent_s: Vec3::new(1.0, 0.0, 0.0),
            tangent_t: Vec3::new(0.0, 1.0, 0.0),
            tex_s: 0.0,
            tex_t: 0.0,
            texture_size: [64; 2],
            lightmap_uv: [0.25, 0.75],
            lightmap_page: Some(gmpublished_domain::scene::map::LightmapPageId(0)),
            lightmap_bump_step: 0.125,
            blend_alpha: 0.0,
            opacity: 1.0,
        };
        let first = MapMeshIndexRange {
            face: 0,
            start: 0,
            count: 3,
        };
        let second = MapMeshIndexRange {
            face: 1,
            start: 3,
            count: 3,
        };
        let third = MapMeshIndexRange {
            face: 2,
            start: 6,
            count: 3,
        };
        let raw = MapMesh {
            entity: false,
            vertices: vec![raw_vertex; 3],
            indices: vec![0, 0, 0, 1, 1, 1, 2, 2, 2],
            material_index: 0,
            visibility: MapMeshVisibility {
                always_visible: vec![first],
                clusters: vec![
                    MapMeshClusterRanges {
                        cluster: 4,
                        ranges: vec![first, second],
                    },
                    MapMeshClusterRanges {
                        cluster: 17,
                        ranges: vec![second, third],
                    },
                ],
            },
            surface_owners: Vec::new(),
        };
        let mut gpu = super::super::world_geometry::map_mesh_to_model_mesh(&raw, &[]);
        prepared.bind_mesh(&raw, &mut gpu);
        assert!(prepared.layout.is_err());
        assert_eq!(gpu.vertices[0].lightmap_bump_step, 0.0);
        assert_eq!(gpu.vertices[0].lighting_mode, vertex_lighting::LIGHTMAP);
        assert_eq!(gpu.vertices[1].lighting_mode, vertex_lighting::LIGHTMAP);
        assert_eq!(gpu.vertices[1].lightmap_bump_step, 0.125);
        assert_eq!(
            gpu.vertices[2].lighting_mode,
            vertex_lighting::WORLD_CONSTANT
        );
        assert_eq!(gpu.vertices[2].illumination, Vec3::splat(1.0));
    }
}
