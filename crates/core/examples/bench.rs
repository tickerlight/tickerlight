//! Run with: cargo run --release -p tickerlight-core --example bench

use std::hint::black_box;
use std::time::{Duration, Instant};

use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Scene, Time};

const CANDLES: usize = 1_000_000;
const RUNS: usize = 30;

fn main() {
    let candles = random_walk(CANDLES);
    let mut chart = Chart::new(candles);
    chart.resize(1664.0, 928.0); // plot 1600 x 900
    let mut scene = Scene::new();

    println!("{CANDLES} candles, median of {RUNS} runs\n");

    bench("frame, default zoom (~200 bars visible)", || {
        chart.build_scene(&mut scene);
        black_box(scene.rects.len())
    });

    for _ in 0..12 {
        chart.handle(Event::Wheel { x: 800.0, delta: -1.0 });
    }
    bench("frame, zoomed out (~1000 bars visible)", || {
        chart.build_scene(&mut scene);
        black_box(scene.rects.len())
    });

    zoom_out_fully(&mut chart);
    bench("frame, everything visible (1M bars)", || {
        chart.build_scene(&mut scene);
        black_box(scene.rects.len())
    });

    let mut replay = Chart::new(random_walk(CANDLES));
    replay.resize(1664.0, 928.0);
    replay.set_cursor(Some(Time(CANDLES as i64 / 2 * 60_000)));
    zoom_out_fully(&mut replay);
    bench("frame, everything visible, replay cursor at half", || {
        replay.build_scene(&mut scene);
        black_box(scene.rects.len())
    });

    let mut frames = Chart::new(random_walk(CANDLES));
    frames.resize(1664.0, 928.0);
    bench("switch 1M one minute bars to 15 minutes, and back", || {
        frames.set_timeframe(Some(15 * 60_000));
        frames.set_timeframe(None);
    });
    frames.set_timeframe(Some(15 * 60_000));
    frames.set_cursor(Some(Time(CANDLES as i64 / 2 * 60_000)));
    frames.handle(Event::Key(tickerlight_core::Key::Alt('P')));
    bench("replay step on 15 minutes over 1M bars", || {
        frames.handle(Event::Key(tickerlight_core::Key::SelectRight));
    });
}

fn zoom_out_fully(chart: &mut Chart) {
    for _ in 0..300 {
        chart.handle(Event::Wheel { x: 800.0, delta: -1.0 });
    }
}

fn bench<T>(name: &str, mut f: impl FnMut() -> T) {
    black_box(f()); // warm up
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            black_box(f());
            start.elapsed()
        })
        .collect();
    times.sort();
    println!("{name:<56} {:>10.1?}", times[RUNS / 2]);
}
