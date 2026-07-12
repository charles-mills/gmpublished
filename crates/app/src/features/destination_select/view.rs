use iced::widget::{
    Space, button, checkbox, column, container, mouse_area, opaque, row, scrollable, stack, svg,
    text, text_input,
};
use iced::{Center, Color, Element, Length, Size, font};

use crate::{
    assets,
    features::modal_stack::responsive_width,
    theme::{self, Tokens, ViewCtx},
    widgets,
};

use super::model::DestinationKind;
use super::{DestinationError, Message, State};

const TOOLTIP_MAX_WIDTH: f32 = 320.0;

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>, viewport_size: Size) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let modal_width = responsive_width(
        tokens.dims.destination_modal_width,
        tokens.dims.destination_modal_max_width,
        viewport_size.width,
        tokens.dims.modal_viewport_ratio,
    );
    let tile_size = ((modal_width
        - tokens.spacing.pad * 2.0
        - tokens.spacing.gap * (DestinationKind::ALL.len() - 1) as f32)
        / DestinationKind::ALL.len() as f32)
        .max(1.0);
    let title = text(i18n.tr("destination-where-to"))
        .size(tokens.typography.display)
        .font(theme::styles::inter_font(font::Weight::Bold));
    let warning = text(i18n.tr("destination-overwrite-warning"))
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Bold));

    let mut content = column![].align_x(Center).width(Length::Fill);
    content = content.push(title);
    // h4 margins: .8rem above, 1.5rem below.
    content = content.push(Space::new().height(tokens.spacing.gap_md));
    content = content.push(warning);
    content = content.push(Space::new().height(tokens.spacing.gap_lg));
    content = content.push(path_input(state, &tokens));
    if let Some(error) = state.error() {
        // Deviation from upstream (which fails silently): a small caption
        // keeps invalid typed paths explainable.
        let error_text = match error {
            DestinationError::InvalidPath => i18n.tr("destination-invalid-path"),
            DestinationError::SaveFailed(error) => error.to_string(),
        };
        content = content.push(Space::new().height(tokens.spacing.gap_xs));
        content = content.push(
            text(error_text)
                .size(tokens.typography.caption)
                .color(Color::from(tokens.colors.error)),
        );
    }
    content = content.push(Space::new().height(tokens.spacing.gap_md));
    if state.shows_create_folder() {
        content = content.push(create_folder_row(state, ctx));
        content = content.push(Space::new().height(tokens.spacing.gap));
    }
    content = content.push(tiles(state, ctx, tile_size));
    if !state.history().is_empty() {
        content = content.push(Space::new().height(tokens.spacing.gap_lg));
        content = content.push(history(state, &tokens));
    }
    content = content.push(Space::new().height(tokens.spacing.gap));
    content = content.push(confirm_button(state, ctx));

    let panel = opaque(
        container(content)
            .width(Length::Fixed(modal_width))
            .padding(tokens.spacing.pad)
            .style(move |_| theme::styles::modal(&tokens)),
    );

    container(panel).center(Length::Fill).into()
}

fn path_input<'a>(state: &'a State, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let alignment = if state.path_input().is_empty() {
        iced::alignment::Horizontal::Center
    } else {
        iced::alignment::Horizontal::Left
    };
    text_input(&state.placeholder(), state.path_input())
        .on_input(Message::PathInputEdited)
        .on_submit(Message::PathAccepted)
        .align_x(alignment)
        .size(tokens.typography.body_sm)
        .padding(tokens.spacing.pad_sm)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::destination_input(&tokens, status))
        .into()
}

fn create_folder_row<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    row![
        checkbox(state.create_folder())
            .on_toggle(Message::CreateFolderToggled)
            .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        text(i18n.tr("destination-create-folder")).size(tokens.typography.body),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .into()
}

fn tiles<'a>(state: &'a State, ctx: ViewCtx<'a>, tile_size: f32) -> Element<'a, Message> {
    let mut tiles = row![].spacing(ctx.tokens.spacing.gap).width(Length::Fill);
    for kind in DestinationKind::ALL {
        tiles = tiles.push(tile(state, ctx, kind, tile_size));
    }
    tiles.into()
}

