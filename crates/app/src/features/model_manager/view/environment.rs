use iced::widget::{button, column, row, text};
use iced::{Element, Length};

use super::super::{Message, State};
use super::choice_field;
use crate::theme::{self, ViewCtx};

pub(super) fn stage_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut maps = state.maps.clone();
    if let Some(map) = &state.selected_map
        && !maps.contains(map)
    {
        maps.push(map.clone());
    }
    let mut body = column![
        text(i18n.tr("models-stage-map")).size(tokens.typography.body_lg),
        row![
            choice_field(
                maps,
                state.selected_map.as_ref(),
                busy,
                Message::MapSelected,
                ctx
            ),
            button(text(i18n.tr("browse")))
                .on_press_maybe((!busy).then_some(Message::MapBrowseRequested))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap),
        button(text(i18n.tr("models-refresh-maps")))
            .on_press_maybe(
                (!busy && state.map_generation.is_none()).then_some(Message::MapsRequested)
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
        button(text(i18n.tr("models-stage")))
            .on_press_maybe(
                (!busy && state.selected_map.is_some() && state.can_preview())
                    .then_some(Message::StageRequested)
            )
            .width(Length::Fill)
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap_sm);
    if state.maps.is_empty() {
        body = body.push(text(i18n.tr("models-no-maps")).size(tokens.typography.body_sm));
    }
    body.into()
}
