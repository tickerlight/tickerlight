use core::ops::Range;

use crate::plugin::{names, order};
use crate::{CandleView, Chart, Color, DrawCtx, Layer, Plugin, Resources, aggregate};

/// The close price as a line, instead of candles. Replaces the candles layer:
/// `chart.remove_layer(names::CANDLES); chart.add_plugin(LineSeries::new(color))`. It takes the place of
/// the candles for autoscale, too.
#[derive(Clone, Copy)]
pub struct LineSeries {
    pub color: Color,
    /// Line width in logical pixels.
    pub width: f32,
}

impl LineSeries {
    pub fn new(color: Color) -> Self {
        Self { color, width: 2.0 }
    }
}

impl Layer for LineSeries {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if ctx.view.is_empty() {
            return;
        }
        // One point per pixel at most: merge bars when they are narrower than a pixel.
        let group = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let columns = aggregate(ctx.view, ctx.visible.clone(), group);
        let width = ctx.px(self.width);
        let mut previous = None;
        for i in 0..columns.len() {
            let center = columns.first[i] as f64 + columns.count[i] as f64 / 2.0;
            let point = (ctx.time.x_of(center) as f32, ctx.price.y_of(columns.close[i]) as f32);
            if let Some(from) = previous {
                ctx.scene.line(from, point, width, self.color);
            }
            previous = Some(point);
        }
    }

    fn price_extent(&self, view: CandleView<'_>, visible: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
        let closes = &view.close()[visible.start.min(view.len())..visible.end.min(view.len())];
        closes.iter().copied().fold(None, |e, v| match e {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
    }
}

impl Plugin for LineSeries {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer(names::CANDLES, order::SERIES, *self);
    }
}
