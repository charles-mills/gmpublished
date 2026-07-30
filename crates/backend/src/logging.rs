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

/// Depth of the queue between [`log::Log::log`] and the writer thread.
///
/// Emitting never blocks on it. A caller can be the UI thread, and stalling a
/// frame to record a debug line is a worse outcome than losing the line, so a
/// burst past this depth is counted and reported into the log rather than
/// applying backpressure.
const LOG_QUEUE_CAPACITY: usize = 4096;

/// Lines held while the file sink is unresolved — the logs directory is not
/// known until appdata loads, and everything logged before then has nowhere to
/// go yet. Reaching this bound drops the *oldest* held lines: what explains a
/// startup failure is at the tail.
const MAX_PENDING_LINES: usize = 4096;

/// Bound on [`flush`](log::Log::flush) and on the shutdown join, so a wedged
/// writer cannot hold process exit.
const LOG_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

struct BackendLogger;

static LOGGER: BackendLogger = BackendLogger;
static LEVEL_CONFIG: OnceLock<LevelConfig> = OnceLock::new();
static FILE_SINK_READY: AtomicBool = AtomicBool::new(false);
// Process-wide by necessity: [`log_panic`] is reached from the process panic
// hook, which has no way to receive a `&Backend`. Set once, by whichever
// `Backend` finishes construction first (`enable_file_sink`).
static LOGS_DIR: OnceLock<PathBuf> = OnceLock::new();
static LOG_SINK: OnceLock<LogSink> = OnceLock::new();
/// Lines the queue or the pending bound refused, reported into the log so a
/// burst that outran the writer reads as a stated gap and not a silent one.
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
    /// Write everything queued ahead of this, then acknowledge.
    ///
    /// The ack is what lets `flush` wait for its own line to land rather than
    /// merely enqueue it. It is a barrier only while the writer keeps up:
    /// both the send and the wait are bounded, so a writer that has stalled
    /// leaves `flush` returning with lines still queued. That is the deliberate
    /// trade — a caller on the way out must not be held by a stuck writer.
    Flush(SyncSender<()>),
    Stop,
}

/// Owns the writer thread and the queue feeding it, so the thread is joinable
/// rather than merely detached and the queue has an end the caller can wait on.
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

    /// Drains what is queued and joins the writer, if the writer can be told
    /// to stop.
    ///
    /// The handle is put back when the stop signal does not land, so a later
    /// call can try again. Taking it unconditionally would make the one case
    /// this deadline exists for — a writer so far behind that the queue stays
    /// full — permanently unrecoverable: the sender is a process-global that
    /// is never dropped, so a writer that never sees `Stop` parks in `recv`
    /// for the life of the process with nothing left able to wake it.
    fn shutdown(&self) {
        let mut slot = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(worker) = slot.take() else {
            return;
        };

        if !send_within(&self.sender, LogCommand::Stop, LOG_DRAIN_TIMEOUT) {
            // The writer is either gone already (nothing to join) or too far
            // behind to accept the signal (joining would just burn the
            // deadline). Either way, hand the handle back.
            *slot = Some(worker);
            return;
        }
        drop(slot);

        crate::util::threads::join_all_within(vec![worker], LOG_DRAIN_TIMEOUT, "logging");
    }
}

/// Sends within `timeout`, reporting whether it landed.
///
/// `SyncSender::send_timeout` is unstable, and a plain `send` would block for
/// as long as a wedged writer takes to drain — which is the thing every caller
/// here is trying to avoid. A full queue means the writer is behind but alive,
/// so retrying is worth more than failing immediately.
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

