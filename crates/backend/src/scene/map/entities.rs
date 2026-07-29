use super::{MapEntity, normalize_material_name, normalize_skyname};

const DEFAULT_DETAIL_MATERIAL: &str = "detail/detailsprites";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapPlayerStart {
    pub origin: [f32; 3],
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapFog {
    pub color_srgb: [u8; 3],
    pub start: f32,
    pub end: f32,
    pub max_density: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSkyCamera {
    pub origin: [f32; 3],
    pub scale: f32,
    pub fog: Option<MapFog>,
}

pub(super) fn worldspawn_detail_material_name(entities: &[MapEntity]) -> String {
    entities
        .iter()
        .find(|entity| entity.is_class("worldspawn"))
        .and_then(|entity| entity.prop("detailmaterial"))
        .and_then(normalize_material_name)
        .unwrap_or_else(|| DEFAULT_DETAIL_MATERIAL.to_owned())
}

pub(super) fn info_player_start(entities: &[MapEntity]) -> Option<MapPlayerStart> {
    entities
        .iter()
        .filter(|entity| entity.is_class("info_player_start"))
        .find_map(|entity| {
            Some(MapPlayerStart {
                origin: parse_entity_vec3(entity.prop("origin")?)?,
                angles: entity
                    .prop("angles")
                    .and_then(parse_entity_vec3)
                    .or_else(|| entity.prop("angle").and_then(parse_entity_yaw_angle))
                    .unwrap_or([0.0; 3]),
            })
        })
}

pub(super) fn map_sky_camera(entities: &[MapEntity]) -> Option<MapSkyCamera> {
    entities
        .iter()
        .filter(|entity| entity.is_class("sky_camera"))
        .find_map(|entity| {
            let origin = parse_entity_vec3(entity.prop("origin")?)?;
            let scale = entity
                .prop("scale")
                .and_then(parse_entity_float)
                .filter(|scale| *scale > 0.0)
                .unwrap_or(16.0);
            Some(MapSkyCamera {
                origin,
                scale,
                fog: parse_map_fog(
                    entity.prop("fogenable"),
                    entity.prop("fogcolor"),
                    entity.prop("fogstart"),
                    entity.prop("fogend"),
                    entity.prop("fogmaxdensity"),
                    sky_camera_fog_enabled,
                ),
            })
        })
}

pub(super) fn worldspawn_skyname(entities: &[MapEntity]) -> Option<String> {
    entities
        .iter()
        .find(|entity| entity.is_class("worldspawn"))
        .and_then(|entity| entity.prop("skyname"))
        .and_then(normalize_skyname)
}

pub(super) fn map_fog(entities: &[MapEntity]) -> Option<MapFog> {
    entities
        .iter()
        .find(|entity| {
            entity.is_class("env_fog_controller")
                && entity.prop("fogenable").is_some_and(fog_bool_enabled)
        })
        .and_then(|entity| {
            parse_map_fog(
                entity.prop("fogenable"),
                entity.prop("fogcolor"),
                entity.prop("fogstart"),
                entity.prop("fogend"),
                entity.prop("fogmaxdensity"),
                fog_bool_enabled,
            )
        })
}

fn parse_map_fog(
    fogenable: Option<&str>,
    fogcolor: Option<&str>,
    fogstart: Option<&str>,
    fogend: Option<&str>,
    fogmaxdensity: Option<&str>,
    enabled: fn(&str) -> bool,
) -> Option<MapFog> {
    fogenable.filter(|value| enabled(value))?;
    let fog = MapFog {
        color_srgb: parse_fog_color(fogcolor?)?,
        start: parse_fog_float(fogstart?)?,
        end: parse_fog_float(fogend?)?,
        max_density: fogmaxdensity
            .and_then(parse_fog_float)
            .filter(|density| (0.0..=1.0).contains(density))
            .unwrap_or(1.0),
    };
    (fog.end > fog.start).then_some(fog)
}

pub(super) fn parse_entity_vec3(value: &str) -> Option<[f32; 3]> {
    let mut components = value.split_ascii_whitespace().map(parse_entity_float);
    let x = components.next()??;
    let y = components.next()??;
    let z = components.next()??;
    components.next().is_none().then_some([x, y, z])
}

fn parse_entity_yaw_angle(value: &str) -> Option<[f32; 3]> {
    Some([0.0, parse_entity_float(value)?, 0.0])
}

pub(super) fn parse_entity_float(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

pub(super) fn parse_entity_i32(value: &str) -> Option<i32> {
    value.trim().parse::<i32>().ok()
}

pub(super) fn parse_entity_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        return Some(false);
    }
    parse_entity_i32(value).map(|value| value != 0)
}

fn fog_bool_enabled(value: &str) -> bool {
    value
        .trim()
        .parse::<f32>()
        .is_ok_and(|value| value.is_finite() && value != 0.0)
}

fn sky_camera_fog_enabled(value: &str) -> bool {
    value
        .trim()
        .parse::<f32>()
        .is_ok_and(|value| value.is_finite() && value == 1.0)
}

fn parse_fog_float(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_fog_color(value: &str) -> Option<[u8; 3]> {
    let mut components = value.split_ascii_whitespace().map(str::parse::<u8>).take(4);
    let red = components.next()?.ok()?;
    let green = components.next()?.ok()?;
    let blue = components.next()?.ok()?;
    components.next().is_none().then_some([red, green, blue])
}
