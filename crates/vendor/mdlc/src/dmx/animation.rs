use super::*;

pub(super) fn names(document: &Document) -> Result<Vec<String>> {
    clips(document)?
        .into_iter()
        .map(|id| Ok(node(document, id)?.name.clone()))
        .collect()
}

fn clips(document: &Document) -> Result<Vec<usize>> {
    match element(node(document, document.root)?, "animationList")? {
        Some(id) => elements(node(document, id)?, "animations"),
        None => Ok(Vec::new()),
    }
}

fn ticks(node: &Node, key: &str, default: i32) -> Result<i32> {
    match value(node, key).or_else(|| value(node, &format!("{key}Time"))) {
        None => Ok(default),
        Some(Value::Time(v) | Value::Int(v)) if *v != i32::MIN => Ok(*v),
        _ => Err(format!("DMX {}.{key} must be a valid time", node.name)),
    }
}

#[derive(Clone, Copy, Debug)]
enum Sample {
    Position([f32; 3]),
    Rotation([f32; 4]),
}

fn valid_quaternion(q: &[f32; 4]) -> bool {
    let length = q.iter().map(|v| v * v).sum::<f32>();
    length.is_finite() && length > 1e-12
}

fn sampled_poses(
    skeleton: &Skeleton,
    matrices: &[Matrix3x4],
    opts: &FbxOpts,
) -> Result<Vec<SmdPose>> {
    let poses = skeleton.poses(matrices, opts);
    if poses.iter().any(|pose| {
        pose.position
            .iter()
            .chain(&pose.rotation)
            .any(|v| !v.is_finite())
    }) {
        return Err("DMX sampled pose contains a nonfinite transform".into());
    }
    Ok(poses)
}

impl Sample {
    fn zero(orientation: bool) -> Self {
        if orientation {
            Self::Rotation([0., 0., 0., 1.])
        } else {
            Self::Position([0.; 3])
        }
    }

    fn read(n: &Node, key: &str, orientation: bool, default: Self) -> Result<Self> {
        match (orientation, value(n, key)) {
            (_, None) => Ok(default),
            (false, Some(Value::Vector3(v))) if v.iter().all(|v| v.is_finite()) => {
                Ok(Self::Position(*v))
            }
            (true, Some(Value::Quaternion(q)))
                if q.iter().all(|v| v.is_finite()) && valid_quaternion(q) =>
            {
                Ok(Self::Rotation(bone_math::quaternion_normalize(*q)))
            }
            _ => Err(format!(
                "DMX {}.{key} has an invalid animation value",
                n.name
            )),
        }
    }

    fn blend(self, other: Self, t: f32) -> Self {
        match (self, other) {
            (Self::Position(a), Self::Position(b)) => {
                Self::Position(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t))
            }
            (Self::Rotation(a), Self::Rotation(b)) => Self::Rotation(slerp(a, b, t)),
            _ => unreachable!(),
        }
    }
}

struct CurveInfo {
    default: i32,
    edge_values: [Sample; 2],
    edge_curves: [i32; 2],
    right_time: i32,
}

fn curve_type(value: i32) -> Result<i32> {
    if value < 0 || value & !0xffff != 0 || value & 255 > 15 || value >> 8 > 15 {
        return Err(format!("DMX animation has unknown curve type {value}"));
    }
    Ok(value)
}

impl CurveInfo {
    fn load(n: &Node, orientation: bool) -> Result<Self> {
        let default = curve_type(integer(n, "defaultCurveType", 0)?)?;
        let mut result = Self {
            default,
            edge_values: [Sample::zero(orientation); 2],
            edge_curves: [default; 2],
            right_time: ticks(n, "rightEdge", 0)?,
        };
        let use_edges = boolean(n, "useEdgeInfo", false)?;
        let zero = Sample::read(
            n,
            "defaultEdgeZeroValue",
            orientation,
            Sample::zero(orientation),
        )?;
        for (side, name) in ["left", "right"].into_iter().enumerate() {
            let active = boolean(n, &format!("{name}EdgeActive"), false)?;
            if use_edges {
                result.edge_values[side] = if active {
                    Sample::read(
                        n,
                        &format!("{name}EdgeValue"),
                        orientation,
                        Sample::zero(orientation),
                    )?
                } else {
                    zero
                };
            }
            if active {
                let selected = curve_type(integer(n, &format!("{name}EdgeCurveType"), 0)?)?;
                if selected != 0 {
                    result.edge_curves[side] = selected;
                }
            }
        }
        Ok(result)
    }
}

struct Layer {
    times: Vec<i32>,
    values: Vec<Sample>,
    curves: Vec<i32>,
    steps: Vec<bool>,
    infinite_left: bool,
    infinite_right: bool,
}

#[derive(Clone, Copy)]
enum Input {
    Value(Sample),
    Bone { bone: usize, orientation: bool },
}

