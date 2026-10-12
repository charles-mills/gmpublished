use cosmic_text::Align;

use super::text::{LINE_HEIGHT_RATIO, shape, shared_font_system};

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
