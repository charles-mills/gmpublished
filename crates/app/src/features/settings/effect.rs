use super::state::{
    PathSetting, PathValidationRequest, ResetAction, SettingsMutation, SettingsSnapshot,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    ModalCloseRequested,
    OpenUrlRequested(&'static str),
    DebugInfoRequested(crate::generation::Generation),
    CopyDebugInfoRequested(crate::generation::Generation, String),
    PathBrowseRequested(PathSetting),
    PathValidationRequested(PathValidationRequest),
    MutationApplied(crate::generation::Generation, SettingsMutation),
    SnapshotApplied(Box<SettingsSnapshot>),
    ResetRunRequested(ResetAction),
}
