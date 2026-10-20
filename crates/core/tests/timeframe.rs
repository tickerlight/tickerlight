//! Higher timeframes: the chart shows coarser candles built from the data, and the replay and the
//! virtual trading keep working on the data as it came.

use tickerlight_core::builtin::Account;
use tickerlight_core::plugin::commands;
use tickerlight_core::{Candle, Candles, Chart, Event, Key, Scene, Time};

const MIN: i64 = 60_000;

/// One minute bars; bar `i` opens at `100 + i`, spans one unit each way and has volume 1.
fn candles(n: i64) -> Candles {
    let mut c = Candles::new();
    for i in 0..n {
        let p = 100.0 + i as f64;
        c.push(Candle {
            time: Time(i * MIN),
            open: p,
            high: p + 0.5,
            low: p - 0.5,
            close: p,
            volume: 1.0,
        })
        .unwrap();
    }
    c
}

fn chart(n: i64) -> Chart {
    let mut chart = Chart::new(candles(n));
    chart.resize(872.0, 528.0);
    chart.build_scene(&mut Scene::new());
    chart
}

fn press(c: &mut Chart, key: Key) {
    c.handle(Event::Key(key));
}

fn next_timeframe(c: &mut Chart) {
    c.handle(Event::Command(commands::TIMEFRAME_CYCLE));
}

#[test]
fn the_timeframe_command_cycles_through_the_timeframes_the_data_allows_and_back() {
    let mut c = chart(600);
    assert_eq!(c.timeframe(), None);
    next_timeframe(&mut c);
    assert_eq!(c.timeframe(), Some(3 * MIN));
    assert_eq!(c.seen().len(), 200);
    next_timeframe(&mut c);
    assert_eq!(c.timeframe(), Some(5 * MIN));
    assert_eq!(c.seen().len(), 120);
    assert_eq!(c.seen().volume()[0], 5.0, "volumes add up");
    for _ in 0..20 {
        next_timeframe(&mut c);
    }
    assert_eq!(c.timeframe(), None, "after the highest one comes the data as it came");
    assert_eq!(c.seen().len(), 600);
}

#[test]
fn a_chart_at_the_live_edge_stays_there_and_one_in_history_keeps_its_moment() {
    let mut c = chart(600);
    let room = c.time_scale().right - 600.0;
    c.set_timeframe(Some(5 * MIN));
    assert_eq!(c.time_scale().right, 120.0 + room);
    // Scroll back into history, then change the timeframe: the right edge stays at that moment.
    c.set_timeframe(None);
    for _ in 0..10 {
        c.handle(Event::Wheel { x: 400.0, delta: 1.0 }); // zoom in around x
    }
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    c.handle(Event::PointerDown);
    for i in 1..=30 {
        c.handle(Event::PointerMoved {
            x: 400.0 + i as f32 * 20.0,
            y: 200.0,
        });
    }
    c.handle(Event::PointerUp);
    c.handle(Event::PointerLeft);
    let edge_time = |c: &Chart| {
        let view = c.seen();
        let i = (c.time_scale().right.floor() as usize).clamp(1, view.len()) - 1;
        view.time()[i].0
    };
    let before = edge_time(&c);
    assert!(before < 550 * MIN, "scrolled back into history: {before}");
    c.set_timeframe(Some(15 * MIN));
    let after = edge_time(&c);
    assert!(
        after <= before && before - after < 15 * MIN,
        "{before} -> {after}: the period that holds the moment"
    );
}

#[test]
fn in_a_replay_the_newest_candle_is_forming_and_holds_only_what_was_shown() {
    let mut c = chart(600);
    c.set_timeframe(Some(5 * MIN));
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    press(&mut c, Key::Alt('P'));
    c.handle(Event::PointerLeft);
    let cursor = c.cursor().expect("replaying").0;
    assert_eq!(
        cursor.rem_euclid(5 * MIN),
        4 * MIN,
        "the bar under the pointer is complete: its last minute is the cursor"
    );
    let view = c.seen();
    let last = view.len() - 1;
    assert_eq!(view.volume()[last], 5.0);
    assert_eq!(*view.time().last().unwrap(), Time(cursor - 4 * MIN));

    // One step reveals one candle of the chart: its five minutes, and nothing after them.
    press(&mut c, Key::SelectRight);
    let view = c.seen();
    assert_eq!(view.len(), last + 2);
    assert_eq!(view.volume()[last + 1], 5.0);
    assert_eq!(c.cursor().unwrap().0, cursor + 5 * MIN);
    let minute = c.cursor().unwrap().0;
    assert_eq!(
        view.high()[last + 1],
        100.0 + (minute / MIN) as f64 + 0.5,
        "nothing of the future in it"
    );
    assert_eq!(view.close()[last + 1], 100.0 + (minute / MIN) as f64);

    // On the minutes a step is a minute, and back on five minutes the candle it began is forming.
    c.set_timeframe(None);
    press(&mut c, Key::SelectRight);
    c.set_timeframe(Some(5 * MIN));
    let view = c.seen();
    assert_eq!(view.len(), last + 3);
    assert_eq!(view.volume()[last + 2], 1.0);
    // The next step finishes it.
    press(&mut c, Key::SelectRight);
    let view = c.seen();
    assert_eq!(view.len(), last + 3);
    assert_eq!(view.volume()[last + 2], 5.0);
}

