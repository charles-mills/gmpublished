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
    fn single_depth_water_and_both_sun_visibility_variants_validate() {
        use iced::wgpu::naga::{self, valid};
        let water = super::super::WATER_SHADER_SOURCE;
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
