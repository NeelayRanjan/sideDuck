//! Portable platform contract; native adapters arrive with the drawing prototype.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabletSample {
    pub position: [f64; 2],
    pub pressure: f32,
}

pub trait TabletSource {
    fn drain_samples(&mut self) -> Vec<TabletSample>;
}

#[derive(Default)]
pub struct MockTabletSource {
    pub samples: Vec<TabletSample>,
}

impl TabletSource for MockTabletSource {
    fn drain_samples(&mut self) -> Vec<TabletSample> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_platform_replays_sample_stream() {
        let sample = TabletSample {
            position: [1.0, 2.0],
            pressure: 0.5,
        };
        let mut source = MockTabletSource {
            samples: vec![sample],
        };
        assert_eq!(source.drain_samples(), vec![sample]);
        assert!(source.drain_samples().is_empty());
    }
}
