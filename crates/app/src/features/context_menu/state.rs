use std::{path::PathBuf, time::Instant};

use iced::Point;
use iced::animation::Easing;

use crate::bridge::domain::PublishedFileId;
use crate::theme::{self, motion};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMenuTarget {
    pub(crate) path: PathBuf,
    pub(crate) path_text: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) workshop_url: Option<String>,
    pub(crate) preview_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Local(LocalMenuTarget),
    MyWorkshop {
        workshop_id: PublishedFileId,
        workshop_url: String,
        preview_url: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextMenuAction {
    Extract,
    OpenAddonLocation,
    CopyPath,
    SteamWorkshop,
    CopyLink,
    Download,
    OpenImage,
    CopyImageLink,
    #[cfg(feature = "debug")]
    HideAddon,
    #[cfg(feature = "debug")]
    AdjustSubscribers(i64),
    #[cfg(feature = "debug")]
    SimulateToast(SimulatedToast),
}

/// Debug-only fake tasks for exercising the tasks overlay end to end,
/// including cancellation of the slow-running success case.
#[cfg(feature = "debug")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulatedToast {
    Success,
    Error,
    Notice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Icon {
    Extract,
    OpenLocation,
    Copy,
    OpenExternal,
    CopyLink,
    Download,
    Image,
    #[cfg(feature = "debug")]
    Hide,
    #[cfg(feature = "debug")]
    DebugPlus,
    #[cfg(feature = "debug")]
    DebugMinus,
}

/// One row of a context menu: either a divider, or something pressable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Entry {
    Separator,
    Item {
        label_key: &'static str,
        action: ContextMenuAction,
        icon: Icon,
    },
}

impl Entry {
    const fn actionable(label_key: &'static str, action: ContextMenuAction, icon: Icon) -> Self {
        Self::Item {
            label_key,
            action,
            icon,
        }
    }

    pub(crate) const fn extract() -> Self {
        Self::actionable(
            "context-menu-extract",
            ContextMenuAction::Extract,
            Icon::Extract,
        )
    }

    pub(crate) const fn open_addon_location() -> Self {
        Self::actionable(
            "context-menu-open-addon-location",
            ContextMenuAction::OpenAddonLocation,
            Icon::OpenLocation,
        )
    }

    pub(crate) const fn copy_path() -> Self {
        Self::actionable(
            "context-menu-copy-path",
            ContextMenuAction::CopyPath,
            Icon::Copy,
        )
    }

    pub(crate) const fn steam_workshop() -> Self {
        Self::actionable(
            "context-menu-steam-workshop",
            ContextMenuAction::SteamWorkshop,
            Icon::OpenExternal,
        )
    }

    pub(crate) const fn copy_link() -> Self {
        Self::actionable(
            "context-menu-copy-link",
            ContextMenuAction::CopyLink,
            Icon::CopyLink,
        )
    }

    pub(crate) const fn download() -> Self {
        Self::actionable(
            "context-menu-download",
            ContextMenuAction::Download,
            Icon::Download,
        )
    }

    pub(crate) const fn open_image() -> Self {
        Self::actionable(
            "context-menu-open-image",
            ContextMenuAction::OpenImage,
            Icon::OpenExternal,
        )
    }

    pub(crate) const fn copy_image_link() -> Self {
        Self::actionable(
            "context-menu-copy-image-link",
            ContextMenuAction::CopyImageLink,
            Icon::Image,
        )
    }

    #[cfg(feature = "debug")]
    pub(crate) const fn hide_addon() -> Self {
        Self::actionable(
            "context-menu-hide-addon",
            ContextMenuAction::HideAddon,
            Icon::Hide,
        )
    }

    #[cfg(feature = "debug")]
    pub(crate) const fn simulate_toast(kind: SimulatedToast) -> Self {
        let label_key = match kind {
            SimulatedToast::Success => "context-menu-debug-toast-success",
            SimulatedToast::Error => "context-menu-debug-toast-error",
            SimulatedToast::Notice => "context-menu-debug-toast-notice",
        };
        Self::actionable(
            label_key,
            ContextMenuAction::SimulateToast(kind),
            Icon::DebugPlus,
        )
    }

    #[cfg(feature = "debug")]
    pub(crate) const fn adjust_subscribers(delta: i64) -> Self {
        let (label_key, icon) = match delta {
            10 => ("context-menu-debug-simulate-plus", Icon::DebugPlus),
            -10 => ("context-menu-debug-simulate-minus", Icon::DebugMinus),
            1_000_000 => ("context-menu-debug-simulate-plus-million", Icon::DebugPlus),
            -1_000_000 => (
                "context-menu-debug-simulate-minus-million",
                Icon::DebugMinus,
            ),
            _ => panic!("unsupported debug subscriber adjustment"),
        };
        Self::actionable(label_key, ContextMenuAction::AdjustSubscribers(delta), icon)
    }

    pub(crate) const fn separator() -> Self {
        Self::Separator
    }

