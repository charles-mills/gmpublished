use super::{
    Generation, Instant, ModelUiState, PreviewContent, PreviewData, PreviewLoadError,
    PreviewRequest, State, Vec3,
};
use crate::media::preview_model::ModelPreview;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StageTransform {
    pub position: Vec3,
    /// Source pitch, yaw, roll in degrees.
    pub angles: Vec3,
}

impl StageTransform {
    pub(in super::super) fn is_finite(self) -> bool {
        self.position.is_finite() && self.angles.is_finite()
    }

    pub(in super::super) fn matrix(self) -> [[f32; 4]; 4] {
        let axes = [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ]
        .map(|axis| axis.rotate_source(self.angles));
        [
            [axes[0][0], axes[0][1], axes[0][2], 0.0],
            [axes[1][0], axes[1][1], axes[1][2], 0.0],
            [axes[2][0], axes[2][1], axes[2][2], 0.0],
            [self.position[0], self.position[1], self.position[2], 1.0],
        ]
    }

    pub(in super::super) fn inverse_vector(self, vector: Vec3) -> Vec3 {
        let matrix = self.matrix();
        Vec3::new(
            vector.dot(Vec3::new(matrix[0][0], matrix[0][1], matrix[0][2])),
            vector.dot(Vec3::new(matrix[1][0], matrix[1][1], matrix[1][2])),
            vector.dot(Vec3::new(matrix[2][0], matrix[2][1], matrix[2][2])),
        )
    }

    pub(in super::super) fn local_point(self, point: Vec3) -> Vec3 {
        self.inverse_vector(point - self.position)
    }
}

#[derive(Debug)]
pub(super) struct StageState {
    pub(super) model: Arc<ModelPreview>,
    pub(super) model_request: PreviewRequest,
    pub(super) transform: StageTransform,
    pub(super) initial: StageTransform,
    pub(super) undo: Vec<StageTransform>,
    pub(super) drag_start: Option<StageTransform>,
    pub(super) selection: ModelUiState,
    pub(super) position_fields: [String; 3],
    pub(super) angle_fields: [String; 3],
    falling: Option<FallState>,
}

impl StageState {
    fn push_undo(&mut self, transform: StageTransform) {
        if self.undo.last() != Some(&transform) {
            self.undo.push(transform);
        }
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    fn refresh_fields(&mut self) {
        self.position_fields = (*self.transform.position.as_array()).map(|v| format!("{v:.2}"));
        self.angle_fields = (*self.transform.angles.as_array()).map(|v| format!("{v:.1}"));
    }
}

impl State {
    pub(crate) fn begin_stage(
        &mut self,
        mut model: PreviewRequest,
        map: PreviewRequest,
    ) -> (PreviewRequest, PreviewRequest) {
        let map = self.begin_open(map);
        self.expanded = true;
        model.request_id = map.request_id;
        self.stage_request = Some(model.clone());
        (model, map)
    }

    pub(crate) fn apply_stage_loaded(
        &mut self,
        generation: Generation,
        result: Result<(PreviewData, Arc<ModelPreview>), PreviewLoadError>,
    ) -> bool {
        if !self.open || self.request_id != generation || !self.loading() {
            return false;
        }
        match result {
            Ok((data, model)) => {
                let Some(model_request) = self.stage_request.take() else {
                    return false;
                };
                let selection = ModelUiState::for_model(&model);
                let initial = initial_placement(&data, selection.framing_bounds);
                if !self.apply_loaded(generation, Ok(data)) {
                    return false;
                }
                let mut stage = StageState {
                    model,
                    model_request,
                    transform: initial,
                    initial,
                    undo: Vec::new(),
                    drag_start: None,
                    selection,
                    position_fields: std::array::from_fn(|_| String::new()),
                    angle_fields: std::array::from_fn(|_| String::new()),
                    falling: None,
                };
                stage.refresh_fields();
                self.stage = Some(stage);
            }
            Err(error) => {
                self.apply_loaded(generation, Err(error));
            }
        }
        true
    }

