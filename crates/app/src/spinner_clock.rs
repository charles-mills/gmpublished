use std::time::Instant;

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
    pub const fn start(&mut self, now: Instant) {
        *self = Self::Running {
            started_at: now,
            now,
        };
    }

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
