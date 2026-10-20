//! Live data over Kraken's futures WebSocket, as chart [`Source`]s or lists to take from. Each runs a thread that
//! reads the socket and hands what came to the chart through a channel, so polling never blocks. Kraken closes a
//! connection that is not pinged at least every minute. There are no candles on the stream: they are made from the
//! trades.

use std::io::ErrorKind;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tickerlight_core::{Candle, Sink, Source, Time, Trade};
use tungstenite::Message;
use tungstenite::stream::MaybeTlsStream;

use crate::{IdTrade, Kraken, MINUTE, Ticker, now_ms, parse};

const STREAM: &str = "wss://futures.kraken.com/ws/v1";
const PING: Duration = Duration::from_secs(30);

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

/// A connection subscribed to `feed` of each symbol.
fn subscribe(feed: &str, symbols: &[String]) -> Option<Socket> {
    let (mut socket, _) = tungstenite::connect(STREAM).ok()?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    stream?.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = json!({ "event": "subscribe", "feed": feed, "product_ids": symbols });
    socket.send(Message::text(request.to_string())).ok()?;
    Some(socket)
}

/// Reads the text messages until the connection drops, pinging it on time. `on_text` returns false to stop; then
/// so does this. True when the connection dropped (to be opened again).
fn read(socket: &mut Socket, mut on_text: impl FnMut(&str) -> bool) -> bool {
    let mut pinged = Instant::now();
    loop {
        if pinged.elapsed() >= PING {
            if socket.send(Message::Ping(Vec::new().into())).is_err() {
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

/// The newest trade time had and the ids of the trades had at it. Trades come in time order, from the API and from
/// the stream, so this tells whether one was had already.
#[derive(Default)]
struct Seen {
    time: Option<Time>,
    uids: Vec<u128>,
}

impl Seen {
    /// Whether `t` is new; if so it is had now.
    fn fresh(&mut self, t: &IdTrade) -> bool {
        match self.time {
            Some(time) if t.trade.time < time => false,
            Some(time) if t.trade.time == time => {
                let new = !self.uids.contains(&t.uid);
                if new {
                    self.uids.push(t.uid);
                }
                new
            }
            _ => {
                self.time = Some(t.trade.time);
                self.uids.clear();
                self.uids.push(t.uid);
                true
            }
        }
    }
}

/// The candles made so far: the newest one sent and the trades had. As Kraken's own, a minute opens at the close
/// before it, and a minute without trades is a flat one at that close.
struct Minutes {
    newest: Time,
    last: Option<Candle>,
    seen: Seen,
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
        if !self.seen.fresh(&t) {
            return true;
        }
        let Trade { time, price, qty, .. } = t.trade;
        let minute = Time(time.0 - time.0.rem_euclid(MINUTE));
        if minute < self.newest {
            return true;
        }
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
                Candle {
                    time: minute,
                    open: close,
                    high: close.max(price),
                    low: close.min(price),
                    close: price,
                    volume: qty,
                }
            }
            None => Candle {
                time: minute,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: qty,
            },
        };
        self.send(candle, tx)
    }
}

/// The 1-minute candles of a symbol from a given one on, live: subscribed to the trades first, then the whole
/// minutes missed since it from the candles of the API and the forming one from its trades, then the stream. After
/// a dropped connection the trades again from the last one had, so a trade is never counted twice. The candle of
/// the time given comes again too (it was forming).
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    pub fn connect(symbol: &str, newest: Time) -> Self {
        Self::connect_to(Kraken::new(), symbol, newest)
    }

    pub fn connect_to(api: Kraken, symbol: &str, newest: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut minutes = Minutes {
            newest,
            last: None,
            seen: Seen::default(),
        };
        let rx = spawn_loop(move |tx: &Sender<Candle>| {
            let Some(mut socket) = subscribe("trade", &symbols) else {
                return true;
            };
            let symbol = &symbols[0];
            // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
            let now = now_ms();
            let missed = match minutes.seen.time {
                Some(last) => api.trades_since(symbol, last, Time(now + 1)),
                _ => {
                    let minute = Time(now - now.rem_euclid(MINUTE));
                    let Ok(whole) = api.candles(symbol, minutes.newest, minute) else {
                        return true;
                    };
                    if !whole.into_iter().all(|c| minutes.send(c, tx)) {
                        return false;
                    }
                    api.trades_since(symbol, minute, Time(now + 1))
                }
            };
            let Ok(missed) = missed else {
                return true;
            };
            if !missed.into_iter().all(|t| minutes.trade(t, tx)) {
                return false;
            }
            read(&mut socket, |text| match parse::trades_event(text) {
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

/// Every trade of a symbol from a moment on: subscribed first, then those since the moment from the API, then the
/// stream; after a dropped connection the same from the last trade had, so none is had twice or missed.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(symbol: &str, from: Time) -> Self {
        Self::connect_to(Kraken::new(), symbol, from)
    }

    pub fn connect_to(api: Kraken, symbol: &str, from: Time) -> Self {
        let symbols = vec![symbol.to_string()];
        let mut seen = Seen::default();
        let rx = spawn_loop(move |tx: &Sender<Trade>| {
            let Some(mut socket) = subscribe("trade", &symbols) else {
                return true;
            };
            let since = seen.time.unwrap_or(from);
            let Ok(missed) = api.trades_since(&symbols[0], since, Time(now_ms() + 1)) else {
                return true;
            };
            // Sends a trade not had yet; false when the chart is gone.
            let mut send = |t: IdTrade| {
                if t.trade.time < from || !seen.fresh(&t) {
                    return true;
                }
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
    pub fn connect(symbols: &[String]) -> Self {
        let symbols = symbols.to_vec();
        let rx = spawn_loop(move |tx: &Sender<Ticker>| {
            let Some(mut socket) = subscribe("ticker", &symbols) else {
                return true;
            };
            read(&mut socket, |text| match parse::tickers_event(text) {
                Some(t) => tx.send(t).is_ok(),
                None => true,
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

    fn trade(uid: u128, time: i64, price: f64, qty: f64) -> IdTrade {
        IdTrade {
            uid,
            trade: Trade {
                time: Time(time),
                price,
                qty,
                side: Side::Buy,
            },
        }
    }

    #[test]
    fn minutes_open_at_the_close_before_and_quiet_ones_are_flat_as_kraken_s() {
        let (tx, rx) = mpsc::channel();
        let mut minutes = Minutes {
            newest: Time(0),
            last: None,
            seen: Seen::default(),
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
        for t in [
            trade(1, 3 * MINUTE + 5, 12.0, 0.5),
            trade(2, 3 * MINUTE + 5, 11.0, 0.25),
            trade(2, 3 * MINUTE + 5, 11.0, 0.25),
        ] {
            assert!(minutes.trade(t, &tx));
        }
        let sent: Vec<Candle> = rx.try_iter().collect();
        let flat = |m: i64| (Time(m * MINUTE), 10.0, 10.0, 10.0, 10.0, 0.0);
        let row = |c: &Candle| (c.time, c.open, c.high, c.low, c.close, c.volume);
        assert_eq!(
            sent.iter().skip(1).take(2).map(row).collect::<Vec<_>>(),
            [flat(1), flat(2)]
        );
        let last = sent.last().unwrap();
        assert_eq!(
            row(last),
            (Time(3 * MINUTE), 10.0, 12.0, 10.0, 11.0, 0.75),
            "a trade had twice counts once"
        );
    }
}
