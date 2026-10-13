use iced::advanced::widget::{Operation, Tree, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{
    Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, keyboard,
    mouse,
};

use crate::theme::Tokens;
use crate::widgets::forward::forward_widget_body;

pub fn keyboard_control<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    id: impl Into<iced::widget::Id>,
    tokens: &Tokens,
    on_key: impl Fn(&keyboard::Key) -> Option<Message> + 'a,
) -> Element<'a, Message> {
    Element::new(KeyboardControl {
        content: content.into(),
        id: id.into(),
        tokens: *tokens,
        on_key: Box::new(on_key),
    })
}

type KeyHandler<'a, Message> = Box<dyn Fn(&keyboard::Key) -> Option<Message> + 'a>;

struct KeyboardControl<'a, Message> {
    content: Element<'a, Message>,
    id: iced::widget::Id,
    tokens: Tokens,
    on_key: KeyHandler<'a, Message>,
}

#[derive(Default)]
struct FocusState {
    focused: bool,
    visible: bool,
}

impl Focusable for FocusState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
        self.visible = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
        self.visible = false;
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for KeyboardControl<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<FocusState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(FocusState::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    forward_widget_body!(content in children[0]; size, size_hint, layout, mouse_interaction, overlay);

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.focusable(
            Some(&self.id),
            layout.bounds(),
            tree.state.downcast_mut::<FocusState>(),
        );
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

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
        let focus = tree.state.downcast_mut::<FocusState>();
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            let focused = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            if focused != focus.focused || focus.visible {
                focus.focused = focused;
                focus.visible = false;
                shell.request_redraw();
            }
        }
        if focus.focused
            && let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && !modifiers.command()
            && !modifiers.alt()
            && let Some(message) = (self.on_key)(key)
        {
            focus.visible = true;
            shell.publish(message);
            shell.capture_event();
            shell.request_redraw();
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<FocusState>().visible {
            use iced::advanced::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: self.tokens.colors.focus_ring.into(),
                        width: self.tokens.dims.focus_border_width,
                        radius: self.tokens.radii.sm.into(),
                    },
                    ..Default::default()
                },
                Color::TRANSPARENT,
            );
        }
    }
}

pub fn activate(key: &keyboard::Key) -> bool {
    matches!(
        key,
        keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Space)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::operation::focusable;
    use iced_test::core::renderer::Headless as _;
    use iced_test::runtime::{UserInterface, user_interface};

    #[test]
    fn pointer_focus_is_quiet_and_keyboard_focus_remains_visible() {
        let tokens = Tokens::dark();
        let mut element = keyboard_control(
            iced::widget::Space::new().width(100).height(40),
            "focus-visibility",
            &tokens,
            |_| None::<u8>,
        );
        let mut tree = Tree::new(&element);
        let renderer = futures::executor::block_on(Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 40.0)),
        );
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(20.0, 20.0)),
            &renderer,
            &mut iced_test::core::clipboard::Null,
            &mut Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(100.0, 40.0)),
        );
        let state = tree.state.downcast_ref::<FocusState>();
        assert!(state.focused);
        assert!(!state.visible);
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut focusable::focus(iced::widget::Id::new("focus-visibility")),
        );
        assert!(tree.state.downcast_ref::<FocusState>().visible);
    }

    #[test]
    fn a_focused_range_accepts_keyboard_changes_without_pointer_hover() {
        let tokens = Tokens::dark();
        let element = keyboard_control(
            iced::widget::slider(90..=150, 100u16, |value| value),
            "keyboard-range",
            &tokens,
            |key| {
                matches!(key, keyboard::Key::Named(keyboard::key::Named::ArrowRight))
                    .then_some(105u16)
            },
        );
        let mut renderer = iced_test::futures::futures::executor::block_on(
            iced_test::renderer::Renderer::new(iced::Font::default(), iced::Pixels(16.0), None),
        )
        .expect("headless renderer");
        let mut ui = UserInterface::build(
            element,
            Size::new(300.0, 40.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        ui.operate(
            &renderer,
            &mut focusable::focus(iced::widget::Id::new("keyboard-range")),
        );
        let key = keyboard::Key::Named(keyboard::key::Named::ArrowRight);
        let event = Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::default(),
            text: None,
            repeat: false,
        });
        let mut messages = Vec::new();
        let _ = ui.update(
            &[event],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, vec![105]);
    }
}
