//! Sparse, bounded, per-layer mip tiles with lazy dirty-region regeneration.
//!
//! Missing pixels and pixels beyond the canvas are transparent. Each reduction
//! averages four premultiplied linear pixels, including transparent padding.
//! Sources may load persisted tiles at any level. A source returning None above
//! level zero requests generation from its four children.

use crate::blend::Pixel;
use std::collections::BTreeMap;
use std::fmt;
use std::mem::size_of;

const ENTRY_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MipKey {
    pub layer: u64,
    pub level: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PyramidError {
    InvalidGeometry,
    InvalidLevel,
    InvalidTileLength,
    InvalidPixels,
    SourceCapacityExceedsScratch,
    BudgetTooSmall,
    SizeOverflow,
    Source(String),
}

impl fmt::Display for PyramidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeometry => {
                f.write_str("mip geometry and zoom must be finite and positive")
            }
            Self::InvalidLevel => f.write_str("mip level exceeds the canvas pyramid"),
            Self::InvalidTileLength => f.write_str("mip source returned an incorrectly sized tile"),
            Self::InvalidPixels => {
                f.write_str("mip source contains non-finite pixels or invalid alpha")
            }
            Self::SourceCapacityExceedsScratch => {
                f.write_str("mip source allocation exceeds one tile of scratch memory")
            }
            Self::BudgetTooSmall => {
                f.write_str("mip budget must hold recursive scratch and at least one cached tile")
            }
            Self::SizeOverflow => f.write_str("mip byte count overflow"),
            Self::Source(message) => write!(f, "mip source: {message}"),
        }
    }
}

impl std::error::Error for PyramidError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PyramidStats {
    /// Cached pixel allocations plus conservative entry metadata.
    pub resident_bytes: usize,
    /// Conservative bound: maximum cached bytes plus all reserved scratch.
    pub peak_resident_bytes: usize,
    pub reserved_scratch_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub evictions: u64,
    pub source_reads: u64,
    pub generated_tiles: u64,
    pub invalidations: u64,
}

struct Entry {
    pixels: Option<Vec<Pixel>>,
    bytes: usize,
    touched: u128,
}

pub struct MipPyramid {
    canvas: [u32; 2],
    tile_size: u32,
    max_level: u8,
    pixel_count: usize,
    budget: usize,
    cache_budget: usize,
    entries: BTreeMap<MipKey, Entry>,
    clock: u128,
    stats: PyramidStats,
}

impl MipPyramid {
    pub fn new(
        canvas: [u32; 2],
        tile_size: u32,
        budget_bytes: usize,
    ) -> Result<Self, PyramidError> {
        if canvas.contains(&0) || tile_size == 0 || tile_size > 4096 {
            return Err(PyramidError::InvalidGeometry);
        }
        if budget_bytes < 32 {
            return Err(PyramidError::BudgetTooSmall);
        }
        let max_level = (32 - (canvas[0].max(canvas[1]) - 1).leading_zeros()) as u8;
        let pixel_count = (tile_size as usize)
            .checked_mul(tile_size as usize)
            .ok_or(PyramidError::SizeOverflow)?;
        let tile_bytes = pixel_count
            .checked_mul(size_of::<Pixel>())
            .ok_or(PyramidError::SizeOverflow)?;
        // At most one output at each recursive level, one returned child, and
        // one cache-insertion copy coexist. Source allocations must fit a tile.
        let scratch = tile_bytes
            .checked_mul(usize::from(max_level) + 2)
            .ok_or(PyramidError::SizeOverflow)?;
        let minimum = scratch
            .checked_add(tile_bytes)
            .and_then(|n| n.checked_add(ENTRY_BYTES))
            .ok_or(PyramidError::SizeOverflow)?;
        if budget_bytes < minimum {
            return Err(PyramidError::BudgetTooSmall);
        }
        Ok(Self {
            canvas,
            tile_size,
            max_level,
            pixel_count,
            budget: budget_bytes,
            cache_budget: budget_bytes - scratch,
            entries: BTreeMap::new(),
            clock: 0,
            stats: PyramidStats {
                reserved_scratch_bytes: scratch,
                ..PyramidStats::default()
            },
        })
    }

