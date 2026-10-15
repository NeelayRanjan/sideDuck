//! Bounded raster rendering from sparse tile stores and persisted normal-range caches.
//!
//! Segment-tree nodes contain linear premultiplied RGBA16F. Nodes are rebuilt
//! from their children after invalidation; no inverse compositing is used. A
//! depth-nine tree has a stated absolute component tolerance of 0.0045 against
//! the RGBA32 scalar reference. GPU blending itself retains its tighter tolerance.
//! A prepared normal viewport needs two passes. Dependent stacks are prepared by
//! streaming ordered passes into a snapshot and must be prepared again after edits.

use crate::blend::{sanitize, BlendMode, Pixel};
use crate::cache::{compile_plan, LayerDescriptor, TileAddress};
use crate::gpu::{GpuCompositor, GpuImage};
use half::f16;
use sideduck_tile::{PixelFormat, StoreOptions, TileKey, TileStore};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

type Result<T> = std::result::Result<T, String>;
const NODE_MASK: u32 = 0x00ff_ffff;
const METADATA_LIMIT: usize = 64 * 1024 * 1024;

/// Flush queued uploads on every error path before another caller can reserve
/// the released image budget. Successful batches explicitly disarm this guard.
struct QueueDrain<'a> {
    gpu: &'a GpuCompositor,
    armed: bool,
}
impl<'a> QueueDrain<'a> {
    fn new(gpu: &'a GpuCompositor) -> Self {
        Self { gpu, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for QueueDrain<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.gpu.submit(self.gpu.encoder());
            self.gpu.wait();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl RenderRect {
    fn validate(self) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || self.x.checked_add(self.width).is_none()
            || self.y.checked_add(self.height).is_none()
        {
            return Err("viewport dimensions must be nonzero without coordinate overflow".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RasterLayer {
    pub descriptor: LayerDescriptor,
    /// A source layer whose alpha masks this layer. Missing mask tiles are transparent.
    /// Increment this layer's revision or call invalidate_tile when an external mask changes.
    pub mask_layer: Option<u64>,
    /// Explicit nearest lower unclipped base. Clipping groups are contiguous;
    /// a different base is rejected instead of silently reordering layers.
    /// With descriptor.clipping and None, use the nearest lower unclipped layer.
    pub clip_base: Option<u64>,
}
impl From<LayerDescriptor> for RasterLayer {
    fn from(descriptor: LayerDescriptor) -> Self {
        Self {
            descriptor,
            mask_layer: None,
            clip_base: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RawTile {
    pub format: PixelFormat,
    pub bytes: Vec<u8>,
}

pub trait RasterSource {
    fn tile_size(&self) -> u32;
    /// Return one owned tile only. Higher levels are already reduced source tiles.
    fn read_tile(&mut self, layer: u64, address: TileAddress) -> Result<Option<RawTile>>;
}

pub struct StoreSource<'a> {
    levels: BTreeMap<u8, &'a mut TileStore>,
    tile_size: u32,
}
impl<'a> StoreSource<'a> {
    pub fn new(base: &'a mut TileStore) -> Self {
        let tile_size = base.options().tile_size;
        Self {
            levels: BTreeMap::from([(0, base)]),
            tile_size,
        }
    }
    pub fn add_level(&mut self, level: u8, store: &'a mut TileStore) -> Result<()> {
        if level == 0
            || self.levels.contains_key(&level)
            || store.options().tile_size != self.tile_size
        {
            return Err("mip level must be unique and use the base tile size".into());
        }
        self.levels.insert(level, store);
        Ok(())
    }
}
impl RasterSource for StoreSource<'_> {
    fn tile_size(&self) -> u32 {
        self.tile_size
    }
    fn read_tile(&mut self, layer: u64, address: TileAddress) -> Result<Option<RawTile>> {
        let store = self
            .levels
            .get_mut(&address.level)
            .ok_or("requested mip level has no persisted source")?;
        let format = store.options().format;
        let layer =
            u32::try_from(layer).map_err(|_| "layer identifier exceeds tile-store range")?;
        store
            .get(TileKey {
                layer,
                x: address.x,
                y: address.y,
            })
            .map(|bytes| {
                bytes.map(|bytes| RawTile {
                    format,
                    bytes: bytes.to_vec(),
                })
            })
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RasterStats {
    pub source_reads: u64,
    pub derived_hits: u64,
    pub derived_writes: u64,
    pub invalidations: u64,
    pub gpu_cache_bytes: u64,
    pub peak_gpu_cache_bytes: u64,
    pub gpu_passes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NodeKey {
    node: u32,
    level: u8,
    x: u32,
    y: u32,
}
impl NodeKey {
    fn store_key(self) -> TileKey {
        TileKey {
            layer: (u32::from(self.level) << 24) | self.node,
            x: self.x,
            y: self.y,
        }
    }
    fn address(self) -> TileAddress {
        TileAddress {
            level: self.level,
            x: self.x,
            y: self.y,
        }
    }
}
struct CachedGpu {
    image: Arc<GpuImage>,
    touched: u128,
}

pub struct RasterRenderer {
    directory: PathBuf,
    derived: TileStore,
    tile_size: u32,
    layers: Vec<RasterLayer>,
    signature: Vec<u8>,
    leaf_base: u32,
    independent: Vec<bool>,
    known: BTreeSet<NodeKey>,
    gpu_cache: BTreeMap<NodeKey, CachedGpu>,
    gpu_cache_budget: u64,
    gpu_identity: Option<u64>,
    clock: u128,
    stats: RasterStats,
    source_versions: BTreeMap<u64, u64>,
    last_change: BTreeMap<u64, TileAddress>,
    generation: Arc<AtomicU64>,
}
impl RasterRenderer {
    pub fn create(
        path: impl AsRef<Path>,
        tile_size: u32,
        decoded_cache_bytes: usize,
        gpu_cache_bytes: u64,
    ) -> Result<Self> {
        let directory = path.as_ref().to_path_buf();
        let derived = TileStore::create(
            &directory,
            StoreOptions {
                tile_size,
                format: PixelFormat::Rgba16Float,
                cache_budget_bytes: decoded_cache_bytes,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Self::new(directory, derived, tile_size, gpu_cache_bytes))
    }
    pub fn open(
        path: impl AsRef<Path>,
        tile_size: u32,
        decoded_cache_bytes: usize,
        gpu_cache_bytes: u64,
    ) -> Result<Self> {
        let directory = path.as_ref().to_path_buf();
        let derived = TileStore::open(
            &directory,
            StoreOptions {
                tile_size,
                format: PixelFormat::Rgba16Float,
                cache_budget_bytes: decoded_cache_bytes,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut renderer = Self::new(directory, derived, tile_size, gpu_cache_bytes);
        renderer.read_metadata()?;
        Ok(renderer)
    }
    fn new(directory: PathBuf, derived: TileStore, tile_size: u32, gpu_cache_budget: u64) -> Self {
        Self {
            directory,
            derived,
            tile_size,
            layers: Vec::new(),
            signature: Vec::new(),
            leaf_base: 1,
            independent: Vec::new(),
            known: BTreeSet::new(),
            gpu_cache: BTreeMap::new(),
            gpu_cache_budget,
            gpu_identity: None,
            clock: 0,
            stats: RasterStats::default(),
            source_versions: BTreeMap::new(),
            last_change: BTreeMap::new(),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn stats(&self) -> RasterStats {
        self.stats
    }
    pub fn derived_store_stats(&self) -> sideduck_tile::StoreStats {
        self.derived.stats()
    }
    pub fn clear_gpu_cache(&mut self) {
        self.gpu_cache.clear();
        self.stats.gpu_cache_bytes = 0;
    }
    fn device(&mut self, gpu: &GpuCompositor) {
        if self.gpu_identity != Some(gpu.device_identity()) {
            self.clear_gpu_cache();
            self.gpu_identity = Some(gpu.device_identity());
        }
    }
    fn configure(&mut self, source: &impl RasterSource, layers: &[RasterLayer]) -> Result<()> {
        if source.tile_size() != self.tile_size || layers.is_empty() {
            return Err("raster stack must be nonempty and match the source tile size".into());
        }
        let count = u32::try_from(layers.len()).map_err(|_| "too many raster layers")?;
        let base = count
            .checked_next_power_of_two()
            .filter(|base| *base <= 1 << 23)
            .ok_or("segment-tree address limit exceeded")?;
        let descriptors: Vec<_> = layers
            .iter()
            .map(|layer| layer.descriptor.clone())
            .collect();
        compile_plan(&descriptors, 0).map_err(|e| e.to_string())?;
        let ids: BTreeSet<_> = layers.iter().map(|layer| layer.descriptor.id).collect();
        if ids.len() != layers.len() {
            return Err("layer identifiers must be unique".into());
        }
        let mut nearest_base = None;
        for layer in layers {
            if is_clipped(layer) {
                if layer.clip_base.is_some() && layer.clip_base != nearest_base {
                    return Err("clipping base must be the nearest lower unclipped layer".into());
                }
            } else {
                nearest_base = Some(layer.descriptor.id);
            }
        }
        let signature = layer_signature(layers);
        if signature != self.signature {
            self.generation.fetch_add(1, Ordering::Relaxed);
            let same_structure = self.layers.len() == layers.len()
                && self
                    .layers
                    .iter()
                    .zip(layers)
                    .all(|(a, b)| a.descriptor.id == b.descriptor.id);
            if same_structure {
                let changed: BTreeSet<_> = self
                    .layers
                    .iter()
                    .zip(layers)
                    .filter_map(|(a, b)| (a != b).then_some(a.descriptor.id))
                    .collect();
                let mut affected = BTreeSet::new();
                for (index, layer) in layers.iter().enumerate() {
                    if changed.contains(&layer.descriptor.id)
                        || layer.mask_layer.is_some_and(|id| changed.contains(&id))
                        || layer.clip_base.is_some_and(|id| changed.contains(&id))
                    {
                        ancestors(base + index as u32, &mut affected);
                    }
                }
                self.invalidate_matching(|key| affected.contains(&key.node));
            } else {
                self.invalidate_matching(|_| true);
            }
            self.signature = signature;
        }
        self.layers = layers.to_vec();
        let mut sources = ids;
        sources.extend(layers.iter().filter_map(|layer| layer.mask_layer));
        self.source_versions.retain(|id, _| sources.contains(id));
        self.last_change.retain(|id, _| sources.contains(id));
        for id in sources {
            self.source_versions.entry(id).or_default();
        }
        self.leaf_base = base;
        self.independent = vec![true; base as usize * 2];
        for (index, layer) in layers.iter().enumerate() {
            self.independent[base as usize + index] = !layer.descriptor.visible
                || (layer.descriptor.independent() && layer.clip_base.is_none());
        }
        for node in (1..base as usize).rev() {
            self.independent[node] = self.independent[node * 2] && self.independent[node * 2 + 1];
        }
        Ok(())
    }
    /// Build real normal-range pixels for the visible tile region, outside the interactive timing gate.
    pub fn warm_viewport(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        layers: &[RasterLayer],
        level: u8,
        rect: RenderRect,
    ) -> Result<()> {
        rect.validate()?;
        self.device(gpu);
        self.configure(source, layers)?;
        for address in addresses(rect, level, self.tile_size) {
            self.warm_node(
                source,
                NodeKey {
                    node: 1,
                    level,
                    x: address.x,
                    y: address.y,
                },
            )?;
        }
        Ok(())
    }
    fn warm_node(&mut self, source: &mut impl RasterSource, key: NodeKey) -> Result<()> {
        if self.independent[key.node as usize] {
            self.node_bytes(source, key)?;
        } else if key.node < self.leaf_base {
            self.warm_node(
                source,
                NodeKey {
                    node: key.node * 2,
                    ..key
                },
            )?;
            self.warm_node(
                source,
                NodeKey {
                    node: key.node * 2 + 1,
                    ..key
                },
            )?;
        }
        Ok(())
    }
    pub fn prepare_viewport(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        layers: &[RasterLayer],
        active: usize,
        level: u8,
        rect: RenderRect,
    ) -> Result<PreparedViewport> {
        rect.validate()?;
        self.device(gpu);
        self.configure(source, layers)?;
        if active >= layers.len() {
            return Err("active layer index is outside the raster stack".into());
        }
        if self.independent[1] {
            let below = self.assemble_range(gpu, source, 0, active as u32, level, rect)?;
            let active_image =
                self.assemble_range(gpu, source, active as u32, active as u32 + 1, level, rect)?;
            let above = self.assemble_range(
                gpu,
                source,
                active as u32 + 1,
                layers.len() as u32,
                level,
                rect,
            )?;
            Ok(PreparedViewport {
                below: Some(below),
                active: Some(active_image),
                above: Some(above),
                work: Some(gpu.create_image(rect.width, rect.height)?),
                output: gpu.create_image(rect.width, rect.height)?,
                dependent: false,
                rect,
                level,
                active_index: active,
                signature: self.signature.clone(),
                device_identity: gpu.device_identity(),
                dependencies: self.dependencies(active),
                active_versions: self.active_versions(active),
                generation: self.generation.clone(),
                prepared_generation: self.generation.load(Ordering::Relaxed),
            })
        } else {
            let output = gpu.create_image(rect.width, rect.height)?;
            for address in addresses(rect, level, self.tile_size) {
                let tile = self.general_tile(gpu, source, address)?;
                copy_tile(gpu, &tile, &output, rect, address, self.tile_size)?;
            }
            Ok(PreparedViewport {
                below: None,
                active: None,
                above: None,
                work: None,
                output,
                dependent: true,
                rect,
                level,
                active_index: active,
                signature: self.signature.clone(),
                device_identity: gpu.device_identity(),
                dependencies: self.dependencies(active),
                active_versions: self.active_versions(active),
                generation: self.generation.clone(),
                prepared_generation: self.generation.load(Ordering::Relaxed),
            })
        }
    }
    /// Switch the active layer while retaining the five viewport allocations.
    /// The old viewport is consumed so a partially rebuilt atlas cannot escape.
    pub fn switch_active(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        layers: &[RasterLayer],
        active: usize,
        mut previous: PreparedViewport,
    ) -> Result<PreparedViewport> {
        let rect = previous.rect;
        let level = previous.level;
        self.device(gpu);
        self.configure(source, layers)?;
        if active >= layers.len() {
            return Err("active layer index is outside the raster stack".into());
        }
        if previous.dependent
            || !self.independent[1]
            || previous.device_identity != gpu.device_identity()
        {
            drop(previous);
            return self.prepare_viewport(gpu, source, layers, active, level, rect);
        }
        for (range, atlas) in [
            (0..active as u32, previous.below.as_ref()),
            (active as u32..active as u32 + 1, previous.active.as_ref()),
            (
                active as u32 + 1..layers.len() as u32,
                previous.above.as_ref(),
            ),
        ] {
            self.assemble_into(
                gpu,
                source,
                range,
                level,
                rect,
                atlas.expect("independent viewport atlas"),
            )?;
        }
        previous.active_index = active;
        previous.signature = self.signature.clone();
        previous.dependencies = self.dependencies(active);
        previous.active_versions = self.active_versions(active);
        previous.generation = self.generation.clone();
        previous.prepared_generation = self.generation.load(Ordering::Relaxed);
        Ok(previous)
    }
    fn dependencies(&self, active: usize) -> BTreeMap<u64, u64> {
        let mut dependencies = BTreeMap::new();
        for (index, layer) in self.layers.iter().enumerate() {
            if index != active {
                for id in std::iter::once(layer.descriptor.id).chain(layer.mask_layer) {
                    dependencies.insert(id, self.source_versions.get(&id).copied().unwrap_or(0));
                }
            }
        }
        dependencies
    }
    fn active_versions(&self, active: usize) -> Vec<(u64, u64)> {
        let layer = &self.layers[active];
        std::iter::once(layer.descriptor.id)
            .chain(layer.mask_layer)
            .map(|id| (id, self.source_versions.get(&id).copied().unwrap_or(0)))
            .collect()
    }
    /// Refresh one active tile after the source changes, keeping the below/above
    /// atlases intact. Opacity and layer mask are baked into the active atlas.
    /// Returns false for tiles outside this viewport or its selected mip level.
    /// Dependent snapshots or a changed stack must be prepared again.
    /// Refresh after each tile notification; multiple pending changes require
    /// preparation so a single refresh cannot conceal another stale active tile.
    pub fn update_active_tile(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        prepared: &mut PreparedViewport,
        address: TileAddress,
    ) -> Result<bool> {
        if prepared.dependent
            || prepared.signature != self.signature
            || prepared.device_identity != gpu.device_identity()
            || source.tile_size() != self.tile_size
            || prepared.dependencies != self.dependencies(prepared.active_index)
        {
            return Err("active tile update requires a current independent viewport".into());
        }
        for &(id, version) in &prepared.active_versions {
            let current = self.source_versions.get(&id).copied().unwrap_or(0);
            if current != version
                && (current != version.wrapping_add(1)
                    || self.last_change.get(&id) != Some(&address))
            {
                return Err("multiple pending active changes require viewport preparation".into());
            }
        }
        let active_id = self.layers[prepared.active_index].descriptor.id;
        if self.layers.iter().enumerate().any(|(index, layer)| {
            index != prepared.active_index && layer.mask_layer == Some(active_id)
        }) {
            return Err("active layer is a mask dependency; prepare the viewport again".into());
        }
        self.invalidate_tile(active_id, address);
        let rect = prepared.rect;
        let tile_x = u64::from(address.x) * u64::from(self.tile_size);
        let tile_y = u64::from(address.y) * u64::from(self.tile_size);
        if address.level != prepared.level {
            return Ok(false);
        }
        if tile_x >= u64::from(rect.x + rect.width)
            || tile_y >= u64::from(rect.y + rect.height)
            || tile_x + u64::from(self.tile_size) <= u64::from(rect.x)
            || tile_y + u64::from(self.tile_size) <= u64::from(rect.y)
        {
            prepared.prepared_generation = self.generation.load(Ordering::Relaxed);
            prepared.active_versions = self.active_versions(prepared.active_index);
            return Ok(false);
        }
        let bytes = self.effective_half(source, prepared.active_index, address)?;
        let tile = gpu.create_image(self.tile_size, self.tile_size)?;
        let mut encoder = gpu.encoder();
        let mut held = None;
        let mut zero = None;
        let mut drain = QueueDrain::new(gpu);
        if let Some(bytes) = bytes {
            let empty = gpu.create_image(self.tile_size, self.tile_size)?;
            let image = gpu.upload_rgba16f(self.tile_size, self.tile_size, &bytes)?;
            gpu.encode_blend(&mut encoder, &empty, &image, &tile, BlendMode::Normal, 1.0)?;
            self.stats.gpu_passes += 1;
            held = Some(image);
            zero = Some(empty);
        }
        encode_copy(
            &mut encoder,
            &tile,
            prepared.active.as_ref().expect("independent active atlas"),
            rect,
            address,
            self.tile_size,
        );
        gpu.submit(encoder);
        gpu.wait();
        drain.disarm();
        drop((held, zero));
        prepared.prepared_generation = self.generation.load(Ordering::Relaxed);
        prepared.active_versions = self.active_versions(prepared.active_index);
        Ok(true)
    }
    fn node_bytes(
        &mut self,
        source: &mut impl RasterSource,
        key: NodeKey,
    ) -> Result<Option<Vec<u8>>> {
        if self.known.contains(&key) {
            self.stats.derived_hits += 1;
            return self
                .derived
                .get(key.store_key())
                .map(|bytes| bytes.map(<[u8]>::to_vec))
                .map_err(|e| e.to_string());
        }
        if !self.independent[key.node as usize] {
            return Err("backdrop-dependent ranges cannot be flattened independently".into());
        }
        let pixels = if key.node >= self.leaf_base {
            let index = (key.node - self.leaf_base) as usize;
            if index >= self.layers.len() {
                None
            } else {
                self.effective_half(source, index, key.address())?
            }
        } else {
            let lower = self.node_bytes(
                source,
                NodeKey {
                    node: key.node * 2,
                    ..key
                },
            )?;
            let upper = self.node_bytes(
                source,
                NodeKey {
                    node: key.node * 2 + 1,
                    ..key
                },
            )?;
            join_half(lower, upper)
        };
        if let Some(bytes) = &pixels {
            self.derived
                .put(key.store_key(), bytes)
                .map_err(|e| e.to_string())?;
        } else {
            self.derived.remove(key.store_key());
        }
        self.known.insert(key);
        self.stats.derived_writes += 1;
        // Derived invalidations are consumed here; retaining them would scale twice with the tree.
        self.derived.drain_dirty_tiles();
        self.derived.drain_dirty_layers();
        Ok(pixels)
    }
    fn read(
        &mut self,
        source: &mut impl RasterSource,
        layer: u64,
        address: TileAddress,
    ) -> Result<Option<RawTile>> {
        self.stats.source_reads += 1;
        let tile = source.read_tile(layer, address)?;
        if let Some(tile) = &tile {
            let expected =
                self.tile_size as usize * self.tile_size as usize * tile.format.bytes_per_pixel();
            if tile.bytes.len() != expected || tile.bytes.capacity() > expected {
                return Err("raster source must return exactly one tile allocation".into());
            }
        }
        Ok(tile)
    }
    fn effective_half(
        &mut self,
        source: &mut impl RasterSource,
        index: usize,
        address: TileAddress,
    ) -> Result<Option<Vec<u8>>> {
        let layer = self.layers[index].clone();
        if !layer.descriptor.visible || layer.descriptor.opacity == 0.0 {
            return Ok(None);
        }
        let Some(tile) = self.read(source, layer.descriptor.id, address)? else {
            return Ok(None);
        };
        let mask = match layer.mask_layer {
            Some(id) => match self.read(source, id, address)? {
                Some(tile) => Some(tile),
                None => return Ok(None),
            },
            None => None,
        };
        let count = self.tile_size as usize * self.tile_size as usize;
        let mut output = vec![0; count * 8];
        let mut nonempty = false;
        if tile.format == PixelFormat::Rgba8 && layer.descriptor.opacity == 1.0 && mask.is_none() {
            let table: [[u8; 2]; 256] =
                std::array::from_fn(|value| f16::from_f32(value as f32 / 255.0).to_le_bytes());
            for (pixel, out) in tile.bytes.chunks_exact(4).zip(output.chunks_exact_mut(8)) {
                let alpha = pixel[3];
                nonempty |= alpha != 0;
                for (channel, bytes) in pixel.iter().zip(out.chunks_exact_mut(2)) {
                    bytes.copy_from_slice(&table[(*channel).min(alpha) as usize]);
                }
            }
            return Ok(nonempty.then_some(output));
        }
        for pixel in 0..count {
            let mut color = decode(&tile, pixel);
            let mut amount = layer.descriptor.opacity;
            if let Some(mask) = &mask {
                amount *= decode(mask, pixel)[3];
            }
            for channel in &mut color {
                *channel *= amount;
            }
            write_half(&mut output[pixel * 8..pixel * 8 + 8], color);
            nonempty |= output[pixel * 8 + 6] != 0 || output[pixel * 8 + 7] & 0x7f != 0;
        }
        Ok(nonempty.then_some(output))
    }
    /// Decode at most 32 misses concurrently, transferring each allocation
    /// straight to upload rather than duplicating it in the decoded LRU.
    fn gpu_nodes(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        keys: &[NodeKey],
    ) -> Result<Vec<Option<Arc<GpuImage>>>> {
        let mut output = vec![None; keys.len()];
        let mut missing = Vec::new();
        for (index, &key) in keys.iter().enumerate() {
            self.clock += 1;
            if let Some(entry) = self.gpu_cache.get_mut(&key) {
                entry.touched = self.clock;
                output[index] = Some(entry.image.clone());
            } else {
                if !self.known.contains(&key) {
                    self.node_bytes(source, key)?;
                }
                missing.push((index, key));
            }
        }
        let tile_bytes = self.tile_size as usize * self.tile_size as usize * 8;
        for chunk in missing.chunks(32) {
            let store_keys: Vec<_> = chunk.iter().map(|(_, key)| key.store_key()).collect();
            let decoded = self
                .derived
                .read_batch_owned(&store_keys, 32 * tile_bytes, 8)
                .map_err(|error| error.to_string())?;
            self.stats.derived_hits += chunk.len() as u64;
            for ((index, key), bytes) in chunk.iter().zip(decoded) {
                if let Some(bytes) = bytes {
                    output[*index] = Some(self.cache_uploaded(gpu, *key, &bytes)?);
                }
            }
        }
        Ok(output)
    }
    fn cache_uploaded(
        &mut self,
        gpu: &GpuCompositor,
        key: NodeKey,
        bytes: &[u8],
    ) -> Result<Arc<GpuImage>> {
        let needed = self.tile_size as u64 * self.tile_size as u64 * 8;
        let mut recycled = None;
        while !self.gpu_cache.is_empty()
            && (self.stats.gpu_cache_bytes.saturating_add(needed) > self.gpu_cache_budget
                || gpu.allocated_bytes().saturating_add(2 * needed) > gpu.budget_bytes())
        {
            let oldest = *self
                .gpu_cache
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .expect("nonempty GPU cache")
                .0;
            if let Some(entry) = self.gpu_cache.remove(&oldest) {
                self.stats.gpu_cache_bytes -= entry.image.byte_len();
                // Current unsubmitted batches retain their own Arc. A unique
                // cache owner therefore belongs to a completed earlier batch.
                if recycled.is_none() && Arc::strong_count(&entry.image) == 1 {
                    recycled = Some(entry.image);
                }
            }
        }
        let image = if let Some(image) = recycled {
            gpu.write_rgba16f(&image, bytes)?;
            image
        } else {
            Arc::new(gpu.upload_rgba16f(self.tile_size, self.tile_size, bytes)?)
        };
        if needed <= self.gpu_cache_budget {
            self.clock += 1;
            self.stats.gpu_cache_bytes += image.byte_len();
            self.stats.peak_gpu_cache_bytes = self
                .stats
                .peak_gpu_cache_bytes
                .max(self.stats.gpu_cache_bytes);
            self.gpu_cache.insert(
                key,
                CachedGpu {
                    image: image.clone(),
                    touched: self.clock,
                },
            );
        }
        Ok(image)
    }
    fn assemble_range(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        begin: u32,
        end: u32,
        level: u8,
        rect: RenderRect,
    ) -> Result<GpuImage> {
        let atlas = gpu.create_image(rect.width, rect.height)?;
        self.assemble_into(gpu, source, begin..end, level, rect, &atlas)?;
        Ok(atlas)
    }
    fn assemble_into(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        range: std::ops::Range<u32>,
        level: u8,
        rect: RenderRect,
        atlas: &GpuImage,
    ) -> Result<()> {
        if range.is_empty() {
            let mut encoder = gpu.encoder();
            gpu.encode_clear(&mut encoder, atlas)?;
            gpu.submit(encoder);
            gpu.wait();
            return Ok(());
        }
        let mut nodes = Vec::new();
        collect_nodes(1, 0, self.leaf_base, range.start, range.end, &mut nodes);
        let mut current = gpu.create_image(self.tile_size, self.tile_size)?;
        let mut next = gpu.create_image(self.tile_size, self.tile_size)?;
        let mut encoder = gpu.encoder();
        let mut held = Vec::new();
        let mut drain = QueueDrain::new(gpu);
        let mut addresses = addresses(rect, level, self.tile_size).peekable();
        // Bound each decode call to 16 MiB; retain at most 32 visible tiles'
        // GPU sources until their submitted commands have completed.
        while addresses.peek().is_some() {
            let batch: Vec<_> = addresses.by_ref().take(32).collect();
            let keys: Vec<_> = batch
                .iter()
                .flat_map(|address| {
                    nodes.iter().map(move |&node| NodeKey {
                        node,
                        level,
                        x: address.x,
                        y: address.y,
                    })
                })
                .collect();
            let images = self.gpu_nodes(gpu, source, &keys)?;
            for (address, images) in batch.iter().zip(images.chunks(nodes.len())) {
                gpu.encode_clear(&mut encoder, &current)?;
                for image in images.iter().flatten() {
                    gpu.encode_blend(&mut encoder, &current, image, &next, BlendMode::Normal, 1.0)?;
                    self.stats.gpu_passes += 1;
                    std::mem::swap(&mut current, &mut next);
                    held.push(image.clone());
                }
                encode_copy(
                    &mut encoder,
                    &current,
                    atlas,
                    rect,
                    *address,
                    self.tile_size,
                );
            }
            gpu.submit(encoder);
            gpu.wait();
            held.clear();
            encoder = gpu.encoder();
        }
        drain.disarm();
        Ok(())
    }
    fn general_tile(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        address: TileAddress,
    ) -> Result<GpuImage> {
        let mut current = gpu.create_image(self.tile_size, self.tile_size)?;
        let mut next = gpu.create_image(self.tile_size, self.tile_size)?;
        let mut drain = QueueDrain::new(gpu);
        let mut index = 0;
        while index < self.layers.len() {
            // An orphan clipped layer has no coverage and contributes no pixels.
            if is_clipped(&self.layers[index]) {
                index += 1;
                continue;
            }
            let base = index;
            index += 1;
            while index < self.layers.len() && is_clipped(&self.layers[index]) {
                index += 1;
            }
            let Some(pixels) = self.effective_pixels(source, base, address)? else {
                continue;
            };
            let image = gpu.upload(self.tile_size, self.tile_size, &pixels)?;
            let mut encoder = gpu.encoder();
            if index == base + 1 {
                gpu.encode_blend(
                    &mut encoder,
                    &current,
                    &image,
                    &next,
                    self.layers[base].descriptor.blend_mode,
                    1.0,
                )?;
                self.stats.gpu_passes += 1;
            } else {
                // Source-atop preserves the base coverage exactly. Masking each
                // clipped source then applying source-over would increase alpha.
                let mut group = gpu.create_image(self.tile_size, self.tile_size)?;
                let mut scratch = gpu.create_image(self.tile_size, self.tile_size)?;
                gpu.encode_blend(
                    &mut encoder,
                    &scratch,
                    &image,
                    &group,
                    BlendMode::Normal,
                    1.0,
                )?;
                self.stats.gpu_passes += 1;
                let mut held = Vec::new();
                for clipped in base + 1..index {
                    let Some(pixels) = self.effective_pixels(source, clipped, address)? else {
                        continue;
                    };
                    let top = gpu.upload(self.tile_size, self.tile_size, &pixels)?;
                    gpu.encode_clip(
                        &mut encoder,
                        &group,
                        &top,
                        &scratch,
                        self.layers[clipped].descriptor.blend_mode,
                        1.0,
                    )?;
                    self.stats.gpu_passes += 1;
                    std::mem::swap(&mut group, &mut scratch);
                    held.push(top);
                    if held.len() == 16 {
                        gpu.submit(encoder);
                        gpu.wait();
                        held.clear();
                        encoder = gpu.encoder();
                    }
                }
                gpu.encode_blend(
                    &mut encoder,
                    &current,
                    &group,
                    &next,
                    self.layers[base].descriptor.blend_mode,
                    1.0,
                )?;
                self.stats.gpu_passes += 1;
                // Keep group and uploaded images alive until their commands finish.
                gpu.submit(encoder);
                gpu.wait();
                std::mem::swap(&mut current, &mut next);
                continue;
            }
            gpu.submit(encoder);
            gpu.wait();
            std::mem::swap(&mut current, &mut next);
        }
        drain.disarm();
        Ok(current)
    }
    // General blend modes may amplify quantization near color-dodge/burn edges.
    // Keep their source/mask/opacity calculation in f32; only independent cached
    // normal ranges use the explicitly bounded half-precision representation.
    fn effective_pixels(
        &mut self,
        source: &mut impl RasterSource,
        index: usize,
        address: TileAddress,
    ) -> Result<Option<Vec<Pixel>>> {
        let layer = self.layers[index].clone();
        if !layer.descriptor.visible || layer.descriptor.opacity == 0.0 {
            return Ok(None);
        }
        let Some(tile) = self.read(source, layer.descriptor.id, address)? else {
            return Ok(None);
        };
        let mask = match layer.mask_layer {
            Some(id) => match self.read(source, id, address)? {
                Some(tile) => Some(tile),
                None => return Ok(None),
            },
            None => None,
        };
        let mut pixels = Vec::with_capacity(self.tile_size as usize * self.tile_size as usize);
        let mut nonempty = false;
        for index in 0..self.tile_size as usize * self.tile_size as usize {
            let mut pixel = decode(&tile, index);
            let amount =
                layer.descriptor.opacity * mask.as_ref().map_or(1.0, |mask| decode(mask, index)[3]);
            pixel.iter_mut().for_each(|v| *v *= amount);
            nonempty |= pixel[3] > 0.0;
            pixels.push(pixel);
        }
        Ok(nonempty.then_some(pixels))
    }
    /// Invalidate an edited source tile, dependent masks, and every cached mip ancestor.
    /// The source is responsible for regenerating stale persisted mip tiles before rendering.
    pub fn invalidate_tile(&mut self, layer: u64, address: TileAddress) -> usize {
        if let Some(version) = self.source_versions.get_mut(&layer) {
            *version = version.wrapping_add(1);
            self.last_change.insert(layer, address);
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        let mut affected = BTreeSet::new();
        for (index, candidate) in self.layers.iter().enumerate() {
            if candidate.descriptor.id == layer
                || candidate.mask_layer == Some(layer)
                || candidate.clip_base == Some(layer)
            {
                ancestors(self.leaf_base + index as u32, &mut affected);
            }
        }
        self.invalidate_matching(|key| {
            let Some(delta) = key.level.checked_sub(address.level) else {
                return false;
            };
            affected.contains(&key.node)
                && key.x == address.x.checked_shr(u32::from(delta)).unwrap_or(0)
                && key.y == address.y.checked_shr(u32::from(delta)).unwrap_or(0)
        })
    }
    fn invalidate_matching(&mut self, predicate: impl Fn(&NodeKey) -> bool) -> usize {
        let keys: Vec<_> = self
            .known
            .iter()
            .filter(|key| predicate(key))
            .copied()
            .collect();
        for key in &keys {
            self.known.remove(key);
            self.derived.remove(key.store_key());
            self.remove_gpu(*key);
        }
        self.stats.invalidations += keys.len() as u64;
        self.derived.drain_dirty_tiles();
        self.derived.drain_dirty_layers();
        keys.len()
    }
    fn remove_gpu(&mut self, key: NodeKey) {
        if let Some(entry) = self.gpu_cache.remove(&key) {
            self.stats.gpu_cache_bytes -= entry.image.byte_len();
        }
    }
    /// Stream a full canvas through one tile-sized GPU/readback result at a time.
    /// The consumer must not retain every callback tile without its own budget.
    pub fn render_canvas(
        &mut self,
        gpu: &GpuCompositor,
        source: &mut impl RasterSource,
        layers: &[RasterLayer],
        level: u8,
        canvas: [u32; 2],
        mut consume: impl FnMut(RenderRect, &[Pixel]) -> Result<()>,
    ) -> Result<()> {
        if canvas.contains(&0) {
            return Err("canvas must be nonempty".into());
        }
        for y in (0..canvas[1]).step_by(self.tile_size as usize) {
            for x in (0..canvas[0]).step_by(self.tile_size as usize) {
                let rect = RenderRect {
                    x,
                    y,
                    width: (canvas[0] - x).min(self.tile_size),
                    height: (canvas[1] - y).min(self.tile_size),
                };
                let mut prepared = self.prepare_viewport(gpu, source, layers, 0, level, rect)?;
                let mut encoder = gpu.encoder();
                prepared.encode(gpu, &mut encoder)?;
                gpu.submit(encoder);
                let pixels = gpu.readback(prepared.output())?;
                consume(rect, &pixels)?;
                gpu.wait();
            }
        }
        Ok(())
    }
    /// Commit derived blobs and their exact signature. Removing the old manifest
    /// before publishing blobs makes interrupted cache updates safely rebuildable.
    pub fn commit(&mut self) -> Result<()> {
        let metadata = self.metadata()?;
        let path = self.directory.join("raster.meta");
        if path.exists() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            File::open(&self.directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        self.derived.commit().map_err(|e| e.to_string())?;
        let pending = self.directory.join("raster.meta.next");
        let mut file = File::create(&pending).map_err(|e| e.to_string())?;
        file.write_all(&metadata)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(pending, path).map_err(|e| e.to_string())?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn metadata(&self) -> Result<Vec<u8>> {
        let length = 28_usize
            .checked_add(self.signature.len())
            .and_then(|n| {
                self.known
                    .len()
                    .checked_mul(13)
                    .and_then(|k| n.checked_add(k))
            })
            .ok_or("raster metadata overflow")?;
        if length > METADATA_LIMIT {
            return Err("raster metadata exceeds its 64 MiB ceiling".into());
        }
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(b"RASTERC1");
        bytes.extend_from_slice(&self.tile_size.to_le_bytes());
        bytes.extend_from_slice(&(self.signature.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.known.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.signature);
        for key in &self.known {
            bytes.extend_from_slice(&key.node.to_le_bytes());
            bytes.push(key.level);
            bytes.extend_from_slice(&key.x.to_le_bytes());
            bytes.extend_from_slice(&key.y.to_le_bytes());
        }
        bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
        Ok(bytes)
    }
    fn read_metadata(&mut self) -> Result<()> {
        let path = self.directory.join("raster.meta");
        if !path.exists() {
            return Ok(());
        }
        let length = fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if !(28..=METADATA_LIMIT as u64).contains(&length) {
            return Err("invalid raster cache metadata length".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let body = bytes.len() - 4;
        if &bytes[..8] != b"RASTERC1"
            || read32(&bytes, 8)? != self.tile_size
            || read32(&bytes, body)? != crc32fast::hash(&bytes[..body])
        {
            return Err("invalid raster cache metadata header or checksum".into());
        }
        let signature = read32(&bytes, 12)? as usize;
        let count = usize::try_from(u64::from_le_bytes(
            bytes[16..24]
                .try_into()
                .map_err(|_| "invalid metadata count")?,
        ))
        .map_err(|_| "metadata count overflow")?;
        let expected = count
            .checked_mul(13)
            .and_then(|n| n.checked_add(signature))
            .and_then(|n| n.checked_add(28))
            .ok_or("metadata size overflow")?;
        if expected != bytes.len() {
            return Err("invalid raster cache metadata record count".into());
        }
        self.signature = bytes[24..24 + signature].to_vec();
        for record in bytes[24 + signature..body].chunks_exact(13) {
            let key = NodeKey {
                node: read32(record, 0)?,
                level: record[4],
                x: read32(record, 5)?,
                y: read32(record, 9)?,
            };
            if key.node == 0 || key.node > NODE_MASK || !self.known.insert(key) {
                return Err("invalid or duplicated raster cache node".into());
            }
        }
        Ok(())
    }
}

pub struct PreparedViewport {
    below: Option<GpuImage>,
    active: Option<GpuImage>,
    above: Option<GpuImage>,
    work: Option<GpuImage>,
    output: GpuImage,
    dependent: bool,
    rect: RenderRect,
    level: u8,
    active_index: usize,
    signature: Vec<u8>,
    device_identity: u64,
    dependencies: BTreeMap<u64, u64>,
    active_versions: Vec<(u64, u64)>,
    generation: Arc<AtomicU64>,
    prepared_generation: u64,
}
impl PreparedViewport {
    pub fn encode<'a>(
        &'a mut self,
        gpu: &GpuCompositor,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<&'a GpuImage> {
        if self.device_identity != gpu.device_identity() {
            return Err("prepared viewport belongs to a different GPU device".into());
        }
        if self.prepared_generation != self.generation.load(Ordering::Relaxed) {
            return Err("prepared viewport was invalidated; refresh or prepare it again".into());
        }
        if let (Some(below), Some(active), Some(above), Some(work)) =
            (&self.below, &self.active, &self.above, &self.work)
        {
            gpu.encode_blend(encoder, below, active, work, BlendMode::Normal, 1.0)?;
            gpu.encode_blend(encoder, work, above, &self.output, BlendMode::Normal, 1.0)?;
        }
        Ok(&self.output)
    }
    pub fn output(&self) -> &GpuImage {
        &self.output
    }
    /// Linear premultiplied RGBA32F with active-layer opacity and mask baked in.
    pub fn active_image(&self) -> Option<&GpuImage> {
        self.active.as_ref()
    }
    pub fn requires_reprepare_on_edit(&self) -> bool {
        self.dependent
    }
}

fn addresses(rect: RenderRect, level: u8, edge: u32) -> impl Iterator<Item = TileAddress> {
    let min_x = rect.x / edge;
    let max_x = (rect.x + rect.width - 1) / edge;
    let min_y = rect.y / edge;
    let max_y = (rect.y + rect.height - 1) / edge;
    (min_y..=max_y).flat_map(move |y| (min_x..=max_x).map(move |x| TileAddress { level, x, y }))
}
fn is_clipped(layer: &RasterLayer) -> bool {
    layer.descriptor.clipping || layer.clip_base.is_some()
}
fn ancestors(mut node: u32, output: &mut BTreeSet<u32>) {
    while node > 0 {
        output.insert(node);
        node /= 2;
    }
}
fn collect_nodes(node: u32, start: u32, end: u32, lo: u32, hi: u32, output: &mut Vec<u32>) {
    if lo >= end || hi <= start {
        return;
    }
    if lo <= start && end <= hi {
        output.push(node);
        return;
    }
    let mid = start + (end - start) / 2;
    collect_nodes(node * 2, start, mid, lo, hi, output);
    collect_nodes(node * 2 + 1, mid, end, lo, hi, output);
}
fn half_pixel(bytes: &[u8], pixel: usize) -> Pixel {
    std::array::from_fn(|c| {
        f16::from_le_bytes([bytes[pixel * 8 + c * 2], bytes[pixel * 8 + c * 2 + 1]]).to_f32()
    })
}
fn decode(tile: &RawTile, pixel: usize) -> Pixel {
    sanitize(match tile.format {
        PixelFormat::Rgba8 => std::array::from_fn(|c| f32::from(tile.bytes[pixel * 4 + c]) / 255.0),
        PixelFormat::Rgba16Float => half_pixel(&tile.bytes, pixel),
    })
}
fn write_half(bytes: &mut [u8], pixel: Pixel) {
    for (out, value) in bytes.chunks_exact_mut(2).zip(pixel) {
        out.copy_from_slice(&f16::from_f32(value).to_le_bytes());
    }
}
fn join_half(lower: Option<Vec<u8>>, upper: Option<Vec<u8>>) -> Option<Vec<u8>> {
    match (lower, upper) {
        (None, other) | (other, None) => other,
        (Some(mut lower), Some(upper)) => {
            for pixel in 0..lower.len() / 8 {
                let offset = pixel * 8;
                let top_alpha = u16::from_le_bytes([upper[offset + 6], upper[offset + 7]]);
                if top_alpha & 0x7fff == 0 {
                    continue;
                }
                let bottom_alpha = u16::from_le_bytes([lower[offset + 6], lower[offset + 7]]);
                if top_alpha == 0x3c00 || bottom_alpha & 0x7fff == 0 {
                    lower[offset..offset + 8].copy_from_slice(&upper[offset..offset + 8]);
                    continue;
                }
                let bottom = half_pixel(&lower, pixel);
                let top = half_pixel(&upper, pixel);
                let mixed = std::array::from_fn(|c| top[c] + bottom[c] * (1.0 - top[3]));
                write_half(&mut lower[pixel * 8..pixel * 8 + 8], mixed);
            }
            Some(lower)
        }
    }
}
fn encode_copy(
    encoder: &mut wgpu::CommandEncoder,
    tile: &GpuImage,
    atlas: &GpuImage,
    rect: RenderRect,
    address: TileAddress,
    edge: u32,
) {
    let tile_x = address.x * edge;
    let tile_y = address.y * edge;
    let x = rect.x.max(tile_x);
    let y = rect.y.max(tile_y);
    let width = (u64::from(rect.x + rect.width).min(u64::from(tile_x) + u64::from(edge))
        - u64::from(x)) as u32;
    let height = (u64::from(rect.y + rect.height).min(u64::from(tile_y) + u64::from(edge))
        - u64::from(y)) as u32;
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tile.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: x - tile_x,
                y: y - tile_y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: atlas.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: x - rect.x,
                y: y - rect.y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}
fn copy_tile(
    gpu: &GpuCompositor,
    tile: &GpuImage,
    atlas: &GpuImage,
    rect: RenderRect,
    address: TileAddress,
    edge: u32,
) -> Result<()> {
    let mut encoder = gpu.encoder();
    encode_copy(&mut encoder, tile, atlas, rect, address, edge);
    gpu.submit(encoder);
    gpu.wait();
    Ok(())
}
fn layer_signature(layers: &[RasterLayer]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(layers.len() * 45);
    for layer in layers {
        bytes.extend_from_slice(&layer.descriptor.id.to_le_bytes());
        bytes.extend_from_slice(&(layer.descriptor.blend_mode as u32).to_le_bytes());
        bytes.extend_from_slice(&layer.descriptor.opacity.to_bits().to_le_bytes());
        bytes.push(u8::from(layer.descriptor.visible));
        bytes.push(u8::from(layer.descriptor.clipping));
        bytes.push(u8::from(layer.descriptor.group_backdrop_dependent));
        bytes.extend_from_slice(&layer.descriptor.revision.to_le_bytes());
        for id in [layer.mask_layer, layer.clip_base] {
            bytes.push(u8::from(id.is_some()));
            bytes.extend_from_slice(&id.unwrap_or(0).to_le_bytes());
        }
    }
    bytes
}
fn read32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("truncated raster metadata")?
            .try_into()
            .map_err(|_| "invalid metadata field")?,
    ))
}

#[cfg(test)]
#[path = "raster_tests.rs"]
mod tests;
