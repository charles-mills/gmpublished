use parking_lot::Mutex;

use super::{
    Arc, AtomicBool, AtomicU64, BACKEND_EVENTS_ID, BackendRuntimeEvent, Hash, Hasher, Ordering,
    Subscription, TASK_EVENTS_ID, UiError, fmt, iced_mpsc, mpsc, stream, thread,
};

/// Event sent from worker code to the UI-facing task drain.
pub type TaskEvent = (TaskId, SharedTaskUpdate);

#[derive(Clone, Debug)]
pub struct SharedTaskUpdate(Arc<TaskUpdate>);

impl SharedTaskUpdate {
    pub(crate) fn new(update: TaskUpdate) -> Self {
        Self(Arc::new(update))
    }

    pub(crate) fn as_update(&self) -> &TaskUpdate {
        self.0.as_ref()
    }

    pub(crate) fn into_update(self) -> TaskUpdate {
        Arc::try_unwrap(self.0).unwrap_or_else(|update| update.as_ref().clone())
    }
}

impl From<TaskUpdate> for SharedTaskUpdate {
    fn from(update: TaskUpdate) -> Self {
        Self::new(update)
    }
}

impl std::ops::Deref for SharedTaskUpdate {
    type Target = TaskUpdate;

    fn deref(&self) -> &Self::Target {
        self.as_update()
    }
}

impl PartialEq for SharedTaskUpdate {
    fn eq(&self, other: &Self) -> bool {
        self.as_update() == other.as_update()
    }
}

impl PartialEq<TaskUpdate> for SharedTaskUpdate {
    fn eq(&self, other: &TaskUpdate) -> bool {
        self.as_update() == other
    }
}

impl PartialEq<SharedTaskUpdate> for TaskUpdate {
    fn eq(&self, other: &SharedTaskUpdate) -> bool {
        self == other.as_update()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(u64);

impl TaskId {
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Coarse task classification used by the UI for labels and icons, and to
/// route each task to the surface that displays it: `Download`/`Extract` own
/// Downloader-page rows, while `Publish`/`OverlayExtract`/`Notice` feed the
/// bottom tasks overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskKind {
    Publish,
    Extract,
    /// An extraction invoked outside the Downloader page (GMA preview,
    /// document open), toasted instead of getting a Downloader row.
    OverlayExtract,
    /// A one-shot overlay message: created already finished, its status key
    /// is the message. Only the debug toast simulator constructs one today.
    #[cfg_attr(not(any(feature = "debug", test)), expect(dead_code))]
    Notice,
    Download,
    Search,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkshopDownloadTaskKind {
    Download,
    Extract,
}

/// Placeholder value attached to a named status key.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskValue {
    Text(String),
    U64(u64),
    F64(f64),
    Bool(bool),
}

impl From<&str> for TaskValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for TaskValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<u64> for TaskValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<u32> for TaskValue {
    fn from(value: u32) -> Self {
        Self::U64(u64::from(value))
    }
}

impl From<f64> for TaskValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<bool> for TaskValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// I18n status key with optional named placeholder values.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusKey {
    pub(crate) key: String,
    pub(crate) values: Vec<(String, TaskValue)>,
}

impl StatusKey {
    pub(crate) fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            values: Vec::new(),
        }
    }
}

impl From<&str> for StatusKey {
    fn from(key: &str) -> Self {
        Self::new(key)
    }
}

