use crate::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Shared mutable desktop fixture lets tests move/close windows while an overlay
/// remains attached. No native event loop or display is required.
#[derive(Clone, Default)]
pub struct MockDesktop {
    pub displays: Arc<Mutex<Vec<DisplayDescriptor>>>,
    pub windows: Arc<Mutex<Vec<WindowDescriptor>>>,
}
impl DisplayInfo for MockDesktop {
    fn displays(&self) -> PlatformResult<Vec<DisplayDescriptor>> {
        Ok(self
            .displays
            .lock()
            .map_err(|_| PlatformError("Mock display state poisoned"))?
            .clone())
    }
}
impl WindowInfo for MockDesktop {
    fn windows(&self) -> PlatformResult<Vec<WindowDescriptor>> {
        Ok(self
            .windows
            .lock()
            .map_err(|_| PlatformError("Mock window state poisoned"))?
            .clone())
    }
}
pub struct MockOverlayWindow {
    desktop: MockDesktop,
    state: OverlayState,
    requested_visible: bool,
}
impl MockOverlayWindow {
    pub fn new(desktop: MockDesktop, target: OverlayTarget) -> PlatformResult<Self> {
        let frame = desktop::target_frame(target, &desktop.displays()?, &desktop.windows()?)?;
        Ok(Self {
            desktop,
            state: OverlayState {
                target,
                frame,
                visible: false,
                input_mode: OverlayInputMode::ClickThrough,
            },
            requested_visible: false,
        })
    }
}
impl OverlayWindow for MockOverlayWindow {
    fn state(&self) -> OverlayState {
        self.state
    }
    fn set_target(&mut self, target: OverlayTarget) -> PlatformResult<()> {
        let frame =
            desktop::target_frame(target, &self.desktop.displays()?, &self.desktop.windows()?)?;
        self.state.target = target;
        self.state.frame = frame;
        self.state.visible = self.requested_visible;
        Ok(())
    }
    fn set_visible(&mut self, visible: bool) -> PlatformResult<()> {
        self.requested_visible = visible;
        self.refresh().map(|_| ())
    }
    fn set_input_mode(&mut self, mode: OverlayInputMode) -> PlatformResult<()> {
        self.state.input_mode = mode;
        Ok(())
    }
    fn refresh(&mut self) -> PlatformResult<bool> {
        let old = self.state;
        match desktop::target_frame(
            self.state.target,
            &self.desktop.displays()?,
            &self.desktop.windows()?,
        ) {
            Ok(frame) => {
                self.state.frame = frame;
                self.state.visible = self.requested_visible;
            }
            Err(_) => self.state.visible = false,
        }
        Ok(self.state != old)
    }
}

