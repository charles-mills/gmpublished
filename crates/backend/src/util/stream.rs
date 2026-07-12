use std::io::{BufRead, BufWriter, ErrorKind, Write};

use crate::Transaction;

pub fn stream_bytes<R: BufRead + ?Sized, W: Write>(
    r: &mut R,
    w: &mut BufWriter<W>,
    mut bytes: usize,
    transaction: Option<&Transaction>,
) -> Result<(), std::io::Error> {
    let bytes_f = bytes as f64;
    let mut consumed_total: f64 = 0.;

    let consumed = loop {
        let consumed = match r.fill_buf() {
            Ok([]) => break 0,
            Ok(data) if data.len() >= bytes => {
                w.write_all(&data[..bytes])?;
                break bytes;
            }
            Ok(data) => {
                w.write_all(data)?;
                bytes -= data.len();
                data.len()
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => 0,
            Err(e) => return Err(e),
        };
        if consumed > 0 {
            r.consume(consumed);

            if let Some(transaction) = transaction {
                consumed_total += consumed as f64;
                transaction.progress(consumed_total / bytes_f);
            }
        }
    };
    if consumed > 0 {
        r.consume(consumed);

        if let Some(transaction) = transaction {
            consumed_total += consumed as f64;
            transaction.progress(consumed_total / bytes_f);
        }
    }

    Ok(())
}

pub fn write_nt_string(writer: &mut impl Write, value: &str) -> Result<(), std::io::Error> {
    writer.write_all(value.as_bytes())?;
    writer.write_all(&[0])
}