impl From<String> for StatusKey {
    fn from(key: String) -> Self {
        Self::new(key)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskUpdate {
    Started { kind: TaskKind, status: StatusKey },
    Status(StatusKey),
    Progress(f64),
    ProgressIncr(f64),
    ProgressReset,
    Total(u64),
    Finished,
    Error(UiError),
    Abandoned,
}

/// Terminal update retained after coalescing a task event batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoalescedTaskTerminal {
    Finished,
    Error(UiError),
    Abandoned,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskEventUpdate {
    Update(SharedTaskUpdate),
    Terminal(CoalescedTaskTerminal),
}

impl From<TaskUpdate> for TaskEventUpdate {
    fn from(update: TaskUpdate) -> Self {
        match update {
            TaskUpdate::Finished => Self::Terminal(CoalescedTaskTerminal::Finished),
            TaskUpdate::Error(error) => Self::Terminal(CoalescedTaskTerminal::Error(error)),
            TaskUpdate::Abandoned => Self::Terminal(CoalescedTaskTerminal::Abandoned),
            update => Self::Update(SharedTaskUpdate::new(update)),
        }
    }
}

impl From<SharedTaskUpdate> for TaskEventUpdate {
    fn from(update: SharedTaskUpdate) -> Self {
        Self::Update(update)
    }
}

impl From<CoalescedTaskTerminal> for TaskEventUpdate {
    fn from(terminal: CoalescedTaskTerminal) -> Self {
        Self::Terminal(terminal)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoalescedTaskStart {
    pub(crate) kind: TaskKind,
    pub(crate) status: StatusKey,
}

/// Coalesced update state for one task in one drained event batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoalescedTaskUpdate {
    pub(crate) started: Option<CoalescedTaskStart>,
    pub(crate) status: Option<StatusKey>,
    pub(crate) progress: Option<f64>,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) terminal: Option<CoalescedTaskTerminal>,
}

impl CoalescedTaskUpdate {
    pub(crate) fn observe(&mut self, update: impl Into<TaskEventUpdate>, current_progress: f64) {
        match update.into() {
            TaskEventUpdate::Update(update) => {
                self.observe_task_update(update.into_update(), current_progress);
            }
            TaskEventUpdate::Terminal(terminal) => {
                self.terminal = Some(terminal);
            }
        }
    }

    fn observe_task_update(&mut self, update: TaskUpdate, current_progress: f64) {
        match update {
            TaskUpdate::Started { kind, status } => {
                self.started = Some(CoalescedTaskStart { kind, status });
                self.status = None;
                self.progress = Some(0.0);
                self.total_bytes = Some(0);
                self.terminal = None;
            }
            TaskUpdate::Status(status) => {
                self.status = Some(status);
            }
            TaskUpdate::Progress(progress) => {
                self.progress = Some(progress.clamp(0.0, 1.0));
            }
            TaskUpdate::ProgressIncr(delta) => {
                let base = self.progress.unwrap_or(current_progress);
                self.progress = Some((base + delta).clamp(0.0, 1.0));
            }
            TaskUpdate::ProgressReset => {
                self.progress = Some(0.0);
            }
            TaskUpdate::Total(total_bytes) => {
                self.total_bytes = Some(total_bytes);
            }
            TaskUpdate::Finished => {
                self.terminal = Some(CoalescedTaskTerminal::Finished);
            }
            TaskUpdate::Abandoned => {
                self.terminal = Some(CoalescedTaskTerminal::Abandoned);
            }
            TaskUpdate::Error(error) => {
                self.terminal = Some(CoalescedTaskTerminal::Error(error));
            }
        }
    }
}

#[derive(Debug)]
pub struct TaskHandle {
    id: TaskId,
    terminal: Arc<AtomicBool>,
    sender: mpsc::Sender<TaskEvent>,
}

impl TaskHandle {
    pub(crate) const fn id(&self) -> TaskId {
        self.id
    }

    pub(crate) fn status(&self, key: impl Into<StatusKey>) {
        self.emit(TaskUpdate::Status(key.into()));
    }

    pub(crate) fn progress(&self, progress: f64) {
        self.emit(TaskUpdate::Progress(progress));
    }

    pub(crate) fn progress_incr(&self, progress: f64) {
        self.emit(TaskUpdate::ProgressIncr(progress));
    }

    pub(crate) fn progress_reset(&self) {
        self.emit(TaskUpdate::ProgressReset);
    }

    pub(crate) fn total(&self, bytes: u64) {
        self.emit(TaskUpdate::Total(bytes));
    }

    pub(crate) fn finished(&self) {
        self.emit_terminal(TaskUpdate::Finished);
    }

    pub(crate) fn error(&self, error: impl Into<UiError>) {
        self.emit_terminal(TaskUpdate::Error(error.into()));
    }

    fn emit(&self, update: TaskUpdate) {
        let _send_result = self.sender.send((self.id, SharedTaskUpdate::new(update)));
    }

    fn emit_terminal(&self, update: TaskUpdate) {
        if self.terminal.swap(true, Ordering::AcqRel) {
            return;
        }

        self.emit(update);
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if self.terminal.swap(true, Ordering::AcqRel) {
            return;
        }

        self.emit(TaskUpdate::Abandoned);
    }
}

/// Registry for creating task handles.
#[derive(Debug)]
pub struct Tasks {
    next_id: AtomicU64,
    sender: mpsc::Sender<TaskEvent>,
}

impl Tasks {
    pub(super) fn channel() -> (Self, mpsc::Receiver<TaskEvent>) {
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                next_id: AtomicU64::new(1),
                sender,
            },
            receiver,
        )
    }

