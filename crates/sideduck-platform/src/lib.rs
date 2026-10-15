//! Native tablet input and process residency, isolated from portable application logic.
//!
//! The mock and native sources use the same bounded, deterministic event router.
//! Native tablet events are never consumed: AppKit receives the original event.

mod memory;
mod routing;

#[cfg(target_os = "macos")]
mod macos;

pub use memory::{process_memory, ProcessMemory};
pub use routing::{EventKind, RawTabletEvent, TabletRouter};

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactPhase {
    Down,
    Move,
    Up,
    Hover,
}

/// One native tablet sample, before brush or pressure-curve processing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabletSample {
    /// Content-view coordinates in logical points, with a bottom-left origin.
    pub position: [f64; 2],
    pub window_size: [f64; 2],
    pub window_id: i64,
    /// Normalized native pressure, clamped to 0..=1.
    pub pressure: f32,
    /// Native AppKit tilt components, clamped to -1..=1.
    pub tilt: [f32; 2],
    /// Radians around the native Y-up plane, derived from atan2(tilt_y, tilt_x).
    /// NSEvent supplies tilt components, not a separate Pencil azimuth property.
    pub azimuth: f32,
    /// Seconds since system startup, from the native event clock.
    pub timestamp_seconds: f64,
    pub device_id: u64,
    pub phase: ContactPhase,
    pub eraser: bool,
    /// Native tablet buttons, with bit zero denoting tip contact.
    pub buttons: u64,
}

impl TabletSample {
    /// Convert to egui's top-left logical-point coordinate convention.
    pub fn position_top_left(self) -> [f64; 2] {
        [self.position[0], self.window_size[1] - self.position[1]]
    }
}

/// Sources live on the UI thread; they deliberately do not require Send or Sync.
pub trait TabletSource {
    fn drain_samples(&mut self) -> Vec<TabletSample>;
    /// Total input samples lost to queue pressure or malformed input.
    /// End an active stroke when this changes, rather than bridging a gap.
    fn dropped_samples(&self) -> u64;
    fn description(&self) -> &'static str;
}

#[derive(Debug)]
pub struct PlatformError(pub &'static str);

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PlatformError {}

/// Attach a local AppKit monitor on macOS; provide an empty mock elsewhere.
/// Call on the UI thread after initializing the native event loop.
pub fn create_tablet_source(capacity: usize) -> Result<Box<dyn TabletSource>, PlatformError> {
    if capacity == 0 {
        return Err(PlatformError("Tablet queue capacity must be positive"));
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::NativeTabletSource::new(capacity)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(Box::new(MockTabletSource::new(capacity)))
    }
}

/// Native window number for an exact app-window title, or None if not present.
/// macOS callers must run on the UI thread. The mock has no native windows.
pub fn window_id_for_title(title: &str) -> Option<i64> {
    #[cfg(target_os = "macos")]
    {
        macos::window_id_for_title(title)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = title;
        None
    }
}

/// Deterministic source for tests, recorded traces, and the Linux harness.
pub struct MockTabletSource {
    router: TabletRouter,
}

impl MockTabletSource {
    pub fn new(capacity: usize) -> Self {
        Self {
            router: TabletRouter::new(capacity),
        }
    }

    pub fn push_event(&mut self, event: RawTabletEvent) {
        self.router.push(event);
    }

    pub fn replay(&mut self, events: impl IntoIterator<Item = RawTabletEvent>) {
        for event in events {
            self.push_event(event);
        }
    }
}

impl TabletSource for MockTabletSource {
    fn drain_samples(&mut self) -> Vec<TabletSample> {
        self.router.drain()
    }

    fn dropped_samples(&self) -> u64 {
        self.router.dropped_samples()
    }

    fn description(&self) -> &'static str {
        "Recorded tablet input (mock)"
    }
}
