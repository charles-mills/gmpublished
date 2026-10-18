use std::fmt::Write as _;

use super::{DecompileError, Reader, invalid, quoted};

pub(super) fn write(
    qc: &mut String,
    reader: &Reader<'_>,
    sequences: &[usize],
    record: usize,
    frames: usize,
) -> Result<(), DecompileError> {
    let layers = relative_table(reader, record, 148, 152, 24)?;
    for layer in layers {
        let index = usize::try_from(reader.i16(layer)?)
            .map_err(|_| invalid("Negative sequence layer index"))?;
        let sequence = *sequences
            .get(index)
            .ok_or_else(|| invalid("Sequence layer references an absent sequence"))?;
        let name = reader.string(sequence, sequence + 4)?;
        let flags = reader.i32(layer + 4)?;
        if flags & !(0x0040 | 0x0080 | 0x0200 | 0x1000 | 0x4000) != 0 {
            return Err(invalid("Unsupported sequence layer flags"));
        }
        let mut times = [0.0; 4];
        for (index, time) in times.iter_mut().enumerate() {
            *time = reader.f32(layer + 8 + index * 4)?;
            if flags & 0x4000 == 0 {
                *time *= frames.saturating_sub(1) as f32;
            }
        }
        if flags == 0 && times == [0.0; 4] {
            writeln!(qc, "    addlayer {}", quoted(name)?).unwrap();
            continue;
        }
        write!(
            qc,
            "    blendlayer {} {} {} {} {}",
            quoted(name)?,
            times[0],
            times[1],
            times[2],
            times[3]
        )
        .unwrap();
        for (bit, keyword) in [
            (0x0040, "spline"),
            (0x0080, "xfade"),
            (0x0200, "noblend"),
            (0x1000, "local"),
        ] {
            if flags & bit != 0 {
                write!(qc, " {keyword}").unwrap();
            }
        }
        if flags & 0x4000 != 0 {
            let index = usize::try_from(reader.i16(layer + 2)?)
                .map_err(|_| invalid("Negative sequence layer pose parameter"))?;
            let parameters = reader.table(300, 304, 20)?;
            let parameter = *parameters
                .get(index)
                .ok_or_else(|| invalid("Sequence layer references an absent pose parameter"))?;
            write!(
                qc,
                " poseparameter {}",
                quoted(reader.string(parameter, parameter)?)?
            )
            .unwrap();
        }
        qc.push('\n');
    }
    let locks = relative_table(reader, record, 164, 168, 32)?;
    let chains = reader.table(284, 288, 16)?;
    for lock in locks {
        let chain = reader.count(lock)?;
        let chain = *chains
            .get(chain)
            .ok_or_else(|| invalid("Sequence lock references an absent IK chain"))?;
        if reader.i32(lock + 12)? != 0 {
            return Err(invalid("Unsupported sequence IK lock flags"));
        }
        writeln!(
            qc,
            "    iklock {} {} {}",
            quoted(reader.string(chain, chain)?)?,
            reader.f32(lock + 4)?,
            reader.f32(lock + 8)?
        )
        .unwrap();
    }
    let size = reader.count(record + 176)?;
    if size != 0 {
        if size > 1_048_576 {
            return Err(DecompileError::SizeLimit);
        }
        let offset = reader.relative(record, record + 172)?;
        let data = reader.bytes(offset, size)?;
        let data = data.strip_suffix(&[0]).unwrap_or(data);
        if data.contains(&0) {
            return Err(invalid("Sequence keyvalues contain embedded nulls"));
        }
        let data =
            std::str::from_utf8(data).map_err(|_| invalid("Sequence keyvalues are not UTF-8"))?;
        let mut values = String::new();
        super::super::metadata::write_keyvalues(&mut values, data)?;
        qc.push_str(values.strip_prefix('$').unwrap());
    }
    Ok(())
}

fn relative_table(
    reader: &Reader<'_>,
    record: usize,
    count_offset: usize,
    index_offset: usize,
    stride: usize,
) -> Result<Vec<usize>, DecompileError> {
    let count = reader.count(record + count_offset)?;
    if count > 65_536 {
        return Err(DecompileError::SizeLimit);
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let start = reader.relative(record, record + index_offset)?;
    reader.bytes(
        start,
        count.checked_mul(stride).ok_or(DecompileError::SizeLimit)?,
    )?;
    Ok((0..count).map(|index| start + index * stride).collect())
}
