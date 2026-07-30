//! Owned, streaming GMA index parsing.
//!
//! `vformats::gma` is the canonical whole-slice parser and writer. Its parsed
//! representation borrows both the entry table and every payload, which is a
//! great fit for in-memory data but would require mapping or allocating an
//! entire multi-gigabyte addon here. This small adapter mirrors the read-side
//! wire layout while retaining only the header and entry extents.

use std::io::{BufRead, BufReader, Read};

use crate::gma::{GMA_VERSION, GmaError};

const MAGIC: &[u8; 4] = b"GMAD";

// Payloads are deliberately uncapped, but the retained index must stay
// bounded even when the archive is hostile. These limits are far above real
// GMA metadata/path sizes and allow roughly a million ordinary entry rows.
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;

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

struct IndexReader<R> {
    inner: BufReader<R>,
    position: u64,
}

impl<R: Read> IndexReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner: BufReader::with_capacity(64 * 1024, inner),
            position: 0,
        }
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn ensure_index_limit(&self) -> Result<(), GmaError> {
        if self.position > MAX_INDEX_BYTES {
            Err(GmaError::FormatError)
        } else {
            Ok(())
        }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], GmaError> {
        let mut bytes = [0; N];
        self.inner.read_exact(&mut bytes).map_err(map_read_error)?;
        self.position = self
            .position
            .checked_add(N as u64)
            .ok_or(GmaError::FormatError)?;
        self.ensure_index_limit()?;
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

    /// Reads a NUL-terminated string without letting a missing terminator
    /// grow a `Vec` to the size of the archive.
    fn c_string(&mut self) -> Result<String, GmaError> {
        let mut value = Vec::new();

        loop {
            let (consumed, finished) = {
                let available = match self.inner.fill_buf() {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result.map_err(map_read_error)?,
                };
                if available.is_empty() {
                    return Err(GmaError::FormatError);
                }

                if let Some(nul) = available.iter().position(|byte| *byte == 0) {
                    if value.len().saturating_add(nul) > MAX_STRING_BYTES {
                        return Err(GmaError::FormatError);
                    }
                    value.extend_from_slice(&available[..nul]);
                    (nul + 1, true)
                } else {
                    if value.len().saturating_add(available.len()) > MAX_STRING_BYTES {
                        return Err(GmaError::FormatError);
                    }
                    value.extend_from_slice(available);
                    (available.len(), false)
                }
            };

            self.inner.consume(consumed);
            self.position = self
                .position
                .checked_add(consumed as u64)
                .ok_or(GmaError::FormatError)?;
            self.ensure_index_limit()?;

            if finished {
                return Ok(String::from_utf8_lossy(&value).into_owned());
            }
        }
    }
}

fn map_read_error(error: std::io::Error) -> GmaError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        GmaError::FormatError
    } else {
        error.into()
    }
}

pub(super) fn parse(reader: impl Read, archive_len: u64) -> Result<ParsedGma, GmaError> {
    let mut reader = IndexReader::new(reader);

    if &reader.bytes::<4>()? != MAGIC {
        return Err(GmaError::InvalidHeader);
    }
    let version = reader.u8()?;
    if version > GMA_VERSION {
        return Err(GmaError::InvalidHeader);
    }

    let _steam_id = reader.u64()?;
    let timestamp = reader.u64()?;

    let max_entries = vformats::Limits::default().max_entries;
    if version > 1 {
        let mut required_content_count = 0usize;
        loop {
            let content = reader.c_string()?;
            if content.is_empty() {
                break;
            }
            required_content_count = required_content_count
                .checked_add(1)
                .ok_or(GmaError::FormatError)?;
            if required_content_count > max_entries {
                return Err(GmaError::FormatError);
            }
        }
    }

    let name = reader.c_string()?;
    let description = reader.c_string()?;
    let author = reader.c_string()?;
    let addon_version = reader.i32()?;

    let mut entries = Vec::new();
    loop {
        if reader.u32()? == 0 {
            break;
        }
        if entries.len() >= max_entries {
            return Err(GmaError::FormatError);
        }

        let path = reader.c_string()?;
        let size = u64::try_from(reader.i64()?).map_err(|_| GmaError::FormatError)?;
        let crc = reader.u32()?;
        entries.push(ParsedEntry {
            path,
            size,
            crc,
            data_offset: 0,
        });
    }

    // Payloads follow the table in entry order. Validate the entire declared
    // extent up front so an index bundle can never name bytes outside the
    // file as it existed when the view was opened.
    let mut data_offset = reader.position();
    for entry in &mut entries {
        entry.data_offset = data_offset;
        data_offset = data_offset
            .checked_add(entry.size)
            .ok_or(GmaError::FormatError)?;
    }
    if data_offset > archive_len {
        return Err(GmaError::FormatError);
    }

    Ok(ParsedGma {
        metadata: ParsedMetadata {
            version,
            timestamp,
            name,
            description,
            author,
            addon_version,
        },
        entries,
    })
}
