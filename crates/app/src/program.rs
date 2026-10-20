//! The program's window: the chart with its panels, and the symbol being loaded in the background.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tickerlight_binance::{Archive, Binance, Interval, Market, Period};
use tickerlight_bitfinex::Bitfinex;
use tickerlight_bitget::Bitget;
use tickerlight_bitstamp::Bitstamp;
use tickerlight_coinbase::Coinbase;
use tickerlight_core::Candle;
use tickerlight_core::builtin::{
    Account, Clock, Drawing, Drawings, Keymap, LearnedDirection, Legend, Live, MovingAverageCross, NoReadouts,
    OpenStudySettings, Replay, RsiReversal, SeriesTitle, Strategy, Studies, Study, VolumeLayer, set_studies, studies,
};
use tickerlight_core::calendar::{DateTime, Zone};
use tickerlight_core::plugin::{names, order};
use tickerlight_core::{
    Candles, Chart, Event, Footprint, Key, Scene, Surface, Time, Trade, Trades, VolumeProfile, native_step, span_label,
};
use tickerlight_deribit::Deribit;
use tickerlight_dukascopy::Dukascopy;
use tickerlight_kraken::Kraken;
use tickerlight_kucoin::Kucoin;
use tickerlight_okx::Okx;
use tickerlight_ui::panels::{
    ContextMenuPanel, DrawToolsPanel, DrawingStylePanel, GO_TO, Journal, JournalColumns, OrderPanel, PositionPanel,
    RangePanel, ReplayButton, ReplayPanel, ReplayPlayer, Review, ScalePanel, StatsPanel, Tape, TimeframePanel,
    ToolsPanel, TradesPanel,
};
use tickerlight_ui::{
    Dock, Icon, IndicatorsPanel, Layout, Panel, SEARCH_INDICATORS, StrategyFiles, StrategyPanel, Ui, Workspace,
};

use crate::flow::Chunk;
use crate::session::{Resume, Resumed, Session, Sessions};
use crate::sessions_page::{OnPage, Page, sessions_panel};
use crate::snapshots::Snapshots;
use crate::store::{Series, Store, Venue};
use crate::watchlist::{Counterpart, Watchlist, display, watchlist_panel};
use crate::{flow, import, sync};

/// The symbol of the first start.
pub const DEFAULT_SYMBOL: &str = "BTCUSDT";
/// Fees of a market order at the base rate, the same on Binance, OKX, KuCoin and Bitget (and Kraken's futures):
/// what a trade costs unless the user's account pays less.
const SPOT_FEE: f64 = 0.001;
const FUTURES_FEE: f64 = 0.0005;
/// KuCoin's and Bitget's futures taker fee at the base rate.
const KUCOIN_BITGET_FUTURES_FEE: f64 = 0.0006;
/// Coinbase Exchange's taker fee in its first tier (up to $10k a month).
const COINBASE_FEE: f64 = 0.006;
/// Bitstamp's taker fee in its first tier (up to $10k a month).
const BITSTAMP_FEE: f64 = 0.004;
/// Bitfinex's taker fee in its first tier.
const BITFINEX_FEE: f64 = 0.002;
/// Deribit's taker fee on its perpetuals.
const DERIBIT_FEE: f64 = 0.00035;
/// How often a session that keeps changing (a replay playing) is saved.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// The layers of the order flow (the names `Footprint` and `VolumeProfile` give them).
const FOOTPRINT: &str = "footprint";
const PROFILE: &str = "volume-profile";
/// How much wider than the narrowest bars the footprint draws in are made when it is switched on.
const FOOTPRINT_ZOOM: f32 = 1.5;
/// The keys the chart trades with (buy, sell, size, close, limit, stop, cancel), taken away in the program.
const TRADING_KEYS: [char; 8] = ['b', 's', ']', '[', 'c', 'l', 't', 'x'];

/// Where history comes from and how much of it.
#[derive(Debug, Clone)]
pub struct Source {
    /// The cache folder.
    pub root: PathBuf,
    pub months: u32,
    /// Only what is cached: nothing is downloaded.
    pub offline: bool,
}

pub fn now() -> Time {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    Time(ms)
}

/// The candles of a series: the cache brought up to date from the archive, then today's candles, which are not in
/// the archive yet, from the API. Without today the history is still worth showing: then the note says why. An
/// imported series is what was imported, every period of it.
pub fn load(
    source: &Source,
    series: &Series,
    now: Time,
    progress: impl FnMut(sync::Progress),
) -> Result<(Candles, Option<String>), Box<dyn Error>> {
    let store = Store::new(&source.root);
    if series.venue == Venue::Imported {
        let candles = sync::load(&store, series, &store.periods(series)?)?;
        if candles.is_empty() {
            return Err(format!("{} was not imported: type the path of a CSV file", series.symbol).into());
        }
        return Ok((candles, None));
    }
    let local = sync::local_now(series, now);
    let periods = sync::periods(sync::months_before(local, source.months), local);
    if !source.offline {
        refresh(source, series, now, progress)?;
    }
    let mut candles = sync::load(&store, series, &periods)?;
    let mut note = None;
    let mut unknown = false;
    if !source.offline {
        let from = candles
            .view()
            .time()
            .last()
            .map_or(now.0, |t| t.0 + series.interval.millis());
        let today = recent(source, series, Time(from), now);
        match today {
            Ok(today) => {
                for candle in today {
                    candles.push(candle).expect("times increase");
                }
            }
            Err((why, not_there)) => {
                unknown = not_there;
                note = Some(format!("Without today's candles: {why}"));
            }
        }
    }
    if candles.is_empty() {
        let (symbol, venue) = (&series.symbol, series.venue.label());
        let why = if source.offline {
            format!("{symbol} {venue} is not on this computer")
        } else if unknown {
            format!("There is no {symbol} on {venue}")
        } else {
            format!("No {symbol} {venue} candles in the last {} months", source.months)
        };
        return Err(why.into());
    }
    Ok((candles, note))
}

/// The candles of `series` from its venue's API opening in `[from, now]`; the error says why, and whether the
/// venue has no such symbol.
fn recent(source: &Source, series: &Series, from: Time, now: Time) -> Result<Vec<Candle>, (String, bool)> {
    let to = Time(now.0 + 1);
    match series.venue {
        Venue::Binance(market) => {
            match Binance::for_market(market).klines(&series.symbol, series.interval, from, now) {
                Ok(today) => {
                    let today = today.view();
                    Ok((0..today.len()).filter_map(|i| today.get(i)).collect())
                }
                Err(e) if e.to_string().contains("restricted location") => Err((
                    format!("the {} API is closed to this location", series.venue.label()),
                    false,
                )),
                Err(e) => {
                    let unknown = matches!(e, tickerlight_binance::Error::Api { code: -1121, .. });
                    Err((e.to_string(), unknown))
                }
            }
        }
        Venue::Okx(market) => Okx::new(market)
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Coinbase => Coinbase::new()
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Kucoin(market) => Kucoin::new(market)
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Bitget(market) => Bitget::new(market)
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Kraken => Kraken::new()
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Bitstamp => Bitstamp::new()
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Bitfinex => Bitfinex::new()
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        Venue::Deribit => Deribit::new()
            .candles(&series.symbol, from, to)
            .map_err(|e| (e.to_string(), e.is_unknown())),
        // The hours since the last day out, from their ticks; the list was kept by the update before.
        Venue::Dukascopy => match crate::dukascopy::kept(&source.root, &series.symbol) {
            Some(instrument) => Dukascopy::new()
                .recent(&instrument, from, now)
                .map_err(|e| (e.to_string(), false)),
            None => Ok(Vec::new()),
        },
        Venue::Imported => Ok(Vec::new()),
    }
}

/// How old the newest cached candle may be for a series to show from the cache at once: the live candles bring
/// the rest from the API in a few requests. The cache ends at most two days back, as a day's file comes out the
/// day after it.
const FRESH: i64 = 3 * 86_400_000;

/// How many of the newest candles a symbol never seen before shows while its history downloads: one API page.
const NEWEST_BARS: i64 = 1000;

/// How many pages of candles before those the API brings while the archive is asked which files it has.
const MORE_PAGES: i64 = 9;

/// The cached candles of `series` when they come close enough to `now` for its live candles to bring the rest:
/// the chart can show them at once, while the archive brings the cache up to date behind it.
pub fn cached(source: &Source, series: &Series, now: Time) -> Option<Candles> {
    // The same series the program follows live.
    let live = series.interval == Interval::M1 || matches!(series.venue, Venue::Binance(_));
    if source.offline || series.venue == Venue::Imported || !live {
        return None;
    }
    let local = sync::local_now(series, now);
    let periods = sync::periods(sync::months_before(local, source.months), local);
    let candles = sync::load(&Store::new(&source.root), series, &periods).ok()?;
    let newest = candles.view().time().last().map(|t| t.0)?;
    (now.0 - newest <= FRESH).then_some(candles)
}

