//! KuCoin's answers and archive files as chart data.
//!
//! Sizes: a futures contract trades in lots, so its trade sizes and candle volumes are turned into the base
//! currency with the [`Contract`]. Spot sizes are in the base currency already.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Ticker};

/// A trade with its sequence number, which reconnections go by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

/// What one lot of a futures contract is worth: `value` of the base currency (linear), or dollars (inverse, such
/// as `XBTUSDM`, whose lots are a dollar each). Spot is one base unit per unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contract {
    pub value: f64,
    pub inverse: bool,
}

impl Contract {
    pub const SPOT: Contract = Contract {
        value: 1.0,
        inverse: false,
    };

    /// A size in lots as base currency, at `price`.
    pub fn base(self, size: f64, price: f64) -> f64 {
        if self.inverse {
            size * self.value / price
        } else {
            size * self.value
        }
    }
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))
}

/// A number KuCoin writes as a string or as a number.
fn number(value: Option<&Value>, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number")))
}

/// An integer KuCoin writes as a string or as a number.
fn integer(value: Option<&Value>, what: &str) -> Result<i64, Error> {
    let parsed = match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not an integer")))
}

/// The `data` of an answer, or the error it is: `{"code":"200000","data":...}`, `{"code":"400100","msg":...}`.
pub fn data(body: &str) -> Result<Value, Error> {
    let mut answer = json(body)?;
    let code = answer.get("code").and_then(Value::as_str).unwrap_or_default();
    if code != "200000" {
        let message = answer.get("msg").and_then(Value::as_str).unwrap_or_default();
        return Err(Error::Api {
            code: code.to_string(),
            message: message.to_string(),
        });
    }
    Ok(answer.get_mut("data").map(Value::take).unwrap_or(Value::Null))
}

fn list(value: &Value, what: &str) -> Result<Vec<Value>, Error> {
    value
        .as_array()
        .cloned()
        .ok_or_else(|| Error::Parse(format!("{what} are not a list")))
}

/// A spot candle row of the API or the stream: `[start in seconds, open, close, high, low, volume, turnover]`.
fn spot_candle(row: &Value) -> Result<Candle, Error> {
    let field = |i: usize| number(row.get(i), "a candle field");
    Ok(Candle {
        time: Time(integer(row.get(0), "a candle's time")? * 1000),
        open: field(1)?,
        close: field(2)?,
        high: field(3)?,
        low: field(4)?,
        volume: field(5)?,
    })
}

/// A futures candle row of the API: `[start in milliseconds, open, high, low, close, lots, turnover]`.
fn futures_candle(row: &Value, contract: Contract) -> Result<Candle, Error> {
    let field = |i: usize| number(row.get(i), "a candle field");
    let close = field(4)?;
    Ok(Candle {
        time: Time(integer(row.get(0), "a candle's time")?),
        open: field(1)?,
        high: field(2)?,
        low: field(3)?,
        close,
        volume: contract.base(field(5)?, close),
    })
}

/// `/api/v1/market/candles` (spot, the newest first) or `/api/v1/kline/query` (futures, `contract` given): oldest
/// first.
pub fn candles(body: &str, futures: Option<Contract>) -> Result<Vec<Candle>, Error> {
    let rows = list(&data(body)?, "candles")?;
    let mut out = rows
        .iter()
        .map(|row| match futures {
            Some(contract) => futures_candle(row, contract),
            None => spot_candle(row),
        })
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|c| c.time);
    Ok(out)
}

/// `/api/v1/market/allTickers`: every spot symbol with its last price and day's change and turnover.
pub fn tickers(body: &str) -> Result<Vec<Ticker>, Error> {
    let data = data(body)?;
    let time = data.get("time").and_then(Value::as_i64).unwrap_or(0);
    let list = data
        .get("ticker")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("no tickers".into()))?;
    Ok(list
        .iter()
        .filter_map(|t| {
            Some(Ticker {
                symbol: t.get("symbol")?.as_str()?.to_string(),
                last: number(t.get("last"), "last").ok()?,
                change: number(t.get("changeRate"), "change").ok()? * 100.0,
                quote_volume: number(t.get("volValue"), "turnover").ok()?,
                close_time: Time(time),
            })
        })
        .collect())
}

