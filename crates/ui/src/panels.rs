//! The panels of a backtesting window. Each one is a control for what the chart does by keys: the replay, the
//! timeframe, the drawing tools, orders. They work on any chart with the default plugins.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};

use tickerlight_core::builtin::{
    Account, Anchor, Dash, Drawing, DrawingStyle, Drawings, Exits, ExitsTried, Fill, Order, OrderKind, Replay,
    SeriesKind, Stats, TEXT_SIZES, Tool, add_position, can_trade, exits_tried, set_studies, size_down, size_up,
    studies,
};
use tickerlight_core::calendar::{Clock, DateTime, MS_PER_DAY, Zone, full_label, parse_time};
use tickerlight_core::plugin::commands;
use tickerlight_core::{Align, Chart, Color, Event, ScaleSetting, Time, TimeScale, native_step, palette, span_label};

use crate::{Cell, Column, Icon, Panel, Ui};

/// Periods offered besides the data's own, when they are whole multiples of it.
const TIMEFRAMES: [i64; 6] = [60_000, 300_000, 900_000, 3_600_000, 14_400_000, 86_400_000];
/// Periods of seconds offered, shorter than the data's own, when the chart's source has trades to make them of.
const SECONDS: [i64; 4] = [1000, 5000, 15_000, 30_000];

/// A chart resource: whether the chart's source has trades, so that bars of seconds can be made of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tape(pub bool);

fn command(chart: &mut Chart, name: &'static str) {
    chart.handle(Event::Command(name));
}

/// A price with as many decimals as its size needs: `82245.93`, `2.5012`, `0.00001234`.
pub fn price(value: f64) -> String {
    if !value.is_finite() || value == 0.0 {
        return format!("{value:.2}");
    }
    let decimals = (4 - value.abs().log10().floor() as i32).clamp(2, 8) as usize;
    format!("{value:.decimals$}")
}

/// Profit or loss in the quote currency, signed.
pub fn money(value: f64) -> String {
    format!("{value:+.2}")
}

fn parse_price(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|p| p.is_finite() && *p > 0.0)
}

/// What the menu of the bars of seconds is found by while none of them is shown.
pub const SECONDS_MENU: &str = "sec";

/// The timeframes of the chart: the bars of seconds in a menu when the source has trades, then a row of the
/// others.
#[derive(Default)]
pub struct TimeframePanel {
    /// The data's own period and whether it has trades: what the choices below were made for.
    made_for: Option<(i64, bool)>,
    seconds: Vec<i64>,
    seconds_labels: Vec<String>,
    /// `None` is the data's own period.
    spans: Vec<Option<i64>>,
    labels: Vec<String>,
}

impl TimeframePanel {
    fn make(&mut self, native: i64, tape: bool) {
        if self.made_for == Some((native, tape)) {
            return;
        }
        self.made_for = Some((native, tape));
        self.seconds = SECONDS.into_iter().filter(|&s| tape && s < native).collect();
        self.seconds_labels = self.seconds.iter().map(|&s| span_label(s)).collect();
        self.spans = std::iter::once(None)
            .chain(
                TIMEFRAMES
                    .into_iter()
                    .filter(|&s| s > native && s % native == 0)
                    .map(Some),
            )
            .collect();
        self.labels = self.spans.iter().map(|s| span_label(s.unwrap_or(native))).collect();
    }
}

impl<C> Panel<C> for TimeframePanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let Some(native) = native_step(chart.base()).filter(|&s| s > 0) else {
            return;
        };
        let tip = "The length of a bar. Type digits over the chart for any other: 15, 30s, 4h, 1d";
        self.make(native, chart.resources().get::<Tape>().is_some_and(|t| t.0));
        let current = chart.timeframe();
        if !self.seconds.is_empty() {
            let labels: [&str; SECONDS.len()] = std::array::from_fn(|i| self.seconds_labels.get(i).map_or("", |l| l));
            let labels = &labels[..self.seconds.len()];
            let at = self.seconds.iter().position(|&s| Some(s) == current);
            let label = at.map_or(SECONDS_MENU, |i| labels[i]);
            if let Some(pick) = ui.menu_choice(label, at, labels) {
                chart.set_timeframe(Some(self.seconds[pick]));
            }
            ui.tip("Bars of seconds, made of the trades");
        }
        let labels: [&str; TIMEFRAMES.len() + 1] = std::array::from_fn(|i| self.labels.get(i).map_or("", |l| l));
        let labels = &labels[..self.labels.len()];
        // Bars of seconds shown: none of these is lit.
        let mut selected = self.spans.iter().position(|&s| s == current).unwrap_or(usize::MAX);
        if ui.choice(&mut selected, labels) {
            chart.set_timeframe(self.spans[selected]);
        }
        ui.tip(tip);
    }
}

/// Where the replay is and when playing it pauses. Made for a column; starting and playing it are the
/// [`ReplayButton`]'s and the [`ReplayPlayer`]'s.
#[derive(Default)]
pub struct ReplayPanel {
    /// The time playing pauses at, as typed.
    clock: String,
}

/// The button of the bar that starts choosing where a replay starts, and ends a replay.
pub struct ReplayButton;

/// What the replay button is found by.
pub const REPLAY: &str = "Replay";

impl<C> Panel<C> for ReplayButton {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let replay = chart.resources().get::<Replay>().cloned().unwrap_or_default();
        if ui.toggle(REPLAY, replay.active || replay.picking) {
            command(
                chart,
                if replay.active {
                    commands::REPLAY_TOGGLE
                } else {
                    commands::REPLAY_PICK
                },
            );
        }
        ui.tip(if replay.active {
            "Leave the replay: all the data again, the trades kept"
        } else {
            "Go back to a moment in history and move forward bar by bar, trading as if it were live. Then click \
             a bar, or type a date under the chart"
        });
    }
}

/// A time typed as `9:30` or `09:30`, as minutes after midnight; nothing typed is none, and `None` is a typo.
fn clock_minutes(text: &str) -> Option<Option<u32>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(None);
    }
    let (hours, minutes) = text.split_once(':')?;
    let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
    (hours < 24 && minutes < 60 && text.len() <= 5).then_some(Some(hours * 60 + minutes))
}

fn clock_text(clock: Option<u32>) -> String {
    clock.map_or(String::new(), |m| format!("{:02}:{:02}", m / 60, m % 60))
}

impl ReplayPlayer {
    /// The field of a date to start at, taken at Enter.
    fn date_field(&mut self, ui: &mut Ui, chart: &mut Chart) {
        let field = ui.text_field(&mut self.date, 120.0, START_DATE);
        ui.tip("Like 2026-09-15 14:00, then Enter");
        if field.changed {
            self.error = None;
        }
        if field.submitted {
            self.error = self.start_at_date(chart).err();
        }
    }

    /// Starts the replay (again, if it runs) at the moment typed, on the chart's clock: the bars that opened before
    /// it are shown.
    fn start_at_date(&self, chart: &mut Chart) -> Result<(), String> {
        let typed = parse_time(&self.date).ok_or("Type a date like 2026-09-15 14:00")?;
        let step = native_step(chart.base()).unwrap_or(60_000);
        // Bars of a day or longer are named by their dates, as shown.
        let at = if step >= MS_PER_DAY {
            typed
        } else {
            zone(chart).utc(typed)
        };
        let (first, last) = chart.data_span().ok_or("no data")?;
        if at <= first {
            return Err("The data begin later".into());
        }
        if at > last {
            return Err("The data end before that".into());
        }
        restart_at(chart, at);
        Ok(())
    }
}

/// Starts the replay (again, if it runs) at `at`: the bars that opened before it are shown. What was traded
/// stays, as when the replay is left.
pub(crate) fn restart_at(chart: &mut Chart, at: Time) {
    if chart.resources().get::<Replay>().is_some_and(|r| r.active) {
        command(chart, commands::REPLAY_TOGGLE);
    }
    if let Some(replay) = chart.resources_mut().get_mut::<Replay>() {
        replay.start_at = Some(at);
    }
    command(chart, commands::REPLAY_TOGGLE);
}

/// The clock the chart's times are shown on.
pub(crate) fn zone(chart: &Chart) -> Zone {
    chart.resources().get::<Zone>().copied().unwrap_or_default()
}

/// The clock the chart's times are written on, without dates in a blind replay.
pub(crate) fn clock(chart: &Chart) -> Clock {
    tickerlight_core::builtin::clock(chart.resources())
}

/// A moment between a tenth and nine tenths of the chart's history, drawn anew each time.
fn random_moment(chart: &Chart) -> Option<Time> {
    let (first, last) = chart.data_span()?;
    let draw = RandomState::new().hash_one((first, last));
    let share = 0.1 + 0.8 * (draw >> 11) as f64 / (1u64 << 53) as f64;
    Some(Time(first.0 + ((last.0 - first.0) as f64 * share) as i64))
}

/// What tests find the replay's timeline by.
pub const TIMELINE: &str = "Timeline";

/// A replay's speed as market time a second: `1s/s` is real time, `5m/s` three hundred times it.
fn speed_label(speed: f64) -> String {
    format!("{}/s", span_label((speed * 1000.0).round() as i64))
}

/// Where a replay playing by time stands, to the second.
fn moment_label(moment: Time, clock: Clock) -> String {
    let seconds = moment.0.div_euclid(1000).rem_euclid(60);
    format!("{}:{seconds:02}", full_label(moment, 60_000, clock))
}

/// The replay as a player, for the row under the chart. While the start is chosen: a date to type, a random
/// start, cancel. In a replay: play and pause, step, speed, leave. Then a timeline of the whole history with the
/// part played since the start; a click or a drag on it starts there.
#[derive(Default)]
pub struct ReplayPlayer {
    /// The date typed to start at.
    date: String,
    /// Why the last start at a date did not happen.
    error: Option<String>,
}

/// What the field of the date to start at is found by.
pub const START_DATE: &str = "Start date";
/// The random start's button.
pub const RANDOM: &str = "Random start";

impl<C> Panel<C> for ReplayPlayer {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let Some((first, last)) = chart.data_span() else {
            return;
        };
        let replay = chart.resources().get::<Replay>().cloned().unwrap_or_default();
        let step = native_step(chart.base()).unwrap_or(60_000);
        let cursor = chart.cursor().filter(|_| replay.active);
        let clock = clock(chart);
        if replay.active {
            if ui.button(if replay.playing { "Pause" } else { "Play" }) {
                command(chart, commands::REPLAY_PLAY_PAUSE);
            }
            ui.tip("Space");
            if ui.button("Step") {
                command(chart, commands::REPLAY_STEP);
            }
            ui.tip("One bar. Shift+Right arrow");
            if ui.button("Slower") {
                command(chart, commands::REPLAY_SLOWER);
            }
            ui.tip("Down arrow or -");
            ui.heading(&speed_label(replay.speed));
            ui.tip(&format!("{}× real time while playing", replay.speed));
            if ui.button("Faster") {
                command(chart, commands::REPLAY_FASTER);
            }
            ui.tip("Up arrow or +");
            if clock.blind
                && ui.button("Show the date")
                && let Some(replay) = chart.resources_mut().get_mut::<Replay>()
            {
                replay.blind = false;
            }
            self.date_field(ui, chart);
            random_start(ui, chart);
            if ui.button("Exit replay") {
                command(chart, commands::REPLAY_TOGGLE);
            }
            ui.tip("Back to all the data, the trades kept");
            if let Some(error) = &self.error {
                let down = ui.theme().down;
                ui.label_colored(error, down);
                return;
            }
            ui.separator();
            if let Some(cursor) = cursor {
                // Playing by time, the moment to the second; moved bar by bar, the bar.
                match chart.moment() {
                    Some(moment) => ui.label(&moment_label(moment, clock)),
                    None => ui.label(&full_label(cursor, step, clock)),
                }
            }
        } else {
            let accent = ui.theme().accent;
            ui.label_colored(PICKING, accent);
            self.date_field(ui, chart);
            random_start(ui, chart);
            if ui.button("Cancel") {
                command(chart, commands::CANCEL);
            }
            ui.tip("Esc");
            if let Some(error) = &self.error {
                let down = ui.theme().down;
                ui.label_colored(error, down);
                return;
            }
            ui.separator();
        }
        // Where the replay stands in the history would tell when it is.
        if clock.blind {
            return;
        }
        let span = (last.0 - first.0).max(1) as f64;
        let fraction = |t: Time| ((t.0 - first.0) as f64 / span) as f32;
        let time_at = |f: f32| Time(first.0 + (f as f64 * span).round() as i64);
        let at = cursor.map(fraction);
        let played = replay.from.zip(at).map(|(from, at)| (fraction(from), at));
        if let Some(f) = ui.timeline(TIMELINE, at, played, |f| full_label(time_at(f), step, clock)) {
            restart_at(chart, time_at(f));
        }
    }
}

/// The button that starts the replay at a moment picked at random, its dates hidden.
fn random_start(ui: &mut Ui, chart: &mut Chart) {
    if ui.button(RANDOM)
        && let Some(at) = random_moment(chart)
    {
        restart_at(chart, at);
        if let Some(replay) = chart.resources_mut().get_mut::<Replay>() {
            replay.blind = true;
        }
    }
    ui.tip("At a moment picked at random, its date hidden until the replay ends or reaches the last bar");
}

impl<C> Panel<C> for ReplayPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let replay = chart.resources().get::<Replay>().cloned().unwrap_or_default();
        let on = replay.active;
        ui.section("Bar replay");
        if on && let Some(cursor) = chart.cursor() {
            let step = native_step(chart.base()).unwrap_or(60_000);
            let clock = clock(chart);
            let at = full_label(cursor, step, clock);
            // Where it is in the history would tell when it is.
            let bars = if clock.blind {
                "date hidden".into()
            } else {
                // Counted in the data's own bars, which a longer timeframe would not show: a share of them.
                format!(
                    "{:.1}% of the history",
                    100.0 * replay.shown as f64 / replay.total.max(1) as f64
                )
            };
            ui.value(&at, &bars, None);
        }
        let mut stops = replay.stops;
        if ui.toggle("Pause at price lines", stops.lines) {
            stops.lines = !stops.lines;
        }
        ui.tip("Playing pauses when the price reaches a horizontal line drawn on the chart");
        let mut to_clock = false;
        let mut typo = false;
        ui.columns(2, |ui| {
            let field = ui.text_field(&mut self.clock, 0.0, "Pause at time");
            ui.tip("Like 09:30, on the chart's clock: playing pauses there each day");
            match clock_minutes(&self.clock) {
                Some(clock) if field.changed => stops.clock = clock,
                // Not being typed in, it shows the time kept (one from a session too).
                Some(clock) if !field.focused && clock != stops.clock => self.clock = clock_text(stops.clock),
                Some(_) => {}
                None => typo = true,
            }
            if on && stops.clock.is_some() {
                to_clock = ui.button("Play to it");
                ui.tip("At once, to the next time the clock reads it");
            }
        });
        if stops != replay.stops
            && let Some(replay) = chart.resources_mut().get_mut::<Replay>()
        {
            replay.stops = stops;
        }
        if to_clock {
            command(chart, commands::REPLAY_TO_CLOCK);
        }
        if typo {
            let down = ui.theme().down;
            ui.label_colored("Type a time like 09:30", down);
        }
    }
}

