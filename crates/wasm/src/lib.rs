//! Strategies loaded from WebAssembly files at run time: written with `tickerlight-strategy`, built to
//! `wasm32-unknown-unknown`, put in a folder. Each runs sandboxed in an interpreter: it reaches nothing but the
//! functions here, with one memory and one table, capped, and caps on the work it does at a bar and in a run and
//! on what it hands over.
//!
//! What a module and the program call each other with (module "chart"; text is a pointer and a length into the
//! module's memory):
//!
//! - The module exports `memory`, `chart_abi() -> i32` ([`ABI`]), `chart_settings()`, `chart_name()`,
//!   `chart_start()`, `chart_push(time: i64, open, high, low, close, volume: f64)` (a bar it has not seen) and
//!   `chart_on_bar(position, equity: f64)`.
//! - The program provides `name(text)` and `setting(text, default)` (called from `chart_name` and
//!   `chart_settings`), `value(i) -> f64` (setting `i`), `indicator(key, p0, p1, p2, column, ago) -> f64`,
//!   `plot(text, value)`, `note(text)` (why, at this bar), `order(kind, qty, price, stop_loss, take_profit)` (kind 0
//!   market, 1 limit, 2 stop; NaN for none), `close_position()` and `cancel_orders()`. A module needs import only
//!   the ones it calls.

use std::cell::RefCell;
use std::fmt;
use std::path::Path;

use tickerlight_core::builtin::{Exits, Kind, Setting, Strategy, StrategyCtx, Study};
use wasmi::errors::HostError;
use wasmi::{
    Caller, Config, Engine, Error, Extern, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
    TypedResumableCall,
};

/// The version of the calls this program speaks; a module built for another is not loaded.
pub const ABI: i32 = 1;

/// The work a module may do at a bar (or in any other call), in the interpreter's units: far more than a strategy
/// needs, so only one that never stops is stopped.
const FUEL: u64 = 50_000_000;

/// The work a whole run may do: this, and [`RUN_FUEL_PER_BAR`] more for each bar. A module slow at every bar is
/// stopped within seconds, not after hours of a frozen window (a run works on the thread that draws).
const RUN_FUEL: u64 = 2_000_000_000;
const RUN_FUEL_PER_BAR: u64 = 50_000;

/// The memory a module may have.
const MEMORY: usize = 256 << 20;

/// The entries of a module's tables (functions called through a pointer).
const TABLE: usize = 100_000;

/// The longest text a module may hand over.
const TEXT: usize = 256;

/// What a module may ask for in one call: plots, notes, orders.
const COMMANDS: usize = 10_000;

/// The lines a module may plot, and the indicators it may read, in a run.
const PLOTS: usize = 32;
const STUDIES: usize = 64;

/// What the module asked for while it runs, done once it returns: in the order asked.
enum Command {
    Plot(String, f64),
    Note(String),
    Order {
        kind: i32,
        qty: f64,
        price: f64,
        exits: Exits,
    },
    ClosePosition,
    CancelOrders,
}

/// Which of the module's functions runs: `name` and `setting` are answers of `chart_name` and `chart_settings`
/// alone, so a module cannot pile up settings while it trades.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Settings,
    Name,
    Run,
}

/// What the functions the module calls work with.
struct Host {
    phase: Phase,
    /// The values of its settings, as the user set them.
    values: Vec<f64>,
    name: String,
    settings: Vec<(String, f64)>,
    commands: Vec<Command>,
    /// The names of the lines it plotted and the indicators it read in this run.
    plots: Vec<String>,
    studies: Vec<Study>,
    limits: StoreLimits,
}

impl Host {
    fn command(&mut self, command: Command) -> Result<(), Error> {
        if self.phase != Phase::Run {
            return Err(Error::new("plots, notes and orders are for chart_on_bar"));
        }
        if self.commands.len() >= COMMANDS {
            return Err(Error::new(format!(
                "more than {COMMANDS} plots, notes and orders at a bar"
            )));
        }
        if let Command::Plot(name, _) = &command
            && !self.plots.contains(name)
        {
            if self.plots.len() >= PLOTS {
                return Err(Error::new(format!("more than {PLOTS} lines plotted")));
            }
            self.plots.push(name.clone());
        }
        self.commands.push(command);
        Ok(())
    }
}

/// An indicator the module asked for: the call stops, the program works it out from the bars, and the call goes
/// on with the value.
#[derive(Debug)]
struct IndicatorCall {
    study: Study,
    column: usize,
    ago: usize,
}

