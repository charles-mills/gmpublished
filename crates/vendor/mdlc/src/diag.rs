//! Crowbar reads compiler diagnostics from stdout in StudioMDL compatibility mode.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    static WARNING_CAPTURES: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct WarningCapture(bool);

impl Drop for WarningCapture {
    fn drop(&mut self) {
        if self.0 {
            WARNING_CAPTURES.with(|captures| {
                captures.borrow_mut().pop();
            });
        }
    }
}

pub fn capture_warnings<T>(work: impl FnOnce() -> T) -> (T, Vec<String>) {
    WARNING_CAPTURES.with(|captures| captures.borrow_mut().push(Vec::new()));
    let mut capture = WarningCapture(true);
    let result = work();
    let warnings = WARNING_CAPTURES.with(|captures| captures.borrow_mut().pop().unwrap());
    capture.0 = false;
    (result, warnings)
}

pub fn emit_warning(message: String) {
    WARNING_CAPTURES.with(|captures| {
        if let Some(warnings) = captures.borrow_mut().last_mut() {
            warnings.push(message.clone());
        }
    });
    log::warn!("{message}");
}

#[macro_export]
macro_rules! warning {
    ($($arg:tt)*) => { $crate::diag::emit_warning(format!($($arg)*)) };
}

const CRATE: &str = env!("CARGO_PKG_NAME");

static TO_STDOUT: AtomicBool = AtomicBool::new(false);

pub fn set_to_stdout(v: bool) {
    TO_STDOUT.store(v, Ordering::Relaxed);
}

#[inline]
pub fn to_stdout() -> bool {
    TO_STDOUT.load(Ordering::Relaxed)
}

fn is_own_target(target: &str) -> bool {
    target == CRATE
        || target
            .strip_prefix(CRATE)
            .is_some_and(|rest| rest.starts_with("::"))
}

pub struct DiagLogger;

impl log::Log for DiagLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        is_own_target(metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // A closed output pipe must not abort compilation.
        if to_stdout() {
            let _ = writeln!(std::io::stdout().lock(), "{}", record.args());
        } else {
            let _ = writeln!(std::io::stderr().lock(), "{}", record.args());
        }
    }

    fn flush(&self) {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }
}

static LOGGER: DiagLogger = DiagLogger;

pub fn init(to_stdout: bool) {
    set_to_stdout(to_stdout);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

#[cfg(test)]
mod tests {
    use log::Log as _;

    #[test]
    fn logger_filters_dependencies_and_similarly_named_crates() {
        for (target, expected) in [
            ("mdlc", true),
            ("mdlc::compile", true),
            ("mdlc::qc::parse", true),
            ("mdlcx", false),
            ("mdlc_extra", false),
            ("i18n_embed::requester", false),
            ("parry3d::query::epa3", false),
            ("ureq::cookies", false),
            ("", false),
        ] {
            let metadata = log::Metadata::builder()
                .level(log::Level::Info)
                .target(target)
                .build();
            assert_eq!(super::DiagLogger.enabled(&metadata), expected, "{target}");
        }
    }

    #[test]
    fn warnings_stay_with_their_compile_even_when_nested_threaded_or_panicking() {
        let (_, outer) = super::capture_warnings(|| {
            crate::warning!("outer");
            let (_, inner) = super::capture_warnings(|| crate::warning!("inner"));
            assert_eq!(inner, ["inner"]);
            let child = std::thread::spawn(|| super::capture_warnings(|| crate::warning!("child")))
                .join()
                .unwrap();
            assert_eq!(child.1, ["child"]);
            assert!(
                std::panic::catch_unwind(|| super::capture_warnings(|| {
                    crate::warning!("aborted");
                    panic!("failed compile");
                }))
                .is_err()
            );
            crate::warning!("after");
        });
        assert_eq!(outer, ["outer", "after"]);
        assert!(super::capture_warnings(|| {}).1.is_empty());
    }
}
