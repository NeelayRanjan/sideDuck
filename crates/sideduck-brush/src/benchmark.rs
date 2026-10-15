use crate::{builtin_library, StrokeEngine, TileRaster};
use serde::Serialize;
use sideduck_input::InputSample;
use std::time::Instant;

#[derive(Clone, Debug, Serialize)]
pub struct BrushBenchmarkReport {
    pub brush: String,
    pub diameter_pixels: f32,
    pub dabs: u64,
    pub elapsed_seconds: f64,
    pub dabs_per_second: f64,
    pub target_resident_bytes: u64,
    pub stroke_peak_bytes: u64,
    pub dirty_tiles: usize,
}
/// A complete sample-to-dirty-tile workload: the shipped 4px technical pen, 10,000 input
/// samples in a serpentine path, including parameter evaluation, stroke accumulation and writes.
/// This is a declared baseline, not a claim about arbitrary 2048px textured brushes.
pub fn run_throughput_gate() -> Result<BrushBenchmarkReport, String> {
    let (config, presets) = builtin_library()?;
    let preset = presets
        .into_iter()
        .find(|preset| preset.definition.name == "Technical pen")
        .ok_or("Missing throughput preset")?;
    let name = preset.definition.name.clone();
    let diameter = preset.definition.parameters.size.value;
    let mut target = TileRaster::new(256, 64, config.tile_size, 16 * 1024 * 1024)?;
    let mut engine = StrokeEngine::new(preset, config, [0.2, 0.4, 0.6], 987)?;
    let start = Instant::now();
    for index in 0..10_000_u32 {
        let row = index / 248;
        let column = index % 248;
        let x = if row % 2 == 0 {
            column + 4
        } else {
            251 - column
        };
        engine.push_sample(
            &mut target,
            InputSample {
                position: [x as f32, (row + 4) as f32],
                pressure: 1.0,
                timestamp_seconds: f64::from(index) / 240.0,
                ..Default::default()
            },
        )?;
    }
    let summary = engine.finish();
    let seconds = start.elapsed().as_secs_f64();
    let rate = summary.dabs as f64 / seconds;
    if rate < 10_000.0 {
        return Err(format!(
            "Stroke pipeline budget failed: {rate:.1} dabs/s, required 10000"
        ));
    }
    if target.resident_bytes() + summary.peak_resident_bytes > 80 * 1024 * 1024 {
        return Err("Stroke pipeline combined residency budget failed".into());
    }
    Ok(BrushBenchmarkReport {
        brush: name,
        diameter_pixels: diameter,
        dabs: summary.dabs,
        elapsed_seconds: seconds,
        dabs_per_second: rate,
        target_resident_bytes: target.resident_bytes(),
        stroke_peak_bytes: summary.peak_resident_bytes,
        dirty_tiles: summary.dirty_tiles.len(),
    })
}
