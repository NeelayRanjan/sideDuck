//! Thin native PDF and ScreenCaptureKit translation. PDF raster tests use local
//! synthetic source bytes. UNVERIFIED: real screen capture, OS consent dialog,
//! protected-window handling, and physical Sidecar pixels require user testing.

use crate::*;
use block2::RcBlock;
use objc2::{AnyThread, MainThreadMarker};
use objc2_core_foundation::{CFData, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::*;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

pub struct NativePdfRasterizer;
fn pdf(source: &[u8]) -> PlatformResult<CFRetained<CGPDFDocument>> {
    if source.len() > 64 * 1024 * 1024 || !source.starts_with(b"%PDF-") {
        return Err(PlatformError(
            "PDF source is invalid or exceeds the 64 MiB source limit",
        ));
    }
    let data = CFData::from_bytes(source);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or(PlatformError("Could not create PDF data provider"))?;
    let document = CGPDFDocument::with_provider(Some(&provider))
        .ok_or(PlatformError("Native PDF parser rejected the document"))?;
    if !CGPDFDocument::is_unlocked(Some(&document)) {
        return Err(PlatformError(
            "Password-protected PDF must be unlocked before import",
        ));
    }
    Ok(document)
}
fn page(
    source: &[u8],
    index: u32,
) -> PlatformResult<(CFRetained<CGPDFDocument>, CFRetained<CGPDFPage>)> {
    let document = pdf(source)?;
    let page = CGPDFDocument::page(Some(&document), index as usize + 1)
        .ok_or(PlatformError("PDF page is out of range"))?;
    Ok((document, page))
}
fn cg_rect(size: [u32; 2]) -> CGRect {
    CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(f64::from(size[0]), f64::from(size[1])),
    )
}
struct Bitmap<'a> {
    context: CFRetained<CGContext>,
    _pixels: &'a mut [u8],
}
impl std::ops::Deref for Bitmap<'_> {
    type Target = CGContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}
/// The lifetime prevents reading or reallocating pixels until the context drops.
fn bitmap(pixels: &mut [u8], size: [u32; 2]) -> PlatformResult<Bitmap<'_>> {
    if image_bytes(size, pixels.len())? != pixels.len() {
        return Err(PlatformError("Native bitmap buffer has the wrong length"));
    }
    // SAFETY: This global is a CoreGraphics constant; it is never mutated.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(PlatformError("sRGB color space is unavailable"))?;
    // SAFETY: Caller owns width*height*4 initialized bytes, packed RGBA with an
    // exact stride. The context is dropped before the buffer is read or moved.
    let context = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            size[0] as usize,
            size[1] as usize,
            8,
            size[0] as usize * 4,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0,
        )
    }
    .ok_or(PlatformError("Could not allocate native bitmap context"))?;
    Ok(Bitmap {
        context,
        _pixels: pixels,
    })
}
impl PdfRasterizer for NativePdfRasterizer {
    fn page_count(&self, source: &[u8]) -> PlatformResult<u32> {
        u32::try_from(CGPDFDocument::number_of_pages(Some(&*pdf(source)?)))
            .map_err(|_| PlatformError("PDF has too many pages"))
    }
    fn page_size(&self, source: &[u8], index: u32) -> PlatformResult<[f64; 2]> {
        let (_document, page) = page(source, index)?;
        let rect = CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox);
        let mut size = [rect.size.width, rect.size.height];
        if CGPDFPage::rotation_angle(Some(&page)).rem_euclid(180) == 90 {
            size.swap(0, 1);
        }
        if !size.into_iter().all(|v| v.is_finite() && v > 0.0) {
            return Err(PlatformError("PDF page dimensions are invalid"));
        }
        Ok(size)
    }
    fn render_page(
        &self,
        source: &[u8],
        index: u32,
        size: [u32; 2],
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage> {
        let length = image_bytes(size, maximum_bytes)?;
        let (_document, page) = page(source, index)?;
        let mut rgba8 = vec![255; length];
        let context = bitmap(&mut rgba8, size)?;
        let bounds = CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox);
        let mut page_size = [bounds.size.width, bounds.size.height];
        if CGPDFPage::rotation_angle(Some(&page)).rem_euclid(180) == 90 {
            page_size.swap(0, 1);
        }
        let (scale, offset) = desktop::page_fit(page_size, size)?;
        // The native page transform can leave an enlarged destination unscaled.
        // Apply zoom explicitly; ask it only for crop-origin/rotation mapping.
        CGContext::translate_ctm(Some(&context), offset[0], offset[1]);
        CGContext::scale_ctm(Some(&context), scale, scale);
        let transform = CGPDFPage::drawing_transform(
            Some(&page),
            CGPDFBox::CropBox,
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(page_size[0], page_size[1]),
            ),
            0,
            true,
        );
        CGContext::concat_ctm(Some(&context), transform);
        CGContext::clip_to_rect(Some(&context), bounds);
        CGContext::draw_pdf_page(Some(&context), Some(&page));
        drop(context);
        // CoreGraphics writes the first bitmap row at the visual top. The native
        // orientation/rotation tests assert this independently with colored marks.
        Ok(CapturedImage {
            width: size[0],
            height: size[1],
            rgba8,
        })
    }
}

