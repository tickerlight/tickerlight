//! Live data over Bitget's WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that reads the
//! socket and hands what came to the chart through a channel, so polling never blocks. Bitget closes a connection
//! that is not pinged (a "ping" text) at least every 30 seconds.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{Bitget, MINUTE, Ticker, now_ms, parse};

const STREAM: &str = "wss://ws.bitget.com/v2/ws/public";
const PING: Duration = Duration::from_secs(20);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// A connection subscribed to `channel` of each symbol.
fn subscribe(api: &Bitget, channel: &str, symbols: &[String]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(STREAM).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let args: Vec<_> = symbols
        .iter()
        .map(|s| json!({ "instType": api.market().inst_type(), "channel": channel, "instId": s }))
        .collect();
    socket
        .send(Message::text(json!({ "op": "subscribe", "args": args }).to_string()))
        .ok()?;
    Some(socket)
}

/// Reads the text messages until the connection drops, pinging it on time. `on_text` returns false to stop; then
/// so does this. True when the connection dropped (to be opened again).
fn read(socket: &mut Socket, mut on_text: impl FnMut(&str) -> bool) -> bool {
    let mut pinged = Instant::now();
    loop {
        if pinged.elapsed() >= PING {
            if socket.send(Message::text("ping")).is_err() {
                return true;
            }
            pinged = Instant::now();
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

/// The 1-minute candles of a symbol from a given one on, live: subscribed first, then those missed since it from
/// the API, then the stream; so again after every reconnection, so a dropped connection leaves no hole. The candle
/// of the time given comes again too (it was forming).
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(market: crate::Market, symbol: &str, newest: Time) -> Self {
        Self::connect_to(Bitget::new(market), symbol, newest)
    }

    pub fn connect_to(api: Bitget, symbol: &str, newest: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut newest = newest;
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(&api, "candle1m", &symbols) else {
                return true;
            };
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let Ok(missed) = api.candles(&symbols[0], newest, Time(now_ms() + MINUTE)) else {
                return true;
            };
            // Sends what is not older than the newest sent; false when the chart is gone.
            let mut send = |candle: Candle| {
                if candle.time < newest {
                    return true;
                }
                newest = candle.time;
                tx.send(candle).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            read(&mut socket, |text| match parse::candles_event(text) {
                Ok(candles) => candles.into_iter().all(&mut send),
                Err(_) => true,
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

/// Every trade of a symbol from a moment on: subscribed first, then those since the moment from the API, then the
/// stream; after a dropped connection the same from the last trade had, by id, so none is had twice or missed.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(market: crate::Market, symbol: &str, from: Time) -> Self {
        Self::connect_to(Bitget::new(market), symbol, from)
    }

    pub fn connect_to(api: Bitget, symbol: &str, from: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut last: Option<(i64, Time)> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe(&api, "trade", &symbols) else {
                return true;
            };
            let since = last.map_or(from, |(_, time)| time);
            let Ok(missed) = api.trades_since(&symbols[0], since, Time(now_ms() + 1)) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: parse::IdTrade| {
                if last.is_some_and(|(id, _)| t.id <= id) || t.trade.time < from {
                    return true;
                }
                last = Some((t.id, t.trade.time));
                tx.send(t.trade).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            read(&mut socket, |text| match parse::trades_event(text) {
                Ok(trades) => trades.into_iter().all(&mut send),
                Err(_) => true,
            })
        });
        Self { rx }
    }

    /// The trades that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Trade> {
        self.rx.try_iter().collect()
    }
}

/// The last prices of a few symbols, live: one connection subscribed to the ticker of each.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(market: crate::Market, symbols: &[String]) -> Self {
        Self::connect_to(Bitget::new(market), symbols)
    }

    pub fn connect_to(api: Bitget, symbols: &[String]) -> Self {
        let symbols = symbols.to_vec();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe(&api, "ticker", &symbols) else {
                return true;
            };
            read(&mut socket, |text| {
                parse::tickers_event(text, api.market())
                    .into_iter()
                    .all(|t| tx.send(t).is_ok())
            })
        });
        Self { rx }
    }

    /// The prices that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rx.try_iter().collect()
    }
}
