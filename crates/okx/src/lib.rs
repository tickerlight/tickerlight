//! OKX public market data as `tickerlight-core` data: 1-minute candles and the trade tape, from the archive
//! (`static.okx.com`), the REST API and the WebSocket. Only public endpoints, no keys.
//!
//! OKX's archive files are days and months of Beijing time (UTC+8): the file of 2025-01-01 starts at
//! 2024-12-31 16:00 UTC. [`DAY_START`] says so; the caller keeps the files as they are.

pub mod parse;

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::{LiveCandles, LivePrices, LiveTrades};

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub use parse::{Contract, IdTrade};
use tickerlight_core::{Candle, Time, Trade};

pub const DEFAULT_BASE: &str = "https://www.okx.com";
pub const DEFAULT_ARCHIVE_BASE: &str = "https://static.okx.com/cdn/okex/traderecords";
/// Where the days of the archive start, after midnight UTC: 16:00 of the day before (UTC+8).
pub const DAY_START: i64 = -8 * 3_600_000;

/// The biggest archive file we accept: a day of all the trades of a busy swap is tens of megabytes.
const MAX_FILE: u64 = 1 << 30;
/// The biggest CSV accepted out of a candle file: a month of 1m candles is a few megabytes.
const MAX_CANDLES_CSV: u64 = 64 << 20;
/// The biggest CSV accepted out of a trade file: a day of a busy swap's trades is a few hundred megabytes.
const MAX_TRADES_CSV: u64 = 2 << 30;
/// The biggest answer of the API accepted, decompressed: the tickers of every symbol are well under a megabyte.
const MAX_ANSWER: u64 = 1 << 28;
/// Rows a page of the history endpoints gives at most.
const PAGE: usize = 100;
/// How often a request refused for going too fast is tried again, a second apart.
const RETRIES: usize = 10;
/// Pages of trades asked for at once: OKX answers twenty requests in two seconds.
const PARALLEL: usize = 5;

#[derive(Debug)]
pub enum Error {
    /// The request failed or the server answered with an error status.
    Http(String),
    /// OKX answered with an error code.
    Api { code: i64, message: String },
    /// The answer was not what the documentation says.
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { code, message } => write!(f, "OKX error {code}: {message}"),
            Error::Parse(e) => write!(f, "unexpected answer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// OKX does not know the instrument.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Api { code: 51001, .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    Spot,
    /// Perpetual swaps (`BTC-USDT-SWAP`).
    Swap,
}

impl Market {
    /// A short name, safe for file names.
    pub fn name(self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::Swap => "swap",
        }
    }

    fn inst_type(self) -> &'static str {
        match self {
            Market::Spot => "SPOT",
            Market::Swap => "SWAP",
        }
    }

    /// The instrument of a symbol such as `BTC-USDT` in this market.
    pub fn inst_id(self, symbol: &str) -> String {
        match self {
            Market::Spot => symbol.to_string(),
            Market::Swap => format!("{symbol}-SWAP"),
        }
    }
}

/// Currencies symbols are quoted in, the longer of two that end alike first.
const QUOTES: [&str; 9] = ["USDT", "USDC", "USD", "EUR", "BTC", "ETH", "OKB", "TRY", "BRL"];
/// Quotes worth a dollar.
const DOLLARS: [&str; 3] = ["USDT", "USDC", "USD"];

/// A symbol as OKX writes it, `BTC-USDT`, from what someone typed: `btcusdt`, `BTC/USDT`, `BTC-USDT-SWAP`.
/// `None` when nothing is left.
pub fn symbol_of(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_uppercase().replace(['/', '_', ' '], "-");
    let text = text.strip_suffix("-SWAP").unwrap_or(&text);
    let text = text.strip_suffix(".P").unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    if text.contains('-') {
        return Some(text.to_string());
    }
    Some(
        match QUOTES.iter().find(|q| text.len() > q.len() && text.ends_with(*q)) {
            Some(quote) => format!("{}-{quote}", &text[..text.len() - quote.len()]),
            None => text.to_string(),
        },
    )
}

/// The last price of a symbol and how busy it was over a day.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticker {
    /// As [`symbol_of`] writes it, without a swap's suffix.
    pub symbol: String,
    pub last: f64,
    /// The change over the last 24 hours, in percent.
    pub change: f64,
    /// What was traded over the 24 hours, in the quote currency.
    pub quote_volume: f64,
    pub close_time: Time,
}

