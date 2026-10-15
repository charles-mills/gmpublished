use super::{GpuModelContext, ModelLightingContext, ModelLightingHistory, encode, encode_into};

#[derive(Debug)]
pub(in super::super) struct State {
    styles: [f32; 128],
    data: Vec<GpuModelContext>,
    scratch: Vec<GpuModelContext>,
    histories: Vec<ModelLightingHistory>,
    initialized: bool,
    time: f64,
    active: Vec<bool>,
}

impl State {
    pub fn new(contexts: &[ModelLightingContext], styles: &[f32; 128]) -> Self {
        let data = encode(contexts, styles, 0.0, None, None);
        Self {
            styles: *styles,
            scratch: Vec::with_capacity(data.len()),
            data,
            histories: vec![ModelLightingHistory::default(); contexts.len()],
            initialized: false,
            time: 0.0,
            active: vec![false; contexts.len()],
        }
    }

    pub fn data(&self) -> &[GpuModelContext] {
        &self.data
    }

    pub fn reset_history(&mut self) {
        for history in &mut self.histories {
            history.reset();
        }
        self.initialized = false;
    }

    #[cfg(feature = "map-capture")]
    pub fn replay(
        &mut self,
        index: usize,
        context: &ModelLightingContext,
        styles: &[f32; 128],
        time: f64,
    ) {
        assert!(
            context.entity && context.source.runtime_seed().is_some(),
            "native timeline requires a production entity seed"
        );
        context
            .evaluate_with_history(styles, time, &mut self.histories[index])
            .expect("native history needs a valid production lighting seed");
    }

    pub fn advance(
        &mut self,
        contexts: &[ModelLightingContext],
        styles: &[f32; 128],
        time: f64,
        active: &[bool],
    ) -> bool {
        assert_eq!(
            contexts.len(),
            self.histories.len(),
            "lighting scene changed without reconstruction"
        );
        assert_eq!(
            contexts.len(),
            active.len(),
            "lighting visibility context identity changed"
        );
        if self.initialized && time < self.time {
            self.reset_history();
        }
        let active_changed = self.active != active;
        if self.initialized && time == self.time && *styles == self.styles && !active_changed {
            return false;
        }
        let dynamic = contexts.iter().zip(active).any(|(context, active)| {
            *active && context.entity && context.source.runtime_seed().is_some()
        });
        if self.initialized
            && !dynamic
            && !active_changed
            && *styles == self.styles
            && !self
                .histories
                .iter()
                .zip(active)
                .any(|(history, active)| *active && history.has_pending())
        {
            return false;
        }
        encode_into(
            contexts,
            styles,
            time,
            Some(&mut self.histories),
            Some(active),
            &mut self.scratch,
        );
        let changed = self.scratch != self.data;
        if changed {
            std::mem::swap(&mut self.data, &mut self.scratch);
        }
        self.styles = *styles;
        self.initialized = true;
        self.time = time;
        self.active.clone_from_slice(active);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_context_history_survives_until_its_next_draw_and_time_rewind_resets_it() {
        let mut contexts = [ModelLightingContext::ambient_fixture(1.0)];
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance(&contexts, &[1.0; 128], 10.0, &[true]);
        let first = state.histories[0].clone();
        contexts[0] = ModelLightingContext::ambient_fixture(0.0);
        state.advance(&contexts, &[1.0; 128], 10.2, &[false]);
        assert_eq!(state.histories[0], first);
        state.advance(&contexts, &[1.0; 128], 10.4, &[true]);
        assert!(state.data[1].ambient[0][0] > 0.0);
        assert!(state.data[1].ambient[0][0] < 1.0);
        state.advance(&contexts, &[1.0; 128], 2.0, &[true]);
        assert_eq!(state.data[1].ambient[0][0], 0.0);
        assert!(!state.histories[0].has_pending());
    }

    #[test]
    fn constant_styles_still_advance_a_visible_transition_and_widget_states_are_independent() {
        let mut contexts = [ModelLightingContext::ambient_fixture(1.0)];
        let mut first = State::new(&contexts, &[1.0; 128]);
        let mut second = State::new(&contexts, &[1.0; 128]);
        first.advance(&contexts, &[1.0; 128], 0.0, &[true]);
        contexts[0] = ModelLightingContext::ambient_fixture(0.0);
        first.advance(&contexts, &[1.0; 128], 0.1, &[true]);
        let previous = first.data[1].ambient[0][0];
        assert!(first.advance(&contexts, &[1.0; 128], 0.2, &[true]));
        assert!(first.data[1].ambient[0][0] < previous);
        second.advance(&contexts, &[1.0; 128], 0.2, &[true]);
        assert_eq!(second.data[1].ambient[0][0], 0.0);
        assert_ne!(first.histories, second.histories);
    }
}
