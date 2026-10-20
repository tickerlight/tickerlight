//! Order flow: the trades behind the footprint and the volume profile. A day of trades of a busy pair is a few
//! megabytes, so only what is on screen is loaded, a piece at a time: a day from the archive (or Coinbase's API)
//! and kept in the cache like the candles, or, for the days the archive does not have yet, an hour from the API.

use std::error::Error;

use tickerlight_binance::{Archive, Binance, Period};
use tickerlight_bitfinex::Bitfinex;
use tickerlight_bitget::Bitget;
use tickerlight_bitstamp::Bitstamp;
use tickerlight_coinbase::Coinbase;
use tickerlight_core::calendar::{MS_PER_DAY, civil_from_days};
use tickerlight_core::{Chart, Time, Trade, Trades, native_step};
use tickerlight_deribit::Deribit;
use tickerlight_kraken::Kraken;
use tickerlight_kucoin::Kucoin;
use tickerlight_okx::Okx;

use crate::program::Source;
use crate::store::{Series, Store, Venue};

/// Days of trades held at most: those at the right of the screen.
pub const MAX_DAYS: i64 = 3;
pub const MS_PER_HOUR: i64 = 3_600_000;
/// Live trades held at most (a busy pair trades a few million a day). Past it the older hours are let go: they
/// load from the API again like any other hour, if they are on screen.
pub const LIVE_HELD: usize = 2_000_000;

/// A piece of the tape loaded at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk {
    /// A day of the archive (of its clock: see [`Venue::day_start`](crate::store::Venue::day_start)).
    Day(Period),
    /// An hour from the API, starting then: today, and the days the archive has not published yet.
    Hour(Time),
}

impl Chunk {
    /// When it starts, for a venue whose days start `day_start` after midnight UTC.
    pub fn start(self, day_start: i64) -> Time {
        match self {
            Chunk::Day(day) => Time(day.start().0 + day_start),
            Chunk::Hour(start) => start,
        }
    }
}

/// Lets the older live trades go once there are more than `held`: those before the start of the hour of the
/// middle one, or of the last one if that does not let any go. `from`, where the live trades start, moves there.
/// Returns whether any went.
pub fn trim_live(trades: &mut Vec<Trade>, from: &mut Time, held: usize) -> bool {
    if trades.len() <= held {
        return false;
    }
    let hour = |t: Time| Time(t.0 - t.0.rem_euclid(MS_PER_HOUR));
    let middle = hour(trades[trades.len() / 2].time);
    let cut = if middle > *from {
        middle
    } else {
        hour(trades[trades.len() - 1].time)
    };
    if cut <= *from {
        return false;
    }
    trades.retain(|t| t.time >= cut);
    *from = cut;
    true
}

