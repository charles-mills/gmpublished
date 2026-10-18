// The macOS document-open bridge (platform_open) and live titlebar helper
// (platform_chrome) need unsafe FFI; everything else stays unsafe-free.
// Non-macOS builds keep the full forbid.
#![cfg_attr(not(target_os = "macos"), forbid(unsafe_code))]
#![cfg_attr(target_os = "macos", deny(unsafe_code))]
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    backtrace::Backtrace,
    fs::OpenOptions,
    io::Write,
    panic::{self, PanicHookInfo},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::OnceLock,
    thread,
};

use app::App;

macro_rules! enum_with_all {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            pub(crate) const ALL: [Self; [$(stringify!($variant)),+].len()] = [
                $(Self::$variant),+
            ];
        }
    };
}

macro_rules! mapped_enum_with_all {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $value:expr),+ $(,)?
        }
        $method:ident -> $value_type:ty
    ) => {
        enum_with_all! {
            $(#[$enum_meta])*
            $vis enum $name {
                $($(#[$variant_meta])* $variant),+
            }
        }

        impl $name {
            pub(crate) const fn $method(self) -> $value_type {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }
    };
}

mod app;
mod assets;
mod bridge;
mod cli;
mod features;
mod format;
mod generation;
mod i18n;
mod media;
mod platform;
#[cfg(target_os = "macos")]
mod platform_chrome;
#[cfg(target_os = "macos")]
mod platform_menu;
#[cfg(target_os = "macos")]
mod platform_open;
mod preparation;
mod spinner_clock;
#[cfg(test)]
mod test_support;
mod theme;
mod treemap;
mod util;
mod widgets;

pub(crate) const APP_NAME: &str = "gmpublished";

const PANIC_LOG_FILE_NAME: &str = "gmpublished-panic.log";
const MIN_WINDOW_WIDTH: f32 = 800.0;
const MIN_WINDOW_HEIGHT: f32 = 600.0;

static PANIC_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

fn main() -> ExitCode {
    let shutdown = GpuShutdownGuard(true);
    let outcome = run();
    let gpu_shutdown: Result<(), features::file_preview::ShutdownError> = shutdown.finish();
    gmpublished_backend::shutdown_logging();
    if let Err(error) = gpu_shutdown {
        eprintln!("GPU completion shutdown failed: {error}");
        if let Err(error) = outcome {
            eprintln!("Error: {error}");
        }
        return ExitCode::FAILURE;
    }

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            eprintln!("Panic log: {}", panic_log_path().display());
            ExitCode::FAILURE
        }
    }
}

struct GpuShutdownGuard(bool);
impl GpuShutdownGuard {
    fn finish(mut self) -> Result<(), features::file_preview::ShutdownError> {
        let result = features::file_preview::finish_gpu_shutdown();
        self.0 = false;
        result
    }
}
impl Drop for GpuShutdownGuard {
    fn drop(&mut self) {
        if self.0
            && let Err(error) = features::file_preview::finish_gpu_shutdown()
        {
            eprintln!("GPU completion shutdown failed during unwinding: {error}");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("backend initialization failed: {0}")]
    BackendInit(#[from] gmpublished_backend::BackendInitError),
    #[error(transparent)]
    Iced(#[from] iced::Error),
    #[error(transparent)]
    Cli(#[from] cli::CliError),
}

fn run() -> Result<(), RunError> {
    // Install before initialization so startup panics are logged too.
    install_panic_log_hook();

    if let Some(outcome) = cli::run() {
        return Ok(outcome?);
    }

    let ctx = bridge::tasks::BackendContext::new()?;
    let (startup_settings, startup_paths) = ctx.settings_and_paths_snapshot();
    let _ = PANIC_LOG_PATH.set(
        startup_paths
            .temp_dir
            .join("logs")
            .join(PANIC_LOG_FILE_NAME),
    );

    // Must run before the Iced event loop starts; see `platform_open::install`.
    #[cfg(target_os = "macos")]
    platform_open::install();

    let chrome_strategy =
        features::shell::ChromeStrategy::resolve(startup_settings.backend.titlebar);

    let window_settings = window_settings(chrome_strategy);
    let application = iced::application(move || App::new(ctx.clone()), App::update, App::view);
    let application = assets::fonts::bundled_fonts()
        .into_iter()
        .fold(application, iced::Application::font);

    application
        .window(window_settings)
        .default_font(assets::fonts::default_font())
        .theme(App::theme)
        .subscription(App::subscription)
        .title(App::title)
        .run()?;

    Ok(())
}

fn window_settings(chrome_strategy: features::shell::ChromeStrategy) -> iced::window::Settings {
    let mut settings = iced::window::Settings {
        min_size: Some(iced::Size::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)),
        ..iced::window::Settings::default()
    };
    apply_platform_chrome(&mut settings, chrome_strategy);

    settings
}

#[cfg(target_os = "macos")]
fn apply_platform_chrome(
    settings: &mut iced::window::Settings,
    chrome_strategy: features::shell::ChromeStrategy,
) {
    let inset = chrome_strategy.mac_native_inset();
    settings.platform_specific = iced::window::settings::PlatformSpecific {
        title_hidden: inset,
        titlebar_transparent: inset,
        fullsize_content_view: inset,
    };
}

/// Sets the Wayland `app_id` and X11 `WM_CLASS`, which iced otherwise leaves
/// empty. Must stay equal to the `StartupWMClass` in `gmpublished.desktop`.
#[cfg(target_os = "linux")]
fn apply_platform_chrome(
    settings: &mut iced::window::Settings,
    _chrome_strategy: features::shell::ChromeStrategy,
) {
    settings.platform_specific = iced::window::settings::PlatformSpecific {
        application_id: APP_NAME.to_owned(),
        ..iced::window::settings::PlatformSpecific::default()
    };
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn apply_platform_chrome(
    _settings: &mut iced::window::Settings,
    _chrome_strategy: features::shell::ChromeStrategy,
) {
}

fn install_panic_log_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        panic::set_hook(Box::new(|panic| {
            let report = PanicReport::capture(panic);
            append_panic_log(&panic_log_path(), &report);
            gmpublished_backend::log_panic(panic, &report.backtrace);
            eprintln!(
                "\nthread '{}' {panic}\n{}",
                thread::current().name().unwrap_or("<unnamed>"),
                report.backtrace
            );
        }));
    });
}

struct PanicReport {
    location: String,
    message: String,
    backtrace: Backtrace,
}

impl PanicReport {
    fn capture(panic: &PanicHookInfo<'_>) -> Self {
        Self {
            location: panic_location(panic),
            message: panic_message(panic),
            backtrace: Backtrace::force_capture(),
        }
    }
}

/// Panic-report path shared by the hook and startup-failure message.
/// Avoids appdata access, which could re-enter a failed initialization.
fn panic_log_path() -> PathBuf {
    PANIC_LOG_PATH
        .get()
        .cloned()
        .unwrap_or_else(fallback_panic_log_path)
}

fn fallback_panic_log_path() -> PathBuf {
    // "gmpublisher", not APP_NAME: mirrors `AppDataPaths::default_temp_dir`.
    std::env::temp_dir()
        .join("gmpublisher")
        .join("logs")
        .join(PANIC_LOG_FILE_NAME)
}

/// Fall back to the temp log if the resolved panic-log path cannot be written.
fn append_panic_log(path: &Path, report: &PanicReport) {
    if append_panic_log_to(path, report).is_ok() {
        return;
    }

    let fallback = fallback_panic_log_path();
    if fallback != path {
        let _ = append_panic_log_to(&fallback, report);
    }
}

fn append_panic_log_to(path: &Path, report: &PanicReport) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let PanicReport {
        location,
        message,
        backtrace,
    } = report;

    writeln!(
        file,
        "\n\n!!!!!!!!!!!!! APP PANIC !!!!!!!!!!!!!\nmessage: {message}\nlocation: {location}\nbacktrace:\n{backtrace}"
    )?;
    file.sync_data()
}

fn panic_location(panic: &PanicHookInfo<'_>) -> String {
    panic.location().map_or_else(
        || "unknown".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    )
}

fn panic_message(panic: &PanicHookInfo<'_>) -> String {
    gmpublished_backend::panic_payload_message(panic.payload())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_settings_enforce_the_supported_minimum_without_changing_initial_size() {
        let defaults = iced::window::Settings::default();
        let settings = window_settings(features::shell::ChromeStrategy::SystemDefault);

        assert_eq!(settings.size, defaults.size);
        assert_eq!(
            settings.min_size,
            Some(iced::Size::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT))
        );
        assert!(settings.max_size.is_none());
    }

    fn report(message: &str) -> PanicReport {
        PanicReport {
            location: "src/somewhere.rs:12:34".to_owned(),
            message: message.to_owned(),
            backtrace: Backtrace::force_capture(),
        }
    }

    #[test]
    fn a_panic_report_reaches_the_log_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("logs").join(PANIC_LOG_FILE_NAME);

        append_panic_log(&path, &report("the panic message"));

        let contents = std::fs::read_to_string(&path).expect("panic log should exist");
        assert!(contents.contains("the panic message"), "{contents}");
        assert!(contents.contains("src/somewhere.rs:12:34"), "{contents}");
    }

    #[test]
    fn a_second_panic_report_appends_rather_than_replacing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(PANIC_LOG_FILE_NAME);

        append_panic_log(&path, &report("first"));
        append_panic_log(&path, &report("second"));

        let contents = std::fs::read_to_string(&path).expect("panic log should exist");
        assert_eq!(contents.matches("APP PANIC").count(), 2, "{contents}");
        assert!(contents.contains("first"), "{contents}");
        assert!(contents.contains("second"), "{contents}");
    }

    #[test]
    fn the_reported_path_is_the_fallback_until_the_backend_resolves_one() {
        assert!(
            PANIC_LOG_PATH.get().is_none(),
            "nothing in the test binary resolves the panic log path"
        );
        assert_eq!(panic_log_path(), fallback_panic_log_path());
    }
}