    pub fn max_level(&self) -> u8 {
        self.max_level
    }

    pub fn dimensions(&self, level: u8) -> Result<[u32; 2], PyramidError> {
        if level > self.max_level {
            return Err(PyramidError::InvalidLevel);
        }
        let divisor = 1_u64 << level;
        Ok(self
            .canvas
            .map(|size| u64::from(size).div_ceil(divisor) as u32))
    }

    pub fn level_for_zoom(&self, zoom: f64) -> Result<u8, PyramidError> {
        if !zoom.is_finite() || zoom <= 0.0 {
            return Err(PyramidError::InvalidGeometry);
        }
        Ok((-zoom.log2()).floor().clamp(0.0, f64::from(self.max_level)) as u8)
    }

    /// Returned pixels transfer to the caller and are outside this cache's
    /// residency budget. The caller must account for its retained output images.
    /// A source must invalidate any persisted ancestor tiles after a base edit.
    pub fn tile<F>(
        &mut self,
        key: MipKey,
        source: &mut F,
    ) -> Result<Option<Vec<Pixel>>, PyramidError>
    where
        F: FnMut(MipKey) -> Result<Option<Vec<Pixel>>, String>,
    {
        self.tile_with_persistence(key, source, &mut |_, _| Ok(()))
    }

    /// Persist generated tiles immediately, one borrowed tile at a time. The sink
    /// must not retain the borrowed buffer or build an unbounded write queue.
    pub fn tile_with_persistence<F, S>(
        &mut self,
        key: MipKey,
        source: &mut F,
        sink: &mut S,
    ) -> Result<Option<Vec<Pixel>>, PyramidError>
    where
        F: FnMut(MipKey) -> Result<Option<Vec<Pixel>>, String>,
        S: FnMut(MipKey, Option<&[Pixel]>) -> Result<(), String>,
    {
        let dimensions = self.dimensions(key.level)?;
        if key.x >= dimensions[0].div_ceil(self.tile_size)
            || key.y >= dimensions[1].div_ceil(self.tile_size)
        {
            return Ok(None);
        }
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stats.hits += 1;
            entry.touched = self.clock;
            return Ok(entry.pixels.clone());
        }
        self.stats.source_reads += 1;
        if let Some(mut pixels) = source(key).map_err(PyramidError::Source)? {
            self.validate_pixels(&pixels)?;
            self.clear_canvas_padding(key, dimensions, &mut pixels);
            let pixels = nonempty(pixels);
            self.insert(key, pixels.clone());
            return Ok(pixels);
        }
        if key.level == 0 {
            self.insert(key, None);
            return Ok(None);
        }
        let mut pixels = vec![[0.0; 4]; self.pixel_count];
        let child_dimensions = self.dimensions(key.level - 1)?;
        for quadrant_y in 0..2 {
            for quadrant_x in 0..2 {
                let child = MipKey {
                    layer: key.layer,
                    level: key.level - 1,
                    x: key.x * 2 + quadrant_x,
                    y: key.y * 2 + quadrant_y,
                };
                let Some(child_pixels) = self.tile_with_persistence(child, source, sink)? else {
                    continue;
                };
                let width = (child_dimensions[0] - child.x * self.tile_size).min(self.tile_size);
                let height = (child_dimensions[1] - child.y * self.tile_size).min(self.tile_size);
                for y in 0..height {
                    for x in 0..width {
                        let output_x = (quadrant_x * self.tile_size + x) / 2;
                        let output_y = (quadrant_y * self.tile_size + y) / 2;
                        let output = &mut pixels[(output_y * self.tile_size + output_x) as usize];
                        let input = child_pixels[(y * self.tile_size + x) as usize];
                        for channel in 0..4 {
                            output[channel] += input[channel] * 0.25;
                        }
                    }
                }
            }
        }
        let pixels = nonempty(pixels);
        sink(key, pixels.as_deref()).map_err(PyramidError::Source)?;
        self.stats.generated_tiles += 1;
        self.insert(key, pixels.clone());
        Ok(pixels)
    }

    fn validate_pixels(&self, pixels: &Vec<Pixel>) -> Result<(), PyramidError> {
        if pixels.len() != self.pixel_count {
            return Err(PyramidError::InvalidTileLength);
        }
        if pixels.capacity() > self.pixel_count {
            return Err(PyramidError::SourceCapacityExceedsScratch);
        }
        if pixels
            .iter()
            .any(|pixel| pixel.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&pixel[3]))
        {
            return Err(PyramidError::InvalidPixels);
        }
        Ok(())
    }

    fn clear_canvas_padding(&self, key: MipKey, dimensions: [u32; 2], pixels: &mut [Pixel]) {
        let width = (dimensions[0] - key.x * self.tile_size).min(self.tile_size);
        let height = (dimensions[1] - key.y * self.tile_size).min(self.tile_size);
        for y in 0..self.tile_size {
            let valid_width = if y < height { width } else { 0 };
            let start = (y * self.tile_size + valid_width) as usize;
            let end = ((y + 1) * self.tile_size) as usize;
            pixels[start..end].fill([0.0; 4]);
        }
    }

    fn insert(&mut self, key: MipKey, pixels: Option<Vec<Pixel>>) {
        let bytes = ENTRY_BYTES
            + pixels
                .as_ref()
                .map_or(0, |pixels| pixels.capacity() * size_of::<Pixel>());
        self.remove(key);
        while self.stats.resident_bytes > self.cache_budget - bytes {
            let oldest = *self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .expect("nonempty cache requiring eviction")
                .0;
            self.remove(oldest);
            self.stats.evictions += 1;
        }
        self.clock += 1;
        self.entries.insert(
            key,
            Entry {
                pixels,
                bytes,
                touched: self.clock,
            },
        );
        self.stats.resident_bytes += bytes;
        self.stats.peak_resident_bytes = self
            .stats
            .peak_resident_bytes
            .max(self.stats.resident_bytes + self.stats.reserved_scratch_bytes);
        self.stats.entries = self.entries.len();
    }

    fn remove(&mut self, key: MipKey) -> bool {
        if let Some(entry) = self.entries.remove(&key) {
            self.stats.resident_bytes -= entry.bytes;
            self.stats.entries = self.entries.len();
            true
        } else {
            false
        }
    }

    /// Invalidate only this base tile and its one ancestor at each mip level.
    pub fn invalidate_base_tile(&mut self, layer: u64, x: u32, y: u32) -> usize {
        let mut count = 0;
        for level in 0..=self.max_level {
            let key = MipKey {
                layer,
                level,
                x: (u64::from(x) >> level) as u32,
                y: (u64::from(y) >> level) as u32,
            };
            count += usize::from(self.remove(key));
        }
        self.stats.invalidations += count as u64;
        count
    }

    /// Dirty base-pixel rectangle, using an exclusive maximum. Regeneration is
    /// incremental at mip-tile granularity and does not visit unrelated tiles.
    pub fn invalidate_region(&mut self, layer: u64, min: [u32; 2], max: [u32; 2]) -> usize {
        if min[0] >= max[0] || min[1] >= max[1] {
            return 0;
        }
        let old_count = self.entries.len();
        self.entries.retain(|key, entry| {
            let span = u64::from(self.tile_size) << key.level;
            let x = u64::from(key.x) * span;
            let y = u64::from(key.y) * span;
            let overlaps = key.layer == layer
                && x < u64::from(max[0])
                && x + span > u64::from(min[0])
                && y < u64::from(max[1])
                && y + span > u64::from(min[1]);
            if overlaps {
                self.stats.resident_bytes -= entry.bytes;
            }
            !overlaps
        });
        let count = old_count - self.entries.len();
        self.stats.invalidations += count as u64;
        self.stats.entries = self.entries.len();
        count
    }

    pub fn invalidate_layer(&mut self, layer: u64) {
        self.invalidate_region(layer, [0, 0], self.canvas);
    }

    pub fn stats(&self) -> PyramidStats {
        self.stats
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats.resident_bytes = 0;
        self.stats.entries = 0;
    }
}