/// What the player says while the start is chosen.
pub const PICKING: &str = "Click the bar to start from, or";

/// The drawing tools, as a column of icons: a button a group, showing the group's tool used last.
#[derive(Default)]
pub struct DrawToolsPanel {
    /// The tool each group shows, by its place in the group.
    shown: [usize; TOOL_GROUPS.len()],
}

/// The icon of a drawing tool.
pub fn tool_icon(tool: Tool) -> Icon {
    match tool {
        Tool::Trend => Icon::Trend,
        Tool::Ray => Icon::Ray,
        Tool::Extended => Icon::Extended,
        Tool::Arrow => Icon::Arrow,
        Tool::HLine => Icon::HLine,
        Tool::HRay => Icon::HRay,
        Tool::VLine => Icon::VLine,
        Tool::Cross => Icon::Cross,
        Tool::Channel => Icon::Channel,
        Tool::Rect => Icon::Rect,
        Tool::Fib => Icon::Fib,
        Tool::PriceRange => Icon::PriceRange,
        Tool::DateRange => Icon::DateRange,
        Tool::Measure => Icon::Ruler,
        Tool::Long => Icon::Long,
        Tool::Short => Icon::Short,
        Tool::Text => Icon::Text,
    }
}

/// How a tool is used, for its tip.
fn tool_tip(tool: Tool) -> String {
    let how = match tool.clicks() {
        1 if tool == Tool::Text => "click, then type",
        1 => "click",
        2 => "click two points or drag",
        _ => "two points, then a third for the width",
    };
    let key = match tool {
        Tool::Trend => ". Alt+T",
        Tool::HLine => ". Alt+H",
        Tool::HRay => ". Alt+J",
        Tool::VLine => ". Alt+V",
        Tool::Cross => ". Alt+C",
        Tool::Rect => ". Alt+Shift+R",
        Tool::Fib => ". Alt+F",
        _ => "",
    };
    format!("{}: {how}{key}", tool.title())
}

/// The tools of the left column, in groups, with what the list of each group's tools is found by.
const TOOL_GROUPS: [(&str, &[Tool]); 6] = [
    ("Lines", &[Tool::Trend, Tool::Ray, Tool::Extended, Tool::Arrow]),
    ("Levels", &[Tool::HLine, Tool::HRay, Tool::VLine, Tool::Cross]),
    ("Shapes", &[Tool::Channel, Tool::Rect, Tool::Fib]),
    ("Measures", &[Tool::PriceRange, Tool::DateRange, Tool::Measure]),
    ("Positions", &[Tool::Long, Tool::Short]),
    ("Notes", &[Tool::Text]),
];

/// What the cursor button is found by.
pub const CURSOR: &str = "Cursor";
pub const MAGNET: &str = "Magnet";
pub const HIDE: &str = "Hide drawings";
pub const REMOVE_ALL: &str = "Remove all drawings";

impl<C> Panel<C> for DrawToolsPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let Some(drawings) = chart.resources().get::<Drawings>() else {
            return;
        };
        let (tool, magnet, hidden) = (drawings.tool, drawings.magnet, drawings.hidden);
        if ui.icon_button(Icon::Cursor, CURSOR, tool.is_none()) {
            command(chart, commands::CANCEL);
        }
        ui.tip("Select and move drawings; with Shift or Ctrl, click or box more. Esc");
        for (group, (more, tools)) in TOOL_GROUPS.iter().enumerate() {
            // A tool picked by a key shows in its group too.
            if let Some(at) = tools.iter().position(|&t| Some(t) == tool) {
                self.shown[group] = at;
            }
            let t = tools[self.shown[group]];
            let (clicked, picked) = if tools.len() == 1 {
                (ui.icon_button(tool_icon(t), t.title(), tool == Some(t)), None)
            } else {
                let mut entries = [(Icon::Cursor, ""); 4];
                for (entry, &t) in entries.iter_mut().zip(tools.iter()) {
                    *entry = (tool_icon(t), t.title());
                }
                let current = tools.iter().position(|&t| Some(t) == tool);
                let entries = &entries[..tools.len()];
                ui.tool_group(entries, self.shown[group], tool == Some(t), more, current)
            };
            ui.tip(&tool_tip(t));
            if clicked {
                command(chart, if tool == Some(t) { commands::CANCEL } else { t.command() });
            }
            if let Some(pick) = picked {
                self.shown[group] = pick;
                command(chart, tools[pick].command());
            }
        }
        ui.separator();
        if ui.icon_button(Icon::Magnet, MAGNET, magnet) {
            command(chart, commands::DRAW_MAGNET);
        }
        ui.tip("Magnet: points snap to the open, high, low or close of the bar");
        let eye = if hidden { Icon::EyeOff } else { Icon::Eye };
        if ui.icon_button(eye, HIDE, hidden) {
            command(chart, commands::DRAW_HIDE);
        }
        ui.tip(if hidden {
            "Show the drawings"
        } else {
            "Hide the drawings"
        });
        if ui.icon_button(Icon::Trash, REMOVE_ALL, false) {
            command(chart, commands::DRAW_CLEAR);
        }
        ui.tip("Remove all drawings. Ctrl+Z brings them back");
    }
}

/// Colors to choose for a drawing.
pub const PALETTE: [(Color, &str); 8] = [
    (palette::BLUE, "Blue"),
    (palette::CORAL, "Red"),
    (palette::AMBER, "Orange"),
    (palette::YELLOW, "Yellow"),
    (palette::JADE, "Green"),
    (palette::CYAN, "Cyan"),
    (palette::VIOLET, "Purple"),
    // Light on the dark look, dark on the light one.
    (palette::STONE, "Contrast"),
];

pub const DELETE_DRAWING: &str = "Delete drawing";
pub const CLONE_DRAWING: &str = "Clone drawing";
pub const LOCK_DRAWING: &str = "Lock drawing";
pub const UNLOCK_DRAWING: &str = "Unlock drawing";

/// The color, width and dashes of the selected drawing, over the chart while one is selected.
pub struct DrawingStylePanel;

impl<C> Panel<C> for DrawingStylePanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        // Out of the way of a field the keys go to, such as the interval box.
        if ui.typing() {
            return;
        }
        let Some(d) = chart.resources().get::<Drawings>().and_then(|d| d.selection()).cloned() else {
            return;
        };
        let mut style = DrawingStyle {
            color: d.color,
            width: d.width,
            dash: d.dash,
            size: d.size,
        };
        let before = style;
        if d.tool.styled() {
            for (color, name) in PALETTE {
                if ui.swatch(color, name, d.color == color) {
                    style.color = color;
                }
            }
            ui.separator();
        }
        if d.tool == Tool::Text {
            let names = TEXT_SIZES.map(|s| s.to_string());
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let nearest = |i: &usize| ((TEXT_SIZES[*i] - d.size).abs() * 100.0) as i64;
            let mut size = (0..TEXT_SIZES.len()).min_by_key(nearest).unwrap_or(0);
            if ui.choice(&mut size, &names) {
                style.size = TEXT_SIZES[size];
            }
            ui.tip("Text size");
            ui.separator();
        }
        if d.tool.styled() && d.tool != Tool::Text {
            let mut width = (d.width.round() as usize).clamp(1, 4) - 1;
            let widths = [Icon::Width(1), Icon::Width(2), Icon::Width(3), Icon::Width(4)];
            if ui.icon_choice(&mut width, &widths, &["Width 1", "Width 2", "Width 3", "Width 4"]) {
                style.width = (width + 1) as f32;
            }
            ui.separator();
            let dashes = [Dash::Solid, Dash::Dashed, Dash::Dotted];
            let mut dash = dashes.iter().position(|&x| x == d.dash).unwrap_or(0);
            let icons = [Icon::Solid, Icon::Dashed, Icon::Dotted];
            if ui.icon_choice(&mut dash, &icons, &["Solid", "Dashed", "Dotted"]) {
                style.dash = dashes[dash];
            }
            ui.separator();
        }
        if style != before
            && let Some(drawings) = chart.resources_mut().get_mut::<Drawings>()
        {
            drawings.restyle(style);
        }
        if ui.icon_button(Icon::Clone, CLONE_DRAWING, false) {
            command(chart, commands::DRAW_CLONE);
        }
        ui.tip("A copy beside it. Or drag the drawing with Ctrl held; Ctrl+C and Ctrl+V copy it too");
        let (icon, name) = if d.locked {
            (Icon::Lock, UNLOCK_DRAWING)
        } else {
            (Icon::Unlock, LOCK_DRAWING)
        };
        if ui.icon_button(icon, name, d.locked) {
            command(chart, commands::DRAW_LOCK);
        }
        ui.tip(if d.locked {
            "Locked: it stays where it is. Click to unlock"
        } else {
            "Lock it where it is"
        });
        if ui.icon_button(Icon::Trash, DELETE_DRAWING, false) {
            command(chart, commands::DRAW_DELETE);
        }
        ui.tip("Delete the drawing. Key Delete");
    }
}

/// The names of [`SeriesKind::ALL`], in its order.
const SERIES_KINDS: [&str; 5] = ["Candles", "Heikin-Ashi", "Bars", "Line", "Area"];

/// How the price series is drawn, picked from a list.
pub struct ToolsPanel;

impl<C> Panel<C> for ToolsPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let kind = chart.resources().get::<SeriesKind>().copied().unwrap_or_default();
        let mut index = SeriesKind::ALL.iter().position(|&k| k == kind).unwrap_or(0);
        if ui.dropdown(&mut index, &SERIES_KINDS) {
            chart.resources_mut().insert(SeriesKind::ALL[index]);
        }
        ui.tip("How the prices are drawn");
    }
}

/// What the toggles of the price scale are found by.
pub const SCALE_LOG: &str = "log";
pub const SCALE_PERCENT: &str = "%";
pub const SCALE_AUTO: &str = "auto";

/// The price scale of the main pane, at the right end of the row under the chart: log, percent, and fitting the
/// prices on screen again after the axis was dragged.
pub struct ScalePanel;

impl<C> Panel<C> for ScalePanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        ui.to_right(&[SCALE_PERCENT, SCALE_LOG, SCALE_AUTO]);
        let panes = chart.pane_stack_mut();
        let scale = panes.scale(0);
        if ui.toggle(SCALE_PERCENT, scale == ScaleSetting::Percent) {
            let next = if scale == ScaleSetting::Percent {
                ScaleSetting::Linear
            } else {
                ScaleSetting::Percent
            };
            panes.set_scale(0, next);
        }
        ui.tip("Percent from the first bar on screen. Alt+P");
        if ui.toggle(SCALE_LOG, scale == ScaleSetting::Log) {
            let next = if scale == ScaleSetting::Log {
                ScaleSetting::Linear
            } else {
                ScaleSetting::Log
            };
            panes.set_scale(0, next);
        }
        ui.tip("Log scale: equal moves in percent look equal. Alt+L");
        let auto = panes.manual_range(0).is_none();
        if ui.toggle(SCALE_AUTO, auto) && !auto {
            panes.set_manual_range(0, None);
        }
        ui.tip("Fit the prices on screen. Drag the price axis to scale by hand");
    }
}

/// What a menu opened by a right press over the chart offers.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MenuAction {
    ResetView,
    /// The price where the press was.
    CopyPrice(f64),
    /// A horizontal line at that price, in the pane pressed.
    HorizontalLine(usize, f64),
    /// An order of `qty` (signed) waiting at the price there, while trading.
    Order(OrderKind, f64, f64),
    HideDrawings,
    ShowDrawings,
    RemoveDrawings,
    RemoveIndicators,
    Clone,
    Lock,
    Unlock,
    Remove,
    Scale(ScaleSetting),
    Autoscale,
}

impl MenuAction {
    /// What the menu says, with the keys that do the same after a tab.
    fn label(self) -> String {
        match self {
            Self::ResetView => "Reset chart view\tAlt+R".into(),
            Self::CopyPrice(at) => format!("Copy price {}", price(at)),
            Self::HorizontalLine(_, at) => format!("Draw horizontal line at {}\tAlt+H", price(at)),
            Self::Order(kind, qty, at) => {
                let side = if qty > 0.0 { "Buy" } else { "Sell" };
                let kind = if kind == OrderKind::Limit { "limit" } else { "stop" };
                format!("{side} {} {kind} at {}", qty.abs(), price(at))
            }
            Self::HideDrawings => "Hide drawings".into(),
            Self::ShowDrawings => "Show drawings".into(),
            Self::RemoveDrawings => "Remove drawings".into(),
            Self::RemoveIndicators => "Remove indicators".into(),
            Self::Clone => "Clone\tCtrl+drag".into(),
            Self::Lock => "Lock".into(),
            Self::Unlock => "Unlock".into(),
            Self::Remove => "Remove\tDel".into(),
            Self::Scale(ScaleSetting::Linear) => "Regular".into(),
            Self::Scale(ScaleSetting::Log) => "Logarithmic\tAlt+L".into(),
            Self::Scale(ScaleSetting::Percent) => "Percent\tAlt+P".into(),
            Self::Autoscale => "Fit to the prices".into(),
        }
    }
}

/// The menu a right press over the chart opens where it was: on a drawing what to do with it, on the price axis
/// that pane's scale, elsewhere the view, the price there, the drawings and the indicators.
#[derive(Default)]
pub struct ContextMenuPanel {
    /// What the open menu offers.
    actions: Vec<MenuAction>,
    /// The pane whose price axis the menu was opened on.
    pane: usize,
}

