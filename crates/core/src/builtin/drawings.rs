use super::legend::decimals;
use crate::palette;
use crate::plugin::{commands, names, order};
use crate::{
    Align, CandleView, Chart, Clip, Color, DrawCtx, Event, EventCtx, Handler, Key, Layer, Panes, Plugin, PointerShape,
    PriceScale, Response, Scene, TimeScale,
};

/// How close (logical pixels) the pointer must be to a drawing to pick it up.
const HIT: f32 = 9.0;
/// A release farther than this (logical pixels) from the pending drawing's last point places the next point there;
/// a nearer one (a click or a tap on the spot) leaves the next point to the next click.
const DRAG: f32 = 4.0;
const HANDLE: f32 = 4.5;
/// How far down and to the right (logical pixels) a clone is put from its original.
const CLONE_OFFSET: f32 = 24.0;
/// How far (logical pixels) the magnet reaches for a price of the bar under the pointer.
const MAGNET: f32 = 30.0;

/// The color new drawings get until another is chosen.
pub const DEFAULT_COLOR: Color = palette::BLUE;

/// The levels of a Fibonacci retracement, and their colors: 0 at the second point, 1 at the first.
pub const FIB_LEVELS: [(f64, Color); 8] = [
    (0.0, palette::GRAY),
    (0.236, palette::CORAL),
    (0.382, palette::AMBER),
    (0.5, palette::JADE),
    (0.618, palette::CYAN),
    (0.786, palette::BLUE),
    (1.0, palette::GRAY),
    (1.618, palette::VIOLET),
];

/// A place on the chart: a moment (milliseconds, fractional, between bars too) and a price. Pinned to a moment rather than
/// a bar, it stays where it is when bars are appended and when the chart shows another timeframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub time: f64,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// A line between two points.
    Trend,
    /// From the first point through the second, on to the edge.
    Ray,
    /// Through both points, from edge to edge.
    Extended,
    /// A line with an arrowhead at the second point.
    Arrow,
    /// A horizontal line at a price (one click).
    HLine,
    /// From a point to the right edge.
    HRay,
    /// A vertical line at a moment (one click).
    VLine,
    /// A horizontal and a vertical line through a point (one click).
    Cross,
    /// Two parallel lines: two points for the first, a third for how far the second is.
    Channel,
    /// A translucent box between two corners.
    Rect,
    /// Fibonacci retracement levels between two prices.
    Fib,
    /// Distance in price between two points.
    PriceRange,
    /// Distance in time and bars between two points.
    DateRange,
    /// Distance in price, percent, bars and time between two points. Not kept: it goes with the next click.
    Measure,
    /// A planned long trade (one click): entry, target above, stop below.
    Long,
    /// A planned short trade (one click): entry, target below, stop above.
    Short,
    /// A note (one click, then typed): its text starts at the point.
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dash {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// One drawing. Which points a tool uses: `a` always; `b` the two point tools, and for a position the end and the
/// target; `c` the channel's second line, and a position's stop. Unused points are kept equal to `a`.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    pub tool: Tool,
    /// The pane it is in.
    pub pane: &'static str,
    pub a: Anchor,
    pub b: Anchor,
    pub c: Anchor,
    pub color: Color,
    /// Line width, logical pixels.
    pub width: f32,
    pub dash: Dash,
    /// What a note says.
    pub text: String,
    /// Kept where it is: it can be picked, not moved nor reshaped.
    pub locked: bool,
    /// The size of a note's letters, logical pixels.
    pub size: f32,
}

/// How new drawings look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawingStyle {
    pub color: Color,
    pub width: f32,
    pub dash: Dash,
    /// The size of a note's letters.
    pub size: f32,
}

impl Default for DrawingStyle {
    fn default() -> Self {
        Self {
            color: DEFAULT_COLOR,
            width: 2.0,
            dash: Dash::Solid,
            size: TEXT_SIZE,
        }
    }
}

/// What the user drew, and the state of the tool in hand. A resource, so a plugin can add drawings
/// (an alert line, a level from a strategy), read them, or save and restore `items`.
#[derive(Default, Debug, Clone)]
pub struct Drawings {
    pub items: Vec<Drawing>,
    /// The tool that the next click uses.
    pub tool: Option<Tool>,
    pub selected: Option<usize>,
    /// More drawings selected along with `selected` (clicked with Shift or Ctrl, or boxed): deleted with it.
    pub group: Vec<usize>,
    /// The box being dragged out with Shift or Ctrl held, from the press to the pointer, in chart pixels: the
    /// drawings in it are selected when the button comes up.
    pub boxing: Option<((f32, f32), (f32, f32))>,
    /// The drawing under the pointer: it shows its handles.
    pub hovered: Option<usize>,
    /// A drawing waiting for its next point (with the pointer as it is now), and the ruler.
    pub pending: Option<Drawing>,
    pub measure: Option<Drawing>,
    /// New drawings look like this.
    pub style: DrawingStyle,
    /// Points snap to the open, high, low or close of the bar under the pointer (in the main pane).
    pub magnet: bool,
    /// Nothing is drawn nor picked.
    pub hidden: bool,
    /// The note being typed: keys go into its text.
    pub editing: Option<usize>,
    /// The drawings before each change, the latest last, and those undone; `recorded` is how they were at the
    /// last look (see [`Drawings::checkpoint`]).
    undo: Vec<Vec<Drawing>>,
    redo: Vec<Vec<Drawing>>,
    recorded: Vec<Drawing>,
    /// What Ctrl+C or Ctrl+X kept, for Ctrl+V.
    copied: Vec<Drawing>,
}

/// Changes kept to undo.
const UNDO_DEPTH: usize = 100;

impl Tool {
    pub const ALL: [Tool; 17] = [
        Tool::Trend,
        Tool::Ray,
        Tool::Extended,
        Tool::Arrow,
        Tool::HLine,
        Tool::HRay,
        Tool::VLine,
        Tool::Cross,
        Tool::Channel,
        Tool::Rect,
        Tool::Fib,
        Tool::PriceRange,
        Tool::DateRange,
        Tool::Measure,
        Tool::Long,
        Tool::Short,
        Tool::Text,
    ];

    /// Its name in saved text.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Trend => "trend",
            Tool::Ray => "ray",
            Tool::Extended => "extended",
            Tool::Arrow => "arrow",
            Tool::HLine => "hline",
            Tool::HRay => "hray",
            Tool::VLine => "vline",
            Tool::Cross => "cross",
            Tool::Channel => "channel",
            Tool::Rect => "rect",
            Tool::Fib => "fib",
            Tool::PriceRange => "price-range",
            Tool::DateRange => "date-range",
            Tool::Measure => "measure",
            Tool::Long => "long",
            Tool::Short => "short",
            Tool::Text => "text",
        }
    }

    /// Its name for people.
    pub fn title(self) -> &'static str {
        match self {
            Tool::Trend => "Trend line",
            Tool::Ray => "Ray",
            Tool::Extended => "Extended line",
            Tool::Arrow => "Arrow",
            Tool::HLine => "Horizontal line",
            Tool::HRay => "Horizontal ray",
            Tool::VLine => "Vertical line",
            Tool::Cross => "Cross line",
            Tool::Channel => "Parallel channel",
            Tool::Rect => "Rectangle",
            Tool::Fib => "Fib retracement",
            Tool::PriceRange => "Price range",
            Tool::DateRange => "Date range",
            Tool::Measure => "Ruler",
            Tool::Long => "Long position",
            Tool::Short => "Short position",
            Tool::Text => "Text",
        }
    }

    /// The command that picks it.
    pub fn command(self) -> &'static str {
        match self {
            Tool::Trend => commands::DRAW_TREND,
            Tool::Ray => commands::DRAW_RAY,
            Tool::Extended => commands::DRAW_EXTENDED,
            Tool::Arrow => commands::DRAW_ARROW,
            Tool::HLine => commands::DRAW_HLINE,
            Tool::HRay => commands::DRAW_HRAY,
            Tool::VLine => commands::DRAW_VLINE,
            Tool::Cross => commands::DRAW_CROSS,
            Tool::Channel => commands::DRAW_CHANNEL,
            Tool::Rect => commands::DRAW_RECT,
            Tool::Fib => commands::DRAW_FIB,
            Tool::PriceRange => commands::DRAW_PRICE_RANGE,
            Tool::DateRange => commands::DRAW_DATE_RANGE,
            Tool::Measure => commands::DRAW_MEASURE,
            Tool::Long => commands::DRAW_LONG,
            Tool::Short => commands::DRAW_SHORT,
            Tool::Text => commands::DRAW_TEXT,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        // Rectangles were zones before.
        let name = if name == "zone" { "rect" } else { name };
        Tool::ALL.into_iter().find(|t| t.name() == name)
    }

    /// How many points are placed with clicks.
    pub fn clicks(self) -> usize {
        match self {
            Tool::HLine | Tool::HRay | Tool::VLine | Tool::Cross | Tool::Long | Tool::Short | Tool::Text => 1,
            Tool::Channel => 3,
            _ => 2,
        }
    }

    /// Whether its price shows on the price axis.
    pub fn labels_price(self) -> bool {
        matches!(self, Tool::HLine | Tool::HRay | Tool::Cross)
    }

    /// Whether it is a line its style applies to (positions and ranges have their own colors).
    pub fn styled(self) -> bool {
        !matches!(
            self,
            Tool::Long | Tool::Short | Tool::Measure | Tool::PriceRange | Tool::DateRange
        )
    }

    /// Whether it is a line between two points, whose angle Shift holds to steps of 45 degrees.
    fn angled(self) -> bool {
        matches!(self, Tool::Trend | Tool::Ray | Tool::Extended | Tool::Arrow)
    }
}

