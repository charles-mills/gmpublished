use crate::bridge::materials::{SurfaceFamily, SurfaceMaterial};

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
    pub state: [u32; 4],
    pub misc: [f32; 4],
    pub env_flags: [u32; 4],
    pub env_tint: [f32; 4],
    pub env_params: [f32; 4],
    pub env_mask_s: [f32; 4],
    pub env_mask_t: [f32; 4],
    pub alpha_state: [u32; 4],
}

fn model_gamma(value: f32) -> f32 {
    if value > 1.0 {
        value
    } else {
        environment_gamma(value)
    }
}

// IMaterialVar's linear-vector path uses the bounded mathlib lookup,
// unlike general modulation constants which bypass conversion above one.
fn environment_gamma(value: f32) -> f32 {
    if value >= 0.95 {
        1.0
    } else {
        ((value.max(0.0) * 255.0).round_ties_even() / 255.0).powf(2.2)
    }
}

fn alpha_test_reference(authored: Option<f32>) -> f32 {
    // Generic helpers only override the inherited GEQUAL state for positive
    // references. ShaderAPI truncates the selected reference to an ALPHAREF byte.
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
            flags: [0; 4],
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
            state: [0; 4],
            misc: [0.5, 0.0, 0.0, 0.0],
            env_flags: [0; 4],
            env_tint: [1.0; 4],
            env_params: [0.0, 1.0, 1.0, 1.0],
            env_mask_s: identity[0],
            env_mask_t: identity[1],
            alpha_state: [1, 1, 0, 0],
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
        result.flags[0] = match surface.family {
            SurfaceFamily::Lightmapped => 1,
            SurfaceFamily::Transition => 2,
            SurfaceFamily::VertexLit => 3,
            SurfaceFamily::Unlit => 4,
        };
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
        result.base_s = surface.base_transform[0];
        result.base_t = surface.base_transform[1];
        if let Some(detail) = &surface.detail {
            result.flags[1] = surface.effective_detail_mode().map_or(0, |mode| mode + 1);
            let model = matches!(
                surface.family,
                SurfaceFamily::VertexLit | SurfaceFamily::Unlit
            );
            let mut transform = if model {
                detail.transform.unwrap_or(surface.base_transform)
            } else {
                surface.base_transform
            };
            // Source scales each affine column and its corresponding
            // translation, including translations authored before detail scale.
            for row in &mut transform {
                row[0] *= detail.scale[0];
                row[1] *= detail.scale[1];
            }
            transform[0][3] *= detail.scale[0];
            transform[1][3] *= detail.scale[1];
            result.detail_s = transform[0];
            result.detail_t = transform[1];
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
        result.flags[3] = u32::from(
            surface.blend_modulation.is_some() && surface.family == SurfaceFamily::Transition,
        );
        result.blend_s = surface.blend_mask_transform[0];
        result.blend_t = surface.blend_mask_transform[1];
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
                ) && !surface.uses_skin_shader()
                {
                    environment_gamma(envmap.tint[channel])
                } else {
                    envmap.tint[channel]
                };
            }
            result.env_params = [envmap.contrast, envmap.saturation, 1.0, envmap.fresnel];
            if surface.uses_skin_shader() {
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
                if result.flags[3] != 0 {
                    result.env_params[..2].copy_from_slice(&[0.0, 1.0]);
                }
            }
            if surface.family == SurfaceFamily::VertexLit
                && !surface.uses_skin_shader()
                && surface.normal.is_some()
                && envmap.mask != EnvironmentMapMask::NormalAlpha
                && (surface.authored_flag("$basealphaenvmapmask")
                    || surface.parameters.contains_key("$envmapmask"))
            {
                result.env_flags[0] = 0;
            }
            result.env_mask_s = envmap.mask_transform[0];
            result.env_mask_t = envmap.mask_transform[1];
        }
        let alpha_mask = surface.base_alpha_is_mask(base_has_alpha);
        result.state[0] = u32::from(surface.alpha_test && !alpha_mask);
        result.state[1] = u32::from(surface.masked_blending);
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
