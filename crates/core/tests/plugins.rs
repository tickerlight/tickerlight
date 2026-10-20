//! Plugins written against the public API only, the way a user of the library would write them.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use tickerlight_core::plugin::{names, order};
use tickerlight_core::{
    Candle, CandleView, Candles, Chart, Color, DrawCtx, Event, EventCtx, Handler, Layer, Plugin, Resources, Response,
    Scene, Sma, Time,
};

fn candles(n: i64, price: impl Fn(i64) -> f64) -> Candles {
    let mut c = Candles::new();
    for i in 0..n {
        let p = price(i);
        c.push(Candle {
            time: Time(i * 60_000),
            open: p,
            high: p + 1.0,
            low: p - 1.0,
            close: p,
            volume: 1.0,
        })
        .unwrap();
    }
    c
}

fn chart(n: i64) -> Chart {
    let mut chart = Chart::new(candles(n, |i| 100.0 + (i % 10) as f64));
    chart.resize(872.0, 528.0);
    chart
}

fn scene_of(chart: &mut Chart) -> Scene {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
}

/// A layer that records what it was allowed to see.
struct Spy(Rc<RefCell<Vec<usize>>>);

impl Layer for Spy {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        self.0.borrow_mut().push(ctx.view.len());
        // Whatever the plugin does, the last bar it can read is the last one before the cursor.
        if let Some(last) = ctx.view.time().last() {
            assert!(last.0 <= 49 * 60_000);
        }
    }
}

#[test]
fn a_custom_layer_only_sees_bars_up_to_the_replay_cursor() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut c = chart(100);
    c.add_layer("spy", order::OVERLAY, Spy(seen.clone()));
    c.set_cursor(Some(Time(49 * 60_000)));
    scene_of(&mut c);
    assert_eq!(*seen.borrow(), [50]);
}

struct Flag(Color);

impl Layer for Flag {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        ctx.scene.fill(0.0, 0.0, 5.0, 5.0, self.0);
    }
}

const MARK: Color = Color::rgb(1, 2, 3);
const OTHER: Color = Color::rgb(4, 5, 6);

fn has(scene: &Scene, color: Color) -> bool {
    scene.rects.iter().any(|r| r.color == color)
}

#[test]
fn builtin_layers_can_be_removed_and_replaced_by_name() {
    let mut c = chart(100);
    let up = c.style.candle.up;
    assert!(has(&scene_of(&mut c), up) || has(&scene_of(&mut c), c.style.candle.down));

    assert!(c.remove_layer(names::CANDLES));
    assert!(!c.remove_layer(names::CANDLES), "already gone");
    let scene = scene_of(&mut c);
    assert!(!has(&scene, c.style.candle.up) && !has(&scene, c.style.candle.down));
    assert!(!scene.texts.is_empty(), "axes are still there");

    c.add_layer(names::AXES, order::AXES, Flag(MARK));
    let scene = scene_of(&mut c);
    assert!(has(&scene, MARK));
    assert!(
        scene.texts.iter().all(|t| t.color != c.style.axis_text),
        "the axes were replaced, not added to"
    );
    assert_eq!(c.layer_names().iter().filter(|n| **n == names::AXES).count(), 1);
}

#[test]
fn layers_are_drawn_in_order_regardless_of_insertion() {
    let mut c = Chart::empty(candles(10, |_| 1.0));
    c.add_layer("top", 20, Flag(MARK));
    c.add_layer("bottom", 10, Flag(OTHER));
    c.add_layer("same order, later", 20, Flag(MARK));
    assert_eq!(c.layer_names(), ["bottom", "top", "same order, later"]);
    let scene = scene_of(&mut c);
    assert_eq!(
        scene.rects.iter().map(|r| r.color).collect::<Vec<_>>(),
        [OTHER, MARK, MARK]
    );
    assert_eq!(scene.layers().len(), 3, "every layer is its own scene layer");
}

