use crate::bridge::materials::SkyMaterial;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyUniform {
    pub kind: [u32; 4],
    pub top: [f32; 4],
    pub bottom: [f32; 4],
    pub dusk: [f32; 4],
    pub sun_direction: [f32; 4],
    pub sun_color: [f32; 4],
    pub stars: [f32; 4],
    pub tint: [f32; 4],
    pub texture_s: [f32; 4],
    pub texture_t: [f32; 4],
}

impl SkyUniform {
    pub fn new(material: &SkyMaterial) -> Self {
        let mut state = Self {
            kind: [0; 4],
            top: [0.0; 4],
            bottom: [0.0; 4],
            dusk: [0.0; 4],
            sun_direction: [0.0; 4],
            sun_color: [0.0; 4],
            stars: [0.0; 4],
            tint: [material.color[0], material.color[1], material.color[2], 1.0],
            texture_s: material.transform[0],
            texture_t: material.transform[1],
        };
        if let Some(paint) = &material.painted {
            state.kind[0] = 1;
            state.kind[1] = if paint.texture.is_some() {
                paint.star_layers.clamp(0, 3) as u32
            } else {
                0
            };
            state.top = [
                paint.top_color[0],
                paint.top_color[1],
                paint.top_color[2],
                paint.fade_bias,
            ];
            // g_sky forces HDRScale to one in the native LDR render path.
            state.bottom = [
                paint.bottom_color[0],
                paint.bottom_color[1],
                paint.bottom_color[2],
                1.0,
            ];
            state.dusk = [
                paint.dusk_color[0],
                paint.dusk_color[1],
                paint.dusk_color[2],
                paint.dusk_scale,
            ];
            state.sun_direction = [
                paint.sun_normal[0],
                paint.sun_normal[1],
                paint.sun_normal[2],
                paint.sun_size,
            ];
            state.sun_color = [
                paint.sun_color[0],
                paint.sun_color[1],
                paint.sun_color[2],
                paint.dusk_intensity,
            ];
            state.stars = [
                if paint.star_scale != 0.0 {
                    1.0 / paint.star_scale
                } else {
                    0.0
                },
                paint.star_fade,
                paint.star_speed,
                paint.star_position,
            ];
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::wgpu::naga;
    use std::mem::{offset_of, size_of};

    #[test]
    fn sky_material_gpu_layout_matches_shader() {
        let source = super::super::super::SKY_SHADER_SOURCE;
        let module = naga::front::wgsl::parse_str(source).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let layout = module
            .types
            .iter()
            .find_map(|(_, ty)| (ty.name.as_deref() == Some("SkyMaterial")).then_some(&ty.inner))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = layout else {
            panic!("SkyMaterial struct");
        };
        assert_eq!(*span as usize, size_of::<SkyUniform>());
        let fields = [
            ("kind", offset_of!(SkyUniform, kind)),
            ("top", offset_of!(SkyUniform, top)),
            ("bottom", offset_of!(SkyUniform, bottom)),
            ("dusk", offset_of!(SkyUniform, dusk)),
            ("sun_direction", offset_of!(SkyUniform, sun_direction)),
            ("sun_color", offset_of!(SkyUniform, sun_color)),
            ("stars", offset_of!(SkyUniform, stars)),
            ("tint", offset_of!(SkyUniform, tint)),
            ("texture_s", offset_of!(SkyUniform, texture_s)),
            ("texture_t", offset_of!(SkyUniform, texture_t)),
        ];
        assert_eq!(members.len(), fields.len());
        for (member, (name, offset)) in members.iter().zip(fields) {
            assert_eq!(member.name.as_deref(), Some(name));
            assert_eq!(member.offset as usize, offset);
        }
    }
}
