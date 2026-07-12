//! Shared boilerplate for wrapper widgets that forward most of the `Widget`
//! trait surface to a single wrapped `content` element, overriding only a
//! handful of methods.
//!
//! Two forwarding flavors, chosen per invocation:
//! - `forward_widget_body!(content; method, ...)` passes the `Tree` given to
//!   the wrapper straight through, for wrappers that are transparent to
//!   widget state (the content's `Tree` node is the wrapper's own).
//! - `forward_widget_body!(content in children[0]; method, ...)` forwards
//!   through `tree.children[0]`, for wrappers that keep a `Tree` node of
//!   their own and store the content's tree as its only child.

macro_rules! forward_widget_body {
    ($content:ident; $($method:ident),+ $(,)?) => {
        $(forward_widget_body!(@direct $content, $method);)+
    };
    ($content:ident in children[0]; $($method:ident),+ $(,)?) => {
        $(forward_widget_body!(@indexed $content, $method);)+
    };

    (@direct $content:ident, tag) => {
        fn tag(&self) -> tree::Tag {
            self.$content.as_widget().tag()
        }
    };
    (@direct $content:ident, state) => {
        fn state(&self) -> tree::State {
            self.$content.as_widget().state()
        }
    };
    (@direct $content:ident, children) => {
        fn children(&self) -> Vec<Tree> {
            self.$content.as_widget().children()
        }
    };
    (@direct $content:ident, diff) => {
        fn diff(&self, tree: &mut Tree) {
            self.$content.as_widget().diff(tree);
        }
    };
    (@direct $content:ident, size) => {
        fn size(&self) -> Size<Length> {
            self.$content.as_widget().size()
        }
    };
    (@direct $content:ident, size_hint) => {
        fn size_hint(&self) -> Size<Length> {
            self.$content.as_widget().size_hint()
        }
    };
    (@direct $content:ident, layout) => {
        fn layout(
            &mut self,
            tree: &mut Tree,
            renderer: &Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.$content.as_widget_mut().layout(tree, renderer, limits)
        }
    };
    (@direct $content:ident, operate) => {
        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &Renderer,
            operation: &mut dyn Operation,
        ) {
            self.$content
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }
    };
    (@direct $content:ident, mouse_interaction) => {
        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &Renderer,
        ) -> mouse::Interaction {
            self.$content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        }
    };
    (@direct $content:ident, draw) => {
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
            self.$content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    };
    (@direct $content:ident, overlay) => {
        fn overlay<'b>(
            &'b mut self,
            tree: &'b mut Tree,
            layout: Layout<'b>,
            renderer: &Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
            self.$content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        }
    };

    (@indexed $content:ident, size) => {
        fn size(&self) -> Size<Length> {
            self.$content.as_widget().size()
        }
    };
    (@indexed $content:ident, size_hint) => {
        fn size_hint(&self) -> Size<Length> {
            self.$content.as_widget().size_hint()
        }
    };
    (@indexed $content:ident, layout) => {
        fn layout(
            &mut self,
            tree: &mut Tree,
            renderer: &Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.$content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        }
    };
    (@indexed $content:ident, operate) => {
        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &Renderer,
            operation: &mut dyn Operation,
        ) {
            self.$content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation);
        }
    };
    (@indexed $content:ident, mouse_interaction) => {
        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &Renderer,
        ) -> mouse::Interaction {
            self.$content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        }
    };
    (@indexed $content:ident, draw) => {
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
            self.$content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        }
    };
    (@indexed $content:ident, overlay) => {
        fn overlay<'b>(
            &'b mut self,
            tree: &'b mut Tree,
            layout: Layout<'b>,
            renderer: &Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
            self.$content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            )
        }
    };
}

pub(crate) use forward_widget_body;
