//! The live candles against a local stand-in for the API and the stream.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use tickerlight_binance::{Binance, Interval, LiveCandles, LivePrices, LiveTrades};
use tickerlight_core::{Sink, Source, Time};

const T0: i64 = 1_700_000_040_000;

fn kline_event(time: i64, close: f64) -> String {
    format!(
        r#"{{"e":"kline","E":{time},"s":"BTCUSDT","k":{{"t":{time},"T":{},"s":"BTCUSDT","i":"1m","o":"10","c":"{close}","h":"12","l":"9","v":"5","x":false}}}}"#,
        time + 59_999
    )
}

/// Answers every request with the same two candles.
fn api() -> String {
    serve(format!(
        r#"[[{T0},"10","12","9","11","5"],[{},"11","12","10","11.5","3"]]"#,
        T0 + 60_000
    ))
}

/// Answers every request with `body`.
fn serve(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

/// A stream that sends an update of the forming candle, the next candle, and one older than both, then closes.
fn stream() -> String {
    stream_of(vec![
        kline_event(T0 + 60_000, 11.8),
        kline_event(T0 + 120_000, 12.0),
        kline_event(T0, 1.0),
    ])
}

/// A stream that sends `messages` to the first connection, then closes it.
fn stream_of(messages: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(tcp).unwrap();
        for text in messages {
            socket.send(tungstenite::Message::text(text)).unwrap();
        }
        let _ = socket.close(None);
        let _ = socket.flush();
        // Later connections are left waiting.
        thread::sleep(Duration::from_secs(60));
        drop(listener);
    });
    format!("ws://{addr}")
}

#[test]
fn what_was_missed_comes_from_the_api_then_the_stream_goes_on() {
    let mut live = LiveCandles::connect_to(Binance::with_base(api()), &stream(), "BTCUSDT", Interval::M1, Time(T0));
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 4 && Instant::now() < deadline {
        let mut sink = Sink::default();
        assert!(live.poll(&mut sink));
        got.extend(sink.candles().iter().map(|c| (c.time.0 - T0, c.close)));
        thread::sleep(Duration::from_millis(10));
    }
    // The candle given comes again, the one missed after it, then the stream; the old one at the end is dropped.
    assert_eq!(got, [(0, 11.0), (60_000, 11.5), (60_000, 11.8), (120_000, 12.0)]);
}

fn trade_event(id: i64, time: i64, price: f64) -> String {
    format!(
        r#"{{"e":"aggTrade","E":{time},"s":"BTCUSDT","a":{id},"p":"{price}","q":"1","f":1,"l":1,"T":{time},"m":false}}"#
    )
}

#[test]
fn trades_missed_come_from_the_api_and_none_comes_twice() {
    // The API always has trades 1 and 2; the stream repeats 2, brings 3, and drops; after that the API is asked
    // again (from 4) and still answers 1 and 2.
    let api = serve(format!(
        r#"[{{"a":1,"p":"10","q":"1","f":1,"l":1,"T":{T0},"m":true}},{{"a":2,"p":"11","q":"2","f":2,"l":2,"T":{},"m":false}}]"#,
        T0 + 1
    ));
    let stream = stream_of(vec![trade_event(2, T0 + 1, 11.0), trade_event(3, T0 + 5, 12.0)]);
    let mut live = LiveTrades::connect_to(Binance::with_base(api), &stream, "BTCUSDT", Time(T0));
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 3 && Instant::now() < deadline {
        got.extend(live.take().into_iter().map(|t| (t.time.0 - T0, t.price)));
        thread::sleep(Duration::from_millis(10));
    }
    // A moment for anything wrongly sent again.
    thread::sleep(Duration::from_millis(1500));
    got.extend(live.take().into_iter().map(|t| (t.time.0 - T0, t.price)));
    assert_eq!(got, [(0, 10.0), (1, 11.0), (5, 12.0)]);
}

#[test]
fn prices_come_for_the_symbols_subscribed() {
    let ticker = |symbol: &str, close: f64| {
        format!(
            r#"{{"e":"24hrMiniTicker","E":{T0},"s":"{symbol}","c":"{close}","o":"100","h":"1","l":"1","v":"1","q":"1"}}"#
        )
    };
    let stream = stream_of(vec![
        r#"{"result":null,"id":1}"#.to_string(),
        ticker("BTCUSDT", 101.0),
        ticker("ETHUSDT", 99.0),
    ]);
    let mut live = LivePrices::connect_to(&stream, &["BTCUSDT".into(), "ETHUSDT".into()]);
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 2 && Instant::now() < deadline {
        got.extend(live.take().into_iter().map(|t| (t.symbol, t.last)));
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(got, [("BTCUSDT".to_string(), 101.0), ("ETHUSDT".to_string(), 99.0)]);
}
