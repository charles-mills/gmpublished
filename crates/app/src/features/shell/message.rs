use crate::features::steam_session::{ConnectionStatus, SteamIdentity};

use super::{Route, UpdateRelease};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Navigate(Route),
    SearchActivated,
    DragRegionPressed,
    DragRegionDoubleClicked,
    AccountRowHoverChanged(bool),
    AccountMenuToggled,
    AccountMenuDismissed,
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
