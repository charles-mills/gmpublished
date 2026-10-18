use std::{
    backtrace::Backtrace,
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::Write,
    panic::PanicHookInfo,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const LOG_FILE_NAME: &str = "gmpublished.log";
const LOCAL_TARGET_PREFIX: &str = "gmpublished";

/// Drop and count excess log lines rather than block the UI thread.
const LOG_QUEUE_CAPACITY: usize = 4096;

/// Hold early lines until appdata resolves the logs directory. Drop oldest lines past the cap.
const MAX_PENDING_LINES: usize = 4096;

/// Bound on [`flush`](log::Log::flush) and on the shutdown join, so a wedged
/// writer cannot hold process exit.
const LOG_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

struct BackendLogger;

static LOGGER: BackendLogger = BackendLogger;
static LEVEL_CONFIG: OnceLock<LevelConfig> = OnceLock::new();
static FILE_SINK_READY: AtomicBool = AtomicBool::new(false);
// The process panic hook uses this sink without a Backend reference. Initialize it once.
static LOGS_DIR: OnceLock<PathBuf> = OnceLock::new();
static LOG_SINK: OnceLock<LogSink> = OnceLock::new();
/// Report dropped-line counts so bounded queues cannot silently hide gaps.
static DROPPED_LINES: AtomicU64 = AtomicU64::new(0);

impl log::Log for BackendLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        should_log_target(metadata.target(), metadata.level(), level_config())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
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

        if let Some(sink) = LOG_SINK.get() {
            sink.send_line(line);
        }
    }

    fn flush(&self) {
        if let Some(sink) = LOG_SINK.get() {
            sink.flush();
        }
    }
}

enum LogCommand {
    Line(String),
    /// Acknowledges after preceding lines are written. Sending and waiting are
    /// bounded, so a stalled writer may leave lines queued when `flush` returns.
    Flush(SyncSender<()>),
    Stop,
}

struct LogSink {
    sender: SyncSender<LogCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LogSink {
    fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel(LOG_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name("gmpublished-logging".to_owned())
            .spawn(move || writer_loop(&receiver))
            .ok();

        Self {
            sender,
            worker: Mutex::new(worker),
        }
    }

    fn send_line(&self, line: String) {
        if self.sender.try_send(LogCommand::Line(line)).is_err() {
            DROPPED_LINES.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {
        let (ack, acked) = mpsc::sync_channel(1);
        if send_within(&self.sender, LogCommand::Flush(ack), LOG_DRAIN_TIMEOUT) {
            let _ = acked.recv_timeout(LOG_DRAIN_TIMEOUT);
        }
    }

    /// Drains and joins the writer if `Stop` can be sent before the deadline.
    /// Retains the handle when sending fails so a later shutdown can retry.
    fn shutdown(&self) {
        let mut slot = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(worker) = slot.take() else {
            return;
        };

        if !send_within(&self.sender, LogCommand::Stop, LOG_DRAIN_TIMEOUT) {
            *slot = Some(worker);
            return;
        }
        drop(slot);

        crate::util::threads::join_all_within(vec![worker], LOG_DRAIN_TIMEOUT, "logging");
    }
}

/// Retry full queues until the deadline; a blocking send could hang shutdown.
fn send_within(sender: &SyncSender<LogCommand>, command: LogCommand, timeout: Duration) -> bool {
    const RETRY_INTERVAL: Duration = Duration::from_millis(1);

    let deadline = Instant::now() + timeout;
    let mut command = command;
    loop {
        match sender.try_send(command) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return false;
                }
                command = returned;
                std::thread::sleep(RETRY_INTERVAL);
            }
        }
    }
}

fn writer_loop(receiver: &mpsc::Receiver<LogCommand>) {
    let mut file = None;
    let mut pending = VecDeque::new();

    while let Ok(command) = receiver.recv() {
        if !apply(command, &mut file, &mut pending) {
            break;
        }
    }

    // Lines that raced the stop signal onto the queue still belong on disk.
    while let Ok(command) = receiver.try_recv() {
        if !apply(command, &mut file, &mut pending) {
            break;
        }
    }
    settle(&mut file, &mut pending);
}

fn apply(command: LogCommand, file: &mut Option<File>, pending: &mut VecDeque<String>) -> bool {
    match command {
        LogCommand::Line(line) => {
            if open_sink(file, pending) {
                write_log_line(file, &line);
            } else {
                hold(pending, line, &DROPPED_LINES);
            }
            true
        }
        LogCommand::Flush(ack) => {
            settle(file, pending);
            let _ = ack.send(());
            true
        }
        LogCommand::Stop => false,
    }
}

