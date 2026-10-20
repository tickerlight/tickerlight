//! Plugins that draw the trade tape.

use tickerlight_core::{
    Candle, Candles, Chart, Color, Event, Key, Scene, Side, Time, Trade, Trades, VolumeProfile, candles_from_trades,
};

const MINUTE: i64 = 60_000;
const BUY: Color = Color::rgb(0, 255, 0);
const SELL: Color = Color::rgb(255, 0, 0);

/// Minute `m` has 10 sells and 30 buys at price `100 + m`.
fn tape(minutes: i64) -> Trades {
    let mut t = Trades::new();
    for m in 0..minutes {
        for k in 0..40 {
            let side = if k < 10 { Side::Sell } else { Side::Buy };
            t.push(Trade {
                time: Time(m * MINUTE + k * 1000),
                price: 100.0 + m as f64,
                qty: 1.0,
                side,
            })
            .unwrap();
        }
    }
    t
}

fn chart(minutes: i64) -> Chart {
    let trades = tape(minutes);
    let candles = candles_from_trades(trades.view(), MINUTE);
    let profile = VolumeProfile {
        buy: BUY,
        sell: SELL,
        ..VolumeProfile::default()
    };
    let mut c = Chart::new(candles).with_trades(trades).with(profile);
    c.resize(872.0, 528.0);
    c
}

fn profile_rects(c: &mut Chart) -> (Vec<tickerlight_core::Rect>, Vec<tickerlight_core::Rect>) {
    let mut scene = Scene::new();
    c.build_scene(&mut scene);
    let sells = scene.rects.iter().copied().filter(|r| r.color == SELL).collect();
    let buys = scene.rects.iter().copied().filter(|r| r.color == BUY).collect();
    (sells, buys)
}

#[test]
fn profile_bars_grow_from_the_right_edge_and_split_by_side() {
    let mut c = chart(30);
    let (sells, buys) = profile_rects(&mut c);
    let plot_w = c.plot_size().0;
    assert!(
        !sells.is_empty() && sells.len() == buys.len(),
        "every row has both sides here"
    );
    for (s, b) in sells.iter().zip(&buys) {
        assert_eq!(s.y, b.y, "one row");
        assert!((s.x + s.w - b.x).abs() < 1e-3, "sells then buys");
        assert!((b.x + b.w - plot_w).abs() < 1e-3, "they end at the plot edge");
        // 10 sells against 30 buys.
        assert!((b.w / s.w - 3.0).abs() < 1e-3, "{} vs {}", s.w, b.w);
        assert!(s.w + b.w <= plot_w * 0.25 + 1e-3);
    }
    // The busiest row reaches the maximum width.
    let widest = sells.iter().zip(&buys).map(|(s, b)| s.w + b.w).fold(0.0, f32::max);
    assert!((widest - plot_w * 0.25).abs() < 1e-2);
}

#[test]
fn the_profile_counts_only_what_is_on_screen() {
    let mut c = chart(1000);
    let (sells, _) = profile_rects(&mut c);
    // 100 bars fit on screen: at most that many distinct prices, so at most that many rows.
    assert!(!sells.is_empty() && sells.len() <= 100, "{} rows", sells.len());
}

#[test]
fn a_replaying_chart_draws_exactly_what_a_chart_without_a_future_draws() {
    let mut replaying = chart(200);
    replaying.handle(Event::PointerMoved { x: 300.0, y: 100.0 });
    replaying.handle(Event::Key(Key::Alt('P')));
    replaying.handle(Event::PointerLeft);
    let shown = replaying.seen().len();
    assert!(shown < 200);

    // The same market, but the future never existed.
    let mut past_only = chart(shown as i64);
    // Line the view up: same scale, the latest bar at the same place (the replay keeps room after it).
    past_only.handle(Event::Wheel { x: 0.0, delta: 0.0 });
    let (want, have) = (replaying.time_scale(), past_only.time_scale());
    let dx = ((have.right - want.right) * have.bar_spacing) as f32;
    past_only.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    past_only.handle(Event::PointerDown);
    past_only.handle(Event::PointerMoved {
        x: 400.0 + dx,
        y: 100.0,
    });
    past_only.handle(Event::PointerUp);
    past_only.handle(Event::PointerLeft);
    assert!((past_only.time_scale().right - replaying.time_scale().right).abs() < 1e-6);
    let (a_sells, a_buys) = profile_rects(&mut replaying);
    let (b_sells, b_buys) = profile_rects(&mut past_only);
    assert!(!a_sells.is_empty());
    assert_eq!(a_sells.len(), b_sells.len());
    for (x, y) in a_sells.iter().zip(&b_sells).chain(a_buys.iter().zip(&b_buys)) {
        assert!((x.y - y.y).abs() < 1e-3 && (x.w - y.w).abs() < 1e-3, "{x:?} vs {y:?}");
    }
}

#[test]
fn no_tape_no_profile_and_no_panic() {
    let mut candles = Candles::new();
    candles
        .push(Candle {
            time: Time(0),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 1.0,
        })
        .unwrap();
    let mut c = Chart::new(candles).with(VolumeProfile::default());
    c.resize(872.0, 528.0);
    let (sells, buys) = profile_rects(&mut c);
    assert!(sells.is_empty() && buys.is_empty());
}

