use super::*;
use crate::blend::blend;
use std::time::{SystemTime, UNIX_EPOCH};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "raster-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct MemorySource {
    edge: u32,
    tiles: BTreeMap<(u64, TileAddress), RawTile>,
    reads: u64,
}
impl MemorySource {
    fn new(edge: u32) -> Self {
        Self {
            edge,
            tiles: BTreeMap::new(),
            reads: 0,
        }
    }
    fn put(&mut self, layer: u64, address: TileAddress, pixels: &[Pixel]) {
        let mut bytes = vec![0; pixels.len() * 8];
        for (out, pixel) in bytes.chunks_exact_mut(8).zip(pixels) {
            write_half(out, *pixel);
        }
        self.tiles.insert(
            (layer, address),
            RawTile {
                format: PixelFormat::Rgba16Float,
                bytes,
            },
        );
    }
    fn pixel(&self, layer: u64, level: u8, x: u32, y: u32) -> Pixel {
        let address = TileAddress {
            level,
            x: x / self.edge,
            y: y / self.edge,
        };
        self.tiles.get(&(layer, address)).map_or([0.0; 4], |tile| {
            decode(tile, ((y % self.edge) * self.edge + x % self.edge) as usize)
        })
    }
}
impl RasterSource for MemorySource {
    fn tile_size(&self) -> u32 {
        self.edge
    }
    fn read_tile(&mut self, layer: u64, address: TileAddress) -> Result<Option<RawTile>> {
        self.reads += 1;
        Ok(self.tiles.get(&(layer, address)).cloned())
    }
}
fn layer(id: u64) -> RasterLayer {
    RasterLayer::from(LayerDescriptor {
        id,
        blend_mode: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
        clipping: false,
        group_backdrop_dependent: false,
        revision: 0,
    })
}
fn renderer(dir: &Directory, edge: u32) -> RasterRenderer {
    RasterRenderer::create(dir.0.join("cache"), edge, 1024 * 1024, 4 * 1024 * 1024).unwrap()
}
fn render(gpu: &GpuCompositor, prepared: &mut PreparedViewport) -> Vec<Pixel> {
    let mut encoder = gpu.encoder();
    prepared.encode(gpu, &mut encoder).unwrap();
    gpu.submit(encoder);
    gpu.readback(prepared.output()).unwrap()
}
fn expected(
    source: &MemorySource,
    layers: &[RasterLayer],
    level: u8,
    rect: RenderRect,
) -> Vec<Pixel> {
    let mut output = Vec::new();
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let mut backdrop = [0.0; 4];
            let effective = |layer: &RasterLayer| {
                let mut color = source.pixel(layer.descriptor.id, level, x, y);
                let amount = if layer.descriptor.visible {
                    layer.descriptor.opacity
                        * layer
                            .mask_layer
                            .map_or(1.0, |id| source.pixel(id, level, x, y)[3])
                } else {
                    0.0
                };
                color.iter_mut().for_each(|v| *v *= amount);
                color
            };
            let mut index = 0;
            while index < layers.len() {
                if is_clipped(&layers[index]) {
                    index += 1;
                    continue;
                }
                let base = index;
                let mut group = effective(&layers[base]);
                index += 1;
                while index < layers.len() && is_clipped(&layers[index]) {
                    let alpha = group[3];
                    if alpha > 0.0 {
                        let opaque = [group[0] / alpha, group[1] / alpha, group[2] / alpha, 1.0];
                        let mixed = blend(
                            opaque,
                            effective(&layers[index]),
                            layers[index].descriptor.blend_mode,
                            1.0,
                        );
                        group = [mixed[0] * alpha, mixed[1] * alpha, mixed[2] * alpha, alpha];
                    }
                    index += 1;
                }
                backdrop = blend(backdrop, group, layers[base].descriptor.blend_mode, 1.0);
            }
            output.push(backdrop);
        }
    }
    output
}
fn parity(actual: &[Pixel], expected: &[Pixel], tolerance: f32) {
    assert_eq!(actual.len(), expected.len());
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        for c in 0..4 {
            assert!(
                (a[c] - b[c]).abs() <= tolerance,
                "pixel {index} channel {c}: {a:?} != {b:?}"
            );
        }
    }
}

