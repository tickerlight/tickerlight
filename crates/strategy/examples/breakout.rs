//! A channel breakout, built to WebAssembly for the program to load:
//!
//! ```text
//! cargo build --release --target wasm32-unknown-unknown -p tickerlight-strategy --example breakout
//! cp target/wasm32-unknown-unknown/release/examples/breakout.wasm <data>/strategies/
//! ```
//!
//! Long when the close breaks over the highest high of the bars before, short when under the lowest low; the
//! stop at the other side of the channel, the size risking a share of the equity there. Out when the close breaks
//! the middle of the channel the other way.

use tickerlight_strategy::{Ctx, Exits, Strategy};

#[derive(Default)]
struct Breakout;

impl Strategy for Breakout {
    const SETTINGS: &'static [(&'static str, f64)] = &[("Length", 20.0), ("Risk %", 1.0)];

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
        if position != 0.0 || !upper.is_finite() || !lower.is_finite() {
            return;
        }
        if close > upper {
            let exits = Exits {
                stop_loss: Some(lower),
                take_profit: None,
            };
            ctx.buy(ctx.risk_size(risk, close, lower), exits);
        } else if close < lower {
            let exits = Exits {
                stop_loss: Some(upper),
                take_profit: None,
            };
            ctx.sell(ctx.risk_size(risk, close, upper), exits);
        }
    }
}

tickerlight_strategy::export!(Breakout);
