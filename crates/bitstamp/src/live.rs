//! Live data over Bitstamp's WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that reads
//! the socket and hands what came to the chart through a channel, so polling never blocks. The stream has trades
//! only: candles and prices are made from them. A heartbeat keeps the connection.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{Bitstamp, IdTrade, MINUTE, Ticker, now_ms, parse};

const STREAM: &str = "wss://ws.bitstamp.net";
const HEARTBEAT: Duration = Duration::from_secs(30);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// A connection subscribed to the trades of each pair.
fn subscribe(symbols: &[String]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(STREAM).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    for symbol in symbols {
        let channel = format!("live_trades_{}", symbol.to_ascii_lowercase());
        let request = json!({ "event": "bts:subscribe", "data": { "channel": channel } });
        socket.send(Message::text(request.to_string())).ok()?;
    }
    Some(socket)
}

/// Reads the text messages until the connection drops, with a heartbeat on time. `on_text` returns false to stop;
/// then so does this. True when the connection dropped (to be opened again).
fn read(socket: &mut Socket, mut on_text: impl FnMut(&str) -> bool) -> bool {
    let mut beat = Instant::now();
    loop {
        if beat.elapsed() >= HEARTBEAT {
            let heartbeat = json!({ "event": "bts:heartbeat" }).to_string();
            if socket.send(Message::text(heartbeat)).is_err() {
                return true;
            }
            beat = Instant::now();
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if !on_text(text.as_str()) {
                    return false;
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return true,
        }
    }
}

/// Runs `work` again and again on a thread, waiting longer after each time it gives up (a failure), until it
/// returns false (the receiving side is gone).
fn spawn_loop<T: Send + 'static>(mut work: impl FnMut(&Sender<T>) -> bool + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut backoff = Duration::from_millis(500);
        while work(&tx) {
            thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
    rx
}

/// The candles made so far: the newest one sent and the last trade in them. As Bitstamp's own, a minute without
/// trades is flat at the close before it.
struct Minutes {
    newest: Time,
    last: Option<Candle>,
    last_id: Option<i64>,
}

impl Minutes {
    /// Sends a candle not older than the newest sent; false when the chart is gone.
    fn send(&mut self, candle: Candle, tx: &Sender<Candle>) -> bool {
        if candle.time < self.newest {
            return true;
        }
        self.newest = candle.time;
        self.last = Some(candle);
        tx.send(candle).is_ok()
    }

    /// Puts a trade not had yet into its minute and sends that, after the flat minutes since the last candle.
    fn trade(&mut self, t: IdTrade, tx: &Sender<Candle>) -> bool {
        if self.last_id.is_some_and(|l| t.id <= l) {
            return true;
        }
        self.last_id = Some(t.id);
        let Trade { time, price, qty, .. } = t.trade;
        let minute = Time(time.0 - time.0.rem_euclid(MINUTE));
        if minute < self.newest {
            return true;
        }
        let fresh = Candle {
            time: minute,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: qty,
        };
        let candle = match self.last {
            Some(mut c) if c.time == minute => {
                c.high = c.high.max(price);
                c.low = c.low.min(price);
                c.close = price;
                c.volume += qty;
                c
            }
            Some(before) => {
                let close = before.close;
                for gap in (before.time.0 + MINUTE..minute.0).step_by(MINUTE as usize) {
                    let flat = Candle {
                        time: Time(gap),
                        open: close,
                        high: close,
                        low: close,
                        close,
                        volume: 0.0,
                    };
                    if !self.send(flat, tx) {
                        return false;
                    }
                }
                fresh
            }
            None => fresh,
        };
        self.send(candle, tx)
    }
}

/// The 1-minute candles of a pair from a given one on, live: subscribed to the trades first, then the whole minutes
/// since it from the candles of the API (they replace the one that was forming) and the forming one from its
/// trades, then the stream; so again after every reconnection. The candle of the time given comes again too.
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(symbol: &str, newest: Time) -> Self {
        Self::connect_to(Bitstamp::new(), symbol, newest)
    }

    pub fn connect_to(api: Bitstamp, symbol: &str, newest: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut minutes = Minutes {
            newest,
            last: None,
            last_id: None,
        };
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(&symbols) else {
                return true;
            };
            let symbol = &symbols[0];
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let now = now_ms();
            let minute = Time(now - now.rem_euclid(MINUTE));
            let Ok(whole) = api.candles(symbol, minutes.newest, minute) else {
                return true;
            };
            if !whole.into_iter().all(|c| minutes.send(c, tx)) {
                return false;
            }
            let Ok(missed) = api.trades_since(symbol, minute, Time(now + 1)) else {
                return true;
            };
            if !missed.into_iter().all(|t| minutes.trade(t, tx)) {
                return false;
            }
            read(&mut socket, |text| match parse::trade_event(text) {
                Some((_, t)) => minutes.trade(t, tx),
                None => true,
            })
        });
        Self { rx }
    }
}

