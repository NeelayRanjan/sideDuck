use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn sample(x: f32, y: f32, time: f64) -> InputSample {
    InputSample {
        position: [x, y],
        pressure: 0.5,
        timestamp_seconds: time,
        device_id: 7,
        ..InputSample::default()
    }
}
fn chain(filters: Vec<FilterSpec>) -> FilterChain {
    FilterChain::new(InputConfig {
        filters,
        ..InputConfig::default()
    })
    .unwrap()
}
fn prediction() -> FilterSpec {
    FilterSpec::Prediction {
        horizon_ms: 10.0,
        max_distance: 3.0,
        minimum_interval_ms: 0.25,
    }
}
fn curve(points: &[[f32; 2]]) -> ResponseCurve {
    ResponseCurve {
        points: points.to_vec(),
    }
}

#[test]
fn recorded_sample_streams_replay_identically() {
    let stream: Vec<InputSample> =
        serde_json::from_str(include_str!("../tests/data/recorded-stroke.json")).unwrap();
    let config = InputConfig::default();
    let encoded = serde_json::to_string(&config).unwrap();
    let replay = || {
        let mut filters = FilterChain::new(InputConfig::from_json(&encoded).unwrap()).unwrap();
        stream
            .iter()
            .map(|s| filters.process(*s))
            .collect::<Vec<_>>()
    };
    let first = replay();
    assert_eq!(first, replay());
    let saved = serde_json::to_string(&first).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<ProcessedSample>>(&saved).unwrap(),
        first
    );
    assert!(first.iter().any(|event| event.provisional.is_some()));
    assert_eq!(
        first.last().unwrap().confirmed.unwrap().phase,
        SamplePhase::End
    );
    assert!(first.last().unwrap().provisional.is_none());
}

fn line(image: &mut [u8; 192], a: [f32; 2], b: [f32; 2]) {
    // Independent integer raster oracle; the fixture uses integral coordinates.
    let (mut x, mut y) = (a[0] as i32, a[1] as i32);
    let (tx, ty) = (b[0] as i32, b[1] as i32);
    let (dx, dy) = ((tx - x).abs(), -(ty - y).abs());
    let (sx, sy) = ((tx - x).signum(), (ty - y).signum());
    let mut error = dx + dy;
    loop {
        if (0..16).contains(&x) && (0..12).contains(&y) {
            image[y as usize * 16 + x as usize] = 255;
        }
        if (x, y) == (tx, ty) {
            break;
        }
        let doubled = 2 * error;
        if doubled >= dy {
            error += dy;
            x += sx;
        }
        if doubled <= dx {
            error += dx;
            y += sy;
        }
    }
}

#[test]
fn prediction_reconciles_without_visible_seams_in_golden_stroke_tests() {
    let stream: Vec<InputSample> =
        serde_json::from_str(include_str!("../tests/data/recorded-stroke.json")).unwrap();
    let mut filters = chain(vec![prediction()]);
    let mut no_prediction = chain(vec![]);
    let mut committed = [0; 192];
    let mut temporary = [0; 192];
    let mut previous = None;
    let mut previous_prediction = None;
    for (index, input) in stream.into_iter().enumerate() {
        let result = filters.process(input);
        assert_eq!(result.confirmed, no_prediction.process(input).confirmed);
        assert_eq!(result.retired_prediction, previous_prediction);
        if result.retired_prediction.is_some() {
            temporary.fill(0);
        }
        let confirmed = result.confirmed.unwrap();
        line(
            &mut committed,
            previous.unwrap_or(confirmed.position),
            confirmed.position,
        );
        previous = Some(confirmed.position);
        if let Some(predicted) = result.provisional {
            line(
                &mut temporary,
                confirmed.position,
                predicted.sample.position,
            );
            assert_eq!(predicted.based_on_sequence, result.sequence);
        }
        previous_prediction = result.provisional.map(|p| p.id);
        if index == 2 {
            assert_eq!(temporary[2 * 16 + 11], 255);
        }
        if index == 3 {
            assert_eq!(
                temporary[2 * 16 + 11],
                0,
                "old straight-ahead prediction survived a real corner"
            );
        }
    }
    assert!(temporary.iter().all(|v| *v == 0));
    let mut golden = [0; 192];
    for x in 2..=8 {
        golden[2 * 16 + x] = 255;
        golden[5 * 16 + x] = 255;
    }
    golden[3 * 16 + 8] = 255;
    golden[4 * 16 + 8] = 255;
    assert_eq!(
        committed, golden,
        "provisional pixels must never enter the committed stroke"
    );
}