#[test]
fn an_empty_chart_draws_only_its_background() {
    let mut c = Chart::empty(candles(10, |_| 1.0));
    let scene = scene_of(&mut c);
    assert!(scene.rects.is_empty() && scene.lines.is_empty() && scene.texts.is_empty());
    assert!(
        !c.handle(Event::Wheel { x: 10.0, delta: 1.0 }),
        "no handlers, nothing reacts"
    );
}

struct Log(&'static str, Rc<RefCell<Vec<&'static str>>>, Response);

impl Handler for Log {
    fn handle(&mut self, _: &Event, _: &mut EventCtx<'_>) -> Response {
        self.1.borrow_mut().push(self.0);
        self.2
    }
}

#[test]
fn handlers_run_in_order_until_one_consumes_the_event() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut c = Chart::empty(candles(10, |_| 1.0));
    c.add_handler("c", 30, Log("c", log.clone(), Response::IGNORED));
    c.add_handler("a", 10, Log("a", log.clone(), Response::REDRAW));
    c.add_handler("b", 20, Log("b", log.clone(), Response::CONSUMED));
    assert_eq!(c.handler_names(), ["a", "b", "c"]);

    assert!(c.handle(Event::PointerLeft), "a asked for a redraw");
    assert_eq!(*log.borrow(), ["a", "b"], "c never saw it");

    assert!(c.remove_handler("b"));
    log.borrow_mut().clear();
    c.handle(Event::PointerLeft);
    assert_eq!(*log.borrow(), ["a", "c"]);
}

/// Takes itself off the chart, with a layer, on the first key.
struct OneShot;

impl Handler for OneShot {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if !matches!(event, Event::Key(_)) {
            return Response::IGNORED;
        }
        ctx.defer(|chart| {
            chart.remove_handler("one-shot");
            chart.remove_layer(names::GRID);
        });
        Response::CONSUMED
    }
}

#[test]
fn a_handler_can_change_the_chart_once_the_event_is_done() {
    let mut c = chart(10);
    c.add_handler("one-shot", 1, OneShot);
    assert!(
        c.handle(Event::Key(tickerlight_core::Key::Escape)),
        "a change is a redraw"
    );
    assert!(!c.handler_names().contains(&"one-shot"));
    assert!(!c.layer_names().contains(&names::GRID));
}

#[test]
fn a_handler_can_stop_the_builtin_pan_and_zoom() {
    let mut c = chart(1000);
    let right = c.time_scale().right;
    // Order below INTERACTION: runs first and swallows wheel events.
    struct Freeze;
    impl Handler for Freeze {
        fn handle(&mut self, event: &Event, _: &mut EventCtx<'_>) -> Response {
            if matches!(event, Event::Wheel { .. }) {
                Response::CONSUMED
            } else {
                Response::IGNORED
            }
        }
    }
    c.add_handler("freeze", order::INTERACTION - 1, Freeze);
    assert!(!c.handle(Event::Wheel { x: 100.0, delta: 3.0 }));
    assert_eq!(c.time_scale().right, right);
    assert_eq!(c.time_scale().bar_spacing, 8.0);
}

#[test]
fn handlers_share_state_with_layers_through_resources() {
    #[derive(Default)]
    struct Clicks(u32);
    struct Count;
    impl Handler for Count {
        fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
            if *event == Event::PointerUp {
                ctx.res.get_mut::<Clicks>().unwrap().0 += 1;
                return Response::REDRAW;
            }
            Response::IGNORED
        }
    }
    struct Show;
    impl Layer for Show {
        fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
            let n = ctx.res.get::<Clicks>().map_or(0, |c| c.0);
            ctx.scene
                .text(0.0, 0.0, 12.0, MARK, tickerlight_core::Align::Left, n.to_string());
        }
    }
    struct Counter;
    impl Plugin for Counter {
        fn build(&self, chart: &mut Chart) {
            chart.resources_mut().insert(Clicks::default());
            chart.add_handler("count", 50, Count);
            chart.add_layer("show", order::OVERLAY, Show);
        }
    }

    let mut c = Chart::empty(candles(10, |_| 1.0)).with(Counter);
    assert_eq!(scene_of(&mut c).texts[0].text, "0");
    assert!(c.handle(Event::PointerUp));
    assert!(c.handle(Event::PointerUp));
    assert_eq!(scene_of(&mut c).texts[0].text, "2");
}

