//! The strategy page: a strategy written in Rust run over the bars of the chart, its trades on the chart and how
//! it did.

use std::ops::Range;
use tickerlight_core::builtin::{
    Account, ClosedTrade, Fill, FillsCompared, Live, LiveRun, Replay, Strategy, StrategyRun, TestPeriod, TradeChanges,
    backtest, backtest_with_tape, closed_trades, features_csv, open_entry, set_strategy_run,
};
use tickerlight_core::plugin::commands;
use tickerlight_core::{Align, CandleView, Chart, Event, Time, TradeView, native_step};

use crate::panels::{count_trades, money, percent_of, price, restart_at, short_time};
use crate::{Cell, Column, Panel, Ui};

/// Bars shown before each entry while watching the strategy trade: what led to it.
const LEAD: usize = 10;

pub struct StrategyPanel {
    strategies: Vec<Box<dyn Strategy>>,
    /// How many of [`strategies`](Self::strategies) are the program's own; those from files come after.
    builtin: usize,
    files: Option<StrategyFiles>,
    /// What is wrong with the files that did not load.
    problems: Vec<String>,
    /// Where the features of the bars were written last, or why they were not.
    written: Option<Written>,
    selected: usize,
    /// What the setting fields show: typed text until it is taken.
    fields: Vec<String>,
    /// The field that had the keys at the last pass.
    editing: Option<usize>,
    /// The bars the run on the chart was made over, while one is: it runs again when they change.
    shown: Option<Bars>,
    /// Watching the strategy trade, while it lasts.
    watch: Option<Watch>,
    /// The trades marked as wrong: the strategy, by its place in the list, and when the trade went in.
    marks: Vec<(usize, Time)>,
    /// When watching pauses by itself.
    pause: Pause,
    /// What the pause fields show, and the one that had the keys at the last pass.
    pause_fields: Vec<String>,
    pause_editing: Option<usize>,
    /// The share of the bars at the end kept for the test, in percent; 0 for none.
    test: f64,
    test_fields: Vec<String>,
    test_editing: Option<usize>,
    /// The test was run: the strategy went over its bars too. A change of settings keeps them apart again.
    tested: bool,
    /// How many times the test was run since its share was set.
    looks: u32,
    /// How many test windows a walk forward has, and how many times longer than one its training windows are.
    windows: f64,
    training: f64,
    walk_fields: Vec<String>,
    walk_editing: Option<usize>,
    walk: Option<Walk>,
    /// The chosen strategy trading by itself on the paper account of a live chart.
    trading: Option<Trading>,
    /// It stops and closes all once the account has lost this share since it started, percent; 0 for never.
    stop_lost: f64,
    lost_fields: Vec<String>,
    lost_editing: Option<usize>,
    /// How trading by itself ended, or why it did not begin.
    trading_ended: Option<String>,
}

/// The loss a strategy trading by itself stops at, at first, percent.
const STOP_LOST: f64 = 5.0;

/// A strategy trading by itself, and the backtest of the bars it traded.
struct Trading {
    run: LiveRun,
    /// What the account was worth when it started, and how many fills it had.
    equity: f64,
    fills: usize,
    /// How many bars the chart showed when it last looked, and the newest: it looks again when they change.
    looked: (usize, Option<Time>),
    /// The fills of its backtest over the bars it has seen.
    backtest: Vec<Fill>,
}

/// The walk forward set up at first: test windows, and how many times longer the training windows are.
const WALK: (f64, f64) = (4.0, 3.0);

/// What each setting is tried at in a walk forward, times its value, one setting after another.
const TRIED: [f64; 4] = [0.5, 0.7, 1.4, 2.0];

/// A walk forward: on each training window the settings are chosen by the result they give there, then the window
/// after it is traded with them, so each test window is traded with settings chosen without it.
#[derive(Debug)]
struct Walk {
    /// How many bars it walks, from the first: the bars before the test when one is kept.
    bars: usize,
    /// The times of the first and the last of them: its windows are of these bars only.
    span: (Option<Time>, Option<Time>),
    /// The settings each search starts from: the strategy's when it began, set back after each run.
    from: Vec<f64>,
    windows: Vec<WalkWindow>,
    /// The window searched and the run in it: the settings as they were, each setting times each of [`TRIED`],
    /// then the test.
    at: (usize, usize),
    /// Why it stopped before the end.
    stopped: Option<&'static str>,
}

#[derive(Debug)]
struct WalkWindow {
    train: Range<usize>,
    test: Range<usize>,
    /// The settings that did best over the training bars so far, and their net result there.
    chosen: Vec<f64>,
    fitted: f64,
    /// How many trades went in during the test window, and their result.
    tested: Option<(usize, f64)>,
}

impl Walk {
    /// A walk over the bars at `times` from the settings `from`: `windows` test windows, each after a training
    /// window `training` times as long.
    fn new(times: &[Time], from: Vec<f64>, windows: usize, training: f64) -> Self {
        let bars = times.len();
        let test = (bars as f64 / (windows as f64 + training)).floor() as usize;
        let train = (test as f64 * training).round() as usize;
        let fits = windows > 0 && test >= 20 && train >= 20 && (windows - 1) * test + train + test <= bars;
        let windows = (0..if fits { windows } else { 0 })
            .map(|i| {
                let start = i * test;
                WalkWindow {
                    train: start..start + train,
                    test: start + train..start + train + test,
                    chosen: from.clone(),
                    fitted: f64::NEG_INFINITY,
                    tested: None,
                }
            })
            .collect();
        let stopped = (!fits).then_some("Too few bars for so many windows");
        Self {
            bars,
            span: (times.first().copied(), times.last().copied()),
            from,
            windows,
            at: (0, 0),
            stopped,
        }
    }

    /// Whether its windows are of the bars at `times`: the same series and timeframe, cut no shorter.
    fn of(&self, times: &[Time]) -> bool {
        let last = self.bars.checked_sub(1).and_then(|i| times.get(i)).copied();
        times.len() >= self.bars && (times.first().copied(), last) == self.span
    }

    fn runs_per_window(&self) -> usize {
        2 + self.from.len() * TRIED.len()
    }

    fn finished(&self) -> bool {
        self.stopped.is_some() || self.at.0 == self.windows.len()
    }

    /// Does the next run of `strategy` over `bars`, from an account like `account`.
    fn step(&mut self, strategy: &mut dyn Strategy, bars: CandleView<'_>, account: &Account) {
        let set = |strategy: &mut dyn Strategy, values: &[f64]| {
            for (setting, &value) in strategy.settings().into_iter().zip(values) {
                *setting.value = value;
            }
        };
        let (w, run) = self.at;
        let last = self.runs_per_window() - 1;
        self.at = if run < last { (w, run + 1) } else { (w + 1, 0) };
        let window = &mut self.windows[w];
        if run < last {
            let mut tried = window.chosen.clone();
            if run > 0 {
                let (s, times) = ((run - 1) / TRIED.len(), TRIED[(run - 1) % TRIED.len()]);
                let value = self.from[s] * times;
                // Whole settings (lengths, counts) stay whole.
                tried[s] = if self.from[s].fract() == 0.0 {
                    value.round().max(1.0)
                } else {
                    value
                };
                if tried[s] == window.chosen[s] {
                    return;
                }
            }
            set(strategy, &tried);
            let bars = bars.slice(window.train.clone());
            let result = backtest(strategy, bars, account.clone());
            let net = result.account.net(bars.close()[bars.len() - 1]);
            if net > window.fitted {
                (window.chosen, window.fitted) = (tried, net);
            }
        } else {
            // The training bars lead in, so what it reads is ready when the test window begins.
            set(strategy, &window.chosen);
            let bars = bars.slice(window.train.start..window.test.end);
            let from = bars.time()[window.train.len()];
            let result = backtest(strategy, bars, account.clone());
            let tested = result.trades().iter().filter(|t| t.entry >= from);
            window.tested = Some((tested.clone().count(), tested.map(|t| t.result).sum()));
        }
        set(strategy, &self.from);
    }
}

/// When watching pauses by itself, at the exit of a trade, so a trade worth a look is not played past.
#[derive(Clone, Copy, Default, PartialEq)]
struct Pause {
    /// At each losing trade.
    loss: bool,
    /// At a trade held more than this many bars; 0 for none.
    held: f64,
    /// At a loss of more than this percent of the balance; 0 for none.
    lost: f64,
}

impl Pause {
    fn at(&self, trade: &Watched, balance: f64) -> bool {
        (self.loss && trade.result < 0.0)
            || (self.held > 0.0 && trade.held as f64 > self.held)
            || (self.lost > 0.0 && -trade.result > self.lost / 100.0 * balance)
    }
}

/// Why the strategy runs again, which decides what becomes of how its trades differ from the run before.
#[derive(Clone, Copy, PartialEq)]
enum Again {
    /// The first run, or another strategy: nothing to compare with.
    Fresh,
    /// The bars changed: what the last change of settings did stays.
    Bars,
    /// Its settings or its file changed: compared with the run before.
    Changed,
}

/// Watching the strategy trade: the replay plays each trade from [`LEAD`] bars before its entry to its exit and
/// jumps over the time between.
struct Watch {
    /// The trades of the run over all the data.
    trades: Vec<Watched>,
    /// The balance the run began with: what a loss is a share of.
    balance: f64,
    /// The trade the replay last jumped to, so it never jumps to it again.
    jumped: Option<usize>,
    /// The last bar shown at the last look, to know which exits the replay went past; `None` after a jump.
    last: Option<Time>,
}

/// A trade being watched.
struct Watched {
    /// The last bar shown when its part starts.
    start: Time,
    exit: Time,
    /// The bar its exit is on.
    out: Time,
    /// How many bars it was held.
    held: usize,
    result: f64,
}

impl Watch {
    /// The trade to play next at `cursor`: the first that has not gone out yet.
    fn current(&self, cursor: Time) -> usize {
        self.trades
            .iter()
            .position(|trade| trade.exit > cursor)
            .unwrap_or(self.trades.len())
    }

    /// The trade being played at `cursor`: the last whose part has begun (the one before may not be out yet).
    fn playing(&self, cursor: Time) -> usize {
        self.trades
            .partition_point(|trade| trade.start <= cursor)
            .saturating_sub(1)
    }
}

