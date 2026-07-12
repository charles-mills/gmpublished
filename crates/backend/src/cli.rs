use std::path::PathBuf;

use clap::{Arg, ArgGroup, ArgMatches, Command};

use crate::{
    Backend, BackendConfig, GMAFile,
    gma::{ExtractDestination, ExtractOptions, Whitelist},
};

/// Whether the process was invoked with CLI-style arguments (a bare
/// `gmpublished <file.gma>` from a file association, or `-e`/`--extract`).
/// Recomputed on demand rather than cached: it's a cheap `argv` length check,
/// not process state worth memoizing behind a global.
#[must_use]
pub fn is_cli_mode() -> bool {
    std::env::args_os().len() > 1
}

pub fn stdin() -> bool {
    if !is_cli_mode() {
        return false;
    }

    // CLI mode prints product output directly; remove any host-installed backend panic hook.
    let _ = std::panic::take_hook();

    let matches = command().get_matches();

    #[cfg(debug_assertions)]
    std::eprintln!("{matches:#?}");

    if let Some(request) = extraction_request(&matches) {
        run_extraction(request);
    }

    true
}

fn command() -> Command {
    Command::new("gmpublisher")
        .version(env!("CARGO_PKG_VERSION"))
        .author("William Venner <william@venner.io>")
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

#[derive(Debug, PartialEq, Eq)]
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

fn run_extraction(request: ExtractionRequest) {
    if !request.path.is_file() {
        std::eprintln!("Invalid GMA file path provided.");
        return;
    }

    // No background threads: extraction never used Steam, and the remote
    // whitelist fetch happens synchronously below (matching the blocking
    // fetch the whitelist's first use always performed) with the built-in
    // list as the failure fallback.
    let backend = Backend::init(BackendConfig {
        cli_mode: true,
        background_threads: false,
        ..BackendConfig::default()
    });
    let backend = match backend {
        Ok(backend) => backend,
        Err(error) => {
            std::eprintln!("Warning: backend initialization failed ({error}); extracting anyway.");
            return;
        }
    };
    backend.whitelist.refresh_from_remote();

    match GMAFile::open(request.path) {
        Ok(gma) => {
            let extract_result = gma.view().and_then(|view| {
                view.extract(
                    &gma,
                    request.destination,
                    &backend.transactions.begin(),
                    ExtractOptions {
                        open_after: true,
                        whitelist: Whitelist::Ignore,
                    },
                    &backend.whitelist,
                    &backend.app_data,
                    &backend.steam,
                )
            });
            if let Err(err) = extract_result {
                std::eprintln!("Error: {err:#?}");
            }
        }
        Err(err) => {
            std::eprintln!("Error: {err:#?}");
        }
    }
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
