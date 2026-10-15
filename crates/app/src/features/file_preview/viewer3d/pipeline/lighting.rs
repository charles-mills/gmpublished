//! One lighting snapshot shared by a viewer's main and optical scene views.
use super::{MapPreview, RenderResources, lightmaps, model_lighting, wgpu};
use crate::media::preview_model::ModelLightingHistory;

#[derive(Debug)]
pub struct Frame {
    pub lightmap: lightmaps::Frame,
    pub model_lights: wgpu::Buffer,
    model_styles: [f32; 128],
    model_data: Vec<model_lighting::GpuModelContext>,
    model_history: Vec<ModelLightingHistory>,
    model_initialized: bool,
    model_time: f64,
    model_active: Vec<bool>,
    #[cfg(feature = "map-capture")]
    capture_timeline: Option<std::sync::Arc<super::super::capture::ModelLightingTimeline>>,
}

impl Frame {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        map: Option<&MapPreview>,
    ) -> Self {
        let model_styles = styles(map, 0.0);
        let model_data = model_lighting::encode(
            map.map_or(&[], |map| map.model_lighting.as_slice()),
            &model_styles,
            0.0,
            None,
            None,
        );
        Self {
            lightmap: lightmaps::Frame::new(
                device,
                queue,
                resources,
                map.and_then(|map| map.lightmap.as_ref()),
            ),
            model_lights: model_lighting::upload(device, &model_data),
            model_styles,
            model_data,
            model_history: vec![
                ModelLightingHistory::default();
                map.map_or(0, |map| map.model_lighting.len())
            ],
            model_initialized: false,
            model_time: 0.0,
            model_active: vec![false; map.map_or(0, |map| map.model_lighting.len())],
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
            for history in &mut self.model_history {
                history.reset();
            }
            self.model_initialized = false;
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
                context
                    .evaluate_with_history(
                        &styles(Some(map), time),
                        time,
                        &mut self.model_history[index],
                    )
                    .expect("native history needs a valid production lighting seed");
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
    ) {
        assert_eq!(
            self.model_history.len(),
            active_contexts.len(),
            "lighting visibility context identity changed"
        );
        let active_changed = self.model_active != active_contexts;
        self.lightmap.update(
            queue,
            map.and_then(|map| map.lightmap.as_ref()),
            time as f32,
        );
        let next = styles(map, time);
        if self.model_initialized && time < self.model_time {
            for history in &mut self.model_history {
                history.reset();
            }
            self.model_initialized = false;
        }
        if self.model_initialized
            && time == self.model_time
            && next == self.model_styles
            && !active_changed
        {
            return;
        }
        let dynamic = map.is_some_and(|map| {
            map.model_lighting
                .iter()
                .enumerate()
                .any(|(index, context)| {
                    active_contexts[index]
                        && context.entity
                        && matches!(context.cpu_seed, Ok(Some(_)))
                })
        });
        if self.model_initialized
            && !dynamic
            && !active_changed
            && next == self.model_styles
            && !self
                .model_history
                .iter()
                .zip(active_contexts)
                .any(|(history, active)| *active && history.has_pending())
        {
            return;
        }
        let data = model_lighting::encode(
            map.map_or(&[], |map| map.model_lighting.as_slice()),
            &next,
            time,
            Some(&mut self.model_history),
            Some(active_contexts),
        );
        assert_eq!(
            data.len(),
            self.model_data.len(),
            "frame lighting content changed without reconstruction"
        );
        if data != self.model_data {
            queue.write_buffer(&self.model_lights, 0, bytemuck::cast_slice(&data));
            self.model_data = data;
        }
        self.model_styles = next;
        self.model_initialized = true;
        self.model_time = time;
        self.model_active.clone_from_slice(active_contexts);
    }
}

fn styles(map: Option<&MapPreview>, time: f64) -> [f32; 128] {
    map.and_then(|map| map.model_lighting_inputs.as_ref())
        .map_or([1.0; 128], |inputs| inputs.style_values(time))
}