struct Channel {
    pass: Option<Input>,
    bone: usize,
    layers: Vec<Layer>,
    curve: Option<CurveInfo>,
    default: Sample,
    orientation: bool,
}

impl Layer {
    fn load(document: &Document, id: usize, orientation: bool) -> Result<Self> {
        let n = node(document, id)?;
        let times = match value(n, "times") {
            None => Vec::new(),
            Some(Value::TimeArray(v) | Value::IntArray(v)) => v.clone(),
            _ => return Err(format!("DMX animation log {} has no time array", n.name)),
        };
        if times.contains(&i32::MIN) || times.windows(2).any(|w| w[0] >= w[1]) {
            return Err(format!(
                "DMX log {} timestamps must strictly increase",
                n.name
            ));
        }
        let samples = match (orientation, value(n, "values")) {
            (_, None) => Vec::new(),
            (false, Some(Value::Vector3Array(v))) if v.iter().flatten().all(|v| v.is_finite()) => {
                v.iter().copied().map(Sample::Position).collect()
            }
            (true, Some(Value::QuaternionArray(v)))
                if v.iter().flatten().all(|v| v.is_finite()) && v.iter().all(valid_quaternion) =>
            {
                v.iter()
                    .copied()
                    .map(|q| Sample::Rotation(bone_math::quaternion_normalize(q)))
                    .collect()
            }
            _ => return Err(format!("DMX animation log {} has invalid values", n.name)),
        };
        if samples.len() != times.len() {
            return Err(format!(
                "DMX log {} times and values have different lengths",
                n.name
            ));
        }
        let curves = match value(n, "curvetypes").or_else(|| value(n, "curveTypes")) {
            None => Vec::new(),
            Some(Value::IntArray(v)) => v
                .iter()
                .copied()
                .map(curve_type)
                .collect::<Result<Vec<_>>>()?,
            _ => return Err("DMX curve types must be an integer array".into()),
        };
        if !curves.is_empty() && curves.len() != times.len() {
            return Err("DMX curve type count differs from sample count".into());
        }
        let steps = match value(n, "segmentInterpolation") {
            None => Vec::new(),
            Some(Value::BoolArray(v)) => v.clone(),
            _ => return Err("DMX segmentInterpolation must be a boolean array".into()),
        };
        if !steps.is_empty() && steps.len() != times.len() {
            return Err("DMX segment interpolation count differs from sample count".into());
        }
        Ok(Self {
            times,
            values: samples,
            curves,
            steps,
            infinite_left: boolean(n, "leftInfinite", false)?,
            infinite_right: boolean(n, "rightInfinite", false)?,
        })
    }

    fn contains(&self, time: i32) -> bool {
        match (self.times.first(), self.times.last()) {
            (Some(&first), Some(&last)) => {
                (time >= first || self.infinite_left) && (time <= last || self.infinite_right)
            }
            _ => false,
        }
    }

    fn sample(&self, time: i32, curve: Option<&CurveInfo>, default: Sample) -> Sample {
        if self.times.is_empty() {
            return default;
        }
        let upper = self.times.partition_point(|&t| t <= time);
        let Some(curve) = curve else {
            let low = upper.saturating_sub(1).min(self.times.len() - 1);
            let high = upper.min(self.times.len() - 1);
            let fraction = if low == high || self.steps.get(low).copied().unwrap_or(false) {
                0.
            } else {
                ((f64::from(time) - f64::from(self.times[low]))
                    / (f64::from(self.times[high]) - f64::from(self.times[low])))
                    as f32
            };
            return self.values[low].blend(self.values[high], fraction);
        };
        if curve.right_time != 0 && time > curve.right_time {
            return match default {
                Sample::Position(_) => Sample::zero(false),
                Sample::Rotation(_) => Sample::zero(true),
            };
        }
        if curve.right_time == 0 && upper == self.times.len() {
            return self.values[self.times.len() - 1];
        }
        let key = upper as isize - 1;
        let bounded = |index: isize| {
            if index < 0 {
                (0, curve.edge_values[0], curve.edge_curves[0])
            } else if index as usize >= self.times.len() {
                (
                    if curve.right_time != 0 {
                        curve.right_time
                    } else {
                        self.times[self.times.len() - 1].saturating_add(1)
                    },
                    curve.edge_values[1],
                    curve.edge_curves[1],
                )
            } else {
                let index = index as usize;
                let kind = self
                    .curves
                    .get(index)
                    .copied()
                    .filter(|&v| v != 0)
                    .unwrap_or(curve.default);
                (self.times[index], self.values[index], kind)
            }
        };
        let samples = [
            bounded(key - 1),
            bounded(key),
            bounded(key + 1),
            bounded(key + 2),
        ];
        let fraction = if samples[2].0 > samples[1].0 {
            ((f64::from(time) - f64::from(samples[1].0))
                / (f64::from(samples[2].0) - f64::from(samples[1].0))) as f32
        } else {
            0.
        };
        if key >= 0 && self.steps.get(key as usize).copied().unwrap_or(false) {
            return samples[1].1;
        }
        let early = samples[1].2 & 255;
        let late = samples[2].2 >> 8;
        if early == 15 {
            return samples[1].1;
        }
        if late == 15 {
            return samples[2].1;
        }
        let values = samples.map(|v| v.1);
        let early_value = interpolate_curve(early, values, fraction);
        if early == late {
            early_value
        } else {
            early_value.blend(interpolate_curve(late, values, fraction), fraction)
        }
    }
}

