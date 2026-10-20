//! Downloads a little data and prints what came back. Needs a network.
//!
//! cargo run -p tickerlight-binance --example fetch -- BTCUSDT 10   # symbol, minutes of trades

use tickerlight_binance::{Binance, Interval};
use tickerlight_core::Time;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "BTCUSDT".into());
    let minutes: i64 = std::env::args().nth(2).and_then(|m| m.parse().ok()).unwrap_or(1);
    let api = Binance::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;

    let candles = api.klines(&symbol, Interval::M1, Time(now - 3 * 24 * 3_600_000), Time(now))?;
    let view = candles.view();
    println!("{} 1m candles", view.len());
    if let (Some(first), Some(last)) = (view.get(0), view.get(view.len().saturating_sub(1))) {
        println!("first {first:?}\nlast  {last:?}");
    }

    let trades = api.agg_trades(&symbol, Time(now - minutes * 60_000), Time(now))?;
    println!("{} trades in the last {minutes} min", trades.len());
    let v = trades.view();
    assert!(v.time().windows(2).all(|w| w[0] <= w[1]), "the tape is ordered");
    if let Some(t) = trades.view().get(0) {
        println!("first {t:?}");
    }
    Ok(())
}
