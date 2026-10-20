//! The extension API. Everything the chart draws or reacts to, built-in features included,
//! is a [`Layer`] or a [`Handler`] added by a [`Plugin`], so anything can be replaced or removed.

use core::any::{Any, TypeId};
use core::ops::Range;
use std::collections::HashMap;

use crate::{CandleView, ChartStyle, Cursor, Event, PaneStack, PriceScale, Scene, TimeScale, TimeframeCtl, TradeView};

/// Typed values plugins keep on the chart and share with each other: one value per type.
#[derive(Default)]
pub struct Resources {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Resources {
    /// Stores `value`, returning the previous value of that type.
    pub fn insert<T: Any>(&mut self, value: T) -> Option<T> {
        let old = self.map.insert(TypeId::of::<T>(), Box::new(value))?;
        old.downcast().ok().map(|b| *b)
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.map.get(&TypeId::of::<T>())?.downcast_ref()
    }

    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.map.get_mut(&TypeId::of::<T>())?.downcast_mut()
    }

    pub fn remove<T: Any>(&mut self) -> Option<T> {
        self.map.remove(&TypeId::of::<T>())?.downcast().ok().map(|b| *b)
    }

    pub fn contains<T: Any>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<T>())
    }
}

/// Where the pointer is, whether its button is down and which keys modify a click. Positions are device pixels of
/// the chart.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pointer {
    /// `None` while the pointer is outside the chart.
    pub position: Option<(f32, f32)>,
    /// The main button is held (and was pressed over the chart).
    pub pressed: bool,
    /// Shift, Ctrl or Cmd is held: a click adds to what is selected instead of replacing it.
    pub adding: bool,
}

/// What the mouse pointer looks like: what a press there would do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PointerShape {
    #[default]
    Default,
    Crosshair,
    /// Something to click.
    Hand,
    /// Text to type in or select.
    Text,
    /// A drag up and down: a price axis, a pane separator, an order line.
    ResizeVertical,
    /// A drag left and right: the time axis.
    ResizeHorizontal,
    /// The chart is being dragged.
    Grabbing,
    /// Something that a drag moves: a drawing's point.
    Move,
}

/// Where a layer is drawn: the panes of a chart are stacked, each with its own price scale and price
/// axis, and all sharing the time axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// The main pane, the one with the candles.
    Main,
    /// The pane with this name (see [`Chart::add_pane`](crate::Chart::add_pane)).
    One(&'static str),
    /// Every pane: for what each pane needs its own copy of, such as the grid and the axes.
    All,
}

/// Where a pane is: the plot part of it (without its price axis), in device pixels of the chart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    pub top: f32,
    pub height: f32,
}

impl PaneRect {
    pub fn contains_y(&self, y: f32) -> bool {
        (self.top..self.top + self.height).contains(&y)
    }
}

/// What a layer is cut to when drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clip {
    /// The plot of the pane: the default, so a series or an indicator never spills over an axis or into the next pane.
    #[default]
    Plot,
    /// The whole pane, axes included: for layers that put labels on the axes.
    Pane,
    /// Not at all.
    None,
}

/// Which pane a [`DrawCtx`] is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneInfo {
    pub name: &'static str,
    /// Index of the pane, top to bottom, and how many panes there are.
    pub index: usize,
    pub count: usize,
    /// Top of the pane's plot in chart coordinates. Everything a layer draws is in pane coordinates
    /// (`0, 0` is the top left of the pane's plot); the chart moves it into place.
    pub top: f32,
    /// The plot of all panes together, in device pixels.
    pub chart_plot: (f32, f32),
    /// Height of the main pane's plot, where the toolbar sits.
    pub main_height: f32,
}

impl PaneInfo {
    pub fn is_first(&self) -> bool {
        self.index == 0
    }

    /// The last pane is the one with the time axis under it.
    pub fn is_last(&self) -> bool {
        self.index + 1 == self.count
    }
}

