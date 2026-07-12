//! Source Engine `.phy` collision files — one or more solids, each an
//! IVP compact surface made of convex pieces ("ledges").
//!
//! There is no official specification; the layout was reverse-engineered
//! from legally obtained game content and byte-verified against shipping
//! files — clean-room interoperability work, no leaked source consulted.
//!
//! Two doors, per the crate contract:
//!
//! - [`parse`] is strict: the first anomaly is an error. Use it to
//!   validate well-formed content.
//! - [`parse_lossy`] validates the container header strictly (empty or
//!   headerless input is an error, never an empty success), then
//!   salvages every solid it can, reporting anomalies as located
//!   [`Skip`]s in [`ReadStats`]. Use it on wild workshop content.
//!
//! Detailed skip records are capped by [`Limits::max_stat_records`];
//! the aggregate [`ReadStats::skip_reasons`] counts stay accurate past
//! the cap.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::Limits;
use crate::keyvalues::{self, KvDocument};
use crate::math::sqrt_f32;

const PHY_HEADER_BYTES: usize = 16;
const COMPACT_SURFACE_HEADER_BYTES: usize = 32;
const LEGACY_SURFACE_HEADER_BYTES: usize = 48;
const COMPACT_LEDGE_HEADER_BYTES: usize = 16;
const COMPACT_TRIANGLE_BYTES: usize = 16;
const COMPACT_POINT_BYTES: usize = 16;
const COMPACT_LEDGETREE_NODE_BYTES: usize = 28;
const COMPACT_LEDGETREE_NODE_USED_BYTES: usize = 24;
const LEGACY_LEDGETREE_ROOT_OFFSET: usize = 32;
const COMPACT_LEDGE_IS_COMPACT_FLAG: u32 = 1;
const COMPACT_LEDGE_FLAGS_BITS: u32 = 8;

// Format-specific hardening caps, tuned against real game and workshop
// content (the largest legitimate files sit far below them). These are
// intentionally not part of [`Limits`]: they bound *structures inside
// one file*, where the generic limits bound the input and diagnostics.
const MAX_SOLIDS: usize = 256;
const MAX_LEDGES: usize = 8192;
const MAX_POINTS: usize = 65_536;
const MAX_TRIANGLES: usize = 262_144;
const MAX_TRIANGLES_PER_LEDGE: usize = 8192;
const CONVEXITY_EPSILON: f32 = 0.5;

/// IVP compact-surface positions are meters; Source model space is inches.
///
/// The coordinate conversion used by Source-space methods is
/// `[x, z, -y] * 39.37008`.
pub const IVP_METERS_TO_SOURCE_INCHES: f32 = 39.370_08;

/// Parsed `.phy` file content.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PhyFile<'a> {
    /// File header fields.
    pub header: Header,
    /// Solids that yielded at least one valid convex ledge.
    pub solids: Vec<Solid>,
    /// Raw trailing keyvalues text section, when present (borrowed).
    pub text: Option<TextSection<'a>>,
    /// Read counters and skip/degrade reasons.
    pub stats: ReadStats,
}

/// `.phy` file header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Header {
    /// Declared header size in bytes (offset 0).
    pub declared_size: usize,
    /// Declared solid-section count (offset 8).
    pub solid_count: usize,
    /// Checksum shared with the sibling `.mdl` (offset 12).
    ///
    /// Tools can compare this against the studiohdr checksum to verify a
    /// `.phy` belongs to a given model without relying on file names.
    pub mdl_checksum: u32,
}

/// A parsed solid section.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Solid {
    /// Convex collision ledges recovered from this solid.
    pub ledges: Vec<ConvexLedge>,
}

/// A convex ledge extracted from an IVP compact surface.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ConvexLedge {
    /// Source-space vertices in inches, using `[x, z, -y] * 39.37008`.
    pub vertices: Vec<[f32; 3]>,
    /// Raw IVP vertices in meters, using IVP's original x/right, y/down,
    /// z/forward axes.
    pub ivp_vertices: Vec<[f32; 3]>,
    /// Triangle indices into [`vertices`](Self::vertices) and
    /// [`ivp_vertices`](Self::ivp_vertices).
    pub triangles: Vec<[usize; 3]>,
}

/// Raw trailing keyvalues text section (surface properties, masses,
/// ragdoll constraints), borrowed from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TextSection<'a> {
    /// Unmodified trailing bytes after the declared solid sections.
    pub bytes: &'a [u8],
}

