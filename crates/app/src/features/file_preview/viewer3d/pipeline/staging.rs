use super::{ModelPrimitive, PreviewScene, StagePrimitive, Uniforms, ViewId};
use std::sync::{Arc, atomic::AtomicUsize};

impl ModelPrimitive {
    pub(super) fn stage_primitive(
        &self,
        stage: &StagePrimitive,
        view_id: ViewId,
        uniforms: &Uniforms,
    ) -> Self {
        Self {
            stage: None,
            studio_grid: None,
            animation_pose: stage.animation_pose.clone(),
            material_tints: stage.material_tints.clone(),
            cpu_executor: self.cpu_executor.clone(),
            render_feedback: self.render_feedback.clone(),
            model_lighting_time: self.model_lighting_time,
            animation_clock: self.animation_clock,
            view_id,
            clear_color: None,
            air_fog: None,
            prepared_frame: AtomicUsize::new(usize::MAX),
            skybox_transform: None,
            preview: PreviewScene::Model(Arc::clone(&stage.model)),
            content_id: stage.content_id,
            skin_remap: stage.skin_remap.clone(),
            bodygroup_choices: stage.bodygroup_choices.clone(),
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: stage.phy_debug_visible,
            uniforms: *uniforms,
            view_camera: None,
            submerged: self.submerged,
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(super) struct OpticalViews {
    pub surface_index: usize,
    pub reflection: Option<super::SceneView>,
    pub refraction: Option<super::SceneView>,
}

pub(super) fn prepare_optics(
    source: &super::PreparedFrame,
    staged: &mut super::PreparedFrame,
    transform: [[f32; 4]; 4],
    primitive: &ModelPrimitive,
    device: &super::wgpu::Device,
    queue: &super::wgpu::Queue,
    resources: &super::RenderResources,
) {
    if source.binding_request.is_none()
        || staged.binding_request.is_none()
        || source.main.plans.is_none()
        || staged.main.plans.is_none()
    {
        staged.staged_optics.clear();
        staged.staged_intersection = None;
        return;
    }
    let preparation = ViewPreparation {
        transform,
        primitive,
        device,
        queue,
        resources,
        scene: &staged.scene,
        lighting: &staged.lighting,
        marks: &staged.marks.buffer,
    };
    for (index, surface) in source.planar_views.iter().enumerate() {
        if index == staged.staged_optics.len() {
            staged.staged_optics.push(OpticalViews {
                surface_index: surface.surface_index,
                reflection: None,
                refraction: None,
            });
        }
        let views = &mut staged.staged_optics[index];
        if views.surface_index != surface.surface_index {
            *views = OpticalViews {
                surface_index: surface.surface_index,
                reflection: None,
                refraction: None,
            };
        }
        preparation.prepare(
            &mut views.reflection,
            surface.reflection.as_ref(),
            super::draw::ViewKind::Reflection,
        );
        preparation.prepare(
            &mut views.refraction,
            surface.refraction.as_ref(),
            super::draw::ViewKind::Refraction,
        );
    }
    staged.staged_optics.truncate(source.planar_views.len());
    preparation.prepare(
        &mut staged.staged_intersection,
        source
            .water_intersection
            .as_ref()
            .filter(|view| view.selection.is_some())
            .map(|view| &view.view),
        super::draw::ViewKind::Main,
    );
}

struct ViewPreparation<'a> {
    transform: [[f32; 4]; 4],
    primitive: &'a ModelPrimitive,
    device: &'a super::wgpu::Device,
    queue: &'a super::wgpu::Queue,
    resources: &'a super::RenderResources,
    scene: &'a super::ViewerScene,
    lighting: &'a super::lighting::Frame,
    marks: &'a super::wgpu::Buffer,
}

impl ViewPreparation<'_> {
    fn prepare(
        &self,
        destination: &mut Option<super::SceneView>,
        source: Option<&super::SceneView>,
        kind: super::draw::ViewKind,
    ) {
        let Some(source) = source.filter(|view| {
            view.entities == super::view_preparation::Entities::All && view.plans.is_some()
        }) else {
            *destination = None;
            return;
        };
        let view = destination.get_or_insert_with(|| {
            super::SceneView::new(self.device, self.resources, self.lighting, self.marks)
        });
        let mut uniforms = source.world.value;
        uniforms.apply_model_transform(self.transform);
        view.world.write(self.queue, &uniforms);
        let input = super::optical_planning::Input {
            world: uniforms,
            skybox: None,
            skybox_visible: false,
            entities: super::view_preparation::Entities::All,
            water_side: None,
            include_water: true,
            kind,
            excluded_plane: None,
            size: source
                .targets
                .as_ref()
                .map_or((0, 0), |targets| targets.size),
        };
        super::optical_planning::prepare_view(
            view,
            &input,
            self.scene,
            super::optical_planning::Common::new(self.primitive, self.scene),
            false,
        );
        super::planar::prepare(
            view,
            self.scene,
            self.primitive.preview.render_scene(),
            self.device,
            self.queue,
            None,
        );
        // Every optical view retains its own argument slot until submission.
        super::transparent_indirect::prepare(view, self.scene, self.device, self.queue);
    }
}
