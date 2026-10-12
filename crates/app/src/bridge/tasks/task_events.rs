use parking_lot::{Condvar, Mutex};

use super::{BACKEND_EVENTS_ID, BackendRuntimeEvent, TASK_EVENTS_ID, TransactionStatus, UiError};
use iced::Subscription;
use iced::futures::channel::mpsc as iced_mpsc;
use iced::stream;
use std::fmt;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskKind {
    Publish,
    Extract,
    OverlayExtract,
    /// Already finished when created; status_key holds the notice text.
    #[cfg_attr(not(any(feature = "debug", test)), expect(dead_code))]
    Notice,
    Download,
    WorkshopSnapshot,
    Search,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkshopDownloadTaskKind {
    Download,
    Extract,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskUpdate {
    Started {
        kind: TaskKind,
        status: TransactionStatus,
    },
    Status(TransactionStatus),
    Progress(f64),
    ProgressIncr(f64),
    ProgressReset,
    Total(u64),
    Finished,
    Error(UiError),
    Abandoned,
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoalescedTaskStart {
    pub(crate) kind: TaskKind,
    pub(crate) status: TransactionStatus,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoalescedTaskUpdate {
    pub(crate) started: Option<CoalescedTaskStart>,
    pub(crate) status: Option<TransactionStatus>,
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

    pub(crate) fn status(&self, status: TransactionStatus) {
        self.emit(TaskUpdate::Status(status));
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

    pub(crate) fn create(&self, kind: TaskKind, initial_status: TransactionStatus) -> TaskHandle {
        let id = TaskId::from_raw(self.next_id.fetch_add(1, Ordering::Relaxed));

        let handle = TaskHandle {
            id,
            terminal: Arc::new(AtomicBool::new(false)),
            sender: self.sender.clone(),
        };

        handle.emit(TaskUpdate::Started {
            kind,
            status: initial_status,
        });

        handle
    }
}

#[derive(Clone)]
pub(super) struct TaskEventStreamFactory {
    id: u64,
    receiver: Arc<Mutex<Option<mpsc::Receiver<TaskEvent>>>>,
    output: Arc<ForwarderOutput<TaskEvent>>,
}

impl TaskEventStreamFactory {
    pub(super) fn new(receiver: mpsc::Receiver<TaskEvent>) -> Self {
        Self {
            id: TASK_EVENTS_ID,
            receiver: Arc::new(Mutex::new(Some(receiver))),
            output: Arc::new(ForwarderOutput::new()),
        }
    }

    pub(super) fn subscription(&self) -> Subscription<TaskEvent> {
        Subscription::run_with(self.clone(), task_event_stream)
    }

    /// Only tests take the receiver directly; production claims it in attach_stream_run.
    #[cfg(test)]
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
        attach_stream_run(
            "task-event-forwarder",
            &factory.receiver,
            &factory.output,
            output,
        );
    })
}

/// Replaceable Iced sender for a process-lifetime forwarder. Iced may rebuild
/// subscription streams, but the std receiver can only be taken once.
/// The forwarder reattaches and retries its pending event on the new sender.
/// Events buffered in the old Iced channel are lost when that stream drops.
struct ForwarderOutput<T> {
    state: Mutex<ForwarderOutputState<T>>,
    attached: Condvar,
}

struct ForwarderOutputState<T> {
    sender: Option<iced_mpsc::Sender<T>>,
    /// Counts attachments so a delivery that failed against a stale sender
    /// can distinguish "no replacement yet" from "replacement installed"
    /// without comparing sender identities.
    epoch: u64,
}

impl<T> ForwarderOutput<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(ForwarderOutputState {
                sender: None,
                epoch: 0,
            }),
            attached: Condvar::new(),
        }
    }

    fn attach(&self, sender: iced_mpsc::Sender<T>) {
        let mut state = self.state.lock();
        state.sender = Some(sender);
        state.epoch += 1;
        drop(state);
        self.attached.notify_all();
    }

    /// Wait through subscription restarts until a sender accepts the event. Teardown may
    /// leave the thread parked.
    fn deliver(&self, mut event: T) {
        loop {
            let (mut sender, epoch) = {
                let mut state = self.state.lock();
                while state.sender.is_none() {
                    self.attached.wait(&mut state);
                }
                let sender = state.sender.clone().expect("sender present after wait");
                (sender, state.epoch)
            };

            // The send blocks outside the lock: backpressure from a full
            // but live channel must not also block a new run's `attach`.
            match crate::util::channel::send_blocking_recoverable(&mut sender, event) {
                Ok(()) => return,
                Err(returned) => {
                    event = returned;
                    let mut state = self.state.lock();
                    while state.epoch == epoch {
                        self.attached.wait(&mut state);
                    }
                }
            }
        }
    }
}

fn attach_stream_run<T: Send + 'static>(
    name: &'static str,
    receiver: &Mutex<Option<mpsc::Receiver<T>>>,
    output: &Arc<ForwarderOutput<T>>,
    sender: iced_mpsc::Sender<T>,
) {
    output.attach(sender);

    if let Some(receiver) = receiver.lock().take() {
        spawn_forwarder_thread(name, receiver, Arc::clone(output));
    } else {
        log::warn!("{name}: subscription stream restarted; reattached to live forwarder");
    }
}

/// This loop never returns. Give it its own thread so it cannot occupy a worker indefinitely.
fn spawn_forwarder_thread<T: Send + 'static>(
    name: &'static str,
    receiver: mpsc::Receiver<T>,
    output: Arc<ForwarderOutput<T>>,
) {
    if let Err(error) = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || forward_to_subscription(&receiver, &output))
    {
        log::error!("failed to start {name} thread, event delivery is disabled: {error}");
    }
}

fn forward_to_subscription<T>(receiver: &mpsc::Receiver<T>, output: &ForwarderOutput<T>) {
    while let Ok(event) = receiver.recv() {
        output.deliver(event);
    }
}

#[derive(Clone)]
pub(super) struct BackendEventStreamFactory {
    id: u64,
    receiver: Arc<Mutex<Option<mpsc::Receiver<BackendRuntimeEvent>>>>,
    output: Arc<ForwarderOutput<BackendRuntimeEvent>>,
}

impl BackendEventStreamFactory {
    pub(super) fn new(receiver: mpsc::Receiver<BackendRuntimeEvent>) -> Self {
        Self {
            id: BACKEND_EVENTS_ID,
            receiver: Arc::new(Mutex::new(Some(receiver))),
            output: Arc::new(ForwarderOutput::new()),
        }
    }

    pub(super) fn subscription(&self) -> Subscription<BackendRuntimeEvent> {
        Subscription::run_with(self.clone(), backend_event_stream)
    }

    /// Only tests take the receiver directly; production claims it in attach_stream_run.
    #[cfg(test)]
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
        attach_stream_run(
            "backend-event-forwarder",
            &factory.receiver,
            &factory.output,
            output,
        );
    })
}