/// Read counters and degraded-section reasons.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadStats {
    /// Number of solids declared in the file header.
    pub declared_solids: usize,
    /// Number of solids that yielded at least one valid convex ledge.
    pub parsed_solids: usize,
    /// Number of convex ledges retained after convexity filtering.
    pub parsed_ledges: usize,
    /// Counted reasons why sections or ledges were skipped. Always
    /// complete, even past the detail cap.
    pub skip_reasons: BTreeMap<SkipReason, usize>,
    /// Skips with location context, in encounter order, capped at
    /// [`Limits::max_stat_records`]. The per-skip byte offsets make
    /// wild-content bug reports diagnosable without the reporter's file.
    pub skips: Vec<Skip>,
}

/// One skipped section or ledge, with location context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Skip {
    /// Why the bytes were not retained.
    pub reason: SkipReason,
    /// Byte offset of the structure the skip was decided at, when known.
    pub byte_offset: Option<usize>,
    /// Index of the solid section being parsed, when the skip happened
    /// inside one.
    pub solid_index: Option<usize>,
}

/// Reason a section or ledge could not be retained.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum SkipReason {
    /// The file declared more solids than the bounded parser will inspect.
    TooManySolids,
    /// A solid section size or range was invalid.
    SectionOutOfRange,
    /// A section did not contain a supported VPHY compact surface.
    UnsupportedSection,
    /// The compact-surface layout could not be interpreted.
    CompactSurfaceInvalid,
    /// A parsed ledge failed the convexity gate.
    NonConvexLedge,
    /// Retaining a solid would exceed parser caps.
    LimitExceeded,
}

/// `.phy` container-level failure. Content-level anomalies are lossy
/// skips (see [`ReadStats`]) except through the strict [`parse`] door.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PhyError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// The input byte slice was empty.
    Empty,
    /// The file header could not be read or contained invalid negative
    /// values.
    BadHeader {
        /// Byte offset of the unreadable field.
        offset: usize,
    },
    /// Strict [`parse`] hit a content anomaly that [`parse_lossy`] would
    /// have skipped.
    Anomaly(Skip),
}

impl fmt::Display for PhyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "phy input is {len} bytes, over the {max}-byte limit")
            }
            Self::Empty => write!(f, "phy input is empty"),
            Self::BadHeader { offset } => {
                write!(f, "phy header unreadable at byte offset {offset:#x}")
            }
            Self::Anomaly(skip) => {
                write!(f, "phy parse failed: {}", skip.reason)?;
                if let Some(offset) = skip.byte_offset {
                    write!(f, " at byte offset {offset:#x}")?;
                }
                if let Some(solid) = skip.solid_index {
                    write!(f, " in solid {solid}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PhyError {}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TooManySolids => "declared solid count exceeds parser cap",
            Self::SectionOutOfRange => "solid section size or range is invalid",
            Self::UnsupportedSection => "section is not a VPHY compact surface",
            Self::CompactSurfaceInvalid => "compact-surface layout could not be interpreted",
            Self::NonConvexLedge => "ledge failed the convexity gate",
            Self::LimitExceeded => "retaining the solid would exceed parser caps",
        };
        f.write_str(message)
    }
}

