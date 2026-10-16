use miniz_oxide::{
    DataFormat, MZFlush, MZStatus,
    inflate::stream::{InflateState, inflate as step},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InflateError;

pub(super) const WORKING_BYTES: usize = std::mem::size_of::<InflateState>();

pub(super) fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, InflateError> {
    decode(data, expected, false)
}

pub(super) fn prefix(data: &[u8], expected: usize) -> Result<Vec<u8>, InflateError> {
    decode(data, expected, true)
}

fn decode(data: &[u8], expected: usize, prefix: bool) -> Result<Vec<u8>, InflateError> {
    let mut output = vec![0; expected];
    if prefix && expected == 0 {
        return Ok(output);
    }
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut input_offset = 0;
    let mut output_offset = 0;
    let mut extra = [0];
    loop {
        let overflow = output_offset == expected;
        let target = if overflow {
            &mut extra[..]
        } else {
            &mut output[output_offset..]
        };
        let result = step(&mut state, &data[input_offset..], target, MZFlush::None);
        input_offset += result.bytes_consumed;
        if overflow && result.bytes_written != 0 {
            return Err(InflateError);
        }
        output_offset += result.bytes_written;
        if prefix && output_offset == expected {
            return Ok(output);
        }
        match result.status {
            Ok(MZStatus::StreamEnd) if output_offset == expected => return Ok(output),
            Ok(MZStatus::Ok) if result.bytes_consumed != 0 || result.bytes_written != 0 => {}
            _ => return Err(InflateError),
        }
    }
}
