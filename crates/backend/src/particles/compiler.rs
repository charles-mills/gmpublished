use super::{
    CoverageEntry, MAX_CONTROL_POINTS, PcfAttributes, PcfFunction, PcfSystem, SupportLevel,
    color_to_rgb, length,
};

// --- Scalar/vector field ids (Source particle attribute indices) ---------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScalarField {
    LifeDuration,
    Radius,
    Rotation,
    RotationSpeed,
    Alpha,
    TrailLength,
}

impl ScalarField {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => Self::LifeDuration,
            3 => Self::Radius,
            4 => Self::Rotation,
            5 => Self::RotationSpeed,
            7 => Self::Alpha,
            10 => Self::TrailLength,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VectorField {
    Position,
    Tint,
}

impl VectorField {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Position,
            6 => Self::Tint,
            _ => return None,
        })
    }
}

// --- Compiled operator types ----------------------------------------------

#[derive(Debug, Clone)]
pub(super) enum Emitter {
    Continuously {
        start_time: f32,
        rate: f32,
        duration: f32,
    },
    Instantaneously {
        start_time: f32,
        count: u32,
    },
    Noise {
        start_time: f32,
        duration: f32,
        minimum: f32,
        maximum: f32,
        time_scale: f32,
    },
}

#[derive(Debug, Clone)]
pub(super) enum Initializer {
    LifetimeRandom {
        min: f32,
        max: f32,
        exponent: f32,
    },
    PreAge {
        min: f32,
        max: f32,
    },
    AlphaRandom {
        min: f32,
        max: f32,
        exponent: f32,
    },
    ColorRandom {
        color1: [f32; 3],
        color2: [f32; 3],
    },
    RadiusRandom {
        min: f32,
        max: f32,
        exponent: f32,
    },
    RotationRandom {
        initial: f32,
        offset_min: f32,
        offset_max: f32,
        exponent: f32,
    },
    RotationSpeedRandom {
        constant: f32,
        min: f32,
        max: f32,
        exponent: f32,
        random_flip: bool,
    },
    YawFlipRandom {
        percentage: f32,
    },
    PositionWithinSphere {
        control_point: usize,
        distance_min: f32,
        distance_max: f32,
        bias: [f32; 3],
        speed_min: f32,
        speed_max: f32,
        speed_exponent: f32,
        local_speed_min: [f32; 3],
        local_speed_max: [f32; 3],
    },
    PositionOffsetRandom {
        control_point: usize,
        offset_min: [f32; 3],
        offset_max: [f32; 3],
        proportional_to_radius: bool,
    },
    PositionWarpRandom {
        control_point: usize,
        warp_min: [f32; 3],
        warp_max: [f32; 3],
    },
    PositionAlongPath {
        start_control_point: usize,
        end_control_point: usize,
        sequential_count: Option<f32>,
    },
    PositionFromParentParticles {
        inherited_velocity_scale: f32,
    },
    MoveBetweenControlPoints {
        end_control_point: usize,
        speed_min: f32,
        speed_max: f32,
        start_offset: f32,
        end_spread: f32,
    },
    VelocityRandom {
        speed_min: f32,
        speed_max: f32,
        local_min: [f32; 3],
        local_max: [f32; 3],
    },
    VelocityNoise {
        output_min: [f32; 3],
        output_max: [f32; 3],
        spatial_scale: f32,
        time_scale: f32,
    },
    SequenceRandom {
        min: i32,
        max: i32,
        second: bool,
    },
    TrailLengthRandom {
        min: f32,
        max: f32,
        exponent: f32,
    },
    RemapInitialScalar {
        input_min: f32,
        input_max: f32,
        output_field: ScalarField,
        output_min: f32,
        output_max: f32,
        /// Input is the particle's creation time relative to emission start.
        scale_initial: bool,
    },
    InitialScalarNoise {
        output_field: ScalarField,
        output_min: f32,
        output_max: f32,
        spatial_scale: f32,
        time_scale: f32,
    },
}

