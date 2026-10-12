use super::*;

#[derive(Clone, Copy, Debug)]
pub struct RenderParticle {
    pub position: Vec3,
    pub velocity: Vec3,
    pub radius: f32,
    /// Roll in radians.
    pub rotation: f32,
    /// sRGB color and opacity, 0..1.
    pub color: Vec3,
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

pub struct RenderParticles<'a> {
    particles: &'a ParticleSet,
    local_time: f32,
    index: usize,
}

impl RenderParticles<'_> {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ExactSizeIterator for RenderParticles<'_> {
    fn len(&self) -> usize {
        self.particles.len().saturating_sub(self.index)
    }
}

impl Iterator for RenderParticles<'_> {
    type Item = RenderParticle;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        if index >= self.particles.len() {
            return None;
        }
        self.index += 1;
        Some(render_particle(self.particles, index, self.local_time))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl ParticleEngine {
    #[must_use]
    pub fn render_instances(&self) -> Vec<InstanceRender<'_>> {
        let mut renders = Vec::with_capacity(self.instances.len());
        self.visit_render_instances(|system, particles| {
            renders.push(InstanceRender {
                system,
                particles: particles.collect(),
            });
        });
        renders
    }

    pub fn visit_render_instances<'a>(
        &'a self,
        mut visit: impl FnMut(&'a CompiledSystem, RenderParticles<'a>),
    ) {
        for instance in &self.instances {
            visit(
                &self.systems[instance.system],
                RenderParticles {
                    particles: &instance.particles,
                    local_time: self.time - instance.start_time,
                    index: 0,
                },
            );
        }
    }
}

fn render_particle(particles: &ParticleSet, index: usize, local_time: f32) -> RenderParticle {
    RenderParticle {
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
    }
}
