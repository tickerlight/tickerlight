//! Dukascopy's list of instruments, kept in the data folder: the loading needs an instrument's pip, the
//! watchlist the symbols to search.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tickerlight_binance::Ticker;
use tickerlight_core::Time;
use tickerlight_dukascopy::{Dukascopy, Instrument, parse_instruments};

/// How long a copy of the list serves before it is downloaded again.
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// The instruments most look at, first in the list in this order: the major pairs, gold and silver, the main
/// indices, oil, the biggest stocks.
const POPULAR: [&str; 20] = [
    "EURUSD",
    "GBPUSD",
    "USDJPY",
    "AUDUSD",
    "USDCAD",
    "USDCHF",
    "NZDUSD",
    "XAUUSD",
    "XAGUSD",
    "USA500IDXUSD",
    "USATECHIDXUSD",
    "USA30IDXUSD",
    "DEUIDXEUR",
    "GBRIDXGBP",
    "JPNIDXJPY",
    "LIGHTCMDUSD",
    "BRENTCMDUSD",
    "AAPLUSUSD",
    "MSFTUSUSD",
    "NVDAUSUSD",
];

fn path(root: &Path) -> PathBuf {
    root.join("dukascopy").join("instruments.txt")
}

/// The instruments: the copy kept when it is under [`KEEP`] old, else downloaded and kept; a copy of any age
/// when the download fails.
pub fn instruments(root: &Path) -> Result<Vec<Instrument>, String> {
    let path = path(root);
    let kept = std::fs::read_to_string(&path).ok();
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < KEEP));
    if let (Some(text), true) = (&kept, fresh)
        && let Ok(all) = parse_instruments(text)
    {
        return Ok(all);
    }
    let downloaded = Dukascopy::new()
        .instruments_text()
        .map_err(|e| e.to_string())
        .and_then(|text| {
            parse_instruments(&text)
                .map(|all| (text, all))
                .map_err(|e| e.to_string())
        });
    match (downloaded, kept) {
        (Ok((text, all)), _) => {
            // Not kept: asked for again next time.
            let _ = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, text));
            Ok(all)
        }
        (Err(e), Some(text)) => parse_instruments(&text).map_err(|_| e),
        (Err(e), None) => Err(e),
    }
}

/// The instrument of `symbol`.
pub fn instrument(root: &Path, symbol: &str) -> Result<Instrument, String> {
    instruments(root)?
        .into_iter()
        .find(|i| i.symbol == symbol)
        .ok_or_else(|| format!("There is no {symbol} on Dukascopy"))
}

/// The instrument of `symbol` in the copy of the list kept here, without asking for it.
pub fn kept(root: &Path, symbol: &str) -> Option<Instrument> {
    let text = std::fs::read_to_string(path(root)).ok()?;
    parse_instruments(&text).ok()?.into_iter().find(|i| i.symbol == symbol)
}

/// The instruments as the watchlist's tickers, without prices: the popular ones first, then by symbol.
pub fn tickers(root: &Path) -> Result<Vec<Ticker>, String> {
    let rank = |symbol: &str| POPULAR.iter().position(|&p| p == symbol).unwrap_or(POPULAR.len());
    let mut all: Vec<Ticker> = instruments(root)?
        .into_iter()
        .map(|i| Ticker {
            symbol: i.symbol,
            last: f64::NAN,
            change: f64::NAN,
            quote_volume: 0.0,
            close_time: Time(0),
        })
        .collect();
    all.sort_by_key(|t| rank(&t.symbol));
    Ok(all)
}
