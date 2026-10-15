//! Bounded CPU preview adapter. Source tiles stay sparse; zoomed views use mip tiles.
//! This fallback is intentionally separate from the GPU performance gate.
use crate::core::{decode_tile, EditorCore, Result, OUTPUT_BUDGET};
use sideduck_composite::{
    blend,
    pyramid::{MipKey, MipPyramid},
    BlendMode, Pixel,
};
use sideduck_doc::{ContentId, Layer, LayerKind, TileAddress};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

type ReferenceKey = (ContentId, u32, [u32; 2]);
struct ReferenceEntry {
    pixels: Rc<Vec<Pixel>>,
    touched: u64,
}
/// Immutable source hashes make these entries valid across strokes and undo.
struct ReferenceCache {
    entries: BTreeMap<ReferenceKey, ReferenceEntry>,
    bytes: usize,
    budget: usize,
    clock: u64,
    renders: u64,
}
impl ReferenceCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            bytes: 0,
            budget,
            clock: 0,
            renders: 0,
        }
    }
    fn raster(&mut self, core: &EditorCore, key: ReferenceKey) -> Result<Rc<Vec<Pixel>>> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.touched = self.clock;
            return Ok(Rc::clone(&entry.pixels));
        }
        let required = (key.2[0] as usize)
            .checked_mul(key.2[1] as usize)
            .and_then(|n| n.checked_mul(16))
            .ok_or("Reference cache size overflow")?;
        while self.bytes.saturating_add(required) > self.budget && !self.entries.is_empty() {
            let oldest = *self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .unwrap()
                .0;
            let entry = self.entries.remove(&oldest).unwrap();
            self.bytes -= entry.pixels.len() * 16;
        }
        let bytes = core
            .document
            .borrow()
            .read_source(key.0)
            .map_err(|e| e.to_string())?;
        let pdf = sideduck_platform::create_pdf_rasterizer();
        let reference = crate::reference::ReferenceSource::from_bytes(bytes, key.1, pdf.as_ref())?;
        let raster = reference.render(key.2, OUTPUT_BUDGET - self.bytes, pdf.as_ref())?;
        if raster.dimensions != key.2 {
            return Err("Reference output dimensions differ".into());
        }
        self.renders += 1;
        let pixels = Rc::new(raster.pixels);
        if required <= self.budget {
            self.bytes += required;
            self.entries.insert(
                key,
                ReferenceEntry {
                    pixels: Rc::clone(&pixels),
                    touched: self.clock,
                },
            );
        }
        Ok(pixels)
    }
}

