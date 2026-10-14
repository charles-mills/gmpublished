use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::widget::{button, column, container, row, scrollable, svg, text};
use iced::{Element, Event, Length, Padding, Rectangle, Size, Vector, mouse};

use crate::assets;
use crate::theme::{self, Tokens};

/// Line height iced's `text` widget applies by default; option rows are sized
/// with it so the caller can predict the menu's height before layout.
const LINE_HEIGHT: f32 = 1.3;

pub const MAX_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub text_size: f32,
    pub padding: Padding,
    pub radius: f32,
    pub max_rows: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Face {
    Collapsed,
    Expanded,
}

impl Face {
    pub(crate) fn is_expanded(self) -> bool {
        self == Self::Expanded
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Chevron {
    Shown,
    Hidden,
}

pub struct Row<Message> {
    pub label: String,
    pub on_select: Message,
    pub selected: bool,
}

/// Option labels must not wrap; menu height assumes a fixed row height.
pub fn row_height(metrics: Metrics) -> f32 {
    metrics.padding.y() + metrics.text_size * LINE_HEIGHT
}

/// Inset of the option list inside the panel. iced's scrollbar runs the full
/// height of its viewport, so without this the rail's rounded ends are sliced
/// off by the panel's own rounded corners.
fn list_inset(metrics: Metrics) -> f32 {
    metrics.radius
}

pub fn menu_height(rows: usize, metrics: Metrics) -> f32 {
    rows.min(metrics.max_rows) as f32 * row_height(metrics) + list_inset(metrics) * 2.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub height: f32,
    /// How far the panel is pulled up over the control. iced borders are one
    /// width on every side, so without this the control's bottom stroke and the
    /// panel's top stroke stack into a seam twice as heavy as the rest of the
    /// ring; overlapping them by exactly one width leaves a single line.
    pub seam: f32,
    pub enter_duration: Duration,
}

pub fn geometry(rows: usize, metrics: Metrics, tokens: &Tokens) -> Geometry {
    Geometry {
        height: menu_height(rows, metrics),
        seam: tokens.dims.focus_border_width,
        enter_duration: tokens.motion.context_menu_enter_duration(),
    }
}

pub fn menu<'a, Message: Clone + 'a>(
    rows: Vec<Row<Message>>,
    metrics: Metrics,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let mut options = column![].width(Length::Fill);
    for row in rows {
        let selected = row.selected;
        options = options.push(
            button(
                text(row.label)
                    .size(metrics.text_size)
                    .line_height(LINE_HEIGHT)
                    .wrapping(text::Wrapping::None)
                    .color(iced::Color::from(if selected {
                        tokens.colors.text_on_neutral
                    } else {
                        tokens.colors.text
                    })),
            )
            .width(Length::Fill)
            .padding(metrics.padding)
            .on_press(row.on_select)
            .style(move |_, status| theme::styles::select_option(&tokens, selected, status)),
        );
    }

    container(
        scrollable(options)
            .height(Length::Shrink)
            .direction(scrollable::Direction::Vertical(
                theme::styles::vertical_scrollbar(&tokens),
            ))
            .style(move |_, status| theme::styles::select_menu_scrollbar(&tokens, status)),
    )
    .width(Length::Fill)
    // A row's fill runs the full width it is given, and iced paints children
    // over the container's own border, so the list is held off the panel's
    // edges: vertically clear of the rounded corners (which would otherwise
    // slice the scrollbar's rail), horizontally clear of the ring itself.
    .padding(
        Padding::ZERO
            .vertical(list_inset(metrics))
            .horizontal(tokens.dims.focus_border_width),
    )
    .clip(true)
    .style(move |_| theme::styles::select_menu(&tokens, metrics.radius))
    .into()
}

pub fn field<'a, Message: Clone + 'a>(
    label: String,
    rows: Vec<Row<Message>>,
    metrics: Metrics,
    chevron: Chevron,
    tokens: &Tokens,
) -> Element<'a, Message> {
    field_with_icon(label, rows, metrics, chevron, None, tokens)
}

pub fn field_with_icon<'a, Message: Clone + 'a>(
    label: String,
    rows: Vec<Row<Message>>,
    metrics: Metrics,
    chevron: Chevron,
    icon: Option<svg::Handle>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let geometry = geometry(rows.len(), metrics, tokens);

    select(
        face(
            label.clone(),
            Face::Collapsed,
            metrics,
            chevron,
            icon.clone(),
            tokens,
        ),
        face(label, Face::Expanded, metrics, chevron, icon, tokens),
        menu(rows, metrics, tokens),
        geometry,
    )
    .into()
}

fn face<'a, Message: 'a>(
    label: String,
    face: Face,
    metrics: Metrics,
    chevron: Chevron,
    icon: Option<svg::Handle>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let mut content = row![].spacing(tokens.spacing.gap_sm).align_y(iced::Center);
    if let Some(icon) = icon {
        content = content.push(
            svg(icon)
                .width(tokens.dims.icon_size.min(metrics.text_size * LINE_HEIGHT))
                .height(tokens.dims.icon_size.min(metrics.text_size * LINE_HEIGHT))
                .style(move |_, _| svg::Style {
                    color: Some(tokens.colors.text.into()),
                }),
        );
    }
    content = content.push(
        text(label)
            .size(metrics.text_size)
            .line_height(LINE_HEIGHT)
            .wrapping(text::Wrapping::None)
            .color(iced::Color::from(tokens.colors.text)),
    );
    if chevron == Chevron::Shown {
        content = content
            .push(iced::widget::Space::new().width(Length::Fill))
            .push(
                svg(assets::icons::chevron_down())
                    .width(tokens.dims.icon_size_sm)
                    .height(tokens.dims.icon_size_sm)
                    .style(move |_, _| svg::Style {
                        color: Some(tokens.colors.text.into()),
                    }),
            );
    }

    container(content)
        .width(Length::Fill)
        .padding(metrics.padding)
        .clip(true)
        .style(move |_| theme::styles::select_face(&tokens, face.is_expanded()))
        .into()
}

#[derive(Default)]
struct State {
    is_open: bool,
    opening_started: Option<Instant>,
    offset: f32,
}

impl State {
    fn open(&mut self, now: Instant, duration: Duration) {
        self.is_open = true;
        self.opening_started = (!duration.is_zero()).then_some(now);
        self.offset = if duration.is_zero() { 0.0 } else { 4.0 };
    }

