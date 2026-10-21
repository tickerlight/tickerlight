//! Strategies written in Rust: code that trades bar by bar and sees only the bars that have closed.
//!
//! A [`Strategy`] is called at the close of each bar with a [`StrategyCtx`] that holds the bars up to that one and
//! no more, so it cannot look ahead. Its orders are those of the manual [`Account`]: a market order fills at the
//! open of the next bar, a limit or a stop on a later bar that reaches its price, and the take profit and stop
//! loss of an entry wait for the bars after the one it filled on. [`backtest`] runs one over a series;
//! [`backtest_with_tape`] also over its trades, which settle what a bar reached first.

use super::drawings::{Dash, Pen};
use super::equity::Equity;
use super::studies::Cache;
#[cfg(test)]
use super::trading::DEFAULT_BALANCE;
use super::trading::{Account, Exits, Fill, Order, OrderKind, Stats};
use super::{Kind, STUDY_COLORS, Study};
use crate::plugin::{names, order};
use crate::{Align, CandleView, Chart, Color, DrawCtx, Layer, Time, TradeView};

/// A trading strategy. [`on_bar`](Strategy::on_bar) is called at the close of every bar, the oldest first.
pub trait Strategy {
    /// What it is called in lists, like "Moving average cross 10/30".
    fn name(&self) -> String;

    /// The numbers it is set with, which the program shows as fields to change. A strategy reads them when it
    /// runs, so it keeps them sane itself (a length of at least one bar, say).
    fn settings(&mut self) -> Vec<Setting<'_>> {
        Vec::new()
    }

    /// Called before the first bar of each run: what it keeps from bar to bar starts again here.
    fn start(&mut self) {}

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>);

    /// Another one set the same, with a state of its own: one trades live while the other runs a backtest beside
    /// it. `None` when none can be made, and then it does not trade by itself.
    fn another(&self) -> Option<Box<dyn Strategy>> {
        None
    }
}

/// A number a strategy is set with.
pub struct Setting<'a> {
    pub name: &'a str,
    pub value: &'a mut f64,
}

/// What a strategy sees and does at the close of a bar.
pub struct StrategyCtx<'a> {
    bars: CandleView<'a>,
    account: &'a mut Account,
    indicators: &'a mut Vec<(Study, Cache)>,
    /// Market orders to fill at the next open: signed size, and the exits put on what fills.
    market: &'a mut Vec<(f64, Exits)>,
    plots: &'a mut Vec<Plot>,
    notes: &'a mut Vec<(Time, String)>,
}

impl<'a> StrategyCtx<'a> {
    /// The bars up to and including this one.
    pub fn bars(&self) -> CandleView<'a> {
        self.bars
    }

    /// The index of this bar in [`bars`](Self::bars): the last one.
    pub fn index(&self) -> usize {
        self.bars.len() - 1
    }

    pub fn time(&self) -> Time {
        self.bars.time()[self.index()]
    }

    pub fn open(&self) -> f64 {
        self.bars.open()[self.index()]
    }

    pub fn high(&self) -> f64 {
        self.bars.high()[self.index()]
    }

    pub fn low(&self) -> f64 {
        self.bars.low()[self.index()]
    }

    pub fn close(&self) -> f64 {
        self.bars.close()[self.index()]
    }

    pub fn account(&self) -> &Account {
        self.account
    }

    /// The signed size of the open position, with the market orders not yet filled: positive long, negative short.
    pub fn position(&self) -> f64 {
        self.account.position() + self.market.iter().map(|m| m.0).sum::<f64>()
    }

    /// What the account is worth at this close.
    pub fn equity(&self) -> f64 {
        self.account.equity(self.close())
    }

    /// The values of an indicator over the bars so far, one series per line as the chart draws them (NaN where
    /// there is none yet): the same numbers the chart shows, worked out bar by bar.
    pub fn indicator(&mut self, study: Study) -> &[Vec<f64>] {
        let at = match self.indicators.iter().position(|(s, _)| *s == study) {
            Some(at) => at,
            None => {
                self.indicators.push((study, Cache::default()));
                self.indicators.len() - 1
            }
        };
        let cache = &mut self.indicators[at].1;
        cache.update(&study, self.bars);
        &cache.out
    }

    /// The first line of an indicator at this bar and the bar before, like the RSI now and a bar ago.
    pub fn line(&mut self, kind: Kind, params: &[f64]) -> (f64, f64) {
        let mut study = Study::new(kind);
        for (i, &p) in params.iter().enumerate() {
            study.set(i, p);
        }
        let line = &self.indicator(study)[0];
        let n = line.len();
        (line[n - 1], if n > 1 { line[n - 2] } else { f64::NAN })
    }

    /// Draws `value` at this bar on the line `name` over the candles: what the strategy works with, like its
    /// averages or its stop. A bar with no value (or NaN) breaks the line.
    pub fn plot(&mut self, name: &str, value: f64) {
        let i = self.index();
        let plot = match self.plots.iter().position(|p| p.name == name) {
            Some(at) => &mut self.plots[at],
            None => {
                let color = STUDY_COLORS[self.plots.len() % STUDY_COLORS.len()];
                self.plots.push(Plot {
                    name: name.to_string(),
                    color,
                    values: Vec::new(),
                });
                self.plots.last_mut().unwrap()
            }
        };
        plot.values.resize(i + 1, f64::NAN);
        plot.values[i] = value;
    }

    /// Notes `text` at this bar: why it does what it does, shown with the trade this bar decided.
    pub fn note(&mut self, text: &str) {
        let time = self.time();
        self.notes.push((time, text.to_string()));
    }

    /// Buys `qty` at the next open, with exits for what it opens.
    pub fn buy(&mut self, qty: f64, exits: Exits) {
        self.market(qty.abs(), exits);
    }

    /// Sells `qty` at the next open, with exits for what it opens.
    pub fn sell(&mut self, qty: f64, exits: Exits) {
        self.market(-qty.abs(), exits);
    }

    fn market(&mut self, qty: f64, exits: Exits) {
        if qty != 0.0 && qty.is_finite() {
            self.market.push((qty, exits));
        }
    }

    /// Closes the position at the next open, and takes its exits off.
    pub fn close_position(&mut self) {
        let open = self.position();
        self.market(-open, Exits::default());
        self.cancel_orders();
    }

    /// A limit order: buys (`qty` positive) at `price` or lower, or sells at it or higher, on a later bar.
    pub fn limit(&mut self, qty: f64, price: f64, exits: Exits) {
        self.place(OrderKind::Limit, qty, price, exits);
    }

    /// A stop order: buys (`qty` positive) once the price rises to `price`, or sells once it falls to it.
    pub fn stop(&mut self, qty: f64, price: f64, exits: Exits) {
        self.place(OrderKind::Stop, qty, price, exits);
    }

    fn place(&mut self, kind: OrderKind, qty: f64, price: f64, exits: Exits) {
        let after = self.time();
        self.account.place(Order {
            kind,
            qty,
            price,
            after,
            reduce_only: false,
            exits,
            break_even: None,
        });
    }

    /// Takes off every order waiting, the exits of the position too.
    pub fn cancel_orders(&mut self) {
        self.account.cancel_all();
    }

    /// The size that loses `risk` of the equity (`0.01` is 1%) if the price goes from `entry` to `stop`; zero when
    /// they are the same.
    pub fn risk_size(&self, risk: f64, entry: f64, stop: f64) -> f64 {
        let distance = (entry - stop).abs();
        if distance > 0.0 && distance.is_finite() {
            self.equity().max(0.0) * risk / distance
        } else {
            0.0
        }
    }
}

