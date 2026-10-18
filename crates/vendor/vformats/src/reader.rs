//! Crate-internal bounded cursor over untrusted bytes, shared by the
//! binary format modules (vtf, vpk, gma, mdl).
//!
//! The bounds and overflow logic lives here exactly once. Each format
//! keeps its own error type and little-endian convenience methods in
//! an `impl Reader<'_, TheirError>` block next to the format — those
//! are trivial wrappers; this is the part that must not drift.

use std::marker::PhantomData;

/// Maps cursor failures into a format module's error type.
pub trait ReadError {
    /// The input ends before `needed` bytes are available.
    fn truncated(needed: u64, available: u64) -> Self;
    /// Position arithmetic overflowed (declared offsets near
    /// `usize::MAX`) — a malformed structure, not mere truncation.
    fn overflow() -> Self;
}

/// A bounds-checked cursor. Fields are crate-visible so format modules
/// can implement their own scanning helpers (NUL-terminated strings)
/// against the same position.
pub struct Reader<'a, E> {
    pub bytes: &'a [u8],
    pub pos: usize,
    error: PhantomData<E>,
}

impl<'a, E: ReadError> Reader<'a, E> {
    pub fn at(bytes: &'a [u8], pos: usize) -> Self {
        Self {
            bytes,
            pos,
            error: PhantomData,
        }
    }

    /// The next `n` bytes, advancing past them.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], E> {
        let end = self.pos.checked_add(n).ok_or_else(E::overflow)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| E::truncated(end as u64, self.bytes.len() as u64))?;
        self.pos = end;
        Ok(slice)
    }
}
