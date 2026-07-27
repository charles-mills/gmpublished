//! A dropdown whose menu is ordinary app-owned chrome.
//!
//! `pick_list` builds its menu's scrollable internally and exposes no hook to
//! restyle it, so those menus always carry iced's default scrollbar (wide,
//! full-height rail) instead of the app's inset pill. Its overlay also opens on
//! whichever side of the control has more room, which drops upward for anything
//! below the middle of the window.
//!
//! This widget owns the placement and hands the menu itself back to the caller
//! as a plain `Element`, so the panel, its option rows and its scrollbar are
//! styled with the same helpers as the rest of the app. The menu always hangs
//! downward, welded to the control's bottom edge.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::widget::{button, column, container, row, scrollable, svg, text};
use iced::{Element, Event, Length, Padding, Point, Rectangle, Size, Vector, mouse};

use crate::assets;
use crate::theme::{self, Tokens};

/// Line height iced's `text` widget applies by default; option rows are sized
/// with it so the caller can predict the menu's height before layout.
const LINE_HEIGHT: f32 = 1.3;

/// Options a menu shows before it starts scrolling.
pub const MAX_ROWS: usize = 8;

/// Chrome sizing for a select and its menu; option rows are padded like the
/// control so the two read at the same height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub text_size: f32,
    pub padding: Padding,
    pub radius: f32,
    /// Options shown before the menu starts scrolling.
    pub max_rows: usize,
}

/// Whether a `field` control carries a chevron on its trailing edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Chevron {
    Shown,
    Hidden,
}

/// One option in a select menu.
pub struct Row<Message> {
    pub label: String,
    pub on_select: Message,
    pub selected: bool,
}

/// Height of one option row, matching how the row's `text` is laid out.
///
/// Exact only because rows never wrap — labels can come from a model file or a
/// long translation, and a wrapped row would silently break every height the
/// menu predicts from this.
pub fn row_height(metrics: Metrics) -> f32 {
    metrics.padding.y() + metrics.text_size * LINE_HEIGHT
}

/// Inset of the option list inside the panel. iced's scrollbar runs the full
/// height of its viewport, so without this the rail's rounded ends are sliced
/// off by the panel's own rounded corners.
fn list_inset(metrics: Metrics) -> f32 {
    metrics.radius
}

/// Height the menu will ask for: every option, up to `max_rows`.
pub fn menu_height(rows: usize, metrics: Metrics) -> f32 {
    rows.min(metrics.max_rows) as f32 * row_height(metrics) + list_inset(metrics) * 2.0
}

/// How the menu sits against its control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Height the panel asks for before it is clamped to the room available.
    pub height: f32,
    /// How far the panel is pulled up over the control. iced borders are one
    /// width on every side, so without this the control's bottom stroke and the
    /// panel's top stroke stack into a seam twice as heavy as the rest of the
    /// ring; overlapping them by exactly one width leaves a single line.
    pub seam: f32,
}

/// Geometry for a menu of `rows` options built with `metrics`.
pub fn geometry(rows: usize, metrics: Metrics, tokens: &Tokens) -> Geometry {
    Geometry {
        height: menu_height(rows, metrics),
        seam: tokens.dims.focus_border_width,
    }
}

/// Builds the floating panel: one row per option over the app's scrollbar.
///
/// The panel hangs flush off the control's bottom edge, so its top corners stay
/// square and only the free edge is rounded.
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

/// The standard app dropdown: a control showing the current label, dropping a
/// menu of every option beneath it.
pub fn field<'a, Message: Clone + 'a>(
    label: String,
    rows: Vec<Row<Message>>,
    metrics: Metrics,
    chevron: Chevron,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let geometry = geometry(rows.len(), metrics, tokens);

    select(
        face(label.clone(), false, metrics, chevron, tokens),
        face(label, true, metrics, chevron, tokens),
        menu(rows, metrics, tokens),
        geometry,
    )
    .into()
}

/// The control's face: the current label, and a chevron if asked for.
fn face<'a, Message: 'a>(
    label: String,
    open: bool,
    metrics: Metrics,
    chevron: Chevron,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let label = text(label)
        .size(metrics.text_size)
        .line_height(LINE_HEIGHT)
        .wrapping(text::Wrapping::None)
        .color(iced::Color::from(tokens.colors.text));

    let content: Element<'_, Message> = if chevron == Chevron::Shown {
        row![
            label,
            iced::widget::Space::new().width(Length::Fill),
            svg(assets::icons::chevron_down())
                .width(Length::Fixed(tokens.dims.icon_size_sm))
                .height(Length::Fixed(tokens.dims.icon_size_sm))
                .style(move |_, _| svg::Style {
                    color: Some(tokens.colors.text.into()),
                }),
        ]
        .align_y(iced::Center)
        .into()
    } else {
        label.into()
    };

    container(content)
        .width(Length::Fill)
        .padding(metrics.padding)
        .clip(true)
        .style(move |_| theme::styles::select_face(&tokens, open))
        .into()
}