/// A layer whose price range is far away from the candles.
struct Band(f64, f64);

impl Layer for Band {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let (top, bottom) = (ctx.price.y_of(self.1) as f32, ctx.price.y_of(self.0) as f32);
        ctx.scene.fill(0.0, top, 10.0, bottom - top, MARK);
    }

    fn price_extent(&self, _: CandleView<'_>, _: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
        Some((self.0, self.1))
    }
}

#[test]
fn layers_can_widen_the_autoscale() {
    let mut c = chart(200);
    let (plot_w, plot_h) = c.plot_size();
    let candle_height = |c: &mut Chart| {
        let s = scene_of(c);
        let up = c.style.candle.up;
        let down = c.style.candle.down;
        s.rects
            .iter()
            .filter(|r| (r.color == up || r.color == down) && r.x < plot_w)
            .map(|r| r.h)
            .fold(0.0, f32::max)
    };
    let alone = candle_height(&mut c);

    c.add_layer("band", order::OVERLAY, Band(0.0, 1000.0));
    let with_band = candle_height(&mut c);
    assert!(
        with_band < alone / 10.0,
        "candles shrink when the scale must also fit 0..1000: {alone} -> {with_band}"
    );
    // The band itself fits on screen: its rect spans (nearly) the whole plot minus margins.
    let s = scene_of(&mut c);
    let band = s.rects.iter().find(|r| r.color == MARK).unwrap();
    assert!(band.y >= 0.0 && band.y + band.h <= plot_h + 1.0, "{band:?}");
    assert!(band.h > plot_h * 0.7);
}

