//! Handle the original AppKit mouseDown without a deferred currentEvent lookup.
//! This view owns no Rust/global window references; AppKit owns it as a subview.
//! Native caption buttons share the rendered header's vertical center.
use super::{HEADER_HEIGHT, MACOS_TRAFFIC_LIGHT_WIDTH, PICK_FOLDER_BUTTON_WIDTH};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSEvent, NSView, NSWindow, NSWindowButton, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};
use raw_window_handle::RawWindowHandle;
use std::cell::Cell;
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
    // only an initialized reentrancy flag, and has no custom Drop/dealloc.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OhMyDiskHeaderDragBand"]
    #[ivars = Cell<bool>]
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

        // SAFETY: Exact NSView lifecycle signatures, invoked on the main thread
        // with a live hierarchy. Superclass behavior completes before alignment.
        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
            self.align_controls();
        }

        // SAFETY: Exact NSView layout signature; self remains main-thread-only.
        #[unsafe(method(layout))]
        fn layout(&self) {
            unsafe { let _: () = msg_send![super(self), layout]; }
            self.align_controls();
        }

        // SAFETY: Exact NSView resizing signature and NSSize ABI.
        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, old_size: NSSize) {
            unsafe { let _: () = msg_send![super(self), resizeSubviewsWithOldSize: old_size]; }
            self.align_controls();
        }

        // SAFETY: Exact NSView pre-draw callback, still on the main thread.
        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            unsafe { let _: () = msg_send![super(self), viewWillDraw]; }
            self.align_controls();
        }
    }
);

impl HeaderDragBand {
    fn align_controls(&self) {
        if self.ivars().replace(true) {
            return;
        }
        // SAFETY: AppKit owns the attached hierarchy throughout this synchronous
        // main-thread callback. Temporary retains never escape the callback.
        if let Some(content) = unsafe { self.superview() }
            && let Some(window) = self.window()
        {
            align_native_controls(&content, &window);
        }
        self.ivars().set(false);
    }

    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(Cell::new(false));
        // SAFETY: The flag is initialized, NSView implements initWithFrame:
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

fn aligned_container_frame(
    mut frame: NSRect,
    current_center_y: f64,
    target_center_y: f64,
    parent_flipped: bool,
) -> Option<NSRect> {
    // Expanding downward while preserving the top moves the whole native
    // control group. Its buttons keep AppKit's original bottom inset.
    let extra_height = if parent_flipped {
        target_center_y - current_center_y
    } else {
        current_center_y - target_center_y
    };
    if !frame.origin.x.is_finite()
        || !frame.origin.y.is_finite()
        || !frame.size.width.is_finite()
        || !frame.size.height.is_finite()
        || frame.size.width <= 0.0
        || frame.size.height <= 0.0
        || !extra_height.is_finite()
        || extra_height.abs() < 0.01
    {
        return None;
    }
    frame.size.height += extra_height;
    if !parent_flipped {
        frame.origin.y -= extra_height;
    }
    (frame.origin.y.is_finite() && frame.size.height.is_finite() && frame.size.height > 0.0)
        .then_some(frame)
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
    let Some(close) = window.standardWindowButton(NSWindowButton::CloseButton) else {
        return;
    };
    let Some(minimize) = window.standardWindowButton(NSWindowButton::MiniaturizeButton) else {
        return;
    };
    let Some(zoom) = window.standardWindowButton(NSWindowButton::ZoomButton) else {
        return;
    };
    // SAFETY: The retained live window owns its native view hierarchy throughout
    // this synchronous main-thread callback. These temporary retains are local.
    let Some(bar) = (unsafe { close.superview() }) else {
        return;
    };
    for button in [&minimize, &zoom] {
        // SAFETY: Same live, retained window hierarchy as above.
        let Some(parent) = (unsafe { button.superview() }) else {
            return;
        };
        if !std::ptr::eq(&*bar, &*parent) {
            return;
        }
    }
    // Like tao's traffic-light inset, resize the native titlebar container,
    // preserving its top edge. The container owns native titlebar layout and
    // drawing; its buttons retain AppKit's original inset within that group.
    // SAFETY: The same window retains both ancestors for this callback.
    let Some(container) = (unsafe { bar.superview() }) else {
        return;
    };
    // SAFETY: The same window retains the container's parent for this callback.
    let Some(parent) = (unsafe { container.superview() }) else {
        return;
    };
    let button_frame = close.frame();
    let center = NSPoint::new(
        button_frame.origin.x + button_frame.size.width / 2.0,
        button_frame.origin.y + button_frame.size.height / 2.0,
    );
    let current = parent.convertPoint_fromView(center, Some(&bar));
    let content_center = content.convertPoint_fromView(center, Some(&bar));
    let target = parent.convertPoint_fromView(
        NSPoint::new(content_center.x, header.origin.y + header.size.height / 2.0),
        Some(content),
    );
    let old_frame = container.frame();
    let Some(frame) = aligned_container_frame(old_frame, current.y, target.y, parent.isFlipped())
    else {
        return;
    };
    // No button is reparented or resized. Native x/y, target/action,
    // accessibility and control ownership all remain AppKit's responsibility.
    container.setFrame(frame);
    if std::env::var_os("OMD_TRACE_WINDOW_CONTROLS").is_some_and(|value| value == "1") {
        let actual = content.convertPoint_fromView(center, Some(&bar));
        let top_distance = if content.isFlipped() {
            actual.y - bounds.origin.y
        } else {
            bounds.origin.y + bounds.size.height - actual.y
        };
        eprintln!(
            "OMD_WINDOW_CONTROLS native_container_before={old_frame:?} native_container_after={:?} button_frame={:?} center_from_content_top={top_distance}",
            container.frame(),
            close.frame()
        );
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
    fn native_container_moves_controls_down_while_preserving_its_top_and_width() {
        let frame = NSRect::new(NSPoint::new(10.0, 668.0), NSSize::new(1000.0, 32.0));
        let aligned = aligned_container_frame(frame, 684.0, 676.0, false).unwrap();
        assert_eq!(aligned.origin, NSPoint::new(10.0, 660.0));
        assert_eq!(aligned.size, NSSize::new(1000.0, 40.0));
        assert_eq!(
            aligned.origin.y + aligned.size.height,
            frame.origin.y + frame.size.height
        );
        assert!(aligned_container_frame(aligned, 676.0, 676.0, false).is_none());
    }

    #[test]
    fn flipped_native_parent_keeps_its_top_and_invalid_sizes_cannot_expand() {
        let frame = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(1000.0, 32.0));
        let aligned = aligned_container_frame(frame, 36.0, 44.0, true).unwrap();
        assert_eq!(aligned.origin, frame.origin);
        assert_eq!(aligned.size, NSSize::new(1000.0, 40.0));
        assert!(aligned_container_frame(frame, f64::NAN, 24.0, true).is_none());
        assert!(aligned_container_frame(frame, 36.0, f64::INFINITY, true).is_none());
        assert!(aligned_container_frame(frame, 36.0, 0.0, true).is_none());
    }

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
