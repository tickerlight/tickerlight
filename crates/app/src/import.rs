//! Importing the candles or trades of any instrument from a CSV file into the cache, where they are kept like the
//! downloaded ones (under [`Venue::Imported`]).
//!
//! The delimiter is a comma, a semicolon or a tab; the header is optional. Columns go by their header names, in
//! any case: the time (`time`, `timestamp`, `date`, `datetime`, `open_time`, `time_utc`; a `date` and a `time`
//! column are joined), then either `open`, `high`, `low`, `close` and maybe `volume` (candles), or `price`, a
//! quantity and maybe the aggressor's `side` or `is_buyer_maker` (trades). Without a header the columns are
//! `time,open,high,low,close[,volume]`. Times are UTC: unix seconds, milliseconds, microseconds or nanoseconds
//! told apart by size, or dates such as `2024-03-01`, `2024-03-01 14:30`, `2024/03/01T14:30:05.250Z`.

use std::collections::HashMap;
use std::path::Path;

use tickerlight_binance::{Interval, Period};
use tickerlight_core::calendar::{DateTime, days_from_civil, time_of_date};
use tickerlight_core::{Candle, Side, Time, Trade, span_label};

use crate::store::{INTERVALS, Series, Store, Venue};

const TIME_NAMES: [&str; 4] = ["datetime", "timestamp", "open_time", "time_utc"];
const QTY_NAMES: [&str; 5] = ["qty", "quantity", "size", "amount", "volume"];

/// Imports a CSV file of candles or trades as the symbol named by the file, replacing what an earlier import of
/// it left (the candles of that interval; the trades). Trades also give 1-minute candles. Returns the series to
/// open.
pub fn import(store: &Store, path: &Path) -> Result<Series, String> {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let symbol = symbol_name(name).ok_or_else(|| format!("{} does not name a symbol", path.display()))?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let name = symbol.clone();
    let failed = |e: std::io::Error| format!("{name} not imported: {e}");
    match parse(&text).map_err(|e| format!("{}: {e}", path.display()))? {
        Rows::Candles(candles) => {
            let series = Series {
                venue: Venue::Imported,
                symbol,
                interval: interval_of(&candles).map_err(|e| format!("{}: {e}", path.display()))?,
            };
            store.remove_candles(&series).map_err(failed)?;
            write_candles(store, &series, &candles).map_err(failed)?;
            Ok(series)
        }
        Rows::Trades(trades) => {
            let series = Series {
                venue: Venue::Imported,
                symbol,
                interval: Interval::M1,
            };
            store.remove_candles(&series).map_err(failed)?;
            store.remove_trades(&series).map_err(failed)?;
            write_candles(store, &series, &minute_candles(&trades)).map_err(failed)?;
            for day in trades.chunk_by(|a, b| day_of(a.time) == day_of(b.time)) {
                store.write_trades(&series, day_of(day[0].time), day).map_err(failed)?;
            }
            Ok(series)
        }
    }
}

