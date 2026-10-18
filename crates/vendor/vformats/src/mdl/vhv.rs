//! Compiled static-prop vertex lighting (`sp_N.vhv` / `sp_hdr_N.vhv`).
//!
//! Version 2 stores a 40-byte file header and 28-byte mesh records. Each
//! hardware mesh is one VTX strip group; its absolute offset locates a BGRA
//! stream. These are encoded lighting bytes, not ordinary texture colours.

use super::{ModelData, ModelLightingVertex};
use crate::Limits;
use std::fmt;

const HEADER_BYTES: usize = 40;
const MESH_BYTES: usize = 28;

/// Borrowed compiled static-prop lighting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vhv<'a> {
    /// Must match the source `.mdl` checksum.
    pub checksum: u32,
    /// Sum of all hardware vertex-pool sizes, across every stored LOD.
    pub vertex_count: u32,
    /// Bodypart/model/LOD/mesh/strip-group order, matching the VTX hierarchy.
    pub meshes: Vec<VhvMesh<'a>>,
}

/// One hardware mesh's compiled lighting stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VhvMesh<'a> {
    /// Source model level of detail.
    pub lod: u32,
    /// Hardware vertex-pool size, including vertices unused by triangles.
    pub vertex_count: u32,
    /// Four bytes per hardware vertex, in blue/green/red/alpha order.
    pub colors_bgra: &'a [u8],
}

impl VhvMesh<'_> {
    /// Returns encoded RGBA lighting for one original hardware vertex.
    #[must_use]
    pub fn color_rgba(&self, vertex: usize) -> Option<[u8; 4]> {
        if vertex >= self.vertex_count as usize {
            return None;
        }
        let start = vertex.checked_mul(4)?;
        let bytes = self.colors_bgra.get(start..start.checked_add(4)?)?;
        Some([bytes[2], bytes[1], bytes[0], bytes[3]])
    }
}

impl Vhv<'_> {
    /// Validate the complete hardware layout before attaching lighting to a model.
    /// Best-effort assembly of stale model companions cannot safely use VHV data.
    pub fn validate_model(&self, model: &ModelData) -> Result<(), VhvError> {
        self.validate_layout(
            model.checksum,
            model.lighting_compatible,
            &model.lighting_meshes,
        )?;
        for mesh in &model.meshes {
            if mesh.vertices.len() != mesh.vertex_lighting.len()
                || mesh
                    .vertex_lighting
                    .iter()
                    .any(|vertex| self.color_rgba(*vertex).is_none())
            {
                return Err(VhvError::Corrupt {
                    part: "model lighting correspondence",
                });
            }
        }
        Ok(())
    }

    /// Validate preserved model metadata when render geometry has been converted
    /// to an application-specific representation. Render-vertex correspondence
    /// must additionally be checked by that representation's consumer.
    pub fn validate_layout(
        &self,
        checksum: u32,
        compatible: bool,
        meshes: &[super::ModelLightingMesh],
    ) -> Result<(), VhvError> {
        if !compatible {
            return Err(VhvError::ModelIncompatible);
        }
        if self.checksum != checksum {
            return Err(VhvError::ChecksumMismatch {
                expected: checksum,
                actual: self.checksum,
            });
        }
        if self.meshes.len() != meshes.len() {
            return Err(VhvError::MeshCountMismatch {
                expected: meshes.len(),
                actual: self.meshes.len(),
            });
        }
        for (index, (actual, expected)) in self.meshes.iter().zip(meshes).enumerate() {
            if actual.lod != expected.lod || actual.vertex_count as usize != expected.vertex_count {
                return Err(VhvError::MeshLayoutMismatch { mesh: index });
            }
            if (actual.vertex_count as usize).checked_mul(4) != Some(actual.colors_bgra.len()) {
                return Err(VhvError::Corrupt {
                    part: "hardware vertex stream length",
                });
            }
        }
        if self
            .meshes
            .iter()
            .map(|mesh| u64::from(mesh.vertex_count))
            .sum::<u64>()
            != u64::from(self.vertex_count)
        {
            return Err(VhvError::Corrupt {
                part: "total vertex count",
            });
        }
        Ok(())
    }

    /// Resolve an assembled vertex's preserved hardware identity to encoded RGBA.
    #[must_use]
    pub fn color_rgba(&self, vertex: ModelLightingVertex) -> Option<[u8; 4]> {
        self.meshes.get(vertex.mesh)?.color_rgba(vertex.vertex)
    }
}

