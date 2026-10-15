//! Annotation/capture orchestration. Native work stays behind platform traits;
//! document import streams one tile at a time into the durable operation log.
use crate::core::{EditorCore, Result};
use sideduck_composite::color::srgb_to_linear;
use sideduck_doc::{Layer, Operation, TileAddress, TileEdit};
use sideduck_platform::{
    CapturePermission, CaptureTarget, CapturedImage, DesktopRect, DisplayInfo, OverlayInputMode,
    OverlayTarget, OverlayWindow, PlatformResult, ScreenCapture, WindowInfo,
};
use sideduck_ui::editor::AnnotationSnapshot;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

type OverlayFactory = Box<dyn Fn(u32, OverlayTarget) -> PlatformResult<Box<dyn OverlayWindow>>>;
type CaptureCompletion = (Box<dyn ScreenCapture>, Result<(CapturedImage, PathBuf)>);
pub struct AnnotationController {
    displays: Box<dyn DisplayInfo>,
    windows: Box<dyn WindowInfo>,
    capture: Option<Box<dyn ScreenCapture>>,
    overlay_factory: OverlayFactory,
    overlay: Option<Box<dyn OverlayWindow>>,
    pending_overlay: Option<OverlayTarget>,
    snapshot: AnnotationSnapshot,
    pending_canvas_frame: Option<DesktopRect>,
    capture_result: Option<Receiver<CaptureCompletion>>,
    capture_worker: Option<JoinHandle<()>>,
}
impl AnnotationController {
    pub fn new() -> Self {
        Self::with_services(
            sideduck_platform::create_display_info(),
            sideduck_platform::create_window_info(),
            sideduck_platform::create_screen_capture(),
            Box::new(sideduck_platform::attach_overlay),
        )
    }
    pub fn with_services(
        displays: Box<dyn DisplayInfo>,
        windows: Box<dyn WindowInfo>,
        capture: Box<dyn ScreenCapture>,
        overlay_factory: OverlayFactory,
    ) -> Self {
        Self {
            displays,
            windows,
            capture: Some(capture),
            overlay_factory,
            overlay: None,
            pending_overlay: None,
            snapshot: AnnotationSnapshot::default(),
            pending_canvas_frame: None,
            capture_result: None,
            capture_worker: None,
        }
    }
    pub fn snapshot(&self) -> AnnotationSnapshot {
        self.snapshot.clone()
    }
    pub fn refresh_desktop(&mut self) -> Result<()> {
        let displays = self.displays.displays().map_err(|e| e.to_string())?;
        let windows = self.windows.windows().map_err(|e| e.to_string())?;
        self.snapshot.displays = displays;
        self.snapshot.windows = windows;
        if self.snapshot.canvas_display.is_none() {
            self.snapshot.canvas_display =
                sideduck_platform::select_canvas_display(&self.snapshot.displays, None)
                    .ok()
                    .map(|d| d.id);
        }
        Ok(())
    }
    pub fn select_canvas_display(&mut self, id: u32) -> Result<()> {
        let display = sideduck_platform::select_canvas_display(&self.snapshot.displays, Some(id))
            .map_err(|e| e.to_string())?;
        self.pending_canvas_frame = Some(display.frame);
        self.snapshot.canvas_display = Some(id);
        Ok(())
    }
    pub fn take_canvas_frame(&mut self) -> Option<DesktopRect> {
        self.pending_canvas_frame.take()
    }
    pub fn active(&self) -> bool {
        self.pending_overlay.is_some() || self.overlay.is_some()
    }
    pub fn begin_overlay(&mut self, target: OverlayTarget) -> Result<[u32; 2]> {
        self.refresh_desktop()?;
        let size = match target {
            OverlayTarget::Display(id) => self
                .snapshot
                .displays
                .iter()
                .find(|d| d.id == id)
                .map(|d| d.pixel_size)
                .ok_or("Choose a connected display first")?,
            OverlayTarget::Window(id) => {
                let window = self
                    .snapshot
                    .windows
                    .iter()
                    .find(|w| w.id == id && w.on_screen)
                    .ok_or("Choose a visible window first")?;
                let scale = self
                    .snapshot
                    .displays
                    .iter()
                    .find(|d| d.frame.contains(window.frame))
                    .map_or(1.0, |d| d.scale_factor);
                let size = [window.frame.width * scale, window.frame.height * scale];
                if !size
                    .into_iter()
                    .all(|v| v.is_finite() && (1.0..=16384.0).contains(&v))
                {
                    return Err("Overlay target dimensions exceed the document limit".into());
                }
                size.map(|v| v.ceil() as u32)
            }
        };
        if size.contains(&0) || size.into_iter().any(|v| v > 16384) {
            return Err("Overlay target dimensions exceed the document limit".into());
        }
        self.stop();
        self.pending_overlay = Some(target);
        self.snapshot.overlay_active = true;
        Ok(size)
    }
    pub fn update(&mut self, window_id: Option<u32>) -> Result<()> {
        if let (Some(target), Some(id)) = (self.pending_overlay, window_id) {
            let mut overlay = (self.overlay_factory)(id, target).map_err(|e| e.to_string())?;
            overlay
                .set_input_mode(if self.snapshot.overlay_drawing {
                    OverlayInputMode::Draw
                } else {
                    OverlayInputMode::ClickThrough
                })
                .map_err(|e| e.to_string())?;
            overlay.set_visible(true).map_err(|e| e.to_string())?;
            self.overlay = Some(overlay);
            self.pending_overlay = None;
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.refresh().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn set_drawing(&mut self, drawing: bool) -> Result<()> {
        if let Some(overlay) = &mut self.overlay {
            overlay
                .set_input_mode(if drawing {
                    OverlayInputMode::Draw
                } else {
                    OverlayInputMode::ClickThrough
                })
                .map_err(|e| e.to_string())?;
        }
        self.snapshot.overlay_drawing = drawing;
        Ok(())
    }
    pub fn stop(&mut self) {
        self.overlay = None;
        self.pending_overlay = None;
        self.snapshot.overlay_active = false;
        self.snapshot.overlay_drawing = false;
    }
    /// Only call after the visible permission explanation and explicit confirmation.
    pub fn start_capture(
        &mut self,
        target: CaptureTarget,
        path: PathBuf,
        request_permission: bool,
        maximum_bytes: usize,
    ) -> Result<()> {
        if self.capture_result.is_some() {
            return Err("A capture is already in progress".into());
        }
        if path.exists() {
            return Err("Capture destination already exists; choose a new document path".into());
        }
        let mut service = self
            .capture
            .take()
            .ok_or("Capture service is unavailable")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        self.capture_worker = Some(std::thread::spawn(move || {
            let result = (|| {
                if service.permission_status() != CapturePermission::Granted {
                    if !request_permission {
                        return Err("Screen Recording permission has not been granted".into());
                    }
                    if service.request_permission().map_err(|e| e.to_string())?
                        != CapturePermission::Granted
                    {
                        return Err(
                            "Screen Recording permission was not granted; no capture was made"
                                .into(),
                        );
                    }
                }
                service
                    .capture(target, maximum_bytes)
                    .map(|image| (image, path))
                    .map_err(|e| e.to_string())
            })();
            let _ = sender.send((service, result));
        }));
        self.capture_result = Some(receiver);
        self.snapshot.capture_busy = true;
        Ok(())
    }
    pub fn poll_capture(&mut self) -> Option<Result<(CapturedImage, PathBuf)>> {
        let receiver = self.capture_result.as_ref()?;
        let result = match receiver.try_recv() {
            Ok((service, result)) => {
                self.capture = Some(service);
                result
            }
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Capture worker stopped unexpectedly".into()),
        };
        self.capture_result = None;
        self.snapshot.capture_busy = false;
        if let Some(worker) = self.capture_worker.take() {
            let _ = worker.join();
        }
        Some(result)
    }
}

pub fn annotation_path(current_document: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |v| v.as_nanos());
    current_document
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("annotation-{stamp}.pond"))
}

