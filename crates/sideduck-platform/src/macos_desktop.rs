//! Thin AppKit/CoreGraphics desktop translation. UNVERIFIED on a physical Sidecar
//! display. Public display metadata does not reliably identify Sidecar. Selective
//! mouse pass-through during native stylus interception remains unverified;
//! AppKit exposes only a window-wide pointer interception switch.

use crate::*;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSColor, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::*;
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};

pub struct NativeDesktop;
fn main_thread() -> PlatformResult<MainThreadMarker> {
    MainThreadMarker::new().ok_or(PlatformError(
        "Desktop window operations must run on the UI thread",
    ))
}
pub(crate) fn from_cg(rect: objc2_core_foundation::CGRect) -> DesktopRect {
    DesktopRect {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}
impl DisplayInfo for NativeDesktop {
    fn displays(&self) -> PlatformResult<Vec<DisplayDescriptor>> {
        let mtm = main_thread()?;
        let screens = NSScreen::screens(mtm);
        let mut result = Vec::with_capacity(screens.len());
        for screen in screens.iter() {
            let description = screen.deviceDescription();
            let id = description
                .objectForKey(&NSString::from_str("NSScreenNumber"))
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .map(|v| v.unsignedIntValue())
                .ok_or(PlatformError("Native screen lacks a display identifier"))?;
            let frame = from_cg(CGDisplayBounds(id));
            frame.validate()?;
            let built_in = CGDisplayIsBuiltin(id);
            result.push(DisplayDescriptor {
                id,
                name: screen.localizedName().to_string(),
                frame,
                pixel_size: [
                    u32::try_from(CGDisplayPixelsWide(id))
                        .map_err(|_| PlatformError("Display width exceeds supported range"))?,
                    u32::try_from(CGDisplayPixelsHigh(id))
                        .map_err(|_| PlatformError("Display height exceeds supported range"))?,
                ],
                scale_factor: screen.backingScaleFactor(),
                primary: id == CGMainDisplayID(),
                built_in,
                sidecar: if built_in { Some(false) } else { None },
            });
        }
        Ok(result)
    }
}
type NativeDictionary = CFDictionary<CFString, CFType>;
fn dictionary(value: CFRetained<CFType>) -> Option<CFRetained<NativeDictionary>> {
    let value = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: Window-server dictionaries have CFString keys and CFType values.
    Some(unsafe { CFRetained::cast_unchecked(value) })
}
fn number(dict: &NativeDictionary, key: &str) -> Option<f64> {
    dict.get(CFString::from_str(key).as_ref())?
        .downcast::<CFNumber>()
        .ok()?
        .as_f64()
}
fn string(dict: &NativeDictionary, key: &str) -> String {
    dict.get(CFString::from_str(key).as_ref())
        .and_then(|v| v.downcast::<CFString>().ok())
        .map(|v| v.to_string())
        .unwrap_or_default()
}
impl WindowInfo for NativeDesktop {
    fn windows(&self) -> PlatformResult<Vec<WindowDescriptor>> {
        let list = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
            0,
        )
        .ok_or(PlatformError("Window server did not return a window list"))?;
        // SAFETY: CGWindowListCopyWindowInfo returns an immutable CFArray of
        // CFDictionary objects with string keys and CoreFoundation values.
        let list: CFRetained<CFArray<NativeDictionary>> =
            unsafe { CFRetained::cast_unchecked(list) };
        let mut result = Vec::new();
        for dict in list.iter() {
            if number(&dict, "kCGWindowLayer") != Some(0.0) {
                continue;
            }
            let Some(id) =
                number(&dict, "kCGWindowNumber").filter(|v| *v > 0.0 && *v <= f64::from(u32::MAX))
            else {
                continue;
            };
            let Some(bounds) = dict
                .get(CFString::from_str("kCGWindowBounds").as_ref())
                .and_then(dictionary)
            else {
                continue;
            };
            let Some(frame) = (|| {
                Some(DesktopRect {
                    x: number(&bounds, "X")?,
                    y: number(&bounds, "Y")?,
                    width: number(&bounds, "Width")?,
                    height: number(&bounds, "Height")?,
                })
            })() else {
                continue;
            };
            if frame.validate().is_err() {
                continue;
            }
            let on_screen = dict
                .get(CFString::from_str("kCGWindowIsOnscreen").as_ref())
                .and_then(|v| v.downcast::<CFBoolean>().ok())
                .is_some_and(|v| v.as_bool());
            result.push(WindowDescriptor {
                id: id as u32,
                owner: string(&dict, "kCGWindowOwnerName"),
                title: string(&dict, "kCGWindowName"),
                frame,
                on_screen,
            });
        }
        Ok(result)
    }
}

