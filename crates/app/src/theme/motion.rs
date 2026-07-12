use std::time::{Duration, Instant};

use iced::animation::{Easing, Float, Interpolable};
use iced::{Animation, Color, Subscription, time};

use super::tokens::{Motion, Rgba};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

impl Motion {
    pub(crate) fn fast_duration(self) -> Duration {
        duration_ms(self.fast_ms)
    }

    pub(crate) fn hover_in_duration(self) -> Duration {
        duration_ms(self.hover_in_ms)
    }

    pub(crate) fn hover_out_duration(self) -> Duration {
        duration_ms(self.hover_out_ms)
    }

    pub(crate) fn modal_enter_duration(self) -> Duration {
        duration_ms(self.modal_enter_ms)
    }

    pub(crate) fn modal_exit_duration(self) -> Duration {
        duration_ms(self.modal_exit_ms)
    }

    pub(crate) fn context_menu_enter_duration(self) -> Duration {
        duration_ms(self.context_menu_enter_ms)
    }

    pub(crate) fn context_menu_exit_duration(self) -> Duration {
        duration_ms(self.context_menu_exit_ms)
    }

    pub(crate) fn thumb_reveal_duration(self) -> Duration {
        duration_ms(self.thumb_reveal_ms)
    }

    pub(crate) fn overlay_toast_duration(self) -> Duration {
        duration_ms(self.overlay_toast_ms)
    }
}

/// Animation presence that keeps redraw clocks alive from stored state.
///
/// Invariant: a redraw-subscription gate must be a pure function of stored
/// state (`needs_ticks`), never of the wall clock. `settled` may flip only
/// inside a tick handler. This guarantees the clock outlives every animation
/// by exactly one delivered tick: the tick that both finalizes dependent state
/// and lets the gate close. It prevents the race where iced rebuilds
/// subscriptions between animation end and the next tick, such as after a
/// cursor-move message, and tears down the clock before finalization runs.
#[derive(Clone, Debug)]
pub(crate) struct Presence<T>
where
    T: Clone + Copy + PartialEq + Float,
{
    animation: Animation<T>,
    enter: Duration,
    exit: Duration,
    easing: Easing,
    settled: bool,
}

impl<T> Presence<T>
where
    T: Clone + Copy + PartialEq + Float,
{
    pub(crate) fn new(initial: T, duration: Duration, easing: Easing) -> Self {
        Self::new_asymmetric(initial, duration, duration, easing)
    }

    pub(crate) fn new_asymmetric(
        initial: T,
        enter: Duration,
        exit: Duration,
        easing: Easing,
    ) -> Self {
        Self {
            animation: Animation::new(initial).duration(enter).easing(easing),
            enter,
            exit,
            easing,
            settled: true,
        }
    }

    pub(crate) fn go(&mut self, target: T, now: Instant) {
        if self.value() == target {
            return;
        }

        // A settled animation adopts the direction's duration by rebuilding
        // in place (iced bakes duration in at construction). A mid-flight
        // retarget keeps the current animation, since rebuilding would drop
        // the in-progress interpolation and visibly snap.
        if self.settled {
            let duration = if target.float_value() > self.value().float_value() {
                self.enter
            } else {
                self.exit
            };
            self.animation = Animation::new(self.value())
                .duration(duration)
                .easing(self.easing);
        }

        self.animation.go_mut(target, now);
        self.settled = false;
    }

    pub(crate) fn snap(&mut self, target: T) {
        // Duration is arbitrary here: the next `go` rebuilds from settled.
        self.animation = Animation::new(target)
            .duration(self.enter)
            .easing(self.easing);
        self.settled = true;
    }

    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        if !self.settled && !self.animation.is_animating(now) {
            self.settled = true;
            true
        } else {
            false
        }
    }

    pub(crate) const fn needs_ticks(&self) -> bool {
        !self.settled
    }

    pub(crate) fn value(&self) -> T {
        self.animation.value()
    }

    /// The eased in-flight value, tracking retargets mid-animation.
    pub(crate) fn current(&self, now: Instant) -> f32 {
        self.animation
            .interpolate_with(|value| value.float_value(), now)
    }

    #[cfg(test)]
    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.animation.is_animating(now)
    }
}

impl<T> PartialEq for Presence<T>
where
    T: Clone + Copy + PartialEq + Float,
{
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value() && self.settled == other.settled
    }
}

