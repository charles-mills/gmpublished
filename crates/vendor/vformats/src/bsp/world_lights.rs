//! Compiled `dworldlight_t` records. Runtime light selection is engine policy.
use super::record::{f32_at, i32_at, u32_at, vec3_at};
use super::{Bsp, BspError, Limits, lump_ids};
use std::fmt;

/// Source's compiled emitter type, including unknown branch-specific values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldLightKind {
    /// Emissive surface with a hemispherical angular response.
    Surface,
    /// Point emitter.
    Point,
    /// Spotlight with inner/outer cone thresholds.
    Spotlight,
    /// Directional sky light requiring sky visibility.
    Skylight,
    /// Legacy linear-falloff emitter.
    Quake,
    /// Sky ambient already represented in ambient lighting.
    SkyAmbient,
    /// Unrecognized raw emitter identifier, retained without reinterpretation.
    Unknown(i32),
}

impl WorldLightKind {
    /// The exact serialized emitter identifier.
    #[must_use]
    pub const fn raw(self) -> i32 {
        match self {
            Self::Surface => 0,
            Self::Point => 1,
            Self::Spotlight => 2,
            Self::Skylight => 3,
            Self::Quake => 4,
            Self::SkyAmbient => 5,
            Self::Unknown(value) => value,
        }
    }
}

impl From<i32> for WorldLightKind {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Surface,
            1 => Self::Point,
            2 => Self::Spotlight,
            3 => Self::Skylight,
            4 => Self::Quake,
            5 => Self::SkyAmbient,
            value => Self::Unknown(value),
        }
    }
}

/// A compiled light, retaining original coordinates and all serialized fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldLight {
    /// Original row index within this LDR or HDR lump.
    pub original_index: usize,
    /// Source-space emitter position.
    pub origin: [f32; 3],
    /// Compiled linear RGB intensity, independent of entity keyvalues.
    pub intensity: [f32; 3],
    /// Emission direction for surfaces/spotlights.
    pub normal: [f32; 3],
    /// Visibility cluster, including native negative sentinels.
    pub cluster: i32,
    /// Native emitter type.
    pub kind: WorldLightKind,
    /// Native lightstyle index.
    pub style: i32,
    /// Inner-cone cosine threshold.
    pub stopdot: f32,
    /// Outer-cone cosine threshold.
    pub stopdot2: f32,
    /// Angular falloff exponent.
    pub exponent: f32,
    /// Cutoff radius, preserving zero/infinite semantics for consumers.
    pub radius: f32,
    /// Constant term of native distance attenuation.
    pub constant_attenuation: f32,
    /// Linear term of native distance attenuation.
    pub linear_attenuation: f32,
    /// Quadratic term of native distance attenuation.
    pub quadratic_attenuation: f32,
    /// Raw native bit flags; bit 0 marks inclusion in the ambient cube.
    pub flags: u32,
    /// Emissive surface texture-info index or native sentinel.
    pub texinfo: i32,
    /// Relative owner entity index or native sentinel.
    pub owner: i32,
}

/// A malformed optional compiled world-light channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorldLightError {
    /// Container/decompression failure.
    Bsp(BspError),
    /// Unsupported native record layout version.
    UnsupportedVersion(i32),
    /// A trailing partial 88-byte record.
    PartialRecord(usize),
    /// Record count exceeded the configured limit.
    TooManyRecords {
        /// Serialized row count.
        count: usize,
        /// Configured maximum row count.
        max: usize,
    },
    /// A serialized float is NaN or infinite; row and field are retained.
    NonFinite {
        /// Original row index, before any filtering.
        index: usize,
        /// Native field containing the invalid value.
        field: &'static str,
    },
}

impl fmt::Display for WorldLightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bsp(error) => error.fmt(f),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported world-light version {version}")
            }
            Self::PartialRecord(bytes) => write!(
                f,
                "world-light payload {bytes} bytes is not divisible by 88"
            ),
            Self::TooManyRecords { count, max } => {
                write!(f, "world-light count {count} exceeds {max}")
            }
            Self::NonFinite { index, field } => {
                write!(f, "world-light {index} has nonfinite {field}")
            }
        }
    }
}

impl std::error::Error for WorldLightError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bsp(error) => Some(error),
            _ => None,
        }
    }
}

impl Bsp<'_> {
    /// Bounded LDR compiled light records, preserving original order.
    pub fn world_lights(&self, limits: &Limits) -> Result<Vec<WorldLight>, WorldLightError> {
        self.decode_world_lights(lump_ids::WORLDLIGHTS, limits)
    }

    /// Bounded HDR compiled light records, independent of the LDR channel.
    pub fn world_lights_hdr(&self, limits: &Limits) -> Result<Vec<WorldLight>, WorldLightError> {
        self.decode_world_lights(lump_ids::WORLDLIGHTS_HDR, limits)
    }

    fn decode_world_lights(
        &self,
        lump: usize,
        limits: &Limits,
    ) -> Result<Vec<WorldLight>, WorldLightError> {
        let bytes = self.lump_data(lump, limits).map_err(WorldLightError::Bsp)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let version = self.lump_version(lump).unwrap_or_default();
        if version != 0 {
            return Err(WorldLightError::UnsupportedVersion(version));
        }
        if bytes.len() as u64 > limits.max_entry_bytes {
            return Err(WorldLightError::Bsp(BspError::LumpTooLarge {
                part: "world lights",
                size: bytes.len() as u64,
                max: limits.max_entry_bytes,
            }));
        }
        if bytes.len() % 88 != 0 {
            return Err(WorldLightError::PartialRecord(bytes.len()));
        }
        let count = bytes.len() / 88;
        if count > limits.max_entries {
            return Err(WorldLightError::TooManyRecords {
                count,
                max: limits.max_entries,
            });
        }
        bytes
            .chunks_exact(88)
            .enumerate()
            .map(|(index, record)| {
                for (field, offset, count) in [
                    ("origin", 0, 3),
                    ("intensity", 12, 3),
                    ("normal", 24, 3),
                    ("stopdot", 48, 1),
                    ("stopdot2", 52, 1),
                    ("exponent", 56, 1),
                    ("radius", 60, 1),
                    ("constant attenuation", 64, 1),
                    ("linear attenuation", 68, 1),
                    ("quadratic attenuation", 72, 1),
                ] {
                    if (0..count).any(|channel| !f32_at(record, offset + channel * 4).is_finite()) {
                        return Err(WorldLightError::NonFinite { index, field });
                    }
                }
                Ok(WorldLight {
                    original_index: index,
                    origin: vec3_at(record, 0),
                    intensity: vec3_at(record, 12),
                    normal: vec3_at(record, 24),
                    cluster: i32_at(record, 36),
                    kind: i32_at(record, 40).into(),
                    style: i32_at(record, 44),
                    stopdot: f32_at(record, 48),
                    stopdot2: f32_at(record, 52),
                    exponent: f32_at(record, 56),
                    radius: f32_at(record, 60),
                    constant_attenuation: f32_at(record, 64),
                    linear_attenuation: f32_at(record, 68),
                    quadratic_attenuation: f32_at(record, 72),
                    flags: u32_at(record, 76),
                    texinfo: i32_at(record, 80),
                    owner: i32_at(record, 84),
                })
            })
            .collect()
    }
}