#[test]
fn tiled_normal_segment_tree_matches_reference_randomized_50_layer_stack() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(32 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 4);
    let mut source = MemorySource::new(4);
    let layers: Vec<_> = (0..50).map(layer).collect();
    let mut state = 197_u64;
    for id in 0..50 {
        for y in 0..2 {
            for x in 0..2 {
                if (id + x as u64 + y as u64).is_multiple_of(3) {
                    continue;
                }
                let mut pixels = Vec::new();
                for _ in 0..16 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let a = 0.1 + (state & 255) as f32 / 512.0;
                    pixels.push([
                        a * ((state >> 8) & 255) as f32 / 255.0,
                        a * ((state >> 16) & 255) as f32 / 255.0,
                        a * ((state >> 24) & 255) as f32 / 255.0,
                        a,
                    ]);
                }
                source.put(id, TileAddress { level: 0, x, y }, &pixels);
            }
        }
    }
    let rect = RenderRect {
        x: 1,
        y: 1,
        width: 6,
        height: 5,
    };
    renderer
        .warm_viewport(&gpu, &mut source, &layers, 0, rect)
        .unwrap();
    let reads = source.reads;
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 0, 0, rect)
        .unwrap();
    for active in [24, 49, 0] {
        prepared = renderer
            .switch_active(&gpu, &mut source, &layers, active, prepared)
            .unwrap();
        assert!(!prepared.requires_reprepare_on_edit());
        assert!(prepared.active_image().is_some());
        parity(
            &render(&gpu, &mut prepared),
            &expected(&source, &layers, 0, rect),
            0.0045,
        );
    }
    assert_eq!(
        source.reads, reads,
        "warm range queries must not reread raw layer tiles"
    );
    assert!(renderer.stats().peak_gpu_cache_bytes <= 4 * 1024 * 1024);
    assert!(gpu.peak_allocated_bytes() <= gpu.budget_bytes());
}

#[test]
fn normal_500_layer_half_float_cache_stays_within_stated_reference_tolerance() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let layers: Vec<_> = (0..500).map(layer).collect();
    for id in 0..500 {
        let a = (1 + id % 7) as f32 / 255.0;
        source.put(
            id,
            TileAddress {
                level: 0,
                x: 0,
                y: 0,
            },
            &[[a * (id % 31) as f32 / 31.0, a * 0.4, a * 0.7, a]],
        );
    }
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    renderer
        .warm_viewport(&gpu, &mut source, &layers, 0, rect)
        .unwrap();
    for active in [0, 127, 249, 499] {
        let mut prepared = renderer
            .prepare_viewport(&gpu, &mut source, &layers, active, 0, rect)
            .unwrap();
        parity(
            &render(&gpu, &mut prepared),
            &expected(&source, &layers, 0, rect),
            0.0045,
        );
    }
}

#[test]
fn ordered_dependent_modes_masks_and_clipping_match_reference_per_pixel() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 2);
    let mut source = MemorySource::new(2);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    source.put(0, address, &[[0.1, 0.2, 0.3, 0.5]; 4]);
    source.put(1, address, &[[0.4, 0.1, 0.05, 0.6]; 4]);
    source.put(2, address, &[[0.05, 0.2, 0.1, 0.4]; 4]);
    source.put(
        99,
        address,
        &[
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.25],
            [0.0, 0.0, 0.0, 0.5],
            [0.0, 0.0, 0.0, 1.0],
        ],
    );
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    for mode in BlendMode::ALL {
        let mut layers = vec![layer(0), layer(1), layer(2)];
        layers[0].descriptor.opacity = 0.7;
        layers[1].descriptor.blend_mode = mode;
        layers[1].mask_layer = Some(99);
        layers[1].descriptor.clipping = true;
        layers[2].descriptor.group_backdrop_dependent = true;
        layers[2].descriptor.blend_mode = BlendMode::Screen;
        let mut prepared = renderer
            .prepare_viewport(&gpu, &mut source, &layers, 1, 0, rect)
            .unwrap();
        assert!(prepared.requires_reprepare_on_edit());
        assert!(prepared.active_image().is_none());
        parity(
            &render(&gpu, &mut prepared),
            &expected(&source, &layers, 0, rect),
            0.00002,
        );
    }
}