/// Strategies, and what is wrong with the files that are not.
pub type ReadStrategies = (Vec<Box<dyn Strategy>>, Vec<String>);

/// Where a file went, or what went wrong.
pub type Written = Result<String, String>;

/// Strategies the user built and put in a folder, read again when asked.
pub struct StrategyFiles {
    /// Where they are, as the page tells it.
    pub folder: String,
    pub read: Box<dyn FnMut() -> ReadStrategies>,
    /// Writes the features of the bars (CSV, see [`features_csv`]) beside them, to train a model on elsewhere.
    pub features: Box<dyn FnMut(&str) -> Written>,
}

/// What tells the bars of the chart apart: a new series, another timeframe, a step of a replay, a new or changed
/// last bar, and how many trades they have (the tape settles fills).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bars {
    len: usize,
    first: Option<Time>,
    last: Option<Time>,
    close: Option<f64>,
    trades: usize,
    /// Bars of the data that are complete.
    complete: usize,
}

impl Bars {
    fn of(chart: &Chart) -> Self {
        let (seen, tape) = settled(chart);
        // A bar of a higher timeframe holding one still forming changes at every trade: the run waits for the
        // finer bars to complete.
        let forming = chart.forming();
        Self {
            len: seen.len(),
            first: seen.time().first().copied(),
            last: seen.time().last().copied(),
            close: seen.close().last().copied().filter(|_| !forming),
            trades: if forming { 0 } else { tape.len() },
            complete: chart.base().len() - usize::from(forming),
        }
    }
}

/// A field for each of `values`, by name. What is typed is taken when Enter is pressed or the field is left, so a
/// number typed digit by digit is not taken at each; `texts` and `editing` keep what the fields show and the one
/// that has the keys between passes. True when a value changed.
fn number_fields(
    ui: &mut Ui,
    values: &mut [(&str, &mut f64)],
    texts: &mut Vec<String>,
    editing: &mut Option<usize>,
) -> bool {
    let (mut changed, mut now) = (false, None);
    texts.resize_with(values.len(), String::new);
    for (j, (name, value)) in values.iter_mut().enumerate() {
        let text = &mut texts[j];
        let was = *editing == Some(j);
        if !was {
            *text = format!("{value}");
        }
        ui.columns(2, |ui| {
            ui.heading(name);
            let response = ui.text_field(text, 0.0, name);
            if response.focused {
                now = Some(j);
            }
            if (response.submitted || was && !response.focused)
                && let Ok(typed) = text.trim().replace(',', ".").parse::<f64>()
                && typed.is_finite()
                && typed != **value
            {
                **value = typed;
                changed = true;
            }
        });
    }
    *editing = now;
    changed
}

/// The bars and the tape a strategy runs over: what the chart shows, but a bar of the data still forming at a
/// replay's moment only once it is complete, as a strategy acts on whole bars. (On a higher timeframe the bar
/// that holds it stays, as the chart shows it.)
fn settled(chart: &Chart) -> (CandleView<'_>, TradeView<'_>) {
    let (seen, tape) = (chart.seen(), chart.seen_trades());
    match seen.time().last() {
        Some(&open) if chart.forming() && chart.timeframe().is_none() => {
            (seen.slice(0..seen.len() - 1), tape.before(open))
        }
        _ => (seen, tape),
    }
}

fn replaying(chart: &Chart) -> bool {
    chart.resources().get::<Replay>().is_some_and(|r| r.active)
}

/// The chart shows live bars, where paper orders fill by the live trades: live data, not in a replay.
fn on_live(chart: &Chart) -> bool {
    !replaying(chart) && chart.resources().get::<Live>().is_some_and(|l| l.0)
}

impl StrategyPanel {
    /// A page to choose among `strategies`, the first one chosen.
    pub fn new(strategies: Vec<Box<dyn Strategy>>) -> Self {
        Self {
            builtin: strategies.len(),
            strategies,
            files: None,
            problems: Vec::new(),
            written: None,
            selected: 0,
            fields: Vec::new(),
            editing: None,
            shown: None,
            watch: None,
            marks: Vec::new(),
            pause: Pause::default(),
            pause_fields: Vec::new(),
            pause_editing: None,
            test: 0.0,
            test_fields: Vec::new(),
            test_editing: None,
            tested: false,
            looks: 0,
            windows: WALK.0,
            training: WALK.1,
            walk_fields: Vec::new(),
            walk_editing: None,
            walk: None,
            trading: None,
            stop_lost: STOP_LOST,
            lost_fields: Vec::new(),
            lost_editing: None,
            trading_ended: None,
        }
    }

    /// With the strategies of `files` after these, read now.
    pub fn with_files(mut self, files: StrategyFiles) -> Self {
        self.files = Some(files);
        self.read_files();
        self
    }

    /// Reads the strategy files again: the choice and the settings stay where the same strategies are, and the
    /// one on the chart runs again.
    fn read_files(&mut self) {
        let Some(files) = &mut self.files else {
            return;
        };
        let (strategies, problems) = (files.read)();
        let kept = self.save();
        self.strategies.truncate(self.builtin);
        self.strategies.extend(strategies);
        self.problems = problems;
        self.selected = self.selected.min(self.strategies.len().saturating_sub(1));
        self.load(&kept);
        self.fields.clear();
        self.editing = None;
    }

    /// What to keep of the page: the strategy chosen, whether it is on the chart, the settings of each, the
    /// trades marked as wrong and when watching pauses, as text.
    pub fn save(&mut self) -> String {
        let mut text = format!("selected {}\nshown {}\n", self.selected, u8::from(self.shown.is_some()));
        for (strategy, entry) in &self.marks {
            text += &format!("mark {strategy} {}\n", entry.0);
        }
        let Pause { loss, held, lost } = self.pause;
        if self.pause != Pause::default() {
            text += &format!("pause {} {held} {lost}\n", u8::from(loss));
        }
        if self.test > 0.0 {
            text += &format!("test {} {}\n", self.test, self.looks);
        }
        if (self.windows, self.training) != WALK {
            text += &format!("walk {} {}\n", self.windows, self.training);
        }
        if self.stop_lost != STOP_LOST {
            text += &format!("lost {}\n", self.stop_lost);
        }
        for (i, strategy) in self.strategies.iter_mut().enumerate() {
            text += &i.to_string();
            for setting in strategy.settings() {
                text += &format!("\t{}={}", setting.name, setting.value);
            }
            text += "\n";
        }
        text
    }

    /// Takes back what [`save`](Self::save) wrote; what does not fit the strategies there are now is left out.
    /// A strategy that was on the chart runs once there are bars.
    pub fn load(&mut self, text: &str) {
        for line in text.lines() {
            let mut parts = line.split('\t');
            let head = parts.next().unwrap_or_default();
            match head.split_once(' ') {
                Some(("selected", i)) => {
                    if let Ok(i) = i.parse::<usize>()
                        && i < self.strategies.len()
                    {
                        self.selected = i;
                    }
                }
                // Bars no chart has: the first look at the chart runs it.
                Some(("shown", on)) => {
                    self.shown = (on == "1").then_some(Bars {
                        len: 0,
                        first: None,
                        last: None,
                        close: Some(f64::NAN),
                        trades: 0,
                        complete: 0,
                    })
                }
                Some(("pause", pause)) => {
                    let values: Vec<f64> = pause.split(' ').filter_map(|v| v.parse().ok()).collect();
                    if let [loss, held, lost] = values[..]
                        && held.is_finite()
                        && lost.is_finite()
                    {
                        self.pause = Pause {
                            loss: loss == 1.0,
                            held,
                            lost,
                        };
                    }
                }
                Some(("test", test)) => {
                    if let Some((Ok(share), Ok(looks))) =
                        test.split_once(' ').map(|(s, l)| (s.parse::<f64>(), l.parse::<u32>()))
                        && (0.0..=90.0).contains(&share)
                    {
                        (self.test, self.looks) = (share, looks);
                    }
                }
                Some(("walk", walk)) => {
                    if let Some((Ok(windows), Ok(training))) =
                        walk.split_once(' ').map(|(w, t)| (w.parse::<f64>(), t.parse::<f64>()))
                        && (1.0..=20.0).contains(&windows)
                        && (0.5..=20.0).contains(&training)
                    {
                        (self.windows, self.training) = (windows.round(), training);
                    }
                }
                Some(("lost", lost)) => {
                    if let Ok(lost) = lost.parse::<f64>()
                        && (0.0..=100.0).contains(&lost)
                    {
                        self.stop_lost = lost;
                    }
                }
                Some(("mark", mark)) => {
                    if let Some((Ok(strategy), Ok(entry))) = mark
                        .split_once(' ')
                        .map(|(s, t)| (s.parse::<usize>(), t.parse::<i64>()))
                        && strategy < self.strategies.len()
                    {
                        self.marks.push((strategy, Time(entry)));
                    }
                }
                _ => {
                    let Some(strategy) = head.parse::<usize>().ok().and_then(|i| self.strategies.get_mut(i)) else {
                        continue;
                    };
                    let mut settings = strategy.settings();
                    for (name, value) in parts.filter_map(|p| p.rsplit_once('=')) {
                        if let (Some(setting), Ok(value)) =
                            (settings.iter_mut().find(|s| s.name == name), value.parse::<f64>())
                            && value.is_finite()
                        {
                            *setting.value = value;
                        }
                    }
                }
            }
        }
    }

    /// Runs the strategy on the chart again if the bars changed: for when the page is not shown. In a replay that
    /// is at each step, so its trades come out from left to right, each when the bars that made it are there.
    pub fn follow(&mut self, chart: &mut Chart) {
        if self.shown.is_some_and(|bars| bars != Bars::of(chart)) {
            self.run(chart, Again::Bars);
        }
        self.keep_watching(chart);
        self.keep_trading(chart);
    }

