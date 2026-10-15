use super::wgpu;
use crate::media::preview_model::{
    ModelLightKind, ModelLightingContext, ModelLightingHistory, ModelLocalLight,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ModelMaterialUniform {
    flags: [u32; 4],
    params: [f32; 4],
    fresnel: [f32; 4],
    tint: [f32; 4],
    rim: [f32; 4],
}

impl ModelMaterialUniform {
    pub(super) fn new(surface: Option<&crate::bridge::materials::SurfaceMaterial>) -> Self {
        let phong = surface.and_then(|surface| surface.phong.as_deref());
        let enabled =
            surface.is_some_and(crate::bridge::materials::SurfaceMaterial::uses_skin_shader);
        let texture_available = |texture: Option<&crate::bridge::materials::PhongTexture>| {
            texture.is_some_and(|texture| texture.texture.is_some())
        };
        let diffuse_warp = phong
            .is_some_and(|phong| texture_available(phong.diffuse_warp.as_ref()))
            && (enabled
                || surface.is_none_or(|surface| surface.effective_emission_mode(true) != 3));
        let specular_warp =
            phong.is_some_and(|phong| texture_available(phong.specular_warp.as_ref()));
        let exponent_map =
            phong.is_some_and(|phong| texture_available(phong.exponent_texture.as_ref()));
        let half_lambert = enabled || phong.is_some_and(|phong| phong.half_lambert == Some(true));
        let base_mask = phong.is_some_and(|phong| phong.base_alpha_mask == Some(true));
        let fresnel = phong
            .and_then(|phong| phong.fresnel_ranges)
            .unwrap_or([0.0; 3]);
        let tint = phong
            .and_then(|phong| phong.tint)
            .filter(|tint| *tint != [0.0; 3])
            .unwrap_or_else(|| {
                if exponent_map && phong.is_some_and(|phong| phong.albedo_tint == Some(true)) {
                    [-1.0; 3]
                } else {
                    [1.0; 3]
                }
            });
        Self {
            flags: [
                u32::from(enabled),
                u32::from(surface.is_some_and(|surface| surface.normal.is_some())),
                u32::from(diffuse_warp)
                    | (u32::from(specular_warp) << 1)
                    | (u32::from(exponent_map) << 2)
                    | (u32::from(half_lambert) << 3)
                    | (u32::from(base_mask) << 4)
                    | (u32::from(phong.is_some_and(|phong| phong.invert_mask == Some(true))) << 5),
                u32::from(enabled && phong.is_some_and(|phong| phong.rim.enabled == Some(true))),
            ],
            params: [
                phong
                    .and_then(|phong| phong.exponent)
                    .filter(|v| *v > 0.0)
                    .unwrap_or(-1.0),
                0.0,
                phong.and_then(|phong| phong.boost).unwrap_or(1.0),
                0.0,
            ],
            fresnel: [fresnel[0], fresnel[1], fresnel[2], 0.0],
            tint: [tint[0], tint[1], tint[2], 0.0],
            rim: [
                phong
                    .and_then(|phong| phong.rim.exponent)
                    .unwrap_or(0.0)
                    .max(1.0),
                phong.and_then(|phong| phong.rim.boost).unwrap_or(0.0),
                f32::from(phong.is_some_and(|phong| phong.rim.exponent_alpha_mask == Some(true))),
                0.0,
            ],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuModelLight {
    position_kind: [f32; 4],
    color: [f32; 4],
    direction: [f32; 4],
    attenuation: [f32; 4],
    spot: [f32; 4],
}

impl From<&ModelLocalLight> for GpuModelLight {
    fn from(light: &ModelLocalLight) -> Self {
        Self {
            position_kind: [
                light.position[0],
                light.position[1],
                light.position[2],
                match light.kind {
                    ModelLightKind::Directional => 0.0,
                    ModelLightKind::Point => 1.0,
                    ModelLightKind::Spot => 2.0,
                },
            ],
            color: [light.radiance[0], light.radiance[1], light.radiance[2], 0.0],
            direction: [
                light.direction[0],
                light.direction[1],
                light.direction[2],
                0.0,
            ],
            attenuation: [
                light.attenuation[0],
                light.attenuation[1],
                light.attenuation[2],
                light.range,
            ],
            spot: [
                light.spot_exponent,
                light.inner_cosine,
                light.outer_cosine,
                1.0 / (light.inner_cosine - light.outer_cosine).max(0.00001),
            ],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuModelContext {
    ambient: [[f32; 4]; 6],
    lights: [GpuModelLight; 4],
    counts: [u32; 4],
}

pub(super) fn encode(
    contexts: &[ModelLightingContext],
    styles: &[f32; 128],
    time: f64,
    mut histories: Option<&mut [ModelLightingHistory]>,
    active_contexts: Option<&[bool]>,
) -> Vec<GpuModelContext> {
    if let Some(histories) = &histories {
        assert_eq!(
            contexts.len(),
            histories.len(),
            "lighting history context identity changed"
        );
    }
    if let Some(active) = active_contexts {
        assert_eq!(
            contexts.len(),
            active.len(),
            "lighting visibility context identity changed"
        );
    }
    let mut data = Vec::with_capacity(contexts.len() + 1);
    data.push(GpuModelContext::default());
    for (index, context) in contexts.iter().enumerate() {
        let history = histories
            .as_deref_mut()
            .filter(|_| active_contexts.is_none_or(|active| active[index]));
        let evaluated = history.map_or_else(
            || context.evaluate(styles),
            |histories| context.evaluate_with_history(styles, time, &mut histories[index]),
        );
        let (ambient, lights) = match &evaluated {
            Ok(Some((ambient, lights))) => (ambient, lights.as_slice()),
            _ => (&context.ambient_cube, context.lights.as_slice()),
        };
        assert!(
            lights.len() <= 4,
            "model context exceeds native local-light slots"
        );
        let mut item = GpuModelContext {
            ambient: ambient.colors.map(|rgb| [rgb[0], rgb[1], rgb[2], 0.0]),
            counts: [lights.len() as u32, 0, 0, 0],
            ..GpuModelContext::default()
        };
        for (target, light) in item.lights.iter_mut().zip(lights) {
            *target = light.into();
            if light.kind == ModelLightKind::Directional {
                // ShaderAPI uploads a finite light position to pixel shaders;
                // its vertex-light constants use the direction independently.
                let position = context.lighting_origin - light.direction * 10_000.0;
                target.position_kind[..3].copy_from_slice(&position.to_array());
            }
        }
        data.push(item);
    }
    data
}

pub(super) fn upload(device: &wgpu::Device, data: &[GpuModelContext]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("file_preview.model_lighting.contexts"),
        contents: bytemuck::cast_slice(data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::wgpu::naga;
    #[test]
    fn hidden_contexts_keep_history_until_their_next_visible_draw() {
        let mut contexts = [
            ModelLightingContext::ambient_fixture(1.0),
            ModelLightingContext::ambient_fixture(1.0),
        ];
        let mut histories = [
            ModelLightingHistory::default(),
            ModelLightingHistory::default(),
        ];
        encode(
            &contexts,
            &[1.0; 128],
            0.0,
            Some(&mut histories),
            Some(&[true, false]),
        );
        assert_ne!(histories[0], ModelLightingHistory::default());
        assert_eq!(histories[1], ModelLightingHistory::default());
        let first_history = histories[0].clone();
        contexts = [
            ModelLightingContext::ambient_fixture(0.0),
            ModelLightingContext::ambient_fixture(0.0),
        ];
        encode(
            &contexts,
            &[1.0; 128],
            0.2,
            Some(&mut histories),
            Some(&[false, true]),
        );
        assert_eq!(histories[0], first_history);
        assert_ne!(histories[1], ModelLightingHistory::default());
        let hidden_history = histories[1].clone();
        contexts[1] = ModelLightingContext::ambient_fixture(1.0);
        let data = encode(
            &contexts,
            &[1.0; 128],
            0.4,
            Some(&mut histories),
            Some(&[true, false]),
        );
        // The first context last drew at0.0, so its filter covers the full0.4
        // seconds. The second context still returns current instantaneous data
        // without advancing its hidden renderable history.
        assert_eq!(data[1].ambient[0][0], (-2.0_f32).exp());
        assert_eq!(data[2].ambient[0][0], 1.0);
        assert_eq!(histories[1], hidden_history);
    }

    #[test]
    fn placement_storage_and_material_uniform_match_wgsl_layouts() {
        use std::mem::{offset_of, size_of};
        let module =
            naga::front::wgsl::parse_str(super::super::super::MODEL_SHADER_SOURCE).unwrap();
        for (name, expected, offsets) in [
            (
                "ModelLightingContext",
                size_of::<GpuModelContext>(),
                vec![
                    offset_of!(GpuModelContext, ambient),
                    offset_of!(GpuModelContext, lights),
                    offset_of!(GpuModelContext, counts),
                ],
            ),
            (
                "ModelLocalLight",
                size_of::<GpuModelLight>(),
                vec![
                    offset_of!(GpuModelLight, position_kind),
                    offset_of!(GpuModelLight, color),
                    offset_of!(GpuModelLight, direction),
                    offset_of!(GpuModelLight, attenuation),
                    offset_of!(GpuModelLight, spot),
                ],
            ),
            (
                "ModelMaterial",
                size_of::<ModelMaterialUniform>(),
                vec![
                    offset_of!(ModelMaterialUniform, flags),
                    offset_of!(ModelMaterialUniform, params),
                    offset_of!(ModelMaterialUniform, fresnel),
                    offset_of!(ModelMaterialUniform, tint),
                    offset_of!(ModelMaterialUniform, rim),
                ],
            ),
        ] {
            let inner = &module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap()
                .1
                .inner;
            let naga::TypeInner::Struct { members, span } = inner else {
                panic!("GPU struct");
            };
            assert_eq!(*span as usize, expected, "{name} size");
            assert_eq!(
                members
                    .iter()
                    .map(|member| member.offset as usize)
                    .collect::<Vec<_>>(),
                offsets,
                "{name} offsets"
            );
        }
    }
}
