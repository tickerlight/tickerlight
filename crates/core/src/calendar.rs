//! Just enough calendar to label a time axis: UTC dates, and the clocks of a few places to show times in.

use crate::Time;

pub const MS_PER_DAY: i64 = 86_400_000;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    /// 1..=12
    pub month: u32,
    /// 1..=31
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

impl DateTime {
    pub fn from_time(t: Time) -> Self {
        let days = t.0.div_euclid(MS_PER_DAY);
        let ms = t.0.rem_euclid(MS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (ms / 3_600_000) as u32,
            minute: (ms / 60_000 % 60) as u32,
        }
    }

    pub fn month_name(&self) -> &'static str {
        MONTHS[self.month as usize - 1]
    }

    /// Monday 0 to Sunday 6.
    pub fn weekday(&self) -> u32 {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year, self.month, self.day) + 3).rem_euclid(7) as u32
    }

    pub fn weekday_name(&self) -> &'static str {
        WEEKDAY_NAMES[self.weekday() as usize]
    }
}

const WEEKDAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// The clock times are written on, and whether their dates are hidden (a replay started blind): then the weekday
/// stands for the date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    pub zone: Zone,
    pub blind: bool,
}

impl From<Zone> for Clock {
    fn from(zone: Zone) -> Self {
        Self { zone, blind: false }
    }
}

/// A UTC moment typed as `2026-09-15` or `2026-09-15 14:00` (a `T` instead of the space works too).
pub fn parse_time(text: &str) -> Option<Time> {
    let text = text.trim();
    let (date, clock) = match text.split_once([' ', 'T']) {
        Some((date, clock)) => (date, Some(clock.trim())),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let minutes = match clock {
        None => 0,
        Some(clock) => {
            let (hour, minute) = clock.split_once(':')?;
            let (hour, minute): (i64, i64) = (hour.parse().ok()?, minute.parse().ok()?);
            if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
                return None;
            }
            hour * 60 + minute
        }
    };
    Some(Time(time_of_date(year, month, day).0 + minutes * 60_000))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    (days_from_civil(next_year, next_month, 1) - days_from_civil(year, month, 1)) as u32
}

/// The start of a UTC day.
pub fn time_of_date(year: i64, month: u32, day: u32) -> Time {
    Time(days_from_civil(year, month, day) * MS_PER_DAY)
}

/// (year, month, day) to days since 1970-01-01. Howard Hinnant's `days_from_civil`.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400); // [0, 399]
    let mp = i64::from((month + 9) % 12); // March is 0
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's `civil_from_days`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

const HOUR: i64 = 3_600_000;

/// The clock of a place, with its daylight saving: what times are shown in (UTC unless another is picked) and
/// what trading sessions are kept in. The rules are those in force since 2007 in America, 1996 in Europe and 2008
/// in Australia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zone {
    #[default]
    Utc,
    NewYork,
    Chicago,
    London,
    Berlin,
    Moscow,
    Dubai,
    Kolkata,
    Singapore,
    Tokyo,
    Sydney,
}

impl Zone {
    /// From the west of UTC to the east, UTC first.
    pub const ALL: [Zone; 11] = [
        Zone::Utc,
        Zone::NewYork,
        Zone::Chicago,
        Zone::London,
        Zone::Berlin,
        Zone::Moscow,
        Zone::Dubai,
        Zone::Kolkata,
        Zone::Singapore,
        Zone::Tokyo,
        Zone::Sydney,
    ];

    /// The names shown of [`ALL`](Self::ALL), in its order.
    pub const LABELS: [&'static str; 11] = {
        let mut labels = [""; 11];
        let mut i = 0;
        while i < labels.len() {
            labels[i] = Self::ALL[i].label();
            i += 1;
        }
        labels
    };

