use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Vector, mouse};

use crate::widgets::forward::forward_widget_body;

pub(super) fn translated<'a, Message: 'a>(
    content: Element<'a, Message>,
    offset: Vector,
) -> Element<'a, Message> {
    Element::new(Translated { content, offset })
}

struct Translated<'a, Message> {
    content: Element<'a, Message>,
    offset: Vector,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Translated<'_, Message> {
    forward_widget_body!(content; tag, state, children, diff, size, size_hint, layout, operate);

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree,
            event,
            layout,
            cursor - self.offset,
            renderer,
            clipboard,
            shell,
            &(*viewport - self.offset),
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        renderer.with_translation(self.offset, |renderer| {
            self.content.as_widget().draw(
                tree,
                renderer,
                theme,
                style,
                layout,
                cursor - self.offset,
                &(*viewport - self.offset),
            );
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            tree,
            layout,
            cursor - self.offset,
            &(*viewport - self.offset),
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            tree,
            layout,
            renderer,
            &(*viewport - self.offset),
            translation + self.offset,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::core::renderer::Headless as _;
    use iced_test::runtime::{UserInterface, user_interface};

    #[test]
    fn moving_card_receives_click_at_its_visible_position() {
        let element = iced::widget::container(translated(
            iced::widget::mouse_area(iced::widget::Space::new().width(100).height(100))
                .on_press(42u8)
                .into(),
            Vector::new(140.0, 0.0),
        ))
        .width(300)
        .height(100);
        let mut renderer = futures::executor::block_on(iced_test::renderer::Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            None,
        ))
        .expect("headless renderer");
        let mut ui = UserInterface::build(
            element,
            Size::new(300.0, 100.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(iced::Point::new(150.0, 20.0)),
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, vec![42]);
        messages.clear();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(iced::Point::new(20.0, 20.0)),
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty());
    }
}
