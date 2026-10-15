//! Document transactions and bounded brush surfaces, independent of desktop windows.
use half::f16;
use serde::{Deserialize, Serialize};
use sideduck_brush::{builtin_library, BrushPreset, EngineConfig, RasterSurface, StrokeEngine};
use sideduck_composite::{color::srgb_to_linear, Pixel};
use sideduck_doc::{
    selection::SelectionMask, Document, DocumentOptions, Layer, Manifest, Operation, StrokeRecord,
    TileAddress, TileEdit, TileFormat,
};
use sideduck_input::{InputSample, SamplePhase};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    rc::Rc,
};

pub type Result<T> = std::result::Result<T, String>;
pub const SURFACE_BUDGET: usize = 192 * 1024 * 1024;
pub const SELECTION_BUDGET: usize = 512 * 1024 * 1024;
pub const OUTPUT_BUDGET: usize = 1024 * 1024 * 1024;

/// Exact committed rasterizer inputs, including textures and linear working color.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedBrush {
    pub version: u32,
    pub preset: BrushPreset,
    pub engine: EngineConfig,
    pub color: [f32; 3],
}
struct ActiveStroke {
    engine: StrokeEngine,
    surface: DocumentSurface,
    record: RecordedBrush,
    samples: Vec<InputSample>,
    seed: u64,
    stroke_id: u64,
}
pub struct EditorCore {
    pub document: Rc<RefCell<Document>>,
    pub brushes: Vec<BrushPreset>,
    pub engine_config: EngineConfig,
    pub brush: usize,
    pub selection: Option<Rc<SelectionMask>>,
    pub revision: u64,
    pub preview_dirty: bool,
    stroke: Option<ActiveStroke>,
}
impl EditorCore {
    pub fn create(path: impl AsRef<Path>, dimensions: [u32; 2]) -> Result<Self> {
        let mut doc = Document::create(path, Manifest::new(dimensions), DocumentOptions::default())
            .map_err(|e| e.to_string())?;
        doc.apply(
            Operation::CreateLayer {
                layer: Layer::raster(0, "Paint 1"),
                index: 0,
            },
            [],
        )
        .map_err(|e| e.to_string())?;
        doc.apply(Operation::SetActiveLayer { id: Some(0) }, [])
            .map_err(|e| e.to_string())?;
        Self::from_document(doc)
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_document(
            Document::open(path, DocumentOptions::default()).map_err(|e| e.to_string())?,
        )
    }
    pub fn from_document(document: Document) -> Result<Self> {
        let (engine_config, brushes) = builtin_library()?;
        let mut core = Self {
            document: Rc::new(RefCell::new(document)),
            brushes,
            engine_config,
            brush: 0,
            selection: None,
            revision: 0,
            preview_dirty: true,
            stroke: None,
        };
        core.reload_selection()?;
        Ok(core)
    }
    pub fn dimensions(&self) -> [u32; 2] {
        self.document.borrow().manifest().canvas
    }
    pub fn active_layer(&self) -> Result<u32> {
        self.document
            .borrow()
            .state()
            .active_layer
            .ok_or_else(|| "Select a raster layer first".into())
    }
    pub fn has_stroke(&self) -> bool {
        self.stroke.is_some()
    }
    pub fn cancel_stroke(&mut self) {
        if let Some(mut stroke) = self.stroke.take() {
            let _ = stroke.engine.cancel(&mut stroke.surface);
            self.changed();
        }
    }
    pub fn apply(&mut self, operation: Operation, edits: Vec<TileEdit>) -> Result<()> {
        self.cancel_stroke();
        self.document
            .borrow_mut()
            .apply(operation, edits)
            .map_err(|e| e.to_string())?;
        self.changed();
        self.reload_selection()
    }
    pub fn undo(&mut self, redo: bool) -> Result<()> {
        self.cancel_stroke();
        {
            let mut doc = self.document.borrow_mut();
            if redo { doc.redo() } else { doc.undo() }.map_err(|e| e.to_string())?;
        }
        self.changed();
        self.reload_selection()
    }
    pub fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.preview_dirty = true;
    }
    pub fn save_selection(&mut self, mask: Option<SelectionMask>) -> Result<()> {
        let value = if let Some(mask) = &mask {
            let source = self
                .document
                .borrow()
                .put_source(mask.pixels())
                .map_err(|e| e.to_string())?;
            Some(serde_json::json!({"version":1,"source":source}))
        } else {
            None
        };
        self.apply(Operation::SetSelection { selection: value }, vec![])
    }
    pub fn reload_selection(&mut self) -> Result<()> {
        let doc = self.document.borrow();
        self.selection = if let Some(value) = &doc.state().selection {
            if value.get("version").and_then(|v| v.as_u64()) != Some(1) {
                return Err("Unsupported selection snapshot version".into());
            }
            let id = serde_json::from_value(
                value
                    .get("source")
                    .ok_or("Selection snapshot has no source")?
                    .clone(),
            )
            .map_err(|e| e.to_string())?;
            let bytes = doc.read_source(id).map_err(|e| e.to_string())?;
            let [w, h] = doc.manifest().canvas;
            Some(Rc::new(SelectionMask::from_pixels(
                w,
                h,
                bytes,
                SELECTION_BUDGET,
            )?))
        } else {
            None
        };
        Ok(())
    }
    pub fn paint(
        &mut self,
        sample: InputSample,
        radius: f32,
        opacity: f32,
        color: [u8; 3],
        eraser: bool,
    ) -> Result<()> {
        if sample.phase == SamplePhase::Hover {
            return Ok(());
        }
        if sample.phase == SamplePhase::Begin {
            self.cancel_stroke();
            let layer = self.active_layer()?;
            {
                let doc = self.document.borrow();
                let info = doc.state().layer(layer).ok_or("Layer disappeared")?;
                if info.locked || !matches!(info.kind, sideduck_doc::LayerKind::Raster) {
                    return Err("Painting requires an unlocked raster layer".into());
                }
            }
            let mut preset = self
                .brushes
                .get(self.brush)
                .ok_or("Brush library is empty")?
                .clone();
            preset.definition.parameters.size.value = radius * 2.0;
            preset.definition.parameters.opacity.value = opacity;
            preset.definition.eraser |= eraser;
            let record = RecordedBrush {
                version: 1,
                preset,
                engine: self.engine_config.clone(),
                color: color.map(|v| srgb_to_linear(v as f32 / 255.0)),
            };
            let stroke_id = self.document.borrow().sequence() + 1;
            let seed = stroke_id.wrapping_mul(0x9e3779b97f4a7c15);
            self.stroke = Some(ActiveStroke {
                engine: StrokeEngine::new(
                    record.preset.clone(),
                    record.engine.clone(),
                    record.color,
                    seed,
                )?,
                surface: DocumentSurface::new(
                    self.document.clone(),
                    layer,
                    SURFACE_BUDGET,
                    self.selection.clone(),
                ),
                record,
                samples: Vec::new(),
                seed,
                stroke_id,
            });
        }
        let Some(stroke) = self.stroke.as_mut() else {
            return Ok(());
        };
        // Bound sample JSON before it can approach the document's 16 MiB record limit.
        if stroke.samples.len() >= 32_768 {
            self.cancel_stroke();
            return Err("Stroke sample budget reached; stroke cancelled".into());
        }
        if let Err(error) = stroke.engine.push_sample(&mut stroke.surface, sample) {
            self.cancel_stroke();
            return Err(error);
        }
        stroke.samples.push(sample);
        self.preview_dirty = true;
        if sample.phase == SamplePhase::End {
            let mut stroke = self.stroke.take().expect("active stroke");
            let layer = stroke.surface.layer;
            self.changed();
            let _summary = stroke.engine.finish();
            let edits = stroke.surface.into_edits()?;
            if !edits.is_empty() {
                let operation = Operation::Stroke(StrokeRecord {
                    version: 1,
                    layer,
                    stroke_id: stroke.stroke_id,
                    seed: stroke.seed,
                    samples: serde_json::to_value(stroke.samples).map_err(|e| e.to_string())?,
                    brush: serde_json::to_value(stroke.record).map_err(|e| e.to_string())?,
                });
                self.document
                    .borrow_mut()
                    .apply(operation, edits)
                    .map_err(|e| e.to_string())?;
                self.changed();
            }
        }
        Ok(())
    }
    pub fn live_tile(&self, layer: u32, x: u32, y: u32) -> Option<Vec<Pixel>> {
        self.stroke
            .as_ref()
            .filter(|s| s.surface.layer == layer)
            .and_then(|s| s.surface.tiles.borrow().get(&[x, y]).cloned())
    }
}

