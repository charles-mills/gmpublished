use super::model::DestinationPersistRequest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    SnapshotApplied,
    FolderPickerRequested,
    CreateFolderChanged(bool),
    DestinationPersistRequested(DestinationPersistRequest),
    DestinationPersisted,
    DestinationDismissed,
}
