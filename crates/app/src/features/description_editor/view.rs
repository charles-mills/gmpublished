use iced::widget::{
    Space, button, column, container, opaque, row, scrollable, stack, text, text_editor,
};
use iced::{Center, Color, Element, Font, Length, Size, font};

use crate::i18n::Arg;
use crate::{
    assets,
    features::modal_stack,
    theme::{self, ViewCtx},
    widgets::bbcode,
    widgets::icon::svg_icon as icon,
    widgets::tooltip as tooltip_widget,
};

use super::markup::ToolbarAction;
use super::state::DESCRIPTION_MAX_BYTES;
use super::{Message, State};

pub fn view<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    chrome_clearance: f32,
) -> Element<'a, Message> {
    if !state.is_open() {
        return container(Space::new()).center(Length::Fill).into();
    }

    let tokens = *ctx.tokens;
    // The editor always takes the whole window: descriptions are the one
    // piece of metadata that deserves the full canvas.
    let modal_size = modal_stack::expanded_size(
        Size::new(viewport_size.width, viewport_size.height),
        viewport_size,
        tokens.spacing.pad,
        chrome_clearance,
    );

    let body = column![
        header(state, ctx),
        toolbar(state, ctx),
        workspace(state, ctx),
    ]
    .spacing(tokens.spacing.gap_sm)
    .height(Length::Fill);

    let panel = container(body)
        .padding(tokens.spacing.pad)
        .width(Length::Fixed(modal_size.width))
        .height(Length::Fixed(modal_size.height))
        .clip(true)
        .style(move |_| theme::styles::modal(&tokens));

    container(opaque(panel)).center(Length::Fill).into()
}

fn header<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;

    let back = button(icon(
        assets::icons::akar_arrow_left(),
        tokens.colors.text.into(),
        tokens.dims.icon_size,
    ))
    .on_press(Message::CloseRequested)
    .padding(tokens.spacing.pad_xs)
    .style(move |_, status| theme::styles::ghost_button(&tokens, status));

    let identity = column![
        text(state.title().unwrap_or_default().to_owned())
            .size(tokens.typography.body_lg)
            .color(Color::from(tokens.colors.text)),
        text(i18n.tr("description-editor-editing"))
            .size(tokens.typography.caption)
            .color(Color::from(tokens.colors.text_dim)),
    ];

    let mut trailing = row![].spacing(tokens.spacing.gap_sm).align_y(Center);
    if state.confirming_discard() {
        trailing = trailing
            .push(
                text(i18n.tr("description-editor-discard-prompt"))
                    .size(tokens.typography.body_sm)
                    .color(Color::from(tokens.colors.text)),
            )
            .push(
                button(text(i18n.tr("description-editor-discard")).size(tokens.typography.body_sm))
                    .on_press(Message::DiscardConfirmed)
                    .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            )
            .push(
                button(
                    text(i18n.tr("description-editor-keep-editing"))
                        .size(tokens.typography.body_sm),
                )
                .on_press(Message::DiscardCancelled)
                .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
                .style(move |_, status| theme::styles::extract_button(&tokens, status)),
            );
    } else {
        trailing = trailing
            .push(length_counter(state, ctx))
            .push(
                button(text(i18n.tr("description-editor-cancel")).size(tokens.typography.body_sm))
                    .on_press(Message::CloseRequested)
                    .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            )
            .push(
                button(
                    text(if state.saving() {
                        i18n.tr("description-editor-saving")
                    } else if state.is_draft() {
                        i18n.tr("description-editor-stage")
                    } else {
                        i18n.tr("description-editor-save")
                    })
                    .size(tokens.typography.body_sm),
                )
                .on_press_maybe(state.can_save().then_some(Message::SaveRequested))
                .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
                .style(move |_, status| theme::styles::extract_button(&tokens, status)),
            );
    }

    row![
        back,
        identity,
        container(Space::new()).width(Length::Fill),
        trailing,
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(Center)
    .into()
}

fn length_counter<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let used = state.source_bytes();
    let used_text = used.to_string();
    let limit_text = DESCRIPTION_MAX_BYTES.to_string();
    let label = i18n.trn(
        "description-editor-length",
        &[
            ("used", Arg::Number(used_text.as_str())),
            ("limit", Arg::Number(limit_text.as_str())),
        ],
    );
    let color = if state.over_limit() {
        tokens.colors.error
    } else {
        tokens.colors.text_dim
    };
    text(label)
        .size(tokens.typography.caption)
        .color(Color::from(color))
        .into()
}

