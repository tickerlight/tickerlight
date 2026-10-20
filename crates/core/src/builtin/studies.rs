//! Indicators: the common set of a trading chart. Each one is worked out once over the bars the chart may see and
//! then again only from the last bar on, so a live candle or a replay step costs a bar or two, not the history.
//! Those with prices for values are drawn over the candles; the others get a pane of their own.

use core::cell::RefCell;
use core::ops::Range;
use std::rc::Rc;
use std::sync::Mutex;

use tickerlight_text::Font;

use super::drawings::{Dash, Pen, hex, parse_hex};
use super::legend::decimals;
use crate::calendar::Zone;
use crate::palette;
use crate::plugin::{names, order};
use crate::{
    Align, CandleView, Chart, Color, DrawCtx, Event, EventCtx, Handler, Layer, Panes, Resources, Response, Scene, Time,
};

/// What an indicator is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sma,
    Ema,
    Vwap,
    Bollinger,
    Rsi,
    Macd,
    Atr,
    Stochastic,
    Wma,
    Donchian,
    Obv,
    Cci,
    Mfi,
    Adx,
    SuperTrend,
    Pivots,
    Volume,
    Ichimoku,
    Sessions,
    /// A session of one's own: its city, its hours there.
    Session,
}

/// A number an indicator is set with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    /// Only whole numbers: a count of bars.
    pub whole: bool,
    /// One of these, by its index, instead of a number typed in.
    pub choices: &'static [&'static str],
    /// Shown in the indicator's name on the chart, like the 14 of "RSI 14".
    pub in_label: bool,
    /// What the field takes, when its name does not say it all; empty if it does.
    pub tip: &'static str,
}

impl Param {
    /// Whether it is one of [`choices`](Self::choices) rather than a number.
    pub fn is_choice(&self) -> bool {
        !self.choices.is_empty()
    }
}

const fn bars(name: &'static str, default: f64, min: f64) -> Param {
    Param {
        name,
        default,
        min,
        max: 2000.0,
        whole: true,
        choices: &[],
        in_label: true,
        tip: "",
    }
}

const fn factor(name: &'static str, default: f64, min: f64, max: f64) -> Param {
    Param {
        name,
        default,
        min,
        max,
        whole: false,
        choices: &[],
        in_label: true,
        tip: "",
    }
}

const fn choice(name: &'static str, default: usize, choices: &'static [&'static str]) -> Param {
    Param {
        name,
        default: default as f64,
        min: 0.0,
        max: (choices.len() - 1) as f64,
        whole: true,
        choices,
        in_label: false,
        tip: "",
    }
}

/// An hour of the day where a session is: 9.5 is 09:30.
const fn hour(name: &'static str, default: f64, tip: &'static str) -> Param {
    Param {
        name,
        default,
        min: 0.0,
        max: 24.0,
        whole: false,
        choices: &[],
        in_label: false,
        tip,
    }
}

const LENGTH: [Param; 1] = [bars("Length", 20.0, 1.0)];
const BOLLINGER: [Param; 2] = [bars("Length", 20.0, 2.0), factor("Deviations", 2.0, 0.1, 10.0)];
const RSI: [Param; 1] = [bars("Length", 14.0, 2.0)];
const MACD: [Param; 3] = [
    bars("Fast", 12.0, 1.0),
    bars("Slow", 26.0, 1.0),
    bars("Signal", 9.0, 1.0),
];
const ATR: [Param; 1] = [bars("Length", 14.0, 1.0)];
const CCI: [Param; 1] = [bars("Length", 20.0, 2.0)];
const MFI: [Param; 1] = [bars("Length", 14.0, 1.0)];
const ICHIMOKU: [Param; 3] = [
    bars("Conversion", 9.0, 1.0),
    bars("Base", 26.0, 1.0),
    bars("Leading span B", 52.0, 1.0),
];
const ADX: [Param; 2] = [bars("DI length", 14.0, 1.0), bars("ADX smoothing", 14.0, 1.0)];
const SUPERTREND: [Param; 2] = [bars("ATR length", 10.0, 1.0), factor("Factor", 3.0, 0.1, 20.0)];
/// The length of an average of an indicator's line, drawn with it; none at zero.
const AVERAGE: Param = Param {
    tip: "Zero for none",
    ..bars("Average", 0.0, 0.0)
};
const STOCHASTIC: [Param; 3] = [
    bars("%K", 14.0, 1.0),
    bars("%K smoothing", 3.0, 1.0),
    bars("%D", 3.0, 1.0),
];
/// How the sessions are drawn (a box with an edge, a zone with soft edges that blend where sessions meet, or a
/// zone strongest at the open fading toward the close), and when each is open, in the time of its city with its
/// daylight saving. A session that opens and closes at the same hour is off; one that closes before it opens runs
/// over midnight.
const SESSIONS: [Param; 9] = [
    choice("Look", 0, &["Boxes", "Soft", "Gradient"]),
    choice("High and low", 0, &["Off", "On"]),
    choice("Names", 1, &["Off", "On"]),
    hour("Asia opens", 9.0, "Hour in Tokyo, 9.5 is 09:30"),
    hour("Asia closes", 18.0, "Hour in Tokyo, 9.5 is 09:30"),
    hour("London opens", 8.0, "Hour in London, 8.5 is 08:30"),
    hour("London closes", 17.0, "Hour in London, 8.5 is 08:30"),
    hour("New York opens", 8.0, "Hour in New York, 9.5 is 09:30"),
    hour("New York closes", 17.0, "Hour in New York, 9.5 is 09:30"),
];
/// A session of one's own, as many as wanted: the city whose clock its hours are on (with its daylight saving),
/// when it opens and closes there, and how it is drawn (as [`SESSIONS`]), in the indicator's color.
const SESSION: [Param; 6] = [
    choice("City", 1, &Zone::LABELS),
    hour("Opens", 9.5, "Hour in the city, 9.5 is 09:30"),
    hour("Closes", 16.0, "Hour in the city, 9.5 is 09:30"),
    choice("Look", 0, &["Boxes", "Soft", "Gradient"]),
    choice("High and low", 0, &["Off", "On"]),
    choice("Name", 1, &["Off", "On"]),
];
/// Where the hours of [`SESSIONS`] start: the first hour setting.
const SESSION_HOURS: usize = 3;
/// The most settings an indicator has: [`SESSIONS`].
const MOST_PARAMS: usize = SESSIONS.len();

/// The sessions: their names, the time they are kept in, and their colors.
const SESSION_PLACES: [(&str, Zone, Color); 3] = [
    ("Asia", Zone::Tokyo, palette::VIOLET),
    ("London", Zone::London, palette::BLUE),
    ("New York", Zone::NewYork, palette::AMBER),
];
const HOUR: i64 = 3_600_000;

/// The second line of an indicator with two (MACD's signal, the stochastic's %D).
const SECOND: Color = palette::AMBER;
/// The line of an indicator's average.
const AVERAGED: Color = palette::YELLOW;
/// The lines of a rise and a fall (+DI and -DI, SuperTrend's two sides).
const RISING: Color = palette::JADE;
const FALLING: Color = palette::CORAL;

impl Kind {
    pub const ALL: [Kind; 20] = [
        Kind::Sma,
        Kind::Ema,
        Kind::Wma,
        Kind::Vwap,
        Kind::Bollinger,
        Kind::Donchian,
        Kind::Rsi,
        Kind::Macd,
        Kind::Atr,
        Kind::Stochastic,
        Kind::Obv,
        Kind::Cci,
        Kind::Mfi,
        Kind::Adx,
        Kind::SuperTrend,
        Kind::Pivots,
        Kind::Sessions,
        Kind::Session,
        Kind::Volume,
        Kind::Ichimoku,
    ];

    /// Its name in saved files.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Sma => "sma",
            Kind::Ema => "ema",
            Kind::Vwap => "vwap",
            Kind::Bollinger => "bb",
            Kind::Rsi => "rsi",
            Kind::Macd => "macd",
            Kind::Atr => "atr",
            Kind::Stochastic => "stoch",
            Kind::Wma => "wma",
            Kind::Donchian => "dc",
            Kind::Obv => "obv",
            Kind::Cci => "cci",
            Kind::Mfi => "mfi",
            Kind::Adx => "adx",
            Kind::SuperTrend => "supertrend",
            Kind::Pivots => "pivots",
            Kind::Volume => "vol",
            Kind::Ichimoku => "ichimoku",
            Kind::Sessions => "sessions",
            Kind::Session => "session",
        }
    }

    /// Its name on the chart.
    pub fn short(self) -> &'static str {
        match self {
            Kind::Sma => "SMA",
            Kind::Ema => "EMA",
            Kind::Vwap => "VWAP",
            Kind::Bollinger => "BB",
            Kind::Rsi => "RSI",
            Kind::Macd => "MACD",
            Kind::Atr => "ATR",
            Kind::Stochastic => "Stoch",
            Kind::Wma => "WMA",
            Kind::Donchian => "DC",
            Kind::Obv => "OBV",
            Kind::Cci => "CCI",
            Kind::Mfi => "MFI",
            Kind::Adx => "ADX",
            Kind::SuperTrend => "SuperTrend",
            Kind::Pivots => "Pivots",
            Kind::Volume => "Vol",
            Kind::Ichimoku => "Ichimoku",
            Kind::Sessions => "Sessions",
            Kind::Session => "Session",
        }
    }

    /// Its name in full.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Sma => "Moving average",
            Kind::Ema => "Exponential moving average",
            Kind::Vwap => "VWAP",
            Kind::Bollinger => "Bollinger bands",
            Kind::Rsi => "Relative strength index",
            Kind::Macd => "MACD",
            Kind::Atr => "Average true range",
            Kind::Stochastic => "Stochastic",
            Kind::Wma => "Weighted moving average",
            Kind::Donchian => "Donchian channels",
            Kind::Obv => "On balance volume",
            Kind::Cci => "Commodity channel index",
            Kind::Mfi => "Money flow index",
            Kind::Adx => "Directional movement (ADX, DI)",
            Kind::SuperTrend => "SuperTrend",
            Kind::Pivots => "Pivot points (daily)",
            Kind::Volume => "Volume with its average",
            Kind::Ichimoku => "Ichimoku cloud",
            Kind::Sessions => "Trading sessions",
            Kind::Session => "Session of your own",
        }
    }

    pub fn params(self) -> &'static [Param] {
        match self {
            Kind::Sma | Kind::Ema | Kind::Wma | Kind::Donchian | Kind::Volume => &LENGTH,
            Kind::Vwap | Kind::Obv | Kind::Pivots => &[],
            Kind::Adx => &ADX,
            Kind::SuperTrend => &SUPERTREND,
            Kind::Ichimoku => &ICHIMOKU,
            Kind::Sessions => &SESSIONS,
            Kind::Session => &SESSION,
            Kind::Cci => &CCI,
            Kind::Mfi => &MFI,
            Kind::Bollinger => &BOLLINGER,
            Kind::Rsi => &RSI,
            Kind::Macd => &MACD,
            Kind::Atr => &ATR,
            Kind::Stochastic => &STOCHASTIC,
        }
    }

    /// Whether it gets a pane of its own: its values are not prices.
    pub fn own_pane(self) -> bool {
        matches!(
            self,
            Kind::Rsi
                | Kind::Macd
                | Kind::Atr
                | Kind::Stochastic
                | Kind::Obv
                | Kind::Cci
                | Kind::Mfi
                | Kind::Adx
                | Kind::Volume
        )
    }

    /// The color of its first line unless it is given one.
    pub fn color(self) -> Color {
        match self {
            Kind::Rsi => palette::VIOLET,
            Kind::Atr => palette::CORAL,
            Kind::Donchian | Kind::Mfi => palette::JADE,
            Kind::Cci => palette::CYAN,
            Kind::Adx => palette::AMBER,
            Kind::SuperTrend => RISING,
            Kind::Pivots | Kind::Sessions => palette::GRAY,
            _ => palette::BLUE,
        }
    }

    /// How many series it works out: the lines it draws and what they are made from.
    fn columns(self) -> usize {
        match self {
            Kind::Bollinger | Kind::Donchian | Kind::Macd | Kind::Stochastic | Kind::Adx => 3,
            Kind::SuperTrend | Kind::Volume | Kind::Session => 2,
            Kind::Pivots | Kind::Ichimoku => 5,
            Kind::Sessions => 2 * SESSION_PLACES.len(),
            _ => 1,
        }
    }

    /// Whether it is drawn as lines, whose color, width and dash can be set.
    pub fn lined(self) -> bool {
        !matches!(self, Kind::Sessions | Kind::Session)
    }

    /// Whether its color can be set: all but the sessions, which have colors of their own.
    pub fn colored(self) -> bool {
        self != Kind::Sessions
    }

    /// The fixed scale of an oscillator, so its levels stay where the eye expects them.
    fn range(self) -> Option<(f64, f64)> {
        matches!(self, Kind::Rsi | Kind::Stochastic | Kind::Mfi).then_some((0.0, 100.0))
    }

    fn levels(self) -> &'static [f64] {
        match self {
            Kind::Rsi => &[30.0, 50.0, 70.0],
            Kind::Stochastic | Kind::Mfi => &[20.0, 80.0],
            Kind::Cci => &[-100.0, 0.0, 100.0],
            Kind::Adx => &[20.0],
            Kind::Macd => &[0.0],
            _ => &[],
        }
    }

    /// Whether an average of its line can be drawn with it: an oscillator with one main line and no signal line
    /// of its own.
    fn averaged(self) -> bool {
        matches!(
            self,
            Kind::Rsi | Kind::Cci | Kind::Mfi | Kind::Obv | Kind::Atr | Kind::Adx
        )
    }

    /// The kind whose [`key`](Self::key) this is.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// An indicator on a chart: what it is, its settings and its color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Study {
    pub kind: Kind,
    /// As many as `kind.params()`, the rest zero.
    pub params: [f64; MOST_PARAMS],
    pub color: Color,
    /// Kept on the chart, not drawn.
    pub hidden: bool,
    /// The width of its lines, logical pixels: one of [`STUDY_WIDTHS`].
    pub width: f32,
    pub dash: Dash,
    /// The length of the average of its first line drawn with it, or zero: an indicator on the indicator, like an
    /// average of the RSI.
    pub average: f64,
}