/// A malformed, unsupported, over-budget, or incompatible VHV file.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VhvError {
    /// Whole input exceeds the configured byte cap.
    InputTooLarge,
    /// A required structure or stream is incomplete.
    Truncated,
    /// Only published VHV version 2 is supported.
    UnsupportedVersion {
        /// Version read from the file header.
        version: u32,
    },
    /// Only four-byte `VERTEX_COLOR` streams are supported.
    UnsupportedVertexFormat,
    /// A count exceeds the configured entry cap.
    TooMany {
        /// The count's role.
        part: &'static str,
    },
    /// Invalid offsets, overlapping streams, or inconsistent counts.
    Corrupt {
        /// The offending structure.
        part: &'static str,
    },
    /// The model's companions were assembled with incompatible metadata.
    ModelIncompatible,
    /// The lighting was compiled for a different model revision.
    ChecksumMismatch {
        /// Checksum of the model being lit.
        expected: u32,
        /// Checksum in the lighting file.
        actual: u32,
    },
    /// The hardware mesh directories have different lengths.
    MeshCountMismatch {
        /// Hardware meshes declared by the model.
        expected: usize,
        /// Hardware meshes declared by the lighting file.
        actual: usize,
    },
    /// A hardware mesh's LOD or vertex count differs.
    MeshLayoutMismatch {
        /// Index of the mismatching hardware mesh.
        mesh: usize,
    },
}

impl fmt::Display for VhvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge => f.write_str("VHV input exceeds the byte limit"),
            Self::Truncated => f.write_str("VHV structure or vertex stream is truncated"),
            Self::UnsupportedVersion { version } => write!(f, "unsupported VHV version {version}"),
            Self::UnsupportedVertexFormat => f.write_str("unsupported VHV vertex format"),
            Self::TooMany { part } => write!(f, "VHV {part} exceeds the entry limit"),
            Self::Corrupt { part } => write!(f, "invalid VHV {part}"),
            Self::ModelIncompatible => {
                f.write_str("model companions are incompatible with compiled lighting")
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "VHV checksum {actual:#x} differs from model {expected:#x}"
            ),
            Self::MeshCountMismatch { expected, actual } => write!(
                f,
                "VHV has {actual} hardware meshes; model requires {expected}"
            ),
            Self::MeshLayoutMismatch { mesh } => {
                write!(f, "VHV hardware mesh {mesh} differs from the model layout")
            }
        }
    }
}

impl std::error::Error for VhvError {}

