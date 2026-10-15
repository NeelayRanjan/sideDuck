//! A real native swapchain benchmark, called once from a standalone main thread.
//!
//! The processing budget is host encoding + submission + `present()` call + the
//! sum of individual GPU compute/render pass durations. The sum is conservative
//! when GPU stages overlap. Wall-clock rendering, acquisition, and completion
//! waits are retained separately: native display scheduling can pace those to a
//! refresh interval. No measurement claims OS scanout or photon latency. Every
//! measured processing frame must satisfy the limit; none are discarded.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::Instant;

use serde::Serialize;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::gpu::{GpuCompositor, GpuImage, TIMESTAMP_CAPACITY};

/// Persistent scene state prepared before warmup. Encode each frame's real
/// compositing work into the supplied command encoder and return its output.
pub trait FrameProducer {
    fn encode_frame<'a>(
        &'a mut self,
        gpu: &GpuCompositor,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<&'a GpuImage, String>;
}

#[derive(Debug, Clone, Copy)]
pub struct PresentationConfig {
    pub width: u32,
    pub height: u32,
    pub warmup_frames: u32,
    pub measured_frames: u32,
    pub budget_bytes: u64,
    pub max_frame_ms: f64,
}

impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            width: 2560,
            height: 1600,
            warmup_frames: 8,
            measured_frames: 20,
            budget_bytes: 4 * 1024 * 1024 * 1024,
            max_frame_ms: 8.0,
        }
    }
}

