//! Preview scene assembly over [`vformats`]: wire-format parsing lives
//! there; this module derives renderable scenes from it.

pub mod map;
pub mod model;
pub mod pcf;

/// A Source engine QAngle: the `[pitch, yaw, roll]` degree triple that map
/// entities and static props store their orientation as.
///
/// Constructed from that layout once, so the index-to-axis mapping and the
/// degrees-to-radians conversion are not restated at each use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QAngle {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

impl QAngle {
    /// Reads the `[pitch, yaw, roll]` degree layout, yielding radians.
    #[must_use]
    pub fn from_source_degrees(angles: [f32; 3]) -> Self {
        Self {
            pitch: angles[0].to_radians(),
            yaw: angles[1].to_radians(),
            roll: angles[2].to_radians(),
        }
    }
}
