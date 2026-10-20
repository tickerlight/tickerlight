//! Coinbase's answers as chart data.

use std::collections::HashMap;

use serde_json::Value;
use tickerlight_core::calendar::time_of_date;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker};

/// A trade with its id: ids follow one another per product, which pages and reconnections go by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// A number Coinbase writes as a string (prices, sizes) or as a number (candles).
fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

/// A moment as Coinbase writes it: `2026-10-19T06:33:38.278388Z`.
pub fn time(text: &str) -> Option<Time> {
    let text = text.strip_suffix('Z')?;
    let (date, clock) = text.split_once('T')?;
    let mut date = date.split('-');
    let (year, month, day) = (
        date.next()?.parse().ok()?,
        date.next()?.parse().ok()?,
        date.next()?.parse().ok()?,
    );
    let mut clock = clock.split(':');
    let (hour, minute): (i64, i64) = (clock.next()?.parse().ok()?, clock.next()?.parse().ok()?);
    let seconds = clock.next()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let seconds: i64 = seconds.parse().ok()?;
    // Milliseconds: the first three digits of the fraction.
    let ms = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .try_fold(0, |ms, b| b.is_ascii_digit().then(|| ms * 10 + i64::from(b - b'0')))?;
    Some(Time(
        time_of_date(year, month, day).0 + ((hour * 60 + minute) * 60 + seconds) * 1000 + ms,
    ))
}

