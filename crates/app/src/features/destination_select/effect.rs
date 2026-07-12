use super::model::DestinationPersistRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    ModalOpenRequested,
    SnapshotApplied,
    FolderPickerRequested,
    CreateFolderChanged(bool),
    DestinationPersistRequested(DestinationPersistRequest),
    DestinationPersisted,
    DestinationDismissed,
}
