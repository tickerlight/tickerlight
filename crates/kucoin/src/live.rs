//! Live data over KuCoin's WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that reads the
//! socket and hands what came to the chart through a channel, so polling never blocks. A connection needs a token
//! from the API first, and a ping at the interval KuCoin gives with it, busy or not.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{Kucoin, MINUTE, Market, Ticker, now_ms, parse};

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// A connection subscribed to the topics, with how often it must be pinged.
struct Connection {
    socket: Socket,
    ping: Duration,
}

impl Connection {
    fn open(api: &Kucoin, topics: &[String]) -> Option<Self> {
        let (endpoint, token, ping) = api.bullet().ok()?;
        let url = format!("{endpoint}?token={token}&connectId={}", now_ms());
        let (mut socket, _) = tungstenite::connect(url.as_str()).ok()?;
        let ping = Duration::from_millis(ping);
        let stream = match socket.get_ref() {
            MaybeTlsStream::Plain(s) => Some(s),
            MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
            _ => None,
        };
        stream?.set_read_timeout(Some(ping / 2)).ok()?;
        for (i, topic) in topics.iter().enumerate() {
            let request = json!({
                "id": (now_ms() + i as i64).to_string(),
                "type": "subscribe",
                "topic": topic,
                "privateChannel": false,
                "response": true,
            });
            socket.send(Message::text(request.to_string())).ok()?;
        }
        Some(Self { socket, ping })
    }

    /// Reads the text messages until the connection drops, pinging it on time. `on_text` returns false to stop;
    /// then so does this.
    fn read(&mut self, mut on_text: impl FnMut(&str) -> bool) -> bool {
        let mut pinged = Instant::now();
        loop {
            if pinged.elapsed() >= self.ping {
                let ping = json!({ "id": now_ms().to_string(), "type": "ping" }).to_string();
                if self.socket.send(Message::text(ping)).is_err() {
                    return true;
                }
                pinged = Instant::now();
            }
            match self.socket.read() {
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
    pub fn connect(market: Market, symbol: &str, newest: Time) -> Self {
        Self::connect_to(Kucoin::new(market), symbol, newest)
    }

    pub fn connect_to(api: Kucoin, symbol: &str, newest: Time) -> Self {
        let symbol = symbol.to_string();
        let topics = vec![match api.market() {
            Market::Spot => format!("/market/candles:{symbol}_1min"),
            Market::Futures => format!("/contractMarket/limitCandle:{symbol}_1min"),
        }];
        let mut newest = newest;
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let Ok(contract) = api.contract(&symbol) else {
                return true;
            };
            let Some(mut connection) = Connection::open(&api, &topics) else {
                return true;
            };
            let Ok(missed) = api.candles(&symbol, newest, Time(now_ms() + MINUTE)) else {
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
            connection.read(|text| match parse::candle_event(text, contract) {
                Ok(Some(candle)) => send(candle),
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

/// Every trade of a symbol streamed from a moment on: subscribed first, then the last hundred trades of the API
/// (all the API gives: a busy symbol's trades before the connection are mostly not had), then the stream; after a
/// dropped connection the same, by sequence number, so none is had twice.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(market: Market, symbol: &str, from: Time) -> Self {
        Self::connect_to(Kucoin::new(market), symbol, from)
    }

    pub fn connect_to(api: Kucoin, symbol: &str, from: Time) -> Self {
        let symbol = symbol.to_string();
        let topics = vec![match api.market() {
            Market::Spot => format!("/market/match:{symbol}"),
            Market::Futures => format!("/contractMarket/execution:{symbol}"),
        }];
        let mut last: Option<i64> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Ok(contract) = api.contract(&symbol) else {
                return true;
            };
            let Some(mut connection) = Connection::open(&api, &topics) else {
                return true;
            };
            let Ok(missed) = api.last_trades(&symbol) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: parse::IdTrade| {
                if last.is_some_and(|l| t.id <= l) || t.trade.time < from {
                    return true;
                }
                last = Some(t.id);
                tx.send(t.trade).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            connection.read(|text| match parse::match_event(text, contract) {
                Ok(Some(trade)) => send(trade),
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

/// The last prices of a few symbols, live: one connection subscribed to the snapshot of each.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(market: Market, symbols: &[String]) -> Self {
        Self::connect_to(Kucoin::new(market), symbols)
    }

    pub fn connect_to(api: Kucoin, symbols: &[String]) -> Self {
        let prefix = match api.market() {
            Market::Spot => "/market/snapshot:",
            Market::Futures => "/contractMarket/snapshot:",
        };
        let topics: Vec<String> = symbols.iter().map(|s| format!("{prefix}{s}")).collect();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut connection) = Connection::open(&api, &topics) else {
                return true;
            };
            connection.read(|text| match parse::snapshot_event(text) {
                Ok(Some(ticker)) => tx.send(ticker).is_ok(),
                _ => true,
            })
        });
        Self { rx }
    }

    /// The prices that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rx.try_iter().collect()
    }
}
