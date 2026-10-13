use super::{ContentUiState, Instant, ModelUiState, State};
use gmpublished_backend::model_decompile::PreviewPoseParameter;

#[derive(Debug, Default)]
pub(super) struct Playback {
    pub sequence: Option<usize>,
    pub cycle: f32,
    pub playing: bool,
    pub last_tick: Option<Instant>,
    pub parameters: Vec<f32>,
}

impl Playback {
    pub(super) fn for_model(model: &crate::media::preview_model::ModelPreview) -> Self {
        let Some(animation) = model
            .animation
            .as_ref()
            .and_then(|animation| animation.as_ref().ok())
        else {
            return Self::default();
        };
        if animation
            .sequences
            .first()
            .is_none_or(|sequence| sequence.unavailable.is_some())
        {
            return Self::default();
        }
        Self {
            sequence: Some(0),
            parameters: animation
                .parameters
                .iter()
                .map(PreviewPoseParameter::default_value)
                .collect(),
            ..Self::default()
        }
    }

    fn advance(&mut self, now: Instant, duration: f32, looping: bool) {
        let previous = self.last_tick.replace(now);
        if !self.playing || !duration.is_finite() || duration <= 0.0 {
            self.playing = false;
            return;
        }
        let elapsed = previous.map_or(0.0, |previous| {
            now.saturating_duration_since(previous).as_secs_f64()
        });
        let cycle = f64::from(self.cycle) + elapsed / f64::from(duration);
        if looping {
            self.cycle = cycle.rem_euclid(1.0) as f32;
        } else {
            self.cycle = cycle.min(1.0) as f32;
            self.playing &= cycle < 1.0;
        }
    }
}

impl State {
    fn model_selection(&self) -> Option<&ModelUiState> {
        if let Some(stage) = &self.stage {
            return Some(&stage.selection);
        }
        match &self.content_ui {
            ContentUiState::Model(model) => Some(model),
            _ => None,
        }
    }

    fn model_selection_mut(&mut self) -> Option<&mut ModelUiState> {
        if let Some(stage) = &mut self.stage {
            return Some(&mut stage.selection);
        }
        match &mut self.content_ui {
            ContentUiState::Model(model) => Some(model),
            _ => None,
        }
    }

    pub(crate) fn model_bounds(&self) -> Option<crate::media::preview_model::ModelBounds> {
        self.current_model()?.visible_bounds(
            self.model_animation_pose().as_ref(),
            self.bodygroup_choices(),
        )
    }

    pub(crate) fn model_framing_bounds(&self) -> Option<crate::media::preview_model::ModelBounds> {
        self.model_selection()?.framing_bounds
    }

    pub(super) fn refresh_model_framing(&mut self) {
        let bounds = self.model_bounds();
        if let Some(selection) = self.model_selection_mut() {
            selection.framing_bounds = bounds;
        }
    }

    pub(crate) fn model_sequence(&self) -> Option<usize> {
        self.model_selection()?.animation.sequence
    }

    pub(crate) fn model_cycle(&self) -> f32 {
        self.model_selection()
            .map_or(0.0, |model| model.animation.cycle)
    }

    pub(crate) fn model_playing(&self) -> bool {
        self.open
            && !self.loading()
            && self
                .model_selection()
                .is_some_and(|model| model.animation.playing)
    }

    pub(crate) fn model_animation_pose(
        &self,
    ) -> Option<gmpublished_backend::model_decompile::PreviewPose> {
        let sequence = self.model_sequence()?;
        let animation = self.current_model()?.animation.as_ref()?.as_ref().ok()?;
        animation
            .sequences
            .get(sequence)?
            .unavailable
            .is_none()
            .then(|| gmpublished_backend::model_decompile::PreviewPose {
                sequence,
                cycle: self.model_cycle(),
                parameters: self.model_pose_parameters().to_vec(),
            })
    }

    pub(crate) fn model_pose_parameters(&self) -> &[f32] {
        self.model_selection()
            .map_or(&[], |model| model.animation.parameters.as_slice())
    }

    pub(crate) fn model_sequence_duration(&self) -> f32 {
        self.current_model()
            .and_then(|model| model.animation.as_ref())
            .and_then(|animation| animation.as_ref().ok())
            .and_then(|animation| {
                animation.duration(self.model_sequence()?, self.model_pose_parameters())
            })
            .unwrap_or(0.0)
    }