#[derive(Default)]
struct State {
    is_open: bool,
}

const CLOSED: usize = 0;
const OPEN: usize = 1;
const MENU: usize = 2;

pub struct Select<'a, Message, Theme, Renderer> {
    /// The control as it looks while shut, and while its menu is down. Both are
    /// built up front because the open state lives in the widget's own tree.
    closed: Element<'a, Message, Theme, Renderer>,
    open: Element<'a, Message, Theme, Renderer>,
    menu: Element<'a, Message, Theme, Renderer>,
    geometry: Geometry,
}

/// A control that drops `menu` beneath itself when pressed.
///
/// `closed` and `open` are the same control in its two states and must lay out
/// to the same size: only one is in the tree per frame, but `size`/`size_hint`
/// always answer from `closed`.
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
            tree.state.downcast_mut::<State>().is_open = !open;
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

        // Shut, the control keeps its own overlay — that is where its tooltip
        // lives. Open, the menu replaces it: a hint hanging over the options it
        // describes is worse than no hint.
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
    /// The control the menu hangs off, in window space.
    anchor: Rectangle,
    /// What the control is clipped into, in window space: a `scrollable` hands
    /// its visible bounds down, so the menu stays inside the panel it belongs
    /// to instead of spilling past its edge.
    limit: Rectangle,
    geometry: Geometry,
}

impl<Message, Theme, Renderer> Menu<'_, '_, Message, Theme, Renderer> {
    /// The box the panel gets to fill: exactly the control's width, and the room
    /// under it.
    ///
    /// The menu only ever hangs downward — it shares the control's fill, ring
    /// and squared-off top edge, so flipping it above would leave a detached box
    /// sitting over the fields it isn't part of. It prefers the panel the
    /// control is clipped into, and spills past that edge only when the whole
    /// menu will not fit inside it.
    fn limits(&self, window: Size) -> layout::Limits {
        let top = self.top();
        let in_panel = (self.limit.y + self.limit.height) - top;
        let height = self.geometry.height;
        let room = if height <= in_panel {
            in_panel
        } else {
            window.height - top
        };

        layout::Limits::new(
            Size::new(self.anchor.width, 0.0),
            Size::new(self.anchor.width, height.min(room).max(0.0)),
        )
    }

    /// Where the panel starts: the control's bottom edge, less the overlap that
    /// merges the two rings into one stroke.
    fn top(&self) -> f32 {
        self.anchor.y + self.anchor.height - self.geometry.seam
    }
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for Menu<'_, '_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let limits = self.limits(bounds);
        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);

        node.move_to(Point::new(self.anchor.x, self.top()))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.content.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
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

        // A local shell tells us whether an option was actually chosen: only
        // then does the menu close. Pressing the scrollbar publishes nothing,
        // so dragging it keeps the menu up.
        let mut messages = Vec::new();
        let mut inner = Shell::new(&mut messages);
        self.content.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, &mut inner, &bounds,
        );
        let chose_option = !inner.is_empty();
        shell.merge(inner, |message| message);

        if chose_option {
            self.state.is_open = false;
            shell.invalidate_layout();
            shell.request_redraw();
            return;
        }

        // Escape is deliberately not handled here: the app's Escape listeners
        // are `event::listen_with` subscriptions that ignore `event::Status`, so
        // capturing it would close the menu *and* unwind the modal underneath.
        let dismissed = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && cursor.position_over(bounds).is_none();

        if dismissed {
            self.state.is_open = false;
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
            layout,
            cursor,
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
    use iced::Padding;

    use super::{Metrics, menu_height, row_height};

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
    fn row_height_covers_padding_and_the_full_text_line() {
        assert_eq!(row_height(METRICS), 22.0 + 14.0 * 1.3);
    }

    /// The list is inset top and bottom so the scrollbar's rounded ends clear
    /// the panel's corners; that inset is part of the height the menu asks for.
    #[test]
    fn menu_height_shows_every_option_up_to_the_cap() {
        let inset = 2.0 * METRICS.radius;

        assert_eq!(menu_height(3, METRICS), 3.0 * row_height(METRICS) + inset);
        assert_eq!(menu_height(20, METRICS), 8.0 * row_height(METRICS) + inset);
    }
}
