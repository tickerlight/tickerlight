//! Live data over OKX's WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that reads the
//! socket and hands what came to the chart through a channel, so polling never blocks. OKX drops a connection
//! that is quiet for 30 seconds: a quiet one is pinged.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{Market, Okx, Ticker, parse};

/// The channels of prices.
pub const PUBLIC_STREAM: &str = "wss://ws.okx.com:8443/ws/v5/public";
/// The channels of candles and of every trade.
pub const BUSINESS_STREAM: &str = "wss://ws.okx.com:8443/ws/v5/business";
/// How long a connection may be quiet before it is pinged.
const PING_AFTER: Duration = Duration::from_secs(20);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// Connects to `url` and subscribes to the channels `args`.
fn subscribe(url: &str, args: Vec<Value>) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(url).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(PING_AFTER)).ok()?;
    let request = json!({ "op": "subscribe", "args": args }).to_string();
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
                if socket.send(Message::text("ping")).is_err() {
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The 1-minute candles of a symbol from a given one on, live: subscribed first, then those missed since it from
/// the API, then the stream; so again after every reconnection, so a dropped connection leaves no hole. The candle
/// of the time given comes again too (it was forming).
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(market: Market, symbol: &str, newest: Time) -> Self {
        Self::connect_to(Okx::new(market), BUSINESS_STREAM, symbol, newest)
    }

    pub fn connect_to(api: Okx, stream: &str, symbol: &str, newest: Time) -> Self {
        let (url, symbol) = (stream.to_string(), symbol.to_string());
        let args = vec![json!({ "channel": "candle1m", "instId": api.market().inst_id(&symbol) })];
        let mut newest = newest;
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe(&url, args.clone()) else {
                return true;
            };
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let Ok(missed) = api.candles(&symbol, newest, Time(now_ms() + 60_000)) else {
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
            let market = api.market();
            read(&mut socket, |text| match parse::candle_event(text, market) {
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

/// Every trade of a symbol from a moment on: subscribed first, then what was missed from the API, then the stream;
/// after a dropped connection the API again from the last trade had, so none is missed or had twice.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(market: Market, symbol: &str, from: Time) -> Self {
        Self::connect_to(Okx::new(market), BUSINESS_STREAM, symbol, from)
    }

    pub fn connect_to(api: Okx, stream: &str, symbol: &str, from: Time) -> Self {
        let (url, symbol) = (stream.to_string(), symbol.to_string());
        let args = vec![json!({ "channel": "trades-all", "instId": api.market().inst_id(&symbol) })];
        let mut last: Option<i64> = None;
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Ok(contract) = api.contract(&symbol) else {
                return true;
            };
            let Some(mut socket) = subscribe(&url, args.clone()) else {
                return true;
            };
            let Ok(missed) = api.trades_since(&symbol, from, last) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: parse::IdTrade| {
                if last.is_some_and(|l| t.id <= l) {
                    return true;
                }
                last = Some(t.id);
                tx.send(t.trade).is_ok()
            };
            if !missed.into_iter().all(&mut send) {
                return false;
            }
            read(&mut socket, |text| match parse::trades_event(text, contract) {
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

/// The last prices of a few symbols, live: one connection subscribed to the `tickers` channel of each.
pub struct LivePrices {
    rx: Receiver<Ticker>,
}

impl LivePrices {
    pub fn connect(market: Market, symbols: &[String]) -> Self {
        Self::connect_to(PUBLIC_STREAM, market, symbols)
    }

    pub fn connect_to(stream: &str, market: Market, symbols: &[String]) -> Self {
        let url = stream.to_string();
        let args: Vec<Value> = symbols
            .iter()
            .map(|s| json!({ "channel": "tickers", "instId": market.inst_id(s) }))
            .collect();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe(&url, args.clone()) else {
                return true;
            };
            read(&mut socket, |text| match parse::tickers_event(text, market) {
                Ok(tickers) => tickers.into_iter().all(|t| tx.send(t).is_ok()),
                Err(_) => true,
            })
        });
        Self { rx }
    }

    /// The prices that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Ticker> {
        self.rx.try_iter().collect()
    }
}
