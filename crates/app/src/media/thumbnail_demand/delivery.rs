//! What the manager hands back to owners: decoded pixels wrapped as GPU-ready
//! handles, the deliveries that carry them, and the messages worker
//! completions arrive on.

use std::{fmt, sync::Arc};

use bytes::Bytes;
use iced::widget::image;
use quick_cache::Weighter;

use crate::{
    bridge::tasks::{RunBlockingError, ScheduleError},
    generation::Generation,
    media::thumbnail_worker::{
        PreparedAnimation, PreparedAnimationFrame, PreparedThumbnail, ThumbnailError, ThumbnailKey,
        ThumbnailMetadata, ThumbnailWorkerOutcome,
    },
};

use super::{DemandId, JobId, Owner, PlaceholderImage, RetryAttempt, RetryId};

/// Why a thumbnail request failed: either the decode/resize pipeline
/// rejected it, or the worker pool couldn't schedule it. Kept transparent so
/// `Display` reproduces the underlying error text verbatim for logging.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ThumbnailDeliveryError {
    #[error(transparent)]
    Thumbnail(#[from] Arc<ThumbnailError>),
    #[error(transparent)]
    Schedule(#[from] Arc<RunBlockingError>),
}

#[derive(Clone, Debug)]
pub enum Message {
    WorkerFinished {
        key: ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
        /// Boxed to keep this variant from setting the size of `RootMessage`,
        /// which every message in the app pays.
        result: Box<Result<ThumbnailWorkerOutcome<PreparedThumbnail>, ThumbnailDeliveryError>>,
    },
    WorkerBackpressured {
        key: ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
    },
    RetryReady {
        key: ThumbnailKey,
        retry_id: RetryId,
    },
    /// Boxed: `Delivery` embeds a `ReadyThumbnail` (~184 bytes) inline, which
    /// would otherwise set the size of `RootMessage`.
    Delivered(Box<Delivery>),
}

#[derive(Clone)]
pub struct ReadyThumbnail {
    key: ThumbnailKey,
    handle: image::Handle,
    metadata: ThumbnailMetadata,
    animation: Option<ReadyAnimation>,
    thumbhash: Option<Arc<[u8]>>,
    byte_len: usize,
}

impl ReadyThumbnail {
    pub fn key(&self) -> &ThumbnailKey {
        &self.key
    }

    pub fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub fn metadata(&self) -> &ThumbnailMetadata {
        &self.metadata
    }

    pub fn animation(&self) -> Option<&ReadyAnimation> {
        self.animation.as_ref()
    }

    pub fn thumbhash(&self) -> Option<&[u8]> {
        self.thumbhash.as_deref()
    }

    #[cfg(test)]
    pub fn for_test(key: ThumbnailKey, metadata: ThumbnailMetadata, rgba: Vec<u8>) -> Self {
        let byte_len = rgba.len();
        let handle = image::Handle::from_rgba(metadata.width, metadata.height, rgba);
        Self {
            key,
            handle,
            metadata,
            animation: None,
            thumbhash: None,
            byte_len,
        }
    }
}

impl fmt::Debug for ReadyThumbnail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyThumbnail")
            .field("key", &self.key)
            .field("metadata", &self.metadata)
            .field(
                "animation_frames",
                &self.animation.as_ref().map(ReadyAnimation::frame_count),
            )
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

#[derive(Clone)]
pub struct ReadyAnimation {
    frames: Vec<ReadyAnimationFrame>,
    byte_len: usize,
}

impl ReadyAnimation {
    pub fn frames(&self) -> &[ReadyAnimationFrame] {
        &self.frames
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl fmt::Debug for ReadyAnimation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyAnimation")
            .field("frame_count", &self.frames.len())
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

#[derive(Clone)]
pub struct ReadyAnimationFrame {
    handle: image::Handle,
    delay: std::time::Duration,
}

impl ReadyAnimationFrame {
    pub fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub const fn delay(&self) -> std::time::Duration {
        self.delay
    }
}

impl fmt::Debug for ReadyAnimationFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyAnimationFrame")
            .field("delay", &self.delay)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Delivery {
    pub owner: Owner,
    pub generation: Generation,
    pub id: DemandId,
    pub key: ThumbnailKey,
    pub result: DeliveryResult,
}

impl Delivery {
    pub(super) fn ready(
        owner: Owner,
        generation: Generation,
        id: DemandId,
        key: ThumbnailKey,
        ready: ReadyThumbnail,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Ready(ready),
        }
    }

    pub(super) fn failed(
        owner: Owner,
        generation: Generation,
        id: DemandId,
        key: ThumbnailKey,
        error: ThumbnailDeliveryError,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Failed { error },
        }
    }

    pub(super) fn placeholder(
        owner: Owner,
        generation: Generation,
        id: DemandId,
        key: ThumbnailKey,
        placeholder: PlaceholderImage,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Placeholder(placeholder),
        }
    }
}

