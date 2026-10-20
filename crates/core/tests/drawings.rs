//! Drawing tools: placing, picking, moving and deleting, driven only by input events.

use tickerlight_core::builtin::{Drawings, Rsi, Tool};
use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Event, Key, PointerShape, Scene};

fn chart() -> Chart {
    let mut chart = Chart::new(random_walk(500)).with(Rsi::new(14));
    chart.resize(872.0, 628.0); // plot 800 x 600, main pane 444 px, RSI below it
    chart
}

/// Handlers use the price scale of the last frame, so draw one first, as a window does.
fn frame(chart: &mut Chart) -> Scene {
    let mut scene = Scene::new();
    chart.build_scene(&mut scene);
    scene
}

fn key(chart: &mut Chart, key: Key) {
    frame(chart);
    chart.handle(Event::Key(key));
}

fn click(chart: &mut Chart, x: f32, y: f32) {
    frame(chart);
    chart.handle(Event::PointerMoved { x, y });
    chart.handle(Event::PointerDown);
    chart.handle(Event::PointerUp);
}

fn drag(chart: &mut Chart, from: (f32, f32), to: (f32, f32)) {
    frame(chart);
    chart.handle(Event::PointerMoved { x: from.0, y: from.1 });
    chart.handle(Event::PointerDown);
    for i in 1..=10 {
        let t = i as f32 / 10.0;
        chart.handle(Event::PointerMoved {
            x: from.0 + (to.0 - from.0) * t,
            y: from.1 + (to.1 - from.1) * t,
        });
    }
    chart.handle(Event::PointerUp);
}

fn drawings(chart: &Chart) -> &Drawings {
    chart.resources().get::<Drawings>().unwrap()
}

#[test]
fn alt_h_then_a_click_places_a_horizontal_line_at_the_price_under_the_pointer() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    assert_eq!(drawings(&c).tool, Some(Tool::HLine));
    let time = *c.time_scale();
    click(&mut c, 300.0, 120.0);
    let d = drawings(&c);
    assert_eq!(d.items.len(), 1);
    assert_eq!((d.items[0].tool, d.items[0].pane), (Tool::HLine, "main"));
    assert_eq!(d.tool, None, "the tool is used up");
    assert_eq!(d.selected, Some(0));
    assert_eq!(*c.time_scale(), time, "the click did not start a pan");

    // The line and its price label on the axis.
    let price = d.items[0].a.price;
    let scene = frame(&mut c);
    let label = format!("{price:.3}");
    assert!(
        scene.texts.iter().any(|t| t.text == label && t.x > 800.0),
        "axis label {label}"
    );
    let y = scene
        .lines
        .iter()
        .find(|l| l.y0 == l.y1 && (l.y0 - 120.0).abs() < 2.0 && l.x1 - l.x0 > 700.0);
    assert!(y.is_some(), "a line across the plot at the click");
}

#[test]
fn a_trend_line_takes_two_clicks_or_one_drag() {
    let mut c = chart();
    key(&mut c, Key::Alt('t'));
    click(&mut c, 100.0, 300.0);
    assert!(drawings(&c).pending.is_some());
    assert!(drawings(&c).items.is_empty());
    frame(&mut c);
    c.handle(Event::PointerMoved { x: 400.0, y: 100.0 });
    let preview = drawings(&c).pending.clone().unwrap();
    assert!(
        preview.b.price > preview.a.price && preview.b.time > preview.a.time,
        "the preview follows the pointer"
    );
    click(&mut c, 400.0, 100.0);
    assert_eq!(drawings(&c).items.len(), 1);
    let line = drawings(&c).items[0].clone();
    assert!(
        line.a.time < line.b.time && line.a.price < line.b.price,
        "from lower left to upper right: {line:?}"
    );

    key(&mut c, Key::Alt('t'));
    drag(&mut c, (100.0, 200.0), (300.0, 250.0));
    assert_eq!(drawings(&c).items.len(), 2, "press, drag and release is one gesture");
    assert!(drawings(&c).pending.is_none());

    // A touchpad drags with a tap and then a tap held down: the first tap starts the line, the held one ends it
    // where it is let go, and the chart does not pan.
    key(&mut c, Key::Alt('t'));
    let time = *c.time_scale();
    click(&mut c, 150.0, 300.0);
    drag(&mut c, (150.0, 300.0), (350.0, 150.0));
    assert_eq!(drawings(&c).items.len(), 3);
    let line = drawings(&c).items[2].clone();
    assert!(line.a.time < line.b.time && line.a.price < line.b.price, "{line:?}");
    assert_eq!(*c.time_scale(), time, "the drag drew, it did not pan");

    // A second click on the first point does not end the line there.
    key(&mut c, Key::Alt('t'));
    click(&mut c, 200.0, 200.0);
    click(&mut c, 201.0, 200.0);
    assert!(drawings(&c).pending.is_some(), "still waiting for the second point");
    click(&mut c, 300.0, 200.0);
    assert_eq!(drawings(&c).items.len(), 4);
}

