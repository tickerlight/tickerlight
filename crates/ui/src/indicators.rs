//! The indicators page: the ones on the chart with their settings, and the list to add more from.

use tickerlight_core::Chart;
use tickerlight_core::builtin::{Dash, Kind, STUDY_COLORS, STUDY_WIDTHS, Studies, set_studies, studies};

use crate::{Icon, Panel, Ui};

/// What the field the indicators to add are looked for in is found by.
pub const SEARCH_INDICATORS: &str = "Search indicators";
/// What the button that closes the list to add from is found by.
pub const CLOSE_LIST: &str = "Done";

#[derive(Default)]
pub struct IndicatorsPanel {
    search: String,
    /// The list to add from shows: the search field was clicked or typed in, and nothing was added since.
    open: bool,
    /// What the setting fields show, by indicator and setting: typed text until it is taken.
    fields: Vec<Vec<String>>,
    /// The field that had the keys at the last pass.
    editing: Option<(usize, usize)>,
}

/// Whether `kind` is what `search` looks for: in its names, in any case and any script.
fn matches(kind: Kind, search: &str) -> bool {
    let search = search.trim().to_lowercase();
    let names = format!("{} {}", kind.title(), kind.short()).to_lowercase();
    search.is_empty() || names.contains(&search)
}

impl<C> Panel<C> for IndicatorsPanel {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, _: &mut C) {
        let mut items = studies(chart).to_vec();
        let before = items.clone();
        ui.section("Add");
        let field = ui.text_field(&mut self.search, 0.0, SEARCH_INDICATORS);
        ui.tip("Click or press / to pick an indicator to add; Enter adds the first found");
        self.open = (self.open || field.gained || field.changed) && !field.cancelled;
        // Esc puts the list away with what was typed for it.
        if field.cancelled {
            self.search.clear();
        }
        // The list to pick from takes the page until one is picked.
        if self.open {
            let mut picked = false;
            if field.submitted
                && let Some(kind) = Kind::ALL.into_iter().find(|&k| matches(k, &self.search))
            {
                items.push(Studies::new_study(&items, kind));
                picked = true;
            }
            let mut shown = 0;
            for kind in Kind::ALL.into_iter().filter(|&k| matches(k, &self.search)) {
                shown += 1;
                if ui.button(kind.title()) {
                    items.push(Studies::new_study(&items, kind));
                    picked = true;
                }
                ui.tip(if kind.own_pane() {
                    "Adds it in a pane of its own"
                } else {
                    "Adds it over the candles"
                });
            }
            if picked {
                (self.open, self.search) = (false, String::new());
            }
            if shown == 0 {
                ui.heading("Nothing by that name");
            }
            if ui.button(CLOSE_LIST) {
                (self.open, self.search) = (false, String::new());
            }
            ui.tip("Back to the indicators on the chart. Esc in the search");
            if items != before {
                set_studies(chart, items);
            }
            return;
        }