    fn tick(&mut self, now: Instant, duration: Duration) -> bool {
        let Some(started) = self.opening_started else {
            return false;
        };
        let previous_offset = self.offset;
        let elapsed = now.saturating_duration_since(started);
        if duration.is_zero() || elapsed >= duration {
            self.opening_started = None;
            self.offset = 0.0;
        } else {
            self.offset = 4.0 * (1.0 - elapsed.as_secs_f32() / duration.as_secs_f32()).powi(3);
        }
        self.offset != previous_offset || self.opening_started.is_none()
    }
}

const CLOSED: usize = 0;
const OPEN: usize = 1;
const MENU: usize = 2;

pub struct Select<'a, Message, Theme, Renderer> {
    closed: Element<'a, Message, Theme, Renderer>,
    open: Element<'a, Message, Theme, Renderer>,
    menu: Element<'a, Message, Theme, Renderer>,
    geometry: Geometry,
}

/// `closed` and `open` must have the same size; layout is measured from `closed`.
pub fn select<'a, Message, Theme, Renderer>(
    closed: impl Into<Element<'a, Message, Theme, Renderer>>,
    open: impl Into<Element<'a, Message, Theme, Renderer>>,
    menu: impl Into<Element<'a, Message, Theme, Renderer>>,
    geometry: Geometry,
) -> Select<'a, Message, Theme, Renderer> {
    Select {
        closed: closed.into(),
        open: open.into(),
        menu: menu.into(),
        geometry,
    }
}

impl<'a, Message, Theme, Renderer> Select<'a, Message, Theme, Renderer> {
    fn control(&self, is_open: bool) -> (&Element<'a, Message, Theme, Renderer>, usize) {
        if is_open {
            (&self.open, OPEN)
        } else {
            (&self.closed, CLOSED)
        }
    }