impl PresentationConfig {
    fn validate(self) -> Result<(), String> {
        if self.width == 0
            || self.height == 0
            || self.measured_frames == 0
            || self.measured_frames > 10_000
            || self.warmup_frames > 10_000
            || self.budget_bytes < 16
            || !self.max_frame_ms.is_finite()
            || self.max_frame_ms <= 0.0
        {
            return Err("Invalid surface presentation benchmark configuration".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PresentationReport {
    pub adapter: String,
    pub physical_width: u32,
    pub physical_height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub sampling: String,
    pub acquisition_milliseconds: Vec<f64>,
    pub gpu_execution_milliseconds: Vec<f64>,
    pub processing_milliseconds: Vec<f64>,
    pub encoding_milliseconds: Vec<f64>,
    pub submission_milliseconds: Vec<f64>,
    pub present_call_milliseconds: Vec<f64>,
    pub completion_wait_milliseconds: Vec<f64>,
    pub end_to_end_milliseconds: Vec<f64>,
    pub max_end_to_end_milliseconds: f64,
    pub swapchain_format: String,
    pub present_mode: String,
    pub warmup_frames: u32,
    pub frame_milliseconds: Vec<f64>,
    pub max_milliseconds: f64,
    pub max_processing_milliseconds: f64,
    pub median_processing_milliseconds: f64,
    pub budget_metric: String,
    pub end_to_end_budget_met: bool,
    pub median_milliseconds: f64,
    pub budget_milliseconds: f64,
    pub peak_owned_image_bytes: u64,
    pub timing_scope: String,
}

type Prepare = Box<dyn FnOnce(&GpuCompositor) -> Result<Box<dyn FrameProducer>, String>>;

/// Must run on the native main thread, with no other winit event loop in this
/// process. Run separate CLI processes for separate viewport configurations.
pub fn run_render_benchmark(
    config: PresentationConfig,
    prepare: impl FnOnce(&GpuCompositor) -> Result<Box<dyn FrameProducer>, String> + 'static,
) -> Result<PresentationReport, String> {
    config.validate()?;
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = PresentationApp {
        config,
        prepare: Some(Box::new(prepare)),
        state: None,
        result: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())?;
    app.result
        .unwrap_or_else(|| Err("Surface benchmark exited before measuring frames".into()))
}

#[derive(Debug)]
struct FramePhases {
    acquisition: f64,
    gpu_execution: f64,
    processing: f64,
    encoding: f64,
    submission: f64,
    present: f64,
    completion_wait: f64,
    end_to_end: f64,
}

struct SurfaceState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuCompositor,
    producer: Box<dyn FrameProducer>,
    pipeline: PresentationPipeline,
    timer: FrameTimer,
    source_size: [u32; 2],
    phases: Vec<FramePhases>,
    format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    frames: u32,
    measured: Vec<f64>,
}

struct PresentationApp {
    config: PresentationConfig,
    prepare: Option<Prepare>,
    state: Option<SurfaceState>,
    result: Option<Result<PresentationReport, String>>,
}

impl PresentationApp {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("sideDuck Render Benchmark")
                        .with_inner_size(PhysicalSize::new(self.config.width, self.config.height))
                        .with_resizable(false),
                )
                .map_err(|error| error.to_string())?,
        );
        check_dimensions(window.inner_size(), self.config)?;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| error.to_string())?;
        let gpu = GpuCompositor::new_for_surface(instance, &surface, self.config.budget_bytes)?;
        let capabilities = surface.get_capabilities(gpu.adapter());
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .ok_or("The surface does not support an sRGB swapchain")?;
        let present_mode = [
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Fifo,
        ]
        .into_iter()
        .find(|mode| capabilities.present_modes.contains(mode))
        .ok_or("The surface has no usable presentation mode")?;
        let alpha_mode = capabilities
            .alpha_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
            .or_else(|| capabilities.alpha_modes.first().copied())
            .ok_or("The surface has no supported alpha mode")?;
        surface.configure(
            gpu.device(),
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: self.config.width,
                height: self.config.height,
                present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            },
        );
        let pipeline =
            presentation_pipeline(gpu.device(), format, self.config.width, self.config.height)?;
        let prepare = self
            .prepare
            .take()
            .ok_or("Surface scene was already prepared")?;
        let producer = prepare(&gpu)?;
        let timer = FrameTimer::new(&gpu)?;
        gpu.submit(gpu.encoder());
        gpu.wait();
        window.request_redraw();
        self.state = Some(SurfaceState {
            window,
            surface,
            gpu,
            producer,
            pipeline,
            timer,
            source_size: [0, 0],
            phases: Vec::with_capacity(self.config.measured_frames as usize),
            format,
            present_mode,
            frames: 0,
            measured: Vec::with_capacity(self.config.measured_frames as usize),
        });
        Ok(())
    }

    fn draw(&mut self) -> Result<Option<PresentationReport>, String> {
        let state = self
            .state
            .as_mut()
            .ok_or("Surface has not been initialized")?;
        check_dimensions(state.window.inner_size(), self.config)?;
        let start = Instant::now();
        let surface_frame = state
            .surface
            .get_current_texture()
            .map_err(|error| format!("Surface acquisition failed: {error}"))?;
        let acquisition_ms = start.elapsed().as_secs_f64() * 1000.0;
        let render_start = Instant::now();
        let view = surface_frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = state.gpu.encoder();
        state.timer.begin(&state.gpu)?;
        let image = state.producer.encode_frame(&state.gpu, &mut encoder)?;
        state.source_size = [image.width(), image.height()];
        let render_timestamp = state.gpu.timestamp_pair()?;
        state.pipeline.encode(
            &state.gpu,
            &mut encoder,
            image,
            &view,
            render_timestamp
                .as_ref()
                .map(|(query, index)| (query.as_ref(), *index)),
        )?;
        state.gpu.end_timestamp_capture()?;

        state.window.pre_present_notify();
        let encoding_ms = render_start.elapsed().as_secs_f64() * 1000.0;
        let submit_start = Instant::now();
        let submission_index = state.gpu.submit(encoder);
        let submission_ms = submit_start.elapsed().as_secs_f64() * 1000.0;
        let present_start = Instant::now();
        surface_frame.present();
        let present_ms = present_start.elapsed().as_secs_f64() * 1000.0;
        // Wait for this frame's rendering submission. The phase report retains
        // any scheduling delay instead of treating a fast present call as GPU
        // completion or dropping slow frames from the budget assertion.
        let wait_start = Instant::now();
        state
            .gpu
            .device()
            .poll(wgpu::Maintain::WaitForSubmissionIndex(submission_index));
        let wait_ms = wait_start.elapsed().as_secs_f64() * 1000.0;
        let milliseconds = render_start.elapsed().as_secs_f64() * 1000.0;
        let total_ms = start.elapsed().as_secs_f64() * 1000.0;
        let gpu_execution_ms = state.timer.read(&state.gpu)?;
        let processing_ms = encoding_ms + submission_ms + present_ms + gpu_execution_ms;
        if state.frames >= self.config.warmup_frames {
            state.measured.push(milliseconds);
            state.phases.push(FramePhases {
                gpu_execution: gpu_execution_ms,
                processing: processing_ms,
                acquisition: acquisition_ms,
                encoding: encoding_ms,
                submission: submission_ms,
                present: present_ms,
                completion_wait: wait_ms,
                end_to_end: total_ms,
            });
        }
        state.frames += 1;
        if state.measured.len() != self.config.measured_frames as usize {
            return Ok(None);
        }
        let processing: Vec<_> = state.phases.iter().map(|phase| phase.processing).collect();
        let (max_processing, median_processing) =
            measured_statistics(&processing, self.config.max_frame_ms).map_err(|error| {
                format!(
                    "{error}; mode {:?}; phase timings {:?}",
                    state.present_mode, state.phases
                )
            })?;
        let (max, median) = measured_statistics(&state.measured, f64::INFINITY)?;
        let max_end_to_end = state
            .phases
            .iter()
            .map(|phase| phase.end_to_end)
            .fold(0.0, f64::max);
        let report = PresentationReport {
            adapter: state.gpu.adapter_name().to_owned(), physical_width: self.config.width,
            physical_height: self.config.height, source_width: state.source_size[0], source_height: state.source_size[1],
            sampling: "exact texel load at 1:1; centered bilinear aspect fit otherwise; white letterbox".into(),
            acquisition_milliseconds: state.phases.iter().map(|p| p.acquisition).collect(),
            gpu_execution_milliseconds: state.phases.iter().map(|p| p.gpu_execution).collect(),
            processing_milliseconds: state.phases.iter().map(|p| p.processing).collect(),
            encoding_milliseconds: state.phases.iter().map(|p| p.encoding).collect(),
            submission_milliseconds: state.phases.iter().map(|p| p.submission).collect(),
            present_call_milliseconds: state.phases.iter().map(|p| p.present).collect(),
            completion_wait_milliseconds: state.phases.iter().map(|p| p.completion_wait).collect(),
            end_to_end_milliseconds: state.phases.iter().map(|p| p.end_to_end).collect(),
            max_end_to_end_milliseconds: state.phases.iter().map(|p| p.end_to_end).fold(0.0, f64::max),
            swapchain_format: format!("{:?}", state.format),
            present_mode: format!("{:?}", state.present_mode), warmup_frames: self.config.warmup_frames,
            frame_milliseconds: state.measured.clone(), max_milliseconds: max, median_milliseconds: median,
            max_processing_milliseconds: max_processing, median_processing_milliseconds: median_processing,
            budget_metric: "host encoding + queue submission + present call + sum of GPU compute/render pass durations".into(),
            end_to_end_budget_met: max_end_to_end < self.config.max_frame_ms,
            budget_milliseconds: self.config.max_frame_ms, peak_owned_image_bytes: state.gpu.peak_allocated_bytes(),
            timing_scope: "processing budget: host encoding + submit + present call + sum of GPU pass durations; wall rendering, acquisition, completion wait and end-to-end retained separately; OS scanout excluded".into(),
        };
        Ok(Some(report))
    }
}