#[test]
fn dirty_tile_invalidates_ancestors_and_persisted_range_cache_reopens_without_source_reads() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 2);
    let mut source = MemorySource::new(2);
    let layers: Vec<_> = (0..8).map(layer).collect();
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    for id in 0..8 {
        source.put(id, address, &[[0.05, 0.1, 0.15, 0.25]; 4]);
    }
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    renderer
        .warm_viewport(&gpu, &mut source, &layers, 0, rect)
        .unwrap();
    renderer.commit().unwrap();
    drop(renderer);
    let mut renderer =
        RasterRenderer::open(dir.0.join("cache"), 2, 1024 * 1024, 4 * 1024 * 1024).unwrap();
    let reads = source.reads;
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 4, 0, rect)
        .unwrap();
    parity(
        &render(&gpu, &mut prepared),
        &expected(&source, &layers, 0, rect),
        0.0045,
    );
    assert_eq!(source.reads, reads);
    source.put(3, address, &[[0.7, 0.0, 0.0, 0.8]; 4]);
    assert_eq!(renderer.invalidate_tile(3, address), 4);
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 4, 0, rect)
        .unwrap();
    parity(
        &render(&gpu, &mut prepared),
        &expected(&source, &layers, 0, rect),
        0.0045,
    );
    assert_eq!(source.reads, reads + 1);
}

#[test]
fn raster_store_adapter_reads_rgba8_and_persisted_mips_and_streams_canvas_under_budget() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut base = TileStore::create(
        dir.0.join("base"),
        StoreOptions {
            tile_size: 2,
            format: PixelFormat::Rgba8,
            cache_budget_bytes: 64,
        },
    )
    .unwrap();
    base.put(
        TileKey {
            layer: 0,
            x: 0,
            y: 0,
        },
        &[255, 0, 0, 255].repeat(4),
    )
    .unwrap();
    let mut mip = TileStore::create(
        dir.0.join("mip"),
        StoreOptions {
            tile_size: 2,
            format: PixelFormat::Rgba16Float,
            cache_budget_bytes: 64,
        },
    )
    .unwrap();
    let mut bytes = vec![0; 32];
    for pixel in bytes.chunks_exact_mut(8) {
        write_half(pixel, [0.25, 0.0, 0.0, 0.25]);
    }
    mip.put(
        TileKey {
            layer: 0,
            x: 0,
            y: 0,
        },
        &bytes,
    )
    .unwrap();
    let mut source = StoreSource::new(&mut base);
    source.add_level(1, &mut mip).unwrap();
    let mut renderer = renderer(&dir, 2);
    let layers = vec![layer(0)];
    let mut tiles = 0;
    renderer
        .render_canvas(&gpu, &mut source, &layers, 0, [3, 3], |rect, pixels| {
            tiles += 1;
            assert_eq!(pixels.len(), (rect.width * rect.height) as usize);
            Ok(())
        })
        .unwrap();
    assert_eq!(tiles, 4);
    let mut prepared = renderer
        .prepare_viewport(
            &gpu,
            &mut source,
            &layers,
            0,
            1,
            RenderRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap();
    assert_eq!(render(&gpu, &mut prepared), vec![[0.25, 0.0, 0.0, 0.25]; 4]);
    assert!(source
        .read_tile(
            0,
            TileAddress {
                level: 2,
                x: 0,
                y: 0
            }
        )
        .is_err());
}

#[test]
fn invalid_raster_geometry_source_and_persistent_metadata_fail_without_partial_success() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 2);
    let mut source = MemorySource::new(2);
    let layers = vec![layer(0)];
    assert!(renderer
        .prepare_viewport(
            &gpu,
            &mut source,
            &layers,
            0,
            0,
            RenderRect {
                x: 0,
                y: 0,
                width: 0,
                height: 1
            }
        )
        .is_err());
    assert!(renderer
        .prepare_viewport(
            &gpu,
            &mut source,
            &layers,
            4,
            0,
            RenderRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
        )
        .is_err());
    source.tiles.insert(
        (
            0,
            TileAddress {
                level: 0,
                x: 0,
                y: 0,
            },
        ),
        RawTile {
            format: PixelFormat::Rgba8,
            bytes: vec![1],
        },
    );
    assert!(renderer
        .warm_viewport(
            &gpu,
            &mut source,
            &layers,
            0,
            RenderRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2
            }
        )
        .is_err());
    source.tiles.clear();
    renderer
        .warm_viewport(
            &gpu,
            &mut source,
            &layers,
            0,
            RenderRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap();
    renderer.commit().unwrap();
    drop(renderer);
    let path = dir.0.join("cache/raster.meta");
    let mut bytes = fs::read(&path).unwrap();
    bytes[10] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(RasterRenderer::open(dir.0.join("cache"), 2, 1024 * 1024, 1024 * 1024).is_err());
}