/// Brings the cache of `series` up to date from the archive, and from the API for the exchanges whose archive is
/// the API.
fn refresh(
    source: &Source,
    series: &Series,
    now: Time,
    progress: impl FnMut(sync::Progress),
) -> Result<(), Box<dyn Error>> {
    let store = Store::new(&source.root);
    // The archive's periods are named on its own clock.
    let local = sync::local_now(series, now);
    let first = sync::months_before(local, source.months);
    let periods = sync::periods(first, local);
    // Only the months not kept yet are asked about (the last one always: it tells the symbol is there).
    let kept = |p: &Period| store.read(series, *p).is_ok_and(|c| c.is_some());
    let missing = periods
        .iter()
        .find(|p| !kept(p))
        .map_or(first, |p| {
            let t = DateTime::from_time(p.start());
            (t.year, t.month)
        })
        .min(sync::months_before(local, 1));
    let since = Period::Month {
        year: missing.0,
        month: missing.1,
    }
    .start();
    let unknown = || format!("There is no {} on {}", series.symbol, series.venue.label());
    match series.venue {
        Venue::Okx(market) => {
            let okx = sync::OkxArchive {
                okx: Okx::new(market),
                first: missing,
                now: local,
            };
            sync::update(&store, &okx, series, &periods, local, progress)?;
        }
        Venue::Coinbase => {
            let api = Coinbase::new();
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Kucoin(market) => {
            let api = Kucoin::new(market);
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Bitget(market) => {
            let api = Bitget::new(market);
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Kraken => {
            let api = Kraken::new();
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Bitstamp => {
            let api = Bitstamp::new();
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Bitfinex => {
            let api = Bitfinex::new();
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Deribit => {
            let api = Deribit::new();
            let listed = match api.first_day(&series.symbol, since) {
                Err(e) if e.is_unknown() => return Err(unknown().into()),
                listed => listed?,
            };
            let archive = sync::ApiArchive {
                api,
                listed,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        Venue::Dukascopy => {
            let archive = sync::DukascopyArchive {
                api: Dukascopy::new(),
                instrument: crate::dukascopy::instrument(&source.root, &series.symbol)?,
                now: local,
            };
            sync::update(&store, &archive, series, &periods, local, progress)?;
        }
        _ => sync::update(&store, &Archive::new(), series, &periods, local, progress)?,
    }
    Ok(())
}

/// An imported symbol asked for by name opens at the interval it was imported at: the asked one if it has it,
/// else the shortest it has.
fn stored_interval(store: &Store, series: Series) -> Series {
    if series.venue != Venue::Imported || store.periods(&series).is_ok_and(|p| !p.is_empty()) {
        return series;
    }
    match store
        .intervals(series.venue, &series.symbol)
        .ok()
        .and_then(|i| i.first().copied())
    {
        Some(interval) => Series { interval, ..series },
        None => series,
    }
}

/// The symbol of `series` and where it is from: `Binance Spot`, `File`.
fn series_title(series: &Series) -> SeriesTitle {
    let (exchange, market) = series.venue.choices();
    let exchange = Venue::EXCHANGES[exchange];
    let source = if series.venue.has_markets() {
        format!("{exchange} {}", Venue::MARKETS[market])
    } else {
        exchange.to_string()
    };
    SeriesTitle {
        symbol: series.symbol.clone(),
        source,
    }
}

/// A chart of a series, with the trading fees of its venue (an imported one pays as on spot) and without the
/// chart's own buttons and readouts: the panels have them.
pub fn new_chart(candles: Candles, venue: Venue) -> Chart {
    let mut chart = Chart::new(candles);
    chart.remove_layer(names::TOOLBAR);
    chart.remove_handler(names::TOOLBAR);
    chart.resources_mut().insert(NoReadouts);
    chart.add_plugin(Legend);
    // Orders go through the order panel only: a key pressed by mistake must not trade.
    if let Some(keymap) = chart.resources_mut().get_mut::<Keymap>() {
        for key in TRADING_KEYS {
            keymap.unbind(Key::Char(key));
        }
    }
    if let Some(account) = chart.resources_mut().get_mut::<Account>() {
        account.fee_rate = match venue {
            Venue::Binance(Market::UsdFutures) | Venue::Okx(tickerlight_okx::Market::Swap) | Venue::Kraken => {
                FUTURES_FEE
            }
            Venue::Kucoin(tickerlight_kucoin::Market::Futures) | Venue::Bitget(tickerlight_bitget::Market::Futures) => {
                KUCOIN_BITGET_FUTURES_FEE
            }
            Venue::Binance(Market::Spot)
            | Venue::Okx(tickerlight_okx::Market::Spot)
            | Venue::Kucoin(tickerlight_kucoin::Market::Spot)
            | Venue::Bitget(tickerlight_bitget::Market::Spot)
            | Venue::Imported => SPOT_FEE,
            Venue::Coinbase => COINBASE_FEE,
            Venue::Bitstamp => BITSTAMP_FEE,
            Venue::Bitfinex => BITFINEX_FEE,
            Venue::Deribit => DERIBIT_FEE,
            // A CFD's cost is its spread, not a fee.
            Venue::Dukascopy => 0.0,
        };
    }
    chart
}

/// What the panels show and ask for besides the chart.
pub struct Program {
    /// The symbol field: a symbol, or with the file venue also the path of a CSV file to import.
    pub symbol: String,
    /// Index into [`Venue::ALL`].
    pub venue: usize,
    /// What the chart shows.
    pub shown: Option<Series>,
    pub status: String,
    pub error: bool,
    /// A series to load, asked for by the symbol panel.
    pub request: Option<Series>,
    /// A CSV file to import and then show.
    pub import: Option<PathBuf>,
    /// Order flow drawn from trades.
    pub footprint: bool,
    pub profile: bool,
    /// How getting the trades goes.
    pub flow_status: String,
    /// What the side column shows; `None` folds it, giving the chart its room.
    pub page: Option<Page>,
    /// The loading is shown in the middle of the empty chart, not in the status line.
    pub loading_card: bool,
    /// When the session going on began, once it has anything worth keeping (a replay, a trade, a drawing).
    pub session: Option<Time>,
    /// The saved sessions, newest first.
    pub sessions: Vec<Session>,
    /// A saved session to open, by when it began.
    pub open: Option<Time>,
    /// Asked for a new session: the replay, the account and the drawings start over.
    pub new_session: bool,
    /// Asked for the trades of this session as CSV.
    pub export: bool,
    /// How the last export went: lines, and whether it failed.
    pub exported: Option<(Vec<String>, bool)>,
    pub watch: Watchlist,
    /// The charts the window shows.
    pub layout: Layout,
    /// The clock every chart shows its times on.
    pub zone: Zone,
    /// The journal columns, the same in every session (`journal.txt`), and as typed on the sessions page.
    pub columns: JournalColumns,
    pub columns_text: String,
    /// The folder of the pictures of this session's trades, once there are any.
    pub snapshots: Option<PathBuf>,
    /// The interval box a digit typed over the chart opens, as typed; `None` while it is closed.
    pub interval: Option<String>,
    /// The symbol and venue that were shown when another exchange or market was picked: what that one has of it
    /// is opened once its list of symbols is here.
    pub carry: Option<(Venue, String)>,
}

impl Program {
    fn venue(&self) -> Venue {
        Venue::ALL[self.venue]
    }

    /// The series the symbol field names, if it names one.
    fn series(&self) -> Option<Series> {
        let venue = self.venue();
        Some(Series {
            venue,
            symbol: venue.symbol_of(&self.symbol)?,
            interval: Interval::M1,
        })
    }

    /// Asks for `symbol` of `venue`, the symbol field showing it. The search that found it is done with.
    pub(crate) fn open(&mut self, venue: Venue, symbol: String) {
        (self.symbol, self.venue) = (symbol.clone(), venue.index());
        self.watch.search.clear();
        self.watch.pick = 0;
        self.request = Some(Series {
            venue,
            symbol,
            interval: Interval::M1,
        });
    }
}

/// A path typed into the symbol field, with `~` for the home folder.
fn typed_path(text: &str) -> PathBuf {
    let text = text.trim();
    match (text.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(text),
    }
}

/// What the symbol field is found by, and with the file venue.
pub const SYMBOL_FIELD: &str = "Symbol";
pub const IMPORT_FIELD: &str = "CSV file or symbol";
/// What the interval box is found by.
pub const INTERVAL: &str = "Interval";

/// The length of bar an interval typed in the box names, in milliseconds: minutes alone (`15`), or with `s`, `m`,
/// `h` or `d` (`30s`, `4h`, `1d`, `d`).
fn parse_interval(text: &str) -> Option<i64> {
    let text = text.trim().to_ascii_lowercase();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: i64 = if number.is_empty() { 1 } else { number.parse().ok()? };
    let unit = match unit.trim() {
        "" | "m" => 60_000,
        "s" => 1_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit).filter(|&span| span > 0)
}

/// The timeframe `typed` asks for on `chart`: `None` for the bars as the data came. Longer bars are made of whole
/// bars of the data, bars of seconds of its trades.
fn timeframe_of(chart: &Chart, typed: &str) -> Result<Option<i64>, &'static str> {
    let span = parse_interval(typed).ok_or("Type an interval like 15, 30s, 4h or 1d")?;
    let native = native_step(chart.base()).filter(|&s| s > 0).ok_or("No bars yet")?;
    let tape = chart.resources().get::<Tape>().is_some_and(|t| t.0);
    if span == native {
        Ok(None)
    } else if span > native && span % native == 0 || span < native && tape && span % 1_000 == 0 {
        Ok(Some(span))
    } else {
        Err("No bars of that length here")
    }
}

/// The interval box over the chart: a digit typed over the chart opens it with that digit, Enter shows bars of the
/// length typed, Escape or a click elsewhere closes it.
fn interval_panel(ui: &mut Ui, chart: &mut Chart, p: &mut Program) {
    let Some(text) = &mut p.interval else {
        return;
    };
    ui.space(8.0);
    ui.heading("Interval");
    let field = ui.text_field(text, 80.0, INTERVAL);
    ui.tip("Minutes like 15, or 30s, 4h, 1d; then Enter");
    if field.submitted {
        match timeframe_of(chart, text) {
            Ok(span) => chart.set_timeframe(span),
            Err(e) => (p.status, p.error) = (e.to_string(), true),
        }
    }
    if !field.focused {
        p.interval = None;
    }
}

/// The symbol to show and where it is from (an exchange and its market, or a file), and how loading it goes. With a
/// file the field takes the path of a CSV file to import, or the name of a symbol imported before.
fn symbol_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    let file = p.venue() == Venue::Imported;
    let field = if file {
        ui.text_field(&mut p.symbol, 220.0, IMPORT_FIELD)
    } else {
        ui.text_field(&mut p.symbol, 110.0, SYMBOL_FIELD)
    };
    if file {
        ui.tip("The path of a CSV file of candles or trades, or a symbol imported before");
    }
    // A click in the field opens the list of symbols beside the chart: what to pick from shows before anything
    // is typed.
    if field.gained && !file {
        p.watch.search.clear();
        p.page = Some(Page::Watchlist);
    }
    if field.changed {
        if !file {
            p.symbol = p.symbol.to_ascii_uppercase();
        }
        // What is typed is looked for in the side column as it is typed; a path is not a symbol to look for.
        let path = file && p.symbol.contains(['/', '\\', '~']);
        p.watch.search = if path { String::new() } else { p.symbol.clone() };
        p.watch.pick = 0;
        p.page = Some(Page::Watchlist);
    }
    // Up and Down move through what the search found; Enter opens the one lit.
    if field.step != 0 {
        p.watch.step(p.venue, field.step);
    }
    // Escape gives up what was typed: the field shows the chart's symbol again.
    if field.cancelled
        && let Some(shown) = &p.shown
    {
        (p.symbol, p.venue) = (shown.symbol.clone(), shown.venue.index());
        p.watch.search.clear();
    }
    let (mut exchange, mut market) = p.venue().choices();
    let exchange_picked = ui.dropdown(&mut exchange, &Venue::EXCHANGES);
    let market_picked = p.venue().has_markets() && ui.choice(&mut market, &Venue::MARKETS);
    if exchange_picked || market_picked {
        let was = p.venue();
        p.venue = Venue::of_choices(exchange, market).index();
        // The same coin is shown there, as it names it, once its list says what it has. A file's symbol is no
        // exchange's: the list opens to pick from.
        let from = (p.shown.as_ref()).map_or((was, p.symbol.clone()), |s| (s.venue, s.symbol.clone()));
        let to = p.venue();
        if to != Venue::Imported && to != was {
            p.watch.search.clear();
            match from.0 {
                Venue::Imported => p.page = Some(Page::Watchlist),
                _ => p.carry = Some(from),
            }
        }
    }
    if field.submitted {
        let path = typed_path(&p.symbol);
        if file && Path::new(&path).is_file() {
            p.import = Some(path);
        } else if let Some((venue, symbol)) = p.watch.picked(p.venue) {
            p.open(venue, symbol);
        } else if let Some(series) = p.series() {
            p.request = Some(series);
        }
    }
    ui.separator();
}

/// The pages of the side column as icons at the window's edge; a click on the page shown folds the column.
fn page_strip(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    let pages = [
        (
            Icon::List,
            "Watchlist",
            "Watchlist: symbols with their prices",
            Page::Watchlist,
        ),
        (
            Icon::Trade,
            "Trade",
            "Trade: the order ticket, the position, the statistics",
            Page::Trade,
        ),
        (
            Icon::Strategy,
            "Strategy",
            "Strategy: run one over the bars",
            Page::Strategy,
        ),
        (
            Icon::Journal,
            "Sessions",
            "Sessions: the journal of the trades",
            Page::Sessions,
        ),
    ];
    for (icon, name, tip, page) in pages {
        if ui.icon_button(icon, name, p.page == Some(page)) {
            p.page = if p.page == Some(page) { None } else { Some(page) };
        }
        ui.tip(tip);
    }
}

/// What the menu of the layouts is found by.
pub const LAYOUTS: &str = "Layout";

/// The indicators page's button, and the layouts of the charts.
fn page_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    let on = p.page == Some(Page::Indicators);
    if ui.toggle("Indicators", on) {
        p.page = if on { None } else { Some(Page::Indicators) };
        // As on TradingView: the list to pick from at once, and typing narrows it.
        if !on {
            ui.focus(SEARCH_INDICATORS);
        }
    }
    ui.tip("The indicators on the chart and those to add. Key /");
    let at = Layout::ALL.iter().position(|&l| l == p.layout).unwrap_or(0);
    let (names, icons) = (Layout::ALL.map(Layout::label), Layout::ALL.map(Layout::icon));
    if let Some(pick) = ui.icon_menu(p.layout.icon(), LAYOUTS, Some(at), &names, &icons) {
        p.layout = Layout::ALL[pick];
    }
    ui.tip("Layout: how many charts the window shows, and how they are placed");
}

/// The strategies to choose from. One's own goes here: a type that implements [`Strategy`]; or, with no rebuild,
/// built to WebAssembly in `<data>/strategies/` (see `tickerlight-strategy`), or a model trained elsewhere as an ONNX file
/// there (see `tickerlight-onnx`).
fn strategies() -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(MovingAverageCross::default()),
        Box::new(RsiReversal::default()),
        Box::new(LearnedDirection::default()),
    ]
}

/// The strategy page; while another page is shown, the strategy on the chart still follows the bars. What it
/// shows is kept in `<data>/strategy.txt`.
struct StrategyPage {
    panel: StrategyPanel,
    path: PathBuf,
    saved: String,
}

impl StrategyPage {
    fn load(source: &Source) -> Self {
        let path = source.root.join("strategy.txt");
        let folder = source.root.join("strategies");
        let written = folder.join("features.csv");
        let files = StrategyFiles {
            folder: folder.display().to_string(),
            read: Box::new(move || {
                let (strategies, mut problems) = tickerlight_wasm::load_folder(&folder);
                let (models, more) = tickerlight_onnx::load_folder(&folder);
                problems.extend(more);
                let strategies = (strategies.into_iter())
                    .map(|s| Box::new(s) as Box<dyn Strategy>)
                    .chain(models.into_iter().map(|m| Box::new(m) as Box<dyn Strategy>))
                    .collect();
                (strategies, problems)
            }),
            features: Box::new(move |csv| {
                (written.parent())
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&written, csv))
                    .map(|()| written.display().to_string())
                    .map_err(|e| e.to_string())
            }),
        };
        let mut panel = StrategyPanel::new(strategies()).with_files(files);
        if let Ok(text) = std::fs::read_to_string(&path) {
            panel.load(&text);
        }
        let saved = panel.save();
        Self { panel, path, saved }
    }
}

impl Panel<Program> for StrategyPage {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, p: &mut Program) {
        if p.page == Some(Page::Strategy) {
            self.panel.ui(ui, chart, p);
        } else {
            self.panel.follow(chart);
        }
        let text = self.panel.save();
        if text != self.saved {
            let written = self
                .path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&self.path, &text));
            if let Err(e) = written {
                (p.status, p.error) = (format!("Strategy not saved: {e}"), true);
            }
            self.saved = text;
        }
    }
}

/// The indicators are the same on every chart: kept in one file.
fn studies_path(source: &Source) -> PathBuf {
    source.root.join("studies.txt")
}

/// The journal columns, a line each.
fn journal_path(source: &Source) -> PathBuf {
    source.root.join("journal.txt")
}

/// Order flow from trades: the footprint of each bar and the volume profile of the screen.
fn flow_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    ui.separator();
    if ui.toggle("Footprint", p.footprint) {
        p.footprint = !p.footprint;
    }
    if ui.toggle("Profile", p.profile) {
        p.profile = !p.profile;
    }
}

/// The time zone the times are shown in: on the axes, the crosshair, the replay and the lists.
fn zone_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    ui.separator();
    let mut zone = Zone::ALL.iter().position(|&z| z == p.zone).unwrap_or(0);
    if ui.dropdown(&mut zone, &Zone::ALL.map(Zone::label)) {
        p.zone = Zone::ALL[zone];
    }
    ui.tip("The time zone times are shown in. Candles stay in UTC days");
}

/// How loading goes, over the top of the chart.
fn status_panel(ui: &mut Ui, _: &mut Chart, p: &mut Program) {
    if p.loading_card {
        return;
    }
    let theme = *ui.theme();
    if !p.status.is_empty() {
        ui.notice(&p.status, if p.error { theme.down } else { theme.muted });
    }
    if !p.flow_status.is_empty() {
        ui.notice(&p.flow_status, theme.muted);
    }
}

/// The trades of a piece of the tape; `None` if the archive does not have that day yet.
type ChunkOfTrades = Result<Option<Vec<Trade>>, String>;

/// The trades held for the order flow, in order, and the piece being loaded.
#[derive(Default)]
struct Flow {
    /// Whose trades they are.
    series: Option<Series>,
    chunks: Vec<(Chunk, Vec<Trade>)>,
    loading: Option<(Chunk, Receiver<ChunkOfTrades>)>,
    /// Pieces that failed to load, and why: not asked for again until another symbol.
    failed: Vec<(Chunk, String)>,
    /// Days the archive does not have yet: loaded by the hour.
    unarchived: Vec<Period>,
    /// The trades from the start of the hour the order flow was switched on, live: they follow the chunks.
    live: Option<(Time, flow::LiveTape)>,
    live_trades: Vec<Trade>,
}

impl Flow {
    /// The tape of the chunks and the live trades after them.
    fn tape(&self) -> Trades {
        let mut tape = flow::tape(&self.chunks);
        for &trade in &self.live_trades {
            let _ = tape.push(trade);
        }
        tape
    }
}

/// What belongs to one chart of the layout, besides the chart. The active chart's is in the [`Window`] and its
/// [`Program`]; another's waits here until it is active (see [`Window::swap_parked`]).
#[derive(Default)]
struct Parked {
    loading: Option<(Series, Receiver<Message>)>,
    flow: Flow,
    saved: String,
    saved_at: Option<Instant>,
    unsaved: bool,
    last_state: Option<Session>,
    pending: Option<Session>,
    files: (usize, usize, bool),
    drawings_saved: String,
    drawings_seen: Vec<Drawing>,
    symbol: String,
    venue: usize,
    shown: Option<Series>,
    status: String,
    error: bool,
    footprint: bool,
    profile: bool,
    flow_status: String,
    session: Option<Time>,
    /// The replay last put on this chart to match the active one's: on, cursor, moment.
    replay: (bool, Option<Time>, Option<Time>),
    second: i64,
}

enum Message {
    Progress(sync::Progress),
    Done(Series, Result<(Candles, Option<String>), String>),
    /// The rest of the history of a series first shown with its newest candles only: a file of it as it comes,
    /// and all of it at the end.
    Older(Candles),
    History(Result<Candles, String>),
}

/// The window: a [`Workspace`] with the program's panels, and the loading of the series asked for.
pub struct Window {
    workspace: Workspace<Program>,
    source: Source,
    loading: Option<(Series, Receiver<Message>)>,
    flow: Flow,
    sessions: Sessions,
    /// The session's text as last saved, and when.
    saved: String,
    saved_at: Option<Instant>,
    /// The second the chart's clock was last set to: the countdown to the bar's close moves once a second.
    second: i64,
    /// Changed since the last save, which waits a moment so a replay playing is not written every bar.
    unsaved: bool,
    /// The session as it was after the last event, saved or not.
    last_state: Option<Session>,
    /// A session to put on the chart once its series is loaded.
    pending: Option<Session>,
    /// Where the trades go as CSV.
    exports: PathBuf,
    /// Where the program is, as last written for the next start.
    resumed: String,
    /// How the loading goes: files checked, files in all, and whether any had to be downloaded.
    files: (usize, usize, bool),
    /// The drawings of the symbol shown, as last read or written, and as they were when last looked at.
    drawings_saved: String,
    drawings_seen: Vec<Drawing>,
    /// The indicators as last read or written: every chart shown gets them.
    studies: Vec<Study>,
    /// The journal columns as last read or written.
    columns_saved: JournalColumns,
    /// Pictures of the chart as trades close; the session, how many of its fills were looked at, and whether that
    /// was during a review.
    snapshots: Snapshots,
    snapped: (Option<Time>, usize, bool),
    /// Per chart of the layout, by cell; the active one's entry is unused.
    parked: Vec<Parked>,
}

