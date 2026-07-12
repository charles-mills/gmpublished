use std::{
    io::{BufRead, BufWriter, ErrorKind, Write},
    sync::Arc,
};

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

pub trait NTStringWriter: Write {
    fn write_nt_string<S: AsRef<str>>(&mut self, str: S) -> Result<(), std::io::Error> {
        self.write_all(str.as_ref().as_bytes())?;
        self.write_all(&[0])?;
        Ok(())
    }
}
impl NTStringWriter for Vec<u8> {}

#[derive(Clone, Debug)]
pub struct ArcBytes(Arc<[u8]>);
impl ArcBytes {
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
impl AsRef<[u8]> for ArcBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
impl From<Vec<u8>> for ArcBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Arc::from(bytes))
    }
}