fn tile<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    kind: DestinationKind,
    tile_size: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let enabled = state.kind_enabled(kind);
    let selected = state.kind_active(kind);
    let icon_size = tokens.dims.destination_tile_icon
        * (tile_size / tokens.dims.destination_tile).clamp(0.75, 1.3);

    let label = text(i18n.tr(kind.label_key()))
        .size(tokens.typography.body)
        .wrapping(text::Wrapping::None);
    let body = container(
        column![
            tile_icon(&tokens, kind, enabled, icon_size),
            Space::new().height(tokens.dims.destination_tile_icon_gap),
            label,
        ]
        .align_x(Center),
    )
    .width(tile_size)
    .height(tile_size)
    .align_x(Center)
    .align_y(Center)
    .style(move |_| theme::styles::destination_tile(&tokens, selected, enabled));

    if !enabled {
        return body.into();
    }

    let area = mouse_area(body)
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Message::KindToggled(kind));

    if kind == DestinationKind::Temp {
        widgets::tooltip::below(
            area,
            i18n.tr("destination-open-tip"),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        )
    } else {
        area.into()
    }
}

fn tile_icon(
    tokens: &Tokens,
    kind: DestinationKind,
    enabled: bool,
    size: f32,
) -> Element<'static, Message> {
    let tokens = *tokens;
    let handle = match kind {
        DestinationKind::Browse => assets::icons::akar_folder(),
        DestinationKind::Temp => assets::icons::akar_folder_add(),
        DestinationKind::Downloads => assets::icons::akar_download(),
        DestinationKind::Addons => {
            // Multicolor Garry's Mod logo: rendered untinted. Upstream dims
            // disabled tiles with brightness(.5); the logo cannot be
            // recolored, so a 50%-alpha film of the disabled tile bg covers
            // it instead (the one sanctioned alpha use in this modal).
            let logo = svg(assets::icons::gmod_logo()).width(size).height(size);
            if enabled {
                return logo.into();
            }
            let film = container(Space::new().width(size).height(size)).style(move |_| {
                iced::widget::container::Style {
                    background: Some(
                        Color::from(tokens.colors.tile_disabled_bg.with_alpha(128)).into(),
                    ),
                    ..iced::widget::container::Style::default()
                }
            });
            return stack![logo, film].into();
        }
    };
    let tint = if enabled {
        tokens.colors.text
    } else {
        tokens.colors.tile_disabled_text
    };
    svg(handle)
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style {
            color: Some(tint.into()),
        })
        .into()
}

fn history<'a>(state: &'a State, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let mut rows = column![];
    for (index, path) in state.history().iter().enumerate() {
        let display = path.to_string_lossy().into_owned();
        let selected = state.is_history_selected(path);
        let shaded = index % 2 == 0;
        let body = container(
            text(display)
                .size(tokens.typography.body_sm)
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .width(Length::Fill)
        .padding(tokens.dims.destination_row_padding)
        .style(move |_| {
            if selected {
                iced::widget::container::Style {
                    text_color: Some(tokens.colors.text.into()),
                    background: Some(Color::from(tokens.colors.destination_input_bg).into()),
                    ..iced::widget::container::Style::default()
                }
            } else {
                theme::styles::striped_row(&tokens, shaded)
            }
        });
        rows = rows.push(
            mouse_area(body)
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Message::HistorySelected(path.clone())),
        );
    }

    // Natural height up to the cap, then scrolls without visible chrome.
    container(
        scrollable(rows)
            .width(Length::Fill)
            .direction(scrollable::Direction::Vertical(
                theme::styles::hidden_vertical_scrollbar(),
            )),
    )
    .width(Length::Fill)
    .max_height(tokens.dims.destination_history_max_height)
    .clip(true)
    .style(move |_| theme::styles::sunken_card(&tokens))
    .into()
}

fn confirm_button<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    button(
        container(
            text(i18n.tr(state.confirm_label_key()))
                .size(tokens.typography.body)
                .line_height(1.0),
        )
        .center_x(Length::Fill),
    )
    .width(Length::Fill)
    .padding(tokens.spacing.pad_control)
    .style(move |_, status| theme::styles::extract_button(&tokens, status))
    .on_press_maybe(state.can_confirm().then_some(Message::ConfirmRequested))
    .into()
}
