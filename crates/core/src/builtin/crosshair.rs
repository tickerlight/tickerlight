use tickerlight_text::Font;

use super::replay::clock;
use super::toolbar::layout;
use crate::calendar::full_label;
use crate::plugin::{names, order};
use crate::ticks::{bar_interval_ms, format_price};
use crate::{Align, Chart, Clip, Color, DrawCtx, Event, EventCtx, Handler, Layer, Panes, Plugin, Response, Scene};

/// How tall a price label on the axis is, before scaling.
pub(crate) const PRICE_LABEL_HEIGHT: f32 = 20.0;
const DASH: f32 = 4.0;
const GAP: f32 = 3.0;

/// Marker resource: the crosshair is installed, so other layers (the axes) keep their labels out
/// of the crosshair's label boxes.
pub struct CrosshairActive;

/// The time under another chart's crosshair (charts side by side). A chart without the pointer draws its
/// vertical line at the bar open then, with no price line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkedTime(pub Option<crate::Time>);

/// What the crosshair shows for the current pointer, in this pane's coordinates, in device pixels.
pub struct CrosshairLabels {
    /// Only in the pane the pointer is over: the horizontal line and its price label.
    pub price: Option<PriceLabel>,
    /// Present only over an existing bar: the empty margin has no time. It shows in every pane
    /// (the vertical line) and on the time axis.
    pub bar: Option<BarLabel>,
}

pub struct PriceLabel {
    /// Pointer y.
    pub y: f32,
    pub text: String,
    /// Vertical extent of the label box.
    pub span: (f32, f32),
}

pub struct BarLabel {
    /// Center of the bar, where the vertical line runs.
    pub x: f32,
    pub text: String,
    /// Horizontal extent of the time label box.
    pub span: (f32, f32),
}

/// The crosshair for the current pointer, or `None` if the pointer is not over the plot of any pane. Without a
/// pointer it is the vertical line of [`LinkedTime`], if any.
pub fn crosshair_labels(ctx: &DrawCtx<'_>) -> Option<CrosshairLabels> {
    let (plot_w, plot_h) = ctx.pane.chart_plot;
    if ctx.view.is_empty() {
        return None;
    }
    let times = ctx.view.time();
    let Some((px, py)) = ctx.chart_pointer() else {
        let t = ctx.res.get::<LinkedTime>()?.0?;
        let bar = times.partition_point(|&x| x <= t).checked_sub(1)?;
        return Some(CrosshairLabels {
            price: None,
            bar: Some(bar_label(ctx, bar as f64)),
        });
    };
    if !(0.0..plot_w).contains(&px) || !(0.0..plot_h).contains(&py) {
        return None;
    }
    // No crosshair over a button: the pointer is pointing at that, not at the chart.
    let over_button = layout(ctx.res, ctx.style, ctx.scale, plot_w, ctx.pane.main_height)
        .iter()
        .any(|(_, _, r)| px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h);
    if over_button {
        return None;
    }
    let label_h = ctx.px(PRICE_LABEL_HEIGHT);

    let price = ctx.pointer_in_plot().map(|(_, y)| {
        let decimals = super::last_price::label_decimals(ctx);
        PriceLabel {
            y,
            text: format_price(ctx.price, ctx.price.display(ctx.price.price_of(y as f64)), decimals),
            span: (y - label_h / 2.0, y + label_h / 2.0),
        }
    });

    let bar = ctx.time.index_of(px as f64).floor();
    let bar = (bar >= 0.0 && (bar as usize) < times.len()).then(|| bar_label(ctx, bar));
    Some(CrosshairLabels { price, bar })
}