impl Source for LiveCandles {
    fn poll(&mut self, sink: &mut Sink) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(candle) => sink.candle(candle),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }
}

/// Every trade of a pair from a moment on (of the last 24 hours): subscribed first, then those since the moment
/// from the API, then the stream; after a dropped connection the same from the last trade had, by id, so none is
/// had twice or missed.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(symbol: &str, from: Time) -> Self {
        Self::connect_to(Bitstamp::new(), symbol, from)
    }

    pub fn connect_to(api: Bitstamp, symbol: &str, from: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut last: Option<(i64, Time)> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe(&symbols) else {
                return true;
            };
            let since = last.map_or(from, |(_, time)| time);
            let Ok(missed) = api.trades_since(&symbols[0], since, Time(now_ms() + 1)) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: IdTrade| {
                if last.is_some_and(|(id, _)| t.id <= id) || t.trade.time < from {
                    return true;
                }
                last = Some((t.id, t.trade.time));
                tx.send(t.trade).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            read(&mut socket, |text| match parse::trade_event(text) {
                Some((_, t)) => send(t),
                None => true,
            })
        });
        Self { rx }
    }

    /// The trades that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Trade> {
        self.rx.try_iter().collect()
    }
}

/// The last prices of a few pairs, live: the day's open and turnover from the API's tickers when connecting, then
/// each trade of the stream as the last price.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(symbols: &[String]) -> Self {
        Self::connect_to(Bitstamp::new(), symbols)
    }

    pub fn connect_to(api: Bitstamp, symbols: &[String]) -> Self {
        let symbols = symbols.to_vec();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe(&symbols) else {
                return true;
            };
            let Ok(tickers) = api.tickers() else {
                return true;
            };
            // Each pair's ticker and the price a day ago, which the change is from.
            let mut day: HashMap<String, (Ticker, f64)> = HashMap::new();
            for t in tickers.into_iter().filter(|t| symbols.contains(&t.symbol)) {
                let open = t.last / (1.0 + t.change / 100.0);
                if tx.send(t.clone()).is_err() {
                    return false;
                }
                day.insert(t.symbol.clone(), (t, open));
            }
            read(&mut socket, |text| {
                let Some((pair, t)) = parse::trade_event(text) else {
                    return true;
                };
                let Some((ticker, open)) = day.get(&pair) else {
                    return true;
                };
                let price = t.trade.price;
                tx.send(Ticker {
                    last: price,
                    change: (price / open - 1.0) * 100.0,
                    close_time: t.trade.time,
                    ..ticker.clone()
                })
                .is_ok()
            })
        });
        Self { rx }
    }

    /// The prices that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use tickerlight_core::Side;

    use super::*;

    #[test]
    fn minutes_open_at_their_first_trade_and_quiet_ones_are_flat_as_bitstamp_s() {
        let (tx, rx) = mpsc::channel();
        let mut minutes = Minutes {
            newest: Time(0),
            last: None,
            last_id: None,
        };
        let whole = Candle {
            time: Time(0),
            open: 9.0,
            high: 10.0,
            low: 9.0,
            close: 10.0,
            volume: 1.0,
        };
        assert!(minutes.send(whole, &tx));
        let trade = |id, price, qty| IdTrade {
            id,
            trade: Trade {
                time: Time(2 * MINUTE + 5),
                price,
                qty,
                side: Side::Sell,
            },
        };
        for t in [trade(7, 12.0, 0.5), trade(8, 11.0, 0.25), trade(8, 11.0, 0.25)] {
            assert!(minutes.trade(t, &tx));
        }
        let sent: Vec<Candle> = rx.try_iter().collect();
        let row = |c: &Candle| (c.time, c.open, c.high, c.low, c.close, c.volume);
        assert_eq!(row(&sent[1]), (Time(MINUTE), 10.0, 10.0, 10.0, 10.0, 0.0));
        let last = sent.last().unwrap();
        assert_eq!(
            row(last),
            (Time(2 * MINUTE), 12.0, 12.0, 11.0, 11.0, 0.75),
            "a trade had twice counts once"
        );
    }
}
