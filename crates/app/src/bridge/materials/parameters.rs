use super::{BTreeMap, Vec3, profile::HardwareProfile};
use vformats::keyvalues::{KvDocument, KvValue};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MaterialProxy {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

pub(super) fn string_values(
    document: &KvDocument<'_>,
    profile: HardwareProfile,
) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for pair in &document.pairs {
        if let KvValue::String(value) = &pair.value {
            values
                .entry(pair.key.to_ascii_lowercase())
                .or_insert_with(|| value.to_string());
        }
    }
    let overrides = document
        .pairs
        .iter()
        .filter_map(|pair| {
            let KvValue::String(value) = &pair.value else {
                return None;
            };
            let key = pair.key.to_ascii_lowercase();
            let (condition, parameter) = key.split_once('?')?;
            let (condition, negate) = condition
                .strip_prefix('!')
                .map_or((condition, false), |condition| (condition, true));
            let Some(enabled) = profile.condition(condition) else {
                log::warn!("unsupported material condition {condition}");
                return None;
            };
            (enabled != negate).then(|| (parameter.to_owned(), value.to_string()))
        })
        .collect::<Vec<_>>();
    values.extend(overrides);
    values
}

pub(super) fn proxies(document: &KvDocument<'_>, profile: HardwareProfile) -> Vec<MaterialProxy> {
    document
        .blocks("proxies")
        .flat_map(|block| &block.pairs)
        .filter_map(|pair| match &pair.value {
            KvValue::Block(block) => Some(MaterialProxy {
                name: pair.key.to_ascii_lowercase(),
                values: string_values(block, profile),
            }),
            KvValue::String(_) => None,
        })
        .collect()
}

pub(super) fn scalar(value: Option<&str>, default: f32) -> f32 {
    value
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn vector(value: Option<&str>) -> Option<Vec<f32>> {
    let value = value?.trim().trim_matches(['[', ']', '{', '}']);
    let values = value
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    values.iter().all(|v| v.is_finite()).then_some(values)
}

pub(super) fn vec2(value: Option<&str>) -> [f32; 2] {
    vector(value)
        .filter(|v| v.len() >= 2)
        .map_or([0.0; 2], |v| [v[0], v[1]])
}

pub(super) fn tint(value: Option<&str>, default: [f32; 3]) -> [f32; 3] {
    value
        .and_then(parse_material_color)
        .map_or(default, |color| {
            color.to_array().map(|v| v.max(0.0).powf(2.2))
        })
}

pub(super) fn texture_transform(value: Option<&str>) -> [[f32; 4]; 2] {
    let mut center = [0.5_f32; 2];
    let mut scale = [1.0_f32; 2];
    let mut translation = [0.0_f32; 2];
    let mut rotation = 0.0_f32;
    if let Some(value) = value {
        let words = value.split_whitespace().collect::<Vec<_>>();
        let mut i = 0;
        while i < words.len() {
            let destination = match words[i].to_ascii_lowercase().as_str() {
                "center" => Some(&mut center),
                "scale" => Some(&mut scale),
                "translate" => Some(&mut translation),
                "rotate" => {
                    rotation = scalar(words.get(i + 1).copied(), 0.0);
                    i += 2;
                    continue;
                }
                _ => None,
            };
            if let Some(destination) = destination {
                *destination = [
                    scalar(words.get(i + 1).copied(), destination[0]),
                    scalar(words.get(i + 2).copied(), destination[1]),
                ];
                i += 3;
            } else {
                i += 1;
            }
        }
    }
    let (sin, cos) = rotation.to_radians().sin_cos();
    let a = cos * scale[0];
    let b = -sin * scale[1];
    let c = sin * scale[0];
    let d = cos * scale[1];
    [
        [
            a,
            b,
            0.0,
            center[0] + translation[0] - a * center[0] - b * center[1],
        ],
        [
            c,
            d,
            0.0,
            center[1] + translation[1] - c * center[0] - d * center[1],
        ],
    ]
}

pub(super) fn parse_material_color(value: &str) -> Option<Vec3> {
    let trimmed = value.trim();
    let (inner, scale) = if let Some(inner) = bracketed_value(trimmed, '[', ']') {
        (inner, 1.0)
    } else if let Some(inner) = bracketed_value(trimmed, '{', '}') {
        (inner, 1.0 / 255.0)
    } else {
        return None;
    };

    let mut components = inner
        .split(|char: char| char.is_ascii_whitespace() || char == ',')
        .filter(|component| !component.is_empty())
        .map(str::parse::<f32>);
    let red = components.next()?.ok()? * scale;
    let green = components.next()?.ok()? * scale;
    let blue = components.next()?.ok()? * scale;
    [red, green, blue]
        .into_iter()
        .all(f32::is_finite)
        .then_some(Vec3::new(red, green, blue))
}

fn bracketed_value(value: &str, open: char, close: char) -> Option<&str> {
    value
        .strip_prefix(open)
        .and_then(|value| value.strip_suffix(close))
        .map(str::trim)
}
