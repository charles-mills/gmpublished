use super::super::state::MovementMode;
use super::camera::FlyCamera;
use super::{
    DoorAudioEvent, DoorAudioEventKind, DoorInstance, DoorSound, MapDoorClass, MapDoorMotion,
    MapDoorOpenDirection, MapPreview, MapTrace, MapVisibilityBucket, ModelVertex, mid,
    normalize_or_up,
};
use gmpublished_domain::math::Vec3;

pub(super) const DOOR_USE_REACH: f32 = 80.0;
pub(super) const DOOR_PROGRESS_EPSILON: f32 = 1.0e-4;
pub(super) const SOURCE_NORM_AUDIBLE_RADIUS: f32 = 1500.0;
pub(super) const SOURCE_NEAR_FULL_GAIN_RADIUS: f32 = 64.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug)]
pub(super) struct DoorRuntime {
    pub(super) progress: f32,
    pub(super) target: DoorTarget,
    pub(super) motion: DoorMotion,
    pub(super) swing: DoorSwing,
    pub(super) bounds_min: Vec3,
    pub(super) bounds_max: Vec3,
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
    listener: Option<Vec3>,
    bounds: (Vec3, Vec3),
    sound: Option<&DoorSound>,
) -> f32 {
    let Some(listener) = listener else {
        return 0.0;
    };
    let Some(sound) = sound else {
        return 0.0;
    };
    source_sound_gain(
        listener.distance(mid(bounds.0, bounds.1)),
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
    player_position: Vec3,
    _view_direction: Vec3,
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
            let forward = (Vec3::new(1.0, 0.0, 0.0)).rotate_source(door.angles);
            if (player_position - door.origin).dot(forward) >= 0.0 {
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
) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for x in [door.local_bounds_min[0], door.local_bounds_max[0]] {
        for y in [door.local_bounds_min[1], door.local_bounds_max[1]] {
            for z in [door.local_bounds_min[2], door.local_bounds_max[2]] {
                let point = transform_door_point(door, Vec3::new(x, y, z), progress, swing);
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
            }
        }
    }
    if min.is_finite() && max.is_finite() {
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
    local: Vec3,
    progress: f32,
    swing: DoorSwing,
) -> Vec3 {
    let progress = progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            ..
        } => (door.origin + local) + (direction * (distance * progress)),
        MapDoorMotion::Rotating { angle_delta, .. } => {
            let delta = angle_delta * (progress * swing.sign());
            let angles = if door.class == MapDoorClass::PropDoorRotating {
                door.angles + delta
            } else {
                delta
            };
            door.origin + (local.rotate_source(angles))
        }
    }
}

pub(super) fn transform_door_normal(
    door: &DoorInstance,
    normal: Vec3,
    progress: f32,
    swing: DoorSwing,
) -> Vec3 {
    let progress = progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear { .. } => {
            if door.class == MapDoorClass::PropDoorRotating {
                normalize_or_up(normal.rotate_source(door.angles))
            } else {
                normal
            }
        }
        MapDoorMotion::Rotating { angle_delta, .. } => {
            let delta = angle_delta * (progress * swing.sign());
            let angles = if door.class == MapDoorClass::PropDoorRotating {
                door.angles + delta
            } else {
                delta
            };
            normalize_or_up(normal.rotate_source(angles))
        }
    }
}

pub(super) fn ray_aabb_distance(start: Vec3, direction: Vec3, bounds: (Vec3, Vec3)) -> Option<f32> {
    let direction = direction.normalize_or_zero();
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
    start: Vec3,
    end: Vec3,
    half_extents: Vec3,
    bounds: (Vec3, Vec3),
) -> Option<MapTrace> {
    let expanded = expand_bounds(bounds, half_extents);
    if bounds_contains_point(expanded, start) {
        return Some(MapTrace {
            fraction: 0.0,
            end_position: start,
            normal: Vec3::splat(0.0),
            start_solid: true,
        });
    }
    let delta = end - start;
    if delta.length_squared() <= f32::EPSILON {
        return None;
    }
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;
    let mut normal = Vec3::splat(0.0);
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
        let mut axis_normal = Vec3::splat(0.0);
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
        end_position: (start + (delta * fraction)),
        normal,
        start_solid: false,
    })
}

