use crate::bridge::{
    domain::{PublishedFileId, SearchQuickRequest},
    tasks::TaskId,
};
use crate::generation::Generation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    PaletteOpened,
    PaletteDismissed,
    FocusInputRequested,
    QuickSearchDebounceRequested(SearchQuickRequest),
    QuickSearchRequested(SearchQuickRequest),
    FullSearchRequested,
    MetadataRefreshRequested {
        generation: Generation,
        item_ids: Vec<PublishedFileId>,
    },
    TaskCancellationRequested(TaskId),
    ResultActivated(usize),
    ThumbnailDemandsChanged,
}
