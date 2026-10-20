use crate::builtin::DefaultPlugins;
use crate::plugin::{
    ChartEdit, Clip, DrawCtx, EventCtx, Handler, Layer, PaneInfo, PaneRect, Panes, Plugin, Pointer, PointerShape,
    Resources, names,
};
use crate::timeframe::TimeframeCtl;
use crate::{
    Candle, CandleStyle, CandleView, Candles, ClipRect, Color, Cursor, OutOfOrder, PriceScale, Resampled, Scene, Sink,
    Source, Time, TimeScale, TradeView, Trades,
};
use crate::{PaneStack, PriceMode, ScaleSetting};

/// Input the chart understands. Backends translate their native events into these,
/// in device pixels of the chart area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    PointerMoved {
        x: f32,
        y: f32,
    },
    PointerDown,
    PointerUp,
    PointerLeft,
    /// The second (right) button went down at the pointer: the menu of what is there is asked for.
    SecondaryDown,
    /// The first button went down and up twice, quickly, at one place; comes after the second `PointerUp`.
    DoubleClick,
    /// Positive `delta` zooms in around `x` (one wheel notch is 1.0).
    Wheel {
        x: f32,
        delta: f32,
    },
    /// A named action, from a key binding, a button or a plugin (see `plugin::commands`). Handlers
    /// react to the commands they know and ignore the rest.
    Command(&'static str),
    /// A key went down. Key repeat arrives as more `Key` events.
    Key(Key),
    /// Time passed, `dt` seconds since the previous tick. Sent by the backend every frame while
    /// [`Chart::needs_tick`] is `true`.
    Tick {
        dt: f32,
    },
    /// Shift, Ctrl or Cmd went down or up: held, a click adds to what is selected.
    Modifiers {
        adding: bool,
    },
}

/// Keys the chart knows about. Letters come as typed; bindings take them in either case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Space,
    Enter,
    Escape,
    Delete,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// With Shift: the caret of a text field moves and the selection follows it. Elsewhere a binding of the key
    /// with Shift, else the same as without.
    SelectLeft,
    SelectRight,
    SelectHome,
    SelectEnd,
    /// The shortcuts of editing (Ctrl or Cmd with A, C, X); pasted text comes as `Char`s.
    SelectAll,
    Copy,
    Cut,
    /// Ctrl or Cmd with V where no text is typed: what was copied on the chart goes in.
    Paste,
    /// Ctrl or Cmd with Z, and with Shift too (or Y).
    Undo,
    Redo,
    /// Alt (Option) with a letter: in lower case, in upper case with Shift too.
    Alt(char),
    /// Ctrl or Cmd with an arrow.
    CtrlLeft,
    CtrlRight,
    CtrlUp,
    CtrlDown,
}

/// The colors the chart and the controls take: dark graphite (the default) or light paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Look {
    #[default]
    Dark,
    Light,
}

impl Look {
    pub const ALL: [Look; 2] = [Look::Dark, Look::Light];

    /// As it is written down.
    pub fn name(self) -> &'static str {
        match self {
            Look::Dark => "dark",
            Look::Light => "light",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }

    /// As the user reads it.
    pub fn label(self) -> &'static str {
        match self {
            Look::Dark => "Dark",
            Look::Light => "Light",
        }
    }

    /// `color` as this look draws it. The palette is tuned for the dark graphite; on light paper each of its
    /// colors gives way to a deeper one that reads on it, so drawings and indicators keep their colors as saved
    /// and still show. Other colors stay as they are.
    pub fn paint(self, color: Color) -> Color {
        match self {
            Look::Dark => color,
            Look::Light => Color {
                a: color.a,
                ..crate::palette::on_paper(Color { a: 255, ..color })
            },
        }
    }
}

/// Colors and sizes the built-in plugins draw with. Sizes are logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChartStyle {
    /// Which look the colors are of: the data colors are painted with it (see [`Look::paint`]).
    pub look: Look,
    pub background: Color,
    pub grid: Color,
    pub axis: Color,
    pub axis_text: Color,
    pub crosshair: Color,
    pub crosshair_label: Color,
    pub crosshair_label_text: Color,
    pub candle: CandleStyle,
    /// Width of the price axis and height of the time axis.
    pub price_axis_width: f32,
    pub time_axis_height: f32,
    pub font_size: f32,
    /// Space between an axis and its labels.
    pub label_padding: f32,
    /// Text of overlays such as the replay and trading readouts, and their accent color.
    pub hud_text: Color,
    pub accent: Color,
    /// Buttons and other floating panels, and under the pointer.
    pub panel: Color,
    pub panel_hover: Color,
    /// Free space above and below the data on the price axis, as a fraction of the price range.
    pub price_margin: f64,
}

impl ChartStyle {
    /// The colors of `look`, with the default sizes.
    pub fn of(look: Look) -> Self {
        let dark = Self::default();
        match look {
            Look::Dark => dark,
            // Warm off-white paper; the chrome is warm grey ink at a few strengths and the data keeps the color.
            Look::Light => Self {
                look,
                background: Color::rgb(251, 250, 247),
                grid: Color::rgb(240, 237, 232),
                axis: Color::rgb(222, 218, 211),
                axis_text: Color::rgb(110, 105, 98),
                crosshair: Color::rgb(150, 145, 138),
                // Dark pills on the light chart, as the light pills on the dark one.
                crosshair_label: Color::rgb(52, 50, 47),
                crosshair_label_text: Color::rgb(248, 246, 242),
                candle: CandleStyle {
                    up: crate::palette::on_paper(crate::palette::JADE),
                    down: crate::palette::on_paper(crate::palette::CORAL),
                    ..dark.candle
                },
                hud_text: Color::rgb(33, 30, 26),
                accent: crate::palette::on_paper(crate::palette::AMBER),
                panel: Color {
                    a: 240,
                    ..Color::rgb(240, 237, 232)
                },
                panel_hover: Color {
                    a: 245,
                    ..Color::rgb(228, 224, 218)
                },
                ..dark
            },
        }
    }

    /// Text that reads on the data color `color` as this look draws it.
    pub fn ink(&self, color: Color) -> Color {
        self.look.paint(color).ink()
    }
}

impl Default for ChartStyle {
    fn default() -> Self {
        Self {
            look: Look::Dark,
            // Warm graphite that recedes behind the data, and one amber accent.
            background: Color::rgb(21, 23, 28),
            grid: Color::rgb(27, 30, 36),
            axis: Color::rgb(44, 48, 57),
            axis_text: Color::rgb(146, 142, 137),
            crosshair: Color::rgb(120, 116, 110),
            crosshair_label: Color::rgb(42, 46, 56),
            crosshair_label_text: Color::rgb(240, 237, 233),
            candle: CandleStyle::default(),
            hud_text: Color::rgb(236, 232, 227),
            accent: Color::rgb(245, 165, 36),
            panel: Color {
                a: 240,
                ..Color::rgb(31, 34, 42)
            },
            panel_hover: Color {
                a: 245,
                ..Color::rgb(42, 46, 56)
            },
            price_axis_width: 72.0,
            time_axis_height: 28.0,
            font_size: 12.0,
            label_padding: 8.0,
            price_margin: 0.1,
        }
    }
}

