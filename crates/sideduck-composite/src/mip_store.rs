//! Stream a persisted mip level through a bounded GPU working set.
//!
//! Inputs remain RGBA8 or RGBA16F during upload. Reusable images reduce at most
//! 1024x1024 source pixels at a time. Destination tiles are assembled on the GPU,
//! read back once, and appended to a separate RGBA16F tile store. No full layer or
//! full pyramid is materialized. A destination store represents exactly one mip
//! level; its caller owns versioning and invalidation against the source store.

use crate::gpu::{GpuCompositor, GpuImage};
use half::f16;
use sideduck_tile::{PixelFormat, TileKey, TileStore};

const MAX_BLOCK_SIZE: u32 = 1024;
const MAX_PENDING_BLOCKS: u32 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MipGenerationStats {
    pub base_tiles_read: u64,
    pub uploaded_blocks: u64,
    pub visited_output_tiles: u64,
    pub nonempty_output_tiles: u64,
    pub completed_layers: u64,
    pub source_bytes_read: u64,
    /// Maximum owned raw block, readback pixels, and encoded destination buffer.
    pub cpu_scratch_bytes: u64,
    /// Includes all images/staging tracked by the supplied compositor.
    pub gpu_peak_bytes: u64,
}

pub fn generate_mips(
    gpu: &GpuCompositor,
    base: &mut TileStore,
    destination: &mut TileStore,
    canvas: [u32; 2],
    layers: &[u32],
    level: u8,
) -> Result<MipGenerationStats, String> {
    generate_mips_with_progress(gpu, base, destination, canvas, layers, level, &mut |_| {
        Ok(())
    })
}

