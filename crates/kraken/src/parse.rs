//! Kraken's futures answers as chart data. Sizes are in the base currency.

use serde_json::Value;
use tickerlight_core::calendar::days_from_civil;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker};

/// A trade with its id. The ids do not grow with time: trades come in time order, and those of one millisecond
/// are told apart by id.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub uid: u128,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// A number Kraken writes as a string or as a number.
fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

fn integer(value: Option<&Value>, what: &str) -> Result<i64, Error> {
    value
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Parse(format!("{what} is not an integer")))
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

/// A trade's id, a UUID, as a number.
fn uid(value: Option<&Value>) -> Result<u128, Error> {
    let text = value.and_then(Value::as_str).unwrap_or_default();
    let hex: String = text.chars().filter(|&c| c != '-').collect();
    u128::from_str_radix(&hex, 16).map_err(|_| Error::Parse(format!("trade id {text}")))
}

/// A moment as Kraken writes it: `2026-10-19T10:25:39.451215875Z`.
fn moment(text: &str) -> Option<Time> {
    let (date, clock) = text.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut clock = clock.split(':');
    let (hour, minute): (i64, i64) = (clock.next()?.parse().ok()?, clock.next()?.parse().ok()?);
    let second: f64 = clock.next()?.parse().ok()?;
    let days = days_from_civil(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?);
    Some(Time(
        days * 86_400_000 + (hour * 60 + minute) * 60_000 + (second * 1000.0) as i64,
    ))
}

/// A continuation token as a URL query value: it is base64, so `+`, `/` and `=` are written out.
pub fn url_encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// An answer of the `derivatives` API, or the error it is: `{"result":"error","error":"contractNotFound"}`.
fn answer(body: &str) -> Result<Value, Error> {
    let value = json(body)?;
    if value.get("result").and_then(Value::as_str) == Some("error") {
        let error = value.get("error").and_then(Value::as_str).unwrap_or("unknown");
        return Err(Error::Api(error.to_string()));
    }
    Ok(value)
}

