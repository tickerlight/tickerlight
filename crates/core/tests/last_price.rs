//! The last price: a line across the plot and a label on the price axis, counting down to the bar's close.

use tickerlight_core::builtin::Clock;
use tickerlight_core::{Candle, Candles, Chart, Scene, Time};

const MINUTE: i64 = 60_000;

/// 100 one-minute bars; the last opens at 100.0 and closes at 104.5.
fn chart() -> Chart {
    let mut candles = Candles::new();
    for i in 0..100 {
        let p = 100.0 + (i % 7) as f64;
        let close = if i == 99 { 104.5 } else { p + 0.5 };
        candles
            .push(Candle {
                time: Time(i * MINUTE),
                open: p,
                high: p + 6.0,
                low: p - 1.0,
                close,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut chart = Chart::new(candles);
    chart.resize(872.0, 528.0); // plot 800 x 500
    chart
}

fn axis_texts(c: &mut Chart) -> Vec<tickerlight_core::Text> {
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let plot_w = c.plot_size().0;
    scene.texts.into_iter().filter(|t| t.x > plot_w).collect()
}

#[test]
fn the_last_close_is_labelled_on_the_axis_and_no_tick_is_under_it() {
    let mut c = chart();
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let plot_w = c.plot_size().0;
    let label = scene
        .texts
        .iter()
        .find(|t| t.x > plot_w && t.text == "104.50")
        .expect("the last close on the axis");
    // On a box in the color of the last bar, which rose, in ink that reads on it.
    assert_eq!(label.color, c.style.candle.up.ink());
    let up = c.style.candle.up;
    let tag = scene
        .rects
        .iter()
        .find(|r| r.x > plot_w && r.color == up && r.y <= label.y && r.y + r.h >= label.y)
        .expect("the label's box");
    // The dotted line across the plot, at the label.
    let dots = scene
        .rects
        .iter()
        .filter(|r| r.x < plot_w && r.color == up && r.w <= 1.0 && r.h <= 1.0 && (r.y - label.y).abs() <= 1.0)
        .count();
    assert!(dots > 100, "{dots} dots");
    let size = c.style.font_size;
    let ticks = scene
        .texts
        .iter()
        .filter(|t| t.x > plot_w && t.color == c.style.axis_text);
    for t in ticks {
        assert!(
            t.y + size / 2.0 <= tag.y || t.y - size / 2.0 >= tag.y + tag.h,
            "{} at {} is under the label",
            t.text,
            t.y
        );
    }
}

#[test]
fn the_label_counts_down_to_the_close_while_the_clock_is_in_the_last_bar() {
    let mut c = chart();
    let has = |c: &mut Chart, text: &str| axis_texts(c).iter().any(|t| t.text == text);
    assert!(!has(&mut c, "00:18"), "no clock, no countdown");
    // The last bar opens at minute 99 and closes at minute 100: 18 s left.
    c.resources_mut().insert(Clock(Time(100 * MINUTE - 18_000)));
    assert!(has(&mut c, "00:18"));
    // Past its close the data are not live: nothing to count down to.
    c.resources_mut().insert(Clock(Time(100 * MINUTE + 1)));
    assert!(!axis_texts(&mut c).iter().any(|t| t.text.contains(':')));
}
