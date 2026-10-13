use std::time::Instant;

use iced::animation::Easing;

use crate::theme::{self, motion};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveModal {
    DestinationSelect,
    DescriptionEditor,
    PreparePublish,
    PreviewGma,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Closed,
    Visible(ActiveModal),
    Closing(ActiveModal),
}

#[derive(Clone, Debug, PartialEq)]
struct Layer {
    phase: Phase,
    presence: motion::Presence<bool>,
    reduce_motion: bool,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            phase: Phase::Closed,
            reduce_motion: false,
            presence: motion::asymmetric(
                false,
                theme::invariant().motion.modal_enter_duration(),
                theme::invariant().motion.modal_exit_duration(),
                Easing::EaseOut,
            ),
        }
    }
}

impl Layer {
    const fn active(&self) -> Option<ActiveModal> {
        match self.phase {
            Phase::Closed => None,
            Phase::Visible(modal) | Phase::Closing(modal) => Some(modal),
        }
    }

    fn opacity(&self, now: Instant) -> f32 {
        self.presence.interpolate(0.0, 1.0, now)
    }

    fn scale(&self, now: Instant) -> f32 {
        if self.reduce_motion && !self.interactive() {
            0.0
        } else {
            self.presence.interpolate(0.985, 1.0, now)
        }
    }

    const fn interactive(&self) -> bool {
        matches!(self.phase, Phase::Visible(_))
    }

    #[cfg(test)]
    fn is_animating(&self, now: Instant) -> bool {
        self.presence.is_animating(now)
    }

    const fn needs_ticks(&self) -> bool {
        self.presence.needs_ticks()
    }

    /// Returns the modal this one replaced, if it displaced a different one.
    /// Re-opening the modal already on the layer reverses its close animation
    /// instead, so nothing is displaced.
    fn open(&mut self, modal: ActiveModal, now: Instant) -> Option<ActiveModal> {
        let displaced = self.active().filter(|active| *active != modal);
        self.phase = Phase::Visible(modal);
        if self.reduce_motion {
            self.presence.snap(true);
        } else {
            self.presence.go(true, now);
        }
        displaced
    }

    fn close(&mut self, now: Instant) {
        if let Phase::Visible(modal) | Phase::Closing(modal) = self.phase {
            self.phase = Phase::Closing(modal);
            self.presence.go(false, now);
        }
    }

    fn tick(&mut self, now: Instant) -> Option<ActiveModal> {
        let settled = if self.reduce_motion {
            self.presence.snap(self.interactive());
            true
        } else {
            self.presence.tick(now)
        };
        if let Phase::Closing(modal) = self.phase
            && settled
        {
            self.phase = Phase::Closed;
            return Some(modal);
        }

        None
    }
}

#[derive(Debug, Default)]
pub struct State {
    base: Layer,
    overlay: Layer,
}

impl State {
    pub(crate) fn set_reduce_motion(&mut self, reduce_motion: bool) -> Vec<ActiveModal> {
        if self.base.reduce_motion == reduce_motion {
            return Vec::new();
        }
        self.base.reduce_motion = reduce_motion;
        self.overlay.reduce_motion = reduce_motion;
        let mut policy = theme::invariant().motion;
        policy.reduce_motion = reduce_motion;
        self.base
            .presence
            .set_durations(policy.modal_enter_duration(), policy.modal_exit_duration());
        self.overlay
            .presence
            .set_durations(policy.modal_enter_duration(), policy.modal_exit_duration());
        if reduce_motion {
            self.tick(Instant::now())
        } else {
            Vec::new()
        }
    }
    pub(crate) const fn active(&self) -> Option<ActiveModal> {
        self.base.active()
    }

    pub(crate) const fn overlay_modal(&self) -> Option<ActiveModal> {
        self.overlay.active()
    }

    pub(crate) const fn overlay_active(&self) -> bool {
        self.overlay.active().is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_active_modal(&self) -> bool {
        self.base.active().is_some() || self.overlay.active().is_some()
    }

    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        self.base.opacity(now)
    }

    pub(crate) fn overlay_opacity(&self, now: Instant) -> f32 {
        self.overlay.opacity(now)
    }

    pub(crate) fn scale(&self, now: Instant) -> f32 {
        self.base.scale(now)
    }

    pub(crate) fn overlay_scale(&self, now: Instant) -> f32 {
        self.overlay.scale(now)
    }

    pub(crate) const fn interactive(&self) -> bool {
        self.base.interactive()
    }

    pub(crate) const fn overlay_interactive(&self) -> bool {
        self.overlay.interactive()
    }

    #[cfg(test)]
    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.base.is_animating(now) || self.overlay.is_animating(now)
    }

    pub(crate) const fn needs_ticks(&self) -> bool {
        self.base.needs_ticks() || self.overlay.needs_ticks()
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Vec<ActiveModal> {
        let mut finished = Vec::new();
        finished.extend(self.overlay.tick(now));
        finished.extend(self.base.tick(now));
        finished
    }

    pub(super) fn open(&mut self, modal: ActiveModal, now: Instant) -> Option<ActiveModal> {
        if modal.is_overlay() {
            self.overlay.open(modal, now)
        } else {
            self.base.open(modal, now)
        }
    }

    /// Closes the topmost layer only: the overlay when visible, else the
    /// base. An overlay already fading out no longer swallows the request,
    /// so a second Escape during its close animation reaches the base modal.
    pub(super) fn close(&mut self, now: Instant) {
        if matches!(self.overlay.phase, Phase::Visible(_)) {
            self.overlay.close(now);
        } else {
            self.base.close(now);
        }
    }
}

impl ActiveModal {
    const fn is_overlay(self) -> bool {
        matches!(self, Self::DestinationSelect | Self::DescriptionEditor)
    }
}

#[cfg(test)]
mod motion_tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn modal_motion_preserves_size_and_finalizes_close_once() {
        let mut state = State::default();
        let started = Instant::now();
        state.open(ActiveModal::Settings, started);
        assert_eq!(state.scale(started), 0.985);
        let midpoint = state.scale(started + Duration::from_millis(60));
        assert!(midpoint > 0.985 && midpoint < 1.0);
        let settled = started + Duration::from_secs(1);
        state.tick(settled);
        assert_eq!(state.scale(settled), 1.0);
        assert!(!state.needs_ticks());
        state.close(settled);
        assert_eq!(
            state.tick(settled + Duration::from_secs(1)),
            vec![ActiveModal::Settings]
        );
        assert_eq!(state.active(), None);
        assert!(state.tick(settled + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn reducing_motion_settles_open_and_reports_pending_close() {
        let mut state = State::default();
        let started = Instant::now();
        state.open(ActiveModal::Settings, started);
        assert!(state.set_reduce_motion(true).is_empty());
        assert_eq!(state.scale(started), 1.0);
        assert!(!state.needs_ticks());
        state.close(started);
        assert_eq!(state.tick(started), vec![ActiveModal::Settings]);
        state.open(ActiveModal::PreviewGma, started);
        assert!(!state.needs_ticks());
        assert_eq!(state.scale(started), 1.0);
    }
}
