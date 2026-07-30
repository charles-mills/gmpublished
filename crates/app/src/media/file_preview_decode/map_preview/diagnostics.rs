//! Structured diagnostics for the map-preview build pipeline.

use std::time::Duration;

use crate::media::preview_model::MapStats;

use super::{PropBakeSkipStats, duration_ms, format_mib};

pub(super) fn lightmap_status(
    lightmap: Option<&gmpublished_domain::scene::map::LightmapAtlas>,
) -> String {
    lightmap.map_or_else(
        || "lightmap none".to_owned(),
        |lightmap| {
            let source = match lightmap.source {
                gmpublished_domain::scene::map::LightmapSource::Ldr => "LDR",
                gmpublished_domain::scene::map::LightmapSource::Hdr => "HDR",
            };
            format!("lightmap {}x{} ({source})", lightmap.width, lightmap.height)
        },
    )
}

/// Pre-formatted status fragments used by the map build summary.
pub(super) struct MapPreviewStatuses<'a> {
    pub(super) water: &'a str,
    pub(super) render_mode: &'a str,
    pub(super) texture_mib: &'a str,
    pub(super) texture_payloads: &'a str,
    pub(super) lightmap: &'a str,
    pub(super) sky: &'a str,
}

/// Wall-clock duration of each map-build phase.
pub(super) struct MapPreviewTimings {
    pub(super) bsp: Duration,
    pub(super) materials: Duration,
    pub(super) props: Duration,
    pub(super) prop_load: Duration,
    pub(super) prop_bake: Duration,
    pub(super) lightmap: Duration,
}

/// Emits the single high-value summary line for a completed map build.
pub(super) fn log_map_preview_summary(
    entry_path: &str,
    stats: &MapStats,
    statuses: &MapPreviewStatuses<'_>,
    prop_skip_stats: &PropBakeSkipStats,
    prop_mesh_bytes: usize,
    skipped_overlay_count: u32,
    timings: &MapPreviewTimings,
) {
    let MapPreviewStatuses {
        water,
        render_mode,
        texture_mib,
        texture_payloads,
        lightmap,
        sky,
    } = statuses;
    log::info!(
        "map {entry_path}: materials resolved {}/{}{water}{render_mode}, textures {texture_mib} MiB{texture_payloads}, {lightmap}, {sky}, clusters {}, skybox faces {}, props {}, props placed {} (skipped {}: cap {}, triangles {}, load {}, invalid {}, empty {}), prop mesh {prop_mesh_bytes} bytes ({} MiB), detail sprites {}, overlays {} (skipped {skipped_overlay_count}), timings: bsp {}ms, materials {}ms, props {}ms, props load {}ms, bake {}ms, lightmap {}ms",
        stats.resolved_material_count,
        stats.material_count,
        stats.cluster_count,
        stats.skybox_face_count,
        stats.skybox_prop_count,
        stats.placed_prop_count,
        stats.skipped_prop_count,
        prop_skip_stats.placement_cap,
        prop_skip_stats.triangle_cap,
        prop_skip_stats.load_failure,
        prop_skip_stats.invalid_model_path,
        prop_skip_stats.no_bakeable_mesh,
        format_mib(prop_mesh_bytes),
        stats.detail_sprite_count,
        stats.overlay_count,
        duration_ms(timings.bsp),
        duration_ms(timings.materials),
        duration_ms(timings.props),
        duration_ms(timings.prop_load),
        duration_ms(timings.prop_bake),
        duration_ms(timings.lightmap),
    );
}
