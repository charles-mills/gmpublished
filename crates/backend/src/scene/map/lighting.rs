use super::{
    BuildMesh, ColorRgbExp, Face, LeafAmbientIndex, LeafAmbientSample, MapBsp, MapEntity, MapLeaf,
    MapLeafLocator, TexInfo, distance_squared, mul, normalize, parse_entity_float,
    parse_entity_vec3, texture_coord, vector_is_finite_nonzero,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientCube {
    pub colors: [[f32; 3]; 6],
}

impl AmbientCube {
    pub const WHITE: Self = Self {
        colors: [[1.0, 1.0, 1.0]; 6],
    };

    pub fn evaluate(self, normal: [f32; 3]) -> [f32; 3] {
        let normal = normalize(normal);
        let mut color = [0.0_f32; 3];
        for (axis, component) in normal.into_iter().enumerate() {
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
        color
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSunLighting {
    pub direction_to_sun: [f32; 3],
    pub color_linear: [f32; 3],
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MapEnvironmentLighting {
    pub sun: Option<MapSunLighting>,
    pub skylight_linear: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AmbientLightSource {
    Neutral,
    Ldr,
    Hdr,
}

#[derive(Debug, Clone, PartialEq)]
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

    pub fn cube_at(&self, position: [f32; 3]) -> AmbientCube {
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
        position: [f32; 3],
    ) -> Option<AmbientCube> {
        let range = self.leaf_sample_ranges.get(leaf_index).copied()?;
        self.nearest_sample_in_range(range, position)
    }

    pub(super) fn nearest_cluster_sample(
        &self,
        cluster: i16,
        position: [f32; 3],
    ) -> Option<AmbientCube> {
        self.samples
            .iter()
            .filter(|sample| sample.cluster == cluster)
            .min_by(|left, right| {
                distance_squared(left.position, position)
                    .total_cmp(&distance_squared(right.position, position))
            })
            .map(|sample| sample.cube)
    }

    pub(super) fn nearest_sample_in_range(
        &self,
        range: AmbientSampleRange,
        position: [f32; 3],
    ) -> Option<AmbientCube> {
        self.samples
            .get(range.start..range.end())
            .and_then(|samples| {
                samples
                    .iter()
                    .min_by(|left, right| {
                        distance_squared(left.position, position)
                            .total_cmp(&distance_squared(right.position, position))
                    })
                    .map(|sample| sample.cube)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MapAmbientSample {
    pub(super) position: [f32; 3],
    pub(super) cube: AmbientCube,
    pub(super) cluster: i16,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(super) struct AmbientSampleRange {
    pub(super) start: usize,
    pub(super) count: usize,
}

impl AmbientSampleRange {
    pub(super) const fn end(self) -> usize {
        self.start + self.count
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LightmapAtlas {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source: LightmapSource,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LightmapSource {
    Ldr,
    Hdr,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct PendingLightmapBlock {
    pub(super) rgba: Vec<u8>,
    pub(super) width: usize,
    pub(super) height: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

/// The pair `MapBsp` holds for one ambient-lighting source (LDR or HDR):
/// samples plus per-leaf index spans into them.
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

pub(super) fn ambient_sample_position(position: [u8; 3], leaf: &MapLeaf) -> [f32; 3] {
    std::array::from_fn(|axis| {
        let min = f32::from(leaf.mins[axis]);
        let max = f32::from(leaf.maxs[axis]);
        min + (max - min) * (f32::from(position[axis]) / 255.0)
    })
}

pub(super) fn ambient_cube(cube: [ColorRgbExp; 6]) -> AmbientCube {
    AmbientCube {
        colors: cube.map(decode_ambient_sample_linear),
    }
}

// Ambient cubes convert via the engine's ColorRGBExp32ToVector — which is
// 255 * TexLightToLinear, i.e. byte * 2^exp WITHOUT the /255 that lightmap
// samples get. Using the lightmap formula here renders every prop roughly
// 255x too dark.
pub(super) fn decode_ambient_sample_linear(sample: ColorRgbExp) -> [f32; 3] {
    let scale = 2.0_f32.powi(i32::from(sample.exponent));
    [
        f32::from(sample.r) * scale,
        f32::from(sample.g) * scale,
        f32::from(sample.b) * scale,
    ]
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

pub(super) fn parse_light_environment_direction(entity: &MapEntity) -> Option<[f32; 3]> {
    let angles = entity.prop("angles").and_then(parse_entity_vec3);
    let yaw = angles.map_or(0.0, |angles| angles[1]);
    let pitch = entity
        .prop("pitch")
        .and_then(parse_entity_float)
        .or_else(|| angles.map(|angles| angles[0]))?;
    let pitch = pitch.to_radians();
    let yaw = yaw.to_radians();
    let travel_direction = normalize([
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        pitch.sin(),
    ]);
    vector_is_finite_nonzero(travel_direction).then_some(mul(travel_direction, -1.0))
}

pub(super) fn parse_entity_rgb_intensity_linear(value: &str) -> Option<[f32; 3]> {
    let mut components = value.split_ascii_whitespace().map(parse_entity_float);
    let red = components.next()??;
    let green = components.next()??;
    let blue = components.next()??;
    // VRAD's LightForString: the intensity scaler is optional (3-component form
    // means "use the RGB as-is") and is NOT capped at 255 — real maps commonly
    // use sun intensities of 300-500. The evaluation-time clamp bounds the result.
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
    // VRAD converts the gamma-space RGB keyvalue to linear with pow 2.2 before
    // scaling — matching it keeps this consistent with the linear color path.
    let scale = intensity / 255.0;
    Some([
        (red / 255.0).powf(2.2) * scale,
        (green / 255.0).powf(2.2) * scale,
        (blue / 255.0).powf(2.2) * scale,
    ])
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

pub(super) fn extract_face_lightmap(
    face: &Face,
    samples: &[ColorRgbExp],
) -> Option<PendingLightmapBlock> {
    if face.light_offset < 0 {
        return None;
    }
    // Face includes styles[4]; Source light_offset points at style-0 samples.
    let width = lightmap_axis_luxels(face.lightmap_size[0])?;
    let height = lightmap_axis_luxels(face.lightmap_size[1])?;
    let count = width.checked_mul(height)?;
    let start = usize::try_from(face.light_offset).ok()?.checked_div(4)?;
    let end = start.checked_add(count)?;
    let face_samples = samples.get(start..end)?;

    let mut rgba = Vec::with_capacity(count * 4);
    for sample in face_samples {
        rgba.extend_from_slice(&decode_light_sample(*sample));
    }

    Some(PendingLightmapBlock {
        rgba,
        width,
        height,
    })
}

pub(super) fn lightmap_axis_luxels(size: i32) -> Option<usize> {
    usize::try_from(size).ok()?.checked_add(1)
}

pub(super) fn decode_light_sample(sample: ColorRgbExp) -> [u8; 4] {
    let linear = decode_light_sample_linear(sample);
    [
        linear_to_srgb_byte(linear[0]),
        linear_to_srgb_byte(linear[1]),
        linear_to_srgb_byte(linear[2]),
        255,
    ]
}

pub(super) fn decode_light_sample_linear(sample: ColorRgbExp) -> [f32; 3] {
    let scale = 2.0_f32.powi(i32::from(sample.exponent)) / 255.0;
    [
        f32::from(sample.r) * scale,
        f32::from(sample.g) * scale,
        f32::from(sample.b) * scale,
    ]
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

pub(super) fn brush_lightmap_uv(position: [f32; 3], texinfo: &TexInfo, face: &Face) -> [f32; 2] {
    brush_lightmap_uv_from_transforms(
        position,
        texinfo.lightmap_vecs[0],
        texinfo.lightmap_vecs[1],
        face.lightmap_mins,
        face.lightmap_size,
    )
}

pub(super) fn brush_lightmap_uv_from_transforms(
    position: [f32; 3],
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
    [
        displacement_lightmap_axis_uv(column as f32 / steps, face.lightmap_size[0]),
        displacement_lightmap_axis_uv(row as f32 / steps, face.lightmap_size[1]),
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

    let mut rgba = vec![linear_to_srgb_byte(0.5); width * height * 4];
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }

    for (block, placement) in blocks.iter().zip(&placements) {
        let Some(rect) = placement else {
            continue;
        };
        for row in 0..block.height {
            let atlas_start = ((rect.y + row) * width + rect.x) * 4;
            let block_start = row * block.width * 4;
            let byte_count = block.width * 4;
            rgba[atlas_start..atlas_start + byte_count]
                .copy_from_slice(&block.rgba[block_start..block_start + byte_count]);
        }
    }

    rewrite_lightmap_uvs(&mut meshes, blocks, &placements, width, height);

    Some(LightmapAtlas {
        rgba,
        width: u32::try_from(width).unwrap_or(u32::MAX),
        height: u32::try_from(height).unwrap_or(u32::MAX),
        source,
    })
}

pub(super) fn clear_lightmap_uvs(meshes: &mut [&mut BuildMesh]) {
    for vertex in meshes.iter_mut().flat_map(|mesh| &mut mesh.vertices) {
        vertex.vertex.lightmap_uv = [0.0; 2];
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
            continue;
        };
        let Some(block) = blocks.get(block_index) else {
            vertex.vertex.lightmap_uv = [0.0; 2];
            continue;
        };
        let Some(Some(rect)) = placements.get(block_index) else {
            vertex.vertex.lightmap_uv = [0.0; 2];
            continue;
        };
        vertex.vertex.lightmap_uv = [
            (rect.x as f32 + vertex.vertex.lightmap_uv[0] * block.width as f32)
                / atlas_width.max(1) as f32,
            (rect.y as f32 + vertex.vertex.lightmap_uv[1] * block.height as f32)
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
