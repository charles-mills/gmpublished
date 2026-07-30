use super::model::{CustomPathValidationRequest, DestinationPersistRequest};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    SnapshotApplied,
    FolderPickerRequested,
    CustomPathValidationRequested(CustomPathValidationRequest),
    CreateFolderChanged(bool),
    DestinationPersistRequested(DestinationPersistRequest),
    DestinationPersisted,
    DestinationDismissed,
}