impl ApplicationHandler for PresentationApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() && self.result.is_none() {
            if let Err(error) = self.initialize(event_loop) {
                self.result = Some(Err(error));
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.result.is_some()
            || self
                .state
                .as_ref()
                .is_none_or(|state| state.window.id() != window_id)
        {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => match self.draw() {
                Ok(Some(report)) => {
                    self.result = Some(Ok(report));
                    event_loop.exit();
                }
                Ok(None) => {}
                Err(error) => {
                    self.result = Some(Err(error));
                    event_loop.exit();
                }
            },
            WindowEvent::CloseRequested => {
                self.result = Some(Err(
                    "Surface benchmark window closed before completion".into()
                ));
                event_loop.exit();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

fn check_dimensions(size: PhysicalSize<u32>, config: PresentationConfig) -> Result<(), String> {
    if size.width != config.width || size.height != config.height {
        return Err(format!(
            "Surface realized {}x{} physical pixels; benchmark requires {}x{}",
            size.width, size.height, config.width, config.height
        ));
    }
    Ok(())
}

fn measured_statistics(values: &[f64], budget: f64) -> Result<(f64, f64), String> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("Missing or invalid surface frame timings".into());
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let max = sorted[sorted.len() - 1];
    let median = sorted[sorted.len() / 2];
    if max >= budget {
        return Err(format!("Surface render-processing budget exceeded: maximum {max:.3} ms >= {budget:.3} ms; all measured frames: {values:?}"));
    }
    Ok((max, median))
}

/// Sum individual compute/render pass intervals. Cross-encoder timestamps may
/// overlap on tiled GPUs; summing each pass is conservative and does not include
/// time waiting for queue scheduling. Telemetry readback follows completion.
struct FrameTimer {
    query: Arc<wgpu::QuerySet>,
    next: Arc<AtomicU32>,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl FrameTimer {
    fn new(gpu: &GpuCompositor) -> Result<Self, String> {
        if !gpu
            .device()
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
        {
            return Err("Presentation benchmark requires GPU timestamp queries".into());
        }
        Ok(Self {
            query: Arc::new(gpu.device().create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame execution timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: TIMESTAMP_CAPACITY,
            })),
            next: Arc::new(AtomicU32::new(0)),
            resolved: gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("resolved frame timestamps"),
                size: u64::from(TIMESTAMP_CAPACITY) * 8,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame timestamp telemetry"),
                size: u64::from(TIMESTAMP_CAPACITY) * 8,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        })
    }

    fn begin(&mut self, gpu: &GpuCompositor) -> Result<(), String> {
        let query = Arc::new(gpu.device().create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("fresh frame timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: TIMESTAMP_CAPACITY,
        }));
        gpu.begin_timestamp_capture(query.clone(), self.next.clone())?;
        self.query = query;
        self.next.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let count = self.next.load(Ordering::Relaxed);
        encoder.resolve_query_set(&self.query, 0..count, &self.resolved, 0);
        encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, u64::from(count) * 8);
    }

