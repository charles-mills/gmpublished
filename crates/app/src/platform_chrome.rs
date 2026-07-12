#![allow(unsafe_code)]

use std::{fmt, ptr::NonNull};

use block2::RcBlock;
use iced::{Subscription, futures::channel::mpsc as iced_mpsc, stream};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSButton, NSView, NSWindow, NSWindowButton, NSWindowDidResizeNotification, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotification, NSNotificationCenter,
    NSOperationQueue, NSPoint, NSString,
};
use raw_window_handle::RawWindowHandle;

/// Posted system-wide when the user (or an automatic schedule) switches
/// between light and dark appearance. AppKit re-lays out the titlebar on
/// that change, resetting the custom traffic-light container and button
/// frames, so the treatment must be re-applied afterwards.
const APPEARANCE_CHANGED_NOTIFICATION: &str = "AppleInterfaceThemeChangedNotification";

/// Emits `()` each time the system appearance flips between light and dark.
pub fn appearance_change_subscription() -> Subscription<()> {
    Subscription::run(appearance_change_stream)
}

fn appearance_change_stream() -> impl iced::futures::Stream<Item = ()> + use<> {
    stream::channel(4, async move |output: iced_mpsc::Sender<()>| {
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            // A full buffer means a reposition is already queued; repositioning
            // is idempotent, so dropping the extra event is fine.
            let _ = output.clone().try_send(());
        });
        // SAFETY: no `object` filter is passed, so there is no typed-object
        // requirement; delivery on the main queue satisfies the queue's
        // threading requirements; the block only clones a `futures` mpsc
        // sender and sends on it, both of which are `Send`.
        let observer = unsafe {
            NSDistributedNotificationCenter::defaultCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(&NSString::from_str(APPEARANCE_CHANGED_NOTIFICATION)),
                    None,
                    Some(&NSOperationQueue::mainQueue()),
                    &block,
                )
        };
        // The observer (and the sender its block holds) lives for the rest of
        // the process, keeping this stream open.
        std::mem::forget(observer);
    })
}

pub fn apply(inset: bool) -> impl FnOnce(&dyn iced::window::Window) {
    move |window| {
        if let Err(error) = apply_to_window(window, inset) {
            log::warn!("failed to live-apply macOS titlebar treatment: {error}");
        }
    }
}

pub fn position_traffic_lights(
    origin_x: f64,
    center_y: f64,
) -> impl FnOnce(&dyn iced::window::Window) {
    move |window| {
        if let Err(error) = position_traffic_lights_on_window(window, origin_x, center_y) {
            log::warn!("failed to position macOS traffic lights: {error}");
        }
    }
}

/// Installs a process-lifetime observer that re-asserts the traffic-light
/// treatment synchronously inside every resize layout pass. AppKit re-lays
/// out the titlebar container on each live-resize frame, so an asynchronous
/// re-apply (round-tripped through the Iced message loop) visibly loses the
/// fight until the resize settles; only a same-pass reposition is flicker-free.
pub fn install_resize_keepalive(
    origin_x: f64,
    center_y: f64,
) -> impl FnOnce(&dyn iced::window::Window) {
    move |window| {
        if let Err(error) = install_resize_keepalive_on_window(window, origin_x, center_y) {
            log::warn!("failed to install macOS traffic-light resize keepalive: {error}");
        }
    }
}

fn install_resize_keepalive_on_window(
    window: &dyn iced::window::Window,
    origin_x: f64,
    center_y: f64,
) -> Result<(), ApplyError> {
    let ns_window = ns_window_for(window)?;
    let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
        // SAFETY: the notification pointer is live for the duration of the
        // synchronous delivery this block runs inside.
        let notification = unsafe { notification.as_ref() };
        let Some(object) = notification.object() else {
            return;
        };
        let Ok(ns_window) = object.downcast::<NSWindow>() else {
            return;
        };
        // The system-titlebar preference may be active (or switched to at
        // runtime); the inset treatment is marked by FullSizeContentView.
        if !ns_window
            .styleMask()
            .contains(NSWindowStyleMask::FullSizeContentView)
        {
            return;
        }
        if let Err(error) = position_traffic_lights_on_ns_window(&ns_window, origin_x, center_y) {
            log::warn!("failed to re-position macOS traffic lights on resize: {error}");
        }
    });
    // SAFETY: the object filter matches without retaining the window; a nil
    // queue delivers synchronously on the posting (main) thread, and the
    // block captures only `Copy` floats, so it is trivially sendable.
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSWindowDidResizeNotification),
            Some(&ns_window),
            None,
            &block,
        )
    };
    // App-lifetime observer for the app's one window: never removed.
    std::mem::forget(observer);
    Ok(())
}