/// Commands emitting commands: this many rounds at most per input event.
const MAX_COMMAND_ROUNDS: usize = 16;
pub(crate) const DEFAULT_BAR_SPACING: f64 = 8.0;
pub(crate) const DEFAULT_RIGHT_MARGIN_BARS: f64 = 5.0;

struct LayerEntry {
    name: &'static str,
    order: i32,
    panes: Panes,
    layer: Box<dyn Layer>,
}

impl LayerEntry {
    fn is_in(&self, pane: usize, panes: &PaneStack) -> bool {
        match self.panes {
            Panes::Main => pane == 0,
            Panes::One(name) => panes.name(pane) == Some(name),
            Panes::All => true,
        }
    }
}

struct SourceEntry {
    source: Box<dyn Source>,
    live: bool,
}

struct HandlerEntry {
    name: &'static str,
    order: i32,
    handler: Box<dyn Handler>,
    /// What the handler answered to `Response::animate` last time.
    animating: bool,
    /// Sees events that were consumed before it (see [`Handler::observes_all`]).
    observer: bool,
}

/// A chart: data, what part of it is shown, and the plugins that draw it and react to input.
///
/// [`Chart::new`] comes with [`DefaultPlugins`]; [`Chart::empty`] starts with nothing.
/// Add, replace or remove layers and handlers to change anything.
pub struct Chart {
    /// The data as it came. Replay and trading work on these bars.
    candles: Candles,
    /// The period of the candles shown when it is not that of `candles`, and those candles.
    timeframe: Option<i64>,
    derived: Option<Resampled>,
    trades: Trades,
    /// Replay cursor: when set, the chart sees only candles with `time <= cursor`.
    cursor: Option<Time>,
    /// Where a replay that plays by time stands (see [`Cursor::play`]); `None` when it moves bar by bar.
    moment: Option<Time>,
    /// The bar forming at the moment: its index and its real values, which it gets back once it is complete.
    /// Meanwhile `candles` holds at that index only what the trades before the moment made of it.
    held: Option<(usize, Candle)>,
    size: (f32, f32),
    /// Device pixels per logical pixel (HiDPI). Sizes in the style are logical, the scene is in device pixels.
    scale: f32,
    time: TimeScale,
    pointer: Pointer,
    shape: PointerShape,
    pub style: ChartStyle,
    layers: Vec<LayerEntry>,
    handlers: Vec<HandlerEntry>,
    sources: Vec<SourceEntry>,
    resources: Resources,
    /// Panes top to bottom; the first is the main one.
    panes: PaneStack,
    /// The price scale each pane had in the last frame built: what the user sees, for handlers that act on a price.
    prices: Vec<Option<PriceScale>>,
}

impl Chart {
    /// A chart with the default plugins: grid, candles, axes, crosshair, pan and zoom.
    pub fn new(candles: Candles) -> Self {
        Self::empty(candles).with(DefaultPlugins)
    }

    /// A chart without any plugin: it draws nothing until layers are added.
    pub fn empty(candles: Candles) -> Self {
        let mut chart = Self {
            time: TimeScale::new(0.0, DEFAULT_BAR_SPACING, 0.0),
            candles,
            timeframe: None,
            derived: None,
            trades: Trades::new(),
            cursor: None,
            moment: None,
            held: None,
            size: (800.0, 600.0),
            scale: 1.0,
            pointer: Pointer::default(),
            shape: PointerShape::Default,
            style: ChartStyle::default(),
            layers: Vec::new(),
            handlers: Vec::new(),
            sources: Vec::new(),
            resources: Resources::default(),
            panes: PaneStack::default(),
            prices: Vec::new(),
        };
        chart.time.width = chart.plot_size().0 as f64;
        chart.time.right = chart.seen().len() as f64 + DEFAULT_RIGHT_MARGIN_BARS;
        chart.clamp();
        chart
    }

    /// Gives the chart a tape of trades for plugins that draw them (footprints, volume profiles).
    /// The tape is cut wherever the candles are, so the replay cursor holds it back too.
    pub fn with_trades(mut self, trades: Trades) -> Self {
        self.set_trades(trades);
        self
    }

    /// Replaces the tape of trades, such as with the trades of the days now on screen. Bars of seconds, made of
    /// it, keep the moment at the right edge where it was (the newest ones come into view when there were none).
    pub fn set_trades(&mut self, trades: Trades) {
        let anchor = anchor(&self.time, self.seen());
        self.trades = trades;
        self.reform();
        if self.derived.as_ref().is_some_and(Resampled::from_tape) {
            let view = view_of(&self.candles, &self.derived, self.cursor);
            match anchor {
                Some(anchor) => restore(&mut self.time, view, anchor),
                None => self.time.right = view.len() as f64 + DEFAULT_RIGHT_MARGIN_BARS,
            }
            self.clamp();
        }
    }

    /// Adds a plugin, builder style.
    pub fn with(mut self, plugin: impl Plugin) -> Self {
        plugin.build(&mut self);
        self
    }

    pub fn add_plugin(&mut self, plugin: impl Plugin) -> &mut Self {
        plugin.build(self);
        self
    }

    /// Adds a layer. Layers are drawn in ascending `order`, later ones on top. A layer with the same
    /// `name` is replaced, which is how a built-in is swapped for your own.
    pub fn add_layer(&mut self, name: &'static str, order: i32, layer: impl Layer + 'static) -> &mut Self {
        self.add_layer_in(Panes::Main, name, order, layer)
    }

    /// Like [`add_layer`](Self::add_layer), in the given pane, or in all of them.
    pub fn add_layer_in(
        &mut self,
        panes: Panes,
        name: &'static str,
        order: i32,
        layer: impl Layer + 'static,
    ) -> &mut Self {
        self.layers.retain(|e| e.name != name);
        let at = self.layers.partition_point(|e| e.order <= order);
        self.layers.insert(
            at,
            LayerEntry {
                name,
                order,
                panes,
                layer: Box::new(layer),
            },
        );
        self
    }

    /// Adds a pane under the others (an indicator such as RSI goes into one), or changes the weight
    /// of the pane called `name`. The panes share the height of the plot in proportion to their weights;
    /// the main pane weighs `1.0` unless changed the same way.
    pub fn add_pane(&mut self, name: &'static str, weight: f32) -> &mut Self {
        self.panes.add(name, weight);
        self
    }

    /// Removes a pane and the layers in it. The main pane cannot be removed. Returns whether there was such a pane.
    pub fn remove_pane(&mut self, name: &str) -> bool {
        if name == names::MAIN_PANE {
            return false;
        }
        let removed = self.panes.remove(name);
        self.layers.retain(|e| !matches!(e.panes, Panes::One(n) if n == name));
        removed
    }

    /// The panes and how they are laid out: weights, price axis settings and manual ranges.
    pub fn pane_stack(&self) -> &PaneStack {
        &self.panes
    }

    pub fn pane_stack_mut(&mut self) -> &mut PaneStack {
        &mut self.panes
    }

