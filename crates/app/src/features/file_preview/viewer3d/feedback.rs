use super::pipeline::texture_upload::{RETIREMENT_TIMEOUT, RetirementSubscription};
use crate::bridge::materials::PageSubscription;
use crate::generation::Generation;
use iced::{Subscription, futures};
use std::{
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::watch;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RenderStatus {
    #[default]
    Pending,
    Ready {
        unavailable_images: usize,
    },
    Retiring {
        unavailable_images: usize,
    },
    Rejected {
        reason: String,
    },
    RetirementTimedOut,
}

#[derive(Debug)]
pub struct RenderFeedback {
    status: Mutex<RenderStatus>,
    retirement: Mutex<Option<RetirementSubscription>>,
    pages: Mutex<Option<PageSubscription>>,
    changes: watch::Sender<u64>,
}
impl Default for RenderFeedback {
    fn default() -> Self {
        Self {
            status: Mutex::new(RenderStatus::default()),
            retirement: Mutex::new(None),
            pages: Mutex::new(None),
            changes: watch::channel(0).0,
        }
    }
}
impl RenderFeedback {
    fn notify(&self) {
        self.changes
            .send_modify(|version| *version = version.wrapping_add(1));
    }
    pub fn set(&self, status: RenderStatus) {
        let mut current = self.status.lock().expect("render feedback lock");
        if *current == status {
            return;
        }
        *current = status;
        drop(current);
        self.notify();
    }
    pub fn snapshot(&self) -> RenderStatus {
        self.status.lock().expect("render feedback lock").clone()
    }
    pub(super) fn bind_retirement(&self, subscription: Option<RetirementSubscription>) {
        let mut current = self.retirement.lock().expect("retirement feedback lock");
        if current.as_ref().map(|binding| binding.id)
            == subscription.as_ref().map(|binding| binding.id)
        {
            return;
        }
        *current = subscription;
        drop(current);
        self.notify();
    }
    pub(super) fn bind_pages(&self, subscription: Option<PageSubscription>) {
        let mut current = self.pages.lock().expect("page feedback lock");
        if current.as_ref().map(|binding| binding.id)
            == subscription.as_ref().map(|binding| binding.id)
        {
            return;
        }
        *current = subscription;
        drop(current);
        self.notify();
    }
    fn page_subscription(&self) -> Option<PageSubscription> {
        self.pages.lock().expect("page feedback lock").clone()
    }
    fn retirement_subscription(&self) -> Option<RetirementSubscription> {
        self.retirement
            .lock()
            .expect("retirement feedback lock")
            .clone()
    }
    pub(super) fn retirement_expired(&self, now: Instant) -> bool {
        self.retirement_subscription().is_some_and(|binding| {
            binding
                .receiver
                .borrow()
                .started
                .is_some_and(|started| now.saturating_duration_since(started) >= RETIREMENT_TIMEOUT)
        })
    }
    pub(super) fn reject_expired_retirement(&self, now: Instant) -> bool {
        if !self.retirement_expired(now) {
            return false;
        }
        self.set(RenderStatus::RetirementTimedOut);
        self.bind_retirement(None);
        true
    }

    pub(super) fn wants_redraw(&self, now: Instant) -> bool {
        if self.reject_expired_retirement(now) {
            return false;
        }
        self.retirement_subscription()
            .is_some_and(|binding| binding.receiver.borrow().pending != 0)
    }
}

#[derive(Clone, Debug)]
struct FeedbackKey {
    feedback: Arc<RenderFeedback>,
    generation: Generation,
}
impl PartialEq for FeedbackKey {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.feedback, &other.feedback)
    }
}
impl Eq for FeedbackKey {}
impl Hash for FeedbackKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.generation.hash(state);
        Arc::as_ptr(&self.feedback).hash(state);
    }
}

pub(in crate::features::file_preview) fn subscription(
    feedback: Arc<RenderFeedback>,
    generation: Generation,
) -> Subscription<super::super::Message> {
    Subscription::run_with(
        FeedbackKey {
            feedback,
            generation,
        },
        feedback_stream,
    )
}
async fn optional_change<T>(receiver: Option<&mut watch::Receiver<T>>) -> bool {
    match receiver {
        Some(receiver) => receiver.changed().await.is_err(),
        None => futures::future::pending().await,
    }
}

