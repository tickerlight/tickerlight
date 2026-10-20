//! The desktop program: manual backtesting on market data kept in a local cache, brought up to date from the
//! public archives of Binance, OKX and Dukascopy, or imported from files.

pub mod dukascopy;
pub mod flow;
pub mod import;
pub mod program;
pub mod session;
pub mod sessions_page;
pub mod snapshots;
pub mod store;
pub mod sync;
pub mod watchlist;
