use super::{
    MapEntity, normalize_source_path, parse_entity_bool, parse_entity_float, parse_entity_i32,
    parse_entity_vec3,
};
use crate::math::Vec3;

/// Garry's Mod's networked painted-sky state after the initial entity think.
#[derive(Clone, Debug, PartialEq)]
pub struct MapSkyPaint {
    pub top_color: Vec3,
    pub bottom_color: Vec3,
    pub fade_bias: f32,
    pub sun_normal: Vec3,
    pub authored_sun_normal: Vec3,
    /// Retained for diagnostics; the Lua entity currently ignores this key.
    pub sun_position_method: Option<i32>,
    pub sun_color: Vec3,
    pub sun_size: f32,
    pub dusk_color: Vec3,
    pub dusk_scale: f32,
    pub dusk_intensity: f32,
    pub draw_stars: bool,
    pub star_texture: String,
    pub star_layers: i32,
    pub star_scale: f32,
    pub star_fade: f32,
    pub star_speed: f32,
    pub hdr_scale: f32,
}

impl MapSkyPaint {
    /// NetworkVarElement stores star parameters in a 16-bit networked QAngle.
    pub fn client_star_scale(&self) -> f32 {
        network_angle(self.star_scale)
    }

    pub fn client_star_fade(&self) -> f32 {
        network_angle(self.star_fade)
    }

    pub fn client_star_speed(&self) -> f32 {
        network_angle(self.star_speed)
    }
}

fn network_angle(value: f32) -> f32 {
    let packed = ((value * 65_536.0 / 360.0) as i64).rem_euclid(65_536);
    packed as f32 * (360.0 / 65_536.0)
}

impl Default for MapSkyPaint {
    fn default() -> Self {
        Self {
            top_color: Vec3::new(0.2, 0.5, 1.0),
            bottom_color: Vec3::new(0.8, 1.0, 1.0),
            fade_bias: 1.0,
            sun_normal: Vec3::new(0.4, 0.0, 0.01),
            authored_sun_normal: Vec3::new(0.4, 0.0, 0.01),
            sun_position_method: None,
            sun_color: Vec3::new(0.2, 0.1, 0.0),
            sun_size: 2.0,
            dusk_color: Vec3::new(1.0, 0.2, 0.0),
            dusk_scale: 1.0,
            dusk_intensity: 1.0,
            draw_stars: true,
            star_texture: "skybox/starfield".to_owned(),
            star_layers: 1,
            star_scale: 0.5,
            star_fade: 1.5,
            star_speed: 0.01,
            hdr_scale: 0.66,
        }
    }
}

/// Visible sun overlays, independent of the baked environment light direction.
#[derive(Clone, Debug, PartialEq)]
pub struct MapSun {
    pub direction_to_sun: Vec3,
    pub color_srgb: [u8; 3],
    pub overlay_color_srgb: [u8; 3],
    pub size: i32,
    pub overlay_size: i32,
    pub material: String,
    pub overlay_material: String,
    pub hdr_color_scale: f32,
    pub enabled: bool,
    pub start_disabled: bool,
    pub spawn_flags: i32,
}

pub(super) fn map_sky_paint(entities: &[MapEntity], suns: &[MapSun]) -> Option<MapSkyPaint> {
    let entity = entities
        .iter()
        .find(|entity| entity.is_class("env_skypaint"))?;
    let mut paint = MapSkyPaint::default();
    for (key, output) in [
        ("topcolor", &mut paint.top_color),
        ("bottomcolor", &mut paint.bottom_color),
        ("sunnormal", &mut paint.authored_sun_normal),
        ("suncolor", &mut paint.sun_color),
        ("duskcolor", &mut paint.dusk_color),
    ] {
        if let Some(value) = entity.prop(key).and_then(parse_entity_vec3) {
            *output = value;
        }
    }
    for (key, output) in [
        ("fadebias", &mut paint.fade_bias),
        ("sunsize", &mut paint.sun_size),
        ("duskscale", &mut paint.dusk_scale),
        ("duskintensity", &mut paint.dusk_intensity),
        ("starscale", &mut paint.star_scale),
        ("starfade", &mut paint.star_fade),
        ("starspeed", &mut paint.star_speed),
        ("hdrscale", &mut paint.hdr_scale),
    ] {
        if let Some(value) = entity.prop(key).and_then(parse_entity_float) {
            *output = value;
        }
    }
    if let Some(value) = entity.prop("drawstars").and_then(parse_entity_bool) {
        paint.draw_stars = value;
    }
    if let Some(value) = entity.prop("starlayers").and_then(parse_entity_i32) {
        paint.star_layers = value;
    }
    if let Some(value) = entity.prop("startexture") {
        paint.star_texture = normalize_source_path(value, Some(".vtf")).unwrap_or_default();
    }
    paint.sun_position_method = entity.prop("sunposmethod").and_then(parse_entity_i32);
    // env_skypaint:Think unconditionally takes the first env_sun's direction,
    // even when sunposmethod is zero or the painted sun itself has zero size.
    paint.sun_normal = suns
        .first()
        .map_or(paint.authored_sun_normal, |sun| sun.direction_to_sun);
    Some(paint)
}

