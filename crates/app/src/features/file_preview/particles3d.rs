//! Custom wgpu pipeline rendering a Source particle system inside the
//! preview modal. The simulation clock rides the redraw chain: each playing
//! frame requests the next, so pausing or closing stops the loop dead (0%
//! idle). Camera state and the sim engine live in the widget tree; the orbit
//! pose and control point positions round-trip through feature state so they
//! survive expand/collapse rebuilds.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Action, Viewport};
use iced::{Event, Point, Rectangle};

use gmpublished_backend::particles::{MAX_CONTROL_POINTS, ParticleEngine, RendererKind};

use super::Message;
use super::model::{ParticlePreview, normalize_particle_material};
use super::state::OrbitPose;
use super::viewer3d::{look_at, mat_mul, perspective};
use crate::bridge::materials::ResolvedTexture;

const SHADER_SOURCE: &str = include_str!("particles.wgsl");
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const ORBIT_SENSITIVITY: f32 = 0.008;
const ZOOM_STEP: f32 = 0.9;
const MIN_PITCH: f32 = -1.55;
const MAX_PITCH: f32 = 1.55;
/// Fallback frame delta when a drag needs a velocity hint.
const DRAG_DT_HINT: f32 = 1.0 / 60.0;
/// Gizmo colors cycle for control points 1..; CP0 is pinned and undrawn.
const GIZMO_COLORS: [[f32; 3]; 7] = [
    [1.0, 0.62, 0.11],
    [0.20, 0.78, 1.0],
    [0.86, 0.39, 1.0],
    [0.35, 1.0, 0.55],
    [1.0, 0.35, 0.35],
    [1.0, 0.95, 0.4],
    [0.55, 0.55, 1.0],
];

pub(super) struct ParticleViewer {
    pub(super) preview: Arc<ParticlePreview>,
    /// Identifies uploads in the shared pipeline cache; bump per load.
    pub(super) content_id: u64,
    pub(super) system_index: usize,
    pub(super) playing: bool,
    pub(super) speed: f32,
    /// Bumped by the restart button; the widget replays from t=0.
    pub(super) restart_epoch: u64,
    pub(super) pose: Option<OrbitPose>,
    pub(super) control_points: [[f32; 3]; MAX_CONTROL_POINTS],
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Orbit(Point),
    /// Dragging a control point gizmo in the camera-facing plane through
    /// its position at grab time.
    Gizmo {
        index: usize,
    },
}

pub(super) struct SimState {
    key: Option<(u64, usize)>,
    yaw: f32,
    pitch: f32,
    distance: f32,
    drag: Option<Drag>,
    engine: Option<ParticleEngine>,
    last_frame: Option<Instant>,
    restart_epoch: u64,
}

impl Default for SimState {
    fn default() -> Self {
        let pose = OrbitPose::default();
        Self {
            key: None,
            yaw: pose.yaw,
            pitch: pose.pitch,
            distance: pose.distance,
            drag: None,
            engine: None,
            last_frame: None,
            restart_epoch: 0,
        }
    }
}

impl SimState {
    fn ensure_spawn(&mut self, viewer: &ParticleViewer) {
        let key = (viewer.content_id, viewer.system_index);
        if self.key != Some(key) {
            self.key = Some(key);
            self.engine = ParticleEngine::new(
                &viewer.preview.file,
                viewer.system_index,
                // Content-derived seed keeps replays identical per file.
                viewer.content_id ^ (viewer.system_index as u64).wrapping_mul(0x9E37),
            );
            let pose = viewer.pose.unwrap_or_default();
            self.yaw = pose.yaw;
            self.pitch = pose.pitch;
            self.distance = pose.distance;
            self.drag = None;
            self.last_frame = None;
            self.restart_epoch = viewer.restart_epoch;
            if let Some(engine) = self.engine.as_mut() {
                for (index, position) in viewer.control_points.iter().enumerate() {
                    engine.set_control_point(index, *position);
                }
            }
            return;
        }
        if self.restart_epoch != viewer.restart_epoch {
            self.restart_epoch = viewer.restart_epoch;
            if let Some(engine) = self.engine.as_mut() {
                engine.restart();
            }
            self.last_frame = None;
        }
        // Feature state owns control points; sync any that moved (e.g. the
        // widget tree was rebuilt, or a future reset control), except the
        // one currently being dragged, which the drag handler feeds.
        let dragging = match self.drag {
            Some(Drag::Gizmo { index }) => Some(index),
            _ => None,
        };
        if let Some(engine) = self.engine.as_mut() {
            for (index, position) in viewer.control_points.iter().enumerate() {
                if Some(index) == dragging {
                    continue;
                }
                let current = engine.control_point(index);
                let delta = [
                    position[0] - current[0],
                    position[1] - current[1],
                    position[2] - current[2],
                ];
                if delta.iter().any(|component| component.abs() > 1e-3) {
                    engine.drag_control_point(index, *position, DRAG_DT_HINT);
                }
            }
        }
    }

