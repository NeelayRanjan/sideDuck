use super::*;
use sideduck_input::{InputSample, SamplePhase, ToolKind};
fn sample(x: f32, y: f32, pressure: f32, time: f64) -> InputSample {
    InputSample {
        position: [x, y],
        pressure,
        timestamp_seconds: time,
        ..Default::default()
    }
}
fn render(preset: BrushPreset, reference: bool, seed: u64) -> (DenseRaster, StrokeSummary) {
    let (config, _) = builtin_library().unwrap();
    let mut raster = DenseRaster::new(64, 64, 64 * 64 * 16).unwrap();
    raster.clear([0.08, 0.12, 0.16, 0.4]).unwrap();
    let mut engine = if reference {
        StrokeEngine::new_reference(preset, config, [0.7, 0.2, 0.1], seed)
    } else {
        StrokeEngine::new(preset, config, [0.7, 0.2, 0.1], seed)
    }
    .unwrap();
    for i in 0..7 {
        let mut input = sample(
            8. + i as f32 * 7.,
            20. + (i as f32 * 0.7).sin() * 12.,
            0.3 + i as f32 * 0.1,
            i as f64 * 0.01,
        );
        input.tilt = [0.1 * i as f32, 0.2];
        input.azimuth = i as f32 * 0.2;
        engine.push_sample(&mut raster, input).unwrap();
    }
    (raster, engine.finish())
}
#[test]
fn every_shipped_brush_reference_parity_and_identical_inputs_produce_identical_pixels() {
    let (_, presets) = builtin_library().unwrap();
    assert_eq!(presets.len(), 12);
    for preset in presets {
        let (actual, summary) = render(preset.clone(), false, 93);
        let (repeat, repeat_summary) = render(preset.clone(), false, 93);
        let (reference, _) = render(preset.clone(), true, 93);
        assert_eq!(
            actual.pixels(),
            repeat.pixels(),
            "{}",
            preset.definition.name
        );
        assert_eq!(summary, repeat_summary);
        assert!(summary.dabs > 0);
        for (index, (a, b)) in actual.pixels().iter().zip(reference.pixels()).enumerate() {
            for channel in 0..4 {
                assert!(
                    (a[channel] - b[channel]).abs() < 2e-5,
                    "{} pixel {index}: {a:?} != {b:?}",
                    preset.definition.name
                );
            }
        }
    }
}

