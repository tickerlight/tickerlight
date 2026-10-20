//! Bitstamp's answers as chart data. Sizes are in the base currency; `type` 0 is a buy, 1 a sell (the taker's).

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker};

/// A trade with its id, which grows with time: reconnections go by it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// A number Bitstamp writes as a string or as a number.
fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

/// An integer Bitstamp writes as a string or as a number.
fn integer(value: Option<&Value>, what: &str) -> Result<i64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not an integer")))
}

/// The taker's side: `type` 0 buys, 1 sells.
fn side(value: Option<&Value>) -> Result<Side, Error> {
    match integer(value, "a side")? {
        0 => Ok(Side::Buy),
        1 => Ok(Side::Sell),
        other => Err(Error::Parse(format!("side {other}"))),
    }
}

/// `/api/v2/ohlc/{pair}/`: `{"data":{"pair":"BTC/USD","ohlc":[{"timestamp":"s","open":"...",...}]}}`, oldest
/// first.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let value = json(body)?;
    let rows = value
        .pointer("/data/ohlc")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no candles".into()))?;
    let mut out = rows
        .iter()
        .map(|c| {
            Ok(Candle {
                time: Time(integer(c.get("timestamp"), "a candle's time")? * 1000),
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

/// `/api/v2/ticker/`: every spot pair (it lists the perpetuals too), `pair` written `BTC/USD`, with `last`,
/// `percent_change_24`, the day's `volume` (base) and `vwap`: the turnover is in the quote currency.
pub fn tickers(body: &str) -> Result<Vec<Ticker>, Error> {
    let value = json(body)?;
    let all = value.as_array().ok_or_else(|| Error::Parse("no tickers".into()))?;
    Ok(all
        .iter()
        .filter(|t| t.get("market_type").and_then(Value::as_str).is_none_or(|m| m == "SPOT"))
        .filter_map(|t| {
            let volume = number(t.get("volume"), "a volume").unwrap_or(0.0);
            let vwap = number(t.get("vwap"), "a vwap").unwrap_or(0.0);
            Some(Ticker {
                symbol: t.get("pair")?.as_str()?.replace('/', ""),
                last: number(t.get("last"), "a price").ok()?,
                change: number(t.get("percent_change_24"), "a change").unwrap_or(0.0),
                quote_volume: volume * vwap,
                close_time: Time(integer(t.get("timestamp"), "a time").ok()? * 1000),
            })
        })
        .collect())
}

/// `/api/v2/transactions/{pair}/` (newest first): `date` in seconds, `tid`, `amount`, `price`, `type`.
pub fn transactions(body: &str) -> Result<Vec<IdTrade>, Error> {
    let value = json(body)?;
    let all = value.as_array().ok_or_else(|| Error::Parse("no trades".into()))?;
    let mut out = all
        .iter()
        .map(|t| {
            Ok(IdTrade {
                id: integer(t.get("tid"), "a trade's id")?,
                trade: Trade {
                    time: Time(integer(t.get("date"), "a trade's time")? * 1000),
                    price: number(t.get("price"), "a price")?,
                    qty: number(t.get("amount"), "an amount")?,
                    side: side(t.get("type"))?,
                },
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// A message of a `live_trades_{pair}` channel: the trade, with its time to the microsecond; `None` for other
/// messages (subscribed, heartbeats).
pub fn trade_event(text: &str) -> Option<(String, IdTrade)> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("event")?.as_str()? != "trade" {
        return None;
    }
    let pair = value.get("channel")?.as_str()?.strip_prefix("live_trades_")?;
    let t = value.get("data")?;
    let time = integer(t.get("microtimestamp"), "a time").ok()? / 1000;
    let trade = IdTrade {
        id: integer(t.get("id"), "an id").ok()?,
        trade: Trade {
            time: Time(time),
            price: number(t.get("price"), "a price").ok()?,
            qty: number(t.get("amount"), "an amount").ok()?,
            side: side(t.get("type")).ok()?,
        },
    };
    Some((pair.to_ascii_uppercase(), trade))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_s_candles_tickers_and_trades() {
        let body = r#"{"data": {"pair": "BTC/USD", "ohlc": [{"timestamp": "1792321320", "open": "82000.1", "high": "82010", "low": "81990", "close": "82005", "volume": "0.5"},
            {"timestamp": "1792321260", "open": "0.02947045", "high": "0.02947045", "low": "0.02947045", "close": "0.02947045", "volume": "0.00000000"}]}}"#;
        let got = candles(body).unwrap();
        assert_eq!(got[0].time, Time(1_792_321_260_000));
        assert_eq!((got[1].open, got[1].high, got[1].volume), (82000.1, 82010.0, 0.5));
        let body = r#"[{"timestamp": "1792407648", "last": "82268.90", "volume": "1169.5", "vwap": "81856.06", "percent_change_24": "0.35", "market_type": "SPOT", "pair": "BTC/USD"},
            {"timestamp": "1792407648", "last": "2416.6", "volume": "10", "vwap": "2416", "percent_change_24": "0", "market_type": "PERPETUAL", "pair": "ETH/USD-PERP"}]"#;
        let got = tickers(body).unwrap();
        assert_eq!(got.len(), 1, "spot only");
        assert_eq!(
            (got[0].symbol.as_str(), got[0].change, got[0].close_time),
            ("BTCUSD", 0.35, Time(1_792_407_648_000))
        );
        assert!((got[0].quote_volume - 1169.5 * 81856.06).abs() < 1e-6);
        let body = r#"[{"amount": "0.00006140", "date": "1792407646", "price": "82268.90", "tid": "657426452", "type": "1"},
            {"amount": "0.00006161", "date": "1792321257", "price": "81982.88", "tid": "656910133", "type": "0"}]"#;
        let got = transactions(body).unwrap();
        assert_eq!(
            (got[0].id, got[0].trade.time, got[0].trade.side, got[1].trade.side),
            (656910133, Time(1_792_321_257_000), Side::Buy, Side::Sell)
        );
    }

    #[test]
    fn the_stream_s_trades_have_their_pair_and_milliseconds() {
        let t = r#"{"data":{"id":657426691,"timestamp":"1792407683","amount":0.00335691,"amount_str":"0.00335691","price":82266.36,"price_str":"82266.36","type":0,"microtimestamp":"1792407683261000"},"channel":"live_trades_btcusd","event":"trade"}"#;
        let (pair, got) = trade_event(t).unwrap();
        assert_eq!(pair, "BTCUSD");
        assert_eq!(
            (got.id, got.trade.time, got.trade.side, got.trade.qty),
            (657426691, Time(1_792_407_683_261), Side::Buy, 0.00335691)
        );
        assert!(
            trade_event(r#"{"event":"bts:subscription_succeeded","channel":"live_trades_btcusd","data":{}}"#).is_none()
        );
    }
}