/// Read a `.phy` file, salvaging malformed content.
///
/// The container header is validated strictly; everything after it is
/// total — content anomalies are reported in [`PhyFile::stats`] instead
/// of returned as errors.
pub fn parse_lossy<'a>(bytes: &'a [u8], limits: &Limits) -> Result<PhyFile<'a>, PhyError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(PhyError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    if bytes.is_empty() {
        return Err(PhyError::Empty);
    }
    let Some(header_size) = read_i32(bytes, 0).and_then(nonnegative_usize) else {
        return Err(PhyError::BadHeader { offset: 0 });
    };
    let Some(solid_count) = read_i32(bytes, 8).and_then(nonnegative_usize) else {
        return Err(PhyError::BadHeader { offset: 8 });
    };
    let Some(mdl_checksum) = read_u32(bytes, 12) else {
        return Err(PhyError::BadHeader { offset: 12 });
    };
    let header = Header {
        declared_size: header_size,
        solid_count,
        mdl_checksum,
    };

    let mut stats = ReadStats {
        declared_solids: solid_count,
        ..ReadStats::default()
    };
    let cap = limits.max_stat_records;
    let mut solids = Vec::new();
    if solid_count > MAX_SOLIDS {
        stats.skip_at(cap, SkipReason::TooManySolids, Some(8), None);
    }

    let mut offset = header_size.max(PHY_HEADER_BYTES);
    let mut completed_declared_solids = solid_count <= MAX_SOLIDS;
    let mut total_ledges = 0_usize;
    let mut total_points = 0_usize;
    let mut total_triangles = 0_usize;

    for solid_index in 0..solid_count.min(MAX_SOLIDS) {
        let Some(section_size) = read_i32(bytes, offset).and_then(nonnegative_usize) else {
            stats.skip_at(
                cap,
                SkipReason::SectionOutOfRange,
                Some(offset),
                Some(solid_index),
            );
            completed_declared_solids = false;
            break;
        };
        let Some(section_end) = offset
            .checked_add(4)
            .and_then(|section_data_start| section_data_start.checked_add(section_size))
        else {
            stats.skip_at(
                cap,
                SkipReason::SectionOutOfRange,
                Some(offset),
                Some(solid_index),
            );
            completed_declared_solids = false;
            break;
        };
        let Some(minimum_section_end) =
            offset.checked_add(COMPACT_SURFACE_HEADER_BYTES + LEGACY_SURFACE_HEADER_BYTES)
        else {
            stats.skip_at(
                cap,
                SkipReason::SectionOutOfRange,
                Some(offset),
                Some(solid_index),
            );
            completed_declared_solids = false;
            break;
        };
        if section_end < minimum_section_end || section_end > bytes.len() {
            stats.skip_at(
                cap,
                SkipReason::SectionOutOfRange,
                Some(offset),
                Some(solid_index),
            );
            completed_declared_solids = false;
            break;
        }
        if bytes.get(offset + 4..offset + 8) != Some(b"VPHY".as_slice()) {
            stats.skip_at(
                cap,
                SkipReason::UnsupportedSection,
                Some(offset),
                Some(solid_index),
            );
            offset = section_end;
            continue;
        }

        let legacy_start = offset + COMPACT_SURFACE_HEADER_BYTES;
        let ledge_start = legacy_start + LEGACY_SURFACE_HEADER_BYTES;
        let Some(parsed_ledges) =
            read_compact_ledgetree(bytes, legacy_start, ledge_start, section_end)
                .or_else(|| read_compact_ledges(bytes, ledge_start, section_end))
                .or_else(|| read_convex_headers(bytes, ledge_start, section_end))
        else {
            stats.skip_at(
                cap,
                SkipReason::CompactSurfaceInvalid,
                Some(offset),
                Some(solid_index),
            );
            offset = section_end;
            continue;
        };

        let mut ledges = Vec::new();
        let mut rejected_nonconvex = 0_usize;
        for ledge in parsed_ledges {
            if ledge.triangles.is_empty() {
                continue;
            }
            if ledge.vertices.len() >= 4 {
                let Some(max_front_distance) = convex_ledge_max_front_distance(&ledge) else {
                    rejected_nonconvex = rejected_nonconvex.saturating_add(1);
                    stats.skip_at(
                        cap,
                        SkipReason::NonConvexLedge,
                        Some(offset),
                        Some(solid_index),
                    );
                    continue;
                };
                if max_front_distance > CONVEXITY_EPSILON {
                    rejected_nonconvex = rejected_nonconvex.saturating_add(1);
                    stats.skip_at(
                        cap,
                        SkipReason::NonConvexLedge,
                        Some(offset),
                        Some(solid_index),
                    );
                    continue;
                }
            }
            ledges.push(ledge);
        }

        let added = ledges.len();
        let added_points = ledges
            .iter()
            .map(|ledge| ledge.vertices.len())
            .sum::<usize>();
        let added_triangles = ledges
            .iter()
            .map(|ledge| ledge.triangles.len())
            .sum::<usize>();
        let exceeds_limits = total_ledges.saturating_add(added) > MAX_LEDGES
            || total_points.saturating_add(added_points) > MAX_POINTS
            || total_triangles.saturating_add(added_triangles) > MAX_TRIANGLES;
        if exceeds_limits {
            stats.skip_at(
                cap,
                SkipReason::LimitExceeded,
                Some(offset),
                Some(solid_index),
            );
        } else if added > 0 {
            total_ledges = total_ledges.saturating_add(added);
            total_points = total_points.saturating_add(added_points);
            total_triangles = total_triangles.saturating_add(added_triangles);
            stats.parsed_solids = stats.parsed_solids.saturating_add(1);
            stats.parsed_ledges = stats.parsed_ledges.saturating_add(added);
            solids.push(Solid { ledges });
        } else if rejected_nonconvex == 0 {
            stats.skip_at(
                cap,
                SkipReason::CompactSurfaceInvalid,
                Some(offset),
                Some(solid_index),
            );
        }

        offset = section_end;
    }

    let text = completed_declared_solids
        .then(|| TextSection::from_tail(bytes, offset))
        .flatten();
    Ok(PhyFile {
        header,
        solids,
        text,
        stats,
    })
}

