//! Centred panel for a route that cannot show its content yet: a tinted
//! glyph, a title, one sentence, and the buttons that fix it.
//!
//! Every blank route surface goes through here — unmet prerequisites and
//! genuinely-empty libraries alike — so "no Garry's Mod" and "no addons"
//! stop being reported in two different visual dialects.

use iced::widget::{Space, button, column, container, row, svg, text};
use iced::{Alignment, Center, Color, ContentFit, Element, Length};

use crate::theme::{self, Tokens, ViewCtx};

/// Panel width. The body wraps inside it at roughly 34 characters.
const PANEL_WIDTH: f32 = 380.0;
const GLYPH_DISC: f32 = 52.0;
const GLYPH_SIZE: f32 = 26.0;
/// Free space above the panel as a share of the total, so the panel's centre
/// lands on the optical centre rather than the mathematical one.
const OPTICAL_TOP_SHARE: u16 = 46;
const OPTICAL_BOTTOM_SHARE: u16 = 54;

/// How loudly the glyph reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    /// An unmet prerequisite the user can act on.
    Warn,
    /// Work in flight — connecting, searching.
    Busy,
    /// Nothing is wrong; there is simply nothing here.
    Quiet,
}

impl Tone {
    fn glyph_color(self, tokens: &Tokens) -> Color {
        match self {
            Self::Warn => tokens.colors.warn.into(),
            Self::Busy => tokens.colors.link.into(),
            Self::Quiet => tokens.colors.text_dim.into(),
        }
    }

    fn disc_style(self, tokens: &Tokens) -> container::Style {
        let fill = match self {
            Self::Warn => tokens.colors.warn_fill,
            Self::Busy => tokens.colors.account_update_bg,
            Self::Quiet => tokens.colors.surface_2,
        };
        theme::styles::circle(fill, GLYPH_DISC / 2.0)
    }
}

/// Weight of a panel button. Exactly one action per panel is `Primary`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Emphasis {
    Primary,
    Secondary,
}

/// One button under the sentence.
pub struct Action<M> {
    label: String,
    emphasis: Emphasis,
    /// `None` renders the button disabled — used while a retry is in flight.
    message: Option<M>,
}

impl<M> Action<M> {
    pub fn primary(label: String, message: M) -> Self {
        Self {
            label,
            emphasis: Emphasis::Primary,
            message: Some(message),
        }
    }

    pub fn secondary(label: String, message: M) -> Self {
        Self {
            label,
            emphasis: Emphasis::Secondary,
            message: Some(message),
        }
    }

    /// Greys the button out without removing it, so the row keeps its shape
    /// while the action it offers is momentarily unavailable.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.message = None;
        self
    }
}

/// What sits inside the tinted disc.
pub enum Glyph {
    Icon(svg::Handle),
    /// The shared equalizer spinner, driven by the caller's animation clock.
    Spinner(f32),
}

/// A route surface with no content to show.
pub struct RouteState<M> {
    glyph: Glyph,
    tone: Tone,
    title: String,
    body: String,
    /// Monospaced supporting line, e.g. the saved path that stopped resolving.
    detail: Option<String>,
    actions: Vec<Action<M>>,
}

impl<M> RouteState<M> {
    pub fn new(glyph: Glyph, tone: Tone, title: String, body: String) -> Self {
        Self {
            glyph,
            tone,
            title,
            body,
            detail: None,
            actions: Vec::new(),
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    #[must_use]
    pub fn action(mut self, action: Action<M>) -> Self {
        self.actions.push(action);
        self
    }
}

/// Renders the panel filling the route's content area.
pub fn view<'a, M: Clone + 'a>(state: RouteState<M>, ctx: ViewCtx<'a>) -> Element<'a, M> {
    let tokens = *ctx.tokens;
    let RouteState {
        glyph,
        tone,
        title,
        body,
        detail,
        actions,
    } = state;

    let inner: Element<'a, M> = match glyph {
        Glyph::Icon(handle) => svg(handle)
            .width(Length::Fixed(GLYPH_SIZE))
            .height(Length::Fixed(GLYPH_SIZE))
            .content_fit(ContentFit::Contain)
            .style(move |_, _| svg::Style {
                color: Some(tone.glyph_color(&tokens)),
            })
            .into(),
        Glyph::Spinner(elapsed) => super::spinner::spinner(&tokens, elapsed, GLYPH_SIZE * 0.7),
    };

    let disc = container(inner)
        .center(Length::Fixed(GLYPH_DISC))
        .style(move |_| tone.disc_style(&tokens));

    let mut panel = column![
        disc,
        Space::new().height(Length::Fixed(tokens.spacing.gap_md)),
        text(title)
            .size(tokens.typography.title_sm)
            .font(theme::styles::inter_font(iced::font::Weight::Semibold))
            .color(Color::from(tokens.colors.text))
            .align_x(Center),
        Space::new().height(Length::Fixed(tokens.spacing.gap_sm)),
        text(body)
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text_dim))
            .align_x(Center),
    ]
    .align_x(Alignment::Center)
    .width(Length::Fixed(PANEL_WIDTH));

    if let Some(detail) = detail {
        panel = panel
            .push(Space::new().height(Length::Fixed(tokens.spacing.gap_sm)))
            .push(
                text(detail)
                    .size(tokens.typography.caption_xs)
                    .font(iced::Font::MONOSPACE)
                    .color(Color::from(tokens.colors.text_watermark))
                    .align_x(Center),
            );
    }

    if !actions.is_empty() {
        let mut buttons = row![].spacing(tokens.spacing.gap_sm).align_y(Center);
        for action in actions {
            buttons = buttons.push(action_button(action, &tokens));
        }
        panel = panel
            .push(Space::new().height(Length::Fixed(tokens.spacing.gap)))
            .push(buttons);
    }

    // Vertical thirds rather than `center`: the panel reads as centred when
    // its mass sits a little above the true middle.
    container(
        column![
            Space::new().height(Length::FillPortion(OPTICAL_TOP_SHARE)),
            panel,
            Space::new().height(Length::FillPortion(OPTICAL_BOTTOM_SHARE)),
        ]
        .align_x(Alignment::Center),
    )
    .center_x(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn action_button<'a, M: Clone + 'a>(action: Action<M>, tokens: &Tokens) -> Element<'a, M> {
    let tokens = *tokens;
    let emphasis = action.emphasis;
    button(
        text(action.label)
            .size(tokens.typography.body_sm)
            .font(theme::styles::inter_font(iced::font::Weight::Medium)),
    )
    .on_press_maybe(action.message)
    .padding([tokens.spacing.pad_control_y, tokens.spacing.pad_control_x])
    .style(move |_, status| match emphasis {
        Emphasis::Primary => theme::styles::action_button(&tokens, status),
        Emphasis::Secondary => theme::styles::button(&tokens, status),
    })
    .into()
}
