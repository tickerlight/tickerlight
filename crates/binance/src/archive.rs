//! Binance's public data archive (`data.binance.vision`): history as zipped CSV files, one per month or per day,
//! each with a SHA-256 checksum next to it. A month's file appears a few days after the month ends, a day's file
//! the next day; anything newer comes from the API.

use sha2::{Digest, Sha256};
use tickerlight_core::{Candle, Time, Trade, calendar};

use crate::{Error, Interval, parse};

pub const DEFAULT_ARCHIVE_BASE: &str = "https://data.binance.vision";
/// The bucket behind the archive, which lists its files (the archive's own pages read the listing from it).
pub const DEFAULT_LISTING_BASE: &str = "https://s3-ap-northeast-1.amazonaws.com/data.binance.vision";

/// The biggest file we accept. Monthly trade files of busy pairs run into gigabytes; candles are a few megabytes.
const MAX_FILE: u64 = 4 << 30;
/// The biggest CSV accepted out of a candle file: a month of 1m candles is about 6 MB.
const MAX_KLINES_CSV: u64 = 64 << 20;
/// The biggest CSV accepted out of a trade file: a day of BTCUSDT's trades is 30 to 100 MB. A month of a busy
/// pair's can be more; the program asks for days.
const MAX_TRADES_CSV: u64 = 2 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    Spot,
    /// USDⓈ-margined futures.
    UsdFutures,
}

impl Market {
    /// A short name, safe for file names.
    pub fn name(self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::UsdFutures => "usd-futures",
        }
    }

    fn archive_path(self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::UsdFutures => "futures/um",
        }
    }
}

/// The span of one archive file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Period {
    Month { year: i64, month: u32 },
    Day { year: i64, month: u32, day: u32 },
}

impl Period {
    /// `2026-09` or `2026-10-16`, as in the archive's file names.
    pub fn label(self) -> String {
        match self {
            Period::Month { year, month } => format!("{year:04}-{month:02}"),
            Period::Day { year, month, day } => format!("{year:04}-{month:02}-{day:02}"),
        }
    }

    pub fn start(self) -> Time {
        match self {
            Period::Month { year, month } => calendar::time_of_date(year, month, 1),
            Period::Day { year, month, day } => calendar::time_of_date(year, month, day),
        }
    }

    /// The start of the next period: the end, exclusive.
    /// The period of a label such as `2026-09` or `2026-10-16`.
    pub fn from_label(label: &str) -> Option<Period> {
        let mut parts = label.split('-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok().filter(|m| (1..=12).contains(m))?;
        let period = match parts.next() {
            None => Period::Month { year, month },
            Some(day) => Period::Day {
                year,
                month,
                day: day.parse().ok().filter(|d| (1..=31).contains(d))?,
            },
        };
        parts.next().is_none().then_some(period)
    }

    pub fn end(self) -> Time {
        match self {
            Period::Month { year, month: 12 } => calendar::time_of_date(year + 1, 1, 1),
            Period::Month { year, month } => calendar::time_of_date(year, month + 1, 1),
            Period::Day { .. } => Time(self.start().0 + calendar::MS_PER_DAY),
        }
    }

    fn folder(self) -> &'static str {
        match self {
            Period::Month { .. } => "monthly",
            Period::Day { .. } => "daily",
        }
    }
}

pub struct Archive {
    base: String,
    listing: String,
    agent: ureq::Agent,
}

impl Default for Archive {
    fn default() -> Self {
        Self::new()
    }
}

impl Archive {
    pub fn new() -> Self {
        Self::with_bases(DEFAULT_ARCHIVE_BASE, DEFAULT_LISTING_BASE)
    }

    /// An archive whose files and listing are both at `base` (a mirror).
    pub fn with_base(base: impl Into<String>) -> Self {
        let base = base.into();
        Self::with_bases(base.clone(), base)
    }

