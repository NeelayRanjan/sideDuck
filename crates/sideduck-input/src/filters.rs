use crate::{
    FilterSpec, InputConfig, InputSample, Modifiers, ResponseCurve, SamplePhase, ToolKind,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRoute {
    Canvas,
    Navigation,
    Hover,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub id: u64,
    pub based_on_sequence: u64,
    pub stroke_id: u64,
    pub sample: InputSample,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessedSample {
    pub sequence: u64,
    pub stroke_id: u64,
    pub route: InputRoute,
    /// Only confirmed samples enter a document operation log or committed raster.
    pub confirmed: Option<InputSample>,
    /// Draw on a separate temporary surface; replace it at the next real event.
    pub provisional: Option<Prediction>,
    /// Clear this prior temporary surface before drawing confirmed/new preview data.
    pub retired_prediction: Option<u64>,
    pub stroke_reset: bool,
    pub rejected_reason: Option<String>,
}
#[derive(Clone, Copy, Debug)]
pub struct FilterContext {
    pub sequence: u64,
    pub stroke_id: u64,
    pub modifiers: Modifiers,
}

/// Portable extension point for future snapping and guide filters. Extensions
/// receive real samples only, in configured order, and must be deterministic.
/// They may change position, pressure, tilt, and azimuth. Event identity, phase,
/// timestamp, buttons, and modifiers are immutable and checked after each stage.
pub trait InputFilter: Send {
    fn process(&mut self, sample: &mut InputSample, context: FilterContext) -> Result<(), String>;
    fn reset(&mut self) {}
}

enum State {
    Calibration,
    GlobalPressure,
    BrushPressure,
    Average {
        window: usize,
        points: VecDeque<[f32; 2]>,
    },
    PullString {
        distance: f32,
        anchor: Option<[f32; 2]>,
    },
    Prediction {
        horizon_ms: f64,
        max_distance: f32,
        minimum_interval_ms: f64,
    },
    Extension(Box<dyn InputFilter>),
}
impl State {
    fn reset(&mut self) {
        match self {
            Self::Average { points, .. } => points.clear(),
            Self::PullString { anchor, .. } => *anchor = None,
            Self::Extension(extension) => extension.reset(),
            _ => {}
        }
    }
}

pub struct FilterChain {
    config: InputConfig,
    states: Vec<State>,
    calibration: ResponseCurve,
    previous: Option<InputSample>,
    contact: Option<(u64, ToolKind)>,
    sequence: u64,
    stroke_id: u64,
    prediction: Option<Prediction>,
}
impl FilterChain {
    pub fn new(config: InputConfig) -> Result<Self, String> {
        Self::with_extensions(config, BTreeMap::new())
    }
    pub fn with_extensions(
        config: InputConfig,
        mut extensions: BTreeMap<String, Box<dyn InputFilter>>,
    ) -> Result<Self, String> {
        config.validate()?;
        let mut states = Vec::with_capacity(config.filters.len());
        for filter in &config.filters {
            states.push(match filter {
                FilterSpec::Calibration => State::Calibration,
                FilterSpec::GlobalPressure => State::GlobalPressure,
                FilterSpec::BrushPressure => State::BrushPressure,
                FilterSpec::Average { window } => State::Average {
                    window: *window,
                    points: VecDeque::with_capacity(*window),
                },
                FilterSpec::PullString { distance } => State::PullString {
                    distance: *distance,
                    anchor: None,
                },
                FilterSpec::Prediction {
                    horizon_ms,
                    max_distance,
                    minimum_interval_ms,
                } => State::Prediction {
                    horizon_ms: *horizon_ms,
                    max_distance: *max_distance,
                    minimum_interval_ms: *minimum_interval_ms,
                },
                FilterSpec::Extension { name } => {
                    State::Extension(extensions.remove(name).ok_or_else(|| {
                        format!("input extension '{name}' is not registered or is duplicated")
                    })?)
                }
            });
        }
        Ok(Self {
            config,
            states,
            calibration: ResponseCurve::default(),
            previous: None,
            contact: None,
            sequence: 0,
            stroke_id: 0,
            prediction: None,
        })
    }
    pub fn config(&self) -> &InputConfig {
        &self.config
    }
    pub fn calibration_curve(&self) -> &ResponseCurve {
        &self.calibration
    }
    pub fn set_global_pressure(&mut self, curve: ResponseCurve) -> Result<Option<u64>, String> {
        curve.validate()?;
        let retired = self.reset();
        self.config.global_pressure = curve;
        Ok(retired)
    }
    /// Install between strokes; resetting returns the provisional ID to erase.
    pub fn set_calibration(&mut self, curve: ResponseCurve) -> Result<Option<u64>, String> {
        curve.validate()?;
        if curve.points.windows(2).any(|pair| pair[0][1] > pair[1][1]) {
            return Err("pressure calibration must be monotonic".into());
        }
        let retired = self.reset();
        self.calibration = curve;
        Ok(retired)
    }
    pub fn set_brush_pressure(&mut self, curve: ResponseCurve) -> Result<Option<u64>, String> {
        curve.validate()?;
        let retired = self.reset();
        self.config.brush_pressure = curve;
        Ok(retired)
    }
    pub fn reset(&mut self) -> Option<u64> {
        for state in &mut self.states {
            state.reset();
        }
        self.previous = None;
        self.contact = None;
        self.prediction.take().map(|prediction| prediction.id)
    }
    pub fn retained_history_samples(&self) -> usize {
        self.states
            .iter()
            .map(|state| match state {
                State::Average { points, .. } => points.len(),
                _ => 0,
            })
            .sum()
    }
    pub fn process(&mut self, input: InputSample) -> ProcessedSample {
        let retired_prediction = self.prediction.take().map(|prediction| prediction.id);
        let mut result = ProcessedSample {
            sequence: self.sequence,
            stroke_id: self.stroke_id,
            route: InputRoute::Rejected,
            confirmed: None,
            provisional: None,
            retired_prediction,
            stroke_reset: false,
            rejected_reason: None,
        };
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.reset();
            result.stroke_reset = true;
            result.rejected_reason = Some("input sequence identifier exhausted".into());
            return result;
        };
        self.sequence = sequence;
        result.sequence = sequence;
        if let Err(error) = input.validate() {
            self.reset();
            result.stroke_reset = true;
            result.rejected_reason = Some(error);
            return result;
        }
        let mut sample = input.normalized();
        if self.config.stylus_only && sample.tool == ToolKind::Mouse {
            self.reset();
            result.route = InputRoute::Navigation;
            result.stroke_reset = true;
            return result;
        }
        if sample.phase == SamplePhase::Hover
            || (sample.pressure == 0.0 && sample.phase != SamplePhase::End)
        {
            self.reset();
            result.route = InputRoute::Hover;
            result.stroke_reset = true;
            return result;
        }
        let key = (sample.device_id, sample.tool);
        let new_contact = self.contact != Some(key) || sample.phase == SamplePhase::Begin;
        if new_contact {
            self.reset();
            if sample.phase == SamplePhase::End {
                result.route = InputRoute::Hover;
                result.stroke_reset = true;
                return result;
            }
            let Some(stroke) = self.stroke_id.checked_add(1) else {
                result.rejected_reason = Some("stroke identifier exhausted".into());
                result.stroke_reset = true;
                return result;
            };
            self.stroke_id = stroke;
            self.contact = Some(key);
            sample.phase = SamplePhase::Begin;
            result.stroke_reset = true;
        }
        result.stroke_id = self.stroke_id;
        if self
            .previous
            .is_some_and(|previous| sample.timestamp_seconds < previous.timestamp_seconds)
        {
            self.reset();
            result.stroke_reset = true;
            result.rejected_reason =
                Some("input timestamps must not move backward within a stroke".into());
            return result;
        }
        let context = FilterContext {
            sequence,
            stroke_id: self.stroke_id,
            modifiers: sample.modifiers,
        };
        let identity = sample;
        let mut prediction_options = None;
        let mut filter_error = None;
        for state in &mut self.states {
            match state {
                State::Calibration => sample.pressure = self.calibration.evaluate(sample.pressure),
                State::GlobalPressure => {
                    sample.pressure = self.config.global_pressure.evaluate(sample.pressure)
                }
                State::BrushPressure => {
                    sample.pressure = self.config.brush_pressure.evaluate(sample.pressure)
                }
                State::Average { window, points } => {
                    if context.modifiers.suspend_stabilization {
                        points.clear();
                    }
                    if points.len() == *window {
                        points.pop_front();
                    }
                    points.push_back(sample.position);
                    if !context.modifiers.suspend_stabilization {
                        for axis in 0..2 {
                            sample.position[axis] = (points
                                .iter()
                                .map(|point| f64::from(point[axis]))
                                .sum::<f64>()
                                / points.len() as f64)
                                as f32;
                        }
                    }
                }
                State::PullString { distance, anchor } => {
                    if context.modifiers.suspend_stabilization || anchor.is_none() {
                        *anchor = Some(sample.position);
                    }
                    let start = anchor.expect("initialized pull-string anchor");
                    let delta = [
                        f64::from(sample.position[0]) - f64::from(start[0]),
                        f64::from(sample.position[1]) - f64::from(start[1]),
                    ];
                    let length = delta[0].hypot(delta[1]);
                    if length > f64::from(*distance) {
                        let fraction = 1.0 - f64::from(*distance) / length;
                        *anchor = Some(std::array::from_fn(|axis| {
                            (f64::from(start[axis]) + delta[axis] * fraction) as f32
                        }));
                    }
                    sample.position = anchor.expect("initialized pull-string anchor");
                }
                State::Prediction {
                    horizon_ms,
                    max_distance,
                    minimum_interval_ms,
                } => prediction_options = Some((*horizon_ms, *max_distance, *minimum_interval_ms)),
                State::Extension(extension) => {
                    if let Err(error) = extension.process(&mut sample, context) {
                        filter_error = Some(error);
                        break;
                    }
                }
            }
            if sample.validate().is_err() {
                filter_error = Some("input filter produced an invalid sample".into());
                break;
            }
            if sample.device_id != identity.device_id
                || sample.tool != identity.tool
                || sample.phase != identity.phase
                || sample.timestamp_seconds != identity.timestamp_seconds
                || sample.buttons != identity.buttons
                || sample.modifiers != identity.modifiers
            {
                filter_error = Some("input filter changed immutable event metadata".into());
                break;
            }
            sample = sample.normalized();
        }
        if let Some(error) = filter_error {
            self.reset();
            result.stroke_reset = true;
            result.rejected_reason = Some(error);
            return result;
        }
        if sample.phase == SamplePhase::End {
            sample.pressure = 0.0;
        }
        result.route = InputRoute::Canvas;
        result.confirmed = Some(sample);
        if sample.phase != SamplePhase::End {
            if let (Some(previous), Some((horizon_ms, max_distance, minimum_interval_ms))) =
                (self.previous, prediction_options)
            {
                let dt = sample.timestamp_seconds - previous.timestamp_seconds;
                if dt >= minimum_interval_ms / 1000.0 && horizon_ms > 0.0 && max_distance > 0.0 {
                    let factor = (horizon_ms / 1000.0) / dt;
                    let mut delta = [
                        f64::from(sample.position[0]) - f64::from(previous.position[0]),
                        f64::from(sample.position[1]) - f64::from(previous.position[1]),
                    ]
                    .map(|v| v * factor);
                    let length = delta[0].hypot(delta[1]);
                    if length > f64::from(max_distance) {
                        delta
                            .iter_mut()
                            .for_each(|v| *v *= f64::from(max_distance) / length);
                    }
                    let mut predicted = sample;
                    predicted.position = std::array::from_fn(|axis| {
                        (f64::from(sample.position[axis]) + delta[axis]) as f32
                    });
                    predicted.timestamp_seconds += horizon_ms / 1000.0;
                    predicted.phase = SamplePhase::Move;
                    if predicted.validate().is_ok() {
                        let prediction = Prediction {
                            id: sequence,
                            based_on_sequence: sequence,
                            stroke_id: self.stroke_id,
                            sample: predicted,
                        };
                        self.prediction = Some(prediction);
                        result.provisional = Some(prediction);
                    }
                }
            }
            self.previous = Some(sample);
        } else {
            self.reset();
        }
        result
    }
}