fn ns_window_for(window: &dyn iced::window::Window) -> Result<Retained<NSWindow>, ApplyError> {
    let _main_thread = MainThreadMarker::new().ok_or(ApplyError::NotMainThread)?;
    let handle = window
        .window_handle()
        .map_err(|_| ApplyError::WindowHandleUnavailable)?
        .as_raw();
    let RawWindowHandle::AppKit(handle) = handle else {
        return Err(ApplyError::NotAppKitWindow);
    };

    // SAFETY: `handle.ns_view` comes from raw-window-handle's
    // `AppKitWindowHandle`, which guarantees it points to a live `NSView*`
    // for as long as the window is open; this runs synchronously against the
    // still-alive `window` passed in by the caller, so the pointer is valid,
    // non-null, and properly aligned for the short-lived reference used below.
    let ns_view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    ns_view.window().ok_or(ApplyError::MissingWindow)
}

fn position_traffic_lights_on_window(
    window: &dyn iced::window::Window,
    origin_x: f64,
    center_y: f64,
) -> Result<(), ApplyError> {
    let ns_window = ns_window_for(window)?;
    position_traffic_lights_on_ns_window(&ns_window, origin_x, center_y)
}

fn position_traffic_lights_on_ns_window(
    ns_window: &NSWindow,
    origin_x: f64,
    center_y: f64,
) -> Result<(), ApplyError> {
    // Native fullscreen owns the auto-hide traffic-light overlay.
    if ns_window
        .styleMask()
        .contains(NSWindowStyleMask::FullScreen)
    {
        return Ok(());
    }

    let close = standard_window_button(ns_window, NSWindowButton::CloseButton)?;
    let miniaturize = standard_window_button(ns_window, NSWindowButton::MiniaturizeButton)?;
    let zoom = standard_window_button(ns_window, NSWindowButton::ZoomButton)?;

    // The buttons live inside the titlebar container view, a strip pinned to
    // the window's top edge — button frames are LOCAL to it, and a target
    // below its bottom edge just vanishes. So, like Electron's
    // trafficLightPosition, grow the container downward to span the target
    // zone, then place the buttons within it.
    // SAFETY: `superview()` returns an unretained reference (per its own
    // "# Safety" doc: "you must ensure the object is still alive"). `close`
    // is a button in `ns_window`'s view hierarchy, and we hold `ns_window`
    // retained for this whole function, so its titlebar bar view and that
    // view's container superview are kept alive transitively for the
    // duration of both calls.
    let container = unsafe { close.superview().and_then(|titlebar| titlebar.superview()) }
        .ok_or(ApplyError::MissingTitlebarContainer)?;
    let container_height = center_y * 2.0;
    let window_frame = ns_window.frame();
    let mut container_frame = container.frame();
    container_frame.origin.y = window_frame.size.height - container_height;
    container_frame.size.height = container_height;
    container.setFrame(container_frame);

    let origins = traffic_light_button_origins(
        origin_x,
        container_height,
        ButtonFrame::from_button(&close),
        ButtonFrame::from_button(&miniaturize),
        ButtonFrame::from_button(&zoom),
    );

    set_button_origin(&close, origins.close);
    set_button_origin(&miniaturize, origins.miniaturize);
    set_button_origin(&zoom, origins.zoom);

    Ok(())
}

fn standard_window_button(
    ns_window: &NSWindow,
    button: NSWindowButton,
) -> Result<Retained<NSButton>, ApplyError> {
    ns_window
        .standardWindowButton(button)
        .ok_or(ApplyError::MissingButtons)
}

fn set_button_origin(button: &NSButton, origin: ButtonOrigin) {
    button.setFrameOrigin(NSPoint::new(origin.x, origin.y));
}

