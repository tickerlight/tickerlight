use tickerlight_text::Font;

use super::crosshair::{CrosshairActive, PRICE_LABEL_HEIGHT, crosshair_labels};
use super::last_price::{LastPriceShown, last_price_label};
use super::replay::clock;
use super::{Account, Drawings};
use crate::plugin::{names, order};
use crate::price_scale::PriceMode;
use crate::ticks::{format_price, price_ticks, time_label, time_ticks};
use crate::{Align, Chart, Clip, Color, DrawCtx, Layer, Panes, Plugin};

/// A label on the price axis from `top` to `bottom`: a pill just inside the axis, its text at the label padding.
pub(crate) fn axis_tag(ctx: &mut DrawCtx<'_>, (top, bottom): (f32, f32), color: Color) {
    let (plot_w, width) = (ctx.plot.0, ctx.size.0);
    let inset = ctx.px(3.0);
    let h = bottom - top;
    let radius = (h / 2.0).min(ctx.px(10.0));
    ctx.scene
        .fill_rounded(plot_w + inset, top, width - plot_w - 2.0 * inset, h, radius, color);
}

/// The price axis on the right of every pane, and the time axis under the last one, with their labels.
pub struct AxesLayer;

impl Layer for AxesLayer {
    fn clip(&self) -> Clip {
        Clip::Pane
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let (width, height) = ctx.size;
        let (plot_w, plot_h) = ctx.plot;
        let line = ctx.line();
        let style = ctx.style;

        let last = ctx.pane.is_last();
        ctx.scene.fill(plot_w, 0.0, width - plot_w, height, style.background);
        ctx.scene.fill(plot_w, 0.0, line, height, style.grid);
        if last {
            ctx.scene.fill(0.0, plot_h, width, height - plot_h, style.background);
        }
        // The line under the plot: the top of the time axis, or the border between two panes (inside this one).
        ctx.scene
            .fill(0.0, if last { plot_h } else { plot_h - line }, width, line, style.axis);
        if ctx.view.is_empty() {
            return;
        }

        let font_size = ctx.px(style.font_size);
        let padding = ctx.px(style.label_padding);

        // Labels the crosshair's own labels would sit on are left out, not half covered; so are those under the
        // labels of the position, the orders and the horizontal lines.
        let keep_out = if ctx.res.contains::<CrosshairActive>() {
            crosshair_labels(ctx)
        } else {
            None
        };
        let mut boxes: Vec<(f32, f32)> = keep_out
            .as_ref()
            .and_then(|k| k.price.as_ref())
            .map(|p| p.span)
            .into_iter()
            .collect();
        let mut marked: Vec<f64> = Vec::new();
        if ctx.pane.index == 0
            && let Some(account) = ctx.res.get::<Account>()
        {
            marked.extend(account.average_price());
            marked.extend(account.orders().iter().map(|o| o.price));
        }
        if let Some(drawings) = ctx.res.get::<Drawings>() {
            let lines = drawings
                .items
                .iter()
                .filter(|d| d.tool.labels_price() && d.pane == ctx.pane.name && !drawings.hidden);
            marked.extend(lines.map(|d| d.a.price));
        }
        if ctx.res.contains::<LastPriceShown>()
            && let Some((.., span)) = last_price_label(ctx)
        {
            boxes.push(span);
        }
        let half = ctx.px(PRICE_LABEL_HEIGHT) / 2.0;
        boxes.extend(marked.into_iter().map(|price| {
            let y = ctx.price.y_of(price) as f32;
            (y - half, y + half)
        }));

        let (ticks, decimals) = price_ticks(ctx.price, plot_h, ctx.scale);
        for (price, y) in ticks {
            // A label half cut off by the plot edge looks broken; leave it out.
            if y < font_size / 2.0 || y > plot_h - font_size / 2.0 {
                continue;
            }
            if boxes
                .iter()
                .any(|&(top, bottom)| y + font_size / 2.0 > top && y - font_size / 2.0 < bottom)
            {
                continue;
            }
            ctx.scene.text(
                plot_w + padding,
                y,
                font_size,
                style.axis_text,
                Align::Left,
                format_price(ctx.price, price, decimals),
            );
        }
        // Says when the axis is not the plain one.
        let mode = match ctx.price.mode {
            PriceMode::Linear => None,
            PriceMode::Log => Some("LOG"),
            PriceMode::Percent { .. } => Some("%"),
        };
        if let Some(mode) = mode {
            ctx.scene.text(
                width - padding,
                ctx.px(12.0),
                font_size * 0.8,
                style.axis_text,
                Align::Right,
                mode,
            );
        }

        if !last {
            return;
        }
        let times = ctx.view.time();
        let zone = clock(ctx.res);
        let label_y = plot_h + (height - plot_h) / 2.0;
        for tick in time_ticks(ctx.time, times, zone.zone, ctx.scale) {
            let label = time_label(&tick, zone);
            let span = keep_out.as_ref().and_then(|k| k.bar.as_ref()).map(|b| b.span);
            let half = Font::ui().measure(&label, font_size) / 2.0;
            // Neither half covered by the crosshair's label nor cut off at an edge of the plot.
            let covered = span.is_some_and(|(left, right)| tick.x + half > left && tick.x - half < right);
            if covered || tick.x - half < 0.0 || tick.x + half > plot_w {
                continue;
            }
            ctx.scene
                .text(tick.x, label_y, font_size, style.axis_text, Align::Center, label);
        }
    }
}

pub struct AxesPlugin;

impl Plugin for AxesPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer_in(Panes::All, names::AXES, order::AXES, AxesLayer);
    }
}
