//! Live feed without a window: fetches recent candles, attaches the WebSocket streams, drives the
//! chart with ticks for a few seconds and prints what arrived.
//!
//! cargo run -p tickerlight-binance --example live -- BTCUSDT 10

use std::time::{Duration, Instant};

use tickerlight_binance::{Binance, Interval, KlineStream, TradeStream};
use tickerlight_core::{Chart, Event, Time};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let candles = Binance::new().klines(&symbol, Interval::M1, Time(now - 3_600_000), Time(now))?;
    let mut chart = Chart::new(candles);
    chart.resize(1200.0, 700.0);
    chart.add_source(KlineStream::connect(&symbol, Interval::M1));
    chart.add_source(TradeStream::connect(&symbol));
    println!("history: {} candles, live: {}", chart.seen().len(), chart.needs_tick());

    let end = Instant::now() + Duration::from_secs(seconds);
    let mut last = Instant::now();
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(16));
        let dt = last.elapsed().as_secs_f32();
        last = Instant::now();
        chart.handle(Event::Tick { dt });
    }
    let candles = chart.seen();
    println!(
        "now: {} candles, {} trades, newest close {:?}, last trade {:?}",
        candles.len(),
        chart.seen_trades().len(),
        candles.close().last(),
        chart.seen_trades().price().last()
    );
    Ok(())
}
