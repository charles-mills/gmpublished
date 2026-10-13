use super::{DecompileError, Reader, invalid};

pub(super) fn identifier(name: &str) -> Result<&str, DecompileError> {
    if !name
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || name.eq_ignore_ascii_case("max")
        || name.eq_ignore_ascii_case("min")
    {
        return Err(invalid(
            "A flex rule name cannot be represented as a QC expression identifier",
        ));
    }
    Ok(name)
}

pub(super) fn expression(
    reader: &Reader<'_>,
    ops: &[usize],
    descriptors: &[String],
    controllers: &[String],
    ranges: &[(f32, f32)],
) -> Result<String, DecompileError> {
    let mut stack: Vec<String> = Vec::new();
    for record in ops {
        let opcode = reader.count(*record)?;
        match opcode {
            1 => {
                let value = reader.float(record + 4)?;
                if value < 0.0 {
                    stack.push(format!("(-{})", -value));
                } else {
                    stack.push(value.to_string());
                }
            }
            2 => stack.push(
                identifier(&controllers[reader.index(record + 4, controllers.len())?])?.to_owned(),
            ),
            3 => stack.push(format!(
                "%{}",
                identifier(&descriptors[reader.index(record + 4, descriptors.len())?])?
            )),
            8 => {
                let value = stack
                    .pop()
                    .ok_or_else(|| invalid("A flex rule underflows its stack"))?;
                stack.push(format!("(-{value})"));
            }
            4..=7 | 13 | 14 => {
                let right = stack
                    .pop()
                    .ok_or_else(|| invalid("A flex rule underflows its stack"))?;
                let left = stack
                    .pop()
                    .ok_or_else(|| invalid("A flex rule underflows its stack"))?;
                stack.push(match opcode {
                    13 => format!("max({left},{right})"),
                    14 => format!("min({left},{right})"),
                    _ => format!("({left}{}{right})", ["+", "-", "*", "/"][opcode - 4]),
                });
            }
            15 | 16 => {
                let controller =
                    identifier(&controllers[reader.index(record + 4, controllers.len())?])?;
                let value = if opcode == 15 {
                    format!("(-{controller})")
                } else {
                    controller.to_owned()
                };
                stack.push(clamp(&value));
            }
            18 | 19 => {
                let count = reader.count(record + 4)?;
                let required = count + usize::from(opcode == 19);
                if count == 0 || stack.len() < required {
                    return Err(invalid("An advanced flex rule underflows its stack"));
                }
                let values = stack.split_off(stack.len() - count);
                let product = values
                    .into_iter()
                    .reduce(|left, right| format!("({left}*{right})"))
                    .unwrap();
                if opcode == 19 {
                    let value = stack.pop().unwrap();
                    stack.push(format!("({value}*(1-{product}))"));
                } else {
                    stack.push(product);
                }
            }
            17 => {
                if stack.len() < 5 {
                    return Err(invalid("An Nway flex rule underflows its stack"));
                }
                let values = stack.split_off(stack.len() - 5);
                let controller = controller_index(&values[4], controllers.len())?;
                let ramp = values[..4]
                    .iter()
                    .map(|value| constant(value))
                    .collect::<Result<Vec<_>, _>>()?;
                if ramp.windows(2).any(|window| window[0] > window[1])
                    || ramp[1] - ramp[0] <= 0.0001
                    || ramp[3] - ramp[2] <= 0.0001
                {
                    return Err(invalid(
                        "An Nway flex rule has a discontinuous or excessively narrow activation ramp",
                    ));
                }
                let input = identifier(&controllers[controller])?;
                let up = clamp(&format!(
                    "(({input}-{})/{})",
                    number(ramp[0]),
                    number(ramp[1] - ramp[0])
                ));
                let down = clamp(&format!(
                    "(({}-{input})/{})",
                    number(ramp[3]),
                    number(ramp[3] - ramp[2])
                ));
                let multiplier =
                    identifier(&controllers[reader.index(record + 4, controllers.len())?])?;
                stack.push(format!("(min({up},{down})*{multiplier})"));
            }
            20 | 21 => {
                if stack.len() < 3 {
                    return Err(invalid("An eyelid flex rule underflows its stack"));
                }
                let values = stack.split_off(stack.len() - 3);
                let eye = constant(&values[0])?;
                controller_index(&values[1], controllers.len())?;
                let close = controller_index(&values[2], controllers.len())?;
                let vertical = reader.index(record + 4, controllers.len())?;
                let close = normalized(close, controllers, ranges)?;
                let vertical = normalized(vertical, controllers, ranges)?;
                let eye = if eye < 0.0 {
                    "0".to_owned()
                } else {
                    let index = controller_index(&values[0], controllers.len())?;
                    format!("(2*{}-1)", normalized(index, controllers, ranges)?)
                };
                if opcode == 20 {
                    stack.push(format!("(((1-max(0,{eye}))*(1-{vertical}))*{close})"));
                } else {
                    stack.push(format!("(((1+min(0,{eye}))*{vertical})*{close})"));
                }
            }
            _ => {
                return Err(invalid(format!(
                    "Flex rule opcode {opcode} cannot yet be reconstructed as standard QC"
                )));
            }
        }
        if stack.iter().map(String::len).sum::<usize>() > 1_048_576 {
            return Err(DecompileError::SizeLimit);
        }
    }
    if stack.len() != 1 {
        return Err(invalid("A flex rule does not produce exactly one value"));
    }
    Ok(stack.pop().unwrap())
}

fn number(value: f32) -> String {
    if value < 0.0 {
        format!("(-{})", -value)
    } else {
        value.to_string()
    }
}

fn constant(value: &str) -> Result<f32, DecompileError> {
    let number = value
        .strip_prefix("(-")
        .and_then(|value| value.strip_suffix(')'))
        .map_or_else(
            || value.parse::<f32>(),
            |value| value.parse::<f32>().map(|value| -value),
        );
    number.map_err(|_| invalid("An advanced flex rule uses a nonconstant controller index or ramp"))
}

fn controller_index(value: &str, count: usize) -> Result<usize, DecompileError> {
    let value = constant(value)?;
    if value < 0.0 || value.fract() != 0.0 || value >= count as f32 {
        return Err(invalid(
            "An advanced flex rule references an absent controller",
        ));
    }
    Ok(value as usize)
}

fn clamp(value: &str) -> String {
    format!("max(0,min(1,{value}))")
}

fn normalized(
    index: usize,
    controllers: &[String],
    ranges: &[(f32, f32)],
) -> Result<String, DecompileError> {
    let (min, max) = ranges[index];
    if max - min <= 0.0001 {
        return Err(invalid(
            "An eyelid flex controller range is too narrow to reconstruct using standard QC",
        ));
    }
    Ok(clamp(&format!(
        "(({}-{})/{})",
        identifier(&controllers[index])?,
        number(min),
        number(max - min)
    )))
}
