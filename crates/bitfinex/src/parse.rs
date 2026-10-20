//! Bitfinex's answers as chart data. Its rows are arrays: a candle `[MTS, OPEN, CLOSE, HIGH, LOW, VOLUME]`, a trade
//! `[ID, MTS, AMOUNT, PRICE]` with a negative amount for a sell (the taker's side). Sizes are in the base currency.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker, kept_name};

/// A trade with its id, which grows with time: reconnections go by it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    value
        .and_then(Value::as_f64)
        .ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

fn integer(value: Option<&Value>, what: &str) -> Result<i64, Error> {
    value
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Parse(format!("{what} is not an integer")))
}

fn rows(body: &str, what: &str) -> Result<Vec<Value>, Error> {
    match json(body)? {
        Value::Array(rows) => Ok(rows),
        _ => Err(Error::Parse(format!("no {what}"))),
    }
}

/// One candle row.
pub fn candle(row: &Value) -> Result<Candle, Error> {
    let at = |i| row.get(i);
    Ok(Candle {
        time: Time(integer(at(0), "a candle's time")?),
        open: number(at(1), "an open")?,
        close: number(at(2), "a close")?,
        high: number(at(3), "a high")?,
        low: number(at(4), "a low")?,
        volume: number(at(5), "a volume")?,
    })
}

/// `/v2/candles/trade:1m:{pair}/hist`: the candles, oldest first. A minute without trades has none.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let mut out = rows(body, "candles")?
        .iter()
        .map(candle)
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|c| c.time);
    Ok(out)
}

/// One trade row.
pub fn trade(row: &Value) -> Result<IdTrade, Error> {
    let at = |i| row.get(i);
    let amount = number(at(2), "an amount")?;
    Ok(IdTrade {
        id: integer(at(0), "a trade's id")?,
        trade: Trade {
            time: Time(integer(at(1), "a trade's time")?),
            price: number(at(3), "a price")?,
            qty: amount.abs(),
            side: if amount < 0.0 { Side::Sell } else { Side::Buy },
        },
    })
}

/// `/v2/trades/{pair}/hist`: the trades, by id (within a millisecond Bitfinex lists them newest first).
pub fn trades(body: &str) -> Result<Vec<IdTrade>, Error> {
    let mut out = rows(body, "trades")?.iter().map(trade).collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// `/v2/tickers?symbols=ALL`: the exchange's pairs (`t...`; the funding ones `f...` and the test pairs are left
/// out), `[SYMBOL, BID, BID_SIZE, ASK, ASK_SIZE, DAILY_CHANGE, DAILY_CHANGE_RELATIVE, LAST_PRICE, VOLUME, HIGH,
/// LOW]`. The turnover is in the quote currency, at the last price.
pub fn tickers(body: &str, now: Time) -> Result<Vec<Ticker>, Error> {
    Ok(rows(body, "tickers")?
        .iter()
        .filter_map(|t| {
            let name = t.get(0)?.as_str()?.strip_prefix('t')?;
            if name.contains("TEST") {
                return None;
            }
            ticker(name, t.as_array()?.get(1..)?, now)
        })
        .collect())
}

/// A pair's ticker (`name` as the API writes it, without its `t`) from the fields after its name in the API's list,
/// which are a ticker channel's numbers too.
pub fn ticker(name: &str, fields: &[Value], now: Time) -> Option<Ticker> {
    let at = |i: usize| fields.get(i).and_then(Value::as_f64);
    let last = at(6)?;
    Some(Ticker {
        symbol: kept_name(name),
        last,
        change: at(5).unwrap_or(0.0) * 100.0,
        quote_volume: at(7).unwrap_or(0.0) * last,
        close_time: now,
    })
}

/// What a message of the stream says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A channel subscribed to: its id, and the pair (`tBTCUSD`) or the candles' key.
    Subscribed { channel: i64, name: String },
    /// The candles a candles channel starts with.
    Candles(i64, Vec<Candle>),
    /// A candle that changed (the forming one, or a new one).
    Candle(i64, Candle),
    /// The trades a trades channel starts with.
    Trades(i64, Vec<IdTrade>),
    /// A trade done.
    Trade(i64, IdTrade),
    /// A ticker channel's numbers.
    Ticker(i64, Vec<Value>),
}

