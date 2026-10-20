use super::drawings::{Drawings, Tool};
use crate::calendar::{Clock, Zone};
use crate::plugin::{commands, names, order};
use crate::{
    Align, Chart, Color, DrawCtx, Event, EventCtx, Handler, Key, Label, Layer, Panes, Plugin, Resources, Response,
    Time, ToolButton,
};

/// Where on the screen a replay started without a pointer begins: this fraction of the visible bars.
const START_FRACTION: f64 = 0.7;
/// The speeds a replay plays at, as multiples of real time: from real time to four weeks a second.
pub const SPEEDS: [f64; 23] = [
    1.0,
    2.0,
    5.0,
    10.0,
    20.0,
    30.0,
    60.0,
    120.0,
    300.0,
    600.0,
    900.0,
    1800.0,
    3600.0,
    7200.0,
    14_400.0,
    21_600.0,
    43_200.0,
    86_400.0,
    172_800.0,
    345_600.0,
    604_800.0,
    1_209_600.0,
    2_419_200.0,
];
/// Bars a second a replay starting on another timeframe plays at: its speed is the nearest of [`SPEEDS`].
const BARS_PER_SECOND: f64 = 4.0;

/// Where playing pauses by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayStops {
    /// When the price reaches a horizontal line drawn on the main pane.
    pub lines: bool,
    /// When the clock of the chart's zone reads this many minutes after midnight.
    pub clock: Option<u32>,
}

/// State of the bar replay. `R` starts it at the bar under the pointer (the Replay button, 70% across the
/// visible bars; [`commands::REPLAY_PICK`] at the bar clicked next), and everything after that bar disappears: the chart, its scales and every plugin see
/// only what has "happened" so far. `Space` plays and pauses: time goes on at the speed, and the newest bar
/// grows trade by trade where the tape is loaded. `Right` reveals one bar, `Up` and `Down` change the speed, `R`
/// again goes back to the live chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub active: bool,
    /// The next click on a plot chooses where the replay starts.
    pub picking: bool,
    pub playing: bool,
    /// How fast time goes while playing, as a multiple of real time (one of [`SPEEDS`]).
    pub speed: f64,
    /// The period of the bars shown when the speed was chosen: a replay starting on another one gets a speed
    /// that suits it.
    pub speed_for: Option<i64>,
    /// Bars shown and bars that exist, kept up to date by the handler for readouts.
    pub shown: usize,
    pub total: usize,
    /// Where the next start of the replay begins, instead of the screen: the bars that opened before this
    /// moment are shown, and the chart scrolls to them. Taken by the start.
    pub start_at: Option<Time>,
    /// The last bar shown when this replay started: what came after it up to the cursor has been played.
    pub from: Option<Time>,
    pub stops: ReplayStops,
    /// The dates are hidden until the replay ends or reaches the last bar (a start at random, not to know when it
    /// is); leaving the replay shows them again.
    pub blind: bool,
    /// Milliseconds of market time not yet played.
    carry: f64,
}

impl Default for Replay {
    fn default() -> Self {
        Self {
            active: false,
            picking: false,
            playing: false,
            speed: 300.0,
            speed_for: None,
            shown: 0,
            total: 0,
            start_at: None,
            from: None,
            stops: ReplayStops::default(),
            blind: false,
            carry: 0.0,
        }
    }
}

/// The speed of [`SPEEDS`] nearest to `speed`.
fn nearest(speed: f64) -> f64 {
    let off = |s: f64| (s / speed).ln().abs();
    SPEEDS
        .into_iter()
        .fold(SPEEDS[0], |best, s| if off(s) < off(best) { s } else { best })
}

fn active(res: &Resources) -> bool {
    res.get::<Replay>().is_some_and(|r| r.active)
}

/// The clock the chart's times are written on: its zone, without dates while a blind replay goes on.
pub fn clock(res: &Resources) -> Clock {
    Clock {
        zone: res.get::<Zone>().copied().unwrap_or_default(),
        blind: res
            .get::<Replay>()
            .is_some_and(|r| r.active && r.blind && r.shown < r.total),
    }
}

pub struct ReplayHandler;

