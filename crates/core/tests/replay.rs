//! Manual bar replay and virtual trades, driven only by input events.

use tickerlight_core::builtin::{Account, Replay};
use tickerlight_core::{Candle, Candles, Chart, Event, Key, Scene, Time};

const MINUTE: i64 = 60_000;

/// Close of bar `i` is `100 + i`, so a fill price tells which bar it happened on.
fn candles(n: i64) -> Candles {
    let mut c = Candles::new();
    for i in 0..n {
        let p = 100.0 + i as f64;
        c.push(Candle {
            time: Time(i * MINUTE),
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
    chart.resize(872.0, 528.0); // plot 800 x 500, 100 bars across at the default zoom
    chart
}

fn key(chart: &mut Chart, c: char) -> bool {
    chart.handle(Event::Key(Key::Char(c)))
}

/// Alt+Shift+P: starts the replay at the bar under the pointer, or leaves it.
fn toggle_replay(chart: &mut Chart) -> bool {
    chart.handle(Event::Key(Key::Alt('P')))
}

fn replay(chart: &Chart) -> &Replay {
    chart.resources().get::<Replay>().unwrap()
}

fn account(chart: &Chart) -> &Account {
    chart.resources().get::<Account>().unwrap()
}

fn texts(chart: &mut Chart) -> Vec<String> {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene.texts.into_iter().map(|t| t.text).collect()
}

#[test]
fn the_replay_command_starts_near_the_right_edge_and_hides_everything_after_it() {
    let mut c = chart(1000);
    // Scroll to older data: the screen shows bars 400..500 (plus the margin).
    c.handle(Event::PointerMoved { x: 100.0, y: 100.0 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved {
        x: 100.0 + 8.0 * 505.0,
        y: 100.0,
    });
    c.handle(Event::PointerUp);
    c.handle(Event::PointerLeft);
    let visible = c.time_scale().visible(1000);
    // The replay button starts 70% of the way across the visible bars (the key uses the pointer if it is over one).
    let edge = visible.start + ((visible.end - visible.start) as f64 * 0.7) as usize;

    assert!(c.handle(Event::Command(tickerlight_core::plugin::commands::REPLAY_TOGGLE)));
    assert!(replay(&c).active);
    assert_eq!(c.seen().len(), edge);
    assert_eq!(replay(&c).shown, edge);
    assert_eq!(replay(&c).total, 1000);
    assert!(texts(&mut c).iter().any(|t| t.starts_with("REPLAY")));
}

#[test]
fn alt_shift_p_starts_under_the_pointer_when_it_is_over_a_bar() {
    let mut c = chart(1000);
    // x = 400 is inside bar 955 (see the crosshair test), so bars 0..=955 are shown.
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    assert_eq!(c.seen().len(), 956);
}

#[test]
fn stepping_reveals_one_bar_and_the_newest_bar_stays_in_place() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    c.handle(Event::PointerLeft);
    let x_of_last = c.time_scale().x_of(c.seen().len() as f64 - 0.5);

    assert!(c.handle(Event::Key(Key::SelectRight)));
    assert_eq!(c.seen().len(), 957);
    // The chart scrolled by one bar: the newest bar is drawn where the previous newest was.
    let x_of_new = c.time_scale().x_of(c.seen().len() as f64 - 0.5);
    assert!((x_of_new - x_of_last).abs() < 1e-9, "{x_of_last} -> {x_of_new}");
}

#[test]
fn a_chart_looking_at_history_does_not_scroll_when_bars_are_revealed() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 700.0, y: 100.0 });
    toggle_replay(&mut c); // shown: 0..=986
    c.handle(Event::PointerLeft);
    // Drag far into the past: the newest bar is off screen.
    c.handle(Event::PointerMoved { x: 100.0, y: 100.0 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved {
        x: 100.0 + 8.0 * 300.0,
        y: 100.0,
    });
    c.handle(Event::PointerUp);
    let right = c.time_scale().right;
    c.handle(Event::Key(Key::SelectRight));
    assert_eq!(c.time_scale().right, right);
}

#[test]
fn autoscale_never_sees_the_bars_that_are_not_revealed_yet() {
    // Quiet market, then a spike to 10 000 at bar 60. Midday, so no time label is a number (a year, a day).
    let mut data = Candles::new();
    for i in 0..100 {
        let p = if i < 60 { 100.0 } else { 10_000.0 };
        data.push(Candle {
            time: Time(12 * 60 * MINUTE + i * MINUTE),
            open: p,
            high: p + 1.0,
            low: p - 1.0,
            close: p,
            volume: 1.0,
        })
        .unwrap();
    }
    let mut c = Chart::new(data);
    c.resize(872.0, 528.0);
    c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    toggle_replay(&mut c);
    c.handle(Event::PointerLeft);
    assert!(c.seen().len() <= 60);

    let max_label = |c: &mut Chart| -> f64 {
        texts(c)
            .iter()
            .filter_map(|t| t.parse::<f64>().ok())
            .fold(0.0, f64::max)
    };
    assert!(
        max_label(&mut c) < 200.0,
        "the scale reaches the spike: {}",
        max_label(&mut c)
    );
    // Once it is revealed, the scale follows.
    for _ in 0..70 {
        c.handle(Event::Key(Key::SelectRight));
    }
    assert!(max_label(&mut c) > 5000.0);
}

#[test]
fn playing_reveals_bars_over_time_and_stops_at_the_end() {
    let mut c = chart(120);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    c.handle(Event::PointerLeft);
    let start = c.seen().len();
    assert!(!c.needs_tick(), "paused: the backend can stay idle");

    c.handle(Event::Key(Key::Space));
    assert!(replay(&c).playing);
    assert!(c.needs_tick());

    // Minute bars play at five minutes a second: half a second plays two bars and half of the third, which has no
    // tape and so stays hidden until it is complete; time is not lost between ticks.
    assert!(c.handle(Event::Tick { dt: 0.5 }));
    assert_eq!(c.seen().len(), start + 2);
    c.handle(Event::Tick { dt: 0.1 });
    assert_eq!(c.seen().len(), start + 3);

    // Run to the end.
    for _ in 0..200 {
        c.handle(Event::Tick { dt: 0.5 });
    }
    assert_eq!(c.seen().len(), 120);
    assert!(!replay(&c).playing);
    assert!(!c.needs_tick(), "finished: no more ticks needed");

    // Pause and resume.
    toggle_replay(&mut c);
    toggle_replay(&mut c);
    c.handle(Event::Key(Key::Space));
    c.handle(Event::Key(Key::Space));
    assert!(!c.needs_tick());
}

#[test]
fn speed_can_be_changed_and_is_limited() {
    let mut c = chart(1000);
    toggle_replay(&mut c);
    let start = replay(&c).speed;
    assert_eq!(start, 300.0, "minute bars: about four a second");
    c.handle(Event::Key(Key::Up));
    assert_eq!(replay(&c).speed, 600.0);
    for _ in 0..30 {
        c.handle(Event::Key(Key::Up));
    }
    assert_eq!(replay(&c).speed, 2_419_200.0, "four weeks a second at most");
    for _ in 0..30 {
        c.handle(Event::Key(Key::Down));
    }
    assert_eq!(replay(&c).speed, 1.0, "real time at least");
    // A restart on the same bars keeps the speed chosen.
    toggle_replay(&mut c);
    toggle_replay(&mut c);
    assert_eq!(replay(&c).speed, 1.0);
}

#[test]
fn alt_shift_p_again_returns_to_the_live_chart() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    assert!(c.seen().len() < 1000);
    toggle_replay(&mut c);
    assert!(!replay(&c).active);
    assert_eq!(c.seen().len(), 1000);
    assert!(!texts(&mut c).iter().any(|t| t.starts_with("REPLAY")));
}

