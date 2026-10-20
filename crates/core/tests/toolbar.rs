//! On-screen buttons: clicks become commands, and the features listen.

use tickerlight_core::builtin::{Account, Replay};
use tickerlight_core::plugin::commands;
use tickerlight_core::{
    ButtonKind, Candle, Candles, Chart, DrawCtx, Event, EventCtx, Handler, Key, Label, Layer, Plugin, Resources,
    Response, Scene, Time, ToolButton, builtin,
};

const MINUTE: i64 = 60_000;

fn chart(n: i64) -> Chart {
    let mut candles = Candles::new();
    for i in 0..n {
        let p = 100.0 + (i % 10) as f64;
        candles
            .push(Candle {
                time: Time(i * MINUTE),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut chart = Chart::new(candles);
    chart.resize(872.0, 528.0);
    chart
}

/// The center of the button that sends `command`, if it is shown.
/// A chart in a replay: the trade buttons are there.
fn replaying(n: i64) -> Chart {
    let mut c = chart(n);
    c.handle(Event::Key(Key::Alt('P')));
    c
}

fn button_center(chart: &Chart, command: &str) -> Option<(f32, f32)> {
    let rects = builtin::layout(
        chart.resources(),
        &chart.style,
        1.0,
        chart.plot_size().0,
        chart.plot_size().1,
    );
    rects
        .iter()
        .find(|(b, _, _)| b.command == command)
        .map(|(_, _, r)| (r.x + r.w / 2.0, r.y + r.h / 2.0))
}

fn click(chart: &mut Chart, at: (f32, f32)) -> bool {
    let mut redraw = chart.handle(Event::PointerMoved { x: at.0, y: at.1 });
    redraw |= chart.handle(Event::PointerDown);
    redraw |= chart.handle(Event::PointerUp);
    redraw
}

#[test]
fn buttons_are_laid_out_in_a_row_without_overlap_inside_the_plot() {
    let c = replaying(100);
    let rects = builtin::layout(c.resources(), &c.style, 1.0, c.plot_size().0, c.plot_size().1);
    assert!(rects.len() >= 3, "replay, buy, sell at least");
    let (plot_w, plot_h) = c.plot_size();
    for (_, label, r) in &rects {
        assert!(!label.is_empty());
        assert!(
            r.x >= 0.0 && r.x + r.w <= plot_w && r.y >= 0.0 && r.y + r.h <= plot_h,
            "{label}: {r:?}"
        );
    }
    for pair in rects.windows(2) {
        assert!(
            pair[0].2.x + pair[0].2.w < pair[1].2.x,
            "{} and {} overlap",
            pair[0].1,
            pair[1].1
        );
        assert_eq!(pair[0].2.y, pair[1].2.y, "one row");
    }
}

#[test]
fn layout_scales_with_the_screen_density() {
    let c = chart(100);
    let one = builtin::layout(c.resources(), &c.style, 1.0, 800.0, 500.0);
    let two = builtin::layout(c.resources(), &c.style, 2.0, 1600.0, 1000.0);
    assert_eq!(one.len(), two.len());
    for (a, b) in one.iter().zip(&two) {
        assert!((b.2.w - 2.0 * a.2.w).abs() < 0.5, "{} width", a.1);
        assert!((b.2.h - 2.0 * a.2.h).abs() < 1e-3);
    }
}

#[test]
fn clicking_the_replay_button_starts_and_stops_the_replay() {
    let mut c = chart(500);
    let at = button_center(&c, commands::REPLAY_TOGGLE).unwrap();
    assert!(click(&mut c, at));
    assert!(c.resources().get::<Replay>().unwrap().active);
    assert!(c.seen().len() < 500);

    // The replay buttons appeared, and the toggle now says how to leave.
    assert!(button_center(&c, commands::REPLAY_STEP).is_some());
    let shown = c.seen().len();
    let step = button_center(&c, commands::REPLAY_STEP).unwrap();
    click(&mut c, step);
    assert_eq!(c.seen().len(), shown + 1);

    let play = button_center(&c, commands::REPLAY_PLAY_PAUSE).unwrap();
    click(&mut c, play);
    assert!(c.resources().get::<Replay>().unwrap().playing);
    assert!(c.needs_tick());

    let at = button_center(&c, commands::REPLAY_TOGGLE).unwrap();
    click(&mut c, at);
    assert!(!c.resources().get::<Replay>().unwrap().active);
    assert_eq!(c.seen().len(), 500);
    assert!(button_center(&c, commands::REPLAY_STEP).is_none(), "hidden again");
}

#[test]
fn trade_buttons_trade_and_close_appears_with_a_position() {
    let mut c = chart(100);
    // Outside a replay there is nothing to trade.
    assert!(button_center(&c, commands::BUY).is_none());
    c.handle(Event::Command(commands::BUY));
    assert!(c.resources().get::<Account>().unwrap().fills().is_empty());
    c.handle(Event::Key(Key::Alt('P')));
    assert!(button_center(&c, commands::CLOSE_POSITION).is_none());
    let at = button_center(&c, commands::BUY).unwrap();
    click(&mut c, at);
    assert_eq!(c.resources().get::<Account>().unwrap().position(), 1.0);
    let close = button_center(&c, commands::CLOSE_POSITION).expect("a position is open");
    click(&mut c, close);
    assert_eq!(c.resources().get::<Account>().unwrap().position(), 0.0);
    assert!(button_center(&c, commands::CLOSE_POSITION).is_none());
    let at = button_center(&c, commands::SELL).unwrap();
    click(&mut c, at);
    assert_eq!(c.resources().get::<Account>().unwrap().position(), -1.0);
}

#[test]
fn a_click_does_not_pan_the_chart_and_a_drag_from_a_button_does_not_either() {
    let mut c = chart(1000);
    c.handle(Event::Key(Key::Alt('P')));
    let right = c.time_scale().right;
    let at = button_center(&c, commands::BUY).unwrap();
    click(&mut c, at);
    assert_eq!(c.time_scale().right, right);

    c.handle(Event::PointerMoved { x: at.0, y: at.1 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved {
        x: at.0 + 300.0,
        y: at.1 - 200.0,
    });
    c.handle(Event::PointerUp);
    assert_eq!(c.time_scale().right, right, "the drag started on a button");
    assert_eq!(
        c.resources().get::<Account>().unwrap().fills().len(),
        1,
        "released elsewhere: no command"
    );
}

#[test]
fn hover_asks_for_a_redraw_only_when_the_hovered_button_changes() {
    let mut c = replaying(100);
    let a = button_center(&c, commands::BUY).unwrap();
    let b = button_center(&c, commands::SELL).unwrap();
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    assert!(c.handle(Event::PointerMoved { x: a.0, y: a.1 }), "entered a button");
    assert!(
        c.handle(Event::PointerMoved { x: b.0, y: b.1 }),
        "moved to the next one"
    );
}

#[test]
fn the_toolbar_is_drawn_above_everything_with_rounded_buttons() {
    let mut c = replaying(100);
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let layers = scene.layers();
    let last = layers.last().unwrap();
    let rounded = last.rects.clone().filter(|&i| scene.rects[i].radius > 0.0).count();
    assert!(
        rounded >= 3,
        "the toolbar's buttons are in the topmost layer: {rounded}"
    );
    let labels: Vec<_> = scene.texts.iter().map(|t| t.text.as_str()).collect();
    assert!(
        labels.contains(&"Exit replay") && labels.contains(&"Buy") && labels.contains(&"Sell"),
        "{labels:?}"
    );
}

#[test]
fn a_plugin_adds_its_own_button_and_command() {
    #[derive(Default)]
    struct Count(u32);
    struct Counter;
    impl Handler for Counter {
        fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
            if *event == Event::Command("count.up") {
                ctx.res.get_mut::<Count>().unwrap().0 += 1;
                return Response::REDRAW;
            }
            Response::IGNORED
        }
    }
    struct Show;
    impl Layer for Show {
        fn draw(&mut self, _: &mut DrawCtx<'_>) {}
    }
    struct Feature;
    impl Plugin for Feature {
        fn build(&self, chart: &mut Chart) {
            chart.resources_mut().insert(Count::default());
            chart.add_handler("counter", 400, Counter);
            chart.add_layer("show", 5000, Show);
            chart.add_button(
                ToolButton::new(
                    "count.up",
                    Label::Dynamic(|res: &Resources| format!("Count {}", res.get::<Count>().unwrap().0)),
                )
                .kind(ButtonKind::Normal),
            );
            chart.bind(Key::Char('n'), "count.up");
        }
    }
    let mut c = chart(50).with(Feature);
    let at = button_center(&c, "count.up").unwrap();
    click(&mut c, at);
    assert_eq!(c.resources().get::<Count>().unwrap().0, 1);
    c.handle(Event::Key(Key::Char('n')));
    assert_eq!(c.resources().get::<Count>().unwrap().0, 2);
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    assert!(
        scene.texts.iter().any(|t| t.text == "Count 2"),
        "the label follows the state"
    );
}

#[test]
fn a_command_that_emits_itself_cannot_hang_the_chart() {
    struct Loop;
    impl Handler for Loop {
        fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
            if *event == Event::Command("loop") {
                ctx.emit("loop");
            }
            Response::IGNORED
        }
    }
    let mut c = chart(10);
    c.add_handler("loop", 400, Loop);
    c.handle(Event::Command("loop"));
}

#[test]
fn keys_still_work_and_can_be_rebound() {
    let mut c = chart(200);
    c.handle(Event::Key(Key::Alt('P')));
    c.handle(Event::Key(Key::Char('b')));
    assert_eq!(c.resources().get::<Account>().unwrap().position(), 1.0);
    c.resources_mut()
        .get_mut::<builtin::Keymap>()
        .unwrap()
        .unbind(Key::Char('b'))
        .bind(Key::Char('x'), commands::BUY);
    c.handle(Event::Key(Key::Char('b')));
    assert_eq!(c.resources().get::<Account>().unwrap().position(), 1.0, "unbound");
    c.handle(Event::Key(Key::Char('x')));
    assert_eq!(c.resources().get::<Account>().unwrap().position(), 2.0);
}

#[test]
fn no_crosshair_over_a_button() {
    let mut c = replaying(200);
    let dashes = |c: &mut Chart| {
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        scene.rects.iter().filter(|r| r.color == c.style.crosshair).count()
    };
    c.handle(Event::PointerMoved { x: 400.0, y: 200.0 });
    assert!(dashes(&mut c) > 0);
    let at = button_center(&c, commands::BUY).unwrap();
    c.handle(Event::PointerMoved { x: at.0, y: at.1 });
    assert_eq!(dashes(&mut c), 0);
}

#[test]
fn buttons_wrap_into_rows_growing_upwards_when_the_plot_is_narrow() {
    let c = replaying(100);
    let wide = builtin::layout(c.resources(), &c.style, 1.0, 800.0, 500.0);
    let narrow = builtin::layout(c.resources(), &c.style, 1.0, 200.0, 500.0);
    assert_eq!(wide.len(), narrow.len(), "nothing is dropped");
    let bottom = wide[0].2.y;
    assert!(wide.iter().all(|(_, _, r)| r.y == bottom), "one row when there is room");
    assert!(
        narrow.iter().all(|(_, _, r)| r.x + r.w <= 200.0),
        "every button fits the width"
    );
    assert!(
        narrow.first().unwrap().2.y < narrow.last().unwrap().2.y,
        "the first buttons are on the top row"
    );
    assert_eq!(
        narrow.last().unwrap().2.y,
        bottom,
        "the last row sits where the single row was"
    );
    for (i, a) in narrow.iter().enumerate() {
        for b in &narrow[i + 1..] {
            let apart =
                a.2.x + a.2.w <= b.2.x || b.2.x + b.2.w <= a.2.x || a.2.y + a.2.h <= b.2.y || b.2.y + b.2.h <= a.2.y;
            assert!(apart, "{} and {} overlap", a.1, b.1);
        }
    }
}
