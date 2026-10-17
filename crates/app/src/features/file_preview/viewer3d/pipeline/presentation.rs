use super::{RenderResources, SceneKey, ViewId, passes, screen_refraction, wgpu};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Readiness {
    Current,
    Previous,
    Loading,
    Retiring,
    Rejected,
}
impl Readiness {
    pub fn can_render(self) -> bool {
        matches!(self, Self::Current | Self::Previous)
    }
    pub fn can_retain(self) -> bool {
        matches!(self, Self::Loading | Self::Retiring)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OutputKey {
    pub view: ViewId,
    pub scene: SceneKey,
    pub size: (u32, u32),
}

#[derive(Debug)]
pub(super) struct ViewOutput {
    blit: wgpu::BindGroup,
    size: (u32, u32),
    overlay: Option<screen_refraction::Snapshot>,
    frozen_overlay: OnceLock<screen_refraction::Frame>,
}
impl ViewOutput {
    pub fn matches(&self, composition: passes::Composition<'_>) -> bool {
        self.blit == *composition.blit
            && self.size == composition.size
            && match (&self.overlay, composition.overlay) {
                (None, None) => true,
                (Some(snapshot), Some(frame)) => snapshot.matches(frame),
                _ => false,
            }
    }
    pub fn new(composition: passes::Composition<'_>) -> Self {
        Self {
            blit: composition.blit.clone(),
            size: composition.size,
            overlay: composition.overlay.map(screen_refraction::Frame::snapshot),
            frozen_overlay: OnceLock::new(),
        }
    }
    pub fn has_material_image(&self) -> bool {
        self.overlay.is_some()
    }
    pub fn prepare(&self, device: &wgpu::Device, queue: &wgpu::Queue, resources: &RenderResources) {
        if let Some(overlay) = &self.overlay {
            self.frozen_overlay
                .get_or_init(|| overlay.freeze(device, queue, resources));
        }
    }
    pub fn composition(&self) -> passes::Composition<'_> {
        passes::Composition {
            blit: &self.blit,
            size: self.size,
            overlay: self.frozen_overlay.get(),
        }
    }
}

#[derive(Debug)]
pub(super) struct SubmissionHistory<K, T> {
    current: Option<(K, T)>,
    encoded: Mutex<Option<(K, T)>>,
}
impl<K, T> Default for SubmissionHistory<K, T> {
    fn default() -> Self {
        Self {
            current: None,
            encoded: Mutex::new(None),
        }
    }
}
impl<K: PartialEq, T> SubmissionHistory<K, T> {
    pub fn begin(&mut self) {
        *self.encoded.get_mut().expect("presentation lock") = None;
    }
    pub fn validate(&mut self, key: &K) {
        if self
            .current
            .as_ref()
            .is_some_and(|(current, _)| current != key)
        {
            self.clear();
        }
    }
    pub fn clear(&mut self) {
        self.current = None;
        self.begin();
    }
    pub fn stage(&self, key: K, output: T) {
        *self.encoded.lock().expect("presentation lock") = Some((key, output));
    }
    pub fn submitted(&mut self, retain: impl FnOnce(&T)) {
        if let Some(output) = self.encoded.get_mut().expect("presentation lock").take() {
            retain(&output.1);
            self.current = Some(output);
        }
    }
    pub fn get(&self, key: &K) -> Option<&T> {
        self.current
            .as_ref()
            .filter(|(current, _)| current == key)
            .map(|(_, output)| output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn only_submitted_output_replaces_history_and_cancelled_encoding_releases_ownership() {
        let mut history = SubmissionHistory::default();
        let old = Arc::new(17);
        history.stage(1, Arc::clone(&old));
        assert!(history.get(&1).is_none());
        history.submitted(|_| {});
        assert_eq!(history.get(&1).map(Arc::as_ref), Some(&17));
        let cancelled = Arc::new(29);
        history.stage(1, Arc::clone(&cancelled));
        history.begin();
        assert_eq!(Arc::strong_count(&cancelled), 1);
        assert_eq!(history.get(&1).map(Arc::as_ref), Some(&17));
        history.stage(1, Arc::clone(&cancelled));
        history.submitted(|_| {});
        assert_eq!(Arc::strong_count(&old), 1);
        assert_eq!(history.get(&1).map(Arc::as_ref), Some(&29));
        history.validate(&2);
        assert!(history.get(&1).is_none());
        assert_eq!(Arc::strong_count(&cancelled), 1);
    }

    #[test]
    fn output_identity_includes_view_scene_and_exact_pixel_extent() {
        let key = OutputKey {
            view: ViewId::default(),
            scene: SceneKey::Model(1),
            size: (320, 180),
        };
        for other in [
            OutputKey {
                view: ViewId::default(),
                ..key
            },
            OutputKey {
                scene: SceneKey::Map(1),
                ..key
            },
            OutputKey {
                size: (640, 360),
                ..key
            },
        ] {
            let mut history = SubmissionHistory::default();
            history.stage(key, 17);
            history.submitted(|_| {});
            assert!(history.get(&other).is_none());
            history.validate(&other);
            assert!(history.get(&key).is_none());
        }
    }
}
