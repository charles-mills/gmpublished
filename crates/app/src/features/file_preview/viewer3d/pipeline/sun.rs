use gmpublished_domain::{math::Vec3, scene::map::MapSun};

pub mod gpu;

#[derive(Clone, Copy, Debug)]
pub struct SunView {
    pub eye: Vec3,
    pub forward: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SunLayer {
    Core,
    Glow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunVertex {
    pub position: Vec3,
    pub uv: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunQuad {
    pub layer: SunLayer,
    pub vertices: [SunVertex; 4],
    /// Source mesh-builder UNORM color, before the sprite shader's color transfer.
    pub vertex_color: [f32; 4],
    pub hdr_color_scale: f32,
}

/// C_SunGlowOverlay geometry in Source world coordinates.
pub fn sun_quads(sun: &MapSun, view: SunView, visibility_fraction: f32) -> [Option<SunQuad>; 2] {
    if !sun.enabled || !visibility_fraction.is_finite() || visibility_fraction <= 0.0 {
        return [None; 2];
    }
    let Some(direction) = sun.direction_to_sun.normalize() else {
        return [None; 2];
    };
    let Some(forward) = view.forward.normalize() else {
        return [None; 2];
    };
    let visibility = visibility_fraction.min(1.0);
    let core_color = core_tint(sun.color_srgb);
    let overlay_color = if sun.overlay_color_srgb == [0; 3] {
        core_color
    } else {
        Vec3::from(
            sun.overlay_color_srgb
                .map(|component| f32::from(component) / 255.0),
        )
    };
    let glow_opacity = ((direction.dot(forward) - 0.9) * 7.5).clamp(0.0, 0.75);
    let core = quad(
        sun,
        view.eye,
        direction,
        SunLayer::Core,
        sun.size as f32,
        core_color * visibility,
    );
    let glow = quad(
        sun,
        view.eye,
        direction,
        SunLayer::Glow,
        sun.overlay_size as f32 * 6.0,
        overlay_color * (visibility * glow_opacity),
    );
    [core, glow]
}

fn core_tint(color: [u8; 3]) -> Vec3 {
    let maximum = color.into_iter().max().unwrap_or(0);
    if maximum == 0 {
        Vec3::splat(1.0)
    } else {
        Vec3::from(color.map(|component| f32::from(component) / f32::from(maximum)))
    }
}

fn quad(
    sun: &MapSun,
    eye: Vec3,
    direction: Vec3,
    layer: SunLayer,
    size: f32,
    color: Vec3,
) -> Option<SunQuad> {
    if size == 0.0 || color.length_squared() < 0.000_01 {
        return None;
    }
    // CGlowOverlay::CalcBasis uses world Z, including its degenerate pole.
    let right = direction.cross(Vec3::new(0.0, 0.0, 1.0)).normalize()?;
    let up = right.cross(direction).normalize()?;
    let right = right * size;
    let up = up * size;
    let center = eye + direction * 100.0;
    Some(SunQuad {
        layer,
        vertices: [
            SunVertex {
                position: center - right + up,
                uv: [0.0, 1.0],
            },
            SunVertex {
                position: center + right + up,
                uv: [1.0, 1.0],
            },
            SunVertex {
                position: center + right - up,
                uv: [1.0, 0.0],
            },
            SunVertex {
                position: center - right - up,
                uv: [0.0, 0.0],
            },
        ],
        vertex_color: [
            source_color_channel(color.x()),
            source_color_channel(color.y()),
            source_color_channel(color.z()),
            1.0,
        ],
        hdr_color_scale: sun.hdr_color_scale,
    })
}

fn source_color_channel(value: f32) -> f32 {
    // FastFToC adds 2^23 to obtain IEEE round-to-even bytes in the mantissa.
    let bits = (value.clamp(0.0, 1.0) * 255.0 + 8_388_608.0).to_bits();
    (bits & 255) as f32 / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sun() -> MapSun {
        MapSun {
            direction_to_sun: Vec3::new(1.0, 0.0, 0.0),
            color_srgb: [128, 64, 32],
            overlay_color_srgb: [0; 3],
            size: 16,
            overlay_size: 24,
            material: "sprites/core".to_owned(),
            overlay_material: "sprites/glow".to_owned(),
            hdr_color_scale: 1.5,
            enabled: true,
            start_disabled: false,
            spawn_flags: 0,
        }
    }

    fn view() -> SunView {
        SunView {
            eye: Vec3::new(10.0, 20.0, 30.0),
            forward: Vec3::new(1.0, 0.0, 0.0),
        }
    }

    #[test]
    fn core_preserves_hue_and_source_world_up_geometry() {
        let sun = sun();
        let [Some(core), Some(glow)] = sun_quads(&sun, view(), 1.0) else {
            panic!("visible sun should include both layers");
        };
        assert_eq!(core.layer, SunLayer::Core);
        assert_eq!(glow.layer, SunLayer::Glow);
        assert_eq!(core.vertices[0].position, Vec3::new(110.0, 36.0, 46.0));
        assert_eq!(core.vertices[2].position, Vec3::new(110.0, 4.0, 14.0));
        assert_eq!(glow.vertices[0].position, Vec3::new(110.0, 164.0, 174.0));
        assert_eq!(core.vertex_color, [1.0, 128.0 / 255.0, 64.0 / 255.0, 1.0]);
        assert_eq!(core.hdr_color_scale, 1.5);
        assert_eq!(
            core.vertices.map(|vertex| vertex.uv),
            [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]
        );
    }

    #[test]
    fn glow_uses_authored_overlay_tint_and_camera_angle_without_changing_core() {
        let mut sun = sun();
        sun.overlay_color_srgb = [32, 64, 128];
        let mut camera = view();
        camera.forward = Vec3::new(0.0, 1.0, 0.0);
        let [Some(core), None] = sun_quads(&sun, camera, 0.5) else {
            panic!("off-axis sun should omit only its glow");
        };
        assert_eq!(core.vertex_color[0], 128.0 / 255.0);
        let [_, Some(glow)] = sun_quads(&sun, view(), 1.0) else {
            panic!("sun-facing view should include the glow");
        };
        assert_eq!(
            glow.vertex_color,
            [24.0 / 255.0, 48.0 / 255.0, 96.0 / 255.0, 1.0]
        );
    }

    #[test]
    fn hidden_disabled_and_degenerate_suns_do_not_emit_quads() {
        let mut sun = sun();
        assert_eq!(sun_quads(&sun, view(), 0.0), [None; 2]);
        sun.enabled = false;
        assert_eq!(sun_quads(&sun, view(), 1.0), [None; 2]);
        sun.enabled = true;
        sun.direction_to_sun = Vec3::new(0.0, 0.0, 1.0);
        assert_eq!(sun_quads(&sun, view(), 1.0), [None; 2]);
        sun.direction_to_sun = Vec3::ZERO;
        assert_eq!(sun_quads(&sun, view(), 1.0), [None; 2]);
    }

    #[test]
    fn black_core_color_falls_back_to_white_and_zero_size_is_independent_per_layer() {
        let mut sun = sun();
        sun.color_srgb = [0; 3];
        sun.size = 0;
        let [None, Some(glow)] = sun_quads(&sun, view(), 1.0) else {
            panic!("independent overlay size should retain the glow");
        };
        assert_eq!(
            glow.vertex_color,
            [191.0 / 255.0, 191.0 / 255.0, 191.0 / 255.0, 1.0]
        );
    }
}
