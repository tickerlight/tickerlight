use crate::calendar::Zone;
use crate::{Candles, Time, Trades, native_step};

/// How a replay that plays by time stands at `moment`: how many bars are shown, and whether the last of them is
/// still forming. A bar is complete once its period is over. The one open at the moment forms from the trades of
/// the tape before it; without any it is not shown until it is complete.
pub(crate) fn cut(candles: &Candles, trades: &Trades, moment: Time) -> (usize, bool) {
    let time = candles.view().time();
    let step = step(candles);
    let complete = time.partition_point(|t| t.0 + step <= moment.0);
    let forming = time
        .get(complete)
        .is_some_and(|&open| open < moment && !trades.view().between(open, moment).is_empty());
    (complete + usize::from(forming), forming)
}

/// The period of a bar of the data.
fn step(candles: &Candles) -> i64 {
    native_step(candles.view()).unwrap_or(1).max(1)
}

/// The first moment after `now` when the clock of `zone` reads `minutes` after midnight.
fn next_clock(now: Time, minutes: u32, zone: Zone) -> Time {
    const DAY: i64 = 86_400_000;
    let local = zone.local(now).0;
    let mut at = local.div_euclid(DAY) * DAY + i64::from(minutes) * 60_000;
    if at <= local {
        at += DAY;
    }
    let at = zone.utc(Time(at));
    // A clock set forward that day may skip it: the day after.
    if at > now {
        at
    } else {
        zone.utc(Time(zone.local(at).0 + DAY))
    }
}

