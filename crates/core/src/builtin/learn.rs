//! Learning from the bars as they come: a model fitted at a bar only on what was known there, so a backtest or a
//! replay of a learned strategy has no lookahead either.

use super::strategy::{Setting, Strategy, StrategyCtx, backtest};
use super::studies::Kind;
use super::trading::{Account, Exits};
use crate::CandleView;

/// Logistic regression with an L2 penalty: the chance of an outcome from a few numbers, fitted by Newton's
/// method on features standardized over the samples.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Logistic {
    mean: Vec<f64>,
    scale: Vec<f64>,
    /// The bias, then a weight per feature.
    weights: Vec<f64>,
}

impl Logistic {
    /// Fits `outcomes` from `samples` (as many features each). `penalty` pulls the weights toward zero: more for
    /// fewer or noisier samples. No samples give a model that says one half.
    pub fn fit(samples: &[&[f64]], outcomes: &[bool], penalty: f64) -> Self {
        let Some(n) = samples.first().map(|s| s.len()) else {
            return Self::default();
        };
        let count = samples.len() as f64;
        let mut mean = vec![0.0; n];
        for s in samples {
            for (m, x) in mean.iter_mut().zip(*s) {
                *m += x / count;
            }
        }
        let mut scale = vec![0.0; n];
        for s in samples {
            for ((v, x), m) in scale.iter_mut().zip(*s).zip(&mean) {
                *v += (x - m).powi(2) / count;
            }
        }
        // A feature that does not change is left out (its scale makes it zero).
        let scale: Vec<f64> = scale
            .into_iter()
            .map(|v| if v > 1e-24 { 1.0 / v.sqrt() } else { 0.0 })
            .collect();
        let mut model = Self {
            mean,
            scale,
            weights: vec![0.0; n + 1],
        };
        let d = n + 1;
        let mut x = vec![0.0; d];
        for _ in 0..25 {
            let mut gradient = vec![0.0; d];
            let mut hessian = vec![0.0; d * d];
            for (s, &y) in samples.iter().zip(outcomes) {
                model.standardize(s, &mut x);
                let p = sigmoid(dot(&model.weights, &x));
                let (error, curve) = (p - f64::from(u8::from(y)), p * (1.0 - p));
                for a in 0..d {
                    gradient[a] += error * x[a];
                    for b in 0..d {
                        hessian[a * d + b] += curve * x[a] * x[b];
                    }
                }
            }
            // The bias is not pulled toward zero.
            for a in 1..d {
                gradient[a] += penalty * model.weights[a];
                hessian[a * d + a] += penalty;
            }
            let Some(step) = solve(&mut hessian, &mut gradient, d) else {
                break;
            };
            for (w, s) in model.weights.iter_mut().zip(&step) {
                *w -= s;
            }
            if step.iter().all(|s| s.abs() < 1e-9) {
                break;
            }
        }
        model
    }

    /// The chance of the outcome for `features`; one half from a model fitted on nothing.
    pub fn chance(&self, features: &[f64]) -> f64 {
        if self.weights.is_empty() {
            return 0.5;
        }
        let mut x = vec![0.0; self.weights.len()];
        self.standardize(features, &mut x);
        sigmoid(dot(&self.weights, &x))
    }