    pub fn pane_names(&self) -> Vec<&'static str> {
        self.panes.names().collect()
    }

    /// Where the panes are, top to bottom, in device pixels. They fill the plot, and each one is at
    /// least a pixel tall.
    pub fn pane_layout(&self) -> Vec<PaneRect> {
        self.panes.layout(self.plot_size().1)
    }

    /// The pane at the height `y` (device pixels) and the price there on its scale as last drawn; `None` before
    /// a frame or over a pane with nothing in it.
    pub fn price_at(&self, y: f32) -> Option<(usize, f64)> {
        let layout = self.pane_layout();
        let pane = layout.iter().position(|p| p.contains_y(y))?;
        let scale = self.prices.get(pane).copied().flatten()?;
        Some((pane, scale.price_of((y - layout[pane].top) as f64)))
    }

    /// Removes the layer called `name`. Returns whether there was one.
    pub fn remove_layer(&mut self, name: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|e| e.name != name);
        self.layers.len() != before
    }

    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|e| e.name).collect()
    }

    /// Adds an event handler. Handlers run in ascending `order` until one consumes the event.
    /// A handler with the same `name` is replaced.
    pub fn add_handler(&mut self, name: &'static str, order: i32, handler: impl Handler + 'static) -> &mut Self {
        self.handlers.retain(|e| e.name != name);
        let at = self.handlers.partition_point(|e| e.order <= order);
        let observer = handler.observes_all();
        self.handlers.insert(
            at,
            HandlerEntry {
                name,
                order,
                handler: Box::new(handler),
                animating: false,
                observer,
            },
        );
        self
    }

    /// Removes the handler called `name`. Returns whether there was one.
    pub fn remove_handler(&mut self, name: &str) -> bool {
        let before = self.handlers.len();
        self.handlers.retain(|e| e.name != name);
        self.handlers.len() != before
    }

    pub fn handler_names(&self) -> Vec<&'static str> {
        self.handlers.iter().map(|e| e.name).collect()
    }

    /// Values plugins share, one per type.
    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    pub fn resources_mut(&mut self) -> &mut Resources {
        &mut self.resources
    }

    /// Adds a live data source. The chart polls it on every tick and applies what it delivers, keeping
    /// the newest bar in place when the chart is scrolled to it. See [`Source`].
    pub fn add_source(&mut self, source: impl Source + 'static) -> &mut Self {
        self.sources.push(SourceEntry {
            source: Box::new(source),
            live: true,
        });
        self
    }

    /// Adds a trade to the tape. Must not be older than the last one. Bars of seconds grow with it, followed as
    /// live candles are.
    pub fn push_trade(&mut self, trade: crate::Trade) -> Result<(), OutOfOrder> {
        let before = self.seen().len();
        self.trades.push(trade)?;
        self.reform();
        self.follow(before);
        Ok(())
    }

    /// Appends a candle (live data). Must be newer than the last one. A chart that is scrolled to the
    /// latest bar follows it; one that is looking at history stays where it is.
    pub fn push(&mut self, candle: Candle) -> Result<(), OutOfOrder> {
        let before = self.seen().len();
        self.unform();
        let pushed = self.candles.push(candle);
        self.reform();
        pushed?;
        self.follow(before);
        Ok(())
    }

    /// Like [`push`](Self::push), but a candle at the time of the newest one replaces it: the forming
    /// candle of a live feed.
    pub fn update(&mut self, candle: Candle) -> Result<(), OutOfOrder> {
        let before = self.seen().len();
        self.unform();
        let updated = self.candles.upsert(candle);
        self.reform();
        updated?;
        self.follow(before);
        Ok(())
    }

    /// Puts the candles of `history` the data have no candle at the time of in with them: the history that comes
    /// after the newest candles were shown, in pieces and in any order. The data's own candles stay as they are,
    /// and so does the bar at the right edge.
    pub fn merge(&mut self, history: &Candles) {
        let history = history.view();
        let (theirs, ours) = (history.time(), self.candles.view().time());
        if theirs.iter().all(|t| ours.binary_search(t).is_ok()) {
            return;
        }
        let anchor = anchor(&self.time, self.seen());
        self.unform();
        let current = self.candles.view();
        let (mut a, mut b) = (0, 0);
        let mut all = Candles::new();
        while a < history.len() || b < current.len() {
            let candle = match (history.time().get(a), current.time().get(b)) {
                (Some(t), Some(u)) if t == u => {
                    a += 1;
                    continue;
                }
                (Some(t), u) if u.is_none_or(|u| t < u) => {
                    a += 1;
                    history.get(a - 1)
                }
                _ => {
                    b += 1;
                    current.get(b - 1)
                }
            };
            all.push(candle.expect("in range")).expect("times increase");
        }
        self.candles = all;
        // The bars of the timeframe shown are made again from the start.
        self.derived = self.timeframe.map(Resampled::new);
        self.reform();
        let view = view_of(&self.candles, &self.derived, self.cursor);
        match anchor {
            Some(anchor) => restore(&mut self.time, view, anchor),
            None => self.time.right = view.len() as f64 + DEFAULT_RIGHT_MARGIN_BARS,
        }
        self.clamp();
    }

    /// Gives the bar forming at the replay's moment its real values back.
    fn unform(&mut self) {
        if let Some((index, real)) = self.held.take() {
            self.candles.set_values(index, real);
        }
    }

    /// See [`reform`].
    fn reform(&mut self) {
        reform(
            &mut self.candles,
            &self.trades,
            &mut self.cursor,
            self.moment,
            &mut self.held,
            &mut self.derived,
        );
    }

    /// Polls the sources and applies what they delivered. Returns whether anything changed the data.
    fn poll_sources(&mut self) -> bool {
        let mut changed = false;
        for i in 0..self.sources.len() {
            let mut sink = Sink::default();
            let live = self.sources[i].source.poll(&mut sink);
            self.sources[i].live = live;
            for candle in sink.candles {
                // A candle older than the newest is a source bug; dropping it beats stopping the chart.
                changed |= self.update(candle).is_ok();
            }
            for trade in sink.trades {
                changed |= self.push_trade(trade).is_ok();
            }
        }
        changed
    }

    /// Keeps the newest bar in the same place on screen after the number of visible bars changed,
    /// if it was on screen before.
    fn follow(&mut self, len_before: usize) {
        let len = self.seen().len();
        if len != len_before && self.time.right >= len_before as f64 {
            self.time.right += len as f64 - len_before as f64;
        }
        self.clamp();
    }

    /// Whether some handler is animating and wants [`Event::Tick`]s. Backends send one per frame
    /// while this is `true`, and stay idle otherwise.
    pub fn needs_tick(&self) -> bool {
        self.animating() || self.sources.iter().any(|s| s.live)
    }

    /// Whether some handler animates: ticks are wanted every frame, not only to poll the sources.
    pub fn animating(&self) -> bool {
        self.handlers.iter().any(|h| h.animating)
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        self.size = (width, height);
        self.time.width = self.plot_size().0 as f64;
        self.clamp();
    }

    /// Size of the area series are drawn in, without the axes, in device pixels.
    pub fn plot_size(&self) -> (f32, f32) {
        let width = (self.size.0 - self.style.price_axis_width * self.scale).max(1.0);
        let height = (self.size.1 - self.style.time_axis_height * self.scale).max(1.0);
        (width, height)
    }

    /// Sets device pixels per logical pixel (2.0 on a typical HiDPI screen). Everything sized in
    /// logical pixels (axes, fonts, lines, bar spacing) grows with it; what is on screen stays in place.
    pub fn set_scale(&mut self, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self.time.bar_spacing *= (scale / self.scale) as f64;
        self.scale = scale;
        self.time.width = self.plot_size().0 as f64;
        self.clamp();
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn time_scale(&self) -> &TimeScale {
        &self.time
    }

    /// Puts the bars where `time` has them: its bar spacing and right edge (the width stays the chart's). Kept
    /// within the usual bounds.
    pub fn set_time_scale(&mut self, time: TimeScale) {
        if time.bar_spacing.is_finite() && time.bar_spacing > 0.0 && time.right.is_finite() {
            self.time.bar_spacing = time.bar_spacing;
            self.time.right = time.right;
            self.clamp();
        }
    }

    pub fn pointer(&self) -> Pointer {
        self.pointer
    }

    /// Everything the chart is allowed to see. Nothing downstream can look past the cursor.
    pub fn seen(&self) -> CandleView<'_> {
        view_of(&self.candles, &self.derived, self.cursor)
    }

    /// The opens of the first and the last bar of all the data, held back by a replay or not: where a replay
    /// can start. Only times: what the bars held back look like stays unseen.
    pub fn data_span(&self) -> Option<(Time, Time)> {
        let time = self.candles.view().time();
        Some((*time.first()?, *time.last()?))
    }

    /// The bars as they came, whatever timeframe is shown, up to the replay cursor: what orders meet.
    pub fn base(&self) -> CandleView<'_> {
        base_view_of(&self.candles, self.cursor)
    }

    /// The trades the chart may see: those before the first bar that is not shown, and before the moment a replay
    /// played to (all of them when nothing is held back).
    pub fn seen_trades(&self) -> TradeView<'_> {
        trades_of(&self.candles, &self.trades, self.cursor, self.moment)
    }

    /// Puts the replay cursor at the bar opening at `cursor` (shown complete), or ends the replay with `None`.
    pub fn set_cursor(&mut self, cursor: Option<Time>) {
        self.cursor = cursor;
        self.moment = None;
        self.reform();
        self.clamp();
    }

    /// Puts a replay that has its cursor at the moment it played to (see [`Cursor::play`]), as a saved session
    /// had it; `None` leaves it at the cursor's bar.
    pub fn set_moment(&mut self, moment: Option<Time>) {
        self.moment = moment.filter(|_| self.cursor.is_some());
        self.reform();
        self.clamp();
    }

    /// Where a replay that plays by time stands; `None` when it moved bar by bar or there is none.
    pub fn moment(&self) -> Option<Time> {
        self.moment
    }

    /// Whether the newest bar shown is still forming at the replay's moment: it holds only the trades so far.
    pub fn forming(&self) -> bool {
        self.held.is_some()
    }

    /// What an order placed now waits for, its `after`: the bars after the newest one shown, or, while that one is
    /// still forming, its trades to come (the trading handler looks at a forming bar's new trades only); on a live
    /// chart outside a replay, the trades after now.
    pub fn order_after(&self) -> Option<Time> {
        let newest = *self.base().time().last()?;
        Some(crate::builtin::order_after(
            newest,
            self.forming(),
            self.cursor().is_some(),
            self.resources(),
        ))
    }

    /// The period of the candles shown, in milliseconds; `None` when they are the data as it came.
    pub fn timeframe(&self) -> Option<i64> {
        self.timeframe
    }

    /// Shows candles of `span` milliseconds built from the data (`None`: the data as it came). During a
    /// replay the newest candle holds only the finer bars shown so far. The chart keeps looking at the
    /// same moment.
    pub fn set_timeframe(&mut self, span: Option<i64>) {
        self.timeframe = span.filter(|&s| s > 0);
        retime(
            &mut self.time,
            &mut self.derived,
            &self.candles,
            &self.trades,
            (self.cursor, self.moment),
            self.timeframe,
        );
        self.clamp();
    }

    /// The replay cursor: `Some(t)` shows only candles up to time `t`.
    pub fn cursor(&self) -> Option<Time> {
        self.cursor
    }

    /// What the pointer looks like now: see [`PointerShape`].
    pub fn pointer_shape(&self) -> PointerShape {
        self.shape
    }

    /// Whether a note on the chart is being typed into.
    pub fn takes_text(&self) -> bool {
        (self.resources.get::<crate::builtin::Drawings>()).is_some_and(|d| d.editing.is_some())
    }

    fn region_shape(&self) -> PointerShape {
        let Some((x, y)) = self.pointer.position else {
            return PointerShape::Default;
        };
        let (w, h) = self.plot_size();
        match (x < w, y < h) {
            (true, true) => PointerShape::Crosshair,
            (false, true) => PointerShape::ResizeVertical,
            (true, false) => PointerShape::ResizeHorizontal,
            (false, false) => PointerShape::Default,
        }
    }

    /// Applies input. Returns `true` if the picture changed and must be redrawn.
    pub fn handle(&mut self, event: Event) -> bool {
        let previous = self.pointer;
        self.pointer = match event {
            Event::PointerMoved { x, y } => Pointer {
                position: Some((x, y)),
                ..previous
            },
            Event::PointerDown => Pointer {
                pressed: previous.position.is_some(),
                ..previous
            },
            Event::PointerUp => Pointer {
                pressed: false,
                ..previous
            },
            Event::PointerLeft => Pointer {
                position: None,
                pressed: false,
                ..previous
            },
            Event::Modifiers { adding } => Pointer { adding, ..previous },
            Event::Wheel { .. }
            | Event::Key(_)
            | Event::Tick { .. }
            | Event::Command(_)
            | Event::SecondaryDown
            | Event::DoubleClick => previous,
        };
        // Where the pointer is decides its shape, unless a handler that would take a press there says otherwise.
        if matches!(
            event,
            Event::PointerMoved { .. } | Event::PointerDown | Event::PointerUp | Event::PointerLeft
        ) {
            self.shape = self.region_shape();
        }
        let before = self.time;
        let cursor_before = self.cursor;
        let mut commands = Vec::new();
        let mut edits = Vec::new();
        // New data first, so the handlers of a tick see it.
        let mut redraw = matches!(event, Event::Tick { .. }) && self.poll_sources();
        redraw |= self.dispatch(&event, previous, &mut commands, &mut edits);
        // Commands that handlers emitted (a button click, a key binding) go through the chain as
        // events of their own, and may emit more. The limit only stops a command that emits itself.
        let mut rounds = 0;
        while !commands.is_empty() && rounds < MAX_COMMAND_ROUNDS {
            rounds += 1;
            for name in std::mem::take(&mut commands) {
                redraw |= self.dispatch(&Event::Command(name), self.pointer, &mut commands, &mut edits);
            }
        }
        // Changes to the chart itself that handlers asked for, now that no handler is running.
        for edit in edits {
            edit(self);
            redraw = true;
        }
        redraw || self.time != before || self.cursor != cursor_before
    }

    /// Runs one event through the handler chain.
    fn dispatch(
        &mut self,
        event: &Event,
        previous: Pointer,
        commands: &mut Vec<&'static str>,
        edits: &mut Vec<ChartEdit>,
    ) -> bool {
        let len_before = self.seen().len();
        let plot = self.plot_size();
        let panes = self.pane_layout();
        let before = self.time;
        let mut redraw = false;
        let mut consumed = false;
        for entry in &mut self.handlers {
            if consumed && !entry.observer {
                continue;
            }
            let view = view_of(&self.candles, &self.derived, self.cursor);
            let base = base_view_of(&self.candles, self.cursor);
            let (cursor_at, moment_at, timeframe_at) = (self.cursor, self.moment, self.timeframe);
            let mut ctx = EventCtx {
                pointer: self.pointer,
                previous_pointer: previous,
                time: &mut self.time,
                view,
                base,
                trades: trades_of(&self.candles, &self.trades, self.cursor, self.moment),
                len: view.len(),
                panes: &panes,
                pane_stack: &mut self.panes,
                prices: &self.prices,
                cursor: Cursor::new(
                    &self.candles,
                    &self.trades,
                    &mut self.cursor,
                    &mut self.moment,
                    self.timeframe,
                ),
                timeframe: TimeframeCtl::new(base, &mut self.timeframe),
                plot,
                scale: self.scale,
                style: &self.style,
                res: &mut self.resources,
                commands,
                edits,
                shape: &mut self.shape,
            };
            let response = entry.handler.handle(event, &mut ctx);
            entry.animating = response.animate;
            redraw |= response.redraw;
            consumed |= response.consumed;
            if self.cursor != cursor_at || self.moment != moment_at {
                reform(
                    &mut self.candles,
                    &self.trades,
                    &mut self.cursor,
                    self.moment,
                    &mut self.held,
                    &mut self.derived,
                );
            }
            if self.timeframe != timeframe_at {
                retime(
                    &mut self.time,
                    &mut self.derived,
                    &self.candles,
                    &self.trades,
                    (self.cursor, self.moment),
                    self.timeframe,
                );
            }
        }
        // The handlers may have revealed bars: keep the latest one where it was on screen. Bars hidden (a replay
        // starting) leave the screen as it was, so the room after the last bar shown stays.
        let len = self.seen().len();
        if len > len_before && before.right >= len_before as f64 {
            self.time.right += len as f64 - len_before as f64;
        }
        self.time.clamp(len);
        redraw
    }

    fn clamp(&mut self) {
        let len = self.seen().len();
        self.time.clamp(len);
    }

    /// Rebuilds `scene`: the background, then every layer in order, each one in its own scene layer.
    /// The panes come one after the other, each with the layers that belong to it.
    pub fn build_scene(&mut self, scene: &mut Scene) {
        scene.clear();
        scene.background = self.style.background;
        let plot = self.plot_size();
        let panes = self.pane_layout();
        let view = view_of(&self.candles, &self.derived, self.cursor);
        let visible = self.time.visible(view.len());
        let axis_height = self.size.1 - plot.1;
        self.prices.clear();

        for (index, rect) in panes.iter().enumerate() {
            // Autoscale to what the layers of this pane ask for.
            let extent = self
                .layers
                .iter()
                .filter(|e| e.is_in(index, &self.panes))
                .filter_map(|e| e.layer.price_extent(view, visible.clone(), &self.resources))
                .filter(|(low, high)| low.is_finite() && high.is_finite())
                .reduce(|(a, b), (low, high)| (a.min(low), b.max(high)));
            let mode = match self.panes.scale(index) {
                ScaleSetting::Linear => PriceMode::Linear,
                ScaleSetting::Log => PriceMode::Log,
                ScaleSetting::Percent => PriceMode::Percent {
                    base: view
                        .close()
                        .get(visible.start.min(view.len().saturating_sub(1)))
                        .copied()
                        .unwrap_or(1.0),
                },
            };
            let mut price = PriceScale::fit_mode(
                extent.unwrap_or((0.0, 1.0)),
                rect.height as f64,
                self.style.price_margin,
                mode,
            );
            if let Some((min, max)) = self.panes.manual_range(index) {
                (price.min, price.max) = (min, max);
            }
            self.prices.push((!view.is_empty()).then_some(price));

            let last = index + 1 == panes.len();
            let size = (self.size.0, rect.height + if last { axis_height } else { 0.0 });
            let pointer = Pointer {
                position: self.pointer.position.map(|(x, y)| (x, y - rect.top)),
                ..self.pointer
            };
            let info = PaneInfo {
                name: self.panes.name(index).unwrap_or(names::MAIN_PANE),
                index,
                count: panes.len(),
                top: rect.top,
                chart_plot: plot,
                main_height: panes[0].height,
            };
            for entry in self.layers.iter_mut().filter(|e| e.is_in(index, &self.panes)) {
                let clip = match entry.layer.clip() {
                    Clip::Plot => Some((plot.0, rect.height)),
                    Clip::Pane => Some(size),
                    Clip::None => None,
                };
                scene.set_clip(clip.map(|(w, h)| ClipRect {
                    x: 0.0,
                    y: rect.top,
                    w,
                    h,
                }));
                let mark = scene.mark();
                let mut ctx = DrawCtx {
                    view,
                    trades: trades_of(&self.candles, &self.trades, self.cursor, self.moment),
                    visible: visible.clone(),
                    time: &self.time,
                    price: &price,
                    size,
                    plot: (plot.0, rect.height),
                    pane: info,
                    scale: self.scale,
                    pointer,
                    style: &self.style,
                    res: &self.resources,
                    scene,
                };
                entry.layer.draw(&mut ctx);
                if rect.top != 0.0 {
                    scene.shift_down_from(mark, rect.top);
                }
                scene.next_layer();
            }
        }
        // Drawings and indicators keep the palette's colors as saved; a light look draws them deeper.
        if self.style.look != Look::Dark {
            let look = self.style.look;
            scene.recolor(|c| look.paint(c));
        }
    }
}

