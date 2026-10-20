use std::collections::BTreeMap;

use tickerlight_text::Font;

use crate::plugin::order;
use crate::ticks::bar_interval_ms;
use crate::{Align, Chart, Color, DrawCtx, Layer, Plugin, Side, Time};

/// Footprint: for every bar the volume sold and bought at each price level, side by side
/// (sold on the left, bought on the right), with the busiest level outlined, the sides that outweigh the
/// other side diagonally marked, a thin candle beside and the delta and volume of the bar under it. Drawn only when the bars
/// are wide enough and the price levels tall enough to read; otherwise the layer is silent and the
/// candles underneath show. Needs a tape (`Chart::with_trades`) and the size of a price level.
#[derive(Clone, Copy)]
pub struct Footprint {
    /// Price height of one level (the tick size, or a multiple of it).
    pub tick: f64,
    /// Bars narrower than this (logical pixels) are not drawn.
    pub min_bar_width: f32,
    /// Levels shorter than this (logical pixels) are not drawn.
    pub min_row_height: f32,
    /// Merges levels, 1, 2, 5, 10... ticks into one, until each is tall enough for its numbers.
    pub grouped: bool,
    /// How many times one side of a level must outweigh the other side diagonally to be marked; 0 marks none.
    pub imbalance: f64,
    pub buy: Color,
    pub sell: Color,
    pub name: &'static str,
}

impl Footprint {
    pub fn new(tick: f64) -> Self {
        Self {
            tick,
            min_bar_width: 56.0,
            min_row_height: 4.0,
            grouped: false,
            imbalance: 3.0,
            buy: crate::CandleStyle::default().up,
            sell: crate::CandleStyle::default().down,
            name: "footprint",
        }
    }
}

impl Plugin for Footprint {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer(self.name, order::OVERLAY, *self);
    }
}

/// The height of a level (logical pixels) that `grouped` aims for: enough for its numbers.
const TEXT_ROW: f32 = 14.0;

/// `1234.5` as `1.2k`, `12.34` as `12.3`, `0.056` as `0.06`: short enough for a small cell.
pub fn compact(qty: f64) -> String {
    let a = qty.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", qty / 1_000_000.0)
    } else if a >= 1000.0 {
        format!("{:.1}k", qty / 1000.0)
    } else if a >= 100.0 {
        format!("{qty:.0}")
    } else if a >= 10.0 {
        format!("{qty:.1}")
    } else if a >= 0.1 || a == 0.0 {
        format!("{qty:.2}")
    } else {
        format!("{qty:.3}")
    }
}

/// The volume sold and bought at each level of a bar, the low level first.
fn levels_of(tape: &crate::TradeView<'_>, tick: f64) -> BTreeMap<i64, [f64; 2]> {
    let mut levels: BTreeMap<i64, [f64; 2]> = BTreeMap::new();
    for k in 0..tape.len() {
        let level = (tape.price()[k] / tick + 1e-9).floor() as i64;
        levels.entry(level).or_default()[matches!(tape.side()[k], Side::Buy) as usize] += tape.qty()[k];
    }
    levels
}

/// Whether each side of each level outweighs the other side diagonally (buying at a level against selling one
/// level lower, selling against buying one level higher) by `ratio` or more, as `[sold, bought]`. Sides lighter
/// than a twentieth of the bar's heaviest are left out, so a stray trade at the edge does not count.
pub fn imbalances(levels: &BTreeMap<i64, [f64; 2]>, ratio: f64) -> BTreeMap<i64, [bool; 2]> {
    let heaviest = levels.values().flatten().copied().fold(0.0, f64::max);
    let side = |level: i64, side: usize| levels.get(&level).map_or(0.0, |v| v[side]);
    levels
        .iter()
        .map(|(&level, &[sold, bought])| {
            let outweighs = |volume: f64, other: f64| {
                ratio > 0.0 && volume > 0.0 && volume >= heaviest / 20.0 && volume >= ratio * other
            };
            let flags = [
                outweighs(sold, side(level + 1, 1)),
                outweighs(bought, side(level - 1, 0)),
            ];
            (level, flags)
        })
        .collect()
}

