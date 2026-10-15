use serde::{Deserialize, Serialize};

/// Piecewise-linear normalized response. Y may decrease for inverse brush
/// responses. Pressure calibration uses a monotonic instance of this same type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseCurve {
    pub points: Vec<[f32; 2]>,
}
impl Default for ResponseCurve {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }
}
impl ResponseCurve {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=256).contains(&self.points.len())
            || self.points.first().is_none_or(|p| p[0] != 0.0)
            || self.points.last().is_none_or(|p| p[0] != 1.0)
            || self
                .points
                .iter()
                .any(|p| p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)))
            || self.points.windows(2).any(|p| p[0][0] >= p[1][0])
        {
            return Err("response curve needs 2 through 256 finite normalized points with increasing X from 0 to 1".into());
        }
        Ok(())
    }
    /// Clamp finite input into the normalized domain; nonfinite input yields 0.
    /// Validate once when installing. Malformed segments use a finite fallback.
    pub fn evaluate(&self, value: f32) -> f32 {
        if !value.is_finite() || self.points.len() < 2 {
            return 0.0;
        }
        let x = value.clamp(0.0, 1.0);
        let high = self.points.partition_point(|point| point[0] < x);
        if high == 0 {
            return finite_unit(self.points[0][1]);
        }
        if high == self.points.len() {
            return finite_unit(self.points[high - 1][1]);
        }
        let a = self.points[high - 1];
        let b = self.points[high];
        let span = b[0] - a[0];
        if !span.is_finite() || span <= 0.0 {
            return 0.0;
        }
        let y = a[1] + (b[1] - a[1]) * ((x - a[0]) / span);
        if y.is_finite() {
            y.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editable_response_curves_interpolate_clamp_and_round_trip() {
        let curve = ResponseCurve {
            points: vec![[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]],
        };
        curve.validate().unwrap();
        assert_eq!(curve.evaluate(0.25), 0.125);
        assert_eq!(curve.evaluate(0.75), 0.625);
        assert_eq!(curve.evaluate(-1.0), 0.0);
        assert_eq!(curve.evaluate(2.0), 1.0);
        assert_eq!(curve.evaluate(f32::NAN), 0.0);
        assert_eq!(
            serde_json::from_str::<ResponseCurve>(&serde_json::to_string(&curve).unwrap()).unwrap(),
            curve
        );
        for points in [
            vec![],
            vec![[0.0, 0.0]],
            vec![[0.0, 0.0], [0.0, 1.0]],
            vec![[0.0, 0.0], [1.0, f32::NAN]],
        ] {
            assert!(ResponseCurve { points }.validate().is_err());
        }
        let reverse = ResponseCurve {
            points: vec![[0.0, 1.0], [1.0, 0.0]],
        };
        reverse.validate().unwrap();
        assert_eq!(reverse.evaluate(0.75), 0.25);
    }
}