impl Window {
    /// A window showing `series`: loaded in the background unless `candles` are given.
    pub fn new(source: Source, series: Series, candles: Option<(Candles, Option<String>)>) -> Self {
        let columns = JournalColumns::parse(&std::fs::read_to_string(journal_path(&source)).unwrap_or_default());
        let program = Program {
            symbol: series.symbol.clone(),
            venue: series.venue.index(),
            shown: None,
            status: String::new(),
            error: false,
            request: None,
            import: None,
            footprint: false,
            profile: false,
            flow_status: String::new(),
            page: Some(Page::Watchlist),
            loading_card: false,
            session: None,
            sessions: Vec::new(),
            open: None,
            new_session: false,
            export: false,
            exported: None,
            watch: Watchlist::load(&source.root),
            layout: Layout::One,
            zone: Zone::Utc,
            columns_text: columns.to_text("; "),
            columns: columns.clone(),
            snapshots: None,
            interval: None,
            carry: None,
        };
        let workspace = Workspace::new(new_chart(Candles::new(), series.venue), program)
            .with(Dock::Top, symbol_panel)
            .with(Dock::Top, TimeframePanel::default())
            .with(Dock::Top, |ui: &mut Ui, _: &mut Chart, _: &mut Program| ui.separator())
            .with(Dock::Top, ToolsPanel)
            .with(Dock::Top, flow_panel)
            .with(Dock::Top, ReplayButton)
            .with(Dock::Top, page_panel)
            // First in its dock, so its menu keeps its place among the controls whatever the status shows.
            .with(Dock::Overlay, ContextMenuPanel::default())
            .with(Dock::Overlay, status_panel)
            .with(Dock::Left, DrawToolsPanel::default())
            // The interval box first, so its field keeps its place while the drawing's style row hides for it.
            .with(Dock::Float, interval_panel)
            .with(Dock::Float, DrawingStylePanel)
            .with(Dock::Player, ReplayPlayer::default())
            .with(Dock::Bottom, RangePanel::default())
            .with(Dock::Bottom, zone_panel)
            .with(Dock::Bottom, ScalePanel)
            .with(Dock::Strip, page_strip)
            .with(Dock::Right, OnPage(Page::Trade, ReplayPanel::default()))
            .with(Dock::Right, OnPage(Page::Trade, OrderPanel::default()))
            .with(Dock::Right, OnPage(Page::Trade, PositionPanel::default()))
            .with(Dock::Right, OnPage(Page::Trade, StatsPanel::default()))
            .with(Dock::Right, OnPage(Page::Trade, TradesPanel::default()))
            .with(Dock::Right, OnPage(Page::Sessions, sessions_panel))
            .with(Dock::Right, OnPage(Page::Indicators, IndicatorsPanel::default()))
            .with(Dock::Right, OnPage(Page::Watchlist, watchlist_panel))
            .with(Dock::Right, StrategyPage::load(&source));
        let mut workspace = workspace;
        workspace.show(Dock::Player, false);
        let sessions = Sessions::new(&source.root);
        let exports = source.root.join("exports");
        let studies = Studies::load(&std::fs::read_to_string(studies_path(&source)).unwrap_or_default());
        let mut window = Self {
            workspace,
            source,
            loading: None,
            flow: Flow::default(),
            sessions,
            saved: String::new(),
            saved_at: None,
            second: 0,
            unsaved: false,
            last_state: None,
            pending: None,
            exports,
            resumed: String::new(),
            files: (0, 0, false),
            drawings_saved: String::new(),
            drawings_seen: Vec::new(),
            studies,
            columns_saved: columns,
            snapshots: Snapshots::default(),
            snapped: (None, 0, false),
            parked: vec![Parked::default()],
        };
        // The chart waiting for the first load has them too, or they would look removed.
        let studies = window.studies.clone();
        set_studies(window.workspace.chart_mut(), studies);
        window.sync_columns();
        window.list_sessions();
        match candles {
            Some((candles, note)) => window.show(series, candles, note),
            None => window.start(series),
        }
        window
    }

    /// A window that starts where the program was left: the layout, and in each chart the series (the active
    /// one's unless `series` asks for another), its timeframe and the session going on then.
    pub fn resumed(source: Source, series: Option<Series>) -> Self {
        let resume = Resume::load(&source.root);
        let mut charts = resume.as_ref().map(|r| r.charts.clone()).unwrap_or_default();
        let active = resume.as_ref().map_or(0, |r| r.active);
        if let Some(series) = series {
            let asked = Resumed {
                series,
                timeframe: None,
                session: None,
            };
            match charts.get_mut(active) {
                Some(chart) if chart.series != asked.series => *chart = asked,
                Some(_) => {}
                None => charts = vec![asked],
            }
        }
        if charts.is_empty() {
            charts.push(Resumed {
                series: Series {
                    venue: Venue::Binance(Market::Spot),
                    symbol: DEFAULT_SYMBOL.into(),
                    interval: Interval::M1,
                },
                timeframe: None,
                session: None,
            });
        }
        let zone = resume.as_ref().map_or(Zone::Utc, |r| r.zone);
        let layout = resume.map_or(Layout::One, |r| r.layout);
        charts.truncate(layout.cells());
        let first = charts.remove(0);
        let mut window = Self::new(source, first.series.clone(), None);
        window.resume_chart(&first);
        window.set_layout(layout, charts);
        window.activate(active.min(layout.cells() - 1));
        window.workspace.ctx.zone = zone;
        window.show_zone();
        window
    }

    /// Puts the active chart, loading, where `chart` was left: its timeframe, and its session once loaded.
    fn resume_chart(&mut self, chart: &Resumed) {
        // Kept by the chart that loads.
        self.workspace.chart_mut().set_timeframe(chart.timeframe);
        let p = &self.workspace.ctx;
        self.pending = chart
            .session
            .and_then(|created| {
                p.sessions
                    .iter()
                    .find(|s| s.created == created && s.series == chart.series)
            })
            .cloned();
    }

    /// Puts the zone chosen on every chart that shows another; whether any did.
    fn show_zone(&mut self) -> bool {
        let zone = self.workspace.ctx.zone;
        let mut changed = false;
        for i in 0..self.workspace.charts() {
            let resources = self.workspace.chart_at_mut(i).resources_mut();
            if resources.get::<Zone>() != Some(&zone) {
                resources.insert(zone);
                changed = true;
            }
        }
        changed
    }

