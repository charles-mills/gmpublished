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

/// Where a door is in its open/close cycle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum DoorMotion {
    #[default]
    Idle,
    Moving,
    /// Stopped part-way while closing because the player is in the way; resumes
    /// on its own once they step clear.
    BlockedClosing,
    /// Fully open and counting down to an automatic close. Source's `wait`
    /// (`returndelay` on `prop_door_rotating`); a negative value means the door
    /// stays open and never enters this state.
    HoldingOpen {
        remaining: f32,
    },
}

impl DoorMotion {
    /// Whether the door still needs frames: moving, or counting down to close.
    pub(super) const fn needs_tick(self) -> bool {
        matches!(self, Self::Moving | Self::HoldingOpen { .. })
    }
}

#[derive(Debug, Clone)]
pub(super) struct DoorRuntime {
    pub(super) progress: f32,
    pub(super) target: DoorTarget,
    pub(super) motion: DoorMotion,
    pub(super) swing: DoorSwing,
    pub(super) bounds_min: [f32; 3],
    pub(super) bounds_max: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DoorRenderPose {
    pub(super) progress: f32,
    pub(super) swing: DoorSwing,
}

/// Which way a door actually swings, once [`MapDoorOpenDirection::Both`] has
/// been resolved against the side the player opened it from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DoorSwing {
    #[default]
    Positive,
    Negative,
}

impl DoorSwing {
    pub(super) const fn sign(self) -> f32 {
        match self {
            Self::Positive => 1.0,
            Self::Negative => -1.0,
        }
    }
}