#[derive(Debug, Clone)]
pub(super) enum Operator {
    LifespanDecay,
    AlphaFadeIn {
        time_min: f32,
        time_max: f32,
        proportional: bool,
    },
    AlphaFadeOut {
        time_min: f32,
        time_max: f32,
        proportional: bool,
        ease: bool,
        fade_bias: f32,
    },
    AlphaFadeAndDecay {
        start_alpha: f32,
        end_alpha: f32,
        start_fade_in: f32,
        end_fade_in: f32,
        start_fade_out: f32,
        end_fade_out: f32,
    },
    RadiusScale {
        start_time: f32,
        end_time: f32,
        start_scale: f32,
        end_scale: f32,
        ease: bool,
        scale_bias: f32,
    },
    ColorFade {
        target: [f32; 3],
        start_time: f32,
        end_time: f32,
        ease: bool,
    },
    RotationSpin {
        rate_radians: f32,
        stop_time: f32,
    },
    RotationBasic,
    MovementBasic {
        gravity: [f32; 3],
        drag: f32,
    },
    MovementLockToControlPoint {
        control_point: usize,
    },
    OscillateScalar {
        field: ScalarField,
        rate_min: f32,
        rate_max: f32,
        frequency_min: f32,
        frequency_max: f32,
        proportional: bool,
        multiplier: f32,
        start_phase: f32,
    },
    OscillateVector {
        field: VectorField,
        rate_min: [f32; 3],
        rate_max: [f32; 3],
        frequency_min: [f32; 3],
        frequency_max: [f32; 3],
        proportional: bool,
        multiplier: f32,
        start_phase: f32,
    },
    NoiseVector {
        field: VectorField,
        output_min: [f32; 3],
        output_max: [f32; 3],
        coordinate_scale: f32,
    },
    RemapNoiseToScalar {
        field: ScalarField,
        output_min: f32,
        output_max: f32,
        time_scale: f32,
        spatial_scale: f32,
    },
    ConstrainDistanceToControlPoint {
        control_point: usize,
        min_distance: f32,
        max_distance: f32,
        offset: [f32; 3],
    },
    ConstrainDistanceToPath {
        start_control_point: usize,
        end_control_point: usize,
        max_distance: f32,
    },
    SetControlPointPositions {
        base_control_point: usize,
        points: Vec<(usize, [f32; 3])>,
    },
    SetChildControlPointsFromParticles {
        first_control_point: usize,
        count: usize,
        first_particle: usize,
    },
}

#[derive(Debug, Clone)]
pub(super) enum Force {
    Random {
        min: [f32; 3],
        max: [f32; 3],
    },
    PullTowardsControlPoint {
        control_point: usize,
        amount: f32,
        falloff_power: f32,
    },
    TwistAroundAxis {
        axis: [f32; 3],
        amount: f32,
        control_point: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererKind {
    /// Camera-facing sprites; the workhorse.
    AnimatedSprites,
    /// Velocity-stretched quads.
    SpriteTrail,
    /// A ribbon threaded through the live particles in spawn order.
    Rope,
}

#[derive(Debug, Clone)]
pub struct RendererInfo {
    pub kind: RendererKind,
    /// Trail length is expressed in seconds of motion.
    pub trail_length_fade_in: f32,
    pub trail_min_length: f32,
    pub trail_max_length: f32,
    pub rope_subdivisions: u32,
    /// Sprite sheet playback speed multiplier (or FPS, see below).
    pub animation_rate: f32,
    /// Stretch the sheet sequence over the particle's whole lifetime.
    pub animation_fit_lifetime: bool,
    /// Interpret `animation_rate` as frames per second instead of a
    /// sequence-time multiplier.
    pub animation_rate_is_fps: bool,
}

impl Default for RendererInfo {
    fn default() -> Self {
        Self {
            kind: RendererKind::AnimatedSprites,
            trail_length_fade_in: 0.0,
            trail_min_length: 0.0,
            trail_max_length: 2000.0,
            rope_subdivisions: 3,
            animation_rate: 0.1,
            animation_fit_lifetime: false,
            animation_rate_is_fps: false,
        }
    }
}

// --- Compiled system -------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct CompiledChild {
    pub(super) system: usize,
    pub(super) delay: f32,
}

#[derive(Debug, Clone)]
pub struct CompiledSystem {
    pub(super) name: String,
    pub(super) material: String,
    pub(super) max_particles: usize,
    pub(super) initial_particles: u32,
    pub(super) constant_color: [f32; 3],
    pub(super) constant_alpha: f32,
    pub(super) constant_radius: f32,
    pub(super) constant_rotation: f32,
    pub(super) constant_rotation_speed: f32,
    pub(super) constant_sequence: i32,
    pub(super) maximum_time_step: f32,
    pub(super) emitters: Vec<Emitter>,
    pub(super) initializers: Vec<Initializer>,
    pub(super) operators: Vec<Operator>,
    pub(super) forces: Vec<Force>,
    pub(super) renderer: RendererInfo,
    pub(super) children: Vec<CompiledChild>,
    pub(super) coverage: Vec<CoverageEntry>,
    /// Highest control point index any compiled op reads.
    pub(super) highest_control_point: usize,
    /// Rough world-space extent for camera framing.
    pub(super) bounding_radius: f32,
}

impl CompiledSystem {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn material(&self) -> &str {
        &self.material
    }

    pub fn coverage(&self) -> &[CoverageEntry] {
        &self.coverage
    }

    pub fn renderer(&self) -> &RendererInfo {
        &self.renderer
    }
}

/// Normalizes a PCF function name: the authoritative `functionName`
/// attribute when present, lowercased, underscores as spaces.
pub(super) fn canonical_name(function: &PcfFunction) -> String {
    function
        .attributes
        .get_string("functionName")
        .unwrap_or(&function.name)
        .to_ascii_lowercase()
        .replace('_', " ")
}

pub(super) struct SystemCompiler {
    pub(super) coverage: Vec<CoverageEntry>,
    pub(super) highest_control_point: usize,
}

impl SystemCompiler {
    fn record(&mut self, function: &PcfFunction, list: &'static str, level: SupportLevel) {
        self.coverage.push(CoverageEntry {
            function: function
                .attributes
                .get_string("functionName")
                .unwrap_or(&function.name)
                .to_owned(),
            list,
            level,
        });
    }

