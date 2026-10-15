use sideduck_brush::{builtin_library, DenseRaster, StrokeEngine};
use sideduck_input::InputSample;
fn main() -> Result<(), String> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Usage: render_golden FILE.png")?;
    let (config, presets) = builtin_library()?;
    let mut pixels = vec![0_u8; 384 * 128 * 4];
    for (preset_index, preset) in presets.into_iter().enumerate() {
        let mut raster = DenseRaster::new(64, 64, 65536)?;
        raster.clear([0.08, 0.12, 0.16, 0.4])?;
        let mut engine = StrokeEngine::new(preset, config.clone(), [0.7, 0.2, 0.1], 93)?;
        for i in 0..7 {
            engine.push_sample(
                &mut raster,
                InputSample {
                    position: [8.0 + i as f32 * 7.0, 20.0 + (i as f32 * 0.7).sin() * 12.0],
                    pressure: 0.3 + i as f32 * 0.1,
                    tilt: [0.1 * i as f32, 0.2],
                    azimuth: i as f32 * 0.2,
                    timestamp_seconds: i as f64 * 0.01,
                    ..Default::default()
                },
            )?;
        }
        for y in 0..64 {
            for x in 0..64 {
                let pixel = raster.pixels()[y * 64 + x];
                let index = ((preset_index / 6 * 64 + y) * 384 + preset_index % 6 * 64 + x) * 4;
                for channel in 0..4 {
                    pixels[index + channel] = (pixel[channel] * 255.0).round() as u8;
                }
            }
        }
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(file, 384, 128);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&pixels)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}
