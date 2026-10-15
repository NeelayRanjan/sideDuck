//! Deterministic linear-light brush strokes, sparse accumulation, and open brush packages.
mod prototype;
pub use prototype::{BrushSettings, Canvas};
mod config;
mod engine;
mod package;
mod raster;
mod texture;
pub use config::*;
pub use engine::*;
pub use package::*;
pub use raster::*;
pub use texture::*;
mod benchmark;
pub use benchmark::{run_throughput_gate, BrushBenchmarkReport};

#[cfg(test)]
mod tests;