#[test]
fn clipping_group_preserves_half_coverage_and_rejects_reordered_bases() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    source.put(0, address, &[[0.5, 0.0, 0.0, 0.5]]);
    let mut layers = vec![layer(0)];
    for id in 1..=20 {
        source.put(id, address, &[[0.0, 1.0, 0.0, 1.0]]);
        let mut clip = layer(id);
        clip.clip_base = Some(0);
        layers.push(clip);
    }
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 10, 0, rect)
        .unwrap();
    assert_eq!(render(&gpu, &mut prepared), vec![[0.0, 0.5, 0.0, 0.5]]);
    layers[0].descriptor.visible = false;
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 10, 0, rect)
        .unwrap();
    assert_eq!(render(&gpu, &mut prepared), vec![[0.0; 4]]);
    layers[0].descriptor.visible = true;
    layers[10].clip_base = None;
    assert!(renderer
        .prepare_viewport(&gpu, &mut source, &layers, 10, 0, rect)
        .err()
        .unwrap()
        .contains("nearest lower"));
}

#[test]
fn active_tile_refresh_preserves_cached_partitions_masks_and_transparent_removal() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 2);
    let mut source = MemorySource::new(2);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let rect = RenderRect {
        x: 1,
        y: 0,
        width: 1,
        height: 2,
    };
    let mut layers = vec![layer(0), layer(1), layer(2)];
    layers[1].descriptor.opacity = 0.5;
    layers[1].mask_layer = Some(99);
    source.put(0, address, &[[0.0, 0.0, 0.25, 0.5]; 4]);
    source.put(1, address, &[[0.5, 0.0, 0.0, 0.5]; 4]);
    source.put(2, address, &[[0.0, 0.25, 0.0, 0.25]; 4]);
    source.put(99, address, &[[0.0, 0.0, 0.0, 0.5]; 4]);
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 1, 0, rect)
        .unwrap();
    let reads = source.reads;
    source.put(1, address, &[[0.0, 0.4, 0.4, 0.8]; 4]);
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut prepared, address)
        .unwrap());
    assert_eq!(source.reads, reads + 2, "read only active and its mask");
    parity(
        &render(&gpu, &mut prepared),
        &expected(&source, &layers, 0, rect),
        0.001,
    );
    source.tiles.remove(&(1, address));
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut prepared, address)
        .unwrap());
    parity(
        &render(&gpu, &mut prepared),
        &expected(&source, &layers, 0, rect),
        0.001,
    );
    assert!(!renderer
        .update_active_tile(
            &gpu,
            &mut source,
            &mut prepared,
            TileAddress { x: 100, ..address }
        )
        .unwrap());
    assert!(!renderer
        .update_active_tile(
            &gpu,
            &mut source,
            &mut prepared,
            TileAddress {
                level: 1,
                ..address
            }
        )
        .unwrap());
    layers[2].mask_layer = Some(1);
    let mut dependent_mask = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 1, 0, rect)
        .unwrap();
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut dependent_mask, address)
        .is_err());
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut prepared, address)
        .is_err());
}

#[test]
fn persisted_cache_interrupted_publication_rebuilds_from_sources() {
    let dir = Directory::new();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let layers = vec![layer(0)];
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let key = NodeKey {
        node: 1,
        level: 0,
        x: 0,
        y: 0,
    };
    source.put(0, address, &[[0.5, 0.0, 0.0, 0.5]]);
    renderer.configure(&source, &layers).unwrap();
    renderer.node_bytes(&mut source, key).unwrap();
    renderer.commit().unwrap();
    // Simulate the commit boundary after old metadata removal and new blob commit.
    fs::remove_file(dir.0.join("cache/raster.meta")).unwrap();
    source.put(0, address, &[[0.0, 0.5, 0.0, 0.5]]);
    renderer.invalidate_tile(0, address);
    renderer.node_bytes(&mut source, key).unwrap();
    renderer.derived.commit().unwrap();
    fs::write(dir.0.join("cache/raster.meta.next"), b"torn metadata").unwrap();
    drop(renderer);
    let mut reopened = RasterRenderer::open(dir.0.join("cache"), 1, 1024, 0).unwrap();
    assert!(reopened.known.is_empty());
    reopened.configure(&source, &layers).unwrap();
    let reads = source.reads;
    let result = reopened.node_bytes(&mut source, key).unwrap().unwrap();
    assert_eq!(half_pixel(&result, 0), [0.0, 0.5, 0.0, 0.5]);
    assert_eq!(source.reads, reads + 1);
    reopened.commit().unwrap();
    assert!(!dir.0.join("cache/raster.meta.next").exists());
}