impl Channel {
    fn sample(&self, time: i32) -> Sample {
        let first = self
            .layers
            .iter()
            .filter_map(|layer| layer.times.first())
            .copied()
            .min();
        let last = self
            .layers
            .iter()
            .filter_map(|layer| layer.times.last())
            .copied()
            .max();
        let time = match (first, last) {
            (Some(first), Some(last)) => time.clamp(first, last),
            _ => time,
        };
        self.layers
            .iter()
            .rev()
            .find(|layer| layer.contains(time))
            .or_else(|| self.layers.first())
            .map(|layer| layer.sample(time, self.curve.as_ref(), self.default))
            .unwrap_or_else(|| Sample::zero(self.orientation))
    }
}

struct Sampler<'a> {
    channels: &'a [Channel],
    targets: &'a HashMap<(usize, bool), usize>,
    reference: &'a [Matrix3x4],
    cache: Vec<Option<Sample>>,
    active: HashSet<usize>,
    time: i32,
}

impl Sampler<'_> {
    fn sample(&mut self, index: usize, depth: usize) -> Result<Sample> {
        if let Some(sample) = self.cache[index] {
            return Ok(sample);
        }
        if depth > 128 || !self.active.insert(index) {
            return Err(
                "DMX animation contains cyclic or excessively deep channel dependencies".into(),
            );
        }
        let channel = &self.channels[index];
        let sample = match channel.pass {
            None => channel.sample(self.time),
            Some(Input::Value(value)) => value,
            Some(Input::Bone { bone, orientation }) => {
                if let Some(&source) = self
                    .targets
                    .get(&(bone, orientation))
                    .filter(|&&source| source != index)
                {
                    self.sample(source, depth + 1)?
                } else {
                    let matrix = &self.reference[bone];
                    if orientation {
                        Sample::Rotation(bone_math::matrix_quaternion(matrix))
                    } else {
                        Sample::Position([matrix[3], matrix[7], matrix[11]])
                    }
                }
            }
        };
        self.active.remove(&index);
        self.cache[index] = Some(sample);
        Ok(sample)
    }
}

fn input(
    document: &Document,
    channel: &Node,
    skeleton: &Skeleton,
    orientation: bool,
) -> Result<Option<Input>> {
    let Some(source) = element(channel, "fromElement")? else {
        return Ok(None);
    };
    let attribute = string(channel, "fromAttribute", "")?;
    let source = node(document, source)?;
    if attribute.is_empty() {
        return Ok(None);
    }
    if let Some(&bone) =
        element(channel, "fromElement")?.and_then(|id| skeleton.by_transform.get(&id))
    {
        let source_orientation = match attribute.as_str() {
            "position" => Some(false),
            "orientation" => Some(true),
            _ => None,
        };
        if let Some(source_orientation) = source_orientation {
            if source_orientation != orientation {
                return Err("DMX channel pass-through types do not match".into());
            }
            return Ok(Some(Input::Bone { bone, orientation }));
        }
    }
    if !(source.attributes.contains_key(&attribute)
        || source.class == "DmeTransform"
            && matches!(attribute.as_str(), "position" | "orientation"))
    {
        return Ok(None);
    }
    let index = integer(channel, "fromIndex", 0)?;
    let sample = match (orientation, value(source, &attribute)) {
        (false, Some(Value::Vector3Array(values))) => Sample::Position(
            *values
                .get(
                    usize::try_from(index)
                        .map_err(|_| "DMX channel has a negative source index")?,
                )
                .ok_or("DMX channel source index is out of range")?,
        ),
        (true, Some(Value::QuaternionArray(values))) => {
            let q = *values
                .get(
                    usize::try_from(index)
                        .map_err(|_| "DMX channel has a negative source index")?,
                )
                .ok_or("DMX channel source index is out of range")?;
            if !q.iter().all(|v| v.is_finite()) || !valid_quaternion(&q) {
                return Err("DMX channel has an invalid source quaternion".into());
            }
            Sample::Rotation(bone_math::quaternion_normalize(q))
        }
        _ => Sample::read(source, &attribute, orientation, Sample::zero(orientation))?,
    };
    Ok(Some(Input::Value(sample)))
}

