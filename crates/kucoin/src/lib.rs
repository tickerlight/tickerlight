//! KuCoin's public market data as `tickerlight-core` data, spot and perpetual futures: 1-minute candles from the REST
//! API (spot back to 2017, futures about two years), the trade tape from the archive
//! (`historical-data.kucoin.com`: a file a day from 2022-12-31 on, out about three hours after the day ends) and
//! the WebSocket. No keys. The archive's own candles are not used: their volumes are off (doubled in 2026, more in
//! 2023) where the API's agree with the trades. The API gives only the last hundred trades, so of today's trades
//! there are those streamed since connecting.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tickerlight_core::{Candle, Time, Trade};

pub use parse::{Contract, IdTrade};

pub const SPOT_BASE: &str = "https://api.kucoin.com";
pub const FUTURES_BASE: &str = "https://api-futures.kucoin.com";
pub const DEFAULT_ARCHIVE_BASE: &str = "https://historical-data.kucoin.com";

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// KuCoin weighs a candle request 3 of the 2000 an address may spend in 30 seconds: about 22 a second.
const SPACING: Duration = Duration::from_millis(50);
/// Requests in flight at once: each takes about a second, so with the spacing about twenty go a second.
const PARALLEL: usize = 20;
/// How often a request refused for going too fast, or failed on KuCoin's side, is tried again, a second apart.
const RETRIES: usize = 10;
/// The biggest archive file we accept: a day of the trades of a busy pair is a few megabytes.
const MAX_FILE: u64 = 1 << 30;

/// A KuCoin market: spot, or perpetual futures (`XBTUSDTM`, quoted in USDT, USDC or, inverse, USD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    Spot,
    Futures,
}

impl Market {
    /// A short name, safe for file names.
    pub fn name(self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::Futures => "futures",
        }
    }

    fn base(self) -> &'static str {
        match self {
            Market::Spot => SPOT_BASE,
            Market::Futures => FUTURES_BASE,
        }
    }

    /// Candles a request gives at most.
    fn page(self) -> i64 {
        match self {
            Market::Spot => 1500,
            Market::Futures => 200,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// KuCoin answered with an error code.
    Api { code: String, message: String },
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { code, message } => write!(f, "KuCoin error {code}: {message}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// KuCoin does not know the symbol: spot's "unsupported trading pair", futures' "no such contract".
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Api { code, .. } if ["400100", "404000", "200003"].contains(&code.as_str()))
    }
}

/// Currencies spot symbols are quoted in, the longer of two that end alike first.
const QUOTES: [&str; 9] = ["USDT", "USDC", "USD1", "BTC", "ETH", "KCS", "DAI", "EUR", "TRY"];
/// Currencies perpetual futures are quoted in.
const FUTURES_QUOTES: [&str; 3] = ["USDT", "USDC", "USD"];
/// Quotes worth a dollar.
const DOLLARS: [&str; 4] = ["USDT", "USDC", "USD1", "USD"];

/// The quote `symbol` ends with, of `quotes`, with something before it.
fn quoted(symbol: &str, quotes: &[&'static str]) -> Option<&'static str> {
    quotes
        .iter()
        .find(|q| symbol.len() > q.len() && symbol.ends_with(*q))
        .copied()
}

/// A symbol as KuCoin writes it, from what someone typed (`None` when nothing is left): spot `BTC-USDT` from
/// `btcusdt` or `BTC/USDT`; futures `XBTUSDTM` from `btcusdt`, `BTC-USDT` or `btc` (Bitcoin is XBT there, and a
/// contract without a quote is in USDT).
pub fn symbol_of(market: Market, text: &str) -> Option<String> {
    let text = text.trim().to_ascii_uppercase();
    if text.is_empty() {
        return None;
    }
    if market == Market::Spot {
        let text = text.replace(['/', '_', ' '], "-");
        if text.contains('-') {
            return Some(text);
        }
        return Some(match quoted(&text, &QUOTES) {
            Some(quote) => format!("{}-{quote}", &text[..text.len() - quote.len()]),
            None => text,
        });
    }
    let text: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    let pair = match text.strip_suffix('M') {
        Some(pair) if quoted(pair, &FUTURES_QUOTES).is_some() => pair.to_string(),
        _ if quoted(&text, &FUTURES_QUOTES).is_some() => text,
        _ => format!("{text}USDT"),
    };
    let pair = match pair.strip_prefix("BTC") {
        Some(quote) if FUTURES_QUOTES.contains(&quote) => format!("XBT{quote}"),
        _ => pair,
    };
    Some(format!("{pair}M"))
}

/// The last price of a symbol and how busy it was over a day.
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
        // A futures symbol has no dash: all of them are quoted in dollars.
        if !symbol.contains('-') || DOLLARS.contains(&quote) {
            1.0
        } else {
            prices.get(format!("{quote}-USDT").as_str()).copied().unwrap_or(0.0)
        }
    };
    let dollars: Vec<f64> = all.iter().map(|t| t.quote_volume * rate(&t.symbol)).collect();
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by(|&a, &b| dollars[b].total_cmp(&dollars[a]));
    order.into_iter().map(|i| all[i].clone()).collect()
}

