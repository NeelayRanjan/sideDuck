use crate::LatencyConfig;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    pub bucket_edges_ms: Vec<f64>,
    /// One overflow bucket follows the configured upper-bound buckets.
    pub counts: Vec<u64>,
    pub samples: u64,
    pub minimum_ms: Option<f64>,
    pub mean_ms: Option<f64>,
    pub maximum_ms: Option<f64>,
    pub p50_upper_ms: Option<f64>,
    pub p95_upper_ms: Option<f64>,
    pub dropped_samples: u64,
    pub pending_samples: usize,
}

/// Bounded sample-to-present instrumentation. Both timestamps must use the same
/// monotonic clock. This measures the application's presentation notification,
/// not physical display scanout or sample-to-photon latency.
pub struct LatencyTracker {
    config: LatencyConfig,
    pending: VecDeque<(u64, f64)>,
    last_sample: Option<(u64, f64)>,
    counts: Vec<u64>,
    samples: u64,
    dropped: u64,
    sum_ms: f64,
    min_ms: f64,
    max_ms: f64,
}
impl LatencyTracker {
    pub fn new(config: LatencyConfig) -> Result<Self, String> {
        config.validate()?;
        let counts = vec![0; config.bucket_edges_ms.len() + 1];
        let pending = VecDeque::with_capacity(config.maximum_pending_samples);
        Ok(Self {
            config,
            pending,
            last_sample: None,
            counts,
            samples: 0,
            dropped: 0,
            sum_ms: 0.0,
            min_ms: f64::INFINITY,
            max_ms: 0.0,
        })
    }
    pub fn record_sample(&mut self, sequence: u64, timestamp_seconds: f64) -> Result<(), String> {
        if !timestamp_seconds.is_finite()
            || timestamp_seconds < 0.0
            || self
                .last_sample
                .is_some_and(|(id, time)| sequence <= id || timestamp_seconds < time)
        {
            return Err(
                "latency samples need increasing sequence IDs and monotonic finite timestamps"
                    .into(),
            );
        }
        if self.pending.len() == self.config.maximum_pending_samples {
            self.pending.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push_back((sequence, timestamp_seconds));
        self.last_sample = Some((sequence, timestamp_seconds));
        Ok(())
    }
    /// Mark all retained confirmed samples through this sequence as included in
    /// the presented frame. Invalid clocks leave the pending queue unchanged.
    pub fn present_through(
        &mut self,
        sequence: u64,
        presented_seconds: f64,
    ) -> Result<usize, String> {
        if !presented_seconds.is_finite() || presented_seconds < 0.0 {
            return Err("presentation timestamp must be finite and nonnegative".into());
        }
        let count = self
            .pending
            .iter()
            .take_while(|(id, _)| *id <= sequence)
            .count();
        let mut added = 0.0;
        for &(_, time) in self.pending.iter().take(count) {
            let ms = (presented_seconds - time) * 1000.0;
            if ms < 0.0 || !ms.is_finite() {
                return Err("presentation must follow samples on the same clock".into());
            }
            added += ms;
        }
        if !added.is_finite()
            || !(self.sum_ms + added).is_finite()
            || self.samples.checked_add(count as u64).is_none()
        {
            return Err("latency histogram accumulation overflow".into());
        }
        for _ in 0..count {
            let (_, time) = self.pending.pop_front().expect("validated pending sample");
            let ms = (presented_seconds - time) * 1000.0;
            let bucket = self
                .config
                .bucket_edges_ms
                .partition_point(|edge| *edge < ms);
            self.counts[bucket] += 1;
            self.samples += 1;
            self.sum_ms += ms;
            self.min_ms = self.min_ms.min(ms);
            self.max_ms = self.max_ms.max(ms);
        }
        Ok(count)
    }
    fn percentile(&self, fraction: f64) -> Option<f64> {
        if self.samples == 0 {
            return None;
        }
        let wanted = (self.samples as f64 * fraction).ceil() as u64;
        let mut observed = 0;
        for (bucket, count) in self.counts.iter().enumerate() {
            observed += count;
            if observed >= wanted {
                return Some(
                    self.config
                        .bucket_edges_ms
                        .get(bucket)
                        .copied()
                        .unwrap_or(self.max_ms),
                );
            }
        }
        Some(self.max_ms)
    }
    pub fn report(&self) -> LatencyReport {
        LatencyReport {
            bucket_edges_ms: self.config.bucket_edges_ms.clone(),
            counts: self.counts.clone(),
            samples: self.samples,
            minimum_ms: (self.samples > 0).then_some(self.min_ms),
            mean_ms: (self.samples > 0).then(|| self.sum_ms / self.samples as f64),
            maximum_ms: (self.samples > 0).then_some(self.max_ms),
            p50_upper_ms: self.percentile(0.5),
            p95_upper_ms: self.percentile(0.95),
            dropped_samples: self.dropped,
            pending_samples: self.pending.len(),
        }
    }
    /// A compact JSON histogram suitable for an opt-in local diagnostics log.
    pub fn report_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.report()).map_err(|error| error.to_string())
    }
}
