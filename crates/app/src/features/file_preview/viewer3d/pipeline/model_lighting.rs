use super::wgpu;
const DIRECTIONAL_PIXEL_DISTANCE: f32 = 10_000.0;
const MODEL_MATERIAL_RIM: u32 = 1;
const MODEL_MATERIAL_PIXEL_LIGHTING: u32 = 2;
const MODEL_DIFFUSE_WARP: u32 = 1;
const MODEL_SPECULAR_WARP: u32 = 2;
const MODEL_EXPONENT_MAP: u32 = 4;
const MODEL_HALF_LAMBERT: u32 = 8;
const MODEL_BASE_ALPHA_MASK: u32 = 16;
const MODEL_INVERT_MASK: u32 = 32;
mod dependencies;
mod state;
use crate::media::preview_model::{
    ModelLightKind, ModelLightingContext, ModelLightingHistory, ModelLocalLight,
};
pub(super) use dependencies::UsedStyles;
pub(super) use state::State;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ModelMaterialUniform {
    skin: u32,
    normal: u32,
    features: u32,
    lighting: u32,
    exponent: f32,
    reserved0: f32,
    boost: f32,
    reserved1: f32,
    fresnel: [f32; 4],
    tint: [f32; 4],
    rim: [f32; 4],
}

impl ModelMaterialUniform {
    pub(super) fn new(surface: Option<&crate::bridge::materials::SurfaceMaterial>) -> Self {
        let phong = surface.and_then(|surface| surface.phong.as_deref());
        let profile: crate::bridge::materials::ModelShadingProfile = surface.map_or_else(
            Default::default,
            crate::bridge::materials::SurfaceMaterial::model_shading_profile,
        );
        let enabled = profile.skin;
        let exponent_map = profile.exponent_map;
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
            skin: u32::from(enabled),
            normal: u32::from(profile.normal),
            features: (u32::from(profile.diffuse_warp) * MODEL_DIFFUSE_WARP)
                | (u32::from(profile.specular_warp) * MODEL_SPECULAR_WARP)
                | (u32::from(exponent_map) * MODEL_EXPONENT_MAP)
                | (u32::from(profile.half_lambert) * MODEL_HALF_LAMBERT)
                | (u32::from(profile.base_alpha_mask) * MODEL_BASE_ALPHA_MASK)
                | (u32::from(profile.invert_mask) * MODEL_INVERT_MASK),
            lighting: (u32::from(profile.rim) * MODEL_MATERIAL_RIM)
                | (u32::from(profile.uses_pixel_lighting()) * MODEL_MATERIAL_PIXEL_LIGHTING),
            exponent: phong
                .and_then(|phong| phong.exponent)
                .filter(|value| *value > 0.0)
                .unwrap_or(-1.0),
            reserved0: 0.0,
            boost: phong.and_then(|phong| phong.boost).unwrap_or(1.0),
            reserved1: 0.0,
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
pub(super) struct GpuLightingState {
    ambient: [[f32; 4]; 6],
    lights: [GpuModelLight; crate::media::preview_model::MODEL_LOCAL_LIGHT_LIMIT],
    counts: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuModelContext {
    full: GpuLightingState,
    compiled_vertex: GpuLightingState,
}

fn encode_snapshot(
    context: &ModelLightingContext,
    snapshot: &crate::media::preview_model::ModelLightingSnapshot,
) -> GpuLightingState {
    let lights = snapshot.lights.as_slice();
    assert!(
        lights.len() <= crate::media::preview_model::MODEL_LOCAL_LIGHT_LIMIT,
        "model context exceeds native local-light slots"
    );
    let mut item = GpuLightingState {
        ambient: snapshot
            .ambient_cube
            .colors
            .map(|rgb| [rgb[0], rgb[1], rgb[2], 0.0]),
        counts: [lights.len() as u32, 0, 0, 0],
        ..GpuLightingState::default()
    };
    for (target, light) in item.lights.iter_mut().zip(lights) {
        *target = light.into();
        if light.kind == ModelLightKind::Directional {
            let position = context.lighting_origin - light.direction * DIRECTIONAL_PIXEL_DISTANCE;
            target.position_kind[..3].copy_from_slice(&position.to_array());
        }
    }
    item
}

pub(super) fn encode(
    contexts: &[ModelLightingContext],
    styles: &[f32; 128],
    time: f64,
    histories: Option<&mut [ModelLightingHistory]>,
    active_contexts: Option<&[bool]>,
) -> Vec<GpuModelContext> {
    let mut data = Vec::with_capacity(contexts.len() + 1);
    encode_into(
        contexts,
        styles,
        time,
        histories,
        active_contexts,
        &mut data,
    );
    data
}

pub(super) fn encode_into(
    contexts: &[ModelLightingContext],
    styles: &[f32; 128],
    time: f64,
    mut histories: Option<&mut [ModelLightingHistory]>,
    active_contexts: Option<&[bool]>,
    data: &mut Vec<GpuModelContext>,
) {
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
    data.clear();
    data.reserve(contexts.len() + 1);
    data.push(GpuModelContext::default());
    for (index, context) in contexts.iter().enumerate() {
        let history = histories
            .as_deref_mut()
            .filter(|_| active_contexts.is_none_or(|active| active[index]));
        data.push(encode_context(
            context,
            styles,
            time,
            history.map(|histories| &mut histories[index]),
        ));
    }
}

fn encode_context(
    context: &ModelLightingContext,
    styles: &[f32; 128],
    time: f64,
    history: Option<&mut ModelLightingHistory>,
) -> GpuModelContext {
    let evaluated = context.source.runtime_seed().map(|_| {
        history.map_or_else(
            || context.evaluate(styles),
            |history| context.evaluate_with_history(styles, time, history),
        )
    });
    let snapshot = match &evaluated {
        Some(Ok(snapshot)) => snapshot,
        Some(Err(error)) => {
            log::error!(
                "model lighting evaluation {:?}: {error:?}; using diagnosed fallback",
                context.model_path
            );
            context.source.fallback_snapshot()
        }
        None => context.source.fallback_snapshot(),
    };
    let compiled_vertex = context
        .evaluate_compiled_supplement(styles)
        .map(|snapshot| encode_snapshot(context, &snapshot))
        .unwrap_or_default();
    GpuModelContext {
        full: encode_snapshot(context, snapshot),
        compiled_vertex,
    }
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
        assert_eq!(data[1].full.ambient[0][0], (-2.0_f32).exp());
        assert_eq!(data[2].full.ambient[0][0], 1.0);
        assert_eq!(histories[1], hidden_history);
    }

    #[test]
    fn packed_vertex_lighting_flags_match_shader_constants() {
        use crate::media::preview_model::vertex_lighting;
        let module =
            naga::front::wgsl::parse_str(super::super::super::MODEL_SHADER_SOURCE).unwrap();
        for (name, expected) in [
            ("MODEL_DIFFUSE_WARP", MODEL_DIFFUSE_WARP),
            ("MODEL_SPECULAR_WARP", MODEL_SPECULAR_WARP),
            ("MODEL_EXPONENT_MAP", MODEL_EXPONENT_MAP),
            ("MODEL_HALF_LAMBERT", MODEL_HALF_LAMBERT),
            ("MODEL_BASE_ALPHA_MASK", MODEL_BASE_ALPHA_MASK),
            ("MODEL_INVERT_MASK", MODEL_INVERT_MASK),
            ("MODEL_MATERIAL_RIM", MODEL_MATERIAL_RIM),
            (
                "MODEL_MATERIAL_PIXEL_LIGHTING",
                MODEL_MATERIAL_PIXEL_LIGHTING,
            ),
            (
                "LIGHTING_LINEAR_ILLUMINATION",
                vertex_lighting::LINEAR_ILLUMINATION,
            ),
            ("LIGHTING_STATIC_PROP", vertex_lighting::STATIC_PROP),
            ("LIGHTING_RUNTIME_CONTEXT", vertex_lighting::RUNTIME_CONTEXT),
            ("LIGHTING_MODEL_DRAW", vertex_lighting::MODEL_DRAW),
            ("LIGHTING_LIGHTMAP", vertex_lighting::LIGHTMAP),
            (
                "LIGHTING_RECEIVER_LIGHTMAP",
                vertex_lighting::RECEIVER_LIGHTMAP,
            ),
            (
                "LIGHTING_RECEIVER_CONSTANT",
                vertex_lighting::RECEIVER_CONSTANT,
            ),
            ("LIGHTING_PREVIEW", vertex_lighting::PREVIEW),
            ("LIGHTING_COMPILED_VERTEX", vertex_lighting::COMPILED_VERTEX),
            ("LIGHTING_WORLD_CONSTANT", vertex_lighting::WORLD_CONSTANT),
            (
                "LIGHTING_BATCHED_STATIC_DECAL",
                vertex_lighting::BATCHED_STATIC_DECAL,
            ),
            (
                "LIGHTING_DIRECTIONAL_LIGHTMAP_BLACK",
                vertex_lighting::DIRECTIONAL_LIGHTMAP_BLACK,
            ),
            (
                "LIGHTING_LEGACY_VERTEX_COLOR",
                vertex_lighting::LEGACY_VERTEX_COLOR,
            ),
        ] {
            let (_, constant) = module
                .constants
                .iter()
                .find(|(_, constant)| constant.name.as_deref() == Some(name))
                .unwrap();
            assert!(
                matches!(module.global_expressions[constant.init], naga::Expression::Literal(naga::Literal::U32(value)) if value == expected),
                "{name}"
            );
        }
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
                    offset_of!(GpuModelContext, full),
                    offset_of!(GpuModelContext, compiled_vertex),
                ],
            ),
            (
                "ModelLightingState",
                size_of::<GpuLightingState>(),
                vec![
                    offset_of!(GpuLightingState, ambient),
                    offset_of!(GpuLightingState, lights),
                    offset_of!(GpuLightingState, counts),
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
                    offset_of!(ModelMaterialUniform, skin),
                    offset_of!(ModelMaterialUniform, normal),
                    offset_of!(ModelMaterialUniform, features),
                    offset_of!(ModelMaterialUniform, lighting),
                    offset_of!(ModelMaterialUniform, exponent),
                    offset_of!(ModelMaterialUniform, reserved0),
                    offset_of!(ModelMaterialUniform, boost),
                    offset_of!(ModelMaterialUniform, reserved1),
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
