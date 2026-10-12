//! Capture the position on right-click so context menus do not need
//! continuous cursor tracking.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, mouse};

use crate::widgets::forward::forward_widget_body;

pub struct ContextArea<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_right_press: Box<dyn Fn(Point) -> Message + 'a>,
}

/// Scrollable cursors use content coordinates, but context menus need window
/// coordinates. Remember raw CursorMoved positions without publishing a message.
#[derive(Default)]
struct State {
    last_window_cursor: Option<Point>,
}

pub fn context_area<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_right_press: impl Fn(Point) -> Message + 'a,
) -> ContextArea<'a, Message, Theme, Renderer> {
    ContextArea {
        content: content.into(),
        on_right_press: Box::new(on_right_press),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ContextArea<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    forward_widget_body!(content in children[0]; size, size_hint, layout, operate);

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
        if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
            tree.state.downcast_mut::<State>().last_window_cursor = Some(*position);
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

        if shell.is_event_captured() {
            return;
        }

        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event
            && let Some(translated) = cursor.position_over(layout.bounds())
        {
            let position = tree
                .state
                .downcast_ref::<State>()
                .last_window_cursor
                .unwrap_or(translated);
            shell.publish((self.on_right_press)(position));
            shell.capture_event();
        }
    }

    forward_widget_body!(content in children[0]; mouse_interaction, draw, overlay);
}

impl<'a, Message, Theme, Renderer> From<ContextArea<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(area: ContextArea<'a, Message, Theme, Renderer>) -> Self {
        Element::new(area)
    }
}