/// The pieces of the tape the chart needs: the days on screen, at most [`MAX_DAYS`] ending at the right edge. A
/// day before today comes whole from the archive, unless it is `unarchived`; those and today come an hour at a
/// time, only the hours on screen, and none from `live` on (those trades come live). The days are the archive's,
/// starting `day_start` after midnight UTC. Bars of seconds are only where the tape is: with none yet, the piece
/// of the replay's moment is wanted, else the one before the live trades (or of the newest bar, which may be days
/// old while the live candles catch up), and the one before the oldest once it is on screen.
pub fn wanted(chart: &Chart, now: Time, unarchived: &[Period], live: Option<Time>, day_start: i64) -> Vec<Chunk> {
    let seen = chart.seen();
    let times = seen.time();
    let visible = chart.time_scale().visible(times.len());
    let seconds = (chart.timeframe()).is_some_and(|span| native_step(chart.base()).is_some_and(|n| span < n));
    let (from, to) = match (seconds, visible.is_empty()) {
        (true, true) => match chart
            .moment()
            .or(live.map(|t| Time(t.0 - 1)))
            .or(chart.base().time().last().copied())
        {
            Some(t) => (t.0, t.0),
            None => return Vec::new(),
        },
        (false, true) => return Vec::new(),
        (_, false) => {
            let earlier = if seconds && visible.start == 0 { MS_PER_HOUR } else { 0 };
            (times[visible.start].0 - earlier, times[visible.end - 1].0)
        }
    };
    // On the archive's clock, where its days start at midnight.
    let (from, to) = (from - day_start, to - day_start);
    let today = (now.0 - day_start).div_euclid(MS_PER_DAY);
    let last = to.div_euclid(MS_PER_DAY).min(today);
    let first = from.div_euclid(MS_PER_DAY).max(last - (MAX_DAYS - 1));
    let end = live.map_or(i64::MAX, |t| t.0 - day_start);
    let mut out = Vec::new();
    for d in first..=last {
        let (year, month, day) = civil_from_days(d);
        let period = Period::Day { year, month, day };
        if d < today && !unarchived.contains(&period) {
            out.push(Chunk::Day(period));
            continue;
        }
        // Days start on the hour, so the hours of the archive's clock are those of UTC.
        let mut hour = (d * MS_PER_DAY).max(from - from.rem_euclid(MS_PER_HOUR));
        while hour <= to && hour < (d + 1) * MS_PER_DAY && hour < end {
            out.push(Chunk::Hour(Time(hour + day_start)));
            hour += MS_PER_HOUR;
        }
    }
    out
}

/// The piece of the tape a replay standing at `t` needs: its day from the archive or, when the archive does not
/// have the day (see [`wanted`]), its hour; none when that hour comes live.
pub fn at(t: Time, now: Time, unarchived: &[Period], live: Option<Time>, day_start: i64) -> Option<Chunk> {
    let d = (t.0 - day_start).div_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(d);
    let period = Period::Day { year, month, day };
    if d < (now.0 - day_start).div_euclid(MS_PER_DAY) && !unarchived.contains(&period) {
        return Some(Chunk::Day(period));
    }
    let hour = Time(t.0 - t.0.rem_euclid(MS_PER_HOUR));
    live.is_none_or(|live| hour < live).then_some(Chunk::Hour(hour))
}

/// Every trade of a symbol from a moment on, live from its exchange.
pub enum LiveTape {
    Binance(tickerlight_binance::LiveTrades),
    Okx(tickerlight_okx::LiveTrades),
    Coinbase(tickerlight_coinbase::LiveTrades),
    Kucoin(tickerlight_kucoin::LiveTrades),
    Bitget(tickerlight_bitget::LiveTrades),
    Kraken(tickerlight_kraken::LiveTrades),
    Bitstamp(tickerlight_bitstamp::LiveTrades),
    Bitfinex(tickerlight_bitfinex::LiveTrades),
    Deribit(tickerlight_deribit::LiveTrades),
}

impl LiveTape {
    /// `None` for an imported symbol or Dukascopy's: no trades come live.
    pub fn connect(series: &Series, from: Time) -> Option<Self> {
        match series.venue {
            Venue::Binance(market) => Some(Self::Binance(tickerlight_binance::LiveTrades::connect(
                market,
                &series.symbol,
                from,
            ))),
            Venue::Okx(market) => Some(Self::Okx(tickerlight_okx::LiveTrades::connect(
                market,
                &series.symbol,
                from,
            ))),
            Venue::Coinbase => Some(Self::Coinbase(tickerlight_coinbase::LiveTrades::connect(
                &series.symbol,
                from,
            ))),
            Venue::Kucoin(market) => Some(Self::Kucoin(tickerlight_kucoin::LiveTrades::connect(
                market,
                &series.symbol,
                from,
            ))),
            Venue::Bitget(market) => Some(Self::Bitget(tickerlight_bitget::LiveTrades::connect(
                market,
                &series.symbol,
                from,
            ))),
            Venue::Kraken => Some(Self::Kraken(tickerlight_kraken::LiveTrades::connect(
                &series.symbol,
                from,
            ))),
            Venue::Bitstamp => Some(Self::Bitstamp(tickerlight_bitstamp::LiveTrades::connect(
                &series.symbol,
                from,
            ))),
            Venue::Bitfinex => Some(Self::Bitfinex(tickerlight_bitfinex::LiveTrades::connect(
                &series.symbol,
                from,
            ))),
            Venue::Deribit => Some(Self::Deribit(tickerlight_deribit::LiveTrades::connect(
                &series.symbol,
                from,
            ))),
            Venue::Dukascopy | Venue::Imported => None,
        }
    }