mod footprint {
    use super::*;
    use tickerlight_core::Footprint;

    /// One bar with trades at levels 100 and 101: 100 has 3 sold, 101 has 5 bought and 1 sold.
    fn one_bar() -> Chart {
        let mut t = Trades::new();
        let mut add = |time, price, qty, side| {
            t.push(Trade {
                time: Time(time),
                price,
                qty,
                side,
            })
            .unwrap()
        };
        add(0, 100.2, 1.0, Side::Sell);
        add(1, 100.7, 2.0, Side::Sell);
        add(2, 101.1, 5.0, Side::Buy);
        add(3, 101.9, 1.0, Side::Sell);
        let candles = candles_from_trades(t.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(t).with(Footprint::new(1.0));
        c.resize(872.0, 528.0);
        c
    }

    fn zoom_in(c: &mut Chart, notches: i32) {
        for _ in 0..notches {
            c.handle(Event::Wheel { x: 400.0, delta: 1.0 });
        }
    }

    fn cell_texts(c: &mut Chart) -> Vec<String> {
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let (plot_w, plot_h) = c.plot_size();
        // The toolbar sits at the bottom left of the plot and uses the same text color.
        let cells = |t: &&tickerlight_core::Text| t.x < plot_w && t.y < plot_h - 60.0 && t.color == c.style.hud_text;
        scene.texts.iter().filter(cells).map(|t| t.text.clone()).collect()
    }

    #[test]
    fn it_stays_silent_when_zoomed_out() {
        let mut c = one_bar();
        assert!(cell_texts(&mut c).is_empty());
    }

    #[test]
    fn levels_show_sold_and_bought_volume() {
        let mut c = one_bar();
        zoom_in(&mut c, 30);
        let mut texts = cell_texts(&mut c);
        texts.sort();
        // Level 100: 3.00 sold; level 101: 1.00 sold, 5.00 bought. Empty sides show nothing.
        assert_eq!(texts, ["1.00", "3.00", "5.00"]);
    }

    #[test]
    fn the_delta_and_volume_of_a_bar_show_under_it() {
        let mut c = one_bar();
        zoom_in(&mut c, 30);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let low = scene.texts.iter().find(|t| t.text == "3.00").unwrap().y;
        let under: Vec<_> = scene
            .texts
            .iter()
            .filter(|t| t.y > low && t.x < c.plot_size().0)
            .collect();
        // 5 bought less 4 sold, then all 9 traded.
        let delta = under.iter().find(|t| t.text == "+1.00").expect("the delta");
        let volume = under.iter().find(|t| t.text == "9.00").expect("the volume");
        assert_eq!(delta.color, Footprint::new(1.0).buy);
        assert!(delta.y < volume.y);
    }

    #[test]
    fn the_busiest_level_is_outlined() {
        let mut c = one_bar();
        zoom_in(&mut c, 30);
        let mut scene = Scene::new();
        c.build_scene(&mut scene);
        let outline = c.style.hud_text;
        // Two thin rects (top and bottom edge) in the text color and as wide as the whole cell.
        let edges: Vec<_> = scene
            .rects
            .iter()
            .filter(|r| r.color == outline && r.h <= 2.0 && r.w > 50.0)
            .collect();
        assert_eq!(edges.len(), 2, "{edges:?}");
        assert!(edges[0].y < edges[1].y);
    }

    #[test]
    fn grouped_levels_grow_until_their_numbers_fit() {
        let fine = |grouped| {
            let mut c = one_bar();
            c.remove_layer("footprint");
            c.add_plugin(Footprint {
                grouped,
                ..Footprint::new(0.01)
            });
            zoom_in(&mut c, 30);
            let mut texts = cell_texts(&mut c);
            texts.sort();
            texts
        };
        // A cent is a few pixels here: too short to draw. Merged, levels grow to a fraction of a unit: still
        // fine enough to keep the 2.00 sold at 100.7 apart from the 1.00 sold at 100.2.
        assert!(fine(false).is_empty());
        let grouped = fine(true);
        assert!(
            grouped.contains(&"2.00".into()) && grouped.contains(&"5.00".into()),
            "{grouped:?}"
        );
    }

    #[test]
    fn hidden_bars_have_no_footprint() {
        // Two bars: replay shows only the first, whose trades are 100.x; the second's are far away at 500.
        let mut t = Trades::new();
        t.push(Trade {
            time: Time(0),
            price: 100.5,
            qty: 1.0,
            side: Side::Buy,
        })
        .unwrap();
        t.push(Trade {
            time: Time(MINUTE),
            price: 500.5,
            qty: 7.0,
            side: Side::Buy,
        })
        .unwrap();
        let candles = candles_from_trades(t.view(), MINUTE);
        let mut c = Chart::new(candles).with_trades(t).with(Footprint::new(1.0));
        c.resize(872.0, 528.0);
        c.set_cursor(Some(Time(0)));
        zoom_in(&mut c, 30);
        assert_eq!(cell_texts(&mut c), ["1.00"], "the 7.00 of the hidden bar must not show");
    }
}
