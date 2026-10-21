//! Bitget's public market data as `tickerlight-core` data, spot and USDT-margined perpetual futures: 1-minute candles
//! from the REST API (years back), the trade tape from the archive (`img.bitgetimg.com/online/trades`: a day in
//! parts of 100 000 trades, of Beijing time, kept only for about two months and out about a day and a half after
//! the day ends) or from the API (the last 90 days), and the WebSocket. No keys.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tickerlight_core::{Candle, Time, Trade};

pub use parse::IdTrade;

pub const DEFAULT_BASE: &str = "https://api.bitget.com";
pub const DEFAULT_ARCHIVE_BASE: &str = "https://img.bitgetimg.com/online";

/// Where the days of the archive start, after midnight UTC: 16:00 of the day before (UTC+8).
pub const DAY_START: i64 = -8 * 3_600_000;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Candles a request gives at most.
const PAGE: i64 = 200;
/// Trades a request gives at most.
const TRADES_PAGE: usize = 1000;
/// Bitget answers twenty candle requests a second from one address, ten of trades.
const CANDLES_SPACING: Duration = Duration::from_millis(50);
const TRADES_SPACING: Duration = Duration::from_millis(100);
/// Requests in flight at once: each takes about a second.
const PARALLEL: usize = 20;
/// How often a request refused for going too fast, or failed on Bitget's side, is tried again, a second apart.
const RETRIES: usize = 10;
/// The biggest answer or archive file we accept: a part is 100 000 trades, a few megabytes.
const MAX_FILE: u64 = 1 << 30;
/// The biggest CSV accepted out of a part of the archive: 100 000 trades are about 6 MB.
const MAX_PART_CSV: u64 = 256 << 20;

/// A Bitget market: spot, or USDT-margined perpetual futures. Both write symbols as `BTCUSDT`.
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

    /// The API's path and the query that picks the market, for an endpoint of `/market/`.
    fn path(self, endpoint: &str) -> (String, Vec<(&'static str, String)>) {
        match self {
            Market::Spot => (format!("/api/v2/spot/market/{endpoint}"), Vec::new()),
            Market::Futures => (
                format!("/api/v2/mix/market/{endpoint}"),
                vec![("productType", "USDT-FUTURES".to_string())],
            ),
        }
    }

    /// The granularity of a minute and of a day.
    fn granularity(self, minutes: i64) -> &'static str {
        match (self, minutes) {
            (Market::Spot, 1) => "1min",
            (Market::Spot, _) => "1day",
            (Market::Futures, 1) => "1m",
            (Market::Futures, _) => "1D",
        }
    }

    /// The stream's and the archive's names of the market.
    fn inst_type(self) -> &'static str {
        match self {
            Market::Spot => "SPOT",
            Market::Futures => "USDT-FUTURES",
        }
    }

    fn archive_dir(self) -> &'static str {
        match self {
            Market::Spot => "SPBL",
            Market::Futures => "UMCBL",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// Bitget answered with an error code.
    Api { code: String, message: String },
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { code, message } => write!(f, "Bitget error {code}: {message}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Bitget does not know the symbol ("parameter ... does not exist").
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Api { code, .. } if code == "40034")
    }
}

/// Currencies symbols are quoted in.
const QUOTES: [&str; 5] = ["USDT", "USDC", "BTC", "ETH", "EUR"];

/// A symbol as Bitget writes it, `BTCUSDT`, from what someone typed: `btc-usdt`, `BTC/USDT`, `btc` (in USDT).
/// `None` when nothing is left.
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
        false => format!("{text}USDT"),
    })
}

/// The last price of a symbol and how busy it was over a day.
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

/// A day of the archive (of Beijing time, see [`DAY_START`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

/// The REST API and the archive of a Bitget market.
pub struct Bitget {
    market: Market,
    base: String,
    archive: String,
    agent: ureq::Agent,
    /// When the next request may go.
    next: Mutex<Instant>,
}

