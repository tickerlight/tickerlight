//! The built-in features. They use the same public API as any plugin, so each one can be replaced
//! (add a layer or handler with the same name) or removed, and serves as an example to copy.

mod axes;
mod candles;
mod crosshair;
mod drawings;
mod equity;
mod footprint;
mod grid;
mod interaction;
pub(crate) use interaction::WHEEL_ZOOM_PER_NOTCH;
mod keymap;
mod last_price;
mod learn;
mod legend;
mod line_series;
mod observer;
mod pane_resize;
mod price_axis;
mod replay;
mod rsi;
mod sma;
mod strategy;
mod studies;
mod timeframe;
mod toolbar;
mod trading;
mod volume;
mod volume_profile;

pub use axes::{AxesLayer, AxesPlugin};
pub use candles::{CandlesLayer, CandlesPlugin, SeriesKind, heikin_ashi};
pub use crosshair::{
    BarLabel, CrosshairActive, CrosshairHandler, CrosshairLabels, CrosshairLayer, CrosshairPlugin, LinkedTime,
    crosshair_labels,
};
pub use drawings::{
    Anchor, DEFAULT_COLOR, Dash, Drawing, DrawingStyle, Drawings, DrawingsHandler, DrawingsLayer, DrawingsPlugin,
    FIB_LEVELS, Outcome, TEXT_SIZES, Tool, position_outcome,
};
pub use equity::Equity;
pub use footprint::Footprint;
pub use grid::{GridLayer, GridPlugin};
pub use interaction::{InteractionPlugin, PanZoomHandler};
pub use keymap::{Keymap, KeymapHandler};
pub use last_price::{Clock, LastPriceLayer, LastPricePlugin, countdown};
pub use learn::{FEATURE_NAMES, FEATURES, LearnedDirection, Logistic, bar_features, features_csv, go_with};
pub use legend::{LEGEND, Legend, LegendLayer, SeriesTitle};
pub use line_series::LineSeries;
pub use observer::{Listener, Observe};
pub use pane_resize::{PaneResizeHandler, PanesPlugin};
pub use price_axis::{PriceAxisHandler, PriceAxisPlugin};
pub use replay::{Replay, ReplayHandler, ReplayLayer, ReplayPlugin, ReplayStops, clock};
pub use rsi::Rsi;
pub use sma::Sma;
pub use strategy::{
    ClosedTrade, FillsCompared, Input, LiveRun, MovingAverageCross, OrderLife, Plot, RsiReversal, Setting, Strategy,
    StrategyCtx, StrategyRun, TestPeriod, TradeChanges, backtest, backtest_from, backtest_with_tape, closed_trades,
    open_entry, set_strategy_run,
};
pub use studies::{
    Kind, OpenStudySettings, Param, STUDY_COLORS, STUDY_WIDTHS, Studies, Study, compute, set_studies, studies,
};
pub use timeframe::{TimeframeHandler, TimeframeLabel, TimeframePlugin};
pub use toolbar::{
    ButtonKind, ButtonRect, Label, ToolButton, Toolbar, ToolbarHandler, ToolbarLayer, ToolbarPlugin, layout,
};
pub(crate) use trading::order_after;
pub use trading::{
    Account, DEFAULT_BALANCE, EXIT_SHARES, Exits, ExitsTried, Fill, Live, Order, OrderDragHandler, OrderKind, Stats,
    TradingHandler, TradingLayer, TradingPlugin, TriedExit, add_position, can_trade, exits_tried, size_down, size_up,
};
pub use volume::{VolumeLayer, VolumePlugin};
pub use volume_profile::VolumeProfile;

use crate::{Chart, Plugin};

/// A resource that tells the built-in layers to leave out their text readouts (the replay state, the position and
/// the results): for a program that shows them in panels of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoReadouts;

/// Everything a normal candlestick chart has: grid, candles, axes with labels, crosshair,
/// pan and zoom with the mouse, and the manual bar replay with virtual trades (keys `R`, `Space`,
/// `Right`, `B`, `S`, `C`).
pub struct DefaultPlugins;

impl Plugin for DefaultPlugins {
    fn build(&self, chart: &mut Chart) {
        chart.add_plugin(GridPlugin);
        chart.add_plugin(VolumePlugin::default());
        chart.add_plugin(CandlesPlugin);
        chart.add_plugin(AxesPlugin);
        chart.add_plugin(LastPricePlugin);
        chart.add_plugin(CrosshairPlugin);
        chart.add_plugin(PanesPlugin);
        chart.add_plugin(PriceAxisPlugin);
        chart.add_plugin(DrawingsPlugin);
        chart.add_plugin(InteractionPlugin);
        chart.add_plugin(TimeframePlugin);
        chart.add_plugin(ReplayPlugin);
        chart.add_plugin(TradingPlugin);
        chart.add_plugin(ToolbarPlugin);
    }
}
