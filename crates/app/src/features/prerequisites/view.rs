use iced::Element;

use crate::assets;
use crate::features::shell::Route;
use crate::theme::ViewCtx;
use crate::widgets::route_state::{self, Action as PanelAction, Glyph, RouteState, Tone};

use super::{Action, Blocker};

/// Renders the panel a blocked route shows instead of its content.
///
/// `on_action` lets each caller keep its own message type: the three fully
/// blocked routes map straight to the root message, while the Downloader
/// wraps it so its live input row survives above the panel.
pub fn view<'a, M: Clone + 'a>(
    ctx: ViewCtx<'a>,
    route: Route,
    blocker: &Blocker,
    spinner_elapsed: f32,
    on_action: impl Fn(Action) -> M,
) -> Element<'a, M> {
    let i18n = ctx.i18n;
    let panel = match blocker {
        Blocker::SteamConnecting => RouteState::new(
            Glyph::Spinner(spinner_elapsed),
            Tone::Busy,
            i18n.tr("steam-connecting-title"),
            i18n.tr("steam-connecting-body"),
        ),
        Blocker::SteamNotRunning { retrying } => {
            let retry = PanelAction::secondary(
                i18n.tr(if *retrying {
                    "steam-offline-checking"
                } else {
                    "steam-offline-retry"
                }),
                on_action(Action::RetrySteam),
            );
            RouteState::new(
                Glyph::Icon(assets::icons::cloud_off()),
                Tone::Warn,
                i18n.tr(steam_offline_title_key(route)),
                i18n.tr(steam_offline_body_key(route)),
            )
            .action(PanelAction::primary(
                i18n.tr("steam-offline-start"),
                on_action(Action::StartSteam),
            ))
            .action(if *retrying { retry.disabled() } else { retry })
        }
        Blocker::SteamNotInstalled => RouteState::new(
            Glyph::Icon(assets::icons::cloud_off()),
            Tone::Warn,
            i18n.tr("steam-missing-title"),
            i18n.tr("steam-missing-body"),
        )
        .action(PanelAction::primary(
            i18n.tr("steam-missing-action"),
            on_action(Action::GetSteam),
        )),
        Blocker::GameSearching => RouteState::new(
            Glyph::Spinner(spinner_elapsed),
            Tone::Busy,
            i18n.tr("gmod-searching-title"),
            i18n.tr("gmod-searching-body"),
        ),
        Blocker::GameMissing { can_install } => {
            let panel = RouteState::new(
                Glyph::Icon(assets::icons::folder_x()),
                Tone::Warn,
                i18n.tr("gmod-missing-title"),
                i18n.tr(gmod_missing_body_key(route)),
            );
            // Without a live client `steam://install/4000` goes nowhere, so
            // locating an existing copy becomes the primary action instead.
            if *can_install {
                panel
                    .action(PanelAction::primary(
                        i18n.tr("gmod-missing-install"),
                        on_action(Action::InstallGame),
                    ))
                    .action(PanelAction::secondary(
                        i18n.tr("gmod-missing-locate"),
                        on_action(Action::LocateGame),
                    ))
            } else {
                panel.action(PanelAction::primary(
                    i18n.tr("gmod-missing-locate"),
                    on_action(Action::LocateGame),
                ))
            }
        }
        Blocker::GameBroken { path } => RouteState::new(
            Glyph::Icon(assets::icons::folder_x()),
            Tone::Warn,
            i18n.tr("gmod-broken-title"),
            i18n.tr("gmod-broken-body"),
        )
        // The saved path is the fastest way to recognise what happened —
        // an unmounted drive reads instantly once you see the prefix.
        .detail(path.display().to_string())
        .action(PanelAction::primary(
            i18n.tr("gmod-missing-locate"),
            on_action(Action::LocateGame),
        ))
        .action(PanelAction::secondary(
            i18n.tr("gmod-broken-research"),
            on_action(Action::SearchGame),
        )),
    };

    route_state::view(panel, ctx)
}

/// The Downloader leads with the consequence rather than the cause: its
/// queue is what the user came for, and it survives the outage.
const fn steam_offline_title_key(route: Route) -> &'static str {
    match route {
        Route::Downloader => "steam-offline-title-downloads",
        _ => "steam-offline-title",
    }
}

const fn steam_offline_body_key(route: Route) -> &'static str {
    match route {
        Route::Downloader => "steam-offline-body-downloads",
        _ => "steam-offline-body-workshop",
    }
}

/// Only the clause naming what this screen wanted to do changes.
const fn gmod_missing_body_key(route: Route) -> &'static str {
    match route {
        Route::SizeAnalyzer => "gmod-missing-body-size",
        _ => "gmod-missing-body-addons",
    }
}