    /// The trades that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Trade> {
        match self {
            Self::Binance(live) => live.take(),
            Self::Okx(live) => live.take(),
            Self::Coinbase(live) => live.take(),
            Self::Kucoin(live) => live.take(),
            Self::Bitget(live) => live.take(),
            Self::Kraken(live) => live.take(),
            Self::Bitstamp(live) => live.take(),
            Self::Bitfinex(live) => live.take(),
            Self::Deribit(live) => live.take(),
        }
    }
}

/// The trades of a piece of the tape, none after `end` (where the candles end). `None` when the archive does not
/// have the day yet: its hours are then asked of the API. An imported symbol has only what was imported;
/// Dukascopy has quotes, not trades; KuCoin's API has no trades but the last hundred; Bitstamp's none older than a
/// day (and no archive).
pub fn load(source: &Source, series: &Series, chunk: Chunk, end: Time) -> Result<Option<Vec<Trade>>, Box<dyn Error>> {
    match (series.venue, chunk) {
        (Venue::Imported, _) => return Ok(Some(imported(source, series, chunk)?)),
        (Venue::Dukascopy, _) => return Err("Dukascopy has quotes, not trades".into()),
        (Venue::Kucoin(_), Chunk::Hour(_)) => {
            return Err("KuCoin gives the trades of the last hours only live".into());
        }
        (Venue::Bitstamp, Chunk::Day(day))
            if day.end().0 + tickerlight_bitstamp::TRADES_KEPT <= crate::program::now().0 =>
        {
            return Err(KEPT_BY_BITSTAMP.into());
        }
        (Venue::Bitstamp, Chunk::Hour(start))
            if start.0 + MS_PER_HOUR + tickerlight_bitstamp::TRADES_KEPT <= crate::program::now().0 =>
        {
            return Err(KEPT_BY_BITSTAMP.into());
        }
        _ => {}
    }
    match chunk {
        Chunk::Day(day) => load_day(source, series, day),
        Chunk::Hour(start) => {
            if source.offline {
                return Err("the trades of today need the network".into());
            }
            let to = Time((start.0 + MS_PER_HOUR).min(end.0));
            let trades = match series.venue {
                Venue::Binance(market) => {
                    let trades = Binance::for_market(market).agg_trades(&series.symbol, start, to)?;
                    let view = trades.view();
                    (0..view.len()).filter_map(|i| view.get(i)).collect()
                }
                Venue::Okx(market) => Okx::new(market).trades(&series.symbol, start, to)?,
                Venue::Coinbase => Coinbase::new().trades(&series.symbol, start, to)?,
                Venue::Bitget(market) => Bitget::new(market).trades(&series.symbol, start, to)?,
                Venue::Kraken => Kraken::new().trades(&series.symbol, start, to)?,
                Venue::Bitstamp => Bitstamp::new().trades(&series.symbol, start, to)?,
                Venue::Bitfinex => Bitfinex::new().trades(&series.symbol, start, to)?,
                Venue::Deribit => Deribit::new().trades(&series.symbol, start, to)?,
                Venue::Kucoin(_) | Venue::Dukascopy | Venue::Imported => Vec::new(),
            };
            Ok(Some(trades))
        }
    }
}

