//! Walks through the program the way a user does, with the mouse and the keyboard, and renders a PNG after each
//! step into a folder, without opening a window: to look at every screen a user sees, at several window sizes.
//!
//! TICKERLIGHT_DATA_DIR=... cargo run -p tickerlight-app --example tour -- DIR [--online]
//!
//! The candles and trades come from the cache in the data folder; offline it uses only what is there, `--online`
//! downloads what is missing (an empty cache shows the first start). What the tour saves (drawings, sessions,
//! settings) goes to a scratch folder that is removed at the end: every tour starts as a first start does and
//! leaves the user's own state alone.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tickerlight_app::program::{Source, Window};
use tickerlight_app::store::{Series, Venue};
use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::{Interval, Market};
use tickerlight_core::builtin::{Live, StrategyRun, closed_trades};
use tickerlight_core::calendar::Zone;
use tickerlight_core::plugin::commands;
use tickerlight_core::{Candle, Event, Key, Scene, Surface, Time, native_step};
use tickerlight_ui::panels::short_time;

struct Tour {
    window: Window,
    gpu: Gpu,
    dir: PathBuf,
    size: (u32, u32),
    scale: f32,
    shots: usize,
}

impl Tour {
    fn resize(&mut self, w: u32, h: u32, scale: f32) {
        (self.size, self.scale) = ((w, h), scale);
        self.window.set_scale(scale);
        self.window.resize(w as f32, h as f32);
        self.window.build_scene(&mut Scene::new());
    }

    fn at(&self, text: &str) -> (f32, f32) {
        self.window
            .workspace()
            .ui_state()
            .rect_of(text)
            .unwrap_or_else(|| panic!("no control {text}"))
            .center()
    }

    /// Scrolls the page under `page` to its top, then down until the control `text` is well inside the window.
    fn reveal(&mut self, page: (f32, f32), text: &str) {
        self.wheel(page, 40.0);
        let bottom = self.size.1 as f32 * self.scale - 40.0;
        let mut steps = 0;
        while steps < 40
            && !(self.window.workspace().ui_state().rect_of(text)).is_some_and(|area| area.y + area.h < bottom)
        {
            self.wheel(page, -1.0);
            steps += 1;
        }
    }

    /// The control whose text starts with `prefix`, for labels with a number in them.
    fn click_starting(&mut self, prefix: &str) {
        let at = self
            .window
            .workspace()
            .ui_state()
            .rects()
            .iter()
            .find(|(t, _)| t.starts_with(prefix))
            .unwrap_or_else(|| panic!("no control {prefix}..."))
            .1
            .center();
        self.click_at(at);
    }

    /// The control laid out right after `heading`: the field it names.
    fn click_beside(&mut self, heading: &str) {
        let rects = self.window.workspace().ui_state().rects();
        let i = rects
            .iter()
            .position(|(t, _)| t == heading)
            .unwrap_or_else(|| panic!("no {heading}"));
        let at = rects[i + 1].1.center();
        self.click_at(at);
    }

    fn has(&self, text: &str) -> bool {
        self.window.workspace().ui_state().rect_of(text).is_some()
    }

    fn event(&mut self, event: Event) {
        self.window.handle(event);
        self.window.build_scene(&mut Scene::new());
    }

    fn click_at(&mut self, (x, y): (f32, f32)) {
        self.event(Event::PointerMoved { x, y });
        self.event(Event::PointerDown);
        self.event(Event::PointerUp);
    }

    fn click(&mut self, text: &str) {
        let at = self.at(text);
        self.click_at(at);
    }

    /// Adds an indicator as the keys do: "/" opens the list to add from, its name narrows it, Enter adds the first.
    fn add_indicator(&mut self, title: &str) {
        self.key(Key::Char('/'));
        self.type_text(title);
        self.key(Key::Enter);
    }

