use core::ops::Range;

use crate::CandleView;

/// Groups of consecutive candles merged into one candle each.
#[derive(Debug, Default, Clone)]
pub struct Columns {
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    /// Index of the first candle of each group.
    pub first: Vec<u32>,
    /// Number of candles in each group.
    pub count: Vec<u32>,
}

impl Columns {
    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// Merges the candles of `range` into groups of `group` consecutive candles.
///
/// Groups are aligned to multiples of `group` in the whole view, not to `range.start`, so
/// while the chart is panned the groups stay the same and nothing flickers. The first and last
/// groups are completed with the neighbouring candles of the view (never past its end).
/// A group keeps the first open, the last close, the highest high and the lowest low, so
/// a million candles cost only as much as the screen is wide. `group == 1` keeps every candle.
pub fn aggregate(view: CandleView<'_>, range: Range<usize>, group: usize) -> Columns {
    let group = group.max(1);
    let len = view.len();
    let range = range.start.min(len)..range.end.min(len);
    let mut out = Columns::default();
    if range.is_empty() {
        return out;
    }
    let groups = range.end.div_ceil(group) - range.start / group;
    out.open.reserve(groups);
    out.high.reserve(groups);
    out.low.reserve(groups);
    out.close.reserve(groups);
    out.first.reserve(groups);
    out.count.reserve(groups);

    let (open, high, low, close) = (view.open(), view.high(), view.low(), view.close());
    let mut start = range.start / group * group;
    while start < range.end {
        let end = (start + group).min(len);
        out.open.push(open[start]);
        out.close.push(close[end - 1]);
        out.high
            .push(high[start..end].iter().copied().fold(f64::NEG_INFINITY, f64::max));
        out.low
            .push(low[start..end].iter().copied().fold(f64::INFINITY, f64::min));
        out.first.push(start as u32);
        out.count.push((end - start) as u32);
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles, Time};

    fn candles(n: i64) -> Candles {
        let mut c = Candles::new();
        for i in 0..n {
            let p = i as f64;
            c.push(Candle {
                time: Time(i),
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
    fn group_of_one_keeps_every_candle() {
        let c = candles(5);
        let cols = aggregate(c.view(), 0..5, 1);
        assert_eq!(cols.len(), 5);
        assert_eq!(cols.open, [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(cols.first, [0, 1, 2, 3, 4]);
        assert_eq!(cols.count, [1; 5]);
    }

    #[test]
    fn merges_groups_preserving_ohlc() {
        let c = candles(10);
        let cols = aggregate(c.view(), 0..10, 5);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols.open, [0.0, 5.0]);
        assert_eq!(cols.close, [5.0, 10.0]);
        assert_eq!(cols.high, [6.0, 11.0]);
        assert_eq!(cols.low, [-1.0, 4.0]);
        assert_eq!(cols.first, [0, 5]);
        assert_eq!(cols.count, [5, 5]);
    }

    #[test]
    fn groups_are_aligned_to_the_whole_view_not_to_the_range() {
        let c = candles(20);
        // Range starts mid-group: group 5..10 is completed with its earlier candles.
        let cols = aggregate(c.view(), 7..13, 5);
        assert_eq!(cols.first, [5, 10]);
        assert_eq!(cols.count, [5, 5]);
        // The same groups regardless of where the range starts inside them.
        let shifted = aggregate(c.view(), 9..11, 5);
        assert_eq!(shifted.first, [5, 10]);
    }

    #[test]
    fn last_group_stops_at_the_end_of_the_view() {
        let c = candles(7);
        let cols = aggregate(c.view(), 0..7, 3);
        assert_eq!(cols.first, [0, 3, 6]);
        assert_eq!(cols.count, [3, 3, 1]);
        assert_eq!(*cols.close.last().unwrap(), 7.0);
    }

    #[test]
    fn does_not_read_past_a_cut_view() {
        let c = candles(20);
        let seen = c.view().until(Time(6));
        let cols = aggregate(seen, 0..seen.len(), 5);
        assert_eq!(cols.count, [5, 2]);
        assert_eq!(cols.high[1], 8.0); // candle 7 and later never leak in
    }

    #[test]
    fn empty_inputs_give_no_columns() {
        let c = Candles::new();
        assert!(aggregate(c.view(), 0..0, 4).is_empty());
        assert!(aggregate(candles(5).view(), 3..3, 4).is_empty());
        assert!(aggregate(candles(5).view(), 9..12, 4).is_empty());
    }
}
