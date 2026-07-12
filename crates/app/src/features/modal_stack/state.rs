use std::time::Instant;

use crate::theme::{Tokens, motion};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveModal {
    /// Extraction destination picker (always the overlay layer).
    DestinationSelect,
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
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            phase: Phase::Closed,
            presence: motion::asymmetric(
                false,
                Tokens::dark().motion.modal_enter_duration(),
                Tokens::dark().motion.modal_exit_duration(),
                motion::expo_ease(),
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
        self.opacity(now)
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

    fn open(&mut self, modal: ActiveModal, now: Instant) {
        self.phase = Phase::Visible(modal);
        self.presence.go(true, now);
    }

    fn close(&mut self, now: Instant) {
        if let Phase::Visible(modal) | Phase::Closing(modal) = self.phase {
            self.phase = Phase::Closing(modal);
            self.presence.go(false, now);
        }
    }

    fn tick(&mut self, now: Instant) -> Option<ActiveModal> {
        let settled = self.presence.tick(now);
        if let Phase::Closing(modal) = self.phase
            && settled
        {
            self.phase = Phase::Closed;
            return Some(modal);
        }

        None
    }
}

/// State owned by the modal-stack host: a base layer for the primary modals
/// plus an overlay layer for secondary modals that stack over an open base
/// modal instead of replacing it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    base: Layer,
    overlay: Layer,
}

impl State {
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

    /// Advances close animations, returning every modal that finished
    /// closing this tick so the root can run its per-modal close task.
    pub(crate) fn tick(&mut self, now: Instant) -> Vec<ActiveModal> {
        let mut finished = Vec::new();
        finished.extend(self.overlay.tick(now));
        finished.extend(self.base.tick(now));
        finished
    }

    pub(super) fn open(&mut self, modal: ActiveModal, now: Instant) {
        if modal.is_overlay() {
            self.overlay.open(modal, now);
        } else {
            self.base.open(modal, now);
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
        matches!(self, Self::DestinationSelect)
    }
}