/// The symbol a file or a typed name stands for: upper-cased, with what is not a letter, a digit, `.`, `-` or `_`
/// as `-`. `None` when nothing is left that could name a folder.
pub fn symbol_name(text: &str) -> Option<String> {
    let symbol: String = text
        .trim()
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .map(|c| {
            if c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    (!symbol.chars().all(|c| c == '.')).then_some(symbol)
}

/// The rows of a file, in time order.
#[derive(Debug, PartialEq)]
enum Rows {
    Candles(Vec<Candle>),
    Trades(Vec<Trade>),
}

/// Where the values of a row are.
enum Columns {
    Candles {
        time: When,
        ohlc: [usize; 4],
        volume: Option<usize>,
    },
    Trades {
        time: When,
        price: usize,
        qty: usize,
        aggressor: Option<Aggressor>,
    },
}

/// The column of the time, or a date and a time column to join.
#[derive(Clone, Copy)]
enum When {
    One(usize),
    Joined(usize, usize),
}

#[derive(Clone, Copy)]
enum Aggressor {
    /// `buy`/`sell`.
    Side(usize),
    /// True when the seller took the buyer's order.
    BuyerMaker(usize),
}

fn parse(text: &str) -> Result<Rows, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty());
    let Some((first_number, first)) = lines.next() else {
        return Err("the file is empty".into());
    };
    let delimiter = ['\t', ';', ',']
        .into_iter()
        .find(|&d| first.contains(d))
        .ok_or("no columns: they are separated by a comma, a semicolon or a tab")?;
    let mut fields: Vec<&str> = Vec::new();
    split(first, delimiter, &mut fields);
    // A header has a name in it; a row has only numbers and times.
    let header = fields
        .iter()
        .any(|f| parse_time(f).is_none() && number(f, delimiter).is_none());
    let columns = if header {
        columns(&fields)?
    } else {
        Columns::Candles {
            time: When::One(0),
            ohlc: [1, 2, 3, 4],
            volume: (fields.len() > 5).then_some(5),
        }
    };
    let rows = std::iter::once((first_number, first)).filter(|_| !header).chain(lines);
    match columns {
        Columns::Candles { time, ohlc, volume } => {
            let mut candles = Vec::new();
            for (n, line) in rows {
                split(line, delimiter, &mut fields);
                let value = |i: usize, name: &str| field_number(&fields, i, delimiter, n, name);
                candles.push(Candle {
                    time: field_time(&fields, time, n)?,
                    open: value(ohlc[0], "open")?,
                    high: value(ohlc[1], "high")?,
                    low: value(ohlc[2], "low")?,
                    close: value(ohlc[3], "close")?,
                    volume: match volume {
                        Some(i) => value(i, "volume")?,
                        None => 0.0,
                    },
                });
            }
            candles.sort_by_key(|c| c.time);
            // A time given again keeps its last row.
            let mut out: Vec<Candle> = Vec::with_capacity(candles.len());
            for candle in candles {
                match out.last_mut() {
                    Some(last) if last.time == candle.time => *last = candle,
                    _ => out.push(candle),
                }
            }
            if out.is_empty() {
                return Err("no rows under the header".into());
            }
            Ok(Rows::Candles(out))
        }
        Columns::Trades {
            time,
            price,
            qty,
            aggressor,
        } => {
            let mut trades = Vec::new();
            // Without the aggressor, a trade above the last price is taken as a buy, below as a sell.
            let mut last: Option<(f64, Side)> = None;
            for (n, line) in rows {
                split(line, delimiter, &mut fields);
                let price = field_number(&fields, price, delimiter, n, "price")?;
                let side = match aggressor {
                    Some(Aggressor::Side(i)) => match field(&fields, i, n, "side")?.to_ascii_lowercase().as_str() {
                        "buy" | "b" => Side::Buy,
                        "sell" | "s" => Side::Sell,
                        other => return Err(format!("line {n}: the side \"{other}\" is not buy or sell")),
                    },
                    Some(Aggressor::BuyerMaker(i)) => {
                        match field(&fields, i, n, "is_buyer_maker")?.to_ascii_lowercase().as_str() {
                            "true" | "1" => Side::Sell,
                            "false" | "0" => Side::Buy,
                            other => return Err(format!("line {n}: is_buyer_maker \"{other}\" is not true or false")),
                        }
                    }
                    None => match last {
                        Some((p, side)) if p == price => side,
                        Some((p, _)) if price < p => Side::Sell,
                        _ => Side::Buy,
                    },
                };
                last = Some((price, side));
                trades.push(Trade {
                    time: field_time(&fields, time, n)?,
                    price,
                    qty: field_number(&fields, qty, delimiter, n, "quantity")?,
                    side,
                });
            }
            // Trades at the same time are all kept, in the file's order.
            trades.sort_by_key(|t| t.time);
            if trades.is_empty() {
                return Err("no rows under the header".into());
            }
            Ok(Rows::Trades(trades))
        }
    }
}

/// The fields of a line, trimmed and without quotes around them.
fn split<'a>(line: &'a str, delimiter: char, out: &mut Vec<&'a str>) {
    out.clear();
    out.extend(line.split(delimiter).map(|f| f.trim().trim_matches('"').trim()));
}

