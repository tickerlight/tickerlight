//! A trading session: what is needed to come back to a replay as it was left, or to paper trading on live data.
//! The series, the timeframe and the look, where the replay stands, the account (fills, orders, fees), the
//! drawings, and notes and tags on trades.
//!
//! Saved as text, a line each: `chart-session 1`, then `key values...` lines. Numbers are written so they read
//! back exactly. Lines that cannot be read are skipped, so a damaged file loses only what is damaged.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tickerlight_core::builtin::{
    Account, DEFAULT_BALANCE, Drawings, Exits, Fill, Order, OrderKind, Replay, ReplayStops, SeriesKind,
};
use tickerlight_core::calendar::{DateTime, Zone};
use tickerlight_core::{Chart, Event, Look, Time, TimeScale, native_step};

use tickerlight_ui::Layout;
use tickerlight_ui::panels::{Journal, OrderSizing, SizeMode};

use crate::store::{INTERVALS, Series, Venue};

const HEADER: &str = "chart-session 1";
const EXTENSION: &str = "session";

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub series: Series,
    /// When the session began (ms): it names the file.
    pub created: Time,
    /// The candles shown, `None` for the data as it came.
    pub timeframe: Option<i64>,
    pub kind: SeriesKind,
    /// Paper trading on live data rather than a replay: its fills and orders came outside one.
    pub live: bool,
    /// The last bar shown by the replay; `None` when it was not running.
    pub cursor: Option<Time>,
    /// Where the replay stood when it played by time (see [`Chart::moment`]).
    pub moment: Option<Time>,
    /// The last bar shown when the replay started (see [`Replay::from`]).
    pub from: Option<Time>,
    /// How fast the replay plays, as a multiple of real time.
    pub speed: f64,
    /// Where playing pauses by itself.
    pub stops: ReplayStops,
    /// The replay hides its dates (see [`Replay::blind`]).
    pub blind: bool,
    /// Bar spacing and the index at the right edge.
    pub view: (f64, f64),
    pub order_size: f64,
    pub fee_rate: f64,
    /// The starting balance.
    pub balance: f64,
    /// How much worse market and stop orders fill (`Account::slippage`).
    pub slippage: f64,
    /// How the order ticket sizes orders.
    pub sizing: OrderSizing,
    pub fills: Vec<Fill>,
    pub orders: Vec<Order>,
    /// In the drawings' own text (`Drawings::save`).
    pub drawings: String,
    /// Notes and tags on the fills.
    pub journal: Journal,
    /// The order flow layers.
    pub footprint: bool,
    pub profile: bool,
}

impl Session {
    /// The session as the chart has it now.
    pub fn capture(chart: &Chart, series: &Series, created: Time) -> Self {
        let res = chart.resources();
        let account = res.get::<Account>().cloned().unwrap_or_default();
        let replay = res.get::<Replay>().cloned().unwrap_or_default();
        let time = chart.time_scale();
        Self {
            series: series.clone(),
            created,
            timeframe: chart.timeframe(),
            kind: res.get::<SeriesKind>().copied().unwrap_or_default(),
            live: false,
            cursor: chart.cursor().filter(|_| replay.active),
            moment: chart.moment().filter(|_| replay.active),
            from: replay.from.filter(|_| replay.active),
            speed: replay.speed,
            stops: replay.stops,
            blind: replay.blind && replay.active,
            view: (time.bar_spacing, time.right),
            order_size: account.order_size,
            fee_rate: account.fee_rate,
            balance: account.balance,
            slippage: account.slippage,
            sizing: res.get::<OrderSizing>().copied().unwrap_or_default(),
            fills: account.fills().to_vec(),
            orders: account.orders().to_vec(),
            drawings: res.get::<Drawings>().map(Drawings::save).unwrap_or_default(),
            journal: res.get::<Journal>().cloned().unwrap_or_default(),
            footprint: false,
            profile: false,
        }
    }