impl Dash {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Dash::Solid => "solid",
            Dash::Dashed => "dashed",
            Dash::Dotted => "dotted",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [Dash::Solid, Dash::Dashed, Dash::Dotted]
            .into_iter()
            .find(|d| d.name() == name)
    }
}

pub(crate) fn hex(color: Color) -> String {
    format!("{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

pub(crate) fn parse_hex(text: &str) -> Option<Color> {
    let v = u32::from_str_radix(text, 16).ok().filter(|_| text.len() == 6)?;
    Some(Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// A note's text as one word of the saved line: `%`, spaces and line breaks as `%25`, `%20`, `%0A`...
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c == '%' || c.is_whitespace() || c.is_control() {
            let mut bytes = [0; 4];
            for b in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unescape(word: &str) -> String {
    let mut bytes = Vec::new();
    let mut rest = word.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let hex = (b == b'%')
            .then(|| tail.get(..2))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                bytes.push(v);
                rest = &tail[2..];
            }
            None => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn parse_anchor(text: &str) -> Option<Anchor> {
    let (time, price) = text.split_once(',')?;
    let num = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite());
    Some(Anchor {
        time: num(time)?,
        price: num(price)?,
    })
}

impl Drawing {
    /// A drawing of `tool` with every point at `at`.
    pub fn new(tool: Tool, pane: &'static str, at: Anchor, style: DrawingStyle) -> Self {
        Self {
            tool,
            pane,
            a: at,
            b: at,
            c: at,
            color: style.color,
            width: style.width,
            dash: style.dash,
            text: String::new(),
            locked: false,
            size: style.size,
        }
    }

    fn shifted(&self, dtime: f64, dprice: f64) -> Self {
        let shift = |p: Anchor| Anchor {
            time: p.time + dtime,
            price: p.price + dprice,
        };
        Self {
            a: shift(self.a),
            b: shift(self.b),
            c: shift(self.c),
            ..self.clone()
        }
    }

    /// One line of text: `tool key=value ...`, see [`Drawings::save`].
    fn save(&self) -> String {
        let p = |a: Anchor| format!("{},{}", a.time, a.price);
        let mut text = if self.text.is_empty() {
            String::new()
        } else {
            format!(" text={} size={}", escape(&self.text), self.size)
        };
        if self.locked {
            text.push_str(" locked=1");
        }
        format!(
            "{} pane={} color={} width={} dash={} a={} b={} c={}{text}",
            self.tool.name(),
            self.pane,
            hex(self.color),
            self.width,
            self.dash.name(),
            p(self.a),
            p(self.b),
            p(self.c)
        )
    }

    fn load(line: &str, panes: &[&'static str]) -> Option<Self> {
        let mut words = line.split_whitespace();
        let tool = Tool::from_name(words.next()?)?;
        if tool == Tool::Measure {
            return None;
        }
        let rest: Vec<&str> = words.collect();
        let pane_of = |name: &str| panes.iter().find(|&&p| p == name).copied();
        if !rest.first()?.contains('=') {
            // The first format: `tool pane time price time price rrggbb`.
            let [pane, t0, p0, t1, p1, color] = rest[..] else {
                return None;
            };
            let mut d = Drawing::new(
                tool,
                pane_of(pane)?,
                parse_anchor(&format!("{t0},{p0}"))?,
                DrawingStyle {
                    color: parse_hex(color)?,
                    width: 1.5,
                    ..DrawingStyle::default()
                },
            );
            d.b = parse_anchor(&format!("{t1},{p1}"))?;
            return Some(d);
        }
        let get = |key: &str| {
            rest.iter()
                .find_map(|w| w.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        };
        let a = parse_anchor(get("a")?)?;
        let mut d = Drawing::new(tool, pane_of(get("pane")?)?, a, DrawingStyle::default());
        d.b = get("b").and_then(parse_anchor).unwrap_or(a);
        d.c = get("c").and_then(parse_anchor).unwrap_or(a);
        d.color = get("color").and_then(parse_hex).unwrap_or(DEFAULT_COLOR);
        d.width = get("width")
            .and_then(|w| w.parse::<f32>().ok())
            .filter(|w| w.is_finite())
            .map_or(2.0, |w| w.clamp(1.0, 4.0));
        d.dash = get("dash").and_then(Dash::from_name).unwrap_or_default();
        d.text = get("text").map(unescape).unwrap_or_default();
        d.locked = get("locked") == Some("1");
        d.size = get("size")
            .and_then(|w| w.parse::<f32>().ok())
            .filter(|w| w.is_finite())
            .map_or(TEXT_SIZE, |w| w.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]));
        if tool == Tool::Text && d.text.is_empty() {
            return None;
        }
        Some(d)
    }
}

impl Drawings {
    /// The kept drawings as text, one line each: `tool pane=... color=rrggbb width=... dash=... a=time,price
    /// b=time,price c=time,price` (times in milliseconds). Hand it back to [`load`](Self::load) (in this or a
    /// later session) to get them again.
    pub fn save(&self) -> String {
        self.items.iter().map(|d| d.save() + "\n").collect()
    }

    /// Replaces the drawings with the ones in `text` (see [`save`](Self::save); the older format of
    /// `tool pane time price time price rrggbb` is read too). `panes` are the names of the panes the chart has: a
    /// drawing of another pane, and any line that cannot be read, is skipped. Returns how many drawings were loaded.
    pub fn load(&mut self, text: &str, panes: &[&'static str]) -> usize {
        self.items = text.lines().filter_map(|line| Drawing::load(line, panes)).collect();
        self.cancel();
        (self.undo, self.redo, self.recorded) = (Vec::new(), Vec::new(), self.items.clone());
        self.items.len()
    }

    /// Makes what changed since the last look one step to undo. Called after every event that is not halfway
    /// through a gesture, so a drag is one step, however many events it takes.
    pub fn checkpoint(&mut self) {
        if self.items == self.recorded {
            return;
        }
        let before = std::mem::replace(&mut self.recorded, self.items.clone());
        self.undo.push(before);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Takes the last change back. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        self.checkpoint();
        let Some(before) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.items, before));
        self.after_history();
        true
    }

    /// Makes the last change taken back again. Returns whether there was one.
    pub fn redo(&mut self) -> bool {
        self.checkpoint();
        let Some(after) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.items, after));
        self.after_history();
        true
    }

    /// Ends the typing of a note; one left empty goes.
    fn finish_note(&mut self) {
        if let Some(i) = self.editing.take()
            && self.items.get(i).is_some_and(|d| d.text.is_empty())
        {
            self.items.remove(i);
            self.selected = None;
            self.group.clear();
            self.hovered = None;
        }
    }

    fn after_history(&mut self) {
        self.recorded = self.items.clone();
        self.cancel();
        self.hovered = None;
    }

    /// The selected drawing.
    pub fn selection(&self) -> Option<&Drawing> {
        self.items.get(self.selected?)
    }

    /// Changes how the selected drawing looks, and how the next ones will.
    pub fn restyle(&mut self, style: DrawingStyle) {
        self.style = style;
        if let Some(d) = self.selected.and_then(|i| self.items.get_mut(i)) {
            (d.color, d.width, d.dash, d.size) = (style.color, style.width, style.dash, style.size);
        }
    }

    fn cancel(&mut self) -> bool {
        self.finish_note();
        let had = self.tool.is_some()
            || self.pending.is_some()
            || self.selected.is_some()
            || !self.group.is_empty()
            || self.measure.is_some();
        self.tool = None;
        self.pending = None;
        self.selected = None;
        self.group.clear();
        self.boxing = None;
        self.hovered = None;
        self.measure = None;
        had
    }

    /// Whether the drawing at `index` is selected, alone or with others.
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected == Some(index) || self.group.contains(&index)
    }

    /// Adds the drawing to what is selected, or takes it out.
    fn toggle(&mut self, index: usize) {
        if self.selected == Some(index) {
            self.selected = self.group.pop();
        } else if let Some(at) = self.group.iter().position(|&i| i == index) {
            self.group.remove(at);
        } else {
            self.group.extend(self.selected.replace(index));
        }
    }
}

type P = (f32, f32);

/// Turns anchors of one pane into pixels and back.
struct Map<'a> {
    time: &'a TimeScale,
    view: &'a CandleView<'a>,
    price: PriceScale,
    /// Where the pane's plot is, in the coordinates drawn in.
    top: f32,
    size: (f32, f32),
    /// Device pixels per logical pixel.
    scale: f32,
}

impl Map<'_> {
    fn x(&self, time: f64) -> Option<f32> {
        Some(self.time.x_of(self.view.bar_of_time(time)?) as f32)
    }

    fn y(&self, price: f64) -> f32 {
        self.top + self.price.y_of(price) as f32
    }

    fn px(&self, a: Anchor) -> Option<P> {
        Some((self.x(a.time)?, self.y(a.price)))
    }
}

