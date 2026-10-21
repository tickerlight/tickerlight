//! Dukascopy's free history as `tickerlight-core` data: forex, metals, indices, stock and commodity CFDs, as 1-minute
//! candles of the bid. No keys; no stream.
//!
//! The datafeed keeps a file of 1-minute candles per day, out once the day has ended, and a file of ticks per
//! hour, out soon after the hour: the newest minutes are those of the last hour that ended. Files are LZMA;
//! months in their paths count from 0; prices are whole numbers of tenths of a pip. A closed market has flat
//! minutes without volume in the day files: those are left out. The datafeed refuses or drops requests now and
//! then: each is tried again a few times, waiting longer each time.

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::LiveCandles;

use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tickerlight_core::calendar::{DateTime, MS_PER_DAY, days_from_civil};
use tickerlight_core::{Candle, Time};

pub const DEFAULT_BASE: &str = "https://datafeed.dukascopy.com/datafeed";
/// The list of instruments Dukascopy's own pages load (JSONP).
pub const DEFAULT_INSTRUMENTS: &str = "https://freeserv.dukascopy.com/2.0/?path=common/instruments&jsonp=_";

const MS_PER_HOUR: i64 = 3_600_000;
/// How often a request is tried before it counts as failed.
const TRIES: u32 = 8;
/// The biggest file accepted, and the most a file may unpack to: an hour of ticks of a busy pair is well under a
/// megabyte packed and a few unpacked, a day of minutes 1440 × 24 bytes.
const MAX_FILE: u64 = 1 << 26;
/// Hours of ticks [`Dukascopy::recent`] goes back at most: the day files cover what is older.
const MAX_HOURS: i64 = 48;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// A file or the list was not what it should be.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// One of Dukascopy's instruments.
#[derive(Debug, Clone, PartialEq)]
pub struct Instrument {
    /// Its name in the datafeed, the one the program uses: `EURUSD`, `XAUUSD`, `AAPLUSUSD`, `USA500IDXUSD`.
    pub symbol: String,
    /// `Euro vs US Dollar`, `APPLE INC`, `USA 500 Index`.
    pub description: String,
    /// The price step the pip is: 0.0001 for EUR/USD, 0.01 for a stock.
    pub pip: f64,
    /// The first minute it has.
    pub start: Time,
}

impl Instrument {
    /// What the whole numbers of the files are divided by.
    pub fn scale(&self) -> f64 {
        10.0 / self.pip
    }
}

