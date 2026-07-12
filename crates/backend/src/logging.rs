use std::{
    fs::{File, OpenOptions},
    io::Write,
    panic::PanicHookInfo,
    path::PathBuf,
    sync::{
        LazyLock, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use std::sync::mpsc::Sender;

const LOG_FILE_NAME: &str = "gmpublished.log";
const LOCAL_TARGET_PREFIX: &str = "gmpublished";

struct BackendLogger;

static LOGGER: BackendLogger = BackendLogger;
static LEVEL_CONFIG: OnceLock<LevelConfig> = OnceLock::new();
static FILE_SINK_READY: AtomicBool = AtomicBool::new(false);
// Process-wide by necessity: `panic::set_hook` installs one global hook that
// has no way to receive a `&Backend`. Set once, by whichever `Backend`
// finishes construction first (`enable_file_sink`).
static LOGS_DIR: OnceLock<PathBuf> = OnceLock::new();

impl log::Log for BackendLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        should_log_target(metadata.target(), metadata.level(), level_config())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) && !is_renderer_selection_record(record) {
            return;
        }

        let line = format!(
            "[{}] [{}] {}",
            record.level(),
            record.target(),
            record.args()
        );

        match record.level() {
            log::Level::Error | log::Level::Warn => std::eprintln!("{line}"),
            log::Level::Info | log::Level::Debug | log::Level::Trace => std::println!("{line}"),
        }

        let _ = LOG_CHANNEL.send(line);
    }

    fn flush(&self) {}
}

static LOG_CHANNEL: LazyLock<Sender<String>> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut file = None;
        let mut backlog: Vec<String> = Vec::new();

        while let Ok(log) = rx.recv() {
            if !FILE_SINK_READY.load(Ordering::Acquire) {
                backlog.push(log);
                continue;
            }

            if file.is_none() {
                file = open_log_file();
            }

            for pending in std::mem::take(&mut backlog) {
                write_log_line(&mut file, &pending);
            }
            write_log_line(&mut file, &log);
        }
    });
    tx
});

fn write_log_line(file: &mut Option<File>, log: &str) {
    if let Some(open_file) = file.as_mut()
        && writeln!(open_file, "{log}").is_err()
    {
        *file = None;
    }
}

pub fn enable_file_sink(logs_dir: PathBuf) {
    let _ = LOGS_DIR.set(logs_dir);
    FILE_SINK_READY.store(true, Ordering::Release);
}

/// Idempotent: `log::set_logger`/`panic::set_hook` are one-shot process
/// resources, so a second `Backend` built in the same process (tests, or a
/// hypothetical re-init) reuses the first one's install rather than erroring.
pub fn install() -> Result<(), log::SetLoggerError> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    static INSTALL_LOCK: Mutex<()> = Mutex::new(());
    if INSTALLED.get().is_some() {
        return Ok(());
    }

    let _install_guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if INSTALLED.get().is_some() {
        return Ok(());
    }

    log::set_logger(&LOGGER)?;
    let config = configured_level_config();
    let _ = LEVEL_CONFIG.set(config);
    log::set_max_level(config.global);
    std::panic::set_hook(Box::new(panic));
    let _ = INSTALLED.set(());
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LevelConfig {
    local: log::LevelFilter,
    external: log::LevelFilter,
    global: log::LevelFilter,
}

impl LevelConfig {
    fn new(local: log::LevelFilter, external: log::LevelFilter) -> Self {
        Self {
            local,
            external,
            global: more_permissive_level(local, external),
        }
    }
}

fn level_config() -> LevelConfig {
    LEVEL_CONFIG.get().map_or_else(
        || LevelConfig::new(log::LevelFilter::Info, log::LevelFilter::Warn),
        |config| *config,
    )
}

fn configured_level_config() -> LevelConfig {
    level_config_for_session(
        std::env::var("GMPUBLISHED_LOG")
            .ok()
            .as_deref()
            .map_or(log::LevelFilter::Info, parse_level_filter),
    )
}

fn level_config_for_session(session: log::LevelFilter) -> LevelConfig {
    let external = match session {
        log::LevelFilter::Debug | log::LevelFilter::Trace => session,
        _ => log::LevelFilter::Warn,
    };
    LevelConfig::new(session, external)
}

fn more_permissive_level(left: log::LevelFilter, right: log::LevelFilter) -> log::LevelFilter {
    if left as usize >= right as usize {
        left
    } else {
        right
    }
}

// iced_wgpu reports the adapter it picked ("Selected: AdapterInfo { name,
// backend, .. }") at Info, below the external-target Warn cap. Surfacing that
// one record keeps a silent fallback to the GL backend (e.g. libvulkan.so.1
// missing on Linux) visible in every startup log. The message match formats
// the record args, so it must stay behind the cheap level/target checks.
fn is_renderer_selection_record(record: &log::Record<'_>) -> bool {
    record.level() == log::Level::Info
        && record.target().starts_with("iced_wgpu")
        && record.args().to_string().starts_with("Selected:")
}

