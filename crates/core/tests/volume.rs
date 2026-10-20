//! The volume histogram under the candles.

use tickerlight_core::plugin::names;
use tickerlight_core::{Candle, Candles, Chart, Event, Scene, Time};

fn chart(volume: impl Fn(i64) -> f64) -> Chart {
    let mut candles = Candles::new();
    for i in 0..200 {
        let p = 100.0 + (i % 7) as f64;
        candles
            .push(Candle {
                time: Time(i * 60_000),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p + 0.5,
                volume: volume(i),
            })
            .unwrap();
    }
    let mut chart = Chart::new(candles);
    chart.resize(872.0, 528.0); // plot 800 x 500
    chart
}

/// The translucent rects: only the volume bars are.
fn bars(chart: &mut Chart) -> Vec<tickerlight_core::Rect> {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
        .rects
        .into_iter()
        .filter(|r| r.color.a < 255 && r.color.a > 0 && r.h > 0.0 && r.y + r.h > 499.0)
        .collect()
}

#[test]
fn bars_stand_on_the_bottom_of_the_plot_and_the_tallest_takes_a_fifth() {
    let mut c = chart(|i| 1.0 + (i % 10) as f64);
    let bars = bars(&mut c);
    assert!(bars.len() >= 90, "one bar per visible candle: {}", bars.len());
    assert!(
        bars.iter().all(|b| (b.y + b.h - 500.0).abs() < 0.5),
        "all end at the bottom of the plot"
    );
    let tallest = bars.iter().map(|b| b.h).fold(0.0, f32::max);
    assert_eq!(tallest, 100.0, "20% of 500 px");
    let shortest = bars.iter().map(|b| b.h).fold(f32::MAX, f32::min);
    assert!((shortest - 10.0).abs() <= 1.0, "volume 1 of 10: {shortest}");
}

#[test]
fn no_volume_draws_nothing_and_the_layer_can_be_removed() {
    let mut c = chart(|_| 0.0);
    assert!(bars(&mut c).is_empty());
    let mut c = chart(|_| 5.0);
    assert!(!bars(&mut c).is_empty());
    assert!(c.remove_layer(names::VOLUME));
    assert!(bars(&mut c).is_empty());
}

#[test]
fn held_back_volume_is_not_drawn() {
    // A giant volume in the future must not rescale the histogram of what is shown.
    let mut c = chart(|i| {
        if i >= 195 {
            1e9
        } else if i == 150 {
            10.0
        } else {
            5.0
        }
    });
    c.handle(Event::Command(tickerlight_core::plugin::commands::REPLAY_TOGGLE));
    assert!(c.seen().len() < 195, "{}", c.seen().len());
    let bars = bars(&mut c);
    let tallest = bars.iter().map(|b| b.h).fold(0.0, f32::max);
    assert_eq!(tallest, 100.0, "scaled to the shown bars only");
    assert!(bars.iter().any(|b| b.h == 50.0));
}

#[test]
fn bars_wide_enough_for_a_footprint_can_be_left_to_it() {
    let mut c = chart(|_| 5.0);
    let volume = tickerlight_core::builtin::VolumeLayer {
        hidden_from: Some(40.0),
        ..Default::default()
    };
    c.add_layer(names::VOLUME, tickerlight_core::plugin::order::VOLUME, volume);
    assert!(!bars(&mut c).is_empty(), "the default bars are narrower");
    for _ in 0..30 {
        c.handle(Event::Wheel { x: 400.0, delta: 1.0 });
    }
    assert!(c.time_scale().bar_spacing >= 40.0);
    assert!(bars(&mut c).is_empty());
}