#[test]
fn changing_the_timeframe_during_a_replay_shows_nothing_beyond_the_cursor() {
    let mut c = chart(600);
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    press(&mut c, Key::Alt('P'));
    c.handle(Event::PointerLeft);
    for _ in 0..2 {
        press(&mut c, Key::SelectRight);
    }
    let cursor = c.cursor().unwrap().0;
    for _ in 0..3 {
        next_timeframe(&mut c);
        let view = c.seen();
        let span = c.timeframe().unwrap();
        let last = view.len() - 1;
        assert_eq!(view.time()[last].0, cursor.div_euclid(span) * span);
        assert_eq!(
            view.close()[last],
            100.0 + (cursor / MIN) as f64,
            "the forming candle closes at the cursor"
        );
        assert!(
            view.high().iter().all(|&h| h <= 100.0 + (cursor / MIN) as f64 + 0.5),
            "no high from beyond the cursor"
        );
    }
}

#[test]
fn trading_uses_the_bars_as_they_came_not_the_candles_shown() {
    let mut c = chart(600);
    c.set_timeframe(Some(15 * MIN));
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    press(&mut c, Key::Alt('P'));
    c.handle(Event::PointerLeft);
    press(&mut c, Key::SelectRight);
    let minute = c.cursor().unwrap().0 / MIN;
    press(&mut c, Key::Char('b'));
    let account = c.resources().get::<Account>().unwrap();
    let fill = account.fills()[0];
    assert_eq!(
        fill.price,
        100.0 + minute as f64,
        "filled at the last minute shown, not at the open of the 15 minute candle"
    );
    assert_eq!(fill.time, Time(minute * MIN));
    // The fill is marked on the period that holds it: the account curve has a value for it.
    c.build_scene(&mut Scene::new());
}

#[test]
fn a_live_feed_updates_the_forming_period_and_starts_the_next() {
    let mut c = chart(10);
    c.set_timeframe(Some(5 * MIN));
    assert_eq!(c.seen().len(), 2);
    let bar = |i: i64, close: f64| Candle {
        time: Time(i * MIN),
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1.0,
    };
    c.update(bar(9, 500.0)).unwrap(); // the forming minute changes
    assert_eq!(c.seen().len(), 2);
    assert_eq!(c.seen().close()[1], 500.0);
    assert_eq!(c.seen().high()[1], 501.0);
    assert_eq!(c.seen().volume()[1], 5.0);
    c.push(bar(10, 510.0)).unwrap(); // a new period opens
    assert_eq!(c.seen().len(), 3);
    assert_eq!(c.seen().open()[2], 510.0);
}

#[test]
fn the_toolbar_button_shows_the_timeframe_and_a_click_cycles_it() {
    let mut c = chart(600);
    let center = |c: &Chart| {
        let rects = tickerlight_core::builtin::layout(c.resources(), &c.style, 1.0, c.plot_size().0, c.plot_size().1);
        rects
            .iter()
            .find(|(b, _, _)| b.command == tickerlight_core::plugin::commands::TIMEFRAME_CYCLE)
            .map(|(_, label, r)| (label.clone(), r.x + r.w / 2.0, r.y + r.h / 2.0))
            .expect("the button is there")
    };
    assert_eq!(center(&c).0, "1m");
    let (_, x, y) = center(&c);
    c.handle(Event::PointerMoved { x, y });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerUp);
    assert_eq!(c.timeframe(), Some(3 * MIN));
    assert_eq!(center(&c).0, "3m");
}