/// Read a `.phy` file, treating any anomaly as an error.
///
/// This is the strict door for validation tooling: where [`parse_lossy`]
/// salvages everything it can, `parse` fails on the FIRST anomaly with
/// its reason and location. The successfully parsed remainder is
/// discarded — use [`parse_lossy`] when partial output is wanted.
pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<PhyFile<'a>, PhyError> {
    let file = parse_lossy(bytes, limits)?;
    if file.stats.total_skips() == 0 {
        return Ok(file);
    }
    // The detail vector is capped by `max_stat_records` and can be empty
    // even when anomalies occurred (a cap of 0 disables detail entirely);
    // the counters are never capped, so they are what strictness keys on.
    let anomaly = file.stats.skips.first().copied().unwrap_or_else(|| Skip {
        reason: *file
            .stats
            .skip_reasons
            .keys()
            .next()
            .expect("total_skips > 0 implies a recorded reason"),
        byte_offset: None,
        solid_index: None,
    });
    Err(PhyError::Anomaly(anomaly))
}

/// Converts one raw IVP compact-surface point to Source model space.
///
/// IVP stores x/right, y/down, z/forward in meters. Source model space
/// stores x/right, y/forward, z/up in inches.
#[must_use]
pub fn ivp_to_source(point: [f32; 3]) -> [f32; 3] {
    [
        point[0] * IVP_METERS_TO_SOURCE_INCHES,
        point[2] * IVP_METERS_TO_SOURCE_INCHES,
        -point[1] * IVP_METERS_TO_SOURCE_INCHES,
    ]
}

impl PhyFile<'_> {
    /// Returns all retained ledges across all parsed solids.
    pub fn ledges(&self) -> impl Iterator<Item = &ConvexLedge> {
        self.solids.iter().flat_map(|solid| solid.ledges.iter())
    }
}

impl<'a> TextSection<'a> {
    /// Views the text section as UTF-8 with trailing NULs trimmed.
    ///
    /// The section is NUL-terminated in most files; [`bytes`](Self::bytes)
    /// keeps the terminator(s), this view drops them. Returns [`None`]
    /// when the section is not valid UTF-8.
    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        let end = self
            .bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |index| index + 1);
        std::str::from_utf8(&self.bytes[..end]).ok()
    }

    /// Parses the text section as KeyValues (surface properties, masses,
    /// ragdoll constraints). `None` when the section is not valid UTF-8
    /// or fails KeyValues limits; call [`crate::keyvalues::parse`] on
    /// [`as_str`](Self::as_str) directly for error detail.
    #[must_use]
    pub fn keyvalues(&self, limits: &Limits) -> Option<KvDocument<'a>> {
        keyvalues::parse(self.as_str()?, limits).ok()
    }

    fn from_tail(bytes: &'a [u8], offset: usize) -> Option<Self> {
        let tail = bytes.get(offset..)?;
        (!tail.is_empty()).then_some(Self { bytes: tail })
    }
}

impl ReadStats {
    /// Total number of skips, counted even past the detail cap.
    #[must_use]
    pub fn total_skips(&self) -> usize {
        self.skip_reasons.values().sum()
    }

    fn skip_at(
        &mut self,
        cap: usize,
        reason: SkipReason,
        byte_offset: Option<usize>,
        solid_index: Option<usize>,
    ) {
        *self.skip_reasons.entry(reason).or_default() += 1;
        if self.skips.len() < cap {
            self.skips.push(Skip {
                reason,
                byte_offset,
                solid_index,
            });
        }
    }
}