#[test]
fn the_pointer_shows_what_a_press_would_do_where_it_is() {
    let mut c = chart();
    key(&mut c, Key::Alt('t'));
    drag(&mut c, (100.0, 300.0), (400.0, 200.0));
    key(&mut c, Key::Escape);
    let mut at = |x: f32, y: f32| {
        frame(&mut c);
        c.handle(Event::PointerMoved { x, y });
        c.pointer_shape()
    };
    assert_eq!(at(600.0, 380.0), PointerShape::Crosshair, "an empty plot");
    assert_eq!(at(836.0, 300.0), PointerShape::ResizeVertical, "the price axis");
    assert_eq!(at(400.0, 614.0), PointerShape::ResizeHorizontal, "the time axis");
    assert_eq!(at(250.0, 250.0), PointerShape::Hand, "the trend line");
    assert_eq!(at(400.0, 200.0), PointerShape::Move, "its end");
    at(600.0, 380.0);
    c.handle(Event::PointerDown);
    c.handle(Event::PointerMoved { x: 580.0, y: 380.0 });
    assert_eq!(c.pointer_shape(), PointerShape::Grabbing, "panning");
    c.handle(Event::PointerUp);
    assert_eq!(c.pointer_shape(), PointerShape::Crosshair);
}

#[test]
fn drawings_clicked_or_boxed_with_shift_go_together_and_all_can_go_at_once() {
    let mut c = chart();
    for y in [100.0, 200.0, 300.0] {
        key(&mut c, Key::Alt('h'));
        click(&mut c, 300.0, y);
    }
    key(&mut c, Key::Escape);
    let held = |c: &mut Chart, adding: bool| {
        c.handle(Event::Modifiers { adding });
    };

    // A click picks one; with Shift another joins it; Delete takes both, and one undo brings both back.
    click(&mut c, 500.0, 100.0);
    held(&mut c, true);
    click(&mut c, 500.0, 200.0);
    held(&mut c, false);
    assert_eq!((drawings(&c).selected, drawings(&c).group.clone()), (Some(1), vec![0]));
    let third = drawings(&c).items[2].clone();
    key(&mut c, Key::Delete);
    assert_eq!(drawings(&c).items, vec![third], "the one not selected stays");
    key(&mut c, Key::Undo);
    assert_eq!(drawings(&c).items.len(), 3);

    // A box dragged with Shift over empty chart selects what it covers, and the chart stays where it was.
    key(&mut c, Key::Escape);
    let time = *c.time_scale();
    held(&mut c, true);
    drag(&mut c, (50.0, 150.0), (790.0, 350.0));
    held(&mut c, false);
    assert_eq!(*c.time_scale(), time, "the box did not pan");
    let mut picked: Vec<usize> = drawings(&c)
        .selected
        .into_iter()
        .chain(drawings(&c).group.clone())
        .collect();
    picked.sort();
    assert_eq!(picked, vec![1, 2]);
    key(&mut c, Key::Delete);
    assert_eq!(drawings(&c).items.len(), 1);

    // All of them at once, and back.
    c.handle(Event::Command(tickerlight_core::plugin::commands::DRAW_CLEAR));
    assert!(drawings(&c).items.is_empty());
    key(&mut c, Key::Undo);
    assert_eq!(drawings(&c).items.len(), 1);
}