    /// Puts the journal columns on every chart that has others, and writes them when they changed; whether any
    /// chart got them.
    fn sync_columns(&mut self) -> bool {
        let columns = self.workspace.ctx.columns.clone();
        let mut changed = false;
        for i in 0..self.workspace.charts() {
            let resources = self.workspace.chart_at_mut(i).resources_mut();
            if resources.get::<JournalColumns>() != Some(&columns) {
                resources.insert(columns.clone());
                changed = true;
            }
        }
        if columns != self.columns_saved {
            let path = journal_path(&self.source);
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, columns.to_text("\n") + "\n"));
            if let Err(e) = written {
                let p = &mut self.workspace.ctx;
                (p.status, p.error) = (format!("Journal columns not saved: {e}"), true);
            }
            self.columns_saved = columns;
        }
        changed
    }

    /// Writes where the program is when that changed.
    fn save_resume(&mut self) {
        let mut charts = Vec::new();
        for i in 0..self.workspace.charts() {
            let (shown, loading, session) = if i == self.workspace.active() {
                let p = &self.workspace.ctx;
                (&p.shown, &self.loading, p.session)
            } else {
                let c = &self.parked[i];
                (&c.shown, &c.loading, c.session)
            };
            // A chart still loading its first series is where it is going.
            let Some(series) = shown.clone().or_else(|| loading.as_ref().map(|(s, _)| s.clone())) else {
                return;
            };
            charts.push(Resumed {
                series,
                timeframe: self.workspace.chart_at(i).timeframe(),
                session,
            });
        }
        let resume = Resume {
            layout: self.workspace.layout(),
            active: self.workspace.active(),
            zone: self.workspace.ctx.zone,
            charts,
        };
        let text = resume.to_text();
        if text != self.resumed {
            if let Err(e) = resume.save(&self.source.root) {
                self.save_failed(e);
            }
            self.resumed = text;
        }
    }

    /// Puts exported trades into `dir` (by default `exports` in the data folder).
    pub fn set_exports(&mut self, dir: PathBuf) {
        self.exports = dir;
    }

    pub fn workspace(&self) -> &Workspace<Program> {
        &self.workspace
    }

    pub fn workspace_mut(&mut self) -> &mut Workspace<Program> {
        &mut self.workspace
    }

    fn show(&mut self, series: Series, candles: Candles, note: Option<String>) {
        let p = &mut self.workspace.ctx;
        // Nothing to say unless something is missing.
        p.status = note.unwrap_or_default();
        p.error = false;
        p.shown = Some(series.clone());
        if series.venue == Venue::Imported {
            // The field shows the symbol, not the path it was imported from.
            (p.symbol, p.venue) = (series.symbol.clone(), series.venue.index());
            p.watch.know(series.venue, &series.symbol);
        }
        // Another symbol is looked at in the same timeframe; bars of seconds only where there are trades to make
        // them of (Dukascopy has none; an imported symbol has those imported with it).
        let tape = match series.venue {
            Venue::Dukascopy => false,
            Venue::Imported => (Store::new(&self.source.root).trade_days(&series)).is_ok_and(|d| !d.is_empty()),
            _ => true,
        };
        let native = native_step(candles.view());
        let timeframe =
            (self.workspace.chart().timeframe()).filter(|&span| tape || native.is_none_or(|native| span >= native));
        let newest = candles.view().time().last().copied();
        let mut chart = new_chart(candles, series.venue);
        chart.set_timeframe(timeframe);
        chart.resources_mut().insert(series_title(&series));
        set_studies(&mut chart, self.studies.clone());
        // From the newest candle on, the chart follows the market; where its trades come too, orders are paper
        // trades on it.
        let mut live = false;
        if let Some(newest) = newest
            && !self.source.offline
        {
            live = series.venue != Venue::Dukascopy;
            match series.venue {
                Venue::Binance(market) => {
                    let live =
                        tickerlight_binance::LiveCandles::connect(market, &series.symbol, series.interval, newest);
                    chart.add_source(live);
                }
                // OKX has 1-minute candles only.
                Venue::Okx(market) if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_okx::LiveCandles::connect(market, &series.symbol, newest));
                }
                // Coinbase, KuCoin, Bitget, Kraken, Bitstamp, Bitfinex and Deribit have 1-minute candles only.
                Venue::Coinbase if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_coinbase::LiveCandles::connect(&series.symbol, newest));
                }
                Venue::Kucoin(market) if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_kucoin::LiveCandles::connect(market, &series.symbol, newest));
                }
                Venue::Bitget(market) if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_bitget::LiveCandles::connect(market, &series.symbol, newest));
                }
                Venue::Kraken if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_kraken::LiveCandles::connect(&series.symbol, newest));
                }
                Venue::Bitstamp if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_bitstamp::LiveCandles::connect(&series.symbol, newest));
                }
                Venue::Bitfinex if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_bitfinex::LiveCandles::connect(&series.symbol, newest));
                }
                Venue::Deribit if series.interval == Interval::M1 => {
                    chart.add_source(tickerlight_deribit::LiveCandles::connect(&series.symbol, newest));
                }
                // The loading kept the list of instruments: no download here.
                Venue::Dukascopy => {
                    if let Some(instrument) = crate::dukascopy::kept(&self.source.root, &series.symbol) {
                        chart.add_source(tickerlight_dukascopy::LiveCandles::connect(instrument, newest));
                    }
                }
                Venue::Okx(_)
                | Venue::Coinbase
                | Venue::Kucoin(_)
                | Venue::Bitget(_)
                | Venue::Kraken
                | Venue::Bitstamp
                | Venue::Bitfinex
                | Venue::Deribit
                | Venue::Imported => live = false,
            }
        }
        chart.resources_mut().insert(Live(live));
        chart.resources_mut().insert(Tape(tape));
        self.workspace.set_chart(chart);
        self.load_drawings(&series);
        match self.pending.take().filter(|s| s.series == series) {
            Some(session) => self.put(&session),
            None => self.resume_live(),
        }
    }

    /// Where the drawings of a symbol are kept: they belong to the symbol, whatever session or timeframe.
    fn drawings_path(&self, series: &Series) -> PathBuf {
        self.source
            .root
            .join("drawings")
            .join(series.venue.name())
            .join(format!("{}.txt", series.symbol))
    }

    fn load_drawings(&mut self, series: &Series) {
        let text = std::fs::read_to_string(self.drawings_path(series)).unwrap_or_default();
        let chart = self.workspace.chart_mut();
        let panes = chart.pane_names();
        if let Some(drawings) = chart.resources_mut().get_mut::<Drawings>() {
            drawings.load(&text, &panes);
            self.drawings_seen = drawings.items.clone();
        }
        self.drawings_saved = text;
    }

    /// Writes the drawings of the symbol shown when they changed (not while one is dragged).
    fn save_drawings(&mut self) {
        let chart = self.workspace.chart();
        let Some(series) = self.workspace.ctx.shown.clone() else {
            return;
        };
        if chart.pointer().pressed {
            return;
        }
        let Some(drawings) = chart.resources().get::<Drawings>() else {
            return;
        };
        // Most events change no drawing: comparing is cheaper than writing them out.
        if drawings.items == self.drawings_seen {
            return;
        }
        self.drawings_seen = drawings.items.clone();
        let text = drawings.save();
        if text == self.drawings_saved {
            return;
        }
        let path = self.drawings_path(&series);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &text));
        match written {
            Ok(()) => self.drawings_saved = text,
            Err(e) => {
                let p = &mut self.workspace.ctx;
                (p.status, p.error) = (format!("Drawings not saved: {e}"), true);
                // Not tried again at every event.
                self.drawings_saved = text;
            }
        }
    }

    /// Keeps the prices of the watchlist page fresh while it is shown, and the list written; opens what another
    /// exchange or market picked has of the symbol shown. Returns whether anything changed.
    fn sync_watchlist(&mut self, event: Event) -> bool {
        let p = &mut self.workspace.ctx;
        let offline = self.source.offline;
        let shown = p.page == Some(Page::Watchlist) && !offline;
        let venue = Venue::ALL[p.venue];
        if (shown || p.carry.is_some()) && !offline {
            p.watch.refresh(venue);
        }
        if shown {
            p.watch.refresh_listed();
        }
        p.watch.follow(shown);
        let tick = matches!(event, Event::Tick { .. });
        let mut changed = tick && (p.watch.poll() | p.watch.take_live());
        if let Err(e) = p.watch.save() {
            (p.status, p.error) = (format!("Watchlist not saved: {e}"), true);
        }
        if let Some((from, symbol)) = p.carry.take() {
            match p.watch.counterpart(from, &symbol, venue, offline) {
                Counterpart::Waiting => p.carry = Some((from, symbol)),
                Counterpart::Found(found) if from == venue && found == symbol => p.symbol = found,
                Counterpart::Found(found) => p.open(venue, found),
                Counterpart::Missing(coin) => {
                    let (name, asked) = (venue.label(), display(from, &symbol));
                    // Its symbols of the coin, if it has any, else all of them to pick from.
                    let any = !p.watch.matches(venue.index(), &coin).is_empty();
                    p.status = match any {
                        true => format!("{name} has no {asked}: its symbols with {coin} are listed"),
                        false => format!("{name} has no {coin}: pick one of its symbols"),
                    };
                    p.error = false;
                    p.watch.search = if any { coin } else { String::new() };
                    (p.symbol, p.watch.pick) = (String::new(), 0);
                    p.page = Some(Page::Watchlist);
                }
            }
            changed |= p.carry.is_none();
        }
        if let Some(series) = self.workspace.ctx.request.take() {
            self.start(series);
            changed = true;
        }
        changed
    }

    /// Writes the indicators when they changed.
    fn save_studies(&mut self) {
        let now = studies(self.workspace.chart());
        if now == self.studies.as_slice() {
            return;
        }
        self.studies = now.to_vec();
        let path = studies_path(&self.source);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, Studies::save(&self.studies)));
        if let Err(e) = written {
            let p = &mut self.workspace.ctx;
            (p.status, p.error) = (format!("Indicators not saved: {e}"), true);
        }
    }

    /// Loads `series` in the background. The chart and its session stay as they are until it has come: one that
    /// fails to load changes nothing.
    fn start(&mut self, series: Series) {
        self.spawn(series, None);
    }

    /// Imports a CSV file in the background, then shows what it had like any other series.
    fn start_import(&mut self, path: PathBuf) {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let series = Series {
            venue: Venue::Imported,
            symbol: import::symbol_name(name).unwrap_or_default(),
            interval: Interval::M1,
        };
        self.spawn(series, Some(path));
    }

    /// Loads `series`, or what `file` has once it is imported.
    fn spawn(&mut self, series: Series, file: Option<PathBuf>) {
        let (send, receive) = mpsc::channel();
        let (source, asked) = (self.source.clone(), series.clone());
        let status = match &file {
            Some(path) => format!("Importing {}...", path.display()),
            None => format!("Loading {}...", series.symbol),
        };
        std::thread::spawn(move || {
            let store = Store::new(&source.root);
            let wanted = match file.map(|path| import::import(&store, &path)) {
                Some(Ok(series)) => series,
                Some(Err(e)) => {
                    let _ = send.send(Message::Done(asked, Err(e)));
                    return;
                }
                None => stored_interval(&store, asked),
            };
            // A series kept up to the last days shows at once; the live candles bring the rest, and the cache is
            // brought up to date for the next time behind it.
            if let Some(candles) = cached(&source, &wanted, now()) {
                let _ = send.send(Message::Done(wanted.clone(), Ok((candles, None))));
                let _ = refresh(&source, &wanted, now(), |_| {});
                return;
            }
            let sent = |progress: mpsc::Sender<Message>| {
                move |p: sync::Progress| {
                    let _ = progress.send(Message::Progress(p));
                }
            };
            // A symbol never seen before shows its newest candles first, from its venue's API at once (Dukascopy's
            // from the newest month of its archive). The rest of the history comes behind them.
            let never_seen = !source.offline
                && wanted.venue != Venue::Imported
                && store.periods(&wanted).is_ok_and(|p| p.is_empty());
            if never_seen {
                let at_once = match wanted.venue {
                    Venue::Dukascopy | Venue::Imported => Ok(Vec::new()),
                    _ => recent(
                        &source,
                        &wanted,
                        Time(now().0 - NEWEST_BARS * wanted.interval.millis()),
                        now(),
                    ),
                };
                let result = match at_once {
                    Err((_, true)) => {
                        let why = format!("There is no {} on {}", wanted.symbol, wanted.venue.label());
                        let _ = send.send(Message::Done(wanted, Err(why)));
                        return;
                    }
                    Ok(newest) if !newest.is_empty() => {
                        let mut candles = Candles::new();
                        for candle in newest {
                            candles.push(candle).expect("times increase");
                        }
                        Ok((candles, None))
                    }
                    _ => {
                        let newest = Source {
                            months: 1,
                            ..source.clone()
                        };
                        load(&newest, &wanted, now(), sent(send.clone()))
                    }
                };
                let shown = result.is_ok();
                let _ = send.send(Message::Done(wanted.clone(), result.map_err(|e| e.to_string())));
                if shown {
                    // Said at once: the archive takes a while to answer which files it has.
                    sent(send.clone())(sync::Progress {
                        done: 0,
                        total: 0,
                        fetched: None,
                    });
                    // Each file as it comes in, so the chart fills in towards the past while the rest download.
                    let progress = sent(send.clone());
                    let mut pieces = |p: sync::Progress| {
                        if let Some(period) = p.fetched
                            && let Ok(Some(candles)) = store.read(&wanted, period)
                        {
                            let mut piece = Candles::new();
                            for candle in candles {
                                let _ = piece.push(candle);
                            }
                            let _ = send.send(Message::Older(piece));
                        }
                        progress(p);
                    };
                    let loaded = AtomicBool::new(false);
                    let result = std::thread::scope(|scope| {
                        // While the archive says which files it has (OKX takes ~15 s), the API's candles before the
                        // newest, a page back at a time.
                        if wanted.venue != Venue::Dukascopy {
                            scope.spawn(|| {
                                let page = NEWEST_BARS * wanted.interval.millis();
                                let mut to = now().0 - page;
                                for _ in 0..MORE_PAGES {
                                    if loaded.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    let Ok(older) = recent(&source, &wanted, Time(to - page), Time(to - 1)) else {
                                        break;
                                    };
                                    if older.is_empty() {
                                        break;
                                    }
                                    let mut piece = Candles::new();
                                    for candle in older {
                                        let _ = piece.push(candle);
                                    }
                                    let _ = send.send(Message::Older(piece));
                                    to -= page;
                                }
                            });
                        }
                        let result = load(&source, &wanted, now(), &mut pieces);
                        loaded.store(true, Ordering::Relaxed);
                        result
                    });
                    let _ = send.send(Message::History(result.map(|(c, _)| c).map_err(|e| e.to_string())));
                }
                return;
            }
            let result = load(&source, &wanted, now(), sent(send.clone()));
            let _ = send.send(Message::Done(wanted, result.map_err(|e| e.to_string())));
        });
        let p = &mut self.workspace.ctx;
        p.status = status;
        p.error = false;
        self.files = (0, 0, false);
        // A load asked for before this one finished is forgotten.
        self.loading = Some((series, receive));
    }

    /// Takes what the loading sent. Returns whether anything changed.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some((series, receive)) = &self.loading {
            match receive.try_recv() {
                Ok(Message::Progress(p)) => {
                    let what = match self.workspace.chart().base().is_empty() {
                        true => "Loading",
                        false => "Loading the history of",
                    };
                    let symbol = &series.symbol;
                    self.workspace.ctx.status = match p.total {
                        0 => format!("{what} {symbol}..."),
                        _ => format!("{what} {symbol}: {} of {} files", p.done, p.total),
                    };
                    self.files = (p.done, p.total, self.files.2 || p.fetched.is_some());
                    changed = true;
                }
                // The loading stays until its thread is done: the rest of the history may follow.
                Ok(Message::Done(series, Ok((candles, note)))) => {
                    // What was on the chart is kept; the series that came begins a session of its own.
                    let pending = self.pending.take();
                    self.autosave(true);
                    self.end_session();
                    self.clear();
                    self.pending = pending;
                    self.show(series, candles, note);
                    changed = true;
                }
                Ok(Message::Done(asked, Err(e))) => {
                    (self.loading, self.pending) = (None, None);
                    let p = &mut self.workspace.ctx;
                    let e = asked.venue.plain(&e);
                    let named = e.contains(&asked.symbol);
                    p.status = if named { e } else { format!("{}: {e}", asked.symbol) };
                    p.error = true;
                    // The symbol field and the source show the chart's again (a path typed to import stays to be
                    // put right): what was asked is in the status.
                    if let Some(shown) = &p.shown
                        && asked.venue != Venue::Imported
                    {
                        (p.symbol, p.venue) = (shown.symbol.clone(), shown.venue.index());
                    }
                    changed = true;
                }
                Ok(Message::Older(piece)) => {
                    if self.workspace.ctx.shown.as_ref() == Some(series) {
                        self.workspace.chart_mut().merge(&piece);
                        changed = true;
                    }
                }
                Ok(Message::History(result)) => {
                    let same = self.workspace.ctx.shown.as_ref() == Some(series);
                    let p = &mut self.workspace.ctx;
                    match result {
                        Ok(older) if same => {
                            self.workspace.chart_mut().merge(&older);
                            self.workspace.ctx.status.clear();
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let e = series.venue.plain(&e);
                            (p.status, p.error) = (format!("The older history did not load: {e}"), true);
                        }
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.loading = None,
            }
        }
        changed
    }

    /// Keeps the order flow in step with the toggles and the screen: the layers on the chart, and the trades of
    /// the days on screen; without the layers, the trades where a replay playing by time stands, so its bars form
    /// trade by trade, or the trades to come while paper orders wait on a live chart. Returns whether anything
    /// changed.
    fn sync_flow(&mut self) -> bool {
        let mut changed = self.receive_trades();
        let (footprint, profile) = (self.workspace.ctx.footprint, self.workspace.ctx.profile);
        let chart = self.workspace.chart();
        let replay = chart.resources().get::<Replay>().cloned().unwrap_or_default();
        let replay_at = (replay.active && (replay.playing || chart.moment().is_some()))
            .then(|| chart.moment().or(chart.cursor()))
            .flatten();
        // Dukascopy has no trades: a replay of it plays its candles.
        let quotes = self
            .workspace
            .ctx
            .shown
            .as_ref()
            .is_some_and(|s| s.venue == Venue::Dukascopy);
        // Paper orders on a live chart wait for the trades to come.
        let orders = chart.cursor().is_none()
            && chart.resources().get::<Live>().is_some_and(|l| l.0)
            && chart
                .resources()
                .get::<Account>()
                .is_some_and(|a| !a.orders().is_empty());
        // Bars of seconds are made of the tape of the days shown, as the footprint and the profile are.
        let seconds = (chart.timeframe()).is_some_and(|span| native_step(chart.base()).is_some_and(|n| span < n));
        let drawn = footprint || profile || seconds && !quotes;
        let needed = drawn || replay_at.is_some() && !quotes || orders;
        let names = self.workspace.chart().layer_names();
        let (has_footprint, has_profile) = (names.contains(&FOOTPRINT), names.contains(&PROFILE));
        if footprint != has_footprint {
            let last = self.workspace.chart().base().close().last().copied().unwrap_or(1.0);
            let chart = self.workspace.chart_mut();
            if footprint {
                let plugin = Footprint {
                    grouped: true,
                    ..Footprint::new(flow::tick_size(last))
                };
                chart.add_plugin(plugin);
                // Where the footprint draws, it shows the volume of each bar under it.
                let volume = VolumeLayer {
                    hidden_from: Some(plugin.min_bar_width),
                    ..VolumeLayer::default()
                };
                chart.add_layer(names::VOLUME, order::VOLUME, volume);
                // Bars too narrow for the numbers are widened at once, around the right edge.
                let mut time = *chart.time_scale();
                let wide = (plugin.min_bar_width * FOOTPRINT_ZOOM * chart.scale()) as f64;
                if time.bar_spacing < wide {
                    // The room after the last bar stays as many pixels as it was.
                    let last = chart.seen().len() as f64;
                    time.right = last + (time.right - last).max(0.0) * time.bar_spacing / wide;
                    time.bar_spacing = wide;
                    chart.set_time_scale(time);
                }
            } else {
                chart.remove_layer(FOOTPRINT);
                chart.add_layer(names::VOLUME, order::VOLUME, VolumeLayer::default());
            }
            changed = true;
        }
        if profile != has_profile {
            let chart = self.workspace.chart_mut();
            if profile {
                let last = chart.base().close().last().copied().unwrap_or(1.0);
                chart.add_plugin(VolumeProfile {
                    tick: flow::tick_size(last),
                    ..VolumeProfile::default()
                });
            } else {
                chart.remove_layer(PROFILE);
            }
            changed = true;
        }
        let shown = self.workspace.ctx.shown.clone();
        if !needed || self.flow.series != shown {
            if !self.flow.chunks.is_empty()
                || self.flow.loading.is_some()
                || self.flow.live.is_some()
                || self.flow.series != shown
            {
                self.flow = Flow {
                    series: shown,
                    ..Flow::default()
                };
                self.workspace.chart_mut().set_trades(Trades::new());
                self.workspace.ctx.flow_status.clear();
                changed = true;
            }
            if !needed {
                return changed;
            }
        }
        let Some(series) = self.flow.series.clone() else {
            return changed;
        };
        // From the start of this hour on, the trades come live; an imported symbol has only what was imported. A
        // replay alone needs none.
        if !(drawn || orders) {
            if self.flow.live.take().is_some() {
                self.flow.live_trades.clear();
                let tape = self.flow.tape();
                self.workspace.chart_mut().set_trades(tape);
            }
        } else if self.flow.live.is_none() && !self.source.offline {
            let now = now();
            let hour = Time(now.0 - now.0.rem_euclid(flow::MS_PER_HOUR));
            self.flow.live = flow::LiveTape::connect(&series, hour).map(|live| (hour, live));
        }
        if let Some((from, live)) = &mut self.flow.live {
            let came = live.take();
            let chart = self.workspace.chart_mut();
            for trade in came {
                if chart.push_trade(trade).is_ok() {
                    self.flow.live_trades.push(trade);
                    changed = true;
                }
            }
            if flow::trim_live(&mut self.flow.live_trades, from, flow::LIVE_HELD) {
                let tape = self.flow.tape();
                self.workspace.chart_mut().set_trades(tape);
            }
        }
        let live_from = self.flow.live.as_ref().map(|(from, _)| *from);
        let day_start = series.venue.day_start();
        let wanted = match replay_at.filter(|_| !drawn) {
            Some(at) => flow::at(at, now(), &self.flow.unarchived, live_from, day_start)
                .into_iter()
                .collect(),
            None if drawn => flow::wanted(
                self.workspace.chart(),
                now(),
                &self.flow.unarchived,
                live_from,
                day_start,
            ),
            // Orders need only the trades to come.
            None => Vec::new(),
        };
        let held = self.flow.chunks.len();
        self.flow.chunks.retain(|(chunk, _)| wanted.contains(chunk));
        if self.flow.chunks.len() != held {
            let tape = self.flow.tape();
            self.workspace.chart_mut().set_trades(tape);
            changed = true;
        }
        // The newest first: the right of the screen is where one looks.
        if self.flow.loading.is_none()
            && let Some(&chunk) = wanted.iter().rev().find(|c| {
                !self.flow.chunks.iter().any(|(h, _)| h == *c) && !self.flow.failed.iter().any(|(f, _)| f == *c)
            })
        {
            let (send, receive) = mpsc::channel();
            let source = self.source.clone();
            // No trade after the candles: the last bar is as it was when they loaded. With the trades coming live,
            // the candles may still be catching up, and the hours before the live ones come whole.
            let base = self.workspace.chart().base();
            let end = match (live_from, base.time().last()) {
                (None, Some(t)) => Time(t.0 + native_step(base).unwrap_or(60_000)),
                _ => now(),
            };
            std::thread::spawn(move || {
                let _ = send.send(flow::load(&source, &series, chunk, end).map_err(|e| e.to_string()));
            });
            self.flow.loading = Some((chunk, receive));
            changed = true;
        }
        let status = self.flow_status(&wanted);
        if status != self.workspace.ctx.flow_status {
            self.workspace.ctx.flow_status = status;
            changed = true;
        }
        changed
    }

    /// Sets the chart's clock to now while the data come live: the last price counts down by it, and an order
    /// placed on a live chart counts as placed then. Returns whether the second changed.
    fn tick_clock(&mut self) -> bool {
        let now = now();
        let live = self
            .workspace
            .ctx
            .shown
            .as_ref()
            .is_some_and(|s| s.venue != Venue::Imported);
        if self.source.offline || !live {
            return false;
        }
        self.workspace.chart_mut().resources_mut().insert(Clock(now));
        let second = now.0.div_euclid(1000);
        std::mem::replace(&mut self.second, second) != second
    }

    /// Takes a piece of the tape that finished loading.
    fn receive_trades(&mut self) -> bool {
        let Some((chunk, receive)) = &self.flow.loading else {
            return false;
        };
        let chunk = *chunk;
        let result = match receive.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the loading stopped".into()),
        };
        self.flow.loading = None;
        match (result, chunk) {
            (Ok(Some(trades)), _) => {
                let day_start = self.flow.series.as_ref().map_or(0, |s| s.venue.day_start());
                let at = self
                    .flow
                    .chunks
                    .partition_point(|(c, _)| c.start(day_start) < chunk.start(day_start));
                self.flow.chunks.insert(at, (chunk, trades));
                let tape = self.flow.tape();
                self.workspace.chart_mut().set_trades(tape);
            }
            (Ok(None), Chunk::Day(day)) => self.flow.unarchived.push(day),
            (Ok(None), Chunk::Hour(_)) => self.flow.chunks.push((chunk, Vec::new())),
            (Err(e), _) => self.flow.failed.push((chunk, e)),
        }
        true
    }

    fn flow_status(&self, wanted: &[Chunk]) -> String {
        // With the dates hidden, a day goes by its weekday and failures (which name it) without it.
        let blind = tickerlight_core::builtin::clock(self.workspace.chart().resources()).blind;
        let short = |chunk: Chunk| {
            // A day by its own date, an hour by its time.
            let t = DateTime::from_time(chunk.start(0));
            let date = if blind {
                t.weekday_name().to_string()
            } else {
                format!("{} {}", t.day, t.month_name())
            };
            match chunk {
                Chunk::Day(_) => date,
                Chunk::Hour(_) => format!("{date} {:02}:00", t.hour),
            }
        };
        let mut parts = Vec::new();
        let chart = self.workspace.chart();
        if self.workspace.ctx.footprint
            && (chart.time_scale().bar_spacing as f32) < Footprint::new(1.0).min_bar_width * chart.scale()
        {
            parts.push("Zoom in to see the footprint".to_string());
        }
        // Bars left of the days loaded show no order flow: said, so it is not taken for broken there.
        let day_start = self.flow.series.as_ref().map_or(0, |s| s.venue.day_start());
        let seen = chart.seen();
        let visible = chart.time_scale().visible(seen.len());
        if (self.workspace.ctx.footprint || self.workspace.ctx.profile)
            && let Some(first) = wanted.first()
            && !visible.is_empty()
            && seen.time()[visible.start] < first.start(day_start)
        {
            parts.push(format!("Order flow covers the last {} days on screen", flow::MAX_DAYS));
        }
        if let Some((chunk, _)) = self.flow.loading {
            let left = wanted
                .iter()
                .filter(|c| !self.flow.chunks.iter().any(|(h, _)| h == *c))
                .filter(|c| !self.flow.failed.iter().any(|(f, _)| f == *c))
                .count();
            let more = if left > 1 {
                format!(", {} more", left - 1)
            } else {
                String::new()
            };
            parts.push(format!("Loading the trades of {}{more}", short(chunk)));
        }
        for (_, e) in self.flow.failed.iter().filter(|(c, _)| wanted.contains(c)) {
            let e = if blind {
                "The trades of some days did not load"
            } else {
                e.as_str()
            };
            if !parts.iter().any(|p| p == e) {
                parts.push(e.to_string());
            }
        }
        parts.join("; ")
    }
}

impl Window {
    /// Loading with nothing on the chart yet: the first start, or a symbol after one that failed.
    fn first_load(&self) -> bool {
        self.loading.is_some() && self.workspace.chart().base().is_empty()
    }