pub(super) fn map_suns(entities: &[MapEntity]) -> Vec<MapSun> {
    entities
        .iter()
        .filter(|entity| entity.is_class("env_sun"))
        .map(|entity| {
            let size = entity.prop("size").and_then(parse_entity_i32).unwrap_or(16);
            let overlay_size = entity
                .prop("overlaysize")
                .and_then(parse_entity_i32)
                .unwrap_or(-1);
            MapSun {
                direction_to_sun: sun_direction(entity, entities),
                color_srgb: entity
                    .prop("rendercolor")
                    .and_then(sun_color)
                    .unwrap_or([255; 3]),
                overlay_color_srgb: entity
                    .prop("overlaycolor")
                    .and_then(sun_color)
                    .unwrap_or([0; 3]),
                size,
                overlay_size: if overlay_size == -1 {
                    size
                } else {
                    overlay_size
                },
                material: sun_material(entity.prop("material")),
                overlay_material: sun_material(entity.prop("overlaymaterial")),
                hdr_color_scale: entity
                    .prop("hdrcolorscale")
                    .and_then(parse_entity_float)
                    .unwrap_or(0.0),
                enabled: true,
                start_disabled: entity
                    .prop("startdisabled")
                    .and_then(parse_entity_bool)
                    .unwrap_or(false),
                spawn_flags: entity
                    .prop("spawnflags")
                    .and_then(parse_entity_i32)
                    .unwrap_or(0),
            }
        })
        .collect()
}

fn sun_material(value: Option<&str>) -> String {
    value
        .and_then(|value| normalize_source_path(value, Some(".vmt")))
        .unwrap_or_else(|| "sprites/light_glow02_add_noz".to_owned())
}

fn sun_color(value: &str) -> Option<[u8; 3]> {
    let mut parts = value.split_ascii_whitespace().map(str::parse::<u8>);
    let color = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    // FIELD_COLOR32 also accepts an alpha component; overlays use RGB only.
    if let Some(alpha) = parts.next() {
        alpha.ok()?;
    }
    parts.next().is_none().then_some(color)
}

