//! Turning Binance JSON into chart data. No network here.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::Error;

fn parse_json(body: &str) -> Result<Value, Error> {
    let value: Value = serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))?;
    if let Some(e) = api_error_of(&value) {
        return Err(e);
    }
    Ok(value)
}

fn api_error_of(value: &Value) -> Option<Error> {
    let code = value.get("code")?.as_i64()?;
    let message = value.get("msg")?.as_str()?.to_string();
    Some(Error::Api { code, message })
}

/// The error inside an error body, if that is what `body` is.
pub fn api_error(body: &str) -> Option<Error> {
    api_error_of(&serde_json::from_str(body).ok()?)
}

fn number(value: &Value, what: &str) -> Result<f64, Error> {
    // Prices and sizes come as strings to keep their exact decimals.
    let n = match value {
        Value::String(s) => s.parse::<f64>().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    n.filter(|n| n.is_finite())
        .ok_or_else(|| Error::Parse(format!("bad {what}: {value}")))
}

fn integer(value: &Value, what: &str) -> Result<i64, Error> {
    value
        .as_i64()
        .ok_or_else(|| Error::Parse(format!("bad {what}: {value}")))
}

/// `/api/v3/klines` and `/fapi/v1/klines`: an array of `[openTime, open, high, low, close, volume, closeTime, ...]`.
pub fn klines(body: &str) -> Result<Vec<Candle>, Error> {
    let value = parse_json(body)?;
    let rows = value
        .as_array()
        .ok_or_else(|| Error::Parse("klines are not an array".into()))?;
    rows.iter()
        .map(|row| {
            let f = row
                .as_array()
                .filter(|f| f.len() >= 6)
                .ok_or_else(|| Error::Parse(format!("bad kline: {row}")))?;
            Ok(Candle {
                time: Time(integer(&f[0], "open time")?),
                open: number(&f[1], "open")?,
                high: number(&f[2], "high")?,
                low: number(&f[3], "low")?,
                close: number(&f[4], "close")?,
                volume: number(&f[5], "volume")?,
            })
        })
        .collect()
}

/// A kline file of the data archive: CSV rows of `open_time,open,high,low,close,volume,close_time,...`. Futures
/// files start with a header line; spot files have none and give times in microseconds since 2025.
pub fn archive_klines(csv: &str) -> Result<Vec<Candle>, Error> {
    let mut out = Vec::new();
    for (i, line) in csv.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (i == 0 && line.starts_with("open_time")) {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            return Err(Error::Parse(format!("short kline row: {line}")));
        }
        let field = |i: usize, what: &str| -> Result<f64, Error> {
            f[i].parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| Error::Parse(format!("bad {what}: {line}")))
        };
        let time: i64 = f[0]
            .parse()
            .map_err(|_| Error::Parse(format!("bad open time: {line}")))?;
        // Milliseconds this large would be past the year 5000: they are microseconds.
        let time = if time >= 100_000_000_000_000 { time / 1000 } else { time };
        out.push(Candle {
            time: Time(time),
            open: field(1, "open")?,
            high: field(2, "high")?,
            low: field(3, "low")?,
            close: field(4, "close")?,
            volume: field(5, "volume")?,
        });
    }
    Ok(out)
}

/// The CSV of an archive file of aggregated trades: `id, price, quantity, first id, last id, time, buyer is
/// maker[, best match]`. Futures files start with a header and say `true`; spot files have none, say `True`, and
/// give microseconds since 2025.
pub fn archive_agg_trades(csv: &str) -> Result<Vec<Trade>, Error> {
    let mut out = Vec::new();
    for (i, line) in csv.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (i == 0 && line.starts_with("agg_trade_id")) {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            return Err(Error::Parse(format!("short trade row: {line}")));
        }
        let number = |i: usize, what: &str| -> Result<f64, Error> {
            f[i].parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| Error::Parse(format!("bad {what}: {line}")))
        };
        let time: i64 = f[5].parse().map_err(|_| Error::Parse(format!("bad time: {line}")))?;
        let time = if time >= 100_000_000_000_000 { time / 1000 } else { time };
        let buyer_maker = match f[6] {
            "true" | "True" => true,
            "false" | "False" => false,
            _ => return Err(Error::Parse(format!("bad buyer is maker: {line}"))),
        };
        out.push(Trade {
            time: Time(time),
            price: number(1, "price")?,
            qty: number(2, "quantity")?,
            side: if buyer_maker { Side::Sell } else { Side::Buy },
        });
    }
    Ok(out)
}

/// A trade with the id Binance gave it, which is what the next page continues from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

