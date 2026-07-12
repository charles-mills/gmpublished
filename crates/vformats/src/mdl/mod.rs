//! Source engine model files: `.mdl` (skeleton, materials, bodygroups),
//! `.vvd` (vertex data), `.vtx` (triangle strips). Together they carry
//! everything needed to assemble render geometry — this module targets
//! the TF2/GMod era (MDL v44–49).
//!
//! Parse with [`parse_mdl`], [`parse_vvd`], and [`parse_vtx`], then
//! [`assemble`] LOD-0 render geometry from the three. Raw struct
//! layouts follow the `vmdl` crate (MIT, © icewind1991) with two known
//! upstream defects corrected at the source instead of worked around:
//! `BoneWeights::weights()` divides by bone count (the VVD-authored raw
//! weights are what the engine blends), and `RadianEuler` quaternion
//! conversion flips 180° on some bones (this module converts via the
//! studiomdl convention).

use std::fmt;

mod assembly;
pub use assembly::{
    AssemblySkip, MdlStats, MeshData, ModelData, ModelRead, ModelVertex, assemble, assemble_lossy,
};

mod studio;
pub use studio::{
    FLAG_STATIC_PROP, Mdl, MdlAnimation, MdlAnimationDescription, MdlBodyPart, MdlBone, MdlMesh,
    MdlModel, parse_mdl,
};

mod vtx;
pub use vtx::{
    Vtx, VtxBodyPart, VtxLod, VtxMesh, VtxModel, VtxStrip, VtxStripGroup, VtxVertex, parse_vtx,
};

mod vvd;
pub use vvd::{Vvd, VvdVertex, parse_vvd};

/// MDL/VVD/VTX parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MdlError {
    /// Input exceeds [`crate::Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// The file does not start with the expected magic.
    BadMagic {
        /// Which file kind was being parsed.
        part: &'static str,
    },
    /// A file version outside the supported range.
    UnsupportedVersion {
        /// Which file kind was being parsed.
        part: &'static str,
        /// The version found.
        version: i32,
    },
    /// Input ends before a required structure.
    Truncated {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// A count, offset, or index is negative or out of range.
    Corrupt {
        /// Which structure was malformed.
        part: &'static str,
    },
    /// A structure count exceeds [`crate::Limits::max_entries`].
    TooMany {
        /// Which structure overflowed the cap.
        part: &'static str,
        /// The configured cap.
        max: usize,
    },
    /// A mesh references a material outside the `.mdl`'s table.
    MaterialIndex {
        /// The out-of-range material index.
        index: i32,
    },
    /// A strip references a vertex outside the `.vvd`'s pool.
    VertexIndex {
        /// The out-of-range vertex index.
        index: usize,
    },
    /// A single mesh exceeds `u32` vertices.
    TooManyVertices,
    /// The `.mdl`/`.vvd`/`.vtx` trio's checksums disagree — they are
    /// not a matched set (e.g. a model updated without its geometry).
    ChecksumMismatch {
        /// `.mdl` checksum.
        mdl: u32,
        /// `.vvd` checksum.
        vvd: u32,
        /// `.vtx` checksum.
        vtx: u32,
    },
    /// The `.mdl`'s mesh count and the `.vtx`'s LOD-0 mesh count
    /// disagree.
    MeshCountMismatch {
        /// Meshes declared by the `.mdl`.
        mdl: usize,
        /// LOD-0 meshes declared by the `.vtx`.
        vtx: usize,
    },
}

impl fmt::Display for MdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "model input is {len} bytes, over the {max}-byte limit")
            }
            Self::BadMagic { part } => write!(f, "not a {part} file (bad magic)"),
            Self::UnsupportedVersion { part, version } => {
                write!(f, "unsupported {part} version {version}")
            }
            Self::Truncated { needed, available } => {
                write!(
                    f,
                    "model file truncated: need {needed} bytes, have {available}"
                )
            }
            Self::Corrupt { part } => write!(f, "model {part} structure is malformed"),
            Self::TooMany { part, max } => {
                write!(f, "model {part} count exceeds the limit of {max}")
            }
            Self::MaterialIndex { index } => {
                write!(f, "mesh references out-of-range material {index}")
            }
            Self::VertexIndex { index } => {
                write!(f, "strip references out-of-range vertex {index}")
            }
            Self::TooManyVertices => write!(f, "mesh exceeds u32 vertices"),
            Self::ChecksumMismatch { mdl, vvd, vtx } => {
                write!(
                    f,
                    "mdl/vvd/vtx checksums disagree (mdl {mdl}, vvd {vvd}, vtx {vtx})"
                )
            }
            Self::MeshCountMismatch { mdl, vtx } => {
                write!(f, "mdl declares {mdl} meshes but vtx declares {vtx}")
            }
        }
    }
}

impl std::error::Error for MdlError {}

impl crate::reader::ReadError for MdlError {
    fn truncated(needed: u64, available: u64) -> Self {
        Self::Truncated { needed, available }
    }
    fn overflow() -> Self {
        Self::Corrupt { part: "offset" }
    }
}

/// Bounds-checked little-endian reader shared by the model parsers.
pub(crate) type Reader<'a> = crate::reader::Reader<'a, MdlError>;

impl Reader<'_> {
    pub(crate) fn i32(&mut self) -> Result<i32, MdlError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, MdlError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn f32(&mut self) -> Result<f32, MdlError> {
        self.u32().map(f32::from_bits)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, MdlError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn vec3(&mut self) -> Result<[f32; 3], MdlError> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    /// A non-negative i32 as usize, or `Corrupt`.
    pub(crate) fn count(&mut self, part: &'static str) -> Result<usize, MdlError> {
        usize::try_from(self.i32()?).map_err(|_| MdlError::Corrupt { part })
    }
}
