//! Where the axis ticks are and what they say. Shared by the grid and the axes.

use crate::calendar::{Clock, MS_PER_DAY, Zone, civil_from_days, tick_label, time_of_date};
use crate::price_scale::PriceMode;
use crate::price_scale::nice_step;
use crate::{PriceScale, Time, TimeScale};

/// Price distance between axis ticks: about one every 60 logical pixels.
pub fn price_step(price: &PriceScale, plot_h: f32, scale: f32) -> f64 {
    let (low, high) = price.price_range();
    nice_step(
        (price.display(high) - price.display(low)).abs(),
        (plot_h / (60.0 * scale)) as f64,
    )
}

/// Digits after the decimal point that make prices `step` apart tell each other apart.
pub fn decimals_for_step(step: f64) -> usize {
    if step >= 1.0 {
        0
    } else {
        (-step.log10()).ceil().clamp(0.0, 10.0) as usize
    }
}

/// A value of the price axis as its label says it: the price, or a percentage in percent mode.
pub fn format_price(scale: &PriceScale, value: f64, decimals: usize) -> String {
    match scale.mode {
        PriceMode::Percent { .. } => format!("{value:.decimals$}%"),
        _ => format!("{value:.decimals$}"),
    }
}

/// The ticks of the price axis: `(value, y)` from the lowest to the highest, where the value is what
/// the axis shows (a price, or a percentage), and the number of decimals its labels need.
pub fn price_ticks(price: &PriceScale, plot_h: f32, scale: f32) -> (Vec<(f64, f32)>, usize) {
    let (low, high) = price.price_range();
    let target = (plot_h / (60.0 * scale)) as f64;
    if price.mode == PriceMode::Log && high / low >= 3.0 {
        return log_ticks(price, low, high, target);
    }
    let (a, b) = (price.display(low), price.display(high));
    let step = price_step(price, plot_h, scale);
    let (first, last) = ((a.min(b) / step).ceil() as i64, (a.max(b) / step).floor() as i64);
    let ticks = (first..=last)
        .map(|i| {
            let value = i as f64 * step;
            (value, price.y_of(price.from_display(value)).round() as f32)
        })
        .collect();
    (ticks, decimals_for_step(step))
}

/// Ticks of a log axis spanning several times its low end: 1, 2 and 5 times powers of ten, fewer of them
/// when the axis is short or spans many decades.
fn log_ticks(price: &PriceScale, low: f64, high: f64, target: f64) -> (Vec<(f64, f32)>, usize) {
    let (first_decade, last_decade) = (low.log10().floor() as i32, high.log10().ceil() as i32);
    let candidates = |mantissas: &[f64], every: i32| -> Vec<f64> {
        (first_decade..=last_decade)
            .filter(|d| d.rem_euclid(every) == 0)
            .flat_map(|d| mantissas.iter().map(move |m| m * 10f64.powi(d)))
            .filter(|p| *p >= low && *p <= high)
            .collect()
    };
    let mut values = candidates(&[1.0, 2.0, 5.0], 1);
    let mut every = 1;
    if values.len() as f64 > target * 1.5 {
        values = candidates(&[1.0], 1);
    }
    while values.len() as f64 > target * 1.5 && every < 100 {
        every += 1;
        values = candidates(&[1.0], every);
    }
    let decimals = values.iter().copied().map(decimals_for_step).max().unwrap_or(0);
    let ticks = values.into_iter().map(|p| (p, price.y_of(p).round() as f32)).collect();
    (ticks, decimals)
}

/// A vertical tick of the time axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeTick {
    pub x: f32,
    /// The time of the bar it sits on (beyond the newest bar, of the bar that would be there).
    pub time: Time,
    /// The time of the bar before: the label says the biggest unit that changed since.
    pub previous: Time,
    /// The time between the bars there.
    pub interval: i64,
    /// The length of the unit whose start it marks: under a minute, its label has the seconds.
    pub unit: i64,
}

/// A unit of time the axis marks the starts of, on the clock the times are read on.
#[derive(Debug, Clone, Copy)]
enum Unit {
    Years(i64),
    Months(u32),
    /// Days of the month that are `1 + k * n`.
    Days(u32),
    Millis(i64),
}

impl Unit {
    /// About how long it is.
    fn length(self) -> f64 {
        let day = MS_PER_DAY as f64;
        match self {
            Unit::Years(k) => k as f64 * 365.25 * day,
            Unit::Months(k) => k as f64 * 30.44 * day,
            Unit::Days(k) => k as f64 * day,
            Unit::Millis(ms) => ms as f64,
        }
    }