fn sun_direction(entity: &MapEntity, entities: &[MapEntity]) -> Vec3 {
    if entity
        .prop("use_angles")
        .and_then(parse_entity_bool)
        .unwrap_or(false)
    {
        let angles = entity
            .prop("angles")
            .and_then(parse_entity_vec3)
            .unwrap_or(Vec3::ZERO);
        let angle = entity
            .prop("angle")
            .and_then(parse_entity_float)
            .unwrap_or(0.0);
        let pitch = entity
            .prop("pitch")
            .and_then(parse_entity_float)
            .filter(|pitch| *pitch != 0.0)
            .unwrap_or_else(|| angles.x())
            .to_radians();
        let yaw = if angle == 0.0 { angles.y() } else { angle }.to_radians();
        // SetupLightNormalFromProps overwrites the special angle's Z value
        // with sin(pitch); CSun::Activate then negates the entire vector.
        let (x, y) = if angle == -1.0 || angle == -2.0 {
            (0.0, 0.0)
        } else {
            (yaw.cos(), yaw.sin())
        };
        return -Vec3::new(x * pitch.cos(), y * pitch.cos(), pitch.sin());
    }
    if let Some(target) = entity.prop("target").and_then(|name| {
        entities
            .iter()
            .find(|candidate| candidate.prop("targetname") == Some(name))
    }) {
        let origin = entity
            .prop("origin")
            .and_then(parse_entity_vec3)
            .unwrap_or(Vec3::ZERO);
        let target_origin = target
            .prop("origin")
            .and_then(parse_entity_vec3)
            .unwrap_or(Vec3::ZERO);
        return (origin - target_origin).normalize_or_zero();
    }
    Vec3::new(0.0, 0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::map::{
        load_map,
        tests::{bsp_fixture_with_entities, parse_test_entities},
    };

    #[test]
    fn painted_sky_requires_entity_and_retains_lua_defaults() {
        assert_eq!(map_sky_paint(&[], &[]), None);
        let map = load_map(&bsp_fixture_with_entities(
            r#"{ "classname" "env_skypaint" }"#,
        ))
        .unwrap();
        assert_eq!(map.sky_paint, Some(MapSkyPaint::default()));
        let paint = map.sky_paint.unwrap();
        assert_eq!(paint.top_color, Vec3::new(0.2, 0.5, 1.0));
        assert_eq!(paint.bottom_color, Vec3::new(0.8, 1.0, 1.0));
        assert_eq!((paint.fade_bias, paint.hdr_scale), (1.0, 0.66));
        assert_eq!(paint.sun_normal, Vec3::new(0.4, 0.0, 0.01));
        assert_eq!(paint.sun_color, Vec3::new(0.2, 0.1, 0.0));
        assert_eq!(paint.sun_size, 2.0);
        assert_eq!(paint.dusk_color, Vec3::new(1.0, 0.2, 0.0));
        assert_eq!((paint.dusk_scale, paint.dusk_intensity), (1.0, 1.0));
        assert!(paint.draw_stars);
        assert_eq!(paint.star_texture, "skybox/starfield");
        assert_eq!(paint.star_layers, 1);
        assert_eq!(
            (paint.star_scale, paint.star_fade, paint.star_speed),
            (0.5, 1.5, 0.01)
        );
        assert!(map.suns.is_empty());
    }

    #[test]
    fn painted_sky_retains_authored_values_without_editor_range_clamps() {
        let entities = parse_test_entities(
            r#"{
            "classname" "env_skypaint"
            "topcolor" "0 0.6 1" "bottomcolor" "0.9 0.95 1" "fadebias" "0"
            "sunnormal" "0 0 1" "suncolor" "0 0 0" "sunsize" "0"
            "duskcolor" "1 2 3" "duskscale" "0" "duskintensity" "11"
            "drawstars" "0" "starlayers" "3" "startexture" "Materials\\Skybox\\Clouds.VTF"
            "starscale" "2" "starfade" "0.5" "starspeed" "0.02" "hdrscale" "0.60"
            "sunposmethod" "1"
        }"#,
        );
        let paint = map_sky_paint(&entities, &[]).unwrap();
        assert_eq!(paint.top_color, Vec3::new(0.0, 0.6, 1.0));
        assert_eq!(paint.bottom_color, Vec3::new(0.9, 0.95, 1.0));
        assert_eq!(paint.fade_bias, 0.0);
        assert_eq!(paint.sun_normal, Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(paint.authored_sun_normal, paint.sun_normal);
        assert_eq!(paint.sun_color, Vec3::ZERO);
        assert_eq!(paint.sun_size, 0.0);
        assert_eq!(paint.dusk_color, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(paint.dusk_scale, 0.0);
        assert_eq!(paint.dusk_intensity, 11.0);
        assert!(!paint.draw_stars);
        assert_eq!(paint.star_layers, 3);
        assert_eq!(paint.star_texture, "skybox/clouds");
        assert_eq!(
            (paint.star_scale, paint.star_fade, paint.star_speed),
            (2.0, 0.5, 0.02)
        );
        assert_eq!(paint.hdr_scale, 0.6);
        assert_eq!(paint.sun_position_method, Some(1));
    }

    #[test]
    fn malformed_sky_fields_fall_back_independently_and_empty_texture_survives() {
        let entities = parse_test_entities(
            r#"{
            "classname" "env_skypaint" "topcolor" "0 nan 1" "bottomcolor" "1 2 3"
            "fadebias" "inf" "sunnormal" "1 2" "suncolor" "-inf 0 0"
            "sunsize" "nan" "duskintensity" "11" "starlayers" "3.5" "drawstars" "nan"
            "startexture" "" "starspeed" "-inf"
        }"#,
        );
        let paint = map_sky_paint(&entities, &[]).unwrap();
        let expected = MapSkyPaint {
            bottom_color: Vec3::new(1.0, 2.0, 3.0),
            dusk_intensity: 11.0,
            star_texture: String::new(),
            ..MapSkyPaint::default()
        };
        assert_eq!(paint, expected);
    }

    #[test]
    fn star_parameters_retain_authored_values_and_match_native_angle_quantization() {
        let entities = parse_test_entities(
            r#"{
            "classname" "env_skypaint" "starscale" "2" "starfade" "0.5" "starspeed" "0.03"
        }"#,
        );
        let paint = map_sky_paint(&entities, &[]).unwrap();
        assert_eq!(
            (paint.star_scale, paint.star_fade, paint.star_speed),
            (2.0, 0.5, 0.03)
        );
        assert_eq!(paint.client_star_scale(), 1.999_511_7);
        assert_eq!(paint.client_star_fade(), 0.499_877_93);
        assert_eq!(paint.client_star_speed(), 0.027_465_82);
        let defaults = MapSkyPaint::default();
        assert_eq!(defaults.client_star_scale(), 0.499_877_93);
        assert_eq!(defaults.client_star_fade(), 1.499_633_8);
        assert_eq!(defaults.client_star_speed(), 0.005_493_164);
    }

    #[test]
    fn first_visible_sun_overrides_paint_even_with_custom_method_and_zero_size() {
        let entities = parse_test_entities(
            r#"
            { "classname" "env_skypaint" "sunnormal" "0 0 1" "sunposmethod" "0" "sunsize" "0" }
            { "classname" "light_environment" "pitch" "-60" "angles" "0 306 0" "_light" "255 255 255 250" }
            { "classname" "env_sun" "use_angles" "1" "pitch" "-50" "angles" "0 306 0" }
            { "classname" "env_sun" "use_angles" "1" "pitch" "-25" "angles" "0 90 0" }
        "#,
        );
        let suns = map_suns(&entities);
        assert_eq!(suns.len(), 2);
        let paint = map_sky_paint(&entities, &suns).unwrap();
        let expected = Vec3::new(-0.377_821_12, 0.520_026, 0.766_044_5);
        assert!((paint.sun_normal - expected).length_squared() < 1e-12);
        assert_eq!(paint.authored_sun_normal, Vec3::new(0.0, 0.0, 1.0));
        assert_ne!(
            paint.sun_normal,
            crate::scene::map::map_environment_lighting(&entities)
                .unwrap()
                .sun
                .unwrap()
                .direction_to_sun
        );
    }

    #[test]
    fn visible_sun_preserves_authored_material_color_size_and_state_metadata() {
        let entities = parse_test_entities(
            r#"{
            "classname" "env_sun" "use_angles" "1" "pitch" "-50" "angles" "0 306 0"
            "rendercolor" "241 240 199 255" "overlaycolor" "244 242 236 0"
            "size" "24" "overlaysize" "-1" "material" "Materials/Sprites/Core.VMT"
            "overlaymaterial" "Sprites/Glow" "HDRColorScale" "1.5"
            "startdisabled" "1" "spawnflags" "4"
        }"#,
        );
        let sun = map_suns(&entities).remove(0);
        assert_eq!(sun.color_srgb, [241, 240, 199]);
        assert_eq!(sun.overlay_color_srgb, [244, 242, 236]);
        assert_eq!((sun.size, sun.overlay_size), (24, 24));
        assert_eq!(sun.material, "sprites/core");
        assert_eq!(sun.overlay_material, "sprites/glow");
        assert_eq!(sun.hdr_color_scale, 1.5);
        assert!(sun.start_disabled);
        assert_eq!(sun.spawn_flags, 4);
        // CSun does not bind StartDisabled or use a spawn flag to change m_bOn.
        assert!(sun.enabled);
    }

    #[test]
    fn sun_direction_resolves_target_and_sdk_zero_angle_overrides() {
        let entities = parse_test_entities(
            r#"
            { "classname" "info_target" "targetname" "sun_target" "origin" "10 20 20" }
            { "classname" "env_sun" "target" "sun_target" "origin" "10 20 30" }
            { "classname" "env_sun" "use_angles" "1" "angles" "-30 90 0" "pitch" "0" "angle" "0" }
            { "classname" "env_sun" "use_angles" "1" "angles" "-30 90 0" "pitch" "-90" "angle" "180" }
            { "classname" "env_sun" "target" "missing" }
        "#,
        );
        let suns = map_suns(&entities);
        assert_eq!(suns[0].direction_to_sun, Vec3::new(0.0, 0.0, 1.0));
        assert!(
            (suns[1].direction_to_sun - Vec3::new(0.0, -0.866_025_4, 0.5)).length_squared() < 1e-12
        );
        assert!((suns[2].direction_to_sun - Vec3::new(0.0, 0.0, 1.0)).length_squared() < 1e-12);
        assert_eq!(suns[3].direction_to_sun, Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(suns[3].material, "sprites/light_glow02_add_noz");
        assert_eq!(suns[3].overlay_color_srgb, [0; 3]);
        assert_eq!(suns[3].hdr_color_scale, 0.0);
    }
}
