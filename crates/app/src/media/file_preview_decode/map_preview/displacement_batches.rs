use crate::media::preview_model::MapDisplacementBatch;
use gmpublished_domain::scene::map::{
    MapDisplacementRendering, MapVisibility, SourceLightmapAllocation, SourceLightmapLayout,
};
use std::collections::HashMap;

const SOURCE_BATCH_VERTEX_LIMIT: u32 = 8192;
const SOURCE_BATCH_INDEX_LIMIT: u32 = 8192;

pub(super) fn prepare(
    visibility: &MapVisibility,
    layout: &SourceLightmapLayout,
) -> HashMap<u32, MapDisplacementBatch> {
    assign(
        visibility
            .displacement_surfaces()
            .filter_map(|(face, geometry)| {
                let Some(&allocation) = layout.surfaces.get(face as usize) else {
                    log::warn!(
                        "displacement face {face}: Source lightmap allocation is unavailable"
                    );
                    return None;
                };
                Some((face, geometry, allocation))
            }),
    )
}

fn assign(
    records: impl IntoIterator<Item = (u32, MapDisplacementRendering, SourceLightmapAllocation)>,
) -> HashMap<u32, MapDisplacementBatch> {
    let mut records = records.into_iter().collect::<Vec<_>>();
    records.sort_by_key(|(_, geometry, _)| geometry.ordinal);
    let mut buckets = HashMap::new();
    let mut batches = Vec::<(usize, u32, u32)>::new();
    let mut result = HashMap::new();
    for (face, geometry, allocation) in records {
        let index = *buckets
            .entry((allocation.material_group, allocation.page))
            .or_insert_with(|| {
                batches.push((0, 0, 0));
                batches.len() - 1
            });
        let (batch, vertices, indices) = &mut batches[index];
        if *vertices != 0
            && (vertices.saturating_add(geometry.full_grid_vertices) > SOURCE_BATCH_VERTEX_LIMIT
                || indices.saturating_add(geometry.full_grid_indices) > SOURCE_BATCH_INDEX_LIMIT)
        {
            *batch += 1;
            *vertices = 0;
            *indices = 0;
        }
        *vertices = vertices.saturating_add(geometry.full_grid_vertices);
        *indices = indices.saturating_add(geometry.full_grid_indices);
        result.insert(
            face,
            MapDisplacementBatch {
                material_bucket: index,
                mesh_batch: *batch,
            },
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::SourceLightmapPage;

    fn record(
        face: u32,
        ordinal: u32,
        material: usize,
        page: SourceLightmapPage,
        power: u32,
    ) -> (u32, MapDisplacementRendering, SourceLightmapAllocation) {
        let side = 1_u32 << power;
        (
            face,
            MapDisplacementRendering {
                ordinal,
                full_grid_vertices: (side + 1).pow(2),
                full_grid_indices: 6 * side.pow(2),
            },
            SourceLightmapAllocation {
                material_group: material,
                page,
                offset: [0; 2],
            },
        )
    }

    #[test]
    fn registry_uses_first_authored_displacement_and_material_page_equality() {
        let result = assign([
            record(10, 2, 4, SourceLightmapPage::White, 2),
            record(11, 0, 2, SourceLightmapPage::Physical(1), 2),
            record(12, 1, 4, SourceLightmapPage::White, 2),
            record(13, 3, 4, SourceLightmapPage::WhiteBump, 2),
        ]);
        assert_eq!(result[&11].material_bucket, 0);
        assert_eq!(result[&12].material_bucket, 1);
        assert_eq!(result[&10], result[&12]);
        assert_eq!(result[&13].material_bucket, 2);
    }

    #[test]
    fn full_grid_index_cost_sets_batches_before_live_tessellation() {
        let result = assign(
            (0..6).map(|ordinal| record(ordinal, ordinal, 0, SourceLightmapPage::Physical(0), 4)),
        );
        assert_eq!(result[&4].mesh_batch, 0);
        assert_eq!(result[&5].mesh_batch, 1);
    }
}
