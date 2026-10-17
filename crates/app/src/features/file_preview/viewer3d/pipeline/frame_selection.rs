use super::MaterialSlot;
use crate::bridge::materials::{
    AnimationClock, AnimationError, DecodedTextureFrames, EnvironmentMapSource, FrameChannel,
    MaterialShader, ResolvedTexture, ScreenRefraction, SurfaceMaterial,
};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(super) struct MaterialFrames {
    pub base: u32,
    pub base2: u32,
    pub normal: u32,
    pub normal2: u32,
    pub detail: u32,
    pub environment: u32,
    pub environment_mask: u32,
}

impl MaterialFrames {
    pub(super) fn select(
        slot: &MaterialSlot,
        clock: AnimationClock,
        water_time: f32,
    ) -> Result<Self, AnimationError> {
        let mut selected = Self::default();
        match &slot.shader {
            MaterialShader::Surface(surface) => {
                selected.base = base_frame(surface.base_frame, base_count(slot.texture.as_deref()));
                if surface.base_blend.uses_secondary() {
                    selected.base2 =
                        base_frame(surface.base2_frame, base_count(slot.texture2.as_deref()));
                }
                if let Some(count) = data_count(surface.normal.as_deref()) {
                    selected.normal = data_frame(surface.normal_frame, Some(count))?;
                }
                if let Some(detail) = &surface.detail
                    && let Some(count) = data_count(detail.texture.as_deref())
                {
                    selected.detail = data_frame(detail.frame, Some(count))?;
                }
                if let Some(environment) = &surface.envmap {
                    selected.environment = environment.frame;
                    selected.environment_mask = environment.mask_frame
                        % data_count(environment.mask_texture.as_deref())
                            .unwrap_or(1)
                            .max(1);
                }
                for animation in &surface.animated_textures {
                    if !destination_available(slot, surface, animation.frame_channel) {
                        continue;
                    }
                    let Some(count) = input_count(slot, surface, &animation.texture_parameter)
                    else {
                        continue;
                    };
                    let frame = animation.frame(clock, count)?;
                    selected.assign(slot, surface, animation.frame_channel, frame);
                }
            }
            MaterialShader::Water(water) => {
                selected.normal = water_frame(
                    water.bump_frame,
                    water.frame_rate,
                    water.animation_wrap,
                    water_time,
                    data_count(water.normal_map.as_deref()).unwrap_or(1),
                )?;
            }
            MaterialShader::Mirror(_)
            | MaterialShader::Unknown
            | MaterialShader::Unsupported(_) => {}
        }
        Ok(selected)
    }

    fn assign(
        &mut self,
        slot: &MaterialSlot,
        surface: &SurfaceMaterial,
        channel: FrameChannel,
        requested: i32,
    ) {
        match channel {
            FrameChannel::Base => {
                self.base = base_integer(requested, base_count(slot.texture.as_deref()));
            }
            FrameChannel::Base2 => {
                self.base2 = base_integer(requested, base_count(slot.texture2.as_deref()));
            }
            FrameChannel::Bump => {
                self.normal = data_integer(requested, data_count(surface.normal.as_deref()));
            }
            FrameChannel::Bump2 => {
                self.normal2 = data_integer(requested, data_count(surface.normal2.as_deref()));
            }
            FrameChannel::Detail => {
                self.detail = data_integer(
                    requested,
                    surface
                        .detail
                        .as_ref()
                        .and_then(|detail| data_count(detail.texture.as_deref())),
                );
            }
            FrameChannel::Environment => self.environment = requested.max(0) as u32,
            FrameChannel::EnvironmentMask => {
                self.environment_mask = data_integer(
                    requested,
                    surface
                        .envmap
                        .as_ref()
                        .and_then(|environment| data_count(environment.mask_texture.as_deref())),
                );
            }
        }
    }
}

fn base_count(texture: Option<&ResolvedTexture>) -> Option<u32> {
    let texture = texture?;
    if texture.load_outcome().is_unavailable() && texture.source_metadata().is_none() {
        return None;
    }
    let count = texture.effective_frame_count();
    (count > 0).then_some(count)
}

fn data_count(texture: Option<&DecodedTextureFrames>) -> Option<u32> {
    texture?
        .source_metadata()
        .map(|metadata| metadata.frame_count)
        .filter(|count| *count > 0)
}

