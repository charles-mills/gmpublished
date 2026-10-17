use super::{MapPreview, model_lighting, wgpu};

#[derive(Debug)]
pub struct Frame {
    pub model_lights: wgpu::Buffer,
    model: model_lighting::State,
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
        #[cfg(feature = "map-capture")] observer: Option<
            &super::super::capture::CaptureDiagnostics,
        >,
    ) {
        #[cfg(feature = "map-capture")]
        let _model_span = observer
            .and_then(|observer| observer.span(super::super::capture::Phase::ModelLightingUpdate));
        if self.model.advance(
            map.map_or(&[], |map| map.model_lighting.as_slice()),
            &styles(map, time),
            time,
            active_contexts,
        ) {
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
