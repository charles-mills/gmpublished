use super::{SheetFrame, SheetSequence, SpriteSheet};
use crate::Limits;
use std::{fmt, mem::size_of};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct SpriteSheetCost {
    pub sequences: usize,
    pub frames: usize,
    pub largest_sequence: usize,
    /// Sequence/frame record storage, excluding allocator and Arc overhead.
    pub metadata_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct SpriteSheetLimits {
    pub max_sequences: usize,
    pub max_frames_per_sequence: usize,
    pub max_frames: usize,
    pub max_metadata_bytes: usize,
}

impl SpriteSheetLimits {
    #[must_use]
    #[allow(missing_docs)]
    pub fn from_limits(limits: &Limits) -> Self {
        Self {
            max_sequences: limits.max_entries.min(1024),
            max_frames_per_sequence: limits.max_entries.min(4096),
            max_frames: limits.max_entries,
            max_metadata_bytes: usize::try_from(limits.max_entry_bytes).unwrap_or(usize::MAX),
        }
    }

    #[allow(missing_docs)]
    pub fn check(self, cost: SpriteSheetCost) -> Result<(), SpriteSheetError> {
        for (metric, required, capacity) in [
            (
                SpriteSheetMetric::Sequences,
                cost.sequences,
                self.max_sequences,
            ),
            (
                SpriteSheetMetric::SequenceFrames,
                cost.largest_sequence,
                self.max_frames_per_sequence,
            ),
            (SpriteSheetMetric::Frames, cost.frames, self.max_frames),
            (
                SpriteSheetMetric::MetadataBytes,
                cost.metadata_bytes,
                self.max_metadata_bytes,
            ),
        ] {
            if required > capacity {
                return Err(SpriteSheetError::Limit {
                    metric,
                    required,
                    capacity,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum SpriteSheetMetric {
    Sequences,
    SequenceFrames,
    Frames,
    MetadataBytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum SpriteSheetError {
    InvalidResource,
    UnsupportedVersion(u32),
    Limit {
        metric: SpriteSheetMetric,
        required: usize,
        capacity: usize,
    },
}

impl fmt::Display for SpriteSheetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResource => f.write_str("invalid VTF sprite-sheet resource"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported sprite-sheet version {version}")
            }
            Self::Limit {
                metric,
                required,
                capacity,
            } => write!(
                f,
                "sprite-sheet {metric:?} requires {required}, limit {capacity}"
            ),
        }
    }
}

impl std::error::Error for SpriteSheetError {}

#[derive(Clone, Copy)]
#[allow(missing_docs)]
pub struct SpriteSheetDescriptor<'a> {
    payload: &'a [u8],
    version: u32,
    cost: SpriteSheetCost,
}

impl fmt::Debug for SpriteSheetDescriptor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpriteSheetDescriptor")
            .field("version", &self.version)
            .field("cost", &self.cost)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Cursor<'_> {
    fn u32(&mut self) -> Result<u32, SpriteSheetError> {
        let end = self
            .position
            .checked_add(4)
            .ok_or(SpriteSheetError::InvalidResource)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(SpriteSheetError::InvalidResource)?;
        self.position = end;
        Ok(u32::from_le_bytes(
            bytes.try_into().expect("four-byte slice"),
        ))
    }

    fn skip(&mut self, length: usize) -> Result<(), SpriteSheetError> {
        self.position = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(SpriteSheetError::InvalidResource)?;
        Ok(())
    }
}

pub(super) fn descriptor(payload: &[u8]) -> Result<SpriteSheetDescriptor<'_>, SpriteSheetError> {
    let mut cursor = Cursor {
        bytes: payload,
        position: 0,
    };
    let version = cursor.u32()?;
    if version > 1 {
        return Err(SpriteSheetError::UnsupportedVersion(version));
    }
    let sequences = cursor.u32()? as usize;
    if sequences > 1024 {
        return Err(SpriteSheetError::Limit {
            metric: SpriteSheetMetric::Sequences,
            required: sequences,
            capacity: 1024,
        });
    }
    let frame_bytes = if version == 0 { 20 } else { 68 };
    let mut frames = 0_usize;
    let mut largest_sequence = 0;
    for _ in 0..sequences {
        cursor.skip(8)?;
        let count = cursor.u32()? as usize;
        if count > 4096 {
            return Err(SpriteSheetError::Limit {
                metric: SpriteSheetMetric::SequenceFrames,
                required: count,
                capacity: 4096,
            });
        }
        cursor.skip(4)?;
        cursor.skip(
            count
                .checked_mul(frame_bytes)
                .ok_or(SpriteSheetError::InvalidResource)?,
        )?;
        frames = frames
            .checked_add(count)
            .ok_or(SpriteSheetError::InvalidResource)?;
        largest_sequence = largest_sequence.max(count);
    }
    let metadata_bytes = sequences
        .checked_mul(size_of::<SheetSequence>())
        .and_then(|bytes| {
            frames
                .checked_mul(size_of::<SheetFrame>())
                .and_then(|frame_bytes| bytes.checked_add(frame_bytes))
        })
        .and_then(|bytes| bytes.checked_add(size_of::<SpriteSheet>()))
        .ok_or(SpriteSheetError::InvalidResource)?;
    Ok(SpriteSheetDescriptor {
        payload,
        version,
        cost: SpriteSheetCost {
            sequences,
            frames,
            largest_sequence,
            metadata_bytes,
        },
    })
}

impl SpriteSheetDescriptor<'_> {
    #[must_use]
    #[allow(missing_docs)]
    pub fn cost(&self) -> SpriteSheetCost {
        self.cost
    }

    #[allow(missing_docs)]
    pub fn decode(&self, limits: SpriteSheetLimits) -> Result<SpriteSheet, SpriteSheetError> {
        limits.check(self.cost)?;
        let mut cursor = Cursor {
            bytes: self.payload,
            position: 8,
        };
        let mut sequences = Vec::with_capacity(self.cost.sequences);
        for _ in 0..self.cost.sequences {
            let number = cursor.u32()?;
            let clamp = cursor.u32()? != 0;
            let count = cursor.u32()? as usize;
            let total_time = f32::from_bits(cursor.u32()?);
            let mut frames = Vec::with_capacity(count);
            for _ in 0..count {
                let display_time = f32::from_bits(cursor.u32()?);
                let mut uv = [0.0; 4];
                for value in &mut uv {
                    *value = f32::from_bits(cursor.u32()?);
                }
                if self.version == 1 {
                    cursor.skip(48)?;
                }
                frames.push(SheetFrame { display_time, uv });
            }
            sequences.push(SheetSequence {
                number,
                clamp,
                total_time,
                frames,
            });
        }
        Ok(SpriteSheet { sequences })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(counts: &[u32], version: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [version, u32::try_from(counts.len()).unwrap()] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for (number, &count) in counts.iter().enumerate() {
            for value in [u32::try_from(number).unwrap(), 0, count, 1_f32.to_bits()] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for _ in 0..count {
                for value in [1_f32, 0.25, 0.5, 0.75, 1.0] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                if version == 1 {
                    bytes.extend_from_slice(&[0; 48]);
                }
            }
        }
        bytes
    }

    #[test]
    fn aggregate_limits_precede_record_allocation_and_preserve_both_versions() {
        for version in [0, 1] {
            let bytes = payload(&[2, 2, 2], version);
            let descriptor = descriptor(&bytes).unwrap();
            let cost = descriptor.cost();
            assert_eq!(cost.frames, 6);
            assert_eq!(
                cost.metadata_bytes,
                size_of::<SpriteSheet>()
                    + 3 * size_of::<SheetSequence>()
                    + 6 * size_of::<SheetFrame>()
            );
            let mut limits = SpriteSheetLimits::from_limits(&Limits::default());
            limits.max_frames = 5;
            assert!(matches!(
                descriptor.decode(limits),
                Err(SpriteSheetError::Limit {
                    metric: SpriteSheetMetric::Frames,
                    required: 6,
                    capacity: 5
                })
            ));
            limits.max_frames = 6;
            limits.max_metadata_bytes = cost.metadata_bytes - 1;
            assert!(matches!(
                descriptor.decode(limits),
                Err(SpriteSheetError::Limit {
                    metric: SpriteSheetMetric::MetadataBytes,
                    ..
                })
            ));
            limits.max_metadata_bytes = cost.metadata_bytes;
            let sheet = descriptor.decode(limits).unwrap();
            assert_eq!(sheet.sequences.len(), 3);
            assert_eq!(sheet.sequences[2].frames[1].uv, [0.25, 0.5, 0.75, 1.0]);
        }
    }

    #[test]
    fn complete_resource_walk_rejects_truncation_and_hostile_counts() {
        let mut bytes = payload(&[1, 2], 1);
        bytes.pop();
        assert_eq!(
            descriptor(&bytes).unwrap_err(),
            SpriteSheetError::InvalidResource
        );
        let mut bytes = payload(&[1], 0);
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            descriptor(&bytes),
            Err(SpriteSheetError::Limit {
                metric: SpriteSheetMetric::SequenceFrames,
                ..
            })
        ));
        assert!(matches!(
            descriptor(&payload(&[], 2)),
            Err(SpriteSheetError::UnsupportedVersion(2))
        ));
    }
}