fn destination_available(
    slot: &MaterialSlot,
    surface: &SurfaceMaterial,
    channel: FrameChannel,
) -> bool {
    match channel {
        FrameChannel::Base => slot.texture.is_some(),
        FrameChannel::Base2 => surface.base_blend.uses_secondary() && slot.texture2.is_some(),
        FrameChannel::Bump => surface.normal.is_some(),
        FrameChannel::Bump2 => surface.normal2.is_some(),
        FrameChannel::Detail => surface
            .detail
            .as_ref()
            .is_some_and(|detail| detail.texture.is_some()),
        FrameChannel::Environment => surface.envmap.is_some(),
        FrameChannel::EnvironmentMask => surface
            .envmap
            .as_ref()
            .is_some_and(|environment| environment.mask_texture.is_some()),
    }
}

fn input_count(slot: &MaterialSlot, surface: &SurfaceMaterial, parameter: &str) -> Option<u32> {
    match parameter {
        "$basetexture" => base_count(slot.texture.as_deref()),
        "$basetexture2" if surface.base_blend.uses_secondary() => {
            base_count(slot.texture2.as_deref())
        }
        "$bumpmap" => data_count(surface.normal.as_deref()),
        "$bumpmap2" => data_count(surface.normal2.as_deref()),
        "$detail" => data_count(surface.detail.as_ref()?.texture.as_deref()),
        "$envmap" => match &surface.envmap.as_ref()?.source {
            EnvironmentMapSource::Named(cube) => {
                cube.source_metadata().map(|metadata| metadata.frame_count)
            }
            EnvironmentMapSource::LocalProbe => None,
        },
        "$envmapmask" => data_count(surface.envmap.as_ref()?.mask_texture.as_deref()),
        "$selfillummask" => data_count(surface.emission.as_ref()?.mask.as_deref()),
        "$blendmodulatetexture" => data_count(surface.blend_modulation.as_deref()),
        "$phongexponenttexture" => data_count(
            surface
                .phong
                .as_ref()?
                .exponent_texture
                .as_ref()?
                .texture
                .as_deref(),
        ),
        "$phongwarptexture" => data_count(
            surface
                .phong
                .as_ref()?
                .specular_warp
                .as_ref()?
                .texture
                .as_deref(),
        ),
        "$lightwarptexture" => data_count(
            surface
                .phong
                .as_ref()?
                .diffuse_warp
                .as_ref()?
                .texture
                .as_deref(),
        ),
        _ => None,
    }
}

fn base_frame(requested: f32, count: Option<u32>) -> u32 {
    let count = count.unwrap_or(1);
    if requested.is_finite() && requested >= 0.0 && requested < count as f32 {
        requested as u32
    } else {
        0
    }
}

fn base_integer(requested: i32, count: Option<u32>) -> u32 {
    u32::try_from(requested)
        .ok()
        .filter(|frame| *frame < count.unwrap_or(1))
        .unwrap_or(0)
}

fn data_frame(requested: f32, count: Option<u32>) -> Result<u32, AnimationError> {
    if !requested.is_finite() {
        return Err(AnimationError::NonFinite);
    }
    let requested = requested.max(0.0).trunc();
    if f64::from(requested) > f64::from(u32::MAX) {
        return Err(AnimationError::IntegerRange);
    }
    Ok(requested as u32 % count.unwrap_or(1).max(1))
}

fn data_integer(requested: i32, count: Option<u32>) -> u32 {
    requested.max(0) as u32 % count.unwrap_or(1).max(1)
}

fn water_frame(
    bump: f32,
    rate: f32,
    wrap: bool,
    time: f32,
    count: u32,
) -> Result<u32, AnimationError> {
    if !bump.is_finite() || !rate.is_finite() || !time.is_finite() {
        return Err(AnimationError::NonFinite);
    }
    let raw = (bump + time * rate).floor();
    if !raw.is_finite()
        || f64::from(raw) < f64::from(i32::MIN)
        || f64::from(raw) > f64::from(i32::MAX)
    {
        return Err(AnimationError::IntegerRange);
    }
    let count = i32::try_from(count.max(1)).map_err(|_| AnimationError::FrameCount)?;
    let raw = raw as i32;
    let selected = if wrap {
        raw % count
    } else {
        raw.min(count - 1)
    };
    u32::try_from(selected).map_err(|_| AnimationError::IntegerRange)
}