fn slerp(a: [f32; 4], mut b: [f32; 4], t: f32) -> [f32; 4] {
    let mut dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    if dot < 0. {
        b = b.map(|v| -v);
        dot = -dot;
    }
    if 1. - dot <= 0.000001 {
        return bone_math::quaternion_normalize(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
    }
    let angle = dot.clamp(-1., 1.).acos();
    let left = ((1. - t) * angle).sin() / angle.sin();
    let right = (t * angle).sin() / angle.sin();
    bone_math::quaternion_normalize(std::array::from_fn(|i| a[i] * left + b[i] * right))
}

fn ease(kind: i32, t: f32) -> f32 {
    match kind {
        2 => (std::f32::consts::FRAC_PI_2 * t).sin(),
        3 => 1. - (std::f32::consts::FRAC_PI_2 * (t + 1.)).sin(),
        4 => t * t * (3. - 2. * t),
        _ => t,
    }
}

fn interpolate_curve(kind: i32, values: [Sample; 4], t: f32) -> Sample {
    match values {
        [
            Sample::Rotation(_),
            Sample::Rotation(a),
            Sample::Rotation(b),
            Sample::Rotation(_),
        ] => {
            if kind == 14 {
                Sample::zero(true)
            } else {
                Sample::Rotation(slerp(a, b, ease(kind, t)))
            }
        }
        [
            Sample::Position(a),
            Sample::Position(b),
            Sample::Position(c),
            Sample::Position(d),
        ] => {
            let position = std::array::from_fn(|axis| {
                let [a, b, c, d] = [a[axis], b[axis], c[axis], d[axis]];
                match kind {
                    2..=4 | 6 => b + (c - b) * ease(kind, t),
                    10 => b + (c - b) * t * t * (3. - 2. * t),
                    5 => {
                        ((1. - t).powi(3) * a
                            + (3. * t.powi(3) - 6. * t * t + 4.) * b
                            + (-3. * t.powi(3) + 3. * t * t + 3. * t + 1.) * c
                            + t.powi(3) * d)
                            / 6.
                    }
                    7..=9 => {
                        let bias = match kind {
                            8 => -1.,
                            9 => 1.,
                            _ => 0.,
                        };
                        let tension = 0.77;
                        let continuity = 0.77;
                        let outgoing = 0.5
                            * (1. - tension)
                            * ((1. + continuity) * (1. + bias) * (b - a)
                                + (1. - continuity) * (1. - bias) * (c - b));
                        let incoming = 0.5
                            * (1. - tension)
                            * ((1. - continuity) * (1. + bias) * (c - b)
                                + (1. + continuity) * (1. - bias) * (d - c));
                        (2. * t.powi(3) - 3. * t * t + 1.) * b
                            + (t.powi(3) - 2. * t * t + t) * outgoing
                            + (-2. * t.powi(3) + 3. * t * t) * c
                            + (t.powi(3) - t * t) * incoming
                    }
                    14 => 0.,
                    _ => {
                        0.5 * ((-a + 3. * b - 3. * c + d) * t.powi(3)
                            + (2. * a - 5. * b + 4. * c - d) * t * t
                            + (-a + c) * t
                            + 2. * b)
                    }
                }
            });
            if kind == 14 {
                let delta = c[0] - b[0];
                Sample::Position([
                    0.,
                    if delta > 0. {
                        b[1] + (1. - 0.001_f32.powf(t)) * (c[1] - b[1])
                    } else {
                        b[1]
                    },
                    0.,
                ])
            } else {
                Sample::Position(position)
            }
        }
        _ => unreachable!(),
    }
}

fn frame_at_time(time: i64, fps: f32) -> i64 {
    let fps = f64::from(fps);
    let adjusted = time as f64 * fps + if time < 0 { -(10000. - fps) } else { fps - 1. };
    (adjusted / 10000.).trunc() as i64
}

fn sample_time(frame: usize, fps: f32, duration: i32, offset: i32, scale: f32) -> Result<i32> {
    let elapsed = if fps.fract() == 0. {
        let rate = fps as usize;
        let seconds = frame / rate;
        let fraction = frame % rate;
        seconds as i64 * 10000 + (fraction as f32 * (1. / fps) * 10000. + 0.5).floor() as i64
    } else {
        (frame as f64 * 10000. / f64::from(fps) + 0.5).floor() as i64
    };
    let elapsed = elapsed.clamp(0, i64::from(duration));
    let shifted = elapsed + i64::from(offset);
    if !(i64::from(i32::MIN) + 1..=i64::from(i32::MAX)).contains(&shifted) {
        return Err("DMX animation time offset overflows".into());
    }
    let time = f64::from((shifted as f32 * scale + 0.5).floor());
    if !(f64::from(i32::MIN) + 1. ..=f64::from(i32::MAX)).contains(&time) {
        return Err("DMX animation sample time overflows".into());
    }
    Ok(time as i32)
}

pub(super) fn read(
    document: &Document,
    skeleton: Skeleton,
    stack: Option<&str>,
    fps: Option<f32>,
    opts: &FbxOpts,
) -> Result<Smd> {
    let clips = clips(document)?;
    let chosen = if let Some(name) = stack {
        clips
            .iter()
            .copied()
            .find(|&id| {
                document
                    .nodes
                    .get(id)
                    .is_some_and(|clip| clip.name.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| {
                format!(
                    "DMX animation {name:?} was not found; available: {:?}",
                    names(document).unwrap_or_default()
                )
            })?
    } else if let Some(&id) = clips.first() {
        id
    } else {
        return Ok(Smd {
            version: 1,
            nodes: skeleton.nodes.clone(),
            frames: vec![SmdFrame {
                time: 0,
                poses: sampled_poses(&skeleton, &skeleton.bind, opts)?,
            }],
            triangles: Vec::new(),
        });
    };
    let clip = node(document, chosen)?;
    if clip.name.eq_ignore_ascii_case("BindPose") {
        return Err("DMX animation cannot be named BindPose".into());
    }
    let frame_rate = match fps {
        Some(fps) => fps,
        None => match value(clip, "frameRate") {
            None => 30.,
            Some(Value::Int(v)) => {
                if *v > 0 {
                    *v as f32
                } else {
                    30.
                }
            }
            _ => return Err("Source1 DMX animation frame rate must be an integer".into()),
        },
    };
    if !frame_rate.is_finite() || !(0.01..=1000.).contains(&frame_rate) {
        return Err("DMX animation frame rate must be between 0.01 and 1000".into());
    }
    let (start, duration, offset, scale) = match element(clip, "timeFrame")? {
        Some(timeframe) => {
            let timeframe = node(document, timeframe)?;
            (
                ticks(timeframe, "start", 0)?,
                ticks(timeframe, "duration", 0)?,
                ticks(timeframe, "offset", 0)?,
                number(timeframe, "scale", 1.)?,
            )
        }
        None => (0, 0, 0, 1.),
    };
    if i64::from(start) + i64::from(duration) > i64::from(i32::MAX) {
        return Err("DMX animation end time overflows".into());
    }
    if duration < 0 {
        return Err("DMX animation has a negative duration".into());
    }
    let count = (frame_at_time(i64::from(start) + i64::from(duration), frame_rate)
        - frame_at_time(i64::from(start), frame_rate)
        + 1) as usize;
    if count > 1_000_000
        || count
            .checked_mul(skeleton.nodes.len())
            .is_none_or(|n| n > 8_000_000)
    {
        return Err("DMX sampled animation exceeds pose budget".into());
    }
    let mut channels = Vec::new();
    let mut targets = HashMap::new();
    for id in elements(clip, "channels")? {
        let channel = node(document, id)?;
        let target = element(channel, "toElement")?;
        let attribute = string(channel, "toAttribute", "")?;
        let bone = target
            .and_then(|target| skeleton.by_transform.get(&target))
            .copied();
        let orientation = match attribute.as_str() {
            "position" => Some(false),
            "orientation" => Some(true),
            _ => None,
        };
        let (Some(bone), Some(orientation)) = (bone, orientation) else {
            crate::warning!(
                "DMX animation channel {:?} does not affect an exported bone transform and is omitted from the model animation",
                channel.name
            );
            continue;
        };
        let source = input(document, channel, &skeleton, orientation)?;
        let Some(log) = element(channel, "log")? else {
            if let Some(pass) = source {
                let index = channels.len();
                targets.insert((bone, orientation), index);
                channels.push(Channel {
                    pass: Some(pass),
                    bone,
                    layers: Vec::new(),
                    curve: None,
                    default: Sample::zero(orientation),
                    orientation,
                });
            }
            continue;
        };
        let log = node(document, log)?;
        let curve = match element(log, "curveinfo")?.or(element(log, "curveInfo")?) {
            Some(id) => Some(CurveInfo::load(node(document, id)?, orientation)?),
            None => None,
        };
        let default = if boolean(log, "usedefaultvalue", false)? {
            Sample::read(log, "defaultvalue", orientation, Sample::zero(orientation))?
        } else {
            Sample::zero(orientation)
        };
        let layers = elements(log, "layers")?
            .into_iter()
            .map(|id| Layer::load(document, id, orientation))
            .collect::<Result<Vec<_>>>()?;
        let pass = (layers.iter().all(|layer| layer.times.is_empty())
            && !boolean(log, "usedefaultvalue", false)?)
        .then_some(source)
        .flatten();
        let index = channels.len();
        targets.insert((bone, orientation), index);
        channels.push(Channel {
            pass,
            bone,
            layers,
            curve,
            default,
            orientation,
        });
    }
    let mut reference = Vec::with_capacity(skeleton.nodes.len());
    for &transform_id in &skeleton.transforms {
        reference.push(if let Some(id) = transform_id {
            transform(document, id)?
        } else {
            bone_math::IDENTITY
        });
    }
    let mut frames = Vec::with_capacity(count);
    for frame in 0..count {
        let time = sample_time(frame, frame_rate, duration, offset, scale)?;
        let mut matrices = reference.clone();
        let mut sampler = Sampler {
            channels: &channels,
            targets: &targets,
            reference: &reference,
            cache: vec![None; channels.len()],
            active: HashSet::new(),
            time,
        };
        for (index, channel) in channels.iter().enumerate() {
            let matrix = &mut matrices[channel.bone];
            match sampler.sample(index, 0)? {
                Sample::Position(position) => {
                    matrix[3] = position[0];
                    matrix[7] = position[1];
                    matrix[11] = position[2];
                }
                Sample::Rotation(rotation) => {
                    *matrix = bone_math::quaternion_local_transform(
                        rotation,
                        [matrix[3], matrix[7], matrix[11]],
                    );
                }
            }
        }
        frames.push(SmdFrame {
            time: frame as i32,
            poses: sampled_poses(&skeleton, &matrices, opts)?,
        });
    }
    Ok(Smd {
        version: 1,
        nodes: skeleton.nodes,
        frames,
        triangles: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(sample: Sample) -> [f32; 3] {
        match sample {
            Sample::Position(v) => v,
            Sample::Rotation(_) => panic!("expected position"),
        }
    }

    fn layer(times: &[i32], values: &[f32]) -> Layer {
        Layer {
            times: times.to_vec(),
            values: values
                .iter()
                .map(|&v| Sample::Position([v, 0., 0.]))
                .collect(),
            curves: Vec::new(),
            steps: Vec::new(),
            infinite_left: false,
            infinite_right: false,
        }
    }

    #[test]
    fn log_layers_select_the_topmost_active_layer_and_preserve_defaults_and_steps() {
        let mut channel = Channel {
            pass: None,
            bone: 0,
            layers: vec![
                layer(&[0, 10000], &[1., 1.]),
                layer(&[2000, 4000], &[9., 9.]),
            ],
            curve: None,
            default: Sample::Position([6., 0., 0.]),
            orientation: false,
        };
        for (time, expected) in [(0, 1.), (2000, 9.), (3000, 9.), (4000, 9.), (5000, 1.)] {
            assert_eq!(position(channel.sample(time))[0], expected);
        }
        channel.layers[1] = layer(&[12000, 14000], &[9., 9.]);
        assert_eq!(position(channel.sample(16000))[0], 9.);
        channel.layers[1] = layer(&[2000, 4000], &[9., 9.]);
        channel.layers[1].infinite_left = true;
        assert_eq!(position(channel.sample(0))[0], 9.);
        channel.layers = vec![layer(&[], &[])];
        assert_eq!(position(channel.sample(5000))[0], 6.);
        let mut stepped = layer(&[0, 10000], &[2., 8.]);
        stepped.steps = vec![true, false];
        assert_eq!(position(stepped.sample(9999, None, channel.default))[0], 2.);
        assert_eq!(
            position(stepped.sample(10000, None, channel.default))[0],
            8.
        );
    }

    #[test]
    fn curve_interpolators_match_source_vector_and_quaternion_semantics() {
        let samples = [0., 2., 10., 14.].map(|v| Sample::Position([v, 0., 0.]));
        for (kind, expected) in [
            (0, 3.671875),
            (1, 3.671875),
            (2, 5.061467),
            (3, 2.6089637),
            (4, 3.25),
            (5, 4.4114585),
            (6, 4.),
            (7, 3.2889204),
            (8, 3.2331812),
            (9, 3.3446593),
            (10, 3.25),
            (11, 3.671875),
            (12, 3.671875),
            (13, 3.671875),
        ] {
            assert!(
                (position(interpolate_curve(kind, samples, 0.25))[0] - expected).abs() < 0.000002,
                "curve {kind}"
            );
        }
        let q = bone_math::angle_quaternion([0., 0., std::f32::consts::FRAC_PI_2]);
        let samples = [
            Sample::zero(true),
            Sample::zero(true),
            Sample::Rotation(q),
            Sample::Rotation(q),
        ];
        for kind in [0, 1, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
            let Sample::Rotation(actual) = interpolate_curve(kind, samples, 0.25) else {
                unreachable!()
            };
            let matrix = bone_math::quaternion_local_transform(actual, [0.; 3]);
            assert!(
                (matrix[0] - (std::f32::consts::PI / 8.).cos()).abs() < 0.000002,
                "quaternion curve {kind}"
            );
            assert!(
                (matrix[4] - (std::f32::consts::PI / 8.).sin()).abs() < 0.000002,
                "quaternion curve {kind}"
            );
        }
        let mut samples = layer(&[2000, 8000], &[2., 8.]);
        let curve = CurveInfo {
            default: 6 * 257,
            edge_values: [
                Sample::Position([-2., 0., 0.]),
                Sample::Position([20., 0., 0.]),
            ],
            edge_curves: [6 * 257; 2],
            right_time: 10000,
        };
        for (time, expected) in [(1000, 0.), (5000, 5.), (9000, 14.), (10001, 0.)] {
            assert_eq!(
                position(samples.sample(time, Some(&curve), Sample::zero(false)))[0],
                expected
            );
        }
        samples.curves = vec![6 * 256 + 15, 6 * 257];
        assert_eq!(
            position(samples.sample(5000, Some(&curve), Sample::zero(false)))[0],
            2.
        );
        samples.curves = vec![6 * 257, 15 * 256 + 6];
        assert_eq!(
            position(samples.sample(5000, Some(&curve), Sample::zero(false)))[0],
            8.
        );
    }

    fn element_node(name: &str, class: &str, attributes: Vec<(&str, Value)>) -> Node {
        Node {
            id: None,
            name: name.into(),
            class: class.into(),
            attributes: attributes
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        }
    }

    fn fixture() -> (Document, Skeleton) {
        let q = bone_math::angle_quaternion([0., 0., std::f32::consts::FRAC_PI_2]);
        let document = Document {
            root: 0,
            format: "model".into(),
            version: 18,
            nodes: vec![
                element_node(
                    "scene",
                    "DmElement",
                    vec![("animationList", Value::Element(Some(1)))],
                ),
                element_node(
                    "animations",
                    "DmeAnimationList",
                    vec![("animations", Value::ElementArray(vec![Some(2)]))],
                ),
                element_node(
                    "motion",
                    "DmeChannelsClip",
                    vec![
                        ("timeFrame", Value::Element(Some(3))),
                        ("frameRate", Value::Int(4)),
                        ("channels", Value::ElementArray(vec![Some(6), Some(9)])),
                    ],
                ),
                element_node(
                    "time",
                    "DmeTimeFrame",
                    vec![
                        ("startTime", Value::Int(4100)),
                        ("durationTime", Value::Int(7400)),
                        ("offsetTime", Value::Int(1000)),
                        ("scale", Value::Float(2.)),
                    ],
                ),
                element_node(
                    "root",
                    "DmeTransform",
                    vec![("position", Value::Vector3([10., 20., 30.]))],
                ),
                element_node(
                    "child",
                    "DmeTransform",
                    vec![("position", Value::Vector3([1., 2., 3.]))],
                ),
                element_node(
                    "root_position",
                    "DmeChannel",
                    vec![
                        ("toElement", Value::Element(Some(4))),
                        ("toAttribute", Value::String("position".into())),
                        ("log", Value::Element(Some(7))),
                    ],
                ),
                element_node(
                    "positions",
                    "DmeVector3Log",
                    vec![("layers", Value::ElementArray(vec![Some(8)]))],
                ),
                element_node(
                    "positions",
                    "DmeVector3LogLayer",
                    vec![
                        ("times", Value::TimeArray(vec![0, 20000])),
                        ("values", Value::Vector3Array(vec![[0.; 3], [20., 0., 0.]])),
                    ],
                ),
                element_node(
                    "root_orientation",
                    "DmeChannel",
                    vec![
                        ("toElement", Value::Element(Some(4))),
                        ("toAttribute", Value::String("orientation".into())),
                        ("log", Value::Element(Some(10))),
                    ],
                ),
                element_node(
                    "rotations",
                    "DmeQuaternionLog",
                    vec![("layers", Value::ElementArray(vec![Some(11)]))],
                ),
                element_node(
                    "rotations",
                    "DmeQuaternionLogLayer",
                    vec![
                        ("times", Value::IntArray(vec![0, 20000])),
                        ("values", Value::QuaternionArray(vec![[0., 0., 0., 1.], q])),
                    ],
                ),
            ],
        };
        let skeleton = Skeleton {
            nodes: vec![
                SmdNode {
                    index: 0,
                    name: "root".into(),
                    parent: -1,
                },
                SmdNode {
                    index: 1,
                    name: "child".into(),
                    parent: 0,
                },
            ],
            transforms: vec![Some(4), Some(5)],
            by_transform: HashMap::from([(4, 0), (5, 1)]),
            by_dag: HashMap::new(),
            joints: vec![0, 1],
            bind: vec![
                bone_math::quaternion_local_transform([0., 0., 0., 1.], [100., 0., 0.]),
                bone_math::IDENTITY,
            ],
            default_bone: 0,
        };
        (document, skeleton)
    }

    #[test]
    fn clip_sampling_maps_timeframe_offset_scale_and_preserves_unkeyed_local_transforms() {
        let (document, skeleton) = fixture();
        let opts = FbxOpts {
            axis: Some(ForcedAxis::Y),
            scale: 2.,
            ..Default::default()
        };
        let smd = read(&document, skeleton, Some("MOTION"), None, &opts).unwrap();
        assert_eq!(smd.frames.len(), 4);
        for (frame, position) in smd.frames.iter().zip([2., 7., 12., 16.8]) {
            assert!((frame.poses[0].position[0] - position * 2.).abs() < 0.00001);
            assert_eq!(frame.poses[1].position, [2., 4., 6.]);
            let rotation = bone_math::angle_matrix(frame.poses[0].rotation);
            let angle = position / 20. * std::f32::consts::FRAC_PI_2;
            let expected = [
                angle.cos(),
                -angle.sin(),
                0.,
                0.,
                0.,
                0.,
                -1.,
                0.,
                angle.sin(),
                angle.cos(),
                0.,
                0.,
            ];
            for (actual, expected) in rotation.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 0.000002,
                    "rotation {actual} != {expected}"
                );
            }
        }
        assert_eq!(frame_at_time(333, 30.), 1);
        assert_eq!(frame_at_time(332, 30.), 0);
        assert_eq!(frame_at_time(-334, 30.), -1);
        assert_eq!(frame_at_time(-335, 30.), -2);
        assert_eq!(sample_time(0, 30., 0, -5, 0.3).unwrap(), -1);
        let (mut document, skeleton) = fixture();
        document.nodes[2].attributes.remove("timeFrame");
        let single = read(&document, skeleton, None, None, &FbxOpts::default()).unwrap();
        assert_eq!(single.frames.len(), 1);
        assert_eq!(single.frames[0].poses[0].position, [0.; 3]);

        let (mut document, skeleton) = fixture();
        document.nodes[0].attributes.remove("animationList");
        let bind = read(&document, skeleton, None, None, &FbxOpts::default()).unwrap();
        assert_eq!(bind.frames[0].poses[0].position, [100., 0., 0.]);
    }

    #[test]
    fn malformed_quaternions_and_overflowing_samples_fail_before_export() {
        let (mut document, skeleton) = fixture();
        document.nodes[11].attributes.insert(
            "values".into(),
            Value::QuaternionArray(vec![[f32::MAX; 4]; 2]),
        );
        assert!(
            read(&document, skeleton, None, None, &FbxOpts::default())
                .unwrap_err()
                .contains("invalid values")
        );
        let (mut document, skeleton) = fixture();
        document.nodes[8].attributes.insert(
            "values".into(),
            Value::Vector3Array(vec![[-f32::MAX, 0., 0.], [f32::MAX, 0., 0.]]),
        );
        assert!(
            read(&document, skeleton, None, None, &FbxOpts::default())
                .unwrap_err()
                .contains("nonfinite transform")
        );
    }

    #[test]
    fn channels_pass_through_empty_logs_and_ignore_non_skeletal_outputs_like_source() {
        let (mut document, skeleton) = fixture();
        document.nodes.push(element_node(
            "flex_weight",
            "DmeChannel",
            vec![
                ("toElement", Value::Element(Some(0))),
                ("toAttribute", Value::String("weight".into())),
            ],
        ));
        document.nodes.push(element_node(
            "child_copy",
            "DmeChannel",
            vec![
                ("toElement", Value::Element(Some(5))),
                ("toAttribute", Value::String("position".into())),
                ("fromElement", Value::Element(Some(4))),
                ("fromAttribute", Value::String("position".into())),
                ("log", Value::Element(Some(14))),
            ],
        ));
        document.nodes.push(element_node(
            "empty",
            "DmeVector3Log",
            vec![("layers", Value::ElementArray(vec![Some(15)]))],
        ));
        document
            .nodes
            .push(element_node("empty", "DmeVector3LogLayer", vec![]));
        document.nodes[2].attributes.insert(
            "channels".into(),
            Value::ElementArray(vec![Some(13), Some(6), Some(9), Some(12)]),
        );
        let (result, warnings) = crate::diag::capture_warnings(|| {
            read(&document, skeleton, None, None, &FbxOpts::default())
        });
        let smd = result.unwrap();
        assert!(warnings.iter().any(|warning| warning.contains("flex_weight") && warning.contains("exported bone")));
        for (frame, expected) in smd.frames.iter().zip([2., 7., 12., 16.8]) {
            assert!((frame.poses[1].position[0] - expected).abs() < 0.00001);
            assert_eq!(frame.poses[1].position, frame.poses[0].position);
        }
        let (_, skeleton) = fixture();
        document.nodes[6]
            .attributes
            .insert("fromElement".into(), Value::Element(Some(5)));
        document.nodes[6]
            .attributes
            .insert("fromAttribute".into(), Value::String("position".into()));
        document.nodes[6]
            .attributes
            .insert("log".into(), Value::Element(Some(14)));
        assert!(
            read(&document, skeleton, None, None, &FbxOpts::default())
                .unwrap_err()
                .contains("cyclic")
        );
    }
}