pub(super) fn bounds_contains_point(bounds: (Vec3, Vec3), point: Vec3) -> bool {
    (0..3).all(|axis| point[axis] >= bounds.0[axis] && point[axis] <= bounds.1[axis])
}

pub(super) fn bounds_intersect(left: (Vec3, Vec3), right: (Vec3, Vec3)) -> bool {
    (0..3).all(|axis| left.0[axis] <= right.1[axis] && left.1[axis] >= right.0[axis])
}

pub(super) fn expand_bounds(bounds: (Vec3, Vec3), half_extents: Vec3) -> (Vec3, Vec3) {
    (
        Vec3::new(
            bounds.0[0] - half_extents[0],
            bounds.0[1] - half_extents[1],
            bounds.0[2] - half_extents[2],
        ),
        Vec3::new(
            bounds.1[0] + half_extents[0],
            bounds.1[1] + half_extents[1],
            bounds.1[2] + half_extents[2],
        ),
    )
}

impl FlyCamera {
    pub(super) fn integrate_doors(
        &mut self,
        scene: &MapPreview,
        content_id: u64,
        dt: f32,
    ) -> Vec<DoorAudioEvent> {
        let player_hull = self.player_hull_bounds();
        let listener = self.position;
        let mut audio_events = Vec::new();
        for (index, runtime) in self.doors.iter_mut().enumerate() {
            if let DoorMotion::HoldingOpen { remaining } = runtime.motion {
                if remaining > dt {
                    runtime.motion = DoorMotion::HoldingOpen {
                        remaining: remaining - dt,
                    };
                    continue;
                }
                // The hold elapsed: close on its own, exactly as a toggle would.
                runtime.motion = DoorMotion::Moving;
                runtime.target = DoorTarget::Closed;
                if let Some(door) = scene.doors.get(index) {
                    let gain = door_sound_gain(
                        listener,
                        (runtime.bounds_min, runtime.bounds_max),
                        door.sounds.move_sound.as_ref(),
                    );
                    audio_events.push(door_audio_event(
                        content_id,
                        index,
                        DoorAudioEventKind::MoveStarted,
                        gain,
                    ));
                }
            }
            if runtime.motion != DoorMotion::Moving {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                runtime.motion = DoorMotion::Idle;
                continue;
            };
            let step = door_progress_step(door.motion, dt);
            let next_progress = match runtime.target {
                DoorTarget::Open => (runtime.progress + step).min(1.0),
                DoorTarget::Closed => (runtime.progress - step).max(0.0),
            };
            if runtime.target == DoorTarget::Closed
                && player_hull.is_some_and(|hull| {
                    let bounds = door_world_bounds(door, next_progress, runtime.swing);
                    bounds_intersect(bounds, hull)
                })
            {
                runtime.motion = DoorMotion::BlockedClosing;
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::Parked,
                    0.0,
                ));
                continue;
            }
            runtime.progress = next_progress;
            (runtime.bounds_min, runtime.bounds_max) =
                door_world_bounds(door, runtime.progress, runtime.swing);
            if (runtime.target == DoorTarget::Open
                && runtime.progress >= 1.0 - DOOR_PROGRESS_EPSILON)
                || (runtime.target == DoorTarget::Closed
                    && runtime.progress <= DOOR_PROGRESS_EPSILON)
            {
                runtime.progress = match runtime.target {
                    DoorTarget::Open => 1.0,
                    DoorTarget::Closed => 0.0,
                };
                (runtime.bounds_min, runtime.bounds_max) =
                    door_world_bounds(door, runtime.progress, runtime.swing);
                let open = runtime.target == DoorTarget::Open;
                runtime.motion = door
                    .auto_close_after
                    .filter(|_| open)
                    .map_or(DoorMotion::Idle, |remaining| DoorMotion::HoldingOpen {
                        remaining,
                    });
                let sound = endpoint_sound(door, open);
                let gain =
                    door_sound_gain(listener, (runtime.bounds_min, runtime.bounds_max), sound);
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MotionEnded { open },
                    gain,
                ));
            } else if door_uses_move_loop(door.class) {
                let gain = door_sound_gain(
                    listener,
                    (runtime.bounds_min, runtime.bounds_max),
                    door.sounds.move_sound.as_ref(),
                );
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MoveLoopVolumeChanged,
                    gain,
                ));
            }
        }
        audio_events
    }

    pub(super) fn resume_blocked_doors_if_clear(
        &mut self,
        scene: &MapPreview,
        content_id: u64,
    ) -> Vec<DoorAudioEvent> {
        let player_hull = self.player_hull_bounds();
        let listener = self.position;
        let mut audio_events = Vec::new();
        for (index, runtime) in self.doors.iter_mut().enumerate() {
            if runtime.motion != DoorMotion::BlockedClosing {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                continue;
            };
            let next_progress = (runtime.progress - DOOR_PROGRESS_EPSILON).max(0.0);
            let bounds = door_world_bounds(door, next_progress, runtime.swing);
            if player_hull.is_none_or(|hull| !bounds_intersect(bounds, hull)) {
                runtime.motion = DoorMotion::Moving;
                runtime.target = DoorTarget::Closed;
                let gain = door_sound_gain(
                    listener,
                    (runtime.bounds_min, runtime.bounds_max),
                    door.sounds.move_sound.as_ref(),
                );
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MoveStarted,
                    gain,
                ));
            }
        }
        audio_events
    }

    pub(super) fn toggle_nearest_door(
        &mut self,
        scene: &MapPreview,
        content_id: u64,
    ) -> Option<DoorAudioEvent> {
        if self.mode != MovementMode::Walk {
            return None;
        }
        let start = self.position?;
        let direction = self.forward();
        let mut best: Option<(usize, f32)> = None;
        for (index, (door, runtime)) in scene.doors.iter().zip(&self.doors).enumerate() {
            if !self.door_visible_from_current_cluster(scene, door) {
                continue;
            }
            let Some(distance) =
                ray_aabb_distance(start, direction, (runtime.bounds_min, runtime.bounds_max))
            else {
                continue;
            };
            if distance <= DOOR_USE_REACH && best.is_none_or(|(_, best)| distance < best) {
                best = Some((index, distance));
            }
        }
        let (index, _) = best?;
        let door = scene.doors.get(index)?;
        let runtime = &mut self.doors[index];
        if runtime.target == DoorTarget::Open {
            runtime.target = DoorTarget::Closed;
        } else {
            runtime.target = DoorTarget::Open;
            runtime.swing = choose_door_swing(door, start, direction);
        }
        runtime.motion = DoorMotion::Moving;
        let gain = door_sound_gain(
            self.position,
            (runtime.bounds_min, runtime.bounds_max),
            door.sounds.move_sound.as_ref(),
        );
        Some(door_audio_event(
            content_id,
            index,
            DoorAudioEventKind::MoveStarted,
            gain,
        ))
    }

    pub(super) fn door_visible_from_current_cluster(
        &self,
        scene: &MapPreview,
        door: &DoorInstance,
    ) -> bool {
        let Some(visibility) = scene.visibility.as_ref() else {
            return true;
        };
        let Some(position) = self.position else {
            return true;
        };
        let Some(cluster) = visibility.cluster_at(position) else {
            return true;
        };
        let Some(visible) = visibility.visible_clusters(cluster) else {
            return true;
        };
        match door.visibility {
            MapVisibilityBucket::Always => true,
            MapVisibilityBucket::Cluster(cluster) => {
                visible.get(cluster as usize).copied().unwrap_or(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::empty_preview;
    use super::super::{FlyCamera, FlyPose, MapPreview, MapVisibilityBucket, MovementMode};
    use super::*;
    use crate::media::preview_model::DoorSounds;
    use gmpublished_domain::scene::map::MapWalkCollision;

    fn door_scene(doors: Vec<DoorInstance>) -> MapPreview {
        let mut scene = empty_preview(
            Vec3::new(-128.0, -128.0, -128.0),
            Vec3::new(256.0, 128.0, 128.0),
        );
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            Vec3::new(1000.0, 1000.0, 1000.0),
            Vec3::new(1100.0, 1100.0, 1100.0),
        ));
        scene.doors = doors;
        scene
    }

    fn test_linear_door(origin: Vec3, distance: f32) -> DoorInstance {
        DoorInstance {
            class: MapDoorClass::FuncDoor,
            origin,
            angles: Vec3::splat(0.0),
            local_bounds_min: Vec3::new(0.0, -16.0, -32.0),
            local_bounds_max: Vec3::new(8.0, 16.0, 32.0),
            visibility: MapVisibilityBucket::Always,
            initial_progress: 0.0,
            // Stays open: the auto-close tests opt in explicitly.
            auto_close_after: None,
            motion: MapDoorMotion::Linear {
                direction: Vec3::new(1.0, 0.0, 0.0),
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

    fn walk_camera_for_scene(scene: &MapPreview, position: Vec3, yaw: f32) -> FlyCamera {
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
        let scene = door_scene(vec![test_linear_door(Vec3::new(40.0, 0.0, 64.0), 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);

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
        let mut door = test_linear_door(Vec3::new(40.0, 0.0, 64.0), 100.0);
        door.auto_close_after = Some(0.5);
        let scene = door_scene(vec![door]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);

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
        let scene = door_scene(vec![test_linear_door(Vec3::new(40.0, 0.0, 64.0), 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        let _ = camera.integrate_doors(&scene, 1, 2.0);
        assert_eq!(camera.doors[0].motion, DoorMotion::Idle);

        let _ = camera.integrate_doors(&scene, 1, 60.0);
        assert_eq!(camera.doors[0].progress, 1.0);
    }

    #[test]
    fn door_endpoint_goes_idle_and_emits_stop_event() {
        let mut door = test_linear_door(Vec3::new(40.0, 0.0, 64.0), 100.0);
        door.sounds.stop_sound = Some(test_door_sound("doors/door1_stop.wav"));
        let scene = door_scene(vec![door]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);

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
        let scene = door_scene(vec![test_linear_door(Vec3::new(40.0, 0.0, 64.0), 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(50.0, 0.0, 64.0), 0.0);
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
            test_linear_door(Vec3::new(70.0, 0.0, 64.0), 32.0),
            test_linear_door(Vec3::new(40.0, 0.0, 64.0), 32.0),
        ]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        assert_eq!(camera.doors[1].target, DoorTarget::Open);
        assert_eq!(camera.doors[1].motion, DoorMotion::Moving);

        let far_scene = door_scene(vec![test_linear_door(Vec3::new(90.0, 0.0, 64.0), 32.0)]);
        let mut far_camera = walk_camera_for_scene(&far_scene, Vec3::new(0.0, 0.0, 64.0), 0.0);
        assert!(
            far_camera.toggle_nearest_door(&far_scene, 1).is_none(),
            "use reach is capped at 80 Source units"
        );
        assert_eq!(far_camera.doors[0].target, DoorTarget::Closed);
    }

    #[test]
    fn walk_trace_hits_door_at_current_mid_swing_pose() {
        let scene = door_scene(vec![test_linear_door(Vec3::new(40.0, 0.0, 64.0), 40.0)]);
        let mut camera = walk_camera_for_scene(&scene, Vec3::new(0.0, 0.0, 64.0), 0.0);
        camera.doors[0].progress = 0.5;
        (camera.doors[0].bounds_min, camera.doors[0].bounds_max) =
            door_world_bounds(&scene.doors[0], 0.5, DoorSwing::Positive);
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let hit = camera.trace_aabb(
            collision,
            Vec3::new(50.0, 0.0, 64.0),
            Vec3::new(80.0, 0.0, 64.0),
            Vec3::splat(1.0),
        );

        assert!(!hit.start_solid);
        assert!(hit.fraction > 0.29 && hit.fraction < 0.31, "{hit:?}");
        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
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
