use std::time::Instant;

use crate::assets;
use crate::bridge::TitlebarPreference;
use crate::bridge::domain::AvatarRgba;
use iced::animation::Easing;
use iced::widget::{image, svg};

use crate::features::steam_session::{ConnectionStatus, SteamIdentity};
use crate::theme::{Tokens, motion};

/// Route identifiers owned by the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    MyWorkshop,
    InstalledAddons,
    Downloader,
    SizeAnalyzer,
}

impl Route {
    pub(crate) const ALL: [Self; 4] = [
        Self::MyWorkshop,
        Self::InstalledAddons,
        Self::Downloader,
        Self::SizeAnalyzer,
    ];

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::MyWorkshop => "my-workshop",
            Self::InstalledAddons => "installed-addons",
            Self::Downloader => "downloader",
            Self::SizeAnalyzer => "size-analyzer",
        }
    }

    pub(crate) fn icon(self) -> svg::Handle {
        match self {
            Self::MyWorkshop => assets::icons::route_my_workshop(),
            Self::InstalledAddons => assets::icons::route_installed_addons(),
            Self::Downloader => assets::icons::route_downloader(),
            Self::SizeAnalyzer => assets::icons::route_size_analyzer(),
        }
    }
}

/// Latest release metadata shown in the account menu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateRelease {
    version: String,
    url: String,
}

impl UpdateRelease {
    pub(crate) fn new(version: String, url: String) -> Self {
        Self { version, url }
    }
}

/// Effective native/window chrome treatment for the current platform/settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeStrategy {
    MacNativeInset,
    SystemDefault,
}

impl ChromeStrategy {
    pub(crate) fn resolve(preference: TitlebarPreference) -> Self {
        Self::resolve_for_platform(preference, cfg!(target_os = "macos"))
    }

    const fn resolve_for_platform(preference: TitlebarPreference, macos: bool) -> Self {
        if macos && matches!(preference, TitlebarPreference::Auto) {
            Self::MacNativeInset
        } else {
            Self::SystemDefault
        }
    }

    pub(crate) const fn mac_native_inset(self) -> bool {
        matches!(self, Self::MacNativeInset)
    }
}

pub fn sidebar_width(tokens: &Tokens, chrome_strategy: ChromeStrategy) -> f32 {
    sidebar_rail_width(tokens, chrome_strategy) + tokens.dims.sidebar_float_margin * 2.0
}

pub fn sidebar_rail_width(tokens: &Tokens, chrome_strategy: ChromeStrategy) -> f32 {
    match chrome_strategy {
        ChromeStrategy::MacNativeInset => tokens.dims.sidebar_rail_width_inset,
        ChromeStrategy::SystemDefault => tokens.dims.sidebar_rail_width,
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn traffic_light_origin_x(tokens: &Tokens) -> f32 {
    tokens.dims.sidebar_float_margin + tokens.dims.sidebar_band_padding_x
}

#[cfg(any(target_os = "macos", test))]
pub fn traffic_light_center_y(tokens: &Tokens) -> f32 {
    tokens.dims.sidebar_float_margin + tokens.dims.sidebar_band_height / 2.0
}

/// Root shell state owned by the Iced app.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "hover, drop-target, and menu open/visible flags are independent UI state, not mutually exclusive"
)]
pub struct State {
    app_version: &'static str,
    route: Route,
    update_nag: UpdateNag,
    steam: SteamStatus,
    account_name: Option<String>,
    downloader_jobs: u32,
    downloader_badge: BadgeMotion,
    downloader_drop_target_hovered: bool,
    account_row_hovered: bool,
    account_menu_open: bool,
    account_menu_visible: bool,
    account_menu_presence: motion::Presence<bool>,
}

impl Default for State {
    fn default() -> Self {
        let tokens = Tokens::dark();
        Self {
            app_version: app_version_text(),
            route: Route::MyWorkshop,
            update_nag: UpdateNag::default(),
            steam: SteamStatus::default(),
            account_name: None,
            downloader_jobs: 0,
            downloader_badge: BadgeMotion::default(),
            downloader_drop_target_hovered: false,
            account_row_hovered: false,
            account_menu_open: false,
            account_menu_visible: false,
            account_menu_presence: motion::asymmetric(
                false,
                tokens.motion.context_menu_enter_duration(),
                tokens.motion.context_menu_exit_duration(),
                Easing::EaseOut,
            ),
        }
    }
}

impl Eq for State {}

