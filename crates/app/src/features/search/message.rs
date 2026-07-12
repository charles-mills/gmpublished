use super::state::{MetadataPatch, MetadataResolution};
use crate::backend::domain::{
    PublishedFileId, SearchFullBatch, SearchFullRequest, SearchMode, SearchQuickBatch,
    SearchQuickRequest, SearchRequestKey,
};
use crate::backend::ui_error::UiError;

#[derive(Clone, Debug)]
pub enum Message {
    QueryEdited(String),
    FocusRequested,
    ModeFocusRequested(SearchMode),
    DropdownScrolled(f32),
    QuickDebounced(SearchQuickRequest),
    QuickSearchCompleted(SearchRequestKey, Result<SearchQuickBatch, UiError>),
    FullSearchSubmitted,
    FullSearchBatchReceived(SearchFullBatch),
    FullSearchFinished(SearchFullRequest),
    MetadataCompleted(
        u64,
        Vec<PublishedFileId>,
        Result<MetadataResolution, UiError>,
    ),
    MetadataRefreshCompleted(
        u64,
        Vec<PublishedFileId>,
        Result<Vec<MetadataPatch>, UiError>,
    ),
    ResultActivated(usize),
    DismissRequested,
    EscapePressed,
}
