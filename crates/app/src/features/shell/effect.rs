use super::Route;

/// Outward consequences of a shell state transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    OpenSettings,
    OpenSearchPalette,
    OpenUrl(String),
    Navigated { from: Route, to: Route },
    BeginWindowDrag,
    ToggleMaximize,
}
