//! A host listens to the chart through `Observe`: fills, replay state and the pointer.

use std::cell::RefCell;
use std::rc::Rc;

use tickerlight_core::builtin::{Account, Drawings, Fill, Listener, Observe};
use tickerlight_core::{Candle, Candles, Chart, Event, Key, Scene, Time};

const MINUTE: i64 = 60_000;

#[derive(Default)]
struct Log {
    fills: Vec<(Fill, usize)>,
    replay: Vec<(bool, usize, usize)>,
    pointer: Vec<(Option<Time>, Option<f64>)>,
    drawings: Vec<String>,
}

#[derive(Clone, Default)]
struct Spy(Rc<RefCell<Log>>);

impl Listener for Spy {
    fn fill(&mut self, fill: &Fill, account: &Account) {
        self.0.borrow_mut().fills.push((*fill, account.fills().len()));
    }
    fn pointer(&mut self, bar: Option<Time>, price: Option<f64>) {
        self.0.borrow_mut().pointer.push((bar, price));
    }
    fn drawings(&mut self, drawings: &Drawings) {
        self.0.borrow_mut().drawings.push(drawings.save());
    }
    fn replay(&mut self, active: bool, shown: usize, total: usize) {
        self.0.borrow_mut().replay.push((active, shown, total));
    }
}

fn chart(spy: &Spy) -> Chart {
    let mut candles = Candles::new();
    for i in 0..500 {
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
    let mut chart = Chart::new(candles).with(Observe::new(spy.clone()));
    chart.resize(872.0, 528.0);
    chart.build_scene(&mut Scene::new());
    chart
}

#[test]
fn fills_are_reported_once_each_and_in_order() {
    let spy = Spy::default();
    let mut c = chart(&spy);
    c.handle(Event::Key(Key::Alt('P')));
    c.handle(Event::Key(Key::Char('b')));
    c.handle(Event::Key(Key::SelectRight));
    c.handle(Event::Key(Key::Char('s')));
    c.handle(Event::Key(Key::SelectRight));
    let log = spy.0.borrow();
    assert_eq!(log.fills.len(), 2);
    assert!(log.fills[0].0.qty > 0.0 && log.fills[1].0.qty < 0.0);
    assert_eq!(
        log.fills.iter().map(|f| f.1).collect::<Vec<_>>(),
        [1, 2],
        "the account is seen as it is after each fill"
    );
}

#[test]
fn replay_changes_are_reported_even_when_another_handler_consumed_the_key() {
    let spy = Spy::default();
    let mut c = chart(&spy);
    c.handle(Event::Key(Key::Alt('P')));
    c.handle(Event::Key(Key::SelectRight));
    c.handle(Event::Key(Key::SelectRight));
    let log = spy.0.borrow();
    let active: Vec<_> = log.replay.iter().map(|r| r.0).collect();
    assert_eq!(active.first(), Some(&true), "{:?}", log.replay);
    let shown: Vec<_> = log.replay.iter().map(|r| r.1).collect();
    assert!(
        shown.windows(2).all(|w| w[1] == w[0] + 1),
        "each step tells the new size once: {shown:?}"
    );
    assert!(log.replay.iter().all(|r| r.2 == 500));
}

#[test]
fn the_pointer_is_reported_with_its_bar_and_price_and_when_it_leaves() {
    let spy = Spy::default();
    let mut c = chart(&spy);
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    c.handle(Event::PointerLeft);
    let log = spy.0.borrow();
    let (bar, price) = log.pointer[0];
    let bar = bar.expect("a bar under the pointer");
    assert_eq!(bar.0 % MINUTE, 0);
    assert!(price.is_some());
    assert_eq!(log.pointer[1], (None, None));
}

#[test]
fn drawings_are_reported_when_placed_and_when_a_drag_ends_not_while_it_goes() {
    let spy = Spy::default();
    let mut c = chart(&spy);
    c.handle(Event::Key(Key::Alt('t')));
    c.handle(Event::PointerMoved { x: 100.0, y: 300.0 });
    c.handle(Event::PointerDown);
    for i in 1..=10 {
        c.handle(Event::PointerMoved {
            x: 100.0 + 30.0 * i as f32,
            y: 300.0 - 15.0 * i as f32,
        });
    }
    assert!(spy.0.borrow().drawings.is_empty(), "still dragging");
    c.handle(Event::PointerUp);
    assert_eq!(spy.0.borrow().drawings.len(), 1);
    assert!(spy.0.borrow().drawings[0].starts_with("trend pane=main "));
    // Nothing changes: nothing is reported.
    c.handle(Event::PointerMoved { x: 50.0, y: 50.0 });
    assert_eq!(spy.0.borrow().drawings.len(), 1);
    // Deleting the selected drawing is a change too.
    c.build_scene(&mut Scene::new());
    c.handle(Event::PointerMoved { x: 250.0, y: 225.0 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerUp);
    c.handle(Event::Key(Key::Delete));
    let log = spy.0.borrow();
    assert_eq!(log.drawings.last().map(String::as_str), Some(""), "{:?}", log.drawings);
}