/// The widths an indicator's lines can have; the second is the default.
pub const STUDY_WIDTHS: [f32; 4] = [1.0, 1.5, 2.0, 3.0];

impl Study {
    pub fn new(kind: Kind) -> Self {
        let mut params = [0.0; MOST_PARAMS];
        for (p, d) in params.iter_mut().zip(kind.params()) {
            *p = d.default;
        }
        Self {
            kind,
            params,
            color: kind.color(),
            hidden: false,
            width: STUDY_WIDTHS[1],
            dash: Dash::Solid,
            average: 0.0,
        }
    }

    /// What it is set with: its parameters, then the length of its average if it can have one.
    pub fn settings(&self) -> Vec<Param> {
        let mut settings = self.kind.params().to_vec();
        if self.kind.averaged() {
            settings.push(AVERAGE);
        }
        settings
    }

    /// The value of setting `i` of [`settings`](Self::settings).
    pub fn setting(&self, i: usize) -> f64 {
        if i == self.kind.params().len() {
            self.average
        } else {
            self.params[i]
        }
    }

    /// Sets the setting `i` of [`settings`](Self::settings), kept to what it may be.
    pub fn set(&mut self, i: usize, value: f64) {
        if let Some(p) = self.settings().get(i)
            && value.is_finite()
        {
            let value = if p.whole { value.round() } else { value };
            let value = value.clamp(p.min, p.max);
            if i == self.kind.params().len() {
                self.average = value;
            } else {
                self.params[i] = value;
            }
        }
    }

    pub fn params(&self) -> &[f64] {
        &self.params[..self.kind.params().len()]
    }

    /// Like "RSI 14"; what is chosen rather than a number is left out. A session of one's own is its city and
    /// hours: "Session New York 09:30-16:00".
    pub fn label(&self) -> String {
        if self.kind == Kind::Session {
            let clock = |hour: f64| {
                let minutes = (hour * 60.0).round() as i64;
                format!("{:02}:{:02}", minutes / 60, minutes % 60)
            };
            let (open, close) = (clock(self.params[1]), clock(self.params[2]));
            return format!("Session {} {open}-{close}", self.zone().label());
        }
        let mut text = self.kind.short().to_string();
        for (p, param) in self.params().iter().zip(self.kind.params()) {
            if param.in_label {
                text += &format!(" {p}");
            }
        }
        text
    }

    /// Like "rsi-14": a name with no spaces, which panes and layers are called by.
    fn id(&self) -> String {
        let mut text = self.kind.key().to_string();
        for p in self.params() {
            text += &format!("-{p}");
        }
        text
    }

    /// Like "rsi 14 color=7e57c2", then " width=2", " dash=dotted" and " average=14" if not the defaults,
    /// " hidden=1" when hidden.
    pub fn save(&self) -> String {
        let mut text = self.kind.key().to_string();
        for p in self.params() {
            text += &format!(" {p}");
        }
        text += &format!(" color={}", hex(self.color));
        if self.width != STUDY_WIDTHS[1] {
            text += &format!(" width={}", self.width);
        }
        if self.dash != Dash::Solid {
            text += &format!(" dash={}", self.dash.name());
        }
        if self.average > 0.0 {
            text += &format!(" average={}", self.average);
        }
        if self.hidden {
            text += " hidden=1";
        }
        text
    }

    /// Reads what [`save`](Self::save) wrote. Missing settings take their defaults, wrong ones are kept in bounds.
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        let mut study = Self::new(Kind::from_key(words.next()?)?);
        let mut i = 0;
        for word in words {
            if let Some(color) = word.strip_prefix("color=") {
                study.color = parse_hex(color)?;
            } else if let Some(width) = word.strip_prefix("width=") {
                let width: f32 = width.parse().ok()?;
                study.width = width.clamp(STUDY_WIDTHS[0], STUDY_WIDTHS[STUDY_WIDTHS.len() - 1]);
            } else if let Some(average) = word.strip_prefix("average=") {
                study.set(study.kind.params().len(), average.parse().ok()?);
            } else if let Some(dash) = word.strip_prefix("dash=") {
                study.dash = Dash::from_name(dash).unwrap_or_default();
            } else if let Some(hidden) = word.strip_prefix("hidden=") {
                study.hidden = hidden == "1";
            } else {
                study.set(i, word.parse().ok()?);
                i += 1;
            }
        }
        Some(study)
    }

    fn whole(&self, i: usize) -> usize {
        self.params[i] as usize
    }

    /// The city of a session of one's own.
    fn zone(&self) -> Zone {
        Zone::ALL[self.whole(0).min(Zone::ALL.len() - 1)]
    }
}

/// Colors given to the indicators over the candles in turn, so two averages never look alike.
pub const STUDY_COLORS: [Color; 6] = [
    palette::BLUE,
    palette::AMBER,
    palette::CYAN,
    palette::MAGENTA,
    palette::YELLOW,
    palette::JADE,
];

/// The indicators of a chart, in the order they were added, and the names of their panes and layers. Change them
/// with [`set_studies`].
#[derive(Debug, Clone, Default)]
pub struct Studies {
    items: Vec<Study>,
    /// The pane or layer of each, and the layer of its label.
    names: Vec<(&'static str, &'static str)>,
}

impl Studies {
    pub fn items(&self) -> &[Study] {
        &self.items
    }

    /// One line each, as [`Study::save`] writes it.
    pub fn save(items: &[Study]) -> String {
        items.iter().map(|s| s.save() + "\n").collect()
    }

    /// The indicators in `text`; lines that do not read are left out.
    pub fn load(text: &str) -> Vec<Study> {
        text.lines().filter_map(Study::parse).collect()
    }

    /// A new indicator of `kind`, in a color the others over the candles do not have yet.
    pub fn new_study(items: &[Study], kind: Kind) -> Study {
        let mut study = Study::new(kind);
        // SuperTrend's green (red when down) and the grey of pivots between their red and green say something.
        if !kind.own_pane() && !matches!(kind, Kind::SuperTrend | Kind::Pivots | Kind::Sessions) {
            let used = |c: &Color| items.iter().any(|s| s.color == *c);
            let overlays = items.iter().filter(|s| !s.kind.own_pane()).count();
            study.color = STUDY_COLORS
                .iter()
                .find(|c| !used(c))
                .copied()
                .unwrap_or(STUDY_COLORS[overlays % STUDY_COLORS.len()]);
        }
        study
    }
}

/// The indicators of `chart`.
pub fn studies(chart: &Chart) -> &[Study] {
    chart.resources().get::<Studies>().map_or(&[], |s| s.items())
}

