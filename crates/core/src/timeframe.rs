//! Other timeframes: the candles of a coarser period, built from the ones the chart was given, or of a period
//! shorter than theirs (seconds), built from the tape of trades.
//!
//! The chart keeps the data as it came (the base candles) and, when a timeframe is chosen, a
//! [`Resampled`] series next to it. The resampled series is built only from the base candles (or the trades)
//! the chart may see, so during a replay the last candle of a 15 minute series is honestly still forming:
//! it holds the 1 minute bars shown so far and nothing else.

use crate::{Candle, CandleView, Candles, Time, TradeView};

/// The timeframes offered by [`TimeframeCtl::cycle`], in milliseconds, when the data allows them.
const STANDARD: [i64; 11] = [
    60_000, 180_000, 300_000, 900_000, 1_800_000, 3_600_000, 7_200_000, 14_400_000, 21_600_000, 43_200_000, 86_400_000,
];

/// Candles of `span` milliseconds built from finer ones, kept up to date as the base grows.
#[derive(Debug, Clone)]
pub struct Resampled {
    span: i64,
    candles: Candles,
    /// How many base candles (or trades) are in, and the time of the last of them: to notice that the base was
    /// cut back.
    used: usize,
    last: Option<Time>,
    /// Whether it was built from trades rather than candles.
    from_tape: bool,
}

impl Resampled {
    pub fn new(span: i64) -> Self {
        Self {
            span: span.max(1),
            candles: Candles::new(),
            used: 0,
            last: None,
            from_tape: false,
        }
    }

    pub fn span(&self) -> i64 {
        self.span
    }

    /// Whether its bars are made of trades: a period shorter than the candles'.
    pub fn from_tape(&self) -> bool {
        self.from_tape
    }

    pub fn candles(&self) -> &Candles {
        &self.candles
    }

    fn bucket(&self, t: Time) -> Time {
        Time(t.0.div_euclid(self.span) * self.span)
    }

    /// Brings the series up to date with `base`, everything the chart may see of the finer candles. Growing
    /// or changing at the end (a live candle) costs only the period that is forming; a base that was cut
    /// back (a replay restarted) makes it start over.
    pub fn sync(&mut self, base: CandleView<'_>) {
        self.source(false);
        let time = base.time();
        let intact = self.used <= base.len() && (self.used == 0 || Some(time[self.used - 1]) == self.last);
        let from = if intact && self.used > 0 {
            // The period the newest candle belongs to is redone: that candle may have changed since.
            let open = self.bucket(time[self.used - 1]);
            if self.candles.view().time().last() == Some(&open) {
                self.candles.pop();
            }
            time.partition_point(|&x| x < open)
        } else {
            self.candles.clear();
            0
        };
        let mut current: Option<Candle> = None;
        for i in from..base.len() {
            let c = base.get(i).expect("in range");
            let open = self.bucket(c.time);
            match current.as_mut() {
                Some(bar) if bar.time == open => {
                    bar.high = bar.high.max(c.high);
                    bar.low = bar.low.min(c.low);
                    bar.close = c.close;
                    bar.volume += c.volume;
                }
                _ => {
                    if let Some(done) = current.take() {
                        self.candles.push(done).expect("buckets increase");
                    }
                    current = Some(Candle { time: open, ..c });
                }
            }
        }
        if let Some(done) = current {
            self.candles.push(done).expect("buckets increase");
        }
        self.used = base.len();
        self.last = time.last().copied();
    }

    /// Brings the series up to date with `tape`, every trade the chart may see: bars of a period shorter than the
    /// candles', made of the trades themselves. As with [`sync`](Self::sync), a tape grown at its end costs only
    /// the period forming, one cut back or with trades put before (earlier days loaded) starts over.
    pub fn sync_trades(&mut self, tape: TradeView<'_>) {
        self.source(true);
        let (time, price, qty) = (tape.time(), tape.price(), tape.qty());
        let intact = self.used <= tape.len() && (self.used == 0 || Some(time[self.used - 1]) == self.last);
        let from = if intact && self.used > 0 {
            let open = self.bucket(time[self.used - 1]);
            if self.candles.view().time().last() == Some(&open) {
                self.candles.pop();
            }
            time.partition_point(|&x| x < open)
        } else {
            self.candles.clear();
            0
        };
        let mut current: Option<Candle> = None;
        for i in from..tape.len() {
            let (open, p) = (self.bucket(time[i]), price[i]);
            match current.as_mut() {
                Some(bar) if bar.time == open => {
                    bar.high = bar.high.max(p);
                    bar.low = bar.low.min(p);
                    bar.close = p;
                    bar.volume += qty[i];
                }
                _ => {
                    if let Some(done) = current.take() {
                        self.candles.push(done).expect("buckets increase");
                    }
                    current = Some(Candle {
                        time: open,
                        open: p,
                        high: p,
                        low: p,
                        close: p,
                        volume: qty[i],
                    });
                }
            }
        }
        if let Some(done) = current {
            self.candles.push(done).expect("buckets increase");
        }
        self.used = tape.len();
        self.last = time.last().copied();
    }

