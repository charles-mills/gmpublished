//! Binary-owned command-line parsing and headless operation dispatch.
//!
//! Process arguments are application policy, so this module translates them
//! into the same backend operations used by the GUI. Blocking work is sent to
//! the backend's explicitly configured blocking executor; backend code never
//! needs to inspect `argv` or weaken its main-thread assertion for this mode.

use std::{path::PathBuf, sync::mpsc};

use clap::{Arg, ArgGroup, ArgMatches, Command};
use gmpublished_backend::{
    Backend, BackendConfig, BackgroundServices, ExtractDestination, ExtractOptions, GmaFile,
    Whitelist,
};

/// What a CLI invocation asked for, and how it ended.
///
/// `None` means no CLI-style arguments were supplied and the GUI should
/// start. A failed request remains a process failure for scripts and file
/// association handlers.
pub fn run() -> Option<Result<(), CliError>> {
    if std::env::args_os().len() <= 1 {
        return None;
    }

    let matches = command().get_matches();
    Some(extraction_request(&matches).map_or(Ok(()), run_extraction))
}

/// Why a CLI invocation failed, as the shell should see it.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{} is not a file", .0.display())]
    NotAFile(PathBuf),
    #[error("backend initialization failed: {0}")]
    BackendInit(#[from] gmpublished_backend::BackendInitError),
    #[error("could not schedule CLI extraction: {0}")]
    CouldNotRun(#[from] gmpublished_backend::ExecutionScheduleError),
    #[error("the CLI extraction worker stopped without returning a result")]
    WorkerStopped,
    #[error(transparent)]
    Gma(#[from] gmpublished_backend::GmaError),
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
            // Bare `gmpublished <FILE>` behaves exactly like `-e <FILE>`.
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

    // No background Steam runtime and no event consumer. The execution
    // resources remain enabled because they are the explicit policy boundary
    // that keeps blocking archive and whitelist work off the process thread.
    let backend = Backend::init(BackendConfig {
        background_services: BackgroundServices::Disabled,
        ..BackendConfig::default()
    })?;
    let execution = backend.execution_resources();
    let (result_tx, result_rx) = mpsc::sync_channel(1);

    execution.spawn_blocking("cli-extract", move || {
        backend.refresh_whitelist();
        let result = GmaFile::open(request.path).and_then(|gma| {
            gma.view().and_then(|view| {
                let context = backend.resolve_extraction(
                    &gma,
                    request.destination,
                    ExtractOptions {
                        open_after: true,
                        whitelist: Whitelist::Ignore,
                    },
                )?;
                backend.extract_gma(&view, &gma, &backend.begin_transaction(), context)
            })
        });
        let _ = result_tx.send(result);
    })?;

    result_rx.recv().map_err(|_| CliError::WorkerStopped)??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(args: &[&str]) -> ArgMatches {
        command()
            .try_get_matches_from(args)
            .expect("arguments should parse")
    }

    #[test]
    fn a_missing_file_is_an_error_rather_than_a_silent_success() {
        let request = ExtractionRequest {
            path: PathBuf::from("/nonexistent/definitely-not-here.gma"),
            destination: ExtractDestination::Temp,
        };
        assert!(matches!(
            run_extraction(request),
            Err(CliError::NotAFile(_))
        ));
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
    fn invalid_argument_combinations_are_rejected() {
        assert!(
            command()
                .try_get_matches_from(["gmpublished", "-o", "/tmp/out"])
                .is_err()
        );
        assert!(
            command()
                .try_get_matches_from(["gmpublished", "-e", "/tmp/a.gma", "/tmp/b.gma",])
                .is_err()
        );
    }
}