/// `/api/v3/aggTrades` and `/fapi/v1/aggTrades`: objects with `a` id, `p` price, `q` quantity, `T` time and `m` buyer-is-maker.
/// If the buyer was the maker the seller was the aggressor.
pub fn agg_trades(body: &str) -> Result<Vec<IdTrade>, Error> {
    let value = parse_json(body)?;
    let rows = value
        .as_array()
        .ok_or_else(|| Error::Parse("trades are not an array".into()))?;
    rows.iter()
        .map(|row| {
            let field = |key: &str| {
                row.get(key)
                    .ok_or_else(|| Error::Parse(format!("trade without `{key}`: {row}")))
            };
            let buyer_maker = field("m")?
                .as_bool()
                .ok_or_else(|| Error::Parse(format!("bad `m`: {row}")))?;
            Ok(IdTrade {
                id: integer(field("a")?, "id")?,
                trade: Trade {
                    time: Time(integer(field("T")?, "time")?),
                    price: number(field("p")?, "price")?,
                    qty: number(field("q")?, "quantity")?,
                    side: if buyer_maker { Side::Sell } else { Side::Buy },
                },
            })
        })
        .collect()
}

/// What a symbol did in the last 24 hours.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticker {
    pub symbol: String,
    pub last: f64,
    /// The change over the 24 hours, in percent.
    pub change: f64,
    /// What was traded, in the quote currency: how busy the symbol is.
    pub quote_volume: f64,
    /// The end of the 24 hours: long ago for a symbol no longer traded.
    pub close_time: Time,
}

/// `/api/v3/ticker/24hr` and `/fapi/v1/ticker/24hr` without a symbol: one object per symbol.
pub fn tickers(body: &str) -> Result<Vec<Ticker>, Error> {
    let value = parse_json(body)?;
    let rows = value
        .as_array()
        .ok_or_else(|| Error::Parse("tickers are not an array".into()))?;
    rows.iter()
        .map(|row| {
            let field = |key: &str| {
                row.get(key)
                    .ok_or_else(|| Error::Parse(format!("ticker without `{key}`: {row}")))
            };
            Ok(Ticker {
                symbol: field("symbol")?
                    .as_str()
                    .ok_or_else(|| Error::Parse(format!("bad symbol: {row}")))?
                    .to_string(),
                last: number(field("lastPrice")?, "last price")?,
                change: number(field("priceChangePercent")?, "change")?,
                quote_volume: number(field("quoteVolume")?, "quote volume")?,
                close_time: Time(integer(field("closeTime")?, "close time")?),
            })
        })
        .collect()
}

/// A message of the `<symbol>@kline_<interval>` stream: the forming candle, updated about every
/// second. `None` for anything else on the socket (subscription answers and the like).
pub fn kline_event(body: &str) -> Result<Option<Candle>, Error> {
    let value = parse_json(body)?;
    if value.get("e").and_then(Value::as_str) != Some("kline") {
        return Ok(None);
    }
    let k = value
        .get("k")
        .ok_or_else(|| Error::Parse(format!("kline event without `k`: {body}")))?;
    let field = |key: &str| {
        k.get(key)
            .ok_or_else(|| Error::Parse(format!("kline without `{key}`: {body}")))
    };
    Ok(Some(Candle {
        time: Time(integer(field("t")?, "open time")?),
        open: number(field("o")?, "open")?,
        high: number(field("h")?, "high")?,
        low: number(field("l")?, "low")?,
        close: number(field("c")?, "close")?,
        volume: number(field("v")?, "volume")?,
    }))
}

/// A message of the `<symbol>@aggTrade` stream. `None` for anything else.
pub fn agg_trade_event(body: &str) -> Result<Option<Trade>, Error> {
    Ok(agg_trade_event_id(body)?.map(|t| t.trade))
}

/// [`agg_trade_event`] with the trade's id.
pub fn agg_trade_event_id(body: &str) -> Result<Option<IdTrade>, Error> {
    let value = parse_json(body)?;
    if value.get("e").and_then(Value::as_str) != Some("aggTrade") {
        return Ok(None);
    }
    let field = |key: &str| {
        value
            .get(key)
            .ok_or_else(|| Error::Parse(format!("trade without `{key}`: {body}")))
    };
    let buyer_maker = field("m")?
        .as_bool()
        .ok_or_else(|| Error::Parse(format!("bad `m`: {body}")))?;
    Ok(Some(IdTrade {
        id: integer(field("a")?, "id")?,
        trade: Trade {
            time: Time(integer(field("T")?, "time")?),
            price: number(field("p")?, "price")?,
            qty: number(field("q")?, "quantity")?,
            side: if buyer_maker { Side::Sell } else { Side::Buy },
        },
    }))
}

