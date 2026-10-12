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
    GmaDocumentsOpened(Vec<PathBuf>),
}

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Debug)]
pub enum Message {}

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
