//! Live streams over WebSocket, as chart [`Source`]s. Native only: each stream runs a thread that
//! reads the socket and hands messages to the chart through a channel, so polling never blocks.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use tickerlight_core::{Candle, Sink, Source, Time, Trade};

use crate::{Binance, Interval, Market, Since, parse};

/// Binance's market-data mirror for streams.
pub const DEFAULT_STREAM_BASE: &str = "wss://data-stream.binance.vision/ws";
/// The streams of USD-M futures.
pub const FUTURES_STREAM_BASE: &str = "wss://fstream.binance.com/ws";

/// The stream address of a market.
pub fn stream_base(market: Market) -> &'static str {
    match market {
        Market::Spot => DEFAULT_STREAM_BASE,
        Market::UsdFutures => FUTURES_STREAM_BASE,
    }
}

/// Reads `url` forever, reconnecting after failures, and sends whatever `parse_message` makes of
/// the messages. Stops when the receiving side is gone.
fn spawn_reader<T: Send + 'static>(
    url: String,
    parse_message: fn(&str) -> Result<Option<T>, crate::Error>,
) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut backoff = Duration::from_millis(500);
        loop {
            if let Ok((mut socket, _)) = tungstenite::connect(url.as_str()) {
                backoff = Duration::from_millis(500);
                // Ends on a read error (the connection dropped); pings are answered by the library.
                while let Ok(message) = socket.read() {
                    if let tungstenite::Message::Text(text) = message
                        // A message we cannot parse is skipped: one odd message must not end the feed.
                        && let Ok(Some(item)) = parse_message(text.as_str())
                        && tx.send(item).is_err()
                    {
                        return; // the chart is gone
                    }
                }
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    });
    rx
}

/// The forming candle of `symbol` at `interval`, updated live.
pub struct KlineStream {
    rx: Receiver<Candle>,
}

impl KlineStream {
    pub fn connect(symbol: &str, interval: Interval) -> Self {
        Self::connect_to(DEFAULT_STREAM_BASE, symbol, interval)
    }

    pub fn connect_to(base: &str, symbol: &str, interval: Interval) -> Self {
        let url = format!("{base}/{}@kline_{}", symbol.to_lowercase(), interval.as_str());
        Self {
            rx: spawn_reader(url, parse::kline_event),
        }
    }
}

impl Source for KlineStream {
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

/// The candles of `symbol` from a given one on, live: first those missed since it, from the API, then the stream;
/// and so again after every reconnection, so a dropped connection leaves no hole. The candle of the time given
/// comes again too (it was forming).
pub struct LiveCandles {
    rx: Receiver<Candle>,
}

impl LiveCandles {
    /// Follows `symbol` from the candle opening at `newest`, at Binance's usual addresses.
    pub fn connect(market: Market, symbol: &str, interval: Interval, newest: Time) -> Self {
        Self::connect_to(
            Binance::for_market(market),
            stream_base(market),
            symbol,
            interval,
            newest,
        )
    }

    pub fn connect_to(api: Binance, stream: &str, symbol: &str, interval: Interval, newest: Time) -> Self {
        let url = format!("{stream}/{}@kline_{}", symbol.to_lowercase(), interval.as_str());
        let symbol = symbol.to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut newest = newest;
            let mut backoff = Duration::from_millis(500);
            // Sends what is not older than the newest sent; false when the chart is gone.
            let send = |candle: Candle, newest: &mut Time| {
                if candle.time < *newest {
                    return true;
                }
                *newest = candle.time;
                tx.send(candle).is_ok()
            };
            loop {
                let now = Time(now_ms() + interval.millis());
                // No network or the API refuses: tried again later, quietly; the chart keeps what it has.
                if let Ok(missed) = api.klines(&symbol, interval, newest, now) {
                    let view = missed.view();
                    for i in 0..view.len() {
                        if let Some(c) = view.get(i)
                            && !send(c, &mut newest)
                        {
                            return;
                        }
                    }
                    if let Ok((mut socket, _)) = tungstenite::connect(url.as_str()) {
                        backoff = Duration::from_millis(500);
                        while let Ok(message) = socket.read() {
                            if let tungstenite::Message::Text(text) = message
                                && let Ok(Some(c)) = parse::kline_event(text.as_str())
                                && !send(c, &mut newest)
                            {
                                return;
                            }
                        }
                    }
                }
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        Self { rx }
    }
}

/// Every aggregated trade of `symbol` from a moment on: what was missed comes from the API, then the stream
/// goes on, and after a dropped connection the API again from the last trade had, so none is missed or had
/// twice.
pub struct LiveTrades {
    rx: Receiver<Trade>,
}

impl LiveTrades {
    pub fn connect(market: Market, symbol: &str, from: Time) -> Self {
        Self::connect_to(Binance::for_market(market), stream_base(market), symbol, from)
    }

