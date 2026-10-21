use std::io::{self, Read};

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

/// All of `reader` if it is at most `cap` bytes, an error if it is more. For what a data source reads from a
/// server, after it is decompressed: a server or an archive that sends more than any real answer cannot take the
/// memory, and an answer cut at the cap is never taken for the whole.
pub fn read_capped(reader: impl Read, cap: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    reader.take(cap.saturating_add(1)).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("more than {cap} bytes"),
        ));
    }
    Ok(out)
}

/// [`read_capped`] as UTF-8 text.
pub fn read_capped_text(reader: impl Read, cap: u64) -> io::Result<String> {
    String::from_utf8(read_capped(reader, cap)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_than_the_cap_is_an_error_not_a_cut() {
        assert_eq!(read_capped(&b"abcd"[..], 4).unwrap(), b"abcd");
        assert!(read_capped(&b"abcde"[..], 4).is_err());
        assert!(read_capped_text(&[0xff][..], 4).is_err());
    }
}
