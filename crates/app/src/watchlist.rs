//! The watchlist page of the side column: symbols kept to come back to, with their last price and change over a
//! day, and a search over every symbol of the market chosen (Binance's or OKX's).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use tickerlight_binance::{Binance, Market, Ticker};
use tickerlight_core::{Align, Chart};
use tickerlight_ui::panels::price;
use tickerlight_ui::{Cell, Column, Sort, Ui};

use crate::program::Program;
use crate::store::{Store, Venue};

/// How long the prices of a market are shown before they are asked for again.
const FRESH: Duration = Duration::from_secs(60);
/// Rows a search shows at most.
const MATCHES: usize = 100;
/// What a first start watches.
const FIRST: [&str; 6] = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "DOGEUSDT"];

const COLUMNS: [Column; 3] = [
    Column {
        title: "Symbol",
        width: 2.2,
        align: Align::Left,
    },
    Column {
        title: "Last",
        width: 1.8,
        align: Align::Right,
    },
    Column {
        title: "24h",
        width: 1.3,
        align: Align::Right,
    },
];

/// The headings of a search, by venue.
const SEARCHED: [&str; VENUES] = [
    "Binance spot symbols",
    "Binance futures symbols",
    "OKX spot symbols",
    "OKX futures symbols",
    "Coinbase symbols",
    "KuCoin spot symbols",
    "KuCoin futures symbols",
    "Bitget spot symbols",
    "Bitget futures symbols",
    "Kraken futures symbols",
    "Bitstamp symbols",
    "Bitfinex symbols",
    "Deribit futures symbols",
    "Dukascopy instruments",
    "Imported symbols",
];

const VENUES: usize = Venue::ALL.len();

/// The currencies a pair's name may end in; the longest that fits is taken (TUSD is left out: `XBTUSD` would end
/// in it).
const QUOTES: [&str; 17] = [
    "FDUSD", "USDT", "USDC", "BUSD", "DAI", "USD", "UST", "EUR", "GBP", "TRY", "BRL", "JPY", "AUD", "CHF", "BTC",
    "ETH", "BNB",
];
/// The kinds of dollar: a coin in one is shown in the one it is busiest in on another venue.
const DOLLARS: [&str; 4] = ["USDT", "USD", "USDC", "FDUSD"];

/// The coin and the currency it is priced in of a venue's symbol: `BTCUSDT`, `BTC-USDT`, `XBTUSDTM`,
/// `PF_XBTUSD`, `BTCUST` and `BTC-PERPETUAL` are all Bitcoin in some dollar.
fn pair(venue: Venue, symbol: &str) -> Option<(String, String)> {
    let upper = symbol.trim().to_ascii_uppercase();
    let mut s = upper.strip_suffix("-SWAP").unwrap_or(&upper);
    match venue {
        Venue::Kraken => s = s.strip_prefix("PF_").unwrap_or(s),
        Venue::Kucoin(tickerlight_kucoin::Market::Futures) => s = s.strip_suffix('M').unwrap_or(s),
        // The inverse contracts are in dollars, the linear ones say their currency.
        Venue::Deribit => {
            let rest = s.strip_suffix("-PERPETUAL")?;
            s = rest;
            if !rest.contains('_') {
                return Some((rest.to_string(), "USD".to_string()));
            }
        }
        _ => {}
    }
    let (base, quote) = match s.split_once(['-', '_', '/', ':']) {
        Some(split) => split,
        None => {
            let quote = QUOTES
                .iter()
                .filter(|q| s.len() > q.len() && s.ends_with(*q))
                .max_by_key(|q| q.len())?;
            s.split_at(s.len() - quote.len())
        }
    };
    // Kraken and KuCoin write Bitcoin XBT; Bitfinex writes Tether UST.
    let coin = |c: &str| match c {
        "XBT" => "BTC".to_string(),
        "UST" => "USDT".to_string(),
        c => c.to_string(),
    };
    (!base.is_empty() && !quote.is_empty()).then(|| (coin(base), coin(quote)))
}

/// What another venue has of a symbol shown on one.
#[derive(Debug, PartialEq, Eq)]
pub enum Counterpart {
    /// Its list of symbols has not come yet.
    Waiting,
    Found(String),
    /// It has nothing of that coin in that currency or in a dollar: the coin, to look for there.
    Missing(String),
}

