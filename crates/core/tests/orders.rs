//! Pending orders (limit and stop) in the replay: placed at the pointer, filled by later bars only.

use tickerlight_core::builtin::{Account, Exits, OrderKind};
use tickerlight_core::{Candle, Candles, Chart, Event, Key, Scene, Time};

const MINUTE: i64 = 60_000;

/// Bar `i` has open and close `100 + i` and spans half a unit each way, so the market only rises.
fn chart() -> Chart {
    let mut candles = Candles::new();
    for i in 0..1000 {
        let p = 100.0 + i as f64;
        candles
            .push(Candle {
                time: Time(i * MINUTE),
                open: p,
                high: p + 0.5,
                low: p - 0.5,
                close: p,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut chart = Chart::new(candles);
    chart.resize(872.0, 528.0);
    chart
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