#[test]
fn dependency_changes_invalidate_only_affected_ranges_and_mip_ancestors() {
    let dir = Directory::new();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let mut layers = vec![layer(0), layer(1), layer(2)];
    layers[1].mask_layer = Some(90);
    renderer.configure(&source, &layers).unwrap();
    for level in 0..=2 {
        let address = TileAddress { level, x: 0, y: 0 };
        for id in [0, 1, 2, 90] {
            source.put(id, address, &[[0.25, 0.0, 0.0, 0.5]]);
        }
        renderer
            .node_bytes(
                &mut source,
                NodeKey {
                    node: 1,
                    level,
                    x: 0,
                    y: 0,
                },
            )
            .unwrap();
    }
    let level_one = TileAddress {
        level: 1,
        x: 0,
        y: 0,
    };
    assert_eq!(renderer.invalidate_tile(90, level_one), 6);
    assert!(renderer.known.contains(&NodeKey {
        node: 5,
        level: 0,
        x: 0,
        y: 0
    }));
    assert!(renderer.known.contains(&NodeKey {
        node: 4,
        level: 2,
        x: 0,
        y: 0
    }));
    layers[2].descriptor.visible = false;
    renderer.configure(&source, &layers).unwrap();
    assert!(!renderer.known.iter().any(|key| key.node == 6));
    layers.swap(0, 1);
    renderer.configure(&source, &layers).unwrap();
    assert!(renderer.known.is_empty());
    layers[0].descriptor.blend_mode = BlendMode::Multiply;
    renderer.configure(&source, &layers).unwrap();
    assert!(renderer
        .node_bytes(
            &mut source,
            NodeKey {
                node: 1,
                level: 0,
                x: 0,
                y: 0
            }
        )
        .is_err());
    renderer
        .warm_node(
            &mut source,
            NodeKey {
                node: 1,
                level: 0,
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    assert!(!renderer.known.contains(&NodeKey {
        node: 4,
        level: 0,
        x: 0,
        y: 0
    }));
}

#[test]
fn invalid_stack_and_cache_metadata_are_rejected_before_allocating_pixels() {
    let dir = Directory::new();
    let mut renderer = renderer(&dir, 1);
    let source = MemorySource::new(1);
    assert!(renderer.configure(&source, &[]).is_err());
    assert!(renderer
        .configure(&MemorySource::new(2), &[layer(0)])
        .is_err());
    assert!(renderer.configure(&source, &[layer(0), layer(0)]).is_err());
    let mut invalid = layer(0);
    invalid.descriptor.opacity = f32::NAN;
    assert!(renderer.configure(&source, &[invalid]).is_err());
    renderer.configure(&source, &[layer(0)]).unwrap();
    renderer.known.insert(NodeKey {
        node: 1,
        level: 0,
        x: 0,
        y: 0,
    });
    renderer.commit().unwrap();
    let good = renderer.metadata().unwrap();
    drop(renderer);
    let path = dir.0.join("cache/raster.meta");
    let open = || RasterRenderer::open(dir.0.join("cache"), 1, 1024, 0);
    fs::write(&path, b"short").unwrap();
    assert!(open().is_err());
    for mutation in 0..4 {
        let mut bytes = good.clone();
        match mutation {
            0 => bytes[..8].copy_from_slice(b"BADMAGIC"),
            1 => bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes()),
            2 => {
                let offset = 24 + read32(&bytes, 12).unwrap() as usize;
                bytes[offset..offset + 4].copy_from_slice(&0_u32.to_le_bytes());
            }
            _ => bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes()),
        }
        let end = bytes.len() - 4;
        let crc = crc32fast::hash(&bytes[..end]);
        bytes[end..].copy_from_slice(&crc.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(open().is_err());
    }
    fs::write(&path, good).unwrap();
    assert!(open().is_ok());
    assert!(RenderRect {
        x: u32::MAX,
        y: 0,
        width: 1,
        height: 1
    }
    .validate()
    .is_err());
}

#[test]
fn stale_prepared_backdrop_cannot_be_reused_by_an_active_tile_refresh() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let layers = vec![layer(0), layer(1)];
    source.put(0, address, &[[0.5, 0.0, 0.0, 0.5]]);
    source.put(1, address, &[[0.0, 0.25, 0.0, 0.25]]);
    let mut prepared = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 1, 0, rect)
        .unwrap();
    source.put(0, address, &[[0.0, 0.0, 0.5, 0.5]]);
    renderer.invalidate_tile(0, address);
    assert!(prepared.encode(&gpu, &mut gpu.encoder()).is_err());
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut prepared, address)
        .is_err());
    let mut rebuilt = renderer
        .prepare_viewport(&gpu, &mut source, &layers, 1, 0, rect)
        .unwrap();
    parity(
        &render(&gpu, &mut rebuilt),
        &expected(&source, &layers, 0, rect),
        0.001,
    );
    source.put(1, address, &[[0.0, 0.5, 0.0, 0.5]]);
    renderer.invalidate_tile(1, address);
    assert!(rebuilt.encode(&gpu, &mut gpu.encoder()).is_err());
    renderer
        .update_active_tile(&gpu, &mut source, &mut rebuilt, address)
        .unwrap();
    parity(
        &render(&gpu, &mut rebuilt),
        &expected(&source, &layers, 0, rect),
        0.001,
    );
    renderer.invalidate_tile(1, address);
    renderer.invalidate_tile(1, TileAddress { x: 1, ..address });
    assert!(renderer
        .update_active_tile(&gpu, &mut source, &mut rebuilt, address)
        .is_err());
    assert!(rebuilt.encode(&gpu, &mut gpu.encoder()).is_err());
}

