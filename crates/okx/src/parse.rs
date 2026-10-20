//! What OKX sends, read into chart data: the archive's CSV files, the REST API's JSON and the WebSocket's
//! messages. Separate from the network so it is tested without one.
//!
//! Sizes: a perpetual swap trades in contracts, so its trade sizes are turned into the base currency with the
//! [`Contract`], and its candles take the volume in the base currency (`vol_ccy`, `volCcy`). Spot sizes are in the
//! base currency already.

use serde_json::Value;
use tickerlight_core::{Candle, Side, Time, Trade};

use crate::{Error, Market, Ticker};

/// What one contract of a swap is worth: `value` of the base currency (linear), or of the quote currency
/// (inverse, such as `BTC-USD-SWAP`, whose contracts are dollars). Spot is one base unit per unit.
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

    /// A size in contracts as base currency, at `price`.
    pub fn base(self, size: f64, price: f64) -> f64 {
        if self.inverse {
            size * self.value / price
        } else {
            size * self.value
        }
    }
}

/// A trade with the id OKX gave it: ids grow, so they tell what was had already.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdTrade {
    pub id: i64,
    pub trade: Trade,
}

fn number(value: &Value, what: &str) -> Result<f64, Error> {
    let parsed = match value {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Parse(format!("{what} is not a number: {value}")))
}

fn side(text: &str) -> Result<Side, Error> {
    match text.trim() {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        other => Err(Error::Parse(format!("unknown side {other:?}"))),
    }
}

/// The `data` of a REST answer, or the error it carries (`code` not `"0"`).
pub fn data(body: &str) -> Result<Vec<Value>, Error> {
    let value: Value = serde_json::from_str(body).map_err(|e| Error::Parse(format!("{e}: {body:.200}")))?;
    let code = value.get("code").and_then(Value::as_str).unwrap_or("0");
    if code != "0" {
        let message = value.get("msg").and_then(Value::as_str).unwrap_or_default();
        return Err(Error::Api {
            code: code.parse().unwrap_or(-1),
            message: message.to_string(),
        });
    }
    match value.get("data") {
        Some(Value::Array(rows)) => Ok(rows.clone()),
        _ => Err(Error::Parse(format!("no data: {body:.200}"))),
    }
}

/// A candle row of the API or the WebSocket: `[ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]`.
fn candle_row(row: &Value, market: Market) -> Result<Candle, Error> {
    let field = |i: usize, what: &str| number(row.get(i).unwrap_or(&Value::Null), what);
    Ok(Candle {
        time: Time(field(0, "time")? as i64),
        open: field(1, "open")?,
        high: field(2, "high")?,
        low: field(3, "low")?,
        close: field(4, "close")?,
        volume: match market {
            Market::Spot => field(5, "volume")?,
            Market::Swap => field(6, "volume")?,
        },
    })
}

/// `/api/v5/market/candles` and `history-candles`: rows newest first. Returned oldest first.
pub fn candles(body: &str, market: Market) -> Result<Vec<Candle>, Error> {
    let mut out = data(body)?
        .iter()
        .map(|row| candle_row(row, market))
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|c| c.time);
    out.dedup_by_key(|c| c.time);
    Ok(out)
}

/// A trade object of the API or the WebSocket: `tradeId`, `px`, `sz`, `side` (the taker's), `ts`.
fn trade_object(object: &Value, contract: Contract) -> Result<IdTrade, Error> {
    let text = |key: &str| object.get(key).unwrap_or(&Value::Null);
    let price = number(text("px"), "price")?;
    Ok(IdTrade {
        id: number(text("tradeId"), "trade id")? as i64,
        trade: Trade {
            time: Time(number(text("ts"), "time")? as i64),
            price,
            qty: contract.base(number(text("sz"), "size")?, price),
            side: side(text("side").as_str().unwrap_or_default())?,
        },
    })
}

