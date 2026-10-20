//! A strategy for Tickerlight, written in Rust and built to WebAssembly: the program loads the `.wasm` file from
//! its `strategies` folder and runs it with no rebuild of its own, sandboxed (no files, no network, a cap on
//! memory and on the work done at a bar).
//!
//! ```ignore
//! #[derive(Default)]
//! struct Breakout;
//!
//! impl tickerlight_strategy::Strategy for Breakout {
//!     const SETTINGS: &'static [(&'static str, f64)] = &[("Length", 20.0)];
//!
//!     fn name(settings: &[f64]) -> String {
//!         format!("Breakout {}", settings[0])
//!     }
//!
//!     fn on_bar(&mut self, ctx: &mut tickerlight_strategy::Ctx) {
//!         let (upper, _) = ctx.line("dc", &[ctx.setting(0)]);
//!         // ...
//!     }
//! }
//!
//! tickerlight_strategy::export!(Breakout);
//! ```
//!
//! Built with `cargo build --release --target wasm32-unknown-unknown` as a `cdylib`; the file goes into
//! `<data>/strategies/`. A strategy sees the bars only up to the one that just closed, as the program's own do.

/// The version of what the program and a strategy call each other with.
pub const ABI: i32 = 1;

/// A bar: its open time (milliseconds since 1970, UTC) and prices.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bar {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// The exits put on what an order opens.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Exits {
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
}

/// A trading strategy. It starts afresh ([`Default`]) for each run, and
/// [`on_bar`](Strategy::on_bar) is called at the close of every bar, the oldest first.
pub trait Strategy: Default {
    /// The numbers it is set with: names and the values they start at. The program shows them as fields to
    /// change; [`Ctx::setting`] reads them.
    const SETTINGS: &'static [(&'static str, f64)] = &[];

    /// What it is called in lists, from the values of its settings now.
    fn name(settings: &[f64]) -> String;

    fn on_bar(&mut self, ctx: &mut Ctx);
}

/// What a strategy sees and does at the close of a bar.
#[derive(Debug, Default)]
pub struct Ctx {
    bars: Vec<Bar>,
    /// The position with the market orders of this bar, as the program counts it.
    position: f64,
    equity: f64,
}

impl Ctx {
    /// The bars up to and including this one.
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    /// This bar: the one that just closed.
    pub fn bar(&self) -> Bar {
        self.bars.last().copied().unwrap_or_default()
    }

    /// The index of this bar in [`bars`](Self::bars): the last one.
    pub fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }

    /// The value of setting `i` of [`Strategy::SETTINGS`] as the user set it; NaN past the last.
    pub fn setting(&self, i: usize) -> f64 {
        host::value(i)
    }

    /// The signed size of the open position, with the market orders not yet filled: positive long, negative short.
    pub fn position(&self) -> f64 {
        self.position
    }

    /// What the account is worth at this close.
    pub fn equity(&self) -> f64 {
        self.equity
    }

    /// Line `column` of an indicator, `ago` bars back from this one (zero for this bar): the numbers the chart
    /// shows, NaN where there is none. `key` is its name in the program's files: "sma", "ema", "wma", "vwap",
    /// "bb", "dc", "rsi", "macd", "atr", "stoch", "obv", "cci", "mfi", "adx", "supertrend", "pivots", "vol",
    /// "ichimoku", "sessions"; `params` its settings in the order the program lists them. An unknown key stops
    /// the strategy with a message.
    pub fn indicator(&mut self, key: &str, params: &[f64], column: usize, ago: usize) -> f64 {
        let p = |i: usize| params.get(i).copied().unwrap_or(f64::NAN);
        host::indicator(key, [p(0), p(1), p(2)], column, ago)
    }

    /// The first line of an indicator at this bar and the bar before, like the RSI now and a bar ago.
    pub fn line(&mut self, key: &str, params: &[f64]) -> (f64, f64) {
        (self.indicator(key, params, 0, 0), self.indicator(key, params, 0, 1))
    }

    /// Draws `value` at this bar on the line `name` over the candles. NaN breaks the line.
    pub fn plot(&mut self, name: &str, value: f64) {
        host::plot(name, value);
    }

    /// Notes `text` at this bar: why it does what it does, shown with the trade this bar decided.
    pub fn note(&mut self, text: &str) {
        host::note(text);
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
            self.position += qty;
            host::order(Kind::Market, qty, f64::NAN, exits);
        }
    }

    /// Closes the position at the next open, and takes its exits off.
    pub fn close_position(&mut self) {
        self.position = 0.0;
        host::close_position();
    }

    /// A limit order: buys (`qty` positive) at `price` or lower, or sells at it or higher, on a later bar.
    pub fn limit(&mut self, qty: f64, price: f64, exits: Exits) {
        host::order(Kind::Limit, qty, price, exits);
    }

    /// A stop order: buys (`qty` positive) once the price rises to `price`, or sells once it falls to it.
    pub fn stop(&mut self, qty: f64, price: f64, exits: Exits) {
        host::order(Kind::Stop, qty, price, exits);
    }

    /// Takes off every order waiting, the exits of the position too.
    pub fn cancel_orders(&mut self) {
        host::cancel_orders();
    }

    /// The size that loses `risk` (0.01 for 1%) of the equity if the price goes from `entry` to `stop`; zero if
    /// they are the same.
    pub fn risk_size(&self, risk: f64, entry: f64, stop: f64) -> f64 {
        let distance = (entry - stop).abs();
        if distance > 0.0 && distance.is_finite() {
            self.equity.max(0.0) * risk / distance
        } else {
            0.0
        }
    }

    #[doc(hidden)]
    pub fn push(&mut self, bar: Bar) {
        self.bars.push(bar);
    }

    #[doc(hidden)]
    pub fn begin(&mut self, position: f64, equity: f64) {
        (self.position, self.equity) = (position, equity);
    }
}