/// Returns whether the writer should keep running.
fn apply(command: LogCommand, file: &mut Option<File>, pending: &mut VecDeque<String>) -> bool {
    match command {
        LogCommand::Line(line) => {
            if open_sink(file, pending) {
                write_log_line(file, &line);
            } else {
                hold(pending, line);
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

/// Opens the log file and writes out everything held for it, once the logs
/// directory is known. Returns whether the file is available to write to.
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

/// Holds a line until the file sink resolves, dropping the oldest once the
/// bound is reached.
fn hold(pending: &mut VecDeque<String>, line: String) {
    if pending.len() >= MAX_PENDING_LINES {
        let _ = pending.pop_front();
        DROPPED_LINES.fetch_add(1, Ordering::Relaxed);
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

/// Gets everything queued onto disk and durable. The `sync_data` is what makes
/// this worth calling before process exit rather than trusting the write.
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

/// Drains and closes the file sink. Safe when logging was never installed, and
/// safe to call more than once.
pub fn shutdown() {
    if let Some(sink) = LOG_SINK.get() {
        sink.shutdown();
    }
}

pub(crate) fn enable_file_sink(logs_dir: PathBuf) {
    let _ = LOGS_DIR.set(logs_dir);
    FILE_SINK_READY.store(true, Ordering::Release);
}

/// Idempotent: `log::set_logger` is a one-shot process resource, so a second
/// `Backend` built in the same process (tests, or a hypothetical re-init)
/// reuses the first one's install rather than erroring.
///
/// Deliberately does not install a panic hook. The process hook is a single
/// global slot with no way to chain onto an existing owner, so it belongs to
/// the executable; this module only offers [`log_panic`] for that owner to
/// call. See `gmpublished::install_panic_log_hook`.
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

/// Records a panic in the backend log file, for the process hook owner to call.
///
/// Writes nothing anywhere else: the owner captured `backtrace` and is
/// responsible for stderr, so echoing here would double every panic report.
/// A no-op until the file sink is up — the logs directory is not known until
/// appdata loads, and resolving it during an appdata-init panic would re-enter
/// the very state that failed.
///
/// The write is synchronous. Under `panic = "abort"` the process dies before
/// the async writer thread would drain a channel send.
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

    /// Nothing else in this crate's test binary resolves the file sink
    /// (`BackendConfig::for_test` sets `file_logging: false`), so these two own
    /// the process-global logs directory between them.
    fn redirect_file_sink_to(dir: &std::path::Path) {
        assert!(
            LOGS_DIR.set(dir.to_path_buf()).is_ok(),
            "the logs directory must not already be resolved in this process"
        );
        FILE_SINK_READY.store(true, Ordering::Release);
    }

    /// `flush` is a barrier: once it returns, everything sent before it is on
    /// disk. Without the ack it is a hint, and a process that exits promptly
    /// after logging loses the tail — which is the case that matters, because
    /// that tail is what explains the exit.
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
        // Idempotent: the handle is taken, so this has nothing left to join.
        sink.shutdown();
    }

    /// The pending bound drops the *oldest* held lines and counts them, rather
    /// than growing without limit while the logs directory is unresolved.
    /// `DROPPED_LINES` is process-global and a live writer swaps it on every
    /// line, so this asserts on the delta it observes rather than on the
    /// counter's absolute value — otherwise a concurrent test's writer steals
    /// the count and this fails for reasons unrelated to the bound.
    #[test]
    fn holding_past_the_bound_drops_oldest_and_counts_the_loss() {
        let mut pending = VecDeque::new();
        let before = DROPPED_LINES.load(Ordering::Relaxed);

        for index in 0..MAX_PENDING_LINES + 8 {
            hold(&mut pending, format!("line-{index}"));
        }

        assert_eq!(pending.len(), MAX_PENDING_LINES);
        assert!(DROPPED_LINES.load(Ordering::Relaxed) >= before + 8);
        assert_eq!(pending.front().map(String::as_str), Some("line-8"));
        assert_eq!(
            pending.back().map(String::as_str),
            Some(format!("line-{}", MAX_PENDING_LINES + 7).as_str())
        );
    }

    /// A send to a writer that is gone reports failure instead of blocking, so
    /// shutdown after the thread has already exited still returns.
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
