//! Bitget's answers and archive files as chart data. Sizes are in the base currency on both markets.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Market, Ticker};

/// A trade with its id, which grows with time: reconnections go by it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// A number Bitget writes as a string or as a number.
fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

/// An integer Bitget writes as a string or as a number.
fn integer(value: Option<&Value>, what: &str) -> Result<i64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not an integer")))
}

/// The taker's side, however it is written.
fn side(text: &str) -> Result<Side, Error> {
    if text.eq_ignore_ascii_case("buy") {
        Ok(Side::Buy)
    } else if text.eq_ignore_ascii_case("sell") {
        Ok(Side::Sell)
    } else {
        Err(Error::Parse(format!("side {text}")))
    }
}

/// The `data` of an answer, or the error it is: `{"code":"00000","data":...}`, `{"code":"40034","msg":...}`.
pub fn data(body: &str) -> Result<Value, Error> {
    let mut answer = json(body)?;
    let code = answer.get("code").and_then(Value::as_str).unwrap_or_default();
    if code != "00000" {
        let message = answer.get("msg").and_then(Value::as_str).unwrap_or_default();
        return Err(Error::Api {
            code: code.to_string(),
            message: message.to_string(),
        });
    }
    Ok(answer.get_mut("data").map(Value::take).unwrap_or(Value::Null))
}

fn rows<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>, Error> {
    value
        .as_array()
        .ok_or_else(|| Error::Parse(format!("{what} are not a list")))
}

/// A candle row of the API or the stream: `[start in ms, open, high, low, close, base volume, quote volume, ...]`.
fn candle_row(row: &Value) -> Result<Candle, Error> {
    let field = |i: usize| number(row.get(i), "a candle field");
    Ok(Candle {
        time: Time(integer(row.get(0), "a candle's time")?),
        open: field(1)?,
        high: field(2)?,
        low: field(3)?,
        close: field(4)?,
        volume: field(5)?,
    })
}

/// `history-candles` of either market: oldest first.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let data = data(body)?;
    let mut out = rows(&data, "candles")?
        .iter()
        .map(candle_row)
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|c| c.time);
    Ok(out)
}

/// A ticker object of the API or the stream: spot's turnover in dollars is `usdtVolume`, futures' `quoteVolume`
/// (all of them are quoted in USDT).
fn ticker_object(t: &Value, market: Market) -> Option<Ticker> {
    let volume = match market {
        Market::Spot => t.get("usdtVolume").or_else(|| t.get("quoteVolume")),
        Market::Futures => t.get("quoteVolume"),
    };
    Some(Ticker {
        symbol: t.get("symbol").or_else(|| t.get("instId"))?.as_str()?.to_string(),
        last: number(t.get("lastPr"), "last").ok()?,
        change: number(t.get("change24h"), "change").ok()? * 100.0,
        quote_volume: number(volume, "turnover").ok()?,
        close_time: Time(integer(t.get("ts"), "time").unwrap_or(0)),
    })
}

/// `/api/v2/spot/market/tickers` or `/api/v2/mix/market/tickers`: every symbol with its last price and day's change
/// and turnover.
pub fn tickers(body: &str, market: Market) -> Result<Vec<Ticker>, Error> {
    let data = data(body)?;
    Ok(rows(&data, "tickers")?
        .iter()
        .filter_map(|t| ticker_object(t, market))
        .collect())
}

/// A trade object of the API or the stream: `tradeId`, `price`, `size`, `side` (the taker's), `ts`.
fn trade_object(t: &Value) -> Result<IdTrade, Error> {
    Ok(IdTrade {
        id: integer(t.get("tradeId"), "a trade's id")?,
        trade: Trade {
            time: Time(integer(t.get("ts"), "a trade's time")?),
            price: number(t.get("price"), "a price")?,
            qty: number(t.get("size"), "a size")?,
            side: side(t.get("side").and_then(Value::as_str).unwrap_or_default())?,
        },
    })
}

/// `fills-history` (newest first): oldest first.
pub fn trades(body: &str) -> Result<Vec<IdTrade>, Error> {
    let data = data(body)?;
    let mut out = rows(&data, "trades")?
        .iter()
        .map(trade_object)
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// The rows of a pushed message on `channel` (a snapshot or an update); `None` for other messages, `pong` among
/// them.
fn pushed<'a>(message: &'a Value, channel: &str) -> Option<&'a Vec<Value>> {
    let arg = message.get("arg")?;
    (message.get("action").is_some() && arg.get("channel").and_then(Value::as_str) == Some(channel))
        .then(|| message.get("data").and_then(Value::as_array))
        .flatten()
}

