# Platform boundary

`TabletSource` is the drawing prototype's only native input dependency. Construct
it on the UI thread after creating the event loop, and drain it once per frame.
`MockTabletSource` replays `RawTabletEvent` values through the same router used by
macOS. Linux builds use the mock and never link Apple frameworks.

The AppKit adapter listens for both `NSEventType::TabletPoint` and mouse events
whose subtype is `NSEventSubtype::TabletPoint`, plus direct and embedded proximity
events. Plain mouse input never reaches the tablet queue. Each callback returns
the original event unchanged so normal window/UI behavior continues. Dropping
the source removes its AppKit monitor.

Samples contain pressure, native X/Y tilt, device identity, eraser state,
button mask, event timestamp, window number, and content-view coordinates.
`position_top_left()` converts the documented bottom-left point coordinates for
an egui canvas. Compare `window_id` with `window_id_for_title("sideDuck Canvas")`
before drawing; a sample in the controls window must not affect the canvas.
Coordinates are native logical points, independent of the display's backing
scale. Divide them by egui's `Context::zoom_factor()` when UI zoom is enabled.

AppKit provides tilt components rather than a standalone azimuth value; the
prototype derives azimuth with `atan2(tilt_y, tilt_x)` in the native Y-up plane.
Zero tilt has azimuth zero. Eraser state comes from proximity events, and barrel
buttons are reported without causing a hover-only pen to paint.

The router tracks one active pen at a time. Changing devices, changing destination
windows, or leaving proximity ends a contact. Rejected samples end any active
contact, including when an input gap occurs inside a single drained batch. It
rejects non-finite samples, clamps pressure and tilt to their
documented ranges, and suppresses exact duplicates exposed through both AppKit
paths. It does not coalesce distinct samples. The queue has a caller-specified
fixed capacity and discards the oldest sample on overflow; when
`dropped_samples()` changes, end the current stroke to avoid joining across a gap.

`process_memory()` reads actual process RSS and the kernel's lifetime peak:
`proc_pidinfo(PROC_PIDTASKINFO)` and `getrusage` on macOS; `/proc/self/status`
`VmRSS` and `VmHWM` on Linux. It does not substitute the tile-cache accounting or
virtual address-space size for process memory.

Tests cover deterministic mock replay, mouse rejection, duplicate suppression,
contact phases, eraser/device changes, barrel-only hover, queue ceilings,
coordinate conversion, malformed samples, and memory-unit conversion. Native
compilation and a running event monitor do not prove physical Apple Pencil
delivery. **Sidecar pressure, tilt, and proximity remain unverified until tested
on an iPad and Pencil.**

API references: Apple's [tablet event packaging guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/HandlingTabletEvents/HandlingTabletEvents.html),
[AppKit NSEvent](https://developer.apple.com/documentation/appkit/nsevent),
[local event monitors](https://developer.apple.com/documentation/appkit/nsevent/addlocalmonitorforevents(matching:handler:)),
and the pinned [objc2 AppKit bindings](https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/struct.NSEvent.html).
