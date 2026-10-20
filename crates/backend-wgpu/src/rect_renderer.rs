use core::ops::Range;

use tickerlight_core::{Rect, Scene};

const RECT_SIZE: usize = size_of::<Rect>();
// `Rect` is uploaded as is: 4 x f32 + 4 x u8 + 2 x f32 + 4 x u8 + f32 + u32, no padding.
const _: () = assert!(RECT_SIZE == 40);

/// Draws `Scene` rectangles as one instanced draw call.
pub(crate) struct RectRenderer {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
}

impl RectRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("rect.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Unorm8x4, 2 => Float32, 3 => Float32, 4 => Unorm8x4, 5 => Float32, 6 => Uint32
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: RECT_SIZE as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // The shader gives premultiplied colors, so a gradient can fade out without darkening.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rect bind group"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let capacity = 1024;
        Self {
            pipeline,
            globals,
            bind_group,
            instances: instance_buffer(device, capacity),
            capacity,
        }
    }

    /// Uploads the scene rectangles and the viewport size. Call before [`draw`](Self::draw).
    pub(crate) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32), scene: &Scene) {
        let count = scene.rects.len();
        if count > self.capacity {
            self.capacity = count.next_power_of_two();
            self.instances = instance_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.globals, 0, &globals_bytes(size));
        queue.write_buffer(&self.instances, 0, rect_bytes(&scene.rects));
    }

    /// Draws the rects of `range` (indices into `Scene::rects`).
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<usize>) {
        if !range.is_empty() {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, range.start as u32..range.end as u32);
        }
    }
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rect instances"),
        size: (capacity * RECT_SIZE) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(crate) fn globals_bytes((w, h): (u32, u32)) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&(w as f32).to_ne_bytes());
    bytes[4..8].copy_from_slice(&(h as f32).to_ne_bytes());
    bytes
}

fn rect_bytes(rects: &[Rect]) -> &[u8] {
    // SAFETY: `Rect` is `repr(C)` with all fields plain numbers and no padding
    // (checked by the size assertion above), so every byte is initialized.
    unsafe { core::slice::from_raw_parts(rects.as_ptr().cast(), core::mem::size_of_val(rects)) }
}
