use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplePhase {
    Begin,
    #[default]
    Move,
    End,
    Hover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Stylus,
    Eraser,
    Mouse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub suspend_stabilization: bool,
    pub suspend_snapping: bool,
}

/// A document-coordinate sample. Timestamps share one monotonic clock in seconds;
/// the application translates native coordinates before passing samples here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputSample {
    pub position: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub azimuth: f32,
    pub timestamp_seconds: f64,
    pub device_id: u64,
    pub phase: SamplePhase,
    pub tool: ToolKind,
    pub buttons: u64,
    pub modifiers: Modifiers,
}

impl InputSample {
    pub fn point(self) -> crate::Point {
        crate::Point {
            x: self.position[0],
            y: self.position[1],
            pressure: self.pressure,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !self
            .position
            .into_iter()
            .chain(self.tilt)
            .chain([self.pressure, self.azimuth])
            .all(f32::is_finite)
            || !self.timestamp_seconds.is_finite()
            || self.timestamp_seconds < 0.0
        {
            return Err("input sample contains a nonfinite value or negative timestamp".into());
        }
        Ok(())
    }
    pub(crate) fn normalized(mut self) -> Self {
        self.pressure = self.pressure.clamp(0.0, 1.0);
        self.tilt = self.tilt.map(|v| v.clamp(-1.0, 1.0));
        self.azimuth = self.azimuth.rem_euclid(std::f32::consts::TAU);
        self
    }
}