/// What a [`Layer`] gets to draw a frame.
///
/// `view` is the only window onto the data, and it never reaches past the replay cursor:
/// a layer cannot look into the future even by mistake.
pub struct DrawCtx<'a> {
    /// The candles the chart may see.
    pub view: CandleView<'a>,
    /// The tape, cut where the candles are: the trades that happened before the first bar that is
    /// not shown. Empty if the chart has no trades.
    pub trades: TradeView<'a>,
    /// Indices in `view` of the bars that are on screen (partly visible ones included).
    pub visible: Range<usize>,
    pub time: &'a TimeScale,
    /// Autoscaled to the price extents the layers asked for. A placeholder `0..1` if nobody
    /// had anything to show, so layers should check `view` for emptiness.
    pub price: &'a PriceScale,
    /// The pane in device pixels: its width, and its height with the time axis if it is the last
    /// pane. The plot (without axes) is at its top left.
    pub size: (f32, f32),
    pub plot: (f32, f32),
    pub pane: PaneInfo,
    /// Device pixels per logical pixel.
    pub scale: f32,
    /// In pane coordinates: `y` is measured from the top of this pane's plot, so it is negative over the
    /// panes above and past `plot.1` over the ones below. See [`chart_pointer`](Self::chart_pointer).
    pub pointer: Pointer,
    pub style: &'a ChartStyle,
    pub res: &'a Resources,
    /// Everything drawn here goes into one scene layer above the layers before it.
    pub scene: &'a mut Scene,
}

impl DrawCtx<'_> {
    /// The pointer in chart coordinates, whichever pane it is over.
    pub fn chart_pointer(&self) -> Option<(f32, f32)> {
        self.pointer.position.map(|(x, y)| (x, y + self.pane.top))
    }

    /// The pointer if it is over the plot of this pane.
    pub fn pointer_in_plot(&self) -> Option<(f32, f32)> {
        self.pointer.position.filter(|&p| self.over_plot(p))
    }

    pub fn over_plot(&self, (x, y): (f32, f32)) -> bool {
        (0.0..self.plot.0).contains(&x) && (0.0..self.plot.1).contains(&y)
    }

    /// A length in logical pixels as device pixels.
    pub fn px(&self, logical: f32) -> f32 {
        logical * self.scale
    }

    /// Thickness of thin lines in device pixels: at least one, whole.
    pub fn line(&self) -> f32 {
        self.scale.round().max(1.0)
    }
}

/// One thing drawn on the chart. Layers are drawn in ascending order, later ones on top.
pub trait Layer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>);

    /// The `(low, high)` prices this layer needs on screen for the bars in `visible`, if any.
    /// The chart autoscales to the union over all layers, so an indicator drawn above the candles
    /// is never cut off by a scale fitted to the candles alone.
    fn price_extent(&self, _view: CandleView<'_>, _visible: Range<usize>, _res: &Resources) -> Option<(f64, f64)> {
        None
    }

    /// What the layer is cut to. The plot of its pane unless it says otherwise.
    fn clip(&self) -> Clip {
        Clip::Plot
    }
}

/// What a [`Handler`] did with an event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Response {
    /// The picture changed and must be redrawn.
    pub redraw: bool,
    /// Later handlers do not see the event.
    pub consumed: bool,
    /// The handler keeps animating: the backend should keep sending [`Event::Tick`] (about once per
    /// frame) for as long as this is `true`. Reported again after every call of the handler.
    pub animate: bool,
}

impl Response {
    pub const IGNORED: Self = Self {
        redraw: false,
        consumed: false,
        animate: false,
    };
    pub const REDRAW: Self = Self {
        redraw: true,
        consumed: false,
        animate: false,
    };
    pub const CONSUMED: Self = Self {
        redraw: false,
        consumed: true,
        animate: false,
    };
    pub const CONSUMED_REDRAW: Self = Self {
        redraw: true,
        consumed: true,
        animate: false,
    };

    /// The same response, but asking for [`Event::Tick`]s to continue.
    pub const fn animating(mut self) -> Self {
        self.animate = true;
        self
    }
}