#[test]
fn average_and_pull_string_match_known_positions_and_suspend_mid_stroke() {
    let mut average = chain(vec![FilterSpec::Average { window: 3 }]);
    for (i, (input, expected)) in [(0.0, 0.0), (6.0, 3.0), (12.0, 6.0), (18.0, 12.0)]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            average
                .process(sample(input, 0.0, i as f64))
                .confirmed
                .unwrap()
                .position,
            [expected, 0.0]
        );
    }
    let mut suspended = sample(30.0, 0.0, 4.0);
    suspended.modifiers.suspend_stabilization = true;
    assert_eq!(
        average.process(suspended).confirmed.unwrap().position,
        [30.0, 0.0]
    );
    assert_eq!(
        average
            .process(sample(36.0, 0.0, 5.0))
            .confirmed
            .unwrap()
            .position,
        [33.0, 0.0]
    );
    let mut pull = chain(vec![FilterSpec::PullString { distance: 5.0 }]);
    assert_eq!(
        pull.process(sample(0.0, 0.0, 0.0))
            .confirmed
            .unwrap()
            .position,
        [0.0, 0.0]
    );
    assert_eq!(
        pull.process(sample(3.0, 4.0, 1.0))
            .confirmed
            .unwrap()
            .position,
        [0.0, 0.0]
    );
    assert_eq!(
        pull.process(sample(6.0, 8.0, 2.0))
            .confirmed
            .unwrap()
            .position,
        [3.0, 4.0]
    );
    suspended.timestamp_seconds = 3.0;
    assert_eq!(
        pull.process(suspended).confirmed.unwrap().position,
        [30.0, 0.0]
    );
    assert_eq!(
        pull.process(sample(36.0, 0.0, 4.0))
            .confirmed
            .unwrap()
            .position,
        [31.0, 0.0]
    );
}

#[test]
fn smoothing_history_is_bounded_for_long_recorded_streams() {
    let mut filters = chain(vec![
        FilterSpec::Average { window: 17 },
        FilterSpec::Average { window: 9 },
    ]);
    for index in 0..20_000 {
        assert!(filters
            .process(sample(index as f32, 0.0, index as f64 / 240.0))
            .confirmed
            .is_some());
        assert!(filters.retained_history_samples() <= 26);
    }
    filters.reset();
    assert_eq!(filters.retained_history_samples(), 0);
}

#[test]
fn pressure_curves_are_ordered_and_editing_retires_preview() {
    let mut config = InputConfig {
        filters: vec![
            FilterSpec::Calibration,
            FilterSpec::GlobalPressure,
            FilterSpec::BrushPressure,
            prediction(),
        ],
        ..InputConfig::default()
    };
    config.global_pressure = curve(&[[0.0, 0.0], [1.0, 0.5]]);
    config.brush_pressure = curve(&[[0.0, 0.0], [0.5, 1.0], [1.0, 1.0]]);
    let mut filters = FilterChain::new(config).unwrap();
    filters
        .set_calibration(curve(&[[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]]))
        .unwrap();
    assert_eq!(filters.calibration_curve().evaluate(0.5), 0.25);
    assert_eq!(
        filters
            .process(sample(0.0, 0.0, 0.0))
            .confirmed
            .unwrap()
            .pressure,
        0.25
    );
    let preview = filters.process(sample(1.0, 0.0, 0.01)).provisional.unwrap();
    assert_eq!(
        filters
            .set_brush_pressure(ResponseCurve::default())
            .unwrap(),
        Some(preview.id)
    );
    assert!(filters.process(sample(2.0, 0.0, 0.02)).stroke_reset);
    filters
        .set_global_pressure(ResponseCurve::default())
        .unwrap();
    assert_eq!(
        filters
            .process(sample(3.0, 0.0, 0.03))
            .confirmed
            .unwrap()
            .pressure,
        0.25
    );
    assert!(filters
        .set_calibration(curve(&[[0.0, 1.0], [1.0, 0.0]]))
        .is_err());
    assert!(filters.set_brush_pressure(curve(&[])).is_err());
    assert!(filters.set_global_pressure(curve(&[])).is_err());
}