/// The vertical line's label at bar `bar` (whole) of the view.
fn bar_label(ctx: &DrawCtx<'_>, bar: f64) -> BarLabel {
    let times = ctx.view.time();
    let i = bar as usize;
    let x = ctx.time.x_of(bar + 0.5).floor() as f32;
    let zone = clock(ctx.res);
    let text = full_label(times[i], bar_interval_ms(times, i), zone);
    let box_w = Font::ui().measure(&text, ctx.px(ctx.style.font_size)) + 2.0 * ctx.px(ctx.style.label_padding);
    let left = (x + 0.5 - box_w / 2.0).clamp(0.0, (ctx.size.0 - box_w).max(0.0));
    BarLabel {
        x,
        text,
        span: (left, left + box_w),
    }
}

/// The dashed lines through the pointer with price and time labels on the axes.
/// The vertical line snaps to the bar under the pointer and runs through every pane; the horizontal one
/// and its price label are in the pane the pointer is over.
pub struct CrosshairLayer;

impl Layer for CrosshairLayer {
    fn clip(&self) -> Clip {
        Clip::Pane
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let Some(labels) = crosshair_labels(ctx) else { return };
        let (plot_w, plot_h) = ctx.plot;
        let height = ctx.size.1;
        let style = ctx.style;
        let line = ctx.line();
        let dashes = (line, ctx.px(DASH), ctx.px(GAP));
        let font_size = ctx.px(style.font_size);

        if let Some(price) = labels.price {
            dashed_horizontal(ctx.scene, price.y.floor(), 0.0, plot_w, dashes, style.crosshair);
            super::axes::axis_tag(ctx, price.span, style.crosshair_label);
            ctx.scene.text(
                plot_w + ctx.px(style.label_padding),
                price.y,
                font_size,
                style.crosshair_label_text,
                Align::Left,
                price.text,
            );
        }

        let Some(bar) = labels.bar else { return };
        dashed_vertical(ctx.scene, bar.x, 0.0, plot_h, dashes, style.crosshair);
        if !ctx.pane.is_last() {
            return;
        }
        let (left, right) = bar.span;
        let box_h = height - plot_h - line;
        let inset = ctx.px(3.0);
        let radius = ((box_h - 2.0 * inset) / 2.0).min(ctx.px(10.0));
        ctx.scene.fill_rounded(
            left,
            plot_h + line + inset,
            right - left,
            box_h - 2.0 * inset,
            radius,
            style.crosshair_label,
        );
        ctx.scene.text(
            (left + right) / 2.0,
            plot_h + line + box_h / 2.0,
            font_size,
            style.crosshair_label_text,
            Align::Center,
            bar.text,
        );
    }
}

/// Asks for a redraw while the crosshair moves: when the pointer is over the plot, and once more
/// when it leaves it (to take the crosshair away).
pub struct CrosshairHandler;

impl Handler for CrosshairHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if !matches!(event, Event::PointerMoved { .. } | Event::PointerLeft) {
            return Response::IGNORED;
        }
        let was = ctx.previous_pointer.position.is_some_and(|p| ctx.over_plot(p));
        let is = ctx.pointer.position.is_some_and(|p| ctx.over_plot(p));
        Response {
            redraw: was || is,
            ..Response::IGNORED
        }
    }
}

pub struct CrosshairPlugin;

impl Plugin for CrosshairPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(CrosshairActive);
        chart.add_layer_in(Panes::All, names::CROSSHAIR, order::CROSSHAIR, CrosshairLayer);
        chart.add_handler(names::CROSSHAIR, order::CROSSHAIR_HANDLER, CrosshairHandler);
    }
}

/// `(thickness, dash, gap)` in device pixels.
type Dashes = (f32, f32, f32);

fn dashed_horizontal(scene: &mut Scene, y: f32, from: f32, to: f32, (thickness, dash, gap): Dashes, color: Color) {
    let mut x = from;
    while x < to {
        scene.fill(x, y, dash.min(to - x), thickness, color);
        x += dash + gap;
    }
}

fn dashed_vertical(scene: &mut Scene, x: f32, from: f32, to: f32, (thickness, dash, gap): Dashes, color: Color) {
    let mut y = from;
    while y < to {
        scene.fill(x, y, thickness, dash.min(to - y), color);
        y += dash + gap;
    }
}