#[test]
fn replay_keys_do_nothing_while_not_replaying() {
    let mut c = chart(1000);
    assert!(!c.handle(Event::Key(Key::SelectRight)));
    assert!(!c.handle(Event::Key(Key::Space)));
    assert!(!c.handle(Event::Tick { dt: 1.0 }));
    assert_eq!(c.seen().len(), 1000);
}

#[test]
fn an_empty_chart_survives_the_replay_keys() {
    let mut c = Chart::new(Candles::new());
    toggle_replay(&mut c);
    for k in ['b', 's', 'c'] {
        key(&mut c, k);
    }
    c.handle(Event::Key(Key::SelectRight));
    c.handle(Event::Tick { dt: 1.0 });
    assert!(account(&c).fills().is_empty());
    texts(&mut c);
}

#[test]
fn orders_fill_at_the_last_shown_close_never_at_a_future_price() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c); // bars 0..=955 shown, last close 1055
    c.handle(Event::PointerLeft);

    assert!(key(&mut c, 'b'));
    c.handle(Event::Key(Key::SelectRight));
    c.handle(Event::Key(Key::SelectRight));
    key(&mut c, 's');
    let fills = account(&c).fills();
    assert_eq!(fills.len(), 2);
    assert_eq!(
        (fills[0].price, fills[0].qty, fills[0].time),
        (1055.0, 1.0, Time(955 * MINUTE))
    );
    assert_eq!(
        (fills[1].price, fills[1].qty, fills[1].time),
        (1057.0, -1.0, Time(957 * MINUTE))
    );
    assert_eq!(account(&c).position(), 0.0);
    assert_eq!(account(&c).realized(), 2.0);
}