/// The part of the line through `a` and `b` inside the plot (with a margin): from `a` to `b`, or on past `b`
/// (`forward`), or past both (`both`).
fn clip_line(a: P, b: P, back: bool, forward: bool, m: &Map) -> Option<(P, P)> {
    let margin = 50.0;
    let (x0, x1, y0, y1) = (-margin, m.size.0 + margin, m.top - margin, m.top + m.size.1 + margin);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (
        if back { f32::NEG_INFINITY } else { 0.0 },
        if forward { f32::INFINITY } else { 1.0 },
    );
    for (p, q) in [(-dx, a.0 - x0), (dx, x1 - a.0), (-dy, a.1 - y0), (dy, y1 - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if dx == 0.0 && dy == 0.0 {
        return Some((a, b));
    }
    (t0 <= t1 && t0.is_finite() && t1.is_finite())
        .then_some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
}

/// A line with a dash pattern.
fn stroke(scene: &mut Scene, ends: (P, P), width: f32, color: Color, dash: Dash, scale: f32) {
    Pen::new(dash, width, color, scale).line(scene, ends);
}

/// Draws lines in a dash pattern that runs on from one line to the next, so a path of short pieces is dashed
/// as one.
pub(crate) struct Pen {
    width: f32,
    color: Color,
    /// The length of a dash and of the gap after it, device pixels; no gap for a solid line.
    on: f32,
    off: f32,
    /// How far into a dash and its gap the last line ended.
    at: f32,
}

impl Pen {
    /// `scale` is device pixels per logical pixel, `width` in device pixels.
    pub(crate) fn new(dash: Dash, width: f32, color: Color, scale: f32) -> Self {
        let (on, off) = match dash {
            Dash::Solid => (f32::INFINITY, 0.0),
            Dash::Dashed => (6.0 * scale, 4.0 * scale),
            Dash::Dotted => (width.max(scale), 2.5 * scale + width),
        };
        Self {
            width,
            color,
            on,
            off,
            at: 0.0,
        }
    }

    pub(crate) fn line(&mut self, scene: &mut Scene, (a, b): (P, P)) {
        if self.off == 0.0 {
            return scene.line(a, b, self.width, self.color);
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        if len == 0.0 || !len.is_finite() {
            return;
        }
        let (ux, uy) = (dx / len, dy / len);
        let point = |s: f32| (a.0 + ux * s, a.1 + uy * s);
        let period = self.on + self.off;
        let mut s = 0.0;
        while s < len {
            let step = if self.at < self.on {
                let step = (self.on - self.at).min(len - s);
                scene.line(point(s), point(s + step), self.width, self.color);
                step
            } else {
                (period - self.at).min(len - s)
            };
            s += step;
            self.at = (self.at + step) % period;
        }
    }
}

/// A box from its corners.
#[derive(Debug, Clone, Copy)]
struct Box2 {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Box2 {
    fn of(a: P, b: P) -> Self {
        Self {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }

    fn contains(&self, (x, y): P, reach: f32) -> bool {
        x >= self.left - reach && x <= self.right + reach && y >= self.top - reach && y <= self.bottom + reach
    }

    fn fill(&self, scene: &mut Scene, color: Color) {
        scene.fill(
            self.left,
            self.top,
            self.right - self.left,
            self.bottom - self.top,
            color,
        );
    }
}

/// The lines and areas a drawing is made of, in pixels: what is drawn with its style, and what the pointer can
/// pick it by.
struct Shape {
    lines: Vec<(P, P)>,
    areas: Vec<Box2>,
}

/// A position's prices: entry, target and stop.
fn position_points(d: &Drawing, m: &Map) -> Option<(f32, f32, f32, f32, f32)> {
    let (left, right) = (m.x(d.a.time)?, m.x(d.b.time)?);
    Some((
        left.min(right),
        left.max(right),
        m.y(d.a.price),
        m.y(d.b.price),
        m.y(d.c.price),
    ))
}

/// The price of the line through `a` and `b` at `time`.
fn line_price_at(a: Anchor, b: Anchor, time: f64) -> f64 {
    if b.time == a.time {
        a.price
    } else {
        a.price + (b.price - a.price) * (time - a.time) / (b.time - a.time)
    }
}

/// How far the second line of a channel is from the first, in price.
fn channel_offset(d: &Drawing) -> f64 {
    d.c.price - line_price_at(d.a, d.b, d.c.time)
}

/// The size of a new note's letters, logical pixels.
const TEXT_SIZE: f32 = 14.0;
/// The sizes a note's letters can be given.
pub const TEXT_SIZES: [f32; 6] = [10.0, 12.0, 14.0, 18.0, 24.0, 32.0];
/// What an empty note shows while it is typed.
const TEXT_HINT: &str = "Text";

/// The box of a note's text: it starts at the point, which is halfway up its first line.
fn text_box(d: &Drawing, a: P, m: &Map) -> Box2 {
    let size = d.size * m.scale;
    let font = tickerlight_text::Font::ui();
    let lines: Vec<&str> = if d.text.is_empty() {
        vec![TEXT_HINT]
    } else {
        d.text.split('\n').collect()
    };
    let width = lines.iter().map(|l| font.measure(l, size)).fold(0.0, f32::max);
    let line_h = size * 1.3;
    Box2 {
        left: a.0,
        top: a.1 - line_h / 2.0,
        right: a.0 + width,
        bottom: a.1 + line_h * (lines.len() as f32 - 0.5),
    }
}

fn shape(d: &Drawing, m: &Map) -> Option<Shape> {
    let (w, top, bottom) = (m.size.0, m.top, m.top + m.size.1);
    let a = m.px(d.a)?;
    let b = m.px(d.b)?;
    let mut lines = Vec::new();
    let mut areas = Vec::new();
    match d.tool {
        Tool::Trend | Tool::Arrow => lines.extend(clip_line(a, b, false, false, m)),
        Tool::Ray => lines.extend(clip_line(a, b, false, true, m)),
        Tool::Extended => lines.extend(clip_line(a, b, true, true, m)),
        Tool::HLine => lines.push(((0.0, a.1), (w, a.1))),
        Tool::HRay => lines.push((a, (w.max(a.0), a.1))),
        Tool::VLine => lines.push(((a.0, top), (a.0, bottom))),
        Tool::Cross => lines.extend([((0.0, a.1), (w, a.1)), ((a.0, top), (a.0, bottom))]),
        Tool::Channel => {
            let off = channel_offset(d);
            let shift = |p: Anchor| Anchor {
                price: p.price + off,
                ..p
            };
            let (a2, b2) = (m.px(shift(d.a))?, m.px(shift(d.b))?);
            lines.extend(clip_line(a, b, false, false, m));
            lines.extend(clip_line(a2, b2, false, false, m));
            areas.push(Box2::of(a, b2));
        }
        Tool::Rect | Tool::PriceRange | Tool::DateRange | Tool::Measure => areas.push(Box2::of(a, b)),
        Tool::Text => areas.push(text_box(d, a, m)),
        Tool::Fib => {
            let (left, right) = (a.0.min(b.0), a.0.max(b.0));
            for (level, _) in FIB_LEVELS {
                let y = m.y(d.b.price + (d.a.price - d.b.price) * level);
                lines.push(((left, y), (right, y)));
            }
        }
        Tool::Long | Tool::Short => {
            let (left, right, entry, target, stop) = position_points(d, m)?;
            areas.push(Box2::of((left, entry), (right, target)));
            areas.push(Box2::of((left, entry), (right, stop)));
        }
    }
    Some(Shape { lines, areas })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    A,
    B,
    C,
    /// A box's other corners: `a`'s time with `b`'s price, and `b`'s time with `a`'s price.
    AB,
    BA,
    /// The right end of a position.
    End,
    Body,
}

/// Where a drawing's handles are.
fn handles(d: &Drawing, m: &Map) -> Vec<(Part, P)> {
    let (Some(a), Some(b)) = (m.px(d.a), m.px(d.b)) else {
        return Vec::new();
    };
    let mid = |p: P, q: P| ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
    match d.tool {
        Tool::HLine => vec![(Part::A, (m.size.0 / 2.0, a.1))],
        Tool::VLine => vec![(Part::A, (a.0, m.top + m.size.1 / 2.0))],
        Tool::HRay | Tool::Cross => vec![(Part::A, a)],
        // A note is moved by its text; a handle would cover its first letter.
        Tool::Text => Vec::new(),
        Tool::Channel => {
            let off = channel_offset(d);
            let shift = |p: Anchor| Anchor {
                price: p.price + off,
                ..p
            };
            let (Some(a2), Some(b2)) = (m.px(shift(d.a)), m.px(shift(d.b))) else {
                return vec![(Part::A, a), (Part::B, b)];
            };
            vec![(Part::A, a), (Part::B, b), (Part::C, mid(a2, b2))]
        }
        Tool::Rect => vec![
            (Part::A, a),
            (Part::B, b),
            (Part::AB, (a.0, b.1)),
            (Part::BA, (b.0, a.1)),
        ],
        Tool::Long | Tool::Short => {
            let Some((left, right, entry, target, stop)) = position_points(d, m) else {
                return Vec::new();
            };
            vec![
                (Part::A, (left, entry)),
                (Part::B, (left, target)),
                (Part::C, (left, stop)),
                (Part::End, (right, entry)),
            ]
        }
        _ => vec![(Part::A, a), (Part::B, b)],
    }
}

/// Moves part of `original` to `at` (or, by its body, by `delta`).
fn reshape(d: &mut Drawing, original: &Drawing, part: Part, at: Anchor, delta: (f64, f64)) {
    let position = matches!(d.tool, Tool::Long | Tool::Short);
    match part {
        Part::Body => *d = original.shifted(delta.0, delta.1),
        Part::A if position => {
            d.a = at;
            d.c.time = at.time;
        }
        Part::A => d.a = at,
        Part::B if position => d.b.price = at.price,
        Part::B => d.b = at,
        Part::C if position => d.c.price = at.price,
        Part::C => d.c = at,
        Part::AB => (d.a.time, d.b.price) = (at.time, at.price),
        Part::BA => (d.b.time, d.a.price) = (at.time, at.price),
        Part::End => d.b.time = at.time,
    }
}

#[derive(Clone, Debug)]
struct Grab {
    index: usize,
    part: Part,
    /// Where the pointer was, and the drawing then.
    from: Anchor,
    original: Drawing,
    /// The other drawings selected with it that its body takes along, as they were.
    others: Vec<(usize, Drawing)>,
}

/// A press with Shift or Ctrl on a drawing: dragging from it drags a copy away, letting go where it went down
/// adds the drawing to the selection or takes it out.
struct AddingPress {
    index: usize,
    from: Anchor,
    at: P,
}

/// Where the pointer is on the chart, as an anchor in the pane it is over.
struct PointerAt {
    pane: usize,
    anchor: Anchor,
}

/// Where `to` would be on the nearest line through `from` at a multiple of 45 degrees.
fn snap_45(from: P, to: P) -> P {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (dy.atan2(dx) / step).round() * step;
    let (cos, sin) = (angle.cos(), angle.sin());
    let length = dx * cos + dy * sin;
    (from.0 + length * cos, from.1 + length * sin)
}

/// How far in time and price `d` goes when its first point moves by `(dx, dy)` pixels.
fn shift_of(ctx: &EventCtx<'_>, d: &Drawing, (dx, dy): P) -> (f64, f64) {
    let pane = ctx.pane_stack.names().position(|n| n == d.pane);
    let shift = pane.and_then(|p| map_of(ctx, p)).and_then(|m| {
        let (x, y) = m.px(d.a)?;
        let time = ctx.view.time_of_bar(ctx.time.index_of((x + dx) as f64))?;
        let price = m.price.price_of((y + dy - m.top) as f64);
        Some((time - d.a.time, price - d.a.price))
    });
    shift.unwrap_or((0.0, 0.0))
}

/// Picks a tool, places drawings with clicks, and selects, moves and deletes them.
#[derive(Default)]
pub struct DrawingsHandler {
    grab: Option<Grab>,
    adding_press: Option<AddingPress>,
    /// Where the pending drawing's last point was put, in pixels.
    last: Option<(f32, f32)>,
    /// The button went down on the chart while a drawing was being placed.
    pressing: bool,
    /// How many points of the pending drawing are placed.
    placed: usize,
}

impl DrawingsHandler {
    /// With Shift or Ctrl held, the end of a line being drawn or dragged goes where the line keeps a multiple of
    /// 45 degrees to the other end.
    fn aimed(&self, ctx: &EventCtx<'_>, drawings: &Drawings, here: PointerAt) -> PointerAt {
        let fixed = match (&drawings.pending, &self.grab) {
            (Some(d), _) if self.placed == 0 => Some((d, d.a)),
            (None, Some(g)) if g.part == Part::A => Some((&g.original, g.original.b)),
            (None, Some(g)) if g.part == Part::B => Some((&g.original, g.original.a)),
            _ => None,
        };
        let Some((d, fixed)) = fixed.filter(|(d, _)| ctx.pointer.adding && d.tool.angled()) else {
            return here;
        };
        let pane = ctx.pane_stack.names().position(|n| n == d.pane);
        let aimed = pane.and_then(|p| Some((p, map_of(ctx, p)?))).and_then(|(pane, m)| {
            let (x, y) = snap_45(m.px(fixed)?, ctx.pointer.position?);
            let time = ctx.view.time_of_bar(ctx.time.index_of(x as f64))?;
            let price = m.price.price_of((y - m.top) as f64);
            Some(PointerAt {
                pane,
                anchor: Anchor { time, price },
            })
        });
        aimed.unwrap_or(here)
    }
}

fn map_of<'a>(ctx: &'a EventCtx<'_>, pane: usize) -> Option<Map<'a>> {
    let rect = ctx.panes.get(pane)?;
    Some(Map {
        time: ctx.time,
        view: &ctx.view,
        price: ctx.prices.get(pane).copied().flatten()?,
        top: rect.top,
        size: (ctx.plot.0, rect.height),
        scale: ctx.scale,
    })
}

fn anchor_at(ctx: &EventCtx<'_>, magnet: bool) -> Option<PointerAt> {
    let (x, y) = ctx.pointer.position?;
    let pane = ctx.pointer_pane()?;
    let m = map_of(ctx, pane)?;
    let bar = ctx.time.index_of(x as f64);
    let mut anchor = Anchor {
        time: ctx.view.time_of_bar(bar)?,
        price: m.price.price_of((y - m.top) as f64),
    };
    if magnet
        && pane == 0
        && bar >= 0.0
        && let Some(candle) = ctx.view.get(bar.floor() as usize)
    {
        // The price of the bar nearest the pointer, if it is near enough, at the middle of the bar.
        let nearest = [candle.open, candle.high, candle.low, candle.close]
            .into_iter()
            .map(|p| (p, (m.y(p) - y).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((price, dist)) = nearest
            && dist <= MAGNET * ctx.scale
        {
            anchor = Anchor {
                time: ctx.view.time_of_bar(bar.floor() + 0.5)?,
                price,
            };
        }
    }
    Some(PointerAt { pane, anchor })
}

fn distance_to_segment((px, py): P, (ax, ay): P, (bx, by): P) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    ((px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2)).sqrt()
}

impl DrawingsHandler {
    /// The drawing (last drawn first) and part under the pointer.
    fn pick(ctx: &EventCtx<'_>, items: &[Drawing]) -> Option<(usize, Part)> {
        let p = ctx.pointer.position?;
        let reach = HIT * ctx.scale;
        for (index, d) in items.iter().enumerate().rev() {
            let Some(pane) = ctx.pane_stack.names().position(|n| n == d.pane) else {
                continue;
            };
            let Some(m) = map_of(ctx, pane) else { continue };
            let near = |q: P| (p.0 - q.0).hypot(p.1 - q.1) <= reach;
            if let Some(&(part, _)) = handles(d, &m).iter().find(|(_, q)| near(*q)) {
                return Some((index, part));
            }
            let Some(shape) = shape(d, &m) else { continue };
            let on_line = shape.lines.iter().any(|&(a, b)| distance_to_segment(p, a, b) <= reach);
            if on_line || shape.areas.iter().any(|b| b.contains(p, reach)) {
                return Some((index, Part::Body));
            }
        }
        None
    }

    /// The drawings with a handle inside the box between the corners `a` and `b` (chart pixels).
    fn boxed(ctx: &EventCtx<'_>, items: &[Drawing], (a, b): (P, P)) -> Vec<usize> {
        let (xs, ys) = (a.0.min(b.0)..=a.0.max(b.0), a.1.min(b.1)..=a.1.max(b.1));
        let mut found = Vec::new();
        for (index, d) in items.iter().enumerate() {
            let Some(pane) = ctx.pane_stack.names().position(|n| n == d.pane) else {
                continue;
            };
            let Some(m) = map_of(ctx, pane) else { continue };
            if handles(d, &m)
                .iter()
                .any(|&(_, q)| xs.contains(&q.0) && ys.contains(&q.1))
            {
                found.push(index);
            }
        }
        found
    }

    /// Places the pending drawing's next point at `at`; the last one finishes it.
    fn place(&mut self, drawings: &mut Drawings, at: Anchor) {
        let Some(mut d) = drawings.pending.clone() else { return };
        self.placed += 1;
        match self.placed {
            1 => d.b = at,
            _ => d.c = at,
        }
        drawings.pending = Some(d.clone());
        if self.placed + 1 >= d.tool.clicks() {
            self.finish(drawings, d);
        }
    }

    fn finish(&mut self, drawings: &mut Drawings, d: Drawing) {
        drawings.pending = None;
        drawings.tool = None;
        self.last = None;
        self.pressing = false;
        self.placed = 0;
        if d.tool == Tool::Measure {
            drawings.measure = Some(d);
        } else {
            let text = d.tool == Tool::Text;
            drawings.items.push(d);
            drawings.selected = Some(drawings.items.len() - 1);
            if text {
                drawings.editing = drawings.selected;
            }
        }
    }

    /// A position placed with one click: the target twice as far from the entry as the stop, about a fifth of the
    /// plot wide.
    fn position(ctx: &EventCtx<'_>, mut d: Drawing, pane: usize) -> Drawing {
        let (Some(m), Some((x, y))) = (map_of(ctx, pane), ctx.pointer.position) else {
            return d;
        };
        let sign = if d.tool == Tool::Long { 1.0 } else { -1.0 };
        let reward = ctx.scale * 60.0;
        let price_at = |y: f32| m.price.price_of((y - m.top) as f64);
        d.b.price = price_at(y - sign * reward);
        d.c.price = price_at(y + sign * reward / 2.0);
        let end = ctx.time.index_of((x + (m.size.0 / 5.0).max(ctx.scale * 80.0)) as f64);
        d.b.time = ctx.view.time_of_bar(end).unwrap_or(d.a.time);
        d
    }
}

impl Handler for DrawingsHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let picked = match *event {
            Event::Command(name) => Tool::ALL.into_iter().find(|t| t.command() == name),
            _ => None,
        };
        let magnet = ctx.res.get::<Drawings>().is_some_and(|d| d.magnet);
        let here = anchor_at(ctx, magnet);
        let pane_name = here.as_ref().and_then(|h| ctx.pane_stack.name(h.pane));
        // Hit tests read the chart while the drawings change, so the drawings are taken out of the
        // resources for the duration (a move, no allocation) and put back.
        let Some(slot) = ctx.res.get_mut::<Drawings>() else {
            return Response::IGNORED;
        };
        let mut drawings = std::mem::take(slot);
        let here = here.map(|h| self.aimed(ctx, &drawings, h));
        let response = self.react(event, ctx, &mut drawings, picked, here, pane_name);
        if drawings.pending.is_none() {
            self.placed = 0;
        }
        if let Some(slot) = ctx.res.get_mut::<Drawings>() {
            *slot = drawings;
        }
        response
    }
}

impl DrawingsHandler {
    fn react(
        &mut self,
        event: &Event,
        ctx: &mut EventCtx<'_>,
        drawings: &mut Drawings,
        picked: Option<Tool>,
        here: Option<PointerAt>,
        pane_name: Option<&'static str>,
    ) -> Response {
        if let Some(tool) = picked {
            drawings.cancel();
            drawings.tool = Some(tool);
            drawings.hidden = false;
            self.grab = None;
            return Response::CONSUMED_REDRAW;
        }
        match event {
            Event::Command(commands::CANCEL) => {
                self.grab = None;
                self.last = None;
                self.pressing = false;
                if drawings.cancel() {
                    Response::CONSUMED_REDRAW
                } else {
                    Response::IGNORED
                }
            }
            Event::Command(commands::DRAW_DELETE) => {
                let mut gone: Vec<usize> = (drawings.selected.take().into_iter())
                    .chain(drawings.group.drain(..))
                    .filter(|&i| i < drawings.items.len())
                    .collect();
                if gone.is_empty() {
                    return Response::IGNORED;
                }
                gone.sort_unstable();
                gone.dedup();
                for i in gone.into_iter().rev() {
                    drawings.items.remove(i);
                }
                drawings.hovered = None;
                self.grab = None;
                Response::CONSUMED_REDRAW
            }
            Event::Command(commands::DRAW_CLEAR) => {
                let had = !drawings.items.is_empty() || drawings.measure.is_some();
                drawings.items.clear();
                drawings.cancel();
                self.grab = None;
                if had {
                    Response::CONSUMED_REDRAW
                } else {
                    Response::IGNORED
                }
            }
            Event::Command(commands::DRAW_UNDO) | Event::Command(commands::DRAW_REDO) => {
                self.grab = None;
                self.last = None;
                self.pressing = false;
                let done = if *event == Event::Command(commands::DRAW_UNDO) {
                    drawings.undo()
                } else {
                    drawings.redo()
                };
                if done {
                    Response::CONSUMED_REDRAW
                } else {
                    Response::IGNORED
                }
            }
            Event::Command(commands::DRAW_LOCK) => match drawings.selected.and_then(|i| drawings.items.get_mut(i)) {
                Some(d) => {
                    d.locked = !d.locked;
                    Response::CONSUMED_REDRAW
                }
                None => Response::IGNORED,
            },
            Event::Command(commands::DRAW_CLONE) => {
                let Some(d) = drawings.selection().cloned() else {
                    return Response::IGNORED;
                };
                // A little down and to the right of the original, unlocked, and selected.
                let step = CLONE_OFFSET * ctx.scale;
                let (dt, dp) = shift_of(ctx, &d, (step, step));
                let mut copy = d.shifted(dt, dp);
                copy.locked = false;
                drawings.items.push(copy);
                drawings.selected = Some(drawings.items.len() - 1);
                drawings.group.clear();
                Response::CONSUMED_REDRAW
            }
            Event::Command(commands::DRAW_MAGNET) => {
                drawings.magnet = !drawings.magnet;
                Response::CONSUMED_REDRAW
            }
            Event::Command(commands::DRAW_HIDE) => {
                drawings.hidden = !drawings.hidden;
                drawings.cancel();
                Response::CONSUMED_REDRAW
            }
            Event::SecondaryDown => {
                drawings.finish_note();
                // A tool in hand or a drawing half made is put away.
                if drawings.tool.is_some() || drawings.pending.is_some() {
                    (self.placed, self.last, self.pressing) = (0, None, false);
                    drawings.cancel();
                    return Response::REDRAW;
                }
                // The drawing under the pointer is selected (its group stays if it is in one), for the menu of
                // what to do with it; elsewhere nothing is.
                let picked = match drawings.hidden {
                    true => None,
                    false => Self::pick(ctx, &drawings.items).map(|(i, _)| i),
                };
                match picked {
                    Some(i) if drawings.is_selected(i) => Response::IGNORED,
                    Some(i) => {
                        drawings.group.clear();
                        drawings.selected = Some(i);
                        Response::REDRAW
                    }
                    None => {
                        let had = drawings.selected.take().is_some() || !drawings.group.is_empty();
                        drawings.group.clear();
                        Response {
                            redraw: had,
                            ..Response::IGNORED
                        }
                    }
                }
            }
            Event::PointerDown if ctx.pointer.pressed => {
                let Some(tool) = drawings.tool else {
                    drawings.measure = None;
                    if drawings.hidden {
                        return Response::IGNORED;
                    }
                    let adding = ctx.pointer.adding;
                    return match Self::pick(ctx, &drawings.items) {
                        // With Shift or Ctrl a click adds the drawing to what is selected or takes it out, a drag
                        // drags a copy away.
                        Some((index, _)) if adding => {
                            match &here {
                                Some(h) => {
                                    self.adding_press = Some(AddingPress {
                                        index,
                                        from: h.anchor,
                                        at: ctx.pointer.position.unwrap_or_default(),
                                    })
                                }
                                None => drawings.toggle(index),
                            }
                            Response::CONSUMED_REDRAW
                        }
                        Some((index, _)) if drawings.items[index].locked => {
                            drawings.group.clear();
                            drawings.selected = Some(index);
                            Response::CONSUMED_REDRAW
                        }
                        Some((index, part)) => {
                            let Some(h) = here else { return Response::IGNORED };
                            // The body of one of several selected drags them all; else it alone is selected.
                            let together =
                                part == Part::Body && drawings.is_selected(index) && !drawings.group.is_empty();
                            if !together {
                                drawings.group.clear();
                            } else if drawings.selected != Some(index) {
                                drawings.group.retain(|&i| i != index);
                                drawings.group.extend(drawings.selected);
                            }
                            drawings.selected = Some(index);
                            let others = match together {
                                true => (drawings.group.iter())
                                    .filter_map(|&i| Some((i, drawings.items.get(i).filter(|d| !d.locked)?.clone())))
                                    .collect(),
                                false => Vec::new(),
                            };
                            self.grab = Some(Grab {
                                index,
                                part,
                                from: h.anchor,
                                original: drawings.items[index].clone(),
                                others,
                            });
                            Response::CONSUMED_REDRAW
                        }
                        // With Shift or Ctrl on an empty place: a box, to select the drawings in it.
                        None if adding && here.is_some() => {
                            let at = ctx.pointer.position.unwrap_or_default();
                            drawings.boxing = Some((at, at));
                            Response::CONSUMED_REDRAW
                        }
                        None => {
                            let had = drawings.selected.take().is_some() || !drawings.group.is_empty();
                            drawings.group.clear();
                            Response {
                                redraw: had,
                                ..Response::IGNORED
                            }
                        }
                    };
                };
                let (Some(h), Some(pane)) = (here, pane_name) else {
                    return Response::IGNORED;
                };
                // The next point goes where the button comes up, so a click, a drag and a touchpad's tap-and-drag
                // all draw.
                if drawings.pending.is_none() {
                    let mut d = Drawing::new(tool, pane, h.anchor, drawings.style);
                    if matches!(tool, Tool::Long | Tool::Short) {
                        d = Self::position(ctx, d, h.pane);
                    }
                    if tool.clicks() == 1 {
                        self.finish(drawings, d);
                    } else {
                        drawings.pending = Some(d);
                        self.last = ctx.pointer.position;
                    }
                }
                self.pressing = drawings.pending.is_some();
                Response::CONSUMED_REDRAW
            }
            Event::PointerMoved { .. } => {
                if let Some((from, _)) = drawings.boxing
                    && let Some(at) = ctx.pointer.position
                {
                    drawings.boxing = Some((from, at));
                    return Response::CONSUMED_REDRAW;
                }
                if let Some(press) = &self.adding_press
                    && let Some(at) = ctx.pointer.position
                    && here.is_some()
                    && (at.0 - press.at.0).hypot(at.1 - press.at.1) > DRAG * ctx.scale
                {
                    // The copy is what moves; the original stays.
                    let mut copy = drawings.items[press.index].clone();
                    copy.locked = false;
                    drawings.items.push(copy.clone());
                    let index = drawings.items.len() - 1;
                    (drawings.selected, drawings.hovered) = (Some(index), None);
                    drawings.group.clear();
                    self.grab = Some(Grab {
                        index,
                        part: Part::Body,
                        from: press.from,
                        original: copy,
                        others: Vec::new(),
                    });
                    self.adding_press = None;
                }
                if let (Some(grab), Some(h)) = (self.grab.as_ref(), here.as_ref()) {
                    *ctx.shape = PointerShape::Move;
                    // Dragging keeps working when the pointer leaves the pane it started in, but not the plot.
                    let delta = (h.anchor.time - grab.from.time, h.anchor.price - grab.from.price);
                    let d = &mut drawings.items[grab.index];
                    reshape(d, &grab.original, grab.part, h.anchor, delta);
                    // Those taken along move as much in time, and in price where they share its pane.
                    for (i, other) in &grab.others {
                        let dp = if other.pane == grab.original.pane { delta.1 } else { 0.0 };
                        if let Some(d) = drawings.items.get_mut(*i) {
                            *d = other.shifted(delta.0, dp);
                        }
                    }
                    return Response::CONSUMED_REDRAW;
                }
                if let Some(pending) = drawings.pending.as_mut() {
                    if let Some(h) = here {
                        match self.placed {
                            0 => pending.b = h.anchor,
                            _ => pending.c = h.anchor,
                        }
                    }
                    return Response {
                        redraw: true,
                        consumed: ctx.pointer.pressed,
                        ..Response::IGNORED
                    };
                }
                // What is under the pointer shows its handles.
                let hovered = if drawings.hidden || drawings.tool.is_some() || ctx.pointer.pressed {
                    None
                } else {
                    Self::pick(ctx, &drawings.items)
                };
                if let Some((i, part)) = hovered
                    && !drawings.items[i].locked
                {
                    *ctx.shape = if part == Part::Body {
                        PointerShape::Hand
                    } else {
                        PointerShape::Move
                    };
                }
                let hovered = hovered.map(|(i, _)| i);
                let changed = hovered != drawings.hovered;
                drawings.hovered = hovered;
                Response {
                    redraw: changed,
                    ..Response::IGNORED
                }
            }
            Event::PointerLeft => {
                self.adding_press = None;
                let changed = drawings.hovered.take().is_some() | drawings.boxing.take().is_some();
                Response {
                    redraw: changed,
                    ..Response::IGNORED
                }
            }
            Event::PointerUp => {
                if let Some(area) = drawings.boxing.take() {
                    for index in Self::boxed(ctx, &drawings.items, area) {
                        if !drawings.is_selected(index) {
                            drawings.group.extend(drawings.selected.replace(index));
                        }
                    }
                    return Response::CONSUMED_REDRAW;
                }
                if let Some(press) = self.adding_press.take() {
                    drawings.toggle(press.index);
                    return Response::CONSUMED_REDRAW;
                }
                let mut response = Response::IGNORED;
                if self.grab.take().is_some() {
                    response = Response::CONSUMED;
                }
                if std::mem::take(&mut self.pressing)
                    && drawings.pending.is_some()
                    && let (Some(last), Some(end), Some(h)) = (self.last, ctx.pointer.position, here)
                    && (end.0 - last.0).hypot(end.1 - last.1) > DRAG * ctx.scale
                {
                    self.place(drawings, h.anchor);
                    self.last = Some(end);
                    response = Response::CONSUMED_REDRAW;
                }
                response
            }
            _ => Response::IGNORED,
        }
    }
}

/// A price as the axis shows it.
/// How a planned position went over the bars the chart has seen from its entry to its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// The target was reached first.
    Target,
    /// The stop was reached first (also when a bar reached both: which came first is not known).
    Stop,
    /// Neither yet: valued at the last close seen.
    Open,
}

/// What became of a long or short position drawing: how it ended, the moment (the bar's open) and the price. `None`
/// for other drawings and before any bar after its entry is seen.
pub fn position_outcome(d: &Drawing, view: &CandleView<'_>) -> Option<(Outcome, f64, f64)> {
    let long = match d.tool {
        Tool::Long => true,
        Tool::Short => false,
        _ => return None,
    };
    let (start, end) = (d.a.time.min(d.b.time), d.a.time.max(d.b.time));
    let times = view.time();
    let from = times.partition_point(|t| (t.0 as f64) < start);
    let mut last = None;
    for (i, t) in times.iter().enumerate().skip(from) {
        let time = t.0 as f64;
        if time > end {
            break;
        }
        let (high, low) = (view.high()[i], view.low()[i]);
        let (stopped, reached) = if long {
            (low <= d.c.price, high >= d.b.price)
        } else {
            (high >= d.c.price, low <= d.b.price)
        };
        if stopped {
            return Some((Outcome::Stop, time, d.c.price));
        }
        if reached {
            return Some((Outcome::Target, time, d.b.price));
        }
        last = Some((Outcome::Open, time, view.close()[i]));
    }
    last
}

pub(crate) fn price_text(price: f64, near: f64) -> String {
    format!("{price:.*}", decimals(near.abs()))
}

/// A span of time as `1d 4h`, `3h 20m` or `45m`.
fn span_text(ms: f64) -> String {
    let minutes = (ms.abs() / 60_000.0).round() as i64;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (d, h, m) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// A label on a rounded box, centered at `(x, y)`.
pub(crate) fn tag(ctx: &mut DrawCtx<'_>, (x, y): P, lines: &[String], fill: Color, ink: Color) {
    let font = ctx.px(ctx.style.font_size);
    let row = font * 1.45;
    let pad = ctx.px(6.0);
    let w = lines
        .iter()
        .map(|l| tickerlight_text::Font::ui().measure(l, font))
        .fold(0.0, f32::max)
        + 2.0 * pad;
    let h = row * lines.len() as f32 + pad;
    // Kept inside the plot.
    let left = (x - w / 2.0).min(ctx.plot.0 - w - pad).max(pad.min(ctx.plot.0 - w));
    let (x, top) = (left + w / 2.0, y - h / 2.0);
    ctx.scene
        .fill_rounded(left, top, w, h, (h / 2.0).min(ctx.px(10.0)), fill);
    for (i, line) in lines.iter().enumerate() {
        let cy = top + pad / 2.0 + row * (i as f32 + 0.5);
        ctx.scene.text(x, cy, font, ink, Align::Center, line.as_str());
    }
}

fn arrowhead(scene: &mut Scene, from: P, to: P, size: f32, width: f32, color: Color) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    for side in [-1.0, 1.0] {
        let (bx, by) = (
            to.0 - ux * size + side * uy * size * 0.6,
            to.1 - uy * size - side * ux * size * 0.6,
        );
        scene.line(to, (bx, by), width, color);
    }
}

/// Draws the drawings of its pane.
pub struct DrawingsLayer;

impl DrawingsLayer {
    fn draw_one(ctx: &mut DrawCtx<'_>, m: &Map, d: &Drawing, emphasis: bool, editing: bool) {
        let Some(shape) = shape(d, m) else { return };
        let width = ctx.px(d.width);
        let scale = ctx.scale;
        let text = ctx.style.hud_text;
        let (up, down) = (ctx.style.candle.up, ctx.style.candle.down);
        let tint = |c: Color, a: u8| Color { a, ..c };
        match d.tool {
            Tool::Rect => {
                for b in &shape.areas {
                    b.fill(ctx.scene, tint(d.color, 40));
                    let corners = [
                        (b.left, b.top),
                        (b.right, b.top),
                        (b.right, b.bottom),
                        (b.left, b.bottom),
                    ];
                    for i in 0..4 {
                        stroke(
                            ctx.scene,
                            (corners[i], corners[(i + 1) % 4]),
                            width,
                            d.color,
                            d.dash,
                            scale,
                        );
                    }
                }
            }
            Tool::Channel => {
                if let (Some(a), Some(bb)) = (m.px(d.a), m.px(d.b))
                    && shape.lines.len() == 2
                {
                    // The band between the lines, as a column of thin slices.
                    let off = channel_offset(d);
                    let (y0, y1) = (m.y(d.a.price + off) - a.1, m.y(d.b.price + off) - bb.1);
                    let (left, right) = (a.0.min(bb.0), a.0.max(bb.0));
                    let slice = ctx.px(2.0).max(1.0);
                    let mut x = left;
                    while x < right {
                        let t = if right > left { (x - left) / (right - left) } else { 0.0 };
                        let (p, q) = if a.0 <= bb.0 { (a, bb) } else { (bb, a) };
                        let (o0, o1) = if a.0 <= bb.0 { (y0, y1) } else { (y1, y0) };
                        let y = p.1 + (q.1 - p.1) * t;
                        let o = o0 + (o1 - o0) * t;
                        ctx.scene
                            .fill(x, y.min(y + o), slice.min(right - x), o.abs(), tint(d.color, 28));
                        x += slice;
                    }
                    // The middle line, dashed.
                    let (l0, l1) = (shape.lines[0], shape.lines[1]);
                    let mid = |p: P, q: P| ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
                    stroke(
                        ctx.scene,
                        (mid(l0.0, l1.0), mid(l0.1, l1.1)),
                        ctx.px(1.0),
                        tint(d.color, 160),
                        Dash::Dashed,
                        scale,
                    );
                }
                for &l in &shape.lines {
                    stroke(ctx.scene, l, width, d.color, d.dash, scale);
                }
            }
            Tool::Fib => {
                let (Some(a), Some(b)) = (m.px(d.a), m.px(d.b)) else {
                    return;
                };
                let (left, right) = (a.0.min(b.0), a.0.max(b.0));
                let font = ctx.px(ctx.style.font_size);
                let mut last: Option<f32> = None;
                for (i, &(level, color)) in FIB_LEVELS.iter().enumerate() {
                    let price = d.b.price + (d.a.price - d.b.price) * level;
                    let y = m.y(price);
                    if let Some(prev) = last
                        && i > 0
                    {
                        ctx.scene
                            .fill(left, prev.min(y), right - left, (y - prev).abs(), tint(color, 26));
                    }
                    last = Some(y);
                    stroke(
                        ctx.scene,
                        ((left, y), (right, y)),
                        ctx.px(1.0).max(1.0),
                        color,
                        d.dash,
                        scale,
                    );
                    let label = format!("{level} ({})", price_text(price, price));
                    ctx.scene
                        .text(left + ctx.px(4.0), y - font * 0.75, font, color, Align::Left, label);
                }
                stroke(ctx.scene, (a, b), ctx.px(1.0), tint(d.color, 150), Dash::Dashed, scale);
            }
            Tool::PriceRange | Tool::DateRange | Tool::Measure => {
                let (Some(a), Some(b)) = (m.px(d.a), m.px(d.b)) else {
                    return;
                };
                let rising = d.b.price >= d.a.price;
                let color = match d.tool {
                    Tool::Measure if rising => up,
                    Tool::Measure => down,
                    _ => DEFAULT_COLOR,
                };
                let bx = Box2::of(a, b);
                let (w, h) = (bx.right - bx.left, bx.bottom - bx.top);
                ctx.scene.fill(bx.left, bx.top, w, h, tint(color, 40));
                let line = ctx.px(1.0).max(1.0);
                let head = ctx.px(6.0);
                let (cx, cy) = ((bx.left + bx.right) / 2.0, (bx.top + bx.bottom) / 2.0);
                if d.tool != Tool::DateRange {
                    ctx.scene.line((cx, a.1), (cx, b.1), line, color);
                    arrowhead(ctx.scene, (cx, a.1), (cx, b.1), head, line, color);
                }
                if d.tool != Tool::PriceRange {
                    ctx.scene.line((a.0, cy), (b.0, cy), line, color);
                    arrowhead(ctx.scene, (a.0, cy), (b.0, cy), head, line, color);
                }
                let change = d.b.price - d.a.price;
                let percent = if d.a.price != 0.0 {
                    change / d.a.price * 100.0
                } else {
                    0.0
                };
                let bars = (ctx.view.bar_of_time(d.b.time).unwrap_or(0.0)
                    - ctx.view.bar_of_time(d.a.time).unwrap_or(0.0))
                .round();
                let price = format!(
                    "{}{} ({percent:+.2}%)",
                    if change >= 0.0 { "+" } else { "-" },
                    price_text(change.abs(), d.a.price)
                );
                let time = format!("{bars} bars, {}", span_text(d.b.time - d.a.time));
                let lines = match d.tool {
                    Tool::PriceRange => vec![price],
                    Tool::DateRange => vec![time],
                    _ => vec![price, time],
                };
                // Below the box when it goes down, above it when it goes up.
                let h = ctx.px(ctx.style.font_size) * 1.45 * lines.len() as f32 + ctx.px(6.0);
                let y = if rising || d.tool == Tool::DateRange {
                    bx.top - h / 2.0 - ctx.px(4.0)
                } else {
                    bx.bottom + h / 2.0 + ctx.px(4.0)
                };
                tag(ctx, (cx, y), &lines, tint(color, 230), text);
            }
            Tool::Long | Tool::Short => {
                let Some((left, right, entry, target, stop)) = position_points(d, m) else {
                    return;
                };
                let zone = |scene: &mut Scene, edge: f32, color: Color| {
                    let (top, bottom) = (entry.min(edge), entry.max(edge));
                    scene.fill(left, top, right - left, bottom - top, tint(color, 48));
                };
                zone(ctx.scene, target, up);
                zone(ctx.scene, stop, down);
                let line = ctx.px(1.0).max(1.0);
                ctx.scene.line((left, entry), (right, entry), line, ctx.style.axis_text);
                let pct = |p: f64| {
                    if d.a.price != 0.0 {
                        (p - d.a.price) / d.a.price * 100.0
                    } else {
                        0.0
                    }
                };
                let risk = (d.c.price - d.a.price).abs();
                let reward = (d.b.price - d.a.price).abs();
                let ratio = if risk > 0.0 { reward / risk } else { 0.0 };
                let cx = (left + right) / 2.0;
                let off = ctx.px(ctx.style.font_size);
                let toward = |from: f32, to: f32| if to < from { -1.0 } else { 1.0 };
                let target_text = format!("Target {} ({:+.2}%)", price_text(d.b.price, d.a.price), pct(d.b.price));
                let stop_text = format!("Stop {} ({:+.2}%)", price_text(d.c.price, d.a.price), pct(d.c.price));
                tag(
                    ctx,
                    (cx, target + toward(entry, target) * off),
                    &[target_text],
                    tint(up, 235),
                    up.ink(),
                );
                tag(
                    ctx,
                    (cx, stop + toward(entry, stop) * off),
                    &[stop_text],
                    tint(down, 235),
                    down.ink(),
                );
                let entry_text = format!("Entry {}  Risk/reward {ratio:.2}", price_text(d.a.price, d.a.price));
                // Its course so far: from the entry to where it ended, or to the last close.
                if let Some((outcome, time, price)) = position_outcome(d, &ctx.view)
                    && let Some(x) = m.x(time)
                {
                    let y = m.y(price);
                    let half = (ctx.time.bar_spacing as f32 / 2.0).max(1.0);
                    let x = x + half;
                    let ink = tint(ctx.style.hud_text, 200);
                    stroke(ctx.scene, ((left, entry), (x, y)), line, ink, Dash::Dashed, scale);
                    let gain = if d.tool == Tool::Long { pct(price) } else { -pct(price) };
                    let (what, color) = match outcome {
                        Outcome::Target => ("Target hit", up),
                        Outcome::Stop => ("Stop hit", down),
                        Outcome::Open => ("Open", if gain >= 0.0 { up } else { down }),
                    };
                    // Beside the box on the entry line, clear of the other tags (the entry tag can be wider).
                    let label = format!("{what} {gain:+.2}%");
                    let (font, pad) = (ctx.px(ctx.style.font_size), ctx.px(6.0));
                    let width = |s: &str| tickerlight_text::Font::ui().measure(s, font) + 2.0 * pad;
                    let from = if emphasis {
                        right.max(cx + width(&entry_text) / 2.0)
                    } else {
                        right
                    };
                    let w = width(&label);
                    tag(
                        ctx,
                        (from + pad + w / 2.0, entry),
                        &[label],
                        tint(color, 235),
                        color.ink(),
                    );
                }
                if emphasis {
                    tag(
                        ctx,
                        (cx, entry),
                        &[entry_text],
                        tint(ctx.style.crosshair_label, 235),
                        ctx.style.crosshair_label_text,
                    );
                }
            }
            Tool::Text => {
                let Some(bx) = shape.areas.first() else { return };
                let size = ctx.px(d.size);
                let line_h = size * 1.3;
                if emphasis {
                    let pad = ctx.px(4.0);
                    let edge = Box2 {
                        left: bx.left - pad,
                        top: bx.top - pad / 2.0,
                        right: bx.right + pad,
                        bottom: bx.bottom + pad / 2.0,
                    };
                    let line = ctx.line();
                    let corners = [
                        (edge.left, edge.top),
                        (edge.right, edge.top),
                        (edge.right, edge.bottom),
                        (edge.left, edge.bottom),
                    ];
                    for i in 0..4 {
                        let side = (corners[i], corners[(i + 1) % 4]);
                        stroke(ctx.scene, side, line, tint(d.color, 140), Dash::Dashed, scale);
                    }
                }
                let (lines, color) = if d.text.is_empty() {
                    (vec![TEXT_HINT], tint(d.color, 110))
                } else {
                    (d.text.split('\n').collect(), d.color)
                };
                let mid = bx.top + line_h / 2.0;
                for (i, l) in lines.iter().enumerate() {
                    let y = mid + line_h * i as f32;
                    ctx.scene.text(bx.left, y, size, color, Align::Left, *l);
                }
                if editing {
                    let last = if d.text.is_empty() { "" } else { lines[lines.len() - 1] };
                    let x = bx.left + tickerlight_text::Font::ui().measure(last, size);
                    let y = mid + line_h * (lines.len() - 1) as f32;
                    ctx.scene
                        .fill(x, y - size / 2.0, ctx.line(), size, tint(ctx.style.hud_text, 230));
                }
            }
            _ => {
                for &l in &shape.lines {
                    stroke(ctx.scene, l, width, d.color, d.dash, scale);
                }
                if d.tool == Tool::Arrow
                    && let (Some(a), Some(b)) = (m.px(d.a), m.px(d.b))
                {
                    arrowhead(ctx.scene, a, b, ctx.px(8.0) + width, width, d.color);
                }
            }
        }
    }

    fn draw_handles(ctx: &mut DrawCtx<'_>, m: &Map, d: &Drawing, selected: bool) {
        let r = ctx.px(HANDLE);
        let ring = ctx.px(1.5).max(1.0);
        let color = if d.tool.styled() { d.color } else { ctx.style.accent };
        for (_, p) in handles(d, m) {
            ctx.scene.circle(p, r, ctx.style.background);
            if selected {
                ctx.scene.circle(p, r - ring, Color { a: 70, ..color });
            }
            ctx.scene
                .stroke_rounded(p.0 - r, p.1 - r, 2.0 * r, 2.0 * r, r, ring, color);
        }
    }
}

impl Layer for DrawingsLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        // The resources outlive the context, so the drawings can be read while the context draws.
        let res = ctx.res;
        let Some(drawings) = res.get::<Drawings>() else {
            return;
        };
        if drawings.hidden {
            return;
        }
        let view = ctx.view;
        let m = Map {
            time: ctx.time,
            view: &view,
            price: *ctx.price,
            top: 0.0,
            size: ctx.plot,
            scale: ctx.scale,
        };
        let pane = ctx.pane.name;
        let all = drawings.items.iter().enumerate().map(|(i, d)| (Some(i), d));
        let extra = drawings
            .pending
            .iter()
            .chain(drawings.measure.iter())
            .map(|d| (None, d));
        for (index, d) in all.chain(extra).filter(|(_, d)| d.pane == pane) {
            let selected = index.is_some_and(|i| drawings.is_selected(i));
            let hovered = index.is_some() && index == drawings.hovered;
            let editing = index.is_some() && index == drawings.editing;
            Self::draw_one(ctx, &m, d, selected || hovered || index.is_none(), editing);
            if selected || hovered {
                Self::draw_handles(ctx, &m, d, selected);
            }
        }
        // The box being dragged out, in this pane's coordinates.
        if let Some((a, b)) = drawings.boxing {
            let top = ctx.pane.top;
            let (x, y) = (a.0.min(b.0), a.1.min(b.1) - top);
            let (w, h) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
            let accent = ctx.style.accent;
            ctx.scene.fill(x, y, w, h, Color { a: 24, ..accent });
            let line = ctx.line();
            ctx.scene.stroke_rounded(x, y, w, h, 0.0, line, accent);
        }
    }
}

/// The price labels of horizontal lines, on the price axis of their pane.
pub struct DrawingLabels;

impl Layer for DrawingLabels {
    fn clip(&self) -> Clip {
        Clip::Pane
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let res = ctx.res;
        let Some(drawings) = res.get::<Drawings>() else {
            return;
        };
        if drawings.hidden {
            return;
        }
        let (plot_w, plot_h) = ctx.plot;
        let label_h = ctx.px(super::crosshair::PRICE_LABEL_HEIGHT);
        let font = ctx.px(ctx.style.font_size);
        let pane = ctx.pane.name;
        for d in drawings
            .items
            .iter()
            .filter(|d| d.tool.labels_price() && d.pane == pane)
        {
            let y = ctx.price.y_of(d.a.price) as f32;
            if !(0.0..plot_h).contains(&y) {
                continue;
            }
            super::axes::axis_tag(ctx, (y - label_h / 2.0, y + label_h / 2.0), d.color);
            ctx.scene.text(
                plot_w + ctx.px(ctx.style.label_padding),
                y,
                font,
                ctx.style.ink(d.color),
                Align::Left,
                price_text(d.a.price, d.a.price),
            );
        }
    }
}

/// Types into the note being written: letters, spaces and Backspace; Enter or Escape ends it (an empty note goes).
/// Enter on a selected note, or a double click on it, writes it again. Before the keymap, so what is typed is not
/// taken for shortcuts.
struct DrawingText;

impl Handler for DrawingText {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let Some(drawings) = ctx.res.get_mut::<Drawings>() else {
            return Response::IGNORED;
        };
        let editing = drawings.editing.filter(|&i| i < drawings.items.len());
        let Some(i) = editing else {
            drawings.editing = None;
            let note = drawings
                .selected
                .filter(|&i| drawings.items.get(i).is_some_and(|d| d.tool == Tool::Text));
            if let (Event::Key(Key::Enter) | Event::DoubleClick, Some(i)) = (event, note) {
                drawings.editing = Some(i);
                return Response::CONSUMED_REDRAW;
            }
            return Response::IGNORED;
        };
        let Event::Key(key) = *event else {
            // A press anywhere else ends the note as it is.
            if matches!(event, Event::PointerDown) {
                drawings.finish_note();
            }
            return Response::IGNORED;
        };
        let text = &mut drawings.items[i].text;
        match key {
            Key::Char(c) if !c.is_control() => text.push(c),
            Key::Space => text.push(' '),
            Key::Backspace => {
                text.pop();
            }
            Key::Enter | Key::Escape => drawings.finish_note(),
            _ => {}
        }
        Response::CONSUMED_REDRAW
    }
}

