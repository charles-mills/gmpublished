use iced::widget::{Space, button, column, row, text};
use iced::{Color, Element, Length};

use crate::assets;
use crate::i18n::{Arg, I18n, translated_error};
use crate::theme::ViewCtx;
use crate::widgets::addon_grid;
use crate::widgets::route_state::{self, Glyph, RouteState, Tone};

use super::state::PageStatus;
use super::{Message, State};

pub const GRID_KEY: &str = "my-workshop-grid";

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let title = row![
        text(ctx.i18n.tr("my-workshop"))
            .size(tokens.typography.title_lg)
            .font(crate::theme::styles::inter_font(
                iced::font::Weight::Semibold
            )),
        text(state.total_count().max(state.loaded_count()).to_string())
            .size(tokens.typography.body)
            .color(Color::from(tokens.colors.text_dim)),
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(iced::Center);
    let publish = button(text(ctx.i18n.tr("publish-new")).size(tokens.typography.body))
        .padding([tokens.spacing.pad_control_y, tokens.spacing.pad_control_x])
        .on_press(Message::Grid(addon_grid::Message::CardClicked(
            super::model::PUBLISH_NEW_ROW_ID.into(),
        )))
        .style(move |_, status| crate::theme::styles::action_button(&tokens, status));
    let heading: Element<'a, Message> =
        if state.grid().available_width() >= 640.0 * tokens.text_scale() {
            row![title, Space::new().width(Length::Fill), publish]
                .align_y(iced::Center)
                .width(Length::Fill)
                .into()
        } else {
            column![title, publish]
                .spacing(tokens.spacing.gap_sm)
                .into()
        };
    let mut header = column![heading].spacing(tokens.spacing.gap_sm);
    if let Some(status) =
        status_line(state, ctx.i18n).or_else(|| partial_count_line(state, ctx.i18n))
    {
        header = header.push(
            text(status)
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text_dim)),
        );
    }
    let content = if state.is_loaded_and_empty() {
        route_state::view(
            RouteState::new(
                Glyph::Icon(assets::icons::package_open()),
                Tone::Quiet,
                ctx.i18n.tr("my-workshop-empty-title"),
                ctx.i18n.tr("my-workshop-empty-body"),
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

fn partial_count_line(state: &State, i18n: &I18n) -> Option<String> {
    let loaded = state.loaded_count();
    let total = state.total_count();
    if loaded == 0 || total == 0 || loaded >= total {
        return None;
    }

    Some(i18n.trn(
        "my-workshop-count",
        &[
            ("loaded", Arg::Number(&loaded.to_string())),
            ("total", Arg::Number(&total.to_string())),
        ],
    ))
}

fn status_line(state: &State, i18n: &I18n) -> Option<String> {
    match state.page_status() {
        PageStatus::Idle
        | PageStatus::Loaded
        | PageStatus::Complete
        | PageStatus::LoadingNextPage => None,
        PageStatus::LoadingFirstPage => Some(i18n.tr("my-workshop-loading")),
        PageStatus::Failed(error) => Some(i18n.trn(
            "my-workshop-error",
            &[("error", Arg::Text(translated_error(i18n, error).as_str()))],
        )),
    }
}
