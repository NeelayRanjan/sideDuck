//! Persistent GPU compositing images; no readback is needed between layer passes.
//!
//! The byte ceiling covers image storage, conservative row-aligned queued upload
//! staging, and readback staging owned by this API.
//! wgpu can retain submitted resources after their Rust owners are dropped. Call
//! `wait()` before dropping and reallocating a batch; process RSS is a separate
//! performance gate and includes driver allocations, uniforms, and host pixels.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use wgpu::util::DeviceExt;

use crate::blend::{BlendMode, Pixel};

const PIXEL_BYTES: u64 = 16;
pub(crate) const TIMESTAMP_CAPACITY: u32 = 4096;

struct TimestampCapture {
    query: Arc<wgpu::QuerySet>,
    next: Arc<AtomicU32>,
}
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(1);
static NEXT_DEVICE: AtomicU64 = AtomicU64::new(1);

struct Budget {
    limit: u64,
    current: AtomicU64,
    peak: AtomicU64,
}

impl Budget {
    fn reserve(self: &Arc<Self>, bytes: u64) -> Result<Reservation, String> {
        let result = self
            .current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.limit)
            });
        let previous = result.map_err(|_| "GPU image residency ceiling exceeded".to_owned())?;
        self.peak.fetch_max(previous + bytes, Ordering::AcqRel);
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

struct Reservation {
    budget: Arc<Budget>,
    bytes: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.current.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// An owned linear-premultiplied image. Retain this owner while
/// external views are in use; accounting cannot track clones of raw wgpu handles.
pub struct GpuImage {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    id: u64,
    device_id: u64,
    format: wgpu::TextureFormat,
    _reservation: Reservation,
}

impl GpuImage {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn byte_len(&self) -> u64 {
        self._reservation.bytes
    }
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
    fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        }
    }
}

pub struct GpuCompositor {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_layout: wgpu::BindGroupLayout,
    clear_pipeline: wgpu::ComputePipeline,
    clear_layout: wgpu::BindGroupLayout,
    adapter_name: String,
    device_id: u64,
    budget: Arc<Budget>,
    timing: Mutex<Option<TimestampCapture>>,
    pending_uploads: Mutex<Vec<Reservation>>,
}