    pub(in super::super) fn set_model_pose_parameter(&mut self, parameter: usize, value: f32) {
        if !self.open
            || self.loading()
            || !value.is_finite()
            || self.model_animation_pose().is_none()
        {
            return;
        }
        let Some(animation) = self
            .current_model()
            .and_then(|model| model.animation.as_ref())
            .and_then(|animation| animation.as_ref().ok())
        else {
            return;
        };
        let Some(sequence) = self
            .model_sequence()
            .and_then(|index| animation.sequences.get(index))
        else {
            return;
        };
        if !sequence.parameters.contains(&parameter) {
            return;
        }
        let Some(range) = animation.parameters.get(parameter) else {
            return;
        };
        let value = value.clamp(range.start.min(range.end), range.start.max(range.end));
        let defaults = animation
            .parameters
            .iter()
            .map(PreviewPoseParameter::default_value)
            .collect::<Vec<_>>();
        if let Some(model) = self.model_selection_mut() {
            if model.animation.parameters.len() != defaults.len() {
                model.animation.parameters = defaults;
            }
            model.animation.parameters[parameter] = value;
            model.animation.last_tick = None;
        }
        self.refresh_model_framing();
    }

    pub(in super::super) fn select_model_sequence(&mut self, choice: usize) {
        if !self.open || self.loading() {
            return;
        }
        let sequence = choice.checked_sub(1);
        if let Some(sequence) = sequence {
            let Some(animation) = self
                .current_model()
                .and_then(|model| model.animation.as_ref())
                .and_then(|animation| animation.as_ref().ok())
            else {
                return;
            };
            if sequence >= animation.sequences.len() {
                return;
            }
        }
        let parameters: Vec<f32> = self
            .current_model()
            .and_then(|model| model.animation.as_ref())
            .and_then(|animation| animation.as_ref().ok())
            .map(|animation| {
                animation
                    .parameters
                    .iter()
                    .map(PreviewPoseParameter::default_value)
                    .collect()
            })
            .unwrap_or_default();
        let parameters = self
            .model_selection()
            .filter(|model| model.animation.parameters.len() == parameters.len())
            .map_or(parameters, |model| model.animation.parameters.clone());
        if let Some(model) = self.model_selection_mut() {
            model.animation = Playback {
                sequence,
                parameters,
                ..Playback::default()
            };
        }
        self.refresh_model_framing();
    }

    pub(in super::super) fn toggle_model_playback(&mut self) {
        if !self.open || self.loading() || self.model_animation_pose().is_none() {
            return;
        }
        let duration = self.model_sequence_duration();
        if duration <= 0.0 {
            return;
        }
        if let Some(model) = self.model_selection_mut() {
            let playback = &mut model.animation;
            if playback.cycle >= 1.0 {
                playback.cycle = 0.0;
            }
            playback.playing = !playback.playing;
            playback.last_tick = None;
        }
    }

    pub(in super::super) fn seek_model_animation(&mut self, cycle: f32) {
        if !self.open
            || self.loading()
            || !cycle.is_finite()
            || self.model_animation_pose().is_none()
        {
            return;
        }
        if let Some(model) = self.model_selection_mut() {
            model.animation.cycle = cycle.clamp(0.0, 1.0);
            model.animation.last_tick = None;
        }
    }

    pub(super) fn tick_model_animation(&mut self, now: Instant) {
        if !self.model_playing() {
            return;
        }
        let timing = self
            .current_model()
            .and_then(|model| model.animation.as_ref())
            .and_then(|animation| animation.as_ref().ok())
            .and_then(|animation| animation.sequences.get(self.model_sequence()?))
            .map(|sequence| (self.model_sequence_duration(), sequence.looping));
        if let Some((duration, looping)) = timing
            && let Some(model) = self.model_selection_mut()
        {
            model.animation.advance(now, duration, looping);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn playback_pauses_seeks_wraps_and_finishes_without_clock_jumps() {
        let now = Instant::now();
        let mut playback = Playback {
            playing: true,
            ..Playback::default()
        };
        playback.advance(now, 2.0, false);
        playback.advance(now + Duration::from_millis(500), 2.0, false);
        assert_eq!(playback.cycle, 0.25);
        playback.playing = false;
        playback.last_tick = None;
        playback.cycle = 0.75;
        playback.advance(now + Duration::from_secs(8), 2.0, false);
        assert_eq!(playback.cycle, 0.75);
        playback.playing = true;
        playback.last_tick = None;
        playback.advance(now + Duration::from_secs(10), 2.0, true);
        playback.advance(now + Duration::from_secs(11), 2.0, true);
        assert_eq!(playback.cycle, 0.25);
        playback.advance(now + Duration::from_secs(14), 2.0, false);
        assert_eq!(playback.cycle, 1.0);
        assert!(!playback.playing);
        playback.playing = true;
        playback.last_tick = Some(now);
        playback.advance(now + Duration::from_secs(60), f32::MIN_POSITIVE, true);
        assert!(playback.cycle.is_finite() && (0.0..=1.0).contains(&playback.cycle));
        playback.advance(now, f32::INFINITY, true);
        assert!(!playback.playing);
    }
}