/// Finds the columns by the header's names.
fn columns(header: &[&str]) -> Result<Columns, String> {
    let names: Vec<String> = header
        .iter()
        .map(|h| h.to_ascii_lowercase().replace([' ', '-'], "_"))
        .collect();
    let find = |wanted: &[&str]| wanted.iter().find_map(|w| names.iter().position(|n| n == w));
    let date = find(&["date"]);
    let clock = find(&["time"]);
    let time = match (find(&TIME_NAMES), date, clock) {
        (Some(i), _, _) => When::One(i),
        (None, Some(d), Some(t)) => When::Joined(d, t),
        (None, Some(i), None) | (None, None, Some(i)) => When::One(i),
        (None, None, None) => return Err("no time column: name it time, date or timestamp".into()),
    };
    if let (Some(open), Some(high), Some(low), Some(close)) =
        (find(&["open"]), find(&["high"]), find(&["low"]), find(&["close"]))
    {
        return Ok(Columns::Candles {
            time,
            ohlc: [open, high, low, close],
            volume: find(&["volume", "vol"]),
        });
    }
    let Some(price) = find(&["price"]) else {
        return Err("neither candles (open, high, low, close) nor trades (price, qty) in the header".into());
    };
    let qty = find(&QTY_NAMES).ok_or("trades need a quantity column: qty, size or amount")?;
    let aggressor = match (find(&["side"]), find(&["is_buyer_maker", "isbuyermaker"])) {
        (Some(i), _) => Some(Aggressor::Side(i)),
        (None, Some(i)) => Some(Aggressor::BuyerMaker(i)),
        (None, None) => None,
    };
    Ok(Columns::Trades {
        time,
        price,
        qty,
        aggressor,
    })
}

fn field<'a>(fields: &[&'a str], i: usize, line: usize, name: &str) -> Result<&'a str, String> {
    fields
        .get(i)
        .copied()
        .ok_or_else(|| format!("line {line}: no {name}: the row has {} columns", fields.len()))
}

fn field_number(fields: &[&str], i: usize, delimiter: char, line: usize, name: &str) -> Result<f64, String> {
    let text = field(fields, i, line, name)?;
    number(text, delimiter).ok_or_else(|| format!("line {line}: the {name} \"{text}\" is not a number"))
}

fn field_time(fields: &[&str], when: When, line: usize) -> Result<Time, String> {
    let (text, joined);
    match when {
        When::One(i) => text = field(fields, i, line, "time")?,
        When::Joined(d, t) => {
            joined = format!(
                "{} {}",
                field(fields, d, line, "date")?,
                field(fields, t, line, "time")?
            );
            text = &joined;
        }
    }
    parse_time(text).ok_or_else(|| format!("line {line}: the time \"{text}\" is not a date or a unix time"))
}

/// A number; with a delimiter other than the comma, a decimal comma too.
fn number(text: &str, delimiter: char) -> Option<f64> {
    let value: f64 = if delimiter != ',' && text.contains(',') {
        text.replace(',', ".").parse().ok()?
    } else {
        text.parse().ok()?
    };
    value.is_finite().then_some(value)
}

/// A UTC time: a unix time in seconds, milliseconds, microseconds or nanoseconds (told apart by size), or a date
/// with an optional clock.
fn parse_time(text: &str) -> Option<Time> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        let n: i64 = text.parse().ok()?;
        // Seconds reach 1e11 only in the year 5138, milliseconds pass it in 1973.
        let ms = match n {
            n if n < 100_000_000_000 => n * 1000,
            n if n < 100_000_000_000_000 => n,
            n if n < 100_000_000_000_000_000 => n / 1000,
            n => n / 1_000_000,
        };
        return Some(Time(ms));
    }
    let text = text.strip_suffix(['Z', 'z']).unwrap_or(text);
    let (date, clock) = match text.split_once(['T', ' ']) {
        Some((date, clock)) => (date, Some(clock.trim())),
        None => (text, None),
    };
    let mut parts = date.split(['-', '.', '/']);
    let year: i64 = digits(parts.next()?)?;
    let month: u32 = digits(parts.next()?)?;
    let day: u32 = digits(parts.next()?)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    if i64::from(day) > days_from_civil(next_year, next_month, 1) - days_from_civil(year, month, 1) {
        return None;
    }
    let ms = match clock {
        None => 0,
        Some(clock) => {
            let mut parts = clock.split(':');
            let hour: i64 = digits(parts.next()?)?;
            let minute: i64 = digits(parts.next()?)?;
            let second = match parts.next() {
                None => 0,
                Some(s) => {
                    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
                    let whole: i64 = digits(whole)?;
                    if whole >= 60 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    // Milliseconds: the first three digits of the fraction.
                    let fraction = format!("{fraction:0<3}");
                    whole * 1000 + fraction[..3].parse::<i64>().ok()?
                }
            };
            if parts.next().is_some() || hour >= 24 || minute >= 60 {
                return None;
            }
            (hour * 60 + minute) * 60_000 + second
        }
    };
    Some(Time(time_of_date(year, month, day).0 + ms))
}

