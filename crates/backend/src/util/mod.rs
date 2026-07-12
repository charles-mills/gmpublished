pub mod path;

#[macro_use]
mod macros;
pub use macros::NUM_THREADS;
pub use macros::available_parallelism_count;

mod stream;
pub use stream::{stream_bytes, write_nt_string};