#[test]
fn non_finite_extents_are_ignored() {
    let mut c = chart(50);
    struct Bad;
    impl Layer for Bad {
        fn draw(&mut self, _: &mut DrawCtx<'_>) {}
        fn price_extent(&self, _: CandleView<'_>, _: Range<usize>, _: &Resources) -> Option<(f64, f64)> {
            Some((f64::NAN, f64::INFINITY))
        }
    }
    c.add_layer("bad", order::OVERLAY, Bad);
    let s = scene_of(&mut c);
    assert!(s.rects.len() > 10);
    assert!(
        s.rects
            .iter()
            .all(|r| r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite())
    );
}

#[test]
fn sma_overlay_draws_a_line_and_stays_in_the_replay_bounds() {
    let mut c = Chart::new(candles(100, |i| i as f64));
    c.resize(872.0, 528.0);
    c.add_plugin(Sma::new(10, MARK));
    let s = scene_of(&mut c);
    assert!(s.lines.len() > 50, "{} segments", s.lines.len());
    assert!(s.lines.iter().all(|l| l.color == MARK));
    // It is above the candles: a later scene layer.
    let layers = s.layers();
    let line_layer = layers.iter().position(|l| !l.lines.is_empty()).unwrap();
    let candle = s.rects.iter().position(|r| r.color == c.style.candle.up).unwrap();
    let candle_layer = layers.iter().position(|l| l.rects.contains(&candle)).unwrap();
    assert!(line_layer > candle_layer);

    // Replay at bar 29: 21 averages exist (bars 9..=29) so 20 segments, and none goes past bar 29.
    c.set_cursor(Some(Time(29 * 60_000)));
    let s = scene_of(&mut c);
    assert_eq!(s.lines.len(), 20);
    let last_x = c.time_scale().x_of(29.5) as f32;
    assert!(s.lines.iter().all(|l| l.x1 <= last_x + 0.01));
}

fn drag(chart: &mut Chart, from: f32, to: f32, ticks: usize, dt: f32) {
    chart.handle(Event::PointerMoved { x: from, y: 100.0 });
    chart.handle(Event::PointerDown);
    for i in 1..=ticks {
        let x = from + (to - from) * i as f32 / ticks as f32;
        chart.handle(Event::PointerMoved { x, y: 100.0 });
        chart.handle(Event::Tick { dt });
    }
}

#[test]
fn a_fast_drag_glides_after_release_and_then_stops() {
    let mut c = chart(10_000);
    // 20 ticks of 16 ms, 15 px each: ~940 px/s towards older bars.
    drag(&mut c, 100.0, 400.0, 20, 0.016);
    assert!(c.needs_tick(), "dragging asks for ticks");
    c.handle(Event::PointerUp);
    assert!(c.needs_tick(), "and so does the glide");

    let released = c.time_scale().right;
    let mut previous = released;
    let mut steps = Vec::new();
    for _ in 0..600 {
        if !c.needs_tick() {
            break;
        }
        assert!(c.handle(Event::Tick { dt: 0.016 }) || !c.needs_tick());
        let right = c.time_scale().right;
        steps.push(previous - right);
        previous = right;
    }
    assert!(!c.needs_tick(), "the glide ends");
    assert!(
        released - c.time_scale().right > 20.0,
        "it went on: {released} -> {}",
        c.time_scale().right
    );
    // Each step is smaller than the one before (within float noise): it decelerates.
    assert!(steps.windows(2).all(|w| w[1] <= w[0] + 1e-9), "{steps:?}");
}

#[test]
fn a_slow_drag_or_a_pause_before_release_does_not_glide() {
    let mut slow = chart(10_000);
    drag(&mut slow, 100.0, 110.0, 20, 0.016); // ~30 px/s
    slow.handle(Event::PointerUp);
    assert!(!slow.needs_tick());

    let mut paused = chart(10_000);
    drag(&mut paused, 100.0, 400.0, 20, 0.016);
    for _ in 0..10 {
        paused.handle(Event::Tick { dt: 0.016 }); // finger rests
    }
    paused.handle(Event::PointerUp);
    assert!(!paused.needs_tick());
}

#[test]
fn touching_or_zooming_stops_a_glide() {
    let mut c = chart(10_000);
    drag(&mut c, 100.0, 400.0, 20, 0.016);
    c.handle(Event::PointerUp);
    c.handle(Event::Tick { dt: 0.016 });
    assert!(c.needs_tick());
    c.handle(Event::Wheel { x: 100.0, delta: 1.0 });
    assert!(!c.needs_tick());

    drag(&mut c, 100.0, 400.0, 20, 0.016);
    c.handle(Event::PointerUp);
    c.handle(Event::PointerDown); // pointer is still over the chart: grab it
    c.handle(Event::PointerUp);
    assert!(!c.needs_tick());
}

#[test]
fn a_glide_stops_at_the_end_of_the_data() {
    let mut c = chart(100);
    drag(&mut c, 100.0, 700.0, 10, 0.016); // fast towards the oldest bars, where the data ends soon
    c.handle(Event::PointerUp);
    for _ in 0..2000 {
        c.handle(Event::Tick { dt: 0.016 });
    }
    assert!(!c.needs_tick());
    assert_eq!(c.time_scale().right, 50.0, "half a screen of empty space, no more");
}

#[test]
fn a_line_series_replaces_the_candles() {
    let mut c = chart(500);
    // The last price keeps the colors of the bars.
    c.remove_layer(names::LAST_PRICE);
    c.add_plugin(tickerlight_core::LineSeries::new(MARK)); // same layer name as the candles: a replacement
    assert_eq!(c.layer_names().iter().filter(|n| **n == names::CANDLES).count(), 1);
    let s = scene_of(&mut c);
    assert!(s.lines.len() > 50 && s.lines.iter().all(|l| l.color == MARK));
    assert!(!has(&s, c.style.candle.up) && !has(&s, c.style.candle.down));
}

#[test]
fn a_line_series_autoscales_to_its_closes() {
    // Wicks reach 1 unit past the close, the line must not leave room for them.
    let mut c = Chart::empty(candles(200, |i| 100.0 + (i % 5) as f64)).with(tickerlight_core::LineSeries::new(MARK));
    c.resize(872.0, 528.0);
    let (lo, hi) = c.seen().extent().unwrap();
    let s = scene_of(&mut c);
    let ys: Vec<f32> = s.lines.iter().flat_map(|l| [l.y0, l.y1]).collect();
    let span = ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min);
    // Closes span 4 units, the candle extent 6: the line fills more of the plot than 4/6 would allow with wicks.
    assert!(span > 500.0 * 0.7, "span {span}, extent {lo}..{hi}");
}

