use std::io::{BufRead, BufReader, Read};

use crate::gma::{GMA_VERSION, GmaError};
use vformats::{ArchiveIndexCost, ArchiveIndexLimits};

const MAGIC: &[u8; 4] = b"GMAD";

#[derive(Debug)]
pub(super) struct ParsedGma {
    pub(super) metadata: ParsedMetadata,
    pub(super) entries: Vec<ParsedEntry>,
}

#[derive(Debug)]
pub(super) struct ParsedMetadata {
    pub(super) version: u8,
    pub(super) timestamp: u64,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) author: String,
    pub(super) addon_version: i32,
}

#[derive(Clone, Debug)]
pub(super) struct ParsedEntry {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) crc: u32,
    pub(super) data_offset: u64,
}

#[derive(Default)]
struct StringLength {
    complete: u64,
    pending: [u8; 4],
    count: usize,
}

impl StringLength {
    fn feed(&mut self, bytes: &[u8]) -> Result<(), GmaError> {
        for &byte in bytes {
            self.pending[self.count] = byte;
            self.count += 1;
            loop {
                let consumed = match std::str::from_utf8(&self.pending[..self.count]) {
                    Ok(_) => {
                        self.complete = self
                            .complete
                            .checked_add(self.count as u64)
                            .ok_or(GmaError::FormatError)?;
                        self.count = 0;
                        break;
                    }
                    Err(error) if error.valid_up_to() > 0 => {
                        self.complete = self
                            .complete
                            .checked_add(error.valid_up_to() as u64)
                            .ok_or(GmaError::FormatError)?;
                        error.valid_up_to()
                    }
                    Err(error) => match error.error_len() {
                        Some(consumed) => {
                            self.complete =
                                self.complete.checked_add(3).ok_or(GmaError::FormatError)?;
                            consumed
                        }
                        None => break,
                    },
                };
                self.pending.copy_within(consumed..self.count, 0);
                self.count -= consumed;
                if self.count == 0 {
                    break;
                }
            }
        }
        Ok(())
    }

    fn prefix_len(&self) -> Result<u64, GmaError> {
        self.complete
            .checked_add(if self.count == 0 { 0 } else { 3 })
            .ok_or(GmaError::FormatError)
    }
}

struct IndexReader<R> {
    inner: BufReader<std::io::Take<R>>,
    cost: ArchiveIndexCost,
    limits: ArchiveIndexLimits,
}

impl<R: Read> IndexReader<R> {
    fn new(inner: R, limits: ArchiveIndexLimits) -> Self {
        Self {
            inner: BufReader::with_capacity(64 * 1024, inner.take(limits.directory_bytes)),
            cost: ArchiveIndexCost::default(),
            limits,
        }
    }

    fn position(&self) -> u64 {
        self.cost.directory_bytes
    }

    fn advance(&mut self, count: usize) -> Result<(), GmaError> {
        self.cost.directory_bytes = self
            .cost
            .directory_bytes
            .checked_add(count as u64)
            .ok_or(GmaError::FormatError)?;
        self.limits.check(self.cost).map_err(GmaError::IndexLimit)
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], GmaError> {
        self.advance(N)?;
        let mut bytes = [0; N];
        self.inner.read_exact(&mut bytes).map_err(map_read_error)?;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, GmaError> {
        Ok(self.bytes::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32, GmaError> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64, GmaError> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn i32(&mut self) -> Result<i32, GmaError> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }
    fn i64(&mut self) -> Result<i64, GmaError> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }

    fn c_string(&mut self, retain: bool) -> Result<String, GmaError> {
        let mut value = Vec::new();
        let mut length = StringLength::default();
        let mut raw_length = 0_u64;
        loop {
            let (consumed, finished) = {
                let available = match self.inner.fill_buf() {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result.map_err(map_read_error)?,
                };
                if available.is_empty() {
                    if self.position() >= self.limits.directory_bytes {
                        self.advance(1)?;
                    }
                    return Err(GmaError::FormatError);
                }
                let nul = available.iter().position(|byte| *byte == 0);
                let count = nul.unwrap_or(available.len());
                length.feed(&available[..count])?;
                raw_length = raw_length
                    .checked_add(count as u64)
                    .ok_or(GmaError::FormatError)?;
                let mut next = self.cost;
                next.string_bytes = next
                    .string_bytes
                    .checked_add(length.prefix_len()?)
                    .ok_or(GmaError::FormatError)?;
                next.copied_bytes = next
                    .copied_bytes
                    .max(raw_length.checked_mul(2).ok_or(GmaError::FormatError)?);
                next.directory_bytes = next
                    .directory_bytes
                    .checked_add((count + usize::from(nul.is_some())) as u64)
                    .ok_or(GmaError::FormatError)?;
                self.limits.check(next).map_err(GmaError::IndexLimit)?;
                if retain {
                    value
                        .try_reserve_exact(count)
                        .map_err(|_| GmaError::FormatError)?;
                    value.extend_from_slice(&available[..count]);
                }
                (count + usize::from(nul.is_some()), nul.is_some())
            };
            self.inner.consume(consumed);
            self.advance(consumed)?;
            if finished {
                self.cost.string_bytes = self
                    .cost
                    .string_bytes
                    .checked_add(length.prefix_len()?)
                    .ok_or(GmaError::FormatError)?;
                self.cost.copied_bytes = self
                    .cost
                    .copied_bytes
                    .max(raw_length.checked_mul(2).ok_or(GmaError::FormatError)?);
                return if retain {
                    Ok(String::from_utf8(value).unwrap_or_else(|error| {
                        String::from_utf8_lossy(error.as_bytes()).into_owned()
                    }))
                } else {
                    Ok(String::new())
                };
            }
        }
    }

    fn entry(&mut self) -> Result<(), GmaError> {
        self.cost.entries = self
            .cost
            .entries
            .checked_add(1)
            .ok_or(GmaError::FormatError)?;
        self.cost.record_bytes = (self.cost.entries as u64)
            .checked_mul(std::mem::size_of::<ParsedEntry>() as u64)
            .ok_or(GmaError::FormatError)?;
        self.limits.check(self.cost).map_err(GmaError::IndexLimit)
    }
}

fn map_read_error(error: std::io::Error) -> GmaError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        GmaError::FormatError
    } else {
        error.into()
    }
}