fn simple() -> (EngineConfig, BrushPreset) {
    let (config, mut presets) = builtin_library().unwrap();
    let mut preset = presets.remove(2);
    preset.definition.parameters.size = Parameter::constant(8.0);
    preset.definition.parameters.spacing = Parameter::constant(0.25);
    preset.shape = Texture::new(1, 1, vec![255]).unwrap();
    preset.grain = Texture::new(1, 1, vec![255]).unwrap();
    (config, preset)
}
#[test]
fn stroke_level_opacity_applies_once_and_eraser_preserves_premultiplication() {
    let (config, mut preset) = simple();
    preset.definition.parameters.opacity = Parameter::constant(0.25);
    let mut raster = DenseRaster::new(32, 16, 8192).unwrap();
    let mut engine = StrokeEngine::new(preset.clone(), config.clone(), [1.0, 1.0, 1.0], 1).unwrap();
    engine
        .push_sample(&mut raster, sample(6.0, 8.0, 1.0, 0.0))
        .unwrap();
    engine
        .push_sample(&mut raster, sample(24.0, 8.0, 1.0, 0.1))
        .unwrap();
    assert_eq!(raster.read_pixel(10, 8).unwrap(), [0.25; 4]);
    assert!(raster.pixels().iter().all(|pixel| pixel[3] <= 0.25));
    engine.finish();
    let mut engine = StrokeEngine::new(preset, config, [0.0; 3], 2).unwrap();
    let mut input = sample(10.0, 8.0, 1.0, 0.0);
    input.tool = ToolKind::Eraser;
    engine.push_sample(&mut raster, input).unwrap();
    assert_eq!(raster.read_pixel(10, 8).unwrap(), [0.1875; 4]);
}
#[test]
fn stroke_compositing_over_translucent_white_matches_analytic_source_over() {
    let (config, mut preset) = simple();
    preset.definition.parameters.opacity = Parameter::constant(0.5);
    let mut raster = DenseRaster::new(16, 16, 4096).unwrap();
    raster.clear([0.5; 4]).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 0).unwrap();
    engine
        .push_sample(&mut raster, sample(8.0, 8.0, 1.0, 0.0))
        .unwrap();
    assert_eq!(raster.read_pixel(8, 8).unwrap(), [0.75; 4]);
}
#[test]
fn even_spacing_is_independent_of_sample_partition_and_zero_pressure_does_not_paint() {
    let (config, preset) = simple();
    let mut a = DenseRaster::new(64, 32, 32768).unwrap();
    let mut b = a.clone();
    let mut first = StrokeEngine::new(preset.clone(), config.clone(), [0.3, 0.5, 0.7], 4).unwrap();
    let mut second = StrokeEngine::new(preset, config, [0.3, 0.5, 0.7], 4).unwrap();
    first
        .push_sample(&mut a, sample(4.0, 16.0, 1.0, 0.0))
        .unwrap();
    first
        .push_sample(&mut a, sample(60.0, 16.0, 1.0, 1.0))
        .unwrap();
    for x in 4..=60 {
        second
            .push_sample(&mut b, sample(x as f32, 16.0, 1.0, (x - 4) as f64 / 56.0))
            .unwrap();
    }
    assert_eq!(a.pixels(), b.pixels());
    assert_eq!(first.finish().dabs, 29);
    assert_eq!(second.finish().dabs, 29);
    let (config, preset) = simple();
    let mut empty = DenseRaster::new(16, 16, 4096).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 1).unwrap();
    engine
        .push_sample(&mut empty, sample(2.0, 2.0, 0.0, 0.0))
        .unwrap();
    engine
        .push_sample(&mut empty, sample(12.0, 12.0, 0.0, 1.0))
        .unwrap();
    assert!(empty.pixels().iter().all(|pixel| *pixel == [0.0; 4]));
}
#[test]
fn seeded_jitter_changes_with_seed_and_every_response_driver_evaluates_its_curve() {
    let (_, presets) = builtin_library().unwrap();
    let (a, _) = render(presets[10].clone(), false, 10);
    let (b, _) = render(presets[10].clone(), false, 11);
    assert_ne!(a.pixels(), b.pixels());
    for (index, driver) in [
        Driver::Pressure,
        Driver::Tilt,
        Driver::Azimuth,
        Driver::Velocity,
        Driver::Random,
    ]
    .into_iter()
    .enumerate()
    {
        let parameter = Parameter {
            value: 4.0,
            response: Some(Response {
                driver,
                curve: Default::default(),
                minimum: 0.25,
                maximum: 0.75,
            }),
        };
        let drivers = [0.0, 0.25, 0.5, 0.75, 1.0];
        assert_eq!(parameter.evaluate(drivers), 1.0 + 2.0 * drivers[index]);
    }
}
#[test]
fn wet_mix_picks_up_target_color_and_smoothing_can_be_suspended() {
    let (config, mut preset) = simple();
    preset.definition.parameters.wet_mix = Parameter::constant(1.0);
    preset.definition.parameters.pickup_length = Parameter::constant(0.0);
    let mut raster = DenseRaster::new(64, 32, 32768).unwrap();
    raster.clear([0.0, 0.5, 0.0, 0.5]).unwrap();
    let mut engine = StrokeEngine::new(preset.clone(), config.clone(), [1.0, 0.0, 0.0], 1).unwrap();
    engine
        .push_sample(&mut raster, sample(10.0, 10.0, 1.0, 0.0))
        .unwrap();
    assert_eq!(raster.read_pixel(10, 10).unwrap(), [0.0, 1.0, 0.0, 1.0]);
    preset.definition.parameters.wet_mix = Parameter::constant(0.0);
    preset.definition.parameters.smoothing = Parameter::constant(1.0);
    let mut raster = DenseRaster::new(64, 32, 32768).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 1).unwrap();
    engine
        .push_sample(&mut raster, sample(10.0, 10.0, 1.0, 0.0))
        .unwrap();
    assert_eq!(
        engine
            .push_sample(&mut raster, sample(30.0, 10.0, 1.0, 1.0))
            .unwrap()
            .dabs,
        0
    );
    let mut input = sample(40.0, 10.0, 1.0, 2.0);
    input.modifiers.suspend_stabilization = true;
    assert!(engine.push_sample(&mut raster, input).unwrap().dabs > 0);
    assert!(raster.read_pixel(38, 10).unwrap()[3] > 0.0);
}
#[test]
fn sparse_memory_ceiling_and_cancel_restore_preimages_after_partial_failure() {
    let (mut config, preset) = simple();
    config.maximum_stroke_bytes = 32 * 32 * 36 + 2048;
    let mut raster = TileRaster::new(8192, 8192, 32, 1 << 20).unwrap();
    let mut engine = StrokeEngine::new(preset, config.clone(), [1.0; 3], 1).unwrap();
    engine
        .push_sample(&mut raster, sample(16.0, 16.0, 1.0, 0.0))
        .unwrap();
    assert_eq!(raster.tile_count(), 1);
    assert!(engine
        .push_sample(&mut raster, sample(80.0, 16.0, 1.0, 1.0))
        .is_err());
    assert!(engine.resident_bytes() <= config.maximum_stroke_bytes);
    engine.cancel(&mut raster).unwrap();
    raster.trim_transparent();
    assert_eq!(raster.tile_count(), 0);
    assert_eq!(engine.resident_bytes(), 0);
    assert!(engine
        .push_sample(&mut raster, sample(16.0, 16.0, 1.0, 2.0))
        .is_err());
    assert!(engine.cancel(&mut raster).is_err());
    let mut raster = TileRaster::new(128, 128, 8, 8 * 8 * 16 + 1024).unwrap();
    raster.write_pixel(1, 1, [0.5; 4]).unwrap();
    assert!(raster.write_pixel(12, 12, [1.0; 4]).is_err());
    assert_eq!(raster.tile_count(), 1);
    assert_eq!(raster.tiles().next().unwrap().0, [0, 0]);
    assert_eq!(raster.take_dirty_tiles(), vec![[0, 0]]);
    assert!(raster.take_dirty_tiles().is_empty());
    assert!(DenseRaster::new(8192, 8192, 1024).is_err());
    assert!(TileRaster::new(0, 2, 8, 4096).is_err());
    assert!(TileRaster::new(2, 2, 7, 4096).is_err());
}
#[test]
fn malformed_samples_parameters_and_surface_changes_are_rejected() {
    let (config, preset) = simple();
    let mut raster = DenseRaster::new(64, 32, 32768).unwrap();
    let mut engine = StrokeEngine::new(preset.clone(), config.clone(), [1.0; 3], 1).unwrap();
    let mut input = sample(10.0, 10.0, 1.0, 0.0);
    input.phase = SamplePhase::Hover;
    assert_eq!(engine.push_sample(&mut raster, input).unwrap().dabs, 0);
    input.phase = SamplePhase::Begin;
    input.timestamp_seconds = 1.0;
    engine.push_sample(&mut raster, input).unwrap();
    input.timestamp_seconds = 0.5;
    assert!(engine.push_sample(&mut raster, input).is_err());
    input.timestamp_seconds = 2.0;
    input.tool = ToolKind::Eraser;
    assert!(engine.push_sample(&mut raster, input).is_err());
    input.tool = ToolKind::Stylus;
    input.position[0] = f32::NAN;
    assert!(engine.push_sample(&mut raster, input).is_err());
    let mut other = DenseRaster::new(16, 16, 4096).unwrap();
    assert!(engine
        .push_sample(&mut other, sample(1.0, 1.0, 1.0, 2.0))
        .is_err());
    assert!(engine.cancel(&mut other).is_err());
    engine.finish();
    assert!(engine
        .push_sample(&mut raster, sample(1.0, 1.0, 1.0, 2.0))
        .is_err());
    assert!(StrokeEngine::new(preset.clone(), config.clone(), [f32::NAN; 3], 1).is_err());
    let mut bad = preset;
    bad.definition.parameters.spacing.value = 0.0;
    assert!(StrokeEngine::new(bad, config, [1.0; 3], 1).is_err());
    assert!(raster.read_pixel(100, 0).is_err());
    assert!(raster.write_pixel(0, 0, [1.0, 0.0, 0.0, 0.5]).is_err());
    assert!(raster.clear([f32::NAN; 4]).is_err());
}
#[test]
fn texture_sampling_matches_independent_weighted_reference_and_invalid_textures_fail() {
    let texture = Texture::new(2, 2, vec![0, 100, 200, 255]).unwrap();
    for repeat in [false, true] {
        for y in -4..8 {
            for x in -4..8 {
                let u = x as f32 / 4.0;
                let v = y as f32 / 4.0;
                assert!(
                    (texture.sample(u, v, repeat) - texture.reference_sample(u, v, repeat)).abs()
                        < 1e-6
                );
            }
        }
    }
    assert!(Texture::new(0, 1, vec![]).is_err());
    assert!(Texture::new(1, 1, vec![]).is_err());
    assert_eq!(texture.dimensions(), [2, 2]);
    assert_eq!(texture.pixels(), [0, 100, 200, 255]);
}
#[test]
fn open_brush_package_round_trip_is_lossless_and_rejects_partial_or_oversized_imports() {
    let (config, presets) = builtin_library().unwrap();
    let root = std::env::temp_dir().join(format!("brush-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (index, preset) in presets.iter().enumerate() {
        let path = root.join(format!("{index}.quill"));
        save_brush_package(&path, preset, &config).unwrap();
        assert_eq!(*preset, load_brush_package(&path, &config).unwrap());
        assert!(save_brush_package(&path, preset, &config).is_err());
    }
    let path = root.join("0.quill");
    std::fs::remove_file(path.join("grain.png")).unwrap();
    assert!(load_brush_package(&path, &config).is_err());
    std::fs::write(path.join("brush.json"), vec![b' '; 1_048_577]).unwrap();
    assert!(load_brush_package(&path, &config).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn golden_images_match_every_shipped_brush_with_one_byte_tolerance() {
    let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!(
        "../assets/strokes.png"
    )));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!([info.width, info.height], [384, 128]);
    let (_, presets) = builtin_library().unwrap();
    for (preset_index, preset) in presets.into_iter().enumerate() {
        let (raster, _) = render(preset, false, 93);
        for y in 0..64 {
            for x in 0..64 {
                let pixel = raster.pixels()[y * 64 + x];
                let index = ((preset_index / 6 * 64 + y) * 384 + preset_index % 6 * 64 + x) * 4;
                for channel in 0..4 {
                    let actual = (pixel[channel] * 255.0).round() as u8;
                    assert!(
                        actual.abs_diff(bytes[index + channel]) <= 1,
                        "preset {preset_index}, {x},{y}, channel {channel}"
                    );
                }
            }
        }
    }
}
#[test]
fn stroke_pipeline_sustains_10000_dabs_per_second_into_dirty_tiles() {
    let report = run_throughput_gate().unwrap();
    assert!(report.dabs >= 10_000);
    assert!(report.dabs_per_second >= 10_000.0);
    assert!(report.dirty_tiles > 0);
}
#[test]
fn invalid_configuration_and_dab_limits_fail_without_unbounded_work() {
    let (mut config, mut preset) = simple();
    config.minimum_spacing = 3000.0;
    assert!(config.validate().is_err());
    config.minimum_spacing = 0.25;
    preset.definition.parameters.size.value = f32::INFINITY;
    assert!(preset.definition.validate().is_err());
    preset.definition.parameters.size.value = 8.0;
    preset.definition.parameters.flow.response = Some(Response {
        driver: Driver::Pressure,
        curve: Default::default(),
        minimum: 2.0,
        maximum: 1.0,
    });
    assert!(preset.definition.validate().is_err());
    preset.definition.parameters.flow.response = None;
    let mut invalid = preset.clone();
    invalid.definition.version = 99;
    assert!(invalid.validate(&config).is_err());
    invalid = preset.clone();
    invalid.definition.name = " ".into();
    assert!(invalid.validate(&config).is_err());
    config.maximum_texture_side = 1;
    assert!(builtin_library().unwrap().1[0].validate(&config).is_err());
    config.maximum_texture_side = 1024;
    config.maximum_dabs_per_sample = 2;
    let mut raster = DenseRaster::new(64, 64, 65536).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 1).unwrap();
    engine
        .push_sample(&mut raster, sample(4.0, 8.0, 1.0, 0.0))
        .unwrap();
    assert!(engine
        .push_sample(&mut raster, sample(60.0, 8.0, 1.0, 1.0))
        .is_err());
    engine.cancel(&mut raster).unwrap();
    assert!(raster.pixels().iter().all(|pixel| *pixel == [0.0; 4]));
    assert!(BrushLibraryConfig::parse("{}").is_err());
    let mut library = BrushLibraryConfig::parse(BUILTIN_CONFIG_JSON).unwrap();
    library.presets[0].texture.side = 4096;
    assert!(BrushLibraryConfig::parse(&serde_json::to_string(&library).unwrap()).is_err());
    library.presets[0].texture.side = 0;
    assert!(library.build().is_err());
}
#[test]
fn off_canvas_strokes_remain_sparse_and_partial_edge_tiles_cancel_exactly() {
    let (config, preset) = simple();
    let mut target = TileRaster::new(35, 19, 32, 65536).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 8).unwrap();
    engine
        .push_sample(&mut target, sample(-20.0, -20.0, 1.0, 0.0))
        .unwrap();
    assert_eq!(target.tile_count(), 0);
    engine
        .push_sample(&mut target, sample(34.0, 18.0, 1.0, 1.0))
        .unwrap();
    assert!(target.read_pixel(34, 18).unwrap()[3] > 0.0);
    assert!(target.tile_count() <= 2);
    assert!(target.read_pixel(35, 0).is_err());
    assert!(target.write_pixel(0, 19, [1.0; 4]).is_err());
    engine.cancel(&mut target).unwrap();
    target.trim_transparent();
    assert_eq!(target.tile_count(), 0);
    assert_eq!(target.tile_size(), 32);
    target.write_pixel(0, 0, [0.0; 4]).unwrap();
    assert_eq!(target.tile_count(), 0);
}