/// A message of the `<symbol>@miniTicker` stream: the last price and the change over the last 24 hours. `None`
/// for anything else (the answer to a subscription).
pub fn mini_ticker_event(body: &str) -> Result<Option<Ticker>, Error> {
    let value = parse_json(body)?;
    if value.get("e").and_then(Value::as_str) != Some("24hrMiniTicker") {
        return Ok(None);
    }
    let field = |key: &str| {
        value
            .get(key)
            .ok_or_else(|| Error::Parse(format!("ticker without `{key}`: {body}")))
    };
    let (last, open) = (number(field("c")?, "last price")?, number(field("o")?, "open")?);
    Ok(Some(Ticker {
        symbol: field("s")?
            .as_str()
            .ok_or_else(|| Error::Parse(format!("bad symbol: {body}")))?
            .to_string(),
        last,
        change: if open > 0.0 { (last / open - 1.0) * 100.0 } else { 0.0 },
        quote_volume: number(field("q")?, "quote volume")?,
        close_time: Time(integer(field("E")?, "event time")?),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mini_ticker_gives_the_last_price_and_the_change() {
        let body = r#"{"e":"24hrMiniTicker","E":1700000000000,"s":"BTCUSDT","c":"110.0","o":"100.0","h":"120","l":"90","v":"5","q":"550"}"#;
        let t = mini_ticker_event(body).unwrap().unwrap();
        assert_eq!((t.symbol.as_str(), t.last, t.quote_volume), ("BTCUSDT", 110.0, 550.0));
        assert!((t.change - 10.0).abs() < 1e-9);
        assert_eq!(t.close_time, Time(1_700_000_000_000));
        assert!(mini_ticker_event(r#"{"result":null,"id":1}"#).unwrap().is_none());
    }

    #[test]
    fn tickers_of_every_symbol() {
        let body = r#"[
            {"symbol":"BTCUSDT","priceChange":"-94.99","priceChangePercent":"-0.115","weightedAvgPrice":"82000","lastPrice":"82245.93","volume":"10","quoteVolume":"1234567.5","openTime":1700000000000,"closeTime":1700086400000,"count":5},
            {"symbol":"OLDUSDT","priceChangePercent":"0.000","lastPrice":"0.00000000","quoteVolume":"0","closeTime":1600000000000}
        ]"#;
        let t = tickers(body).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            t[0],
            Ticker {
                symbol: "BTCUSDT".into(),
                last: 82245.93,
                change: -0.115,
                quote_volume: 1234567.5,
                close_time: Time(1700086400000),
            }
        );
        assert!(tickers(r#"{"code":-1003,"msg":"Too many requests"}"#).is_err());
        assert!(tickers(r#"[{"symbol":"X"}]"#).is_err());
    }

    const KLINES: &str = r#"[
        [1499040000000,"0.01634790","0.80000000","0.01575800","0.01577100","148976.11427815",1499644799999,"2434.19055334",308,"1756.87402397","28.46694368","0"],
        [1499644800000,"0.01577100","0.01600000","0.01500000","0.01550000","1000.5",1500249599999,"1.0",1,"1","1","0"]
    ]"#;

    #[test]
    fn parses_klines() {
        let c = klines(KLINES).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].time, Time(1499040000000));
        assert_eq!(
            (c[0].open, c[0].high, c[0].low, c[0].close),
            (0.0163479, 0.8, 0.015758, 0.015771)
        );
        assert_eq!(c[0].volume, 148976.11427815);
        assert_eq!(c[1].volume, 1000.5);
    }

    const TRADES: &str = r#"[
        {"a":4076242628,"p":"83462.01000000","q":"0.00239000","f":6720440075,"l":6720440075,"T":1790641606797,"m":false,"M":true},
        {"a":4076242629,"p":"83461.99000000","q":"1.5","f":6720440076,"l":6720440077,"T":1790641608261,"m":true,"M":true}
    ]"#;

    #[test]
    fn parses_archive_trades_of_both_markets() {
        let spot = "677635552,109.14000000,30.90100000,2075904065,2075904075,1791590400226299,False,True\n\
                    677635553,109.15000000,4.72400000,2075904076,2075904079,1791590400618164,True,True\n";
        let t = archive_agg_trades(spot).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[0].time, t[0].price, t[0].qty, t[0].side),
            (Time(1791590400226), 109.14, 30.901, Side::Buy)
        );
        assert_eq!(t[1].side, Side::Sell, "the buyer was the maker: a seller took");
        let futures = "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n\
                       1104572108,109.07,10.41,3582873559,3582873561,1791590400005,true\n";
        let t = archive_agg_trades(futures).unwrap();
        assert_eq!((t[0].time, t[0].side), (Time(1791590400005), Side::Sell));
        assert!(archive_agg_trades("1,2,3").is_err());
        assert!(archive_agg_trades("1,109,1,1,1,1791590400005,maybe").is_err());
    }

    #[test]
    fn parses_agg_trades_with_the_aggressor_side() {
        let t = agg_trades(TRADES).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].id, 4076242628);
        assert_eq!(
            t[0].trade,
            Trade {
                time: Time(1790641606797),
                price: 83462.01,
                qty: 0.00239,
                side: Side::Buy
            }
        );
        assert_eq!(
            t[1].trade.side,
            Side::Sell,
            "buyer is the maker: the seller hit the bid"
        );
    }

    #[test]
    fn empty_answers_are_fine() {
        assert!(klines("[]").unwrap().is_empty());
        assert!(agg_trades("[]").unwrap().is_empty());
    }

    #[test]
    fn error_bodies_become_api_errors() {
        let body = r#"{"code":-1121,"msg":"Invalid symbol."}"#;
        assert!(matches!(klines(body), Err(Error::Api { code: -1121, .. })));
        assert!(matches!(agg_trades(body), Err(Error::Api { code: -1121, .. })));
        assert!(api_error(body).is_some());
        assert!(api_error("[]").is_none());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for body in [
            "",
            "nope",
            "{}",
            "[1]",
            "[[1,2]]",
            r#"[[1,"a","b","c","d","e"]]"#,
            r#"[{"a":1}]"#,
            "null",
        ] {
            assert!(klines(body).is_err(), "{body}");
            assert!(agg_trades(body).is_err(), "{body}");
        }
    }

    #[test]
    fn parses_stream_messages() {
        let kline = r#"{"e":"kline","E":1672515782136,"s":"BNBBTC","k":{"t":1672515780000,"T":1672515839999,"s":"BNBBTC","i":"1m","f":100,"L":200,"o":"0.0010","c":"0.0020","h":"0.0025","l":"0.0015","v":"1000","n":100,"x":false,"q":"1.0000","V":"500","Q":"0.500","B":"123456"}}"#;
        let c = kline_event(kline).unwrap().unwrap();
        assert_eq!(
            (c.time, c.open, c.high, c.low, c.close, c.volume),
            (Time(1672515780000), 0.001, 0.0025, 0.0015, 0.002, 1000.0)
        );

        let trade = r#"{"e":"aggTrade","E":123456789,"s":"BNBBTC","a":12345,"p":"0.001","q":"100","f":100,"l":105,"T":123456785,"m":true,"M":true}"#;
        let t = agg_trade_event(trade).unwrap().unwrap();
        assert_eq!(
            (t.time, t.price, t.qty, t.side),
            (Time(123456785), 0.001, 100.0, Side::Sell)
        );

        // Other events and the answer to a subscription are not errors, just not ours.
        assert_eq!(kline_event(trade).unwrap(), None);
        assert_eq!(agg_trade_event(kline).unwrap(), None);
        assert_eq!(kline_event(r#"{"result":null,"id":1}"#).unwrap(), None);
        assert!(kline_event("nope").is_err());
        assert!(kline_event(r#"{"e":"kline","k":{}}"#).is_err());
    }

    #[test]
    fn archive_klines_read_spot_microseconds_and_futures_headers() {
        let spot = "1792108800000000,82245.93,82245.93,82239.81,82242.46,19.01985,1792108859999999,1564269.5,692,3.8,315139.2,0\n\
                    1792108860000000,82242.46,82252.61,82212.00,82252.61,12.46182,1792108919999999,1024769.3,2831,4.3,354969.4,0\n";
        let candles = archive_klines(spot).unwrap();
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].time, Time(1_792_108_800_000));
        assert_eq!(candles[1].close, 82252.61);

        let futures = "open_time,open,high,low,close,volume,close_time,quote_volume,count,taker_buy_volume,taker_buy_quote_volume,ignore\r\n\
                       1792108800000,82195.30,82207.40,82181.50,82203.40,67.142,1792108859999,5518786.7,1354,40.8,3356831.3,0\r\n";
        let candles = archive_klines(futures).unwrap();
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].time, Time(1_792_108_800_000));
        assert_eq!(candles[0].volume, 67.142);

        assert!(archive_klines("1792108800000,1,2,3\n").is_err());
        assert!(archive_klines("1792108800000,1,2,x,4,5\n").is_err());
    }
}
