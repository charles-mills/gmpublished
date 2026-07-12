//! Source engine particle system simulation.
//!
//! Compiles [`crate::scene::pcf`] definitions into typed operators
//! and steps them on the CPU. Coverage is honest: every function name in the
//! file is classified as fully simulated, approximated, inert-in-preview
//! (world collision / lighting that has no meaning without a map), or
//! unsupported, so a preview can say exactly what it is and is not showing.
//!
//! Conventions follow Source: Z-up world, distances in hammer units, colors
//! as sRGB bytes in the file (kept as 0..1 sRGB floats here), rotation in
//! radians internally (degrees in the file), lifetimes in seconds. Operators
//! that derive a value from age recompute it from the spawn-time initial
//! value each step, so operator order cannot accumulate drift.

use crate::scene::pcf::{PcfAttributes, PcfFile, PcfFunction, PcfSystem};

mod compiler;

pub use compiler::{CompiledSystem, RendererInfo, RendererKind};
use compiler::{Emitter, Force, Initializer, Operator, ScalarField, VectorField, compile_system};

pub const MAX_CONTROL_POINTS: usize = 8;
/// Hard ceiling across all instances so a hostile file cannot OOM the app.
pub const MAX_TOTAL_PARTICLES: usize = 100_000;

// --- Coverage ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    /// Simulated with Source-equivalent math.
    Full,
    /// Simulated, but with simplified math; the look may differ.
    Approximate,
    /// Meaningless without a map/entity context; deliberately inert.
    PreviewInert,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageEntry {
    pub function: String,
    /// Which operator list the function came from ("emitters", ...).
    pub list: &'static str,
    pub level: SupportLevel,
}

// --- Deterministic RNG ---------------------------------------------------

/// PCG32; deterministic so a restarted preview replays identically.
#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        let mut rng = Self {
            state: seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        };
        rng.next_u32();
        rng
    }

    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    fn range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.unit()
    }

    /// Source's exponent-biased random: `min + (max-min) * unit^exponent`.
    fn range_exp(&mut self, min: f32, max: f32, exponent: f32) -> f32 {
        let t = if exponent == 1.0 {
            self.unit()
        } else {
            self.unit().powf(exponent.max(1e-6))
        };
        min + (max - min) * t
    }

    fn range_vec(&mut self, min: [f32; 3], max: [f32; 3]) -> [f32; 3] {
        [
            self.range(min[0], max[0]),
            self.range(min[1], max[1]),
            self.range(min[2], max[2]),
        ]
    }

    fn range_int(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }
        min + (self.next_u32() % ((max - min + 1) as u32)) as i32
    }

    /// Uniform direction, componentwise-scaled by `bias` then renormalized.
    fn biased_unit_vector(&mut self, bias: [f32; 3]) -> [f32; 3] {
        for _ in 0..16 {
            let v = [
                self.range(-1.0, 1.0) * bias[0],
                self.range(-1.0, 1.0) * bias[1],
                self.range(-1.0, 1.0) * bias[2],
            ];
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if len > 1e-4 && len <= 1.0 {
                return [v[0] / len, v[1] / len, v[2] / len];
            }
        }
        [0.0, 0.0, 1.0]
    }
}

/// Cheap value noise in [-1, 1]; smooth in its argument. Stands in for
/// Source's Perlin-style curl noise where only "coherent wobble" matters.
fn value_noise(x: f32, y: f32, z: f32, w: f32) -> f32 {
    fn hash(mut n: u32) -> f32 {
        n = (n ^ 61) ^ (n >> 16);
        n = n.wrapping_mul(9);
        n ^= n >> 4;
        n = n.wrapping_mul(0x27d4_eb2d);
        n ^= n >> 15;
        (n & 0xffff) as f32 / 32767.5 - 1.0
    }
    fn smooth(t: f32) -> f32 {
        t * t * (3.0 - 2.0 * t)
    }
    let cell = |ix: i32, iy: i32, iz: i32, iw: i32| {
        hash(
            (ix as u32)
                .wrapping_mul(73856093)
                .wrapping_add((iy as u32).wrapping_mul(19349663))
                .wrapping_add((iz as u32).wrapping_mul(83492791))
                .wrapping_add((iw as u32).wrapping_mul(2654435761)),
        )
    };
    let (fx, fy, fz, fw) = (x.floor(), y.floor(), z.floor(), w.floor());
    let (ix, iy, iz, iw) = (fx as i32, fy as i32, fz as i32, fw as i32);
    let (tx, ty, tz, tw) = (
        smooth(x - fx),
        smooth(y - fy),
        smooth(z - fz),
        smooth(w - fw),
    );
    // Bilinear over (x, w) at the two (y, z) corners kept nearest; a full
    // 4D lattice is overkill for a visual wobble source.
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let corner = |dx: i32, dw: i32| {
        let a = cell(ix + dx, iy, iz, iw + dw);
        let b = cell(ix + dx, iy + 1, iz + 1, iw + dw);
        lerp(a, b, (ty + tz) * 0.5)
    };
    lerp(
        lerp(corner(0, 0), corner(1, 0), tx),
        lerp(corner(0, 1), corner(1, 1), tx),
        tw,
    )
}

// --- Small vector helpers ------------------------------------------------

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn color_to_rgb(color: [u8; 4]) -> [f32; 3] {
    [
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
    ]
}

