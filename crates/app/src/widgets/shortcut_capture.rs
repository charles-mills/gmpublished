//! Captures global keyboard shortcuts ahead of the widget tree.
//!
//! Subscription listeners (`event::listen_with`) observe key events only
//! after the widget tree has processed them, so a focused text input can
//! consume the keystroke first — e.g. a closing ⌘F inserting a stray "f"
//! into the search palette for a frame (iced's `text_input` tracks
//! modifiers in widget state that a freshly remounted input starts
//! without). This wrapper sees the event before any child, publishes the
//! mapped message, and captures the event so no child ever receives it.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector, keyboard, mouse};

use crate::widgets::forward::forward_widget_body;

/// Maps a key press to a message, or `None` to let it pass through.
type Mapper<Message> = fn(&keyboard::Key, keyboard::Modifiers) -> Option<Message>;

pub struct ShortcutCapture<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    mapper: Option<Mapper<Message>>,
}

pub fn shortcut_capture<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    mapper: Option<Mapper<Message>>,
) -> ShortcutCapture<'a, Message, Theme, Renderer> {
    ShortcutCapture {
        content: content.into(),
        mapper,
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ShortcutCapture<'_, Message, Theme, Renderer>
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
        if let Some(mapper) = self.mapper
            && let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && let Some(message) = mapper(key, *modifiers)
        {
            shell.publish(message);
            shell.capture_event();
            return;
        }

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    forward_widget_body!(content; mouse_interaction, draw, overlay);
}

impl<'a, Message, Theme, Renderer> From<ShortcutCapture<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(capture: ShortcutCapture<'a, Message, Theme, Renderer>) -> Self {
        Element::new(capture)
    }
}