fn open_sink(file: &mut Option<File>, pending: &mut VecDeque<String>) -> bool {
    if !FILE_SINK_READY.load(Ordering::Acquire) {
        return false;
    }
    if file.is_none() {
        *file = open_log_file();
    }
    if file.is_none() {
        return false;
    }

    for held in pending.drain(..) {
        write_log_line(file, &held);
    }
    report_dropped_lines(file);
    file.is_some()
}

fn hold(pending: &mut VecDeque<String>, line: String, dropped_lines: &AtomicU64) {
    if pending.len() >= MAX_PENDING_LINES {
        let _ = pending.pop_front();
        dropped_lines.fetch_add(1, Ordering::Relaxed);
    }
    pending.push_back(line);
}

fn report_dropped_lines(file: &mut Option<File>) {
    let dropped = DROPPED_LINES.swap(0, Ordering::Relaxed);
    if dropped > 0 {
        write_log_line(
            file,
            &format!("[WARN] [{LOCAL_TARGET_PREFIX}] {dropped} log line(s) dropped"),
        );
    }
}

fn settle(file: &mut Option<File>, pending: &mut VecDeque<String>) {
    if open_sink(file, pending)
        && let Some(open_file) = file.as_mut()
    {
        let _ = open_file.flush();
        let _ = open_file.sync_data();
    }
}

fn write_log_line(file: &mut Option<File>, log: &str) {
    if let Some(open_file) = file.as_mut()
        && writeln!(open_file, "{log}").is_err()
    {
        *file = None;
    }
}

pub fn shutdown() {
    if let Some(sink) = LOG_SINK.get() {
        sink.shutdown();
    }
}

pub(crate) fn enable_file_sink(logs_dir: PathBuf) {
    let _ = LOGS_DIR.set(logs_dir);
    FILE_SINK_READY.store(true, Ordering::Release);
}

pub(crate) fn install() -> Result<(), log::SetLoggerError> {
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

    // Before `set_logger`, so the first record already has somewhere to go.
    let _ = LOG_SINK.get_or_init(LogSink::start);
    log::set_logger(&LOGGER)?;
    let config = configured_level_config();
    let _ = LEVEL_CONFIG.set(config);
    log::set_max_level(config.global);
    let _ = INSTALLED.set(());
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    let logs_dir = LOGS_DIR.get()?;
    std::fs::create_dir_all(logs_dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join(LOG_FILE_NAME))
        .ok()
}

/// Write panic logs synchronously for panic = abort. Never resolve appdata from the hook.
pub fn log_panic(panic: &PanicHookInfo<'_>, backtrace: &Backtrace) {
    if !FILE_SINK_READY.load(Ordering::Acquire) {
        return;
    }
    let Some(mut file) = open_log_file() else {
        return;
    };

    let _ = writeln!(
        file,
        "\n\n!!!!!!!!!!!!! PANIC !!!!!!!!!!!!!\n{panic}\n{backtrace}\n"
    );
    let _ = file.sync_data();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests own the process-wide log sink; test backends disable file logging.
    fn redirect_file_sink_to(dir: &std::path::Path) {
        assert!(
            LOGS_DIR.set(dir.to_path_buf()).is_ok(),
            "the logs directory must not already be resolved in this process"
        );
        FILE_SINK_READY.store(true, Ordering::Release);
    }

    #[test]
    fn flush_lands_queued_lines_on_disk_before_it_returns() {
        let dir = tempfile::tempdir().expect("temp dir");
        redirect_file_sink_to(dir.path());

        let sink = LogSink::start();
        for index in 0..64 {
            sink.send_line(format!("barrier-probe-{index}"));
        }
        sink.flush();

        let written = std::fs::read_to_string(dir.path().join(LOG_FILE_NAME))
            .expect("the log file should exist once the sink resolved");
        for index in 0..64 {
            assert!(
                written.contains(&format!("barrier-probe-{index}")),
                "line {index} was still queued when flush returned"
            );
        }

        sink.shutdown();
        sink.shutdown();
    }

    #[test]
    fn holding_past_the_bound_drops_oldest_and_counts_the_loss() {
        let mut pending = VecDeque::new();
        let dropped_lines = AtomicU64::new(0);

        for index in 0..MAX_PENDING_LINES + 8 {
            hold(&mut pending, format!("line-{index}"), &dropped_lines);
        }

        assert_eq!(pending.len(), MAX_PENDING_LINES);
        assert_eq!(dropped_lines.load(Ordering::Relaxed), 8);
        assert_eq!(pending.front().map(String::as_str), Some("line-8"));
        assert_eq!(
            pending.back().map(String::as_str),
            Some(format!("line-{}", MAX_PENDING_LINES + 7).as_str())
        );
    }

    #[test]
    fn sending_to_a_departed_writer_fails_rather_than_blocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);

        assert!(!send_within(
            &sender,
            LogCommand::Stop,
            Duration::from_millis(50)
        ));
    }

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
