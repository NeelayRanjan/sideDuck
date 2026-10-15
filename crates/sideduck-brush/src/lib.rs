//! Deterministic scalar prototype rasterizer. GPU brush rendering is not implemented yet.
use serde::Deserialize;
use sideduck_input::Point;

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct BrushSettings {
    pub radius: f32,
    pub minimum_radius_fraction: f32,
    pub spacing_fraction: f32,
    pub pressure_gamma: f32,
}
impl BrushSettings {
    pub fn validate(self) -> Result<Self, &'static str> {
        if !(0.5..=128.0).contains(&self.radius)
            || !(0.01..=1.0).contains(&self.minimum_radius_fraction)
            || !(0.05..=1.0).contains(&self.spacing_fraction)
            || !(0.1..=5.0).contains(&self.pressure_gamma)
        {
            return Err("Invalid brush parameters");
        }
        Ok(self)
    }
    fn radius_at(self, pressure: f32) -> f32 {
        self.radius
            * (self.minimum_radius_fraction
                + (1.0 - self.minimum_radius_fraction)
                    * pressure.clamp(0.0, 1.0).powf(self.pressure_gamma))
    }
}

pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    revision: u64,
}
impl Canvas {
    pub fn new(width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Canvas dimensions must be between 1 and 8192");
        }
        Ok(Self {
            width,
            height,
            pixels: vec![255; width as usize * height as usize * 4],
            revision: 0,
        })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn clear(&mut self) {
        self.pixels.fill(255);
        self.revision += 1;
    }
    pub fn stroke(
        &mut self,
        a: Point,
        b: Point,
        settings: BrushSettings,
        color: [u8; 3],
        eraser: bool,
    ) {
        if settings.validate().is_err()
            || ![a.x, a.y, a.pressure, b.x, b.y, b.pressure]
                .into_iter()
                .all(f32::is_finite)
            || b.pressure <= 0.0
        {
            return;
        }
        // Route only points inside this fixed-size prototype document. This also bounds work.
        if [a, b].into_iter().any(|p| {
            p.x < 0.0 || p.y < 0.0 || p.x >= self.width as f32 || p.y >= self.height as f32
        }) {
            return;
        }
        let length = (b.x - a.x).hypot(b.y - a.y);
        let spacing =
            (settings.radius_at(a.pressure.min(b.pressure)) * settings.spacing_fraction).max(0.5);
        let steps = (length / spacing).ceil().max(1.0) as u32;
        // A single contact stamps once. Interpolation is deterministic for a fixed sample stream.
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let p = Point {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                pressure: a.pressure + (b.pressure - a.pressure) * t,
            };
            self.stamp(
                p,
                settings.radius_at(p.pressure),
                if eraser { [255; 3] } else { color },
            );
        }
        self.revision += 1;
    }
    fn stamp(&mut self, p: Point, radius: f32, color: [u8; 3]) {
        let left = (p.x - radius - 1.0).floor().max(0.0) as u32;
        let top = (p.y - radius - 1.0).floor().max(0.0) as u32;
        let right = ((p.x + radius + 1.0).ceil() as u32).min(self.width);
        let bottom = ((p.y + radius + 1.0).ceil() as u32).min(self.height);
        for y in top..bottom {
            for x in left..right {
                let distance = (x as f32 + 0.5 - p.x).hypot(y as f32 + 0.5 - p.y);
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                let index = (y as usize * self.width as usize + x as usize) * 4;
                for (channel, value) in color.into_iter().enumerate() {
                    self.pixels[index + channel] = (self.pixels[index + channel] as f32
                        * (1.0 - coverage)
                        + value as f32 * coverage)
                        .round() as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> BrushSettings {
        BrushSettings {
            radius: 8.0,
            minimum_radius_fraction: 0.1,
            spacing_fraction: 0.2,
            pressure_gamma: 1.0,
        }
    }
    #[test]
    fn identical_sample_streams_produce_bit_identical_pixels() {
        let mut a = Canvas::new(128, 128).unwrap();
        let mut b = Canvas::new(128, 128).unwrap();
        for canvas in [&mut a, &mut b] {
            canvas.stroke(
                Point {
                    x: 10.0,
                    y: 20.0,
                    pressure: 0.1,
                },
                Point {
                    x: 110.0,
                    y: 100.0,
                    pressure: 1.0,
                },
                settings(),
                [20, 50, 40],
                false,
            );
        }
        assert_eq!(a.pixels, b.pixels);
        assert!(a.pixels.iter().any(|v| *v < 255));
    }
    #[test]
    fn pressure_changes_footprint_and_eraser_restores_white() {
        let mut a = Canvas::new(64, 64).unwrap();
        let mut b = Canvas::new(64, 64).unwrap();
        let p = Point {
            x: 32.0,
            y: 32.0,
            pressure: 0.1,
        };
        a.stroke(p, p, settings(), [0; 3], false);
        let q = Point { pressure: 1.0, ..p };
        b.stroke(q, q, settings(), [0; 3], false);
        assert!(
            a.pixels.iter().filter(|v| **v < 255).count()
                < b.pixels.iter().filter(|v| **v < 255).count()
        );
        a.stroke(q, q, settings(), [0; 3], true);
        assert!(a.pixels.iter().all(|v| *v == 255));
    }
    #[test]
    fn zero_pressure_and_nonfinite_samples_do_not_paint() {
        let mut a = Canvas::new(64, 64).unwrap();
        let p = Point {
            x: 32.0,
            y: 32.0,
            pressure: 0.0,
        };
        a.stroke(p, p, settings(), [0; 3], false);
        a.stroke(
            p,
            Point {
                x: f32::NAN,
                pressure: 1.0,
                ..p
            },
            settings(),
            [0; 3],
            false,
        );
        assert!(a.pixels.iter().all(|v| *v == 255));
    }
    #[test]
    fn circle_center_and_outside_match_analytic_reference() {
        let mut a = Canvas::new(64, 64).unwrap();
        let p = Point {
            x: 32.5,
            y: 32.5,
            pressure: 1.0,
        };
        a.stroke(p, p, settings(), [10, 20, 30], false);
        assert_eq!(
            &a.pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4],
            &[10, 20, 30, 255]
        );
        assert_eq!(&a.pixels[0..4], &[255; 4]);
    }
}
