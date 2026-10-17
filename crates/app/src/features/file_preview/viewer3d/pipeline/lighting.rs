use super::{MapPreview, model_lighting, wgpu};

#[derive(Debug)]
pub struct Frame {
    pub model_lights: wgpu::Buffer,
    model: model_lighting::State,
    animated_contexts:
        std::collections::BTreeMap<usize, crate::media::preview_model::ModelLightingContext>,
    dynamic_cache: crate::media::preview_model::ModelLightingDynamicCache,
    #[cfg(feature = "map-capture")]
    capture_timeline: Option<std::sync::Arc<super::super::capture::ModelLightingTimeline>>,
}

impl Frame {
    pub(super) fn new(device: &wgpu::Device, map: Option<&MapPreview>) -> Self {
        let model = model_lighting::State::new(
            map.map_or(&[], |map| map.model_lighting.as_slice()),
            &styles(map, 0.0),
        );
        Self {
            model_lights: model_lighting::upload(device, model.data()),
            model,
            animated_contexts: std::collections::BTreeMap::new(),
            dynamic_cache: Default::default(),
            #[cfg(feature = "map-capture")]
            capture_timeline: None,
        }
    }

    #[cfg(feature = "map-capture")]
    pub fn apply_capture_timeline(
        &mut self,
        map: Option<&MapPreview>,
        timeline: Option<&std::sync::Arc<super::super::capture::ModelLightingTimeline>>,
    ) {
        if self
            .capture_timeline
            .as_ref()
            .zip(timeline)
            .is_some_and(|(old, new)| std::sync::Arc::ptr_eq(old, new))
        {
            return;
        }
        if self.capture_timeline.is_some() || timeline.is_some() {
            self.model.reset_history();
        }
        if let Some(timeline) = timeline {
            let map = map.expect("native model timeline needs a map");
            let matches = map
                .model_lighting
                .iter()
                .enumerate()
                .filter(|(_, context)| {
                    context.entity
                        && context.model_path == timeline.model
                        && context.origin.to_array() == timeline.origin
                })
                .collect::<Vec<_>>();
            assert_eq!(
                matches.len(),
                1,
                "native history must identify exactly one renderable"
            );
            let (index, context) = matches[0];
            for &time in &timeline.draw_times_before_capture {
                self.model
                    .replay(index, context, &styles(Some(map), time), time);
            }
        }
        self.capture_timeline = timeline.cloned();
    }

    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        map: Option<&MapPreview>,
        time: f64,
        active_contexts: &[bool],
        animated_origins: &std::collections::BTreeMap<usize, super::Vec3>,
        #[cfg(feature = "map-capture")] observer: Option<
            &super::super::capture::CaptureDiagnostics,
        >,
    ) {
        #[cfg(feature = "map-capture")]
        let _model_span = observer
            .and_then(|observer| observer.span(super::super::capture::Phase::ModelLightingUpdate));
        let contexts = map.map_or(&[][..], |map| map.model_lighting.as_slice());
        let origins_changed = map
            .and_then(|map| map.model_lighting_inputs.as_ref())
            .is_some_and(|inputs| {
                inputs.update_animated_origins(
                    contexts,
                    animated_origins,
                    &mut self.animated_contexts,
                    &mut self.dynamic_cache,
                )
            });
        let styles = styles(map, time);
        let changed = if self.animated_contexts.is_empty() && !origins_changed {
            self.model.advance(contexts, &styles, time, active_contexts)
        } else {
            self.model.advance_overrides(
                contexts,
                &self.animated_contexts,
                origins_changed,
                &styles,
                time,
                active_contexts,
            )
        };
        if changed {
            for range in self.model.changed_ranges() {
                queue.write_buffer(
                    &self.model_lights,
                    (range.start * std::mem::size_of::<model_lighting::GpuModelContext>()) as u64,
                    bytemuck::cast_slice(&self.model.data()[range.clone()]),
                );
            }
        }
    }
}

fn styles(map: Option<&MapPreview>, time: f64) -> [f32; 128] {
    map.and_then(|map| map.model_lighting_inputs.as_ref())
        .map_or([1.0; 128], |inputs| inputs.style_values(time))
}
