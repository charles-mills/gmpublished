use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

use crate::theme::Tokens;

pub fn settle<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    key: u64,
    tokens: &Tokens,
) -> Element<'a, Message> {
    Element::new(Settle {
        content: content.into(),
        key,
        duration: tokens.motion.layout_duration(),
    })
}

struct Settle<'a, Message> {
    content: Element<'a, Message>,
    key: u64,
    duration: Duration,
}

#[derive(Debug)]
struct MotionState {
    key: u64,
    started: Option<Instant>,
    offset: f32,
}

impl MotionState {
    fn tick(&mut self, now: Instant, duration: Duration) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        let previous = self.offset;
        self.offset = self.offset(now, duration);
        let settled = now.saturating_duration_since(started) >= duration;
        if settled {
            self.started = None;
        }
        self.offset != previous || settled
    }

    fn offset(&self, now: Instant, duration: Duration) -> f32 {
        self.started.map_or(0.0, |started| {
            if duration.is_zero() {
                return 0.0;
            }
            let remaining = 1.0
                - (now.saturating_duration_since(started).as_secs_f32() / duration.as_secs_f32())
                    .clamp(0.0, 1.0);
            4.0 * remaining.powi(3)
        })
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Settle<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<MotionState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(MotionState {
            key: self.key,
            started: None,
            offset: 0.0,
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<MotionState>();
        if state.key != self.key {
            state.key = self.key;
            state.started = (!self.duration.is_zero()).then(Instant::now);
            state.offset = if state.started.is_some() { 4.0 } else { 0.0 };
        }
        if self.duration.is_zero() {
            state.started = None;
            state.offset = 0.0;
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = child.size();
        let offset = tree.state.downcast_ref::<MotionState>().offset;
        layout::Node::with_children(size, vec![child.move_to(Point::new(0.0, offset))])
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }
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
        let state = tree.state.downcast_mut::<MotionState>();
        if let Event::Window(iced::window::Event::RedrawRequested(now)) = event {
            if state.tick(*now, self.duration) {
                shell.invalidate_layout();
            }
            if state.started.is_some() {
                shell.request_redraw_at(*now + Duration::from_millis(16));
            }
        }
        if let Some(clip) = viewport.intersection(&layout.bounds()) {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout.children().next().unwrap(),
                cursor,
                renderer,
                clipboard,
                shell,
                &clip,
            );
        }
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
        use iced::advanced::Renderer as _;
        if let Some(clip) = viewport.intersection(&layout.bounds()) {
            renderer.with_layer(clip, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout.children().next().unwrap(),
                    cursor,
                    &clip,
                );
            });
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let Some(clip) = viewport.intersection(&layout.bounds()) else {
            return mouse::Interaction::None;
        };
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor,
            &clip,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_replays_during_resize_converge_and_preserve_the_animation_clock() {
        use iced_test::core::renderer::Headless as _;
        use iced_test::runtime::{UserInterface, user_interface};
        let mut tokens = Tokens::dark();
        tokens.motion.layout_ms = 60_000;
        let content = |key| {
            settle(
                iced::widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fill),
                key,
                &tokens,
            )
        };
        let mut renderer = futures::executor::block_on(Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let ui = UserInterface::<u8, Theme, Renderer>::build(
            content(0),
            Size::new(100.0, 100.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        let mut ui = UserInterface::build(
            content(1),
            Size::new(100.0, 100.0),
            ui.into_cache(),
            &mut renderer,
        );
        let started = Instant::now();
        let mut messages = Vec::new();
        for frame in 1..=3 {
            ui = ui.relayout(Size::new(100.0 + frame as f32 * 20.0, 100.0), &mut renderer);
            let now = started + Duration::from_millis(frame * 20);
            let event = Event::Window(iced::window::Event::RedrawRequested(now));
            for replay in 0..4 {
                let (state, _) = ui.update(
                    std::slice::from_ref(&event),
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut iced_test::core::clipboard::Null,
                    &mut messages,
                );
                assert_eq!(state.has_layout_changed(), replay == 0);
                assert!(
                    matches!(state, user_interface::State::Updated { redraw_request: iced::window::RedrawRequest::At(at), .. } if at == now + Duration::from_millis(16))
                );
            }
        }
        let event = Event::Window(iced::window::Event::RedrawRequested(
            started + Duration::from_secs(61),
        ));
        let _ = ui.update(
            std::slice::from_ref(&event),
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        let (state, _) = ui.update(
            &[event],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        assert!(!state.has_layout_changed());
        assert!(matches!(
            state,
            user_interface::State::Updated {
                redraw_request: iced::window::RedrawRequest::Wait,
                ..
            }
        ));
    }

    #[test]
    fn settling_has_bounded_travel_and_an_exact_endpoint() {
        let started = Instant::now();
        let state = MotionState {
            key: 1,
            started: Some(started),
            offset: 4.0,
        };
        let duration = Duration::from_millis(180);
        assert_eq!(state.offset(started, duration), 4.0);
        assert!(state.offset(started + Duration::from_millis(90), duration) < 1.0);
        assert_eq!(state.offset(started + duration, duration), 0.0);
        assert_eq!(state.offset(started, Duration::ZERO), 0.0);
    }

    #[test]
    fn settling_content_uses_its_visible_bounds_for_clicks() {
        use iced_test::core::renderer::Headless as _;
        let tokens = Tokens::dark();
        let mut element = settle(
            iced::widget::button(iced::widget::Space::new().width(80).height(30))
                .padding(0)
                .on_press(1u8),
            1,
            &tokens,
        );
        let mut tree = Tree::new(&element);
        tree.state.downcast_mut::<MotionState>().started = Some(Instant::now());
        tree.state.downcast_mut::<MotionState>().offset = 4.0;
        let renderer = futures::executor::block_on(Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let layout = Layout::new(&node);
        assert!(layout.children().next().unwrap().bounds().y > 0.0);
        let viewport = Rectangle::with_size(Size::new(100.0, 100.0));
        let mut messages = Vec::new();
        for position in [Point::new(10.0, 0.0), Point::new(10.0, 10.0)] {
            for event in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                element.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(event),
                    layout,
                    mouse::Cursor::Available(position),
                    &renderer,
                    &mut iced_test::core::clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            if position.y == 0.0 {
                assert!(messages.is_empty());
            }
        }
        assert_eq!(messages, [1]);
    }
}