    /// `features` as the model takes them, after a one for the bias.
    fn standardize(&self, features: &[f64], x: &mut [f64]) {
        x[0] = 1.0;
        for (i, f) in features.iter().enumerate() {
            x[i + 1] = (f - self.mean[i]) * self.scale[i];
        }
    }
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

/// Solves `m x = v` for `x` (`m` is `d` by `d`, row by row) by elimination with partial pivoting; none if `m` is
/// singular. Both are used up.
fn solve(m: &mut [f64], v: &mut [f64], d: usize) -> Option<Vec<f64>> {
    for col in 0..d {
        let pivot = (col..d).max_by(|&a, &b| m[a * d + col].abs().total_cmp(&m[b * d + col].abs()))?;
        if m[pivot * d + col].abs() < 1e-12 {
            return None;
        }
        if pivot != col {
            for k in 0..d {
                m.swap(pivot * d + k, col * d + k);
            }
            v.swap(pivot, col);
        }
        for row in col + 1..d {
            let f = m[row * d + col] / m[col * d + col];
            for k in col..d {
                m[row * d + k] -= f * m[col * d + k];
            }
            v[row] -= f * v[col];
        }
    }
    let mut x = vec![0.0; d];
    for row in (0..d).rev() {
        let rest: f64 = (row + 1..d).map(|k| m[row * d + k] * x[k]).sum();
        x[row] = (v[row] - rest) / m[row * d + row];
    }
    x.iter().all(|x| x.is_finite()).then_some(x)
}

/// How many numbers [`bar_features`] describes a bar with.
pub const FEATURES: usize = 6;

/// What the numbers of [`bar_features`] are, in their order: the log return of the bar, of the one before and of
/// the one before that; RSI 14 over 100 less a half; the close less its 20-bar average, and the bar's body, in
/// ATR 14s.
pub const FEATURE_NAMES: [&str; FEATURES] = [
    "return",
    "return_1_back",
    "return_2_back",
    "rsi",
    "from_average",
    "body",
];

/// What is known of the bar at its close, for a model: the numbers of [`FEATURE_NAMES`], NaN while the bars before
/// are too few. A model trained elsewhere on [`features_csv`] reads the same numbers here.
pub fn bar_features(ctx: &mut StrategyCtx<'_>) -> [f64; FEATURES] {
    let (rsi, _) = ctx.line(Kind::Rsi, &[14.0]);
    let (average, _) = ctx.line(Kind::Sma, &[20.0]);
    let (atr, _) = ctx.line(Kind::Atr, &[14.0]);
    let bars = ctx.bars();
    let (close, i) = (bars.close(), ctx.index());
    let back = |k: usize| {
        if i >= k {
            (close[i + 1 - k] / close[i - k]).ln()
        } else {
            f64::NAN
        }
    };
    [
        back(1),
        back(2),
        back(3),
        rsi / 100.0 - 0.5,
        (ctx.close() - average) / atr,
        (ctx.close() - ctx.open()) / atr,
    ]
}

/// The features of the bars and whether each next close was higher, as CSV to train a model on elsewhere: a header,
/// then a row for each bar whose features are all known and whose next close is (time in ms since 1970, the
/// numbers of [`FEATURE_NAMES`], `higher_next` 1 or 0).
pub fn features_csv(bars: CandleView<'_>) -> String {
    struct Recorder(Vec<[f64; FEATURES]>);
    impl Strategy for Recorder {
        fn name(&self) -> String {
            "Features".into()
        }

        fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
            self.0.push(bar_features(ctx));
        }
    }
    let mut recorder = Recorder(Vec::with_capacity(bars.len()));
    backtest(&mut recorder, bars, Account::default());
    let mut csv = format!("time,{},higher_next\n", FEATURE_NAMES.join(","));
    let (time, close) = (bars.time(), bars.close());
    for (i, features) in recorder.0.iter().enumerate().take(bars.len().saturating_sub(1)) {
        if features.iter().all(|x| x.is_finite()) {
            csv += &time[i].0.to_string();
            for x in features {
                csv += &format!(",{x}");
            }
            csv += if close[i + 1] > close[i] { ",1\n" } else { ",0\n" };
        }
    }
    csv
}

/// Goes with all of the equity the way a model is sure enough of: long when `up`, its chance of a higher next close,
/// is at least `sure` percent (a half to all), short when the chance of a lower one is; notes the chance.
pub fn go_with(ctx: &mut StrategyCtx<'_>, up: f64, sure: f64) {
    let sure = (sure / 100.0).clamp(0.5, 1.0);
    let want = if up >= sure {
        1.0
    } else if 1.0 - up >= sure {
        -1.0
    } else {
        return;
    };
    if ctx.position().signum() == want {
        return;
    }
    let size = ctx.equity().max(0.0) / ctx.close();
    let qty = want * size - ctx.position();
    ctx.note(&format!(
        "A higher close next: {:.0}% (sure at {:.0}%)",
        up * 100.0,
        sure * 100.0
    ));
    if qty > 0.0 {
        ctx.buy(qty, Exits::default());
    } else {
        ctx.sell(qty, Exits::default());
    }
}