/// What a [`Handler`] gets with an event. Changes to `time` (pan, zoom) are kept within bounds
/// and count as a redraw by themselves.
pub struct EventCtx<'a> {
    /// The pointer after the event, and before it.
    pub pointer: Pointer,
    pub previous_pointer: Pointer,
    pub time: &'a mut TimeScale,
    /// The candles the chart may see, as of before this handler ran (the same bounded view layers get).
    pub view: CandleView<'a>,
    /// The data as it came, up to the same point. It is `view` unless the chart shows a higher timeframe; the replay
    /// and the trading work on these bars, however the chart is drawn.
    pub base: CandleView<'a>,
    /// The trades up to the same point.
    pub trades: TradeView<'a>,
    /// Number of bars in `view`.
    pub len: usize,
    /// Where the panes are, top to bottom, and the price scale each had in the last frame drawn (if
    /// it showed any bars; it lags one frame behind changes). `plot` is all of them together.
    pub panes: &'a [PaneRect],
    pub prices: &'a [Option<PriceScale>],
    /// The panes and their weights: change them to resize.
    pub pane_stack: &'a mut PaneStack,
    /// The replay cursor.
    pub cursor: Cursor<'a>,
    /// The period of the candles shown.
    pub timeframe: TimeframeCtl<'a>,
    pub plot: (f32, f32),
    pub scale: f32,
    pub style: &'a ChartStyle,
    pub res: &'a mut Resources,
    /// Commands to run after this event, in order. Use [`emit`](Self::emit).
    pub commands: &'a mut Vec<&'static str>,
    /// Changes to the chart to make after this event. Use [`defer`](Self::defer).
    pub edits: &'a mut Vec<ChartEdit>,
    /// The pointer's shape after this event: the chart sets it by where the pointer is (crosshair over the plot,
    /// resize over the axes), and a handler that would take a press there sets its own.
    pub shape: &'a mut PointerShape,
}

/// A change to the chart a handler asks for: adding or removing panes and layers, which no handler can do while
/// the chart runs it.
pub type ChartEdit = Box<dyn FnOnce(&mut crate::Chart)>;

impl EventCtx<'_> {
    /// Asks the chart to send `Event::Command(name)` through the handlers once this event is done.
    pub fn emit(&mut self, name: &'static str) {
        self.commands.push(name);
    }

    /// Runs `edit` on the chart once this event and the commands it emitted are done.
    pub fn defer(&mut self, edit: impl FnOnce(&mut crate::Chart) + 'static) {
        self.edits.push(Box::new(edit));
    }

    /// The pane under the pointer, if it is over a plot.
    pub fn pointer_pane(&self) -> Option<usize> {
        let (x, y) = self.pointer.position?;
        (0.0..self.plot.0).contains(&x).then_some(())?;
        self.panes.iter().position(|p| p.contains_y(y))
    }

    /// The price under the pointer in the pane it is over, if a frame has been drawn.
    pub fn pointer_price(&self) -> Option<f64> {
        let (_, y) = self.pointer.position?;
        let pane = self.pointer_pane()?;
        Some(
            self.prices
                .get(pane)
                .copied()
                .flatten()?
                .price_of((y - self.panes[pane].top) as f64),
        )
    }

    pub fn over_plot(&self, (x, y): (f32, f32)) -> bool {
        (0.0..self.plot.0).contains(&x) && (0.0..self.plot.1).contains(&y)
    }
}

/// Reacts to input. Handlers run in ascending order until one consumes the event.
pub trait Handler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response;

    /// Whether the handler wants to see every event, also the ones an earlier handler consumed.
    /// Only for observers, which must not consume events themselves.
    fn observes_all(&self) -> bool {
        false
    }
}

/// A bundle of layers, handlers and resources that adds a feature to a chart.
pub trait Plugin {
    fn build(&self, chart: &mut crate::Chart);
}

