macro_rules! main_thread_forbidden {
    () => {
        #[cfg(debug_assertions)]
        debug_assert_ne!(
            std::thread::current().name(),
            Some("main"),
            "This should never be called from the main thread"
        );
    };
}
pub(crate) use main_thread_forbidden;
