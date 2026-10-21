# Writing a strategy

A strategy is written in Rust and built to WebAssembly. Tickerlight loads the `.wasm` file from its
`strategies` folder and runs it without being rebuilt. The strategy runs sandboxed: no files, no network, one
memory of at most 256 MB, a cap on the work done at a bar and in a whole run, at most 32 plotted lines and 64
indicators read in a run. It sees the bars only up to the one that just closed, as the built-in strategies do,
so a backtest cannot look ahead.

The same file runs everywhere the program runs a strategy: the backtest on the chart, the replay (its trades come
out as the bars do), the test period kept apart, the walk forward, and paper trading on a live chart.

## A new strategy

```sh
rustup target add wasm32-unknown-unknown
cargo new --lib my-strategy
```

`Cargo.toml`:

```toml
[package]
name = "my-strategy"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
tickerlight-strategy = { git = "https://github.com/tickerlight/tickerlight", tag = "v0.1.1" }

[profile.release]
opt-level = "s"
```

(In a clone of this repository, `{ path = "<the clone>/crates/strategy" }` does the same.)

`src/lib.rs`, a channel breakout (the code of
[`crates/strategy/examples/breakout.rs`](../crates/strategy/examples/breakout.rs)):

```rust
use tickerlight_strategy::{Ctx, Exits, Strategy};

#[derive(Default)]
struct Breakout;

impl Strategy for Breakout {
    const SETTINGS: &'static [(&'static str, f64)] = &[("Length", 20.0), ("Risk %", 1.0), ("Min width %", 1.0)];

    fn name(settings: &[f64]) -> String {
        format!("Channel breakout {}", settings[0])
    }

    fn on_bar(&mut self, ctx: &mut Ctx) {
        let (length, risk) = (ctx.setting(0).round().max(2.0), ctx.setting(1) / 100.0);
        // The channel of the bars before this one: this close breaks out of it or not.
        let upper = ctx.indicator("dc", &[length], 1, 1);
        let lower = ctx.indicator("dc", &[length], 2, 1);
        let middle = ctx.indicator("dc", &[length], 0, 1);
        ctx.plot("Upper", upper);
        ctx.plot("Lower", lower);
        let close = ctx.bar().close;
        let position = ctx.position();
        if position > 0.0 && close < middle || position < 0.0 && close > middle {
            ctx.close_position();
            return;
        }
        // A channel narrower than this (on 1m bars, most are) puts the stop a few fees away: 0.1% a fill makes
        // every trade cost 0.2% of what it trades, which the breakouts do not make back. No channel yet is NaN.
        let wide = upper - lower >= close * ctx.setting(2) / 100.0;
        if position != 0.0 || !wide {
            return;
        }
        // The size that risks `risk` of the equity at the stop, but no more than the equity buys.
        let most = ctx.equity() / close;
        if close > upper {
            let exits = Exits { stop_loss: Some(lower), take_profit: None };
            ctx.buy(ctx.risk_size(risk, close, lower).min(most), exits);
        } else if close < lower {
            let exits = Exits { stop_loss: Some(upper), take_profit: None };
            ctx.sell(ctx.risk_size(risk, close, upper).min(most), exits);
        }
    }
}

tickerlight_strategy::export!(Breakout);
```

Build it and put the file in the `strategies` folder of the data folder: `$XDG_DATA_HOME/tickerlight`, by default
`~/.local/share/tickerlight`, on Linux, `~/Library/Application Support/tickerlight` on macOS,
`%APPDATA%\tickerlight` on Windows, or the folder `TICKERLIGHT_DATA_DIR` names (the tooltip of "Read the strategy
files again" shows it). The program makes the `strategies` folder when it starts; on Linux, before the first
start:

```sh
cargo build --release --target wasm32-unknown-unknown
mkdir -p ~/.local/share/tickerlight/strategies
cp target/wasm32-unknown-unknown/release/my_strategy.wasm ~/.local/share/tickerlight/strategies/
```

The files there are read when the program starts; after adding or rebuilding one, in the program: Strategy, then
"Read the strategy files again". It is listed by its `name`, with a field for each of its `SETTINGS`.

## What a strategy sees and does

`on_bar` is called at the close of every bar, the oldest first. The strategy starts afresh (`Default`) for each
run.

| `Ctx` | |
| --- | --- |
| `bars()`, `bar()`, `index()` | The bars up to and including this one (time in ms since 1970 UTC, open, high, low, close, volume). |
| `setting(i)` | The value of setting `i` as the user set it. |
| `position()` | The signed size of the position, with this bar's market orders: positive long, negative short. |
| `equity()` | What the account is worth at this close. |
| `indicator(key, params, column, ago)` | Line `column` of an indicator `ago` bars back, as the chart shows it; NaN where there is none. |
| `line(key, params)` | The first line of an indicator now and a bar ago. |
| `buy(qty, exits)`, `sell(qty, exits)` | A market order, filled at the next open, with a stop loss and a take profit for what it opens. |
| `limit(qty, price, exits)`, `stop(qty, price, exits)` | Orders filled on a later bar; `qty` positive buys. |
| `close_position()`, `cancel_orders()` | Closes at the next open; takes off the orders waiting. |
| `risk_size(risk, entry, stop)` | The size that loses `risk` (0.01 for 1%) of the equity from `entry` to `stop`. No cap: on a narrow stop it can be many times the equity. |
| `plot(name, value)` | A line over the candles. |
| `note(text)` | Why it does what it does, shown with the trade this bar decided. |

Indicator keys: `sma`, `ema`, `wma`, `vwap`, `bb`, `dc`, `rsi`, `macd`, `atr`, `stoch`, `obv`, `cci`, `mfi`,
`adx`, `supertrend`, `pivots`, `vol`, `ichimoku`, `sessions`, `session`. `params` are the indicator's first three
settings at most, in the order the program lists them; a setting left out keeps its default. An unknown key stops
the strategy with a message.

## When it stops

A panic, an unknown indicator, or too much work at one bar stops the strategy for the run; the program shows it as
"name (stopped: why)". The text of a `plot` or a `note` is cut at 256 bytes. A strategy may use any crate that
builds to `wasm32-unknown-unknown` with no system calls.
