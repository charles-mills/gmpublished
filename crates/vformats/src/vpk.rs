//! VPK archive directories (versions 1 and 2), sans-io.
//!
//! [`parse`] reads a `_dir.vpk`'s bytes and yields entries
//! whose payloads are *located*, not read: preload bytes are borrowed
//! from the directory, and [`VpkLocation`] names exactly which file and
//! byte range holds the rest. The caller owns all I/O — mmap, `pread`,
//! or an HTTP range request are equally at home:
//!
//! - [`VpkLocation::InDirectory`]: read from the `_dir.vpk` itself at
//!   the given absolute offset.
//! - [`VpkLocation::InArchive`]: open the numbered sibling
//!   (`pak01_dir.vpk` → `pak01_007.vpk`, see [`sibling_archive_name`])
//!   and read the range.
//!
//! [`VpkEntry::assemble`] glues preload + chunk bytes back into the
//! full payload. Game-directory discovery and mount ordering are
//! deliberately out of scope — that is application policy, not format.

use std::borrow::Cow;
use std::fmt;

use crate::Limits;
use crate::entry_path::is_unsafe_entry_path;

const VPK_SIGNATURE: u32 = 0x55aa_1234;
const VPK_V1_HEADER_BYTES: u64 = 12;
const VPK_V2_HEADER_BYTES: u64 = 28;
const VPK_DIR_ARCHIVE_INDEX: u16 = 0x7fff;
const VPK_ENTRY_TERMINATOR: u16 = 0xffff;

/// A parsed VPK directory: validated header plus located entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpkDirectory<'a> {
    version: u32,
    /// Entries sorted by path (deterministic; duplicate paths keep the
    /// last occurrence, matching engine override order).
    entries: Vec<VpkEntry<'a>>,
}

/// One directory entry with its payload location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpkEntry<'a> {
    /// Full entry path, composed from the tree's extension/directory/
    /// name triple (owning is honest: the wire format never stores it
    /// contiguously).
    pub path: String,
    /// IEEE CRC-32 of the full payload (preload + located bytes).
    pub crc32: u32,
    /// Payload head stored inline in the directory, borrowed.
    pub preload: &'a [u8],
    /// Where the rest of the payload lives.
    pub location: VpkLocation,
}

/// Where an entry's non-preload bytes live. `len` can be zero (payload
/// entirely in preload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VpkLocation {
    /// In the `_dir.vpk` itself, at this absolute byte offset.
    InDirectory {
        /// Absolute offset within the directory file.
        offset: u64,
        /// Byte length.
        len: u64,
    },
    /// In a numbered sibling archive (see [`sibling_archive_name`]).
    InArchive {
        /// Archive number, e.g. 7 for `pak01_007.vpk`.
        archive: u16,
        /// Absolute offset within that archive file.
        offset: u64,
        /// Byte length.
        len: u64,
    },
}

impl VpkLocation {
    /// Byte length of the located (non-preload) payload part.
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::InDirectory { len, .. } | Self::InArchive { len, .. } => *len,
        }
    }

    /// Whether the payload is entirely in preload.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl VpkEntry<'_> {
    /// Full payload byte length (preload + located part).
    #[must_use]
    pub fn size(&self) -> u64 {
        self.preload.len() as u64 + self.location.len()
    }

    /// Glue preload and the located bytes (read by the caller per
    /// [`VpkEntry::location`]) into the full payload.
    #[must_use]
    pub fn assemble(&self, chunk: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.preload.len() + chunk.len());
        bytes.extend_from_slice(self.preload);
        bytes.extend_from_slice(chunk);
        bytes
    }
}

impl<'a> VpkDirectory<'a> {
    /// Directory format version (1 or 2).
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// All entries, sorted by path.
    #[must_use]
    pub fn entries(&self) -> &[VpkEntry<'a>] {
        &self.entries
    }

    /// Look up one entry by exact path.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&VpkEntry<'a>> {
        self.entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.entries[index])
    }
}

/// The sibling archive file name for a directory file name and archive
/// number: `pak01_dir.vpk`, 7 → `pak01_007.vpk`. `None` when the
/// directory name does not end in `_dir.vpk`.
#[must_use]
pub fn sibling_archive_name(dir_file_name: &str, archive: u16) -> Option<String> {
    let prefix = dir_file_name.strip_suffix("_dir.vpk")?;
    Some(format!("{prefix}_{archive:03}.vpk"))
}

/// VPK directory parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VpkError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// The file does not start with the VPK signature.
    BadMagic,
    /// Not a version 1 or 2 directory.
    UnsupportedVersion(u32),
    /// The directory ends before a required structure (the common
    /// symptom of a truncated `_dir.vpk` download).
    Truncated {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// The directory tree is malformed (unterminated string, overflow).
    Corrupt,
    /// An entry path would escape an extraction root.
    UnsafePath(String),
    /// Entry count exceeds [`Limits::max_entries`].
    TooManyEntries {
        /// The configured cap.
        max: usize,
    },
    /// An entry's declared preload-plus-located size exceeds
    /// [`Limits::max_entry_bytes`].
    EntryTooLarge {
        /// Declared entry size.
        size: u64,
        /// The configured cap.
        max: u64,
    },
}

impl fmt::Display for VpkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "vpk directory is {len} bytes, over the {max}-byte limit")
            }
            Self::BadMagic => write!(f, "not a vpk directory (bad signature)"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported vpk version {version}")
            }
            Self::Truncated { needed, available } => {
                write!(f, "vpk truncated: need {needed} bytes, have {available}")
            }
            Self::Corrupt => write!(f, "vpk directory tree is malformed"),
            Self::UnsafePath(path) => write!(f, "vpk entry path is unsafe: {path:?}"),
            Self::TooManyEntries { max } => {
                write!(f, "vpk entry count exceeds the limit of {max}")
            }
            Self::EntryTooLarge { size, max } => {
                write!(f, "vpk entry of {size} bytes exceeds the {max}-byte limit")
            }
        }
    }
}

