use std::path::PathBuf;

use super::model::DownloadPreviewTarget;
use crate::backend::domain::PublishedFileId;
use crate::backend::tasks::TaskId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    WorkshopSubmissionAccepted(Vec<PublishedFileId>),
    TaskCancellationRequested(Vec<TaskId>),
    /// Stop in-flight submission batches from queueing further downloads
    /// (items still resolving have no task to cancel yet).
    DownloadQueueCancellationRequested,
    PathsOpenRequested(Vec<PathBuf>),
    PreviewRequested(DownloadPreviewTarget),
    WorkshopPageOpenRequested(Option<PublishedFileId>),
    BulkExtractPickerRequested,
    LocalExtractionRequested(Vec<PathBuf>),
    DestinationSelectionRequested,
    WorkshopTitleQueryRequested(Vec<PublishedFileId>),
    ActiveJobCountChanged(u32),
}
