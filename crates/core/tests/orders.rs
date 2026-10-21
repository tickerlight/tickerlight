//! Pending orders (limit and stop) in the replay: placed at the pointer, filled by later bars only.

use tickerlight_core::builtin::{Account, Exits, Fill, Order, OrderKind, Replay, exits_tried};
use tickerlight_core::{Candle, Candles, Chart, Event, Key, Scene, Time};

const MINUTE: i64 = 60_000;

/// Bar `i` has open and close `100 + i` and spans half a unit each way, so the market only rises.
fn chart() -> Chart {
    let mut chart = Chart::new(rising(&[]));
    chart.resize(872.0, 528.0);
    chart
}

/// A thousand rising bars as [`chart`]'s, but for the `(i, (open, high, low, close))` given.
fn rising(but: &[(i64, (f64, f64, f64, f64))]) -> Candles {
    let mut candles = Candles::new();
    for i in 0..1000 {
        let p = 100.0 + i as f64;
        let (open, high, low, close) = but.iter().find(|b| b.0 == i).map_or((p, p + 0.5, p - 0.5, p), |b| b.1);
        candles
            .push(Candle {
                time: Time(i * MINUTE),
                open,
                high,
                low,
                close,
                volume: 1.0,
            })
            .unwrap();
    }
    candles
}

fn account(chart: &Chart) -> &Account {
    chart.resources().get::<Account>().unwrap()
}

/// Draws a frame (the price scale a handler sees is the one of the last frame), then presses `c` with
/// the pointer at height `y`.
fn press_at(chart: &mut Chart, y: f32, c: char) {
    chart.build_scene(&mut Scene::new());
    chart.handle(Event::PointerMoved { x: 300.0, y });
    chart.handle(Event::Key(Key::Char(c)));
}

fn last_close(chart: &Chart) -> f64 {
    *chart.seen().close().last().unwrap()
}

#[test]
fn a_limit_order_above_the_market_is_a_sell_and_fills_on_a_later_bar() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    let close = last_close(&c);
    press_at(&mut c, 40.0, 'l');
    let order = account(&c).orders()[0];
    assert_eq!(order.kind, OrderKind::Limit);
    assert!(
        order.qty < 0.0 && order.price > close,
        "a limit above the market sells: {order:?}"
    );
    assert!(
        account(&c).fills().is_empty(),
        "nothing fills on the bar it was placed on"
    );

    let mut steps = 0;
    while account(&c).fills().is_empty() {
        c.handle(Event::Key(Key::SelectRight));
        steps += 1;
        assert!(steps < 200);
    }
    let fill = account(&c).fills()[0];
    let bar = (last_close(&c) - 100.0) as i64;
    assert_eq!(fill.time, Time(bar * MINUTE), "filled on the newest bar shown");
    assert!(last_close(&c) + 0.5 >= order.price, "the bar reached the price");
    assert!(last_close(&c) - 1.0 + 0.5 < order.price, "and the bar before did not");
    assert_eq!(fill.price, order.price.max(last_close(&c)));
    assert!(fill.qty < 0.0);
    assert!(account(&c).orders().is_empty());
}

#[test]
fn a_stop_order_above_the_market_is_a_buy() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 40.0, 't');
    let order = account(&c).orders()[0];
    assert_eq!(order.kind, OrderKind::Stop);
    assert!(order.qty > 0.0);
    for _ in 0..200 {
        c.handle(Event::Key(Key::SelectRight));
    }
    assert_eq!(account(&c).position(), order.qty, "the rising market took the stop");
}

#[test]
fn the_future_never_fills_an_order() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 40.0, 'l');
    let price = account(&c).orders()[0].price;
    // Bars that exist but are held back by the replay reach the price; nothing has happened yet.
    assert!(c.seen().len() < 1000);
    assert!(account(&c).fills().is_empty());
    assert_eq!(account(&c).orders().len(), 1);
    assert!(price < 1100.0);
}

