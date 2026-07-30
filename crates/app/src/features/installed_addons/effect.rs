use super::model::{ContextMenuRequest, PreviewTarget};
use crate::bridge::domain::PublishedFileId;
use crate::generation::Generation;
use crate::widgets::grid_rows::CardId;

/// Outward consequences of an Installed Addons state transition.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    MetadataRequested {
        generation: Generation,
        item_ids: Vec<PublishedFileId>,
    },
    MetadataRefreshRequested {
        generation: Generation,
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
        card_id: CardId,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
