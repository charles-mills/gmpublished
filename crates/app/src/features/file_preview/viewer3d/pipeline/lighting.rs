use super::{MapPreview, model_lighting, wgpu};

#[derive(Debug)]
pub struct Frame {
    pub model_lights: wgpu::Buffer,
    model: model_lighting::State,
    animated_contexts:
        std::collections::BTreeMap<usize, crate::media::preview_model::ModelLightingContext>,
    dynamic_cache: crate::media::preview_model::ModelLightingDynamicCache,
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
        }
    }

    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        map: Option<&MapPreview>,
        time: f64,
        active_contexts: &[bool],
        animated_origins: &std::collections::BTreeMap<usize, super::Vec3>,
    ) {
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
