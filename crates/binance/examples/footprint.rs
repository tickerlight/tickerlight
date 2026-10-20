//! Footprint and volume profile from real trades, rendered offscreen to a PNG (no window).
//!
//! cargo run --release -p tickerlight-binance --example footprint -- out.png BTCUSDT 2.0

use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::Binance;
use tickerlight_core::{Chart, Event, Footprint, Scene, Time, VolumeProfile, candles_from_trades};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "footprint.png".into());
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let tick: f64 = args.next().and_then(|t| t.parse().ok()).unwrap_or(2.0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;

    let trades = Binance::new().agg_trades(&symbol, Time(now - 12 * 60_000), Time(now))?;
    println!("{} trades", trades.len());
    let candles = candles_from_trades(trades.view(), 60_000);
    let (w, h) = (1200u32, 700u32);
    let mut chart = Chart::new(candles)
        .with_trades(trades)
        .with(Footprint::new(tick))
        .with(VolumeProfile::default());
    chart.resize(w as f32, h as f32);
    for _ in 0..17 {
        chart.handle(Event::Wheel { x: 1000.0, delta: 1.0 });
    }

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