/// Source's SimpleSpline ease used by `ease_in_and_out` operator flags.
fn simple_spline(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Source's bias curve (0.5 = identity).
fn bias(t: f32, amount: f32) -> f32 {
    if (amount - 0.5).abs() < 1e-3 {
        return t;
    }
    t / ((1.0 / amount.clamp(1e-3, 1.0 - 1e-3) - 2.0) * (1.0 - t) + 1.0)
}

// --- Particle storage ------------------------------------------------------

/// Structure-of-arrays particle storage; swap-remove keeps steps O(live).
#[derive(Debug, Default, Clone)]
struct ParticleSet {
    position: Vec<[f32; 3]>,
    velocity: Vec<[f32; 3]>,
    /// System time at spawn, already shifted by any pre-age.
    creation_time: Vec<f32>,
    lifetime: Vec<f32>,
    radius_initial: Vec<f32>,
    radius: Vec<f32>,
    alpha_initial: Vec<f32>,
    alpha: Vec<f32>,
    color_initial: Vec<[f32; 3]>,
    color: Vec<[f32; 3]>,
    rotation: Vec<f32>,
    rotation_speed: Vec<f32>,
    sequence: Vec<i32>,
    trail_length: Vec<f32>,
    mirrored: Vec<bool>,
    spawn_index: Vec<u32>,
}

impl ParticleSet {
    fn len(&self) -> usize {
        self.position.len()
    }

    fn swap_remove(&mut self, index: usize) {
        self.position.swap_remove(index);
        self.velocity.swap_remove(index);
        self.creation_time.swap_remove(index);
        self.lifetime.swap_remove(index);
        self.radius_initial.swap_remove(index);
        self.radius.swap_remove(index);
        self.alpha_initial.swap_remove(index);
        self.alpha.swap_remove(index);
        self.color_initial.swap_remove(index);
        self.color.swap_remove(index);
        self.rotation.swap_remove(index);
        self.rotation_speed.swap_remove(index);
        self.sequence.swap_remove(index);
        self.trail_length.swap_remove(index);
        self.mirrored.swap_remove(index);
        self.spawn_index.swap_remove(index);
    }

    fn clear(&mut self) {
        self.position.clear();
        self.velocity.clear();
        self.creation_time.clear();
        self.lifetime.clear();
        self.radius_initial.clear();
        self.radius.clear();
        self.alpha_initial.clear();
        self.alpha.clear();
        self.color_initial.clear();
        self.color.clear();
        self.rotation.clear();
        self.rotation_speed.clear();
        self.sequence.clear();
        self.trail_length.clear();
        self.mirrored.clear();
        self.spawn_index.clear();
    }

    fn scalar_mut(&mut self, field: ScalarField, index: usize) -> &mut f32 {
        match field {
            ScalarField::LifeDuration => &mut self.lifetime[index],
            ScalarField::Radius => &mut self.radius[index],
            ScalarField::Rotation => &mut self.rotation[index],
            ScalarField::RotationSpeed => &mut self.rotation_speed[index],
            ScalarField::Alpha => &mut self.alpha[index],
            ScalarField::TrailLength => &mut self.trail_length[index],
        }
    }
}

// --- Engine ----------------------------------------------------------------

#[derive(Debug, Clone)]
struct Instance {
    system: usize,
    /// Engine time at which this instance starts simulating.
    start_time: f32,
    parent: Option<usize>,
    particles: ParticleSet,
    emit_accumulator: Vec<f32>,
    burst_done: Vec<bool>,
    spawn_counter: u32,
    rng: Rng,
}

/// Read-only view of one live particle for rendering.
#[derive(Debug, Clone, Copy)]
pub struct RenderParticle {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub radius: f32,
    /// Roll in radians.
    pub rotation: f32,
    /// sRGB color and opacity, 0..1.
    pub color: [f32; 3],
    pub alpha: f32,
    pub sequence: i32,
    pub trail_length: f32,
    pub mirrored: bool,
    /// Monotonic per-instance spawn order; storage order is shuffled by
    /// swap-removal, so path renderers must sort on this.
    pub spawn_index: u32,
    /// Seconds since spawn; drives sprite sheet animation.
    pub age: f32,
    pub lifetime: f32,
}

pub struct InstanceRender<'a> {
    pub system: &'a CompiledSystem,
    pub particles: Vec<RenderParticle>,
}

pub struct ParticleEngine {
    systems: Vec<CompiledSystem>,
    instances: Vec<Instance>,
    control_points: [[f32; 3]; MAX_CONTROL_POINTS],
    control_point_velocity: [[f32; 3]; MAX_CONTROL_POINTS],
    time: f32,
    seed: u64,
    emitters_alive: bool,
}

impl ParticleEngine {
    /// Compiles `root` plus its transitive children out of `file`. Returns
    /// `None` when the index is out of range.
    pub fn new(file: &PcfFile, root: usize, seed: u64) -> Option<Self> {
        if root >= file.systems.len() {
            return None;
        }
        // Collect the transitive closure of children, parent-first so child
        // instances can read parent particles during their own spawn.
        let mut include: Vec<usize> = Vec::new();
        let mut queue = vec![root];
        while let Some(index) = queue.pop() {
            if include.contains(&index) {
                continue;
            }
            include.push(index);
            for child in &file.systems[index].children {
                if let Some(child_index) = child.system_index
                    && child_index < file.systems.len()
                {
                    queue.push(child_index);
                }
            }
        }

        let mut system_indices = vec![None; file.systems.len()];
        for (compiled_index, &definition_index) in include.iter().enumerate() {
            system_indices[definition_index] = Some(compiled_index);
        }
        let systems: Vec<CompiledSystem> = include
            .iter()
            .map(|&index| compile_system(&file.systems[index], &system_indices))
            .collect();

        let mut engine = Self {
            systems,
            instances: Vec::new(),
            control_points: [[0.0; 3]; MAX_CONTROL_POINTS],
            control_point_velocity: [[0.0; 3]; MAX_CONTROL_POINTS],
            time: 0.0,
            seed,
            emitters_alive: true,
        };
        engine.spawn_instance_tree(0, 0.0, None);
        Some(engine)
    }

    fn spawn_instance_tree(&mut self, system: usize, start_time: f32, parent: Option<usize>) {
        // Duplicate systems in a cycle are cut off by depth: a child chain
        // deeper than the compiled system count must be recursive.
        if self.instances.len() >= self.systems.len() * 2 {
            return;
        }
        let compiled = &self.systems[system];
        let instance_index = self.instances.len();
        self.instances.push(Instance {
            system,
            start_time,
            parent,
            particles: ParticleSet::default(),
            emit_accumulator: vec![0.0; compiled.emitters.len()],
            burst_done: vec![false; compiled.emitters.len()],
            spawn_counter: 0,
            rng: Rng::new(
                self.seed
                    .wrapping_add(instance_index as u64)
                    .wrapping_mul(0x9E3779B97F4A7C15),
            ),
        });
        let children = self.systems[system].children.clone();
        for child in children {
            self.spawn_instance_tree(child.system, start_time + child.delay, Some(instance_index));
        }
    }

    pub fn systems(&self) -> &[CompiledSystem] {
        &self.systems
    }

    pub fn root_system(&self) -> &CompiledSystem {
        &self.systems[0]
    }

    pub fn time(&self) -> f32 {
        self.time
    }

    pub fn live_particles(&self) -> usize {
        self.instances
            .iter()
            .map(|instance| instance.particles.len())
            .sum()
    }

    /// World-space framing radius over the whole effect tree, including
    /// control point spread.
    pub fn bounding_radius(&self) -> f32 {
        let system_radius = self
            .systems
            .iter()
            .map(|system| system.bounding_radius)
            .fold(24.0_f32, f32::max);
        let control_point_reach = (0..=self.highest_control_point())
            .map(|index| length(self.control_points[index]))
            .fold(0.0_f32, f32::max);
        system_radius + control_point_reach
    }

    /// Highest control point index read by any compiled operator, i.e. how
    /// many gizmos are worth showing.
    pub fn highest_control_point(&self) -> usize {
        self.systems
            .iter()
            .map(|system| system.highest_control_point)
            .max()
            .unwrap_or(0)
    }

    pub fn control_point(&self, index: usize) -> [f32; 3] {
        self.control_points[index.min(MAX_CONTROL_POINTS - 1)]
    }

    pub fn set_control_point(&mut self, index: usize, position: [f32; 3]) {
        if index < MAX_CONTROL_POINTS {
            self.control_points[index] = position;
        }
    }

