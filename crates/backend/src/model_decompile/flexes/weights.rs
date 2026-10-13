use mdlc::vta::VtaVert;

use super::{DecompileError, Shape, half, invalid, same_half};

pub(super) fn absolute(base: f32, delta: f32) -> Result<f32, DecompileError> {
    let bits = mdlc::anim_writer::float_to_half_public(delta);
    let next = half(bits.wrapping_add(1));
    for adjustment in [0.0, 0.25, 0.5, 0.75] {
        let value = base + delta + (next - delta) * adjustment;
        if value.is_finite()
            && (value - base == delta
                || mdlc::anim_writer::float_to_half_public(value - base) == bits)
        {
            return Ok(value);
        }
    }
    Err(invalid(
        "A flex delta cannot be represented as an absolute VTA coordinate at this model scale",
    ))
}

pub(super) fn infer_decay(
    shape: &Shape,
    base: &[VtaVert],
    frame: &mut [VtaVert],
) -> Result<f32, DecompileError> {
    if shape.deltas.iter().all(|delta| delta.speed == 255) {
        return Ok(0.0);
    }
    let lengths: Vec<f32> = frame
        .iter()
        .map(|vertex| {
            let original = &base[vertex.index as usize];
            (0..3)
                .map(|axis| (vertex.pos[axis] - original.pos[axis]).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .collect();
    let maximum = lengths.iter().copied().fold(0.0f32, f32::max).max(
        if lengths.iter().all(|length| *length == 0.0) {
            0.01
        } else {
            0.0
        },
    );
    let (mut low, mut high) = (0.0f64, f64::INFINITY);
    for (delta, length) in shape.deltas.iter().zip(&lengths) {
        let ratio = f64::from(*length / maximum);
        if delta.speed < 255 {
            low = low.max(255.0 * ratio / (f64::from(delta.speed) + 1.0));
        }
        if delta.speed > 0 {
            high = high.min(255.0 * ratio / f64::from(delta.speed));
        }
    }
    for fraction in [0.5, 0.25, 0.75, 0.01, 0.99] {
        let decay = if high.is_finite() {
            (low + (high - low) * fraction) as f32
        } else {
            (low + 1.0) as f32
        };
        if decay > 0.0
            && decay.is_finite()
            && shape.deltas.iter().zip(&lengths).all(|(delta, length)| {
                (255.0 * (*length / (maximum * decay)).clamp(0.0, 1.0)) as u8 == delta.speed
            })
        {
            return Ok(decay);
        }
    }
    recover_decay(shape, base, frame).ok_or_else(|| {
        invalid("The flex speed weights cannot be represented by a common QC decay value")
    })
}

pub(super) fn infer_split(shape: &Shape, base: &[VtaVert]) -> Result<f32, DecompileError> {
    if shape.pair == 0 {
        if shape.deltas.iter().any(|delta| delta.side != 0) {
            return Err(invalid("An unpaired flex contains side weights"));
        }
        return Ok(0.0);
    }
    if shape.deltas.iter().all(|delta| delta.side == 0) {
        return Ok(0.0);
    }
    let (mut low, mut high) = (f64::NEG_INFINITY, f64::INFINITY);
    for (delta, vertex) in shape.deltas.iter().zip(base) {
        let x = f64::from(vertex.pos[0]);
        if x == 0.0 {
            if delta.side != 127 {
                return Err(invalid(
                    "A paired flex has incompatible side weights at X=0",
                ));
            }
            continue;
        }
        let a = if delta.side == 0 {
            f64::NEG_INFINITY
        } else {
            2.0 * inverse_smooth(f64::from(delta.side) / 255.0) - 1.0
        };
        let b = if delta.side == 255 {
            f64::INFINITY
        } else {
            2.0 * inverse_smooth((f64::from(delta.side) + 1.0) / 255.0) - 1.0
        };
        let (a, b) = if x > 0.0 {
            (a / x, b / x)
        } else {
            (b / x, a / x)
        };
        low = low.max(a);
        high = high.min(b);
    }
    for fraction in [0.5, 0.25, 0.75, 0.01, 0.99] {
        let reciprocal = match (low.is_finite(), high.is_finite()) {
            (true, true) => low + (high - low) * fraction,
            (true, false) => low + 1.0,
            (false, true) => high - 1.0,
            (false, false) => 1.0,
        };
        let split = reciprocal.recip() as f32;
        if split != 0.0
            && split.is_finite()
            && shape
                .deltas
                .iter()
                .zip(base)
                .all(|(delta, vertex)| side(vertex.pos[0], split) == delta.side)
        {
            return Ok(split);
        }
    }
    Err(invalid(
        "The paired flex side weights cannot be represented by a common QC split value",
    ))
}

fn inverse_smooth(value: f64) -> f64 {
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..48 {
        let middle = (low + high) * 0.5;
        if middle * middle * (3.0 - 2.0 * middle) < value {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) * 0.5
}

fn side(x: f32, split: f32) -> u8 {
    if split == 0.0 {
        return 0;
    }
    let t = ((split - x) / (2.0 * split)).clamp(0.0, 1.0);
    (255.0 * (1.0 - (3.0 * t * t - 2.0 * t * t * t))) as u8
}

fn length(position: [f32; 3], base: [f32; 3]) -> f32 {
    let delta = std::array::from_fn::<_, 3, _>(|axis| position[axis] - base[axis]);
    (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt()
}

fn coordinate(base: f32, delta: f32, fraction: f32) -> Option<f32> {
    let bits = mdlc::anim_writer::float_to_half_public(delta);
    let next = half(bits.wrapping_add(1));
    let position = base + delta + (next - delta) * fraction;
    if position.is_finite()
        && (position - base == delta
            || mdlc::anim_writer::float_to_half_public(position - base) == bits)
    {
        Some(position)
    } else {
        None
    }
}

fn recover_decay(shape: &Shape, base: &[VtaVert], frame: &mut [VtaVert]) -> Option<f32> {
    for keep_x in [true, false] {
        let mut bounds = Vec::new();
        let (mut low, mut high) = (0.0f64, f64::INFINITY);
        for (delta, vertex) in shape.deltas.iter().zip(frame.iter()) {
            let original = base[delta.vertex].pos;
            let mut lower = vertex.pos;
            let mut upper = vertex.pos;
            for axis in usize::from(keep_x)..3 {
                let mut candidates = [0.0, 0.1, 0.5, 0.9, 0.9999]
                    .into_iter()
                    .filter_map(|fraction| coordinate(original[axis], delta.delta[axis], fraction))
                    .collect::<Vec<_>>();
                candidates.push(vertex.pos[axis]);
                candidates.sort_by(|a, b| {
                    (a - original[axis])
                        .abs()
                        .total_cmp(&(b - original[axis]).abs())
                });
                lower[axis] = candidates[0];
                upper[axis] = *candidates.last()?;
            }
            let min = length(lower, original);
            let max = length(upper, original);
            if delta.speed < 255 {
                low = low.max(255.0 * f64::from(min) / (f64::from(delta.speed) + 1.0));
            }
            if delta.speed > 0 {
                high = high.min(255.0 * f64::from(max) / f64::from(delta.speed));
            }
            bounds.push((lower, upper, min, max));
        }
        if low >= high {
            continue;
        }
        for fraction in [0.5, 0.25, 0.75, 0.01, 0.99] {
            let product = if high.is_finite() {
                low + (high - low) * fraction
            } else {
                low + 1.0
            };
            let mut candidate = frame.to_vec();
            for ((delta, vertex), (lower, upper, min, max)) in
                shape.deltas.iter().zip(&mut candidate).zip(&bounds)
            {
                let required_min = (product * f64::from(delta.speed) / 255.0).max(f64::from(*min));
                let required_max = if delta.speed == 255 {
                    f64::from(*max)
                } else {
                    (product * (f64::from(delta.speed) + 1.0) / 255.0).min(f64::from(*max))
                };
                let target = ((required_min + required_max) * 0.5) as f32;
                let (mut start, mut end) = (0.0, 1.0);
                for _ in 0..32 {
                    let middle = (start + end) * 0.5;
                    vertex.pos = std::array::from_fn(|axis| {
                        lower[axis] + (upper[axis] - lower[axis]) * middle
                    });
                    if length(vertex.pos, base[delta.vertex].pos) < target {
                        start = middle;
                    } else {
                        end = middle;
                    }
                }
                vertex.pos = [start, end, 0.0, 1.0]
                    .into_iter()
                    .map(|fraction| {
                        std::array::from_fn(|axis| {
                            lower[axis] + (upper[axis] - lower[axis]) * fraction
                        })
                    })
                    .find(|position| {
                        let ratio = length(*position, base[delta.vertex].pos) / product as f32;
                        (255.0 * ratio.clamp(0.0, 1.0)) as u8 == delta.speed
                    })
                    .unwrap_or(vertex.pos);
            }
            let lengths = candidate
                .iter()
                .map(|vertex| length(vertex.pos, base[vertex.index as usize].pos))
                .collect::<Vec<_>>();
            let maximum = lengths.iter().copied().fold(0.0f32, f32::max);
            let decay = (product / f64::from(maximum)) as f32;
            if decay > 0.0
                && decay.is_finite()
                && shape.deltas.iter().zip(&candidate).zip(lengths).all(
                    |((delta, vertex), length)| {
                        let position_delta = std::array::from_fn(|axis| {
                            vertex.pos[axis] - base[delta.vertex].pos[axis]
                        });
                        same_half(position_delta, delta.delta)
                            && (255.0 * (length / (maximum * decay)).clamp(0.0, 1.0)) as u8
                                == delta.speed
                    },
                )
            {
                frame.copy_from_slice(&candidate);
                return Some(decay);
            }
        }
    }
    None
}
