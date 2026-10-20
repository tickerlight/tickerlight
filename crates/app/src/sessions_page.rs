//! The sessions page of the side column: the session going on, the results of all of them together, by tag and by
//! journal column, the journal columns, and the saved sessions to open.

use std::collections::BTreeMap;

use tickerlight_core::builtin::{Account, Stats};
use tickerlight_core::{Align, Chart};
use tickerlight_ui::panels::{
    Journal, JournalColumns, WEEKDAYS, count_trades, hour_stats, money, pick_stats, short_time, tag_stats,
    weekday_stats,
};
use tickerlight_ui::{Cell, Column, Panel, Ui};

use crate::program::Program;
use crate::session::Session;

/// What the side column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// The replay, the order ticket, the position and the trades.
    #[default]
    Trade,
    Sessions,
    Indicators,
    Watchlist,
    Strategy,
}

/// A panel shown only on one page.
pub struct OnPage<P>(pub Page, pub P);

impl<P: Panel<Program>> Panel<Program> for OnPage<P> {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, p: &mut Program) {
        if p.page == Some(self.0) {
            self.1.ui(ui, chart, p);
        }
    }
}

/// How many tags the page lists, the busiest first.
const MAX_TAGS: usize = 6;

const SESSION_COLUMNS: [Column; 4] = [
    Column {
        title: "Started",
        width: 2.4,
        align: Align::Left,
    },
    Column {
        title: "Symbol",
        width: 2.0,
        align: Align::Left,
    },
    Column {
        title: "Trades",
        width: 1.1,
        align: Align::Right,
    },
    Column {
        title: "Result",
        width: 1.6,
        align: Align::Right,
    },
];

/// The closed trades of an account and its result after fees (the open position aside).
fn result(account: &Account) -> (Stats, f64) {
    (account.stats(), account.realized() - account.fees())
}

fn saved_result(s: &Session) -> (Stats, f64) {
    result(&s.account())
}

