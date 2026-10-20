//! Live data over Deribit's WebSocket (JSON-RPC), as chart [`Source`]s or lists to take from. Each runs a thread
//! that reads the socket and hands what came to the chart through a channel, so polling never blocks. The server
//! sends a heartbeat that asks for an answer every 30 seconds; a connection quiet for longer than 90 is opened
//! again.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::parse::{self, Event};
use crate::{Deribit, IdTrade, MINUTE, Ticker, now_ms};

const STREAM: &str = "wss://www.deribit.com/ws/api/v2";
/// How long a connection may be quiet (no heartbeat either) before it is taken as dropped.
const QUIET: Duration = Duration::from_secs(90);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

fn request(method: &str, params: serde_json::Value) -> Message {
    Message::text(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string())
}

/// A connection with a heartbeat, subscribed to `channels`.
fn subscribe(channels: &[String]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(STREAM).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    socket
        .send(request("public/set_heartbeat", json!({ "interval": 30 })))
        .ok()?;
    socket
        .send(request("public/subscribe", json!({ "channels": channels })))
        .ok()?;
    Some(socket)
}

/// Reads the messages until the connection drops or goes quiet, answering the heartbeat. `on_event` returns false
/// to stop; then so does this. True when the connection dropped (to be opened again).
fn read(socket: &mut Socket, mut on_event: impl FnMut(Event) -> bool) -> bool {
    let mut heard = Instant::now();
    loop {
        if heard.elapsed() >= QUIET {
            return true;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                heard = Instant::now();
                match parse::event(text.as_str()) {
                    Some(Event::TestRequest) => {
                        if socket.send(request("public/test", json!({}))).is_err() {
                            return true;
                        }
                    }
                    Some(event) => {
                        if !on_event(event) {
                            return false;
                        }
                    }
                    None => {}
                }
            }
            Ok(_) => heard = Instant::now(),
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

/// Sends candles not older than the newest sent; false when the chart is gone.
struct Newest(Time);

impl Newest {
    fn send(&mut self, candle: Candle, tx: &Sender<Candle>) -> bool {
        if candle.time < self.0 {
            return true;
        }
        self.0 = candle.time;
        tx.send(candle).is_ok()
    }
}

/// The 1-minute candles of a perpetual from a given one on, live: subscribed to its forming candle first, then the
/// minutes since the newest had from the API, then the stream; so again after every reconnection. The candle of
/// the time given comes again too.
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(symbol: &str, newest: Time) -> Self {
        Self::connect_to(Deribit::new(), symbol, newest)
    }

    pub fn connect_to(api: Deribit, symbol: &str, newest: Time) -> Self {
        let symbol = symbol.to_string();
        let channels = vec![format!("chart.trades.{symbol}.1")];
        let mut newest = Newest(newest);
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(&channels) else {
                return true;
            };
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let Ok(missed) = api.candles(&symbol, newest.0, Time(now_ms() + MINUTE)) else {
                return true;
            };
            if !missed.into_iter().all(|c| newest.send(c, tx)) {
                return false;
            }
            read(&mut socket, |event| match event {
                Event::Candle(candle) => newest.send(candle, tx),
                _ => true,
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

/// Every trade of a perpetual from a moment on: subscribed first, then those since the moment from the history
/// server, then the stream; after a dropped connection the same from the last trade had, by sequence, so none is
/// had twice or missed.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(symbol: &str, from: Time) -> Self {
        Self::connect_to(Deribit::new(), symbol, from)
    }

    pub fn connect_to(api: Deribit, symbol: &str, from: Time) -> Self {
        let symbol = symbol.to_string();
        let channels = vec![format!("trades.{symbol}.100ms")];
        let mut last: Option<(i64, Time)> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe(&channels) else {
                return true;
            };
            let since = last.map_or(from, |(_, time)| time);
            let Ok(missed) = api.trades_since(&symbol, since, Time(now_ms() + 1)) else {
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
            read(&mut socket, |event| match event {
                Event::Trades(trades) => trades.into_iter().all(&mut send),
                _ => true,
            })
        });
        Self { rx }
    }

    /// The trades that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Trade> {
        self.rx.try_iter().collect()
    }
}

/// The last prices of a few perpetuals, live, from their ticker channels.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(symbols: &[String]) -> Self {
        let channels: Vec<String> = symbols.iter().map(|s| format!("ticker.{s}.100ms")).collect();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe(&channels) else {
                return true;
            };
            read(&mut socket, |event| match event {
                Event::Ticker(ticker) => tx.send(ticker).is_ok(),
                _ => true,
            })
        });
        Self { rx }
    }

    /// The tickers that came since the last call.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rx.try_iter().collect()
    }
}