    /// Puts the chart (of this session's series) where the session was. A live session goes on with the chart as
    /// it is (the newest bars, in the timeframe and look chosen): only its account, sizing, drawings and notes.
    pub fn apply(&self, chart: &mut Chart) {
        if !self.live {
            chart.set_timeframe(self.timeframe);
        }
        chart.set_cursor(self.cursor);
        chart.set_moment(self.moment);
        let period = chart.timeframe().or(native_step(chart.base()));
        if let Some(replay) = chart.resources_mut().get_mut::<Replay>() {
            replay.active = self.cursor.is_some();
            replay.playing = false;
            (replay.speed, replay.speed_for) = (self.speed, period);
            replay.from = self.from.filter(|_| self.cursor.is_some());
            replay.stops = self.stops;
            replay.blind = self.blind && self.cursor.is_some();
        }
        // The handlers see the replay begin first: a start cancels the orders pending before it, and these are
        // the session's own.
        chart.handle(Event::Tick { dt: 0.0 });
        let panes = chart.pane_names();
        let res = chart.resources_mut();
        if let Some(kind) = res.get_mut::<SeriesKind>().filter(|_| !self.live) {
            *kind = self.kind;
        }
        res.insert(self.account());
        res.insert(self.sizing);
        if let Some(drawings) = res.get_mut::<Drawings>() {
            drawings.load(&self.drawings, &panes);
        }
        res.insert(self.journal.clone());
        if !self.live {
            let width = chart.time_scale().width;
            chart.set_time_scale(TimeScale::new(width, self.view.0, self.view.1));
        }
        // Lets the handlers bring their readouts (bars shown, bars in all) up to date.
        chart.handle(Event::Tick { dt: 0.0 });
    }

    /// The account of the session, with its history.
    pub fn account(&self) -> Account {
        let mut account = Account::restore(self.order_size, self.fee_rate, &self.fills, &self.orders);
        account.balance = self.balance;
        account.slippage = self.slippage;
        account
    }

