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

    /// Compiled, unevaluated samples.
    pub fn samples(&self) -> &[MapAmbientSample] {
        &self.samples
    }

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

    pub fn leaf_bounds(&self, leaf_index: usize) -> Option<([i16; 3], [i16; 3])> {
        self.locator
            .leaves
            .get(leaf_index)
            .map(|leaf| (leaf.mins, leaf.maxs))
    }

    /// Interpolate at the caller-selected radiosity cache sample.
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
            // Relative offsets preserve precision far from the origin.
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LightmapPageId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapSet {
    pub source: LightmapSource,
    pages: Vec<LightmapPage>,
    styles: Vec<Vec<u8>>,
    active_styles: Vec<u8>,
    animated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapPage {
    pub width: u32,
    pub height: u32,
    patches: Vec<LightmapPatch>,
    active_styles: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LightmapPageError {
    InvalidPage,
    InvalidDimensions,
    AllocationFailed,
}

impl std::fmt::Display for LightmapPageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for LightmapPageError {}

impl LightmapPage {
    pub fn active_styles(&self) -> &[u8] {
        &self.active_styles
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LightmapSource {
    Ldr,
    Hdr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingLightmapBlock {
    pub(super) sample_start: usize,
    pub(super) face_width: usize,
    pub(super) face_height: usize,
    pub(super) styles: Vec<u8>,
    pub(super) samples: Vec<ColorRgbExp>,
    pub(super) sample_basis: LightmapBasis,
    pub(super) output_basis: LightmapBasis,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum LightmapBasis {
    Scalar,
    Directional,
}

impl LightmapBasis {
    pub(super) const fn from_bumped(bumped: bool) -> Self {
        if bumped {
            Self::Directional
        } else {
            Self::Scalar
        }
    }

    const fn count(self) -> usize {
        match self {
            Self::Scalar => 1,
            Self::Directional => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LightmapRect {
    pub(super) x: usize,
    pub(super) y: usize,
}

pub const LIGHTMAP_PAGE_SIZE: usize = 1024;

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

/// Bound payloads at the next offset; average-prefix records are optional.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LightmapExtractionError {
    InvalidPayload,
}

impl super::MapLightmapStats {
    pub(super) fn record_extraction(
        &mut self,
        result: &Result<Option<PendingLightmapBlock>, LightmapExtractionError>,
    ) {
        match result {
            Ok(Some(_)) => self.valid_payloads = self.valid_payloads.saturating_add(1),
            Ok(None) => self.missing_payloads = self.missing_payloads.saturating_add(1),
            Err(LightmapExtractionError::InvalidPayload) => {
                self.invalid_payloads = self.invalid_payloads.saturating_add(1);
            }
        }
    }
}

pub(super) fn extract_face_lightmap(
    face: &Face,
    samples: &[ColorRgbExp],
    sample_basis: LightmapBasis,
    output_basis: LightmapBasis,
) -> Result<Option<PendingLightmapBlock>, LightmapExtractionError> {
    if face.light_offset == -1 {
        return Ok(None);
    }
    if face.light_offset < -1 || face.light_offset % 4 != 0 {
        return Err(LightmapExtractionError::InvalidPayload);
    }
    let styles = face
        .styles
        .into_iter()
        .take_while(|style| *style != 255)
        .collect::<Vec<_>>();
    if styles.is_empty() {
        return Err(LightmapExtractionError::InvalidPayload);
    }
    let (face_width, face_height, start, end) = (|| {
        let face_width = lightmap_axis_luxels(face.lightmap_size[0])?;
        let face_height = lightmap_axis_luxels(face.lightmap_size[1])?;
        let count = face_width
            .checked_mul(face_height)?
            .checked_mul(sample_basis.count())?
            .checked_mul(styles.len())?;
        let start = usize::try_from(face.light_offset).ok()? / 4;
        let end = start.checked_add(count)?;
        face_width
            .checked_add(2)?
            .checked_mul(output_basis.count())?;
        face_height.checked_add(2)?;
        Some((face_width, face_height, start, end))
    })()
    .ok_or(LightmapExtractionError::InvalidPayload)?;
    let samples = samples
        .get(start..end)
        .ok_or(LightmapExtractionError::InvalidPayload)?;
    Ok(Some(PendingLightmapBlock {
        sample_start: start,
        face_width,
        face_height,
        styles,
        samples: samples.to_vec(),
        sample_basis,
        output_basis,
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LightmapPatch {
    origin: [u32; 2],
    region: LightmapRegion,
    block: std::sync::Arc<PendingLightmapBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LightmapRegion {
    offset: [usize; 2],
    size: [usize; 2],
}

impl LightmapRegion {
    fn dimensions(self, basis: LightmapBasis) -> [usize; 2] {
        [(self.size[0] + 2) * basis.count(), self.size[1] + 2]
    }
}

impl LightmapSet {
    fn from_pages(source: LightmapSource, pages: Vec<LightmapPage>, styles: Vec<Vec<u8>>) -> Self {
        let active_styles = pages
            .iter()
            .flat_map(|page| page.active_styles.iter().copied())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let animated = active_styles.iter().any(|style| {
            let pattern = &styles[usize::from(*style)];
            pattern.iter().any(|value| *value != pattern[0])
        });
        Self {
            source,
            pages,
            styles,
            active_styles,
            animated,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn style_pages_for_tests(
        source: LightmapSource,
        pages: &[Vec<(u8, [u8; 4])>],
        patterns: &[(u8, &str)],
    ) -> Self {
        let mut styles = lightstyle_patterns(&[]);
        for &(style, pattern) in patterns {
            assert!(!pattern.is_empty() && pattern.bytes().all(|value| value >= b'a'));
            styles[usize::from(style)] = pattern.as_bytes().to_vec();
        }
        let pages = pages
            .iter()
            .enumerate()
            .map(|(index, samples)| {
                assert!(!samples.is_empty() && samples.len() <= 4);
                let block = PendingLightmapBlock {
                    sample_start: index,
                    face_width: 1,
                    face_height: 1,
                    styles: samples.iter().map(|(style, _)| *style).collect(),
                    samples: samples
                        .iter()
                        .map(|(_, sample)| ColorRgbExp {
                            r: sample[0],
                            g: sample[1],
                            b: sample[2],
                            exponent: sample[3] as i8,
                        })
                        .collect(),
                    sample_basis: LightmapBasis::Scalar,
                    output_basis: LightmapBasis::Scalar,
                };
                LightmapPage {
                    width: 4,
                    height: 4,
                    active_styles: block.styles.clone(),
                    patches: vec![LightmapPatch {
                        origin: [0, 0],
                        region: LightmapRegion {
                            offset: [0, 0],
                            size: [1, 1],
                        },
                        block: std::sync::Arc::new(block),
                    }],
                }
            })
            .collect();
        Self::from_pages(source, pages, styles)
    }

    pub fn pages(&self) -> &[LightmapPage] {
        &self.pages
    }

    pub fn page(&self, id: LightmapPageId) -> Option<&LightmapPage> {
        self.pages.get(id.0 as usize)
    }

    pub fn render_page(
        &self,
        id: LightmapPageId,
        values: &[f32; 256],
    ) -> Result<Vec<u8>, LightmapPageError> {
        let page = self.page(id).ok_or(LightmapPageError::InvalidPage)?;
        let width = page.width as usize;
        let height = page.height as usize;
        if width == 0 || height == 0 || width > LIGHTMAP_PAGE_SIZE || height > LIGHTMAP_PAGE_SIZE {
            return Err(LightmapPageError::InvalidDimensions);
        }
        let size = width
            .checked_mul(height)
            .and_then(|size| size.checked_mul(4))
            .ok_or(LightmapPageError::InvalidDimensions)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(size)
            .map_err(|_| LightmapPageError::AllocationFailed)?;
        rgba.resize(size, 128);
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        for patch in &page.patches {
            let [patch_width, patch_height] = patch.region.dimensions(patch.block.output_basis);
            if (patch.origin[0] as usize)
                .checked_add(patch_width)
                .is_none_or(|end| end > width)
                || (patch.origin[1] as usize)
                    .checked_add(patch_height)
                    .is_none_or(|end| end > height)
            {
                return Err(LightmapPageError::InvalidDimensions);
            }
            let pixels = patch.block.render_region(patch.region, values, self.source);
            for row in 0..patch_height {
                let target =
                    ((patch.origin[1] as usize + row) * width + patch.origin[0] as usize) * 4;
                let source = row * patch_width * 4;
                rgba[target..target + patch_width * 4]
                    .copy_from_slice(&pixels[source..source + patch_width * 4]);
            }
        }
        Ok(rgba)
    }

    pub fn active_styles(&self) -> Vec<u8> {
        self.active_styles.clone()
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
        self.animated
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
    #[cfg(test)]
    fn dimensions(&self) -> [usize; 2] {
        LightmapRegion {
            offset: [0, 0],
            size: [self.face_width, self.face_height],
        }
        .dimensions(self.output_basis)
    }

    #[cfg(test)]
    fn render(&self, values: &[f32; 256], source: LightmapSource) -> Vec<u8> {
        self.render_region(
            LightmapRegion {
                offset: [0, 0],
                size: [self.face_width, self.face_height],
            },
            values,
            source,
        )
    }

    fn render_region(
        &self,
        region: LightmapRegion,
        values: &[f32; 256],
        source: LightmapSource,
    ) -> Vec<u8> {
        let [width, height] = region.dimensions(self.output_basis);
        let mut rgba = vec![0; width * height * 4];
        for y in 0..height {
            let sample_y = (region.offset[1] + y)
                .saturating_sub(1)
                .min(self.face_height - 1);
            for x in 0..region.size[0] + 2 {
                let sample_x = (region.offset[0] + x)
                    .saturating_sub(1)
                    .min(self.face_width - 1);
                let colors = self.luxel(sample_y * self.face_width + sample_x, values, source);
                for (basis, color) in colors.iter().enumerate().take(self.output_basis.count()) {
                    let index = (y * width + basis * (region.size[0] + 2) + x) * 4;
                    rgba[index..index + 4].copy_from_slice(color);
                }
            }
        }
        rgba
    }

    fn luxel(&self, luxel: usize, values: &[f32; 256], source: LightmapSource) -> [[u8; 4]; 4] {
        let luxels = self.face_width * self.face_height;
        let mut linear = [Vec3::ZERO; 4];
        if self.sample_basis == LightmapBasis::Scalar
            && self.output_basis == LightmapBasis::Directional
        {
            linear[1..].fill(Vec3::new(1.0, 1.0, 1.0));
        }
        for (style_index, style) in self.styles.iter().enumerate() {
            let weight = values[usize::from(*style)];
            for (basis, color) in linear
                .iter_mut()
                .enumerate()
                .take(self.sample_basis.count())
            {
                *color += decode_light_sample_linear(
                    self.samples
                        [(style_index * self.sample_basis.count() + basis) * luxels + luxel],
                ) * weight;
            }
        }
        match source {
            LightmapSource::Ldr => convert_lightmap_luxel(linear, self.output_basis),
            LightmapSource::Hdr => {
                if self.output_basis == LightmapBasis::Directional {
                    let directions = normalized_lightmap_directions(linear, linear[0]);
                    linear[1..].copy_from_slice(&directions);
                }
                linear.map(|color| {
                    [
                        linear_to_srgb_byte(color[0]),
                        linear_to_srgb_byte(color[1]),
                        linear_to_srgb_byte(color[2]),
                        255,
                    ]
                })
            }
        }
    }
}

fn lightmap_gamma_goal(value: f32) -> f32 {
    let index = (value * 1024.0).round().clamp(0.0, 4095.0);
    if index.is_nan() {
        return 1.0;
    }
    static GOALS: std::sync::OnceLock<[f32; 4096]> = std::sync::OnceLock::new();
    GOALS.get_or_init(|| {
        std::array::from_fn(|index| ((index as f32 / 1024.0).powf(1.0 / 2.2) * 0.5).min(1.0))
    })[index as usize]
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

fn convert_lightmap_luxel(linear: [Vec3; 4], output_basis: LightmapBasis) -> [[u8; 4]; 4] {
    let goal = Vec3::from(linear[0].to_array().map(lightmap_gamma_goal));
    let mut colors = [[0, 0, 0, 255]; 4];
    colors[0] = lightmap_bytes(goal);
    if output_basis == LightmapBasis::Directional {
        let mut directions = normalized_lightmap_directions(linear, goal);
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

fn normalized_lightmap_directions(linear: [Vec3; 4], goal: Vec3) -> [Vec3; 3] {
    let average = (linear[1] + linear[2] + linear[3]) / 3.0;
    let correction = Vec3::from(std::array::from_fn(|channel| {
        if average[channel] != 0.0 {
            goal[channel] / average[channel]
        } else {
            0.0
        }
    }));
    [linear[1], linear[2], linear[3]].map(|color| {
        Vec3::from(std::array::from_fn(|channel| {
            color[channel] * correction[channel]
        }))
    })
}

pub(super) fn lightmap_axis_luxels(size: i32) -> Option<usize> {
    usize::try_from(size).ok()?.checked_add(1)
}

#[cfg(test)]
pub(super) fn decode_light_sample(sample: ColorRgbExp) -> [u8; 4] {
    convert_lightmap_luxel(
        [decode_light_sample_linear(sample); 4],
        LightmapBasis::Scalar,
    )[0]
}

pub(super) fn decode_light_sample_linear(sample: ColorRgbExp) -> Vec3 {
    static SCALES: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    let scale = SCALES.get_or_init(|| {
        std::array::from_fn(|exponent| 2.0_f32.powi(i32::from(exponent as u8 as i8)) / 255.0)
    })[usize::from(sample.exponent as u8)];
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
    // Grid columns advance V along rotated corner 0 -> 1.
    [
        displacement_lightmap_axis_uv(row as f32 / steps, face.lightmap_size[0]),
        displacement_lightmap_axis_uv(column as f32 / steps, face.lightmap_size[1]),
    ]
}

pub(super) fn displacement_lightmap_axis_uv(grid_uv: f32, size: i32) -> f32 {
    let size = size.max(0) as f32;
    (grid_uv * size + 0.5) / (size + 1.0)
}

#[derive(Clone, Copy, Debug)]
struct LightmapPlacement {
    page: LightmapPageId,
    rect: LightmapRect,
    region: LightmapRegion,
}

pub(super) fn bake_lightmap_set<'a>(
    meshes: impl IntoIterator<Item = &'a mut BuildMesh>,
    blocks: Vec<PendingLightmapBlock>,
    source: Option<LightmapSource>,
    entities: &[MapEntity],
) -> Option<LightmapSet> {
    let mut meshes = meshes.into_iter().collect::<Vec<_>>();
    let Some(source) = source.filter(|_| !blocks.is_empty()) else {
        clear_lightmap_uvs(&mut meshes);
        return None;
    };
    let mut canonical = Vec::with_capacity(blocks.len());
    let mut sources = std::collections::HashMap::new();
    let blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            let key = (
                block.sample_start,
                block.face_width,
                block.face_height,
                block.styles.clone(),
                block.sample_basis,
                block.output_basis,
            );
            canonical.push(*sources.entry(key).or_insert(index));
            std::sync::Arc::new(block)
        })
        .collect::<Vec<_>>();
    let regions = blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| canonical[*index] == *index)
        .flat_map(|(index, block)| {
            let tile_width = LIGHTMAP_PAGE_SIZE / block.output_basis.count() - 2;
            (0..block.face_height)
                .step_by(LIGHTMAP_PAGE_SIZE - 2)
                .flat_map(move |y| {
                    (0..block.face_width).step_by(tile_width).map(move |x| {
                        (
                            index,
                            LightmapRegion {
                                offset: [x, y],
                                size: [
                                    tile_width.min(block.face_width - x),
                                    (LIGHTMAP_PAGE_SIZE - 2).min(block.face_height - y),
                                ],
                            },
                        )
                    })
                })
        })
        .collect::<Vec<_>>();
    let dimensions = regions
        .iter()
        .map(|(index, region)| region.dimensions(blocks[*index].output_basis))
        .collect::<Vec<_>>();
    let (page_dimensions, placements) = pack_lightmap_regions(&dimensions);
    let mut pages = page_dimensions
        .into_iter()
        .map(|[width, height]| LightmapPage {
            width: width as u32,
            height: height as u32,
            patches: Vec::new(),
            active_styles: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut block_placements = vec![Vec::new(); blocks.len()];
    for ((index, region), (page, rect)) in regions.into_iter().zip(placements) {
        pages[page.0 as usize].patches.push(LightmapPatch {
            origin: [rect.x as u32, rect.y as u32],
            region,
            block: std::sync::Arc::clone(&blocks[index]),
        });
        block_placements[index].push(LightmapPlacement { page, rect, region });
    }
    for (index, &canonical) in canonical.iter().enumerate() {
        if index != canonical {
            let (previous, current) = block_placements.split_at_mut(index);
            current[0].clone_from(&previous[canonical]);
        }
    }
    for page in &mut pages {
        page.active_styles = page
            .patches
            .iter()
            .flat_map(|patch| patch.block.styles.iter().copied())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    for mesh in meshes {
        rewrite_lightmap_mesh(mesh, &blocks, &block_placements, &pages);
    }
    Some(LightmapSet::from_pages(
        source,
        pages,
        lightstyle_patterns(entities),
    ))
}

pub(super) fn clear_lightmap_uvs(meshes: &mut [&mut BuildMesh]) {
    for vertex in meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
        clear_lightmap_vertex(vertex);
    }
}

fn clear_lightmap_vertex(vertex: &mut super::mesh::BuildVertex) {
    vertex.vertex.lightmap_uv = [0.0; 2];
    vertex.vertex.lightmap_page = None;
    vertex.vertex.lightmap_bump_step = 0.0;
    vertex.lightmap_block = None;
}

fn placed_lightmap_vertex(
    mut vertex: super::mesh::BuildVertex,
    block: &PendingLightmapBlock,
    placement: LightmapPlacement,
    pages: &[LightmapPage],
) -> super::mesh::BuildVertex {
    let page = &pages[placement.page.0 as usize];
    vertex.vertex.lightmap_page = Some(placement.page);
    vertex.vertex.lightmap_bump_step = if block.output_basis == LightmapBasis::Directional {
        (placement.region.size[0] + 2) as f32 / page.width as f32
    } else {
        0.0
    };
    vertex.vertex.lightmap_uv = [
        (placement.rect.x as f32 + 1.0 + vertex.vertex.lightmap_uv[0] * block.face_width as f32
            - placement.region.offset[0] as f32)
            / page.width as f32,
        (placement.rect.y as f32 + 1.0 + vertex.vertex.lightmap_uv[1] * block.face_height as f32
            - placement.region.offset[1] as f32)
            / page.height as f32,
    ];
    vertex
}

fn rewrite_lightmap_mesh(
    mesh: &mut BuildMesh,
    blocks: &[std::sync::Arc<PendingLightmapBlock>],
    placements: &[Vec<LightmapPlacement>],
    pages: &[LightmapPage],
) {
    let tiled = mesh.vertices.iter().any(|vertex| {
        vertex
            .lightmap_block
            .is_some_and(|index| placements[index].len() > 1)
    });
    if !tiled {
        for vertex in &mut mesh.vertices {
            if let Some(index) = vertex.lightmap_block {
                *vertex =
                    placed_lightmap_vertex(*vertex, &blocks[index], placements[index][0], pages);
            } else {
                clear_lightmap_vertex(vertex);
            }
        }
        return;
    }
    let old_vertices = std::mem::take(&mut mesh.vertices);
    let old_indices = std::mem::take(&mut mesh.indices);
    let mut remap = vec![0_u32; old_indices.len() + 1];
    let mut reused = std::collections::HashMap::new();
    for (triangle_index, triangle) in old_indices.chunks_exact(3).enumerate() {
        let start = triangle_index * 3;
        remap[start..start + 3].fill(mesh.indices.len() as u32);
        let vertices =
            std::array::from_fn::<_, 3, _>(|index| old_vertices[triangle[index] as usize]);
        let index = vertices[0].lightmap_block;
        assert!(vertices.iter().all(|vertex| vertex.lightmap_block == index));
        if let Some(index) = index.filter(|index| placements[*index].len() > 1) {
            let block = &blocks[index];
            let bounds = std::array::from_fn::<_, 2, _>(|axis| {
                vertices
                    .iter()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), vertex| {
                        (
                            min.min(vertex.vertex.lightmap_uv[axis]),
                            max.max(vertex.vertex.lightmap_uv[axis]),
                        )
                    })
            });
            for &placement in &placements[index] {
                if (0..2).any(|axis| {
                    let extent = [block.face_width, block.face_height][axis];
                    let offset = placement.region.offset[axis];
                    let end = offset + placement.region.size[axis];
                    (offset != 0 && bounds[axis].1 < offset as f32 / extent as f32)
                        || (end != extent && bounds[axis].0 >= end as f32 / extent as f32)
                }) {
                    continue;
                }
                let mut polygon = vertices
                    .iter()
                    .map(|vertex| vertex.vertex)
                    .collect::<Vec<_>>();
                for axis in 0..2 {
                    let extent = [block.face_width, block.face_height][axis];
                    if placement.region.offset[axis] != 0 {
                        polygon = clip_lightmap_polygon(
                            &polygon,
                            axis,
                            placement.region.offset[axis] as f32 / extent as f32,
                            true,
                        );
                    }
                    let end = placement.region.offset[axis] + placement.region.size[axis];
                    if end != extent {
                        polygon = clip_lightmap_polygon(
                            &polygon,
                            axis,
                            end as f32 / extent as f32,
                            false,
                        );
                    }
                }
                if polygon.len() < 3 {
                    continue;
                }
                let base = mesh.vertices.len() as u32;
                mesh.vertices.extend(polygon.iter().map(|vertex| {
                    placed_lightmap_vertex(
                        super::mesh::BuildVertex {
                            vertex: *vertex,
                            lightmap_block: Some(index),
                        },
                        block,
                        placement,
                        pages,
                    )
                }));
                for corner in 1..polygon.len() - 1 {
                    let triangle = [
                        polygon[0].position,
                        polygon[corner].position,
                        polygon[corner + 1].position,
                    ];
                    if (triangle[1] - triangle[0])
                        .cross(triangle[2] - triangle[0])
                        .length_squared()
                        == 0.0
                    {
                        continue;
                    }
                    mesh.indices
                        .extend([base, base + corner as u32, base + corner as u32 + 1]);
                }
            }
        } else {
            for (&old, mut vertex) in triangle.iter().zip(vertices) {
                let new = *reused.entry(old).or_insert_with(|| {
                    if let Some(index) = vertex.lightmap_block {
                        vertex = placed_lightmap_vertex(
                            vertex,
                            &blocks[index],
                            placements[index][0],
                            pages,
                        );
                    } else {
                        clear_lightmap_vertex(&mut vertex);
                    }
                    let new = mesh.vertices.len() as u32;
                    mesh.vertices.push(vertex);
                    new
                });
                mesh.indices.push(new);
            }
        }
        remap[start + 3] = mesh.indices.len() as u32;
    }
    for range in mesh
        .visibility
        .always_visible
        .iter_mut()
        .chain(mesh.visibility.clusters.values_mut().flatten())
    {
        let end = range.start as usize + range.count as usize;
        let start = remap[range.start as usize];
        range.count = remap[end] - start;
        range.start = start;
    }
    for owner in &mut mesh.surface_owners {
        owner.index_start = remap[owner.index_start as usize];
    }
}

fn clip_lightmap_polygon(
    polygon: &[super::MapVertex],
    axis: usize,
    edge: f32,
    keep_above: bool,
) -> Vec<super::MapVertex> {
    let distance = |vertex: &super::MapVertex| {
        if keep_above {
            vertex.lightmap_uv[axis] - edge
        } else {
            edge - vertex.lightmap_uv[axis]
        }
    };
    let mut result = Vec::new();
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let da = distance(&a);
        let db = distance(&b);
        if da >= 0.0 {
            result.push(a);
        }
        if (da > 0.0 && db < 0.0) || (da < 0.0 && db > 0.0) {
            result.push(super::projection::interpolate_surface(a, b, da / (da - db)));
        }
    }
    result
}

fn pack_lightmap_regions(
    dimensions: &[[usize; 2]],
) -> (Vec<[usize; 2]>, Vec<(LightmapPageId, LightmapRect)>) {
    let mut width = dimensions
        .iter()
        .map(|size| size[0])
        .max()
        .unwrap_or(1)
        .next_power_of_two();
    let mut height = dimensions
        .iter()
        .map(|size| size[1])
        .max()
        .unwrap_or(1)
        .next_power_of_two();
    loop {
        if let Some(placements) = shelf_pack_regions(dimensions, width, height, false) {
            return (vec![[width, height]], placements);
        }
        if width == LIGHTMAP_PAGE_SIZE && height == LIGHTMAP_PAGE_SIZE {
            let placements = shelf_pack_regions(dimensions, width, height, true)
                .expect("bounded lightmap regions");
            let count = placements.last().map_or(0, |(page, _)| page.0 as usize + 1);
            return (vec![[width, height]; count], placements);
        }
        if (width <= height && width < LIGHTMAP_PAGE_SIZE) || height == LIGHTMAP_PAGE_SIZE {
            width = (width * 2).min(LIGHTMAP_PAGE_SIZE);
        } else {
            height = (height * 2).min(LIGHTMAP_PAGE_SIZE);
        }
    }
}

fn shelf_pack_regions(
    dimensions: &[[usize; 2]],
    width: usize,
    height: usize,
    multiple_pages: bool,
) -> Option<Vec<(LightmapPageId, LightmapRect)>> {
    let mut placements = Vec::with_capacity(dimensions.len());
    let (mut x, mut y, mut shelf_height, mut page) = (0, 0, 0, 0_u32);
    for &[block_width, block_height] in dimensions {
        if block_width > width || block_height > height {
            return None;
        }
        if x + block_width > width {
            y += shelf_height;
            x = 0;
            shelf_height = 0;
        }
        if y + block_height > height {
            if !multiple_pages {
                return None;
            }
            page = page.checked_add(1).expect("lightmap page count");
            x = 0;
            y = 0;
            shelf_height = 0;
        }
        placements.push((LightmapPageId(page), LightmapRect { x, y }));
        x += block_width;
        shelf_height = shelf_height.max(block_height);
    }
    Some(placements)
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

    fn scalar_gamma_goal(value: f32) -> f32 {
        let index = (value * 1024.0).round().clamp(0.0, 4095.0);
        ((index / 1024.0).powf(1.0 / 2.2) * 0.5).min(1.0)
    }

    fn scalar_decode_light_sample_linear(sample: ColorRgbExp) -> Vec3 {
        let scale = 2.0_f32.powi(i32::from(sample.exponent)) / 255.0;
        Vec3::new(
            f32::from(sample.r) * scale,
            f32::from(sample.g) * scale,
            f32::from(sample.b) * scale,
        )
    }

    #[test]
    fn lightmap_gamma_table_matches_every_bin_and_rounding_boundary_bit_exactly() {
        let check = |value| {
            assert_eq!(
                lightmap_gamma_goal(value).to_bits(),
                scalar_gamma_goal(value).to_bits(),
                "gamma input {value:?}"
            );
        };
        for index in 0..4096 {
            check(index as f32 / 1024.0);
            let boundary = (index as f32 + 0.5) / 1024.0;
            check(boundary.next_down());
            check(boundary);
            check(boundary.next_up());
        }
        for value in [
            -0.0,
            0.0,
            f32::from_bits(1),
            -f32::from_bits(1),
            -1.0,
            -f32::MAX,
            f32::MAX,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NAN,
            f32::from_bits(0x7fc0_1234),
            f32::from_bits(0xffc0_1234),
        ] {
            check(value);
        }
    }

    #[test]
    fn lightmap_exponent_table_preserves_every_exponent_and_mantissa_bit_exactly() {
        for exponent in i8::MIN..=i8::MAX {
            for channel in u8::MIN..=u8::MAX {
                let sample = ColorRgbExp {
                    r: channel,
                    g: channel.wrapping_add(85),
                    b: channel.wrapping_add(170),
                    exponent,
                };
                assert_eq!(
                    decode_light_sample_linear(sample)
                        .to_array()
                        .map(f32::to_bits),
                    scalar_decode_light_sample_linear(sample)
                        .to_array()
                        .map(f32::to_bits),
                    "sample {sample:?}"
                );
            }
        }
    }

    fn benchmark_lightmap_arithmetic(
        samples: &[ColorRgbExp],
        decode: impl Fn(ColorRgbExp) -> Vec3,
        gamma: impl Fn(f32) -> f32,
    ) -> u64 {
        let mut checksum = 0_u64;
        for _ in 0..64 {
            for pair in samples.chunks_exact(2) {
                let linear = decode(std::hint::black_box(pair[0])) * 0.75
                    + decode(std::hint::black_box(pair[1])) * 0.25;
                let goal = Vec3::from(linear.to_array().map(&gamma));
                checksum += u64::from(u32::from_le_bytes(lightmap_bytes(goal)));
            }
        }
        std::hint::black_box(checksum)
    }

    #[test]
    #[ignore = "release-only CPU microbenchmark; run without competing compilation"]
    fn benchmark_scalar_and_table_lightmap_cook_arithmetic() {
        assert!(
            !std::hint::black_box(cfg!(debug_assertions)),
            "run this benchmark with --release"
        );
        let samples = (0..16384_usize)
            .map(|index| ColorRgbExp {
                r: index as u8,
                g: (index / 3) as u8,
                b: (index / 7) as u8,
                exponent: (index % 12) as i8 - 8,
            })
            .collect::<Vec<_>>();
        std::hint::black_box(lightmap_gamma_goal(1.0));
        std::hint::black_box(decode_light_sample_linear(samples[0]));
        let mut times = [Vec::new(), Vec::new()];
        let mut expected = None;
        for round in 0..8 {
            for method in [round % 2, 1 - round % 2] {
                let start = std::time::Instant::now();
                let checksum = if method == 0 {
                    benchmark_lightmap_arithmetic(
                        &samples,
                        scalar_decode_light_sample_linear,
                        scalar_gamma_goal,
                    )
                } else {
                    benchmark_lightmap_arithmetic(
                        &samples,
                        decode_light_sample_linear,
                        lightmap_gamma_goal,
                    )
                };
                times[method].push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(*expected.get_or_insert(checksum), checksum);
            }
        }
        for values in &mut times {
            values.sort_by(f64::total_cmp);
        }
        let scalar = times[0][4];
        let table = times[1][4];
        eprintln!(
            "lightmap LUT arithmetic: samples=16384 pairs_per_run=524288 scalar_ms={scalar:.3} table_ms={table:.3} speedup={:.3} checksum={} scalar_runs_ms={:?} table_runs_ms={:?}",
            scalar / table,
            expected.unwrap(),
            times[0],
            times[1]
        );
    }

    #[test]
    fn displacement_lightmap_follows_rotated_base_corners_on_rectangular_atlas() {
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [8, 4];
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
        let block = extract_face_lightmap(
            &face,
            &[sample(128, 128, 128); 16],
            LightmapBasis::Directional,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let mut mesh = BuildMesh {
            vertices: vec![BuildVertex {
                vertex: MapVertex {
                    position: Vec3::ZERO,
                    base_position: Vec3::ZERO,
                    tangent_s: Vec3::ZERO,
                    tangent_t: Vec3::ZERO,
                    normal: Vec3::new(0.0, 0.0, 1.0),
                    tex_s: 0.0,
                    tex_t: 0.0,
                    texture_size: [1, 1],
                    lightmap_uv: [0.25, 0.75],
                    lightmap_page: Some(crate::scene::map::LightmapPageId(0)),
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
        let pages = [LightmapPage {
            width: 32,
            height: 16,
            patches: Vec::new(),
            active_styles: vec![0],
        }];
        rewrite_lightmap_mesh(
            &mut mesh,
            &[std::sync::Arc::new(block)],
            &[vec![LightmapPlacement {
                page: LightmapPageId(0),
                rect: LightmapRect { x: 2, y: 3 },
                region: LightmapRegion {
                    offset: [0, 0],
                    size: [2, 2],
                },
            }]],
            &pages,
        );
        assert_eq!(
            mesh.vertices[0].vertex.lightmap_uv,
            [3.5 / 32.0, 5.5 / 16.0]
        );
        assert_eq!(mesh.vertices[0].vertex.lightmap_bump_step, 4.0 / 32.0);
        assert_eq!(
            mesh.vertices[0].vertex.lightmap_page,
            Some(LightmapPageId(0))
        );
        clear_lightmap_uvs(&mut [&mut mesh]);
        assert_eq!(mesh.vertices[0].vertex.lightmap_uv, [0.0; 2]);
        assert_eq!(mesh.vertices[0].vertex.lightmap_bump_step, 0.0);
        assert_eq!(mesh.vertices[0].vertex.lightmap_page, None);
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
        assert!(
            extract_face_lightmap(
                &first,
                &samples[..ends[&0]],
                LightmapBasis::Scalar,
                LightmapBasis::Scalar
            )
            .is_err()
        );
        assert!(
            extract_face_lightmap(
                &second,
                &samples[..ends[&4]],
                LightmapBasis::Scalar,
                LightmapBasis::Scalar
            )
            .is_ok_and(|block| block.is_some())
        );
    }

    #[test]
    fn contiguous_face_payloads_without_average_prefixes_are_accepted() {
        let first = face([0, 32, 255, 255]);
        let mut second = face([0, 255, 255, 255]);
        second.light_offset = 8;
        let samples = [sample(255, 0, 0), sample(0, 255, 0), sample(0, 0, 255)];
        let ends = lightmap_sample_ends(&[first, second], samples.len());
        let first_block = extract_face_lightmap(
            &first,
            &samples[..ends[&0]],
            LightmapBasis::Scalar,
            LightmapBasis::Scalar,
        )
        .unwrap()
        .unwrap();
        let second_block = extract_face_lightmap(
            &second,
            &samples[..ends[&8]],
            LightmapBasis::Scalar,
            LightmapBasis::Scalar,
        )
        .unwrap()
        .unwrap();
        assert_eq!(first_block.samples, samples[..2]);
        assert_eq!(second_block.samples, samples[2..]);
        assert_eq!(first_block.styles, [0, 32]);
        assert_eq!(second_block.styles, [0]);
    }

    #[test]
    fn source_ldr_conversion_preserves_overbright_before_gamma_and_clamp() {
        let flat = |value| {
            convert_lightmap_luxel([Vec3::new(value, value, value); 4], LightmapBasis::Scalar)[0]
        };
        assert_eq!(flat(0.0), [0, 0, 0, 255]);
        assert_eq!(flat(0.25), [68, 68, 68, 255]);
        assert_eq!(flat(1.0), [128, 128, 128, 255]);
        assert_eq!(flat(2.0), [175, 175, 175, 255]);
        assert_eq!(flat(4.0), [239, 239, 239, 255]);
        let converted = convert_lightmap_luxel(
            [
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(3.0, 0.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(0.0, 0.0, 3.0),
            ],
            LightmapBasis::Directional,
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
        assert!(
            extract_face_lightmap(
                &face,
                &samples[..7],
                LightmapBasis::Directional,
                LightmapBasis::Directional
            )
            .is_err()
        );
        assert!(
            extract_face_lightmap(
                &face,
                &samples,
                LightmapBasis::Directional,
                LightmapBasis::Directional
            )
            .is_ok_and(|block| block.is_some())
        );
        face.light_offset = 1;
        assert!(
            extract_face_lightmap(
                &face,
                &samples,
                LightmapBasis::Scalar,
                LightmapBasis::Scalar
            )
            .is_err()
        );
        face.light_offset = i32::MAX - 3;
        assert!(
            extract_face_lightmap(
                &face,
                &samples,
                LightmapBasis::Scalar,
                LightmapBasis::Scalar
            )
            .is_err()
        );
        face.light_offset = 0;
        face.styles = [255, 0, 0, 0];
        assert!(
            extract_face_lightmap(
                &face,
                &samples,
                LightmapBasis::Scalar,
                LightmapBasis::Scalar
            )
            .is_err()
        );
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
        let block = extract_face_lightmap(
            &face([0, 32, 255, 255]),
            &samples,
            LightmapBasis::Directional,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        assert_eq!((block.dimensions()[0], block.dimensions()[1]), (12, 3));
        let mut values = [0.0; 256];
        values[0] = 1.0;
        values[32] = 1.0;
        let pixels = block.render(&values, LightmapSource::Ldr);
        let pixel = |basis: usize| {
            <[u8; 4]>::try_from(&pixels[(block.dimensions()[0] + basis * 3 + 1) * 4..][..4])
                .unwrap()
        };
        assert_eq!(pixel(0), [93, 93, 0, 255]);
        assert!(pixel(1)[0] > pixel(1)[1] && pixel(1)[1] > 0);
        assert!(pixel(2)[1] > pixel(2)[0] && pixel(2)[0] > 0);
        assert_eq!(pixel(3), [0, 0, 0, 255]);
        values[32] = 0.0;
        let off = block.render(&values, LightmapSource::Ldr);
        assert_eq!(off[(block.dimensions()[0] + 1) * 4 + 1], 0);
    }

    #[test]
    fn every_lightmap_plane_repeats_its_own_gutters() {
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [1, 1];
        let samples = (0..16)
            .map(|index| sample((index + 1) * 10, 0, 0))
            .collect::<Vec<_>>();
        let block = extract_face_lightmap(
            &face,
            &samples,
            LightmapBasis::Directional,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let pixels = block.render(&[1.0; 256], LightmapSource::Ldr);
        let pixel = |x: usize, y: usize| {
            <[u8; 4]>::try_from(&pixels[(y * block.dimensions()[0] + x) * 4..][..4]).unwrap()
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
    fn aggregate_styles_are_unique_and_only_used_patterns_control_animation() {
        let pages = [
            vec![(32, [32, 64, 96, 0])],
            vec![(0, [16, 8, 4, 0]), (32, [8, 4, 2, 0])],
        ];
        let fixed = LightmapSet::style_pages_for_tests(
            LightmapSource::Ldr,
            &pages,
            &[(32, "aaa"), (33, "am")],
        );
        assert_eq!(fixed.active_styles(), vec![0, 32]);
        assert!(!fixed.is_animated());
        assert_eq!(fixed.style_values(0.0)[32], 0.0);
        assert_eq!(fixed.style_values(0.1)[32], 0.0);
        let animated =
            LightmapSet::style_pages_for_tests(LightmapSource::Ldr, &pages, &[(32, "am")]);
        assert_eq!(animated.active_styles(), vec![0, 32]);
        assert!(animated.is_animated());
        assert_eq!(animated.style_values(0.0)[32], 0.0);
        assert_eq!(animated.style_values(0.1)[32], 1.0);
        let unused = LightmapSet::style_pages_for_tests(LightmapSource::Ldr, &[], &[(32, "am")]);
        assert!(unused.active_styles().is_empty());
        assert!(!unused.is_animated());
    }

    #[test]
    fn page_styles_preserve_unaffected_luxels() {
        let blocks = [(0, sample(255, 0, 0)), (32, sample(0, 255, 0))].map(|(style, sample)| {
            extract_face_lightmap(
                &face([style, 255, 255, 255]),
                &[sample],
                LightmapBasis::Scalar,
                LightmapBasis::Scalar,
            )
            .unwrap()
            .unwrap()
        });
        let set = bake_lightmap_set(
            std::iter::empty(),
            blocks.into(),
            Some(LightmapSource::Ldr),
            &[],
        )
        .unwrap();
        assert_eq!(set.pages[0].active_styles(), &[0, 32]);
        let previous = set.style_values(0.0);
        let mut next = previous;
        next[32] = 0.0;
        let before = set.render_page(LightmapPageId(0), &previous).unwrap();
        let after = set.render_page(LightmapPageId(0), &next).unwrap();
        let differences = before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .collect::<Vec<_>>();
        assert_eq!(differences.len(), 9);
        assert!(
            differences
                .iter()
                .all(|(_, after)| *after == [0, 0, 0, 255])
        );
    }

    #[test]
    fn scalar_samples_fill_effective_directional_planes_after_style_and_gamma_conversion() {
        let block = extract_face_lightmap(
            &face([0, 32, 255, 255]),
            &[sample(128, 0, 0), sample(0, 128, 0)],
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        assert_eq!((block.dimensions()[0], block.dimensions()[1]), (12, 3));
        let mut values = [0.0; 256];
        values[0] = 1.0;
        values[32] = 1.0;
        let combined = block.render(&values, LightmapSource::Ldr);
        for basis in 0..4 {
            let interior = (block.dimensions()[0] + basis * 3 + 1) * 4;
            assert_eq!(&combined[interior..interior + 4], &[93, 93, 0, 255]);
            let gutter = basis * 3 * 4;
            assert_eq!(&combined[gutter..gutter + 4], &[93, 93, 0, 255]);
        }
        values[32] = 0.0;
        let red = block.render(&values, LightmapSource::Ldr);
        for basis in 0..4 {
            let interior = (block.dimensions()[0] + basis * 3 + 1) * 4;
            assert_eq!(&red[interior..interior + 4], &[93, 0, 0, 255]);
        }
    }

    #[test]
    fn scalar_output_keeps_directional_input_style_stride_and_payload_validation() {
        let samples = [
            sample(128, 0, 0),
            sample(255, 255, 255),
            sample(255, 255, 255),
            sample(255, 255, 255),
            sample(0, 128, 0),
            sample(255, 255, 255),
            sample(255, 255, 255),
            sample(255, 255, 255),
        ];
        let face = face([0, 32, 255, 255]);
        assert_eq!(
            extract_face_lightmap(
                &face,
                &samples[..2],
                LightmapBasis::Directional,
                LightmapBasis::Scalar,
            ),
            Err(LightmapExtractionError::InvalidPayload)
        );
        let block = extract_face_lightmap(
            &face,
            &samples,
            LightmapBasis::Directional,
            LightmapBasis::Scalar,
        )
        .unwrap()
        .unwrap();
        assert_eq!((block.dimensions()[0], block.dimensions()[1]), (3, 3));
        let pixels = block.render(&[1.0; 256], LightmapSource::Ldr);
        assert_eq!(
            &pixels[(block.dimensions()[0] + 1) * 4..][..4],
            &[93, 93, 0, 255]
        );
    }

    #[test]
    fn oversized_directional_payloads_are_preserved_in_bounded_pages() {
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [1022, 0];
        let samples = vec![sample(128, 128, 128); 1023];
        let block = extract_face_lightmap(
            &face,
            &samples,
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let set = bake_lightmap_set(
            std::iter::empty(),
            vec![block],
            Some(LightmapSource::Ldr),
            &[],
        )
        .unwrap();
        assert!(
            set.pages
                .iter()
                .map(|page| page.patches.len())
                .sum::<usize>()
                > 1
        );
        for (index, page) in set.pages.iter().enumerate() {
            assert!(
                page.width <= LIGHTMAP_PAGE_SIZE as u32 && page.height <= LIGHTMAP_PAGE_SIZE as u32
            );
            assert_eq!(
                set.render_page(LightmapPageId(index as u32), &[1.0; 256])
                    .unwrap()
                    .len(),
                page.width as usize * page.height as usize * 4
            );
        }
    }

    #[test]
    fn repeated_placements_share_authored_payload_pages() {
        let block = extract_face_lightmap(
            &face([0, 255, 255, 255]),
            &[sample(100, 150, 200)],
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let set = bake_lightmap_set(
            std::iter::empty(),
            vec![block.clone(), block.clone(), block],
            Some(LightmapSource::Ldr),
            &[],
        )
        .unwrap();
        assert_eq!(set.pages.len(), 1);
        assert_eq!(set.pages[0].patches.len(), 1);
        let image = set.render_page(LightmapPageId(0), &[1.0; 256]).unwrap();
        assert_eq!(
            image.len(),
            set.pages[0].width as usize * set.pages[0].height as usize * 4
        );
    }

    #[test]
    fn page_rendering_rejects_unknown_ids_and_inconsistent_dimensions() {
        let block = extract_face_lightmap(
            &face([0, 255, 255, 255]),
            &[sample(100, 150, 200)],
            LightmapBasis::Scalar,
            LightmapBasis::Scalar,
        )
        .unwrap()
        .unwrap();
        let mut set = bake_lightmap_set(
            std::iter::empty(),
            vec![block],
            Some(LightmapSource::Ldr),
            &[],
        )
        .unwrap();
        assert_eq!(
            set.render_page(LightmapPageId(1), &[1.0; 256]),
            Err(LightmapPageError::InvalidPage)
        );
        set.pages[0].width = 1;
        assert_eq!(
            set.render_page(LightmapPageId(0), &[1.0; 256]),
            Err(LightmapPageError::InvalidDimensions)
        );
        set.pages[0].width = LIGHTMAP_PAGE_SIZE as u32 + 1;
        assert_eq!(
            set.render_page(LightmapPageId(0), &[1.0; 256]),
            Err(LightmapPageError::InvalidDimensions)
        );
    }

    #[test]
    fn page_packing_preserves_every_receiver_without_a_map_capacity_limit() {
        let dimensions = vec![[1024, 1024]; 300];
        let (pages, placements) = pack_lightmap_regions(&dimensions);
        assert_eq!(pages.len(), dimensions.len());
        assert_eq!(placements.len(), dimensions.len());
        for (index, (page, rect)) in placements.into_iter().enumerate() {
            assert_eq!(page, LightmapPageId(index as u32));
            assert_eq!(rect, LightmapRect { x: 0, y: 0 });
        }
    }

    #[test]
    fn tiled_regions_match_whole_face_luxels_including_neighbor_gutters_and_styles() {
        for basis in [LightmapBasis::Scalar, LightmapBasis::Directional] {
            let mut face = face([0, 32, 255, 255]);
            face.lightmap_size = [299, 2];
            let samples = (0..300 * 3 * basis.count() * 2)
                .map(|index| {
                    sample(
                        (index % 251) as u8,
                        ((index * 7) % 239) as u8,
                        ((index * 13) % 227) as u8,
                    )
                })
                .collect::<Vec<_>>();
            let block = extract_face_lightmap(&face, &samples, basis, basis)
                .unwrap()
                .unwrap();
            let regions = [
                LightmapRegion {
                    offset: [0, 0],
                    size: [150, 2],
                },
                LightmapRegion {
                    offset: [150, 0],
                    size: [150, 2],
                },
                LightmapRegion {
                    offset: [0, 2],
                    size: [150, 1],
                },
                LightmapRegion {
                    offset: [150, 2],
                    size: [150, 1],
                },
            ];
            for source in [LightmapSource::Ldr, LightmapSource::Hdr] {
                for weight in [0.0, 0.75, 2.0] {
                    let mut weights = [1.0; 256];
                    weights[32] = weight;
                    let whole = block.render(&weights, source);
                    for region in regions {
                        let [width, height] = region.dimensions(basis);
                        let tile = block.render_region(region, &weights, source);
                        for plane in 0..basis.count() {
                            for y in 0..height {
                                for x in 0..region.size[0] + 2 {
                                    let whole_x = (region.offset[0] + x).clamp(1, block.face_width);
                                    let whole_y =
                                        (region.offset[1] + y).clamp(1, block.face_height);
                                    let expected = (whole_y * block.dimensions()[0]
                                        + plane * (block.face_width + 2)
                                        + whole_x)
                                        * 4;
                                    let actual = (y * width + plane * (region.size[0] + 2) + x) * 4;
                                    assert_eq!(
                                        &tile[actual..actual + 4],
                                        &whole[expected..expected + 4],
                                        "{basis:?} {source:?} {region:?} {plane} {x} {y}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tiled_geometry_preserves_area_visibility_surface_ownership_and_interpolated_attributes() {
        use super::super::mesh::{BuildMeshVisibility, BuildVertex, GeometryPartition};
        use super::super::{MapMeshIndexRange, MapMeshSurfaceOwner, MapSurfaceOwner, MapVertex};
        let mut face = face([0, 255, 255, 255]);
        face.lightmap_size = [299, 3];
        let block = extract_face_lightmap(
            &face,
            &vec![sample(100, 150, 200); 300 * 4],
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let vertex = |position: Vec3, lightmap_uv: [f32; 2], lightmap_block| BuildVertex {
            vertex: MapVertex {
                position,
                base_position: position * 2.0,
                normal: Vec3::new(0.0, 0.0, 1.0),
                tangent_s: Vec3::new(1.0, 0.0, 0.0),
                tangent_t: Vec3::new(0.0, 1.0, 0.0),
                tex_s: position[0] * 7.0,
                tex_t: position[1] * 9.0,
                texture_size: [128, 256],
                lightmap_uv,
                lightmap_page: None,
                lightmap_bump_step: 0.0,
                blend_alpha: position[0],
                opacity: 1.0 - position[1] * 0.5,
            },
            lightmap_block,
        };
        let owner = |index_start| MapMeshSurfaceOwner {
            index_start,
            owner: MapSurfaceOwner::World,
            solid: true,
            allows_decals: true,
            plane_normal: Vec3::new(0.0, 0.0, 1.0),
            plane_distance: 0.0,
            displacement: None,
            texture_projection: None,
        };
        let mut mesh = BuildMesh {
            vertices: vec![
                vertex(Vec3::new(0.0, 0.0, 0.0), [0.5 / 300.0, 0.5 / 4.0], Some(0)),
                vertex(
                    Vec3::new(1.0, 0.0, 0.0),
                    [299.5 / 300.0, 0.5 / 4.0],
                    Some(0),
                ),
                vertex(Vec3::new(0.0, 1.0, 0.0), [0.5 / 300.0, 3.5 / 4.0], Some(0)),
                vertex(Vec3::new(2.0, 0.0, 0.0), [0.0; 2], None),
                vertex(Vec3::new(3.0, 0.0, 0.0), [0.0; 2], None),
                vertex(Vec3::new(2.0, 1.0, 0.0), [0.0; 2], None),
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            material_index: 0,
            entity: false,
            partition: GeometryPartition::Visible,
            visibility: BuildMeshVisibility {
                always_visible: vec![MapMeshIndexRange {
                    face: 7,
                    start: 0,
                    count: 3,
                }],
                clusters: [(
                    2,
                    vec![MapMeshIndexRange {
                        face: 8,
                        start: 3,
                        count: 3,
                    }],
                )]
                .into(),
            },
            surface_owners: vec![owner(0), owner(3)],
        };
        bake_lightmap_set([&mut mesh], vec![block], Some(LightmapSource::Ldr), &[]).unwrap();
        assert!(mesh.indices.len() > 6);
        let first = mesh.visibility.always_visible[0];
        let second = mesh.visibility.clusters[&2][0];
        assert_eq!(first.face, 7);
        assert_eq!(first.start, 0);
        assert_eq!(first.start + first.count, second.start);
        assert_eq!(second.face, 8);
        assert_eq!(second.count, 3);
        assert_eq!(second.start + second.count, mesh.indices.len() as u32);
        assert_eq!(mesh.surface_owners[1].index_start, second.start);
        let mut area = 0.0;
        for triangle in mesh.indices[..first.count as usize].chunks_exact(3) {
            let vertices = triangle
                .iter()
                .map(|index| mesh.vertices[*index as usize].vertex)
                .collect::<Vec<_>>();
            assert!(
                vertices
                    .iter()
                    .all(|vertex| vertex.lightmap_page == vertices[0].lightmap_page)
            );
            assert!(
                vertices
                    .iter()
                    .all(|vertex| vertex.lightmap_bump_step == vertices[0].lightmap_bump_step)
            );
            area += (vertices[1].position - vertices[0].position)
                .cross(vertices[2].position - vertices[0].position)
                .length()
                * 0.5;
            for vertex in vertices {
                assert!((vertex.tex_s - vertex.position[0] * 7.0).abs() < 0.00001);
                assert!((vertex.tex_t - vertex.position[1] * 9.0).abs() < 0.00001);
                assert!((vertex.blend_alpha - vertex.position[0]).abs() < 0.00001);
                assert!((vertex.opacity - (1.0 - vertex.position[1] * 0.5)).abs() < 0.00001);
                assert_eq!(vertex.base_position, vertex.position * 2.0);
            }
        }
        assert!((area - 0.5).abs() < 0.00001);
        assert!(mesh.indices[second.start as usize..].iter().all(|index| {
            mesh.vertices[*index as usize]
                .vertex
                .lightmap_page
                .is_none()
        }));
        let boundary_uv = [254.0 / 300.0, 0.5];
        let mut boundary = BuildMesh {
            vertices: vec![
                vertex(Vec3::new(0.0, 0.0, 0.0), boundary_uv, Some(0)),
                vertex(Vec3::new(1.0, 0.0, 0.0), boundary_uv, Some(0)),
                vertex(Vec3::new(0.0, 1.0, 0.0), boundary_uv, Some(0)),
            ],
            indices: vec![0, 1, 2],
            material_index: 0,
            entity: false,
            partition: GeometryPartition::Visible,
            visibility: BuildMeshVisibility::default(),
            surface_owners: vec![owner(0)],
        };
        let block = extract_face_lightmap(
            &face,
            &vec![sample(100, 150, 200); 300 * 4],
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        bake_lightmap_set([&mut boundary], vec![block], Some(LightmapSource::Ldr), &[]).unwrap();
        assert_eq!(boundary.indices.len(), 3);
        assert_eq!(
            boundary.vertices[0].vertex.position,
            Vec3::new(0.0, 0.0, 0.0)
        );
        assert_eq!(
            boundary.vertices[1].vertex.position,
            Vec3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(
            boundary.vertices[2].vertex.position,
            Vec3::new(0.0, 1.0, 0.0)
        );
    }

    #[test]
    fn hdr_directional_normalization_targets_linear_scalar_without_ldr_gamma() {
        let block = extract_face_lightmap(
            &face([0, 255, 255, 255]),
            &[
                sample(64, 0, 0),
                sample(128, 0, 0),
                sample(0, 0, 0),
                sample(0, 0, 0),
            ],
            LightmapBasis::Directional,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let pixels = block.render(&[1.0; 256], LightmapSource::Hdr);
        let pixel = |basis: usize| {
            <[u8; 4]>::try_from(&pixels[(block.dimensions()[0] + basis * 3 + 1) * 4..][..4])
                .unwrap()
        };
        assert_eq!(pixel(0), [137, 0, 0, 255]);
        assert_eq!(pixel(1), [225, 0, 0, 255]);
        assert_eq!(pixel(2), [0, 0, 0, 255]);
        assert_eq!(pixel(3), [0, 0, 0, 255]);
        let scalar = extract_face_lightmap(
            &face([0, 255, 255, 255]),
            &[sample(128, 73, 97)],
            LightmapBasis::Scalar,
            LightmapBasis::Directional,
        )
        .unwrap()
        .unwrap();
        let pixels = scalar.render(&[1.0; 256], LightmapSource::Hdr);
        let base = &pixels[(scalar.dimensions()[0] + 1) * 4..][..4];
        for basis in 1..4 {
            assert_eq!(
                &pixels[(scalar.dimensions()[0] + basis * 3 + 1) * 4..][..4],
                base
            );
        }
    }
}
