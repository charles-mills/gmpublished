pub mod path;

#[macro_use]
mod macros;
pub use macros::NUM_THREADS;
pub use macros::available_parallelism_count;

mod stream;
pub use stream::ArcBytes;
pub use stream::{NTStringWriter, stream_bytes};
