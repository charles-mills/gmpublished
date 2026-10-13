use iced::widget::{Space, button, column, container, rich_text, row, span, svg, text};
use iced::{Border, Color, ContentFit, Element, Length, font};

use crate::assets;
use crate::theme::{self, Tokens, ViewCtx};
use crate::widgets::icon::svg_icon;

use super::about::{self, AboutLink, DebugReport};
use super::{Message, State};

pub(super) fn view<'a>(state: &'a State, ctx: ViewCtx<'a>, compact: bool) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let platform = if compact {
        format!(
            "{} · {} · {}",
            about::os_name(),
            std::env::consts::ARCH,
            about::VERSION
        )
    } else {
        format!("{} · {}", about::os_name(), std::env::consts::ARCH)
    };
    let art = svg(about::os_art())
        .width(tokens.icon_size(76.0))
        .height(tokens.icon_size(90.0))
        .content_fit(ContentFit::Contain);
    let heading = column![
        text("gmpublished")
            .size(if compact {
                tokens.typography.title_lg
            } else {
                tokens.typography.display_lg
            })
            .font(theme::styles::inter_font(font::Weight::Semibold)),
        text(i18n.tr("settings-about-description"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        text(platform)
            .size(tokens.typography.caption_xs)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap_sm)
    .width(Length::Fill);
    let hero: Element<'a, Message> = if compact {
        column![art, heading].spacing(tokens.space(20.0)).into()
    } else {
        row![art, heading]
            .spacing(tokens.space(20.0))
            .align_y(iced::Center)
            .into()
    };

    let repository = button(
        row![
            svg_icon(
                svg::Handle::from_memory(include_bytes!("../../../ui/icons/github.svg")),
                tokens.colors.text.into(),
                tokens.icon_size(29.0)
            ),
            column![
                text(i18n.tr("settings-about-github")).size(tokens.typography.body),
                text("charles-mills / gmpublished")
                    .size(tokens.typography.caption)
                    .color(tokens.colors.text_dim),
            ]
            .spacing(tokens.space(3.0))
            .width(Length::Fill),
            svg_icon(
                assets::icons::context_link_out(),
                tokens.colors.text_dim.into(),
                tokens.icon_size(17.0)
            ),
        ]
        .spacing(tokens.space(15.0))
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .padding(tokens.space(16.0))
    .on_press(Message::AboutLinkOpened(AboutLink::Repository))
    .style(move |_, status| card_style(&tokens, status));

    let ready = matches!(state.about.report, DebugReport::Ready(_));
    let copy = button(
        row![
            svg_icon(
                if state.about.copied {
                    assets::icons::check()
                } else {
                    assets::icons::context_copy()
                },
                tokens.colors.text.into(),
                tokens.icon_size(15.0)
            ),
            text(i18n.tr(if state.about.copied {
                "settings-about-copied"
            } else {
                "settings-about-copy-debug"
            }))
            .size(tokens.typography.caption),
        ]
        .spacing(tokens.space(8.0))
        .align_y(iced::Center),
    )
    .padding([tokens.space(9.0), tokens.space(12.0)])
    .on_press_maybe(ready.then_some(Message::CopyDebugInfoRequested))
    .style(move |_, status| super::view::text_button_style(&tokens, status));

    let debug_heading = column![
        text(i18n.tr("settings-about-debug-title")).size(tokens.typography.body_sm),
        text(i18n.tr("settings-about-debug-description"))
            .size(tokens.typography.caption)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap_xs)
    .width(Length::Fill);
    let debug_header: Element<'a, Message> = if compact {
        column![debug_heading, copy]
            .spacing(tokens.spacing.gap_sm)
            .into()
    } else {
        row![debug_heading, copy]
            .spacing(tokens.spacing.gap_sm)
            .align_y(iced::Center)
            .into()
    };
    let mut debug = column![debug_header].spacing(tokens.space(12.0));
    match &state.about.report {
        DebugReport::Ready(report) => {
            debug = debug.push(
                button(
                    row![
                        svg_icon(
                            if state.about.expanded {
                                assets::icons::chevron_up()
                            } else {
                                assets::icons::chevron_down()
                            },
                            tokens.colors.text_dim.into(),
                            tokens.icon_size(12.0)
                        ),
                        text(i18n.tr(if state.about.expanded {
                            "settings-about-debug-hide"
                        } else {
                            "settings-about-debug-show"
                        }))
                        .size(tokens.typography.caption_xs),
                    ]
                    .spacing(tokens.space(5.0))
                    .align_y(iced::Center),
                )
                .padding(0)
                .on_press(Message::DebugInfoToggled)
                .style(move |_, status| link_style(&tokens, status, false)),
            );
            if state.about.expanded {
                debug = debug.push(
                    container(
                        text(report)
                            .size(tokens.typography.caption)
                            .font(iced::Font::MONOSPACE)
                            .color(tokens.colors.text_dim)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                    )
                    .width(Length::Fill)
                    .padding(tokens.space(12.0))
                    .style(move |_| container::Style {
                        background: Some(Color::from(tokens.colors.surface_sunken).into()),
                        border: Border {
                            color: tokens.colors.border_subtle.into(),
                            width: 1.0,
                            radius: tokens.radii.md.into(),
                        },
                        ..container::Style::default()
                    }),
                );
            }
        }
        DebugReport::Failed => {
            debug = debug.push(
                text(i18n.tr("settings-about-debug-failed"))
                    .size(tokens.typography.caption)
                    .color(tokens.colors.error),
            );
            debug = debug.push(
                button(text(i18n.tr("settings-about-debug-retry")).size(tokens.typography.caption))
                    .padding(0)
                    .on_press(Message::DebugInfoRequested)
                    .style(move |_, status| link_style(&tokens, status, true)),
            );
        }
        DebugReport::Idle | DebugReport::Loading => {
            debug = debug.push(
                text(i18n.tr("settings-about-debug-loading"))
                    .size(tokens.typography.caption_xs)
                    .color(tokens.colors.text_dim),
            );
        }
    }

    let footer = row![
        footer_link(
            i18n.tr("settings-about-license"),
            AboutLink::License,
            &tokens
        ),
        footer_link(
            i18n.tr("settings-about-notices"),
            AboutLink::ThirdPartyNotices,
            &tokens
        ),
        footer_link(
            i18n.tr("settings-about-releases"),
            AboutLink::Releases,
            &tokens
        ),
    ]
    .spacing(tokens.space(16.0))
    .wrap();
    let credits = column![
        text(i18n.tr("settings-about-credits")).size(tokens.typography.body_sm),
        linked_copy(
            &i18n.tr("settings-about-upstream"),
            &[
                ("gmpublisher", AboutLink::Upstream),
                ("William Venner", AboutLink::OriginalAuthor),
            ],
            &tokens
        ),
        linked_copy(
            &i18n.tr("settings-about-maintainers"),
            &[("Charles Mills", AboutLink::Maintainer),],
            &tokens
        ),
        Space::new().height(4),
        footer,
    ]
    .spacing(tokens.space(7.0));

    column![
        hero,
        repository,
        divider(&tokens),
        debug,
        divider(&tokens),
        credits
    ]
    .spacing(tokens.space(18.0))
    .width(Length::Fill)
    .into()
}

