use crate::*;
fn rect(x: f64, y: f64, width: f64, height: f64) -> DesktopRect {
    DesktopRect {
        x,
        y,
        width,
        height,
    }
}
fn display(id: u32, frame: DesktopRect, sidecar: Option<bool>) -> DisplayDescriptor {
    DisplayDescriptor {
        id,
        name: format!("Display {id}"),
        frame,
        pixel_size: [8, 8],
        scale_factor: 2.0,
        primary: id == 1,
        built_in: id == 1,
        sidecar,
    }
}
fn fixture() -> MockDesktop {
    let desktop = MockDesktop::default();
    *desktop.displays.lock().unwrap() = vec![
        display(1, rect(0.0, 0.0, 4.0, 4.0), Some(false)),
        display(2, rect(-4.0, 0.0, 4.0, 4.0), Some(true)),
    ];
    *desktop.windows.lock().unwrap() = vec![WindowDescriptor {
        id: 20,
        owner: "Fixture".into(),
        title: "Reference".into(),
        frame: rect(1.0, 1.0, 2.0, 2.0),
        on_screen: true,
    }];
    desktop
}
#[test]
fn canvas_display_selection_uses_reliable_identity_and_explicit_override() {
    let desktop = fixture();
    let mut displays = desktop.displays().unwrap();
    assert_eq!(select_canvas_display(&displays, None).unwrap().id, 2);
    assert_eq!(select_canvas_display(&displays, Some(1)).unwrap().id, 1);
    displays[1].sidecar = None;
    assert_eq!(select_canvas_display(&displays, None).unwrap().id, 1);
    assert!(select_canvas_display(&displays, Some(999)).is_err());
    assert!(select_canvas_display(&[], None).is_err());
}
#[test]
fn overlay_modes_work_against_mock_platform_and_pinned_geometry_tracks_window() {
    let desktop = fixture();
    let mut overlay = MockOverlayWindow::new(desktop.clone(), OverlayTarget::Display(2)).unwrap();
    assert_eq!(overlay.state().frame, rect(-4.0, 0.0, 4.0, 4.0));
    assert_eq!(overlay.state().input_mode, OverlayInputMode::ClickThrough);
    overlay.set_visible(true).unwrap();
    overlay.set_input_mode(OverlayInputMode::Draw).unwrap();
    overlay.set_target(OverlayTarget::Window(20)).unwrap();
    assert_eq!(overlay.state().frame, rect(1.0, 1.0, 2.0, 2.0));
    desktop.windows.lock().unwrap()[0].frame = rect(-3.0, 0.0, 3.0, 4.0);
    assert!(overlay.refresh().unwrap());
    assert_eq!(overlay.state().frame, rect(-3.0, 0.0, 3.0, 4.0));
    assert!(!overlay.refresh().unwrap());
    desktop.windows.lock().unwrap()[0].on_screen = false;
    assert!(overlay.refresh().unwrap());
    assert!(!overlay.state().visible);
    desktop.windows.lock().unwrap()[0].on_screen = true;
    overlay.refresh().unwrap();
    assert!(overlay.state().visible);
    let before = overlay.state();
    assert!(overlay.set_target(OverlayTarget::Window(999)).is_err());
    assert_eq!(overlay.state(), before);
    overlay.set_visible(false).unwrap();
    assert!(!overlay.state().visible);
    assert!(MockOverlayWindow::new(desktop, OverlayTarget::Display(999)).is_err());
}
#[test]
fn capture_requires_explicit_permission_and_obeys_pixel_budget() {
    let mut capture = MockScreenCapture {
        desktop: fixture(),
        ..MockScreenCapture::default()
    };
    let rgba8 = (0..64).flat_map(|n| [n, n + 1, n + 2, 255]).collect();
    let image = CapturedImage {
        width: 8,
        height: 8,
        rgba8,
    };
    capture.display_images.insert(1, image.clone());
    capture.window_images.insert(20, image.clone());
    assert_eq!(capture.permission_status(), CapturePermission::NotGranted);
    assert!(capture.capture(CaptureTarget::Display(1), 256).is_err());
    assert_eq!(capture.permission_requests, 0);
    assert_eq!(
        capture.request_permission().unwrap(),
        CapturePermission::NotGranted
    );
    capture.grant_on_request = true;
    assert_eq!(
        capture.request_permission().unwrap(),
        CapturePermission::Granted
    );
    assert!(CAPTURE_PERMISSION_EXPLANATION.contains("local document"));
    assert!(capture.capture(CaptureTarget::Display(1), 255).is_err());
    assert_eq!(
        capture.capture(CaptureTarget::Window(20), 256).unwrap(),
        image
    );
    let crop = capture
        .capture(CaptureTarget::Region(rect(1.0, 1.0, 2.0, 1.0)), 32)
        .unwrap();
    assert_eq!([crop.width, crop.height], [4, 2]);
    assert_eq!(
        &crop.rgba8[..16],
        &image.rgba8[(2 * 8 + 2) * 4..(2 * 8 + 6) * 4]
    );
    assert_eq!(
        &crop.rgba8[16..],
        &image.rgba8[(3 * 8 + 2) * 4..(3 * 8 + 6) * 4]
    );
    assert_eq!(capture.captures, 2);
    assert!(capture
        .capture(CaptureTarget::Region(rect(3.0, 0.0, 2.0, 2.0)), 256)
        .is_err());
    assert!(capture.capture(CaptureTarget::Window(999), 256).is_err());
    let malformed = CapturedImage {
        width: 1,
        height: 1,
        rgba8: vec![0],
    };
    assert!(malformed.validate(4).is_err());
    assert!(image_bytes([u32::MAX, u32::MAX], usize::MAX).is_err());
    assert!(image_bytes([0, 1], usize::MAX).is_err());
}
#[test]
fn pdf_reference_layer_rerenders_crisply_across_zoom_levels_in_mock() {
    let source = b"portable vector page".to_vec();
    let mut renderer = MockPdfRasterizer::default();
    renderer.documents.insert(
        source.clone(),
        vec![MockPdfPage {
            size: [100.0, 100.0],
            rectangles: vec![(rect(26.0, 25.0, 0.5, 50.0), [0, 0, 0, 255])],
        }],
    );
    assert_eq!(renderer.page_count(&source).unwrap(), 1);
    assert_eq!(renderer.page_size(&source, 0).unwrap(), [100.0, 100.0]);
    let low = renderer.render_page(&source, 0, [10, 10], 400).unwrap();
    assert!(low.rgba8.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    let high = renderer
        .render_page(&source, 0, [400, 400], 400 * 400 * 4)
        .unwrap();
    assert_eq!(
        &high.rgba8[(150 * 400 + 104) * 4..(150 * 400 + 104) * 4 + 4],
        &[0, 0, 0, 255]
    );
    assert_eq!(
        &high.rgba8[(150 * 400 + 106) * 4..(150 * 400 + 106) * 4 + 4],
        &[255, 255, 255, 255]
    );
    assert!(renderer.render_page(&source, 0, [400, 400], 400).is_err());
    assert!(renderer.page_size(&source, 1).is_err());
    assert!(renderer.page_count(b"missing").is_err());
    assert_eq!(source, b"portable vector page");
}
#[test]
fn desktop_geometry_rejects_nonfinite_or_degenerate_frames() {
    for r in [
        rect(0.0, 0.0, 0.0, 1.0),
        rect(f64::NAN, 0.0, 1.0, 1.0),
        rect(f64::MAX, 0.0, f64::MAX, 1.0),
    ] {
        assert!(r.validate().is_err());
    }
    assert!(rect(-4.0, -4.0, 4.0, 4.0).contains(rect(-3.0, -3.0, 1.0, 1.0)));
    assert!(!rect(0.0, 0.0, 4.0, 4.0).contains(rect(3.0, 3.0, 2.0, 2.0)));
}