impl Handler for ReplayHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        // Without a pointer the replay starts a bit before the right edge of the screen, so there is room
        // to watch new bars appear (at the very edge nothing would be hidden while looking at the latest data).
        let visible = ctx.time.visible(ctx.len);
        let edge = visible.start + ((visible.end - visible.start) as f64 * START_FRACTION) as usize;
        // Where a replay started from the keyboard begins: under the pointer if it is over a bar.
        let start = ctx
            .pointer
            .position
            .filter(|&p| ctx.over_plot(p))
            .map(|(x, _)| ctx.time.index_of(x as f64).floor() + 1.0)
            .filter(|&n| n >= 1.0 && n <= ctx.len as f64)
            .map_or(edge, |n| n as usize);
        // The period of the bars shown: what the speed of a replay starting suits.
        let period = ctx.timeframe.get().or_else(|| ctx.timeframe.native());
        let zone = ctx.res.get::<Zone>().copied().unwrap_or_default();
        // The prices playing stops at: horizontal lines of the main pane.
        let lines: Vec<f64> = match (event, ctx.res.get::<Replay>(), ctx.res.get::<Drawings>()) {
            (Event::Tick { .. }, Some(replay), Some(drawings)) if replay.stops.lines => drawings
                .items
                .iter()
                .filter(|d| d.pane == names::MAIN_PANE && matches!(d.tool, Tool::HLine | Tool::HRay))
                .map(|d| d.a.price)
                .collect(),
            _ => Vec::new(),
        };
        let Some(state) = ctx.res.get_mut::<Replay>() else {
            return Response::IGNORED;
        };
        let (mut changed, mut left) = (false, false);
        match *event {
            Event::Command(name @ (commands::REPLAY_TOGGLE | commands::REPLAY_TOGGLE_AT_POINTER)) => {
                if state.active {
                    ctx.cursor.finish();
                    state.active = false;
                    state.playing = false;
                    state.from = None;
                    state.blind = false;
                    // Back to the present, as out of a replay in TradingView: the newest bars, not the replay's.
                    left = true;
                } else if ctx.len > 0 {
                    // The bar of the chart the replay starts on is complete; the finer bars after it are hidden.
                    let bar = if let Some(at) = state.start_at.take() {
                        let bar = ctx.view.time().partition_point(|&open| open < at).clamp(1, ctx.len);
                        // The last bar shown sits where a start from the screen would put it.
                        ctx.time.right = bar as f64 + (1.0 - START_FRACTION) * ctx.time.width / ctx.time.bar_spacing;
                        bar
                    } else if name == commands::REPLAY_TOGGLE_AT_POINTER {
                        start
                    } else {
                        edge
                    };
                    let from = ctx.view.time()[bar.clamp(1, ctx.len) - 1];
                    ctx.cursor.start_at_time(from);
                    state.from = Some(from);
                    state.active = true;
                    state.picking = false;
                    state.playing = false;
                    state.carry = 0.0;
                    if let Some(period) = period
                        && state.speed_for != Some(period)
                    {
                        state.speed = nearest(BARS_PER_SECOND * period as f64 / 1000.0);
                        state.speed_for = Some(period);
                    }
                }
                changed = true;
            }
            Event::Command(commands::REPLAY_PLAY_PAUSE) if state.active => {
                state.playing = !state.playing;
                changed = true;
            }
            Event::Command(commands::REPLAY_STEP) if state.active => {
                state.playing = false;
                changed = ctx.cursor.advance(1) > 0;
            }
            Event::Command(command @ (commands::REPLAY_FASTER | commands::REPLAY_SLOWER)) if state.active => {
                let speed = nearest(state.speed);
                state.speed = if command == commands::REPLAY_FASTER {
                    SPEEDS.into_iter().find(|&s| s > speed).unwrap_or(speed)
                } else {
                    SPEEDS.into_iter().rev().find(|&s| s < speed).unwrap_or(speed)
                };
                state.speed_for = period;
                changed = true;
            }
            Event::Tick { dt } if state.active && state.playing => {
                state.carry += f64::from(dt) * 1000.0 * state.speed;
                let ms = state.carry.floor();
                state.carry -= ms;
                let clock = state.stops.clock.map(|minutes| (minutes, zone));
                let (moved, stopped) = ctx.cursor.play_until(ms as i64, &lines, clock);
                changed = moved;
                if stopped {
                    state.playing = false;
                    state.carry = 0.0;
                }
            }
            Event::Command(commands::REPLAY_TO_CLOCK) if state.active => {
                if let Some(minutes) = state.stops.clock {
                    changed = ctx.cursor.play_until(i64::MAX, &[], Some((minutes, zone))).0;
                }
            }
            _ => {}
        }
        state.shown = ctx.cursor.shown();
        state.total = ctx.cursor.total();
        if state.active && state.shown >= state.total && !ctx.cursor.forming() {
            state.playing = false; // the end of the data
        }
        let response = Response {
            redraw: changed,
            animate: state.active && state.playing,
            ..Response::IGNORED
        };
        if left {
            ctx.emit(commands::VIEW_END);
        }
        response
    }
}

/// Choosing where the replay starts: [`commands::REPLAY_PICK`], then a click on a plot starts it at the bar
/// clicked; Escape or the command again stops choosing.
pub struct ReplayPickHandler;

