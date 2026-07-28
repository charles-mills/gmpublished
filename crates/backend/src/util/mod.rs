pub mod path;

mod macros;
pub(crate) use macros::NUM_THREADS;
pub(crate) use macros::available_parallelism_count;
pub(crate) use macros::{main_thread_forbidden, thread_pool};

mod stream;
pub(crate) use stream::{stream_bytes, write_nt_string};
