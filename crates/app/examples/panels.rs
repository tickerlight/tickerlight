//! Renders the window in use into PNGs, without opening it: a replay with a few trades on cached data, tagged,
//! and then the sessions page (into `OUT-sessions.png`) after exporting the trades as CSV. The session is saved in a
//! scratch data folder with the cached market data linked in.
//!
//! TICKERLIGHT_DATA_DIR=... cargo run -p tickerlight-app --example panels -- OUT.png [SYMBOL]

use std::error::Error;

use tickerlight_app::program::{self, Source, Window};
use tickerlight_app::store::{Series, Venue};
use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::{Interval, Market};
use tickerlight_core::builtin::Account;
use tickerlight_core::calendar::Zone;
use tickerlight_core::{Event, Key, Scene, Surface};
use tickerlight_ui::panels::short_time;

const SIZE: (u32, u32) = (1400, 800);

fn click(window: &mut Window, text: &str) {
    let rects = window.workspace().ui_state().rects();
    let area = rects
        .iter()
        .rev()
        .find(|(t, _)| t == text)
        .map(|&(_, a)| a)
        .unwrap_or_else(|| panic!("no control {text}"));
    click_at(window, area.center());
}

/// The control laid out right after `heading`: the field it names.
fn click_beside(window: &mut Window, heading: &str) {
    let rects = window.workspace().ui_state().rects();
    let i = rects
        .iter()
        .position(|(t, _)| t == heading)
        .unwrap_or_else(|| panic!("no {heading}"));
    let at = rects[i + 1].1.center();
    click_at(window, at);
}

fn click_at(window: &mut Window, (x, y): (f32, f32)) {
    window.handle(Event::PointerMoved { x, y });
    window.handle(Event::PointerDown);
    window.handle(Event::PointerUp);
    window.build_scene(&mut Scene::new());
}

fn type_into(window: &mut Window, field: &str, text: &str) {
    click(window, field);
    type_text(window, text);
}

fn type_beside(window: &mut Window, heading: &str, text: &str) {
    click_beside(window, heading);
    type_text(window, text);
}

fn type_text(window: &mut Window, text: &str) {
    window.handle(Event::Key(Key::SelectAll));
    for c in text.chars() {
        window.handle(Event::Key(Key::Char(c)));
    }
    window.handle(Event::Key(Key::Enter));
    window.build_scene(&mut Scene::new());
}

fn scroll_column(window: &mut Window, notches: f32) {
    let (x, y) = (SIZE.0 as f32 - 140.0, 200.0);
    window.handle(Event::PointerMoved { x, y });
    window.handle(Event::Wheel { x, delta: notches });
    window.build_scene(&mut Scene::new());
}

fn last_close(window: &Window) -> f64 {
    *window.workspace().chart().base().close().last().unwrap()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("an output file")?;
    let series = Series {
        venue: Venue::Binance(Market::Spot),
        symbol: args.next().unwrap_or_else(|| "BTCUSDT".into()),
        interval: Interval::M1,
    };
    let source = Source {
        root: tickerlight_app::store::scratch_root(
            std::env::var_os("TICKERLIGHT_DATA_DIR")
                .ok_or("set TICKERLIGHT_DATA_DIR")?
                .as_ref(),
            "panels",
        )?,
        months: 3,
        offline: true,
    };
    let candles = program::load(&source, &series, program::now(), |_| {})?;
    let mut window = Window::new(source, series, Some(candles));
    window.resize(SIZE.0 as f32, SIZE.1 as f32);
    window.build_scene(&mut Scene::new());

    click(&mut window, "15m");
    // Start a few days before the end of the data.
    let (_, last) = window.workspace().chart().data_span().ok_or("no data")?;
    let start = tickerlight_core::calendar::DateTime::from_time(tickerlight_core::Time(last.0 - 4 * 86_400_000));
    let date = format!("{}-{:02}-{:02} 12:00", start.year, start.month, start.day);
    type_into(&mut window, "Start date", &date);
    // One coin an order.
    click(&mut window, "Quantity");
    type_into(&mut window, "Order size", "1");
    for round in 0..4 {
        let price = last_close(&window);
        let (side, tp, sl) = if round % 2 == 0 {
            ("Buy", price * 1.004, price * 0.997)
        } else {
            ("Sell", price * 0.996, price * 1.003)
        };
        click(&mut window, side);
        type_beside(&mut window, "Take profit", &format!("{tp:.2}"));
        type_beside(&mut window, "Stop loss", &format!("{sl:.2}"));
        click(&mut window, &format!("{side} 1 at market"));
        // The entry gets a tag.
        let fills = window
            .workspace()
            .chart()
            .resources()
            .get::<Account>()
            .unwrap()
            .fills()
            .to_vec();
        // The trades are below the window: scrolled to, then back to the replay.
        scroll_column(&mut window, -10.0);
        click(&mut window, &short_time(fills.last().unwrap().time, 60_000, Zone::Utc));
        type_into(&mut window, "Tags", if round < 2 { "breakout" } else { "fade" });
        if round == 0 {
            type_into(&mut window, "Note", "entered on the retest");
        }
        scroll_column(&mut window, 10.0);
        for _ in 0..240 {
            click(&mut window, "Step");
        }
    }
    let price = last_close(&window);
    click(&mut window, "Limit");
    type_into(&mut window, "Limit price", &format!("{:.2}", price * 0.995));
    click(&mut window, "Buy");
    click(&mut window, "Buy 1 limit");

    let gpu = Gpu::new_headless()?;
    save(&gpu, &mut window, &out)?;
    click(&mut window, "Sessions");
    click(&mut window, "Export CSV");
    eprintln!("{:?}", window.workspace().ctx.exported);
    let sessions = out.strip_suffix(".png").unwrap_or(&out).to_string() + "-sessions.png";
    save(&gpu, &mut window, &sessions)
}

fn save(gpu: &Gpu, window: &mut Window, out: &str) -> Result<(), Box<dyn Error>> {
    let (w, h) = SIZE;
    let mut scene = Scene::new();
    window.build_scene(&mut scene);
    let rgba = render_to_rgba(gpu, &scene, w, h);
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(out)?), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
