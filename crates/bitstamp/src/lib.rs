//! Bitstamp's public spot market data as `tickerlight-core` data: 1-minute candles from the REST API (BTC/USD from
//! 2011-08), the trades of the last 24 hours (the API keeps no older ones) and the WebSocket, which has trades only;
//! live candles and prices are made from them. No keys.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tickerlight_core::{Candle, Time, Trade};

pub use parse::IdTrade;

pub const DEFAULT_BASE: &str = "https://www.bitstamp.net";

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// How far back the API has trades.
pub const TRADES_KEPT: i64 = DAY;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Candles a request gives at most.
const PAGE: i64 = 1000;
/// Bitstamp allows 8000 requests in 10 minutes from one address: a little under that.
const SPACING: Duration = Duration::from_millis(80);
/// Requests in flight at once.
const PARALLEL: usize = 8;
/// How often a request refused for going too fast, or failed on Bitstamp's side, is tried again, a second apart.
const RETRIES: usize = 10;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// Bitstamp does not know the pair (it answers 404).
    Unknown,
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Unknown => write!(f, "Bitstamp has no such pair"),
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

/// Currencies pairs are quoted in, the longer names first.
const QUOTES: [&str; 7] = ["USDT", "USDC", "USD", "EUR", "GBP", "BTC", "ETH"];

/// A pair as kept here, `BTCUSD` (Bitstamp writes `btcusd` in its addresses, `BTC/USD` in its names), from what
/// someone typed: `btc/usd`, `BTC-EUR`, `btc` (in dollars). `None` when nothing is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text: String = text
        .trim()
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if text.is_empty() {
        return None;
    }
    Some(match QUOTES.iter().any(|q| text.len() > q.len() && text.ends_with(q)) {
        true => text,
        false => format!("{text}USD"),
    })
}

/// The last price of a pair and how busy it was over a day.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticker {
    pub symbol: String,
    pub last: f64,
    /// The change over the last 24 hours, in percent.
    pub change: f64,
    /// What was traded over the 24 hours, in dollars (through the quote's dollar pair when it is not quoted in
    /// dollars).
    pub quote_volume: f64,
    pub close_time: Time,
}

/// Bitstamp's REST API.
pub struct Bitstamp {
    base: String,
    agent: ureq::Agent,
    /// When the next request may go.
    next: Mutex<Instant>,
}

impl Default for Bitstamp {
    fn default() -> Self {
        Self::new()
    }
}

impl Bitstamp {
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
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

    /// A GET of `path`: the body. 404 is an unknown pair; a refusal for going too fast, a failure on Bitstamp's
    /// side or a dropped connection is tried again a second later.
    fn get(&self, path: &str) -> Result<String, Error> {
        let url = format!("{}{path}", self.base);
        let mut failure = String::new();
        for _ in 0..RETRIES {
            self.pace();
            // A day of a busy pair's trades is a few megabytes.
            let answer = self.agent.get(&url).call().and_then(|mut response| {
                let status = response.status().as_u16();
                let body = response.body_mut().with_config().limit(1 << 28).read_to_string()?;
                Ok((status, body))
            });
            match answer {
                Ok((404, _)) => return Err(Error::Unknown),
                Ok((status, _)) if status == 429 || status >= 500 => failure = format!("status {status} for {url}"),
                Ok((status, body)) if (200..300).contains(&status) => return Ok(body),
                Ok((status, _)) => return Err(Error::Http(format!("status {status} for {url}"))),
                Err(e) => failure = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(Error::Http(failure))
    }

    /// Every pair that trades, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let mut all = parse::tickers(&self.get("/api/v2/ticker/")?)?;
        // Turnover in dollars: through the quote's dollar pair.
        let dollars: HashMap<String, f64> = all
            .iter()
            .filter_map(|t| Some((t.symbol.strip_suffix("USD")?.to_string(), t.last)))
            .collect();
        for t in &mut all {
            if let Some(quote) = QUOTES.iter().find(|q| t.symbol.ends_with(*q)).filter(|&&q| q != "USD") {
                t.quote_volume *= dollars.get(*quote).copied().unwrap_or(0.0);
            }
        }
        all.retain(|t| t.quote_volume > 0.0);
        all.sort_by(|a, b| b.quote_volume.total_cmp(&a.quote_volume));
        Ok(all)
    }

    /// The candles `step` seconds long opening in `[from, from + a page)`, oldest first.
    fn candle_page(&self, symbol: &str, step: i64, from: i64) -> Result<Vec<Candle>, Error> {
        let path = format!(
            "/api/v2/ohlc/{}/?step={step}&limit={PAGE}&start={}",
            symbol.to_ascii_lowercase(),
            from.div_euclid(1000)
        );
        parse::candles(&self.get(&path)?)
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
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, 60, s)))
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

    /// The first day `symbol` traded on from `since` on, `None` if it has not since; an unknown pair is an error
    /// ([`Error::is_unknown`]).
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        while start <= now {
            if let Some(first) = self.candle_page(symbol, 86_400, start)?.first() {
                return Ok(Some(first.time));
            }
            start += PAGE * DAY;
        }
        Ok(None)
    }

    /// Every trade of `symbol` in `[from, to)` of the last 24 hours, oldest first: older ones are gone. Their
    /// times are whole seconds.
    pub fn trades_since(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        // The last minute, hour or day: the shortest that reaches back to `from`.
        let back = now_ms() - from.0;
        let span = match back {
            ..55_000 => "minute",
            55_000..3_540_000 => "hour",
            _ => "day",
        };
        let path = format!("/api/v2/transactions/{}/?time={span}", symbol.to_ascii_lowercase());
        let mut out = parse::transactions(&self.get(&path)?)?;
        out.retain(|t| t.trade.time >= from && t.trade.time < to);
        out.sort_by_key(|t| t.id);
        Ok(out)
    }

    /// Every trade of `symbol` in `[from, to)` of the last 24 hours, oldest first.
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
    fn a_typed_symbol_is_a_pair_in_dollars_unless_it_names_its_quote() {
        assert_eq!(symbol_of("btc").as_deref(), Some("BTCUSD"));
        assert_eq!(symbol_of("btc/usd").as_deref(), Some("BTCUSD"));
        assert_eq!(symbol_of(" eth-eur ").as_deref(), Some("ETHEUR"));
        assert_eq!(symbol_of("ETHBTC").as_deref(), Some("ETHBTC"));
        assert_eq!(symbol_of("usdtusd").as_deref(), Some("USDTUSD"));
        assert_eq!(symbol_of(" / "), None);
    }
}