/// Parse a bounded, borrowed VHV v2 BGRA8888 lighting file.
/// Sector padding is optional; stream ranges must be aligned, disjoint and
/// outside the header directory. Every declared vertex must have data.
pub fn parse_vhv<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Vhv<'a>, VhvError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(VhvError::InputTooLarge);
    }
    if bytes.len() < HEADER_BYTES {
        return Err(VhvError::Truncated);
    }
    let version = u32_at(bytes, 0);
    if version != 2 {
        return Err(VhvError::UnsupportedVersion { version });
    }
    if u32_at(bytes, 8) != 4 || u32_at(bytes, 12) != 4 {
        return Err(VhvError::UnsupportedVertexFormat);
    }
    let vertex_count = u32_at(bytes, 16);
    let mesh_count = u32_at(bytes, 20) as usize;
    if mesh_count > i32::MAX as usize {
        return Err(VhvError::Corrupt { part: "mesh count" });
    }
    if mesh_count > limits.max_entries {
        return Err(VhvError::TooMany { part: "mesh count" });
    }
    if vertex_count as usize > limits.max_entries {
        return Err(VhvError::TooMany {
            part: "vertex count",
        });
    }
    let directory_end = mesh_count
        .checked_mul(MESH_BYTES)
        .and_then(|size| HEADER_BYTES.checked_add(size))
        .ok_or(VhvError::Corrupt {
            part: "mesh directory",
        })?;
    if directory_end > bytes.len() {
        return Err(VhvError::Truncated);
    }
    let mut meshes = Vec::with_capacity(mesh_count);
    let mut ranges = Vec::with_capacity(mesh_count);
    let mut total_vertices = 0_u64;
    for index in 0..mesh_count {
        let header = HEADER_BYTES + index * MESH_BYTES;
        let count = u32_at(bytes, header + 4);
        total_vertices += u64::from(count);
        if total_vertices > u64::from(vertex_count) {
            return Err(VhvError::Corrupt {
                part: "total vertex count",
            });
        }
        let offset = u32_at(bytes, header + 8) as usize;
        if offset < directory_end || offset % 4 != 0 {
            return Err(VhvError::Corrupt {
                part: "vertex stream offset",
            });
        }
        let end = (count as usize)
            .checked_mul(4)
            .and_then(|size| offset.checked_add(size))
            .ok_or(VhvError::Corrupt {
                part: "vertex stream range",
            })?;
        let colors_bgra = bytes.get(offset..end).ok_or(VhvError::Truncated)?;
        if offset != end {
            ranges.push((offset, end));
        }
        meshes.push(VhvMesh {
            lod: u32_at(bytes, header),
            vertex_count: count,
            colors_bgra,
        });
    }
    if total_vertices != u64::from(vertex_count) {
        return Err(VhvError::Corrupt {
            part: "total vertex count",
        });
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(VhvError::Corrupt {
            part: "overlapping vertex streams",
        });
    }
    Ok(Vhv {
        checksum: u32_at(bytes, 4),
        vertex_count,
        meshes,
    })
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated header"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = vec![0; 1024];
        for (offset, value) in [
            (0, 2),
            (4, 0x9277_4084),
            (8, 4),
            (12, 4),
            (16, 3),
            (20, 2),
            (40, 0),
            (44, 2),
            (48, 512),
            (68, 1),
            (72, 1),
            (76, 520),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&u32::to_le_bytes(value));
        }
        bytes[512..524].copy_from_slice(&[11, 22, 33, 255, 44, 55, 66, 128, 77, 88, 99, 0]);
        bytes
    }

    #[test]
    fn borrowed_padded_streams_preserve_lods_order_and_bgra_identity() {
        let bytes = fixture();
        let vhv = parse_vhv(&bytes, &Limits::default()).unwrap();
        assert_eq!(vhv.checksum, 0x9277_4084);
        assert_eq!(vhv.vertex_count, 3);
        assert_eq!(vhv.meshes[0].lod, 0);
        assert_eq!(vhv.meshes[1].lod, 1);
        assert_eq!(
            vhv.color_rgba(ModelLightingVertex { mesh: 0, vertex: 1 }),
            Some([66, 55, 44, 128])
        );
        assert_eq!(vhv.meshes[1].color_rgba(0), Some([99, 88, 77, 0]));
        assert_eq!(vhv.meshes[1].color_rgba(1), None);
        assert_eq!(vhv.meshes[0].colors_bgra.as_ptr(), bytes[512..].as_ptr());
        assert!(parse_vhv(&bytes[..524], &Limits::default()).is_ok());
    }

    #[test]
    fn truncation_at_every_required_boundary_returns_errors() {
        let bytes = fixture();
        for length in 0..524 {
            assert!(
                parse_vhv(&bytes[..length], &Limits::default()).is_err(),
                "length {length}"
            );
        }
    }

    #[test]
    fn rejects_untrusted_counts_formats_offsets_overlap_and_inconsistent_totals() {
        for (offset, value) in [
            (0, 3),
            (8, 0),
            (12, 12),
            (16, 2),
            (16, 4),
            (20, u32::MAX),
            (44, u32::MAX),
            (48, 40),
            (48, 513),
            (48, u32::MAX),
            (76, 516),
        ] {
            let mut bytes = fixture();
            bytes[offset..offset + 4].copy_from_slice(&u32::to_le_bytes(value));
            assert!(
                parse_vhv(&bytes, &Limits::default()).is_err(),
                "offset {offset}, value {value}"
            );
        }
        let bytes = fixture();
        assert_eq!(
            parse_vhv(
                &bytes,
                &Limits {
                    max_input_bytes: 1023,
                    ..Limits::default()
                }
            ),
            Err(VhvError::InputTooLarge)
        );
        assert_eq!(
            parse_vhv(
                &bytes,
                &Limits {
                    max_entries: 1,
                    ..Limits::default()
                }
            ),
            Err(VhvError::TooMany { part: "mesh count" })
        );
        assert_eq!(
            parse_vhv(
                &bytes,
                &Limits {
                    max_entries: 2,
                    ..Limits::default()
                }
            ),
            Err(VhvError::TooMany {
                part: "vertex count"
            })
        );
    }
}