fn feedback_stream(
    key: &FeedbackKey,
) -> impl futures::Stream<Item = super::super::Message> + use<> {
    let changes = key.feedback.changes.subscribe();
    futures::stream::unfold(
        (
            key.clone(),
            changes,
            None::<RetirementSubscription>,
            None::<PageSubscription>,
            true,
        ),
        |(key, mut changes, mut retirement, mut pages, initial)| async move {
            let message = super::super::Message::RenderFeedbackChanged(key.generation);
            if initial {
                return Some((message, (key, changes, retirement, pages, false)));
            }
            let binding = key.feedback.retirement_subscription();
            let page_binding = key.feedback.page_subscription();
            if retirement.as_ref().map(|value| value.id) != binding.as_ref().map(|value| value.id)
                || pages.as_ref().map(|value| value.id)
                    != page_binding.as_ref().map(|value| value.id)
            {
                return Some((message, (key, changes, binding, page_binding, false)));
            }
            let (closed, retirement_closed, pages_closed) = match futures::future::select(
                Box::pin(changes.changed()),
                Box::pin(futures::future::select(
                    Box::pin(optional_change(
                        retirement.as_mut().map(|binding| &mut binding.receiver),
                    )),
                    Box::pin(optional_change(
                        pages.as_mut().map(|binding| &mut binding.receiver),
                    )),
                )),
            )
            .await
            {
                futures::future::Either::Left((result, _)) => (result.is_err(), false, false),
                futures::future::Either::Right((futures::future::Either::Left((closed, _)), _)) => {
                    (false, closed, false)
                }
                futures::future::Either::Right((
                    futures::future::Either::Right((closed, _)),
                    _,
                )) => (false, false, closed),
            };
            if closed {
                return None;
            }
            if retirement_closed {
                retirement = None;
                key.feedback.bind_retirement(None);
            }
            if pages_closed {
                pages = None;
                key.feedback.bind_pages(None);
            }
            Some((message, (key, changes, retirement, pages, false)))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::pipeline::texture_upload::RetirementSignal;
    use super::*;
    use futures::{FutureExt, StreamExt};
    #[test]
    fn retirement_notifies_idle_feedback_without_polling_settled_capacity() {
        let signal = RetirementSignal::default();
        let feedback = Arc::new(RenderFeedback::default());
        feedback.bind_retirement(Some(signal.subscribe()));
        let key = FeedbackKey {
            feedback: Arc::clone(&feedback),
            generation: Generation::INITIAL,
        };
        let mut stream = Box::pin(feedback_stream(&key));
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        assert!(!feedback.wants_redraw(Instant::now()));
        signal.begin();
        assert_eq!(
            stream.next().now_or_never(),
            Some(Some(super::super::super::Message::RenderFeedbackChanged(
                Generation::INITIAL
            )))
        );
        assert!(feedback.wants_redraw(Instant::now()));
        signal.complete();
        assert!(!feedback.wants_redraw(Instant::now()));
        assert_eq!(signal.state().epoch, 1);
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
    }
    #[test]
    fn feedback_changes_before_first_poll_and_between_yields_are_not_consumed() {
        let feedback = Arc::new(RenderFeedback::default());
        let signal = RetirementSignal::default();
        let key = FeedbackKey {
            feedback: Arc::clone(&feedback),
            generation: Generation::INITIAL,
        };
        let mut stream = Box::pin(feedback_stream(&key));
        feedback.set(RenderStatus::Ready {
            unavailable_images: 1,
        });
        feedback.bind_retirement(Some(signal.subscribe()));
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        feedback.set(RenderStatus::Ready {
            unavailable_images: 2,
        });
        assert!(stream.next().now_or_never().is_some());
        signal.begin();
        signal.complete();
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        assert_eq!(
            feedback.snapshot(),
            RenderStatus::Ready {
                unavailable_images: 2
            }
        );
    }

    #[test]
    fn stalled_gpu_retirement_stops_redraws_at_the_completion_deadline() {
        let signal = RetirementSignal::default();
        let feedback = RenderFeedback::default();
        feedback.bind_retirement(Some(signal.subscribe()));
        signal.begin();
        let started = signal.state().started.unwrap();
        assert!(feedback.wants_redraw(started));
        assert!(!feedback.wants_redraw(started + RETIREMENT_TIMEOUT));
        assert_eq!(feedback.snapshot(), RenderStatus::RetirementTimedOut);
        signal.complete();
        assert!(!feedback.wants_redraw(Instant::now()));
    }

    #[test]
    fn page_completion_wakes_static_pending_preview_without_redraw_polling() {
        let (sender, receiver) = watch::channel(0);
        let feedback = Arc::new(RenderFeedback::default());
        feedback.bind_pages(Some(PageSubscription { id: 41, receiver }));
        let key = FeedbackKey {
            feedback: Arc::clone(&feedback),
            generation: Generation::INITIAL,
        };
        let mut stream = Box::pin(feedback_stream(&key));
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        assert!(!feedback.wants_redraw(Instant::now()));
        sender.send_modify(|revision| *revision += 1);
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        assert_eq!(feedback.snapshot(), RenderStatus::Pending);
    }

    #[test]
    fn detached_or_closed_page_source_does_not_keep_feedback_waking() {
        let (sender, receiver) = watch::channel(0);
        let feedback = Arc::new(RenderFeedback::default());
        feedback.bind_pages(Some(PageSubscription { id: 42, receiver }));
        let key = FeedbackKey {
            feedback: Arc::clone(&feedback),
            generation: Generation::INITIAL,
        };
        let mut stream = Box::pin(feedback_stream(&key));
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_some());
        assert!(stream.next().now_or_never().is_none());
        feedback.bind_pages(None);
        while stream.next().now_or_never().is_some() {}
        sender.send_modify(|revision| *revision += 1);
        assert!(stream.next().now_or_never().is_none());
        let (closed_sender, receiver) = watch::channel(0);
        feedback.bind_pages(Some(PageSubscription { id: 43, receiver }));
        while stream.next().now_or_never().is_some() {}
        drop(closed_sender);
        while stream.next().now_or_never().is_some() {}
        assert!(feedback.page_subscription().is_none());
        assert!(stream.next().now_or_never().is_none());
        assert!(!feedback.wants_redraw(Instant::now()));
    }
}