/// What a contract object says of its lots.
fn contract_of(c: &Value) -> Result<Contract, Error> {
    Ok(Contract {
        value: number(c.get("multiplier"), "multiplier")?.abs(),
        inverse: c.get("isInverse").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// `/api/v1/contracts/active`: every open perpetual contract with its last price and day's change, and its
/// turnover in dollars (an inverse contract's volume is dollars already).
pub fn contracts(body: &str) -> Result<Vec<(Ticker, Contract)>, Error> {
    let rows = list(&data(body)?, "contracts")?;
    Ok(rows
        .iter()
        .filter(|c| {
            c.get("type").and_then(Value::as_str) == Some("FFWCSX")
                && c.get("status").and_then(Value::as_str) == Some("Open")
        })
        .filter_map(|c| {
            let contract = contract_of(c).ok()?;
            let volume = if contract.inverse {
                "volumeOf24h"
            } else {
                "turnoverOf24h"
            };
            let ticker = Ticker {
                symbol: c.get("symbol")?.as_str()?.to_string(),
                last: number(c.get("lastTradePrice"), "last").ok()?,
                change: number(c.get("priceChgPct"), "change").ok()? * 100.0,
                quote_volume: number(c.get(volume), "turnover").ok()?,
                close_time: Time(0),
            };
            Some((ticker, contract))
        })
        .collect())
}

/// `/api/v1/contracts/<symbol>`: one contract.
pub fn contract(body: &str) -> Result<Contract, Error> {
    contract_of(&data(body)?)
}

/// `/api/v1/market/histories` (spot) and `/api/v1/trade/history` (futures): the last hundred trades, oldest first.
/// Their side is the taker's.
pub fn histories(body: &str, contract: Contract) -> Result<Vec<IdTrade>, Error> {
    let mut out = list(&data(body)?, "trades")?
        .iter()
        .map(|t| trade_object(t, contract))
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// A trade of the API or the stream: its time in nanoseconds (`time` on spot, `ts` on futures).
fn trade_object(t: &Value, contract: Contract) -> Result<IdTrade, Error> {
    let side = match t.get("side").and_then(Value::as_str) {
        Some("buy") => Side::Buy,
        Some("sell") => Side::Sell,
        _ => return Err(Error::Parse("a trade without a side".into())),
    };
    let price = number(t.get("price"), "a price")?;
    Ok(IdTrade {
        id: integer(t.get("sequence"), "a trade's sequence")?,
        trade: Trade {
            time: Time(integer(t.get("time").or_else(|| t.get("ts")), "a trade's time")? / 1_000_000),
            price,
            qty: contract.base(number(t.get("size"), "a size")?, price),
            side,
        },
    })
}

/// The topic and `data` of a `message` of the stream; `None` for other messages.
fn message(value: &Value) -> Option<(&str, &Value)> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    Some((value.get("topic")?.as_str()?, value.get("data")?))
}

/// A `/market/match` (spot) or `/contractMarket/execution` (futures) message: one trade.
pub fn match_event(text: &str, contract: Contract) -> Result<Option<IdTrade>, Error> {
    let value = json(text)?;
    match message(&value) {
        Some((topic, data))
            if topic.starts_with("/market/match:") || topic.starts_with("/contractMarket/execution:") =>
        {
            trade_object(data, contract).map(Some)
        }
        _ => Ok(None),
    }
}

/// A `/market/candles` (spot) or `/contractMarket/limitCandle` (futures) message: the candle as it is now. The
/// futures row is `[start in seconds, open, close, high, low, turnover, lots]`.
pub fn candle_event(text: &str, contract: Contract) -> Result<Option<Candle>, Error> {
    let value = json(text)?;
    let Some((topic, data)) = message(&value) else {
        return Ok(None);
    };
    let row = data.get("candles").unwrap_or(&Value::Null);
    if topic.starts_with("/market/candles:") {
        return spot_candle(row).map(Some);
    }
    if !topic.starts_with("/contractMarket/limitCandle:") {
        return Ok(None);
    }
    let mut candle = spot_candle(row)?;
    candle.volume = contract.base(number(row.get(6), "lots")?, candle.close);
    Ok(Some(candle))
}

/// A `/market/snapshot` (spot) or `/contractMarket/snapshot` (futures) message: a symbol's last price, change and
/// turnover (an inverse contract's: its volume, in dollars).
pub fn snapshot_event(text: &str) -> Result<Option<Ticker>, Error> {
    let value = json(text)?;
    let Some((topic, data)) = message(&value) else {
        return Ok(None);
    };
    let symbol = |data: &Value| {
        data.get("symbol")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Parse("a snapshot without a symbol".into()))
    };
    if topic.starts_with("/market/snapshot:") {
        let data = data.get("data").unwrap_or(data);
        return Ok(Some(Ticker {
            symbol: symbol(data)?,
            last: number(data.get("lastTradedPrice"), "last")?,
            change: number(data.get("changeRate"), "change")? * 100.0,
            quote_volume: number(data.get("volValue"), "turnover")?,
            close_time: Time(integer(data.get("datetime"), "time").unwrap_or(0)),
        }));
    }
    if !topic.starts_with("/contractMarket/snapshot:") {
        return Ok(None);
    }
    let symbol = symbol(data)?;
    let volume = if symbol.ends_with("USDM") { "volume" } else { "turnover" };
    Ok(Some(Ticker {
        last: number(data.get("lastPrice"), "last")?,
        change: number(data.get("priceChgPct"), "change")? * 100.0,
        quote_volume: number(data.get(volume), "turnover")?,
        close_time: Time(integer(data.get("ts"), "time").unwrap_or(0) / 1_000_000),
        symbol,
    }))
}

/// `/api/v1/bullet-public`: where to connect, the token to connect with and how often to ping.
pub fn bullet(body: &str) -> Result<(String, String, u64), Error> {
    let data = data(body)?;
    let token = data.get("token").and_then(Value::as_str);
    let server = data.get("instanceServers").and_then(|s| s.get(0));
    let endpoint = server.and_then(|s| s.get("endpoint")).and_then(Value::as_str);
    let ping = server.and_then(|s| s.get("pingInterval")).and_then(Value::as_u64);
    match (endpoint, token, ping) {
        (Some(endpoint), Some(token), Some(ping)) => Ok((endpoint.to_string(), token.to_string(), ping)),
        _ => Err(Error::Parse("no server to connect to".into())),
    }
}

/// The rows of a CSV file after its header.
fn rows(csv: &str) -> impl Iterator<Item = Vec<&str>> {
    csv.lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().split(',').collect())
}

fn field<T: std::str::FromStr>(row: &[&str], i: usize, what: &str) -> Result<T, Error> {
    row.get(i)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Parse(format!("{what} in {row:?}")))
}