pub(super) fn immersion_frame(effect: &ScreenRefraction) -> Result<u32, AnimationError> {
    if !effect.bump_frame.is_finite() {
        return Err(AnimationError::NonFinite);
    }
    Ok(effect
        .bump_frame
        .floor()
        .rem_euclid(data_count(Some(&effect.normal_map)).unwrap_or(1).max(1) as f32) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{
        gma::PreviewArchive,
        materials::{
            AnimatedTexture, AnimationWrap, EnvironmentMapMask, EnvironmentMapMaterial,
            MaterialResolver, ResolvedCubemap, ResolvedTextureMip,
        },
    };
    use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
    use std::sync::Arc;

    fn clock(time_seconds: f32) -> AnimationClock {
        AnimationClock {
            time_seconds,
            frame_seconds: 1.0 / 60.0,
        }
    }

    fn vtf(frames: u16) -> Vec<u8> {
        let pixels = [255, 128, 64, 255].repeat(usize::from(frames));
        let mut bytes = fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&pixels]);
        bytes[24..26].copy_from_slice(&frames.to_le_bytes());
        bytes
    }

    fn slot(parameters: &str) -> MaterialSlot {
        let material = format!(r#"LightmappedGeneric {{ "$basetexture" "base" {parameters} }}"#);
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Selected frames")
                .entry("materials/control.vmt", material.into_bytes())
                .entry("materials/base.vtf", vtf(363))
                .entry("materials/normal.vtf", vtf(3))
                .entry("materials/detail.vtf", vtf(5))
                .build(),
        )
        .unwrap();
        let resolved = MaterialResolver::new(Arc::new(archive), None)
            .with_on_demand_textures()
            .resolve_with_base2(&[], "control");
        let resolution = resolved.outcome();
        let material = resolved.into_material().unwrap();
        MaterialSlot {
            resolution,
            canonical_name: Some("control".into()),
            shader: material.shader,
            name: "control".into(),
            texture: material.texture,
            texture2: material.texture2,
            render_mode: material.render_mode,
            alpha_policy: material.render_mode.texture_alpha_policy(),
        }
    }

    #[test]
    fn on_demand_metadata_selects_every_global_frame_without_an_array_allocation() {
        let slot = slot(
            r#"Proxies { AnimatedTexture {
            animatedtexturevar "$basetexture" animatedtextureframenumvar "$frame"
            animatedtextureframerate "30"
        } }"#,
        );
        let texture = slot.texture.as_ref().unwrap();
        assert!(texture.resident().is_none());
        assert_eq!(texture.effective_frame_count(), 363);
        for frame in 0..363 {
            assert_eq!(
                MaterialFrames::select(&slot, clock((frame as f32 + 0.25) / 30.0), 0.0)
                    .unwrap()
                    .base,
                frame
            );
        }
        assert_eq!(
            MaterialFrames::select(&slot, clock(12.1), 0.0)
                .unwrap()
                .base,
            0
        );
    }

    #[test]
    fn cross_channel_programs_use_input_counts_then_existing_destination_normalization() {
        let slot = slot(
            r#""$bumpmap" "normal" "$detail" "detail"
            Proxies {
                AnimatedTexture { animatedtexturevar "$detail" animatedtextureframenumvar "$frame" animatedtextureframerate "1" }
                AnimatedTexture { animatedtexturevar "$basetexture" animatedtextureframenumvar "$bumpframe" animatedtextureframerate "1" }
                AnimatedTexture { animatedtexturevar "$basetexture2" animatedtextureframenumvar "$frame" animatedtextureframerate "100" }
            }"#,
        );
        let frames = MaterialFrames::select(&slot, clock(7.25), 0.0).unwrap();
        assert_eq!(frames.base, 2);
        assert_eq!(frames.normal, 1);
        assert_eq!(frames.normal2, 0);
    }

    #[test]
    fn ordered_writes_and_negative_base_output_keep_native_reset_to_zero() {
        let slot = slot(
            r#"Proxies {
            AnimatedTexture { animatedtexturevar "$basetexture" animatedtextureframenumvar "$frame" animatedtextureframerate "1" }
            AnimatedTexture { animatedtexturevar "$basetexture" animatedtextureframenumvar "$frame" animatedtextureframerate "2" }
        }"#,
        );
        assert_eq!(
            MaterialFrames::select(&slot, clock(7.25), 0.0)
                .unwrap()
                .base,
            14
        );
        let mut negative = slot;
        if let MaterialShader::Surface(surface) = &mut negative.shader {
            Arc::make_mut(surface).animated_textures[1].frame_rate = -2.0;
        }
        assert_eq!(
            MaterialFrames::select(&negative, clock(7.25), 0.0)
                .unwrap()
                .base,
            0
        );
    }

    #[test]
    fn fixed_base_resets_and_data_array_clamp_modulo_are_retained() {
        let slot = slot(
            r#""$frame" "363" "$bumpmap" "normal" "$bumpframe" "7.9" "$detail" "detail" "$detailframe" "-1""#,
        );
        let frames = MaterialFrames::select(&slot, clock(0.0), 0.0).unwrap();
        assert_eq!(frames.base, 0);
        assert_eq!(frames.normal, 1);
        assert_eq!(frames.detail, 0);
        assert_eq!(base_frame(f32::NAN, Some(363)), 0);
        assert_eq!(
            data_frame(f32::NAN, Some(3)),
            Err(AnimationError::NonFinite)
        );
    }

    #[test]
    fn named_cube_input_uses_global_metadata_count_not_its_resident_subset() {
        let mut slot = slot("");
        let cube = Arc::new(
            ResolvedCubemap::from_rgba(
                0,
                4,
                std::array::from_fn(|_| {
                    vec![ResolvedTextureMip {
                        rgba: vec![255; 4],
                        width: 1,
                        height: 1,
                    }]
                }),
            )
            .unwrap(),
        );
        assert_eq!(cube.source_metadata().unwrap().frame_count, 5);
        let MaterialShader::Surface(surface) = &mut slot.shader else {
            panic!("surface");
        };
        let surface = Arc::make_mut(surface);
        surface.envmap = Some(Arc::new(EnvironmentMapMaterial {
            source: EnvironmentMapSource::Named(cube),
            path: "cube".into(),
            frame: 9,
            tint: [1.0; 3],
            contrast: 0.0,
            saturation: 1.0,
            mask: EnvironmentMapMask::None,
            mask_texture: None,
            mask_frame: 0,
            mask_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            fresnel: 1.0,
        }));
        surface.animated_textures.push(AnimatedTexture {
            texture_parameter: "$envmap".into(),
            frame_channel: FrameChannel::Base,
            frame_rate: 1.0,
            wrap: AnimationWrap::Loop,
        });
        let frames = MaterialFrames::select(&slot, clock(7.25), 0.0).unwrap();
        assert_eq!(frames.base, 2);
        assert_eq!(frames.environment, 9);
    }

    #[test]
    fn water_retains_legacy_floor_loop_and_lifetime_clamp() {
        assert_eq!(water_frame(0.0, 30.0, true, 256.25 / 30.0, 363), Ok(256));
        assert_eq!(water_frame(0.0, 30.0, true, 12.1, 363), Ok(0));
        assert_eq!(water_frame(0.0, 30.0, false, 12.1, 363), Ok(362));
        assert_eq!(water_frame(0.75, 1.0, true, 0.5, 3), Ok(1));
        assert_eq!(
            water_frame(0.0, 1.0, true, f32::NAN, 3),
            Err(AnimationError::NonFinite)
        );
    }

    #[test]
    fn immersion_uses_existing_floor_positive_remainder() {
        let normal_map = Arc::new(
            DecodedTextureFrames::from_rgba(
                0,
                (0..3)
                    .map(|_| {
                        vec![ResolvedTextureMip {
                            rgba: vec![255; 4],
                            width: 1,
                            height: 1,
                        }]
                    })
                    .collect(),
            )
            .unwrap(),
        );
        let effect = ScreenRefraction {
            name: "underwater".into(),
            normal_map,
            bump_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            proxy_scroll: [0.0; 2],
            bump_frame: -0.25,
            refract_amount: 0.0,
            tint_linear: [1.0; 3],
            blur: false,
        };
        assert_eq!(immersion_frame(&effect), Ok(2));
    }
}