#[test]
fn close_flattens_the_position_and_the_readout_shows_the_pnl() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    c.handle(Event::PointerLeft);
    key(&mut c, 'b');
    key(&mut c, 'b');
    assert_eq!(account(&c).position(), 2.0);
    for _ in 0..5 {
        c.handle(Event::Key(Key::SelectRight));
    }
    // Two longs at 1055, the price is 1060 now: +10 unrealized.
    let shown = texts(&mut c);
    assert!(
        shown
            .iter()
            .any(|t| t.contains("Position +2") && t.contains("P&L +10.00")),
        "{shown:?}"
    );

    key(&mut c, 'c');
    assert_eq!(account(&c).position(), 0.0);
    assert_eq!(account(&c).realized(), 10.0);
    assert!(
        texts(&mut c)
            .iter()
            .any(|t| t.starts_with("Flat") && t.contains("Realized +10.00"))
    );
    // Closing a flat account does nothing.
    key(&mut c, 'c');
    assert_eq!(account(&c).fills().len(), 3);
}

#[test]
fn markers_and_the_average_price_line_are_drawn() {
    let mut c = chart(1000);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    toggle_replay(&mut c);
    c.handle(Event::PointerLeft);
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let (up, accent) = (c.style.candle.up, c.style.accent);
    let count = |scene: &Scene, color| scene.rects.iter().filter(|r| r.color == color).count();
    let ups_before = count(&scene, up);
    assert_eq!(
        scene.lines.iter().filter(|l| l.color == accent).count(),
        0,
        "no position, no line"
    );

    key(&mut c, 'b');
    c.build_scene(&mut scene);
    // The buy marker is a triangle of 7 rows in the buy color.
    assert_eq!(count(&scene, up), ups_before + 7);
    assert_eq!(scene.lines.iter().filter(|l| l.color == accent).count(), 1);
    // The label of the average price is on the price axis, in the accent color.
    let plot_w = c.plot_size().0;
    assert!(scene.rects.iter().any(|r| r.color == accent && r.x > plot_w));
    assert!(scene.texts.iter().any(|t| t.text == "1055.00" && t.x > plot_w));
}

mod tape {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tickerlight_core::{DrawCtx, Layer, Side, Trade, Trades, candles_from_trades};

