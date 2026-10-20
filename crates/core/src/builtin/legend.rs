use tickerlight_text::Font;

use super::footprint::compact;
use super::timeframe::TimeframeLabel;
use crate::plugin::order;
use crate::{Align, Chart, DrawCtx, Layer, Plugin};

/// The name of the legend layer.
pub const LEGEND: &str = "legend";

/// What the chart shows, for the start of the legend: the symbol and where it is from (`BTCUSDT`, `Binance Spot`).
/// Set by the program; without it the legend starts with the bar's prices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesTitle {
    pub symbol: String,
    pub source: String,
}

/// The bar under the pointer, or the newest one shown, in the top left corner of the plot, after the symbol, the
/// timeframe and the source: open, high, low, close, the change from the close before and the volume.
pub struct Legend;

impl Plugin for Legend {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer(LEGEND, order::REPLAY, LegendLayer);
    }
}

pub struct LegendLayer;

/// Decimals that show about five significant digits of `price`, two at least.
pub(crate) fn decimals(price: f64) -> usize {
    if !(price.is_finite() && price > 0.0) {
        return 2;
    }
    (4 - price.log10().floor() as i32).clamp(2, 8) as usize
}

impl Layer for LegendLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let view = &ctx.view;
        let style = &ctx.style;
        // The symbol, the timeframe and the source, in the color of text.
        let series = ctx.res.get::<SeriesTitle>();
        let frame = ctx.res.get::<TimeframeLabel>().map_or("", |l| l.text.as_str());
        let title_of = |source: bool| {
            series.map(|t| {
                let source = if source { t.source.as_str() } else { "" };
                match (frame.is_empty(), source.is_empty()) {
                    (false, false) => format!("{} · {frame} · {source}", t.symbol),
                    (false, true) => format!("{} · {frame}", t.symbol),
                    (true, false) => format!("{} · {source}", t.symbol),
                    (true, true) => t.symbol.clone(),
                }
            })
        };
        let font = Font::ui();
        let size = ctx.px(style.font_size);
        let gap = ctx.px(10.0);
        let (x0, y) = (ctx.px(12.0), ctx.px(16.0));
        let (pad, h) = (ctx.px(4.0), size * 1.6);
        // On the chart's background, so it reads over whatever is under it.
        let backdrop = crate::Color {
            a: 220,
            ..style.background
        };
        // No bar yet (bars of seconds waiting for their trades): what the chart is of, still.
        if view.is_empty() {
            if let Some(title) = title_of(true) {
                let w = font.measure(&title, size);
                ctx.scene
                    .fill_rounded(x0 - pad, y - h / 2.0, w + 2.0 * pad, h, pad, backdrop);
                ctx.scene.text(x0, y, size, style.hud_text, Align::Left, &title);
            }
            return;
        }
        let pointed = ctx
            .pointer_in_plot()
            .map(|(x, _)| ctx.time.index_of(x as f64).floor())
            .filter(|&i| i >= 0.0 && (i as usize) < view.len())
            .map(|i| i as usize);
        let i = pointed.unwrap_or(view.len() - 1);
        let (open, high, low, close) = (view.open()[i], view.high()[i], view.low()[i], view.close()[i]);
        let before = if i > 0 { view.close()[i - 1] } else { open };
        let change = if before != 0.0 {
            (close - before) / before * 100.0
        } else {
            0.0
        };
        let d = decimals(close);
        let color = if close >= open {
            style.candle.up
        } else {
            style.candle.down
        };
        let muted = style.axis_text;
        let parts = [
            ("O ", format!("{open:.d$}")),
            ("H ", format!("{high:.d$}")),
            ("L ", format!("{low:.d$}")),
            ("C ", format!("{close:.d$}")),
            ("", format!("{change:+.2}%")),
            ("V ", compact(view.volume()[i])),
        ];
        let part_w = |(name, value): &(&str, String)| font.measure(name, size) + font.measure(value, size) + gap;
        let room = ctx.plot.0 - 2.0 * x0;
        // In a narrow plot it says less: first without the source, then without the volume, the change, and
        // at last only the close and its change.
        const LEVELS: [&[usize]; 5] = [&[0, 1, 2, 3, 4, 5], &[0, 1, 2, 3, 4], &[0, 1, 2, 3], &[3, 4], &[3]];
        let mut title = title_of(true);
        let title_w = |t: &Option<String>| t.as_deref().map_or(0.0, |t| font.measure(t, size) + 1.5 * gap);
        let width = |title: &Option<String>, shown: &[usize]| {
            title_w(title) + shown.iter().map(|&i| part_w(&parts[i])).sum::<f32>() - gap
        };
        if width(&title, LEVELS[0]) > room {
            title = title_of(false);
        }
        let shown = (LEVELS.iter())
            .find(|&&level| width(&title, level) <= room)
            .unwrap_or(&LEVELS[LEVELS.len() - 1]);
        let w = width(&title, shown);
        ctx.scene
            .fill_rounded(x0 - pad, y - h / 2.0, w + 2.0 * pad, h, pad, backdrop);
        let mut x = x0;
        if let Some(title) = &title {
            ctx.scene.text(x, y, size, style.hud_text, Align::Left, title);
            x += font.measure(title, size) + 1.5 * gap;
        }
        for (name, value) in shown.iter().map(|&i| &parts[i]) {
            if !name.is_empty() {
                ctx.scene.text(x, y, size, muted, Align::Left, *name);
                x += font.measure(name, size);
            }
            ctx.scene.text(x, y, size, color, Align::Left, value);
            x += font.measure(value, size) + gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_keep_about_five_digits() {
        assert_eq!(decimals(81_620.5), 2);
        assert_eq!(decimals(104.35), 2);
        assert_eq!(decimals(1.2345), 4);
        assert_eq!(decimals(0.000_123_4), 8);
        assert_eq!(decimals(0.0), 2);
    }
}