/// A day of the archive (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

/// The REST API and the archive of a KuCoin market.
pub struct Kucoin {
    market: Market,
    base: String,
    archive: String,
    agent: ureq::Agent,
    /// When the next request may go.
    next: Mutex<Instant>,
    /// The contract of each futures symbol asked about.
    contracts: Mutex<HashMap<String, Contract>>,
}

impl Kucoin {
    pub fn new(market: Market) -> Self {
        Self::with_bases(market, market.base(), DEFAULT_ARCHIVE_BASE)
    }

    pub fn with_bases(market: Market, base: impl Into<String>, archive: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self {
            market,
            base: base.into().trim_end_matches('/').to_string(),
            archive: archive.into().trim_end_matches('/').to_string(),
            agent,
            next: Mutex::new(Instant::now()),
            contracts: Mutex::default(),
        }
    }

    pub fn market(&self) -> Market {
        self.market
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

    /// A request: the body of the answer, an error status or a body KuCoin's errors are written in. A refusal for
    /// going too fast or a failure on KuCoin's side is tried again a second later.
    fn request(&self, url: &str, post: bool) -> Result<(u16, String), Error> {
        let mut failure = String::new();
        for _ in 0..RETRIES {
            self.pace();
            let call = if post {
                self.agent.post(url).send_empty()
            } else {
                self.agent.get(url).call()
            };
            let mut response = call.map_err(|e| Error::Http(e.to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .body_mut()
                .with_config()
                .limit(MAX_FILE)
                .read_to_vec()
                .map_err(|e| Error::Http(e.to_string()))?;
            if status == 429 || status >= 500 {
                failure = format!("status {status} for {url}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            return Ok((status, String::from_utf8_lossy(&body).into_owned()));
        }
        Err(Error::Http(failure))
    }

    /// A GET of the API.
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, Error> {
        let mut url = format!("{}{path}", self.base);
        for (i, (key, value)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{key}={value}"));
        }
        let (status, body) = self.request(&url, false)?;
        if !(200..300).contains(&status) && !body.starts_with('{') {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        Ok(body)
    }

    /// Where the WebSocket is and the token to connect with: `(endpoint, token, ping interval in ms)`.
    pub fn bullet(&self) -> Result<(String, String, u64), Error> {
        let (_, body) = self.request(&format!("{}/api/v1/bullet-public", self.base), true)?;
        parse::bullet(&body)
    }

    /// What a lot of `symbol` is worth (spot: one unit); an error when there is no such contract.
    pub fn contract(&self, symbol: &str) -> Result<Contract, Error> {
        if self.market == Market::Spot {
            return Ok(Contract::SPOT);
        }
        if let Some(&c) = self.contracts.lock().expect("not poisoned").get(symbol) {
            return Ok(c);
        }
        let contract = parse::contract(&self.get(&format!("/api/v1/contracts/{symbol}"), &[])?)?;
        self.contracts
            .lock()
            .expect("not poisoned")
            .insert(symbol.to_string(), contract);
        Ok(contract)
    }

    /// Every symbol that traded in the last day, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        match self.market {
            Market::Spot => Ok(busiest(parse::tickers(&self.get("/api/v1/market/allTickers", &[])?)?)),
            Market::Futures => {
                let all = parse::contracts(&self.get("/api/v1/contracts/active", &[])?)?;
                let mut contracts = self.contracts.lock().expect("not poisoned");
                let tickers = all
                    .into_iter()
                    .map(|(ticker, contract)| {
                        contracts.insert(ticker.symbol.clone(), contract);
                        ticker
                    })
                    .collect();
                Ok(busiest(tickers))
            }
        }
    }

    /// Candles a day (`1440` minutes) or a minute long opening in `[start, end)`, oldest first.
    fn candle_page(&self, symbol: &str, minutes: i64, start: i64, end: i64) -> Result<Vec<Candle>, Error> {
        let body = match self.market {
            Market::Spot => {
                let kind = if minutes == 1 { "1min" } else { "1day" };
                let query = [
                    ("type", kind.to_string()),
                    ("symbol", symbol.to_string()),
                    ("startAt", (start / 1000).to_string()),
                    ("endAt", (end / 1000).to_string()),
                ];
                self.get("/api/v1/market/candles", &query)?
            }
            // `to` is in the span: the last candle of it opens a step before `end`.
            Market::Futures => {
                let query = [
                    ("symbol", symbol.to_string()),
                    ("granularity", minutes.to_string()),
                    ("from", start.to_string()),
                    ("to", (end - minutes * MINUTE).to_string()),
                ];
                self.get("/api/v1/kline/query", &query)?
            }
        };
        let futures = match self.market {
            Market::Spot => None,
            Market::Futures => Some(self.contract(symbol)?),
        };
        let mut candles = parse::candles(&body, futures)?;
        candles.retain(|c| c.time.0 >= start && c.time.0 < end);
        Ok(candles)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. Each
    /// request takes about a second, so [`PARALLEL`] go at once.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        // Asked first: a futures symbol KuCoin does not know is an error, not a span without candles.
        self.contract(symbol)?;
        let span = self.market.page() * MINUTE;
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by(span as usize).collect();
        let mut out: Vec<Candle> = Vec::new();
        for starts in starts.chunks(PARALLEL) {
            let pages: Vec<Result<Vec<Candle>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = starts
                    .iter()
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, 1, s, (s + span).min(to.0))))
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

    /// The first day `symbol` traded on from `since` on, `None` if it has not since.
    pub fn first_day(&self, symbol: &str, since: Time) -> Result<Option<Time>, Error> {
        self.contract(symbol)?;
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        while start <= now {
            let end = start + self.market.page() * DAY;
            if let Some(first) = self.candle_page(symbol, 1440, start, end.min(now + DAY))?.first() {
                return Ok(Some(first.time));
            }
            start = end;
        }
        Ok(None)
    }

    /// The last hundred trades of `symbol`, oldest first.
    pub fn last_trades(&self, symbol: &str) -> Result<Vec<IdTrade>, Error> {
        let query = [("symbol", symbol.to_string())];
        let path = match self.market {
            Market::Spot => "/api/v1/market/histories",
            Market::Futures => "/api/v1/trade/history",
        };
        parse::histories(&self.get(path, &query)?, self.contract(symbol)?)
    }

    /// The archive's name of a symbol: `BTCUSDT` for spot `BTC-USDT`, `BTCUSDTM` for futures `XBTUSDTM`.
    fn archive_name(&self, symbol: &str) -> String {
        match self.market {
            Market::Spot => symbol.replace('-', ""),
            Market::Futures => match symbol.strip_prefix("XBT") {
                Some(rest) => format!("BTC{rest}"),
                None => symbol.to_string(),
            },
        }
    }

    fn trades_url(&self, symbol: &str, day: Day) -> String {
        let name = self.archive_name(symbol);
        let Day { year, month, day } = day;
        format!(
            "{}/data/{}/daily/trades/{name}/{name}-trades-{year:04}-{month:02}-{day:02}.zip",
            self.archive,
            self.market.name()
        )
    }

    /// Every trade of a day of the archive, oldest first, or `None` when it has no such file.
    pub fn archive_trades(&self, symbol: &str, day: Day) -> Result<Option<Vec<Trade>>, Error> {
        let contract = self.contract(symbol)?;
        match self.file(&self.trades_url(symbol, day))? {
            Some(csv) => parse::archive_trades(&csv, contract).map(Some),
            None => Ok(None),
        }
    }

    /// The text inside a zip of the archive; `None` when there is no such file.
    fn file(&self, url: &str) -> Result<Option<String>, Error> {
        let mut response = self.agent.get(url).call().map_err(|e| Error::Http(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 404 || status == 403 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        let zip = response
            .body_mut()
            .with_config()
            .limit(MAX_FILE)
            .read_to_vec()
            .map_err(|e| Error::Http(e.to_string()))?;
        unzip_single(&zip).map(Some)
    }
}

/// The text of the only file inside a zip.
fn unzip_single(data: &[u8]) -> Result<String, Error> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| Error::Parse(e.to_string()))?;
    let mut file = zip.by_index(0).map_err(|e| Error::Parse(e.to_string()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| Error::Parse(e.to_string()))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_written_as_kucoin_writes_them() {
        let spot = |text| symbol_of(Market::Spot, text);
        assert_eq!(spot("btcusdt").as_deref(), Some("BTC-USDT"));
        assert_eq!(spot("eth/btc").as_deref(), Some("ETH-BTC"));
        assert_eq!(spot("KCS-USDT").as_deref(), Some("KCS-USDT"));
        assert_eq!(spot(""), None);
        let futures = |text| symbol_of(Market::Futures, text);
        assert_eq!(futures("btcusdt").as_deref(), Some("XBTUSDTM"));
        assert_eq!(futures("BTC-USDT").as_deref(), Some("XBTUSDTM"));
        assert_eq!(futures("XBTUSDTM").as_deref(), Some("XBTUSDTM"));
        assert_eq!(futures("btc").as_deref(), Some("XBTUSDTM"));
        assert_eq!(futures("btcusd").as_deref(), Some("XBTUSDM"));
        assert_eq!(futures("ETHUSDCM").as_deref(), Some("ETHUSDCM"));
        assert_eq!(
            futures("BTCDOMUSDT").as_deref(),
            Some("BTCDOMUSDTM"),
            "only Bitcoin itself is XBT"
        );
        assert_eq!(futures(" "), None);
    }

    #[test]
    fn the_archive_names_bitcoin_futures_btc() {
        let futures = Kucoin::new(Market::Futures);
        let day = Day {
            year: 2026,
            month: 10,
            day: 17,
        };
        assert!(
            futures
                .trades_url("XBTUSDTM", day)
                .ends_with("/data/futures/daily/trades/BTCUSDTM/BTCUSDTM-trades-2026-10-17.zip")
        );
        let spot = Kucoin::new(Market::Spot);
        assert!(
            spot.trades_url("BTC-USDT", day)
                .ends_with("/data/spot/daily/trades/BTCUSDT/BTCUSDT-trades-2026-10-17.zip")
        );
    }
}
