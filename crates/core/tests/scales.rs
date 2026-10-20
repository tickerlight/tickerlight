//! Price axis modes: linear, log, percent.

use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Key, PriceMode, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(500));
    chart.resize(872.0, 628.0);
    chart
}

fn axis_texts(chart: &mut Chart) -> Vec<String> {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
        .texts
        .into_iter()
        .filter(|t| t.x > 800.0)
        .map(|t| t.text)
        .collect()
}

#[test]
fn alt_l_and_alt_p_turn_log_and_percent_on_and_off_and_the_axis_says_so() {
    let mut c = chart();
    let plain = axis_texts(&mut c);
    assert!(!plain.iter().any(|t| t == "LOG" || t.ends_with('%')));

    c.handle(Event::Key(Key::Alt('l')));
    let log = axis_texts(&mut c);
    assert!(log.iter().any(|t| t == "LOG"), "{log:?}");

    c.handle(Event::Key(Key::Alt('p')));
    let percent = axis_texts(&mut c);
    assert!(percent.iter().any(|t| t == "%"));
    assert!(
        percent.iter().filter(|t| t.ends_with('%') && t.len() > 1).count() >= 3,
        "{percent:?}"
    );

    c.handle(Event::Key(Key::Alt('p')));
    assert_eq!(axis_texts(&mut c), plain, "and back to linear");
    c.handle(Event::Key(Key::Alt('l')));
    c.handle(Event::Key(Key::Alt('l')));
    assert_eq!(axis_texts(&mut c), plain, "log off again");
}

#[test]
fn the_scale_mode_is_per_pane_and_follows_the_pointer() {
    let mut c = Chart::new(random_walk(500)).with(tickerlight_core::Rsi::new(14));
    c.resize(872.0, 628.0);
    let rsi_top = c.pane_layout()[1].top;
    c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    c.handle(Event::Key(Key::Alt('l')));
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    // Percent on the RSI pane too, to see that the panes are independent.
    c.handle(Event::PointerMoved {
        x: 300.0,
        y: rsi_top + 20.0,
    });
    c.handle(Event::Key(Key::Alt('p')));
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let logs: Vec<f32> = scene.texts.iter().filter(|t| t.text == "LOG").map(|t| t.y).collect();
    assert_eq!(logs.len(), 1);
    assert!(logs[0] < rsi_top, "only the main pane is in log mode");
}

#[test]
fn candles_are_placed_by_ratio_in_log_mode() {
    let mut c = chart();
    c.handle(Event::Key(Key::Alt('l')));
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    // The frame's scale is what handlers see; it is a log scale now.
    struct Probe(std::rc::Rc<std::cell::Cell<Option<PriceMode>>>);
    impl tickerlight_core::Handler for Probe {
        fn handle(&mut self, _: &Event, ctx: &mut tickerlight_core::EventCtx<'_>) -> tickerlight_core::Response {
            self.0.set(ctx.prices[0].map(|p| p.mode));
            tickerlight_core::Response::IGNORED
        }
    }
    let seen = std::rc::Rc::new(std::cell::Cell::new(None));
    c.add_handler("probe", 1, Probe(seen.clone()));
    c.handle(Event::PointerMoved { x: 10.0, y: 10.0 });
    assert_eq!(seen.get(), Some(PriceMode::Log));
}

