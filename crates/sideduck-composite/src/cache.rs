//! Semantic stack partitioning and bounded caches for derived tile images.
//!
//! Only normal, unclipped layers outside a backdrop-dependent group can be
//! independently flattened. All other layers retain their ordered pass.

use crate::blend::{BlendMode, Pixel};
use std::collections::BTreeMap;
use std::fmt;
use std::mem::size_of;

#[derive(Clone, Debug, PartialEq)]
pub struct LayerDescriptor {
    pub id: u64,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub visible: bool,
    pub clipping: bool,
    pub group_backdrop_dependent: bool,
    pub revision: u64,
}

impl LayerDescriptor {
    pub fn independent(&self) -> bool {
        self.blend_mode == BlendMode::Normal && !self.clipping && !self.group_backdrop_dependent
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Flattened(Vec<usize>),
    Pass(usize),
    /// An unclipped base and its consecutive clipping layers. Execute in an
    /// isolated source-atop group, then apply the base blend and opacity once.
    ClippingGroup(Vec<usize>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositePlan {
    /// Visible layers below the complete active group, in bottom-to-top order.
    pub below: Vec<usize>,
    pub active: usize,
    /// The selected layer's entire clipping chain, including invisible members.
    pub active_group: Vec<usize>,
    pub above: Vec<Run>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheError {
    InvalidActiveLayer,
    InvalidOpacity,
    MissingClipBase,
    InvalidGeometry,
    ImageExceedsBudget,
    SizeOverflow,
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidActiveLayer => "active layer index is outside the stack",
            Self::InvalidOpacity => "layer opacity must be finite and between zero and one",
            Self::MissingClipBase => "a clipping layer requires an unclipped base beneath it",
            Self::InvalidGeometry => "viewport geometry must be finite and positive",
            Self::ImageExceedsBudget => {
                "derived image and dependency metadata exceed the cache budget"
            }
            Self::SizeOverflow => "derived image byte count overflow",
        })
    }
}

impl std::error::Error for CacheError {}

pub fn compile_plan(
    layers: &[LayerDescriptor],
    active: usize,
) -> Result<CompositePlan, CacheError> {
    if active >= layers.len() {
        return Err(CacheError::InvalidActiveLayer);
    }
    if layers
        .iter()
        .any(|layer| !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity))
    {
        return Err(CacheError::InvalidOpacity);
    }
    if layers[0].clipping {
        return Err(CacheError::MissingClipBase);
    }
    let mut groups = Vec::new();
    let mut start = 0;
    while start < layers.len() {
        let mut end = start + 1;
        while end < layers.len() && layers[end].clipping {
            end += 1;
        }
        groups.push(start..end);
        start = end;
    }
    let active_range = groups
        .iter()
        .find(|range| range.contains(&active))
        .expect("validated active index belongs to a group")
        .clone();
    let below = groups
        .iter()
        .take_while(|range| range.end <= active_range.start)
        .filter(|range| layers[range.start].visible)
        .flat_map(|range| range.clone().filter(|&index| layers[index].visible))
        .collect();
    let mut above = Vec::new();
    let mut independent = Vec::new();
    for range in groups
        .iter()
        .filter(|range| range.start >= active_range.end)
    {
        let index = range.start;
        let layer = &layers[index];
        if !layer.visible {
            continue;
        }
        if range.len() == 1 && layer.independent() {
            independent.push(index);
        } else {
            if !independent.is_empty() {
                above.push(Run::Flattened(std::mem::take(&mut independent)));
            }
            if range.len() > 1 {
                above.push(Run::ClippingGroup(
                    range
                        .clone()
                        .filter(|&index| layers[index].visible)
                        .collect(),
                ));
            } else {
                above.push(Run::Pass(index));
            }
        }
    }
    if !independent.is_empty() {
        above.push(Run::Flattened(independent));
    }
    Ok(CompositePlan {
        below,
        active,
        active_group: active_range.collect(),
        above,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheInvalidation {
    pub backdrop: bool,
    pub above: bool,
}

impl CacheInvalidation {
    /// Edits anywhere inside the live clipping group leave its two independent
    /// partitions valid; their shared base coverage is recomputed live.
    pub fn edited_for_plan(edited: usize, plan: &CompositePlan) -> Self {
        Self {
            backdrop: edited < plan.active_group.first().copied().unwrap_or(plan.active),
            above: edited > plan.active_group.last().copied().unwrap_or(plan.active),
        }
    }

    pub fn switched_plans(previous: &CompositePlan, next: &CompositePlan) -> Self {
        Self {
            backdrop: previous.below != next.below,
            above: previous.above != next.above,
        }
    }

    pub fn edited(edited: usize, active: usize) -> Self {
        Self {
            backdrop: edited < active,
            above: edited > active,
        }
    }

    /// Moving the partition changes both dependencies in general. Existing
    /// exact-key segment caches can satisfy either side without recomputation;
    /// declaring one side unchanged would be incorrect for arbitrary switches.
    pub fn switched(previous: usize, next: usize) -> Self {
        Self {
            backdrop: previous != next,
            above: previous != next,
        }
    }

    pub fn structure_changed() -> Self {
        Self {
            backdrop: true,
            above: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileAddress {
    pub level: u8,
    pub x: u32,
    pub y: u32,
}

/// An exact dependency key, avoiding correctness dependence on hash collisions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LayerStamp {
    pub id: u64,
    pub revision: u64,
    pub blend_mode: BlendMode,
    pub opacity_bits: u32,
    pub visible: bool,
    pub clipping: bool,
    pub group_backdrop_dependent: bool,
}

impl From<&LayerDescriptor> for LayerStamp {
    fn from(layer: &LayerDescriptor) -> Self {
        Self {
            id: layer.id,
            revision: layer.revision,
            blend_mode: layer.blend_mode,
            opacity_bits: layer.opacity.to_bits(),
            visible: layer.visible,
            clipping: layer.clipping,
            group_backdrop_dependent: layer.group_backdrop_dependent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    pub tile: TileAddress,
    /// Ordered bottom-to-top dependencies, including all mask/group revisions.
    pub dependencies: Vec<LayerStamp>,
}

impl CacheKey {
    pub fn new<'a>(
        tile: TileAddress,
        layers: impl IntoIterator<Item = &'a LayerDescriptor>,
    ) -> Self {
        Self {
            tile,
            dependencies: layers.into_iter().map(LayerStamp::from).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCacheStats {
    /// Allocated pixel/dependency capacities plus conservative per-entry metadata.
    pub resident_bytes: usize,
    pub peak_resident_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

struct CachedImage {
    pixels: Vec<Pixel>,
    bytes: usize,
    touched: u128,
}

pub struct ImageCache {
    budget: usize,
    entries: BTreeMap<CacheKey, CachedImage>,
    clock: u128,
    stats: ImageCacheStats,
}

impl ImageCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget: budget_bytes,
            entries: BTreeMap::new(),
            clock: 0,
            stats: ImageCacheStats::default(),
        }
    }

    pub fn get(&mut self, key: &CacheKey) -> Option<&[Pixel]> {
        self.clock += 1;
        if let Some(image) = self.entries.get_mut(key) {
            image.touched = self.clock;
            self.stats.hits += 1;
            Some(&image.pixels)
        } else {
            self.stats.misses += 1;
            None
        }
    }

    pub fn insert(&mut self, key: CacheKey, pixels: Vec<Pixel>) -> Result<(), CacheError> {
        let bytes = pixels
            .capacity()
            .checked_mul(size_of::<Pixel>())
            .and_then(|n| {
                key.dependencies
                    .capacity()
                    .checked_mul(size_of::<LayerStamp>())
                    .and_then(|dependencies| n.checked_add(dependencies))
            })
            .and_then(|n| n.checked_add(size_of::<CacheKey>() + size_of::<CachedImage>() + 128))
            .ok_or(CacheError::SizeOverflow)?;
        if bytes > self.budget {
            return Err(CacheError::ImageExceedsBudget);
        }
        self.remove(&key);
        while self.stats.resident_bytes > self.budget - bytes {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, image)| image.touched)
                .map(|(key, _)| key.clone())
                .expect("nonempty cache while eviction is required");
            self.remove(&oldest);
            self.stats.evictions += 1;
        }
        self.clock += 1;
        self.entries.insert(
            key,
            CachedImage {
                pixels,
                bytes,
                touched: self.clock,
            },
        );
        self.stats.resident_bytes += bytes;
        self.stats.peak_resident_bytes = self
            .stats
            .peak_resident_bytes
            .max(self.stats.resident_bytes);
        self.stats.entries = self.entries.len();
        Ok(())
    }

    pub fn remove(&mut self, key: &CacheKey) -> bool {
        if let Some(image) = self.entries.remove(key) {
            self.stats.resident_bytes -= image.bytes;
            self.stats.entries = self.entries.len();
            true
        } else {
            false
        }
    }

    pub fn invalidate_layer(&mut self, id: u64) {
        self.entries.retain(|key, image| {
            let keep = !key.dependencies.iter().any(|layer| layer.id == id);
            if !keep {
                self.stats.resident_bytes -= image.bytes;
            }
            keep
        });
        self.stats.entries = self.entries.len();
    }

    pub fn stats(&self) -> ImageCacheStats {
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Screen position of the canvas origin, in physical pixels.
    pub pan: [f64; 2],
    pub zoom: f64,
    pub size: [u32; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileRegion {
    pub min: [u32; 2],
    /// Exclusive upper bounds.
    pub max: [u32; 2],
}

impl TileRegion {
    pub fn iter(&self) -> impl Iterator<Item = [u32; 2]> + '_ {
        (self.min[1]..self.max[1]).flat_map(|y| (self.min[0]..self.max[0]).map(move |x| [x, y]))
    }

    pub fn count(&self) -> u64 {
        u64::from(self.max[0] - self.min[0]) * u64::from(self.max[1] - self.min[1])
    }
}

impl Viewport {
    pub fn tile_region(
        self,
        canvas: [u32; 2],
        tile_size: u32,
        level: u8,
    ) -> Result<TileRegion, CacheError> {
        if canvas.contains(&0)
            || tile_size == 0
            || !self.zoom.is_finite()
            || self.zoom <= 0.0
            || self.pan.iter().any(|p| !p.is_finite())
            || level > 31
        {
            return Err(CacheError::InvalidGeometry);
        }
        let divisor = f64::from(tile_size) * f64::from(1_u32 << level);
        let mut region = TileRegion::default();
        for (axis, &canvas_size) in canvas.iter().enumerate() {
            let start = (-self.pan[axis] / self.zoom).clamp(0.0, f64::from(canvas_size));
            let end = ((f64::from(self.size[axis]) - self.pan[axis]) / self.zoom)
                .clamp(0.0, f64::from(canvas_size));
            if start >= end {
                return Ok(TileRegion::default());
            }
            region.min[axis] = (start / divisor).floor() as u32;
            region.max[axis] = (end / divisor).ceil() as u32;
        }
        Ok(region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::{blend, composite_stack};

    fn layer(id: u64, mode: BlendMode) -> LayerDescriptor {
        LayerDescriptor {
            id,
            blend_mode: mode,
            opacity: 0.7,
            visible: true,
            clipping: false,
            group_backdrop_dependent: false,
            revision: 0,
        }
    }

    fn key(id: u64) -> CacheKey {
        CacheKey::new(
            TileAddress {
                level: 0,
                x: 0,
                y: 0,
            },
            [&layer(id, BlendMode::Normal)],
        )
    }

    #[test]
    fn backdrop_dependent_blend_modes_clipping_and_groups_never_preflatten() {
        let mut layers = vec![layer(0, BlendMode::Normal); 8];
        layers[3].blend_mode = BlendMode::Multiply;
        layers[5].clipping = true;
        layers[6].group_backdrop_dependent = true;
        assert_eq!(
            compile_plan(&layers, 0).unwrap().above,
            vec![
                Run::Flattened(vec![1, 2]),
                Run::Pass(3),
                Run::ClippingGroup(vec![4, 5]),
                Run::Pass(6),
                Run::Flattened(vec![7])
            ]
        );
        layers[2].visible = false;
        assert_eq!(compile_plan(&layers, 3).unwrap().below, vec![0, 1]);
        assert_eq!(compile_plan(&[], 0), Err(CacheError::InvalidActiveLayer));
        layers[0].clipping = true;
        assert_eq!(compile_plan(&layers, 0), Err(CacheError::MissingClipBase));
        layers[0].clipping = false;
        layers[0].opacity = f32::NAN;
        assert_eq!(compile_plan(&layers, 0), Err(CacheError::InvalidOpacity));
    }

    fn reference_group(
        backdrop: Pixel,
        indices: &[usize],
        layers: &[LayerDescriptor],
        pixels: &[Pixel],
    ) -> Pixel {
        let base = indices[0];
        if !layers[base].visible {
            return backdrop;
        }
        let alpha = pixels[base][3];
        let mut isolated = pixels[base];
        if alpha > 0.0 {
            for &index in &indices[1..] {
                if !layers[index].visible {
                    continue;
                }
                // Source-atop: blend straight colors inside the base coverage.
                // Its opacity applies only when the complete group is composited.
                let opaque = [
                    isolated[0] / alpha,
                    isolated[1] / alpha,
                    isolated[2] / alpha,
                    1.0,
                ];
                let mixed = blend(
                    opaque,
                    pixels[index],
                    layers[index].blend_mode,
                    layers[index].opacity,
                );
                isolated = [mixed[0] * alpha, mixed[1] * alpha, mixed[2] * alpha, alpha];
            }
        }
        blend(
            backdrop,
            isolated,
            layers[base].blend_mode,
            layers[base].opacity,
        )
    }

    fn reference_stack(indices: &[usize], layers: &[LayerDescriptor], pixels: &[Pixel]) -> Pixel {
        let mut output = [0.0; 4];
        let mut start = 0;
        while start < indices.len() {
            let mut end = start + 1;
            while end < indices.len() && layers[indices[end]].clipping {
                end += 1;
            }
            output = reference_group(output, &indices[start..end], layers, pixels);
            start = end;
        }
        output
    }

    #[test]
    fn clipping_group_preserves_base_coverage_and_active_group_cache_boundaries() {
        let mut layers = (0..6)
            .map(|id| layer(id, BlendMode::Normal))
            .collect::<Vec<_>>();
        layers[2].clipping = true;
        layers[3].clipping = true;
        layers[2].opacity = 1.0;
        layers[3].opacity = 1.0;
        layers[1].opacity = 0.5;
        let pixels = [
            [0.0; 4],
            [0.5, 0.0, 0.0, 0.5],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [0.0; 4],
            [0.0; 4],
        ];
        assert_eq!(
            reference_group([0.0; 4], &[1, 2, 3], &layers, &pixels),
            [0.0, 0.0, 0.25, 0.25]
        );
        layers[1].opacity = 1.0;
        assert_eq!(reference_group([0.0; 4], &[1, 2], &layers, &pixels)[3], 0.5);
        let base_plan = compile_plan(&layers, 1).unwrap();
        let clip_plan = compile_plan(&layers, 3).unwrap();
        assert_eq!(clip_plan.active_group, vec![1, 2, 3]);
        assert_eq!(clip_plan.below, vec![0]);
        assert_eq!(clip_plan.above, vec![Run::Flattened(vec![4, 5])]);
        assert_eq!(
            CacheInvalidation::switched_plans(&base_plan, &clip_plan),
            CacheInvalidation::default()
        );
        for edited in 1..=3 {
            assert_eq!(
                CacheInvalidation::edited_for_plan(edited, &clip_plan),
                CacheInvalidation::default()
            );
        }
        assert!(CacheInvalidation::edited_for_plan(0, &clip_plan).backdrop);
        assert!(CacheInvalidation::edited_for_plan(4, &clip_plan).above);
        layers[1].visible = false;
        assert_eq!(compile_plan(&layers, 5).unwrap().below, vec![0, 4]);
        assert_eq!(
            compile_plan(&layers, 0).unwrap().above,
            vec![Run::Flattened(vec![4, 5])]
        );
        assert_eq!(
            reference_group([0.0; 4], &[1, 2, 3], &layers, &pixels),
            [0.0; 4]
        );
    }

    #[test]
    fn randomized_50_layer_stack_run_flattening_matches_cpu_reference_per_pixel() {
        let mut state = 0x4a8f_4ec3_u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 32) as u32) as f32 / u32::MAX as f32
        };
        for trial in 0..32 {
            let mut layers = Vec::new();
            let mut pixels = Vec::new();
            for index in 0..50 {
                let mode = if index % 4 != 0 {
                    BlendMode::Normal
                } else {
                    BlendMode::ALL[(trial + index) % BlendMode::ALL.len()]
                };
                let mut descriptor = layer(index as u64, mode);
                descriptor.opacity = next();
                descriptor.clipping = index > 0 && index % 9 == 0;
                descriptor.group_backdrop_dependent = index % 13 == 0;
                descriptor.visible = index % 17 != 0;
                let alpha = next();
                let pixel = [next() * alpha, next() * alpha, next() * alpha, alpha];
                layers.push(descriptor);
                pixels.push(pixel);
            }
            for active in [0, 7, 8, 9, 18, 25, 49] {
                let reference = reference_stack(&(0..50).collect::<Vec<_>>(), &layers, &pixels);
                let plan = compile_plan(&layers, active).unwrap();
                let mut result = reference_stack(&plan.below, &layers, &pixels);
                result = reference_group(result, &plan.active_group, &layers, &pixels);
                for run in plan.above {
                    match run {
                        Run::Pass(i) => {
                            result =
                                blend(result, pixels[i], layers[i].blend_mode, layers[i].opacity)
                        }
                        Run::Flattened(indices) => {
                            let flat = composite_stack(
                                indices
                                    .into_iter()
                                    .map(|i| (pixels[i], BlendMode::Normal, layers[i].opacity)),
                            );
                            result = blend(result, flat, BlendMode::Normal, 1.0);
                        }
                        Run::ClippingGroup(indices) => {
                            result = reference_group(result, &indices, &layers, &pixels);
                        }
                    }
                }
                for channel in 0..4 {
                    assert!(
                        (result[channel] - reference[channel]).abs() <= 2e-6,
                        "trial {trial} active {active}: {result:?} != {reference:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn editing_layer_invalidates_only_affected_side_and_switching_rekeys_both() {
        assert_eq!(
            CacheInvalidation::edited(2, 4),
            CacheInvalidation {
                backdrop: true,
                above: false
            }
        );
        assert_eq!(
            CacheInvalidation::edited(7, 4),
            CacheInvalidation {
                backdrop: false,
                above: true
            }
        );
        assert_eq!(
            CacheInvalidation::edited(4, 4),
            CacheInvalidation::default()
        );
        assert_eq!(
            CacheInvalidation::switched(4, 4),
            CacheInvalidation::default()
        );
        assert_eq!(
            CacheInvalidation::switched(4, 7),
            CacheInvalidation::structure_changed()
        );
    }

    #[test]
    fn cache_residency_ceiling_lru_and_exact_revision_invalidation() {
        let mut cache = ImageCache::new(900);
        cache.insert(key(1), vec![[1.0; 4]; 4]).unwrap();
        cache.insert(key(2), vec![[0.5; 4]; 4]).unwrap();
        assert!(cache.get(&key(1)).is_some());
        cache.insert(key(3), vec![[0.25; 4]; 4]).unwrap();
        assert!(cache.get(&key(2)).is_none());
        assert!(cache.get(&key(1)).is_some());
        let mut revised = key(1);
        revised.dependencies[0].revision += 1;
        assert!(cache.get(&revised).is_none());
        cache.invalidate_layer(1);
        assert!(cache.get(&key(1)).is_none());
        assert!(cache.get(&key(3)).is_some());
        assert_eq!(
            cache.insert(key(5), vec![[0.0; 4]; 100]),
            Err(CacheError::ImageExceedsBudget)
        );
        assert!(cache.stats().peak_resident_bytes <= cache.budget_bytes());
        assert!(cache.stats().evictions > 0);
        cache.clear();
        assert_eq!(cache.stats().resident_bytes, 0);
    }

    #[test]
    fn viewport_tiles_intersect_negative_pan_zoom_and_exact_boundaries() {
        let viewport = Viewport {
            pan: [-256.0, -128.0],
            zoom: 1.0,
            size: [512, 256],
        };
        let region = viewport.tile_region([8192, 8192], 256, 0).unwrap();
        assert_eq!(
            region,
            TileRegion {
                min: [1, 0],
                max: [3, 2]
            }
        );
        assert_eq!(
            region.iter().collect::<Vec<_>>(),
            vec![[1, 0], [2, 0], [1, 1], [2, 1]]
        );
        assert_eq!(region.count(), 4);
        assert_eq!(
            Viewport {
                pan: [0.0; 2],
                zoom: 0.5,
                size: [256; 2]
            }
            .tile_region([1024; 2], 256, 0)
            .unwrap()
            .max,
            [2; 2]
        );
        assert_eq!(
            Viewport {
                pan: [0.0; 2],
                zoom: 1.0,
                size: [256; 2]
            }
            .tile_region([1024; 2], 256, 0)
            .unwrap()
            .max,
            [1; 2]
        );
        assert_eq!(
            Viewport {
                pan: [500.0; 2],
                zoom: 1.0,
                size: [100; 2]
            }
            .tile_region([1024; 2], 256, 0)
            .unwrap()
            .count(),
            0
        );
        assert_eq!(
            Viewport {
                zoom: 0.0,
                ..viewport
            }
            .tile_region([1024; 2], 256, 0),
            Err(CacheError::InvalidGeometry)
        );
        assert_eq!(viewport.tile_region([8192; 2], 256, 2).unwrap().max, [1, 1]);
    }
}