/// The data as it came, as far as the replay cursor lets it be seen.
fn base_view_of(candles: &Candles, cursor: Option<Time>) -> CandleView<'_> {
    match cursor {
        Some(t) => candles.view().until(t),
        None => candles.view(),
    }
}

/// What is drawn: the candles of the chosen timeframe, or the data itself. Both stop at the cursor.
fn view_of<'a>(candles: &'a Candles, derived: &'a Option<Resampled>, cursor: Option<Time>) -> CandleView<'a> {
    match derived {
        Some(derived) => derived.candles().view(),
        None => base_view_of(candles, cursor),
    }
}

/// Brings the shown candles of a timeframe up to date with the data, the tape and the replay: of a period shorter
/// than the data's own, from the trades seen.
fn sync(derived: &mut Option<Resampled>, candles: &Candles, trades: &Trades, (cursor, moment): Replayed) {
    let Some(derived) = derived else {
        return;
    };
    if crate::native_step(candles.view()).is_some_and(|step| derived.span() < step) {
        derived.sync_trades(trades_of(candles, trades, cursor, moment));
    } else {
        derived.sync(base_view_of(candles, cursor));
    }
}

/// The replay cursor and moment.
type Replayed = (Option<Time>, Option<Time>);

/// The bar at the right edge of `view` and the room after it.
fn anchor(time: &TimeScale, view: CandleView<'_>) -> Option<(Time, f64)> {
    (!view.is_empty()).then(|| {
        let index = (time.right.floor().max(1.0) as usize).min(view.len()) - 1;
        (view.time()[index], time.right - (index + 1) as f64)
    })
}

