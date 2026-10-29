//! Handle the original AppKit mouseDown without a deferred currentEvent lookup.
//! This view owns no Rust/global window references; AppKit owns it as a subview.
//! Native caption buttons share the rendered header's vertical center.
use super::{HEADER_HEIGHT, MACOS_TRAFFIC_LIGHT_WIDTH, PICK_FOLDER_BUTTON_WIDTH};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSEvent, NSView, NSWindow, NSWindowButton, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};
use raw_window_handle::RawWindowHandle;
use std::sync::atomic::{AtomicU8, Ordering};

const PICKER_RIGHT_SPACING: f64 = 8.0;
// Diagnostic flags hold no native objects. Fixed messages are emitted once and
// contain no paths or window IDs; ordinary hidden/closing cases stay quiet.
static REPORTED_SETUP_ERRORS: AtomicU8 = AtomicU8::new(0);

fn report_setup_once(flag: u8, message: &str) {
    if REPORTED_SETUP_ERRORS.fetch_or(flag, Ordering::Relaxed) & flag == 0 {
        eprintln!("ohMyDisk window chrome: {message}");
    }
}

define_class!(
    // SAFETY: NSView is subclassable. The class remains MainThreadOnly, adds
    // only initialized unit ivars, and has no custom Drop/dealloc behavior.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OhMyDiskHeaderDragBand"]
    #[ivars = ()]
    struct HeaderDragBand;

    // SAFETY: NSObjectProtocol adds no invariants beyond NSView's superclass.
    unsafe impl NSObjectProtocol for HeaderDragBand {}

    impl HeaderDragBand {
        // SAFETY: This is NSResponder's exact mouseDown: signature. AppKit calls
        // this NSView on the main thread with a live event borrowed for this
        // callback. The event is forwarded immediately and is never stored.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(window) = self.window() else { return; };
            if event.clickCount() == 2 {
                window.zoom(None);
            } else {
                window.performWindowDragWithEvent(event);
            }
        }

        // SAFETY: Matches NSView's BOOL isOpaque getter. Drawing nothing with a
        // non-opaque view leaves iced's rendered header visible beneath it.
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool { false }

        // SAFETY: Matches NSView's acceptsFirstMouse: signature, including its
        // nullable event. Accepting the activating click permits normal dragging
        // of an inactive window without claiming keyboard/first-responder focus.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool { true }
    }
);

impl HeaderDragBand {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: Unit ivars are initialized, NSView implements initWithFrame:
        // with this exact NSRect ABI, and alloc/initialization stay on the main
        // thread. The returned +1 object is managed by Retained until AppKit
        // retains it through addSubview below.
        unsafe { msg_send![super(allocated), initWithFrame: frame] }
    }
}

fn drag_bounds(bounds: NSRect, flipped: bool) -> Option<NSRect> {
    let width = bounds.size.width
        - f64::from(MACOS_TRAFFIC_LIGHT_WIDTH)
        - f64::from(PICK_FOLDER_BUTTON_WIDTH)
        - PICKER_RIGHT_SPACING;
    let height = bounds.size.height.min(f64::from(HEADER_HEIGHT));
    if !bounds.origin.x.is_finite()
        || !bounds.origin.y.is_finite()
        || !bounds.size.width.is_finite()
        || !bounds.size.height.is_finite()
        || width <= 0.0
        || height <= 0.0
    {
        return None;
    }
    let x = bounds.origin.x + f64::from(MACOS_TRAFFIC_LIGHT_WIDTH);
    let y = if flipped {
        bounds.origin.y
    } else {
        bounds.origin.y + bounds.size.height - height
    };
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)))
}

fn autoresizing(flipped: bool) -> NSAutoresizingMaskOptions {
    NSAutoresizingMaskOptions::ViewWidthSizable
        | if flipped {
            NSAutoresizingMaskOptions::ViewMaxYMargin
        } else {
            NSAutoresizingMaskOptions::ViewMinYMargin
        }
}