    pub(crate) fn create(
        &self,
        kind: TaskKind,
        initial_status: impl Into<StatusKey>,
    ) -> TaskHandle {
        let id = TaskId::from_raw(self.next_id.fetch_add(1, Ordering::Relaxed));

        let handle = TaskHandle {
            id,
            terminal: Arc::new(AtomicBool::new(false)),
            sender: self.sender.clone(),
        };

        handle.emit(TaskUpdate::Started {
            kind,
            status: initial_status.into(),
        });

        handle
    }
}

#[derive(Clone)]
pub(super) struct TaskEventStreamFactory {
    id: u64,
    receiver: Arc<Mutex<Option<mpsc::Receiver<TaskEvent>>>>,
}

impl TaskEventStreamFactory {
    pub(super) fn new(receiver: Option<mpsc::Receiver<TaskEvent>>) -> Self {
        Self {
            id: TASK_EVENTS_ID,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub(super) fn subscription(&self) -> Subscription<TaskEvent> {
        Subscription::run_with(self.clone(), task_event_stream)
    }

    pub(super) fn take_receiver(&self) -> Option<mpsc::Receiver<TaskEvent>> {
        self.receiver.lock().take()
    }
}

impl fmt::Debug for TaskEventStreamFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskEventStreamFactory")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Hash for TaskEventStreamFactory {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

pub(super) fn task_event_stream(
    factory: &TaskEventStreamFactory,
) -> impl iced::futures::Stream<Item = TaskEvent> + use<> {
    let factory = factory.clone();
    stream::channel(100, async move |output| {
        let Some(receiver) = factory.take_receiver() else {
            return;
        };

        spawn_forwarder_thread("task-event-forwarder", receiver, output);
    })
}

/// Spawns a process-lifetime forwarding loop on its own thread rather than
/// submitting it to the bounded blocking worker pool: it never returns, so
/// on a pool this small it would permanently pin one of only a handful of
/// worker threads.
fn spawn_forwarder_thread<T: Send + 'static>(
    name: &'static str,
    receiver: mpsc::Receiver<T>,
    output: iced_mpsc::Sender<T>,
) {
    if let Err(error) = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || forward_to_subscription(&receiver, output))
    {
        log::warn!("failed to start {name} thread: {error}");
    }
}

fn forward_to_subscription<T>(receiver: &mpsc::Receiver<T>, mut output: iced_mpsc::Sender<T>) {
    while let Ok(event) = receiver.recv() {
        if !crate::util::channel::send_blocking(&mut output, event) {
            return;
        }
    }
}

#[derive(Clone)]
pub(super) struct BackendEventStreamFactory {
    id: u64,
    receiver: Arc<Mutex<Option<mpsc::Receiver<BackendRuntimeEvent>>>>,
}

impl BackendEventStreamFactory {
    pub(super) fn new(receiver: Option<mpsc::Receiver<BackendRuntimeEvent>>) -> Self {
        Self {
            id: BACKEND_EVENTS_ID,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub(super) fn subscription(&self) -> Subscription<BackendRuntimeEvent> {
        Subscription::run_with(self.clone(), backend_event_stream)
    }

    pub(super) fn take_receiver(&self) -> Option<mpsc::Receiver<BackendRuntimeEvent>> {
        self.receiver.lock().take()
    }
}

impl fmt::Debug for BackendEventStreamFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendEventStreamFactory")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Hash for BackendEventStreamFactory {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

pub(super) fn backend_event_stream(
    factory: &BackendEventStreamFactory,
) -> impl iced::futures::Stream<Item = BackendRuntimeEvent> + use<> {
    let factory = factory.clone();
    stream::channel(100, async move |output| {
        let Some(receiver) = factory.take_receiver() else {
            return;
        };

        spawn_forwarder_thread("backend-event-forwarder", receiver, output);
    })
}