fn read_compact_ledgetree(
    bytes: &[u8],
    legacy_start: usize,
    ledge_start: usize,
    section_end: usize,
) -> Option<Vec<ConvexLedge>> {
    let root_offset =
        read_i32(bytes, legacy_start + LEGACY_LEDGETREE_ROOT_OFFSET).and_then(nonnegative_usize)?;
    if root_offset == 0 {
        return None;
    }
    let root = legacy_start.checked_add(root_offset)?;
    if root <= ledge_start || root > section_end {
        return None;
    }

    let mut state = CompactLedgetreeState::default();
    collect_compact_ledgetree_terminals(bytes, root, root, section_end, 0, &mut state)?;

    (!state.ledges.is_empty()).then_some(state.ledges)
}

#[derive(Debug, Default)]
struct CompactLedgetreeState {
    nodes: BTreeSet<usize>,
    ledge_offsets: BTreeSet<usize>,
    ledges: Vec<ConvexLedge>,
}

/// Depth-first (left before right, matching the recursive original —
/// ledge output order is part of the parse result) over an explicit
/// heap stack: a degenerate chain of `MAX_LEDGES` nodes must not
/// become call-stack depth.
fn collect_compact_ledgetree_terminals(
    bytes: &[u8],
    root: usize,
    tree_root: usize,
    section_end: usize,
    root_depth: usize,
    state: &mut CompactLedgetreeState,
) -> Option<()> {
    let mut pending = vec![(root, root_depth)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_LEDGES || state.nodes.len() > MAX_LEDGES.saturating_mul(2).saturating_add(1)
        {
            return None;
        }
        if node < tree_root || node.checked_add(COMPACT_LEDGETREE_NODE_USED_BYTES)? > section_end {
            return None;
        }
        if !state.nodes.insert(node) {
            return None;
        }

        let right_node_offset = read_i32(bytes, node)?;
        let compact_ledge_offset = read_i32(bytes, node + 4)?;
        if right_node_offset == 0 {
            let ledge_offset = checked_add_i32(node, compact_ledge_offset)?;
            if !state.ledge_offsets.insert(ledge_offset) {
                return None;
            }
            let ledge = compact_ledge_at(bytes, ledge_offset, tree_root)?.0;
            if !ledge.vertices.is_empty() && !ledge.triangles.is_empty() {
                state.ledges.push(ledge);
            }
            continue;
        }

        let right_node_offset = nonnegative_usize(right_node_offset)?;
        if right_node_offset < COMPACT_LEDGETREE_NODE_BYTES {
            return None;
        }
        let left_node = node.checked_add(COMPACT_LEDGETREE_NODE_BYTES)?;
        let right_node = node.checked_add(right_node_offset)?;
        // LIFO: push right first so the left subtree is walked first.
        pending.push((right_node, depth + 1));
        pending.push((left_node, depth + 1));
    }
    Some(())
}

fn read_compact_ledges(
    bytes: &[u8],
    ledge_start: usize,
    section_end: usize,
) -> Option<Vec<ConvexLedge>> {
    let first_header = read_compact_ledge_header(bytes, ledge_start)?;
    let first_point_start = ledge_start.checked_add(first_header.point_offset)?;
    if first_point_start <= ledge_start || first_point_start > section_end {
        return None;
    }

    let mut ledges = Vec::new();
    let mut cursor = ledge_start;
    let mut parsed = 0_usize;
    while cursor < first_point_start {
        let (ledge, triangle_end, point_start) = compact_ledge_at(bytes, cursor, section_end)?;
        if point_start < first_point_start || point_start > section_end {
            return None;
        }
        if triangle_end > first_point_start {
            return None;
        }
        if !ledge.vertices.is_empty() && !ledge.triangles.is_empty() {
            ledges.push(ledge);
        }
        cursor = triangle_end;
        parsed += 1;
        if parsed > MAX_LEDGES {
            return None;
        }
    }

    (cursor == first_point_start && parsed > 0).then_some(ledges)
}

