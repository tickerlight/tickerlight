use crate::plugin::{commands, names, order};
use crate::timeframe::{native_step, span_label};
use crate::{Chart, Event, EventCtx, Handler, Label, Plugin, Resources, Response, ToolButton};

/// What the timeframe button says: the period of the candles shown, kept up to date by the handler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeframeLabel {
    pub text: String,
    /// A coarser timeframe than the data is shown.
    pub coarser: bool,
}

/// The button shows the next higher timeframe. Replay and trading keep working on the data as it
/// came, so a replay on 15 minutes moves one minute at a time and the newest 15 minute candle forms in front
/// of you.
pub struct TimeframeHandler;

impl Handler for TimeframeHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let command = *event == Event::Command(commands::TIMEFRAME_CYCLE);
        if command {
            ctx.timeframe.cycle();
        }
        let now = ctx.timeframe.get();
        let label = match (now, ctx.timeframe.native()) {
            (Some(span), _) => TimeframeLabel {
                text: span_label(span),
                coarser: true,
            },
            (None, Some(step)) => TimeframeLabel {
                text: span_label(step),
                coarser: false,
            },
            (None, None) => TimeframeLabel::default(),
        };
        if ctx.res.get::<TimeframeLabel>() != Some(&label) {
            ctx.res.insert(label);
        }
        if command {
            Response::CONSUMED_REDRAW
        } else {
            Response::IGNORED
        }
    }
}

fn label(res: &Resources) -> String {
    res.get::<TimeframeLabel>().map_or_else(String::new, |l| l.text.clone())
}

pub struct TimeframePlugin;

impl Plugin for TimeframePlugin {
    fn build(&self, chart: &mut Chart) {
        let text = native_step(chart.seen()).map(span_label).unwrap_or_default();
        chart.resources_mut().insert(TimeframeLabel { text, coarser: false });
        chart.add_button(
            ToolButton::new(commands::TIMEFRAME_CYCLE, Label::Dynamic(label))
                .visible_when(|res| res.get::<TimeframeLabel>().is_some_and(|l| !l.text.is_empty()))
                .highlighted_when(|res| res.get::<TimeframeLabel>().is_some_and(|l| l.coarser)),
        );
        chart.add_handler(names::TIMEFRAME, order::PANES_HANDLER + 4, TimeframeHandler);
    }
}