    fn read(&self, gpu: &GpuCompositor) -> Result<f64, String> {
        // Metal on macOS 26 can return incomplete counters when resolution is
        // committed before sampling completes (wgpu issue #9414). Resolve in
        // a separate submission after host-observed completion on every backend.
        gpu.wait();
        let mut encoder = gpu.encoder();
        self.resolve(&mut encoder);
        gpu.submit(encoder);
        let (send, receive) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        gpu.device().poll(wgpu::Maintain::Wait);
        receive
            .recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let bytes = self.readback.slice(..).get_mapped_range();
        let count = self.next.load(Ordering::Relaxed) as usize;
        let result =
            timestamp_milliseconds(&bytes[..count * 8], gpu.queue().get_timestamp_period());
        drop(bytes);
        self.readback.unmap();
        result
    }
}

fn timestamp_milliseconds(bytes: &[u8], period: f32) -> Result<f64, String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        return Err("Incomplete GPU timestamp pairs".into());
    }
    let mut elapsed = 0u64;
    for (index, pair) in bytes.chunks_exact(16).enumerate() {
        let ticks: [u64; 2] = bytemuck::pod_read_unaligned(pair);
        if ticks[0] == 0 || ticks[1] == 0 {
            return Err(format!(
                "GPU did not write both pass timestamps: pair {index} {ticks:?}"
            ));
        }
        let duration = ticks[1]
            .checked_sub(ticks[0])
            .ok_or_else(|| format!("GPU pass timestamps ran backwards: pair {index} {ticks:?}"))?;
        elapsed = elapsed
            .checked_add(duration)
            .ok_or("GPU timestamp duration overflow")?;
    }
    let milliseconds = elapsed as f64 * f64::from(period) / 1_000_000.0;
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        return Err("GPU timestamp duration was not positive and finite".into());
    }
    Ok(milliseconds)
}

struct PresentationPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    dimensions: wgpu::Buffer,
}

