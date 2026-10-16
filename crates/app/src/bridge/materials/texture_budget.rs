use super::{HashMap, Mutex, TextureAssetError, TextureResidentId};

#[derive(Clone, Copy, Debug, Default)]
struct ResidentCost {
    bytes: usize,
    images: usize,
}

#[derive(Debug, Default)]
struct AdmissionState {
    cost: ResidentCost,
    admitted: HashMap<TextureResidentId, ResidentCost>,
    rejected: HashMap<TextureResidentId, TextureAssetError>,
}

#[derive(Debug)]
pub struct DecodedTextureBudget {
    limit: ResidentCost,
    state: Mutex<AdmissionState>,
}

impl DecodedTextureBudget {
    pub(crate) fn new(bytes: usize) -> Self {
        Self::with_image_limit(bytes, usize::MAX)
    }

    pub(crate) fn with_image_limit(bytes: usize, images: usize) -> Self {
        Self {
            limit: ResidentCost { bytes, images },
            state: Mutex::new(AdmissionState::default()),
        }
    }

    pub(crate) fn decoded_bytes(&self) -> usize {
        self.state.lock().cost.bytes
    }

    pub(crate) fn admitted_images(&self) -> usize {
        self.state.lock().cost.images
    }

    pub(crate) fn rejected_textures(&self) -> usize {
        self.state.lock().rejected.len()
    }

    pub(super) fn try_admit(
        &self,
        id: &TextureResidentId,
        bytes: usize,
        images: usize,
    ) -> Result<(), TextureAssetError> {
        let mut state = self.state.lock();
        if state.admitted.contains_key(id) {
            return Ok(());
        }
        if let Some(error) = state.rejected.get(id) {
            return Err(error.clone());
        }
        let total_bytes = state.cost.bytes.checked_add(bytes);
        let total_images = state.cost.images.checked_add(images);
        let error = if total_bytes.is_none_or(|total| total > self.limit.bytes) {
            Some(TextureAssetError::Budget)
        } else if total_images.is_none_or(|total| total > self.limit.images) {
            Some(TextureAssetError::ResidentImageBudget {
                required: total_images.unwrap_or(usize::MAX),
                capacity: self.limit.images,
            })
        } else {
            None
        };
        if let Some(error) = error {
            state.rejected.insert(id.clone(), error.clone());
            return Err(error);
        }
        state.cost = ResidentCost {
            bytes: total_bytes.expect("checked above"),
            images: total_images.expect("checked above"),
        };
        state
            .admitted
            .insert(id.clone(), ResidentCost { bytes, images });
        drop(state);
        Ok(())
    }

    pub(super) fn release(&self, id: &TextureResidentId) {
        let mut state = self.state.lock();
        if let Some(cost) = state.admitted.remove(id) {
            state.cost.bytes -= cost.bytes;
            state.cost.images -= cost.images;
        }
    }
}
