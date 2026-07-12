//! Origin-scaled content that can render through the overlay pass.
//!
//! Adapted from `iced_widget` 0.14.2 `float.rs` (MIT). Deltas: lift whenever
//! `scale != 1.0`, allow overshoot, drop shadow styling, and let callers
//! disable all child interactivity while still drawing the content.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Transformation, Vector, mouse};

use crate::widgets::forward::forward_widget_body;

const MIN_VISIBLE_SCALE: f32 = 0.001;

fn is_invisible(scale: f32) -> bool {
    scale <= MIN_VISIBLE_SCALE
}

/// Scales content around its center without changing layout.
pub struct Scaled<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    scale: f32,
    origin: Origin,
    interactive: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Origin {
    #[default]
    Center,
    BottomLeft,
}

pub fn scaled<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    scale: f32,
) -> Scaled<'a, Message, Theme, Renderer> {
    Scaled::new(content).scale(scale)
}

impl<'a, Message, Theme, Renderer> Scaled<'a, Message, Theme, Renderer> {
    pub(crate) fn new(content: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            content: content.into(),
            scale: 1.0,
            origin: Origin::Center,
            interactive: true,
        }
    }

    pub(crate) fn scale(mut self, scale: f32) -> Self {
        self.scale = if scale.is_finite() {
            scale.max(0.0)
        } else {
            1.0
        };
        self
    }

    pub(crate) const fn origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub(crate) const fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    fn lifts_to_overlay(&self) -> bool {
        !is_invisible(self.scale) && (self.scale - 1.0).abs() > f32::EPSILON
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Scaled<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    forward_widget_body!(content; tag, state, children, diff, size, size_hint, layout, operate);

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if !self.interactive || is_invisible(self.scale) || self.lifts_to_overlay() {
            return;
        }

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.interactive || is_invisible(self.scale) || self.lifts_to_overlay() {
            return mouse::Interaction::None;
        }

        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if is_invisible(self.scale) {
            return;
        }

        if self.lifts_to_overlay() {
            return;
        }

        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if is_invisible(self.scale) {
            return None;
        }

        let bounds = layout.bounds();

        if self.lifts_to_overlay() {
            // Accepted tradeoff: while scale != 1.0, content renders in the
            // overlay layer and may draw above sibling overlays for the short
            // animation window. At rest, scale == 1.0 restores normal z-order.
            let origin = match self.origin {
                Origin::Center => (
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                ),
                Origin::BottomLeft => (bounds.x, bounds.y + bounds.height),
            };
            let transformed_origin = (origin.0 + translation.x, origin.1 + translation.y);
            let transformation =
                Transformation::translate(transformed_origin.0, transformed_origin.1)
                    * Transformation::scale(self.scale)
                    * Transformation::translate(-origin.0, -origin.1);

            Some(overlay::Element::new(Box::new(Overlay {
                scaled: self,
                state: tree,
                layout,
                viewport: *viewport,
                transformation,
            })))
        } else if self.interactive {
            self.content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        } else {
            None
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Scaled<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(scaled: Scaled<'a, Message, Theme, Renderer>) -> Self {
        Element::new(scaled)
    }
}

struct Overlay<'a, 'b, Message, Theme, Renderer> {
    scaled: &'a mut Scaled<'b, Message, Theme, Renderer>,
    state: &'a mut Tree,
    layout: Layout<'a>,
    viewport: Rectangle,
    transformation: Transformation,
}

impl<Message, Theme, Renderer> Overlay<'_, '_, Message, Theme, Renderer> {
    fn debug_assert_visible_scale(&self) {
        debug_assert!(
            self.scaled.scale > MIN_VISIBLE_SCALE,
            "scaled overlay should never be constructed for invisible scale {}",
            self.scaled.scale
        );
    }
}

impl<Message, Theme, Renderer> iced::advanced::Overlay<Message, Theme, Renderer>
    for Overlay<'_, '_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn layout(&mut self, _renderer: &Renderer, _bounds: Size) -> layout::Node {
        self.debug_assert_visible_scale();

        let bounds = self.layout.bounds() * self.transformation;
        layout::Node::new(bounds.size()).move_to(bounds.position())
    }

    fn update(
        &mut self,
        event: &Event,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.debug_assert_visible_scale();

        if !self.scaled.interactive {
            return;
        }

        let inverse = self.transformation.inverse();
        self.scaled.content.as_widget_mut().update(
            self.state,
            event,
            self.layout,
            cursor * inverse,
            renderer,
            clipboard,
            shell,
            &(self.viewport * inverse),
        );
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.debug_assert_visible_scale();

        let inverse = self.transformation.inverse();

        renderer.with_layer(self.viewport, |renderer| {
            renderer.with_transformation(self.transformation, |renderer| {
                self.scaled.content.as_widget().draw(
                    self.state,
                    renderer,
                    theme,
                    style,
                    self.layout,
                    cursor * inverse,
                    &(self.viewport * inverse),
                );
            });
        });
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.debug_assert_visible_scale();

        if !self.scaled.interactive || !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }

        let inverse = self.transformation.inverse();
        self.scaled.content.as_widget().mouse_interaction(
            self.state,
            self.layout,
            cursor * inverse,
            &(self.viewport * inverse),
            renderer,
        )
    }

    fn index(&self) -> f32 {
        self.debug_assert_visible_scale();

        self.scaled.scale.max(MIN_VISIBLE_SCALE) * 0.5
    }

    fn overlay<'a>(
        &'a mut self,
        _layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.debug_assert_visible_scale();

        if !self.scaled.interactive {
            return None;
        }

        self.scaled.content.as_widget_mut().overlay(
            self.state,
            self.layout,
            renderer,
            &(self.viewport * self.transformation.inverse()),
            self.transformation.translation(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_VISIBLE_SCALE, is_invisible};

    #[test]
    fn invisible_scale_boundaries_are_stable() {
        assert!(is_invisible(0.0));
        assert!(is_invisible(MIN_VISIBLE_SCALE));
        assert!(!is_invisible(1.0));
        assert!(!is_invisible(1.08));
    }
}