impl PresentationPipeline {
    /// The same draw path targets both the native swapchain and parity-test
    /// textures. The source remains linear premultiplied RGBA32Float.
    fn encode(
        &self,
        gpu: &GpuCompositor,
        encoder: &mut wgpu::CommandEncoder,
        image: &GpuImage,
        target: &wgpu::TextureView,
        timestamp_query: Option<(&wgpu::QuerySet, u32)>,
    ) -> Result<(), String> {
        gpu.validate_image(image)?;
        if image.format() != wgpu::TextureFormat::Rgba32Float {
            return Err("Presented image must use RGBA32Float".into());
        }
        let bindings = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("presented compositing frame"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(image.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.dimensions.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("native surface presentation"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamp_query.map(|(query_set, index)| {
                wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(index),
                    end_of_pass_write_index: Some(index + 1),
                }
            }),
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

fn presentation_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<PresentationPipeline, String> {
    if width == 0 || height == 0 || !format.is_srgb() {
        return Err("Presentation requires nonzero dimensions and an sRGB target".into());
    }
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("native presentation shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
    });
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("presentation input binding"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            },
        ],
    });
    let dimensions = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("presentation physical dimensions"),
        contents: bytemuck::cast_slice(&[width as f32, height as f32, 0.0, 0.0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("presentation layout"),
        bind_group_layouts: &[&bind_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("native image presentation"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    });
    if let Some(error) = pollster::block_on(device.pop_error_scope()) {
        return Err(format!("Presentation pipeline creation failed: {error}"));
    }
    Ok(PresentationPipeline {
        pipeline,
        bind_layout,
        dimensions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_budget_requires_valid_dimensions_frames_and_finite_limit() {
        let config = PresentationConfig::default();
        assert!(config.validate().is_ok());
        for invalid in [
            PresentationConfig { width: 0, ..config },
            PresentationConfig {
                height: 0,
                ..config
            },
            PresentationConfig {
                measured_frames: 0,
                ..config
            },
            PresentationConfig {
                measured_frames: 10_001,
                ..config
            },
            PresentationConfig {
                warmup_frames: 10_001,
                ..config
            },
            PresentationConfig {
                budget_bytes: 15,
                ..config
            },
            PresentationConfig {
                max_frame_ms: 0.0,
                ..config
            },
            PresentationConfig {
                max_frame_ms: f64::NAN,
                ..config
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        assert!(check_dimensions(PhysicalSize::new(2560, 1600), config).is_ok());
        assert!(check_dimensions(PhysicalSize::new(1280, 800), config).is_err());
        assert_eq!(
            measured_statistics(&[3.0, 1.0, 2.0], 8.0).unwrap(),
            (3.0, 2.0)
        );
        // A fast median never hides a single over-budget frame; equality fails.
        assert!(measured_statistics(&[1.0, 1.0, 8.0], 8.0).is_err());
        assert!(measured_statistics(&[], 8.0).is_err());
        assert!(measured_statistics(&[f64::NAN], 8.0).is_err());
        assert!(measured_statistics(&[-1.0], 8.0).is_err());
    }

    #[test]
    fn timestamp_budget_rejects_zero_unordered_missing_or_nonfinite_measurements() {
        assert_eq!(
            timestamp_milliseconds(bytemuck::cast_slice(&[100_u64, 200, 50, 250]), 1000.0).unwrap(),
            0.3
        );
        for ticks in [[0_u64, 100], [100, 0], [101, 100], [100, 100]] {
            assert!(timestamp_milliseconds(bytemuck::cast_slice(&ticks), 1.0).is_err());
        }
        assert!(timestamp_milliseconds(&[], 1.0).is_err());
        assert!(timestamp_milliseconds(&[0; 8], 1.0).is_err());
        assert!(timestamp_milliseconds(bytemuck::cast_slice(&[1_u64, 2]), f32::NAN).is_err());
        assert!(
            timestamp_milliseconds(bytemuck::cast_slice(&[1_u64, u64::MAX, 1, u64::MAX]), 1.0)
                .is_err()
        );
    }

    fn reference_pixel(
        pixels: &[[f32; 4]],
        source: [u32; 2],
        target: [u32; 2],
        x: u32,
        y: u32,
    ) -> [u8; 4] {
        let scale = (target[0] as f32 / source[0] as f32).min(target[1] as f32 / source[1] as f32);
        let local = [
            x as f32 + 0.5 - (target[0] as f32 - source[0] as f32 * scale) * 0.5,
            y as f32 + 0.5 - (target[1] as f32 - source[1] as f32 * scale) * 0.5,
        ];
        if local
            .iter()
            .enumerate()
            .any(|(i, v)| *v < 0.0 || *v >= source[i] as f32 * scale)
        {
            return [255; 4];
        }
        let location = [local[0] / scale - 0.5, local[1] / scale - 0.5];
        let base = [location[0].floor() as i32, location[1].floor() as i32];
        let fraction = [
            location[0] - location[0].floor(),
            location[1] - location[1].floor(),
        ];
        let mut color = [0.0; 4];
        // An independent weighted sum of four neighbors, rather than the
        // shader's nested mixes, preserves premultiplication while filtering.
        for dy in 0..2 {
            for dx in 0..2 {
                let sx = (base[0] + dx).clamp(0, source[0] as i32 - 1) as usize;
                let sy = (base[1] + dy).clamp(0, source[1] as i32 - 1) as usize;
                let weight = if dx == 0 {
                    1.0 - fraction[0]
                } else {
                    fraction[0]
                } * if dy == 0 {
                    1.0 - fraction[1]
                } else {
                    fraction[1]
                };
                for (channel, value) in color.iter_mut().enumerate() {
                    *value += pixels[sy * source[0] as usize + sx][channel] * weight;
                }
            }
        }
        let mut encoded = [255; 4];
        for channel in 0..3 {
            encoded[channel] = (crate::color::linear_to_srgb(color[channel] + 1.0 - color[3])
                * 255.0)
                .round() as u8;
        }
        encoded
    }

    #[test]
    fn presentation_per_pixel_cpu_reference_parity_at_one_to_one_and_aspect_fit() {
        let gpu = GpuCompositor::new(8 * 1024 * 1024).unwrap();
        let pixels = [
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.25, 0.0, 0.25],
            [0.0; 4],
            [0.0, 0.0, 1.0, 1.0],
            [0.2, 0.4, 0.6, 0.8],
            [0.125, 0.125, 0.125, 0.5],
        ];
        let image = gpu.upload(3, 2, &pixels).unwrap();
        let mut timer = FrameTimer::new(&gpu).unwrap();
        let transparent = gpu.create_image(3, 2).unwrap();
        let composed = gpu.create_image(3, 2).unwrap();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        for [width, height] in [[3, 2], [9, 9], [9, 3], [2, 2], [1, 1]] {
            let pipeline = presentation_pipeline(gpu.device(), format, width, height).unwrap();
            let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("headless presentation parity target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = gpu.encoder();
            timer.begin(&gpu).unwrap();
            assert!(timer.begin(&gpu).is_err());
            gpu.encode_clear(&mut encoder, &transparent).unwrap();
            gpu.encode_blend(
                &mut encoder,
                &image,
                &transparent,
                &composed,
                crate::blend::BlendMode::Normal,
                1.0,
            )
            .unwrap();
            let timestamp = gpu.timestamp_pair().unwrap().unwrap();
            pipeline
                .encode(
                    &gpu,
                    &mut encoder,
                    &composed,
                    &target_view,
                    Some((timestamp.0.as_ref(), timestamp.1)),
                )
                .unwrap();
            gpu.end_timestamp_capture().unwrap();

            let row_bytes = (width * 4).div_ceil(256) * 256;
            let buffer = gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("presentation parity readback"),
                size: u64::from(row_bytes * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row_bytes),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            gpu.submit(encoder);
            let (send, receive) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    send.send(result).unwrap();
                });
            gpu.wait();
            receive.recv().unwrap().unwrap();
            let actual = buffer.slice(..).get_mapped_range();
            for y in 0..height {
                for x in 0..width {
                    let expected = reference_pixel(&pixels, [3, 2], [width, height], x, y);
                    let offset = (y * row_bytes + x * 4) as usize;
                    for channel in 0..4 {
                        // Hardware sRGB conversion/8-bit quantization permits
                        // one code value; spatial sampling and alpha stay exact.
                        assert!(
                            actual[offset + channel].abs_diff(expected[channel]) <= 1,
                            "{width}x{height} ({x},{y}) channel {channel}: {} != {}",
                            actual[offset + channel],
                            expected[channel]
                        );
                    }
                }
            }
            drop(actual);
            buffer.unmap();
            assert!(timer.read(&gpu).unwrap() > 0.0);
            let invalid_image = gpu.upload_rgba8(1, 1, &[0; 4]).unwrap();
            assert!(pipeline
                .encode(&gpu, &mut gpu.encoder(), &invalid_image, &target_view, None)
                .is_err());
        }
        assert!(presentation_pipeline(gpu.device(), format, 0, 1).is_err());
        assert!(
            presentation_pipeline(gpu.device(), wgpu::TextureFormat::Rgba8Unorm, 1, 1).is_err()
        );
    }
}
