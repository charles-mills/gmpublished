use std::io::{BufRead, BufWriter, ErrorKind, Write};

use crate::Transaction;

pub(crate) fn stream_bytes<R: BufRead + ?Sized, W: Write>(
    r: &mut R,
    w: &mut BufWriter<W>,
    mut bytes: u64,
    transaction: Option<&Transaction>,
) -> Result<(), std::io::Error> {
    let bytes_f = bytes as f64;
    let mut consumed_total: f64 = 0.;

    while bytes > 0 {
        let consumed = match r.fill_buf() {
            Ok([]) => {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "input ended before the declared byte count",
                ));
            }
            Ok(data) => {
                let consumed = usize::try_from(bytes.min(data.len() as u64))
                    .expect("chunk length is bounded by a usize buffer");
                w.write_all(&data[..consumed])?;
                consumed
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        r.consume(consumed);
        bytes -= consumed as u64;

        if let Some(transaction) = transaction {
            consumed_total += consumed as f64;
            transaction.progress(consumed_total / bytes_f);
        }
    }

    Ok(())
}

pub(crate) fn write_nt_string(writer: &mut impl Write, value: &str) -> Result<(), std::io::Error> {
    writer.write_all(value.as_bytes())?;
    writer.write_all(&[0])
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Cursor, ErrorKind};

    use super::stream_bytes;

    #[test]
    fn a_short_source_is_not_silently_accepted() {
        let mut reader = BufReader::new(Cursor::new(b"short"));
        let mut writer = BufWriter::new(Vec::new());

        let error = stream_bytes(&mut reader, &mut writer, 6, None).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_declared_prefix_does_not_consume_following_bytes() {
        let mut reader = BufReader::new(Cursor::new(b"payloadtrailer"));
        let mut writer = BufWriter::new(Vec::new());

        stream_bytes(&mut reader, &mut writer, 7, None).unwrap();

        assert_eq!(writer.into_inner().unwrap(), b"payload");
    }
}
