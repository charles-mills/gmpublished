use super::camera::rotate_source_vector;
use super::{
    DoorAudioEvent, DoorAudioEventKind, DoorInstance, DoorSound, MapDoorClass, MapDoorMotion,
    MapDoorOpenDirection, MapTrace, ModelVertex, add, distance, dot, length_squared, mid, mul,
    normalize, normalize_or_zero, sub,
};

pub(super) const DOOR_USE_REACH: f32 = 80.0;
pub(super) const DOOR_PROGRESS_EPSILON: f32 = 1.0e-4;
pub(super) const SOURCE_NORM_AUDIBLE_RADIUS: f32 = 1500.0;
pub(super) const SOURCE_NEAR_FULL_GAIN_RADIUS: f32 = 64.0;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum DoorTarget {
    Closed,
    Open,
}

#[derive(Debug, Clone)]
pub(super) struct DoorRuntime {
    pub(super) progress: f32,
    pub(super) target: DoorTarget,
    pub(super) moving: bool,
    pub(super) blocked_closing: bool,
    pub(super) move_loop: Option<DoorMoveLoopRuntime>,
    pub(super) open_sign: f32,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DoorMoveLoopRuntime;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DoorRenderPose {
    pub(super) progress: f32,
    pub(super) open_sign: f32,
}

pub(super) fn initial_door_open_sign(motion: MapDoorMotion) -> f32 {
    match motion {
        MapDoorMotion::Rotating {
            open_direction: MapDoorOpenDirection::Forward,
            ..
        } => -1.0,
        _ => 1.0,
    }
}

pub(super) fn door_audio_event(
    content_id: u64,
    door_index: usize,
    kind: DoorAudioEventKind,
    gain: f32,
) -> DoorAudioEvent {
    DoorAudioEvent {
        content_id,
        door_index,
        kind,
        gain,
    }
}

pub(super) fn door_uses_move_loop(class: MapDoorClass) -> bool {
    matches!(
        class,
        MapDoorClass::FuncDoor | MapDoorClass::FuncDoorRotating | MapDoorClass::FuncMoveLinear
    )
}

pub(super) fn endpoint_sound(door: &DoorInstance, open: bool) -> Option<&DoorSound> {
    match door.class {
        MapDoorClass::PropDoorRotating if open => door.sounds.open_sound.as_ref(),
        MapDoorClass::PropDoorRotating => door.sounds.close_sound.as_ref(),
        _ => door.sounds.stop_sound.as_ref(),
    }
}

pub(super) fn door_sound_gain(
    listener: Option<[f32; 3]>,
    bounds: ([f32; 3], [f32; 3]),
    sound: Option<&DoorSound>,
) -> f32 {
    let Some(listener) = listener else {
        return 0.0;
    };
    let Some(sound) = sound else {
        return 0.0;
    };
    source_sound_gain(
        distance(listener, mid(bounds.0, bounds.1)),
        sound.sound_level,
    ) * sound.volume
}

pub(super) fn source_sound_gain(distance: f32, sound_level: f32) -> f32 {
    if !distance.is_finite() || !sound_level.is_finite() {
        return 0.0;
    }
    // Source's sound engine applies soundlevel attenuation. The preview has
    // no PAS/DSP/panning model, so approximate SNDLVL_NORM (75 dB) as
    // inaudible beyond ~1500 Source units and scale that radius by soundlevel.
    let audible_radius = SOURCE_NORM_AUDIBLE_RADIUS * 10.0_f32.powf((sound_level - 75.0) / 40.0);
    if audible_radius <= SOURCE_NEAR_FULL_GAIN_RADIUS || distance >= audible_radius {
        return 0.0;
    }
    if distance <= SOURCE_NEAR_FULL_GAIN_RADIUS {
        return 1.0;
    }
    let fade = ((audible_radius - distance) / (audible_radius - SOURCE_NEAR_FULL_GAIN_RADIUS))
        .clamp(0.0, 1.0);
    let inverse = (SOURCE_NEAR_FULL_GAIN_RADIUS / distance).sqrt();
    (fade * inverse).clamp(0.0, 1.0)
}

pub(super) fn door_progress_step(motion: MapDoorMotion, dt: f32) -> f32 {
    if !dt.is_finite() || dt <= 0.0 {
        return 0.0;
    }
    let (speed, span) = match motion {
        MapDoorMotion::Linear {
            distance, speed, ..
        } => (speed, distance),
        MapDoorMotion::Rotating { degrees, speed, .. } => (speed, degrees),
    };
    if !speed.is_finite() || !span.is_finite() || speed <= 0.0 || span <= DOOR_PROGRESS_EPSILON {
        return 1.0;
    }
    (dt * speed / span).clamp(0.0, 1.0)
}

pub(super) fn choose_door_open_sign(
    door: &DoorInstance,
    player_position: [f32; 3],
    _view_direction: [f32; 3],
) -> f32 {
    let MapDoorMotion::Rotating { open_direction, .. } = door.motion else {
        return 1.0;
    };
    if door.class != MapDoorClass::PropDoorRotating {
        return 1.0;
    }
    match open_direction {
        MapDoorOpenDirection::Forward => -1.0,
        MapDoorOpenDirection::Backward => 1.0,
        MapDoorOpenDirection::Both => {
            let forward = rotate_source_vector([1.0, 0.0, 0.0], door.angles);
            if dot(sub(player_position, door.origin), forward) >= 0.0 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

pub(super) fn door_world_bounds(
    door: &DoorInstance,
    progress: f32,
    open_sign: f32,
) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for x in [door.local_bounds_min[0], door.local_bounds_max[0]] {
        for y in [door.local_bounds_min[1], door.local_bounds_max[1]] {
            for z in [door.local_bounds_min[2], door.local_bounds_max[2]] {
                let point = transform_door_point(door, [x, y, z], progress, open_sign);
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
            }
        }
    }
    if min.iter().all(|value| value.is_finite()) && max.iter().all(|value| value.is_finite()) {
        (min, max)
    } else {
        (door.origin, door.origin)
    }
}

pub(super) fn transform_door_vertices(
    door: &DoorInstance,
    vertices: &[ModelVertex],
    pose: DoorRenderPose,
) -> Vec<ModelVertex> {
    vertices
        .iter()
        .map(|vertex| {
            let mut transformed = *vertex;
            transformed.position =
                transform_door_point(door, vertex.position, pose.progress, pose.open_sign);
            transformed.normal =
                transform_door_normal(door, vertex.normal, pose.progress, pose.open_sign);
            transformed
        })
        .collect()
}

pub(super) fn transform_door_point(
    door: &DoorInstance,
    local: [f32; 3],
    progress: f32,
    open_sign: f32,
) -> [f32; 3] {
    let progress = progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            ..
        } => add(add(door.origin, local), mul(direction, distance * progress)),
        MapDoorMotion::Rotating { angle_delta, .. } => {
            let delta = mul(angle_delta, progress * open_sign);
            let angles = if door.class == MapDoorClass::PropDoorRotating {
                add(door.angles, delta)
            } else {
                delta
            };
            add(door.origin, rotate_source_vector(local, angles))
        }
    }
}

pub(super) fn transform_door_normal(
    door: &DoorInstance,
    normal: [f32; 3],
    progress: f32,
    open_sign: f32,
) -> [f32; 3] {
    let progress = progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear { .. } => {
            if door.class == MapDoorClass::PropDoorRotating {
                normalize(rotate_source_vector(normal, door.angles))
            } else {
                normal
            }
        }
        MapDoorMotion::Rotating { angle_delta, .. } => {
            let delta = mul(angle_delta, progress * open_sign);
            let angles = if door.class == MapDoorClass::PropDoorRotating {
                add(door.angles, delta)
            } else {
                delta
            };
            normalize(rotate_source_vector(normal, angles))
        }
    }
}

