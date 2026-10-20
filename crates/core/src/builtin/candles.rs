use core::ops::Range;

use crate::plugin::{commands, names, order};
use crate::{
    CandleView, Chart, Color, Columns, DrawCtx, Event, EventCtx, Handler, Layer, Plugin, Resources, Response,
    aggregate, emit_candles,
};

/// How the price series is drawn. The candles layer reads it from the resources, so any plugin can
/// change it (the `K` key cycles through the kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeriesKind {
    #[default]
    Candles,
    /// Candles of smoothed prices: trends show as runs of one color. Zoomed out to fewer pixels than
    /// bars they are plain candles.
    HeikinAshi,
    /// Open, high, low and close as a stem with ticks.
    Bars,
    /// The close as a line.
    Line,
    /// The close as a line with the area under it filled.
    Area,
}

impl SeriesKind {
    pub const ALL: [SeriesKind; 5] = [Self::Candles, Self::HeikinAshi, Self::Bars, Self::Line, Self::Area];

    pub fn next(self) -> Self {
        match self {
            Self::Candles => Self::HeikinAshi,
            Self::HeikinAshi => Self::Bars,
            Self::Bars => Self::Line,
            Self::Line => Self::Area,
            Self::Area => Self::Candles,
        }
    }
}

/// Bars a Heikin-Ashi series is calculated on before the first visible one: its smoothing halves
/// the memory of the start with every bar, so this leaves nothing of it.
const HEIKIN_ASHI_WARMUP: usize = 60;

/// Heikin-Ashi candles for the bars of `range`, one per bar.
pub fn heikin_ashi(view: CandleView<'_>, range: Range<usize>) -> Columns {
    let range = range.start.min(view.len())..range.end.min(view.len());
    let mut out = Columns::default();
    if range.is_empty() {
        return out;
    }
    let (open, high, low, close) = (view.open(), view.high(), view.low(), view.close());
    let mut previous: Option<(f64, f64)> = None;
    for i in range.start.saturating_sub(HEIKIN_ASHI_WARMUP)..range.end {
        let ha_close = (open[i] + high[i] + low[i] + close[i]) / 4.0;
        let ha_open = previous.map_or((open[i] + close[i]) / 2.0, |(o, c)| (o + c) / 2.0);
        previous = Some((ha_open, ha_close));
        if i >= range.start {
            out.open.push(ha_open);
            out.close.push(ha_close);
            out.high.push(high[i].max(ha_open).max(ha_close));
            out.low.push(low[i].min(ha_open).min(ha_close));
            out.first.push(i as u32);
            out.count.push(1);
        }
    }
    out
}

/// The price series, drawn as its [`SeriesKind`] says.
pub struct CandlesLayer;

impl CandlesLayer {
    fn kind(res: &Resources) -> SeriesKind {
        res.get::<SeriesKind>().copied().unwrap_or_default()
    }
}

impl Layer for CandlesLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        if ctx.view.is_empty() {
            return;
        }
        // With less than a pixel per bar, merge bars so the scene stays as small as the screen.
        let group = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let kind = Self::kind(ctx.res);
        let columns = match kind {
            SeriesKind::HeikinAshi if group == 1 => heikin_ashi(ctx.view, ctx.visible.clone()),
            _ => aggregate(ctx.view, ctx.visible.clone(), group),
        };
        match kind {
            SeriesKind::Candles | SeriesKind::HeikinAshi => emit_candles(
                &columns,
                ctx.time,
                ctx.price,
                &ctx.style.candle,
                ctx.scale as f64,
                ctx.scene,
            ),
            SeriesKind::Bars => draw_bars(ctx, &columns),
            SeriesKind::Line => draw_line(ctx, &columns, false),
            SeriesKind::Area => draw_line(ctx, &columns, true),
        }
    }

    fn price_extent(&self, view: CandleView<'_>, visible: Range<usize>, res: &Resources) -> Option<(f64, f64)> {
        match Self::kind(res) {
            SeriesKind::Line | SeriesKind::Area => {
                let closes = &view.close()[visible.start.min(view.len())..visible.end.min(view.len())];
                closes.iter().copied().fold(None, |e, v| match e {
                    None => Some((v, v)),
                    Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
                })
            }
            SeriesKind::HeikinAshi => {
                let ha = heikin_ashi(view, visible);
                let low = ha.low.iter().copied().fold(f64::INFINITY, f64::min);
                let high = ha.high.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                (low <= high).then_some((low, high))
            }
            _ => view.slice(visible).extent(),
        }
    }
}

fn draw_bars(ctx: &mut DrawCtx<'_>, columns: &Columns) {
    let style = &ctx.style.candle;
    let line = (style.wick_width * ctx.scale as f64).round().max(1.0);
    for i in 0..columns.len() {
        let (open, close) = (columns.open[i], columns.close[i]);
        let color = if close >= open { style.up } else { style.down };
        let count = columns.count[i] as f64;
        let center = ctx.time.x_of(columns.first[i] as f64 + count / 2.0);
        let slot = count * ctx.time.bar_spacing;
        let tick = (slot * 0.4).round().max(line);
        let x = (center - line / 2.0).round();
        let (top, bottom) = (ctx.price.y_of(columns.high[i]), ctx.price.y_of(columns.low[i]));
        ctx.scene.fill(
            x as f32,
            top as f32,
            line as f32,
            (bottom - top).max(line) as f32,
            color,
        );
        let (y_open, y_close) = (ctx.price.y_of(open), ctx.price.y_of(close));
        ctx.scene.fill(
            (x - tick) as f32,
            (y_open - line / 2.0).round() as f32,
            tick as f32,
            line as f32,
            color,
        );
        ctx.scene.fill(
            (x + line) as f32,
            (y_close - line / 2.0).round() as f32,
            tick as f32,
            line as f32,
            color,
        );
    }
}

