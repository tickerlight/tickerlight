//! The chart in a browser. Build with `crates/web/build.sh`, then serve `crates/web/www`.
//!
//! JavaScript calls `start(history)` (or `start_demo()`): a canvas is added to the page and the chart
//! runs in it. `start` returns a [`ChartHandle`](wasm::ChartHandle) through which the page feeds live
//! candles and trades. Candles travel as plain numbers `time_ms, open, high, low, close, volume`, so no
//! JS object is created per candle.

/// Data waiting to move from JavaScript into the chart. The chart polls it like any other source.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod feed {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tickerlight_core::{Candle, Side, Sink, Source, Time, Trade};

    #[derive(Default)]
    pub struct Queue {
        candles: Vec<Candle>,
        trades: Vec<Trade>,
    }

    /// The page's side of the feed: pushes into the queue.
    #[derive(Clone, Default)]
    pub struct Feed(Rc<RefCell<Queue>>);

    impl Feed {
        pub fn candle(&self, time: f64, open: f64, high: f64, low: f64, close: f64, volume: f64) {
            self.0.borrow_mut().candles.push(Candle {
                time: Time(time as i64),
                open,
                high,
                low,
                close,
                volume,
            });
        }

        pub fn trade(&self, time: f64, price: f64, qty: f64, buy: bool) {
            let side = if buy { Side::Buy } else { Side::Sell };
            self.0.borrow_mut().trades.push(Trade {
                time: Time(time as i64),
                price,
                qty,
                side,
            });
        }

        /// The chart's side: hands the queue over to the chart.
        pub fn source(&self) -> impl Source + use<> {
            FeedSource(self.0.clone())
        }
    }

    struct FeedSource(Rc<RefCell<Queue>>);

    impl Source for FeedSource {
        fn poll(&mut self, sink: &mut Sink) -> bool {
            let mut queue = self.0.borrow_mut();
            queue.candles.drain(..).for_each(|c| sink.candle(c));
            queue.trades.drain(..).for_each(|t| sink.trade(t));
            true
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use tickerlight_core::builtin::{Account, Drawings, Fill, Listener, Observe};
    use tickerlight_core::sample::random_walk;
    use tickerlight_core::{Candle, Candles, Chart, Color, Equity, Rsi, Sma, Time};
    use wasm_bindgen::prelude::*;

    use crate::feed::Feed;

    /// Which indicators the chart gets. `new Options().sma(50).rsi(14)`; without options the chart
    /// has only the price series and the volume.
    #[wasm_bindgen]
    #[derive(Default, Clone)]
    pub struct Options {
        smas: Vec<usize>,
        rsi: Option<usize>,
        equity: bool,
        on_fill: Option<js_sys::Function>,
        on_pointer: Option<js_sys::Function>,
        on_replay: Option<js_sys::Function>,
        on_drawings: Option<js_sys::Function>,
        drawings: Option<String>,
    }

    #[wasm_bindgen]
    impl Options {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Options {
            Options::default()
        }

        /// A simple moving average of the close over `period` bars, drawn over the candles.
        pub fn sma(mut self, period: usize) -> Options {
            self.smas.push(period);
            self
        }

        /// The relative strength index in a pane of its own.
        pub fn rsi(mut self, period: usize) -> Options {
            self.rsi = Some(period);
            self
        }

        /// The equity curve of the virtual account (replay trading) in a pane of its own.
        pub fn equity(mut self) -> Options {
            self.equity = true;
            self
        }

        /// Calls `f(time_ms, price, qty, fee, position, realized)` for every fill of the virtual account;
        /// `qty` is negative for a sell, `position` and `realized` are the account after the fill.
        pub fn on_fill(mut self, f: js_sys::Function) -> Options {
            self.on_fill = Some(f);
            self
        }

        /// Calls `f(time_ms, price)` as the pointer moves over the chart; both are `undefined` when the
        /// pointer is not over a bar or a plot, and `f()` is called with no arguments when it leaves.
        pub fn on_pointer(mut self, f: js_sys::Function) -> Options {
            self.on_pointer = Some(f);
            self
        }

        /// Calls `f(text)` when the drawings change (placed, moved, deleted; a drag when it ends) with all of them
        /// as text: keep it (in `localStorage`, say) and give it back to `load_drawings` next time.
        pub fn on_drawings(mut self, f: js_sys::Function) -> Options {
            self.on_drawings = Some(f);
            self
        }

        /// Starts with the drawings saved as text by `on_drawings`.
        pub fn load_drawings(mut self, text: String) -> Options {
            self.drawings = Some(text);
            self
        }

        /// Calls `f(active, shown, total)` when the replay starts, stops or moves.
        pub fn on_replay(mut self, f: js_sys::Function) -> Options {
            self.on_replay = Some(f);
            self
        }
    }

    /// What the page keeps after `start`: the way to push live data into the running chart.
    #[wasm_bindgen]
    pub struct ChartHandle {
        feed: Feed,
    }

    #[wasm_bindgen]
    impl ChartHandle {
        /// A candle. At the time of the newest candle it replaces that one (the forming candle);
        /// later it is appended; older ones are ignored.
        pub fn push_candle(&self, time_ms: f64, open: f64, high: f64, low: f64, close: f64, volume: f64) {
            self.feed.candle(time_ms, open, high, low, close, volume);
        }

        /// A trade of the tape; `buy` is true when the buyer was the aggressor.
        pub fn push_trade(&self, time_ms: f64, price: f64, qty: f64, buy: bool) {
            self.feed.trade(time_ms, price, qty, buy);
        }
    }

    /// Forwards what the chart reports to the functions the page gave in `Options`.
    #[derive(Clone)]
    struct Callbacks {
        fill: Option<js_sys::Function>,
        pointer: Option<js_sys::Function>,
        replay: Option<js_sys::Function>,
        drawings: Option<js_sys::Function>,
    }

    impl Listener for Callbacks {
        fn fill(&mut self, fill: &Fill, account: &Account) {
            if let Some(f) = &self.fill {
                let args = [
                    fill.time.0 as f64,
                    fill.price,
                    fill.qty,
                    fill.fee,
                    account.position(),
                    account.realized(),
                ];
                let _ = f.apply(
                    &JsValue::NULL,
                    &args.iter().map(|&v| JsValue::from_f64(v)).collect::<js_sys::Array>(),
                );
            }
        }

        fn pointer(&mut self, bar: Option<Time>, price: Option<f64>) {
            if let Some(f) = &self.pointer {
                let _ = match (bar, price) {
                    (None, None) => f.call0(&JsValue::NULL),
                    _ => f.call2(
                        &JsValue::NULL,
                        &bar.map_or(JsValue::UNDEFINED, |t| JsValue::from_f64(t.0 as f64)),
                        &price.map_or(JsValue::UNDEFINED, JsValue::from_f64),
                    ),
                };
            }
        }

        fn drawings(&mut self, drawings: &Drawings) {
            if let Some(f) = &self.drawings {
                let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&drawings.save()));
            }
        }

        fn replay(&mut self, active: bool, shown: usize, total: usize) {
            if let Some(f) = &self.replay {
                let _ = f.call3(
                    &JsValue::NULL,
                    &JsValue::from_bool(active),
                    &JsValue::from_f64(shown as f64),
                    &JsValue::from_f64(total as f64),
                );
            }
        }
    }

    const SMA_COLORS: [Color; 3] = [
        Color::rgb(255, 183, 77),
        Color::rgb(66, 165, 245),
        Color::rgb(171, 71, 188),
    ];

    fn run(
        candles: Candles,
        canvas: Option<web_sys::HtmlCanvasElement>,
        options: Options,
    ) -> Result<ChartHandle, JsError> {
        console_error_panic_hook::set_once();
        let mut chart = Chart::new(candles);
        for (i, &period) in options.smas.iter().enumerate() {
            // Every average is a layer of its own, so each needs its own name.
            let names = ["sma-1", "sma-2", "sma-3", "sma-4", "sma-5", "sma-6"];
            let name = names[i.min(names.len() - 1)];
            chart.add_plugin(Sma::new(period, SMA_COLORS[i % SMA_COLORS.len()]).named(name));
        }
        if let Some(period) = options.rsi {
            chart.add_plugin(Rsi::new(period));
        }
        if options.equity {
            chart.add_plugin(Equity::new());
        }
        if let Some(text) = &options.drawings {
            let panes: Vec<&'static str> = chart.pane_stack().names().collect();
            if let Some(drawings) = chart.resources_mut().get_mut::<Drawings>() {
                drawings.load(text, &panes);
            }
        }
        if options.on_fill.is_some()
            || options.on_pointer.is_some()
            || options.on_replay.is_some()
            || options.on_drawings.is_some()
        {
            chart.add_plugin(Observe::new(Callbacks {
                fill: options.on_fill,
                pointer: options.on_pointer,
                replay: options.on_replay,
                drawings: options.on_drawings,
            }));
        }
        let feed = Feed::default();
        chart.add_source(feed.source());
        match canvas {
            Some(canvas) => tickerlight_backend_wgpu::run_on_canvas(chart, canvas),
            None => tickerlight_backend_wgpu::run(chart),
        }
        .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(ChartHandle { feed })
    }

    /// Starts the chart on `history`: six numbers per candle (`time_ms, open, high, low, close, volume`),
    /// oldest first. Fails when the length is not a multiple of six or the times are not increasing.
    #[wasm_bindgen]
    pub fn start(history: &[f64], options: Option<Options>) -> Result<ChartHandle, JsError> {
        start_with(history, None, options.unwrap_or_default())
    }

    /// Like `start`, but draws into a canvas of the page (sized by CSS) instead of adding one.
    #[wasm_bindgen]
    pub fn start_on(
        canvas: web_sys::HtmlCanvasElement,
        history: &[f64],
        options: Option<Options>,
    ) -> Result<ChartHandle, JsError> {
        start_with(history, Some(canvas), options.unwrap_or_default())
    }

    fn start_with(
        history: &[f64],
        canvas: Option<web_sys::HtmlCanvasElement>,
        options: Options,
    ) -> Result<ChartHandle, JsError> {
        if !history.len().is_multiple_of(6) {
            return Err(JsError::new("history must hold six numbers per candle"));
        }
        let mut candles = Candles::with_capacity(history.len() / 6);
        for c in history.as_chunks::<6>().0 {
            candles
                .push(Candle {
                    time: Time(c[0] as i64),
                    open: c[1],
                    high: c[2],
                    low: c[3],
                    close: c[4],
                    volume: c[5],
                })
                .map_err(|_| JsError::new("candle times must be increasing"))?;
        }
        run(candles, canvas, options)
    }

    /// Shows a demo chart of random data in the page.
    #[wasm_bindgen]
    pub fn start_demo() -> Result<ChartHandle, JsError> {
        run(random_walk(100_000), None, Options::new().sma(50).rsi(14))
    }
}
