use crate::widgets::addon_grid;
use std::time::Instant;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::LibraryRefreshReason;
use crate::bridge::ui_error::UiError;

use super::model::{MetadataPatch, MetadataResolution, Row};
use crate::generation::Generation;

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
        Generation,
        Vec<PublishedFileId>,
        Result<MetadataResolution, UiError>,
    ),
    /// A stale Workshop metadata refresh completed.
    ///
    /// The id list is the set the refresh covered, carried so a failure can put
    /// those ids back in line for another attempt — this is the leg that
    /// actually talks to the network, so it is the one that realistically
    /// fails. It is empty on the streaming success batches, which need no such
    /// bookkeeping.
    MetadataRefreshCompleted(
        Generation,
        Vec<PublishedFileId>,
        Result<Vec<MetadataPatch>, UiError>,
    ),
    /// Steam came up after being down. Unlike the Steam-gated routes, this one
    /// only has its rows *enriched* by Steam, so it is not re-entered on
    /// reconnect — but metadata lookups that failed while Steam was down are
    /// now answerable and get another go.
    SteamReconnected,
    /// The route-gated animation clock advanced.
    AnimationTick(Instant),
    Grid(addon_grid::Message),
}