    fn drag(&mut self, from: (f32, f32), to: (f32, f32)) {
        self.event(Event::PointerMoved { x: from.0, y: from.1 });
        self.event(Event::PointerDown);
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.event(Event::PointerMoved { x, y });
        }
        self.event(Event::PointerUp);
    }

    fn wheel(&mut self, (x, y): (f32, f32), delta: f32) {
        self.event(Event::PointerMoved { x, y });
        self.event(Event::Wheel { x, delta });
    }

    fn key(&mut self, key: Key) {
        self.event(Event::Key(key));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(Key::Char(c));
        }
    }

    /// Runs the clock for `seconds`, as the window does while something animates or loads.
    fn wait(&mut self, seconds: f32) {
        let started = Instant::now();
        while started.elapsed().as_secs_f32() < seconds {
            self.event(Event::Tick { dt: 0.05 });
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Ticks until nothing loads, at most `seconds`.
    fn settle(&mut self, seconds: u64) {
        let started = Instant::now();
        while self.window.needs_tick() && started.elapsed() < Duration::from_secs(seconds) {
            self.event(Event::Tick { dt: 0.05 });
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A point of the chart, in fractions of its area.
    fn chart_point(&self, fx: f32, fy: f32) -> (f32, f32) {
        let a = self.window.workspace().chart_area();
        (a.x + a.w * fx, a.y + a.h * fy)
    }

    fn shot(&mut self, name: &str) -> Result<(), Box<dyn Error>> {
        self.shots += 1;
        // What lights up under the pointer takes a few frames, as in the window.
        for _ in 0..4 {
            if !self.window.animating() {
                break;
            }
            self.event(Event::Tick { dt: 0.05 });
        }
        let (w, h) = self.size;
        let mut scene = Scene::new();
        self.window.build_scene(&mut scene);
        let rgba = render_to_rgba(&self.gpu, &scene, w, h);
        let path = self.dir.join(format!("{:02}-{name}.png", self.shots));
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;
        eprintln!("{} {}", path.display(), self.window.workspace().ctx.status);
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).ok_or("an output folder")?);
    std::fs::create_dir_all(&dir)?;
    let data: PathBuf = std::env::var_os("TICKERLIGHT_DATA_DIR")
        .ok_or("set TICKERLIGHT_DATA_DIR")?
        .into();
    let root = tickerlight_app::store::scratch_root(&data, "tour")?;
    // A strategy built to WebAssembly, in the folder the program reads them from.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let built = std::process::Command::new(cargo)
        .args(["build", "-q", "--release", "--target", "wasm32-unknown-unknown"])
        .args(["-p", "tickerlight-strategy", "--example", "breakout"])
        .status()?;
    if !built.success() {
        return Err("the example strategy did not build".into());
    }
    let wasm = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wasm32-unknown-unknown/release/examples/breakout.wasm"
    );
    std::fs::create_dir_all(root.join("strategies"))?;
    std::fs::copy(wasm, root.join("strategies/breakout.wasm"))?;
    let source = Source {
        root: root.clone(),
        months: 3,
        offline: !std::env::args().any(|a| a == "--online"),
    };
    let series = Series {
        venue: Venue::Binance(Market::Spot),
        symbol: "BTCUSDT".into(),
        interval: Interval::M1,
    };
    // The way the program starts: an empty window that loads the symbol in the background.
    let window = Window::new(source, series, None);
    let mut tour = Tour {
        window,
        gpu: Gpu::new_headless()?,
        dir,
        size: (1200, 700),
        scale: 1.0,
        shots: 0,
    };
    tour.resize(1200, 700, 1.0);
    tour.shot("loading")?;
    tour.wait(3.0);
    tour.shot("loading-later")?;
    tour.settle(120);
    tour.shot("loaded")?;

    // What a button does and its key, shown by it.
    let auto = tour.at(tickerlight_ui::panels::SCALE_AUTO);
    tour.event(Event::PointerMoved { x: auto.0, y: auto.1 });
    tour.shot("tip")?;
    // A long one, at the right edge of the window.
    let start = tour.at(tickerlight_ui::panels::REPLAY);
    tour.event(Event::PointerMoved { x: start.0, y: start.1 });
    tour.shot("tip-long")?;

    // Looking around: the crosshair, zooming out, panning back, the price axis.
    let middle = tour.chart_point(0.5, 0.5);
    tour.event(Event::PointerMoved {
        x: middle.0,
        y: middle.1,
    });
    tour.shot("crosshair")?;
    for _ in 0..6 {
        tour.wheel(tour.chart_point(0.8, 0.5), -1.0);
    }
    tour.shot("zoomed-out")?;
    tour.drag(tour.chart_point(0.3, 0.5), tour.chart_point(0.8, 0.5));
    tour.shot("panned")?;
    let a = tour.window.workspace().chart_area();
    tour.drag((a.x + a.w - 20.0, a.y + a.h * 0.3), (a.x + a.w - 20.0, a.y + a.h * 0.6));
    tour.shot("price-axis-dragged")?;
    tour.click("auto");

    // A replay started where clicked on the chart, then played.
    tour.click("15m");
    tour.click(tickerlight_ui::panels::REPLAY);
    tour.event(Event::PointerMoved {
        x: tour.chart_point(0.4, 0.5).0,
        y: tour.chart_point(0.4, 0.5).1,
    });
    tour.shot("replay-choosing")?;
    tour.click_at(tour.chart_point(0.4, 0.5));
    tour.shot("replay-chosen")?;
    tour.key(Key::Space);
    tour.wait(2.0);
    tour.key(Key::Space);
    tour.shot("replay-played")?;
    // Playing pauses at a time of day typed; "Play to it" goes there at once.
    tour.click("Pause at time");
    tour.type_text("14:00");
    tour.key(Key::Enter);
    tour.click("Play to it");
    tour.shot("replay-to-clock")?;
    // The player's timeline: the pointer on it shows the moment a click would start at.
    let track = tour
        .window
        .workspace()
        .ui_state()
        .rect_of(tickerlight_ui::panels::TIMELINE)
        .unwrap();
    tour.event(Event::PointerMoved {
        x: track.x + track.w * 0.3,
        y: track.center().1,
    });
    tour.shot("replay-timeline")?;
    // The times on New York's clock: the axis, the crosshair, the player and the replay panel. Then UTC again.
    tour.click("UTC");
    tour.click("New York");
    tour.event(Event::PointerMoved {
        x: tour.chart_point(0.3, 0.5).0,
        y: tour.chart_point(0.3, 0.5).1,
    });
    tour.shot("new-york")?;
    tour.click("New York");
    tour.click("UTC");

    // Trading: a market buy that risks 1% of the balance at its stop, with a target, then steps.
    tour.click("Buy");
    tour.click("Risk %");
    tour.click_beside("Take profit");
    let price = *tour.window.workspace().chart().base().close().last().unwrap();
    tour.type_text(&format!("{:.2}", price * 1.01));
    tour.click_beside("Stop loss");
    tour.type_text(&format!("{:.2}", price * 0.995));
    tour.click_beside("Break-even at");
    tour.type_text(&format!("{:.2}", price * 1.004));
    tour.key(Key::Enter);
    tour.shot("order-ticket")?;
    tour.click_starting("Buy 0");
    // Its exits wait as orders, each in a row of its own: a price to type, a Cancel; the stop's break-even is a
    // dotted line above.
    tour.shot("position-exits")?;
    for _ in 0..8 {
        tour.key(Key::SelectRight);
    }
    tour.shot("position")?;

    // Drawings made with the mouse: a trend line, a rectangle, a Fibonacci retracement, a channel, a long position.
    tour.click("Trend line");
    tour.click_at(tour.chart_point(0.2, 0.7));
    tour.click_at(tour.chart_point(0.5, 0.3));
    tour.click("Shapes");
    tour.shot("tool-group")?;
    tour.click("Rectangle");
    tour.drag(tour.chart_point(0.55, 0.4), tour.chart_point(0.65, 0.6));
    tour.shot("drawings")?;
    tour.click("Red");
    tour.click("Width 3");
    tour.shot("drawing-styled")?;
    tour.click("Shapes");
    tour.click("Fib retracement");
    tour.drag(tour.chart_point(0.1, 0.8), tour.chart_point(0.3, 0.2));
    tour.click("Shapes");
    tour.click("Parallel channel");
    tour.click_at(tour.chart_point(0.35, 0.75));
    tour.click_at(tour.chart_point(0.6, 0.55));
    tour.click_at(tour.chart_point(0.6, 0.7));
    tour.click("Long position");
    tour.click_at(tour.chart_point(0.75, 0.5));
    tour.click("Measures");
    tour.click("Price range");
    tour.drag(tour.chart_point(0.4, 0.2), tour.chart_point(0.48, 0.1));
    tour.shot("drawing-tools")?;
    // A note on the chart, typed where it was placed.
    tour.click("Text");
    tour.click_at(tour.chart_point(0.08, 0.15));
    tour.type_text("Breakout? Ждём ретест");
    tour.shot("note-typing")?;
    tour.key(Key::Enter);
    // Picked again and made bigger.
    tour.click_at(tour.chart_point(0.1, 0.15));
    tour.click("24");
    tour.shot("note-size")?;
    // A long planned on bars the replay has shown says how it went: target, stop or still open.
    tour.click("Long position");
    tour.click_at(tour.chart_point(0.36, 0.66));
    tour.shot("position-outcome")?;

    // Typing a note with capitals.
    let fills = tour
        .window
        .workspace()
        .chart()
        .resources()
        .get::<tickerlight_core::builtin::Account>()
        .unwrap()
        .fills()
        .to_vec();
    // Journal columns of one's own, typed on the sessions page; a trade takes a choice in each.
    tour.click("Sessions");
    // The field is at the foot of the column: scrolled up to it first.
    let (w, _) = tour.size;
    for _ in 0..4 {
        tour.wheel((w as f32 - 140.0, 200.0), -1.0);
    }
    tour.click("Grade: A, B, C; Mistake: early, late");
    tour.type_text("Grade: A, B, C; Mistake: early, late, size");
    tour.key(Key::Enter);
    tour.click("Trade");
    // The column is taller than the window: scrolled with the wheel to the trades.
    for _ in 0..12 {
        tour.wheel((w as f32 - 140.0, 200.0), -1.0);
    }
    tour.shot("column-scrolled")?;
    if let Some(fill) = fills.last() {
        tour.click(&short_time(fill.time, 60_000, Zone::Utc));
        tour.click("Note");
        tour.type_text("Retest entry → 回测 リテスト 리테스트 ✓");
        tour.key(Key::Enter);
        tour.click("B");
        tour.click("late");
    }
    tour.shot("note")?;
    // The trade replayed where it happened, a few bars on; then back to the session.
    if !fills.is_empty() {
        // Further down the column: the button and what replaces the notes.
        for _ in 0..4 {
            tour.wheel((w as f32 - 140.0, 200.0), -1.0);
        }
        tour.click("Replay this trade");
        for _ in 0..22 {
            tour.click("Step");
        }
        tour.shot("trade-review")?;
        tour.click("Back to the session");
    }
    // Letters typed over the chart start a symbol search with them, as on TradingView; Escape gives it up.
    tour.event(Event::PointerMoved {
        x: tour.chart_point(0.5, 0.5).0,
        y: tour.chart_point(0.5, 0.5).1,
    });
    for c in ['s', 'o', 'l'] {
        tour.key(Key::Char(c));
    }
    tour.shot("symbol-typed-over-chart")?;
    tour.key(Key::Escape);
    tour.click("Trade");
    // Digits open the interval box.
    tour.key(Key::Char('4'));
    tour.shot("interval-box")?;
    tour.key(Key::Escape);
    // A right press on the chart: the menu of the view, the drawings and the indicators.
    let (x, y) = tour.chart_point(0.4, 0.3);
    tour.event(Event::PointerMoved { x, y });
    tour.event(Event::SecondaryDown);
    tour.shot("chart-menu")?;
    tour.key(Key::Escape);

    // Smaller and bigger windows, and a screen with twice the pixels.
    for (w, h, scale) in [(1000, 650, 1.0), (800, 600, 1.0), (1920, 1080, 1.0), (2400, 1400, 2.0)] {
        tour.resize(w, h, scale);
        tour.shot(&format!("size-{w}x{h}@{scale}"))?;
    }
    tour.resize(1200, 700, 1.0);

    // The position closed: its picture is kept beside the session, and other exits are tried on it.
    // A point on the page of the right dock, to scroll it.
    let page = (tour.size.0 as f32 - 146.0, tour.size.1 as f32 / 2.0);
    tour.reveal(page, "Close");
    tour.click("Close");
    tour.reveal(page, "Try other stops and targets");
    tour.click("Try other stops and targets");
    tour.shot("other-exits")?;

    tour.click("Sessions");
    tour.shot("sessions")?;
    tour.click("Sessions");

    // The watchlist, with prices saved from Binance if there are any, and a search typed into the symbol field.
    if let Some(path) = std::env::var_os("CHART_TICKERS") {
        let tickers = tickerlight_binance::parse::tickers(&std::fs::read_to_string(path)?)?;
        let watch = &mut tour.window.workspace_mut().ctx.watch;
        watch.set_tickers(Venue::Binance(Market::Spot), tickerlight_binance::busiest(tickers));
    }
    tour.click("Watchlist");
    tour.shot("watchlist")?;
    // Sorted by the change, the biggest gain first, with the pointer on a row: its mark to take it off.
    tour.click("24h");
    let row = tour.at("ETHUSDT");
    tour.event(Event::PointerMoved { x: row.0, y: row.1 });
    tour.shot("watchlist-sorted")?;
    tour.click("24h");
    tour.click("24h");
    tour.click("Symbol");
    tour.key(Key::SelectAll);
    tour.type_text("eth");
    tour.shot("symbol-search")?;
    tour.key(Key::Escape);
    // A futures search with no futures prices (the API is closed to some places): the symbols with data here.
    let ctx = &mut tour.window.workspace_mut().ctx;
    ctx.venue = 1;
    ctx.watch.search = "usdt".into();
    tour.shot("watchlist-kept")?;
    let ctx = &mut tour.window.workspace_mut().ctx;
    ctx.venue = 0;
    tour.window.workspace_mut().ctx.watch.search.clear();
    tour.click("Watchlist");

    // Indicators: over the candles and in panes of their own, then taken off again. "/" opens the page.
    for name in [
        "Moving average",
        "Exponential moving average",
        "Bollinger bands",
        "Relative strength index",
        "MACD",
    ] {
        tour.add_indicator(name);
    }
    tour.click_beside("Length");
    tour.key(Key::SelectAll);
    tour.type_text("50");
    tour.key(Key::Enter);
    tour.click("EMA 20 dashed");
    // An average of the RSI, drawn with it.
    tour.click_beside("Average");
    tour.key(Key::SelectAll);
    tour.type_text("14");
    tour.key(Key::Enter);
    tour.shot("indicators")?;
    tour.event(Event::PointerMoved {
        x: tour.chart_point(0.4, 0.5).0,
        y: tour.chart_point(0.4, 0.5).1,
    });
    tour.shot("indicators-pointed")?;
    // On the label of the RSI: its buttons to hide it, set it up or remove it; hidden, its name stays.
    let chart = tour.window.workspace().chart();
    let rsi = chart.pane_names().iter().position(|&n| n == "rsi-14").unwrap();
    let top = chart.pane_layout()[rsi].top;
    let a = tour.window.workspace().chart_area();
    tour.event(Event::PointerMoved {
        x: a.x + 20.0,
        y: a.y + top + 14.0,
    });
    tour.shot("indicator-label")?;
    for label in ["SMA 50", "EMA 20", "BB 20 2", "RSI 14", "MACD 12 26 9"] {
        tour.click(&format!("Remove {label}"));
    }
    // The rest of the list.
    for name in [
        "Money flow index",
        "Commodity channel index",
        "On balance volume",
        "Donchian channels",
        "Weighted moving average",
    ] {
        tour.add_indicator(name);
    }
    tour.shot("indicators-more")?;
    for label in ["MFI 14", "CCI 20", "OBV", "DC 20", "WMA 20"] {
        tour.click(&format!("Remove {label}"));
    }
    for name in [
        "Volume with its average",
        "Pivot points (daily)",
        "SuperTrend",
        "Directional movement (ADX, DI)",
    ] {
        tour.add_indicator(name);
    }
    tour.shot("indicators-trend")?;
    for label in ["Vol 20", "Pivots", "SuperTrend 10 3", "ADX 14 14"] {
        tour.click(&format!("Remove {label}"));
    }
    tour.add_indicator("Ichimoku cloud");
    tour.shot("ichimoku")?;
    tour.click("Remove Ichimoku 9 26 52");
    // The trading sessions in each look: boxes, soft zones that blend, gradients with high and low lines.
    tour.add_indicator("Trading sessions");
    tour.shot("trading-sessions")?;
    tour.click("Soft");
    tour.shot("trading-sessions-soft")?;
    tour.click("Gradient");
    tour.click("On");
    tour.shot("trading-sessions-gradient")?;
    tour.click("Remove Sessions");
    // A session of one's own: its city picked from a list, its hours on that city's clock.
    tour.add_indicator("Session of your own");
    tour.click("New York");
    tour.shot("own-session-city")?;
    tour.click("London");
    tour.shot("own-session")?;
    tour.click("Remove Session London 09:30-16:00");
    tour.click("Indicators");

    // A strategy run over the bars: its trades on the candles, its equity in a pane, how it did on its page.
    tour.click("Strategy");
    tour.shot("strategy-page")?;
    tour.click("Run on the chart");
    tour.shot("strategy")?;
    // The bars' features written beside the strategies, to train a model on elsewhere.
    tour.click("Write the bars for training");
    tour.shot("strategy-features")?;
    // The last 30% of the bars kept apart to test it on, then the test run: how it did before and in it.
    tour.click_beside("Test on the last %");
    tour.key(Key::SelectAll);
    tour.type_text("30");
    tour.key(Key::Enter);
    tour.shot("strategy-test-apart")?;
    tour.click("Run the test");
    tour.shot("strategy-tested")?;
    // A walk forward over the bars before the test: settings chosen on each window, traded on the one after it.
    // A taller window shows the page down to its table.
    tour.resize(1200, 1100, 1.0);
    tour.click("Start walking");
    for _ in 0..5000 {
        if !tour.has("Stop walking") {
            break;
        }
        tour.event(Event::Tick { dt: 0.016 });
    }
    let (x, y) = tour.chart_point(0.5, 0.3);
    tour.event(Event::PointerMoved { x, y });
    tour.shot("strategy-walk")?;
    tour.resize(1200, 700, 1.0);
    tour.click_beside("Test on the last %");
    tour.key(Key::SelectAll);
    tour.type_text("0");
    tour.key(Key::Enter);
    // The newest trade picked in the list: brought into view, its line drawn solid.
    let chart = tour.window.workspace().chart();
    let newest = *closed_trades(chart.resources().get::<StrategyRun>().unwrap().account.fills())
        .last()
        .unwrap();
    let step = native_step(chart.seen()).unwrap();
    tour.click(&short_time(newest.entry, step, Zone::Utc));
    tour.shot("strategy-trade")?;
    tour.click("RSI 14 reversal from 30");
    tour.shot("strategy-rsi")?;
    // Watching it trade: the replay plays from a little before each entry to its exit, the time between skipped.
    let page = tour.at("Take it off the chart");
    tour.reveal(page, "Next trade");
    tour.click("Watch it trade");
    for _ in 0..60 {
        tour.event(Event::Tick { dt: 0.05 });
    }
    tour.shot("strategy-watching")?;
    tour.click("Next trade");
    for _ in 0..20 {
        tour.event(Event::Tick { dt: 0.05 });
    }
    tour.shot("strategy-watching-next")?;
    // Paused by itself at the exit of a loss, the page scrolled to why that trade went in.
    tour.wheel(page, -10.0);
    tour.click("Pause at each loss");
    for _ in 0..3000 {
        tour.event(Event::Tick { dt: 0.05 });
        if tour.has("Play") {
            break;
        }
    }
    tour.shot("strategy-paused")?;
    tour.click("Pause at each loss");
    tour.reveal(page, "Stop watching");
    tour.click("Stop watching");
    tour.event(Event::Command(commands::REPLAY_TOGGLE));
    // A trade marked as wrong, then a setting changed: the trades it took away are ghosts, the mark stays.
    tour.reveal(page, "Pause past bars held");
    tour.click("Previous trade");
    tour.reveal(page, "Mark as wrong");
    tour.click("Mark as wrong");
    tour.wheel(page, 40.0);
    tour.click_beside("Level");
    tour.key(Key::SelectAll);
    tour.type_text("25");
    tour.key(Key::Enter);
    for _ in 0..2 {
        tour.wheel(tour.chart_point(0.9, 0.5), -1.0);
    }
    tour.shot("strategy-changed")?;
    tour.click_beside("Level");
    tour.key(Key::SelectAll);
    tour.type_text("30");
    tour.key(Key::Enter);
    // In a replay its trades come out from left to right, as the bars do.
    tour.click("Moving average cross 10/30");
    if tour.window.workspace().chart().cursor().is_none() {
        tour.event(Event::Command(commands::REPLAY_TOGGLE));
    }
    for _ in 0..40 {
        tour.event(Event::Command(commands::REPLAY_STEP));
    }
    tour.shot("strategy-replay")?;
    for _ in 0..40 {
        tour.event(Event::Command(commands::REPLAY_STEP));
    }
    tour.shot("strategy-replay-later")?;
    // On until the strategy trades on the bar just shown: that fill stands out.
    for _ in 0..400 {
        tour.event(Event::Command(commands::REPLAY_STEP));
        let chart = tour.window.workspace().chart();
        let last = chart.seen().time().last().copied();
        let run = chart.resources().get::<StrategyRun>();
        if run
            .and_then(|r| r.account.fills().last())
            .is_some_and(|f| Some(f.time) >= last)
        {
            break;
        }
    }
    tour.shot("strategy-replay-fill")?;
    tour.event(Event::Command(commands::REPLAY_TOGGLE));
    tour.click("Take it off the chart");
    // The one loaded from the strategies folder runs as the program's own do.
    tour.click("Channel breakout 20");
    tour.click("Run on the chart");
    tour.shot("strategy-wasm")?;
    tour.click("Take it off the chart");
    // A model fitted on the bars before, again every 50 bars; its newest trade picked says why it went in.
    tour.click("Learned direction 500/55");
    tour.click("Run on the chart");
    tour.click("Previous trade");
    tour.shot("strategy-learned")?;
    tour.click("Take it off the chart");
    tour.click("Moving average cross 10/30");
    // On a live chart it trades by itself on paper, beside its backtest of the same bars. Offline, the chart is
    // made live by hand and made-up bars come after its own; the series is opened again afterwards.
    tour.window
        .workspace_mut()
        .chart_mut()
        .resources_mut()
        .insert(Live(true));
    let page = tour.at("Run on the chart");
    tour.reveal(page, "Trade it on paper");
    tour.click("Trade it on paper");
    let chart = tour.window.workspace().chart();
    let base = chart.base();
    // A bar of the timeframe shown each, so the strategy sees each one.
    let step = chart.timeframe().or(native_step(base)).unwrap_or(60_000);
    let (mut time, mut close) = (base.time()[base.len() - 1].0, base.close()[base.len() - 1]);
    let mut seed: u64 = 11;
    for _ in 0..240 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let move_by = ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.006;
        let open = close;
        close = open * (1.0 + move_by);
        time += step;
        let candle = Candle {
            time: Time(time),
            open,
            high: open.max(close) * 1.0005,
            low: open.min(close) * 0.9995,
            close,
            volume: 10.0,
        };
        let pushed = tour.window.workspace_mut().chart_mut().push(candle);
        pushed.map_err(|_| "a made-up bar out of order")?;
        tour.settle(1);
    }
    tour.reveal(page, "Stop it and close all");
    tour.shot("strategy-live")?;
    tour.click("Stop it and close all");
    let ctx = &mut tour.window.workspace_mut().ctx;
    ctx.request = ctx.shown.clone();
    // A tick takes the request; then the load is waited for.
    tour.event(Event::Tick { dt: 0.05 });
    tour.settle(60);
    tour.click("Strategy");

    // The order flow on another symbol, typed in.
    if tour.has("Symbol") {
        // A symbol that does not exist.
        tour.click("Symbol");
        tour.key(Key::SelectAll);
        tour.type_text("nosuchcoin");
        tour.key(Key::Enter);
        tour.settle(60);
        tour.shot("unknown-symbol")?;
        tour.click("Symbol");
        tour.key(Key::SelectAll);
        tour.shot("symbol-selected")?;
        tour.type_text("solusdt");
        tour.key(Key::Left);
        tour.key(Key::Left);
        tour.shot("symbol-caret")?;
        // Shift with an arrow selects from the caret.
        for _ in 0..3 {
            tour.key(Key::SelectLeft);
        }
        tour.shot("symbol-shift-select")?;
        tour.key(Key::End);
        // Another market shows the symbol there at once.
        tour.click("Futures");
        tour.settle(60);
        tour.shot("other-symbol")?;
        tour.click("Footprint");
        tour.click("Profile");
        tour.settle(120);
        tour.shot("flow-default-zoom")?;
        for _ in 0..8 {
            tour.wheel(tour.chart_point(0.9, 0.5), 1.0);
        }
        tour.settle(120);
        tour.shot("flow-zoomed-in")?;
        // The replay cuts the order flow too: a start in the middle, then a few bars on.
        let middle = tour.chart_point(0.5, 0.5);
        tour.event(Event::PointerMoved {
            x: middle.0,
            y: middle.1,
        });
        tour.key(Key::Alt('P'));
        tour.settle(60);
        tour.shot("flow-replay")?;
        for _ in 0..2 {
            tour.key(Key::SelectRight);
        }
        tour.settle(60);
        tour.shot("flow-replay-stepped")?;
        // Bars of 15 seconds, made of the same tape and cut at the same moment; a step plays one of them on.
        tour.click(tickerlight_ui::panels::SECONDS_MENU);
        tour.click("15s");
        tour.settle(60);
        tour.shot("seconds-replay")?;
        for _ in 0..4 {
            tour.key(Key::SelectRight);
        }
        tour.settle(60);
        tour.shot("seconds-stepped")?;
        tour.click("15m");
    }
    eprintln!("flow: {}", tour.window.workspace().ctx.flow_status);

    // A file of another market: daily bars of a made-up stock, imported through the symbol field, then replayed.
    // Last, so the steps before see the program as it was.
    if tour.window.workspace().ctx.footprint {
        tour.click("Footprint");
    }
    if tour.window.workspace().ctx.profile {
        tour.click("Profile");
    }
    let csv = root.join("acme.csv");
    std::fs::write(&csv, stock_csv())?;
    // The source is a dropdown: its button shows the one picked, a click lists them all over the chart.
    tour.click("Binance");
    tour.shot("sources")?;
    tour.click("File");
    tour.click("CSV file or symbol");
    tour.key(Key::SelectAll);
    tour.type_text(&csv.display().to_string());
    tour.key(Key::Enter);
    tour.settle(60);
    tour.shot("imported")?;
    let middle = tour.chart_point(0.6, 0.5);
    tour.event(Event::PointerMoved {
        x: middle.0,
        y: middle.1,
    });
    tour.key(Key::Alt('P'));
    for _ in 0..5 {
        tour.key(Key::SelectRight);
    }
    tour.settle(60);
    tour.shot("imported-replay")?;
    // Another exchange: OKX's futures, picked in the symbol panel (offline here: what is not cached is not shown).
    tour.click("File");
    tour.click("OKX");
    tour.click("Futures");
    tour.window.workspace_mut().ctx.symbol = "BTC-USDT".into();
    tour.click("Symbol");
    tour.key(Key::Enter);
    tour.settle(60);
    tour.shot("okx")?;
    // Four charts of BTCUSDT: three timeframes, the pointer's time on all, then one replay for all. The exchange
    // picked shows only once its symbol loads.
    let shown = if tour.window.workspace().ui_state().rect_of("OKX").is_some() {
        "OKX"
    } else {
        "File"
    };
    tour.click(shown);
    tour.click("Binance");
    tour.click("Spot");
    tour.window.workspace_mut().ctx.symbol = "BTCUSDT".into();
    tour.click("Symbol");
    tour.key(Key::Enter);
    tour.settle(60);
    tour.click("Layout");
    tour.click("Four charts");
    tour.settle(120);
    let first = tour.window.workspace().chart_area();
    let cell = |i: usize| {
        let (column, row) = ((i % 2) as f32, (i / 2) as f32);
        (
            first.x + first.w * (column + 0.5) + 12.0 * column,
            first.y + first.h * (row + 0.5) + 12.0 * row,
        )
    };
    for (i, timeframe) in [(1, "15m"), (2, "1h"), (3, "4h")] {
        tour.click_at(cell(i));
        tour.click(timeframe);
    }
    tour.click_at(cell(0));
    tour.window.handle(Event::PointerMoved {
        x: first.x + first.w * 0.6,
        y: first.y + first.h * 0.4,
    });
    tour.settle(10);
    tour.shot("layout")?;
    // From a third of the way across the first chart, two of its bars on.
    tour.window.handle(Event::PointerMoved {
        x: first.x + first.w * 0.3,
        y: first.y + first.h * 0.4,
    });
    tour.key(Key::Alt('P'));
    for _ in 0..2 {
        tour.key(Key::SelectRight);
    }
    tour.settle(30);
    tour.shot("layout-replay")?;
    // A start at random: every chart without its dates, the weekday in their place, and no timeline.
    tour.click("Random start");
    tour.settle(30);
    tour.event(Event::PointerMoved {
        x: first.x + first.w * 0.5,
        y: first.y + first.h * 0.5,
    });
    tour.shot("random-start")?;
    let out = tour.dir.clone();
    drop(tour);
    // The picture kept as the position closed, written by the time the window is gone.
    for entry in std::fs::read_dir(root.join("sessions"))? {
        let picture = entry?.path().join("trade-1.png");
        if picture.exists() {
            std::fs::copy(&picture, out.join("trade-picture.png"))?;
        }
    }
    // Removes the link to the cache, not the cache.
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// Daily bars of a made-up stock on business days, as a broker exports them: a random walk that drifts up.
fn stock_csv() -> String {
    let mut seed: u64 = 7;
    let mut random = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut out = String::from("Date,Open,High,Low,Close,Adj Close,Volume\n");
    let first = tickerlight_core::calendar::days_from_civil(2024, 1, 1);
    let mut close = 42.0;
    for day in (first..).filter(|d| (d + 3).rem_euclid(7) < 5).take(320) {
        let (y, m, d) = tickerlight_core::calendar::civil_from_days(day);
        let open = close * (1.0 + (random() - 0.5) * 0.01);
        close = open * (1.0 + (random() - 0.48) * 0.04);
        let high = open.max(close) * (1.0 + random() * 0.015);
        let low = open.min(close) * (1.0 - random() * 0.015);
        let volume = 1_000_000.0 * (0.5 + random());
        out += &format!("{y}-{m:02}-{d:02},{open:.2},{high:.2},{low:.2},{close:.2},{close:.2},{volume:.0}\n");
    }
    out
}
