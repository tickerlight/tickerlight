use crate::plugin::{names, order};
use crate::{Chart, Color, DrawCtx, Layer, Plugin};

/// Volume as a histogram along the bottom of the plot, under the candles. Up bars and down bars
/// take the candle colors, translucent so the grid and candles stay readable.
#[derive(Clone, Copy)]
pub struct VolumeLayer {
    /// Share of the plot height the tallest bar takes.
    pub height: f32,
    /// Opacity of the bars, 0 to 1.
    pub opacity: f32,
    /// Bars this far apart (logical pixels) or more are not drawn: a footprint then shows the volume of each.
    pub hidden_from: Option<f32>,
}

impl Default for VolumeLayer {
    fn default() -> Self {
        Self {
            height: 0.2,
            opacity: 0.45,
            hidden_from: None,
        }
    }
}

impl Layer for VolumeLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let hidden = self
            .hidden_from
            .is_some_and(|from| ctx.time.bar_spacing as f32 >= ctx.px(from));
        if ctx.view.is_empty() || hidden {
            return;
        }
        let (volume, open, close) = (ctx.view.volume(), ctx.view.open(), ctx.view.close());
        let len = ctx.view.len();
        // Below one pixel per bar, bars are merged like the candles are: the tallest volume of a
        // group stands for it, so its height does not depend on how far out the chart is zoomed.
        let group = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let range = ctx.visible.start.min(len)..ctx.visible.end.min(len);
        let mut columns = Vec::new();
        let mut start = range.start / group * group;
        while start < range.end {
            let end = (start + group).min(len);
            let tallest = volume[start..end].iter().copied().fold(0.0, f64::max);
            columns.push((start, end - start, tallest, close[end - 1] >= open[start]));
            start = end;
        }
        let top = columns.iter().map(|c| c.2).fold(0.0, f64::max);
        if top <= 0.0 || !top.is_finite() {
            return;
        }

        let style = &ctx.style.candle;
        let with_opacity = |c: Color| Color {
            a: (self.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            ..c
        };
        let (up, down) = (with_opacity(style.up), with_opacity(style.down));
        let max_h = ctx.plot.1 * self.height;
        let gap = ctx.px(1.0);
        for (first, count, tallest, rising) in columns {
            let slot = count as f64 * ctx.time.bar_spacing;
            let w = ((slot * style.body_ratio).round() as f32).max(ctx.line());
            let x = (ctx.time.x_of(first as f64 + count as f64 / 2.0) as f32 - w / 2.0).round();
            let h = (max_h * (tallest / top) as f32).max(gap).round();
            let color = if rising { up } else { down };
            ctx.scene.fill(x, ctx.plot.1 - h, w, h, color);
        }
    }
}

/// Adds the volume histogram; `DefaultPlugins` has it. Replace or remove the layer named
/// [`names::VOLUME`] to change it.
#[derive(Default)]
pub struct VolumePlugin(pub VolumeLayer);

impl Plugin for VolumePlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer(names::VOLUME, order::VOLUME, self.0);
    }
}