    /// What the loading does and how far it is, in the middle of the empty chart.
    fn loading_card(&self, scene: &mut Scene) {
        let Some((series, _)) = &self.loading else {
            return;
        };
        let theme = self.workspace.theme;
        let scale = self.workspace.chart().scale();
        let px = |v: f32| v * scale;
        let (done, total, downloading) = self.files;
        let mut lines = vec![(
            format!(
                "{} {}",
                if downloading { "Downloading" } else { "Loading" },
                series.symbol
            ),
            theme.text,
        )];
        if downloading {
            lines.push((
                format!(
                    "1-minute candles of the last {} months from the {} public data archive, the newest first.",
                    self.source.months,
                    Venue::EXCHANGES[series.venue.choices().0]
                ),
                theme.muted,
            ));
            lines.push(("Only this once: they are kept on this computer.".into(), theme.muted));
        }
        let size = px(theme.font_size);
        let font = tickerlight_core::Font::ui();
        let text_w = lines
            .iter()
            .map(|(l, _)| font.measure(l, size))
            .fold(px(240.0), f32::max);
        let (pad, row) = (px(16.0), px(theme.row_height));
        let area = self.workspace.chart_area();
        let w = (text_w + 2.0 * pad).min(area.w);
        let h = 2.0 * pad + row * (lines.len() as f32 + 1.0);
        let (x, y) = (area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0);
        scene.next_layer();
        scene.set_clip(Some(area.clip()));
        scene.fill_rounded(x, y, w, h, px(theme.radius) * 2.0, theme.control);
        for (i, (line, color)) in lines.iter().enumerate() {
            let cy = y + pad + row * (i as f32 + 0.5);
            scene.text(x + pad, cy, size, *color, tickerlight_core::Align::Left, line.as_str());
        }
        // The bar of files checked, and the count beside it.
        let cy = y + pad + row * (lines.len() as f32 + 0.5);
        let count = if total > 0 {
            format!("{done} of {total} files")
        } else {
            String::new()
        };
        let count_w = font.measure("000 of 000 files", size);
        let bar_w = (w - 2.0 * pad - count_w - pad).max(0.0);
        let bar_h = px(6.0);
        scene.fill_rounded(x + pad, cy - bar_h / 2.0, bar_w, bar_h, bar_h / 2.0, theme.background);
        if total > 0 {
            let filled = bar_w * done as f32 / total as f32;
            scene.fill_rounded(x + pad, cy - bar_h / 2.0, filled, bar_h, bar_h / 2.0, theme.accent);
        }
        scene.text(
            x + w - pad,
            cy,
            size,
            theme.muted,
            tickerlight_core::Align::Right,
            count,
        );
    }
}

/// The charts of the layout: which is active, what each has besides the chart, one replay for all.
impl Window {
    /// Trades the active chart's state, in the window and its program, for what `parked[i]` holds.
    fn swap_parked(&mut self, i: usize) {
        use std::mem::swap;
        let c = &mut self.parked[i];
        swap(&mut self.loading, &mut c.loading);
        swap(&mut self.flow, &mut c.flow);
        swap(&mut self.saved, &mut c.saved);
        swap(&mut self.saved_at, &mut c.saved_at);
        swap(&mut self.unsaved, &mut c.unsaved);
        swap(&mut self.last_state, &mut c.last_state);
        swap(&mut self.pending, &mut c.pending);
        swap(&mut self.files, &mut c.files);
        swap(&mut self.drawings_saved, &mut c.drawings_saved);
        swap(&mut self.drawings_seen, &mut c.drawings_seen);
        swap(&mut self.second, &mut c.second);
        let p = &mut self.workspace.ctx;
        swap(&mut p.symbol, &mut c.symbol);
        swap(&mut p.venue, &mut c.venue);
        swap(&mut p.shown, &mut c.shown);
        swap(&mut p.status, &mut c.status);
        swap(&mut p.error, &mut c.error);
        swap(&mut p.footprint, &mut c.footprint);
        swap(&mut p.profile, &mut c.profile);
        swap(&mut p.flow_status, &mut c.flow_status);
        swap(&mut p.session, &mut c.session);
    }

    /// Runs `f` with chart `i` active for the while: what the window does for the active chart (loading, the
    /// order flow, saving) it does so for the others.
    fn with_cell<R>(&mut self, i: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        let was = self.workspace.active();
        if i == was {
            return f(self);
        }
        self.swap_parked(was);
        self.workspace.set_active(i);
        self.swap_parked(i);
        let out = f(self);
        self.swap_parked(i);
        self.workspace.set_active(was);
        self.swap_parked(was);
        out
    }

    /// Makes chart `i` the active one: the panels act on it now. A replay playing goes on in it.
    fn activate(&mut self, i: usize) {
        let was = self.workspace.active();
        if i == was || i >= self.workspace.charts() {
            return;
        }
        self.flush();
        let playing = self
            .workspace
            .chart_mut()
            .resources_mut()
            .get_mut::<Replay>()
            .is_some_and(|r| std::mem::take(&mut r.playing));
        self.swap_parked(was);
        self.workspace.set_active(i);
        self.swap_parked(i);
        if playing
            && let Some(replay) = self.workspace.chart_mut().resources_mut().get_mut::<Replay>()
            && replay.active
        {
            replay.playing = true;
        }
    }

    /// Saves the session that waits for its turn (see [`Window::autosave`]) now.
    fn flush(&mut self) {
        if self.unsaved
            && let Some(session) = self.last_state.clone()
        {
            let text = session.to_text();
            self.store(session);
            (self.saved, self.saved_at) = (text, Some(Instant::now()));
        }
        self.unsaved = false;
    }

    /// Shows the charts in `layout`. A chart it adds shows the next of `given`, else what the active one does; the
    /// charts it drops are saved first. The active chart stays (see [`Workspace::set_layout`]).
    fn set_layout(&mut self, layout: Layout, given: Vec<Resumed>) {
        let n = layout.cells();
        let active = self.workspace.active();
        // The charts that go are saved: the cells past the layout's but the active one, which takes the place of
        // the last cell kept.
        let moves = active >= n;
        for i in n - usize::from(moves)..self.workspace.charts() {
            if i != active {
                self.with_cell(i, |w| w.autosave(true));
            }
        }
        if moves {
            self.parked.swap(active, n - 1);
        }
        let shown = (self.workspace.ctx.shown.clone()).or_else(|| self.loading.as_ref().map(|(s, _)| s.clone()));
        let timeframe = self.workspace.chart().timeframe();
        let venue = shown.as_ref().map_or(Venue::Binance(Market::Spot), |s| s.venue);
        let old = self.workspace.charts();
        self.workspace.set_layout(layout, || new_chart(Candles::new(), venue));
        self.parked.truncate(n);
        self.parked.resize_with(n, Parked::default);
        self.workspace.ctx.layout = layout;
        let mut given = given.into_iter();
        for i in old..n {
            let chart = given.next().or_else(|| {
                shown.clone().map(|series| Resumed {
                    series,
                    timeframe,
                    session: None,
                })
            });
            let Some(chart) = chart else { continue };
            self.with_cell(i, |w| {
                let studies = w.studies.clone();
                set_studies(w.workspace.chart_mut(), studies);
                w.resume_chart(&chart);
                let p = &mut w.workspace.ctx;
                (p.symbol, p.venue) = (chart.series.symbol.clone(), chart.series.venue.index());
                w.start(chart.series.clone());
            });
        }
    }

    /// Whether the active chart is in a replay.
    fn replaying(&self) -> bool {
        let res = self.workspace.chart().resources();
        res.get::<Replay>().is_some_and(|r| r.active)
    }

    /// Keys the window takes before the chart, as on TradingView: a letter starts a symbol search with it, a digit
    /// opens the interval box with it, Alt+G the date to go to, "/" the indicators to add. Not while a field or a
    /// list has the keys or a drawing's text is typed. Returns whether it took the key.
    fn shortcut(&mut self, event: Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        let chart = self.workspace.chart();
        let writing = chart.resources().get::<Drawings>().is_some_and(|d| d.editing.is_some());
        if self.workspace.typing() || writing {
            return false;
        }
        let p = &mut self.workspace.ctx;
        match key {
            Key::Char(c) if c.is_alphabetic() => {
                let field = if p.venue() == Venue::Imported {
                    IMPORT_FIELD
                } else {
                    SYMBOL_FIELD
                };
                self.workspace.type_into(field, Some(key))
            }
            Key::Char(c) if c.is_ascii_digit() => {
                p.interval = Some(String::new());
                self.workspace.type_into(INTERVAL, Some(key))
            }
            Key::Alt('g') => self.workspace.type_into(GO_TO, None),
            Key::Char('/') => {
                p.page = Some(Page::Indicators);
                self.show_docks();
                self.workspace.type_into(SEARCH_INDICATORS, None)
            }
            _ => false,
        }
    }

    /// Folds the side column when no page is picked, and shows the player while a replay runs or its start is
    /// chosen. Returns whether either changed.
    fn show_docks(&mut self) -> bool {
        let replay = self.workspace.chart().resources().get::<Replay>();
        let player = replay.is_some_and(|r| r.active || r.picking);
        let column = self.workspace.ctx.page.is_some();
        let before = self.workspace.chart_area();
        self.workspace.show(Dock::Player, player);
        self.workspace.show(Dock::Right, column);
        self.workspace.chart_area() != before
    }

    /// Puts the active chart's replay on the others, so none shows what it hides: started, played to the same
    /// bar and moment, or ended, its dates hidden or not. Returns whether any changed.
    fn sync_replay(&mut self) -> bool {
        let active = self.workspace.active();
        if self.workspace.charts() == 1 || self.workspace.ctx.shown.is_none() {
            return false;
        }
        let chart = self.workspace.chart();
        let on = chart.resources().get::<Replay>().is_some_and(|r| r.active);
        let blind = chart.resources().get::<Replay>().is_some_and(|r| r.active && r.blind);
        let wanted = if on {
            (true, chart.cursor(), chart.moment())
        } else {
            (false, None, None)
        };
        let mut changed = false;
        for i in 0..self.workspace.charts() {
            let chart = self.workspace.chart_at(i);
            let replaying = chart.resources().get::<Replay>().is_some_and(|r| r.active);
            let hidden = chart.resources().get::<Replay>().is_some_and(|r| r.blind);
            // A chart still loading gets the replay when it has come.
            let same = self.parked[i].replay == wanted && replaying == on && hidden == blind;
            if i == active || chart.base().is_empty() || same {
                continue;
            }
            self.parked[i].replay = wanted;
            let chart = self.workspace.chart_at_mut(i);
            chart.set_cursor(wanted.1);
            chart.set_moment(wanted.2);
            if let Some(replay) = chart.resources_mut().get_mut::<Replay>() {
                (replay.active, replay.playing, replay.blind) = (on, false, blind);
            }
            // The handlers see the replay begin or end (pending orders go then).
            chart.handle(Event::Tick { dt: 0.0 });
            changed = true;
        }
        changed
    }
}

/// The sessions: saving the one going on, starting a new one, opening a saved one.
impl Window {
    fn list_sessions(&mut self) {
        match self.sessions.list() {
            Ok(list) => self.workspace.ctx.sessions = list,
            Err(e) => self.save_failed(e),
        }
    }

    fn save_failed(&mut self, e: std::io::Error) {
        let p = &mut self.workspace.ctx;
        (p.status, p.error) = (format!("Sessions folder: {e}"), true);
    }

    /// The session as it is now, or `None` while there is nothing worth keeping.
    fn current(&self) -> Option<Session> {
        let p = &self.workspace.ctx;
        let series = p.shown.as_ref()?;
        let chart = self.workspace.chart();
        let res = chart.resources();
        // A trade under review is not the session: it stays as it was until the review is left.
        if res.get::<Review>().is_some() {
            return self.last_state.clone();
        }
        let account = res.get::<Account>()?;
        let worth = p.session.is_some()
            || chart.cursor().is_some()
            || !account.fills().is_empty()
            || !account.orders().is_empty();
        if !worth {
            return None;
        }
        // A new session is named by when it began, one millisecond after the newest if need be.
        let created = p.session.unwrap_or_else(|| {
            let newest = p.sessions.iter().map(|s| s.created.0 + 1).max().unwrap_or(0);
            Time(now().0.max(newest))
        });
        let mut session = Session::capture(chart, series, created);
        (session.footprint, session.profile) = (p.footprint, p.profile);
        // A session keeps its kind; a new one is live when it begins outside a replay.
        session.live = self
            .last_state
            .as_ref()
            .filter(|s| p.session == Some(s.created))
            .map_or(chart.cursor().is_none(), |s| s.live);
        Some(session)
    }

    /// Saves the session if it changed: at once when `now`, else at most once a second (`needs_tick` brings the
    /// rest).
    fn autosave(&mut self, now: bool) {
        self.last_state = self.current();
        let Some(session) = self.last_state.clone() else {
            self.unsaved = false;
            return;
        };
        let text = session.to_text();
        if text == self.saved {
            self.unsaved = false;
            return;
        }
        self.workspace.ctx.session = Some(session.created);
        if !now && self.saved_at.is_some_and(|t| t.elapsed() < SAVE_EVERY) {
            self.unsaved = true;
            return;
        }
        self.store(session);
        (self.saved, self.saved_at, self.unsaved) = (text, Some(Instant::now()), false);
    }

    /// Takes a picture of the chart for each trade closed since the last look, into the session's folder of
    /// snapshots (`trade-N.png`, the Nth trade closed in the session).
    fn snapshot_trades(&mut self) {
        if let Some(e) = self.snapshots.failure() {
            let p = &mut self.workspace.ctx;
            (p.status, p.error) = (e, true);
        }
        let p = &self.workspace.ctx;
        let res = self.workspace.chart().resources();
        let fills = res.get::<Account>().map_or(&[][..], |a| a.fills());
        let (seen, reviewing) = (self.snapped.1, res.get::<Review>().is_some());
        // Another session (opened, begun, another chart's), a trade under review or just back from one: its fills
        // were not made now.
        if p.session != self.snapped.0 || reviewing || self.snapped.2 || fills.len() < seen {
            let other = p.session != self.snapped.0;
            let dir = other
                .then(|| p.session.zip(p.shown.as_ref()))
                .flatten()
                .map(|(created, series)| self.sessions.snapshots(created, &series.symbol));
            self.snapped = (p.session, fills.len(), reviewing);
            if other {
                self.workspace.ctx.snapshots = dir.filter(|d| d.exists());
            }
            return;
        }
        if fills.len() == seen {
            return;
        }
        let results = Account::results(fills);
        let closed_before = results[..seen].iter().flatten().count();
        let closed_now = results[seen..].iter().flatten().count();
        self.snapped.1 = fills.len();
        let area = self.workspace.chart_area();
        let size = (area.w.round() as u32, area.h.round() as u32);
        let (Some(created), Some(series)) = (p.session, &p.shown) else {
            return;
        };
        if closed_now == 0 || size.0 == 0 || size.1 == 0 {
            return;
        }
        let dir = self.sessions.snapshots(created, &series.symbol);
        self.workspace.ctx.snapshots = Some(dir.clone());
        let mut scene = Scene::new();
        self.workspace.chart_mut().build_scene(&mut scene);
        for n in closed_before + 1..=closed_before + closed_now {
            self.snapshots
                .take(scene.clone(), size, dir.join(format!("trade-{n}.png")));
        }
    }

    /// Writes `session` and puts it in the list.
    fn store(&mut self, session: Session) {
        match self.sessions.save(&session) {
            Ok(()) => {
                let list = &mut self.workspace.ctx.sessions;
                match list.iter_mut().find(|s| s.created == session.created) {
                    Some(saved) => *saved = session,
                    None => list.insert(0, session),
                }
            }
            Err(e) => self.save_failed(e),
        }
    }

    /// Keeps replays and paper trading on live data in sessions of their own: when a replay starts on a session
    /// traded live, or a replay's session goes on outside it (the replay ended), that session is kept as it was and
    /// ended. Back out of a replay, a live chart takes up the newest live session of its symbol.
    fn split_on_mode(&mut self) {
        let p = &self.workspace.ctx;
        let Some(live) = self
            .last_state
            .as_ref()
            .filter(|s| p.session == Some(s.created))
            .map(|s| s.live)
        else {
            return;
        };
        let replaying = self.workspace.chart().cursor().is_some();
        if live != replaying {
            return;
        }
        if let Some(before) = self.last_state.take() {
            self.store(before);
        }
        self.end_session();
        let res = self.workspace.chart_mut().resources_mut();
        if let Some(account) = res.get_mut::<Account>() {
            *account = account.empty_like();
        }
        res.insert(Journal::default());
        if !replaying {
            self.resume_live();
        }
    }