impl ContextMenuPanel {
    /// Puts together what the menu for a press at `(x, y)` (the chart's pixels) offers. Returns the one to mark.
    fn open(&mut self, chart: &Chart, (x, y): (f32, f32)) -> Option<usize> {
        self.actions.clear();
        let (w, h) = chart.plot_size();
        if x >= w && y < h {
            self.pane = chart.pane_layout().iter().position(|p| p.contains_y(y)).unwrap_or(0);
            let scale = chart.pane_stack().scale(self.pane);
            self.actions.extend([
                MenuAction::Autoscale,
                MenuAction::Scale(ScaleSetting::Linear),
                MenuAction::Scale(ScaleSetting::Log),
                MenuAction::Scale(ScaleSetting::Percent),
            ]);
            return self.actions.iter().position(|&a| a == MenuAction::Scale(scale));
        }
        let drawings = chart.resources().get::<Drawings>();
        if let Some(d) = drawings.and_then(Drawings::selection) {
            let lock = if d.locked { MenuAction::Unlock } else { MenuAction::Lock };
            self.actions.extend([MenuAction::Clone, lock, MenuAction::Remove]);
            return None;
        }
        self.actions.push(MenuAction::ResetView);
        if let Some((pane, at)) = chart.price_at(y) {
            // As it is written, so a line or an order is at the price the menu says.
            let at = price(at).parse().unwrap_or(at);
            self.actions
                .extend([MenuAction::CopyPrice(at), MenuAction::HorizontalLine(pane, at)]);
            // Above the last price a buy waits as a stop and a sell as a limit; below it the other way round.
            let last = chart.base().close().last().copied();
            let sizing = chart.resources().get::<OrderSizing>().copied().unwrap_or_default();
            if pane == 0
                && can_trade(chart.resources())
                && let (Some(last), Some(account)) = (last, chart.resources().get::<Account>())
                && let Ok(qty) = sizing.quantity(account, at, None, account.equity(last))
            {
                let (buy, sell) = if at < last {
                    (OrderKind::Limit, OrderKind::Stop)
                } else {
                    (OrderKind::Stop, OrderKind::Limit)
                };
                self.actions
                    .extend([MenuAction::Order(buy, qty, at), MenuAction::Order(sell, -qty, at)]);
            }
        }
        if let Some(d) = drawings.filter(|d| !d.items.is_empty()) {
            let hide = if d.hidden {
                MenuAction::ShowDrawings
            } else {
                MenuAction::HideDrawings
            };
            self.actions.extend([hide, MenuAction::RemoveDrawings]);
        }
        if !studies(chart).is_empty() {
            self.actions.push(MenuAction::RemoveIndicators);
        }
        None
    }

    fn run(&self, ui: &mut Ui, chart: &mut Chart, action: MenuAction) {
        match action {
            MenuAction::ResetView => command(chart, commands::VIEW_RESET),
            MenuAction::CopyPrice(at) => ui.copy(price(at)),
            MenuAction::HorizontalLine(pane, at) => {
                let Some(&name) = chart.pane_names().get(pane) else {
                    return;
                };
                let time = chart.seen().time().last().map_or(0.0, |t| t.0 as f64);
                if let Some(drawings) = chart.resources_mut().get_mut::<Drawings>() {
                    let anchor = Anchor { time, price: at };
                    drawings
                        .items
                        .push(Drawing::new(Tool::HLine, name, anchor, drawings.style));
                }
            }
            MenuAction::Order(kind, qty, at) => {
                let Some(after) = chart.order_after() else {
                    return;
                };
                if let Some(account) = chart.resources_mut().get_mut::<Account>() {
                    account.place(Order {
                        kind,
                        qty,
                        price: at,
                        after,
                        reduce_only: account.position() * qty < 0.0,
                        exits: Exits::default(),
                        break_even: None,
                    });
                }
            }
            MenuAction::HideDrawings | MenuAction::ShowDrawings => command(chart, commands::DRAW_HIDE),
            MenuAction::RemoveDrawings => command(chart, commands::DRAW_CLEAR),
            MenuAction::RemoveIndicators => set_studies(chart, Vec::new()),
            MenuAction::Clone => command(chart, commands::DRAW_CLONE),
            MenuAction::Lock | MenuAction::Unlock => command(chart, commands::DRAW_LOCK),
            MenuAction::Remove => command(chart, commands::DRAW_DELETE),
            MenuAction::Scale(scale) => chart.pane_stack_mut().set_scale(self.pane, scale),
            MenuAction::Autoscale => chart.pane_stack_mut().set_manual_range(self.pane, None),
        }
    }
}

impl<C> Panel<C> for ContextMenuPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        // The options are only read when the menu opens.
        let (mut current, mut labels) = (None, Vec::new());
        if let Some(at) = ui.context_at() {
            current = self.open(chart, at);
            labels = self.actions.iter().map(|a| a.label()).collect();
        }
        let options: Vec<&str> = labels.iter().map(String::as_str).collect();
        if let Some(&action) = ui.context_menu(&options, current).and_then(|i| self.actions.get(i)) {
            self.run(ui, chart, action);
        }
    }
}

/// Spans of time to show at once, ending at the newest bar: their labels and lengths (`None`: since the year
/// began; 0: everything).
pub const RANGES: [(&str, Option<i64>); 9] = [
    ("1D", Some(MS_PER_DAY)),
    ("5D", Some(5 * MS_PER_DAY)),
    ("1M", Some(30 * MS_PER_DAY)),
    ("3M", Some(91 * MS_PER_DAY)),
    ("6M", Some(182 * MS_PER_DAY)),
    ("YTD", None),
    ("1Y", Some(365 * MS_PER_DAY)),
    ("5Y", Some(1826 * MS_PER_DAY)),
    ("All", Some(0)),
];

/// The hint of the field that scrolls the chart to a date.
pub const GO_TO: &str = "Go to date";

/// What the chart shows: a span of time ending at the newest bar, or the bars around a date typed in.
#[derive(Default)]
pub struct RangePanel {
    date: String,
    /// Why the last date typed was not gone to.
    error: Option<String>,
}

/// Zooms the chart to show the bars since `from` (all of them for `None`), up to the newest bar.
fn show_since(chart: &mut Chart, from: Option<Time>) {
    let view = chart.seen();
    let len = view.len();
    if len == 0 {
        return;
    }
    let first = from.and_then(|t| view.bar_of_time(t.0 as f64)).unwrap_or(0.0).max(0.0);
    // Room for a few bars after the newest one, as when the chart opens.
    let margin = 3.0;
    let bars = (len as f64 - first + margin).max(1.0);
    let width = chart.plot_size().0 as f64;
    chart.set_time_scale(TimeScale::new(width, width / bars, len as f64 - 1.0 + margin));
}

impl<C> Panel<C> for RangePanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let last = chart.seen().time().last().copied();
        for (label, span) in RANGES {
            if ui.button(label)
                && let Some(last) = last
            {
                let from = match span {
                    Some(0) => None,
                    Some(span) => Some(Time(last.0 - span)),
                    None => parse_time(&format!("{}-01-01", DateTime::from_time(last).year)),
                };
                show_since(chart, from);
            }
        }
        ui.separator();
        let field = ui.text_field(&mut self.date, 110.0, GO_TO);
        ui.tip("Like 2026-09-15 or 2026-09-15 14:00, then Enter. Alt+G");
        if field.changed {
            self.error = None;
        }
        if field.submitted {
            self.error = go_to(chart, &self.date).err();
        }
        if let Some(error) = &self.error {
            let down = ui.theme().down;
            ui.label_colored(error, down);
        }
    }
}

/// Scrolls the chart so the bar of the date typed is in the middle, or says why it did not.
fn go_to(chart: &mut Chart, typed: &str) -> Result<(), String> {
    let typed = parse_time(typed).ok_or("Type a date like 2026-09-15")?;
    let step = chart.timeframe().or(native_step(chart.base())).unwrap_or(60_000);
    // Bars of a day or longer are named by their dates, as shown.
    let at = if step >= MS_PER_DAY {
        typed
    } else {
        zone(chart).utc(typed)
    };
    let view = chart.seen();
    let (Some(&first), Some(&last)) = (view.time().first(), view.time().last()) else {
        return Err("No bars".into());
    };
    if at < first {
        return Err("The data begin later".into());
    }
    if at.0 >= last.0 + step {
        return Err("The bars end before that".into());
    }
    let bar = view.bar_of_time(at.0 as f64).unwrap_or(0.0);
    let mut scale = *chart.time_scale();
    scale.right = bar + scale.width / scale.bar_spacing / 2.0;
    chart.set_time_scale(scale);
    Ok(())
}

const SIDES: [&str; 2] = ["Buy", "Sell"];
const KINDS: [&str; 3] = ["Market", "Limit", "Stop"];
const SIZE_MODES: [&str; 3] = ["Quantity", "Value", "Risk %"];

/// What the size of the order ticket is given in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeMode {
    /// Units of the base asset (coins): the account's order size.
    Quantity,
    /// Value in the quote currency at the entry price.
    #[default]
    Value,
    /// Percent of the equity lost if the stop loss is hit.
    Risk,
}

impl SizeMode {
    pub const ALL: [SizeMode; 3] = [SizeMode::Quantity, SizeMode::Value, SizeMode::Risk];

    pub fn name(self) -> &'static str {
        match self {
            SizeMode::Quantity => "quantity",
            SizeMode::Value => "value",
            SizeMode::Risk => "risk",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// How the order ticket sizes an order, kept as a chart resource so that a session saves it. The quantity mode
/// uses the account's order size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderSizing {
    pub mode: SizeMode,
    /// In the quote currency.
    pub value: f64,
    /// In percent of the equity.
    pub risk: f64,
}

impl Default for OrderSizing {
    fn default() -> Self {
        Self {
            mode: SizeMode::Value,
            value: 1_000.0,
            risk: 1.0,
        }
    }
}

impl OrderSizing {
    /// The quantity (unsigned) of an order entered at `entry`, for an account worth `equity`, with the stop
    /// loss if there is one. Rounded to four significant digits.
    pub fn quantity(&self, account: &Account, entry: f64, stop_loss: Option<f64>, equity: f64) -> Result<f64, String> {
        let qty = match self.mode {
            SizeMode::Quantity => account.order_size,
            SizeMode::Value => self.value / entry,
            SizeMode::Risk => {
                let stop = stop_loss.ok_or("Risk % needs a stop loss")?;
                equity * self.risk / 100.0 / (entry - stop).abs()
            }
        };
        let qty = significant(qty, 4);
        if qty.is_finite() && qty > 0.0 {
            Ok(qty)
        } else {
            Err("The size is zero".into())
        }
    }
}

/// `value` rounded to `digits` significant digits.
fn significant(value: f64, digits: i32) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let scale = 10f64.powi(digits - 1 - value.abs().log10().floor() as i32);
    (value * scale).round() / scale
}

/// A new order: side, size, market, limit or stop, and a take profit and a stop loss for the position it opens.
#[derive(Default)]
pub struct OrderPanel {
    side: usize,
    kind: usize,
    size: String,
    /// The size field takes the keys: what is typed is kept, else it shows the account's size.
    size_focused: bool,
    price: String,
    take_profit: String,
    stop_loss: String,
    break_even: String,
    /// Why the last order was not placed.
    error: Option<String>,
}

impl OrderPanel {
    /// The entry and the stop loss as typed, if they are prices: what the size of the order follows from.
    fn entry_and_stop(&self, last: f64) -> (f64, Option<f64>) {
        let entry = if self.kind == 0 {
            Some(last)
        } else {
            parse_price(&self.price)
        };
        (entry.unwrap_or(last), parse_price(&self.stop_loss))
    }

    fn submit(&self, chart: &mut Chart) -> Result<(), String> {
        if !can_trade(chart.resources()) {
            return Err(NOT_REPLAYING.into());
        }
        let base = chart.base();
        let (Some(&time), Some(&last), Some(after)) = (base.time().last(), base.close().last(), chart.order_after())
        else {
            return Err("no data".into());
        };
        let buy = self.side == 0;
        let optional = |text: &str, name: &str| match text.trim() {
            "" => Ok(None),
            t => parse_price(t).map(Some).ok_or(format!("{name} is not a price")),
        };
        let exits = Exits {
            take_profit: optional(&self.take_profit, "Take profit")?,
            stop_loss: optional(&self.stop_loss, "Stop loss")?,
            break_even: optional(&self.break_even, "Break-even")?,
        };
        let kind = match self.kind {
            0 => None,
            1 => Some(OrderKind::Limit),
            _ => Some(OrderKind::Stop),
        };
        let entry = match kind {
            None => last,
            Some(_) => parse_price(&self.price).ok_or("Price is not a price")?,
        };
        // A limit waits for a better price, a stop for a worse one; on the wrong side they would fill at once.
        let side = if buy { "buy" } else { "sell" };
        let (better, worse) = if buy { ("below", "above") } else { ("above", "below") };
        match kind {
            Some(OrderKind::Limit) if (entry < last) != buy => {
                return Err(format!("A {side} limit goes {better} the price"));
            }
            Some(OrderKind::Stop) if (entry > last) != buy => {
                return Err(format!("A {side} stop goes {worse} the price"));
            }
            _ => {}
        }
        if exits.take_profit.is_some_and(|tp| (tp > entry) != buy) {
            return Err(format!(
                "The take profit goes {} the entry",
                if buy { "above" } else { "below" }
            ));
        }
        if exits.stop_loss.is_some_and(|sl| (sl < entry) != buy) {
            return Err(format!(
                "The stop loss goes {} the entry",
                if buy { "below" } else { "above" }
            ));
        }
        if exits.break_even.is_some() && exits.stop_loss.is_none() {
            return Err("A break-even moves the stop loss: set one".into());
        }
        if exits.break_even.is_some_and(|be| (be > entry) != buy) {
            return Err(format!(
                "The break-even goes {} the entry",
                if buy { "above" } else { "below" }
            ));
        }
        let sizing = chart.resources().get::<OrderSizing>().copied().unwrap_or_default();
        let account = chart.resources_mut().get_mut::<Account>().ok_or("no trading")?;
        let size = sizing.quantity(account, entry, exits.stop_loss, account.equity(last))?;
        let qty = if buy { size } else { -size };
        match kind {
            None => {
                account.market(time, last, qty);
                account.protect(after, qty, exits);
            }
            Some(kind) => account.place(Order {
                kind,
                qty,
                price: entry,
                after,
                // Against the open position it only closes, like an order placed on the chart.
                reduce_only: account.position() * qty < 0.0,
                exits,
                break_even: None,
            }),
        }
        Ok(())
    }
}

