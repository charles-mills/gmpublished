use super::super::{RenderMode, Uniforms, Vec3, ViewerScene};
use super::{Draw, Primitive, WaterSide, depth};

#[derive(Clone, Copy)]
pub(super) struct DetailProfile {
    pub fast_sprites: bool,
    pub fast_chunk_quads: usize,
}

impl DetailProfile {
    pub const SOURCE_LDR: Self = Self {
        fast_sprites: true,
        fast_chunk_quads: 32768 / 6,
    };
}

pub(super) struct DetailDraw {
    pub leaf_index: u16,
    pub fast_eligible: bool,
    pub distance_squared: f32,
    pub draw: Draw,
}

pub(super) fn radial_distance_squared(delta: Vec3, fast: bool) -> f32 {
    let squares = delta.to_array().map(|coordinate| coordinate * coordinate);
    if fast {
        (squares[0] + squares[1]) + squares[2]
    } else {
        squares[0] + (squares[1] + squares[2])
    }
}

pub(super) fn prepare(
    upload: &ViewerScene,
    uniforms: &Uniforms,
    visibility: &super::super::visibility::UploadedVisibility,
    skybox: bool,
    water_side: Option<WaterSide>,
) -> Vec<DetailDraw> {
    let sprites = if skybox {
        &upload.assets.map_skybox_detail_sprites
    } else {
        &upload.assets.detail_sprites
    };
    let Some(sprites) = sprites else {
        return Vec::new();
    };
    let stride = super::super::DETAIL_VERTEX_FLOAT_COUNT as usize * 4;
    let eye = Vec3::new(
        uniforms.camera_position[0],
        uniforms.camera_position[1],
        uniforms.camera_position[2],
    );
    let mut draws = Vec::new();
    for (index, bytes) in sprites.all_vertices.chunks_exact(6 * stride).enumerate() {
        if water_side
            .is_some_and(|side| sprites.underwater_leaves[index] != (side == WaterSide::Under))
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
        let radius = bytes
            .chunks_exact(stride)
            .map(|vertex| {
                let x = f32::from_le_bytes(vertex[12..16].try_into().unwrap());
                let y = f32::from_le_bytes(vertex[16..20].try_into().unwrap());
                x.hypot(y)
            })
            .fold(0.0_f32, f32::max);
        let distance_squared =
            radial_distance_squared(position - eye, sprites.fast_eligible[index]);
        if distance_squared > (1024.0 + radius).powi(2) {
            continue;
        }
        draws.push(DetailDraw {
            leaf_index: sprites.leaf_indices[index],
            fast_eligible: sprites.fast_eligible[index],
            distance_squared,
            draw: Draw {
                primitive: Primitive::Sprite(index as u32 * 6..(index as u32 + 1) * 6),
                depth: depth(uniforms, position),
                blend: RenderMode::Translucent,
            },
        });
    }
    draws
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_threshold_preserves_fast_and_ordinary_float_association() {
        let delta = Vec3::new(4096.0, 1.0, 1.0);
        assert_eq!(radial_distance_squared(delta, true), 16_777_216.0);
        assert_eq!(radial_distance_squared(delta, false), 16_777_218.0);
    }
}