impl fmt::Display for IndicatorCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "indicator {}", self.study.label())
    }
}

impl HostError for IndicatorCall {}

/// A strategy in a WebAssembly module.
pub struct WasmStrategy {
    /// The file's name without its extension: what it is called if it cannot say.
    file: String,
    /// The module, to make another instance of it.
    module: Module,
    store: RefCell<Store<Host>>,
    names: Vec<String>,
    values: Vec<f64>,
    exports: Exports,
    /// How many bars of this run the module has.
    pushed: usize,
    /// Why it stopped in this run: it trades no more until the next.
    stopped: Option<String>,
    /// The work it may do in a call: [`FUEL`].
    fuel: u64,
    /// The work it did in this run, and the base of what a run may do: [`RUN_FUEL`].
    spent: u64,
    run_fuel: u64,
    /// What it called itself with these settings: asked once, not at every frame the list is drawn.
    named: RefCell<Option<(Vec<f64>, String)>>,
}

struct Exports {
    name: TypedFunc<(), ()>,
    start: TypedFunc<(), ()>,
    push: TypedFunc<(i64, f64, f64, f64, f64, f64), ()>,
    on_bar: TypedFunc<(f64, f64), ()>,
}

/// The text at `ptr`, `len` bytes, in the memory of the module calling.
fn text(caller: &Caller<'_, Host>, ptr: i32, len: i32) -> Result<String, Error> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| Error::new("the module has no memory"))?;
    let (ptr, len) = (ptr as u32 as usize, (len as u32 as usize).min(TEXT));
    let bytes = memory
        .data(caller)
        .get(ptr..ptr + len)
        .ok_or_else(|| Error::new("text out of the module's memory"))?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn optional(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

fn linker(engine: &Engine) -> Result<Linker<Host>, Error> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("chart", "name", |caller: Caller<'_, Host>, ptr: i32, len: i32| {
        if caller.data().phase != Phase::Name {
            return Err(Error::new("name is for chart_name"));
        }
        let name = text(&caller, ptr, len)?;
        let mut caller = caller;
        caller.data_mut().name = name;
        Ok(())
    })?;
    linker.func_wrap(
        "chart",
        "setting",
        |caller: Caller<'_, Host>, ptr: i32, len: i32, default: f64| {
            if caller.data().phase != Phase::Settings {
                return Err(Error::new("setting is for chart_settings"));
            }
            let name = text(&caller, ptr, len)?;
            let mut caller = caller;
            caller.data_mut().settings.push((name, default));
            Ok(())
        },
    )?;
    linker.func_wrap("chart", "value", |caller: Caller<'_, Host>, i: i32| {
        caller.data().values.get(i as u32 as usize).copied().unwrap_or(f64::NAN)
    })?;
    linker.func_wrap(
        "chart",
        "indicator",
        |caller: Caller<'_, Host>, ptr: i32, len: i32, p0: f64, p1: f64, p2: f64, column: i32, ago: i32| {
            let key = text(&caller, ptr, len)?;
            let kind = Kind::from_key(&key).ok_or_else(|| Error::new(format!("there is no indicator \"{key}\"")))?;
            let mut study = Study::new(kind);
            for (i, p) in [p0, p1, p2].into_iter().enumerate().take(kind.params().len()) {
                study.set(i, p);
            }
            // Each indicator read is worked out over all the bars and kept for the run.
            let mut caller = caller;
            let studies = &mut caller.data_mut().studies;
            if !studies.contains(&study) {
                if studies.len() >= STUDIES {
                    return Err(Error::new(format!("more than {STUDIES} indicators read")));
                }
                studies.push(study);
            }
            let call = IndicatorCall {
                study,
                column: column as u32 as usize,
                ago: ago as u32 as usize,
            };
            Err::<f64, _>(Error::host(call))
        },
    )?;
    linker.func_wrap(
        "chart",
        "plot",
        |caller: Caller<'_, Host>, ptr: i32, len: i32, value: f64| {
            let name = text(&caller, ptr, len)?;
            let mut caller = caller;
            caller.data_mut().command(Command::Plot(name, value))
        },
    )?;
    linker.func_wrap("chart", "note", |caller: Caller<'_, Host>, ptr: i32, len: i32| {
        let note = text(&caller, ptr, len)?;
        let mut caller = caller;
        caller.data_mut().command(Command::Note(note))
    })?;
    linker.func_wrap(
        "chart",
        "order",
        |mut caller: Caller<'_, Host>, kind: i32, qty: f64, price: f64, stop_loss: f64, take_profit: f64| {
            let exits = Exits {
                stop_loss: optional(stop_loss),
                take_profit: optional(take_profit),
                // The calls set no break-even.
                break_even: None,
            };
            caller.data_mut().command(Command::Order {
                kind,
                qty,
                price,
                exits,
            })
        },
    )?;
    linker.func_wrap("chart", "close_position", |mut caller: Caller<'_, Host>| {
        caller.data_mut().command(Command::ClosePosition)
    })?;
    linker.func_wrap("chart", "cancel_orders", |mut caller: Caller<'_, Host>| {
        caller.data_mut().command(Command::CancelOrders)
    })?;
    Ok(linker)
}

