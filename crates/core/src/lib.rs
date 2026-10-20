//! Platform-independent core.
//!
//! Takes data and input events, produces a scene of drawing primitives.
//! Knows nothing about the browser or the OS; backends only draw the scene.

pub mod aggregate;
pub mod builtin;
pub mod calendar;
pub mod candles;
pub mod candlestick;
pub mod chart;
pub mod cursor;
pub mod palette;
pub mod panes;
pub mod plugin;
pub mod price_scale;
pub mod sample;
pub mod scene;
pub mod source;
pub mod surface;
pub mod ticks;
pub mod time;
pub mod time_scale;
pub mod timeframe;
pub mod touch;
pub mod trades;

pub use aggregate::{Columns, aggregate};
pub use builtin::{
    ButtonKind, DefaultPlugins, Equity, Footprint, Label, LineSeries, Rsi, Sma, ToolButton, Toolbar, VolumeProfile,
};
pub use candles::{Candle, CandleView, Candles, OutOfOrder};
pub use candlestick::{CandleStyle, emit_candles};
pub use chart::{Chart, ChartStyle, Event, Key};
pub use cursor::Cursor;
pub use panes::{PaneStack, ScaleSetting};
pub use plugin::{
    ChartEdit, Clip, DrawCtx, EventCtx, Handler, Layer, PaneInfo, PaneRect, Panes, Plugin, Pointer, PointerShape,
    Resources, Response,
};
pub use price_scale::{PriceMode, PriceScale};
pub use scene::{ClipRect, Color, Direction, LayerRange, Line, Rect, Scene, Text};
pub use source::{Sink, Source};
pub use surface::Surface;
pub use tickerlight_text::{Align, Font};
pub use time::Time;
pub use time_scale::TimeScale;
pub use timeframe::{Resampled, TimeframeCtl, native_step, span_label};
pub use trades::{Side, Trade, TradeView, Trades, candles_from_trades};