    fn control_point(&mut self, attrs: &PcfAttributes, names: &[&str]) -> usize {
        let index = names
            .iter()
            .find_map(|name| attrs.get_int(name))
            .unwrap_or(0)
            .clamp(0, (MAX_CONTROL_POINTS - 1) as i32) as usize;
        self.highest_control_point = self.highest_control_point.max(index);
        index
    }

    fn float(attrs: &PcfAttributes, name: &str, default: f32) -> f32 {
        attrs.get_float(name).unwrap_or(default)
    }

    fn vec3(attrs: &PcfAttributes, name: &str, default: [f32; 3]) -> [f32; 3] {
        attrs.get_vector3(name).unwrap_or(default)
    }
}

pub(super) fn compile_system(
    definition: &PcfSystem,
    system_indices: &[Option<usize>],
) -> CompiledSystem {
    let attrs = &definition.attributes;
    let mut compiler = SystemCompiler {
        coverage: Vec::new(),
        highest_control_point: 0,
    };

    let mut emitters = Vec::new();
    for function in &definition.emitters {
        match compile_emitter(&mut compiler, function) {
            Some(emitter) => emitters.push(emitter),
            None => compiler.record(function, "emitters", SupportLevel::Unsupported),
        }
    }

    let mut initializers = Vec::new();
    for function in &definition.initializers {
        compile_initializer(&mut compiler, function, &mut initializers);
    }

    let mut operators = Vec::new();
    for function in &definition.operators {
        compile_operator(&mut compiler, function, &mut operators);
    }

    let mut forces = Vec::new();
    for function in &definition.forces {
        compile_force(&mut compiler, function, &mut forces);
    }

    for function in &definition.constraints {
        compile_constraint(&mut compiler, function, &mut operators);
    }

    let renderer = compile_renderer(&mut compiler, &definition.renderers);

    let children = definition
        .children
        .iter()
        .filter_map(|child| {
            let system = child.system_index?;
            system_indices
                .get(system)
                .copied()
                .flatten()
                .map(|system| CompiledChild {
                    system,
                    delay: child.delay.max(0.0),
                })
        })
        .collect();

    let color = attrs.get_color("color").unwrap_or([255, 255, 255, 255]);
    // Framing: declared bounding box, spawn-sphere reach, and a nod to how
    // far the fastest sphere-spawned particles travel in half a second.
    let bbox_min = SystemCompiler::vec3(attrs, "bounding_box_min", [-10.0; 3]);
    let bbox_max = SystemCompiler::vec3(attrs, "bounding_box_max", [10.0; 3]);
    let bbox_extent = bbox_min
        .iter()
        .chain(bbox_max.iter())
        .fold(0.0_f32, |acc, value| acc.max(value.abs()));
    let spawn_reach = initializers
        .iter()
        .map(|initializer| match initializer {
            Initializer::PositionWithinSphere {
                distance_max,
                speed_max,
                ..
            } => distance_max + speed_max * 0.5,
            Initializer::PositionOffsetRandom {
                offset_min,
                offset_max,
                ..
            } => length(*offset_min).max(length(*offset_max)),
            _ => 0.0,
        })
        .fold(0.0_f32, f32::max);
    CompiledSystem {
        name: definition.name.clone(),
        material: definition.material().unwrap_or("").to_owned(),
        max_particles: definition.max_particles() as usize,
        initial_particles: attrs.get_int("initial_particles").unwrap_or(0).max(0) as u32,
        constant_color: color_to_rgb(color),
        constant_alpha: f32::from(color[3]) / 255.0,
        constant_radius: SystemCompiler::float(attrs, "radius", 5.0),
        constant_rotation: SystemCompiler::float(attrs, "rotation", 0.0).to_radians(),
        constant_rotation_speed: SystemCompiler::float(attrs, "rotation_speed", 0.0).to_radians(),
        constant_sequence: attrs.get_int("sequence_number").unwrap_or(0),
        maximum_time_step: SystemCompiler::float(attrs, "maximum time step", 0.1).clamp(0.01, 0.5),
        emitters,
        initializers,
        operators,
        forces,
        renderer,
        children,
        coverage: compiler.coverage,
        highest_control_point: compiler.highest_control_point,
        bounding_radius: bbox_extent.max(spawn_reach).max(24.0),
    }
}

pub(super) fn compile_emitter(
    compiler: &mut SystemCompiler,
    function: &PcfFunction,
) -> Option<Emitter> {
    let attrs = &function.attributes;
    let name = canonical_name(function);
    let emitter = match name.as_str() {
        "emit continuously" => Emitter::Continuously {
            start_time: SystemCompiler::float(attrs, "emission_start_time", 0.0),
            rate: SystemCompiler::float(attrs, "emission_rate", 100.0).max(0.0),
            duration: SystemCompiler::float(attrs, "emission_duration", 0.0).max(0.0),
        },
        "emit instantaneously" => Emitter::Instantaneously {
            start_time: SystemCompiler::float(attrs, "emission_start_time", 0.0),
            count: attrs.get_int("num_to_emit").unwrap_or(1).max(0) as u32,
        },
        "emit noise" => Emitter::Noise {
            start_time: SystemCompiler::float(attrs, "emission_start_time", 0.0),
            duration: SystemCompiler::float(attrs, "emission_duration", 0.0).max(0.0),
            minimum: SystemCompiler::float(attrs, "emission minimum", 0.0).max(0.0),
            maximum: SystemCompiler::float(attrs, "emission maximum", 100.0).max(0.0),
            time_scale: SystemCompiler::float(attrs, "time noise coordinate scale", 0.1),
        },
        _ => return None,
    };
    let level = if matches!(emitter, Emitter::Noise { .. }) {
        SupportLevel::Approximate
    } else {
        SupportLevel::Full
    };
    compiler.record(function, "emitters", level);
    Some(emitter)
}

pub(super) fn compile_initializer(
    compiler: &mut SystemCompiler,
    function: &PcfFunction,
    out: &mut Vec<Initializer>,
) {
    let attrs = &function.attributes;
    let name = canonical_name(function);
    let (initializer, level) = match name.as_str() {
        "lifetime random" => (
            Initializer::LifetimeRandom {
                min: SystemCompiler::float(attrs, "lifetime_min", 1.0),
                max: SystemCompiler::float(attrs, "lifetime_max", 1.0),
                exponent: SystemCompiler::float(attrs, "lifetime_random_exponent", 1.0),
            },
            SupportLevel::Full,
        ),
        "lifetime pre-age noise" => (
            Initializer::PreAge {
                min: SystemCompiler::float(attrs, "start age minimum", 0.0),
                max: SystemCompiler::float(attrs, "start age maximum", 0.0),
            },
            SupportLevel::Approximate,
        ),
        "alpha random" => (
            Initializer::AlphaRandom {
                min: SystemCompiler::float(attrs, "alpha_min", 255.0) / 255.0,
                max: SystemCompiler::float(attrs, "alpha_max", 255.0) / 255.0,
                exponent: SystemCompiler::float(attrs, "alpha_random_exponent", 1.0),
            },
            SupportLevel::Full,
        ),
        "color random" => (
            Initializer::ColorRandom {
                color1: color_to_rgb(attrs.get_color("color1").unwrap_or([255, 255, 255, 255])),
                color2: color_to_rgb(attrs.get_color("color2").unwrap_or([255, 255, 255, 255])),
            },
            SupportLevel::Full,
        ),
        "radius random" => (
            Initializer::RadiusRandom {
                min: SystemCompiler::float(attrs, "radius_min", 1.0),
                max: SystemCompiler::float(attrs, "radius_max", 1.0),
                exponent: SystemCompiler::float(attrs, "radius_random_exponent", 1.0),
            },
            SupportLevel::Full,
        ),
        "rotation random" => (
            Initializer::RotationRandom {
                initial: SystemCompiler::float(attrs, "rotation_initial", 0.0).to_radians(),
                offset_min: SystemCompiler::float(attrs, "rotation_offset_min", 0.0).to_radians(),
                offset_max: SystemCompiler::float(attrs, "rotation_offset_max", 360.0).to_radians(),
                exponent: SystemCompiler::float(attrs, "rotation_random_exponent", 1.0),
            },
            SupportLevel::Full,
        ),
        "rotation speed random" => (
            Initializer::RotationSpeedRandom {
                constant: SystemCompiler::float(attrs, "rotation_speed_constant", 0.0).to_radians(),
                min: SystemCompiler::float(attrs, "rotation_speed_random_min", 0.0).to_radians(),
                max: SystemCompiler::float(attrs, "rotation_speed_random_max", 0.0).to_radians(),
                exponent: SystemCompiler::float(attrs, "rotation_speed_random_exponent", 1.0),
                random_flip: attrs.get_bool("randomly_flip_direction").unwrap_or(true),
            },
            SupportLevel::Full,
        ),
        "rotation yaw flip random" => (
            Initializer::YawFlipRandom {
                percentage: SystemCompiler::float(attrs, "Flip Percentage", 0.5),
            },
            SupportLevel::Full,
        ),
        "position within sphere random" | "position within sphere" => (
            Initializer::PositionWithinSphere {
                control_point: compiler.control_point(attrs, &["control_point_number"]),
                distance_min: SystemCompiler::float(attrs, "distance_min", 0.0),
                distance_max: SystemCompiler::float(attrs, "distance_max", 0.0),
                bias: SystemCompiler::vec3(attrs, "distance_bias", [1.0, 1.0, 1.0]),
                speed_min: SystemCompiler::float(attrs, "speed_min", 0.0),
                speed_max: SystemCompiler::float(attrs, "speed_max", 0.0),
                speed_exponent: SystemCompiler::float(attrs, "speed_random_exponent", 1.0),
                local_speed_min: SystemCompiler::vec3(
                    attrs,
                    "speed_in_local_coordinate_system_min",
                    [0.0; 3],
                ),
                local_speed_max: SystemCompiler::vec3(
                    attrs,
                    "speed_in_local_coordinate_system_max",
                    [0.0; 3],
                ),
            },
            SupportLevel::Full,
        ),
        "position offset random" | "position modify offset random" => (
            Initializer::PositionOffsetRandom {
                control_point: compiler.control_point(attrs, &["control_point_number"]),
                offset_min: SystemCompiler::vec3(attrs, "offset min", [0.0; 3]),
                offset_max: SystemCompiler::vec3(attrs, "offset max", [0.0; 3]),
                proportional_to_radius: attrs
                    .get_bool("offset proportional to radius 0/1")
                    .unwrap_or(false),
            },
            SupportLevel::Full,
        ),
        "position warp random" | "position modify warp random" => (
            Initializer::PositionWarpRandom {
                control_point: compiler
                    .control_point(attrs, &["control point number", "control_point_number"]),
                warp_min: SystemCompiler::vec3(attrs, "warp min", [1.0; 3]),
                warp_max: SystemCompiler::vec3(attrs, "warp max", [1.0; 3]),
            },
            SupportLevel::Approximate,
        ),
        "position along path random" | "position along path sequential" => (
            Initializer::PositionAlongPath {
                start_control_point: compiler.control_point(attrs, &["start control point number"]),
                end_control_point: compiler.control_point(attrs, &["end control point number"]),
                sequential_count: (name == "position along path sequential").then(|| {
                    SystemCompiler::float(attrs, "particles to map from start to end", 10.0)
                        .max(1.0)
                }),
            },
            SupportLevel::Approximate,
        ),
        "position from parent particles" => (
            Initializer::PositionFromParentParticles {
                inherited_velocity_scale: SystemCompiler::float(
                    attrs,
                    "Inherited Velocity Scale",
                    0.0,
                ),
            },
            SupportLevel::Full,
        ),
        "move particles between 2 control points" => (
            Initializer::MoveBetweenControlPoints {
                end_control_point: compiler.control_point(attrs, &["end control point"]),
                speed_min: SystemCompiler::float(attrs, "minimum speed", 0.0),
                speed_max: SystemCompiler::float(attrs, "maximum speed", 0.0),
                start_offset: SystemCompiler::float(attrs, "start offset", 0.0),
                end_spread: SystemCompiler::float(attrs, "end spread", 0.0),
            },
            SupportLevel::Full,
        ),
        "velocity random" => (
            Initializer::VelocityRandom {
                speed_min: SystemCompiler::float(attrs, "random_speed_min", 0.0),
                speed_max: SystemCompiler::float(attrs, "random_speed_max", 0.0),
                local_min: SystemCompiler::vec3(
                    attrs,
                    "speed_in_local_coordinate_system_min",
                    [0.0; 3],
                ),
                local_max: SystemCompiler::vec3(
                    attrs,
                    "speed_in_local_coordinate_system_max",
                    [0.0; 3],
                ),
            },
            SupportLevel::Full,
        ),
        "velocity noise" => (
            Initializer::VelocityNoise {
                output_min: SystemCompiler::vec3(attrs, "output minimum", [0.0; 3]),
                output_max: SystemCompiler::vec3(attrs, "output maximum", [0.0; 3]),
                spatial_scale: SystemCompiler::float(attrs, "Spatial Noise Coordinate Scale", 0.01),
                time_scale: SystemCompiler::float(attrs, "Time Noise Coordinate Scale", 1.0),
            },
            SupportLevel::Approximate,
        ),
        "velocity inherit from control point" => {
            // Control points are static in the preview unless dragged; the
            // spawn-time contribution is effectively zero either way.
            compiler.record(function, "initializers", SupportLevel::PreviewInert);
            return;
        }
        "sequence random" | "sequence two random" => (
            Initializer::SequenceRandom {
                min: attrs.get_int("sequence_min").unwrap_or(0),
                max: attrs.get_int("sequence_max").unwrap_or(0),
                second: name == "sequence two random",
            },
            SupportLevel::Full,
        ),
        "trail length random" => (
            Initializer::TrailLengthRandom {
                min: SystemCompiler::float(attrs, "length_min", 0.1),
                max: SystemCompiler::float(attrs, "length_max", 0.1),
                exponent: SystemCompiler::float(attrs, "length_random_exponent", 1.0),
            },
            SupportLevel::Full,
        ),
        "remap initial scalar" => {
            match ScalarField::from_id(attrs.get_int("output field").unwrap_or(-1)) {
                Some(output_field) if attrs.get_int("input field").unwrap_or(-1) == 8 => (
                    Initializer::RemapInitialScalar {
                        input_min: SystemCompiler::float(attrs, "input minimum", 0.0),
                        input_max: SystemCompiler::float(attrs, "input maximum", 1.0),
                        output_field,
                        output_min: SystemCompiler::float(attrs, "output minimum", 0.0),
                        output_max: SystemCompiler::float(attrs, "output maximum", 1.0),
                        scale_initial: attrs
                            .get_bool("output is scalar of initial random range")
                            .unwrap_or(false),
                    },
                    SupportLevel::Approximate,
                ),
                _ => {
                    compiler.record(function, "initializers", SupportLevel::Unsupported);
                    return;
                }
            }
        }
        "initial scalar noise" => {
            let Some(output_field) =
                ScalarField::from_id(attrs.get_int("output field").unwrap_or(-1))
            else {
                compiler.record(function, "initializers", SupportLevel::Unsupported);
                return;
            };
            (
                Initializer::InitialScalarNoise {
                    output_field,
                    output_min: SystemCompiler::float(attrs, "output minimum", 0.0),
                    output_max: SystemCompiler::float(attrs, "output maximum", 1.0),
                    spatial_scale: SystemCompiler::float(
                        attrs,
                        "spatial noise coordinate scale",
                        0.001,
                    ),
                    time_scale: SystemCompiler::float(attrs, "time noise coordinate scale", 1.0),
                },
                SupportLevel::Approximate,
            )
        }
        "lifetime from sequence" | "sequence lifetime" => {
            compiler.record(function, "initializers", SupportLevel::Unsupported);
            return;
        }
        _ => {
            compiler.record(function, "initializers", SupportLevel::Unsupported);
            return;
        }
    };
    compiler.record(function, "initializers", level);
    out.push(initializer);
}

pub(super) fn compile_operator(
    compiler: &mut SystemCompiler,
    function: &PcfFunction,
    out: &mut Vec<Operator>,
) {
    let attrs = &function.attributes;
    let name = canonical_name(function);
    let (operator, level) = match name.as_str() {
        "lifespan decay" => (Operator::LifespanDecay, SupportLevel::Full),
        "alpha fade in random" => (
            Operator::AlphaFadeIn {
                time_min: SystemCompiler::float(attrs, "fade in time min", 0.25),
                time_max: SystemCompiler::float(attrs, "fade in time max", 0.25),
                proportional: attrs.get_bool("proportional 0/1").unwrap_or(true),
            },
            SupportLevel::Full,
        ),
        "alpha fade out random" => (
            Operator::AlphaFadeOut {
                time_min: SystemCompiler::float(attrs, "fade out time min", 0.25),
                time_max: SystemCompiler::float(attrs, "fade out time max", 0.25),
                proportional: attrs.get_bool("proportional 0/1").unwrap_or(true),
                ease: attrs.get_bool("ease in and out").unwrap_or(false),
                fade_bias: SystemCompiler::float(attrs, "fade bias", 0.5),
            },
            SupportLevel::Full,
        ),
        "alpha fade and decay" => (
            Operator::AlphaFadeAndDecay {
                start_alpha: SystemCompiler::float(attrs, "start_alpha", 1.0),
                end_alpha: SystemCompiler::float(attrs, "end_alpha", 0.0),
                start_fade_in: SystemCompiler::float(attrs, "start_fade_in_time", 0.0),
                end_fade_in: SystemCompiler::float(attrs, "end_fade_in_time", 0.5),
                start_fade_out: SystemCompiler::float(attrs, "start_fade_out_time", 0.5),
                end_fade_out: SystemCompiler::float(attrs, "end_fade_out_time", 1.0),
            },
            SupportLevel::Full,
        ),
        "radius scale" => (
            Operator::RadiusScale {
                start_time: SystemCompiler::float(attrs, "start_time", 0.0),
                end_time: SystemCompiler::float(attrs, "end_time", 1.0),
                start_scale: SystemCompiler::float(attrs, "radius_start_scale", 1.0),
                end_scale: SystemCompiler::float(attrs, "radius_end_scale", 1.0),
                ease: attrs.get_bool("ease_in_and_out").unwrap_or(false),
                scale_bias: SystemCompiler::float(attrs, "scale_bias", 0.5),
            },
            SupportLevel::Full,
        ),
        "color fade" => (
            Operator::ColorFade {
                target: color_to_rgb(
                    attrs
                        .get_color("color_fade")
                        .unwrap_or([255, 255, 255, 255]),
                ),
                start_time: SystemCompiler::float(attrs, "fade_start_time", 0.0),
                end_time: SystemCompiler::float(attrs, "fade_end_time", 1.0),
                ease: attrs.get_bool("ease_in_and_out").unwrap_or(true),
            },
            SupportLevel::Full,
        ),
        "rotation spin roll" | "rotation spin" => (
            Operator::RotationSpin {
                rate_radians: SystemCompiler::float(attrs, "spin_rate_degrees", 0.0).to_radians(),
                stop_time: SystemCompiler::float(attrs, "spin_stop_time", 0.0),
            },
            SupportLevel::Full,
        ),
        "rotation basic" => (Operator::RotationBasic, SupportLevel::Full),
        "movement basic" | "basic movement" => (
            Operator::MovementBasic {
                gravity: SystemCompiler::vec3(attrs, "gravity", [0.0; 3]),
                drag: SystemCompiler::float(attrs, "drag", 0.0),
            },
            SupportLevel::Full,
        ),
        "movement lock to control point" => (
            Operator::MovementLockToControlPoint {
                control_point: compiler.control_point(attrs, &["control_point_number"]),
            },
            SupportLevel::Approximate,
        ),
        "oscillate scalar" => {
            let Some(field) =
                ScalarField::from_id(attrs.get_int("oscillation field").unwrap_or(-1))
            else {
                compiler.record(function, "operators", SupportLevel::Unsupported);
                return;
            };
            (
                Operator::OscillateScalar {
                    field,
                    rate_min: SystemCompiler::float(attrs, "oscillation rate min", 0.0),
                    rate_max: SystemCompiler::float(attrs, "oscillation rate max", 0.0),
                    frequency_min: SystemCompiler::float(attrs, "oscillation frequency min", 1.0),
                    frequency_max: SystemCompiler::float(attrs, "oscillation frequency max", 1.0),
                    proportional: attrs.get_bool("proportional 0/1").unwrap_or(true),
                    multiplier: SystemCompiler::float(attrs, "oscillation multiplier", 2.0),
                    start_phase: SystemCompiler::float(attrs, "oscillation start phase", 0.5),
                },
                SupportLevel::Approximate,
            )
        }
        "oscillate vector" => {
            let Some(field) =
                VectorField::from_id(attrs.get_int("oscillation field").unwrap_or(-1))
            else {
                compiler.record(function, "operators", SupportLevel::Unsupported);
                return;
            };
            (
                Operator::OscillateVector {
                    field,
                    rate_min: SystemCompiler::vec3(attrs, "oscillation rate min", [0.0; 3]),
                    rate_max: SystemCompiler::vec3(attrs, "oscillation rate max", [0.0; 3]),
                    frequency_min: SystemCompiler::vec3(
                        attrs,
                        "oscillation frequency min",
                        [1.0; 3],
                    ),
                    frequency_max: SystemCompiler::vec3(
                        attrs,
                        "oscillation frequency max",
                        [1.0; 3],
                    ),
                    proportional: attrs.get_bool("proportional 0/1").unwrap_or(true),
                    multiplier: SystemCompiler::float(attrs, "oscillation multiplier", 2.0),
                    start_phase: SystemCompiler::float(attrs, "oscillation start phase", 0.5),
                },
                SupportLevel::Approximate,
            )
        }
        "noise vector" => {
            let Some(field) = VectorField::from_id(attrs.get_int("output field").unwrap_or(-1))
            else {
                compiler.record(function, "operators", SupportLevel::Unsupported);
                return;
            };
            (
                Operator::NoiseVector {
                    field,
                    output_min: SystemCompiler::vec3(attrs, "output minimum", [0.0; 3]),
                    output_max: SystemCompiler::vec3(attrs, "output maximum", [0.0; 3]),
                    coordinate_scale: SystemCompiler::float(attrs, "noise coordinate scale", 1.0),
                },
                SupportLevel::Approximate,
            )
        }
        "remap noise to scalar" => {
            let Some(field) = ScalarField::from_id(attrs.get_int("output field").unwrap_or(-1))
            else {
                compiler.record(function, "operators", SupportLevel::Unsupported);
                return;
            };
            (
                Operator::RemapNoiseToScalar {
                    field,
                    output_min: SystemCompiler::float(attrs, "output minimum", 0.0),
                    output_max: SystemCompiler::float(attrs, "output maximum", 1.0),
                    time_scale: SystemCompiler::float(attrs, "time noise coordinate scale", 1.0),
                    spatial_scale: SystemCompiler::float(
                        attrs,
                        "spatial noise coordinate scale",
                        1.0,
                    ),
                },
                SupportLevel::Approximate,
            )
        }
        "set control point positions" => {
            let mut points = Vec::new();
            for (number_key, location_key) in [
                ("First Control Point Number", "First Control Point Location"),
                (
                    "Second Control Point Number",
                    "Second Control Point Location",
                ),
                ("Third Control Point Number", "Third Control Point Location"),
                (
                    "Fourth Control Point Number",
                    "Fourth Control Point Location",
                ),
            ] {
                let index = compiler.control_point(attrs, &[number_key]);
                let location = SystemCompiler::vec3(attrs, location_key, [0.0; 3]);
                points.push((index, location));
            }
            (
                Operator::SetControlPointPositions {
                    base_control_point: compiler
                        .control_point(attrs, &["Control Point to offset positions from"]),
                    points,
                },
                SupportLevel::Approximate,
            )
        }
        "set child control points from particle positions" => (
            Operator::SetChildControlPointsFromParticles {
                first_control_point: compiler.control_point(attrs, &["First control point to set"]),
                count: attrs
                    .get_int("# of control points to set")
                    .unwrap_or(1)
                    .clamp(0, MAX_CONTROL_POINTS as i32) as usize,
                first_particle: attrs.get_int("first particle to copy").unwrap_or(0).max(0)
                    as usize,
            },
            SupportLevel::Approximate,
        ),
        "movement dampen relative to control point" => {
            compiler.record(function, "operators", SupportLevel::Unsupported);
            return;
        }
        "collision via traces"
        | "prevent passing through static part of world"
        | "velocity repulse from world"
        | "movement lock to bone" => {
            compiler.record(function, "operators", SupportLevel::PreviewInert);
            return;
        }
        "color light from control point" | "color lit per particle" => {
            compiler.record(function, "operators", SupportLevel::PreviewInert);
            return;
        }
        "rotation spin yaw" | "rotation yaw random" => {
            // Billboards in the preview have no yaw axis to spin.
            compiler.record(function, "operators", SupportLevel::PreviewInert);
            return;
        }
        _ => {
            compiler.record(function, "operators", SupportLevel::Unsupported);
            return;
        }
    };
    compiler.record(function, "operators", level);
    out.push(operator);
}

pub(super) fn compile_force(
    compiler: &mut SystemCompiler,
    function: &PcfFunction,
    out: &mut Vec<Force>,
) {
    let attrs = &function.attributes;
    let name = canonical_name(function);
    let (force, level) = match name.as_str() {
        "random force" => (
            Force::Random {
                min: SystemCompiler::vec3(attrs, "min force", [0.0; 3]),
                max: SystemCompiler::vec3(attrs, "max force", [0.0; 3]),
            },
            SupportLevel::Full,
        ),
        "pull towards control point" => (
            Force::PullTowardsControlPoint {
                control_point: compiler.control_point(attrs, &["control point number"]),
                amount: SystemCompiler::float(attrs, "amount of force", 0.0),
                falloff_power: SystemCompiler::float(attrs, "falloff power", 2.0),
            },
            SupportLevel::Full,
        ),
        "twist around axis" => (
            Force::TwistAroundAxis {
                axis: SystemCompiler::vec3(attrs, "twist axis", [0.0, 0.0, 1.0]),
                amount: SystemCompiler::float(attrs, "amount of force", 0.0),
                control_point: compiler.control_point(attrs, &["control point number"]),
            },
            SupportLevel::Approximate,
        ),
        _ => {
            compiler.record(function, "forces", SupportLevel::Unsupported);
            return;
        }
    };
    compiler.record(function, "forces", level);
    out.push(force);
}

pub(super) fn compile_constraint(
    compiler: &mut SystemCompiler,
    function: &PcfFunction,
    out: &mut Vec<Operator>,
) {
    let attrs = &function.attributes;
    let name = canonical_name(function);
    let (operator, level) = match name.as_str() {
        "constrain distance to control point" => (
            Operator::ConstrainDistanceToControlPoint {
                control_point: compiler.control_point(attrs, &["control point number"]),
                min_distance: SystemCompiler::float(attrs, "minimum distance", 0.0),
                max_distance: SystemCompiler::float(attrs, "maximum distance", 0.0),
                offset: SystemCompiler::vec3(attrs, "offset of center", [0.0; 3]),
            },
            SupportLevel::Full,
        ),
        "constrain distance to path between two control points" => (
            Operator::ConstrainDistanceToPath {
                start_control_point: compiler.control_point(attrs, &["start control point number"]),
                end_control_point: compiler.control_point(attrs, &["end control point number"]),
                max_distance: SystemCompiler::float(attrs, "maximum distance", 0.0),
            },
            SupportLevel::Approximate,
        ),
        "collision via traces" | "prevent passing through static part of world" => {
            compiler.record(function, "constraints", SupportLevel::PreviewInert);
            return;
        }
        _ => {
            compiler.record(function, "constraints", SupportLevel::Unsupported);
            return;
        }
    };
    compiler.record(function, "constraints", level);
    out.push(operator);
}

pub(super) fn compile_renderer(
    compiler: &mut SystemCompiler,
    renderers: &[PcfFunction],
) -> RendererInfo {
    let mut info = RendererInfo::default();
    let mut chosen = false;
    for function in renderers {
        let attrs = &function.attributes;
        let name = canonical_name(function);
        match name.as_str() {
            "render animated sprites" => {
                if !chosen {
                    info.kind = RendererKind::AnimatedSprites;
                    info.animation_rate = SystemCompiler::float(attrs, "animation rate", 0.1);
                    info.animation_fit_lifetime =
                        attrs.get_bool("animation_fit_lifetime").unwrap_or(false);
                    info.animation_rate_is_fps =
                        attrs.get_bool("use animation rate as FPS").unwrap_or(false);
                    chosen = true;
                }
                compiler.record(function, "renderers", SupportLevel::Approximate);
            }
            "render sprite trail" => {
                if !chosen {
                    info.kind = RendererKind::SpriteTrail;
                    info.trail_length_fade_in =
                        SystemCompiler::float(attrs, "length fade in time", 0.0);
                    info.trail_min_length = SystemCompiler::float(attrs, "min length", 0.0);
                    info.trail_max_length = SystemCompiler::float(attrs, "max length", 2000.0);
                    chosen = true;
                }
                compiler.record(function, "renderers", SupportLevel::Approximate);
            }
            "render rope" => {
                if !chosen {
                    info.kind = RendererKind::Rope;
                    info.rope_subdivisions =
                        attrs.get_int("subdivision_count").unwrap_or(3).clamp(1, 16) as u32;
                    chosen = true;
                }
                compiler.record(function, "renderers", SupportLevel::Approximate);
            }
            _ => {
                // Unknown renderers fall back to sprites so the sim is still
                // visible; coverage tells the user the look is wrong.
                compiler.record(function, "renderers", SupportLevel::Unsupported);
            }
        }
    }
    info
}
