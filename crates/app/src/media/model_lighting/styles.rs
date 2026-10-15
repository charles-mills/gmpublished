use gmpublished_domain::scene::map::MapLightStyleEntity;

const DEFAULT_PATTERNS: [&str; 13] = [
    "m",
    "mmnmmommommnonmmonqnmmo",
    "abcdefghijklmnopqrstuvwxyzyxwvutsrqponmlkjihgfedcba",
    "mmmmmaaaaammmmmaaaaaabcdefgabcdefg",
    "mamamamamama",
    "jklmnopqrstuvwxyzyxwvutsrqponmlkj",
    "nmonqnmomnmomomno",
    "mmmaaaabcdefgmmmmaaaammmaamm",
    "mmmaaammmaaammmabcdefaaaammmmabcdefmmmaaaa",
    "aaaaaaaazzzzzzzz",
    "mmamammmmammamamaaamammma",
    "abcdefghijklmnopqrrqponmlkjihgfedcba",
    "mmnnmmnnnmmnn",
];
const LEVEL_SCALE: f32 = f32::from_bits(0x3b78_3e10);

pub(super) fn model_light_style_values(entities: &[MapLightStyleEntity], time: f64) -> [f32; 128] {
    let tick = if time.is_finite() {
        (time.max(0.0) * 10.0) as usize
    } else {
        0
    };
    patterns(entities)
        .map(|pattern| pattern.map_or(256.0 * LEVEL_SCALE, |pattern| pattern_value(pattern, tick)))
}

fn patterns(entities: &[MapLightStyleEntity]) -> [Option<&str>; 128] {
    let mut values = [None; 128];
    for (index, pattern) in DEFAULT_PATTERNS.iter().enumerate() {
        values[index] = Some(*pattern);
    }
    values[63] = Some("a");
    for entity in entities {
        if entity.targetname.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let Some(style) = entity
            .style
            .and_then(|style| usize::try_from(style).ok())
            .filter(|style| (32..128).contains(style))
        else {
            continue;
        };
        let off = entity.spawnflags.is_some_and(|flags| flags & 1 != 0);
        let pattern = if off {
            "a"
        } else {
            entity
                .pattern
                .as_deref()
                .filter(|pattern| !pattern.is_empty())
                .unwrap_or_else(|| {
                    entity
                        .default_style
                        .and_then(|style| usize::try_from(style).ok())
                        .filter(|index| *index > 0)
                        .and_then(|index| DEFAULT_PATTERNS.get(index))
                        .copied()
                        .unwrap_or("m")
                })
        };
        values[style] = Some(pattern);
    }
    values
}

pub(super) fn model_light_styles_animated(
    entities: &[MapLightStyleEntity],
    lights: &[gmpublished_domain::scene::map::MapWorldLight],
) -> bool {
    let patterns = patterns(entities);
    lights
        .iter()
        .filter(|light| light.intensity.into_iter().any(|value| value > 0.0))
        .filter(|light| light.kind != vformats::bsp::WorldLightKind::SkyAmbient)
        .filter_map(|light| {
            usize::try_from(light.style)
                .ok()
                .and_then(|style| patterns.get(style))
                .copied()
                .flatten()
        })
        .any(|pattern| {
            pattern
                .as_bytes()
                .first()
                .is_some_and(|first| pattern.bytes().any(|value| value != *first))
        })
}

fn pattern_value(pattern: &str, tick: usize) -> f32 {
    let Some(&byte) = pattern.as_bytes().get(tick % pattern.len().max(1)) else {
        return 256.0 * LEVEL_SCALE;
    };
    // The engine stores integer letter levels before applying1/264. This
    // differs by an ULP from directly dividing the letter index by12.
    f32::from((i16::from(byte) - i16::from(b'a')) * 22) * LEVEL_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::load_map;

    fn authored(entities: &str) -> Vec<MapLightStyleEntity> {
        let mut bytes = vec![0; 1036];
        bytes[..4].copy_from_slice(b"VBSP");
        bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&1036_i32.to_le_bytes());
        bytes[12..16].copy_from_slice(&(entities.len() as i32).to_le_bytes());
        bytes.extend_from_slice(entities.as_bytes());
        load_map(&bytes)
            .unwrap()
            .world_lighting
            .light_style_entities
    }

    #[test]
    fn empty_slots_and_integer_native_letter_levels_remain_distinct() {
        let defaults = model_light_style_values(&[], 0.0);
        assert_eq!(defaults[0], 1.0);
        assert_eq!(defaults[13], 256.0 * LEVEL_SCALE);
        assert_eq!(defaults[63], 0.0);
        let entities = authored(
            r#"{ "classname" "light" "targetname" "controlled" "style" "32" "pattern" "amz" }"#,
        );
        assert_eq!(model_light_style_values(&entities, 0.0)[32], 0.0);
        assert_eq!(model_light_style_values(&entities, 0.1)[32], 1.0);
        assert_eq!(
            model_light_style_values(&entities, 0.2)[32].to_bits(),
            0x4005_5556
        );
        assert_eq!(model_light_style_values(&entities, 0.3)[32], 0.0);
    }

    #[test]
    fn switchable_spawn_policy_preserves_off_defaultstyle_and_authored_order() {
        let entities = authored(
            r#"
        { "classname" "light" "targetname" "off" "style" "32" "pattern" "z" "spawnflags" "1" }
        { "classname" "light_spot" "targetname" "default" "style" "33" "defaultstyle" "9" }
        { "classname" "light_environment" "targetname" "on" "style" "34" }
        { "classname" "light" "style" "35" "pattern" "a" }
        { "classname" "light" "targetname" "first" "style" "36" "pattern" "a" }
        { "classname" "light" "targetname" "second" "style" "36" "pattern" "z" }
        "#,
        );
        let values = model_light_style_values(&entities, 0.0);
        assert_eq!(values[32], 0.0);
        assert_eq!(values[33], 0.0);
        assert_eq!(values[34], 1.0);
        assert_eq!(values[35], 256.0 * LEVEL_SCALE);
        assert_eq!(values[36].to_bits(), 0x4005_5556);
        assert_eq!(model_light_style_values(&entities, f64::NAN), values);
    }

    #[test]
    fn native_double_clock_preserves_lightstyle_tick_boundaries() {
        let entities = authored(
            r#"{ "classname" "light" "targetname" "controlled" "style" "32" "pattern" "amz" }"#,
        );
        let before = 0.099_999_999_9_f64;
        assert_eq!(model_light_style_values(&entities, before)[32], 0.0);
        assert_eq!(model_light_style_values(&entities, 0.1)[32], 1.0);
        assert_eq!(
            model_light_style_values(&entities, f64::from(before as f32))[32],
            1.0
        );
        assert_eq!(model_light_style_values(&entities, -1.0)[32], 0.0);
        assert_eq!(model_light_style_values(&entities, f64::INFINITY)[32], 0.0);
    }
}
