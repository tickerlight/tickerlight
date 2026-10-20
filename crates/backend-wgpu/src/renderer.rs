use tickerlight_core::Scene;

use crate::line_renderer::LineRenderer;
use crate::rect_renderer::RectRenderer;
use crate::text_renderer::TextRenderer;

/// Draws a whole [`Scene`]: the background, then its layers bottom to top (in each: rects, lines, text).
///
/// Colors are written as they are, so the target must not be an sRGB format,
/// otherwise the GPU would brighten them a second time.
pub struct Renderer {
    rects: RectRenderer,
    lines: LineRenderer,
    text: TextRenderer,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            rects: RectRenderer::new(device, target_format),
            lines: LineRenderer::new(device, target_format),
            text: TextRenderer::new(device, target_format),
        }
    }

    /// Clears `target` with the scene background and draws the scene onto it.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        size: (u32, u32),
        scene: &Scene,
    ) {
        self.rects.prepare(device, queue, size, scene);
        self.lines.prepare(device, queue, size, scene);
        self.text.prepare(device, queue, size, scene);

        let bg = scene.background;
        let clear = wgpu::Color {
            r: bg.r as f64 / 255.0,
            g: bg.g as f64 / 255.0,
            b: bg.b as f64 / 255.0,
            a: bg.a as f64 / 255.0,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scene encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (width, height) = size;
            for layer in scene.layers() {
                // Layers are cut to their clip rectangle (a pane's plot, say), whole pixels, inside the target.
                let (x, y, w, h) = match layer.clip {
                    Some(c) => {
                        let x0 = (c.x.round().max(0.0) as u32).min(width);
                        let y0 = (c.y.round().max(0.0) as u32).min(height);
                        let x1 = ((c.x + c.w).round().max(0.0) as u32).min(width);
                        let y1 = ((c.y + c.h).round().max(0.0) as u32).min(height);
                        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
                    }
                    None => (0, 0, width, height),
                };
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                self.rects.draw(&mut pass, layer.rects);
                self.lines.draw(&mut pass, layer.lines);
                self.text.draw(&mut pass, layer.texts);
            }
        }
        queue.submit([encoder.finish()]);
    }
}