/// Names of the built-in commands. Keys and buttons are only ways to send these; the features listen.
pub mod commands {
    /// Start the replay at the right edge of the screen, or leave it.
    pub const REPLAY_TOGGLE: &str = "replay.toggle";
    /// The same, but start under the pointer if it is over a bar.
    pub const REPLAY_TOGGLE_AT_POINTER: &str = "replay.toggle-at-pointer";
    /// Choose where the replay starts: the next click on a plot starts it at that bar (again: stop choosing).
    pub const REPLAY_PICK: &str = "replay.pick";
    pub const REPLAY_PLAY_PAUSE: &str = "replay.play-pause";
    pub const REPLAY_STEP: &str = "replay.step";
    pub const REPLAY_FASTER: &str = "replay.faster";
    pub const REPLAY_SLOWER: &str = "replay.slower";
    /// Play on at once to the next time the clock reads the replay's pause time (see `ReplayStops::clock`).
    pub const REPLAY_TO_CLOCK: &str = "replay.to-clock";
    pub const BUY: &str = "trade.buy";
    pub const SELL: &str = "trade.sell";
    pub const CLOSE_POSITION: &str = "trade.close";
    /// A limit order at the price under the pointer: buys below the last close, sells above it.
    pub const LIMIT_ORDER: &str = "trade.limit";
    /// A stop order at the price under the pointer: buys above the last close, sells below it.
    pub const STOP_ORDER: &str = "trade.stop";
    pub const CANCEL_ORDERS: &str = "trade.cancel";
    /// The next size in 1-2-5 steps: what one order buys or sells.
    pub const SIZE_UP: &str = "trade.size-up";
    pub const SIZE_DOWN: &str = "trade.size-down";
    /// Pick a drawing tool; the next clicks on a plot place the drawing.
    pub const DRAW_TREND: &str = "draw.trend";
    pub const DRAW_RAY: &str = "draw.ray";
    pub const DRAW_EXTENDED: &str = "draw.extended";
    pub const DRAW_ARROW: &str = "draw.arrow";
    pub const DRAW_HLINE: &str = "draw.hline";
    pub const DRAW_HRAY: &str = "draw.hray";
    pub const DRAW_VLINE: &str = "draw.vline";
    pub const DRAW_CROSS: &str = "draw.cross";
    pub const DRAW_CHANNEL: &str = "draw.channel";
    pub const DRAW_RECT: &str = "draw.rect";
    pub const DRAW_FIB: &str = "draw.fib";
    pub const DRAW_PRICE_RANGE: &str = "draw.price-range";
    pub const DRAW_DATE_RANGE: &str = "draw.date-range";
    pub const DRAW_MEASURE: &str = "draw.measure";
    pub const DRAW_LONG: &str = "draw.long";
    pub const DRAW_SHORT: &str = "draw.short";
    pub const DRAW_TEXT: &str = "draw.text";
    /// Points snap to the prices of the bar under the pointer, or no longer.
    pub const DRAW_MAGNET: &str = "draw.magnet";
    /// Hide the drawings, or show them again.
    pub const DRAW_HIDE: &str = "draw.hide";
    /// Stop what is being done (Escape): drop the drawing tool, the half-made drawing, the selection, the choosing
    /// of where a replay starts.
    pub const CANCEL: &str = "cancel";
    pub const DRAW_DELETE: &str = "draw.delete";
    pub const DRAW_CLEAR: &str = "draw.clear";
    /// A copy of the selected drawing next to it, and whether the selected drawing can be moved.
    pub const DRAW_CLONE: &str = "draw.clone";
    pub const DRAW_LOCK: &str = "draw.lock";
    /// Take back the last change to the drawings, and make it again.
    pub const DRAW_UNDO: &str = "draw.undo";
    pub const DRAW_REDO: &str = "draw.redo";
    /// Log scale on or off for the price axis of the pane under the pointer (the main one without a pointer).
    pub const SCALE_LOG: &str = "scale.log";
    /// Percent from the first bar on screen, on or off, for the price axis of the pane under the pointer.
    pub const SCALE_PERCENT: &str = "scale.percent";
    /// Autoscale again the pane under the pointer, after its price axis was dragged.
    pub const SCALE_AUTO: &str = "scale.auto";
    /// Candles, Heikin-Ashi, bars, line, area: how the price series is drawn.
    pub const SERIES_CYCLE: &str = "series.cycle";
    /// Show the next higher timeframe, and after the highest the data as it came.
    pub const TIMEFRAME_CYCLE: &str = "timeframe.cycle";
    /// Move the bars by one: to older ones, to newer ones.
    pub const VIEW_BACK: &str = "view.back";
    pub const VIEW_FORWARD: &str = "view.forward";
    /// Wider bars, narrower bars, around the right edge of the plot.
    pub const VIEW_ZOOM_IN: &str = "view.zoom-in";
    pub const VIEW_ZOOM_OUT: &str = "view.zoom-out";
    /// To the first bar, to the newest one.
    pub const VIEW_START: &str = "view.start";
    pub const VIEW_END: &str = "view.end";
    /// The bars as a new chart has them (their usual width, the newest at the right) and every pane autoscaled.
    pub const VIEW_RESET: &str = "view.reset";
}