    /// True once every emitter has finished and no particles remain; the
    /// caller can restart to loop the effect.
    pub fn finished(&self) -> bool {
        !self.emitters_alive && self.live_particles() == 0
    }

    pub fn restart(&mut self) {
        self.time = 0.0;
        self.emitters_alive = true;
        for instance in &mut self.instances {
            instance.particles.clear();
            instance.emit_accumulator.iter_mut().for_each(|a| *a = 0.0);
            instance.burst_done.iter_mut().for_each(|b| *b = false);
            instance.spawn_counter = 0;
        }
    }

    /// Aggregate coverage across every compiled system, deduplicated by
    /// function name (worst level wins).
    pub fn coverage_summary(&self) -> Vec<CoverageEntry> {
        let mut entries: Vec<CoverageEntry> = Vec::new();
        for system in &self.systems {
            for entry in &system.coverage {
                match entries
                    .iter_mut()
                    .find(|existing| existing.function == entry.function)
                {
                    Some(existing) => {
                        if rank(entry.level) > rank(existing.level) {
                            existing.level = entry.level;
                        }
                    }
                    None => entries.push(entry.clone()),
                }
            }
        }
        fn rank(level: SupportLevel) -> u8 {
            match level {
                SupportLevel::Full => 0,
                SupportLevel::Approximate => 1,
                SupportLevel::PreviewInert => 2,
                SupportLevel::Unsupported => 3,
            }
        }
        entries.sort_by(|a, b| {
            rank(b.level)
                .cmp(&rank(a.level))
                .then(a.function.cmp(&b.function))
        });
        entries
    }

    pub fn step(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        // Break large jumps (window minimized, slow frame) into bounded
        // sub-steps so integration stays stable.
        let max_step = self
            .systems
            .iter()
            .map(|system| system.maximum_time_step)
            .fold(0.1_f32, f32::min);
        let mut remaining = dt.min(1.0);
        while remaining > 0.0 {
            let sub = remaining.min(max_step);
            self.step_once(sub);
            remaining -= sub;
        }
        for velocity in &mut self.control_point_velocity {
            *velocity = [0.0; 3];
        }
    }

    /// Reports control point motion since the last step so operators that
    /// track a moving control point respond to gizmo drags.
    pub fn drag_control_point(&mut self, index: usize, position: [f32; 3], dt_hint: f32) {
        if index >= MAX_CONTROL_POINTS {
            return;
        }
        let previous = self.control_points[index];
        self.control_points[index] = position;
        if dt_hint > 1e-4 {
            self.control_point_velocity[index] = scale(sub(position, previous), 1.0 / dt_hint);
        }
    }

    fn step_once(&mut self, dt: f32) {
        let new_time = self.time + dt;
        let mut emitters_alive = false;
        let total_live: usize = self.live_particles();
        let mut spawn_budget = MAX_TOTAL_PARTICLES.saturating_sub(total_live);

        for instance_index in 0..self.instances.len() {
            let local_time = new_time - self.instances[instance_index].start_time;
            if local_time <= 0.0 {
                emitters_alive = true;
                continue;
            }
            let system = self.instances[instance_index].system;

            // Run system-level control point writers before emission so the
            // frame's spawns see fresh control points.
            self.run_control_point_operators(instance_index, local_time);

            let spawned = self.emit(instance_index, local_time, dt, &mut spawn_budget);
            if spawned {
                emitters_alive = true;
            } else {
                let instance = &self.instances[instance_index];
                let compiled = &self.systems[system];
                if compiled
                    .emitters
                    .iter()
                    .zip(&instance.burst_done)
                    .any(|(emitter, done)| emitter_alive(emitter, local_time) && !*done)
                {
                    emitters_alive = true;
                }
            }

            self.simulate_instance(instance_index, local_time, dt);
        }

        self.time = new_time;
        self.emitters_alive = emitters_alive;
    }

