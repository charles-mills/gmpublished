#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    OpenDestinationSelect,
    OpenPreparePublish,
    OpenPreviewGma,
    OpenSettings,
    CloseRequested,
}