/// The progress hook runs after each completed destination tile. Benchmark
/// callers use it to assert total process RSS, in addition to image/cache bounds.
pub fn generate_mips_with_progress<F>(
    gpu: &GpuCompositor,
    base: &mut TileStore,
    destination: &mut TileStore,
    canvas: [u32; 2],
    layers: &[u32],
    level: u8,
    progress: &mut F,
) -> Result<MipGenerationStats, String>
where
    F: FnMut(&MipGenerationStats) -> Result<(), String>,
{
    let source_options = base.options();
    let destination_options = destination.options();
    let tile_size = source_options.tile_size;
    if canvas.contains(&0) || !(1..=10).contains(&level) {
        return Err(
            "persisted mip generation requires a positive canvas and level 1 through 10".into(),
        );
    }
    if tile_size != destination_options.tile_size
        || destination_options.format != PixelFormat::Rgba16Float
    {
        return Err(
            "persisted mip destination must use matching tile size and RGBA16F pixels".into(),
        );
    }
    let factor = 1_u32 << level;
    let block_size = tile_size
        .checked_mul(factor)
        .ok_or("mip source block size overflow")?
        .min(MAX_BLOCK_SIZE);
    if block_size < factor
        || !block_size.is_power_of_two()
        || !tile_size.is_multiple_of(block_size / factor)
    {
        return Err("tile size cannot be partitioned into bounded blocks at this mip level".into());
    }
    let output_block_size = block_size / factor;
    let blocks_per_output_tile = tile_size / output_block_size;
    let pixel_bytes = match source_options.format {
        PixelFormat::Rgba8 => 4,
        PixelFormat::Rgba16Float => 8,
    };
    let raw_length = (u64::from(block_size) * u64::from(block_size) * pixel_bytes) as usize;
    let mut raw = vec![0_u8; raw_length];
    let mut reductions = Vec::with_capacity(usize::from(level));
    for step in 1..=level {
        reductions.push(gpu.create_image(block_size >> step, block_size >> step)?);
    }
    let output = gpu.create_image(tile_size, tile_size)?;
    // Create the input last: it queues an upload, whereas image allocation alone
    // does not submit work. The workspace drains every exit path before owners
    // release their reservations, including a later source/readback failure.
    let input = match source_options.format {
        PixelFormat::Rgba8 => gpu.upload_rgba8(block_size, block_size, &raw)?,
        PixelFormat::Rgba16Float => gpu.upload_rgba16f(block_size, block_size, &raw)?,
    };
    let workspace = Workspace {
        gpu,
        input,
        reductions,
        output,
    };
    let reducer = Downsampler::new(gpu)?;
    let dimensions = canvas.map(|n| n.div_ceil(factor));
    let block_counts = canvas.map(|n| n.div_ceil(block_size));
    let mut encoded = vec![0_u8; tile_size as usize * tile_size as usize * 8];
    let mut stats = MipGenerationStats {
        cpu_scratch_bytes: (raw.len() + encoded.len()) as u64,
        ..MipGenerationStats::default()
    };
    for &layer in layers {
        for tile_y in 0..dimensions[1].div_ceil(tile_size) {
            for tile_x in 0..dimensions[0].div_ceil(tile_size) {
                let key = TileKey {
                    layer,
                    x: tile_x,
                    y: tile_y,
                };
                let mut encoder = gpu.encoder();
                gpu.encode_clear(&mut encoder, &workspace.output)?;
                let first_block = [
                    tile_x * blocks_per_output_tile,
                    tile_y * blocks_per_output_tile,
                ];
                let mut nonempty_blocks = 0_u32;
                for block_y in
                    first_block[1]..(first_block[1] + blocks_per_output_tile).min(block_counts[1])
                {
                    for block_x in first_block[0]
                        ..(first_block[0] + blocks_per_output_tile).min(block_counts[0])
                    {
                        raw.fill(0);
                        let found = fill_block(
                            base,
                            layer,
                            canvas,
                            [block_x, block_y],
                            block_size,
                            tile_size,
                            pixel_bytes as usize,
                            &mut raw,
                            &mut stats,
                        )?;
                        if !found {
                            continue;
                        }
                        gpu.queue().write_texture(
                            workspace.input.texture().as_image_copy(),
                            &raw,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(block_size * pixel_bytes as u32),
                                rows_per_image: Some(block_size),
                            },
                            wgpu::Extent3d {
                                width: block_size,
                                height: block_size,
                                depth_or_array_layers: 1,
                            },
                        );
                        let mut previous = &workspace.input;
                        for reduced in &workspace.reductions {
                            reducer.encode(&mut encoder, previous, reduced);
                            previous = reduced;
                        }
                        encoder.copy_texture_to_texture(
                            previous.texture().as_image_copy(),
                            wgpu::TexelCopyTextureInfo {
                                texture: workspace.output.texture(),
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: (block_x - first_block[0]) * output_block_size,
                                    y: (block_y - first_block[1]) * output_block_size,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: output_block_size,
                                height: output_block_size,
                                depth_or_array_layers: 1,
                            },
                        );
                        gpu.submit(encoder);
                        encoder = gpu.encoder();
                        stats.uploaded_blocks += 1;
                        nonempty_blocks += 1;
                        // Queue upload staging is driver-owned. Bound queued work
                        // rather than relying only on the image ownership counter.
                        if nonempty_blocks.is_multiple_of(MAX_PENDING_BLOCKS) {
                            gpu.wait();
                        }
                    }
                }
                if nonempty_blocks == 0 {
                    destination.remove(key);
                } else {
                    let pixels = gpu.readback(&workspace.output)?;
                    for (pixel, bytes) in pixels.iter().zip(encoded.chunks_exact_mut(8)) {
                        for channel in 0..4 {
                            bytes[channel * 2..channel * 2 + 2].copy_from_slice(
                                &f16::from_f32(pixel[channel]).to_bits().to_le_bytes(),
                            );
                        }
                    }
                    destination
                        .put(key, &encoded)
                        .map_err(|error| error.to_string())?;
                    stats.nonempty_output_tiles += u64::from(destination.contains(key));
                    stats.cpu_scratch_bytes = stats
                        .cpu_scratch_bytes
                        .max((raw.len() + encoded.len() + pixels.len() * 16) as u64);
                }
                stats.visited_output_tiles += 1;
                stats.gpu_peak_bytes = gpu.peak_allocated_bytes();
                progress(&stats)?;
            }
        }
        stats.completed_layers += 1;
    }
    destination.commit().map_err(|error| error.to_string())?;
    // All reused image owners remain alive until their submissions complete.
    gpu.wait();
    Ok(stats)
}

struct Workspace<'a> {
    gpu: &'a GpuCompositor,
    input: GpuImage,
    reductions: Vec<GpuImage>,
    output: GpuImage,
}

