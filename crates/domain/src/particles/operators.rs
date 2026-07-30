//! Per-frame operators and constraints.

use super::*;

impl ParticleEngine {
    pub(super) fn run_control_point_operators(&mut self, instance_index: usize, _local_time: f32) {
        let system = self.instances[instance_index].system;
        let operators = &self.systems[system].operators;
        let mut writes: Vec<(ControlPointIndex, Vec3)> = Vec::new();
        for operator in operators {
            match operator {
                Operator::SetControlPointPositions {
                    base_control_point,
                    points,
                } => {
                    let base = self.control_points[base_control_point.get()];
                    for (index, location) in points {
                        writes.push((*index, (base + (*location))));
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
                                ControlPointIndex::clamped(first_control_point.get() + offset),
                                particles.position[particle],
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        for (index, position) in writes {
            self.control_points[index.get()] = position;
        }
    }

    pub(super) fn simulate_instance(&mut self, instance_index: usize, local_time: f32, dt: f32) {
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
                        let target = control_points[control_point.get()];
                        let delta = target - (particles.position[index]);
                        let distance = delta.length().max(1.0);
                        let strength = amount / distance.powf(*falloff_power - 1.0);
                        delta * (strength / distance)
                    }
                    Force::TwistAroundAxis {
                        axis,
                        amount,
                        control_point,
                    } => {
                        let center = control_points[control_point.get()];
                        let offset = (particles.position[index]) - center;
                        let tangent = axis.cross(offset);
                        let len = tangent.length().max(1e-4);
                        tangent * (amount / len)
                    }
                };
                particles.velocity[index] += acceleration * dt;
            }
        }

        let mut has_movement = false;
        for operator in &compiled.operators {
            if let Operator::MovementBasic { gravity, drag } = operator {
                has_movement = true;
                // Source applies drag per 30Hz tick; normalize to dt.
                let drag_factor = (1.0 - drag.clamp(0.0, 1.0)).powf(dt * 30.0);
                for index in 0..particles.len() {
                    let velocity = (particles.velocity[index]) + ((*gravity) * dt);
                    let velocity = velocity * drag_factor;
                    particles.velocity[index] = velocity;
                    particles.position[index] += velocity * dt;
                }
            }
        }
        if !has_movement && !compiled.forces.is_empty() {
            // Forces without a movement operator still need integration.
            for index in 0..particles.len() {
                particles.position[index] =
                    (particles.position[index]) + ((particles.velocity[index]) * dt);
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
                            particles.color_initial[index].lerp(*target, progress);
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
                    let delta = control_point_velocity[control_point.get()] * dt;
                    if delta.length() > 0.0 {
                        for index in 0..particles.len() {
                            particles.position[index] += delta;
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
                        let mut delta = Vec3::ZERO;
                        for axis in 0..3 {
                            let value = &mut delta[axis];
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
                                particles.position[index] += delta;
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
                        let value = Vec3::new(
                            output_min[0] + (output_max[0] - output_min[0]) * noise[0],
                            output_min[1] + (output_max[1] - output_min[1]) * noise[1],
                            output_min[2] + (output_max[2] - output_min[2]) * noise[2],
                        );
                        match field {
                            // Position noise is applied as drift regardless
                            // of Source's set-vs-add flag; visually close and
                            // stable under variable dt.
                            VectorField::Position => {
                                particles.position[index] += value * dt;
                            }
                            VectorField::Tint => {
                                particles.color[index] = Vec3::new(
                                    value[0].clamp(0.0, 1.0),
                                    value[1].clamp(0.0, 1.0),
                                    value[2].clamp(0.0, 1.0),
                                );
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
                    let center = control_points[control_point.get()] + (*offset);
                    for index in 0..particles.len() {
                        let delta = (particles.position[index]) - center;
                        let distance = delta.length();
                        if distance < 1e-4 {
                            continue;
                        }
                        let clamped =
                            distance.clamp(*min_distance, max_distance.max(*min_distance));
                        if (clamped - distance).abs() > 1e-4 {
                            particles.position[index] = center + (delta * (clamped / distance));
                        }
                    }
                }
                Operator::ConstrainDistanceToPath {
                    start_control_point,
                    end_control_point,
                    max_distance,
                } => {
                    let start = control_points[start_control_point.get()];
                    let end = control_points[end_control_point.get()];
                    let axis = end - start;
                    let axis_length_sq = axis.dot(axis);
                    for index in 0..particles.len() {
                        let rel = (particles.position[index]) - start;
                        let t = if axis_length_sq > 1e-6 {
                            (rel.dot(axis) / axis_length_sq).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let closest = start + (axis * t);
                        let delta = (particles.position[index]) - closest;
                        let distance = delta.length();
                        if distance > *max_distance && distance > 1e-4 {
                            particles.position[index] =
                                closest + (delta * (max_distance / distance));
                        }
                    }
                }
            }
        }

        // Decay last so freshly-faded values are what the final frame shows.
        if compiled.retires_particles {
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
}
