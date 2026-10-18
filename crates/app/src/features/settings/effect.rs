use super::state::{PathSetting, PathValidationRequest};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    ModalCloseRequested,
    OpenUrlRequested(&'static str),
    DebugInfoRequested(crate::generation::Generation),
    CopyDebugInfoRequested(crate::generation::Generation, String),
    PathBrowseRequested(PathSetting),
    PathValidationRequested(PathValidationRequest),
    FocusMoved(bool),
    SettingsChanged,
    SnapshotApplied,
    PersistenceRequested,
}
