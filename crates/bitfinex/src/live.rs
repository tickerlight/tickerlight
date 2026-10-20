//! Live data over Bitfinex's WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that reads
//! the socket and hands what came to the chart through a channel, so polling never blocks. Bitfinex sends a
//! heartbeat on each channel every 15 seconds: a connection quiet for longer than a minute is opened again.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::parse::{self, Event};
use crate::{Bitfinex, IdTrade, MINUTE, Ticker, api_name, now_ms};

const STREAM: &str = "wss://api-pub.bitfinex.com/ws/2";
/// How long a connection may be quiet (no heartbeat either) before it is taken as dropped.
const QUIET: Duration = Duration::from_secs(60);
/// Channels on one connection: Bitfinex allows 30.
const CHANNELS: usize = 25;

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// A connection that asked for each of `requests` (subscriptions).
fn subscribe(requests: &[Value]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(STREAM).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    for request in requests {
        socket.send(Message::text(request.to_string())).ok()?;
    }
    Some(socket)
}

/// Reads the messages until the connection drops or goes quiet. `on_event` returns false to stop; then so does
/// this. True when the connection dropped (to be opened again).
fn read(socket: &mut Socket, mut on_event: impl FnMut(Event) -> bool) -> bool {
    let mut heard = Instant::now();
    loop {
        if heard.elapsed() >= QUIET {
            return true;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                heard = Instant::now();
                if let Some(event) = parse::event(text.as_str())
                    && !on_event(event)
                {
                    return false;
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

/// The 1-minute candles of a pair from a given one on, live: subscribed to its candles first, then the minutes
/// since the newest had from the API, then the stream (its candles of the last hours, then each change); so again
/// after every reconnection. The candle of the time given comes again too.
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(symbol: &str, newest: Time) -> Self {
        Self::connect_to(Bitfinex::new(), symbol, newest)
    }

    pub fn connect_to(api: Bitfinex, symbol: &str, newest: Time) -> Self {
        let symbol = symbol.to_string();
        let request = json!({
            "event": "subscribe",
            "channel": "candles",
            "key": format!("trade:1m:{}", api_name(&symbol)),
        });
        let mut newest = Newest(newest);
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(std::slice::from_ref(&request)) else {
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
                Event::Candles(_, candles) => candles.into_iter().all(|c| newest.send(c, tx)),
                Event::Candle(_, candle) => newest.send(candle, tx),
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

/// Every trade of a pair from a moment on: subscribed first, then those since the moment from the API, then the
/// stream; after a dropped connection the same from the last trade had, by id, so none is had twice or missed.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(symbol: &str, from: Time) -> Self {
        Self::connect_to(Bitfinex::new(), symbol, from)
    }

    pub fn connect_to(api: Bitfinex, symbol: &str, from: Time) -> Self {
        let symbol = symbol.to_string();
        let request = json!({ "event": "subscribe", "channel": "trades", "symbol": api_name(&symbol) });
        let mut last: Option<(i64, Time)> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe(std::slice::from_ref(&request)) else {
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
                Event::Trades(_, trades) => trades.into_iter().all(&mut send),
                Event::Trade(_, t) => send(t),
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

/// The last prices of a few pairs, live, from their ticker channels (a connection for each 25 of them).
pub struct LivePrices {
    rxs: Vec<Receiver<Ticker>>,
}

impl LivePrices {
    pub fn connect(symbols: &[String]) -> Self {
        let rxs = symbols
            .chunks(CHANNELS)
            .map(|some| {
                let requests: Vec<Value> = some
                    .iter()
                    .map(|s| json!({ "event": "subscribe", "channel": "ticker", "symbol": api_name(s) }))
                    .collect();
                spawn_loop(move |tx: &Sender<Ticker>| {
                    let Some(mut socket) = subscribe(&requests) else {
                        return true;
                    };
                    // The channels' pairs, as they are subscribed.
                    let mut pairs: HashMap<i64, String> = HashMap::new();
                    read(&mut socket, |event| match event {
                        Event::Subscribed { channel, name } => {
                            if let Some(name) = name.strip_prefix('t') {
                                pairs.insert(channel, name.to_string());
                            }
                            true
                        }
                        Event::Ticker(channel, fields) => {
                            match pairs
                                .get(&channel)
                                .and_then(|name| parse::ticker(name, &fields, Time(now_ms())))
                            {
                                Some(ticker) => tx.send(ticker).is_ok(),
                                None => true,
                            }
                        }
                        _ => true,
                    })
                })
            })
            .collect();
        Self { rxs }
    }

    /// The tickers that came since the last call.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rxs.iter().flat_map(|rx| rx.try_iter()).collect()
    }
}
