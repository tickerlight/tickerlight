//! Kraken's public futures market data as `tickerlight-core` data: the perpetual contracts margined in any currency
//! (`PF_XBTUSD`, sizes in the base currency). 1-minute candles from the charts API (from each contract's start, in
//! 2022 for the first), every trade from the history API (pages of a thousand: a busy day is a few hundred pages,
//! so its pieces of ten minutes go at once) and the WebSocket, which has trades and tickers; live candles are made
//! from the trades. No keys.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tickerlight_core::{Candle, Time, Trade};

pub use parse::IdTrade;

pub const DEFAULT_BASE: &str = "https://futures.kraken.com";

const MINUTE: i64 = 60_000;
/// The trades are asked for in pieces this long, many at once: a busy piece is a few pages.
const PIECE: i64 = 10 * MINUTE;
const DAY: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Candles a request gives at most.
const PAGE: i64 = 2000;
/// Requests go at least this far apart, from any thread.
const SPACING: Duration = Duration::from_millis(25);
/// Requests in flight at once: each takes about half a second.
const PARALLEL: usize = 24;
/// How often a request refused for going too fast, or failed on Kraken's side, is tried again, a second apart.
const RETRIES: usize = 10;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// Kraken answered with an error.
    Api(String),
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api(e) => write!(f, "Kraken error: {e}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Kraken does not know the contract. Its candles and trades of one it does not know are just empty: only the
    /// ticker tells.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Api(e) if e == "contractNotFound")
    }
}

/// Currencies a typed symbol may be quoted in: the contracts are all in dollars.
const QUOTES: [&str; 3] = ["USDT", "USDC", "USD"];

/// A contract as Kraken writes it, `PF_XBTUSD`, from what someone typed: `PF_XBTUSD`, `pf-xbtusd`, `btcusdt`,
/// `BTC/USD`, `btc`. Bitcoin is XBT on Kraken. `None` when nothing is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_uppercase();
    if let Some(rest) = text.strip_prefix("PF_") {
        return (!rest.is_empty()).then_some(text);
    }
    // The prefix with another separator, as `PF-XRPUSD` or `PF/XRP`.
    if let Some(rest) = text
        .strip_prefix("PF")
        .and_then(|r| r.strip_prefix(|c: char| !c.is_ascii_alphanumeric()))
    {
        return symbol_of(rest);
    }
    let text: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    let base = QUOTES
        .iter()
        .find_map(|q| text.strip_suffix(q).filter(|b| !b.is_empty()))
        .unwrap_or(&text);
    if base.is_empty() {
        return None;
    }
    Some(format!("PF_{}USD", if base == "BTC" { "XBT" } else { base }))
}

/// The last price of a contract and how busy it was over a day.
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

/// Kraken's futures REST API.
pub struct Kraken {
    base: String,
    agent: ureq::Agent,
    /// When the next request may go.
    next: Mutex<Instant>,
}

impl Default for Kraken {
    fn default() -> Self {
        Self::new()
    }
}

