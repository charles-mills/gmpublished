//! Shared Source engine particle system simulation.
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
mod initializers;
mod lifecycle;
mod operators;
mod rendering;
mod rng;
mod storage;

pub use compiler::{CompiledSystem, RendererInfo, RendererKind};
use compiler::{Emitter, Force, Initializer, Operator, ScalarField, VectorField, compile_system};
pub use rendering::{InstanceRender, RenderParticle, RenderParticles};
use rng::{Rng, deterministic_range, value_noise};
use storage::ParticleSet;

use crate::math::{Vec3, simple_spline};

/// Canonical material identity shared by particle loading, compilation, and
/// per-frame rendering.
pub fn normalize_material_name(name: &str) -> String {
    let name = name.trim();
    let bytes = name.as_bytes();
    let start = if bytes.len() >= 10
        && bytes[..9].eq_ignore_ascii_case(b"materials")
        && matches!(bytes[9], b'/' | b'\\')
    {
        10
    } else {
        0
    };
    let end = if bytes.len().saturating_sub(start) >= 4
        && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".vmt")
    {
        bytes.len() - 4
    } else {
        bytes.len()
    };
    let mut normalized = bytes[start..end].to_vec();
    for byte in &mut normalized {
        if *byte == b'\\' {
            *byte = b'/';
        } else {
            byte.make_ascii_lowercase();
        }
    }
    String::from_utf8(normalized).expect("normalizing valid UTF-8 preserves validity")
}

pub const MAX_CONTROL_POINTS: usize = 8;

/// An index into a system's control points, guaranteed in range.
///
/// PCF files name control points freely, so the clamp happens once, here. Every
/// path that reaches a control point — the compiler, reads, writes — therefore
/// resolves an out-of-range number the same way.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ControlPointIndex(usize);

impl ControlPointIndex {
    /// Clamps into range: content naming control point 40 gets the last one
    /// rather than being dropped, so a wild index degrades rather than
    /// silently removing the effect that used it.
    #[must_use]
    pub const fn clamped(index: usize) -> Self {
        if index >= MAX_CONTROL_POINTS {
            Self(MAX_CONTROL_POINTS - 1)
        } else {
            Self(index)
        }
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// Every valid index, ascending. The only way to iterate control points
    /// without a `usize` that has to be re-bounded at each use.
    pub fn all() -> impl DoubleEndedIterator<Item = Self> + ExactSizeIterator {
        (0..MAX_CONTROL_POINTS).map(Self)
    }
}
/// How deep a chain of child systems may go. Separate from the instance-count
/// ceiling, which bounds how many instances a file produces rather than how
/// far the walk descends.
pub const MAX_INSTANCE_TREE_DEPTH: usize = 64;

/// Hard ceiling across all instances so a hostile file cannot OOM the app.
pub const MAX_TOTAL_PARTICLES: usize = 100_000;

// --- Coverage ------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SupportLevel {
    /// Simulated with Source-equivalent math.
    Full,
    /// Simulated, but with simplified math; the look may differ.
    Approximate,
    /// Meaningless without a map/entity context; deliberately inert.
    PreviewInert,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageEntry {
    pub function: String,
    /// Which operator list the function came from ("emitters", ...).
    pub list: &'static str,
    pub level: SupportLevel,
}

// --- Small vector helpers ------------------------------------------------

fn color_to_rgb(color: [u8; 4]) -> Vec3 {
    Vec3::new(
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
    )
}

/// Source's bias curve (0.5 = identity).
fn bias(t: f32, amount: f32) -> f32 {
    if (amount - 0.5).abs() < 1e-3 {
        return t;
    }
    t / ((1.0 / amount.clamp(1e-3, 1.0 - 1e-3) - 2.0) * (1.0 - t) + 1.0)
}

// --- Engine ----------------------------------------------------------------

#[derive(Clone, Debug)]
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

pub struct ParticleEngine {
    systems: Vec<CompiledSystem>,
    instances: Vec<Instance>,
    control_points: [Vec3; MAX_CONTROL_POINTS],
    control_point_velocity: [Vec3; MAX_CONTROL_POINTS],
    time: f32,
    seed: u64,
    emitters_alive: bool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::pcf::{PcfChild, PcfValue};

    #[test]
    fn integer_rng_supports_the_entire_i32_domain() {
        let mut expected = Rng::new(0xfeed_beef);
        let raw = expected.next_u32();
        let mut actual = Rng::new(0xfeed_beef);

        assert_eq!(
            actual.range_int(i32::MIN, i32::MAX),
            (i64::from(i32::MIN) + i64::from(raw)) as i32
        );
    }

