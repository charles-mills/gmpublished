use parking_lot::{Condvar, Mutex};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("preview preparation cancelled")]
pub struct Cancelled;

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    wakes: Mutex<Vec<Weak<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone, Default)]
pub struct PreparationCancellation(Arc<CancellationState>);

impl std::fmt::Debug for PreparationCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparationCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
impl PartialEq for PreparationCancellation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PreparationCancellation {}

pub struct WakeRegistration {
    _wake: Arc<dyn Fn() + Send + Sync>,
}
impl PreparationCancellation {
    pub(crate) fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let wakes: Vec<_> = {
            let mut wakes = self.0.wakes.lock();
            let active = wakes.iter().filter_map(Weak::upgrade).collect();
            wakes.clear();
            active
        };
        for wake in wakes {
            wake();
        }
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
    pub(crate) fn register(&self, wake: impl Fn() + Send + Sync + 'static) -> WakeRegistration {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        {
            let mut wakes = self.0.wakes.lock();
            wakes.retain(|wake| wake.strong_count() != 0);
            wakes.push(Arc::downgrade(&wake));
        }
        if self.is_cancelled() {
            wake();
        }
        WakeRegistration { _wake: wake }
    }
}

struct FlightInner<T> {
    value: std::sync::OnceLock<T>,
    loading: Mutex<bool>,
    changed: Condvar,
}
pub struct FlightCell<T>(Arc<FlightInner<T>>);
impl<T> Default for FlightCell<T> {
    fn default() -> Self {
        Self(Arc::new(FlightInner {
            value: std::sync::OnceLock::new(),
            loading: Mutex::new(false),
            changed: Condvar::new(),
        }))
    }
}
impl<T> std::fmt::Debug for FlightCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlightCell")
            .field("ready", &self.0.value.get().is_some())
            .field("loading", &*self.0.loading.lock())
            .finish()
    }
}
struct Initializer<T>(Arc<FlightInner<T>>);
impl<T> Drop for Initializer<T> {
    fn drop(&mut self) {
        let mut loading = self.0.loading.lock();
        *loading = false;
        drop(loading);
        self.0.changed.notify_all();
    }
}
impl<T: Send + Sync + 'static> FlightCell<T> {
    pub(crate) fn get_ref(&self) -> Option<&T> {
        self.0.value.get()
    }
    pub(crate) fn get_or_try_init_ref(
        &self,
        cancellation: &PreparationCancellation,
        init: impl FnOnce() -> Result<T, Cancelled>,
    ) -> Result<&T, Cancelled> {
        cancellation.check()?;
        let inner = Arc::clone(&self.0);
        let registration = cancellation.register(move || {
            let loading = inner.loading.lock();
            inner.changed.notify_all();
            drop(loading);
        });
        let mut loading = self.0.loading.lock();
        loop {
            cancellation.check()?;
            if let Some(value) = self.0.value.get() {
                return Ok(value);
            }
            if !*loading {
                *loading = true;
                break;
            }
            self.0.changed.wait(&mut loading);
        }
        drop(loading);
        drop(registration);
        let _initializer = Initializer(Arc::clone(&self.0));
        let value = init()?;
        cancellation.check()?;
        assert!(
            self.0.value.set(value).is_ok(),
            "exclusive flight initializer"
        );
        Ok(self.0.value.get().expect("initialized flight"))
    }
}
impl<T: Clone + Send + Sync + 'static> FlightCell<T> {
    pub(crate) fn get(&self) -> Option<T> {
        self.get_ref().cloned()
    }
    pub(crate) fn get_or_try_init(
        &self,
        cancellation: &PreparationCancellation,
        init: impl FnOnce() -> Result<T, Cancelled>,
    ) -> Result<T, Cancelled> {
        self.get_or_try_init_ref(cancellation, init).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};
    #[test]
    fn cloned_lifetimes_only_cancel_explicitly() {
        let token = PreparationCancellation::default();
        let clone = token.clone();
        drop(clone);
        assert!(token.check().is_ok());
        token.cancel();
        assert_eq!(token.check(), Err(Cancelled));
    }
    #[test]
    fn cancelled_initializer_does_not_poison_another_caller() {
        let cell = FlightCell::<usize>::default();
        let token = PreparationCancellation::default();
        assert_eq!(
            cell.get_or_try_init(&token, || {
                token.cancel();
                Ok(7)
            }),
            Err(Cancelled)
        );
        assert!(cell.get().is_none());
        assert_eq!(
            cell.get_or_try_init(&PreparationCancellation::default(), || Ok(9)),
            Ok(9)
        );
    }
    #[test]
    fn unwind_releases_initializer_and_preserves_retry() {
        let cell = FlightCell::<usize>::default();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell
                .get_or_try_init(&PreparationCancellation::default(), || panic!(
                    "initialization"
                ))))
            .is_err()
        );
        assert_eq!(
            cell.get_or_try_init(&PreparationCancellation::default(), || Ok(9)),
            Ok(9)
        );
    }
    #[test]
    fn cancelled_waiter_leaves_live_initializer_and_cache_intact() {
        let cell = Arc::new(FlightCell::<usize>::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner = Arc::clone(&cell);
        let worker = thread::spawn(move || {
            owner.get_or_try_init(&PreparationCancellation::default(), || {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(42)
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let token = PreparationCancellation::default();
        let waiter_token = token.clone();
        let waiter_cell = Arc::clone(&cell);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            done_tx
                .send(waiter_cell.get_or_try_init(&waiter_token, || panic!("second initializer")))
                .unwrap();
        });
        token.cancel();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(Cancelled)
        );
        finish_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Ok(42));
        waiter.join().unwrap();
        assert_eq!(cell.get(), Some(42));
    }
}
