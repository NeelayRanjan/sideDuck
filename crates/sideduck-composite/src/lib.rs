//! Linear-premultiplied compositing, bounded derived caches, and viewport mips.

pub mod blend;
pub mod cache;
pub mod color;
pub mod gpu;
pub mod mip_store;
pub mod present;
pub mod pyramid;
pub mod raster;

pub use blend::{blend, composite_stack, BlendMode, Pixel};

/// Establishes a real headless device; failure is never replaced with a mock pass.
pub fn gpu_probe() -> Result<String, String> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok_or("No headless GPU adapter available")?;
        let info = adapter.get_info();
        let (device, _) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .map_err(|e| e.to_string())?;
        device.poll(wgpu::Maintain::Wait);
        Ok(format!("{} ({:?})", info.name, info.backend))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn headless_wgpu_device_initializes() {
        let adapter = super::gpu_probe().expect("headless GPU is required for this gate");
        println!("GPU: {adapter}");
        assert!(!adapter.is_empty());
    }
}