    #[test]
    fn integer_rng_keeps_degenerate_ranges_total_without_consuming_entropy() {
        let mut expected = Rng::new(17);
        let next = expected.next_u32();
        let mut actual = Rng::new(17);

        assert_eq!(actual.range_int(12, 12), 12);
        assert_eq!(actual.range_int(12, -4), 12);
        assert_eq!(actual.next_u32(), next);
    }

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

    /// Decided at compile time, not rediscovered by two full scans of
    /// `operators` per instance per substep. The predicate has to keep
    /// matching both operators that retire particles — dropping either would
    /// leave dead particles on screen forever, which no frame-count assertion
    /// elsewhere distinguishes from a slow emitter.
    #[test]
    fn retirement_is_decided_once_at_compile_time() {
        let with_decay = engine_for(vec![basic_system()]);
        assert!(with_decay.systems[0].retires_particles);

        let mut fading = basic_system();
        fading.operators = vec![function("Alpha Fade and Decay", &[])];
        assert!(engine_for(vec![fading]).systems[0].retires_particles);

        let mut immortal = basic_system();
        immortal.operators = Vec::new();
        assert!(!engine_for(vec![immortal]).systems[0].retires_particles);
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
            &[("gravity", PcfValue::Vector3(Vec3::new(0.0, 0.0, -100.0)))],
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

    /// A `.pcf` is untrusted, and its child links can chain arbitrarily deep.
    /// The walk must stop at a fixed depth rather than following the file —
    /// a recursive version would spend attacker-controlled stack frames.
    #[test]
    fn a_deep_child_chain_stops_at_the_depth_bound() {
        let depth = MAX_INSTANCE_TREE_DEPTH * 2;
        let systems = (0..depth)
            .map(|index| {
                let mut system = basic_system();
                system.name = format!("system{index}");
                if index + 1 < depth {
                    system.children = vec![PcfChild {
                        name: format!("system{}", index + 1),
                        system_index: Some(index + 1),
                        delay: 0.0,
                    }];
                }
                system
            })
            .collect::<Vec<_>>();

        let engine = engine_for(systems);

        assert_eq!(
            engine.instances.len(),
            MAX_INSTANCE_TREE_DEPTH,
            "the walk must stop exactly at the bound, not before or past it"
        );
    }

    /// A cycle between two systems must terminate on the instance ceiling.
    #[test]
    fn a_child_cycle_terminates() {
        let mut first = basic_system();
        first.name = "first".to_owned();
        first.children = vec![PcfChild {
            name: "second".to_owned(),
            system_index: Some(1),
            delay: 0.0,
        }];
        let mut second = basic_system();
        second.name = "second".to_owned();
        second.children = vec![PcfChild {
            name: "first".to_owned(),
            system_index: Some(0),
            delay: 0.0,
        }];

        let engine = engine_for(vec![first, second]);

        assert!(
            engine.instances.len() <= 4,
            "instance ceiling is 2 * systems"
        );
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
        engine.set_control_point(ControlPointIndex::clamped(0), Vec3::new(100.0, 0.0, 0.0));
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
    /// PCF files name control points freely, and the clamp is the only thing
    /// keeping an out-of-range number from meaning something different on each
    /// path that reaches it.
    #[test]
    fn an_out_of_range_control_point_clamps_once_at_construction() {
        assert_eq!(ControlPointIndex::clamped(0).get(), 0);
        assert_eq!(
            ControlPointIndex::clamped(MAX_CONTROL_POINTS - 1).get(),
            MAX_CONTROL_POINTS - 1
        );
        assert_eq!(
            ControlPointIndex::clamped(MAX_CONTROL_POINTS).get(),
            MAX_CONTROL_POINTS - 1
        );
        assert_eq!(
            ControlPointIndex::clamped(usize::MAX).get(),
            MAX_CONTROL_POINTS - 1
        );
    }

    /// Every read indexes all sixteen arrays by the same position, so a
    /// whole-set operation that misses one desynchronises them permanently.
    /// Spawning *and* expiring particles exercises `push` and `swap_remove`.
    #[test]
    fn whole_set_operations_keep_the_arrays_parallel() {
        let mut engine = engine_for(vec![basic_system()]);
        engine.step(0.5);
        assert!(engine.live_particles() > 0, "the fixture must spawn");
        for instance in &engine.instances {
            assert!(instance.particles.arrays_are_parallel(), "push went ragged");
        }

        // Past the 1s lifetime, so the earliest particles swap_remove out
        // while the continuous emitter keeps spawning. The live count alone
        // cannot show that — it stays roughly flat — but having spawned more
        // than are alive can.
        engine.step(2.0);
        assert!(
            engine
                .instances
                .iter()
                .any(|instance| instance.spawn_counter as usize > instance.particles.len()),
            "nothing expired, so swap_remove never ran"
        );
        for instance in &engine.instances {
            assert!(
                instance.particles.arrays_are_parallel(),
                "swap_remove went ragged"
            );
        }
    }
}