impl Layer for Footprint {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let times = ctx.view.time();
        let range = ctx.visible.start.min(times.len())..ctx.visible.end.min(times.len());
        if self.tick <= 0.0 {
            return;
        }
        let row_of = |tick: f64| (ctx.price.y_of(0.0) - ctx.price.y_of(tick)).abs() as f32;
        let tick = if self.grouped {
            (0..7)
                .flat_map(|e| [1.0, 2.0, 5.0].map(|m| self.tick * m * 10f64.powi(e)))
                .find(|&t| row_of(t) >= ctx.px(TEXT_ROW))
                .unwrap_or(self.tick)
        } else {
            self.tick
        };
        let row_px = row_of(tick);
        if range.is_empty()
            || ctx.trades.is_empty()
            || (ctx.time.bar_spacing as f32) < ctx.px(self.min_bar_width)
            || row_px < ctx.px(self.min_row_height)
        {
            return;
        }
        let font = Font::ui();
        let font_size = (row_px * 0.8).min(ctx.px(ctx.style.font_size));
        let summary_size = ctx.px(ctx.style.font_size - 1.0);
        let show_text = row_px >= ctx.px(10.0);
        let unit = ctx.line();
        let (open, high, low, close) = (ctx.view.open(), ctx.view.high(), ctx.view.low(), ctx.view.close());

