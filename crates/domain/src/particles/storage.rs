use super::*;

macro_rules! particle_set {
    ($($(#[$meta:meta])* $field:ident: $ty:ty),+ $(,)?) => {
        #[derive(Clone, Debug, Default)]
        pub(super) struct ParticleSet {
            $($(#[$meta])* pub(super) $field: Vec<$ty>,)+
        }

        impl ParticleSet {
            pub(super) fn swap_remove(&mut self, index: usize) {
                $(self.$field.swap_remove(index);)+
            }

            pub(super) fn clear(&mut self) {
                $(self.$field.clear();)+
            }

            #[cfg(test)]
            pub(super) fn arrays_are_parallel(&self) -> bool {
                let len = self.position.len();
                $(self.$field.len() == len &&)+ true
            }
        }
    };
}

particle_set! {
    position: Vec3,
    velocity: Vec3,
    creation_time: f32,
    lifetime: f32,
    radius_initial: f32,
    radius: f32,
    alpha_initial: f32,
    alpha: f32,
    color_initial: Vec3,
    color: Vec3,
    rotation: f32,
    rotation_speed: f32,
    sequence: i32,
    trail_length: f32,
    mirrored: bool,
    spawn_index: u32,
}

impl ParticleSet {
    pub(super) fn len(&self) -> usize {
        self.position.len()
    }

    pub(super) fn scalar_mut(&mut self, field: ScalarField, index: usize) -> &mut f32 {
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