    /// The name shown: "UTC", "New York".
    pub const fn label(self) -> &'static str {
        match self {
            Zone::Utc => "UTC",
            Zone::NewYork => "New York",
            Zone::Chicago => "Chicago",
            Zone::London => "London",
            Zone::Berlin => "Berlin",
            Zone::Moscow => "Moscow",
            Zone::Dubai => "Dubai",
            Zone::Kolkata => "Kolkata",
            Zone::Singapore => "Singapore",
            Zone::Tokyo => "Tokyo",
            Zone::Sydney => "Sydney",
        }
    }

    /// The name in saved text: "utc", "new-york".
    pub fn name(self) -> &'static str {
        match self {
            Zone::Utc => "utc",
            Zone::NewYork => "new-york",
            Zone::Chicago => "chicago",
            Zone::London => "london",
            Zone::Berlin => "berlin",
            Zone::Moscow => "moscow",
            Zone::Dubai => "dubai",
            Zone::Kolkata => "kolkata",
            Zone::Singapore => "singapore",
            Zone::Tokyo => "tokyo",
            Zone::Sydney => "sydney",
        }
    }

    pub fn from_name(name: &str) -> Option<Zone> {
        Zone::ALL.into_iter().find(|z| z.name() == name)
    }

    /// How far its clocks are ahead of UTC at `t` (UTC milliseconds).
    pub fn offset(self, t: i64) -> i64 {
        let (year, _, _) = civil_from_days(t.div_euclid(MS_PER_DAY));
        // The moment (since 1970) of the `n`th Sunday of a month, the last for `n` = 0.
        let sunday = |month: u32, n: i64| -> i64 {
            let first = days_from_civil(year, month, 1);
            let next = if month == 12 {
                days_from_civil(year + 1, 1, 1)
            } else {
                days_from_civil(year, month + 1, 1)
            };
            // 1970-01-01 was a Thursday: Sunday is 3 days on.
            let day = if n == 0 {
                let last = next - 1;
                last - (last - 3).rem_euclid(7)
            } else {
                first + (3 - first).rem_euclid(7) + 7 * (n - 1)
            };
            day * MS_PER_DAY
        };
        // An hour ahead from 02:00 on the second Sunday of March to 02:00 on the first Sunday of November, local.
        let american = |standard: i64| {
            let (from, to) = (sunday(3, 2) + 2 * HOUR - standard, sunday(11, 1) + HOUR - standard);
            if (from..to).contains(&t) {
                standard + HOUR
            } else {
                standard
            }
        };
        // An hour ahead from 01:00 UTC on the last Sunday of March to 01:00 UTC on the last Sunday of October.
        let european = |standard: i64| {
            let (from, to) = (sunday(3, 0) + HOUR, sunday(10, 0) + HOUR);
            if (from..to).contains(&t) {
                standard + HOUR
            } else {
                standard
            }
        };
        match self {
            Zone::Utc => 0,
            Zone::NewYork => american(-5 * HOUR),
            Zone::Chicago => american(-6 * HOUR),
            Zone::London => european(0),
            Zone::Berlin => european(HOUR),
            Zone::Moscow => 3 * HOUR,
            Zone::Dubai => 4 * HOUR,
            Zone::Kolkata => 5 * HOUR + HOUR / 2,
            Zone::Singapore => 8 * HOUR,
            Zone::Tokyo => 9 * HOUR,
            // Southern summer: an hour ahead from 02:00 on the first Sunday of October to 03:00 (summer time) on the
            // first Sunday of April.
            Zone::Sydney => {
                let standard = 10 * HOUR;
                let ends = sunday(4, 1) + 2 * HOUR - standard;
                let starts = sunday(10, 1) + 2 * HOUR - standard;
                if t < ends || t >= starts {
                    standard + HOUR
                } else {
                    standard
                }
            }
        }
    }

    /// `t` (UTC) as this clock reads it, as a time to take the date and the hour of.
    pub fn local(self, t: Time) -> Time {
        Time(t.0 + self.offset(t.0))
    }

    /// The UTC moment this clock reads as `local`. A time daylight saving skips or repeats gives one of the moments
    /// an hour apart around it.
    pub fn utc(self, local: Time) -> Time {
        let guess = local.0 - self.offset(local.0);
        Time(local.0 - self.offset(guess))
    }

    /// The time to label a bar of `bar_interval_ms` with: bars of a day or longer keep their date.
    fn shown(self, t: Time, bar_interval_ms: i64) -> DateTime {
        DateTime::from_time(if bar_interval_ms >= MS_PER_DAY {
            t
        } else {
            self.local(t)
        })
    }
}

/// Label for a tick of the time axis, in the way trading charts do it: the biggest unit
/// that changed since the previous tick (year, month or day), otherwise the time of day, with its seconds when it
/// is not on a whole minute (bars of seconds) (or the day number if bars are a day or longer), read on `clock`.
/// With the dates hidden, the weekday stands for the year, month and day.
pub fn tick_label(time: Time, previous: Option<Time>, bar_interval_ms: i64, clock: impl Into<Clock>) -> String {
    let Clock { zone, blind } = clock.into();
    let now = zone.shown(time, bar_interval_ms);
    let previous = previous.map(|p| zone.shown(p, bar_interval_ms));
    if let Some(prev) = previous.filter(|_| !blind) {
        if prev.year != now.year {
            return now.year.to_string();
        }
        if prev.month != now.month {
            return now.month_name().to_string();
        }
    }
    let day_changed = previous.is_none_or(|p| p.day != now.day);
    // The zones are whole minutes off UTC: the seconds are those of UTC.
    let second = time.0.rem_euclid(60_000) / 1000;
    if (day_changed || bar_interval_ms >= MS_PER_DAY) && blind {
        now.weekday_name().to_string()
    } else if day_changed || bar_interval_ms >= MS_PER_DAY {
        now.day.to_string()
    } else if second != 0 {
        format!("{:02}:{:02}:{second:02}", now.hour, now.minute)
    } else {
        format!("{:02}:{:02}", now.hour, now.minute)
    }
}

