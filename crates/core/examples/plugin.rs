//! How to write a plugin: a "last price" line that can be switched off with a button or the `P` key.
//! It uses everything a plugin has: a shared resource, a layer, a handler, a command, a key, a
//! button, and autoscale room. The built-in features are written the same way.
//!
//! Run headless with: cargo run -p tickerlight-core --example plugin
//! (open it in a window by handing the chart to `tickerlight_backend_wgpu::run` instead).

use tickerlight_core::plugin::order;
use tickerlight_core::sample::random_walk;
use tickerlight_core::{
    Align, Chart, DrawCtx, Event, EventCtx, Handler, Key, Label, Layer, Plugin, Resources, Response, Scene, ToolButton,
};

/// The command other plugins, keys and buttons send to toggle the line.
const TOGGLE: &str = "last-price.toggle";

/// State shared through the chart's resources: any plugin can read or change it.
struct LastPriceVisible(bool);

/// Draws the line. A layer reads the data only through `ctx.view`, which stops at the replay cursor.
struct LastPriceLayer;

impl Layer for LastPriceLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if !ctx.res.get::<LastPriceVisible>().is_some_and(|v| v.0) {
            return;
        }
        let Some(&close) = ctx.view.close().last() else { return };
        let y = ctx.price.y_of(close) as f32;
        let up = ctx.view.open().last().is_some_and(|&open| close >= open);
        let color = if up { ctx.style.candle.up } else { ctx.style.candle.down };
        ctx.scene.line((0.0, y), (ctx.plot.0, y), ctx.line(), color);
        ctx.scene.text(
            ctx.plot.0 - ctx.px(8.0),
            y - ctx.px(10.0),
            ctx.px(ctx.style.font_size),
            color,
            Align::Right,
            format!("{close:.2}"),
        );
    }

    // A layer may ask for room on the price scale: `fn price_extent(&self, view, visible, res) -> Option<(low, high)>`.
}

/// Reacts to the command. Handlers run in order until one consumes the event.
struct LastPriceHandler;

impl Handler for LastPriceHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if *event != Event::Command(TOGGLE) {
            return Response::IGNORED;
        }
        if let Some(visible) = ctx.res.get_mut::<LastPriceVisible>() {
            visible.0 = !visible.0;
        }
        Response::REDRAW
    }
}

/// A plugin bundles all of it. `chart.add_plugin(LastPrice)` is the whole installation.
struct LastPrice;

impl Plugin for LastPrice {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(LastPriceVisible(true));
        chart.add_layer("last-price", order::OVERLAY, LastPriceLayer);
        chart.add_handler("last-price", 150, LastPriceHandler);
        chart.bind(Key::Char('p'), TOGGLE);
        chart.add_button(ToolButton::new(
            TOGGLE,
            Label::Dynamic(|res: &Resources| {
                let on = res.get::<LastPriceVisible>().is_some_and(|v| v.0);
                format!("Last price: {}", if on { "on" } else { "off" })
            }),
        ));
    }
}

fn main() {
    let mut chart = Chart::new(random_walk(1000)).with(LastPrice);
    chart.resize(1200.0, 700.0);
    let mut scene = Scene::new();

    chart.build_scene(&mut scene);
    let labelled = |scene: &Scene| {
        scene
            .texts
            .iter()
            .filter(|t| t.align == Align::Right && t.x < 1128.0)
            .count()
    };
    println!("layers: {:?}", chart.layer_names());
    println!("line label drawn: {}", labelled(&scene) > 0);

    // A key press becomes the command, the handler flips the resource, the layer stops drawing.
    chart.handle(Event::Key(Key::Char('p')));
    chart.build_scene(&mut scene);
    println!("after pressing P: {}", labelled(&scene) > 0);
}
