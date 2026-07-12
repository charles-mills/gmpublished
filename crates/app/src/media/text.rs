//! Shared cosmic-text font system for UI text measurement and worker-side
//! label rasterization, both shaped over the same bundled Inter Regular face.

use std::sync::{Arc, OnceLock};

use cosmic_text::{Align, Attrs, Buffer, FontSystem, Metrics, PlatformFallback, Shaping, fontdb};
use parking_lot::Mutex;

use crate::assets;

/// Line-height multiple used while shaping (only affects the scroll clamp /
/// vertical centering, never glyph size).
pub const LINE_HEIGHT_RATIO: f32 = 1.18;

fn new_font_system() -> FontSystem {
    let mut db = fontdb::Database::new();
    db.load_font_source(fontdb::Source::Binary(Arc::new(
        assets::fonts::inter_regular_bytes(),
    )));
    db.set_sans_serif_family("Inter");

    FontSystem::new_with_locale_and_db_and_fallback("en-US".to_owned(), db, PlatformFallback)
}

/// The process-wide Inter font system shared by UI text measurement and
/// worker-side label rasterization.
pub fn shared_font_system() -> &'static Mutex<FontSystem> {
    static FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    FONT_SYSTEM.get_or_init(|| Mutex::new(new_font_system()))
}

/// Shapes `text` at `font_size` into a buffer sized `(width, height)`,
/// running the shaper to completion. `align` controls line alignment within
/// the buffer.
pub fn shape(
    font_system: &mut FontSystem,
    text: &str,
    font_size: f32,
    size: (Option<f32>, Option<f32>),
    align: Align,
) -> Buffer {
    let metrics = Metrics::new(font_size, font_size * LINE_HEIGHT_RATIO);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, size.0, size.1);
    buffer.set_text(
        font_system,
        text,
        &Attrs::new(),
        Shaping::Advanced,
        Some(align),
    );
    buffer.shape_until_scroll(font_system, false);
    buffer
}
