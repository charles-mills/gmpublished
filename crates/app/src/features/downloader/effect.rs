use std::path::PathBuf;

use super::jobs::DownloadPreviewTarget;
use crate::bridge::domain::PublishedFileId;
use crate::bridge::tasks::TaskId;

#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// A button on the prerequisite panel hosted in place of the job columns.
    PrerequisiteActivated(crate::features::prerequisites::Action),
}