/// Learns from the bars so far whether the next close is higher, and goes with all of the equity the way it is
/// sure enough of: logistic regression on the last returns, the RSI and where the close is against its average and
/// its bar, in ATRs. It fits again every so many bars on the bars before (the outcome of a bar is known only at the
/// next close), so a backtest is what it would have done live. An example of a model trained and run as the bars
/// come.
#[derive(Debug, Clone, PartialEq)]
pub struct LearnedDirection {
    /// How many bars back it learns from.
    pub window: f64,
    /// How many bars it trades with a model before it fits a new one.
    pub refit: f64,
    /// How sure it has to be, percent: long at this chance of a higher close or more, short at as much of a lower.
    pub sure: f64,
    features: Vec<[f64; FEATURES]>,
    model: Logistic,
    /// Bars since the model was fitted, none before the first.
    fitted: Option<usize>,
}

impl Default for LearnedDirection {
    fn default() -> Self {
        Self {
            window: 500.0,
            refit: 50.0,
            sure: 55.0,
            features: Vec::new(),
            model: Logistic::default(),
            fitted: None,
        }
    }
}

impl LearnedDirection {
    /// The fewest samples it fits on.
    const LEAST: usize = 50;

    /// Fits on the bars of the window whose next close is known: all before bar `i`.
    fn fit(&mut self, close: &[f64], i: usize) {
        let from = i.saturating_sub(self.window.max(1.0) as usize);
        let (mut samples, mut outcomes) = (Vec::new(), Vec::new());
        for j in from..i {
            let f = &self.features[j];
            if f.iter().all(|x| x.is_finite()) {
                samples.push(&f[..]);
                outcomes.push(close[j + 1] > close[j]);
            }
        }
        if samples.len() >= Self::LEAST {
            self.model = Logistic::fit(&samples, &outcomes, 1.0);
            self.fitted = Some(0);
        }
    }
}

impl Strategy for LearnedDirection {
    fn name(&self) -> String {
        format!("Learned direction {}/{}", self.window, self.sure)
    }

