//! Coinbase Exchange public market data as `tickerlight-core` data: 1-minute candles and the trade tape from the REST
//! API (`api.exchange.coinbase.com`), live from its WebSocket feed. Only public endpoints, no keys. Coinbase keeps
//! no archive files: history is paged from the API, 300 candles or 1000 trades a request, ten requests a second.

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

pub const DEFAULT_BASE: &str = "https://api.exchange.coinbase.com";

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// Candles a request gives at most.
const CANDLES_PAGE: i64 = 300;
/// Trades a request gives at most.
const TRADES_PAGE: i64 = 1000;
/// Coinbase answers ten public requests a second from one address.
const SPACING: Duration = Duration::from_millis(100);
/// How often a request refused for going too fast, or failed on Coinbase's side, is tried again.
const RETRIES: usize = 10;
/// Requests in flight at once: deep history takes Coinbase about a second to answer, so with the spacing ten go
/// out before the first comes back.
const PARALLEL: usize = 10;

#[derive(Debug)]
pub enum Error {
    /// The request failed or did not finish.
    Http(String),
    /// Coinbase answered with an error status.
    Api { status: u16, message: String },
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { status, message } => write!(f, "Coinbase error {status}: {message}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Coinbase does not know the product.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Api { status: 404, .. })
    }
}

/// Currencies symbols are quoted in, the longer of two that end alike first.
const QUOTES: [&str; 8] = ["USDT", "USDC", "USD", "EUR", "GBP", "BTC", "ETH", "DAI"];
/// Quotes worth a dollar.
const DOLLARS: [&str; 3] = ["USD", "USDC", "USDT"];

/// A symbol as Coinbase writes it, `BTC-USD`, from what someone typed: `btcusd`, `BTC/USD`. `None` when nothing
/// is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_uppercase().replace(['/', '_', ' '], "-");
    if text.is_empty() {
        return None;
    }
    if text.contains('-') {
        return Some(text);
    }
    Some(
        match QUOTES.iter().find(|q| text.len() > q.len() && text.ends_with(*q)) {
            Some(quote) => format!("{}-{quote}", &text[..text.len() - quote.len()]),
            None => text,
        },
    )
}

/// The last price of a product and how busy it was over a day.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticker {
    pub symbol: String,
    pub last: f64,
    /// The change over the last 24 hours, in percent.
    pub change: f64,
    /// What was traded over the 24 hours, in the quote currency.
    pub quote_volume: f64,
    pub close_time: Time,
}