/// One formatting control: its glyph, styled after the effect it applies.
struct Tool {
    glyph: ToolGlyph,
    tooltip_key: &'static str,
    action: ToolbarAction,
}

/// Typography where a bundled glyph exists (B, H1, …); an SVG asset where
/// none does — the bundled fonts carry no emoji coverage.
enum ToolGlyph {
    Text {
        label: &'static str,
        font: Option<Font>,
    },
    Icon(iced::widget::svg::Handle),
}

impl Tool {
    const fn plain(label: &'static str, tooltip_key: &'static str, action: ToolbarAction) -> Self {
        Self {
            glyph: ToolGlyph::Text { label, font: None },
            tooltip_key,
            action,
        }
    }

    const fn styled(
        label: &'static str,
        font: Font,
        tooltip_key: &'static str,
        action: ToolbarAction,
    ) -> Self {
        Self {
            glyph: ToolGlyph::Text {
                label,
                font: Some(font),
            },
            tooltip_key,
            action,
        }
    }

    fn icon(
        handle: iced::widget::svg::Handle,
        tooltip_key: &'static str,
        action: ToolbarAction,
    ) -> Self {
        Self {
            glyph: ToolGlyph::Icon(handle),
            tooltip_key,
            action,
        }
    }
}

fn tools() -> Vec<Vec<Tool>> {
    let default = assets::fonts::default_font();
    let bold = Font {
        weight: font::Weight::Bold,
        ..default
    };
    let italic = Font {
        style: font::Style::Italic,
        ..default
    };
    vec![
        vec![
            Tool::styled(
                "B",
                bold,
                "description-editor-tool-bold",
                ToolbarAction::Bold,
            ),
            Tool::styled(
                "I",
                italic,
                "description-editor-tool-italic",
                ToolbarAction::Italic,
            ),
            Tool::plain(
                "U",
                "description-editor-tool-underline",
                ToolbarAction::Underline,
            ),
            Tool::plain(
                "S",
                "description-editor-tool-strike",
                ToolbarAction::Strikethrough,
            ),
        ],
        vec![
            Tool::plain(
                "H1",
                "description-editor-tool-h1",
                ToolbarAction::Heading(1),
            ),
            Tool::plain(
                "H2",
                "description-editor-tool-h2",
                ToolbarAction::Heading(2),
            ),
            Tool::plain(
                "H3",
                "description-editor-tool-h3",
                ToolbarAction::Heading(3),
            ),
        ],
        vec![
            Tool::plain(
                "•—",
                "description-editor-tool-list",
                ToolbarAction::BulletList,
            ),
            Tool::plain(
                "1.",
                "description-editor-tool-olist",
                ToolbarAction::OrderedList,
            ),
            Tool::plain("❝", "description-editor-tool-quote", ToolbarAction::Quote),
            Tool::plain("</>", "description-editor-tool-code", ToolbarAction::Code),
            Tool::icon(
                assets::icons::table(),
                "description-editor-tool-table",
                ToolbarAction::Table,
            ),
        ],
        vec![
            Tool::icon(
                assets::icons::link_chain(),
                "description-editor-tool-link",
                ToolbarAction::Link,
            ),
            Tool::icon(
                assets::icons::image(),
                "description-editor-tool-image",
                ToolbarAction::Image,
            ),
            Tool::icon(
                assets::icons::play(),
                "description-editor-tool-youtube",
                ToolbarAction::Youtube,
            ),
            Tool::icon(
                assets::icons::eye_off(),
                "description-editor-tool-spoiler",
                ToolbarAction::Spoiler,
            ),
            Tool::plain(
                "—",
                "description-editor-tool-hr",
                ToolbarAction::HorizontalRule,
            ),
        ],
    ]
}

