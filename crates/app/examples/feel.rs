//! Measures how the program answers a user, without opening a window: the time from a request to the chart of a
//! symbol, and the time the program takes for each pointer move, drag and wheel turn (the event and the frame it
//! draws, not the GPU), on several timeframes, with an indicator, with the order flow and in a replay.
//!
//! TICKERLIGHT_DATA_DIR=... cargo run --release -p tickerlight-app --example feel [-- SYMBOL OTHER]
//!
//! It reads the cached market data of the data folder and writes nothing there.

use std::error::Error;
use std::time::{Duration, Instant};

use tickerlight_app::program::{Source, Window};
use tickerlight_app::store::{Series, Venue, scratch_root};
use tickerlight_binance::{Interval, Market};
use tickerlight_core::{Event, Scene, Surface};

struct Feel {
    window: Window,
    scene: Scene,
}

impl Feel {
    /// The time the program takes for `event` and the frame after it.
    fn event(&mut self, event: Event) -> Duration {
        let started = Instant::now();
        self.window.handle(event);
        self.scene = Scene::new();
        self.window.build_scene(&mut self.scene);
        started.elapsed()
    }

    fn click(&mut self, text: &str) {
        let (x, y) = (self.window.workspace().ui_state().rect_of(text))
            .unwrap_or_else(|| panic!("no control {text}"))
            .center();
        self.event(Event::PointerMoved { x, y });
        self.event(Event::PointerDown);
        self.event(Event::PointerUp);
    }

    /// Ticks as the window does until nothing loads; the time it took.
    fn settle(&mut self) -> Duration {
        let started = Instant::now();
        while self.window.needs_tick() && started.elapsed() < Duration::from_secs(120) {
            self.event(Event::Tick { dt: 0.016 });
            std::thread::sleep(Duration::from_millis(16));
        }
        started.elapsed()
    }

    fn point(&self, fx: f32, fy: f32) -> (f32, f32) {
        let a = self.window.workspace().chart_area();
        (a.x + a.w * fx, a.y + a.h * fy)
    }

    /// The pointer moved across the chart, a drag that pans it, and wheel turns, each timed.
    fn gestures(&mut self, label: &str) {
        let mut hover = Vec::new();
        for i in 0..120 {
            let (x, y) = self.point(0.1 + 0.8 * i as f32 / 120.0, 0.3 + 0.002 * i as f32);
            hover.push(self.event(Event::PointerMoved { x, y }));
        }
        let mut drag = Vec::new();
        let (x, y) = self.point(0.6, 0.5);
        self.event(Event::PointerMoved { x, y });
        self.event(Event::PointerDown);
        for i in 0..60 {
            let (x, y) = self.point(0.6 - 0.4 * i as f32 / 60.0, 0.5);
            drag.push(self.event(Event::PointerMoved { x, y }));
        }
        self.event(Event::PointerUp);
        let mut wheel = Vec::new();
        let (x, _) = self.point(0.5, 0.5);
        for i in 0..16 {
            let delta = if i < 8 { -1.0 } else { 1.0 };
            wheel.push(self.event(Event::Wheel { x, delta }));
        }
        let rects = self.scene.rects.len();
        println!(
            "{label:<28} hover {}  drag {}  wheel {}  ({rects} rects)",
            summary(&mut hover),
            summary(&mut drag),
            summary(&mut wheel)
        );
    }
}

/// The median and the slowest, in milliseconds.
fn summary(times: &mut [Duration]) -> String {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!(
        "{:6.2} / {:6.2} ms",
        ms(times[times.len() / 2]),
        ms(times[times.len() - 1])
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let other = args.next().unwrap_or_else(|| "ETHUSDT".into());
    let data = std::env::var_os("TICKERLIGHT_DATA_DIR").ok_or("set TICKERLIGHT_DATA_DIR")?;
    let source = Source {
        root: scratch_root(data.as_ref(), "feel")?,
        months: 12,
        offline: true,
    };
    let series = Series {
        venue: Venue::Binance(Market::Spot),
        symbol: symbol.clone(),
        interval: Interval::M1,
    };
    let mut feel = Feel {
        window: Window::new(source, series, None),
        scene: Scene::new(),
    };
    feel.window.resize(1600.0, 900.0);
    let loaded = feel.settle();
    let bars = feel.window.workspace().chart().base().len();
    println!(
        "{symbol} shown after {:.2} s ({bars} bars of a minute)",
        loaded.as_secs_f64()
    );

    for timeframe in ["1m", "15m", "1h", "1d"] {
        feel.click(timeframe);
        feel.gestures(timeframe);
    }
    feel.click("15m");
    feel.click("Indicators");
    feel.click("Search indicators");
    for c in "ichimoku".chars() {
        feel.event(Event::Key(tickerlight_core::Key::Char(c)));
    }
    feel.click("Ichimoku cloud");
    feel.click("Indicators");
    feel.gestures("15m, Ichimoku");
    feel.click("Footprint");
    feel.click("Profile");
    feel.settle();
    feel.gestures("15m, Ichimoku, order flow");
    feel.click("Footprint");
    feel.click("Profile");
    let (x, y) = feel.point(0.7, 0.5);
    feel.event(Event::PointerMoved { x, y });
    feel.event(Event::Key(tickerlight_core::Key::Alt('P')));
    feel.gestures("15m, Ichimoku, replay");

    feel.window.workspace_mut().ctx.symbol = other.clone();
    feel.click("Symbol");
    let started = Instant::now();
    feel.event(Event::Key(tickerlight_core::Key::Enter));
    feel.settle();
    println!("{other} shown after {:.2} s", started.elapsed().as_secs_f64());
    Ok(())
}
