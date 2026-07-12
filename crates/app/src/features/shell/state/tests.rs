use std::time::{Duration, Instant};

use crate::backend::{
    TitlebarPreference,
    domain::{AvatarRgba, SteamUser},
};
use iced::widget::image;

use super::{
    ChromeStrategy, Route, State, UpdateRelease, sidebar_rail_width, sidebar_width,
    traffic_light_center_y, traffic_light_origin_x,
};

use crate::features::steam_session::{ConnectionStatus, SteamIdentity};
use crate::theme::Tokens;

#[test]
fn default_state_matches_shared_shell_startup_surface() {
    let state = State::default();

    assert_eq!(state.app_version(), env!("CARGO_PKG_VERSION"));
    assert_eq!(state.route(), Route::MyWorkshop);
    assert_eq!(state.account_name(), None);
    assert!(!state.update_available());
    assert_eq!(state.update_version(), "");
    assert_eq!(state.update_release_url(), "");
    assert_eq!(state.steam_status(), ConnectionStatus::Disconnected);
    assert!(!state.steam_status().connected());
    assert_eq!(state.steam_status().kind(), 0);
    assert_eq!(state.steam_status().translation_key(), "steam_disconnected");
    assert!(state.steam_avatar().is_none());
    assert_eq!(state.downloader_jobs(), 0);
    assert!(state.downloader_badge(Instant::now()).is_none());
    assert!(!state.account_menu_open());
}

#[test]
fn update_release_marks_update_row_available() {
    let mut state = State::default();

    state.apply_update_release(UpdateRelease::new(
        "v0.1.1".to_owned(),
        "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
    ));

    assert!(state.update_available());
    assert_eq!(state.update_version(), "v0.1.1");
    assert_eq!(
        state.update_release_url(),
        "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1"
    );
}

#[test]
fn downloader_badge_fades_out_before_unmounting() {
    let mut state = State::default();
    let started = Instant::now();

    state.set_downloader_jobs(3, started);
    assert!(state.needs_motion_ticks());

    let visible = state
        .downloader_badge(started + Duration::from_millis(50))
        .expect("badge should be mounted during fade in");
    assert_eq!(visible.count, 3);
    assert!(visible.opacity > 0.0 && visible.opacity < 1.0);

    let hide_started = started + Duration::from_millis(160);
    state.set_downloader_jobs(0, hide_started);
    assert!(state.needs_motion_ticks());

    let fading = state
        .downloader_badge(hide_started + Duration::from_millis(50))
        .expect("badge should stay mounted during fade out");
    assert_eq!(fading.count, 3);
    assert!(fading.opacity > 0.0 && fading.opacity < 1.0);

    let settled = hide_started + Duration::from_millis(130);
    assert!(state.needs_motion_ticks());
    state.tick_motion(settled);

    assert!(state.downloader_badge(settled).is_none());
    assert!(!state.needs_motion_ticks());
}

#[test]
fn steam_status_tracks_stable_status_fields() {
    let mut state = State::default();

    state.apply_steam_status(ConnectionStatus::Connecting);

    assert_eq!(state.steam_status(), ConnectionStatus::Connecting);
    assert!(!state.steam_status().connected());
    assert_eq!(state.steam_status().kind(), 1);
    assert_eq!(state.steam_status().translation_key(), "steam_connecting");

    state.apply_steam_status(ConnectionStatus::Connected);

    assert!(state.steam_status().connected());
    assert_eq!(state.steam_status().kind(), 2);
    assert_eq!(state.steam_status().translation_key(), "steam_connected");
}

#[test]
fn steam_identity_updates_account_name_and_avatar() {
    let mut state = State::default();

    state.apply_steam_identity(Some(steam_identity("Ada", Some(valid_avatar()))));

    assert_eq!(state.account_name(), Some("Ada"));

    let image::Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = state
        .steam_avatar()
        .expect("avatar should be converted")
        .clone()
    else {
        panic!("Steam avatar should use a decoded RGBA handle");
    };
    assert_eq!(width, 1);
    assert_eq!(height, 1);
    assert_eq!(pixels.as_ref(), &[1, 2, 3, 4]);
}

