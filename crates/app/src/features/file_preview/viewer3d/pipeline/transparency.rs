//! Sort logical faces/prop placements across materials, overlays and sprites.
//! Material batching must not turn distant props into one indivisible layer.
use super::reflections::SceneView;
use super::uniforms::Uniforms;
use super::upload::UploadedModel;
use super::{DrawItem, DrawPlan, OverlayDrawItem, Vec3};
use gmpublished_domain::scene::map::MapMeshVisibility;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

#[derive(Debug)]
pub struct Part {
    pub indices: Range<u32>,
    pub centre: Vec3,
    /// None means always visible; one logical part may occur in several leaves.
    pub clusters: Option<Arc<[u32]>>,
}

#[derive(Clone, Debug)]
pub enum Primitive {
    Mesh(DrawItem, Option<Range<u32>>),
    Overlay(OverlayDrawItem),
    Sprite(Range<u32>),
}
#[derive(Clone, Debug)]
pub struct Draw {
    pub primitive: Primitive,
    pub depth: f32,
    pub additive: bool,
}

pub fn parts(
    vertices: &[super::ModelVertex],
    indices: &[u32],
    visibility: &MapMeshVisibility,
) -> Vec<Part> {
    let mut ranges = BTreeMap::<u32, (Range<u32>, Option<Vec<u32>>)>::new();
    for range in &visibility.always_visible {
        ranges.insert(
            range.face,
            (range.start..range.start.saturating_add(range.count), None),
        );
    }
    for cluster in &visibility.clusters {
        for range in &cluster.ranges {
            let entry = ranges.entry(range.face).or_insert_with(|| {
                (
                    range.start..range.start.saturating_add(range.count),
                    Some(Vec::new()),
                )
            });
            if let Some(clusters) = entry.1.as_mut() {
                clusters.push(cluster.cluster);
            }
        }
    }
    let mut parts = Vec::new();
    for (range, clusters) in ranges.into_values() {
        let Some(selected) = indices.get(range.start as usize..range.end as usize) else {
            continue;
        };
        let clusters = clusters.map(Arc::<[u32]>::from);
        // Sorting a whole prop cannot put its front face in front of a pane
        // which crosses its bounds. Retain PVS membership but order triangles.
        for (offset, triangle) in selected.chunks_exact(3).enumerate() {
            let Some(a) = vertices.get(triangle[0] as usize) else {
                continue;
            };
            let Some(b) = vertices.get(triangle[1] as usize) else {
                continue;
            };
            let Some(c) = vertices.get(triangle[2] as usize) else {
                continue;
            };
            let start = range.start + offset as u32 * 3;
            parts.push(Part {
                indices: start..start + 3,
                centre: (a.position + b.position + c.position) / 3.0,
                clusters: clusters.clone(),
            });
        }
    }
    parts
}

pub fn build_parts(
    upload: &mut UploadedModel,
    scene: &super::RenderScene,
    map: Option<&super::MapPreview>,
) {
    let Some(map) = map else {
        return;
    };
    for mesh in &mut upload.meshes {
        if mesh.door_index.is_some() || mesh.map_skybox {
            continue;
        }
        if !matches!(
            upload.material_render_modes.get(mesh.material_index),
            Some(super::RenderMode::Translucent | super::RenderMode::Additive)
        ) {
            continue;
        }
        let Some(source) = scene.meshes.get(mesh.scene_mesh_index) else {
            continue;
        };
        let Some(visibility) = map.mesh_visibility.get(mesh.scene_mesh_index) else {
            continue;
        };
        mesh.transparent_parts = parts(&source.vertices, &source.indices, visibility);
    }
}

fn depth(uniforms: Uniforms, position: Vec3) -> f32 {
    (0..3)
        .map(|axis| uniforms.view_proj[axis][3] * position[axis])
        .sum::<f32>()
        + uniforms.view_proj[3][3]
}

pub fn prepare(
    view: &mut SceneView,
    upload: &UploadedModel,
    world: Uniforms,
    skybox: Option<Uniforms>,
    detail_water_side: Option<bool>,
) {
    let Some(plans) = view.plans.as_ref() else {
        return;
    };
    view.transparency = queue(
        upload,
        &plans.world,
        world,
        &view.visibility,
        false,
        detail_water_side,
    );
    view.skybox_transparency = plans
        .map_skybox
        .as_ref()
        .zip(skybox)
        .map_or_else(Vec::new, |(plan, uniforms)| {
            queue(upload, plan, uniforms, &view.skybox_visibility, true, None)
        });
}