fn should_log_target(target: &str, level: log::Level, config: LevelConfig) -> bool {
    let effective_level = if target.starts_with(LOCAL_TARGET_PREFIX) {
        config.local
    } else {
        config.external
    };
    level.to_level_filter() <= effective_level
}

fn parse_level_filter(value: &str) -> log::LevelFilter {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    }
}

fn open_log_file() -> Option<File> {
    std::panic::catch_unwind(|| {
        let logs_dir = LOGS_DIR.get()?;
        std::fs::create_dir_all(logs_dir).ok()?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs_dir.join(LOG_FILE_NAME))
            .ok()
    })
    .ok()
    .flatten()
}

fn panic(panic: &PanicHookInfo<'_>) {
    let backtrace = std::backtrace::Backtrace::force_capture();

    let panic_log = format!("\n\n!!!!!!!!!!!!! PANIC !!!!!!!!!!!!!\n{panic}\n{backtrace}\n");
    // Write synchronously: with `panic = "abort"` the process dies before the
    // async writer thread would drain a channel send. Only touch the log file
    // once appdata is up — resolving the logs dir during an appdata-init panic
    // would re-enter the lazy static.
    if FILE_SINK_READY.load(Ordering::Acquire)
        && let Some(mut file) = open_log_file()
    {
        let _ = writeln!(file, "{panic_log}");
        let _ = file.sync_data();
    }

    std::eprintln!("{panic}\n{backtrace}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_installation_is_idempotent() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    install()
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            assert!(
                thread
                    .join()
                    .expect("install thread should not panic")
                    .is_ok()
            );
        }
    }

    #[test]
    fn log_level_parser_accepts_known_values_and_defaults_invalid_input() {
        assert_eq!(parse_level_filter("error"), log::LevelFilter::Error);
        assert_eq!(parse_level_filter("WARN"), log::LevelFilter::Warn);
        assert_eq!(parse_level_filter(" info "), log::LevelFilter::Info);
        assert_eq!(parse_level_filter("debug"), log::LevelFilter::Debug);
        assert_eq!(parse_level_filter("trace"), log::LevelFilter::Trace);
        assert_eq!(parse_level_filter("verbose"), log::LevelFilter::Info);
        assert_eq!(parse_level_filter(""), log::LevelFilter::Info);
    }

    #[test]
    fn target_filter_uses_session_level_for_local_targets_and_warn_for_externals() {
        let config = level_config_for_session(log::LevelFilter::Info);

        assert_eq!(config.global, log::LevelFilter::Info);
        assert!(should_log_target(
            "gmpublished_backend::appdata",
            log::Level::Info,
            config
        ));
        assert!(!should_log_target(
            "gmpublished_backend::appdata",
            log::Level::Debug,
            config
        ));
        assert!(should_log_target("wgpu_core", log::Level::Warn, config));
        assert!(!should_log_target("wgpu_core", log::Level::Info, config));
    }

    #[test]
    fn target_filter_keeps_external_warn_when_session_level_is_stricter() {
        let config = level_config_for_session(log::LevelFilter::Error);

        assert_eq!(config.global, log::LevelFilter::Warn);
        assert!(should_log_target(
            "gmpublished::main",
            log::Level::Error,
            config
        ));
        assert!(!should_log_target(
            "gmpublished::main",
            log::Level::Warn,
            config
        ));
        assert!(should_log_target("iced_wgpu", log::Level::Warn, config));
        assert!(!should_log_target("iced_wgpu", log::Level::Info, config));
    }

    #[test]
    fn renderer_selection_record_bypasses_external_warn_cap() {
        let selected = log::Record::builder()
            .level(log::Level::Info)
            .target("iced_wgpu::window::compositor")
            .args(format_args!("Selected: AdapterInfo {{ backend: Gl }}"))
            .build();
        assert!(is_renderer_selection_record(&selected));

        let other_info = log::Record::builder()
            .level(log::Level::Info)
            .target("iced_wgpu::window::compositor")
            .args(format_args!("Available adapters: []"))
            .build();
        assert!(!is_renderer_selection_record(&other_info));

        let wrong_target = log::Record::builder()
            .level(log::Level::Info)
            .target("wgpu_core::instance")
            .args(format_args!("Selected: something"))
            .build();
        assert!(!is_renderer_selection_record(&wrong_target));
    }

    #[test]
    fn target_filter_lets_externals_follow_debug_or_trace_sessions() {
        let debug_config = level_config_for_session(log::LevelFilter::Debug);
        assert_eq!(debug_config.global, log::LevelFilter::Debug);
        assert!(should_log_target(
            "cosmic_text",
            log::Level::Debug,
            debug_config
        ));
        assert!(!should_log_target(
            "cosmic_text",
            log::Level::Trace,
            debug_config
        ));

        let trace_config = level_config_for_session(log::LevelFilter::Trace);
        assert_eq!(trace_config.global, log::LevelFilter::Trace);
        assert!(should_log_target("naga", log::Level::Trace, trace_config));
    }
}