    pub fn connect_to(api: Binance, stream: &str, symbol: &str, from: Time) -> Self {
        let url = format!("{stream}/{}@aggTrade", symbol.to_lowercase());
        let symbol = symbol.to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut last: Option<i64> = None;
            let mut backoff = Duration::from_millis(500);
            // Sends a trade not had yet; false when the chart is gone.
            let send = |t: parse::IdTrade, last: &mut Option<i64>| {
                if last.is_some_and(|l| t.id <= l) {
                    return true;
                }
                *last = Some(t.id);
                tx.send(t.trade).is_ok()
            };
            loop {
                let since = last.map_or(Since::Time(from), |id| Since::Id(id + 1));
                let mut open = true;
                let caught_up = api.agg_trades_since(&symbol, since, |t| {
                    open = send(t, &mut last);
                    open
                });
                if !open {
                    return;
                }
                if caught_up.is_ok()
                    && let Ok((mut socket, _)) = tungstenite::connect(url.as_str())
                {
                    backoff = Duration::from_millis(500);
                    while let Ok(message) = socket.read() {
                        if let tungstenite::Message::Text(text) = message
                            && let Ok(Some(t)) = parse::agg_trade_event_id(text.as_str())
                            && !send(t, &mut last)
                        {
                            return;
                        }
                    }
                }
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        Self { rx }
    }

    /// The trades that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<Trade> {
        self.rx.try_iter().collect()
    }
}

/// The last prices of a few symbols, live: one connection subscribed to the `miniTicker` stream of each, about
/// once a second for a symbol that trades.
pub struct LivePrices {
    rx: Receiver<parse::Ticker>,
}

impl LivePrices {
    pub fn connect(market: Market, symbols: &[String]) -> Self {
        Self::connect_to(stream_base(market), symbols)
    }

    pub fn connect_to(stream: &str, symbols: &[String]) -> Self {
        let url = stream.to_string();
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@miniTicker", s.to_lowercase()))
            .collect();
        let subscribe = serde_json::json!({ "method": "SUBSCRIBE", "params": streams, "id": 1 }).to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut backoff = Duration::from_millis(500);
            loop {
                if let Ok((mut socket, _)) = tungstenite::connect(url.as_str())
                    && socket.send(tungstenite::Message::text(subscribe.clone())).is_ok()
                {
                    backoff = Duration::from_millis(500);
                    while let Ok(message) = socket.read() {
                        if let tungstenite::Message::Text(text) = message
                            && let Ok(Some(ticker)) = parse::mini_ticker_event(text.as_str())
                            && tx.send(ticker).is_err()
                        {
                            return;
                        }
                    }
                }
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        Self { rx }
    }

    /// The prices that came since the last call, oldest first.
    pub fn take(&mut self) -> Vec<parse::Ticker> {
        self.rx.try_iter().collect()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
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

/// Every aggregated trade of `symbol`, live.
pub struct TradeStream {
    rx: Receiver<Trade>,
}

impl TradeStream {
    pub fn connect(symbol: &str) -> Self {
        Self::connect_to(DEFAULT_STREAM_BASE, symbol)
    }

    pub fn connect_to(base: &str, symbol: &str) -> Self {
        let url = format!("{base}/{}@aggTrade", symbol.to_lowercase());
        Self {
            rx: spawn_reader(url, parse::agg_trade_event),
        }
    }
}

impl Source for TradeStream {
    fn poll(&mut self, sink: &mut Sink) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(trade) => sink.trade(trade),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }
}
