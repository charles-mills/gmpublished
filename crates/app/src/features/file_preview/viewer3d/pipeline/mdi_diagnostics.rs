use super::DrawItem;
use super::lightmap_ranges::PageRange;
use super::reflections::SceneView;
use super::surface_pipelines::SurfaceBlend;
use super::transparency::{Draw, Primitive, WorldDrawGroups};
use super::visibility::UploadedVisibility;
use super::{DrawPlan, ViewerScene, wgpu};
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Default)]
struct RunHistogram {
    surface_batches: u64,
    total_parts: u64,
    run_count: u64,
    lengths: BTreeMap<u64, u64>,
}

impl RunHistogram {
    fn observe_batch<K: Eq>(&mut self, parts: impl IntoIterator<Item = K>) {
        self.surface_batches += 1;
        let mut previous = None;
        let mut length = 0;
        for key in parts {
            if previous.as_ref().is_some_and(|previous| *previous != key) {
                self.finish_run(length);
                length = 0;
            }
            previous = Some(key);
            length += 1;
            self.total_parts += 1;
        }
        if length != 0 {
            self.finish_run(length);
        }
    }

    fn finish_run(&mut self, length: u64) {
        self.run_count += 1;
        *self.lengths.entry(length).or_default() += 1;
    }

    fn percentile_95(&self) -> Option<u64> {
        let target = (self.run_count * 95).div_ceil(100);
        let mut cumulative = 0;
        self.lengths.iter().find_map(|(&length, &count)| {
            cumulative += count;
            (cumulative >= target).then_some(length)
        })
    }

    fn json(&self) -> serde_json::Value {
        let stride = std::mem::size_of::<wgpu::util::DrawIndexedIndirectArgs>() as u64;
        serde_json::json!({
            "surface_batches": self.surface_batches,
            "total_parts": self.total_parts,
            "run_count": self.run_count,
            "singleton_runs": self.lengths.get(&1).copied().unwrap_or(0),
            "multi_part_runs": self.run_count - self.lengths.get(&1).copied().unwrap_or(0),
            "max_run_parts": self.lengths.last_key_value().map(|(&length, _)| length),
            "p95_run_parts": self.percentile_95(),
            "indirect_argument_stride_bytes": stride,
            "indirect_arguments_bytes": self.total_parts * stride,
            "multi_part_indirect_arguments_bytes": (self.total_parts - self.lengths.get(&1).copied().unwrap_or(0)) * stride,
            "draw_calls_saved_upper_bound": self.total_parts - self.run_count,
            "run_length_histogram": self.lengths.iter().map(|(&parts, &runs)| {
                serde_json::json!({"parts": parts, "runs": runs})
            }).collect::<Vec<_>>(),
        })
    }
}

fn collect_plan(plan: &DrawPlan, upload: &ViewerScene, groups: WorldDrawGroups) -> RunHistogram {
    let mut histogram = RunHistogram::default();
    for batch in &plan.world_opaque.batches {
        if groups.allows(batch.material_group) {
            histogram.observe_batch(batch.surfaces.meshes.iter().flat_map(|mesh| {
                upload.meshes[mesh.item.mesh_index]
                    .lightmaps
                    .intersect(mesh.indices.clone())
                    .map(move |part| (mesh.item, part.page))
            }));
        }
    }
    histogram
}

// Collect only emitted indexed parts; non-mesh operations and unavailable
// resources terminate a segment just as they reset draw_operations binding state.
fn transparent_histogram(
    operations: &[Draw],
    mut resolve: impl FnMut(DrawItem, Option<&Range<u32>>) -> Option<(bool, Vec<PageRange>)>,
) -> RunHistogram {
    let mut histogram = RunHistogram::default();
    let mut parts = Vec::new();
    let mut transfer = None;
    let flush = |histogram: &mut RunHistogram, parts: &mut Vec<_>| {
        if !parts.is_empty() {
            histogram.observe_batch(parts.drain(..));
        }
    };
    for operation in operations {
        let next_transfer = SurfaceBlend::from(operation.blend).color_transfer();
        if transfer.is_some_and(|previous| previous != next_transfer) {
            flush(&mut histogram, &mut parts);
        }
        transfer = Some(next_transfer);
        let Primitive::Mesh(item, range) = &operation.primitive else {
            flush(&mut histogram, &mut parts);
            continue;
        };
        let Some((visible, ranges)) = resolve(*item, range.as_ref()) else {
            flush(&mut histogram, &mut parts);
            continue;
        };
        // An empty operation can still replace pipeline/material/index bindings.
        if ranges.is_empty() {
            flush(&mut histogram, &mut parts);
        }
        parts.extend(
            ranges
                .into_iter()
                .map(|part| (*item, operation.blend, visible, part.page)),
        );
    }
    flush(&mut histogram, &mut parts);
    histogram
}

fn collect_transparency(
    operations: &[Draw],
    visibility: &UploadedVisibility,
    upload: &ViewerScene,
) -> serde_json::Value {
    let histogram = transparent_histogram(operations, |item, range| {
        let mesh = &upload.meshes[item.mesh_index];
        // Missing implicit materials/visible buffers emit no draw; explicit
        // ranges use original indices regardless of any visibility entry.
        upload.bindings.materials.get(item.material_slot)?;
        if let Some(range) = range {
            Some((false, mesh.lightmaps.intersect(range.clone()).collect()))
        } else if let Some(visible) = visibility.mesh_indices.get(&item.mesh_index) {
            visible.buffer.as_ref()?;
            Some((true, visible.lightmaps.clone()))
        } else {
            Some((false, mesh.lightmaps.indexed.clone()))
        }
    });
    let mut json = histogram.json();
    let segments = json
        .as_object_mut()
        .unwrap()
        .remove("surface_batches")
        .unwrap();
    json["mesh_segments"] = segments;
    json
}