fn nonempty(pixels: Vec<Pixel>) -> Option<Vec<Pixel>> {
    pixels
        .iter()
        .any(|pixel| *pixel != [0.0; 4])
        .then_some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(layer: u64, level: u8, x: u32, y: u32) -> MipKey {
        MipKey { layer, level, x, y }
    }

    #[test]
    fn mip_pyramid_averages_premultiplied_pixels_with_missing_transparent_tiles() {
        let mut pyramid = MipPyramid::new([4, 4], 2, 8192).unwrap();
        let mut source = |k: MipKey| {
            Ok(if k == key(1, 0, 0, 0) {
                Some(vec![[0.5, 0.0, 0.0, 0.5]; 4])
            } else {
                None
            })
        };
        let image = pyramid.tile(key(1, 1, 0, 0), &mut source).unwrap().unwrap();
        assert_eq!(
            image,
            vec![[0.5, 0.0, 0.0, 0.5], [0.0; 4], [0.0; 4], [0.0; 4]]
        );
        let smallest = pyramid.tile(key(1, 2, 0, 0), &mut source).unwrap().unwrap();
        assert_eq!(smallest[0], [0.125, 0.0, 0.0, 0.125]);
        assert!(smallest[1..].iter().all(|p| *p == [0.0; 4]));
    }

    #[test]
    fn odd_canvas_boundary_and_single_pixel_tiles_match_area_reference() {
        let mut pyramid = MipPyramid::new([3, 3], 2, 8192).unwrap();
        let mut source = |k: MipKey| Ok((k.level == 0).then(|| vec![[1.0; 4]; 4]));
        let image = pyramid.tile(key(0, 1, 0, 0), &mut source).unwrap().unwrap();
        assert_eq!(image, vec![[1.0; 4], [0.5; 4], [0.5; 4], [0.25; 4]]);
        let mut single = MipPyramid::new([2, 2], 1, 4096).unwrap();
        let image = single
            .tile(key(0, 1, 0, 0), &mut |k| {
                Ok((k.level == 0).then(|| vec![[1.0; 4]]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(image, vec![[1.0; 4]]);
    }

    #[test]
    fn dirty_region_updates_ancestor_only_and_preserves_unrelated_cached_layers() {
        let mut pyramid = MipPyramid::new([8, 8], 2, 32768).unwrap();
        let mut value = 0.25;
        let mut reads = 0;
        for layer in [1, 2] {
            pyramid
                .tile(key(layer, 1, 0, 0), &mut |k| {
                    reads += 1;
                    Ok((k.level == 0).then(|| vec![[value; 4]; 4]))
                })
                .unwrap();
        }
        let prior_reads = reads;
        assert!(pyramid.invalidate_region(1, [0, 0], [1, 1]) > 0);
        value = 0.5;
        let unchanged = pyramid
            .tile(key(2, 1, 0, 0), &mut |_| {
                reads += 1;
                Ok(None)
            })
            .unwrap()
            .unwrap();
        assert_eq!(reads, prior_reads);
        assert_eq!(unchanged[0], [0.25; 4]);
        let changed = pyramid
            .tile(key(1, 1, 0, 0), &mut |k| {
                reads += 1;
                Ok((k.level == 0).then(|| vec![[value; 4]; 4]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(changed[0], [0.5; 4]);
        assert_eq!(changed[1], [0.25; 4]);
        assert_eq!(reads - prior_reads, 2); // parent plus one invalidated base tile
        assert!(pyramid.invalidate_base_tile(1, 0, 0) > 0);
        pyramid.invalidate_layer(2);
        assert_eq!(pyramid.invalidate_region(1, [1, 1], [0, 0]), 0);
    }

    #[test]
    fn persisted_mip_avoids_full_resolution_reads_and_streams_generated_tiles() {
        let mut pyramid = MipPyramid::new([8192, 8192], 2, 32768).unwrap();
        let mut reads = Vec::new();
        let image = pyramid
            .tile(key(3, 5, 0, 0), &mut |k| {
                reads.push(k);
                Ok(Some(vec![[0.3; 4]; 4]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(reads, vec![key(3, 5, 0, 0)]);
        assert_eq!(image[0], [0.3; 4]);
        pyramid.clear();
        let mut saved = Vec::new();
        pyramid
            .tile_with_persistence(
                key(1, 2, 0, 0),
                &mut |k| Ok((k.level == 0).then(|| vec![[1.0; 4]; 4])),
                &mut |k, pixels| {
                    saved.push((k, pixels.map(|p| p.len())));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(saved.len(), 5);
        assert!(saved.iter().all(|(_, length)| *length == Some(4)));
    }

    #[test]
    fn sparse_mip_cache_residency_ceiling_held_under_adversarial_layer_switches() {
        let mut pyramid = MipPyramid::new([16, 16], 2, 3000).unwrap();
        for layer in 0..200 {
            pyramid
                .tile(key(layer, 2, 0, 0), &mut |k| {
                    Ok((k.level == 0 && (k.x + k.y) % 3 == 0).then(|| vec![[0.5; 4]; 4]))
                })
                .unwrap();
            assert!(pyramid.stats().peak_resident_bytes <= pyramid.budget_bytes());
            assert!(
                pyramid.stats().resident_bytes + pyramid.stats().reserved_scratch_bytes
                    <= pyramid.budget_bytes()
            );
        }
        assert!(pyramid.stats().evictions > 0);
        assert_eq!(pyramid.level_for_zoom(0.125).unwrap(), 3);
        assert_eq!(pyramid.level_for_zoom(2.0).unwrap(), 0);
        assert!(pyramid.level_for_zoom(f64::NAN).is_err());
    }

    #[test]
    fn invalid_geometry_source_lengths_pixels_and_budgets_fail_before_use() {
        assert!(matches!(
            MipPyramid::new([0, 1], 2, 5000),
            Err(PyramidError::InvalidGeometry)
        ));
        assert!(matches!(
            MipPyramid::new([1, 1], 0, 5000),
            Err(PyramidError::InvalidGeometry)
        ));
        assert!(matches!(
            MipPyramid::new([8, 8], 2, 1),
            Err(PyramidError::BudgetTooSmall)
        ));
        assert!(matches!(
            MipPyramid::new([8, 8], 2, 500),
            Err(PyramidError::BudgetTooSmall)
        ));
        let mut pyramid = MipPyramid::new([8, 8], 2, 8192).unwrap();
        assert_eq!(
            pyramid.tile(key(0, 8, 0, 0), &mut |_| Ok(None)),
            Err(PyramidError::InvalidLevel)
        );
        assert_eq!(
            pyramid.tile(key(0, 0, 99, 99), &mut |_| panic!("outside canvas")),
            Ok(None)
        );
        assert_eq!(
            pyramid.tile(key(0, 0, 0, 0), &mut |_| Ok(Some(vec![[0.0; 4]; 1]))),
            Err(PyramidError::InvalidTileLength)
        );
        assert_eq!(
            pyramid.tile(key(0, 0, 0, 0), &mut |_| Ok(Some(vec![[f32::NAN; 4]; 4]))),
            Err(PyramidError::InvalidPixels)
        );
        assert_eq!(
            pyramid.tile(key(0, 0, 0, 0), &mut |_| Err("disk read".into())),
            Err(PyramidError::Source("disk read".into()))
        );
        let mut over = Vec::with_capacity(100);
        over.resize(4, [0.0; 4]);
        assert_eq!(
            pyramid.tile(key(0, 0, 0, 0), &mut |_| Ok(Some(std::mem::take(
                &mut over
            )))),
            Err(PyramidError::SourceCapacityExceedsScratch)
        );
    }
}
