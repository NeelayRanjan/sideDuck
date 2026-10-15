//! Portable tablet samples, deterministic filters, calibration, and latency accounting.
mod calibration;
mod config;
mod curve;
mod filters;
mod latency;
mod sample;
pub use calibration::{CalibrationResult, PressureCalibrator};
pub use config::{CalibrationConfig, FilterSpec, InputConfig, LatencyConfig};
pub use curve::ResponseCurve;
pub use filters::{
    FilterChain, FilterContext, InputFilter, InputRoute, Prediction, ProcessedSample,
};
pub use latency::{LatencyReport, LatencyTracker};
pub use sample::{InputSample, Modifiers, SamplePhase, ToolKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasMapping {
    pub origin: [f32; 2],
    pub display_size: [f32; 2],
    pub pixel_size: [u32; 2],
}

impl CanvasMapping {
    pub fn map(&self, position: [f32; 2], pressure: f32) -> Option<Point> {
        if !position.into_iter().chain([pressure]).all(f32::is_finite)
            || !self.origin.into_iter().all(f32::is_finite)
            || !self
                .display_size
                .into_iter()
                .all(|v| v.is_finite() && v > 0.0)
            || self.pixel_size.contains(&0)
        {
            return None;
        }
        let x = (position[0] - self.origin[0]) / self.display_size[0];
        let y = (position[1] - self.origin[1]) / self.display_size[1];
        if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
            return None;
        }
        Some(Point {
            x: x * self.pixel_size[0] as f32,
            y: y * self.pixel_size[1] as f32,
            pressure: pressure.clamp(0.0, 1.0),
        })
    }
}

/// Stops interpolation whenever input leaves the canvas, changes source, or loses contact.
#[derive(Default)]
pub struct ContactTracker {
    previous: Option<Point>,
    eraser: bool,
}
impl ContactTracker {
    pub fn reset(&mut self) {
        self.previous = None;
    }
    pub fn segment(&mut self, point: Option<Point>, eraser: bool) -> Option<(Point, Point)> {
        let Some(point) = point.filter(|p| {
            p.x.is_finite() && p.y.is_finite() && p.pressure.is_finite() && p.pressure > 0.0
        }) else {
            self.reset();
            return None;
        };
        let previous = if self.eraser == eraser {
            self.previous.unwrap_or(point)
        } else {
            point
        };
        self.eraser = eraser;
        self.previous = Some(point);
        Some((previous, point))
    }
}

#[cfg(test)]
mod tests;
