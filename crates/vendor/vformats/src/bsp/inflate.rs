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

pub(super) fn range(
    data: &[u8],
    range: std::ops::Range<usize>,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<u8>, InflateError> {
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut input_offset = 0;
    let mut produced = 0;
    let mut output = vec![0; range.len()];
    let mut discard = [0; 8192];
    loop {
        if cancelled() {
            return Err(InflateError);
        }
        let target = if produced < range.start {
            let count = (range.start - produced).min(discard.len());
            &mut discard[..count]
        } else {
            &mut output[produced - range.start..]
        };
        let result = step(&mut state, &data[input_offset..], target, MZFlush::None);
        input_offset += result.bytes_consumed;
        produced += result.bytes_written;
        if produced == range.end {
            return Ok(output);
        }
        match result.status {
            Ok(MZStatus::Ok) if result.bytes_consumed != 0 || result.bytes_written != 0 => {}
            _ => return Err(InflateError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_range_uses_bounded_discard_and_checks_cancellation() {
        let input=(0..200_000).map(|n|(n%251) as u8).collect::<Vec<_>>();
        let compressed=miniz_oxide::deflate::compress_to_vec(&input,6);
        let mut checks=0;
        assert_eq!(range(&compressed,170_123..170_151,||{checks+=1;false}).unwrap(),input[170_123..170_151]);
        assert!(checks>20);
        let mut checks=0;
        assert_eq!(range(&compressed,170_123..170_151,||{checks+=1;checks==3}),Err(InflateError));
        assert!(range(&compressed,199_999..200_001,||false).is_err());
        assert_eq!(range(&compressed,0..0,||false).unwrap(),Vec::<u8>::new());
    }
}
