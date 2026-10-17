use std::fmt;

#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveIndexLimits {
    pub directory_bytes: u64,
    pub entries: usize,
    pub retained_bytes: u64,
}

impl Default for ArchiveIndexLimits {
    fn default() -> Self {
        Self {
            directory_bytes: 1024 * 1024 * 1024,
            entries: 1_000_000,
            retained_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Requested record, string and preload storage; excludes allocator overhead.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArchiveIndexCost {
    pub directory_bytes: u64,
    pub entries: usize,
    pub record_bytes: u64,
    pub string_bytes: u64,
    pub copied_bytes: u64,
}

#[allow(missing_docs)]
impl ArchiveIndexCost {
    #[must_use]
    pub fn retained_bytes(self) -> Option<u64> {
        self.record_bytes
            .checked_add(self.string_bytes)?
            .checked_add(self.copied_bytes)
    }
}

#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveIndexLimitKind {
    DirectoryBytes,
    Entries,
    RetainedBytes,
    WorkingBytes,
    ArithmeticOverflow,
}

#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveIndexLimit {
    pub kind: ArchiveIndexLimitKind,
    pub required: u64,
    pub limit: u64,
}

#[allow(missing_docs)]
impl ArchiveIndexLimit {
    #[must_use]
    pub const fn arithmetic_overflow() -> Self {
        Self {
            kind: ArchiveIndexLimitKind::ArithmeticOverflow,
            required: 0,
            limit: 0,
        }
    }
}

impl fmt::Display for ArchiveIndexLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == ArchiveIndexLimitKind::ArithmeticOverflow {
            return f.write_str("archive index cost arithmetic overflow");
        }
        write!(
            f,
            "archive index {:?} requires {}; limit {}",
            self.kind, self.required, self.limit
        )
    }
}
impl std::error::Error for ArchiveIndexLimit {}

#[allow(missing_docs)]
impl ArchiveIndexLimits {
    pub fn check(self, cost: ArchiveIndexCost) -> Result<(), ArchiveIndexLimit> {
        let retained = cost
            .retained_bytes()
            .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?;
        for (kind, required, limit) in [
            (
                ArchiveIndexLimitKind::DirectoryBytes,
                cost.directory_bytes,
                self.directory_bytes,
            ),
            (
                ArchiveIndexLimitKind::Entries,
                cost.entries as u64,
                self.entries as u64,
            ),
            (
                ArchiveIndexLimitKind::RetainedBytes,
                retained,
                self.retained_bytes,
            ),
        ] {
            if required > limit {
                return Err(ArchiveIndexLimit {
                    kind,
                    required,
                    limit,
                });
            }
        }
        Ok(())
    }
}

pub fn lossy_utf8_len(mut bytes: &[u8]) -> Option<usize> {
    let mut length = 0usize;
    loop {
        match std::str::from_utf8(bytes) {
            Ok(_) => return length.checked_add(bytes.len()),
            Err(error) => {
                length = length.checked_add(error.valid_up_to())?.checked_add(3)?;
                bytes = &bytes[error.valid_up_to()..];
                let Some(invalid) = error.error_len() else {
                    return Some(length);
                };
                bytes = &bytes[invalid..];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_cost_overflow_rejects_even_an_unbounded_profile() {
        let limits = ArchiveIndexLimits {
            directory_bytes: u64::MAX,
            entries: usize::MAX,
            retained_bytes: u64::MAX,
        };
        for cost in [
            ArchiveIndexCost {
                record_bytes: u64::MAX,
                string_bytes: 1,
                ..Default::default()
            },
            ArchiveIndexCost {
                string_bytes: u64::MAX,
                copied_bytes: 1,
                ..Default::default()
            },
            ArchiveIndexCost {
                record_bytes: u64::MAX - 1,
                string_bytes: 1,
                copied_bytes: 1,
                ..Default::default()
            },
        ] {
            let error = limits.check(cost).unwrap_err();
            assert_eq!(error.kind, ArchiveIndexLimitKind::ArithmeticOverflow);
            assert_eq!(error.to_string(), "archive index cost arithmetic overflow");
        }
        assert!(
            limits
                .check(ArchiveIndexCost {
                    record_bytes: u64::MAX - 2,
                    string_bytes: 1,
                    copied_bytes: 1,
                    ..Default::default()
                })
                .is_ok()
        );
    }

    #[test]
    fn lossy_cost_matches_valid_and_invalid_utf8_without_materializing() {
        for bytes in [
            b"materials/abc".as_slice(),
            &[0xff, 0xfe, b'a'],
            &[0xe2, 0x82],
            &[0xe2, 0x82, 0xac],
        ] {
            assert_eq!(
                lossy_utf8_len(bytes),
                Some(String::from_utf8_lossy(bytes).len())
            );
        }
    }
}
