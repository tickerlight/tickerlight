//! Binance public market data as `tickerlight-core` data: candles (klines) and the trade tape.
//!
//! Only public endpoints, no keys. Parsing is separate from HTTP ([`parse`]) so it is tested
//! without a network. The default host is Binance's market-data mirror, which also answers from
//! places where `api.binance.com` refuses.

pub mod archive;
#[cfg(not(target_arch = "wasm32"))]
mod live;
pub mod parse;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{
    DEFAULT_STREAM_BASE, FUTURES_STREAM_BASE, KlineStream, LiveCandles, LivePrices, LiveTrades, TradeStream,
    stream_base,
};

use std::collections::HashMap;

pub use archive::{Archive, Market, Period};
pub use parse::Ticker;
use tickerlight_core::{Candles, Time, Trades};

pub const DEFAULT_BASE: &str = "https://data-api.binance.vision";
/// The USDⓈ-M futures API. Binance refuses it (status 451) in places its terms exclude.
pub const DEFAULT_FUTURES_BASE: &str = "https://fapi.binance.com";

/// Rows per request Binance allows for both endpoints.
const PAGE: usize = 1000;
/// The biggest answer of the API accepted, decompressed: the tickers of every symbol are a megabyte or so.
const MAX_ANSWER: u64 = 1 << 28;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// Binance answered with an error body (`{"code": ..., "msg": ...}`).
    Api { code: i64, message: String },
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { code, message } => write!(f, "binance error {code}: {message}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Candle sizes Binance offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    M1,
    M3,
    M5,
    M15,
    M30,
    H1,
    H2,
    H4,
    H6,
    H8,
    H12,
    D1,
    W1,
}

impl Interval {
    pub fn as_str(self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M3 => "3m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::M30 => "30m",
            Interval::H1 => "1h",
            Interval::H2 => "2h",
            Interval::H4 => "4h",
            Interval::H6 => "6h",
            Interval::H8 => "8h",
            Interval::H12 => "12h",
            Interval::D1 => "1d",
            Interval::W1 => "1w",
        }
    }

    pub fn millis(self) -> i64 {
        const MIN: i64 = 60_000;
        match self {
            Interval::M1 => MIN,
            Interval::M3 => 3 * MIN,
            Interval::M5 => 5 * MIN,
            Interval::M15 => 15 * MIN,
            Interval::M30 => 30 * MIN,
            Interval::H1 => 60 * MIN,
            Interval::H2 => 120 * MIN,
            Interval::H4 => 240 * MIN,
            Interval::H6 => 360 * MIN,
            Interval::H8 => 480 * MIN,
            Interval::H12 => 720 * MIN,
            Interval::D1 => 1440 * MIN,
            Interval::W1 => 10_080 * MIN,
        }
    }
}

/// Currencies symbols are quoted in, the longer of two that end alike first (`FDUSD` before `USD`).
const QUOTES: [&str; 30] = [
    "FDUSD", "USDT", "USDC", "TUSD", "BUSD", "USD1", "AEUR", "EURI", "DAI", "USD", "BTC", "ETH", "BNB", "SOL", "XRP",
    "TRX", "DOGE", "EUR", "GBP", "TRY", "BRL", "JPY", "IDR", "ARS", "MXN", "PLN", "RON", "UAH", "ZAR", "COP",
];
/// Quotes worth a dollar.
const DOLLARS: [&str; 7] = ["FDUSD", "USDT", "USDC", "TUSD", "BUSD", "USD1", "USD"];

/// What one unit of the quote currency of `symbol` is worth in dollars, from the other tickers: zero if unknown.
fn dollar_rate(prices: &HashMap<&str, f64>, symbol: &str) -> f64 {
    let Some(quote) = QUOTES.iter().find(|q| symbol.len() > q.len() && symbol.ends_with(*q)) else {
        return 0.0;
    };
    if DOLLARS.contains(quote) {
        return 1.0;
    }
    let price = |s: String| prices.get(s.as_str()).copied().filter(|p| *p > 0.0);
    price(format!("{quote}USDT"))
        .or_else(|| price(format!("USDT{quote}")).map(|p| 1.0 / p))
        // Through bitcoin, for currencies with no dollar pair (the euro, the yen).
        .or_else(|| Some(price("BTCUSDT".into())? / price(format!("BTC{quote}"))?))
        .unwrap_or(0.0)
}

/// The symbols of `all` traded in the last day, the busiest in dollars first: a pair quoted in rupiah trades
/// billions of them.
pub fn busiest(mut all: Vec<Ticker>) -> Vec<Ticker> {
    // Symbols no longer traded keep their last day: it ends well before the others'.
    let newest = all.iter().map(|t| t.close_time.0).max().unwrap_or(0);
    all.retain(|t| t.close_time.0 > newest - 86_400_000 && t.quote_volume > 0.0);
    let prices: HashMap<&str, f64> = all.iter().map(|t| (t.symbol.as_str(), t.last)).collect();
    let dollars: Vec<f64> = all
        .iter()
        .map(|t| t.quote_volume * dollar_rate(&prices, &t.symbol))
        .collect();
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by(|&a, &b| dollars[b].total_cmp(&dollars[a]));
    order.into_iter().map(|i| all[i].clone()).collect()
}

pub struct Binance {
    base: String,
    market: Market,
    agent: ureq::Agent,
}

impl Default for Binance {
    fn default() -> Self {
        Self::new()
    }
}