#[test]
fn a_drag_with_shift_or_ctrl_drags_a_copy_and_the_body_of_one_of_several_selected_drags_them_all() {
    let mut c = chart();
    for y in [100.0, 200.0] {
        key(&mut c, Key::Alt('h'));
        click(&mut c, 300.0, y);
    }
    key(&mut c, Key::Escape);
    let price = |c: &Chart, i: usize| drawings(c).items[i].a.price;
    let (first, second) = (price(&c, 0), price(&c, 1));
    c.handle(Event::Modifiers { adding: true });
    drag(&mut c, (500.0, 100.0), (500.0, 150.0));
    c.handle(Event::Modifiers { adding: false });
    assert_eq!(drawings(&c).items.len(), 3);
    assert_eq!(price(&c, 0), first, "the original stays");
    assert!(
        price(&c, 2) < first && price(&c, 2) > second,
        "the copy went where it was dragged"
    );
    assert_eq!(drawings(&c).selected, Some(2));

    // The copy and the second line selected, then the copy dragged by its body: both go down as much.
    c.handle(Event::Modifiers { adding: true });
    click(&mut c, 500.0, 200.0);
    c.handle(Event::Modifiers { adding: false });
    let (copy, second) = (price(&c, 2), price(&c, 1));
    drag(&mut c, (500.0, 150.0), (500.0, 180.0));
    let moved = price(&c, 2) - copy;
    assert!(moved < 0.0);
    assert!((price(&c, 1) - second - moved).abs() < 1e-9);
    assert_eq!(price(&c, 0), first);
}

#[test]
fn shift_holds_a_line_being_drawn_to_steps_of_45_degrees() {
    let mut c = chart();
    for adding in [false, true] {
        key(&mut c, Key::Alt('t'));
        c.handle(Event::Modifiers { adding });
        drag(&mut c, (100.0, 300.0), (250.0, 310.0));
        c.handle(Event::Modifiers { adding: false });
    }
    let items = &drawings(&c).items;
    assert_ne!(items[0].a.price, items[0].b.price);
    assert!(
        (items[1].a.price - items[1].b.price).abs() < 1e-9,
        "level: {:?}",
        items[1]
    );
    assert!(items[1].b.time > items[1].a.time);
}

#[test]
fn the_arrows_move_the_selected_drawings_and_the_chart_when_none_is() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 200.0);
    let price = drawings(&c).items[0].a.price;
    let time = *c.time_scale();
    key(&mut c, Key::Up);
    assert!(drawings(&c).items[0].a.price > price);
    key(&mut c, Key::Down);
    assert!((drawings(&c).items[0].a.price - price).abs() < 1e-9);
    let at = drawings(&c).items[0].a.time;
    key(&mut c, Key::Right);
    assert!(drawings(&c).items[0].a.time > at);
    assert_eq!(*c.time_scale(), time, "the chart stays");
    key(&mut c, Key::Escape);
    key(&mut c, Key::Right);
    assert_ne!(*c.time_scale(), time);
}

#[test]
fn copied_drawings_are_pasted_beside_where_they_were() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 200.0);
    key(&mut c, Key::Copy);
    key(&mut c, Key::Paste);
    key(&mut c, Key::Paste);
    let d = drawings(&c);
    assert_eq!(d.items.len(), 3);
    assert_eq!(d.selected, Some(2));
    assert!(d.items[2].a.price < d.items[1].a.price && d.items[1].a.price < d.items[0].a.price);
    key(&mut c, Key::Cut);
    assert_eq!(drawings(&c).items.len(), 2);
    key(&mut c, Key::Paste);
    assert_eq!(drawings(&c).items.len(), 3);
}