/// Runs `strategy` over `bars` with `account` (its balance, fee and nothing traded yet): the account with every
/// fill, and the lines it drew. At each bar the market orders of the bar before fill at its open, then the orders
/// waiting fill if it reaches them, then the strategy sees it closed.
pub fn backtest(strategy: &mut dyn Strategy, bars: CandleView<'_>, account: Account) -> StrategyRun {
    backtest_with_tape(strategy, bars, TradeView::default(), account)
}

/// [`backtest`] with the trades of the bars: where a bar has them, its orders fill in the order the market reached
/// them (see [`Account::match_tape`]), and the exits of a market order work from the bar it fills on.
pub fn backtest_with_tape(
    strategy: &mut dyn Strategy,
    bars: CandleView<'_>,
    trades: TradeView<'_>,
    account: Account,
) -> StrategyRun {
    backtest_from(strategy, bars, trades, account, 0)
}

/// [`backtest_with_tape`] trading from bar `from` on, as a [`LiveRun`] started then does: the bars before it are
/// seen with an account of their own, and what was held or ordered there stays there.
pub fn backtest_from(
    strategy: &mut dyn Strategy,
    bars: CandleView<'_>,
    trades: TradeView<'_>,
    mut account: Account,
    from: usize,
) -> StrategyRun {
    strategy.start();
    let mut steps = Steps::default();
    let mut book = Book::default();
    let mut scratch = account.empty_like();
    for i in 0..from.min(bars.len()) {
        steps.bar(strategy, bars, trades, i, &mut scratch, None);
    }
    steps.market.clear();
    for i in from..bars.len() {
        steps.bar(strategy, bars, trades, i, &mut account, Some(&mut book));
    }
    StrategyRun {
        name: strategy.name(),
        trades: closed_trades(account.fills()),
        stats: account.stats(),
        account,
        plots: steps.plots,
        orders: book.lives,
        notes: steps.notes,
        inputs: steps
            .indicators
            .into_iter()
            .map(|(study, cache)| Input {
                study,
                lines: cache.out,
            })
            .collect(),
        selected: None,
        decided: None,
        changes: None,
        marked: Vec::new(),
        test: None,
    }
}

/// What a strategy keeps from bar to bar as it runs: the indicators it read, the market orders it gave at the last
/// close, what it drew and noted.
#[derive(Default)]
struct Steps {
    indicators: Vec<(Study, Cache)>,
    /// Signed sizes, and the exits put on what fills.
    market: Vec<(f64, Exits)>,
    plots: Vec<Plot>,
    notes: Vec<(Time, String)>,
}

impl Steps {
    /// Bar `i` of `bars` as a backtest takes it: the market orders of the bar before fill at its open, then the
    /// orders waiting fill if it reaches them, then the strategy sees it closed. `book` follows the orders.
    fn bar(
        &mut self,
        strategy: &mut dyn Strategy,
        bars: CandleView<'_>,
        trades: TradeView<'_>,
        i: usize,
        account: &mut Account,
        mut book: Option<&mut Book>,
    ) {
        let (time, open, high, low) = (bars.time(), bars.open(), bars.high(), bars.low());
        let mut track = |account: &Account, filled_from: usize| {
            if let Some(book) = book.as_deref_mut() {
                book.track(account, time[i], filled_from);
            }
        };
        let tape = super::trading::bar_trades(trades, time, i);
        // With the tape, what comes after the open is known; without it, the exits wait for the next bar.
        let exits_after = if tape.is_empty() { time[i] } else { Time(time[i].0 - 1) };
        self.fill_market(account, time[i], open[i], exits_after);
        // The market fills reach no waiting order: only what they put on or took off counts here.
        track(account, account.fills().len());
        let filled_from = account.fills().len();
        account.match_tape(time[i], open[i], high[i], low[i], tape);
        track(account, filled_from);
        self.close(strategy, bars.slice(0..i + 1), account);
        track(account, account.fills().len());
    }

    /// Fills at `price` the market orders given at the last close; their exits wait for what comes after
    /// `exits_after`.
    fn fill_market(&mut self, account: &mut Account, time: Time, price: f64, exits_after: Time) {
        for (qty, exits) in self.market.drain(..) {
            account.market(time, price, qty);
            account.protect(exits_after, qty, exits);
        }
    }

    /// Lets `strategy` see the last of `bars` closed, trading on `account`.
    fn close(&mut self, strategy: &mut dyn Strategy, bars: CandleView<'_>, account: &mut Account) {
        let mut ctx = StrategyCtx {
            bars,
            account,
            indicators: &mut self.indicators,
            market: &mut self.market,
            plots: &mut self.plots,
            notes: &mut self.notes,
        };
        strategy.on_bar(&mut ctx);
    }
}

/// A strategy trading by itself as the bars come, on an account it does not own (paper on a live chart). A bar is
/// complete once the next one has opened: then the strategy sees it, and its market orders fill at that open. Its
/// limits, stops and exits wait in the account, which fills them as it fills any.
pub struct LiveRun {
    strategy: Box<dyn Strategy>,
    /// The same strategy, for the backtest of the bars it traded.
    shadow: Box<dyn Strategy>,
    steps: Steps,
    /// The first bar it saw with the account.
    pub from: Time,
    /// The last bar it saw and its open: when the bars have it no more, they are other bars.
    seen: (Time, f64),
}

impl LiveRun {
    /// Another of `strategy` to trade from the last of `bars`, the one forming: it has seen the bars before with
    /// their `trades` and an account like `account`, as [`backtest_from`] takes them. `None` when the strategy
    /// makes no other, or there is no complete bar.
    pub fn new(
        strategy: &dyn Strategy,
        bars: CandleView<'_>,
        trades: TradeView<'_>,
        account: &Account,
    ) -> Option<Self> {
        let last = bars.len().checked_sub(2)?;
        let (mut live, shadow) = (strategy.another()?, strategy.another()?);
        live.start();
        let mut steps = Steps::default();
        let mut scratch = account.empty_like();
        for i in 0..=last {
            steps.bar(live.as_mut(), bars, trades, i, &mut scratch, None);
        }
        steps.market.clear();
        Some(Self {
            strategy: live,
            shadow,
            steps,
            from: bars.time()[last + 1],
            seen: (bars.time()[last], bars.open()[last]),
        })
    }

    pub fn name(&self) -> String {
        self.strategy.name()
    }

    /// Lets the strategy see the bars completed since it last looked, trading on `account`: how many it saw, or
    /// an error when `bars` are not the ones it saw.
    pub fn follow(&mut self, bars: CandleView<'_>, account: &mut Account) -> Result<usize, &'static str> {
        let (time, open) = (bars.time(), bars.open());
        let at = time.partition_point(|&t| t < self.seen.0);
        if time.get(at) != Some(&self.seen.0) || open[at] != self.seen.1 {
            return Err("the chart shows other bars");
        }
        let mut seen = 0;
        for i in at + 1..bars.len() {
            // Bar i has opened: what was given at the close of the one before fills at its open.
            self.steps.fill_market(account, time[i], open[i], Time(time[i].0 - 1));
            if i + 1 == bars.len() {
                break;
            }
            self.steps.close(self.strategy.as_mut(), bars.slice(0..i + 1), account);
            self.seen = (time[i], open[i]);
            seen += 1;
        }
        Ok(seen)
    }

    /// The backtest of the bars it has seen, trading from where it started, with an account like `account`: what
    /// it would have done.
    pub fn backtest(&mut self, bars: CandleView<'_>, trades: TradeView<'_>, account: &Account) -> StrategyRun {
        let time = bars.time();
        let end = time.partition_point(|&t| t <= self.seen.0);
        let trades = match time.get(end) {
            Some(&next) => trades.before(next),
            None => trades,
        };
        let from = time.partition_point(|&t| t < self.from);
        backtest_from(
            self.shadow.as_mut(),
            bars.slice(0..end),
            trades,
            account.empty_like(),
            from,
        )
    }
}

/// The fills of a live run beside those of its backtest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillsCompared {
    pub live: usize,
    pub backtest: usize,
    /// Live fills with one of the backtest on the same bar and side.
    pub same: usize,
    /// How far the prices of those were apart, on average, as a fraction of the backtest's.
    pub off: f64,
}