fn compact_ledge_at(
    bytes: &[u8],
    ledge_offset: usize,
    vertex_end: usize,
) -> Option<(ConvexLedge, usize, usize)> {
    let header = read_compact_ledge_header(bytes, ledge_offset)?;
    if header.compact_flag != COMPACT_LEDGE_IS_COMPACT_FLAG {
        return None;
    }
    let point_start = ledge_offset.checked_add(header.point_offset)?;
    if point_start <= ledge_offset || point_start > vertex_end {
        return None;
    }
    let tri_count = header.triangle_count;
    if tri_count == 0 || tri_count > MAX_TRIANGLES_PER_LEDGE {
        return None;
    }
    let triangle_start = ledge_offset.checked_add(COMPACT_LEDGE_HEADER_BYTES)?;
    let triangle_end =
        triangle_start.checked_add(tri_count.checked_mul(COMPACT_TRIANGLE_BYTES)?)?;
    if triangle_end > point_start {
        return None;
    }
    let ledge = ledge_from_triangles(bytes, triangle_start, tri_count, point_start, vertex_end)?;
    Some((ledge, triangle_end, point_start))
}

fn read_convex_headers(
    bytes: &[u8],
    convex_start: usize,
    section_end: usize,
) -> Option<Vec<ConvexLedge>> {
    let vertices_offset = read_i32(bytes, convex_start).and_then(nonnegative_usize)?;
    let vertices_start = convex_start.checked_add(vertices_offset)?;
    if vertices_start > section_end || vertices_start <= convex_start {
        return None;
    }

    let mut convex_count = 0_usize;
    let mut triangle_count = 0_usize;
    loop {
        let header_end = convex_start
            .checked_add((convex_count + 1).checked_mul(COMPACT_LEDGE_HEADER_BYTES)?)?;
        if header_end > vertices_start {
            return None;
        }
        let tri_count = read_i32(
            bytes,
            convex_start + convex_count * COMPACT_LEDGE_HEADER_BYTES + 12,
        )
        .and_then(nonnegative_usize)?;
        if tri_count > MAX_TRIANGLES_PER_LEDGE {
            return None;
        }
        triangle_count = triangle_count.checked_add(tri_count)?;
        convex_count += 1;
        let triangle_start =
            convex_start.checked_add(convex_count.checked_mul(COMPACT_LEDGE_HEADER_BYTES)?)?;
        let triangle_end =
            triangle_start.checked_add(triangle_count.checked_mul(COMPACT_TRIANGLE_BYTES)?)?;
        if triangle_end == vertices_start {
            break;
        }
        if triangle_end > vertices_start || convex_count > MAX_LEDGES {
            return None;
        }
    }

    let mut ledges = Vec::new();
    let mut triangle_cursor =
        convex_start.checked_add(convex_count.checked_mul(COMPACT_LEDGE_HEADER_BYTES)?)?;
    for convex_index in 0..convex_count {
        let header =
            convex_start.checked_add(convex_index.checked_mul(COMPACT_LEDGE_HEADER_BYTES)?)?;
        let tri_count = read_i32(bytes, header + 12).and_then(nonnegative_usize)?;
        let ledge = ledge_from_triangles(
            bytes,
            triangle_cursor,
            tri_count,
            vertices_start,
            section_end,
        )?;
        if !ledge.vertices.is_empty() && !ledge.triangles.is_empty() {
            ledges.push(ledge);
        }
        triangle_cursor =
            triangle_cursor.checked_add(tri_count.checked_mul(COMPACT_TRIANGLE_BYTES)?)?;
    }
    Some(ledges)
}

#[derive(Debug, Clone, Copy)]
struct CompactLedgeHeader {
    point_offset: usize,
    compact_flag: u32,
    triangle_count: usize,
}

fn read_compact_ledge_header(bytes: &[u8], offset: usize) -> Option<CompactLedgeHeader> {
    let point_offset = read_i32(bytes, offset).and_then(nonnegative_usize)?;
    let flags = read_u32(bytes, offset + 8)?;
    let compact_flag = (flags >> 2) & 0x3;
    let size_div_16 = flags >> COMPACT_LEDGE_FLAGS_BITS;
    let size_bytes = usize::try_from(size_div_16).ok()?.checked_mul(16)?;
    let triangle_count = read_i16(bytes, offset + 12)?;
    if triangle_count < 0 {
        return None;
    }
    let triangle_count = usize::try_from(triangle_count).ok()?;
    let triangle_bytes = triangle_count.checked_mul(COMPACT_TRIANGLE_BYTES)?;
    let minimum_size = COMPACT_LEDGE_HEADER_BYTES.checked_add(triangle_bytes)?;
    if size_bytes < minimum_size {
        return None;
    }
    Some(CompactLedgeHeader {
        point_offset,
        compact_flag,
        triangle_count,
    })
}

