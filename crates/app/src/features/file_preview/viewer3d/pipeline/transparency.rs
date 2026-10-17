use super::geometry::ViewerScene;
use super::reflections::SceneView;
use super::uniforms::Uniforms;
use super::{DrawItem, DrawPlan, OverlayDrawItem, RenderMode, Vec3};
use gmpublished_domain::scene::map::MapMeshVisibility;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

mod details;
mod order;
mod schedule;
pub use schedule::Diagnostics as TransparencyDiagnostics;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldDrawGroups {
    pub water_side: Option<WaterSide>,
    pub include_water: bool,
}

impl WorldDrawGroups {
    pub const ALL: Self = Self {
        water_side: None,
        include_water: true,
    };

    pub fn allows(self, group: gmpublished_domain::scene::map::MapWorldMaterialGroup) -> bool {
        if group == gmpublished_domain::scene::map::MapWorldMaterialGroup::Water {
            self.include_water
        } else {
            self.water_side.is_none_or(|side| side.allows(group))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaterSide {
    Above,
    Under,
}

impl WaterSide {
    pub fn from_submerged(submerged: bool) -> Self {
        if submerged { Self::Under } else { Self::Above }
    }

    pub(super) fn allows(
        self,
        group: gmpublished_domain::scene::map::MapWorldMaterialGroup,
    ) -> bool {
        use gmpublished_domain::scene::map::MapWorldMaterialGroup;
        matches!(
            (self, group),
            (_, MapWorldMaterialGroup::IntersectsWater)
                | (Self::Above, MapWorldMaterialGroup::AboveWater)
                | (Self::Under, MapWorldMaterialGroup::Underwater)
        )
    }
}

#[derive(Debug)]
pub struct Part {
    pub indices: Range<u32>,
    pub centre: Vec3,
    /// None means always visible; one logical part may occur in several leaves.
    pub clusters: Option<Arc<[u32]>>,
}

#[derive(Clone, Debug)]
pub enum Primitive {
    Mesh(DrawItem, Option<Range<u32>>),
    Overlay(OverlayDrawItem),
    Sprite(Range<u32>),
}
#[derive(Clone, Debug)]
pub struct Draw {
    pub primitive: Primitive,
    pub depth: f32,
    pub blend: RenderMode,
}

struct DrawGroup {
    depth: f32,
    operations: Range<usize>,
}

pub fn parts(
    vertices: &[super::ModelVertex],
    indices: &[u32],
    visibility: &MapMeshVisibility,
) -> Vec<Part> {
    let mut ranges = BTreeMap::<u32, (Range<u32>, Option<Vec<u32>>)>::new();
    for range in &visibility.always_visible {
        ranges.insert(
            range.face,
            (range.start..range.start.saturating_add(range.count), None),
        );
    }
    for cluster in &visibility.clusters {
        for range in &cluster.ranges {
            let entry = ranges.entry(range.face).or_insert_with(|| {
                (
                    range.start..range.start.saturating_add(range.count),
                    Some(Vec::new()),
                )
            });
            if let Some(clusters) = entry.1.as_mut() {
                clusters.push(cluster.cluster);
            }
        }
    }
    let mut parts = Vec::new();
    for (range, clusters) in ranges.into_values() {
        let Some(selected) = indices.get(range.start as usize..range.end as usize) else {
            continue;
        };
        let clusters = clusters.map(Arc::<[u32]>::from);
        for (offset, triangle) in selected.chunks_exact(3).enumerate() {
            let Some(a) = vertices.get(triangle[0] as usize) else {
                continue;
            };
            let Some(b) = vertices.get(triangle[1] as usize) else {
                continue;
            };
            let Some(c) = vertices.get(triangle[2] as usize) else {
                continue;
            };
            let start = range.start + offset as u32 * 3;
            parts.push(Part {
                indices: start..start + 3,
                centre: (a.position + b.position + c.position) / 3.0,
                clusters: clusters.clone(),
            });
        }
    }
    parts
}

fn skybox_parts(vertices: &[super::ModelVertex], indices: &[u32]) -> Vec<Part> {
    parts(
        vertices,
        indices,
        &MapMeshVisibility {
            always_visible: vec![gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 0,
                start: 0,
                count: indices.len() as u32,
            }],
            clusters: Vec::new(),
        },
    )
}

pub fn build_parts(
    meshes: &mut [super::upload::UploadedMesh],
    material_render_modes: &[super::RenderMode],
    scene: &super::RenderScene,
    map: Option<&super::MapPreview>,
) {
    let Some(map) = map else {
        return;
    };
    for mesh in meshes {
        if mesh.door_index.is_some() {
            continue;
        }
        if !matches!(
            material_render_modes.get(mesh.material_index),
            Some(RenderMode::Translucent | RenderMode::Additive | RenderMode::Modulate)
        ) {
            continue;
        }
        let source = if mesh.map_skybox {
            map.map_skybox_meshes.get(mesh.scene_mesh_index)
        } else {
            scene.meshes.get(mesh.scene_mesh_index)
        };
        let Some(source) = source else {
            continue;
        };
        if mesh.map_skybox {
            mesh.transparent_parts = skybox_parts(&source.vertices, &source.indices);
        } else if let Some(visibility) = map.mesh_visibility.get(mesh.scene_mesh_index) {
            mesh.transparent_parts = parts(&source.vertices, &source.indices, visibility);
        }
    }
}

fn depth(uniforms: &Uniforms, position: Vec3) -> f32 {
    (0..3)
        .map(|axis| uniforms.view_proj[axis][3] * position[axis])
        .sum::<f32>()
        + uniforms.view_proj[3][3]
}

fn owner_depth(uniforms: &Uniforms, origin: Vec3, local_center: Vec3) -> f32 {
    let eye = Vec3::new(
        uniforms.camera_position[0],
        uniforms.camera_position[1],
        uniforms.camera_position[2],
    );
    let delta = (origin - eye) + local_center;
    let terms: [f32; 3] = std::array::from_fn(|axis| uniforms.view_proj[axis][3] * delta[axis]);
    (terms[1] + terms[0]) + terms[2]
}

pub fn prepare(
    view: &mut SceneView,
    upload: &ViewerScene,
    world: &Uniforms,
    skybox: Option<&Uniforms>,
    water_side: Option<WaterSide>,
    include_water: bool,
) {
    view.world_groups = WorldDrawGroups {
        water_side,
        include_water,
    };
    let Some(plans) = view.plans.as_ref() else {
        view.queue_cache = super::view_preparation::QueueCache::default();
        view.transparency.clear();
        view.skybox_transparency.clear();
        view.transparency_diagnostics = TransparencyDiagnostics::default();
        view.skybox_transparency_diagnostics = TransparencyDiagnostics::default();
        return;
    };
    if !view.queue_cache.update(
        upload,
        plans,
        super::view_preparation::QueueInputs {
            world,
            skybox,
            groups: view.world_groups,
            visibility_revisions: (view.visibility.revision, view.skybox_visibility.revision),
        },
    ) {
        return;
    }
    (view.transparency, view.transparency_diagnostics) = queue(
        upload,
        &plans.world,
        world,
        &view.visibility,
        false,
        view.world_groups,
    );
    (
        view.skybox_transparency,
        view.skybox_transparency_diagnostics,
    ) = plans.map_skybox.as_ref().zip(skybox).map_or_else(
        || (Vec::new(), TransparencyDiagnostics::default()),
        |(plan, uniforms)| {
            queue(
                upload,
                plan,
                uniforms,
                &view.skybox_visibility,
                true,
                WorldDrawGroups::ALL,
            )
        },
    );
}

fn queue(
    upload: &ViewerScene,
    plan: &DrawPlan,
    uniforms: &Uniforms,
    visibility: &super::visibility::UploadedVisibility,
    skybox: bool,
    groups: WorldDrawGroups,
) -> (Vec<Draw>, TransparencyDiagnostics) {
    let details = details::prepare(upload, uniforms, visibility, skybox, groups.water_side);
    let schedule = schedule::Schedule::prepare(upload, plan, uniforms, skybox, groups, &details);
    let draws = remaining_queue(
        upload,
        plan,
        uniforms,
        visibility,
        &details,
        schedule.as_ref().ok(),
    );
    match schedule {
        Ok(schedule) => {
            let diagnostics = schedule.diagnostics.clone();
            (schedule.compose(draws), diagnostics)
        }
        Err(unavailable) => (
            draws,
            TransparencyDiagnostics {
                unavailable: Some(unavailable),
                ..Default::default()
            },
        ),
    }
}

fn remaining_queue(
    upload: &ViewerScene,
    plan: &DrawPlan,
    uniforms: &Uniforms,
    visibility: &super::visibility::UploadedVisibility,
    details: &[details::DetailDraw],
    schedule: Option<&schedule::Schedule>,
) -> Vec<Draw> {
    let mut draws = Vec::new();
    for items in [&plan.translucent, &plan.additive] {
        for item in items {
            if schedule.is_some_and(|schedule| schedule.covers(item.mesh_index, None)) {
                continue;
            }
            let mesh = &upload.meshes[item.mesh_index];
            if mesh.transparent_parts.is_empty() {
                draws.push(Draw {
                    primitive: Primitive::Mesh(*item, None),
                    depth: depth(uniforms, mesh.centroid()),
                    blend: upload.assets.material_render_modes[item.material_slot],
                });
            } else {
                for part in &mesh.transparent_parts {
                    if schedule.is_some_and(|schedule| {
                        schedule.covers(item.mesh_index, Some(&part.indices))
                    }) {
                        continue;
                    }
                    if let Some(plan) = visibility.plan.as_ref()
                        && part.clusters.as_ref().is_some_and(|clusters| {
                            !clusters.iter().any(|cluster| {
                                plan.visible_clusters
                                    .get(*cluster as usize)
                                    .copied()
                                    .unwrap_or(false)
                            })
                        })
                    {
                        continue;
                    }
                    draws.push(Draw {
                        primitive: Primitive::Mesh(*item, Some(part.indices.clone())),
                        depth: depth(uniforms, part.centre),
                        blend: upload.assets.material_render_modes[item.material_slot],
                    });
                }
            }
        }
    }
    if schedule.is_none() {
        draws.extend(details.iter().map(|sprite| sprite.draw.clone()));
    }
    sort_draws(&mut draws);
    for mark in &plan.receiver_marks {
        let last = draws.iter().rposition(|draw| match &draw.primitive {
            Primitive::Mesh(item, range) if Some(item.mesh_index) == mark.receiver_mesh_index => {
                range.as_ref().is_none_or(|range| {
                    range.start < mark.receiver_range.start + mark.receiver_range.count
                        && range.end > mark.receiver_range.start
                })
            }
            Primitive::Overlay(item) => {
                item.receiver_mesh_index == mark.receiver_mesh_index
                    && item.receiver_range == mark.receiver_range
            }
            _ => false,
        });
        if let Some(index) = last {
            draws.insert(
                index + 1,
                Draw {
                    primitive: Primitive::Overlay(*mark),
                    depth: draws[index].depth,
                    blend: mark.blend,
                },
            );
        }
    }
    if plan
        .brushes
        .iter()
        .all(|brush| brush.transparent.is_empty())
    {
        return draws;
    }
    let mut groups: Vec<DrawGroup> = Vec::with_capacity(draws.len() + plan.brushes.len());
    for (index, draw) in draws.iter().enumerate() {
        if matches!(draw.primitive, Primitive::Overlay(_))
            && let Some(previous) = groups.last_mut()
            && previous.operations.end == index
            && previous.depth == draw.depth
        {
            previous.operations.end += 1;
        } else {
            groups.push(DrawGroup {
                depth: draw.depth,
                operations: index..index + 1,
            });
        }
    }
    for (index, brush) in plan.brushes.iter().enumerate() {
        if schedule.is_some_and(|schedule| schedule.covers_brush(index)) {
            continue;
        }
        let first = draws.len();
        let depth = owner_depth(uniforms, brush.origin, brush.local_sort_center);
        for surface in &brush.transparent {
            for mesh in &surface.meshes {
                draws.push(Draw {
                    primitive: Primitive::Mesh(mesh.item, Some(mesh.indices.clone())),
                    depth,
                    blend: upload.assets.material_render_modes[mesh.item.material_slot],
                });
            }
            draws.extend(surface.surface_marks.iter().map(|&mark| Draw {
                primitive: Primitive::Overlay(mark),
                depth,
                blend: mark.blend,
            }));
        }
        if first != draws.len() {
            groups.push(DrawGroup {
                depth,
                operations: first..draws.len(),
            });
        }
    }
    flatten_groups(&draws, &mut groups)
}

fn flatten_groups(draws: &[Draw], groups: &mut [DrawGroup]) -> Vec<Draw> {
    groups.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    let mut ordered = Vec::with_capacity(draws.len());
    for group in groups {
        ordered.extend_from_slice(&draws[group.operations.clone()]);
    }
    ordered
}

fn sort_draws(draws: &mut [Draw]) {
    draws.sort_by(|a, b| b.depth.total_cmp(&a.depth));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::{MapMeshClusterRanges, MapMeshIndexRange};

    #[test]
    fn owner_sort_subtracts_eye_before_adding_local_midpoint() {
        use bytemuck::Zeroable;
        let mut uniforms = Uniforms::zeroed();
        uniforms.camera_position[0] = 16_777_216.0;
        uniforms.view_proj[0][3] = 1.0;
        assert_eq!(
            owner_depth(
                &uniforms,
                Vec3::new(16_777_216.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
            ),
            1.0
        );
    }

    #[test]
    fn world_water_surface_enable_is_independent_of_above_under_selection() {
        use gmpublished_domain::scene::map::MapWorldMaterialGroup;
        for side in [WaterSide::Above, WaterSide::Under] {
            for include_water in [false, true] {
                let groups = WorldDrawGroups {
                    water_side: Some(side),
                    include_water,
                };
                assert_eq!(groups.allows(MapWorldMaterialGroup::Water), include_water);
                assert!(groups.allows(MapWorldMaterialGroup::IntersectsWater));
                assert_eq!(
                    groups.allows(MapWorldMaterialGroup::AboveWater),
                    side == WaterSide::Above
                );
                assert_eq!(
                    groups.allows(MapWorldMaterialGroup::Underwater),
                    side == WaterSide::Under
                );
            }
        }
    }

    #[test]
    fn owner_geometry_and_marks_stay_atomic_when_member_depths_straddle_foreground() {
        let mesh = |index, depth| Draw {
            primitive: Primitive::Mesh(
                DrawItem {
                    mesh_index: index,
                    material_slot: 0,
                },
                Some(0..3),
            ),
            depth,
            blend: RenderMode::Translucent,
        };
        let mark = OverlayDrawItem {
            blend: RenderMode::Modulate,
            receiver_mesh_index: Some(0),
            receiver_range: MapMeshIndexRange {
                face: 7,
                start: 0,
                count: 6,
            },
            overlay_index: 9,
            material_slot: 1,
        };
        let draws = vec![
            mesh(0, 30.0),
            mesh(1, 10.0),
            Draw {
                primitive: Primitive::Overlay(mark),
                depth: 10.0,
                blend: mark.blend,
            },
            mesh(2, 20.0),
        ];
        let mut groups = [
            DrawGroup {
                depth: 25.0,
                operations: 0..3,
            },
            DrawGroup {
                depth: 20.0,
                operations: 3..4,
            },
        ];
        let ordered = flatten_groups(&draws, &mut groups);
        assert!(matches!(ordered[0].primitive, Primitive::Mesh(item, _) if item.mesh_index == 0));
        assert!(matches!(ordered[1].primitive, Primitive::Mesh(item, _) if item.mesh_index == 1));
        assert!(
            matches!(ordered[2].primitive, Primitive::Overlay(item) if item.overlay_index == 9)
        );
        assert_eq!(ordered[2].blend, RenderMode::Modulate);
        assert!(matches!(ordered[3].primitive, Primitive::Mesh(item, _) if item.mesh_index == 2));
    }

    #[test]
    fn one_transparent_prop_can_straddle_a_separately_sorted_pane() {
        let make = |y| super::super::ModelVertex {
            position: Vec3::new(0.0, y, 0.0),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            surface_mark: 0,
            normal: Vec3::new(0.0, 1.0, 0.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            lightmap_page: 0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 0,
        };
        let vertices = vec![
            make(300.0),
            make(300.0),
            make(300.0),
            make(100.0),
            make(100.0),
            make(100.0),
        ];
        let metadata = MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 6,
            }],
            clusters: vec![],
        };
        let selected = parts(&vertices, &[0, 1, 2, 3, 4, 5], &metadata);
        assert_eq!(selected.len(), 2);
        let mut depths = selected
            .iter()
            .map(|part| part.centre[1])
            .chain([200.0])
            .collect::<Vec<_>>();
        depths.sort_by(|a, b| b.total_cmp(a));
        assert_eq!(depths, vec![300.0, 200.0, 100.0]);
        assert_eq!(selected[0].indices, 0..3);
        assert_eq!(selected[1].indices, 3..6);
    }

    #[test]
    fn shared_material_placements_keep_separate_ranges_and_cluster_membership() {
        let make = |z| super::super::ModelVertex {
            position: Vec3::new(0.0, 0.0, z),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            surface_mark: 0,
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            lightmap_page: 0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 0,
        };
        let vertices = vec![
            make(300.0),
            make(300.0),
            make(300.0),
            make(100.0),
            make(100.0),
            make(100.0),
        ];
        let range = MapMeshIndexRange {
            face: 7,
            start: 3,
            count: 3,
        };
        let metadata = MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 3,
            }],
            clusters: vec![
                MapMeshClusterRanges {
                    cluster: 2,
                    ranges: vec![range],
                },
                MapMeshClusterRanges {
                    cluster: 9,
                    ranges: vec![range],
                },
            ],
        };
        let selected = parts(&vertices, &[0, 1, 2, 3, 4, 5], &metadata);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].indices, 0..3);
        assert_eq!(selected[0].centre[2], 300.0);
        assert_eq!(selected[1].indices, 3..6);
        assert_eq!(selected[1].centre[2], 100.0);
        assert_eq!(selected[1].clusters.as_deref(), Some([2, 9].as_slice()));
    }

    #[test]
    fn skybox_material_batches_sort_individual_triangles_in_each_camera() {
        let make = |y| super::super::ModelVertex {
            position: Vec3::new(0.0, y, 0.0),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            surface_mark: 0,
            normal: Vec3::new(0.0, 1.0, 0.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            lightmap_page: 0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 0,
        };
        let vertices = [
            make(100.0),
            make(100.0),
            make(100.0),
            make(300.0),
            make(300.0),
            make(300.0),
        ];
        let selected = skybox_parts(&vertices, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|part| part.clusters.is_none()));
        for direction in [1.0, -1.0] {
            let mut uniforms = <Uniforms as bytemuck::Zeroable>::zeroed();
            uniforms.view_proj[1][3] = direction;
            let item = DrawItem {
                mesh_index: 0,
                material_slot: 0,
            };
            let mut draws = selected
                .iter()
                .map(|part| Draw {
                    primitive: Primitive::Mesh(item, Some(part.indices.clone())),
                    depth: depth(&uniforms, part.centre),
                    blend: RenderMode::Translucent,
                })
                .collect::<Vec<_>>();
            sort_draws(&mut draws);
            let Primitive::Mesh(_, Some(first)) = &draws[0].primitive else {
                panic!("triangle part");
            };
            assert_eq!(first.start, if direction > 0.0 { 3 } else { 0 });
        }
    }
}