    /// The first start of the unit at or after the local time `t`.
    fn start_from(self, t: i64) -> i64 {
        let day = t.div_euclid(MS_PER_DAY) + i64::from(t.rem_euclid(MS_PER_DAY) != 0);
        match self {
            Unit::Millis(ms) => t.div_euclid(ms) * ms + if t.rem_euclid(ms) != 0 { ms } else { 0 },
            Unit::Days(k) => {
                (day..)
                    .find(|&d| (civil_from_days(d).2 - 1).is_multiple_of(k))
                    .expect("a month has a first day")
                    * MS_PER_DAY
            }
            Unit::Months(k) => {
                let (mut year, mut month, first) = civil_from_days(day);
                if first != 1 {
                    (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
                }
                while (month - 1) % k != 0 {
                    (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
                }
                time_of_date(year, month, 1).0
            }
            Unit::Years(k) => {
                let (year, month, first) = civil_from_days(day);
                let year = if (month, first) == (1, 1) { year } else { year + 1 };
                let year = year + (k - year.rem_euclid(k)) % k;
                time_of_date(year, 1, 1).0
            }
        }
    }
}

/// The units the axis marks, the biggest first.
const UNITS: [Unit; 23] = {
    use Unit::*;
    const S: i64 = 1000;
    [
        Years(10),
        Years(5),
        Years(1),
        Months(6),
        Months(3),
        Months(1),
        Days(15),
        Days(10),
        Days(5),
        Days(3),
        Days(2),
        Millis(MS_PER_DAY),
        Millis(12 * 3600 * S),
        Millis(6 * 3600 * S),
        Millis(3 * 3600 * S),
        Millis(3600 * S),
        Millis(30 * 60 * S),
        Millis(15 * 60 * S),
        Millis(5 * 60 * S),
        Millis(60 * S),
        Millis(30 * S),
        Millis(15 * S),
        Millis(5 * S),
    ]
};

/// Logical pixels between time ticks at the least.
const TIME_TICK_GAP: f64 = 72.0;

/// The time ticks across the plot, as trading charts place them: where the bars cross into a new year, month,
/// day, hour or minute, the biggest units first, at least [`TIME_TICK_GAP`] apart. They stay where they are while
/// the chart pans. Beyond the newest bar the times go on at its interval (before the oldest, back at its).
pub fn time_ticks(time: &TimeScale, times: &[Time], zone: Zone, scale: f32) -> Vec<TimeTick> {
    let mut ticks = Vec::new();
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else {
        return ticks;
    };
    let n = times.len() as i64;
    let (step_first, step_last) = (
        bar_interval_ms(times, 0).max(1),
        bar_interval_ms(times, times.len() - 1).max(1),
    );
    let at = |i: i64| match i {
        _ if i < 0 => Time(first.0 + i * step_first),
        _ if i >= n => Time(last.0 + (i - n + 1) * step_last),
        _ => times[i as usize],
    };
    // The first bar at or after `t`.
    let index = |t: Time| {
        if t < first {
            -((first.0 - t.0) / step_first)
        } else if t > last {
            n - 1 + (t.0 - last.0 + step_last - 1) / step_last
        } else {
            times.partition_point(|&x| x < t) as i64
        }
    };
    let gap = (TIME_TICK_GAP * scale as f64 / time.bar_spacing).ceil().max(1.0) as i64;
    // Ticks just off the plot keep those on it apart from them, so none comes and goes as the chart pans.
    let lo = time.index_of(0.0).floor() as i64 - gap;
    let hi = time.index_of(time.width).floor() as i64 + gap;
    // Bars of a day or longer keep their dates.
    let zone = if step_last >= MS_PER_DAY { Zone::Utc } else { zone };
    let (from, to) = (zone.local(at(lo)).0, zone.local(at(hi)).0);
    let bars_per_ms = (hi - lo) as f64 / (to - from).max(1) as f64;
    // Bar indexes and the length of the unit each marks.
    let mut marks: Vec<(i64, i64)> = Vec::new();
    // A unit whose starts come closer than the gap would be marked unevenly, and the smaller ones more so.
    let fits = |unit: Unit| unit.length() * bars_per_ms >= gap as f64;
    let finest = UNITS.iter().take_while(|&&u| fits(u)).count();
    // The days of a month do not nest (5 is no step of 3): of those steps, only the smallest that fits.
    let days = (UNITS[..finest].iter()).rposition(|u| matches!(u, Unit::Days(_) | Unit::Millis(MS_PER_DAY)));
    for (k, &unit) in UNITS[..finest].iter().enumerate() {
        if matches!(unit, Unit::Days(_)) && Some(k) != days {
            continue;
        }
        let mut start = unit.start_from(from);
        while start <= to {
            let i = index(zone.utc(Time(start)));
            let at = marks.partition_point(|&(m, _)| m < i);
            let apart = (at == 0 || i - marks[at - 1].0 >= gap) && marks.get(at).is_none_or(|&(m, _)| m - i >= gap);
            if apart {
                marks.insert(at, (i, unit.length() as i64));
            }
            start = unit.start_from(start + 1);
        }
    }
    for (i, unit) in marks {
        let x = time.x_of(i as f64 + 0.5).round();
        if (0.0..=time.width).contains(&x) {
            let interval = if (0..n).contains(&i) {
                bar_interval_ms(times, i as usize)
            } else {
                at(i + 1).0 - at(i).0
            };
            ticks.push(TimeTick {
                x: x as f32,
                time: at(i),
                previous: at(i - 1),
                interval,
                unit,
            });
        }
    }
    ticks
}

/// The label of a time tick, read on `clock`: the seconds only where it marks a unit shorter than a minute.
pub fn time_label(tick: &TimeTick, clock: Clock) -> String {
    let time = match tick.unit < 60_000 {
        true => tick.time,
        false => Time(tick.time.0 - tick.time.0.rem_euclid(60_000)),
    };
    tick_label(time, Some(tick.previous), tick.interval, clock)
}

/// Time between bar `i` and its neighbour.
pub fn bar_interval_ms(times: &[Time], i: usize) -> i64 {
    if i + 1 < times.len() {
        times[i + 1].0 - times[i].0
    } else if i > 0 {
        times[i].0 - times[i - 1].0
    } else {
        60_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_follow_the_price_step() {
        assert_eq!(decimals_for_step(10.0), 0);
        assert_eq!(decimals_for_step(1.0), 0);
        assert_eq!(decimals_for_step(0.5), 1);
        assert_eq!(decimals_for_step(0.05), 2);
        assert_eq!(decimals_for_step(0.0002), 4);
    }

    #[test]
    fn price_ticks_are_round_and_inside_the_scale() {
        let price = PriceScale::linear(97.3, 112.9, 500.0);
        let (ticks, decimals) = price_ticks(&price, 500.0, 1.0);
        assert!(ticks.len() >= 4);
        assert_eq!(decimals, 0);
        for &(p, y) in &ticks {
            assert!(p >= price.min && p <= price.max);
            assert_eq!(p, p.round());
            assert!((0.0..=500.0).contains(&y));
        }
        assert!(ticks.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 < w[0].1));
    }

    #[test]
    fn log_ticks_are_ones_twos_and_fives_and_equally_spaced_by_ratio() {
        let price = PriceScale::fit_mode((1.0, 10_000.0), 600.0, 0.0, PriceMode::Log);
        let (ticks, decimals) = price_ticks(&price, 600.0, 1.0);
        assert_eq!(decimals, 0);
        let values: Vec<f64> = ticks.iter().map(|t| t.0).collect();
        assert!(
            values.contains(&1.0) && values.contains(&10.0) && values.contains(&1000.0) && values.contains(&5000.0)
        );
        assert!(
            values
                .iter()
                .all(|v| [1.0, 2.0, 5.0].contains(&(v / 10f64.powf(v.log10().floor())))),
            "{values:?}"
        );
        // 10 -> 100 and 100 -> 1000 are the same distance.
        let y = |v: f64| price.y_of(v);
        assert!(((y(10.0) - y(100.0)) - (y(100.0) - y(1000.0))).abs() < 1e-6);
        // A short axis keeps only the decades.
        let (short, _) = price_ticks(&price, 120.0, 1.0);
        assert!(short.iter().all(|t| t.0.log10().fract().abs() < 1e-9), "{short:?}");
        // Under three times the low end a log axis is close enough to linear.
        let narrow = PriceScale::fit_mode((100.0, 150.0), 500.0, 0.0, PriceMode::Log);
        let (ticks, _) = price_ticks(&narrow, 500.0, 1.0);
        assert!(ticks.len() >= 4 && ticks.windows(2).all(|w| w[1].0 - w[0].0 == ticks[1].0 - ticks[0].0));
    }

    #[test]
    fn percent_ticks_are_round_percentages_placed_at_their_prices() {
        let price = PriceScale::fit_mode((90.0, 120.0), 500.0, 0.0, PriceMode::Percent { base: 100.0 });
        let (ticks, decimals) = price_ticks(&price, 500.0, 1.0);
        assert!(ticks.iter().any(|t| t.0 == 0.0));
        let zero = ticks.iter().find(|t| t.0 == 0.0).unwrap();
        assert_eq!(zero.1, price.y_of(100.0).round() as f32, "0% sits at the base price");
        assert_eq!(format_price(&price, 5.0, decimals), format!("{:.decimals$}%", 5.0));
        assert_eq!(format_price(&PriceScale::linear(0.0, 1.0, 1.0), 5.0, 1), "5.0");
    }

    const MINUTE: i64 = 60_000;

    /// 1-minute bars from 05:31 on 1 Jan 2026, an hour and a half missing in the middle.
    fn minutes() -> Vec<Time> {
        let start = time_of_date(2026, 1, 1).0 + (5 * 60 + 31) * MINUTE;
        (0..1000)
            .filter(|i| !(400..490).contains(i))
            .map(|i| Time(start + i * MINUTE))
            .collect()
    }

    #[test]
    fn time_ticks_mark_round_times_apart_and_stay_put_while_the_chart_pans() {
        let times = minutes();
        for right in [380.0, 450.0, 905.0, 940.0] {
            let time = TimeScale::new(800.0, 8.0, right);
            let ticks = time_ticks(&time, &times, Zone::Utc, 1.0);
            assert!(ticks.len() >= 4, "{ticks:?}");
            for tick in &ticks {
                // On a quarter hour, or the first bar after the hole.
                let quarter = |t: Time| t.0.div_euclid(15 * MINUTE);
                assert!(
                    tick.time.0 % (15 * MINUTE) == 0 || tick.previous.0 + MINUTE < tick.time.0,
                    "{tick:?}"
                );
                assert_ne!(quarter(tick.previous), quarter(tick.time), "{right}: {tick:?}");
            }
            assert!(ticks.windows(2).all(|w| w[1].x - w[0].x >= 72.0), "{ticks:?}");
            // Seven bars further on, the same times sit seven bars to the left.
            let panned = time_ticks(&TimeScale::new(800.0, 8.0, right + 7.0), &times, Zone::Utc, 1.0);
            for tick in ticks.iter().filter(|t| t.x >= 56.0) {
                let moved = panned.iter().find(|p| p.time == tick.time).expect("still there");
                assert_eq!(moved.x, tick.x - 56.0);
            }
        }
    }

    #[test]
    fn the_times_go_on_past_the_newest_bar() {
        let times = minutes();
        let last = *times.last().unwrap();
        let time = TimeScale::new(800.0, 8.0, times.len() as f64 + 60.0);
        let ticks = time_ticks(&time, &times, Zone::Utc, 1.0);
        let future = ticks.iter().find(|t| t.time > last).expect("a tick in the margin");
        let bars_on = (future.time.0 - last.0) / MINUTE;
        let x = time.x_of(times.len() as f64 - 1.0 + bars_on as f64 + 0.5).round() as f32;
        assert_eq!(future.x, x);
        assert_eq!(time_label(future, Zone::Utc.into()).len(), 5, "a time of day");
    }

    #[test]
    fn daily_bars_are_marked_at_the_months_and_the_years() {
        let start = time_of_date(2025, 3, 7).0;
        let times: Vec<Time> = (0..500).map(|i| Time(start + i * MS_PER_DAY)).collect();
        let time = TimeScale::new(900.0, 3.0, 500.0);
        let ticks = time_ticks(&time, &times, Zone::NewYork, 1.0);
        assert!(ticks.len() >= 5, "{ticks:?}");
        let labels: Vec<String> = ticks.iter().map(|t| time_label(t, Zone::NewYork.into())).collect();
        for (tick, label) in ticks.iter().zip(&labels) {
            assert_eq!(civil_from_days(tick.time.0 / MS_PER_DAY).2, 1, "{label}");
        }
        assert!(labels.contains(&"2026".to_string()), "{labels:?}");
        assert!(
            labels.contains(&"Feb".to_string()) || labels.contains(&"Mar".to_string()),
            "{labels:?}"
        );
    }

    #[test]
    fn time_ticks_do_not_depend_on_the_scale_factor() {
        let times = minutes();
        let one = time_ticks(&TimeScale::new(800.0, 8.0, 1000.0), &times, Zone::Utc, 1.0);
        let two = time_ticks(&TimeScale::new(1600.0, 16.0, 1000.0), &times, Zone::Utc, 2.0);
        assert_eq!(
            one.iter().map(|t| t.time).collect::<Vec<_>>(),
            two.iter().map(|t| t.time).collect::<Vec<_>>()
        );
    }
}