/// A symbol as the list shows it: Binance's and Bitget's futures with ".P", as perpetual contracts are written
/// elsewhere too; OKX's, Coinbase's, KuCoin's, Kraken's (whose symbols have a dash, an M or a PF_), Bitstamp's,
/// Bitfinex's and Deribit's as they write them, OKX's swaps with "-SWAP"; an imported one with "(file)".
pub fn display(venue: Venue, symbol: &str) -> String {
    match venue {
        Venue::Binance(Market::Spot)
        | Venue::Okx(tickerlight_okx::Market::Spot)
        | Venue::Coinbase
        | Venue::Kucoin(_)
        | Venue::Bitget(tickerlight_bitget::Market::Spot)
        | Venue::Kraken
        | Venue::Bitstamp
        | Venue::Bitfinex
        | Venue::Deribit
        | Venue::Dukascopy => symbol.to_string(),
        Venue::Binance(Market::UsdFutures) | Venue::Bitget(tickerlight_bitget::Market::Futures) => {
            format!("{symbol}.P")
        }
        Venue::Okx(tickerlight_okx::Market::Swap) => format!("{symbol}-SWAP"),
        Venue::Imported => format!("{symbol} (file)"),
    }
}

/// The prices of an exchange's market, the busiest first, as Binance's tickers; Dukascopy's instruments without
/// prices (`root` is the data folder that keeps their list).
fn tickers(venue: Venue, root: &Path) -> Result<Vec<Ticker>, String> {
    match venue {
        Venue::Binance(market) => Binance::for_market(market).tickers().map_err(|e| e.to_string()),
        Venue::Okx(market) => tickerlight_okx::Okx::new(market)
            .tickers()
            .map(|all| all.into_iter().map(okx_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Coinbase => tickerlight_coinbase::Coinbase::new()
            .tickers()
            .map(|all| all.into_iter().map(coinbase_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Kucoin(market) => tickerlight_kucoin::Kucoin::new(market)
            .tickers()
            .map(|all| all.into_iter().map(kucoin_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Bitget(market) => tickerlight_bitget::Bitget::new(market)
            .tickers()
            .map(|all| all.into_iter().map(bitget_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Kraken => tickerlight_kraken::Kraken::new()
            .tickers()
            .map(|all| all.into_iter().map(kraken_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Bitstamp => tickerlight_bitstamp::Bitstamp::new()
            .tickers()
            .map(|all| all.into_iter().map(bitstamp_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Bitfinex => tickerlight_bitfinex::Bitfinex::new()
            .tickers()
            .map(|all| all.into_iter().map(bitfinex_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Deribit => tickerlight_deribit::Deribit::new()
            .tickers()
            .map(|all| all.into_iter().map(deribit_ticker).collect())
            .map_err(|e| e.to_string()),
        Venue::Dukascopy => crate::dukascopy::tickers(root),
        Venue::Imported => Ok(Vec::new()),
    }
}

fn okx_ticker(t: tickerlight_okx::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn coinbase_ticker(t: tickerlight_coinbase::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn kucoin_ticker(t: tickerlight_kucoin::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn bitget_ticker(t: tickerlight_bitget::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn kraken_ticker(t: tickerlight_kraken::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn bitstamp_ticker(t: tickerlight_bitstamp::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn bitfinex_ticker(t: tickerlight_bitfinex::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

fn deribit_ticker(t: tickerlight_deribit::Ticker) -> Ticker {
    Ticker {
        symbol: t.symbol,
        last: t.last,
        change: t.change,
        quote_volume: t.quote_volume,
        close_time: t.close_time,
    }
}

/// The prices of a few symbols, live from their exchange.
enum LivePrices {
    Binance(tickerlight_binance::LivePrices),
    Okx(tickerlight_okx::LivePrices),
    Coinbase(tickerlight_coinbase::LivePrices),
    Kucoin(tickerlight_kucoin::LivePrices),
    Bitget(tickerlight_bitget::LivePrices),
    Kraken(tickerlight_kraken::LivePrices),
    Bitstamp(tickerlight_bitstamp::LivePrices),
    Bitfinex(tickerlight_bitfinex::LivePrices),
    Deribit(tickerlight_deribit::LivePrices),
}

impl LivePrices {
    fn connect(venue: Venue, symbols: &[String]) -> Option<Self> {
        match venue {
            Venue::Binance(market) => Some(Self::Binance(tickerlight_binance::LivePrices::connect(market, symbols))),
            Venue::Okx(market) => Some(Self::Okx(tickerlight_okx::LivePrices::connect(market, symbols))),
            Venue::Coinbase => Some(Self::Coinbase(tickerlight_coinbase::LivePrices::connect(symbols))),
            Venue::Kucoin(market) => Some(Self::Kucoin(tickerlight_kucoin::LivePrices::connect(market, symbols))),
            Venue::Bitget(market) => Some(Self::Bitget(tickerlight_bitget::LivePrices::connect(market, symbols))),
            Venue::Kraken => Some(Self::Kraken(tickerlight_kraken::LivePrices::connect(symbols))),
            Venue::Bitstamp => Some(Self::Bitstamp(tickerlight_bitstamp::LivePrices::connect(symbols))),
            Venue::Bitfinex => Some(Self::Bitfinex(tickerlight_bitfinex::LivePrices::connect(symbols))),
            Venue::Deribit => Some(Self::Deribit(tickerlight_deribit::LivePrices::connect(symbols))),
            Venue::Dukascopy | Venue::Imported => None,
        }
    }

    fn take(&mut self) -> Vec<Ticker> {
        match self {
            Self::Binance(live) => live.take(),
            Self::Okx(live) => live.take().into_iter().map(okx_ticker).collect(),
            Self::Coinbase(live) => live.take().into_iter().map(coinbase_ticker).collect(),
            Self::Kucoin(live) => live.take().into_iter().map(kucoin_ticker).collect(),
            Self::Bitget(live) => live.take().into_iter().map(bitget_ticker).collect(),
            Self::Kraken(live) => live.take().into_iter().map(kraken_ticker).collect(),
            Self::Bitstamp(live) => live.take().into_iter().map(bitstamp_ticker).collect(),
            Self::Bitfinex(live) => live.take().into_iter().map(bitfinex_ticker).collect(),
            Self::Deribit(live) => live.take().into_iter().map(deribit_ticker).collect(),
        }
    }
}

type Fetch = Receiver<Result<Vec<Ticker>, String>>;

pub struct Watchlist {
    pub search: String,
    /// The match of the search that Enter opens, highlighted; Up and Down move it.
    pub pick: usize,
    pub items: Vec<(Venue, String)>,
    /// How the list is shown sorted; as it was put together when `None`.
    pub sort: Option<Sort>,
    /// The last tickers of each venue (as [`Venue::ALL`]; imported symbols have none), the busiest first, and
    /// when they came.
    tickers: [Vec<Ticker>; VENUES],
    fetched: [Option<Instant>; VENUES],
    /// Why the prices of a market are missing.
    note: [Option<String>; VENUES],
    /// The symbols of each venue with candles kept here: what the list offers when no tickers come.
    known: [Vec<String>; VENUES],
    /// The prices asked for, by market.
    loading: Vec<(usize, Fetch)>,
    /// The prices of the listed symbols, live from each of their markets while the list is shown.
    live: Vec<(usize, Vec<String>, LivePrices)>,
    path: PathBuf,
    saved: String,
}

impl Watchlist {
    /// The watchlist kept in the data folder `root`; on a first start a few of the busiest symbols.
    pub fn load(root: &Path) -> Self {
        let path = root.join("watchlist.txt");
        let (items, saved) = match std::fs::read_to_string(&path) {
            Ok(text) => (parse(&text), text),
            Err(_) => (
                FIRST
                    .iter()
                    .map(|s| (Venue::Binance(Market::Spot), s.to_string()))
                    .collect(),
                String::new(),
            ),
        };
        Self {
            search: String::new(),
            pick: 0,
            items,
            sort: None,
            tickers: Default::default(),
            fetched: [None; VENUES],
            note: Default::default(),
            known: Venue::ALL.map(|v| Store::new(root).symbols(v)),
            loading: Vec::new(),
            live: Vec::new(),
            path,
            saved,
        }
    }

    fn text(&self) -> String {
        self.items
            .iter()
            .map(|(venue, symbol)| format!("{} {symbol}\n", venue.name()))
            .collect()
    }

    /// Writes the list if it changed since it was read or written.
    pub fn save(&mut self) -> std::io::Result<()> {
        let text = self.text();
        if text == self.saved {
            return Ok(());
        }
        // Not tried again at every event if it fails.
        self.saved = text.clone();
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, text)
    }

    pub fn contains(&self, venue: Venue, symbol: &str) -> bool {
        self.items.iter().any(|(v, s)| *v == venue && s == symbol)
    }

    pub fn toggle(&mut self, venue: Venue, symbol: &str) {
        if self.contains(venue, symbol) {
            self.items.retain(|(v, s)| !(*v == venue && s == symbol));
        } else {
            self.items.push((venue, symbol.to_string()));
        }
    }

    /// Notes a symbol that has candles here now, such as one just imported.
    pub fn know(&mut self, venue: Venue, symbol: &str) {
        let known = &mut self.known[venue.index()];
        if let Err(at) = known.binary_search_by(|s| s.as_str().cmp(symbol)) {
            known.insert(at, symbol.to_string());
        }
    }

    /// Asks for the prices of the venue's market in the background if they are old (an imported symbol has none).
    pub fn refresh(&mut self, venue: Venue) {
        let i = venue.index();
        let stale = self.fetched[i].is_none_or(|t| t.elapsed() > FRESH);
        if self.loading.iter().any(|(m, _)| *m == i) || !stale || venue == Venue::Imported {
            return;
        }
        let (send, receive) = mpsc::channel();
        let root = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        std::thread::spawn(move || {
            let _ = send.send(tickers(venue, &root));
        });
        self.loading.push((i, receive));
    }

    /// Asks for the prices of every market the list has symbols of, if they are old.
    pub fn refresh_listed(&mut self) {
        for venue in Venue::ALL {
            if self.items.iter().any(|(v, _)| *v == venue) {
                self.refresh(venue);
            }
        }
    }

    pub fn loading(&self) -> bool {
        !self.loading.is_empty()
    }

    /// Takes the prices that came. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut k = 0;
        while let Some((market, receive)) = self.loading.get(k) {
            let market = *market;
            let result = match receive.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    k += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err("the request stopped".into()),
            };
            self.loading.swap_remove(k);
            self.fetched[market] = Some(Instant::now());
            match result {
                Ok(tickers) => (self.tickers[market], self.note[market]) = (tickers, None),
                Err(e) if e.contains("restricted location") => {
                    let venue = Venue::ALL[market].label();
                    self.note[market] = Some(format!("No prices: the {venue} API is closed to this location"));
                }
                Err(e) => self.note[market] = Some(format!("No prices: {}", Venue::ALL[market].plain(&e))),
            }
            changed = true;
        }
        changed
    }

    /// Keeps the prices of the listed symbols coming live from their markets, or, with `false`, stops them.
    pub fn follow(&mut self, shown: bool) {
        if !shown {
            self.live.clear();
            return;
        }
        // Dukascopy's and the imported have no live prices.
        for venue in Venue::ALL
            .into_iter()
            .filter(|v| !matches!(v, Venue::Dukascopy | Venue::Imported))
        {
            let i = venue.index();
            let items = &self.items;
            let of = || items.iter().filter(|(v, _)| *v == venue).map(|(_, s)| s);
            let at = self.live.iter().position(|(m, _, _)| *m == i);
            let same = match at {
                Some(at) => self.live[at].1.iter().eq(of()),
                None => of().next().is_none(),
            };
            if same {
                continue;
            }
            if let Some(at) = at {
                self.live.swap_remove(at);
            }
            let symbols: Vec<String> = of().cloned().collect();
            if !symbols.is_empty()
                && let Some(live) = LivePrices::connect(venue, &symbols)
            {
                self.live.push((i, symbols, live));
            }
        }
    }

    pub fn live(&self) -> bool {
        !self.live.is_empty()
    }

    /// Takes the live prices that came. Returns whether any did.
    pub fn take_live(&mut self) -> bool {
        let mut came_any = false;
        for (market, _, live) in &mut self.live {
            let came = live.take();
            merge(&mut self.tickers[*market], &came);
            came_any |= !came.is_empty();
        }
        came_any
    }

    /// Puts tickers in as if they had come from the venue.
    pub fn set_tickers(&mut self, venue: Venue, tickers: Vec<Ticker>) {
        let i = venue.index();
        (self.tickers[i], self.fetched[i], self.note[i]) = (tickers, Some(Instant::now()), None);
    }

    fn ticker(&self, venue: Venue, symbol: &str) -> Option<&Ticker> {
        self.tickers[venue.index()].iter().find(|t| t.symbol == symbol)
    }

    /// The symbols of a venue (by its index) with `search` in them: the one typed first, then those starting with
    /// it, then the rest, each the busiest first. The typed symbol itself if there is no list. Imported symbols
    /// are those imported that have it: what is typed may be the path of a file. Dashes do not count: `BTCUSDT`
    /// finds OKX's `BTC-USDT`.
    pub(crate) fn matches(&self, venue: usize, search: &str) -> Vec<(Venue, String)> {
        let squash = |s: &str| s.replace('-', "");
        let query = squash(&search.trim().to_uppercase());
        let list = &self.tickers[venue];
        let kept = self.known[venue]
            .iter()
            .filter(|s| squash(s).contains(&query) && squash(s) != query);
        if Venue::ALL[venue] == Venue::Imported {
            let typed = self.known[venue].iter().filter(|s| squash(s) == query);
            return typed
                .chain(kept)
                .take(MATCHES)
                .map(|s| (Venue::Imported, s.clone()))
                .collect();
        }
        if list.is_empty() {
            // What was typed, then the symbols kept here that have it.
            let typed = Venue::ALL[venue].symbol_of(search);
            return typed
                .into_iter()
                .chain(kept.cloned())
                .take(MATCHES)
                .map(|s| (Venue::ALL[venue], s))
                .collect();
        }
        let mut found: Vec<(&Ticker, String)> = list
            .iter()
            .map(|t| (t, squash(&t.symbol)))
            .filter(|(_, s)| s.contains(&query))
            .collect();
        found.sort_by_key(|(_, s)| (*s != query, !s.starts_with(&query)));
        found
            .into_iter()
            .take(MATCHES)
            .map(|(t, _)| (Venue::ALL[venue], t.symbol.clone()))
            .collect()
    }

    /// Moves the highlight through the matches of the search by `step`, staying within them.
    pub fn step(&mut self, venue: usize, step: i32) {
        let last = self.matches(venue, &self.search).len().saturating_sub(1);
        self.pick = self.pick.saturating_add_signed(step as isize).min(last);
    }

    /// The highlighted match of the search, if there is a search and a match.
    pub fn picked(&self, venue: usize) -> Option<(Venue, String)> {
        if self.search.trim().is_empty() {
            return None;
        }
        let matches = self.matches(venue, &self.search);
        let at = self.pick.min(matches.len().checked_sub(1)?);
        matches.into_iter().nth(at)
    }

    /// The symbol of `to` that is the pair `symbol` of `from` is: the same coin in the same currency, or, priced
    /// in a dollar, in the kind of dollar busiest there. Without a list of `to` (none came, or `offline`), the
    /// symbol it would be.
    pub fn counterpart(&self, from: Venue, symbol: &str, to: Venue, offline: bool) -> Counterpart {
        let i = to.index();
        if !offline && self.fetched[i].is_none() {
            return Counterpart::Waiting;
        }
        let Some((base, quote)) = pair(from, symbol) else {
            return match to.symbol_of(symbol) {
                Some(found) => Counterpart::Found(found),
                None => Counterpart::Missing(symbol.to_string()),
            };
        };
        let dollars = DOLLARS.contains(&quote.as_str());
        let quotes = std::iter::once(quote.as_str()).chain(DOLLARS.into_iter().filter(|&q| dollars && q != quote));
        let candidates: Vec<String> = quotes.filter_map(|q| to.symbol_of(&format!("{base}/{q}"))).collect();
        // The busiest of them in the venue's list, else one kept here: a coin is traded mostly in one dollar
        // there, and that one is shown.
        let rank = |s: &&String| {
            let busy = self.tickers[i].iter().position(|t| t.symbol == **s);
            busy.or_else(|| self.known[i].contains(*s).then_some(usize::MAX))
        };
        let found = candidates.iter().filter(|s| rank(s).is_some()).min_by_key(rank);
        // Without the venue's list only those kept here are known to be there, else the first one is tried.
        let found = match found {
            Some(found) => Some(found.clone()),
            None if self.tickers[i].is_empty() => candidates.into_iter().next(),
            None => None,
        };
        match found {
            Some(found) => Counterpart::Found(found),
            None => Counterpart::Missing(base),
        }
    }
}

/// Puts live prices into the list: the price and change of a symbol it has, the busiest first as it is; a symbol
/// it does not have (no list came) at the end.
fn merge(list: &mut Vec<Ticker>, came: &[Ticker]) {
    for ticker in came {
        match list.iter_mut().find(|t| t.symbol == ticker.symbol) {
            Some(t) => (t.last, t.change) = (ticker.last, ticker.change),
            None => list.push(ticker.clone()),
        }
    }
}

/// The lines `spot BTCUSDT`, `usd-futures ETHUSDT`, `imported ACME`; others are left out.
fn parse(text: &str) -> Vec<(Venue, String)> {
    text.lines()
        .filter_map(|line| {
            let (venue, symbol) = line.trim().split_once(' ')?;
            let venue = Venue::from_name(venue)?;
            let symbol = symbol.trim();
            (!symbol.is_empty()).then(|| (venue, symbol.to_ascii_uppercase()))
        })
        .collect()
}

/// Orders `rows` by a column of the list: the name, the last price or the change; those with no price last.
fn sort_rows<'a>(rows: &mut [(Venue, String)], sort: Sort, ticker: impl Fn(Venue, &str) -> Option<&'a Ticker>) {
    let key = |(m, symbol): &(Venue, String)| match sort.column {
        1 => ticker(*m, symbol).map(|t| t.last).filter(|v| v.is_finite()),
        2 => ticker(*m, symbol).map(|t| t.change).filter(|v| v.is_finite()),
        _ => None,
    };
    rows.sort_by(|a, b| {
        let order = if sort.column == 0 {
            display(a.0, &a.1).cmp(&display(b.0, &b.1))
        } else {
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => return std::cmp::Ordering::Less,
                (None, Some(_)) => return std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        };
        if sort.descending { order.reverse() } else { order }
    });
}

pub fn watchlist_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    let (up, down, muted) = (ui.theme().up, ui.theme().down, ui.theme().muted);
    let venue = p.venue;
    let w = &mut p.watch;
    ui.section("Watchlist");
    let field = ui.text_field(&mut w.search, 0.0, "Search symbols");
    ui.tip("Any part of a symbol, like ETH or USDC; Up and Down pick, Enter opens");
    if field.changed {
        w.pick = 0;
    }
    if field.step != 0 {
        w.step(venue, field.step);
    }
    if field.submitted
        && let Some((m, symbol)) = w.picked(venue)
    {
        p.open(m, symbol);
        return;
    }
    if let Some(shown) = &p.shown {
        let watched = w.contains(shown.venue, &shown.symbol);
        let name = display(shown.venue, &shown.symbol);
        let text = if watched {
            format!("Remove {name}")
        } else {
            format!("Add {name}")
        };
        if ui.button(&text) {
            w.toggle(shown.venue, &shown.symbol);
        }
    }
    if let Some(note) = &w.note[venue] {
        ui.label_colored(note, muted);
    }
    let searching = !w.search.trim().is_empty();
    let listed = !searching && !w.items.is_empty();
    let mut rows: Vec<(Venue, String)> = if searching {
        ui.heading(SEARCHED[venue]);
        w.matches(venue, &w.search)
    } else if w.items.is_empty() && w.tickers[venue].is_empty() {
        ui.heading("Symbols with data here");
        w.known[venue].iter().map(|s| (Venue::ALL[venue], s.clone())).collect()
    } else if w.items.is_empty() {
        ui.heading(if Venue::ALL[venue] == Venue::Dukascopy {
            "Popular instruments"
        } else {
            "The busiest symbols"
        });
        w.tickers[venue]
            .iter()
            .take(MATCHES)
            .map(|t| (Venue::ALL[venue], t.symbol.clone()))
            .collect()
    } else {
        w.items.clone()
    };
    // The matches of a search keep their order: the one typed first, Enter opens the highlighted one.
    if let Some(sort) = w.sort
        && !searching
    {
        sort_rows(&mut rows, sort, |m, symbol| w.ticker(m, symbol));
    }
    let shown = if searching {
        (!rows.is_empty()).then(|| w.pick.min(rows.len() - 1))
    } else {
        p.shown
            .as_ref()
            .and_then(|s| rows.iter().position(|(v, sym)| *v == s.venue && *sym == s.symbol))
    };
    let mut sort = w.sort;
    let response = ui.table_with(&COLUMNS, rows.len(), shown, Some(&mut sort), listed, |i| {
        let (m, symbol) = &rows[i];
        let name = display(*m, symbol);
        match w.ticker(*m, symbol) {
            Some(t) if t.last.is_finite() => vec![
                Cell::new(name),
                Cell::new(price(t.last)),
                Cell::colored(format!("{:+.2}%", t.change), if t.change >= 0.0 { up } else { down }),
            ],
            _ => vec![Cell::new(name), Cell::new("-"), Cell::new("-")],
        }
    });
    w.sort = sort;
    if let Some(i) = response.removed {
        let (m, symbol) = &rows[i];
        w.toggle(*m, symbol);
    }
    if let Some(i) = response.clicked {
        let (m, symbol) = rows[i].clone();
        p.open(m, symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::Time;

    const SPOT: Venue = Venue::Binance(Market::Spot);
    const FUTURES: Venue = Venue::Binance(Market::UsdFutures);

    fn ticker(symbol: &str, last: f64) -> Ticker {
        Ticker {
            symbol: symbol.into(),
            last,
            change: 1.0,
            quote_volume: 1.0,
            close_time: Time(0),
        }
    }

    #[test]
    fn the_list_is_kept_as_text() {
        let dir = std::env::temp_dir().join(format!("chart-watchlist-{}", std::process::id()));
        let mut w = Watchlist::load(&dir);
        assert_eq!(w.items.len(), FIRST.len(), "a first start watches a few");
        w.toggle(SPOT, "BTCUSDT");
        w.toggle(FUTURES, "ETHUSDT");
        w.toggle(Venue::Imported, "ACME");
        w.save().unwrap();
        let read = Watchlist::load(&dir);
        assert!(!read.contains(SPOT, "BTCUSDT"));
        assert!(read.contains(FUTURES, "ETHUSDT"));
        assert!(read.contains(Venue::Imported, "ACME"));
        assert_eq!(read.items, w.items);
        assert_eq!(
            parse("spot btcusdt\nnowhere X\nfutures \nimported acme\n"),
            [(SPOT, "BTCUSDT".into()), (Venue::Imported, "ACME".into())]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn live_prices_update_the_list_in_place() {
        let mut list = vec![ticker("BTCUSDT", 1.0), ticker("ETHUSDT", 2.0)];
        let mut eth = ticker("ETHUSDT", 3.0);
        eth.change = -2.0;
        merge(&mut list, &[eth, ticker("SOLUSDT", 4.0)]);
        let got: Vec<_> = list.iter().map(|t| (t.symbol.as_str(), t.last, t.change)).collect();
        assert_eq!(
            got,
            [("BTCUSDT", 1.0, 1.0), ("ETHUSDT", 3.0, -2.0), ("SOLUSDT", 4.0, 1.0)]
        );
    }

    #[test]
    fn enter_opens_the_first_match_and_up_and_down_move_within_the_matches() {
        let dir = std::env::temp_dir().join("chart-watchlist-pick");
        let mut w = Watchlist::load(&dir);
        let list = ["BTCUSDT", "SOLUSDT", "SOLBTC", "SOLETH"].map(|s| ticker(s, 1.0));
        w.set_tickers(SPOT, list.to_vec());
        assert_eq!(w.picked(0), None, "nothing typed: the field names the symbol itself");
        w.search = "sol".into();
        assert_eq!(w.picked(0), Some((SPOT, "SOLUSDT".into())));
        w.step(0, -1);
        assert_eq!(w.picked(0), Some((SPOT, "SOLUSDT".into())), "no row above the first");
        for _ in 0..5 {
            w.step(0, 1);
        }
        assert_eq!(w.picked(0), Some((SPOT, "SOLETH".into())), "no row below the last");
        w.step(0, -1);
        assert_eq!(w.picked(0), Some((SPOT, "SOLBTC".into())));
    }

    #[test]
    fn a_search_puts_the_symbol_typed_first() {
        let dir = std::env::temp_dir().join("chart-watchlist-none");
        let mut w = Watchlist::load(&dir);
        assert_eq!(w.matches(0, "eth"), [(SPOT, "ETH".into())], "no list: what was typed");
        let list = ["ETHBTC", "BTCUSDT", "ETHUSDT", "ETH", "WETHUSDT"].map(|s| ticker(s, 1.0));
        w.set_tickers(SPOT, list.to_vec());
        let found: Vec<String> = w.matches(0, "eth").into_iter().map(|m| m.1).collect();
        assert_eq!(found, ["ETH", "ETHBTC", "ETHUSDT", "WETHUSDT"]);
        // OKX writes its symbols with a dash; typed without one, they are found all the same.
        let okx = Venue::Okx(tickerlight_okx::Market::Swap);
        assert_eq!(
            w.matches(okx.index(), "btcusdt"),
            [(okx, "BTC-USDT".into())],
            "no list: as OKX writes it"
        );
        let list = ["ETH-USDT", "BTC-USDC", "BTC-USDT"].map(|s| ticker(s, 1.0));
        w.set_tickers(okx, list.to_vec());
        let found: Vec<String> = w.matches(okx.index(), "BTCUSDT").into_iter().map(|m| m.1).collect();
        assert_eq!(found, ["BTC-USDT"]);
        assert_eq!(display(okx, "BTC-USDT"), "BTC-USDT-SWAP");
    }

    #[test]
    fn another_exchange_shows_the_same_coin_as_it_names_it() {
        let dir = std::env::temp_dir().join(format!("chart-watchlist-carry-{}", std::process::id()));
        let mut w = Watchlist::load(&dir);
        let (okx, coinbase, kraken) = (
            Venue::Okx(tickerlight_okx::Market::Spot),
            Venue::Coinbase,
            Venue::Kraken,
        );
        let kucoin = Venue::Kucoin(tickerlight_kucoin::Market::Futures);
        assert_eq!(
            w.counterpart(SPOT, "BTCUSDT", okx, false),
            Counterpart::Waiting,
            "its list has not come"
        );
        w.set_tickers(
            okx,
            ["BTC-USDT", "ETH-USDT", "BTC-USDC"].map(|s| ticker(s, 1.0)).to_vec(),
        );
        w.set_tickers(
            coinbase,
            ["BTC-USD", "BTC-USDT", "ETH-EUR"].map(|s| ticker(s, 1.0)).to_vec(),
        );
        w.set_tickers(kraken, ["PF_XBTUSD", "PF_ETHUSD"].map(|s| ticker(s, 1.0)).to_vec());
        w.set_tickers(kucoin, ["XBTUSDTM", "ETHUSDTM"].map(|s| ticker(s, 1.0)).to_vec());
        w.set_tickers(SPOT, ["BTCUSDT", "BTCUSDC", "ETHUSDT"].map(|s| ticker(s, 1.0)).to_vec());
        let found = |from, symbol, to| w.counterpart(from, symbol, to, false);
        assert_eq!(found(SPOT, "BTCUSDT", okx), Counterpart::Found("BTC-USDT".into()));
        assert_eq!(
            found(SPOT, "BTCUSDT", coinbase),
            Counterpart::Found("BTC-USD".into()),
            "the dollar it is busiest in there"
        );
        assert_eq!(found(okx, "BTC-USDT", kraken), Counterpart::Found("PF_XBTUSD".into()));
        assert_eq!(
            found(kraken, "PF_XBTUSD", kucoin),
            Counterpart::Found("XBTUSDTM".into())
        );
        assert_eq!(found(kucoin, "ETHUSDTM", SPOT), Counterpart::Found("ETHUSDT".into()));
        assert_eq!(
            found(Venue::Deribit, "BTC-PERPETUAL", SPOT),
            Counterpart::Found("BTCUSDT".into())
        );
        // Not priced in a dollar, the coin is not looked for in one.
        assert_eq!(found(coinbase, "ETH-EUR", SPOT), Counterpart::Missing("ETH".into()));
        assert_eq!(found(SPOT, "SOLUSDT", okx), Counterpart::Missing("SOL".into()));
    }

    #[test]
    fn with_no_prices_the_symbols_kept_here_are_offered() {
        let dir = std::env::temp_dir().join(format!("chart-watchlist-kept-{}", std::process::id()));
        for symbol in ["SOLUSDT", "ETHUSDT", "ETHBTC"] {
            std::fs::create_dir_all(dir.join("binance/usd-futures").join(symbol).join("1m")).unwrap();
        }
        std::fs::create_dir_all(dir.join("imported/ETHX/1d")).unwrap();
        let mut w = Watchlist::load(&dir);
        assert_eq!(
            w.known,
            [
                vec![],
                vec!["ETHBTC".to_string(), "ETHUSDT".into(), "SOLUSDT".into()],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec!["ETHX".to_string()]
            ]
        );
        let found: Vec<String> = w.matches(1, "eth").into_iter().map(|m| m.1).collect();
        assert_eq!(found, ["ETH", "ETHBTC", "ETHUSDT"]);
        w.know(Venue::Imported, "ACME");
        w.know(Venue::Imported, "ETHX");
        let imported = Venue::Imported.index();
        assert_eq!(w.known[imported], ["ACME", "ETHX"]);
        assert_eq!(
            w.matches(imported, "/home/me/eth.csv"),
            [],
            "a path typed is not a symbol"
        );
        assert_eq!(w.matches(imported, "eth"), [(Venue::Imported, "ETHX".into())]);
        assert_eq!(display(Venue::Imported, "ETHX"), "ETHX (file)");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