impl State {
    pub(crate) const fn app_version(&self) -> &'static str {
        self.app_version
    }

    pub(crate) const fn route(&self) -> Route {
        self.route
    }

    #[cfg(test)]
    pub(crate) fn account_name(&self) -> Option<&str> {
        self.account_name.as_deref()
    }

    pub(crate) const fn update_available(&self) -> bool {
        self.update_nag.available
    }

    pub(crate) fn update_version(&self) -> &str {
        &self.update_nag.version
    }

    pub(crate) fn update_release_url(&self) -> &str {
        &self.update_nag.url
    }

    pub(crate) const fn steam_status(&self) -> ConnectionStatus {
        self.steam.status
    }

    pub(crate) fn steam_avatar(&self) -> Option<&image::Handle> {
        self.steam.avatar.as_ref()
    }

    #[cfg(test)]
    pub(crate) const fn downloader_jobs(&self) -> u32 {
        self.downloader_jobs
    }

    pub(crate) fn downloader_badge(&self, now: Instant) -> Option<DownloaderBadge> {
        self.downloader_badge.render(now)
    }

    pub(crate) fn needs_motion_ticks(&self) -> bool {
        self.downloader_badge.needs_ticks() || self.account_menu_presence.needs_ticks()
    }

    pub(crate) const fn downloader_drop_target_hovered(&self) -> bool {
        self.downloader_drop_target_hovered
    }

    pub(crate) const fn account_menu_open(&self) -> bool {
        self.account_menu_open
    }

    pub(crate) const fn account_menu_visible(&self) -> bool {
        self.account_menu_visible
    }

    pub(crate) fn account_menu_opacity(&self, now: Instant) -> f32 {
        self.account_menu_presence.interpolate(0.0, 1.0, now)
    }

    pub(crate) fn account_menu_scale(&self, now: Instant) -> f32 {
        self.account_menu_presence
            .interpolate(motion::POPOVER_CLOSED_SCALE, 1.0, now)
    }

    pub(super) fn select_route(&mut self, route: Route, now: Instant) -> Option<(Route, Route)> {
        let previous = self.route;
        self.route = route;
        self.dismiss_account_menu(now);
        (previous != route).then_some((previous, route))
    }

    pub(crate) const fn account_row_hovered(&self) -> bool {
        self.account_row_hovered
    }

    pub(super) fn set_account_row_hovered(&mut self, hovered: bool) {
        self.account_row_hovered = hovered;
    }

    pub(super) fn toggle_account_menu(&mut self, now: Instant) {
        if self.account_menu_open {
            self.dismiss_account_menu(now);
        } else {
            self.account_menu_open = true;
            self.account_menu_visible = true;
            self.account_menu_presence.go(true, now);
        }
    }

    pub(super) fn dismiss_account_menu(&mut self, now: Instant) {
        if self.account_menu_open {
            self.account_menu_open = false;
            self.account_menu_presence.go(false, now);
        }
    }

    pub(super) const fn set_downloader_drop_target_hovered(&mut self, hovered: bool) {
        self.downloader_drop_target_hovered = hovered;
    }

    pub(super) fn set_downloader_jobs(&mut self, count: u32, now: Instant) {
        self.downloader_jobs = count;
        self.downloader_badge.set_count(count, now);
    }

    pub(crate) fn tick_motion(&mut self, now: Instant) {
        self.downloader_badge.tick(now);
        if self.account_menu_presence.tick(now)
            && !self.account_menu_open
            && self.account_menu_visible
        {
            self.account_menu_visible = false;
        }
    }

    pub(super) fn apply_update_release(&mut self, release: UpdateRelease) {
        self.update_nag.available = true;
        self.update_nag.version = release.version;
        self.update_nag.url = release.url;
    }

    pub(super) fn apply_steam_status(&mut self, status: ConnectionStatus) {
        self.steam.status = status;
        if !status.connected() {
            self.apply_anonymous_steam_identity();
        }
    }

    pub(super) fn apply_steam_identity(&mut self, identity: Option<SteamIdentity>) {
        let Some(identity) = identity else {
            self.apply_anonymous_steam_identity();
            return;
        };

        let name = identity.name().trim();
        self.account_name = (!name.is_empty()).then(|| name.to_owned());
        self.steam.avatar = identity.avatar().and_then(avatar_handle_from_rgba);
    }

    fn apply_anonymous_steam_identity(&mut self) {
        self.account_name = None;
        self.steam.avatar = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DownloaderBadge {
    pub(crate) count: u32,
    pub(crate) opacity: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BadgeMotion {
    count: u32,
    visible: bool,
    opacity: motion::Presence<bool>,
}

impl Default for BadgeMotion {
    fn default() -> Self {
        Self {
            count: 0,
            visible: false,
            opacity: motion::boolean(
                false,
                Tokens::dark().motion.fast_duration(),
                Easing::EaseInOut,
            ),
        }
    }
}

impl BadgeMotion {
    fn set_count(&mut self, count: u32, now: Instant) {
        if count > 0 {
            self.count = count;
        }

        let visible = count > 0;
        if self.visible != visible {
            self.visible = visible;
            self.opacity.go(visible, now);
        } else if !visible {
            self.count = 0;
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.opacity.tick(now) && !self.visible {
            self.count = 0;
        }
    }

    fn render(&self, now: Instant) -> Option<DownloaderBadge> {
        (self.count > 0).then(|| DownloaderBadge {
            count: self.count,
            opacity: self.opacity.interpolate(0.0, 1.0, now),
        })
    }

    fn needs_ticks(&self) -> bool {
        self.opacity.needs_ticks()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct UpdateNag {
    available: bool,
    version: String,
    url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamStatus {
    status: ConnectionStatus,
    avatar: Option<image::Handle>,
}

impl Default for SteamStatus {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            avatar: None,
        }
    }
}

fn avatar_handle_from_rgba(avatar: &AvatarRgba) -> Option<image::Handle> {
    let expected = expected_rgba_len(avatar.width, avatar.height)?;
    if avatar.rgba.len() != expected {
        log::warn!(
            "current Steam user avatar has {} bytes, expected {expected}",
            avatar.rgba.len()
        );
        return None;
    }

    Some(image::Handle::from_rgba(
        avatar.width,
        avatar.height,
        avatar.rgba.as_ref().to_vec(),
    ))
}

fn expected_rgba_len(width: u32, height: u32) -> Option<usize> {
    width
        .checked_mul(height)?
        .checked_mul(4)
        .and_then(|len| usize::try_from(len).ok())
}

const fn app_version_text() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests;