fn ledge_from_triangles(
    bytes: &[u8],
    triangle_start: usize,
    tri_count: usize,
    point_start: usize,
    section_end: usize,
) -> Option<ConvexLedge> {
    let mut raw_triangles = Vec::<[usize; 3]>::with_capacity(tri_count);
    let mut referenced = BTreeSet::<usize>::new();
    let mut cursor = triangle_start;
    for _ in 0..tri_count {
        let mut indices = [0_usize; 3];
        for (slot, index_offset) in [4_usize, 8, 12].into_iter().enumerate() {
            let index = read_i16(bytes, cursor + index_offset)?;
            if index < 0 {
                return None;
            }
            let index = usize::try_from(index).ok()?;
            indices[slot] = index;
            referenced.insert(index);
        }
        raw_triangles.push(indices);
        cursor = cursor.checked_add(COMPACT_TRIANGLE_BYTES)?;
    }

    let mut index_remap = BTreeMap::<usize, usize>::new();
    let mut vertices = Vec::with_capacity(referenced.len());
    let mut ivp_vertices = Vec::with_capacity(referenced.len());
    for raw_index in referenced {
        let vertex_offset = point_start.checked_add(raw_index.checked_mul(COMPACT_POINT_BYTES)?)?;
        if vertex_offset.checked_add(12)? > section_end {
            return None;
        }
        let ivp_vertex = [
            read_f32(bytes, vertex_offset)?,
            read_f32(bytes, vertex_offset + 4)?,
            read_f32(bytes, vertex_offset + 8)?,
        ];
        let vertex = ivp_to_source(ivp_vertex);
        if !ivp_vertex.iter().all(|value| value.is_finite())
            || !vertex.iter().all(|value| value.is_finite())
        {
            return None;
        }
        index_remap.insert(raw_index, vertices.len());
        vertices.push(vertex);
        ivp_vertices.push(ivp_vertex);
    }

    let triangles = raw_triangles
        .into_iter()
        .filter_map(|triangle| {
            Some([
                *index_remap.get(&triangle[0])?,
                *index_remap.get(&triangle[1])?,
                *index_remap.get(&triangle[2])?,
            ])
        })
        .collect::<Vec<_>>();

    Some(ConvexLedge {
        vertices,
        ivp_vertices,
        triangles,
    })
}

fn convex_ledge_max_front_distance(ledge: &ConvexLedge) -> Option<f32> {
    if ledge.vertices.len() < 4 || ledge.triangles.is_empty() {
        return None;
    }
    let centroid = mul(
        ledge.vertices.iter().copied().fold([0.0; 3], add),
        1.0 / ledge.vertices.len() as f32,
    );
    let mut worst = f32::NEG_INFINITY;
    for triangle in &ledge.triangles {
        let vertices = [
            *ledge.vertices.get(triangle[0])?,
            *ledge.vertices.get(triangle[1])?,
            *ledge.vertices.get(triangle[2])?,
        ];
        let mut normal = normalize(cross(
            sub(vertices[1], vertices[0]),
            sub(vertices[2], vertices[0]),
        ));
        if !vector_is_finite_nonzero(normal) {
            continue;
        }
        let mut dist = dot(vertices[0], normal);
        if dot(centroid, normal) - dist > 0.0 {
            normal = mul(normal, -1.0);
            dist = -dist;
        }
        for vertex in &ledge.vertices {
            worst = worst.max(dot(*vertex, normal) - dist);
        }
    }
    worst.is_finite().then_some(worst)
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn mul(vector: [f32; 3], scalar: f32) -> [f32; 3] {
    [vector[0] * scalar, vector[1] * scalar, vector[2] * scalar]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let len = sqrt_f32(dot(vector, vector));
    if len <= f32::EPSILON || !len.is_finite() {
        return [0.0; 3];
    }
    mul(vector, 1.0 / len)
}

fn vector_is_finite_nonzero(vector: [f32; 3]) -> bool {
    vector.iter().all(|value| value.is_finite()) && dot(vector, vector) > 1.0e-12
}

fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    Some(i16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn nonnegative_usize(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

fn checked_add_i32(base: usize, offset: i32) -> Option<usize> {
    if offset >= 0 {
        base.checked_add(usize::try_from(offset).ok()?)
    } else {
        base.checked_sub(usize::try_from(offset.unsigned_abs()).ok()?)
    }
}
