//! Deribit's answers (JSON-RPC: the data under `result`, or an `error`) as chart data. Sizes are in the base coin:
//! the inverse contracts' trade amounts, in dollars, are divided by the price.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker, is_inverse};

/// A trade with its sequence number in its instrument, which grows with time: reconnections go by it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// The `result` of an answer; an error about the instrument is an unknown one.
fn result(body: &str) -> Result<Value, Error> {
    let mut value = json(body)?;
    if let Some(error) = value.get("error") {
        let param = error.pointer("/data/param").and_then(Value::as_str);
        return Err(match param {
            Some("instrument_name") => Error::Unknown,
            _ => Error::Http(error.to_string()),
        });
    }
    value
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| Error::Parse("no result".into()))
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

/// `public/get_tradingview_chart_data`: columns `ticks`, `open`, `high`, `low`, `close`, `volume` (base coin),
/// oldest first; `status` `no_data` when there are none. A quiet minute is flat with no volume.
pub fn candles(body: &str) -> Result<Vec<Candle>, Error> {
    let result = result(body)?;
    let column = |name: &str| result.get(name).and_then(Value::as_array).cloned().unwrap_or_default();
    let (ticks, open, high, low, close, volume) = (
        column("ticks"),
        column("open"),
        column("high"),
        column("low"),
        column("close"),
        column("volume"),
    );
    (0..ticks.len())
        .map(|i| {
            Ok(Candle {
                time: Time(integer(ticks.get(i), "a candle's time")?),
                open: number(open.get(i), "an open")?,
                high: number(high.get(i), "a high")?,
                low: number(low.get(i), "a low")?,
                close: number(close.get(i), "a close")?,
                volume: number(volume.get(i), "a volume")?,
            })
        })
        .collect()
}

/// One trade: `trade_seq`, `timestamp`, `price`, `amount` (dollars for the inverse contracts, else the base
/// coin), `direction` (the taker's).
pub fn trade(t: &Value) -> Result<IdTrade, Error> {
    let name = t.get("instrument_name").and_then(Value::as_str).unwrap_or("");
    let price = number(t.get("price"), "a price")?;
    let amount = number(t.get("amount"), "an amount")?;
    let side = match t.get("direction").and_then(Value::as_str) {
        Some("buy") => Side::Buy,
        Some("sell") => Side::Sell,
        other => return Err(Error::Parse(format!("direction {other:?}"))),
    };
    Ok(IdTrade {
        id: integer(t.get("trade_seq"), "a trade's sequence")?,
        trade: Trade {
            time: Time(integer(t.get("timestamp"), "a trade's time")?),
            price,
            qty: if is_inverse(name) { amount / price } else { amount },
            side,
        },
    })
}

/// `public/get_last_trades_by_instrument_and_time`: the trades by sequence, and whether more follow.
pub fn trades(body: &str) -> Result<(Vec<IdTrade>, bool), Error> {
    let result = result(body)?;
    let mut out = result
        .get("trades")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no trades".into()))?
        .iter()
        .map(trade)
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|t| t.id);
    let more = result.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    Ok((out, more))
}

/// `public/get_instrument`: when it was listed.
pub fn created(body: &str) -> Result<Time, Error> {
    Ok(Time(integer(
        result(body)?.get("creation_timestamp"),
        "a listing time",
    )?))
}

/// `public/get_book_summary_by_currency`: the perpetuals (the dated futures are left out), with `last`,
/// `price_change` (percent) and `volume_usd`.
pub fn summaries(body: &str) -> Result<Vec<Ticker>, Error> {
    let result = result(body)?;
    let all = result.as_array().ok_or_else(|| Error::Parse("no summaries".into()))?;
    Ok(all
        .iter()
        .filter_map(|s| {
            let symbol = s.get("instrument_name")?.as_str()?;
            if !symbol.ends_with("-PERPETUAL") {
                return None;
            }
            Some(Ticker {
                symbol: symbol.to_string(),
                last: s.get("last")?.as_f64()?,
                change: s.get("price_change").and_then(Value::as_f64).unwrap_or(0.0),
                quote_volume: s.get("volume_usd").and_then(Value::as_f64).unwrap_or(0.0),
                close_time: Time(s.get("creation_timestamp").and_then(Value::as_i64).unwrap_or(0)),
            })
        })
        .collect())
}

/// What a message of the stream says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The server asks for an answer to keep the connection.
    TestRequest,
    /// The forming 1-minute candle of an instrument (`chart.trades.<name>.1`).
    Candle(Candle),
    /// Trades done (`trades.<name>.100ms`).
    Trades(Vec<IdTrade>),
    /// An instrument's ticker (`ticker.<name>.100ms`).
    Ticker(Ticker),
}

