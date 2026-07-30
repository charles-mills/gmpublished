//! Engine construction, lifecycle, stepping, and control points.

use super::*;

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
            control_points: [Vec3::splat(0.0); MAX_CONTROL_POINTS],
            control_point_velocity: [Vec3::splat(0.0); MAX_CONTROL_POINTS],
            time: 0.0,
            seed,
            emitters_alive: true,
        };
        engine.spawn_instance_tree(0, 0.0, None);
        Some(engine)
    }

    /// Spawns a system and its child systems.
    ///
    /// An explicit stack, not recursion: `.pcf` child links are untrusted and
    /// can cycle, so depth would be attacker-controlled stack frames. Bounded
    /// twice — [`MAX_INSTANCE_TREE_DEPTH`] for a deep chain, the instance
    /// count for a wide one.
    fn spawn_instance_tree(&mut self, system: usize, start_time: f32, parent: Option<usize>) {
        let mut pending = vec![(system, start_time, parent, 0_usize)];
        while let Some((system, start_time, parent, depth)) = pending.pop() {
            if self.instances.len() >= self.systems.len() * 2 || depth >= MAX_INSTANCE_TREE_DEPTH {
                continue;
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
            // Reversed: a stack pops last-pushed first, so pushing in
            // reverse spawns children in declaration order.
            for child in self.systems[system].children.clone().into_iter().rev() {
                pending.push((
                    child.system,
                    start_time + child.delay,
                    Some(instance_index),
                    depth + 1,
                ));
            }
        }
    }

    /// Every compiled system in parent-before-child order.
    #[must_use]
    pub fn systems(&self) -> &[CompiledSystem] {
        &self.systems
    }

    /// Root system selected when the engine was constructed.
    #[must_use]
    pub fn root_system(&self) -> &CompiledSystem {
        &self.systems[0]
    }

    /// Current simulation time in seconds.
    #[must_use]
    pub fn time(&self) -> f32 {
        self.time
    }

    /// Total live particle count across every instance.
    #[must_use]
    pub fn live_particles(&self) -> usize {
        self.instances
            .iter()
            .map(|instance| instance.particles.len())
            .sum()
    }

    /// World-space framing radius over the whole effect tree, including
    /// control point spread.
    #[must_use]
    pub fn bounding_radius(&self) -> f32 {
        let system_radius = self
            .systems
            .iter()
            .map(|system| system.bounding_radius)
            .fold(24.0_f32, f32::max);
        let control_point_reach = (0..=self.highest_control_point())
            .map(|index| (self.control_points[index]).length())
            .fold(0.0_f32, f32::max);
        system_radius + control_point_reach
    }

    /// Highest control point index read by any compiled operator, i.e. how
    /// many gizmos are worth showing.
    #[must_use]
    pub fn highest_control_point(&self) -> usize {
        self.systems
            .iter()
            .map(|system| system.highest_control_point)
            .max()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn control_point(&self, index: ControlPointIndex) -> Vec3 {
        self.control_points[index.get()]
    }

    pub fn set_control_point(&mut self, index: ControlPointIndex, position: Vec3) {
        self.control_points[index.get()] = position;
    }

    /// True once every emitter has finished and no particles remain; the
    /// caller can restart to loop the effect.
    #[must_use]
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
    #[must_use]
    pub fn coverage_summary(&self) -> Vec<CoverageEntry> {
        let mut entries: Vec<CoverageEntry> = Vec::new();
        for system in &self.systems {
            for entry in &system.coverage {
                match entries
                    .iter_mut()
                    .find(|existing| existing.function == entry.function)
                {
                    Some(existing) => {
                        if entry.level > existing.level {
                            existing.level = entry.level;
                        }
                    }
                    None => entries.push(entry.clone()),
                }
            }
        }
        entries.sort_by(|a, b| b.level.cmp(&a.level).then(a.function.cmp(&b.function)));
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
            *velocity = Vec3::splat(0.0);
        }
    }

    /// Reports control point motion since the last step so operators that
    /// track a moving control point respond to gizmo drags.
    pub fn drag_control_point(&mut self, index: ControlPointIndex, position: Vec3, dt_hint: f32) {
        let index = index.get();
        let previous = self.control_points[index];
        self.control_points[index] = position;
        if dt_hint > 1e-4 {
            self.control_point_velocity[index] = (position - previous) * (1.0 / dt_hint);
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
}