mod live {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tickerlight_core::{Key, Sink, Source};

    /// Delivers the queued candles one per poll, then reports it is done.
    struct Feed(Rc<RefCell<Vec<Candle>>>);

    impl Source for Feed {
        fn poll(&mut self, sink: &mut Sink) -> bool {
            let mut queue = self.0.borrow_mut();
            if !queue.is_empty() {
                sink.candle(queue.remove(0));
            }
            !queue.is_empty()
        }
    }

    fn candle(i: i64, close: f64) -> Candle {
        Candle {
            time: Time(i * 60_000),
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 1.0,
        }
    }

    #[test]
    fn a_source_feeds_the_chart_on_ticks_and_the_forming_candle_updates() {
        let queue = Rc::new(RefCell::new(vec![candle(100, 5.0), candle(100, 6.0), candle(101, 7.0)]));
        let mut c = chart(100); // bars 0..=99
        c.add_source(Feed(queue.clone()));
        assert!(c.needs_tick(), "a live source needs ticks");
        let right = c.time_scale().right;

        assert!(c.handle(Event::Tick { dt: 0.016 }));
        assert_eq!(c.seen().len(), 101);
        assert_eq!(
            c.time_scale().right,
            right + 1.0,
            "scrolled to the newest bar: it follows"
        );
        assert!(c.handle(Event::Tick { dt: 0.016 }));
        assert_eq!(
            c.seen().len(),
            101,
            "same time: the forming candle changed, nothing appended"
        );
        assert_eq!(c.seen().close().last(), Some(&6.0));
        assert!(c.handle(Event::Tick { dt: 0.016 }));
        assert_eq!(c.seen().len(), 102);
        assert!(!c.needs_tick(), "the source finished");
        assert!(!c.handle(Event::Tick { dt: 0.016 }), "nothing left to show");
    }

    #[test]
    fn live_data_stays_hidden_during_a_replay() {
        let queue = Rc::new(RefCell::new(vec![candle(100, 5.0)]));
        let mut c = chart(100);
        c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
        c.handle(Event::Key(Key::Alt('P')));
        c.handle(Event::PointerLeft);
        let shown = c.seen().len();
        c.add_source(Feed(queue));
        c.handle(Event::Tick { dt: 0.016 });
        assert_eq!(c.seen().len(), shown, "new bars arrive behind the replay cursor");
    }

    #[test]
    fn an_out_of_order_candle_is_dropped_not_fatal() {
        let queue = Rc::new(RefCell::new(vec![candle(5, 1.0), candle(100, 2.0)]));
        let mut c = chart(100);
        c.add_source(Feed(queue));
        c.handle(Event::Tick { dt: 0.016 });
        assert_eq!(c.seen().len(), 100);
        c.handle(Event::Tick { dt: 0.016 });
        assert_eq!(c.seen().len(), 101);
    }
}
