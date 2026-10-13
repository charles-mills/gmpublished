use std::path::Path;

use crate::bridge::ui_error::UiError;
use crate::generation::Generation;

pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AboutLink {
    Repository,
    Upstream,
    OriginalAuthor,
    Maintainer,
    License,
    ThirdPartyNotices,
    Releases,
}

impl AboutLink {
    pub(super) const fn url(self) -> &'static str {
        match self {
            Self::Repository => env!("CARGO_PKG_REPOSITORY"),
            Self::Upstream => "https://github.com/WilliamVenner/gmpublisher",
            Self::OriginalAuthor => "https://github.com/WilliamVenner",
            Self::Maintainer => "https://github.com/charles-mills",
            Self::License => "https://github.com/charles-mills/gmpublished/blob/HEAD/LICENSE",
            Self::ThirdPartyNotices => {
                "https://github.com/charles-mills/gmpublished/blob/HEAD/THIRD-PARTY-NOTICES.md"
            }
            Self::Releases => "https://github.com/charles-mills/gmpublished/releases",
        }
    }
}

#[derive(Debug, Default)]
pub(super) enum DebugReport {
    #[default]
    Idle,
    Loading,
    Ready(String),
    Failed,
}

#[derive(Debug, Default)]
pub(super) struct AboutState {
    pub report: DebugReport,
    pub expanded: bool,
    pub copied: bool,
    pub generation: Generation,
}

impl AboutState {
    pub fn reset(&mut self) {
        self.generation.bump();
        self.report = DebugReport::Idle;
        self.expanded = false;
        self.copied = false;
    }

    pub fn load(&mut self) -> Generation {
        self.report = DebugReport::Loading;
        self.copied = false;
        self.generation.bump()
    }

    pub fn complete(&mut self, generation: Generation, result: Result<String, UiError>) {
        if generation == self.generation {
            self.report = result.map_or(DebugReport::Failed, DebugReport::Ready);
        }
    }
}

pub(super) const fn os_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        std::env::consts::OS
    }
}

pub(super) fn os_art() -> iced::widget::svg::Handle {
    #[cfg(target_os = "linux")]
    let bytes: &[u8] = include_bytes!("../../../ui/images/about/tux.svg");
    #[cfg(target_os = "windows")]
    let bytes: &[u8] = include_bytes!("../../../ui/images/about/windows.svg");
    #[cfg(target_os = "macos")]
    let bytes: &[u8] = include_bytes!("../../../ui/images/about/macos.svg");
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let bytes: &[u8] = include_bytes!("../../../ui/icons/globe.svg");
    iced::widget::svg::Handle::from_memory(bytes)
}

pub fn debug_report(
    info: &iced::system::Information,
    scale: f32,
    locale: &str,
    theme: &str,
    steam: &str,
) -> String {
    let executable = std::env::current_exe().ok();
    let installation = installation_format(
        executable.as_deref(),
        std::env::var_os("APPIMAGE").is_some(),
        std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists(),
        std::env::var_os("SNAP").is_some(),
    );
    let session = if cfg!(target_os = "linux") {
        match std::env::var("XDG_SESSION_TYPE").as_deref() {
            Ok("wayland") => "Wayland",
            Ok("x11") => "X11",
            _ if std::env::var_os("WAYLAND_DISPLAY").is_some() => "Wayland",
            _ if std::env::var_os("DISPLAY").is_some() => "X11",
            _ => "Unknown",
        }
    } else {
        os_name()
    };
    let distribution = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/etc/os-release")
            .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
            .ok()
            .and_then(|source| distribution_name(&source))
            .unwrap_or_else(|| "Unknown".into())
    } else {
        "Not applicable".into()
    };
    format!(
        "gmpublished debug information\n\
         Version: {VERSION}\n\
         Build: {}\n\
         OS: {}\n\
         Distribution: {distribution}\n\
         Kernel: {}\n\
         Architecture: {}\n\
         Installation format: {installation}\n\
         Session: {session}\n\
         Graphics backend: {}\n\
         Graphics adapter: {}\n\
         CPU: {}\n\
         Memory: {} MiB\n\
         Display scale: {:.0}%\n\
         Steam: {steam}\n\
         Language: {locale}\n\
         Theme: {theme}",
        if cfg!(debug_assertions) {
            "Debug"
        } else {
            "Release"
        },
        info.system_version.as_deref().unwrap_or(os_name()),
        info.system_kernel.as_deref().unwrap_or("Unknown"),
        std::env::consts::ARCH,
        info.graphics_backend,
        info.graphics_adapter,
        info.cpu_brand,
        info.memory_total / (1024 * 1024),
        scale * 100.0,
    )
}

fn distribution_name(source: &str) -> Option<String> {
    ["PRETTY_NAME=", "NAME="].into_iter().find_map(|prefix| {
        source.lines().find_map(|line| {
            let value = line.trim().strip_prefix(prefix)?.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(value);
            (!value.is_empty()).then(|| value.to_owned())
        })
    })
}

fn installation_format(
    executable: Option<&Path>,
    appimage: bool,
    flatpak: bool,
    snap: bool,
) -> &'static str {
    if appimage {
        return "AppImage";
    }
    if flatpak {
        return "Flatpak";
    }
    if snap {
        return "Snap";
    }
    if let Some(path) = executable {
        if path.starts_with("/nix/store") {
            return "Nix";
        }
        if path
            .components()
            .any(|part| part.as_os_str().to_string_lossy().ends_with(".app"))
        {
            return "macOS app bundle (installer unknown)";
        }
    }
    "Unknown (native executable)"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_detection_does_not_guess_from_generic_install_paths() {
        assert_eq!(
            installation_format(Some(Path::new("/usr/bin/gmpublished")), false, false, false),
            "Unknown (native executable)"
        );
        assert_eq!(
            installation_format(
                Some(Path::new("/nix/store/abc-gmpublished/bin/gmpublished")),
                false,
                false,
                false
            ),
            "Nix"
        );
        assert_eq!(installation_format(None, true, false, false), "AppImage");
        assert_eq!(installation_format(None, false, true, false), "Flatpak");
        assert_eq!(installation_format(None, false, false, true), "Snap");
        assert_eq!(
            installation_format(
                Some(Path::new(
                    "/Applications/gmpublished.app/Contents/MacOS/gmpublished"
                )),
                false,
                false,
                false
            ),
            "macOS app bundle (installer unknown)"
        );
    }

    #[test]
    fn distro_uses_pretty_name_without_shell_evaluation() {
        assert_eq!(
            distribution_name("NAME=NixOS\nPRETTY_NAME=\"NixOS 26.05 (Yarara)\"\n"),
            Some("NixOS 26.05 (Yarara)".into())
        );
        assert_eq!(
            distribution_name("NAME='Example Linux'"),
            Some("Example Linux".into())
        );
        assert_eq!(distribution_name("ID=linux"), None);
    }

    #[test]
    fn late_debug_info_cannot_overwrite_a_new_request_or_reopened_settings() {
        let mut state = AboutState::default();
        let old = state.load();
        let current = state.load();
        state.complete(old, Ok("old".into()));
        assert!(matches!(state.report, DebugReport::Loading));
        state.complete(current, Ok("current".into()));
        assert!(matches!(&state.report, DebugReport::Ready(text) if text == "current"));
        state.reset();
        state.complete(current, Ok("old window".into()));
        assert!(matches!(state.report, DebugReport::Idle));
    }
}