impl FillsCompared {
    /// Pairs each of `live` with the first fill of `backtest` on the same bar and side not paired yet.
    pub fn of(live: &[Fill], backtest: &[Fill]) -> Self {
        let mut paired = vec![false; backtest.len()];
        let (mut same, mut off) = (0, 0.0);
        for fill in live {
            let start = backtest.partition_point(|b| b.time < fill.time);
            let found = (start..backtest.len())
                .take_while(|&j| backtest[j].time == fill.time)
                .find(|&j| !paired[j] && backtest[j].qty.signum() == fill.qty.signum());
            if let Some(j) = found {
                paired[j] = true;
                same += 1;
                off += (fill.price - backtest[j].price).abs() / backtest[j].price;
            }
        }
        Self {
            live: live.len(),
            backtest: backtest.len(),
            same,
            off: if same > 0 { off / same as f64 } else { 0.0 },
        }
    }
}

/// An order of a strategy from when it was placed to when it filled or was taken off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderLife {
    pub order: Order,
    /// The bar it was placed on.
    pub placed: Time,
    /// The bar it filled or was taken off on; `None` while it waits.
    pub ended: Option<Time>,
    pub filled: bool,
}

/// The orders of a run as they come and go, told apart by comparing the account's book before and after each step.
#[derive(Default)]
struct Book {
    lives: Vec<OrderLife>,
    /// The lives still waiting, by their place in `lives`.
    open: Vec<usize>,
}

impl Book {
    /// Records what changed in the orders of `account` on the bar at `at`: orders gone ended there, filled by the
    /// fills from `filled_from` on (each to the order of its side nearest in price), taken off otherwise; new
    /// ones were placed there.
    fn track(&mut self, account: &Account, at: Time, filled_from: usize) {
        let orders = account.orders();
        if self.open.is_empty() && orders.is_empty() {
            return;
        }
        let mut taken = vec![false; orders.len()];
        let mut ended = Vec::new();
        self.open.retain(|&k| {
            let life = &mut self.lives[k];
            match (0..orders.len()).find(|&j| !taken[j] && orders[j] == life.order) {
                Some(j) => {
                    taken[j] = true;
                    true
                }
                None => {
                    life.ended = Some(at);
                    ended.push(k);
                    false
                }
            }
        });
        for fill in &account.fills()[filled_from..] {
            let off = |k: &usize| (self.lives[*k].order.price - fill.price).abs();
            let reached = ended
                .iter()
                .filter(|&&k| !self.lives[k].filled && self.lives[k].order.qty.signum() == fill.qty.signum())
                .min_by(|a, b| off(a).total_cmp(&off(b)))
                .copied();
            if let Some(k) = reached {
                self.lives[k].filled = true;
            }
        }
        for (_, &order) in orders.iter().enumerate().filter(|&(j, _)| !taken[j]) {
            self.open.push(self.lives.len());
            self.lives.push(OrderLife {
                order,
                placed: at,
                ended: None,
                filled: false,
            });
        }
    }
}

/// Long while the fast moving average is over the slow one, short while under: in at the cross, with all of the
/// equity. An example to start one's own from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovingAverageCross {
    /// The lengths of the averages, bars.
    pub fast: f64,
    pub slow: f64,
}

impl Default for MovingAverageCross {
    fn default() -> Self {
        Self { fast: 10.0, slow: 30.0 }
    }
}

impl Strategy for MovingAverageCross {
    fn name(&self) -> String {
        format!("Moving average cross {}/{}", self.fast, self.slow)
    }

    fn settings(&mut self) -> Vec<Setting<'_>> {
        vec![
            Setting {
                name: "Fast",
                value: &mut self.fast,
            },
            Setting {
                name: "Slow",
                value: &mut self.slow,
            },
        ]
    }

    fn another(&self) -> Option<Box<dyn Strategy>> {
        Some(Box::new(*self))
    }

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
        let (fast, fast_before) = ctx.line(Kind::Sma, &[self.fast]);
        let (slow, slow_before) = ctx.line(Kind::Sma, &[self.slow]);
        ctx.plot("Fast", fast);
        ctx.plot("Slow", slow);
        let up = fast > slow && fast_before <= slow_before;
        let down = fast < slow && fast_before >= slow_before;
        if !(up || down) {
            return;
        }
        let size = ctx.equity().max(0.0) / ctx.close();
        let want = if up { size } else { -size };
        let qty = want - ctx.position();
        ctx.market(qty, Exits::default());
    }
}

/// Buys when the RSI comes back up over its low level, with a stop under the bar's low and a target twice as far,
/// risking a share of the equity on each trade (at most all of it in the position). An example of exits and a size
/// from the risk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RsiReversal {
    pub length: f64,
    pub level: f64,
    /// The share of the equity lost at the stop, percent.
    pub risk: f64,
}

impl Default for RsiReversal {
    fn default() -> Self {
        Self {
            length: 14.0,
            level: 30.0,
            risk: 1.0,
        }
    }
}

impl Strategy for RsiReversal {
    fn name(&self) -> String {
        format!("RSI {} reversal from {}", self.length, self.level)
    }

    fn settings(&mut self) -> Vec<Setting<'_>> {
        vec![
            Setting {
                name: "RSI length",
                value: &mut self.length,
            },
            Setting {
                name: "Level",
                value: &mut self.level,
            },
            Setting {
                name: "Risk %",
                value: &mut self.risk,
            },
        ]
    }

    fn another(&self) -> Option<Box<dyn Strategy>> {
        Some(Box::new(*self))
    }

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
        let (rsi, before) = ctx.line(Kind::Rsi, &[self.length]);
        if ctx.position() != 0.0 || !(before < self.level && rsi >= self.level) {
            return;
        }
        let (entry, stop) = (ctx.close(), ctx.low());
        if stop >= entry {
            return;
        }
        let exits = Exits {
            take_profit: Some(entry + 2.0 * (entry - stop)),
            stop_loss: Some(stop),
            break_even: None,
        };
        // No more than the equity buys: no borrowing.
        let size = ctx
            .risk_size(self.risk / 100.0, entry, stop)
            .min(ctx.equity().max(0.0) / entry);
        ctx.buy(size, exits);
    }
}