    /// Starts watching the strategy trade from its trade `from`: out of a replay, the strategy runs over all the
    /// data to know its trades, then the replay plays from a little before that one.
    fn start_watching(&mut self, chart: &mut Chart, from: usize) {
        if replaying(chart) {
            chart.handle(Event::Command(commands::REPLAY_TOGGLE));
        }
        self.run(chart, Again::Bars);
        let Some(run) = chart.resources().get::<StrategyRun>() else {
            return;
        };
        let times = chart.seen().time();
        let bar = |t: Time| times.partition_point(|&x| x <= t).saturating_sub(1);
        let trades: Vec<Watched> = run
            .trades()
            .iter()
            .map(|trade| {
                let (entry, out) = (bar(trade.entry), bar(trade.exit));
                Watched {
                    start: times[entry.saturating_sub(LEAD)],
                    exit: trade.exit,
                    out: times[out],
                    held: out - entry,
                    result: trade.result,
                }
            })
            .collect();
        if trades.is_empty() {
            return;
        }
        let from = from.min(trades.len() - 1);
        self.watch = Some(Watch {
            trades,
            balance: run.account.balance,
            jumped: None,
            last: None,
        });
        // Watched at the usual bar spacing, whatever the zoom before ("All" shows a sliver): the replay then puts
        // the start where a start from the screen would be, so the trade plays in view.
        chart.handle(Event::Command(commands::VIEW_RESET));
        self.jump(chart, from);
    }

    /// Plays trade `i` of the watch: the replay starts again where its part does.
    fn jump(&mut self, chart: &mut Chart, i: usize) {
        let Some(watch) = &mut self.watch else {
            return;
        };
        let Some(start) = watch.trades.get(i).map(|trade| trade.start) else {
            return;
        };
        watch.jumped = Some(i);
        watch.last = None;
        // The bars that opened before this moment are shown: up to the start's one.
        restart_at(chart, Time(start.0 + 1));
        chart.handle(Event::Command(commands::REPLAY_PLAY_PAUSE));
    }

    /// While watching and playing: past the exit of a trade worth a look, a pause; past a trade's exit, on to the
    /// next one's start; after the last, a pause. Left with the replay.
    fn keep_watching(&mut self, chart: &mut Chart) {
        let pause = self.pause;
        let Some(watch) = &mut self.watch else {
            return;
        };
        let Some(replay) = chart.resources().get::<Replay>().filter(|r| r.active) else {
            self.watch = None;
            return;
        };
        let (playing, Some(cursor)) = (replay.playing, chart.cursor()) else {
            return;
        };
        let last = watch.last.replace(cursor);
        if !playing {
            return;
        }
        let past = |trade: &Watched| last.is_some_and(|last| last < trade.out && trade.out <= cursor);
        if watch
            .trades
            .iter()
            .any(|trade| past(trade) && pause.at(trade, watch.balance))
        {
            chart.handle(Event::Command(commands::REPLAY_PLAY_PAUSE));
            return;
        }
        let i = watch.current(cursor);
        match watch.trades.get(i).map(|trade| trade.start) {
            Some(start) if cursor < start && watch.jumped != Some(i) => self.jump(chart, i),
            Some(_) => {}
            None => self.stop_watching(chart),
        }
    }

    /// Ends the watch where it is, paused.
    fn stop_watching(&mut self, chart: &mut Chart) {
        self.watch = None;
        if chart.resources().get::<Replay>().is_some_and(|r| r.active && r.playing) {
            chart.handle(Event::Command(commands::REPLAY_PLAY_PAUSE));
        }
    }

    /// Picks trade `i` of the run in the list and brings it into view.
    fn pick(chart: &mut Chart, i: usize) {
        let Some(run) = chart.resources_mut().get_mut::<StrategyRun>() else {
            return;
        };
        run.selected = Some(i);
        let trade = run.trades()[i];
        show(chart, trade.entry, trade.exit);
    }

    /// The entries of the trades of the chosen strategy marked as wrong.
    fn marked(&self) -> Vec<Time> {
        let marks = self.marks.iter().filter(|(strategy, _)| *strategy == self.selected);
        marks.map(|&(_, entry)| entry).collect()
    }

    /// Runs the chosen strategy over the bars the chart may show, with the balance and fee of the account.
    fn run(&mut self, chart: &mut Chart, again: Again) {
        let marked = self.marked();
        let (bars, tape) = settled(chart);
        // The test's bars stay out of the run until the test is run.
        let test = self.test_from(bars);
        let (bars, tape) = match test {
            Some(from) if !self.tested => {
                let before = bars.time().partition_point(|&t| t < from);
                (bars.slice(0..before), tape.until(Time(from.0 - 1)))
            }
            _ => (bars, tape),
        };
        let tested = self.tested;
        let Some(strategy) = self.strategies.get_mut(self.selected) else {
            return;
        };
        let account = chart
            .resources()
            .get::<Account>()
            .map_or_else(Account::default, Account::empty_like);
        let mut run = backtest_with_tape(strategy.as_mut(), bars, tape, account);
        run.test = test.map(|from| TestPeriod { from, run: tested });
        run.marked = marked;
        if let Some(old) = chart.resources().get::<StrategyRun>() {
            let (before, now) = (old.trades(), run.trades());
            // The trade picked stays picked while it is still there.
            let selected = old.selected.filter(|&i| {
                let (Some(trade), Some(now)) = (before.get(i), now.get(i)) else {
                    return false;
                };
                trade.entry == now.entry && trade.exit == now.exit
            });
            let changes = match again {
                Again::Fresh => None,
                Again::Bars => old.changes.clone(),
                Again::Changed => Some(TradeChanges::between(before, now)),
            };
            (run.selected, run.changes) = (selected, changes);
        }
        set_strategy_run(chart, Some(run));
        self.shown = Some(Bars::of(chart));
    }

    /// The open of the first bar of the test, of `bars`: the last [`test`](Self::test) percent of them.
    fn test_from(&self, bars: CandleView<'_>) -> Option<Time> {
        let kept = (bars.len() as f64 * self.test / 100.0).round() as usize;
        (self.test > 0.0 && kept > 0 && kept < bars.len()).then(|| bars.time()[bars.len() - kept])
    }

    /// The test kept apart, to run once the settings are chosen; then how the trades did before it and in it.
    fn test_section(&mut self, ui: &mut Ui, chart: &mut Chart) {
        let Some(run) = chart.resources().get::<StrategyRun>() else {
            return;
        };
        let Some(test) = run.test else {
            return;
        };
        let trades = run.trades();
        let step = native_step(chart.seen()).unwrap_or(60_000);
        let from = short_time(test.from, step, crate::panels::clock(chart));
        let (muted, down) = (ui.theme().muted, ui.theme().down);
        ui.section("Test");
        ui.label_colored(&format!("The last {}% of the bars, from {from}", self.test), muted);
        let mut again = None;
        if test.run {
            let (before, after): (Vec<&ClosedTrade>, Vec<&ClosedTrade>) =
                trades.iter().partition(|t| t.entry < test.from);
            period_row(ui, "Before the test", &before);
            period_row(ui, "In the test", &after);
            if ui.button("Keep the test apart") {
                again = Some(false);
            }
            ui.tip("Back to choosing on the bars before it");
        } else {
            ui.label_colored("The strategy has not seen these bars", muted);
            if ui.button("Run the test") {
                again = Some(true);
            }
            ui.tip("On bars it was not fitted to");
        }
        if self.looks > 0 {
            ui.value("Test runs", &self.looks.to_string(), None);
        }
        if self.looks > 1 {
            ui.label_colored("Settings changed after seeing the test are fitted to it", down);
        }
        if let Some(tested) = again {
            self.tested = tested;
            self.looks += u32::from(tested);
            self.run(chart, Again::Fresh);
        }
    }

    /// Starts the chosen strategy trading by itself on the paper account of the live chart, from the bar forming.
    fn start_trading(&mut self, chart: &Chart) {
        let (Some(strategy), Some(account)) = (self.strategies.get(self.selected), chart.resources().get::<Account>())
        else {
            return;
        };
        let seen = chart.seen();
        let last = seen.close().last().copied().unwrap_or(f64::NAN);
        self.trading = LiveRun::new(strategy.as_ref(), seen, chart.seen_trades(), account).map(|run| Trading {
            run,
            equity: account.equity(last),
            fills: account.fills().len(),
            looked: (seen.len(), seen.time().last().copied()),
            backtest: Vec::new(),
        });
        self.trading_ended = match self.trading {
            Some(_) => None,
            None => Some("Not started: this strategy cannot trade by itself".into()),
        };
    }

    /// The strategy trading by itself sees the bars completed since it last looked. It stops when the chart
    /// leaves the live bars, and closes all past the loss allowed.
    fn keep_trading(&mut self, chart: &mut Chart) {
        let Some(trading) = &mut self.trading else {
            return;
        };
        let mut stop = None;
        let seen = chart.seen();
        let now = (seen.len(), seen.time().last().copied());
        if !on_live(chart) {
            stop = Some(("Stopped: the chart left the live bars".to_string(), false));
        } else if now != trading.looked {
            trading.looked = now;
            // The account goes to the strategy while it looks, and back.
            if let Some(mut account) = chart.resources_mut().remove::<Account>() {
                match trading.run.follow(chart.seen(), &mut account) {
                    Ok(0) => {}
                    Ok(_) => {
                        let run = trading.run.backtest(chart.seen(), chart.seen_trades(), &account);
                        trading.backtest = run.account.fills().to_vec();
                    }
                    Err(why) => stop = Some((format!("Stopped: {why}"), false)),
                }
                chart.resources_mut().insert(account);
            }
        }
        if stop.is_none()
            && self.stop_lost > 0.0
            && let (Some(account), Some(&last)) = (chart.resources().get::<Account>(), chart.seen().close().last())
        {
            let lost = (1.0 - account.equity(last) / trading.equity) * 100.0;
            if lost >= self.stop_lost {
                stop = Some((format!("Stopped at {lost:.1}% lost since it started, all closed"), true));
            }
        }
        if let Some((why, close)) = stop {
            self.stop_trading(chart, why, close);
        }
    }

    /// Ends trading by itself, telling `why`; with `close`, the account's orders go and its position is closed.
    fn stop_trading(&mut self, chart: &mut Chart, why: String, close: bool) {
        self.trading = None;
        self.trading_ended = Some(why);
        let base = chart.base();
        if close
            && let (Some(&time), Some(&price)) = (base.time().last(), base.close().last())
            && let Some(account) = chart.resources_mut().get_mut::<Account>()
        {
            account.cancel_all();
            let open = account.position();
            if open != 0.0 {
                account.market(time, price, -open);
            }
        }
    }