    /// Puts the newest live session of the series shown back on a live chart: its position and orders go on.
    fn resume_live(&mut self) {
        let p = &self.workspace.ctx;
        if !self.workspace.chart().resources().get::<Live>().is_some_and(|l| l.0) {
            return;
        }
        let Some(series) = &p.shown else { return };
        // The list is newest first.
        if let Some(session) = p.sessions.iter().find(|s| s.live && s.series == *series).cloned() {
            self.put(&session);
        }
    }

    /// A replay that starts before the last trade of the session would put that trade in its future. The session
    /// is kept as it was, and the replay begins another one, with the same drawings and no trades.
    fn split_on_rewind(&mut self) {
        let chart = self.workspace.chart();
        let last_fill = chart
            .resources()
            .get::<Account>()
            .and_then(|a| a.fills().last())
            .map(|f| f.time);
        let (Some(cursor), Some(last_fill)) = (chart.cursor(), last_fill) else {
            return;
        };
        if cursor >= last_fill {
            return;
        }
        if let Some(before) = self.last_state.take() {
            self.store(before);
        }
        self.end_session();
        let res = self.workspace.chart_mut().resources_mut();
        if let Some(account) = res.get_mut::<Account>() {
            *account = account.empty_like();
        }
        res.insert(Journal::default());
    }

    /// Forgets the session going on (after saving it): what comes next is another one.
    fn end_session(&mut self) {
        self.workspace.ctx.session = None;
        self.saved.clear();
        self.unsaved = false;
        self.pending = None;
    }

    /// Clears the chart of a session: no replay, no account history, no notes. The drawings stay: they are the
    /// symbol's.
    fn clear(&mut self) {
        let chart = self.workspace.chart_mut();
        chart.set_cursor(None);
        let res = chart.resources_mut();
        if let Some(replay) = res.get_mut::<Replay>() {
            (replay.active, replay.playing) = (false, false);
        }
        if let Some(account) = res.get_mut::<Account>() {
            *account = account.empty_like();
        }
        res.insert(Journal::default());
        res.remove::<Review>();
        chart.handle(Event::Tick { dt: 0.0 });
    }

    /// Puts a session of the series shown on the chart.
    fn put(&mut self, session: &Session) {
        self.clear();
        session.apply(self.workspace.chart_mut());
        // The symbol's drawings, unless it has none yet: then those the session was saved with (sessions kept the
        // drawings before the symbols did).
        if !self.drawings_saved.is_empty() {
            let text = self.drawings_saved.clone();
            let chart = self.workspace.chart_mut();
            let panes = chart.pane_names();
            if let Some(drawings) = chart.resources_mut().get_mut::<Drawings>() {
                drawings.load(&text, &panes);
            }
        }
        let p = &mut self.workspace.ctx;
        // A live session goes on with the chart as it is.
        if !session.live {
            (p.footprint, p.profile) = (session.footprint, session.profile);
        }
        p.session = Some(session.created);
        self.saved = session.to_text();
        (self.saved_at, self.unsaved) = (Some(Instant::now()), false);
    }

    /// Acts on what the sessions page asked for. Returns whether anything changed.
    fn session_requests(&mut self) -> bool {
        if std::mem::take(&mut self.workspace.ctx.export) {
            self.autosave(true);
            let exported = match self.current().map(|s| Sessions::export(&s, &self.exports)) {
                Some(Ok(path)) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    // The home folder as `~`, so the folder fits in the column.
                    let dir = match dirs::home_dir().and_then(|home| self.exports.strip_prefix(home).ok()) {
                        Some(rest) => format!("~/{}", rest.display()),
                        None => self.exports.display().to_string(),
                    };
                    (vec![format!("Saved {name}"), format!("in {dir}")], false)
                }
                Some(Err(e)) => (vec![format!("Not saved: {e}")], true),
                None => (vec!["No trades to export yet".into()], false),
            };
            self.workspace.ctx.exported = Some(exported);
            return true;
        }
        let p = &mut self.workspace.ctx;
        if std::mem::take(&mut p.new_session) {
            self.autosave(true);
            self.end_session();
            self.clear();
            return true;
        }
        let Some(created) = p.open.take() else {
            return false;
        };
        if p.session == Some(created) {
            return false;
        }
        let Some(session) = p.sessions.iter().find(|s| s.created == created).cloned() else {
            return false;
        };
        (p.symbol, p.venue) = (session.series.symbol.clone(), session.series.venue.index());
        if p.shown.as_ref() == Some(&session.series) && self.loading.is_none() {
            self.autosave(true);
            self.end_session();
            self.put(&session);
        } else {
            // Saves and ends the session going on.
            self.start(session.series.clone());
            self.pending = Some(session);
        }
        true
    }
}

impl Surface for Window {
    fn resize(&mut self, width: f32, height: f32) {
        self.workspace.resize(width, height);
    }

    fn set_scale(&mut self, scale: f32) {
        self.workspace.set_scale(scale);
    }

    fn handle(&mut self, event: Event) -> bool {
        let page = self.workspace.ctx.page;
        let replaying = self.replaying();
        let active = self.workspace.active();
        // An order placed by this event counts as placed now.
        let mut redraw = self.tick_clock();
        redraw |= self.shortcut(event) || self.workspace.handle(event);
        // A press made another chart active: its state comes out, the other's is parked.
        let pressed = self.workspace.active();
        if pressed != active {
            self.workspace.set_active(active);
            self.activate(pressed);
        }
        if matches!(event, Event::Tick { .. }) {
            redraw |= self.poll();
            for i in 0..self.workspace.charts() {
                if i != self.workspace.active() {
                    redraw |= self.with_cell(i, |w| {
                        let changed = w.poll() | w.tick_clock() | w.sync_flow();
                        w.split_on_mode();
                        w.autosave(false);
                        changed
                    });
                }
            }
        }
        if self.workspace.ctx.layout != self.workspace.layout() {
            self.set_layout(self.workspace.ctx.layout, Vec::new());
            redraw = true;
        }
        if let Some(series) = self.workspace.ctx.request.take() {
            self.start(series);
            redraw = true;
        }
        if let Some(path) = self.workspace.ctx.import.take() {
            self.start_import(path);
            redraw = true;
        }
        redraw |= self.session_requests();
        // The gear on an indicator's label opens the indicators page.
        if self
            .workspace
            .chart_mut()
            .resources_mut()
            .remove::<OpenStudySettings>()
            .is_some()
        {
            self.workspace.ctx.page = Some(Page::Indicators);
            redraw = true;
        }
        // A replay is for trading: its start shows the order ticket, unless a page that works with the replay
        // (a strategy watched trading, a session) is open.
        let now_replaying = self.replaying();
        let p = &mut self.workspace.ctx;
        if now_replaying && !replaying && matches!(p.page, None | Some(Page::Watchlist)) {
            p.page = Some(Page::Trade);
        }
        redraw |= self.show_docks();
        // Another page: the column fades in with it.
        if self.workspace.ctx.page != page {
            self.workspace.fade_in(Dock::Right);
            redraw = true;
        }
        redraw |= self.sync_replay();
        self.split_on_mode();
        self.split_on_rewind();
        self.autosave(false);
        self.snapshot_trades();
        self.save_drawings();
        self.save_studies();
        // Charts that came since (a symbol, a layout) are shown on the clock chosen too.
        redraw |= self.show_zone();
        redraw |= self.sync_columns();
        self.save_resume();
        redraw |= self.sync_watchlist(event);
        redraw | self.sync_flow()
    }

    fn needs_tick(&self) -> bool {
        let busy = |p: &Parked| p.loading.is_some() || p.flow.loading.is_some() || p.unsaved || p.flow.live.is_some();
        self.workspace.needs_tick()
            || self.loading.is_some()
            || self.flow.loading.is_some()
            || self.unsaved
            || self.workspace.ctx.watch.loading()
            || self.workspace.ctx.watch.live()
            || self.flow.live.is_some()
            || self.parked.iter().any(busy)
    }

    fn animating(&self) -> bool {
        self.workspace.animating()
    }

    fn build_scene(&mut self, scene: &mut Scene) {
        // An order placed on the panels counts as placed now.
        self.tick_clock();
        let card = self.first_load();
        self.workspace.ctx.loading_card = card;
        self.workspace.build_scene(scene);
        if card {
            self.loading_card(scene);
        }
    }

    fn take_copied(&mut self) -> Option<String> {
        self.workspace.take_copied()
    }

    fn takes_text(&self) -> bool {
        self.workspace.takes_text()
    }

    fn pointer_shape(&self) -> tickerlight_core::PointerShape {
        self.workspace.pointer_shape()
    }

