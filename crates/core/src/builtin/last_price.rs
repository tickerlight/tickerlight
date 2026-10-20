use super::crosshair::PRICE_LABEL_HEIGHT;
use super::legend::decimals;
use crate::plugin::{names, order};
use crate::price_scale::PriceMode;
use crate::ticks::{bar_interval_ms, decimals_for_step, format_price, price_step};
use crate::{Align, Chart, Clip, DrawCtx, Layer, Plugin, Time};

/// The wall clock, kept by the program as a resource: the last price label counts down to the close of the bar
/// by it. Without it, or once the bar is past its close, there is no countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock(pub Time);

/// Marks the chart as showing the last price, so the axis leaves its labels out from under it.
pub(crate) struct LastPriceShown;

/// `42s` as `00:42`, an hour and more as `01:00:42`, a day and more as `2d 03h`.
pub fn countdown(ms: i64) -> String {
    let s = ms.max(0).div_euclid(1000);
    let (d, h, m, s) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    if d > 0 {
        format!("{d}d {h:02}h")
    } else if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// The decimals of a price label on the axis: as many as the prices of the main pane have, so the last price and
/// the pointer's are exact; elsewhere, and in percent, a little finer than the ticks.
pub(crate) fn label_decimals(ctx: &DrawCtx<'_>) -> usize {
    match ctx.view.close().last() {
        Some(&close) if ctx.pane.index == 0 && !matches!(ctx.price.mode, PriceMode::Percent { .. }) => decimals(close),
        _ => decimals_for_step(price_step(ctx.price, ctx.plot.1, ctx.scale)).max(2),
    }
}

/// The label of the last price: its y and its text, the time left to the close if the clock runs inside the last
/// bar, and its top and bottom (device pixels).
type Label = (f32, String, Option<String>, (f32, f32));

/// The label of the last price, or `None` if there is no bar or the price is off the plot.
pub(crate) fn last_price_label(ctx: &DrawCtx<'_>) -> Option<Label> {
    if ctx.pane.index != 0 {
        return None;
    }
    let close = *ctx.view.close().last()?;
    let y = ctx.price.y_of(close) as f32;
    if !(0.0..=ctx.plot.1).contains(&y) {
        return None;
    }
    let text = format_price(ctx.price, ctx.price.display(close), label_decimals(ctx));
    let times = ctx.view.time();
    let last = times.len() - 1;
    let end = times[last].0 + bar_interval_ms(times, last);
    let left = ctx
        .res
        .get::<Clock>()
        .map(|c| c.0.0)
        .filter(|&now| now >= times[last].0 && now < end)
        .map(|now| countdown(end - now));
    let half = ctx.px(PRICE_LABEL_HEIGHT) / 2.0;
    let below = if left.is_some() {
        ctx.px(PRICE_LABEL_HEIGHT - 4.0)
    } else {
        0.0
    };
    Some((y, text, left, (y - half, y + half + below)))
}

/// The last price: a dotted line across the plot in the color of the last bar and a label on the price axis,
/// with the time left to the bar's close under the price while the data are live.
pub struct LastPriceLayer;

impl Layer for LastPriceLayer {
    fn clip(&self) -> Clip {
        Clip::Pane
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let Some((y, text, left, (top, bottom))) = last_price_label(ctx) else {
            return;
        };
        let style = ctx.style;
        let (open, close) = (ctx.view.open(), ctx.view.close());
        let rising = close[close.len() - 1] >= open[open.len() - 1];
        let color = if rising { style.candle.up } else { style.candle.down };
        let plot_w = ctx.plot.0;
        let unit = ctx.line();
        let y = y.floor();
        let (dot, gap) = (unit, ctx.px(2.0).max(unit + 1.0));
        let mut x = 0.0;
        while x < plot_w {
            ctx.scene.fill(x, y, dot, unit, color);
            x += dot + gap;
        }

        let font_size = ctx.px(style.font_size);
        let padding = ctx.px(style.label_padding);
        super::axes::axis_tag(ctx, (top, bottom), color);
        let half = ctx.px(PRICE_LABEL_HEIGHT) / 2.0;
        let text_color = color.ink();
        ctx.scene
            .text(plot_w + padding, top + half, font_size, text_color, Align::Left, text);
        if let Some(left) = left {
            let at = top + 2.0 * half + (bottom - top - 2.0 * half) / 2.0 - ctx.px(2.0);
            ctx.scene
                .text(plot_w + padding, at, font_size * 0.9, text_color, Align::Left, left);
        }
    }
}

/// Adds the last price line and label; `DefaultPlugins` has it.
pub struct LastPricePlugin;

impl Plugin for LastPricePlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer_in(crate::Panes::Main, names::LAST_PRICE, order::LAST_PRICE, LastPriceLayer);
        chart.resources_mut().insert(LastPriceShown);
    }
}

#[cfg(test)]
mod tests {
    use super::countdown;

    #[test]
    fn the_time_left_is_short() {
        assert_eq!(countdown(42_900), "00:42");
        assert_eq!(countdown(3_600_000 + 42_000), "01:00:42");
        assert_eq!(countdown(2 * 86_400_000 + 3 * 3_600_000 + 5), "2d 03h");
        assert_eq!(countdown(-5), "00:00");
    }
}
