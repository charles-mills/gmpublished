//! Immutable spatial seeds with per-frame style selection and ambient folding.

use super::initialization::initialized_light;
use super::{CompiledPointLight, LightCacheCell, LightCacheKey, PointLightResponse};
use gmpublished_domain::{
    math::Vec3,
    scene::map::{
        AmbientCube, AmbientLightSource, LightmapSource, MASK_OPAQUE, MapAmbientLighting,
        MapBounds, MapLightingCollision, MapVisibility, MapWorldLight, MapWorldLightSet,
        MapWorldLighting,
    },
};
use std::{collections::HashMap, sync::Arc};
use vformats::bsp::WorldLightKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelLightingFailure {
    MissingCollision,
    MissingCompiledLights,
    IncoherentMode,
    InvalidPosition,
    MissingLeaf,
    MissingAmbientSpan(usize),
    InvalidStyle,
    InvalidBudget,
    InvalidTrace,
    MissingRadiositySurfaces,
    MissingSurfaceMaterial(usize),
    MissingSurfaceSample(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelLightingDiagnostic {
    UnsupportedEmitter {
        original_index: usize,
        kind: WorldLightKind,
    },
    InvalidEmitter {
        original_index: usize,
    },
    MissingVisibility,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SpatialLight {
    light: MapWorldLight,
    response: PointLightResponse,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingSeed {
    pub sample_point: Vec3,
    pub leaf: usize,
    pub ambient_cube: AmbientCube,
    candidates: Vec<SpatialLight>,
    pub diagnostics: Vec<ModelLightingDiagnostic>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectedModelLight {
    pub light: MapWorldLight,
    pub style_weight: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingEvaluation {
    pub ambient_cube: AmbientCube,
    pub selected: Vec<SelectedModelLight>,
    pub folded_ids: Vec<usize>,
}

pub struct ModelLightingFactory<'a> {
    ambient: &'a MapAmbientLighting,
    lights: &'a MapWorldLightSet,
    collision: &'a MapLightingCollision,
    visibility: Option<&'a MapVisibility>,
    source: LightmapSource,
    cache: HashMap<LightCacheKey, Arc<ModelLightingSeed>>,
}

impl<'a> ModelLightingFactory<'a> {
    pub fn new(
        ambient: &'a MapAmbientLighting,
        world: &'a MapWorldLighting,
        collision: &'a MapLightingCollision,
        visibility: Option<&'a MapVisibility>,
    ) -> Result<Self, ModelLightingFailure> {
        let lights = world
            .selected()
            .filter(|set| set.error.is_none())
            .ok_or(ModelLightingFailure::MissingCompiledLights)?;
        if matches!(
            (ambient.source(), world.source),
            (AmbientLightSource::Ldr, Some(LightmapSource::Hdr))
                | (AmbientLightSource::Hdr, Some(LightmapSource::Ldr))
        ) {
            return Err(ModelLightingFailure::IncoherentMode);
        }
        Ok(Self {
            ambient,
            lights,
            collision,
            visibility,
            source: world
                .source
                .ok_or(ModelLightingFailure::MissingCompiledLights)?,
            cache: HashMap::new(),
        })
    }

    /// The caller supplies a verified engine sampling point. Static spherical
    /// radiosity and model/attachment/anchor selection are separate policies.
    pub fn seed_dynamic(
        &mut self,
        point: Vec3,
    ) -> Result<Arc<ModelLightingSeed>, ModelLightingFailure> {
        let leaf = self
            .collision
            .leaf_at(point)
            .ok_or(ModelLightingFailure::MissingLeaf)?;
        let cell = LightCacheCell::from_position(point, leaf)
            .ok_or(ModelLightingFailure::InvalidPosition)?;
        if let Some(seed) = self.cache.get(&cell.key) {
            return Ok(Arc::clone(seed));
        }
        let sample_point = validate_center(self.collision, point, cell.center, leaf)?;
        let ambient_cube = self
            .ambient
            .interpolate_leaf_at(leaf, sample_point)
            .ok_or(ModelLightingFailure::MissingAmbientSpan(leaf))?;
        let seed = self.build_seed(
            point,
            sample_point,
            leaf,
            ambient_cube,
            cell.bounds_max,
            false,
        )?;
        self.cache.insert(cell.key, Arc::clone(&seed));
        Ok(seed)
    }

    /// Static cache creation retains the resolved continuous point. Its
    /// spherical surface radiosity is supplied independently of leaf samples.
    pub fn seed_static(
        &self,
        point: Vec3,
        bounds: MapBounds,
        ambient_cube: AmbientCube,
    ) -> Result<Arc<ModelLightingSeed>, ModelLightingFailure> {
        if !point.is_finite()
            || !bounds.min.is_finite()
            || !bounds.max.is_finite()
            || (0..3).any(|axis| bounds.min[axis] > bounds.max[axis])
            || !ambient_cube.colors.into_iter().all(Vec3::is_finite)
        {
            return Err(ModelLightingFailure::InvalidPosition);
        }
        let leaf = self
            .collision
            .leaf_at(point)
            .ok_or(ModelLightingFailure::MissingLeaf)?;
        let cell = LightCacheCell::from_position(point, leaf)
            .ok_or(ModelLightingFailure::InvalidPosition)?;
        // Selection derives its culling cell from the point even for a
        // continuous static cache seed; model hull bounds are separate metadata.
        self.build_seed(point, point, leaf, ambient_cube, cell.bounds_max, true)
    }

    fn build_seed(
        &self,
        point: Vec3,
        sample_point: Vec3,
        leaf: usize,
        ambient_cube: AmbientCube,
        bounds_max: Vec3,
        is_static: bool,
    ) -> Result<Arc<ModelLightingSeed>, ModelLightingFailure> {
        let visible = self.visibility.and_then(|visibility| {
            visibility
                .cluster_at(point)
                .and_then(|cluster| visibility.visible_clusters(cluster))
        });
        let mut diagnostics = Vec::new();
        if visible.is_none() {
            diagnostics.push(ModelLightingDiagnostic::MissingVisibility);
        }
        let mut candidates = Vec::new();
        let selection_cell = LightCacheCell::from_position(sample_point, leaf)
            .ok_or(ModelLightingFailure::InvalidPosition)?;
        let mut sky_seen = false;
        for light in &self.lights.lights {
            if (!is_static && light.flags & 1 != 0)
                || (is_static && light.style != 0)
                || light.kind == WorldLightKind::SkyAmbient
                || (light.kind != WorldLightKind::Skylight
                    && light.intensity.into_iter().all(|channel| channel <= 0.0))
                || (light.kind == WorldLightKind::Skylight && sky_seen)
            {
                continue;
            }
            if light.kind != WorldLightKind::Skylight
                && (light.cluster < 0
                    || visible.as_ref().is_some_and(|row| {
                        usize::try_from(light.cluster)
                            .ok()
                            .and_then(|cluster| row.get(cluster))
                            != Some(&true)
                    }))
            {
                continue;
            }
            if !matches!(
                light.kind,
                WorldLightKind::Surface
                    | WorldLightKind::Point
                    | WorldLightKind::Spotlight
                    | WorldLightKind::Skylight
                    | WorldLightKind::Quake
            ) {
                diagnostics.push(ModelLightingDiagnostic::UnsupportedEmitter {
                    original_index: light.original_index,
                    kind: light.kind,
                });
                continue;
            }
            let Some(light) = initialized_light(*light, self.source) else {
                diagnostics.push(ModelLightingDiagnostic::InvalidEmitter {
                    original_index: light.original_index,
                });
                continue;
            };
            if light.kind == WorldLightKind::Point
                && !point_intersects_cell(
                    light,
                    selection_cell.bounds_min,
                    selection_cell.bounds_max,
                )
            {
                continue;
            }
            let Some(response) = compiled_response(light, sample_point, 1.0) else {
                diagnostics.push(ModelLightingDiagnostic::InvalidEmitter {
                    original_index: light.original_index,
                });
                continue;
            };
            if response.attenuation <= 0.0 {
                continue;
            }
            if light.kind == WorldLightKind::Skylight {
                if self
                    .collision
                    .leaf_at(sample_point)
                    .and_then(|leaf| self.collision.leaf_flags(leaf))
                    .is_none_or(|flags| flags & 5 == 0)
                {
                    continue;
                }
                let end = sample_point + Vec3::from(light.normal) * -57_016.32;
                let trace = self
                    .collision
                    .trace_opaque(sample_point, end)
                    .ok_or(ModelLightingFailure::InvalidTrace)?;
                if trace.surface_flags & vformats::bsp::texture_flags::SKY == 0 {
                    continue;
                }
                sky_seen = true;
            } else {
                if light.kind == WorldLightKind::Spotlight
                    && !spot_intersects_cell(light, sample_point, bounds_max)
                {
                    continue;
                }
                if light.kind == WorldLightKind::Surface {
                    let mut hemisphere = light;
                    hemisphere.stopdot2 = 0.0;
                    if !spot_intersects_cell(hemisphere, sample_point, bounds_max) {
                        continue;
                    }
                }
                let origin = Vec3::from(light.origin);
                let trace = self
                    .collision
                    .trace_opaque(origin, sample_point)
                    .ok_or(ModelLightingFailure::InvalidTrace)?;
                let distance = sample_point.distance(origin);
                // Engine world-light visibility permits an obstruction within
                // eight units of the receiving point, after tracing from source.
                if !source_visible(distance, trace.fraction, trace.start_solid) {
                    continue;
                }
            }
            candidates.push(SpatialLight { light, response });
        }
        let seed = Arc::new(ModelLightingSeed {
            sample_point,
            leaf,
            ambient_cube,
            candidates,
            diagnostics,
        });
        Ok(seed)
    }
}

fn source_visible(distance: f32, fraction: f32, start_solid: bool) -> bool {
    let reached = if start_solid {
        0.0
    } else {
        distance * fraction
    };
    distance <= reached + 8.0
}

fn validate_center(
    collision: &MapLightingCollision,
    point: Vec3,
    center: Vec3,
    leaf: usize,
) -> Result<Vec3, ModelLightingFailure> {
    let center_leaf = collision
        .leaf_at(center)
        .ok_or(ModelLightingFailure::MissingLeaf)?;
    if center_leaf == leaf
        || collision
            .leaf_contents(center_leaf)
            .is_some_and(|contents| contents & MASK_OPAQUE == 0)
    {
        let candidate = if center_leaf == leaf {
            center
        } else {
            collision
                .snap_to_point_leaf(point, center)
                .ok_or(ModelLightingFailure::MissingLeaf)?
        };
        let trace = collision
            .trace_opaque(point, candidate)
            .ok_or(ModelLightingFailure::InvalidTrace)?;
        if trace.start_solid {
            return Ok(point);
        }
        if trace.fraction >= 1.0 {
            return Ok(candidate);
        }
    }
    let candidate = collision
        .snap_to_point_leaf(point, Vec3::new(center[0], center[1], point[2]))
        .ok_or(ModelLightingFailure::MissingLeaf)?;
    let trace = collision
        .trace_opaque(point, candidate)
        .ok_or(ModelLightingFailure::InvalidTrace)?;
    Ok(if trace.fraction >= 1.0 {
        candidate
    } else {
        point
    })
}

impl ModelLightingSeed {
    pub fn evaluate(
        &self,
        styles: &[f32; 128],
        maximum_lights: usize,
        minimum: f32,
    ) -> Result<ModelLightingEvaluation, ModelLightingFailure> {
        if maximum_lights > 4 || !minimum.is_finite() || minimum < 0.0 {
            return Err(ModelLightingFailure::InvalidBudget);
        }
        let (mut ambient_cube, mut selected, mut folded_ids) =
            self.evaluate_class(styles, maximum_lights, minimum, false, self.ambient_cube)?;
        let (styled_ambient, styled_selected, styled_folded) = self.evaluate_class(
            styles,
            maximum_lights,
            minimum,
            true,
            AmbientCube {
                colors: [Vec3::ZERO; 6],
            },
        )?;
        // Source caches ordinary and switchable-light lists separately. At
        // composition, retained switchable lights replace the weakest ordinary
        // slot using a fixed comparison limit, even at equal/lower brightness.
        for slot in styled_selected {
            select_slot(
                slot,
                maximum_lights,
                true,
                &mut selected,
                &mut ambient_cube,
                &mut folded_ids,
            );
        }
        for (color, styled) in ambient_cube.colors.iter_mut().zip(styled_ambient.colors) {
            *color += styled;
        }
        folded_ids.extend(styled_folded);
        Ok(ModelLightingEvaluation {
            ambient_cube,
            selected: selected
                .into_iter()
                .map(|slot| SelectedModelLight {
                    light: slot.candidate.light,
                    style_weight: slot.weight,
                })
                .collect(),
            folded_ids,
        })
    }

    fn evaluate_class(
        &self,
        styles: &[f32; 128],
        maximum_lights: usize,
        minimum: f32,
        styled: bool,
        mut ambient: AmbientCube,
    ) -> Result<(AmbientCube, Vec<LightSlot>, Vec<usize>), ModelLightingFailure> {
        let mut selected = Vec::new();
        let mut folded_ids = Vec::new();
        for &candidate in self
            .candidates
            .iter()
            .filter(|candidate| (candidate.light.style != 0) == styled)
        {
            let weight = usize::try_from(candidate.light.style)
                .ok()
                .and_then(|style| styles.get(style))
                .copied()
                .unwrap_or(1.0);
            if !weight.is_finite() || weight < 0.0 {
                return Err(ModelLightingFailure::InvalidStyle);
            }
            let attenuation = candidate.response.attenuation * weight;
            if attenuation <= 0.0 {
                continue;
            }
            let intensity = candidate.light.intensity;
            // Spatial response first drops sub-threshold maximum-channel
            // energy. Local-slot selection has a separate luminance threshold;
            // lights below that second threshold still contribute ambient.
            if !matches!(
                candidate.light.kind,
                WorldLightKind::Surface | WorldLightKind::Skylight
            ) && intensity.into_iter().fold(f32::NEG_INFINITY, f32::max) * attenuation < minimum
            {
                continue;
            }
            let score = ((intensity[0] * 0.299 + intensity[1] * 0.587) + intensity[2] * 0.114)
                * attenuation;
            let slot = LightSlot {
                candidate,
                weight,
                score,
            };
            if candidate.light.kind != WorldLightKind::Surface && score < minimum {
                fold_slot(slot, &mut ambient, &mut folded_ids);
            } else {
                select_slot(
                    slot,
                    maximum_lights,
                    false,
                    &mut selected,
                    &mut ambient,
                    &mut folded_ids,
                );
            }
        }
        Ok((ambient, selected, folded_ids))
    }
}

#[derive(Clone, Copy)]
struct LightSlot {
    candidate: SpatialLight,
    weight: f32,
    score: f32,
}

fn select_slot(
    slot: LightSlot,
    maximum_lights: usize,
    force: bool,
    selected: &mut Vec<LightSlot>,
    ambient: &mut AmbientCube,
    folded_ids: &mut Vec<usize>,
) {
    if selected.len() < maximum_lights {
        selected.push(slot);
        return;
    }
    let mut weakest = None;
    let mut weakest_score = if force { 100_000.0 } else { slot.score };
    for (index, previous) in selected.iter().enumerate() {
        if previous.score < weakest_score {
            weakest = Some(index);
            weakest_score = previous.score;
        }
    }
    let folded = weakest.map_or(slot, |index| std::mem::replace(&mut selected[index], slot));
    fold_slot(folded, ambient, folded_ids);
}

fn fold_slot(slot: LightSlot, cube: &mut AmbientCube, ids: &mut Vec<usize>) {
    fold_light(cube, slot.candidate.response, slot.weight);
    ids.push(slot.candidate.light.original_index);
}

fn fold_light(cube: &mut AmbientCube, response: PointLightResponse, weight: f32) {
    for (side, color) in cube.colors.iter_mut().enumerate() {
        let component =
            response.direction_to_light[side / 2] * if side % 2 == 0 { 1.0 } else { -1.0 };
        if component > 0.0 {
            *color += response.radiance * (component * weight);
        }
    }
}

#[cfg(test)]
impl ModelLightingEvaluation {
    pub fn lambert_at(&self, point: Vec3, normal: Vec3) -> Vec3 {
        let mut color = self.ambient_cube.evaluate(normal);
        for selected in &self.selected {
            if let Some(response) = selected_response(selected.light, point, selected.style_weight)
            {
                color += response.lambert(normal);
            }
        }
        color
    }
}

#[cfg(test)]
impl ModelLightingSeed {
    pub(crate) fn ambient_fixture(ambient_cube: AmbientCube) -> Self {
        Self {
            sample_point: Vec3::ZERO,
            leaf: 0,
            ambient_cube,
            candidates: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

fn compiled_response(light: MapWorldLight, point: Vec3, weight: f32) -> Option<PointLightResponse> {
    if !point.is_finite() || !weight.is_finite() || weight < 0.0 {
        return None;
    }
    if light.kind == WorldLightKind::Skylight {
        let direction = -Vec3::from(light.normal);
        let radiance = Vec3::from(light.intensity) * weight;
        return (direction.is_finite() && direction.length_squared() > 0.0 && radiance.is_finite())
            .then_some(PointLightResponse {
                direction_to_light: direction,
                attenuation: 1.0,
                radiance,
            });
    }
    if matches!(light.kind, WorldLightKind::Surface | WorldLightKind::Quake) {
        let delta = Vec3::from(light.origin) - point;
        let squared = delta.length_squared();
        if !squared.is_finite() || squared <= 0.0 {
            return None;
        }
        let distance = squared.sqrt();
        let direction = delta / distance;
        let attenuation = if light.kind == WorldLightKind::Surface {
            if light.radius > 0.0 && squared > light.radius * light.radius {
                0.0
            } else if squared < 1.0 {
                1.0
            } else {
                (1.0_f64 / f64::from(squared)) as f32
            }
        } else {
            (light.linear_attenuation - distance).max(0.0)
        };
        let angular = if light.kind == WorldLightKind::Surface {
            let cosine = -direction.dot(Vec3::from(light.normal));
            if f64::from(cosine) > 0.01 {
                cosine
            } else {
                0.0
            }
        } else {
            1.0
        };
        let radiance = Vec3::from(light.intensity) * (attenuation * weight * angular);
        return (attenuation.is_finite() && radiance.is_finite()).then_some(PointLightResponse {
            direction_to_light: direction,
            attenuation,
            radiance,
        });
    }
    let mut point_light = light;
    point_light.kind = WorldLightKind::Point;
    // Point selection already checks the entire cache cell against its range.
    // The subsequent response and GPU descriptor ignore pointwise radius.
    if light.kind == WorldLightKind::Point {
        point_light.radius = 0.0;
    }
    let mut response = CompiledPointLight::from_compiled(point_light)?.response(point, weight)?;
    if light.kind == WorldLightKind::Spotlight {
        let angular = spot_angular(light, response.direction_to_light)?;
        response.radiance *= angular;
    }
    Some(response)
}

#[cfg(test)]
fn selected_response(
    mut light: MapWorldLight,
    point: Vec3,
    weight: f32,
) -> Option<PointLightResponse> {
    if light.kind == WorldLightKind::Quake {
        return None;
    }
    if light.kind == WorldLightKind::Point
        && light.radius != 0.0
        && Vec3::from(light.origin).distance_squared(point) > light.radius * light.radius
    {
        return Some(PointLightResponse {
            direction_to_light: Vec3::ZERO,
            attenuation: 0.0,
            radiance: Vec3::ZERO,
        });
    }
    if light.kind == WorldLightKind::Surface {
        // The hardware descriptor is a hemisphere spotlight with inverse-square
        // attenuation. Its selected-light response differs from emitter-cosine
        // weighting used when Source folds this light into the ambient cube.
        light.kind = WorldLightKind::Spotlight;
        light.constant_attenuation = 0.0;
        light.linear_attenuation = 0.0;
        light.quadratic_attenuation = 1.0;
        light.stopdot = 0.0;
        light.stopdot2 = 0.0;
        light.exponent = 1.0;
    }
    compiled_response(light, point, weight)
}

fn spot_angular(light: MapWorldLight, direction: Vec3) -> Option<f32> {
    if !Vec3::from(light.normal).is_finite()
        || ![light.stopdot, light.stopdot2, light.exponent]
            .into_iter()
            .all(f32::is_finite)
        || !(-1.0..=1.0).contains(&light.stopdot2)
        || light.stopdot < light.stopdot2
        || light.exponent < 0.0
    {
        return None;
    }
    let cosine = -Vec3::from(light.normal).dot(direction);
    if cosine <= light.stopdot2 {
        return Some(0.0);
    }
    if cosine >= light.stopdot {
        return Some(1.0);
    }
    let factor = (cosine - light.stopdot2) / (light.stopdot - light.stopdot2);
    Some(if light.exponent == 0.0 || light.exponent == 1.0 {
        factor
    } else {
        f64::from(factor).powf(f64::from(light.exponent)) as f32
    })
}

fn spot_intersects_cell(light: MapWorldLight, center: Vec3, maximum: Vec3) -> bool {
    let radius = center.distance(maximum);
    let cosine = light.stopdot2;
    let sine = cosine.acos().sin();
    if !sine.is_finite() || sine <= 0.0 {
        return false;
    }
    let normal = Vec3::from(light.normal);
    let origin = Vec3::from(light.origin);
    let offset = center - (origin - normal * (radius / sine));
    if offset.dot(normal) < offset.length() * cosine {
        return false;
    }
    let delta = center - origin;
    let distance = delta.length();
    -delta.dot(normal) < distance * sine || radius >= distance
}

fn point_intersects_cell(light: MapWorldLight, minimum: Vec3, maximum: Vec3) -> bool {
    let origin = Vec3::from(light.origin);
    let closest = Vec3::from(std::array::from_fn(|axis| {
        origin[axis].clamp(minimum[axis], maximum[axis])
    }));
    origin.distance_squared(closest) <= light.radius * light.radius
}

#[cfg(test)]
#[path = "factory/tests.rs"]
mod tests;
