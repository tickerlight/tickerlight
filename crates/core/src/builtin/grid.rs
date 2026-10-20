use crate::builtin::replay::clock;
use crate::plugin::{names, order};
use crate::ticks::{price_ticks, time_ticks};
use crate::{Chart, DrawCtx, Layer, Panes, Plugin};

/// Grid lines through the axis ticks.
pub struct GridLayer;

impl Layer for GridLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if ctx.view.is_empty() {
            return;
        }
        let (plot_w, plot_h) = ctx.plot;
        let (line, color) = (ctx.line(), ctx.style.grid);
        let (ticks, _) = price_ticks(ctx.price, plot_h, ctx.scale);
        for (_, y) in ticks {
            ctx.scene.fill(0.0, y, plot_w, line, color);
        }
        for tick in time_ticks(ctx.time, ctx.view.time(), clock(ctx.res).zone, ctx.scale) {
            ctx.scene.fill(tick.x, 0.0, line, plot_h, color);
        }
    }
}

pub struct GridPlugin;

impl Plugin for GridPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_layer_in(Panes::All, names::GRID, order::GRID, GridLayer);
    }
}