/// A whole number written with digits only.
fn digits<T: std::str::FromStr>(text: &str) -> Option<T> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The interval of candles: the most common gap between neighbours (the shorter of two as common), which must be
/// one the chart has.
fn interval_of(candles: &[Candle]) -> Result<Interval, String> {
    if candles.len() < 2 {
        return Err("one row is not enough to tell the timeframe".into());
    }
    let mut gaps: HashMap<i64, usize> = HashMap::new();
    for pair in candles.windows(2) {
        *gaps.entry(pair[1].time.0 - pair[0].time.0).or_default() += 1;
    }
    let (gap, _) = gaps
        .into_iter()
        .max_by_key(|&(gap, count)| (count, std::cmp::Reverse(gap)))
        .expect("two rows have a gap");
    INTERVALS.into_iter().find(|i| i.millis() == gap).ok_or_else(|| {
        let gap = if gap % 1000 == 0 {
            span_label(gap)
        } else {
            format!("{gap} ms")
        };
        let known: Vec<&str> = INTERVALS.iter().map(|i| i.as_str()).collect();
        format!(
            "the rows are mostly {gap} apart: not a timeframe the chart has ({})",
            known.join(", ")
        )
    })
}

/// The 1-minute candles of trades in time order.
fn minute_candles(trades: &[Trade]) -> Vec<Candle> {
    let minute = |t: &Trade| t.time.0.div_euclid(60_000);
    trades
        .chunk_by(|a, b| minute(a) == minute(b))
        .map(|group| {
            let (first, last) = (group[0], group[group.len() - 1]);
            Candle {
                time: Time(minute(&first) * 60_000),
                open: first.price,
                high: group.iter().map(|t| t.price).fold(f64::MIN, f64::max),
                low: group.iter().map(|t| t.price).fold(f64::MAX, f64::min),
                close: last.price,
                volume: group.iter().map(|t| t.qty).sum(),
            }
        })
        .collect()
}

/// Writes candles in time order a month a file.
fn write_candles(store: &Store, series: &Series, candles: &[Candle]) -> std::io::Result<()> {
    for month in candles.chunk_by(|a, b| month_of(a.time) == month_of(b.time)) {
        store.write(series, month_of(month[0].time), month)?;
    }
    Ok(())
}

fn month_of(time: Time) -> Period {
    let t = DateTime::from_time(time);
    Period::Month {
        year: t.year,
        month: t.month,
    }
}

