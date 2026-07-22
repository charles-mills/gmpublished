use super::model::{ContextMenuRequest, PreparePublishTarget};
use crate::bridge::domain::PublishedFileId;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    PageRequested {
        generation: u64,
        page: u32,
    },
    StatsRefreshRequested {
        generation: u64,
        pages: u32,
    },
    PreparePublishRequested(PreparePublishTarget),
    ContextMenuRequested(ContextMenuRequest),
    ThumbnailDemandsChanged,
    AddonDragPressed {
        card_id: String,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