impl Binance {
    /// The spot API.
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    /// The API of a market at its usual address.
    pub fn for_market(market: Market) -> Self {
        match market {
            Market::Spot => Self::new(),
            Market::UsdFutures => Self::with_market_base(market, DEFAULT_FUTURES_BASE),
        }
    }

    /// The spot API at `base`.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self::with_market_base(Market::Spot, base)
    }

    /// The API at `base`. An `https` base is never left for plain `http` by a redirect (a local stand-in for tests
    /// is `http`).
    pub fn with_market_base(market: Market, base: impl Into<String>) -> Self {
        let base = base.into();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(base.starts_with("https://"))
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            base: base.trim_end_matches('/').to_string(),
            market,
            agent,
        }
    }

    /// The path of an endpoint (`klines`, `aggTrades`) in this market's API.
    fn path(&self, endpoint: &str) -> String {
        match self.market {
            Market::Spot => format!("/api/v3/{endpoint}"),
            Market::UsdFutures => format!("/fapi/v1/{endpoint}"),
        }
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, Error> {
        let mut request = self.agent.get(format!("{}{path}", self.base));
        for (k, v) in query {
            request = request.query(*k, v);
        }
        let mut response = request.call().map_err(|e| Error::Http(e.to_string()))?;
        let status = response.status();
        let body = tickerlight_core::read_capped_text(response.body_mut().as_reader(), MAX_ANSWER)
            .map_err(|e| Error::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(parse::api_error(&body).unwrap_or_else(|| Error::Http(format!("status {status}"))));
        }
        Ok(body)
    }

    /// Every symbol traded in the last day, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        Ok(busiest(parse::tickers(&self.get(&self.path("ticker/24hr"), &[])?)?))
    }

    /// Candles of `symbol` (like `"BTCUSDT"`) opening in `[from, to)`, oldest first. Downloads page after page.
    pub fn klines(&self, symbol: &str, interval: Interval, from: Time, to: Time) -> Result<Candles, Error> {
        let mut out = Candles::new();
        let mut start = from.0;
        while start < to.0 {
            let body = self.get(
                &self.path("klines"),
                &[
                    ("symbol", symbol.to_string()),
                    ("interval", interval.as_str().to_string()),
                    ("startTime", start.to_string()),
                    ("endTime", (to.0 - 1).to_string()),
                    ("limit", PAGE.to_string()),
                ],
            )?;
            let page = parse::klines(&body)?;
            let count = page.len();
            let Some(last) = page.last().map(|c| c.time) else { break };
            for candle in page {
                // A page can repeat the last candle of the previous one; skip what we have.
                if out.view().time().last().is_none_or(|&t| candle.time > t) && candle.time.0 < to.0 {
                    out.push(candle)
                        .map_err(|e| Error::Parse(format!("out of order: {e:?}")))?;
                }
            }
            if count < PAGE {
                break;
            }
            start = last.0 + 1;
        }
        Ok(out)
    }

    /// Aggregated trades of `symbol` with time in `[from, to)`, oldest first. This can be a lot of
    /// requests: a busy pair has hundreds of thousands of trades an hour.
    pub fn agg_trades(&self, symbol: &str, from: Time, to: Time) -> Result<Trades, Error> {
        let mut out = Trades::new();
        let mut failed = None;
        self.agg_trades_since(symbol, Since::Time(from), |t| {
            if t.trade.time.0 >= to.0 {
                return false;
            }
            if t.trade.time.0 >= from.0
                && let Err(e) = out.push(t.trade)
            {
                failed = Some(Error::Parse(format!("out of order: {e:?}")));
                return false;
            }
            true
        })?;
        failed.map_or(Ok(out), Err)
    }

    /// Hands every aggregated trade of `symbol` from `since` up to now to `each`, oldest first, until it returns
    /// false.
    pub fn agg_trades_since(
        &self,
        symbol: &str,
        since: Since,
        mut each: impl FnMut(parse::IdTrade) -> bool,
    ) -> Result<(), Error> {
        let mut since = since;
        loop {
            let mut query = vec![("symbol", symbol.to_string()), ("limit", PAGE.to_string())];
            match since {
                Since::Time(t) => query.push(("startTime", t.0.to_string())),
                Since::Id(id) => query.push(("fromId", id.to_string())),
            }
            let body = self.get(&self.path("aggTrades"), &query)?;
            let page = parse::agg_trades(&body)?;
            let count = page.len();
            let Some(last_id) = page.last().map(|t| t.id) else {
                return Ok(());
            };
            for t in page {
                if !each(t) {
                    return Ok(());
                }
            }
            if count < PAGE {
                return Ok(());
            }
            since = Since::Id(last_id + 1);
        }
    }
}

/// Where a run of trades starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    Time(Time),
    /// The id Binance gave the trade.
    Id(i64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_are_turned_into_dollars() {
        let prices: HashMap<&str, f64> = [("BTCUSDT", 80_000.0), ("USDTIDR", 16_000.0), ("BTCJPY", 12_000_000.0)]
            .into_iter()
            .collect();
        assert_eq!(dollar_rate(&prices, "ETHUSDT"), 1.0);
        assert_eq!(dollar_rate(&prices, "ETHFDUSD"), 1.0);
        assert_eq!(dollar_rate(&prices, "ETHBTC"), 80_000.0);
        assert_eq!(dollar_rate(&prices, "BTCIDR"), 1.0 / 16_000.0);
        assert_eq!(dollar_rate(&prices, "ETHJPY"), 80_000.0 / 12_000_000.0);
        assert_eq!(dollar_rate(&prices, "ETHXYZ"), 0.0);
    }
}