/// Puts `items` on the chart in place of the indicators it has. A pane that stays keeps its height.
pub fn set_studies(chart: &mut Chart, items: Vec<Study>) {
    let old = chart.resources_mut().remove::<Studies>().unwrap_or_default();
    let stack = chart.pane_stack();
    let weights: Vec<(&'static str, f32)> = (0..stack.len())
        .filter_map(|i| Some((stack.name(i)?, stack.weight(i)?)))
        .collect();
    for (study, &(name, label)) in old.items.iter().zip(&old.names) {
        if study.kind.own_pane() {
            chart.remove_pane(name);
        } else {
            chart.remove_layer(name);
            chart.remove_layer(label);
        }
    }
    let mut names: Vec<(&'static str, &'static str)> = Vec::new();
    let mut row = 0;
    for (index, study) in items.iter().enumerate() {
        let id = study.id();
        let mut name = id.clone();
        let mut n = 2;
        while names.iter().any(|(taken, _)| *taken == name) {
            name = format!("{id}#{n}");
            n += 1;
        }
        let (label, name) = (intern(&format!("{name} label")), intern(&name));
        names.push((name, label));
        let cache = Rc::new(RefCell::new(Cache::default()));
        let lines = StudyLayer {
            study: *study,
            cache: cache.clone(),
        };
        // Over the drawings and the crosshair, as the legend is.
        let labels = StudyLabel {
            study: *study,
            index,
            row,
            cache,
        };
        if study.kind.own_pane() {
            let weight = weights.iter().find(|(n, _)| *n == name).map_or(PANE_WEIGHT, |w| w.1);
            chart.add_pane(name, weight);
            chart.add_layer_in(Panes::One(name), name, order::SERIES, lines);
            chart.add_layer_in(Panes::One(name), label, order::REPLAY, labels);
        } else {
            chart.add_layer(name, order::OVERLAY, lines);
            chart.add_layer(label, order::REPLAY, labels);
            row += 1;
        }
    }
    chart.resources_mut().insert(Studies { items, names });
    chart.resources_mut().insert(LabelHover::default());
    chart.add_handler(names::STUDY_LABELS, order::STUDY_LABELS_HANDLER, StudyLabels);
}

/// Asks the program to show the settings of an indicator (its index in [`studies`]): its gear button was
/// pressed. The program takes it out of the resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenStudySettings(pub usize);

/// The buttons a label shows while the pointer is on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelButton {
    Hide,
    Settings,
    Remove,
}

const LABEL_BUTTONS: [LabelButton; 3] = [LabelButton::Hide, LabelButton::Settings, LabelButton::Remove];
/// A button's width, logical pixels.
const LABEL_BUTTON: f32 = 18.0;

/// The label under the pointer, and the button of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LabelHover(Option<(usize, Option<LabelButton>)>);

/// Where a label is in its pane, in device pixels: its left, the middle of its line, its height, and the left of
/// its buttons (right after the name).
struct LabelSpot {
    x: f32,
    y: f32,
    h: f32,
    buttons: f32,
}

fn label_spot(study: &Study, row: usize, scale: f32, font_size: f32) -> LabelSpot {
    let px = |v: f32| v * scale;
    let size = px(font_size);
    let x = px(12.0);
    let y = if study.kind.own_pane() {
        px(14.0)
    } else {
        px(16.0 + 20.0 * (row + 1) as f32)
    };
    let name = Font::ui().measure(&study.label(), size);
    LabelSpot {
        x,
        y,
        h: size * 1.6,
        buttons: x + name + px(4.0),
    }
}

/// Shows the buttons of the label under the pointer and does what they say: hide or show, the settings, remove.
struct StudyLabels;

impl StudyLabels {
    /// The label at `(x, y)` (chart coordinates) and the button there.
    fn at(ctx: &EventCtx<'_>, (x, y): (f32, f32)) -> Option<(usize, Option<LabelButton>)> {
        let studies = ctx.res.get::<Studies>()?;
        let mut row = 0;
        for (i, (study, &(name, _))) in studies.items.iter().zip(&studies.names).enumerate() {
            let spot = label_spot(study, row, ctx.scale, ctx.style.font_size);
            let pane = if study.kind.own_pane() {
                (0..ctx.pane_stack.len()).find(|&p| ctx.pane_stack.name(p) == Some(name))?
            } else {
                row += 1;
                0
            };
            let top = ctx.panes.get(pane)?.top;
            let end = spot.buttons + LABEL_BUTTONS.len() as f32 * LABEL_BUTTON * ctx.scale;
            if (y - top - spot.y).abs() <= spot.h / 2.0 && (spot.x - 4.0 * ctx.scale..end).contains(&x) {
                let button = (x >= spot.buttons)
                    .then(|| LABEL_BUTTONS[((x - spot.buttons) / (LABEL_BUTTON * ctx.scale)) as usize]);
                return Some((i, button));
            }
        }
        None
    }
}

impl Handler for StudyLabels {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let hit = ctx.pointer.position.and_then(|p| Self::at(ctx, p));
        match event {
            Event::PointerMoved { .. } | Event::PointerLeft => {
                let hover = LabelHover(hit);
                if ctx.res.get::<LabelHover>() == Some(&hover) {
                    return Response::IGNORED;
                }
                ctx.res.insert(hover);
                Response::REDRAW
            }
            Event::PointerDown => {
                let (Some((i, Some(button))), Some(studies)) = (hit, ctx.res.get::<Studies>()) else {
                    return Response::IGNORED;
                };
                let mut items = studies.items.clone();
                match button {
                    LabelButton::Settings => {
                        ctx.res.insert(OpenStudySettings(i));
                        return Response::CONSUMED_REDRAW;
                    }
                    LabelButton::Hide => items[i].hidden = !items[i].hidden,
                    LabelButton::Remove => {
                        items.remove(i);
                    }
                }
                ctx.defer(move |chart| set_studies(chart, items));
                Response::CONSUMED_REDRAW
            }
            _ => Response::IGNORED,
        }
    }
}

/// Draws the icon of `button` centered at `(x, y)`, `s` across.
fn button_icon(scene: &mut Scene, button: LabelButton, (x, y): (f32, f32), s: f32, color: Color, hidden: bool) {
    let width = (s / 9.0).max(1.0);
    match button {
        LabelButton::Hide => {
            // An eye: two arcs meeting at the corners, a pupil; struck through while hidden.
            let (w, h) = (s * 0.5, s * 0.32);
            let arc = |side: f32| {
                (0..=8).map(move |k| {
                    let t = k as f32 / 8.0;
                    (x - w + 2.0 * w * t, y + side * h * (core::f32::consts::PI * t).sin())
                })
            };
            for side in [-1.0, 1.0] {
                let points: Vec<(f32, f32)> = arc(side).collect();
                for pair in points.windows(2) {
                    scene.line(pair[0], pair[1], width, color);
                }
            }
            scene.circle((x, y), s * 0.14, color);
            if hidden {
                scene.line((x - w, y + w), (x + w, y - w), width, color);
            }
        }
        LabelButton::Settings => {
            // A gear: a ring with teeth.
            let r = s * 0.3;
            for k in 0..8 {
                let a = k as f32 * core::f32::consts::FRAC_PI_4;
                let (dx, dy) = (a.cos(), a.sin());
                scene.line(
                    (x + dx * r * 0.8, y + dy * r * 0.8),
                    (x + dx * r * 1.45, y + dy * r * 1.45),
                    width * 1.6,
                    color,
                );
            }
            for k in 0..16 {
                let (a0, a1) = (k as f32, k as f32 + 1.0);
                let at = |a: f32| {
                    let a = a * core::f32::consts::PI / 8.0;
                    (x + a.cos() * r, y + a.sin() * r)
                };
                scene.line(at(a0), at(a1), width * 1.3, color);
            }
        }
        LabelButton::Remove => {
            let d = s * 0.3;
            scene.line((x - d, y - d), (x + d, y + d), width, color);
            scene.line((x - d, y + d), (x + d, y - d), width, color);
        }
    }
}

/// Height of a new indicator pane relative to the main one.
const PANE_WEIGHT: f32 = 0.3;

/// Panes and layers are named with `&'static str`; the names of indicators are made as they are added, so each
/// distinct one is kept for good. There are only as many as the user ever set up.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(n);
    n
}

/// What carries over from one bar to the next.
#[derive(Debug, Clone, Copy)]
struct State {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    day: i64,
    /// The sum of the first line over the bars of its average, and how many of them have it.
    sum: f64,
    count: usize,
    /// Of each session: the day it last opened (none yet: `i64::MIN`), its high and low since.
    sessions: [(i64, f64, f64); SESSION_PLACES.len()],
}

impl Default for State {
    fn default() -> Self {
        Self {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 0.0,
            day: 0,
            sum: 0.0,
            count: 0,
            sessions: [(i64::MIN, 0.0, 0.0); SESSION_PLACES.len()],
        }
    }
}

const DAY: i64 = 86_400_000;