pub fn sessions_panel(ui: &mut Ui, chart: &mut Chart, p: &mut Program) {
    let (up, down) = (ui.theme().up, ui.theme().down);
    let color = |v: f64| if v >= 0.0 { up } else { down };
    let res = chart.resources();
    let account = res.get::<Account>().cloned().unwrap_or_default();
    let journal = res.get::<Journal>().cloned().unwrap_or_default();
    let (stats, net) = result(&account);

    ui.section("This session");
    let name = p
        .shown
        .as_ref()
        .map_or(String::new(), |s| format!("{} {}", s.symbol, s.venue.label()));
    ui.value(&name, &count_trades(stats.closed), None);
    ui.columns(2, |ui| {
        ui.value("Result", &money(net), Some(color(net)));
        let share = if account.balance > 0.0 {
            format!("{:+.2}%", net * 100.0 / account.balance)
        } else {
            "-".into()
        };
        ui.value("Return", &share, Some(color(net)));
    });
    ui.columns(2, |ui| {
        if ui.button("New session") {
            p.new_session = true;
        }
        if ui.button("Export CSV") {
            p.export = true;
        }
    });
    if let Some(dir) = &p.snapshots {
        let muted = ui.theme().muted;
        ui.label_colored(&format!("Pictures of its trades in {}", dir.display()), muted);
    }
    if let Some((lines, failed)) = &p.exported {
        let color = if *failed { down } else { ui.theme().muted };
        for line in lines {
            ui.label_colored(line, color);
        }
    }

    // Every session: the saved ones, with this one as it is now rather than as last saved.
    let others = p.sessions.iter().filter(|s| Some(s.created) != p.session);
    let mut all: Vec<(Stats, f64)> = others.clone().map(saved_result).collect();
    let mut by_tag: BTreeMap<String, Stats> = BTreeMap::new();
    let mut by_pick: BTreeMap<String, Stats> = BTreeMap::new();
    let mut by_hour: BTreeMap<u32, Stats> = BTreeMap::new();
    let mut by_weekday: BTreeMap<u32, Stats> = BTreeMap::new();
    for s in others {
        add_up(&mut by_tag, tag_stats(&s.fills, &s.journal));
        add_up(&mut by_pick, pick_stats(&s.fills, &s.journal));
        add_up(&mut by_hour, hour_stats(&s.fills, p.zone));
        add_up(&mut by_weekday, weekday_stats(&s.fills, p.zone));
    }
    if p.session.is_some() || !account.fills().is_empty() {
        all.push((stats, net));
        add_up(&mut by_tag, tag_stats(account.fills(), &journal));
        add_up(&mut by_pick, pick_stats(account.fills(), &journal));
        add_up(&mut by_hour, hour_stats(account.fills(), p.zone));
        add_up(&mut by_weekday, weekday_stats(account.fills(), p.zone));
    }
    let mut sum = Stats::default();
    for (s, _) in &all {
        sum.merge(s);
    }
    let total: f64 = all.iter().map(|(_, n)| n).sum();
    let or_dash = |v: Option<f64>| v.map_or("-".into(), money);
    ui.section("All sessions");
    ui.columns(2, |ui| {
        ui.value("Sessions", &all.len().to_string(), None);
        ui.value("Trades", &sum.closed.to_string(), None);
    });
    ui.columns(2, |ui| {
        let rate = sum.win_rate().map_or("-".into(), |w| format!("{:.0}%", w * 100.0));
        ui.value("Wins", &rate, None);
        ui.value("Result", &money(total), Some(color(total)));
    });
    ui.columns(2, |ui| {
        ui.value("Average win", &or_dash(sum.average_win()), Some(up));
        ui.value("Average loss", &or_dash(sum.average_loss().map(|l| -l)), Some(down));
    });
    let expectancy = sum.expectancy();
    ui.value("Per trade", &or_dash(expectancy), expectancy.map(color));
    ui.tip("What a trade made on average after fees: what the next is worth if they go on as they went");
    // A row of its own: the runs need more room than half a narrow column.
    let streaks = format!("{} won, {} lost", sum.win_streak, sum.loss_streak);
    ui.value("In a row", &streaks, None);
    ui.tip("The longest runs of wins and of losses in a session");

    ui.section("By hour");
    if by_hour.is_empty() {
        ui.label("Trades by the hour they went in at");
    }
    let hours: Vec<(String, Stats)> = by_hour.into_iter().map(|(h, s)| (format!("{h:02}:00"), s)).collect();
    stats_rows(ui, &hours);
    ui.section("By weekday");
    if by_weekday.is_empty() {
        ui.label("Trades by the day they went in on");
    }
    let days: Vec<(String, Stats)> = by_weekday
        .into_iter()
        .map(|(d, s)| (WEEKDAYS[d as usize].to_string(), s))
        .collect();
    stats_rows(ui, &days);

    ui.section("By tag");
    let mut tags: Vec<(String, Stats)> = by_tag.into_iter().collect();
    tags.sort_by_key(|(_, s)| std::cmp::Reverse(s.closed));
    if tags.is_empty() {
        ui.label("Tag trades in the trades table");
    }
    stats_rows(ui, &tags[..tags.len().min(MAX_TAGS)]);

    // Choices of one's own for every trade, like a grade or a mistake: picked in the trades table.
    ui.section("Journal columns");
    let field = ui.text_field(&mut p.columns_text, 0.0, "Grade: A, B, C; Mistake: early, late");
    ui.tip("A name, a colon and its choices; columns apart with semicolons. The same in every session");
    if field.changed {
        p.columns = JournalColumns::parse(&p.columns_text);
    }
    // A column at a time, its choices in their order.
    let picks: Vec<(String, Stats)> = p
        .columns
        .0
        .iter()
        .flat_map(|c| c.choices.iter().map(move |choice| format!("{}: {choice}", c.name)))
        .filter_map(|key| by_pick.get(&key).map(|s| (key, *s)))
        .collect();
    stats_rows(ui, &picks);

    ui.section("Saved sessions");
    let sessions = &p.sessions;
    let current = sessions.iter().position(|s| Some(s.created) == p.session);
    let clicked = ui.table(&SESSION_COLUMNS, sessions.len(), current, |i| {
        let s = &sessions[i];
        let (stats, net) = saved_result(s);
        vec![
            Cell::new(short_time(s.created, 60_000, p.zone)),
            // Paper trading on live data says so; the rest are replays.
            Cell::new(if s.live {
                format!("{} live", s.series.symbol)
            } else {
                s.series.symbol.clone()
            }),
            Cell::new(stats.closed.to_string()),
            Cell::colored(money(net), color(net)),
        ]
    });
    if let Some(i) = clicked {
        p.open = Some(sessions[i].created);
    }
}

/// Adds the stats of each key to those of the same key.
fn add_up<K: Ord>(sums: &mut BTreeMap<K, Stats>, more: Vec<(K, Stats)>) {
    for (key, s) in more {
        sums.entry(key).or_default().merge(&s);
    }
}

/// A row each: the trades, the share won and the result.
fn stats_rows(ui: &mut Ui, rows: &[(String, Stats)]) {
    let (up, down) = (ui.theme().up, ui.theme().down);
    for (key, s) in rows {
        let r = s.gross_profit - s.gross_loss;
        let rate = s.win_rate().map_or("-".into(), |w| format!("{:.0}%", w * 100.0));
        ui.value(
            key,
            &format!("{}, {rate}, {}", count_trades(s.closed), money(r)),
            Some(if r >= 0.0 { up } else { down }),
        );
    }
}
