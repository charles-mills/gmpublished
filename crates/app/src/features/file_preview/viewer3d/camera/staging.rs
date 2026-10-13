use super::{
    Action, Event, FlyCamera, FlyViewer, Message, Point, Rectangle, SOURCE_UP, Vec3, mouse,
};
use crate::features::file_preview::state::StageTransform;

#[derive(Debug)]
pub(in super::super) struct StageGrab {
    distance: f32,
    offset: Vec3,
    direction: Vec3,
}

impl FlyViewer {
    pub(super) fn camera_message(
        &self,
        camera: &FlyCamera,
        door_audio_events: Vec<super::DoorAudioEvent>,
    ) -> Option<Message> {
        let pose = camera.pose()?;
        if let Some(stage) = &self.stage
            && let Some(grab) = &camera.stage_grab
        {
            let mut transform = stage.transform;
            transform.position = camera_eye(camera)? + grab.direction * grab.distance + grab.offset;
            Some(Message::StageCameraChanged {
                pose,
                mode: camera.mode(),
                transform,
                door_audio_events,
            })
        } else if door_audio_events.is_empty() {
            camera.camera_update_message()
        } else {
            Some(Message::FlyCameraAndDoorAudioChanged {
                pose,
                mode: camera.mode(),
                door_audio_events,
            })
        }
    }

    pub(super) fn stage_interaction(
        &self,
        camera: &mut FlyCamera,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let stage = self.stage.as_ref()?;
        match event {
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyZ),
                modifiers,
                ..
            }) if modifiers.command() => {
                camera.stage_grab = None;
                Some(Action::publish(Message::StageUndoRequested).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                let point = cursor.position_over(bounds)?;
                let (eye, direction) = cursor_ray(camera, bounds, point)?;
                let collision = self.scene.walk_collision.as_ref()?;
                let trace =
                    camera.trace_aabb(collision, eye, eye + direction * 32768.0, Vec3::ZERO);
                if trace.start_solid || trace.fraction >= 1.0 {
                    return Some(Action::capture());
                }
                let mut transform = stage.transform;
                let local = stage.local_bounds?;
                let support =
                    support_distance(local.min, local.max, transform.angles, trace.normal);
                transform.position = trace.end_position - trace.normal * support;
                Some(Action::publish(Message::StageTransformChanged(transform)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) => {
                let point = cursor.position_over(bounds)?;
                let (eye, direction) = cursor_ray(camera, bounds, point)?;
                let local = stage.local_bounds?;
                let distance = ray_box_distance(
                    stage.transform.local_point(eye),
                    stage.transform.inverse_vector(direction),
                    local.min,
                    local.max,
                )?;
                if let Some(collision) = &self.scene.walk_collision {
                    let trace =
                        camera.trace_aabb(collision, eye, eye + direction * distance, Vec3::ZERO);
                    if trace.start_solid || trace.fraction < 1.0 {
                        return Some(Action::capture());
                    }
                }
                camera.stage_grab = Some(StageGrab {
                    distance,
                    offset: stage.transform.position - (eye + direction * distance),
                    direction,
                });
                Some(Action::publish(Message::StageMoveStarted).and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if camera.stage_grab.is_some() => {
                let point = cursor.position()?;
                let (eye, direction) = cursor_ray(camera, bounds, point)?;
                let grab = camera.stage_grab.as_mut()?;
                grab.direction = direction;
                Some(move_grab(stage.transform, grab, eye))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if camera.stage_grab.is_some() => {
                let eye = camera_eye(camera)?;
                let grab = camera.stage_grab.as_mut()?;
                let steps = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                grab.distance = (grab.distance * 1.15_f32.powf(steps)).clamp(8.0, 16384.0);
                Some(move_grab(stage.transform, grab, eye))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Middle)) => camera
                .stage_grab
                .take()
                .map(|_| Action::publish(Message::StageMoveFinished).and_capture()),
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyR),
                ..
            }) if camera.stage_grab.is_some() => {
                let mut transform = stage.transform;
                transform.angles[1] = (transform.angles[1] + 15.0).rem_euclid(360.0);
                Some(Action::publish(Message::StageTransformChanged(transform)).and_capture())
            }
            Event::Window(iced::window::Event::Unfocused) if camera.stage_grab.take().is_some() => {
                Some(Action::publish(Message::StageMoveFinished))
            }
            _ => None,
        }
    }
}