    pub fn to_text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        let mut line = |s: String| {
            out += &s;
            out.push('\n');
        };
        let Series {
            venue,
            symbol,
            interval,
        } = &self.series;
        line(format!("series {} {symbol} {}", venue.name(), interval.as_str()));
        line(format!("created {}", self.created.0));
        line(format!(
            "timeframe {}",
            self.timeframe.map_or("native".into(), |t| t.to_string())
        ));
        line(format!("kind {}", kind_name(self.kind)));
        if self.live {
            line("mode live".into());
        }
        line(format!(
            "cursor {}",
            self.cursor.map_or("live".into(), |t| t.0.to_string())
        ));
        if let Some(moment) = self.moment {
            line(format!("moment {}", moment.0));
        }
        if let Some(from) = self.from {
            line(format!("from {}", from.0));
        }
        line(format!("pace {}", self.speed));
        if self.stops != ReplayStops::default() {
            let clock = self.stops.clock.map_or("-".into(), |m| m.to_string());
            line(format!("stops {} {clock}", u8::from(self.stops.lines)));
        }
        if self.blind {
            line("blind".into());
        }
        line(format!("view {} {}", self.view.0, self.view.1));
        line(format!("account {} {}", self.order_size, self.fee_rate));
        line(format!("balance {}", self.balance));
        line(format!("slippage {}", self.slippage));
        let OrderSizing { mode, value, risk } = self.sizing;
        line(format!("sizing {} {value} {risk}", mode.name()));
        line(format!("flow {} {}", self.footprint as u8, self.profile as u8));
        for f in &self.fills {
            line(format!("fill {} {} {} {}", f.time.0, f.price, f.qty, f.fee));
        }
        for o in &self.orders {
            let price = |p: Option<f64>| p.map_or("-".into(), |p| p.to_string());
            line(format!(
                "order {} {} {} {} {} {} {} {} {}",
                match o.kind {
                    OrderKind::Limit => "limit",
                    OrderKind::Stop => "stop",
                },
                o.qty,
                o.price,
                o.after.0,
                o.reduce_only as u8,
                price(o.exits.take_profit),
                price(o.exits.stop_loss),
                price(o.exits.break_even),
                price(o.break_even)
            ));
        }
        for (fill, n) in &self.journal.notes {
            if !n.tags.is_empty() || !n.text.trim().is_empty() {
                let tags = if n.tags.is_empty() {
                    "-".into()
                } else {
                    n.tags.join(",")
                };
                line(format!("note {fill} {tags} {}", n.text.replace('\n', " ")));
            }
            for (column, choice) in &n.picks {
                line(format!("pick {fill} {column}: {choice}"));
            }
        }
        for d in self.drawings.lines() {
            line(format!("drawing {d}"));
        }
        out
    }

    /// Reads a session written by [`to_text`](Self::to_text). `None` without a header or a series.
    pub fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()?.trim() != HEADER {
            return None;
        }
        let mut session: Option<Self> = None;
        for line in lines {
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            if key == "series" {
                session = parse_series(rest).map(Self::empty);
                continue;
            }
            // Everything else belongs to a series named before it.
            let Some(s) = session.as_mut() else { continue };
            let _ = s.read(key, rest);
        }
        session
    }

    fn empty(series: Series) -> Self {
        Self {
            series,
            created: Time(0),
            timeframe: None,
            kind: SeriesKind::default(),
            live: false,
            cursor: None,
            moment: None,
            from: None,
            speed: Replay::default().speed,
            stops: ReplayStops::default(),
            blind: false,
            view: (8.0, 0.0),
            order_size: 1.0,
            fee_rate: 0.0,
            balance: DEFAULT_BALANCE,
            slippage: 0.0,
            sizing: OrderSizing::default(),
            fills: Vec::new(),
            orders: Vec::new(),
            drawings: String::new(),
            journal: Journal::default(),
            footprint: false,
            profile: false,
        }
    }

    /// One line; `None` if it cannot be read.
    fn read(&mut self, key: &str, rest: &str) -> Option<()> {
        let mut w = rest.split_whitespace();
        let mut f64_ = || w.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
        match key {
            "created" => self.created = Time(rest.trim().parse().ok()?),
            "timeframe" => {
                self.timeframe = match rest.trim() {
                    "native" => None,
                    t => Some(t.parse().ok().filter(|&t: &i64| t > 0)?),
                }
            }
            "kind" => self.kind = kind_from(rest.trim())?,
            "mode" => self.live = rest.trim() == "live",
            "cursor" => {
                self.cursor = match rest.trim() {
                    "live" => None,
                    t => Some(Time(t.parse().ok()?)),
                }
            }
            "moment" => self.moment = Some(Time(rest.trim().parse().ok()?)),
            "from" => self.from = Some(Time(rest.trim().parse().ok()?)),
            "blind" => self.blind = true,
            // Times real time; sessions from before kept bars a second as `speed`, which is not read.
            "pace" => self.speed = f64_().filter(|s| *s > 0.0)?,
            "stops" => {
                let lines = w.next()? == "1";
                let clock = match w.next()? {
                    "-" => None,
                    m => Some(m.parse().ok().filter(|&m: &u32| m < 24 * 60)?),
                };
                self.stops = ReplayStops { lines, clock };
            }
            "view" => self.view = (f64_()?, f64_()?),
            "account" => (self.order_size, self.fee_rate) = (f64_()?, f64_()?),
            "balance" => self.balance = f64_().filter(|b| *b > 0.0)?,
            "slippage" => self.slippage = f64_().filter(|s| (0.0..1.0).contains(s))?,
            "sizing" => {
                let mode = SizeMode::from_name(w.next()?)?;
                let mut num = || w.next()?.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0);
                self.sizing = OrderSizing {
                    mode,
                    value: num()?,
                    risk: num()?,
                };
            }
            "flow" => {
                let mut flag = || w.next().map(|f| f == "1");
                (self.footprint, self.profile) = (flag()?, flag()?);
            }
            "fill" => {
                let time = Time(w.next()?.parse().ok()?);
                let mut num = || w.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
                let (price, qty, fee) = (num()?, num()?, num()?);
                self.fills.push(Fill { time, price, qty, fee });
            }
            "order" => {
                let kind = match w.next()? {
                    "limit" => OrderKind::Limit,
                    "stop" => OrderKind::Stop,
                    _ => return None,
                };
                let mut num = || w.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
                let (qty, price) = (num()?, num()?);
                let after = Time(w.next()?.parse().ok()?);
                let reduce_only = w.next()? == "1";
                let mut exit = || match w.next()? {
                    "-" => Some(None),
                    p => p.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some),
                };
                let exits = Exits {
                    take_profit: exit()?,
                    stop_loss: exit()?,
                    // Sessions from before break-evens end here.
                    break_even: exit().flatten(),
                };
                self.orders.push(Order {
                    kind,
                    qty,
                    price,
                    after,
                    reduce_only,
                    exits,
                    break_even: exit().flatten(),
                });
            }
            "note" => {
                let mut parts = rest.splitn(3, ' ');
                let fill = parts.next()?.parse().ok()?;
                let tags = match parts.next()? {
                    "-" => Vec::new(),
                    t => t.split(',').map(str::to_string).collect(),
                };
                let text = parts.next().unwrap_or("").to_string();
                let note = self.journal.notes.entry(fill).or_default();
                (note.tags, note.text) = (tags, text);
            }
            // A column's name has no colon; a choice may.
            "pick" => {
                let (fill, pick) = rest.split_once(' ')?;
                let (column, choice) = pick.split_once(':')?;
                let note = self.journal.notes.entry(fill.parse().ok()?).or_default();
                note.picks.insert(column.to_string(), choice.trim_start().to_string());
            }
            "drawing" => {
                self.drawings += rest;
                self.drawings.push('\n');
            }
            _ => return None,
        }
        Some(())
    }

    /// The fills as CSV, oldest first: UTC time, side, size, price, fee, what a closing fill realized after the
    /// fees of its trade, the note and tags (space separated) of the fill, and a column for each journal column
    /// picked in.
    pub fn trades_csv(&self) -> String {
        let columns: BTreeSet<&String> = self.journal.notes.values().flat_map(|n| n.picks.keys()).collect();
        let mut out = String::from("time_utc,side,size,price,fee,result,note,tags");
        for column in &columns {
            out += &format!(",{}", csv_field(column));
        }
        out.push('\n');
        let results = Account::results(&self.fills);
        for (i, f) in self.fills.iter().enumerate() {
            let t = DateTime::from_time(f.time);
            let seconds = f.time.0.rem_euclid(60_000) / 1000;
            let result = results[i].map_or(String::new(), number);
            let note = self.journal.notes.get(&i).cloned().unwrap_or_default();
            out += &format!(
                "{}-{:02}-{:02} {:02}:{:02}:{seconds:02},{},{},{},{},{result},{},{}",
                t.year,
                t.month,
                t.day,
                t.hour,
                t.minute,
                if f.qty > 0.0 { "buy" } else { "sell" },
                number(f.qty.abs()),
                number(f.price),
                number(f.fee),
                csv_field(&note.text),
                csv_field(&note.tags.join(" ")),
            );
            for &column in &columns {
                out += &format!(",{}", csv_field(note.picks.get(column).map_or("", String::as_str)));
            }
            out.push('\n');
        }
        out
    }

    /// The file name: when it began and the symbol, so a listing sorts by time.
    pub fn file_name(&self) -> String {
        format!("{}-{}.{EXTENSION}", self.created.0, self.series.symbol)
    }
}