    fn run_control_point_operators(&mut self, instance_index: usize, _local_time: f32) {
        let system = self.instances[instance_index].system;
        let operators = &self.systems[system].operators;
        let mut writes: Vec<(usize, [f32; 3])> = Vec::new();
        for operator in operators {
            match operator {
                Operator::SetControlPointPositions {
                    base_control_point,
                    points,
                } => {
                    let base = self.control_points[*base_control_point];
                    for (index, location) in points {
                        writes.push((*index, add(base, *location)));
                    }
                }
                Operator::SetChildControlPointsFromParticles {
                    first_control_point,
                    count,
                    first_particle,
                } => {
                    let particles = &self.instances[instance_index].particles;
                    for offset in 0..*count {
                        let particle = first_particle + offset;
                        if particle < particles.len() {
                            writes.push((
                                (*first_control_point + offset).min(MAX_CONTROL_POINTS - 1),
                                particles.position[particle],
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        for (index, position) in writes {
            if index < MAX_CONTROL_POINTS {
                self.control_points[index] = position;
            }
        }
    }

    fn emit(
        &mut self,
        instance_index: usize,
        local_time: f32,
        dt: f32,
        spawn_budget: &mut usize,
    ) -> bool {
        let system = self.instances[instance_index].system;
        let compiled = &self.systems[system];
        // u64: hostile emission rates would overflow a u32 before the spawn
        // budget gets a chance to clamp.
        let mut to_spawn: u64 = 0;

        // Initial particles burst once at instance start.
        if self.instances[instance_index].spawn_counter == 0 && compiled.initial_particles > 0 {
            to_spawn += u64::from(compiled.initial_particles);
        }

        for (emitter_index, emitter) in compiled.emitters.iter().enumerate() {
            match emitter {
                Emitter::Continuously {
                    start_time,
                    rate,
                    duration,
                } => {
                    if local_time < *start_time {
                        continue;
                    }
                    if *duration > 0.0 && local_time > start_time + duration {
                        continue;
                    }
                    let accumulator =
                        &mut self.instances[instance_index].emit_accumulator[emitter_index];
                    *accumulator += rate * dt;
                    let whole = accumulator.floor();
                    *accumulator -= whole;
                    to_spawn += whole as u64;
                }
                Emitter::Instantaneously { start_time, count } => {
                    let done = &mut self.instances[instance_index].burst_done[emitter_index];
                    if !*done && local_time >= *start_time {
                        *done = true;
                        to_spawn += u64::from(*count);
                    }
                }
                Emitter::Noise {
                    start_time,
                    duration,
                    minimum,
                    maximum,
                    time_scale,
                } => {
                    if local_time < *start_time {
                        continue;
                    }
                    if *duration > 0.0 && local_time > start_time + duration {
                        continue;
                    }
                    let noise = value_noise(local_time * time_scale * 10.0, 0.0, 0.0, 0.0);
                    let rate = minimum + (maximum - minimum) * (noise * 0.5 + 0.5);
                    let accumulator =
                        &mut self.instances[instance_index].emit_accumulator[emitter_index];
                    *accumulator += rate * dt;
                    let whole = accumulator.floor();
                    *accumulator -= whole;
                    to_spawn += whole as u64;
                }
            }
        }

        if to_spawn == 0 {
            return false;
        }

        let headroom = compiled
            .max_particles
            .saturating_sub(self.instances[instance_index].particles.len())
            .min(*spawn_budget);
        let spawning = (to_spawn as usize).min(headroom);
        *spawn_budget -= spawning;
        for _ in 0..spawning {
            self.spawn_particle(instance_index, local_time);
        }
        spawning > 0
    }

    fn spawn_particle(&mut self, instance_index: usize, local_time: f32) {
        let system = self.instances[instance_index].system;
        let parent = self.instances[instance_index].parent;

        let spawn_index = self.instances[instance_index].spawn_counter;
        self.instances[instance_index].spawn_counter = spawn_index.wrapping_add(1);

        // The RNG is tiny; moving it out sidesteps borrowing `instances`
        // mutably while parent particles are read from the same vec.
        let mut spawn_rng = self.instances[instance_index].rng.clone();
        let compiled = &self.systems[system];

        let mut position = self.control_points[0];
        let mut velocity = [0.0_f32; 3];
        let mut lifetime = f32::MAX;
        let mut radius = compiled.constant_radius;
        let mut alpha = compiled.constant_alpha;
        let mut color = compiled.constant_color;
        let mut rotation = compiled.constant_rotation;
        let mut rotation_speed = compiled.constant_rotation_speed;
        let mut sequence = compiled.constant_sequence;
        let mut trail_length = 0.1_f32;
        let mut mirrored = false;
        let mut pre_age = 0.0_f32;

        let control_points = self.control_points;
        let rng = &mut spawn_rng;

        for initializer in &compiled.initializers {
            match initializer {
                Initializer::LifetimeRandom { min, max, exponent } => {
                    lifetime = rng.range_exp(*min, *max, *exponent);
                }
                Initializer::PreAge { min, max } => {
                    pre_age = rng.range(*min, *max).max(0.0);
                }
                Initializer::AlphaRandom { min, max, exponent } => {
                    alpha = rng.range_exp(*min, *max, *exponent).clamp(0.0, 1.0);
                }
                Initializer::ColorRandom { color1, color2 } => {
                    color = lerp3(*color1, *color2, rng.unit());
                }
                Initializer::RadiusRandom { min, max, exponent } => {
                    radius = rng.range_exp(*min, *max, *exponent);
                }
                Initializer::RotationRandom {
                    initial,
                    offset_min,
                    offset_max,
                    exponent,
                } => {
                    rotation = initial + rng.range_exp(*offset_min, *offset_max, *exponent);
                }
                Initializer::RotationSpeedRandom {
                    constant,
                    min,
                    max,
                    exponent,
                    random_flip,
                } => {
                    let mut speed = constant + rng.range_exp(*min, *max, *exponent);
                    if *random_flip && rng.unit() < 0.5 {
                        speed = -speed;
                    }
                    rotation_speed = speed;
                }
                Initializer::YawFlipRandom { percentage } => {
                    mirrored = rng.unit() < *percentage;
                }
                Initializer::PositionWithinSphere {
                    control_point,
                    distance_min,
                    distance_max,
                    bias,
                    speed_min,
                    speed_max,
                    speed_exponent,
                    local_speed_min,
                    local_speed_max,
                } => {
                    let direction = rng.biased_unit_vector(*bias);
                    let distance = rng.range(*distance_min, *distance_max);
                    position = add(control_points[*control_point], scale(direction, distance));
                    let speed = rng.range_exp(*speed_min, *speed_max, *speed_exponent);
                    velocity = add(velocity, scale(direction, speed));
                    velocity = add(velocity, rng.range_vec(*local_speed_min, *local_speed_max));
                }
                Initializer::PositionOffsetRandom {
                    control_point,
                    offset_min,
                    offset_max,
                    proportional_to_radius,
                } => {
                    let mut offset = rng.range_vec(*offset_min, *offset_max);
                    if *proportional_to_radius {
                        offset = scale(offset, radius);
                    }
                    // Offsets apply on top of wherever an earlier position
                    // initializer put the particle.
                    let _ = control_point;
                    position = add(position, offset);
                }
                Initializer::PositionWarpRandom {
                    control_point,
                    warp_min,
                    warp_max,
                } => {
                    let warp = rng.range_vec(*warp_min, *warp_max);
                    let center = control_points[*control_point];
                    let offset = sub(position, center);
                    position = add(
                        center,
                        [
                            offset[0] * warp[0],
                            offset[1] * warp[1],
                            offset[2] * warp[2],
                        ],
                    );
                }
                Initializer::PositionAlongPath {
                    start_control_point,
                    end_control_point,
                    sequential_count,
                } => {
                    let t = sequential_count
                        .as_ref()
                        .map_or_else(|| rng.unit(), |count| (spawn_index as f32 % count) / count);
                    position = lerp3(
                        control_points[*start_control_point],
                        control_points[*end_control_point],
                        t,
                    );
                }
                Initializer::PositionFromParentParticles {
                    inherited_velocity_scale,
                } => {
                    if let Some(parent_index) = parent {
                        let parent_particles = &self.instances[parent_index].particles;
                        if parent_particles.len() > 0 {
                            let pick = (rng.next_u32() as usize) % parent_particles.len();
                            position = parent_particles.position[pick];
                            velocity = add(
                                velocity,
                                scale(parent_particles.velocity[pick], *inherited_velocity_scale),
                            );
                        }
                    }
                }
                Initializer::MoveBetweenControlPoints {
                    end_control_point,
                    speed_min,
                    speed_max,
                    start_offset,
                    end_spread,
                } => {
                    let start = position;
                    let mut end = control_points[*end_control_point];
                    if *end_spread > 0.0 {
                        end = add(end, rng.range_vec([-*end_spread; 3], [*end_spread; 3]));
                    }
                    let path = sub(end, start);
                    let distance = length(path);
                    if distance > 1e-4 {
                        let direction = scale(path, 1.0 / distance);
                        position = add(start, scale(direction, *start_offset));
                        let speed = rng.range(*speed_min, *speed_max);
                        velocity = add(velocity, scale(direction, speed));
                        // Cap the lifetime so the particle dies on arrival.
                        if speed > 1e-4 {
                            lifetime = lifetime.min(distance / speed);
                        }
                    }
                }
                Initializer::VelocityRandom {
                    speed_min,
                    speed_max,
                    local_min,
                    local_max,
                } => {
                    let direction = rng.biased_unit_vector([1.0; 3]);
                    let speed = rng.range(*speed_min, *speed_max);
                    velocity = add(velocity, scale(direction, speed));
                    velocity = add(velocity, rng.range_vec(*local_min, *local_max));
                }
                Initializer::VelocityNoise {
                    output_min,
                    output_max,
                    spatial_scale,
                    time_scale,
                } => {
                    let sample = |axis_offset: f32| {
                        value_noise(
                            position[0] * spatial_scale + axis_offset,
                            position[1] * spatial_scale,
                            position[2] * spatial_scale,
                            local_time * time_scale * 0.02,
                        ) * 0.5
                            + 0.5
                    };
                    let noise = [sample(0.0), sample(37.2), sample(91.7)];
                    velocity = add(
                        velocity,
                        [
                            output_min[0] + (output_max[0] - output_min[0]) * noise[0],
                            output_min[1] + (output_max[1] - output_min[1]) * noise[1],
                            output_min[2] + (output_max[2] - output_min[2]) * noise[2],
                        ],
                    );
                }
                Initializer::SequenceRandom { min, max, second } => {
                    if !second {
                        sequence = rng.range_int(*min, *max);
                    }
                }
                Initializer::TrailLengthRandom { min, max, exponent } => {
                    trail_length = rng.range_exp(*min, *max, *exponent);
                }
                Initializer::RemapInitialScalar {
                    input_min,
                    input_max,
                    output_field,
                    output_min,
                    output_max,
                    scale_initial,
                } => {
                    let input = local_time;
                    let span = (input_max - input_min).max(1e-6);
                    let t = ((input - input_min) / span).clamp(0.0, 1.0);
                    let value = output_min + (output_max - output_min) * t;
                    let target = match output_field {
                        ScalarField::LifeDuration => &mut lifetime,
                        ScalarField::Radius => &mut radius,
                        ScalarField::Rotation => &mut rotation,
                        ScalarField::RotationSpeed => &mut rotation_speed,
                        ScalarField::Alpha => &mut alpha,
                        ScalarField::TrailLength => &mut trail_length,
                    };
                    if *scale_initial {
                        *target *= value;
                    } else {
                        *target = value;
                    }
                }
                Initializer::InitialScalarNoise {
                    output_field,
                    output_min,
                    output_max,
                    spatial_scale,
                    time_scale,
                } => {
                    let noise = value_noise(
                        position[0] * spatial_scale,
                        position[1] * spatial_scale,
                        position[2] * spatial_scale,
                        local_time * time_scale,
                    ) * 0.5
                        + 0.5;
                    let value = output_min + (output_max - output_min) * noise;
                    let target = match output_field {
                        ScalarField::LifeDuration => &mut lifetime,
                        ScalarField::Radius => &mut radius,
                        ScalarField::Rotation => &mut rotation,
                        ScalarField::RotationSpeed => &mut rotation_speed,
                        ScalarField::Alpha => &mut alpha,
                        ScalarField::TrailLength => &mut trail_length,
                    };
                    *target = value;
                }
            }
        }
        if lifetime == f32::MAX {
            // No lifetime initializer: default to one second so emit-only
            // test systems still cycle.
            lifetime = 1.0;
        }

        let particles = &mut self.instances[instance_index].particles;
        particles.position.push(position);
        particles.velocity.push(velocity);
        particles.creation_time.push(local_time - pre_age);
        particles.lifetime.push(lifetime);
        particles.radius_initial.push(radius);
        particles.radius.push(radius);
        particles.alpha_initial.push(alpha);
        particles.alpha.push(alpha);
        particles.color_initial.push(color);
        particles.color.push(color);
        particles.rotation.push(rotation);
        particles.rotation_speed.push(rotation_speed);
        particles.sequence.push(sequence);
        particles.trail_length.push(trail_length);
        particles.mirrored.push(mirrored);
        particles.spawn_index.push(spawn_index);
        self.instances[instance_index].rng = spawn_rng;
    }

    fn simulate_instance(&mut self, instance_index: usize, local_time: f32, dt: f32) {
        let system = self.instances[instance_index].system;
        let compiled = &self.systems[system];
        let control_points = self.control_points;
        let control_point_velocity = self.control_point_velocity;
        let instance = &mut self.instances[instance_index];
        let particles = &mut instance.particles;

        // Forces first (accelerations), then movement integrates, then the
        // age-driven value operators, then constraints, then decay culls.
        for force in &compiled.forces {
            for index in 0..particles.len() {
                let acceleration = match force {
                    Force::Random { min, max } => instance.rng.range_vec(*min, *max),
                    Force::PullTowardsControlPoint {
                        control_point,
                        amount,
                        falloff_power,
                    } => {
                        let target = control_points[*control_point];
                        let delta = sub(target, particles.position[index]);
                        let distance = length(delta).max(1.0);
                        let strength = amount / distance.powf(*falloff_power - 1.0);
                        scale(delta, strength / distance)
                    }
                    Force::TwistAroundAxis {
                        axis,
                        amount,
                        control_point,
                    } => {
                        let center = control_points[*control_point];
                        let offset = sub(particles.position[index], center);
                        // Tangent = axis x offset.
                        let tangent = [
                            axis[1] * offset[2] - axis[2] * offset[1],
                            axis[2] * offset[0] - axis[0] * offset[2],
                            axis[0] * offset[1] - axis[1] * offset[0],
                        ];
                        let len = length(tangent).max(1e-4);
                        scale(tangent, amount / len)
                    }
                };
                particles.velocity[index] = add(particles.velocity[index], scale(acceleration, dt));
            }
        }

        let mut has_movement = false;
        for operator in &compiled.operators {
            if let Operator::MovementBasic { gravity, drag } = operator {
                has_movement = true;
                // Source applies drag per 30Hz tick; normalize to dt.
                let drag_factor = (1.0 - drag.clamp(0.0, 1.0)).powf(dt * 30.0);
                for index in 0..particles.len() {
                    let velocity = add(particles.velocity[index], scale(*gravity, dt));
                    let velocity = scale(velocity, drag_factor);
                    particles.velocity[index] = velocity;
                    particles.position[index] = add(particles.position[index], scale(velocity, dt));
                }
            }
        }
        if !has_movement && !compiled.forces.is_empty() {
            // Forces without a movement operator still need integration.
            for index in 0..particles.len() {
                particles.position[index] = add(
                    particles.position[index],
                    scale(particles.velocity[index], dt),
                );
            }
        }

        for operator in &compiled.operators {
            match operator {
                Operator::LifespanDecay
                | Operator::MovementBasic { .. }
                | Operator::SetControlPointPositions { .. }
                | Operator::SetChildControlPointsFromParticles { .. } => {}
                Operator::AlphaFadeIn {
                    time_min,
                    time_max,
                    proportional,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let fade_time = deterministic_range(
                            particles.spawn_index[index],
                            0x11,
                            *time_min,
                            *time_max,
                        );
                        let fade_end = if *proportional {
                            fade_time * lifetime
                        } else {
                            fade_time
                        };
                        if fade_end > 1e-6 && age < fade_end {
                            particles.alpha[index] =
                                particles.alpha_initial[index] * (age / fade_end).clamp(0.0, 1.0);
                        }
                    }
                }
                Operator::AlphaFadeOut {
                    time_min,
                    time_max,
                    proportional,
                    ease,
                    fade_bias,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let fade_time = deterministic_range(
                            particles.spawn_index[index],
                            0x22,
                            *time_min,
                            *time_max,
                        );
                        let fade_duration = if *proportional {
                            fade_time * lifetime
                        } else {
                            fade_time
                        };
                        let fade_start = lifetime - fade_duration;
                        if fade_duration > 1e-6 && age > fade_start {
                            let mut t = ((age - fade_start) / fade_duration).clamp(0.0, 1.0);
                            t = bias(t, *fade_bias);
                            if *ease {
                                t = simple_spline(t);
                            }
                            particles.alpha[index] = particles.alpha_initial[index] * (1.0 - t);
                        }
                    }
                }
                Operator::AlphaFadeAndDecay {
                    start_alpha,
                    end_alpha,
                    start_fade_in,
                    end_fade_in,
                    start_fade_out,
                    end_fade_out,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let t = (age / lifetime.max(1e-6)).clamp(0.0, 1.0);
                        let base = particles.alpha_initial[index];
                        let alpha = if t < *start_fade_in {
                            0.0
                        } else if t < *end_fade_in {
                            let span = (end_fade_in - start_fade_in).max(1e-6);
                            start_alpha * ((t - start_fade_in) / span)
                        } else if t < *start_fade_out {
                            *start_alpha
                        } else if t < *end_fade_out {
                            let span = (end_fade_out - start_fade_out).max(1e-6);
                            let f = (t - start_fade_out) / span;
                            start_alpha + (end_alpha - start_alpha) * f
                        } else {
                            *end_alpha
                        };
                        particles.alpha[index] = base * alpha;
                    }
                }
                Operator::RadiusScale {
                    start_time,
                    end_time,
                    start_scale,
                    end_scale,
                    ease,
                    scale_bias,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let t = (age / lifetime.max(1e-6)).clamp(0.0, 1.0);
                        let span = (end_time - start_time).max(1e-6);
                        let mut progress = ((t - start_time) / span).clamp(0.0, 1.0);
                        progress = bias(progress, *scale_bias);
                        if *ease {
                            progress = simple_spline(progress);
                        }
                        let factor = start_scale + (end_scale - start_scale) * progress;
                        particles.radius[index] = particles.radius_initial[index] * factor;
                    }
                }
                Operator::ColorFade {
                    target,
                    start_time,
                    end_time,
                    ease,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let t = (age / lifetime.max(1e-6)).clamp(0.0, 1.0);
                        let span = (end_time - start_time).max(1e-6);
                        let mut progress = ((t - start_time) / span).clamp(0.0, 1.0);
                        if *ease {
                            progress = simple_spline(progress);
                        }
                        particles.color[index] =
                            lerp3(particles.color_initial[index], *target, progress);
                    }
                }
                Operator::RotationSpin {
                    rate_radians,
                    stop_time,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let t = (age / lifetime.max(1e-6)).clamp(0.0, 1.0);
                        if *stop_time <= 0.0 || t < *stop_time {
                            particles.rotation[index] += rate_radians * dt;
                        }
                    }
                }
                Operator::RotationBasic => {
                    for index in 0..particles.len() {
                        particles.rotation[index] += particles.rotation_speed[index] * dt;
                    }
                }
                Operator::MovementLockToControlPoint { control_point } => {
                    let delta = scale(control_point_velocity[*control_point], dt);
                    if length(delta) > 0.0 {
                        for index in 0..particles.len() {
                            particles.position[index] = add(particles.position[index], delta);
                        }
                    }
                }
                Operator::OscillateScalar {
                    field,
                    rate_min,
                    rate_max,
                    frequency_min,
                    frequency_max,
                    proportional,
                    multiplier,
                    start_phase,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let clock = if *proportional {
                            (age / lifetime.max(1e-6)).clamp(0.0, 1.0)
                        } else {
                            age
                        };
                        let spawn = particles.spawn_index[index];
                        let rate = deterministic_range(spawn, 0x33, *rate_min, *rate_max);
                        let frequency =
                            deterministic_range(spawn, 0x44, *frequency_min, *frequency_max);
                        let wave = (std::f32::consts::TAU
                            * (clock * frequency * multiplier + start_phase))
                            .sin();
                        *particles.scalar_mut(*field, index) += rate * wave * dt;
                    }
                }
                Operator::OscillateVector {
                    field,
                    rate_min,
                    rate_max,
                    frequency_min,
                    frequency_max,
                    proportional,
                    multiplier,
                    start_phase,
                } => {
                    for index in 0..particles.len() {
                        let (age, lifetime) = age_of(particles, index, local_time);
                        let clock = if *proportional {
                            (age / lifetime.max(1e-6)).clamp(0.0, 1.0)
                        } else {
                            age
                        };
                        let spawn = particles.spawn_index[index];
                        let mut delta = [0.0_f32; 3];
                        for (axis, value) in delta.iter_mut().enumerate() {
                            let salt = 0x50 + axis as u32;
                            let rate =
                                deterministic_range(spawn, salt, rate_min[axis], rate_max[axis]);
                            let frequency = deterministic_range(
                                spawn,
                                salt + 8,
                                frequency_min[axis],
                                frequency_max[axis],
                            );
                            let wave = (std::f32::consts::TAU
                                * (clock * frequency * multiplier + start_phase))
                                .sin();
                            *value = rate * wave * dt;
                        }
                        match field {
                            VectorField::Position => {
                                particles.position[index] = add(particles.position[index], delta);
                            }
                            VectorField::Tint => {
                                let color = &mut particles.color[index];
                                color[0] = (color[0] + delta[0]).clamp(0.0, 1.0);
                                color[1] = (color[1] + delta[1]).clamp(0.0, 1.0);
                                color[2] = (color[2] + delta[2]).clamp(0.0, 1.0);
                            }
                        }
                    }
                }
                Operator::NoiseVector {
                    field,
                    output_min,
                    output_max,
                    coordinate_scale,
                } => {
                    for index in 0..particles.len() {
                        let position = particles.position[index];
                        let sample = |offset: f32| {
                            value_noise(
                                position[0] * coordinate_scale * 0.01 + offset,
                                position[1] * coordinate_scale * 0.01,
                                position[2] * coordinate_scale * 0.01,
                                particles.spawn_index[index] as f32 * 0.7,
                            ) * 0.5
                                + 0.5
                        };
                        let noise = [sample(0.0), sample(51.3), sample(117.9)];
                        let value = [
                            output_min[0] + (output_max[0] - output_min[0]) * noise[0],
                            output_min[1] + (output_max[1] - output_min[1]) * noise[1],
                            output_min[2] + (output_max[2] - output_min[2]) * noise[2],
                        ];
                        match field {
                            // Position noise is applied as drift regardless
                            // of Source's set-vs-add flag; visually close and
                            // stable under variable dt.
                            VectorField::Position => {
                                particles.position[index] =
                                    add(particles.position[index], scale(value, dt));
                            }
                            VectorField::Tint => {
                                particles.color[index] = [
                                    value[0].clamp(0.0, 1.0),
                                    value[1].clamp(0.0, 1.0),
                                    value[2].clamp(0.0, 1.0),
                                ];
                            }
                        }
                    }
                }
                Operator::RemapNoiseToScalar {
                    field,
                    output_min,
                    output_max,
                    time_scale,
                    spatial_scale,
                } => {
                    for index in 0..particles.len() {
                        let position = particles.position[index];
                        let noise = value_noise(
                            position[0] * spatial_scale * 0.01,
                            position[1] * spatial_scale * 0.01,
                            position[2] * spatial_scale * 0.01,
                            local_time * time_scale,
                        ) * 0.5
                            + 0.5;
                        *particles.scalar_mut(*field, index) =
                            output_min + (output_max - output_min) * noise;
                    }
                }
                Operator::ConstrainDistanceToControlPoint {
                    control_point,
                    min_distance,
                    max_distance,
                    offset,
                } => {
                    let center = add(control_points[*control_point], *offset);
                    for index in 0..particles.len() {
                        let delta = sub(particles.position[index], center);
                        let distance = length(delta);
                        if distance < 1e-4 {
                            continue;
                        }
                        let clamped =
                            distance.clamp(*min_distance, max_distance.max(*min_distance));
                        if (clamped - distance).abs() > 1e-4 {
                            particles.position[index] =
                                add(center, scale(delta, clamped / distance));
                        }
                    }
                }
                Operator::ConstrainDistanceToPath {
                    start_control_point,
                    end_control_point,
                    max_distance,
                } => {
                    let start = control_points[*start_control_point];
                    let end = control_points[*end_control_point];
                    let axis = sub(end, start);
                    let axis_length_sq = axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2];
                    for index in 0..particles.len() {
                        let rel = sub(particles.position[index], start);
                        let t = if axis_length_sq > 1e-6 {
                            ((rel[0] * axis[0] + rel[1] * axis[1] + rel[2] * axis[2])
                                / axis_length_sq)
                                .clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let closest = add(start, scale(axis, t));
                        let delta = sub(particles.position[index], closest);
                        let distance = length(delta);
                        if distance > *max_distance && distance > 1e-4 {
                            particles.position[index] =
                                add(closest, scale(delta, max_distance / distance));
                        }
                    }
                }
            }
        }

        // Decay last so freshly-faded values are what the final frame shows.
        let has_decay = compiled
            .operators
            .iter()
            .any(|operator| matches!(operator, Operator::LifespanDecay))
            || compiled
                .operators
                .iter()
                .any(|operator| matches!(operator, Operator::AlphaFadeAndDecay { .. }));
        if has_decay {
            let mut index = 0;
            while index < particles.len() {
                let (age, lifetime) = age_of(particles, index, local_time);
                if age >= lifetime {
                    particles.swap_remove(index);
                } else {
                    index += 1;
                }
            }
        }
    }