/// Names of the built-in layers and handlers: use them to replace or remove a built-in.
pub mod names {
    /// The pane every chart has: the one layers go to unless told otherwise.
    pub const MAIN_PANE: &str = "main";
    pub const GRID: &str = "grid";
    pub const CANDLES: &str = "candles";
    pub const VOLUME: &str = "volume";
    pub const STUDY_LABELS: &str = "study-labels";
    /// The trades of a strategy run, and the pane of its equity.
    pub const STRATEGY: &str = "strategy";
    pub const STRATEGY_EQUITY: &str = "strategy-equity";
    pub const AXES: &str = "axes";
    pub const CROSSHAIR: &str = "crosshair";
    pub const INTERACTION: &str = "interaction";
    pub const REPLAY: &str = "replay";
    pub const REPLAY_PICK: &str = "replay-pick";
    pub const KEYMAP: &str = "keymap";
    pub const TOOLBAR: &str = "toolbar";
    pub const TRADING: &str = "trading";
    pub const PANES: &str = "panes";
    pub const DRAWINGS: &str = "drawings";
    pub const TIMEFRAME: &str = "timeframe";
    pub const LAST_PRICE: &str = "last-price";
}

/// Order of the built-in layers and handlers. Pick numbers between them to go in between.
pub mod order {
    pub const GRID: i32 = 100;
    /// The volume histogram: under the candles.
    pub const VOLUME: i32 = 150;
    /// Candles and other series.
    pub const SERIES: i32 = 200;
    /// Indicators and drawings on top of the series.
    pub const OVERLAY: i32 = 300;
    /// The user's drawings: over indicators, under the axes.
    pub const DRAWINGS: i32 = 350;
    /// The trades of a strategy: over the indicators, under the drawings.
    pub const STRATEGY: i32 = 340;
    /// The price labels of drawings, on the axes.
    pub const DRAWING_LABELS: i32 = 460;
    pub const AXES: i32 = 400;
    /// The last price line and its label on the axis.
    pub const LAST_PRICE: i32 = 410;
    /// Fills and the position line: above the axes so their price label sits on the axis.
    pub const TRADES: i32 = 450;
    pub const CROSSHAIR: i32 = 500;
    pub const REPLAY: i32 = 600;
    pub const TOOLBAR: i32 = 700;

    /// The keymap turns keys into commands first, then the toolbar gets the pointer before pan and zoom.
    /// Keys typed into a note on the chart, before the keymap would take letters for shortcuts.
    pub const DRAWING_TEXT_HANDLER: i32 = 5;
    /// The arrows, Ctrl+C, Ctrl+X and Ctrl+V for the selected drawings, before the keymap would move the chart.
    pub const DRAWING_KEYS_HANDLER: i32 = 6;
    pub const KEYMAP: i32 = 10;
    pub const TOOLBAR_HANDLER: i32 = 50;
    pub const PANES_HANDLER: i32 = 60;
    /// The buttons on the labels of indicators, before a click there could start a drawing.
    pub const STUDY_LABELS_HANDLER: i32 = 80;
    /// The click that chooses where a replay starts, before the drawings and pan and zoom.
    pub const REPLAY_PICK_HANDLER: i32 = 85;
    /// Before pan and zoom, which a drawing being made or moved must not trigger.
    pub const DRAWINGS_HANDLER: i32 = 90;
    /// Grabs pending order lines to move them (just after the drawings, before pan and zoom).
    pub const ORDER_DRAG_HANDLER: i32 = 91;
    /// Observers come last and see everything, consumed events included.
    pub const OBSERVER_HANDLER: i32 = 10_000;
    pub const INTERACTION: i32 = 100;
    pub const CROSSHAIR_HANDLER: i32 = 200;
    pub const REPLAY_HANDLER: i32 = 300;
    pub const TRADING_HANDLER: i32 = 310;
}