#[test]
fn x_cancels_and_leaving_the_replay_drops_pending_orders() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 40.0, 'l');
    assert_eq!(account(&c).orders().len(), 1);
    c.handle(Event::Key(Key::Char('x')));
    assert!(account(&c).orders().is_empty());

    press_at(&mut c, 40.0, 'l');
    c.handle(Event::Key(Key::Alt('P'))); // leave the replay
    assert!(account(&c).orders().is_empty(), "the session ended");
    assert!(
        account(&c).fills().is_empty(),
        "and the bars revealed by leaving filled nothing"
    );
}

#[test]
fn orders_are_drawn_dashed_with_a_label() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 40.0, 'l');
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    assert!(scene.texts.iter().any(|t| t.text.starts_with("Sell limit")));
    let y = 40.0;
    let dashes = scene
        .lines
        .iter()
        .filter(|l| (l.y0 - y).abs() < 20.0 && l.y0 == l.y1)
        .count();
    assert!(dashes > 10, "a dashed line across the plot: {dashes}");
}

#[test]
fn the_size_keys_change_what_an_order_trades_and_the_button_shows_it() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    let label = |c: &mut Chart| {
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        scene
            .texts
            .into_iter()
            .map(|t| t.text)
            .find(|t| t.starts_with("Size "))
            .unwrap()
    };
    assert_eq!(label(&mut c), "Size 1");
    c.handle(Event::Key(Key::Char(']')));
    c.handle(Event::Key(Key::Char(']')));
    assert_eq!(account(&c).order_size, 5.0);
    assert_eq!(label(&mut c), "Size 5");
    c.handle(Event::Key(Key::Char('b')));
    assert_eq!(account(&c).position(), 5.0);
    c.handle(Event::Key(Key::Char('[')));
    c.handle(Event::Key(Key::Char('s')));
    assert_eq!(account(&c).position(), 3.0, "sold 2");
    // A click on the button steps up.
    let rects =
        tickerlight_core::builtin::layout(c.resources(), &c.style, 1.0, c.plot_size().0, c.pane_layout()[0].height);
    let size = rects
        .iter()
        .find(|(b, _, _)| b.command == tickerlight_core::plugin::commands::SIZE_UP)
        .unwrap()
        .2;
    c.handle(Event::PointerMoved {
        x: size.x + size.w / 2.0,
        y: size.y + size.h / 2.0,
    });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerUp);
    assert_eq!(account(&c).order_size, 5.0, "2 -> 5");
}

#[test]
fn a_stop_loss_and_a_take_profit_on_a_position_close_it_once_and_cancel_each_other() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    c.handle(Event::Key(Key::Char('b')));
    press_at(&mut c, 40.0, 'l'); // above the market: a sell limit, the take profit
    press_at(&mut c, 480.0, 't'); // below it: a sell stop, the stop loss
    let orders = account(&c).orders().to_vec();
    assert_eq!(orders.len(), 2);
    assert!(orders.iter().all(|o| o.reduce_only && o.qty < 0.0), "{orders:?}");
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    assert!(scene.texts.iter().any(|t| t.text == "Take profit 1"));
    assert!(scene.texts.iter().any(|t| t.text == "Stop loss 1"));

    for _ in 0..300 {
        c.handle(Event::Key(Key::SelectRight));
    }
    let a = account(&c);
    assert_eq!(a.position(), 0.0, "closed, not flipped short");
    assert_eq!(a.fills().len(), 2, "the buy and the take profit: {:?}", a.fills());
    assert!(a.orders().is_empty(), "the stop loss went with it");
    assert!(a.realized() > 0.0);
}

#[test]
fn an_order_without_a_position_is_not_reduce_only() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 40.0, 'l');
    assert!(!account(&c).orders()[0].reduce_only);
}

