//! The local candle cache: one file per archive period, so a download that stops halfway resumes where it
//! stopped and a finished period is never fetched again.
//!
//! Layout: `<root>/binance/<market>/<SYMBOL>/<interval>/<period>.bin`, `<root>/okx/<market>/<SYMBOL>/...` (OKX's
//! periods are its own, of UTC+8), and for symbols imported from files `<root>/imported/<SYMBOL>/<interval>/...`.
//! A file is the magic `CNDL`, a `u32` version, then rows of
//! the open time (`i64`, ms) and open, high, low, close, volume (`f64`), little-endian. An empty file (no rows)
//! records a period the archive will never have, such as one before the listing.
//!
//! Trades of a day: `<symbol folder>/trades/<day>.bin`, the magic `TRDS`, a `u32` version, then rows of the time
//! (`i64`, ms), price and quantity (`f64`) and the aggressor (`u8`, 0 buy, 1 sell).

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use tickerlight_binance::{Interval, Market, Period};
use tickerlight_core::{Candle, Side, Time, Trade};

const MAGIC: &[u8; 4] = b"CNDL";
const TRADES_MAGIC: &[u8; 4] = b"TRDS";
const VERSION: u32 = 1;
const HEADER: usize = 8;
const ROW: usize = 48;
const TRADE_ROW: usize = 25;
/// The folder of the trades beside the interval folders of a symbol.
const TRADES: &str = "trades";

/// Every interval, the shortest first.
pub const INTERVALS: [Interval; 13] = {
    use Interval::*;
    [M1, M3, M5, M15, M30, H1, H2, H4, H6, H8, H12, D1, W1]
};

/// Where a series comes from: a market of an exchange, Dukascopy's history, or files the user imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    Binance(Market),
    Okx(tickerlight_okx::Market),
    /// Coinbase Exchange's spot products: no archive files, history from the API.
    Coinbase,
    /// KuCoin's spot and perpetual futures: candles from the API, trades from the archive.
    Kucoin(tickerlight_kucoin::Market),
    /// Bitget's spot and USDT perpetual futures: candles from the API, trades from the archive or the API.
    Bitget(tickerlight_bitget::Market),
    /// Kraken's perpetual futures: candles and every trade from the API.
    Kraken,
    /// Bitstamp's spot pairs: candles from the API, trades of the last day only.
    Bitstamp,
    /// Bitfinex's spot pairs: candles and every trade from the API.
    Bitfinex,
    /// Deribit's perpetual futures: candles from the API, every trade from its history server.
    Deribit,
    /// Forex, metals, indices, stock and commodity CFDs: history only, up to the last full hour.
    Dukascopy,
    Imported,
}

impl Venue {
    /// Every venue in the order of the symbol panel: the exchanges' markets, Dukascopy, then files.
    pub const ALL: [Venue; 15] = [
        Venue::Binance(Market::Spot),
        Venue::Binance(Market::UsdFutures),
        Venue::Okx(tickerlight_okx::Market::Spot),
        Venue::Okx(tickerlight_okx::Market::Swap),
        Venue::Coinbase,
        Venue::Kucoin(tickerlight_kucoin::Market::Spot),
        Venue::Kucoin(tickerlight_kucoin::Market::Futures),
        Venue::Bitget(tickerlight_bitget::Market::Spot),
        Venue::Bitget(tickerlight_bitget::Market::Futures),
        Venue::Kraken,
        Venue::Bitstamp,
        Venue::Bitfinex,
        Venue::Deribit,
        Venue::Dukascopy,
        Venue::Imported,
    ];
    /// The sources as the symbol panel names them; files last.
    pub const EXCHANGES: [&str; 11] = [
        "Binance",
        "OKX",
        "Coinbase",
        "KuCoin",
        "Bitget",
        "Kraken",
        "Bitstamp",
        "Bitfinex",
        "Deribit",
        "Dukascopy",
        "File",
    ];
    /// The markets of an exchange as the symbol panel names them.
    pub const MARKETS: [&str; 2] = ["Spot", "Futures"];