        for i in range {
            let from = times[i];
            let to = Time(
                times[i].0
                    + if i + 1 < times.len() {
                        times[i + 1].0 - times[i].0
                    } else {
                        bar_interval_ms(times, i)
                    },
            );
            let tape = ctx.trades.between(from, to);
            if tape.is_empty() {
                continue;
            }
            let levels = levels_of(&tape, tick);
            let marked = imbalances(&levels, self.imbalance);
            let busiest = levels
                .iter()
                .map(|(&l, v)| (l, v[0] + v[1]))
                .fold((0, 0.0), |m, x| if x.1 > m.1 { x } else { m });
            let most_side = levels.values().flat_map(|v| v.iter().copied()).fold(0.0, f64::max);

            // The slot of the bar: a thin candle at its left, then the sold and the bought cells.
            let slot = ctx.time.bar_spacing as f32;
            let start = ctx.time.x_of(i as f64) as f32 + slot * 0.05;
            let candle_w = (slot * 0.08).max(ctx.px(3.0)).round();
            let left = (start + candle_w + ctx.px(2.0)).round();
            let half = ((start + slot * 0.9 - left) / 2.0).floor();

            let rising = close[i] >= open[i];
            let color = if rising {
                ctx.style.candle.up
            } else {
                ctx.style.candle.down
            };
            let y = |p: f64| ctx.price.y_of(p) as f32;
            let wick_x = (start + candle_w / 2.0 - unit / 2.0).round();
            ctx.scene
                .fill(wick_x, y(high[i]), unit, (y(low[i]) - y(high[i])).max(unit), color);
            let (body_top, body_bottom) = (y(open[i].max(close[i])), y(open[i].min(close[i])));
            ctx.scene
                .fill(start, body_top, candle_w, (body_bottom - body_top).max(unit), color);

            // One solid backdrop for the whole bar, so the candle underneath does not show through
            // the gaps between the levels.
            let (first, last) = (*levels.keys().next().unwrap(), *levels.keys().next_back().unwrap());
            let column_top = y((last + 1) as f64 * tick);
            let column_bottom = y(first as f64 * tick);
            ctx.scene.fill(
                left,
                column_top,
                2.0 * half - unit,
                column_bottom - column_top,
                ctx.style.background,
            );
            for (&level, &[sell, buy]) in &levels {
                let top = y((level + 1) as f64 * tick);
                let bottom = y(level as f64 * tick);
                let h = (bottom - top - unit).max(1.0);
                let flags = marked[&level];
                for (side, volume, x, imbalanced) in [
                    (self.sell, sell, left, flags[0]),
                    (self.buy, buy, left + half, flags[1]),
                ] {
                    if volume <= 0.0 {
                        continue;
                    }
                    let strength = if most_side > 0.0 {
                        (volume / most_side) as f32
                    } else {
                        0.0
                    };
                    let alpha = 50.0 + 170.0 * strength;
                    ctx.scene.fill(x, top, half - unit, h, Color { a: alpha as u8, ..side });
                    if imbalanced {
                        // A bar along the outer edge of the cell: left of the sold, right of the bought.
                        let w = ctx.px(2.0).round().max(unit);
                        let edge = if x == left { x } else { x + half - unit - w };
                        ctx.scene.fill(edge, top, w, h, side);
                    }
                    let text = compact(volume);
                    if show_text && font.measure(&text, font_size) <= half - 2.0 * unit {
                        ctx.scene.text(
                            x + (half - unit) / 2.0,
                            top + h / 2.0,
                            font_size,
                            ctx.style.hud_text,
                            Align::Center,
                            text,
                        );
                    }
                }
                if level == busiest.0 {
                    let outline = ctx.style.hud_text;
                    ctx.scene.fill(left, top, 2.0 * half - unit, unit, outline);
                    ctx.scene.fill(left, top + h - unit, 2.0 * half - unit, unit, outline);
                }
            }

            // Under the bar: what was bought less what was sold, and all that was traded.
            let (sold, bought) = levels.values().fold((0.0, 0.0), |(s, b), v| (s + v[0], b + v[1]));
            let delta = bought - sold;
            let below = column_bottom.max(y(low[i])) + ctx.px(4.0);
            let middle = left + half - unit / 2.0;
            let line_h = summary_size * 1.3;
            let delta_text = if delta > 0.0 {
                format!("+{}", compact(delta))
            } else {
                compact(delta)
            };
            let lines = [
                (delta_text, if delta >= 0.0 { self.buy } else { self.sell }),
                (compact(sold + bought), ctx.style.axis_text),
            ];
            for (k, (text, color)) in lines.into_iter().enumerate() {
                if font.measure(&text, summary_size) <= 2.0 * half {
                    let y = below + line_h * (k as f32 + 0.5);
                    ctx.scene.text(middle, y, summary_size, color, Align::Center, text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, compact, imbalances};

    #[test]
    fn a_side_outweighing_the_other_diagonally_is_marked() {
        // [sold, bought] by level. Bought 9 at 101 against 3 sold at 100: marked; sold 3 at 100 against the 9
        // bought at 101: not. Sold 8 at 102 against nothing bought above: marked. 0.1 bought at 100 against
        // nothing below is too little to count.
        let levels = BTreeMap::from([(100, [3.0, 0.1]), (101, [2.0, 9.0]), (102, [8.0, 4.0])]);
        let marked = imbalances(&levels, 3.0);
        assert_eq!(marked[&100], [false, false]);
        assert_eq!(marked[&101], [false, true]);
        assert_eq!(marked[&102], [true, false]);
        assert!(imbalances(&levels, 0.0).values().all(|m| m == &[false, false]));
    }

    #[test]
    fn compact_numbers_stay_short() {
        assert_eq!(compact(0.056), "0.056");
        assert_eq!(compact(0.5), "0.50");
        assert_eq!(compact(12.34), "12.3");
        assert_eq!(compact(123.4), "123");
        assert_eq!(compact(1234.5), "1.2k");
        assert_eq!(compact(2_500_000.0), "2.5M");
        assert_eq!(compact(0.0), "0.00");
        assert_eq!(compact(0.0004), "0.000");
    }
}