#[test]
fn a_drawing_can_be_picked_moved_by_its_body_and_reshaped_by_its_end() {
    let mut c = chart();
    key(&mut c, Key::Alt('t'));
    drag(&mut c, (100.0, 300.0), (400.0, 100.0));
    let original = drawings(&c).items[0].clone();
    key(&mut c, Key::Escape); // nothing selected any more
    assert_eq!(drawings(&c).selected, None);

    // Grab the middle of the line and move it: both ends move together.
    let time = *c.time_scale();
    drag(&mut c, (250.0, 200.0), (250.0, 250.0));
    let moved = drawings(&c).items[0].clone();
    assert_eq!(drawings(&c).selected, Some(0));
    assert!(moved.a.price < original.a.price && moved.b.price < original.b.price);
    assert!(((moved.a.price - original.a.price) - (moved.b.price - original.b.price)).abs() < 1e-9);
    assert_eq!(moved.a.time, original.a.time);
    assert_eq!(*c.time_scale(), time, "moving a drawing does not pan");

    // Grab the right end and pull it: only that end moves.
    let end_y = 100.0 + 50.0; // it moved down by 50 px
    drag(&mut c, (400.0, end_y), (500.0, end_y));
    let reshaped = drawings(&c).items[0].clone();
    assert_eq!(reshaped.a, moved.a);
    assert!(reshaped.b.time > moved.b.time);
}

#[test]
fn delete_removes_the_selection_and_escape_drops_the_tool() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 200.0);
    assert_eq!(drawings(&c).items.len(), 2);
    key(&mut c, Key::Delete);
    assert_eq!(drawings(&c).items.len(), 1);
    assert_eq!(drawings(&c).selected, None);
    key(&mut c, Key::Backspace);
    assert_eq!(drawings(&c).items.len(), 1, "nothing selected, nothing deleted");

    key(&mut c, Key::Alt('R'));
    click(&mut c, 100.0, 100.0);
    assert!(drawings(&c).pending.is_some());
    key(&mut c, Key::Escape);
    assert!(drawings(&c).pending.is_none() && drawings(&c).tool.is_none());
    assert_eq!(drawings(&c).items.len(), 1);
}

#[test]
fn a_click_on_empty_chart_deselects_and_a_drag_there_still_pans() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    assert_eq!(drawings(&c).selected, Some(0));
    let time = *c.time_scale();
    drag(&mut c, (300.0, 350.0), (400.0, 350.0));
    assert_eq!(drawings(&c).selected, None);
    assert_ne!(*c.time_scale(), time, "panned");
    assert_eq!(drawings(&c).items.len(), 1);
}

#[test]
fn a_right_press_selects_the_drawing_under_it_and_puts_a_tool_away() {
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 200.0);
    assert_eq!(drawings(&c).selected, Some(1));
    let right_press = |c: &mut Chart, x: f32, y: f32| {
        c.handle(Event::PointerMoved { x, y });
        c.handle(Event::SecondaryDown);
    };
    right_press(&mut c, 400.0, 120.0);
    assert_eq!(drawings(&c).selected, Some(0));
    right_press(&mut c, 400.0, 350.0);
    assert_eq!(drawings(&c).selected, None);
    key(&mut c, Key::Alt('R'));
    click(&mut c, 100.0, 100.0);
    assert!(drawings(&c).pending.is_some());
    right_press(&mut c, 150.0, 150.0);
    let d = drawings(&c);
    assert!(d.pending.is_none() && d.tool.is_none());
    assert_eq!(d.items.len(), 2);
}

#[test]
fn drawings_stay_in_the_pane_they_were_made_in() {
    let mut c = chart();
    let rsi_top = c.pane_layout()[1].top;
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, rsi_top + 40.0);
    assert_eq!(drawings(&c).items[0].pane, "rsi");
    let price = drawings(&c).items[0].a.price;
    assert!((0.0..=100.0).contains(&price), "a price of the RSI scale: {price}");
    let scene = frame(&mut c);
    let lines: Vec<_> = scene
        .lines
        .iter()
        .filter(|l| l.y0 == l.y1 && l.x1 - l.x0 > 700.0 && l.color.a == 255) // not the selection's glow
        .collect();
    assert_eq!(lines.len(), 1);
    assert!(
        (lines[0].y0 - (rsi_top + 40.0)).abs() < 2.0,
        "{} in the RSI pane",
        lines[0].y0
    );
}