impl std::error::Error for VpkError {}

impl crate::reader::ReadError for VpkError {
    fn truncated(needed: u64, available: u64) -> Self {
        Self::Truncated { needed, available }
    }
    fn overflow() -> Self {
        Self::Corrupt
    }
}

type Reader<'a> = crate::reader::Reader<'a, VpkError>;

impl<'a> Reader<'a> {
    fn u16(&mut self) -> Result<u16, VpkError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, VpkError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// NUL-terminated tree string, lossily decoded (real trees contain
    /// non-UTF-8 name bytes).
    fn c_string(&mut self) -> Result<Cow<'a, str>, VpkError> {
        let rest = self.bytes.get(self.pos..).ok_or(VpkError::Corrupt)?;
        let nul = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(VpkError::Corrupt)?;
        let value = &rest[..nul];
        self.pos += nul + 1;
        Ok(String::from_utf8_lossy(value))
    }
}

/// Parse a `_dir.vpk`'s bytes into located entries.
pub fn parse<'a>(dir_bytes: &'a [u8], limits: &Limits) -> Result<VpkDirectory<'a>, VpkError> {
    if dir_bytes.len() as u64 > limits.max_input_bytes {
        return Err(VpkError::InputTooLarge {
            len: dir_bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(dir_bytes, 0);
    if r.u32().map_err(|_| VpkError::BadMagic)? != VPK_SIGNATURE {
        return Err(VpkError::BadMagic);
    }
    let version = r.u32().map_err(|_| VpkError::BadMagic)?;
    let tree_size = u64::from(r.u32().map_err(|_| VpkError::BadMagic)?);
    let header_size = match version {
        1 => VPK_V1_HEADER_BYTES,
        2 => {
            r.take(16).map_err(|_| VpkError::BadMagic)?;
            VPK_V2_HEADER_BYTES
        }
        other => return Err(VpkError::UnsupportedVersion(other)),
    };
    let tree_end = header_size
        .checked_add(tree_size)
        .filter(|end| *end <= dir_bytes.len() as u64)
        .ok_or(VpkError::Corrupt)?;
    // Embedded payload offsets in the tree are relative to the end of
    // the tree; expose them as absolute directory-file offsets.
    let data_base = tree_end;

    let mut entries = Vec::new();
    loop {
        let extension = r.c_string()?;
        if extension.is_empty() {
            break;
        }
        loop {
            let directory = r.c_string()?;
            if directory.is_empty() {
                break;
            }
            loop {
                let name = r.c_string()?;
                if name.is_empty() {
                    break;
                }
                let crc32 = r.u32()?;
                let preload_bytes = r.u16()?;
                let archive_index = r.u16()?;
                let entry_offset = r.u32()?;
                let entry_length = r.u32()?;
                if r.u16()? != VPK_ENTRY_TERMINATOR {
                    return Err(VpkError::Corrupt);
                }
                let declared_size = u64::from(preload_bytes) + u64::from(entry_length);
                if declared_size > limits.max_entry_bytes {
                    return Err(VpkError::EntryTooLarge {
                        size: declared_size,
                        max: limits.max_entry_bytes,
                    });
                }
                let preload = r.take(usize::from(preload_bytes))?;
                if r.pos as u64 > tree_end {
                    return Err(VpkError::Corrupt);
                }

                let path = compose_entry_path(&directory, &name, &extension);
                if is_unsafe_entry_path(&path) {
                    return Err(VpkError::UnsafePath(path));
                }
                if entries.len() >= limits.max_entries {
                    return Err(VpkError::TooManyEntries {
                        max: limits.max_entries,
                    });
                }

                let location = if archive_index == VPK_DIR_ARCHIVE_INDEX {
                    VpkLocation::InDirectory {
                        offset: data_base
                            .checked_add(u64::from(entry_offset))
                            .ok_or(VpkError::Corrupt)?,
                        len: u64::from(entry_length),
                    }
                } else {
                    VpkLocation::InArchive {
                        archive: archive_index,
                        offset: u64::from(entry_offset),
                        len: u64::from(entry_length),
                    }
                };
                entries.push(VpkEntry {
                    path,
                    crc32,
                    preload,
                    location,
                });
            }
        }
    }
    if r.pos as u64 > tree_end {
        return Err(VpkError::Corrupt);
    }

    // Deterministic order; duplicate paths keep the last occurrence
    // (engine override order). The sort is stable, so after sorting by
    // path, the last of equal paths is the later tree occurrence.
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|later, earlier| {
        if later.path == earlier.path {
            *earlier = later.clone();
            true
        } else {
            false
        }
    });

    Ok(VpkDirectory { version, entries })
}

/// The tree stores empty directory/extension components as `" "`.
fn compose_entry_path(directory: &str, name: &str, extension: &str) -> String {
    let directory = none_component(directory);
    let extension = none_component(extension);
    let mut path = String::with_capacity(directory.len() + name.len() + extension.len() + 2);
    if !directory.is_empty() {
        path.push_str(directory);
        path.push('/');
    }
    path.push_str(name);
    if !extension.is_empty() {
        path.push('.');
        path.push_str(extension);
    }
    path
}

fn none_component(component: &str) -> &str {
    if component == " " { "" } else { component }
}

impl fmt::Display for VpkLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InDirectory { offset, len } => {
                write!(f, "directory file @{offset}+{len}")
            }
            Self::InArchive {
                archive,
                offset,
                len,
            } => write!(f, "archive {archive:03} @{offset}+{len}"),
        }
    }
}