impl WasmStrategy {
    /// Loads the strategy in the module `wasm` (its bytes, or its text form), called `file` if it cannot say.
    pub fn new(file: &str, wasm: &[u8]) -> Result<Self, String> {
        let mut config = Config::default();
        config.consume_fuel(true);
        // The calls know one memory; more would only be more to allocate.
        config.wasm_multi_memory(false);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| e.to_string())?;
        Self::instantiate(file, module)
    }

    /// A new instance of `module`, with the settings it starts with.
    fn instantiate(file: &str, module: Module) -> Result<Self, String> {
        let engine = module.engine().clone();
        let host = Host {
            phase: Phase::Settings,
            values: Vec::new(),
            name: String::new(),
            settings: Vec::new(),
            commands: Vec::new(),
            plots: Vec::new(),
            studies: Vec::new(),
            // One memory and one table, the calls' own: a module of many memories would get each of them whole.
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY)
                .memories(1)
                .tables(1)
                .table_elements(TABLE)
                .instances(1)
                .build(),
        };
        let mut store = Store::new(&engine, host);
        store.limiter(|host| &mut host.limits);
        store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let linker = linker(&engine).map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| e.to_string())?;
        let abi = instance
            .get_typed_func::<(), i32>(&store, "chart_abi")
            .map_err(|_| "not a strategy: it has no chart_abi".to_string())?
            .call(&mut store, ())
            .map_err(|e| e.to_string())?;
        if abi != ABI {
            return Err(format!(
                "built for version {abi} of the strategy calls, this program has {ABI}"
            ));
        }
        fn func<P: wasmi::WasmParams>(
            instance: &wasmi::Instance,
            store: &Store<Host>,
            name: &str,
        ) -> Result<TypedFunc<P, ()>, String> {
            instance
                .get_typed_func(store, name)
                .map_err(|_| format!("not a strategy: it has no {name} of the right kind"))
        }
        let exports = Exports {
            name: func(&instance, &store, "chart_name")?,
            start: func(&instance, &store, "chart_start")?,
            push: func(&instance, &store, "chart_push")?,
            on_bar: func(&instance, &store, "chart_on_bar")?,
        };
        let settings: TypedFunc<(), ()> = func(&instance, &store, "chart_settings")?;
        settings.call(&mut store, ()).map_err(|e| e.to_string())?;
        store.data_mut().phase = Phase::Run;
        let (names, values) = std::mem::take(&mut store.data_mut().settings).into_iter().unzip();
        Ok(Self {
            file: file.to_string(),
            module,
            store: RefCell::new(store),
            names,
            values,
            exports,
            pushed: 0,
            fuel: FUEL,
            stopped: None,
            spent: 0,
            run_fuel: RUN_FUEL,
            named: RefCell::new(None),
        })
    }

    /// Why it stopped in the last run, if it did: a panic, a bar that took too long, an unknown indicator.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// Calls `func` with the fuel of one call; returns the work it did.
    fn call<P: wasmi::WasmParams>(
        store: &mut Store<Host>,
        fuel: u64,
        func: &TypedFunc<P, ()>,
        params: P,
    ) -> Result<u64, String> {
        store.set_fuel(fuel).map_err(|e| e.to_string())?;
        func.call(&mut *store, params).map_err(|e| stopped_by(&e))?;
        Ok(fuel - store.get_fuel().unwrap_or(0))
    }

    fn bar(&mut self, ctx: &mut StrategyCtx<'_>) -> Result<(), String> {
        let store = self.store.get_mut();
        store.data_mut().values.clone_from(&self.values);
        let bars = ctx.bars();
        for i in self.pushed..bars.len() {
            let bar = (
                bars.time()[i].0,
                bars.open()[i],
                bars.high()[i],
                bars.low()[i],
                bars.close()[i],
                bars.volume()[i],
            );
            self.spent += Self::call(store, self.fuel, &self.exports.push, bar)?;
        }
        self.pushed = bars.len();
        store.set_fuel(self.fuel).map_err(|e| e.to_string())?;
        let mut call = self
            .exports
            .on_bar
            .call_resumable(&mut *store, (ctx.position(), ctx.equity()))
            .map_err(|e| stopped_by(&e))?;
        loop {
            call = match call {
                TypedResumableCall::Finished(()) => break,
                TypedResumableCall::OutOfFuel(_) => return Err("it worked too long at a bar".to_string()),
                TypedResumableCall::HostTrap(trap) => {
                    let Some(asked) = trap.host_error().downcast_ref::<IndicatorCall>() else {
                        return Err(stopped_by(trap.host_error()));
                    };
                    let values = ctx.indicator(asked.study);
                    let value = values
                        .get(asked.column)
                        .and_then(|line| line.len().checked_sub(asked.ago + 1).map(|i| line[i]))
                        .unwrap_or(f64::NAN);
                    trap.resume(&mut *store, &[value.into()]).map_err(|e| stopped_by(&e))?
                }
            };
        }
        self.spent += self.fuel - store.get_fuel().unwrap_or(0);
        if self.spent > self.run_fuel + RUN_FUEL_PER_BAR * self.pushed as u64 {
            return Err("it worked too long in this run".to_string());
        }
        for command in store.data_mut().commands.drain(..) {
            match command {
                Command::Plot(name, value) => ctx.plot(&name, value),
                Command::Note(text) => ctx.note(&text),
                Command::Order {
                    kind: 0, qty, exits, ..
                } if qty > 0.0 => ctx.buy(qty, exits),
                Command::Order {
                    kind: 0, qty, exits, ..
                } => ctx.sell(qty, exits),
                Command::Order {
                    kind: 1,
                    qty,
                    price,
                    exits,
                } => ctx.limit(qty, price, exits),
                Command::Order {
                    kind: 2,
                    qty,
                    price,
                    exits,
                } => ctx.stop(qty, price, exits),
                Command::Order { kind, .. } => return Err(format!("there is no order of kind {kind}")),
                Command::ClosePosition => ctx.close_position(),
                Command::CancelOrders => ctx.cancel_orders(),
            }
        }
        Ok(())
    }
}