impl GpuCompositor {
    pub fn new(memory_budget_bytes: u64) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        Self::with_surface(instance, None, memory_budget_bytes)
    }

    pub fn new_for_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        memory_budget_bytes: u64,
    ) -> Result<Self, String> {
        Self::with_surface(instance, Some(surface), memory_budget_bytes)
    }

    fn with_surface(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        memory_budget_bytes: u64,
    ) -> Result<Self, String> {
        if memory_budget_bytes < PIXEL_BYTES {
            return Err("GPU image budget must hold at least one pixel".into());
        }
        pollster::block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: surface,
                })
                .await
                .ok_or("No GPU adapter is available for compositing")?;
            let format = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba32Float);
            let usages = wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST;
            if !format.allowed_usages.contains(usages) {
                return Err("GPU does not support RGBA32Float compositing textures".into());
            }
            let info = adapter.get_info();
            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("compositing device"),
                        required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
                        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("linear blend modes"),
                source: wgpu::ShaderSource::Wgsl(include_str!("blend.wgsl").into()),
            });
            let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("compositing bindings"),
                entries: &[
                    texture_entry(0),
                    texture_entry(1),
                    output_entry(2),
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(16),
                        },
                        count: None,
                    },
                ],
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("compositing layout"),
                bind_group_layouts: &[&bind_layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("linear blend"),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some("composite"),
                compilation_options: Default::default(),
                cache: None,
            });
            let clear_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("clear binding"),
                entries: &[output_entry(2)],
            });
            let clear_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("clear layout"),
                    bind_group_layouts: &[&clear_layout],
                    push_constant_ranges: &[],
                });
            let clear_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("clear image"),
                layout: Some(&clear_pipeline_layout),
                module: &shader,
                entry_point: Some("clear"),
                compilation_options: Default::default(),
                cache: None,
            });
            if let Some(error) = device.pop_error_scope().await {
                return Err(format!("Compositor shader initialization failed: {error}"));
            }
            Ok(Self {
                instance,
                adapter,
                device,
                queue,
                pipeline,
                bind_layout,
                clear_pipeline,
                clear_layout,
                adapter_name: format!("{} ({:?})", info.name, info.backend),
                device_id: NEXT_DEVICE.fetch_add(1, Ordering::Relaxed),
                timing: Mutex::new(None),
                pending_uploads: Mutex::new(Vec::new()),
                budget: Arc::new(Budget {
                    limit: memory_budget_bytes,
                    current: AtomicU64::new(0),
                    peak: AtomicU64::new(0),
                }),
            })
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }
    pub fn device_identity(&self) -> u64 {
        self.device_id
    }
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    pub fn allocated_bytes(&self) -> u64 {
        self.budget.current.load(Ordering::Acquire)
    }
    pub fn peak_allocated_bytes(&self) -> u64 {
        self.budget.peak.load(Ordering::Acquire)
    }
    pub fn budget_bytes(&self) -> u64 {
        self.budget.limit
    }

    pub fn create_image(&self, width: u32, height: u32) -> Result<GpuImage, String> {
        self.create_format(width, height, wgpu::TextureFormat::Rgba32Float, PIXEL_BYTES)
    }

    fn create_format(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        pixel_bytes: u64,
    ) -> Result<GpuImage, String> {
        let max = self.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err("GPU image dimensions are zero or exceed the adapter limit".into());
        }
        let bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|v| v.checked_mul(pixel_bytes))
            .ok_or("GPU image size overflow")?;
        let reservation = self.budget.reserve(bytes)?;
        // WebGPU guarantees zero initialization before the first observable use.
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("linear compositing image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(GpuImage {
            texture,
            view,
            width,
            height,
            id: NEXT_IMAGE.fetch_add(1, Ordering::Relaxed),
            device_id: self.device_id,
            format,
            _reservation: reservation,
        })
    }

    /// Linear RGBA8 premultiplied bytes; no sRGB transfer function is applied.
    pub fn upload_rgba8(&self, width: u32, height: u32, pixels: &[u8]) -> Result<GpuImage, String> {
        self.upload_bytes(width, height, pixels, wgpu::TextureFormat::Rgba8Unorm, 4)
    }

    /// Little-endian IEEE754 half-float components, linear and premultiplied.
    pub fn upload_rgba16f(
        &self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<GpuImage, String> {
        self.upload_bytes(width, height, pixels, wgpu::TextureFormat::Rgba16Float, 8)
    }

    fn upload_bytes(
        &self,
        width: u32,
        height: u32,
        pixels: &[u8],
        format: wgpu::TextureFormat,
        pixel_bytes: u64,
    ) -> Result<GpuImage, String> {
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(pixel_bytes))
            .ok_or("GPU upload size overflow")?;
        if u64::try_from(pixels.len()).ok() != Some(expected) {
            return Err("GPU upload byte count does not match image dimensions and format".into());
        }
        let image = self.create_format(width, height, format, pixel_bytes)?;
        self.write_bytes(&image, pixels, format, pixel_bytes)?;
        Ok(image)
    }

    pub fn upload(&self, width: u32, height: u32, pixels: &[Pixel]) -> Result<GpuImage, String> {
        if u64::try_from(pixels.len()).ok() != Some(u64::from(width) * u64::from(height)) {
            return Err("GPU upload pixel count does not match image dimensions".into());
        }
        let image = self.create_image(width, height)?;
        self.write(&image, pixels)?;
        Ok(image)
    }

    /// Queue writes execute before the next submitted command buffer. Do not
    /// overwrite a source between encoded passes in one unsubmitted batch.
    pub fn write(&self, image: &GpuImage, pixels: &[Pixel]) -> Result<(), String> {
        self.write_bytes(
            image,
            bytemuck::cast_slice(pixels),
            wgpu::TextureFormat::Rgba32Float,
            PIXEL_BYTES,
        )
    }

    /// Replace all linear premultiplied half-float pixels without allocating a
    /// texture. The renderer must hold exclusive logical ownership (for example
    /// `Arc::get_mut`) and finish any prior batch before recycling its image.
    /// Upload staging remains charged until the next submission completes.
    pub fn write_rgba16f(&self, image: &GpuImage, pixels: &[u8]) -> Result<(), String> {
        self.write_bytes(image, pixels, wgpu::TextureFormat::Rgba16Float, 8)
    }

    fn write_bytes(
        &self,
        image: &GpuImage,
        pixels: &[u8],
        format: wgpu::TextureFormat,
        pixel_bytes: u64,
    ) -> Result<(), String> {
        self.validate_image(image)?;
        if image.format != format {
            return Err("GPU upload format does not match the destination texture".into());
        }
        let row_bytes = u64::from(image.width)
            .checked_mul(pixel_bytes)
            .ok_or("GPU upload row size overflow")?;
        let expected = row_bytes
            .checked_mul(u64::from(image.height))
            .ok_or("GPU upload size overflow")?;
        if u64::try_from(pixels.len()).ok() != Some(expected) {
            return Err("GPU upload byte count does not match image dimensions and format".into());
        }
        let row_pitch = row_bytes.div_ceil(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
            * u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging_bytes = row_pitch
            .checked_mul(u64::from(image.height))
            .ok_or("GPU upload staging size overflow")?;
        if staging_bytes > self.device.limits().max_buffer_size {
            return Err(
                "GPU upload staging exceeds the adapter buffer limit; upload tiles instead".into(),
            );
        }
        let mut pending = self
            .pending_uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reservation = self.budget.reserve(staging_bytes)?;
        self.queue.write_texture(
            image.texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(image.height),
            },
            image.extent(),
        );
        pending.push(reservation);
        Ok(())
    }

    pub(crate) fn begin_timestamp_capture(
        &self,
        query: Arc<wgpu::QuerySet>,
        next: Arc<AtomicU32>,
    ) -> Result<(), String> {
        let mut timing = self
            .timing
            .lock()
            .map_err(|_| "GPU timestamp capture lock poisoned")?;
        if timing.is_some() {
            return Err("A GPU timestamp capture is already active".into());
        }
        *timing = Some(TimestampCapture { query, next });
        Ok(())
    }

    pub(crate) fn end_timestamp_capture(&self) -> Result<(), String> {
        self.timing
            .lock()
            .map_err(|_| "GPU timestamp capture lock poisoned")?
            .take();
        Ok(())
    }

    pub(crate) fn timestamp_pair(&self) -> Result<Option<(Arc<wgpu::QuerySet>, u32)>, String> {
        let timing = self
            .timing
            .lock()
            .map_err(|_| "GPU timestamp capture lock poisoned")?;
        if let Some(capture) = &*timing {
            let index = capture.next.fetch_add(2, Ordering::Relaxed);
            if index + 1 >= TIMESTAMP_CAPACITY {
                return Err("GPU timestamp pass capacity exceeded".into());
            }
            Ok(Some((capture.query.clone(), index)))
        } else {
            Ok(None)
        }
    }

    pub fn encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compositing commands"),
            })
    }

    pub fn encode_blend(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bottom: &GpuImage,
        top: &GpuImage,
        output: &GpuImage,
        mode: BlendMode,
        opacity: f32,
    ) -> Result<(), String> {
        self.encode_operation(encoder, bottom, top, output, mode as u32, opacity)
    }

    /// Source-atop within an isolated clipping group. This preserves the base
    /// alpha: clipped paint cannot create coverage outside the base layer.
    pub fn encode_clip(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bottom: &GpuImage,
        top: &GpuImage,
        output: &GpuImage,
        mode: BlendMode,
        opacity: f32,
    ) -> Result<(), String> {
        self.encode_operation(
            encoder,
            bottom,
            top,
            output,
            (mode as u32) | 0x8000_0000,
            opacity,
        )
    }

    fn encode_operation(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bottom: &GpuImage,
        top: &GpuImage,
        output: &GpuImage,
        operation: u32,
        opacity: f32,
    ) -> Result<(), String> {
        self.validate_triplet(bottom, top, output)?;
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let parameters = [operation, opacity.to_bits(), output.width, output.height];
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blend parameters"),
                contents: bytemuck::cast_slice(&parameters),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend inputs"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&bottom.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&top.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&output.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let timestamp = self.timestamp_pair()?;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("blend pass"),
            timestamp_writes: timestamp.as_ref().map(|(query_set, index)| {
                wgpu::ComputePassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(*index),
                    end_of_pass_write_index: Some(*index + 1),
                }
            }),
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups(output.width.div_ceil(16), output.height.div_ceil(16), 1);
        Ok(())
    }

    pub fn encode_clear(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        image: &GpuImage,
    ) -> Result<(), String> {
        self.validate_image(image)?;
        if image.format != wgpu::TextureFormat::Rgba32Float {
            return Err("GPU clear output must be RGBA32Float".into());
        }
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clear output"),
            layout: &self.clear_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&image.view),
            }],
        });
        let timestamp = self.timestamp_pair()?;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("clear pass"),
            timestamp_writes: timestamp.as_ref().map(|(query_set, index)| {
                wgpu::ComputePassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(*index),
                    end_of_pass_write_index: Some(*index + 1),
                }
            }),
        });
        pass.set_pipeline(&self.clear_pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups(image.width.div_ceil(16), image.height.div_ceil(16), 1);
        Ok(())
    }

    pub fn submit(&self, encoder: wgpu::CommandEncoder) -> wgpu::SubmissionIndex {
        // Serialize with writes so a reservation cannot be released for a later
        // write that was not actually part of this submission.
        let mut pending = self
            .pending_uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let uploads = std::mem::take(&mut *pending);
        let index = self.queue.submit([encoder.finish()]);
        if !uploads.is_empty() {
            self.queue.on_submitted_work_done(move || drop(uploads));
        }
        index
    }

    pub fn wait(&self) {
        self.device.poll(wgpu::Maintain::Wait);
    }

    pub fn readback(&self, image: &GpuImage) -> Result<Vec<Pixel>, String> {
        self.validate_image(image)?;
        if image.format != wgpu::TextureFormat::Rgba32Float {
            return Err(
                "GPU readback requires RGBA32Float; blend the input into a float output first"
                    .into(),
            );
        }
        let row_bytes = image.width * PIXEL_BYTES as u32;
        let stride = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let staging_bytes = u64::from(stride) * u64::from(image.height);
        if staging_bytes > self.device.limits().max_buffer_size {
            return Err(
                "GPU readback exceeds adapter buffer limit; read viewport tiles instead".into(),
            );
        }
        let _reservation = self.budget.reserve(staging_bytes)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compositing readback"),
            size: staging_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder();
        encoder.copy_texture_to_buffer(
            image.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(image.height),
                },
            },
            image.extent(),
        );
        self.submit(encoder);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.wait();
        receiver
            .recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mapping = buffer.slice(..).get_mapped_range();
        let count = usize::try_from(u64::from(image.width) * u64::from(image.height))
            .map_err(|_| "GPU image exceeds host address space")?;
        let mut pixels = Vec::with_capacity(count);
        for row in mapping.chunks_exact(stride as usize) {
            for pixel in row[..row_bytes as usize].chunks_exact(16) {
                let mut decoded = [0.0; 4];
                for (component, bytes) in decoded.iter_mut().zip(pixel.chunks_exact(4)) {
                    *component = f32::from_ne_bytes(
                        bytes.try_into().map_err(|_| "Invalid GPU pixel width")?,
                    );
                }
                pixels.push(decoded);
            }
        }
        drop(mapping);
        buffer.unmap();
        Ok(pixels)
    }

    pub(crate) fn validate_image(&self, image: &GpuImage) -> Result<(), String> {
        if image.device_id != self.device_id {
            return Err("GPU image belongs to a different compositor device".into());
        }
        Ok(())
    }

    fn validate_triplet(
        &self,
        bottom: &GpuImage,
        top: &GpuImage,
        output: &GpuImage,
    ) -> Result<(), String> {
        for image in [bottom, top, output] {
            self.validate_image(image)?;
        }
        if output.format != wgpu::TextureFormat::Rgba32Float {
            return Err("GPU blend output must be RGBA32Float".into());
        }
        if bottom.width != output.width
            || top.width != output.width
            || bottom.height != output.height
            || top.height != output.height
        {
            return Err("GPU blend image dimensions do not match".into());
        }
        if output.id == bottom.id || output.id == top.id {
            return Err("GPU blend output must not alias either input".into());
        }
        Ok(())
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn output_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::blend;

    // RGBA32Float arithmetic may contract multiply-adds on the GPU. This absolute
    // per-component tolerance is < 0.006 of an RGBA8 code value, including 50 passes.
    const TOLERANCE: f32 = 2e-5;

    struct Random(u64);
    impl Random {
        fn unit(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / 16_777_215.0
        }
        fn pixel(&mut self) -> Pixel {
            let alpha = self.unit();
            [
                self.unit() * alpha,
                self.unit() * alpha,
                self.unit() * alpha,
                alpha,
            ]
        }
    }

    fn assert_pixels(actual: &[Pixel], expected: &[Pixel], context: &str) {
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            for channel in 0..4 {
                assert!(actual[channel].is_finite() && (actual[channel] - expected[channel]).abs() <= TOLERANCE,
                    "{context}: pixel {index} component {channel}: GPU {actual:?}, CPU {expected:?}");
            }
        }
    }

    #[test]
    fn gpu_clipping_every_blend_mode_per_pixel_reference_parity_preserves_base_alpha() {
        let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
        let mut random = Random(0x4581_7ca2_989f_bc53);
        let (width, height) = (7, 11);
        let mut bottom: Vec<_> = (0..width * height).map(|_| random.pixel()).collect();
        let mut top: Vec<_> = (0..width * height).map(|_| random.pixel()).collect();
        bottom[0] = [0.0; 4];
        bottom[1] = [1.0; 4];
        bottom[2] = [f32::NAN, 0.5, f32::INFINITY, 0.75];
        top[3] = [0.0; 4];
        top[4] = [f32::NAN, 0.25, 0.0, 0.5];
        let bottom_image = gpu.upload(width, height, &bottom).unwrap();
        let top_image = gpu.upload(width, height, &top).unwrap();
        let output = gpu.create_image(width, height).unwrap();
        for mode in BlendMode::ALL {
            for opacity in [0.0, 0.37, 1.0, f32::NAN] {
                let expected: Vec<_> = bottom
                    .iter()
                    .zip(&top)
                    .map(|(dst, src)| {
                        let dst = crate::blend::sanitize(*dst);
                        if dst[3] == 0.0 {
                            return [0.0; 4];
                        }
                        // Apply the ordinary reference to an opaque backdrop, then
                        // restore base coverage. This independently derives atop.
                        let opaque = [dst[0] / dst[3], dst[1] / dst[3], dst[2] / dst[3], 1.0];
                        let color = blend(opaque, *src, mode, opacity);
                        [
                            color[0] * dst[3],
                            color[1] * dst[3],
                            color[2] * dst[3],
                            dst[3],
                        ]
                    })
                    .collect();
                let mut encoder = gpu.encoder();
                gpu.encode_clip(
                    &mut encoder,
                    &bottom_image,
                    &top_image,
                    &output,
                    mode,
                    opacity,
                )
                .unwrap();
                gpu.submit(encoder);
                let actual = gpu.readback(&output).unwrap();
                assert_pixels(
                    &actual,
                    &expected,
                    &format!("clipping {mode:?} opacity {opacity}"),
                );
                for (actual, source) in actual.iter().zip(&bottom) {
                    assert_eq!(actual[3], crate::blend::sanitize(*source)[3]);
                }
            }
        }
    }

    #[test]
    fn gpu_every_blend_mode_per_pixel_parity_with_cpu_reference() {
        let gpu = GpuCompositor::new(16 * 1024 * 1024).unwrap();
        let (width, height) = (13, 17); // Partial workgroups and padded readback rows.
        let mut random = Random(0x46ad_d613_b8c0_2287);
        let mut bottom: Vec<_> = (0..width * height).map(|_| random.pixel()).collect();
        let mut top: Vec<_> = (0..width * height).map(|_| random.pixel()).collect();
        let edges = [
            [0.0; 4],
            [1.0; 4],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 1.0],
            [0.4, 0.4, 0.4, 1.0],
            [f32::NAN, f32::INFINITY, -1.0, 2.0],
            [0.2, 0.1, 0.3, 0.0],
            [0.0, 0.25, 1.0, 1.0],
            [1.0, 0.5, 0.0, 1.0],
        ];
        for (index, pixel) in edges.iter().copied().enumerate() {
            bottom[index] = pixel;
            top[index] = edges[edges.len() - index - 1];
        }
        let bottom_image = gpu.upload(width, height, &bottom).unwrap();
        let top_image = gpu.upload(width, height, &top).unwrap();
        let output = gpu.create_image(width, height).unwrap();
        for mode in BlendMode::ALL {
            for opacity in [0.0, 0.37, 1.0, f32::NAN] {
                let expected: Vec<_> = bottom
                    .iter()
                    .zip(&top)
                    .map(|(b, s)| blend(*b, *s, mode, opacity))
                    .collect();
                let mut encoder = gpu.encoder();
                gpu.encode_blend(
                    &mut encoder,
                    &bottom_image,
                    &top_image,
                    &output,
                    mode,
                    opacity,
                )
                .unwrap();
                gpu.submit(encoder);
                let actual = gpu.readback(&output).unwrap();
                assert_pixels(&actual, &expected, &format!("{mode:?}, opacity {opacity}"));
            }
        }
    }

    #[test]
    fn gpu_randomized_50_layer_stack_per_pixel_parity_without_intermediate_readback() {
        let gpu = GpuCompositor::new(16 * 1024 * 1024).unwrap();
        let (width, height) = (23, 19);
        let mut random = Random(0xaf41_234e_2c1a_778b);
        let mut layers = Vec::new();
        let mut expected = vec![[0.0; 4]; (width * height) as usize];
        for layer in 0..50 {
            let pixels: Vec<_> = (0..width * height).map(|_| random.pixel()).collect();
            let mode = BlendMode::ALL[layer % BlendMode::ALL.len()];
            let opacity = random.unit();
            for (result, pixel) in expected.iter_mut().zip(&pixels) {
                *result = blend(*result, *pixel, mode, opacity);
            }
            layers.push((gpu.upload(width, height, &pixels).unwrap(), mode, opacity));
        }
        let mut current = gpu.create_image(width, height).unwrap();
        let mut next = gpu.create_image(width, height).unwrap();
        let mut encoder = gpu.encoder();
        for (image, mode, opacity) in &layers {
            gpu.encode_blend(&mut encoder, &current, image, &next, *mode, *opacity)
                .unwrap();
            std::mem::swap(&mut current, &mut next);
        }
        gpu.submit(encoder);
        assert_pixels(
            &gpu.readback(&current).unwrap(),
            &expected,
            "50-layer stack",
        );
        assert!(gpu.peak_allocated_bytes() <= gpu.budget_bytes());
    }

    #[test]
    fn gpu_residency_ceiling_and_invalid_inputs_are_checked_before_commands() {
        assert!(GpuCompositor::new(0).is_err());
        let gpu = GpuCompositor::new(4096).unwrap();
        assert!(gpu.create_image(0, 1).is_err());
        assert!(gpu.create_image(u32::MAX, 1).is_err());
        assert!(gpu.create_image(100, 100).is_err());
        assert_eq!(gpu.allocated_bytes(), 0);
        assert!(gpu.upload(2, 2, &[[0.0; 4]]).is_err());
        assert!(gpu.upload_rgba8(1, 1, &[0; 3]).is_err());
        assert!(gpu.upload_rgba16f(u32::MAX, u32::MAX, &[]).is_err());
        let bottom = gpu.create_image(2, 2).unwrap();
        let wrong = gpu.create_image(3, 3).unwrap();
        let output = gpu.create_image(2, 2).unwrap();
        assert!(gpu.write(&bottom, &[]).is_err());
        let mut encoder = gpu.encoder();
        assert!(gpu
            .encode_blend(
                &mut encoder,
                &bottom,
                &bottom,
                &bottom,
                BlendMode::Normal,
                1.0
            )
            .is_err());
        assert!(gpu
            .encode_blend(
                &mut encoder,
                &bottom,
                &wrong,
                &output,
                BlendMode::Normal,
                1.0
            )
            .is_err());
        let input = gpu.upload_rgba8(2, 2, &[255; 16]).unwrap();
        assert!(gpu
            .encode_blend(
                &mut encoder,
                &bottom,
                &bottom,
                &input,
                BlendMode::Normal,
                1.0
            )
            .is_err());
        assert!(gpu.readback(&input).is_err());
        assert!(gpu.write(&input, &[[1.0; 4]; 4]).is_err());
        assert!(gpu.encode_clear(&mut encoder, &input).is_err());
        gpu.submit(encoder);
        gpu.wait();
        drop((bottom, wrong, output, input));
        assert_eq!(gpu.allocated_bytes(), 0);
        assert!(gpu.peak_allocated_bytes() <= 4096);
    }

    #[test]
    fn gpu_rgba16f_texture_reuse_matches_reference_and_charges_pending_staging() {
        let gpu = GpuCompositor::new(8192).unwrap();
        let mut image = Arc::new(gpu.upload_rgba16f(2, 1, &[0; 16]).unwrap());
        let image_id = image.id;
        assert_eq!(gpu.allocated_bytes(), 16 + 256);
        gpu.submit(gpu.encoder());
        gpu.wait();
        assert_eq!(gpu.allocated_bytes(), 16);
        let bottom = gpu.create_image(2, 1).unwrap();
        let output = gpu.create_image(2, 1).unwrap();
        for pixels in [
            [[0.125, 0.25, 0.5, 0.75], [0.25, 0.0, 0.125, 0.5]],
            [[0.0; 4], [1.0; 4]],
        ] {
            let bytes: Vec<_> = pixels
                .into_iter()
                .flatten()
                .flat_map(|component| half::f16::from_f32(component).to_bits().to_le_bytes())
                .collect();
            gpu.write_rgba16f(Arc::get_mut(&mut image).unwrap(), &bytes)
                .unwrap();
            assert_eq!(image.id, image_id);
            assert_eq!(gpu.allocated_bytes(), 16 + 32 + 32 + 256);
            let mut encoder = gpu.encoder();
            gpu.encode_blend(
                &mut encoder,
                &bottom,
                &image,
                &output,
                BlendMode::Normal,
                1.0,
            )
            .unwrap();
            gpu.submit(encoder);
            assert_pixels(
                &gpu.readback(&output).unwrap(),
                &pixels,
                "reused half-float texture",
            );
            assert_eq!(gpu.allocated_bytes(), 16 + 32 + 32);
        }
        let before = gpu.allocated_bytes();
        assert!(gpu.write_rgba16f(&image, &[0; 15]).is_err());
        assert!(gpu.write_rgba16f(&output, &[0; 16]).is_err());
        let other = GpuCompositor::new(8192).unwrap();
        assert!(other.write_rgba16f(&image, &[0; 16]).is_err());
        assert_eq!(gpu.allocated_bytes(), before);
        assert_eq!(other.allocated_bytes(), 0);

        let limited = GpuCompositor::new(264).unwrap();
        let image = limited.upload_rgba16f(1, 1, &[0; 8]).unwrap();
        assert!(limited.write_rgba16f(&image, &[0; 8]).is_err());
        assert_eq!(limited.allocated_bytes(), 264);
        limited.submit(limited.encoder());
        limited.wait();
        assert_eq!(limited.allocated_bytes(), 8);
        limited.write_rgba16f(&image, &[0; 8]).unwrap();
        limited.submit(limited.encoder());
        limited.wait();
        assert_eq!(limited.peak_allocated_bytes(), 264);
    }

    #[test]
    fn gpu_native_rgba8_and_rgba16f_uploads_match_cpu_and_clear_is_transparent() {
        let gpu = GpuCompositor::new(4096).unwrap();
        let bytes8 = [64_u8, 128, 32, 192];
        let pixel8 = bytes8.map(|n| f32::from(n) / 255.0);
        let pixel16: Pixel = [0.125, 0.25, 0.5, 0.75];
        let bytes16: Vec<_> = pixel16
            .into_iter()
            .flat_map(|n| half::f16::from_f32(n).to_bits().to_le_bytes())
            .collect();
        let bottom = gpu.upload_rgba8(1, 1, &bytes8).unwrap();
        let top = gpu.upload_rgba16f(1, 1, &bytes16).unwrap();
        let output = gpu.create_image(1, 1).unwrap();
        assert_eq!(
            (bottom.byte_len(), top.byte_len(), output.byte_len()),
            (4, 8, 16)
        );
        assert_eq!(bottom.format(), wgpu::TextureFormat::Rgba8Unorm);
        let mut encoder = gpu.encoder();
        gpu.encode_blend(
            &mut encoder,
            &bottom,
            &top,
            &output,
            BlendMode::Multiply,
            0.7,
        )
        .unwrap();
        gpu.submit(encoder);
        assert_pixels(
            &gpu.readback(&output).unwrap(),
            &[blend(pixel8, pixel16, BlendMode::Multiply, 0.7)],
            "native format upload",
        );
        let mut encoder = gpu.encoder();
        gpu.encode_clear(&mut encoder, &output).unwrap();
        gpu.submit(encoder);
        assert_eq!(gpu.readback(&output).unwrap(), [[0.0; 4]]);
    }
}
