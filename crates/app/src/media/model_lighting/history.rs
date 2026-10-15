use super::ModelLightingFailure;
use super::cpu::factory::{ModelLightingEvaluation, SelectedModelLight};
use gmpublished_domain::{math::Vec3, scene::map::AmbientCube};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelLightingHistory {
    state: Option<HistoryState>,
    pending: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct HistoryState {
    time: f32,
    ambient: AmbientCube,
    selected: Vec<SelectedModelLight>,
}

impl ModelLightingHistory {
    pub fn has_pending(&self) -> bool {
        self.pending
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn update(
        &mut self,
        time: f64,
        mut target: ModelLightingEvaluation,
        styles: &[f32; 128],
    ) -> Result<ModelLightingEvaluation, ModelLightingFailure> {
        if !time.is_finite() || !(time as f32).is_finite() || target.selected.len() > 4 {
            return Err(ModelLightingFailure::InvalidPosition);
        }
        let Some(previous) = &mut self.state else {
            self.state = Some(HistoryState {
                time: time as f32,
                ambient: target.ambient_cube,
                selected: target.selected.clone(),
            });
            self.pending = false;
            return Ok(target);
        };
        // Source subtracts a float timestamp from its double client clock.
        // Repeated draws can still have a positive delta if that timestamp
        // rounded down; only a nonpositive delta preserves the old value.
        let elapsed = (time - f64::from(previous.time)) as f32;
        let decay = if elapsed > 0.0 {
            previous.time = time as f32;
            (-5.0 * elapsed).exp()
        } else {
            1.0
        };
        self.pending = false;
        for (color, old) in target
            .ambient_cube
            .colors
            .iter_mut()
            .zip(previous.ambient.colors)
        {
            let desired = *color;
            *color = Vec3::from(
                (*color - (*color - old) * decay)
                    .to_array()
                    .map(|v| v.clamp(0.0, 8.0)),
            );
            self.pending |= *color != Vec3::from(desired.to_array().map(|v| v.clamp(0.0, 8.0)))
                && (elapsed <= 0.0 || *color != old);
        }
        let current_ids = target
            .selected
            .iter()
            .map(|selected| selected.light.original_index)
            .collect::<Vec<_>>();
        for selected in &mut target.selected {
            let current = Vec3::from(selected.light.intensity);
            let old = previous
                .selected
                .iter()
                .find(|old| old.light.original_index == selected.light.original_index)
                .map_or(Vec3::ZERO, |old| Vec3::from(old.light.intensity));
            let filtered = current - (current - old) * decay;
            selected.light.intensity = filtered.to_array();
            self.pending |= filtered != current && (elapsed <= 0.0 || filtered != old);
        }
        // Averaging precedes descriptor conversion and lightstyle modulation.
        // Removed raw lights below unit squared intensity disappear immediately;
        // brighter lights occupy remaining slots in their previous order.
        for old in &previous.selected {
            if target.selected.len() == 4 {
                break;
            }
            if current_ids.contains(&old.light.original_index)
                || Vec3::from(old.light.intensity).length_squared() < 1.0
            {
                continue;
            }
            let mut retained = *old;
            retained.light.intensity = (Vec3::from(old.light.intensity) * decay).to_array();
            retained.style_weight = usize::try_from(retained.light.style)
                .ok()
                .and_then(|style| styles.get(style))
                .copied()
                .unwrap_or(1.0);
            if !retained.style_weight.is_finite() || retained.style_weight < 0.0 {
                return Err(ModelLightingFailure::InvalidStyle);
            }
            target.selected.push(retained);
            self.pending = true;
        }
        previous.ambient = target.ambient_cube;
        previous.selected.clone_from(&target.selected);
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::MapWorldLight;
    use vformats::bsp::WorldLightKind;

    fn light(id: usize, intensity: f32, weight: f32) -> SelectedModelLight {
        SelectedModelLight {
            light: MapWorldLight {
                original_index: id,
                origin: [64.0, 0.0, 0.0],
                intensity: [intensity; 3],
                normal: [0.0; 3],
                cluster: 0,
                kind: WorldLightKind::Point,
                style: 32,
                stopdot: 0.0,
                stopdot2: 0.0,
                exponent: 0.0,
                radius: 512.0,
                constant_attenuation: 1.0,
                linear_attenuation: 0.0,
                quadratic_attenuation: 0.0,
                flags: 0,
                texinfo: -1,
                owner: -1,
            },
            style_weight: weight,
        }
    }

    fn target(ambient: f32, selected: Vec<SelectedModelLight>) -> ModelLightingEvaluation {
        ModelLightingEvaluation {
            ambient_cube: AmbientCube {
                colors: [Vec3::splat(ambient); 6],
            },
            selected,
            folded_ids: Vec::new(),
        }
    }

    #[test]
    fn initialization_snapshots_actual_first_draw_without_clamping_or_fading() {
        let mut history = ModelLightingHistory::default();
        let value = target(9.0, vec![light(3, 0.5, 2.0)]);
        assert_eq!(history.update(100.0, value.clone(), &[1.0; 128]), Ok(value));
        assert!(!history.has_pending());
        assert_eq!(history.state.as_ref().unwrap().time, 100.0);
    }

    #[test]
    fn raw_identity_fades_new_lights_while_style_changes_remain_instantaneous() {
        let mut history = ModelLightingHistory::default();
        history
            .update(0.0, target(0.0, vec![]), &[1.0; 128])
            .unwrap();
        let decay = (-1.0_f32).exp();
        let filtered = history
            .update(0.2, target(1.0, vec![light(3, 0.5, 1.0)]), &[1.0; 128])
            .unwrap();
        assert_eq!(filtered.selected[0].light.intensity, [0.5 - 0.5 * decay; 3]);
        assert_eq!(filtered.ambient_cube.colors, [Vec3::splat(1.0 - decay); 6]);
        assert!(history.has_pending());
        let same_time = f64::from(history.state.as_ref().unwrap().time);
        let new_style = history
            .update(
                same_time,
                target(1.0, vec![light(3, 0.5, 2.0)]),
                &[1.0; 128],
            )
            .unwrap();
        assert_eq!(
            new_style.selected[0].light.intensity,
            filtered.selected[0].light.intensity
        );
        assert_eq!(new_style.selected[0].style_weight, 2.0);
    }

    #[test]
    fn removed_dim_lights_drop_and_bright_lights_keep_raw_order_and_current_style() {
        let mut history = ModelLightingHistory::default();
        history
            .update(
                0.0,
                target(0.0, vec![light(1, 0.5, 1.0), light(2, 1.0, 1.0)]),
                &[1.0; 128],
            )
            .unwrap();
        let mut styles = [1.0; 128];
        styles[32] = 0.0;
        let value = history.update(0.2, target(0.0, vec![]), &styles).unwrap();
        assert_eq!(value.selected.len(), 1);
        assert_eq!(value.selected[0].light.original_index, 2);
        assert_eq!(value.selected[0].style_weight, 0.0);
        assert_eq!(value.selected[0].light.intensity, [(-1.0_f32).exp(); 3]);
        assert!(history.has_pending());
        let value = history.update(0.4, target(0.0, vec![]), &styles).unwrap();
        assert!(value.selected.is_empty());
        assert!(!history.has_pending());
    }

    #[test]
    fn previous_float_timestamp_and_double_clock_control_decay_and_clamping() {
        let mut history = ModelLightingHistory::default();
        let initial = 16_777_217.0_f64;
        history
            .update(initial, target(9.0, vec![]), &[1.0; 128])
            .unwrap();
        assert_eq!(history.state.as_ref().unwrap().time, 16_777_216.0);
        let value = history
            .update(initial, target(0.0, vec![]), &[1.0; 128])
            .unwrap();
        assert_eq!(
            value.ambient_cube.colors,
            [Vec3::splat(9.0 * (-5.0_f32).exp()); 6]
        );
        history.reset();
        history
            .update(0.0, target(9.0, vec![]), &[1.0; 128])
            .unwrap();
        let value = history
            .update(0.0, target(9.0, vec![]), &[1.0; 128])
            .unwrap();
        assert_eq!(value.ambient_cube.colors, [Vec3::splat(8.0); 6]);
    }

    #[test]
    fn current_raw_slots_take_priority_over_missing_history_and_unit_threshold_is_inclusive() {
        let mut history = ModelLightingHistory::default();
        let mut unit = light(9, 0.0, 1.0);
        unit.light.intensity = [1.0, 0.0, 0.0];
        history
            .update(0.0, target(0.0, vec![unit]), &[1.0; 128])
            .unwrap();
        let retained = history
            .update(0.0, target(0.0, vec![]), &[1.0; 128])
            .unwrap();
        assert_eq!(retained.selected, vec![unit]);
        let current = (0..4).map(|id| light(id, 0.25, 1.0)).collect::<Vec<_>>();
        let value = history
            .update(0.2, target(0.0, current), &[1.0; 128])
            .unwrap();
        assert_eq!(
            value
                .selected
                .iter()
                .map(|light| light.light.original_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(
            value
                .selected
                .iter()
                .all(|selected| selected.light.origin == [64.0, 0.0, 0.0])
        );
        assert!(
            value
                .selected
                .iter()
                .all(|selected| selected.light.constant_attenuation == 1.0)
        );
    }

    #[test]
    fn repeated_double_clock_can_advance_when_its_retained_float_rounds_down() {
        let mut history = ModelLightingHistory::default();
        let time = 1.000_000_04_f64;
        history
            .update(time, target(0.0, vec![]), &[1.0; 128])
            .unwrap();
        let target = target(0.0, vec![light(3, 0.5, 1.0)]);
        let first = history.update(time, target.clone(), &[1.0; 128]).unwrap();
        let second = history.update(time, target, &[1.0; 128]).unwrap();
        assert_eq!(history.state.as_ref().unwrap().time, 1.0);
        assert!(first.selected[0].light.intensity[0] > 0.0);
        assert!(second.selected[0].light.intensity[0] > first.selected[0].light.intensity[0]);
    }
}
