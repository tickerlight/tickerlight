use core::cell::RefCell;
use core::ops::Range;

use super::trading::{Account, EquityBars};
use crate::plugin::order;
use crate::{Align, CandleView, Chart, Color, DrawCtx, Layer, Panes, Plugin, Resources};

/// The value of the virtual account bar by bar (profit after fees, open position marked to each
/// close) in a pane under the candles. Reads the [`Account`] and only the bars the chart may see, so during a
/// replay the curve grows with it. Not in `DefaultPlugins`: `chart.add_plugin(Equity::new())`.
#[derive(Clone)]
pub struct Equity {
    pub color: Color,
    /// Name of the pane and of the layer in it.
    pub name: &'static str,
    /// Height of the pane relative to the main one (`1.0`).
    pub weight: f32,
    /// Where the account is: the manual one by default.
    pub account: fn(&Resources) -> Option<&Account>,
    /// What the readout calls it.
    pub title: &'static str,
    /// The equity at each bar, kept between frames.
    pub(crate) curve: RefCell<EquityBars>,
}

impl Equity {
    pub fn new() -> Self {
        Self {
            color: Color::rgb(41, 182, 246),
            name: "equity",
            weight: 0.3,
            account: |res| res.get::<Account>(),
            title: "Equity",
            curve: RefCell::default(),
        }
    }
}

impl Default for Equity {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Equity {
    fn build(&self, chart: &mut Chart) {
        chart.add_pane(self.name, self.weight);
        chart.add_layer_in(Panes::One(self.name), self.name, order::SERIES, self.clone());
    }
}

impl Equity {
    /// The equity at the bars of `range` (those of `view` only), and the first of them; none without an account.
    fn values<'a>(
        curve: &'a mut EquityBars,
        account: Option<&Account>,
        view: CandleView<'_>,
        range: Range<usize>,
    ) -> (&'a [f64], usize) {
        let Some(account) = account else {
            return (&[], 0);
        };
        let values = curve.update(account, view);
        let range = range.start.min(values.len())..range.end.min(values.len());
        (&values[range.clone()], range.start)
    }
}

impl Layer for Equity {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let style = ctx.style;
        let unit = ctx.line();
        let zero = ctx.price.y_of(0.0) as f32;
        ctx.scene.fill(0.0, zero.floor(), ctx.plot.0, unit, style.axis);
        if ctx.view.is_empty() {
            return;
        }
        let mut curve = self.curve.borrow_mut();
        let (values, first) = Self::values(&mut curve, (self.account)(ctx.res), ctx.view, ctx.visible.clone());
        let stride = (1.0 / ctx.time.bar_spacing).ceil().max(1.0) as usize;
        let width = ctx.px(1.5);
        let mut previous = None;
        // Every `stride`th bar, as the bars are drawn.
        let skip = first.next_multiple_of(stride) - first;
        for (i, &value) in (first..).zip(values).skip(skip).step_by(stride) {
            let point = (ctx.time.x_of(i as f64 + 0.5) as f32, ctx.price.y_of(value) as f32);
            if let Some(from) = previous {
                super::candles::fill_area(ctx.scene, (from, point), zero, self.color, ctx.plot);
                ctx.scene.line(from, point, width, self.color);
            }
            previous = Some(point);
        }
        let text = match values.last() {
            Some(v) => format!("{}  {}", self.title, crate::ticks::signed(*v, 2)),
            None => self.title.to_string(),
        };
        ctx.scene.text(
            ctx.px(12.0),
            ctx.px(14.0),
            ctx.px(style.font_size),
            style.axis_text,
            Align::Left,
            text,
        );
    }

    /// The curve, and zero, which is where the account started.
    fn price_extent(&self, view: CandleView<'_>, visible: Range<usize>, res: &Resources) -> Option<(f64, f64)> {
        if view.is_empty() {
            return None;
        }
        let mut curve = self.curve.borrow_mut();
        let (values, _) = Self::values(&mut curve, (self.account)(res), view, visible);
        Some(values.iter().fold((0.0, 0.0), |(lo, hi), &v| (lo.min(v), hi.max(v))))
    }
}