fn move_grab(mut transform: StageTransform, grab: &StageGrab, eye: Vec3) -> Action<Message> {
    transform.position = eye + grab.direction * grab.distance + grab.offset;
    Action::publish(Message::StageTransformChanged(transform)).and_capture()
}

fn camera_eye(camera: &FlyCamera) -> Option<Vec3> {
    Some(
        camera.position?
            + Vec3::new(
                0.0,
                0.0,
                camera.view_bob_offset() + camera.duck_view_offset(),
            ),
    )
}

fn cursor_ray(camera: &FlyCamera, bounds: Rectangle, point: Point) -> Option<(Vec3, Vec3)> {
    let eye = camera_eye(camera)?;
    let forward = camera.forward();
    let right = forward.cross(SOURCE_UP).normalize()?;
    let up = right.cross(forward);
    let tan = (super::super::FOV_Y * 0.5).tan();
    let x = ((point.x - bounds.x) / bounds.width.max(1.0) * 2.0 - 1.0)
        * (bounds.width / bounds.height.max(1.0))
        * tan;
    let y = (1.0 - (point.y - bounds.y) / bounds.height.max(1.0) * 2.0) * tan;
    Some((eye, (forward + right * x + up * y).normalize()?))
}

fn ray_box_distance(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut enter = 0.0_f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() < 1e-6 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
        } else {
            let a = (min[axis] - origin[axis]) / direction[axis];
            let b = (max[axis] - origin[axis]) / direction[axis];
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
            if enter > exit {
                return None;
            }
        }
    }
    (exit >= 0.0).then_some(enter)
}