pub struct PreviewRenderer {
    pyramid: MipPyramid,
    canvas: [u32; 2],
    revision: u64,
    tile_size: u32,
    references: ReferenceCache,
}
impl PreviewRenderer {
    pub fn new(core: &EditorCore) -> Result<Self> {
        let doc = core.document.borrow();
        let canvas = doc.manifest().canvas;
        let tile_size = doc.manifest().tile_size;
        Ok(Self {
            pyramid: MipPyramid::new(canvas, tile_size, 128 * 1024 * 1024)
                .map_err(|e| e.to_string())?,
            canvas,
            revision: u64::MAX,
            tile_size,
            references: ReferenceCache::new(128 * 1024 * 1024),
        })
    }
    pub fn level_for_preview(&self, maximum_side: u32) -> u8 {
        let mut level = 0;
        while self.canvas[0].max(self.canvas[1]).div_ceil(1 << level) > maximum_side {
            level += 1;
        }
        level
    }
    pub fn render(
        &mut self,
        core: &EditorCore,
        level: u8,
        exclude_references: bool,
    ) -> Result<([u32; 2], Vec<Pixel>)> {
        if self.canvas != core.dimensions()
            || self.tile_size != core.document.borrow().manifest().tile_size
        {
            *self = Self::new(core)?;
        }
        // In-flight tiles change without a committed revision. Clearing a bounded cache is
        // conservative; the production GPU adapter has finer per-tile invalidation.
        if self.revision != core.revision || core.has_stroke() {
            self.pyramid = MipPyramid::new(self.canvas, self.tile_size, 128 * 1024 * 1024)
                .map_err(|e| e.to_string())?;
            self.revision = core.revision;
        }
        let dimensions = self.pyramid.dimensions(level).map_err(|e| e.to_string())?;
        let count = dimensions[0] as usize * dimensions[1] as usize;
        if count.checked_mul(16).is_none_or(|n| n > OUTPUT_BUDGET) {
            return Err("Composite output exceeds its memory budget".into());
        }
        let layers = core.document.borrow().state().layers.clone();
        let occupied: BTreeSet<_> = core
            .document
            .borrow()
            .tile_keys()
            .map(|key| key.layer)
            .collect();
        let mut output = vec![[0.; 4]; count];
        for ty in 0..dimensions[1].div_ceil(self.tile_size) {
            for tx in 0..dimensions[0].div_ceil(self.tile_size) {
                let mut renderer = TileRenderer {
                    core,
                    pyramid: &mut self.pyramid,
                    references: &mut self.references,
                    layers: &layers,
                    occupied: &occupied,
                    edge: self.tile_size,
                    key: MipKey {
                        layer: 0,
                        level,
                        x: tx,
                        y: ty,
                    },
                    exclude_references,
                    depth: 0,
                };
                let tile = renderer.children(None)?;
                for y in 0..self.tile_size.min(dimensions[1] - ty * self.tile_size) {
                    let width = self.tile_size.min(dimensions[0] - tx * self.tile_size) as usize;
                    let source = y as usize * self.tile_size as usize;
                    let destination =
                        ((ty * self.tile_size + y) * dimensions[0] + tx * self.tile_size) as usize;
                    output[destination..destination + width]
                        .copy_from_slice(&tile[source..source + width]);
                }
            }
        }
        Ok((dimensions, output))
    }
}
struct TileRenderer<'a> {
    core: &'a EditorCore,
    pyramid: &'a mut MipPyramid,
    references: &'a mut ReferenceCache,
    layers: &'a [Layer],
    occupied: &'a BTreeSet<u32>,
    edge: u32,
    key: MipKey,
    exclude_references: bool,
    depth: usize,
}
impl TileRenderer<'_> {
    fn empty(&self) -> Vec<Pixel> {
        vec![[0.; 4]; (self.edge as usize).pow(2)]
    }
    fn children(&mut self, parent: Option<u32>) -> Result<Vec<Pixel>> {
        self.children_over(parent, self.empty())
    }
    fn children_over(&mut self, parent: Option<u32>, mut out: Vec<Pixel>) -> Result<Vec<Pixel>> {
        self.depth += 1;
        if self
            .depth
            .checked_mul((self.edge as usize).pow(2) * 16 * 4)
            .is_none_or(|n| n > 128 * 1024 * 1024)
        {
            return Err("Layer group nesting exceeds composite scratch budget".into());
        }
        let siblings: Vec<_> = self
            .layers
            .iter()
            .filter(|l| {
                l.parent == parent && !self.layers.iter().any(|owner| owner.mask == Some(l.id))
            })
            .cloned()
            .collect();
        let mut index = 0;
        while index < siblings.len() {
            let layer = &siblings[index];
            if layer.clip_base.is_some() {
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while end < siblings.len() && siblings[end].clip_base == Some(layer.id) {
                end += 1;
            }
            if layer.visible {
                // Pass-through children see the actual parent backdrop. Group opacity
                // and mask interpolate the complete result against its original backdrop.
                // A clipping run is an atomic isolated source, as in the GPU run compiler.
                if matches!(layer.kind, LayerKind::Group { isolated: false })
                    && layer.blend_mode == BlendMode::Normal
                    && end == index + 1
                {
                    let original = out.clone();
                    out = self.children_over(Some(layer.id), out)?;
                    let mask = layer.mask.map(|id| self.source(id)).transpose()?;
                    for (i, (result, before)) in out.iter_mut().zip(original).enumerate() {
                        let amount = layer.opacity * mask.as_ref().map_or(1., |m| m[i][3]);
                        for channel in 0..4 {
                            result[channel] =
                                before[channel] + amount * (result[channel] - before[channel]);
                        }
                    }
                    index = end;
                    continue;
                }
                let mut source = self.layer_pixels(layer)?;
                for clip in &siblings[index + 1..end] {
                    if clip.visible {
                        let top = self.layer_pixels(clip)?;
                        for (base, top) in source.iter_mut().zip(top) {
                            // Source-atop internal clipping preserves base coverage.
                            let coverage = base[3];
                            if coverage > 0. {
                                let opaque = [
                                    base[0] / coverage,
                                    base[1] / coverage,
                                    base[2] / coverage,
                                    1.,
                                ];
                                let mixed = blend(opaque, top, clip.blend_mode, clip.opacity);
                                *base = [
                                    mixed[0] * coverage,
                                    mixed[1] * coverage,
                                    mixed[2] * coverage,
                                    coverage,
                                ];
                            }
                        }
                    }
                }
                for (dst, src) in out.iter_mut().zip(source) {
                    *dst = blend(*dst, src, layer.blend_mode, layer.opacity);
                }
            }
            index = end;
        }
        self.depth -= 1;
        Ok(out)
    }
    fn layer_pixels(&mut self, layer: &Layer) -> Result<Vec<Pixel>> {
        let mut pixels = match layer.kind {
            LayerKind::Group { .. } => self.children(Some(layer.id))?,
            LayerKind::Reference { .. } if self.exclude_references => self.empty(),
            LayerKind::Reference { source, page, .. } => {
                let dimensions = self
                    .pyramid
                    .dimensions(self.key.level)
                    .map_err(|e| e.to_string())?;
                let pixels = self
                    .references
                    .raster(self.core, (source, page, dimensions))?;
                let mut tile = self.empty();
                for y in 0..self.edge {
                    for x in 0..self.edge {
                        let sx = self.key.x * self.edge + x;
                        let sy = self.key.y * self.edge + y;
                        if sx < dimensions[0] && sy < dimensions[1] {
                            tile[(y * self.edge + x) as usize] =
                                pixels[(sy * dimensions[0] + sx) as usize];
                        }
                    }
                }
                tile
            }
            _ => self.source(layer.id)?,
        };
        if let Some(mask) = layer.mask {
            let mask = self.source(mask)?;
            for (pixel, mask) in pixels.iter_mut().zip(mask) {
                for c in pixel {
                    *c *= mask[3];
                }
            }
        }
        Ok(pixels)
    }
    fn source(&mut self, layer: u32) -> Result<Vec<Pixel>> {
        if !(self.occupied.contains(&layer)
            || self.core.has_stroke() && self.core.active_layer().ok() == Some(layer))
        {
            return Ok(self.empty());
        }
        let core = self.core;
        let key = MipKey {
            layer: u64::from(layer),
            ..self.key
        };
        self.pyramid
            .tile(key, &mut |key| {
                if key.level != 0 {
                    return Ok(None);
                }
                if let Some(tile) = core.live_tile(key.layer as u32, key.x, key.y) {
                    return Ok(Some(tile));
                }
                let mut doc = core.document.borrow_mut();
                let format = doc.manifest().tile_format;
                doc.read_tile(TileAddress {
                    layer: key.layer as u32,
                    x: key.x,
                    y: key.y,
                })
                .map_err(|e| e.to_string())?
                .map(|bytes| decode_tile(bytes, format))
                .transpose()
            })
            .map_err(|e| e.to_string())
            .map(|tile| tile.unwrap_or_else(|| self.empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sideduck_doc::{Document, DocumentOptions, Manifest, Operation, TileEdit};
    fn core(path: &std::path::Path) -> EditorCore {
        let mut manifest = Manifest::new([4, 4]);
        manifest.tile_size = 2;
        EditorCore::from_document(
            Document::create(path, manifest, DocumentOptions::default()).unwrap(),
        )
        .unwrap()
    }
    fn add(core: &mut EditorCore, layer: Layer, color: Option<[u8; 4]>) {
        let index = core.document.borrow().state().layers.len();
        let edits = color
            .map(|p| TileEdit {
                address: TileAddress {
                    layer: layer.id,
                    x: 0,
                    y: 0,
                },
                pixels: Some(p.repeat(4)),
            })
            .into_iter()
            .collect();
        core.apply(Operation::CreateLayer { layer, index }, edits)
            .unwrap();
    }
    #[test]
    fn pass_through_group_blends_against_backdrop_and_interpolates_masked_opacity() {
        let temp = tempfile::tempdir().unwrap();
        let mut core = core(temp.path());
        add(
            &mut core,
            Layer::raster(0, "Background"),
            Some([0, 0, 255, 255]),
        );
        let mut group = Layer::raster(1, "Group");
        group.kind = LayerKind::Group { isolated: false };
        add(&mut core, group.clone(), None);
        let mut child = Layer::raster(2, "Multiply");
        child.parent = Some(1);
        child.blend_mode = BlendMode::Multiply;
        add(&mut core, child, Some([255, 0, 0, 255]));
        let mut renderer = PreviewRenderer::new(&core).unwrap();
        assert_eq!(
            renderer.render(&core, 0, false).unwrap().1[0],
            [0., 0., 0., 1.]
        );
        let mut mask = Layer::raster(3, "Mask");
        mask.visible = false;
        add(&mut core, mask, Some([0, 0, 0, 128]));
        group.mask = Some(3);
        group.opacity = 0.5;
        core.apply(
            Operation::SetLayer {
                layer: group.clone(),
            },
            vec![],
        )
        .unwrap();
        let pixel = renderer.render(&core, 0, false).unwrap().1[0];
        assert!((pixel[2] - (1. - 0.5 * 128. / 255.)).abs() < 1e-6);
        assert_eq!([pixel[0], pixel[1], pixel[3]], [0., 0., 1.]);
        group.kind = LayerKind::Group { isolated: true };
        group.mask = None;
        group.opacity = 1.;
        core.apply(Operation::SetLayer { layer: group }, vec![])
            .unwrap();
        assert_eq!(
            renderer.render(&core, 0, false).unwrap().1[0],
            [1., 0., 0., 1.]
        );
    }
    #[test]
    fn reference_cache_reuses_source_across_tiles_and_revisions_but_tracks_zoom() {
        let temp = tempfile::tempdir().unwrap();
        let mut core = core(temp.path());
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([255, 0, 0, 255]),
        ))
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
        let source = core
            .document
            .borrow()
            .put_source(&encoded.into_inner())
            .unwrap();
        let mut layer = Layer::raster(0, "Reference");
        layer.kind = LayerKind::Reference {
            source,
            media_type: "image/png".into(),
            page: 0,
        };
        add(&mut core, layer, None);
        let mut renderer = PreviewRenderer::new(&core).unwrap();
        let (_, pixels) = renderer.render(&core, 0, false).unwrap();
        assert!(pixels.iter().all(|p| *p == [1., 0., 0., 1.]));
        assert_eq!(renderer.references.renders, 1);
        core.changed();
        renderer.render(&core, 0, false).unwrap();
        assert_eq!(renderer.references.renders, 1);
        renderer.render(&core, 1, false).unwrap();
        assert_eq!(renderer.references.renders, 2);
        assert!(renderer
            .render(&core, 0, true)
            .unwrap()
            .1
            .iter()
            .all(|p| *p == [0.; 4]));
        renderer.references.budget = 64;
        renderer.render(&core, 2, false).unwrap();
        assert!(renderer.references.bytes <= 64);
    }
}
