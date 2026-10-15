//! Render the original vector mark at all macOS icon sizes during distribution.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args.next().ok_or("expected source SVG")?;
    let directory = std::path::PathBuf::from(args.next().ok_or("expected iconset directory")?);
    let bytes = std::fs::read(source)?;
    let tree = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default())?;
    std::fs::create_dir_all(&directory)?;
    for size in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let pixels = size * scale;
            let mut pixmap =
                resvg::tiny_skia::Pixmap::new(pixels, pixels).ok_or("invalid icon size")?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(
                    pixels as f32 / tree.size().width(),
                    pixels as f32 / tree.size().height(),
                ),
                &mut pixmap.as_mut(),
            );
            let suffix = if scale == 2 { "@2x" } else { "" };
            pixmap.save_png(directory.join(format!("icon_{size}x{size}{suffix}.png")))?;
        }
    }
    Ok(())
}
