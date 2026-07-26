use std::path::PathBuf;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::tasks::TaskEvent;

use super::model::{DownloaderEvent, RowId, Section};

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    CompactSectionSelected(Section),
    InputEdited(String),
    InputSubmitted,
    /// A direct list of Workshop IDs should be submitted by drag/drop or search handoff.
    WorkshopIdsSubmitted(Vec<PublishedFileId>),
    EventReceived(DownloaderEvent),
    TaskEventsReceived(Vec<TaskEvent>),
    CancelRequested {
        section: Section,
        row_id: RowId,
    },
    RemoveAllRequested(Section),
    OpenRequested {
        section: Section,
        row_id: RowId,
    },
    PreviewRequested {
        section: Section,
        row_id: RowId,
    },
    OpenWorkshopRequested(Option<PublishedFileId>),
    OpenAllRequested,
    BulkExtractRequested,
    BulkExtractPathsSelected(Vec<PathBuf>),
    DestinationRequested,
    DestinationLabelChanged(String),
    /// A button on the prerequisite panel this route renders in place of its
    /// job columns. Forwarded straight up: the Downloader has no opinion on
    /// Steam, it only hosts the panel.
    Prerequisite(crate::features::prerequisites::Action),
}
