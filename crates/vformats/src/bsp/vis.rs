//! The visibility lump (`LUMP_VISIBILITY`): a cluster directory over
//! run-length-encoded potentially-visible and potentially-audible
//! sets. Rows decompress on demand; the RLE (a zero byte followed by
//! a zero-byte run length) is tolerant of truncation — missing tail
//! clusters stay invisible, matching the engine's reader.

use std::borrow::Cow;

use super::{Bsp, BspError, lump_ids};
use crate::Limits;

/// The parsed visibility directory (`dvis_t`) over the lump's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visibility<'a> {
    cluster_count: usize,
    /// Per-cluster `[pvs, pas]` byte offsets, relative to lump start.
    offsets: Vec<[u32; 2]>,
    data: Cow<'a, [u8]>,
}

impl<'a> Bsp<'a> {
    /// The visibility lump; `None` when the map has none (fullbright
    /// or unvised maps). Decompresses an LZMA-compressed lump (`lzma`
    /// feature).
    pub fn visibility(&self, limits: &Limits) -> Result<Option<Visibility<'a>>, BspError> {
        let malformed = BspError::Decode { part: "visibility" };
        let data = self.lump_data(lump_ids::VISIBILITY, limits)?;
        if data.is_empty() {
            return Ok(None);
        }
        if data.len() < 4 {
            return Err(malformed);
        }
        let cluster_count = i32::from_le_bytes(data[0..4].try_into().expect("4 bytes"));
        let cluster_count = usize::try_from(cluster_count).map_err(|_| malformed.clone())?;
        if cluster_count > limits.max_entries {
            return Err(BspError::TooManyRecords {
                part: "visibility clusters",
                max: limits.max_entries,
            });
        }
        let directory = data.get(4..4 + cluster_count * 8).ok_or(malformed)?;
        let offsets = directory
            .chunks_exact(8)
            .map(|pair| {
                [
                    u32::from_le_bytes(pair[0..4].try_into().expect("4 bytes")),
                    u32::from_le_bytes(pair[4..8].try_into().expect("4 bytes")),
                ]
            })
            .collect();
        Ok(Some(Visibility {
            cluster_count,
            offsets,
            data,
        }))
    }
}

impl Visibility<'_> {
    /// Detach from the source lump bytes, cloning the payload if it is
    /// still borrowed. Callers that hold the parsed [`super::Bsp`] only
    /// for the duration of a decode step (and want to keep the
    /// visibility data past that) use this to drop the borrow.
    #[must_use]
    pub fn into_owned(self) -> Visibility<'static> {
        Visibility {
            cluster_count: self.cluster_count,
            offsets: self.offsets,
            data: Cow::Owned(self.data.into_owned()),
        }
    }

    /// How many visibility clusters the map has.
    #[must_use]
    pub fn cluster_count(&self) -> usize {
        self.cluster_count
    }

    /// The visibility lump's decompressed byte length (cluster
    /// directory plus RLE row payload, which is retained on `Self`) —
    /// what callers reporting a memory footprint should count.
    #[must_use]
    pub fn lump_len(&self) -> usize {
        self.data.len()
    }

    /// The potentially visible set of `cluster`: one flag per cluster.
    /// `None` when the cluster index or its row offset is out of
    /// range.
    #[must_use]
    pub fn pvs(&self, cluster: usize) -> Option<Vec<bool>> {
        self.row(cluster, 0)
    }

    /// The potentially audible set of `cluster` (see [`pvs`](Self::pvs)).
    #[must_use]
    pub fn pas(&self, cluster: usize) -> Option<Vec<bool>> {
        self.row(cluster, 1)
    }

    fn row(&self, cluster: usize, which: usize) -> Option<Vec<bool>> {
        let offset = usize::try_from(self.offsets.get(cluster)?[which]).ok()?;
        let row = self.data.get(offset..)?;
        let mut visible = vec![false; self.cluster_count];
        let mut at = 0;
        let mut bit = 0;
        while bit < self.cluster_count {
            let Some(packed) = row.get(at) else {
                break; // Truncated row: the tail stays invisible.
            };
            at += 1;
            if *packed == 0 {
                let Some(run) = row.get(at) else {
                    break;
                };
                at += 1;
                if *run == 0 {
                    break; // A zero run length would never advance.
                }
                bit += usize::from(*run) * 8;
            } else {
                for (index, flag) in visible.iter_mut().skip(bit).take(8).enumerate() {
                    if packed & (1 << index) != 0 {
                        *flag = true;
                    }
                }
                bit += 8;
            }
        }
        Some(visible)
    }
}