    pub(crate) fn staged_placement(&self) -> Option<([f32; 3], [f32; 3])> {
        self.stage_transform()
            .map(|t| (*t.position.as_array(), *t.angles.as_array()))
    }
    pub(crate) fn restore_stage_placement(&mut self, position: [f32; 3], angles: [f32; 3]) {
        let transform = StageTransform {
            position: Vec3::from(position),
            angles: Vec3::from(angles),
        };
        if transform.is_finite()
            && let Some(stage) = &mut self.stage
        {
            stage.transform = transform;
            stage.initial = transform;
            stage.undo.clear();
            stage.refresh_fields();
        }
    }
    pub(crate) fn stage_transform(&self) -> Option<StageTransform> {
        self.stage.as_ref().map(|s| s.transform)
    }
    pub(crate) fn stage_model(&self) -> Option<&Arc<ModelPreview>> {
        self.stage.as_ref().map(|s| &s.model)
    }
    pub(crate) fn stage_content_id(&self) -> Option<u64> {
        let stage = self.stage.as_ref()?;
        Some(
            PreviewData::from_request(
                &stage.model_request,
                PreviewContent::Model(Arc::clone(&stage.model)),
            )
            .content_id(),
        )
    }
    pub(in super::super) fn stage_fields(&self) -> Option<(&[String; 3], &[String; 3])> {
        self.stage
            .as_ref()
            .map(|s| (&s.position_fields, &s.angle_fields))
    }
    pub(in super::super) fn stage_can_undo(&self) -> bool {
        self.stage.as_ref().is_some_and(|s| !s.undo.is_empty())
    }
    pub(in super::super) fn stage_move_started(&mut self) {
        self.stage_move_finished();
        if let Some(stage) = &mut self.stage {
            stage.falling = None;
            stage.drag_start = Some(stage.transform);
        }
    }
    pub(in super::super) fn stage_move_finished(&mut self) {
        if let Some(stage) = &mut self.stage
            && let Some(start) = stage.drag_start.take()
            && start != stage.transform
        {
            stage.push_undo(start);
        }
    }
    pub(in super::super) fn set_stage_transform(&mut self, transform: StageTransform) {
        if !transform.is_finite() {
            return;
        }
        if self.stage_falling() {
            self.stage_move_finished();
        }
        if let Some(stage) = &mut self.stage
            && stage.transform != transform
        {
            if stage.drag_start.is_none() {
                stage.push_undo(stage.transform);
            }
            stage.falling = None;
            stage.transform = transform;
            stage.refresh_fields();
        }
    }
    pub(in super::super) fn edit_stage_field(
        &mut self,
        rotation: bool,
        axis: usize,
        value: String,
    ) {
        if axis >= 3 {
            return;
        }
        if self.stage_falling() {
            self.stage_move_finished();
        }
        if let Some(stage) = &mut self.stage {
            let fields = if rotation {
                &mut stage.angle_fields
            } else {
                &mut stage.position_fields
            };
            let parsed = value.parse::<f32>();
            fields[axis] = value;
            stage.falling = None;
            if let Ok(number) = parsed
                && number.is_finite()
            {
                let mut transform = stage.transform;
                if rotation {
                    transform.angles[axis] = number;
                } else {
                    transform.position[axis] = number;
                }
                if stage.transform != transform {
                    stage.push_undo(stage.transform);
                    stage.transform = transform;
                }
            }
        }
    }
    pub(in super::super) fn reset_stage(&mut self) {
        self.stage_move_finished();
        if let Some(stage) = &mut self.stage {
            stage.falling = None;
        }
        if let Some(initial) = self.stage.as_ref().map(|s| s.initial) {
            self.set_stage_transform(initial);
        }
    }
    pub(in super::super) fn undo_stage(&mut self) {
        if let Some(stage) = &mut self.stage
            && let Some(transform) = stage
                .drag_start
                .take()
                .filter(|start| *start != stage.transform)
                .or_else(|| stage.undo.pop())
        {
            stage.transform = transform;
            stage.falling = None;
            stage.drag_start = None;
            stage.refresh_fields();
        }
    }
    pub(in super::super) fn place_stage_ahead(&mut self) {
        let Some(pose) = self.fly_pose() else {
            return;
        };
        let Some(mut transform) = self.stage_transform() else {
            return;
        };
        let forward = Vec3::new(
            pose.pitch.cos() * pose.yaw.cos(),
            pose.pitch.cos() * pose.yaw.sin(),
            pose.pitch.sin(),
        );
        let Some(bounds) = self.model_bounds() else {
            return;
        };
        let center = bounds.center();
        transform.position =
            pose.position + forward * 192.0 - center.rotate_source(transform.angles);
        self.set_stage_transform(transform);
    }
}

fn initial_placement(
    data: &PreviewData,
    bounds: Option<crate::media::preview_model::ModelBounds>,
) -> StageTransform {
    let PreviewContent::Map { scene, spawn, .. } = &data.content else {
        return StageTransform::default();
    };
    let (origin, yaw) = spawn.map_or(
        (
            super::super::viewer3d::mid(scene.scene.bounds_min, scene.scene.bounds_max),
            0.0,
        ),
        |spawn| (spawn.origin, spawn.angles[1]),
    );
    let yaw_radians = yaw.to_radians();
    let mut position = origin + Vec3::new(yaw_radians.cos(), yaw_radians.sin(), 0.0) * 192.0;
    if let Some(collision) = &scene.walk_collision {
        let start = position + Vec3::new(0.0, 0.0, 128.0);
        let end = position - Vec3::new(0.0, 0.0, 4096.0);
        let trace = collision.trace_aabb(start, end, Vec3::ZERO);
        if !trace.start_solid && trace.fraction < 1.0 {
            position = trace.end_position;
        }
    }
    if let Some(bounds) = bounds {
        let center = bounds.center();
        position[0] -= center[0];
        position[1] -= center[1];
        position[2] -= bounds.min[2];
    }
    StageTransform {
        position,
        angles: Vec3::ZERO,
    }
}

#[derive(Debug)]
struct FallState {
    velocity: f32,
    elapsed: f32,
    previous: Option<Instant>,
}

impl State {
    pub(crate) fn stage_falling(&self) -> bool {
        self.stage
            .as_ref()
            .is_some_and(|stage| stage.falling.is_some())
    }
    pub(in super::super) fn stage_can_drop(&self) -> bool {
        self.stage
            .as_ref()
            .is_some_and(|stage| !stage.model.scene.phy_debug_meshes.is_empty())
            && matches!(self.current().map(|data| &data.content), Some(PreviewContent::Map { scene, .. }) if scene.walk_collision.as_ref().is_some_and(|collision| !collision.is_empty()))
    }
    pub(in super::super) fn drop_stage(&mut self) {
        if !self.stage_can_drop() {
            return;
        }
        self.stage_move_started();
        if let Some(stage) = &mut self.stage {
            stage.falling = Some(FallState {
                velocity: 0.0,
                elapsed: 0.0,
                previous: None,
            });
        }
    }
    pub(in super::super) fn tick_stage(&mut self, now: Instant) {
        let super::LoadState::Ready(data) = &self.load else {
            return;
        };
        let PreviewContent::Map { scene, .. } = &data.content else {
            return;
        };
        let Some(collision) = &scene.walk_collision else {
            return;
        };
        let Some(stage) = &mut self.stage else {
            return;
        };
        let Some(fall) = &mut stage.falling else {
            return;
        };
        let Some(previous) = fall.previous.replace(now) else {
            return;
        };
        let dt = now
            .saturating_duration_since(previous)
            .as_secs_f32()
            .min(0.1);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for vertex in stage
            .model
            .scene
            .phy_debug_meshes
            .iter()
            .flat_map(|mesh| &mesh.vertices)
        {
            let point = vertex.position.rotate_source(stage.transform.angles);
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
        if !min.is_finite() || !max.is_finite() {
            stage.falling = None;
            return;
        }
        let offset = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        fall.velocity -= 600.0 * dt;
        fall.elapsed += dt;
        let center = stage.transform.position + offset;
        let end = center + Vec3::new(0.0, 0.0, fall.velocity * dt);
        let trace = collision.trace_aabb(center, end, half);
        if !trace.start_solid {
            stage.transform.position = trace.end_position - offset;
            stage.refresh_fields();
        }
        if trace.start_solid
            || trace.fraction < 1.0
            || stage
                .falling
                .as_ref()
                .is_some_and(|fall| fall.elapsed >= 10.0)
        {
            stage.falling = None;
            self.stage_move_finished();
        }
    }
}
