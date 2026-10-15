//! Desktop entry point. All native APIs live behind sideduck-platform.
mod annotation;
mod core;
mod editing;
mod editor;
mod export;
mod preferences;
mod preview;
mod reference;
use std::path::PathBuf;
use std::time::Duration;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = None;
    let mut prototype = false;
    let mut document = sideduck_platform::app_data_directory()?.join("Scratch.pond");
    let mut exports = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?)
        .join("Pictures/sideDuck Exports");
    let mut smoke = None;
    let mut demo = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--prototype" => prototype = true,
            "--document" => {
                document = PathBuf::from(args.next().ok_or("--document requires a path")?)
            }
            "--config" => {
                config = Some(PathBuf::from(
                    args.next().ok_or("--config requires a path")?,
                ))
            }
            "--exports" => {
                exports = PathBuf::from(args.next().ok_or("--exports requires a directory")?)
            }
            "--smoke-test" => smoke = Some(Duration::from_secs(4)),
            "--demo" => demo = true,
            "--gpu-probe" => {
                println!("{}", sideduck_composite::gpu_probe()?);
                return Ok(());
            }
            "--help" => {
                println!("sideDuck local editor\n  --document PATH  Open or create a document package (default local application data/Scratch.pond)\n  --prototype    Open the earlier drawing lab\n  --config PATH  Override config/prototype.json defaults\n  --exports DIR  PNG output directory (default ~/Pictures/sideDuck Exports)\n  --demo         Draw reproducible pressure sample strokes\n  --smoke-test   Open both windows, then exit after four seconds\n  --gpu-probe    Initialize and report the headless GPU device");
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    if !prototype {
        let options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_title("sideDuck")
                .with_inner_size([1000.0, 800.0]),
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        };
        eframe::run_native(
            "sideDuck",
            options,
            Box::new(move |_cc| Ok(Box::new(editor::EditorApp::new(document, smoke, demo)?))),
        )?;
        return Ok(());
    }
    let config = sideduck_ui::PrototypeConfig::load(config.as_deref())?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("sideDuck · Drawing Lab")
            .with_inner_size([470.0, 900.0])
            .with_min_inner_size([400.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "sideDuck",
        options,
        Box::new(move |cc| {
            Ok(Box::new(
                sideduck_ui::PrototypeApp::new(cc, config, exports, smoke, demo)
                    .map_err(|e| e.to_string())?,
            ))
        }),
    )?;
    Ok(())
}
