//! Renders the chart offscreen after a scripted sequence of input events and saves PNGs.
//! No window is opened.
//!
//! cargo run --release -p tickerlight-backend-wgpu --example snapshot -- out_dir

use std::path::Path;

use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Color, Event, Key, Scene, Sma};

const SIZE: (u32, u32) = (1200, 700);

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let gpu = Gpu::new_headless().expect("GPU");
    let mut chart = Chart::new(random_walk(5_000));
    chart.resize(SIZE.0 as f32, SIZE.1 as f32);
    let mut scene = Scene::new();

    snapshot(&gpu, &mut chart, &mut scene, &dir, "1_start.png");

    // An indicator is a plugin like any other.
    let mut overlay = Chart::new(random_walk(5_000)).with(Sma::new(20, Color::rgb(255, 183, 77)));
    overlay.resize(SIZE.0 as f32, SIZE.1 as f32);
    for _ in 0..3 {
        overlay.handle(Event::Wheel { x: 900.0, delta: 1.0 });
    }
    snapshot(&gpu, &mut overlay, &mut scene, &dir, "1c_sma_plugin.png");

    // An indicator with a pane of its own; the crosshair runs through both panes.
    let mut panes = Chart::new(random_walk(5_000))
        .with(Sma::new(20, Color::rgb(255, 183, 77)))
        .with(tickerlight_core::builtin::Rsi::new(14));
    panes.resize(SIZE.0 as f32, SIZE.1 as f32);
    panes.handle(Event::PointerMoved { x: 640.0, y: 600.0 });
    snapshot(&gpu, &mut panes, &mut scene, &dir, "6_panes.png");

    // The price axis in log mode and in percent mode, zoomed out on a long history.
    let mut scales = Chart::new(random_walk(5_000));
    scales.resize(SIZE.0 as f32, SIZE.1 as f32);
    for _ in 0..100 {
        scales.handle(Event::Wheel { x: 600.0, delta: -1.0 });
    }
    scales.handle(Event::Key(Key::Char('o')));
    snapshot(&gpu, &mut scales, &mut scene, &dir, "8a_log.png");
    scales.handle(Event::Key(Key::Char('o')));
    snapshot(&gpu, &mut scales, &mut scene, &dir, "8b_percent.png");

    // Chart types: Heikin-Ashi, bars, area.
    let mut kinds = Chart::new(random_walk(5_000));
    kinds.resize(SIZE.0 as f32, SIZE.1 as f32);
    for name in ["9a_heikin_ashi.png", "9b_bars.png", "9c_line.png", "9d_area.png"] {
        kinds.handle(Event::Key(Key::Char('k')));
        snapshot(&gpu, &mut kinds, &mut scene, &dir, name);
    }

    // Drawings: a horizontal line, a trend line and a zone, one of them selected, and the ruler.
    panes.handle(Event::PointerLeft);
    let gesture = |chart: &mut Chart, key: char, from: (f32, f32), to: (f32, f32)| {
        let mut frame = Scene::new();
        chart.build_scene(&mut frame);
        chart.handle(Event::Key(Key::Char(key)));
        chart.handle(Event::PointerMoved { x: from.0, y: from.1 });
        chart.handle(Event::PointerDown);
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            chart.handle(Event::PointerMoved {
                x: from.0 + (to.0 - from.0) * t,
                y: from.1 + (to.1 - from.1) * t,
            });
        }
        chart.handle(Event::PointerUp);
    };
    gesture(&mut panes, 'h', (300.0, 150.0), (300.0, 150.0));
    gesture(&mut panes, 'd', (200.0, 420.0), (700.0, 250.0));
    gesture(&mut panes, 'z', (800.0, 300.0), (1000.0, 400.0));
    gesture(&mut panes, 'm', (300.0, 200.0), (500.0, 330.0));
    panes.handle(Event::PointerLeft);
    snapshot(&gpu, &mut panes, &mut scene, &dir, "7_drawings.png");

    // Replay: start at the bar under the pointer, buy, play a few bars, sell.
    let mut replay = Chart::new(random_walk(5_000)).with(tickerlight_core::builtin::Equity::new());
    replay.resize(SIZE.0 as f32, SIZE.1 as f32);
    replay.handle(Event::PointerMoved { x: 700.0, y: 300.0 });
    replay.handle(Event::Key(Key::Char('r')));
    replay.handle(Event::PointerLeft);
    replay.handle(Event::Key(Key::Char('b')));
    // Hover the Buy button.
    let rects = tickerlight_core::builtin::layout(
        replay.resources(),
        &replay.style,
        1.0,
        replay.plot_size().0,
        replay.plot_size().1,
    );
    let buy = rects
        .iter()
        .find(|(b, _, _)| b.command == tickerlight_core::plugin::commands::BUY)
        .unwrap()
        .2;
    replay.handle(Event::PointerMoved {
        x: buy.x + buy.w / 2.0,
        y: buy.y + buy.h / 2.0,
    });
    snapshot(&gpu, &mut replay, &mut scene, &dir, "5a_replay_bought.png");
    replay.handle(Event::PointerLeft);
    replay.handle(Event::Key(Key::Space));
    for _ in 0..40 {
        replay.handle(Event::Tick { dt: 0.25 });
    }
    replay.handle(Event::Key(Key::Char('s')));
    snapshot(&gpu, &mut replay, &mut scene, &dir, "5b_replay_sold.png");

    // A higher timeframe in a replay: 15 minute candles that form one minute at a time.
    let mut frames = Chart::new(random_walk(5_000));
    frames.resize(SIZE.0 as f32, SIZE.1 as f32);
    frames.handle(Event::PointerMoved { x: 700.0, y: 300.0 });
    frames.handle(Event::Key(Key::Char('r')));
    frames.handle(Event::PointerLeft);
    for _ in 0..4 {
        frames.handle(Event::Key(Key::Char('f')));
    }
    for _ in 0..7 {
        frames.handle(Event::Key(Key::Right));
    }
    snapshot(&gpu, &mut frames, &mut scene, &dir, "10_timeframe_replay.png");

    // Pending orders: a limit below the market and a stop above it, placed at the pointer.
    replay.handle(Event::Key(Key::Char('c')));
    for (y, key) in [(430.0, 'l'), (120.0, 't')] {
        let mut frame = Scene::new();
        replay.build_scene(&mut frame); // handlers act on the price scale of the last frame
        replay.handle(Event::PointerMoved { x: 500.0, y });
        replay.handle(Event::Key(Key::Char(key)));
    }
    replay.handle(Event::PointerMoved { x: 300.0, y: 250.0 });
    snapshot(&gpu, &mut replay, &mut scene, &dir, "5c_orders.png");

    // Hover the pointer over the plot: the crosshair with its price and time labels.
    chart.handle(Event::PointerMoved { x: 640.0, y: 260.0 });
    snapshot(&gpu, &mut chart, &mut scene, &dir, "1b_crosshair.png");
    chart.handle(Event::PointerLeft);

    // Zoom in with the wheel at x = 600, then drag the chart to the right (towards older bars).
    for _ in 0..4 {
        chart.handle(Event::Wheel { x: 600.0, delta: 1.0 });
    }
    chart.handle(Event::PointerMoved { x: 300.0, y: 300.0 });
    chart.handle(Event::PointerDown);
    for i in 1..=20 {
        chart.handle(Event::PointerMoved {
            x: 300.0 + i as f32 * 30.0,
            y: 300.0,
        });
    }
    chart.handle(Event::PointerUp);
    snapshot(&gpu, &mut chart, &mut scene, &dir, "2_zoomed_and_panned.png");

    // Zoom all the way out: 5 000 bars in ~1100 px.
    for _ in 0..100 {
        chart.handle(Event::Wheel { x: 600.0, delta: -1.0 });
    }
    snapshot(&gpu, &mut chart, &mut scene, &dir, "3_zoomed_out.png");

    // The same chart on a 2x screen: twice the pixels, same picture.
    let mut hidpi = Chart::new(random_walk(5_000));
    hidpi.set_scale(2.0);
    hidpi.resize(2.0 * SIZE.0 as f32, 2.0 * SIZE.1 as f32);
    hidpi.handle(Event::PointerMoved { x: 1280.0, y: 520.0 });
    snapshot_sized(
        &gpu,
        &mut hidpi,
        &mut scene,
        &dir,
        "4_hidpi_2x.png",
        (2 * SIZE.0, 2 * SIZE.1),
    );
}

fn snapshot(gpu: &Gpu, chart: &mut Chart, scene: &mut Scene, dir: &str, name: &str) {
    snapshot_sized(gpu, chart, scene, dir, name, SIZE);
}

fn snapshot_sized(gpu: &Gpu, chart: &mut Chart, scene: &mut Scene, dir: &str, name: &str, size: (u32, u32)) {
    chart.build_scene(scene);
    let rgba = render_to_rgba(gpu, scene, size.0, size.1);
    let path = Path::new(dir).join(name);
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut encoder = png::Encoder::new(file, size.0, size.1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
    println!("{} rects -> {}", scene.rects.len(), path.display());
}