fn toolbar<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let enabled = !state.loading() && !state.load_failed() && !state.saving();

    let mut bar = row![].spacing(2.0).align_y(Center);
    let groups = tools();
    let group_count = groups.len();
    for (index, group) in groups.into_iter().enumerate() {
        for tool in group {
            let glyph: Element<'a, Message> = match tool.glyph {
                ToolGlyph::Text { label, font } => {
                    let mut label = text(label).size(tokens.typography.body_sm);
                    if let Some(font) = font {
                        label = label.font(font);
                    }
                    label.into()
                }
                ToolGlyph::Icon(handle) => {
                    icon(handle, tokens.colors.text.into(), tokens.dims.icon_size).into()
                }
            };
            let control = button(container(glyph).center_x(Length::Fixed(26.0)))
                .on_press_maybe(enabled.then_some(Message::ToolbarApplied(tool.action)))
                .padding(tokens.spacing.pad_xs)
                .style(move |_, status| theme::styles::ghost_button(&tokens, status));
            bar = bar.push(tooltip_widget::below(
                control,
                i18n.tr(tool.tooltip_key),
                &tokens,
                tokens.dims.tooltip_max_width,
            ));
        }
        if index + 1 < group_count {
            bar = bar.push(
                container(Space::new().width(1.0).height(16.0)).style(move |_| {
                    iced::widget::container::Style {
                        background: Some(Color::from(tokens.colors.divider).into()),
                        ..iced::widget::container::Style::default()
                    }
                }),
            );
            bar = bar.push(Space::new().width(tokens.spacing.gap_xs));
        }
    }
    bar.into()
}

fn workspace<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;

    if state.loading() {
        return container(
            text(i18n.tr("description-editor-loading"))
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text_dim)),
        )
        .center(Length::Fill)
        .into();
    }
    if state.load_failed() {
        return container(
            text(i18n.tr("description-editor-load-failed"))
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.error)),
        )
        .center(Length::Fill)
        .into();
    }

    row![source_pane(state, ctx), preview_pane(state, ctx)]
        .spacing(tokens.spacing.gap_sm)
        .height(Length::Fill)
        .into()
}

fn source_pane<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let Some(source) = state.source() else {
        return Space::new().into();
    };

    // Read-only while a save is in flight: edits made after the submitted
    // snapshot would be silently lost when the modal auto-closes on success.
    let mut editor = text_editor(source.content()).key_binding(editor_key_binding);
    if !state.saving() {
        editor = editor.on_action(Message::SourceActionPerformed);
    }
    let editor = editor
        .height(Length::Fill)
        .padding(tokens.spacing.pad_control)
        .size(tokens.typography.caption)
        .font(Font::MONOSPACE)
        .style(move |_, status| theme::styles::text_editor(&tokens, status));

    let mut layers = stack![editor];
    if state.source_is_empty() {
        layers = layers.push(
            container(
                text(i18n.tr("description-editor-watermark"))
                    .size(tokens.typography.body_lg)
                    .color(Color::from(tokens.colors.text_watermark)),
            )
            .center(Length::Fill),
        );
    }

    container(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Ctrl/Cmd shortcuts route to the toolbar, Enter goes through the
/// list-aware handler, everything else keeps iced's default behavior.
fn editor_key_binding(key_press: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::{Key, key::Named};

    if key_press.modifiers.command()
        && let Key::Character(character) = &key_press.key
    {
        let action = match character.as_str() {
            "b" => Some(ToolbarAction::Bold),
            "i" => Some(ToolbarAction::Italic),
            "u" => Some(ToolbarAction::Underline),
            _ => None,
        };
        if let Some(action) = action {
            return Some(text_editor::Binding::Custom(Message::ToolbarApplied(
                action,
            )));
        }
    }
    if matches!(key_press.key, Key::Named(Named::Enter)) && key_press.modifiers.is_empty() {
        return Some(text_editor::Binding::Custom(Message::EnterPressed));
    }
    text_editor::Binding::from_key_press(key_press)
}

/// The rendered half: the document exactly as the Workshop page shows it —
/// Steam's canvas color, Steam's column width, Steam's type ramp.
fn preview_pane<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let (Some(document), Some(revealed)) = (state.document(), state.revealed_spoilers()) else {
        return Space::new().into();
    };

    let rendered =
        bbcode::steam_view(document, revealed, state, ctx.i18n).map(|event| match event {
            bbcode::Event::OpenLink(url) => Message::LinkOpenRequested(url),
            bbcode::Event::ToggleSpoiler(id) => Message::SpoilerToggled(id),
        });

    let column = container(rendered)
        .width(Length::Fixed(bbcode::STEAM_DESCRIPTION_WIDTH))
        .padding(tokens.spacing.pad);

    container(
        scrollable(container(column).center_x(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fixed(
        bbcode::STEAM_DESCRIPTION_WIDTH + tokens.spacing.pad * 2.0 + tokens.spacing.gap_sm,
    ))
    .height(Length::Fill)
    .clip(true)
    .style(move |_| iced::widget::container::Style {
        background: Some(Color::from(bbcode::STEAM_PAGE_BACKGROUND).into()),
        border: iced::border::rounded(tokens.radii.sm),
        ..iced::widget::container::Style::default()
    })
    .into()
}