/// What to tell the user of an error that stopped a module: a panic or a trap, short.
fn stopped_by(error: &Error) -> String {
    error.to_string()
}

impl Strategy for WasmStrategy {
    fn name(&self) -> String {
        let mut named = self.named.borrow_mut();
        let name = match &*named {
            Some((values, name)) if *values == self.values => name.clone(),
            _ => {
                let mut store = self.store.borrow_mut();
                store.data_mut().values.clone_from(&self.values);
                store.data_mut().name.clear();
                store.data_mut().phase = Phase::Name;
                let called = Self::call(&mut store, self.fuel, &self.exports.name, ());
                store.data_mut().phase = Phase::Run;
                let name = match called {
                    Ok(_) if !store.data().name.is_empty() => std::mem::take(&mut store.data_mut().name),
                    _ => self.file.clone(),
                };
                *named = Some((self.values.clone(), name.clone()));
                name
            }
        };
        match &self.stopped {
            Some(why) => format!("{name} (stopped: {why})"),
            None => name,
        }
    }

    fn settings(&mut self) -> Vec<Setting<'_>> {
        self.names
            .iter()
            .zip(&mut self.values)
            .map(|(name, value)| Setting { name, value })
            .collect()
    }

    fn start(&mut self) {
        self.pushed = 0;
        self.spent = 0;
        self.stopped = None;
        let store = self.store.get_mut();
        let host = store.data_mut();
        host.commands.clear();
        host.plots.clear();
        host.studies.clear();
        if let Err(why) = Self::call(store, self.fuel, &self.exports.start, ()) {
            self.stopped = Some(why);
        }
    }

    fn another(&self) -> Option<Box<dyn Strategy>> {
        let mut other = Self::instantiate(&self.file, self.module.clone()).ok()?;
        other.values.clone_from(&self.values);
        (other.fuel, other.run_fuel) = (self.fuel, self.run_fuel);
        Some(Box::new(other))
    }

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
        if self.stopped.is_some() {
            return;
        }
        if let Err(why) = self.bar(ctx) {
            self.store.get_mut().data_mut().commands.clear();
            self.stopped = Some(why);
        }
    }
}