    /// The symbol, where it is from and the timeframe: `BTCUSDT Binance Spot 15m`, `ACME File 1h`.
    fn title(&self) -> String {
        let Some(series) = &self.workspace.ctx.shown else {
            return String::new();
        };
        let chart = self.workspace.chart();
        let step = chart.timeframe().or(native_step(chart.base())).unwrap_or(60_000);
        let SeriesTitle { symbol, source } = series_title(series);
        format!("{symbol} {source} {}", span_label(step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::Candle;
    use tickerlight_ui::panels::short_time;

    fn source(name: &str) -> Source {
        let root = std::env::temp_dir().join(format!("chart-program-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Source {
            root,
            months: 1,
            offline: true,
        }
    }

    fn series(symbol: &str) -> Series {
        Series {
            venue: Venue::Binance(Market::Spot),
            symbol: symbol.into(),
            interval: Interval::M1,
        }
    }

    /// Ticks the window until the loading is over.
    fn settle(window: &mut Window) {
        for _ in 0..500 {
            if !window.needs_tick() {
                return;
            }
            window.handle(Event::Tick { dt: 0.01 });
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the loading never ended");
    }

    #[test]
    fn a_symbol_cached_up_to_the_last_days_shows_from_the_cache_at_once() {
        let source = Source {
            offline: false,
            ..source("cached")
        };
        let store = Store::new(&source.root);
        let keep = |symbol: &str, t: i64| {
            let day = DateTime::from_time(Time(t));
            let candle = Candle {
                time: Time(t - t % 60_000),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            };
            // The cache has whole days up to yesterday: today is not in the archive yet.
            let period = Period::Day {
                year: day.year,
                month: day.month,
                day: day.day,
            };
            store.write(&series(symbol), period, &[candle]).unwrap();
        };
        let hour = 3_600_000;
        keep("ETHUSDT", now().0 - 24 * hour);
        keep("SOLUSDT", now().0 - 120 * hour);
        assert!(cached(&source, &series("ETHUSDT"), now()).is_some());
        assert!(
            cached(&source, &series("SOLUSDT"), now()).is_none(),
            "days behind: the archive first"
        );
        let offline = Source {
            offline: true,
            ..source.clone()
        };
        assert!(
            cached(&offline, &series("ETHUSDT"), now()).is_none(),
            "no live candles to bring the rest"
        );
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn a_series_loads_in_the_background_and_replaces_the_chart() {
        let source = source("loads");
        // The month before this one is cached.
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..90)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();

        let mut window = Window::new(source.clone(), series("NOPEUSDT"), None);
        window.resize(1200.0, 700.0);
        // While it loads, the window shows an empty chart with what it does in the middle, not in the status.
        let mut scene = Scene::new();
        window.build_scene(&mut scene);
        let loading: Vec<_> = scene.texts.iter().filter(|t| t.text == "Loading NOPEUSDT").collect();
        let area = window.workspace().chart_area();
        assert_eq!(loading.len(), 1);
        assert!(loading[0].y > area.y + area.h / 3.0, "{} {area:?}", loading[0].y);
        assert!(window.needs_tick());
        settle(&mut window);
        let p = &window.workspace().ctx;
        assert!(
            p.error && p.status.contains("NOPEUSDT Binance spot is not on this computer"),
            "{}",
            p.status
        );
        assert_eq!(window.workspace().chart().base().len(), 0);

        // The symbol panel asks for another symbol.
        window.workspace_mut().ctx.symbol = "ethusdt".into();
        open(&mut window);
        settle(&mut window);
        let p = &window.workspace().ctx;
        assert!(!p.error, "{}", p.status);
        assert_eq!(p.shown, Some(series("ETHUSDT")));
        assert_eq!(window.workspace().chart().base().len(), 90);
        window.build_scene(&mut Scene::new());

        // A replay with a trade, then a symbol that is not there: the replay and the trade stay as they were.
        start_replay(&mut window);
        click(&mut window, "Buy 95.24 at market");
        let session = window.workspace().ctx.session;
        assert!(session.is_some());
        window.workspace_mut().ctx.symbol = "nopeusdt".into();
        open(&mut window);
        settle(&mut window);
        let p = &window.workspace().ctx;
        assert!(p.error && p.status.contains("NOPEUSDT"), "{}", p.status);
        assert_eq!((p.shown.clone(), p.session), (Some(series("ETHUSDT")), session));
        let chart = window.workspace().chart();
        assert!(chart.cursor().is_some());
        assert_eq!(chart.resources().get::<Account>().unwrap().fills().len(), 1);
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn a_symbol_typed_in_opens() {
        let source = source("typed");
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        click(&mut window, "Symbol");
        window.handle(Event::Key(Key::SelectAll));
        for c in "solusdt".chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().ctx.symbol, "SOLUSDT");
        // Another market shows the symbol there at once.
        click(&mut window, "Futures");
        assert!(
            window.workspace().ctx.status.contains("SOLUSDT"),
            "{}",
            window.workspace().ctx.status
        );
        settle(&mut window);
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn escape_in_the_symbol_field_brings_back_the_symbol_shown() {
        let source = source("escaped");
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        settle(&mut window);
        // No data here to load: the chart is taken to show it.
        window.workspace_mut().ctx.shown = Some(series("ETHUSDT"));
        window.build_scene(&mut Scene::new());
        click(&mut window, "Symbol");
        window.handle(Event::Key(Key::SelectAll));
        for c in "sol".chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().ctx.watch.search, "SOL");
        window.handle(Event::Key(Key::Escape));
        window.build_scene(&mut Scene::new());
        let p = &window.workspace().ctx;
        assert_eq!((p.symbol.as_str(), p.watch.search.as_str()), ("ETHUSDT", ""));
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn journal_columns_typed_on_the_sessions_page_are_kept_for_every_session() {
        let source = source("columns");
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        click(&mut window, "Sessions");
        click(&mut window, "Grade: A, B, C; Mistake: early, late");
        for c in "Grade: A, B".chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.build_scene(&mut Scene::new());
        let typed = JournalColumns::parse("Grade: A, B");
        let on_chart = window.workspace().chart().resources().get::<JournalColumns>().cloned();
        assert_eq!(on_chart.as_ref(), Some(&typed));
        drop(window);
        let text = std::fs::read_to_string(source.root.join("journal.txt")).unwrap();
        assert_eq!(text, "Grade: A, B\n");

        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().ctx.columns_text, "Grade: A, B");
        let on_chart = window.workspace().chart().resources().get::<JournalColumns>().cloned();
        assert_eq!(on_chart, Some(typed), "the next start has them");
    }

    #[test]
    fn the_watchlist_sorts_by_a_column_and_a_row_comes_off() {
        let source = source("sort");
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        let ticker = |symbol: &str, last: f64, change: f64| tickerlight_binance::Ticker {
            symbol: symbol.into(),
            last,
            change,
            quote_volume: 1.0,
            close_time: Time(0),
        };
        window.workspace_mut().ctx.watch.set_tickers(
            Venue::Binance(Market::Spot),
            vec![
                ticker("BTCUSDT", 80000.0, 1.0),
                ticker("ETHUSDT", 2500.0, 3.0),
                ticker("SOLUSDT", 150.0, -2.0),
            ],
        );
        window.workspace_mut().ctx.watch.items = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
            .map(|s| (Venue::Binance(Market::Spot), s.to_string()))
            .to_vec();
        window.build_scene(&mut Scene::new());
        click(&mut window, "Watchlist");
        let order = |w: &Window| -> Vec<String> {
            let rects = w.workspace().ui_state().rects();
            let mut rows: Vec<(f32, String)> = rects
                .iter()
                .filter(|(t, _)| t.ends_with("USDT") && !t.contains(' '))
                .map(|(t, a)| (a.y, t.clone()))
                .collect();
            rows.sort_by(|a, b| a.0.total_cmp(&b.0));
            rows.into_iter().map(|r| r.1).collect()
        };
        assert_eq!(order(&window), ["BTCUSDT", "ETHUSDT", "SOLUSDT"], "as listed");
        click(&mut window, "24h");
        assert_eq!(
            order(&window),
            ["ETHUSDT", "BTCUSDT", "SOLUSDT"],
            "the biggest gain first"
        );
        click(&mut window, "24h");
        assert_eq!(order(&window), ["SOLUSDT", "BTCUSDT", "ETHUSDT"]);
        click(&mut window, "24h");
        assert_eq!(order(&window), ["BTCUSDT", "ETHUSDT", "SOLUSDT"], "and as listed again");
        click(&mut window, "Last");
        assert_eq!(order(&window), ["BTCUSDT", "ETHUSDT", "SOLUSDT"]);

        // The mark at the end of a row takes it off the list, and opens nothing.
        click(&mut window, "Remove row ETHUSDT");
        assert!(
            !window
                .workspace()
                .ctx
                .watch
                .contains(Venue::Binance(Market::Spot), "ETHUSDT")
        );
        assert_eq!(order(&window), ["BTCUSDT", "SOLUSDT"]);
        assert!(window.workspace().ctx.request.is_none());
        drop(window);
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn typing_a_symbol_lists_matches_and_a_click_opens_one() {
        let source = source("search");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        for symbol in ["ETHUSDT", "SOLUSDT"] {
            store
                .write(&series(symbol), Period::Month { year, month }, &candles)
                .unwrap();
        }
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        settle(&mut window);
        let ticker = |symbol: &str, last: f64, change: f64| tickerlight_binance::Ticker {
            symbol: symbol.into(),
            last,
            change,
            quote_volume: 1.0,
            close_time: Time(0),
        };
        window.workspace_mut().ctx.watch.set_tickers(
            Venue::Binance(Market::Spot),
            vec![
                ticker("ETHUSDT", 2500.0, 1.5),
                ticker("SOLBTC", 0.002, -2.0),
                ticker("SOLUSDT", 150.25, -3.25),
            ],
        );
        window.build_scene(&mut Scene::new());
        click(&mut window, "Symbol");
        window.handle(Event::Key(Key::SelectAll));
        for c in "sol".chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().ctx.page, Some(Page::Watchlist));
        let rects = window.workspace().ui_state().rects();
        let has = |t: &str| rects.iter().any(|(r, _)| r == t);
        assert!(has("SOLBTC") && has("SOLUSDT") && !has("ETHUSDT"), "{rects:?}");

        click(&mut window, "SOLUSDT");
        settle(&mut window);
        assert_eq!(window.workspace().ctx.shown, Some(series("SOLUSDT")));
        window.build_scene(&mut Scene::new());
        // It is one of those a first start watches: taken off, then put back at the end.
        click(&mut window, "Remove SOLUSDT");
        assert!(
            !window
                .workspace()
                .ctx
                .watch
                .contains(Venue::Binance(Market::Spot), "SOLUSDT")
        );
        click(&mut window, "Add SOLUSDT");
        drop(window);
        let text = std::fs::read_to_string(source.root.join("watchlist.txt")).unwrap();
        assert!(
            text.starts_with("spot BTCUSDT\n") && text.ends_with("spot SOLUSDT\n"),
            "{text}"
        );
        std::fs::remove_dir_all(&source.root).ok();
    }

    /// A replay started the way the Replay button's command starts it: 70% across the bars on screen.
    fn start_replay(window: &mut Window) {
        window.handle(Event::Command(tickerlight_core::plugin::commands::REPLAY_TOGGLE));
        window.build_scene(&mut Scene::new());
    }

    fn click(window: &mut Window, text: &str) {
        let (x, y) = window.workspace().ui_state().rect_of(text).unwrap().center();
        window.handle(Event::PointerMoved { x, y });
        window.handle(Event::PointerDown);
        window.handle(Event::PointerUp);
        window.build_scene(&mut Scene::new());
    }

    /// Opens the symbol typed, as Enter in the symbol field does.
    fn open(window: &mut Window) {
        click(window, "Symbol");
        window.handle(Event::Key(Key::Enter));
        window.build_scene(&mut Scene::new());
    }

    #[test]
    fn the_footprint_loads_the_trades_of_the_days_on_screen() {
        let source = source("flow");
        let (year, month) = sync::months_before(now(), 1);
        let first = Period::Day { year, month, day: 1 };
        let second = Period::Day { year, month, day: 2 };
        // Bars from 23:00 of the 1st to 00:29 of the 2nd, and trades of the 1st only.
        let start = first.start().0 + 23 * 3_600_000;
        let candles: Vec<Candle> = (0..90)
            .map(|i| Candle {
                time: Time(start + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let trades: Vec<Trade> = (0..60)
            .map(|i| Trade {
                time: Time(start + i * 60_000 + 1),
                price: 10.0 + (i % 3) as f64 * 0.01,
                qty: 1.0,
                side: tickerlight_core::Side::Buy,
            })
            .collect();
        let store = Store::new(&source.root);
        store
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        store.write_trades(&series("ETHUSDT"), first, &trades).unwrap();

        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().chart().seen_trades().len(), 0);

        click(&mut window, "Footprint");
        assert!(window.workspace().chart().layer_names().contains(&FOOTPRINT));
        // The bars were widened for the numbers; zoomed out again, both days are on screen.
        let footprint = Footprint::new(1.0).min_bar_width as f64;
        assert!(window.workspace().chart().time_scale().bar_spacing >= footprint);
        let area = window.workspace().chart_area();
        let (x, y) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
        window.handle(Event::PointerMoved { x, y });
        for _ in 0..20 {
            window.handle(Event::Wheel { x, delta: -1.0 });
        }
        window.handle(Event::PointerLeft);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().chart().seen_trades().len(), 60);
        let status = &window.workspace().ctx.flow_status;
        assert!(
            status.starts_with("Zoom in to see the footprint; ") && status.contains("not cached"),
            "{status}"
        );
        assert!(window.flow.chunks.iter().all(|(c, _)| *c == Chunk::Day(first)));
        assert!(window.flow.failed.iter().any(|(c, _)| *c == Chunk::Day(second)));

        // The profile shares the trades; with both off they are let go.
        click(&mut window, "Profile");
        window.build_scene(&mut Scene::new());
        assert!(window.workspace().chart().layer_names().contains(&PROFILE));
        click(&mut window, "Footprint");
        click(&mut window, "Profile");
        let names = window.workspace().chart().layer_names();
        assert!(!names.contains(&FOOTPRINT) && !names.contains(&PROFILE));
        assert_eq!(window.workspace().chart().seen_trades().len(), 0);
        assert!(window.workspace().ctx.flow_status.is_empty());
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn the_status_says_when_the_screen_reaches_past_the_days_of_order_flow() {
        let source = source("flow-days");
        let (year, month) = sync::months_before(now(), 1);
        // Five days of hours: more than the order flow loads.
        let start = Period::Month { year, month }.start().0;
        let candles: Vec<Candle> = (0..5 * 24)
            .map(|i| Candle {
                time: Time(start + i * 3_600_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        store
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1200.0, 700.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        click(&mut window, "Footprint");
        let show = |window: &mut Window, bar_spacing: f64| {
            let chart = window.workspace_mut().chart_mut();
            let mut time = *chart.time_scale();
            time.bar_spacing = bar_spacing;
            time.right = candles.len() as f64;
            chart.set_time_scale(time);
            // As a wheel would have: an event lets the program see the new view.
            window.handle(Event::PointerLeft);
            settle(window);
            window.build_scene(&mut Scene::new());
            window.workspace().ctx.flow_status.clone()
        };
        let partly = "Order flow covers the last 3 days on screen";
        let status = show(&mut window, 5.0);
        assert!(status.contains(partly), "all five days on screen: {status}");
        let status = show(&mut window, 80.0);
        assert!(!status.contains(partly), "the last day only: {status}");
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn a_replay_started_before_the_last_trade_begins_another_session() {
        let source = source("rewind");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());

        // A trade in a replay, then out of it.
        start_replay(&mut window);
        click(&mut window, "Buy 95.24 at market");
        let traded = window.workspace().chart().cursor().unwrap();
        let first = window.workspace().ctx.session.unwrap();
        click(&mut window, "Exit replay");
        // Out of the replay nothing trades.
        assert!(
            window
                .workspace()
                .ui_state()
                .rect_of("To trade, start a bar replay: Replay in the top bar")
                .is_some()
        );

        // A replay from before the trade: the trade stays with the first session, this is another.
        let date = tickerlight_core::calendar::DateTime::from_time(Time(traded.0 - 30 * 60_000));
        let typed = format!(
            "{}-{:02}-{:02} {:02}:{:02}",
            date.year, date.month, date.day, date.hour, date.minute
        );
        click(&mut window, "Replay");
        click(&mut window, "Start date");
        for c in typed.chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.handle(Event::Key(Key::Enter));
        window.build_scene(&mut Scene::new());
        let chart = window.workspace().chart();
        assert!(chart.cursor().unwrap() < traded);
        assert!(chart.resources().get::<Account>().unwrap().fills().is_empty());
        let p = &window.workspace().ctx;
        assert_ne!(p.session, Some(first));
        let kept = p
            .sessions
            .iter()
            .find(|s| s.created == first)
            .expect("the first session is kept");
        assert_eq!(kept.fills.len(), 1);
        assert!(kept.cursor.is_none_or(|c| c >= traded));
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn paper_trades_on_a_live_chart_keep_a_session_of_their_own() {
        let source = source("paper");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        // Offline in a test: the chart is made live by hand.
        window.workspace_mut().chart_mut().resources_mut().insert(Live(true));
        window.build_scene(&mut Scene::new());
        let account = |w: &Window| w.workspace().chart().resources().get::<Account>().unwrap().clone();

        // A trade on the live chart is a live session.
        click(&mut window, "Trade");
        click(&mut window, "Buy 95.24 at market");
        assert_eq!(account(&window).position(), 95.24);
        let paper = window.workspace().ctx.session.expect("saved at the first change");

        // A replay is another session: the live one is kept, its position out of the replay's way.
        start_replay(&mut window);
        window.handle(Event::Tick { dt: 0.0 });
        assert!(account(&window).fills().is_empty());
        click(&mut window, "Sell");
        click(&mut window, "Sell 95.24 at market");
        let replay = window.workspace().ctx.session.unwrap();
        assert_ne!(replay, paper);

        // Out of the replay, the live session goes on where it was.
        click(&mut window, "Exit replay");
        assert_eq!(window.workspace().ctx.session, Some(paper));
        assert_eq!(account(&window).position(), 95.24);
        settle(&mut window);
        let saved = Sessions::new(&source.root).list().unwrap();
        let kinds: Vec<(Time, bool, usize)> = saved.iter().map(|s| (s.created, s.live, s.fills.len())).collect();
        assert_eq!(kinds, [(replay, false, 1), (paper, true, 1)]);
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn the_program_starts_where_it_was_left() {
        let source = source("resume");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        for symbol in ["ETHUSDT", "SOLUSDT"] {
            store
                .write(&series(symbol), Period::Month { year, month }, &candles)
                .unwrap();
        }
        // The first start shows the default symbol.
        let first = Window::resumed(source.clone(), None);
        assert_eq!(first.workspace().ctx.symbol, DEFAULT_SYMBOL);
        drop(first);

        let mut window = Window::resumed(source.clone(), Some(series("ETHUSDT")));
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        click(&mut window, "15m");
        start_replay(&mut window);
        click(&mut window, "Step");
        settle(&mut window);
        let session = window.workspace().ctx.session;
        let cursor = window.workspace().chart().cursor();
        assert!(session.is_some() && cursor.is_some());
        drop(window);

        let mut window = Window::resumed(source.clone(), None);
        settle(&mut window);
        let (p, chart) = (&window.workspace().ctx, window.workspace().chart());
        assert_eq!(p.shown, Some(series("ETHUSDT")));
        assert_eq!((p.session, chart.cursor()), (session, cursor));
        assert_eq!(chart.timeframe(), Some(900_000));
        assert_eq!(window.title(), "ETHUSDT Binance Spot 15m");

        // Another symbol keeps the timeframe and begins no session; that is where the next start is.
        window.workspace_mut().ctx.request = Some(series("SOLUSDT"));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(window.workspace().chart().timeframe(), Some(900_000));
        drop(window);
        let mut window = Window::resumed(source.clone(), None);
        settle(&mut window);
        let (p, chart) = (&window.workspace().ctx, window.workspace().chart());
        assert_eq!((p.shown.clone(), p.session), (Some(series("SOLUSDT")), None));
        assert_eq!((chart.timeframe(), chart.cursor()), (Some(900_000), None));
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn two_charts_keep_their_symbols_replay_as_one_and_come_back() {
        let source = source("layout");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        for symbol in ["ETHUSDT", "SOLUSDT"] {
            store
                .write(&series(symbol), Period::Month { year, month }, &candles)
                .unwrap();
        }
        let mut window = Window::resumed(source.clone(), Some(series("ETHUSDT")));
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        // The new chart shows what the first does.
        click(&mut window, LAYOUTS);
        click(&mut window, "Two charts side by side");
        settle(&mut window);
        let w = window.workspace();
        assert_eq!((w.charts(), w.active()), (2, 0));
        assert_eq!(w.chart_at(1).base().len(), 300);
        // A press on the second makes it the one the symbol field acts on.
        let first = w.chart_area();
        let (x, y) = (first.x + first.w + 40.0, first.y + first.h / 2.0);
        window.handle(Event::PointerMoved { x, y });
        window.handle(Event::PointerDown);
        window.handle(Event::PointerUp);
        assert_eq!(window.workspace().active(), 1);
        window.workspace_mut().ctx.request = Some(series("SOLUSDT"));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(window.workspace().ctx.shown, Some(series("SOLUSDT")));
        assert_eq!(window.parked[0].shown, Some(series("ETHUSDT")));
        // A replay in one is a replay in both, at the same bar.
        start_replay(&mut window);
        window.handle(Event::Tick { dt: 0.0 });
        click(&mut window, "Step");
        let cursor = |w: &Window, i| w.workspace().chart_at(i).cursor();
        assert!(cursor(&window, 1).is_some());
        assert_eq!(cursor(&window, 0), cursor(&window, 1));
        // The time zone picked is every chart's.
        click(&mut window, "UTC");
        click(&mut window, "New York");
        let zones = |w: &Window| {
            (0..w.workspace().charts())
                .map(|i| w.workspace().chart_at(i).resources().get::<Zone>().copied())
                .collect::<Vec<_>>()
        };
        assert_eq!(zones(&window), [Some(Zone::NewYork); 2]);
        // A random start hides the dates of both, until they are asked for.
        let blind = |w: &Window| {
            (0..2)
                .map(|i| tickerlight_core::builtin::clock(w.workspace().chart_at(i).resources()).blind)
                .collect::<Vec<_>>()
        };
        click(&mut window, "Random start");
        window.handle(Event::Tick { dt: 0.0 });
        assert_eq!(blind(&window), [true; 2]);
        assert_eq!(cursor(&window, 0), cursor(&window, 1));
        click(&mut window, "Show the date");
        window.handle(Event::Tick { dt: 0.0 });
        assert_eq!(blind(&window), [false; 2]);
        drop(window);

        let mut window = Window::resumed(source.clone(), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.handle(Event::Tick { dt: 0.0 });
        window.build_scene(&mut Scene::new());
        let w = window.workspace();
        assert_eq!((w.layout(), w.active()), (Layout::Columns, 1));
        assert_eq!(w.ctx.shown, Some(series("SOLUSDT")));
        assert_eq!(window.parked[0].shown, Some(series("ETHUSDT")));
        assert!(cursor(&window, 1).is_some());
        assert_eq!(cursor(&window, 0), cursor(&window, 1));
        assert_eq!(zones(&window), [Some(Zone::NewYork); 2], "the zone too");
        // Back to one chart: the active one stays.
        click(&mut window, LAYOUTS);
        click(&mut window, "One chart");
        assert_eq!(window.workspace().charts(), 1);
        assert_eq!(window.workspace().ctx.shown, Some(series("SOLUSDT")));
        // A chart that comes with a layout shows the zone at once.
        click(&mut window, LAYOUTS);
        click(&mut window, "Two charts side by side");
        assert_eq!(zones(&window), [Some(Zone::NewYork); 2]);
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn a_csv_file_is_imported_replayed_and_there_after_a_restart() {
        // Not offline: an imported symbol needs no network all the same.
        let source = Source {
            offline: false,
            ..source("import")
        };
        let start = tickerlight_core::calendar::time_of_date(2024, 3, 1).0;
        let mut csv = String::from("Date,Open,High,Low,Close,Volume\n");
        for i in 0..300 {
            let t = DateTime::from_time(Time(start + i * 3_600_000));
            let close = 50.0 + (i % 11) as f64;
            csv += &format!(
                "{}-{:02}-{:02} {:02}:00,{close},{},{},{close},100\n",
                t.year,
                t.month,
                t.day,
                t.hour,
                close + 1.0,
                close - 1.0
            );
        }
        std::fs::create_dir_all(&source.root).unwrap();
        let file = source.root.join("acme.csv");
        std::fs::write(&file, csv).unwrap();
        let imported = |symbol: &str, interval| Series {
            venue: Venue::Imported,
            symbol: symbol.into(),
            interval,
        };

        let mut window = Window::new(source.clone(), imported("NOTHING", Interval::M1), None);
        // Nothing watched: the watchlist would ask the exchanges for its prices.
        window.workspace_mut().ctx.watch.items.clear();
        window.resize(1400.0, 800.0);
        settle(&mut window);
        let p = &window.workspace().ctx;
        assert_eq!(
            (p.status.as_str(), p.error),
            ("NOTHING was not imported: type the path of a CSV file", true)
        );
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().ctx.venue, Venue::Imported.index());
        click(&mut window, "CSV file or symbol");
        window.handle(Event::Key(Key::SelectAll));
        for c in file.display().to_string().chars() {
            window.handle(Event::Key(Key::Char(c)));
        }
        window.handle(Event::Key(Key::Enter));
        settle(&mut window);
        let acme = imported("ACME", Interval::H1);
        let p = &window.workspace().ctx;
        assert_eq!(
            (p.shown.as_ref(), p.symbol.as_str(), p.error),
            (Some(&acme), "ACME", false)
        );
        assert_eq!(window.workspace().chart().base().len(), 300);
        assert_eq!(window.title(), "ACME File 1h");
        let fee = window
            .workspace()
            .chart()
            .resources()
            .get::<Account>()
            .unwrap()
            .fee_rate;
        assert_eq!(fee, SPOT_FEE);

        window.build_scene(&mut Scene::new());
        start_replay(&mut window);
        click(&mut window, "Step");
        click(&mut window, "Step");
        settle(&mut window);
        let session = window.workspace().ctx.session;
        let cursor = window.workspace().chart().cursor();
        assert!(session.is_some() && cursor.is_some());
        drop(window);

        // A restart opens it again from the cache, the file gone, where the replay was.
        std::fs::remove_file(&file).unwrap();
        let mut window = Window::resumed(source.clone(), None);
        window.workspace_mut().ctx.watch.items.clear();
        settle(&mut window);
        let (p, chart) = (&window.workspace().ctx, window.workspace().chart());
        assert_eq!(p.shown.as_ref(), Some(&acme));
        assert_eq!((p.session, chart.cursor()), (session, cursor));

        // By name, without the interval: the one it was imported at.
        window.workspace_mut().ctx.request = Some(imported("ACME", Interval::M1));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(window.workspace().ctx.shown.as_ref(), Some(&acme));
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn drawings_belong_to_the_symbol() {
        let source = source("drawings");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0 + (i % 5) as f64 * 0.1,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        for symbol in ["ETHUSDT", "SOLUSDT"] {
            store
                .write(&series(symbol), Period::Month { year, month }, &candles)
                .unwrap();
        }
        let drawn = |w: &Window| w.workspace().chart().resources().get::<Drawings>().unwrap().items.len();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        click(&mut window, "Horizontal line");
        let area = window.workspace().chart_area();
        let (x, y) = (area.x + area.w * 0.3, area.y + area.h * 0.5);
        window.handle(Event::PointerMoved { x, y });
        window.handle(Event::PointerDown);
        window.handle(Event::PointerUp);
        assert_eq!(drawn(&window), 1);
        assert!(window.workspace().ctx.session.is_none(), "a drawing is no session");

        // Another symbol has its own (none), and a new session keeps them.
        window.workspace_mut().ctx.request = Some(series("SOLUSDT"));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(drawn(&window), 0);
        window.workspace_mut().ctx.request = Some(series("ETHUSDT"));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(drawn(&window), 1);
        window.workspace_mut().ctx.new_session = true;
        window.handle(Event::Tick { dt: 0.0 });
        assert_eq!(drawn(&window), 1);
        drop(window);

        let mut window = Window::resumed(source.clone(), None);
        settle(&mut window);
        assert_eq!(window.workspace().ctx.shown, Some(series("ETHUSDT")));
        assert_eq!(drawn(&window), 1, "and they are there after a restart");
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn letters_digits_and_alt_g_over_the_chart_open_the_symbol_search_the_interval_box_and_the_date() {
        let source = source("shortcuts");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..600)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.0,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.workspace_mut().ctx.page = None;
        window.build_scene(&mut Scene::new());
        let type_keys = |window: &mut Window, keys: &str| {
            for c in keys.chars() {
                window.handle(Event::Key(Key::Char(c)));
            }
        };
        // A letter: the symbol field takes it in place of the symbol, the watchlist lists what it finds.
        type_keys(&mut window, "so");
        let p = &window.workspace().ctx;
        assert_eq!((p.symbol.as_str(), p.page), ("SO", Some(Page::Watchlist)));
        window.handle(Event::Key(Key::Escape));
        assert_eq!(window.workspace().ctx.symbol, "ETHUSDT", "Escape gives it back");
        // Digits: the interval box; Enter shows bars that long, and closes it.
        type_keys(&mut window, "15");
        assert_eq!(window.workspace().ctx.interval.as_deref(), Some("15"));
        window.handle(Event::Key(Key::Enter));
        window.build_scene(&mut Scene::new());
        assert_eq!(window.workspace().chart().timeframe(), Some(15 * 60_000));
        assert_eq!(window.workspace().ctx.interval, None);
        type_keys(&mut window, "1m");
        window.handle(Event::Key(Key::Enter));
        assert_eq!(window.workspace().chart().timeframe(), None, "the data's own bars");
        type_keys(&mut window, "7m");
        window.handle(Event::Key(Key::Enter));
        assert_eq!(window.workspace().chart().timeframe(), Some(7 * 60_000));
        type_keys(&mut window, "2x");
        window.handle(Event::Key(Key::Enter));
        assert!(window.workspace().ctx.error, "not an interval");
        assert_eq!(window.workspace().chart().timeframe(), Some(7 * 60_000));
        // Alt+G: the date to go to takes the keys.
        window.handle(Event::Key(Key::Alt('g')));
        type_keys(&mut window, "x");
        assert!(window.workspace().typing());
        std::fs::remove_dir_all(&source.root).ok();
    }

    #[test]
    fn indicators_stay_through_another_symbol_and_a_restart() {
        let source = source("indicators");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.0 + (i % 7) as f64 * 0.1,
                volume: 1.0,
            })
            .collect();
        let store = Store::new(&source.root);
        for symbol in ["ETHUSDT", "SOLUSDT"] {
            store
                .write(&series(symbol), Period::Month { year, month }, &candles)
                .unwrap();
        }
        let labels = |w: &Window| -> Vec<String> { studies(w.workspace().chart()).iter().map(|s| s.label()).collect() };
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        // "/" opens the list to add from, its search taking the keys.
        for name in ["Relative strength index", "Bollinger bands"] {
            window.handle(Event::Key(Key::Char('/')));
            window.build_scene(&mut Scene::new());
            assert_eq!(window.workspace().ctx.page, Some(Page::Indicators));
            assert!(window.workspace().typing());
            click(&mut window, name);
        }
        assert_eq!(labels(&window), ["RSI 14", "BB 20 2"]);
        assert!(window.workspace().chart().pane_names().contains(&"rsi-14"));
        // The gear on a label on the chart opens the page again.
        click(&mut window, "Indicators");
        assert_eq!(window.workspace().ctx.page, None, "the column folds");
        let mut scene = Scene::new();
        window.build_scene(&mut scene);
        let bb = scene.texts.iter().find(|t| t.text == "BB 20 2").unwrap();
        let gear = (
            bb.x + tickerlight_core::Font::ui().measure("BB 20 2", bb.size) + 4.0 + 18.0 + 9.0,
            bb.y,
        );
        window.handle(Event::PointerMoved { x: gear.0, y: gear.1 });
        window.handle(Event::PointerDown);
        window.handle(Event::PointerUp);
        assert_eq!(window.workspace().ctx.page, Some(Page::Indicators));

        window.workspace_mut().ctx.request = Some(series("SOLUSDT"));
        window.handle(Event::Tick { dt: 0.0 });
        settle(&mut window);
        assert_eq!(window.workspace().ctx.shown, Some(series("SOLUSDT")));
        assert_eq!(labels(&window), ["RSI 14", "BB 20 2"], "the same on another symbol");
        drop(window);

        let mut window = Window::resumed(source.clone(), None);
        settle(&mut window);
        assert_eq!(labels(&window), ["RSI 14", "BB 20 2"], "and after a restart");
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn the_strategy_page_is_kept_through_a_restart() {
        let source = source("strategy");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| {
                let close = 10.0 + (i as f64 / 9.0).sin();
                Candle {
                    time: Time(start.0 + i * 60_000),
                    open: close,
                    high: close + 0.1,
                    low: close - 0.1,
                    close,
                    volume: 1.0,
                }
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let run = |w: &Window| {
            w.workspace()
                .chart()
                .resources()
                .get::<tickerlight_core::builtin::StrategyRun>()
                .map(|r| r.name.clone())
        };
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        click(&mut window, "Strategy");
        click(&mut window, "Run on the chart");
        assert_eq!(run(&window).as_deref(), Some("Moving average cross 10/30"));
        assert!(source.root.join("strategy.txt").exists(), "written as it changes");
        drop(window);

        let mut window = Window::resumed(source.clone(), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        assert_eq!(
            run(&window).as_deref(),
            Some("Moving average cross 10/30"),
            "on the chart again after a restart"
        );
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn sessions_are_saved_as_they_go_and_open_again() {
        let source = source("sessions");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0 + (i % 7) as f64 * 0.1,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        // Looking at a chart is not a session.
        assert!(window.workspace().ctx.session.is_none());

        start_replay(&mut window);
        // An order is worth 1000 by default: 95.24 at the close of 10.5.
        click(&mut window, "Buy 95.24 at market");
        click(&mut window, "Step");
        let first = window.workspace().ctx.session.expect("saved at the first change");
        let cursor = window.workspace().chart().cursor();
        assert!(cursor.is_some());
        // A change right after waits a moment, and the window ticks until it is written.
        click(&mut window, "Step");
        assert!(window.unsaved && window.needs_tick());
        settle(&mut window);
        let saved = Sessions::new(&source.root).list().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].cursor, window.workspace().chart().cursor());

        // The column scrolled down to the trades; the sessions page opens at its top.
        window.resize(1400.0, 400.0);
        window.build_scene(&mut Scene::new());
        let top = window.workspace().ui_state().rect_of("Bar replay").unwrap().y;
        let (x, y) = window.workspace().ui_state().rect_of("Order").unwrap().center();
        window.handle(Event::PointerMoved { x, y });
        for _ in 0..10 {
            window.handle(Event::Wheel { x, delta: -1.0 });
        }
        window.build_scene(&mut Scene::new());
        let scrolled = window.workspace().ui_state().rect_of("Order").unwrap().y;
        assert!(scrolled < y - 100.0, "{scrolled} {y}");
        click(&mut window, "Sessions");
        assert_eq!(window.workspace().ui_state().rect_of("This session").unwrap().y, top);
        window.resize(1400.0, 800.0);
        window.build_scene(&mut Scene::new());
        click(&mut window, "New session");
        let chart = window.workspace().chart();
        assert!(chart.cursor().is_none());
        assert!(chart.resources().get::<Account>().unwrap().fills().is_empty());
        start_replay(&mut window);
        click(&mut window, "Trade");
        click(&mut window, "Sell");
        click(&mut window, "Sell 95.24 at market");
        let second = window.workspace().ctx.session.unwrap();
        assert!(second > first);
        settle(&mut window);
        assert_eq!(window.workspace().ctx.sessions.len(), 2);

        // The older session is the second row of the list.
        click(&mut window, "Sessions");
        window.build_scene(&mut Scene::new());
        // Rows are named by their start, to the minute: the same for both, unless a minute turned in between.
        let rows = [
            short_time(second, 60_000, Zone::Utc),
            short_time(first, 60_000, Zone::Utc),
        ];
        let (x, y) = window
            .workspace()
            .ui_state()
            .rects()
            .iter()
            .filter(|(t, _)| rows.contains(t))
            .nth(1)
            .unwrap()
            .1
            .center();
        window.handle(Event::PointerMoved { x, y });
        window.handle(Event::PointerDown);
        window.handle(Event::PointerUp);
        assert_eq!(window.workspace().ctx.session, Some(first));
        let chart = window.workspace().chart();
        assert_eq!(chart.resources().get::<Account>().unwrap().fills()[0].qty, 95.24);
        assert!(chart.cursor() > cursor, "where it was left, two steps on");
        let saved = Sessions::new(&source.root).list().unwrap();
        assert_eq!(
            saved.iter().map(|s| s.fills[0].qty).collect::<Vec<_>>(),
            [-95.24, 95.24]
        );

        // Still on the sessions page.
        click(&mut window, "Export CSV");
        let t = DateTime::from_time(first);
        let name = format!(
            "ETHUSDT-{}-{:02}-{:02}-{:02}{:02}.csv",
            t.year, t.month, t.day, t.hour, t.minute
        );
        let exports = source.root.join("exports");
        let lines = vec![format!("Saved {name}"), format!("in {}", exports.display())];
        assert_eq!(window.workspace().ctx.exported, Some((lines, false)));
        let csv = exports.join(name);
        let text = std::fs::read_to_string(csv).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.lines().nth(1).unwrap().contains(",buy,95.24,"), "{text}");
        std::fs::remove_dir_all(&source.root).unwrap();
    }

    #[test]
    fn each_trade_closed_leaves_a_picture_of_the_chart_beside_the_session() {
        let source = source("snapshots");
        let (year, month) = sync::months_before(now(), 1);
        let start = Period::Month { year, month }.start();
        let candles: Vec<Candle> = (0..300)
            .map(|i| Candle {
                time: Time(start.0 + i * 60_000),
                open: 10.0,
                high: 11.0,
                low: 9.0,
                close: 10.5,
                volume: 1.0,
            })
            .collect();
        Store::new(&source.root)
            .write(&series("ETHUSDT"), Period::Month { year, month }, &candles)
            .unwrap();
        let mut window = Window::new(source.clone(), series("ETHUSDT"), None);
        window.resize(1400.0, 800.0);
        settle(&mut window);
        window.build_scene(&mut Scene::new());
        start_replay(&mut window);
        for _ in 0..2 {
            click(&mut window, "Buy");
            click(&mut window, "Buy 95.24 at market");
            click(&mut window, "Step");
            click(&mut window, "Sell");
            click(&mut window, "Sell 95.24 at market");
            click(&mut window, "Step");
        }
        // Waits for the pictures asked for so far.
        let failed = window.snapshots.wait();
        let session = window.workspace().ctx.sessions[0].clone();
        let dir = Sessions::new(&source.root).snapshots(session.created, "ETHUSDT");
        let second = std::fs::read(dir.join("trade-2.png")).unwrap_or_else(|e| panic!("{e}: {failed:?}"));
        assert_eq!(
            window.workspace().ctx.snapshots.as_ref(),
            Some(&dir),
            "named on the sessions page"
        );

        // A trade replayed is not traded again, nor are the session's trades when it comes back.
        Review::start(window.workspace_mut().chart_mut(), 1);
        for _ in 0..25 {
            click(&mut window, "Step");
        }
        let fills = |w: &Window| {
            w.workspace()
                .chart()
                .resources()
                .get::<Account>()
                .unwrap()
                .fills()
                .len()
        };
        assert_eq!(fills(&window), 2, "the trade's own");
        Review::back(window.workspace_mut().chart_mut());
        click(&mut window, "Step");
        assert_eq!(fills(&window), 4);
        let area = window.workspace().chart_area();
        drop(window);

        let sessions = Sessions::new(&source.root);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["trade-1.png", "trade-2.png"]);
        assert!(
            std::fs::read(dir.join("trade-2.png")).unwrap() == second,
            "not taken again"
        );
        let png = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(dir.join("trade-2.png")).unwrap(),
        ));
        let info = png.read_info().unwrap();
        let (w, h) = (info.info().width, info.info().height);
        assert_eq!(
            (w, h),
            (area.w.round() as u32, area.h.round() as u32),
            "the chart's size"
        );

        sessions.delete(&session).unwrap();
        assert!(!dir.exists(), "gone with its session");
        std::fs::remove_dir_all(&source.root).unwrap();
    }
}
