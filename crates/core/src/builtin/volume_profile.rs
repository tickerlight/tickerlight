use std::collections::BTreeMap;

use crate::plugin::order;
use crate::ticks::bar_interval_ms;
use crate::{Chart, Color, DrawCtx, Layer, Plugin, Side};

/// Traded volume by price over the bars on screen, as horizontal bars growing from the right edge of
/// the plot: sells (left part) and buys (right part) of every price row. Needs a tape of trades
/// (`Chart::with_trades`); it sees only the trades the replay cursor lets through.
#[derive(Clone, Copy)]
pub struct VolumeProfile {
    /// The longest bar, as a fraction of the plot width.
    pub max_width: f32,
    /// Height of a price row in logical pixels.
    pub row: f32,
    /// The price step of the market: a row is never shorter than one, so zoomed in the rows are its price
    /// levels. Zero for rows of `row` pixels at any zoom.
    pub tick: f64,
    pub buy: Color,
    pub sell: Color,
    /// Layer name.
    pub name: &'static str,
}

impl Default for VolumeProfile {
    fn default() -> Self {
        Self {
            max_width: 0.25,
            row: 4.0,
            tick: 0.0,
            buy: Color {
                a: 110,
                ..crate::CandleStyle::default().up
            },
            sell: Color {
                a: 110,
                ..crate::CandleStyle::default().down
            },
            name: "volume-profile",
        }
    }
}

impl Plugin for VolumeProfile {
    fn build(&self, chart: &mut Chart) {
        // Behind the candles and the footprint, which it would hide at the right edge.
        chart.add_layer(self.name, order::VOLUME + 1, *self);
    }
}

impl Layer for VolumeProfile {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let times = ctx.view.time();
        let range = ctx.visible.start.min(times.len())..ctx.visible.end.min(times.len());
        if range.is_empty() || ctx.trades.is_empty() {
            return;
        }
        let from = times[range.start];
        let last = range.end - 1;
        let to = crate::Time(times[last].0 + bar_interval_ms(times, last));
        let tape = ctx.trades.between(from, to);

        let (plot_w, plot_h) = ctx.plot;
        let row_h = ctx.px(self.row).max(1.0);
        let tick_h = (ctx.price.y_of(0.0) - ctx.price.y_of(self.tick)).abs() as f32;
        // [sell, buy] volume of every row, with its top and height: price levels when they are taller than a
        // row, else rows of the screen counted from the top of the plot.
        let mut bins: BTreeMap<i64, [f64; 2]> = BTreeMap::new();
        let by_level = self.tick > 0.0 && tick_h >= row_h;
        for i in 0..tape.len() {
            let price = tape.price()[i];
            let y = ctx.price.y_of(price);
            if y >= 0.0 && y < plot_h as f64 {
                let key = if by_level {
                    (price / self.tick + 1e-9).floor() as i64
                } else {
                    (y / row_h as f64) as i64
                };
                bins.entry(key).or_default()[matches!(tape.side()[i], Side::Buy) as usize] += tape.qty()[i];
            }
        }
        let place = |key: i64| {
            if by_level {
                let top = ctx.price.y_of((key + 1) as f64 * self.tick) as f32;
                (top, ctx.price.y_of(key as f64 * self.tick) as f32 - top)
            } else {
                (key as f32 * row_h, row_h)
            }
        };
        let most = bins.values().map(|b| b[0] + b[1]).fold(0.0, f64::max);
        if most <= 0.0 {
            return;
        }
        let full = plot_w * self.max_width;
        let gap = ctx.line();
        for (&key, [sell, buy]) in &bins {
            let total = sell + buy;
            if total <= 0.0 {
                continue;
            }
            let width = (total / most) as f32 * full;
            let sell_w = (sell / total) as f32 * width;
            let (y, row_h) = place(key);
            let h = (row_h - gap).max(1.0);
            let left = plot_w - width;
            if sell_w > 0.0 {
                ctx.scene.fill(left, y, sell_w, h, self.sell);
            }
            if width - sell_w > 0.0 {
                ctx.scene.fill(left + sell_w, y, width - sell_w, h, self.buy);
            }
        }
    }
}
