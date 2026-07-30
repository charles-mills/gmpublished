use std::path::PathBuf;

use clap::{Arg, ArgGroup, ArgMatches, Command};

use crate::{
    Backend, BackendConfig, GmaFile,
    gma::{ExtractDestination, ExtractOptions, Whitelist},
};

/// Whether the process was invoked with CLI-style arguments (a bare
/// `gmpublished <file.gma>` from a file association, or `-e`/`--extract`).
///
/// An `argv` probe, and only [`run`] should need it. Anything asking "is this
/// process headless?" wants [`is_headless`], which is what `run` decided
/// rather than what the arguments hint at.
///
/// This is where opening a `.gma` diverges by platform. macOS delivers
/// documents by Apple Event, leaving `argv` empty, so the GUI starts and
/// previews; every other shell passes the path in `argv`, so the process
/// extracts headlessly and exits.
#[must_use]
pub fn is_cli_mode() -> bool {
    std::env::args_os().len() > 1
}

/// Whether this process took the headless path.
///
/// Latched by [`run`] rather than re-derived from `argv`, because
/// `main_thread_forbidden!` consults it: deriving it means any argument at all
/// — `cargo run -- --some-flag`, a debugger's, a profiler's — silently disarms
/// that assertion for a GUI session it was never about.
#[must_use]
pub fn is_headless() -> bool {
    HEADLESS.load(std::sync::atomic::Ordering::Acquire)
}

static HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a CLI invocation asked for, and how it ended.
///
/// `None` means the process was not invoked CLI-style and the GUI should
/// start. The `Result` is the process's exit status: a failed extraction has
/// to reach the shell as a failure, or scripts and file-association handlers
/// read every error as success.
pub fn run() -> Option<Result<(), CliError>> {
    if !is_cli_mode() {
        return None;
    }
    HEADLESS.store(true, std::sync::atomic::Ordering::Release);

    let matches = command().get_matches();

    // Clap handles `--help`/`--version` by exiting itself, so reaching here
    // with no request means the arguments named no work to do.
    Some(extraction_request(&matches).map_or(Ok(()), run_extraction))
}