pub(super) fn collect(view: &SceneView, upload: &ViewerScene) -> serde_json::Value {
    view.plans.as_ref().map_or(serde_json::Value::Null, |plans| {
        serde_json::json!({
            "scope": "canonical world surface batches after lightmap intersections",
            "compatibility": "consecutive identical DrawItem and lightmap page; reset at every surface batch",
            "native_multi_draw_capability_checked": false,
            "transparent": {
                "scope": "actual ordered indexed parts; separate world/sky/view/pass/transfer segments",
                "compatibility": "consecutive identical DrawItem, RenderMode, original/visible index source, lightmap page; reset at Overlay/Sprite/missing resource/empty mesh",
                "world": collect_transparency(&view.transparency, &view.visibility, upload),
                "skybox": collect_transparency(&view.skybox_transparency, &view.skybox_visibility, upload),
            },
            "world": collect_plan(&plans.world, upload, view.world_groups).json(),
            "skybox": plans.map_skybox.as_ref().map(|plan| {
                collect_plan(plan, upload, WorldDrawGroups::ALL).json()
            }),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_preserve_item_material_page_order_and_surface_batch_boundaries() {
        let a = (3, 4, Some(7));
        let b = (3, 5, Some(7));
        let c = (3, 5, Some(8));
        let d = (4, 5, Some(8));
        let mut histogram = RunHistogram::default();
        histogram.observe_batch([a, a, b, c, d, d, a]);
        histogram.observe_batch([a, a]);
        histogram.observe_batch(std::iter::empty::<(u32, u32, Option<u32>)>());
        histogram.observe_batch([(3, 4, None::<u32>), (3, 4, None)]);
        assert_eq!(histogram.total_parts, 11);
        assert_eq!(histogram.run_count, 7);
        assert_eq!(histogram.lengths, BTreeMap::from([(1, 3), (2, 4)]));
        assert_eq!(histogram.surface_batches, 4);
        let json = histogram.json();
        assert_eq!(json["indirect_argument_stride_bytes"], 20);
        assert_eq!(json["indirect_arguments_bytes"], 220);
        assert_eq!(json["draw_calls_saved_upper_bound"], 4);
    }

    #[test]
    fn transparent_census_matches_ordered_parts_and_all_draw_loop_barriers() {
        use super::super::RenderMode;
        use gmpublished_domain::scene::map::LightmapPageId;
        let item = DrawItem {
            mesh_index: 0,
            material_slot: 0,
        };
        let mesh = |item, range, blend| Draw {
            primitive: Primitive::Mesh(item, range),
            depth: 0.0,
            blend,
        };
        let ordinary = || mesh(item, Some(9..12), RenderMode::Translucent);
        let operations = [
            mesh(item, Some(0..6), RenderMode::Translucent),
            ordinary(),
            mesh(item, None, RenderMode::Translucent),
            ordinary(),
            Draw {
                primitive: Primitive::Sprite(0..6),
                depth: 0.0,
                blend: RenderMode::Translucent,
            },
            ordinary(),
            ordinary(),
            mesh(
                DrawItem {
                    material_slot: 99,
                    ..item
                },
                None,
                RenderMode::Translucent,
            ),
            ordinary(),
            mesh(
                DrawItem {
                    mesh_index: 1,
                    ..item
                },
                None,
                RenderMode::Translucent,
            ),
            ordinary(),
            mesh(item, Some(12..15), RenderMode::Additive),
            mesh(item, Some(15..18), RenderMode::Additive),
            mesh(item, Some(18..21), RenderMode::Modulate),
            ordinary(),
        ];
        let histogram = transparent_histogram(&operations, |selected, range| {
            if selected.material_slot == 99 || selected.mesh_index == 1 {
                return None;
            }
            let page = |indices, page| PageRange {
                indices,
                page: Some(LightmapPageId(page)),
            };
            Some(match range {
                None => (true, vec![page(0..3, 0), page(3..6, 0)]),
                Some(range) if *range == (0..6) => (false, vec![page(0..3, 0), page(3..6, 0)]),
                Some(range) => (
                    false,
                    vec![page(range.clone(), u32::from(range.start == 15))],
                ),
            })
        });
        // Independent emitted order: original page0 x3; visible page0 x2;
        // original page0; Sprite; original x2; missing material; original;
        // missing visible buffer; original; additive page0; additive page1;
        // modulate page0; original page0. Ranges/sources never sort or dedup.
        assert_eq!(histogram.total_parts, 14);
        assert_eq!(histogram.run_count, 10);
        assert_eq!(histogram.lengths, BTreeMap::from([(1, 7), (2, 2), (3, 1)]));
        assert_eq!(histogram.json()["draw_calls_saved_upper_bound"], 4);
        // A fresh invocation is a fresh pass/view; identical edge keys cannot join.
        let singleton = transparent_histogram(&[ordinary()], |_, _| {
            Some((
                false,
                vec![PageRange {
                    indices: 9..12,
                    page: None,
                }],
            ))
        });
        assert_eq!(singleton.total_parts, 1);
        assert_eq!(singleton.run_count, 1);
    }

    #[test]
    fn percentile_counts_runs_rather_than_parts_and_empty_batches_have_no_runs() {
        let mut histogram = RunHistogram::default();
        assert_eq!(histogram.percentile_95(), None);
        for _ in 0..19 {
            histogram.observe_batch([1]);
        }
        histogram.observe_batch([1; 100]);
        assert_eq!(histogram.percentile_95(), Some(1));
        histogram.observe_batch([1; 100]);
        assert_eq!(histogram.percentile_95(), Some(100));
        assert_eq!(histogram.lengths.last_key_value(), Some((&100, &2)));
    }
}