    /// A short name, safe for file names and kept in saved text: the market's own name for Binance.
    pub fn name(self) -> &'static str {
        match self {
            Venue::Binance(market) => market.name(),
            Venue::Okx(tickerlight_okx::Market::Spot) => "okx-spot",
            Venue::Okx(tickerlight_okx::Market::Swap) => "okx-swap",
            Venue::Coinbase => "coinbase",
            Venue::Kucoin(tickerlight_kucoin::Market::Spot) => "kucoin-spot",
            Venue::Kucoin(tickerlight_kucoin::Market::Futures) => "kucoin-futures",
            Venue::Bitget(tickerlight_bitget::Market::Spot) => "bitget-spot",
            Venue::Bitget(tickerlight_bitget::Market::Futures) => "bitget-futures",
            Venue::Kraken => "kraken-futures",
            Venue::Bitstamp => "bitstamp",
            Venue::Bitfinex => "bitfinex",
            Venue::Deribit => "deribit-futures",
            Venue::Dukascopy => "dukascopy",
            Venue::Imported => "imported",
        }
    }

    pub fn from_name(name: &str) -> Option<Venue> {
        Venue::ALL.into_iter().find(|v| v.name() == name)
    }

    /// The venue as messages name it: "Binance spot", "OKX futures", "Coinbase", "file".
    pub fn label(self) -> &'static str {
        match self {
            Venue::Binance(Market::Spot) => "Binance spot",
            Venue::Binance(Market::UsdFutures) => "Binance futures",
            Venue::Okx(tickerlight_okx::Market::Spot) => "OKX spot",
            Venue::Okx(tickerlight_okx::Market::Swap) => "OKX futures",
            Venue::Coinbase => "Coinbase",
            Venue::Kucoin(tickerlight_kucoin::Market::Spot) => "KuCoin spot",
            Venue::Kucoin(tickerlight_kucoin::Market::Futures) => "KuCoin futures",
            Venue::Bitget(tickerlight_bitget::Market::Spot) => "Bitget spot",
            Venue::Bitget(tickerlight_bitget::Market::Futures) => "Bitget futures",
            Venue::Kraken => "Kraken futures",
            Venue::Bitstamp => "Bitstamp",
            Venue::Bitfinex => "Bitfinex",
            Venue::Deribit => "Deribit futures",
            Venue::Dukascopy => "Dukascopy",
            Venue::Imported => "file",
        }
    }

    /// Whether it has markets to choose (spot, futures).
    pub fn has_markets(self) -> bool {
        matches!(
            self,
            Venue::Binance(_) | Venue::Okx(_) | Venue::Kucoin(_) | Venue::Bitget(_)
        )
    }

    /// The Binance market, for a series of Binance.
    pub fn binance(self) -> Option<Market> {
        match self {
            Venue::Binance(market) => Some(market),
            _ => None,
        }
    }

    /// The OKX market, for a series of OKX.
    pub fn okx(self) -> Option<tickerlight_okx::Market> {
        match self {
            Venue::Okx(market) => Some(market),
            _ => None,
        }
    }

    /// Its place in [`Venue::ALL`].
    pub fn index(self) -> usize {
        Venue::ALL
            .iter()
            .position(|&v| v == self)
            .expect("every venue is listed")
    }

    /// The source (index into [`Venue::EXCHANGES`]) and the market (into [`Venue::MARKETS`]) of the symbol
    /// panel's choices.
    pub fn choices(self) -> (usize, usize) {
        match self {
            Venue::Binance(Market::Spot) => (0, 0),
            Venue::Binance(Market::UsdFutures) => (0, 1),
            Venue::Okx(tickerlight_okx::Market::Spot) => (1, 0),
            Venue::Okx(tickerlight_okx::Market::Swap) => (1, 1),
            Venue::Coinbase => (2, 0),
            Venue::Kucoin(tickerlight_kucoin::Market::Spot) => (3, 0),
            Venue::Kucoin(tickerlight_kucoin::Market::Futures) => (3, 1),
            Venue::Bitget(tickerlight_bitget::Market::Spot) => (4, 0),
            Venue::Bitget(tickerlight_bitget::Market::Futures) => (4, 1),
            Venue::Kraken => (5, 0),
            Venue::Bitstamp => (6, 0),
            Venue::Bitfinex => (7, 0),
            Venue::Deribit => (8, 0),
            Venue::Dukascopy => (9, 0),
            Venue::Imported => (10, 0),
        }
    }

    /// The venue of the symbol panel's choices.
    pub fn of_choices(exchange: usize, market: usize) -> Venue {
        match (exchange, market) {
            (0, 0) => Venue::Binance(Market::Spot),
            (0, _) => Venue::Binance(Market::UsdFutures),
            (1, 0) => Venue::Okx(tickerlight_okx::Market::Spot),
            (1, _) => Venue::Okx(tickerlight_okx::Market::Swap),
            (2, _) => Venue::Coinbase,
            (3, 0) => Venue::Kucoin(tickerlight_kucoin::Market::Spot),
            (3, _) => Venue::Kucoin(tickerlight_kucoin::Market::Futures),
            (4, 0) => Venue::Bitget(tickerlight_bitget::Market::Spot),
            (4, _) => Venue::Bitget(tickerlight_bitget::Market::Futures),
            (5, _) => Venue::Kraken,
            (6, _) => Venue::Bitstamp,
            (7, _) => Venue::Bitfinex,
            (8, _) => Venue::Deribit,
            (9, _) => Venue::Dukascopy,
            _ => Venue::Imported,
        }
    }

    /// Where the days of its trade archive start after midnight UTC: OKX's and Bitget's are of UTC+8.
    pub fn day_start(self) -> i64 {
        match self {
            Venue::Okx(_) => tickerlight_okx::DAY_START,
            Venue::Bitget(_) => tickerlight_bitget::DAY_START,
            Venue::Binance(_)
            | Venue::Coinbase
            | Venue::Kucoin(_)
            | Venue::Kraken
            | Venue::Bitstamp
            | Venue::Bitfinex
            | Venue::Deribit
            | Venue::Dukascopy
            | Venue::Imported => 0,
        }
    }

    /// Where the days of its candle periods start after midnight UTC: OKX's archive files are of UTC+8; candles
    /// asked of an API by time are kept in days of UTC.
    pub fn period_start(self) -> i64 {
        match self {
            Venue::Okx(_) => tickerlight_okx::DAY_START,
            _ => 0,
        }
    }

    /// The symbol as the venue writes it, from what was typed; `None` if nothing names one or what does could not
    /// name its folder ([`path_safe`]).
    pub fn symbol_of(self, text: &str) -> Option<String> {
        match self {
            Venue::Binance(_) => {
                let symbol: String = (text.trim().to_ascii_uppercase().chars())
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                Some(symbol).filter(|s| !s.is_empty())
            }
            Venue::Okx(_) => tickerlight_okx::symbol_of(text),
            Venue::Coinbase => tickerlight_coinbase::symbol_of(text),
            Venue::Kucoin(market) => tickerlight_kucoin::symbol_of(market, text),
            Venue::Bitget(_) => tickerlight_bitget::symbol_of(text),
            Venue::Kraken => tickerlight_kraken::symbol_of(text),
            Venue::Bitstamp => tickerlight_bitstamp::symbol_of(text),
            Venue::Bitfinex => tickerlight_bitfinex::symbol_of(text),
            Venue::Deribit => tickerlight_deribit::symbol_of(text),
            Venue::Dukascopy => tickerlight_dukascopy::symbol_of(text),
            Venue::Imported => crate::import::symbol_name(text),
        }
        .filter(|s| path_safe(s))
    }

    /// An error of a request to it in words: what its answer means instead of the address and the status.
    /// Other errors are as they were.
    pub fn plain(self, error: &str) -> String {
        let Some(at) = error.find("request failed: ") else {
            return error.to_string();
        };
        let (before, failure) = (&error[..at], &error[at + "request failed: ".len()..]);
        let name = Venue::EXCHANGES[self.choices().0];
        let status = failure
            .strip_prefix("status ")
            .and_then(|s| s.split(' ').next()?.parse::<u16>().ok());
        let what = match status {
            Some(429) => format!("{name} asks to wait, too many requests: try again in a minute"),
            Some(401 | 403) => format!("{name} refused the request"),
            Some(404) => format!("{name} has no such data"),
            Some(code @ 500..) => format!("{name} is down or busy (error {code})"),
            Some(code) => format!("{name} answered with error {code}"),
            None => format!("{name} could not be reached: is the computer online?"),
        };
        format!("{before}{what}")
    }
}