impl Drop for Workspace<'_> {
    fn drop(&mut self) {
        self.gpu.queue().submit(std::iter::empty());
        self.gpu.wait();
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_block(
    base: &mut TileStore,
    layer: u32,
    canvas: [u32; 2],
    block: [u32; 2],
    block_size: u32,
    tile_size: u32,
    pixel_bytes: usize,
    output: &mut [u8],
    stats: &mut MipGenerationStats,
) -> Result<bool, String> {
    let start = block.map(|n| u64::from(n) * u64::from(block_size));
    let end = [
        (start[0] + u64::from(block_size)).min(u64::from(canvas[0])),
        (start[1] + u64::from(block_size)).min(u64::from(canvas[1])),
    ];
    let mut found = false;
    for tile_y in start[1] / u64::from(tile_size)..end[1].div_ceil(u64::from(tile_size)) {
        for tile_x in start[0] / u64::from(tile_size)..end[0].div_ceil(u64::from(tile_size)) {
            let key = TileKey {
                layer,
                x: tile_x as u32,
                y: tile_y as u32,
            };
            let Some(bytes) = base.get(key).map_err(|error| error.to_string())? else {
                continue;
            };
            found = true;
            stats.base_tiles_read += 1;
            stats.source_bytes_read += bytes.len() as u64;
            let tile_origin = [tile_x * u64::from(tile_size), tile_y * u64::from(tile_size)];
            let left = tile_origin[0].max(start[0]);
            let right = (tile_origin[0] + u64::from(tile_size)).min(end[0]);
            let top = tile_origin[1].max(start[1]);
            let bottom = (tile_origin[1] + u64::from(tile_size)).min(end[1]);
            let width = (right - left) as usize * pixel_bytes;
            for y in top..bottom {
                let source_offset = ((y - tile_origin[1]) * u64::from(tile_size)
                    + (left - tile_origin[0])) as usize
                    * pixel_bytes;
                let output_offset = ((y - start[1]) * u64::from(block_size) + (left - start[0]))
                    as usize
                    * pixel_bytes;
                output[output_offset..output_offset + width]
                    .copy_from_slice(&bytes[source_offset..source_offset + width]);
            }
        }
    }
    Ok(found)
}

struct Downsampler<'a> {
    gpu: &'a GpuCompositor,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl<'a> Downsampler<'a> {
    fn new(gpu: &'a GpuCompositor) -> Result<Self, String> {
        let device = gpu.device();
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("premultiplied mip reduction"),
            source: wgpu::ShaderSource::Wgsl(include_str!("downsample.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mip reduction bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mip reduction layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mip reduction"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("downsample"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(device.pop_error_scope()) {
            return Err(format!("mip shader initialization failed: {error}"));
        }
        Ok(Self {
            gpu,
            pipeline,
            layout,
        })
    }

    fn encode(&self, encoder: &mut wgpu::CommandEncoder, input: &GpuImage, output: &GpuImage) {
        let bindings = self
            .gpu
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mip reduction images"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(output.view()),
                    },
                ],
            });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("mip reduction pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups(output.width().div_ceil(16), output.height().div_ceil(16), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::Pixel;
    use crate::pyramid::{MipKey, MipPyramid};
    use sideduck_tile::StoreOptions;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mip-test-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn decode(bytes: &[u8], format: PixelFormat) -> Vec<Pixel> {
        match format {
            PixelFormat::Rgba8 => bytes
                .chunks_exact(4)
                .map(|p| std::array::from_fn(|c| f32::from(p[c]) / 255.0))
                .collect(),
            PixelFormat::Rgba16Float => bytes
                .chunks_exact(8)
                .map(|p| {
                    std::array::from_fn(|c| {
                        f16::from_bits(u16::from_le_bytes([p[c * 2], p[c * 2 + 1]])).to_f32()
                    })
                })
                .collect(),
        }
    }

    fn compare_fixture(
        format: PixelFormat,
        tile_size: u32,
        canvas: [u32; 2],
        level: u8,
        keys: &[TileKey],
    ) {
        let scratch = Scratch::new();
        let options = StoreOptions {
            tile_size,
            format,
            cache_budget_bytes: 4 * 1024 * 1024,
        };
        let mut base = TileStore::create(scratch.0.join("base"), options).unwrap();
        for (ordinal, &key) in keys.iter().enumerate() {
            let pixels: Vec<Pixel> = (0..tile_size * tile_size)
                .map(|n| {
                    let alpha = ((n + ordinal as u32) % 7 + 1) as f32 / 8.0;
                    [alpha * 0.6, alpha * 0.3, alpha * 0.1, alpha]
                })
                .collect();
            let bytes: Vec<u8> = match format {
                PixelFormat::Rgba8 => pixels
                    .iter()
                    .flat_map(|p| p.map(|c| (c * 255.0).round() as u8))
                    .collect(),
                PixelFormat::Rgba16Float => pixels
                    .iter()
                    .flat_map(|p| {
                        p.iter()
                            .flat_map(|c| f16::from_f32(*c).to_bits().to_le_bytes())
                    })
                    .collect(),
            };
            base.put(key, &bytes).unwrap();
        }
        base.commit().unwrap();
        let mut destination = TileStore::create(
            scratch.0.join("mips"),
            StoreOptions {
                format: PixelFormat::Rgba16Float,
                ..options
            },
        )
        .unwrap();
        let gpu = GpuCompositor::new(64 * 1024 * 1024).unwrap();
        let stats = generate_mips(&gpu, &mut base, &mut destination, canvas, &[0], level).unwrap();
        assert!(stats.gpu_peak_bytes <= 64 * 1024 * 1024);
        assert!(stats.cpu_scratch_bytes <= 16 * 1024 * 1024);
        let mut reference = MipPyramid::new(canvas, tile_size, 16 * 1024 * 1024).unwrap();
        let dimensions = canvas.map(|n| n.div_ceil(1 << level));
        for y in 0..dimensions[1].div_ceil(tile_size) {
            for x in 0..dimensions[0].div_ceil(tile_size) {
                let key = TileKey { layer: 0, x, y };
                let expected = reference
                    .tile(
                        MipKey {
                            layer: 0,
                            level,
                            x,
                            y,
                        },
                        &mut |k| {
                            if k.level != 0 {
                                return Ok(None);
                            }
                            base.get(TileKey {
                                layer: 0,
                                x: k.x,
                                y: k.y,
                            })
                            .map(|b| b.map(|b| decode(b, format)))
                            .map_err(|e| e.to_string())
                        },
                    )
                    .unwrap()
                    .unwrap_or_else(|| vec![[0.0; 4]; tile_size as usize * tile_size as usize]);
                let actual = destination
                    .get(key)
                    .unwrap()
                    .map(|b| decode(b, PixelFormat::Rgba16Float))
                    .unwrap_or_else(|| vec![[0.0; 4]; expected.len()]);
                for (index, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
                    for channel in 0..4 {
                        assert!(
                            (a[channel] - b[channel]).abs() <= 0.0005,
                            "pixel {index}: {a:?} != {b:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn persisted_gpu_mip_rgba8_and_rgba16f_match_scalar_reference_at_canvas_edges() {
        let keys = [
            TileKey {
                layer: 0,
                x: 0,
                y: 0,
            },
            TileKey {
                layer: 0,
                x: 2,
                y: 1,
            },
        ];
        for format in [PixelFormat::Rgba8, PixelFormat::Rgba16Float] {
            compare_fixture(format, 2, [5, 3], 1, &keys);
        }
    }

    #[test]
    fn bounded_gpu_blocks_assemble_sparse_mip_with_per_pixel_reference_parity() {
        let keys = [
            TileKey {
                layer: 0,
                x: 0,
                y: 0,
            },
            TileKey {
                layer: 0,
                x: 32,
                y: 0,
            },
            TileKey {
                layer: 0,
                x: 0,
                y: 32,
            },
            TileKey {
                layer: 0,
                x: 32,
                y: 32,
            },
        ];
        compare_fixture(PixelFormat::Rgba8, 32, [1301, 1151], 6, &keys);
    }

    #[test]
    fn persisted_mip_rejects_invalid_level_format_and_tiny_gpu_ceiling() {
        let scratch = Scratch::new();
        let options = StoreOptions {
            tile_size: 2,
            format: PixelFormat::Rgba8,
            cache_budget_bytes: 1024,
        };
        let mut base = TileStore::create(scratch.0.join("base"), options).unwrap();
        base.put(
            TileKey {
                layer: 0,
                x: 0,
                y: 0,
            },
            &[255; 16],
        )
        .unwrap();
        let mut destination = TileStore::create(scratch.0.join("dest"), options).unwrap();
        let gpu = GpuCompositor::new(512).unwrap();
        assert!(generate_mips(&gpu, &mut base, &mut destination, [4, 4], &[0], 0).is_err());
        assert!(generate_mips(&gpu, &mut base, &mut destination, [4, 4], &[0], 1).is_err());
        assert_eq!(gpu.allocated_bytes(), 0);
        let mut destination = TileStore::create(
            scratch.0.join("half"),
            StoreOptions {
                format: PixelFormat::Rgba16Float,
                ..options
            },
        )
        .unwrap();
        assert!(generate_mips(&gpu, &mut base, &mut destination, [4, 4], &[0], 1).is_err());
        assert_eq!(gpu.allocated_bytes(), 0);
    }
}
