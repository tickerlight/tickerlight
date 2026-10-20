use core::ops::Range;

use crate::{Candle, Candles, Time};

/// Which side took liquidity: a `Buy` hit the asks, a `Sell` hit the bids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Side {
    Buy,
    Sell,
}

/// One executed trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trade {
    pub time: Time,
    pub price: f64,
    pub qty: f64,
    /// The aggressor.
    pub side: Side,
}

/// The tape: every trade, in time order, stored by column. Footprints, volume profiles, delta and
/// candles of any timeframe are all computed from this.
#[derive(Debug, Default, Clone)]
pub struct Trades {
    time: Vec<Time>,
    price: Vec<f64>,
    qty: Vec<f64>,
    side: Vec<Side>,
}

impl Trades {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            time: Vec::with_capacity(capacity),
            price: Vec::with_capacity(capacity),
            qty: Vec::with_capacity(capacity),
            side: Vec::with_capacity(capacity),
        }
    }

    /// Appends a trade. Trades at the same time are fine; going back in time is not.
    pub fn push(&mut self, trade: Trade) -> Result<(), crate::OutOfOrder> {
        if let Some(&last) = self.time.last()
            && trade.time < last
        {
            return Err(crate::OutOfOrder { last, got: trade.time });
        }
        self.time.push(trade.time);
        self.price.push(trade.price);
        self.qty.push(trade.qty);
        self.side.push(trade.side);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.time.len()
    }

    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }

    pub fn view(&self) -> TradeView<'_> {
        TradeView {
            time: &self.time,
            price: &self.price,
            qty: &self.qty,
            side: &self.side,
        }
    }
}

/// Read-only window into trades. Like `CandleView` it can only shrink, so once the replay cursor cut
/// the tape, nothing downstream can reach past the cut. The default is empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct TradeView<'a> {
    time: &'a [Time],
    price: &'a [f64],
    qty: &'a [f64],
    side: &'a [Side],
}

impl<'a> TradeView<'a> {
    pub fn len(&self) -> usize {
        self.time.len()
    }

    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }

    pub fn time(&self) -> &'a [Time] {
        self.time
    }

    pub fn price(&self) -> &'a [f64] {
        self.price
    }

    pub fn qty(&self) -> &'a [f64] {
        self.qty
    }

    pub fn side(&self) -> &'a [Side] {
        self.side
    }

    pub fn get(&self, i: usize) -> Option<Trade> {
        Some(Trade {
            time: *self.time.get(i)?,
            price: self.price[i],
            qty: self.qty[i],
            side: self.side[i],
        })
    }

    /// Sub-view by index range. Panics if the range is out of bounds, like slices.
    pub fn slice(&self, range: Range<usize>) -> Self {
        Self {
            time: &self.time[range.clone()],
            price: &self.price[range.clone()],
            qty: &self.qty[range.clone()],
            side: &self.side[range],
        }
    }

    /// Trades with `time <= t`.
    pub fn until(&self, t: Time) -> Self {
        self.slice(0..self.time.partition_point(|&x| x <= t))
    }

    /// Trades with `time < t`.
    pub fn before(&self, t: Time) -> Self {
        self.slice(0..self.time.partition_point(|&x| x < t))
    }

    /// Indices of trades with `from <= time < to`.
    pub fn index_range(&self, from: Time, to: Time) -> Range<usize> {
        let start = self.time.partition_point(|&x| x < from);
        let end = self.time.partition_point(|&x| x < to);
        start..end.max(start)
    }

    /// Sub-view of the trades in `[from, to)`.
    pub fn between(&self, from: Time, to: Time) -> Self {
        self.slice(self.index_range(from, to))
    }
}

/// Builds candles of `interval_ms` from trades. Intervals without trades get no candle.
/// Candle times are the start of their interval.
pub fn candles_from_trades(trades: TradeView<'_>, interval_ms: i64) -> Candles {
    let interval = interval_ms.max(1);
    let mut out = Candles::new();
    let mut i = 0;
    while i < trades.len() {
        let bucket = trades.time[i].0.div_euclid(interval) * interval;
        let end_time = bucket + interval;
        let mut candle = Candle {
            time: Time(bucket),
            open: trades.price[i],
            high: trades.price[i],
            low: trades.price[i],
            close: trades.price[i],
            volume: 0.0,
        };
        while i < trades.len() && trades.time[i].0 < end_time {
            let p = trades.price[i];
            candle.high = candle.high.max(p);
            candle.low = candle.low.min(p);
            candle.close = p;
            candle.volume += trades.qty[i];
            i += 1;
        }
        out.push(candle).expect("buckets only grow");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tape() -> Trades {
        let mut t = Trades::new();
        for (time, price, qty, side) in [
            (0, 10.0, 1.0, Side::Buy),
            (500, 12.0, 2.0, Side::Sell),
            (999, 9.0, 1.0, Side::Sell),
            (1000, 11.0, 4.0, Side::Buy),
            (1000, 11.5, 1.0, Side::Buy),
            (3500, 20.0, 1.0, Side::Sell),
        ] {
            t.push(Trade {
                time: Time(time),
                price,
                qty,
                side,
            })
            .unwrap();
        }
        t
    }

    #[test]
    fn same_time_is_fine_going_back_is_not() {
        let mut t = tape();
        assert!(
            t.push(Trade {
                time: Time(3500),
                price: 1.0,
                qty: 1.0,
                side: Side::Buy
            })
            .is_ok()
        );
        assert!(
            t.push(Trade {
                time: Time(3499),
                price: 1.0,
                qty: 1.0,
                side: Side::Buy
            })
            .is_err()
        );
        assert_eq!(t.len(), 7);
    }

    #[test]
    fn views_only_shrink_and_cut_by_time() {
        let t = tape();
        let v = t.view();
        assert_eq!(v.until(Time(999)).len(), 3);
        assert_eq!(v.before(Time(1000)).len(), 3);
        assert_eq!(v.until(Time(1000)).len(), 5, "both trades at 1000");
        assert_eq!(v.between(Time(500), Time(1000)).len(), 2);
        assert_eq!(v.between(Time(2000), Time(3000)).len(), 0);
        assert_eq!(
            v.between(Time(5), Time(1)).len(),
            0,
            "reversed range is empty, not a panic"
        );
        assert_eq!(v.get(1).unwrap().side, Side::Sell);
        assert!(v.get(99).is_none());
    }

    #[test]
    fn candles_from_trades_follow_ohlcv_rules() {
        let c = candles_from_trades(tape().view(), 1000);
        let view = c.view();
        assert_eq!(view.len(), 3, "the interval 2000..3000 is empty");
        let first = view.get(0).unwrap();
        assert_eq!(
            (first.time, first.open, first.high, first.low, first.close, first.volume),
            (Time(0), 10.0, 12.0, 9.0, 9.0, 4.0)
        );
        let second = view.get(1).unwrap();
        assert_eq!(
            (second.time, second.open, second.close, second.volume),
            (Time(1000), 11.0, 11.5, 5.0)
        );
        assert_eq!(view.get(2).unwrap().time, Time(3000));
    }

    #[test]
    fn candles_from_no_trades_is_empty() {
        assert!(candles_from_trades(Trades::new().view(), 60_000).is_empty());
    }

    #[test]
    fn negative_times_bucket_downwards() {
        let mut t = Trades::new();
        t.push(Trade {
            time: Time(-1),
            price: 1.0,
            qty: 1.0,
            side: Side::Buy,
        })
        .unwrap();
        let c = candles_from_trades(t.view(), 1000);
        assert_eq!(c.view().get(0).unwrap().time, Time(-1000));
    }
}
