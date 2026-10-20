use core::ops::Range;

use tickerlight_core::{Line, Scene};

const LINE_SIZE: usize = size_of::<Line>();
// `Line` is uploaded as is: 5 x f32 + 4 x u8, no padding.
const _: () = assert!(LINE_SIZE == 24);

/// Draws `Scene` line segments, anti-aliased, as one instanced draw call.
pub(crate) struct LineRenderer {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
}

impl LineRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("line shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("line.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32, 2 => Unorm8x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("line pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: LINE_SIZE as u64,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("line bind group"),
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

    pub(crate) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32), scene: &Scene) {
        let count = scene.lines.len();
        if count > self.capacity {
            self.capacity = count.next_power_of_two();
            self.instances = instance_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.globals, 0, &crate::rect_renderer::globals_bytes(size));
        queue.write_buffer(&self.instances, 0, line_bytes(&scene.lines));
    }

    /// Draws the lines of `range` (indices into `Scene::lines`).
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
        label: Some("line instances"),
        size: (capacity * LINE_SIZE) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn line_bytes(lines: &[Line]) -> &[u8] {
    // SAFETY: `Line` is `repr(C)`, made only of numbers, and has no padding (checked by the size
    // assertion above), so every byte is initialized.
    unsafe { core::slice::from_raw_parts(lines.as_ptr().cast(), core::mem::size_of_val(lines)) }
}