/// A number for a spreadsheet: at most 8 decimals, without the float noise (`0.30000000000000004` is `0.3`).
fn number(v: f64) -> String {
    let text = format!("{v:.8}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".into() } else { text.into() }
}

/// A CSV field: quoted when it holds a comma, a quote or a line break, quotes doubled.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn parse_series(rest: &str) -> Option<Series> {
    let mut w = rest.split_whitespace();
    let (venue, symbol, interval) = (w.next()?, w.next()?, w.next()?);
    Some(Series {
        venue: Venue::from_name(venue)?,
        symbol: symbol.into(),
        interval: INTERVALS.into_iter().find(|i| i.as_str() == interval)?,
    })
}

fn kind_name(kind: SeriesKind) -> &'static str {
    match kind {
        SeriesKind::Candles => "candles",
        SeriesKind::HeikinAshi => "heikin-ashi",
        SeriesKind::Bars => "bars",
        SeriesKind::Line => "line",
        SeriesKind::Area => "area",
    }
}

fn kind_from(name: &str) -> Option<SeriesKind> {
    [
        SeriesKind::Candles,
        SeriesKind::HeikinAshi,
        SeriesKind::Bars,
        SeriesKind::Line,
        SeriesKind::Area,
    ]
    .into_iter()
    .find(|&k| kind_name(k) == name)
}

