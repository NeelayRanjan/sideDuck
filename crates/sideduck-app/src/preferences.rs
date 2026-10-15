//! Versioned local preferences. Full brush data is retained without runtime network access.
use crate::core::Result;
use serde::{Deserialize, Serialize};
use sideduck_brush::{BrushPreset, EngineConfig};
use sideduck_input::{InputConfig, ResponseCurve};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};
const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub version: u32,
    pub input: InputConfig,
    pub brushes: Vec<BrushPreset>,
    pub engine: EngineConfig,
    pub selected_brush: usize,
    pub brush_pressure: BTreeMap<usize, Option<ResponseCurve>>,
    pub calibration: ResponseCurve,
}
impl Preferences {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.brushes.is_empty()
            || self.brushes.len() > 512
            || self.selected_brush >= self.brushes.len()
        {
            return Err("Invalid preferences version or brush library size".into());
        }
        self.input.validate()?;
        self.engine.validate()?;
        self.calibration.validate()?;
        for brush in &self.brushes {
            brush.validate(&self.engine)?;
        }
        for (&index, curve) in &self.brush_pressure {
            if index >= self.brushes.len() {
                return Err("Pressure curve points to a missing brush".into());
            }
            if let Some(curve) = curve {
                curve.validate()?;
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        if fs::metadata(path).map_err(|e| e.to_string())?.len() > MAX_BYTES as u64 {
            return Err("Preferences exceed64MiB".into());
        }
        let prefs: Self = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        prefs.validate()?;
        Ok(Some(prefs))
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path.parent().ok_or("Preferences path needs a parent")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = path.with_extension(format!("{}.next", std::process::id()));
        let result = (|| {
            let file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            let mut bounded = BoundedWriter {
                writer: file,
                remaining: MAX_BYTES,
            };
            serde_json::to_writer(&mut bounded, self).map_err(|e| e.to_string())?;
            bounded.writer.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&temporary, path).map_err(|e| e.to_string())?;
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
struct BoundedWriter<W> {
    writer: W,
    remaining: usize,
}
impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("Preferences exceed64MiB"));
        }
        let written = self.writer.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brush_pressure_and_full_texture_preferences_survive_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let (engine, brushes) = sideduck_brush::builtin_library().unwrap();
        let curve = ResponseCurve {
            points: vec![[0., 0.], [0.5, 0.8], [1., 1.]],
        };
        let prefs = Preferences {
            version: 1,
            input: InputConfig::default(),
            brushes,
            engine,
            selected_brush: 2,
            brush_pressure: BTreeMap::from([(2, Some(curve.clone()))]),
            calibration: ResponseCurve::default(),
        };
        prefs.save(&path).unwrap();
        let loaded = Preferences::load(&path).unwrap().unwrap();
        assert_eq!(loaded.brush_pressure[&2], Some(curve));
        assert_eq!(loaded.brushes, prefs.brushes);
        let mut bounded = BoundedWriter {
            writer: Vec::new(),
            remaining: 2,
        };
        assert!(bounded.write_all(b"three").is_err());
        assert!(bounded.writer.is_empty());
    }
}