/// The tickers still trading, the busiest in dollars first.
pub fn busiest(mut all: Vec<Ticker>) -> Vec<Ticker> {
    let newest = all.iter().map(|t| t.close_time.0).max().unwrap_or(0);
    all.retain(|t| t.close_time.0 > newest - 86_400_000 && t.quote_volume > 0.0);
    let prices: HashMap<&str, f64> = all.iter().map(|t| (t.symbol.as_str(), t.last)).collect();
    let rate = |symbol: &str| {
        let quote = symbol.rsplit('-').next().unwrap_or_default();
        if DOLLARS.contains(&quote) {
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

/// A day of the archive (of Beijing time, see [`DAY_START`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

/// The REST API and the archive of one market.
pub struct Okx {
    base: String,
    archive: String,
    market: Market,
    agent: ureq::Agent,
    /// The contract of each swap asked about.
    contracts: Mutex<HashMap<String, Contract>>,
}

impl Okx {
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
            base: base.into().trim_end_matches('/').to_string(),
            archive: archive.into().trim_end_matches('/').to_string(),
            market,
            agent,
            contracts: Mutex::default(),
        }
    }

    pub fn market(&self) -> Market {
        self.market
    }

    /// A GET of the API; a refusal for going too fast is waited out.
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, Error> {
        let mut url = format!("{}{path}", self.base);
        for (i, (key, value)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{key}={value}"));
        }
        for _ in 0..RETRIES {
            let mut response = self.agent.get(&url).call().map_err(|e| Error::Http(e.to_string()))?;
            let status = response.status();
            let body = tickerlight_core::read_capped_text(response.body_mut().as_reader(), MAX_ANSWER)
                .map_err(|e| Error::Http(e.to_string()))?;
            let too_fast = status == 429 || body.contains("\"code\":\"50011\"");
            if too_fast {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            if !status.is_success() && !body.starts_with('{') {
                return Err(Error::Http(format!("status {status} for {url}")));
            }
            return Ok(body);
        }
        Err(Error::Http(format!("too many requests to {path}")))
    }

    /// What a contract of `symbol` is worth (spot: one unit).
    pub fn contract(&self, symbol: &str) -> Result<Contract, Error> {
        if self.market == Market::Spot {
            return Ok(Contract::SPOT);
        }
        if let Some(&c) = self.contracts.lock().expect("not poisoned").get(symbol) {
            return Ok(c);
        }
        let query = [("instType", "SWAP".into()), ("instId", self.market.inst_id(symbol))];
        let contract = parse::contract(&self.get("/api/v5/public/instruments", &query)?)?;
        self.contracts
            .lock()
            .expect("not poisoned")
            .insert(symbol.to_string(), contract);
        Ok(contract)
    }

    /// Every instrument of the market traded in the last day, with its last price and change, the busiest first.
    pub fn tickers(&self) -> Result<Vec<Ticker>, Error> {
        let body = self.get(
            "/api/v5/market/tickers",
            &[("instType", self.market.inst_type().into())],
        )?;
        Ok(busiest(parse::tickers(&body, self.market)?))
    }

    /// The 1-minute candles of `symbol` opening in `[from, to)`, oldest first; the newest may be forming. Pages back
    /// from `to`, a hundred at a time.
    pub fn candles(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Candle>, Error> {
        let inst = self.market.inst_id(symbol);
        let mut out: Vec<Candle> = Vec::new();
        let mut before = to.0;
        while before > from.0 {
            let query = [
                ("instId", inst.clone()),
                ("bar", "1m".into()),
                ("limit", PAGE.to_string()),
                ("after", before.to_string()),
            ];
            let page = parse::candles(&self.get("/api/v5/market/history-candles", &query)?, self.market)?;
            let Some(first) = page.first() else { break };
            before = first.time.0;
            out.extend(page.into_iter().filter(|c| c.time >= from && c.time < to));
        }
        out.sort_by_key(|c| c.time);
        out.dedup_by_key(|c| c.time);
        Ok(out)
    }

    /// The trades of `symbol` after the one with id `after` (or, without it, from `from` on), oldest first, up to
    /// now. Pages back from the newest, a hundred at a time: a busy swap trades tens of thousands an hour.
    pub fn trades_since(&self, symbol: &str, from: Time, after: Option<i64>) -> Result<Vec<IdTrade>, Error> {
        self.trades_between(symbol, from, Time(i64::MAX), after)
    }

    /// The trades of `symbol` in `[from, to)`, oldest first.
    pub fn trades(&self, symbol: &str, from: Time, to: Time) -> Result<Vec<Trade>, Error> {
        Ok(self
            .trades_between(symbol, from, to, None)?
            .into_iter()
            .map(|t| t.trade)
            .collect())
    }

    fn trades_between(&self, symbol: &str, from: Time, to: Time, after: Option<i64>) -> Result<Vec<IdTrade>, Error> {
        let contract = self.contract(symbol)?;
        let inst = self.market.inst_id(symbol);
        // The hundred trades before a cursor: a time (`type` 2) or a trade id (1).
        let page = |cursor: Option<(&str, i64)>| -> Result<Vec<IdTrade>, Error> {
            let mut query = vec![("instId", inst.clone()), ("limit", PAGE.to_string())];
            if let Some((kind, value)) = cursor {
                query.push(("type", kind.into()));
                query.push(("after", value.to_string()));
            }
            parse::trades(&self.get("/api/v5/market/history-trades", &query)?, contract)
        };
        let wanted = |t: &IdTrade| t.trade.time >= from && t.trade.time < to && after.is_none_or(|a| t.id > a);
        let enough = |t: &IdTrade| t.trade.time < from || after.is_some_and(|a| t.id <= a);
        let first = page((to.0 != i64::MAX).then_some(("2", to.0)))?;
        let mut oldest = first.first().copied();
        let mut out: Vec<IdTrade> = first.into_iter().filter(wanted).collect();
        // A busy swap trades tens of thousands an hour, so pages are asked for several at once. The ids follow one
        // another, so the next pages start a hundred apart; where they do not, pages overlap and none is missed.
        while let Some(o) = oldest
            && !enough(&o)
        {
            let pages: Vec<Result<Vec<IdTrade>, Error>> = std::thread::scope(|scope| {
                let asked: Vec<_> = (0..PARALLEL as i64)
                    .map(|k| scope.spawn(move || page(Some(("1", o.id - k * PAGE as i64)))))
                    .collect();
                asked
                    .into_iter()
                    .map(|h| h.join().expect("a page does not panic"))
                    .collect()
            });
            oldest = None;
            for p in pages {
                let p = p?;
                if let Some(&first) = p.first()
                    && oldest.is_none_or(|n: IdTrade| first.id < n.id)
                {
                    oldest = Some(first);
                }
                out.extend(p.into_iter().filter(wanted));
            }
        }
        out.sort_by_key(|t| t.id);
        out.dedup_by_key(|t| t.id);
        Ok(out)
    }

    fn candles_url(&self, symbol: &str, year: i64, month: u32, day: Option<u32>) -> String {
        let inst = self.market.inst_id(symbol);
        match day {
            None => format!(
                "{}/candlesticks/monthly/{year:04}{month:02}/{inst}-candlesticks-{year:04}-{month:02}.zip",
                self.archive
            ),
            Some(day) => format!(
                "{}/candlesticks/daily/{year:04}{month:02}{day:02}/{inst}-candlesticks-{year:04}-{month:02}-{day:02}.zip",
                self.archive
            ),
        }
    }

    fn trades_url(&self, symbol: &str, day: Day) -> String {
        let Day { year, month, day } = day;
        let inst = self.market.inst_id(symbol);
        format!(
            "{}/trades/daily/{year:04}{month:02}{day:02}/{inst}-trades-{year:04}-{month:02}-{day:02}.zip",
            self.archive
        )
    }

    /// Whether the archive has the candles of a month (`day` `None`) or of a day.
    pub fn has_candles(&self, symbol: &str, year: i64, month: u32, day: Option<u32>) -> Result<bool, Error> {
        let url = self.candles_url(symbol, year, month, day);
        let response = self.agent.head(&url).call().map_err(|e| Error::Http(e.to_string()))?;
        match response.status().as_u16() {
            200..=299 => Ok(true),
            403 | 404 => Ok(false),
            status => Err(Error::Http(format!("status {status} for {url}"))),
        }
    }

    /// The candles of a month (`day` `None`) or a day of the archive, oldest first, or `None` when it has no
    /// such file.
    pub fn archive_candles(
        &self,
        symbol: &str,
        year: i64,
        month: u32,
        day: Option<u32>,
    ) -> Result<Option<Vec<Candle>>, Error> {
        match self.file(&self.candles_url(symbol, year, month, day), MAX_CANDLES_CSV)? {
            Some(csv) => parse::archive_candles(&csv, self.market).map(Some),
            None => Ok(None),
        }
    }

    /// Every trade of a day of the archive, oldest first, or `None` when it has no such file.
    pub fn archive_trades(&self, symbol: &str, day: Day) -> Result<Option<Vec<Trade>>, Error> {
        let contract = self.contract(symbol)?;
        match self.file(&self.trades_url(symbol, day), MAX_TRADES_CSV)? {
            Some(csv) => parse::archive_trades(&csv, contract).map(Some),
            None => Ok(None),
        }
    }

    /// The text inside a zip of the archive, of at most `cap` bytes; `None` when there is no such file.
    fn file(&self, url: &str, cap: u64) -> Result<Option<String>, Error> {
        let mut response = self.agent.get(url).call().map_err(|e| Error::Http(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 404 || status == 403 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        let zip = tickerlight_core::read_capped(response.body_mut().as_reader(), MAX_FILE)
            .map_err(|e| Error::Http(e.to_string()))?;
        unzip_single(&zip, cap).map(Some)
    }
}

/// The text of the only file inside a zip, refused when it is over `cap` bytes: the size the zip gives is checked
/// first, and the reading stops past it all the same (the zip may lie). Read as bytes: the headers of older files
/// are partly in Chinese, not in UTF-8.
fn unzip_single(data: &[u8], cap: u64) -> Result<String, Error> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| Error::Parse(e.to_string()))?;
    let file = zip.by_index(0).map_err(|e| Error::Parse(e.to_string()))?;
    if file.size() > cap {
        return Err(Error::Parse(format!(
            "the file in the zip is {} bytes, more than {cap}",
            file.size()
        )));
    }
    let bytes = tickerlight_core::read_capped(file, cap).map_err(|e| Error::Parse(e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_written_as_okx_writes_them() {
        assert_eq!(symbol_of(" btcusdt ").as_deref(), Some("BTC-USDT"));
        assert_eq!(symbol_of("BTC/USDT").as_deref(), Some("BTC-USDT"));
        assert_eq!(symbol_of("eth-usdt-swap").as_deref(), Some("ETH-USDT"));
        assert_eq!(symbol_of("BTCUSD").as_deref(), Some("BTC-USD"));
        assert_eq!(symbol_of("ETHBTC").as_deref(), Some("ETH-BTC"));
        assert_eq!(symbol_of("  "), None);
        assert_eq!(Market::Swap.inst_id("BTC-USDT"), "BTC-USDT-SWAP");
    }

    #[test]
    fn the_busiest_in_dollars_come_first() {
        let ticker = |symbol: &str, last: f64, quote_volume: f64| Ticker {
            symbol: symbol.into(),
            last,
            change: 0.0,
            quote_volume,
            close_time: Time(1_000_000_000),
        };
        let all = vec![
            ticker("ETH-BTC", 0.03, 1_000.0),
            ticker("SOL-USDT", 150.0, 1_000_000.0),
            ticker("BTC-USDT", 80_000.0, 2_000_000.0),
            ticker("DEAD-USDT", 1.0, 0.0),
        ];
        let order: Vec<String> = busiest(all).into_iter().map(|t| t.symbol).collect();
        // 1000 BTC is 80 million dollars.
        assert_eq!(order, ["ETH-BTC", "BTC-USDT", "SOL-USDT"]);
    }

    #[test]
    fn archive_files_are_named_by_their_beijing_date() {
        let okx = Okx::new(Market::Swap);
        assert_eq!(
            okx.candles_url("BTC-USDT", 2025, 1, Some(1)),
            "https://static.okx.com/cdn/okex/traderecords/candlesticks/daily/20250101/BTC-USDT-SWAP-candlesticks-2025-01-01.zip"
        );
        assert_eq!(
            okx.trades_url(
                "BTC-USDT",
                Day {
                    year: 2026,
                    month: 10,
                    day: 17
                }
            ),
            "https://static.okx.com/cdn/okex/traderecords/trades/daily/20261017/BTC-USDT-SWAP-trades-2026-10-17.zip"
        );
    }
}
