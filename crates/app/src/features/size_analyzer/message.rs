use std::collections::HashMap;

use iced::{Point, Size};

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::{LibraryRefreshReason, LibrarySnapshot};
use crate::bridge::ui_error::UiError;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    RouteEntered,
    RouteExited,
    ViewportResized(Size),
    SnapshotPushed(
        LibraryRefreshReason,
        Result<Option<LibrarySnapshot>, UiError>,
    ),
    ScaleFactorChanged,
    PreviewUrlsResolved(HashMap<PublishedFileId, String>),
    HoverMoved(Point),
    HoverExited,
    TreemapPressed,
    TreemapReleased,
    TreemapClicked,
    TreemapRightPressed(iced::Point),
}