/// `Eq` holds whenever the animated value itself is `Eq` (e.g. `bool`),
/// since equality above never inspects the `f32`-valued easing internals.
impl<T> Eq for Presence<T> where T: Clone + Copy + Eq + Float {}

impl Presence<bool> {
    pub(crate) fn interpolate<I>(&self, start: I, end: I, at: Instant) -> I
    where
        I: Interpolable + Clone,
    {
        self.animation.interpolate(start, end, at)
    }
}

pub(crate) fn boolean(initial: bool, duration: Duration, easing: Easing) -> Presence<bool> {
    Presence::new(initial, duration, easing)
}

pub(crate) fn asymmetric(
    initial: bool,
    enter: Duration,
    exit: Duration,
    easing: Easing,
) -> Presence<bool> {
    Presence::new_asymmetric(initial, enter, exit, easing)
}

/// Resting scale for closed popovers and menus, paired with an opacity fade.
pub(crate) const POPOVER_CLOSED_SCALE: f32 = 0.98;

/// The shared enter/exit curve, upstream's cubic-bezier(0.16, 1, 0.3, 1):
/// heavily front-loaded, so entrances read as a snap that settles.
pub(crate) fn expo_ease() -> Easing {
    Easing::Custom(expo_ease_curve)
}

fn expo_ease_curve(x: f32) -> f32 {
    cubic_bezier(x, 0.16, 1.0, 0.3, 1.0)
}

fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x == 0.0 || x == 1.0 {
        return x;
    }

    let mut t = x;
    for _ in 0..8 {
        let error = bezier_sample(t, x1, x2) - x;
        let slope = bezier_slope(t, x1, x2);
        if slope.abs() < 1.0e-6 {
            break;
        }
        let next = t - error / slope;
        if !(0.0..=1.0).contains(&next) {
            break;
        }
        t = next;
        if error.abs() < 1.0e-6 {
            return bezier_sample(t, y1, y2);
        }
    }

    let mut lower = 0.0;
    let mut upper = 1.0;
    t = x;
    for _ in 0..24 {
        let sample = bezier_sample(t, x1, x2);
        if (sample - x).abs() < 1.0e-6 {
            break;
        }
        if sample < x {
            lower = t;
        } else {
            upper = t;
        }
        t = (lower + upper) * 0.5;
    }

    bezier_sample(t, y1, y2)
}

fn bezier_sample(t: f32, a1: f32, a2: f32) -> f32 {
    let a = 1.0 - 3.0 * a2 + 3.0 * a1;
    let b = 3.0 * a2 - 6.0 * a1;
    let c = 3.0 * a1;
    ((a * t + b) * t + c) * t
}

fn bezier_slope(t: f32, a1: f32, a2: f32) -> f32 {
    let a = 1.0 - 3.0 * a2 + 3.0 * a1;
    let b = 3.0 * a2 - 6.0 * a1;
    let c = 3.0 * a1;
    (3.0 * a * t + 2.0 * b) * t + c
}

pub(crate) fn redraw_subscription(active: bool) -> Subscription<Instant> {
    if active {
        time::every(FRAME_INTERVAL)
    } else {
        Subscription::none()
    }
}

/// GIF thumbnails advance at ~10-25fps and playback is elapsed-based, so a
/// 50ms tick keeps frame timing exact at a third of the redraws. Used when
/// GIF playback is the only live motion; any easing animation upgrades the
/// route back to the full-rate tick.
const GIF_FRAME_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn gif_redraw_subscription() -> Subscription<Instant> {
    time::every(GIF_FRAME_INTERVAL)
}

pub(crate) fn scaled_alpha(color: Rgba, opacity: f32) -> Rgba {
    color.with_alpha(opacity_byte(f32::from(color.a) / 255.0 * opacity))
}

