use super::state::{ContextMenuRequest, PreviewTarget};
use crate::bridge::domain::PublishedFileId;
use crate::widgets::grid_rows::CardId;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    PreviewUrlsResolveRequested(Vec<PublishedFileId>),
    PreviewRequested(PreviewTarget),
    ContextMenuRequested(ContextMenuRequest),
    ThumbnailDemandsChanged,
    AddonDragPressed {
        card_id: CardId,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
