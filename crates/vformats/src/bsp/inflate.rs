use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InflateError;

pub(super) fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, InflateError> {
    let limit = u64::try_from(expected)
        .map_err(|_| InflateError)?
        .checked_add(1)
        .ok_or(InflateError)?;
    let mut out = Vec::with_capacity(expected);
    flate2::read::DeflateDecoder::new(data)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|_| InflateError)?;
    (out.len() == expected).then_some(out).ok_or(InflateError)
}
