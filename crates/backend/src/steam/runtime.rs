use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use steamworks::SteamId;

use super::Steam;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamRuntimeUser {
    pub steamid: SteamId,
    pub name: String,
    pub avatar: Option<SteamAvatarRgba>,
    pub dead: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamAvatarRgba {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl From<crate::RgbaImage> for SteamAvatarRgba {
    fn from(image: crate::RgbaImage) -> Self {
        let (rgba, width, height) = image.into_rgba_parts();
        Self {
            width,
            height,
            rgba,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteamRuntimeStatus {
    NotStarted,
    Connecting,
    Connected,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SteamRuntimeError {
    #[error("ERR_STEAM_ERROR:STEAM_UNAVAILABLE")]
    Unavailable,
    #[error("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED")]
    NotConnected,
}

impl crate::error_key::HasErrorKey for SteamRuntimeError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        crate::error_key::keys::STEAM_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::Unavailable => Some("STEAM_UNAVAILABLE".to_owned()),
            Self::NotConnected => Some("STEAM_NOT_CONNECTED".to_owned()),
        }
    }
}

#[derive(Clone)]
pub struct SteamRuntime {
    inner: Arc<SteamRuntimeInner>,
}

struct SteamRuntimeInner {
    started: AtomicBool,
    unavailable: AtomicBool,
    /// `None` means this runtime was built disabled (tests): it never
    /// touches a real `Steam` connection.
    steam: Option<Arc<Steam>>,
    connect_timeout: Duration,
}

impl SteamRuntime {
    #[must_use]
    pub fn new(steam: Arc<Steam>) -> Self {
        Self::with_options(Some(steam), DEFAULT_CONNECT_TIMEOUT)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn unavailable_for_tests() -> Self {
        Self::with_options(None, Duration::ZERO)
    }

    fn with_options(steam: Option<Arc<Steam>>, connect_timeout: Duration) -> Self {
        let disabled = steam.is_none();
        Self {
            inner: Arc::new(SteamRuntimeInner {
                started: AtomicBool::new(false),
                unavailable: AtomicBool::new(disabled),
                steam,
                connect_timeout,
            }),
        }
    }

    pub fn status(&self) -> SteamRuntimeStatus {
        if self.is_connected() {
            return SteamRuntimeStatus::Connected;
        }

        if !self.inner.started.load(Ordering::Acquire) {
            return SteamRuntimeStatus::NotStarted;
        }

        if self.inner.unavailable.load(Ordering::Acquire) {
            SteamRuntimeStatus::Unavailable
        } else {
            SteamRuntimeStatus::Connecting
        }
    }

    pub fn is_connected(&self) -> bool {
        self.inner
            .steam
            .as_ref()
            .is_some_and(|steam| steam.connected())
    }

    pub fn connect(&self) -> Result<(), SteamRuntimeError> {
        let Some(steam) = &self.inner.steam else {
            return Err(SteamRuntimeError::Unavailable);
        };

        self.inner.unavailable.store(false, Ordering::Release);
        self.inner.started.store(true, Ordering::Release);

        if steam.wait_for_connected(self.inner.connect_timeout) {
            self.inner.unavailable.store(false, Ordering::Release);
            Ok(())
        } else {
            self.inner.unavailable.store(true, Ordering::Release);
            Err(SteamRuntimeError::Unavailable)
        }
    }

    pub fn current_user(&self) -> Result<SteamRuntimeUser, SteamRuntimeError> {
        let Some(steam) = &self.inner.steam else {
            return Err(SteamRuntimeError::NotConnected);
        };
        if !self.is_connected() {
            return Err(SteamRuntimeError::NotConnected);
        }

        let user = steam.current_user();
        Ok(SteamRuntimeUser {
            steamid: user.steamid,
            name: user.name,
            avatar: user.avatar.map(SteamAvatarRgba::from),
            dead: user.dead,
        })
    }
}

impl fmt::Debug for SteamRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SteamRuntime")
            .field("status", &self.status())
            .field("disabled", &self.inner.steam.is_none())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Steam, SteamRuntime, SteamRuntimeError, SteamRuntimeStatus};
    use crate::{events::NullEventSink, transactions::Transactions};

    fn test_steam() -> Arc<Steam> {
        Arc::new(Steam::new(Transactions::new(
            Arc::new(NullEventSink),
            false,
        )))
    }

    #[test]
    fn disabled_runtime_reports_unavailable_without_starting_steam() {
        let runtime = SteamRuntime::unavailable_for_tests();

        assert!(!runtime.is_connected());
        assert_eq!(runtime.status(), SteamRuntimeStatus::NotStarted);
        assert_eq!(runtime.connect(), Err(SteamRuntimeError::Unavailable));
        assert!(!runtime.is_connected());
        assert_eq!(runtime.status(), SteamRuntimeStatus::NotStarted);
    }

    #[test]
    fn fresh_runtime_reads_underlying_steam_connected_flag_without_connecting() {
        let steam = test_steam();
        // Same-crate access: `runtime` is a descendant module of `steam`, so
        // this private setter (normally flipped only by the connect
        // callbacks) is visible here for the test double.
        steam.set_connected(true);
        let runtime = SteamRuntime::new(steam);

        assert!(runtime.is_connected());
        assert_eq!(runtime.status(), SteamRuntimeStatus::Connected);
    }
}
