# Platform services

This crate contains the macOS boundary and portable fixtures. The rest of the
application uses `TabletSource`, `DisplayInfo`, `WindowInfo`, `OverlayWindow`,
`ScreenCapture`, and `PdfRasterizer`. The native implementation is Rust using
pinned objc2 AppKit, CoreGraphics, and ScreenCaptureKit bindings. No Swift,
Objective-C source, shell capture commands, or runtime network are involved.

`create_display_info` and `create_window_info` enumerate native desktop metadata.
Display and window rectangles use global logical points with a top-left origin;
negative coordinates support displays above or to the left of the main display.
AppKit window placement converts to its bottom-left convention at the boundary.
The public API does not establish that an arbitrary external display is Sidecar,
so native external displays return `sidecar: None`. `select_canvas_display`
honors a selected display ID before an explicitly identified Sidecar display or
the primary display. Display names are never used as a detection heuristic.
Window titles may be empty when macOS restricts metadata.

`attach_overlay` configures an application-owned window by its native ID. The
app must render with a transparent surface/background; this service makes the
native window borderless, nonopaque, above ordinary windows, and shadowless.
`OverlayTarget::Display` is live display coverage; `OverlayTarget::Window` follows
the selected window's frame on `refresh`. Refresh also hides a missing/minimized
target, and restores visibility when it reappears. The UI should call refresh
when geometry changes or during its frame update. Dropping the attachment
restores the original native window configuration and frame.

The native input modes are explicit: `ClickThrough` passes pointer events to
underlying applications, and `Draw` accepts events while the portable input
router paints stylus input only. **Unverified/deferred:** AppKit's
`ignoresMouseEvents` switch applies to the whole window; these modes do not yet
provide simultaneous physical-mouse pass-through and selective Pencil
interception over another application. Do not describe the current native path
as satisfying that stronger behavior. Live overlays never invoke screen capture
or request screen-recording permission.

`create_screen_capture` does not prompt. First show
`CAPTURE_PERMISSION_EXPLANATION`; call `request_permission` only from an explicit
capture/permission action. `capture` refuses to run without existing permission,
requires a worker thread on macOS, and uses the ScreenCaptureKit still-image API
(macOS 14 or newer). Targets are a display ID, a window ID, or a region contained
within one display. Native completion is asynchronous behind the synchronous
worker interface. A 30-second timeout retains a busy guard until the OS finishes,
preventing repeated requests on the same service from accumulating native work.
Capture returns encoded-sRGB premultiplied RGBA8 in top-left row order. Importers
must unpremultiply, decode the sRGB transfer function, and premultiply in linear
space. The byte argument bounds the returned pixel allocation; native framework
scratch allocations must also be included in the application's process budget.

`create_pdf_rasterizer` uses immutable source bytes with zero-based page indices.
The native parser accepts at most 64 MiB of PDF source; output dimensions have a
checked caller-supplied pixel budget. Each render draws the original cropped and
rotated PDF page at the requested resolution with preserved aspect ratio and a
white background. It does not enlarge a previous bitmap. CoreGraphics controls
internal parser allocations, so the application still monitors process memory.
Keep original source bytes in the document package for later rerender/export.

Portable tests use shared `MockDesktop` geometry, `MockOverlayWindow`, explicit
permission transitions and synthetic source images in `MockScreenCapture`, and
vector fixtures in `MockPdfRasterizer`. Native tests construct a small PDF and
verify orientation, rotation, color, premultiplication, and fine detail at a
larger zoom. The mock is a deterministic vector fixture renderer, not a second
PDF parser. `./scripts/cargo.sh test -p sideduck-platform` does not capture the
screen or request permission. Physical Sidecar/Pencil delivery, actual capture
consent/denial, protected-window behavior, and native overlay interaction remain
unverified until the runbook's device checks are performed.

Primary API references: Apple's
[ScreenCaptureKit still capture](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager/captureimage%28contentfilter%3Aconfiguration%3Acompletionhandler%3A%29?language=objc),
[window pointer interception](https://developer.apple.com/documentation/appkit/nswindow/ignoresmouseevents),
and [Quartz PDF drawing and transforms](https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/drawingwithquartz2d/dq_pdf/dq_pdf.html).

## Tablet events and process memory

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
