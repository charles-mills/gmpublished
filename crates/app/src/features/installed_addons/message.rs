use crate::widgets::addon_grid;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::LibraryRefreshReason;
use crate::bridge::ui_error::UiError;

use super::model::{MetadataPatch, MetadataResolution, Row};
use crate::generation::Generation;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    WorkshopLanguageChanged,
    RouteExited,
    LibraryRefreshStarted(LibraryRefreshReason),
    WatchArmed {
        degraded: bool,
    },
    SnapshotPushed(LibraryRefreshReason, Result<Vec<Row>, UiError>),
    MetadataCompleted(
        Generation,
        Vec<PublishedFileId>,
        Result<MetadataResolution, UiError>,
    ),
    /// Metadata refresh completion. Failed ids are retained for retry;
    /// streaming success batches carry an empty id list.
    MetadataRefreshCompleted(
        Generation,
        Vec<PublishedFileId>,
        Result<Vec<MetadataPatch>, UiError>,
    ),
    /// Retry failed metadata queries on reconnect; this route is not re-entered with the
    /// Steam-gated routes.
    SteamReconnected,
    AnimationTick(Instant),
    Grid(addon_grid::Message),
}