const KEPT_BY_BITSTAMP: &str = "Bitstamp keeps the trades of the last 24 hours only";

/// The imported trades of a piece of the tape; none if that day was not imported.
fn imported(source: &Source, series: &Series, chunk: Chunk) -> Result<Vec<Trade>, Box<dyn Error>> {
    let day = match chunk {
        Chunk::Day(day) => day,
        Chunk::Hour(start) => {
            let (year, month, day) = civil_from_days(start.0.div_euclid(MS_PER_DAY));
            Period::Day { year, month, day }
        }
    };
    let mut trades = Store::new(&source.root).read_trades(series, day)?.unwrap_or_default();
    if let Chunk::Hour(start) = chunk {
        trades.retain(|t| t.time >= start && t.time.0 < start.0 + MS_PER_HOUR);
    }
    Ok(trades)
}

/// The trades of a day: from the cache, else from the archive (and then cached). Coinbase, Kraken, Bitfinex and
/// Deribit have no archive: a day that has ended comes from their API (Kraken's and Deribit's by the hour at once).
/// Bitget's archive keeps only the last weeks: an older day comes by the hour from the API (which has 90 days).
fn load_day(source: &Source, series: &Series, day: Period) -> Result<Option<Vec<Trade>>, Box<dyn Error>> {
    let store = Store::new(&source.root);
    if let Some(trades) = store.read_trades(series, day)? {
        return Ok(Some(trades));
    }
    if source.offline {
        return Err(format!("trades of {} are not cached", day.label()).into());
    }
    let trades = match (series.venue, day) {
        (Venue::Binance(market), _) => Archive::new().agg_trades(market, &series.symbol, day)?,
        (Venue::Okx(market), Period::Day { year, month, day }) => {
            let day = tickerlight_okx::Day { year, month, day };
            Okx::new(market).archive_trades(&series.symbol, day)?
        }
        (Venue::Kucoin(market), Period::Day { year, month, day }) => {
            let day = tickerlight_kucoin::Day { year, month, day };
            Kucoin::new(market).archive_trades(&series.symbol, day)?
        }
        (Venue::Bitget(market), Period::Day { year, month, day }) => {
            let day = tickerlight_bitget::Day { year, month, day };
            Bitget::new(market).archive_trades(&series.symbol, day)?
        }
        (Venue::Coinbase, _) if day.end() <= crate::program::now() => {
            Some(Coinbase::new().trades(&series.symbol, day.start(), day.end())?)
        }
        (Venue::Kraken, _) if day.end() <= crate::program::now() => {
            Some(Kraken::new().trades(&series.symbol, day.start(), day.end())?)
        }
        (Venue::Bitfinex, _) if day.end() <= crate::program::now() => {
            Some(Bitfinex::new().trades(&series.symbol, day.start(), day.end())?)
        }
        (Venue::Deribit, _) if day.end() <= crate::program::now() => {
            Some(Deribit::new().trades(&series.symbol, day.start(), day.end())?)
        }
        _ => None,
    };
    let Some(trades) = trades else {
        return Ok(None);
    };
    store.write_trades(series, day, &trades)?;
    Ok(Some(trades))
}

/// A price step for the footprint's levels: about a ten-thousandth of the price, rounded to 1, 2 or 5 times a
/// power of ten (10 for 80000, 0.01 for 100).
pub fn tick_size(price: f64) -> f64 {
    if !(price.is_finite() && price > 0.0) {
        return 1.0;
    }
    let raw = price * 1e-4;
    let decade = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|&m| m * decade >= raw)
        .unwrap_or(10.0);
    // Trim the noise of the multiplication (0.1 * 5 is not exactly 0.5).
    format!("{:.10e}", step * decade).parse().unwrap_or(step * decade)
}