impl Bitget {
    pub fn new(market: Market) -> Self {
        Self::with_bases(market, DEFAULT_BASE, DEFAULT_ARCHIVE_BASE)
    }

    pub fn with_bases(market: Market, base: impl Into<String>, archive: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self {
            market,
            base: base.into().trim_end_matches('/').to_string(),
            archive: archive.into().trim_end_matches('/').to_string(),
            agent,
            next: Mutex::new(Instant::now()),
        }
    }

    pub fn market(&self) -> Market {
        self.market
    }

    /// Waits for this request's turn: requests go `spacing` after the one before, from any thread.
    fn pace(&self, spacing: Duration) {
        let wait = {
            let mut next = self.next.lock().expect("not poisoned");
            let now = Instant::now();
            let at = (*next).max(now);
            *next = at + spacing;
            at - now
        };
        std::thread::sleep(wait);
    }

    /// A GET: the status and the body. A refusal for going too fast, a failure on Bitget's side or a dropped
    /// connection is tried again a second later.
    fn request(&self, url: &str, spacing: Duration) -> Result<(u16, Vec<u8>), Error> {
        let mut failure = String::new();
        for _ in 0..RETRIES {
            self.pace(spacing);
            // A dropped connection is tried again like a refusal: Bitget drops some when many requests are open.
            let answer = self.agent.get(url).call().and_then(|mut response| {
                let status = response.status().as_u16();
                let body = tickerlight_core::read_capped(response.body_mut().as_reader(), MAX_FILE)?;
                Ok((status, body))
            });
            match answer {
                Ok((status, _)) if status == 429 || status >= 500 => failure = format!("status {status} for {url}"),
                Ok(answer) => return Ok(answer),
                Err(e) => failure = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(Error::Http(failure))
    }

    /// A GET of the market's `endpoint`: the body, or the body Bitget's errors are written in.
    fn get(&self, endpoint: &str, query: &[(&str, String)], spacing: Duration) -> Result<String, Error> {
        let (path, mut all) = self.market.path(endpoint);
        all.extend(query.iter().cloned());
        let mut url = format!("{}{path}", self.base);
        for (i, (key, value)) in all.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{key}={value}"));
        }
        let (status, body) = self.request(&url, spacing)?;
        let body = String::from_utf8_lossy(&body).into_owned();
        if !(200..300).contains(&status) && !body.starts_with('{') {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        Ok(body)
    }

    /// Every symbol that traded in the last day, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let mut all = parse::tickers(&self.get("tickers", &[], CANDLES_SPACING)?, self.market)?;
        all.retain(|t| t.quote_volume > 0.0);
        all.sort_by(|a, b| b.quote_volume.total_cmp(&a.quote_volume));
        Ok(all)
    }

    /// The candles a day (`1440` minutes) or a minute long before `end`, at most a page of them, oldest first.
    fn candle_page(&self, symbol: &str, minutes: i64, end: i64) -> Result<Vec<Candle>, Error> {
        let query = [
            ("symbol", symbol.to_string()),
            ("granularity", self.market.granularity(minutes).to_string()),
            ("endTime", end.to_string()),
            ("limit", PAGE.to_string()),
        ];
        parse::candles(&self.get("history-candles", &query, CANDLES_SPACING)?)
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. Each
    /// request takes about a second, so [`PARALLEL`] go at once.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let span = PAGE * MINUTE;
        let first = from.0 - from.0.rem_euclid(MINUTE);
        let starts: Vec<i64> = (first..to.0).step_by(span as usize).collect();
        let mut out: Vec<Candle> = Vec::new();
        for starts in starts.chunks(PARALLEL) {
            let pages: Vec<Result<Vec<Candle>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = starts
                    .iter()
                    .map(|&s| scope.spawn(move || self.candle_page(symbol, 1, (s + span).min(to.0))))
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
        let now = now_ms();
        let mut start = since.0 - since.0.rem_euclid(DAY);
        while start <= now {
            let end = start + PAGE * DAY;
            let page = self.candle_page(symbol, 1440, end.min(now + DAY))?;
            if let Some(first) = page.iter().find(|c| c.time.0 >= start) {
                return Ok(Some(first.time));
            }
            start = end;
        }
        Ok(None)
    }

    /// Every trade of `symbol` in `[from, to)` (a week at most, of the last 90 days), oldest first: pages of the
    /// newest thousand, each before the oldest of the one before.
    pub fn trades_since(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<IdTrade>, Error> {
        let mut out: Vec<IdTrade> = Vec::new();
        let mut before: Option<i64> = None;
        loop {
            let mut query = vec![
                ("symbol", symbol.to_string()),
                ("limit", TRADES_PAGE.to_string()),
                ("startTime", from.0.to_string()),
                ("endTime", to.0.to_string()),
            ];
            if let Some(id) = before {
                query.push(("idLessThan", id.to_string()));
            }
            let page = parse::trades(&self.get("fills-history", &query, TRADES_SPACING)?)?;
            let full = page.len() == TRADES_PAGE;
            before = page.first().map(|t| t.id);
            let older = page.first().is_some_and(|t| t.trade.time < from);
            out.extend(page.into_iter().filter(|t| t.trade.time >= from && t.trade.time < to));
            if !full || older {
                break;
            }
        }
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

    fn part_url(&self, symbol: &str, day: Day, part: usize) -> String {
        let Day { year, month, day } = day;
        format!(
            "{}/trades/{}/{symbol}/{symbol}_{year:04}{month:02}{day:02}_{part:03}.zip",
            self.archive,
            self.market.archive_dir()
        )
    }

    /// Every trade of a day of the archive, oldest first, or `None` when it does not have the day.
    pub fn archive_trades(&self, symbol: &str, day: Day) -> Result<Option<Vec<Trade>>, Error> {
        let mut out = Vec::new();
        for part in 1.. {
            match self.file(&self.part_url(symbol, day, part))? {
                Some(csv) => out.extend(parse::archive_trades(&csv)?),
                None if part == 1 => return Ok(None),
                None => break,
            }
        }
        out.sort_by_key(|t| t.time);
        Ok(Some(out))
    }

    /// The text inside a zip of the archive; `None` when there is no such file (it answers 403).
    fn file(&self, url: &str) -> Result<Option<String>, Error> {
        let (status, zip) = self.request(url, Duration::ZERO)?;
        if status == 404 || status == 403 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        unzip_single(&zip, MAX_PART_CSV).map(Some)
    }
}

/// The text of the only file inside a zip, refused when it is over `cap` bytes: the size the zip gives is checked
/// first, and the reading stops past it all the same (the zip may lie).
fn unzip_single(data: &[u8], cap: u64) -> Result<String, Error> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| Error::Parse(e.to_string()))?;
    let file = zip.by_index(0).map_err(|e| Error::Parse(e.to_string()))?;
    if file.size() > cap {
        return Err(Error::Parse(format!(
            "the file in the zip is {} bytes, more than {cap}",
            file.size()
        )));
    }
    tickerlight_core::read_capped_text(file, cap).map_err(|e| Error::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_written_as_bitget_writes_them() {
        assert_eq!(symbol_of("btc-usdt").as_deref(), Some("BTCUSDT"));
        assert_eq!(symbol_of("ETH/BTC").as_deref(), Some("ETHBTC"));
        assert_eq!(symbol_of("sol").as_deref(), Some("SOLUSDT"));
        assert_eq!(symbol_of(" "), None);
    }

    #[test]
    fn a_day_of_the_archive_is_in_numbered_parts() {
        let day = Day {
            year: 2026,
            month: 10,
            day: 6,
        };
        assert_eq!(
            Bitget::new(Market::Futures).part_url("BTCUSDT", day, 2),
            "https://img.bitgetimg.com/online/trades/UMCBL/BTCUSDT/BTCUSDT_20261006_002.zip"
        );
    }
}