impl<C> Panel<C> for OrderPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let Some(account) = chart.resources().get::<Account>().cloned() else {
            return;
        };
        let mut sizing = chart.resources().get::<OrderSizing>().copied().unwrap_or_default();
        let last = chart.base().close().last().copied().unwrap_or(0.0);
        ui.section("Order");
        let sides = [ui.theme().up, ui.theme().down];
        ui.choice_colored(&mut self.side, &SIDES, &sides);
        if ui.choice(&mut self.kind, &KINDS) && self.kind > 0 && self.price.is_empty() {
            self.price = price(last);
        }
        let mut mode = sizing.mode as usize;
        if ui.choice(&mut mode, &SIZE_MODES) {
            // The size carries over between a quantity and a value at the entry price.
            let (entry, stop) = self.entry_and_stop(last);
            let qty = sizing.quantity(&account, entry, stop, account.equity(last)).ok();
            sizing.mode = SizeMode::ALL[mode];
            match (sizing.mode, qty) {
                (SizeMode::Quantity, Some(qty)) => {
                    if let Some(a) = chart.resources_mut().get_mut::<Account>() {
                        a.order_size = qty;
                    }
                }
                (SizeMode::Value, Some(qty)) => sizing.value = (qty * entry).round().max(1.0),
                _ => {}
            }
            self.size_focused = false;
        }
        let size = match sizing.mode {
            SizeMode::Quantity => chart.resources().get::<Account>().map_or(1.0, |a| a.order_size),
            SizeMode::Value => sizing.value,
            SizeMode::Risk => sizing.risk,
        };
        let mut size_changed = None;
        // The field wider than the steps beside it, so a size of a few digits fits a narrow column.
        ui.columns_weighted(&[1.0, 1.4, 0.8, 0.8], |ui| {
            ui.heading("Size");
            if !self.size_focused {
                self.size = format!("{size}");
            }
            let field = ui.text_field(&mut self.size, 0.0, "Order size");
            self.size_focused = field.focused;
            if field.changed {
                size_changed = self
                    .size
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0);
            }
            if ui.button("-") {
                size_changed = Some(size_down(size));
            }
            if ui.button("+") {
                size_changed = Some(size_up(size));
            }
        });
        if let Some(new) = size_changed {
            match sizing.mode {
                SizeMode::Quantity => {
                    if let Some(a) = chart.resources_mut().get_mut::<Account>() {
                        a.order_size = new;
                    }
                }
                SizeMode::Value => sizing.value = new,
                SizeMode::Risk => sizing.risk = new,
            }
        }
        chart.resources_mut().insert(sizing);
        ui.columns(2, |ui| {
            ui.heading("Price");
            if self.kind == 0 {
                ui.heading("at market");
            } else {
                let hint = if self.kind == 1 { "Limit price" } else { "Stop price" };
                ui.text_field(&mut self.price, 0.0, hint);
            }
        });
        // Named beside the fields, so a typed price still says which exit it is.
        ui.columns(2, |ui| {
            ui.heading("Take profit");
            ui.text_field(&mut self.take_profit, 0.0, "none");
        });
        ui.columns(2, |ui| {
            ui.heading("Stop loss");
            ui.text_field(&mut self.stop_loss, 0.0, "none");
        });
        ui.columns(2, |ui| {
            ui.heading("Break-even at");
            ui.text_field(&mut self.break_even, 0.0, "none");
        });
        ui.tip("Once the price reaches it, the stop loss moves to the entry");
        // What the order comes to: its quantity, its value and what it loses at the stop.
        let account = chart.resources().get::<Account>().cloned().unwrap_or_default();
        let (entry, stop) = self.entry_and_stop(last);
        let equity = account.equity(last);
        let qty = sizing.quantity(&account, entry, stop, equity).ok();
        ui.columns(2, |ui| {
            // Beyond the equity the order needs leverage: said, not refused, as futures have it.
            let value = match qty {
                Some(q) if equity > 0.0 && q * entry > equity => {
                    format!("{:.2} ({:.1}x)", q * entry, q * entry / equity)
                }
                Some(q) => format!("{:.2}", q * entry),
                None => "-".into(),
            };
            ui.value("Value", &value, None);
            let risk = match (qty, stop) {
                (Some(q), Some(stop)) if equity > 0.0 => {
                    let loss = q * (entry - stop).abs();
                    format!("{loss:.2} ({:.2}%)", loss * 100.0 / equity)
                }
                (_, None) if sizing.mode == SizeMode::Risk => "needs a stop".into(),
                _ => "-".into(),
            };
            ui.value("Risk", &risk, None);
        });
        let buy = self.side == 0;
        let color = if buy { ui.theme().up } else { ui.theme().down };
        let how = if self.kind == 0 { "at market" } else { KINDS[self.kind] }.to_lowercase();
        let action = match qty {
            Some(q) => format!("{} {q} {how}", SIDES[self.side]),
            None => format!("{} {how}", SIDES[self.side]),
        };
        // Outside a replay, on a chart that is not live, no bar comes after a trade and all of them are in view:
        // there is nothing to trade.
        let clicked = if can_trade(chart.resources()) {
            ui.button_colored(&action, color)
        } else {
            ui.button_if(&action, false);
            ui.heading(NOT_REPLAYING);
            false
        };
        if clicked {
            self.error = self.submit(chart).err();
            // The exits were for this order; the next one is made at another price.
            if self.error.is_none() {
                self.take_profit.clear();
                self.stop_loss.clear();
                self.break_even.clear();
            }
        }
        if let Some(error) = &self.error {
            let color = ui.theme().down;
            ui.label_colored(error, color);
        }
    }
}

const NOT_REPLAYING: &str = "To trade, start a bar replay: Replay in the top bar";

/// The open position, its result at the last price, and the pending orders: each one's price can be typed (Enter
/// moves it) and each can be cancelled.
#[derive(Default)]
pub struct PositionPanel {
    /// The order whose price is being typed, and the text so far.
    editing: Option<(usize, String)>,
}

impl<C> Panel<C> for PositionPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let last = chart.base().close().last().copied();
        let Some(account) = chart.resources().get::<Account>() else {
            return;
        };
        let (position, orders, average) = (account.position(), account.orders().to_vec(), account.average_price());
        ui.section("Position");
        let (up, down) = (ui.theme().up, ui.theme().down);
        match account.average_price() {
            Some(average) => {
                let side = if position > 0.0 { "Long" } else { "Short" };
                ui.value(side, &format!("{} at {}", position.abs(), price(average)), None);
                if let Some(last) = last {
                    let pnl = account.unrealized(last);
                    ui.value("Open result", &money(pnl), Some(if pnl >= 0.0 { up } else { down }));
                }
            }
            None => ui.value("Flat", "", None),
        }
        ui.value("Pending orders", &orders.len().to_string(), None);
        // An order filled or gone while its price was typed: the text was for it.
        if self.editing.as_ref().is_some_and(|(i, _)| *i >= orders.len()) {
            self.editing = None;
        }
        let (mut moved, mut cancelled, mut break_even) = (None, None, None);
        for (i, order) in orders.iter().enumerate() {
            let buy = order.qty > 0.0;
            let name = match (order.kind, order.reduce_only) {
                (OrderKind::Limit, true) => "Take profit",
                (OrderKind::Stop, true) => "Stop loss",
                (OrderKind::Limit, false) if buy => "Buy limit",
                (OrderKind::Limit, false) => "Sell limit",
                (OrderKind::Stop, false) if buy => "Buy stop",
                (OrderKind::Stop, false) => "Sell stop",
            };
            ui.columns(3, |ui| {
                ui.label_colored(name, if buy { up } else { down });
                let mut text = match self.editing.take() {
                    Some((at, text)) if at == i => text,
                    other => {
                        self.editing = other;
                        price(order.price)
                    }
                };
                let field = ui.text_field(&mut text, 0.0, "Order price");
                let side = if buy { "Buys" } else { "Sells" };
                ui.tip(&format!(
                    "{side} {} at this price; type another and press Enter to move it",
                    order.qty.abs()
                ));
                if field.submitted {
                    moved = text.trim().parse::<f64>().ok().map(|p| (i, p));
                } else if field.focused {
                    self.editing = Some((i, text));
                }
                if ui.button("Cancel") {
                    cancelled = Some(i);
                }
            });
            // A stop loss on the losing side of the entry can move to it: at first once the price is as far past
            // the entry as the stop is before it (1R), then where its line is dragged.
            let one_r = average
                .map(|entry| 2.0 * entry - order.price)
                .filter(|_| average.is_some_and(|entry| (order.price - entry) * order.qty > 0.0));
            if order.reduce_only && order.kind == OrderKind::Stop && (order.break_even.is_some() || one_r.is_some()) {
                ui.columns(2, |ui| {
                    let on = order.break_even.is_some();
                    if ui.toggle("Break-even", on) {
                        break_even = Some((i, if on { None } else { one_r }));
                    }
                    ui.tip("Moves this stop to the entry once the price reaches its line (drag it on the chart)");
                    ui.heading(&order.break_even.map_or("at 1R".into(), |t| format!("at {}", price(t))));
                });
            }
        }
        if let Some(account) = chart.resources_mut().get_mut::<Account>() {
            if let Some((i, p)) = moved {
                account.move_order(i, p);
            }
            if let Some(i) = cancelled {
                account.cancel_order(i);
                self.editing = None;
            }
            if let Some((i, trigger)) = break_even {
                account.set_break_even(i, trigger);
            }
        }
        ui.columns(2, |ui| {
            if ui.button_if("Close", position != 0.0 && can_trade(chart.resources())) {
                command(chart, commands::CLOSE_POSITION);
            }
            if ui.button_if("Cancel orders", !orders.is_empty()) {
                command(chart, commands::CANCEL_ORDERS);
            }
        });
    }
}

/// How the session went: the balance, the result, the trades, their win rate and profit factor, the deepest
/// drawdown. The starting balance is typed in before the first fill; the slippage at any time (it counts for the
/// fills after).
#[derive(Default)]
pub struct StatsPanel {
    balance: String,
    balance_focused: bool,
    slippage: String,
    slippage_focused: bool,
    /// Other stops and targets tried, with the fills and bars they were tried on.
    tried: Option<((usize, usize), ExitsTried)>,
}

/// `+1.23%`: a share of the starting balance, signed.
pub(crate) fn percent_of(value: f64, balance: f64) -> String {
    if balance > 0.0 {
        format!("{:+.2}%", value * 100.0 / balance)
    } else {
        "-".into()
    }
}

impl<C> Panel<C> for StatsPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let last = chart.base().close().last().copied().unwrap_or(0.0);
        let Some(account) = chart.resources().get::<Account>() else {
            return;
        };
        let stats = account.stats();
        let (net, balance, fresh) = (account.net(last), account.balance, account.fills().is_empty());
        let slippage = account.slippage;
        let fees = account.fees();
        let (up, down) = (ui.theme().up, ui.theme().down);
        let tint = |v: f64| Some(if v >= 0.0 { up } else { down });
        ui.section("Statistics");
        let mut new_balance = None;
        ui.columns(2, |ui| {
            ui.heading("Starting balance");
            if fresh {
                if !self.balance_focused {
                    self.balance = format!("{balance}");
                }
                let field = ui.text_field(&mut self.balance, 0.0, "Starting balance");
                self.balance_focused = field.focused;
                if field.changed {
                    new_balance = self
                        .balance
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|b| b.is_finite() && *b > 0.0);
                }
            } else {
                // The trades so far were sized and measured against it.
                ui.heading(&format!("{balance:.2}"));
            }
        });
        let mut new_slippage = None;
        ui.columns(2, |ui| {
            ui.heading("Slippage %");
            if !self.slippage_focused {
                // Rounded, so 0.05% does not show as 0.049999…
                self.slippage = format!("{}", (slippage * 1e8).round() / 1e6);
            }
            let field = ui.text_field(&mut self.slippage, 0.0, "Slippage");
            ui.tip("How much worse than the price market and stop orders fill, in % of it");
            self.slippage_focused = field.focused;
            if field.changed {
                new_slippage = self
                    .slippage
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|s| (0.0..100.0).contains(s))
                    .map(|s| s / 100.0);
            }
        });
        if let Some(account) = chart.resources_mut().get_mut::<Account>() {
            if let Some(b) = new_balance {
                account.balance = b;
            }
            if let Some(s) = new_slippage {
                account.slippage = s;
            }
        }
        ui.value("Balance", &format!("{:.2}", balance + net), None);
        ui.columns(2, |ui| {
            ui.value("Net", &money(net), tint(net));
            ui.value("Return", &percent_of(net, balance), tint(net));
        });
        ui.columns(2, |ui| {
            ui.value("Trades", &stats.closed.to_string(), None);
            let win = stats.win_rate().map_or("-".into(), |w| format!("{:.0}%", w * 100.0));
            ui.value("Wins", &win, None);
        });
        ui.columns(2, |ui| {
            let pf = stats.profit_factor().map_or("-".into(), |p| format!("{p:.2}"));
            ui.value("Profit factor", &pf, None);
            ui.value("Fees", &format!("{fees:.2}"), None);
        });
        let expectancy = stats.expectancy();
        ui.value(
            "Per trade",
            &expectancy.map_or("-".into(), money),
            expectancy.and_then(tint),
        );
        ui.tip("What a trade made on average after fees");
        // A row of its own: the runs need more room than half a narrow column.
        let streaks = format!("{} won, {} lost", stats.win_streak, stats.loss_streak);
        ui.value("In a row", &streaks, None);
        ui.tip("The longest runs of wins and of losses");
        let dd = format!("{:.2} ({:.2}%)", stats.max_drawdown, stats.max_drawdown_share * 100.0);
        ui.value("Max drawdown", &dd, None);

        let fills = chart.resources().get::<Account>().map_or(&[][..], |a| a.fills());
        let on = (fills.len(), chart.base().len());
        if self.tried.as_ref().is_some_and(|(seen, _)| *seen != on) {
            self.tried = None;
        }
        if stats.closed > 0 && self.tried.is_none() {
            if ui.button("Try other stops and targets") {
                self.tried = Some((on, exits_tried(fills, chart.base())));
            }
            ui.tip("What the trades would have made with a stop and a target at a fixed % of their entry, on the bars seen");
        }
        if let Some((_, tried)) = &self.tried {
            // Before fees, as each trade's % of its entry added up.
            let share = |v: f64| format!("{:+.2}%", v * 100.0);
            let traded = format!("{} in {}", share(tried.as_traded), count_trades(tried.trades));
            ui.value("As traded", &traded, tint(tried.as_traded));
            for t in tried.tried.iter().take(3) {
                let level = |v: f64| format!("{}%", (v * 1e4).round() / 100.0);
                let name = format!("Stop {}, target {}", level(t.stop), level(t.target));
                ui.value(&name, &share(t.total), tint(t.total));
            }
        }
    }
}

/// `1 trade`, `3 trades`.
pub fn count_trades(n: usize) -> String {
    match n {
        1 => "1 trade".into(),
        n => format!("{n} trades"),
    }
}

/// `16 Oct 23:59`: the date without the year, and the time unless bars are days (with its seconds on bars of
/// seconds), on `clock`; with the dates hidden, the weekday instead: `Fri 23:59`.
pub fn short_time(time: Time, step: i64, clock: impl Into<Clock>) -> String {
    let Clock { zone, blind } = clock.into();
    let t = DateTime::from_time(if step >= MS_PER_DAY { time } else { zone.local(time) });
    let date = if blind {
        t.weekday_name().to_string()
    } else {
        format!("{} {}", t.day, t.month_name())
    };
    if step >= MS_PER_DAY {
        date
    } else if step < 60_000 {
        let second = time.0.rem_euclid(60_000) / 1000;
        format!("{date} {:02}:{:02}:{second:02}", t.hour, t.minute)
    } else {
        format!("{date} {:02}:{:02}", t.hour, t.minute)
    }
}

/// Notes, tags and picks on trades, by the fill's place in the account's fills. A resource of the chart, so
/// whatever keeps a session can keep these too.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Journal {
    pub notes: BTreeMap<usize, Note>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Note {
    pub tags: Vec<String>,
    pub text: String,
    /// The choice picked in each journal column, by the column's name.
    pub picks: BTreeMap<String, String>,
}

impl Note {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.text.trim().is_empty() && self.picks.is_empty()
    }
}

/// A journal column of the user's own: a name, and the choices a trade takes one of.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalColumn {
    pub name: String,
    pub choices: Vec<String>,
}

