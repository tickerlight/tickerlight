//! Bitfinex's public spot market data as `tickerlight-core` data: 1-minute candles from the REST API (BTC/USD from
//! 2013-04; a minute without trades has no candle), every trade from the API, and the WebSocket's candles, trades
//! and tickers live. No keys.

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

pub const DEFAULT_BASE: &str = "https://api-pub.bitfinex.com";

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Candles or trades a request gives at most.
const PAGE: i64 = 10_000;
/// Daily candles asked for at once: Bitfinex refuses some longer spans of them (`time_interval: invalid`).
const DAY_PAGE: i64 = 1000;
/// Bitfinex allows about 30 requests of candles a minute and 15 of trades from one address, and blocks one that
/// asks more for a minute.
const CANDLE_SPACING: Duration = Duration::from_secs(2);
const TRADE_SPACING: Duration = Duration::from_secs(4);
/// Requests in flight at once: the answers take longer than the spacing.
const PARALLEL: usize = 4;
/// How often a request refused for going too fast, or failed on Bitfinex's side, is tried again.
const RETRIES: usize = 10;
/// The biggest answer accepted, decompressed: a page of 10 000 candles or trades is about a megabyte.
const MAX_ANSWER: u64 = 1 << 28;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// Bitfinex does not list the pair.
    Unknown,
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Unknown => write!(f, "Bitfinex has no such pair"),
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

/// Currencies pairs are quoted in, the longer names first. Bitfinex calls tether `UST`.
const QUOTES: [&str; 8] = ["XAUT", "USD", "UST", "EUR", "GBP", "JPY", "BTC", "ETH"];

/// A pair as kept here: Bitfinex's name without its `t` and its colon (`BTCUSD`, `DOGEUSD` for `tDOGE:USD`), from
/// what someone typed: `btc/usd`, `BTC-USDT` (`BTCUST`), `doge` (in dollars). `None` when nothing is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text: String = text
        .trim()
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .replace("USDT", "UST");
    if text.is_empty() {
        return None;
    }
    Some(match QUOTES.iter().any(|q| text.len() > q.len() && text.ends_with(q)) {
        true => text,
        false => format!("{text}USD"),
    })
}

/// A pair's name as Bitfinex writes it after its `t` (`DOGE:USD`), as kept here.
fn kept_name(name: &str) -> String {
    name.replace(':', "")
}