pub(super) fn initial_door_swing(motion: MapDoorMotion) -> DoorSwing {
    match motion {
        MapDoorMotion::Rotating {
            open_direction: MapDoorOpenDirection::Forward,
            ..
        } => DoorSwing::Negative,
        _ => DoorSwing::Positive,
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

pub(super) fn choose_door_swing(
    door: &DoorInstance,
    player_position: [f32; 3],
    _view_direction: [f32; 3],
) -> DoorSwing {
    let MapDoorMotion::Rotating { open_direction, .. } = door.motion else {
        return DoorSwing::Positive;
    };
    if door.class != MapDoorClass::PropDoorRotating {
        return DoorSwing::Positive;
    }
    match open_direction {
        MapDoorOpenDirection::Forward => DoorSwing::Negative,
        MapDoorOpenDirection::Backward => DoorSwing::Positive,
        MapDoorOpenDirection::Both => {
            let forward = rotate_source_vector([1.0, 0.0, 0.0], door.angles);
            if dot(sub(player_position, door.origin), forward) >= 0.0 {
                DoorSwing::Positive
            } else {
                DoorSwing::Negative
            }
        }
    }
}

pub(super) fn door_world_bounds(
    door: &DoorInstance,
    progress: f32,
    swing: DoorSwing,
) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for x in [door.local_bounds_min[0], door.local_bounds_max[0]] {
        for y in [door.local_bounds_min[1], door.local_bounds_max[1]] {
            for z in [door.local_bounds_min[2], door.local_bounds_max[2]] {
                let point = transform_door_point(door, [x, y, z], progress, swing);
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
                transform_door_point(door, vertex.position, pose.progress, pose.swing);
            transformed.normal =
                transform_door_normal(door, vertex.normal, pose.progress, pose.swing);
            transformed
        })
        .collect()
}

pub(super) fn transform_door_point(
    door: &DoorInstance,
    local: [f32; 3],
    progress: f32,
    swing: DoorSwing,
) -> [f32; 3] {
    let progress = progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            ..
        } => add(add(door.origin, local), mul(direction, distance * progress)),
        MapDoorMotion::Rotating { angle_delta, .. } => {
            let delta = mul(angle_delta, progress * swing.sign());
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
    swing: DoorSwing,
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
            let delta = mul(angle_delta, progress * swing.sign());
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

#[cfg(test)]
mod tests {
    use super::super::test_support::empty_preview;
    use super::super::{
        FlyCamera, FlyPose, MapVisibilityBucket, MapWalkCollision, ModelPreview, MovementMode,
    };
    use super::*;
    use crate::features::file_preview::model::DoorSounds;

    fn door_scene(doors: Vec<DoorInstance>) -> ModelPreview {
        let mut scene = empty_preview([-128.0, -128.0, -128.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [1000.0, 1000.0, 1000.0],
            [1100.0, 1100.0, 1100.0],
        ));
        scene.doors = doors;
        scene
    }

    fn test_linear_door(origin: [f32; 3], distance: f32) -> DoorInstance {
        DoorInstance {
            class: MapDoorClass::FuncDoor,
            origin,
            angles: [0.0; 3],
            local_bounds_min: [0.0, -16.0, -32.0],
            local_bounds_max: [8.0, 16.0, 32.0],
            visibility: MapVisibilityBucket::Always,
            initial_progress: 0.0,
            // Stays open: the auto-close tests opt in explicitly.
            auto_close_after: None,
            motion: MapDoorMotion::Linear {
                direction: [1.0, 0.0, 0.0],
                distance,
                speed: 100.0,
            },
            sounds: DoorSounds::default(),
            meshes: Vec::new(),
        }
    }

    fn test_door_sound(reference: &str) -> DoorSound {
        DoorSound {
            reference: reference.to_owned(),
            sound_level: 75.0,
            volume: 1.0,
            waves: Vec::new(),
        }
    }

    fn walk_camera_for_scene(scene: &ModelPreview, position: [f32; 3], yaw: f32) -> FlyCamera {
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(
            scene,
            None,
            1,
            Some(FlyPose {
                position,
                yaw,
                pitch: 0.0,
                speed: 1.0,
            }),
            Some(MovementMode::Fly),
        );
        camera.mode = MovementMode::Walk;
        camera.grounded = true;
        camera
    }

    #[test]
    fn door_toggle_reverses_mid_transition_and_then_goes_idle() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(matches!(
            camera.toggle_nearest_door(&scene, 1),
            Some(DoorAudioEvent {
                kind: DoorAudioEventKind::MoveStarted,
                ..
            })
        ));
        assert_eq!(camera.doors[0].motion, DoorMotion::Moving);
        let events = camera.integrate_doors(&scene, 1, 0.25);
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                DoorAudioEventKind::MoveLoopVolumeChanged | DoorAudioEventKind::MotionEnded { .. }
            )
        }));
        assert!(camera.doors[0].progress > 0.24 && camera.doors[0].progress < 0.26);
        assert_eq!(camera.doors[0].target, DoorTarget::Open);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        let _ = camera.integrate_doors(&scene, 1, 0.10);
        assert!(
            camera.doors[0].progress < 0.25,
            "closing after a mid-transition toggle must reverse from the current pose"
        );
        for _ in 0..60 {
            let _ = camera.integrate_doors(&scene, 1, 1.0 / 60.0);
        }

        assert_eq!(camera.doors[0].progress, 0.0);
        assert_eq!(camera.doors[0].motion, DoorMotion::Idle);
        assert!(
            !camera.needs_movement_tick(),
            "a settled door must not keep the redraw loop alive"
        );
    }

    #[test]
    fn an_opened_door_closes_itself_after_its_auto_close_delay() {
        let mut door = test_linear_door([40.0, 0.0, 64.0], 100.0);
        door.auto_close_after = Some(0.5);
        let scene = door_scene(vec![door]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        let _ = camera.integrate_doors(&scene, 1, 2.0);
        assert_eq!(camera.doors[0].progress, 1.0);
        assert!(
            matches!(camera.doors[0].motion, DoorMotion::HoldingOpen { .. }),
            "an opened door with a wait holds before closing: {:?}",
            camera.doors[0].motion
        );
        assert!(
            camera.needs_movement_tick(),
            "the hold needs frames or its timer never advances"
        );

        // Part-way through the hold nothing has moved yet.
        let _ = camera.integrate_doors(&scene, 1, 0.3);
        assert_eq!(camera.doors[0].progress, 1.0);

        let events = camera.integrate_doors(&scene, 1, 0.3);
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        assert!(
            events
                .iter()
                .any(|event| event.kind == DoorAudioEventKind::MoveStarted),
            "closing on its own sounds like any other close: {events:?}"
        );

        for _ in 0..120 {
            let _ = camera.integrate_doors(&scene, 1, 1.0 / 60.0);
        }
        assert_eq!(camera.doors[0].progress, 0.0);
        assert_eq!(camera.doors[0].motion, DoorMotion::Idle);
    }

    #[test]
    fn a_door_with_no_auto_close_delay_stays_open() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        let _ = camera.integrate_doors(&scene, 1, 2.0);
        assert_eq!(camera.doors[0].motion, DoorMotion::Idle);

        let _ = camera.integrate_doors(&scene, 1, 60.0);
        assert_eq!(camera.doors[0].progress, 1.0);
    }

    #[test]
    fn door_endpoint_goes_idle_and_emits_stop_event() {
        let mut door = test_linear_door([40.0, 0.0, 64.0], 100.0);
        door.sounds.stop_sound = Some(test_door_sound("doors/door1_stop.wav"));
        let scene = door_scene(vec![door]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].motion, DoorMotion::Moving);

        let events = camera.integrate_doors(&scene, 1, 2.0);

        assert_eq!(camera.doors[0].progress, 1.0);
        assert_eq!(camera.doors[0].motion, DoorMotion::Idle);
        assert!(events.iter().any(|event| {
            event.door_index == 0
                && event.gain > 0.0
                && event.kind == (DoorAudioEventKind::MotionEnded { open: true })
        }));
    }

    #[test]
    fn blocked_closing_door_parks_and_emits_parked_event() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, [50.0, 0.0, 64.0], 0.0);
        camera.doors[0].progress = 0.2;
        camera.doors[0].target = DoorTarget::Closed;
        camera.doors[0].motion = DoorMotion::Moving;
        (camera.doors[0].bounds_min, camera.doors[0].bounds_max) =
            door_world_bounds(&scene.doors[0], 0.2, DoorSwing::Positive);

        let events = camera.integrate_doors(&scene, 1, 1.0 / 60.0);

        assert_eq!(camera.doors[0].motion, DoorMotion::BlockedClosing);
        assert!(
            events
                .iter()
                .any(|event| { event.door_index == 0 && event.kind == DoorAudioEventKind::Parked })
        );
    }

    #[test]
    fn use_ray_picks_nearest_door_and_ignores_doors_beyond_reach() {
        let scene = door_scene(vec![
            test_linear_door([70.0, 0.0, 64.0], 32.0),
            test_linear_door([40.0, 0.0, 64.0], 32.0),
        ]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        assert_eq!(camera.doors[1].target, DoorTarget::Open);
        assert_eq!(camera.doors[1].motion, DoorMotion::Moving);

        let far_scene = door_scene(vec![test_linear_door([90.0, 0.0, 64.0], 32.0)]);
        let mut far_camera = walk_camera_for_scene(&far_scene, [0.0, 0.0, 64.0], 0.0);
        assert!(
            far_camera.toggle_nearest_door(&far_scene, 1).is_none(),
            "use reach is capped at 80 Source units"
        );
        assert_eq!(far_camera.doors[0].target, DoorTarget::Closed);
    }

    #[test]
    fn walk_trace_hits_door_at_current_mid_swing_pose() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 40.0)]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);
        camera.doors[0].progress = 0.5;
        (camera.doors[0].bounds_min, camera.doors[0].bounds_max) =
            door_world_bounds(&scene.doors[0], 0.5, DoorSwing::Positive);
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let hit = camera.trace_aabb(collision, [50.0, 0.0, 64.0], [80.0, 0.0, 64.0], [1.0; 3]);

        assert!(!hit.start_solid);
        assert!(hit.fraction > 0.29 && hit.fraction < 0.31, "{hit:?}");
        assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
        assert!((hit.end_position[0] - 59.0).abs() < 1.0e-4, "{hit:?}");
    }

    #[test]
    fn source_sound_gain_matches_documented_three_point_falloff() {
        let near = source_sound_gain(64.0, 75.0);
        let mid = source_sound_gain(750.0, 75.0);
        let far = source_sound_gain(1500.0, 75.0);

        assert_eq!(near, 1.0);
        assert!(mid > 0.0 && mid < near);
        assert_eq!(far, 0.0);
    }
}