pub(crate) fn opacity_byte(opacity: f32) -> u8 {
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(crate) fn mix_color(start: Color, end: Color, progress: f32) -> Color {
    let progress = progress.clamp(0.0, 1.0);
    Color {
        r: start.r + (end.r - start.r) * progress,
        g: start.g + (end.g - start.g) * progress,
        b: start.b + (end.b - start.b) * progress,
        a: start.a + (end.a - start.a) * progress,
    }
}

fn duration_ms(ms: u16) -> Duration {
    Duration::from_millis(u64::from(ms))
}

#[cfg(test)]
mod tests {
    use iced::animation::Easing;

    use super::*;

    #[test]
    fn motion_tokens_convert_to_durations() {
        let motion = Motion {
            fast_ms: 100,
            hover_in_ms: 90,
            hover_out_ms: 220,
            modal_enter_ms: 180,
            modal_exit_ms: 130,
            context_menu_enter_ms: 120,
            context_menu_exit_ms: 100,
            thumb_reveal_ms: 150,
            overlay_toast_ms: 500,
        };

        assert_eq!(motion.fast_duration(), Duration::from_millis(100));
        assert_eq!(motion.hover_in_duration(), Duration::from_millis(90));
        assert_eq!(motion.hover_out_duration(), Duration::from_millis(220));
        assert_eq!(motion.modal_enter_duration(), Duration::from_millis(180));
        assert_eq!(motion.modal_exit_duration(), Duration::from_millis(130));
        assert_eq!(
            motion.context_menu_enter_duration(),
            Duration::from_millis(120)
        );
        assert_eq!(
            motion.context_menu_exit_duration(),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn asymmetric_presence_uses_direction_durations() {
        let started = Instant::now();
        let mut animation = asymmetric(
            false,
            Duration::from_millis(200),
            Duration::from_millis(100),
            Easing::Linear,
        );

        animation.go(true, started);
        assert!(animation.is_animating(started + Duration::from_millis(150)));
        assert!(animation.tick(started + Duration::from_millis(225)));

        animation.go(false, started);
        assert!(!animation.is_animating(started + Duration::from_millis(150)));
        assert!(animation.tick(started + Duration::from_millis(150)));
    }

    #[test]
    fn presence_tracks_transitions() {
        let started = Instant::now();
        let mut animation = boolean(false, Duration::from_millis(100), Easing::Linear);

        animation.go(true, started);

        assert!(animation.needs_ticks());
        assert!(animation.is_animating(started + Duration::from_millis(50)));
        assert!(!animation.is_animating(started + Duration::from_millis(125)));
        assert_eq!(
            animation.interpolate(0.0, 1.0, started + Duration::from_millis(125)),
            1.0
        );
        assert!(animation.tick(started + Duration::from_millis(125)));
        assert!(!animation.needs_ticks());
    }

    #[test]
    fn presence_keeps_gate_open_until_late_finalizing_tick() {
        let started = Instant::now();
        let mut animation = boolean(true, Duration::from_millis(100), Easing::Linear);

        animation.go(false, started);

        assert!(!animation.is_animating(started + Duration::from_millis(125)));
        assert!(animation.needs_ticks());
        assert!(animation.tick(started + Duration::from_millis(500)));
        assert!(!animation.needs_ticks());
    }

    #[test]
    fn alpha_helpers_clamp_to_byte_range() {
        assert_eq!(opacity_byte(-1.0), 0);
        assert_eq!(opacity_byte(0.5), 128);
        assert_eq!(opacity_byte(2.0), 255);
        assert_eq!(
            scaled_alpha(Rgba::from_rgba(0xFFFFFF, 128), 0.5),
            Rgba::from_rgba(0xFFFFFF, 64)
        );
    }

    #[test]
    fn color_mix_interpolates_channels() {
        let mixed = mix_color(Color::BLACK, Color::WHITE, 0.25);

        assert_eq!(mixed, Color::from_rgba(0.25, 0.25, 0.25, 1.0));
        assert_eq!(mix_color(Color::BLACK, Color::WHITE, -1.0), Color::BLACK);
        assert_eq!(mix_color(Color::BLACK, Color::WHITE, 2.0), Color::WHITE);
    }

    #[test]
    fn cubic_bezier_helpers_match_reference_points() {
        assert_approx(expo_ease_curve(0.0), 0.0);
        assert_approx(expo_ease_curve(1.0), 1.0);

        // Front-loaded curve: most of the travel happens by the midpoint.
        assert!(expo_ease_curve(0.5) > 0.9);
    }

    #[test]
    fn monotonic_bezier_helpers_never_go_backwards() {
        let mut previous = expo_ease_curve(0.0);
        for step in 1..=20 {
            let next = expo_ease_curve(step as f32 / 20.0);
            assert!(next >= previous);
            previous = next;
        }
    }

    fn assert_approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {actual} to be within 0.001 of {expected}"
        );
    }
}