/// The name of a kept pair in Bitfinex's addresses: `tBTCUSD`, with a colon when a currency is not three letters
/// (`tDOGE:USD`).
fn api_name(symbol: &str) -> String {
    let split = QUOTES
        .iter()
        .find(|q| symbol.len() > q.len() && symbol.ends_with(*q))
        .map(|q| symbol.split_at(symbol.len() - q.len()));
    match split {
        Some((base, quote)) if base.len() != 3 || quote.len() != 3 => format!("t{base}:{quote}"),
        _ => format!("t{symbol}"),
    }
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

/// When the next request of the program may go. Bitfinex counts an address's requests: every [`Bitfinex`] (the
/// loading, the live candles, the order flow, the watchlist) waits its turn in this one line.
static NEXT: Mutex<Option<Instant>> = Mutex::new(None);
/// How long Bitfinex blocks an address that asked too much.
const BLOCKED: Duration = Duration::from_secs(60);

/// Waits for a request's turn; the one after it waits `spacing` more.
fn pace(spacing: Duration) {
    let wait = {
        let mut next = NEXT.lock().expect("not poisoned");
        let now = Instant::now();
        let at = next.map_or(now, |n| n.max(now));
        *next = Some(at + spacing);
        at - now
    };
    std::thread::sleep(wait);
}

/// Holds every request back until Bitfinex lifts its block.
fn wait_out_block() {
    let mut next = NEXT.lock().expect("not poisoned");
    let until = Instant::now() + BLOCKED;
    *next = Some(next.map_or(until, |n| n.max(until)));
}

/// Bitfinex's REST API.
pub struct Bitfinex {
    base: String,
    agent: ureq::Agent,
}

impl Default for Bitfinex {
    fn default() -> Self {
        Self::new()
    }
}

impl Bitfinex {
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            agent,
        }
    }

    /// A GET of `path`: the body. A refusal for going too fast waits out Bitfinex's block; a failure on its side or
    /// a dropped connection is tried again a second later.
    fn get(&self, path: &str, spacing: Duration) -> Result<String, Error> {
        let url = format!("{}{path}", self.base);
        let mut failure = String::new();
        for _ in 0..RETRIES {
            pace(spacing);
            let answer = self.agent.get(&url).call().and_then(|mut response| {
                let status = response.status().as_u16();
                let body = tickerlight_core::read_capped_text(response.body_mut().as_reader(), MAX_ANSWER)?;
                Ok((status, body))
            });
            let wait = match answer {
                Ok((status, body)) if (200..300).contains(&status) => return Ok(body),
                Ok((429, _)) => {
                    failure = format!("status 429 for {url}");
                    wait_out_block();
                    Duration::ZERO
                }
                // A request Bitfinex finds wrong (a time span it does not take, say) fails the same way again.
                Ok((500, body)) if body.contains("10020") => return Err(Error::Http(format!("{body} for {url}"))),
                Ok((status, _)) if status >= 500 => {
                    failure = format!("status {status} for {url}");
                    Duration::from_secs(1)
                }
                Ok((status, _)) => return Err(Error::Http(format!("status {status} for {url}"))),
                Err(e) => {
                    failure = e.to_string();
                    Duration::from_secs(1)
                }
            };
            std::thread::sleep(wait);
        }
        Err(Error::Http(failure))
    }

    /// Whether Bitfinex lists the pair.
    fn listed(&self, symbol: &str) -> Result<bool, Error> {
        let body = self.get("/v2/conf/pub:list:pair:exchange", CANDLE_SPACING)?;
        let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| Error::Parse(e.to_string()))?;
        let pairs = value
            .pointer("/0")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Error::Parse("no pairs".into()))?;
        Ok(pairs
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|p| kept_name(p) == symbol))
    }

    /// Every pair that trades, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let body = self.get("/v2/tickers?symbols=ALL", CANDLE_SPACING)?;
        let mut all = parse::tickers(&body, Time(now_ms()))?;
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

    /// The candles of `frame` (`1m`, `1D`) opening in `[from, to)`, at most `limit`, oldest first. Bitfinex refuses
    /// an end some way past now (`time_interval: invalid`): it is now at the latest.
    fn candle_page(&self, symbol: &str, frame: &str, from: i64, to: i64, limit: i64) -> Result<Vec<Candle>, Error> {
        let path = format!(
            "/v2/candles/trade:{frame}:{}/hist?start={from}&end={}&limit={limit}&sort=1",
            api_name(symbol),
            (to - 1).min(now_ms())
        );
        parse::candles(&self.get(&path, CANDLE_SPACING)?)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. A minute
    /// without trades has none.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let span = PAGE * MINUTE;
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by(span as usize).collect();
        let mut out: Vec<Candle> = Vec::new();
        for chunk in starts.chunks(PARALLEL) {
            let pages: Vec<Result<Vec<Candle>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = chunk
                    .iter()
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, "1m", s, (s + span).min(to.0), PAGE)))
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

    /// The first day `symbol` traded on from `since` on, `None` if it has not since; a pair Bitfinex does not list
    /// is an error ([`Error::is_unknown`]).
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        let mut first = None;
        while start <= now && first.is_none() {
            let end = start + DAY_PAGE * DAY;
            first = self
                .candle_page(symbol, "1D", start, end, DAY_PAGE)?
                .first()
                .map(|c| c.time);
            start = end;
        }
        // An unknown pair has no candles either.
        if first.is_none() && !self.listed(symbol)? {
            return Err(Error::Unknown);
        }
        Ok(first)
    }

    /// Every trade of `symbol` in `[from, to)`, by id (which is the order they were done in).
    pub fn trades_since(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        let name = api_name(symbol);
        let mut out: Vec<IdTrade> = Vec::new();
        let mut start = from.0;
        loop {
            let path = format!(
                "/v2/trades/{name}/hist?start={start}&end={}&limit={PAGE}&sort=1",
                to.0 - 1
            );
            let page = parse::trades(&self.get(&path, TRADE_SPACING)?)?;
            let full = page.len() as i64 == PAGE;
            let newest = page.iter().map(|t| t.trade.time.0).max();
            out.extend(page);
            match newest {
                // The next page starts at the last millisecond had: its other trades may be on it.
                Some(newest) if full => start = newest.max(start + 1),
                _ => break,
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
    fn a_typed_symbol_is_a_pair_in_dollars_unless_it_names_its_quote() {
        assert_eq!(symbol_of("btc").as_deref(), Some("BTCUSD"));
        assert_eq!(symbol_of("btc/usd").as_deref(), Some("BTCUSD"));
        assert_eq!(symbol_of(" btc-usdt ").as_deref(), Some("BTCUST"));
        assert_eq!(symbol_of("usdtusd").as_deref(), Some("USTUSD"));
        assert_eq!(symbol_of("DOGE").as_deref(), Some("DOGEUSD"));
        assert_eq!(symbol_of("ETHBTC").as_deref(), Some("ETHBTC"));
        assert_eq!(symbol_of(" / "), None);
    }

    #[test]
    fn a_pair_has_a_colon_in_bitfinex_s_name_when_a_currency_is_not_three_letters() {
        assert_eq!(api_name("BTCUSD"), "tBTCUSD");
        assert_eq!(api_name("BTCUST"), "tBTCUST");
        assert_eq!(api_name("DOGEUSD"), "tDOGE:USD");
        assert_eq!(api_name("BTCXAUT"), "tBTC:XAUT");
        assert_eq!(api_name("AVAXBTC"), "tAVAX:BTC");
        assert_eq!(kept_name("DOGE:USD"), "DOGEUSD");
    }
}
