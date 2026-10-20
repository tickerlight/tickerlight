use core::ops::Range;

use crate::Time;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candle {
    pub time: Time,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// The gap assumed between bars when there is only one bar to go by: a minute.
const FALLBACK_GAP: f64 = 60_000.0;

/// A candle was pushed with a time not greater than the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfOrder {
    pub last: Time,
    pub got: Time,
}

/// Candles stored as columns, sorted by strictly increasing time.
#[derive(Debug, Default, Clone)]
pub struct Candles {
    time: Vec<Time>,
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
}

impl Candles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            time: Vec::with_capacity(capacity),
            open: Vec::with_capacity(capacity),
            high: Vec::with_capacity(capacity),
            low: Vec::with_capacity(capacity),
            close: Vec::with_capacity(capacity),
            volume: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, candle: Candle) -> Result<(), OutOfOrder> {
        if let Some(&last) = self.time.last()
            && candle.time <= last
        {
            return Err(OutOfOrder { last, got: candle.time });
        }
        self.time.push(candle.time);
        self.open.push(candle.open);
        self.high.push(candle.high);
        self.low.push(candle.low);
        self.close.push(candle.close);
        self.volume.push(candle.volume);
        Ok(())
    }

    /// Live data: a candle at the time of the last one replaces it (the forming candle got a new
    /// tick), a newer one is appended, an older one is an error.
    pub fn upsert(&mut self, candle: Candle) -> Result<(), OutOfOrder> {
        if self.time.last() == Some(&candle.time) {
            self.set_values(self.time.len() - 1, candle);
            return Ok(());
        }
        self.push(candle)
    }

    /// Gives the candle at `index` the prices and volume of `candle`; its time stays.
    pub(crate) fn set_values(&mut self, index: usize, candle: Candle) {
        self.open[index] = candle.open;
        self.high[index] = candle.high;
        self.low[index] = candle.low;
        self.close[index] = candle.close;
        self.volume[index] = candle.volume;
    }

    /// Removes the newest candle.
    pub fn pop(&mut self) {
        self.time.pop();
        self.open.pop();
        self.high.pop();
        self.low.pop();
        self.close.pop();
        self.volume.pop();
    }

    pub fn clear(&mut self) {
        self.time.clear();
        self.open.clear();
        self.high.clear();
        self.low.clear();
        self.close.clear();
        self.volume.clear();
    }

    pub fn len(&self) -> usize {
        self.time.len()
    }

    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }

    pub fn view(&self) -> CandleView<'_> {
        CandleView {
            time: &self.time,
            open: &self.open,
            high: &self.high,
            low: &self.low,
            close: &self.close,
            volume: &self.volume,
        }
    }
}

/// Read-only window into candles.
///
/// This is the only way plugins, scales and autoscale see data. A view can only
/// shrink ([`slice`](Self::slice), [`until`](Self::until)), never grow, so once
/// the replay cursor has cut the data, nothing downstream can reach past it.
#[derive(Debug, Clone, Copy)]
pub struct CandleView<'a> {
    time: &'a [Time],
    open: &'a [f64],
    high: &'a [f64],
    low: &'a [f64],
    close: &'a [f64],
    volume: &'a [f64],
}

