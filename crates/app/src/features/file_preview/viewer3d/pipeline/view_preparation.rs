use super::super::draw_plan::{DrawPlanRequest, WorldCache, prepare_draw_plans};
use super::geometry::ViewerScene;
use super::upload::UploadedModel;
use super::{DrawPlan, DrawPlans, Uniforms};
use std::sync::{Arc, Weak};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ViewGeometry {
    projection: [[f32; 4]; 4],
    eye: [f32; 4],
    clip: [f32; 4],
    raster_projection: [[f32; 4]; 4],
    raster_plane: [f32; 4],
}

impl From<&Uniforms> for ViewGeometry {
    fn from(view: &Uniforms) -> Self {
        Self {
            projection: view.view_proj,
            eye: view.camera_position,
            clip: view.clip_plane,
            raster_projection: view.raster_clip.view_projection,
            raster_plane: view.raster_clip.plane,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Entities {
    #[default]
    All,
    WorldOnly,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PlanGeometry {
    world: Option<ViewGeometry>,
    skybox: Option<ViewGeometry>,
    eye: [f32; 4],
    skybox_eye: Option<[f32; 4]>,
    skybox_visible: bool,
    skybox_present: bool,
    content_id: u64,
    visibility_revision: u64,
    geometry_revision: u64,
    entities: Entities,
}

impl PlanGeometry {
    fn new(
        request: DrawPlanRequest<'_>,
        visibility_revision: u64,
        geometry_revision: u64,
        entities: Entities,
    ) -> Self {
        Self {
            world: request.world_view.map(ViewGeometry::from),
            skybox: request.skybox_view.map(ViewGeometry::from),
            eye: request.camera_position,
            skybox_eye: request.map_skybox_camera_position,
            skybox_visible: request.map_skybox_visible,
            skybox_present: request.map_skybox_content_present,
            content_id: request.content_id,
            visibility_revision,
            geometry_revision,
            entities,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PlanCache {
    assets: Weak<UploadedModel>,
    geometry: Option<PlanGeometry>,
    skin: Vec<u16>,
    bodygroups: Vec<usize>,
    plans: Option<DrawPlans>,
    world_cache: WorldCache,
    traversal: gmpublished_domain::scene::map::MapWorldTraversal,
}

impl PlanCache {
    pub(super) fn is_cached(
        &self,
        upload: &ViewerScene,
        request: DrawPlanRequest<'_>,
        visibility_revision: u64,
        entities: Entities,
    ) -> bool {
        self.matches(
            upload,
            request.selection,
            &PlanGeometry::new(
                request,
                visibility_revision,
                upload.geometry_revision(),
                entities,
            ),
        )
    }

    fn matches(
        &self,
        upload: &ViewerScene,
        selection: super::super::draw_plan::DrawPlanSelection<'_>,
        geometry: &PlanGeometry,
    ) -> bool {
        self.assets.ptr_eq(&Arc::downgrade(&upload.assets))
            && self.geometry.as_ref() == Some(geometry)
            && self.skin == selection.skin_remap
            && self.bodygroups == selection.bodygroup_choices
    }

    pub fn prepare(
        &mut self,
        upload: &ViewerScene,
        request: DrawPlanRequest<'_>,
        visibility_revision: u64,
        entities: Entities,
    ) -> DrawPlans {
        let geometry = PlanGeometry::new(
            request,
            visibility_revision,
            upload.geometry_revision(),
            entities,
        );
        if self.matches(upload, request.selection, &geometry) {
            return self.plans.as_ref().expect("cached draw plans").clone();
        }
        if !self.assets.ptr_eq(&Arc::downgrade(&upload.assets)) {
            // Scratch belongs to this scene, not to a process-wide high-water mark.
            self.traversal = Default::default();
        }
        let mut plans = prepare_draw_plans(
            upload,
            request,
            &mut self.world_cache,
            &mut self.traversal,
            entities == Entities::WorldOnly,
        );
        if entities == Entities::WorldOnly {
            let world = Arc::make_mut(&mut plans.world);
            world.brushes.clear();
            world.receiver_marks.retain(|mark| {
                upload.overlays[mark.overlay_index].owner
                    == gmpublished_domain::scene::map::MapSurfaceOwner::World
            });
            for items in [
                &mut world.opaque,
                &mut world.translucent,
                &mut world.additive,
                &mut world.planar,
            ] {
                items.retain(|item| !upload.meshes[item.mesh_index].entity);
            }
        }
        self.assets = Arc::downgrade(&upload.assets);
        self.geometry = Some(geometry);
        self.skin.clear();
        self.skin.extend_from_slice(request.selection.skin_remap);
        self.bodygroups.clear();
        self.bodygroups
            .extend_from_slice(request.selection.bodygroup_choices);
        self.plans = Some(plans.clone());
        plans
    }
}

#[derive(Debug, Default)]
pub(super) struct QueueCache {
    assets: Weak<UploadedModel>,
    world_plan: Weak<DrawPlan>,
    skybox_plan: Option<Weak<DrawPlan>>,
    geometry: Option<(ViewGeometry, Option<ViewGeometry>)>,
    groups: Option<super::transparency::WorldDrawGroups>,
    visibility_revision: u64,
    skybox_visibility_revision: u64,
    geometry_revision: u64,
    animation_revision: u64,
}

#[derive(Clone, Copy)]
pub(super) struct QueueInputs<'a> {
    pub world: &'a Uniforms,
    pub skybox: Option<&'a Uniforms>,
    pub groups: super::transparency::WorldDrawGroups,
    pub visibility_revisions: (u64, u64),
}

impl QueueCache {
    pub fn update(
        &mut self,
        upload: &ViewerScene,
        plans: &DrawPlans,
        inputs: QueueInputs<'_>,
    ) -> bool {
        let QueueInputs {
            world,
            skybox,
            groups,
            visibility_revisions: (visibility_revision, skybox_visibility_revision),
        } = inputs;
        let geometry = (ViewGeometry::from(world), skybox.map(ViewGeometry::from));
        let skybox_plan = plans.map_skybox.as_ref().map(Arc::downgrade);
        let skybox_matches = match (&self.skybox_plan, &skybox_plan) {
            (None, None) => true,
            (Some(previous), Some(current)) => previous.ptr_eq(current),
            _ => false,
        };
        let assets = Arc::downgrade(&upload.assets);
        let world_plan = Arc::downgrade(&plans.world);
        let unchanged = self.assets.ptr_eq(&assets)
            && self.world_plan.ptr_eq(&world_plan)
            && skybox_matches
            && self.geometry == Some(geometry)
            && self.groups == Some(groups)
            && self.visibility_revision == visibility_revision
            && self.skybox_visibility_revision == skybox_visibility_revision
            && self.geometry_revision == upload.geometry_revision()
            && self.animation_revision == upload.animation_revision(plans);
        if !unchanged {
            *self = Self {
                assets,
                world_plan,
                skybox_plan,
                geometry: Some(geometry),
                groups: Some(groups),
                visibility_revision,
                skybox_visibility_revision,
                geometry_revision: upload.geometry_revision(),
                animation_revision: upload.animation_revision(plans),
            };
        }
        !unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn animation_and_shading_do_not_invalidate_geometry_but_optical_clipping_does() {
        let initial = Uniforms::zeroed();
        let mut animated = initial;
        animated.water_time_sky_tint[0] = 100.0;
        animated.lightmap_mode = 1;
        animated.fog.start = 123.0;
        assert_eq!(ViewGeometry::from(&initial), ViewGeometry::from(&animated));
        animated.raster_clip.plane[3] = 0.1;
        assert_ne!(ViewGeometry::from(&initial), ViewGeometry::from(&animated));
        animated = initial;
        animated.view_proj[0][0] = 2.0;
        assert_ne!(ViewGeometry::from(&initial), ViewGeometry::from(&animated));
        animated = initial;
        animated.camera_position[0] = 1.0;
        assert_ne!(ViewGeometry::from(&initial), ViewGeometry::from(&animated));
    }
}
