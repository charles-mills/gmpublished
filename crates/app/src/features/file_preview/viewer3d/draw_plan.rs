use super::pipeline::UploadedModel;
use super::{MapVisibilityBucket, RenderMode, WorldVisibilityPlan, distance_squared};

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlans {
    pub(super) content_id: u64,
    pub(super) world: DrawPlan,
    pub(super) map_skybox: Option<DrawPlan>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlan {
    pub(super) opaque: Vec<DrawItem>,
    pub(super) water: Vec<DrawItem>,
    pub(super) overlay_opaque: Vec<OverlayDrawItem>,
    pub(super) overlay_translucent: Vec<OverlayDrawItem>,
    pub(super) overlay_additive: Vec<OverlayDrawItem>,
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
    pub(super) overlay_index: usize,
    pub(super) material_slot: usize,
    pub(super) distance_squared: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MeshPlanSource {
    pub(super) mesh_index: usize,
    pub(super) scene_mesh_index: usize,
    pub(super) material_index: usize,
    pub(super) bodygroup: usize,
    pub(super) bodygroup_choice: usize,
    pub(super) centroid: [f32; 3],
    pub(super) map_skybox: bool,
    pub(super) door_index: Option<usize>,
    pub(super) door_visibility: Option<MapVisibilityBucket>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayPlanSource {
    pub(super) overlay_index: usize,
    pub(super) material_index: usize,
    pub(super) centroid: [f32; 3],
    pub(super) map_skybox: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanMaterials<'a> {
    pub(super) render_modes: &'a [RenderMode],
    pub(super) water_fallbacks: &'a [bool],
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
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanSourceSlices<'a> {
    pub(super) meshes: &'a [MeshPlanSource],
    pub(super) overlays: &'a [OverlayPlanSource],
    pub(super) materials: DrawPlanMaterials<'a>,
}

#[cfg(test)]
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

pub(super) fn prepare_draw_plans(
    content_id: u64,
    upload: &UploadedModel,
    skin_remap: &[u16],
    bodygroup_choices: &[usize],
    camera_position: [f32; 4],
    map_skybox_visible: bool,
    map_skybox_camera_position: Option<[f32; 4]>,
) -> DrawPlans {
    let world = prepare_partition_draw_plan(
        upload,
        skin_remap,
        bodygroup_choices,
        camera_position,
        PartitionFilter::World,
    );
    let map_skybox = if map_skybox_visible && upload.has_map_skybox_content() {
        map_skybox_camera_position.map(|camera_position| {
            prepare_partition_draw_plan(
                upload,
                skin_remap,
                bodygroup_choices,
                camera_position,
                PartitionFilter::MapSkybox,
            )
        })
    } else {
        None
    };
    DrawPlans {
        content_id,
        world,
        map_skybox,
    }
}

pub(super) fn prepare_partition_draw_plan(
    upload: &UploadedModel,
    skin_remap: &[u16],
    bodygroup_choices: &[usize],
    camera_position: [f32; 4],
    partition: PartitionFilter,
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
                centroid: mesh.centroid,
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
                centroid: overlay.centroid,
                map_skybox: overlay.map_skybox,
            }),
        PartitionPlanContext {
            materials: DrawPlanMaterials {
                render_modes: upload.material_render_modes.as_slice(),
                water_fallbacks: upload.material_water_fallbacks.as_slice(),
                material_count: upload.material_bind_groups.len(),
            },
            selection: DrawPlanSelection {
                skin_remap,
                bodygroup_choices,
            },
            camera_position,
            partition,
            visibility_plan: upload
                .visibility
                .plan
                .as_ref()
                .filter(|_| partition == PartitionFilter::World),
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
            distance_squared: distance_squared(mesh.centroid, camera),
        };
        if context
            .materials
            .water_fallbacks
            .get(material_slot)
            .copied()
            .unwrap_or(false)
        {
            plan.water.push(item);
        } else {
            match render_mode {
                RenderMode::Opaque | RenderMode::Cutout => plan.opaque.push(item),
                RenderMode::Translucent => plan.translucent.push(item),
                RenderMode::Additive => plan.additive.push(item),
            }
        }
    }
    for overlay in overlays {
        if !context.partition.accepts(overlay.map_skybox) {
            continue;
        }
        if context.partition == PartitionFilter::World
            && context
                .visibility_plan
                .is_some_and(|plan| !plan.overlay_visible(overlay.overlay_index))
        {
            continue;
        }
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
            overlay_index: overlay.overlay_index,
            material_slot,
            distance_squared: distance_squared(overlay.centroid, camera),
        };
        match render_mode {
            RenderMode::Opaque | RenderMode::Cutout => plan.overlay_opaque.push(item),
            RenderMode::Translucent => plan.overlay_translucent.push(item),
            RenderMode::Additive => plan.overlay_additive.push(item),
        }
    }
    plan.translucent.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan.water.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan.overlay_translucent.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.overlay_index.cmp(&right.overlay_index))
    });
    plan
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
