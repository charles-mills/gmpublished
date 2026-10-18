use iced::widget::{canvas, stack};
use iced::{Color, Element, Length, Point, Radians, Rectangle, Vector, mouse};

pub fn rounded<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    radius: f32,
    background: Color,
) -> Element<'a, Message> {
    stack![
        content.into(),
        canvas(Corners { radius, background })
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .into()
}

struct Corners {
    radius: f32,
    background: Color,
}

impl<Message> canvas::Program<Message> for Corners {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let radius = self.radius.min(bounds.width * 0.5).min(bounds.height * 0.5);
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        // Container clipping is rectangular, so cover the shader's corners with
        // the surrounding surface rather than rounding only its background.
        let corner = canvas::Path::new(|path| {
            path.move_to(Point::ORIGIN);
            path.line_to(Point::new(radius, 0.0));
            let control = radius * (1.0 - 0.552_284_8);
            path.bezier_curve_to(
                Point::new(control, 0.0),
                Point::new(0.0, control),
                Point::new(0.0, radius),
            );
            path.close();
        });
        for (x, y, angle) in [
            (0.0, 0.0, 0.0),
            (bounds.width, 0.0, std::f32::consts::FRAC_PI_2),
            (bounds.width, bounds.height, std::f32::consts::PI),
            (0.0, bounds.height, -std::f32::consts::FRAC_PI_2),
        ] {
            frame.with_save(|frame| {
                frame.translate(Vector::new(x, y));
                frame.rotate(Radians(angle));
                frame.fill(&corner, self.background);
            });
        }
        vec![frame.into_geometry()]
    }
}
