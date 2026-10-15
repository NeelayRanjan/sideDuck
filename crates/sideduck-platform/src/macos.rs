//! Thin AppKit event translation. UNVERIFIED with physical Apple Pencil/Sidecar:
//! native compilation and portable routing tests cannot establish device delivery,
//! pressure/tilt ranges, or eraser availability on a user's particular device.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventSubtype, NSEventType, NSPointingDeviceType,
};

use crate::{EventKind, PlatformError, RawTabletEvent, TabletRouter, TabletSample, TabletSource};

pub struct NativeTabletSource {
    monitor: Retained<AnyObject>,
    router: Rc<RefCell<TabletRouter>>,
    // Prevent moving the monitor away from the thread that owns the event loop.
    _main_thread: MainThreadMarker,
}

impl NativeTabletSource {
    pub fn new(capacity: usize) -> Result<Self, PlatformError> {
        let main_thread = MainThreadMarker::new().ok_or(PlatformError(
            "Tablet monitor must be attached on the UI thread",
        ))?;
        let router = Rc::new(RefCell::new(TabletRouter::new(capacity)));
        let callback_router = router.clone();
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit supplies a live event for the duration of the callback.
            if let Some(sample) = unsafe { translate_event(event.as_ref(), main_thread) } {
                callback_router.borrow_mut().push(sample);
            }
            // Monitoring must not interfere with winit's normal event handling.
            event.as_ptr()
        });
        let mask = NSEventMask::TabletPoint
            | NSEventMask::TabletProximity
            | NSEventMask::LeftMouseDown
            | NSEventMask::LeftMouseUp
            | NSEventMask::LeftMouseDragged
            | NSEventMask::RightMouseDown
            | NSEventMask::RightMouseUp
            | NSEventMask::RightMouseDragged
            | NSEventMask::OtherMouseDown
            | NSEventMask::OtherMouseUp
            | NSEventMask::OtherMouseDragged
            | NSEventMask::MouseMoved;
        // SAFETY: Correct callback ABI; AppKit copies the block and invokes it on
        // this main thread. The token is removed before its captured state is freed.
        let monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) }
                .ok_or(PlatformError(
                    "AppKit did not create the tablet event monitor",
                ))?;
        Ok(Self {
            monitor,
            router,
            _main_thread: main_thread,
        })
    }
}

impl TabletSource for NativeTabletSource {
    fn drain_samples(&mut self) -> Vec<TabletSample> {
        self.router.borrow_mut().drain()
    }

    fn dropped_samples(&self) -> u64 {
        self.router.borrow().dropped_samples()
    }

    fn description(&self) -> &'static str {
        "AppKit tablet events (physical Sidecar verification pending)"
    }
}

impl Drop for NativeTabletSource {
    fn drop(&mut self) {
        // SAFETY: The token came from addLocalMonitor and is removed exactly once,
        // on the main thread. AppKit releases its copied block when removed.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
    }
}

fn translate_event(event: &NSEvent, main_thread: MainThreadMarker) -> Option<RawTabletEvent> {
    // Only getters applicable to the checked event type/subtype are used.
    // All AppKit view calls run on the main thread.
    let event_type = event.r#type();
    let subtype = event.subtype();
    let is_proximity =
        event_type == NSEventType::TabletProximity || subtype == NSEventSubtype::TabletProximity;
    if is_proximity {
        return Some(RawTabletEvent {
            kind: EventKind::Proximity {
                entering: event.isEnteringProximity(),
                eraser: event.pointingDeviceType() == NSPointingDeviceType::Eraser,
            },
            tablet_subtype: true,
            position: [0.0; 2],
            window_size: [0.0; 2],
            window_id: event.windowNumber() as i64,
            pressure: 0.0,
            tilt: [0.0; 2],
            timestamp_seconds: event.timestamp(),
            device_id: event.deviceID() as u64,
            buttons: 0,
        });
    }
    let tablet_subtype = subtype == NSEventSubtype::TabletPoint;
    if event_type != NSEventType::TabletPoint && !tablet_subtype {
        return None;
    }
    let kind = match event_type {
        NSEventType::TabletPoint => EventKind::TabletPoint,
        NSEventType::LeftMouseDown => EventKind::MouseDown,
        NSEventType::LeftMouseUp => EventKind::MouseUp,
        NSEventType::LeftMouseDragged => EventKind::MouseDragged,
        // Secondary buttons can be a barrel switch while hovering. Preserve
        // their mask but let pressure/tip contact determine the stroke phase.
        NSEventType::RightMouseDown
        | NSEventType::RightMouseUp
        | NSEventType::RightMouseDragged
        | NSEventType::OtherMouseDown
        | NSEventType::OtherMouseUp
        | NSEventType::OtherMouseDragged => EventKind::TabletPoint,
        NSEventType::MouseMoved => EventKind::MouseMoved,
        _ => return None,
    };
    let (position, window_size) = if let Some(content) = event
        .window(main_thread)
        .and_then(|window| window.contentView())
    {
        let bounds = content.bounds();
        let location = content.convertPoint_fromView(event.locationInWindow(), None);
        let x = location.x - bounds.origin.x;
        let y = location.y - bounds.origin.y;
        let y = if content.isFlipped() {
            bounds.size.height - y
        } else {
            y
        };
        ([x, y], [bounds.size.width, bounds.size.height])
    } else {
        // A point without a drawable content view is an input gap. The portable
        // router rejects zero size and releases any earlier contact, instead of
        // silently dropping a possible pen-up from a window being closed.
        ([0.0; 2], [0.0; 2])
    };
    let tilt = event.tilt();
    Some(RawTabletEvent {
        kind,
        tablet_subtype,
        position,
        window_size,
        window_id: event.windowNumber() as i64,
        pressure: event.pressure(),
        tilt: [tilt.x as f32, tilt.y as f32],
        timestamp_seconds: event.timestamp(),
        device_id: event.deviceID() as u64,
        buttons: event.buttonMask().bits() as u64,
    })
}

pub fn window_id_for_title(title: &str) -> Option<i64> {
    let main_thread = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(main_thread);
    let windows = app.windows();
    for index in 0..windows.count() {
        let window = windows.objectAtIndex(index);
        if window.title().to_string() == title {
            return Some(window.windowNumber() as i64);
        }
    }
    None
}