        self.fields.resize_with(items.len(), Default::default);
        ui.section("On the chart");
        if items.is_empty() {
            ui.heading("None yet: add one above");
        }
        let mut remove = None;
        let mut editing = None;
        for (i, study) in items.iter_mut().enumerate() {
            let label = study.label();
            ui.horizontal(|ui| {
                if study.kind.colored() {
                    let name = format!("Color of {label}");
                    if ui.swatch(study.color, &name, false) {
                        let at = STUDY_COLORS.iter().position(|&c| c == study.color);
                        study.color = STUDY_COLORS[at.map_or(0, |a| (a + 1) % STUDY_COLORS.len())];
                    }
                    ui.tip("Click for the next color");
                }
                ui.label(study.kind.title());
                let (eye, name) = if study.hidden {
                    (Icon::EyeOff, format!("Show {label}"))
                } else {
                    (Icon::Eye, format!("Hide {label}"))
                };
                if ui.icon_button(eye, &name, study.hidden) {
                    study.hidden = !study.hidden;
                }
                if ui.icon_button(Icon::Trash, &format!("Remove {label}"), false) {
                    remove = Some(i);
                }
            });
            let settings = study.settings();
            self.fields[i].resize_with(settings.len(), String::new);
            for (j, param) in settings.iter().enumerate() {
                // A choice of a few words: two side by side with its name, a few more on a row of their own under
                // it, many in a dropdown beside its name.
                if param.is_choice() {
                    let mut at = study.setting(j) as usize;
                    let mut changed = false;
                    if (3..=4).contains(&param.choices.len()) {
                        ui.heading(param.name);
                        changed = ui.choice(&mut at, param.choices);
                    } else {
                        ui.columns(2, |ui| {
                            ui.heading(param.name);
                            changed = if param.choices.len() > 4 {
                                ui.dropdown(&mut at, param.choices)
                            } else {
                                ui.choice(&mut at, param.choices)
                            };
                        });
                    }
                    if changed {
                        study.set(j, at as f64);
                    }
                    continue;
                }
                let field = &mut self.fields[i][j];
                if self.editing != Some((i, j)) {
                    *field = format!("{}", study.setting(j));
                }
                ui.columns(2, |ui| {
                    ui.heading(param.name);
                    let response = ui.text_field(field, 0.0, param.name);
                    if !param.tip.is_empty() {
                        ui.tip(param.tip);
                    }
                    if response.focused {
                        editing = Some((i, j));
                    }
                    // Taken when Enter is pressed or the field is left, so a length typed digit by digit does not
                    // make an indicator of each.
                    let left = self.editing == Some((i, j)) && !response.focused;
                    if (response.submitted || left)
                        && let Ok(value) = field.trim().replace(',', ".").parse::<f64>()
                    {
                        study.set(j, value);
                    }
                });
            }
            if !study.kind.lined() {
                continue;
            }
            // Two rows: the widths and the dashes side by side do not fit a narrow dock.
            ui.horizontal(|ui| {
                ui.heading("Width");
                let names: Vec<String> = STUDY_WIDTHS.iter().map(|w| format!("{label} width {w}")).collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let icons = [Icon::Width(1), Icon::Width(2), Icon::Width(3), Icon::Width(4)];
                let mut at = STUDY_WIDTHS.iter().position(|&w| w == study.width).unwrap_or(1);
                if ui.icon_choice(&mut at, &icons, &names) {
                    study.width = STUDY_WIDTHS[at];
                }
            });
            ui.horizontal(|ui| {
                ui.heading("Style");
                let dashes = [Dash::Solid, Dash::Dashed, Dash::Dotted];
                let names = ["solid", "dashed", "dotted"].map(|d| format!("{label} {d}"));
                let names = names.each_ref().map(String::as_str);
                let mut at = dashes.iter().position(|&d| d == study.dash).unwrap_or(0);
                if ui.icon_choice(&mut at, &[Icon::Solid, Icon::Dashed, Icon::Dotted], &names) {
                    study.dash = dashes[at];
                }
            });
        }
        self.editing = editing;
        if let Some(i) = remove {
            items.remove(i);
            self.fields.remove(i);
        }

        if items != before {
            set_studies(chart, items);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dock, Workspace};
    use tickerlight_core::sample::random_walk;
    use tickerlight_core::{Event, Key, Scene, Surface};

    fn window() -> Workspace<()> {
        let mut w = Workspace::new(Chart::new(random_walk(300)), ()).with(Dock::Right, IndicatorsPanel::default());
        w.resize(1400.0, 1000.0);
        w.build_scene(&mut Scene::new());
        w
    }