/// One step of an average that starts as the mean of its first `period` inputs and then smooths with `alpha`: the
/// exponential one (`2 / (period + 1)`) and Wilder's (`1 / period`). `k` counts the inputs from zero. NaN until
/// there are `period` of them.
fn smooth(avg: &mut f64, x: f64, k: usize, period: usize, alpha: f64) -> f64 {
    if k < period {
        *avg += x;
        if k + 1 < period {
            return f64::NAN;
        }
        *avg /= period as f64;
    } else {
        *avg += alpha * (x - *avg);
    }
    *avg
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Works out bar `i` of `study` into `out`, from the state after bar `i - 1`.
fn step(study: &Study, s: &mut State, v: &CandleView<'_>, i: usize, out: &mut [Vec<f64>]) {
    let (close, high, low) = (v.close(), v.high(), v.low());
    let c = close[i];
    match study.kind {
        Kind::Sma => {
            let n = study.whole(0);
            s.a += c;
            if i >= n {
                s.a -= close[i - n];
            }
            if i + 1 >= n {
                out[0][i] = s.a / n as f64;
            }
        }
        Kind::Ema => {
            let n = study.whole(0);
            out[0][i] = smooth(&mut s.a, c, i, n, 2.0 / (n as f64 + 1.0));
        }
        Kind::Vwap => {
            // From the start of each day, in UTC.
            let day = v.time()[i].0.div_euclid(DAY);
            if i == 0 || day != s.day {
                (s.a, s.b, s.day) = (0.0, 0.0, day);
            }
            let typical = (high[i] + low[i] + c) / 3.0;
            let volume = v.volume()[i];
            s.a += typical * volume;
            s.b += volume;
            out[0][i] = if s.b > 0.0 { s.a / s.b } else { typical };
        }
        Kind::Bollinger => {
            let n = study.whole(0);
            if i + 1 >= n {
                let window = &close[i + 1 - n..=i];
                let m = mean(window);
                let variance = window.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n as f64;
                let d = study.params[1] * variance.sqrt();
                (out[0][i], out[1][i], out[2][i]) = (m, m + d, m - d);
            }
        }
        Kind::Rsi => {
            if i == 0 {
                return;
            }
            let n = study.whole(0);
            let change = c - close[i - 1];
            let alpha = 1.0 / n as f64;
            let gain = smooth(&mut s.a, change.max(0.0), i - 1, n, alpha);
            let loss = smooth(&mut s.b, (-change).max(0.0), i - 1, n, alpha);
            if !gain.is_nan() {
                out[0][i] = match (gain, loss) {
                    (0.0, 0.0) => 50.0,
                    (_, 0.0) => 100.0,
                    (g, l) => 100.0 - 100.0 / (1.0 + g / l),
                };
            }
        }
        Kind::Macd => {
            let (fast, slow, signal) = (study.whole(0), study.whole(1), study.whole(2));
            let ema = |n: usize| 2.0 / (n as f64 + 1.0);
            let f = smooth(&mut s.a, c, i, fast, ema(fast));
            let sl = smooth(&mut s.b, c, i, slow, ema(slow));
            if f.is_nan() || sl.is_nan() {
                return;
            }
            let macd = f - sl;
            out[0][i] = macd;
            let first = fast.max(slow) - 1;
            let sig = smooth(&mut s.c, macd, i - first, signal, ema(signal));
            if !sig.is_nan() {
                (out[1][i], out[2][i]) = (sig, macd - sig);
            }
        }
        Kind::Atr => {
            let n = study.whole(0);
            let range = high[i] - low[i];
            let tr = match i {
                0 => range,
                _ => range
                    .max((high[i] - close[i - 1]).abs())
                    .max((low[i] - close[i - 1]).abs()),
            };
            out[0][i] = smooth(&mut s.a, tr, i, n, 1.0 / n as f64);
        }
        Kind::Wma => {
            // The sum of the window and the sum weighted 1 (oldest) to n (newest): moving on a bar takes every
            // weight down by one, which drops the oldest, and puts the new close in at n.
            let n = study.whole(0);
            if i < n {
                (s.a, s.b) = (s.a + c, s.b + (i + 1) as f64 * c);
            } else {
                s.b += n as f64 * c - s.a;
                s.a += c - close[i - n];
            }
            if i + 1 >= n {
                out[0][i] = s.b / (n * (n + 1) / 2) as f64;
            }
        }
        Kind::Donchian => {
            let n = study.whole(0);
            if i + 1 >= n {
                let highest = high[i + 1 - n..=i].iter().fold(f64::MIN, |a, &b| a.max(b));
                let lowest = low[i + 1 - n..=i].iter().fold(f64::MAX, |a, &b| a.min(b));
                (out[0][i], out[1][i], out[2][i]) = ((highest + lowest) / 2.0, highest, lowest);
            }
        }
        Kind::Obv => {
            if i > 0 {
                let volume = v.volume()[i];
                s.a += match c.partial_cmp(&close[i - 1]) {
                    Some(std::cmp::Ordering::Greater) => volume,
                    Some(std::cmp::Ordering::Less) => -volume,
                    _ => 0.0,
                };
            }
            out[0][i] = s.a;
        }
        Kind::Cci => {
            let n = study.whole(0);
            if i + 1 >= n {
                let typical: Vec<f64> = (i + 1 - n..=i).map(|j| (high[j] + low[j] + close[j]) / 3.0).collect();
                let m = mean(&typical);
                let deviation = typical.iter().map(|t| (t - m).abs()).sum::<f64>() / n as f64;
                out[0][i] = if deviation > 0.0 {
                    (typical[n - 1] - m) / (0.015 * deviation)
                } else {
                    0.0
                };
            }
        }
        Kind::Mfi => {
            // Money in on bars whose typical price rose, out on those where it fell, over the last n changes.
            let n = study.whole(0);
            if i < n {
                return;
            }
            let typical = |j: usize| (high[j] + low[j] + close[j]) / 3.0;
            let (mut up, mut down) = (0.0, 0.0);
            for j in i + 1 - n..=i {
                let flow = typical(j) * v.volume()[j];
                if typical(j) > typical(j - 1) {
                    up += flow;
                } else if typical(j) < typical(j - 1) {
                    down += flow;
                }
            }
            out[0][i] = match (up, down) {
                (0.0, 0.0) => 50.0,
                (_, 0.0) => 100.0,
                (u, d) => 100.0 - 100.0 / (1.0 + u / d),
            };
        }
        Kind::Ichimoku => {
            // The middles of the highest high and the lowest low over three lengths; the leading spans are
            // drawn `base - 1` bars ahead and the close as many behind (see `shift`), worked out here at the bar
            // they are made from.
            let middle = |n: usize| {
                (i + 1 >= n).then(|| {
                    let highest = high[i + 1 - n..=i].iter().fold(f64::MIN, |a, &b| a.max(b));
                    let lowest = low[i + 1 - n..=i].iter().fold(f64::MAX, |a, &b| a.min(b));
                    (highest + lowest) / 2.0
                })
            };
            let (conversion, base) = (middle(study.whole(0)), middle(study.whole(1)));
            if let Some(v) = conversion {
                out[0][i] = v;
            }
            if let Some(v) = base {
                out[1][i] = v;
            }
            if let (Some(a), Some(b)) = (conversion, base) {
                out[2][i] = (a + b) / 2.0;
            }
            if let Some(v) = middle(study.whole(2)) {
                out[3][i] = v;
            }
            out[4][i] = c;
        }
        Kind::Volume => {
            let (n, volume) = (study.whole(0), v.volume());
            s.a += volume[i];
            if i >= n {
                s.a -= volume[i - n];
            }
            out[1][i] = volume[i];
            if i + 1 >= n {
                out[0][i] = s.a / n as f64;
            }
        }
        Kind::Adx => {
            // Wilder's: the true range and the moves up and down smoothed over `n`, their ratios the DIs, the
            // ADX the smoothed spread of the DIs.
            if i == 0 {
                return;
            }
            let (n, m) = (study.whole(0), study.whole(1));
            let range = (high[i] - low[i])
                .max((high[i] - close[i - 1]).abs())
                .max((low[i] - close[i - 1]).abs());
            let (up, down) = (high[i] - high[i - 1], low[i - 1] - low[i]);
            let plus = if up > down && up > 0.0 { up } else { 0.0 };
            let minus = if down > up && down > 0.0 { down } else { 0.0 };
            let alpha = 1.0 / n as f64;
            let tr = smooth(&mut s.a, range, i - 1, n, alpha);
            let plus = smooth(&mut s.b, plus, i - 1, n, alpha);
            let minus = smooth(&mut s.c, minus, i - 1, n, alpha);
            if tr.is_nan() {
                return;
            }
            let (plus, minus) = if tr > 0.0 {
                (100.0 * plus / tr, 100.0 * minus / tr)
            } else {
                (0.0, 0.0)
            };
            (out[1][i], out[2][i]) = (plus, minus);
            let dx = if plus + minus > 0.0 {
                100.0 * (plus - minus).abs() / (plus + minus)
            } else {
                0.0
            };
            out[0][i] = smooth(&mut s.d, dx, i - n, m, 1.0 / m as f64);
        }
        Kind::SuperTrend => {
            // Bands `factor` ATRs around the middle of the bar that only tighten while the close stays inside;
            // the line is the lower one while the trend is up, the upper while down, and flips when the close
            // crosses it. `d` keeps the side: 1 up, -1 down, 0 not yet.
            let n = study.whole(0);
            let range = match i {
                0 => high[i] - low[i],
                _ => (high[i] - low[i])
                    .max((high[i] - close[i - 1]).abs())
                    .max((low[i] - close[i - 1]).abs()),
            };
            let atr = smooth(&mut s.a, range, i, n, 1.0 / n as f64);
            if atr.is_nan() {
                return;
            }
            let middle = (high[i] + low[i]) / 2.0;
            let (mut lower, mut upper) = (middle - study.params[1] * atr, middle + study.params[1] * atr);
            if s.d != 0.0 {
                if close[i - 1] >= s.b {
                    lower = lower.max(s.b);
                }
                if close[i - 1] <= s.c {
                    upper = upper.min(s.c);
                }
            }
            s.d = match s.d {
                0.0 => -1.0,
                d if d > 0.0 && c < lower => -1.0,
                d if d < 0.0 && c > upper => 1.0,
                d => d,
            };
            (s.b, s.c) = (lower, upper);
            (out[0][i], out[1][i]) = (if s.d > 0.0 { lower } else { upper }, s.d);
        }
        Kind::Pivots => {
            // The classic levels of the day before: `a`..`c` its high, low and close, `d`..`f` today's so far.
            let day = v.time()[i].0.div_euclid(DAY);
            if i == 0 || day != s.day {
                if i > 0 {
                    (s.a, s.b, s.c) = (s.d, s.e, close[i - 1]);
                }
                (s.d, s.e, s.day) = (high[i], low[i], day);
            } else {
                (s.d, s.e) = (s.d.max(high[i]), s.e.min(low[i]));
            }
            if s.a == 0.0 && s.b == 0.0 {
                return;
            }
            let (h, l, cl) = (s.a, s.b, s.c);
            let p = (h + l + cl) / 3.0;
            out[0][i] = p;
            (out[1][i], out[2][i]) = (2.0 * p - l, 2.0 * p - h);
            (out[3][i], out[4][i]) = (p + (h - l), p - (h - l));
        }
        Kind::Sessions | Kind::Session => {
            // A bar of a day or longer holds every session: there is nothing to show.
            let t = v.time()[i].0;
            if i > 0 && t - v.time()[i - 1].0 >= DAY {
                return;
            }
            let hour = |j: usize| (study.params[j] * HOUR as f64).round() as i64;
            if study.kind == Kind::Session {
                let hours = (hour(1), hour(2));
                if let Some(range) = session_at(&mut s.sessions[0], study.zone(), hours, t, (high[i], low[i])) {
                    (out[0][i], out[1][i]) = range;
                }
                return;
            }
            for (k, &(_, zone, _)) in SESSION_PLACES.iter().enumerate() {
                let hours = (hour(SESSION_HOURS + 2 * k), hour(SESSION_HOURS + 2 * k + 1));
                if let Some(range) = session_at(&mut s.sessions[k], zone, hours, t, (high[i], low[i])) {
                    (out[2 * k][i], out[2 * k + 1][i]) = range;
                }
            }
        }
        Kind::Stochastic => {
            let (k, smoothing, d) = (study.whole(0), study.whole(1), study.whole(2));
            if i + 1 < k {
                return;
            }
            let from = i + 1 - k;
            let highest = high[from..=i].iter().fold(f64::MIN, |a, &b| a.max(b));
            let lowest = low[from..=i].iter().fold(f64::MAX, |a, &b| a.min(b));
            out[2][i] = if highest > lowest {
                (c - lowest) / (highest - lowest) * 100.0
            } else {
                50.0
            };
            if i + 2 < k + smoothing {
                return;
            }
            out[0][i] = mean(&out[2][i + 1 - smoothing..=i]);
            if i + 3 < k + smoothing + d {
                return;
            }
            out[1][i] = mean(&out[0][i + 1 - d..=i]);
        }
    }
}

/// Whether a session open over `hours` (from its open to its close, in ms after midnight on `zone`'s clock) is
/// open at `t`: then the high and low since it opened, with the bar's `range` in, kept in `slot` (the day it last
/// opened, its high and low). One that closes before it opens runs over midnight: the hours after it belong to the
/// session that opened the day before.
fn session_at(
    slot: &mut (i64, f64, f64),
    zone: Zone,
    (open, close): (i64, i64),
    t: i64,
    (high, low): (f64, f64),
) -> Option<(f64, f64)> {
    let local = t + zone.offset(t);
    let (mut day, at) = (local.div_euclid(DAY), local.rem_euclid(DAY));
    let open_now = if open <= close {
        (open..close).contains(&at)
    } else {
        day -= i64::from(at < close);
        at >= open || at < close
    };
    if !open_now {
        return None;
    }
    *slot = if slot.0 == day {
        (day, slot.1.max(high), slot.2.min(low))
    } else {
        (day, high, low)
    };
    Some((slot.1, slot.2))
}

/// Works out bar `i` of the average of the first line of `study`, into its last column.
fn average_step(study: &Study, s: &mut State, i: usize, out: &mut [Vec<f64>]) {
    let n = study.average as usize;
    let line = &out[0];
    if line[i].is_finite() {
        (s.sum, s.count) = (s.sum + line[i], s.count + 1);
    }
    if i >= n && line[i - n].is_finite() {
        (s.sum, s.count) = (s.sum - line[i - n], s.count - 1);
    }
    if s.count == n {
        let last = out.len() - 1;
        out[last][i] = s.sum / n as f64;
    }
}

/// What a study worked out, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// One series per column, as long as the bars, NaN where there is no value.
    pub(crate) out: Vec<Vec<f64>>,
    /// The state after the bar before the last: the last bar may still change, and is worked out again from here.
    before_last: State,
    /// The times of the first two bars and the last one worked out: the bars are the same ones while these are.
    key: [Option<Time>; 3],
    len: usize,
}

