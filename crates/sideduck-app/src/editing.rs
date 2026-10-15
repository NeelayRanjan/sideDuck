//! Transactional selection editing with bounded source tiles and output buffers.
use crate::core::{decode_pixel, decode_tile, encode_tile, EditorCore, Result, OUTPUT_BUDGET};
use sideduck_composite::{blend, BlendMode, Pixel};
use sideduck_doc::transform::{render_tiles, Sampling, Transform};
use sideduck_doc::{Layer, LayerKind, Operation, TileAddress, TileEdit, TileFormat};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub struct Clipboard {
    edge: u32,
    format: TileFormat,
    tiles: Vec<(TileAddress, Vec<u8>)>,
}
fn admit(current: &mut usize, additional: usize) -> Result<()> {
    *current = current
        .checked_add(additional)
        .filter(|n| *n <= OUTPUT_BUDGET)
        .ok_or("Editing output exceeds its memory budget")?;
    Ok(())
}
pub fn copy_selection(core: &mut EditorCore, cut: bool) -> Result<Clipboard> {
    core.cancel_stroke();
    let id = core.active_layer()?;
    let (edge, format, keys) = {
        let doc = core.document.borrow();
        let layer = doc.state().layer(id).ok_or("Layer disappeared")?;
        if !matches!(layer.kind, LayerKind::Raster) || cut && layer.locked {
            return Err("Copy requires raster pixels; cut also requires an unlocked layer".into());
        }
        (
            doc.manifest().tile_size,
            doc.manifest().tile_format,
            doc.tile_keys()
                .filter(|key| key.layer == id)
                .collect::<Vec<_>>(),
        )
    };
    let mut tiles = Vec::new();
    let mut edits = Vec::new();
    let mut bytes = 0;
    for address in keys {
        let pixels = {
            let mut doc = core.document.borrow_mut();
            decode_tile(
                doc.read_tile(address)
                    .map_err(|e| e.to_string())?
                    .ok_or("Tile disappeared")?,
                format,
            )?
        };
        let mut copied = pixels.clone();
        let mut remaining = cut.then_some(pixels);
        for (index, pixel) in copied.iter_mut().enumerate() {
            let x = address.x * edge + index as u32 % edge;
            let y = address.y * edge + index as u32 / edge;
            let amount = core
                .selection
                .as_ref()
                .map_or(1.0, |mask| mask.coverage(x, y) as f32 / 255.0);
            pixel.iter_mut().for_each(|v| *v *= amount);
            if let Some(remaining) = &mut remaining {
                remaining[index].iter_mut().for_each(|v| *v *= 1.0 - amount);
            }
        }
        if copied.iter().any(|p| p[3] > 0.0) {
            let encoded = encode_tile(&copied, format);
            admit(&mut bytes, encoded.len() + 128)?;
            tiles.push((address, encoded));
        }
        if let Some(remaining) = remaining {
            let encoded = encode_tile(&remaining, format);
            admit(&mut bytes, encoded.len() + 128)?;
            edits.push(TileEdit {
                address,
                pixels: Some(encoded),
            });
        }
    }
    if cut {
        core.apply(
            Operation::RasterEdit {
                layers: vec![id],
                label: "Cut selection".into(),
            },
            edits,
        )?;
    }
    Ok(Clipboard {
        edge,
        format,
        tiles,
    })
}
pub fn paste(core: &mut EditorCore, clipboard: &Clipboard) -> Result<()> {
    let (id, index, edge, format, dimensions) = {
        let doc = core.document.borrow();
        (
            u32::try_from(doc.state().next_layer_id).map_err(|_| "Layer identifiers exhausted")?,
            doc.state().layers.len(),
            doc.manifest().tile_size,
            doc.manifest().tile_format,
            doc.manifest().canvas,
        )
    };
    if clipboard.edge != edge {
        return Err("Pasting between different tile sizes requires resampling".into());
    }
    let mut edits = Vec::new();
    let mut bytes = 0;
    for (source, pixels) in &clipboard.tiles {
        if source.x * edge >= dimensions[0] || source.y * edge >= dimensions[1] {
            continue;
        }
        let mut decoded = decode_tile(pixels, clipboard.format)?;
        for (index, pixel) in decoded.iter_mut().enumerate() {
            if source.x * edge + index as u32 % edge >= dimensions[0]
                || source.y * edge + index as u32 / edge >= dimensions[1]
            {
                *pixel = [0.0; 4];
            }
        }
        let pixels = encode_tile(&decoded, format);
        admit(&mut bytes, pixels.len() + 128)?;
        edits.push(TileEdit {
            address: TileAddress {
                layer: id,
                x: source.x,
                y: source.y,
            },
            pixels: Some(pixels),
        });
    }
    core.apply(
        Operation::CreateLayer {
            layer: Layer::raster(id, "Pasted selection"),
            index,
        },
        edits,
    )
}
struct Sampler {
    document: Rc<RefCell<sideduck_doc::Document>>,
    layer: u32,
    dimensions: [u32; 2],
    edge: u32,
    format: TileFormat,
    cache: BTreeMap<[u32; 2], (Vec<u8>, u64)>,
    clock: u64,
    bytes: usize,
}
impl Sampler {
    fn new(core: &EditorCore, layer: u32) -> Self {
        let doc = core.document.borrow();
        Self {
            document: core.document.clone(),
            layer,
            dimensions: doc.manifest().canvas,
            edge: doc.manifest().tile_size,
            format: doc.manifest().tile_format,
            cache: BTreeMap::new(),
            clock: 0,
            bytes: 0,
        }
    }
    fn pixel(&mut self, x: i64, y: i64) -> Result<Pixel> {
        if x < 0 || y < 0 || x >= self.dimensions[0] as i64 || y >= self.dimensions[1] as i64 {
            return Ok([0.0; 4]);
        }
        let key = [x as u32 / self.edge, y as u32 / self.edge];
        self.clock += 1;
        if !self.cache.contains_key(&key) {
            let tile = {
                let mut doc = self.document.borrow_mut();
                doc.read_tile(TileAddress {
                    layer: self.layer,
                    x: key[0],
                    y: key[1],
                })
                .map_err(|e| e.to_string())?
                .map(<[u8]>::to_vec)
            };
            let Some(tile) = tile else {
                return Ok([0.0; 4]);
            };
            while self.bytes + tile.len() > 32 * 1024 * 1024 {
                let Some(oldest) = self
                    .cache
                    .iter()
                    .min_by_key(|(_, (_, t))| *t)
                    .map(|(key, _)| *key)
                else {
                    return Err("Source tile exceeds editing cache".into());
                };
                self.bytes -= self
                    .cache
                    .remove(&oldest)
                    .expect("present cache entry")
                    .0
                    .len();
            }
            self.bytes += tile.len();
            self.cache.insert(key, (tile, self.clock));
        }
        let (pixels, touched) = self.cache.get_mut(&key).expect("loaded tile");
        *touched = self.clock;
        decode_pixel(
            pixels,
            self.format,
            ((y as u32 % self.edge) * self.edge + x as u32 % self.edge) as usize,
        )
    }
}
pub fn transform_selection(core: &mut EditorCore, transform: Transform) -> Result<()> {
    core.cancel_stroke();
    let layer = core.active_layer()?;
    {
        let doc = core.document.borrow();
        let info = doc.state().layer(layer).ok_or("Layer disappeared")?;
        if info.locked || !matches!(info.kind, LayerKind::Raster) {
            return Err("Transform requires an unlocked raster layer".into());
        }
    }
    let (edge, format, canvas) = {
        let doc = core.document.borrow();
        (
            doc.manifest().tile_size,
            doc.manifest().tile_format,
            doc.manifest().canvas,
        )
    };
    let selection = core.selection.clone();
    let sampler = RefCell::new(Sampler::new(core, layer));
    let error = RefCell::new(None);
    let mut edits = Vec::new();
    let mut bytes = 0;
    render_tiles(
        transform,
        canvas,
        edge,
        Sampling::Bilinear,
        (edge as usize).pow(2) * 16,
        |x, y| match sampler.borrow_mut().pixel(x, y) {
            Ok(mut pixel) => {
                let amount = selection.as_ref().map_or(1.0, |mask| {
                    if x < 0 || y < 0 {
                        0.0
                    } else {
                        mask.coverage(x as u32, y as u32) as f32 / 255.0
                    }
                });
                pixel.iter_mut().for_each(|v| *v *= amount);
                pixel
            }
            Err(cause) => {
                *error.borrow_mut() = Some(cause);
                [0.0; 4]
            }
        },
        |region, moved| {
            if let Some(error) = error.borrow_mut().take() {
                return Err(error);
            }
            let mut output = vec![[0.0; 4]; (edge as usize).pow(2)];
            for y in 0..region.height {
                for x in 0..region.width {
                    let px = region.x + x;
                    let py = region.y + y;
                    let mut original = sampler.borrow_mut().pixel(px as i64, py as i64)?;
                    let removed = selection
                        .as_ref()
                        .map_or(1.0, |mask| mask.coverage(px, py) as f32 / 255.0);
                    original.iter_mut().for_each(|v| *v *= 1.0 - removed);
                    output[(y * edge + x) as usize] = blend(
                        original,
                        moved[(y * region.width + x) as usize],
                        BlendMode::Normal,
                        1.0,
                    );
                }
            }
            let pixels = encode_tile(&output, format);
            admit(&mut bytes, pixels.len() + 128)?;
            edits.push(TileEdit {
                address: TileAddress {
                    layer,
                    x: region.x / edge,
                    y: region.y / edge,
                },
                pixels: Some(pixels),
            });
            Ok(())
        },
    )?;
    let matrix = transform
        .matrix()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "Invalid transform matrix")?;
    core.apply(Operation::Transform { layer, matrix }, edits)
}
pub fn merge_down(core: &mut EditorCore, id: u32) -> Result<()> {
    core.cancel_stroke();
    let (bottom, top, edge, format, keys) = {
        let doc = core.document.borrow();
        let index = doc
            .state()
            .layers
            .iter()
            .position(|l| l.id == id)
            .ok_or("Layer disappeared")?;
        if index == 0 {
            return Err("No lower layer to merge into".into());
        }
        let bottom = doc.state().layers[index - 1].clone();
        let top = doc.state().layers[index].clone();
        if [&bottom, &top].iter().any(|layer| {
            layer.locked
                || !matches!(layer.kind, LayerKind::Raster)
                || layer.blend_mode != BlendMode::Normal
                || layer.mask.is_some()
                || layer.clip_base.is_some()
        }) || bottom.parent != top.parent
        {
            return Err("Merge currently requires two normal unmasked raster siblings".into());
        }
        let keys = doc
            .tile_keys()
            .filter(|k| k.layer == bottom.id || k.layer == top.id)
            .map(|k| [k.x, k.y])
            .collect::<std::collections::BTreeSet<_>>();
        (
            bottom,
            top,
            doc.manifest().tile_size,
            doc.manifest().tile_format,
            keys,
        )
    };
    // Baking target opacity into its pixels requires normalizing its properties
    // in the same journal transaction; Merge currently preserves target props.
    if bottom.opacity != 1.0 || !bottom.visible {
        return Err("Set the lower layer to visible at full opacity before merging".into());
    }
    let mut edits = Vec::new();
    let mut bytes = 0;
    for [x, y] in keys {
        let read = |layer| -> Result<Vec<Pixel>> {
            let mut doc = core.document.borrow_mut();
            doc.read_tile(TileAddress { layer, x, y })
                .map_err(|e| e.to_string())?
                .map(|b| decode_tile(b, format))
                .transpose()
                .map(|p| p.unwrap_or_else(|| vec![[0.0; 4]; (edge as usize).pow(2)]))
        };
        let lower = read(bottom.id)?;
        let upper = read(top.id)?;
        let output: Vec<_> = lower
            .into_iter()
            .zip(upper)
            .map(|(a, b)| {
                blend(
                    a,
                    b,
                    BlendMode::Normal,
                    if top.visible { top.opacity } else { 0.0 },
                )
            })
            .collect();
        let pixels = encode_tile(&output, format);
        admit(&mut bytes, pixels.len() + 128)?;
        edits.push(TileEdit {
            address: TileAddress {
                layer: bottom.id,
                x,
                y,
            },
            pixels: Some(pixels),
        });
    }
    core.apply(
        Operation::Merge {
            sources: vec![bottom.id, top.id],
            target: bottom.id,
        },
        edits,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> (tempfile::TempDir, EditorCore) {
        let dir = tempfile::tempdir().unwrap();
        let mut core = EditorCore::create(dir.path().join("test.pond"), [8, 8]).unwrap();
        let edge = core.document.borrow().manifest().tile_size;
        let mut pixels = vec![[0.0; 4]; (edge as usize).pow(2)];
        pixels[2 * edge as usize + 2] = [0.5, 0.0, 0.0, 0.5];
        core.apply(
            Operation::RasterEdit {
                layers: vec![0],
                label: "Fixture".into(),
            },
            vec![TileEdit {
                address: TileAddress {
                    layer: 0,
                    x: 0,
                    y: 0,
                },
                pixels: Some(encode_tile(&pixels, TileFormat::Rgba8)),
            }],
        )
        .unwrap();
        (dir, core)
    }
    #[test]
    fn cut_paste_and_transform_are_transactional_and_undoable() {
        let (_dir, mut core) = seeded();
        let clipboard = copy_selection(&mut core, true).unwrap();
        assert_eq!(Sampler::new(&core, 0).pixel(2, 2).unwrap(), [0.0; 4]);
        core.undo(false).unwrap();
        assert!(Sampler::new(&core, 0).pixel(2, 2).unwrap()[3] > 0.49);
        paste(&mut core, &clipboard).unwrap();
        let layer = core.active_layer().unwrap();
        assert_ne!(layer, 0);
        transform_selection(&mut core, Transform::translation(1.0, 0.0).unwrap()).unwrap();
        let mut sampler = Sampler::new(&core, layer);
        assert_eq!(sampler.pixel(2, 2).unwrap(), [0.0; 4]);
        assert!(sampler.pixel(3, 2).unwrap()[3] > 0.49);
        core.undo(false).unwrap();
        assert!(Sampler::new(&core, layer).pixel(2, 2).unwrap()[3] > 0.49);
    }
    #[test]
    fn normal_merge_matches_reference_and_undo_restores_both_layers() {
        let (_dir, mut core) = seeded();
        let clipboard = copy_selection(&mut core, false).unwrap();
        paste(&mut core, &clipboard).unwrap();
        let top = core.active_layer().unwrap();
        merge_down(&mut core, top).unwrap();
        assert_eq!(core.document.borrow().state().layers.len(), 1);
        assert!((Sampler::new(&core, 0).pixel(2, 2).unwrap()[3] - 0.75).abs() < 0.01);
        core.undo(false).unwrap();
        assert_eq!(core.document.borrow().state().layers.len(), 2);
    }
}
