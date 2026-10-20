//! Bringing the cache up to date: which archive files a span of history needs, fetching the missing ones, and
//! reading the span back as one series.

use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use tickerlight_binance::{Archive, Market, Period};
use tickerlight_bitget::Bitget;
use tickerlight_coinbase::Coinbase;
use tickerlight_core::calendar::{DateTime, MS_PER_DAY};
use tickerlight_core::{Candle, Candles, Time};
use tickerlight_dukascopy::{Dukascopy, Instrument};
use tickerlight_kucoin::Kucoin;
use tickerlight_okx::Okx;

use crate::store::{Series, Store};

/// How long after a day ends its file may still be missing from the listing because it is not out yet. After
/// that a missing day means the symbol did not trade then.
const PUBLISH_DELAY: i64 = 7 * MS_PER_DAY;

/// The month `months` months before the one holding `now`.
pub fn months_before(now: Time, months: u32) -> (i64, u32) {
    let date = DateTime::from_time(now);
    let index = date.year * 12 + i64::from(date.month - 1) - i64::from(months);
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// The files covering whole months from `first` up to the month before `now`, then the days of `now`'s month
/// before its day. Today is not in the archive yet.
pub fn periods(first: (i64, u32), now: Time) -> Vec<Period> {
    let today = DateTime::from_time(now);
    let mut out = Vec::new();
    let (mut year, mut month) = first;
    while (year, month) < (today.year, today.month) {
        out.push(Period::Month { year, month });
        (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    }
    out.extend((1..today.day).map(|day| Period::Day {
        year: today.year,
        month: today.month,
        day,
    }));
    out
}

fn days_of(year: i64, month: u32) -> Vec<Period> {
    let span = Period::Month { year, month };
    let days = (span.end().0 - span.start().0) / MS_PER_DAY;
    (1..=days as u32).map(|day| Period::Day { year, month, day }).collect()
}

/// Progress of [`update`]: `done` of `total` files are checked; `fetched` is the one just downloaded, if any.
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub fetched: Option<Period>,
}

/// What [`update`] needs of the archive: the archive itself, or a stand-in in tests.
pub trait Remote {
    /// The months it has, oldest first.
    fn months(&self, series: &Series) -> Result<Vec<Period>, Box<dyn Error>>;
    /// The days of a month it has, oldest first.
    fn days(&self, series: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>>;
    /// The candles of a file, `None` if it has no such file.
    fn candles(&self, series: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>>;
}

impl Remote for Archive {
    fn months(&self, s: &Series) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(self.kline_months(binance(s)?, &s.symbol, s.interval)?)
    }

    fn days(&self, s: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(self.kline_days(binance(s)?, &s.symbol, s.interval, year, month)?)
    }

    fn candles(&self, s: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>> {
        Ok(self.klines(binance(s)?, &s.symbol, s.interval, period)?)
    }
}

/// The Binance market of a series; another venue is not in Binance's archive.
fn binance(series: &Series) -> Result<Market, Box<dyn Error>> {
    series
        .venue
        .binance()
        .ok_or_else(|| format!("{} is not on Binance", series.symbol).into())
}

/// OKX's archive of 1-minute candles. It has no listing, so whether a file is there is asked of each, several at
/// once: the months from `first` on, and the days of a month that have ended by `now`. Its periods are of UTC+8,
/// and so are `first` and `now` (see [`local_now`]).
pub struct OkxArchive {
    pub okx: Okx,
    pub first: (i64, u32),
    pub now: Time,
}

impl Remote for OkxArchive {
    fn months(&self, s: &Series) -> Result<Vec<Period>, Box<dyn Error>> {
        let today = DateTime::from_time(self.now);
        let mut months = Vec::new();
        let (mut year, mut month) = self.first;
        while (year, month) < (today.year, today.month) {
            months.push(Period::Month { year, month });
            (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
        }
        probe(&months, |p| match p {
            Period::Month { year, month } => self.okx.has_candles(&s.symbol, year, month, None),
            Period::Day { .. } => Ok(false),
        })
    }

    fn days(&self, s: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>> {
        let ended: Vec<Period> = days_of(year, month)
            .into_iter()
            .filter(|d| d.end() <= self.now)
            .collect();
        probe(&ended, |p| match p {
            Period::Day { year, month, day } => self.okx.has_candles(&s.symbol, year, month, Some(day)),
            Period::Month { .. } => Ok(false),
        })
    }

    fn candles(&self, s: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>> {
        Ok(match period {
            Period::Month { year, month } => self.okx.archive_candles(&s.symbol, year, month, None)?,
            Period::Day { year, month, day } => self.okx.archive_candles(&s.symbol, year, month, Some(day))?,
        })
    }
}

/// The whole months from the one of `start` to the one before `now`'s.
fn months_from(start: Time, now: Time) -> Vec<Period> {
    let (start, today) = (DateTime::from_time(start), DateTime::from_time(now));
    let mut months = Vec::new();
    let (mut year, mut month) = (start.year, start.month);
    while (year, month) < (today.year, today.month) {
        months.push(Period::Month { year, month });
        (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    }
    months
}

/// The days of a month that end after `start` and by `now`.
fn days_between(year: i64, month: u32, start: Time, now: Time) -> Vec<Period> {
    days_of(year, month)
        .into_iter()
        .filter(|d| d.end() <= now && d.end() > start)
        .collect()
}

/// An exchange's API that has the 1-minute candles of any span of its history.
pub trait History {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>>;
}

impl History for Coinbase {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(Coinbase::candles(self, symbol, from, to)?)
    }
}

impl History for Kucoin {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(Kucoin::candles(self, symbol, from, to)?)
    }
}

impl History for Bitget {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(Bitget::candles(self, symbol, from, to)?)
    }
}

impl History for tickerlight_kraken::Kraken {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(tickerlight_kraken::Kraken::candles(self, symbol, from, to)?)
    }
}

impl History for tickerlight_bitstamp::Bitstamp {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(tickerlight_bitstamp::Bitstamp::candles(self, symbol, from, to)?)
    }
}

impl History for tickerlight_bitfinex::Bitfinex {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(tickerlight_bitfinex::Bitfinex::candles(self, symbol, from, to)?)
    }
}

impl History for tickerlight_deribit::Deribit {
    fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Box<dyn Error>> {
        Ok(tickerlight_deribit::Deribit::candles(self, symbol, from, to)?)
    }
}

/// An exchange's API as an archive: every month and every ended day from the symbol's first trading day on
/// (`None`: none in the months asked about), each asked for as a span of candles.
pub struct ApiArchive<H> {
    pub api: H,
    pub listed: Option<Time>,
    pub now: Time,
}

impl<H: History> Remote for ApiArchive<H> {
    fn months(&self, _: &Series) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(self
            .listed
            .map_or_else(Vec::new, |listed| months_from(listed, self.now)))
    }

    fn days(&self, _: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(self
            .listed
            .map_or_else(Vec::new, |listed| days_between(year, month, listed, self.now)))
    }

    fn candles(&self, s: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>> {
        Ok(Some(self.api.candles(&s.symbol, period.start(), period.end())?))
    }
}

/// Dukascopy's datafeed: a file per day, so a month is its days, asked for one after the other. Every month and
/// every day that has ended is there from the instrument's first minute on (a closed market's day is empty).
pub struct DukascopyArchive {
    pub api: Dukascopy,
    pub instrument: Instrument,
    pub now: Time,
}

impl Remote for DukascopyArchive {
    fn months(&self, _: &Series) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(months_from(self.instrument.start, self.now))
    }

    fn days(&self, _: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>> {
        Ok(days_between(year, month, self.instrument.start, self.now))
    }

    fn candles(&self, _: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>> {
        let day = |year, month, day| self.api.day(&self.instrument, year, month, day);
        Ok(match period {
            Period::Day { year, month, day: d } => day(year, month, d)?,
            Period::Month { year, month } => {
                let mut all = Vec::new();
                for d in days_of(year, month) {
                    if let Period::Day { day: d, .. } = d {
                        // A day without a file had no trading.
                        all.extend(day(year, month, d)?.unwrap_or_default());
                    }
                }
                Some(all)
            }
        })
    }
}

/// The time on the clock of a venue's candle archive: its periods are named by it.
pub fn local_now(series: &Series, now: Time) -> Time {
    Time(now.0 - series.venue.period_start())
}

/// Those of `periods` that `has` says are there, in order, asked about [`PARALLEL`] at a time.
fn probe(
    periods: &[Period],
    has: impl Fn(Period) -> Result<bool, tickerlight_okx::Error> + Sync,
) -> Result<Vec<Period>, Box<dyn Error>> {
    let mut out = Vec::new();
    for batch in periods.chunks(PARALLEL) {
        let has = &has;
        let found: Vec<Result<bool, tickerlight_okx::Error>> = std::thread::scope(|scope| {
            let asked: Vec<_> = batch.iter().map(|&p| scope.spawn(move || has(p))).collect();
            asked
                .into_iter()
                .map(|h| h.join().expect("a probe does not panic"))
                .collect()
        });
        for (&period, found) in batch.iter().zip(found) {
            if found? {
                out.push(period);
            }
        }
    }
    Ok(out)
}

/// The days of each month the archive has, asked for once per month.
struct Days<'a, R: Remote> {
    remote: &'a R,
    series: &'a Series,
    months: Vec<((i64, u32), Vec<Period>)>,
}

impl<R: Remote> Days<'_, R> {
    fn of(&mut self, year: i64, month: u32) -> Result<&[Period], Box<dyn Error>> {
        let i = match self.months.iter().position(|(m, _)| *m == (year, month)) {
            Some(i) => i,
            None => {
                let days = self.remote.days(self.series, year, month)?;
                self.months.push(((year, month), days));
                self.months.len() - 1
            }
        };
        Ok(&self.months[i].1)
    }
}

