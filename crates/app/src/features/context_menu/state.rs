use std::time::Instant;

use iced::Point;

use crate::theme::{Tokens, motion};

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
pub enum Owner {
    InstalledAddons,
    MyWorkshop,
    SizeAnalyzer,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    label_key: &'static str,
    action: Option<ContextMenuAction>,
    separator: bool,
    icon: Option<Icon>,
}

impl Entry {
    const fn actionable(label_key: &'static str, action: ContextMenuAction, icon: Icon) -> Self {
        Self {
            label_key,
            action: Some(action),
            separator: false,
            icon: Some(icon),
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
        Self {
            label_key: "",
            action: None,
            separator: true,
            icon: None,
        }
    }

    pub(crate) const fn label_key(&self) -> &'static str {
        self.label_key
    }

    /// The entry's dispatched action. Only `None` for separator rows, which
    /// never render as pressable.
    pub(crate) const fn action(&self) -> Option<ContextMenuAction> {
        self.action
    }

    pub(crate) const fn separator_row(&self) -> bool {
        self.separator
    }

    pub(crate) const fn icon(&self) -> Option<Icon> {
        self.icon
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    owner: Owner,
    position: Point,
    entries: Vec<Entry>,
}

impl OpenRequest {
    pub(crate) fn new(owner: Owner, position: Point, entries: Vec<Entry>) -> Self {
        Self {
            owner,
            position,
            entries,
        }
    }
}

#[derive(Clone, Debug)]
pub struct State {
    open: bool,
    visible: bool,
    owner: Option<Owner>,
    position: Point,
    entries: Vec<Entry>,
    presence: motion::Presence<bool>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            visible: false,
            owner: None,
            position: Point::ORIGIN,
            entries: Vec::new(),
            presence: motion::asymmetric(
                false,
                Tokens::dark().motion.context_menu_enter_duration(),
                Tokens::dark().motion.context_menu_exit_duration(),
                motion::expo_ease(),
            ),
        }
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.open == other.open
            && self.visible == other.visible
            && self.owner == other.owner
            && self.position == other.position
            && self.entries == other.entries
            && self.presence == other.presence
    }
}

impl State {
    pub(crate) const fn open(&self) -> bool {
        self.open
    }

    pub(crate) const fn visible(&self) -> bool {
        self.visible
    }

    #[cfg(test)]
    pub(crate) const fn owner(&self) -> Option<Owner> {
        self.owner
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
        if settled && !self.open && self.visible {
            self.visible = false;
            self.owner = None;
            self.entries.clear();
            true
        } else {
            false
        }
    }

    pub(super) fn open_request(&mut self, request: OpenRequest, now: Instant) {
        self.open = true;
        self.visible = true;
        self.owner = Some(request.owner);
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
        self.open = false;
        self.presence.go(false, now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn micro_scale_tracks_the_shared_presence_animation() {
        let mut state = State::default();
        let now = Instant::now();
        state.open_request(
            OpenRequest::new(
                Owner::InstalledAddons,
                Point::ORIGIN,
                vec![Entry::copy_path()],
            ),
            now,
        );

        let midpoint = now + Duration::from_millis(60);
        let settled = now + Duration::from_millis(300);

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