#[test]
fn calibration_fits_a_known_distribution_and_preserves_histogram_counts() {
    let config = CalibrationConfig {
        sample_limit: 2000,
        minimum_samples: 100,
        low_quantile: 0.0,
        high_quantile: 1.0,
        curve_knots: 65,
        histogram_bins: 10,
        ..InputConfig::default().calibration
    };
    let mut calibration = PressureCalibrator::new(config).unwrap();
    for i in 1..=2000 {
        let q = i as f32 / 2000.0;
        let mut input = sample(0.0, 0.0, i as f64);
        input.pressure = q * q;
        assert!(calibration.observe(input));
    }
    assert!(!calibration.observe(sample(0.0, 0.0, 3000.0)));
    let fit = calibration.fit().unwrap();
    for q in [0.1_f32, 0.25, 0.5, 0.75, 0.9] {
        assert!((fit.curve.evaluate(q * q) - q).abs() < 0.002);
    }
    assert_eq!(fit.sample_count, 2000);
    assert_eq!(fit.before_histogram.iter().sum::<u64>(), 2000);
    assert_eq!(fit.after_histogram.iter().sum::<u64>(), 2000);
    assert!(fit.before_histogram[0] > 600);
    assert!(fit.after_histogram.iter().all(|n| (195..=205).contains(n)));
    assert_eq!(
        serde_json::from_str::<CalibrationResult>(&serde_json::to_string(&fit).unwrap()).unwrap(),
        fit
    );
    calibration.clear();
    assert_eq!(calibration.sample_count(), 0);
    assert!(calibration.fit().is_err());
}

#[test]
fn calibration_rejects_noncontact_and_degenerate_samples() {
    let mut calibration = PressureCalibrator::new(InputConfig::default().calibration).unwrap();
    let valid = sample(0.0, 0.0, 0.0);
    for invalid in [
        InputSample {
            pressure: 0.0,
            ..valid
        },
        InputSample {
            pressure: f32::NAN,
            ..valid
        },
        InputSample {
            tool: ToolKind::Mouse,
            ..valid
        },
        InputSample {
            phase: SamplePhase::End,
            ..valid
        },
        InputSample {
            phase: SamplePhase::Hover,
            ..valid
        },
    ] {
        assert!(!calibration.observe(invalid));
    }
    for _ in 0..100 {
        assert!(calibration.observe(valid));
    }
    assert!(calibration.fit().unwrap_err().contains("wider range"));
    calibration.clear();
    for i in 0..100 {
        assert!(calibration.observe(InputSample {
            pressure: if i < 50 { 0.2 } else { 0.8 },
            ..valid
        }));
    }
    calibration.fit().unwrap().curve.validate().unwrap();
}