    pub fn with_bases(base: impl Into<String>, listing: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .timeout_global(Some(std::time::Duration::from_secs(300)))
            .build()
            .into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            listing: listing.into().trim_end_matches('/').to_string(),
            agent,
        }
    }

    pub fn klines_url(&self, market: Market, symbol: &str, interval: Interval, period: Period) -> String {
        let interval = interval.as_str();
        self.url(
            market,
            period,
            &format!("klines/{symbol}/{interval}"),
            &format!("{symbol}-{interval}"),
        )
    }

    /// The file of aggregated trades of a period.
    pub fn agg_trades_url(&self, market: Market, symbol: &str, period: Period) -> String {
        self.url(
            market,
            period,
            &format!("aggTrades/{symbol}"),
            &format!("{symbol}-aggTrades"),
        )
    }

    fn url(&self, market: Market, period: Period, folder: &str, name: &str) -> String {
        format!(
            "{}/data/{}/{}/{folder}/{name}-{}.zip",
            self.base,
            market.archive_path(),
            period.folder(),
            period.label()
        )
    }

    fn klines_folder(&self, market: Market, symbol: &str, interval: Interval, folder: &str) -> String {
        format!(
            "data/{}/{folder}/klines/{symbol}/{}/",
            market.archive_path(),
            interval.as_str()
        )
    }

    /// The months the archive has candles of, oldest first. None at all: the symbol is not in this market, or
    /// it is new and has only days so far.
    pub fn kline_months(&self, market: Market, symbol: &str, interval: Interval) -> Result<Vec<Period>, Error> {
        let folder = self.klines_folder(market, symbol, interval, "monthly");
        self.periods(&folder, &format!("{symbol}-{}-", interval.as_str()))
    }

    /// The days of one month the archive has candles of, oldest first.
    pub fn kline_days(
        &self,
        market: Market,
        symbol: &str,
        interval: Interval,
        year: i64,
        month: u32,
    ) -> Result<Vec<Period>, Error> {
        let folder = self.klines_folder(market, symbol, interval, "daily");
        let name = format!("{symbol}-{}-", interval.as_str());
        self.periods(&format!("{folder}{name}{year:04}-{month:02}-"), &name)
    }

    /// The days of one month the archive has trades of, oldest first.
    pub fn trade_days(&self, market: Market, symbol: &str, year: i64, month: u32) -> Result<Vec<Period>, Error> {
        let name = format!("{symbol}-aggTrades-");
        let prefix = format!(
            "data/{}/daily/aggTrades/{symbol}/{name}{year:04}-{month:02}-",
            market.archive_path()
        );
        self.periods(&prefix, &name)
    }

    /// The periods of the zip files whose keys start with `prefix`, named `<name><label>.zip`.
    fn periods(&self, prefix: &str, name: &str) -> Result<Vec<Period>, Error> {
        let folder_end = prefix.rfind('/').map_or(0, |i| i + 1);
        let mut out: Vec<Period> = self
            .list(prefix)?
            .iter()
            .filter_map(|key| {
                key[folder_end.min(key.len())..]
                    .strip_prefix(name)?
                    .strip_suffix(".zip")
            })
            .filter_map(Period::from_label)
            .collect();
        out.sort();
        Ok(out)
    }

    /// Every key that starts with `prefix`, a page of up to a thousand at a time.
    fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let mut keys = Vec::new();
        loop {
            let marker = keys.last().map_or("", String::as_str);
            let url = format!("{}?delimiter=/&prefix={prefix}&marker={marker}", self.listing);
            let page = self
                .fetch(&url)?
                .ok_or_else(|| Error::Http(format!("no listing at {url}")))?;
            let page = String::from_utf8_lossy(&page);
            let (more, page_keys) = parse_listing(&page);
            let before = keys.len();
            keys.extend(page_keys);
            if !more || keys.len() == before {
                return Ok(keys);
            }
        }
    }

    /// The candles of one file, oldest first, or `None` when the archive has no such file: the symbol did not
    /// trade then, or the file is not out yet.
    pub fn klines(
        &self,
        market: Market,
        symbol: &str,
        interval: Interval,
        period: Period,
    ) -> Result<Option<Vec<Candle>>, Error> {
        match self.file(&self.klines_url(market, symbol, interval, period), MAX_KLINES_CSV)? {
            Some(csv) => parse::archive_klines(&csv).map(Some),
            None => Ok(None),
        }
    }

    /// The trades of one file (a day's is a few megabytes for a busy pair), oldest first, or `None` when the
    /// archive has no such file.
    pub fn agg_trades(&self, market: Market, symbol: &str, period: Period) -> Result<Option<Vec<Trade>>, Error> {
        match self.file(&self.agg_trades_url(market, symbol, period), MAX_TRADES_CSV)? {
            Some(csv) => parse::archive_agg_trades(&csv).map(Some),
            None => Ok(None),
        }
    }

    /// The text inside a zip file, checked against its checksum, of at most `cap` bytes; `None` when there is no
    /// such file.
    fn file(&self, url: &str, cap: u64) -> Result<Option<String>, Error> {
        let Some(zip) = self.fetch(url)? else {
            return Ok(None);
        };
        let checksum = self
            .fetch(&format!("{url}.CHECKSUM"))?
            .ok_or_else(|| Error::Parse(format!("no checksum for {url}")))?;
        verify(&zip, &checksum)?;
        unzip_single(&zip, cap).map(Some)
    }

    fn fetch(&self, url: &str) -> Result<Option<Vec<u8>>, Error> {
        let mut response = self.agent.get(url).call().map_err(|e| Error::Http(e.to_string()))?;
        let status = response.status();
        if status == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Error::Http(format!("status {status} for {url}")));
        }
        let body = tickerlight_core::read_capped(response.body_mut().as_reader(), MAX_FILE)
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(Some(body))
    }
}

/// The keys of a bucket listing page, and whether more pages follow.
fn parse_listing(xml: &str) -> (bool, Vec<String>) {
    let keys = xml
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>").map(|(key, _)| key.replace("&amp;", "&")))
        .collect();
    (xml.contains("<IsTruncated>true</IsTruncated>"), keys)
}