/// `/api/v5/market/trades` and `history-trades`: newest first. Returned oldest first.
pub fn trades(body: &str, contract: Contract) -> Result<Vec<IdTrade>, Error> {
    let mut out = data(body)?
        .iter()
        .map(|o| trade_object(o, contract))
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// A ticker object of the API or the WebSocket.
fn ticker_object(object: &Value, market: Market) -> Result<Ticker, Error> {
    let text = |key: &str| object.get(key).unwrap_or(&Value::Null);
    let inst = text("instId").as_str().unwrap_or_default();
    let last = number(text("last"), "last")?;
    let open = number(text("open24h"), "open")?;
    let volume = number(text("volCcy24h"), "volume")?;
    Ok(Ticker {
        symbol: match market {
            Market::Spot => inst.to_string(),
            Market::Swap => inst.strip_suffix("-SWAP").unwrap_or(inst).to_string(),
        },
        last,
        change: if open > 0.0 { (last / open - 1.0) * 100.0 } else { 0.0 },
        // A swap's volume is in the base currency (or, inverse, in it too): its worth in the quote is at the price.
        quote_volume: match market {
            Market::Spot => volume,
            Market::Swap => volume * last,
        },
        close_time: Time(number(text("ts"), "time")? as i64),
    })
}

/// `/api/v5/market/tickers`: every instrument of a type. Those of another market (an expiring future among the
/// swaps) are left out.
pub fn tickers(body: &str, market: Market) -> Result<Vec<Ticker>, Error> {
    let rows = data(body)?;
    let mine = rows.iter().filter(|o| {
        let inst = o.get("instId").and_then(Value::as_str).unwrap_or_default();
        (market == Market::Swap) == inst.ends_with("-SWAP")
    });
    mine.map(|o| ticker_object(o, market)).collect()
}

/// `/api/v5/public/instruments` of one swap: its contract.
pub fn contract(body: &str) -> Result<Contract, Error> {
    let rows = data(body)?;
    let row = rows.first().ok_or_else(|| Error::Parse("no such instrument".into()))?;
    let text = |key: &str| row.get(key).unwrap_or(&Value::Null);
    let value = number(text("ctVal"), "contract value")?;
    let multiplier = number(text("ctMult"), "contract multiplier").unwrap_or(1.0);
    Ok(Contract {
        value: value * multiplier,
        inverse: text("ctType").as_str() == Some("inverse"),
    })
}

/// A WebSocket message: the rows of its `data`, or nothing for the answers to subscribing and pings.
fn event_rows(text: &str) -> Result<Vec<Value>, Error> {
    if text == "pong" {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(text).map_err(|e| Error::Parse(e.to_string()))?;
    if let Some(event) = value.get("event").and_then(Value::as_str)
        && event == "error"
    {
        return Err(Error::Parse(format!("stream error: {text:.200}")));
    }
    match value.get("data") {
        Some(Value::Array(rows)) => Ok(rows.clone()),
        _ => Ok(Vec::new()),
    }
}

/// The `candle1m` channel: the forming candle (and the one just closed).
pub fn candle_event(text: &str, market: Market) -> Result<Vec<Candle>, Error> {
    event_rows(text)?.iter().map(|row| candle_row(row, market)).collect()
}

/// The `trades-all` channel: every trade, one or more a message.
pub fn trades_event(text: &str, contract: Contract) -> Result<Vec<IdTrade>, Error> {
    event_rows(text)?.iter().map(|o| trade_object(o, contract)).collect()
}

/// The `tickers` channel.
pub fn tickers_event(text: &str, market: Market) -> Result<Vec<Ticker>, Error> {
    event_rows(text)?.iter().map(|o| ticker_object(o, market)).collect()
}

/// The columns of an archive file by their names. A name may go on with `/` and the same in Chinese.
fn header(line: &str) -> Vec<String> {
    line.split(',')
        .map(|name| name.split('/').next().unwrap_or_default().trim().to_ascii_lowercase())
        .collect()
}

fn column(names: &[String], name: &str) -> Result<usize, Error> {
    names
        .iter()
        .position(|n| n == name)
        .ok_or_else(|| Error::Parse(format!("no {name} column in {names:?}")))
}

/// An archive file of candles: `instrument_name,open,high,low,close,vol,vol_ccy,vol_quote,open_time,confirm`.
/// Oldest first.
pub fn archive_candles(csv: &str, market: Market) -> Result<Vec<Candle>, Error> {
    let mut lines = csv.lines();
    let names = header(lines.next().unwrap_or_default());
    let at = |name| column(&names, name);
    let (open, high, low, close, time) = (at("open")?, at("high")?, at("low")?, at("close")?, at("open_time")?);
    let volume = match market {
        Market::Spot => at("vol")?,
        Market::Swap => at("vol_ccy")?,
    };
    let mut out = Vec::new();
    for (i, line) in lines.enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split(',').collect();
        let field = |c: usize| -> Result<f64, Error> {
            fields
                .get(c)
                .and_then(|f| f.trim().parse().ok())
                .ok_or_else(|| Error::Parse(format!("line {}: {line:.100}", i + 2)))
        };
        out.push(Candle {
            time: Time(field(time)? as i64),
            open: field(open)?,
            high: field(high)?,
            low: field(low)?,
            close: field(close)?,
            volume: field(volume)?,
        });
    }
    out.sort_by_key(|c| c.time);
    out.dedup_by_key(|c| c.time);
    Ok(out)
}

/// An archive file of trades: `trade_id`, `side`, `size`, `price`, `created_time` in some order (and maybe the
/// instrument). Not always in time order: returned oldest first.
pub fn archive_trades(csv: &str, contract: Contract) -> Result<Vec<Trade>, Error> {
    let mut lines = csv.lines();
    let names = header(lines.next().unwrap_or_default());
    let at = |name| column(&names, name);
    let (id, side_at, size, price, time) = (
        at("trade_id")?,
        at("side")?,
        at("size")?,
        at("price")?,
        at("created_time")?,
    );
    let mut out = Vec::new();
    for (i, line) in lines.enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split(',').collect();
        let bad = || Error::Parse(format!("line {}: {line:.100}", i + 2));
        let field =
            |c: usize| -> Result<f64, Error> { fields.get(c).and_then(|f| f.trim().parse().ok()).ok_or_else(bad) };
        let price = field(price)?;
        out.push(IdTrade {
            id: field(id)? as i64,
            trade: Trade {
                time: Time(field(time)? as i64),
                price,
                qty: contract.base(field(size)?, price),
                side: side(fields.get(side_at).ok_or_else(bad)?)?,
            },
        });
    }
    out.sort_by_key(|t| (t.trade.time, t.id));
    Ok(out.into_iter().map(|t| t.trade).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_candles_take_the_volume_in_the_base_currency() {
        let csv = "instrument_name,open,high,low,close,vol,vol_ccy,vol_quote,open_time,confirm\n\
                   BTC-USDT-SWAP,95.5,96,95,95.2,18109,181.09,17278807.6,1735660860000,1\n\
                   BTC-USDT-SWAP,95.4,95.5,95.3,95.5,4089,40.89,3902421.9,1735660800000,1\n";
        let swap = archive_candles(csv, Market::Swap).unwrap();
        assert_eq!(swap.len(), 2);
        assert_eq!(
            (swap[0].time, swap[0].open, swap[0].volume),
            (Time(1735660800000), 95.4, 40.89)
        );
        let spot = archive_candles(csv, Market::Spot).unwrap();
        assert_eq!(spot[0].volume, 4089.0, "spot's vol is in the base currency already");
    }

    #[test]
    fn archive_trades_are_read_by_their_names_sorted_and_in_the_base_currency() {
        // The older files: other columns, a header in two languages (not UTF-8), rows out of order.
        let csv = "trade_id/\u{fffd}\u{fffd}id,side/\u{fffd},size/\u{fffd},price/\u{fffd},created_time/\u{fffd}\n\
                   638128868,sell,2,95184.6,1735662017483\n\
                   638124370,buy,3,95185.0,1735661220169\n";
        let swap = Contract {
            value: 0.01,
            inverse: false,
        };
        let trades = archive_trades(csv, swap).unwrap();
        assert_eq!(trades[0].time, Time(1735661220169));
        assert_eq!((trades[0].side, trades[1].side), (Side::Buy, Side::Sell));
        assert!((trades[0].qty - 0.03).abs() < 1e-12);
        // The newer files.
        let csv = "instrument_name,trade_id,side,price,size,created_time,source\n\
                   BTC-USDT,3000926349,buy,81330.0,2.62e-05,1792166400003,0\n";
        let trades = archive_trades(csv, Contract::SPOT).unwrap();
        assert_eq!((trades[0].price, trades[0].qty), (81330.0, 2.62e-05));
        let inverse = Contract {
            value: 100.0,
            inverse: true,
        };
        assert_eq!(inverse.base(5.0, 50_000.0), 0.01, "five 100-dollar contracts at 50000");
    }

    #[test]
    fn api_answers_are_read_oldest_first_and_errors_named() {
        let body = r#"{"code":"0","msg":"","data":[
            ["1792385880000","82784.8","82784.9","82774.4","82784.2","1.28","106438.9","106438.9","0"],
            ["1792385820000","82787.1","82801.7","82784.3","82784.9","0.86","71233.6","71233.6","1"]]}"#;
        let c = candles(body, Market::Spot).unwrap();
        assert_eq!(
            (c[0].time, c[1].close, c[0].volume),
            (Time(1792385820000), 82784.2, 0.86)
        );
        let body = r#"{"code":"0","data":[
            {"instId":"BTC-USDT-SWAP","tradeId":"3004586946","px":"82744.7","sz":"0.26","side":"sell","ts":"1792385920106"},
            {"instId":"BTC-USDT-SWAP","tradeId":"3004586945","px":"82744.7","sz":"7.84","side":"buy","ts":"1792385919872"}]}"#;
        let swap = Contract {
            value: 0.01,
            inverse: false,
        };
        let t = trades(body, swap).unwrap();
        assert_eq!((t[0].id, t[0].trade.side), (3004586945, Side::Buy));
        assert!((t[0].trade.qty - 0.0784).abs() < 1e-12);
        let error = r#"{"code":"51001","data":[],"msg":"Instrument ID doesn't exist."}"#;
        assert!(matches!(
            candles(error, Market::Spot),
            Err(Error::Api { code: 51001, .. })
        ));
    }

    #[test]
    fn tickers_are_named_without_the_swap_suffix_and_valued_in_the_quote() {
        let body = r#"{"code":"0","data":[
            {"instId":"BTC-USDT-SWAP","last":"80000","open24h":"78000","volCcy24h":"2","ts":"1792385922071"},
            {"instId":"BTC-USDT-261225","last":"81000","open24h":"80000","volCcy24h":"1","ts":"1792385922071"}]}"#;
        let t = tickers(body, Market::Swap).unwrap();
        assert_eq!(t.len(), 1, "an expiring future is not a swap");
        assert_eq!((t[0].symbol.as_str(), t[0].quote_volume), ("BTC-USDT", 160_000.0));
        assert!((t[0].change - 2.5641).abs() < 1e-3);
        let body = r#"{"code":"0","data":[{"ctVal":"0.01","ctMult":"1","ctType":"linear"}]}"#;
        assert_eq!(contract(body).unwrap(), swap_of(0.01));
    }

    fn swap_of(value: f64) -> Contract {
        Contract { value, inverse: false }
    }

    #[test]
    fn stream_messages_give_their_rows_and_answers_give_none() {
        let candle = r#"{"arg":{"channel":"candle1m","instId":"BTC-USDT"},"data":[["1597026383085","8533.02","8553.74","8527.17","8548.26","45247","529.5858061","529.58","0"]]}"#;
        let c = candle_event(candle, Market::Spot).unwrap();
        assert_eq!((c[0].time, c[0].close), (Time(1597026383085), 8548.26));
        let trades = r#"{"arg":{"channel":"trades-all","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT","tradeId":"130639474","px":"42219.9","sz":"0.12","side":"buy","ts":"1630048897897"}]}"#;
        assert_eq!(trades_event(trades, Contract::SPOT).unwrap()[0].id, 130639474);
        let subscribed = r#"{"event":"subscribe","arg":{"channel":"tickers","instId":"BTC-USDT"},"connId":"a4d3ae55"}"#;
        assert!(tickers_event(subscribed, Market::Spot).unwrap().is_empty());
        assert!(tickers_event("pong", Market::Spot).unwrap().is_empty());
        let failed = r#"{"event":"error","code":"60018","msg":"Wrong URL or channel"}"#;
        assert!(tickers_event(failed, Market::Spot).is_err());
    }
}