    /// Trading by itself on paper: set up and started, then how it goes beside its backtest.
    fn trading_section(&mut self, ui: &mut Ui, chart: &mut Chart) {
        ui.section("Trading by itself");
        let (muted, down) = (ui.theme().muted, ui.theme().down);
        let (mut start, mut stop) = (false, false);
        match &self.trading {
            None => {
                let mut lost = [("Stop past % lost", &mut self.stop_lost)];
                if number_fields(ui, &mut lost, &mut self.lost_fields, &mut self.lost_editing) {
                    self.stop_lost = self.stop_lost.clamp(0.0, 100.0);
                }
                ui.tip("Stops and closes all past this loss; 0 never");
                if on_live(chart) {
                    start = ui.button("Trade it on paper");
                    ui.tip("On this chart's paper account, from now");
                } else {
                    ui.label_colored("On a live chart it trades by itself on paper", muted);
                }
                if let Some(ended) = &self.trading_ended {
                    ui.label_colored(ended, down);
                }
            }
            Some(trading) => {
                let step = native_step(chart.seen()).unwrap_or(60_000);
                let from = short_time(trading.run.from, step, crate::panels::clock(chart));
                ui.label_colored(&format!("{} on paper since {from}", trading.run.name()), muted);
                if self.stop_lost > 0.0 {
                    ui.value("Stops past", &format!("{}% lost", self.stop_lost), None);
                }
                let fills = chart
                    .resources()
                    .get::<Account>()
                    .map_or(&[][..], |a| &a.fills()[trading.fills.min(a.fills().len())..]);
                let (paper, backtest) = (closed_trades(fills), closed_trades(&trading.backtest));
                period_row(ui, "On paper", &paper.iter().collect::<Vec<_>>());
                period_row(ui, "Its backtest", &backtest.iter().collect::<Vec<_>>());
                let compared = FillsCompared::of(fills, &trading.backtest);
                let of = compared.live.max(compared.backtest);
                ui.value("Fills as in the backtest", &format!("{} of {of}", compared.same), None);
                if compared.same > 0 {
                    ui.value("Their prices apart", &format!("{:.3}%", compared.off * 100.0), None);
                }
                stop = ui.button("Stop it and close all");
                ui.tip("Cancels its orders, closes at the last price");
            }
        }
        if start {
            self.start_trading(chart);
        }
        if stop {
            self.stop_trading(chart, "Stopped by hand, all closed".into(), true);
        }
    }

    /// The walk forward: set up, then going on a run a pass, then what each window chose and how its test did.
    fn walk_section(&mut self, ui: &mut Ui, chart: &mut Chart) {
        ui.section("Walk forward");
        let mut setup = [
            ("Test windows", &mut self.windows),
            ("Training per test", &mut self.training),
        ];
        if number_fields(ui, &mut setup, &mut self.walk_fields, &mut self.walk_editing) {
            self.windows = self.windows.round().clamp(1.0, 20.0);
            self.training = self.training.clamp(0.5, 20.0);
            self.walk = None;
        }
        ui.tip("Training is this many test windows long");
        let (bars, _) = settled(chart);
        // It chooses settings, so it stays before the test.
        let bars = match self.test_from(bars) {
            Some(from) => bars.until(Time(from.0 - 1)),
            None => bars,
        };
        if let (Some(walk), Some(strategy)) = (&mut self.walk, self.strategies.get_mut(self.selected))
            && !walk.finished()
        {
            if bars.len() == walk.bars {
                let account = chart
                    .resources()
                    .get::<Account>()
                    .map_or_else(Account::default, Account::empty_like);
                walk.step(strategy.as_mut(), bars, &account);
            } else {
                walk.stopped = Some("The bars changed: walk again");
            }
            if !walk.finished() {
                ui.again();
            }
        }
        // Its windows are bars counted from the first: other bars, another timeframe or a replay's, are not them.
        if self
            .walk
            .as_ref()
            .is_some_and(|walk| walk.finished() && !walk.of(bars.time()))
        {
            self.walk = None;
        }

        let (muted, up, down) = (ui.theme().muted, ui.theme().up, ui.theme().down);
        // What rounds to zero is neither a gain nor a loss.
        let tint = |result: f64| match money(result).as_bytes()[0] {
            b'+' => up,
            b'-' => down,
            _ => muted,
        };
        // A replay's walk sees only what the replay has shown: the bars still to come stay unseen.
        let replaying = chart.resources().get::<Replay>().is_some_and(|r| r.active);
        match &self.walk {
            Some(walk) if !walk.finished() => {
                let per = walk.runs_per_window();
                let (done, runs) = (walk.at.0 * per + walk.at.1, walk.windows.len() * per);
                ui.label_colored(&format!("Walking: run {done} of {runs}"), muted);
                if ui.button("Stop walking") {
                    self.walk = None;
                }
                return;
            }
            _ => {
                if ui.button("Start walking") {
                    let from = self
                        .strategies
                        .get_mut(self.selected)
                        .map_or_else(Vec::new, |strategy| {
                            strategy.settings().iter().map(|s| *s.value).collect()
                        });
                    self.walk = Some(Walk::new(bars.time(), from, self.windows as usize, self.training));
                    ui.again();
                    return;
                }
                ui.tip("Fit on each training window, trade the next");
                if replaying {
                    ui.label_colored(&format!("On the {} bars shown so far", bars.len()), muted);
                }
            }
        }
        let Some(walk) = &self.walk else {
            return;
        };
        if let Some(why) = walk.stopped {
            ui.label_colored(why, down);
            return;
        }
        let step = native_step(chart.seen()).unwrap_or(60_000);
        let clock = crate::panels::clock(chart);
        let times = bars.time();
        let picked = ui.table(&WALK_COLUMNS, walk.windows.len(), None, |i| {
            let window = &walk.windows[i];
            let (_, tested) = window.tested.unwrap_or_default();
            let settings: Vec<String> = window
                .chosen
                .iter()
                .map(|v| ((v * 1e4).round() / 1e4).to_string())
                .collect();
            vec![
                Cell::new(short_time(times[window.test.start], step, clock)),
                Cell::new(settings.join("/")),
                Cell::colored(money(window.fitted), tint(window.fitted)),
                Cell::colored(money(tested), tint(tested)),
            ]
        });
        let (trades, result) = (walk.windows.iter())
            .filter_map(|w| w.tested)
            .fold((0, 0.0), |(n, sum), (k, r)| (n + k, sum + r));
        // Closed trades, as "How it did" counts them, but only those of the test windows.
        let together = format!("{trades} closed, {}", money(result));
        ui.value(IN_THE_TESTS, &together, Some(tint(result)));
        ui.label_colored("A window picked sets its settings", muted);

        if let Some(i) = picked {
            let chosen = walk.windows[i].chosen.clone();
            if let Some(strategy) = self.strategies.get_mut(self.selected) {
                for (setting, value) in strategy.settings().into_iter().zip(chosen) {
                    *setting.value = value;
                }
            }
            self.editing = None;
            self.tested = false;
            if self.shown.is_some() {
                self.run(chart, Again::Changed);
            }
        }
    }
}

/// The walk's row of its test windows together.
const IN_THE_TESTS: &str = "In the tests";

const WALK_COLUMNS: [Column; 4] = [
    Column {
        title: "Test from",
        width: 2.2,
        align: Align::Left,
    },
    Column {
        title: "Settings",
        width: 1.6,
        align: Align::Left,
    },
    Column {
        title: "Fitted",
        width: 1.4,
        align: Align::Right,
    },
    Column {
        title: "Test",
        width: 1.4,
        align: Align::Right,
    },
];

/// A row of how the trades of a period did: how many, the share won and their result.
fn period_row(ui: &mut Ui, name: &str, trades: &[&ClosedTrade]) {
    let result: f64 = trades.iter().map(|t| t.result).sum();
    let wins = trades.iter().filter(|t| t.result > 0.0).count();
    let rate = if trades.is_empty() {
        "-".into()
    } else {
        format!("{:.0}%", wins as f64 * 100.0 / trades.len() as f64)
    };
    let color = if result >= 0.0 { ui.theme().up } else { ui.theme().down };
    let text = format!("{}, {rate}, {}", count_trades(trades.len()), money(result));
    ui.value(name, &text, Some(color));
}

const TRADE_COLUMNS: [Column; 4] = [
    Column {
        title: "In",
        width: 2.2,
        align: Align::Left,
    },
    Column {
        title: "Side",
        width: 0.9,
        align: Align::Left,
    },
    Column {
        title: "Out",
        width: 2.2,
        align: Align::Left,
    },
    Column {
        title: "Result",
        width: 1.5,
        align: Align::Right,
    },
];

/// Scrolls the chart to a trade: its bars in the middle, all of them on screen.
fn show(chart: &mut Chart, from: Time, to: Time) {
    let times = chart.seen().time();
    let bar = |t: Time| times.partition_point(|&x| x <= t).saturating_sub(1) as f64;
    let (a, b) = (bar(from), bar(to));
    let mut scale = *chart.time_scale();
    let span = b - a + 1.0;
    if span * scale.bar_spacing > scale.width * 0.8 {
        scale.bar_spacing = scale.width * 0.8 / span;
    }
    scale.right = (a + b + 1.0) / 2.0 + scale.width / scale.bar_spacing / 2.0;
    chart.set_time_scale(scale);
}

