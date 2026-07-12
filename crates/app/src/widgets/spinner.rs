//! Five-bar equalizer spinner: bars pulse in a staggered wave animation.

use iced::widget::{Space, container, row};
use iced::{Center, Element, Length};

use crate::theme::{self, Tokens};

const KEYFRAMES: [f32; 11] = [
    120.0, 110.0, 100.0, 90.0, 80.0, 70.0, 60.0, 50.0, 40.0, 140.0, 120.0,
];
const FULL_HEIGHT: f32 = 140.0;
const OFFSETS: [f32; 5] = [0.5, 0.25, 0.0, 0.25, 0.5];

/// Purely a function of the clock passed in: callers gate their tick
/// subscription on the pending work, so nothing animates while idle.
pub fn spinner<'a, M: 'a>(tokens: &Tokens, elapsed: f32, size: f32) -> Element<'a, M> {
    let tokens = *tokens;
    let bar_width = (size * 15.0 / FULL_HEIGHT).max(2.0);
    let mut bars = row![]
        .align_y(Center)
        .spacing(bar_width)
        .height(Length::Fixed(size));
    for offset in OFFSETS {
        let phase = (elapsed + offset).rem_euclid(1.0) * (KEYFRAMES.len() - 1) as f32;
        let index = (phase as usize).min(KEYFRAMES.len() - 2);
        let fraction = phase - index as f32;
        let keyed = KEYFRAMES[index] + (KEYFRAMES[index + 1] - KEYFRAMES[index]) * fraction;
        let height = (keyed / FULL_HEIGHT * size).max(1.0);
        bars = bars.push(
            container(Space::new())
                .width(Length::Fixed(bar_width))
                .height(Length::Fixed(height))
                .style(move |_| theme::styles::spinner_bar(&tokens)),
        );
    }

    bars.into()
}
