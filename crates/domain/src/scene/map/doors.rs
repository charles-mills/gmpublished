//! Door geometry and motion data.

use super::{
    BspError, BspModel, BuildMesh, BuildMeshes, FaceAppendContext, FaceAttributions, MapBounds,
    MapBsp, MapEntity, MapFaceVisibility, MapMesh, MapPropSolid, MapVisibilityBucket, ModelIndex,
    QAngle, StaticPropPlacement, append_face, bounds_from_points_iter, model_face_range,
    normalize_entity_prop_model_path, parse_entity_float, parse_entity_i32, parse_entity_vec3,
    point_visibility_bucket,
};
use crate::math::Vec3;

const PROP_DOOR_DEFAULT_MOVE_SOUND: &str = "DoorSound.DefaultMove";
const PROP_DOOR_DEFAULT_ARRIVE_SOUND: &str = "DoorSound.DefaultArrive";

const SF_DOOR_START_OPEN_OBSOLETE: u32 = 1;
const SF_DOOR_ROTATE_BACKWARDS: u32 = 2;
/// Source's "Toggle" flag: the door waits to be triggered again rather than
/// closing itself.
const SF_DOOR_NO_AUTO_RETURN: u32 = 32;
const SF_DOOR_ROTATE_ROLL: u32 = 64;
const SF_DOOR_ROTATE_PITCH: u32 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapDoorClass {
    FuncDoor,
    FuncMoveLinear,
    FuncDoorRotating,
    PropDoorRotating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapDoorOpenDirection {
    Both,
    Forward,
    Backward,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapDoor {
    pub class: MapDoorClass,
    pub origin: Vec3,
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: Vec3,
    pub local_bounds_min: Vec3,
    pub local_bounds_max: Vec3,
    pub visibility: MapVisibilityBucket,
    /// Seconds an opened door waits before closing itself, or `None` for a
    /// door that stays open until triggered again.
    pub auto_close_after: Option<f32>,
    pub initial_progress: f32,
    pub motion: MapDoorMotion,
    pub sounds: MapDoorSounds,
    pub geometry: MapDoorGeometry,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapDoorSounds {
    pub move_sound: Option<String>,
    pub stop_sound: Option<String>,
    pub open_sound: Option<String>,
    pub close_sound: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MapDoorGeometry {
    Brush {
        model_index: ModelIndex,
        meshes: Vec<MapMesh>,
    },
    Prop {
        placement: StaticPropPlacement,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapDoorMotion {
    Linear {
        direction: Vec3,
        distance: f32,
        speed: f32,
    },
    Rotating {
        angle_delta: Vec3,
        degrees: f32,
        speed: f32,
        open_direction: MapDoorOpenDirection,
    },
}

#[derive(Debug)]
pub(super) struct PendingMapDoor {
    pub(super) class: MapDoorClass,
    pub(super) origin: Vec3,
    pub(super) angles: Vec3,
    pub(super) visibility: MapVisibilityBucket,
    pub(super) auto_close_after: Option<f32>,
    pub(super) initial_progress: f32,
    pub(super) motion: MapDoorMotion,
    pub(super) sounds: MapDoorSounds,
    pub(super) geometry: PendingDoorGeometry,
}

#[derive(Debug)]
pub(super) enum PendingDoorGeometry {
    Brush { model_index: ModelIndex },
    Prop { placement: StaticPropPlacement },
}

impl PendingMapDoor {
    pub(super) fn brush_model_index(&self) -> Option<ModelIndex> {
        match self.geometry {
            PendingDoorGeometry::Brush { model_index } => Some(model_index),
            PendingDoorGeometry::Prop { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct PendingDoorBuild {
    pub(super) door: PendingMapDoor,
    pub(super) meshes: Vec<BuildMesh>,
}

impl PendingDoorBuild {
    pub(super) fn into_map_door(self) -> Option<MapDoor> {
        match self.door.geometry {
            PendingDoorGeometry::Brush { model_index } => {
                let meshes = self
                    .meshes
                    .into_iter()
                    .map(|mesh| build_mesh_to_map_mesh_local(mesh, self.door.origin))
                    .collect::<Vec<_>>();
                let MapBounds {
                    min: local_bounds_min,
                    max: local_bounds_max,
                } = bounds_from_map_meshes(&meshes)?;
                Some(MapDoor {
                    class: self.door.class,
                    origin: self.door.origin,
                    angles: self.door.angles,
                    local_bounds_min,
                    local_bounds_max,
                    visibility: self.door.visibility,
                    auto_close_after: self.door.auto_close_after,
                    initial_progress: self.door.initial_progress,
                    motion: self.door.motion,
                    sounds: self.door.sounds,
                    geometry: MapDoorGeometry::Brush {
                        model_index,
                        meshes,
                    },
                })
            }
            PendingDoorGeometry::Prop { placement } => Some(MapDoor {
                class: self.door.class,
                origin: self.door.origin,
                angles: self.door.angles,
                local_bounds_min: Vec3::splat(0.0),
                local_bounds_max: Vec3::splat(0.0),
                visibility: self.door.visibility,
                auto_close_after: self.door.auto_close_after,
                initial_progress: self.door.initial_progress,
                motion: self.door.motion,
                sounds: self.door.sounds,
                geometry: MapDoorGeometry::Prop { placement },
            }),
        }
    }
}

fn build_mesh_to_map_mesh_local(mesh: BuildMesh, origin: Vec3) -> MapMesh {
    MapMesh {
        vertices: mesh
            .vertices
            .into_iter()
            .map(|mut vertex| {
                vertex.vertex.position -= origin;
                vertex.vertex
            })
            .collect(),
        indices: mesh.indices,
        material_index: mesh.material_index,
        visibility: mesh.visibility.into_map_visibility(),
    }
}

fn bounds_from_map_meshes(meshes: &[MapMesh]) -> Option<MapBounds> {
    bounds_from_points_iter(
        meshes
            .iter()
            .flat_map(|mesh| mesh.vertices.iter().map(|vertex| vertex.position)),
    )
}

pub(super) fn pending_map_doors(bsp: &MapBsp) -> Vec<PendingMapDoor> {
    let cluster_count = bsp.cluster_count();
    bsp.entities
        .iter()
        .filter_map(|entity| pending_map_door(entity, bsp, cluster_count))
        .collect()
}

fn pending_map_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    match entity.prop("classname")? {
        "func_door" => {
            pending_linear_brush_door(entity, bsp, cluster_count, MapDoorClass::FuncDoor)
        }
        "func_movelinear" => {
            pending_linear_brush_door(entity, bsp, cluster_count, MapDoorClass::FuncMoveLinear)
        }
        "func_door_rotating" => pending_rotating_brush_door(entity, bsp, cluster_count),
        "prop_door_rotating" => pending_prop_door(entity, bsp, cluster_count),
        _ => None,
    }
}

fn pending_linear_brush_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
    class: MapDoorClass,
) -> Option<PendingMapDoor> {
    let model_index = parse_bmodel_index(entity.prop("model")?)?;
    let Some(model) = bsp.models.get(model_index.get()) else {
        log::debug!("bsp {class:?} skipped: invalid bmodel index {model_index}");
        return None;
    };
    let origin = entity_origin_or_model_origin(entity, model, class);
    let direction = entity
        .prop("movedir")
        .or_else(|| entity.prop("angles"))
        .and_then(parse_entity_vec3)
        .map_or(Vec3::new(1.0, 0.0, 0.0), angle_vectors_forward);
    let speed = parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed");
    let lip = parse_entity_float_default(entity.prop("lip"), 0.0, class, "lip");
    let model_distance = linear_door_distance(model, direction, lip);
    let distance = if class == MapDoorClass::FuncMoveLinear {
        // Lowercase: `MapEntity` folds every key at construction, and `prop`
        // is exact-match, so the BSP's own casing never reaches this lookup.
        entity
            .prop("movedistance")
            .and_then(parse_entity_float)
            .filter(|distance| *distance > 0.0)
            .unwrap_or(model_distance)
    } else {
        model_distance
    };
    let initial_progress = if class == MapDoorClass::FuncMoveLinear {
        entity
            .prop("startposition")
            .and_then(parse_entity_float)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    } else if door_spawn_starts_open(entity) {
        1.0
    } else {
        0.0
    };
    Some(PendingMapDoor {
        class,
        origin,
        angles: Vec3::splat(0.0),
        visibility: point_visibility_bucket(bsp, origin, cluster_count),
        auto_close_after: door_auto_close_after(entity, class),
        initial_progress,
        motion: MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        },
        sounds: brush_door_sounds(entity),
        geometry: PendingDoorGeometry::Brush { model_index },
    })
}

fn pending_rotating_brush_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    let class = MapDoorClass::FuncDoorRotating;
    let model_index = parse_bmodel_index(entity.prop("model")?)?;
    let Some(model) = bsp.models.get(model_index.get()) else {
        log::debug!("bsp {class:?} skipped: invalid bmodel index {model_index}");
        return None;
    };
    let origin = entity_origin_or_model_origin(entity, model, class);
    let angles = entity
        .prop("angles")
        .and_then(parse_entity_vec3)
        .unwrap_or(Vec3::splat(0.0));
    let spawnflags = parse_entity_spawnflags(entity);
    let degrees = parse_entity_float_default(entity.prop("distance"), 90.0, class, "distance")
        .abs()
        .max(0.0);
    let mut angle_delta = rotation_axis_delta(spawnflags, degrees);
    if spawnflags & SF_DOOR_ROTATE_BACKWARDS != 0 {
        angle_delta *= -1.0;
    }
    Some(PendingMapDoor {
        class,
        origin,
        angles,
        visibility: point_visibility_bucket(bsp, origin, cluster_count),
        auto_close_after: door_auto_close_after(entity, class),
        initial_progress: if door_spawn_starts_open(entity) {
            1.0
        } else {
            0.0
        },
        motion: MapDoorMotion::Rotating {
            angle_delta,
            degrees,
            speed: parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed"),
            open_direction: MapDoorOpenDirection::Both,
        },
        sounds: brush_door_sounds(entity),
        geometry: PendingDoorGeometry::Brush { model_index },
    })
}

fn pending_prop_door(
    entity: &MapEntity,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<PendingMapDoor> {
    let class = MapDoorClass::PropDoorRotating;
    let model = entity.prop("model")?;
    let Some(model_path) = normalize_entity_prop_model_path(model) else {
        log::debug!("bsp prop_door_rotating skipped: invalid model {model:?}");
        return None;
    };
    let Some(origin) = entity.prop("origin").and_then(parse_entity_vec3) else {
        log::debug!("bsp prop_door_rotating skipped: missing/invalid origin");
        return None;
    };
    let angles = entity
        .prop("angles")
        .and_then(parse_entity_vec3)
        .unwrap_or(Vec3::splat(0.0));
    let skin = entity.prop("skin").and_then(parse_entity_i32).unwrap_or(0);
    let degrees = parse_entity_float_default(entity.prop("distance"), 90.0, class, "distance")
        .abs()
        .max(0.0);
    let open_direction = match entity
        .prop("opendir")
        .and_then(parse_entity_i32)
        .unwrap_or(0)
    {
        1 => MapDoorOpenDirection::Forward,
        2 => MapDoorOpenDirection::Backward,
        _ => MapDoorOpenDirection::Both,
    };
    let visibility = point_visibility_bucket(bsp, origin, cluster_count);
    let placement = StaticPropPlacement {
        model_path,
        origin,
        angles,
        skin,
        scale: 1.0,
        solid: MapPropSolid::None,
        visibility: visibility.into(),
    };
    Some(PendingMapDoor {
        class,
        origin,
        angles,
        visibility,
        auto_close_after: prop_door_auto_close_after(entity, class),
        initial_progress: prop_door_initial_progress(entity),
        motion: MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, degrees, 0.0),
            degrees,
            speed: parse_entity_float_default(entity.prop("speed"), 100.0, class, "speed"),
            open_direction,
        },
        sounds: prop_door_sounds(entity),
        geometry: PendingDoorGeometry::Prop { placement },
    })
}

fn brush_door_sounds(entity: &MapEntity) -> MapDoorSounds {
    MapDoorSounds {
        move_sound: normalized_sound_keyvalue(entity.prop("noise1")),
        stop_sound: normalized_sound_keyvalue(entity.prop("noise2")),
        open_sound: None,
        close_sound: None,
    }
}

fn prop_door_sounds(entity: &MapEntity) -> MapDoorSounds {
    // Source SDK 2013 `CBasePropDoor` fills missing sound overrides from
    // model `door_options` skin blocks before validating to script names.
    // The current `vmdl` path does not expose the embedded MDL keyvalues, so
    // unresolved prop-door overrides fall back to the mounted script defaults
    // used by GMod content. This keeps the runtime soundful and records the
    // model-keyvalues gap without touching raw-offset MDL parsing here.
    MapDoorSounds {
        move_sound: normalized_sound_keyvalue(entity.prop("soundmoveoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_MOVE_SOUND.to_owned())),
        stop_sound: None,
        open_sound: normalized_sound_keyvalue(entity.prop("soundopenoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_ARRIVE_SOUND.to_owned())),
        close_sound: normalized_sound_keyvalue(entity.prop("soundcloseoverride"))
            .or_else(|| Some(PROP_DOOR_DEFAULT_ARRIVE_SOUND.to_owned())),
    }
}

fn normalized_sound_keyvalue(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn extract_pending_door_meshes(
    pending: PendingMapDoor,
    bsp: &MapBsp,
    face_attributions: &FaceAttributions,
    context: &mut FaceAppendContext<'_>,
) -> Result<PendingDoorBuild, BspError> {
    let model_index = match pending.geometry {
        PendingDoorGeometry::Brush { model_index } => model_index,
        PendingDoorGeometry::Prop { .. } => {
            return Ok(PendingDoorBuild {
                door: pending,
                meshes: Vec::new(),
            });
        }
    };
    let mut meshes = BuildMeshes::default();
    let Some(model) = bsp.models.get(model_index.get()) else {
        return Ok(PendingDoorBuild {
            door: pending,
            meshes: Vec::new(),
        });
    };
    if let Some(face_range) = model_face_range(model, bsp.faces.len()) {
        let visibility = MapFaceVisibility::from_bucket(pending.visibility);
        let mut door_context = FaceAppendContext {
            meshes: &mut meshes,
            material_names: context.material_names,
            material_indexes: context.material_indexes,
            lightmap_samples: context.lightmap_samples,
            lightmap_blocks: context.lightmap_blocks,
        };
        for face_index in face_range {
            let Some(face) = bsp.face(face_index) else {
                continue;
            };
            append_face(
                bsp,
                face,
                face_index,
                face_attributions.partition(face_index),
                &visibility,
                &mut door_context,
            )?;
        }
    }
    Ok(PendingDoorBuild {
        door: pending,
        meshes: meshes.into_inner(),
    })
}

fn parse_bmodel_index(model: &str) -> Option<ModelIndex> {
    let index = model.strip_prefix('*')?.parse::<usize>().ok()?;
    (index > 0).then(|| ModelIndex::new(index))
}

fn entity_origin_or_model_origin(
    entity: &MapEntity,
    model: &BspModel,
    class: MapDoorClass,
) -> Vec3 {
    if let Some(origin) = entity.prop("origin") {
        if let Some(parsed) = parse_entity_vec3(origin) {
            return parsed;
        }
        log::debug!("bsp {class:?} invalid origin {origin:?}: using bmodel origin");
    }
    Vec3::from(model.origin)
}

pub(super) fn linear_door_distance(model: &BspModel, direction: Vec3, lip: f32) -> f32 {
    let mins = model.mins;
    let maxs = model.maxs;
    let size = std::array::from_fn(|axis| (maxs[axis] - mins[axis] - 2.0).max(0.0));
    (direction.dot_abs(Vec3::from(size)) - lip).max(0.0)
}

fn angle_vectors_forward(angles: Vec3) -> Vec3 {
    let QAngle { pitch, yaw, .. } = QAngle::from_source_degrees(angles);
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    // Unit by construction: cos²p(cos²y + sin²y) + sin²p == 1.
    Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, -sin_pitch)
}

fn parse_entity_float_default(
    value: Option<&str>,
    default: f32,
    class: MapDoorClass,
    field: &'static str,
) -> f32 {
    value.map_or(default, |value| {
        parse_entity_float(value).unwrap_or_else(|| {
            log::debug!("bsp {class:?} {field} invalid: defaulting to {default}");
            default
        })
    })
}

fn parse_entity_spawnflags(entity: &MapEntity) -> u32 {
    entity
        .prop("spawnflags")
        .and_then(parse_entity_i32)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0)
}

fn rotation_axis_delta(spawnflags: u32, degrees: f32) -> Vec3 {
    // Source SDK 2013 CBaseToggle::AxisDir checks roll before pitch; yaw is
    // the default. Preserve that priority so mixed wild flags degrade like
    // the engine instead of inventing a new axis.
    if spawnflags & SF_DOOR_ROTATE_ROLL != 0 {
        Vec3::new(0.0, 0.0, degrees)
    } else if spawnflags & SF_DOOR_ROTATE_PITCH != 0 {
        Vec3::new(degrees, 0.0, 0.0)
    } else {
        Vec3::new(0.0, degrees, 0.0)
    }
}

/// Seconds an opened brush door waits before closing itself.
///
/// `func_movelinear` has no auto-return in Source — it is a mover, not a door
/// — and the Toggle spawnflag turns auto-return off for the classes that do,
/// so neither takes the `wait` default.
fn door_auto_close_after(entity: &MapEntity, class: MapDoorClass) -> Option<f32> {
    if class == MapDoorClass::FuncMoveLinear
        || parse_entity_spawnflags(entity) & SF_DOOR_NO_AUTO_RETURN != 0
    {
        return None;
    }
    auto_close_after(entity.prop("wait"), 3.0, class, "wait")
}

/// `prop_door_rotating` spells the same delay `returndelay`, and defaults to
/// staying open.
fn prop_door_auto_close_after(entity: &MapEntity, class: MapDoorClass) -> Option<f32> {
    auto_close_after(entity.prop("returndelay"), -1.0, class, "returndelay")
}

/// A door delay keyvalue as an absence-or-duration.
///
/// Source reads a negative delay as "never auto-return" — the usual way a
/// mapper pins a door open, and far more common than the Toggle spawnflag. Both
/// keyvalues spell it the same way, so both fold it into `None` here rather
/// than passing a negative duration on for a consumer to re-interpret.
fn auto_close_after(
    value: Option<&str>,
    default: f32,
    class: MapDoorClass,
    field: &'static str,
) -> Option<f32> {
    let delay = parse_entity_float_default(value, default, class, field);
    (delay >= 0.0).then_some(delay)
}

fn door_spawn_starts_open(entity: &MapEntity) -> bool {
    parse_entity_spawnflags(entity) & SF_DOOR_START_OPEN_OBSOLETE != 0
        || entity.prop("spawnpos").and_then(parse_entity_i32) == Some(1)
}

fn prop_door_initial_progress(entity: &MapEntity) -> f32 {
    if parse_entity_spawnflags(entity) & SF_DOOR_START_OPEN_OBSOLETE != 0 {
        return 1.0;
    }
    matches!(
        entity.prop("spawnpos").and_then(parse_entity_i32),
        Some(1 | 2)
    )
    .then_some(1.0)
    .unwrap_or(0.0)
}
