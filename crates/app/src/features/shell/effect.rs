use super::Route;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    OpenSettings,
    OpenSearchPalette,
    OpenUrl(String),
    Navigated { from: Route, to: Route },
    BeginWindowDrag,
    ToggleMaximize,
}