#[test]
fn the_ruler_measures_and_goes_away() {
    let mut c = chart();
    frame(&mut c);
    c.handle(Event::Command(tickerlight_core::plugin::commands::DRAW_MEASURE));
    drag(&mut c, (100.0, 300.0), (300.0, 100.0));
    assert!(drawings(&c).items.is_empty(), "the ruler is not kept");
    let m = drawings(&c).measure.clone().expect("measured");
    let scene = frame(&mut c);
    let expected = format!("{:+.2}", m.b.price - m.a.price);
    let price = &scene
        .texts
        .iter()
        .find(|t| t.text.contains('%'))
        .expect("price readout")
        .text;
    assert!(price.starts_with(&expected), "{price} vs {expected}");
    let step = (c.seen().time()[1].0 - c.seen().time()[0].0) as f64;
    let bars = ((m.b.time - m.a.time) / step).round();
    let time = &scene
        .texts
        .iter()
        .find(|t| t.text.contains("bars"))
        .expect("time readout")
        .text;
    assert!(time.starts_with(&format!("{bars} bars")), "{time}");
    click(&mut c, 600.0, 350.0);
    assert!(drawings(&c).measure.is_none());
}

#[test]
fn a_drawing_made_in_a_replay_is_still_there_afterwards() {
    let mut c = chart();
    key(&mut c, Key::Alt('P'));
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    key(&mut c, Key::Alt('P'));
    assert_eq!(drawings(&c).items.len(), 1);
}

#[test]
fn drawings_are_pinned_to_moments_so_they_survive_a_change_of_timeframe() {
    let mut c = chart();
    frame(&mut c);
    key(&mut c, Key::Alt('t'));
    drag(&mut c, (500.0, 300.0), (700.0, 150.0));
    let line = drawings(&c).items[0].clone();
    let orange = tickerlight_core::builtin::DEFAULT_COLOR;
    let drawn = |scene: &Scene| scene.lines.iter().filter(|l| l.color == orange).count();
    assert_eq!(drawn(&frame(&mut c)), 1);

    c.set_timeframe(Some(5 * (c.seen().time()[1].0 - c.seen().time()[0].0)));
    assert_eq!(drawings(&c).items[0], line, "nothing about the drawing changed");
    let scene = frame(&mut c);
    assert_eq!(drawn(&scene), 1, "and it is still drawn, on the same moments");
    let l = scene.lines.iter().find(|l| l.color == orange).unwrap();
    assert!(l.x0 < l.x1 && l.y0 > l.y1, "same direction: {l:?}");

    c.set_timeframe(None);
    assert_eq!(drawn(&frame(&mut c)), 1);
}

#[test]
fn drawings_are_saved_as_text_and_loaded_back() {
    let mut c = chart();
    frame(&mut c);
    key(&mut c, Key::Alt('t'));
    drag(&mut c, (100.0, 300.0), (400.0, 100.0));
    key(&mut c, Key::Alt('R'));
    drag(&mut c, (500.0, 300.0), (600.0, 200.0));
    let rsi_top = c.pane_layout()[1].top;
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, rsi_top + 40.0);
    let saved = drawings(&c).save();
    assert_eq!(saved.lines().count(), 3, "{saved}");

    let mut fresh = chart();
    let panes: Vec<&'static str> = fresh.pane_stack().names().collect();
    let d = fresh.resources_mut().get_mut::<Drawings>().unwrap();
    assert_eq!(d.load(&saved, &panes), 3);
    assert_eq!(d.items, drawings(&c).items);
    // Garbage, unknown panes and the ruler do not get in.
    let junk = "trend main 1 2 3\nhline nowhere 1 2 3 4 ffffff\nmeasure main 1 2 3 4 ffffff\nzone main x 2 3 4 ffffff\nzone main 1 2 3 4 ff8800\n";
    assert_eq!(d.load(junk, &panes), 1);
    assert_eq!(d.items[0].color, tickerlight_core::Color::rgb(255, 136, 0));
}