/// Why a CLI invocation failed, as the shell should see it.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{} is not a file", .0.display())]
    NotAFile(PathBuf),
    #[error("backend initialization failed: {0}")]
    BackendInit(#[from] crate::BackendInitError),
    #[error(transparent)]
    Gma(#[from] crate::gma::GmaError),
}

fn command() -> Command {
    Command::new("gmpublished")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Publish, extract and work with GMA files")
        .args(&[
            Arg::new("extract")
                .short('e')
                .long("extract")
                .value_name("FILE")
                .help("Extracts a .GMA file"),
            // Bare `gmpublished <FILE>` behaves exactly like `-e <FILE>`. This
            // is what freedesktop `Exec={{exec}} %f` desktop entries invoke;
            // Windows file associations keep using `-e "%1"`.
            Arg::new("file")
                .value_name("FILE")
                .help("Extracts a .GMA file (same as --extract)"),
            Arg::new("out")
                .short('o')
                .long("out")
                .value_name("PATH")
                .help("Sets the output path for extracting GMAs. Defaults to the temp directory.")
                .requires("extract_input"),
        ])
        // `-e FILE` and a bare FILE are two spellings of the same input; the
        // group makes them mutually exclusive and gives `--out` one anchor.
        .group(ArgGroup::new("extract_input").args(["extract", "file"]))
}

#[derive(Debug, Eq, PartialEq)]
struct ExtractionRequest {
    path: PathBuf,
    destination: ExtractDestination,
}

fn extraction_request(matches: &ArgMatches) -> Option<ExtractionRequest> {
    let extract_path = matches
        .get_one::<String>("extract")
        .or_else(|| matches.get_one::<String>("file"))?;

    let destination = matches
        .get_one::<String>("out")
        .map_or(ExtractDestination::Temp, |out| {
            ExtractDestination::Directory(PathBuf::from(out))
        });

    Some(ExtractionRequest {
        path: PathBuf::from(extract_path),
        destination,
    })
}

fn run_extraction(request: ExtractionRequest) -> Result<(), CliError> {
    if !request.path.is_file() {
        return Err(CliError::NotAFile(request.path));
    }

    // No background threads: extraction never used Steam, and the remote
    // whitelist fetch happens synchronously below (matching the blocking
    // fetch the whitelist's first use always performed) with the built-in
    // list as the failure fallback.
    // The default `NullEventSink` is what makes this headless: nothing is
    // listening for transaction progress, so nothing is emitted to.
    let backend = Backend::init(BackendConfig {
        background_services: crate::BackgroundServices::Disabled,
        ..BackendConfig::default()
    });
    // Extraction needs the whitelist and the transaction sink, so a failed
    // init is fatal here rather than something to continue past.
    let backend = backend?;
    backend.whitelist.refresh_from_remote();

    let gma = GmaFile::open(request.path)?;
    gma.view().and_then(|view| {
        let context = backend.resolve_extraction(
            &gma,
            request.destination,
            ExtractOptions {
                open_after: true,
                whitelist: Whitelist::Ignore,
            },
        )?;
        view.extract(&gma, &backend.transactions.begin(), context)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// A file association or a script reads the exit status, so an extraction
    /// that could not happen must not report success. Printing a message and
    /// returning is not enough — the shell only sees the code.
    #[test]
    fn a_missing_file_is_an_error_rather_than_a_silent_success() {
        let request = ExtractionRequest {
            path: std::path::PathBuf::from("/nonexistent/definitely-not-here.gma"),
            destination: ExtractDestination::Temp,
        };

        assert!(matches!(
            super::run_extraction(request),
            Err(CliError::NotAFile(_))
        ));
    }

    use super::*;

    fn matches(args: &[&str]) -> ArgMatches {
        command()
            .try_get_matches_from(args)
            .expect("arguments should parse")
    }

    #[test]
    fn no_arguments_requests_no_extraction() {
        assert_eq!(extraction_request(&matches(&["gmpublished"])), None);
    }

    #[test]
    fn extract_flag_requests_temp_extraction() {
        assert_eq!(
            extraction_request(&matches(&["gmpublished", "-e", "/tmp/addon.gma"])),
            Some(ExtractionRequest {
                path: PathBuf::from("/tmp/addon.gma"),
                destination: ExtractDestination::Temp,
            })
        );
    }

    #[test]
    fn bare_positional_file_matches_extract_flag() {
        assert_eq!(
            extraction_request(&matches(&["gmpublished", "/tmp/addon.gma"])),
            extraction_request(&matches(&["gmpublished", "-e", "/tmp/addon.gma"])),
        );
    }

    #[test]
    fn out_flag_applies_to_both_extract_spellings() {
        for args in [
            &["gmpublished", "-e", "/tmp/addon.gma", "-o", "/tmp/out"][..],
            &["gmpublished", "/tmp/addon.gma", "-o", "/tmp/out"][..],
        ] {
            assert_eq!(
                extraction_request(&matches(args)),
                Some(ExtractionRequest {
                    path: PathBuf::from("/tmp/addon.gma"),
                    destination: ExtractDestination::Directory(PathBuf::from("/tmp/out")),
                })
            );
        }
    }

    #[test]
    fn out_flag_without_extract_input_is_rejected() {
        assert!(
            command()
                .try_get_matches_from(["gmpublished", "-o", "/tmp/out"])
                .is_err()
        );
    }

    #[test]
    fn extract_flag_conflicts_with_positional_file() {
        assert!(
            command()
                .try_get_matches_from(["gmpublished", "-e", "/tmp/a.gma", "/tmp/b.gma"])
                .is_err()
        );
    }
}