/// Which series: one symbol of one venue at one interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub venue: Venue,
    pub symbol: String,
    pub interval: Interval,
}

/// Whether a symbol can name its folder: letters (of any script: Binance lists `币安人生USDT`), digits, `.`, `-` and
/// `_`, 1 to 64 of them, and not `.` or `..`. A symbol becomes a folder and file names, so one from a server, a
/// session file or the symbol field that is not like this (`../x`, `/tmp/x`) could put files outside the data
/// folder; it is refused where it comes in. Every real symbol of every venue is like this.
pub fn path_safe(symbol: &str) -> bool {
    (1..=64).contains(&symbol.chars().count())
        && symbol
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && symbol != "."
        && symbol != ".."
}

/// The folders of a data folder that hold only market data downloaded from a source, one a source.
pub const CACHE_FOLDERS: [&str; 10] = [
    "binance",
    "okx",
    "coinbase",
    "kucoin",
    "bitget",
    "kraken",
    "bitstamp",
    "bitfinex",
    "deribit",
    "dukascopy",
];

/// A data folder in the system's temporary folder for a run that must leave the user's alone: the market data of
/// `data` are linked in, so they are read and what is downloaded is kept, while sessions, drawings and settings
/// start empty there and stay there.
pub fn scratch_root(data: &Path, name: &str) -> std::io::Result<PathBuf> {
    let root = std::env::temp_dir().join(format!("tickerlight-{name}-{}", std::process::id()));
    // Removing it removes the links, not what they point to.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    for folder in CACHE_FOLDERS {
        let cached = data.join(folder);
        if cached.is_dir() {
            link(&cached, &root.join(folder))?;
        }
    }
    Ok(root)
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, at)
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of the venue's symbols.
    fn venue_dir(&self, venue: Venue) -> PathBuf {
        match venue {
            Venue::Binance(market) => self.root.join("binance").join(market.name()),
            Venue::Okx(market) => self.root.join("okx").join(market.name()),
            Venue::Coinbase => self.root.join("coinbase"),
            Venue::Kucoin(market) => self.root.join("kucoin").join(market.name()),
            Venue::Bitget(market) => self.root.join("bitget").join(market.name()),
            Venue::Kraken => self.root.join("kraken").join("futures"),
            Venue::Bitstamp => self.root.join("bitstamp"),
            Venue::Bitfinex => self.root.join("bitfinex"),
            Venue::Deribit => self.root.join("deribit").join("futures"),
            Venue::Dukascopy => self.root.join("dukascopy"),
            Venue::Imported => self.root.join("imported"),
        }
    }

    fn candles_dir(&self, series: &Series) -> PathBuf {
        debug_assert!(path_safe(&series.symbol), "{:?}", series.symbol);
        self.venue_dir(series.venue)
            .join(&series.symbol)
            .join(series.interval.as_str())
    }

    fn trades_dir(&self, series: &Series) -> PathBuf {
        debug_assert!(path_safe(&series.symbol), "{:?}", series.symbol);
        self.venue_dir(series.venue).join(&series.symbol).join(TRADES)
    }

    fn path(&self, series: &Series, period: Period) -> PathBuf {
        self.candles_dir(series).join(format!("{}.bin", period.label()))
    }

    /// The cached candles of a period, or `None` if it is not cached.
    pub fn read(&self, series: &Series, period: Period) -> io::Result<Option<Vec<Candle>>> {
        let bytes = match fs::read(self.path(series, period)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        decode(&bytes).map(Some)
    }

    /// Caches the candles of a period. The file appears whole or not at all.
    pub fn write(&self, series: &Series, period: Period, candles: &[Candle]) -> io::Result<()> {
        write_whole(&self.path(series, period), &encode(candles))
    }

    fn trades_path(&self, series: &Series, period: Period) -> PathBuf {
        self.trades_dir(series).join(format!("{}.bin", period.label()))
    }

    /// The cached trades of a period (of the series' symbol, whatever its interval), or `None` if not cached.
    pub fn read_trades(&self, series: &Series, period: Period) -> io::Result<Option<Vec<Trade>>> {
        let bytes = match fs::read(self.trades_path(series, period)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        decode_trades(&bytes).map(Some)
    }

    pub fn write_trades(&self, series: &Series, period: Period, trades: &[Trade]) -> io::Result<()> {
        write_whole(&self.trades_path(series, period), &encode_trades(trades))
    }

    /// The periods of the series with a candle file, the oldest first.
    pub fn periods(&self, series: &Series) -> io::Result<Vec<Period>> {
        periods_in(&self.candles_dir(series))
    }

    /// The days of the series' symbol with a trade file, the oldest first.
    pub fn trade_days(&self, series: &Series) -> io::Result<Vec<Period>> {
        periods_in(&self.trades_dir(series))
    }

    /// The intervals of a symbol with candle files, the shortest first.
    pub fn intervals(&self, venue: Venue, symbol: &str) -> io::Result<Vec<Interval>> {
        let mut out = Vec::new();
        for interval in INTERVALS {
            let series = Series {
                venue,
                symbol: symbol.into(),
                interval,
            };
            if !self.periods(&series)?.is_empty() {
                out.push(interval);
            }
        }
        Ok(out)
    }

    /// The symbols of a venue with a folder here, by name.
    pub fn symbols(&self, venue: Venue) -> Vec<String> {
        let mut symbols: Vec<String> = fs::read_dir(self.venue_dir(venue))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        symbols.sort();
        symbols
    }

    /// Removes the candles of the series, every period.
    pub fn remove_candles(&self, series: &Series) -> io::Result<()> {
        remove_dir(&self.candles_dir(series))
    }

    /// Removes the trades of the series' symbol, every day.
    pub fn remove_trades(&self, series: &Series) -> io::Result<()> {
        remove_dir(&self.trades_dir(series))
    }
}

fn remove_dir(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The periods named by the `.bin` files of a folder (none if it is missing), in time order.
fn periods_in(dir: &Path) -> io::Result<Vec<Period>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(period) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".bin"))
            .and_then(Period::from_label)
        {
            out.push(period);
        }
    }
    out.sort_by_key(|p| (p.start(), p.end()));
    Ok(out)
}

