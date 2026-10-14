use iced::widget::{button, checkbox, column, row, text, text_input};
use iced::{Element, Length};

use super::super::{Message, State};
use crate::i18n::Arg;
use crate::theme::{self, ViewCtx};

pub(super) fn animations_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut content = column![text(i18n.tr("models-animations")).size(tokens.typography.body_lg),]
        .spacing(tokens.spacing.gap_lg);
    for (index, animation) in state.animations.iter().enumerate() {
        let mut controls = column![
            row![
                checkbox(animation.included)
                    .label(i18n.tr("models-animation-include"))
                    .on_toggle_maybe(
                        (!busy).then_some(move |value| Message::AnimationIncluded(index, value))
                    )
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                text(animation.source.as_deref().unwrap_or("idle"))
                    .width(Length::Fill)
                    .size(tokens.typography.body_sm),
            ]
            .spacing(tokens.spacing.gap_lg)
            .align_y(iced::Center),
            text(i18n.tr("models-animation-name")).size(tokens.typography.body_sm),
            text_input("idle", &animation.name)
                .on_input_maybe(
                    (!busy && animation.included)
                        .then_some(move |value| Message::AnimationNameChanged(index, value))
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::input(&tokens, status)),
            row![
                column![
                    text(i18n.tr("models-animation-fps")).size(tokens.typography.body_sm),
                    text_input("30", &animation.fps)
                        .on_input_maybe(
                            (!busy && animation.included)
                                .then_some(move |value| Message::AnimationFpsChanged(index, value))
                        )
                        .padding(tokens.spacing.pad_control)
                        .width(Length::Fixed(120.0))
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap_xs),
                checkbox(animation.looping)
                    .label(i18n.tr("models-animation-loop"))
                    .on_toggle_maybe(
                        (!busy && animation.included)
                            .then_some(move |value| Message::AnimationLoopChanged(index, value))
                    )
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_lg)
            .align_y(iced::Center),
        ]
        .spacing(tokens.spacing.gap);
        controls = controls.push(
            button(
                row![
                    text(if animation.advanced_expanded {
                        "▾"
                    } else {
                        "▸"
                    }),
                    text(i18n.tr("models-animation-advanced"))
                ]
                .spacing(tokens.spacing.gap_xs),
            )
            .on_press_maybe(
                (!busy && animation.included).then_some(Message::AnimationAdvancedToggled(index)),
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
        );
        if animation.advanced_expanded {
            controls = controls.push(animation_advanced(index, animation, ctx, busy));
        }
        content = content.push(controls);
    }
    if state.parsed_animation_settings().is_none() {
        content = content.push(
            text(i18n.tr("models-animation-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    if state.parsed_animation_settings().is_none()
        && state
            .animations
            .iter()
            .any(|animation| !animation.activity.trim().is_empty() || !animation.events.is_empty())
    {
        content = content.push(
            text(i18n.tr("models-animation-advanced-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    content.into()
}

fn animation_advanced<'a>(
    index: usize,
    animation: &'a super::super::state::AnimationEditor,
    ctx: ViewCtx<'a>,
    busy: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let enabled = !busy && animation.included;
    let last = animation.frame_count.saturating_sub(1).to_string();
    let mut controls = column![
        text(i18n.tr("models-animation-activity-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        row![
            column![
                text(i18n.tr("models-animation-activity")).size(tokens.typography.body_sm),
                text_input("ACT_VM_IDLE", &animation.activity)
                    .on_input_maybe(
                        enabled.then_some(move |value| Message::AnimationActivityChanged(
                            index, value
                        ))
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs)
            .width(Length::Fill),
            column![
                text(i18n.tr("models-animation-activity-weight")).size(tokens.typography.body_sm),
                text_input("1", &animation.activity_weight)
                    .on_input_maybe(
                        (enabled && !animation.activity.trim().is_empty()).then_some(
                            move |value| Message::AnimationActivityWeightChanged(index, value)
                        )
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs)
            .width(Length::Fixed(120.0)),
        ]
        .spacing(tokens.spacing.gap),
        text(i18n.tr("models-animation-events")).size(tokens.typography.body),
        text(i18n.trn(
            "models-animation-event-range",
            &[("last", Arg::Number(&last))]
        ))
        .size(tokens.typography.body_sm)
        .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    for (event_index, event) in animation.events.iter().enumerate() {
        controls = controls.push(
            column![
                row![
                    column![
                        text(i18n.tr("models-animation-event-frame"))
                            .size(tokens.typography.body_sm),
                        text_input("0", &event.frame)
                            .on_input_maybe(enabled.then_some(move |value| {
                                Message::AnimationEventFrameChanged(index, event_index, value)
                            }))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap_xs)
                    .width(Length::FillPortion(1)),
                    column![
                        text(i18n.tr("models-animation-event-code"))
                            .size(tokens.typography.body_sm),
                        text_input("5004 / AE_CL_PLAYSOUND", &event.event)
                            .on_input_maybe(enabled.then_some(move |value| {
                                Message::AnimationEventCodeChanged(index, event_index, value)
                            }))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap_xs)
                    .width(Length::FillPortion(3)),
                ]
                .spacing(tokens.spacing.gap)
                .align_y(iced::Bottom),
                text(i18n.tr("models-animation-event-options")).size(tokens.typography.body_sm),
                text_input("", &event.options)
                    .on_input_maybe(enabled.then_some(move |value| {
                        Message::AnimationEventOptionsChanged(index, event_index, value)
                    }))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
                button(text(i18n.tr("models-animation-event-remove")))
                    .on_press_maybe(
                        enabled.then_some(Message::AnimationEventRemoved(index, event_index))
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs),
        );
    }
    controls = controls.push(
        button(text(i18n.tr("models-animation-event-add")))
            .on_press_maybe(
                (enabled && animation.events.len() < 1024)
                    .then_some(Message::AnimationEventAdded(index)),
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    );
    controls.into()
}
