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

/// Declares a fieldless enum and derives its declaration-ordered catalog from
/// the same variant list. Rust does not expose enum iteration, so keeping an
/// `ALL` array beside an independently-written enum otherwise leaves an
/// omission that still compiles.
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

/// [`enum_with_all!`] plus one exhaustive value mapping, also sourced from
/// the declaration. This is deliberately local rather than a derive
/// dependency: these small UI catalogs are the only capability required.
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
mod features;
mod format;
mod generation;
mod i18n;
mod media;
mod net;
mod platform;
#[cfg(target_os = "macos")]
mod platform_chrome;
#[cfg(target_os = "macos")]
mod platform_menu;
#[cfg(target_os = "macos")]
mod platform_open;
mod spinner_clock;
#[cfg(test)]
mod test_support;
mod theme;
mod treemap;
mod util;
mod widgets;

/// The product name. Brand identity, not a translatable string, so it is a
/// constant rather than a Fluent message duplicated across twelve catalogs —
/// see `i18n::tests::translated_catalogs_do_not_leak_english_values`.
pub(crate) const APP_NAME: &str = "gmpublished";

const PANIC_LOG_FILE_NAME: &str = "gmpublished-panic.log";
const MIN_WINDOW_WIDTH: f32 = 800.0;
const MIN_WINDOW_HEIGHT: f32 = 600.0;

static PANIC_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

fn main() -> ExitCode {
    let outcome = run();
    // Both exit paths converge here — GUI and headless CLI alike — so this is
    // the one place that can guarantee queued file logs reach disk. Anything
    // logged past this point still reaches stderr.
    gmpublished_backend::shutdown_logging();

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            eprintln!("Panic log: {}", panic_log_path().display());
            ExitCode::FAILURE
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
    Cli(#[from] gmpublished_backend::cli::CliError),
}

fn run() -> Result<(), RunError> {
    // Before any other work, GUI or headless: this process has exactly one
    // panic-hook owner, and everything after this point can panic. Until
    // `PANIC_LOG_PATH` resolves below, reports land on the fallback path.
    install_panic_log_hook();

    if let Some(outcome) = gmpublished_backend::cli::run() {
        return Ok(outcome?);
    }

    // Construct the one authoritative backend first. Its resolved snapshot is
    // also sufficient for pre-window configuration, avoiding a throwaway
    // second settings-file read on every GUI launch. Background Steam and
    // whitelist work remains dormant until the first frame has been shown.
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

/// Claims the process panic hook for this executable.
///
/// The hook is a single global slot with no way to chain onto an existing
/// owner, so exactly one component may set it and that component is the
/// composition root. The backend deliberately installs none; it exposes
/// [`gmpublished_backend::log_panic`] for this hook to call instead.
///
/// Nothing is delegated to the previous hook: the report below is a superset
/// of what the default one prints, and calling both would emit every panic
/// twice.
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

/// One panic, in the form every destination needs it.
///
/// Capturing a backtrace is the expensive part of reporting a panic, and there
/// are three destinations; taking it once here is what stops that cost being
/// paid three times. Holding the owned strings rather than the
/// [`PanicHookInfo`] is also what lets the writers below be exercised without
/// a live panic.
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

/// Where a panic report is written, resolved identically by the hook and by
/// the startup-failure message in [`main`] — so the path printed to the user
/// is always the path that was actually written.
///
/// Deliberately cheap and infallible. Deriving the real temp directory means
/// loading appdata, which is both too much work for a hook and a re-entry
/// into whatever state an init panic just failed in.
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

/// Appends `report` to the panic log, falling back to the temp-directory copy
/// when the resolved path cannot be written — a panic report that reaches
/// neither file is the one case with nothing left to diagnose from.
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
        // Nested: the logs directory does not exist yet when a startup panic
        // is the first thing to write one.
        let path = temp.path().join("logs").join(PANIC_LOG_FILE_NAME);

        append_panic_log(&path, &report("the panic message"));

        let contents = std::fs::read_to_string(&path).expect("panic log should exist");
        assert!(contents.contains("the panic message"), "{contents}");
        assert!(contents.contains("src/somewhere.rs:12:34"), "{contents}");
    }

    /// A second panic must not erase the first: the earlier one is often the
    /// cause and the later one the consequence.
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

    /// The path `main` prints on a startup failure has to be the one the hook
    /// wrote, including before the backend has resolved the real temp dir.
    #[test]
    fn the_reported_path_is_the_fallback_until_the_backend_resolves_one() {
        assert!(
            PANIC_LOG_PATH.get().is_none(),
            "nothing in the test binary resolves the panic log path"
        );
        assert_eq!(panic_log_path(), fallback_panic_log_path());
    }

    /// `panic::set_hook` is a single global slot with no chaining, so a second
    /// component installing one silently disables the first. The backend used
    /// to do exactly that from `logging::install`, which ran after this
    /// binary's hook and left `gmpublished-panic.log` permanently unwritten.
    ///
    /// Asserted against the sources rather than at runtime: swapping the
    /// process hook inside a test races every other test in the binary.
    #[test]
    fn only_this_binary_installs_a_process_panic_hook() {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && let Ok(source) = std::fs::read_to_string(&path)
                {
                    for (number, line) in source.lines().enumerate() {
                        // The call as a statement, which is the only way it is
                        // ever written. Matching the bare name instead would
                        // match this very line.
                        let line = line.trim_start();
                        if line.starts_with("panic::set_hook(")
                            || line.starts_with("std::panic::set_hook(")
                        {
                            out.push(format!("{}:{}", path.display(), number + 1));
                        }
                    }
                }
            }
        }

        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the app crate sits inside the workspace");
        let mut installs = Vec::new();
        for crate_name in ["backend", "app"] {
            walk(&workspace.join(crate_name).join("src"), &mut installs);
        }
        assert!(
            workspace.join("backend/src").is_dir(),
            "the backend crate must be walked, not silently skipped"
        );

        assert_eq!(
            installs.len(),
            1,
            "exactly one component may own the process panic hook: {installs:#?}"
        );
        let owner = workspace.join("app").join("src").join("main.rs");
        let owner = owner.display().to_string();
        assert!(
            installs[0].starts_with(&owner),
            "the owner must be this binary's composition root, not {}",
            installs[0]
        );
    }
}