    /// Starts over when what it is built from changes between candles and trades.
    fn source(&mut self, from_tape: bool) {
        if self.from_tape != from_tape {
            self.from_tape = from_tape;
            self.candles.clear();
            self.used = 0;
            self.last = None;
        }
    }
}

/// The timeframe as a handler sees it: what it is, what the data allows, and a way to change it.
pub struct TimeframeCtl<'a> {
    base: CandleView<'a>,
    at: &'a mut Option<i64>,
}

impl<'a> TimeframeCtl<'a> {
    pub(crate) fn new(base: CandleView<'a>, at: &'a mut Option<i64>) -> Self {
        Self { base, at }
    }

    /// The period of the candles shown, in milliseconds; `None` for the data as it came.
    pub fn get(&self) -> Option<i64> {
        *self.at
    }

    /// Shows candles of `span` milliseconds (`None`: the data as it came).
    pub fn set(&mut self, span: Option<i64>) {
        *self.at = span.filter(|&s| s > 0);
    }

    /// The period of the data as it came, see [`native_step`].
    pub fn native(&self) -> Option<i64> {
        native_step(self.base)
    }

    /// The coarser periods that make sense for this data: standard ones that are whole multiples of the native period.
    pub fn options(&self) -> Vec<i64> {
        let Some(step) = self.native().filter(|&s| s > 0) else {
            return Vec::new();
        };
        STANDARD
            .iter()
            .copied()
            .filter(|&s| s > step && s % step == 0)
            .collect()
    }

    /// The next timeframe: native, then each coarser one, then native again.
    pub fn cycle(&mut self) {
        let options = self.options();
        let next = match *self.at {
            None => options.first().copied(),
            Some(now) => options.iter().copied().find(|&s| s > now),
        };
        *self.at = next;
    }
}

/// The period of candles as they came: the smallest gap between neighbouring candles among the first few
/// thousand. `None` with fewer than two candles.
pub fn native_step(candles: CandleView<'_>) -> Option<i64> {
    let time = candles.time();
    time[..time.len().min(4096)].windows(2).map(|w| w[1].0 - w[0].0).min()
}