#[test]
fn changes_are_undone_and_redone_one_gesture_at_a_time() {
    let mut c = chart();
    let prices = |c: &Chart| drawings(c).items.iter().map(|d| d.a.price).collect::<Vec<_>>();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    let placed = prices(&c);
    // Moved by a drag of many events: one step.
    drag(&mut c, (300.0, 120.0), (300.0, 200.0));
    let moved = prices(&c);
    assert_ne!(moved, placed);
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 300.0);
    assert_eq!(drawings(&c).items.len(), 2);

    key(&mut c, Key::Undo);
    assert_eq!(prices(&c), moved);
    key(&mut c, Key::Undo);
    assert_eq!(prices(&c), placed, "the drag is taken back whole");
    key(&mut c, Key::Undo);
    assert!(prices(&c).is_empty());
    key(&mut c, Key::Undo);
    assert!(prices(&c).is_empty(), "nothing more to undo");

    key(&mut c, Key::Redo);
    key(&mut c, Key::Redo);
    assert_eq!(prices(&c), moved);
    // A new change ends what could be redone: the line, where it was dragged to, picked and deleted.
    assert_eq!(drawings(&c).selected, None, "undo leaves nothing selected");
    click(&mut c, 300.0, 200.0);
    key(&mut c, Key::Delete);
    assert!(prices(&c).is_empty());
    key(&mut c, Key::Redo);
    assert!(prices(&c).is_empty());
    key(&mut c, Key::Undo);
    assert_eq!(prices(&c), moved);
}

fn typed(c: &mut Chart, text: &str) {
    for ch in text.chars() {
        key(c, Key::Char(ch));
    }
}

#[test]
fn a_note_is_typed_where_it_is_placed_in_any_script() {
    let mut c = chart();
    frame(&mut c);
    c.handle(Event::Command(tickerlight_core::plugin::commands::DRAW_TEXT));
    click(&mut c, 300.0, 120.0);
    assert_eq!(drawings(&c).editing, Some(0));
    // B and S are shortcuts of orders; here they are letters.
    typed(&mut c, "Bs 100% Привет");
    key(&mut c, Key::Backspace);
    assert_eq!(drawings(&c).tool, None);
    key(&mut c, Key::Enter);
    let d = drawings(&c);
    assert_eq!((d.items.len(), d.editing), (1, None));
    assert_eq!(d.items[0].text, "Bs 100% Приве");
    let scene = frame(&mut c);
    let shown = scene
        .texts
        .iter()
        .find(|t| t.text == "Bs 100% Приве")
        .expect("the note");
    assert!((shown.x - 300.0).abs() < 1.0 && (shown.y - 120.0).abs() < 1.0);

    // Kept as text and read back.
    let saved = drawings(&c).save();
    let mut read = tickerlight_core::builtin::Drawings::default();
    assert_eq!(read.load(&saved, &["main"]), 1);
    assert_eq!(read.items[0].text, "Bs 100% Приве");

    // Its size: drawn so, kept so.
    c.resources_mut().get_mut::<Drawings>().unwrap().items[0].size = 24.0;
    let scene = frame(&mut c);
    let shown = scene.texts.iter().find(|t| t.text == "Bs 100% Приве").unwrap();
    assert_eq!(shown.size, 24.0);
    read.load(&drawings(&c).save(), &["main"]);
    assert_eq!(read.items[0].size, 24.0);

    // Enter on the selected note writes it again; a note left empty goes.
    click(&mut c, 310.0, 120.0);
    assert_eq!(drawings(&c).selected, Some(0));
    key(&mut c, Key::Enter);
    assert_eq!(drawings(&c).editing, Some(0));
    for _ in 0.."Bs 100% Приве".chars().count() {
        key(&mut c, Key::Backspace);
    }
    key(&mut c, Key::Escape);
    assert!(drawings(&c).items.is_empty());
    // Typed and taken back as one step: the note comes back whole.
    key(&mut c, Key::Undo);
    assert_eq!(drawings(&c).items[0].text, "Bs 100% Приве");
    // A double click on it writes it again too.
    click(&mut c, 310.0, 120.0);
    c.handle(Event::DoubleClick);
    assert_eq!(drawings(&c).editing, Some(0));
}