/// Keys for the selected drawings, before the keymap so that the arrows move them rather than the chart: Left and
/// Right a bar, Up and Down a pixel (a locked one stays). Ctrl+C or Ctrl+X keep them, Ctrl+V puts copies in a
/// little down and to the right of them.
struct DrawingKeys;

impl Handler for DrawingKeys {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let Event::Key(key) = *event else {
            return Response::IGNORED;
        };
        let Some(slot) = ctx.res.get_mut::<Drawings>() else {
            return Response::IGNORED;
        };
        if slot.hidden || slot.editing.is_some() {
            return Response::IGNORED;
        }
        let mut drawings = std::mem::take(slot);
        let response = Self::react(key, ctx, &mut drawings);
        if let Some(slot) = ctx.res.get_mut::<Drawings>() {
            *slot = drawings;
        }
        response
    }
}

impl DrawingKeys {
    fn react(key: Key, ctx: &mut EventCtx<'_>, drawings: &mut Drawings) -> Response {
        let step = match key {
            Key::Left => (-ctx.time.bar_spacing as f32, 0.0),
            Key::Right => (ctx.time.bar_spacing as f32, 0.0),
            Key::Up => (0.0, -ctx.scale),
            Key::Down => (0.0, ctx.scale),
            Key::Copy | Key::Cut => {
                let chosen = drawings.selected.into_iter().chain(drawings.group.iter().copied());
                let copied: Vec<Drawing> = chosen.filter_map(|i| drawings.items.get(i).cloned()).collect();
                if copied.is_empty() {
                    return Response::IGNORED;
                }
                drawings.copied = copied;
                if key == Key::Cut {
                    ctx.emit(commands::DRAW_DELETE);
                }
                return Response::CONSUMED;
            }
            Key::Paste => {
                if drawings.copied.is_empty() {
                    return Response::IGNORED;
                }
                // Each paste goes on from the last, so copies do not pile up on one another.
                let offset = CLONE_OFFSET * ctx.scale;
                for d in &mut drawings.copied {
                    let (dt, dp) = shift_of(ctx, d, (offset, offset));
                    *d = d.shifted(dt, dp);
                    d.locked = false;
                }
                let first = drawings.items.len();
                drawings.items.extend(drawings.copied.iter().cloned());
                drawings.selected = Some(first);
                drawings.group = (first + 1..drawings.items.len()).collect();
                return Response::CONSUMED_REDRAW;
            }
            _ => return Response::IGNORED,
        };
        let mut any = false;
        for k in 0..=drawings.group.len() {
            let i = if k == 0 {
                drawings.selected
            } else {
                Some(drawings.group[k - 1])
            };
            let Some((i, d)) = i.and_then(|i| Some((i, drawings.items.get(i)?))) else {
                continue;
            };
            any = true;
            if !d.locked {
                let (dt, dp) = shift_of(ctx, d, step);
                drawings.items[i] = d.shifted(dt, dp);
            }
        }
        if any {
            Response::CONSUMED_REDRAW
        } else {
            Response::IGNORED
        }
    }
}