fn divider<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let color = tokens.colors.divider;
    container(Space::new().height(1))
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Color::from(color).into()),
            ..container::Style::default()
        })
        .into()
}

fn footer_link<'a>(label: String, target: AboutLink, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    button(
        row![
            text(label).size(tokens.typography.caption_xs),
            svg_icon(
                assets::icons::context_link_out(),
                tokens.colors.text_dim.into(),
                tokens.icon_size(12.0)
            ),
        ]
        .spacing(tokens.space(5.0))
        .align_y(iced::Center),
    )
    .padding(0)
    .on_press(Message::AboutLinkOpened(target))
    .style(move |_, status| link_style(&tokens, status, false))
    .into()
}

// Match names after translation so link positions follow each language's word order.
fn linked_copy<'a>(
    sentence: &str,
    links: &[(&str, AboutLink)],
    tokens: &Tokens,
) -> Element<'a, Message> {
    let mut positions = links
        .iter()
        .filter_map(|(name, target)| sentence.find(name).map(|offset| (offset, *name, *target)))
        .collect::<Vec<_>>();
    positions.sort_by_key(|(offset, _, _)| *offset);
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (offset, name, target) in positions {
        spans.push(span(sentence[cursor..offset].to_owned()));
        spans.push(span(name.to_owned()).link(target).color(tokens.colors.link));
        cursor = offset + name.len();
    }
    spans.push(span(sentence[cursor..].to_owned()));
    rich_text(spans)
        .size(tokens.typography.caption)
        .color(tokens.colors.text_dim)
        .on_link_click(Message::AboutLinkOpened)
        .into()
}

fn card_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            Color::from(if hovered {
                tokens.colors.surface_2
            } else {
                tokens.colors.surface
            })
            .into(),
        ),
        text_color: tokens.colors.text.into(),
        border: Border {
            color: if hovered {
                tokens.colors.link
            } else {
                tokens.colors.border
            }
            .into(),
            width: 1.0,
            radius: tokens.radii.md.into(),
        },
        snap: true,
        ..button::Style::default()
    }
}

fn link_style(tokens: &Tokens, status: button::Status, accent: bool) -> button::Style {
    button::Style {
        background: None,
        text_color: if accent {
            tokens.colors.link
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            tokens.colors.text
        } else {
            tokens.colors.text_dim
        }
        .into(),
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::Simulator;

    fn renderer_settings() -> iced::Settings {
        iced::Settings {
            fonts: assets::fonts::bundled_fonts()
                .into_iter()
                .map(std::borrow::Cow::Borrowed)
                .collect(),
            default_font: assets::fonts::default_font(),
            ..Default::default()
        }
    }

    #[test]
    fn about_buttons_dispatch_copy_disclosure_and_repository_actions() {
        let mut state = State::default();
        let generation = state.about.load();
        state
            .about
            .complete(generation, Ok("Version: test\nOS: test".into()));
        let tokens = Tokens::dark();
        let i18n = crate::i18n::I18n::for_locale(Some("en"));
        let mut ui = Simulator::with_size(
            renderer_settings(),
            iced::Size::new(640.0, 720.0),
            container(view(&state, ViewCtx::new(&tokens, &i18n), false))
                .width(472)
                .padding(tokens.space(24.0)),
        );
        ui.click("gmpublished on GitHub").expect("repository card");
        ui.click("Copy debug info").expect("copy button");
        ui.click("View debug information")
            .expect("debug disclosure");
        let messages = ui.into_messages().collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![
                Message::AboutLinkOpened(AboutLink::Repository),
                Message::CopyDebugInfoRequested,
                Message::DebugInfoToggled,
            ]
        );
    }
}
