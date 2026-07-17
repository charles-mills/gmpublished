use crate::widgets::addon_grid;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::LibraryRefreshReason;
use crate::bridge::ui_error::UiError;

use super::model::{MetadataPatch, MetadataResolution, Row};

/// Facts emitted by the Installed Addons route.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    /// The root library store started an event-driven refresh.
    LibraryRefreshStarted(LibraryRefreshReason),
    /// The live library watcher finished a best-effort arm attempt.
    WatchArmed {
        degraded: bool,
    },
    /// The root library store pushed a refreshed installed-addon snapshot.
    SnapshotPushed(LibraryRefreshReason, Result<Vec<Row>, UiError>),
    /// A visible-row Workshop metadata query completed.
    MetadataCompleted(
        u64,
        Vec<PublishedFileId>,
        Result<MetadataResolution, UiError>,
    ),
    /// A stale Workshop metadata refresh completed.
    MetadataRefreshCompleted(u64, Result<Vec<MetadataPatch>, UiError>),
    /// The route-gated animation clock advanced.
    AnimationTick(Instant),
    Grid(addon_grid::Message),
}