#[derive(Clone, Debug)]
pub enum DeliveryResult {
    Ready(ReadyThumbnail),
    /// A blurred ThumbHash stand-in painted before real pixels arrive. Replaced
    /// by [`DeliveryResult::Ready`] once the decode lands.
    Placeholder(PlaceholderImage),
    Failed {
        error: ThumbnailDeliveryError,
    },
}

#[derive(Clone)]
pub(super) struct ReadyThumbnailWeighter;

impl Weighter<ThumbnailKey, ReadyThumbnail> for ReadyThumbnailWeighter {
    fn weight(&self, _key: &ThumbnailKey, value: &ReadyThumbnail) -> u64 {
        value.byte_len as u64
    }
}

pub(super) fn ready_thumbnail(key: ThumbnailKey, thumbnail: &PreparedThumbnail) -> ReadyThumbnail {
    let metadata = thumbnail.thumbnail().metadata().clone();
    let rgba_len = thumbnail.thumbnail().rgba_bytes().len();
    let animation = thumbnail.animation().map(ready_animation);
    let byte_len = rgba_len + animation.as_ref().map_or(0, |animation| animation.byte_len);
    let handle = image::Handle::from_rgba(
        metadata.width,
        metadata.height,
        Bytes::from_owner(thumbnail.thumbnail().rgba_arc()),
    );
    ReadyThumbnail {
        key,
        handle,
        metadata,
        animation,
        thumbhash: thumbnail.thumbnail().thumbhash_arc(),
        byte_len,
    }
}

fn ready_animation(animation: &PreparedAnimation) -> ReadyAnimation {
    let mut byte_len = 0_usize;
    let frames = animation
        .frames()
        .iter()
        .map(|frame| {
            byte_len = byte_len.saturating_add(frame.rgba_bytes().len());
            ready_animation_frame(frame)
        })
        .collect();

    ReadyAnimation { frames, byte_len }
}

fn ready_animation_frame(frame: &PreparedAnimationFrame) -> ReadyAnimationFrame {
    ReadyAnimationFrame {
        handle: image::Handle::from_rgba(
            frame.width(),
            frame.height(),
            Bytes::from_owner(frame.rgba_arc()),
        ),
        delay: frame.delay(),
    }
}

pub(super) fn worker_result_message(
    key: ThumbnailKey,
    job_id: JobId,
    attempt: RetryAttempt,
    result: Result<
        Result<ThumbnailWorkerOutcome<PreparedThumbnail>, ThumbnailError>,
        RunBlockingError,
    >,
) -> Message {
    match result {
        Err(RunBlockingError::Schedule(ScheduleError::QueueFull { .. })) => {
            Message::WorkerBackpressured {
                key,
                job_id,
                attempt,
            }
        }
        Err(error) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Err(ThumbnailDeliveryError::Schedule(Arc::new(error)))),
        },
        Ok(Err(error)) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Err(ThumbnailDeliveryError::Thumbnail(Arc::new(error)))),
        },
        Ok(Ok(outcome)) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Ok(outcome)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_thumbnail_creates_animation_frame_handles_once() {
        let dir = crate::test_support::TestDir::new("gmpublished-ready-animation");
        let gif = dir.gif("animated.gif", 8, 8);
        let thumbnail = crate::media::thumbnail_worker::ThumbnailDecoder::new()
            .decode_and_resize_file(gif, 64)
            .expect("animated GIF thumbnail should decode");
        let ready = ready_thumbnail(
            ThumbnailKey::for_bytes("animated", 64),
            &PreparedThumbnail::from_thumbnail(thumbnail),
        );

        let animation = ready
            .animation()
            .expect("animated GIF should prepare ready frames");

        assert_eq!(animation.frame_count(), 2);
        assert!(animation.frames().iter().all(|frame| {
            frame.delay() > std::time::Duration::ZERO && frame.handle().id() != ready.handle().id()
        }));
    }
}