/// A message of the stream; `None` for heartbeats, the trade updates that repeat a trade (`tu`) and the rest.
pub fn event(text: &str) -> Option<Event> {
    let value: Value = serde_json::from_str(text).ok()?;
    if let Some(kind) = value.get("event").and_then(Value::as_str) {
        if kind != "subscribed" {
            return None;
        }
        let name = value.get("key").or_else(|| value.get("symbol"))?.as_str()?;
        return Some(Event::Subscribed {
            channel: value.get("chanId")?.as_i64()?,
            name: name.to_string(),
        });
    }
    let message = value.as_array()?;
    let channel = message.first()?.as_i64()?;
    match (message.get(1)?, message.get(2)) {
        (Value::String(kind), Some(row)) if kind == "te" => Some(Event::Trade(channel, trade(row).ok()?)),
        (Value::String(_), _) => None,
        (Value::Array(rows), _) => {
            let first = rows.first()?;
            match first {
                // A list of rows: a snapshot of trades (four fields) or of candles (six).
                Value::Array(row) if row.len() == 4 => {
                    let mut trades = rows.iter().map(trade).collect::<Result<Vec<_>, _>>().ok()?;
                    trades.sort_by_key(|t| t.id);
                    Some(Event::Trades(channel, trades))
                }
                Value::Array(_) => {
                    let mut candles = rows.iter().map(candle).collect::<Result<Vec<_>, _>>().ok()?;
                    candles.sort_by_key(|c| c.time);
                    Some(Event::Candles(channel, candles))
                }
                // One row: a candle (six fields) or a ticker (ten).
                _ if rows.len() == 6 => Some(Event::Candle(channel, candle(&message[1]).ok()?)),
                _ => Some(Event::Ticker(channel, rows.clone())),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_s_candles_trades_and_tickers() {
        let got = candles("[[1364774880000,100,101,102,99,93.3],[1364774820000,93.25,93.3,93.3,93.25,0.5]]").unwrap();
        assert_eq!(got[0].time, Time(1_364_774_820_000));
        let c = got[1];
        assert_eq!(
            (c.open, c.close, c.high, c.low, c.volume),
            (100.0, 101.0, 102.0, 99.0, 93.3)
        );
        // The same millisecond, newest first.
        let got = trades("[[43413245,1500000002000,-0.5,2277.5],[43413244,1500000002000,1.6,2278]]").unwrap();
        assert_eq!(
            got.iter()
                .map(|t| (t.id, t.trade.side, t.trade.qty))
                .collect::<Vec<_>>(),
            [(43413244, Side::Buy, 1.6), (43413245, Side::Sell, 0.5)]
        );
        let body = r#"[["tBTCUSD",82000,1,82001,2,150,0.0025,82010,1000,83000,81000],
            ["tDOGE:USD",0.2,1,0.21,1,0,-0.01,0.2,500,0.22,0.19],
            ["fUSD",0.0001,0.0001,2,1,0.0001,2,1,0,0,0,0,0,0,0,0],
            ["tTESTBTC:TESTUSD",1,1,1,1,0,0,1,1,1,1]]"#;
        let got = tickers(body, Time(5)).unwrap();
        assert_eq!(
            got.iter()
                .map(|t| (t.symbol.as_str(), t.last, t.change))
                .collect::<Vec<_>>(),
            [("BTCUSD", 82010.0, 0.25), ("DOGEUSD", 0.2, -1.0)]
        );
        assert_eq!(got[0].quote_volume, 1000.0 * 82010.0);
    }

    #[test]
    fn the_stream_s_messages() {
        let subscribed = r#"{"event":"subscribed","channel":"candles","chanId":7,"key":"trade:1m:tBTCUSD"}"#;
        assert_eq!(
            event(subscribed),
            Some(Event::Subscribed {
                channel: 7,
                name: "trade:1m:tBTCUSD".into()
            })
        );
        let Some(Event::Candles(7, got)) = event("[7,[[120000,1,2,3,0.5,9],[60000,1,1,1,1,1]]]") else {
            panic!("candles")
        };
        assert_eq!(got[0].time, Time(60_000));
        let Some(Event::Candle(7, got)) = event("[7,[120000,1,2,3,0.5,9]]") else {
            panic!("a candle")
        };
        assert_eq!((got.close, got.high), (2.0, 3.0));
        let Some(Event::Trade(3, got)) = event(r#"[3,"te",[11,1500,-0.25,82000]]"#) else {
            panic!("a trade")
        };
        assert_eq!((got.id, got.trade.side, got.trade.qty), (11, Side::Sell, 0.25));
        assert_eq!(event(r#"[3,"tu",[11,1500,-0.25,82000]]"#), None);
        assert_eq!(event(r#"[3,"hb"]"#), None);
        let Some(Event::Trades(3, got)) = event("[3,[[12,1500,1,1],[11,1500,1,1]]]") else {
            panic!("trades")
        };
        assert_eq!(got[0].id, 11);
        let Some(Event::Ticker(9, fields)) = event("[9,[82000,1,82001,2,150,0.0025,82010,1000,83000,81000]]") else {
            panic!("a ticker")
        };
        let t = ticker("BTCUSD", &fields, Time(1)).unwrap();
        assert_eq!((t.symbol.as_str(), t.last, t.change), ("BTCUSD", 82010.0, 0.25));
    }
}
