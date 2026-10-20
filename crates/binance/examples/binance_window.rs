//! A live-data chart in a window: the last days of 1 minute candles from Binance, then streamed live.
//! Drag and wheel to navigate, `R` to start a replay, `B`/`S`/`C` to trade.
//!
//! cargo run --release -p tickerlight-binance --example binance_window -- BTCUSDT

use tickerlight_binance::{Binance, Interval, KlineStream, TradeStream};
use tickerlight_core::{Chart, Color, Sma, Time};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "BTCUSDT".into());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let candles = Binance::new().klines(&symbol, Interval::M1, Time(now - 7 * 24 * 3_600_000), Time(now))?;
    let mut chart = Chart::new(candles).with(Sma::new(50, Color::rgb(255, 183, 77)));
    chart.add_source(KlineStream::connect(&symbol, Interval::M1));
    chart.add_source(TradeStream::connect(&symbol));
    tickerlight_backend_wgpu::run(chart)
}