/// What a strategy did over the bars: shown on the chart, its trades over the candles and its equity in a pane.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyRun {
    pub name: String,
    pub account: Account,
    /// The lines it drew, by the bars it ran over.
    pub plots: Vec<Plot>,
    /// Its orders, in the order they were placed: where they stood and how they ended.
    pub orders: Vec<OrderLife>,
    /// What it noted, by the bar it noted it on (see [`StrategyCtx::note`]).
    pub notes: Vec<(Time, String)>,
    /// The indicators it read, with their values over the bars.
    pub inputs: Vec<Input>,
    /// The trade picked in the list, by its place in [`closed_trades`]: drawn brighter.
    pub selected: Option<usize>,
    /// The bar whose close decided the trade looked at: marked on the chart.
    pub decided: Option<Time>,
    /// How its trades differ from the run before a change of settings; `None` when nothing was changed.
    pub changes: Option<TradeChanges>,
    /// When the trades marked as wrong went in.
    pub marked: Vec<Time>,
    /// The bars kept for testing it, at the end: shaded on the chart.
    pub test: Option<TestPeriod>,
    /// The trades and stats of `account` as the run left it, worked out once rather than on every frame.
    trades: Vec<ClosedTrade>,
    stats: Stats,
}

/// The last bars, kept apart from choosing a strategy's settings to test it on what it was not fitted to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestPeriod {
    /// The open of its first bar.
    pub from: Time,
    /// Whether the run went over it; until then the strategy has not seen those bars.
    pub run: bool,
}

/// An indicator a strategy read: one series per line, NaN where it has no value.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub study: Study,
    pub lines: Vec<Vec<f64>>,
}

impl StrategyRun {
    /// Its trades, as [`closed_trades`] gives them for its fills.
    pub fn trades(&self) -> &[ClosedTrade] {
        &self.trades
    }

    /// How its fills went, as [`Account::stats`] gives it.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The bar whose close decided the entry at `entry` (`long` or short), of the bars at `times` it ran over: where
    /// its entry order was placed, or the bar before a market entry's fill.
    pub fn decision(&self, entry: Time, long: bool, times: &[Time]) -> Option<usize> {
        let bar = |t: Time| times.partition_point(|&x| x <= t).checked_sub(1);
        let entry = bar(entry)?;
        let placed = self.orders.iter().find(|life| {
            life.filled
                && !life.order.reduce_only
                && (life.order.qty > 0.0) == long
                && life.ended.and_then(bar) == Some(entry)
        });
        match placed {
            Some(life) => bar(life.placed),
            None => entry.checked_sub(1),
        }
    }
}

/// A line a strategy drew: a value per bar, NaN where it has none.
#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    pub name: String,
    pub color: Color,
    pub values: Vec<f64>,
}

/// A trade from going in to coming out: a fill that closes (part of) a position, and where that position began.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClosedTrade {
    pub long: bool,
    /// The fill that opened the position, and its average price.
    pub entry: Time,
    pub entry_price: f64,
    pub exit: Time,
    pub exit_price: f64,
    /// How much of the position this closed.
    pub qty: f64,
    /// After the fees of its part of the position.
    pub result: f64,
}

/// How the trades of a run differ from those of the run before a change of settings, matched by when they went in
/// and their side.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeChanges {
    /// Trades of the run before that this one has not.
    pub gone: Vec<ClosedTrade>,
    /// Trades of this run the run before had not, by their place in [`closed_trades`].
    pub new: Vec<usize>,
    /// Trades of this run that went in as one before did but came out elsewhere or at another price.
    pub changed: Vec<usize>,
}

impl TradeChanges {
    pub fn between(before: &[ClosedTrade], after: &[ClosedTrade]) -> Self {
        let mut waiting: std::collections::HashMap<(i64, bool), Vec<usize>> = Default::default();
        for (i, trade) in before.iter().enumerate().rev() {
            waiting.entry((trade.entry.0, trade.long)).or_default().push(i);
        }
        let mut changes = Self::default();
        let mut matched = vec![false; before.len()];
        for (i, trade) in after.iter().enumerate() {
            // The parts of a position closed one by one pair up in order.
            match waiting.get_mut(&(trade.entry.0, trade.long)).and_then(Vec::pop) {
                Some(j) => {
                    matched[j] = true;
                    let was = &before[j];
                    if was.exit != trade.exit || was.exit_price != trade.exit_price {
                        changes.changed.push(i);
                    }
                }
                None => changes.new.push(i),
            }
        }
        changes.gone = before
            .iter()
            .zip(&matched)
            .filter(|(_, m)| !**m)
            .map(|(t, _)| *t)
            .collect();
        changes
    }

    pub fn is_empty(&self) -> bool {
        self.gone.is_empty() && self.new.is_empty() && self.changed.is_empty()
    }
}

/// When the position open at the end of `fills` went in, and whether it is long; `None` when flat.
pub fn open_entry(fills: &[Fill]) -> Option<(Time, bool)> {
    let mut position = 0.0_f64;
    let mut entry = None;
    for fill in fills {
        let before = position;
        position = super::trading::add_position(position, fill.qty);
        if position == 0.0 {
            entry = None;
        } else if before == 0.0 || before.signum() != position.signum() {
            entry = Some((fill.time, position > 0.0));
        }
    }
    entry
}

/// The trades of `fills`, in the order they closed.
pub fn closed_trades(fills: &[Fill]) -> Vec<ClosedTrade> {
    let results = Account::results(fills);
    let mut trades = Vec::new();
    let mut position = 0.0_f64;
    // The open position: when it went in, and its average price.
    let mut entry: Option<(Time, f64)> = None;
    for (fill, result) in fills.iter().zip(results) {
        let before = position;
        position = super::trading::add_position(position, fill.qty);
        if let (Some((time, price)), Some(result)) = (entry, result) {
            trades.push(ClosedTrade {
                long: before > 0.0,
                entry: time,
                entry_price: price,
                exit: fill.time,
                exit_price: fill.price,
                qty: fill.qty.abs().min(before.abs()),
                result,
            });
        }
        if before == 0.0 || position != 0.0 && before.signum() != position.signum() {
            entry = Some((fill.time, fill.price));
        } else if position.abs() > before.abs() {
            entry = entry.map(|(time, average)| {
                let added = (average * before.abs() + fill.price * fill.qty.abs()) / position.abs();
                (time, added)
            });
        }
        if position == 0.0 {
            entry = None;
        }
    }
    trades
}

/// Shows a run on the chart, or takes the one shown off.
pub fn set_strategy_run(chart: &mut Chart, run: Option<StrategyRun>) {
    let Some(run) = run else {
        chart.resources_mut().remove::<StrategyRun>();
        chart.remove_layer(names::STRATEGY);
        chart.remove_pane(names::STRATEGY_EQUITY);
        return;
    };
    chart.resources_mut().insert(run);
    chart.add_layer(names::STRATEGY, order::STRATEGY, StrategyLayer::default());
    if !chart.pane_names().contains(&names::STRATEGY_EQUITY) {
        chart.add_plugin(Equity {
            name: names::STRATEGY_EQUITY,
            account: |res| res.get::<StrategyRun>().map(|run| &run.account),
            title: "Strategy equity",
            ..Equity::new()
        });
    }
}

/// The colors of a strategy's buys and sells: apart from the candles and from the fills of manual trading.
const BUY: Color = crate::palette::CYAN;
const SELL: Color = crate::palette::MAGENTA;

/// The trades of a [`StrategyRun`]: an arrow at each fill, under the bar for a buy and over it for a sell, named
/// when there is room, and a dotted line from where each trade went in to where it came out, green for a gain.
#[derive(Default)]
struct StrategyLayer {
    placed: Placed,
}