    fn pose(&self) -> OrbitPose {
        OrbitPose {
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
        }
    }
}

// --- Camera --------------------------------------------------------------

struct CameraFrame {
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    forward: [f32; 3],
    view_proj: [[f32; 4]; 4],
    aspect: f32,
}

fn v_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn v_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn v_scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn v_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn v_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn v_length(a: [f32; 3]) -> f32 {
    v_dot(a, a).sqrt()
}

fn v_normalize(a: [f32; 3]) -> [f32; 3] {
    let len = v_length(a).max(1e-6);
    v_scale(a, 1.0 / len)
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn camera_frame(state: &SimState, bounds: Rectangle) -> CameraFrame {
    let radius = state
        .engine
        .as_ref()
        .map_or(64.0, ParticleEngine::bounding_radius);
    let center = [0.0, 0.0, 0.0];
    let distance = radius * 2.2 * state.distance;
    let eye = [
        center[0] + distance * state.pitch.cos() * state.yaw.sin(),
        center[1] + distance * state.pitch.cos() * state.yaw.cos(),
        center[2] + distance * state.pitch.sin(),
    ];
    // Source is Z-up.
    let forward = v_normalize(v_sub(center, eye));
    let right = v_normalize(v_cross(forward, [0.0, 0.0, 1.0]));
    let up = v_cross(right, forward);
    let view = look_at(eye, center, [0.0, 0.0, 1.0]);
    let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
    let proj = perspective(FOV_Y, aspect, radius * 0.01, radius * 20.0 + distance);
    CameraFrame {
        eye,
        right,
        up,
        forward,
        view_proj: mat_mul(proj, view),
        aspect,
    }
}

/// World-space ray through a cursor position inside `bounds`.
fn cursor_ray(frame: &CameraFrame, bounds: Rectangle, cursor: Point) -> [f32; 3] {
    let ndc_x = ((cursor.x - bounds.x) / bounds.width.max(1.0)) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((cursor.y - bounds.y) / bounds.height.max(1.0)) * 2.0;
    let tan_half = (FOV_Y * 0.5).tan();
    v_normalize(v_add(
        frame.forward,
        v_add(
            v_scale(frame.right, ndc_x * tan_half * frame.aspect),
            v_scale(frame.up, ndc_y * tan_half),
        ),
    ))
}

/// Intersects a cursor ray with the camera-facing plane through `anchor`.
fn drag_plane_position(frame: &CameraFrame, ray: [f32; 3], anchor: [f32; 3]) -> Option<[f32; 3]> {
    let denominator = v_dot(ray, frame.forward);
    if denominator.abs() < 1e-4 {
        return None;
    }
    let t = v_dot(v_sub(anchor, frame.eye), frame.forward) / denominator;
    (t > 0.0).then(|| v_add(frame.eye, v_scale(ray, t)))
}

fn gizmo_pick_radius(frame: &CameraFrame, position: [f32; 3]) -> f32 {
    v_length(v_sub(position, frame.eye)) * 0.035
}

impl ParticleViewer {
    /// Draggable control points: 1..=highest referenced. CP0 stays pinned at
    /// the viewport centre so the effect cannot be dragged off-origin.
    fn gizmo_indices(&self, state: &SimState) -> std::ops::RangeInclusive<usize> {
        let highest = state
            .engine
            .as_ref()
            .map_or(0, ParticleEngine::highest_control_point);
        1..=highest.min(MAX_CONTROL_POINTS - 1)
    }

    fn pick_gizmo(&self, state: &SimState, frame: &CameraFrame, ray: [f32; 3]) -> Option<usize> {
        let engine = state.engine.as_ref()?;
        let mut best: Option<(usize, f32)> = None;
        for index in self.gizmo_indices(state) {
            let position = engine.control_point(index);
            let to_point = v_sub(position, frame.eye);
            let along = v_dot(to_point, ray);
            if along <= 0.0 {
                continue;
            }
            let closest = v_add(frame.eye, v_scale(ray, along));
            let miss = v_length(v_sub(position, closest));
            if miss <= gizmo_pick_radius(frame, position)
                && best.is_none_or(|(_, distance)| along < distance)
            {
                best = Some((index, along));
            }
        }
        best.map(|(index, _)| index)
    }
}

impl shader::Program<Message> for ParticleViewer {
    type State = SimState;
    type Primitive = ParticlePrimitive;

    fn update(
        &self,
        state: &mut SimState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        state.ensure_spawn(self);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_over(bounds)?;
                let frame = camera_frame(state, bounds);
                let ray = cursor_ray(&frame, bounds, position);
                state.drag = Some(
                    self.pick_gizmo(state, &frame, ray)
                        .map_or(Drag::Orbit(position), |index| Drag::Gizmo { index }),
                );
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => match state.drag? {
                Drag::Orbit(from) => {
                    let to = cursor.position()?;
                    state.yaw += (to.x - from.x) * ORBIT_SENSITIVITY;
                    state.pitch = (state.pitch + (to.y - from.y) * ORBIT_SENSITIVITY)
                        .clamp(MIN_PITCH, MAX_PITCH);
                    state.drag = Some(Drag::Orbit(to));
                    Some(Action::publish(Message::OrbitPoseChanged(state.pose())).and_capture())
                }
                Drag::Gizmo { index } => {
                    let to = cursor.position()?;
                    let frame = camera_frame(state, bounds);
                    let ray = cursor_ray(&frame, bounds, to);
                    let engine = state.engine.as_mut()?;
                    let anchor = engine.control_point(index);
                    let position = drag_plane_position(&frame, ray, anchor)?;
                    engine.drag_control_point(index, position, DRAG_DT_HINT);
                    Some(
                        Action::publish(Message::ParticleControlPointChanged { index, position })
                            .and_capture(),
                    )
                }
            },
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.drag.take().map(|_| Action::capture())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_over(bounds)?;
                let steps = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                state.distance = (state.distance * ZOOM_STEP.powf(steps)).clamp(0.05, 8.0);
                Some(Action::publish(Message::OrbitPoseChanged(state.pose())).and_capture())
            }
            Event::Window(iced::window::Event::RedrawRequested(now)) => {
                if !self.playing {
                    state.last_frame = None;
                    return None;
                }
                if let Some(last) = state.last_frame {
                    let dt = now.saturating_duration_since(last).as_secs_f32().min(0.1);
                    if let Some(engine) = state.engine.as_mut() {
                        engine.step(dt * self.speed.clamp(0.05, 10.0));
                        if engine.finished() {
                            engine.restart();
                        }
                    }
                }
                state.last_frame = Some(*now);
                // Each playing frame requests the next; pausing breaks the
                // chain and the widget goes fully idle.
                Some(Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &SimState,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> ParticlePrimitive {
        let frame = camera_frame(state, bounds);
        let mut instances: Vec<GpuInstance> = Vec::new();
        let mut batches: Vec<DrawBatch> = Vec::new();

        if let Some(engine) = state.engine.as_ref() {
            let mut translucent: Vec<(Option<usize>, Vec<GpuInstance>)> = Vec::new();
            let mut additive: Vec<(Option<usize>, Vec<GpuInstance>)> = Vec::new();
            for render in engine.render_instances() {
                if render.particles.is_empty() {
                    continue;
                }
                let material_key = normalize_particle_material(render.system.material());
                let slot = self
                    .preview
                    .materials
                    .iter()
                    .position(|slot| slot.name == material_key);
                let is_additive = slot.is_some_and(|slot| self.preview.materials[slot].additive);
                let sheet = slot.and_then(|slot| self.preview.materials[slot].sheet.as_deref());
                let mut list = build_instances(&render, &frame, sheet);
                if !is_additive {
                    // Painter's order: no depth buffer, so translucents sort
                    // back-to-front along the view direction.
                    list.sort_by(|a, b| {
                        let da = v_dot(v_sub(a.position(), frame.eye), frame.forward);
                        let db = v_dot(v_sub(b.position(), frame.eye), frame.forward);
                        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    translucent.push((slot, list));
                } else {
                    additive.push((slot, list));
                }
            }
            for (slot, list) in translucent {
                push_batch(&mut instances, &mut batches, slot, false, list);
            }
            for (slot, list) in additive {
                push_batch(&mut instances, &mut batches, slot, true, list);
            }

            // Gizmos last, on top of the effect.
            let mut gizmos = Vec::new();
            for index in self.gizmo_indices(state) {
                let position = engine.control_point(index);
                let color = GIZMO_COLORS[(index - 1) % GIZMO_COLORS.len()];
                let size = gizmo_pick_radius(&frame, position) * 0.5;
                gizmos.push(GpuInstance {
                    position_rotation: [
                        position[0],
                        position[1],
                        position[2],
                        std::f32::consts::FRAC_PI_4,
                    ],
                    axis_mode: [0.0, 0.0, 0.0, 0.0],
                    color: [color[0], color[1], color[2], 0.9],
                    size: [size, size, 0.0, 0.0],
                    uv_rect: FULL_UV_RECT,
                });
            }
            if !gizmos.is_empty() {
                push_batch(&mut instances, &mut batches, None, false, gizmos);
            }
        }

        ParticlePrimitive {
            preview: Arc::clone(&self.preview),
            content_id: self.content_id,
            uniforms: Uniforms {
                view_proj: frame.view_proj,
                camera_right: [frame.right[0], frame.right[1], frame.right[2], 0.0],
                camera_up: [frame.up[0], frame.up[1], frame.up[2], 0.0],
                camera_eye: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            },
            instances,
            batches,
        }
    }

    fn mouse_interaction(
        &self,
        state: &SimState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match state.drag {
            Some(Drag::Gizmo { .. }) => mouse::Interaction::Move,
            Some(Drag::Orbit(_)) => mouse::Interaction::Grabbing,
            None if cursor.position_over(bounds).is_some() => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }
}

fn build_instances(
    render: &gmpublished_backend::particles::InstanceRender<'_>,
    frame: &CameraFrame,
    sheet: Option<&vformats::vtf::SpriteSheet>,
) -> Vec<GpuInstance> {
    let renderer = render.system.renderer();
    let mut list = Vec::with_capacity(render.particles.len());
    match renderer.kind {
        RendererKind::AnimatedSprites => {
            for particle in &render.particles {
                list.push(GpuInstance::billboard(
                    particle,
                    sheet_uv_rect(sheet, renderer, particle),
                ));
            }
        }
        RendererKind::SpriteTrail => {
            for particle in &render.particles {
                let speed = v_length(particle.velocity);
                let length = (speed * particle.trail_length)
                    .clamp(renderer.trail_min_length, renderer.trail_max_length)
                    .max(particle.radius);
                let axis = if speed > 1e-3 {
                    v_normalize(particle.velocity)
                } else {
                    frame.up
                };
                let half_length = length * 0.5;
                let center = v_sub(particle.position, v_scale(axis, half_length));
                list.push(GpuInstance {
                    position_rotation: [center[0], center[1], center[2], 0.0],
                    axis_mode: [axis[0], axis[1], axis[2], 1.0],
                    color: GpuInstance::linear_color(particle),
                    size: [particle.radius, half_length, 0.0, 0.0],
                    uv_rect: sheet_uv_rect(sheet, renderer, particle),
                });
            }
        }
        RendererKind::Rope => {
            let mut ordered: Vec<_> = render.particles.iter().collect();
            ordered.sort_by_key(|particle| particle.spawn_index);
            for pair in ordered.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let span = v_sub(b.position, a.position);
                let length = v_length(span);
                if length < 1e-3 {
                    continue;
                }
                let center = v_scale(v_add(a.position, b.position), 0.5);
                let axis = v_scale(span, 1.0 / length);
                let mut color = GpuInstance::linear_color(a);
                let color_b = GpuInstance::linear_color(b);
                for (target, other) in color.iter_mut().zip(color_b) {
                    *target = (*target + other) * 0.5;
                }
                list.push(GpuInstance {
                    position_rotation: [center[0], center[1], center[2], 0.0],
                    axis_mode: [axis[0], axis[1], axis[2], 1.0],
                    color,
                    size: [(a.radius + b.radius) * 0.5, length * 0.5, 0.0, 0.0],
                    uv_rect: FULL_UV_RECT,
                });
            }
        }
    }
    list
}

fn push_batch(
    instances: &mut Vec<GpuInstance>,
    batches: &mut Vec<DrawBatch>,
    texture_slot: Option<usize>,
    additive: bool,
    list: Vec<GpuInstance>,
) {
    if list.is_empty() {
        return;
    }
    let start = instances.len() as u32;
    instances.extend(list);
    batches.push(DrawBatch {
        texture_slot,
        additive,
        range: start..instances.len() as u32,
    });
}

// --- Primitive & pipeline --------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    camera_eye: [f32; 4],
}

/// Full-texture UV rect as offset + scale.
const FULL_UV_RECT: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuInstance {
    position_rotation: [f32; 4],
    axis_mode: [f32; 4],
    color: [f32; 4],
    size: [f32; 4],
    /// `[u_offset, v_offset, u_scale, v_scale]` into the sprite atlas.
    uv_rect: [f32; 4],
}

impl GpuInstance {
    fn position(&self) -> [f32; 3] {
        [
            self.position_rotation[0],
            self.position_rotation[1],
            self.position_rotation[2],
        ]
    }

    fn linear_color(particle: &gmpublished_backend::particles::RenderParticle) -> [f32; 4] {
        [
            srgb_to_linear(particle.color[0]),
            srgb_to_linear(particle.color[1]),
            srgb_to_linear(particle.color[2]),
            particle.alpha,
        ]
    }

    fn billboard(
        particle: &gmpublished_backend::particles::RenderParticle,
        uv_rect: [f32; 4],
    ) -> Self {
        Self {
            position_rotation: [
                particle.position[0],
                particle.position[1],
                particle.position[2],
                particle.rotation,
            ],
            axis_mode: [0.0, 0.0, 0.0, 0.0],
            color: Self::linear_color(particle),
            size: [
                particle.radius,
                particle.radius,
                f32::from(u8::from(particle.mirrored)),
                0.0,
            ],
            uv_rect,
        }
    }
}

/// Atlas rect (as offset+scale) for a particle's current animation frame.
fn sheet_uv_rect(
    sheet: Option<&vformats::vtf::SpriteSheet>,
    renderer: &gmpublished_backend::particles::RendererInfo,
    particle: &gmpublished_backend::particles::RenderParticle,
) -> [f32; 4] {
    let Some(sequence) = sheet.and_then(|sheet| sheet.sequence(particle.sequence)) else {
        return FULL_UV_RECT;
    };
    let time = if renderer.animation_fit_lifetime {
        (particle.age / particle.lifetime.max(1e-6)) * sequence.total_time
    } else if renderer.animation_rate_is_fps {
        let frames = sequence.frames.len().max(1) as f32;
        (particle.age * renderer.animation_rate / frames) * sequence.total_time
    } else {
        particle.age * renderer.animation_rate
    };
    let uv = sequence.uv_at(time);
    [uv[0], uv[1], uv[2] - uv[0], uv[3] - uv[1]]
}

#[derive(Debug, Clone)]
struct DrawBatch {
    /// Index into [`ParticlePreview::materials`]; `None` = white fallback.
    texture_slot: Option<usize>,
    additive: bool,
    range: std::ops::Range<u32>,
}

#[derive(Debug)]
pub(super) struct ParticlePrimitive {
    preview: Arc<ParticlePreview>,
    content_id: u64,
    uniforms: Uniforms,
    instances: Vec<GpuInstance>,
    batches: Vec<DrawBatch>,
}

impl shader::Primitive for ParticlePrimitive {
    type Pipeline = ParticlePipeline;

    fn prepare(
        &self,
        pipeline: &mut ParticlePipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        pipeline.touch(self.content_id);
        for batch in &self.batches {
            if let Some(slot) = batch.texture_slot {
                pipeline.ensure_texture(device, queue, self.content_id, slot, &self.preview);
            }
        }
        pipeline.write_instances(device, queue, &self.instances);
        queue.write_buffer(
            &pipeline.uniform_buffer,
            0,
            bytemuck::bytes_of(&self.uniforms),
        );
        pipeline.draw_batches.clone_from(&self.batches);
        pipeline.draw_content_id = self.content_id;
    }

    fn draw(&self, pipeline: &ParticlePipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(instance_buffer) = pipeline.instance_buffer.as_ref() else {
            return true;
        };
        render_pass.set_bind_group(0, &pipeline.uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, instance_buffer.slice(..));
        for batch in &pipeline.draw_batches {
            if batch.range.is_empty() {
                continue;
            }
            let texture_bind_group = batch
                .texture_slot
                .and_then(|slot| pipeline.textures.get(&(pipeline.draw_content_id, slot)))
                .map_or(&pipeline.white_bind_group, |entry| &entry.bind_group);
            render_pass.set_pipeline(if batch.additive {
                &pipeline.additive
            } else {
                &pipeline.translucent
            });
            render_pass.set_bind_group(1, texture_bind_group, &[]);
            render_pass.draw(0..6, batch.range.clone());
        }
        true
    }
}

#[derive(Debug)]
struct TextureEntry {
    bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
pub(super) struct ParticlePipeline {
    translucent: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white_bind_group: wgpu::BindGroup,
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: usize,
    textures: HashMap<(u64, usize), TextureEntry>,
    /// Content ids drawn since the last trim; others are evicted.
    live: Vec<u64>,
    draw_batches: Vec<DrawBatch>,
    draw_content_id: u64,
}

impl ParticlePipeline {
    fn touch(&mut self, content_id: u64) {
        if !self.live.contains(&content_id) {
            self.live.push(content_id);
        }
    }

    fn write_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[GpuInstance],
    ) {
        if instances.is_empty() {
            return;
        }
        let needed = instances.len();
        if self.instance_buffer.is_none() || self.instance_capacity < needed {
            let capacity = needed.next_power_of_two().max(256);
            self.instance_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("particle preview instances"),
                size: (capacity * std::mem::size_of::<GpuInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.instance_capacity = capacity;
        }
        if let Some(buffer) = self.instance_buffer.as_ref() {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        content_id: u64,
        slot: usize,
        preview: &ParticlePreview,
    ) {
        if self.textures.contains_key(&(content_id, slot)) {
            return;
        }
        let Some(texture) = preview
            .materials
            .get(slot)
            .and_then(|material| material.texture.as_ref())
        else {
            return;
        };
        let Some(view) = create_rgba_texture_view(device, queue, texture) else {
            return;
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle preview material"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.textures
            .insert((content_id, slot), TextureEntry { bind_group });
    }
}

/// Uploads the RGBA mip chain of a resolved texture. BC payloads never reach
/// here: the particle loader resolves with BC disabled.
fn create_rgba_texture_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &ResolvedTexture,
) -> Option<wgpu::TextureView> {
    let levels: Vec<_> = texture.mip_chain().collect();
    let first = levels.first()?;
    if first.width == 0 || first.height == 0 {
        return None;
    }
    let valid_levels = levels
        .iter()
        .take_while(|level| {
            level.width > 0
                && level.height > 0
                && level.rgba.len() == (level.width * level.height * 4) as usize
        })
        .count()
        .max(1);
    let gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("particle preview sprite"),
        size: wgpu::Extent3d {
            width: first.width,
            height: first.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: valid_levels as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, mip) in levels.iter().take(valid_levels).enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu_texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            mip.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mip.width * 4),
                rows_per_image: Some(mip.height),
            },
            wgpu::Extent3d {
                width: mip.width,
                height: mip.height,
                depth_or_array_layers: 1,
            },
        );
    }
    Some(gpu_texture.create_view(&wgpu::TextureViewDescriptor::default()))
}

impl shader::Pipeline for ParticlePipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle preview shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particle preview uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particle preview material layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particle preview pipeline layout"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });

        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x4,
                1 => Float32x4,
                2 => Float32x4,
                3 => Float32x4,
                4 => Float32x4,
            ],
        };

        let create_pipeline = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader_module,
                    entry_point: Some("vs_main"),
                    buffers: std::slice::from_ref(&instance_layout),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        // Source sprite blending: translucent = classic alpha, additive =
        // src_alpha additive (`$additive 1`).
        let translucent = create_pipeline(
            "particle preview translucent",
            wgpu::BlendState::ALPHA_BLENDING,
        );
        let additive = create_pipeline(
            "particle preview additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
        );

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle preview uniform buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle preview uniform bind group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("particle preview sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        // 1x1 white: gizmos and unresolved materials.
        let white_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("particle preview white"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_view = white_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let white_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle preview white bind group"),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&white_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            translucent,
            additive,
            uniform_buffer,
            uniform_bind_group,
            texture_layout,
            sampler,
            white_bind_group,
            instance_buffer: None,
            instance_capacity: 0,
            textures: HashMap::new(),
            live: Vec::new(),
            draw_batches: Vec::new(),
            draw_content_id: 0,
        }
    }

    fn trim(&mut self) {
        if self.live.is_empty() {
            // Nothing drawn this frame: the preview closed; free everything.
            self.textures.clear();
            self.instance_buffer = None;
            self.instance_capacity = 0;
            self.draw_batches.clear();
            return;
        }
        let live = std::mem::take(&mut self.live);
        self.textures
            .retain(|(content_id, _), _| live.contains(content_id));
    }
}