impl<'a> CandleView<'a> {
    pub fn len(&self) -> usize {
        self.time.len()
    }

    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }

    pub fn time(&self) -> &'a [Time] {
        self.time
    }

    pub fn open(&self) -> &'a [f64] {
        self.open
    }

    pub fn high(&self) -> &'a [f64] {
        self.high
    }

    pub fn low(&self) -> &'a [f64] {
        self.low
    }

    pub fn close(&self) -> &'a [f64] {
        self.close
    }

    pub fn volume(&self) -> &'a [f64] {
        self.volume
    }

    pub fn get(&self, index: usize) -> Option<Candle> {
        Some(Candle {
            time: *self.time.get(index)?,
            open: self.open[index],
            high: self.high[index],
            low: self.low[index],
            close: self.close[index],
            volume: self.volume[index],
        })
    }

    /// The time (milliseconds, fractional) at a fractional bar position: bar `i` opens at its time and the
    /// space between two bars is interpolated; before the first and after the last bar it goes on at the pace of
    /// the nearest pair. `None` without bars. With [`bar_of_time`](Self::bar_of_time) it lets something be
    /// pinned to a moment instead of a bar index, so it stays put when the chart shows another timeframe.
    pub fn time_of_bar(&self, bar: f64) -> Option<f64> {
        let n = self.time.len();
        if n == 0 {
            return None;
        }
        let at = |i: usize| self.time[i].0 as f64;
        let (first_gap, last_gap) = if n > 1 {
            (at(1) - at(0), at(n - 1) - at(n - 2))
        } else {
            (FALLBACK_GAP, FALLBACK_GAP)
        };
        Some(if bar < 0.0 {
            at(0) + bar * first_gap
        } else if bar >= (n - 1) as f64 {
            at(n - 1) + (bar - (n - 1) as f64) * last_gap
        } else {
            let i = bar.floor() as usize;
            at(i) + (bar - i as f64) * (at(i + 1) - at(i))
        })
    }

    /// The fractional bar position of a time in milliseconds, the inverse of [`time_of_bar`](Self::time_of_bar).
    pub fn bar_of_time(&self, time: f64) -> Option<f64> {
        let n = self.time.len();
        if n == 0 {
            return None;
        }
        let at = |i: usize| self.time[i].0 as f64;
        let (first_gap, last_gap) = if n > 1 {
            (at(1) - at(0), at(n - 1) - at(n - 2))
        } else {
            (FALLBACK_GAP, FALLBACK_GAP)
        };
        let passed = self.time.partition_point(|&x| (x.0 as f64) <= time);
        Some(match passed {
            0 => (time - at(0)) / first_gap,
            p if p == n => (n - 1) as f64 + (time - at(n - 1)) / last_gap,
            p => (p - 1) as f64 + (time - at(p - 1)) / (at(p) - at(p - 1)),
        })
    }

    /// Sub-view by index range. Panics if the range is out of bounds, like slices.
    pub fn slice(&self, range: Range<usize>) -> Self {
        Self {
            time: &self.time[range.clone()],
            open: &self.open[range.clone()],
            high: &self.high[range.clone()],
            low: &self.low[range.clone()],
            close: &self.close[range.clone()],
            volume: &self.volume[range],
        }
    }

    /// Candles with `time <= t`.
    pub fn until(&self, t: Time) -> Self {
        let end = self.time.partition_point(|&x| x <= t);
        self.slice(0..end)
    }

    /// Indices of candles with `from <= time <= to`.
    pub fn index_range(&self, from: Time, to: Time) -> Range<usize> {
        let start = self.time.partition_point(|&x| x < from);
        let end = self.time.partition_point(|&x| x <= to);
        start..end.max(start)
    }

    /// `(lowest low, highest high)` of the view, `None` if it is empty.
    pub fn extent(&self) -> Option<(f64, f64)> {
        if self.is_empty() {
            return None;
        }
        let lo = self.low.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = self.high.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(n: i64) -> Candles {
        let mut c = Candles::new();
        for i in 0..n {
            let p = i as f64;
            c.push(Candle {
                time: Time(i * 60_000),
                open: p,
                high: p + 2.0,
                low: p - 1.0,
                close: p + 1.0,
                volume: 1.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn rejects_out_of_order() {
        let mut c = candles(3);
        let bad = Candle {
            time: Time(60_000),
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
        };
        assert_eq!(
            c.push(bad),
            Err(OutOfOrder {
                last: Time(120_000),
                got: Time(60_000)
            })
        );
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn until_hides_the_future_from_extent() {
        let c = candles(100);
        let seen = c.view().until(Time(10 * 60_000));
        assert_eq!(seen.len(), 11);
        // Highest high among candles 0..=10 is 10 + 2; candle 99 must not leak in.
        assert_eq!(seen.extent(), Some((-1.0, 12.0)));
    }

    #[test]
    fn until_before_start_is_empty() {
        let c = candles(5);
        assert!(c.view().until(Time(-1)).is_empty());
        assert_eq!(c.view().until(Time(-1)).extent(), None);
    }

    #[test]
    fn index_range_is_inclusive_on_both_ends() {
        let c = candles(10);
        assert_eq!(c.view().index_range(Time(2 * 60_000), Time(4 * 60_000)), 2..5);
        assert_eq!(c.view().index_range(Time(1), Time(2)), 1..1);
    }

    #[test]
    fn moments_and_bar_positions_convert_both_ways_including_outside_the_data() {
        let c = candles(10);
        let v = c.view();
        assert_eq!(v.time_of_bar(3.0), Some(180_000.0));
        assert_eq!(v.time_of_bar(3.5), Some(210_000.0));
        assert_eq!(v.bar_of_time(210_000.0), Some(3.5));
        for bar in [-2.5, 0.0, 4.25, 9.0, 12.75] {
            let t = v.time_of_bar(bar).unwrap();
            assert!((v.bar_of_time(t).unwrap() - bar).abs() < 1e-9, "bar {bar}");
        }
        // Past the ends the pace of the nearest bars goes on.
        assert_eq!(v.time_of_bar(11.0), Some(660_000.0));
        assert_eq!(v.time_of_bar(-1.0), Some(-60_000.0));
        assert_eq!(v.slice(0..0).time_of_bar(1.0), None);
        assert_eq!(
            v.slice(0..1).bar_of_time(120_000.0),
            Some(2.0),
            "one bar: a minute is assumed"
        );
    }
}