fn day_of(time: Time) -> Period {
    let t = DateTime::from_time(time);
    Period::Day {
        year: t.year,
        month: t.month,
        day: t.day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::calendar::parse_time as typed;

    fn at(text: &str) -> Time {
        typed(text).unwrap()
    }

    fn candles(text: &str) -> Vec<Candle> {
        match parse(text) {
            Ok(Rows::Candles(candles)) => candles,
            other => panic!("not candles: {other:?}"),
        }
    }

    fn trades(text: &str) -> Vec<Trade> {
        match parse(text) {
            Ok(Rows::Trades(trades)) => trades,
            other => panic!("not trades: {other:?}"),
        }
    }

    #[test]
    fn times_come_in_many_forms() {
        let t = at("2024-03-01 14:30").0;
        for (text, ms) in [
            ("1709303400", t),
            ("1709303400000", t),
            ("1709303400000000", t),
            ("1709303400000000000", t),
            ("2024-03-01", at("2024-03-01").0),
            ("2024-03-01 14:30", t),
            ("2024-03-01T14:30:00Z", t),
            ("2024.03.01 14:30:05", t + 5000),
            ("2024/03/01T14:30:05.25", t + 5250),
            ("2024-03-01 14:30:05.123456", t + 5123),
            ("2024-02-29", at("2024-02-29").0),
        ] {
            assert_eq!(parse_time(text), Some(Time(ms)), "{text}");
        }
        for bad in [
            "",
            "abc",
            "2023-02-29",
            "2024-13-01",
            "2024-03-01 24:00",
            "2024-03-01 14:60",
            "-5",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn candles_are_read_with_or_without_a_header() {
        let with = candles(
            "Date;Open;High;Low;Close;Adj Close;Volume\n\
             2024-03-04;10,5;12;10;11;11;1000\n\
             2024-03-01;10;11;9;10,5;10,5;2000\n",
        );
        assert_eq!(
            with,
            [
                Candle {
                    time: at("2024-03-01"),
                    open: 10.0,
                    high: 11.0,
                    low: 9.0,
                    close: 10.5,
                    volume: 2000.0
                },
                Candle {
                    time: at("2024-03-04"),
                    open: 10.5,
                    high: 12.0,
                    low: 10.0,
                    close: 11.0,
                    volume: 1000.0
                },
            ],
            "sorted by time, decimal commas under semicolons"
        );
        let without = candles("1709251200,10,11,9,10.5\n\n1709510400,10.5,12,10,11\n");
        assert_eq!(without.len(), 2);
        assert_eq!((without[0].time, without[0].volume), (at("2024-03-01"), 0.0));
        let tabs = candles(
            "\u{feff}\"date\"\t\"time\"\t\"OPEN\"\t\"HIGH\"\t\"LOW\"\t\"CLOSE\"\n2024-03-01\t14:30\t1\t2\t0.5\t1.5\n",
        );
        assert_eq!(tabs[0].time, at("2024-03-01 14:30"), "a date and a time column joined");
        let repeated = candles("time,open,high,low,close\n60,1,1,1,1\n120,2,2,2,2\n60,3,3,3,3\n");
        assert_eq!(
            repeated.iter().map(|c| c.close).collect::<Vec<_>>(),
            [3.0, 2.0],
            "the last row of a time"
        );
    }

    #[test]
    fn bad_rows_are_named_by_their_line() {
        let bad = |text: &str| parse(text).unwrap_err();
        assert!(bad("").contains("empty"));
        assert_eq!(
            bad("time,open,high,low,close\n60,1,1,1,1\n\n120,1,x,1,1\n"),
            "line 4: the high \"x\" is not a number"
        );
        assert_eq!(
            bad("time,open,high,low,close\n60,1,1,1\n"),
            "line 2: no close: the row has 4 columns"
        );
        assert!(bad("time,open,high,low,close\nyesterday,1,1,1,1\n").starts_with("line 2: the time"));
        assert!(bad("when,open,high,low,close\n").contains("no time column"));
        assert!(bad("time,bid,ask\n").contains("neither candles"));
        assert!(bad("time,price,qty,side\n1,2,3,up\n").contains("line 2: the side \"up\""));
    }

    #[test]
    fn the_interval_is_the_most_common_gap() {
        let day = 86_400_000;
        // Business days: weekends make gaps of three days.
        let days = [0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 14];
        let rows: Vec<Candle> = days
            .iter()
            .map(|d| Candle {
                time: Time(d * day),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 0.0,
            })
            .collect();
        assert_eq!(interval_of(&rows), Ok(Interval::D1));
        let hourly: Vec<Candle> = rows
            .iter()
            .map(|c| Candle {
                time: Time(c.time.0 / 24),
                ..*c
            })
            .collect();
        assert_eq!(interval_of(&hourly), Ok(Interval::H1));
        let odd: Vec<Candle> = rows
            .iter()
            .map(|c| Candle {
                time: Time(c.time.0 * 2),
                ..*c
            })
            .collect();
        assert!(interval_of(&odd).unwrap_err().contains("mostly 2d apart"));
        assert!(interval_of(&rows[..1]).is_err());
    }

    #[test]
    fn trades_make_minute_candles() {
        let tape = trades(
            "Time,Price,Size,Side\n\
             2024-03-01 10:00:05,100,1,buy\n\
             2024-03-01 10:00:30,102,2,s\n\
             2024-03-01 10:00:59.999,99,1,Sell\n\
             2024-03-01 10:02:00,101,4,B\n",
        );
        assert_eq!(tape[1].side, Side::Sell);
        assert_eq!(tape[3].side, Side::Buy);
        let t = at("2024-03-01 10:00");
        assert_eq!(
            minute_candles(&tape),
            [
                Candle {
                    time: t,
                    open: 100.0,
                    high: 102.0,
                    low: 99.0,
                    close: 99.0,
                    volume: 4.0
                },
                Candle {
                    time: Time(t.0 + 120_000),
                    open: 101.0,
                    high: 101.0,
                    low: 101.0,
                    close: 101.0,
                    volume: 4.0
                },
            ]
        );
        let maker = trades("timestamp,price,qty,is_buyer_maker\n1709287205000,1,1,true\n1709287205000,1,1,false\n");
        assert_eq!(
            maker.iter().map(|t| t.side).collect::<Vec<_>>(),
            [Side::Sell, Side::Buy]
        );
        let ticks = trades("time,price,amount\n1,10,1\n2,9,1\n3,9,1\n4,11,1\n");
        assert_eq!(
            ticks.iter().map(|t| t.side).collect::<Vec<_>>(),
            [Side::Buy, Side::Sell, Side::Sell, Side::Buy],
            "without a side, by the tick"
        );
    }

    #[test]
    fn a_file_is_imported_as_its_symbol_and_replaced_by_the_next_import() {
        let root = std::env::temp_dir().join(format!("chart-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::new(root.join("data"));
        let file = root.join("acme corp.csv");
        let mut text = String::from("date,open,high,low,close,volume\n");
        for day in 0..40 {
            let (y, m, d) = tickerlight_core::calendar::civil_from_days(19_783 + day);
            text += &format!("{y}-{m:02}-{d:02},1,2,0.5,1.5,{day}\n");
        }
        std::fs::write(&file, &text).unwrap();
        let series = import(&store, &file).unwrap();
        assert_eq!(
            series,
            Series {
                venue: Venue::Imported,
                symbol: "ACME-CORP".into(),
                interval: Interval::D1,
            }
        );
        assert_eq!(store.periods(&series).unwrap().len(), 2, "a month a file");
        assert!(root.join("data/imported/ACME-CORP/1d/2024-03.bin").exists());

        // Again with fewer rows: the old months go.
        std::fs::write(
            &file,
            "date,open,high,low,close\n2024-03-01,1,1,1,1\n2024-03-02,1,1,1,1\n",
        )
        .unwrap();
        import(&store, &file).unwrap();
        assert_eq!(
            store.periods(&series).unwrap(),
            [Period::Month { year: 2024, month: 3 }]
        );

        let tape = root.join("acme corp.csv");
        std::fs::write(&tape, "time,price,qty\n2024-03-01 10:00,1,1\n2024-03-02 11:00:01,2,1\n").unwrap();
        let series = import(&store, &tape).unwrap();
        assert_eq!(series.interval, Interval::M1);
        assert_eq!(store.trade_days(&series).unwrap().len(), 2, "a day a file");
        assert_eq!(
            store.intervals(Venue::Imported, "ACME-CORP").unwrap(),
            [Interval::M1, Interval::D1]
        );

        let bad = root.join("bad.csv");
        std::fs::write(&bad, "time,open,high,low,close\n1,1,1,1\n").unwrap();
        assert!(import(&store, &bad).unwrap_err().contains("bad.csv: line 2"));
        assert!(
            import(&store, &root.join("missing.csv"))
                .unwrap_err()
                .starts_with("Cannot read")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn symbols_are_safe_folder_names() {
        assert_eq!(symbol_name("aapl daily").as_deref(), Some("AAPL-DAILY"));
        assert_eq!(symbol_name("brk.b_1d").as_deref(), Some("BRK.B_1D"));
        assert_eq!(symbol_name("../x").as_deref(), Some("..-X"));
        assert_eq!(symbol_name(".."), None);
        assert_eq!(symbol_name(" "), None);
    }
}