    /// The last control with this text.
    fn find(w: &Workspace<()>, text: &str) -> (f32, f32) {
        let rects = w.ui_state().rects();
        let (_, area) = rects
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("no {text}: {rects:?}"));
        area.center()
    }

    fn click(w: &mut Workspace<()>, text: &str) {
        let (x, y) = find(w, text);
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
    }

    /// Opens the list to add from and picks `title` in it.
    fn add(w: &mut Workspace<()>, title: &str) {
        click(w, SEARCH_INDICATORS);
        click(w, title);
    }

    /// Types into the field recorded right after `name`: the field itself, or the field beside a heading.
    fn type_into(w: &mut Workspace<()>, name: &str, text: &str, end: Key) {
        let rects = w.ui_state().rects();
        let i = rects
            .iter()
            .position(|(t, _)| t == name)
            .unwrap_or_else(|| panic!("no {name}"));
        let at = if name.starts_with("Search") { i } else { i + 1 };
        let (x, y) = rects[at].1.center();
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        w.build_scene(&mut Scene::new());
        w.handle(Event::Key(Key::SelectAll));
        w.handle(Event::Key(Key::Delete));
        for c in text.chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(end));
        w.build_scene(&mut Scene::new());
    }

    fn labels(w: &Workspace<()>) -> Vec<String> {
        studies(w.chart()).iter().map(|s| s.label()).collect()
    }

    #[test]
    fn indicators_are_added_set_and_removed() {
        let mut w = window();
        // The list shows from a click in the search, in place of the indicators on the chart, until a pick.
        assert!(w.ui_state().rect_of("MACD").is_none());
        add(&mut w, "Relative strength index");
        assert!(w.ui_state().rect_of("MACD").is_none(), "closed by the pick");
        add(&mut w, "Moving average");
        add(&mut w, "Moving average");
        assert_eq!(labels(&w), ["RSI 14", "SMA 20", "SMA 20"]);
        let colors: Vec<_> = studies(w.chart()).iter().map(|s| s.color).collect();
        assert_ne!(colors[1], colors[2], "two averages in two colors");
        assert!(w.chart().pane_names().contains(&"rsi-14"));

        // Typed digit by digit, the length is taken at Enter.
        type_into(&mut w, "Length", "21", Key::Enter);
        assert_eq!(labels(&w), ["RSI 21", "SMA 20", "SMA 20"]);
        assert!(w.chart().pane_names().contains(&"rsi-21"));
        // Nonsense leaves it as it was.
        type_into(&mut w, "Length", "abc", Key::Enter);
        assert_eq!(labels(&w)[0], "RSI 21");

        click(&mut w, "RSI 21 width 3");
        assert_eq!(studies(w.chart())[0].width, 3.0);
        type_into(&mut w, "Average", "9", Key::Enter);
        assert_eq!(studies(w.chart())[0].average, 9.0);
        assert_eq!(labels(&w)[0], "RSI 21", "the average is not in the name");
        click(&mut w, "RSI 21 dotted");
        assert_eq!(studies(w.chart())[0].dash, Dash::Dotted);
        click(&mut w, "Hide RSI 21");
        assert!(studies(w.chart())[0].hidden);
        click(&mut w, "Show RSI 21");
        assert!(!studies(w.chart())[0].hidden);

        click(&mut w, "Remove RSI 21");
        assert_eq!(labels(&w), ["SMA 20", "SMA 20"]);
        assert!(!w.chart().pane_names().iter().any(|n| n.starts_with("rsi")));
    }

    #[test]
    fn a_choice_is_picked_from_its_words_and_a_sessions_indicator_has_no_line_options() {
        let mut w = window();
        add(&mut w, "Trading sessions");
        assert_eq!(labels(&w), ["Sessions"]);
        let rects = w.ui_state().rects();
        assert!(!rects.iter().any(|(t, _)| t == "Width" || t.starts_with("Color of")));
        click(&mut w, "Gradient");
        assert_eq!(studies(w.chart())[0].params[0], 2.0);
        click(&mut w, "Remove Sessions");
        assert!(labels(&w).is_empty());
    }

    #[test]
    fn a_session_of_one_s_own_has_a_color_a_city_and_hours() {
        let mut w = window();
        add(&mut w, "Session of your own");
        add(&mut w, "Session of your own");
        assert_eq!(labels(&w), ["Session New York 09:30-16:00"; 2]);
        let colors: Vec<_> = studies(w.chart()).iter().map(|s| s.color).collect();
        assert_ne!(colors[0], colors[1], "two sessions in two colors");
        let rects = w.ui_state().rects();
        assert!(rects.iter().any(|(t, _)| t == "Color of Session New York 09:30-16:00"));
        assert!(!rects.iter().any(|(t, _)| t == "Width"));
        // The city from its dropdown: the second session's, then Tokyo in the list it opens.
        click(&mut w, "New York");
        click(&mut w, "Tokyo");
        type_into(&mut w, "Opens", "9", Key::Enter);
        assert_eq!(
            labels(&w),
            ["Session New York 09:00-16:00", "Session Tokyo 09:30-16:00"]
        );
    }

    #[test]
    fn the_search_narrows_the_list_enter_adds_the_first_found_and_escape_closes_it() {
        let mut w = window();
        type_into(&mut w, "Search indicators", "aver", Key::Home);
        let has = |w: &Workspace<()>, t: &str| w.ui_state().rects().iter().any(|(r, _)| r == t);
        assert!(has(&w, "Moving average") && has(&w, "Average true range") && has(&w, "Exponential moving average"));
        assert!(!has(&w, "Relative strength index") && !has(&w, "MACD"));
        type_into(&mut w, "Search indicators", "zzz", Key::Enter);
        assert!(
            has(&w, "Nothing by that name"),
            "Enter with nothing found keeps the list"
        );
        type_into(&mut w, "Search indicators", "zzz", Key::Escape);
        assert!(!has(&w, "Nothing by that name") && has(&w, "None yet: add one above"));
        type_into(&mut w, "Search indicators", "relative", Key::Enter);
        assert_eq!(labels(&w), ["RSI 14"]);
        assert!(!has(&w, "MACD"), "closed by the pick");
    }
}