fn draw_line(ctx: &mut DrawCtx<'_>, columns: &Columns, fill: bool) {
    let color = ctx.style.accent;
    let width = ctx.px(2.0);
    let bottom = ctx.plot.1;
    let mut previous: Option<(f32, f32)> = None;
    for i in 0..columns.len() {
        let center = columns.first[i] as f64 + columns.count[i] as f64 / 2.0;
        let point = (ctx.time.x_of(center) as f32, ctx.price.y_of(columns.close[i]) as f32);
        if let Some(from) = previous {
            if fill {
                fill_area(ctx.scene, (from, point), bottom, color, ctx.plot.0);
            }
            ctx.scene.line(from, point, width, color);
        }
        previous = Some(point);
    }
}

/// The area between the segment `from`..`to` and the level `base`, a column per pixel, in `color` at the line
/// fading to nothing at the base (below the base the fade goes up to it).
pub(crate) fn fill_area(
    scene: &mut crate::Scene,
    (from, to): ((f32, f32), (f32, f32)),
    base: f32,
    color: Color,
    max_x: f32,
) {
    let (strong, none) = (Color { a: 48, ..color }, Color { a: 0, ..color });
    // The pixels whose centers are in [from, to): the next segment starts where this one stops, none twice.
    let (left, right) = ((from.0 - 0.5).ceil().max(0.0), (to.0 - 0.5).ceil().min(max_x));
    let span = (to.0 - from.0).max(f32::EPSILON);
    let mut x = left;
    while x < right {
        let t = ((x + 0.5 - from.0) / span).clamp(0.0, 1.0);
        let y = from.1 + (to.1 - from.1) * t;
        let (top, colors) = if y <= base {
            (y, (strong, none))
        } else {
            (base, (none, strong))
        };
        let h = (y - base).abs();
        if h > 0.0 {
            scene.fill_gradient(x, top, 1.0, h, colors, crate::Direction::Down);
        }
        x += 1.0;
    }
}

/// `K` cycles the kind of the price series.
pub struct SeriesKindHandler;

impl Handler for SeriesKindHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if *event != Event::Command(commands::SERIES_CYCLE) {
            return Response::IGNORED;
        }
        let kind = ctx.res.get::<SeriesKind>().copied().unwrap_or_default().next();
        ctx.res.insert(kind);
        Response::CONSUMED_REDRAW
    }
}

pub struct CandlesPlugin;

impl Plugin for CandlesPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.resources_mut().insert(SeriesKind::default());
        chart.add_layer(names::CANDLES, order::SERIES, CandlesLayer);
        chart.add_handler("series-kind", order::PANES_HANDLER + 2, SeriesKindHandler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candle, Candles, Time};

    fn candles(rows: &[(f64, f64, f64, f64)]) -> Candles {
        let mut c = Candles::new();
        for (i, &(open, high, low, close)) in rows.iter().enumerate() {
            c.push(Candle {
                time: Time(i as i64),
                open,
                high,
                low,
                close,
                volume: 1.0,
            })
            .unwrap();
        }
        c
    }

    #[test]
    fn heikin_ashi_follows_its_definition() {
        let c = candles(&[
            (10.0, 12.0, 9.0, 11.0),
            (11.0, 14.0, 10.0, 13.0),
            (13.0, 13.5, 8.0, 9.0),
        ]);
        let ha = heikin_ashi(c.view(), 0..3);
        // Bar 0: open is the mid of open and close, close the mean of the four.
        assert_eq!((ha.open[0], ha.close[0]), (10.5, 10.5));
        // Bar 1: open is the mid of the previous HA open and close.
        assert_eq!(ha.open[1], 10.5);
        assert_eq!(ha.close[1], (11.0 + 14.0 + 10.0 + 13.0) / 4.0);
        assert_eq!(ha.high[1], 14.0);
        assert_eq!(ha.low[1], 10.0);
        assert_eq!(ha.open[2], (10.5 + 12.0) / 2.0);
        assert!(ha.low[2] <= 8.0 && ha.high[2] >= 13.5);
        assert_eq!(
            (ha.first.as_slice(), ha.count.as_slice()),
            ([0, 1, 2].as_slice(), [1, 1, 1].as_slice())
        );
    }

    #[test]
    fn a_window_far_from_the_start_matches_the_full_series() {
        let rows: Vec<_> = (0..500)
            .map(|i| {
                let p = 100.0 + (i as f64 * 0.3).sin() * 10.0;
                (p, p + 2.0, p - 2.0, p + (i as f64 * 0.7).cos())
            })
            .collect();
        let c = candles(&rows);
        let full = heikin_ashi(c.view(), 0..500);
        let window = heikin_ashi(c.view(), 400..450);
        assert_eq!(window.len(), 50);
        for k in 0..50 {
            assert!((full.open[400 + k] - window.open[k]).abs() < 1e-9);
            assert!((full.close[400 + k] - window.close[k]).abs() < 1e-9);
        }
    }

    #[test]
    fn empty_and_out_of_range_windows_are_empty() {
        let c = candles(&[(1.0, 2.0, 0.5, 1.5)]);
        assert!(heikin_ashi(c.view(), 5..9).is_empty());
        assert!(heikin_ashi(Candles::new().view(), 0..3).is_empty());
    }

    #[test]
    fn kinds_cycle_through_all_and_back() {
        let mut kind = SeriesKind::Candles;
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(kind);
            kind = kind.next();
        }
        assert_eq!(kind, SeriesKind::Candles);
        seen.sort_by_key(|k| *k as u8);
        seen.dedup();
        assert_eq!(seen.len(), 5);
    }
}