/// Writes a file so that it appears whole or not at all, also when two loads write it at the same time.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("a cache file is in a folder");
    fs::create_dir_all(dir)?;
    let writer = format!("{}-{:?}", std::process::id(), std::thread::current().id());
    let partial = path.with_extension(format!("{writer}.partial"));
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

fn encode_trades(trades: &[Trade]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + trades.len() * TRADE_ROW);
    out.extend_from_slice(TRADES_MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    for t in trades {
        out.extend_from_slice(&t.time.0.to_le_bytes());
        out.extend_from_slice(&t.price.to_le_bytes());
        out.extend_from_slice(&t.qty.to_le_bytes());
        out.push(match t.side {
            Side::Buy => 0,
            Side::Sell => 1,
        });
    }
    out
}

fn decode_trades(bytes: &[u8]) -> io::Result<Vec<Trade>> {
    let bad = |what: &str| io::Error::new(ErrorKind::InvalidData, format!("trade cache: {what}"));
    if bytes.len() < HEADER || &bytes[..4] != TRADES_MAGIC {
        return Err(bad("not a trade file"));
    }
    if bytes[4..8] != VERSION.to_le_bytes() {
        return Err(bad("unknown version"));
    }
    let rows = &bytes[HEADER..];
    if !rows.len().is_multiple_of(TRADE_ROW) {
        return Err(bad("cut short"));
    }
    let word = |row: &[u8; TRADE_ROW], at: usize| -> [u8; 8] { row[at..at + 8].try_into().expect("8 bytes") };
    rows.as_chunks::<TRADE_ROW>()
        .0
        .iter()
        .map(|row| {
            Ok(Trade {
                time: Time(i64::from_le_bytes(word(row, 0))),
                price: f64::from_le_bytes(word(row, 8)),
                qty: f64::from_le_bytes(word(row, 16)),
                side: match row[24] {
                    0 => Side::Buy,
                    1 => Side::Sell,
                    _ => return Err(bad("bad side")),
                },
            })
        })
        .collect()
}