    pub(crate) const fn separator_row(&self) -> bool {
        matches!(self, Self::Separator)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    position: Point,
    entries: Vec<Entry>,
    target: ContextMenuTarget,
}

impl OpenRequest {
    pub(crate) fn new(position: Point, entries: Vec<Entry>, target: ContextMenuTarget) -> Self {
        Self {
            position,
            entries,
            target,
        }
    }
}

#[derive(Debug, Default)]
enum ContextMenuSession {
    #[default]
    Closed,
    Open {
        target: Option<ContextMenuTarget>,
    },
    Closing {
        target: Option<ContextMenuTarget>,
    },
}

impl ContextMenuSession {
    const fn open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    const fn visible(&self) -> bool {
        !matches!(self, Self::Closed)
    }

    #[cfg(test)]
    const fn target(&self) -> Option<&ContextMenuTarget> {
        match self {
            Self::Closed => None,
            Self::Open { target } | Self::Closing { target } => target.as_ref(),
        }
    }

    fn take_target(&mut self) -> Option<ContextMenuTarget> {
        match self {
            Self::Closed => None,
            Self::Open { target } | Self::Closing { target } => target.take(),
        }
    }

    fn begin_close(&mut self) {
        let current = std::mem::take(self);
        *self = match current {
            Self::Open { target } | Self::Closing { target } => Self::Closing { target },
            Self::Closed => Self::Closed,
        };
    }
}

#[derive(Debug)]
pub struct State {
    session: ContextMenuSession,
    position: Point,
    entries: Vec<Entry>,
    presence: motion::Presence<bool>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            session: ContextMenuSession::Closed,
            position: Point::ORIGIN,
            entries: Vec::new(),
            presence: motion::asymmetric(
                false,
                theme::invariant().motion.context_menu_enter_duration(),
                theme::invariant().motion.context_menu_exit_duration(),
                Easing::EaseOut,
            ),
        }
    }
}

impl State {
    pub(crate) const fn open(&self) -> bool {
        self.session.open()
    }

    pub(crate) const fn visible(&self) -> bool {
        self.session.visible()
    }

    #[cfg(test)]
    pub(crate) const fn target(&self) -> Option<&ContextMenuTarget> {
        self.session.target()
    }

    /// Consumes this session's action target without disturbing its fade-out
    /// entries. A second queued action for the same rendered menu is ignored.
    pub(crate) fn take_target(&mut self) -> Option<ContextMenuTarget> {
        self.session.take_target()
    }

    pub(crate) const fn position(&self) -> Point {
        self.position
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        self.presence.interpolate(0.0, 1.0, now)
    }

    pub(crate) fn scale(&self, now: Instant) -> f32 {
        self.presence
            .interpolate(motion::POPOVER_CLOSED_SCALE, 1.0, now)
    }

    pub(crate) fn needs_ticks(&self) -> bool {
        self.presence.needs_ticks()
    }

    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let settled = self.presence.tick(now);
        if settled && matches!(self.session, ContextMenuSession::Closing { .. }) {
            self.session = ContextMenuSession::Closed;
            self.entries.clear();
            true
        } else {
            false
        }
    }

    pub(super) fn open_request(&mut self, request: OpenRequest, now: Instant) {
        self.session = ContextMenuSession::Open {
            target: Some(request.target),
        };
        self.position = request.position;
        self.entries = request.entries;

        // Overlay simulators ride along in every menu so the toast states
        // can be exercised from anywhere.
        #[cfg(feature = "debug")]
        self.entries.extend([
            Entry::separator(),
            Entry::simulate_toast(SimulatedToast::Success),
            Entry::simulate_toast(SimulatedToast::Error),
            Entry::simulate_toast(SimulatedToast::Notice),
        ]);

        self.presence.go(true, now);
    }

    pub(super) fn dismiss(&mut self, now: Instant) {
        self.session.begin_close();
        self.presence.go(false, now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn target() -> ContextMenuTarget {
        ContextMenuTarget::Local(LocalMenuTarget {
            path: PathBuf::from("/tmp/addon.gma"),
            path_text: "/tmp/addon.gma".to_owned(),
            workshop_id: None,
            workshop_url: None,
            preview_url: None,
        })
    }

    #[test]
    fn entrance_starts_visibly_instead_of_snapping_open() {
        let mut state = State::default();
        let now = Instant::now();
        state.open_request(
            OpenRequest::new(Point::ORIGIN, vec![Entry::copy_path()], target()),
            now,
        );

        let first_frame = now + Duration::from_millis(16);
        let midpoint = now + Duration::from_millis(60);
        let settled = now + Duration::from_millis(300);

        assert!((0.1..0.3).contains(&state.opacity(first_frame)));
        assert!(state.scale(midpoint) > motion::POPOVER_CLOSED_SCALE);
        assert!(state.scale(midpoint) < 1.0);
        assert_eq!(state.scale(settled), 1.0);

        state.dismiss(settled);

        let closing_midpoint = settled + Duration::from_millis(50);
        let closed = settled + Duration::from_millis(300);

        assert!(state.scale(closing_midpoint) > motion::POPOVER_CLOSED_SCALE);
        assert!(state.scale(closing_midpoint) < 1.0);
        assert_eq!(state.scale(closed), motion::POPOVER_CLOSED_SCALE);
    }
}