fn queue(
    upload: &UploadedModel,
    plan: &DrawPlan,
    uniforms: Uniforms,
    visibility: &super::visibility::UploadedVisibility,
    skybox: bool,
    detail_water_side: Option<bool>,
) -> Vec<Draw> {
    let mut draws = Vec::new();
    for (items, additive) in [(&plan.translucent, false), (&plan.additive, true)] {
        for item in items {
            let mesh = &upload.meshes[item.mesh_index];
            if mesh.transparent_parts.is_empty() {
                draws.push(Draw {
                    primitive: Primitive::Mesh(*item, None),
                    depth: depth(uniforms, mesh.centroid),
                    additive,
                });
            } else {
                for part in &mesh.transparent_parts {
                    if let Some(plan) = visibility.plan.as_ref()
                        && part.clusters.as_ref().is_some_and(|clusters| {
                            !clusters.iter().any(|cluster| {
                                plan.visible_clusters
                                    .get(*cluster as usize)
                                    .copied()
                                    .unwrap_or(false)
                            })
                        })
                    {
                        continue;
                    }
                    draws.push(Draw {
                        primitive: Primitive::Mesh(*item, Some(part.indices.clone())),
                        depth: depth(uniforms, part.centre),
                        additive,
                    });
                }
            }
        }
    }
    for (items, additive) in [
        (&plan.overlay_translucent, false),
        (&plan.overlay_additive, true),
    ] {
        for item in items {
            draws.push(Draw {
                primitive: Primitive::Overlay(*item),
                depth: depth(uniforms, upload.overlays[item.overlay_index].centroid),
                additive,
            });
        }
    }
    let sprites = if skybox {
        upload.map_skybox_detail_sprites.as_ref()
    } else {
        upload.detail_sprites.as_ref()
    };
    if let Some(sprites) = sprites {
        let stride = super::DETAIL_VERTEX_FLOAT_COUNT as usize * 4;
        for (index, bytes) in sprites.all_vertices.chunks_exact(6 * stride).enumerate() {
            // Source's PruneWorldListInfo selects detail props by their leaf's
            // fog volume, not by the corners of their camera-facing quad.
            if detail_water_side
                .is_some_and(|underwater| sprites.underwater_leaves[index] != underwater)
            {
                continue;
            }
            if visibility.plan.as_ref().is_some_and(|plan| {
                !plan
                    .detail_sprite_visible
                    .get(index)
                    .copied()
                    .unwrap_or(false)
            }) {
                continue;
            }
            let position = Vec3::from(std::array::from_fn(|axis| {
                f32::from_le_bytes(bytes[axis * 4..axis * 4 + 4].try_into().unwrap())
            }));
            let eye = Vec3::from([
                uniforms.camera_position[0],
                uniforms.camera_position[1],
                uniforms.camera_position[2],
            ]);
            let radius = bytes
                .chunks_exact(stride)
                .map(|vertex| {
                    let x = f32::from_le_bytes(vertex[12..16].try_into().unwrap());
                    let y = f32::from_le_bytes(vertex[16..20].try_into().unwrap());
                    x.hypot(y)
                })
                .fold(0.0_f32, f32::max);
            if position.distance_squared(eye) > (1024.0 + radius).powi(2) {
                continue;
            }
            draws.push(Draw {
                primitive: Primitive::Sprite(index as u32 * 6..(index as u32 + 1) * 6),
                depth: depth(uniforms, position),
                additive: false,
            });
        }
    }
    // Stable ordering preserves authored order for coplanar/tied primitives.
    draws.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    draws
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::{MapMeshClusterRanges, MapMeshIndexRange};
    #[test]
    fn one_transparent_prop_can_straddle_a_separately_sorted_pane() {
        let make = |y| super::super::ModelVertex {
            position: Vec3::new(0.0, y, 0.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
        };
        let vertices = vec![
            make(300.0),
            make(300.0),
            make(300.0),
            make(100.0),
            make(100.0),
            make(100.0),
        ];
        let metadata = MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 6,
            }],
            clusters: vec![],
        };
        let selected = parts(&vertices, &[0, 1, 2, 3, 4, 5], &metadata);
        assert_eq!(selected.len(), 2);
        let mut depths = selected
            .iter()
            .map(|part| part.centre[1])
            .chain([200.0])
            .collect::<Vec<_>>();
        depths.sort_by(|a, b| b.total_cmp(a));
        assert_eq!(depths, vec![300.0, 200.0, 100.0]);
        assert_eq!(selected[0].indices, 0..3);
        assert_eq!(selected[1].indices, 3..6);
    }

    #[test]
    fn shared_material_placements_keep_separate_ranges_and_cluster_membership() {
        let make = |z| super::super::ModelVertex {
            position: Vec3::new(0.0, 0.0, z),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
        };
        let vertices = vec![
            make(300.0),
            make(300.0),
            make(300.0),
            make(100.0),
            make(100.0),
            make(100.0),
        ];
        let range = MapMeshIndexRange {
            face: 7,
            start: 3,
            count: 3,
        };
        let metadata = MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 3,
            }],
            clusters: vec![
                MapMeshClusterRanges {
                    cluster: 2,
                    ranges: vec![range],
                },
                MapMeshClusterRanges {
                    cluster: 9,
                    ranges: vec![range],
                },
            ],
        };
        let selected = parts(&vertices, &[0, 1, 2, 3, 4, 5], &metadata);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].indices, 0..3);
        assert_eq!(selected[0].centre[2], 300.0);
        assert_eq!(selected[1].indices, 3..6);
        assert_eq!(selected[1].centre[2], 100.0);
        assert_eq!(selected[1].clusters.as_deref(), Some([2, 9].as_slice()));
    }
}
