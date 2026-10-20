//! Indicators on a chart: their panes, their labels, and taking them off again.

use tickerlight_core::builtin::{Kind, Study, set_studies, studies};
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Key, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(500));
    chart.resize(872.0, 628.0);
    chart
}

fn frame(chart: &mut Chart) -> Scene {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
}

fn has_text(scene: &Scene, start: &str) -> bool {
    scene.texts.iter().any(|t| t.text.starts_with(start))
}

#[test]
fn oscillators_get_panes_and_averages_go_over_the_candles() {
    let mut c = chart();
    let panes = c.pane_names().len();
    set_studies(
        &mut c,
        vec![Study::new(Kind::Sma), Study::new(Kind::Rsi), Study::new(Kind::Macd)],
    );
    assert_eq!(c.pane_names().len(), panes + 2, "{:?}", c.pane_names());
    assert!(c.pane_names().contains(&"rsi-14"));
    let scene = frame(&mut c);
    for label in ["SMA 20", "RSI 14", "MACD 12 26 9"] {
        assert!(has_text(&scene, label), "{label}");
    }
    // The values beside the label are of the newest bar.
    let close: Vec<f64> = c.seen().close().to_vec();
    let sma: f64 = close[close.len() - 20..].iter().sum::<f64>() / 20.0;
    assert!(
        scene.texts.iter().any(|t| t.text == format!("{sma:.3}")),
        "SMA value {sma:.3}"
    );

    // Taken off, they leave no pane and draw nothing.
    set_studies(&mut c, vec![Study::new(Kind::Rsi)]);
    assert_eq!(c.pane_names().len(), panes + 1);
    assert_eq!(studies(&c), [Study::new(Kind::Rsi)]);
    let scene = frame(&mut c);
    assert!(!has_text(&scene, "SMA") && !has_text(&scene, "MACD"));
    set_studies(&mut c, Vec::new());
    assert_eq!(c.pane_names().len(), panes);
}

#[test]
fn two_alike_get_panes_of_their_own() {
    let mut c = chart();
    set_studies(&mut c, vec![Study::new(Kind::Rsi), Study::new(Kind::Rsi)]);
    let names = c.pane_names();
    assert!(names.contains(&"rsi-14") && names.contains(&"rsi-14#2"), "{names:?}");
}

#[test]
fn a_replay_shows_only_what_was_known_then() {
    let mut c = chart();
    set_studies(&mut c, vec![Study::new(Kind::Sma)]);
    frame(&mut c);
    c.handle(Event::Key(Key::Alt('P')));
    let scene = frame(&mut c);
    let close: Vec<f64> = c.seen().close().to_vec();
    assert!(close.len() < 500, "the replay cut the bars");
    let sma: f64 = close[close.len() - 20..].iter().sum::<f64>() / 20.0;
    assert!(scene.texts.iter().any(|t| t.text == format!("{sma:.3}")));
}

#[test]
fn a_label_has_buttons_to_hide_set_up_and_remove_its_indicator() {
    use tickerlight_core::builtin::OpenStudySettings;
    let mut c = chart();
    set_studies(&mut c, vec![Study::new(Kind::Sma), Study::new(Kind::Rsi)]);
    let scene = frame(&mut c);
    let label = |scene: &Scene, name: &str| scene.texts.iter().find(|t| t.text == name).cloned().unwrap();
    let sma = label(&scene, "SMA 20");
    // Its values, on its line, left of the price axis.
    let on_line = |t: &&tickerlight_core::scene::Text| t.y == sma.y && t.x < 700.0 && t.text.contains('.');
    let value_x = |scene: &Scene| scene.texts.iter().find(on_line).unwrap().x;
    let before = value_x(&scene);
    // The buttons come right after the name, 18 px each: hide, settings, remove.
    let buttons = sma.x + tickerlight_text::Font::ui().measure("SMA 20", sma.size) + 4.0;
    let button = |k: f32| (buttons + 18.0 * k + 9.0, sma.y);
    let press = |c: &mut Chart, (x, y): (f32, f32)| {
        c.handle(Event::PointerMoved { x, y });
        c.handle(Event::PointerDown);
        c.handle(Event::PointerUp);
    };

    // Pointed at, the label makes room for them.
    c.handle(Event::PointerMoved {
        x: sma.x + 4.0,
        y: sma.y,
    });
    let scene = frame(&mut c);
    assert!(value_x(&scene) >= before + 3.0 * 18.0);

    // Hidden: kept on the list, the name stays, the values and the line go.
    let lines = |c: &mut Chart| frame(c).lines.len();
    let with_line = lines(&mut c);
    press(&mut c, button(0.0));
    assert!(studies(&c)[0].hidden);
    let scene = frame(&mut c);
    assert!(has_text(&scene, "SMA 20"));
    assert!(scene.texts.iter().find(on_line).is_none());
    assert!(lines(&mut c) < with_line);
    assert!(Study::parse(&studies(&c)[0].save()).unwrap().hidden, "kept hidden");
    // And shown again.
    press(&mut c, button(0.0));
    assert!(!studies(&c)[0].hidden);

    // Settings are the program's to show.
    press(&mut c, button(1.0));
    assert_eq!(c.resources().get::<OpenStudySettings>(), Some(&OpenStudySettings(0)));

    // Removed from its own pane, the pane goes with it.
    let panes = c.pane_names().len();
    let rsi = label(&frame(&mut c), "RSI 14");
    let x = rsi.x + tickerlight_text::Font::ui().measure("RSI 14", rsi.size) + 4.0 + 18.0 * 2.0 + 9.0;
    assert!(rsi.y > c.pane_layout()[1].top, "in its pane");
    press(&mut c, (x, rsi.y));
    assert_eq!(studies(&c), [Study::new(Kind::Sma)]);
    assert_eq!(c.pane_names().len(), panes - 1);
}
