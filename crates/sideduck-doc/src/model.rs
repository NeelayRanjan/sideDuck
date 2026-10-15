//! Versioned document and operation data. Pixels are linear and premultiplied.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sideduck_composite::BlendMode;
use sideduck_tile::{PixelFormat, TileKey};
use std::collections::{BTreeMap, BTreeSet};

pub type LayerId = u32;
pub type ContentId = [u8; 32];
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TileFormat {
    Rgba8,
    Rgba16Float,
}

impl From<TileFormat> for PixelFormat {
    fn from(value: TileFormat) -> Self {
        match value {
            TileFormat::Rgba8 => Self::Rgba8,
            TileFormat::Rgba16Float => Self::Rgba16Float,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub canvas: [u32; 2],
    pub tile_size: u32,
    pub tile_format: TileFormat,
    pub color_space: String,
}

impl Manifest {
    pub fn new(canvas: [u32; 2]) -> Self {
        Self {
            version: FORMAT_VERSION,
            canvas,
            tile_size: 256,
            tile_format: TileFormat::Rgba8,
            color_space: "linear_srgb".into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != FORMAT_VERSION || self.color_space != "linear_srgb" {
            return Err(Error::Invalid(
                "unsupported document version or color space",
            ));
        }
        if self.canvas.contains(&0) || self.tile_size == 0 || self.tile_size > 4096 {
            return Err(Error::Invalid(
                "canvas and tile dimensions must be positive",
            ));
        }
        Ok(())
    }

    pub fn tile_bytes(&self) -> usize {
        self.tile_size as usize
            * self.tile_size as usize
            * PixelFormat::from(self.tile_format).bytes_per_pixel()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerKind {
    Raster,
    Group {
        isolated: bool,
    },
    Reference {
        source: ContentId,
        media_type: String,
        page: u32,
    },
    /// Reserved pass type for a future format version; v1 does not execute it.
    Adjustment {
        parameters: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub kind: LayerKind,
    pub parent: Option<LayerId>,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub mask: Option<LayerId>,
    pub clip_base: Option<LayerId>,
    pub revision: u64,
}

impl Layer {
    pub fn raster(id: LayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            kind: LayerKind::Raster,
            parent: None,
            visible: true,
            locked: false,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            mask: None,
            clip_base: None,
            revision: 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentState {
    /// Sibling order is bottom-to-top; parent references form the group tree.
    pub layers: Vec<Layer>,
    pub active_layer: Option<LayerId>,
    pub next_layer_id: u64,
    /// Versioned operation parameters; raster mask generation belongs to selection.
    pub selection: Option<Value>,
    /// Reserved extension collection. No guide tools are implemented in v1.
    pub guides: Vec<Value>,
}

impl DocumentState {
    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn validate(&self) -> Result<()> {
        let by_id: BTreeMap<_, _> = self.layers.iter().map(|layer| (layer.id, layer)).collect();
        if by_id.len() != self.layers.len() {
            return Err(Error::Invalid("layer identifiers must be unique"));
        }
        if self.active_layer.is_some_and(|id| !by_id.contains_key(&id)) {
            return Err(Error::Invalid("active layer does not exist"));
        }
        for layer in &self.layers {
            if layer.name.len() > 1024
                || !layer.opacity.is_finite()
                || !(0.0..=1.0).contains(&layer.opacity)
            {
                return Err(Error::Invalid("invalid layer name or opacity"));
            }
            if u64::from(layer.id) >= self.next_layer_id {
                return Err(Error::Invalid(
                    "layer allocation counter precedes an existing layer",
                ));
            }
            let mut seen = BTreeSet::from([layer.id]);
            let mut parent = layer.parent;
            while let Some(id) = parent {
                let group = by_id
                    .get(&id)
                    .ok_or(Error::Invalid("group parent does not exist"))?;
                if !matches!(group.kind, LayerKind::Group { .. }) || !seen.insert(id) {
                    return Err(Error::Invalid("group parents must be acyclic groups"));
                }
                parent = group.parent;
            }
            if let Some(mask) = layer.mask {
                if mask == layer.id
                    || !by_id
                        .get(&mask)
                        .is_some_and(|mask| matches!(mask.kind, LayerKind::Raster))
                {
                    return Err(Error::Invalid(
                        "layer mask must reference another raster layer",
                    ));
                }
            }
            if let Some(base) = layer.clip_base {
                let position = self
                    .layers
                    .iter()
                    .position(|item| item.id == layer.id)
                    .expect("present layer");
                let nearest = self.layers[..position]
                    .iter()
                    .rev()
                    .find(|item| item.parent == layer.parent && item.clip_base.is_none());
                if nearest.map(|layer| layer.id) != Some(base) {
                    return Err(Error::Invalid(
                        "clipping base must be the nearest lower unclipped sibling",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn apply(
        &mut self,
        operation: &Operation,
        sequence: u64,
    ) -> Result<BTreeSet<LayerId>> {
        let mut removed = BTreeSet::new();
        match operation {
            Operation::CreateLayer { layer, index } => {
                if *index > self.layers.len()
                    || self.layer(layer.id).is_some()
                    || matches!(layer.kind, LayerKind::Adjustment { .. })
                {
                    return Err(Error::Invalid("invalid new layer or position"));
                }
                let mut layer = layer.clone();
                layer.revision = sequence;
                self.next_layer_id = self.next_layer_id.max(u64::from(layer.id) + 1);
                self.active_layer = Some(layer.id);
                self.layers.insert(*index, layer);
            }
            Operation::SetLayer { layer } => {
                let old = self
                    .layers
                    .iter_mut()
                    .find(|old| old.id == layer.id)
                    .ok_or(Error::Invalid("layer does not exist"))?;
                if std::mem::discriminant(&old.kind) != std::mem::discriminant(&layer.kind) {
                    return Err(Error::Invalid(
                        "layer kind cannot change through property editing",
                    ));
                }
                *old = layer.clone();
                old.revision = sequence;
            }
            Operation::RemoveLayer { id } => {
                if self.layer(*id).is_none() {
                    return Err(Error::Invalid("layer does not exist"));
                }
                removed.insert(*id);
                loop {
                    let previous = removed.len();
                    for layer in &self.layers {
                        if layer.parent.is_some_and(|parent| removed.contains(&parent)) {
                            removed.insert(layer.id);
                        }
                    }
                    if previous == removed.len() {
                        break;
                    }
                }
                self.layers.retain(|layer| !removed.contains(&layer.id));
                for layer in &mut self.layers {
                    if layer.mask.is_some_and(|id| removed.contains(&id)) {
                        layer.mask = None;
                        layer.revision = sequence;
                    }
                    if layer.clip_base.is_some_and(|id| removed.contains(&id)) {
                        layer.clip_base = None;
                        layer.revision = sequence;
                    }
                }
                if self.active_layer.is_some_and(|id| removed.contains(&id)) {
                    self.active_layer = self.layers.last().map(|layer| layer.id);
                }
            }
            Operation::ReorderLayer { id, index, parent } => {
                let old = self
                    .layers
                    .iter()
                    .position(|layer| layer.id == *id)
                    .ok_or(Error::Invalid("layer does not exist"))?;
                if *index >= self.layers.len() {
                    return Err(Error::Invalid("layer position is outside stack"));
                }
                let mut layer = self.layers.remove(old);
                layer.parent = *parent;
                layer.revision = sequence;
                self.layers.insert(*index, layer);
            }
            Operation::SetActiveLayer { id } => self.active_layer = *id,
            Operation::SetSelection { selection } => self.selection = selection.clone(),
            Operation::Stroke(stroke) => {
                if stroke.version != 1 {
                    return Err(Error::Invalid("unsupported stroke record version"));
                }
                self.ensure_editable(stroke.layer)?;
            }
            Operation::RasterEdit { layers, label } => {
                if layers.is_empty() || label.len() > 1024 {
                    return Err(Error::Invalid(
                        "raster edit requires layers and a bounded label",
                    ));
                }
                for &layer in layers {
                    self.ensure_editable(layer)?;
                }
            }
            Operation::Transform { layer, matrix } => {
                self.ensure_editable(*layer)?;
                if matrix.iter().any(|value| !value.is_finite()) {
                    return Err(Error::Invalid("transform matrix must be finite"));
                }
            }
            Operation::Merge { sources, target } => {
                if sources.is_empty() || !sources.contains(target) {
                    return Err(Error::Invalid("merge target must belong to its sources"));
                }
                for &id in sources {
                    self.ensure_editable(id)?;
                    if id != *target {
                        removed.insert(id);
                    }
                }
                self.layers.retain(|layer| !removed.contains(&layer.id));
                for layer in &mut self.layers {
                    if layer.mask.is_some_and(|id| removed.contains(&id)) {
                        layer.mask = None;
                    }
                    if layer.clip_base.is_some_and(|id| removed.contains(&id)) {
                        layer.clip_base = None;
                    }
                }
                self.active_layer = Some(*target);
            }
            Operation::Initialize | Operation::Undo { .. } | Operation::Redo { .. } => {
                return Err(Error::Invalid(
                    "internal journal operations cannot be applied directly",
                ));
            }
        }
        self.validate()?;
        Ok(removed)
    }

    pub(crate) fn ensure_editable(&self, id: LayerId) -> Result<()> {
        let layer = self
            .layer(id)
            .ok_or(Error::Invalid("layer does not exist"))?;
        if layer.locked || !matches!(layer.kind, LayerKind::Raster) {
            return Err(Error::Invalid("layer is locked or is not raster"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeRecord {
    pub version: u32,
    pub layer: LayerId,
    pub stroke_id: u64,
    pub seed: u64,
    pub samples: Value,
    pub brush: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "data", rename_all = "snake_case")]
pub enum Operation {
    Initialize,
    CreateLayer {
        layer: Layer,
        index: usize,
    },
    SetLayer {
        layer: Layer,
    },
    RemoveLayer {
        id: LayerId,
    },
    ReorderLayer {
        id: LayerId,
        index: usize,
        parent: Option<LayerId>,
    },
    SetActiveLayer {
        id: Option<LayerId>,
    },
    SetSelection {
        selection: Option<Value>,
    },
    Stroke(StrokeRecord),
    RasterEdit {
        layers: Vec<LayerId>,
        label: String,
    },
    Transform {
        layer: LayerId,
        matrix: [f64; 9],
    },
    /// The rasterizer supplies the composited target tiles in the same transaction.
    Merge {
        sources: Vec<LayerId>,
        target: LayerId,
    },
    Undo {
        target: u64,
    },
    Redo {
        target: u64,
    },
}

impl Operation {
    pub(crate) fn permits_tile(&self, layer: LayerId) -> bool {
        match self {
            Self::CreateLayer { layer: created, .. } => created.id == layer,
            Self::Stroke(stroke) => stroke.layer == layer,
            Self::RasterEdit { layers, .. } => layers.contains(&layer),
            Self::Transform { layer: target, .. } | Self::Merge { target, .. } => *target == layer,
            _ => false,
        }
    }

    pub(crate) fn expensive(&self) -> bool {
        matches!(
            self,
            Self::Transform { .. } | Self::Merge { .. } | Self::RemoveLayer { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileAddress {
    pub layer: LayerId,
    pub x: u32,
    pub y: u32,
}
impl From<TileAddress> for TileKey {
    fn from(value: TileAddress) -> Self {
        Self {
            layer: value.layer,
            x: value.x,
            y: value.y,
        }
    }
}
impl From<TileKey> for TileAddress {
    fn from(value: TileKey) -> Self {
        Self {
            layer: value.layer,
            x: value.x,
            y: value.y,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TileEdit {
    pub address: TileAddress,
    pub pixels: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TileChange {
    pub address: TileAddress,
    pub before: Option<ContentId>,
    pub after: Option<ContentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryEntry {
    pub sequence: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct History {
    pub undo: Vec<HistoryEntry>,
    pub redo: Vec<HistoryEntry>,
}

impl History {
    pub fn trim(&mut self, ceiling: u64) {
        let mut total = self
            .undo
            .iter()
            .chain(&self.redo)
            .fold(0_u64, |sum, item| sum.saturating_add(item.bytes));
        while total > ceiling {
            let item = if !self.undo.is_empty() {
                self.undo.remove(0)
            } else if !self.redo.is_empty() {
                self.redo.remove(0)
            } else {
                break;
            };
            total = total.saturating_sub(item.bytes);
        }
    }
}