/// How many files are downloaded at once: the archive is slow to answer each request rather than to send it.
const PARALLEL: usize = 6;

/// Downloads the files of `periods` that are not cached yet, going by the archive's listing, several at once. A
/// month that is not out yet comes day by day. A period the archive does not have and never will (before the
/// listing, or long past) is cached as empty, so it is not asked for again. A symbol the archive does not know is
/// an error, and nothing is cached for it. Stopping halfway loses at most the files being downloaded.
pub fn update(
    store: &Store,
    remote: &(impl Remote + Sync),
    series: &Series,
    periods: &[Period],
    now: Time,
    mut progress: impl FnMut(Progress),
) -> Result<(), Box<dyn Error>> {
    let months = remote.months(series)?;
    let mut days = Days {
        remote,
        series,
        months: Vec::new(),
    };
    if months.is_empty() {
        // A symbol listed lately has days only: this month's, or the last one's (its month is not out yet).
        let today = DateTime::from_time(now);
        let (year, month) = months_before(now, 1);
        if days.of(today.year, today.month)?.is_empty() && days.of(year, month)?.is_empty() {
            let venue = series.venue.label();
            return Err(format!("There is no {} in the {venue} archive", series.symbol).into());
        }
    }
    // The files to download, by the index of the period they are for; the rest is settled here.
    let last_month = months.last().copied();
    let mut jobs: Vec<(usize, Period)> = Vec::new();
    for (i, &period) in periods.iter().enumerate() {
        match period {
            _ if store.read(series, period)?.is_some() => {}
            Period::Month { .. } if months.binary_search(&period).is_ok() => jobs.push((i, period)),
            // Before the last month out, a month that is missing will never be there.
            Period::Month { .. } if last_month.is_some_and(|last| period < last) => store.write(series, period, &[])?,
            Period::Month { year, month } => {
                for day in days_of(year, month) {
                    if store.read(series, day)?.is_none() && listed_day(store, &mut days, series, day, now)? {
                        jobs.push((i, day));
                    }
                }
            }
            Period::Day { .. } => {
                if listed_day(store, &mut days, series, period, now)? {
                    jobs.push((i, period));
                }
            }
        }
    }
    // The newest files first: they are the ones looked at.
    jobs.reverse();
    let total = periods.len();
    let mut left = vec![0usize; total];
    for &(i, _) in &jobs {
        left[i] += 1;
    }
    let mut done = left.iter().filter(|&&n| n == 0).count();
    if done > 0 {
        progress(Progress {
            done,
            total,
            fetched: None,
        });
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let (send, receive) = mpsc::channel::<(usize, Period, Result<bool, String>)>();
    let mut error = None;
    std::thread::scope(|scope| {
        for _ in 0..PARALLEL.min(jobs.len()) {
            let (send, next, failed, jobs) = (send.clone(), &next, &failed, &jobs);
            scope.spawn(move || {
                while !failed.load(Ordering::Relaxed)
                    && let Some(&(i, period)) = jobs.get(next.fetch_add(1, Ordering::Relaxed))
                {
                    let result = fetch(store, remote, series, period).map_err(|e| e.to_string());
                    failed.fetch_or(result.is_err(), Ordering::Relaxed);
                    if send.send((i, period, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(send);
        for (i, period, result) in receive {
            match result {
                Ok(fetched) => {
                    left[i] -= 1;
                    if left[i] == 0 {
                        done += 1;
                        progress(Progress {
                            done,
                            total,
                            fetched: fetched.then_some(period),
                        });
                    }
                }
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
    });
    match error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// Caches one listed file. Returns whether it was there to download.
fn fetch(store: &Store, remote: &impl Remote, series: &Series, period: Period) -> Result<bool, Box<dyn Error>> {
    match remote.candles(series, period)? {
        Some(candles) => {
            store.write(series, period, &candles)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Whether the archive has a day to download; a day not there long after it ended is cached as empty.
fn listed_day<R: Remote>(
    store: &Store,
    days: &mut Days<'_, R>,
    series: &Series,
    day: Period,
    now: Time,
) -> Result<bool, Box<dyn Error>> {
    let Period::Day { year, month, .. } = day else {
        return Ok(false);
    };
    if days.of(year, month)?.binary_search(&day).is_ok() {
        return Ok(true);
    }
    if now.0 - day.end().0 > PUBLISH_DELAY {
        store.write(series, day, &[])?;
    }
    Ok(false)
}

/// The cached candles of `periods`, in order. A month that is not cached is read from its days.
pub fn load(store: &Store, series: &Series, periods: &[Period]) -> Result<Candles, Box<dyn Error>> {
    let mut out = Candles::new();
    let mut add = |candles: Vec<Candle>| {
        for candle in candles {
            // Files of neighbouring periods may share an edge candle; keep the first.
            if out.view().time().last().is_none_or(|&t| candle.time > t) {
                out.push(candle).expect("times increase");
            }
        }
    };
    for &period in periods {
        match (store.read(series, period)?, period) {
            (Some(candles), _) => add(candles),
            (None, Period::Month { year, month }) => {
                for day in days_of(year, month) {
                    if let Some(candles) = store.read(series, day)? {
                        add(candles);
                    }
                }
            }
            (None, Period::Day { .. }) => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::calendar;

    fn at(year: i64, month: u32, day: u32, hour: i64) -> Time {
        Time(calendar::time_of_date(year, month, day).0 + hour * 3_600_000)
    }

    #[test]
    fn months_count_back_across_years() {
        assert_eq!(months_before(at(2026, 10, 17, 12), 0), (2026, 10));
        assert_eq!(months_before(at(2026, 10, 17, 12), 9), (2026, 1));
        assert_eq!(months_before(at(2026, 10, 17, 12), 10), (2025, 12));
        assert_eq!(months_before(at(2026, 1, 1, 0), 25), (2023, 12));
    }

    #[test]
    fn periods_are_whole_months_then_the_days_before_today() {
        let got = periods((2026, 8), at(2026, 10, 4, 15));
        assert_eq!(
            got,
            vec![
                Period::Month { year: 2026, month: 8 },
                Period::Month { year: 2026, month: 9 },
                Period::Day {
                    year: 2026,
                    month: 10,
                    day: 1
                },
                Period::Day {
                    year: 2026,
                    month: 10,
                    day: 2
                },
                Period::Day {
                    year: 2026,
                    month: 10,
                    day: 3
                },
            ]
        );
        // They tile the span without gaps.
        for pair in got.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start());
        }
        // On the first of a month there are no days yet; across a new year the months wrap.
        assert_eq!(
            periods((2025, 12), at(2026, 1, 1, 9)),
            vec![Period::Month { year: 2025, month: 12 }]
        );
    }

    #[test]
    fn months_have_their_days() {
        assert_eq!(days_of(2024, 2).len(), 29);
        assert_eq!(days_of(2026, 2).len(), 28);
        assert_eq!(days_of(2026, 12).len(), 31);
    }

    #[test]
    fn dukascopy_lists_months_and_ended_days_from_the_instrument_s_first_minute() {
        let archive = DukascopyArchive {
            api: Dukascopy::new(),
            instrument: Instrument {
                symbol: "XUSD".into(),
                description: String::new(),
                pip: 0.01,
                start: at(2025, 11, 20, 9),
            },
            now: at(2026, 2, 3, 15),
        };
        let months = archive.months(&series()).unwrap();
        let month = |year, month| Period::Month { year, month };
        assert_eq!(months, [month(2025, 11), month(2025, 12), month(2026, 1)]);
        // The day it starts on counts: it has minutes after 9:00; today has not ended.
        assert_eq!(archive.days(&series(), 2025, 11,).unwrap()[0], day(2025, 11, 20));
        assert_eq!(archive.days(&series(), 2025, 11).unwrap().len(), 11);
        assert_eq!(
            archive.days(&series(), 2026, 2).unwrap(),
            [day(2026, 2, 1), day(2026, 2, 2)]
        );
    }

    /// An archive in memory: the listed periods have one candle each, at their start.
    struct Fake {
        months: Vec<Period>,
        days: Vec<Period>,
        downloads: AtomicUsize,
        /// A file that fails to download.
        broken: Option<Period>,
    }

    impl Remote for Fake {
        fn months(&self, _: &Series) -> Result<Vec<Period>, Box<dyn Error>> {
            Ok(self.months.clone())
        }

        fn days(&self, _: &Series, year: i64, month: u32) -> Result<Vec<Period>, Box<dyn Error>> {
            Ok(self
                .days
                .iter()
                .copied()
                .filter(|d| matches!(*d, Period::Day { year: y, month: m, .. } if (y, m) == (year, month)))
                .collect())
        }

        fn candles(&self, _: &Series, period: Period) -> Result<Option<Vec<Candle>>, Box<dyn Error>> {
            if self.broken == Some(period) {
                return Err("connection reset".into());
            }
            let listed = self.months.contains(&period) || self.days.contains(&period);
            self.downloads.fetch_add(usize::from(listed), Ordering::Relaxed);
            Ok(listed.then(|| {
                vec![Candle {
                    time: period.start(),
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                }]
            }))
        }
    }

    fn day(year: i64, month: u32, day: u32) -> Period {
        Period::Day { year, month, day }
    }

    fn series() -> Series {
        Series {
            venue: crate::store::Venue::Binance(Market::Spot),
            symbol: "XUSDT".into(),
            interval: tickerlight_binance::Interval::M1,
        }
    }

    fn temp_store(name: &str) -> (Store, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("chart-sync-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (Store::new(&root), root)
    }

    #[test]
    fn the_listing_decides_what_is_downloaded_and_what_is_never_there() {
        let (store, root) = temp_store("listing");
        let now = at(2026, 10, 4, 15);
        // Listed since August; September is not out as a month yet, October 3 not out as a day.
        let mut days = days_of(2026, 9);
        days.extend([day(2026, 10, 1), day(2026, 10, 2)]);
        let remote = Fake {
            months: vec![Period::Month { year: 2026, month: 8 }],
            days,
            downloads: Default::default(),
            broken: None,
        };
        let wanted = periods((2026, 7), now);
        let mut done = Vec::new();
        update(&store, &remote, &series(), &wanted, now, |p| {
            done.push((p.done, p.total))
        })
        .unwrap();
        assert_eq!(remote.downloads.load(Ordering::Relaxed), 1 + 30 + 2);
        // July and October 3 are settled at once; August, September (its days) and two days of October as they
        // come.
        assert_eq!(done, (2..=wanted.len()).map(|d| (d, wanted.len())).collect::<Vec<_>>());
        let july = Period::Month { year: 2026, month: 7 };
        assert_eq!(
            store.read(&series(), july).unwrap(),
            Some(vec![]),
            "before the listing: never there"
        );
        assert_eq!(
            store.read(&series(), day(2026, 10, 3)).unwrap(),
            None,
            "not out yet: asked again later"
        );
        let candles = load(&store, &series(), &wanted).unwrap();
        assert_eq!(candles.len(), 1 + 30 + 2);

        // The next run downloads nothing again.
        update(&store, &remote, &series(), &wanted, now, |_| {}).unwrap();
        assert_eq!(remote.downloads.load(Ordering::Relaxed), 33);
        // A day still missing long after is a day without trading.
        let later = at(2026, 10, 20, 0);
        update(&store, &remote, &series(), &wanted, later, |_| {}).unwrap();
        assert_eq!(store.read(&series(), day(2026, 10, 3)).unwrap(), Some(vec![]));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_download_is_an_error_and_keeps_the_files_that_came() {
        let (store, root) = temp_store("failed");
        let now = at(2026, 10, 4, 15);
        let months: Vec<Period> = (1..=9).map(|month| Period::Month { year: 2026, month }).collect();
        let remote = Fake {
            months: months.clone(),
            days: vec![day(2026, 10, 1), day(2026, 10, 2), day(2026, 10, 3)],
            downloads: Default::default(),
            broken: Some(Period::Month { year: 2026, month: 5 }),
        };
        let wanted = periods((2026, 1), now);
        let err = update(&store, &remote, &series(), &wanted, now, |_| {}).unwrap_err();
        assert_eq!(err.to_string(), "connection reset");
        assert_eq!(store.read(&series(), months[4]).unwrap(), None);
        // The next try gets the rest, and nothing twice.
        let fixed = Fake { broken: None, ..remote };
        update(&store, &fixed, &series(), &wanted, now, |_| {}).unwrap();
        assert_eq!(fixed.downloads.load(Ordering::Relaxed), 12);
        assert_eq!(load(&store, &series(), &wanted).unwrap().len(), 12);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unknown_symbol_is_an_error_and_leaves_nothing_behind() {
        let (store, root) = temp_store("unknown");
        let now = at(2026, 10, 4, 15);
        let remote = Fake {
            months: vec![],
            days: vec![],
            downloads: Default::default(),
            broken: None,
        };
        let err = update(&store, &remote, &series(), &periods((2026, 1), now), now, |_| {}).unwrap_err();
        assert_eq!(err.to_string(), "There is no XUSDT in the Binance spot archive");
        assert!(!root.exists());

        // A symbol listed last week has days only.
        let remote = Fake {
            months: vec![],
            days: vec![day(2026, 9, 29), day(2026, 9, 30), day(2026, 10, 1)],
            downloads: Default::default(),
            broken: None,
        };
        let wanted = periods((2026, 9), now);
        update(&store, &remote, &series(), &wanted, now, |_| {}).unwrap();
        assert_eq!(load(&store, &series(), &wanted).unwrap().len(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
