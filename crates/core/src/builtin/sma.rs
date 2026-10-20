use core::ops::Range;

use crate::plugin::order;
use crate::{CandleView, Chart, Color, DrawCtx, Layer, Plugin, Resources};

/// Simple moving average of the close, drawn over the candles. Not in `DefaultPlugins`: add it with
/// `chart.add_plugin(Sma::new(20, color))`. It doubles as the example of an indicator: it reads
/// bars only through the bounded view, and asks for price room through `price_extent`.
#[derive(Clone, Copy)]
pub struct Sma {
    pub period: usize,
    pub color: Color,
    /// Layer name. Give every average on one chart its own.
    pub name: &'static str,
}

impl Sma {
    pub fn new(period: usize, color: Color) -> Self {
        Self {
            period: period.max(1),
            color,
            name: "sma",
        }
    }

    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }
}

impl Plugin for Sma {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer(self.name, order::OVERLAY, *self);
    }
}

/// The average at bar `i`, defined from bar `period - 1` on.
fn averages(view: CandleView<'_>, period: usize, range: Range<usize>) -> impl Iterator<Item = (usize, f64)> + '_ {
    let close = view.close();
    let start = range.start.max(period - 1);
    let mut sum = 0.0;
    if start < range.end {
        sum = close[start + 1 - period..start].iter().sum();
    }
    (start..range.end).map(move |i| {
        sum += close[i];
        let value = sum / period as f64;
        sum -= close[i + 1 - period];
        (i, value)
    })
}

impl Layer for Sma {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        // With less than a pixel per bar, one point per pixel is enough.
        let stride = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let width = ctx.px(1.5);
        let mut previous = None;
        for (i, value) in averages(ctx.view, self.period, ctx.visible.clone()).step_by(stride) {
            let point = (ctx.time.x_of(i as f64 + 0.5) as f32, ctx.price.y_of(value) as f32);
            if let Some(from) = previous {
                ctx.scene.line(from, point, width, self.color);
            }
            previous = Some(point);
        }
    }

    fn price_extent(&self, view: CandleView<'_>, visible: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
        averages(view, self.period, visible).fold(None, |extent, (_, v)| match extent {
            None => Some((v, v)),
            Some((low, high)) => Some((low.min(v), high.max(v))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles, Time};

    fn closes(values: &[f64]) -> Candles {
        let mut c = Candles::new();
        for (i, &v) in values.iter().enumerate() {
            c.push(Candle {
                time: Time(i as i64),
                open: v,
                high: v,
                low: v,
                close: v,
                volume: 0.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn averages_start_once_a_full_window_exists() {
        let data = closes(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let all: Vec<_> = averages(data.view(), 3, 0..5).collect();
        assert_eq!(all, [(2, 2.0), (3, 3.0), (4, 4.0)]);
    }

    #[test]
    fn a_visible_window_uses_bars_before_it() {
        let data = closes(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let tail: Vec<_> = averages(data.view(), 3, 3..5).collect();
        assert_eq!(tail, [(3, 3.0), (4, 4.0)]);
        assert_eq!(averages(data.view(), 3, 0..2).count(), 0);
        assert_eq!(averages(data.view(), 3, 5..5).count(), 0);
    }

    #[test]
    fn period_one_is_the_close_itself() {
        let data = closes(&[7.0, 8.0]);
        let all: Vec<_> = averages(data.view(), 1, 0..2).collect();
        assert_eq!(all, [(0, 7.0), (1, 8.0)]);
    }
}