#[test]
fn stylus_only_routing_and_contact_resets_prevent_cross_stroke_prediction() {
    let mut filters = chain(vec![prediction()]);
    let first = filters.process(sample(0.0, 0.0, 1.0));
    assert_eq!(first.route, InputRoute::Canvas);
    assert_eq!(first.confirmed.unwrap().phase, SamplePhase::Begin);
    let second = filters.process(sample(3.0, 0.0, 1.01));
    assert!(second.provisional.is_some());
    let mouse = filters.process(InputSample {
        tool: ToolKind::Mouse,
        ..sample(4.0, 0.0, 1.02)
    });
    assert_eq!(mouse.route, InputRoute::Navigation);
    assert_eq!(mouse.retired_prediction, second.provisional.map(|p| p.id));
    assert!(mouse.confirmed.is_none());
    let eraser = filters.process(InputSample {
        tool: ToolKind::Eraser,
        ..sample(5.0, 0.0, 1.03)
    });
    assert!(eraser.stroke_reset && eraser.provisional.is_none());
    let changed = filters.process(InputSample {
        device_id: 8,
        tool: ToolKind::Eraser,
        ..sample(6.0, 0.0, 1.04)
    });
    assert!(changed.stroke_reset && changed.stroke_id > eraser.stroke_id);
    let backward = filters.process(InputSample {
        device_id: 8,
        tool: ToolKind::Eraser,
        ..sample(7.0, 0.0, 1.0)
    });
    assert_eq!(backward.route, InputRoute::Rejected);
    assert!(backward.stroke_reset);
    let ended = filters.process(InputSample {
        phase: SamplePhase::End,
        ..sample(0.0, 0.0, 2.0)
    });
    assert_eq!(ended.route, InputRoute::Hover);
    assert_eq!(
        filters
            .process(InputSample {
                pressure: 0.0,
                ..sample(0.0, 0.0, 3.0)
            })
            .route,
        InputRoute::Hover
    );
    assert_eq!(
        filters
            .process(InputSample {
                position: [f32::NAN, 0.0],
                ..sample(0.0, 0.0, 4.0)
            })
            .route,
        InputRoute::Rejected
    );
    let mut fallback = FilterChain::new(InputConfig {
        stylus_only: false,
        filters: vec![],
        ..InputConfig::default()
    })
    .unwrap();
    assert_eq!(
        fallback
            .process(InputSample {
                tool: ToolKind::Mouse,
                ..sample(0.0, 0.0, 0.0)
            })
            .route,
        InputRoute::Canvas
    );
}

#[test]
fn prediction_caps_distance_and_ignores_equal_timestamp_and_nonfinite_input() {
    let mut filters = chain(vec![prediction()]);
    filters.process(sample(0.0, 0.0, 1.0));
    assert!(filters.process(sample(1.0, 0.0, 1.0)).provisional.is_none());
    let predicted = filters
        .process(sample(1000.0, 0.0, 1.001))
        .provisional
        .unwrap();
    assert_eq!(predicted.sample.position, [1003.0, 0.0]);
    let rejected = filters.process(InputSample {
        timestamp_seconds: f64::INFINITY,
        ..sample(0.0, 0.0, 0.0)
    });
    assert_eq!(rejected.retired_prediction, Some(predicted.id));
    assert!(rejected.confirmed.is_none() && rejected.provisional.is_none());
    assert!(filters
        .process(sample(10.0, 0.0, 2.0))
        .provisional
        .is_none());
}

type ExtensionEvents = Arc<Mutex<Vec<(u64, bool)>>>;
struct Extension {
    seen: ExtensionEvents,
    change_metadata: bool,
    invalid: bool,
}
impl InputFilter for Extension {
    fn process(&mut self, sample: &mut InputSample, context: FilterContext) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap()
            .push((context.sequence, context.modifiers.suspend_snapping));
        if self.change_metadata {
            sample.timestamp_seconds += 1.0;
        }
        if self.invalid {
            sample.pressure = f32::NAN;
        }
        if !context.modifiers.suspend_snapping {
            sample.position[0] *= 2.0;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.seen.lock().unwrap().push((0, false));
    }
}
fn extended(change_metadata: bool, invalid: bool) -> (FilterChain, ExtensionEvents) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let extension: Box<dyn InputFilter> = Box::new(Extension {
        seen: seen.clone(),
        change_metadata,
        invalid,
    });
    let config = InputConfig {
        filters: vec![
            FilterSpec::Extension {
                name: "guide".into(),
            },
            FilterSpec::Average { window: 2 },
        ],
        ..InputConfig::default()
    };
    let filters =
        FilterChain::with_extensions(config, BTreeMap::from([("guide".into(), extension)]))
            .unwrap();
    (filters, seen)
}
#[test]
fn ordered_extensible_chain_passes_modifiers_and_protects_event_metadata() {
    let (mut filters, seen) = extended(false, false);
    assert_eq!(
        filters
            .process(sample(2.0, 0.0, 0.0))
            .confirmed
            .unwrap()
            .position,
        [4.0, 0.0]
    );
    assert_eq!(
        filters
            .process(sample(4.0, 0.0, 1.0))
            .confirmed
            .unwrap()
            .position,
        [6.0, 0.0]
    );
    let bypass = InputSample {
        modifiers: Modifiers {
            suspend_snapping: true,
            suspend_stabilization: true,
        },
        ..sample(7.0, 0.0, 2.0)
    };
    assert_eq!(
        filters.process(bypass).confirmed.unwrap().position,
        [7.0, 0.0]
    );
    filters.reset();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(0, false), (1, false), (2, false), (3, true), (0, false)]
    );
    for (metadata, invalid) in [(true, false), (false, true)] {
        let (mut filters, _) = extended(metadata, invalid);
        let result = filters.process(sample(1.0, 0.0, 0.0));
        assert_eq!(result.route, InputRoute::Rejected);
        assert!(result.stroke_reset && result.rejected_reason.is_some());
    }
    assert!(FilterChain::new(InputConfig {
        filters: vec![FilterSpec::Extension {
            name: "missing".into()
        }],
        ..InputConfig::default()
    })
    .is_err());
}

