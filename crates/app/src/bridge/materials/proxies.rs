//! Ordered Source material programs. Variables and random state belong to a
//! viewer, never to the shared material/texture cache.
use super::parameters::{MaterialProxy, texture_transform};
use super::{AnimationClock, AnimationError, EffectiveMaterial};
use std::collections::BTreeMap;
mod apply;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Float(f32),
    Vector([f32; 4], usize),
    Matrix([[f32; 4]; 2]),
}
impl Value {
    fn scalar(&self, component: Option<usize>) -> f32 {
        match self {
            Self::Float(v) => *v,
            Self::Vector(v, size) => component.map_or(v[0], |i| if i < *size { v[i] } else { 0.0 }),
            Self::Matrix(_) => 0.0,
        }
    }
    fn vector(&self) -> [f32; 4] {
        match self {
            Self::Vector(v, _) => *v,
            Self::Float(v) => [*v; 4],
            Self::Matrix(_) => [0.0; 4],
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
struct Variable {
    index: usize,
    component: Option<usize>,
}
#[derive(Clone, Debug, PartialEq)]
enum Input {
    Constant(f32),
    Variable(Variable),
}
#[derive(Clone, Debug, PartialEq)]
enum Op {
    Sine([Input; 4]),
    Equals(Variable),
    Arithmetic(&'static str, Variable, Variable),
    Unary(&'static str, Variable),
    Clamp(Variable, [Input; 2]),
    Ramp([Input; 2]),
    Noise {
        gaussian: bool,
        inputs: [Input; 4],
    },
    Transform([Option<Variable>; 4]),
    Scroll([Input; 3]),
    Animated {
        texture: String,
        rate: Input,
        nowrap: bool,
    },
}
#[derive(Clone, Debug, PartialEq)]
struct Instruction {
    result: Variable,
    op: Op,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    names: BTreeMap<String, usize>,
    initial: Vec<Value>,
    code: Vec<Instruction>,
    written: Vec<bool>,
    pub requires_evaluation: bool,
    animated: bool,
}
#[derive(Clone, Debug, Default)]
pub struct State {
    values: Vec<Value>,
    clock: Option<AnimationClock>,
    random: u64,
}

impl Program {
    pub(super) fn compile(material: &EffectiveMaterial) -> (Self, Vec<String>) {
        if material.proxies.is_empty() {
            return (Self::default(), Vec::new());
        }
        let mut p = Self::default();
        for (name, text) in &material.parameters {
            let value = if text.trim_start().starts_with("center ") {
                Some(Value::Matrix(texture_transform(Some(text))))
            } else {
                let color_scale = if text.trim_start().starts_with('{') {
                    1.0 / 255.0
                } else {
                    1.0
                };
                let components = text
                    .trim()
                    .trim_matches(['[', ']', '{', '}'])
                    .split_whitespace()
                    .map(|v| v.parse::<f32>().map(|v| v * color_scale))
                    .collect::<Result<Vec<_>, _>>()
                    .ok();
                components.and_then(|v| {
                    if v.is_empty() || v.len() > 4 || !v.iter().all(|v| v.is_finite()) {
                        return None;
                    }
                    if v.len() == 1 {
                        Some(Value::Float(v[0]))
                    } else {
                        let mut vector = [0.0; 4];
                        vector[..v.len()].copy_from_slice(&v);
                        Some(Value::Vector(vector, v.len()))
                    }
                })
            };
            if let Some(value) = value {
                p.add(name, value);
            }
        }
        for name in [
            "$basetexturetransform",
            "$bumptransform",
            "$bumptransform2",
            "$detailtexturetransform",
            "$envmapmasktransform",
            "$blendmasktransform",
        ] {
            if !p.names.contains_key(name) {
                p.add(name, Value::Matrix(texture_transform(None)));
            }
        }
        for (name, value) in [
            ("$alpha", 1.0),
            ("$frame", 0.0),
            ("$frame2", 0.0),
            ("$bumpframe", 0.0),
            ("$bumpframe2", 0.0),
            ("$detailframe", 0.0),
            ("$refracttinttextureframe", 0.0),
            ("$envmapframe", 0.0),
            ("$envmapmaskframe", 0.0),
            ("$detailblendfactor", 1.0),
            ("$phongboost", 1.0),
            ("$phongexponent", 0.0),
            ("$refractamount", 2.0),
            ("$envmapcontrast", 0.0),
            ("$envmapsaturation", 1.0),
        ] {
            if !p.names.contains_key(name) {
                p.add(name, Value::Float(value));
            }
        }
        for name in [
            "$color",
            "$color2",
            "$selfillumtint",
            "$envmaptint",
            "$detailtint",
            "$phongtint",
            "$refracttint",
        ] {
            if !p.names.contains_key(name) {
                p.add(name, Value::Vector([1.0, 1.0, 1.0, 0.0], 3));
            }
        }
        p.written.resize(p.initial.len(), false);
        let mut unsupported = Vec::new();
        for proxy in &material.proxies {
            if proxy.name == "empty" || proxy.name == "waterlod" {
                continue;
            }
            match p.instruction(proxy) {
                Some(instruction) => {
                    p.written[instruction.result.index] = true;
                    p.animated |= !matches!(
                        instruction.op,
                        Op::Equals(_)
                            | Op::Arithmetic(..)
                            | Op::Unary(..)
                            | Op::Clamp(..)
                            | Op::Transform(..)
                    );
                    p.requires_evaluation |= match &instruction.op {
                        Op::Scroll(inputs) => {
                            inputs.iter().any(|v| matches!(v, Input::Variable(_)))
                                || proxy
                                    .values
                                    .get("texturescrollvar")
                                    .is_some_and(|v| v.eq_ignore_ascii_case("$bumptransform2"))
                        }
                        Op::Animated { rate, texture, .. } => {
                            matches!(rate, Input::Variable(_))
                                || texture == "$normalmap2"
                                || proxy
                                    .values
                                    .get("animatedtextureframenumvar")
                                    .is_some_and(|v| {
                                        ![
                                            "$frame",
                                            "$frame2",
                                            "$bumpframe",
                                            "$bumpframe2",
                                            "$detailframe",
                                            "$envmapframe",
                                            "$envmapmaskframe",
                                        ]
                                        .iter()
                                        .any(|name| v.eq_ignore_ascii_case(name))
                                    })
                        }
                        _ => true,
                    };
                    p.code.push(instruction);
                }
                None => unsupported.push(proxy.name.clone()),
            }
        }
        (p, unsupported)
    }
    fn add(&mut self, name: &str, value: Value) {
        self.names.insert(name.to_owned(), self.initial.len());
        self.initial.push(value);
    }
    fn variable(&self, value: &str) -> Option<Variable> {
        let value = value.trim().to_ascii_lowercase();
        let (name, component) = if let Some((name, suffix)) = value.split_once('[') {
            let i = suffix.strip_suffix(']')?.parse::<usize>().ok()?;
            if i >= 4 {
                return None;
            }
            (name, Some(i))
        } else {
            (value.as_str(), None)
        };
        Some(Variable {
            index: *self.names.get(name)?,
            component,
        })
    }
    fn instruction(&self, proxy: &MaterialProxy) -> Option<Instruction> {
        let text = |name: &str| proxy.values.get(name).map(String::as_str);
        let var = |name| self.variable(text(name)?);
        let input = |name, default| -> Option<Input> {
            let Some(text) = text(name) else {
                return Some(Input::Constant(default));
            };
            if let Ok(v) = text.trim().parse::<f32>() {
                return v.is_finite().then_some(Input::Constant(v));
            }
            self.variable(text).map(Input::Variable)
        };
        let result = match proxy.name.as_str() {
            "texturescroll" => var("texturescrollvar")?,
            "animatedtexture" => var("animatedtextureframenumvar")?,
            _ => var("resultvar")?,
        };
        let op = match proxy.name.as_str() {
            "sine" => Op::Sine([
                input("sineperiod", 1.0)?,
                input("sinemin", 0.0)?,
                input("sinemax", 1.0)?,
                input("timeoffset", 0.0)?,
            ]),
            "equals" => Op::Equals(var("srcvar1")?),
            "add" | "subtract" | "multiply" | "divide" => Op::Arithmetic(
                match proxy.name.as_str() {
                    "add" => "add",
                    "subtract" => "subtract",
                    "multiply" => "multiply",
                    _ => "divide",
                },
                var("srcvar1")?,
                var("srcvar2")?,
            ),
            "abs" | "frac" | "int" => Op::Unary(
                match proxy.name.as_str() {
                    "abs" => "abs",
                    "frac" => "frac",
                    _ => "int",
                },
                var("srcvar1")?,
            ),
            "clamp" => Op::Clamp(var("srcvar1")?, [input("min", 0.0)?, input("max", 1.0)?]),
            "linearramp" => Op::Ramp([input("rate", 1.0)?, input("initialvalue", 0.0)?]),
            "gaussiannoise" | "uniformnoise" => {
                let gaussian = proxy.name == "gaussiannoise";
                Op::Noise {
                    gaussian,
                    inputs: [
                        input("minval", if gaussian { -f32::MAX } else { 0.0 })?,
                        input("maxval", if gaussian { f32::MAX } else { 1.0 })?,
                        input("mean", 0.0)?,
                        input("halfwidth", 1.0)?,
                    ],
                }
            }
            "texturetransform" => {
                if !matches!(self.initial[result.index], Value::Matrix(_)) {
                    return None;
                }
                let mut inputs = [None, None, None, None];
                for (i, name) in ["centervar", "scalevar", "rotatevar", "translatevar"]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(value) = text(name) {
                        inputs[i] = Some(self.variable(value)?);
                    }
                }
                Op::Transform(inputs)
            }
            "texturescroll" => {
                if !matches!(self.initial[result.index], Value::Matrix(_)) {
                    return None;
                }
                Op::Scroll([
                    input("texturescrollrate", 1.0)?,
                    input("texturescrollangle", 0.0)?,
                    input("texturescale", 1.0)?,
                ])
            }
            "animatedtexture" => {
                let texture = text("animatedtexturevar")?.trim().to_ascii_lowercase();
                if !material_texture_parameter(&texture) {
                    return None;
                }
                Op::Animated {
                    texture,
                    rate: input("animatedtextureframerate", 15.0)?,
                    nowrap: super::vmt_bool(text("animationnowrap")),
                }
            }
            _ => return None,
        };
        Some(Instruction { result, op })
    }
    pub fn writes(&self, name: &str) -> bool {
        self.names
            .get(name)
            .is_some_and(|&index| self.written[index])
    }
    pub fn is_animated(&self) -> bool {
        // An ordered program may read the value retained from its previous
        // bind. Keep it ticking even when it contains no explicit time input.
        self.animated || (self.requires_evaluation && !self.code.is_empty())
    }
    pub fn evaluate<'a>(
        &self,
        state: &'a mut State,
        clock: AnimationClock,
        seed: u64,
        count: impl Fn(&str) -> Option<u32>,
    ) -> Result<&'a State, AnimationError> {
        if !clock.time_seconds.is_finite() || !clock.frame_seconds.is_finite() {
            return Err(AnimationError::NonFinite);
        }
        if state.clock == Some(clock) {
            return Ok(state);
        }
        if state.values.len() != self.initial.len()
            || state
                .clock
                .is_none_or(|last| clock.time_seconds < last.time_seconds)
        {
            state.values.clone_from(&self.initial);
            let mut mixed = seed.wrapping_add(0x9e3779b97f4a7c15);
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d049bb133111eb);
            state.random = (mixed ^ (mixed >> 31)).max(1);
        }
        for instruction in &self.code {
            let read = |input: &Input| match input {
                Input::Constant(v) => *v,
                Input::Variable(v) => state.values[v.index].scalar(v.component),
            };
            let scalar = |v: &Variable| state.values[v.index].scalar(v.component);
            let vector = |v: &Variable| {
                if v.component.is_some() {
                    [scalar(v); 4]
                } else {
                    state.values[v.index].vector()
                }
            };
            let vector_size = match state.values[instruction.result.index] {
                Value::Vector(_, size) if instruction.result.component.is_none() => Some(size),
                _ => None,
            };
            let result = match &instruction.op {
                Op::Sine(inputs) => {
                    let [period, min, max, offset] = inputs.each_ref().map(read);
                    let period = if period == 0.0 { 1.0 } else { period };
                    Value::Float(
                        (max - min)
                            * ((std::f64::consts::TAU * f64::from(clock.time_seconds - offset)
                                / f64::from(period))
                            .sin() as f32
                                * 0.5
                                + 0.5)
                            + min,
                    )
                }
                Op::Equals(source) => vector_size.map_or_else(
                    || Value::Float(scalar(source)),
                    |size| Value::Vector(vector(source), size),
                ),
                Op::Arithmetic(kind, a, b) => {
                    let apply = |a, b| match *kind {
                        "add" => a + b,
                        "subtract" => a - b,
                        "multiply" => a * b,
                        _ => {
                            if b == 0.0 {
                                a
                            } else {
                                a / b
                            }
                        }
                    };
                    vector_size.map_or_else(
                        || Value::Float(apply(scalar(a), scalar(b))),
                        |size| {
                            let av = vector(a);
                            let bv = vector(b);
                            Value::Vector(std::array::from_fn(|i| apply(av[i], bv[i])), size)
                        },
                    )
                }
                Op::Unary(kind, a) => {
                    let apply = |v: f32| match *kind {
                        "abs" => v.abs(),
                        "int" => v.trunc(),
                        _ => v.fract(),
                    };
                    if *kind != "abs"
                        && let Some(size) = vector_size
                    {
                        Value::Vector(vector(a).map(apply), size)
                    } else {
                        Value::Float(apply(scalar(a)))
                    }
                }
                Op::Clamp(a, inputs) => {
                    let [min, max] = inputs.each_ref().map(read);
                    let (min, max) = (min.min(max), min.max(max));
                    vector_size.map_or_else(
                        || Value::Float(scalar(a).clamp(min, max)),
                        |size| Value::Vector(vector(a).map(|v| v.clamp(min, max)), size),
                    )
                }
                Op::Ramp(inputs) => {
                    Value::Float(read(&inputs[0]) * clock.time_seconds + read(&inputs[1]))
                }
                Op::Noise { gaussian, inputs } => {
                    let [min, max, mean, width] = inputs.each_ref().map(read);
                    let (min, max) = (min.min(max), min.max(max));
                    let a = random(&mut state.random);
                    let value = if *gaussian {
                        let b = random(&mut state.random);
                        mean + width * (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
                    } else {
                        min + (max - min) * a
                    };
                    Value::Float(value.clamp(min, max))
                }
                Op::Transform(inputs) => {
                    let center = inputs[0].as_ref().map_or([0.5; 4], vector);
                    let scale = inputs[1].as_ref().map_or([1.0; 4], vector);
                    let rotate = inputs[2].as_ref().map_or(0.0, scalar).to_radians();
                    let translate = inputs[3].as_ref().map_or([0.0; 4], vector);
                    let (sin, cos) = rotate.sin_cos();
                    let a = cos * scale[0];
                    let b = -sin * scale[1];
                    let c = sin * scale[0];
                    let d = cos * scale[1];
                    Value::Matrix([
                        [
                            a,
                            b,
                            0.0,
                            center[0] + translate[0] - a * center[0] - b * center[1],
                        ],
                        [
                            c,
                            d,
                            0.0,
                            center[1] + translate[1] - c * center[0] - d * center[1],
                        ],
                    ])
                }
                Op::Scroll(inputs) => {
                    let [rate, angle, scale] = inputs.each_ref().map(read);
                    let angle = f64::from(angle).to_radians();
                    let wrap = |v: f64| {
                        let v = v as f32;
                        let v = if v < 0.0 { v + 1.0 - v.trunc() } else { v };
                        v - v.trunc()
                    };
                    Value::Matrix([
                        [
                            scale,
                            0.0,
                            0.0,
                            wrap(f64::from(clock.time_seconds) * angle.cos() * f64::from(rate)),
                        ],
                        [
                            0.0,
                            scale,
                            0.0,
                            wrap(f64::from(clock.time_seconds) * angle.sin() * f64::from(rate)),
                        ],
                    ])
                }
                Op::Animated {
                    texture,
                    rate,
                    nowrap,
                } => {
                    let Some(count) = count(texture) else {
                        continue;
                    };
                    let count = i32::try_from(count)
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or(AnimationError::FrameCount)?;
                    let rate = read(rate);
                    let current = rate * clock.time_seconds.max(0.0);
                    let previous = rate * (clock.time_seconds - clock.frame_seconds).max(0.0);
                    if !current.is_finite()
                        || !previous.is_finite()
                        || f64::from(current.trunc()).abs() > f64::from(i32::MAX)
                        || f64::from(previous.trunc()).abs() > f64::from(i32::MAX)
                    {
                        return Err(AnimationError::IntegerRange);
                    }
                    let frame = current as i32 % count;
                    Value::Float(if *nowrap && previous as i32 % count > frame {
                        count - 1
                    } else {
                        frame
                    } as f32)
                }
            };
            let finite = match &result {
                Value::Float(v) => v.is_finite(),
                Value::Vector(v, _) => v.iter().all(|v| v.is_finite()),
                Value::Matrix(v) => v.iter().flatten().all(|v| v.is_finite()),
            };
            if !finite {
                return Err(AnimationError::NonFinite);
            }
            state.assign(&instruction.result, result);
        }
        state.clock = Some(clock);
        Ok(state)
    }
    pub fn value<'a>(&self, state: &'a State, name: &str) -> Option<&'a Value> {
        let &index = self.names.get(name)?;
        self.written[index].then(|| &state.values[index])
    }
}
impl State {
    fn assign(&mut self, target: &Variable, value: Value) {
        match (&mut self.values[target.index], value) {
            (Value::Vector(v, size), Value::Float(value)) => {
                if let Some(i) = target.component {
                    if i < *size {
                        v[i] = value;
                    }
                } else {
                    v[..*size].fill(value);
                }
            }
            (destination, value) => *destination = value,
        }
    }
}
fn random(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (((*state >> 40) as u32) as f32 + 0.5) / 16777216.0
}
fn material_texture_parameter(name: &str) -> bool {
    matches!(
        name,
        "$basetexture"
            | "$basetexture2"
            | "$bumpmap"
            | "$bumpmap2"
            | "$normalmap"
            | "$normalmap2"
            | "$detail"
            | "$envmap"
            | "$envmapmask"
            | "$selfillummask"
            | "$blendmodulatetexture"
            | "$phongexponenttexture"
            | "$phongwarptexture"
            | "$lightwarptexture"
            | "$refracttinttexture"
    )
}
