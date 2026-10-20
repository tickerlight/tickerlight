//! Deribit's public data of its perpetual futures as `tickerlight-core` data: 1-minute candles from the API
//! (BTC-PERPETUAL from 2018-08), every trade from its history server, and the WebSocket's candles, trades and
//! tickers live. BTC-PERPETUAL and ETH-PERPETUAL are inverse (sized in dollars); the others, `<COIN>_USDC-PERPETUAL`,
//! are linear in USDC. No keys.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tickerlight_core::{Candle, Time, Trade};

pub use parse::IdTrade;

pub const DEFAULT_BASE: &str = "https://www.deribit.com";
/// Every trade is kept here; the main server has only the latest.
pub const HISTORY_BASE: &str = "https://history.deribit.com";

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Candles a request gives at most (the last ones before its end).
const PAGE: i64 = 5000;
/// Trades a request gives at most.
const TRADE_PAGE: usize = 1000;
/// Requests in flight at once: the history server takes over a second to answer.
const PARALLEL: usize = 8;
/// Between two requests of the program: Deribit gives an address about twenty a second.
const SPACING: Duration = Duration::from_millis(100);
/// How often a request refused for going too fast, or failed on Deribit's side, is tried again, a second apart.
const RETRIES: usize = 10;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error.
    Http(String),
    /// Deribit has no such instrument.
    Unknown,
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Unknown => write!(f, "Deribit has no such instrument"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Unknown)
    }
}

/// Whether an instrument is inverse: sized in dollars, settled in its coin (`BTC-PERPETUAL`, `ETH-PERPETUAL`).
pub fn is_inverse(symbol: &str) -> bool {
    !symbol.contains('_')
}

/// The perpetual as Deribit names it, from what someone typed: `btc` and `eth` are the inverse `BTC-PERPETUAL` and
/// `ETH-PERPETUAL`, other coins and anything in USDC the linear `<COIN>_USDC-PERPETUAL` (`sol`, `btc-usdc`).
/// `None` when nothing is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_uppercase();
    if text.ends_with("-PERPETUAL") {
        return Some(text);
    }
    let bare: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    let (base, usdc) = match bare.strip_suffix("USDC") {
        Some(base) => (base, true),
        None => (
            bare.strip_suffix("USDT")
                .or_else(|| bare.strip_suffix("USD"))
                .filter(|b| !b.is_empty())
                .unwrap_or(&bare),
            false,
        ),
    };
    if base.is_empty() {
        return None;
    }
    Some(match (base, usdc) {
        ("BTC" | "ETH", false) => format!("{base}-PERPETUAL"),
        _ => format!("{base}_USDC-PERPETUAL"),
    })
}

/// The last price of a perpetual and how busy it was over a day.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticker {
    pub symbol: String,
    pub last: f64,
    /// The change over the last 24 hours, in percent.
    pub change: f64,
    /// What was traded over the 24 hours, in dollars.
    pub quote_volume: f64,
    pub close_time: Time,
}

/// When the next request of the program may go: every [`Deribit`] waits its turn in one line.
static NEXT: Mutex<Option<Instant>> = Mutex::new(None);

fn pace() {
    let wait = {
        let mut next = NEXT.lock().expect("not poisoned");
        let now = Instant::now();
        let at = next.map_or(now, |n| n.max(now));
        *next = Some(at + SPACING);
        at - now
    };
    std::thread::sleep(wait);
}

/// Deribit's REST API and its history server.
pub struct Deribit {
    base: String,
    history: String,
    agent: ureq::Agent,
}

impl Default for Deribit {
    fn default() -> Self {
        Self::new()
    }
}

impl Deribit {
    pub fn new() -> Self {
        Self::with_bases(DEFAULT_BASE, HISTORY_BASE)
    }