fn image_pixels(image: &CGImage, maximum_bytes: usize) -> PlatformResult<CapturedImage> {
    let size = [
        u32::try_from(CGImage::width(Some(image)))
            .map_err(|_| PlatformError("Captured image width exceeds supported range"))?,
        u32::try_from(CGImage::height(Some(image)))
            .map_err(|_| PlatformError("Captured image height exceeds supported range"))?,
    ];
    let mut rgba8 = vec![0; image_bytes(size, maximum_bytes)?];
    let context = bitmap(&mut rgba8, size)?;
    CGContext::draw_image(Some(&context), cg_rect(size), Some(image));
    drop(context);
    Ok(CapturedImage {
        width: size[0],
        height: size[1],
        rgba8,
    })
}

#[derive(Default)]
pub struct NativeScreenCapture {
    busy: Arc<AtomicBool>,
}
impl ScreenCapture for NativeScreenCapture {
    fn permission_status(&self) -> CapturePermission {
        if CGPreflightScreenCaptureAccess() {
            CapturePermission::Granted
        } else {
            CapturePermission::NotGranted
        }
    }
    fn request_permission(&mut self) -> PlatformResult<CapturePermission> {
        Ok(if CGRequestScreenCaptureAccess() {
            CapturePermission::Granted
        } else {
            CapturePermission::NotGranted
        })
    }
    fn capture(
        &mut self,
        target: CaptureTarget,
        maximum_bytes: usize,
    ) -> PlatformResult<CapturedImage> {
        if MainThreadMarker::new().is_some() {
            return Err(PlatformError("Run screen capture on a worker thread"));
        }
        if self.permission_status() != CapturePermission::Granted {
            return Err(PlatformError(
                "Screen Recording permission is required for capture",
            ));
        }
        if maximum_bytes < 4 {
            return Err(PlatformError("Capture byte budget is too small"));
        }
        if objc2::runtime::AnyClass::get(c"SCScreenshotManager").is_none() {
            return Err(PlatformError("Still capture requires macOS 14 or newer"));
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(PlatformError("A native capture is still in progress"));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let busy = self.busy.clone();
        let handler = RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                // SAFETY: ScreenCaptureKit supplies these live objects for this
                // callback. Helpers retain their filter/config until the screenshot
                // request has copied them; no borrowed native value leaves the call.
                let result = unsafe {
                    if !error.is_null() || content.is_null() {
                        Err(PlatformError(
                            "ScreenCaptureKit could not enumerate capture sources",
                        ))
                    } else {
                        start_capture(
                            &*content,
                            target,
                            maximum_bytes,
                            sender.clone(),
                            busy.clone(),
                        )
                    }
                };
                if let Err(error) = result {
                    let _ = sender.send(Err(error));
                    busy.store(false, Ordering::Release);
                }
            },
        );
        // SAFETY: The copied block owns all callback state; the OS invokes it on
        // its own queue. It cannot capture/reuse a caller stack reference.
        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&handler);
        }
        // A timed-out request keeps the per-service busy flag set until the OS
        // callback completes, so repeated timeouts cannot pile up allocations.
        receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| {
                PlatformError(
                    "Native screen capture timed out; the request may still be completing",
                )
            })?
    }
}
unsafe fn start_capture(
    content: &SCShareableContent,
    target: CaptureTarget,
    maximum_bytes: usize,
    sender: mpsc::SyncSender<PlatformResult<CapturedImage>>,
    busy: Arc<AtomicBool>,
) -> PlatformResult<()> {
    let configuration = SCStreamConfiguration::new();
    configuration.setShowsCursor(false);
    configuration.setQueueDepth(1);
    let filter = match target {
        CaptureTarget::Window(id) => {
            let window = content
                .windows()
                .iter()
                .find(|w| w.windowID() == id)
                .ok_or(PlatformError("Capture window is no longer available"))?;
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
        }
        CaptureTarget::Display(id) => {
            let display = content
                .displays()
                .iter()
                .find(|d| d.displayID() == id)
                .ok_or(PlatformError("Capture display is no longer available"))?;
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            )
        }
        CaptureTarget::Region(region) => {
            region.validate()?;
            let display = content
                .displays()
                .iter()
                .find(|d| crate::macos_desktop::from_cg(d.frame()).contains(region))
                .ok_or(PlatformError("Capture region must fit within one display"))?;
            let frame = display.frame();
            configuration.setSourceRect(CGRect::new(
                CGPoint::new(region.x - frame.origin.x, region.y - frame.origin.y),
                CGSize::new(region.width, region.height),
            ));
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            )
        }
    };
    let rect = match target {
        CaptureTarget::Region(rect) => rect,
        _ => crate::macos_desktop::from_cg(filter.contentRect()),
    };
    rect.validate()?;
    let scale = f64::from(filter.pointPixelScale());
    let width = (rect.width * scale).ceil();
    let height = (rect.height * scale).ceil();
    if !scale.is_finite()
        || scale <= 0.0
        || width > f64::from(u32::MAX)
        || height > f64::from(u32::MAX)
    {
        return Err(PlatformError("Capture dimensions are invalid"));
    }
    let size = [width as u32, height as u32];
    image_bytes(size, maximum_bytes)?;
    configuration.setWidth(size[0] as usize);
    configuration.setHeight(size[1] as usize);
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let result = if !error.is_null() || image.is_null() {
            Err(PlatformError("ScreenCaptureKit did not produce an image"))
        } else {
            // SAFETY: Image is borrowed for this callback only. Copy its
            // color-converted pixels before returning to ScreenCaptureKit.
            image_pixels(unsafe { &*image }, maximum_bytes)
        };
        let _ = sender.send(result);
        busy.store(false, Ordering::Release);
    });
    SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
        &filter,
        &configuration,
        Some(&handler),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source_pdf(rotation: i32) -> Vec<u8> {
        let stream =
            "1 0 0 rg 0 75 100 25 re f\n0 0 1 rg 0 0 25 25 re f\n0 0 0 rg 50 25 0.5 50 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Rotate {rotation} /Resources << >> /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{stream}endstream",stream.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        pdf
    }
    fn pixel(image: &CapturedImage, x: usize, y: usize) -> &[u8] {
        let i = (y * image.width as usize + x) * 4;
        &image.rgba8[i..i + 4]
    }
    #[test]
    fn native_pdf_source_rerenders_crisply_across_zoom_levels_and_preserves_orientation() {
        let renderer = NativePdfRasterizer;
        let source = source_pdf(0);
        let original = source.clone();
        assert_eq!(renderer.page_count(&source).unwrap(), 1);
        assert_eq!(renderer.page_size(&source, 0).unwrap(), [100.0, 100.0]);
        let image = renderer
            .render_page(&source, 0, [100, 100], 40_000)
            .unwrap();
        assert_eq!(pixel(&image, 10, 10), &[255, 0, 0, 255]);
        assert_eq!(pixel(&image, 10, 90), &[0, 0, 255, 255]);
        assert_eq!(pixel(&image, 90, 90), &[255, 255, 255, 255]);
        let enlarged = renderer
            .render_page(&source, 0, [400, 400], 640_000)
            .unwrap();
        assert_eq!(pixel(&enlarged, 201, 200), &[0, 0, 0, 255]);
        assert_eq!(pixel(&enlarged, 203, 200), &[255, 255, 255, 255]);
        assert_eq!(source, original);
        assert!(renderer
            .render_page(&source, 0, [400, 400], 639_999)
            .is_err());
        assert!(renderer.render_page(&source, 1, [1, 1], 4).is_err());
        assert!(renderer.page_count(b"not a PDF").is_err());
    }
    #[test]
    fn native_bitmap_conversion_preserves_rgba_premultiplication_and_row_order() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 128, 128, 0, 0, 0, 0];
        let mut storage = pixels;
        let context = bitmap(&mut storage, [2, 2]).unwrap();
        let image = CGBitmapContextCreateImage(Some(&context)).unwrap();
        let converted = image_pixels(&image, 16).unwrap();
        assert_eq!(converted.rgba8, pixels);
        assert!(image_pixels(&image, 15).is_err());
        drop(context);
        assert!(bitmap(&mut [0; 3], [1, 1]).is_err());
    }
    #[test]
    fn native_pdf_rotation_is_applied_by_page_transform() {
        let renderer = NativePdfRasterizer;
        let image = renderer
            .render_page(&source_pdf(90), 0, [100, 100], 40_000)
            .unwrap();
        assert_eq!(pixel(&image, 90, 10), &[255, 0, 0, 255]);
        assert_eq!(pixel(&image, 10, 10), &[0, 0, 255, 255]);
    }
}
