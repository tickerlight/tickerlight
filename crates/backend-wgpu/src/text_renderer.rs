use core::ops::Range;

use tickerlight_core::{Color, Scene};
use tickerlight_text::{Atlas, Font, GlyphQuad, layout};

const ATLAS_SIZE: u32 = 1024;

/// A glyph quad with its color, uploaded as is: 8 x f32 + 4 x u8, no padding.
#[derive(Clone, Copy)]
#[repr(C)]
struct GlyphInstance {
    quad: GlyphQuad,
    color: Color,
}
const INSTANCE_SIZE: usize = size_of::<GlyphInstance>();
const _: () = assert!(INSTANCE_SIZE == 36);

/// Draws `Scene` text: glyphs are rasterized into an atlas texture and drawn as one instanced call.
pub(crate) struct TextRenderer {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    /// For each scene text, where its glyphs end in the instance buffer.
    glyph_ends: Vec<usize>,
    atlas: Atlas,
    uploaded_version: u64,
    texture: wgpu::Texture,
    /// Reused between frames.
    quads: Vec<GlyphQuad>,
    staging: Vec<GlyphInstance>,
}

impl TextRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Unorm8x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_SIZE as u64,
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
            label: Some("text globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Glyphs sit on whole pixels at their natural size, so nearest is exact.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text bind group"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let capacity = 256;
        Self {
            pipeline,
            globals,
            bind_group,
            instances: instance_buffer(device, capacity),
            capacity,
            glyph_ends: Vec::new(),
            atlas: Atlas::new(ATLAS_SIZE),
            uploaded_version: u64::MAX,
            texture,
            quads: Vec::new(),
            staging: Vec::new(),
        }
    }

    /// Lays out the scene text, uploads glyphs and instances. Call before [`draw`](Self::draw).
    pub(crate) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32), scene: &Scene) {
        let font = Font::ui();
        self.staging.clear();
        self.glyph_ends.clear();
        for text in &scene.texts {
            self.quads.clear();
            layout(
                font,
                &mut self.atlas,
                &text.text,
                text.size,
                (text.x, text.y),
                text.align,
                &mut self.quads,
            );
            self.staging.extend(self.quads.iter().map(|&quad| GlyphInstance {
                quad,
                color: text.color,
            }));
            self.glyph_ends.push(self.staging.len());
        }

        if self.atlas.version() != self.uploaded_version {
            self.uploaded_version = self.atlas.version();
            queue.write_texture(
                self.texture.as_image_copy(),
                self.atlas.pixels(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ATLAS_SIZE),
                    rows_per_image: Some(ATLAS_SIZE),
                },
                wgpu::Extent3d {
                    width: ATLAS_SIZE,
                    height: ATLAS_SIZE,
                    depth_or_array_layers: 1,
                },
            );
        }
        if self.staging.len() > self.capacity {
            self.capacity = self.staging.len().next_power_of_two();
            self.instances = instance_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.globals, 0, &crate::rect_renderer::globals_bytes(size));
        queue.write_buffer(&self.instances, 0, instance_bytes(&self.staging));
    }

    /// Draws the texts of `range` (indices into `Scene::texts`).
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let glyph_end = |text: usize| if text == 0 { 0 } else { self.glyph_ends[text - 1] };
        let glyphs = glyph_end(range.start)..glyph_end(range.end);
        if !glyphs.is_empty() {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, glyphs.start as u32..glyphs.end as u32);
        }
    }
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glyph instances"),
        size: (capacity * INSTANCE_SIZE) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn instance_bytes(instances: &[GlyphInstance]) -> &[u8] {
    // SAFETY: `GlyphInstance` is `repr(C)`, made only of numbers, and has no padding
    // (checked by the size assertion above), so every byte is initialized.
    unsafe { core::slice::from_raw_parts(instances.as_ptr().cast(), core::mem::size_of_val(instances)) }
}
