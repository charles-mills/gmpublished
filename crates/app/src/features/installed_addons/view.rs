use iced::widget::{column, row, text};
use iced::{Color, Element, Length};

use crate::assets;
use crate::i18n::{Arg, I18n, translated_error};
use crate::theme::ViewCtx;
use crate::widgets::addon_grid;
use crate::widgets::route_state::{self, Glyph, RouteState, Tone};

use super::state::Library;
use super::{Message, State};

pub const GRID_KEY: &str = "installed-addons-grid";

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = ctx.tokens;
    let title = row![
        text(ctx.i18n.tr("installed-addons"))
            .size(tokens.typography.title_lg)
            .font(crate::theme::styles::inter_font(
                iced::font::Weight::Semibold
            )),
        text(state.row_count().to_string())
            .size(tokens.typography.body)
            .color(Color::from(tokens.colors.text_dim)),
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(iced::Center);
    let mut header = column![title].spacing(tokens.spacing.gap_sm);
    if let Some(status) = status_line(state, ctx.i18n) {
        header = header.push(
            text(status)
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text_dim)),
        );
    }
    let content = if matches!(state.library(), Library::Scanned(rows) if rows.is_empty()) {
        route_state::view(
            RouteState::new(
                Glyph::Icon(assets::icons::package_open()),
                Tone::Quiet,
                ctx.i18n.tr("installed-addons-empty-title"),
                ctx.i18n.tr("installed-addons-empty-body"),
            ),
            ctx,
        )
    } else {
        addon_grid::view(state.grid(), ctx.tokens, GRID_KEY).map(Message::Grid)
    };
    column![header, content]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(tokens.spacing.gap)
        .into()
}

fn status_line(state: &State, i18n: &I18n) -> Option<String> {
    match state.library() {
        Library::Unscanned | Library::Scanned(_) => None,
        Library::Scanning => Some(i18n.tr("installed-addons-loading")),
        Library::Failed(error) => Some(i18n.trn(
            "installed-addons-error",
            &[("error", Arg::Text(translated_error(i18n, error).as_str()))],
        )),
    }
}
