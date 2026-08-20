#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    OpenDescriptionEditor,
    OpenDestinationSelect,
    OpenPreparePublish,
    OpenPreviewGma,
    OpenSettings,
    CloseRequested,
}
