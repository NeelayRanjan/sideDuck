use crate::ResponseCurve;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilterSpec {
    Calibration,
    GlobalPressure,
    BrushPressure,
    Average {
        window: usize,
    },
    PullString {
        distance: f32,
    },
    Prediction {
        horizon_ms: f64,
        max_distance: f32,
        minimum_interval_ms: f64,
    },
    /// Application-supplied portable filter, inserted at this exact chain position.
    Extension {
        name: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationConfig {
    pub sample_limit: usize,
    pub minimum_samples: usize,
    pub low_quantile: f32,
    pub high_quantile: f32,
    pub curve_knots: usize,
    pub minimum_pressure_span: f32,
    pub histogram_bins: usize,
}
impl CalibrationConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=1_000_000).contains(&self.sample_limit)
            || !(2..=self.sample_limit).contains(&self.minimum_samples)
            || !self.low_quantile.is_finite()
            || !self.high_quantile.is_finite()
            || !(0.0..1.0).contains(&self.low_quantile)
            || !(0.0..=1.0).contains(&self.high_quantile)
            || self.high_quantile <= self.low_quantile
            || !(2..=254).contains(&self.curve_knots)
            || !self.minimum_pressure_span.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_pressure_span)
            || self.minimum_pressure_span == 0.0
            || !(2..=1024).contains(&self.histogram_bins)
        {
            return Err("invalid bounded pressure calibration configuration".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LatencyConfig {
    pub bucket_edges_ms: Vec<f64>,
    pub maximum_pending_samples: usize,
}
impl LatencyConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.bucket_edges_ms.is_empty()
            || self.bucket_edges_ms.len() > 1024
            || self
                .bucket_edges_ms
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.0)
            || self
                .bucket_edges_ms
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || !(1..=1_000_000).contains(&self.maximum_pending_samples)
        {
            return Err("latency buckets must increase and pending input must be bounded".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputConfig {
    pub stylus_only: bool,
    pub global_pressure: ResponseCurve,
    pub brush_pressure: ResponseCurve,
    pub filters: Vec<FilterSpec>,
    pub calibration: CalibrationConfig,
    pub latency: LatencyConfig,
}
impl Default for InputConfig {
    fn default() -> Self {
        Self::from_json(include_str!("../../../config/input.json"))
            .expect("shipped input configuration is valid")
    }
}
impl InputConfig {
    pub fn from_json(json: &str) -> Result<Self, String> {
        if json.len() > 256 * 1024 {
            return Err("input configuration exceeds 256 KiB".into());
        }
        let value: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.global_pressure.validate()?;
        self.brush_pressure.validate()?;
        self.calibration.validate()?;
        self.latency.validate()?;
        if self.filters.len() > 32 {
            return Err("input chain exceeds 32 filters".into());
        }
        for (index, filter) in self.filters.iter().enumerate() {
            match filter {
                FilterSpec::Average { window } if !(1..=4096).contains(window) => {
                    return Err("average window must be between 1 and 4096 samples".into())
                }
                FilterSpec::PullString { distance } if !distance.is_finite() || *distance < 0.0 => {
                    return Err("pull-string distance must be finite and nonnegative".into())
                }
                FilterSpec::Prediction {
                    horizon_ms,
                    max_distance,
                    minimum_interval_ms,
                } => {
                    if index + 1 != self.filters.len()
                        || !horizon_ms.is_finite()
                        || !(0.0..=1000.0).contains(horizon_ms)
                        || !max_distance.is_finite()
                        || *max_distance < 0.0
                        || !minimum_interval_ms.is_finite()
                        || *minimum_interval_ms <= 0.0
                    {
                        return Err("prediction must be last with finite bounded horizon and positive minimum interval".into());
                    }
                }
                FilterSpec::Extension { name } if name.is_empty() || name.len() > 256 => {
                    return Err("extension filter needs a short stable name".into())
                }
                _ => {}
            }
        }
        Ok(())
    }
}