/// A message of the stream; `None` for answers to requests and the rest.
pub fn event(text: &str) -> Option<Event> {
    let value: Value = serde_json::from_str(text).ok()?;
    let params = value.get("params")?;
    match value.get("method")?.as_str()? {
        "heartbeat" => (params.get("type")?.as_str()? == "test_request").then_some(Event::TestRequest),
        "subscription" => {
            let channel = params.get("channel")?.as_str()?;
            let data = params.get("data")?;
            if channel.starts_with("chart.trades.") {
                Some(Event::Candle(Candle {
                    time: Time(data.get("tick")?.as_i64()?),
                    open: data.get("open")?.as_f64()?,
                    high: data.get("high")?.as_f64()?,
                    low: data.get("low")?.as_f64()?,
                    close: data.get("close")?.as_f64()?,
                    volume: data.get("volume")?.as_f64()?,
                }))
            } else if channel.starts_with("trades.") {
                let mut trades = data.as_array()?.iter().map(trade).collect::<Result<Vec<_>, _>>().ok()?;
                trades.sort_by_key(|t| t.id);
                Some(Event::Trades(trades))
            } else if channel.starts_with("ticker.") {
                let stats = data.get("stats");
                Some(Event::Ticker(Ticker {
                    symbol: data.get("instrument_name")?.as_str()?.to_string(),
                    last: data.get("last_price")?.as_f64()?,
                    change: stats
                        .and_then(|s| s.get("price_change"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    quote_volume: stats
                        .and_then(|s| s.get("volume_usd"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    close_time: Time(data.get("timestamp")?.as_i64()?),
                }))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_s_candles_trades_and_errors() {
        let body = r#"{"jsonrpc":"2.0","result":{"volume":[3.5,0],"ticks":[1599999960000,1600000020000],"status":"ok",
            "open":[10343.0,10346.5],"low":[10343.0,10346.5],"high":[10345.5,10346.5],"cost":[36310.0,0],"close":[10345.5,10346.5]}}"#;
        let got = candles(body).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].time, got[0].high, got[0].volume),
            (Time(1_599_999_960_000), 10345.5, 3.5)
        );
        assert!(candles(r#"{"result":{"status":"no_data"}}"#).unwrap().is_empty());
        // An inverse contract's amount is in dollars; a linear one's in the coin. Within a millisecond the list is
        // not in sequence.
        let body = r#"{"result":{"trades":[
            {"trade_seq":8,"timestamp":5,"price":100.0,"instrument_name":"SOL_USDC-PERPETUAL","direction":"sell","amount":9.6},
            {"trade_seq":7,"timestamp":5,"price":80000.0,"instrument_name":"BTC-PERPETUAL","direction":"buy","amount":4000.0}],
            "has_more":true}}"#;
        let (got, more) = trades(body).unwrap();
        assert!(more);
        assert_eq!(
            got.iter()
                .map(|t| (t.id, t.trade.side, t.trade.qty))
                .collect::<Vec<_>>(),
            [(7, Side::Buy, 0.05), (8, Side::Sell, 9.6)]
        );
        let unknown = r#"{"error":{"message":"Invalid params","data":{"reason":"instrument not found","param":"instrument_name"},"code":-32602}}"#;
        assert!(candles(unknown).unwrap_err().is_unknown());
        let body = r#"{"result":[{"instrument_name":"BTC-PERPETUAL","last":82353.0,"price_change":0.428,"volume_usd":1.3e9,"creation_timestamp":17},
            {"instrument_name":"BTC-26DEC26","last":83000.0,"price_change":0.1,"volume_usd":1e6,"creation_timestamp":17}]}"#;
        let got = summaries(body).unwrap();
        assert_eq!(
            got.iter().map(|t| (t.symbol.as_str(), t.change)).collect::<Vec<_>>(),
            [("BTC-PERPETUAL", 0.428)]
        );
    }

    #[test]
    fn the_stream_s_messages() {
        let candle = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"close":82305.0,"high":82305.0,"low":82293.0,"open":82298.0,"tick":1792411500000,"cost":69440.0,"volume":0.84}}}"#;
        let Some(Event::Candle(c)) = event(candle) else {
            panic!("a candle")
        };
        assert_eq!((c.time, c.low, c.volume), (Time(1_792_411_500_000), 82293.0, 0.84));
        let trades = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"timestamp":1792411513834,"price":82000.0,"direction":"sell","instrument_name":"BTC-PERPETUAL","trade_seq":307330654,"amount":41000.0}]}}"#;
        let Some(Event::Trades(t)) = event(trades) else {
            panic!("trades")
        };
        assert_eq!(
            (t[0].id, t[0].trade.side, t[0].trade.qty),
            (307_330_654, Side::Sell, 0.5)
        );
        let ticker = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-PERPETUAL.100ms","data":{"timestamp":9,"stats":{"price_change":0.386,"volume_usd":1305232320.0},"instrument_name":"BTC-PERPETUAL","last_price":82305.0}}}"#;
        let Some(Event::Ticker(t)) = event(ticker) else {
            panic!("a ticker")
        };
        assert_eq!((t.symbol.as_str(), t.last, t.change), ("BTC-PERPETUAL", 82305.0, 0.386));
        let test = r#"{"params":{"type":"test_request"},"method":"heartbeat","jsonrpc":"2.0"}"#;
        assert_eq!(event(test), Some(Event::TestRequest));
        assert_eq!(event(r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#), None);
    }
}