impl<C> Panel<C> for StrategyPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        ui.section("Strategy");
        let (mut changed, mut switched) = (false, false);
        for i in 0..self.strategies.len() {
            if ui.toggle(&self.strategies[i].name(), i == self.selected) && i != self.selected {
                (self.selected, switched) = (i, true);
                self.fields.clear();
                self.editing = None;
            }
        }
        let mut write = false;
        if let Some(files) = &self.files {
            for problem in &self.problems {
                // A parser's message runs over lines (a pointer under the source); its first says what is wrong.
                ui.heading(problem.lines().next().unwrap_or_default());
            }
            let read = ui.button("Read the strategy files again");
            ui.tip(&format!(
                "Strategies built to WebAssembly and models (ONNX files), in {}",
                files.folder
            ));
            if read {
                self.read_files();
                changed = true;
            }
            write = ui.button("Write the bars for training");
            ui.tip("The bars' features as CSV, to train a model");
            let (muted, down) = (ui.theme().muted, ui.theme().down);
            match &self.written {
                Some(Ok(path)) => ui.label_colored(&format!("Written to {path}"), muted),
                Some(Err(why)) => ui.label_colored(&format!("Not written: {why}"), down),
                None => {}
            }
        }
        if write {
            let (bars, _) = settled(chart);
            let bars = match self.test_from(bars) {
                Some(from) => bars.until(Time(from.0 - 1)),
                None => bars,
            };
            let csv = features_csv(bars);
            // The header alone is no data to train on: a file of it would only stand for a written one.
            self.written = if bars.is_empty() {
                Some(Err("no bars on the chart".into()))
            } else if csv.lines().nth(1).is_none() {
                Some(Err("too few bars to know all the numbers of one".into()))
            } else {
                self.files.as_mut().map(|files| (files.features)(&csv))
            };
        }
        let Some(strategy) = self.strategies.get_mut(self.selected) else {
            ui.heading("No strategies");
            return;
        };

        let mut settings = strategy.settings();
        let mut values: Vec<(&str, &mut f64)> = settings.iter_mut().map(|s| (s.name, &mut *s.value)).collect();
        changed |= number_fields(ui, &mut values, &mut self.fields, &mut self.editing);
        drop(values);
        drop(settings);
        let mut share = [("Test on the last %", &mut self.test)];
        let test_changed = number_fields(ui, &mut share, &mut self.test_fields, &mut self.test_editing);
        ui.tip("Last bars kept unseen until the test");
        if test_changed {
            self.test = self.test.clamp(0.0, 90.0);
            (self.tested, self.looks) = (false, 0);
        }
        // Settings chosen while looking at the test would be fitted to it.
        if changed {
            self.tested = false;
        }
        // A walk set out from other settings, or for another strategy, is not this one's.
        if changed || switched {
            self.walk = None;
        }

        if self.shown.is_none() {
            if ui.button("Run on the chart") {
                self.run(chart, Again::Fresh);
            }
            ui.tip("Trades the bars shown, one at a time");
        } else {
            if switched || test_changed {
                self.run(chart, Again::Fresh);
            } else if changed {
                self.run(chart, Again::Changed);
            } else if self.shown != Some(Bars::of(chart)) {
                self.run(chart, Again::Bars);
            }
            if ui.button("Take it off the chart") {
                self.stop_watching(chart);
                set_strategy_run(chart, None);
                self.shown = None;
            }
        }
        self.keep_watching(chart);
        self.test_section(ui, chart);
        self.walk_section(ui, chart);
        self.keep_trading(chart);
        self.trading_section(ui, chart);

        let Some(run) = chart.resources().get::<StrategyRun>() else {
            return;
        };
        let account = &run.account;
        let last = chart.seen().close().last().copied().unwrap_or(0.0);
        let (net, balance, stats) = (account.net(last), account.balance, run.stats());
        let (up, down) = (ui.theme().up, ui.theme().down);
        let tint = |v: f64| Some(if v >= 0.0 { up } else { down });
        ui.section("How it did");
        ui.columns(2, |ui| {
            ui.value("Net", &money(net), tint(net));
            ui.value("Return", &percent_of(net, balance), tint(net));
        });
        ui.columns(2, |ui| {
            ui.value("Closed trades", &stats.closed.to_string(), None);
            let win = stats.win_rate().map_or("-".into(), |w| format!("{:.0}%", w * 100.0));
            ui.value("Wins", &win, None);
        });
        ui.columns(2, |ui| {
            let pf = stats.profit_factor().map_or("-".into(), |p| format!("{p:.2}"));
            ui.value("Profit factor", &pf, None);
            ui.value("Fees", &format!("{:.2}", account.fees()), None);
        });
        let dd = format!("{:.2} ({:.2}%)", stats.max_drawdown, stats.max_drawdown_share * 100.0);
        ui.value("Max drawdown", &dd, None);
        let position = match account.average_price() {
            Some(average) => format!("{:+.4} at {}", account.position(), price(average)),
            None => "Flat".into(),
        };
        ui.value("Position", &position, None);

        let trades = run.trades();
        let chosen = run.selected;
        let open = open_entry(account.fills());
        let mut marked = self.marked();

        if let Some(changes) = &run.changes {
            ui.section("Since the change");
            if changes.is_empty() {
                ui.heading("The same trades");
            }
            ui.columns(2, |ui| {
                ui.value("New", &changes.new.len().to_string(), None);
                ui.value("Gone", &changes.gone.len().to_string(), None);
            });
            ui.value("Changed", &changes.changed.len().to_string(), None);
            if !marked.is_empty() {
                let there =
                    |entry: &Time| trades.iter().any(|t| t.entry == *entry) || open.map(|o| o.0) == Some(*entry);
                let kept = marked.iter().filter(|entry| there(entry)).count();
                ui.value(
                    "Marked wrong, still there",
                    &format!("{kept} of {}", marked.len()),
                    None,
                );
            }
        }

        ui.section("Trades");
        let watching = self.watch.is_some();
        let watch = ui.button(if watching { "Stop watching" } else { "Watch it trade" });
        ui.tip("Plays each trade, skipping the time between");
        let (mut previous, mut next) = (false, false);
        ui.columns(2, |ui| {
            previous = ui.button("Previous trade");
            next = ui.button("Next trade");
        });
        if ui.toggle("Pause at each loss", self.pause.loss) {
            self.pause.loss = !self.pause.loss;
        }
        ui.tip("While watching, after a losing trade");
        let mut pauses = [
            ("Pause past bars held", &mut self.pause.held),
            ("Pause past % lost", &mut self.pause.lost),
        ];
        number_fields(ui, &mut pauses, &mut self.pause_fields, &mut self.pause_editing);
        let step = native_step(chart.seen()).unwrap_or(60_000);
        let zone = crate::panels::clock(chart);
        // Why the trade looked at went in: while watching, the one in the market or else the last that went out
        // (paused at its exit, say); otherwise the one picked.
        let closed = |t: &ClosedTrade| (t.entry, t.long);
        let looked_at = match open {
            _ if !watching => chosen.and_then(|i| trades.get(i)).map(closed),
            Some(open) => Some(open),
            None => trades.last().map(closed),
        };
        let times = settled(chart).0.time();
        let decision = looked_at.and_then(|(entry, long)| run.decision(entry, long, times));
        let decided = decision.map(|d| times[d]);
        if let Some(d) = decision {
            ui.section("Why it went in");
            ui.value("Decided at the close of", &short_time(times[d], step, zone), None);
            for input in &run.inputs {
                let at = |bar: usize| {
                    let values: Vec<String> = input
                        .lines
                        .iter()
                        .map(|line| price(line.get(bar).copied().unwrap_or(f64::NAN)))
                        .collect();
                    values.join(" / ")
                };
                let was = d.checked_sub(1).map_or("-".into(), at);
                ui.value(&input.study.label(), &format!("{} (was {was})", at(d)), None);
            }
            for (_, note) in run.notes.iter().filter(|(t, _)| *t == times[d]) {
                ui.heading(note);
            }
        }
        if let Some((entry, _)) = looked_at {
            let wrong = marked.contains(&entry);
            if ui.button(if wrong { "Not wrong" } else { "Mark as wrong" }) {
                if wrong {
                    self.marks.retain(|&mark| mark != (self.selected, entry));
                } else {
                    self.marks.push((self.selected, entry));
                }
                marked = self.marked();
            }
            ui.tip("Kept by entry across setting changes");
        }
        let n = trades.len();
        let clicked = if trades.is_empty() {
            ui.heading("None yet");
            None
        } else {
            let (new, changed) = run
                .changes
                .as_ref()
                .map_or((&[][..], &[][..]), |c| (&c.new[..], &c.changed[..]));
            if !(new.is_empty() && changed.is_empty() && marked.is_empty()) {
                ui.heading("+ new, ~ changed, ! marked wrong");
            }
            // The newest first.
            ui.table(&TRADE_COLUMNS, n, chosen.map(|i| n - 1 - i), |row| {
                let i = n - 1 - row;
                let trade = trades[i];
                let (side, color) = if trade.long { ("Long", up) } else { ("Short", down) };
                let tint = if trade.result >= 0.0 { up } else { down };
                let mark = if marked.contains(&trade.entry) {
                    "! "
                } else if changed.binary_search(&i).is_ok() {
                    "~ "
                } else if new.binary_search(&i).is_ok() {
                    "+ "
                } else {
                    ""
                };
                vec![
                    Cell::new(format!("{mark}{}", short_time(trade.entry, step, zone))),
                    Cell::colored(side, color),
                    Cell::new(short_time(trade.exit, step, zone)),
                    Cell::colored(money(trade.result), tint),
                ]
            })
            .map(|row| n - 1 - row)
        };
        if (run.decided != decided || run.marked != marked)
            && let Some(run) = chart.resources_mut().get_mut::<StrategyRun>()
        {
            run.decided = decided;
            run.marked = marked;
        }
        if watch {
            if watching {
                self.stop_watching(chart);
            } else {
                self.start_watching(chart, chosen.unwrap_or(0));
            }
        } else if previous || next {
            if let Some(watch) = &self.watch {
                // From the trade being played to the one before or after it.
                let now = chart.cursor().map_or(0, |cursor| watch.playing(cursor));
                let i = if next { now + 1 } else { now.saturating_sub(1) };
                if i < watch.trades.len() {
                    self.jump(chart, i);
                }
            } else if n > 0 {
                let i = match (chosen, next) {
                    (Some(i), true) => (i + 1).min(n - 1),
                    (Some(i), false) => i.saturating_sub(1),
                    (None, true) => 0,
                    (None, false) => n - 1,
                };
                Self::pick(chart, i);
            }
        } else if let Some(i) = clicked {
            if self.watch.is_some() {
                self.jump(chart, i);
            } else if chosen == Some(i) {
                if let Some(run) = chart.resources_mut().get_mut::<StrategyRun>() {
                    run.selected = None;
                }
            } else {
                Self::pick(chart, i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dock, Workspace};
    use tickerlight_core::Candles;
    use tickerlight_core::builtin::{MovingAverageCross, RsiReversal, backtest_from};
    use tickerlight_core::plugin::commands;
    use tickerlight_core::sample::random_walk;
    use tickerlight_core::{Event, Key, Scene, Surface, TimeScale};

    fn window() -> Workspace<()> {
        let strategies: Vec<Box<dyn Strategy>> = vec![
            Box::new(MovingAverageCross::default()),
            Box::new(RsiReversal::default()),
        ];
        window_with(StrategyPanel::new(strategies))
    }

    fn window_with(panel: StrategyPanel) -> Workspace<()> {
        let chart = Chart::new(random_walk(2000));
        let mut w = Workspace::new(chart, ()).with(Dock::Right, panel);
        w.resize(1400.0, 1000.0);
        w.build_scene(&mut Scene::new());
        w
    }

    #[test]
    fn on_a_live_chart_it_trades_by_itself_as_its_backtest_does_and_stops_with_all_closed() {
        let data = random_walk(2700);
        let mut first = Candles::new();
        for i in 0..2000 {
            first.push(data.view().get(i).unwrap()).unwrap();
        }
        let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(MovingAverageCross::default())];
        let mut w = Workspace::new(Chart::new(first), ()).with(Dock::Right, StrategyPanel::new(strategies));
        w.resize(1400.0, 1600.0);
        w.build_scene(&mut Scene::new());
        assert!(has(&w, "On a live chart it trades by itself on paper") && !has(&w, "Trade it on paper"));
        w.chart_mut().resources_mut().insert(Live(true));
        w.build_scene(&mut Scene::new());
        let type_into = |w: &mut Workspace<()>, field: &str, text: &str| {
            click(w, field);
            w.handle(Event::Key(Key::SelectAll));
            for c in text.chars() {
                w.handle(Event::Key(Key::Char(c)));
            }
            w.handle(Event::Key(Key::Enter));
            w.build_scene(&mut Scene::new());
        };
        let live_bars = |w: &mut Workspace<()>, bars: Range<usize>| {
            for i in bars {
                w.chart_mut().push(data.view().get(i).unwrap()).unwrap();
                w.build_scene(&mut Scene::new());
            }
        };
        let account = |w: &Workspace<()>| w.chart().resources().get::<Account>().unwrap().clone();
        // No stop for a loss at first.
        type_into(&mut w, "Stop past % lost", "0");
        click(&mut w, "Trade it on paper");
        let since = |w: &Workspace<()>| {
            let rects = w.ui_state().rects();
            rects
                .iter()
                .any(|(t, _)| t.starts_with("Moving average cross 10/30 on paper since"))
        };
        assert!(since(&w));
        live_bars(&mut w, 2000..2400);
        let fills = account(&w).fills().to_vec();
        assert!(closed_trades(&fills).len() >= 2);
        // Bar 1999 was forming when it started: the backtest from there over the bars complete is the same.
        let mut strategy = MovingAverageCross::default();
        let bars = data.view().slice(0..2399);
        let backtest = backtest_from(
            &mut strategy,
            bars,
            TradeView::default(),
            account(&w).empty_like(),
            1999,
        );
        assert_eq!(fills, backtest.account.fills());
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        let same = format!("{} of {}", fills.len(), fills.len());
        assert!(scene.texts.iter().any(|t| t.text == same), "{same}");

        click(&mut w, "Stop it and close all");
        let stopped = account(&w);
        assert!(stopped.position() == 0.0 && stopped.orders().is_empty() && !since(&w));
        assert!(has(&w, "Stopped by hand, all closed"));
        // Bars come and it trades no more.
        live_bars(&mut w, 2400..2450);
        assert_eq!(account(&w).fills().len(), stopped.fills().len());

        // Past the loss allowed it stops by itself with all closed.
        type_into(&mut w, "Stop past % lost", "0.01");
        click(&mut w, "Trade it on paper");
        let mut i = 2450;
        while since(&w) && i < 2600 {
            live_bars(&mut w, i..i + 1);
            i += 1;
        }
        let rects = w.ui_state().rects();
        assert!(
            rects
                .iter()
                .any(|(t, _)| t.starts_with("Stopped at ") && t.ends_with("% lost since it started, all closed"))
        );
        assert_eq!(account(&w).position(), 0.0);

        // A replay is not the live bars.
        click(&mut w, "Trade it on paper");
        assert!(since(&w));
        command(&mut w, commands::REPLAY_TOGGLE);
        w.build_scene(&mut Scene::new());
        assert!(has(&w, "Stopped: the chart left the live bars") && !since(&w));
    }

    #[test]
    fn strategies_from_files_come_after_the_programs_own_and_are_read_again_keeping_the_choice() {
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = reads.clone();
        let written = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let kept = written.clone();
        let files = StrategyFiles {
            folder: "/data/strategies".into(),
            read: Box::new(move || {
                counted.set(counted.get() + 1);
                let strategy: Box<dyn Strategy> = Box::new(MovingAverageCross { fast: 3.0, slow: 7.0 });
                (
                    vec![strategy],
                    vec!["broken.wasm: expected `(`\n --> <anon>:1:1".into()],
                )
            }),
            features: Box::new(move |csv| {
                kept.replace(csv.to_string());
                Ok("/data/strategies/features.csv".into())
            }),
        };
        let own: Vec<Box<dyn Strategy>> = vec![Box::new(RsiReversal::default())];
        let mut w = window_with(StrategyPanel::new(own).with_files(files));
        assert!(has(&w, "broken.wasm: expected `(`"));
        click(&mut w, "Moving average cross 3/7");
        click(&mut w, "Run on the chart");
        assert_eq!(run(&w).unwrap().name, "Moving average cross 3/7");
        click(&mut w, "Read the strategy files again");
        assert_eq!(reads.get(), 2);
        // Still chosen and on the chart, run again from the file read anew.
        assert_eq!(run(&w).unwrap().name, "Moving average cross 3/7");
        assert!(has(&w, "Take it off the chart"));

        // The bars written for training: all of them, then only those before a test kept apart.
        click(&mut w, "Write the bars for training");
        assert!(has(&w, "Written to /data/strategies/features.csv"));
        assert_eq!(*written.borrow(), features_csv(w.chart().seen()));
        click(&mut w, "Test on the last %");
        w.handle(Event::Key(Key::SelectAll));
        for c in "25".chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Enter));
        click(&mut w, "Write the bars for training");
        let seen = w.chart().seen();
        let before = seen.slice(0..1500);
        assert_eq!(*written.borrow(), features_csv(before));

        // Too few bars for a row: no file of the header alone, and the page says why.
        let wrote = std::rc::Rc::new(std::cell::Cell::new(false));
        let set = wrote.clone();
        let files = StrategyFiles {
            folder: "/data/strategies".into(),
            read: Box::new(|| (Vec::new(), Vec::new())),
            features: Box::new(move |_| {
                set.set(true);
                Ok("/data/strategies/features.csv".into())
            }),
        };
        let mut w = Workspace::new(Chart::new(random_walk(5)), ())
            .with(Dock::Right, StrategyPanel::new(Vec::new()).with_files(files));
        w.resize(1400.0, 1000.0);
        w.build_scene(&mut Scene::new());
        click(&mut w, "Write the bars for training");
        assert!(!wrote.get());
        assert!(has(&w, "Not written: too few bars to know all the numbers of one"));
    }

    fn click(w: &mut Workspace<()>, text: &str) {
        let rects = w.ui_state().rects();
        // The last with this text: a field comes after its heading.
        let (_, area) = rects
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("no {text}: {rects:?}"));
        let (x, y) = area.center();
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
    }

    fn has(w: &Workspace<()>, text: &str) -> bool {
        w.ui_state().rects().iter().any(|(t, _)| t == text)
    }

    fn run(w: &Workspace<()>) -> Option<&StrategyRun> {
        w.chart().resources().get::<StrategyRun>()
    }

    fn command(w: &mut Workspace<()>, name: &'static str) {
        w.chart_mut().handle(Event::Command(name));
        w.build_scene(&mut Scene::new());
    }

    #[test]
    fn in_a_replay_its_trades_come_out_as_the_bars_do() {
        let mut w = window();
        click(&mut w, "Run on the chart");
        let all = closed_trades(run(&w).unwrap().account.fills()).len();
        assert!(all > 4, "{all}");

        command(&mut w, commands::REPLAY_TOGGLE);
        let cursor = w.chart().cursor().unwrap();
        let mut seen = closed_trades(run(&w).unwrap().account.fills()).len();
        assert!(seen < all, "{seen} of {all}");
        assert!(w.chart().pane_names().contains(&"strategy-equity"));
        for _ in 0..300 {
            command(&mut w, commands::REPLAY_STEP);
            let trades = closed_trades(run(&w).unwrap().account.fills());
            let last = *w.chart().seen().time().last().unwrap();
            assert!(trades.iter().all(|t| t.exit <= last), "nothing from bars still to come");
            assert!(trades.len() >= seen);
            seen = trades.len();
        }
        assert!(w.chart().cursor().unwrap() > cursor);
        assert!(seen > 0 && has(&w, "Net"));

        command(&mut w, commands::REPLAY_TOGGLE);
        assert_eq!(
            closed_trades(run(&w).unwrap().account.fills()).len(),
            all,
            "all again when it ends"
        );
    }

    #[test]
    fn the_page_is_kept_as_text_and_runs_again_from_it() {
        let strategies: Vec<Box<dyn Strategy>> = vec![
            Box::new(MovingAverageCross { fast: 5.0, slow: 50.0 }),
            Box::new(RsiReversal::default()),
        ];
        let mut panel = StrategyPanel::new(strategies);
        let text = panel.save();
        assert_eq!(
            text,
            "selected 0\nshown 0\n0\tFast=5\tSlow=50\n1\tRSI length=14\tLevel=30\tRisk %=1\n"
        );
        panel.load("selected 1\nshown 1\n0\tFast=7\tSlow=nope\tOther=3\n9\tFast=1\n");
        assert_eq!(
            panel.save(),
            "selected 1\nshown 1\n0\tFast=7\tSlow=50\n1\tRSI length=14\tLevel=30\tRisk %=1\n"
        );
        // On a chart, what was on is run.
        let mut chart = Chart::new(random_walk(500));
        panel.follow(&mut chart);
        let run = chart.resources().get::<StrategyRun>().expect("run from what was kept");
        assert_eq!(run.name, "RSI 14 reversal from 30");
    }

    /// A walk forward of the moving average cross from its first settings over `data`, to the end.
    fn walk_over(data: &Candles) -> Walk {
        let mut strategy = MovingAverageCross::default();
        let from = strategy.settings().iter().map(|s| *s.value).collect();
        let mut walk = Walk::new(data.view().time(), from, 4, 3.0);
        while !walk.finished() {
            walk.step(&mut strategy, data.view(), &Account::default());
        }
        walk
    }

    #[test]
    fn a_walk_forward_chooses_each_windows_settings_without_the_bars_after_it() {
        let data = random_walk(2000);
        let one = walk_over(&data);
        // 285-bar test windows, each after 855 training bars, one after another.
        assert_eq!(one.windows.len(), 4);
        assert_eq!(
            (one.windows[0].train.clone(), one.windows[0].test.clone()),
            (0..855, 855..1140)
        );
        assert_eq!(one.windows[3].test, 1710..1995);
        for window in &one.windows {
            // Never worse over its training bars than the settings it set out from.
            let bars = data.view().slice(window.train.clone());
            let start = backtest(&mut MovingAverageCross::default(), bars, Account::default());
            assert!(window.fitted >= start.account.net(bars.close()[bars.len() - 1]));
            assert!(window.tested.is_some());
        }
        assert!(
            one.windows.iter().any(|w| w.chosen != [10.0, 30.0]),
            "a search that chose something"
        );

        // Other bars after the first test window: that window chose and did the same, the last one did not.
        let mut other = Candles::new();
        for i in 0..data.len() {
            let mut candle = data.view().get(i).unwrap();
            if i >= 1140 {
                let k = 1.0 + (i - 1140) as f64 * 0.002;
                (candle.open, candle.high, candle.low, candle.close) =
                    (candle.open * k, candle.high * k, candle.low * k, candle.close * k);
            }
            other.push(candle).unwrap();
        }
        let two = walk_over(&other);
        let row = |w: &WalkWindow| (w.chosen.clone(), w.fitted, w.tested);
        assert_eq!(row(&one.windows[0]), row(&two.windows[0]));
        assert_ne!(row(&one.windows[3]), row(&two.windows[3]));

        let times: Vec<Time> = (0..100).map(|i| Time(i * 60_000)).collect();
        let few = Walk::new(&times, vec![10.0], 4, 3.0);
        assert!(few.finished() && few.stopped.is_some() && few.windows.is_empty());
    }

    #[test]
    fn a_walk_forward_goes_on_a_run_a_frame_and_a_window_picked_puts_its_settings_on() {
        let mut w = window();
        click(&mut w, "Start walking");
        let mut frames = 0;
        while has(&w, "Stop walking") {
            assert!(
                w.needs_tick() && w.handle(Event::Tick { dt: 0.016 }),
                "frames while it walks"
            );
            w.build_scene(&mut Scene::new());
            frames += 1;
            assert!(frames < 500);
        }
        assert!(frames > 4, "spread over frames, not one: {frames}");
        // Done, it asks for no more frames (once the button under the pointer has lit up).
        w.handle(Event::Tick { dt: 1.0 });
        w.build_scene(&mut Scene::new());
        assert!(!w.needs_tick() && has(&w, IN_THE_TESTS));
        assert!(has(&w, "Moving average cross 10/30"));

        let walk = walk_over(&random_walk(2000));
        let window = &walk.windows[2];
        let start = w.chart().seen().time()[window.test.start];
        click(
            &mut w,
            &short_time(start, 60_000, tickerlight_core::calendar::Zone::Utc),
        );
        let name = format!("Moving average cross {}/{}", window.chosen[0], window.chosen[1]);
        assert!(has(&w, &name), "{name}");
        // The walk stays to pick another window from, until a setting is typed.
        assert!(has(&w, IN_THE_TESTS));
        // Its windows are bars of this timeframe: on another one they would be other bars.
        w.chart_mut().set_timeframe(Some(300_000));
        w.build_scene(&mut Scene::new());
        assert!(has(&w, "Start walking") && !has(&w, IN_THE_TESTS));
        w.chart_mut().set_timeframe(None);
        click(&mut w, "Start walking");
        while has(&w, "Stop walking") {
            w.handle(Event::Tick { dt: 0.016 });
            w.build_scene(&mut Scene::new());
        }
        assert!(has(&w, IN_THE_TESTS));
        click(&mut w, "Fast");
        w.handle(Event::Key(Key::SelectAll));
        w.handle(Event::Key(Key::Char('7')));
        w.handle(Event::Key(Key::Enter));
        w.build_scene(&mut Scene::new());
        assert!(has(&w, "Start walking") && !has(&w, IN_THE_TESTS));
    }

    #[test]
    fn the_test_bars_stay_out_of_the_run_until_the_test_is_run_and_each_run_counts() {
        let mut w = window();
        click(&mut w, "Run on the chart");
        let all = run(&w).unwrap().clone();
        let type_into = |w: &mut Workspace<()>, field: &str, text: &str| {
            click(w, field);
            w.handle(Event::Key(Key::SelectAll));
            for c in text.chars() {
                w.handle(Event::Key(Key::Char(c)));
            }
            w.handle(Event::Key(Key::Enter));
            w.build_scene(&mut Scene::new());
        };
        let on_chart = |w: &mut Workspace<()>, text: &str| {
            let mut scene = Scene::new();
            w.build_scene(&mut scene);
            scene.texts.iter().any(|t| t.text == text)
        };
        type_into(&mut w, "Test on the last %", "25");
        // A quarter of the 2000 bars.
        let from = w.chart().seen().time()[1500];
        let kept = run(&w).unwrap().clone();
        assert_eq!(kept.test, Some(TestPeriod { from, run: false }));
        let before: Vec<_> = all.account.fills().iter().filter(|f| f.time < from).copied().collect();
        assert_eq!(
            kept.account.fills(),
            &before[..],
            "the same up to the test, nothing in it"
        );
        assert!(on_chart(&mut w, "Test, kept apart"));
        assert!(!has(&w, "In the test"));

        click(&mut w, "Run the test");
        let tested = run(&w).unwrap().clone();
        assert_eq!(tested.account.fills(), all.account.fills());
        assert_eq!(tested.test, Some(TestPeriod { from, run: true }));
        assert!(has(&w, "Before the test") && has(&w, "In the test") && on_chart(&mut w, "Test"));
        let warning = "Settings changed after seeing the test are fitted to it";
        assert!(!has(&w, warning));

        // Settings changed after it keep the test apart again; running it once more is a second look.
        type_into(&mut w, "Slow", "80");
        let changed = run(&w).unwrap();
        assert!(!changed.test.unwrap().run && changed.account.fills().iter().all(|f| f.time < from));
        click(&mut w, "Run the test");
        assert!(has(&w, warning));

        let mut panel = StrategyPanel::new(vec![Box::new(MovingAverageCross::default())]);
        panel.load("test 25 2\n");
        assert!(panel.save().contains("test 25 2\n"));
    }

    #[test]
    fn a_strategy_runs_on_the_chart_and_again_when_set_otherwise() {
        let mut w = window();
        assert!(run(&w).is_none() && !has(&w, "Net"));
        click(&mut w, "Run on the chart");
        let first = run(&w).expect("a run").clone();
        assert_eq!(first.name, "Moving average cross 10/30");
        assert!(!first.account.fills().is_empty());
        assert!(has(&w, "Net") && has(&w, "Max drawdown"));
        assert!(w.chart().pane_names().contains(&"strategy-equity"));

        // A slower average trades less, at once.
        click(&mut w, "Slow");
        w.handle(Event::Key(Key::SelectAll));
        for c in "80".chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Enter));
        w.build_scene(&mut Scene::new());
        let slower = run(&w).unwrap();
        assert_eq!(slower.name, "Moving average cross 10/80");
        assert!(slower.account.fills().len() < first.account.fills().len());

        // A trade picked in the list is brought into view.
        let trades = closed_trades(run(&w).unwrap().account.fills());
        let (n, newest) = (trades.len(), trades[trades.len() - 1]);
        // The newest row is the first, named by when it went in (after what the change made of it).
        let step = native_step(w.chart().seen()).unwrap();
        let rects = w.ui_state().rects();
        let (_, row) = rects
            .iter()
            .find(|(t, _)| t.ends_with(&short_time(newest.entry, step, tickerlight_core::calendar::Zone::Utc)))
            .unwrap();
        let (x, y) = row.center();
        w.chart_mut().set_time_scale(TimeScale::new(0.0, 6.0, 300.0));
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        assert_eq!(run(&w).unwrap().selected, Some(n - 1));
        let times = w.chart().seen().time();
        let visible = w.chart().time_scale().visible(times.len());
        assert!(times[visible.start] <= newest.entry && newest.exit <= times[visible.end - 1]);

        click(&mut w, "RSI 14 reversal from 30");
        assert_eq!(run(&w).unwrap().name, "RSI 14 reversal from 30");

        click(&mut w, "Take it off the chart");
        assert!(run(&w).is_none());
        assert!(!w.chart().pane_names().contains(&"strategy-equity"));
    }

    #[test]
    fn a_trade_marked_wrong_is_followed_through_a_change_of_settings() {
        let mut w = few_trades();
        click(&mut w, "Next trade");
        click(&mut w, "Mark as wrong");
        let before = closed_trades(run(&w).unwrap().account.fills());
        let entry = before[0].entry;
        assert_eq!(run(&w).unwrap().marked, [entry]);
        assert!(has(&w, "Not wrong") && has(&w, "+ new, ~ changed, ! marked wrong"));
        assert!(!has(&w, "Since the change"), "nothing changed yet");

        // Another level: the trades are compared with those before.
        click(&mut w, "Level");
        w.handle(Event::Key(Key::SelectAll));
        for c in "27".chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Enter));
        w.build_scene(&mut Scene::new());
        let now = run(&w).unwrap();
        let changes = now.changes.as_ref().expect("compared");
        assert!(!changes.new.is_empty() && !changes.gone.is_empty(), "{changes:?}");
        assert!(
            changes
                .gone
                .iter()
                .all(|g| !closed_trades(now.account.fills()).contains(g))
        );
        assert_eq!(now.marked, [entry]);
        assert!(has(&w, "Since the change") && has(&w, "Marked wrong, still there"));
        // On the chart, over the marked trade: whether the change took it away.
        let label = if changes.gone.iter().any(|g| g.entry == entry) {
            "Wrong, gone"
        } else {
            "Wrong"
        };
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        assert!(scene.texts.iter().any(|t| t.text == label), "{label}");

        // Another strategy: nothing to compare, nothing marked; back, the mark is there again.
        click(&mut w, "Moving average cross 10/30");
        assert!(run(&w).unwrap().changes.is_none() && run(&w).unwrap().marked.is_empty());
        click(&mut w, "RSI 14 reversal from 27");
        assert_eq!(run(&w).unwrap().marked, [entry]);

        // Kept with the page.
        let mut panel = StrategyPanel::new(vec![Box::new(RsiReversal::default())]);
        panel.load(&format!("mark 0 {}\nmark 5 1\n", entry.0));
        assert_eq!(panel.marks, [(0, entry)], "a mark of a strategy not there is left out");
        assert!(panel.save().contains(&format!("\nmark 0 {}\n", entry.0)));
    }

    fn replay(w: &crate::Workspace<()>) -> &Replay {
        w.chart().resources().get::<Replay>().unwrap()
    }

    /// A strategy out of the market between its trades.
    fn few_trades() -> crate::Workspace<()> {
        let mut w = window();
        click(&mut w, "RSI 14 reversal from 30");
        click(&mut w, "Run on the chart");
        w
    }

    #[test]
    fn watching_it_trade_plays_each_trade_and_skips_the_time_between() {
        let mut w = few_trades();
        let times = w.chart().seen().time().to_vec();
        let bar = |t: Time| times.partition_point(|&x| x <= t) - 1;
        let trades = closed_trades(run(&w).unwrap().account.fills());
        // The bars of each trade's part: from the lead before its entry to the bar after its exit.
        let parts: Vec<(usize, usize)> = trades
            .iter()
            .map(|t| (bar(t.entry).saturating_sub(LEAD), bar(t.exit) + 1))
            .collect();
        let first = parts[0].0;
        let last = parts[parts.len() - 1].1;
        let between = (first..last)
            .filter(|&b| !parts.iter().any(|&(a, z)| a <= b && b <= z))
            .count();
        assert!(
            trades.len() > 3 && between > 50,
            "{} trades, {between} bars between",
            trades.len()
        );

        click(&mut w, "Watch it trade");
        assert_eq!(
            w.chart().cursor(),
            Some(times[first]),
            "it starts {LEAD} bars before the first entry"
        );
        assert!(replay(&w).playing && has(&w, "Stop watching"));
        let mut stood = Vec::new();
        for _ in 0..20_000 {
            w.handle(Event::Tick { dt: 0.05 });
            w.build_scene(&mut Scene::new());
            if !replay(&w).playing {
                break;
            }
            stood.push(bar(w.chart().cursor().unwrap()));
        }
        assert!(!replay(&w).playing && replay(&w).active, "paused after the last trade");
        assert!(
            bar(w.chart().cursor().unwrap()) >= last - 1,
            "the last trade played out"
        );
        assert!(has(&w, "Watch it trade"));
        for &b in &stood {
            assert!(
                parts.iter().any(|&(a, z)| a <= b && b <= z),
                "bar {b} is out of every trade"
            );
        }
        // Each trade was played.
        for &(a, z) in &parts {
            assert!(stood.iter().any(|&b| a <= b && b <= z), "trade at {a}..{z} not played");
        }
        // The strategy on the chart is the one cut at the cursor: its trades so far.
        assert_eq!(closed_trades(run(&w).unwrap().account.fills()).len(), trades.len());
    }

    #[test]
    fn previous_and_next_go_through_the_trades_and_jump_the_watch() {
        let mut w = few_trades();
        let times = w.chart().seen().time().to_vec();
        let bar = |t: Time| times.partition_point(|&x| x <= t) - 1;
        let trades = closed_trades(run(&w).unwrap().account.fills());
        let n = trades.len();

        click(&mut w, "Next trade");
        assert_eq!(run(&w).unwrap().selected, Some(0), "from none, the first");
        click(&mut w, "Next trade");
        assert_eq!(run(&w).unwrap().selected, Some(1));
        let visible = w.chart().time_scale().visible(times.len());
        assert!(visible.start <= bar(trades[1].entry) && bar(trades[1].exit) < visible.end);
        click(&mut w, "Previous trade");
        click(&mut w, "Previous trade");
        assert_eq!(run(&w).unwrap().selected, Some(0), "not before the first");

        // The watch starts at the trade picked; next and previous move it a trade at a time.
        click(&mut w, "Next trade");
        click(&mut w, "Watch it trade");
        let start = |i: usize| Some(times[bar(trades[i].entry).saturating_sub(LEAD)]);
        assert_eq!(w.chart().cursor(), start(1));
        click(&mut w, "Next trade");
        assert_eq!(w.chart().cursor(), start(2));
        click(&mut w, "Previous trade");
        click(&mut w, "Previous trade");
        assert_eq!(w.chart().cursor(), start(0));
        assert!(replay(&w).playing);

        click(&mut w, "Stop watching");
        assert!(!replay(&w).playing && replay(&w).active, "paused where it was");
        assert!(n > 3 && has(&w, "Watch it trade"));
    }

    #[test]
    fn the_trade_looked_at_says_why_it_went_in_and_its_bar_is_marked() {
        let mut w = few_trades();
        assert!(!has(&w, "Why it went in"));
        click(&mut w, "Next trade");
        let times = w.chart().seen().time().to_vec();
        let decided = |w: &Workspace<()>| {
            let run = run(w).unwrap();
            let entry = closed_trades(run.account.fills())[run.selected.unwrap()].entry;
            (run.decided, run.decision(entry, true, &times).map(|d| times[d]))
        };
        let (marked, wanted) = decided(&w);
        assert!(wanted.is_some() && marked == wanted);
        assert!(has(&w, "Why it went in") && has(&w, "Decided at the close of") && has(&w, "RSI 14"));

        // While watching: the trade being played, once it is in.
        click(&mut w, "Watch it trade");
        let mut seen = Vec::new();
        for _ in 0..2_000 {
            w.handle(Event::Tick { dt: 0.05 });
            w.build_scene(&mut Scene::new());
            let run = run(&w).unwrap();
            if let Some((entry, long)) = open_entry(run.account.fills()) {
                let d = run.decision(entry, long, w.chart().seen().time()).unwrap();
                assert_eq!(run.decided, Some(w.chart().seen().time()[d]));
                seen.push(d);
            }
        }
        seen.dedup();
        assert!(seen.len() > 1, "more than one trade explained: {seen:?}");
    }

    #[test]
    fn the_watch_pauses_at_the_exit_of_a_trade_worth_a_look_and_goes_on_from_there() {
        let mut w = few_trades();
        let times = w.chart().seen().time().to_vec();
        let bar = |t: Time| times.partition_point(|&x| x <= t) - 1;
        let trades = closed_trades(run(&w).unwrap().account.fills());
        let losses: Vec<usize> = trades.iter().filter(|t| t.result < 0.0).map(|t| bar(t.exit)).collect();
        assert!(losses.len() > 2, "{} losses", losses.len());
        let play_to_a_pause = |w: &mut Workspace<()>| {
            for _ in 0..20_000 {
                w.handle(Event::Tick { dt: 0.05 });
                w.build_scene(&mut Scene::new());
                if !replay(w).playing {
                    break;
                }
            }
            bar(w.chart().cursor().unwrap())
        };

        click(&mut w, "Pause at each loss");
        click(&mut w, "Watch it trade");
        for &exit in &losses[..3] {
            assert_eq!(play_to_a_pause(&mut w), exit, "paused at the exit of a loss");
            assert!(has(&w, "Stop watching"), "still watching");
            assert!(has(&w, "Why it went in"), "why the loss went in");
            command(&mut w, commands::REPLAY_PLAY_PAUSE);
        }
        click(&mut w, "Stop watching");
        click(&mut w, "Pause at each loss");

        // Held longer than all but the longest: paused at the first of those.
        let held: Vec<usize> = trades.iter().map(|t| bar(t.exit) - bar(t.entry)).collect();
        let longest = *held.iter().max().unwrap();
        let first_longest = trades[held.iter().position(|&h| h == longest).unwrap()];
        click(&mut w, "Pause past bars held");
        w.handle(Event::Key(Key::SelectAll));
        for c in (longest - 1).to_string().chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Enter));
        click(&mut w, "Watch it trade");
        assert_eq!(play_to_a_pause(&mut w), bar(first_longest.exit));
        assert!(has(&w, "Stop watching"));
        click(&mut w, "Stop watching");

        // A loss of more than a share of the balance: just under the biggest one, that one.
        let type_into = |w: &mut Workspace<()>, field: &str, text: &str| {
            click(w, field);
            w.handle(Event::Key(Key::SelectAll));
            for c in text.chars() {
                w.handle(Event::Key(Key::Char(c)));
            }
            w.handle(Event::Key(Key::Enter));
        };
        type_into(&mut w, "Pause past bars held", "0");
        let balance = run(&w).unwrap().account.balance;
        let worst = *trades.iter().min_by(|a, b| a.result.total_cmp(&b.result)).unwrap();
        type_into(
            &mut w,
            "Pause past % lost",
            &format!("{}", -worst.result / balance * 100.0 * 0.99),
        );
        click(&mut w, "Watch it trade");
        assert_eq!(play_to_a_pause(&mut w), bar(worst.exit));
    }
}