#[test]
fn missing_steam_identity_restores_anonymous_account() {
    let mut state = State::default();
    state.apply_steam_identity(Some(steam_identity("Ada", Some(valid_avatar()))));

    state.apply_steam_identity(None);

    assert_eq!(state.account_name(), None);
    assert!(state.steam_avatar().is_none());
}

#[test]
fn route_labels_cover_all_sidebar_items() {
    let expected = [
        "my-workshop",
        "installed-addons",
        "downloader",
        "size-analyzer",
    ];
    assert_eq!(Route::ALL.map(Route::label_key), expected);
}

#[test]
fn rail_width_uses_rail_width_token() {
    let tokens = Tokens::dark();

    assert_eq!(
        sidebar_rail_width(&tokens, ChromeStrategy::SystemDefault),
        tokens.dims.sidebar_rail_width
    );
}

#[test]
fn rail_width_uses_inset_rail_width_for_mac_native_inset() {
    let tokens = Tokens::dark();

    assert_eq!(
        sidebar_rail_width(&tokens, ChromeStrategy::MacNativeInset),
        tokens.dims.sidebar_rail_width_inset
    );
}

#[test]
fn sidebar_width_reserves_panel_and_float_margins() {
    let tokens = Tokens::dark();

    assert_eq!(
        sidebar_width(&tokens, ChromeStrategy::SystemDefault),
        tokens.dims.sidebar_rail_width + tokens.dims.sidebar_float_margin * 2.0
    );
    assert_eq!(
        sidebar_width(&tokens, ChromeStrategy::MacNativeInset),
        tokens.dims.sidebar_rail_width_inset + tokens.dims.sidebar_float_margin * 2.0
    );
}

#[test]
fn traffic_light_position_helpers_derive_from_sidebar_tokens() {
    let tokens = Tokens::dark();

    assert_eq!(
        traffic_light_origin_x(&tokens),
        tokens.dims.sidebar_float_margin + tokens.dims.sidebar_band_padding_x
    );
    assert_eq!(
        traffic_light_center_y(&tokens),
        tokens.dims.sidebar_float_margin + tokens.dims.sidebar_band_height / 2.0
    );
}

#[test]
fn chrome_strategy_resolves_from_titlebar_preference_and_platform() {
    assert_eq!(
        ChromeStrategy::resolve_for_platform(TitlebarPreference::Auto, true),
        ChromeStrategy::MacNativeInset
    );
    assert_eq!(
        ChromeStrategy::resolve_for_platform(TitlebarPreference::System, true),
        ChromeStrategy::SystemDefault
    );
    assert_eq!(
        ChromeStrategy::resolve_for_platform(TitlebarPreference::Auto, false),
        ChromeStrategy::SystemDefault
    );
}

#[test]
fn account_menu_open_close_transitions_are_state_local() {
    let mut state = State::default();
    let started = Instant::now();

    state.toggle_account_menu(started);
    assert!(state.account_menu_open());

    state.dismiss_account_menu(started + Duration::from_millis(1));
    assert!(!state.account_menu_open());
    assert!(state.account_menu_visible());

    state.tick_motion(started + Duration::from_millis(500));
    assert!(!state.account_menu_visible());

    state.toggle_account_menu(started + Duration::from_millis(600));
    state.select_route(Route::Downloader, started + Duration::from_millis(610));
    assert!(!state.account_menu_open());
}

fn steam_identity(name: &str, avatar: Option<AvatarRgba>) -> SteamIdentity {
    SteamIdentity::from_user(SteamUser {
        steamid: 76561198000000001,
        name: name.to_owned(),
        avatar,
        dead: false,
    })
}

fn valid_avatar() -> AvatarRgba {
    AvatarRgba::new(1, 1, vec![1, 2, 3, 4]).expect("test avatar should be valid")
}