/// The instruments of Dukascopy's list, by symbol; those without a file in the datafeed are left out.
pub fn parse_instruments(text: &str) -> Result<Vec<Instrument>, Error> {
    let text = text.trim().trim_end_matches(';');
    let json = text
        .strip_prefix("_(")
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or(text);
    let value: Value = serde_json::from_str(json).map_err(|e| Error::Parse(format!("instruments: {e}")))?;
    let all = value["instruments"]
        .as_object()
        .ok_or_else(|| Error::Parse("no instruments in the list".into()))?;
    let mut out: Vec<Instrument> = all
        .values()
        .filter_map(|i| {
            let symbol = i["historical_filename"].as_str().filter(|s| !s.is_empty())?;
            Some(Instrument {
                symbol: symbol.to_string(),
                description: i["description"].as_str().unwrap_or_default().to_string(),
                pip: i["pipValue"].as_f64().filter(|&p| p > 0.0)?,
                start: Time(
                    i["history_start_60sec"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                ),
            })
        })
        .collect();
    out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    out.dedup_by(|a, b| a.symbol == b.symbol);
    Ok(out)
}

/// The symbol a typed name stands for: `eur/usd` and `EURUSD` are `EURUSD`, `AAPL.US/USD` is `AAPLUSUSD`.
pub fn symbol_of(text: &str) -> Option<String> {
    let symbol: String = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, '/' | '.' | '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (!symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(symbol)
}

fn unlzma(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Capped(Vec::new());
    // The datafeed has empty files for hours without ticks.
    if !data.is_empty() {
        // The memory limit keeps the dictionary a file asks for within what it may unpack to; the output is capped
        // as it is written.
        let options = lzma_rs::decompress::Options {
            memlimit: Some(MAX_FILE as usize),
            ..Default::default()
        };
        lzma_rs::lzma_decompress_with_options(&mut std::io::Cursor::new(data), &mut out, &options)
            .map_err(|e| Error::Parse(format!("LZMA: {e:?}")))?;
    }
    Ok(out.0)
}

/// What a file unpacks to, refusing to grow past [`MAX_FILE`].
struct Capped(Vec<u8>);

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if (self.0.len() + buf.len()) as u64 > MAX_FILE {
            return Err(std::io::Error::other(format!("unpacks to more than {MAX_FILE} bytes")));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be_f32(b: &[u8]) -> f32 {
    f32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The minutes of a day file (`day` is its midnight): records of 24 bytes, the second of the day, then open,
/// close, low and high in tenths of a pip, then the volume.
pub fn decode_minutes(data: &[u8], day: Time, scale: f64) -> Result<Vec<Candle>, Error> {
    let raw = unlzma(data)?;
    let (records, rest) = raw.as_chunks::<24>();
    if !rest.is_empty() {
        return Err(Error::Parse(format!("a day file of {} bytes", raw.len())));
    }
    let price = |b: &[u8]| f64::from(be_u32(b)) / scale;
    Ok(records
        .iter()
        .filter(|r| be_f32(&r[20..]) > 0.0)
        .map(|r| Candle {
            time: Time(day.0 + i64::from(be_u32(r)) * 1000),
            open: price(&r[4..]),
            close: price(&r[8..]),
            low: price(&r[12..]),
            high: price(&r[16..]),
            volume: f64::from(be_f32(&r[20..])),
        })
        .collect())
}

/// The minutes of an hour of ticks (`hour` is its start), made the way the day files are: the bid's open, high,
/// low and close, the bid volumes added. A tick is 20 bytes: the millisecond of the hour, the ask and the bid in
/// tenths of a pip, then the ask and bid volumes.
pub fn decode_ticks(data: &[u8], hour: Time, scale: f64) -> Result<Vec<Candle>, Error> {
    let raw = unlzma(data)?;
    let (ticks, rest) = raw.as_chunks::<20>();
    if !rest.is_empty() {
        return Err(Error::Parse(format!("a tick file of {} bytes", raw.len())));
    }
    let mut out: Vec<Candle> = Vec::new();
    for tick in ticks {
        let ms = i64::from(be_u32(tick));
        let minute = Time(hour.0 + ms - ms % 60_000);
        let bid = f64::from(be_u32(&tick[8..])) / scale;
        let volume = f64::from(be_f32(&tick[16..]));
        match out.last_mut() {
            Some(c) if c.time == minute => {
                c.high = c.high.max(bid);
                c.low = c.low.min(bid);
                c.close = bid;
                c.volume += volume;
            }
            Some(c) if c.time > minute => return Err(Error::Parse("ticks out of order".into())),
            _ => out.push(Candle {
                time: minute,
                open: bid,
                high: bid,
                low: bid,
                close: bid,
                volume,
            }),
        }
    }
    Ok(out)
}

/// The datafeed and the list of instruments.
pub struct Dukascopy {
    base: String,
    instruments: String,
    agent: ureq::Agent,
}

impl Default for Dukascopy {
    fn default() -> Self {
        Self::new()
    }
}

impl Dukascopy {
    pub fn new() -> Self {
        Self::with_bases(DEFAULT_BASE, DEFAULT_INSTRUMENTS)
    }

    pub fn with_bases(base: impl Into<String>, instruments: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            instruments: instruments.into(),
            agent,
        }
    }

    /// A GET; `None` when there is no such file. Refusals, server errors and dropped connections are tried again.
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, Error> {
        let mut why = String::new();
        for attempt in 0..TRIES {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(500 << attempt.min(4)));
            }
            let mut response = match self.agent.get(url).call() {
                Ok(response) => response,
                Err(e) => {
                    why = e.to_string();
                    continue;
                }
            };
            let status = response.status().as_u16();
            match status {
                404 => return Ok(None),
                200..=299 => {}
                429 | 500..=599 => {
                    why = format!("status {status}");
                    continue;
                }
                _ => return Err(Error::Http(format!("status {status} for {url}"))),
            }
            match tickerlight_core::read_capped(response.body_mut().as_reader(), MAX_FILE) {
                Ok(body) => return Ok(Some(body)),
                Err(e) => why = e.to_string(),
            }
        }
        Err(Error::Http(format!("{why} for {url}")))
    }

    /// The list of instruments as it came, for [`parse_instruments`].
    pub fn instruments_text(&self) -> Result<String, Error> {
        let body = self
            .get(&self.instruments)?
            .ok_or_else(|| Error::Http("no list of instruments".into()))?;
        String::from_utf8(body).map_err(|e| Error::Parse(e.to_string()))
    }

    /// The minutes of a day (UTC) with trading, `None` when the file is not out (yet).
    pub fn day(&self, instrument: &Instrument, year: i64, month: u32, day: u32) -> Result<Option<Vec<Candle>>, Error> {
        let url = format!(
            "{}/{}/{year}/{:02}/{day:02}/BID_candles_min_1.bi5",
            self.base,
            instrument.symbol,
            month - 1
        );
        let start = Time(days_from_civil(year, month, day) * MS_PER_DAY);
        self.get(&url)?
            .map(|data| decode_minutes(&data, start, instrument.scale()))
            .transpose()
    }

    /// The minutes of the hour starting at `hour`, from its ticks; `None` when the file is not out (yet).
    pub fn hour(&self, instrument: &Instrument, hour: Time) -> Result<Option<Vec<Candle>>, Error> {
        let t = DateTime::from_time(hour);
        let url = format!(
            "{}/{}/{}/{:02}/{:02}/{:02}h_ticks.bi5",
            self.base,
            instrument.symbol,
            t.year,
            t.month - 1,
            t.day,
            t.hour
        );
        self.get(&url)?
            .map(|data| decode_ticks(&data, hour, instrument.scale()))
            .transpose()
    }

    /// The minutes from `from` on, up to the last hour out by `now`, from the ticks of each hour; at most
    /// [`MAX_HOURS`] back.
    pub fn recent(&self, instrument: &Instrument, from: Time, now: Time) -> Result<Vec<Candle>, Error> {
        let end = now.0 - now.0.rem_euclid(MS_PER_HOUR);
        let mut hour = (from.0 - from.0.rem_euclid(MS_PER_HOUR)).max(end - MAX_HOURS * MS_PER_HOUR);
        let mut out = Vec::new();
        while hour < end {
            // An hour not out yet ends what there is.
            let Some(minutes) = self.hour(instrument, Time(hour))? else {
                break;
            };
            out.extend(minutes.into_iter().filter(|c| c.time >= from));
            hour += MS_PER_HOUR;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lzma(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        lzma_rs::lzma_compress(&mut std::io::Cursor::new(raw), &mut out).unwrap();
        out
    }

    #[test]
    fn a_day_file_reads_as_its_minutes_with_trading() {
        let mut raw = Vec::new();
        // 00:00 traded, 00:01 closed (flat, no volume), 23:59 traded.
        for (second, open, close, low, high, volume) in [
            (0u32, 113_269u32, 113_268u32, 113_257u32, 113_271u32, 117.97f32),
            (60, 113_268, 113_268, 113_268, 113_268, 0.0),
            (86_340, 112_483, 112_479, 112_479, 112_483, 84.5),
        ] {
            for v in [second, open, close, low, high] {
                raw.extend(v.to_be_bytes());
            }
            raw.extend(volume.to_be_bytes());
        }
        let day = Time(days_from_civil(2026, 10, 1) * MS_PER_DAY);
        let minutes = decode_minutes(&lzma(&raw), day, 1e5).unwrap();
        assert_eq!(minutes.len(), 2);
        let first = minutes[0];
        assert_eq!(first.time, day);
        assert_eq!(
            (first.open, first.high, first.low, first.close),
            (1.13269, 1.13271, 1.13257, 1.13268)
        );
        assert!((first.volume - 117.97).abs() < 1e-4);
        assert_eq!(minutes[1].time, Time(day.0 + 86_340_000));
        assert!(decode_minutes(&lzma(&raw[..30]), day, 1e5).is_err());
    }

    #[test]
    fn ticks_make_the_minutes_the_day_file_would() {
        let mut raw = Vec::new();
        for (ms, ask, bid, volume) in [
            (100u32, 157_570u32, 157_565u32, 1.5f32),
            (30_000, 157_640, 157_632, 2.0),
            (59_999, 157_630, 157_628, 0.5),
            (61_000, 157_700, 157_690, 1.0),
        ] {
            for v in [ms, ask, bid] {
                raw.extend(v.to_be_bytes());
            }
            raw.extend(9.0f32.to_be_bytes());
            raw.extend(volume.to_be_bytes());
        }
        let hour = Time(1_790_000_000_000 - 1_790_000_000_000 % MS_PER_HOUR);
        let minutes = decode_ticks(&lzma(&raw), hour, 1e3).unwrap();
        assert_eq!(minutes.len(), 2);
        let first = minutes[0];
        assert_eq!(first.time, hour);
        assert_eq!(
            (first.open, first.high, first.low, first.close),
            (157.565, 157.632, 157.565, 157.628)
        );
        assert_eq!(first.volume, 4.0);
        assert_eq!(minutes[1].time, Time(hour.0 + 60_000));
        // An hour without ticks is an empty file.
        assert_eq!(decode_ticks(&[], hour, 1e3).unwrap(), []);
    }

    #[test]
    fn the_list_gives_each_instrument_its_file_and_pip() {
        let text = r#"_({"instruments":{
            "EUR/USD":{"historical_filename":"EURUSD","description":"Euro vs US Dollar","pipValue":0.0001,
                "history_start_60sec":"1167609600000"},
            "E_SandP-500":{"historical_filename":"USA500IDXUSD","description":"USA 500 Index","pipValue":0.01,
                "history_start_60sec":"1356998400000"},
            "NONE":{"historical_filename":null,"pipValue":0.01}}});"#;
        let all = parse_instruments(text).unwrap();
        let symbols: Vec<&str> = all.iter().map(|i| i.symbol.as_str()).collect();
        assert_eq!(symbols, ["EURUSD", "USA500IDXUSD"]);
        assert_eq!(all[0].scale(), 100_000.0);
        assert_eq!(all[1].scale(), 1000.0);
        assert_eq!(all[1].start, Time(1_356_998_400_000));
        assert!(parse_instruments("<html>").is_err());
    }

    #[test]
    fn typed_names_become_symbols() {
        assert_eq!(symbol_of(" eur/usd ").as_deref(), Some("EURUSD"));
        assert_eq!(symbol_of("AAPL.US/USD").as_deref(), Some("AAPLUSUSD"));
        assert_eq!(symbol_of("E_SandP"), Some("E_SANDP".into()));
        assert_eq!(symbol_of("/"), None);
        assert_eq!(symbol_of("BTC$"), None);
    }
}