/// A period as a short label: `30s`, `5m`, `4h`, `1d`.
pub fn span_label(ms: i64) -> String {
    match ms {
        m if m % 86_400_000 == 0 => format!("{}d", m / 86_400_000),
        m if m % 3_600_000 == 0 => format!("{}h", m / 3_600_000),
        m if m % 60_000 == 0 => format!("{}m", m / 60_000),
        m => format!("{}s", m / 1000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn minutes(n: i64) -> Candles {
        let mut c = Candles::new();
        for i in 0..n {
            let p = 100.0 + i as f64;
            c.push(Candle {
                time: Time(i * MIN),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p + 0.5,
                volume: 2.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn periods_are_built_from_the_candles_inside_them() {
        let base = minutes(12);
        let mut r = Resampled::new(5 * MIN);
        r.sync(base.view());
        let v = r.candles().view();
        assert_eq!(v.time(), [Time(0), Time(5 * MIN), Time(10 * MIN)]);
        assert_eq!(v.open()[0], 100.0);
        assert_eq!(v.close()[0], 104.5);
        assert_eq!(v.high()[0], 105.0);
        assert_eq!(v.low()[0], 99.0);
        assert_eq!(v.volume()[0], 10.0);
        assert_eq!(v.volume()[2], 4.0, "the forming period holds what exists");
    }

    #[test]
    fn a_growing_or_changing_base_updates_only_the_forming_period() {
        let mut base = minutes(7);
        let mut r = Resampled::new(5 * MIN);
        r.sync(base.view());
        assert_eq!(r.candles().len(), 2);
        base.upsert(Candle {
            time: Time(6 * MIN),
            open: 106.0,
            high: 200.0,
            low: 105.0,
            close: 150.0,
            volume: 1.0,
        })
        .unwrap();
        base.push(Candle {
            time: Time(7 * MIN),
            open: 150.0,
            high: 151.0,
            low: 149.0,
            close: 150.0,
            volume: 3.0,
        })
        .unwrap();
        r.sync(base.view());
        let v = r.candles().view();
        assert_eq!(v.len(), 2);
        assert_eq!(
            (v.high()[1], v.close()[1], v.volume()[1]),
            (200.0, 150.0, 2.0 + 1.0 + 3.0)
        );
        let mut fresh = Resampled::new(5 * MIN);
        fresh.sync(base.view());
        assert_eq!(
            fresh.candles().view().close(),
            v.close(),
            "same as building it in one go"
        );
    }

    #[test]
    fn a_base_cut_back_starts_over_and_shows_nothing_of_what_was_cut() {
        let base = minutes(20);
        let mut r = Resampled::new(5 * MIN);
        r.sync(base.view());
        assert_eq!(r.candles().len(), 4);
        r.sync(base.view().until(Time(7 * MIN)));
        let v = r.candles().view();
        assert_eq!(v.len(), 2);
        assert_eq!(v.close()[1], 107.5, "the last period ends at the last minute allowed");
        assert_eq!(v.volume()[1], 6.0);
    }

    #[test]
    fn options_are_the_standard_periods_the_data_divides_into() {
        let base = minutes(50);
        let mut at = None;
        let mut ctl = TimeframeCtl::new(base.view(), &mut at);
        assert_eq!(ctl.native(), Some(MIN));
        assert_eq!(ctl.options()[..3], [3 * MIN, 5 * MIN, 15 * MIN]);
        ctl.cycle();
        assert_eq!(ctl.get(), Some(3 * MIN));
        for _ in 0..ctl.options().len() - 1 {
            ctl.cycle();
        }
        assert_eq!(ctl.get(), Some(86_400_000));
        ctl.cycle();
        assert_eq!(ctl.get(), None, "and around to the data as it came");
    }

    #[test]
    fn labels() {
        assert_eq!(
            [
                span_label(30_000),
                span_label(300_000),
                span_label(14_400_000),
                span_label(86_400_000)
            ],
            ["30s", "5m", "4h", "1d"]
        );
    }

    #[test]
    fn bars_of_seconds_follow_the_tape_as_it_grows_and_is_cut_back() {
        let mut tape = crate::Trades::new();
        for s in 0..10 {
            let trade = crate::Trade {
                time: Time(s * 1000),
                price: s as f64,
                qty: 1.0,
                side: crate::Side::Buy,
            };
            tape.push(trade).unwrap();
        }
        let bars = |r: &Resampled| {
            let v = r.candles().view();
            (0..v.len())
                .map(|i| v.get(i).unwrap())
                .map(|c| (c.time.0, c.open, c.high, c.low, c.close, c.volume))
                .collect::<Vec<_>>()
        };
        let mut r = Resampled::new(5000);
        r.sync_trades(tape.view().before(Time(7000)));
        assert_eq!(
            bars(&r),
            [(0, 0.0, 4.0, 0.0, 4.0, 5.0), (5000, 5.0, 6.0, 5.0, 6.0, 2.0)]
        );
        // Grown at its end: the bar forming takes the new trades in.
        r.sync_trades(tape.view());
        assert_eq!(bars(&r)[1], (5000, 5.0, 9.0, 5.0, 9.0, 5.0));
        // Cut back (a replay started earlier), then built from candles: it starts over each time.
        r.sync_trades(tape.view().before(Time(3000)));
        assert_eq!(bars(&r), [(0, 0.0, 2.0, 0.0, 2.0, 3.0)]);
        assert!(r.from_tape());
        r.sync(minutes(1).view());
        assert_eq!((r.candles().len(), r.from_tape()), (1, false));
        assert_eq!(r.candles().view().get(0).unwrap().volume, 2.0);
    }
}