/// The strategies in the `.wasm` files of `folder`, by file name, and what is wrong with those that do not load.
/// No folder is no strategies.
pub fn load_folder(folder: &Path) -> (Vec<WasmStrategy>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return (Vec::new(), Vec::new());
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
        .collect();
    paths.sort();
    let (mut strategies, mut problems) = (Vec::new(), Vec::new());
    for path in paths {
        let file = path.file_stem().unwrap_or_default().to_string_lossy();
        // A `.wasm` file is binary: its text form is left to the tests, where a parse error of text is understood.
        let read = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
            if bytes.starts_with(b"\0asm") {
                WasmStrategy::new(&file, &bytes)
            } else {
                Err("not a WebAssembly module (built with --target wasm32-unknown-unknown?)".into())
            }
        });
        match read {
            Ok(strategy) => strategies.push(strategy),
            Err(why) => problems.push(format!("{}: {why}", path.file_name().unwrap_or_default().display())),
        }
    }
    (strategies, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::builtin::{Account, backtest};
    use tickerlight_core::{Candle, Candles, Time};

    fn bars(closes: &[f64]) -> Candles {
        let mut candles = Candles::new();
        for (i, &c) in closes.iter().enumerate() {
            let candle = Candle {
                time: Time(i as i64 * 60_000),
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 1.0,
            };
            candles.push(candle).unwrap();
        }
        candles
    }

    /// A module named "Probe" with a setting "At" (2): it plots a 2 bar average and buys one at the bar that makes
    /// "At" + 1 bars, noting "Bought"; `on_bar` runs `extra` first.
    fn probe(extra: &str) -> String {
        format!(
            r#"(module
                (import "chart" "name" (func $name (param i32 i32)))
                (import "chart" "setting" (func $setting (param i32 i32 f64)))
                (import "chart" "value" (func $value (param i32) (result f64)))
                (import "chart" "indicator" (func $indicator (param i32 i32 f64 f64 f64 i32 i32) (result f64)))
                (import "chart" "plot" (func $plot (param i32 i32 f64)))
                (import "chart" "note" (func $note (param i32 i32)))
                (import "chart" "order" (func $order (param i32 f64 f64 f64 f64)))
                (memory (export "memory") 1)
                (data (i32.const 0) "Probe")
                (data (i32.const 8) "At")
                (data (i32.const 16) "sma")
                (data (i32.const 24) "nope")
                (data (i32.const 32) "Bought")
                (global $bars (mut i32) (i32.const 0))
                (func (export "chart_abi") (result i32) (i32.const 1))
                (func (export "chart_settings") (call $setting (i32.const 8) (i32.const 2) (f64.const 2)))
                (func (export "chart_name") (call $name (i32.const 0) (i32.const 5)))
                (func (export "chart_start") (global.set $bars (i32.const 0)))
                (func (export "chart_push") (param i64 f64 f64 f64 f64 f64)
                    (global.set $bars (i32.add (global.get $bars) (i32.const 1))))
                (func (export "chart_on_bar") (param f64 f64)
                    {extra}
                    (call $plot (i32.const 16) (i32.const 3)
                        (call $indicator (i32.const 16) (i32.const 3) (f64.const 2) (f64.const 0) (f64.const 0)
                            (i32.const 0) (i32.const 0)))
                    (if (i32.eq (global.get $bars) (i32.add (i32.trunc_f64_s (call $value (i32.const 0))) (i32.const 1)))
                        (then (call $note (i32.const 32) (i32.const 6))
                            (call $order (i32.const 0) (f64.const 1) (f64.const nan) (f64.const nan) (f64.const nan))))))"#
        )
    }

    #[test]
    fn a_module_names_itself_takes_settings_reads_indicators_and_trades() {
        let data = bars(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let mut strategy = WasmStrategy::new("probe", probe("").as_bytes()).unwrap();
        assert_eq!(strategy.name(), "Probe");
        let run = backtest(&mut strategy, data.view(), Account::default());
        // Bought at the close of the third bar, filled at the open of the fourth.
        let fills = run.account.fills();
        assert_eq!(fills.len(), 1);
        assert_eq!(
            (fills[0].time, fills[0].price, fills[0].qty),
            (Time(3 * 60_000), 13.0, 1.0)
        );
        // The average the chart would show, bar by bar.
        assert_eq!(run.plots[0].name, "sma");
        assert!(run.plots[0].values[0].is_nan());
        assert_eq!(run.plots[0].values[1..], [10.5, 11.5, 12.5, 13.5]);
        // Why, at the bar that decided, and the average it read.
        assert_eq!(run.notes, [(Time(2 * 60_000), "Bought".to_string())]);
        assert_eq!(run.inputs[0].study.label(), "SMA 2");
        assert_eq!(run.inputs[0].lines[0][1..], [10.5, 11.5, 12.5, 13.5]);
        // A setting changed in the program reaches the module.
        let mut settings = strategy.settings();
        assert_eq!(settings[0].name, "At");
        *settings[0].value = 3.0;
        let run = backtest(&mut strategy, data.view(), Account::default());
        assert_eq!(run.account.fills()[0].time, Time(4 * 60_000));
    }

    #[test]
    fn a_module_that_fails_stops_trading_and_says_why() {
        let data = bars(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let unknown = r#"(drop (call $indicator (i32.const 24) (i32.const 4) (f64.const 0) (f64.const 0)
            (f64.const 0) (i32.const 0) (i32.const 0)))"#;
        let setting = "(call $setting (i32.const 8) (i32.const 2) (f64.const 1))";
        // Forty lines of other names: the first one to forty bytes of the memory.
        let plots = r#"(local $i i32)
            (loop (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (call $plot (i32.const 0) (local.get $i) (f64.const 1))
                (br_if 0 (i32.lt_u (local.get $i) (i32.const 40))))"#;
        for (extra, why) in [
            ("(unreachable)", "unreachable"),
            ("(loop (br 0))", "too long"),
            (unknown, "no indicator \"nope\""),
            (setting, "chart_settings"),
            (plots, "more than 32 lines"),
        ] {
            let mut strategy = WasmStrategy::new("probe", probe(extra).as_bytes()).unwrap();
            // Less work, so the one that never stops is stopped soon.
            strategy.fuel = 100_000;
            let run = backtest(&mut strategy, data.view(), Account::default());
            assert!(run.account.fills().is_empty(), "{extra}");
            let stopped = strategy.stopped().unwrap_or_default();
            assert!(stopped.contains(why), "{extra}: {stopped}");
            assert!(strategy.name().starts_with("Probe (stopped: "));
        }
    }

    #[test]
    fn a_module_slow_at_every_bar_is_stopped_within_the_run_s_work() {
        let data = bars(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let busy = r#"(local $i i32)
            (loop (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if 0 (i32.lt_u (local.get $i) (i32.const 100000))))"#;
        let mut strategy = WasmStrategy::new("probe", probe(busy).as_bytes()).unwrap();
        // Each bar is within a call's work; the run's is gone after the first.
        (strategy.fuel, strategy.run_fuel) = (10_000_000, 0);
        backtest(&mut strategy, data.view(), Account::default());
        assert_eq!(strategy.stopped(), Some("it worked too long in this run"));
        strategy.run_fuel = RUN_FUEL;
        let run = backtest(&mut strategy, data.view(), Account::default());
        assert_eq!(strategy.stopped(), None);
        assert_eq!(run.account.fills().len(), 1);
    }

    #[test]
    fn a_module_cannot_take_more_than_one_memory_and_table() {
        let abi = r#"(func (export "chart_abi") (result i32) (i32.const 1))"#;
        let err = |body: &str| {
            WasmStrategy::new("x", format!("(module {body} {abi})").as_bytes())
                .err()
                .unwrap_or_default()
        };
        assert!(!err("(memory 1) (memory 1)").is_empty());
        assert!(!err("(table 10000000 funcref)").is_empty());
        assert!(!err("(table 1 funcref) (table 1 funcref)").is_empty());
        assert!(err("(memory 1) (table 10 funcref)").contains("no chart_name"));
    }

    #[test]
    fn what_is_not_a_strategy_is_not_loaded() {
        let other = r#"(module (func (export "chart_abi") (result i32) (i32.const 7)))"#;
        let err = |wasm: &str| WasmStrategy::new("x", wasm.as_bytes()).err().unwrap_or_default();
        assert!(err(other).contains("version 7"));
        assert!(err("(module)").contains("no chart_abi"));
        assert!(!err("not wasm").is_empty());
    }
}