#[test]
fn latency_histogram_is_bounded_and_invalid_clocks_are_transactional() {
    let mut tracker = LatencyTracker::new(LatencyConfig {
        bucket_edges_ms: vec![1.0, 4.0, 8.0],
        maximum_pending_samples: 3,
    })
    .unwrap();
    assert_eq!(tracker.report().samples, 0);
    assert_eq!(tracker.report().p50_upper_ms, None);
    for index in 1..=4 {
        tracker.record_sample(index, index as f64).unwrap();
    }
    assert_eq!(tracker.report().dropped_samples, 1);
    assert_eq!(tracker.report().pending_samples, 3);
    assert!(tracker.record_sample(4, 5.0).is_err());
    assert!(tracker.record_sample(5, 3.0).is_err());
    let before = tracker.report();
    assert!(tracker.present_through(4, 3.5).is_err());
    assert_eq!(tracker.report(), before);
    assert_eq!(tracker.present_through(2, 2.0005).unwrap(), 1);
    assert_eq!(tracker.present_through(3, 3.003).unwrap(), 1);
    assert_eq!(tracker.present_through(4, 4.02).unwrap(), 1);
    let report = tracker.report();
    assert_eq!(report.counts, vec![1, 1, 0, 1]);
    assert_eq!(report.p50_upper_ms, Some(4.0));
    assert!((report.p95_upper_ms.unwrap() - 20.0).abs() < 1e-9);
    assert!((report.mean_ms.unwrap() - 23.5 / 3.0).abs() < 1e-9);
    assert_eq!(
        serde_json::from_str::<LatencyReport>(&tracker.report_json().unwrap()).unwrap(),
        report
    );
    assert_eq!(tracker.present_through(4, 10.0).unwrap(), 0);
    assert!(tracker.present_through(4, f64::NAN).is_err());
    tracker.record_sample(5, 5.0).unwrap();
    assert!(tracker.present_through(5, f64::MAX).is_err());
    assert_eq!(tracker.report().pending_samples, 1);
}

#[test]
fn shipped_config_round_trips_and_rejects_unbounded_or_ambiguous_filters() {
    let shipped = InputConfig::default();
    assert_eq!(
        InputConfig::from_json(&serde_json::to_string(&shipped).unwrap()).unwrap(),
        shipped
    );
    assert!(InputConfig::from_json("{}").is_err());
    assert!(InputConfig::from_json(&" ".repeat(256 * 1024 + 1)).is_err());
    for filters in [
        vec![FilterSpec::Average { window: 0 }],
        vec![FilterSpec::Average { window: 4097 }],
        vec![FilterSpec::PullString { distance: -1.0 }],
        vec![FilterSpec::PullString { distance: f32::NAN }],
        vec![prediction(), FilterSpec::Calibration],
        vec![FilterSpec::Extension {
            name: String::new(),
        }],
        vec![FilterSpec::Calibration; 33],
    ] {
        assert!(InputConfig {
            filters,
            ..shipped.clone()
        }
        .validate()
        .is_err());
    }
    let mut bad = shipped.clone();
    bad.calibration.minimum_samples = bad.calibration.sample_limit + 1;
    assert!(bad.validate().is_err());
    let mut bad = shipped.clone();
    bad.latency.bucket_edges_ms = vec![1.0, 1.0];
    assert!(bad.validate().is_err());
    let mut bad = shipped;
    bad.global_pressure.points.clear();
    assert!(bad.validate().is_err());
}

