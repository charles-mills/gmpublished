use iced::widget::{
    Space, button, column, container, opaque, pane_grid, row, scrollable, sensor, stack, text,
    text_editor, text_input,
};
use iced::{Center, Color, Element, Font, Length, Size, font};

use crate::i18n::Arg;
use crate::{
    assets,
    features::modal_stack,
    theme::{self, ViewCtx},
    widgets::icon::svg_icon as icon,
    widgets::tooltip as tooltip_widget,
    widgets::{bbcode, select, split_pane},
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
    let modal_size = modal_stack::expanded_size(
        Size::new(viewport_size.width, viewport_size.height),
        viewport_size,
        tokens.spacing.pad,
        chrome_clearance,
    );

    let body = column![
        header(state, ctx),
        language_and_title(state, ctx, modal_size.width - tokens.spacing.pad * 2.0),
        feedback(state, ctx),
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

    let identity = text(i18n.tr("description-editor-editing"))
        .size(tokens.typography.body_sm)
        .color(Color::from(tokens.colors.text_dim));

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
        trailing.wrap(),
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

struct Tool {
    glyph: ToolGlyph,
    tooltip_key: &'static str,
    action: ToolbarAction,
}

/// Use SVGs where the bundled fonts have no glyph, including emoji.
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

    let mut bar = row![].spacing(tokens.space(2.0)).align_y(Center);
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
            let control = button(container(glyph).center_x(Length::Fixed(tokens.row_height(26.0))))
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
    bar.wrap().into()
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
            column![
                text(i18n.tr("description-editor-load-failed"))
                    .size(tokens.typography.body)
                    .color(Color::from(tokens.colors.error)),
                button(text(i18n.tr("workshop-text-retry"))).on_press(Message::RetryFetchRequested),
            ]
            .spacing(tokens.spacing.gap_sm),
        )
        .center(Length::Fill)
        .into();
    }

    let grid = pane_grid(state.panes(), |_, pane, _| {
        pane_grid::Content::new(match pane {
            super::state::Pane::Source => source_pane(state, ctx),
            super::state::Pane::Preview => preview_pane(state, ctx),
        })
    })
    .spacing(split_pane::DIVIDER_WIDTH)
    .min_size(240.0)
    .on_resize(split_pane::GRAB_LEEWAY, |event| Message::PanesResized {
        split: event.split,
        ratio: event.ratio,
    })
    .style(move |_| split_pane::style(&tokens));
    sensor(
        stack![
            grid,
            split_pane::reset_overlay(state.source_pane_width(), Message::PanesReset)
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .on_show(|size| Message::PanesLayoutChanged(size.width))
    .on_resize(|size| Message::PanesLayoutChanged(size.width))
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
    let mut editor = text_editor(source.content())
        .id("workshop-description-source")
        .key_binding(editor_key_binding);
    if !state.saving() {
        editor = editor.on_action(Message::SourceActionPerformed);
    }
    let editor = editor
        .height(Length::Fill)
        .padding(tokens.spacing.pad_control)
        .size(tokens.typography.caption)
        .font(assets::fonts::default_font())
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

fn editor_key_binding(key_press: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::{Key, key::Named};

    if !matches!(key_press.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    if key_press.modifiers.command()
        && let Key::Character(character) = &key_press.key
    {
        if character.as_str() == "z" {
            return Some(text_editor::Binding::Custom(
                if key_press.modifiers.shift() {
                    Message::Redo
                } else {
                    Message::Undo
                },
            ));
        }
        if character.as_str() == "y" {
            return Some(text_editor::Binding::Custom(Message::Redo));
        }
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

fn preview_pane<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let (Some(document), Some(revealed)) = (state.document(), state.revealed_spoilers()) else {
        return Space::new().into();
    };

    let rendered =
        bbcode::steam_view(document, revealed, state, ctx.i18n, &tokens).map(|event| match event {
            bbcode::Event::OpenLink(url) => Message::LinkOpenRequested(url),
            bbcode::Event::ToggleSpoiler(id) => Message::SpoilerToggled(id),
        });

    let column = container(rendered)
        .width(Length::Fill)
        .max_width(bbcode::STEAM_DESCRIPTION_WIDTH * tokens.text_scale())
        .padding(tokens.spacing.pad);

    container(
        scrollable(container(column).center_x(Length::Fill))
            .direction(scrollable::Direction::Vertical(
                theme::styles::vertical_scrollbar(&tokens),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .id("workshop-description-preview")
    .style(move |_| iced::widget::container::Style {
        background: Some(Color::from(bbcode::STEAM_PAGE_BACKGROUND).into()),
        border: iced::border::rounded(tokens.radii.sm),
        ..iced::widget::container::Style::default()
    })
    .into()
}

fn language_and_title<'a>(state: &'a State, ctx: ViewCtx<'a>, width: f32) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut title = text_input(
        &ctx.i18n.tr("prepare-publish-title-placeholder"),
        state.title().unwrap_or_default(),
    )
    .id("workshop-localized-title")
    .padding(tokens.spacing.pad_control)
    .size(tokens.typography.body_sm)
    .style(move |_, status| theme::styles::input(&tokens, status));
    if !state.saving() && !state.loading() && !state.load_failed() {
        title = title.on_input(Message::TitleEdited);
    }
    let metrics = select::Metrics {
        text_size: tokens.typography.body_sm,
        padding: tokens.spacing.pad_control.into(),
        radius: tokens.radii.sm,
        max_rows: select::MAX_ROWS,
    };
    let language: Element<'a, Message> = if state.saving() {
        container(
            row![
                icon(
                    assets::icons::globe(),
                    tokens.colors.text.into(),
                    tokens.dims.icon_size.min(metrics.text_size * 1.3)
                ),
                text(state.language().name()).size(metrics.text_size)
            ]
            .spacing(tokens.spacing.gap_sm)
            .align_y(Center),
        )
        .padding(metrics.padding)
        .style(move |_| theme::styles::select_face(&tokens, false))
        .into()
    } else {
        select::field_with_icon(
            state.language().name().into(),
            crate::bridge::workshop_text::WorkshopLanguage::ALL
                .into_iter()
                .map(|language| select::Row {
                    label: language.name().into(),
                    selected: language == state.language(),
                    on_select: Message::LanguageSelected(language),
                })
                .collect(),
            metrics,
            select::Chevron::Shown,
            Some(assets::icons::globe()),
            &tokens,
        )
    };
    let language = container(language).id("workshop-language");
    if width < 540.0 * tokens.text_scale() + tokens.spacing.gap_sm {
        column![language.width(Length::Fill), title]
            .spacing(tokens.spacing.gap_sm)
            .into()
    } else {
        row![
            language.width(Length::Fixed(240.0 * tokens.text_scale())),
            title
        ]
        .spacing(tokens.spacing.gap_sm)
        .align_y(Center)
        .into()
    }
}

fn feedback<'a>(state: &State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let message = state.invalid_language().map_or_else(
        || {
            state.failed_language().map(|language| {
                ctx.i18n.trn(
                    "workshop-text-save-failed",
                    &[("language", Arg::Text(language.name()))],
                )
            })
        },
        |language| {
            Some(ctx.i18n.trn(
                "workshop-text-invalid",
                &[("language", Arg::Text(language.name()))],
            ))
        },
    );
    message.map_or_else(
        || Space::new().into(),
        |message| {
            text(message)
                .size(ctx.tokens.typography.caption)
                .color(Color::from(ctx.tokens.colors.error))
                .into()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bridge::workshop_text::{WorkshopLanguage, WorkshopText},
        i18n::I18n,
    };
    use iced_test::{Simulator, selector};

    #[test]
    fn source_remains_usable_at_minimum_window_width_and_shortcuts_follow_focus() {
        let settings = || iced::Settings {
            fonts: assets::fonts::bundled_fonts()
                .into_iter()
                .map(std::borrow::Cow::Borrowed)
                .collect(),
            default_font: assets::fonts::default_font(),
            ..Default::default()
        };
        let _fonts = Simulator::<Message>::with_settings(settings(), Space::new());
        let mut state = State::default();
        state.open_localized_draft(
            WorkshopLanguage::French,
            [(
                WorkshopLanguage::French,
                WorkshopText {
                    title: "Titre français".into(),
                    description: "[h1]Bonjour[/h1]\n\n[b]世界[/b]\nПривет · Türkçe · Tiếng Việt"
                        .into(),
                },
            )]
            .into(),
        );
        let tokens = theme::Tokens::dark();
        let i18n = I18n::from_user_or_system(Some("en"));
        for size in [Size::new(800.0, 600.0), Size::new(1280.0, 800.0)] {
            let mut ui = Simulator::with_size(
                settings(),
                size,
                view(&state, ViewCtx::new(&tokens, &i18n), size, 0.0),
            );
            let source = ui
                .find(selector::id("workshop-description-source"))
                .unwrap();
            let language = ui.find(selector::id("workshop-language")).unwrap();
            let title = ui.find(selector::id("workshop-localized-title")).unwrap();
            assert!(
                (language.bounds().height - title.bounds().height).abs() < 0.1,
                "language {:?}, title {:?}",
                language.bounds(),
                title.bounds()
            );
            assert!((language.bounds().y - title.bounds().y).abs() < 0.1);

            assert!(
                source.bounds().width >= 250.0,
                "source bounds: {:?}",
                source.bounds()
            );
            assert!(source.bounds().height >= 200.0);
            assert!(
                source.bounds().width <= size.width && source.bounds().height <= size.height,
                "source bounds: {:?}",
                source.bounds()
            );
            ui.click(selector::id("workshop-language")).unwrap();
            assert!(
                ui.find("Turkish").is_ok(),
                "app language menu should contain Turkish"
            );
            ui.click("Arabic").unwrap();
            ui.click(selector::id("workshop-localized-title")).unwrap();
            ui.tap_key(iced::keyboard::key::Named::Enter);
            let divider = iced::Point::new(
                source.bounds().x + source.bounds().width + 1.0,
                source.bounds().center_y(),
            );
            ui.point_at(divider);
            ui.simulate([iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
                iced::mouse::Button::Left,
            ))]);
            let moved = iced::Point::new(divider.x + 60.0, divider.y);
            ui.point_at(moved);
            ui.simulate([
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: moved }),
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )),
            ]);
            let messages: Vec<_> = ui.into_messages().collect();
            assert!(messages.iter().any(
                |message| matches!(message, Message::PanesResized { ratio, .. } if *ratio > 0.5)
            ));
            assert!(messages.iter().any(|message| matches!(
                message,
                Message::LanguageSelected(WorkshopLanguage::Arabic)
            )));
            assert!(!messages.iter().any(|message| matches!(
                message,
                Message::EnterPressed | Message::ToolbarApplied(_) | Message::Undo | Message::Redo
            )));
        }
    }
}

#[cfg(test)]
#[test]
#[ignore = "opt-in real UI screenshots"]
fn capture_editor_display_fixtures() {
    let mut state = State::default();
    state.open_draft("Workshop description".into(), "[h1]Welcome to rRadio[/h1]\n\nA configurable radio for your server.\n\n[b]Features[/b]\n[list][*]Boombox and car radio[*]日本語 · 中文 · 한국어[*]Configurable station lists[/list]\n[code]print(\"Hello, world!\")[/code]");
    let i18n = crate::i18n::I18n::for_locale(Some("en"));
    for (density, percent, size, name) in crate::test_support::capture_display_cases() {
        let tokens = theme::Tokens::dark().with_display_preferences(density, percent);
        crate::test_support::capture_ui_surface(
            &format!("editor-{name}"),
            size,
            view(&state, ViewCtx::new(&tokens, &i18n), size, 0.0),
        );
    }
}
