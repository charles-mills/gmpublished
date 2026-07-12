use std::time::Duration;

use iced::widget::image;

use crate::media::thumbnail_demand::ReadyThumbnail;

pub const ANIMATION_TICK_INTERVAL: Duration = Duration::from_millis(16);

const MIN_FRAME_DELAY: Duration = Duration::from_millis(1);

/// Playback policy chosen by each animated thumbnail use site.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayPolicy {
    /// Play only while the visible item is actively hovered, unless the user setting opts in.
    OnHover,
}

impl PlayPolicy {
    pub(crate) const fn should_play(
        self,
        visible: bool,
        play_requested: bool,
        play_gifs_by_default: bool,
    ) -> bool {
        match self {
            Self::OnHover => visible && (play_requested || play_gifs_by_default),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playback {
    frames: Vec<Frame>,
    current_frame: usize,
    elapsed_in_frame: Duration,
}

impl Playback {
    pub(crate) fn from_frame_handles(
        frames: impl IntoIterator<Item = (image::Handle, Duration)>,
    ) -> Option<Self> {
        let frames = frames
            .into_iter()
            .map(|(handle, delay)| Frame {
                handle,
                delay: delay.max(MIN_FRAME_DELAY),
            })
            .collect::<Vec<_>>();
        if frames.len() <= 1 {
            return None;
        }

        Some(Self {
            frames,
            current_frame: 0,
            elapsed_in_frame: Duration::ZERO,
        })
    }

    pub(crate) fn from_ready(ready: &ReadyThumbnail) -> Option<Self> {
        let animation = ready.animation()?;
        if animation.frame_count() <= 1 {
            return None;
        }

        let frames = animation
            .frames()
            .iter()
            .map(|frame| Frame {
                handle: frame.handle().clone(),
                delay: frame.delay().max(MIN_FRAME_DELAY),
            })
            .collect::<Vec<_>>();
        if frames.len() <= 1 {
            return None;
        }

        Some(Self {
            frames,
            current_frame: 0,
            elapsed_in_frame: Duration::ZERO,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            frames: vec![
                Frame {
                    handle: image::Handle::from_rgba(1, 1, vec![b'a', 0, 0, 255]),
                    delay: Duration::from_millis(30),
                },
                Frame {
                    handle: image::Handle::from_rgba(1, 1, vec![b'b', 0, 0, 255]),
                    delay: Duration::from_millis(90),
                },
            ],
            current_frame: 0,
            elapsed_in_frame: Duration::ZERO,
        }
    }

    pub(crate) fn current_handle(&self) -> &image::Handle {
        &self.frames[self.current_frame].handle
    }

    pub(crate) fn advance(&mut self, elapsed: Duration) -> bool {
        if self.frames.len() <= 1 {
            return false;
        }

        self.elapsed_in_frame = self.elapsed_in_frame.saturating_add(elapsed);
        let mut changed = false;
        while self.elapsed_in_frame >= self.frames[self.current_frame].delay {
            self.elapsed_in_frame = self
                .elapsed_in_frame
                .saturating_sub(self.frames[self.current_frame].delay);
            self.current_frame = (self.current_frame + 1) % self.frames.len();
            changed = true;
        }
        changed
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Frame {
    handle: image::Handle,
    delay: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_policy_respects_visibility_hover_and_user_default() {
        assert!(!PlayPolicy::OnHover.should_play(true, false, false));
        assert!(PlayPolicy::OnHover.should_play(true, true, false));
        assert!(PlayPolicy::OnHover.should_play(true, false, true));
        assert!(!PlayPolicy::OnHover.should_play(false, true, true));
    }

    #[test]
    fn playback_advances_by_frame_delay_and_loops() {
        let mut playback = Playback::for_test();
        let first = playback.current_handle().clone();

        assert!(!playback.advance(Duration::from_millis(29)));
        assert_eq!(playback.current_handle(), &first);
        assert!(playback.advance(Duration::from_millis(1)));
        assert_ne!(playback.current_handle(), &first);
        assert!(playback.advance(Duration::from_millis(90)));
        assert_eq!(playback.current_handle(), &first);
    }

    #[test]
    fn playback_can_be_built_from_cached_frame_handles() {
        let playback = Playback::from_frame_handles([
            (
                image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]),
                Duration::from_millis(0),
            ),
            (
                image::Handle::from_rgba(1, 1, vec![0, 255, 0, 255]),
                Duration::from_millis(30),
            ),
        ])
        .expect("two cached handles should produce playback");

        assert_eq!(playback.frames[0].delay, MIN_FRAME_DELAY);
        assert_eq!(playback.frames.len(), 2);
    }
}
