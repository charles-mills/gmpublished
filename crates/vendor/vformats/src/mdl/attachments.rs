//! Bounded local attachment records and optional illumination selector.
use super::{MdlError, Reader};
use crate::Limits;

/// Source `ATTACHMENT_FLAG_WORLD_ALIGN`: orientation is world aligned.
pub const ATTACHMENT_FLAG_WORLD_ALIGN: u32 = 0x0001_0000;

/// Authored `mstudioattachment_t`, before bone posing or entity placement.
#[derive(Clone, Debug, PartialEq)]
pub struct MdlAttachment {
    /// Attachment name.
    pub name: String,
    /// Authored flag word, including unknown bits.
    pub flags: u32,
    /// Index into the model's local bone table.
    pub local_bone: usize,
    /// Row-major bone-local 3x4 affine transform.
    pub local: [[f32; 4]; 3],
}

pub(super) fn read(
    bytes: &[u8],
    bone_count: usize,
    limits: &Limits,
) -> Result<(Vec<MdlAttachment>, Option<usize>), MdlError> {
    let mut header = Reader::at(bytes, 240);
    let count = header.count("mdl local attachment count")?;
    let offset = header.count("mdl local attachment offset")?;
    if count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "mdl local attachments",
            max: limits.max_entries,
        });
    }
    let length = count.checked_mul(92).ok_or(MdlError::Corrupt {
        part: "mdl attachment table length",
    })?;
    if count > 0 && offset < 408 {
        return Err(MdlError::Corrupt {
            part: "mdl attachment table overlaps header",
        });
    }
    if count > 0 {
        Reader::at(bytes, offset).take(length)?;
    }
    let mut attachments = Vec::with_capacity(count);
    for index in 0..count {
        let base = offset.checked_add(index * 92).ok_or(MdlError::Corrupt {
            part: "mdl attachment offset",
        })?;
        let mut record = Reader::at(bytes, base);
        let name_offset = record.count("mdl attachment name offset")?;
        let flags = record.u32()?;
        let local_bone = record.count("mdl attachment bone")?;
        if local_bone >= bone_count {
            return Err(MdlError::Corrupt {
                part: "mdl attachment bone",
            });
        }
        let mut local = [[0.0; 4]; 3];
        for row in &mut local {
            for value in row {
                *value = record.f32()?;
            }
        }
        if !local.iter().flatten().all(|value| value.is_finite()) {
            return Err(MdlError::Corrupt {
                part: "mdl attachment matrix",
            });
        }
        record.take(32)?;
        let name_at = base.checked_add(name_offset).ok_or(MdlError::Corrupt {
            part: "mdl attachment name offset",
        })?;
        let remaining = bytes.get(name_at..).ok_or(MdlError::Corrupt {
            part: "mdl attachment name",
        })?;
        let end = remaining
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(MdlError::Corrupt {
                part: "mdl attachment name",
            })?;
        let name = String::from_utf8_lossy(&remaining[..end]).into_owned();
        attachments.push(MdlAttachment {
            name,
            flags,
            local_bone,
            local,
        });
    }
    let secondary = Reader::at(bytes, 400).count("mdl secondary header offset")?;
    let illumination_attachment_index = if secondary == 0 {
        None
    } else {
        if secondary < 408 {
            return Err(MdlError::Corrupt {
                part: "mdl secondary header overlaps primary header",
            });
        }
        let mut header2 = Reader::at(bytes, secondary);
        header2.take(8)?;
        header2
            .count("mdl illumination attachment index")?
            .checked_sub(1)
    };
    Ok((attachments, illumination_attachment_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = vec![0; 768];
        for (at, value) in [
            (240, 1u32),
            (244, 408),
            (400, 512),
            (408, 92),
            (412, 0x1234),
            (416, 0),
            (520, 1),
        ] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [(420, 1.0f32), (440, 1.0), (460, 1.0), (432, 128.0)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[500..505].copy_from_slice(b"lamp\0");
        bytes
    }

    #[test]
    fn preserves_attachment_record_and_one_based_selector() {
        let (records, selected) = read(&fixture(), 1, &Limits::default()).unwrap();
        assert_eq!(selected, Some(0));
        assert_eq!(records[0].name, "lamp");
        assert_eq!(records[0].flags, 0x1234);
        assert_eq!(records[0].local[0][3], 128.0);
    }

    #[test]
    fn absent_and_virtual_attachment_selectors_are_distinct() {
        let mut bytes = fixture();
        bytes[520..524].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(read(&bytes, 1, &Limits::default()).unwrap().1, None);
        bytes[520..524].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(read(&bytes, 1, &Limits::default()).unwrap().1, Some(8));
    }

    #[test]
    fn rejects_truncated_tables_bad_bones_and_nonfinite_transforms() {
        assert!(read(&fixture()[..470], 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[416..420].copy_from_slice(&1u32.to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[420..424].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[520..524].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
    }

    #[test]
    fn bounds_counts_offsets_and_names() {
        let mut bytes = fixture();
        bytes[240..244].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[244..248].copy_from_slice(&240u32.to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[408..412].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(read(&bytes, 1, &Limits::default()).is_err());
        let mut bytes = fixture();
        bytes[500..].fill(1);
        assert!(read(&bytes, 1, &Limits::default()).is_err());
    }
}