/// The tickers that traded, the busiest in dollars first.
pub fn busiest(mut all: Vec<Ticker>) -> Vec<Ticker> {
    all.retain(|t| t.quote_volume > 0.0);
    let prices: HashMap<&str, f64> = all.iter().map(|t| (t.symbol.as_str(), t.last)).collect();
    let rate = |symbol: &str| {
        let quote = symbol.rsplit('-').next().unwrap_or_default();
        if DOLLARS.contains(&quote) {
            1.0
        } else {
            prices.get(format!("{quote}-USD").as_str()).copied().unwrap_or(0.0)
        }
    };
    let dollars: Vec<f64> = all.iter().map(|t| t.quote_volume * rate(&t.symbol)).collect();
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by(|&a, &b| dollars[b].total_cmp(&dollars[a]));
    order.into_iter().map(|i| all[i].clone()).collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The REST API of Coinbase Exchange's spot products.
pub struct Coinbase {
    base: String,
    agent: ureq::Agent,
    /// When the next request may go.
    next: Mutex<Instant>,
}

impl Default for Coinbase {
    fn default() -> Self {
        Self::new()
    }
}

impl Coinbase {
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

    /// Waits for this request's turn: requests go [`SPACING`] apart, from any thread.
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

    /// A GET of the API. A refusal for going too fast, a failure on Coinbase's side or a dropped connection is
    /// tried again a second later.
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, Error> {
        let mut url = format!("{}{path}", self.base);
        for (i, (key, value)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{key}={value}"));
        }
        let mut failure = String::new();
        for _ in 0..RETRIES {
            self.pace();
            let mut response = match self.agent.get(&url).call() {
                Ok(response) => response,
                Err(e) => {
                    failure = e.to_string();
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            let status = response.status().as_u16();
            let body = match response.body_mut().read_to_string() {
                Ok(body) => body,
                Err(e) => {
                    failure = e.to_string();
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            if status == 429 || status >= 500 {
                failure = format!("status {status} for {url}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(Error::Api {
                    status,
                    message: parse::message(&body),
                });
            }
            return Ok(body);
        }
        Err(Error::Http(failure))
    }

    /// Every product open for trading with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let products = self.get("/products", &[])?;
        let stats = self.get("/products/stats", &[])?;
        Ok(busiest(parse::tickers(&products, &stats, Time(now_ms()))?))
    }

    /// Candles of `granularity` seconds opening in `[start, end]`, oldest first.
    fn candle_page(&self, symbol: &str, granularity: i64, start: i64, end: i64) -> Result<Vec<Candle>, Error> {
        let query = [
            ("granularity", granularity.to_string()),
            ("start", (start / 1000).to_string()),
            ("end", (end / 1000).to_string()),
        ];
        parse::candles(&self.get(&format!("/products/{symbol}/candles"), &query)?)
    }

    /// The first day `symbol` traded on from `since` on, `None` if it has not since.
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        while start <= now {
            let end = start + (CANDLES_PAGE - 1) * DAY;
            if let Some(first) = self.candle_page(symbol, 86_400, start, end.min(now))?.first() {
                return Ok(Some(first.time));
            }
            start = end + DAY;
        }
        Ok(None)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. A minute
    /// without a trade has none.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by((CANDLES_PAGE * MINUTE) as usize).collect();
        let mut out = Vec::new();
        for starts in starts.chunks(PARALLEL) {
            let pages: Vec<Result<Vec<Candle>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = starts
                    .iter()
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, 60, s, s + (CANDLES_PAGE - 1) * MINUTE)))
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

    /// The trades before the one with id `before` (the newest without it), a thousand at most, oldest first.
    fn trade_page(&self, symbol: &str, before: Option<i64>, limit: i64) -> Result<Vec<IdTrade>, Error> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(id) = before {
            query.push(("after", id.to_string()));
        }
        parse::trades(&self.get(&format!("/products/{symbol}/trades"), &query)?)
    }

    /// The id of the first trade of `symbol` at `t` or later; one past the newest if there is none yet. Ids follow
    /// one another at a fairly even pace, so the guess is where `t` falls between the times of the ids known
    /// before and after it, every other one the middle: about a dozen requests.
    fn id_at(&self, symbol: &str, t: Time) -> Result<i64, Error> {
        let Some(newest) = self.trade_page(symbol, None, 1)?.pop() else {
            return Ok(0);
        };
        if newest.trade.time < t {
            return Ok(newest.id + 1);
        }
        // Every id up to `low` is before `t`, `high` is at or after it; with their times when known.
        let (mut low, mut high) = ((0, None), (newest.id, newest.trade.time));
        let mut middle = false;
        while high.0 - low.0 > 1 {
            let mid = match low.1 {
                Some(before) if !middle && high.1 > before => {
                    let share = (t.0 - before.0) as f64 / (high.1.0 - before.0) as f64;
                    (low.0 + (share * (high.0 - low.0) as f64) as i64).clamp(low.0 + 1, high.0 - 1)
                }
                _ => low.0 + (high.0 - low.0) / 2,
            };
            middle = !middle;
            match self.trade_page(symbol, Some(mid + 1), 1)?.pop() {
                Some(probe) if probe.trade.time >= t => high = (probe.id, probe.trade.time),
                // The ids between the probe and `mid` do not exist.
                probe => low = (mid, probe.map(|p| p.trade.time)),
            }
        }
        Ok(high.0)
    }

    /// The trades of `symbol` after the one with id `after` (or, without it, from `from` on), oldest first, up to
    /// now.
    pub fn trades_since(&self, symbol: &str, from: Time, after: Option<i64>) -> Result<Vec<IdTrade>, Error> {
        self.trades_between(symbol, from, None, after)
    }

    /// The trades of `symbol` in `[from, to)`, oldest first.
    pub fn trades(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Trade>, Error> {
        Ok(self
            .trades_between(symbol, from, Some(to), None)?
            .into_iter()
            .map(|t| t.trade)
            .collect())
    }

    fn trades_between(
        &self,
        symbol: &str,
        from: Time,
        to: Option<Time>,
        after: Option<i64>,
    ) -> Result<Vec<IdTrade>, Error> {
        let wanted = |t: &IdTrade| {
            t.trade.time >= from && to.is_none_or(|to| t.trade.time < to) && after.is_none_or(|a| t.id > a)
        };
        let enough = |t: &IdTrade| t.trade.time < from || after.is_some_and(|a| t.id <= a);
        let top = match to {
            Some(to) => Some(self.id_at(symbol, to)?),
            None => None,
        };
        let first = self.trade_page(symbol, top, TRADES_PAGE)?;
        let mut oldest = first.first().copied();
        let mut out: Vec<IdTrade> = first.into_iter().filter(wanted).collect();
        // The ids follow one another, so the next pages start a thousand apart and are asked for at once.
        while let Some(o) = oldest
            && !enough(&o)
            && o.id > 1
        {
            let pages: Vec<Result<Vec<IdTrade>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = (0..PARALLEL as i64)
                    .map(|k| o.id - k * TRADES_PAGE)
                    .filter(|&before| before > 1)
                    .map(|before| scope.spawn(move || self.trade_page(symbol, Some(before), TRADES_PAGE)))
                    .collect();
                asked
                    .into_iter()
                    .map(|h| h.join().expect("a page does not panic"))
                    .collect()
            });
            oldest = None;
            for page in pages {
                let page = page?;
                if let Some(&first) = page.first()
                    && oldest.is_none_or(|n: IdTrade| first.id < n.id)
                {
                    oldest = Some(first);
                }
                out.extend(page.into_iter().filter(wanted));
            }
        }
        out.sort_by_key(|t| t.id);
        out.dedup_by_key(|t| t.id);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_written_as_coinbase_writes_them() {
        assert_eq!(symbol_of("btcusd").as_deref(), Some("BTC-USD"));
        assert_eq!(symbol_of("eth/usdc").as_deref(), Some("ETH-USDC"));
        assert_eq!(symbol_of("SOL-EUR").as_deref(), Some("SOL-EUR"));
        assert_eq!(symbol_of("ethbtc").as_deref(), Some("ETH-BTC"));
        assert_eq!(symbol_of("  "), None);
    }

    #[test]
    fn the_busiest_are_counted_in_dollars() {
        let ticker = |symbol: &str, last: f64, quote_volume: f64| Ticker {
            symbol: symbol.into(),
            last,
            change: 0.0,
            quote_volume,
            close_time: Time(0),
        };
        let all = vec![
            ticker("ETH-BTC", 0.03, 100.0),
            ticker("BTC-USD", 80_000.0, 1_000_000.0),
            ticker("SOL-USDC", 150.0, 2_000_000.0),
            ticker("DEAD-USD", 1.0, 0.0),
        ];
        let got: Vec<String> = busiest(all).into_iter().map(|t| t.symbol).collect();
        // 100 BTC is eight million dollars.
        assert_eq!(got, ["ETH-BTC", "SOL-USDC", "BTC-USD"]);
    }
}
