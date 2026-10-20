use tickerlight_core::Scene;

use crate::Renderer;

/// A GPU device without a window.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn new_headless() -> Result<Self, String> {
        // No GL: probing it wakes every GPU driver, which on a hybrid laptop with a stalled discrete GPU waits for
        // a kernel timeout. The primary backends have a software rasterizer of their own.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // Offscreen frames go to the software rasterizer when there is one: the pixels are the same on every
        // machine, and the user's GPU is left alone (on hybrid laptops a hardware driver may lose the device).
        let software = wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        };
        let adapter = pollster::block_on(instance.request_adapter(&software))
            .or_else(|_| pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())))
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| format!("cannot open GPU device: {e}"))?;
        Ok(Self { device, queue })
    }
}

/// Renders the scene into an offscreen texture and returns RGBA8 pixels, row by row.
pub fn render_to_rgba(gpu: &Gpu, scene: &Scene, width: u32, height: u32) -> Vec<u8> {
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless target"),
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Renderer::new(&gpu.device, format).render(&gpu.device, &gpu.queue, &view, (width, height), scene);

    // Rows in a texture-to-buffer copy must be padded to 256 bytes.
    let row = width as usize * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("headless readback"),
        size: (padded * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("GPU poll failed");
    rx.recv().unwrap().expect("cannot map readback buffer");

    let mapped = buffer.slice(..).get_mapped_range().expect("cannot read mapped range");
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in mapped.chunks(padded) {
        pixels.extend_from_slice(&line[..row]);
    }
    pixels
}