#[test]
fn dragging_an_order_line_moves_the_order_and_does_not_pan() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    press_at(&mut c, 100.0, 'l');
    let before = account(&c).orders()[0].price;
    let time = *c.time_scale();
    c.build_scene(&mut Scene::new());
    c.handle(Event::PointerMoved { x: 300.0, y: 103.0 }); // within reach of the line
    c.handle(Event::PointerDown);
    for i in 1..=10 {
        c.handle(Event::PointerMoved {
            x: 300.0 + 10.0 * i as f32,
            y: 103.0 + 20.0 * i as f32,
        });
    }
    c.handle(Event::PointerUp);
    let after = account(&c).orders()[0].price;
    assert!(after < before - 1.0, "dragged down: {before} -> {after}");
    assert_eq!(*c.time_scale(), time, "the chart did not pan");
    assert_eq!(account(&c).orders().len(), 1);

    // A drag that starts away from every line pans as usual.
    c.build_scene(&mut Scene::new());
    c.handle(Event::PointerMoved { x: 300.0, y: 20.0 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved { x: 400.0, y: 20.0 });
    c.handle(Event::PointerUp);
    assert_ne!(*c.time_scale(), time);
    assert_eq!(account(&c).orders()[0].price, after);
}

#[test]
fn dragging_a_break_even_line_moves_the_trigger_and_not_the_stop() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    let close = last_close(&c);
    // A short on a market that only rises: its break-even below the bars stays out of reach as it is dragged down.
    let a = c.resources_mut().get_mut::<Account>().unwrap();
    a.fill(Time(0), close + 10.0, -1.0);
    let exits = Exits {
        take_profit: None,
        stop_loss: Some(close + 15.0),
        break_even: Some(close - 3.0),
    };
    a.protect(Time(0), -1.0, exits);
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let label = scene
        .texts
        .iter()
        .find(|t| t.text == "Break-even")
        .expect("its line is named");
    // The name sits above its line.
    let y = label.y + label.size * 0.8;
    c.handle(Event::PointerMoved { x: 300.0, y });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved { x: 300.0, y: y + 40.0 });
    c.handle(Event::PointerUp);
    let stop = account(&c).orders()[0];
    assert_eq!(stop.price, close + 15.0, "the stop stays");
    assert!(
        stop.break_even.unwrap() < close - 5.0,
        "dragged down: {:?}",
        stop.break_even
    );
}

#[test]
fn an_order_label_hides_the_axis_price_under_it() {
    let mut c = chart();
    let axis = |c: &mut Chart| -> Vec<(String, f32)> {
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        scene
            .texts
            .into_iter()
            .filter(|t| t.x > 800.0)
            .map(|t| (t.text, t.y))
            .collect()
    };
    let before = axis(&mut c);
    // A tick in the middle of the axis, with an order a little above it: the label would cover half of it.
    let (tick, y) = before[before.len() / 2].clone();
    let price: f64 = tick.parse().unwrap();
    let order = tickerlight_core::builtin::Order {
        kind: OrderKind::Limit,
        qty: 1.0,
        price: price + 0.1,
        after: Time(0),
        reduce_only: false,
        exits: Default::default(),
        break_even: None,
    };
    c.resources_mut().get_mut::<Account>().unwrap().place(order);
    let after = axis(&mut c);
    assert!(
        after.iter().any(|(t, _)| *t == format!("{:.2}", price + 0.1)),
        "{after:?}"
    );
    assert!(!after.iter().any(|(t, ty)| *t == tick && *ty == y), "{after:?}");
    // The other ticks stay.
    assert!(after.iter().filter(|(t, _)| t.parse::<f64>().is_ok()).count() >= before.len() - 1);
}

/// The chart in a replay showing the first `shown` bars of `candles`.
fn replay(candles: Candles, shown: i64) -> Chart {
    let mut chart = Chart::new(candles);
    chart.resize(872.0, 528.0);
    let replay = chart.resources_mut().get_mut::<Replay>().unwrap();
    replay.start_at = Some(Time(shown * MINUTE));
    chart.handle(Event::Key(Key::Alt('P')));
    assert_eq!(chart.seen().len(), shown as usize);
    chart
}

fn account_mut(chart: &mut Chart) -> &mut Account {
    chart.resources_mut().get_mut::<Account>().unwrap()
}

