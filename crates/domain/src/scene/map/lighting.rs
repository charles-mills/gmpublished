use crate::math::Vec3;
use crate::scene::QAngle;

use super::{
    BuildMesh, ColorRgbExp, Face, LeafAmbientIndex, LeafAmbientSample, MapBsp, MapEntity, MapLeaf,
    MapLeafLocator, TexInfo, parse_entity_float, parse_entity_vec3, texture_coord,
    vector_is_finite_nonzero,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AmbientCube {
    pub colors: [Vec3; 6],
}

impl AmbientCube {
    pub const WHITE: Self = Self {
        colors: [Vec3::new(1.0, 1.0, 1.0); 6],
    };

    pub fn evaluate(self, normal: Vec3) -> Vec3 {
        let normal = normal.normalize_or_zero();
        let mut color = [0.0_f32; 3];
        for (axis, component) in normal.to_array().into_iter().enumerate() {
            let side = if component >= 0.0 {
                axis * 2
            } else {
                axis * 2 + 1
            };
            let weight = component * component;
            color[0] += self.colors[side][0] * weight;
            color[1] += self.colors[side][1] * weight;
            color[2] += self.colors[side][2] * weight;
        }
        Vec3::from(color)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSunLighting {
    pub direction_to_sun: Vec3,
    pub color_linear: Vec3,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapEnvironmentLighting {
    pub sun: Option<MapSunLighting>,
    pub skylight_linear: Option<Vec3>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbientLightSource {
    Neutral,
    Ldr,
    Hdr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapAmbientLighting {
    pub(super) source: AmbientLightSource,
    pub(super) locator: MapLeafLocator,
    pub(super) samples: Vec<MapAmbientSample>,
    pub(super) leaf_sample_ranges: Vec<AmbientSampleRange>,
}

impl MapAmbientLighting {
    pub fn neutral() -> Self {
        Self {
            source: AmbientLightSource::Neutral,
            locator: MapLeafLocator::default(),
            samples: Vec::new(),
            leaf_sample_ranges: Vec::new(),
        }
    }

    pub const fn source(&self) -> AmbientLightSource {
        self.source
    }

    /// Compiled samples before nearest-sample evaluation or fallback policy.
    pub fn samples(&self) -> &[MapAmbientSample] {
        &self.samples
    }

    /// Original per-leaf sample grouping, preserving all directional colours.
    pub fn samples_for_leaf(&self, leaf_index: usize) -> &[MapAmbientSample] {
        self.leaf_sample_ranges
            .get(leaf_index)
            .and_then(|range| self.samples.get(range.start..range.end()))
            .unwrap_or_default()
    }

    pub fn leaf_at(&self, position: Vec3) -> Option<usize> {
        position
            .is_finite()
            .then(|| self.locator.leaf_at(position))
            .flatten()
    }

    /// Original BSP integer bounds, before absolute sample positions are rounded.
    pub fn leaf_bounds(&self, leaf_index: usize) -> Option<([i16; 3], [i16; 3])> {
        self.locator
            .leaves
            .get(leaf_index)
            .map(|leaf| (leaf.mins, leaf.maxs))
    }

    /// Source radiosity-4 leaf interpolation at an already validated sample point.
    /// Cache-center correction, empty-leaf redirection and static-prop spherical
    /// sampling belong to the caller's lighting policy.
    pub fn interpolate_leaf_at(&self, leaf_index: usize, position: Vec3) -> Option<AmbientCube> {
        if !position.is_finite() {
            return None;
        }
        let (mins, maxs) = self.leaf_bounds(leaf_index)?;
        let samples = self.samples_for_leaf(leaf_index);
        if samples.is_empty() {
            return None;
        }
        let minimum = Vec3::from(mins.map(f32::from));
        let maximum = Vec3::from(maxs.map(f32::from));
        let center = (minimum + maximum) * 0.5;
        let half_extent = (maximum - minimum) * 0.5;
        let mut total_weight = 0.0;
        let mut colors = [Vec3::ZERO; 6];
        for sample in samples {
            // Native computes relative deltas directly. Subtracting the rounded
            // absolute position loses precision at large world coordinates.
            let delta: [f32; 3] = std::array::from_fn(|axis| {
                center[axis] - (position[axis] + half_extent[axis])
                    + (half_extent[axis] * (2.0 / 255.0)) * f32::from(sample.encoded_position[axis])
            });
            let weight =
                1.0 / ((delta[0] * delta[0] + delta[1] * delta[1] + 1.0) + delta[2] * delta[2]);
            total_weight += weight;
            for (color, value) in colors.iter_mut().zip(sample.cube.colors) {
                *color += value * weight;
            }
        }
        if !total_weight.is_finite() || total_weight <= 0.0 {
            return None;
        }
        let inverse_weight = total_weight.recip();
        colors = colors.map(|color| color * inverse_weight);
        colors
            .into_iter()
            .all(Vec3::is_finite)
            .then_some(AmbientCube { colors })
    }

    pub fn cube_at(&self, position: Vec3) -> AmbientCube {
        if self.source == AmbientLightSource::Neutral {
            log::debug!("map ambient fallback white: no LDR/HDR leaf ambient samples");
            return AmbientCube::WHITE;
        }
        let Some(leaf_index) = self.locator.leaf_at(position) else {
            log::debug!("map ambient fallback white: leaf lookup failed at {position:?}");
            return AmbientCube::WHITE;
        };
        if let Some(cube) = self.nearest_leaf_sample(leaf_index, position) {
            return cube;
        }
        let Some(leaf) = self.locator.leaves.get(leaf_index) else {
            log::debug!("map ambient fallback white: leaf {leaf_index} missing from locator");
            return AmbientCube::WHITE;
        };
        if let Some(cube) = self.nearest_cluster_sample(leaf.cluster, position) {
            log::debug!(
                "map ambient fallback cluster-mate: leaf {leaf_index} cluster {}",
                leaf.cluster
            );
            return cube;
        }
        log::debug!(
            "map ambient fallback white: leaf {leaf_index} cluster {} has no ambient samples",
            leaf.cluster
        );
        AmbientCube::WHITE
    }

    pub(super) fn nearest_leaf_sample(
        &self,
        leaf_index: usize,
        position: Vec3,
    ) -> Option<AmbientCube> {
        let range = self.leaf_sample_ranges.get(leaf_index).copied()?;
        self.nearest_sample_in_range(range, position)
    }

    pub(super) fn nearest_cluster_sample(
        &self,
        cluster: i16,
        position: Vec3,
    ) -> Option<AmbientCube> {
        self.samples
            .iter()
            .filter(|sample| sample.cluster == cluster)
            .min_by(|left, right| {
                left.position
                    .distance_squared(position)
                    .total_cmp(&right.position.distance_squared(position))
            })
            .map(|sample| sample.cube)
    }

    pub(super) fn nearest_sample_in_range(
        &self,
        range: AmbientSampleRange,
        position: Vec3,
    ) -> Option<AmbientCube> {
        self.samples
            .get(range.start..range.end())
            .and_then(|samples| {
                samples
                    .iter()
                    .min_by(|left, right| {
                        left.position
                            .distance_squared(position)
                            .total_cmp(&right.position.distance_squared(position))
                    })
                    .map(|sample| sample.cube)
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapAmbientSample {
    pub position: Vec3,
    pub encoded_position: [u8; 3],
    pub cube: AmbientCube,
    pub cluster: i16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct AmbientSampleRange {
    pub(super) start: usize,
    pub(super) count: usize,
}

impl AmbientSampleRange {
    pub(super) const fn end(self) -> usize {
        self.start + self.count
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapAtlas {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source: LightmapSource,
    patches: Vec<LightmapPatch>,
    styles: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LightmapSource {
    Ldr,
    Hdr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingLightmapBlock {
    pub(super) rgba: Vec<u8>,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) face_width: usize,
    pub(super) face_height: usize,
    pub(super) styles: Vec<u8>,
    pub(super) samples: Vec<ColorRgbExp>,
    pub(super) basis_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AtlasRect {
    pub(super) x: usize,
    pub(super) y: usize,
}

pub(super) const LIGHTMAP_ATLAS_LIMIT: usize = 4096;

pub(super) fn map_ambient_lighting(bsp: &MapBsp) -> MapAmbientLighting {
    let Some((ambient, source)) = selected_ambient_lighting(bsp) else {
        return MapAmbientLighting::neutral();
    };
    let locator = MapLeafLocator::from_bsp(bsp);
    let mut samples = Vec::new();
    let mut leaf_sample_ranges = vec![AmbientSampleRange::default(); locator.leaves.len()];
    for (leaf_index, leaf) in locator.leaves.iter().enumerate() {
        let Some(index) = ambient.index.get(leaf_index).copied() else {
            continue;
        };
        let first_sample = usize::from(index.first_sample);
        let sample_count = usize::from(index.sample_count);
        let sample_end = first_sample.saturating_add(sample_count);
        let Some(leaf_samples) = ambient.samples.get(first_sample..sample_end) else {
            log::debug!(
                "map ambient leaf {leaf_index}: invalid sample range {first_sample}..{sample_end}"
            );
            continue;
        };
        let start = samples.len();
        for sample in leaf_samples {
            samples.push(MapAmbientSample {
                position: ambient_sample_position(sample.position, leaf),
                encoded_position: sample.position,
                cube: ambient_cube(sample.cube),
                cluster: leaf.cluster,
            });
        }
        leaf_sample_ranges[leaf_index] = AmbientSampleRange {
            start,
            count: samples.len() - start,
        };
    }

    if samples.is_empty() {
        return MapAmbientLighting::neutral();
    }

    MapAmbientLighting {
        source,
        locator,
        samples,
        leaf_sample_ranges,
    }
}

pub(super) struct LeafAmbientLightingRef<'a> {
    pub(super) samples: &'a [LeafAmbientSample],
    pub(super) index: &'a [LeafAmbientIndex],
}

pub(super) fn selected_ambient_lighting(
    bsp: &MapBsp,
) -> Option<(LeafAmbientLightingRef<'_>, AmbientLightSource)> {
    let ldr = LeafAmbientLightingRef {
        samples: &bsp.leaf_ambient_lighting,
        index: &bsp.leaf_ambient_indices,
    };
    let hdr = LeafAmbientLightingRef {
        samples: &bsp.leaf_ambient_lighting_hdr,
        index: &bsp.leaf_ambient_indices_hdr,
    };
    if ambient_lighting_is_usable(&ldr) {
        Some((ldr, AmbientLightSource::Ldr))
    } else if ambient_lighting_is_usable(&hdr) {
        Some((hdr, AmbientLightSource::Hdr))
    } else {
        None
    }
}

pub(super) fn ambient_lighting_is_usable(ambient: &LeafAmbientLightingRef<'_>) -> bool {
    !ambient.samples.is_empty() && !ambient.index.is_empty()
}

pub(super) fn ambient_sample_position(position: [u8; 3], leaf: &MapLeaf) -> Vec3 {
    Vec3::from(std::array::from_fn(|axis| {
        let min = f32::from(leaf.mins[axis]);
        let max = f32::from(leaf.maxs[axis]);
        min + (max - min) * (f32::from(position[axis]) / 255.0)
    }))
}

pub(super) fn ambient_cube(cube: [ColorRgbExp; 6]) -> AmbientCube {
    AmbientCube {
        colors: cube.map(decode_ambient_sample_linear),
    }
}

// Ambient cubes use byte * 2^exp, without the /255 applied to lightmaps.
pub(super) fn decode_ambient_sample_linear(sample: ColorRgbExp) -> Vec3 {
    let scale = 2.0_f32.powi(i32::from(sample.exponent));
    Vec3::new(
        f32::from(sample.r) * scale,
        f32::from(sample.g) * scale,
        f32::from(sample.b) * scale,
    )
}

pub(super) fn map_environment_lighting(entities: &[MapEntity]) -> Option<MapEnvironmentLighting> {
    let entity = entities
        .iter()
        .find(|entity| entity.prop("classname") == Some("light_environment"))?;
    let direction_to_sun = parse_light_environment_direction(entity);
    let sun = match (entity.prop("_light"), direction_to_sun) {
        (Some(value), Some(direction_to_sun)) => {
            parse_entity_rgb_intensity_linear(value).map(|color_linear| MapSunLighting {
                direction_to_sun,
                color_linear,
            })
        }
        (Some(_), None) => {
            log::debug!("bsp light_environment sun skipped: missing or invalid pitch/angles");
            None
        }
        (None, _) => None,
    };
    if entity.prop("_light").is_some() && sun.is_none() && direction_to_sun.is_some() {
        log::debug!("bsp light_environment sun skipped: invalid _light");
    }

    let skylight_linear = entity.prop("_ambient").and_then(|value| {
        let parsed = parse_entity_rgb_intensity_linear(value);
        if parsed.is_none() {
            log::debug!("bsp light_environment skylight skipped: invalid _ambient");
        }
        parsed
    });

    Some(MapEnvironmentLighting {
        sun,
        skylight_linear,
    })
}

pub(super) fn parse_light_environment_direction(entity: &MapEntity) -> Option<Vec3> {
    let angles = entity
        .prop("angles")
        .and_then(parse_entity_vec3)
        .map(QAngle::from_source_degrees);
    let yaw = angles.map_or(0.0, |angles| angles.yaw);
    // A separate `pitch` key overrides the one inside `angles`.
    let pitch = entity
        .prop("pitch")
        .and_then(parse_entity_float)
        .map(f32::to_radians)
        .or_else(|| angles.map(|angles| angles.pitch))?;
    let travel_direction = Vec3::new(
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        pitch.sin(),
    )
    .normalize_or_zero();
    vector_is_finite_nonzero(travel_direction).then_some(travel_direction * -1.0)
}

pub(super) fn parse_entity_rgb_intensity_linear(value: &str) -> Option<Vec3> {
    let mut components = value.split_ascii_whitespace().map(parse_entity_float);
    let red = components.next()??;
    let green = components.next()??;
    let blue = components.next()??;
    // VRAD LightForString defaults intensity to 255 and permits values above it.
    let intensity = match components.next() {
        Some(value) => value?,
        None => 255.0,
    };
    if components.next().is_some() {
        return None;
    }
    if ![red, green, blue]
        .into_iter()
        .all(|value| (0.0..=255.0).contains(&value))
        || !intensity.is_finite()
        || intensity < 0.0
    {
        return None;
    }
    // VRAD converts RGB to linear with pow 2.2 before applying intensity.
    let scale = intensity / 255.0;
    Some(Vec3::new(
        (red / 255.0).powf(2.2) * scale,
        (green / 255.0).powf(2.2) * scale,
        (blue / 255.0).powf(2.2) * scale,
    ))
}

pub(super) fn selected_lightmap_samples(
    bsp: &MapBsp,
) -> (Option<&[ColorRgbExp]>, Option<LightmapSource>) {
    if !bsp.lighting.is_empty() {
        (Some(&bsp.lighting), Some(LightmapSource::Ldr))
    } else if !bsp.lighting_hdr.is_empty() {
        (Some(&bsp.lighting_hdr), Some(LightmapSource::Hdr))
    } else {
        (None, None)
    }
}

/// Bound each face at the next distinct payload offset. Compiler-generated
/// maps usually include per-style averages before that next payload, but
/// native Source also accepts maps without these unused prefix records.
pub(super) fn lightmap_sample_ends(
    faces: &[Face],
    sample_count: usize,
) -> std::collections::HashMap<i32, usize> {
    let offsets = faces
        .iter()
        .map(|face| face.light_offset)
        .filter(|offset| *offset >= 0 && *offset % 4 == 0)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    offsets
        .iter()
        .enumerate()
        .map(|(index, offset)| {
            let end = offsets
                .get(index + 1)
                .map_or(sample_count, |next| *next as usize / 4)
                .min(sample_count);
            (*offset, end)
        })
        .collect()
}

pub(super) fn extract_face_lightmap(
    face: &Face,
    samples: &[ColorRgbExp],
    bumped: bool,
) -> Option<PendingLightmapBlock> {
    if face.light_offset < 0 || face.light_offset % 4 != 0 {
        return None;
    }
    let face_width = lightmap_axis_luxels(face.lightmap_size[0])?;
    let face_height = lightmap_axis_luxels(face.lightmap_size[1])?;
    let basis_count = if bumped { 4 } else { 1 };
    let styles = face
        .styles
        .into_iter()
        .take_while(|style| *style != 255)
        .collect::<Vec<_>>();
    if styles.is_empty() {
        return None;
    }
    let count = face_width
        .checked_mul(face_height)?
        .checked_mul(basis_count)?
        .checked_mul(styles.len())?;
    let start = usize::try_from(face.light_offset).ok()? / 4;
    let end = start.checked_add(count)?;
    let width = face_width.checked_add(2)?.checked_mul(basis_count)?;
    let height = face_height.checked_add(2)?;
    // Reject impossible faces before allocating their padded atlas block.
    if width > LIGHTMAP_ATLAS_LIMIT || height > LIGHTMAP_ATLAS_LIMIT {
        return None;
    }
    let samples = samples.get(start..end)?.to_vec();
    let block = PendingLightmapBlock {
        rgba: Vec::new(),
        width,
        height,
        face_width,
        face_height,
        styles,
        samples,
        basis_count,
    };
    Some(block)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LightmapPatch {
    origin: [u32; 2],
    block: PendingLightmapBlock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapUpdate {
    pub origin: [u32; 2],
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl LightmapAtlas {
    pub fn active_styles(&self) -> Vec<u8> {
        self.patches
            .iter()
            .flat_map(|patch| patch.block.styles.iter().copied())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn style_values(&self, time: f32) -> [f32; 256] {
        let tick = if time.is_finite() {
            (time.max(0.0) * 10.0).floor() as usize
        } else {
            0
        };
        std::array::from_fn(|index| {
            let pattern = &self.styles[index];
            f32::from(pattern[tick % pattern.len()].saturating_sub(b'a')) / 12.0
        })
    }

    pub fn is_animated(&self) -> bool {
        self.patches.iter().any(|patch| {
            patch.block.styles.iter().any(|style| {
                let pattern = &self.styles[usize::from(*style)];
                pattern.iter().any(|value| *value != pattern[0])
            })
        })
    }

    pub fn updates(&self, previous: &[f32; 256], next: &[f32; 256]) -> Vec<LightmapUpdate> {
        self.patches
            .iter()
            .filter(|patch| {
                patch
                    .block
                    .styles
                    .iter()
                    .any(|style| previous[usize::from(*style)] != next[usize::from(*style)])
            })
            .map(|patch| LightmapUpdate {
                origin: patch.origin,
                width: patch.block.width as u32,
                height: patch.block.height as u32,
                rgba: patch.block.render(next, self.source),
            })
            .collect()
    }
}

const DEFAULT_LIGHTSTYLES: [&str; 13] = [
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

fn lightstyle_patterns(entities: &[MapEntity]) -> Vec<Vec<u8>> {
    let mut styles = vec![b"m".to_vec(); 256];
    for (index, pattern) in DEFAULT_LIGHTSTYLES.iter().enumerate() {
        styles[index] = pattern.as_bytes().to_vec();
    }
    styles[63] = b"a".to_vec();
    for entity in entities {
        if !matches!(
            entity.prop("classname"),
            Some("light" | "light_spot" | "light_environment")
        ) || entity.prop("targetname").is_none_or(str::is_empty)
        {
            continue;
        }
        let Some(style) = entity
            .prop("style")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (32..255).contains(value))
        else {
            continue;
        };
        let start_off = entity
            .prop("spawnflags")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|flags| flags & 1 != 0);
        let pattern = if start_off {
            "a"
        } else if let Some(pattern) = entity.prop("pattern").filter(|value| !value.is_empty()) {
            pattern
        } else {
            entity
                .prop("defaultstyle")
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|index| *index > 0)
                .and_then(|index| DEFAULT_LIGHTSTYLES.get(index))
                .copied()
                .unwrap_or("m")
        };
        // Lightstyle characters above z are legal Source intensity values.
        if pattern.bytes().all(|value| value >= b'a') {
            styles[style] = pattern.as_bytes().to_vec();
        }
    }
    styles
}

impl PendingLightmapBlock {
    fn render(&self, values: &[f32; 256], source: LightmapSource) -> Vec<u8> {
        let luxels = self.face_width * self.face_height;
        let mut rgba = vec![0; self.width * self.height * 4];
        for luxel in 0..luxels {
            let mut linear = [Vec3::ZERO; 4];
            for (style_index, style) in self.styles.iter().enumerate() {
                let weight = values[usize::from(*style)];
                for (basis, color) in linear.iter_mut().enumerate().take(self.basis_count) {
                    *color += decode_light_sample_linear(
                        self.samples[(style_index * self.basis_count + basis) * luxels + luxel],
                    ) * weight;
                }
            }
            let colors = match source {
                LightmapSource::Ldr => convert_lightmap_luxel(linear, self.basis_count),
                // Explicit display fallback until floating-point HDR targets,
                // tonemapping and exposure are supported together.
                LightmapSource::Hdr => linear.map(|color| {
                    [
                        linear_to_srgb_byte(color[0]),
                        linear_to_srgb_byte(color[1]),
                        linear_to_srgb_byte(color[2]),
                        255,
                    ]
                }),
            };
            let x = luxel % self.face_width;
            let y = luxel / self.face_width;
            for (basis, color) in colors.iter().enumerate().take(self.basis_count) {
                let index = ((y + 1) * self.width + basis * (self.face_width + 2) + x + 1) * 4;
                rgba[index..index + 4].copy_from_slice(color);
            }
        }
        // Every plane has its own repeated edge texels, preventing filtering
        // between neighboring faces and between directional lightmap planes.
        for basis in 0..self.basis_count {
            let left = basis * (self.face_width + 2);
            for y in 1..=self.face_height {
                for (dst, src) in [
                    (left, left + 1),
                    (left + self.face_width + 1, left + self.face_width),
                ] {
                    let source = (y * self.width + src) * 4;
                    let color: [u8; 4] = rgba[source..source + 4].try_into().expect("texel");
                    let target = (y * self.width + dst) * 4;
                    rgba[target..target + 4].copy_from_slice(&color);
                }
            }
            for (dst, src) in [(0, 1), (self.face_height + 1, self.face_height)] {
                let source = (src * self.width + left) * 4;
                let target = (dst * self.width + left) * 4;
                let row = rgba[source..source + (self.face_width + 2) * 4].to_vec();
                rgba[target..target + row.len()].copy_from_slice(&row);
            }
        }
        rgba
    }
}

fn lightmap_gamma_goal(value: f32) -> f32 {
    let index = (value * 1024.0).round().clamp(0.0, 4095.0);
    ((index / 1024.0).powf(1.0 / 2.2) * 0.5).min(1.0)
}

fn lightmap_bytes(value: Vec3) -> [u8; 4] {
    let value = value / value[0].max(value[1]).max(value[2]).max(1.0);
    [
        (value[0].max(0.0) * 255.0).round() as u8,
        (value[1].max(0.0) * 255.0).round() as u8,
        (value[2].max(0.0) * 255.0).round() as u8,
        255,
    ]
}

fn convert_lightmap_luxel(linear: [Vec3; 4], basis_count: usize) -> [[u8; 4]; 4] {
    let goal = Vec3::from(linear[0].to_array().map(lightmap_gamma_goal));
    let mut colors = [[0, 0, 0, 255]; 4];
    colors[0] = lightmap_bytes(goal);
    if basis_count == 4 {
        let average = (linear[1] + linear[2] + linear[3]) / 3.0;
        let correction = Vec3::from(std::array::from_fn(|channel| {
            if average[channel] != 0.0 {
                goal[channel] / average[channel]
            } else {
                0.0
            }
        }));
        let mut directions = [linear[1], linear[2], linear[3]].map(|color| {
            Vec3::from(std::array::from_fn(|channel| {
                color[channel] * correction[channel]
            }))
        });
        let maxima = directions.map(|color| color[0].max(color[1]).max(color[2]));
        // SDK resolves equal brightness in the last matching permutation.
        let mut order = [0, 1, 2];
        for candidate in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            if maxima[candidate[0]] >= maxima[candidate[1]]
                && maxima[candidate[1]] >= maxima[candidate[2]]
            {
                order = candidate;
            }
        }
        for index in 0..3 {
            let current = order[index];
            let maximum = directions[current][0]
                .max(directions[current][1])
                .max(directions[current][2]);
            if maximum > 1.0 {
                let redistribution = directions[current] * ((maximum - 1.0) / maximum);
                directions[current] -= redistribution;
                directions[order[(index + 1) % 3]] += redistribution * 0.5;
                directions[order[(index + 2) % 3]] += redistribution * 0.5;
            }
        }
        for (index, color) in directions.into_iter().enumerate() {
            colors[index + 1] = lightmap_bytes(color);
        }
    }
    colors
}

pub(super) fn lightmap_axis_luxels(size: i32) -> Option<usize> {
    usize::try_from(size).ok()?.checked_add(1)
}

#[cfg(test)]
pub(super) fn decode_light_sample(sample: ColorRgbExp) -> [u8; 4] {
    convert_lightmap_luxel([decode_light_sample_linear(sample); 4], 1)[0]
}

pub(super) fn decode_light_sample_linear(sample: ColorRgbExp) -> Vec3 {
    let scale = 2.0_f32.powi(i32::from(sample.exponent)) / 255.0;
    Vec3::new(
        f32::from(sample.r) * scale,
        f32::from(sample.g) * scale,
        f32::from(sample.b) * scale,
    )
}

pub(super) fn linear_to_srgb_byte(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let srgb = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

pub(super) fn brush_lightmap_uv(position: Vec3, texinfo: &TexInfo, face: &Face) -> [f32; 2] {
    brush_lightmap_uv_from_transforms(
        position,
        texinfo.lightmap_vecs[0],
        texinfo.lightmap_vecs[1],
        face.lightmap_mins,
        face.lightmap_size,
    )
}

pub(super) fn brush_lightmap_uv_from_transforms(
    position: Vec3,
    light_map_scale: [f32; 4],
    light_map_transform: [f32; 4],
    light_map_texture_min: [i32; 2],
    light_map_texture_size: [i32; 2],
) -> [f32; 2] {
    [
        lightmap_axis_uv(
            texture_coord(position, light_map_scale),
            light_map_texture_min[0],
            light_map_texture_size[0],
        ),
        lightmap_axis_uv(
            texture_coord(position, light_map_transform),
            light_map_texture_min[1],
            light_map_texture_size[1],
        ),
    ]
}

pub(super) fn lightmap_axis_uv(value: f32, min: i32, size: i32) -> f32 {
    let denominator = size as f32 + 1.0;
    if denominator <= 0.0 {
        0.0
    } else {
        (value - min as f32 + 0.5) / denominator
    }
}

pub(super) fn displacement_lightmap_uv(
    column: usize,
    row: usize,
    steps: usize,
    face: &Face,
) -> [f32; 2] {
    let steps = steps.max(1) as f32;
    // Source's rotated base corners are (0,0), (0,V), (U,V), (U,0).
    // The outer grid index follows corner 0 -> 1, so it advances V.
    [
        displacement_lightmap_axis_uv(row as f32 / steps, face.lightmap_size[0]),
        displacement_lightmap_axis_uv(column as f32 / steps, face.lightmap_size[1]),
    ]
}

pub(super) fn displacement_lightmap_axis_uv(grid_uv: f32, size: i32) -> f32 {
    let size = size.max(0) as f32;
    (grid_uv * size + 0.5) / (size + 1.0)
}

pub(super) fn bake_lightmap_atlas<'a>(
    meshes: impl IntoIterator<Item = &'a mut BuildMesh>,
    blocks: &[PendingLightmapBlock],
    source: Option<LightmapSource>,
    entities: &[MapEntity],
) -> Option<LightmapAtlas> {
    let mut meshes = meshes.into_iter().collect::<Vec<_>>();
    let source = source?;
    if blocks.is_empty() {
        clear_lightmap_uvs(&mut meshes);
        return None;
    }

    let (width, height, placements) = pack_lightmap_blocks(blocks)?;
    let overflow = placements
        .iter()
        .filter(|placement| placement.is_none())
        .count();
    if overflow > 0 {
        log::debug!("map lightmap atlas overflow: dropped {overflow} faces");
    }

    let mut rgba = vec![128; width * height * 4];
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }

    let styles = lightstyle_patterns(entities);
    let mut atlas = LightmapAtlas {
        rgba: Vec::new(),
        width: width as u32,
        height: height as u32,
        source,
        patches: Vec::new(),
        styles,
    };
    let initial_values = atlas.style_values(0.0);
    for (block, placement) in blocks.iter().zip(&placements) {
        let Some(rect) = placement else {
            continue;
        };
        let rendered = if block.samples.is_empty() {
            block.rgba.clone()
        } else {
            block.render(&initial_values, source)
        };
        atlas.patches.push(LightmapPatch {
            origin: [rect.x as u32, rect.y as u32],
            block: block.clone(),
        });
        for row in 0..block.height {
            let atlas_start = ((rect.y + row) * width + rect.x) * 4;
            let block_start = row * block.width * 4;
            let byte_count = block.width * 4;
            rgba[atlas_start..atlas_start + byte_count]
                .copy_from_slice(&rendered[block_start..block_start + byte_count]);
        }
    }

    rewrite_lightmap_uvs(&mut meshes, blocks, &placements, width, height);

    atlas.rgba = rgba;
    Some(atlas)
}

pub(super) fn clear_lightmap_uvs(meshes: &mut [&mut BuildMesh]) {
    for vertex in meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
        vertex.vertex.lightmap_uv = [0.0; 2];
        vertex.vertex.has_lightmap = false;
        vertex.vertex.lightmap_bump_step = 0.0;
        vertex.lightmap_block = None;
    }
}

pub(super) fn rewrite_lightmap_uvs(
    meshes: &mut [&mut BuildMesh],
    blocks: &[PendingLightmapBlock],
    placements: &[Option<AtlasRect>],
    atlas_width: usize,
    atlas_height: usize,
) {
    for vertex in meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
        let Some(block_index) = vertex.lightmap_block else {
            vertex.vertex.lightmap_uv = [0.0; 2];
            vertex.vertex.has_lightmap = false;
            vertex.vertex.lightmap_bump_step = 0.0;
            continue;
        };
        let Some(block) = blocks.get(block_index) else {
            vertex.vertex.lightmap_uv = [0.0; 2];
            vertex.vertex.has_lightmap = false;
            vertex.vertex.lightmap_bump_step = 0.0;
            continue;
        };
        let Some(Some(rect)) = placements.get(block_index) else {
            vertex.vertex.lightmap_uv = [0.0; 2];
            vertex.vertex.has_lightmap = false;
            vertex.vertex.lightmap_bump_step = 0.0;
            continue;
        };
        vertex.vertex.has_lightmap = true;
        vertex.vertex.lightmap_bump_step = if block.basis_count == 4 {
            (block.face_width + 2) as f32 / atlas_width.max(1) as f32
        } else {
            0.0
        };
        vertex.vertex.lightmap_uv = [
            (rect.x as f32 + 1.0 + vertex.vertex.lightmap_uv[0] * block.face_width as f32)
                / atlas_width.max(1) as f32,
            (rect.y as f32 + 1.0 + vertex.vertex.lightmap_uv[1] * block.face_height as f32)
                / atlas_height.max(1) as f32,
        ];
    }
}

pub(super) fn pack_lightmap_blocks(
    blocks: &[PendingLightmapBlock],
) -> Option<(usize, usize, Vec<Option<AtlasRect>>)> {
    let max_width = blocks.iter().map(|block| block.width).max()?.max(1);
    let max_height = blocks.iter().map(|block| block.height).max()?.max(1);
    if max_width > LIGHTMAP_ATLAS_LIMIT || max_height > LIGHTMAP_ATLAS_LIMIT {
        return Some((
            LIGHTMAP_ATLAS_LIMIT,
            LIGHTMAP_ATLAS_LIMIT,
            shelf_pack_partial(blocks, LIGHTMAP_ATLAS_LIMIT, LIGHTMAP_ATLAS_LIMIT),
        ));
    }

    let mut width = max_width.next_power_of_two();
    let mut height = max_height.next_power_of_two();

    loop {
        if let Some(placements) = shelf_pack_all(blocks, width, height) {
            return Some((width, height, placements));
        }
        if width >= LIGHTMAP_ATLAS_LIMIT && height >= LIGHTMAP_ATLAS_LIMIT {
            return Some((
                LIGHTMAP_ATLAS_LIMIT,
                LIGHTMAP_ATLAS_LIMIT,
                shelf_pack_partial(blocks, LIGHTMAP_ATLAS_LIMIT, LIGHTMAP_ATLAS_LIMIT),
            ));
        }
        if (width <= height && width < LIGHTMAP_ATLAS_LIMIT) || height >= LIGHTMAP_ATLAS_LIMIT {
            width = (width * 2).min(LIGHTMAP_ATLAS_LIMIT);
        } else {
            height = (height * 2).min(LIGHTMAP_ATLAS_LIMIT);
        }
    }
}

pub(super) fn shelf_pack_all(
    blocks: &[PendingLightmapBlock],
    width: usize,
    height: usize,
) -> Option<Vec<Option<AtlasRect>>> {
    let placements = shelf_pack(blocks, width, height, false);
    placements.iter().all(Option::is_some).then_some(placements)
}

pub(super) fn shelf_pack_partial(
    blocks: &[PendingLightmapBlock],
    width: usize,
    height: usize,
) -> Vec<Option<AtlasRect>> {
    shelf_pack(blocks, width, height, true)
}

pub(super) fn shelf_pack(
    blocks: &[PendingLightmapBlock],
    width: usize,
    height: usize,
    partial: bool,
) -> Vec<Option<AtlasRect>> {
    let mut placements = Vec::with_capacity(blocks.len());
    let mut x = 0_usize;
    let mut y = 0_usize;
    let mut shelf_height = 0_usize;

    for block in blocks {
        if block.width > width || block.height > height {
            if !partial {
                return vec![None; blocks.len()];
            }
            placements.push(None);
            continue;
        }
        if x + block.width > width {
            y += shelf_height;
            x = 0;
            shelf_height = 0;
        }
        if y + block.height > height {
            if !partial {
                return vec![None; blocks.len()];
            }
            placements.push(None);
            continue;
        }
        placements.push(Some(AtlasRect { x, y }));
        x += block.width;
        shelf_height = shelf_height.max(block.height);
    }

    placements
}

#[cfg(test)]
mod lightmap_tests {
    use super::*;

    fn face(styles: [u8; 4]) -> Face {
        Face {
            plane: 0,
            side: 0,
            on_node: 0,
            first_edge: 0,
            edge_count: 3,
            texinfo: 0,
            displacement: -1,
            styles,
            light_offset: 0,
            area: 1.0,
            lightmap_mins: [0, 0],
            lightmap_size: [0, 0],
            original_face: -1,
        }
    }

    fn sample(r: u8, g: u8, b: u8) -> ColorRgbExp {
        ColorRgbExp {
            r,
            g,
            b,
            exponent: 0,
        }
    }

    #[test]
    fn displacement_lightmap_follows_rotated_base_corners_on_rectangular_atlas() {
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [8, 4];
        // Grid order is 0 -> 1 along columns, then 0 -> 3 along rows.
        // Distinct extents and an off-diagonal interior point catch transposes.
        for (column, row, expected) in [
            (0, 0, [0.5 / 9.0, 0.5 / 5.0]),
            (4, 0, [0.5 / 9.0, 4.5 / 5.0]),
            (4, 4, [8.5 / 9.0, 4.5 / 5.0]),
            (0, 4, [8.5 / 9.0, 0.5 / 5.0]),
            (1, 3, [6.5 / 9.0, 1.5 / 5.0]),
        ] {
            assert_eq!(displacement_lightmap_uv(column, row, 4, &face), expected);
        }
    }

    #[test]
    fn atlas_uvs_address_the_flat_plane_interior_and_directional_stride() {
        use super::super::MapVertex;
        use super::super::mesh::{BuildMeshVisibility, BuildVertex, GeometryPartition};
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [1, 1];
        let block = extract_face_lightmap(&face, &[sample(128, 128, 128); 16], true).unwrap();
        let mut mesh = BuildMesh {
            vertices: vec![BuildVertex {
                vertex: MapVertex {
                    position: Vec3::ZERO,
                    tangent_s: Vec3::ZERO,
                    tangent_t: Vec3::ZERO,
                    normal: Vec3::new(0.0, 0.0, 1.0),
                    tex_s: 0.0,
                    tex_t: 0.0,
                    texture_size: [1, 1],
                    lightmap_uv: [0.25, 0.75],
                    has_lightmap: true,
                    lightmap_bump_step: 0.0,
                    blend_alpha: 0.0,
                    opacity: 1.0,
                },
                lightmap_block: Some(0),
            }],
            indices: Vec::new(),
            material_index: 0,
            partition: GeometryPartition::Visible,
            entity: false,
            visibility: BuildMeshVisibility::default(),
            surface_owners: Vec::new(),
        };
        rewrite_lightmap_uvs(
            &mut [&mut mesh],
            &[block],
            &[Some(AtlasRect { x: 2, y: 3 })],
            32,
            16,
        );
        assert_eq!(
            mesh.vertices[0].vertex.lightmap_uv,
            [3.5 / 32.0, 5.5 / 16.0]
        );
        assert_eq!(mesh.vertices[0].vertex.lightmap_bump_step, 4.0 / 32.0);
        assert!(mesh.vertices[0].vertex.has_lightmap);
        rewrite_lightmap_uvs(&mut [&mut mesh], &[], &[None], 32, 16);
        assert_eq!(mesh.vertices[0].vertex.lightmap_uv, [0.0; 2]);
        assert_eq!(mesh.vertices[0].vertex.lightmap_bump_step, 0.0);
        assert!(!mesh.vertices[0].vertex.has_lightmap);
    }

    #[test]
    fn next_distinct_face_payload_bounds_the_current_style_bundle() {
        let first = face([0, 32, 255, 255]);
        let mut second = face([0, 33, 255, 255]);
        second.light_offset = 4;
        let ends = lightmap_sample_ends(&[first, second, second], 3);
        assert_eq!(ends[&0], 1);
        assert_eq!(ends[&4], 3);
        let samples = [sample(255, 0, 0); 3];
        assert!(extract_face_lightmap(&first, &samples[..ends[&0]], false).is_none());
        assert!(extract_face_lightmap(&second, &samples[..ends[&4]], false).is_some());
    }

    #[test]
    fn contiguous_face_payloads_without_average_prefixes_are_accepted() {
        let first = face([0, 32, 255, 255]);
        let mut second = face([0, 255, 255, 255]);
        second.light_offset = 8;
        let samples = [sample(255, 0, 0), sample(0, 255, 0), sample(0, 0, 255)];
        let ends = lightmap_sample_ends(&[first, second], samples.len());
        let first_block = extract_face_lightmap(&first, &samples[..ends[&0]], false).unwrap();
        let second_block = extract_face_lightmap(&second, &samples[..ends[&8]], false).unwrap();
        assert_eq!(first_block.samples, samples[..2]);
        assert_eq!(second_block.samples, samples[2..]);
        assert_eq!(first_block.styles, [0, 32]);
        assert_eq!(second_block.styles, [0]);
    }

    #[test]
    fn source_ldr_conversion_preserves_overbright_before_gamma_and_clamp() {
        let flat = |value| convert_lightmap_luxel([Vec3::new(value, value, value); 4], 1)[0];
        assert_eq!(flat(0.0), [0, 0, 0, 255]);
        assert_eq!(flat(0.25), [68, 68, 68, 255]);
        assert_eq!(flat(1.0), [128, 128, 128, 255]);
        assert_eq!(flat(2.0), [175, 175, 175, 255]);
        assert_eq!(flat(4.0), [239, 239, 239, 255]);
        // Unequal directional contributions may exceed1 before conversion;
        // the engine trades directionality for brightness across the lobes.
        let converted = convert_lightmap_luxel(
            [
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(3.0, 0.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(0.0, 0.0, 3.0),
            ],
            4,
        );
        assert!(converted[1][0] > converted[1][1]);
        assert!(converted[1][1] > 0);
        assert!(converted[2][1] > converted[2][0]);
        assert!(converted[3][2] > converted[3][0]);
    }

    #[test]
    fn complete_style_bundles_are_validated_before_reading() {
        let mut face = face([0, 32, 255, 7]);
        let samples = [sample(255, 0, 0); 8];
        assert!(extract_face_lightmap(&face, &samples[..7], true).is_none());
        assert!(extract_face_lightmap(&face, &samples, true).is_some());
        face.light_offset = 1;
        assert!(extract_face_lightmap(&face, &samples, false).is_none());
        face.light_offset = i32::MAX - 3;
        assert!(extract_face_lightmap(&face, &samples, false).is_none());
        face.light_offset = 0;
        face.styles = [255, 0, 0, 0];
        assert!(extract_face_lightmap(&face, &samples, false).is_none());
    }

    #[test]
    fn styles_sum_raw_samples_before_conversion_and_keep_directional_stride() {
        let samples = [
            sample(128, 0, 0),
            sample(128, 0, 0),
            sample(64, 0, 0),
            sample(0, 0, 0),
            sample(0, 128, 0),
            sample(0, 64, 0),
            sample(0, 128, 0),
            sample(0, 0, 0),
        ];
        let block = extract_face_lightmap(&face([0, 32, 255, 255]), &samples, true).unwrap();
        assert_eq!((block.width, block.height), (12, 3));
        let mut values = [0.0; 256];
        values[0] = 1.0;
        values[32] = 1.0;
        let pixels = block.render(&values, LightmapSource::Ldr);
        let pixel = |basis: usize| {
            <[u8; 4]>::try_from(&pixels[(block.width + basis * 3 + 1) * 4..][..4]).unwrap()
        };
        assert_eq!(pixel(0), [93, 93, 0, 255]);
        assert!(pixel(1)[0] > pixel(1)[1] && pixel(1)[1] > 0);
        assert!(pixel(2)[1] > pixel(2)[0] && pixel(2)[0] > 0);
        assert_eq!(pixel(3), [0, 0, 0, 255]);
        values[32] = 0.0;
        let off = block.render(&values, LightmapSource::Ldr);
        assert_eq!(off[(block.width + 1) * 4 + 1], 0);
    }

    #[test]
    fn every_lightmap_plane_repeats_its_own_gutters() {
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [1, 1];
        let samples = (0..16)
            .map(|index| sample((index + 1) * 10, 0, 0))
            .collect::<Vec<_>>();
        let block = extract_face_lightmap(&face, &samples, true).unwrap();
        let pixels = block.render(&[1.0; 256], LightmapSource::Ldr);
        let pixel = |x: usize, y: usize| {
            <[u8; 4]>::try_from(&pixels[(y * block.width + x) * 4..][..4]).unwrap()
        };
        for basis in 0..4 {
            let x = basis * 4;
            assert_eq!(pixel(x, 0), pixel(x + 1, 1));
            assert_eq!(pixel(x + 3, 3), pixel(x + 2, 2));
            assert_eq!(pixel(x, 2), pixel(x + 1, 2));
            assert_eq!(pixel(x + 3, 1), pixel(x + 2, 1));
        }
    }

    #[test]
    fn style_updates_only_regenerate_affected_patches() {
        let block0 =
            extract_face_lightmap(&face([0, 255, 255, 255]), &[sample(255, 0, 0)], false).unwrap();
        let block32 =
            extract_face_lightmap(&face([32, 255, 255, 255]), &[sample(0, 255, 0)], false).unwrap();
        let atlas = LightmapAtlas {
            rgba: Vec::new(),
            width: 8,
            height: 4,
            source: LightmapSource::Ldr,
            patches: vec![
                LightmapPatch {
                    origin: [0, 0],
                    block: block0,
                },
                LightmapPatch {
                    origin: [3, 0],
                    block: block32,
                },
            ],
            styles: lightstyle_patterns(&[]),
        };
        let previous = atlas.style_values(0.0);
        let mut next = previous;
        next[32] = 0.0;
        let updates = atlas.updates(&previous, &next);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].origin, [3, 0]);
        assert!(
            updates[0]
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 0, 0, 255])
        );
    }
}
