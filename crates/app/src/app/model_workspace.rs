use gmpublished_backend::model_project::SkinMaterial;
use gmpublished_domain::math::Vec3;

use super::App;

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

impl App {
    pub(super) fn sync_model_workspace(&mut self) {
        let manager = &self.state.features.model_manager;
        let (skin, bodygroups) = manager.preview_selection();
        self.state
            .features
            .model_preview
            .apply_workspace_selection(skin, &bodygroups);
        let manager = &self.state.features.model_manager;
        let matrix = match (
            manager.displayed_preview_transform(),
            manager.desired_preview_transform(),
        ) {
            (Some(base), Some(desired)) => preview_transform(base, desired),
            (Some(_), None) => self.state.features.model_preview.workspace_transform(),
            (None, _) => IDENTITY,
        };
        let mut appearance = None;
        let mut tints = Vec::new();
        if let Some((replacements, slots)) = manager.preview_skin_materials()
            && let Some(model) = self.state.features.model_preview.current_model()
            && let Some(base) = model.skin_tables.first()
        {
            let mut remap = model
                .skin_tables
                .get(skin)
                .cloned()
                .unwrap_or_else(|| base.clone());
            for (slot, name) in slots.iter().enumerate() {
                let Some(&original) = base.get(slot) else {
                    continue;
                };
                match replacements.get(name) {
                    Some(SkinMaterial::Tint { color }) => {
                        if let Some(material) = remap.get_mut(slot) {
                            *material = original;
                        }
                        tints.push((usize::from(original), *color));
                    }
                    None => {
                        if let Some(material) = remap.get_mut(slot) {
                            *material = original;
                        }
                    }
                    Some(SkinMaterial::Texture { .. } | SkinMaterial::Material { .. }) => {}
                }
            }
            appearance = Some(remap);
        }
        self.state
            .features
            .model_preview
            .set_workspace_transform(matrix);
        self.state
            .features
            .model_preview
            .set_workspace_appearance(appearance, tints);
    }
}

fn preview_transform(base: (f32, [f32; 3]), desired: (f32, [f32; 3])) -> [[f32; 4]; 4] {
    let rotation = |angles: [f32; 3]| {
        let angles = Vec3::new(angles[1], angles[2], angles[0]);
        [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ]
        .map(|axis| axis.rotate_source(angles))
    };
    let old = rotation(base.1);
    let new = rotation(desired.1);
    let ratio = desired.0 / base.0;
    let mut matrix = IDENTITY;
    for (column, values) in matrix.iter_mut().enumerate().take(3) {
        for (row, value) in values.iter_mut().enumerate().take(3) {
            *value = (0..3)
                .map(|axis| new[axis][row] * old[axis][column])
                .sum::<f32>()
                * ratio;
        }
    }
    matrix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_transform_replaces_baked_scale_and_orientation() {
        let base = (2.0, [20.0, -30.0, 45.0]);
        let desired = (0.5, [-60.0, 15.0, 100.0]);
        let point = Vec3::new(3.0, -4.0, 2.0);
        let baked = point.rotate_source(Vec3::new(base.1[1], base.1[2], base.1[0])) * base.0;
        let expected =
            point.rotate_source(Vec3::new(desired.1[1], desired.1[2], desired.1[0])) * desired.0;
        let matrix = preview_transform(base, desired);
        let actual = Vec3::from(std::array::from_fn(|row| {
            (0..3)
                .map(|column| matrix[column][row] * baked[column])
                .sum::<f32>()
        }));
        for axis in 0..3 {
            assert!((actual[axis] - expected[axis]).abs() < 0.0001);
        }
    }
}
