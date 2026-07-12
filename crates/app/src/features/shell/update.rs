use std::time::Instant;

use crate::theme::Tokens;

use super::{ChromeStrategy, Effect, Message, State, UPSTREAM_REPO_URL};

/// Applies a shell message and returns outward effects as plain data.
pub fn update(
    state: &mut State,
    message: Message,
    _tokens: &Tokens,
    _chrome_strategy: ChromeStrategy,
) -> Vec<Effect> {
    match message {
        Message::Navigate(route) => state
            .select_route(route, Instant::now())
            .map(|(from, to)| Effect::Navigated { from, to })
            .into_iter()
            .collect(),
        Message::SearchActivated => {
            state.dismiss_account_menu(Instant::now());
            vec![Effect::OpenSearchPalette]
        }
        Message::AccountMenuDismissed => {
            state.dismiss_account_menu(Instant::now());
            Vec::new()
        }
        Message::DragRegionPressed => vec![Effect::BeginWindowDrag],
        Message::DragRegionDoubleClicked => vec![Effect::ToggleMaximize],
        Message::AccountRowHoverChanged(hovered) => {
            state.set_account_row_hovered(hovered);
            Vec::new()
        }
        Message::AccountMenuToggled => {
            state.toggle_account_menu(Instant::now());
            Vec::new()
        }
        Message::UpdateReleaseFound(release) => {
            state.apply_update_release(release);
            Vec::new()
        }
        Message::SteamStatusChanged(status) => {
            state.apply_steam_status(status);
            Vec::new()
        }
        Message::SteamIdentityChanged(identity) => {
            state.apply_steam_identity(identity);
            Vec::new()
        }
        Message::UpdateNagActivated => {
            state.dismiss_account_menu(Instant::now());
            let url = state.update_release_url();
            if url.is_empty() {
                Vec::new()
            } else {
                vec![Effect::OpenUrl(url.to_owned())]
            }
        }
        Message::UpstreamRepoActivated => {
            state.dismiss_account_menu(Instant::now());
            vec![Effect::OpenUrl(UPSTREAM_REPO_URL.to_owned())]
        }
        Message::SettingsActivated => {
            state.dismiss_account_menu(Instant::now());
            vec![Effect::OpenSettings]
        }
        Message::DownloaderJobCountChanged(count) => {
            state.set_downloader_jobs(count, Instant::now());
            Vec::new()
        }
        Message::DownloaderDropTargetEntered => {
            state.set_downloader_drop_target_hovered(true);
            Vec::new()
        }
        Message::DownloaderDropTargetExited => {
            state.set_downloader_drop_target_hovered(false);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::domain::{AvatarRgba, SteamUser};

    use super::{Message, State, update};
    use crate::{
        features::{
            shell::{ChromeStrategy, Effect, Route, UPSTREAM_REPO_URL, UpdateRelease},
            steam_session::{ConnectionStatus, SteamIdentity},
        },
        theme::Tokens,
    };

    #[test]
    fn account_row_hover_tracks_and_clears_explicitly() {
        let mut state = State::default();

        assert!(update_dark(&mut state, Message::AccountRowHoverChanged(true)).is_empty());
        assert!(state.account_row_hovered());

        assert!(update_dark(&mut state, Message::AccountRowHoverChanged(false)).is_empty());
        assert!(!state.account_row_hovered());
    }

    #[test]
    fn route_selection_updates_current_route_and_closes_account_menu() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::Navigate(Route::SizeAnalyzer));

        assert_eq!(state.route(), Route::SizeAnalyzer);
        assert!(!state.account_menu_open());
        assert_eq!(
            effects,
            vec![Effect::Navigated {
                from: Route::MyWorkshop,
                to: Route::SizeAnalyzer,
            }]
        );
    }

    #[test]
    fn same_route_selection_closes_account_menu_without_navigation_effect() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::Navigate(Route::MyWorkshop));

        assert_eq!(state.route(), Route::MyWorkshop);
        assert!(!state.account_menu_open());
        assert!(effects.is_empty());
    }

    #[test]
    fn account_menu_messages_toggle_and_dismiss_popover() {
        let mut state = State::default();

        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());
        assert!(state.account_menu_open());

        assert!(update_dark(&mut state, Message::AccountMenuDismissed).is_empty());
        assert!(!state.account_menu_open());
    }

    #[test]
    fn search_activation_dismisses_menu_and_emits_open_search_effect() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::SearchActivated);

        assert!(!state.account_menu_open());
        assert_eq!(effects, vec![Effect::OpenSearchPalette]);
    }

    #[test]
    fn update_release_message_updates_the_nag() {
        let mut state = State::default();

        assert!(
            update(
                &mut state,
                Message::UpdateReleaseFound(UpdateRelease::new(
                    "v0.1.1".to_owned(),
                    "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
                )),
                &Tokens::dark(),
                strategy(),
            )
            .is_empty()
        );

        assert!(state.update_available());
        assert_eq!(state.update_version(), "v0.1.1");
        assert_eq!(
            state.update_release_url(),
            "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1"
        );
    }

    #[test]
    fn steam_status_message_updates_the_account_status() {
        let mut state = State::default();

        assert!(
            update(
                &mut state,
                Message::SteamStatusChanged(ConnectionStatus::Unavailable),
                &Tokens::dark(),
                strategy(),
            )
            .is_empty()
        );

        assert_eq!(state.steam_status(), ConnectionStatus::Unavailable);
        assert_eq!(state.steam_status().kind(), 3);
        assert_eq!(state.steam_status().translation_key(), "steam_unavailable");
    }

    #[test]
    fn steam_identity_message_updates_named_account() {
        let mut state = State::default();

        assert!(
            update(
                &mut state,
                Message::SteamIdentityChanged(Some(steam_identity("Ada"))),
                &Tokens::dark(),
                strategy(),
            )
            .is_empty()
        );

        assert_eq!(state.account_name(), Some("Ada"));
        assert!(state.steam_avatar().is_some());
    }

    #[test]
    fn settings_activation_dismisses_menu_and_emits_open_settings_effect() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::SettingsActivated);

        assert!(!state.account_menu_open());
        assert_eq!(effects, vec![Effect::OpenSettings]);
    }

    #[test]
    fn update_nag_activation_emits_release_url_effect() {
        let mut state = State::default();
        let release_url = "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1";
        assert!(
            update(
                &mut state,
                Message::UpdateReleaseFound(UpdateRelease::new(
                    "v0.1.1".to_owned(),
                    release_url.to_owned(),
                )),
                &Tokens::dark(),
                strategy(),
            )
            .is_empty()
        );
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::UpdateNagActivated);

        assert!(!state.account_menu_open());
        assert_eq!(effects, vec![Effect::OpenUrl(release_url.to_owned())]);
    }

    #[test]
    fn update_nag_activation_without_release_url_emits_nothing() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::UpdateNagActivated);

        assert!(!state.account_menu_open());
        assert!(effects.is_empty());
    }

    #[test]
    fn upstream_repo_activation_emits_upstream_url_effect() {
        let mut state = State::default();
        assert!(update_dark(&mut state, Message::AccountMenuToggled).is_empty());

        let effects = update_dark(&mut state, Message::UpstreamRepoActivated);

        assert!(!state.account_menu_open());
        assert_eq!(effects, vec![Effect::OpenUrl(UPSTREAM_REPO_URL.to_owned())]);
    }

    #[test]
    fn native_drag_region_messages_emit_window_effects() {
        let mut state = State::default();

        assert_eq!(
            update_dark(&mut state, Message::DragRegionPressed),
            vec![Effect::BeginWindowDrag]
        );
        assert_eq!(
            update_dark(&mut state, Message::DragRegionDoubleClicked),
            vec![Effect::ToggleMaximize]
        );
    }

    #[test]
    fn downloader_job_count_message_updates_badge_motion() {
        let mut state = State::default();

        assert!(
            update(
                &mut state,
                Message::DownloaderJobCountChanged(2),
                &Tokens::dark(),
                strategy(),
            )
            .is_empty()
        );

        assert_eq!(state.downloader_jobs(), 2);
        assert!(state.downloader_badge(std::time::Instant::now()).is_some());
    }

    fn steam_identity(name: &str) -> SteamIdentity {
        SteamIdentity::from_user(SteamUser {
            steamid: 76561198000000001,
            name: name.to_owned(),
            avatar: Some(
                AvatarRgba::new(1, 1, vec![1, 2, 3, 4]).expect("test avatar should be valid"),
            ),
            dead: false,
        })
    }

    fn update_dark(state: &mut State, message: Message) -> Vec<Effect> {
        update(state, message, &Tokens::dark(), strategy())
    }

    const fn strategy() -> ChromeStrategy {
        ChromeStrategy::SystemDefault
    }
}