/// Checks `data` against a checksum file: the hex SHA-256 first, then the file name.
pub fn verify(data: &[u8], checksum_file: &[u8]) -> Result<(), Error> {
    let expected = std::str::from_utf8(checksum_file)
        .ok()
        .and_then(|s| s.split_whitespace().next())
        .ok_or_else(|| Error::Parse("empty checksum file".into()))?;
    let actual: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(Error::Parse(format!(
            "checksum mismatch: expected {expected}, got {actual}"
        )))
    }
}

/// The text of the only file inside an archive's zip, refused when it is over `cap` bytes: the size the zip gives
/// is checked first, and the reading stops past it all the same (the zip may lie).
pub fn unzip_single(data: &[u8], cap: u64) -> Result<String, Error> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| Error::Parse(e.to_string()))?;
    let file = zip.by_index(0).map_err(|e| Error::Parse(e.to_string()))?;
    if file.size() > cap {
        return Err(Error::Parse(format!(
            "the file in the zip is {} bytes, more than {cap}",
            file.size()
        )));
    }
    tickerlight_core::read_capped_text(file, cap).map_err(|e| Error::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_name_files_and_cover_their_span() {
        let month = Period::Month { year: 2026, month: 9 };
        assert_eq!(month.label(), "2026-09");
        assert_eq!(month.start(), calendar::time_of_date(2026, 9, 1));
        assert_eq!(month.end(), calendar::time_of_date(2026, 10, 1));
        let december = Period::Month { year: 2025, month: 12 };
        assert_eq!(december.end(), calendar::time_of_date(2026, 1, 1));
        let day = Period::Day {
            year: 2026,
            month: 10,
            day: 16,
        };
        assert_eq!(day.label(), "2026-10-16");
        assert_eq!(day.end().0 - day.start().0, calendar::MS_PER_DAY);
    }

    #[test]
    fn urls_follow_the_archive_layout() {
        let archive = Archive::with_base("https://example.test/");
        let day = Period::Day {
            year: 2026,
            month: 10,
            day: 16,
        };
        assert_eq!(
            archive.klines_url(Market::UsdFutures, "BTCUSDT", Interval::M1, day),
            "https://example.test/data/futures/um/daily/klines/BTCUSDT/1m/BTCUSDT-1m-2026-10-16.zip"
        );
        let month = Period::Month { year: 2026, month: 9 };
        assert_eq!(
            archive.klines_url(Market::Spot, "ETHUSDT", Interval::H1, month),
            "https://example.test/data/spot/monthly/klines/ETHUSDT/1h/ETHUSDT-1h-2026-09.zip"
        );
        assert_eq!(
            archive.agg_trades_url(Market::Spot, "SOLUSDT", day),
            "https://example.test/data/spot/daily/aggTrades/SOLUSDT/SOLUSDT-aggTrades-2026-10-16.zip"
        );
    }

    #[test]
    fn labels_read_back_into_periods() {
        for period in [
            Period::Month { year: 2017, month: 8 },
            Period::Day {
                year: 2026,
                month: 10,
                day: 16,
            },
        ] {
            assert_eq!(Period::from_label(&period.label()), Some(period));
        }
        for bad in ["", "2026", "2026-13", "2026-10-32", "2026-10-16-1", "x-01"] {
            assert_eq!(Period::from_label(bad), None, "{bad}");
        }
    }

    #[test]
    fn listings_give_their_keys_and_say_when_more_follow() {
        let page = "<?xml version=\"1.0\"?><ListBucketResult><Prefix>data/</Prefix><IsTruncated>true</IsTruncated>\
            <Contents><Key>data/a/X-1m-2017-08.zip</Key><Size>1</Size></Contents>\
            <Contents><Key>data/a/X-1m-2017-08.zip.CHECKSUM</Key></Contents></ListBucketResult>";
        let (more, keys) = parse_listing(page);
        assert!(more);
        assert_eq!(keys, ["data/a/X-1m-2017-08.zip", "data/a/X-1m-2017-08.zip.CHECKSUM"]);
        let (more, keys) = parse_listing("<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>");
        assert!(!more && keys.is_empty());
    }

    #[test]
    fn checksums_are_checked() {
        // SHA-256 of "abc".
        let file = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  x.zip\n";
        assert!(verify(b"abc", file).is_ok());
        assert!(verify(b"abd", file).is_err());
        assert!(verify(b"abc", b"").is_err());
    }

    #[test]
    fn the_single_file_comes_out_of_the_zip() {
        use std::io::Write;
        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("BTCUSDT-1m-2026-10-16.csv", options).unwrap();
        writer.write_all(b"1,2,3\n").unwrap();
        writer.finish().unwrap();
        assert_eq!(unzip_single(buffer.get_ref(), 6).unwrap(), "1,2,3\n");
        assert!(unzip_single(buffer.get_ref(), 5).is_err(), "more than the cap");
        assert!(unzip_single(b"not a zip", 6).is_err());
    }
}
