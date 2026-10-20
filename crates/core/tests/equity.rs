//! The equity curve pane and the trade statistics in a replay.

use tickerlight_core::builtin::{Account, Equity};
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Key, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(2000)).with(Equity::new());
    chart.resize(872.0, 628.0);
    chart
}

fn key(chart: &mut Chart, c: char) {
    chart.handle(Event::Key(Key::Char(c)));
}

fn texts(chart: &mut Chart) -> Vec<String> {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene.texts.into_iter().map(|t| t.text).collect()
}

#[test]
fn the_equity_pane_follows_the_account_during_a_replay() {
    let mut c = chart();
    assert_eq!(c.pane_names(), ["main", "equity"]);
    assert!(texts(&mut c).iter().any(|t| t == "Equity  +0.00"), "no trades yet");
    c.handle(Event::Key(Key::Alt('P')));
    key(&mut c, 'b');
    for _ in 0..10 {
        c.handle(Event::Key(Key::SelectRight));
    }
    let shown = c.seen().len();
    assert!(shown < 2000);
    let account = c.resources().get::<Account>().unwrap();
    assert_eq!(account.fills().len(), 1);
    let curve = account.equity_curve(c.seen(), 0..shown + 100);
    assert_eq!(curve.len(), shown, "the curve ends where the replay is");
    let last = c.seen().close()[shown - 1];
    assert!((curve.last().unwrap().1 - account.net(last)).abs() < 1e-9);
    let net = account.net(last);
    let readout = texts(&mut c).into_iter().find(|t| t.starts_with("Equity")).unwrap();
    assert_eq!(readout, format!("Equity  {net:+.2}"));
}

#[test]
fn the_hud_shows_trade_statistics_after_the_first_closed_trade() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('P')));
    key(&mut c, 'b');
    assert!(!texts(&mut c).iter().any(|t| t.starts_with("Trades")));
    c.handle(Event::Key(Key::SelectRight));
    key(&mut c, 'c');
    let hud = texts(&mut c)
        .into_iter()
        .find(|t| t.starts_with("Trades 1"))
        .expect("statistics line");
    assert!(hud.contains("Max drawdown"), "{hud}");
}