/// `/products/<id>/candles`: `[time in seconds, low, high, open, close, volume]` rows, the newest first.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let rows = json(body)?;
    let rows = rows
        .as_array()
        .ok_or_else(|| Error::Parse("candles are not a list".into()))?;
    let mut out = rows
        .iter()
        .map(|row| {
            let field = |i: usize| number(row.get(i), "a candle field");
            Ok(Candle {
                time: Time(field(0)? as i64 * 1000),
                low: field(1)?,
                high: field(2)?,
                open: field(3)?,
                close: field(4)?,
                volume: field(5)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    out.reverse();
    Ok(out)
}

/// A trade of `/products/<id>/trades` or a `match` of the stream. Its side is the resting order's: a resting sell
/// was bought from.
fn trade(object: &Value) -> Result<IdTrade, Error> {
    let id = object
        .get("trade_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Parse("a trade without an id".into()))?;
    let time = object
        .get("time")
        .and_then(Value::as_str)
        .and_then(time)
        .ok_or_else(|| Error::Parse("a trade without a time".into()))?;
    let side = match object.get("side").and_then(Value::as_str) {
        Some("sell") => Side::Buy,
        Some("buy") => Side::Sell,
        _ => return Err(Error::Parse("a trade without a side".into())),
    };
    Ok(IdTrade {
        id,
        trade: Trade {
            time,
            price: number(object.get("price"), "a price")?,
            qty: number(object.get("size"), "a size")?,
            side,
        },
    })
}

/// `/products/<id>/trades`: the newest first; returned oldest first.
pub fn trades(body: &str) -> Result<Vec<IdTrade>, Error> {
    let list = json(body)?;
    let list = list
        .as_array()
        .ok_or_else(|| Error::Parse("trades are not a list".into()))?;
    let mut out = list.iter().map(trade).collect::<Result<Vec<_>, Error>>()?;
    out.reverse();
    Ok(out)
}

/// `/products` and `/products/stats`: the products open for trading with their day's prices. The change is from
/// the price 24 hours ago; the volume, in the base currency, is turned into the quote.
pub fn tickers(products: &str, stats: &str, now: Time) -> Result<Vec<Ticker>, Error> {
    let products = json(products)?;
    let products = products
        .as_array()
        .ok_or_else(|| Error::Parse("products are not a list".into()))?;
    let stats = json(stats)?;
    let stats: HashMap<&str, &Value> = stats
        .as_object()
        .ok_or_else(|| Error::Parse("stats are not an object".into()))?
        .iter()
        .map(|(id, s)| (id.as_str(), s))
        .collect();
    let mut out = Vec::new();
    for product in products {
        let text = |key: &str| product.get(key).and_then(Value::as_str).unwrap_or_default();
        let disabled = product
            .get("trading_disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if text("status") != "online" || disabled {
            continue;
        }
        let Some(day) = stats.get(text("id")).and_then(|s| s.get("stats_24hour")) else {
            continue;
        };
        let (Ok(open), Ok(last), Ok(volume)) = (
            number(day.get("open"), "open"),
            number(day.get("last"), "last"),
            number(day.get("volume"), "volume"),
        ) else {
            continue;
        };
        out.push(Ticker {
            symbol: text("id").to_string(),
            last,
            change: if open > 0.0 { (last / open - 1.0) * 100.0 } else { 0.0 },
            quote_volume: volume * last,
            close_time: now,
        });
    }
    Ok(out)
}

/// A message of the stream: the trades of a `match` (or `last_match`, the one before subscribing); none of others.
pub fn match_event(text: &str) -> Result<Vec<IdTrade>, Error> {
    let message = json(text)?;
    match message.get("type").and_then(Value::as_str) {
        Some("match" | "last_match") => Ok(vec![trade(&message)?]),
        _ => Ok(Vec::new()),
    }
}

/// A `ticker` message of the stream: the last price of a product; `None` for other messages.
pub fn ticker_event(text: &str) -> Result<Option<Ticker>, Error> {
    let message = json(text)?;
    if message.get("type").and_then(Value::as_str) != Some("ticker") {
        return Ok(None);
    }
    let last = number(message.get("price"), "price")?;
    let open = number(message.get("open_24h"), "open")?;
    let volume = number(message.get("volume_24h"), "volume")?;
    let close_time = message
        .get("time")
        .and_then(Value::as_str)
        .and_then(time)
        .unwrap_or(Time(0));
    Ok(Some(Ticker {
        symbol: message
            .get("product_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        last,
        change: if open > 0.0 { (last / open - 1.0) * 100.0 } else { 0.0 },
        quote_volume: volume * last,
        close_time,
    }))
}

/// An error answer: `{"message":"NotFound"}`.
pub fn message(body: &str) -> String {
    json(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_to_the_millisecond_with_or_without_a_fraction() {
        let day = time_of_date(2026, 10, 19).0;
        let at = |h: i64, m: i64, s: i64| day + ((h * 60 + m) * 60 + s) * 1000;
        assert_eq!(time("2026-10-19T06:33:38.278388Z"), Some(Time(at(6, 33, 38) + 278)));
        assert_eq!(time("2026-10-19T06:33:38.2Z"), Some(Time(at(6, 33, 38) + 200)));
        assert_eq!(time("2026-10-19T06:33:38Z"), Some(Time(at(6, 33, 38))));
        assert_eq!(time("2026-10-19 06:33:38"), None);
    }

    #[test]
    fn candles_come_oldest_first_and_a_trade_s_side_is_the_taker_s() {
        let got =
            candles("[[1577837040,7155,7158.9,7158.9,7155.5,6.99],[1577836980,7156.13,7161.78,7158.51,7161.73,1.02]]")
                .unwrap();
        assert_eq!(got[0].time, Time(1_577_836_980_000));
        assert_eq!(
            (got[1].open, got[1].high, got[1].low, got[1].close),
            (7158.9, 7158.9, 7155.0, 7155.5)
        );
        let body = r#"[{"trade_id":12,"side":"sell","size":"0.5","price":"82711.00","time":"2026-10-19T06:33:38.278Z"},
            {"trade_id":11,"side":"buy","size":"0.25","price":"82710.99","time":"2026-10-19T06:33:34.853Z"}]"#;
        let got = trades(body).unwrap();
        assert_eq!(got.iter().map(|t| t.id).collect::<Vec<_>>(), [11, 12]);
        // A resting buy was sold into: the taker sold.
        assert_eq!((got[0].trade.side, got[0].trade.qty), (Side::Sell, 0.25));
        assert_eq!((got[1].trade.side, got[1].trade.price), (Side::Buy, 82711.0));
    }

    #[test]
    fn tickers_are_the_products_open_for_trading() {
        let products = r#"[{"id":"BTC-USD","status":"online","trading_disabled":false},
            {"id":"OLD-USD","status":"delisted","trading_disabled":true},
            {"id":"ETH-BTC","status":"online","trading_disabled":false}]"#;
        let stats = r#"{"BTC-USD":{"stats_24hour":{"open":"80000","last":"82000","volume":"10"}},
            "OLD-USD":{"stats_24hour":{"open":"1","last":"1","volume":"1"}},
            "ETH-BTC":{"stats_24hour":{"open":"0.04","last":"0.03","volume":"100"}}}"#;
        let got = tickers(products, stats, Time(5)).unwrap();
        let symbols: Vec<&str> = got.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(symbols, ["BTC-USD", "ETH-BTC"]);
        assert!((got[0].change - 2.5).abs() < 1e-9);
        assert_eq!(got[0].quote_volume, 820_000.0);
        assert!((got[1].quote_volume - 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_stream_gives_matches_and_tickers() {
        let m = r#"{"type":"match","trade_id":7,"side":"buy","size":"1","price":"10","product_id":"BTC-USD","time":"2026-10-19T00:00:01Z"}"#;
        assert_eq!(match_event(m).unwrap()[0].trade.side, Side::Sell);
        assert!(
            match_event(r#"{"type":"subscriptions","channels":[]}"#)
                .unwrap()
                .is_empty()
        );
        let t = r#"{"type":"ticker","product_id":"ETH-USD","price":"110","open_24h":"100","volume_24h":"2","time":"2026-10-19T00:00:01Z"}"#;
        let got = ticker_event(t).unwrap().unwrap();
        assert_eq!(
            (got.symbol.as_str(), got.last, got.quote_volume),
            ("ETH-USD", 110.0, 220.0)
        );
        assert!((got.change - 10.0).abs() < 1e-9);
    }
}
