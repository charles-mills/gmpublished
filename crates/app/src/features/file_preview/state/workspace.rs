use super::{ContentUiState, LoadState, State};
use crate::media::preview_model::ModelBounds;

pub(super) const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

impl State {
    pub(crate) const fn workspace_transform(&self) -> [[f32; 4]; 4] {
        self.workspace_transform
    }

    pub(crate) fn set_workspace_transform(&mut self, matrix: [[f32; 4]; 4]) {
        if matrix.iter().flatten().all(|value| value.is_finite()) {
            self.workspace_transform = matrix;
        }
    }

    pub(crate) fn workspace_material_tints(&self) -> &[(usize, [u8; 3])] {
        &self.workspace_material_tints
    }

    pub(crate) fn workspace_skin_remap(&self) -> Vec<u16> {
        self.workspace_skin_remap.clone().unwrap_or_else(|| {
            self.current_model()
                .and_then(|model| model.skin_tables.get(self.selected_skin()))
                .cloned()
                .unwrap_or_default()
        })
    }

    pub(crate) fn set_workspace_appearance(
        &mut self,
        remap: Option<Vec<u16>>,
        tints: Vec<(usize, [u8; 3])>,
    ) {
        self.workspace_skin_remap = remap;
        self.workspace_material_tints = tints;
    }

    pub(crate) fn cancel_workspace_load(&mut self) {
        if !self.loading() {
            return;
        }
        if let Some(request) = &self.request {
            request.cancellation.cancel();
        }
        self.request_id.bump();
        if let Some(retained) = self.workspace_retained.take() {
            self.load = LoadState::Ready(retained.data);
            self.request = retained.request;
        } else {
            self.load = LoadState::Idle;
            self.request = None;
            self.content_ui = ContentUiState::None;
            self.stage = None;
        }
        self.stage_request = None;
        self.workspace_pending = false;
        self.workspace_error = None;
    }

    pub(super) fn workspace_point(
        matrix: [[f32; 4]; 4],
        point: gmpublished_domain::math::Vec3,
    ) -> gmpublished_domain::math::Vec3 {
        gmpublished_domain::math::Vec3::from(std::array::from_fn(|row| {
            matrix[3][row]
                + (0..3)
                    .map(|column| matrix[column][row] * point[column])
                    .sum::<f32>()
        }))
    }

    pub(super) fn transformed_workspace_bounds(&self, bounds: ModelBounds) -> ModelBounds {
        let matrix = self.workspace_transform;
        let mut result = ModelBounds {
            min: gmpublished_domain::math::Vec3::splat(f32::INFINITY),
            max: gmpublished_domain::math::Vec3::splat(f32::NEG_INFINITY),
        };
        for corner in 0..8 {
            let point: [f32; 3] = std::array::from_fn(|axis| {
                if corner & (1 << axis) == 0 {
                    bounds.min[axis]
                } else {
                    bounds.max[axis]
                }
            });
            let point = Self::workspace_point(matrix, point.into());
            for (axis, &value) in point.as_array().iter().enumerate() {
                result.min[axis] = result.min[axis].min(value);
                result.max[axis] = result.max[axis].max(value);
            }
        }
        result
    }
}
