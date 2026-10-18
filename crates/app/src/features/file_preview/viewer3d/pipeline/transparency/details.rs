use super::super::{DetailSprite, RenderMode, Uniforms, Vec3, ViewerScene};
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
    let Some(map) = upload.assets.map() else {
        return Vec::new();
    };
    let (uploaded, sprites) = if skybox {
        (
            &upload.assets.map_skybox_detail_sprites,
            map.map_skybox_detail_sprites.as_slice(),
        )
    } else {
        (&upload.assets.detail_sprites, map.detail_sprites.as_slice())
    };
    if uploaded.as_ref().is_none_or(|uploaded| {
        upload.assets.material_render_modes[uploaded.material_index] == RenderMode::NoDraw
    }) {
        return Vec::new();
    }
    prepare_sprites(
        sprites,
        uniforms,
        visibility
            .plan
            .as_ref()
            .map(|plan| plan.detail_sprite_visible.as_slice()),
        water_side,
    )
}

fn sprite_radius(sprite: &DetailSprite) -> f32 {
    let [left, top] = sprite.upper_left;
    let [right, bottom] = sprite.lower_right;
    [
        (left, top),
        (right, top),
        (right, bottom),
        (left, top),
        (right, bottom),
        (left, bottom),
    ]
    .into_iter()
    .map(|(x, y)| x.hypot(y))
    .fold(0.0_f32, f32::max)
}

fn prepare_sprites(
    sprites: &[DetailSprite],
    uniforms: &Uniforms,
    visible: Option<&[bool]>,
    water_side: Option<WaterSide>,
) -> Vec<DetailDraw> {
    let eye = Vec3::new(
        uniforms.camera_position[0],
        uniforms.camera_position[1],
        uniforms.camera_position[2],
    );
    let mut draws = Vec::new();
    for (index, sprite) in sprites.iter().enumerate() {
        if water_side.is_some_and(|side| sprite.underwater_leaf != (side == WaterSide::Under)) {
            continue;
        }
        if visible.is_some_and(|visible| !visible.get(index).copied().unwrap_or(false)) {
            continue;
        }
        let position = sprite.origin;
        let radius = sprite_radius(sprite);
        let distance_squared = radial_distance_squared(position - eye, sprite.fast_eligible);
        if distance_squared > (1024.0 + radius).powi(2) {
            continue;
        }
        draws.push(DetailDraw {
            leaf_index: sprite.leaf_index,
            fast_eligible: sprite.fast_eligible,
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
    fn sprite() -> DetailSprite {
        DetailSprite {
            fast_eligible: true,
            leaf_index: 7,
            origin: Vec3::new(1.0, 2.0, 3.0),
            angles: Vec3::ZERO,
            orientation: 0,
            underwater_leaf: false,
            upper_left: [-3.0, 4.0],
            lower_right: [6.0, -8.0],
            tex_upper_left: [0.0, 0.0],
            tex_lower_right: [1.0, 1.0],
            material_index: 0,
            visibility: gmpublished_domain::scene::map::MapVisibilityBucket::Always,
        }
    }

    #[test]
    fn canonical_radius_matches_uploaded_corner_sequence() {
        let mut sprite = sprite();
        for corners in [
            ([-3.0, 4.0], [6.0, -8.0]),
            ([0.125, -1023.0], [2048.5, 0.25]),
        ] {
            sprite.upper_left = corners.0;
            sprite.lower_right = corners.1;
            let mut encoded = Vec::new();
            super::super::super::upload::push_detail_sprite_vertices(&mut encoded, &sprite);
            let radius = encoded
                .chunks_exact(14 * size_of::<f32>())
                .map(|vertex| {
                    let read =
                        |offset| f32::from_ne_bytes(vertex[offset..offset + 4].try_into().unwrap());
                    read(12).hypot(read(16))
                })
                .fold(0.0_f32, f32::max);
            assert_eq!(sprite_radius(&sprite).to_bits(), radius.to_bits());
        }
    }

    #[test]
    fn canonical_leaf_visibility_and_medium_filter_unchanged_ranges() {
        use bytemuck::Zeroable;
        let uniforms = Uniforms::zeroed();
        let dry = sprite();
        let mut wet = dry;
        wet.leaf_index = 91;
        wet.underwater_leaf = true;
        wet.fast_eligible = false;
        let mut distant = dry;
        distant.origin = Vec3::splat(4096.0);
        let sprites = [dry, wet, distant];
        let draws = prepare_sprites(&sprites, &uniforms, None, None);
        assert_eq!(
            draws
                .iter()
                .map(|draw| (draw.leaf_index, draw.fast_eligible))
                .collect::<Vec<_>>(),
            [(7, true), (91, false)]
        );
        assert!(matches!(&draws[1].draw.primitive, Primitive::Sprite(range) if *range== (6..12)));
        let wet = prepare_sprites(
            &sprites,
            &uniforms,
            Some(&[true, true, true]),
            Some(WaterSide::Under),
        );
        assert_eq!(wet.len(), 1);
        assert_eq!(wet[0].leaf_index, 91);
        assert!(
            prepare_sprites(
                &sprites,
                &uniforms,
                Some(&[false, true, true]),
                Some(WaterSide::Above)
            )
            .is_empty()
        );
        assert_eq!(sprites[0], dry);
    }
}
