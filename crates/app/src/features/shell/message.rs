use crate::features::steam_session::{ConnectionStatus, SteamIdentity};

use super::{Route, UpdateRelease};

/// Facts emitted by the shell chrome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Navigate(Route),
    SearchActivated,
    DragRegionPressed,
    DragRegionDoubleClicked,
    AccountRowHoverChanged(bool),
    AccountMenuToggled,
    /// The account menu should close because of Escape, click-away, or routing.
    AccountMenuDismissed,
    /// A newer release was found by the startup update check.
    UpdateReleaseFound(UpdateRelease),
    SteamStatusChanged(ConnectionStatus),
    SteamIdentityChanged(Option<SteamIdentity>),
    UpdateNagActivated,
    UpstreamRepoActivated,
    SettingsActivated,
    DownloaderJobCountChanged(u32),
    DownloaderDropTargetEntered,
    DownloaderDropTargetExited,
}