/// The text of a message, or `None` when it is not JSON (`pong`).
fn message(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// A `trade` message: trades, oldest first.
pub fn trades_event(text: &str) -> Result<Vec<IdTrade>, Error> {
    let Some(value) = message(text) else {
        return Ok(Vec::new());
    };
    let mut out = pushed(&value, "trade")
        .map(|rows| rows.iter().map(trade_object).collect::<Result<Vec<_>, Error>>())
        .transpose()?
        .unwrap_or_default();
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// A `candle1m` message: the candles as they are now, oldest first (a snapshot has the last few hundred).
pub fn candles_event(text: &str) -> Result<Vec<Candle>, Error> {
    let Some(value) = message(text) else {
        return Ok(Vec::new());
    };
    let mut out = pushed(&value, "candle1m")
        .map(|rows| rows.iter().map(candle_row).collect::<Result<Vec<_>, Error>>())
        .transpose()?
        .unwrap_or_default();
    out.sort_by_key(|c| c.time);
    Ok(out)
}

/// A `ticker` message.
pub fn tickers_event(text: &str, market: Market) -> Vec<Ticker> {
    message(text)
        .and_then(|value| {
            pushed(&value, "ticker").map(|rows| rows.iter().filter_map(|t| ticker_object(t, market)).collect())
        })
        .unwrap_or_default()
}

/// A part of a day's trades of the archive, with a header naming the columns: `trade_id`, `timestamp` (ms),
/// `price`, `side` (the taker's), `volume(quote)`, `size(base)`. Oldest first.
pub fn archive_trades(csv: &str) -> Result<Vec<Trade>, Error> {
    let mut lines = csv.lines();
    let names: Vec<&str> = lines.next().unwrap_or_default().trim().split(',').collect();
    let at = |name: &str| {
        names
            .iter()
            .position(|n| n.trim() == name)
            .ok_or_else(|| Error::Parse(format!("no column {name}")))
    };
    let (time, price, side_at, size) = (at("timestamp")?, at("price")?, at("side")?, at("size(base)")?);
    let mut out = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.trim().split(',').collect();
        let field = |i: usize| fields.get(i).copied().unwrap_or_default();
        let bad = || Error::Parse(format!("a trade: {line}"));
        out.push(Trade {
            time: Time(field(time).parse().map_err(|_| bad())?),
            price: field(price).parse().map_err(|_| bad())?,
            qty: field(size).parse().map_err(|_| bad())?,
            side: side(field(side_at))?,
        });
    }
    out.sort_by_key(|t| t.time);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_trades_read_by_their_header() {
        let csv = "trade_id,timestamp,price,side,volume(quote),size(base)\n\
            1494657340550135808,1792080000001,82420.91,sell,7.99482827,0.000097\n\
            1494657340499804160,1792080000000,82420.91,buy,2.96715276,0.000036\n";
        let got = archive_trades(csv).unwrap();
        assert_eq!(
            (got[0].time, got[0].side, got[0].qty),
            (Time(1_792_080_000_000), Side::Buy, 0.000036)
        );
        assert_eq!(got[1].side, Side::Sell);
    }

    #[test]
    fn the_api_s_answers_and_errors() {
        let body = r#"{"code":"00000","msg":"success","data":[["1792399740000","82796.63","82817.22","82796.63","82804.98","0.497184","41173.9","41173.9"],
            ["1792399680000","82870.56","82872.62","82791.55","82796.63","0.614074","50868.5","50868.5"]]}"#;
        let got = candles(body).unwrap();
        assert_eq!(got[0].time, Time(1_792_399_680_000));
        assert_eq!(
            (got[1].open, got[1].high, got[1].low, got[1].close, got[1].volume),
            (82796.63, 82817.22, 82796.63, 82804.98, 0.497184)
        );
        let err = candles(r#"{"code":"40034","msg":"Parameter NOPEUSDT does not exist","data":null}"#).unwrap_err();
        assert!(err.is_unknown());
        let body = r#"{"code":"00000","data":[{"symbol":"ETHBTC","lastPr":"0.03","change24h":"-0.011","quoteVolume":"5","usdtVolume":"400000","ts":"9"}]}"#;
        let got = tickers(body, Market::Spot).unwrap();
        assert_eq!((got[0].quote_volume, got[0].close_time), (400000.0, Time(9)));
        assert!((got[0].change + 1.1).abs() < 1e-9);
        let body = r#"{"code":"00000","data":[{"tradeId":"1495304409686228992","price":"81624.1","size":"0.0009","side":"Buy","ts":"1792234273358","symbol":"BTCUSDT"},
            {"tradeId":"1495304384973389828","price":"81624","size":"0.016","side":"Sell","ts":"1792234267466","symbol":"BTCUSDT"}]}"#;
        let got = trades(body).unwrap();
        assert_eq!(
            (got[0].id, got[0].trade.side, got[1].trade.side),
            (1495304384973389828, Side::Sell, Side::Buy)
        );
    }

    #[test]
    fn the_stream_gives_trades_candles_and_tickers() {
        let t = r#"{"action":"update","arg":{"instType":"SPOT","channel":"trade","instId":"BTCUSDT"},
            "data":[{"ts":"1792403520132","price":"82957.84","size":"0.000188","side":"buy","tradeId":"1496014282107371520"}],"ts":1}"#;
        let got = trades_event(t).unwrap();
        assert_eq!(
            (got[0].id, got[0].trade.time, got[0].trade.side),
            (1496014282107371520, Time(1_792_403_520_132), Side::Buy)
        );
        let c = r#"{"action":"update","arg":{"instType":"USDT-FUTURES","channel":"candle1m","instId":"BTCUSDT"},
            "data":[["1792403520000","82912.6","82912.7","82912.6","82912.7","0.0121","1003.2","1003.2"]],"ts":1}"#;
        let got = candles_event(c).unwrap();
        assert_eq!(
            (got[0].time, got[0].high, got[0].volume),
            (Time(1_792_403_520_000), 82912.7, 0.0121)
        );
        let s = r#"{"action":"snapshot","arg":{"instType":"USDT-FUTURES","channel":"ticker","instId":"BTCUSDT"},
            "data":[{"instId":"BTCUSDT","lastPr":"82912.6","change24h":"0.00889","quoteVolume":"2150510185.07","ts":"1792403518836"}]}"#;
        let got = tickers_event(s, Market::Futures);
        assert_eq!((got[0].symbol.as_str(), got[0].last), ("BTCUSDT", 82912.6));
        assert!(trades_event("pong").unwrap().is_empty());
        assert!(candles_event(t).unwrap().is_empty(), "a trade is no candle");
    }
}