pub fn capture_to_document(
    image: CapturedImage,
    path: &Path,
    maximum_bytes: usize,
) -> Result<EditorCore> {
    image.validate(maximum_bytes).map_err(|e| e.to_string())?;
    let mut core = EditorCore::create(path, [image.width, image.height])?;
    let edge = core.document.borrow().manifest().tile_size;
    let nx = image.width.div_ceil(edge);
    let ny = image.height.div_ceil(edge);
    let edits = (0..ny)
        .flat_map(|ty| (0..nx).map(move |tx| (tx, ty)))
        .map(|(tx, ty)| {
            let mut tile = vec![0; edge as usize * edge as usize * 4];
            for y in 0..edge.min(image.height - ty * edge) {
                for x in 0..edge.min(image.width - tx * edge) {
                    let source = ((ty * edge + y) as usize * image.width as usize
                        + (tx * edge + x) as usize)
                        * 4;
                    let target = (y as usize * edge as usize + x as usize) * 4;
                    let alpha = image.rgba8[source + 3] as f32 / 255.0;
                    tile[target + 3] = image.rgba8[source + 3];
                    if alpha > 0.0 {
                        for channel in 0..3 {
                            let straight = (image.rgba8[source + channel] as f32 / 255.0 / alpha)
                                .clamp(0.0, 1.0);
                            tile[target + channel] =
                                (srgb_to_linear(straight) * alpha * 255.0).round() as u8;
                        }
                    }
                }
            }
            TileEdit {
                address: TileAddress {
                    layer: 0,
                    x: tx,
                    y: ty,
                },
                pixels: Some(tile),
            }
        });
    core.document
        .borrow_mut()
        .apply(
            Operation::RasterEdit {
                layers: vec![0],
                label: "Capture image".into(),
            },
            edits,
        )
        .map_err(|e| e.to_string())?;
    let mut base = Layer::raster(0, "Captured image");
    base.locked = true;
    core.apply(Operation::SetLayer { layer: base }, vec![])?;
    core.apply(
        Operation::CreateLayer {
            layer: Layer::raster(1, "Annotations"),
            index: 1,
        },
        vec![],
    )?;
    core.apply(Operation::SetActiveLayer { id: Some(1) }, vec![])?;
    core.document
        .borrow_mut()
        .checkpoint()
        .map_err(|e| e.to_string())?;
    core.changed();
    Ok(core)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sideduck_platform::{DisplayDescriptor, MockDesktop, MockOverlayWindow, MockScreenCapture};

    fn controller() -> AnnotationController {
        let desktop = MockDesktop::default();
        desktop.displays.lock().unwrap().push(DisplayDescriptor {
            id: 7,
            name: "Test display".into(),
            frame: DesktopRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
            pixel_size: [200, 100],
            scale_factor: 2.0,
            primary: true,
            built_in: true,
            sidecar: Some(false),
        });
        let mut capture = MockScreenCapture {
            desktop: desktop.clone(),
            grant_on_request: true,
            ..Default::default()
        };
        capture.display_images.insert(
            7,
            CapturedImage {
                width: 1,
                height: 1,
                rgba8: vec![255, 0, 0, 255],
            },
        );
        let overlay_desktop = desktop.clone();
        AnnotationController::with_services(
            Box::new(desktop.clone()),
            Box::new(desktop),
            Box::new(capture),
            Box::new(move |_, target| {
                Ok(Box::new(MockOverlayWindow::new(
                    overlay_desktop.clone(),
                    target,
                )?))
            }),
        )
    }
    #[test]
    fn live_overlay_mock_workflow_chooses_target_and_explicit_input_mode() {
        let mut controller = controller();
        assert!(controller
            .begin_overlay(OverlayTarget::Display(999))
            .is_err());
        assert_eq!(
            controller.begin_overlay(OverlayTarget::Display(7)).unwrap(),
            [200, 100]
        );
        controller.update(None).unwrap();
        assert!(controller.overlay.is_none());
        controller.update(Some(42)).unwrap();
        assert_eq!(
            controller.overlay.as_ref().unwrap().state().input_mode,
            OverlayInputMode::ClickThrough
        );
        controller.set_drawing(true).unwrap();
        assert_eq!(
            controller.overlay.as_ref().unwrap().state().input_mode,
            OverlayInputMode::Draw
        );
        controller.stop();
        assert!(!controller.active());
    }
    #[test]
    fn capture_permission_requires_explicit_confirmation_before_mock_capture() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("capture.pond");
        let mut controller = controller();
        for (confirmed, success) in [(false, false), (true, true)] {
            controller
                .start_capture(CaptureTarget::Display(7), path.clone(), confirmed, 4)
                .unwrap();
            // Joining a mock-only worker avoids wall-clock timing assumptions.
            controller.capture_worker.take().unwrap().join().unwrap();
            let result = controller.poll_capture().unwrap();
            assert_eq!(result.is_ok(), success);
            assert!(!controller.snapshot().capture_busy);
            if let Ok((image, result_path)) = result {
                assert_eq!(image.rgba8, [255, 0, 0, 255]);
                assert_eq!(result_path, path);
            }
        }
        assert!(!path.exists());
    }
    #[test]
    fn capture_to_document_linearizes_pixels_locks_base_and_survives_reopen() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("capture.pond");
        let core = capture_to_document(
            CapturedImage {
                width: 2,
                height: 1,
                rgba8: vec![128, 128, 128, 255, 64, 0, 0, 128],
            },
            &path,
            8,
        )
        .unwrap();
        assert!(core.document.borrow().state().layers[0].locked);
        assert_eq!(core.document.borrow().state().active_layer, Some(1));
        drop(core);
        let reopened = EditorCore::open(&path).unwrap();
        let mut document = reopened.document.borrow_mut();
        let pixels = document
            .read_tile(TileAddress {
                layer: 0,
                x: 0,
                y: 0,
            })
            .unwrap()
            .unwrap();
        assert_eq!(&pixels[..8], &[55, 55, 55, 255, 27, 0, 0, 128]);
        assert!(document.state().layers[0].locked);
        assert_eq!(document.state().layers.len(), 2);
    }
}