impl Handler for ReplayPickHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let over_bar = ctx
            .pointer
            .position
            .filter(|&p| ctx.over_plot(p))
            .is_some_and(|(x, _)| {
                let bar = ctx.time.index_of(x as f64).floor();
                bar >= 0.0 && (bar as usize) < ctx.len
            });
        let Some(state) = ctx.res.get_mut::<Replay>() else {
            return Response::IGNORED;
        };
        match *event {
            Event::Command(commands::REPLAY_PICK) => {
                state.picking = !state.picking && !state.active;
                Response::CONSUMED_REDRAW
            }
            Event::Command(commands::CANCEL) if state.picking => {
                state.picking = false;
                Response::CONSUMED_REDRAW
            }
            Event::PointerDown if state.picking && over_bar => {
                state.picking = false;
                ctx.emit(commands::REPLAY_TOGGLE_AT_POINTER);
                Response::CONSUMED_REDRAW
            }
            // The line follows the pointer.
            Event::PointerMoved { .. } | Event::PointerLeft if state.picking => Response::REDRAW,
            _ => Response::IGNORED,
        }
    }
}

/// While the start is chosen: a line after the bar under the pointer and the bars that the replay would hide
/// dimmed. What to do is said by the player under the chart.
pub struct ReplayPickLayer;

impl Layer for ReplayPickLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if !ctx.res.get::<Replay>().is_some_and(|r| r.picking) {
            return;
        }
        let (plot_w, plot_h) = ctx.plot;
        let Some((px, _)) = ctx.chart_pointer() else { return };
        let bar = ctx.time.index_of(px as f64).floor();
        if !(0.0..plot_w).contains(&px) || bar < 0.0 || bar as usize >= ctx.view.len() {
            return;
        }
        let edge = (ctx.time.x_of(bar + 1.0) as f32).round().max(0.0);
        if edge < plot_w {
            // What would hide is shaded.
            let shade = Color {
                a: 160,
                ..ctx.style.background
            };
            ctx.scene.fill(edge, 0.0, plot_w - edge, plot_h, shade);
        }
        let accent = ctx.style.accent;
        ctx.scene.fill(edge - ctx.px(1.0), 0.0, ctx.px(2.0), plot_h, accent);
    }
}

/// The replay readout in the top left corner.
pub struct ReplayLayer;

impl Layer for ReplayLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let Some(state) = ctx.res.get::<Replay>().filter(|r| r.active) else {
            return;
        };
        if ctx.res.get::<super::NoReadouts>().is_some() {
            return;
        }
        let status = if state.playing {
            format!("playing, {} bars/s", state.speed)
        } else {
            "paused".into()
        };
        let text = format!("REPLAY  {status}  {} / {}", state.shown, state.total);
        ctx.scene.text(
            ctx.px(12.0),
            ctx.px(16.0),
            ctx.px(ctx.style.font_size),
            ctx.style.accent,
            Align::Left,
            text,
        );
    }
}

pub struct ReplayPlugin;

impl Plugin for ReplayPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(Replay::default());
        chart.add_button(
            ToolButton::new(
                commands::REPLAY_TOGGLE,
                Label::Dynamic(|res| {
                    if active(res) {
                        "Exit replay".into()
                    } else {
                        "Replay".into()
                    }
                }),
            )
            .highlighted_when(active),
        );
        chart.add_button(
            ToolButton::new(
                commands::REPLAY_PLAY_PAUSE,
                Label::Dynamic(|res| {
                    if res.get::<Replay>().is_some_and(|r| r.playing) {
                        "Pause".into()
                    } else {
                        "Play".into()
                    }
                }),
            )
            .visible_when(active),
        );
        chart.add_button(ToolButton::new(commands::REPLAY_STEP, Label::Fixed("Step")).visible_when(active));
        chart.add_button(ToolButton::new(commands::REPLAY_SLOWER, Label::Fixed("Slower")).visible_when(active));
        chart.add_button(ToolButton::new(commands::REPLAY_FASTER, Label::Fixed("Faster")).visible_when(active));
        chart.bind(Key::Alt('P'), commands::REPLAY_TOGGLE_AT_POINTER);
        chart.bind(Key::Space, commands::REPLAY_PLAY_PAUSE);
        chart.bind(Key::SelectRight, commands::REPLAY_STEP);
        chart.bind(Key::Up, commands::REPLAY_FASTER);
        chart.bind(Key::Char('+'), commands::REPLAY_FASTER);
        chart.bind(Key::Char('='), commands::REPLAY_FASTER);
        chart.bind(Key::Down, commands::REPLAY_SLOWER);
        chart.bind(Key::Char('-'), commands::REPLAY_SLOWER);
        chart.bind(Key::Escape, commands::CANCEL);
        chart.add_handler(names::REPLAY, order::REPLAY_HANDLER, ReplayHandler);
        chart.add_handler(names::REPLAY_PICK, order::REPLAY_PICK_HANDLER, ReplayPickHandler);
        chart.add_layer_in(Panes::All, names::REPLAY_PICK, order::REPLAY - 1, ReplayPickLayer);
        chart.add_layer(names::REPLAY, order::REPLAY, ReplayLayer);
    }
}