    fn settings(&mut self) -> Vec<Setting<'_>> {
        vec![
            Setting {
                name: "Learns from bars",
                value: &mut self.window,
            },
            Setting {
                name: "Fits every bars",
                value: &mut self.refit,
            },
            Setting {
                name: "Sure %",
                value: &mut self.sure,
            },
        ]
    }

    fn another(&self) -> Option<Box<dyn Strategy>> {
        Some(Box::new(self.clone()))
    }

    fn start(&mut self) {
        self.features.clear();
        self.model = Logistic::default();
        self.fitted = None;
    }

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
        let features = bar_features(ctx);
        self.features.push(features);
        let i = ctx.index();
        if self.fitted.is_none_or(|n| n + 1 >= self.refit.max(1.0) as usize) {
            self.fit(ctx.bars().close(), i);
        } else {
            self.fitted = self.fitted.map(|n| n + 1);
        }
        if self.fitted.is_none() || !features.iter().all(|x| x.is_finite()) {
            return;
        }
        go_with(ctx, self.model.chance(&features), self.sure);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::{Account, backtest};
    use crate::{Candle, Candles, Time};

    #[test]
    fn logistic_regression_finds_the_side_an_outcome_falls_on() {
        // The outcome is whether the first feature is over a half, with a little overlap; the second is noise.
        let samples: Vec<[f64; 2]> = (0..400)
            .map(|k| {
                let a = (k as f64 * 0.618).fract();
                [a, (k as f64 * 0.381).fract()]
            })
            .collect();
        let outcomes: Vec<bool> = samples
            .iter()
            .enumerate()
            .map(|(k, s)| s[0] > 0.5 || k % 17 == 0)
            .collect();
        let refs: Vec<&[f64]> = samples.iter().map(|s| &s[..]).collect();
        let model = Logistic::fit(&refs, &outcomes, 1.0);
        assert!(model.chance(&[0.9, 0.5]) > 0.9);
        assert!(model.chance(&[0.1, 0.5]) < 0.2);
        assert!((model.chance(&[0.9, 0.1]) - model.chance(&[0.9, 0.9])).abs() < 0.1);
        assert_eq!(Logistic::fit(&[], &[], 1.0).chance(&[1.0]), 0.5);
    }

    fn bars(closes: &[f64]) -> Candles {
        let mut candles = Candles::new();
        for (i, &c) in closes.iter().enumerate() {
            let open = if i > 0 { closes[i - 1] } else { c };
            let candle = Candle {
                time: Time(i as i64 * 60_000),
                open,
                high: c.max(open) + 0.5,
                low: c.min(open) - 0.5,
                close: c,
                volume: 1.0,
            };
            candles.push(candle).unwrap();
        }
        candles
    }

    #[test]
    fn it_learns_a_pattern_from_the_bars_before_and_trades_it() {
        // Every rise is followed by a fall and the other way round, over a slow drift.
        let closes: Vec<f64> = (0..400)
            .map(|i| 100.0 + i as f64 * 0.01 + if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let data = bars(&closes);
        let mut learned = LearnedDirection::default();
        let fills = backtest(&mut learned, data.view(), Account::default())
            .account
            .fills()
            .to_vec();
        assert!(fills.len() > 100, "{}", fills.len());
        // Each order goes against the bar it was placed at: it fills at the next open.
        for fill in &fills {
            let k = (fill.time.0 / 60_000) as usize;
            let rose = closes[k - 1] > closes[k - 2];
            assert_eq!(fill.qty < 0.0, rose, "bar {k}");
        }
    }

    #[test]
    fn what_it_did_up_to_a_bar_does_not_depend_on_the_bars_after() {
        let closes: Vec<f64> = (0..900)
            .map(|i| 100.0 + (i as f64 * 0.37).sin() * 3.0 + (i as f64 * 0.05).cos())
            .collect();
        let all = bars(&closes);
        let full = backtest(&mut LearnedDirection::default(), all.view(), Account::default());
        for cut in [300, 555, 800] {
            let part = bars(&closes[..cut]);
            let early = backtest(&mut LearnedDirection::default(), part.view(), Account::default());
            let last = Time((cut as i64 - 1) * 60_000);
            let same: Vec<_> = full
                .account
                .fills()
                .iter()
                .filter(|f| f.time <= last)
                .copied()
                .collect();
            assert_eq!(early.account.fills(), &same[..], "cut at {cut}");
        }
    }

    #[test]
    fn the_features_are_written_for_each_bar_they_and_its_next_close_are_known_at() {
        let closes: Vec<f64> = (0..120).map(|i| 100.0 + (i as f64 * 0.37).sin() * 3.0).collect();
        let data = bars(&closes);
        let csv = features_csv(data.view());
        let mut lines = csv.lines();
        assert_eq!(
            lines.next(),
            Some("time,return,return_1_back,return_2_back,rsi,from_average,body,higher_next")
        );
        let rows: Vec<Vec<f64>> = lines
            .map(|l| l.split(',').map(|x| x.parse().unwrap()).collect())
            .collect();
        // From the first bar with a 20-bar average to the one before the last.
        let bar = |row: &[f64]| (row[0] / 60_000.0) as usize;
        assert_eq!((bar(&rows[0]), bar(&rows[rows.len() - 1]), rows.len()), (19, 118, 100));
        for row in &rows {
            let i = bar(row);
            assert_eq!(row.len(), 2 + FEATURES);
            assert!((row[1] - (closes[i] / closes[i - 1]).ln()).abs() < 1e-12);
            assert_eq!(row[7] == 1.0, closes[i + 1] > closes[i], "bar {i}");
        }
    }
}
