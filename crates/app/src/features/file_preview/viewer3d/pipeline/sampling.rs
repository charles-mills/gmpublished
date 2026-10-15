//! Main sampling is fixed at resource creation; optical attachments use one sample.

pub(in crate::features::file_preview::viewer3d) fn scene_sample_count() -> u32 {
    #[cfg(feature = "map-capture")]
    {
        static COUNT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *COUNT.get_or_init(|| {
            capture_sample_count(
                std::env::var("GMPUBLISHED_MAP_CAPTURE_MSAA_SAMPLES")
                    .ok()
                    .as_deref(),
            )
        })
    }
    #[cfg(not(feature = "map-capture"))]
    {
        super::super::MSAA_SAMPLE_COUNT
    }
}

#[cfg(any(test, feature = "map-capture"))]
fn capture_sample_count(value: Option<&str>) -> u32 {
    match value {
        None | Some("4") => super::super::MSAA_SAMPLE_COUNT,
        Some("1") => 1,
        Some(_) => panic!("GMPUBLISHED_MAP_CAPTURE_MSAA_SAMPLES must be 1 or 4"),
    }
}

pub(super) fn visibility_shader(source: &str, count: u32) -> std::borrow::Cow<'_, str> {
    if count > 1 {
        return source.into();
    }
    // Single-sample textureLoad interprets its third argument as mip zero.
    source
        .replace("texture_depth_multisampled_2d", "texture_depth_2d")
        .replace("textureNumSamples(depth)", "1u")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_override_preserves_the_default_and_allows_single_sample() {
        assert_eq!(capture_sample_count(None), 4);
        assert_eq!(capture_sample_count(Some("4")), 4);
        assert_eq!(capture_sample_count(Some("1")), 1);
    }

    #[test]
    #[should_panic(expected = "must be 1 or 4")]
    fn unsupported_capture_sample_count_is_rejected_before_gpu_allocation() {
        capture_sample_count(Some("3"));
    }

    #[test]
    fn single_depth_water_and_both_sun_visibility_variants_validate() {
        use iced::wgpu::naga::{self, valid};
        let water = super::super::WATER_SHADER_SOURCE;
        assert!(!water.contains("@builtin(sample_index)"));
        assert!(!water.contains("texture_depth_multisampled_2d"));
        let sun = include_str!("sun/visibility.wgsl");
        for source in [
            water.into(),
            visibility_shader(sun, 1),
            visibility_shader(sun, 4),
        ] {
            let module = naga::front::wgsl::parse_str(&source).unwrap();
            valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all())
                .validate(&module)
                .unwrap();
        }
    }
}
