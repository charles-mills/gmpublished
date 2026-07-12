use iced::widget::{column, text};
use iced::{Color, Element, Length};

use crate::i18n::I18n;
use crate::theme::ViewCtx;
use crate::widgets::addon_grid;

use super::state::LoadStatus;
use super::{Message, State};

/// Identifies this route's grid across route switches; see `addon_grid::view`.
pub const GRID_KEY: &str = "my-workshop-grid";

pub fn view<'a>(state: &State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let grid = addon_grid::view(state.grid(), ctx.tokens, GRID_KEY).map(Message::Grid);
    if let Some(header) = header_line(state, ctx) {
        column![header, grid]
            .width(Length::Fill)
            .height(Length::Fill)
            .spacing(ctx.tokens.spacing.gap)
            .into()
    } else {
        column![grid]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn header_line<'a>(state: &State, ctx: ViewCtx<'a>) -> Option<Element<'a, Message>> {
    let tokens = ctx.tokens;
    let i18n = ctx.i18n;
    if state.loaded_count() == 0 {
        return status_line(state, i18n).map(|status| {
            text(status)
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text_dim))
                .into()
        });
    }

    partial_count_line(state, i18n).map(|count| {
        text(count)
            .size(tokens.typography.title_lg)
            .color(Color::from(tokens.colors.text))
            .into()
    })
}

fn partial_count_line(state: &State, i18n: &I18n) -> Option<String> {
    let loaded = state.loaded_count();
    let total = state.total_count();
    if loaded == 0 || total == 0 || loaded >= total {
        return None;
    }

    Some(i18n.trn(
        "my-workshop-count",
        &[("arg0", &loaded.to_string()), ("arg1", &total.to_string())],
    ))
}

fn status_line(state: &State, i18n: &I18n) -> Option<String> {
    match state.load_status() {
        LoadStatus::Idle | LoadStatus::Ready => None,
        LoadStatus::Loading => Some(i18n.tr("my-workshop-loading")),
        LoadStatus::Empty => Some(i18n.tr("my-workshop-empty")),
        LoadStatus::Error(error) => {
            Some(i18n.trn("my-workshop-error", &[("arg0", error.as_str())]))
        }
    }
}