/// Where the fills, trades and orders of a run fall on the bars: worked out once for a run and its bars rather
/// than on every frame, and again when either changes.
#[derive(Default)]
struct Placed {
    /// What it was worked out from: the fills, when the orders were placed and ended, the trades a change took
    /// away, and the bars (how many, the times of the first two and the last).
    fills: Vec<Fill>,
    orders: Vec<(Time, Option<Time>)>,
    gone: Vec<ClosedTrade>,
    bars: (usize, [Option<Time>; 3]),
    /// Of each fill on the bars: its place in the fills, its bar, and what it did to the position.
    fill_bars: Vec<(usize, usize, &'static str)>,
    /// Of each trade on the bars (by its place in [`StrategyRun::trades`]), and of each trade gone: its place and
    /// the bars it went in and came out on.
    trades: Vec<(usize, usize, usize)>,
    ghosts: Vec<(usize, usize, usize)>,
    /// Of each order placed on the bars: its place, the bar it was placed on and the one it ended on (the last
    /// while it waits).
    lives: Vec<(usize, usize, usize)>,
}

impl Placed {
    fn update(&mut self, run: &StrategyRun, times: &[Time]) {
        let fills = run.account.fills();
        let gone = run.changes.as_ref().map_or(&[][..], |c| &c.gone[..]);
        let bars = (
            times.len(),
            [times.first().copied(), times.get(1).copied(), times.last().copied()],
        );
        let orders = run.orders.iter().map(|life| (life.placed, life.ended));
        if self.bars == bars && self.fills == fills && self.gone == gone && self.orders.iter().copied().eq(orders) {
            return;
        }
        // The bar that holds a moment: on a higher timeframe that is a period, not a bar.
        let bar = |t: Time| times.partition_point(|&x| x <= t).checked_sub(1);
        let span = |(k, (entry, exit)): (usize, (Time, Time))| Some((k, bar(entry)?, bar(exit)?));
        let mut position = 0.0_f64;
        self.fill_bars = fills
            .iter()
            .enumerate()
            .filter_map(|(k, fill)| {
                let before = position;
                position = super::trading::add_position(position, fill.qty);
                let name = if position == 0.0 {
                    "Close"
                } else if before == 0.0 || before.signum() != position.signum() {
                    if fill.qty > 0.0 { "Long" } else { "Short" }
                } else if position.abs() > before.abs() {
                    "Add"
                } else {
                    "Reduce"
                };
                Some((k, bar(fill.time)?, name))
            })
            .collect();
        let times_of = |t: &ClosedTrade| (t.entry, t.exit);
        self.trades = run.trades().iter().map(times_of).enumerate().filter_map(span).collect();
        self.ghosts = gone.iter().map(times_of).enumerate().filter_map(span).collect();
        let last = times.len().saturating_sub(1);
        self.lives = run
            .orders
            .iter()
            .enumerate()
            .filter_map(|(k, life)| Some((k, bar(life.placed)?, life.ended.and_then(bar).unwrap_or(last))))
            .collect();
        (self.fills, self.gone, self.bars) = (fills.to_vec(), gone.to_vec(), bars);
        self.orders = run.orders.iter().map(|life| (life.placed, life.ended)).collect();
    }
}

impl Layer for StrategyLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let Some(run) = ctx.res.get::<StrategyRun>() else {
            return;
        };
        let view = ctx.view;
        if view.is_empty() {
            return;
        }
        let style = ctx.style;
        let unit = ctx.line();
        let times = view.time();
        // The bar that holds a moment: on a higher timeframe that is a period, not a bar.
        let bar = |t: Time| times.partition_point(|&x| x <= t).checked_sub(1);
        let x_of = |i: usize| ctx.time.x_of(i as f64 + 0.5) as f32;
        let visible = ctx.visible.clone();
        self.placed.update(run, times);
        let placed = &self.placed;

        let around = visible.start.saturating_sub(1)..(visible.end + 1).min(view.len());
        let width = ctx.px(1.5);
        for plot in &run.plots {
            let mut previous: Option<(f32, f32)> = None;
            for i in around.clone() {
                let point = plot
                    .values
                    .get(i)
                    .filter(|v| v.is_finite())
                    .map(|&v| (x_of(i), ctx.price.y_of(v) as f32));
                if let (Some(from), Some(to)) = (previous, point) {
                    ctx.scene.line(from, to, width, plot.color);
                }
                previous = point;
            }
        }

        // The test period: shaded from its first bar on, named at its foot.
        if let Some(test) = run.test {
            let first = times.partition_point(|&x| x < test.from);
            let x = (ctx.time.x_of(first as f64) as f32).max(0.0);
            if x < ctx.plot.0 {
                let shade = Color {
                    a: 16,
                    ..style.hud_text
                };
                ctx.scene.fill(x, 0.0, ctx.plot.0 - x, ctx.plot.1, shade);
                let name = if test.run { "Test" } else { "Test, kept apart" };
                let size = ctx.px(style.font_size * 0.85);
                let y = ctx.plot.1 - ctx.px(10.0);
                ctx.scene
                    .text(x + ctx.px(6.0), y, size, style.hud_text, Align::Left, name);
            }
        }

        // The trades a change of settings took away, as ghosts: what they were, dashed and grey.
        let ghost = Color {
            a: 150,
            ..style.hud_text
        };
        let label = ctx.px(style.font_size * 0.85);
        let in_view = |from: usize, to: usize| from <= visible.end && to + 1 >= visible.start;
        for &(k, from, to) in &placed.ghosts {
            let trade = &placed.gone[k];
            if !in_view(from, to) {
                continue;
            }
            let a = (x_of(from), ctx.price.y_of(trade.entry_price) as f32);
            let b = (x_of(to), ctx.price.y_of(trade.exit_price) as f32);
            Pen::new(Dash::Dashed, unit, ghost, ctx.px(1.0)).line(ctx.scene, (a, b));
            ctx.scene.fill(a.0 - unit, a.1 - unit, 2.0 * unit, 2.0 * unit, ghost);
            if run.marked.contains(&trade.entry) {
                let y = a.1 - ctx.px(14.0);
                ctx.scene.text(a.0, y, label, ghost, Align::Center, "Wrong, gone");
            }
        }

        let trades = run.trades();
        for &(k, from, to) in &placed.trades {
            let trade = &trades[k];
            if !in_view(from, to) {
                continue;
            }
            let picked = run.selected == Some(k);
            let color = Color {
                a: if picked { 255 } else { 200 },
                ..if trade.result >= 0.0 {
                    style.candle.up
                } else {
                    style.candle.down
                }
            };
            let a = (x_of(from), ctx.price.y_of(trade.entry_price) as f32);
            let b = (x_of(to), ctx.price.y_of(trade.exit_price) as f32);
            if picked {
                ctx.scene.line(a, b, 2.0 * unit, color);
            } else {
                Pen::new(Dash::Dotted, unit, color, ctx.px(1.0)).line(ctx.scene, (a, b));
            }
            if run.marked.contains(&trade.entry) {
                let y = a.1 - ctx.px(14.0);
                ctx.scene.text(a.0, y, label, style.candle.down, Align::Center, "Wrong");
            }
        }

        // The bar that decided the trade looked at: a band over it, under the arrows, and what it read and noted
        // there in a tag at the top, over everything.
        let mut why = None;
        if let Some(i) = run.decided.and_then(bar).filter(|i| visible.contains(i)) {
            let w = (ctx.time.bar_spacing as f32).max(ctx.px(3.0));
            let band = Color {
                a: 40,
                ..style.hud_text
            };
            ctx.scene.fill(x_of(i) - w / 2.0, 0.0, w, ctx.plot.1, band);
            let at = |input: &Input, bar: usize| {
                let values: Vec<String> = input
                    .lines
                    .iter()
                    .map(|line| line.get(bar).map_or("-".into(), |&v| super::drawings::price_text(v, v)))
                    .collect();
                values.join(" / ")
            };
            let mut lines: Vec<String> = run
                .inputs
                .iter()
                .map(|input| {
                    let was = i.checked_sub(1).map_or("-".into(), |b| at(input, b));
                    format!("{} {} (was {was})", input.study.label(), at(input, i))
                })
                .collect();
            let time = times[i];
            lines.extend(
                run.notes
                    .iter()
                    .filter(|(t, _)| *t == time)
                    .map(|(_, note)| note.clone()),
            );
            if !lines.is_empty() {
                why = Some((x_of(i), lines));
            }
        }