impl Kraken {
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            agent,
            next: Mutex::new(Instant::now()),
        }
    }

    /// Waits for this request's turn.
    fn pace(&self) {
        let wait = {
            let mut next = self.next.lock().expect("not poisoned");
            let now = Instant::now();
            let at = (*next).max(now);
            *next = at + SPACING;
            at - now
        };
        std::thread::sleep(wait);
    }

    /// A GET of `path`: the body, also of an error Kraken writes in JSON. A refusal for going too fast, a failure on
    /// Kraken's side or a dropped connection is tried again a second later.
    fn get(&self, path: &str) -> Result<String, Error> {
        let url = format!("{}{path}", self.base);
        let mut failure = String::new();
        for _ in 0..RETRIES {
            self.pace();
            let answer = self.agent.get(&url).call().and_then(|mut response| {
                let status = response.status().as_u16();
                Ok((status, response.body_mut().read_to_string()?))
            });
            match answer {
                Ok((status, _)) if status == 429 || status >= 500 => failure = format!("status {status} for {url}"),
                Ok((status, body)) if (200..300).contains(&status) || body.starts_with('{') => return Ok(body),
                Ok((status, _)) => return Err(Error::Http(format!("status {status} for {url}"))),
                Err(e) => failure = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(Error::Http(failure))
    }

    /// Every perpetual contract trading now, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let mut all = parse::tickers(&self.get("/derivatives/api/v3/tickers")?)?;
        all.retain(|t| t.quote_volume > 0.0);
        all.sort_by(|a, b| b.quote_volume.total_cmp(&a.quote_volume));
        Ok(all)
    }

    /// The ticker of one contract; an unknown one is an error ([`Error::is_unknown`]).
    pub fn ticker(&self, symbol: &str) -> Result<Ticker, Error> {
        parse::ticker(&self.get(&format!("/derivatives/api/v3/tickers/{symbol}"))?)
    }

    /// The candles of `resolution` (`1m`, `1d`) opening in `[from, to)`, at most a page of them, oldest first.
    fn candle_page(&self, symbol: &str, resolution: &str, from: i64, to: i64) -> Result<Vec<Candle>, Error> {
        let path = format!(
            "/api/charts/v1/trade/{symbol}/{resolution}?from={}&to={}",
            from.div_euclid(1000),
            (to - 1).div_euclid(1000)
        );
        let mut page = parse::candles(&self.get(&path)?)?;
        page.retain(|c| c.time.0 >= from && c.time.0 < to);
        Ok(page)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let span = PAGE * MINUTE;
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by(span as usize).collect();
        let mut out: Vec<Candle> = Vec::new();
        for pages in in_parallel(&starts, |s| self.candle_page(symbol, "1m", s, (s + span).min(to.0))) {
            out.extend(pages?.into_iter().filter(|c| c.time >= from));
        }
        out.sort_by_key(|c| c.time);
        out.dedup_by_key(|c| c.time);
        Ok(out)
    }

    /// The first day `symbol` traded on from `since` on, `None` if it has not since; an unknown contract is an
    /// error ([`Error::is_unknown`]).
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        self.ticker(symbol)?;
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        while start <= now {
            let end = start + PAGE * DAY;
            if let Some(first) = self.candle_page(symbol, "1d", start, end)?.first() {
                return Ok(Some(first.time));
            }
            start = end;
        }
        Ok(None)
    }

    /// Every trade of `symbol` in `[from, to)`, oldest first, page after page: for a short span (the trades of a
    /// busy hour are a dozen pages).
    fn span_trades(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        let mut out: Vec<IdTrade> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut path = format!(
                "/api/history/v2/market/{symbol}/executions?since={}&before={}&sort=asc",
                from.0, to.0
            );
            if let Some(token) = &token {
                path.push_str(&format!("&continuationToken={}", parse::url_encoded(token)));
            }
            let (page, next) = parse::executions(&self.get(&path)?)?;
            let empty = page.is_empty();
            out.extend(page.into_iter().filter(|t| t.trade.time >= from && t.trade.time < to));
            match next {
                Some(next) if !empty => token = Some(next),
                _ => break,
            }
        }
        out.sort_by_key(|t| t.trade.time);
        Ok(out)
    }

    /// Every trade of `symbol` in `[from, to)` with its id, oldest first: its pieces of ten minutes at once.
    pub fn trades_since(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        let starts: Vec<i64> = (from.0..to.0).step_by(PIECE as usize).collect();
        let mut out = Vec::new();
        for piece in in_parallel(&starts, |s| {
            self.span_trades(symbol, Time(s), Time((s + PIECE).min(to.0)))
        }) {
            out.extend(piece?);
        }
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

/// `work` for each of `starts`, [`PARALLEL`] at once, the results in their order.
fn in_parallel<T: Send>(starts: &[i64], work: impl Fn(i64) -> T + Sync) -> Vec<T> {
    let work = &work;
    let mut out = Vec::with_capacity(starts.len());
    for chunk in starts.chunks(PARALLEL) {
        std::thread::scope(|scope| {
            let asked: Vec<_> = chunk.iter().map(|&s| scope.spawn(move || work(s))).collect();
            out.extend(asked.into_iter().map(|h| h.join().expect("a request does not panic")));
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_symbol_is_the_perpetual_in_dollars_with_bitcoin_as_xbt() {
        assert_eq!(symbol_of("btc").as_deref(), Some("PF_XBTUSD"));
        assert_eq!(symbol_of("BTC/USDT").as_deref(), Some("PF_XBTUSD"));
        assert_eq!(symbol_of(" eth-usd ").as_deref(), Some("PF_ETHUSD"));
        assert_eq!(symbol_of("pf_solusd").as_deref(), Some("PF_SOLUSD"));
        assert_eq!(symbol_of("PF-XRPUSD").as_deref(), Some("PF_XRPUSD"));
        assert_eq!(symbol_of("pf/btc").as_deref(), Some("PF_XBTUSD"));
        assert_eq!(symbol_of(" -/ "), None);
        assert_eq!(symbol_of("PF_"), None);
    }
}