/// The sessions on disk: `<root>/sessions/<created>-<SYMBOL>.session`.
pub struct Sessions {
    dir: PathBuf,
}

impl Sessions {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("sessions"),
        }
    }

    /// Writes the session, replacing its earlier save (through a temporary file, so a crash leaves the old one).
    pub fn save(&self, session: &Session) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(session.file_name());
        let partial = path.with_extension("partial");
        fs::write(&partial, session.to_text())?;
        fs::rename(partial, path)
    }

    /// Every session that can be read, newest first.
    pub fn list(&self) -> io::Result<Vec<Session>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut sessions = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == EXTENSION)
                && let Some(session) = fs::read_to_string(&path).ok().as_deref().and_then(Session::from_text)
            {
                sessions.push(session);
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created));
        Ok(sessions)
    }

    /// Deletes the session and the snapshots of its trades.
    pub fn delete(&self, session: &Session) -> io::Result<()> {
        fs::remove_file(self.dir.join(session.file_name()))?;
        match fs::remove_dir_all(self.snapshots(session.created, &session.series.symbol)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Where the snapshots of a session's trades go: beside its file, named as it is without the extension.
    pub fn snapshots(&self, created: Time, symbol: &str) -> PathBuf {
        self.dir.join(format!("{}-{symbol}", created.0))
    }

    /// Writes the trades of a session as CSV into `dir` (`BTCUSDT-2026-10-17-2050.csv`, after when the session
    /// began), and returns where.
    pub fn export(session: &Session, dir: &Path) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let t = DateTime::from_time(session.created);
        let name = format!(
            "{}-{}-{:02}-{:02}-{:02}{:02}.csv",
            session.series.symbol, t.year, t.month, t.day, t.hour, t.minute
        );
        let path = dir.join(name);
        fs::write(&path, session.trades_csv())?;
        Ok(path)
    }
}

/// Where the program was left: the layout, the chart in each of its cells and which one was active. The window
/// writes it when any of that changes and starts from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resume {
    pub layout: Layout,
    /// Of the charts, then the one in a window of its own.
    pub active: usize,
    /// The clock times are shown on.
    pub zone: Zone,
    /// The colors of the window.
    pub look: Look,
    /// At least one.
    pub charts: Vec<Resumed>,
    /// The chart in a window of its own.
    pub apart: Option<Resumed>,
}

/// A chart where it was left: the series, its timeframe and the session going on.
#[derive(Debug, Clone, PartialEq)]
pub struct Resumed {
    pub series: Series,
    pub timeframe: Option<i64>,
    pub session: Option<Time>,
}

impl Resume {
    const HEADER: &str = "chart-resume 1";

    fn path(root: &Path) -> PathBuf {
        root.join("resume")
    }

    /// What was saved, if anything can be read.
    pub fn load(root: &Path) -> Option<Self> {
        Self::from_text(&fs::read_to_string(Self::path(root)).ok()?)
    }

    pub fn save(&self, root: &Path) -> io::Result<()> {
        fs::create_dir_all(root)?;
        let path = Self::path(root);
        let partial = path.with_extension("partial");
        fs::write(&partial, self.to_text())?;
        fs::rename(partial, path)
    }

