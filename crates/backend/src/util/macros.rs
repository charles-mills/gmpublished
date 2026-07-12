#[macro_export]
macro_rules! main_thread_forbidden {
    () => {
        #[cfg(debug_assertions)]
        if !$crate::cli::is_cli_mode() {
            debug_assert_ne!(
                std::thread::current().name(),
                Some("main"),
                "This should never be called from the main thread"
            );
        }
    };
}

pub fn available_parallelism_count() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

pub static NUM_THREADS: std::sync::LazyLock<usize> =
    std::sync::LazyLock::new(|| available_parallelism_count().saturating_sub(2).max(2));

#[macro_export]
macro_rules! thread_pool {
    ( $n:expr ) => {
        rayon::ThreadPoolBuilder::new()
            .num_threads(isize::max(
                isize::min($n - 2, $crate::util::available_parallelism_count() as isize),
                2,
            ) as usize)
            .build()
            .unwrap()
    };

    () => {
        rayon::ThreadPoolBuilder::new()
            .num_threads(*$crate::util::NUM_THREADS)
            .build()
            .unwrap()
    };
}
