use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use iced::futures::{
    SinkExt, StreamExt,
    channel::mpsc::{self, Sender},
};
use iced::{Subscription, stream};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};

use crate::backend::gma::is_gma_path;

const QUIET_WINDOW: Duration = Duration::from_secs(1);
/// Sustained disk churn (e.g. Steam mass-updating workshop items) never
/// produces a quiet window, so cap how long a burst can defer the refresh:
/// the library catches up live during the storm instead of freezing on
/// whatever the launch scan saw.
const STORM_MAX_LATENCY: Duration = Duration::from_secs(5);
const WATCH_EVENT_QUEUE: usize = 100;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WatchSubscriptionKey {
    roots: WatchRoots,
    arm_epoch: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WatchRoots {
    addons: PathBuf,
    cache: PathBuf,
    workshop_content: PathBuf,
}

impl WatchRoots {
    pub(crate) fn from_gmod_dir(gmod_dir: &Path) -> Option<Self> {
        let steamapps = gmod_dir.parent()?.parent()?;
        Some(Self {
            addons: gmod_dir.join("GarrysMod/addons"),
            cache: gmod_dir.join("GarrysMod/cache/workshop"),
            workshop_content: steamapps.join("workshop/content/4000"),
        })
    }

    fn iter(&self) -> impl Iterator<Item = (&Path, RecursiveMode)> {
        [
            (self.addons.as_path(), RecursiveMode::NonRecursive),
            (self.cache.as_path(), RecursiveMode::NonRecursive),
            (self.workshop_content.as_path(), RecursiveMode::Recursive),
        ]
        .into_iter()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    DiskChanged,
    WatchArmed { degraded: bool },
}

pub fn subscription(gmod_dir: Option<&Path>, arm_epoch: u64) -> Subscription<Message> {
    let Some(roots) = gmod_dir.and_then(WatchRoots::from_gmod_dir) else {
        return Subscription::none();
    };

    Subscription::run_with(
        WatchSubscriptionKey { roots, arm_epoch },
        library_watch_stream,
    )
}

fn library_watch_stream(
    key: &WatchSubscriptionKey,
) -> impl iced::futures::Stream<Item = Message> + use<> {
    let key = key.clone();
    stream::channel(10, async move |mut output| {
        let (event_sender, event_receiver) = mpsc::channel(WATCH_EVENT_QUEUE);
        let mut watcher = match watcher_for_roots(&key.roots, event_sender) {
            Ok(watcher) => watcher,
            Err(error) => {
                log::debug!("failed to create installed addon library watcher: {error}");
                let _ = output.send(Message::WatchArmed { degraded: true }).await;
                return;
            }
        };

        let degraded = arm_roots(&mut watcher, &key.roots);
        if output.send(Message::WatchArmed { degraded }).await.is_err() {
            return;
        }

        debounce_disk_events(event_receiver, output, QUIET_WINDOW, STORM_MAX_LATENCY).await;
        drop(watcher);
    })
}

fn watcher_for_roots(
    roots: &WatchRoots,
    mut event_sender: Sender<()>,
) -> notify::Result<RecommendedWatcher> {
    let workshop_content = roots.workshop_content.clone();
    RecommendedWatcher::new(
        move |result| match result {
            Ok(event) if should_forward_event(&event, &workshop_content) => {
                let _ = event_sender.try_send(());
            }
            Ok(_) => {}
            Err(error) => {
                log::debug!("installed addon library watch event error: {error}");
            }
        },
        Config::default(),
    )
}

fn arm_roots(watcher: &mut RecommendedWatcher, roots: &WatchRoots) -> bool {
    let mut degraded = false;
    for (path, mode) in roots.iter() {
        if let Err(error) = watcher.watch(path, mode) {
            log::debug!(
                "failed to arm installed addon library watcher for {} ({mode:?}): {error}",
                path.display()
            );
            degraded = true;
        }
    }
    degraded
}

async fn debounce_disk_events(
    mut receiver: mpsc::Receiver<()>,
    mut output: Sender<Message>,
    quiet_window: Duration,
    max_latency: Duration,
) {
    loop {
        if receiver.next().await.is_none() {
            return;
        }

        let burst_started = std::time::Instant::now();
        loop {
            match tokio::time::timeout(quiet_window, receiver.next()).await {
                Ok(Some(())) if burst_started.elapsed() < max_latency => {}
                Ok(Some(())) | Err(_) => break,
                Ok(None) => return,
            }
        }

        if output.send(Message::DiskChanged).await.is_err() {
            return;
        }
    }
}

pub fn should_forward_event(event: &Event, workshop_content: &Path) -> bool {
    if matches!(
        event.kind,
        EventKind::Access(_)
            | EventKind::Modify(ModifyKind::Metadata(_) | ModifyKind::Other)
            | EventKind::Other
    ) {
        return false;
    }

    if event.paths.iter().any(|path| is_gma_path(path)) {
        return true;
    }

    is_workshop_dir_event_kind(event.kind)
        && event
            .paths
            .iter()
            .any(|path| is_direct_workshop_content_child(path, workshop_content))
}

fn is_workshop_dir_event_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(CreateKind::Any | CreateKind::Folder)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn is_direct_workshop_content_child(path: &Path, workshop_content: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(workshop_content) else {
        return false;
    };
    let mut components = relative.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;
    use notify::event::{AccessKind, DataChange, MetadataKind, RenameMode};

    fn event(kind: EventKind, paths: &[&str]) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn filter_forwards_gma_paths_for_mutating_events() {
        let root = Path::new("/steamapps/workshop/content/4000");
        let event = event(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &["/steamapps/common/GarrysMod/garrysmod/addons/a.GMA"],
        );

        assert!(should_forward_event(&event, root));
    }

    #[test]
    fn filter_drops_access_metadata_other_and_non_gma_noise() {
        let root = Path::new("/steamapps/workshop/content/4000");
        let access = event(
            EventKind::Access(AccessKind::Read),
            &["/steamapps/common/GarrysMod/garrysmod/addons/a.gma"],
        );
        let metadata = event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
            &["/steamapps/common/GarrysMod/garrysmod/addons/a.gma"],
        );
        let other = event(
            EventKind::Other,
            &["/steamapps/common/GarrysMod/garrysmod/addons/a.gma"],
        );
        let temp = event(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &["/steamapps/workshop/content/4000/123/addon.patch"],
        );

        assert!(!should_forward_event(&access, root));
        assert!(!should_forward_event(&metadata, root));
        assert!(!should_forward_event(&other, root));
        assert!(!should_forward_event(&temp, root));
    }

    #[test]
    fn filter_forwards_direct_workshop_folder_create_remove_and_rename() {
        let root = Path::new("/steamapps/workshop/content/4000");
        let create = event(
            EventKind::Create(CreateKind::Folder),
            &["/steamapps/workshop/content/4000/123"],
        );
        let remove = event(
            EventKind::Remove(RemoveKind::Any),
            &["/steamapps/workshop/content/4000/456"],
        );
        let rename = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[
                "/steamapps/workshop/content/4000/789.tmp",
                "/steamapps/workshop/content/4000/789",
            ],
        );

        assert!(should_forward_event(&create, root));
        assert!(should_forward_event(&remove, root));
        assert!(should_forward_event(&rename, root));
    }

    #[test]
    fn filter_drops_nested_or_file_workshop_directory_noise() {
        let root = Path::new("/steamapps/workshop/content/4000");
        let nested = event(
            EventKind::Create(CreateKind::Folder),
            &["/steamapps/workshop/content/4000/123/nested"],
        );
        let file = event(
            EventKind::Create(CreateKind::File),
            &["/steamapps/workshop/content/4000/123"],
        );

        assert!(!should_forward_event(&nested, root));
        assert!(!should_forward_event(&file, root));
    }

    #[test]
    fn debounce_coalesces_bursts_and_exits_when_channel_closes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (mut input, receiver) = mpsc::channel(10);
            let (output, mut messages) = mpsc::channel(10);
            let task = tokio::spawn(debounce_disk_events(
                receiver,
                output,
                Duration::from_millis(20),
                Duration::from_secs(5),
            ));

            for _ in 0..5 {
                input.try_send(()).expect("send event");
            }

            let message = tokio::time::timeout(Duration::from_secs(1), messages.next())
                .await
                .expect("debounced message")
                .expect("message");
            assert_eq!(message, Message::DiskChanged);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), messages.next())
                    .await
                    .is_err(),
                "burst should emit exactly one message"
            );

            drop(input);
            task.await.expect("debounce task");
            assert_eq!(messages.next().await, None);
        });
    }

    #[test]
    fn sustained_storm_still_emits_within_max_latency() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let (mut input, receiver) = mpsc::channel(10);
            let (output, mut messages) = mpsc::channel(10);
            let _task = tokio::spawn(debounce_disk_events(
                receiver,
                output,
                Duration::from_millis(50),
                Duration::from_millis(100),
            ));

            let feeder = tokio::spawn(async move {
                // Events every 10ms never open a 50ms quiet window; only the
                // max-latency cap can fire during the storm.
                for _ in 0..40 {
                    let _ = input.try_send(());
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                input
            });

            let message = tokio::time::timeout(Duration::from_millis(300), messages.next())
                .await
                .expect("storm should emit within the max-latency cap")
                .expect("message");
            assert_eq!(message, Message::DiskChanged);
            drop(feeder.await.expect("feeder"));
        });
    }
}