/// Reads sparse document tiles on demand. Writes are private until the stroke commits.
/// Cache admission is checked before allocation; the engine has its independent stroke budget.
pub struct DocumentSurface {
    pub document: Rc<RefCell<Document>>,
    pub layer: u32,
    pub edge: u32,
    dimensions: [u32; 2],
    budget: usize,
    pub tiles: RefCell<BTreeMap<[u32; 2], Vec<Pixel>>>,
    dirty: BTreeSet<[u32; 2]>,
    selection: Option<Rc<SelectionMask>>,
}
impl DocumentSurface {
    pub fn new(
        document: Rc<RefCell<Document>>,
        layer: u32,
        budget: usize,
        selection: Option<Rc<SelectionMask>>,
    ) -> Self {
        let manifest = document.borrow().manifest().clone();
        Self {
            document,
            layer,
            edge: manifest.tile_size,
            dimensions: manifest.canvas,
            budget,
            tiles: RefCell::new(BTreeMap::new()),
            dirty: BTreeSet::new(),
            selection,
        }
    }
    fn load(&self, key: [u32; 2]) -> Result<()> {
        if self.tiles.borrow().contains_key(&key) {
            return Ok(());
        }
        let bytes = (self.edge as usize).pow(2) * 16;
        if (self.tiles.borrow().len() + 1)
            .checked_mul(bytes + 128)
            .is_none_or(|n| n > self.budget)
        {
            return Err("Stroke surface exceeds its memory budget".into());
        }
        let mut doc = self.document.borrow_mut();
        let format = doc.manifest().tile_format;
        let tile = if let Some(bytes) = doc
            .read_tile(TileAddress {
                layer: self.layer,
                x: key[0],
                y: key[1],
            })
            .map_err(|e| e.to_string())?
        {
            decode_tile(bytes, format)?
        } else {
            vec![[0.; 4]; (self.edge as usize).pow(2)]
        };
        self.tiles.borrow_mut().insert(key, tile);
        Ok(())
    }
    pub fn into_edits(self) -> Result<Vec<TileEdit>> {
        let format = self.document.borrow().manifest().tile_format;
        let mut tiles = self.tiles.into_inner();
        Ok(self
            .dirty
            .into_iter()
            .map(|[x, y]| TileEdit {
                address: TileAddress {
                    layer: self.layer,
                    x,
                    y,
                },
                pixels: Some(encode_tile(
                    &tiles.remove(&[x, y]).expect("dirty tile resident"),
                    format,
                )),
            })
            .collect())
    }
}
impl RasterSurface for DocumentSurface {
    fn dimensions(&self) -> [u32; 2] {
        self.dimensions
    }
    fn read_pixel(&self, x: u32, y: u32) -> Result<Pixel> {
        if x >= self.dimensions[0] || y >= self.dimensions[1] {
            return Ok([0.; 4]);
        }
        let key = [x / self.edge, y / self.edge];
        self.load(key)?;
        Ok(self.tiles.borrow()[&key][((y % self.edge) * self.edge + x % self.edge) as usize])
    }
    fn write_pixel(&mut self, x: u32, y: u32, pixel: Pixel) -> Result<()> {
        if x >= self.dimensions[0] || y >= self.dimensions[1] {
            return Ok(());
        }
        let key = [x / self.edge, y / self.edge];
        self.load(key)?;
        let index = ((y % self.edge) * self.edge + x % self.edge) as usize;
        let mut tiles = self.tiles.borrow_mut();
        let target = &mut tiles.get_mut(&key).expect("loaded tile")[index];
        // Engine writes the whole accumulated stroke against its original. Selection must
        // therefore be applied against the immutable document preimage, not the last dab.
        let coverage = self
            .selection
            .as_ref()
            .map_or(1., |s| s.coverage(x, y) as f32 / 255.);
        if coverage == 0. {
            return Ok(());
        }
        if coverage < 1. {
            let mut doc = self.document.borrow_mut();
            let format = doc.manifest().tile_format;
            let original = doc
                .read_tile(TileAddress {
                    layer: self.layer,
                    x: key[0],
                    y: key[1],
                })
                .map_err(|e| e.to_string())?
                .map(|bytes| decode_pixel(bytes, format, index))
                .transpose()?
                .unwrap_or([0.; 4]);
            *target = std::array::from_fn(|c| original[c] + (pixel[c] - original[c]) * coverage);
        } else {
            *target = pixel;
        }
        self.dirty.insert(key);
        Ok(())
    }
}
pub fn decode_pixel(bytes: &[u8], format: TileFormat, index: usize) -> Result<Pixel> {
    match format {
        TileFormat::Rgba8 => {
            let p = bytes
                .get(index * 4..index * 4 + 4)
                .ok_or("Invalid tile length")?;
            Ok(std::array::from_fn(|c| p[c] as f32 / 255.))
        }
        TileFormat::Rgba16Float => {
            let p = bytes
                .get(index * 8..index * 8 + 8)
                .ok_or("Invalid tile length")?;
            Ok(std::array::from_fn(|c| {
                f16::from_le_bytes([p[c * 2], p[c * 2 + 1]]).to_f32()
            }))
        }
    }
}
pub fn decode_tile(bytes: &[u8], format: TileFormat) -> Result<Vec<Pixel>> {
    let stride = if format == TileFormat::Rgba8 { 4 } else { 8 };
    if !bytes.len().is_multiple_of(stride) {
        return Err("Invalid tile length".into());
    }
    (0..bytes.len() / stride)
        .map(|i| decode_pixel(bytes, format, i))
        .collect()
}
pub fn encode_tile(pixels: &[Pixel], format: TileFormat) -> Vec<u8> {
    match format {
        TileFormat::Rgba8 => pixels
            .iter()
            .flat_map(|p| p.map(|v| (v.clamp(0., 1.) * 255.).round() as u8))
            .collect(),
        TileFormat::Rgba16Float => pixels
            .iter()
            .flat_map(|p| p.iter().flat_map(|v| f16::from_f32(*v).to_le_bytes()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(index: u32) -> InputSample {
        InputSample {
            position: [8. + index as f32 * 2., 18.],
            pressure: 0.5 + index as f32 * 0.02,
            timestamp_seconds: index as f64 * 0.01,
            phase: if index == 0 {
                SamplePhase::Begin
            } else if index == 12 {
                SamplePhase::End
            } else {
                SamplePhase::Move
            },
            ..Default::default()
        }
    }
    fn fixture(path: &Path) -> EditorCore {
        let mut core = EditorCore::create(path, [64, 64]).unwrap();
        core.brush = 2;
        core
    }
    #[test]
    fn document_paint_undo_redo_save_reopen_and_logged_brush_replay_are_bit_identical() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("paint.pond");
        let mut core = fixture(&path);
        for i in 0..13 {
            core.paint(sample(i), 5., 0.65, [20, 95, 180], false)
                .unwrap();
        }
        let sequence = core.document.borrow().sequence();
        let Operation::Stroke(stroke) = core.document.borrow().operation(sequence).unwrap() else {
            panic!("stroke operation")
        };
        let record: RecordedBrush = serde_json::from_value(stroke.brush).unwrap();
        let samples: Vec<InputSample> = serde_json::from_value(stroke.samples).unwrap();
        let key = TileAddress {
            layer: 0,
            x: 0,
            y: 0,
        };
        let expected = core
            .document
            .borrow_mut()
            .read_tile(key)
            .unwrap()
            .unwrap()
            .to_vec();
        assert!(expected.iter().any(|&p| p != 0));
        core.undo(false).unwrap();
        assert!(core.document.borrow_mut().read_tile(key).unwrap().is_none());
        core.undo(true).unwrap();
        assert_eq!(
            core.document.borrow_mut().read_tile(key).unwrap().unwrap(),
            expected
        );
        drop(core);
        let core = EditorCore::open(&path).unwrap();
        assert_eq!(
            core.document.borrow_mut().read_tile(key).unwrap().unwrap(),
            expected
        );
        core.document.borrow_mut().checkpoint().unwrap();
        let replay = fixture(&temp.path().join("replay.pond"));
        let mut surface = DocumentSurface::new(replay.document.clone(), 0, SURFACE_BUDGET, None);
        let mut engine =
            StrokeEngine::new(record.preset, record.engine, record.color, stroke.seed).unwrap();
        for sample in samples {
            engine.push_sample(&mut surface, sample).unwrap();
        }
        engine.finish();
        let edits = surface.into_edits().unwrap();
        assert_eq!(edits[0].pixels.as_deref(), Some(expected.as_slice()));
    }
    #[test]
    fn failed_or_cancelled_stroke_never_commits_partial_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let mut core = fixture(&temp.path().join("paint.pond"));
        let before = core.document.borrow().sequence();
        core.paint(sample(0), 5., 1., [0, 0, 0], false).unwrap();
        core.cancel_stroke();
        assert_eq!(core.document.borrow().sequence(), before);
        assert_eq!(core.document.borrow().tile_keys().count(), 0);
        core.engine_config.maximum_stroke_bytes = 128;
        assert!(core.paint(sample(0), 5., 1., [0, 0, 0], false).is_err());
        assert!(!core.has_stroke());
        assert_eq!(core.document.borrow().tile_keys().count(), 0);
        let surface = DocumentSurface::new(core.document.clone(), 0, 1, None);
        assert!(surface.read_pixel(0, 0).is_err());
        assert!(surface.tiles.borrow().is_empty());
    }
    #[test]
    fn selection_snapshot_undo_and_fractional_coverage_apply_once_per_stroke() {
        let temp = tempfile::tempdir().unwrap();
        let mut core = fixture(&temp.path().join("paint.pond"));
        core.save_selection(Some(
            SelectionMask::from_pixels(64, 64, vec![128; 64 * 64], SELECTION_BUDGET).unwrap(),
        ))
        .unwrap();
        assert_eq!(core.selection.as_ref().unwrap().coverage(1, 1), 128);
        core.undo(false).unwrap();
        assert!(core.selection.is_none());
        core.undo(true).unwrap();
        for i in 0..13 {
            core.paint(sample(i), 5., 1., [255, 255, 255], false)
                .unwrap();
        }
        let bytes = core
            .document
            .borrow_mut()
            .read_tile(TileAddress {
                layer: 0,
                x: 0,
                y: 0,
            })
            .unwrap()
            .unwrap()
            .to_vec();
        assert!(bytes.chunks_exact(4).all(|p| p[3] <= 128));
        assert!(bytes.chunks_exact(4).any(|p| p[3] > 60));
    }
}