impl Cache {
    /// Works out the bars of `view` it has not yet: only the last one again while the bars are the same ones.
    pub(crate) fn update(&mut self, study: &Study, view: CandleView<'_>) {
        let (n, t) = (view.len(), view.time());
        let same = self.len > 0
            && n >= self.len
            && self.key == [t.first().copied(), t.get(1).copied(), t.get(self.len - 1).copied()];
        let start = if same { self.len - 1 } else { 0 };
        if !same {
            self.before_last = State::default();
            self.out = vec![Vec::new(); study.columns()];
        }
        let mut state = self.before_last;
        for column in &mut self.out {
            column.truncate(start);
            column.resize(n, f64::NAN);
        }
        for i in start..n {
            if i + 1 == n {
                self.before_last = state;
            }
            step(study, &mut state, &view, i, &mut self.out);
            if study.average > 0.0 {
                average_step(study, &mut state, i, &mut self.out);
            }
        }
        self.len = n;
        self.key = [t.first().copied(), t.get(1).copied(), t.last().copied()];
    }
}

/// All of a study's values over `view`, one series per column: what the layer draws.
pub fn compute(study: &Study, view: CandleView<'_>) -> Vec<Vec<f64>> {
    let mut cache = Cache::default();
    cache.update(study, view);
    cache.out
}

/// Draws one study. The values are worked out here, where the layers that draw the study share them.
struct StudyLayer {
    study: Study,
    cache: Rc<RefCell<Cache>>,
}

/// The name and values of a study, drawn over everything in the pane.
struct StudyLabel {
    study: Study,
    /// Its place in the list of indicators.
    index: usize,
    /// Which line of labels under the legend it has, if it is over the candles.
    row: usize,
    cache: Rc<RefCell<Cache>>,
}

impl Study {
    /// How many series it works out: those of its kind, and its average.
    fn columns(&self) -> usize {
        self.kind.columns() + usize::from(self.average > 0.0)
    }

    /// The lines it draws: column and color.
    fn lines(&self) -> Vec<(usize, Color)> {
        let mut lines = self.kind_lines();
        if self.average > 0.0 {
            lines.push((self.kind.columns(), AVERAGED));
        }
        lines
    }

    fn kind_lines(&self) -> Vec<(usize, Color)> {
        let color = self.color;
        match self.kind {
            Kind::Bollinger | Kind::Donchian => vec![
                (0, color),
                (1, Color { a: 200, ..color }),
                (2, Color { a: 200, ..color }),
            ],
            Kind::Macd | Kind::Stochastic => vec![(0, color), (1, SECOND)],
            Kind::Adx => vec![(0, color), (1, RISING), (2, FALLING)],
            Kind::Ichimoku => vec![
                (0, color),
                (1, Color::rgb(183, 28, 28)),
                (2, Color { a: 200, ..RISING }),
                (3, Color { a: 200, ..FALLING }),
                (4, Color::rgb(67, 160, 71)),
            ],
            Kind::Pivots => vec![
                (0, color),
                (1, FALLING),
                (2, RISING),
                (3, Color { a: 160, ..FALLING }),
                (4, Color { a: 160, ..RISING }),
            ],
            // The sessions are zones in colors of their own, named on the chart.
            Kind::Sessions | Kind::Session => Vec::new(),
            _ => vec![(0, color)],
        }
    }

    /// How many bars after the one it is worked out at a value is drawn: Ichimoku's leading spans ahead, its
    /// lagging close behind.
    fn shift(&self, column: usize) -> isize {
        let ahead = self.whole(1) as isize - 1;
        match (self.kind, column) {
            (Kind::Ichimoku, 2 | 3) => ahead,
            (Kind::Ichimoku, 4) => -ahead,
            _ => 0,
        }
    }

    /// The value of `column` drawn at bar `i`.
    fn drawn_at(&self, out: &[Vec<f64>], column: usize, i: usize) -> f64 {
        let from = i as isize - self.shift(column);
        let values = &out[column];
        usize::try_from(from)
            .ok()
            .and_then(|j| values.get(j))
            .copied()
            .unwrap_or(f64::NAN)
    }

    fn histogram(&self) -> Option<usize> {
        match self.kind {
            Kind::Macd => Some(2),
            Kind::Volume => Some(1),
            _ => None,
        }
    }

    /// The color of bar `i` of the histogram `values`: volume in the color of its candle; MACD's by its sign,
    /// paler while it shrinks towards zero.
    fn bar_color(&self, ctx: &DrawCtx<'_>, values: &[f64], i: usize) -> Color {
        let candle = &ctx.style.candle;
        if self.kind == Kind::Volume {
            let rising = ctx.view.close()[i] >= ctx.view.open()[i];
            return Color {
                a: 130,
                ..if rising { candle.up } else { candle.down }
            };
        }
        let v = values[i];
        let before = if i > 0 { values[i - 1] } else { f64::NAN };
        let growing = !before.is_finite() || v.abs() >= before.abs();
        Color {
            a: if growing { 220 } else { 110 },
            ..if v >= 0.0 { candle.up } else { candle.down }
        }
    }
}

/// Indices around `visible`, a bar more on each side so lines run to the edges of the plot.
fn around(visible: &Range<usize>, len: usize) -> Range<usize> {
    visible.start.saturating_sub(1)..(visible.end + 1).min(len)
}

