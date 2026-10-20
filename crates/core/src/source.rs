use crate::{Candle, Trade};

/// Where a source puts what it has received. The chart applies it after the poll.
#[derive(Default)]
pub struct Sink {
    pub(crate) candles: Vec<Candle>,
    pub(crate) trades: Vec<Trade>,
}

impl Sink {
    /// A candle: at the time of the newest one it replaces that (the forming candle), later it is appended.
    pub fn candle(&mut self, candle: Candle) {
        self.candles.push(candle);
    }

    pub fn trade(&mut self, trade: Trade) {
        self.trades.push(trade);
    }

    /// What was put in so far.
    pub fn candles(&self) -> &[Candle] {
        &self.candles
    }

    pub fn trades(&self) -> &[Trade] {
        &self.trades
    }
}

/// A feed of new data (a WebSocket, a file being replayed, a simulator). Data sources are plugins:
/// the core downloads nothing itself. Add one with [`Chart::add_source`](crate::Chart::add_source); the
/// chart polls it on every tick, on the thread it lives on, so `poll` must not block: a socket runs on
/// its own thread and `poll` only drains a channel.
pub trait Source {
    /// Hands over what arrived since the last call. Returns `false` when the source is finished (or
    /// dead), so the chart stops asking for ticks on its account.
    fn poll(&mut self, sink: &mut Sink) -> bool;
}