        let named = ctx.time.bar_spacing >= ctx.px(5.0) as f64;
        let size = ctx.px(style.font_size * 0.85);
        let (rows, gap) = (5, ctx.px(3.0));

        // Its orders: a dashed line at the price from the bar it was placed on to the one it ended on (the last
        // bar while it waits, named), a cross where it was taken off.
        for &(k, from, to) in &placed.lives {
            let life = &run.orders[k];
            if from > visible.end || to + 1 < visible.start {
                continue;
            }
            let order = life.order;
            // An exit by what it does, as on the trading page: a take profit up, a stop loss down, whatever the
            // side; an entry by its side.
            let color = Color {
                a: 190,
                ..match (order.reduce_only, order.kind) {
                    (true, OrderKind::Limit) => style.candle.up,
                    (true, OrderKind::Stop) => style.candle.down,
                    _ if order.qty > 0.0 => BUY,
                    _ => SELL,
                }
            };
            let y = ctx.price.y_of(order.price) as f32;
            let (a, b) = (x_of(from), x_of(to));
            Pen::new(Dash::Dashed, unit, color, ctx.px(1.0)).line(ctx.scene, ((a, y), (b, y)));
            match life.ended {
                Some(_) if !life.filled => {
                    let r = ctx.px(3.0);
                    ctx.scene.line((b - r, y - r), (b + r, y + r), unit, color);
                    ctx.scene.line((b - r, y + r), (b + r, y - r), unit, color);
                }
                Some(_) => {}
                None => {
                    let name = match (order.kind, order.reduce_only, order.qty > 0.0) {
                        (OrderKind::Stop, true, _) => "Stop loss",
                        (OrderKind::Limit, true, _) => "Take profit",
                        (OrderKind::Stop, false, true) => "Buy stop",
                        (OrderKind::Stop, false, false) => "Sell stop",
                        (OrderKind::Limit, false, true) => "Buy limit",
                        (OrderKind::Limit, false, false) => "Sell limit",
                    };
                    ctx.scene.text(b + ctx.px(8.0), y, size, color, Align::Left, name);
                }
            }
        }
        // In a replay, what the strategy did on the bar just revealed is labelled with its price, whatever the
        // zoom.
        let replaying = ctx.res.get::<super::Replay>().is_some_and(|r| r.active);
        let mut fresh = Vec::new();
        let fills = run.account.fills();
        for &(k, i, name) in placed.fill_bars.iter().filter(|f| visible.contains(&f.1)) {
            let fill = &fills[k];
            let buy = fill.qty > 0.0;
            let x = (x_of(i) / unit).round() * unit;
            let (color, start) = if buy {
                (BUY, ctx.price.y_of(view.low()[i]) as f32 + gap)
            } else {
                (SELL, ctx.price.y_of(view.high()[i]) as f32 - gap)
            };
            let h = rows as f32 * unit;
            let just_now = replaying && i + 1 == view.len();
            for row in 0..rows {
                let w = (2 * row + 1) as f32 * unit;
                let y = if buy {
                    start + row as f32 * unit
                } else {
                    start - (row + 1) as f32 * unit
                };
                ctx.scene.fill(x - w / 2.0, y, w, unit, color);
            }
            if just_now {
                let reach = h + ctx.px(14.0);
                let y = if buy { start + reach } else { start - reach };
                let text = format!("{name} {}", super::drawings::price_text(fill.price, fill.price));
                fresh.push(((x, y), text, color));
            } else if named {
                let reach = h + size * 0.7;
                let y = if buy { start + reach } else { start - reach };
                ctx.scene.text(x, y, size, color, Align::Center, name);
            }
        }
        // Over the arrows, as more than one fill can be on the bar.
        for (at, text, color) in fresh {
            let ink = ctx.style.ink(color);
            super::drawings::tag(ctx, at, &[text], color, ink);
        }
        // Its name and its lines' values at the bar under the pointer, or at the newest: a row of the legend, under
        // the indicators over the candles.
        let overlays = ctx
            .res
            .get::<super::Studies>()
            .map_or(0, |s| s.items().iter().filter(|s| !s.kind.own_pane()).count());
        let i = ctx
            .chart_pointer()
            .filter(|&(x, y)| (0.0..ctx.plot.0).contains(&x) && (0.0..ctx.pane.chart_plot.1).contains(&y))
            .map(|(x, _)| ctx.time.index_of(x as f64).floor())
            .filter(|&i| i >= 0.0 && (i as usize) < view.len())
            .map_or(view.len() - 1, |i| i as usize);
        let font = tickerlight_text::Font::ui();
        let (size, gap, pad) = (ctx.px(style.font_size), ctx.px(8.0), ctx.px(4.0));
        let (x, y, h) = (ctx.px(12.0), ctx.px(16.0 + 20.0 * (overlays + 1) as f32), size * 1.6);
        let mut end = x + font.measure(&run.name, size);
        let values: Vec<(f32, String, Color)> = run
            .plots
            .iter()
            .map(|plot| {
                let text = match plot.values.get(i).filter(|v| v.is_finite()) {
                    Some(&v) => super::drawings::price_text(v, v),
                    None => "-".into(),
                };
                let at = end + gap;
                end = at + font.measure(&text, size);
                (at, text, plot.color)
            })
            .collect();
        let backdrop = Color {
            a: 220,
            ..style.background
        };
        ctx.scene
            .fill_rounded(x - pad, y - h / 2.0, end - x + 2.0 * pad, h, pad, backdrop);
        ctx.scene
            .text(x, y, size, style.axis_text, Align::Left, run.name.as_str());
        for (at, text, color) in values {
            ctx.scene.text(at, y, size, color, Align::Left, text);
        }
        if let Some((x, lines)) = why {
            let fill = Color {
                a: 235,
                ..style.hud_text
            };
            // Under the legend row at the top of the plot.
            let row = ctx.px(style.font_size) * 1.45;
            let y = ctx.px(34.0) + (row * lines.len() as f32 + ctx.px(6.0)) / 2.0;
            super::drawings::tag(ctx, (x, y), &lines, fill, fill.ink());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles};

    fn bars(closes: &[f64]) -> Candles {
        let mut candles = Candles::new();
        for (i, &c) in closes.iter().enumerate() {
            candles
                .push(Candle {
                    time: Time(i as i64 * 60_000),
                    open: c,
                    high: c + 1.0,
                    low: c - 1.0,
                    close: c,
                    volume: 1.0,
                })
                .unwrap();
        }
        candles
    }

    /// Records what it was shown, and buys once at a given bar.
    struct Probe {
        seen: Vec<usize>,
        buy_at: usize,
    }

    impl Strategy for Probe {
        fn name(&self) -> String {
            "probe".into()
        }

        fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
            assert_eq!(ctx.bars().len(), ctx.index() + 1);
            self.seen.push(ctx.index());
            if ctx.index() == self.buy_at {
                ctx.buy(2.0, Exits::default());
                assert_eq!(ctx.position(), 2.0, "counted before it fills");
            }
        }
    }

    #[test]
    fn a_strategy_sees_each_bar_once_and_its_order_fills_at_the_next_open() {
        let data = bars(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let mut probe = Probe {
            seen: Vec::new(),
            buy_at: 1,
        };
        let account = backtest(&mut probe, data.view(), Account::default()).account;
        assert_eq!(probe.seen, [0, 1, 2, 3, 4]);
        let fill = account.fills()[0];
        assert_eq!((fill.time, fill.price, fill.qty), (Time(2 * 60_000), 12.0, 2.0));
        assert_eq!(account.position(), 2.0);
    }

    /// Buys at the first bar with exits.
    struct BuyOnce(Exits);

    impl Strategy for BuyOnce {
        fn name(&self) -> String {
            "buy once".into()
        }

        fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
            if ctx.index() == 0 {
                ctx.buy(1.0, self.0);
            }
        }
    }

    /// A buy limit far below, taken off at bar 3; a market buy at bar 4 with a stop and a target; a sell limit far
    /// above at bar 8, left waiting.
    struct Orders;

    impl Strategy for Orders {
        fn name(&self) -> String {
            "orders".into()
        }

        fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
            let exits = Exits {
                take_profit: Some(12.0),
                stop_loss: Some(8.5),
                break_even: None,
            };
            match ctx.index() {
                0 => ctx.limit(1.0, 5.0, Exits::default()),
                3 => ctx.cancel_orders(),
                4 => ctx.buy(1.0, exits),
                8 => ctx.limit(-1.0, 100.0, Exits::default()),
                _ => {}
            }
        }
    }

    #[test]
    fn the_bar_that_decided_a_trade_is_where_its_rule_held_by_the_indicators_it_read() {
        let data = crate::sample::random_walk(3000);
        let run = backtest(&mut RsiReversal::default(), data.view(), Account::default());
        let trades = closed_trades(run.account.fills());
        assert!(trades.len() > 5, "{}", trades.len());
        let rsi = &run.inputs.iter().find(|i| i.study.kind == Kind::Rsi).unwrap().lines[0];
        let times = data.view().time();
        for trade in &trades {
            // A market entry: decided at the close before its fill.
            let d = run.decision(trade.entry, trade.long, times).unwrap();
            assert_eq!(times[d + 1], trade.entry);
            assert!(
                rsi[d - 1] < 30.0 && rsi[d] >= 30.0,
                "RSI {} then {}",
                rsi[d - 1],
                rsi[d]
            );
        }

        // A limit entry: decided where it was placed, not at the bar before its fill.
        struct LimitAt1;
        impl Strategy for LimitAt1 {
            fn name(&self) -> String {
                "limit".into()
            }
            fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
                if ctx.index() == 1 {
                    ctx.note("under the range");
                    ctx.limit(1.0, 7.5, Exits::default());
                }
                if ctx.index() == 5 {
                    ctx.close_position();
                }
            }
        }
        let data = bars(&[10.0, 10.0, 10.0, 8.0, 8.0, 8.0, 8.0]);
        let run = backtest(&mut LimitAt1, data.view(), Account::default());
        let trade = closed_trades(run.account.fills())[0];
        assert_eq!(trade.entry, Time(3 * 60_000));
        assert_eq!(run.decision(trade.entry, trade.long, data.view().time()), Some(1));
        assert_eq!(run.notes, [(Time(60_000), "under the range".to_string())]);
    }

    #[test]
    fn trades_after_a_change_are_matched_by_entry_and_side_to_those_before() {
        let trade = |entry: i64, long: bool, exit: i64, exit_price: f64| ClosedTrade {
            long,
            entry: Time(entry),
            entry_price: 10.0,
            exit: Time(exit),
            exit_price,
            qty: 1.0,
            result: 0.0,
        };
        let before = [
            trade(1, true, 2, 11.0),
            // A position closed in two parts.
            trade(3, false, 4, 9.0),
            trade(3, false, 5, 8.0),
            trade(6, true, 7, 12.0),
            trade(8, true, 9, 12.0),
        ];
        let after = [
            // The same, but bigger: not a change.
            ClosedTrade {
                qty: 2.0,
                result: 5.0,
                ..before[0]
            },
            // Only its first part is left, and it came out later.
            trade(3, false, 5, 9.0),
            // Went in at the same time on the other side: another trade.
            trade(6, false, 7, 12.0),
            // Came out at another price.
            trade(8, true, 9, 13.0),
        ];
        let changes = TradeChanges::between(&before, &after);
        assert_eq!(changes.new, [2]);
        assert_eq!(changes.changed, [1, 3]);
        assert_eq!(changes.gone, [before[2], before[3]]);
        assert!(TradeChanges::between(&before, &before).is_empty());
    }

    #[test]
    fn a_run_keeps_each_order_from_where_it_was_placed_to_how_it_ended() {
        let data = bars(&[10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 7.0, 7.0, 7.0]);
        let run = backtest(&mut Orders, data.view(), Account::default());
        let t = |bar: i64| Time(bar * 60_000);
        let lives: Vec<_> = run
            .orders
            .iter()
            .map(|l| (l.order.kind, l.order.price, l.placed, l.ended, l.filled))
            .collect();
        assert_eq!(
            lives,
            [
                (OrderKind::Limit, 5.0, t(0), Some(t(3)), false),
                // The exits of the entry filled at bar 5: the stop is reached at bar 7, the target goes with it.
                (OrderKind::Limit, 12.0, t(5), Some(t(7)), false),
                (OrderKind::Stop, 8.5, t(5), Some(t(7)), true),
                (OrderKind::Limit, 100.0, t(8), None, false),
            ]
        );
    }

    #[test]
    fn with_the_tape_a_backtest_s_exits_work_on_the_bar_its_entry_fills_on() {
        let data = bars(&[10.0, 11.0, 12.0]);
        let exits = Exits {
            take_profit: Some(11.8),
            stop_loss: Some(10.2),
            break_even: None,
        };
        let exit = |run: StrategyRun| {
            let fill = run.account.fills()[1];
            (fill.time, fill.price)
        };
        let blind = backtest(&mut BuyOnce(exits), data.view(), Account::default());
        assert_eq!(exit(blind), (Time(2 * 60_000), 12.0), "the exits wait for the next bar");
        let mut tape = crate::Trades::new();
        for (ms, price) in [(60_000, 11.0), (60_001, 10.1), (60_002, 12.0), (120_000, 12.0)] {
            let (time, side) = (Time(ms), crate::Side::Sell);
            tape.push(crate::Trade {
                time,
                price,
                qty: 1.0,
                side,
            })
            .unwrap();
        }
        let run = backtest_with_tape(&mut BuyOnce(exits), data.view(), tape.view(), Account::default());
        assert_eq!(run.account.fills()[0].price, 11.0, "in at the open");
        assert_eq!(
            exit(run),
            (Time(60_000), 10.1),
            "the stop, which the tape reached first"
        );
    }

    /// Trades for each of `bars`: its open, its low and high (the high first on a falling bar), its close.
    fn tape_of(bars: CandleView<'_>) -> crate::Trades {
        let mut tape = crate::Trades::new();
        for i in 0..bars.len() {
            let (open, high, low, close) = (bars.open()[i], bars.high()[i], bars.low()[i], bars.close()[i]);
            let path = if close >= open {
                [open, low, high, close]
            } else {
                [open, high, low, close]
            };
            for (k, price) in path.into_iter().enumerate() {
                let time = Time(bars.time()[i].0 + k as i64 * 15_000);
                let side = crate::Side::Buy;
                tape.push(crate::Trade {
                    time,
                    price,
                    qty: 1.0,
                    side,
                })
                .unwrap();
            }
        }
        tape
    }

    #[test]
    fn a_strategy_trading_live_does_what_its_backtest_of_the_same_bars_does() {
        let data = crate::sample::random_walk(1200);
        let tape = tape_of(data.view());
        let (bars, trades, time) = (data.view(), tape.view(), data.view().time());
        let strategies: [&dyn Strategy; 2] = [&RsiReversal::default(), &MovingAverageCross::default()];
        for strategy in strategies {
            // Started while bar 300 forms: what came before is warm-up.
            let start = 300;
            let shown = trades.before(Time(time[start].0 + 1));
            let mut live = LiveRun::new(strategy, bars.slice(0..start + 1), shown, &Account::default()).unwrap();
            assert_eq!(live.from, time[start]);
            let mut account = Account::default();
            for n in start + 2..=bars.len() {
                // The bar just completed: its trades met the orders waiting as they came.
                let completed = super::super::trading::bar_trades(trades, time, n - 2);
                account.match_live(completed, &time[..n]);
                assert_eq!(live.follow(bars.slice(0..n), &mut account), Ok(1));
            }
            let backtest = live.backtest(bars, trades, &Account::default());
            let name = strategy.name();
            assert!(closed_trades(account.fills()).len() >= 2, "{name}");
            assert!(
                account.fills()[0].time > time[start],
                "{name}: nothing from the warm-up"
            );
            assert_eq!(account.fills(), backtest.account.fills(), "{name}");
            let compared = FillsCompared::of(account.fills(), backtest.account.fills());
            assert_eq!((compared.same, compared.off), (compared.live, 0.0), "{name}");
            assert_eq!(
                live.follow(bars.slice(0..start), &mut account),
                Err("the chart shows other bars")
            );
        }
    }

    #[test]
    fn in_a_replay_a_fill_on_the_bar_just_shown_is_labelled_with_its_price() {
        use crate::plugin::commands;
        use crate::{Event, Scene};
        let closes: Vec<f64> = (0..60).map(|i| 100.0 + i as f64).collect();
        let mut chart = Chart::new(bars(&closes));
        chart.resize(800.0, 500.0);
        // Run over what the chart has seen, as the strategy panel does, then drawn.
        let marks = |chart: &mut Chart| -> Vec<String> {
            let mut probe = Probe {
                seen: Vec::new(),
                buy_at: 29,
            };
            let run = backtest(&mut probe, chart.seen(), Account::default());
            set_strategy_run(chart, Some(run));
            let mut scene = Scene::new();
            chart.build_scene(&mut scene);
            scene
                .texts
                .iter()
                .filter(|t| t.text.starts_with("Long "))
                .map(|t| t.text.clone())
                .collect()
        };
        // Bar 30, where the buy fills at the open (130), is the last one shown.
        chart
            .resources_mut()
            .get_mut::<super::super::Replay>()
            .unwrap()
            .start_at = Some(Time(30 * 60_000 + 1));
        chart.handle(Event::Command(commands::REPLAY_TOGGLE));
        let shown = marks(&mut chart);
        assert!(shown.len() == 1 && shown[0].contains("130"), "{shown:?}");
        // A bar later it is an arrow like the others.
        chart.handle(Event::Command(commands::REPLAY_STEP));
        assert_eq!(marks(&mut chart), Vec::<String>::new());
        // Out of the replay nothing is fresh.
        chart.handle(Event::Command(commands::REPLAY_TOGGLE));
        assert_eq!(marks(&mut chart), Vec::<String>::new());
    }

    #[test]
    fn the_cross_flips_the_position_at_each_cross() {
        // Down, up, down: two crosses of a 2 and a 4 bar average.
        let closes = [
            10.0, 9.0, 8.0, 7.0, 6.0, 8.0, 10.0, 12.0, 14.0, 12.0, 10.0, 8.0, 6.0, 4.0,
        ];
        let data = bars(&closes);
        let mut cross = MovingAverageCross { fast: 2.0, slow: 4.0 };
        let run = backtest(&mut cross, data.view(), Account::default());
        assert_eq!(
            run.plots.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Fast", "Slow"]
        );
        assert_eq!(run.plots[1].values.len(), closes.len());
        assert!(run.plots[1].values[2].is_nan() && run.plots[1].values[3] == 8.5);
        let account = run.account;
        let sides: Vec<f64> = account.fills().iter().map(|f| f.qty.signum()).collect();
        assert_eq!(
            sides,
            [1.0, -1.0],
            "long at the cross up, then reversed at the cross down"
        );
        assert!(account.position() < 0.0);
        // Reversed in one fill: the new short is as big as the long was.
        let (long, flip) = (account.fills()[0].qty, account.fills()[1].qty);
        assert!((flip + 2.0 * long).abs() / long < 0.3, "{long} {flip}");
    }

    #[test]
    fn indicators_seen_bar_by_bar_are_what_the_chart_shows() {
        let closes: Vec<f64> = (0..200).map(|i| 100.0 + (i as f64 * 0.3).sin() * 5.0).collect();
        let data = bars(&closes);
        struct Rsi(Vec<f64>);
        impl Strategy for Rsi {
            fn name(&self) -> String {
                "rsi".into()
            }
            fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
                let (now, _) = ctx.line(Kind::Rsi, &[14.0]);
                self.0.push(now);
            }
        }
        let mut rsi = Rsi(Vec::new());
        backtest(&mut rsi, data.view(), Account::default());
        let whole = super::super::compute(&Study::new(Kind::Rsi), data.view()).remove(0);
        for (a, b) in rsi.0.iter().zip(&whole) {
            assert!(a == b || (a.is_nan() && b.is_nan()), "{a} vs {b}");
        }
    }

    #[test]
    fn trades_go_from_where_the_position_began_to_where_it_closed() {
        let fill = |t: i64, price: f64, qty: f64| Fill {
            time: Time(t),
            price,
            qty,
            fee: 0.0,
        };
        // Long 1 at 10, add 1 at 12 (average 11), half out at 15, the rest and a short of 1 at 9, out at 8.
        let fills = [
            fill(1, 10.0, 1.0),
            fill(2, 12.0, 1.0),
            fill(3, 15.0, -1.0),
            fill(4, 9.0, -2.0),
            fill(5, 8.0, 1.0),
        ];
        let trades = closed_trades(&fills);
        let got: Vec<_> = trades
            .iter()
            .map(|t| (t.long, t.entry.0, t.entry_price, t.exit.0, t.qty, t.result))
            .collect();
        assert_eq!(
            got,
            [
                (true, 1, 11.0, 3, 1.0, 4.0),
                (true, 1, 11.0, 4, 1.0, -2.0),
                (false, 4, 9.0, 5, 1.0, 1.0)
            ]
        );
    }

    #[test]
    fn a_reversal_risks_its_share_at_the_stop() {
        let closes: Vec<f64> = (0..300).map(|i| 100.0 + (i as f64 * 0.15).sin() * 10.0).collect();
        let data = bars(&closes);
        let account = backtest(&mut RsiReversal::default(), data.view(), Account::default()).account;
        let results: Vec<f64> = Account::results(account.fills()).into_iter().flatten().collect();
        assert!(!results.is_empty(), "it traded");
        // A loss is about 1% of the equity (the fill is at the next open, not the close the size was taken at);
        // a win about twice that.
        for r in results {
            assert!(r > -0.015 * DEFAULT_BALANCE && r < 0.03 * DEFAULT_BALANCE * 1.5, "{r}");
        }
    }
}
