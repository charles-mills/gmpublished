//! Particle emission and spawn-time initialization.

use super::*;

impl ParticleEngine {
    pub(super) fn emit(
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
        let mut velocity = Vec3::ZERO;
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
                    color = color1.lerp(*color2, rng.unit());
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
                    position = control_points[control_point.get()] + (direction * distance);
                    let speed = rng.range_exp(*speed_min, *speed_max, *speed_exponent);
                    velocity += direction * speed;
                    velocity += rng.range_vec(*local_speed_min, *local_speed_max);
                }
                Initializer::PositionOffsetRandom {
                    control_point: _control_point,
                    offset_min,
                    offset_max,
                    proportional_to_radius,
                } => {
                    let mut offset = rng.range_vec(*offset_min, *offset_max);
                    if *proportional_to_radius {
                        offset *= radius;
                    }
                    // Source carries a control-point field for this initializer,
                    // but the offset is relative to the position established by
                    // earlier initializers; it does not read the point itself.
                    position += offset;
                }
                Initializer::PositionWarpRandom {
                    control_point,
                    warp_min,
                    warp_max,
                } => {
                    let warp = rng.range_vec(*warp_min, *warp_max);
                    let center = control_points[control_point.get()];
                    let offset = position - center;
                    position = center
                        + Vec3::new(
                            offset[0] * warp[0],
                            offset[1] * warp[1],
                            offset[2] * warp[2],
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
                    position = control_points[start_control_point.get()]
                        .lerp(control_points[end_control_point.get()], t);
                }
                Initializer::PositionFromParentParticles {
                    inherited_velocity_scale,
                } => {
                    if let Some(parent_index) = parent {
                        let parent_particles = &self.instances[parent_index].particles;
                        if parent_particles.len() > 0 {
                            let pick = (rng.next_u32() as usize) % parent_particles.len();
                            position = parent_particles.position[pick];
                            velocity +=
                                (parent_particles.velocity[pick]) * (*inherited_velocity_scale);
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
                    let mut end = control_points[end_control_point.get()];
                    if *end_spread > 0.0 {
                        end += rng.range_vec(Vec3::splat(-*end_spread), Vec3::splat(*end_spread));
                    }
                    let path = end - start;
                    let distance = path.length();
                    if distance > 1e-4 {
                        let direction = path * (1.0 / distance);
                        position = start + (direction * (*start_offset));
                        let speed = rng.range(*speed_min, *speed_max);
                        velocity += direction * speed;
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
                    let direction = rng.biased_unit_vector(Vec3::splat(1.0));
                    let speed = rng.range(*speed_min, *speed_max);
                    velocity += direction * speed;
                    velocity += rng.range_vec(*local_min, *local_max);
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
                    let noise = Vec3::new(sample(0.0), sample(37.2), sample(91.7));
                    velocity += Vec3::new(
                        output_min[0] + (output_max[0] - output_min[0]) * noise[0],
                        output_min[1] + (output_max[1] - output_min[1]) * noise[1],
                        output_min[2] + (output_max[2] - output_min[2]) * noise[2],
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
}
