use iced::animation::Easing;

use crate::theme::{self, motion};

const PALETTE_CLOSED_SCALE: f32 = 0.985;

#[derive(Debug, PartialEq)]
pub(super) struct PaletteMotion {
    pub(super) expanded: bool,
    pub(super) visible: bool,
    pub(super) presence: motion::Presence<bool>,
    pub(super) reduce_motion: bool,
}

impl Default for PaletteMotion {
    fn default() -> Self {
        let tokens = theme::invariant();
        Self {
            expanded: false,
            visible: false,
            reduce_motion: false,
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn palette_uses_small_scale_and_returns_to_idle() {
        let mut palette = PaletteMotion::default();
        let started = Instant::now();
        palette.presence.go(true, started);
        assert_eq!(palette.scale(started), 0.985);
        assert!(palette.scale(started + Duration::from_millis(60)) > 0.985);
        palette.presence.tick(started + Duration::from_secs(1));
        assert_eq!(palette.scale(started + Duration::from_secs(1)), 1.0);
        assert!(!palette.presence.needs_ticks());
    }
}
