use super::state::{ContextMenuRequest, PreviewTarget};
use crate::backend::domain::PublishedFileId;

/// Outward consequences of a Size Analyzer state transition.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    PreviewUrlsResolveRequested(Vec<PublishedFileId>),
    PreviewRequested(PreviewTarget),
    ContextMenuRequested(ContextMenuRequest),
    ThumbnailDemandsChanged,
    AddonDragPressed {
        card_id: String,
        workshop_id: Option<PublishedFileId>,
    },
    AddonDragReleased,
}