/// Looks at the drawings after every event and keeps what changed as a step to undo, unless a gesture is
/// halfway: a button held, a drawing waiting for its next point.
struct DrawingsHistory;

impl Handler for DrawingsHistory {
    fn observes_all(&self) -> bool {
        true
    }

    fn handle(&mut self, _: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if !ctx.pointer.pressed
            && let Some(drawings) = ctx.res.get_mut::<Drawings>()
            && drawings.pending.is_none()
            && drawings.editing.is_none()
        {
            drawings.checkpoint();
        }
        Response::IGNORED
    }
}

pub struct DrawingsPlugin;

impl Plugin for DrawingsPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(Drawings::default());
        chart.add_handler(names::DRAWINGS, order::DRAWINGS_HANDLER, DrawingsHandler::default());
        chart.add_layer_in(Panes::All, names::DRAWINGS, order::DRAWINGS, DrawingsLayer);
        chart.add_layer_in(Panes::All, "drawing-labels", order::DRAWING_LABELS, DrawingLabels);
        chart.add_handler("drawings-history", order::OBSERVER_HANDLER, DrawingsHistory);
        chart.add_handler("drawing-text", order::DRAWING_TEXT_HANDLER, DrawingText);
        chart.add_handler("drawing-keys", order::DRAWING_KEYS_HANDLER, DrawingKeys);
        chart.bind(Key::Undo, commands::DRAW_UNDO);
        chart.bind(Key::Redo, commands::DRAW_REDO);
        chart.bind(Key::Alt('t'), commands::DRAW_TREND);
        chart.bind(Key::Alt('h'), commands::DRAW_HLINE);
        chart.bind(Key::Alt('j'), commands::DRAW_HRAY);
        chart.bind(Key::Alt('v'), commands::DRAW_VLINE);
        chart.bind(Key::Alt('c'), commands::DRAW_CROSS);
        chart.bind(Key::Alt('f'), commands::DRAW_FIB);
        chart.bind(Key::Alt('R'), commands::DRAW_RECT);
        chart.bind(Key::Escape, commands::CANCEL);
        chart.bind(Key::Delete, commands::DRAW_DELETE);
        chart.bind(Key::Backspace, commands::DRAW_DELETE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dash_runs_on_into_the_next_piece_of_a_path() {
        let mut scene = Scene::new();
        let mut pen = Pen::new(Dash::Dashed, 1.0, DEFAULT_COLOR, 1.0);
        // Pieces of 4 px: a dash of 6 covers the first and half the second, then a gap of 4 to the third.
        for x in [0.0, 4.0, 8.0, 12.0] {
            pen.line(&mut scene, ((x, 0.0), (x + 4.0, 0.0)));
        }
        let drawn: Vec<(f32, f32)> = scene.lines.iter().map(|l| (l.x0, l.x1)).collect();
        assert_eq!(drawn, [(0.0, 4.0), (4.0, 6.0), (10.0, 12.0), (12.0, 16.0)]);
    }

    #[test]
    fn spans_read_like_a_clock() {
        assert_eq!(span_text(45.0 * 60_000.0), "45m");
        assert_eq!(span_text(3.0 * 3_600_000.0), "3h");
        assert_eq!(span_text(200.0 * 60_000.0), "3h 20m");
        assert_eq!(span_text(-28.0 * 3_600_000.0), "1d 4h");
        assert_eq!(span_text(2.0 * 86_400_000.0), "2d");
    }

    #[test]
    fn a_line_extended_past_its_points_stops_at_the_plot() {
        let time = TimeScale::new(100.0, 10.0, 0.0);
        let candles = crate::Candles::new();
        let view = candles.view();
        let m = Map {
            time: &time,
            view: &view,
            price: PriceScale::linear(0.0, 1.0, 100.0),
            top: 0.0,
            size: (100.0, 100.0),
            scale: 1.0,
        };
        let (a, b) = clip_line((10.0, 10.0), (20.0, 20.0), true, true, &m).unwrap();
        assert_eq!((a, b), ((-50.0, -50.0), (150.0, 150.0)));
        let (a, b) = clip_line((10.0, 10.0), (20.0, 20.0), false, false, &m).unwrap();
        assert_eq!((a, b), ((10.0, 10.0), (20.0, 20.0)));
        assert!(clip_line((500.0, 10.0), (600.0, 10.0), false, true, &m).is_none());
    }
}