pub struct NativeOverlayWindow {
    window: Retained<NSWindow>,
    state: OverlayState,
    requested_visible: bool,
    original_style: NSWindowStyleMask,
    original_level: isize,
    original_opaque: bool,
    original_shadow: bool,
    original_mouse: bool,
    original_collection: NSWindowCollectionBehavior,
    original_background: Option<Retained<NSColor>>,
    original_frame: NSRect,
    original_visible: bool,
}
impl NativeOverlayWindow {
    pub fn attach(window_id: u32, target: OverlayTarget) -> PlatformResult<Self> {
        if target == OverlayTarget::Window(window_id) {
            return Err(PlatformError("An overlay cannot track its own window"));
        }
        let mtm = main_thread()?;
        let window = NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .find(|w| w.windowNumber() == window_id as isize)
            .ok_or(PlatformError(
                "Overlay requires an application-owned native window",
            ))?;
        let frame = desktop::target_frame(
            target,
            &NativeDesktop.displays()?,
            &NativeDesktop.windows()?,
        )?;
        let value = Self {
            original_style: window.styleMask(),
            original_level: window.level(),
            original_opaque: window.isOpaque(),
            original_shadow: window.hasShadow(),
            original_mouse: window.ignoresMouseEvents(),
            original_collection: window.collectionBehavior(),
            original_background: Some(window.backgroundColor()),
            original_frame: window.frame(),
            original_visible: window.isVisible(),
            window,
            state: OverlayState {
                target,
                frame,
                visible: false,
                input_mode: OverlayInputMode::ClickThrough,
            },
            requested_visible: false,
        };
        value.window.setStyleMask(NSWindowStyleMask::Borderless);
        value.window.setOpaque(false);
        value.window.setHasShadow(false);
        value
            .window
            .setBackgroundColor(Some(&NSColor::clearColor()));
        value.window.setLevel(25); // NSStatusWindowLevel, above ordinary application windows.
        value.window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
        value.window.setIgnoresMouseEvents(true);
        value.window.orderOut(None);
        value.apply_frame();
        Ok(value)
    }
    fn apply_frame(&self) {
        let top = CGDisplayBounds(CGMainDisplayID()).size.height;
        let r = self.state.frame;
        self.window.setFrame_display(
            NSRect::new(
                NSPoint::new(r.x, top - r.y - r.height),
                NSSize::new(r.width, r.height),
            ),
            true,
        );
    }
}
impl OverlayWindow for NativeOverlayWindow {
    fn state(&self) -> OverlayState {
        self.state
    }
    fn set_target(&mut self, target: OverlayTarget) -> PlatformResult<()> {
        if target == OverlayTarget::Window(self.window.windowNumber() as u32) {
            return Err(PlatformError("An overlay cannot track its own window"));
        }
        let frame = desktop::target_frame(
            target,
            &NativeDesktop.displays()?,
            &NativeDesktop.windows()?,
        )?;
        self.state.target = target;
        self.state.frame = frame;
        self.apply_frame();
        self.refresh().map(|_| ())
    }
    fn set_visible(&mut self, visible: bool) -> PlatformResult<()> {
        self.requested_visible = visible;
        self.refresh().map(|_| ())
    }
    fn set_input_mode(&mut self, mode: OverlayInputMode) -> PlatformResult<()> {
        self.window
            .setIgnoresMouseEvents(mode == OverlayInputMode::ClickThrough);
        self.state.input_mode = mode;
        Ok(())
    }
    fn refresh(&mut self) -> PlatformResult<bool> {
        let old = self.state;
        match desktop::target_frame(
            self.state.target,
            &NativeDesktop.displays()?,
            &NativeDesktop.windows()?,
        ) {
            Ok(frame) => {
                self.state.frame = frame;
                self.state.visible = self.requested_visible;
                self.apply_frame();
            }
            Err(_) => self.state.visible = false,
        }
        if self.state.visible {
            self.window.orderFrontRegardless();
        } else {
            self.window.orderOut(None);
        }
        Ok(self.state != old)
    }
}
impl Drop for NativeOverlayWindow {
    fn drop(&mut self) {
        self.window.setIgnoresMouseEvents(self.original_mouse);
        self.window.setCollectionBehavior(self.original_collection);
        self.window.setLevel(self.original_level);
        self.window.setOpaque(self.original_opaque);
        self.window.setHasShadow(self.original_shadow);
        self.window
            .setBackgroundColor(self.original_background.as_deref());
        self.window.setStyleMask(self.original_style);
        self.window.setFrame_display(self.original_frame, true);
        if self.original_visible {
            self.window.orderFrontRegardless();
        } else {
            self.window.orderOut(None);
        }
    }
}