/// The journal columns, the same in every session. A resource of the chart, put there by the program.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalColumns(pub Vec<JournalColumn>);

impl JournalColumns {
    /// Columns as typed: `Grade: A, B, C; Mistake: early, late`, or a line each. A column needs a name and a
    /// choice; a name or a choice comes once.
    pub fn parse(text: &str) -> Self {
        let mut columns: Vec<JournalColumn> = Vec::new();
        for (name, choices) in text.split([';', '\n']).filter_map(|c| c.split_once(':')) {
            let name = name.trim().to_string();
            let mut picked: Vec<String> = Vec::new();
            for choice in choices.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                if !picked.iter().any(|p| p == choice) {
                    picked.push(choice.to_string());
                }
            }
            if !name.is_empty() && !picked.is_empty() && !columns.iter().any(|c| c.name == name) {
                columns.push(JournalColumn { name, choices: picked });
            }
        }
        Self(columns)
    }

    /// As [`parse`](Self::parse) reads them, the columns joined by `between`.
    pub fn to_text(&self, between: &str) -> String {
        let columns: Vec<String> = self
            .0
            .iter()
            .map(|c| format!("{}: {}", c.name, c.choices.join(", ")))
            .collect();
        columns.join(between)
    }
}

/// Tags as typed: separated by commas or spaces, each once, in lower case.
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in text
        .split([',', ' '])
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
    {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// The closed trades of each tag, as [`Account::stats`] counts them. A trade counts for the tags of the fill that
/// closed it and of the fill that opened its position.
pub fn tag_stats(fills: &[Fill], journal: &Journal) -> Vec<(String, Stats)> {
    note_stats(fills, journal, |note| note.tags.clone())
}

/// The closed trades of each choice picked in a journal column, named `Column: choice`, counted as by
/// [`tag_stats`].
pub fn pick_stats(fills: &[Fill], journal: &Journal) -> Vec<(String, Stats)> {
    note_stats(fills, journal, |note| {
        note.picks
            .iter()
            .map(|(column, choice)| format!("{column}: {choice}"))
            .collect()
    })
}

/// The closed trades of each key of the notes of the fill that closed them and of the fill that opened their
/// position.
fn note_stats(fills: &[Fill], journal: &Journal, keys: impl Fn(&Note) -> Vec<String>) -> Vec<(String, Stats)> {
    let keys_of = |i: Option<usize>| i.and_then(|i| journal.notes.get(&i)).map(&keys).unwrap_or_default();
    trade_stats(fills, |opener, closer| {
        let mut keys: Vec<String> = keys_of(Some(closer)).into_iter().chain(keys_of(opener)).collect();
        keys.sort();
        keys.dedup();
        keys
    })
}

/// The closed trades by the hour they went in at (0 to 23), on the clock of `zone`.
pub fn hour_stats(fills: &[Fill], zone: Zone) -> Vec<(u32, Stats)> {
    trade_stats(fills, |opener, closer| {
        let entry = fills[opener.unwrap_or(closer)].time;
        vec![DateTime::from_time(zone.local(entry)).hour]
    })
}

/// The closed trades by the weekday they went in on, Monday 0 to Sunday 6, on the clock of `zone`.
pub fn weekday_stats(fills: &[Fill], zone: Zone) -> Vec<(u32, Stats)> {
    trade_stats(fills, |opener, closer| {
        let entry = fills[opener.unwrap_or(closer)].time;
        vec![DateTime::from_time(zone.local(entry)).weekday()]
    })
}

/// The weekdays as [`weekday_stats`] numbers them.
pub const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The closed trades of each key a trade has, by the fill that opened its position (if it was in the fills) and
/// the one that closed it.
fn trade_stats<K: Ord>(fills: &[Fill], keys: impl Fn(Option<usize>, usize) -> Vec<K>) -> Vec<(K, Stats)> {
    let mut by_key: BTreeMap<K, Stats> = BTreeMap::new();
    let results = Account::results(fills);
    let mut position = 0.0;
    let mut opener: Option<usize> = None;
    for (i, fill) in fills.iter().enumerate() {
        let before = position;
        if let Some(profit) = results[i] {
            for key in keys(opener, i) {
                by_key.entry(key).or_default().add(profit);
            }
        }
        position = add_position(position, fill.qty);
        let after = position;
        if after == 0.0 {
            opener = None;
        } else if before == 0.0 || before.signum() != after.signum() {
            opener = Some(i);
        }
    }
    by_key.into_iter().collect()
}

/// The fills of the round trip fill `i` is part of: from the one that opened the position to the one that closed
/// it (or turned it), the last so far while it is open.
fn round_trip(fills: &[Fill], i: usize) -> std::ops::Range<usize> {
    let (mut position, mut start) = (0.0, 0);
    for (j, fill) in fills.iter().enumerate() {
        let before = position;
        position = add_position(position, fill.qty);
        let turned = before != 0.0 && position != 0.0 && before.signum() != position.signum();
        if j >= i && (position == 0.0 || turned) && j > start {
            return start..j + 1;
        }
        if before == 0.0 || turned {
            start = j;
        }
    }
    start..fills.len()
}

/// A trade replayed where it happened, on an account of its own that gets the trade's fills as the replay reaches
/// them; the session waits to be gone back to. A resource while it lasts.
pub struct Review {
    fills: Vec<Fill>,
    /// Fills already put in the account.
    added: usize,
    back: Back,
}

/// Where the session was.
struct Back {
    timeframe: Option<i64>,
    cursor: Option<Time>,
    moment: Option<Time>,
    from: Option<Time>,
    view: (f64, f64),
    account: Account,
}

/// Bars of the chart shown before a trade reviewed opens.
const REVIEW_LEAD: i64 = 20;

impl Review {
    /// Replays the round trip of fill `i`: from a few bars before it opened, paused, on an empty account.
    pub fn start(chart: &mut Chart, i: usize) {
        let Some(account) = chart.resources().get::<Account>() else {
            return;
        };
        let fills = account.fills()[round_trip(account.fills(), i)].to_vec();
        let (Some(first), Some(period)) = (fills.first(), chart.timeframe().or(native_step(chart.base()))) else {
            return;
        };
        let at = Time(first.time.0 - REVIEW_LEAD * period);
        let time = chart.time_scale();
        let back = Back {
            timeframe: chart.timeframe(),
            cursor: chart.cursor(),
            moment: chart.moment(),
            from: chart.resources().get::<Replay>().and_then(|r| r.from),
            view: (time.bar_spacing, time.right),
            account: account.clone(),
        };
        // The replay's start cancels what is pending: it is the empty account's, the session's waits aside.
        let scratch = account.empty_like();
        let res = chart.resources_mut();
        res.insert(scratch);
        res.insert(Review { fills, added: 0, back });
        restart_at(chart, at);
    }

    /// Puts the fills the replay has reached in the account.
    fn keep_up(chart: &mut Chart) {
        let step = native_step(chart.base()).unwrap_or(60_000);
        let Some(seen) = chart.moment().or(chart.cursor().map(|t| Time(t.0 + step))) else {
            return;
        };
        let res = chart.resources_mut();
        let Some(review) = res.get_mut::<Review>() else {
            return;
        };
        let reached: Vec<Fill> = review.fills[review.added..]
            .iter()
            .take_while(|f| f.time < seen)
            .copied()
            .collect();
        review.added += reached.len();
        if let Some(account) = res.get_mut::<Account>() {
            for fill in reached {
                account.fill(fill.time, fill.price, fill.qty);
            }
        }
    }

    /// Back to the session as it was: its timeframe, place in the replay, view and account.
    pub fn back(chart: &mut Chart) {
        let Some(Review { back, .. }) = chart.resources_mut().remove::<Review>() else {
            return;
        };
        if let Some(account) = chart.resources_mut().get_mut::<Account>() {
            account.cancel_all();
        }
        chart.set_timeframe(back.timeframe);
        chart.set_cursor(back.cursor);
        chart.set_moment(back.moment);
        if let Some(replay) = chart.resources_mut().get_mut::<Replay>() {
            (replay.active, replay.playing) = (back.cursor.is_some(), false);
            replay.from = back.from;
        }
        // The handlers see the jump with the empty account: the session's orders do not meet those bars again.
        chart.handle(Event::Tick { dt: 0.0 });
        chart.resources_mut().insert(back.account);
        let width = chart.time_scale().width;
        chart.set_time_scale(TimeScale::new(width, back.view.0, back.view.1));
        chart.handle(Event::Tick { dt: 0.0 });
    }
}

/// Every fill, newest first, with what it realized. A click picks a trade for a note and tags (kept in the
/// chart's [`Journal`]).
#[derive(Default)]
pub struct TradesPanel {
    /// The fill picked: its place and itself, so a fill of another session is not taken for it.
    selected: Option<(usize, Fill)>,
    note: String,
    tags: String,
    /// The fill the fields show.
    editing: Option<usize>,
}

const TRADE_COLUMNS: [Column; 5] = [
    Column {
        title: "Time",
        width: 2.3,
        align: Align::Left,
    },
    Column {
        title: "Side",
        width: 0.8,
        align: Align::Left,
    },
    Column {
        title: "Size",
        width: 1.3,
        align: Align::Right,
    },
    Column {
        title: "Price",
        width: 1.7,
        align: Align::Right,
    },
    Column {
        title: "Result",
        width: 1.4,
        align: Align::Right,
    },
];

impl<C> Panel<C> for TradesPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        Review::keep_up(chart);
        let reviewing = chart.resources().get::<Review>().is_some();
        let step = native_step(chart.base()).unwrap_or(60_000);
        let Some(account) = chart.resources().get::<Account>() else {
            return;
        };
        let fills = account.fills().to_vec();
        if self.selected.is_some_and(|(i, fill)| fills.get(i) != Some(&fill)) {
            self.selected = None;
        }
        let selected = self.selected.map(|(i, _)| i);
        if self.editing != selected {
            let note = selected
                .and_then(|i| chart.resources().get::<Journal>()?.notes.get(&i).cloned())
                .unwrap_or_default();
            self.note = note.text;
            self.tags = note.tags.join(", ");
            self.editing = selected;
        }
        ui.section("Trades");
        if reviewing {
            let muted = ui.theme().muted;
            ui.label_colored(
                "Replaying a trade where it happened: its fills come as the replay reaches them. What is traded \
                 meanwhile is not kept.",
                muted,
            );
            if ui.button("Back to the session") {
                Review::back(chart);
                self.selected = None;
                return;
            }
            ui.tip("To where the replay was, with the session's trades and orders");
        } else {
            let (mut note, mut tags) = Default::default();
            ui.columns(2, |ui| {
                note = ui.text_field(&mut self.note, 0.0, "Note");
                tags = ui.text_field(&mut self.tags, 0.0, "Tags");
            });
            // A choice in each journal column: a click picks it, a click on the one picked takes it back.
            let columns = chart.resources().get::<JournalColumns>().cloned().unwrap_or_default();
            let mut picked: Option<(String, Option<String>)> = None;
            if let Some(i) = selected {
                let journal = chart.resources().get::<Journal>();
                let picks = journal
                    .and_then(|j| j.notes.get(&i))
                    .map(|n| n.picks.clone())
                    .unwrap_or_default();
                for column in &columns.0 {
                    ui.columns(column.choices.len() + 1, |ui| {
                        ui.heading(&column.name);
                        for choice in &column.choices {
                            let on = picks.get(&column.name) == Some(choice);
                            if ui.toggle(choice, on) {
                                picked = Some((column.name.clone(), (!on).then(|| choice.clone())));
                            }
                        }
                    });
                }
            }
            if let Some(i) = selected
                && (note.changed || tags.changed || picked.is_some())
            {
                let res = chart.resources_mut();
                if res.get::<Journal>().is_none() {
                    res.insert(Journal::default());
                }
                let journal = res.get_mut::<Journal>().unwrap();
                let mut entry = journal.notes.remove(&i).unwrap_or_default();
                entry.tags = parse_tags(&self.tags);
                entry.text = self.note.trim().to_string();
                match picked {
                    Some((column, Some(choice))) => {
                        entry.picks.insert(column, choice);
                    }
                    Some((column, None)) => {
                        entry.picks.remove(&column);
                    }
                    None => {}
                }
                if !entry.is_empty() {
                    journal.notes.insert(i, entry);
                }
            }
            if let Some(i) = selected {
                if ui.button("Replay this trade") {
                    Review::start(chart, i);
                    self.selected = None;
                    return;
                }
                ui.tip("From a little before it opened, then back to here");
            }
        }
        // What each fill realized after its fees.
        let realized = Account::results(&fills);
        // The notes are on the session's fills, not on those of a review.
        let noted: Vec<bool> = {
            let journal = chart.resources().get::<Journal>().filter(|_| !reviewing);
            (0..fills.len())
                .map(|i| journal.is_some_and(|j| j.notes.contains_key(&i)))
                .collect()
        };
        let (up, down) = (ui.theme().up, ui.theme().down);
        let zone = clock(chart);
        let n = fills.len();
        let selected_row = selected.map(|i| n - 1 - i);
        let clicked = ui.table(&TRADE_COLUMNS, n, selected_row, |row| {
            let i = n - 1 - row;
            let fill = fills[i];
            let buy = fill.qty > 0.0;
            let side = if buy { "Buy" } else { "Sell" };
            let result = match realized[i] {
                Some(r) => Cell::colored(money(r), if r >= 0.0 { up } else { down }),
                None => Cell::new(""),
            };
            let time = short_time(fill.time, step, zone);
            vec![
                // A mark on the trades with a note.
                Cell::new(if noted[i] { format!("{time} *") } else { time }),
                Cell::colored(side, if buy { up } else { down }),
                Cell::new(format!("{}", fill.qty.abs())),
                Cell::new(price(fill.price)),
                result,
            ]
        });
        if let Some(row) = clicked {
            let i = n - 1 - row;
            self.selected = if selected == Some(i) { None } else { Some((i, fills[i])) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dock, Workspace};
    use tickerlight_core::{Candle, Candles, Key, Scene, Surface};

    #[test]
    fn prices_keep_the_digits_that_matter() {
        assert_eq!(price(82245.934), "82245.93");
        assert_eq!(price(2.50123), "2.5012");
        assert_eq!(price(0.000012346), "0.00001235");
        assert_eq!(money(12.3), "+12.30");
        assert_eq!(money(-0.5), "-0.50");
    }

    /// The last close of the test data.
    const LAST: f64 = 100.0;

    fn window() -> Workspace<()> {
        let mut candles = Candles::new();
        for i in 0..300 {
            let p = LAST + ((i - 299) as f64 * 0.2).sin() * 5.0;
            candles
                .push(Candle {
                    time: Time(i * 60_000),
                    open: p,
                    high: p + 1.0,
                    low: p - 1.0,
                    close: p,
                    volume: 1.0,
                })
                .unwrap();
        }
        let mut w = Workspace::new(Chart::new(candles), ())
            .with(Dock::Overlay, ContextMenuPanel::default())
            .with(Dock::Top, TimeframePanel::default())
            .with(Dock::Top, ToolsPanel)
            .with(Dock::Top, ReplayButton)
            .with(Dock::Left, DrawToolsPanel::default())
            .with(Dock::Float, DrawingStylePanel)
            .with(Dock::Player, ReplayPlayer::default())
            .with(Dock::Bottom, RangePanel::default())
            .with(Dock::Bottom, ScalePanel)
            .with(Dock::Right, ReplayPanel::default())
            .with(Dock::Right, OrderPanel::default())
            .with(Dock::Right, PositionPanel::default())
            .with(Dock::Right, StatsPanel::default())
            .with(Dock::Right, TradesPanel::default());
        w.resize(1400.0, 1000.0);
        w.build_scene(&mut Scene::new());
        w
    }

    /// The control with this text; the last one when there are several (a field comes after its heading).
    fn find(w: &Workspace<()>, text: &str) -> (f32, f32) {
        let rects = w.ui_state().rects();
        let (_, area) = rects
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("no {text}: {rects:?}"));
        area.center()
    }

    fn click(w: &mut Workspace<()>, text: &str) {
        let (x, y) = find(w, text);
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        // Draw, as the window would: the controls are laid out anew.
        w.build_scene(&mut Scene::new());
    }

    fn type_into(w: &mut Workspace<()>, field: &str, text: &str) {
        let at = find(w, field);
        type_at(w, at, text);
    }

    /// Types into the field named by the heading beside it (it is recorded right after the heading).
    fn type_beside(w: &mut Workspace<()>, heading: &str, text: &str) {
        let rects = w.ui_state().rects();
        let i = rects
            .iter()
            .position(|(t, _)| t == heading)
            .unwrap_or_else(|| panic!("no {heading}"));
        let at = rects[i + 1].1.center();
        type_at(w, at, text);
    }

    fn type_at(w: &mut Workspace<()>, (x, y): (f32, f32), text: &str) {
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        w.handle(Event::Key(Key::SelectAll));
        w.handle(Event::Key(Key::Delete));
        for c in text.chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Escape));
        w.build_scene(&mut Scene::new());
    }

    fn find_opt(w: &Workspace<()>, text: &str) -> Option<(f32, f32)> {
        w.ui_state().rect_of(text).map(|a| a.center())
    }

    /// A replay started the way the toolbar starts it: 70% across the bars on screen.
    fn start_replay(w: &mut Workspace<()>) {
        w.chart_mut().handle(Event::Command(commands::REPLAY_TOGGLE));
        w.build_scene(&mut Scene::new());
    }

    /// A replay with every bar shown: trades are made at the last close.
    fn replay_at_the_end(w: &mut Workspace<()>) {
        start_at(w, "1970-01-01 04:59");
        click(w, "Step");
        assert!(replay(w).active && w.chart().base().len() == 300);
    }

    fn start_at(w: &mut Workspace<()>, typed: &str) {
        type_into(w, START_DATE, typed);
        click(w, START_DATE);
        w.handle(Event::Key(Key::Enter));
        w.build_scene(&mut Scene::new());
    }

    fn replay(w: &Workspace<()>) -> Replay {
        w.chart().resources().get::<Replay>().unwrap().clone()
    }

    fn texts(w: &mut Workspace<()>) -> Vec<String> {
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        scene.texts.into_iter().map(|t| t.text).collect()
    }

    /// Orders of one coin, so that their size does not follow the price of the replay.
    fn in_coins(w: &mut Workspace<()>) {
        let sizing = OrderSizing {
            mode: SizeMode::Quantity,
            ..OrderSizing::default()
        };
        w.chart_mut().resources_mut().insert(sizing);
        w.build_scene(&mut Scene::new());
    }

    fn account(w: &Workspace<()>) -> Account {
        w.chart().resources().get::<Account>().unwrap().clone()
    }

    #[test]
    fn the_replay_panel_does_what_the_replay_keys_do() {
        let mut w = window();
        assert!(
            find_opt(&w, "Step").is_none(),
            "nothing to step before the replay starts"
        );
        // The start is chosen with a click on the chart; Escape stops choosing.
        click(&mut w, REPLAY);
        assert!(replay(&w).picking);
        w.handle(Event::Key(Key::Escape));
        w.build_scene(&mut Scene::new());
        assert!(!replay(&w).picking);
        click(&mut w, REPLAY);
        let area = w.chart_area();
        let (x, y) = (area.x + area.w * 0.4, area.y + area.h * 0.5);
        let clicked = w.chart().time_scale().index_of((x - area.x) as f64).floor() as usize;
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        let right = w.chart().time_scale().right;
        w.handle(Event::PointerMoved { x: x - 50.0, y });
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        let start = replay(&w);
        assert!(start.active && !start.picking);
        // The bar clicked is the last one shown, and moving on with the button held did not pan.
        assert_eq!(start.shown, clicked + 1);
        assert_eq!(w.chart().time_scale().right, right);
        assert_eq!(w.chart().cursor(), Some(Time((start.shown as i64 - 1) * 60_000)));
        click(&mut w, "Step");
        assert_eq!(replay(&w).shown, start.shown + 1);
        click(&mut w, "Faster");
        assert!(replay(&w).speed > start.speed);
        click(&mut w, "Slower");
        click(&mut w, "Slower");
        assert!(replay(&w).speed < start.speed);
        click(&mut w, "Play");
        assert!(replay(&w).playing && w.needs_tick());
        click(&mut w, "Pause");
        assert!(!replay(&w).playing);
        click(&mut w, "Exit replay");
        assert!(!replay(&w).active);
    }

    #[test]
    fn playing_pauses_at_the_time_typed_and_at_a_drawn_line() {
        // Bars of a minute from 1970-01-01 00:00 to 04:59, on the UTC clock.
        let mut w = window();
        start_at(&mut w, "1970-01-01 01:00");
        type_into(&mut w, "Pause at time", "25:00");
        w.build_scene(&mut Scene::new());
        assert!(w.ui_state().rect_of("Type a time like 09:30").is_some());
        type_into(&mut w, "Pause at time", "03:00");
        w.build_scene(&mut Scene::new());
        assert_eq!(replay(&w).stops.clock, Some(180));
        click(&mut w, "Play to it");
        assert_eq!(w.chart().moment(), Some(Time(180 * 60_000)));
        assert!(!replay(&w).playing, "at once, without playing");

        // Played on, the next 03:00 is past the data.
        type_into(&mut w, "Pause at time", "");
        click(&mut w, "Pause at price lines");
        let drawings = "hline main 0 100 0 100 2962ff\n";
        let drawn = w
            .chart_mut()
            .resources_mut()
            .get_mut::<Drawings>()
            .unwrap()
            .load(drawings, &["main"]);
        assert_eq!(drawn, 1);
        click(&mut w, "Play");
        // Enough time for the rest of the data at once.
        w.handle(Event::Tick { dt: 100.0 });
        let r = replay(&w);
        assert!(!r.playing && r.shown > 180 && r.shown < 200, "{} shown", r.shown);
        let bar = w.chart().base().len() - 1;
        let (low, high) = (w.chart().base().low()[bar], w.chart().base().high()[bar]);
        assert!(
            low <= 100.0 && 100.0 <= high,
            "the bar it paused at reached the line: {low}..{high}"
        );
    }

    #[test]
    fn the_replay_starts_at_a_typed_date_and_scrolls_there() {
        // Bars of a minute from 1970-01-01 00:00 to 04:59.
        let mut w = window();
        start_at(&mut w, "1970-01-01 02:00");
        let r = replay(&w);
        assert!(r.active && r.start_at.is_none());
        // The bars that opened before 02:00 are shown, the last of them on screen.
        assert_eq!(r.shown, 120);
        assert_eq!(w.chart().cursor(), Some(Time(119 * 60_000)));
        assert_eq!(w.chart().time_scale().visible(r.shown).end, 120);

        // During a replay it starts again at the new date, earlier or later.
        start_at(&mut w, "1970-01-01 01:00");
        assert_eq!(replay(&w).shown, 60);
        type_into(&mut w, "Start date", "1970-01-01 04:30");
        w.handle(Event::Key(Key::Enter));
        assert_eq!(replay(&w).shown, 60, "Escape left the field: Enter goes to the chart");
        click(&mut w, "Start date");
        w.handle(Event::Key(Key::Enter));
        assert_eq!(replay(&w).shown, 270);

        for (typed, error) in [
            ("tomorrow", "Type a date like 2026-09-15 14:00"),
            ("1970-01-01 00:00", "The data begin later"),
            ("1970-01-02", "The data end before that"),
        ] {
            start_at(&mut w, typed);
            assert!(w.ui_state().rect_of(error).is_some(), "{typed}");
            assert_eq!(replay(&w).shown, 270, "{typed}");
        }
    }

    #[test]
    fn the_timeline_starts_the_replay_where_it_is_clicked_and_a_drag_moves_it() {
        // Bars of a minute from 00:00 to 04:59: the timeline runs over 299 minutes.
        let mut w = window();
        let at = |w: &Workspace<()>, f: f32| {
            let track = w.ui_state().rect_of(TIMELINE).expect("the timeline");
            let knob = 5.0;
            (track.x + knob + (track.w - 2.0 * knob) * f, track.center().1)
        };
        let (x, y) = at(&w, 0.5);
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        // The bars that opened before 149.5 minutes in.
        let start = Some(Time(149 * 60_000));
        assert!(replay(&w).active);
        assert_eq!(w.chart().cursor(), start);
        assert_eq!(replay(&w).from, start, "played from there");
        click(&mut w, "Step");
        click(&mut w, "Step");
        assert_eq!(w.chart().cursor(), Some(Time(151 * 60_000)));
        assert_eq!(replay(&w).from, start, "the start stays as it plays");

        // Pressed at one place and let go at another: it starts again where it was let go, and the chart got no drag.
        let right = w.chart().time_scale().right;
        let (x, y) = at(&w, 0.8);
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        let (x, _) = at(&w, 0.25);
        w.handle(Event::PointerMoved { x, y: y - 200.0 });
        assert_eq!(w.chart().cursor(), Some(Time(151 * 60_000)), "not before it is let go");
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        assert_eq!(w.chart().cursor(), Some(Time(74 * 60_000)));
        assert_eq!(replay(&w).from, Some(Time(74 * 60_000)));
        assert_ne!(w.chart().time_scale().right, right, "scrolled to the new start");
    }

    #[test]
    fn the_timeframe_choice_sets_the_chart_timeframe() {
        let mut w = window();
        click(&mut w, "5m");
        assert_eq!(w.chart().timeframe(), Some(300_000));
        click(&mut w, "1h");
        assert_eq!(w.chart().timeframe(), Some(3_600_000));
        click(&mut w, "1m");
        assert_eq!(w.chart().timeframe(), None);
    }

    #[test]
    fn a_control_tells_what_it_does_and_its_key_while_the_pointer_is_over_it() {
        let mut w = window();
        assert_eq!(w.ui_state().tip(), None);
        let (x, y) = find(&w, "Trend line");
        w.handle(Event::PointerMoved { x, y });
        let texts = texts(&mut w);
        let tip = "Trend line: click two points or drag. Alt+T";
        assert_eq!(w.ui_state().tip(), Some(tip));
        assert!(texts.contains(&tip.to_string()));
        // At the right edge of the window the tip stays inside it.
        let pause = w.ui_state().rect_of("Pause at price lines").unwrap();
        w.handle(Event::PointerMoved {
            x: pause.x + pause.w - 2.0,
            y: pause.y + 2.0,
        });
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        let tip = scene
            .texts
            .iter()
            .find(|t| t.text.starts_with("Playing pauses"))
            .unwrap();
        let width = tickerlight_core::Font::ui().measure(&tip.text, tip.size);
        assert!(tip.x + width <= 1400.0, "{} + {width}", tip.x);
        w.handle(Event::PointerMoved { x: 400.0, y: 400.0 });
        w.build_scene(&mut Scene::new());
        assert_eq!(w.ui_state().tip(), None, "over the chart");
    }

    #[test]
    fn a_right_press_opens_the_menu_of_what_is_under_it_where_it_was() {
        let mut w = window();
        let area = w.chart_area();
        let (plot_w, plot_h) = w.chart().plot_size();
        let at = |fx: f32, fy: f32| (area.x + plot_w * fx, area.y + plot_h * fy);
        let right_press = |w: &mut Workspace<()>, (x, y): (f32, f32)| {
            w.handle(Event::PointerMoved { x, y });
            w.handle(Event::SecondaryDown);
            w.build_scene(&mut Scene::new());
        };
        let drawings = |w: &Workspace<()>| w.chart().resources().get::<Drawings>().unwrap().items.len();
        // A horizontal line across the middle.
        w.handle(Event::Command(commands::DRAW_HLINE));
        let (x, y) = at(0.3, 0.5);
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        assert_eq!(drawings(&w), 1);
        // On the line: what to do with it.
        right_press(&mut w, at(0.6, 0.5));
        assert!(find_opt(&w, "Reset chart view").is_none());
        click(&mut w, "Clone");
        assert_eq!(drawings(&w), 2);
        // On an empty place: the view and the drawings; what was selected is not any more.
        right_press(&mut w, at(0.6, 0.2));
        let menu = find(&w, "Reset chart view");
        assert!(find_opt(&w, "Remove drawings").is_some() && find_opt(&w, "Clone").is_none());
        assert!(w.chart().resources().get::<Drawings>().unwrap().selection().is_none());
        // It opens where the press was.
        assert!((menu.0 - at(0.6, 0.2).0).abs() < 200.0 && (menu.1 - at(0.6, 0.2).1).abs() < 60.0);
        click(&mut w, "Remove drawings");
        assert_eq!(drawings(&w), 0);
        assert!(find_opt(&w, "Reset chart view").is_none(), "a pick closes it");
        let spacing = w.chart().time_scale().bar_spacing;
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::Wheel { x, delta: 3.0 });
        assert_ne!(w.chart().time_scale().bar_spacing, spacing);
        right_press(&mut w, at(0.6, 0.2));
        click(&mut w, "Reset chart view");
        assert_eq!(w.chart().time_scale().bar_spacing, spacing);
        // The price where the press was: copied, or a horizontal line there.
        let (_, at_price) = w.chart().price_at(at(0.6, 0.2).1 - area.y).unwrap();
        let at_price: f64 = price(at_price).parse().unwrap();
        right_press(&mut w, at(0.6, 0.2));
        click(&mut w, &format!("Copy price {}", price(at_price)));
        assert_eq!(w.take_copied(), Some(price(at_price)));
        right_press(&mut w, at(0.6, 0.2));
        click(&mut w, &format!("Draw horizontal line at {}", price(at_price)));
        let line = w.chart().resources().get::<Drawings>().unwrap().items[0].clone();
        assert_eq!((line.tool, line.a.price), (Tool::HLine, at_price));
        // On the price axis: its scale; Escape closes it.
        right_press(&mut w, (area.x + plot_w + 10.0, at(0.6, 0.5).1));
        assert!(find_opt(&w, "Percent").is_some());
        w.handle(Event::Key(Key::Escape));
        w.build_scene(&mut Scene::new());
        assert!(find_opt(&w, "Percent").is_none());
        right_press(&mut w, (area.x + plot_w + 10.0, at(0.6, 0.5).1));
        click(&mut w, "Logarithmic");
        assert_eq!(w.chart().pane_stack().scale(0), ScaleSetting::Log);
    }

    #[test]
    fn the_tools_pick_drawing_tools_and_the_look_of_the_series() {
        let mut w = window();
        let tool = |w: &Workspace<()>| w.chart().resources().get::<Drawings>().unwrap().tool;
        click(&mut w, "Trend line");
        assert_eq!(tool(&w), Some(Tool::Trend));
        // A group's other tools are in a list beside it; the one picked is what the group shows then.
        assert!(find_opt(&w, "Rectangle").is_none());
        click(&mut w, "Shapes");
        click(&mut w, "Rectangle");
        assert_eq!(tool(&w), Some(Tool::Rect));
        click(&mut w, "Rectangle");
        assert_eq!(tool(&w), None, "a second click puts it down");
        // A key's tool shows in its group too.
        w.handle(Event::Command(Tool::Fib.command()));
        w.build_scene(&mut Scene::new());
        assert!(find_opt(&w, "Fib retracement").is_some());
        click(&mut w, "Long position");
        click(&mut w, CURSOR);
        assert_eq!(tool(&w), None);
        click(&mut w, MAGNET);
        assert!(w.chart().resources().get::<Drawings>().unwrap().magnet);
        let kind = |w: &Workspace<()>| w.chart().resources().get::<SeriesKind>().copied().unwrap();
        click(&mut w, "Candles");
        assert_eq!(kind(&w), SeriesKind::Candles, "the click opens the list");
        click(&mut w, "Bars");
        assert_eq!(kind(&w), SeriesKind::Bars);
        // The price scale's toggles in the corner of the chart.
        let scale = |w: &Workspace<()>| w.chart().pane_stack().scale(0);
        click(&mut w, SCALE_LOG);
        assert_eq!(scale(&w), ScaleSetting::Log);
        click(&mut w, SCALE_PERCENT);
        assert_eq!(scale(&w), ScaleSetting::Percent);
        click(&mut w, SCALE_PERCENT);
        assert_eq!(scale(&w), ScaleSetting::Linear);
        w.chart_mut().pane_stack_mut().set_manual_range(0, Some((1.0, 2.0)));
        click(&mut w, SCALE_AUTO);
        assert_eq!(w.chart().pane_stack().manual_range(0), None);
    }

    #[test]
    fn the_selected_drawing_gets_a_row_of_its_style_over_the_chart() {
        let mut w = window();
        assert!(find_opt(&w, DELETE_DRAWING).is_none(), "nothing selected");
        click(&mut w, "Horizontal line");
        let area = w.chart_area();
        let at = (area.x + area.w * 0.3, area.y + area.h * 0.6);
        w.handle(Event::PointerMoved { x: at.0, y: at.1 });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        let drawings = |w: &Workspace<()>| w.chart().resources().get::<Drawings>().unwrap().clone();
        assert_eq!(drawings(&w).selected, Some(0));
        // The row is centered over the plot, below the top of the chart.
        let delete = w.ui_state().rect_of(DELETE_DRAWING).expect("the style row");
        assert!(delete.y > area.y && delete.y < area.y + 80.0, "{delete:?}");
        click(&mut w, "Red");
        click(&mut w, "Width 3");
        click(&mut w, "Dashed");
        let line = drawings(&w).items[0].clone();
        assert_eq!((line.color, line.width, line.dash), (PALETTE[1].0, 3.0, Dash::Dashed));
        assert_eq!(
            drawings(&w).style.color,
            PALETTE[1].0,
            "the next drawing looks the same"
        );
        // Around the row the pointer is the chart's.
        let red = w.ui_state().rect_of("Red").unwrap();
        w.handle(Event::PointerMoved {
            x: red.x - 60.0,
            y: red.y + red.h / 2.0,
        });
        assert!(w.chart().pointer().position.is_some());
        click(&mut w, DELETE_DRAWING);
        assert!(drawings(&w).items.is_empty());
        assert!(find_opt(&w, DELETE_DRAWING).is_none());
    }

    #[test]
    fn the_style_row_grows_to_the_controls_of_the_drawing_selected() {
        let mut w = window();
        let area = w.chart_area();
        let place = |w: &mut Workspace<()>, tool: &str, points: &[(f32, f32)]| {
            click(w, tool);
            for (x, y) in points {
                let at = (area.x + area.w * x, area.y + area.h * y);
                w.handle(Event::PointerMoved { x: at.0, y: at.1 });
                w.handle(Event::PointerDown);
                w.handle(Event::PointerUp);
            }
            w.build_scene(&mut Scene::new());
        };
        // A price range has only a few controls; a line picked right after it has a whole row of them.
        place(&mut w, "Horizontal line", &[(0.3, 0.7)]);
        place(&mut w, "Price range", &[(0.2, 0.2), (0.3, 0.3)]);
        assert!(find_opt(&w, DELETE_DRAWING).is_some() && find_opt(&w, "Dashed").is_none());
        let at = (area.x + area.w * 0.5, area.y + area.h * 0.7);
        w.handle(Event::PointerMoved { x: at.0, y: at.1 });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        let state = w.ui_state();
        let (red, delete) = (state.rect_of("Red").unwrap(), state.rect_of(DELETE_DRAWING).unwrap());
        assert_eq!(red.y, delete.y, "one row, not wrapped at the width of the last one");
    }

    #[test]
    fn trades_wait_for_a_replay() {
        let mut w = window();
        assert!(w.ui_state().rect_of(NOT_REPLAYING).is_some());
        click(&mut w, "Buy 10 at market");
        assert!(account(&w).fills().is_empty());
        start_replay(&mut w);
        assert!(w.ui_state().rect_of(NOT_REPLAYING).is_none());
        // The size is at the close where the replay is now.
        let rects = w.ui_state().rects();
        let buy = rects
            .iter()
            .find(|(t, _)| t.starts_with("Buy ") && t.ends_with(" at market"));
        let label = buy.unwrap().0.clone();
        click(&mut w, &label);
        assert_eq!(account(&w).fills().len(), 1);
    }

    #[test]
    fn a_market_order_fills_at_the_last_close_with_its_exits() {
        let mut w = window();
        replay_at_the_end(&mut w);
        // An order is worth 1000 by default; a step up makes it 2000, 20 at the close of 100.
        click(&mut w, "+");
        assert_eq!(w.chart().resources().get::<OrderSizing>().unwrap().value, 2000.0);
        type_beside(&mut w, "Take profit", "110");
        type_beside(&mut w, "Stop loss", "95.5");
        click(&mut w, "Buy 20 at market");
        let a = account(&w);
        assert_eq!(a.position(), 20.0);
        assert_eq!(a.fills()[0].price, LAST);
        let orders = a.orders();
        assert_eq!(orders.len(), 2);
        assert_eq!((orders[0].kind, orders[0].price), (OrderKind::Limit, 110.0));
        assert_eq!((orders[1].kind, orders[1].price), (OrderKind::Stop, 95.5));
        assert!(orders.iter().all(|o| o.reduce_only && o.qty == -20.0));

        click(&mut w, "Cancel orders");
        assert!(account(&w).orders().is_empty());
        click(&mut w, "Close");
        assert_eq!(account(&w).position(), 0.0);
        // Both fills are in the list, the closing one with its result.
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        let texts: Vec<&str> = scene.texts.iter().map(|t| t.text.as_str()).collect();
        let sells = texts.iter().filter(|&&t| t == "Sell").count();
        assert!(sells >= 2, "the side choice and the closing fill: {texts:?}");
        assert!(texts.contains(&"+0.00"));
    }

    #[test]
    fn a_pending_order_moves_to_a_price_typed_in_its_row_and_goes_alone_when_cancelled() {
        let mut w = window();
        replay_at_the_end(&mut w);
        type_beside(&mut w, "Take profit", "110");
        type_beside(&mut w, "Stop loss", "95.5");
        click(&mut w, "Buy 10 at market");
        let all = |w: &Workspace<()>, text: &str| -> Vec<(f32, f32)> {
            let rects = w.ui_state().rects();
            rects
                .iter()
                .filter(|(t, _)| t == text)
                .map(|(_, a)| a.center())
                .collect()
        };
        let fields = all(&w, "Order price");
        assert_eq!(fields.len(), 2, "a row for each exit");
        // Left without Enter, a typed price moves nothing: on a live chart half a price could fill.
        type_at(&mut w, fields[0], "120");
        assert_eq!(account(&w).orders()[0].price, 110.0);
        let (x, y) = fields[0];
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        w.handle(Event::Key(Key::SelectAll));
        for c in "120".chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Enter));
        w.build_scene(&mut Scene::new());
        assert_eq!(account(&w).orders()[0].price, 120.0);

        let cancels = all(&w, "Cancel");
        assert_eq!(cancels.len(), 2);
        let (x, y) = cancels[1];
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        let a = account(&w);
        assert_eq!(a.orders().len(), 1, "the stop alone");
        assert_eq!((a.orders()[0].kind, a.orders()[0].price), (OrderKind::Limit, 120.0));
        assert_eq!(a.position(), 10.0);
    }

    #[test]
    fn slippage_is_typed_in_percent_and_a_market_order_fills_that_much_worse() {
        let mut w = window();
        replay_at_the_end(&mut w);
        type_beside(&mut w, "Slippage %", "0.1");
        click(&mut w, "Buy 10 at market");
        let fill = account(&w).fills()[0];
        assert!((fill.price - LAST * 1.001).abs() < 1e-9, "{}", fill.price);
    }

    #[test]
    fn a_break_even_is_set_in_the_ticket_or_at_1r_on_a_stop_loss() {
        let mut w = window();
        replay_at_the_end(&mut w);
        let price = |p: f64| format!("{p}");
        type_beside(&mut w, "Break-even at", &price(LAST + 5.0));
        click(&mut w, "Buy 10 at market");
        assert_eq!(account(&w).position(), 0.0);
        assert!(
            w.ui_state()
                .rect_of("A break-even moves the stop loss: set one")
                .is_some()
        );
        type_beside(&mut w, "Stop loss", &price(LAST - 4.0));
        type_beside(&mut w, "Break-even at", &price(LAST - 1.0));
        click(&mut w, "Buy 10 at market");
        assert!(w.ui_state().rect_of("The break-even goes above the entry").is_some());

        type_beside(&mut w, "Break-even at", &price(LAST + 5.0));
        click(&mut w, "Buy 10 at market");
        let stop = account(&w).orders()[0];
        assert_eq!((stop.price, stop.break_even), (LAST - 4.0, Some(LAST + 5.0)));
        // Taken off on the stop's row, and put back at 1R: as far above the entry as the stop is below it.
        click(&mut w, "Break-even");
        assert_eq!(account(&w).orders()[0].break_even, None);
        click(&mut w, "Break-even");
        let entry = account(&w).average_price().unwrap();
        let trigger = account(&w).orders()[0].break_even.unwrap();
        assert!((trigger - (2.0 * entry - (LAST - 4.0))).abs() < 1e-9, "{trigger}");
    }

    #[test]
    fn exits_on_the_wrong_side_and_limits_above_the_price_are_refused() {
        let mut w = window();
        replay_at_the_end(&mut w);
        type_beside(&mut w, "Take profit", "90");
        click(&mut w, "Buy 10 at market");
        assert_eq!(account(&w).position(), 0.0);
        assert!(w.ui_state().rect_of("The take profit goes above the entry").is_some());

        type_beside(&mut w, "Take profit", "");
        click(&mut w, "Limit");
        // The price starts at the last close: a buy limit there is refused, below it is placed.
        click(&mut w, "Buy 10 limit");
        assert!(w.ui_state().rect_of("A buy limit goes below the price").is_some());
        // The value is at the limit price: 1000 at 98 is 10.2 (four digits).
        type_into(&mut w, "Limit price", "98");
        click(&mut w, "Buy 10.2 limit");
        let orders = account(&w).orders().to_vec();
        assert_eq!(orders.len(), 1);
        assert_eq!(
            (orders[0].kind, orders[0].price, orders[0].qty),
            (OrderKind::Limit, 98.0, 10.2)
        );

        click(&mut w, "Sell");
        click(&mut w, "Stop");
        type_into(&mut w, "Stop price", "99");
        // In coins the size starts as the value was: 1000 at 99.
        click(&mut w, "Quantity");
        assert_eq!(account(&w).order_size, 10.1);
        type_into(&mut w, "Order size", "0.5");
        assert_eq!(account(&w).order_size, 0.5);
        click(&mut w, "Sell 0.5 stop");
        let order = *account(&w).orders().last().unwrap();
        assert_eq!((order.kind, order.price, order.qty), (OrderKind::Stop, 99.0, -0.5));
    }

    #[test]
    fn a_right_press_places_an_order_at_the_price_there_on_the_side_it_waits_on() {
        let mut w = window();
        replay_at_the_end(&mut w);
        let area = w.chart_area();
        let (plot_w, plot_h) = w.chart().plot_size();
        let option = |w: &mut Workspace<()>, fy: f32, start: &str| {
            let (x, y) = (area.x + plot_w * 0.5, area.y + plot_h * fy);
            w.handle(Event::PointerMoved { x, y });
            w.handle(Event::SecondaryDown);
            w.build_scene(&mut Scene::new());
            let rects = w.ui_state().rects();
            let found = rects
                .iter()
                .find(|(t, _)| t.starts_with(start) && (t.contains(" limit at ") || t.contains(" stop at ")))
                .map(|(t, _)| t.clone());
            found.unwrap_or_else(|| panic!("no {start}…"))
        };
        // Above the price a buy waits as a stop and a sell as a limit.
        assert!(option(&mut w, 0.05, "Buy ").contains(" stop at "));
        let sell = option(&mut w, 0.05, "Sell ");
        assert!(sell.contains(" limit at "), "{sell}");
        click(&mut w, &sell);
        let order = account(&w).orders()[0];
        assert_eq!(order.kind, OrderKind::Limit);
        assert!(order.qty < 0.0 && order.price > LAST);
        assert!(sell.ends_with(&price(order.price)), "{sell}");
        // Below it the other way round.
        assert!(option(&mut w, 0.6, "Buy ").contains(" limit at "));
        assert!(option(&mut w, 0.6, "Sell ").contains(" stop at "));
    }

    #[test]
    fn a_risk_in_percent_sizes_the_order_by_its_stop_and_the_balance_by_the_results() {
        let mut w = window();
        replay_at_the_end(&mut w);
        type_into(&mut w, "Starting balance", "20000");
        assert_eq!(account(&w).balance, 20_000.0);
        click(&mut w, "Risk %");
        click(&mut w, "Buy at market");
        assert!(w.ui_state().rect_of("Risk % needs a stop loss").is_some());
        assert!(account(&w).fills().is_empty());
        // 1% of 20000 is 200; with the stop 4 below the entry that is 50.
        type_beside(&mut w, "Stop loss", "96");
        assert!(texts(&mut w).contains(&"200.00 (1.00%)".to_string()));
        click(&mut w, "Buy 50 at market");
        assert_eq!(account(&w).position(), 50.0);
        // The trades so far were sized by it: the starting balance is fixed now.
        let shown = texts(&mut w);
        assert!(
            shown.contains(&"20000.00".into()) && !shown.contains(&"20000".into()),
            "{shown:?}"
        );
        click(&mut w, "Close");
        let texts = texts(&mut w);
        for shown in ["20000.00", "+0.00", "+0.00%", "0.00 (0.00%)"] {
            assert!(texts.contains(&shown.to_string()), "{shown}: {texts:?}");
        }
    }

    #[test]
    fn order_sizes_keep_four_digits() {
        let a = Account::default();
        let s = OrderSizing::default();
        assert_eq!(s.quantity(&a, 82_245.93, None, 10_000.0), Ok(0.01216));
        assert_eq!(s.quantity(&a, 0.00001234, None, 10_000.0), Ok(81_040_000.0));
        let risk = OrderSizing {
            mode: SizeMode::Risk,
            ..s
        };
        assert_eq!(
            risk.quantity(&a, 100.0, Some(100.0), 10_000.0),
            Err("The size is zero".into())
        );
    }

    #[test]
    fn the_wheel_scrolls_the_trades_and_zooms_only_over_the_chart() {
        let mut w = window();
        // Tall enough for the whole table: in a shorter window the wheel scrolls the column to it first.
        w.resize(1400.0, 1200.0);
        in_coins(&mut w);
        start_replay(&mut w);
        for _ in 0..40 {
            click(&mut w, "Buy 1 at market");
            click(&mut w, "Step");
        }
        let first_row = |w: &mut Workspace<()>| {
            let mut scene = Scene::new();
            w.build_scene(&mut scene);
            let header = scene.texts.iter().rposition(|t| t.text == "Result").unwrap();
            scene.texts[header + 1..]
                .iter()
                .map(|t| t.text.clone())
                .take(5)
                .collect::<Vec<_>>()
        };
        let top = first_row(&mut w);
        let spacing = w.chart().time_scale().bar_spacing;
        let (x, y) = find(&w, "Result");
        w.handle(Event::PointerMoved { x, y: y + 60.0 });
        w.handle(Event::Wheel { x, delta: -1.0 });
        assert_ne!(first_row(&mut w), top, "scrolled down");
        assert_eq!(w.chart().time_scale().bar_spacing, spacing, "the chart did not zoom");
        w.handle(Event::Wheel { x, delta: 1.0 });
        assert_eq!(first_row(&mut w), top);
        w.handle(Event::PointerMoved { x: 400.0, y: 400.0 });
        w.handle(Event::Wheel { x: 400.0, delta: 1.0 });
        assert!(w.chart().time_scale().bar_spacing > spacing);
    }

    #[test]
    fn a_trade_is_replayed_where_it_happened_and_the_session_comes_back_as_it_was() {
        let mut w = window();
        w.resize(1400.0, 1200.0);
        in_coins(&mut w);
        start_replay(&mut w);
        click(&mut w, "Buy 1 at market");
        for _ in 0..3 {
            click(&mut w, "Step");
        }
        click(&mut w, "Sell");
        click(&mut w, "Sell 1 at market");
        click(&mut w, "Step");
        click(&mut w, "Buy");
        click(&mut w, "Limit");
        type_into(&mut w, "Limit price", "50");
        click(&mut w, "Buy 1 limit");
        let before = account(&w).clone();
        assert_eq!((before.fills().len(), before.orders().len()), (2, 1));
        let (cursor, entry) = (w.chart().cursor().unwrap(), before.fills()[0].time);

        // The exit picks the same round trip as the entry.
        click(&mut w, &short_time(before.fills()[1].time, 60_000, Zone::Utc));
        click(&mut w, "Replay this trade");
        assert_eq!(
            w.chart().cursor(),
            Some(Time(entry.0 - 21 * 60_000)),
            "20 bars before it"
        );
        assert!(account(&w).fills().is_empty() && account(&w).orders().is_empty());
        assert!(w.ui_state().rect_of("Note").is_none(), "the notes are the session's");
        for _ in 0..21 {
            click(&mut w, "Step");
        }
        assert_eq!(account(&w).fills(), &before.fills()[..1], "the entry, once reached");
        for _ in 0..3 {
            click(&mut w, "Step");
        }
        assert_eq!(account(&w).fills(), before.fills());

        click(&mut w, "Back to the session");
        assert_eq!(w.chart().cursor(), Some(cursor));
        assert_eq!(account(&w), before, "the pending order too");
        click(&mut w, "Step");
        assert_eq!(
            account(&w).orders().len(),
            1,
            "the next event does not take it for a new replay"
        );
    }

    #[test]
    fn a_trade_takes_a_choice_in_each_journal_column_and_counts_for_it() {
        let columns = JournalColumns::parse("Grade: A, B, A\n: nameless; Empty:; Mistake: early, late; Grade: C");
        assert_eq!(columns.to_text("; "), "Grade: A, B; Mistake: early, late");
        let mut w = window();
        w.resize(1400.0, 1200.0);
        w.chart_mut().resources_mut().insert(columns);
        in_coins(&mut w);
        start_replay(&mut w);
        click(&mut w, "Buy 1 at market");
        click(&mut w, "Step");
        click(&mut w, "Sell");
        click(&mut w, "Sell 1 at market");
        let fills = account(&w).fills().to_vec();
        let picks = |w: &Workspace<()>| {
            let journal = w.chart().resources().get::<Journal>().cloned().unwrap_or_default();
            journal.notes.get(&0).map(|n| n.picks.clone()).unwrap_or_default()
        };

        click(&mut w, &short_time(fills[0].time, 60_000, Zone::Utc));
        click(&mut w, "B");
        click(&mut w, "late");
        type_into(&mut w, "Note", "chased it");
        let both: BTreeMap<String, String> = [("Grade", "B"), ("Mistake", "late")]
            .map(|(c, p)| (c.to_string(), p.to_string()))
            .into();
        assert_eq!(picks(&w), both, "kept by the note typed after");
        let journal = w.chart().resources().get::<Journal>().unwrap();
        let counted: Vec<(String, usize)> = pick_stats(&fills, journal)
            .into_iter()
            .map(|(k, s)| (k, s.closed))
            .collect();
        assert_eq!(counted, [("Grade: B".to_string(), 1), ("Mistake: late".to_string(), 1)]);

        click(&mut w, "B");
        assert_eq!(picks(&w).get("Grade"), None, "taken back");
        click(&mut w, "A");
        assert_eq!(picks(&w).get("Grade").map(String::as_str), Some("A"));
    }

    #[test]
    fn a_random_start_hides_the_dates_until_asked_or_left() {
        let mut w = window();
        w.resize(1400.0, 1200.0);
        in_coins(&mut w);
        let (first, last) = w.chart().data_span().unwrap();
        // What is on the screen with the pointer on the chart, its time under the crosshair.
        let shown = |w: &mut Workspace<()>| {
            let a = w.chart_area();
            w.handle(Event::PointerMoved {
                x: a.x + a.w * 0.4,
                y: a.y + a.h * 0.5,
            });
            texts(w)
        };
        let dated = |texts: &[String]| texts.iter().any(|t| t.contains("Jan") || t.contains("'70"));
        assert!(dated(&shown(&mut w)), "the crosshair's date");

        click(&mut w, "Random start");
        let cursor = w.chart().cursor().expect("replaying");
        let span = (last.0 - first.0) as f64;
        let share = (cursor.0 - first.0) as f64 / span;
        assert!((0.09..=0.91).contains(&share), "{share}");
        assert!(clock(w.chart()).blind);
        // No date anywhere, nor where in the history the replay stands.
        let texts_now = shown(&mut w);
        assert!(!dated(&texts_now), "{texts_now:?}");
        assert!(
            texts_now.iter().any(|t| t.starts_with("Thu ")),
            "the crosshair's weekday: {texts_now:?}"
        );
        assert!(texts_now.contains(&"date hidden".to_string()));
        assert!(w.ui_state().rect_of(TIMELINE).is_none());
        click(&mut w, "Buy 1 at market");
        let entry = short_time(account(&w).fills()[0].time, 60_000, clock(w.chart()));
        assert!(entry.starts_with("Thu "), "{entry}");
        assert!(shown(&mut w).contains(&entry), "the trades table");

        click(&mut w, "Show the date");
        assert!(!clock(w.chart()).blind);
        assert!(dated(&shown(&mut w)));
        assert_eq!(w.chart().cursor(), Some(cursor), "where it was");

        // Hidden again from another moment, and shown again by leaving.
        click(&mut w, "Random start");
        assert!(clock(w.chart()).blind);
        click(&mut w, "Exit replay");
        assert!(!replay(&w).blind);
        assert!(dated(&shown(&mut w)));
    }

    #[test]
    fn other_stops_and_targets_are_tried_on_the_trades_until_the_next_bar() {
        let mut w = window();
        w.resize(1400.0, 1200.0);
        in_coins(&mut w);
        start_replay(&mut w);
        let button = "Try other stops and targets";
        assert!(w.ui_state().rect_of(button).is_none(), "no trades to try them on");
        click(&mut w, "Buy 1 at market");
        for _ in 0..3 {
            click(&mut w, "Step");
        }
        click(&mut w, "Sell");
        click(&mut w, "Sell 1 at market");
        click(&mut w, button);
        let tried = exits_tried(account(&w).fills(), w.chart().base());
        let best = tried.tried[0];
        let name = |v: f64| format!("{}%", (v * 1e4).round() / 100.0);
        let row = format!("Stop {}, target {}", name(best.stop), name(best.target));
        assert!(w.ui_state().rect_of(&row).is_some(), "{row}");
        assert!(w.ui_state().rect_of(button).is_none());
        // A bar more and the bars tried on are not those seen.
        click(&mut w, "Step");
        assert!(w.ui_state().rect_of(&row).is_none() && w.ui_state().rect_of(button).is_some());
    }

    #[test]
    fn a_trade_takes_a_note_and_tags_that_count_for_its_round_trip() {
        let mut w = window();
        // Tall enough for the note under the trades.
        w.resize(1400.0, 1200.0);
        in_coins(&mut w);
        start_replay(&mut w);
        click(&mut w, "Buy 1 at market");
        for _ in 0..3 {
            click(&mut w, "Step");
        }
        click(&mut w, "Sell");
        click(&mut w, "Sell 1 at market");
        let fills = account(&w).fills().to_vec();
        assert_eq!(fills.len(), 2);
        let entry = short_time(fills[0].time, 60_000, Zone::Utc);

        click(&mut w, &entry);
        type_into(&mut w, "Note", "late entry");
        type_into(&mut w, "Tags", "Breakout, FOMO breakout");
        let journal = w.chart().resources().get::<Journal>().unwrap().clone();
        let note = Note {
            tags: vec!["breakout".into(), "fomo".into()],
            text: "late entry".into(),
            ..Note::default()
        };
        assert_eq!(journal.notes.into_iter().collect::<Vec<_>>(), [(0, note)]);
        let journal = w.chart().resources().get::<Journal>().unwrap();
        let stats = tag_stats(&fills, journal);
        assert_eq!(
            stats.iter().map(|(t, s)| (t.as_str(), s.closed)).collect::<Vec<_>>(),
            [("breakout", 1), ("fomo", 1)]
        );

        // The noted trade is marked; a second click lets it go and the fields are empty again.
        click(&mut w, &format!("{entry} *"));
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        assert!(!scene.texts.iter().any(|t| t.text == "late entry"));
        // Picking it again shows what it has.
        click(&mut w, &format!("{entry} *"));
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        assert!(scene.texts.iter().any(|t| t.text == "late entry"));
        assert!(scene.texts.iter().any(|t| t.text == "breakout, fomo"));
    }

    #[test]
    fn tags_count_the_trades_they_are_on() {
        let fill = |time, price, qty| Fill {
            time: Time(time),
            price,
            qty,
            fee: 0.0,
        };
        // A long closed in two parts, then a short; the long's entry and the short's exit are tagged.
        let fills = [
            fill(1, 100.0, 2.0),
            fill(2, 110.0, -1.0),
            fill(3, 90.0, -1.0),
            fill(4, 100.0, -1.0),
            fill(5, 95.0, 1.0),
        ];
        let mut journal = Journal::default();
        journal.notes.insert(
            0,
            Note {
                tags: vec!["a".into()],
                text: String::new(),
                ..Note::default()
            },
        );
        journal.notes.insert(
            4,
            Note {
                tags: vec!["a".into(), "b".into()],
                text: String::new(),
                ..Note::default()
            },
        );
        let stats = tag_stats(&fills, &journal);
        let a = stats[0].1;
        assert_eq!(
            (stats[0].0.as_str(), a.closed, a.wins, a.gross_profit, a.gross_loss),
            ("a", 3, 2, 15.0, 10.0)
        );
        let b = stats[1].1;
        assert_eq!((stats[1].0.as_str(), b.closed, b.wins), ("b", 1, 1));
        assert_eq!(parse_tags(" X, y  x,,z "), ["x", "y", "z"]);
    }

    #[test]
    fn trades_count_for_the_hour_and_weekday_they_went_in_at_on_the_clock_chosen() {
        let hour = 3_600_000;
        let fill = |at: i64, price, qty| Fill {
            time: Time(at * hour),
            price,
            qty,
            fee: 0.0,
        };
        // A long in on Thursday 1 January 1970 at 03:00 UTC, out the next day; a short in on Friday at 15:00.
        let fills = [
            fill(3, 100.0, 1.0),
            fill(25, 110.0, -1.0),
            fill(39, 100.0, -1.0),
            fill(40, 105.0, 1.0),
        ];
        let short = |v: Vec<(u32, Stats)>| v.into_iter().map(|(k, s)| (k, s.closed, s.wins)).collect::<Vec<_>>();
        assert_eq!(short(hour_stats(&fills, Zone::Utc)), [(3, 1, 1), (15, 1, 0)]);
        assert_eq!(short(weekday_stats(&fills, Zone::Utc)), [(3, 1, 1), (4, 1, 0)]);
        // Five hours behind in winter: the long went in on Wednesday evening there.
        assert_eq!(short(hour_stats(&fills, Zone::NewYork)), [(10, 1, 0), (22, 1, 1)]);
        assert_eq!(short(weekday_stats(&fills, Zone::NewYork)), [(2, 1, 1), (4, 1, 0)]);
        assert_eq!(WEEKDAYS[3], "Thursday");
    }
}
