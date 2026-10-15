//! Native, real-surface benchmark. Run on the main thread with a desktop session.
use sideduck_bench::render_gate::{
    open_fixture, reference_layers, RenderMeasurements, DERIVED_CACHE, GPU_CEILING,
};
use sideduck_bench::{CANVAS_SIZE, LAYERS, MIB, TILE_SIZE};
use sideduck_composite::gpu::{GpuCompositor, GpuImage};
use sideduck_composite::mip_store::generate_mips_with_progress;
use sideduck_composite::present::{run_render_benchmark, FrameProducer, PresentationConfig};
use sideduck_composite::raster::{
    PreparedViewport, RasterLayer, RasterRenderer, RenderRect, StoreSource,
};
use sideduck_tile::{PixelFormat, StoreOptions, TileStore};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Frames {
    viewport: PreparedViewport,
    measurements: Arc<Mutex<RenderMeasurements>>,
}

impl FrameProducer for Frames {
    fn encode_frame<'a>(
        &'a mut self,
        gpu: &GpuCompositor,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<&'a GpuImage, String> {
        let mut report = self
            .measurements
            .lock()
            .map_err(|error| error.to_string())?;
        report.gpu_peak_bytes = gpu.peak_allocated_bytes();
        report.sample_memory()?;
        drop(report);
        self.viewport.encode(gpu, encoder)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Render budget gate FAILED: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut root = PathBuf::from("target/storage-gate");
    let mut results = PathBuf::from("target/render-results.json");
    let mut fit = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--fit" => fit = true,
            "--root" => root = args.next().ok_or("--root needs a path")?.into(),
            "--results" => results = args.next().ok_or("--results needs a path")?.into(),
            "--help" => {
                println!("Real-surface render gate: --root STORAGE_GATE_ROOT --results JSON\nRequires the full storage corpus and a desktop GPU. Measures every active layer switch, then 2560x1600 composition and native presentation. Preparation is reported separately; OS scanout is not measured.");
                return Ok(());
            }
            _ => return Err(format!("unknown render-gate argument {argument}")),
        }
    }
    let started = Instant::now();
    let mut store = open_fixture(&root)?;
    let measurements = Arc::new(Mutex::new(RenderMeasurements {
        canvas: [CANVAS_SIZE; 2],
        layers: LAYERS,
        occupied_tiles: store.tile_count(),
        empty_percent: 60,
        source_open_ms: started.elapsed().as_secs_f64() * 1000.0,
        conditions: "Full independently generated painting-like corpus. All normal layers, linear RGBA8 sources, RGBA16F derived range tiles. Derived tree built before switch timing; source/derived decoded caches bounded, OS cache uncontrolled. All 500 active indices measured; GPU completion included. Presentation includes acquire, encode, submit, native present call and GPU completion, excluding physical scanout.".into(),
        ..Default::default()
    }));
    let observed = measurements.clone();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let cache_path = root.join(format!("render-cache-{}-{nonce}", std::process::id()));
    let cleanup_path = cache_path.clone();
    let config = PresentationConfig {
        width: 2560,
        height: 1600,
        warmup_frames: 8,
        measured_frames: 60,
        budget_bytes: GPU_CEILING,
        max_frame_ms: if fit { 16.0 } else { 8.0 },
    };
    let presentation = run_render_benchmark(config, move |gpu| {
        println!("Render adapter: {}", gpu.adapter_name());
        let prepared_at = Instant::now();
        let layers: Vec<RasterLayer> = reference_layers().into_iter().map(Into::into).collect();
        let level = if fit { 3 } else { 0 };
        let mut mips = if fit {
            let mut destination = TileStore::create(
                cache_path.join("mips"),
                StoreOptions {
                    tile_size: TILE_SIZE,
                    format: PixelFormat::Rgba16Float,
                    cache_budget_bytes: DERIVED_CACHE,
                },
            )
            .map_err(|error| error.to_string())?;
            let ids: Vec<u32> = (0..LAYERS).collect();
            println!("Generating persisted level-3 mips from the full 8K corpus");
            generate_mips_with_progress(
                gpu,
                &mut store,
                &mut destination,
                [CANVAS_SIZE; 2],
                &ids,
                level,
                &mut |stats| {
                    let mut report = observed.lock().map_err(|error| error.to_string())?;
                    report.gpu_peak_bytes = stats.gpu_peak_bytes;
                    report.sample_memory()?;
                    if stats.visited_output_tiles % 800 == 0 {
                        println!(
                            "Pyramid tiles {}/8000, process peak {:.1}MiB",
                            stats.visited_output_tiles,
                            report.process_peak_bytes as f64 / MIB as f64
                        );
                    }
                    Ok(())
                },
            )?;
            destination.commit().map_err(|error| error.to_string())?;
            Some(destination)
        } else {
            None
        };
        let mut source = StoreSource::new(&mut store);
        if let Some(mips) = mips.as_mut() {
            source.add_level(level, mips)?;
        }
        let mut renderer = RasterRenderer::create(
            cache_path.join("ranges"),
            TILE_SIZE,
            DERIVED_CACHE,
            256 * MIB,
        )?;
        let rect = RenderRect {
            x: 0,
            y: 0,
            width: if fit { 1024 } else { 2560 },
            height: if fit { 1024 } else { 1600 },
        };
        renderer.warm_viewport(gpu, &mut source, &layers, level, rect)?;
        renderer.commit()?;
        // Initial display belongs to opening/preparation. Every timed operation
        // below changes an existing active layer; no split is prewarmed for it.
        let mut last = Some(renderer.prepare_viewport(
            gpu,
            &mut source,
            &layers,
            LAYERS as usize / 2,
            level,
            rect,
        )?);
        gpu.wait();
        {
            let mut report = observed.lock().map_err(|error| error.to_string())?;
            report.cache_preparation_seconds = prepared_at.elapsed().as_secs_f64();
            report.mip_level = level;
            report.gpu_peak_bytes = gpu.peak_allocated_bytes();
            report.sample_memory()?;
            println!(
                "Derived range preparation: {:.3}s",
                report.cache_preparation_seconds
            );
        }
        for ordinal in 0..LAYERS as usize {
            // Alternate ends to avoid measuring only adjacent incremental moves.
            let active = if ordinal % 2 == 0 {
                ordinal / 2
            } else {
                LAYERS as usize - 1 - ordinal / 2
            };
            let previous = last.take().ok_or("active viewport missing")?;
            let switched = Instant::now();
            let next = renderer.switch_active(gpu, &mut source, &layers, active, previous)?;
            gpu.wait();
            let elapsed_ms = switched.elapsed().as_secs_f64() * 1000.0;
            let mut report = observed.lock().map_err(|error| error.to_string())?;
            report.record_switch(elapsed_ms);
            report.gpu_peak_bytes = gpu.peak_allocated_bytes();
            report.sample_memory()?;
            if ordinal < 10 || ordinal % 50 == 0 {
                println!(
                    "Layer switches {}/{LAYERS}: current {:.3}ms, worst {:.3}ms, process peak {:.1}MiB",
                    ordinal + 1,
                    elapsed_ms,
                    report.active_switch_max_ms,
                    report.process_peak_bytes as f64 / MIB as f64
                );
            }
            last = Some(next);
        }
        let viewport = last.ok_or("no viewport prepared")?;
        Ok(Box::new(Frames {
            viewport,
            measurements: observed,
        }) as Box<dyn FrameProducer>)
    });
    let mut report = measurements.lock().map_err(|error| error.to_string())?;
    report.sample_memory()?;
    let output = serde_json::json!({
        "render": &*report,
        "presentation": presentation.as_ref().ok(),
        "presentation_error": presentation.as_ref().err(),
    });
    if let Some(parent) = results.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &results,
        serde_json::to_vec_pretty(&output).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!("Render measurements: {}", results.display());
    if cleanup_path.exists() {
        std::fs::remove_dir_all(cleanup_path).map_err(|error| error.to_string())?;
    }
    presentation?;
    report.assert_budgets()?;
    println!(
        "Render budgets passed: worst layer switch {:.3}ms",
        report.active_switch_max_ms
    );
    Ok(())
}