pub struct MockScreenCapture {
    pub permission: CapturePermission,
    pub grant_on_request: bool,
    pub permission_requests: usize,
    pub captures: usize,
    pub desktop: MockDesktop,
    pub display_images: BTreeMap<u32, CapturedImage>,
    pub window_images: BTreeMap<u32, CapturedImage>,
}
impl Default for MockScreenCapture {
    fn default() -> Self {
        Self {
            permission: CapturePermission::NotGranted,
            grant_on_request: false,
            permission_requests: 0,
            captures: 0,
            desktop: MockDesktop::default(),
            display_images: BTreeMap::new(),
            window_images: BTreeMap::new(),
        }
    }
}
impl ScreenCapture for MockScreenCapture {
    fn permission_status(&self) -> CapturePermission {
        self.permission
    }
    fn request_permission(&mut self) -> PlatformResult<CapturePermission> {
        self.permission_requests += 1;
        if self.grant_on_request {
            self.permission = CapturePermission::Granted;
        }
        Ok(self.permission)
    }
    fn capture(
        &mut self,
        target: CaptureTarget,
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage> {
        if self.permission != CapturePermission::Granted {
            return Err(PlatformError(
                "Screen Recording permission is required for capture",
            ));
        }
        let image = match target {
            CaptureTarget::Display(id) => self
                .display_images
                .get(&id)
                .ok_or(PlatformError("Capture display not found"))?,
            CaptureTarget::Window(id) => self
                .window_images
                .get(&id)
                .ok_or(PlatformError("Capture window not found"))?,
            CaptureTarget::Region(region) => {
                region.validate()?;
                let displays = self.desktop.displays()?;
                let display = displays
                    .iter()
                    .find(|d| d.frame.contains(region))
                    .ok_or(PlatformError("Capture region must fit within one display"))?;
                let source = self
                    .display_images
                    .get(&display.id)
                    .ok_or(PlatformError("Capture display not found"))?;
                source.validate(usize::MAX)?;
                let left = ((region.x - display.frame.x) / display.frame.width
                    * f64::from(source.width))
                .floor()
                .clamp(0.0, f64::from(source.width)) as u32;
                let top = ((region.y - display.frame.y) / display.frame.height
                    * f64::from(source.height))
                .floor()
                .clamp(0.0, f64::from(source.height)) as u32;
                let right = ((region.x + region.width - display.frame.x) / display.frame.width
                    * f64::from(source.width))
                .ceil()
                .min(f64::from(source.width)) as u32;
                let bottom = ((region.y + region.height - display.frame.y) / display.frame.height
                    * f64::from(source.height))
                .ceil()
                .min(f64::from(source.height)) as u32;
                let size = [right - left, bottom - top];
                let mut rgba8 = vec![0; image_bytes(size, maximum_bytes)?];
                for row in 0..size[1] as usize {
                    let start = ((top as usize + row) * source.width as usize + left as usize) * 4;
                    let destination = row * size[0] as usize * 4;
                    rgba8[destination..destination + size[0] as usize * 4]
                        .copy_from_slice(&source.rgba8[start..start + size[0] as usize * 4]);
                }
                self.captures += 1;
                return Ok(CapturedImage {
                    width: size[0],
                    height: size[1],
                    rgba8,
                });
            }
        };
        image.validate(maximum_bytes)?;
        self.captures += 1;
        Ok(image.clone())
    }
}

#[derive(Clone)]
pub struct MockPdfPage {
    pub size: [f64; 2],
    pub rectangles: Vec<(DesktopRect, [u8; 4])>,
}
#[derive(Default)]
pub struct MockPdfRasterizer {
    pub documents: BTreeMap<Vec<u8>, Vec<MockPdfPage>>,
}
impl MockPdfRasterizer {
    fn pages(&self, source: &[u8]) -> PlatformResult<&[MockPdfPage]> {
        self.documents
            .get(source)
            .map(Vec::as_slice)
            .ok_or(PlatformError("PDF source is not registered in the mock"))
    }
}
impl PdfRasterizer for MockPdfRasterizer {
    fn page_count(&self, source: &[u8]) -> PlatformResult<u32> {
        u32::try_from(self.pages(source)?.len())
            .map_err(|_| PlatformError("PDF page count exceeds supported range"))
    }
    fn page_size(&self, source: &[u8], page: u32) -> PlatformResult<[f64; 2]> {
        self.pages(source)?
            .get(page as usize)
            .map(|p| p.size)
            .ok_or(PlatformError("PDF page is out of range"))
    }
    fn render_page(
        &self,
        source: &[u8],
        page: u32,
        size: [u32; 2],
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage> {
        let page = self
            .pages(source)?
            .get(page as usize)
            .ok_or(PlatformError("PDF page is out of range"))?;
        let (scale, offset) = desktop::page_fit(page.size, size)?;
        let mut image = CapturedImage {
            width: size[0],
            height: size[1],
            rgba8: vec![255; image_bytes(size, maximum_bytes)?],
        };
        for (rect, color) in &page.rectangles {
            rect.validate()?;
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let px = (f64::from(x) + 0.5 - offset[0]) / scale;
                    let py = (f64::from(y) + 0.5 - offset[1]) / scale;
                    if px >= 0.0
                        && px < page.size[0]
                        && py >= 0.0
                        && py < page.size[1]
                        && px >= rect.x
                        && px < rect.x + rect.width
                        && py >= rect.y
                        && py < rect.y + rect.height
                    {
                        let offset = (y as usize * size[0] as usize + x as usize) * 4;
                        for (channel, value) in color.iter().enumerate().take(3) {
                            image.rgba8[offset + channel] = (*value as u16
                                + (image.rgba8[offset + channel] as u16 * (255 - color[3]) as u16
                                    + 127)
                                    / 255)
                                .min(255)
                                as u8;
                        }
                    }
                }
            }
        }
        Ok(image)
    }
}
