use super::{
    GpuModelContext, ModelLightingContext, ModelLightingHistory, UsedStyles, encode, encode_context,
};

#[derive(Clone, Copy)]
struct Contexts<'a> {
    originals: &'a [ModelLightingContext],
    overrides: Option<&'a std::collections::BTreeMap<usize, ModelLightingContext>>,
}

impl<'a> Contexts<'a> {
    fn get(self, index: usize) -> &'a ModelLightingContext {
        self.overrides
            .and_then(|overrides| overrides.get(&index))
            .unwrap_or(&self.originals[index])
    }
}

#[derive(Debug)]
pub(in super::super) struct State {
    styles: [f32; 128],
    used_styles: UsedStyles<128>,
    data: Vec<GpuModelContext>,
    changed_ranges: Vec<std::ops::Range<usize>>,
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
            used_styles: UsedStyles::new(contexts.iter().flat_map(|context| {
                context
                    .source
                    .runtime_seed()
                    .into_iter()
                    .chain(
                        context
                            .compiled_supplement
                            .as_ref()
                            .and_then(|seed| seed.as_ref().ok()),
                    )
                    .flat_map(|seed| seed.used_styles())
            })),
            data,
            changed_ranges: Vec::new(),
            histories: vec![ModelLightingHistory::default(); contexts.len()],
            initialized: false,
            time: 0.0,
            active: vec![false; contexts.len()],
        }
    }

    pub fn data(&self) -> &[GpuModelContext] {
        &self.data
    }

    pub fn changed_ranges(&self) -> &[std::ops::Range<usize>] {
        &self.changed_ranges
    }

    fn mark_changed(&mut self, slot: usize) {
        if let Some(previous) = self.changed_ranges.last_mut()
            && previous.end == slot
        {
            previous.end += 1;
        } else {
            self.changed_ranges.push(slot..slot + 1);
        }
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
        self.advance_with(contexts, styles, time, active, encode_context)
    }

    pub fn advance_overrides(
        &mut self,
        contexts: &[ModelLightingContext],
        overrides: &std::collections::BTreeMap<usize, ModelLightingContext>,
        origins_changed: bool,
        styles: &[f32; 128],
        time: f64,
        active: &[bool],
    ) -> bool {
        if origins_changed {
            self.used_styles
                .include(overrides.values().flat_map(|context| {
                    context
                        .source
                        .runtime_seed()
                        .into_iter()
                        .chain(
                            context
                                .compiled_supplement
                                .as_ref()
                                .and_then(|seed| seed.as_ref().ok()),
                        )
                        .flat_map(|seed| seed.used_styles())
                }));
        }
        self.advance_resolved(
            Contexts {
                originals: contexts,
                overrides: Some(overrides),
            },
            styles,
            time,
            active,
            origins_changed,
            encode_context,
        )
    }

    fn advance_with(
        &mut self,
        contexts: &[ModelLightingContext],
        styles: &[f32; 128],
        time: f64,
        active: &[bool],
        evaluate: impl FnMut(
            &ModelLightingContext,
            &[f32; 128],
            f64,
            Option<&mut ModelLightingHistory>,
        ) -> GpuModelContext,
    ) -> bool {
        self.advance_resolved(
            Contexts {
                originals: contexts,
                overrides: None,
            },
            styles,
            time,
            active,
            false,
            evaluate,
        )
    }

    fn advance_resolved(
        &mut self,
        contexts: Contexts<'_>,
        styles: &[f32; 128],
        time: f64,
        active: &[bool],
        origins_changed: bool,
        mut evaluate: impl FnMut(
            &ModelLightingContext,
            &[f32; 128],
            f64,
            Option<&mut ModelLightingHistory>,
        ) -> GpuModelContext,
    ) -> bool {
        assert_eq!(
            contexts.originals.len(),
            self.histories.len(),
            "lighting scene changed without reconstruction"
        );
        assert_eq!(
            contexts.originals.len(),
            active.len(),
            "lighting visibility context identity changed"
        );
        if self.initialized && time < self.time {
            self.reset_history();
        }
        self.changed_ranges.clear();
        let active_changed = self.active != active;
        let styles_changed = self.used_styles.changed(&self.styles, styles);
        if self.initialized
            && time == self.time
            && !styles_changed
            && !active_changed
            && !origins_changed
        {
            return false;
        }
        let dynamic = active.iter().enumerate().any(|(index, active)| {
            let context = contexts.get(index);
            *active && context.entity && context.source.runtime_seed().is_some()
        });
        if self.initialized
            && !dynamic
            && !origins_changed
            && !active_changed
            && !styles_changed
            && !self
                .histories
                .iter()
                .zip(active)
                .any(|(history, active)| *active && history.has_pending())
        {
            self.time = time;
            return false;
        }
        for (index, active) in active.iter().enumerate() {
            if !active {
                continue;
            }
            let context = contexts.get(index);
            let next = evaluate(context, styles, time, Some(&mut self.histories[index]));
            let previous = &mut self.data[index + 1];
            if *previous != next {
                *previous = next;
                self.mark_changed(index + 1);
            }
        }
        self.styles = *styles;
        self.initialized = true;
        self.time = time;
        self.active.clone_from_slice(active);
        !self.changed_ranges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_override_of_a_fallback_keeps_history_and_new_styles_live() {
        use crate::media::preview_model::{ModelLightingFallbackReason, ModelLightingSource};
        let mut posed = ModelLightingContext::ambient_fixture(1.0);
        let ModelLightingSource::Runtime { seed, .. } = &mut posed.source else {
            unreachable!()
        };
        std::sync::Arc::make_mut(seed).set_fixture_style(32);
        let mut original = posed.clone();
        original.source = ModelLightingSource::DiagnosedFallback {
            reason: ModelLightingFallbackReason::LegacyVertexLighting,
            snapshot: original.source.fallback_snapshot().clone(),
        };
        let contexts = [original];
        let overrides = [(0, posed)].into();
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance_overrides(&contexts, &overrides, true, &[1.0; 128], 0.0, &[true]);
        assert!(!state.histories[0].has_pending());
        let history = state.histories[0].clone();
        state.advance_overrides(&contexts, &overrides, false, &[1.0; 128], 1.0, &[true]);
        assert_ne!(
            state.histories[0], history,
            "effective runtime source must advance its history timestamp after settling"
        );
        let previous = state.data[1];
        let mut styles = [1.0; 128];
        styles[32] = 0.25;
        assert!(state.advance_overrides(&contexts, &overrides, false, &styles, 1.25, &[true]));
        assert_ne!(state.data[1], previous);
        let history = state.histories[0].clone();
        state.advance_overrides(&contexts, &overrides, false, &styles, 2.0, &[false]);
        assert_eq!(
            state.histories[0], history,
            "hidden effective sources must keep their last drawn history"
        );
        assert!(contexts[0].source.runtime_seed().is_none());
    }

    #[test]
    fn posed_context_overrides_update_at_a_fixed_capture_clock_without_mutating_the_scene() {
        use crate::media::preview_model::{ModelLightingSnapshot, ModelLightingSource};
        use gmpublished_domain::scene::map::AmbientCube;
        let mut context = ModelLightingContext::ambient_fixture(1.0);
        context.source = ModelLightingSource::Explicit(ModelLightingSnapshot {
            ambient_cube: AmbientCube::WHITE,
            lights: Vec::new(),
        });
        let contexts = [context];
        let mut state = State::new(&contexts, &[1.; 128]);
        state.advance(&contexts, &[1.; 128], 1., &[true]);
        let mut posed = contexts[0].clone();
        posed.source = ModelLightingSource::Explicit(ModelLightingSnapshot {
            ambient_cube: AmbientCube {
                colors: [gmpublished_domain::math::Vec3::ZERO; 6],
            },
            lights: Vec::new(),
        });
        let overrides = [(0, posed)].into();
        assert!(state.advance_overrides(&contexts, &overrides, true, &[1.; 128], 1., &[true]));
        assert_eq!(state.data[1].full.ambient[0][0], 0.);
        assert!(!state.advance_overrides(&contexts, &overrides, false, &[1.; 128], 1., &[true]));
        assert_eq!(
            contexts[0].source.fallback_snapshot().ambient_cube,
            AmbientCube::WHITE
        );
    }

    #[test]
    fn unused_styles_do_not_reencode_constant_contexts_but_reactivation_still_does() {
        let mut context = ModelLightingContext::ambient_fixture(1.0);
        context.entity = false;
        let mut contexts = [context];
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance(&contexts, &[1.0; 128], 0.0, &[true]);
        let mut styles = [1.0; 128];
        styles[2] = 0.0;
        assert!(!state.advance(&contexts, &styles, 0.1, &[true]));
        assert!(
            !state.advance_with(&contexts, &styles, 0.1, &[true], |_, _, _, _| {
                panic!("unused style triggered evaluation")
            })
        );
        state.advance(&contexts, &styles, 0.2, &[false]);
        contexts[0] = ModelLightingContext::ambient_fixture(0.0);
        contexts[0].entity = false;
        assert!(state.advance(&contexts, &styles, 0.3, &[true]));
        assert_eq!(state.data[1].full.ambient[0][0], 0.0);
    }

    #[test]
    fn hidden_clock_rewind_is_observed_even_when_only_unused_styles_change() {
        let contexts = [ModelLightingContext::ambient_fixture(1.0)];
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance(&contexts, &[1.0; 128], 10.0, &[true]);
        state.advance(&contexts, &[1.0; 128], 10.1, &[false]);
        let mut styles = [1.0; 128];
        styles[2] = 0.0;
        state.advance(&contexts, &styles, 11.0, &[false]);
        assert_ne!(state.histories[0], ModelLightingHistory::default());
        state.advance(&contexts, &styles, 10.5, &[false]);
        assert_eq!(state.histories[0], ModelLightingHistory::default());
    }

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
        assert!(state.data[1].full.ambient[0][0] > 0.0);
        assert!(state.data[1].full.ambient[0][0] < 1.0);
        state.advance(&contexts, &[1.0; 128], 2.0, &[true]);
        assert_eq!(state.data[1].full.ambient[0][0], 0.0);
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
        let previous = first.data[1].full.ambient[0][0];
        assert!(first.advance(&contexts, &[1.0; 128], 0.2, &[true]));
        assert!(first.data[1].full.ambient[0][0] < previous);
        second.advance(&contexts, &[1.0; 128], 0.2, &[true]);
        assert_eq!(second.data[1].full.ambient[0][0], 0.0);
        assert_ne!(first.histories, second.histories);
    }

    #[test]
    fn hidden_evaluations_are_never_invoked_and_initial_gpu_payload_is_retained() {
        let mut contexts = [
            ModelLightingContext::ambient_fixture(1.0),
            ModelLightingContext::ambient_fixture(2.0),
        ];
        contexts[1].model_path = "hidden-error".into();
        let mut state = State::new(&contexts, &[1.0; 128]);
        let hidden = state.data[2];
        for time in [0.0, 0.1, 0.2] {
            state.advance_with(
                &contexts,
                &[1.0; 128],
                time,
                &[true, false],
                |context, styles, time, history| {
                    assert_ne!(
                        context.model_path, "hidden-error",
                        "hidden evaluation would fail"
                    );
                    encode_context(context, styles, time, history)
                },
            );
            assert_eq!(state.data[2], hidden);
            assert_eq!(state.histories[1], ModelLightingHistory::default());
            assert_eq!(state.data[0], GpuModelContext::default());
        }
    }

    #[test]
    fn hidden_style_changes_are_observed_on_reactivation_without_touching_hidden_payloads() {
        use crate::media::preview_model::ModelLightingSource;
        let mut context = ModelLightingContext::ambient_fixture(1.0);
        context.entity = false;
        let ModelLightingSource::Runtime { seed, .. } = &mut context.source else {
            unreachable!()
        };
        std::sync::Arc::make_mut(seed).set_fixture_style(32);
        let mut supplement = seed.as_ref().clone();
        supplement.set_fixture_style(33);
        context.compiled_supplement = Some(Ok(std::sync::Arc::new(supplement)));
        let contexts = [context];
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance(&contexts, &[1.0; 128], 0.0, &[true]);
        let initial = state.data[1];
        let mut styles = [1.0; 128];
        styles[32] = 0.25;
        styles[33] = 0.5;
        assert!(
            !state.advance_with(&contexts, &styles, 1.0, &[false], |_, _, _, _| {
                panic!("hidden style change evaluated its context")
            })
        );
        assert_eq!(state.data[1], initial);
        assert!(state.advance(&contexts, &styles, 2.0, &[true]));
        assert_eq!(
            state.data[1],
            super::super::encode(&contexts, &styles, 2.0, None, None)[1]
        );
        assert_ne!(state.data[1].full, initial.full);
        assert_ne!(state.data[1].compiled_vertex, initial.compiled_vertex);
        assert_eq!(state.changed_ranges().len(), 1);
        assert_eq!(state.changed_ranges()[0], 1..2);
        styles[33] = 0.125;
        assert!(state.advance(&contexts, &styles, 2.1, &[true]));
        assert_eq!(
            state.data[1],
            super::super::encode(&contexts, &styles, 2.1, None, None)[1]
        );
    }

    #[test]
    fn changed_upload_ranges_exclude_default_hidden_and_unchanged_entries_and_coalesce_neighbors() {
        let mut contexts = (0..5)
            .map(|_| ModelLightingContext::ambient_fixture(1.0))
            .collect::<Vec<_>>();
        let mut state = State::new(&contexts, &[1.0; 128]);
        state.advance(&contexts, &[1.0; 128], 0.0, &[true; 5]);
        for context in &mut contexts {
            *context = ModelLightingContext::ambient_fixture(0.0);
        }
        assert!(state.advance(
            &contexts,
            &[1.0; 128],
            1.0,
            &[true, true, false, true, false]
        ));
        assert_eq!(state.changed_ranges(), [1..3, 4..5]);
        assert_eq!(state.data[3].full.ambient[0][0], 1.0);
        assert_eq!(state.data[5].full.ambient[0][0], 1.0);
        assert!(!state.advance(
            &contexts,
            &[1.0; 128],
            1.0,
            &[true, true, false, true, false]
        ));
        assert!(state.changed_ranges().is_empty());
        assert_eq!(std::mem::size_of::<GpuModelContext>() % 4, 0);
    }

    #[test]
    fn active_payloads_and_histories_match_full_encoding_across_visibility_and_clock_rewind() {
        let mut contexts = [
            ModelLightingContext::ambient_fixture(1.0),
            ModelLightingContext::ambient_fixture(0.25),
        ];
        let styles = [1.0; 128];
        let mut state = State::new(&contexts, &styles);
        let mut reference_histories = vec![ModelLightingHistory::default(); contexts.len()];
        let mut other_view = State::new(&contexts, &styles);
        for (step, (time, active)) in [
            (0.0, [true, false]),
            (0.1, [true, false]),
            (0.2, [false, true]),
            (0.2, [true, true]),
            (0.3, [true, false]),
            (0.05, [true, true]),
        ]
        .into_iter()
        .enumerate()
        {
            if step == 1 {
                contexts[0] = ModelLightingContext::ambient_fixture(0.0);
            }
            if state.initialized && time < state.time {
                reference_histories
                    .iter_mut()
                    .for_each(ModelLightingHistory::reset);
            }
            let expected = super::super::encode(
                &contexts,
                &styles,
                time,
                Some(&mut reference_histories),
                Some(&active),
            );
            state.advance(&contexts, &styles, time, &active);
            for (index, active) in active.into_iter().enumerate() {
                if active {
                    assert_eq!(state.data[index + 1], expected[index + 1]);
                }
            }
            assert_eq!(state.histories, reference_histories);
        }
        assert_eq!(
            other_view.histories,
            vec![ModelLightingHistory::default(); contexts.len()]
        );
        other_view.advance(&contexts, &styles, 0.05, &[true, true]);
        assert_eq!(other_view.data, state.data);
    }

    #[test]
    fn active_encoder_preserves_runtime_error_fallback_and_compiled_supplement_failure() {
        use crate::media::preview_model::ModelLightingSource;
        let mut context = ModelLightingContext::ambient_fixture(1.0);
        if let ModelLightingSource::Runtime { fallback, .. } = &mut context.source {
            fallback.ambient_cube.colors = [gmpublished_domain::math::Vec3::splat(0.125); 6];
        }
        let ModelLightingSource::Runtime { seed, .. } = &context.source else {
            unreachable!()
        };
        let mut failed_supplement = seed.as_ref().clone();
        failed_supplement.set_fixture_style(32);
        let mut invalid_styles = [1.0; 128];
        invalid_styles[32] = f32::NAN;
        context.compiled_supplement = Some(Err(failed_supplement
            .evaluate(&invalid_styles, 4, 0.0002)
            .unwrap_err()));
        let contexts = [context];
        let mut state = State::new(&contexts, &[1.0; 128]);
        assert!(!state.advance(&contexts, &[1.0; 128], f64::NAN, &[false]));
        assert_eq!(state.data[1].full.ambient[0][0], 1.0);
        assert!(state.advance(&contexts, &[1.0; 128], f64::NAN, &[true]));
        assert_eq!(state.data[1].full.ambient[0][0], 0.125);
        assert_eq!(
            state.data[1].compiled_vertex,
            super::super::GpuLightingState::default()
        );
    }
}