pub(super) fn preflight(
    reader: impl Read,
    archive_len: u64,
    limits: ArchiveIndexLimits,
) -> Result<ArchiveIndexCost, GmaError> {
    scan(reader, archive_len, limits, None).map(|(_, cost)| cost)
}

pub(super) fn parse(
    reader: impl Read,
    archive_len: u64,
    limits: ArchiveIndexLimits,
    cost: ArchiveIndexCost,
) -> Result<ParsedGma, GmaError> {
    scan(reader, archive_len, limits, Some(cost.entries)).and_then(|(parsed, actual)| {
        if actual == cost {
            Ok(parsed)
        } else {
            Err(GmaError::FormatError)
        }
    })
}

fn scan(
    reader: impl Read,
    archive_len: u64,
    limits: ArchiveIndexLimits,
    capacity: Option<usize>,
) -> Result<(ParsedGma, ArchiveIndexCost), GmaError> {
    let retain = capacity.is_some();
    let mut reader = IndexReader::new(reader, limits);
    if &reader.bytes::<4>()? != MAGIC {
        return Err(GmaError::InvalidHeader);
    }
    let version = reader.u8()?;
    if version > GMA_VERSION {
        return Err(GmaError::InvalidHeader);
    }
    reader.u64()?;
    let timestamp = reader.u64()?;
    if version > 1 {
        let mut required_content_count = 0usize;
        loop {
            let before = reader.position();
            reader.c_string(retain)?;
            if reader.position() - before == 1 {
                break;
            }
            required_content_count = required_content_count
                .checked_add(1)
                .ok_or(GmaError::FormatError)?;
            if required_content_count > limits.entries {
                return Err(GmaError::IndexLimit(vformats::ArchiveIndexLimit {
                    kind: vformats::ArchiveIndexLimitKind::Entries,
                    required: required_content_count as u64,
                    limit: limits.entries as u64,
                }));
            }
        }
    }
    let name = reader.c_string(retain)?;
    let description = reader.c_string(retain)?;
    let author = reader.c_string(retain)?;
    let addon_version = reader.i32()?;
    let mut entries = Vec::with_capacity(capacity.unwrap_or(0));
    let mut payload_bytes = 0_u64;
    loop {
        if reader.u32()? == 0 {
            break;
        }
        reader.entry()?;
        let path = reader.c_string(retain)?;
        let size = u64::try_from(reader.i64()?).map_err(|_| GmaError::FormatError)?;
        let crc = reader.u32()?;
        payload_bytes = payload_bytes
            .checked_add(size)
            .ok_or(GmaError::FormatError)?;
        if retain {
            if entries.len() >= capacity.unwrap_or(0) {
                return Err(GmaError::FormatError);
            }
            entries.push(ParsedEntry {
                path,
                size,
                crc,
                data_offset: 0,
            });
        }
    }
    let mut data_offset = reader.position();
    if data_offset
        .checked_add(payload_bytes)
        .is_none_or(|end| end > archive_len)
    {
        return Err(GmaError::FormatError);
    }
    for entry in &mut entries {
        entry.data_offset = data_offset;
        data_offset = data_offset
            .checked_add(entry.size)
            .ok_or(GmaError::FormatError)?;
    }
    Ok((
        ParsedGma {
            metadata: ParsedMetadata {
                version,
                timestamp,
                name,
                description,
                author,
                addon_version,
            },
            entries,
        },
        reader.cost,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_lossy_utf8_cost_matches_owned_decoding_across_every_chunk_boundary() {
        let values: &[&[u8]] = &[
            b"valid",
            &[0xff, 0xfe],
            &[0xe2, 0x82],
            &[0xf0, 0x90, 0x80, 0x80, 0xff],
            &[0xe2, 0x28, 0xa1],
            &[0xed, 0xa0, 0x80],
        ];
        for bytes in values {
            for split in 0..=bytes.len() {
                let mut length = StringLength::default();
                length.feed(&bytes[..split]).unwrap();
                length.feed(&bytes[split..]).unwrap();
                assert_eq!(
                    length.prefix_len().unwrap(),
                    String::from_utf8_lossy(bytes).len() as u64
                );
            }
        }
    }
}