#[derive(Clone, Copy)]
#[repr(i32)]
enum Kind {
    Market = 0,
    Limit = 1,
    Stop = 2,
}

#[doc(hidden)]
pub fn declare<S: Strategy>() {
    for (name, default) in S::SETTINGS {
        host::setting(name, *default);
    }
}

#[doc(hidden)]
pub fn name<S: Strategy>() {
    let values: Vec<f64> = (0..S::SETTINGS.len()).map(host::value).collect();
    host::name(&S::name(&values));
}

/// What the program provides, in the module "chart". Outside the program (on another target) there is nothing to
/// call: these stop with a message.
mod host {
    use super::{Exits, Kind};

    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "chart")]
    unsafe extern "C" {
        #[link_name = "name"]
        fn name_(ptr: *const u8, len: usize);
        #[link_name = "setting"]
        fn setting_(ptr: *const u8, len: usize, default: f64);
        #[link_name = "value"]
        fn value_(i: usize) -> f64;
        #[link_name = "indicator"]
        fn indicator_(ptr: *const u8, len: usize, p0: f64, p1: f64, p2: f64, column: usize, ago: usize) -> f64;
        #[link_name = "plot"]
        fn plot_(ptr: *const u8, len: usize, value: f64);
        #[link_name = "note"]
        fn note_(ptr: *const u8, len: usize);
        #[link_name = "order"]
        fn order_(kind: i32, qty: f64, price: f64, stop_loss: f64, take_profit: f64);
        #[link_name = "close_position"]
        fn close_position_();
        #[link_name = "cancel_orders"]
        fn cancel_orders_();
    }

    #[cfg(target_arch = "wasm32")]
    mod calls {
        use super::*;

        pub fn name(text: &str) {
            unsafe { name_(text.as_ptr(), text.len()) }
        }

        pub fn setting(text: &str, default: f64) {
            unsafe { setting_(text.as_ptr(), text.len(), default) }
        }

        pub fn value(i: usize) -> f64 {
            unsafe { value_(i) }
        }

        pub fn indicator(key: &str, [p0, p1, p2]: [f64; 3], column: usize, ago: usize) -> f64 {
            unsafe { indicator_(key.as_ptr(), key.len(), p0, p1, p2, column, ago) }
        }

        pub fn plot(text: &str, value: f64) {
            unsafe { plot_(text.as_ptr(), text.len(), value) }
        }

        pub fn note(text: &str) {
            unsafe { note_(text.as_ptr(), text.len()) }
        }

        pub fn order(kind: Kind, qty: f64, price: f64, exits: Exits) {
            let (sl, tp) = (
                exits.stop_loss.unwrap_or(f64::NAN),
                exits.take_profit.unwrap_or(f64::NAN),
            );
            unsafe { order_(kind as i32, qty, price, sl, tp) }
        }

        pub fn close_position() {
            unsafe { close_position_() }
        }

        pub fn cancel_orders() {
            unsafe { cancel_orders_() }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod calls {
        use super::*;

        const OUTSIDE: &str = "a strategy runs inside Tickerlight, built to wasm32-unknown-unknown";

        pub fn name(_: &str) {
            panic!("{OUTSIDE}")
        }

        pub fn setting(_: &str, _: f64) {
            panic!("{OUTSIDE}")
        }

        pub fn value(_: usize) -> f64 {
            panic!("{OUTSIDE}")
        }

        pub fn indicator(_: &str, _: [f64; 3], _: usize, _: usize) -> f64 {
            panic!("{OUTSIDE}")
        }

        pub fn plot(_: &str, _: f64) {
            panic!("{OUTSIDE}")
        }

        pub fn note(_: &str) {
            panic!("{OUTSIDE}")
        }

        pub fn order(_: Kind, _: f64, _: f64, _: Exits) {
            panic!("{OUTSIDE}")
        }

        pub fn close_position() {
            panic!("{OUTSIDE}")
        }

        pub fn cancel_orders() {
            panic!("{OUTSIDE}")
        }
    }

    pub use calls::*;
}

/// Makes `$strategy` the strategy of this WebAssembly module: the functions the program calls.
#[macro_export]
macro_rules! export {
    ($strategy:ty) => {
        const _: () = {
            use ::std::cell::RefCell;

            ::std::thread_local! {
                static STATE: RefCell<($strategy, $crate::Ctx)> = RefCell::new(Default::default());
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_abi() -> i32 {
                $crate::ABI
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_settings() {
                $crate::declare::<$strategy>();
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_name() {
                $crate::name::<$strategy>();
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_start() {
                STATE.with(|s| *s.borrow_mut() = Default::default());
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_push(time: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) {
                let bar = $crate::Bar {
                    time,
                    open,
                    high,
                    low,
                    close,
                    volume,
                };
                STATE.with(|s| s.borrow_mut().1.push(bar));
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn chart_on_bar(position: f64, equity: f64) {
                STATE.with(|s| {
                    let (strategy, ctx) = &mut *s.borrow_mut();
                    ctx.begin(position, equity);
                    $crate::Strategy::on_bar(strategy, ctx);
                });
            }
        };
    };
}
