//! View bob: the walk cycle's vertical sway and the landing dip.

use super::super::super::state::MovementMode;
use super::{FlyCamera, LAND_BOB_DURATION};

const WALK_BOB_AMPLITUDE: f32 = 1.1;

const WALK_BOB_FREQUENCY_HZ: f32 = 1.8;

const WALK_BOB_RETURN_SPEED: f32 = 10.0;

impl FlyCamera {
    pub(super) fn land_bob_active(&self) -> bool {
        self.land_bob_amplitude > 0.0 && self.land_bob_elapsed < LAND_BOB_DURATION
    }

    pub(in super::super) fn view_bob_offset(&self) -> f32 {
        if self.mode != MovementMode::Walk {
            return 0.0;
        }
        let landing = if self.land_bob_active() {
            let t = (self.land_bob_elapsed / LAND_BOB_DURATION).clamp(0.0, 1.0);
            -self.land_bob_amplitude * (std::f32::consts::PI * t).sin()
        } else {
            0.0
        };
        self.walk_bob_offset + landing
    }

    pub(super) fn update_walk_bob(&mut self, dt: f32, moving: bool) {
        if moving {
            self.walk_bob_phase = (self.walk_bob_phase
                + dt * WALK_BOB_FREQUENCY_HZ * std::f32::consts::TAU)
                % std::f32::consts::TAU;
            self.walk_bob_offset = self.walk_bob_phase.sin() * WALK_BOB_AMPLITUDE;
        } else {
            let decay = (WALK_BOB_RETURN_SPEED * dt).clamp(0.0, 1.0);
            self.walk_bob_offset += (0.0 - self.walk_bob_offset) * decay;
            if self.walk_bob_offset.abs() <= 0.01 {
                self.walk_bob_offset = 0.0;
            }
        }

        if self.land_bob_active() {
            self.land_bob_elapsed = (self.land_bob_elapsed + dt).min(LAND_BOB_DURATION);
        }
    }
}