/// `/api/charts/v1/trade/...`: `{"candles":[{"time":ms,"open":"...",...,"volume":"..."}],"more_candles":...}`,
/// oldest first.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let value = json(body)?;
    let rows = value
        .get("candles")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no candles".into()))?;
    let mut out = rows
        .iter()
        .map(|c| {
            Ok(Candle {
                time: Time(integer(c.get("time"), "a candle's time")?),
                open: number(c.get("open"), "an open")?,
                high: number(c.get("high"), "a high")?,
                low: number(c.get("low"), "a low")?,
                close: number(c.get("close"), "a close")?,
                volume: number(c.get("volume"), "a volume")?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|c| c.time);
    Ok(out)
}

/// A ticker of the API: a perpetual contract margined in any currency (`PF_`) that trades, else `None`.
fn ticker_object(t: &Value) -> Option<Ticker> {
    let symbol = t.get("symbol")?.as_str()?;
    if !symbol.starts_with("PF_")
        || t.get("tag").and_then(Value::as_str) != Some("perpetual")
        || t.get("suspended").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    Some(Ticker {
        symbol: symbol.to_string(),
        last: number(t.get("last"), "a price").ok()?,
        change: number(t.get("change24h"), "a change").unwrap_or(0.0),
        quote_volume: number(t.get("volumeQuote"), "a turnover").unwrap_or(0.0),
        close_time: t
            .get("lastTime")
            .and_then(Value::as_str)
            .and_then(moment)
            .unwrap_or(Time(0)),
    })
}

/// `/derivatives/api/v3/tickers`: the perpetual contracts that trade.
pub fn tickers(body: &str) -> Result<Vec<Ticker>, Error> {
    let value = answer(body)?;
    let all = value
        .get("tickers")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no tickers".into()))?;
    Ok(all.iter().filter_map(ticker_object).collect())
}

/// `/derivatives/api/v3/tickers/{symbol}`.
pub fn ticker(body: &str) -> Result<Ticker, Error> {
    let value = answer(body)?;
    value
        .get("ticker")
        .and_then(ticker_object)
        .ok_or_else(|| Error::Api("contractNotFound".into()))
}

/// `/api/history/v2/market/{symbol}/executions`: the trades of a page, oldest first (asked so), and the token of
/// the next page, if there is one. Each element is an `Execution` event: its `execution` has the `uid`, `price`,
/// `quantity`, `timestamp` and the taker's order with its `direction`.
pub fn executions(body: &str) -> Result<(Vec<IdTrade>, Option<String>), Error> {
    let value = json(body)?;
    let elements = value
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no elements".into()))?;
    let mut out = Vec::with_capacity(elements.len());
    for element in elements {
        let Some(e) = element.pointer("/event/Execution/execution") else {
            continue;
        };
        let direction = e.pointer("/takerOrder/direction").and_then(Value::as_str);
        out.push(IdTrade {
            uid: uid(e.get("uid"))?,
            trade: Trade {
                time: Time(integer(e.get("timestamp"), "a trade's time")?),
                price: number(e.get("price"), "a price")?,
                qty: number(e.get("quantity"), "a quantity")?,
                side: side(direction.unwrap_or_default())?,
            },
        });
    }
    let token = value
        .get("continuationToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((out, token))
}

/// A trade of the stream: `uid`, `side` (the taker's), `time`, `qty`, `price`.
fn trade_object(t: &Value) -> Result<IdTrade, Error> {
    Ok(IdTrade {
        uid: uid(t.get("uid"))?,
        trade: Trade {
            time: Time(integer(t.get("time"), "a trade's time")?),
            price: number(t.get("price"), "a price")?,
            qty: number(t.get("qty"), "a quantity")?,
            side: side(t.get("side").and_then(Value::as_str).unwrap_or_default())?,
        },
    })
}

/// A message of the `trade` feed: its trades, oldest first (a snapshot has the last hundred, newest first); none
/// for other messages.
pub fn trades_event(text: &str) -> Result<Vec<IdTrade>, Error> {
    let value = json(text)?;
    let mut out = match value.get("feed").and_then(Value::as_str) {
        Some("trade") if value.get("uid").is_some() => vec![trade_object(&value)?],
        Some("trade_snapshot") => value
            .get("trades")
            .and_then(Value::as_array)
            .map_or(Ok(Vec::new()), |all| all.iter().map(trade_object).collect())?,
        _ => Vec::new(),
    };
    out.sort_by_key(|t| t.trade.time);
    Ok(out)
}

/// A message of the `ticker` feed: `product_id`, `last`, `change` (percent), `volumeQuote`, `time`.
pub fn tickers_event(text: &str) -> Option<Ticker> {
    let t = serde_json::from_str::<Value>(text).ok()?;
    if t.get("feed")?.as_str()? != "ticker" {
        return None;
    }
    Some(Ticker {
        symbol: t.get("product_id")?.as_str()?.to_string(),
        last: number(t.get("last"), "a price").ok()?,
        change: number(t.get("change"), "a change").unwrap_or(0.0),
        quote_volume: number(t.get("volumeQuote"), "a turnover").unwrap_or(0.0),
        close_time: Time(integer(t.get("time"), "a time").ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_s_answers_and_errors() {
        let body = r#"{"candles":[{"time":1792108860000,"open":"82149","high":"82160","low":"82140","close":"82150","volume":"0.5"},
            {"time":1792108800000,"open":"82142","high":"82151","low":"82130","close":"82149","volume":"1.4954"}],"more_candles":false}"#;
        let got = candles(body).unwrap();
        assert_eq!(got[0].time, Time(1_792_108_800_000));
        assert_eq!(
            (got[0].open, got[0].high, got[0].low, got[0].close, got[0].volume),
            (82142.0, 82151.0, 82130.0, 82149.0, 1.4954)
        );
        let err = ticker(r#"{"result":"error","error":"contractNotFound","serverTime":"2026-10-19T10:31:34.562Z"}"#);
        assert!(err.unwrap_err().is_unknown());
        let body = r#"{"result":"success","tickers":[
            {"symbol":"PF_XBTUSD","last":82389,"lastTime":"2026-10-19T10:25:39.451215875Z","tag":"perpetual","volumeQuote":323032190.4,"change24h":0.35,"suspended":false},
            {"symbol":"PI_XBTUSD","last":82390,"tag":"perpetual","volumeQuote":5.0,"change24h":0.3,"suspended":false},
            {"symbol":"FF_XBTUSD_261127","last":83000,"tag":"month","volumeQuote":5.0,"change24h":0.3,"suspended":false}]}"#;
        let got = tickers(body).unwrap();
        assert_eq!(got.len(), 1, "only the perpetuals margined in any currency");
        assert_eq!((got[0].last, got[0].change), (82389.0, 0.35));
        assert_eq!(got[0].close_time, Time(1_792_405_539_451));
    }

    #[test]
    fn executions_are_trades_by_the_taker_s_direction() {
        let body = r#"{"elements":[{"uid":"6069a5a9-52d0-48c7-9a92-372079a5ac84","timestamp":1792401941959,"event":{"Execution":{"execution":{
            "uid":"dfedf735-4229-44ea-a671-395ab52da052","makerOrder":{"direction":"Buy"},"takerOrder":{"direction":"Sell","quantity":"0.215"},
            "timestamp":1792401941959,"quantity":"0.0001","price":"82886","usdValue":"8.29"}}}}],"len":1,"continuationToken":"MTc5Mg+/="}"#;
        let (got, token) = executions(body).unwrap();
        assert_eq!(
            (
                got[0].trade.time,
                got[0].trade.price,
                got[0].trade.qty,
                got[0].trade.side
            ),
            (Time(1_792_401_941_959), 82886.0, 0.0001, Side::Sell)
        );
        assert_eq!(got[0].uid, 0xdfedf735_4229_44ea_a671_395ab52da052);
        assert_eq!(url_encoded(&token.unwrap()), "MTc5Mg%2B%2F%3D");
        let (got, token) = executions(r#"{"elements":[],"len":0}"#).unwrap();
        assert!(got.is_empty() && token.is_none());
    }

    #[test]
    fn the_stream_gives_trades_and_tickers() {
        let t = r#"{"feed":"trade","product_id":"PF_XBTUSD","uid":"034e0f58-384a-4142-9f4e-4ee31335ea63","side":"sell","type":"fill","seq":806363,"time":1792405609347,"qty":0.0001,"price":82374}"#;
        let got = trades_event(t).unwrap();
        assert_eq!(
            (got[0].trade.time, got[0].trade.side, got[0].trade.price),
            (Time(1_792_405_609_347), Side::Sell, 82374.0)
        );
        let s = r#"{"feed":"trade_snapshot","product_id":"PF_XBTUSD","trades":[
            {"feed":"trade","product_id":"PF_XBTUSD","uid":"45ee9737-1877-4682-bc68-e4ef818ef88b","side":"buy","type":"fill","seq":2,"time":1612269657781,"qty":440,"price":34893},
            {"feed":"trade","product_id":"PF_XBTUSD","uid":"45ee9737-1877-4682-bc68-e4ef818ef88a","side":"sell","type":"liquidation","seq":1,"time":1612269656839,"qty":9643,"price":34891}]}"#;
        let got = trades_event(s).unwrap();
        assert_eq!(
            (got[0].trade.qty, got[1].trade.side),
            (9643.0, Side::Buy),
            "oldest first"
        );
        assert!(
            trades_event(r#"{"event":"subscribed","feed":"trade","product_ids":["PF_XBTUSD"]}"#)
                .unwrap()
                .is_empty()
        );
        let k = r#"{"time":1792405895630,"product_id":"PF_ETHUSD","feed":"ticker","last":2415.7,"change":-0.27,"volumeQuote":83066055.1706}"#;
        let got = tickers_event(k).unwrap();
        assert_eq!(
            (got.symbol.as_str(), got.last, got.change),
            ("PF_ETHUSD", 2415.7, -0.27)
        );
        assert!(tickers_event(t).is_none());
    }
}