    /// Snapshot of everything a renderer needs, instance by instance.
    pub fn render_instances(&self) -> Vec<InstanceRender<'_>> {
        self.instances
            .iter()
            .map(|instance| {
                let local_time = self.time - instance.start_time;
                let particles = &instance.particles;
                let list = (0..particles.len())
                    .map(|index| RenderParticle {
                        position: particles.position[index],
                        velocity: particles.velocity[index],
                        radius: particles.radius[index],
                        rotation: particles.rotation[index],
                        color: particles.color[index],
                        alpha: particles.alpha[index].clamp(0.0, 1.0),
                        sequence: particles.sequence[index],
                        trail_length: particles.trail_length[index],
                        mirrored: particles.mirrored[index],
                        spawn_index: particles.spawn_index[index],
                        age: (local_time - particles.creation_time[index]).max(0.0),
                        lifetime: particles.lifetime[index],
                    })
                    .collect();
                InstanceRender {
                    system: &self.systems[instance.system],
                    particles: list,
                }
            })
            .collect()
    }
}

fn age_of(particles: &ParticleSet, index: usize, local_time: f32) -> (f32, f32) {
    (
        local_time - particles.creation_time[index],
        particles.lifetime[index].max(1e-6),
    )
}

fn emitter_alive(emitter: &Emitter, local_time: f32) -> bool {
    match emitter {
        Emitter::Continuously {
            start_time,
            duration,
            ..
        }
        | Emitter::Noise {
            start_time,
            duration,
            ..
        } => *duration <= 0.0 || local_time <= start_time + duration,
        Emitter::Instantaneously { start_time, .. } => local_time < *start_time,
    }
}

