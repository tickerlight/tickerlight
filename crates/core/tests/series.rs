//! The kinds of the price series.

use tickerlight_core::builtin::SeriesKind;
use tickerlight_core::plugin::commands::SERIES_CYCLE;
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(1000));
    chart.resize(872.0, 628.0);
    chart
}

/// The scene of the candles layer only: the scene layer that follows the volume bars.
fn scene(chart: &mut Chart) -> Scene {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
}

fn kind(chart: &Chart) -> SeriesKind {
    *chart.resources().get::<SeriesKind>().unwrap()
}

#[test]
fn the_cycle_goes_through_the_kinds_and_every_kind_draws_something_inside_the_plot() {
    let mut c = chart();
    let mut kinds = vec![kind(&c)];
    let mut previous = scene(&mut c);
    for _ in 0..5 {
        c.handle(Event::Command(SERIES_CYCLE));
        kinds.push(kind(&c));
        let now = scene(&mut c);
        assert!(!now.rects.is_empty() || !now.lines.is_empty());
        if kinds.len() <= 5 {
            assert!(
                now.rects != previous.rects || now.lines != previous.lines,
                "{:?} draws differently from {:?}",
                kinds.last(),
                kinds[kinds.len() - 2]
            );
        }
        previous = now;
    }
    assert_eq!(kinds[0], kinds[5], "a full cycle");
    assert_eq!(kinds[1], SeriesKind::HeikinAshi);
}

#[test]
fn line_and_area_use_lines_and_bars_use_no_body() {
    let mut c = chart();
    let candles_lines = scene(&mut c).lines.len();
    c.handle(Event::Command(SERIES_CYCLE)); // Heikin-Ashi
    c.handle(Event::Command(SERIES_CYCLE)); // Bars
    let bars = scene(&mut c);
    // A bar has a stem and two ticks: three rects, not two.
    let bar_rects = bars.rects.iter().filter(|r| r.color.a == 255 && r.y < 444.0).count();
    assert!(bar_rects >= 3 * 90, "{bar_rects}");
    c.handle(Event::Command(SERIES_CYCLE)); // Line
    let line = scene(&mut c);
    assert!(line.lines.len() > candles_lines + 90, "a segment per bar");
    c.handle(Event::Command(SERIES_CYCLE)); // Area
    let area = scene(&mut c);
    assert!(area.lines.len() >= line.lines.len());
    let fading = |s: &tickerlight_core::Scene| s.rects.iter().filter(|r| r.color.a > 0 && r.to.a == 0).count();
    assert_eq!(fading(&line), 0, "a line has no fill");
    assert!(fading(&area) > 90, "a fill under the line, fading to nothing");
}

#[test]
fn autoscale_follows_the_kind() {
    // A line only needs the closes: a spike in a high does not stretch its axis.
    use tickerlight_core::{Candle, Candles, Time};
    let mut candles = Candles::new();
    for i in 0..100 {
        let high = if i == 50 { 500.0 } else { 101.0 };
        candles
            .push(Candle {
                time: Time(i * 60_000),
                open: 100.0,
                high,
                low: 99.0,
                close: 100.5,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut c = Chart::new(candles);
    c.resize(872.0, 628.0);
    let axis_max = |c: &mut Chart| {
        scene(c)
            .texts
            .iter()
            .filter(|t| t.x > 800.0)
            .filter_map(|t| t.text.parse::<f64>().ok())
            .fold(0.0, f64::max)
    };
    assert!(axis_max(&mut c) > 400.0, "candles show the spike");
    for _ in 0..3 {
        c.handle(Event::Command(SERIES_CYCLE));
    }
    assert_eq!(kind(&c), SeriesKind::Line);
    assert!(axis_max(&mut c) < 102.0, "the line does not");
}
