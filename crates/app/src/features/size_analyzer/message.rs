use std::collections::HashMap;

use iced::{Point, Size};

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::{LibraryRefreshReason, LibrarySnapshot};
use crate::bridge::ui_error::UiError;

/// Facts emitted by the Size Analyzer route.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    ViewportResized(Size),
    /// The shared installed-addon library produced a new snapshot.
    SnapshotPushed(
        LibraryRefreshReason,
        Result<Option<LibrarySnapshot>, UiError>,
    ),
    /// The window scale factor changed the label raster scale bucket.
    ScaleFactorChanged,
    /// Workshop preview URLs were resolved for current treemap leaves.
    PreviewUrlsResolved(HashMap<PublishedFileId, String>),
    HoverMoved(Point),
    HoverExited,
    TreemapPressed,
    TreemapReleased,
    /// The active treemap cell should open in Preview GMA.
    TreemapClicked,
    TreemapRightPressed(iced::Point),
}
