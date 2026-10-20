use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use tickerlight_core::source::{Sink, Source};
use tickerlight_core::{Candle, Time};

use crate::{Dukascopy, Instrument};

/// How often the datafeed is asked whether another hour is out.
const EVERY: Duration = Duration::from_secs(60);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The minutes after a given one, as the hours of ticks come out: asked for once a minute on a thread. There is
/// no stream, so the chart is behind by up to an hour and a bit.
pub struct LiveCandles {
    rx: Receiver<Vec<Candle>>,
}

impl LiveCandles {
    pub fn connect(instrument: Instrument, newest: Time) -> Self {
        Self::connect_to(Dukascopy::new(), instrument, newest)
    }

    pub fn connect_to(api: Dukascopy, instrument: Instrument, newest: Time) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut newest = newest;
            loop {
                // A failure is tried again the next time; the chart keeps what it has.
                let came: Vec<Candle> = api
                    .recent(&instrument, Time(newest.0 + 60_000), Time(now_ms()))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.time > newest)
                    .collect();
                if let Some(last) = came.last() {
                    newest = last.time;
                }
                // Sent even when empty: a send that fails says the chart is gone.
                if tx.send(came).is_err() {
                    return;
                }
                thread::sleep(EVERY);
            }
        });
        Self { rx }
    }
}

impl Source for LiveCandles {
    fn poll(&mut self, sink: &mut Sink) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(candles) => candles.into_iter().for_each(|c| sink.candle(c)),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }
}