#[test]
fn queued_upload_failures_drain_before_releasing_the_gpu_budget_to_the_next_call() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(384).unwrap();
    let mut renderer = renderer(&dir, 1);
    let mut source = MemorySource::new(1);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut layers = vec![layer(0), layer(1)];
    layers[1].descriptor.clipping = true;
    source.put(0, address, &[[0.5, 0.0, 0.0, 0.5]]);
    source.put(1, address, &[[0.0, 1.0, 0.0, 1.0]]);
    for _ in 0..20 {
        assert!(renderer
            .prepare_viewport(&gpu, &mut source, &layers, 0, 0, rect)
            .is_err());
        assert_eq!(gpu.allocated_bytes(), 0);
        assert!(gpu.peak_allocated_bytes() <= 384);
    }
    let image = gpu.upload(1, 1, &[[0.25, 0.0, 0.0, 0.5]]).unwrap();
    gpu.submit(gpu.encoder());
    gpu.wait();
    assert_eq!(gpu.allocated_bytes(), image.byte_len());
    drop(image);
    assert_eq!(gpu.allocated_bytes(), 0);
}

#[test]
fn randomized_50_layer_dependent_raster_stack_matches_cpu_reference_within_2e_5() {
    let dir = Directory::new();
    let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
    let mut renderer = renderer(&dir, 2);
    let mut source = MemorySource::new(2);
    let address = TileAddress {
        level: 0,
        x: 0,
        y: 0,
    };
    let rect = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    let mut layers: Vec<_> = (0..50).map(layer).collect();
    let mut state = 2007_u64;
    for (index, layer) in layers.iter_mut().enumerate() {
        layer.descriptor.blend_mode = BlendMode::ALL[index % BlendMode::ALL.len()];
        layer.descriptor.opacity = 0.3 + (index % 7) as f32 * 0.1;
        layer.descriptor.visible = index % 11 != 3;
        layer.descriptor.clipping = index % 5 == 1 || index % 5 == 2;
        layer.mask_layer = (index % 7 == 1).then_some(99);
        let mut pixels = Vec::new();
        for _ in 0..4 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let alpha = (1 + (state & 127)) as f32 / 256.0;
            pixels.push([
                alpha * ((state >> 8) & 255) as f32 / 255.0,
                alpha * ((state >> 16) & 255) as f32 / 255.0,
                alpha * ((state >> 24) & 255) as f32 / 255.0,
                alpha,
            ]);
        }
        source.put(index as u64, address, &pixels);
    }
    source.put(99, address, &[[0.0, 0.0, 0.0, 0.75]; 4]);
    for active in [0, 21, 49] {
        let mut prepared = renderer
            .prepare_viewport(&gpu, &mut source, &layers, active, 0, rect)
            .unwrap();
        parity(
            &render(&gpu, &mut prepared),
            &expected(&source, &layers, 0, rect),
            0.00002,
        );
    }
}
