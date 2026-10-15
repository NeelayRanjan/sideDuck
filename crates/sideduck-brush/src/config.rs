use serde::{Deserialize, Serialize};
use sideduck_input::ResponseCurve;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Driver {
    Pressure,
    Tilt,
    Azimuth,
    Velocity,
    Random,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub driver: Driver,
    pub curve: ResponseCurve,
    pub minimum: f32,
    pub maximum: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub value: f32,
    #[serde(default)]
    pub response: Option<Response>,
}
impl Parameter {
    pub fn constant(value: f32) -> Self {
        Self {
            value,
            response: None,
        }
    }
    pub fn evaluate(&self, drivers: [f32; 5]) -> f32 {
        let factor = self.response.as_ref().map_or(1.0, |response| {
            let index = match response.driver {
                Driver::Pressure => 0,
                Driver::Tilt => 1,
                Driver::Azimuth => 2,
                Driver::Velocity => 3,
                Driver::Random => 4,
            };
            response.minimum
                + (response.maximum - response.minimum) * response.curve.evaluate(drivers[index])
        });
        self.value * factor
    }
    fn validate(&self) -> Result<(), String> {
        if !self.value.is_finite() || self.value.abs() > 1_000_000.0 {
            return Err("Brush parameter must be finite".into());
        }
        if let Some(response) = &self.response {
            response.curve.validate()?;
            if !response.minimum.is_finite()
                || !response.maximum.is_finite()
                || response.minimum < 0.0
                || response.maximum < response.minimum
                || response.maximum > 16.0
            {
                return Err("Invalid brush response range".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushParameters {
    /// Diameter in document pixels; spacing is a fraction of that diameter.
    pub size: Parameter,
    pub spacing: Parameter,
    pub scatter: Parameter,
    pub rotation: Parameter,
    pub rotation_follows_direction: Parameter,
    pub flow: Parameter,
    pub opacity: Parameter,
    pub hardness: Parameter,
    pub grain_scale: Parameter,
    pub grain_motion: Parameter,
    pub wet_mix: Parameter,
    pub pickup_length: Parameter,
    pub hue_jitter: Parameter,
    pub saturation_jitter: Parameter,
    pub value_jitter: Parameter,
    pub smoothing: Parameter,
}
impl BrushParameters {
    pub fn validate(&self) -> Result<(), String> {
        for parameter in [
            &self.size,
            &self.spacing,
            &self.scatter,
            &self.rotation,
            &self.rotation_follows_direction,
            &self.flow,
            &self.opacity,
            &self.hardness,
            &self.grain_scale,
            &self.grain_motion,
            &self.wet_mix,
            &self.pickup_length,
            &self.hue_jitter,
            &self.saturation_jitter,
            &self.value_jitter,
            &self.smoothing,
        ] {
            parameter.validate()?;
        }
        if self.size.value <= 0.0
            || self.size.value > 2048.0
            || self.spacing.value <= 0.0
            || self.grain_scale.value <= 0.0
            || self.pickup_length.value < 0.0
            || self.scatter.value < 0.0
        {
            return Err("Brush size, spacing and texture scale must be positive".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushDefinition {
    pub version: u32,
    pub name: String,
    pub eraser: bool,
    pub parameters: BrushParameters,
}
impl BrushDefinition {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.name.trim().is_empty() || self.name.len() > 128 {
            return Err("Unsupported brush version or invalid name".into());
        }
        self.parameters.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineConfig {
    pub tile_size: u32,
    pub minimum_spacing: f32,
    pub antialias_width: f32,
    pub velocity_normalizer: f32,
    pub tilt_normalizer: f32,
    pub maximum_dabs_per_sample: u32,
    pub maximum_stroke_bytes: u64,
    pub maximum_texture_side: u32,
}
impl EngineConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(8..=256).contains(&self.tile_size)
            || !self.tile_size.is_power_of_two()
            || self.maximum_dabs_per_sample == 0
            || self.maximum_dabs_per_sample > 1_000_000
            || self.maximum_stroke_bytes < u64::from(self.tile_size).pow(2) * 36 + 2048
            || !(0.001..=2048.0).contains(&self.minimum_spacing)
            || self.maximum_texture_side == 0
            || self.maximum_texture_side > 4096
            || [
                self.minimum_spacing,
                self.antialias_width,
                self.velocity_normalizer,
                self.tilt_normalizer,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("Invalid brush engine limits".into());
        }
        Ok(())
    }
}