impl Layer for StudyLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if self.study.hidden {
            return;
        }
        let view = ctx.view;
        let mut cache = self.cache.borrow_mut();
        cache.update(&self.study, view);
        let out = &cache.out;
        let kind = self.study.kind;
        let style = ctx.style;
        let (plot_w, _) = ctx.plot;
        let unit = ctx.line();
        let y_of = |v: f64| ctx.price.y_of(v) as f32;

        // The levels of an oscillator, and the band between the outer ones.
        let levels = kind.levels();
        if levels.len() >= 2 && !view.is_empty() {
            let (top, bottom) = (y_of(levels[levels.len() - 1]), y_of(levels[0]));
            let band = Color {
                a: 20,
                ..self.study.color
            };
            ctx.scene.fill(0.0, top, plot_w, bottom - top, band);
        }
        for &level in levels {
            let y = y_of(level).floor();
            let color = Color {
                a: 110,
                ..style.axis_text
            };
            let (on, off) = (ctx.px(4.0), ctx.px(4.0));
            let mut x = 0.0;
            while x < plot_w {
                ctx.scene.fill(x, y, on.min(plot_w - x), unit, color);
                x += on + off;
            }
        }
        if view.is_empty() {
            return;
        }
        if matches!(kind, Kind::Sessions | Kind::Session) {
            draw_sessions(ctx, &self.study, out);
            return;
        }

        let range = around(&ctx.visible, view.len());
        let stride = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let x_of = |i: isize| ctx.time.x_of(i as f64 + 0.5) as f32;
        // The bars on screen, a bar more on each side, also those past the newest (a shifted line runs there).
        let (first, last) = (
            ctx.time.index_of(0.0).floor() as isize - 1,
            ctx.time.index_of(plot_w as f64).ceil() as isize + 1,
        );
        let points = |column: usize| -> Vec<Option<(f32, f32)>> {
            let values = &out[column];
            let shift = self.study.shift(column);
            let from = if shift == 0 {
                range.start
            } else {
                (first - shift).max(0) as usize
            };
            let to = if shift == 0 {
                range.end
            } else {
                ((last - shift).max(0) as usize).min(values.len())
            };
            (from..to.max(from))
                .step_by(stride)
                .map(|i| {
                    let v = values[i];
                    v.is_finite().then(|| (x_of(i as isize + shift), y_of(v)))
                })
                .collect()
        };

        // The bands of Bollinger's and Donchian's are filled between, a slice per device pixel, and so is
        // Ichimoku's cloud: green where span A is over span B, red where under.
        let band = match kind {
            Kind::Bollinger | Kind::Donchian => Some((1, 2)),
            Kind::Ichimoku => Some((2, 3)),
            _ => None,
        };
        if let Some((a, b)) = band {
            let (upper, lower) = (points(a), points(b));
            for i in 1..upper.len().min(lower.len()) {
                let (Some(u0), Some(u1), Some(l0), Some(l1)) = (upper[i - 1], upper[i], lower[i - 1], lower[i]) else {
                    continue;
                };
                let color = match kind {
                    Kind::Ichimoku if u1.1 <= l1.1 => Color { a: 36, ..RISING },
                    Kind::Ichimoku => Color { a: 36, ..FALLING },
                    _ => Color {
                        a: 20,
                        ..self.study.color
                    },
                };
                let slices = (u1.0 - u0.0).ceil().max(1.0) as usize;
                let w = (u1.0 - u0.0) / slices as f32;
                for s in 0..slices {
                    let t = (s as f32 + 0.5) / slices as f32;
                    let top = u0.1 + (u1.1 - u0.1) * t;
                    let bottom = l0.1 + (l1.1 - l0.1) * t;
                    let (top, bottom) = (top.min(bottom), top.max(bottom));
                    ctx.scene.fill(u0.0 + w * s as f32, top, w, bottom - top, color);
                }
            }
        }

        if let Some(column) = self.study.histogram() {
            let values = &out[column];
            let zero = y_of(0.0);
            let w = (ctx.time.bar_spacing as f32 * 0.6).max(unit);
            for i in range.clone().step_by(stride) {
                let v = values[i];
                if !v.is_finite() {
                    continue;
                }
                let color = self.study.bar_color(ctx, values, i);
                let y = y_of(v);
                ctx.scene.fill(
                    x_of(i as isize) - w / 2.0,
                    y.min(zero),
                    w,
                    (y - zero).abs().max(unit),
                    color,
                );
            }
        }

        let width = ctx.px(self.study.width);
        let (dash, scale) = (self.study.dash, ctx.px(1.0));
        if kind == Kind::SuperTrend {
            // In the color of its side, broken where it flips.
            let side: Vec<f64> = range.clone().step_by(stride).map(|i| out[1][i]).collect();
            let line = points(0);
            let mut pens = [self.study.color, FALLING].map(|color| Pen::new(dash, width, color, scale));
            for j in 1..line.len() {
                if let (Some(from), Some(to)) = (line[j - 1], line[j])
                    && side[j - 1] == side[j]
                {
                    pens[usize::from(side[j] < 0.0)].line(ctx.scene, (from, to));
                }
            }
            return;
        }
        // Pivots are levels: steps that only join within a day.
        let steps = kind == Kind::Pivots;
        for &(column, color) in &self.study.lines() {
            let mut pen = Pen::new(dash, width, color, scale);
            let mut previous: Option<(f32, f32)> = None;
            for point in points(column) {
                if let (Some(from), Some(to)) = (previous, point)
                    && (!steps || from.1 == to.1)
                {
                    pen.line(ctx.scene, (from, to));
                }
                previous = point;
            }
        }
    }

    fn price_extent(&self, view: CandleView<'_>, visible: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
        if view.is_empty() || self.study.hidden {
            return None;
        }
        if let Some(range) = self.study.kind.range() {
            return Some(range);
        }
        // Levels far away do not squeeze the candles; they show when the prices come near.
        if matches!(self.study.kind, Kind::Pivots | Kind::Sessions | Kind::Session) {
            return None;
        }
        let mut cache = self.cache.borrow_mut();
        cache.update(&self.study, view);
        let mut columns: Vec<usize> = self.study.lines().iter().map(|l| l.0).collect();
        columns.extend(self.study.histogram());
        let mut extent = self.study.histogram().map(|_| (0.0, 0.0));
        for column in columns {
            let shift = self.study.shift(column);
            let len = cache.out[column].len() as isize;
            let from = (visible.start as isize - shift).clamp(0, len) as usize;
            let to = (visible.end as isize - shift).clamp(0, len) as usize;
            for &v in &cache.out[column][from..to.max(from)] {
                if v.is_finite() {
                    extent = Some(extent.map_or((v, v), |(lo, hi): (f64, f64)| (lo.min(v), hi.max(v))));
                }
            }
        }
        extent
    }
}

/// Each session where it was open: a zone from its high to its low, in the look chosen, with its high and low
/// lines and its name if asked. A session still open is drawn as far as it has got. A session of one's own is
/// named by its city, in the indicator's color.
fn draw_sessions(ctx: &mut DrawCtx<'_>, study: &Study, out: &[Vec<f64>]) {
    let (places, look_at) = match study.kind {
        Kind::Session => (vec![(study.zone().label(), study.color)], 3),
        _ => (
            SESSION_PLACES.iter().map(|&(name, _, color)| (name, color)).collect(),
            0,
        ),
    };
    let look = study.params[look_at] as usize;
    let (lines, names) = (study.params[look_at + 1] > 0.0, study.params[look_at + 2] > 0.0);
    let n = ctx.view.len();
    let half = (ctx.time.bar_spacing / 2.0) as f32;
    let unit = ctx.line();
    let size = ctx.px(ctx.style.font_size * 0.9);
    for (k, &(name, color)) in places.iter().enumerate() {
        let (high, low) = (&out[2 * k], &out[2 * k + 1]);
        // From the start of a session cut by the left edge to the end of one cut by the right.
        let (mut from, mut to) = (ctx.visible.start.min(n), ctx.visible.end.min(n));
        while from > 0 && high[from - 1].is_finite() {
            from -= 1;
        }
        while to < n && high[to].is_finite() {
            to += 1;
        }
        let mut i = from;
        while i < to {
            if !high[i].is_finite() {
                i += 1;
                continue;
            }
            let start = i;
            while i < to && high[i].is_finite() {
                i += 1;
            }
            // The high and low at its last bar are those of all of it.
            let (left, right) = (
                ctx.time.x_of(start as f64 + 0.5) as f32 - half,
                ctx.time.x_of(i as f64 - 0.5) as f32 + half,
            );
            let (top, bottom) = (ctx.price.y_of(high[i - 1]) as f32, ctx.price.y_of(low[i - 1]) as f32);
            let (w, h) = (right - left, (bottom - top).max(unit));
            match look {
                0 => {
                    ctx.scene.fill(left, top, w, h, Color { a: 26, ..color });
                    ctx.scene
                        .stroke_rounded(left, top, w, h, 0.0, unit, Color { a: 150, ..color });
                }
                1 => {
                    let feather = (w.min(h) * 0.35).min(ctx.px(28.0));
                    ctx.scene
                        .fill_soft(left, top, w, h, 0.0, feather, Color { a: 44, ..color });
                }
                _ => {
                    let shades = (Color { a: 60, ..color }, Color { a: 4, ..color });
                    ctx.scene
                        .fill_gradient(left, top, w, h, shades, crate::Direction::Right);
                }
            }
            if lines {
                for y in [top, bottom - unit] {
                    ctx.scene.fill(left, y.floor(), w, unit, Color { a: 210, ..color });
                }
            }
            if names {
                // Over the zone, or just inside it when there is no room above.
                let y = if top - size > 0.0 {
                    top - size * 0.7
                } else {
                    top + size * 0.7
                };
                ctx.scene.text(
                    left + ctx.px(2.0),
                    y,
                    size,
                    Color { a: 230, ..color },
                    Align::Left,
                    name,
                );
            }
        }
    }
}

