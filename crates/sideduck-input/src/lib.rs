//! Portable coordinate mapping and contact routing for the prototype.
#[derive(Clone, Copy, Debug, PartialEq)]
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
mod tests {
    use super::*;
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
}
