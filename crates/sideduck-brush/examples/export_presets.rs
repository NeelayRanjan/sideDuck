use std::path::PathBuf;
fn main() -> Result<(), String> {
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("Usage: export_presets DIRECTORY")?;
    let (config, presets) = sideduck_brush::builtin_library()?;
    for preset in presets {
        let filename = preset.definition.name.to_lowercase().replace(' ', "-") + ".quill";
        sideduck_brush::save_brush_package(destination.join(filename), &preset, &config)?;
    }
    Ok(())
}