impl Layer for StudyLabel {
    /// The name and the values at the bar under the pointer, or at the newest bar: over the candles under the
    /// legend, in a pane of its own at its top left.
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let view = &ctx.view;
        if view.is_empty() {
            return;
        }
        let mut cache = self.cache.borrow_mut();
        cache.update(&self.study, *view);
        let out = &cache.out;
        let pointed = ctx
            .chart_pointer()
            .filter(|&(x, y)| (0.0..ctx.plot.0).contains(&x) && (0.0..ctx.pane.chart_plot.1).contains(&y))
            .map(|(x, _)| ctx.time.index_of(x as f64).floor())
            .filter(|&i| i >= 0.0 && (i as usize) < view.len())
            .map(|i| i as usize);
        let i = pointed.unwrap_or(view.len() - 1);
        let style = ctx.style;
        let fixed = self.study.kind.range().is_some() || matches!(self.study.kind, Kind::Adx | Kind::Cci);
        let mut parts = vec![(self.study.label(), style.axis_text)];
        let mut shown: Vec<(usize, Color)> = self.study.lines();
        if let Some(h) = self.study.histogram() {
            shown.insert(0, (h, self.study.bar_color(ctx, &out[h], i)));
        }
        for (column, mut color) in shown {
            if self.study.kind == Kind::SuperTrend && out[1][i] < 0.0 {
                color = FALLING;
            }
            let v = self.study.drawn_at(out, column, i);
            let text = if !v.is_finite() {
                "-".to_string()
            } else if fixed {
                format!("{v:.2}")
            } else {
                let d = decimals(v.abs());
                format!("{v:.d$}")
            };
            parts.push((text, Color { a: 255, ..color }));
        }
        // A hidden one shows its name only, paler; the one under the pointer shows its buttons after the name.
        if self.study.hidden {
            parts.truncate(1);
            parts[0].1 = Color {
                a: 110,
                ..style.axis_text
            };
        }
        let hover = ctx
            .res
            .get::<LabelHover>()
            .and_then(|h| h.0)
            .filter(|h| h.0 == self.index);
        let font = Font::ui();
        let size = ctx.px(style.font_size);
        let gap = ctx.px(8.0);
        let spot = label_spot(&self.study, self.row, ctx.scale, style.font_size);
        let (x, y, h) = (spot.x, spot.y, spot.h);
        let button = ctx.px(LABEL_BUTTON);
        let mut end = match hover {
            Some(_) => spot.buttons + LABEL_BUTTONS.len() as f32 * button,
            None => x + font.measure(&parts[0].0, size),
        };
        let values: Vec<(f32, String, Color)> = parts
            .drain(1..)
            .map(|(text, color)| {
                let at = end + gap;
                end = at + font.measure(&text, size);
                (at, text, color)
            })
            .collect();
        let pad = ctx.px(4.0);
        let backdrop = Color {
            a: 220,
            ..style.background
        };
        ctx.scene
            .fill_rounded(x - pad, y - h / 2.0, end - x + 2.0 * pad, h, pad, backdrop);
        let (name, color) = &parts[0];
        ctx.scene.text(x, y, size, *color, Align::Left, name);
        if let Some((_, under)) = hover {
            for (k, b) in LABEL_BUTTONS.into_iter().enumerate() {
                let left = spot.buttons + k as f32 * button;
                if under == Some(b) {
                    let lit = Color {
                        a: 40,
                        ..style.axis_text
                    };
                    ctx.scene
                        .fill_rounded(left + 1.0, y - button / 2.0 + 1.0, button - 2.0, button - 2.0, pad, lit);
                }
                let center = (left + button / 2.0, y);
                button_icon(ctx.scene, b, center, button * 0.75, style.axis_text, self.study.hidden);
            }
        }
        for (at, text, color) in values {
            ctx.scene.text(at, y, size, color, Align::Left, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles, calendar};

    fn bars(values: &[(f64, f64, f64)]) -> Candles {
        let mut c = Candles::new();
        for (i, &(high, low, close)) in values.iter().enumerate() {
            c.push(Candle {
                time: Time(i as i64 * 60_000),
                open: close,
                high,
                low,
                close,
                volume: 1.0,
            })
            .unwrap();
        }
        c
    }

    fn closes(values: &[f64]) -> Candles {
        bars(&values.iter().map(|&v| (v, v, v)).collect::<Vec<_>>())
    }

    fn with(kind: Kind, params: &[f64]) -> Study {
        let mut s = Study::new(kind);
        for (i, &p) in params.iter().enumerate() {
            s.set(i, p);
        }
        s
    }

    fn defined(column: &[f64]) -> Vec<(usize, f64)> {
        column
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, &v)| (i, v))
            .collect()
    }

    #[test]
    fn moving_averages() {
        let data = closes(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let sma = compute(&with(Kind::Sma, &[3.0]), data.view());
        assert_eq!(defined(&sma[0]), [(2, 2.0), (3, 3.0), (4, 4.0)]);
        // Seeded with the mean of the first three, then halfway to each new close.
        let ema = compute(&with(Kind::Ema, &[3.0]), data.view());
        assert_eq!(defined(&ema[0]), [(2, 2.0), (3, 3.0), (4, 4.0)]);
        let data = closes(&[2.0, 4.0, 6.0, 12.0]);
        let ema = compute(&with(Kind::Ema, &[3.0]), data.view());
        assert_eq!(defined(&ema[0]), [(2, 4.0), (3, 8.0)]);
    }

    #[test]
    fn rsi_matches_wilders_worked_example() {
        let data = closes(&[
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03, 45.61, 46.28, 46.28,
            46.00, 46.03, 46.41, 46.22, 45.64,
        ]);
        let rsi = defined(&compute(&with(Kind::Rsi, &[14.0]), data.view())[0]);
        assert_eq!(rsi.iter().map(|x| x.0).collect::<Vec<_>>(), [14, 15, 16, 17, 18, 19]);
        for (got, want) in rsi.iter().map(|x| x.1).zip([70.53, 66.32, 66.55, 69.41, 66.36, 57.97]) {
            assert!((got - want).abs() < 0.2, "{got} vs {want}");
        }
    }

    #[test]
    fn an_average_of_the_rsi_is_drawn_with_it() {
        let data = closes(
            &(0..40)
                .map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0)
                .collect::<Vec<_>>(),
        );
        let mut rsi = with(Kind::Rsi, &[14.0]);
        assert_eq!(rsi.settings().len(), 2, "its length, then the average");
        rsi.set(1, 3.4);
        assert_eq!((rsi.average, rsi.setting(1)), (3.0, 3.0));
        let out = compute(&rsi, data.view());
        assert_eq!(out.len(), 2);
        let averaged = defined(&out[1]);
        assert_eq!(averaged[0].0, 16, "from the third RSI value on");
        for &(i, v) in &averaged {
            assert!((v - mean(&out[0][i - 2..=i])).abs() < 1e-9);
        }
        assert_eq!(rsi.lines().last(), Some(&(1, AVERAGED)));
        assert_eq!(rsi.save(), "rsi 14 color=aa82ff average=3");
        assert_eq!(Study::parse("rsi 14 average=3"), Some(rsi));
        // Not every indicator has one.
        let mut sma = with(Kind::Sma, &[20.0]);
        sma.set(1, 5.0);
        assert_eq!((sma.settings().len(), sma.average), (1, 0.0));
    }

    #[test]
    fn bollinger_bands_are_the_mean_and_deviations_of_the_window() {
        let data = closes(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        let bb = compute(&with(Kind::Bollinger, &[8.0, 2.0]), data.view());
        // Mean 5, population deviation 2.
        assert_eq!((bb[0][7], bb[1][7], bb[2][7]), (5.0, 9.0, 1.0));
        assert!(bb[0][6].is_nan());
    }

    #[test]
    fn macd_is_the_gap_of_two_averages_and_its_own_average() {
        let prices: Vec<f64> = (0..60).map(|i| 100.0 + i as f64).collect();
        let data = closes(&prices);
        let m = compute(&with(Kind::Macd, &[3.0, 6.0, 4.0]), data.view());
        // On a straight line the averages lag by (n - 1) / 2 bars: 1 and 2.5.
        assert!((m[0][59] - 1.5).abs() < 1e-9, "{}", m[0][59]);
        assert!((m[1][59] - 1.5).abs() < 1e-9 && m[2][59].abs() < 1e-9);
        assert_eq!(defined(&m[0])[0].0, 5);
        assert_eq!(defined(&m[1])[0].0, 8);
    }

    #[test]
    fn atr_and_stochastic() {
        let data = bars(&[
            (11.0, 8.0, 10.0),
            (12.0, 10.0, 11.0),
            (15.0, 11.0, 14.0),
            (14.0, 12.0, 13.0),
        ]);
        let atr = compute(&with(Kind::Atr, &[2.0]), data.view());
        // True ranges 3, 2, 4, 2: seeded with their mean, then halfway to each.
        assert_eq!(defined(&atr[0]), [(1, 2.5), (2, 3.25), (3, 2.625)]);
        let st = compute(&with(Kind::Stochastic, &[2.0, 1.0, 2.0]), data.view());
        // Bar 1: from the low 8 to the high 12, at 11. Bar 2: from 10 to 15, at 14. Bar 3: from 11 to 15, at 13.
        assert_eq!(defined(&st[0]), [(1, 75.0), (2, 80.0), (3, 50.0)]);
        assert_eq!(defined(&st[1]), [(2, 77.5), (3, 65.0)]);
    }

    #[test]
    fn weighted_average_and_donchian_channels() {
        let wma = compute(&with(Kind::Wma, &[3.0]), closes(&[1.0, 2.0, 3.0, 4.0, 5.0]).view());
        // Weights 1, 2, 3 from the oldest: (1 + 4 + 9) / 6, then (2 + 6 + 12) / 6...
        assert_eq!(defined(&wma[0]), [(2, 14.0 / 6.0), (3, 20.0 / 6.0), (4, 26.0 / 6.0)]);
        let data = bars(&[
            (11.0, 8.0, 10.0),
            (12.0, 10.0, 11.0),
            (15.0, 11.0, 14.0),
            (14.0, 12.0, 13.0),
        ]);
        let dc = compute(&with(Kind::Donchian, &[2.0]), data.view());
        assert_eq!(defined(&dc[1]), [(1, 12.0), (2, 15.0), (3, 15.0)]);
        assert_eq!(defined(&dc[2]), [(1, 8.0), (2, 10.0), (3, 11.0)]);
        assert_eq!(defined(&dc[0]), [(1, 10.0), (2, 12.5), (3, 13.0)]);
    }

    #[test]
    fn volume_and_typical_price_oscillators() {
        let obv = compute(&Study::new(Kind::Obv), closes(&[10.0, 11.0, 11.0, 9.0]).view());
        assert_eq!(defined(&obv[0]), [(0, 0.0), (1, 1.0), (2, 1.0), (3, 0.0)]);
        let data = bars(&[
            (11.0, 8.0, 10.0),
            (12.0, 10.0, 11.0),
            (15.0, 11.0, 14.0),
            (14.0, 12.0, 13.0),
        ]);
        // Over two bars the typical price is always as far above or below their mean as it is from the other.
        let cci = defined(&compute(&with(Kind::Cci, &[2.0]), data.view())[0]);
        let signs: Vec<_> = cci.iter().map(|&(i, v)| (i, (v * 100.0).round() / 100.0)).collect();
        assert_eq!(signs, [(1, 66.67), (2, 66.67), (3, -66.67)]);
        // Typical prices 29/3, 11, 40/3, 13: two rises, then a rise of 40/3 against a fall of 13.
        let mfi = defined(&compute(&with(Kind::Mfi, &[2.0]), data.view())[0]);
        assert_eq!(mfi[0], (2, 100.0));
        assert_eq!(mfi[1].0, 3);
        assert!((mfi[1].1 - (100.0 - 100.0 / (1.0 + (40.0 / 3.0) / 13.0))).abs() < 1e-9);
    }

    #[test]
    fn directional_movement_and_supertrend() {
        let data = bars(&[
            (11.0, 9.0, 10.0),
            (13.0, 11.0, 12.0),
            (15.0, 13.0, 14.0),
            (12.0, 8.0, 9.0),
        ]);
        let adx = compute(&with(Kind::Adx, &[1.0, 2.0]), data.view());
        // True ranges 3, 3, 6; moves up 2, 2, none; down none, none, 5.
        assert_eq!(defined(&adx[1]), [(1, 200.0 / 3.0), (2, 200.0 / 3.0), (3, 0.0)]);
        assert_eq!(defined(&adx[2]), [(1, 0.0), (2, 0.0), (3, 500.0 / 6.0)]);
        // The spread is 100 each bar: the mean of the first two, then halfway to the next.
        assert_eq!(defined(&adx[0]), [(2, 100.0), (3, 100.0)]);

        let st = compute(&with(Kind::SuperTrend, &[1.0, 1.0]), data.view());
        // Down at first, on the upper band (12, held at 12 while the close stays under it); up once the close
        // goes over it, on the lower band (11); down again when the close falls under that, on the upper (16).
        assert_eq!(st[0], [12.0, 12.0, 11.0, 16.0]);
        assert_eq!(st[1], [-1.0, -1.0, 1.0, -1.0]);
    }

    #[test]
    fn ichimoku_lines_and_where_they_are_drawn() {
        let data = bars(&[(10.0, 8.0, 9.0), (12.0, 9.0, 11.0), (13.0, 11.0, 12.0)]);
        let study = with(Kind::Ichimoku, &[1.0, 2.0, 3.0]);
        let out = compute(&study, data.view());
        assert_eq!(out[0], [9.0, 10.5, 12.0]);
        assert_eq!(defined(&out[1]), [(1, 10.0), (2, 11.0)]);
        assert_eq!(defined(&out[2]), [(1, 10.25), (2, 11.5)]);
        assert_eq!(defined(&out[3]), [(2, 10.5)]);
        // The leading spans a bar ahead (base 2 - 1), the close a bar behind.
        assert_eq!(study.drawn_at(&out, 2, 3), 11.5, "past the newest bar");
        assert!(study.drawn_at(&out, 2, 1).is_nan());
        assert_eq!(study.drawn_at(&out, 4, 0), 11.0);
        assert!(study.drawn_at(&out, 4, 2).is_nan());
    }

    #[test]
    fn volume_and_its_average() {
        let mut c = Candles::new();
        for (i, volume) in [4.0, 2.0, 6.0].into_iter().enumerate() {
            c.push(Candle {
                time: Time(i as i64 * 60_000),
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume,
            })
            .unwrap();
        }
        let v = compute(&with(Kind::Volume, &[2.0]), c.view());
        assert_eq!(v[1], [4.0, 2.0, 6.0]);
        assert_eq!(defined(&v[0]), [(1, 3.0), (2, 4.0)]);
    }

    #[test]
    fn pivots_are_the_levels_of_the_day_before() {
        let mut c = Candles::new();
        for (t, high, low, close) in [(0, 12.0, 8.0, 10.0), (60_000, 14.0, 9.0, 13.0), (DAY, 13.0, 11.0, 12.0)] {
            c.push(Candle {
                time: Time(t),
                open: close,
                high,
                low,
                close,
                volume: 1.0,
            })
            .unwrap();
        }
        let v = compute(&Study::new(Kind::Pivots), c.view());
        // The first day has none; the second the levels of high 14, low 8, close 13.
        let p = (14.0 + 8.0 + 13.0) / 3.0;
        let at = |column: usize| defined(&v[column]);
        assert_eq!(at(0), [(2, p)]);
        assert_eq!((at(1), at(2)), (vec![(2, 2.0 * p - 8.0)], vec![(2, 2.0 * p - 14.0)]));
        assert_eq!((at(3), at(4)), (vec![(2, p + 6.0)], vec![(2, p - 6.0)]));
    }

    #[test]
    fn sessions_are_the_high_and_low_of_each_since_it_opened() {
        let mut c = Candles::new();
        // An hour a bar from 06:00 to 09:00 UTC, then 00:00 the next day.
        for (hour, high, low) in [
            (6, 10.0, 9.0),
            (7, 12.0, 8.0),
            (8, 11.0, 7.0),
            (9, 13.0, 10.0),
            (24, 5.0, 4.0),
        ] {
            c.push(Candle {
                time: Time(hour * HOUR),
                open: low,
                high,
                low,
                close: high,
                volume: 1.0,
            })
            .unwrap();
        }
        let v = compute(&Study::new(Kind::Sessions), c.view());
        // Asia (09:00 to 18:00 in Tokyo) runs to 09:00 UTC and starts again the next day; London (08:00 to 17:00)
        // opens at 08:00 in winter; New York not yet.
        assert_eq!(defined(&v[0]), [(0, 10.0), (1, 12.0), (2, 12.0), (4, 5.0)]);
        assert_eq!(defined(&v[1]), [(0, 9.0), (1, 8.0), (2, 7.0), (4, 4.0)]);
        assert_eq!(defined(&v[2]), [(2, 11.0), (3, 13.0)]);
        assert_eq!(defined(&v[3]), [(2, 7.0), (3, 7.0)]);
        assert!(defined(&v[4]).is_empty());
        // A bar a day later holds every session: none is shown at it.
        c.push(Candle {
            time: Time(48 * HOUR),
            open: 4.0,
            high: 6.0,
            low: 3.0,
            close: 5.0,
            volume: 1.0,
        })
        .unwrap();
        let v = compute(&Study::new(Kind::Sessions), c.view());
        assert!(v.iter().all(|column| !column[5].is_finite()));
    }

    /// Hourly bars from `from` (UTC, like "2026-07-01 06:00"), with these highs; the lows a point under.
    fn hourly(from: &str, highs: &[f64]) -> Candles {
        let start = calendar::parse_time(from).unwrap().0;
        let mut c = Candles::new();
        for (k, &high) in highs.iter().enumerate() {
            let candle = Candle {
                time: Time(start + k as i64 * HOUR),
                open: high,
                high,
                low: high - 1.0,
                close: high,
                volume: 1.0,
            };
            c.push(candle).unwrap();
        }
        c
    }

    #[test]
    fn session_hours_are_kept_in_their_city_with_its_daylight_saving() {
        let at = |text: &str| calendar::parse_time(text).unwrap().0;
        // 2026: Britain's summer time from 29 March to 25 October, America's from 8 March to 1 November.
        for (t, london, new_york) in [
            ("2026-01-15 12:00", 0, -5),
            ("2026-03-08 06:59", 0, -5),
            ("2026-03-08 07:00", 0, -4),
            ("2026-03-29 00:59", 0, -4),
            ("2026-03-29 01:00", 1, -4),
            ("2026-10-25 00:59", 1, -4),
            ("2026-10-25 01:00", 0, -4),
            ("2026-11-01 05:59", 0, -4),
            ("2026-11-01 06:00", 0, -5),
        ] {
            let offsets = (Zone::London.offset(at(t)) / HOUR, Zone::NewYork.offset(at(t)) / HOUR);
            assert_eq!(offsets, (london, new_york), "{t}");
        }
        assert_eq!(Zone::Tokyo.offset(at("2026-07-01 00:00")), 9 * HOUR);
        // In July London's 08:00 is 07:00 UTC, New York's is 12:00.
        let v = compute(
            &Study::new(Kind::Sessions),
            hourly("2026-07-01 06:00", &[1.0; 7]).view(),
        );
        assert_eq!(defined(&v[2]).first().map(|d| d.0), Some(1));
        assert_eq!(defined(&v[4]).first().map(|d| d.0), Some(6));
    }

    #[test]
    fn a_session_set_over_midnight_is_one_session_and_one_set_to_nothing_is_off() {
        let mut study = Study::new(Kind::Sessions);
        // Asia from 22:00 to 02:00 in Tokyo: 13:00 to 17:00 UTC, its midnight at 15:00.
        study.set(3, 22.0);
        study.set(4, 2.0);
        // London off.
        study.set(5, 9.0);
        study.set(6, 9.0);
        let v = compute(
            &study,
            hourly("2026-01-15 12:00", &[6.0, 5.0, 4.0, 3.0, 2.0, 1.0]).view(),
        );
        assert_eq!(defined(&v[0]), [(1, 5.0), (2, 5.0), (3, 5.0), (4, 5.0)]);
        assert!(defined(&v[2]).is_empty());
        assert_eq!(study.label(), "Sessions");
    }

    #[test]
    fn a_session_of_one_s_own_keeps_its_city_s_hours_through_daylight_saving() {
        let mut study = Study::new(Kind::Session);
        assert_eq!(study.label(), "Session New York 09:30-16:00");
        study.set(1, 9.0);
        // New York's 09:00 to 16:00 is 13:00 to 20:00 UTC in summer, 14:00 to 21:00 in winter.
        let summer = compute(&study, hourly("2026-07-01 12:00", &[1.0; 10]).view());
        assert_eq!(
            defined(&summer[0]).iter().map(|d| d.0).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7]
        );
        let winter = compute(&study, hourly("2026-01-15 12:00", &[1.0; 10]).view());
        assert_eq!(
            defined(&winter[0]).iter().map(|d| d.0).collect::<Vec<_>>(),
            [2, 3, 4, 5, 6, 7, 8]
        );
        // Tokyo from 08:00 to 10:00: 23:00 to 01:00 UTC; the high is that of all of it so far.
        study.set(0, Zone::ALL.iter().position(|&z| z == Zone::Tokyo).unwrap() as f64);
        study.set(1, 8.0);
        study.set(2, 10.0);
        let v = compute(&study, hourly("2026-01-15 22:00", &[3.0, 4.0, 2.0, 5.0]).view());
        assert_eq!(defined(&v[0]), [(1, 4.0), (2, 4.0)]);
        assert_eq!(defined(&v[1]), [(1, 3.0), (2, 1.0)]);
        assert_eq!(study.label(), "Session Tokyo 08:00-10:00");
        assert_eq!(Study::parse(&study.save()), Some(study));
    }

    #[test]
    fn vwap_starts_again_each_day() {
        let mut c = Candles::new();
        for (t, price, volume) in [(0, 10.0, 1.0), (60_000, 20.0, 3.0), (DAY, 30.0, 1.0)] {
            c.push(Candle {
                time: Time(t),
                open: price,
                high: price,
                low: price,
                close: price,
                volume,
            })
            .unwrap();
        }
        let v = compute(&Study::new(Kind::Vwap), c.view());
        assert_eq!(v[0], [10.0, 17.5, 30.0]);
    }

    #[test]
    fn a_changed_last_bar_or_new_bars_give_what_a_fresh_start_gives() {
        let prices: Vec<f64> = (0..300).map(|i| 100.0 + (i as f64 * 0.37).sin() * 5.0).collect();
        for kind in Kind::ALL {
            let mut study = Study::new(kind);
            // With an average where there can be one.
            study.set(kind.params().len(), 9.0);
            let mut cache = Cache::default();
            let mut data = closes(&prices[..200]);
            cache.update(&study, data.view());
            // The forming bar changes, then more come.
            data.upsert(Candle {
                close: 90.0,
                ..data.view().get(199).unwrap()
            })
            .unwrap();
            cache.update(&study, data.view());
            for (i, &price) in prices.iter().enumerate().skip(200) {
                data.push(Candle {
                    time: Time(i as i64 * 60_000),
                    ..closes(&[price]).view().get(0).unwrap()
                })
                .unwrap();
                cache.update(&study, data.view());
            }
            let fresh = compute(&study, data.view());
            for (a, b) in cache.out.iter().zip(&fresh) {
                assert_eq!(defined(a), defined(b), "{kind:?}");
            }
            // Fewer bars (a replay going back) start again.
            let shorter = closes(&prices[..50]);
            cache.update(&study, shorter.view());
            assert_eq!(cache.out[0].len(), 50);
            for (a, b) in cache.out.iter().zip(&compute(&study, shorter.view())) {
                assert_eq!(defined(a), defined(b), "{kind:?}");
            }
        }
    }

    #[test]
    fn studies_are_saved_as_text_and_read_back() {
        let mut bb = Study::new(Kind::Bollinger);
        bb.set(1, 2.5);
        bb.width = 3.0;
        bb.dash = Dash::Dashed;
        let items = vec![Study::new(Kind::Rsi), bb, Study::new(Kind::Vwap)];
        let text = Studies::save(&items);
        assert_eq!(
            text,
            "rsi 14 color=aa82ff\nbb 20 2.5 color=608cff width=3 dash=dashed\nvwap color=608cff\n"
        );
        assert_eq!(Studies::load(&text), items);
        assert_eq!(bb.label(), "BB 20 2.5");
        // Out of bounds is kept in, the unknown is left out.
        let read = Studies::load("sma 0\nnope 3\nema 12.7 color=xyz\nrsi\n");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].params(), [1.0]);
        assert_eq!(read[1], Study::new(Kind::Rsi));
    }
}
