use crate::{CalibrationConfig, InputSample, ResponseCurve, SamplePhase, ToolKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationResult {
    pub curve: ResponseCurve,
    pub sample_count: usize,
    pub before_histogram: Vec<u64>,
    pub after_histogram: Vec<u64>,
    pub low_pressure: f32,
    pub high_pressure: f32,
}

/// Collects bounded real contact samples. Predictions must not be observed.
pub struct PressureCalibrator {
    config: CalibrationConfig,
    samples: Vec<f32>,
}
impl PressureCalibrator {
    pub fn new(config: CalibrationConfig) -> Result<Self, String> {
        config.validate()?;
        let samples = Vec::with_capacity(config.sample_limit);
        Ok(Self { config, samples })
    }
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }
    pub fn clear(&mut self) {
        self.samples.clear();
    }
    pub fn observe(&mut self, sample: InputSample) -> bool {
        if sample.validate().is_err()
            || sample.tool == ToolKind::Mouse
            || !matches!(sample.phase, SamplePhase::Begin | SamplePhase::Move)
            || sample.pressure <= 0.0
            || self.samples.len() == self.config.sample_limit
        {
            return false;
        }
        self.samples.push(sample.pressure.clamp(0.0, 1.0));
        true
    }
    /// Empirical quantiles map the observed pressure distribution into a uniform
    /// normalized response. Low/high quantiles trim configurable outliers.
    pub fn fit(&self) -> Result<CalibrationResult, String> {
        if self.samples.len() < self.config.minimum_samples {
            return Err("more calibration contact samples are required".into());
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(f32::total_cmp);
        let quantile = |q: f32| {
            let index = q as f64 * (sorted.len() - 1) as f64;
            let low = index.floor() as usize;
            let high = index.ceil() as usize;
            sorted[low] + (sorted[high] - sorted[low]) * (index - low as f64) as f32
        };
        let low_pressure = quantile(self.config.low_quantile);
        let high_pressure = quantile(self.config.high_quantile);
        if high_pressure - low_pressure < self.config.minimum_pressure_span {
            return Err("calibration needs a wider range of contact pressures".into());
        }
        let mut points = vec![[0.0, 0.0]];
        for index in 0..self.config.curve_knots {
            let y = index as f32 / (self.config.curve_knots - 1) as f32;
            let q = self.config.low_quantile
                + y * (self.config.high_quantile - self.config.low_quantile);
            let x = quantile(q);
            if x <= 0.0 || x >= 1.0 {
                continue;
            }
            if points.last().is_some_and(|point| point[0] == x) {
                points.last_mut().expect("existing quantile")[1] = y;
            } else {
                points.push([x, y]);
            }
        }
        points.push([1.0, 1.0]);
        let curve = ResponseCurve { points };
        curve.validate()?;
        let mut before_histogram = vec![0; self.config.histogram_bins];
        let mut after_histogram = before_histogram.clone();
        let bucket = |pressure: f32| {
            ((pressure * self.config.histogram_bins as f32) as usize)
                .min(self.config.histogram_bins - 1)
        };
        for &sample in &self.samples {
            before_histogram[bucket(sample)] += 1;
            after_histogram[bucket(curve.evaluate(sample))] += 1;
        }
        Ok(CalibrationResult {
            curve,
            sample_count: sorted.len(),
            before_histogram,
            after_histogram,
            low_pressure,
            high_pressure,
        })
    }
}
