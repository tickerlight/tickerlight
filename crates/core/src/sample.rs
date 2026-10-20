//! Deterministic sample data for examples, benchmarks and tests. No external crates.

use crate::{Candle, Candles, Time};

/// One-minute candles of a random walk starting at 100.
pub fn random_walk(n: usize) -> Candles {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64 // [0, 1)
    };
    let mut candles = Candles::with_capacity(n);
    let mut price = 100.0f64;
    for i in 0..n {
        let open = price;
        let close = open + (next() - 0.5) * 2.0;
        let high = open.max(close) + next();
        let low = open.min(close) - next();
        price = close;
        candles
            .push(Candle {
                time: Time(i as i64 * 60_000),
                open,
                high,
                low,
                close,
                volume: next() * 100.0,
            })
            .unwrap();
    }
    candles
}
