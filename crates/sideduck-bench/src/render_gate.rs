//! Measurements shared by the real-surface render gate. Thresholds are fixed;
//! failed runs retain their measurements before returning a nonzero exit status.

use serde::Serialize;
use sideduck_composite::cache::LayerDescriptor;
use sideduck_composite::BlendMode;
use sideduck_platform::process_memory;
use sideduck_tile::{PixelFormat, StoreOptions, TileStore};
use std::path::{Path, PathBuf};

use crate::{Config, CORPUS_VERSION, LAYERS, MIB, STORED_TILES, TILE_SIZE};

pub const PROCESS_CEILING: u64 = 4096 * MIB;
pub const GPU_CEILING: u64 = 2048 * MIB;
pub const TILE_CACHE: usize = 64 * MIB as usize;
pub const DERIVED_CACHE: usize = 128 * MIB as usize;

#[derive(Debug, Serialize, Default)]
pub struct RenderMeasurements {
    pub canvas: [u32; 2],
    pub layers: u32,
    pub occupied_tiles: usize,
    pub empty_percent: u32,
    pub source_open_ms: f64,
    pub cache_preparation_seconds: f64,
    pub active_switch_ms: Vec<f64>,
    pub active_switch_max_ms: f64,
    pub process_peak_bytes: u64,
    pub process_ceiling_bytes: u64,
    pub gpu_peak_bytes: u64,
    pub mip_level: u8,
    pub conditions: String,
}

impl RenderMeasurements {
    pub fn sample_memory(&mut self) -> Result<(), String> {
        let memory = process_memory().map_err(|error| error.to_string())?;
        self.process_peak_bytes = self.process_peak_bytes.max(memory.peak_resident_bytes);
        self.process_ceiling_bytes = PROCESS_CEILING;
        if self.process_peak_bytes >= PROCESS_CEILING {
            return Err(format!(
                "process residency {} bytes reached the {} byte ceiling",
                self.process_peak_bytes, PROCESS_CEILING
            ));
        }
        if self.process_peak_bytes.saturating_add(self.gpu_peak_bytes) >= PROCESS_CEILING {
            return Err("combined process peak and owned GPU image peak reached the residency ceiling (conservative on unified memory)".into());
        }
        Ok(())
    }

    pub fn record_switch(&mut self, elapsed_ms: f64) {
        self.active_switch_ms.push(elapsed_ms);
        self.active_switch_max_ms = self.active_switch_max_ms.max(elapsed_ms);
    }

    pub fn assert_budgets(&self) -> Result<(), String> {
        if self.source_open_ms >= 2000.0 {
            return Err(format!(
                "document open {:.3} ms exceeds 2000 ms",
                self.source_open_ms
            ));
        }
        if self.active_switch_ms.len() != LAYERS as usize
            || self
                .active_switch_ms
                .iter()
                .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err("all 500 active-layer switches must have valid measurements".into());
        }
        if self.active_switch_max_ms >= 150.0 {
            return Err(format!(
                "worst active-layer switch {:.3} ms exceeds 150 ms",
                self.active_switch_max_ms
            ));
        }
        if self.process_peak_bytes == 0
            || self.process_peak_bytes.saturating_add(self.gpu_peak_bytes) >= PROCESS_CEILING
        {
            return Err("process residency was not measured or exceeded its ceiling".into());
        }
        Ok(())
    }
}

pub fn source_options() -> StoreOptions {
    StoreOptions {
        tile_size: TILE_SIZE,
        format: PixelFormat::Rgba8,
        cache_budget_bytes: TILE_CACHE,
    }
}

pub fn fixture_path(root: &Path) -> PathBuf {
    root.join(format!(
        "corpus-v{CORPUS_VERSION}-{}",
        Config::default().seed
    ))
}

pub fn open_fixture(root: &Path) -> Result<TileStore, String> {
    let path = fixture_path(root);
    let marker = std::fs::read_to_string(path.join(".complete"))
        .map_err(|error| format!("run the storage gate first to prepare the corpus: {error}"))?;
    if marker != Config::default().marker() {
        return Err("render fixture marker does not match the full reference corpus".into());
    }
    let store =
        TileStore::open(path.join("store"), source_options()).map_err(|error| error.to_string())?;
    if store.tile_count() != STORED_TILES as usize {
        return Err("render fixture is missing reference-document tiles".into());
    }
    Ok(store)
}

pub fn reference_layers() -> Vec<LayerDescriptor> {
    (0..LAYERS)
        .map(|id| LayerDescriptor {
            id: u64::from(id),
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            clipping: false,
            group_backdrop_dependent: false,
            revision: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_switch_under_150_ms_and_residency_ceiling_are_hard_assertions() {
        let mut report = RenderMeasurements {
            source_open_ms: 10.0,
            process_peak_bytes: 100 * MIB,
            ..Default::default()
        };
        assert!(report.assert_budgets().is_err());
        for _ in 0..LAYERS {
            report.record_switch(149.99);
        }
        assert!(report.assert_budgets().is_ok());
        report.active_switch_ms[0] = 150.0;
        report.active_switch_max_ms = 150.0;
        assert!(report.assert_budgets().is_err());
        report.active_switch_max_ms = 1.0;
        report.process_peak_bytes = PROCESS_CEILING;
        assert!(report.assert_budgets().is_err());
    }
}
