use super::{DecompileError, Reader, invalid};

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPoseParameter {
    pub name: String,
    pub start: f32,
    pub end: f32,
    pub loop_range: f32,
}

impl PreviewPoseParameter {
    pub fn default_value(&self) -> f32 {
        0.0_f32.clamp(self.start.min(self.end), self.start.max(self.end))
    }

    fn normalized(&self, mut value: f32) -> Option<f32> {
        if !value.is_finite() {
            return None;
        }
        if self.loop_range > 0.0 {
            let shift = self.loop_range * 0.5 - (self.start + self.end) * 0.5;
            value -= self.loop_range * ((value + shift) / self.loop_range).floor();
        }
        let normalized = (value - self.start) / (self.end - self.start);
        normalized.is_finite().then(|| normalized.clamp(0.0, 1.0))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Axis {
    pub parameter: usize,
    keys: Vec<f32>,
}

impl Axis {
    pub(super) fn sample(
        &self,
        parameters: &[PreviewPoseParameter],
        values: &[f32],
    ) -> Option<(usize, f32)> {
        let parameter = parameters.get(self.parameter)?;
        let mut value = parameter.normalized(
            values
                .get(self.parameter)
                .copied()
                .unwrap_or_else(|| parameter.default_value()),
        )?;
        if parameter.loop_range > 0.0 {
            let shift = parameter.loop_range * 0.5 - (parameter.start + parameter.end) * 0.5;
            value -= parameter.loop_range * ((value + shift) / parameter.loop_range).floor();
        }
        let value = value * (parameter.end - parameter.start) + parameter.start;
        let mut index = 0;
        loop {
            let fraction = (value - self.keys[index]) / (self.keys[index + 1] - self.keys[index]);
            if index < self.keys.len() - 2 && fraction > 1.0 {
                index += 1;
            } else {
                return fraction
                    .is_finite()
                    .then(|| (index, fraction.clamp(0.0, 1.0)));
            }
        }
    }
}

pub(super) fn parameters(reader: &Reader<'_>) -> Result<Vec<PreviewPoseParameter>, DecompileError> {
    let records = reader.table(300, 304, 20)?;
    if records.len() > 256 {
        return Err(DecompileError::SizeLimit);
    }
    records
        .into_iter()
        .map(|record| {
            let parameter = PreviewPoseParameter {
                name: reader.string(record, record)?.to_owned(),
                start: reader.f32(record + 8)?,
                end: reader.f32(record + 12)?,
                loop_range: reader.f32(record + 16)?,
            };
            if parameter.start == parameter.end
                || parameter.loop_range < 0.0
                || !(parameter.end - parameter.start).is_finite()
            {
                return Err(invalid("Invalid preview pose parameter range"));
            }
            Ok(parameter)
        })
        .collect()
}

pub(super) fn axis(
    reader: &Reader<'_>,
    record: usize,
    axis: usize,
    grid: [usize; 2],
    parameters: &[PreviewPoseParameter],
) -> Result<Option<Axis>, DecompileError> {
    if grid[axis] == 1 {
        return Ok(None);
    }
    let parameter = reader.i32(record + 76 + axis * 4)?;
    if parameter == -1 {
        return Ok(None);
    }
    let parameter =
        usize::try_from(parameter).map_err(|_| invalid("Invalid blend pose parameter index"))?;
    if parameter >= parameters.len() {
        return Err(invalid("Blend grid references an absent pose parameter"));
    }
    let keys = if reader.i32(record + 160)? == 0 {
        let start = reader.f32(record + 84 + axis * 4)?;
        let end = reader.f32(record + 92 + axis * 4)?;
        (0..grid[axis])
            .map(|index| start + (end - start) * index as f32 / (grid[axis] - 1) as f32)
            .collect::<Vec<_>>()
    } else {
        let table = reader.relative(record, record + 160)?;
        let first = if axis == 0 { 0 } else { grid[0] };
        (0..grid[axis])
            .map(|index| reader.f32(table + (first + index) * 4))
            .collect::<Result<Vec<_>, _>>()?
    };
    let increasing = keys[1] > keys[0];
    if keys.iter().any(|value| !value.is_finite())
        || keys
            .windows(2)
            .any(|window| window[0] == window[1] || (window[1] > window[0]) != increasing)
    {
        return Err(invalid(
            "Blend pose keys must be finite and strictly monotonic",
        ));
    }
    Ok(Some(Axis { parameter, keys }))
}

pub(super) fn triangle(index: [usize; 2], fraction: [f32; 2]) -> [([usize; 2], f32); 3] {
    let [x, y] = index;
    let [s, t] = fraction;
    let (corners, mut weights) = if (x + y) % 2 == 0 {
        if s > t {
            ([[x, y], [x + 1, y], [x + 1, y + 1]], [1.0 - s, s - t, t])
        } else {
            ([[x + 1, y + 1], [x, y + 1], [x, y]], [s, t - s, 1.0 - t])
        }
    } else if s + t > 1.0 {
        (
            [[x + 1, y], [x + 1, y + 1], [x, y + 1]],
            [1.0 - t, s + t - 1.0, 1.0 - s],
        )
    } else {
        ([[x, y + 1], [x, y], [x + 1, y]], [t, 1.0 - s - t, s])
    };
    if weights[1] < 0.001 {
        weights[1] = 0.0;
    }
    weights[2] = 1.0 - weights[0] - weights[1];
    std::array::from_fn(|index| (corners[index], weights[index]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonlinear_keys_reverse_ranges_and_looped_values_select_source_intervals() {
        let parameter = PreviewPoseParameter {
            name: "aim".into(),
            start: -90.0,
            end: 90.0,
            loop_range: 0.0,
        };
        let axis = Axis {
            parameter: 0,
            keys: vec![-90.0, -30.0, 90.0],
        };
        assert_eq!(
            axis.sample(std::slice::from_ref(&parameter), &[-60.0]),
            Some((0, 0.5))
        );
        assert_eq!(
            axis.sample(std::slice::from_ref(&parameter), &[30.0]),
            Some((1, 0.5))
        );
        assert_eq!(
            axis.sample(std::slice::from_ref(&parameter), &[900.0]),
            Some((1, 1.0))
        );
        assert!(
            axis.sample(std::slice::from_ref(&parameter), &[f32::NAN])
                .is_none()
        );
        let reversed = Axis {
            parameter: 0,
            keys: vec![90.0, -30.0, -90.0],
        };
        assert_eq!(reversed.sample(&[parameter], &[30.0]), Some((0, 0.5)));
        let parameter = PreviewPoseParameter {
            name: "yaw".into(),
            start: -180.0,
            end: 180.0,
            loop_range: 360.0,
        };
        let axis = Axis {
            parameter: 0,
            keys: vec![-180.0, 180.0],
        };
        let a = axis
            .sample(std::slice::from_ref(&parameter), &[210.0])
            .unwrap();
        let b = axis
            .sample(std::slice::from_ref(&parameter), &[-150.0])
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(axis.sample(&[parameter], &[180.0]), Some((0, 0.0)));
    }

    #[test]
    fn two_dimensional_blends_follow_alternating_source_triangles() {
        assert_eq!(
            triangle([0, 0], [0.25, 0.75]),
            [([1, 1], 0.25), ([0, 1], 0.5), ([0, 0], 0.25)]
        );
        assert_eq!(
            triangle([1, 0], [0.25, 0.25]),
            [([1, 1], 0.25), ([1, 0], 0.5), ([2, 0], 0.25)]
        );
        assert_eq!(triangle([0, 0], [0.5, 0.5005])[1].1, 0.0);
    }
}