/// Puts the bar of `view` at or before the anchor's time at the right edge, with the same room after it.
fn restore(time: &mut TimeScale, view: CandleView<'_>, (at, room): (Time, f64)) {
    let index = view.time().partition_point(|&x| x <= at).max(1) - 1;
    time.right = (index + 1) as f64 + room;
}

/// Switches the shown candles to `timeframe` and moves the time scale so that the moment at its right
/// edge stays there: the same bar (or the period that holds it), with the same room after it.
fn retime(
    time: &mut TimeScale,
    derived: &mut Option<Resampled>,
    candles: &Candles,
    trades: &Trades,
    replayed: Replayed,
    timeframe: Option<i64>,
) {
    let old = anchor(time, view_of(candles, derived, replayed.0));
    *derived = timeframe.map(Resampled::new);
    sync(derived, candles, trades, replayed);
    if let Some(old) = old {
        restore(time, view_of(candles, derived, replayed.0), old);
    }
}

/// Cuts the data at a replay's moment again, after it or the tape changed: the bar forming then holds what the
/// trades before the moment made of it (its real values kept in `held`), and the candles of the timeframe shown
/// follow. Without a moment only the bar held gets its values back.
fn reform(
    candles: &mut Candles,
    trades: &Trades,
    cursor: &mut Option<Time>,
    moment: Option<Time>,
    held: &mut Option<(usize, Candle)>,
    derived: &mut Option<Resampled>,
) {
    if let Some((index, real)) = held.take() {
        candles.set_values(index, real);
    }
    if let (Some(moment), Some(_)) = (moment, *cursor) {
        let (shown, forming) = crate::cursor::cut(candles, trades, moment);
        let time = candles.view().time();
        *cursor = time.get(shown.max(1) - 1).copied().or(*cursor);
        if forming {
            let index = shown - 1;
            let open = time[index];
            let tape = trades.view().between(open, moment);
            let price = tape.price();
            let partial = Candle {
                time: open,
                open: price[0],
                high: price.iter().copied().fold(f64::MIN, f64::max),
                low: price.iter().copied().fold(f64::MAX, f64::min),
                close: price[price.len() - 1],
                volume: tape.qty().iter().sum(),
            };
            let real = candles.view().get(index).expect("in range");
            candles.set_values(index, partial);
            *held = Some((index, real));
        }
    }
    sync(derived, candles, trades, (*cursor, moment));
}

