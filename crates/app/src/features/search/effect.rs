use crate::bridge::{
    domain::{PublishedFileId, SearchQuickRequest},
    tasks::TaskId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    PaletteOpened,
    PaletteDismissed,
    FocusInputRequested,
    QuickSearchDebounceRequested(SearchQuickRequest),
    QuickSearchRequested(SearchQuickRequest),
    FullSearchRequested,
    MetadataRefreshRequested {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
    TaskCancellationRequested(TaskId),
    ResultActivated(usize),
    ThumbnailDemandsChanged,
}
