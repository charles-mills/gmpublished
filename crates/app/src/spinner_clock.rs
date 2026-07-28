//! The stopwatch behind every indeterminate spinner in the UI.

use std::time::Instant;

/// Elapsed time for a spinner, advanced only by the animation loop.
///
/// `Running` carries the frame it was last advanced to rather than reading the
/// clock on demand, so every widget drawn from one frame reports the same
/// elapsed time. `Idle` has no timestamps at all, so a stopped spinner cannot
/// report a stale duration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpinnerClock {
    #[default]
    Idle,
    Running {
        started_at: Instant,
        now: Instant,
    },
}

impl SpinnerClock {
    /// Restarts from zero at `now`.
    pub const fn start(&mut self, now: Instant) {
        *self = Self::Running {
            started_at: now,
            now,
        };
    }

    /// Moves a running clock to `now`, reporting whether one was running.
    pub fn advance(&mut self, now: Instant) -> bool {
        let Self::Running {
            now: current_frame, ..
        } = self
        else {
            return false;
        };
        *current_frame = now;
        true
    }

    /// Moves the clock to `now`, starting it if it was idle.
    pub fn advance_or_start(&mut self, now: Instant) {
        if !self.advance(now) {
            self.start(now);
        }
    }

    pub const fn stop(&mut self) {
        *self = Self::Idle;
    }

    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn elapsed(self) -> f32 {
        match self {
            Self::Idle => 0.0,
            Self::Running { started_at, now } => {
                now.saturating_duration_since(started_at).as_secs_f32()
            }
        }
    }
}
