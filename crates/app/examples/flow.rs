//! Renders the window with the order flow into a PNG, without opening it: the footprint and the volume profile
//! of the last day on screen, its trades taken from the cache or else the archive, in bars of 15 minutes or of
//! the timeframe given (`15s`: bars made of the trades).
//!
//! TICKERLIGHT_DATA_DIR=... cargo run -p tickerlight-app --example flow -- OUT.png [SYMBOL [spot|usd-futures|okx-spot|okx-swap
//! [TIMEFRAME]]]

use std::error::Error;
use std::time::{Duration, Instant};

use tickerlight_app::program::{self, Source, Window};
use tickerlight_app::store::{Series, Venue};
use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::Interval;
use tickerlight_core::{Event, Scene, Surface};

const SIZE: (u32, u32) = (1400, 800);

fn click(window: &mut Window, text: &str) {
    let (x, y) = window
        .workspace()
        .ui_state()
        .rect_of(text)
        .unwrap_or_else(|| panic!("no control {text}"))
        .center();
    window.handle(Event::PointerMoved { x, y });
    window.handle(Event::PointerDown);
    window.handle(Event::PointerUp);
    window.build_scene(&mut Scene::new());
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("an output file")?;
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let venue = match args.next() {
        Some(name) => *Venue::ALL.iter().find(|v| v.name() == name).ok_or("no such market")?,
        None => Venue::ALL[0],
    };
    let timeframe = args.next().unwrap_or_else(|| "15m".into());
    let series = Series {
        venue,
        symbol: venue.symbol_of(&symbol).ok_or("no symbol")?,
        interval: Interval::M1,
    };
    let data = std::env::var_os("TICKERLIGHT_DATA_DIR").ok_or("set TICKERLIGHT_DATA_DIR")?;
    // The candles come from the cache; the trades may be downloaded.
    let cached = Source {
        root: tickerlight_app::store::scratch_root(data.as_ref(), "flow")?,
        months: 1,
        offline: true,
    };
    let candles = program::load(&cached, &series, program::now(), |_| {})?;
    let source = Source {
        offline: false,
        ..cached
    };
    let mut window = Window::new(source, series, Some(candles));
    window.resize(SIZE.0 as f32, SIZE.1 as f32);
    window.build_scene(&mut Scene::new());

    click(&mut window, &timeframe);
    click(&mut window, "Footprint");
    click(&mut window, "Profile");
    // Zoom in at the right edge of the chart until the footprint has room.
    let area = window.workspace().chart_area();
    let x = area.x + area.w - 120.0;
    window.handle(Event::PointerMoved {
        x,
        y: area.y + area.h / 2.0,
    });
    while window.workspace().chart().time_scale().bar_spacing < 80.0 {
        window.handle(Event::Wheel { x, delta: 1.0 });
    }
    window.handle(Event::PointerLeft);
    window.build_scene(&mut Scene::new());

    // Live data keep the window ticking: done when no piece of the tape is loading any more.
    let started = Instant::now();
    let mut ticks = 0;
    while ticks < 20 || window.workspace().ctx.flow_status.contains("Loading") {
        if started.elapsed() > Duration::from_secs(300) {
            let status = &window.workspace().ctx.flow_status;
            return Err(format!("the trades did not load in 5 minutes: {status}").into());
        }
        window.handle(Event::Tick { dt: 0.05 });
        std::thread::sleep(Duration::from_millis(50));
        ticks += 1;
    }
    eprintln!("{}", window.workspace().ctx.flow_status);

    let (w, h) = SIZE;
    let mut scene = Scene::new();
    window.build_scene(&mut scene);
    let rgba = render_to_rgba(&Gpu::new_headless()?, &scene, w, h);
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(out)?), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