fn support_distance(min: Vec3, max: Vec3, angles: Vec3, normal: Vec3) -> f32 {
    let mut support = f32::INFINITY;
    for x in [min[0], max[0]] {
        for y in [min[1], max[1]] {
            for z in [min[2], max[2]] {
                support = support.min(Vec3::new(x, y, z).rotate_source(angles).dot(normal));
            }
        }
    }
    support
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn middle_click_grabs_posed_off_origin_geometry_and_ignores_empty_choices() {
        use crate::features::file_preview::viewer3d::pipeline::StagePrimitive;
        use crate::features::file_preview::viewer3d::test_support::empty_preview;
        use gmpublished_backend::model_decompile::PreviewPose;
        let model = crate::model_animation_test_support::off_origin_model();
        let pose = PreviewPose {
            sequence: 0,
            cycle: 0.0,
            parameters: vec![],
        };
        let local = model.visible_bounds(Some(&pose), &[]).unwrap();
        let mut viewer = FlyViewer {
            scene: std::sync::Arc::new(empty_preview(Vec3::splat(-1024.0), Vec3::splat(1024.0))),
            content_id: 1,
            fog: None,
            fog_enabled: false,
            sky_camera: None,
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: false,
            spawn: None,
            pose: None,
            movement_mode: None,
            requested_movement_mode: None,
            stage: Some(StagePrimitive {
                model: std::sync::Arc::clone(&model),
                animation_pose: Some(pose),
                local_bounds: Some(local),
                content_id: 2,
                transform: StageTransform::default(),
                skin_remap: vec![],
                bodygroup_choices: vec![],
                phy_debug_visible: false,
            }),
        };
        let mut camera = FlyCamera {
            position: Some(Vec3::new(0.0, 0.0, 10.0)),
            yaw: std::f32::consts::FRAC_PI_2,
            ..FlyCamera::default()
        };
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(200.0, 200.0));
        let event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle));
        let cursor = mouse::Cursor::Available(Point::new(100.0, 100.0));
        assert!(
            ray_box_distance(
                Vec3::new(0.0, 0.0, 10.0),
                Vec3::new(0.0, 1.0, 0.0),
                model.scene.bounds_min,
                model.scene.bounds_max
            )
            .is_none()
        );
        assert!(
            viewer
                .stage_interaction(&mut camera, &event, bounds, cursor)
                .is_some()
        );
        assert!((camera.stage_grab.as_ref().unwrap().distance - 98.0).abs() < 0.005);
        assert!(
            (support_distance(local.min, local.max, Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)) - 98.0)
                .abs()
                < 0.005
        );
        camera.stage_grab = None;
        viewer.stage.as_mut().unwrap().local_bounds = None;
        assert!(
            viewer
                .stage_interaction(&mut camera, &event, bounds, cursor)
                .is_none()
        );
        assert!(camera.stage_grab.is_none());
    }

    #[test]
    fn held_prop_follows_camera_and_uses_rendered_eye_height() {
        use crate::features::file_preview::viewer3d::pipeline::StagePrimitive;
        use crate::features::file_preview::viewer3d::test_support::{
            empty_model_preview, empty_preview,
        };
        let viewer = FlyViewer {
            scene: std::sync::Arc::new(empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0))),
            content_id: 1,
            fog: None,
            fog_enabled: false,
            sky_camera: None,
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: false,
            spawn: None,
            pose: None,
            movement_mode: None,
            requested_movement_mode: None,
            stage: Some(StagePrimitive {
                local_bounds: None,
                animation_pose: None,
                model: std::sync::Arc::new(empty_model_preview(
                    Vec3::splat(-1.0),
                    Vec3::splat(1.0),
                )),
                content_id: 2,
                transform: StageTransform::default(),
                skin_remap: vec![],
                bodygroup_choices: vec![],
                phy_debug_visible: false,
            }),
        };
        let mut camera = FlyCamera {
            position: Some(Vec3::new(20.0, 30.0, 40.0)),
            mode: super::super::MovementMode::Walk,
            walk_bob_offset: 3.0,
            stage_grab: Some(StageGrab {
                distance: 10.0,
                offset: Vec3::new(0.0, 2.0, 0.0),
                direction: Vec3::new(1.0, 0.0, 0.0),
            }),
            ..FlyCamera::default()
        };
        let Some(Message::StageCameraChanged { transform, .. }) =
            viewer.camera_message(&camera, Vec::new())
        else {
            panic!("expected stage camera update");
        };
        assert_eq!(transform.position, Vec3::new(30.0, 32.0, 43.0));
        camera.position = Some(Vec3::new(21.0, 32.0, 43.0));
        let Some(Message::StageCameraChanged { transform, .. }) =
            viewer.camera_message(&camera, Vec::new())
        else {
            panic!("expected stage camera update");
        };
        assert_eq!(transform.position, Vec3::new(31.0, 34.0, 46.0));
    }

    #[test]
    fn ray_hit_handles_parallel_axes_and_rejects_behind_camera() {
        let min = Vec3::splat(-1.0);
        let max = Vec3::splat(1.0);
        assert_eq!(
            ray_box_distance(
                Vec3::new(-4.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                min,
                max
            ),
            Some(3.0)
        );
        assert_eq!(
            ray_box_distance(
                Vec3::new(-4.0, 2.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                min,
                max
            ),
            None
        );
        assert_eq!(
            ray_box_distance(Vec3::new(4.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), min, max),
            None
        );
    }
    #[test]
    fn rotated_support_places_bottom_on_surface() {
        assert!(
            (support_distance(
                Vec3::new(-2.0, -1.0, -3.0),
                Vec3::new(2.0, 1.0, 3.0),
                Vec3::new(90.0, 0.0, 0.0),
                SOURCE_UP
            ) + 2.0)
                .abs()
                < 1e-5
        );
    }
}
