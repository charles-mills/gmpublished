use super::model::{ContextMenuRequest, PreviewTarget};
use crate::backend::domain::PublishedFileId;

/// Outward consequences of an Installed Addons state transition.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    MetadataRequested {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
    MetadataRefreshRequested {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
    PreviewRequested(PreviewTarget),
    ContextMenuRequested(ContextMenuRequest),
    ThumbnailDemandsChanged,
    AddonDragPressed {
        card_id: String,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