/// Stable per-particle random in [min, max]: operators like fade-out draw a
/// random duration per particle that must not change between frames.
fn deterministic_range(spawn_index: u32, salt: u32, min: f32, max: f32) -> f32 {
    let mut n = spawn_index
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(salt.wrapping_mul(0x85EB_CA6B));
    n ^= n >> 13;
    n = n.wrapping_mul(0xC2B2_AE35);
    n ^= n >> 16;
    let t = (n & 0xffff) as f32 / 65535.0;
    min + (max - min) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::pcf::{PcfChild, PcfValue};

    fn attrs(entries: &[(&str, PcfValue)]) -> PcfAttributes {
        let mut out = PcfAttributes::default();
        for (name, value) in entries {
            out.push(*name, value.clone());
        }
        out
    }

    fn function(name: &str, entries: &[(&str, PcfValue)]) -> PcfFunction {
        let mut all = vec![("functionName", PcfValue::String(name.to_owned()))];
        all.extend(entries.iter().cloned());
        PcfFunction {
            name: name.to_owned(),
            attributes: attrs(&all),
        }
    }

    fn basic_system() -> PcfSystem {
        PcfSystem {
            name: "test".to_owned(),
            attributes: attrs(&[
                ("max_particles", PcfValue::Int(100)),
                ("radius", PcfValue::Float(4.0)),
                ("color", PcfValue::Color([255, 128, 0, 255])),
            ]),
            emitters: vec![function(
                "emit_continuously",
                &[("emission_rate", PcfValue::Float(10.0))],
            )],
            initializers: vec![function(
                "Lifetime Random",
                &[
                    ("lifetime_min", PcfValue::Float(1.0)),
                    ("lifetime_max", PcfValue::Float(1.0)),
                ],
            )],
            operators: vec![function("Lifespan Decay", &[])],
            renderers: vec![function("render_animated_sprites", &[])],
            forces: vec![],
            constraints: vec![],
            children: vec![],
        }
    }

    fn engine_for(systems: Vec<PcfSystem>) -> ParticleEngine {
        let file = PcfFile {
            encoding_version: 2,
            format_version: 1,
            systems,
        };
        ParticleEngine::new(&file, 0, 7).expect("engine builds")
    }

    #[test]
    fn continuous_emission_rate_and_decay() {
        let mut engine = engine_for(vec![basic_system()]);
        engine.step(0.5);
        // 10/s for 0.5s: allow off-by-one from accumulator flooring.
        let live = engine.live_particles();
        assert!((4..=6).contains(&live), "live={live}");
        // After the 1s lifetime elapses, the earliest particles die off.
        engine.step(2.0);
        assert!(engine.live_particles() <= 11);
        assert!(!engine.finished(), "continuous emitters never finish");
    }

    #[test]
    fn instantaneous_burst_finishes() {
        let mut system = basic_system();
        system.emitters = vec![function(
            "emit_instantaneously",
            &[("num_to_emit", PcfValue::Int(25))],
        )];
        let mut engine = engine_for(vec![system]);
        engine.step(0.1);
        assert_eq!(engine.live_particles(), 25);
        engine.step(1.5);
        assert_eq!(engine.live_particles(), 0);
        assert!(engine.finished());
        engine.restart();
        engine.step(0.1);
        assert_eq!(engine.live_particles(), 25, "restart replays the burst");
    }

    #[test]
    fn max_particles_caps_spawns() {
        let mut system = basic_system();
        system.attributes = attrs(&[("max_particles", PcfValue::Int(8))]);
        system.emitters = vec![function(
            "emit_instantaneously",
            &[("num_to_emit", PcfValue::Int(500))],
        )];
        let mut engine = engine_for(vec![system]);
        engine.step(0.1);
        assert_eq!(engine.live_particles(), 8);
    }

    #[test]
    fn alpha_fade_out_reaches_zero() {
        let mut system = basic_system();
        system.operators.push(function(
            "Alpha Fade Out Random",
            &[
                ("fade out time min", PcfValue::Float(0.5)),
                ("fade out time max", PcfValue::Float(0.5)),
                ("proportional 0/1", PcfValue::Bool(true)),
            ],
        ));
        system.emitters = vec![function(
            "emit_instantaneously",
            &[("num_to_emit", PcfValue::Int(1))],
        )];
        let mut engine = engine_for(vec![system]);
        engine.step(0.05);
        let early = engine.render_instances()[0].particles[0].alpha;
        engine.step(0.90);
        let late = engine.render_instances()[0].particles[0].alpha;
        assert!(early > 0.9, "early={early}");
        // Age ~0.90 of a 1s life with a 0.5-proportional fade => ~0.2.
        assert!(late < 0.3, "late={late}");
        assert!(late < early);
    }

    #[test]
    fn radius_scale_interpolates_from_initial() {
        let mut system = basic_system();
        system.operators.push(function(
            "Radius Scale",
            &[
                ("start_time", PcfValue::Float(0.0)),
                ("end_time", PcfValue::Float(0.5)),
                ("radius_start_scale", PcfValue::Float(1.0)),
                ("radius_end_scale", PcfValue::Float(3.0)),
            ],
        ));
        system.emitters = vec![function(
            "emit_instantaneously",
            &[("num_to_emit", PcfValue::Int(1))],
        )];
        let mut engine = engine_for(vec![system]);
        // Past end_time (0.5 of proportional life) the scale is pinned at 3.
        engine.step(0.8);
        let particle = engine.render_instances()[0].particles[0];
        assert!(
            (particle.radius - 12.0).abs() < 0.01,
            "radius={} (expected 12: initial 4 * end scale 3)",
            particle.radius
        );
    }

    #[test]
    fn movement_basic_applies_gravity() {
        let mut system = basic_system();
        system.operators.push(function(
            "Movement Basic",
            &[("gravity", PcfValue::Vector3([0.0, 0.0, -100.0]))],
        ));
        system.emitters = vec![function(
            "emit_instantaneously",
            &[("num_to_emit", PcfValue::Int(1))],
        )];
        let mut engine = engine_for(vec![system]);
        engine.step(0.5);
        let particle = engine.render_instances()[0].particles[0];
        assert!(particle.position[2] < -1.0, "z={}", particle.position[2]);
        assert!(particle.velocity[2] < -20.0);
    }

    #[test]
    fn children_start_after_delay() {
        let mut parent = basic_system();
        parent.name = "parent".to_owned();
        parent.children = vec![PcfChild {
            name: "kid".to_owned(),
            system_index: Some(1),
            delay: 1.0,
        }];
        let mut kid = basic_system();
        kid.name = "kid".to_owned();
        let mut engine = engine_for(vec![parent, kid]);
        engine.step(0.5);
        let renders = engine.render_instances();
        assert_eq!(renders.len(), 2);
        assert!(!renders[0].particles.is_empty());
        assert!(renders[1].particles.is_empty(), "child delayed 1s");
        engine.step(1.0);
        let renders = engine.render_instances();
        assert!(!renders[1].particles.is_empty());
    }

    #[test]
    fn deterministic_replay() {
        let build = || {
            let mut system = basic_system();
            system.initializers.push(function(
                "Position Within Sphere Random",
                &[
                    ("distance_max", PcfValue::Float(20.0)),
                    ("speed_max", PcfValue::Float(50.0)),
                ],
            ));
            engine_for(vec![system])
        };
        let mut a = build();
        let mut b = build();
        for _ in 0..10 {
            a.step(0.1);
            b.step(0.1);
        }
        let pa = &a.render_instances()[0].particles;
        let pb = &b.render_instances()[0].particles;
        assert_eq!(pa.len(), pb.len());
        for (x, y) in pa.iter().zip(pb.iter()) {
            assert_eq!(x.position, y.position);
        }
    }

    #[test]
    fn coverage_reports_unsupported_and_inert() {
        let mut system = basic_system();
        system.operators.push(function("Collision via traces", &[]));
        system.operators.push(function("Made Up Operator", &[]));
        let engine = engine_for(vec![system]);
        let coverage = engine.coverage_summary();
        let level_of = |name: &str| {
            coverage
                .iter()
                .find(|entry| entry.function == name)
                .map(|entry| entry.level)
        };
        assert_eq!(level_of("Lifespan Decay"), Some(SupportLevel::Full));
        assert_eq!(
            level_of("Collision via traces"),
            Some(SupportLevel::PreviewInert)
        );
        assert_eq!(
            level_of("Made Up Operator"),
            Some(SupportLevel::Unsupported)
        );
    }

    #[test]
    fn control_point_gizmo_moves_spawns() {
        let mut system = basic_system();
        system.initializers.push(function(
            "Position Within Sphere Random",
            &[("distance_max", PcfValue::Float(0.0))],
        ));
        let mut engine = engine_for(vec![system]);
        engine.set_control_point(0, [100.0, 0.0, 0.0]);
        engine.step(0.2);
        let particle = engine.render_instances()[0].particles[0];
        assert!((particle.position[0] - 100.0).abs() < 1.0);
    }

    #[test]
    fn spawn_budget_survives_hostile_counts() {
        let mut system = basic_system();
        system.attributes = attrs(&[("max_particles", PcfValue::Int(1_000_000))]);
        system.emitters = vec![function(
            "emit_continuously",
            &[("emission_rate", PcfValue::Float(1e12))],
        )];
        let mut engine = engine_for(vec![system]);
        engine.step(1.0);
        assert!(engine.live_particles() <= MAX_TOTAL_PARTICLES);
    }
}