#[test]
fn a_locked_drawing_is_picked_but_not_moved_and_a_clone_is_free() {
    use tickerlight_core::plugin::commands;
    let mut c = chart();
    key(&mut c, Key::Alt('h'));
    click(&mut c, 300.0, 120.0);
    let price = drawings(&c).items[0].a.price;
    c.handle(Event::Command(commands::DRAW_LOCK));
    assert!(drawings(&c).items[0].locked);
    let time = *c.time_scale();
    drag(&mut c, (300.0, 120.0), (300.0, 250.0));
    assert_eq!(drawings(&c).items[0].a.price, price, "it stays");
    assert_eq!(drawings(&c).selected, Some(0), "picked all the same");
    assert_eq!(*c.time_scale(), time, "the drag did not pan the chart either");

    // Kept locked in the saved text.
    let mut read = Drawings::default();
    read.load(&drawings(&c).save(), &["main"]);
    assert!(read.items[0].locked);

    // The clone lands a little lower, unlocked and selected, and moves.
    c.handle(Event::Command(commands::DRAW_CLONE));
    let d = drawings(&c);
    assert_eq!((d.items.len(), d.selected), (2, Some(1)));
    let copy = d.items[1].clone();
    assert!(!copy.locked && copy.a.price < price);
    let y = 120.0 + 24.0;
    drag(&mut c, (300.0, y), (300.0, 300.0));
    assert!(drawings(&c).items[1].a.price < copy.a.price, "the clone moved");
    assert_eq!(drawings(&c).items[0].a.price, price);
}

#[test]
fn a_position_shows_how_it_went_over_the_bars_seen() {
    use tickerlight_core::builtin::{Anchor, Drawing, DrawingStyle, Outcome, position_outcome};
    use tickerlight_core::{Candle, Candles, Time};
    // Bars a minute apart: 100, then up 1 a bar to 110 at bar 10, with highs and lows 0.5 away.
    let mut candles = Candles::new();
    for i in 0..20 {
        let close = 100.0 + (i.min(10)) as f64;
        candles
            .push(Candle {
                time: Time(i * 60_000),
                open: close,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1.0,
            })
            .unwrap();
    }
    let mut c = Chart::new(candles);
    c.resize(872.0, 528.0);
    let at = |minute: f64, price: f64| Anchor {
        time: minute * 60_000.0,
        price,
    };
    // Long from 101 at minute 1 to minute 15: target 105, stop 99.
    let mut long = Drawing::new(Tool::Long, "main", at(1.0, 101.0), DrawingStyle::default());
    (long.b, long.c) = (at(15.0, 105.0), at(1.0, 99.0));
    let outcome = |c: &Chart, d: &Drawing| position_outcome(d, &c.seen());
    assert_eq!(
        outcome(&c, &long),
        Some((Outcome::Target, 5.0 * 60_000.0, 105.0)),
        "bar 5 is the first high at 105"
    );
    // A short from the same place is stopped there, at 105 (stop) against a target at 97.
    let mut short = Drawing::new(Tool::Short, "main", at(1.0, 101.0), DrawingStyle::default());
    (short.b, short.c) = (at(15.0, 97.0), at(1.0, 105.0));
    assert_eq!(outcome(&c, &short).unwrap().0, Outcome::Stop);
    // In a replay that has shown only up to minute 2 the long is still open, at the last close.
    c.set_cursor(Some(Time(2 * 60_000)));
    assert_eq!(outcome(&c, &long), Some((Outcome::Open, 2.0 * 60_000.0, 102.0)));

    // On the chart: the label says so.
    c.set_cursor(None);
    c.resources_mut().get_mut::<Drawings>().unwrap().items.push(long);
    let scene = frame(&mut c);
    assert!(
        scene.texts.iter().any(|t| t.text == "Target hit +3.96%"),
        "{:?}",
        scene.texts.iter().map(|t| &t.text).collect::<Vec<_>>()
    );
}