    pub fn to_text(&self) -> String {
        let mut text = format!(
            "{}\nlayout {}\nactive {}\nzone {}\nlook {}\n",
            Self::HEADER,
            self.layout.name(),
            self.active,
            self.zone.name(),
            self.look.name()
        );
        for (i, chart) in self.charts.iter().chain(&self.apart).enumerate() {
            let Series {
                venue,
                symbol,
                interval,
            } = &chart.series;
            // The chart in a window of its own comes last, after a line that says so.
            if i == self.charts.len() {
                text += "apart\n";
            }
            text += &format!(
                "series {} {symbol} {}\ntimeframe {}\nsession {}\n",
                venue.name(),
                interval.as_str(),
                chart.timeframe.map_or("native".into(), |t| t.to_string()),
                chart.session.map_or("none".into(), |t| t.0.to_string()),
            );
        }
        text
    }

    /// A `series` line begins each chart; one that cannot be read drops its chart. One after an `apart` line is
    /// the chart in a window of its own. Without a `layout` it is one chart, as before there were more; without a
    /// `zone`, UTC; without a `look`, dark.
    fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()?.trim() != Self::HEADER {
            return None;
        }
        let (mut layout, mut active, mut zone, mut charts) = (Layout::One, 0, Zone::Utc, Vec::new());
        let mut look = Look::Dark;
        // The chart the lines are about: `None` after a series that cannot be read.
        let mut current: Option<usize> = None;
        // Where in `charts` the chart in a window of its own is, and whether the next series is it.
        let (mut apart, mut next_apart) = (None, false);
        for line in lines {
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            match (key, rest.trim(), current) {
                ("layout", name, _) => layout = Layout::from_name(name).unwrap_or(Layout::One),
                ("active", i, _) => active = i.parse().unwrap_or(0),
                ("zone", name, _) => zone = Zone::from_name(name).unwrap_or_default(),
                ("look", name, _) => look = Look::from_name(name).unwrap_or_default(),
                ("apart", _, _) => next_apart = true,
                ("series", rest, _) => {
                    current = parse_series(rest).map(|series| {
                        charts.push(Resumed {
                            series,
                            timeframe: None,
                            session: None,
                        });
                        charts.len() - 1
                    });
                    if std::mem::take(&mut next_apart) {
                        apart = current;
                    }
                }
                ("timeframe", t, Some(c)) => charts[c].timeframe = t.parse().ok().filter(|&t: &i64| t > 0),
                ("session", t, Some(c)) => charts[c].session = t.parse().ok().map(Time),
                _ => {}
            }
        }
        let apart = apart.map(|i| charts.remove(i));
        (!charts.is_empty()).then(|| Self {
            layout,
            active: active.min(charts.len() - usize::from(apart.is_none())),
            zone,
            look,
            charts,
            apart,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::new_chart;
    use tickerlight_binance::{Interval, Market};
    use tickerlight_core::plugin::commands;
    use tickerlight_core::{Candle, Candles, Scene};
    use tickerlight_ui::panels::Note;

    fn chart() -> Chart {
        let mut candles = Candles::new();
        for i in 0..600 {
            let p = 100.0 + (i as f64 * 0.05).sin() * 5.0;
            candles
                .push(Candle {
                    time: Time(1_700_000_040_000 + i * 60_000),
                    open: p,
                    high: p + 1.0,
                    low: p - 1.0,
                    close: p + 0.3,
                    volume: 2.0,
                })
                .unwrap();
        }
        let mut chart = new_chart(candles, Venue::Binance(Market::Spot));
        chart.resize(1000.0, 600.0);
        chart
    }

    fn scene(chart: &mut Chart) -> String {
        let mut scene = Scene::new();
        chart.build_scene(&mut scene);
        format!("{scene:?}")
    }

    fn series() -> Series {
        Series {
            venue: Venue::Binance(Market::Spot),
            symbol: "BTCUSDT".into(),
            interval: Interval::M1,
        }
    }

    #[test]
    fn a_saved_session_comes_back_as_it_was() {
        let mut a = chart();
        a.set_timeframe(Some(300_000));
        a.handle(Event::Wheel { x: 500.0, delta: 2.0 });
        a.handle(Event::Command(commands::REPLAY_TOGGLE));
        for _ in 0..7 {
            a.handle(Event::Command(commands::REPLAY_STEP));
        }
        a.handle(Event::Command(commands::REPLAY_FASTER));
        let now = *a.base().time().last().unwrap();
        let res = a.resources_mut();
        *res.get_mut::<SeriesKind>().unwrap() = SeriesKind::HeikinAshi;
        res.insert(OrderSizing {
            mode: SizeMode::Risk,
            value: 250.0,
            risk: 0.5,
        });
        res.get_mut::<Replay>().unwrap().stops = ReplayStops {
            lines: true,
            clock: Some(9 * 60 + 30),
        };
        res.get_mut::<Replay>().unwrap().blind = true;
        let account = res.get_mut::<Account>().unwrap();
        account.order_size = 0.5;
        account.balance = 2_500.5;
        account.slippage = 0.0005;
        account.fill(Time(now.0 - 600_000), 101.25, 0.5);
        account.fill(now, 103.5, -0.25);
        account.protect(
            now,
            0.25,
            Exits {
                take_profit: Some(110.0),
                stop_loss: Some(95.5),
                break_even: Some(105.0),
            },
        );
        let drawn = res
            .get_mut::<Drawings>()
            .unwrap()
            .load(&format!("hline main {} 102 {} 102 2962ff\n", now.0, now.0), &["main"]);
        assert_eq!(drawn, 1);
        a.handle(Event::Tick { dt: 0.0 });

        let mut session = Session::capture(&a, &series(), Time(1_700_000_000_000));
        session.journal.notes.insert(
            1,
            Note {
                tags: vec!["breakout".into(), "early".into()],
                text: "took half off  too soon".into(),
                picks: [("Setup grade", "A+"), ("Mistake", "size: too big")]
                    .map(|(c, p)| (c.to_string(), p.to_string()))
                    .into(),
            },
        );
        // Picks alone, without a note.
        session.journal.notes.insert(
            0,
            Note {
                picks: [("Mistake".to_string(), "late".to_string())].into(),
                ..Note::default()
            },
        );
        session.footprint = true;
        assert_eq!(session.cursor, a.cursor());
        assert!(session.blind, "a blind replay stays blind");
        assert_eq!((session.fills.len(), session.orders.len()), (2, 2));
        let text = session.to_text();
        let back = Session::from_text(&text).unwrap();
        assert_eq!(back, session, "{text}");
        let live = Session {
            live: true,
            ..Session::empty(series())
        };
        assert_eq!(
            Session::from_text(&live.to_text()),
            Some(live),
            "a live one reads back as live"
        );

        let mut b = chart();
        back.apply(&mut b);
        assert_eq!(b.resources().get::<Account>(), a.resources().get::<Account>());
        assert_eq!(b.resources().get::<OrderSizing>(), a.resources().get::<OrderSizing>());
        assert_eq!(b.resources().get::<Replay>(), a.resources().get::<Replay>());
        assert_eq!(scene(&mut b), scene(&mut a));
    }

    #[test]
    fn where_the_program_was_left_reads_back() {
        let root = std::env::temp_dir().join(format!("chart-resume-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(Resume::load(&root), None);
        let chart = Resumed {
            series: series(),
            timeframe: Some(900_000),
            session: Some(Time(1_700_000_000_000)),
        };
        let resume = Resume {
            layout: Layout::Columns,
            active: 1,
            zone: Zone::NewYork,
            look: Look::Light,
            charts: vec![
                chart.clone(),
                Resumed {
                    timeframe: None,
                    session: None,
                    ..chart.clone()
                },
            ],
            apart: None,
        };
        resume.save(&root).unwrap();
        assert_eq!(Resume::load(&root), Some(resume.clone()));
        assert!(resume.to_text().contains("\nlook light\n"));
        // A chart in a window of its own, active, comes back as it was.
        let apart = Resume {
            active: 2,
            apart: Some(Resumed {
                timeframe: Some(3_600_000),
                session: None,
                ..chart.clone()
            }),
            ..resume.clone()
        };
        apart.save(&root).unwrap();
        assert_eq!(Resume::load(&root), Some(apart));
        // Written before there were several charts, a time zone or a look: one, active, in UTC, dark.
        let s = series();
        let old = format!(
            "chart-resume 1\nseries {} {} {}\ntimeframe 900000\nsession 1700000000000\n",
            s.venue.name(),
            s.symbol,
            s.interval.as_str()
        );
        fs::write(root.join("resume"), old).unwrap();
        let one = Resume {
            layout: Layout::One,
            active: 0,
            zone: Zone::Utc,
            look: Look::Dark,
            charts: vec![chart],
            apart: None,
        };
        assert_eq!(Resume::load(&root), Some(one));
        fs::write(root.join("resume"), "chart-resume 1\ntimeframe 60000\n").unwrap();
        assert_eq!(Resume::load(&root), None, "no series");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_cannot_be_read_is_skipped() {
        assert_eq!(Session::from_text("something else\nseries spot BTCUSDT 1m"), None);
        assert_eq!(Session::from_text("chart-session 1\nfill 1 2 3 4"), None, "no series");
        let s = Session::from_text("chart-session 1\nseries spot BTCUSDT 1m\nfill 1 x 3 4\nfill 5 6 7 0\nkind odd\n")
            .unwrap();
        assert_eq!(s.fills.len(), 1);
        assert_eq!(s.kind, SeriesKind::Candles);
        // Saved before there was a balance or a choice of size: the defaults.
        assert_eq!((s.balance, s.sizing), (DEFAULT_BALANCE, OrderSizing::default()));
    }

    #[test]
    fn sessions_are_listed_newest_first() {
        let root = std::env::temp_dir().join(format!("chart-sessions-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sessions = Sessions::new(&root);
        assert!(sessions.list().unwrap().is_empty());
        let mut s = Session::empty(series());
        for created in [3, 1, 2] {
            s.created = Time(created);
            sessions.save(&s).unwrap();
        }
        s.created = Time(2);
        s.fills.push(Fill {
            time: Time(9),
            price: 1.0,
            qty: 1.0,
            fee: 0.0,
        });
        sessions.save(&s).unwrap();
        let listed = sessions.list().unwrap();
        assert_eq!(listed.iter().map(|s| s.created.0).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(listed[1].fills.len(), 1, "a save replaces the earlier one");
        sessions.delete(&listed[0]).unwrap();
        assert_eq!(sessions.list().unwrap().len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_trades_export_as_csv() {
        let mut s = Session::empty(series());
        let fill = |time, price, qty, fee| Fill {
            time: Time(time),
            price,
            qty,
            fee,
        };
        // 2026-10-12 21:45:00 UTC and 21:59:30; the sale makes 74.75 less both fees.
        let t = 1_791_841_500_000;
        s.fills = vec![
            fill(t, 81850.5, 0.5, 0.0409),
            fill(t + 870_000, 82000.0, -0.5, 0.1 + 0.2),
        ];
        s.journal.notes.insert(
            0,
            Note {
                tags: vec!["breakout".into(), "late".into()],
                text: "said \"wait\", went in, regretted".into(),
                picks: [("Grade".to_string(), "B, at best".to_string())].into(),
            },
        );
        assert_eq!(
            s.trades_csv(),
            "time_utc,side,size,price,fee,result,note,tags,Grade\n\
             2026-10-12 21:45:00,buy,0.5,81850.5,0.0409,,\"said \"\"wait\"\", went in, regretted\",breakout late,\"B, at best\"\n\
             2026-10-12 21:59:30,sell,0.5,82000,0.3,74.4091,,,\n"
        );

        let root = std::env::temp_dir().join(format!("chart-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = Sessions::export(&s, &root.join("exports")).unwrap();
        assert_eq!(path, root.join("exports").join("BTCUSDT-1970-01-01-0000.csv"));
        assert_eq!(fs::read_to_string(&path).unwrap(), s.trades_csv());
        fs::remove_dir_all(&root).unwrap();
    }
}
