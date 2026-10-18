use crate::bridge::materials::{SourceSurfaceBaseBlend, SurfaceFamily, SurfaceMaterial};

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceShaderKind {
    Fallback = 0,
    Lightmapped = 1,
    Transition = 2,
    VertexLit = 3,
    Unlit = 4,
    DecalModulate = 5,
}

impl From<SurfaceFamily> for SurfaceShaderKind {
    fn from(family: SurfaceFamily) -> Self {
        match family {
            SurfaceFamily::Lightmapped => Self::Lightmapped,
            SurfaceFamily::Transition => Self::Transition,
            SurfaceFamily::VertexLit => Self::VertexLit,
            SurfaceFamily::Unlit => Self::Unlit,
            SurfaceFamily::DecalModulate => Self::DecalModulate,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceBlendUniform {
    pub base_mode: u32,
    pub masked: u32,
    pub modulation: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceUniform {
    pub flags: [u32; 4],
    pub modulation: [f32; 4],
    pub model_modulation: [f32; 4],
    pub base_s: [f32; 4],
    pub base_t: [f32; 4],
    pub detail_s: [f32; 4],
    pub detail_t: [f32; 4],
    pub detail: [f32; 4],
    pub detail_tint: [f32; 4],
    pub emission: [f32; 4],
    pub emission_fresnel: [f32; 4],
    pub blend_s: [f32; 4],
    pub blend_t: [f32; 4],
    pub blend: SurfaceBlendUniform,
    pub state: [u32; 4],
    pub misc: [f32; 4],
    pub env_flags: [u32; 4],
    pub env_tint: [f32; 4],
    pub env_params: [f32; 4],
    pub env_mask_s: [f32; 4],
    pub env_mask_t: [f32; 4],
    pub alpha_state: [u32; 4],
    pub base_scroll: [f32; 4],
    pub normal_scroll: [f32; 4],
    pub detail_scroll: [f32; 4],
    pub blend_scroll: [f32; 4],
    pub env_mask_scroll: [f32; 4],
}

fn model_gamma(value: f32) -> f32 {
    if value > 1.0 {
        value
    } else {
        environment_gamma(value)
    }
}

// IMaterialVar bounds environment tint before gamma conversion.
fn environment_gamma(value: f32) -> f32 {
    if value >= 0.95 {
        1.0
    } else {
        ((value.max(0.0) * 255.0).round_ties_even() / 255.0).powf(2.2)
    }
}

fn alpha_test_reference(authored: Option<f32>) -> f32 {
    // Non-positive references inherit GEQUAL at byte178.
    let reference = authored.filter(|value| *value > 0.0).unwrap_or(0.7);
    (reference * 255.0).trunc() / 255.0
}

impl SurfaceUniform {
    pub fn new(
        surface: Option<&SurfaceMaterial>,
        base_flags: Option<u32>,
        base2_flags: Option<u32>,
    ) -> Self {
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];
        let mut result = Self {
            flags: [SurfaceShaderKind::Fallback as u32, 0, 0, 0],
            modulation: [1.0; 4],
            model_modulation: [1.0; 4],
            base_s: identity[0],
            base_t: identity[1],
            detail_s: identity[0],
            detail_t: identity[1],
            detail: [0.0; 4],
            detail_tint: [1.0; 4],
            emission: [1.0; 4],
            emission_fresnel: [1.0, 0.0, 1.0, 1.0],
            blend_s: identity[0],
            blend_t: identity[1],
            blend: SurfaceBlendUniform {
                base_mode: SourceSurfaceBaseBlend::Single as u32,
                masked: 0,
                modulation: 0,
                padding: 0,
            },
            state: [0; 4],
            misc: [0.5, 0.0, 0.0, 0.0],
            env_flags: [0; 4],
            env_tint: [1.0; 4],
            env_params: [0.0, 1.0, 1.0, 1.0],
            env_mask_s: identity[0],
            env_mask_t: identity[1],
            alpha_state: [1, 1, 0, 0],
            base_scroll: [0.0; 4],
            normal_scroll: [0.0; 4],
            detail_scroll: [0.0; 4],
            blend_scroll: [0.0; 4],
            env_mask_scroll: [0.0; 4],
        };
        let Some(surface) = surface else {
            return result;
        };
        use vformats::vtf::texture_flags;
        let base_has_alpha = base_flags.is_some_and(|flags| {
            flags & (texture_flags::ONEBITALPHA | texture_flags::EIGHTBITALPHA) != 0
        });
        result.alpha_state[0] = u32::from(base_flags.is_none() || base_has_alpha);
        result.alpha_state[1] = u32::from(base2_flags.is_none_or(|flags| {
            flags & (texture_flags::ONEBITALPHA | texture_flags::EIGHTBITALPHA) != 0
        }));
        result.flags[0] = SurfaceShaderKind::from(surface.family) as u32;
        if surface.blends_in_gamma_space() {
            return result;
        }
        for channel in 0..3 {
            result.modulation[channel] = surface.color[channel] * surface.color2[channel];
        }
        result.modulation[3] = surface.alpha;
        result.model_modulation = [
            surface.color2[0],
            surface.color2[1],
            surface.color2[2],
            surface.alpha,
        ];
        let (base_transform, base_scroll) =
            surface.texture_transform("$basetexturetransform", surface.base_transform);
        result.base_s = base_transform[0];
        result.base_t = base_transform[1];
        result.base_scroll = base_scroll;
        result.normal_scroll = surface
            .texture_transform("$bumptransform", surface.transform)
            .1;
        if let Some(detail) = &surface.detail {
            result.flags[1] = surface.effective_detail_mode().map_or(0, |mode| mode + 1);
            let model = matches!(
                surface.family,
                SurfaceFamily::VertexLit | SurfaceFamily::Unlit
            );
            let (mut transform, mut scroll) = if model {
                surface.texture_transform(
                    "$detailtexturetransform",
                    detail.transform.unwrap_or(base_transform),
                )
            } else {
                (base_transform, base_scroll)
            };
            if model && detail.transform.is_none() && scroll == [0.0; 4] {
                scroll = base_scroll;
            }
            // Detail scale also scales authored translation.
            for row in &mut transform {
                row[0] *= detail.scale[0];
                row[1] *= detail.scale[1];
            }
            transform[0][3] *= detail.scale[0];
            transform[1][3] *= detail.scale[1];
            scroll[2] *= detail.scale[0];
            scroll[3] *= detail.scale[1];
            result.detail_s = transform[0];
            result.detail_t = transform[1];
            result.detail_scroll = scroll;
            result.detail = [
                if surface.family == SurfaceFamily::Unlit {
                    1.0
                } else {
                    detail.factor
                },
                detail.frame,
                0.0,
                0.0,
            ];
            for channel in 0..3 {
                result.detail_tint[channel] = if model {
                    model_gamma(detail.tint[channel])
                } else {
                    detail.tint[channel]
                };
            }
        }
        if let Some(emission) = &surface.emission
            && surface.effective_emission_mode(base_has_alpha) != 0
        {
            result.flags[2] = surface.effective_emission_mode(base_has_alpha);
            result.emission[..3].copy_from_slice(&emission.tint);
            if let Some([minimum, maximum, exponent]) = emission.fresnel {
                let bias = if maximum != 0.0 {
                    minimum / maximum
                } else {
                    0.0
                };
                result.emission_fresnel = [1.0 - bias, bias, exponent, maximum];
            }
        }
        result.blend = SurfaceBlendUniform {
            base_mode: surface.base_blend as u32,
            masked: u32::from(surface.masked_blending),
            modulation: u32::from(surface.uses_blend_modulation(base_has_alpha)),
            padding: 0,
        };
        let (blend_transform, blend_scroll) =
            surface.texture_transform("$blendmasktransform", surface.blend_mask_transform);
        result.blend_s = blend_transform[0];
        result.blend_t = blend_transform[1];
        result.blend_scroll = blend_scroll;
        if let Some(envmap) = &surface.envmap {
            use crate::bridge::materials::{EnvironmentMapMask, EnvironmentMapSource};
            result.env_flags[0] =
                u32::from(matches!(envmap.source, EnvironmentMapSource::Named(_)));
            result.env_flags[1] = match surface.effective_environment_mask(base_has_alpha) {
                EnvironmentMapMask::None => 0,
                EnvironmentMapMask::BaseAlpha => 1,
                EnvironmentMapMask::NormalAlpha => 2,
                EnvironmentMapMask::Texture => 3,
            };
            result.env_flags[2] = envmap.mask_frame;
            result.env_flags[3] = if surface.family == SurfaceFamily::Unlit {
                u32::from(surface.vertex_color || surface.vertex_alpha) << 3
            } else {
                u32::from(surface.normal.is_some())
                    | (u32::from(surface.ssbump) << 1)
                    | (u32::from(surface.normal2.is_some()) << 2)
            };
            for channel in 0..3 {
                result.env_tint[channel] = if matches!(
                    surface.family,
                    SurfaceFamily::VertexLit | SurfaceFamily::Unlit
                ) && !surface.model_shading_profile().skin
                {
                    environment_gamma(envmap.tint[channel])
                } else {
                    envmap.tint[channel]
                };
            }
            result.env_params = [envmap.contrast, envmap.saturation, 1.0, envmap.fresnel];
            if surface.model_shading_profile().skin {
                result.env_params[3] = surface
                    .parameters
                    .get("$envmapfresnel")
                    .and_then(|value| value.parse::<f32>().ok())
                    .filter(|value| value.is_finite())
                    .unwrap_or(0.0);
            }
            if matches!(
                surface.family,
                SurfaceFamily::Lightmapped | SurfaceFamily::Transition
            ) {
                let contrast = envmap.contrast;
                let slow = (contrast != 0.0 && contrast != 1.0 && envmap.saturation != 1.0)
                    || envmap.fresnel != 1.0
                    || surface
                        .emission
                        .as_ref()
                        .is_some_and(|emission| emission.tint != [1.0; 3]);
                if !slow {
                    result.env_params[0] = if contrast == 1.0 { 1.0 } else { 0.0 };
                    result.env_params[1] = 1.0;
                }
                if result.blend.modulation != 0 {
                    result.env_params[..2].copy_from_slice(&[0.0, 1.0]);
                }
            }
            if surface.family == SurfaceFamily::VertexLit
                && !surface.model_shading_profile().skin
                && surface.normal.is_some()
                && envmap.mask != EnvironmentMapMask::NormalAlpha
                && (surface.authored_flag("$basealphaenvmapmask")
                    || surface.parameters.contains_key("$envmapmask"))
            {
                result.env_flags[0] = 0;
            }
            let (transform, scroll) =
                surface.texture_transform("$envmapmasktransform", envmap.mask_transform);
            result.env_mask_s = transform[0];
            result.env_mask_t = transform[1];
            result.env_mask_scroll = scroll;
        }
        let alpha_mask = surface.base_alpha_is_mask(base_has_alpha);
        result.state[0] = u32::from(surface.alpha_test && !alpha_mask);
        result.state[2] = u32::from(surface.blend_tint_by_base_alpha);
        result.state[3] =
            u32::from(surface.additive && !surface.translucent && !surface.vertex_alpha);
        result.misc = [
            alpha_test_reference(surface.alpha_test_reference),
            surface.tint_replace_factor,
            surface.normal_frame,
            if alpha_mask { 0.0 } else { 1.0 },
        ];
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::wgpu::naga;
    use std::mem::{offset_of, size_of};

    #[test]
    fn surface_shader_selectors_match_wgsl_constants() {
        let module = naga::front::wgsl::parse_str(
            crate::features::file_preview::viewer3d::MODEL_SHADER_SOURCE,
        )
        .unwrap();
        for (name, kind) in [
            ("SURFACE_NONE", SurfaceShaderKind::Fallback),
            ("SURFACE_LIGHTMAPPED", SurfaceShaderKind::Lightmapped),
            ("SURFACE_TRANSITION", SurfaceShaderKind::Transition),
            ("SURFACE_VERTEX_LIT", SurfaceShaderKind::VertexLit),
            ("SURFACE_UNLIT", SurfaceShaderKind::Unlit),
            ("SURFACE_DECAL_MODULATE", SurfaceShaderKind::DecalModulate),
        ] {
            let constant = module
                .constants
                .iter()
                .find_map(|(_, constant)| {
                    (constant.name.as_deref() == Some(name)).then_some(constant)
                })
                .unwrap();
            assert!(
                matches!(module.global_expressions[constant.init], naga::Expression::Literal(naga::Literal::U32(actual)) if actual == kind as u32),
                "{name}"
            );
        }
        for (name, kind) in [
            ("BASE_BLEND_SINGLE", SourceSurfaceBaseBlend::Single),
            (
                "BASE_BLEND_VERTEX_ALPHA",
                SourceSurfaceBaseBlend::VertexAlpha,
            ),
            ("BASE_BLEND_MASKED", SourceSurfaceBaseBlend::Masked),
        ] {
            let constant = module
                .constants
                .iter()
                .find_map(|(_, constant)| {
                    (constant.name.as_deref() == Some(name)).then_some(constant)
                })
                .unwrap();
            assert!(
                matches!(module.global_expressions[constant.init], naga::Expression::Literal(naga::Literal::U32(actual)) if actual == kind as u32),
                "{name}"
            );
        }
    }

    #[test]
    fn base_blend_and_masked_normal_weights_are_independent() {
        let mut material = SurfaceMaterial {
            family: SurfaceFamily::Lightmapped,
            masked_blending: true,
            ..SurfaceMaterial::default()
        };
        let single = SurfaceUniform::new(Some(&material), None, None);
        assert_eq!(
            single.blend.base_mode,
            SourceSurfaceBaseBlend::Single as u32
        );
        assert_eq!(single.blend.masked, 1);
        assert_eq!(single.blend.modulation, 0);
        material.base_blend = SourceSurfaceBaseBlend::Masked;
        let secondary = SurfaceUniform::new(Some(&material), None, None);
        assert_eq!(
            secondary.blend.base_mode,
            SourceSurfaceBaseBlend::Masked as u32
        );
        assert_eq!(secondary.blend.masked, 1);
        assert_eq!(secondary.blend.modulation, 0);
    }

    #[test]
    fn modulate_shader_ignores_generic_tint_alpha_and_texture_transform() {
        let material = SurfaceMaterial {
            family: SurfaceFamily::DecalModulate,
            color: [0.1, 0.2, 0.3],
            color2: [0.4, 0.5, 0.6],
            alpha: 0.25,
            base_transform: [[2.0, 0.0, 0.0, 0.5], [0.0, 3.0, 0.0, -0.5]],
            vertex_color: true,
            vertex_alpha: true,
            alpha_test: true,
            ..SurfaceMaterial::default()
        };
        let uniform = SurfaceUniform::new(Some(&material), None, None);
        assert_eq!(
            uniform.flags,
            [SurfaceShaderKind::DecalModulate as u32, 0, 0, 0]
        );
        assert_eq!(uniform.modulation, [1.0; 4]);
        assert_eq!(uniform.base_s, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(uniform.base_t, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(uniform.state, [0; 4]);
    }

    #[test]
    fn native_alpha_cutoff_inherits_gequal_and_preserves_byte_boundaries() {
        for authored in [None, Some(0.0), Some(-1.0), Some(0.7)] {
            let threshold = alpha_test_reference(authored);
            assert!(177.0 / 255.0 < threshold);
            assert_eq!(178.0 / 255.0, threshold);
            assert!(179.0 / 255.0 > threshold);
        }
        let threshold = alpha_test_reference(Some(0.5));
        assert!(126.0 / 255.0 < threshold);
        assert_eq!(127.0 / 255.0, threshold);
        assert!(128.0 / 255.0 > threshold);
    }

    #[test]
    fn surface_uniform_matches_composed_shader_layout() {
        let shader = naga::front::wgsl::parse_str(
            crate::features::file_preview::viewer3d::MODEL_SHADER_SOURCE,
        )
        .unwrap();
        let ty = shader
            .types
            .iter()
            .find_map(|(_, ty)| (ty.name.as_deref() == Some("Surface")).then_some(ty))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("surface uniform must be a struct")
        };
        assert_eq!(*span as usize, size_of::<SurfaceUniform>());
        for (name, offset) in [
            ("flags", offset_of!(SurfaceUniform, flags)),
            ("modulation", offset_of!(SurfaceUniform, modulation)),
            (
                "model_modulation",
                offset_of!(SurfaceUniform, model_modulation),
            ),
            ("base_s", offset_of!(SurfaceUniform, base_s)),
            ("base_t", offset_of!(SurfaceUniform, base_t)),
            ("detail_s", offset_of!(SurfaceUniform, detail_s)),
            ("detail_t", offset_of!(SurfaceUniform, detail_t)),
            ("detail", offset_of!(SurfaceUniform, detail)),
            ("detail_tint", offset_of!(SurfaceUniform, detail_tint)),
            ("emission", offset_of!(SurfaceUniform, emission)),
            (
                "emission_fresnel",
                offset_of!(SurfaceUniform, emission_fresnel),
            ),
            ("blend_s", offset_of!(SurfaceUniform, blend_s)),
            ("blend_t", offset_of!(SurfaceUniform, blend_t)),
            ("blend", offset_of!(SurfaceUniform, blend)),
            ("state", offset_of!(SurfaceUniform, state)),
            ("misc", offset_of!(SurfaceUniform, misc)),
            ("env_flags", offset_of!(SurfaceUniform, env_flags)),
            ("env_tint", offset_of!(SurfaceUniform, env_tint)),
            ("env_params", offset_of!(SurfaceUniform, env_params)),
            ("env_mask_s", offset_of!(SurfaceUniform, env_mask_s)),
            ("env_mask_t", offset_of!(SurfaceUniform, env_mask_t)),
            ("alpha_state", offset_of!(SurfaceUniform, alpha_state)),
        ] {
            assert_eq!(
                members
                    .iter()
                    .find(|member| member.name.as_deref() == Some(name))
                    .unwrap()
                    .offset as usize,
                offset,
                "{name}"
            );
        }
    }
}