fn align_native_controls(content: &NSView, window: &NSWindow) {
    // AppKit manages its own temporary titlebar/button layout in fullscreen.
    // The ordinary resize/focus callback reapplies this after restoration.
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return;
    }
    let bounds = content.bounds();
    let Some(header) = drag_bounds(bounds, content.isFlipped()) else {
        return;
    };
    let center = NSPoint::new(bounds.origin.x, header.origin.y + header.size.height / 2.0);
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let Some(button) = window.standardWindowButton(kind) else {
            continue;
        };
        // SAFETY: The retained window owns its live native button hierarchy for
        // this synchronous main-thread callback. Retain the current superview
        // only for this iteration; no hierarchy or ownership is changed.
        let Some(parent) = (unsafe { button.superview() }) else {
            continue;
        };
        // The winit content view and native titlebar use different coordinate
        // systems. Converting the actual content center also handles flipped
        // content views, nonzero bounds origins and native titlebar dimensions.
        let target = parent.convertPoint_fromView(center, Some(content));
        let frame = button.frame();
        let y = target.y - frame.size.height / 2.0;
        if y.is_finite() && y != frame.origin.y {
            // Preserve AppKit's horizontal placement, size, target/action,
            // accessibility and fullscreen behavior of the original buttons.
            button.setFrameOrigin(NSPoint::new(frame.origin.x, y));
        }
    }
}

/// Called exclusively from iced::window::run while that window is still alive.
/// All native objects and the borrowed raw handle remain inside this callback.
pub(super) fn install(native: &dyn iced::window::Window) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        report_setup_once(1, "Mac drag setup was not on the AppKit main thread");
        return false;
    };
    let Ok(handle) = native.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        report_setup_once(
            2,
            "Mac drag setup received an unexpected window handle kind",
        );
        return false;
    };
    // SAFETY: raw-window-handle specifies a live NSView pointer for AppKit. iced
    // retains the window throughout this synchronous event-loop callback, and
    // mtm checked that we are on AppKit's main thread. Retain the borrowed view
    // only for this scope; no pointer/reference escapes into a task or global.
    let Some(parent) = (unsafe { Retained::<NSView>::retain(handle.ns_view.as_ptr().cast()) })
    else {
        return false;
    };
    let Some(window) = parent.window() else {
        return false;
    };
    align_native_controls(&parent, &window);
    let flipped = parent.isFlipped();
    let frame = drag_bounds(parent.bounds(), flipped);
    let subviews = parent.subviews();
    for child in subviews.iter() {
        if child.isKindOfClass(HeaderDragBand::class()) {
            if let Some(frame) = frame {
                child.setFrame(frame);
                child.setAutoresizingMask(autoresizing(flipped));
                child.setHidden(false);
            } else {
                child.setHidden(true);
            }
            return frame.is_some();
        }
    }
    let Some(frame) = frame else {
        return false;
    };
    let band = HeaderDragBand::new(frame, mtm);
    band.setAutoresizingMask(autoresizing(flipped));
    // addSubview retains its child. Dropping this temporary Retained leaves
    // exactly AppKit's hierarchy ownership; window close releases the hierarchy.
    // There is no retained parent/window ivar and therefore no retain cycle.
    parent.addSubview(&band);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bounds_preserve_native_controls_picker_and_body_at_all_sizes() {
        for width in [500.0, 800.0, 1000.0, 1370.0] {
            for height in [480.0, 600.0, 900.0] {
                for flipped in [false, true] {
                    let frame = drag_bounds(
                        NSRect::new(NSPoint::ZERO, NSSize::new(width, height)),
                        flipped,
                    )
                    .unwrap();
                    assert_eq!(frame.origin.x, 78.0);
                    assert_eq!(frame.size.width, width - 78.0 - 128.0);
                    assert_eq!(frame.size.height, 48.0);
                    assert_eq!(frame.origin.y, if flipped { 0.0 } else { height - 48.0 });
                    assert_eq!(frame.origin.x + frame.size.width, width - 128.0);
                }
            }
        }
    }

    #[test]
    fn flipped_and_unflipped_views_keep_the_band_attached_to_the_top() {
        let bounds = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(500.0, 480.0));
        assert_eq!(
            drag_bounds(bounds, true).unwrap().origin,
            NSPoint::new(88.0, 20.0)
        );
        assert_eq!(
            drag_bounds(bounds, false).unwrap().origin,
            NSPoint::new(88.0, 452.0)
        );
        assert_eq!(
            autoresizing(true),
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin
        );
        assert_eq!(
            autoresizing(false),
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin
        );
    }

    #[test]
    fn empty_or_invalid_viewport_cannot_leave_a_drag_band_over_controls() {
        for size in [
            NSSize::new(206.0, 480.0),
            NSSize::new(500.0, 0.0),
            NSSize::new(f64::NAN, 480.0),
            NSSize::new(500.0, f64::INFINITY),
        ] {
            assert!(drag_bounds(NSRect::new(NSPoint::ZERO, size), true).is_none());
        }
        let tiny = drag_bounds(NSRect::new(NSPoint::ZERO, NSSize::new(500.0, 20.0)), true).unwrap();
        assert_eq!(tiny.size.height, 20.0);
    }
}