#[test]
fn serialized_stroke_preset_retains_textures_and_rejects_invalid_deserialized_shape() {
    let (_, presets) = builtin_library().unwrap();
    let preset = &presets[10];
    let restored: BrushPreset =
        serde_json::from_str(&serde_json::to_string(preset).unwrap()).unwrap();
    assert_eq!(preset, &restored);
    let (expected, _) = render(preset.clone(), false, 721);
    let (actual, _) = render(restored, false, 721);
    assert_eq!(expected.pixels(), actual.pixels());
    let mut value = serde_json::to_value(preset).unwrap();
    value["shape"]["width"] = serde_json::json!(0);
    let invalid: BrushPreset = serde_json::from_value(value).unwrap();
    let (config, _) = builtin_library().unwrap();
    assert!(StrokeEngine::new(invalid, config, [1.0; 3], 0).is_err());
}

#[test]
fn zero_pressure_contact_can_activate_without_pointer_motion() {
    let (config, preset) = simple();
    let mut raster = DenseRaster::new(16, 16, 4096).unwrap();
    let mut engine = StrokeEngine::new(preset, config, [1.0; 3], 0).unwrap();
    engine
        .push_sample(&mut raster, sample(8.0, 8.0, 0.0, 0.0))
        .unwrap();
    assert_eq!(
        engine
            .push_sample(&mut raster, sample(8.0, 8.0, 1.0, 0.1))
            .unwrap()
            .dabs,
        1
    );
    assert_eq!(raster.read_pixel(8, 8).unwrap(), [1.0; 4]);
}