#[test]
fn input_normalization_never_propagates_nonfinite_values() {
    let mut filters = chain(vec![]);
    let input = InputSample {
        pressure: 4.0,
        tilt: [-4.0, 3.0],
        azimuth: -1.0,
        ..sample(0.0, 0.0, 0.0)
    };
    let confirmed = filters.process(input).confirmed.unwrap();
    assert_eq!(confirmed.pressure, 1.0);
    assert_eq!(confirmed.tilt, [-1.0, 1.0]);
    assert!((0.0..std::f32::consts::TAU).contains(&confirmed.azimuth));
    assert_eq!(
        confirmed.point(),
        Point {
            x: 0.0,
            y: 0.0,
            pressure: 1.0
        }
    );
    for bad in [
        InputSample {
            tilt: [f32::INFINITY, 0.0],
            ..input
        },
        InputSample {
            azimuth: f32::NAN,
            ..input
        },
        InputSample {
            timestamp_seconds: -1.0,
            ..input
        },
    ] {
        assert!(bad.validate().is_err());
    }
    assert_eq!(curve(&[[0.0, f32::NAN], [1.0, 1.0]]).evaluate(0.0), 0.0);
    assert_eq!(
        curve(&[[0.0, 0.0], [1.0, f32::INFINITY]]).evaluate(1.0),
        0.0
    );
}

#[test]
fn canvas_mapping_accounts_for_letterbox_and_scale() {
    let map = CanvasMapping {
        origin: [10.0, 20.0],
        display_size: [512.0, 384.0],
        pixel_size: [2048, 1536],
    };
    assert_eq!(
        map.map([266.0, 212.0], 0.5),
        Some(Point {
            x: 1024.0,
            y: 768.0,
            pressure: 0.5
        })
    );
    assert!(map.map([9.0, 20.0], 1.0).is_none());
    assert!(map.map([f32::NAN, 20.0], 1.0).is_none());
    assert!(map.map([522.0, 20.0], 1.0).is_none());
}
#[test]
fn lifting_or_leaving_canvas_does_not_join_strokes() {
    let mut tracker = ContactTracker::default();
    let a = Point {
        x: 1.0,
        y: 2.0,
        pressure: 1.0,
    };
    let b = Point { x: 100.0, ..a };
    assert_eq!(tracker.segment(Some(a), false), Some((a, a)));
    tracker.segment(None, false);
    assert_eq!(tracker.segment(Some(b), false), Some((b, b)));
    tracker.segment(Some(Point { pressure: 0.0, ..b }), false);
    assert_eq!(tracker.segment(Some(a), false), Some((a, a)));
}
#[test]
fn malformed_sample_ends_contact_before_next_valid_point() {
    let mut tracker = ContactTracker::default();
    let a = Point {
        x: 1.0,
        y: 2.0,
        pressure: 1.0,
    };
    let b = Point { x: 100.0, ..a };
    tracker.segment(Some(a), false);
    assert!(tracker
        .segment(Some(Point { x: f32::NAN, ..a }), false)
        .is_none());
    assert_eq!(tracker.segment(Some(b), false), Some((b, b)));
}

#[test]
fn eraser_switch_starts_new_segment() {
    let mut tracker = ContactTracker::default();
    let a = Point {
        x: 1.0,
        y: 2.0,
        pressure: 1.0,
    };
    let b = Point { x: 100.0, ..a };
    tracker.segment(Some(a), false);
    assert_eq!(tracker.segment(Some(b), true), Some((b, b)));
}
