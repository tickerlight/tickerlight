//! Panes: stacked areas with their own price scale, sharing the time axis.

use tickerlight_core::builtin::Rsi;
use tickerlight_core::plugin::names;
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, ClipRect, Event, Panes, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(500)).with(Rsi::new(14));
    chart.resize(872.0, 628.0); // plot 800 x 600
    chart
}

fn scene(chart: &mut Chart) -> Scene {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
}

#[test]
fn panes_split_the_plot_by_weight_and_fill_it() {
    let c = chart();
    assert_eq!(c.pane_names(), [names::MAIN_PANE, "rsi"]);
    let layout = c.pane_layout();
    assert_eq!(layout.len(), 2);
    assert_eq!(layout[0].top, 0.0);
    // 1.0 : 0.35 of 600 px.
    assert_eq!(layout[0].height, (600.0_f32 / 1.35).round());
    assert_eq!(layout[1].top, layout[0].height);
    assert_eq!(layout[0].height + layout[1].height, 600.0);
}

#[test]
fn a_single_pane_chart_is_unchanged() {
    let mut c = Chart::new(random_walk(500));
    c.resize(872.0, 628.0);
    assert_eq!(c.pane_layout().len(), 1);
    assert_eq!((c.pane_layout()[0].top, c.pane_layout()[0].height), (0.0, 600.0));
}

#[test]
fn every_layer_is_cut_to_its_pane() {
    let mut c = chart();
    let layout = c.pane_layout();
    let scene = scene(&mut c);
    let layers = scene.layers();
    // The RSI line and its texts lie inside the second pane's plot.
    let rsi_text = scene
        .texts
        .iter()
        .find(|t| t.text.starts_with("RSI 14"))
        .expect("the RSI readout");
    assert!(
        rsi_text.y > layout[1].top && rsi_text.y < layout[1].top + layout[1].height,
        "{}",
        rsi_text.y
    );
    let clipped: Vec<ClipRect> = layers.iter().filter_map(|l| l.clip).collect();
    assert!(
        clipped
            .iter()
            .any(|c| c.y == layout[1].top && c.h == layout[1].height && c.w == 800.0),
        "a layer clipped to the plot of pane 2"
    );
    assert!(
        clipped
            .iter()
            .any(|c| c.y == 0.0 && c.h == layout[0].height && c.w == 800.0),
        "and one to pane 1"
    );
    // The axes are clipped to the whole pane, the last one including the time axis.
    assert!(
        clipped
            .iter()
            .any(|c| c.y == layout[1].top && c.h == layout[1].height + 28.0 && c.w == 872.0)
    );
    assert!(
        clipped
            .iter()
            .any(|c| c.y == 0.0 && c.h == layout[0].height && c.w == 872.0)
    );
    // Every rect a layer draws stays where its clip allows (the axes may use the whole pane).
    for layer in &layers {
        let Some(clip) = layer.clip else { continue };
        for r in &scene.rects[layer.rects.clone()] {
            assert!(
                r.y >= clip.y - 1.0 && r.y + r.h <= clip.y + clip.h + 1.0 || r.h > clip.h,
                "{r:?} outside {clip:?}"
            );
        }
    }
}

#[test]
fn each_pane_has_its_own_price_axis_and_only_the_last_has_time_labels() {
    let mut c = chart();
    let layout = c.pane_layout();
    let scene = scene(&mut c);
    let axis_x = 800.0;
    let labels_in = |top: f32, height: f32| {
        scene
            .texts
            .iter()
            .filter(|t| t.x > axis_x && t.y > top && t.y < top + height)
            .count()
    };
    assert!(
        labels_in(layout[0].top, layout[0].height) >= 3,
        "price labels of the main pane"
    );
    assert!(
        labels_in(layout[1].top, layout[1].height) >= 2,
        "and of the RSI pane (0..100)"
    );
    let below_plot = scene.texts.iter().filter(|t| t.y > 600.0 && t.x < axis_x).count();
    assert!(below_plot >= 3, "time labels under the last pane: {below_plot}");
    let between = scene
        .texts
        .iter()
        .filter(|t| {
            t.x < axis_x && t.y > layout[0].height - 12.0 && t.y < layout[0].height + 4.0 && !t.text.starts_with("RSI")
        })
        .count();
    assert_eq!(between, 0, "no time labels between the panes");
}

#[test]
fn the_crosshair_line_runs_through_all_panes_and_the_price_label_is_in_one() {
    let mut c = chart();
    let layout = c.pane_layout();
    c.handle(Event::PointerMoved {
        x: 300.0,
        y: layout[1].top + 30.0,
    }); // over the RSI pane
    let scene = scene(&mut c);
    let vertical_in = |top: f32, height: f32| {
        scene
            .rects
            .iter()
            .filter(|r| {
                r.w <= 1.0
                    && r.h > 1.0
                    && r.h <= 8.0
                    && r.y >= top
                    && r.y < top + height
                    && r.color == c.style.crosshair
            })
            .count()
    };
    assert!(vertical_in(0.0, layout[0].height) > 10, "dashes in the main pane");
    assert!(vertical_in(layout[1].top, layout[1].height) > 3, "and in the RSI pane");
    let on_axis =
        |color: tickerlight_core::Color| scene.texts.iter().filter(|t| t.color == color && t.x > 800.0).count();
    let crosshair: Vec<_> = scene
        .texts
        .iter()
        .filter(|t| t.color == c.style.crosshair_label_text && t.x > 800.0)
        .collect();
    assert_eq!(crosshair.len(), 1, "one price label, {crosshair:?}");
    assert!(crosshair[0].y > layout[0].height, "in the pane the pointer is over");
    let (up, down) = (c.style.candle.up.ink(), c.style.candle.down.ink());
    let last = if up == down {
        on_axis(up)
    } else {
        on_axis(up) + on_axis(down)
    };
    assert_eq!(last, 1, "and the last price's in the main pane");
}

