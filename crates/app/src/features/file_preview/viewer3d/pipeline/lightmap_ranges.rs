use super::ModelVertex;
use gmpublished_domain::scene::map::LightmapPageId;
use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PageRange {
    pub indices: Range<u32>,
    pub page: Option<LightmapPageId>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, thiserror::Error)]
pub(super) enum RangeError {
    #[error("lightmap geometry has {count} indices, which is not a triangle list")]
    TriangleCount { count: usize },
    #[error("lightmap triangle {triangle} references missing vertex {vertex}")]
    Vertex { triangle: usize, vertex: u32 },
    #[error("lightmap triangle {triangle} crosses page assignments {pages:?}")]
    Mixed { triangle: usize, pages: [u32; 3] },
    #[error("lightmap geometry exceeds the indexed draw limit")]
    DrawLimit,
}

#[derive(Debug)]
struct VertexRange {
    end: u32,
    page: Option<LightmapPageId>,
}

#[derive(Debug, Default)]
pub(super) struct Ranges {
    vertices: Vec<VertexRange>,
    pub indexed: Vec<PageRange>,
}

impl Ranges {
    pub fn new(vertices: &[ModelVertex], indices: &[u32]) -> Result<Self, RangeError> {
        let mut ranges = Self::default();
        for (index, vertex) in vertices.iter().enumerate() {
            let end = u32::try_from(index + 1).map_err(|_| RangeError::DrawLimit)?;
            if let Some(last) = ranges.vertices.last_mut()
                && last.page == vertex.lightmap_page_id()
            {
                last.end = end;
            } else {
                ranges.vertices.push(VertexRange {
                    end,
                    page: vertex.lightmap_page_id(),
                });
            }
        }
        ranges.indexed = ranges.select(indices)?;
        Ok(ranges)
    }

    pub fn expanded(vertices: &[ModelVertex]) -> Result<Vec<PageRange>, RangeError> {
        let indices = (0..vertices.len())
            .map(|index| u32::try_from(index).map_err(|_| RangeError::DrawLimit))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(vertices, &indices)?.indexed)
    }

    fn vertex_page(
        &self,
        index: u32,
        triangle: usize,
    ) -> Result<Option<LightmapPageId>, RangeError> {
        let range = self.vertices.partition_point(|range| range.end <= index);
        self.vertices
            .get(range)
            .map(|range| range.page)
            .ok_or(RangeError::Vertex {
                triangle,
                vertex: index,
            })
    }

    pub fn select(&self, indices: &[u32]) -> Result<Vec<PageRange>, RangeError> {
        if !indices.len().is_multiple_of(3) {
            return Err(RangeError::TriangleCount {
                count: indices.len(),
            });
        }
        let mut output: Vec<PageRange> = Vec::new();
        for (triangle, vertices) in indices.chunks_exact(3).enumerate() {
            let pages = [
                self.vertex_page(vertices[0], triangle)?,
                self.vertex_page(vertices[1], triangle)?,
                self.vertex_page(vertices[2], triangle)?,
            ];
            if pages[1] != pages[0] || pages[2] != pages[0] {
                return Err(RangeError::Mixed {
                    triangle,
                    pages: pages.map(|page| page.map_or(0, |page| page.0 + 1)),
                });
            }
            let start = u32::try_from(triangle * 3).map_err(|_| RangeError::DrawLimit)?;
            let end = start.checked_add(3).ok_or(RangeError::DrawLimit)?;
            let page = pages[0];
            if let Some(last) = output.last_mut()
                && last.page == page
            {
                last.indices.end = end;
            } else {
                output.push(PageRange {
                    indices: start..end,
                    page,
                });
            }
        }
        Ok(output)
    }

    pub fn intersect(&self, indices: Range<u32>) -> impl Iterator<Item = PageRange> + '_ {
        intersections(&self.indexed, indices)
    }
}

pub(super) fn intersections(
    ranges: &[PageRange],
    indices: Range<u32>,
) -> impl Iterator<Item = PageRange> + '_ {
    let first = ranges.partition_point(|range| range.indices.end <= indices.start);
    ranges[first..]
        .iter()
        .take_while(move |range| range.indices.start < indices.end)
        .map(move |range| PageRange {
            indices: range.indices.start.max(indices.start)..range.indices.end.min(indices.end),
            page: range.page,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertices() -> Vec<ModelVertex> {
        [1, 1, 1, 2, 2, 2, 0, 0, 0]
            .into_iter()
            .map(|page| ModelVertex {
                lightmap_page: page,
                ..bytemuck::Zeroable::zeroed()
            })
            .collect()
    }

    #[test]
    fn visibility_reordering_preserves_page_runs_and_unlit_assignment() {
        let ranges = Ranges::new(&vertices(), &[0, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(ranges.indexed.len(), 3);
        let selected = ranges
            .select(&[3, 4, 5, 3, 5, 4, 0, 1, 2, 6, 7, 8])
            .unwrap();
        assert_eq!(
            selected,
            [
                PageRange {
                    indices: 0..6,
                    page: Some(LightmapPageId(1))
                },
                PageRange {
                    indices: 6..9,
                    page: Some(LightmapPageId(0))
                },
                PageRange {
                    indices: 9..12,
                    page: None
                }
            ]
        );
        let clipped: Vec<_> = ranges.intersect(3..9).collect();
        assert_eq!(clipped, ranges.indexed[1..]);
    }

    #[test]
    fn malformed_or_mixed_triangles_are_rejected() {
        let vertices = vertices();
        assert!(matches!(
            Ranges::new(&vertices, &[0, 1]),
            Err(RangeError::TriangleCount { .. })
        ));
        assert!(matches!(
            Ranges::new(&vertices, &[0, 1, 3]),
            Err(RangeError::Mixed { .. })
        ));
        assert!(matches!(
            Ranges::new(&vertices, &[0, 1, 99]),
            Err(RangeError::Vertex { .. })
        ));
    }
}