    fn control_mut(
        &mut self,
        is_open: bool,
    ) -> (&mut Element<'a, Message, Theme, Renderer>, usize) {
        if is_open {
            (&mut self.open, OPEN)
        } else {
            (&mut self.closed, CLOSED)
        }
    }
}

fn is_open(tree: &Tree) -> bool {
    tree.state.downcast_ref::<State>().is_open
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Select<'_, Message, Theme, Renderer>
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
        vec![
            Tree::new(&self.closed),
            Tree::new(&self.open),
            Tree::new(&self.menu),
        ]
    }

    fn diff(&self, tree: &mut Tree) {
        if self.geometry.enter_duration.is_zero() {
            let state = tree.state.downcast_mut::<State>();
            state.opening_started = None;
            state.offset = 0.0;
        }
        tree.diff_children(&[&self.closed, &self.open, &self.menu]);
    }

    fn size(&self) -> Size<Length> {
        self.closed.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.closed.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let (control, index) = self.control_mut(is_open(tree));
        control
            .as_widget_mut()
            .layout(&mut tree.children[index], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let (control, index) = self.control_mut(is_open(tree));
        control
            .as_widget_mut()
            .operate(&mut tree.children[index], layout, renderer, operation);
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
        let (control, index) = self.control(is_open(tree));
        control.as_widget().draw(
            &tree.children[index],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
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
        let open = is_open(tree);
        let (control, index) = self.control_mut(open);
        control.as_widget_mut().update(
            &mut tree.children[index],
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

        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && cursor.is_over(layout.bounds())
        {
            let state = tree.state.downcast_mut::<State>();
            if open {
                state.is_open = false;
                state.opening_started = None;
                state.offset = 0.0;
            } else {
                state.open(Instant::now(), self.geometry.enter_duration);
            }
            shell.capture_event();
            shell.invalidate_layout();
            shell.request_redraw();
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let Tree {
            state: tree_state,
            children,
            ..
        } = tree;

        // Hide the control's tooltip while the menu is open.
        if !tree_state.downcast_ref::<State>().is_open {
            return self.closed.as_widget_mut().overlay(
                &mut children[CLOSED],
                layout,
                renderer,
                viewport,
                translation,
            );
        }

        let state = tree_state.downcast_mut::<State>();

        Some(overlay::Element::new(Box::new(Menu {
            content: &mut self.menu,
            tree: &mut children[MENU],
            state,
            anchor: layout.bounds() + translation,
            limit: *viewport,
            geometry: self.geometry,
        })))
    }
}

struct Menu<'a, 'b, Message, Theme, Renderer> {
    content: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    state: &'b mut State,
    anchor: Rectangle,
    /// What the control is clipped into, in window space: a `scrollable` hands
    /// its visible bounds down, so the menu stays inside the panel it belongs
    /// to instead of spilling past its edge.
    limit: Rectangle,
    geometry: Geometry,
}

impl<Message, Theme, Renderer> Menu<'_, '_, Message, Theme, Renderer> {
    fn bounds(&self, window: Size) -> Rectangle {
        menu_bounds(self.anchor, self.limit, self.geometry, window)
    }
}

/// Prefer the containing panel, then flip above the field when the bottom
/// edge cannot accommodate the menu. Always constrain the scroll viewport.
fn menu_bounds(anchor: Rectangle, limit: Rectangle, geometry: Geometry, window: Size) -> Rectangle {
    let below_top = anchor.y + anchor.height - geometry.seam;
    let below = (window.height - below_top).max(0.0);
    let above = (anchor.y + geometry.seam).max(0.0);
    let in_panel = (limit.y + limit.height - below_top).max(0.0);
    let open_above = geometry.height > in_panel && geometry.height > below && above > below;
    let height = geometry.height.min(if open_above { above } else { below });
    let width = anchor.width.min(window.width).max(0.0);
    Rectangle {
        x: anchor.x.clamp(0.0, (window.width - width).max(0.0)),
        y: if open_above {
            (anchor.y + geometry.seam - height).max(0.0)
        } else {
            below_top.max(0.0)
        },
        width,
        height,
    }
}

fn opening_offset(panel: Rectangle, anchor: Rectangle, remaining: f32) -> f32 {
    if panel.y < anchor.y {
        remaining
    } else {
        -remaining
    }
}

fn clipped_cursor(cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
    if cursor.is_over(bounds) {
        cursor
    } else {
        mouse::Cursor::Unavailable
    }
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for Menu<'_, '_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let placement = self.bounds(bounds);
        let limits = layout::Limits::new(Size::new(placement.width, 0.0), placement.size());
        let child = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = child.size();
        let offset = opening_offset(placement, self.anchor, self.state.offset);
        layout::Node::with_children(size, vec![child.move_to(iced::Point::new(0.0, offset))])
            .move_to(placement.position())
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        renderer.with_layer(layout.bounds(), |renderer| {
            self.content.as_widget().draw(
                self.tree,
                renderer,
                theme,
                style,
                layout.children().next().unwrap(),
                cursor,
                &layout.bounds(),
            );
        });
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(
            self.tree,
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let bounds = layout.bounds();
        if let Event::Window(iced::window::Event::RedrawRequested(now)) = event {
            if self.state.tick(*now, self.geometry.enter_duration) {
                shell.invalidate_layout();
            }
            if self.state.opening_started.is_some() {
                shell.request_redraw_at(*now + Duration::from_millis(16));
            }
        }

        // A local shell tells us whether an option was actually chosen: only
        // then does the menu close. Pressing the scrollbar publishes nothing,
        // so dragging it keeps the menu up.
        let mut messages = Vec::new();
        let mut inner = Shell::new(&mut messages);
        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout.children().next().unwrap(),
            clipped_cursor(cursor, bounds),
            renderer,
            clipboard,
            &mut inner,
            &bounds,
        );
        let chose_option = !inner.is_empty();
        shell.merge(inner, |message| message);

        if chose_option {
            self.state.is_open = false;
            self.state.opening_started = None;
            shell.invalidate_layout();
            shell.request_redraw();
            return;
        }

        // Leave Escape to the app subscription, which ignores event status. Handling it
        // here would close both menu and modal.
        let dismissed = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && cursor.position_over(bounds).is_none();

        if dismissed {
            self.state.is_open = false;
            self.state.opening_started = None;
            shell.capture_event();
            shell.invalidate_layout();
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            self.tree,
            layout.children().next().unwrap(),
            clipped_cursor(cursor, layout.bounds()),
            &layout.bounds(),
            renderer,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Select<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(select: Select<'a, Message, Theme, Renderer>) -> Self {
        Element::new(select)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use iced::{Padding, Point, Rectangle, Size, mouse};

    use super::{
        Geometry, Metrics, State, clipped_cursor, menu_bounds, menu_height, opening_offset,
        row_height,
    };

    const METRICS: Metrics = Metrics {
        text_size: 14.0,
        padding: Padding {
            top: 11.0,
            right: 12.0,
            bottom: 11.0,
            left: 12.0,
        },
        radius: 6.0,
        max_rows: 8,
    };

    #[test]
    fn dropdown_redraw_replays_converge_and_keep_the_next_frame_scheduled() {
        use iced_test::core::renderer::Headless as _;
        use iced_test::runtime::{UserInterface, user_interface};
        let mut tokens = crate::theme::Tokens::dark();
        // Font initialization must not consume the synthetic animation interval.
        tokens.motion.context_menu_enter_ms = 60_000;
        let element = super::field(
            "First".into(),
            vec![
                super::Row {
                    label: "First".into(),
                    on_select: 1u8,
                    selected: true,
                },
                super::Row {
                    label: "Second".into(),
                    on_select: 2,
                    selected: false,
                },
            ],
            METRICS,
            super::Chevron::Shown,
            &tokens,
        );
        let mut renderer = futures::executor::block_on(iced::Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut ui = UserInterface::build(
            element,
            Size::new(300.0, 300.0),
            user_interface::Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(20.0, 20.0)),
            &mut renderer,
            &mut iced_test::core::clipboard::Null,
            &mut messages,
        );
        let now = Instant::now() + Duration::from_millis(40);
        let event = iced::Event::Window(iced::window::Event::RedrawRequested(now));
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
                matches!(state, user_interface::State::Updated { redraw_request, .. }
                if redraw_request <= iced::window::RedrawRequest::At(now + Duration::from_millis(16))),
                "{state:?}"
            );
        }
    }

    #[test]
    fn menu_flips_above_a_field_near_the_bottom_of_the_window() {
        let anchor = Rectangle {
            x: 600.0,
            y: 520.0,
            width: 250.0,
            height: 40.0,
        };
        let window = Size::new(800.0, 600.0);
        let bounds = menu_bounds(
            anchor,
            Rectangle::with_size(window),
            Geometry {
                height: 300.0,
                seam: 2.0,
                enter_duration: Duration::from_millis(120),
            },
            window,
        );
        assert_eq!(bounds.y, 222.0);
        assert_eq!(bounds.x, 550.0);
        assert_eq!(bounds.height, 300.0);
        assert!(bounds.y + bounds.height <= anchor.y + 2.0);
    }

    #[test]
    fn oversized_menu_has_a_reachable_scrolling_viewport() {
        let anchor = Rectangle {
            x: 10.0,
            y: 200.0,
            width: 1000.0,
            height: 60.0,
        };
        let window = Size::new(800.0, 600.0);
        let bounds = menu_bounds(
            anchor,
            Rectangle::with_size(window),
            Geometry {
                height: 1000.0,
                seam: 2.0,
                enter_duration: Duration::from_millis(120),
            },
            window,
        );
        assert_eq!(
            bounds,
            Rectangle {
                x: 0.0,
                y: 258.0,
                width: 800.0,
                height: 342.0
            }
        );
    }

    #[test]
    fn row_height_covers_padding_and_the_full_text_line() {
        assert_eq!(row_height(METRICS), 22.0 + 14.0 * 1.3);
    }

    #[test]
    fn menu_height_shows_every_option_up_to_the_cap() {
        let inset = 2.0 * METRICS.radius;

        assert_eq!(menu_height(3, METRICS), 3.0 * row_height(METRICS) + inset);
        assert_eq!(menu_height(20, METRICS), 8.0 * row_height(METRICS) + inset);
    }

    #[test]
    fn opening_settles_within_four_pixels_and_stops_requesting_frames() {
        let mut state = State::default();
        let now = Instant::now();
        let duration = Duration::from_millis(120);
        state.open(now, duration);
        assert_eq!(state.offset, 4.0);
        assert!(state.tick(now + duration / 2, duration));
        assert!((state.offset - 0.5).abs() < 0.001);
        assert!(state.tick(now + duration, duration));
        assert_eq!(state.offset, 0.0);
        assert!(!state.tick(now + duration * 2, duration));
        assert!(state.is_open);
    }

    #[test]
    fn reduced_motion_opens_without_a_frame_gate() {
        let mut state = State::default();
        let now = Instant::now();
        state.open(now, Duration::ZERO);
        assert!(state.is_open);
        assert_eq!(state.offset, 0.0);
        assert!(!state.tick(now, Duration::ZERO));
    }

    #[test]
    fn motion_moves_away_from_anchor_without_expanding_pointer_bounds() {
        let anchor = Rectangle {
            x: 20.0,
            y: 200.0,
            width: 200.0,
            height: 40.0,
        };
        let below = Rectangle {
            y: 238.0,
            height: 100.0,
            ..anchor
        };
        let above = Rectangle {
            y: 102.0,
            height: 100.0,
            ..anchor
        };
        assert_eq!(opening_offset(below, anchor, 4.0), -4.0);
        assert_eq!(opening_offset(above, anchor, 4.0), 4.0);
        assert_eq!(opening_offset(above, anchor, 0.0), 0.0);
        let inside = mouse::Cursor::Available(Point::new(30.0, 239.0));
        assert_eq!(clipped_cursor(inside, below), inside);
        let clipped = mouse::Cursor::Available(Point::new(30.0, 236.0));
        assert_eq!(clipped_cursor(clipped, below), mouse::Cursor::Unavailable);
    }
}
