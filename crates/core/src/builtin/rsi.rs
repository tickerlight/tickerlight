use core::ops::Range;

use crate::plugin::order;
use crate::{Align, CandleView, Chart, Color, DrawCtx, Layer, Panes, Plugin, Resources};

/// Bars before the visible ones that the smoothing is warmed up on. Wilder's smoothing never forgets,
/// but after this many periods what is left of the start is far below a pixel.
const WARMUP_PERIODS: usize = 10;

/// Relative strength index in a pane of its own under the candles: the example of an indicator with
/// its own scale. Not in `DefaultPlugins`: `chart.add_plugin(Rsi::new(14))`.
#[derive(Clone, Copy)]
pub struct Rsi {
    pub period: usize,
    pub color: Color,
    /// Name of the pane, and of the layer in it. Give every RSI on one chart its own.
    pub name: &'static str,
    /// Height of the pane relative to the main one (`1.0`).
    pub weight: f32,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        Self {
            period: period.max(2),
            color: crate::palette::VIOLET,
            name: "rsi",
            weight: 0.35,
        }
    }

    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }
}

impl Plugin for Rsi {
    fn build(&self, chart: &mut Chart) {
        chart.add_pane(self.name, self.weight);
        chart.add_layer_in(Panes::One(self.name), self.name, order::SERIES, *self);
    }
}

/// The RSI at every bar of `range` that has one: from `period` bars after where the calculation
/// starts (the start of the data, or a warm-up before the range).
fn values(view: CandleView<'_>, period: usize, range: Range<usize>) -> Vec<(usize, f64)> {
    let close = view.close();
    let end = range.end.min(close.len());
    let from = range.start.saturating_sub(WARMUP_PERIODS * period);
    if end <= from + period {
        return Vec::new();
    }
    let rsi = |gain: f64, loss: f64| match (gain, loss) {
        (0.0, 0.0) => 50.0,
        (_, 0.0) => 100.0,
        (g, l) => 100.0 - 100.0 / (1.0 + g / l),
    };
    let n = period as f64;
    let (mut gain, mut loss) = (0.0, 0.0);
    for i in from + 1..=from + period {
        let change = close[i] - close[i - 1];
        gain += change.max(0.0);
        loss += (-change).max(0.0);
    }
    gain /= n;
    loss /= n;
    let mut out = Vec::new();
    let mut push = |i: usize, gain: f64, loss: f64| {
        if range.contains(&i) {
            out.push((i, rsi(gain, loss)));
        }
    };
    push(from + period, gain, loss);
    for i in from + period + 1..end {
        let change = close[i] - close[i - 1];
        gain = (gain * (n - 1.0) + change.max(0.0)) / n;
        loss = (loss * (n - 1.0) + (-change).max(0.0)) / n;
        push(i, gain, loss);
    }
    out
}

impl Layer for Rsi {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let (plot_w, _) = ctx.plot;
        let style = ctx.style;
        let unit = ctx.line();
        let (y30, y70) = (ctx.price.y_of(30.0) as f32, ctx.price.y_of(70.0) as f32);
        ctx.scene
            .fill(0.0, y70, plot_w, y30 - y70, Color { a: 18, ..self.color });
        for y in [y30, y70] {
            ctx.scene.fill(0.0, y.floor(), plot_w, unit, style.axis);
        }
        if ctx.view.is_empty() {
            return;
        }

        let stride = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let width = ctx.px(1.5);
        let mut previous = None;
        let mut last = None;
        for (i, value) in values(ctx.view, self.period, ctx.visible.clone()) {
            last = Some(value);
            if i % stride != 0 {
                continue;
            }
            let point = (ctx.time.x_of(i as f64 + 0.5) as f32, ctx.price.y_of(value) as f32);
            if let Some(from) = previous {
                ctx.scene.line(from, point, width, self.color);
            }
            previous = Some(point);
        }
        let text = match last {
            Some(v) => format!("RSI {}  {v:.1}", self.period),
            None => format!("RSI {}", self.period),
        };
        ctx.scene.text(
            ctx.px(12.0),
            ctx.px(14.0),
            ctx.px(style.font_size),
            style.axis_text,
            Align::Left,
            text,
        );
    }

    /// A fixed scale, so 30 and 70 stay where the eye expects them.
    fn price_extent(&self, view: CandleView<'_>, _: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
        (!view.is_empty()).then_some((0.0, 100.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles, Time};

    fn closes(values: &[f64]) -> Candles {
        let mut c = Candles::new();
        for (i, &v) in values.iter().enumerate() {
            c.push(Candle {
                time: Time(i as i64),
                open: v,
                high: v,
                low: v,
                close: v,
                volume: 0.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn matches_wilders_worked_example() {
        let data = closes(&[
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03, 45.61, 46.28, 46.28,
            46.00, 46.03, 46.41, 46.22, 45.64,
        ]);
        let v = values(data.view(), 14, 0..20);
        assert_eq!(v.iter().map(|x| x.0).collect::<Vec<_>>(), [14, 15, 16, 17, 18, 19]);
        for (got, want) in v.iter().map(|x| x.1).zip([70.53, 66.32, 66.55, 69.41, 66.36, 57.97]) {
            assert!((got - want).abs() < 0.2, "{got} vs {want}"); // the published table rounds its intermediate values
        }
    }

    #[test]
    fn extremes_and_flat() {
        let up = closes(&(0..30).map(f64::from).collect::<Vec<_>>());
        assert!(values(up.view(), 14, 0..30).iter().all(|x| x.1 == 100.0));
        let down = closes(&(0..30).rev().map(f64::from).collect::<Vec<_>>());
        assert!(values(down.view(), 14, 0..30).iter().all(|x| x.1 == 0.0));
        let flat = closes(&[5.0; 30]);
        assert!(values(flat.view(), 14, 0..30).iter().all(|x| x.1 == 50.0));
        assert!(values(flat.view(), 14, 0..10).is_empty(), "not enough bars yet");
    }

    #[test]
    fn a_window_far_from_the_start_agrees_with_the_full_calculation() {
        let prices: Vec<f64> = (0..2000)
            .map(|i| 100.0 + (i as f64 * 0.37).sin() * 5.0 + (i as f64 * 0.011).cos() * 9.0)
            .collect();
        let data = closes(&prices);
        let full = values(data.view(), 14, 0..2000);
        let window = values(data.view(), 14, 1500..1600);
        assert_eq!(window.len(), 100);
        for (i, v) in window {
            assert!((full[i - 14].1 - v).abs() < 1e-3, "bar {i}");
        }
    }
}
