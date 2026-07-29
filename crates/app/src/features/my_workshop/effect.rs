use super::model::{ContextMenuRequest, PreparePublishTarget};
use crate::bridge::domain::PublishedFileId;
use crate::generation::Generation;
use crate::widgets::grid_rows::CardId;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    PageRequested {
        generation: Generation,
        page: u32,
    },
    StatsRefreshRequested {
        generation: Generation,
        pages: u32,
    },
    PreparePublishRequested(PreparePublishTarget),
    ContextMenuRequested(ContextMenuRequest),
    ThumbnailDemandsChanged,
    AddonDragPressed {
        card_id: CardId,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
