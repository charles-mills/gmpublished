//! Native OS integration, as one message rather than one root variant per
//! affordance.
//!
//! Off macOS [`Message`] is uninhabited, so the root's single arm is
//! statically unreachable and the subscriptions list is empty — where four
//! `cfg`'d variants, four `cfg`'d dispatch arms and three `cfg`'d subscription
//! pushes each had to be kept in step by hand.

use iced::Subscription;

#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
#[derive(Clone, Debug)]
pub enum Message {
    MenuCommand(crate::platform_menu::Command),
    MenuOpenGmaCompleted(Option<PathBuf>),
    /// The system flipped between light and dark appearance; AppKit resets the
    /// custom traffic-light frames during that re-layout.
    SystemAppearanceChanged,
    /// `.gma` documents were opened via the OS file association (double-click
    /// or "Open With"), delivered by the platform-open bridge.
    GmaDocumentsOpened(Vec<PathBuf>),
}

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Debug)]
pub enum Message {}

/// Every platform stream the root should subscribe to.
#[cfg(target_os = "macos")]
#[must_use]
pub fn subscriptions() -> Vec<Subscription<Message>> {
    vec![
        crate::platform_menu::subscription().map(Message::MenuCommand),
        crate::platform_chrome::appearance_change_subscription()
            .map(|()| Message::SystemAppearanceChanged),
        crate::platform_open::subscription().map(Message::GmaDocumentsOpened),
    ]
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn subscriptions() -> Vec<Subscription<Message>> {
    Vec::new()
}
