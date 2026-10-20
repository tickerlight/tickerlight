//! Renders a chart of real Binance candles to a PNG, offscreen (no window).
//!
//! cargo run --release -p tickerlight-binance --example binance_snapshot -- out.png BTCUSDT

use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::{Binance, Interval};
use tickerlight_core::{Chart, Color, Event, Scene, Sma, Time};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "binance.png".into());
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let candles = Binance::new().klines(&symbol, Interval::M5, Time(now - 3 * 24 * 3_600_000), Time(now))?;
    let (w, h) = (1200u32, 700u32);
    let mut chart = Chart::new(candles).with(Sma::new(50, Color::rgb(255, 183, 77)));
    chart.resize(w as f32, h as f32);
    chart.handle(Event::PointerMoved { x: 700.0, y: 300.0 });

    let gpu = Gpu::new_headless()?;
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    let rgba = render_to_rgba(&gpu, &scene, w, h);
    let file = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut encoder = png::Encoder::new(file, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    println!("wrote {out}");
    Ok(())
}