/// Full date for the crosshair label, e.g. `14 Nov '23 22:13` (`22:13:45` on bars of seconds); only the date if bars
/// are a day or longer. Read on `clock`; with the dates hidden, the weekday instead: `Tue 22:13`.
pub fn full_label(time: Time, bar_interval_ms: i64, clock: impl Into<Clock>) -> String {
    let Clock { zone, blind } = clock.into();
    let t = zone.shown(time, bar_interval_ms);
    let date = if blind {
        t.weekday_name().to_string()
    } else {
        format!("{} {} '{:02}", t.day, t.month_name(), t.year.rem_euclid(100))
    };
    if bar_interval_ms >= MS_PER_DAY {
        date
    } else if bar_interval_ms < 60_000 {
        let second = time.0.rem_euclid(60_000) / 1000;
        format!("{date} {:02}:{:02}:{second:02}", t.hour, t.minute)
    } else {
        format!("{date} {:02}:{:02}", t.hour, t.minute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(ms: i64) -> DateTime {
        DateTime::from_time(Time(ms))
    }

    #[test]
    fn epoch() {
        assert_eq!(
            dt(0),
            DateTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0
            }
        );
    }

    #[test]
    fn known_moments() {
        // 2023-11-14 22:13:20 UTC
        assert_eq!(
            dt(1_700_000_000_000),
            DateTime {
                year: 2023,
                month: 11,
                day: 14,
                hour: 22,
                minute: 13
            }
        );
        // Leap day 2024-02-29 00:00 UTC
        assert_eq!(
            dt(1_709_164_800_000),
            DateTime {
                year: 2024,
                month: 2,
                day: 29,
                hour: 0,
                minute: 0
            }
        );
        // The next day is March 1st.
        assert_eq!(dt(1_709_164_800_000 + MS_PER_DAY).month, 3);
    }

    #[test]
    fn before_the_epoch() {
        assert_eq!(
            dt(-1),
            DateTime {
                year: 1969,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59
            }
        );
    }

    #[test]
    fn full_label_has_date_and_time_for_intraday_bars() {
        let utc = Zone::Utc;
        assert_eq!(full_label(Time(1_700_000_000_000), 60_000, utc), "14 Nov '23 22:13");
        assert_eq!(full_label(Time(1_700_000_000_000), MS_PER_DAY, utc), "14 Nov '23");
        assert_eq!(full_label(Time(0), 60_000, utc), "1 Jan '70 00:00");
    }

    #[test]
    fn labels_show_the_biggest_change() {
        let (min, utc) = (60_000, Zone::Utc);
        let day = MS_PER_DAY;
        let t = |y_days: i64, minutes: i64| Time(y_days * day + minutes * min);
        // 1970-01-01 -> 1970-01-02: the day.
        assert_eq!(tick_label(t(1, 0), Some(t(0, 0)), min, utc), "2");
        // Same day: the time.
        assert_eq!(tick_label(t(1, 90), Some(t(1, 30)), min, utc), "01:30");
        // Month change (Jan 31 -> Feb 1).
        assert_eq!(tick_label(t(31, 0), Some(t(30, 0)), min, utc), "Feb");
        // Year change.
        assert_eq!(tick_label(t(365, 0), Some(t(364, 0)), min, utc), "1971");
        // Daily bars without a change of month show the day, not "00:00".
        assert_eq!(tick_label(t(5, 0), Some(t(3, 0)), day, utc), "6");
        // No previous tick: the day.
        assert_eq!(tick_label(t(1, 90), None, min, utc), "2");
        // Bars of seconds: a tick off the whole minute has its seconds.
        let s = |seconds: i64| Time(MS_PER_DAY + seconds * 1000);
        assert_eq!(tick_label(s(75), Some(s(60)), 15_000, utc), "00:01:15");
        assert_eq!(tick_label(s(120), Some(s(105)), 15_000, utc), "00:02");
        assert_eq!(full_label(s(75), 15_000, Zone::Tokyo), "2 Jan '70 09:01:15");
    }

    #[test]
    fn clocks_change_on_their_own_days() {
        let at = |text: &str| parse_time(text).unwrap().0;
        let hours = |zone: Zone, text: &str| zone.offset(at(text)) as f64 / HOUR as f64;
        // 2026: America's summer time from 8 March to 1 November at 02:00 local, Europe's from 29 March to
        // 25 October at 01:00 UTC, Sydney's ends on 5 April at 03:00 and starts on 4 October at 02:00 local.
        for (zone, text, want) in [
            (Zone::Chicago, "2026-03-08 07:59", -6.0),
            (Zone::Chicago, "2026-03-08 08:00", -5.0),
            (Zone::Chicago, "2026-11-01 06:59", -5.0),
            (Zone::Chicago, "2026-11-01 07:00", -6.0),
            (Zone::NewYork, "2026-03-08 07:00", -4.0),
            (Zone::Berlin, "2026-03-29 00:59", 1.0),
            (Zone::Berlin, "2026-03-29 01:00", 2.0),
            (Zone::Berlin, "2026-10-25 00:59", 2.0),
            (Zone::Berlin, "2026-10-25 01:00", 1.0),
            (Zone::Sydney, "2026-01-10 00:00", 11.0),
            (Zone::Sydney, "2026-04-04 15:59", 11.0),
            (Zone::Sydney, "2026-04-04 16:00", 10.0),
            (Zone::Sydney, "2026-10-03 15:59", 10.0),
            (Zone::Sydney, "2026-10-03 16:00", 11.0),
            (Zone::Kolkata, "2026-07-01 00:00", 5.5),
            (Zone::Utc, "2026-07-01 00:00", 0.0),
        ] {
            assert_eq!(hours(zone, text), want, "{zone:?} at {text}");
        }
        // What a clock reads comes back as the moment it was.
        for zone in Zone::ALL {
            for text in ["2026-01-15 12:00", "2026-07-01 23:30", "2026-12-31 22:00"] {
                let t = Time(at(text));
                assert_eq!(zone.utc(zone.local(t)), t, "{zone:?} at {text}");
            }
            assert_eq!(Zone::from_name(zone.name()), Some(zone));
        }
    }

    #[test]
    fn times_are_read_on_the_zone_s_clock_but_days_keep_their_dates() {
        let ny = Zone::NewYork;
        let t = parse_time("2026-07-01 02:00").unwrap();
        assert_eq!(full_label(t, 60_000, ny), "30 Jun '26 22:00");
        assert_eq!(full_label(t, MS_PER_DAY, ny), "1 Jul '26");
        // Midnight passes in New York four hours after UTC's.
        let before = parse_time("2026-07-15 03:00").unwrap();
        let after = parse_time("2026-07-15 05:00").unwrap();
        assert_eq!(tick_label(after, Some(before), 60_000, ny), "15");
        assert_eq!(tick_label(after, Some(before), 60_000, Zone::Utc), "05:00");
    }

    #[test]
    fn days_and_dates_convert_both_ways() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // Every day of four centuries round-trips, leap days and century rules included.
        for days in -146_097..146_097 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        assert_eq!(time_of_date(2023, 11, 14), Time(1_699_920_000_000));
    }

    #[test]
    fn typed_times_parse_or_are_refused() {
        let day = time_of_date(2026, 9, 15).0;
        assert_eq!(parse_time("2026-09-15"), Some(Time(day)));
        assert_eq!(
            parse_time(" 2026-09-15 14:05 "),
            Some(Time(day + 14 * 3_600_000 + 5 * 60_000))
        );
        assert_eq!(parse_time("2026-9-15T00:30"), Some(Time(day + 30 * 60_000)));
        assert_eq!(parse_time("2024-02-29"), Some(time_of_date(2024, 2, 29)));
        for bad in [
            "",
            "2026-09",
            "2026-13-01",
            "2025-02-29",
            "2026-09-31",
            "2026-09-15 24:00",
            "2026-09-15 9",
            "x",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn hidden_dates_are_written_as_weekdays() {
        let blind = Clock {
            zone: Zone::Utc,
            blind: true,
        };
        let t = |day: i64, minutes: i64| Time(day * MS_PER_DAY + minutes * 60_000);
        // 1 January 1970 was a Thursday.
        assert_eq!(full_label(t(0, 90), 60_000, blind), "Thu 01:30");
        assert_eq!(full_label(t(3, 0), MS_PER_DAY, blind), "Sun");
        // A new month or year is a new day like any other; within a day, the time as always.
        assert_eq!(tick_label(t(31, 0), Some(t(30, 0)), 60_000, blind), "Sun");
        assert_eq!(tick_label(t(365, 0), Some(t(364, 0)), MS_PER_DAY, blind), "Fri");
        assert_eq!(tick_label(t(1, 90), Some(t(1, 30)), 60_000, blind), "01:30");
        // Five hours behind, Friday's first hours are still Thursday.
        let new_york = Clock {
            zone: Zone::NewYork,
            blind: true,
        };
        assert_eq!(full_label(t(1, 60), 60_000, new_york), "Thu 20:00");
    }
}
