use super::{ModelPrimitive, PreviewScene, StagePrimitive, ViewId, mat_mul};
use std::sync::{Arc, atomic::AtomicUsize};

impl ModelPrimitive {
    pub(super) fn stage_primitive(&self, stage: &StagePrimitive, view_id: ViewId) -> Self {
        let mut uniforms = self.uniforms;
        uniforms.apply_model_transform(mat_mul(stage.transform.matrix(), stage.live_transform));
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
            uniforms,
            view_camera: None,
            submerged: self.submerged,
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
        }
    }
}
