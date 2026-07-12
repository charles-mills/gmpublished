// The macOS document-open bridge (platform_open) and live titlebar helper
// (platform_chrome) need unsafe FFI; everything else stays unsafe-free.
// Non-macOS builds keep the full forbid.
#![cfg_attr(not(target_os = "macos"), forbid(unsafe_code))]
#![cfg_attr(target_os = "macos", deny(unsafe_code))]

use std::{
    backtrace::Backtrace,
    fmt,
    fs::OpenOptions,
    io::Write,
    panic::{self, PanicHookInfo},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::OnceLock,
};

use app::App;

mod app;
mod assets;
mod backend;
mod features;
mod format;
mod i18n;
mod media;
mod net;
#[cfg(target_os = "macos")]
mod platform_chrome;
#[cfg(target_os = "macos")]
mod platform_menu;
#[cfg(target_os = "macos")]
mod platform_open;
#[cfg(test)]
mod test_support;
pub mod theme;
mod util;
mod widgets;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const PANIC_LOG_FILE_NAME: &str = "gmpublished-panic.log";
const MIN_WINDOW_WIDTH: f32 = 800.0;
const MIN_WINDOW_HEIGHT: f32 = 600.0;

static PANIC_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            eprintln!("Panic log: {}", panic_log_path().display());
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
enum RunError {
    BackendInit(gmpublished_backend::BackendInitError),
    Iced(iced::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendInit(error) => write!(f, "backend initialization failed: {error}"),
            Self::Iced(error) => write!(f, "{error}"),
        }
    }
}

impl From<iced::Error> for RunError {
    fn from(error: iced::Error) -> Self {
        Self::Iced(error)
    }
}

impl From<gmpublished_backend::BackendInitError> for RunError {
    fn from(error: gmpublished_backend::BackendInitError) -> Self {
        Self::BackendInit(error)
    }
}

fn run() -> Result<(), RunError> {
    if gmpublished_backend::cli::stdin() {
        return Ok(());
    }

    // A quiet, throwaway `AppData` read: just enough to resolve the window
    // chrome strategy and the panic log path before the Iced event loop
    // (and the real `Backend`, with its Steam/whitelist background
    // threads) exists. `App::new` builds the one real `Backend` when the
    // event loop actually starts.
    let early_app_data = gmpublished_backend::appdata::AppData::load(
        gmpublished_backend::appdata::AppDataPaths::production(),
        gmpublished_backend::transactions::Transactions::new(
            std::sync::Arc::new(gmpublished_backend::events::NullEventSink),
            false,
        ),
    );
    install_panic_log_hook(
        early_app_data
            .temp_dir()
            .join("logs")
            .join(PANIC_LOG_FILE_NAME),
    );

    // Must run before the Iced event loop starts; see `platform_open::install`.
    #[cfg(target_os = "macos")]
    platform_open::install();

    let chrome_strategy =
        features::shell::ChromeStrategy::resolve(early_app_data.settings.load().titlebar);

    let ctx = backend::tasks::BackendContext::new()?;
    let application = iced::application(move || App::new(ctx.clone()), App::update, App::view);
    let application = assets::fonts::bundled_fonts()
        .into_iter()
        .fold(application, iced::Application::font);

    application
        .window(window_settings(chrome_strategy))
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

#[cfg(not(target_os = "macos"))]
fn apply_platform_chrome(
    _settings: &mut iced::window::Settings,
    _chrome_strategy: features::shell::ChromeStrategy,
) {
}

fn install_panic_log_hook(path: PathBuf) {
    let _ = PANIC_LOG_PATH.set(path.clone());
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |panic| {
            append_panic_log(&path, panic);
            previous(panic);
        }));
    });
}

fn panic_log_path() -> PathBuf {
    if let Some(path) = PANIC_LOG_PATH.get() {
        return path.clone();
    }

    panic::catch_unwind(resolved_panic_log_path).unwrap_or_else(|_| fallback_panic_log_path())
}

fn resolved_panic_log_path() -> PathBuf {
    let app_data = gmpublished_backend::appdata::AppData::load(
        gmpublished_backend::appdata::AppDataPaths::production(),
        gmpublished_backend::transactions::Transactions::new(
            std::sync::Arc::new(gmpublished_backend::events::NullEventSink),
            false,
        ),
    );
    app_data.temp_dir().join("logs").join(PANIC_LOG_FILE_NAME)
}

fn fallback_panic_log_path() -> PathBuf {
    std::env::temp_dir()
        .join("gmpublisher")
        .join("logs")
        .join(PANIC_LOG_FILE_NAME)
}

fn append_panic_log(path: &Path, panic: &PanicHookInfo<'_>) {
    if append_panic_log_to(path, panic).is_ok() {
        return;
    }

    let fallback = fallback_panic_log_path();
    if fallback != path {
        let _ = append_panic_log_to(&fallback, panic);
    }
}

fn append_panic_log_to(path: &Path, panic: &PanicHookInfo<'_>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let location = panic_location(panic);
    let message = panic_message(panic);
    let backtrace = Backtrace::force_capture();

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
    panic.payload().downcast_ref::<&str>().map_or_else(
        || {
            panic
                .payload()
                .downcast_ref::<String>()
                .map_or_else(|| "non-string panic payload".to_owned(), String::clone)
        },
        |message| (*message).to_owned(),
    )
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
}
