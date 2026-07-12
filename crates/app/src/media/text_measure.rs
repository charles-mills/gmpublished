//! Shared UI text measurement over the bundled Inter Regular face.
//!
//! Iced has no post-layout callback, so widgets that must size themselves
//! before positioning (context menus, the size-analyzer tooltip) shape text
//! off-thread with cosmic-text to estimate widths and wrapped line counts.

use cosmic_text::Align;

use super::text::{LINE_HEIGHT_RATIO, shape, shared_font_system};

/// Returns the shaped width of a single unwrapped line of `text`.
pub fn measure_width(text: &str, font_size: f32) -> f32 {
    let line_height = font_size * LINE_HEIGHT_RATIO;
    let buffer = {
        let mut font_system = shared_font_system().lock();
        shape(
            &mut font_system,
            text,
            font_size,
            (None, Some(line_height)),
            Align::Left,
        )
    };

    buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0, f32::max)
}

/// Returns the number of laid-out lines when `text` is wrapped at
/// `max_width`. Always at least 1.
pub fn wrapped_line_count(text: &str, font_size: f32, max_width: f32) -> usize {
    let buffer = {
        let mut font_system = shared_font_system().lock();
        shape(
            &mut font_system,
            text,
            font_size,
            (Some(max_width.max(1.0)), None),
            Align::Left,
        )
    };

    buffer.layout_runs().count().max(1)
}
