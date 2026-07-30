//! Orbit-camera input shared by the model and particle viewers.

use super::state::OrbitPose;

/// Radians of rotation per pixel of drag.
const SENSITIVITY: f32 = 0.008;
/// Distance multiplier per scroll step; below 1 so scrolling up zooms in.
const ZOOM_STEP: f32 = 0.9;
/// Just short of straight up/down: the view basis goes degenerate against the
/// world up axis at exactly vertical.
pub(super) const MIN_PITCH: f32 = -1.55;
pub(super) const MAX_PITCH: f32 = 1.55;

/// How far out the camera may orbit, as a multiple of the framing distance.
const MAX_DISTANCE: f32 = 8.0;

/// How close the camera may orbit, as a multiple of the framing distance.
///
/// Per-subject: a solid mesh has a surface to clip through, a particle cloud
/// does not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ZoomFloor {
    /// Far enough out that a solid model's surface stays ahead of the eye.
    SolidMesh,
    /// Inside a sparse cloud, where there is nothing to clip through.
    ParticleCloud,
}

impl ZoomFloor {
    const fn min_distance(self) -> f32 {
        match self {
            Self::SolidMesh => 0.2,
            Self::ParticleCloud => 0.05,
        }
    }
}

/// Yaw, pitch and distance driven by drag and scroll.
///
/// Owns the clamps so a viewer cannot apply its own: pitch past vertical
/// flips the view, and a distance of zero puts the eye inside the subject.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Orbit {
    yaw: f32,
    pitch: f32,
    distance: f32,
    floor: ZoomFloor,
}

impl Orbit {
    pub(super) fn new(floor: ZoomFloor) -> Self {
        Self::from_pose(OrbitPose::default(), floor)
    }

    pub(super) fn from_pose(pose: OrbitPose, floor: ZoomFloor) -> Self {
        Self {
            yaw: pose.yaw,
            pitch: pose.pitch.clamp(MIN_PITCH, MAX_PITCH),
            distance: pose.distance.clamp(floor.min_distance(), MAX_DISTANCE),
            floor,
        }
    }

    pub(super) const fn pose(self) -> OrbitPose {
        OrbitPose {
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
        }
    }

    pub(super) const fn yaw(self) -> f32 {
        self.yaw
    }

    pub(super) const fn pitch(self) -> f32 {
        self.pitch
    }

    pub(super) const fn distance(self) -> f32 {
        self.distance
    }

    pub(super) fn set_distance(&mut self, distance: f32) {
        self.distance = distance.clamp(self.floor.min_distance(), MAX_DISTANCE);
    }

    /// Applies a drag of `(dx, dy)` pixels.
    pub(super) fn drag(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * SENSITIVITY;
        self.pitch = (self.pitch + dy * SENSITIVITY).clamp(MIN_PITCH, MAX_PITCH);
    }

    /// Applies `steps` of scroll wheel; positive zooms in.
    pub(super) fn zoom(&mut self, steps: f32) {
        self.set_distance(self.distance * ZOOM_STEP.powf(steps));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_stops_short_of_vertical_in_both_directions() {
        let mut orbit = Orbit::new(ZoomFloor::SolidMesh);
        orbit.drag(0.0, 10_000.0);
        assert_eq!(orbit.pitch(), MAX_PITCH);
        orbit.drag(0.0, -20_000.0);
        assert_eq!(orbit.pitch(), MIN_PITCH);
    }

    /// Zoom is unbounded scroll input against a bounded distance, so the
    /// clamps have to hold however far the wheel is spun.
    #[test]
    fn zoom_saturates_rather_than_passing_through_the_subject() {
        let mut orbit = Orbit::new(ZoomFloor::SolidMesh);
        orbit.zoom(1000.0);
        assert_eq!(orbit.distance(), ZoomFloor::SolidMesh.min_distance());
        orbit.zoom(-1000.0);
        assert_eq!(orbit.distance(), MAX_DISTANCE);
    }

    /// The two viewers frame identically (`radius * 2.2 * distance`) but hold
    /// different subjects, so they stop at different distances: a mesh has a
    /// surface to clip through, a particle cloud does not. Sharing one floor
    /// would let the model viewer scroll four times closer than it does.
    #[test]
    fn a_solid_mesh_stops_further_out_than_a_particle_cloud() {
        let mut mesh = Orbit::new(ZoomFloor::SolidMesh);
        mesh.zoom(1000.0);
        let mut cloud = Orbit::new(ZoomFloor::ParticleCloud);
        cloud.zoom(1000.0);

        assert_eq!(mesh.distance(), 0.2);
        assert_eq!(cloud.distance(), 0.05);
        assert!(cloud.distance() < mesh.distance());
    }

    /// A pose arriving from persisted state is outside this type's control.
    #[test]
    fn a_restored_pose_is_clamped_on_the_way_in() {
        let orbit = Orbit::from_pose(
            OrbitPose {
                yaw: 0.0,
                pitch: 99.0,
                distance: 0.0,
            },
            ZoomFloor::SolidMesh,
        );

        assert_eq!(orbit.pitch(), MAX_PITCH);
        assert_eq!(orbit.distance(), ZoomFloor::SolidMesh.min_distance());
    }
}
