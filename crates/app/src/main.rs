//! Tickerlight, the desktop program: a chart of one symbol with its panels (drawings, indicators, watchlist,
//! strategy, replay and virtual trading), on data kept in a local cache that is brought up to date from public
//! archives and APIs (Binance, OKX, Coinbase, Dukascopy).
//!
//! tickerlight [SYMBOL] [--okx] [--futures] [--coinbase] [--dukascopy] [--months N] [--offline] [--snapshot FILE.png]
//!
//! Without a symbol it starts where it was left: the symbol, the timeframe and the session.

// A release build on Windows opens no console window; a fatal error shows in a message box instead.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::error::Error;
use std::path::PathBuf;

use tickerlight_app::program::{self, Source, Window};
use tickerlight_app::store::{Series, Venue};
use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_binance::{Interval, Market};
use tickerlight_core::{Scene, Surface};

/// The folder of the program's data inside the system's data folder.
const DATA_FOLDER: &str = "tickerlight";
/// Where the data were kept before the program had its name: moved to [`DATA_FOLDER`] on the first start.
const OLD_DATA_FOLDER: &str = "charts";
/// Overrides where the data are kept.
const DATA_ENV: &str = "TICKERLIGHT_DATA_DIR";
const SNAPSHOT_SIZE: (u32, u32) = (1400, 800);

struct Options {
    /// The series asked for on the command line.
    series: Option<Series>,
    months: u32,
    offline: bool,
    snapshot: Option<PathBuf>,
}

fn options() -> Result<Options, String> {
    let mut symbol = None;
    let mut futures = false;
    // Binance unless a flag names another source: `--okx`, `--coinbase`, `--kucoin`, `--bitget`, `--kraken`,
    // `--bitstamp`, `--bitfinex`, `--deribit`, `--dukascopy`.
    let mut exchange = 0;
    let mut months = 12;
    let mut offline = false;
    let mut snapshot = None;
    let exchanges = &Venue::EXCHANGES[..Venue::EXCHANGES.len() - 1];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let named = arg
            .strip_prefix("--")
            .and_then(|name| exchanges.iter().position(|e| e.eq_ignore_ascii_case(name)));
        if let Some(i) = named {
            exchange = i;
            continue;
        }
        match arg.as_str() {
            "--futures" => futures = true,
            "--offline" => offline = true,
            "--months" => {
                months = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--months needs a number")?
            }
            "--snapshot" => snapshot = Some(PathBuf::from(args.next().ok_or("--snapshot needs a file")?)),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            name => symbol = Some(name.to_string()),
        }
    }
    let venue = Venue::of_choices(exchange, usize::from(futures));
    let series = match symbol {
        Some(name) => Some(venue.symbol_of(&name).ok_or("no symbol")?),
        None if venue == Venue::Dukascopy => Some("EURUSD".to_string()),
        None if exchange != 0 || futures => venue.symbol_of(program::DEFAULT_SYMBOL),
        None => None,
    };
    Ok(Options {
        series: series.map(|symbol| Series {
            venue,
            symbol,
            interval: Interval::M1,
        }),
        months,
        offline,
        snapshot,
    })
}

fn data_root() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os(DATA_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let system = dirs::data_dir().ok_or_else(|| format!("no data folder on this system; set {DATA_ENV}"))?;
    let (root, old) = (system.join(DATA_FOLDER), system.join(OLD_DATA_FOLDER));
    // The folder of before is moved once; if it cannot be, the program goes on with it where it is.
    if !root.exists() && old.is_dir() && std::fs::rename(&old, &root).is_err() {
        return Ok(old);
    }
    Ok(root)
}

fn main() {
    if let Err(e) = run() {
        fail(&format!("error: {e}"));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn fail(text: &str) {
    eprintln!("{text}");
}

#[cfg(windows)]
fn fail(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    eprintln!("{text}");
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide("Tickerlight"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

fn run() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    let source = Source {
        root: data_root()?,
        months: options.months,
        offline: options.offline,
    };
    match &options.snapshot {
        Some(path) => {
            let series = options.series.unwrap_or_else(|| Series {
                venue: Venue::Binance(Market::Spot),
                symbol: program::DEFAULT_SYMBOL.into(),
                interval: Interval::M1,
            });
            let candles = program::load(&source, &series, program::now(), |p| {
                if let Some(period) = p.fetched {
                    eprintln!("downloaded {} ({}/{})", period.label(), p.done, p.total);
                }
            })?;
            snapshot(Window::new(source, series, Some(candles)), path)
        }
        None => {
            let mut window = Window::resumed(source, options.series);
            if let Some(dir) = dirs::download_dir() {
                window.set_exports(dir);
            }
            tickerlight_backend_wgpu::run(window)
        }
    }
}

/// Renders the window offscreen into a PNG instead of opening it.
fn snapshot(mut window: Window, path: &PathBuf) -> Result<(), Box<dyn Error>> {
    let (w, h) = SNAPSHOT_SIZE;
    window.resize(w as f32, h as f32);
    let mut scene = Scene::new();
    window.build_scene(&mut scene);
    let rgba = render_to_rgba(&Gpu::new_headless()?, &scene, w, h);
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