#[test]
fn an_order_moved_into_the_bar_on_screen_waits_for_the_next() {
    let mut c = replay(rising(&[]), 500);
    let after = c.order_after().unwrap();
    account_mut(&mut c).place(Order {
        kind: OrderKind::Limit,
        qty: 1.0,
        price: 50.0,
        after,
        reduce_only: false,
        exits: Exits::default(),
        break_even: None,
    });
    c.handle(Event::Key(Key::SelectRight));
    assert!(account(&c).fills().is_empty());
    // Dragged just above the low of the bar the user has seen: that bar is past, the next ones only rise.
    let low = c.seen().low()[500];
    assert!(account_mut(&mut c).move_order(0, low + 0.01));
    c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    c.handle(Event::Key(Key::SelectRight));
    assert!(account(&c).fills().is_empty(), "{:?}", account(&c).fills());
}

#[test]
fn a_stop_moved_to_break_even_does_not_fill_on_the_bar_that_moved_it() {
    // Bar 500 reaches the trigger and falls back below the entry; bar 501 stays above it.
    let but = [(500, (600.0, 602.0, 598.0, 601.0)), (501, (601.0, 603.0, 600.5, 602.0))];
    let mut c = replay(rising(&but), 500);
    let entered = Time(499 * MINUTE);
    let exits = Exits {
        take_profit: None,
        stop_loss: Some(590.0),
        break_even: Some(601.5),
    };
    account_mut(&mut c).fill(entered, 599.0, 1.0);
    account_mut(&mut c).protect(entered, 1.0, exits);
    c.handle(Event::Key(Key::SelectRight));
    assert_eq!(account(&c).position(), 1.0);
    c.handle(Event::Key(Key::SelectRight));
    assert_eq!(account(&c).position(), 1.0, "{:?}", account(&c).fills());
}

#[test]
fn two_entries_sized_by_risk_leave_no_dust_when_both_targets_fill() {
    // Sizes of a coin at a tenth of a cent as risk sizing gives them (equity × risk / distance), the first pair
    // whose sum less each is not zero in floats.
    let sizes =
        (1..2000).flat_map(|k| (1..50).map(move |j| (1.0 / (0.00001234 * k as f64), 1.0 / (0.00000777 * j as f64))));
    let (a, b) = sizes
        .map(|(a, b)| (a * 100.0, b * 100.0))
        .find(|&(a, b)| a + b - a - b != 0.0)
        .unwrap();
    let mut account = Account::default();
    let exits = Exits {
        take_profit: Some(0.0011),
        stop_loss: Some(0.0009),
        break_even: None,
    };
    for qty in [a, b] {
        account.fill(Time(0), 0.001, qty);
        account.protect(Time(0), qty, exits);
    }
    account.match_bar(Time(1), 0.00105, 0.00112, 0.00104);
    assert_eq!(account.position(), 0.0);
    // A later fall meets no stop left behind.
    account.match_bar(Time(2), 0.00095, 0.00096, 0.00085);
    assert_eq!(account.stats().closed, 2);
}

#[test]
fn a_trade_turned_by_a_bigger_fill_counts_at_its_own_size() {
    let mut candles = Candles::new();
    for (i, p) in [100.0, 99.4, 102.1, 96.0, 97.0, 100.5].into_iter().enumerate() {
        let candle = Candle {
            time: Time(i as i64),
            open: p,
            high: p,
            low: p,
            close: p,
            volume: 1.0,
        };
        candles.push(candle).unwrap();
    }
    let fill = |time, price, qty| Fill {
        time: Time(time),
        price,
        qty,
        fee: 0.0,
    };
    // Long 1 at 100, sold 3 at 97 (out of the long at -3%, short 2), bought back 2 at 100.5.
    let fills = [fill(0, 100.0, 1.0), fill(4, 97.0, -3.0), fill(5, 100.5, 2.0)];
    let tried = exits_tried(&fills, candles.view());
    assert!(
        (tried.as_traded - (-0.03 - 3.5 / 97.0)).abs() < 1e-12,
        "{}",
        tried.as_traded
    );
}
