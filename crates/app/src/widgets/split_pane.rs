use iced::widget::{Space, mouse_area, pane_grid, row};
use iced::{Background, Border, Color, Element, Length, mouse};

use crate::theme::Tokens;

pub const DIVIDER_WIDTH: f32 = 2.0;
pub const GRAB_LEEWAY: f32 = 8.0;

#[derive(Clone, Debug)]
pub struct State<T> {
    grid: pane_grid::State<T>,
    split: pane_grid::Split,
    ratio: f32,
    default_ratio: f32,
}

impl<T> State<T> {
    pub fn vertical(first: T, second: T, default_ratio: f32) -> Self {
        let grid = pane_grid::State::with_configuration(pane_grid::Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: default_ratio,
            a: Box::new(pane_grid::Configuration::Pane(first)),
            b: Box::new(pane_grid::Configuration::Pane(second)),
        });
        let split = *grid.layout().splits().next().expect("split configuration");
        Self {
            grid,
            split,
            ratio: default_ratio,
            default_ratio,
        }
    }

    pub const fn grid(&self) -> &pane_grid::State<T> {
        &self.grid
    }

    pub const fn ratio(&self) -> f32 {
        self.ratio
    }

    pub fn resize(&mut self, split: pane_grid::Split, ratio: f32) {
        if split != self.split || !ratio.is_finite() {
            return;
        }
        self.ratio = ratio.clamp(0.0, 1.0);
        self.grid.resize(self.split, self.ratio);
    }

    pub fn set_ratio(&mut self, ratio: f32) {
        self.resize(self.split, ratio);
    }

    pub fn reset(&mut self) {
        self.resize(self.split, self.default_ratio);
    }
}

impl<T: PartialEq> PartialEq for State<T> {
    fn eq(&self, other: &Self) -> bool {
        self.split == other.split
            && self.ratio == other.ratio
            && self.default_ratio == other.default_ratio
            && self.grid.panes == other.grid.panes
    }
}

pub fn clamp_ratio(
    ratio: f32,
    width: f32,
    first_min: f32,
    first_max: f32,
    second_min: f32,
    second_max: f32,
) -> f32 {
    if !width.is_finite() || width <= 0.0 {
        return ratio.clamp(0.0, 1.0);
    }

    let lower = (first_min / width).max(if second_max.is_finite() {
        1.0 - second_max / width
    } else {
        0.0
    });
    let upper = (first_max / width).min(1.0 - second_min / width);
    if lower <= upper {
        ratio.clamp(lower, upper)
    } else {
        (first_min / (first_min + second_min)).clamp(0.0, 1.0)
    }
}

pub fn style(tokens: &Tokens) -> pane_grid::Style {
    pane_grid::Style {
        hovered_region: pane_grid::Highlight {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
        },
        hovered_split: pane_grid::Line {
            color: tokens.colors.border_strong.into(),
            width: 1.0,
        },
        picked_split: pane_grid::Line {
            color: tokens.colors.link.into(),
            width: DIVIDER_WIDTH,
        },
    }
}

pub fn reset_overlay<'a, Message: Clone + 'a>(
    first_width: f32,
    message: Message,
) -> Element<'a, Message> {
    let leading = (first_width - DIVIDER_WIDTH / 2.0).max(0.0);
    row![
        Space::new().width(Length::Fixed(leading)),
        mouse_area(
            Space::new()
                .width(Length::Fixed(DIVIDER_WIDTH))
                .height(Length::Fill)
        )
        .on_double_click(message)
        .interaction(mouse::Interaction::ResizingHorizontally),
        Space::new().width(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::clamp_ratio;

    #[test]
    fn clamps_both_pane_minimums_and_maximums() {
        assert_eq!(
            clamp_ratio(0.1, 1000.0, 240.0, 450.0, 420.0, f32::INFINITY),
            0.24
        );
        assert_eq!(
            clamp_ratio(0.8, 1000.0, 240.0, 450.0, 420.0, f32::INFINITY),
            0.45
        );
        assert!(
            (clamp_ratio(0.5, 1000.0, 240.0, f32::INFINITY, 200.0, 420.0) - 0.58).abs()
                < f32::EPSILON
        );
    }
}