/// The replay cursor as a handler sees it: how many bars are shown, and ways to reveal more: bar by bar, or by
/// playing on in time, where the newest bar grows trade by trade from the tape.
///
/// It only moves forward. Bars that were revealed are never hidden again (except by starting over
/// with [`start_at`](Self::start_at)), so what the user has seen stays consistent with the virtual
/// trades made on it. A handler learns how many bars exist, but never their values: to read prices
/// it has the bounded view in the event context.
pub struct Cursor<'a> {
    candles: &'a Candles,
    trades: &'a Trades,
    at: &'a mut Option<Time>,
    /// Where a replay that plays by time stands; `None` when it moved bar by bar.
    moment: &'a mut Option<Time>,
    /// The period of the candles the chart shows, when it is not the data as it came.
    timeframe: Option<i64>,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(
        candles: &'a Candles,
        trades: &'a Trades,
        at: &'a mut Option<Time>,
        moment: &'a mut Option<Time>,
        timeframe: Option<i64>,
    ) -> Self {
        Self {
            candles,
            trades,
            at,
            moment,
            timeframe,
        }
    }

    /// `true` while the chart shows only part of the history.
    pub fn is_replaying(&self) -> bool {
        self.at.is_some()
    }

    /// Bars that are shown now, the one forming included.
    pub fn shown(&self) -> usize {
        match *self.at {
            Some(t) => self.candles.view().until(t).len(),
            None => self.candles.len(),
        }
    }

    /// Bars that exist, shown or not.
    pub fn total(&self) -> usize {
        self.candles.len()
    }

    /// Where the replay stands in time: the moment it played to, or the close of the newest bar shown when it
    /// moved bar by bar. `None` when not replaying.
    pub fn moment(&self) -> Option<Time> {
        let at = (*self.at)?;
        Some(self.moment.unwrap_or(Time(at.0 + step(self.candles))))
    }

    /// Whether the newest bar shown is still forming: it holds only the trades before the moment played to.
    pub fn forming(&self) -> bool {
        match (*self.at, *self.moment) {
            (Some(at), Some(moment)) => at.0 + step(self.candles) > moment.0,
            _ => false,
        }
    }

    /// Starts (or restarts) the replay with the first `shown` bars visible, at least one.
    pub fn start_at(&mut self, shown: usize) {
        let shown = shown.clamp(1, self.candles.len().max(1));
        *self.at = self.candles.view().time().get(shown - 1).copied();
        *self.moment = None;
    }

    /// Starts (or restarts) the replay so that the bar of the chart that opens at `open` is complete and
    /// nothing after it is shown. On a higher timeframe that is the last finer bar of the period; on bars of
    /// seconds the replay stands at the moment that bar ends.
    pub fn start_at_time(&mut self, open: Time) {
        let end = open.0.saturating_add(self.timeframe.unwrap_or(1));
        if self.seconds().is_some() {
            self.stand_at(Time(end));
            return;
        }
        let time = self.candles.view().time();
        let shown = time.partition_point(|&x| x.0 < end);
        *self.at = time.get(shown.max(1) - 1).copied();
        *self.moment = None;
    }

    /// The period of the bars shown when it is shorter than the data's own: bars made of the trades.
    fn seconds(&self) -> Option<i64> {
        self.timeframe.filter(|&span| span < step(self.candles))
    }

    /// Puts the replay at `moment`, the bar open then forming from the trades before it.
    fn stand_at(&mut self, moment: Time) {
        let (shown, _) = cut(self.candles, self.trades, moment);
        *self.at = self.candles.view().time().get(shown.max(1) - 1).copied();
        *self.moment = Some(moment);
    }

    /// Reveals up to `bars` more bars of the chart: on a higher timeframe whole periods, the one forming
    /// finished first. A bar forming from the tape counts as hidden: it is finished first too. Returns how many
    /// bars of the data were revealed (none at the end of the data). Does nothing when not replaying. On bars of
    /// seconds it plays to the end of each next one that has a trade, and returns how many of those it did.
    pub fn advance(&mut self, bars: usize) -> usize {
        if !self.is_replaying() || bars == 0 {
            return 0;
        }
        if let (Some(span), Some(mut now)) = (self.seconds(), self.moment()) {
            let time = self.trades.view().time();
            let mut done = 0;
            while done < bars {
                // The trades before the moment are seen: the next is the first at it or after.
                let Some(next) = time.get(time.partition_point(|&t| t < now)) else {
                    break;
                };
                now = Time(next.0.div_euclid(span) * span + span);
                done += 1;
            }
            if done > 0 {
                self.stand_at(now);
            }
            return done;
        }
        let before = self.shown() - usize::from(self.forming());
        let time = self.candles.view().time();
        let after = match (self.timeframe, time.get(before)) {
            (Some(span), Some(next)) => {
                // Up to the end of the period of the first hidden bar, and `bars - 1` periods after it.
                let end = next.0.div_euclid(span) * span + span * bars as i64;
                time.partition_point(|t| t.0 < end)
            }
            _ => (before + bars).min(self.candles.len()),
        };
        if after > before {
            *self.at = time.get(after - 1).copied();
            *self.moment = None;
        }
        after - before
    }

    /// Plays `ms` of market time on: the bars whose period is over by then are complete, and the one open then
    /// forms from the trades before that moment (see [`Cursor::forming`]). Time in which no bar is open (a market
    /// closed, a hole in the data) is skipped. Returns whether the replay moved: not at the end of the data, or
    /// when not replaying.
    pub fn play(&mut self, ms: i64) -> bool {
        let Some(now) = self.moment() else {
            return false;
        };
        if ms <= 0 {
            return false;
        }
        let time = self.candles.view().time();
        let step = step(self.candles);
        // The bar open now or next; past the last one's end there is nothing more to play.
        let Some(&open) = time.get(time.partition_point(|t| t.0 + step <= now.0)) else {
            return false;
        };
        let moment = Time((now.0 + ms).max(open.0));
        let (shown, _) = cut(self.candles, self.trades, moment);
        *self.at = time.get(shown.max(1) - 1).copied();
        *self.moment = Some(moment);
        true
    }

    /// Like [`play`](Self::play), but stops early at the first moment within the `ms` when a trade reaches one of
    /// `lines` from where the price was (with no trade in that time, when a bar completes with one in its range)
    /// or when the clock reads `clock`: minutes after midnight on the clock of its zone. Returns whether the replay
    /// moved, and whether it stopped early.
    pub fn play_until(&mut self, ms: i64, lines: &[f64], clock: Option<(u32, Zone)>) -> (bool, bool) {
        let Some(now) = self.moment() else {
            return (false, false);
        };
        let target = Time(now.0.saturating_add(ms.max(0)));
        let at_clock = clock
            .map(|(minutes, zone)| next_clock(now, minutes, zone))
            .filter(|&t| t <= target);
        let at_line = self.first_reach(now, target, lines);
        match at_clock.into_iter().chain(at_line).min() {
            Some(stop) => (self.play(stop.0 - now.0), true),
            None => (self.play(ms), false),
        }
    }

    /// The moment just after the first trade in `[from, to)` that reaches one of `lines` from the price before it;
    /// with no trade then, the end of the first bar completing by `to` with one in its range.
    fn first_reach(&self, from: Time, to: Time, lines: &[f64]) -> Option<Time> {
        if lines.is_empty() {
            return None;
        }
        let tape = self.trades.view();
        let played = tape.between(from, to);
        if !played.is_empty() {
            let mut last = tape.between(Time(i64::MIN), from).price().last().copied();
            for (&time, &price) in played.time().iter().zip(played.price()) {
                let reached =
                    |line: f64| last.is_some_and(|p| (p < line && price >= line) || (p > line && price <= line));
                if lines.iter().any(|&line| reached(line)) {
                    return Some(Time(time.0 + 1));
                }
                last = Some(price);
            }
            return None;
        }
        let view = self.candles.view();
        let (time, high, low) = (view.time(), view.high(), view.low());
        let step = step(self.candles);
        // The first bar not complete at `from`, on.
        for i in time.partition_point(|t| t.0 + step <= from.0)..time.len() {
            let end = time[i].0 + step;
            if end > to.0 {
                break;
            }
            if lines.iter().any(|&line| low[i] <= line && line <= high[i]) {
                return Some(Time(end));
            }
        }
        None
    }

    /// Leaves the replay: everything is shown again, and new live data shows up as it arrives.
    pub fn finish(&mut self) {
        *self.at = None;
        *self.moment = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Side, Trade};

    fn data(n: i64) -> Candles {
        let mut c = Candles::new();
        for i in 0..n {
            c.push(Candle {
                time: Time(i * 10),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 0.0,
            })
            .unwrap();
        }
        c
    }

    fn tape(times: &[i64]) -> Trades {
        let mut trades = Trades::new();
        for &t in times {
            trades
                .push(Trade {
                    time: Time(t),
                    price: 1.0,
                    qty: 1.0,
                    side: Side::Buy,
                })
                .unwrap();
        }
        trades
    }

    #[test]
    fn moves_forward_and_stops_at_the_end() {
        let candles = data(10);
        let trades = Trades::new();
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        assert!(!cursor.is_replaying());
        assert_eq!((cursor.shown(), cursor.total()), (10, 10));
        assert_eq!(cursor.advance(3), 0, "not replaying: nothing to reveal");

        cursor.start_at(4);
        assert!(cursor.is_replaying());
        assert_eq!(cursor.shown(), 4);
        assert_eq!(cursor.advance(3), 3);
        assert_eq!(cursor.shown(), 7);
        assert_eq!(cursor.advance(100), 3, "only three were left");
        assert_eq!(cursor.shown(), 10);
        assert_eq!(cursor.advance(1), 0);

        cursor.finish();
        assert!(!cursor.is_replaying());
    }

    #[test]
    fn playing_until_stops_at_the_clock_and_where_the_price_reaches_a_line() {
        const HOUR: i64 = 3_600_000;
        // Hourly bars from 1970-01-01 (winter in New York), bar i spanning i to i + 0.5.
        let mut candles = Candles::new();
        for i in 0..48 {
            let p = i as f64;
            let bar = Candle {
                time: Time(i * HOUR),
                open: p,
                high: p + 0.5,
                low: p,
                close: p + 0.5,
                volume: 1.0,
            };
            candles.push(bar).unwrap();
        }
        let trades = Trades::new();
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        cursor.start_at(1);
        let nine_thirty = Some((9 * 60 + 30, Zone::Utc));
        assert_eq!(cursor.play_until(30 * HOUR, &[], nine_thirty), (true, true));
        assert_eq!(cursor.moment(), Some(Time(9 * HOUR + HOUR / 2)));
        assert_eq!(
            cursor.play_until(30 * HOUR, &[], nine_thirty),
            (true, true),
            "the day after"
        );
        assert_eq!(cursor.moment(), Some(Time(33 * HOUR + HOUR / 2)));
        assert_eq!(cursor.play_until(HOUR, &[], nine_thirty), (true, false), "not that far");

        cursor.start_at(1);
        // 09:30 in New York is 14:30 UTC then.
        cursor.play_until(30 * HOUR, &[], Some((9 * 60 + 30, Zone::NewYork)));
        assert_eq!(cursor.moment(), Some(Time(14 * HOUR + HOUR / 2)));

        // Without the tape, at the end of the first bar with the line in its range.
        cursor.start_at(1);
        assert_eq!(cursor.play_until(30 * HOUR, &[5.2, 40.0], None), (true, true));
        assert_eq!(cursor.moment(), Some(Time(6 * HOUR)));
        assert_eq!(cursor.shown(), 6);

        // With it, just after the trade that reached the line from the price before it.
        let mut trades = Trades::new();
        for (time, price) in [(10, 1.0), (20, 2.0), (30, 3.0), (40, 2.5), (50, 1.5)] {
            let side = Side::Buy;
            trades
                .push(Trade {
                    time: Time(HOUR + time),
                    price,
                    qty: 1.0,
                    side,
                })
                .unwrap();
        }
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        cursor.start_at(1);
        assert_eq!(cursor.play_until(HOUR, &[2.8], None), (true, true));
        assert_eq!(cursor.moment(), Some(Time(HOUR + 31)));
        // Back under it at the next trade, and on down through the next line.
        assert_eq!(cursor.play_until(HOUR, &[2.8, 1.8], None), (true, true));
        assert_eq!(cursor.moment(), Some(Time(HOUR + 41)));
        assert_eq!(cursor.play_until(HOUR, &[2.8, 1.8], None), (true, true));
        assert_eq!(cursor.moment(), Some(Time(HOUR + 51)));
        // A line the price started at is not reached again by leaving it.
        cursor.start_at(1);
        assert_eq!(cursor.play_until(HOUR, &[1.0], None), (true, false));
    }

    #[test]
    fn on_a_higher_timeframe_a_step_is_a_whole_bar_of_the_chart() {
        // Bars every 10 ms shown as bars of 40: four to a bar of the chart.
        let candles = data(20);
        let trades = Trades::new();
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, Some(40));
        cursor.start_at(6);
        // The bar forming (times 40 to 70) is finished first.
        assert_eq!(cursor.advance(1), 2);
        assert_eq!(cursor.shown(), 8);
        assert_eq!(cursor.advance(1), 4);
        assert_eq!(cursor.advance(2), 8);
        assert_eq!(cursor.shown(), 20);
        assert_eq!(cursor.advance(1), 0);
    }

    #[test]
    fn playing_in_time_shows_a_bar_forming_only_from_its_trades() {
        // Bars every 10 ms; the tape has trades in the bar at 30 only.
        let candles = data(6);
        let trades = tape(&[31, 34, 38]);
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        assert!(!cursor.play(5), "not replaying");
        cursor.start_at(3);
        assert_eq!(cursor.moment(), Some(Time(30)), "the close of the newest bar");

        // Into the bar at 30: forming from the first trades.
        assert!(cursor.play(5));
        assert_eq!(
            (cursor.shown(), cursor.forming(), cursor.moment()),
            (4, true, Some(Time(35)))
        );
        // The bar at 40 has no tape: shown only once its period is over.
        assert!(cursor.play(10));
        assert_eq!((cursor.shown(), cursor.forming()), (4, false));
        assert!(cursor.play(5));
        assert_eq!((cursor.shown(), cursor.forming()), (5, false));

        // A step finishes the bar forming before it reveals more.
        cursor.start_at(3);
        cursor.play(5);
        assert_eq!(cursor.advance(1), 1);
        assert_eq!(
            (cursor.shown(), cursor.forming(), cursor.moment()),
            (4, false, Some(Time(40)))
        );

        // At the end of the data the play stops.
        assert!(cursor.play(100));
        assert_eq!(cursor.shown(), 6);
        assert!(!cursor.play(10));
    }

    #[test]
    fn playing_skips_the_time_no_bar_is_open() {
        // Bars at 0, 10, then a hole until 100.
        let mut candles = data(2);
        candles
            .push(Candle {
                time: Time(100),
                ..candles.view().get(0).unwrap()
            })
            .unwrap();
        let trades = tape(&[101]);
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        cursor.start_at(2);
        assert!(cursor.play(5));
        assert_eq!(cursor.moment(), Some(Time(100)), "on from the next bar's open");
        assert!(cursor.play(5));
        assert_eq!((cursor.shown(), cursor.forming()), (3, true));
    }

    #[test]
    fn start_is_clamped_to_the_data() {
        let candles = data(5);
        let trades = Trades::new();
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        cursor.start_at(0);
        assert_eq!(cursor.shown(), 1);
        cursor.start_at(99);
        assert_eq!(cursor.shown(), 5);
    }

    #[test]
    fn no_data_is_fine() {
        let candles = Candles::new();
        let trades = Trades::new();
        let (mut at, mut moment) = (None, None);
        let mut cursor = Cursor::new(&candles, &trades, &mut at, &mut moment, None);
        cursor.start_at(3);
        assert_eq!(cursor.shown(), 0);
        assert_eq!(cursor.advance(1), 0);
        assert!(!cursor.play(10));
    }
}