    /// One trade per second for `n` minutes, price = the minute it belongs to.
    fn tape(minutes: i64) -> Trades {
        let mut t = Trades::new();
        for s in 0..minutes * 60 {
            t.push(Trade {
                time: Time(s * 1000),
                price: 100.0 + (s / 60) as f64,
                qty: 1.0,
                side: Side::Buy,
            })
            .unwrap();
        }
        t
    }

    /// What a layer saw of the tape at each frame: how many trades and the time of the last.
    type Seen = Rc<RefCell<Vec<(usize, Option<Time>)>>>;
    struct Spy(Seen);
    impl Layer for Spy {
        fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
            self.0
                .borrow_mut()
                .push((ctx.trades.len(), ctx.trades.time().last().copied()));
        }
    }

    #[test]
    fn candles_built_from_the_tape_match_it() {
        let trades = tape(10);
        let candles = candles_from_trades(trades.view(), MINUTE);
        assert_eq!(candles.len(), 10);
        assert_eq!(candles.view().get(3).unwrap().volume, 60.0);
    }

    #[test]
    fn the_tape_is_cut_where_the_candles_are() {
        let trades = tape(1000);
        let candles = candles_from_trades(trades.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(trades);
        c.resize(872.0, 528.0);
        let seen = Rc::new(RefCell::new(Vec::new()));
        c.add_layer("spy", 1000, Spy(seen.clone()));

        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        assert_eq!(seen.borrow().last().unwrap().0, 60_000, "live: the whole tape");

        // Replay with 10 bars shown: minutes 0..=9, so the trades up to 9:59.
        c.set_cursor(Some(Time(9 * MINUTE)));
        c.build_scene(&mut scene);
        let (len, last) = *seen.borrow().last().unwrap();
        assert_eq!(len, 600);
        assert_eq!(last, Some(Time(599_000)), "not a single trade of the hidden minute");
        assert_eq!(c.seen_trades().len(), 600);
    }

    #[test]
    fn handlers_get_the_same_cut_and_reveal_it_bar_by_bar() {
        let trades = tape(200);
        let candles = candles_from_trades(trades.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(trades);
        c.resize(872.0, 528.0);
        c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
        toggle_replay(&mut c);
        let shown = c.seen().len();
        assert_eq!(c.seen_trades().len(), shown * 60);
        c.handle(Event::Key(Key::SelectRight));
        assert_eq!(c.seen_trades().len(), (shown + 1) * 60);
    }

    #[test]
    fn bars_of_seconds_are_made_of_the_tape_live_and_step_a_bar_at_a_time_in_a_replay() {
        let trades = tape(10);
        let candles = candles_from_trades(trades.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(trades);
        c.resize(872.0, 528.0);
        c.set_timeframe(Some(15_000));
        assert_eq!(c.seen().len(), 40);
        let bar = c.seen().get(5).unwrap();
        assert_eq!((bar.time, bar.open, bar.volume), (Time(75_000), 101.0, 15.0));

        // A live trade after a quiet minute and a half opens a bar of its own; the chart at the newest bar follows.
        let right = c.time_scale().right;
        let late = Trade {
            time: Time(700_000),
            price: 120.0,
            qty: 2.0,
            side: Side::Sell,
        };
        c.push_trade(late).unwrap();
        assert_eq!(c.seen().len(), 41);
        assert_eq!(c.time_scale().right, right + 1.0);

        // The replay starts at the end of a bar of seconds: 28 of the 41 bars, the trades before 7:00.
        assert!(c.handle(Event::Command(tickerlight_core::plugin::commands::REPLAY_TOGGLE)));
        assert_eq!(c.moment(), Some(Time(420_000)));
        assert_eq!((c.seen().len(), c.seen_trades().len()), (28, 420));
        // A step plays one bar of seconds on, the base bar of that minute forming.
        c.handle(Event::Key(Key::SelectRight));
        assert_eq!(c.moment(), Some(Time(435_000)));
        assert_eq!((c.seen().len(), c.seen_trades().len()), (29, 435));
        assert!(c.forming());
        // Steps go over the quiet time to the next bar with a trade, and stop at the end of the tape.
        for _ in 0..11 {
            c.handle(Event::Key(Key::SelectRight));
        }
        assert_eq!((c.seen().len(), c.moment()), (40, Some(Time(600_000))));
        c.handle(Event::Key(Key::SelectRight));
        assert_eq!((c.seen().len(), c.moment()), (41, Some(Time(705_000))));
        assert!(!c.handle(Event::Key(Key::SelectRight)));
        assert_eq!(c.moment(), Some(Time(705_000)));
    }

    #[test]
    fn a_chart_without_a_tape_has_an_empty_view() {
        let c = chart(10);
        assert!(c.seen_trades().is_empty());
    }
}

mod by_trade {
    use super::*;
    use tickerlight_core::builtin::{Exits, Order, OrderKind};
    use tickerlight_core::{Side, Trade, Trades, candles_from_trades};

    /// Minute bars made from trades at (second, price).
    fn chart_of(trades: &[(i64, f64)]) -> Chart {
        let mut tape = Trades::new();
        for &(s, price) in trades {
            let side = Side::Buy;
            tape.push(Trade {
                time: Time(s * 1000),
                price,
                qty: 1.0,
                side,
            })
            .unwrap();
        }
        let candles = candles_from_trades(tape.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(tape);
        c.resize(872.0, 528.0);
        c
    }

    /// Starts a replay with the first `bars` minutes shown.
    fn start(c: &mut Chart, bars: i64) {
        c.resources_mut().get_mut::<Replay>().unwrap().start_at = Some(Time(bars * MINUTE));
        toggle_replay(c);
    }

    fn place(c: &mut Chart, order: Order) {
        c.resources_mut().get_mut::<Account>().unwrap().place(order);
    }

    #[test]
    fn an_exit_does_not_fill_on_trades_before_its_entry() {
        // Minute 1 dips to 98.5, then rises through 105.
        let mut c = chart_of(&[
            (0, 100.0),
            (61, 98.5),
            (70, 100.0),
            (80, 105.0),
            (90, 104.0),
            (150, 104.0),
        ]);
        start(&mut c, 1);
        place(
            &mut c,
            Order {
                kind: OrderKind::Stop,
                qty: 1.0,
                price: 105.0,
                after: Time(0),
                reduce_only: false,
                exits: Exits {
                    take_profit: None,
                    stop_loss: Some(99.0),
                    break_even: None,
                },
                break_even: None,
            },
        );
        c.handle(Event::Key(Key::SelectRight));
        c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
        assert_eq!(account(&c).position(), 1.0, "the stop entry filled at 105");
        // Every event looks at the newest bar again: the dip before the entry does not reach its stop loss.
        c.handle(Event::PointerMoved { x: 310.0, y: 100.0 });
        assert_eq!(account(&c).position(), 1.0);
        assert_eq!(account(&c).orders().len(), 1, "the stop loss waits");
    }

    #[test]
    fn playing_grows_the_newest_bar_trade_by_trade() {
        // Minute 1 trades every 10 s at 100, 101, ... 105; minute 2 opens at 106.
        let mut trades = vec![(0, 100.0)];
        trades.extend((0..6).map(|k| (60 + k * 10, 100.0 + k as f64)));
        trades.push((120, 106.0));
        let mut c = chart_of(&trades);
        start(&mut c, 1);
        c.resources_mut().get_mut::<Replay>().unwrap().speed = 1.0;
        c.handle(Event::Key(Key::Space));

        // 85 s: the bar of minute 1 holds the trades up to 80 s, and nothing after them is seen.
        c.handle(Event::Tick { dt: 25.0 });
        assert!(c.forming());
        assert_eq!(c.moment(), Some(Time(85_000)));
        let bar = c.seen().get(1).unwrap();
        assert_eq!((bar.open, bar.high, bar.close, bar.volume), (100.0, 102.0, 102.0, 3.0));
        assert_eq!(c.seen_trades().time().last(), Some(&Time(80_000)));

        // A sell limit placed now fills on a later trade of the same bar.
        let after = c.order_after().unwrap();
        place(
            &mut c,
            Order {
                kind: OrderKind::Limit,
                qty: -1.0,
                price: 104.0,
                after,
                reduce_only: false,
                exits: Exits::default(),
                break_even: None,
            },
        );
        c.handle(Event::Tick { dt: 10.0 });
        assert_eq!(account(&c).position(), 0.0, "95 s: 103 at most");
        c.handle(Event::Tick { dt: 10.0 });
        assert_eq!(account(&c).position(), -1.0, "105 s: 104 traded");

        // Once the minute is over its bar is whole, and the next one forms.
        c.handle(Event::Tick { dt: 20.0 });
        let bar = c.seen().get(1).unwrap();
        assert_eq!((bar.high, bar.close, bar.volume), (105.0, 105.0, 6.0));
        assert_eq!(c.seen().len(), 3);
        assert_eq!(c.seen().get(2).unwrap().open, 106.0);

        // A step finishes the bar forming; leaving the replay shows every bar as it is.
        c.handle(Event::Key(Key::SelectRight));
        assert!(!c.forming());
        assert_eq!(c.seen().len(), 3);
        toggle_replay(&mut c);
        assert_eq!(c.seen().get(1).unwrap().volume, 6.0);
    }

    #[test]
    fn on_a_live_chart_an_order_fills_only_on_the_trades_after_it_was_placed() {
        use tickerlight_core::builtin::{Clock, Live};
        let trade = |s: f64, price: f64| Trade {
            time: Time((s * 1000.0) as i64),
            price,
            qty: 1.0,
            side: Side::Sell,
        };
        let mut c = chart_of(&[(0, 100.0), (30, 99.0), (61, 101.0)]);
        // Not live: outside a replay nothing can be traded.
        key(&mut c, 'b');
        assert!(account(&c).fills().is_empty());
        c.resources_mut().insert(Live(true));
        c.resources_mut().insert(Clock(Time(65_000)));
        let after = c.order_after().unwrap();
        assert_eq!(after, Time(65_000), "now by the clock");
        place(
            &mut c,
            Order {
                kind: OrderKind::Limit,
                qty: 1.0,
                price: 100.0,
                after,
                reduce_only: false,
                exits: Exits {
                    take_profit: Some(103.0),
                    stop_loss: None,
                    break_even: None,
                },
                break_even: None,
            },
        );
        // A trade from before the order, come late (the stream's catch-up), does not fill it.
        c.push_trade(trade(62.0, 99.5)).unwrap();
        c.handle(Event::Tick { dt: 0.1 });
        assert!(account(&c).fills().is_empty());
        c.push_trade(trade(70.0, 100.0)).unwrap();
        c.handle(Event::Tick { dt: 0.1 });
        let fills = account(&c).fills();
        assert_eq!((fills.len(), fills[0].price), (1, 100.0));
        assert_eq!(fills[0].time, Time(MINUTE), "the fill keeps its bar's time");
        // The take profit works from the trade after its entry.
        c.push_trade(trade(70.0, 103.5)).unwrap();
        c.handle(Event::Tick { dt: 0.1 });
        assert_eq!(account(&c).position(), 1.0);
        c.push_trade(trade(80.0, 103.0)).unwrap();
        c.handle(Event::Tick { dt: 0.1 });
        assert_eq!(account(&c).position(), 0.0);
        assert_eq!(account(&c).fills()[1].price, 103.0);
        // The keys trade at the last price.
        key(&mut c, 's');
        assert_eq!(account(&c).position(), -1.0);
        assert_eq!(account(&c).fills()[2].price, 101.0);
    }
}
