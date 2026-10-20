//! Live data over Coinbase's WebSocket feed, as chart [`Source`]s or lists to take from. Each runs a thread that
//! reads the socket and hands what came to the chart through a channel, so polling never blocks. The feed has no
//! 1-minute candles: they are made from the trades, as Coinbase makes its own.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{Coinbase, IdTrade, MINUTE, Ticker, now_ms, parse};

pub const STREAM: &str = "wss://ws-feed.exchange.coinbase.com";
/// How long a connection may be quiet before it is pinged.
const PING_AFTER: Duration = Duration::from_secs(20);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// Connects to `url` and subscribes to `channel` of the products.
fn subscribe(url: &str, channel: &str, products: &[String]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(url).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(PING_AFTER)).ok()?;
    let request = json!({ "type": "subscribe", "product_ids": products, "channels": [channel] }).to_string();
    socket.send(Message::text(request)).ok()?;
    Some(socket)
}

/// Reads the text messages of `socket` until the connection drops, pinging it when quiet. `on_text` returns false
/// to stop; then so does this.
fn read(socket: &mut Socket, mut on_text: impl FnMut(&str) -> bool) -> bool {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if !on_text(text.as_str()) {
                    return false;
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if socket.send(Message::Ping(Vec::new().into())).is_err() {
                    return true;
                }
            }
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

/// The candles made so far: the newest one sent and the last trade in it.
struct Minutes {
    newest: Time,
    forming: Option<Candle>,
    last: Option<i64>,
}

impl Minutes {
    /// Sends a candle not older than the newest sent; false when the chart is gone.
    fn send(&mut self, candle: Candle, tx: &Sender<Candle>) -> bool {
        if candle.time < self.newest {
            return true;
        }
        self.newest = candle.time;
        tx.send(candle).is_ok()
    }

    /// Puts a trade not had yet into its minute and sends that.
    fn trade(&mut self, t: IdTrade, tx: &Sender<Candle>) -> bool {
        if self.last.is_some_and(|l| t.id <= l) {
            return true;
        }
        self.last = Some(t.id);
        let Trade { time, price, qty, .. } = t.trade;
        let minute = Time(time.0 - time.0.rem_euclid(MINUTE));
        if minute < self.newest {
            return true;
        }
        let candle = match self.forming {
            Some(mut c) if c.time == minute => {
                c.high = c.high.max(price);
                c.low = c.low.min(price);
                c.close = price;
                c.volume += qty;
                c
            }
            _ => Candle {
                time: minute,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: qty,
            },
        };
        self.forming = Some(candle);
        self.send(candle, tx)
    }
}

/// The 1-minute candles of a symbol from a given one on, live: subscribed to the trades first, then the whole
/// minutes missed since it from the candles of the API and the forming one from its trades, then the stream. After
/// a dropped connection the trades again from the last one had, so a minute is never counted twice. The candle of
/// the time given comes again too (it was forming).
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(symbol: &str, newest: Time) -> Self {
        Self::connect_to(Coinbase::new(), STREAM, symbol, newest)
    }

    pub fn connect_to(api: Coinbase, stream: &str, symbol: &str, newest: Time) -> Self {
        let (url, symbol) = (stream.to_string(), symbol.to_string());
        let mut minutes = Minutes {
            newest,
            forming: None,
            last: None,
        };
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(&url, "matches", std::slice::from_ref(&symbol)) else {
                return true;
            };
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let missed = match (minutes.last, minutes.forming) {
                (Some(last), Some(forming)) => api.trades_since(&symbol, forming.time, Some(last)),
                _ => {
                    let now = now_ms();
                    let minute = Time(now - now.rem_euclid(MINUTE));
                    let Ok(whole) = api.candles(&symbol, minutes.newest, minute) else {
                        return true;
                    };
                    if !whole.into_iter().all(|c| minutes.send(c, tx)) {
                        return false;
                    }
                    api.trades_since(&symbol, minute, None)
                }
            };
            let Ok(missed) = missed else {
                return true;
            };
            if !missed.into_iter().all(|t| minutes.trade(t, tx)) {
                return false;
            }
            read(&mut socket, |text| match parse::match_event(text) {
                Ok(trades) => trades.into_iter().all(|t| minutes.trade(t, tx)),
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

/// Every trade of a symbol from a moment on: subscribed first, then what was missed from the API, then the stream;
/// after a dropped connection the API again from the last trade had, so none is missed or had twice.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(symbol: &str, from: Time) -> Self {
        Self::connect_to(Coinbase::new(), STREAM, symbol, from)
    }

    pub fn connect_to(api: Coinbase, stream: &str, symbol: &str, from: Time) -> Self {
        let (url, symbol) = (stream.to_string(), symbol.to_string());
        let mut last: Option<i64> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe(&url, "matches", std::slice::from_ref(&symbol)) else {
                return true;
            };
            let Ok(missed) = api.trades_since(&symbol, from, last) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: IdTrade| {
                if last.is_some_and(|l| t.id <= l) || t.trade.time < from {
                    return true;
                }
                last = Some(t.id);
                tx.send(t.trade).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            read(&mut socket, |text| match parse::match_event(text) {
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

/// The last prices of a few symbols, live: one connection subscribed to their `ticker` channel.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(symbols: &[String]) -> Self {
        Self::connect_to(STREAM, symbols)
    }

    pub fn connect_to(stream: &str, symbols: &[String]) -> Self {
        let (url, symbols) = (stream.to_string(), symbols.to_vec());
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe(&url, "ticker", &symbols) else {
                return true;
            };
            read(&mut socket, |text| match parse::ticker_event(text) {
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
