use super::pipeline::ViewerScene;
use super::{MapVisibilityBucket, RenderMode, WorldVisibilityPlan};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::MapSurfaceMarkSource;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlans {
    pub(super) content_id: u64,
    pub(super) world: DrawPlan,
    pub(super) map_skybox: Option<DrawPlan>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlan {
    pub(super) opaque: Vec<DrawItem>,
    pub(super) planar: Vec<DrawItem>,
    pub(super) surface_marks: Vec<OverlayDrawItem>,
    pub(super) receiver_marks: Vec<OverlayDrawItem>,
    pub(super) translucent: Vec<DrawItem>,
    pub(super) additive: Vec<DrawItem>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawItem {
    pub(super) mesh_index: usize,
    pub(super) material_slot: usize,
    pub(super) distance_squared: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayDrawItem {
    pub(super) blend: RenderMode,
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange,
    pub(super) overlay_index: usize,
    pub(super) material_slot: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MeshPlanSource {
    pub(super) mesh_index: usize,
    pub(super) scene_mesh_index: usize,
    pub(super) material_index: usize,
    pub(super) bodygroup: usize,
    pub(super) bodygroup_choice: usize,
    pub(super) centroid: Vec3,
    pub(super) map_skybox: bool,
    pub(super) door_index: Option<usize>,
    pub(super) door_visibility: Option<MapVisibilityBucket>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayPlanSource {
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange,
    pub(super) receiver_material_index: usize,
    pub(super) receiver_lightmap_page: u32,
    pub(super) source: gmpublished_domain::scene::map::MapSurfaceMarkSource,
    pub(super) render_order: u16,
    pub(super) overlay_index: usize,
    pub(super) material_index: usize,
    pub(super) map_skybox: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ReceiverPlanSource {
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) face: u32,
    pub(super) material_index: usize,
    pub(super) lightmap_page: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanMaterials<'a> {
    pub(super) render_modes: &'a [RenderMode],
    pub(super) planar_materials: &'a [bool],
    pub(super) material_count: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanSelection<'a> {
    pub(super) skin_remap: &'a [u16],
    pub(super) bodygroup_choices: &'a [usize],
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PartitionPlanContext<'a> {
    pub(super) materials: DrawPlanMaterials<'a>,
    pub(super) selection: DrawPlanSelection<'a>,
    pub(super) camera_position: [f32; 4],
    pub(super) partition: PartitionFilter,
    pub(super) visibility_plan: Option<&'a WorldVisibilityPlan>,
    pub(super) receiver_encounters: &'a [ReceiverPlanSource],
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanSourceSlices<'a> {
    pub(super) meshes: &'a [MeshPlanSource],
    pub(super) overlays: &'a [OverlayPlanSource],
    pub(super) materials: DrawPlanMaterials<'a>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanRequest<'a> {
    pub(super) content_id: u64,
    pub(super) selection: DrawPlanSelection<'a>,
    pub(super) camera_position: [f32; 4],
    pub(super) map_skybox_visible: bool,
    pub(super) map_skybox_content_present: bool,
    pub(super) map_skybox_camera_position: Option<[f32; 4]>,
    pub(super) visibility_plan: Option<&'a WorldVisibilityPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PartitionFilter {
    World,
    MapSkybox,
}

impl PartitionFilter {
    pub(super) const fn accepts(self, map_skybox: bool) -> bool {
        match self {
            Self::World => !map_skybox,
            Self::MapSkybox => map_skybox,
        }
    }
}

pub(super) fn prepare_draw_plans(upload: &ViewerScene, request: DrawPlanRequest<'_>) -> DrawPlans {
    let world = prepare_partition_draw_plan(
        upload,
        request.selection.skin_remap,
        request.selection.bodygroup_choices,
        request.camera_position,
        PartitionFilter::World,
        request.visibility_plan,
    );
    let map_skybox = if request.map_skybox_visible && request.map_skybox_content_present {
        request.map_skybox_camera_position.map(|camera_position| {
            prepare_partition_draw_plan(
                upload,
                request.selection.skin_remap,
                request.selection.bodygroup_choices,
                camera_position,
                PartitionFilter::MapSkybox,
                None,
            )
        })
    } else {
        None
    };
    DrawPlans {
        content_id: request.content_id,
        world,
        map_skybox,
    }
}

pub(super) fn prepare_partition_draw_plan(
    upload: &ViewerScene,
    skin_remap: &[u16],
    bodygroup_choices: &[usize],
    camera_position: [f32; 4],
    partition: PartitionFilter,
    visibility_plan: Option<&WorldVisibilityPlan>,
) -> DrawPlan {
    prepare_partition_draw_plan_from_sources(
        upload
            .meshes
            .iter()
            .enumerate()
            .map(|(mesh_index, mesh)| MeshPlanSource {
                mesh_index,
                scene_mesh_index: mesh.scene_mesh_index,
                material_index: mesh.material_index,
                bodygroup: mesh.bodygroup,
                bodygroup_choice: mesh.bodygroup_choice,
                centroid: mesh.centroid(),
                map_skybox: mesh.map_skybox,
                door_index: mesh.door_index,
                door_visibility: mesh.door_visibility,
            }),
        upload
            .overlays
            .iter()
            .enumerate()
            .map(|(overlay_index, overlay)| OverlayPlanSource {
                overlay_index,
                material_index: overlay.material_index,
                receiver_mesh_index: overlay.receiver_mesh_index,
                receiver_range: overlay.receiver_range,
                receiver_material_index: overlay.receiver_material_index,
                receiver_lightmap_page: 0,
                source: overlay.source,
                render_order: overlay.render_order,
                map_skybox: overlay.map_skybox,
            }),
        PartitionPlanContext {
            materials: DrawPlanMaterials {
                render_modes: upload.assets.material_render_modes.as_slice(),
                planar_materials: upload.assets.material_planar_materials.as_slice(),
                material_count: upload.assets.material_bind_groups.len(),
            },
            selection: DrawPlanSelection {
                skin_remap,
                bodygroup_choices,
            },
            camera_position,
            partition,
            visibility_plan,
            receiver_encounters: &[],
        },
    )
}

#[cfg(test)]
pub(super) fn prepare_draw_plans_from_sources(
    sources: DrawPlanSourceSlices<'_>,
    request: DrawPlanRequest<'_>,
) -> DrawPlans {
    let world = prepare_partition_draw_plan_from_sources(
        sources.meshes.iter().copied(),
        sources.overlays.iter().copied(),
        PartitionPlanContext {
            materials: sources.materials,
            selection: request.selection,
            camera_position: request.camera_position,
            partition: PartitionFilter::World,
            visibility_plan: request.visibility_plan,
            receiver_encounters: &[],
        },
    );
    let map_skybox = if request.map_skybox_visible && request.map_skybox_content_present {
        request.map_skybox_camera_position.map(|camera_position| {
            prepare_partition_draw_plan_from_sources(
                sources.meshes.iter().copied(),
                sources.overlays.iter().copied(),
                PartitionPlanContext {
                    materials: sources.materials,
                    selection: request.selection,
                    camera_position,
                    partition: PartitionFilter::MapSkybox,
                    visibility_plan: None,
                    receiver_encounters: &[],
                },
            )
        })
    } else {
        None
    };
    DrawPlans {
        content_id: request.content_id,
        world,
        map_skybox,
    }
}

pub(super) fn prepare_partition_draw_plan_from_sources(
    meshes: impl IntoIterator<Item = MeshPlanSource>,
    overlays: impl IntoIterator<Item = OverlayPlanSource>,
    context: PartitionPlanContext<'_>,
) -> DrawPlan {
    let mut plan = DrawPlan::default();
    let camera = [
        context.camera_position[0],
        context.camera_position[1],
        context.camera_position[2],
    ];
    for mesh in meshes {
        if !context.partition.accepts(mesh.map_skybox) {
            continue;
        }
        if context.partition == PartitionFilter::World
            && let Some(plan) = context.visibility_plan
        {
            let visible = if mesh.door_index.is_some() {
                mesh.door_visibility
                    .is_none_or(|bucket| plan.bucket_visible(bucket))
            } else {
                plan.mesh_visible(mesh.scene_mesh_index)
            };
            if !visible {
                continue;
            }
        }
        let selected = context
            .selection
            .bodygroup_choices
            .get(mesh.bodygroup)
            .copied()
            .unwrap_or(0);
        if mesh.bodygroup_choice != selected {
            continue;
        }
        let material_slot = remapped_material_slot(
            mesh.material_index,
            context.selection.skin_remap,
            context.materials.material_count,
        );
        let render_mode = context
            .materials
            .render_modes
            .get(material_slot)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        let item = DrawItem {
            mesh_index: mesh.mesh_index,
            material_slot,
            distance_squared: mesh.centroid.distance_squared(Vec3::from(camera)),
        };
        if context
            .materials
            .planar_materials
            .get(material_slot)
            .copied()
            .unwrap_or(false)
        {
            plan.planar.push(item);
        } else {
            match render_mode {
                RenderMode::Opaque | RenderMode::Cutout => plan.opaque.push(item),
                RenderMode::Translucent | RenderMode::Modulate => plan.translucent.push(item),
                RenderMode::Additive => plan.additive.push(item),
            }
        }
    }
    let overlays = overlays
        .into_iter()
        .filter(|overlay| {
            context.partition.accepts(overlay.map_skybox)
                && (context.partition != PartitionFilter::World
                    || context.visibility_plan.is_none_or(|plan| {
                        plan.overlay_visible(overlay.overlay_index)
                    }))
        })
        .collect::<Vec<_>>();
    for overlay in ordered_surface_marks(&overlays, context.receiver_encounters) {
        let material_slot = overlay
            .material_index
            .min(context.materials.material_count.saturating_sub(1));
        let render_mode = context
            .materials
            .render_modes
            .get(material_slot)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        let item = OverlayDrawItem {
            blend: render_mode,
            receiver_mesh_index: overlay.receiver_mesh_index,
            receiver_range: overlay.receiver_range,
            overlay_index: overlay.overlay_index,
            material_slot,
        };
        let receiver_mode = context
            .materials
            .render_modes
            .get(overlay.receiver_material_index)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        if matches!(receiver_mode, RenderMode::Opaque | RenderMode::Cutout) {
            plan.surface_marks.push(item);
        } else {
            plan.receiver_marks.push(item);
        }
    }

    plan.translucent.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan.planar.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan
}

fn ordered_surface_marks<'a>(
    marks: &'a [OverlayPlanSource],
    receiver_encounters: &[ReceiverPlanSource],
) -> Vec<&'a OverlayPlanSource> {
    let mut receiver_chains = Vec::new();
    let mut receiver_chain_indices = HashMap::new();
    let mut encountered_faces = HashSet::new();
    for receiver in receiver_encounters.iter().copied().chain(marks.iter().map(|mark| {
        ReceiverPlanSource {
            receiver_mesh_index: mark.receiver_mesh_index,
            face: mark.receiver_range.face,
            material_index: mark.receiver_material_index,
            lightmap_page: mark.receiver_lightmap_page,
        }
    })) {
        let face = (receiver.receiver_mesh_index, receiver.face);
        if !encountered_faces.insert(face) {
            continue;
        }
        let next_index = receiver_chains.len();
        let index = *receiver_chain_indices
            .entry((receiver.material_index, receiver.lightmap_page))
            .or_insert_with(|| {
                receiver_chains.push(Vec::new());
                next_index
            });
        receiver_chains[index].push(face);
    }

    let mut face_fragments = HashMap::<_, Vec<&OverlayPlanSource>>::new();
    let mut decals = Vec::new();
    for mark in marks {
        match mark.source {
            MapSurfaceMarkSource::Overlay { .. } => {
                face_fragments
                    .entry((mark.receiver_mesh_index, mark.receiver_range.face))
                    .or_default()
                    .push(mark);
            }
            MapSurfaceMarkSource::Decal { .. } => decals.push(mark),
        }
    }
    for fragments in face_fragments.values_mut() {
        fragments.sort_by_key(|mark| {
            let MapSurfaceMarkSource::Overlay { index, .. } = mark.source else {
                unreachable!();
            };
            std::cmp::Reverse((index, mark.overlay_index))
        });
    }

    let mut material_buckets = Vec::<Vec<&OverlayPlanSource>>::new();
    let mut material_bucket_indices = HashMap::new();
    for face in receiver_chains.iter().flatten() {
        for mark in face_fragments.get(face).into_iter().flatten() {
            let next_index = material_buckets.len();
            let index = *material_bucket_indices
                .entry((mark.material_index, mark.receiver_lightmap_page))
                .or_insert_with(|| {
                    material_buckets.push(Vec::new());
                    next_index
                });
            material_buckets[index].push(mark);
        }
    }

    // Source prepends both material buckets and their encountered fragments.
    let mut ordered = Vec::with_capacity(marks.len());
    for render_order in 0..=3 {
        for bucket in material_buckets.iter().rev() {
            ordered.extend(bucket.iter().rev().copied().filter(|mark| mark.render_order == render_order));
        }
    }
    decals.sort_by_key(|mark| match mark.source {
        MapSurfaceMarkSource::Decal { entity_index } => entity_index,
        MapSurfaceMarkSource::Overlay { .. } => unreachable!(),
    });
    ordered.extend(decals);
    ordered
}

pub(super) fn remapped_material_slot(
    material_index: usize,
    skin_remap: &[u16],
    material_count: usize,
) -> usize {
    let slot = skin_remap
        .get(material_index)
        .map_or(material_index, |&remapped| usize::from(remapped));
    slot.min(material_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh_plan_source(mesh_index: usize, map_skybox: bool, centroid: Vec3) -> MeshPlanSource {
        MeshPlanSource {
            mesh_index,
            scene_mesh_index: mesh_index,
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
            centroid,
            map_skybox,
            door_index: None,
            door_visibility: None,
        }
    }

    fn overlay_plan_source(
        overlay_index: usize,
        map_skybox: bool,
        _centroid: Vec3,
    ) -> OverlayPlanSource {
        OverlayPlanSource {
            receiver_mesh_index: None,
            receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 3,
            },
            receiver_material_index: 0,
            receiver_lightmap_page: 0,
            source: gmpublished_domain::scene::map::MapSurfaceMarkSource::Overlay {
                index: overlay_index,
                id: overlay_index as i32,
            },
            render_order: 0,
            overlay_index,
            material_index: 0,
            map_skybox,
        }
    }

    #[test]
    fn marks_follow_receiver_phase_and_authored_layer_order() {
        use gmpublished_domain::scene::map::MapSurfaceMarkSource;
        let base = overlay_plan_source(0, false, Vec3::ZERO);
        let overlays = [
            OverlayPlanSource {
                material_index: 1,
                render_order: 2,
                ..base
            },
            OverlayPlanSource {
                overlay_index: 1,
                material_index: 2,
                source: MapSurfaceMarkSource::Decal { entity_index: 7 },
                ..base
            },
            OverlayPlanSource {
                overlay_index: 2,
                material_index: 1,
                receiver_material_index: 1,
                receiver_mesh_index: Some(3),
                ..base
            },
            OverlayPlanSource {
                overlay_index: 3,
                material_index: 1,
                render_order: 0,
                ..base
            },
        ];
        let plan = prepare_partition_draw_plan_from_sources(
            [],
            overlays,
            PartitionPlanContext {
                materials: DrawPlanMaterials {
                    render_modes: &[
                        RenderMode::Opaque,
                        RenderMode::Translucent,
                        RenderMode::Modulate,
                    ],
                    planar_materials: &[],
                    material_count: 3,
                },
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                partition: PartitionFilter::World,
                visibility_plan: None,
                receiver_encounters: &[],
            },
        );
        assert_eq!(
            plan.surface_marks
                .iter()
                .map(|item| item.overlay_index)
                .collect::<Vec<_>>(),
            [3, 0, 1]
        );
        assert_eq!(plan.surface_marks[2].blend, RenderMode::Modulate);
        assert_eq!(plan.receiver_marks.len(), 1);
        assert_eq!(plan.receiver_marks[0].receiver_mesh_index, Some(3));
    }

    fn mark(index: usize, material: usize, face: u32) -> OverlayPlanSource {
        let mut mark = overlay_plan_source(index, false, Vec3::ZERO);
        mark.material_index = material;
        mark.receiver_range.face = face;
        mark
    }

    fn ordered_indices(marks: &[OverlayPlanSource], receivers: &[ReceiverPlanSource]) -> Vec<usize> {
        ordered_surface_marks(marks, receivers)
            .iter()
            .map(|mark| mark.overlay_index)
            .collect()
    }

    #[test]
    fn shared_material_fragments_keep_lump_order_inside_reverse_material_buckets() {
        let mut marks = [mark(0, 4, 7), mark(1, 2, 7), mark(2, 4, 7)];
        for (mark, id) in marks.iter_mut().zip([91, -10, 0]) {
            let MapSurfaceMarkSource::Overlay { index, .. } = mark.source else {
                unreachable!();
            };
            mark.source = MapSurfaceMarkSource::Overlay { index, id };
        }
        assert_eq!(ordered_indices(&marks, &[]), [1, 0, 2]);
        marks.reverse();
        assert_eq!(ordered_indices(&marks, &[]), [1, 0, 2]);
    }

    #[test]
    fn unmarked_receivers_prime_material_chains_before_marked_faces() {
        let mut marks = [mark(0, 7, 30), mark(1, 8, 20)];
        marks[0].receiver_material_index = 4;
        marks[1].receiver_material_index = 2;
        let receiver = |face, material_index| ReceiverPlanSource {
            receiver_mesh_index: None,
            face,
            material_index,
            lightmap_page: 0,
        };
        assert_eq!(
            ordered_indices(&marks, &[receiver(20, 2), receiver(30, 4)]),
            [0, 1]
        );
        assert_eq!(
            ordered_indices(&marks, &[receiver(10, 4), receiver(20, 2), receiver(30, 4)]),
            [1, 0]
        );
    }

    #[test]
    fn lightmap_pages_separate_receiver_and_mark_material_buckets() {
        let mut marks = [mark(0, 4, 0), mark(1, 2, 1), mark(2, 4, 2)];
        assert_eq!(ordered_indices(&marks, &[]), [1, 2, 0]);
        marks[2].receiver_lightmap_page = 1;
        assert_eq!(ordered_indices(&marks, &[]), [2, 1, 0]);
    }

    #[test]
    fn placed_brush_receivers_with_one_bsp_face_keep_separate_encounters() {
        let mut marks = [mark(0, 4, 7), mark(1, 4, 7)];
        marks[0].receiver_mesh_index = Some(0);
        marks[1].receiver_mesh_index = Some(1);
        assert_eq!(ordered_indices(&marks, &[]), [1, 0]);
    }

    #[test]
    fn render_order_precedes_material_buckets_and_static_decals_follow_overlays() {
        let mut marks = [mark(0, 4, 7), mark(1, 2, 7), mark(2, 4, 7), mark(3, 8, 7)];
        marks[0].render_order = 2;
        marks[3].source = MapSurfaceMarkSource::Decal { entity_index: 3 };
        assert_eq!(ordered_indices(&marks, &[]), [1, 2, 0, 3]);
    }

    #[test]
    fn water_meshes_are_partitioned_and_sorted_back_to_front() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(1.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                material_index: 1,
                centroid: Vec3::new(2.0, 0.0, 0.0),
                ..mesh_plan_source(1, false, Vec3::new(2.0, 0.0, 0.0))
            },
            MeshPlanSource {
                mesh_index: 2,
                material_index: 1,
                centroid: Vec3::new(4.0, 0.0, 0.0),
                ..mesh_plan_source(2, false, Vec3::new(4.0, 0.0, 0.0))
            },
            MeshPlanSource {
                mesh_index: 3,
                material_index: 2,
                centroid: Vec3::new(3.0, 0.0, 0.0),
                ..mesh_plan_source(3, false, Vec3::new(3.0, 0.0, 0.0))
            },
        ];
        let render_modes = [
            RenderMode::Opaque,
            RenderMode::Translucent,
            RenderMode::Translucent,
        ];
        let planar_materials = [false, true, false];

        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &planar_materials,
                    material_count: 3,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plans
                .world
                .planar
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            plans
                .world
                .translucent
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn map_skybox_disabled_omits_skybox_sets_from_draw_plans() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            mesh_plan_source(1, true, Vec3::new(10.0, 0.0, 0.0)),
        ];
        let overlays = [overlay_plan_source(0, true, Vec3::new(12.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &overlays,
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: true,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.content_id, 7);
        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert!(plans.world.surface_marks.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn missing_sky_camera_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, true, Vec3::new(10.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: true,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert!(plans.world.opaque.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn empty_skybox_partition_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: false,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.world.opaque.len(), 1);
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn visibility_plan_filters_world_meshes_but_off_path_draws_everything() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            mesh_plan_source(1, false, Vec3::new(10.0, 0.0, 0.0)),
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new()],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let off = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );
        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            off.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn visibility_plan_lookup_uses_scene_index_not_upload_index() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: 2,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: Vec3::new(10.0, 0.0, 0.0),
                map_skybox: false,
                door_index: None,
                door_visibility: None,
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new(), vec![3, 4, 5]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn visibility_plan_filters_door_meshes_by_bucket_not_scene_index() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: usize::MAX,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: Vec3::new(10.0, 0.0, 0.0),
                map_skybox: false,
                door_index: Some(0),
                door_visibility: Some(MapVisibilityBucket::Cluster(1)),
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let hidden = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true, false],
            visible_cluster_count: 1,
        };
        let visible = WorldVisibilityPlan {
            visible_clusters: vec![true, true],
            visible_cluster_count: 2,
            ..hidden.clone()
        };

        let plan = |visibility_plan| {
            prepare_draw_plans_from_sources(
                DrawPlanSourceSlices {
                    meshes: &meshes,
                    overlays: &[],
                    materials: DrawPlanMaterials {
                        render_modes: &render_modes,
                        planar_materials: &[],
                        material_count: 1,
                    },
                },
                DrawPlanRequest {
                    content_id: 7,
                    selection: DrawPlanSelection {
                        skin_remap: &[],
                        bodygroup_choices: &[],
                    },
                    camera_position: [0.0, 0.0, 0.0, 0.0],
                    map_skybox_visible: false,
                    map_skybox_content_present: false,
                    map_skybox_camera_position: None,
                    visibility_plan: Some(visibility_plan),
                },
            )
        };

        assert_eq!(
            plan(&hidden)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plan(&visible)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
