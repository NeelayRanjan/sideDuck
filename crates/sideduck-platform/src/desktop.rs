use crate::PlatformError;

pub type PlatformResult<T> = Result<T, PlatformError>;

/// Global desktop coordinates in logical points, top-left origin. Negative
/// coordinates are valid for displays arranged left of or above the main display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl DesktopRect {
    pub fn validate(self) -> PlatformResult<()> {
        if ![
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(PlatformError(
                "Desktop rectangle must have finite positive dimensions",
            ));
        }
        Ok(())
    }
    pub fn contains(self, other: Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayDescriptor {
    pub id: u32,
    pub name: String,
    pub frame: DesktopRect,
    pub pixel_size: [u32; 2],
    pub scale_factor: f64,
    pub primary: bool,
    pub built_in: bool,
    /// None means the public native API does not establish whether this is Sidecar.
    pub sidecar: Option<bool>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct WindowDescriptor {
    pub id: u32,
    pub owner: String,
    pub title: String,
    pub frame: DesktopRect,
    pub on_screen: bool,
}
pub trait DisplayInfo {
    fn displays(&self) -> PlatformResult<Vec<DisplayDescriptor>>;
}
pub trait WindowInfo {
    fn windows(&self) -> PlatformResult<Vec<WindowDescriptor>>;
}

pub fn select_canvas_display(
    displays: &[DisplayDescriptor],
    override_id: Option<u32>,
) -> PlatformResult<&DisplayDescriptor> {
    if let Some(id) = override_id {
        return displays.iter().find(|d| d.id == id).ok_or(PlatformError(
            "Selected canvas display is no longer connected",
        ));
    }
    displays
        .iter()
        .find(|d| d.sidecar == Some(true))
        .or_else(|| displays.iter().find(|d| d.primary))
        .or_else(|| displays.first())
        .ok_or(PlatformError("No connected display is available"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePermission {
    Granted,
    NotGranted,
}
pub const CAPTURE_PERMISSION_EXPLANATION: &str = "Capture a display, window, or region into a local document. macOS requires Screen Recording permission to read those pixels. Live drawing overlays do not require this permission. Captures stay on this Mac.";
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaptureTarget {
    Display(u32),
    Window(u32),
    Region(DesktopRect),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    /// Top-left row order, RGBA8 with premultiplied alpha in encoded sRGB.
    /// Importers must unpremultiply, decode sRGB, then premultiply in linear space.
    pub rgba8: Vec<u8>,
}
impl CapturedImage {
    pub fn validate(&self, maximum_bytes: usize) -> PlatformResult<()> {
        if image_bytes([self.width, self.height], maximum_bytes)? != self.rgba8.len() {
            return Err(PlatformError(
                "Image dimensions do not match its pixel buffer",
            ));
        }
        Ok(())
    }
}
pub fn image_bytes(size: [u32; 2], maximum_bytes: usize) -> PlatformResult<usize> {
    if size.contains(&0) {
        return Err(PlatformError("Image dimensions must be positive"));
    }
    let bytes = (size[0] as usize)
        .checked_mul(size[1] as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(PlatformError(
            "Image dimensions overflow addressable memory",
        ))?;
    if bytes > maximum_bytes {
        return Err(PlatformError("Image exceeds the configured byte budget"));
    }
    Ok(bytes)
}
/// Capture is explicit and synchronous. Run it on a worker; native capture rejects
/// the UI thread. Merely constructing this service never requests permission.
pub trait ScreenCapture: Send {
    fn permission_status(&self) -> CapturePermission;
    /// Call only after an explicit capture/permission UI action and explanation.
    fn request_permission(&mut self) -> PlatformResult<CapturePermission>;
    fn capture(
        &mut self,
        target: CaptureTarget,
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayTarget {
    Display(u32),
    Window(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayInputMode {
    /// All pointer events pass through to the underlying applications.
    ClickThrough,
    /// The window accepts pointer events; application routing paints stylus only.
    Draw,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayState {
    pub target: OverlayTarget,
    pub frame: DesktopRect,
    pub visible: bool,
    pub input_mode: OverlayInputMode,
}
/// Configures an application-owned canvas window. Rendering and document data
/// remain portable; this trait only changes native geometry/window properties.
pub trait OverlayWindow {
    fn state(&self) -> OverlayState;
    fn set_target(&mut self, target: OverlayTarget) -> PlatformResult<()>;
    fn set_visible(&mut self, visible: bool) -> PlatformResult<()>;
    fn set_input_mode(&mut self, mode: OverlayInputMode) -> PlatformResult<()>;
    /// Call when desktop/window geometry changes. Missing targets hide the overlay.
    fn refresh(&mut self) -> PlatformResult<bool>;
}

/// Immutable PDF source bytes are supplied for each render, so zoom never scales
/// a previously rasterized bitmap. Page indices are zero-based. Output preserves
/// aspect ratio and centers the cropped/rotated page on a white background.
pub trait PdfRasterizer: Send {
    fn page_count(&self, source: &[u8]) -> PlatformResult<u32>;
    fn page_size(&self, source: &[u8], page: u32) -> PlatformResult<[f64; 2]>;
    fn render_page(
        &self,
        source: &[u8],
        page: u32,
        output_size: [u32; 2],
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage>;
}

/// Aspect-preserving fit in a requested raster; shared by native and mock paths.
pub(crate) fn page_fit(
    page_size: [f64; 2],
    output_size: [u32; 2],
) -> PlatformResult<(f64, [f64; 2])> {
    if !page_size.into_iter().all(|v| v.is_finite() && v > 0.0) || output_size.contains(&0) {
        return Err(PlatformError("PDF page dimensions are invalid"));
    }
    let scale =
        (f64::from(output_size[0]) / page_size[0]).min(f64::from(output_size[1]) / page_size[1]);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(PlatformError("PDF render scale is invalid"));
    }
    Ok((
        scale,
        std::array::from_fn(|axis| (f64::from(output_size[axis]) - page_size[axis] * scale) / 2.0),
    ))
}

pub(crate) fn target_frame(
    target: OverlayTarget,
    displays: &[DisplayDescriptor],
    windows: &[WindowDescriptor],
) -> PlatformResult<DesktopRect> {
    let frame = match target {
        OverlayTarget::Display(id) => displays.iter().find(|d| d.id == id).map(|d| d.frame),
        OverlayTarget::Window(id) => windows
            .iter()
            .find(|w| w.id == id && w.on_screen)
            .map(|w| w.frame),
    }
    .ok_or(PlatformError("Overlay target is not currently visible"))?;
    frame.validate()?;
    Ok(frame)
}
