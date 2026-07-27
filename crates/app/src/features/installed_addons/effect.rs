use super::model::{ContextMenuRequest, PreviewTarget};
use crate::bridge::domain::PublishedFileId;

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
    /// The grid re-anchored its scroll offset after hydration changed row
    /// heights above the viewport; the app must move the Iced scrollable by
    /// this relative delta so content does not jump under the user.
    GridScrollAnchored(f32),
    AddonDragPressed {
        card_id: String,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