fn encode(candles: &[Candle]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + candles.len() * ROW);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    for c in candles {
        out.extend_from_slice(&c.time.0.to_le_bytes());
        for value in [c.open, c.high, c.low, c.close, c.volume] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out
}

fn decode(bytes: &[u8]) -> io::Result<Vec<Candle>> {
    let bad = |what: &str| io::Error::new(ErrorKind::InvalidData, format!("candle cache: {what}"));
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err(bad("not a cache file"));
    }
    if bytes[4..8] != VERSION.to_le_bytes() {
        return Err(bad("unknown version"));
    }
    let rows = &bytes[HEADER..];
    if !rows.len().is_multiple_of(ROW) {
        return Err(bad("cut short"));
    }
    let word = |row: &[u8; ROW], i: usize| -> [u8; 8] { row[i * 8..i * 8 + 8].try_into().expect("8 bytes") };
    Ok(rows
        .as_chunks::<ROW>()
        .0
        .iter()
        .map(|row| {
            let f = |i| f64::from_le_bytes(word(row, i));
            Candle {
                time: Time(i64::from_le_bytes(word(row, 0))),
                open: f(1),
                high: f(2),
                low: f(3),
                close: f(4),
                volume: f(5),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("chart-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn series() -> Series {
        Series {
            venue: Venue::Binance(Market::Spot),
            symbol: "BTCUSDT".into(),
            interval: Interval::M1,
        }
    }

    #[test]
    fn real_symbols_can_name_a_folder_and_paths_cannot() {
        let real = [
            "BTCUSDT",
            "币安人生USDT",
            "BTC-USDT",
            "PF_XBTUSD",
            "BTC_USDC-PERPETUAL",
            "AAPL.US",
            "ACME-CORP",
        ];
        for symbol in real {
            assert!(path_safe(symbol), "{symbol}");
        }
        assert!(path_safe(&"A".repeat(64)));
        for symbol in ["", ".", "..", "../x", "/tmp/x", "a\\b", "C:x", "a b", &"A".repeat(65)] {
            assert!(!path_safe(symbol), "{symbol:?}");
        }
    }

    fn candle(minute: i64, close: f64) -> Candle {
        Candle {
            time: Time(minute * 60_000),
            open: 1.5,
            high: close + 1.0,
            low: 0.25,
            close,
            volume: 1e-8,
        }
    }

    #[test]
    fn candles_survive_a_round_trip_exactly() {
        let root = temp_root("round-trip");
        let store = Store::new(&root);
        let period = Period::Month { year: 2026, month: 9 };
        assert_eq!(store.read(&series(), period).unwrap(), None);

        let candles = vec![candle(0, 82_245.93), candle(1, 0.000_012_34), candle(2, f64::MAX)];
        store.write(&series(), period, &candles).unwrap();
        assert_eq!(store.read(&series(), period).unwrap(), Some(candles));
        let folder = root.join("binance/spot/BTCUSDT/1m");
        let names: Vec<_> = fs::read_dir(&folder).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["2026-09.bin"], "nothing half written is left");

        // An empty period is remembered as empty, not as missing.
        let before = Period::Month { year: 2016, month: 1 };
        store.write(&series(), before, &[]).unwrap();
        assert_eq!(store.read(&series(), before).unwrap(), Some(vec![]));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn trades_survive_a_round_trip_beside_the_candles() {
        let root = temp_root("trades");
        let store = Store::new(&root);
        let day = Period::Day {
            year: 2026,
            month: 10,
            day: 10,
        };
        assert_eq!(store.read_trades(&series(), day).unwrap(), None);
        let trades = vec![
            Trade {
                time: Time(1_791_590_400_005),
                price: 109.07,
                qty: 10.41,
                side: Side::Sell,
            },
            Trade {
                time: Time(1_791_590_400_223),
                price: 109.08,
                qty: 991.66,
                side: Side::Buy,
            },
        ];
        store.write_trades(&series(), day, &trades).unwrap();
        assert_eq!(store.read_trades(&series(), day).unwrap(), Some(trades));
        assert!(root.join("binance/spot/BTCUSDT/trades/2026-10-10.bin").exists());
        assert_eq!(store.read(&series(), day).unwrap(), None, "not mixed up with candles");
        let mut bad = encode_trades(&[]);
        bad.extend_from_slice(&[0; TRADE_ROW - 1]);
        bad.push(7);
        assert!(decode_trades(&bad).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn damaged_files_are_errors() {
        assert!(decode(b"").is_err());
        assert!(decode(b"XXXX\x01\0\0\0").is_err());
        assert!(decode(b"CNDL\x02\0\0\0").is_err());
        let mut cut = encode(&[candle(0, 1.0)]);
        cut.pop();
        assert!(decode(&cut).is_err());
        assert_eq!(decode(&encode(&[])).unwrap(), vec![]);
    }

    #[test]
    fn imported_symbols_have_a_folder_of_their_own() {
        let root = temp_root("imported");
        let store = Store::new(&root);
        let series = Series {
            venue: Venue::Imported,
            symbol: "ACME".into(),
            interval: Interval::D1,
        };
        assert_eq!(store.periods(&series).unwrap(), vec![]);
        let (march, january) = (
            Period::Month { year: 2024, month: 3 },
            Period::Month { year: 2024, month: 1 },
        );
        let day = Period::Day {
            year: 2024,
            month: 2,
            day: 5,
        };
        for period in [march, day, january] {
            store.write(&series, period, &[candle(0, 1.0)]).unwrap();
        }
        store.write_trades(&series, day, &[]).unwrap();
        assert!(root.join("imported/ACME/1d/2024-03.bin").exists());
        assert!(root.join("imported/ACME/trades/2024-02-05.bin").exists());
        assert_eq!(store.periods(&series).unwrap(), [january, day, march], "in time order");
        assert_eq!(store.trade_days(&series).unwrap(), [day]);
        assert_eq!(store.intervals(Venue::Imported, "ACME").unwrap(), [Interval::D1]);
        assert_eq!(store.symbols(Venue::Imported), ["ACME"]);
        assert_eq!(store.symbols(Venue::Binance(Market::Spot)), Vec::<String>::new());

        store.remove_candles(&series).unwrap();
        store.remove_trades(&series).unwrap();
        assert_eq!(store.periods(&series).unwrap(), vec![]);
        assert_eq!(store.trade_days(&series).unwrap(), vec![]);
        store.remove_candles(&series).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn venues_keep_the_names_saved_text_uses_and_the_panel_s_choices() {
        let names = Venue::ALL.map(Venue::name);
        assert_eq!(
            names,
            [
                "spot",
                "usd-futures",
                "okx-spot",
                "okx-swap",
                "coinbase",
                "kucoin-spot",
                "kucoin-futures",
                "bitget-spot",
                "bitget-futures",
                "kraken-futures",
                "bitstamp",
                "bitfinex",
                "deribit-futures",
                "dukascopy",
                "imported"
            ]
        );
        for venue in Venue::ALL {
            assert_eq!(Venue::from_name(venue.name()), Some(venue));
            assert_eq!(Venue::ALL[venue.index()], venue);
            let (exchange, market) = venue.choices();
            assert_eq!(Venue::of_choices(exchange, market), venue);
        }
        // A source without markets takes whichever was picked before.
        assert_eq!(Venue::of_choices(2, 1), Venue::Coinbase);
        assert_eq!(Venue::of_choices(5, 0), Venue::Kraken);
        assert_eq!(Venue::of_choices(10, 1), Venue::Imported);
        assert_eq!(Venue::from_name("futures"), None);
        let okx = Venue::Okx(tickerlight_okx::Market::Swap);
        assert_eq!(okx.symbol_of("btcusdt-swap").as_deref(), Some("BTC-USDT"));
        let path = Store::new("/data").venue_dir(okx);
        assert_eq!(path, Path::new("/data/okx/swap"));
    }
}