#[test]
fn the_price_under_the_pointer_belongs_to_its_pane() {
    let mut c = chart();
    c.build_scene(&mut Scene::new());
    let layout = c.pane_layout();
    type Seen = std::rc::Rc<std::cell::RefCell<Vec<(Option<usize>, Option<f64>)>>>;
    struct Probe(Seen);
    impl tickerlight_core::Handler for Probe {
        fn handle(&mut self, event: &Event, ctx: &mut tickerlight_core::EventCtx<'_>) -> tickerlight_core::Response {
            if matches!(event, Event::PointerMoved { .. }) {
                self.0.borrow_mut().push((ctx.pointer_pane(), ctx.pointer_price()));
            }
            tickerlight_core::Response::IGNORED
        }
    }
    let seen = Default::default();
    c.add_handler("probe", 1, Probe(std::rc::Rc::clone(&seen)));
    c.handle(Event::PointerMoved { x: 100.0, y: 100.0 });
    c.handle(Event::PointerMoved {
        x: 100.0,
        y: layout[1].top + layout[1].height / 2.0,
    });
    c.handle(Event::PointerMoved { x: 850.0, y: 100.0 }); // over the axis
    let seen = seen.borrow();
    assert_eq!(seen[0].0, Some(0));
    assert_eq!(seen[1].0, Some(1));
    let rsi_price = seen[1].1.unwrap();
    assert!(
        (rsi_price - 50.0).abs() < 10.0,
        "mid pane of a 0..100 scale (with margin): {rsi_price}"
    );
    assert_eq!(seen[2], (None, None));
}

#[test]
fn removing_a_pane_removes_its_layers_and_the_main_pane_stays() {
    let mut c = chart();
    assert!(c.layer_names().contains(&"rsi"));
    assert!(c.remove_pane("rsi"));
    assert!(!c.layer_names().contains(&"rsi"));
    assert!(!c.remove_pane(names::MAIN_PANE));
    assert_eq!(c.pane_layout().len(), 1);
}

#[test]
fn a_layer_in_an_unknown_pane_is_simply_not_drawn() {
    struct Mark;
    impl tickerlight_core::Layer for Mark {
        fn draw(&mut self, ctx: &mut tickerlight_core::DrawCtx<'_>) {
            ctx.scene
                .fill(0.0, 0.0, 5.0, 5.0, tickerlight_core::Color::rgb(1, 2, 3));
        }
    }
    let mut c = chart();
    c.add_layer_in(Panes::One("nowhere"), "mark", 1, Mark);
    let s = scene(&mut c);
    assert!(!s.rects.iter().any(|r| (r.color.r, r.color.g, r.color.b) == (1, 2, 3)));
}

#[test]
fn dragging_a_separator_resizes_the_two_panes_and_does_not_pan() {
    let mut c = chart();
    let before = c.pane_layout();
    let time = *c.time_scale();
    let y = before[1].top;
    c.handle(Event::PointerMoved { x: 300.0, y: y + 1.0 });
    c.handle(Event::PointerDown);
    assert!(c.handle(Event::PointerMoved { x: 340.0, y: y - 99.0 }));
    c.handle(Event::PointerUp);
    let after = c.pane_layout();
    assert_eq!(after[0].height, before[0].height - 100.0);
    assert_eq!(after[1].height, before[1].height + 100.0);
    assert_eq!(after[0].height + after[1].height, 600.0);
    assert_eq!(*c.time_scale(), time, "the chart did not pan sideways");

    // Nothing is left grabbed: moving with the button up changes nothing.
    c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    assert_eq!(c.pane_layout(), after);
}

#[test]
fn a_pane_cannot_be_dragged_away() {
    let mut c = chart();
    let y = c.pane_layout()[1].top;
    c.handle(Event::PointerMoved { x: 300.0, y });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved { x: 300.0, y: 0.0 });
    assert!(c.pane_layout()[0].height >= 40.0);
    c.handle(Event::PointerMoved { x: 300.0, y: 5000.0 });
    assert!(c.pane_layout()[1].height >= 40.0);
    c.handle(Event::PointerUp);
}

#[test]
fn a_press_away_from_a_separator_still_pans() {
    let mut c = chart();
    let time = *c.time_scale();
    c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved { x: 340.0, y: 100.0 });
    c.handle(Event::PointerUp);
    assert_ne!(*c.time_scale(), time);
}