/// One tape of the pieces held, in order.
pub fn tape(chunks: &[(Chunk, Vec<Trade>)]) -> Trades {
    let mut out = Trades::with_capacity(chunks.iter().map(|(_, d)| d.len()).sum());
    for (_, day) in chunks {
        for &trade in day {
            // A trade out of order (none in the archive's files) is left out rather than failing the day.
            let _ = out.push(trade);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::calendar::time_of_date;
    use tickerlight_core::{Candle, Candles};

    #[test]
    fn older_live_trades_go_by_the_hour() {
        let trade = |minute: i64| Trade {
            time: Time(minute * 60_000),
            price: 1.0,
            qty: 1.0,
            side: tickerlight_core::Side::Buy,
        };
        // Two hours and a bit, one trade a minute.
        let mut trades: Vec<Trade> = (0..150).map(trade).collect();
        let mut from = Time(0);
        assert!(!trim_live(&mut trades, &mut from, 150));
        assert!(trim_live(&mut trades, &mut from, 100));
        // The middle trade is in the second hour: the first goes.
        assert_eq!(
            (from, trades.len(), trades[0].time),
            (Time(MS_PER_HOUR), 90, Time(MS_PER_HOUR))
        );
        // The middle is now in the hour `from` is at: the hour of the last one is kept.
        assert!(trim_live(&mut trades, &mut from, 10));
        assert_eq!((from, trades.len()), (Time(2 * MS_PER_HOUR), 30));
        // All in one hour: nothing can go.
        assert!(!trim_live(&mut trades, &mut from, 10));
        assert_eq!(trades.len(), 30);
    }

    #[test]
    fn ticks_follow_the_price() {
        assert_eq!(tick_size(81_234.0), 10.0);
        assert_eq!(tick_size(104.3), 0.02);
        assert_eq!(tick_size(2_500.0), 0.5);
        assert_eq!(tick_size(0.0), 1.0);
    }

    #[test]
    fn the_days_on_screen_are_wanted_whole_or_by_the_hour() {
        let start = time_of_date(2026, 10, 10);
        let mut candles = Candles::new();
        // Six days of hourly bars.
        for i in 0..144 {
            let p = 100.0;
            candles
                .push(Candle {
                    time: Time(start.0 + i * 3_600_000),
                    open: p,
                    high: p,
                    low: p,
                    close: p,
                    volume: 1.0,
                })
                .unwrap();
        }
        let mut chart = Chart::new(candles);
        chart.resize(800.0, 400.0);
        let day = |d| Period::Day {
            year: 2026,
            month: 10,
            day: d,
        };
        let hours = |d: u32| {
            (0..24)
                .map(|h| Chunk::Hour(Time(day(d).start().0 + h * MS_PER_HOUR)))
                .collect::<Vec<_>>()
        };
        // Zoomed out, all six days show: the last three are wanted.
        let now = time_of_date(2026, 10, 20);
        while chart.time_scale().visible(144).start > 0 {
            chart.handle(tickerlight_core::Event::Wheel { x: 400.0, delta: -1.0 });
        }
        let days = |ds: &[u32]| ds.iter().map(|&d| Chunk::Day(day(d))).collect::<Vec<_>>();
        assert_eq!(wanted(&chart, now, &[], None, 0), days(&[13, 14, 15]));
        // An archive of UTC+8 has the last bars, from 16:00 UTC on, in the next day of its own.
        let okx = tickerlight_okx::DAY_START;
        assert_eq!(wanted(&chart, now, &[], None, okx), days(&[14, 15, 16]));
        assert_eq!(
            Chunk::Day(day(16)).start(okx),
            Time(day(15).start().0 + 16 * MS_PER_HOUR)
        );
        // Today is not in the archive yet, nor is a day it has not published: they come by the hour.
        let now = Time(time_of_date(2026, 10, 15).0 + 1000);
        assert_eq!(wanted(&chart, now, &[], None, 0), [days(&[13, 14]), hours(15)].concat());
        assert_eq!(
            wanted(&chart, now, &[day(14)], None, 0),
            [days(&[13]), hours(14), hours(15)].concat()
        );
        // There it is the 15th from 16:00 UTC of the 14th: its hours come from the API, on the hour of UTC.
        let okx_hours: Vec<Chunk> = (16..40)
            .map(|h| Chunk::Hour(Time(day(14).start().0 + h * MS_PER_HOUR)))
            .collect();
        assert_eq!(
            wanted(&chart, now, &[], None, okx),
            [days(&[13, 14]), okx_hours].concat()
        );
        // From the hour the trades come live, no more hours are asked for.
        let live = Time(day(15).start().0 + 10 * MS_PER_HOUR);
        assert_eq!(
            wanted(&chart, now, &[], Some(live), 0),
            [days(&[13, 14]), hours(15)[..10].to_vec()].concat()
        );
    }

    #[test]
    fn bars_of_seconds_want_the_newest_piece_then_the_one_before_the_oldest() {
        let start = time_of_date(2026, 10, 15);
        let mut candles = Candles::new();
        // A day of minute bars.
        for i in 0..1440 {
            let bar = Candle {
                time: Time(start.0 + i * 60_000),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            };
            candles.push(bar).unwrap();
        }
        let mut chart = Chart::new(candles);
        chart.resize(800.0, 400.0);
        chart.set_timeframe(Some(15_000));
        let day = |d| {
            Chunk::Day(Period::Day {
                year: 2026,
                month: 10,
                day: d,
            })
        };
        let now = time_of_date(2026, 10, 20);
        // No tape yet: the day of the newest bar.
        assert_eq!(wanted(&chart, now, &[], None, 0), [day(15)]);
        // Live from an hour on, while the candles still end days before: the hour before the live trades.
        let live = Time(now.0 + 5 * MS_PER_HOUR);
        assert_eq!(
            wanted(&chart, live, &[], Some(live), 0),
            [Chunk::Hour(Time(live.0 - MS_PER_HOUR))]
        );
        // Ten minutes of it from 00:10, all on screen: the hour before the oldest bar is in the 14th.
        let mut tape = Trades::new();
        for k in 0..40 {
            let trade = Trade {
                time: Time(start.0 + 10 * 60_000 + k * 15_000),
                price: 1.0,
                qty: 1.0,
                side: tickerlight_core::Side::Buy,
            };
            tape.push(trade).unwrap();
        }
        chart.set_trades(tape);
        assert_eq!(chart.seen().len(), 40);
        assert_eq!(wanted(&chart, now, &[], None, 0), [day(14), day(15)]);
        // On the 15th itself its hours come from the API: only the one on screen.
        let now = Time(start.0 + 2 * MS_PER_HOUR);
        assert_eq!(wanted(&chart, now, &[], None, 0), [day(14), Chunk::Hour(start)]);
    }

    #[test]
    fn a_replay_wants_the_piece_of_the_tape_it_stands_in() {
        let day = |d| Period::Day {
            year: 2026,
            month: 10,
            day: d,
        };
        let now = time_of_date(2026, 10, 15);
        let t = Time(day(13).start().0 + 17 * MS_PER_HOUR + 5);
        assert_eq!(at(t, now, &[], None, 0), Some(Chunk::Day(day(13))));
        // 17:00 UTC of the 13th is in the 14th of an archive of UTC+8.
        assert_eq!(
            at(t, now, &[], None, tickerlight_okx::DAY_START),
            Some(Chunk::Day(day(14)))
        );
        // A day the archive does not have comes by the hour, until the hours come live.
        let hour = Chunk::Hour(Time(day(13).start().0 + 17 * MS_PER_HOUR));
        assert_eq!(at(t, now, &[day(13)], None, 0), Some(hour));
        assert_eq!(at(t, now, &[day(13)], Some(Time(t.0 - 5)), 0), None);
    }
}