pub(super) fn ray_aabb_distance(
    start: [f32; 3],
    direction: [f32; 3],
    bounds: ([f32; 3], [f32; 3]),
) -> Option<f32> {
    let direction = normalize_or_zero(direction);
    if bounds_contains_point(bounds, start) {
        return Some(0.0);
    }
    let mut enter = 0.0_f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() <= f32::EPSILON {
            if start[axis] < bounds.0[axis] || start[axis] > bounds.1[axis] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / direction[axis];
        let mut t0 = (bounds.0[axis] - start[axis]) * inv;
        let mut t1 = (bounds.1[axis] - start[axis]) * inv;
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        enter = enter.max(t0);
        exit = exit.min(t1);
        if enter > exit {
            return None;
        }
    }
    (exit >= 0.0).then_some(enter.max(0.0))
}

pub(super) fn trace_aabb_against_aabb(
    start: [f32; 3],
    end: [f32; 3],
    half_extents: [f32; 3],
    bounds: ([f32; 3], [f32; 3]),
) -> Option<MapTrace> {
    let expanded = expand_bounds(bounds, half_extents);
    if bounds_contains_point(expanded, start) {
        return Some(MapTrace {
            fraction: 0.0,
            end_position: start,
            normal: [0.0; 3],
            start_solid: true,
        });
    }
    let delta = sub(end, start);
    if length_squared(delta) <= f32::EPSILON {
        return None;
    }
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;
    let mut normal = [0.0; 3];
    for axis in 0..3 {
        if delta[axis].abs() <= f32::EPSILON {
            if start[axis] < expanded.0[axis] || start[axis] > expanded.1[axis] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / delta[axis];
        let mut t0 = (expanded.0[axis] - start[axis]) * inv;
        let mut t1 = (expanded.1[axis] - start[axis]) * inv;
        let mut axis_normal = [0.0; 3];
        axis_normal[axis] = if delta[axis] > 0.0 { -1.0 } else { 1.0 };
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
            axis_normal[axis] *= -1.0;
        }
        if t0 > enter {
            enter = t0;
            normal = axis_normal;
        }
        exit = exit.min(t1);
        if enter > exit {
            return None;
        }
    }
    if !(0.0..=1.0).contains(&enter) {
        return None;
    }
    let fraction = enter.clamp(0.0, 1.0);
    Some(MapTrace {
        fraction,
        end_position: add(start, mul(delta, fraction)),
        normal,
        start_solid: false,
    })
}

pub(super) fn bounds_contains_point(bounds: ([f32; 3], [f32; 3]), point: [f32; 3]) -> bool {
    (0..3).all(|axis| point[axis] >= bounds.0[axis] && point[axis] <= bounds.1[axis])
}

pub(super) fn bounds_intersect(left: ([f32; 3], [f32; 3]), right: ([f32; 3], [f32; 3])) -> bool {
    (0..3).all(|axis| left.0[axis] <= right.1[axis] && left.1[axis] >= right.0[axis])
}

pub(super) fn expand_bounds(
    bounds: ([f32; 3], [f32; 3]),
    half_extents: [f32; 3],
) -> ([f32; 3], [f32; 3]) {
    (
        [
            bounds.0[0] - half_extents[0],
            bounds.0[1] - half_extents[1],
            bounds.0[2] - half_extents[2],
        ],
        [
            bounds.1[0] + half_extents[0],
            bounds.1[1] + half_extents[1],
            bounds.1[2] + half_extents[2],
        ],
    )
}
