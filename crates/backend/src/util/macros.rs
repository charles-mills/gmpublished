macro_rules! main_thread_forbidden {
    () => {
        #[cfg(debug_assertions)]
        if !$crate::cli::is_headless() {
            debug_assert_ne!(
                std::thread::current().name(),
                Some("main"),
                "This should never be called from the main thread"
            );
        }
    };
}
pub(crate) use main_thread_forbidden;

pub(crate) fn available_parallelism_count() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

pub(crate) static NUM_THREADS: std::sync::LazyLock<usize> =
    std::sync::LazyLock::new(|| available_parallelism_count().saturating_sub(2).max(2));

/// Threads for a pool asked to be at most `ceiling` wide.
///
/// Two are held back for the main thread and whatever else the process is
/// doing, and the result never drops below two — a one-thread "pool" turns
/// every parallel iterator into a serial one.
pub(crate) fn pool_threads(ceiling: usize) -> usize {
    ceiling
        .min(available_parallelism_count())
        .saturating_sub(2)
        .max(2)
}

macro_rules! thread_pool {
    ( $n:expr ) => {
        rayon::ThreadPoolBuilder::new()
            .num_threads($crate::util::pool_threads($n))
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
pub(crate) use thread_pool;

#[cfg(test)]
mod tests {
    use super::pool_threads;

    /// The ceiling is a request, not the answer: two threads are held back,
    /// and the floor of two stops a "pool" that would serialize every parallel
    /// iterator run through it.
    #[test]
    fn a_pool_holds_two_threads_back_and_never_falls_below_two() {
        let available = super::available_parallelism_count();

        assert!(pool_threads(1) >= 2);
        assert!(pool_threads(2) >= 2);
        assert!(pool_threads(usize::MAX) >= 2);
        assert!(pool_threads(usize::MAX) <= available.max(2));
    }

    /// A ceiling below what the machine offers is what limits the pool.
    #[test]
    fn a_ceiling_under_the_available_width_is_what_binds() {
        if super::available_parallelism_count() >= 8 {
            assert_eq!(pool_threads(8), 6);
        }
    }
}
