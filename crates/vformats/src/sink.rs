//! Sans-io write target for the archive writers ([`crate::gma`]).

/// A byte sink. The writers stream into one of these instead of doing
/// I/O; `Vec<u8>` collects in memory, and [`IoSink`] adapts any
/// `std::io::Write`.
pub trait Sink {
    /// The sink's write error.
    type Error;

    /// Write all of `bytes` or fail.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

impl Sink for Vec<u8> {
    type Error = std::convert::Infallible;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Adapts a `std::io::Write` into a [`Sink`].
#[derive(Debug)]
pub struct IoSink<W>(pub W);

impl<W: std::io::Write> Sink for IoSink<W> {
    type Error = std::io::Error;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        std::io::Write::write_all(&mut self.0, bytes)
    }
}