/// The tape as far as the candles go: up to the open of the first candle that is held back, and before the moment
/// a replay played to.
fn trades_of<'a>(candles: &Candles, trades: &'a Trades, cursor: Option<Time>, moment: Option<Time>) -> TradeView<'a> {
    let view = trades.view();
    let Some(t) = cursor else { return view };
    let view = match candles.view().time().get(candles.view().until(t).len()) {
        Some(&next) => view.before(next),
        None => view,
    };
    match moment {
        Some(moment) => view.before(moment),
        None => view,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Align;

    fn candles(n: i64, price: impl Fn(i64) -> f64) -> Candles {
        let mut c = Candles::new();
        for i in 0..n {
            let p = price(i);
            c.push(Candle {
                time: Time(i * 60_000),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p + 0.5,
                volume: 1.0,
            })
            .unwrap();
        }
        c
    }

    fn chart(n: i64) -> Chart {
        let mut chart = Chart::new(candles(n, |i| 100.0 + (i % 10) as f64));
        chart.resize(872.0, 528.0); // plot 800 x 500
        chart
    }

    /// Without the last price, whose label is in the colors of the crosshair's and does not sit on a grid line.
    fn bare_chart(n: i64) -> Chart {
        let mut chart = chart(n);
        chart.remove_layer(names::LAST_PRICE);
        chart
    }

    #[test]
    fn starts_at_the_latest_bars_with_a_margin() {
        let c = chart(1000);
        assert_eq!(c.plot_size(), (800.0, 500.0));
        assert_eq!(c.time_scale().right, 1005.0);
        // 800 px / 8 px = 100 bars across, the last 5 of them are the empty margin.
        assert_eq!(c.time_scale().visible(1000), 905..1000);
    }

    #[test]
    fn history_goes_in_where_the_data_have_none_and_the_bar_at_the_right_edge_stays() {
        let all = candles(1000, |i| 100.0 + (i % 10) as f64);
        let piece = |range: core::ops::Range<usize>| {
            let mut piece = Candles::new();
            for i in range {
                piece.push(all.view().get(i).unwrap()).unwrap();
            }
            piece
        };
        let mut c = Chart::new(piece(600..1000));
        c.resize(872.0, 528.0);
        c.set_timeframe(Some(5 * 60_000));
        c.time.right -= 20.0;
        let before = anchor(&c.time, c.seen());
        // The older pieces come in any order, the last overlapping what is there.
        c.merge(&piece(0..200));
        assert_eq!(c.base().len(), 600);
        c.merge(&piece(150..650));
        assert_eq!(c.base().len(), 1000, "the gap is filled, the overlap left out");
        assert!(c.base().time().windows(2).all(|w| w[0] < w[1]));
        assert_eq!(c.seen().len(), 200, "the timeframe shown is made of all of them");
        assert_eq!(anchor(&c.time, c.seen()), before);
    }

    #[test]
    fn dragging_pans_only_while_pressed() {
        let mut c = chart(1000);
        c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
        c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
        assert_eq!(c.time_scale().right, 1005.0, "hover must not pan");

        c.handle(Event::PointerDown);
        assert!(c.handle(Event::PointerMoved { x: 380.0, y: 100.0 }));
        // 80 px right = 10 bars of 8 px towards older data.
        assert_eq!(c.time_scale().right, 995.0);
        c.handle(Event::PointerUp);
        c.handle(Event::PointerMoved { x: 100.0, y: 100.0 });
        assert_eq!(c.time_scale().right, 995.0);
    }

    #[test]
    fn cannot_drag_past_the_data() {
        let mut c = chart(100);
        c.handle(Event::PointerMoved { x: 0.0, y: 0.0 });
        c.handle(Event::PointerDown);
        c.handle(Event::PointerMoved { x: 1e6, y: 0.0 });
        assert_eq!(c.time_scale().right, 50.0);
        c.handle(Event::PointerMoved { x: -1e6, y: 0.0 });
        assert_eq!(c.time_scale().right, 100.0 + 50.0);
    }

    #[test]
    fn wheel_zooms_around_the_pointer() {
        let mut c = chart(1000);
        let anchor = c.time_scale().index_of(200.0);
        assert!(c.handle(Event::Wheel { x: 200.0, delta: 1.0 }));
        assert!(c.time_scale().bar_spacing > 8.0);
        assert!((c.time_scale().index_of(200.0) - anchor).abs() < 1e-9);
    }

    #[test]
    fn wheel_at_the_zoom_limit_reports_no_change() {
        let mut c = chart(10);
        // Everything already fits at max zoom out for 10 bars only after zooming out a lot.
        for _ in 0..200 {
            c.handle(Event::Wheel { x: 100.0, delta: -1.0 });
        }
        assert!(!c.handle(Event::Wheel { x: 100.0, delta: -1.0 }));
    }

    #[test]
    fn scene_stays_small_when_zoomed_out_on_a_lot_of_data() {
        let mut c = chart(1_000_000);
        for _ in 0..200 {
            c.handle(Event::Wheel { x: 400.0, delta: -1.0 });
        }
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        // All the data is on screen, yet there are only a couple of rects per pixel column.
        let candle_rects = scene
            .rects
            .iter()
            .filter(|r| r.color == c.style.candle.up || r.color == c.style.candle.down);
        let n = candle_rects.count();
        assert!(n > 400, "candles vanished: {n} rects");
        assert!(n < 3 * 800, "{n} rects");
    }

    #[test]
    fn autoscale_ignores_candles_after_the_cursor() {
        // Quiet market for 50 bars, then a spike to 10 000.
        let data = candles(100, |i| if i < 50 { 100.0 } else { 10_000.0 });
        let mut c = Chart::new(data);
        c.resize(872.0, 528.0);
        c.set_cursor(Some(Time(49 * 60_000)));
        assert_eq!(c.seen().len(), 50);

        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let up = c.style.candle.up;
        let down = c.style.candle.down;
        let tallest_wick = scene
            .rects
            .iter()
            .filter(|r| r.w == 1.0 && (r.color == up || r.color == down))
            .map(|r| r.h)
            .fold(0.0, f32::max);
        // A 2-point wick fills a large part of the plot, which is only possible if the
        // scale never saw the spike (with it the wick would shrink to the 1 px minimum).
        assert!(tallest_wick > 100.0, "wick is {tallest_wick} px");
    }

    #[test]
    fn price_labels_sit_on_grid_lines_right_of_the_plot() {
        let mut c = bare_chart(1000);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let (plot_w, plot_h) = c.plot_size();

        let prices: Vec<_> = scene.texts.iter().filter(|t| t.x > plot_w).collect();
        assert!(prices.len() >= 4, "{} price labels", prices.len());
        for t in &prices {
            assert_eq!(t.align, Align::Left);
            assert!(t.y > 0.0 && t.y < plot_h, "label y {}", t.y);
            let value: f64 = t.text.parse().expect("a number");
            assert!((97.0..=113.0).contains(&value), "{value}"); // the data lives in 99..111, plus margin
            // A grid line runs through the label's y.
            assert!(
                scene.rects.iter().any(|r| r.h == 1.0 && r.w == plot_w && r.y == t.y),
                "no line at {}",
                t.y
            );
        }
        // Labels use one number of decimals and are in increasing price order from the bottom.
        let decimals = |s: &str| s.split('.').nth(1).map_or(0, str::len);
        assert!(prices.windows(2).all(|w| decimals(&w[0].text) == decimals(&w[1].text)));
        assert!(prices.windows(2).all(|w| w[0].y > w[1].y));
    }

    #[test]
    fn time_labels_are_under_the_plot() {
        let mut c = chart(1000);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let (plot_w, plot_h) = c.plot_size();
        let times: Vec<_> = scene
            .texts
            .iter()
            .filter(|t| t.x <= plot_w && t.color == c.style.axis_text)
            .collect();
        assert!(times.len() >= 4, "{} time labels", times.len());
        for t in &times {
            assert_eq!(t.align, Align::Center);
            assert!(t.y > plot_h && t.y < c.size.1, "label y {}", t.y);
            // Bars are one minute apart starting at the epoch: "HH:MM" or a day number.
            assert!(t.text.contains(':') || t.text.parse::<u32>().is_ok(), "{}", t.text);
        }
    }

    fn crosshair_texts(scene: &Scene, c: &Chart) -> Vec<String> {
        let color = c.style.crosshair_label_text;
        scene
            .texts
            .iter()
            .filter(|t| t.color == color)
            .map(|t| t.text.clone())
            .collect()
    }

    #[test]
    fn crosshair_snaps_to_the_bar_and_labels_price_and_time() {
        let mut c = bare_chart(1000);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        assert!(crosshair_texts(&scene, &c).is_empty(), "no pointer, no crosshair");

        // x = 400 is inside the bar with index floor(1005 + (400 - 800) / 8) = 955.
        assert!(
            c.handle(Event::PointerMoved { x: 400.0, y: 250.0 }),
            "the crosshair moved: redraw"
        );
        c.build_scene(&mut scene);
        let texts = crosshair_texts(&scene, &c);
        assert_eq!(texts.len(), 2, "{texts:?}");
        let value: f64 = texts[0].parse().expect("price");
        assert!((97.0..=113.0).contains(&value));
        // Bar 955 is 955 minutes after the epoch: 15:55 on 1 Jan 1970.
        assert_eq!(texts[1], "1 Jan '70 15:55");

        // The vertical line runs through the middle of that bar.
        let x = c.time_scale().x_of(955.5).floor() as f32;
        assert!(
            scene
                .rects
                .iter()
                .any(|r| r.color == c.style.crosshair && r.x == x && r.w == 1.0 && r.h > 1.0)
        );
        // The label box is a later scene layer than the axes, so it covers their labels.
        let layers = scene.layers();
        let box_index = scene
            .rects
            .iter()
            .position(|r| r.color == c.style.crosshair_label)
            .unwrap();
        let box_layer = layers.iter().position(|l| l.rects.contains(&box_index)).unwrap();
        let axis_label = scene.texts.iter().position(|t| t.color == c.style.axis_text).unwrap();
        let axis_layer = layers.iter().position(|l| l.texts.contains(&axis_label)).unwrap();
        assert!(box_layer > axis_layer);
    }

    #[test]
    fn axis_labels_under_the_crosshair_labels_are_left_out_not_half_covered() {
        let mut c = chart(1000);
        let mut scene = Scene::new();
        let axis_texts = |scene: &Scene, c: &Chart| scene.texts.iter().filter(|t| t.color == c.style.axis_text).count();
        c.build_scene(&mut scene);
        let without = axis_texts(&scene, &c);

        // Put the pointer exactly on a price tick and on a time tick.
        let (plot_w, _) = c.plot_size();
        let price_label_y = scene.texts.iter().find(|t| t.x > plot_w).unwrap().y;
        let time_label_x = scene.texts.iter().find(|t| t.x <= plot_w).unwrap().x;
        c.handle(Event::PointerMoved {
            x: time_label_x,
            y: price_label_y,
        });
        c.build_scene(&mut scene);
        assert!(
            axis_texts(&scene, &c) <= without - 2,
            "{} -> {}",
            without,
            axis_texts(&scene, &c)
        );
        let boxes: Vec<_> = scene
            .rects
            .iter()
            .filter(|r| r.color == c.style.crosshair_label)
            .collect();
        assert_eq!(boxes.len(), 2);
        for t in scene.texts.iter().filter(|t| t.color == c.style.axis_text) {
            let w = tickerlight_text::Font::ui().measure(&t.text, t.size);
            let left = match t.align {
                Align::Left => t.x,
                Align::Center => t.x - w / 2.0,
                Align::Right => t.x - w,
            };
            for b in &boxes {
                let overlaps =
                    left < b.x + b.w && left + w > b.x && t.y + t.size / 2.0 > b.y && t.y - t.size / 2.0 < b.y + b.h;
                assert!(!overlaps, "axis label {:?} overlaps a crosshair box {b:?}", t.text);
            }
        }
    }

    #[test]
    fn crosshair_over_the_empty_margin_has_no_time_and_no_vertical_line() {
        let mut c = bare_chart(1000);
        // Bars end at x = 800 - 5 * 8 = 760; x = 790 is in the margin.
        c.handle(Event::PointerMoved { x: 790.0, y: 250.0 });
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        assert_eq!(crosshair_texts(&scene, &c).len(), 1, "only the price label");
        assert!(
            !scene
                .rects
                .iter()
                .any(|r| r.color == c.style.crosshair && r.w == 1.0 && r.h > 1.0 && r.h <= 4.0)
        );
    }

    #[test]
    fn crosshair_disappears_when_the_pointer_leaves_or_is_over_the_axis() {
        let mut c = bare_chart(1000);
        let mut scene = Scene::new();
        c.handle(Event::PointerMoved { x: 400.0, y: 250.0 });
        assert!(
            c.handle(Event::PointerMoved { x: 850.0, y: 250.0 }),
            "leaving the plot redraws"
        );
        c.build_scene(&mut scene);
        assert!(crosshair_texts(&scene, &c).is_empty(), "over the price axis");

        c.handle(Event::PointerMoved { x: 400.0, y: 250.0 });
        assert!(c.handle(Event::PointerLeft));
        assert!(!c.handle(Event::PointerLeft), "nothing to redraw the second time");
        c.build_scene(&mut scene);
        assert!(crosshair_texts(&scene, &c).is_empty());
    }

    #[test]
    fn hover_outside_the_plot_does_not_ask_for_a_redraw() {
        let mut c = chart(1000);
        assert!(!c.handle(Event::PointerMoved { x: 850.0, y: 100.0 }));
        assert!(!c.handle(Event::PointerMoved { x: 860.0, y: 120.0 }));
    }

    #[test]
    fn crosshair_never_labels_bars_after_the_replay_cursor() {
        let mut c = bare_chart(1000);
        c.set_cursor(Some(Time(499 * 60_000)));
        assert_eq!(c.seen().len(), 500);
        // After the cursor cut the data the view scrolled back to the end of it (bar 500 + margin),
        // so x = 790 is over bar 548 of the full data, which the chart must not know about.
        c.handle(Event::PointerMoved { x: 790.0, y: 250.0 });
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        assert_eq!(crosshair_texts(&scene, &c).len(), 1, "a price label but no time label");

        // Over the last visible bar the label is there and shows 08:19 (499 minutes).
        let x = c.time_scale().x_of(499.5) as f32;
        c.handle(Event::PointerMoved { x, y: 250.0 });
        c.build_scene(&mut scene);
        assert_eq!(crosshair_texts(&scene, &c)[1], "1 Jan '70 08:19");
    }

    #[test]
    fn hidpi_scale_keeps_the_picture_and_doubles_the_pixels() {
        let normal = chart(1000);
        let mut hidpi = Chart::new(candles(1000, |i| 100.0 + (i % 10) as f64));
        hidpi.set_scale(2.0);
        hidpi.resize(2.0 * 872.0, 2.0 * 528.0);

        assert_eq!(hidpi.plot_size(), (1600.0, 1000.0));
        // The same bars are on screen, just drawn with twice the pixels.
        assert_eq!(hidpi.time_scale().visible(1000), normal.time_scale().visible(1000));
        assert_eq!(hidpi.time_scale().bar_spacing, 16.0);

        let mut scene = Scene::new();
        hidpi.build_scene(&mut scene);
        assert!(
            scene.texts.iter().all(|t| t.size == 24.0),
            "fonts are 12 logical px = 24 device px"
        );
        let (plot_w, _) = hidpi.plot_size();
        let grid_lines: Vec<_> = scene
            .rects
            .iter()
            .filter(|r| r.color == hidpi.style.grid && r.w == plot_w)
            .collect();
        assert!(
            !grid_lines.is_empty() && grid_lines.iter().all(|r| r.h == 2.0),
            "grid lines are 2 px thick"
        );
        let wicks = scene
            .rects
            .iter()
            .filter(|r| r.color == hidpi.style.candle.up || r.color == hidpi.style.candle.down);
        assert!(wicks.clone().any(|r| r.w == 2.0), "wicks are 2 px wide");
    }

    #[test]
    fn hidpi_has_the_same_number_of_labels() {
        let mut normal_scene = Scene::new();
        chart(1000).build_scene(&mut normal_scene);
        let mut hidpi = Chart::new(candles(1000, |i| 100.0 + (i % 10) as f64));
        hidpi.set_scale(2.0);
        hidpi.resize(2.0 * 872.0, 2.0 * 528.0);
        let mut hidpi_scene = Scene::new();
        hidpi.build_scene(&mut hidpi_scene);
        assert_eq!(hidpi_scene.texts.len(), normal_scene.texts.len());
        let texts = |s: &Scene| s.texts.iter().map(|t| t.text.clone()).collect::<Vec<_>>();
        assert_eq!(
            texts(&hidpi_scene),
            texts(&normal_scene),
            "the same labels, only bigger"
        );
    }

    #[test]
    fn changing_the_scale_later_keeps_what_is_on_screen() {
        let mut c = chart(1000);
        let before = c.time_scale().visible(1000);
        c.set_scale(2.0);
        c.resize(2.0 * 872.0, 2.0 * 528.0); // the window did not change, only the pixel density
        assert_eq!(c.time_scale().visible(1000), before);
        c.set_scale(1.0);
        c.resize(872.0, 528.0);
        assert_eq!(c.time_scale().visible(1000), before);
        assert_eq!(c.time_scale().bar_spacing, 8.0);
    }

    #[test]
    fn nonsense_scale_falls_back_to_one() {
        let mut c = chart(10);
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            c.set_scale(bad);
            assert_eq!(c.plot_size(), (800.0, 500.0), "scale {bad}");
        }
    }

    #[test]
    fn empty_chart_builds_a_scene_without_panicking() {
        let mut c = Chart::new(Candles::new());
        c.resize(872.0, 528.0);
        c.handle(Event::Wheel { x: 10.0, delta: 3.0 });
        c.handle(Event::PointerDown);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        // No data: no candles, but the frame (grid, axes) is still there.
        let candle_colors = [c.style.candle.up, c.style.candle.down];
        assert!(!scene.rects.iter().any(|r| candle_colors.contains(&r.color)));
    }
}