    pub fn with_bases(base: impl Into<String>, history: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            history: history.into().trim_end_matches('/').to_string(),
            agent,
        }
    }

    /// A GET of `url`: the body, also of an error Deribit explains (400, an unknown instrument say). A refusal for
    /// going too fast, a failure on Deribit's side or a dropped connection is tried again a second later.
    fn get(&self, url: &str) -> Result<String, Error> {
        let mut failure = String::new();
        for _ in 0..RETRIES {
            pace();
            let answer = self.agent.get(url).call().and_then(|mut response| {
                let status = response.status().as_u16();
                let body = response.body_mut().with_config().limit(1 << 28).read_to_string()?;
                Ok((status, body))
            });
            match answer {
                Ok((status, body)) if (200..300).contains(&status) || status == 400 => return Ok(body),
                Ok((status, _)) if status == 429 || status >= 500 => failure = format!("status {status} for {url}"),
                Ok((status, _)) => return Err(Error::Http(format!("status {status} for {url}"))),
                Err(e) => failure = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(Error::Http(failure))
    }

    /// Every perpetual, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let mut all = Vec::new();
        for currency in ["BTC", "ETH", "USDC"] {
            let url = format!(
                "{}/api/v2/public/get_book_summary_by_currency?currency={currency}&kind=future",
                self.base
            );
            all.extend(parse::summaries(&self.get(&url)?)?);
        }
        all.retain(|t| t.quote_volume > 0.0);
        all.sort_by(|a, b| b.quote_volume.total_cmp(&a.quote_volume));
        Ok(all)
    }

    /// The 1-minute candles opening in `[from, from + a page)`, oldest first.
    fn candle_page(&self, symbol: &str, from: i64) -> Result<Vec<Candle>, Error> {
        let url = format!(
            "{}/api/v2/public/get_tradingview_chart_data?instrument_name={symbol}&start_timestamp={from}&end_timestamp={}&resolution=1",
            self.base,
            from + PAGE * MINUTE - 1
        );
        parse::candles(&self.get(&url)?)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. A minute
    /// without trades is flat at the close before, with no volume.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let span = PAGE * MINUTE;
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by(span as usize).collect();
        let mut out: Vec<Candle> = Vec::new();
        for chunk in starts.chunks(PARALLEL) {
            let pages: Vec<Result<Vec<Candle>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = chunk
                    .iter()
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, s)))
                    .collect();
                asked
                    .into_iter()
                    .map(|h| h.join().expect("a page does not panic"))
                    .collect()
            });
            for page in pages {
                out.extend(page?.into_iter().filter(|c| c.time >= from && c.time < to));
            }
        }
        out.sort_by_key(|c| c.time);
        out.dedup_by_key(|c| c.time);
        Ok(out)
    }

    /// The first day `symbol` traded on from `since` on (the day it was listed, or `since`'s), `None` if that is
    /// to come; an unknown instrument is an error ([`Error::is_unknown`]).
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        let url = format!("{}/api/v2/public/get_instrument?instrument_name={symbol}", self.base);
        let listed = parse::created(&self.get(&url)?)?;
        let first = listed.0.max(since.0);
        let day = first - first.rem_euclid(DAY);
        Ok((day <= now_ms()).then_some(Time(day)))
    }

    /// Every trade of `symbol` in `[from, to)` of one piece, a page after another.
    fn trade_piece(&self, symbol: &str, from: i64, to: i64) -> Result<Vec<IdTrade>, Error> {
        let mut out: Vec<IdTrade> = Vec::new();
        let mut start = from;
        loop {
            let url = format!(
                "{}/api/v2/public/get_last_trades_by_instrument_and_time?instrument_name={symbol}&start_timestamp={start}&end_timestamp={}&count={TRADE_PAGE}&sorting=asc",
                self.history,
                to - 1
            );
            let (page, more) = parse::trades(&self.get(&url)?)?;
            let newest = page.iter().map(|t| t.trade.time.0).max();
            out.extend(page);
            match newest {
                // The next page starts at the last millisecond had: its other trades may be on it.
                Some(newest) if more => start = newest.max(start + 1),
                _ => return Ok(out),
            }
        }
    }

    /// Every trade of `symbol` in `[from, to)`, by sequence (the order they were done in). An hour at a time, a few
    /// hours at once.
    pub fn trades_since(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        let starts: Vec<i64> = (from.0..to.0).step_by(HOUR as usize).collect();
        let mut out: Vec<IdTrade> = Vec::new();
        for chunk in starts.chunks(PARALLEL) {
            let pieces: Vec<Result<Vec<IdTrade>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = chunk
                    .iter()
                    .map(|&s| scope.spawn(move || self.trade_piece(symbol, s, (s + HOUR).min(to.0))))
                    .collect();
                asked
                    .into_iter()
                    .map(|h| h.join().expect("a piece does not panic"))
                    .collect()
            });
            for piece in pieces {
                out.extend(piece?);
            }
        }
        out.retain(|t| t.trade.time >= from && t.trade.time < to);
        out.sort_by_key(|t| t.id);
        out.dedup_by_key(|t| t.id);
        Ok(out)
    }

    /// Every trade of `symbol` in `[from, to)`, oldest first.
    pub fn trades(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Trade>, Error> {
        Ok(self
            .trades_since(symbol, from, to)?
            .into_iter()
            .map(|t| t.trade)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_coin_is_its_perpetual() {
        assert_eq!(symbol_of("btc").as_deref(), Some("BTC-PERPETUAL"));
        assert_eq!(symbol_of("ETHUSD").as_deref(), Some("ETH-PERPETUAL"));
        assert_eq!(symbol_of("btc-usdc").as_deref(), Some("BTC_USDC-PERPETUAL"));
        assert_eq!(symbol_of("sol").as_deref(), Some("SOL_USDC-PERPETUAL"));
        assert_eq!(symbol_of("solusdt").as_deref(), Some("SOL_USDC-PERPETUAL"));
        assert_eq!(symbol_of("1000pepe").as_deref(), Some("1000PEPE_USDC-PERPETUAL"));
        assert_eq!(symbol_of("sol_usdc-perpetual").as_deref(), Some("SOL_USDC-PERPETUAL"));
        assert_eq!(symbol_of("usdc").as_deref(), None);
        assert_eq!(symbol_of(" - "), None);
        assert!(is_inverse("BTC-PERPETUAL") && !is_inverse("BTC_USDC-PERPETUAL"));
    }
}