/// A day's trades of the archive: `trade_id`, `trade_time` (milliseconds), price, size, the taker's side (upper or
/// lower case, both in one file).
pub fn archive_trades(csv: &str, contract: Contract) -> Result<Vec<Trade>, Error> {
    let mut out = rows(csv)
        .map(|r| {
            let side = match r.get(4).map(|s| s.trim()) {
                Some("BUY") | Some("buy") => Side::Buy,
                Some("SELL") | Some("sell") => Side::Sell,
                _ => return Err(Error::Parse(format!("side in {r:?}"))),
            };
            let price = field(&r, 2, "price")?;
            Ok(Trade {
                time: Time(field(&r, 1, "time")?),
                price,
                qty: contract.base(field(&r, 3, "size")?, price),
                side,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    out.sort_by_key(|t| t.time);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: Contract = Contract {
        value: 0.001,
        inverse: false,
    };

    #[test]
    fn archive_trades_read_with_the_taker_s_side_in_base_currency() {
        let csv = "trade_id,trade_time,price,size,side\n\
            24852485830492161,1792195200828,81605.6,0.0158,BUY\n\
            24852485928927232,1792195201409,81605.5,0.0001,SELL\n";
        let got = archive_trades(csv, Contract::SPOT).unwrap();
        assert_eq!(
            (got[0].time, got[0].side, got[0].qty),
            (Time(1_792_195_200_828), Side::Buy, 0.0158)
        );
        assert_eq!(got[1].side, Side::Sell);
        let csv = "trade_id,trade_time,price,size,side\n1835394231585,1792195200367,2000,94,sell\n";
        let got = archive_trades(csv, LINEAR).unwrap();
        assert_eq!((got[0].side, got[0].qty), (Side::Sell, 0.094));
        let inverse = Contract {
            value: 1.0,
            inverse: true,
        };
        assert_eq!(inverse.base(500.0, 50_000.0), 0.01, "500 one-dollar lots at 50000");
    }

    #[test]
    fn the_api_s_answers_and_errors() {
        let body = r#"{"code":"200000","data":[["1792392660","82788.9","82785.7","82789","82785.7","0.23","19453.9"],
            ["1792392600","82794.3","82788.9","82794.3","82788.9","1.40","116214.8"]]}"#;
        let got = candles(body, None).unwrap();
        assert_eq!(got[0].time, Time(1_792_392_600_000));
        assert_eq!(
            (got[1].open, got[1].close, got[1].high, got[1].low),
            (82788.9, 82785.7, 82789.0, 82785.7)
        );
        let body = r#"{"code":"200000","data":[[1792307040000,81717.0,81733.4,81716.9,81733.4,356,29093.2829]]}"#;
        let got = candles(body, Some(LINEAR)).unwrap();
        assert_eq!(
            (got[0].time, got[0].high, got[0].low, got[0].close),
            (Time(1_792_307_040_000), 81733.4, 81716.9, 81733.4)
        );
        assert!((got[0].volume - 0.356).abs() < 1e-12);
        let err = candles(r#"{"msg":"Unsupported trading pair.","code":"400100"}"#, None).unwrap_err();
        assert!(err.is_unknown());
        let err = contract(r#"{"msg":"The contract information you requested does not exist.","code":"404000"}"#);
        assert!(err.unwrap_err().is_unknown());
        let body = r#"{"code":"200000","data":{"time":5,"ticker":[{"symbol":"BTC-USDT","last":"82000","changeRate":"-0.011","volValue":"1000"}]}}"#;
        let got = tickers(body).unwrap();
        assert_eq!(
            (got[0].symbol.as_str(), got[0].last, got[0].quote_volume),
            ("BTC-USDT", 82000.0, 1000.0)
        );
        assert!((got[0].change + 1.1).abs() < 1e-9);
    }

    #[test]
    fn contracts_give_perpetuals_with_their_dollar_turnover() {
        let body = r#"{"code":"200000","data":[
            {"symbol":"XBTUSDTM","type":"FFWCSX","status":"Open","multiplier":0.001,"isInverse":false,
             "lastTradePrice":82855.4,"priceChgPct":0.0137,"turnoverOf24h":278275877.06,"volumeOf24h":3404.262},
            {"symbol":"XBTUSDM","type":"FFWCSX","status":"Open","multiplier":-1.0,"isInverse":true,
             "lastTradePrice":82731.5,"priceChgPct":0.0134,"turnoverOf24h":35.55,"volumeOf24h":2906458.0},
            {"symbol":"XBTMZ26","type":"FFICSX","status":"Open","multiplier":1.0,"isInverse":true,
             "lastTradePrice":1,"priceChgPct":0,"turnoverOf24h":1,"volumeOf24h":1}]}"#;
        let got = contracts(body).unwrap();
        assert_eq!(got.len(), 2, "dated futures are left out");
        assert_eq!((got[0].0.quote_volume, got[0].1), (278275877.06, LINEAR));
        assert!((got[0].0.change - 1.37).abs() < 1e-9);
        assert_eq!(
            (got[1].0.quote_volume, got[1].1),
            (
                2906458.0,
                Contract {
                    value: 1.0,
                    inverse: true
                }
            )
        );
    }

    #[test]
    fn the_stream_gives_trades_candles_and_snapshots() {
        let m = r#"{"type":"message","topic":"/market/match:BTC-USDT","subject":"trade.l3match","data":{"sequence":"7",
            "symbol":"BTC-USDT","side":"sell","price":"10","size":"2","tradeId":"7","time":"1792392557946000000"}}"#;
        let got = match_event(m, Contract::SPOT).unwrap().unwrap();
        assert_eq!(
            (got.id, got.trade.time, got.trade.side, got.trade.qty),
            (7, Time(1_792_392_557_946), Side::Sell, 2.0)
        );
        let m = r#"{"topic":"/contractMarket/execution:XBTUSDTM","type":"message","subject":"match","data":{"symbol":"XBTUSDTM",
            "sequence":1948166604258,"side":"buy","size":60,"price":"82846.7","ts":1792393590560000000}}"#;
        let got = match_event(m, LINEAR).unwrap().unwrap();
        assert_eq!(
            (got.id, got.trade.time, got.trade.side),
            (1948166604258, Time(1_792_393_590_560), Side::Buy)
        );
        assert!((got.trade.qty - 0.06).abs() < 1e-12);
        let c = r#"{"type":"message","topic":"/market/candles:BTC-USDT_1min","subject":"trade.candles.update",
            "data":{"symbol":"BTC-USDT","candles":["1792392600","1","2","3","0.5","4","5"],"time":1}}"#;
        let got = candle_event(c, Contract::SPOT).unwrap().unwrap();
        assert_eq!(
            (got.time, got.close, got.high, got.low, got.volume),
            (Time(1_792_392_600_000), 2.0, 3.0, 0.5, 4.0)
        );
        let c = r#"{"topic":"/contractMarket/limitCandle:XBTUSDTM_1min","type":"message","subject":"candle.stick",
            "data":{"symbol":"XBTUSDTM","candles":["1792393560","82843.8","82846.7","82846.7","82843.8","9195.8387","111"]}}"#;
        let got = candle_event(c, LINEAR).unwrap().unwrap();
        assert_eq!((got.time, got.close), (Time(1_792_393_560_000), 82846.7));
        assert!((got.volume - 0.111).abs() < 1e-12, "lots, not turnover");
        let s = r#"{"type":"message","topic":"/market/snapshot:ETH-USDT","subject":"trade.snapshot","data":{"sequence":"1",
            "data":{"symbol":"ETH-USDT","lastTradedPrice":110,"changeRate":0.1,"volValue":2000,"datetime":9}}}"#;
        let got = snapshot_event(s).unwrap().unwrap();
        assert_eq!(
            (got.symbol.as_str(), got.last, got.quote_volume),
            ("ETH-USDT", 110.0, 2000.0)
        );
        assert!((got.change - 10.0).abs() < 1e-9);
        let s = r#"{"topic":"/contractMarket/snapshot:XBTUSDTM","type":"message","subject":"snapshot.24h","data":{
            "lastPrice":82843.8,"priceChgPct":0.0134,"symbol":"XBTUSDTM","ts":1792393570004076047,"turnover":139132044.4875,"volume":1702.055}}"#;
        let got = snapshot_event(s).unwrap().unwrap();
        assert_eq!(
            (got.symbol.as_str(), got.last, got.quote_volume, got.close_time),
            ("XBTUSDTM", 82843.8, 139132044.4875, Time(1_792_393_570_004))
        );
        assert!(
            match_event(r#"{"type":"welcome","id":"1"}"#, Contract::SPOT)
                .unwrap()
                .is_none()
        );
    }
}