#[test]
fn a_chart_with_non_positive_prices_stays_linear_in_log_mode() {
    use tickerlight_core::{Candle, Candles, Time};
    let mut candles = Candles::new();
    for i in 0..100 {
        let p = (i as f64 - 50.0) / 10.0;
        candles
            .push(Candle {
                time: Time(i * 60_000),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut c = Chart::new(candles);
    c.resize(872.0, 628.0);
    c.handle(Event::Key(Key::Alt('l')));
    let texts = axis_texts(&mut c);
    assert!(
        !texts.iter().any(|t| t.contains("NaN") || t.contains("inf")),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t == "LOG"),
        "cannot be log, and does not claim to be"
    );
}

mod manual {
    use super::*;

    fn frame(c: &mut Chart) {
        c.build_scene(&mut Scene::new());
    }

    fn range(c: &Chart) -> Option<(f64, f64)> {
        c.pane_stack().manual_range(0)
    }

    fn axis_drag(c: &mut Chart, from: f32, to: f32) {
        frame(c);
        c.handle(Event::PointerMoved { x: 850.0, y: from });
        c.handle(Event::PointerDown);
        c.handle(Event::PointerMoved { x: 850.0, y: to });
        c.handle(Event::PointerUp);
    }

    #[test]
    fn dragging_the_axis_stretches_the_range_around_its_middle_and_does_not_pan() {
        let mut c = chart();
        frame(&mut c);
        assert_eq!(range(&c), None, "autoscale to begin with");
        let time = *c.time_scale();
        axis_drag(&mut c, 200.0, 300.0);
        let (min, max) = range(&c).expect("a manual range");
        assert_eq!(*c.time_scale(), time);
        let before = max - min;
        axis_drag(&mut c, 200.0, 100.0);
        let (min2, max2) = range(&c).unwrap();
        assert!(
            ((min2 + max2) / 2.0 - (min + max) / 2.0).abs() < 1e-6,
            "the middle stays"
        );
        assert!((max2 - min2) < before, "dragging up shrinks the range: magnified");
        assert!(axis_texts(&mut c).len() >= 3, "the axis still has labels");
    }

    #[test]
    fn the_wheel_over_the_axis_zooms_the_price_not_the_time() {
        let mut c = chart();
        frame(&mut c);
        let time = *c.time_scale();
        c.handle(Event::PointerMoved { x: 850.0, y: 200.0 });
        c.handle(Event::Wheel { x: 850.0, delta: 3.0 });
        assert!(range(&c).is_some());
        assert_eq!(*c.time_scale(), time);
        let (min, max) = range(&c).unwrap();
        frame(&mut c);
        c.handle(Event::Wheel { x: 850.0, delta: 3.0 });
        let (min2, max2) = range(&c).unwrap();
        assert!(max2 - min2 < max - min, "positive notches zoom in");

        // Over the plot the wheel is the time zoom as before.
        c.handle(Event::PointerMoved { x: 300.0, y: 200.0 });
        c.handle(Event::Wheel { x: 300.0, delta: 3.0 });
        assert_ne!(*c.time_scale(), time);
        assert_eq!(range(&c), Some((min2, max2)));
    }

    #[test]
    fn a_manual_range_lets_the_plot_drag_move_prices_and_alt_r_resets() {
        let mut c = chart();
        // Without a manual range, dragging in the plot only pans time.
        frame(&mut c);
        c.handle(Event::PointerMoved { x: 300.0, y: 200.0 });
        c.handle(Event::PointerDown);
        c.handle(Event::PointerMoved { x: 340.0, y: 260.0 });
        c.handle(Event::PointerUp);
        assert_eq!(range(&c), None);

        axis_drag(&mut c, 200.0, 210.0);
        let (min, max) = range(&c).unwrap();
        frame(&mut c);
        c.handle(Event::PointerMoved { x: 300.0, y: 200.0 });
        c.handle(Event::PointerDown);
        c.handle(Event::PointerMoved { x: 300.0, y: 260.0 }); // down by a tenth of the 600 px pane
        c.handle(Event::PointerUp);
        let (min2, max2) = range(&c).unwrap();
        assert!(
            (min2 - min - (max - min) * 0.1).abs() < 1e-6,
            "the range moved up by a tenth of itself"
        );
        assert!(((max2 - min2) - (max - min)).abs() < 1e-9);

        // Alt+R resets, with the pointer anywhere over the chart.
        c.handle(Event::Key(Key::Alt('r')));
        assert_eq!(range(&c), None);
    }

    #[test]
    fn changing_the_axis_mode_gives_autoscale_back() {
        let mut c = chart();
        axis_drag(&mut c, 200.0, 250.0);
        assert!(range(&c).is_some());
        c.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
        c.handle(Event::Key(Key::Alt('l')));
        assert_eq!(range(&c), None);
    }

    #[test]
    fn a_double_click_on_the_price_axis_autoscales_and_on_the_time_axis_resets_the_view() {
        let mut c = chart();
        let (w, h) = c.plot_size();
        axis_drag(&mut c, 200.0, 250.0);
        let double_click = |c: &mut Chart, x: f32, y: f32| {
            c.handle(Event::PointerMoved { x, y });
            c.handle(Event::DoubleClick);
        };
        // In the plot it is neither.
        double_click(&mut c, w / 2.0, h / 2.0);
        assert!(range(&c).is_some());
        double_click(&mut c, w + 10.0, 200.0);
        assert_eq!(range(&c), None);
        let time = *c.time_scale();
        c.handle(Event::Wheel { x: w / 2.0, delta: 3.0 });
        axis_drag(&mut c, 200.0, 250.0);
        double_click(&mut c, w / 2.0, h + 5.0);
        assert_eq!(*c.time_scale(), time);
        assert_eq!(range(&c), None);
    }

    #[test]
    fn bad_ranges_are_refused() {
        let mut c = chart();
        c.pane_stack_mut().set_manual_range(0, Some((5.0, 5.0)));
        c.pane_stack_mut().set_manual_range(0, Some((f64::NAN, 1.0)));
        assert_eq!(range(&c), None);
        c.pane_stack_mut().set_manual_range(0, Some((1.0, 2.0)));
        assert_eq!(range(&c), Some((1.0, 2.0)));
        let texts = axis_texts(&mut c);
        assert!(texts.iter().all(|t| !t.contains("NaN")));
    }
}
