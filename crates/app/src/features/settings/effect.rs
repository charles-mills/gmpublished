use super::state::{
    PathSetting, PathValidationRequest, ResetAction, SettingsMutation, SettingsSnapshot,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    ModalCloseRequested,
    PathBrowseRequested(PathSetting),
    PathValidationRequested(PathValidationRequest),
    MutationApplied(SettingsMutation),
    SnapshotApplied(Box<SettingsSnapshot>),
    ResetRunRequested(ResetAction),
}
