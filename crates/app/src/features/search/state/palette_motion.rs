//! Search-palette visibility and animated presence.

use iced::animation::Easing;

use crate::theme::{self, motion};

const PALETTE_CLOSED_SCALE: f32 = 0.98;

#[derive(Debug, PartialEq)]
pub(super) struct PaletteMotion {
    pub(super) expanded: bool,
    pub(super) visible: bool,
    pub(super) presence: motion::Presence<bool>,
}

impl Default for PaletteMotion {
    fn default() -> Self {
        let tokens = theme::invariant();
        Self {
            expanded: false,
            visible: false,
            presence: motion::asymmetric(
                false,
                tokens.motion.modal_enter_duration(),
                tokens.motion.modal_exit_duration(),
                Easing::EaseOut,
            ),
        }
    }
}

impl PaletteMotion {
    pub(super) fn opacity(&self, now: std::time::Instant) -> f32 {
        self.presence.interpolate(0.0, 1.0, now)
    }

    pub(super) fn scale(&self, now: std::time::Instant) -> f32 {
        self.presence.interpolate(PALETTE_CLOSED_SCALE, 1.0, now)
    }
}