fn apply_to_window(window: &dyn iced::window::Window, inset: bool) -> Result<(), ApplyError> {
    let ns_window = ns_window_for(window)?;

    // Captured before any treatment: toggling FullSizeContentView remaps
    // content-rect ↔ frame-rect, and a cached content size re-asserted under
    // the new mapping shrinks the window by exactly the titlebar height —
    // the window must end this function at the frame it entered with.
    let original_frame = ns_window.frame();

    ns_window.setTitlebarAppearsTransparent(inset);
    ns_window.setTitleVisibility(if inset {
        NSWindowTitleVisibility::Hidden
    } else {
        NSWindowTitleVisibility::Visible
    });

    let mut style_mask = ns_window.styleMask();
    if inset {
        style_mask.insert(NSWindowStyleMask::FullSizeContentView);
    } else {
        style_mask.remove(NSWindowStyleMask::FullSizeContentView);
    }
    ns_window.setStyleMask(style_mask);

    // AppKit does not re-lay out a live window's content view when
    // FullSizeContentView is toggled — the content keeps its old frame until
    // the next real resize, leaving the band hidden under (or shy of) the
    // titlebar. Force a layout pass via a 1pt frame detour; a same-rect
    // setFrame is short-circuited and does nothing. Ending on the entry
    // frame (not the post-toggle frame, which may already be shrunken)
    // also undoes any mid-flip size reassertion.
    let mut nudged = original_frame;
    nudged.size.height -= 1.0;
    ns_window.setFrame_display(nudged, false);
    ns_window.setFrame_display(original_frame, true);

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonFrame {
    x: f64,
    height: f64,
}

impl ButtonFrame {
    fn from_button(button: &NSButton) -> Self {
        let frame = button.frame();
        Self {
            x: frame.origin.x,
            height: frame.size.height,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonOrigin {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrafficLightButtonOrigins {
    close: ButtonOrigin,
    miniaturize: ButtonOrigin,
    zoom: ButtonOrigin,
}

fn traffic_light_button_origins(
    origin_x: f64,
    container_height: f64,
    close: ButtonFrame,
    miniaturize: ButtonFrame,
    zoom: ButtonFrame,
) -> TrafficLightButtonOrigins {
    // x preserves the system's inter-button spacing (read from the live
    // frames — it shifts across macOS releases); y centers each button in
    // the grown container, which is correct whether or not the titlebar
    // view is flipped.
    let button_origin = |button: ButtonFrame| ButtonOrigin {
        x: origin_x + (button.x - close.x),
        y: (container_height - button.height) / 2.0,
    };

    TrafficLightButtonOrigins {
        close: button_origin(close),
        miniaturize: button_origin(miniaturize),
        zoom: button_origin(zoom),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyError {
    NotMainThread,
    WindowHandleUnavailable,
    NotAppKitWindow,
    MissingWindow,
    MissingButtons,
    MissingTitlebarContainer,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => formatter.write_str("callback did not run on the main thread"),
            Self::WindowHandleUnavailable => formatter.write_str("window handle was unavailable"),
            Self::NotAppKitWindow => formatter.write_str("window handle was not AppKit"),
            Self::MissingWindow => formatter.write_str("NSView was not attached to an NSWindow"),
            Self::MissingButtons => {
                formatter.write_str("window has no standard traffic-light buttons")
            }
            Self::MissingTitlebarContainer => {
                formatter.write_str("traffic-light buttons have no titlebar container view")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonFrame, ButtonOrigin, traffic_light_button_origins};

    #[test]
    fn traffic_light_origins_preserve_system_button_offsets() {
        let origins = traffic_light_button_origins(
            22.0,
            58.0,
            ButtonFrame {
                x: 8.0,
                height: 14.0,
            },
            ButtonFrame {
                x: 28.0,
                height: 14.0,
            },
            ButtonFrame {
                x: 48.0,
                height: 14.0,
            },
        );

        assert_eq!(origins.close, ButtonOrigin { x: 22.0, y: 22.0 });
        assert_eq!(origins.miniaturize, ButtonOrigin { x: 42.0, y: 22.0 });
        assert_eq!(origins.zoom, ButtonOrigin { x: 62.0, y: 22.0 });
    }

    #[test]
    fn traffic_light_origins_center_each_button_in_the_container() {
        let origins = traffic_light_button_origins(
            22.0,
            58.0,
            ButtonFrame {
                x: 8.0,
                height: 14.0,
            },
            ButtonFrame {
                x: 28.0,
                height: 18.0,
            },
            ButtonFrame {
                x: 50.0,
                height: 12.0,
            },
        );

        assert_eq!(origins.close, ButtonOrigin { x: 22.0, y: 22.0 });
        assert_eq!(origins.miniaturize, ButtonOrigin { x: 42.0, y: 20.0 });
        assert_eq!(origins.zoom, ButtonOrigin { x: 64.0, y: 23.0 });
    }
}
